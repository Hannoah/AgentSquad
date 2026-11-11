use crate::config::{is_bare_shell, pty_env, size, Kind, Project};
use crate::ledger;
#[cfg(unix)]
use crate::sys::kill_group;
use crate::sys::{cpu_percent, Usage};
use crate::sys::{kill_tree_by_pid, kill_trees};
use crate::ui::{
    pane_status_text, screen_lines, spinner_frame, status_glyph, status_style, uptime_label, Notice,
};
use anyhow::{anyhow, Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    widgets::{Paragraph, Widget},
};
use std::{
    borrow::Cow,
    cell::RefCell,
    fmt,
    io::{Read, Write},
    num::NonZero,
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
    time::{Duration, Instant},
};

/// Scrollback lines kept per session unless the config says otherwise.
pub const DEFAULT_SCROLLBACK: usize = 2000;
static SCROLLBACK: AtomicUsize = AtomicUsize::new(DEFAULT_SCROLLBACK);
/// Set the per-session scrollback for sessions spawned from now on.
pub fn set_scrollback_limit(lines: usize) {
    SCROLLBACK.store(lines.clamp(100, 50_000), Ordering::Relaxed);
}
/// A real prompt waits for the user, so its screen stops changing. Output
/// younger than this means the agent is still busy, whatever the text says.
const NEEDS_INPUT_QUIET: Duration = Duration::from_millis(600);
/// Longest the UI thread spends draining one session's output per pass,
/// so a flood of output cannot starve key handling.
const DRAIN_BUDGET: Duration = Duration::from_millis(8);
/// Minimum gap between screen-text scans while output keeps arriving.
const FLAGS_EVERY: Duration = Duration::from_millis(100);

/// Name for the terminal pane title.
///
/// For services, show the command being served (e.g. `npm run dev` or `cmd.exe`)
/// so running servers read by their role rather than a generic folder name.
/// Agents continue to show their registered project name.
pub fn pane_title_name(p: &Project) -> String {
    if p.kind == Kind::Service && !p.command.is_empty() && !is_bare_shell(&p.command) {
        p.command.join(" ")
    } else {
        p.name.clone()
    }
}
/// Title name that follows what a shell service is really running: a
/// service registered as a bare shell reads as the program started in it.
pub fn live_pane_title(p: &Project, fg: Option<&str>) -> String {
    match fg {
        Some(f) if p.kind == Kind::Service && is_bare_shell(&p.command) => f.to_string(),
        _ => pane_title_name(p),
    }
}
/// Sidebar name, with the same shell-service rule as `live_pane_title`.
pub fn live_name<'a>(p: &'a Project, fg: Option<&'a str>) -> &'a str {
    match fg {
        Some(f) if p.kind == Kind::Service && is_bare_shell(&p.command) => f,
        _ => &p.name,
    }
}

/// Lifecycle of a session slot. A project with no session is "Stopped".
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SessionStatus {
    Running,
    Exited(u32),
    Error(String),
}
impl SessionStatus {
    pub fn is_running(&self) -> bool {
        matches!(self, SessionStatus::Running)
    }
    /// Crashed exits and errors; clean exits stay quiet.
    pub fn needs_attention(&self) -> bool {
        match self {
            SessionStatus::Running => false,
            SessionStatus::Exited(code) => *code != 0,
            SessionStatus::Error(_) => true,
        }
    }
    /// Display text: "Running", "Exited (1)", "Error: ...".
    pub fn label(&self) -> Cow<'static, str> {
        match self {
            SessionStatus::Running => Cow::Borrowed("Running"),
            other => Cow::Owned(other.to_string()),
        }
    }
}
impl fmt::Display for SessionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionStatus::Running => f.write_str("Running"),
            SessionStatus::Exited(code) => write!(f, "Exited ({code})"),
            SessionStatus::Error(e) => write!(f, "Error: {e}"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgentPhase {
    Working,
    NeedsInput,
    Done,
    Idle,
}

/// Should the bell ring when an agent enters `now`? Only when it starts
/// waiting for you; a finished task shows in the title and sidebar instead,
/// so the bell stays rare.
pub fn rings_bell(now: AgentPhase) -> bool {
    now == AgentPhase::NeedsInput
}

impl AgentPhase {
    /// The state in words, as the pane title shows it.
    pub fn word(self) -> &'static str {
        match self {
            AgentPhase::Working => "Engaged",
            AgentPhase::NeedsInput => "Awaiting orders",
            AgentPhase::Done => "Task complete",
            AgentPhase::Idle => "Standby",
        }
    }
    pub fn icon(self, tick: u64) -> (String, Style) {
        match self {
            AgentPhase::Working => (
                format!("{}", spinner_frame(tick)),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            ),
            AgentPhase::NeedsInput => (
                "?".to_string(),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            ),
            AgentPhase::Done => (
                "✔".to_string(),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            ),
            AgentPhase::Idle => ("○".to_string(), Style::default().fg(Color::DarkGray)),
        }
    }
}

/// Non-empty trailing lines of `text`, newest first, trimmed and lowercased.
fn tail_lines(text: &str, n: usize) -> Vec<String> {
    text.lines()
        .rev()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(n)
        .map(str::to_ascii_lowercase)
        .collect()
}

/// Hints that the agent is busy right now ("esc to interrupt", or the
/// "(esc to cancel, 5s)" timer some CLIs show while working).
fn has_busy_hint(line: &str) -> bool {
    line.contains("to interrupt")
        || line.contains("esc to cancel,")
        || line.contains("(esc to cancel")
}

/// True when `line` looks like source code, a diff hunk, or a numbered file
/// listing rather than an interactive prompt (e.g. `180: "[y/n]",` or `+ assert!`).
fn is_code_or_diff_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//")
        || trimmed.starts_with("+ ")
        || trimmed.starts_with("- ")
        || trimmed.starts_with("assert")
        || trimmed.starts_with('"')
    {
        return true;
    }
    // Numbered code lines like `180: "[y/n]"` or `180 | ...`
    let digits = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 {
        let rest = trimmed[digits..].trim_start();
        if rest.starts_with(':') || rest.starts_with('|') {
            return true;
        }
    }
    false
}

/// A selectable answer such as "❯ 1. Yes" or "1) Yes".
fn is_choice_line(line: &str) -> bool {
    let rest = line.trim_start_matches(['❯', '>', '›', '●', '○', ' ']);
    rest.starts_with("1. yes")
        || rest.starts_with("1) yes")
        || rest.starts_with("1. allow")
        || rest.starts_with("1) allow")
        || rest.starts_with("yes,")
}

/// Does the bottom of the screen ask the user something? A busy hint or
/// active spinner wins over everything: the agent may print "do you want to"
/// or code containing `"[y/n]"` in its own output while it works.
pub fn is_needs_input(screen_text: &str) -> bool {
    !is_working_text(screen_text) && has_prompt(screen_text)
}
/// Phrases that mean an agent is waiting on the user.
const STRONG: [&str; 17] = [
    "[y/n]",
    "(y/n)",
    "[y/n/a]",
    "[y/n/d]",
    "(y/n/a)",
    "[yes/no]",
    "(yes/no)",
    "(y)es / (n)o",
    "allow once",
    "allow always",
    "allow tool",
    "allow execution",
    "esc to cancel",
    "press enter to continue",
    "press enter to proceed",
    "press any key",
    "don't ask again",
];
const QUESTION: [&str; 6] = [
    "do you want to",
    "are you sure",
    "proceed?",
    "approve?",
    "confirm?",
    "permission to",
];

/// The prompt half of `is_needs_input`, for callers that already know the
/// agent is not working.
fn has_prompt(screen_text: &str) -> bool {
    let lines: Vec<String> = tail_lines(screen_text, 6)
        .into_iter()
        .filter(|l| !is_code_or_diff_line(l))
        .collect();
    if lines.iter().any(|l| {
        STRONG.iter().any(|p| {
            // Ignore quoted string literals like `"[y/n]"` in code output.
            l.contains(p) && !l.contains(&format!("\"{p}\""))
        })
    }) {
        return true;
    }
    lines.iter().any(|l| is_choice_line(l))
        && lines.iter().any(|l| QUESTION.iter().any(|p| l.contains(p)))
}

/// The line of `screen_text` that says what the agent is asking: the newest
/// one that reads like a question, else the newest line. Box-drawing frames
/// around the text are dropped.
pub fn prompt_summary(screen_text: &str) -> Option<String> {
    let frame = |c: char| c.is_whitespace() || "│┃|╭╮╰╯─".contains(c);
    let lines: Vec<&str> = screen_text
        .lines()
        .rev()
        .map(|l| l.trim_matches(frame))
        .filter(|l| !l.is_empty())
        .take(6)
        .collect();
    let asks = |l: &str| {
        let lower = l.to_ascii_lowercase();
        !is_code_or_diff_line(&lower)
            && STRONG
                .iter()
                .chain(QUESTION.iter())
                .any(|p| lower.contains(p))
    };
    lines
        .iter()
        .copied()
        .find(|l| asks(l))
        .or_else(|| lines.first().copied())
        .map(str::to_string)
}

/// A status line led by a spinner glyph and an ellipsis: "✻ Pondering…".
fn is_spinner_line(line: &str) -> bool {
    let Some(first) = line.chars().next() else {
        return false;
    };
    let glyph = ('\u{2800}'..='\u{28ff}').contains(&first) || "✻✶✳✢✽✺✹✸✷·".contains(first);
    glyph && (line.contains('…') || line.contains("..."))
}

pub fn is_working_text(screen_text: &str) -> bool {
    const VERBS: [&str; 13] = [
        "thinking",
        "running tool",
        "executing",
        "searching",
        "generating",
        "compiling",
        "building",
        "reading",
        "writing",
        "editing",
        "analyzing",
        "pondering",
        "synthesizing",
    ];
    for lower in tail_lines(screen_text, 5) {
        if has_busy_hint(&lower) || is_spinner_line(&lower) {
            return true;
        }
        // Verbs count only at the start of a line, where status text puts
        // them; mid-sentence mentions ("finished reading") do not.
        let head = lower.trim_start_matches(|c: char| !c.is_alphanumeric());
        if VERBS.iter().any(|v| head.starts_with(v)) {
            return true;
        }
    }
    false
}

/// Status segment for the terminal pane title: "◐ Running 7s".
/// Reuses the sidebar glyph and color language so both panels read as one.
pub fn pane_status_display(
    status: &str,
    uptime_secs: Option<u64>,
    tick: u64,
    kind: Kind,
    phase: Option<AgentPhase>,
) -> (String, Style) {
    if status == "Running" && kind == Kind::Agent {
        if let Some(phase) = phase {
            let uptime_str = uptime_secs
                .map(|s| format!(" {}", uptime_label(s)))
                .unwrap_or_default();
            return match phase {
                AgentPhase::Working => (
                    format!("{} {}{uptime_str}", spinner_frame(tick), phase.word()),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ),
                AgentPhase::NeedsInput => (
                    format!("? {}{uptime_str}", phase.word()),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ),
                AgentPhase::Done => (
                    format!("✔ {}{uptime_str}", phase.word()),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ),
                AgentPhase::Idle => (
                    format!("○ {}{uptime_str}", phase.word()),
                    Style::default().fg(Color::DarkGray),
                ),
            };
        }
    }
    let text = if status == "Running" {
        let uptime_str = uptime_secs
            .map(|s| format!(" {}", uptime_label(s)))
            .unwrap_or_default();
        format!("{}{uptime_str}", status_glyph(status, tick))
    } else {
        pane_status_text(status, uptime_secs, tick)
    };
    let style = status_style(status).add_modifier(ratatui::style::Modifier::BOLD);
    (text, style)
}
pub fn stop_target<T>(order: &[usize], selected: usize, slots: &[Option<T>]) -> Option<usize> {
    order
        .get(selected)
        .copied()
        .filter(|&pi| slots.get(pi).and_then(|s| s.as_ref()).is_some())
}
/// Sessions that quitting would discard (running or finished output).
pub fn live_session_count<T>(sessions: &[Option<T>]) -> usize {
    sessions.iter().flatten().count()
}
/// Unregister a project and drop its session slot, stopping a live
/// session through `Session`'s own teardown. Returns the removed project
/// so callers can roll back when persisting fails.
pub fn remove_project<T>(
    projects: &mut Vec<Project>,
    slots: &mut Vec<Option<T>>,
    pi: usize,
) -> Option<Project> {
    if pi >= projects.len() {
        return None;
    }
    if pi < slots.len() {
        slots.remove(pi);
    }
    Some(projects.remove(pi))
}

/// The pane as last painted, reused until the screen changes.
struct RenderCache {
    gen: u64,
    area: Rect,
    buf: Buffer,
}

pub struct Session {
    pub master: Box<dyn MasterPty + Send>,
    pub child: Box<dyn Child + Send + Sync>,
    pub writer: Box<dyn Write + Send>,
    pub rx: Receiver<Vec<u8>>,
    pub parser: vt100::Parser,
    pub status: SessionStatus,
    pub scroll: usize,
    pub started: Option<Instant>,
    pub announced_exit: bool,
    pub usage: Option<Usage>,
    /// The memory-limit warning was shown and the session has not dropped back.
    pub mem_warned: bool,
    pub cpu_prev: u64,
    pub sampled: Option<Instant>,
    pub last_output: Option<Instant>,
    pub last_input: Option<Instant>,
    pub had_activity: bool,
    pub dismissed_exit: bool,
    /// Phase seen at the last check, to spot changes worth a bell.
    pub last_phase: AgentPhase,
    /// A finished task the user has already looked at, so the window title
    /// stops counting it.
    pub acked: bool,
    /// When the current task began working; set at Working, taken at Done
    /// (or when the session ends mid-task) to file the work history entry.
    pub work_started: Option<std::time::SystemTime>,
    /// Project folder whose ledger gets this session's events, if enabled.
    ledger: Option<std::path::PathBuf>,
    /// One-time message for the status bar (ledger created, or why not).
    pub ledger_note: Option<(String, Notice)>,
    pub cached_needs_input: bool,
    /// The question a waiting agent shows, for the sidebar.
    pub prompt_hint: Option<String>,
    pub cached_working_text: bool,
    /// Program the user started inside a shell session ("jarvis").
    pub fg_name: Option<String>,
    /// Bumped whenever the visible screen may have changed.
    gen: u64,
    flags_stale: bool,
    last_flags: Instant,
    /// Set once the whole tree was killed in bulk, so `Drop` need not walk
    /// the process table again.
    tree_killed: bool,
    render: RefCell<Option<RenderCache>>,
}
impl Session {
    pub fn spawn(p: &Project, rows: u16, cols: u16) -> Result<Self> {
        // A hand-edited config can carry `command = []`.
        let Some(program) = p.command.first() else {
            anyhow::bail!("Project '{}' has an empty command", p.name);
        };
        // Otherwise the OS blames the program ("directory name is invalid").
        if !p.path.is_dir() {
            anyhow::bail!("Folder {} of '{}' is missing", p.path.display(), p.name);
        }
        let (ledger, ledger_note) = if p.has_ledger() {
            match ledger::ensure(&p.path) {
                Ok(created) => {
                    let note = created.then(|| {
                        (
                            format!(
                                "Operations log opened in {} - press b to brief the agent",
                                p.name
                            ),
                            Notice::Info,
                        )
                    });
                    (Some(p.path.clone()), note)
                }
                Err(e) => (
                    None,
                    Some((
                        format!("Operations log unavailable in {}: {e:#}", p.name),
                        Notice::Warn,
                    )),
                ),
            }
        } else {
            (None, None)
        };
        // Claude Code agents with a ledger get a Stop hook that sends them
        // back to file a SITREP; anything else (or an own --settings) is left alone.
        let is_claude = std::path::Path::new(program)
            .file_stem()
            .is_some_and(|s| s.eq_ignore_ascii_case("claude"));
        let hook =
            (is_claude && p.kind == Kind::Agent && !p.command.iter().any(|a| a == "--settings"))
                .then(|| ledger.as_deref().and_then(ledger::write_hook_settings))
                .flatten();
        let pair = native_pty_system().openpty(size(rows, cols))?;
        let mut cmd = CommandBuilder::new(program);
        if let Some(file) = &hook {
            cmd.arg("--settings");
            cmd.arg(file);
        }
        cmd.args(&p.command[1..]);
        cmd.cwd(&p.path);
        for (k, v) in pty_env() {
            cmd.env(k, v);
        }
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let child = pair
            .slave
            .spawn_command(cmd)
            .with_context(|| format!("Cannot start {program}"))?;
        drop(pair.slave);
        if ledger.is_some() {
            ledger::event(&p.path, &format!("start {}", p.name));
        }
        // Bounded queue: at most 32 * 16 KiB of pending output per session.
        let (tx, rx) = mpsc::sync_channel(32);
        thread::spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Ok(Self {
            master: pair.master,
            child,
            writer,
            rx,
            parser: vt100::Parser::new(
                NonZero::new(rows.max(1)).unwrap(),
                NonZero::new(cols.max(1)).unwrap(),
                SCROLLBACK.load(Ordering::Relaxed),
            ),
            status: SessionStatus::Running,
            scroll: 0,
            started: Some(Instant::now()),
            announced_exit: false,
            usage: None,
            mem_warned: false,
            cpu_prev: 0,
            sampled: None,
            last_output: None,
            last_input: None,
            had_activity: false,
            dismissed_exit: false,
            last_phase: AgentPhase::Idle,
            acked: false,
            work_started: None,
            ledger,
            ledger_note,
            cached_needs_input: false,
            prompt_hint: None,
            cached_working_text: false,
            fg_name: None,
            gen: 0,
            flags_stale: false,
            last_flags: Instant::now(),
            tree_killed: false,
            render: RefCell::new(None),
        })
    }
    /// The last `n` rows of the live screen (whatever the scroll position)
    /// as text.
    pub fn tail_text(&mut self, n: u16) -> String {
        let scrolled = self.parser.screen().scrollback();
        if scrolled != 0 {
            self.parser.screen_mut().set_scrollback(0);
        }
        let text = {
            let screen = self.parser.screen();
            let (rows, cols) = screen.size();
            let (rows, cols) = (rows.get(), cols.get());
            screen.contents_between(rows.saturating_sub(n), 0, rows - 1, cols)
        };
        if scrolled != 0 {
            self.parser.screen_mut().set_scrollback(scrolled);
        }
        text
    }
    /// Rescan the bottom of the live screen for prompt/working text. Only
    /// the last rows are read: that is all the detectors look at, and a
    /// full-screen text dump per output chunk was the hot spot.
    pub fn refresh_screen_flags(&mut self) {
        let tail = self.tail_text(12);
        let working = is_working_text(&tail);
        self.cached_working_text = working;
        self.cached_needs_input = !working && has_prompt(&tail);
        self.prompt_hint = if self.cached_needs_input {
            prompt_summary(&tail)
        } else {
            None
        };
        self.flags_stale = false;
        self.last_flags = Instant::now();
    }
    pub fn update(&mut self) -> bool {
        let mut changed = false;
        let mut received_output = false;
        let started = Instant::now();
        for _ in 0..32 {
            match self.rx.try_recv() {
                Ok(bytes) => {
                    self.parser.process(&bytes);
                    self.last_output = Some(Instant::now());
                    self.had_activity = true;
                    received_output = true;
                    changed = true;
                }
                Err(_) => break,
            }
            if started.elapsed() >= DRAIN_BUDGET {
                break;
            }
        }
        if received_output {
            self.gen += 1;
            self.flags_stale = true;
        }
        // Throttled: scanning on every chunk of a flood costs more than the
        // answer is worth; one scan per 100 ms is plenty for a status icon.
        if self.flags_stale && self.last_flags.elapsed() >= FLAGS_EVERY {
            let before = (self.cached_needs_input, self.cached_working_text);
            self.refresh_screen_flags();
            changed |= before != (self.cached_needs_input, self.cached_working_text);
        }
        if self.status.is_running() {
            match self.child.try_wait() {
                Ok(Some(exit)) => {
                    self.log_event(&format!("exit code={}", exit.exit_code()));
                    self.status = SessionStatus::Exited(exit.exit_code());
                    changed = true;
                }
                Err(e) => {
                    self.log_event("error");
                    self.status = SessionStatus::Error(e.to_string());
                    changed = true;
                }
                _ => {}
            }
        }
        changed
    }
    /// Record a fact in the project's ledger, when it has one.
    pub fn log_event(&self, text: &str) {
        if let Some(dir) = &self.ledger {
            ledger::event(dir, text);
        }
    }
    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        self.master.resize(size(rows, cols))?;
        self.parser.set_size(
            NonZero::new(rows.max(1)).unwrap(),
            NonZero::new(cols.max(1)).unwrap(),
        );
        self.gen += 1;
        self.refresh_screen_flags();
        Ok(())
    }
    pub fn input(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()?;
        self.last_input = Some(Instant::now());
        self.had_activity = true;
        Ok(())
    }
    pub fn phase(&self) -> AgentPhase {
        if !self.status.is_running() {
            return AgentPhase::Idle;
        }
        let user_just_typed = self
            .last_input
            .is_some_and(|t| t.elapsed().as_millis() < 800);
        let cpu_active = self.usage.is_some_and(|u| u.cpu > 5.0);
        let output_streaming = self
            .last_output
            .is_some_and(|t| t.elapsed() < NEEDS_INPUT_QUIET);
        // Screen evidence beats a prompt match so an agent never flashes `?` mid-run.
        if self.cached_working_text || output_streaming {
            return AgentPhase::Working;
        }
        if !user_just_typed && self.cached_needs_input {
            return AgentPhase::NeedsInput;
        }
        // CPU is whole-tree: check it after the prompt so a dev server cannot hide a question.
        if cpu_active {
            return AgentPhase::Working;
        }
        let recent_output = self
            .last_output
            .is_some_and(|t| t.elapsed().as_millis() < 2500);
        let recent_input = self
            .last_input
            .is_some_and(|t| t.elapsed().as_millis() < 2500);
        if recent_output || recent_input {
            return AgentPhase::Working;
        }
        if self.had_activity && self.last_input.is_some() {
            return AgentPhase::Done;
        }
        AgentPhase::Idle
    }
    /// True while output or typing is recent enough that the UI should keep
    /// its fast poll rate.
    pub fn is_active(&self, within: Duration) -> bool {
        self.status.is_running()
            && (self.last_output.is_some_and(|t| t.elapsed() < within)
                || self.last_input.is_some_and(|t| t.elapsed() < within))
    }
    pub fn scroll_by(&mut self, delta_rows: isize) {
        let max = self.parser.screen().scrollback_len() as isize;
        self.scroll = (self.scroll as isize + delta_rows).clamp(0, max) as usize;
        self.parser.screen_mut().set_scrollback(self.scroll);
        self.gen += 1;
    }
    /// Record a measurement of this session's process tree taken at `at`.
    /// CPU needs two samples, so it reads 0% on the first one. Results for
    /// a previous process (the session was restarted meanwhile) are dropped.
    pub fn apply_usage(&mut self, pid: u32, cpu_time: u64, mem: u64, at: Instant, cores: usize) {
        if !self.status.is_running() || self.child.process_id() != Some(pid) {
            return;
        }
        let cpu = self.sampled.map_or(0.0, |prev| {
            cpu_percent(
                self.cpu_prev,
                cpu_time,
                at.saturating_duration_since(prev).as_secs_f64(),
                cores,
            )
        });
        self.cpu_prev = cpu_time;
        self.sampled = Some(at);
        self.usage = Some(Usage { cpu, mem });
    }
    /// Record the program running inside this shell session. Ignored for a
    /// previous process, like `apply_usage`.
    pub fn apply_label(&mut self, pid: u32, label: Option<String>) {
        if self.status.is_running() && self.child.process_id() == Some(pid) {
            self.fg_name = label;
        }
    }
    /// Forget usage once the session stops running.
    pub fn clear_usage(&mut self) {
        self.usage = None;
        self.mem_warned = false;
        self.sampled = None;
        self.fg_name = None;
    }
    pub fn scroll_live(&mut self) {
        if self.scroll != 0 {
            self.gen += 1;
        }
        self.scroll = 0;
        self.parser.screen_mut().set_scrollback(0);
    }
    /// Paint the terminal screen into `buf` at `area`. The cells are built
    /// once per screen change and copied afterwards, so spinner ticks and
    /// status updates redraw the frame without re-walking every cell.
    pub fn paint(&self, buf: &mut Buffer, area: Rect) {
        let mut cache = self.render.borrow_mut();
        let fresh = cache
            .as_ref()
            .is_some_and(|c| c.gen == self.gen && c.area == area);
        if !fresh {
            let mut cells = Buffer::empty(area);
            Paragraph::new(screen_lines(self.parser.screen(), area.width, area.height))
                .render(area, &mut cells);
            *cache = Some(RenderCache {
                gen: self.gen,
                area,
                buf: cells,
            });
        }
        if let Some(c) = cache.as_ref() {
            for y in area.top()..area.bottom() {
                for x in area.left()..area.right() {
                    if let (Some(dst), Some(src)) = (buf.cell_mut((x, y)), c.buf.cell((x, y))) {
                        dst.clone_from(src);
                    }
                }
            }
        }
    }
}
pub fn scroll_live_selected(sessions: &mut [Option<Session>], order: &[usize], selected: usize) {
    if let Some(&pi) = order.get(selected) {
        if let Some(Some(s)) = sessions.get_mut(pi) {
            s.scroll_live();
        }
    }
}
/// Stop any existing session for `pi` and spawn a fresh one.
/// Returns an error message when the spawn fails.
pub fn restart_session(
    sessions: &mut [Option<Session>],
    projects: &[Project],
    pi: usize,
    rows: u16,
    cols: u16,
) -> Option<String> {
    if let Some(slot) = sessions.get_mut(pi) {
        if let Some(old) = slot.take() {
            thread::spawn(move || drop(old));
        }
    }
    let Some(p) = projects.get(pi) else {
        return Some(format!("No such project #{pi}"));
    };
    match Session::spawn(p, rows, cols) {
        Ok(s) => {
            if let Some(slot) = sessions.get_mut(pi) {
                *slot = Some(s);
            }
            None
        }
        Err(e) => Some(format!("{e:#}")),
    }
}
pub fn slot_running(sessions: &[Option<Session>], pi: usize) -> bool {
    sessions
        .get(pi)
        .and_then(|o| o.as_ref())
        .is_some_and(|s| s.status.is_running())
}
/// Start the project session unless it is already running.
/// Returns an error message when the spawn fails.
pub fn start_session(
    sessions: &mut [Option<Session>],
    projects: &[Project],
    pi: usize,
    rows: u16,
    cols: u16,
) -> Option<String> {
    if slot_running(sessions, pi) {
        return None;
    }
    restart_session(sessions, projects, pi, rows, cols)
}
/// Start every listed project that is not running yet, spawning them in
/// parallel so "start all" takes about as long as one start instead of
/// freezing the UI for the sum. Returns (started, last error).
pub fn start_sessions(
    sessions: &mut [Option<Session>],
    projects: &[Project],
    pis: &[usize],
    rows: u16,
    cols: u16,
) -> (usize, Option<String>) {
    let todo: Vec<usize> = pis
        .iter()
        .copied()
        .filter(|&pi| pi < projects.len() && !slot_running(sessions, pi))
        .collect();
    for &pi in &todo {
        if let Some(old) = sessions.get_mut(pi).and_then(|s| s.take()) {
            thread::spawn(move || drop(old));
        }
    }
    let results: Vec<(usize, Result<Session>)> = thread::scope(|scope| {
        let handles: Vec<_> = todo
            .iter()
            .map(|&pi| {
                let p = &projects[pi];
                (pi, scope.spawn(move || Session::spawn(p, rows, cols)))
            })
            .collect();
        handles
            .into_iter()
            .map(|(pi, h)| {
                let r = h
                    .join()
                    .unwrap_or_else(|_| Err(anyhow!("start thread panicked")));
                (pi, r)
            })
            .collect()
    });
    let (mut started, mut failed) = (0, None);
    for (pi, r) in results {
        match r {
            Ok(s) => {
                if let Some(slot) = sessions.get_mut(pi) {
                    *slot = Some(s);
                    started += 1;
                }
            }
            Err(e) => failed = Some(format!("{e:#}")),
        }
    }
    (started, failed)
}
/// Stop every session on the way out. All process trees are killed from
/// one process-table snapshot instead of one walk per session.
pub fn shutdown_all(sessions: &mut [Option<Session>]) {
    let mut live = Vec::new();
    for s in sessions.iter_mut().flatten() {
        let Some(pid) = s.child.process_id() else {
            continue;
        };
        if s.root_reaped() {
            #[cfg(unix)]
            kill_group(pid);
        } else {
            live.push(pid);
        }
        s.tree_killed = true;
    }
    kill_trees(&live);
    for slot in sessions.iter_mut() {
        drop(slot.take());
    }
}
impl Session {
    /// True when the root process has exited on a platform that recycles
    /// pids (unix): its pid may now belong to a stranger, so only the process
    /// group may be swept. Windows keeps walking the tree: its pids are not
    /// reused while the exited root's children still reference it.
    fn root_reaped(&mut self) -> bool {
        cfg!(unix) && self.child.try_wait().ok().flatten().is_some()
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if self.status.is_running() {
            self.log_event("stop");
        }
        // Sweep the whole tree first: the direct child may already be dead
        // while orphaned grandchildren (the real agent) live on.
        if !self.tree_killed {
            if let Some(pid) = self.child.process_id() {
                if self.root_reaped() {
                    #[cfg(unix)]
                    kill_group(pid);
                } else {
                    kill_tree_by_pid(pid);
                }
            }
        }
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
