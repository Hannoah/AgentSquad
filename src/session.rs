use crate::config::{is_bare_shell, pty_env, sanitize_chrome_line, size, Kind, Project};
use crate::ledger;
#[cfg(unix)]
use crate::sys::kill_group;
use crate::sys::{cpu_percent, Usage};
use crate::sys::{kill_tree_by_pid, kill_trees};
use crate::ui::{
    color as ui_color, pane_status_text, spinner_frame, status_glyph, status_style, uptime_label,
    Notice,
};
use anyhow::{Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
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

/// A fresh per-session file for the hooks' state reports.
fn new_state_file() -> std::path::PathBuf {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    std::env::temp_dir().join(format!(
        "agentsquad-{}-{}.state",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ))
}

/// What a hook wrote into the state file (`ledger::write_state`).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn parse_hook_state(text: &str) -> Option<AgentPhase> {
    parse_hook_report(text).map(|(phase, _)| phase)
}

/// Parse a state file report: `<state>` or `<state> <unix_nanos>`.
pub(crate) fn parse_hook_report(text: &str) -> Option<(AgentPhase, Option<std::time::SystemTime>)> {
    let mut parts = text.split_whitespace();
    let phase = match parts.next()? {
        ledger::STATE_DONE => AgentPhase::Done,
        ledger::STATE_ASKING => AgentPhase::NeedsInput,
        _ => return None,
    };
    let ts = match parts.next() {
        Some(ns_str) => {
            let ns: u128 = ns_str.parse().ok()?;
            let secs = u64::try_from(ns / 1_000_000_000).ok()?;
            let subsec = (ns % 1_000_000_000) as u32;
            Some(std::time::UNIX_EPOCH + Duration::new(secs, subsec))
        }
        None => None,
    };
    if parts.next().is_some() {
        return None;
    }
    Some((phase, ts))
}

/// Return lines added strictly after the previous snapshot's trailing anchor.
/// Uses the rightmost suffix match of `prev_anchor` (down to 3 lines when longer)
/// so duplicate output is never appended even as scrollback advances.
pub(crate) fn diff_new_lines(lines: &[String], prev_anchor: &[String]) -> Vec<String> {
    let trim_bounds = |slice: &[String]| -> Vec<String> {
        let start = slice
            .iter()
            .position(|l| !l.trim().is_empty())
            .unwrap_or(slice.len());
        let end = slice
            .iter()
            .rposition(|l| !l.trim().is_empty())
            .map_or(start, |i| i + 1);
        slice[start..end].to_vec()
    };
    if prev_anchor.is_empty() {
        return trim_bounds(lines);
    }
    let non_empty: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, _)| i)
        .collect();
    let min_len = prev_anchor.len().min(3);
    for suffix_len in (min_len..=prev_anchor.len()).rev() {
        let target = &prev_anchor[prev_anchor.len() - suffix_len..];
        if non_empty.len() < suffix_len {
            continue;
        }
        for k in (0..=non_empty.len() - suffix_len).rev() {
            let matches = (0..suffix_len).all(|j| lines[non_empty[k + j]] == target[j]);
            if matches {
                let after = non_empty[k + suffix_len - 1] + 1;
                return trim_bounds(&lines[after..]);
            }
        }
    }
    trim_bounds(lines)
}

/// The phase Claude Code's own hooks report, while it still holds: they say
/// when a turn ended (Stop) or a question is up (Notification), which no
/// screen scan can be sure of. Typing after the report means the user moved
/// on (answered, interrupted, new prompt), and visible work (`busy`) means
/// the agent did; either way the screen decides again.
pub(crate) fn hook_verdict(
    hook: Option<(AgentPhase, Instant)>,
    last_input: Option<Instant>,
    busy: bool,
) -> Option<AgentPhase> {
    let (phase, at) = hook?;
    (!busy && last_input.is_none_or(|t| t < at)).then_some(phase)
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
    let tail = tail_lines(screen_text, 6);
    !is_working_lines(&tail) && has_prompt_lines(&tail)
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

/// The prompt half of `is_needs_input`, for callers that already have
/// `tail_lines(screen_text, 6)`.
fn has_prompt_lines(tail: &[String]) -> bool {
    let lines: Vec<&String> = tail.iter().filter(|l| !is_code_or_diff_line(l)).collect();
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
        .map(|l| sanitize_chrome_line(l.trim()))
}

/// A status line led by a spinner glyph and an ellipsis: "✻ Pondering…".
fn is_spinner_line(line: &str) -> bool {
    let Some(first) = line.chars().next() else {
        return false;
    };
    let glyph = ('\u{2800}'..='\u{28ff}').contains(&first) || "✻✶✳✢✽✺✹✸✷".contains(first);
    glyph && (line.contains('…') || line.contains("..."))
}

fn is_working_lines(tail: &[String]) -> bool {
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
    for (idx, lower) in tail.iter().take(5).enumerate() {
        if has_busy_hint(lower) || is_spinner_line(lower) {
            return true;
        }
        // Verbs count only near the very bottom (last 2 lines) at line start,
        // so completed prose paragraphs above an idle prompt do not stick.
        if idx < 2 {
            let head = lower.trim_start_matches(|c: char| !c.is_alphanumeric());
            if VERBS.iter().any(|v| head.starts_with(v)) {
                return true;
            }
        }
    }
    false
}

pub fn is_working_text(screen_text: &str) -> bool {
    is_working_lines(&tail_lines(screen_text, 5))
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SearchMatch {
    pub line: usize,
    pub col_start: u16,
    pub col_end: u16,
}

fn find_row_matches(screen: &vt100::Screen, row: u16, cols: u16, q_lower: &str) -> Vec<(u16, u16)> {
    if q_lower.is_empty() {
        return Vec::new();
    }
    let mut text = String::new();
    let mut start_cols = Vec::new();
    let mut end_cols = Vec::new();
    for col in 0..cols {
        let Some(cell) = screen.cell(row, col) else {
            continue;
        };
        if cell.is_wide_continuation() {
            continue;
        }
        let s = if cell.has_contents() {
            cell.contents()
        } else {
            " "
        };
        let cell_w = if col + 1 < cols
            && screen
                .cell(row, col + 1)
                .is_some_and(|c| c.is_wide_continuation())
        {
            2
        } else {
            1
        };
        for _ in 0..s.len() {
            start_cols.push(col);
            end_cols.push(col + cell_w);
        }
        text.push_str(s);
    }
    let lower = text.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(rel) = lower[from..].find(q_lower) {
        let at = from + rel;
        let end = at + q_lower.len();
        if let (Some(&cs), Some(&ce)) = (start_cols.get(at), end_cols.get(end - 1)) {
            out.push((cs, ce));
        }
        from = end;
    }
    out
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
    // Fields drop in this order: `rx` before `writer`. Closing the channel
    // lets the reader drain the pty (see `spawn`), and the writer's drop
    // writes a newline and EOF whose echo needs room in the pty's output
    // queue; with a full channel and nobody reading, that write never returns.
    pub rx: Receiver<Vec<u8>>,
    pub writer: Box<dyn Write + Send>,
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
    /// When the pane last changed size; see `RESIZE_ECHO`.
    pub resized: Option<Instant>,
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
    /// File this session's Claude Code hooks report to (`ledger::STATE_ENV`).
    pub(crate) hook_file: Option<std::path::PathBuf>,
    /// Modification time and raw text of `hook_file` last read, and when checked.
    hook_seen: Option<std::time::SystemTime>,
    hook_seen_raw: Option<String>,
    hook_checked: Instant,
    /// The latest state the hooks reported, and when they wrote it.
    hook_phase: Option<(AgentPhase, Instant)>,
    /// Trailing non-empty lines from the last captured history snapshot (dedup anchor).
    history_anchor: Vec<String>,
    /// Partial line typed by the user into the PTY before pressing Enter.
    input_line_buf: String,
    /// Prompt lines sent to the PTY during the current task.
    task_prompts: Vec<String>,
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
        // The hooks run inside the agent and inherit its environment: this
        // names the file they tell this session's state through.
        let hook_file = hook.is_some().then(new_state_file);
        if let Some(f) = &hook_file {
            let _ = std::fs::remove_file(f);
            cmd.env(ledger::STATE_ENV, f);
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
            let mut listening = true;
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    // Once the session is gone, keep draining until the pty
                    // closes: a full output queue would block the writer's
                    // final EOF (and a flooding program) forever.
                    Ok(n) if listening => listening = tx.send(buf[..n].to_vec()).is_ok(),
                    Ok(_) => {}
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
            resized: None,
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
            hook_file,
            hook_seen: None,
            hook_seen_raw: None,
            hook_checked: Instant::now(),
            hook_phase: None,
            history_anchor: Vec::new(),
            input_line_buf: String::new(),
            task_prompts: Vec::new(),
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
    /// Non-empty trailing lines of the live screen for Quick Reply display.
    pub fn tail_lines_snapshot(&mut self, max_lines: u16) -> Vec<String> {
        let raw = self.tail_text(max_lines.saturating_add(6));
        let mut lines: Vec<String> = raw
            .lines()
            .map(str::trim_end)
            .filter(|l| !l.trim().is_empty())
            .map(str::to_string)
            .collect();
        let keep = usize::from(max_lines);
        if lines.len() > keep {
            lines.drain(..lines.len() - keep);
        }
        lines
    }
    /// Search scrollback and live screen case-insensitively (ASCII) and return
    /// all matching column spans in logical line order (oldest first).
    pub fn search_scrollback(&mut self, query: &str) -> Vec<SearchMatch> {
        let q_lower = query.to_ascii_lowercase();
        if q_lower.is_empty() {
            return Vec::new();
        }
        let saved = self.scroll;
        let total_sb = self.parser.screen().scrollback_len();
        let (rows_nz, cols_nz) = self.parser.screen().size();
        let (rows, cols) = (usize::from(rows_nz.get()), cols_nz.get());
        let total_lines = total_sb + rows;
        let mut out = Vec::new();
        let mut start = 0usize;
        while start < total_lines {
            let sb = total_sb.saturating_sub(start);
            self.parser.screen_mut().set_scrollback(sb);
            let base = total_sb - self.parser.screen().scrollback();
            for r in 0..rows {
                let line = base + r;
                if line < start {
                    continue;
                }
                for (col_start, col_end) in
                    find_row_matches(self.parser.screen(), r as u16, cols, &q_lower)
                {
                    out.push(SearchMatch {
                        line,
                        col_start,
                        col_end,
                    });
                }
            }
            start = base + rows;
        }
        self.parser.screen_mut().set_scrollback(saved);
        out
    }
    /// Matches currently visible on screen: `(vis_row, logical_line, col_start, col_end)`.
    pub fn visible_search_hits(&self, query: &str) -> Vec<(u16, usize, u16, u16)> {
        let q_lower = query.to_ascii_lowercase();
        if q_lower.is_empty() {
            return Vec::new();
        }
        let screen = self.parser.screen();
        let total_sb = screen.scrollback_len();
        let base = total_sb.saturating_sub(screen.scrollback());
        let (rows_nz, cols_nz) = screen.size();
        let (rows, cols) = (rows_nz.get(), cols_nz.get());
        let mut out = Vec::new();
        for r in 0..rows {
            let line = base + usize::from(r);
            for (cs, ce) in find_row_matches(screen, r, cols, &q_lower) {
                out.push((r, line, cs, ce));
            }
        }
        out
    }
    /// Scroll the viewport so logical `line` sits near the middle of the pane.
    pub fn scroll_to_logical_line(&mut self, line: usize) {
        let total_sb = self.parser.screen().scrollback_len();
        let rows = usize::from(self.parser.screen().size().0.get());
        let target = (total_sb + rows / 2).saturating_sub(line).min(total_sb);
        self.scroll = target;
        self.parser.screen_mut().set_scrollback(target);
        self.gen += 1;
    }
    /// Rescan the bottom of the live screen for prompt/working text. Only
    /// the last rows are read: that is all the detectors look at, and a
    /// full-screen text dump per output chunk was the hot spot.
    pub fn refresh_screen_flags(&mut self) {
        let tail = self.tail_text(12);
        let lines = tail_lines(&tail, 6);
        let working = is_working_lines(&lines);
        self.cached_working_text = working;
        self.cached_needs_input = !working && has_prompt_lines(&lines);
        let hook_asking = matches!(
            hook_verdict(self.hook_phase, self.last_input, false),
            Some(AgentPhase::NeedsInput)
        );
        self.prompt_hint = if self.cached_needs_input || (!working && hook_asking) {
            prompt_summary(&tail)
        } else {
            None
        };
        self.flags_stale = false;
        self.last_flags = Instant::now();
    }
    pub fn update(&mut self) -> bool {
        // ponytail: fixed window; real output in it is not counted either, the
        // next chunk after it is. Compare screens if a slow repaint slips past.
        const RESIZE_ECHO: Duration = Duration::from_millis(500);
        let mut changed = false;
        let mut received_output = false;
        let started = Instant::now();
        for i in 0..64 {
            match self.rx.try_recv() {
                Ok(bytes) => {
                    self.parser.process(&bytes);
                    // The terminal repaints after a resize; that is not the agent working.
                    if self.resized.is_none_or(|t| t.elapsed() >= RESIZE_ECHO) {
                        self.last_output = Some(Instant::now());
                        self.had_activity = true;
                    }
                    received_output = true;
                    changed = true;
                }
                Err(_) => break,
            }
            if i & 3 == 3 && started.elapsed() >= DRAIN_BUDGET {
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
        if self.hook_file.is_some() && self.hook_checked.elapsed() >= FLAGS_EVERY {
            self.hook_checked = Instant::now();
            changed |= self.read_hook_state();
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
    /// Pick up a state the hooks wrote since the last look. True when one was.
    pub(crate) fn read_hook_state(&mut self) -> bool {
        let Some(file) = &self.hook_file else {
            return false;
        };
        let Ok(mtime) = std::fs::metadata(file).and_then(|m| m.modified()) else {
            return false;
        };
        // Only skip reading when the mtime is both unchanged and safely older
        // than coarse filesystem timestamp ticks (e.g. 15.6 ms on NTFS).
        if self.hook_seen == Some(mtime)
            && mtime
                .elapsed()
                .is_ok_and(|d| d >= Duration::from_millis(50))
        {
            return false;
        }
        let Ok(raw) = std::fs::read_to_string(file) else {
            return false;
        };
        if self.hook_seen_raw.as_deref() == Some(raw.as_str()) {
            self.hook_seen = Some(mtime);
            return false;
        }
        // Caught between truncate and write: unrecognized, look again next pass.
        let Some((phase, stamped_at)) = parse_hook_report(&raw) else {
            return false;
        };
        self.hook_seen = Some(mtime);
        self.hook_seen_raw = Some(raw);
        let ts = stamped_at.unwrap_or(mtime);
        let age = ts.elapsed().unwrap_or_default();
        let at = Instant::now().checked_sub(age).unwrap_or_else(Instant::now);
        self.hook_phase = Some((phase, at));
        if phase == AgentPhase::NeedsInput
            && self.prompt_hint.is_none()
            && !self.cached_working_text
        {
            let tail = self.tail_text(12);
            self.prompt_hint = prompt_summary(&tail);
        } else if phase == AgentPhase::Done && !self.cached_needs_input {
            self.prompt_hint = None;
        }
        true
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
        self.resized = Some(Instant::now());
        self.refresh_screen_flags();
        Ok(())
    }
    pub fn input(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()?;
        self.record_input_for_history(bytes);
        self.last_input = Some(Instant::now());
        self.had_activity = true;
        Ok(())
    }
    fn record_input_for_history(&mut self, bytes: &[u8]) {
        // Strip bracketed paste wrappers if present; drop pure control/escape sequences.
        let inner = if let Some(rest) = bytes.strip_prefix(b"\x1b[200~") {
            rest.strip_suffix(b"\x1b[201~").unwrap_or(rest)
        } else if bytes.starts_with(b"\x1b") {
            return;
        } else {
            bytes
        };
        let Ok(text) = std::str::from_utf8(inner) else {
            return;
        };
        for c in text.chars() {
            match c {
                '\r' => {
                    let line = std::mem::take(&mut self.input_line_buf);
                    let trimmed = line.trim();
                    if !trimmed.is_empty() && self.task_prompts.len() < 20 {
                        self.task_prompts.push(trimmed.to_string());
                    }
                }
                '\n' => {
                    if !self.input_line_buf.is_empty() && self.input_line_buf.len() < 4000 {
                        self.input_line_buf.push(' ');
                    }
                }
                '\x08' | '\x7f' => {
                    self.input_line_buf.pop();
                }
                '\x03' | '\x15' => {
                    self.input_line_buf.clear();
                }
                _ if !c.is_control() && self.input_line_buf.len() < 4000 => {
                    self.input_line_buf.push(c);
                }
                _ => {}
            }
        }
    }
    /// Take all user prompt lines recorded since the previous task.
    pub fn take_task_prompts(&mut self) -> Vec<String> {
        std::mem::take(&mut self.task_prompts)
    }
    /// Capture all new PTY scrollback/screen lines added since the previous snapshot.
    pub fn capture_new_output(&mut self) -> String {
        let saved = self.parser.screen().scrollback();
        let total_sb = self.parser.screen().scrollback_len();
        let (rows_nz, cols_nz) = self.parser.screen().size();
        let (rows, cols) = (usize::from(rows_nz.get()), cols_nz.get());
        let total_lines = total_sb + rows;
        let mut all_lines = Vec::with_capacity(total_lines);
        let mut start = 0usize;
        while start < total_lines {
            let sb = total_sb.saturating_sub(start);
            self.parser.screen_mut().set_scrollback(sb);
            let base = total_sb - self.parser.screen().scrollback();
            for r in 0..rows {
                let line = base + r;
                if line < start {
                    continue;
                }
                let text = self
                    .parser
                    .screen()
                    .contents_between(r as u16, 0, r as u16, cols);
                all_lines.push(text.trim_end().to_string());
            }
            start = base + rows;
        }
        self.parser.screen_mut().set_scrollback(saved);
        while all_lines.last().is_some_and(|l| l.is_empty()) {
            all_lines.pop();
        }
        let mut new_lines = diff_new_lines(&all_lines, &self.history_anchor);
        let anchor: Vec<String> = all_lines
            .iter()
            .filter(|l| !l.trim().is_empty())
            .rev()
            .take(8)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if !anchor.is_empty() {
            self.history_anchor = anchor;
        }
        if new_lines.len() > 2000 {
            new_lines.drain(..new_lines.len() - 2000);
        }
        let mut out = new_lines.join("\n");
        const MAX_BYTES: usize = 200 * 1024;
        if out.len() > MAX_BYTES {
            let mut cut = out.len() - MAX_BYTES;
            while !out.is_char_boundary(cut) {
                cut += 1;
            }
            out.drain(..cut);
        }
        out
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
        if let Some(p) = hook_verdict(
            self.hook_phase,
            self.last_input,
            self.cached_working_text && output_streaming,
        ) {
            return p;
        }
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
    /// previous process, like `apply_usage`. Control characters are dropped:
    /// the label is drawn into the pane title, and process names come from
    /// programs the user runs.
    pub fn apply_label(&mut self, pid: u32, label: Option<String>) {
        if self.status.is_running() && self.child.process_id() == Some(pid) {
            self.fg_name = label
                .map(|l| sanitize_chrome_line(&l))
                .filter(|l| !l.is_empty());
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
    /// Paint the terminal screen into `buf` at `area`. Cells are populated
    /// directly into the cached `Buffer` (reusing its allocation when `area`
    /// matches) and blitted row-by-row so spinner ticks copy without allocating
    /// intermediate `Line`/`Span`/`String` buffers.
    pub fn paint(&self, buf: &mut Buffer, area: Rect) {
        let mut cache = self.render.borrow_mut();
        let fresh = cache
            .as_ref()
            .is_some_and(|c| c.gen == self.gen && c.area == area);
        if !fresh {
            let mut cells = match cache.take() {
                Some(mut prev) if prev.area == area => {
                    prev.buf.reset();
                    prev.buf
                }
                _ => Buffer::empty(area),
            };
            paint_screen_to_buffer(self.parser.screen(), &mut cells, area);
            *cache = Some(RenderCache {
                gen: self.gen,
                area,
                buf: cells,
            });
        }
        if let Some(c) = cache.as_ref() {
            let clamped = area.intersection(buf.area);
            let row_len = usize::from(clamped.width);
            if row_len == 0 {
                return;
            }
            let src_w = usize::from(c.buf.area.width);
            let dst_w = usize::from(buf.area.width);
            let src_x = usize::from(clamped.x.saturating_sub(c.buf.area.x));
            let dst_x = usize::from(clamped.x.saturating_sub(buf.area.x));
            for y in clamped.top()..clamped.bottom() {
                let src_start = usize::from(y - c.buf.area.y) * src_w + src_x;
                let dst_start = usize::from(y - buf.area.y) * dst_w + dst_x;
                buf.content[dst_start..dst_start + row_len]
                    .clone_from_slice(&c.buf.content[src_start..src_start + row_len]);
            }
        }
    }
}

fn paint_screen_to_buffer(screen: &vt100::Screen, cells: &mut Buffer, area: Rect) {
    let w = usize::from(area.width);
    for row in 0..area.height {
        let row_start = usize::from(row) * w;
        for col in 0..area.width {
            let Some(cell) = screen.cell(row, col) else {
                continue;
            };
            if cell.is_wide_continuation() {
                continue;
            }
            let mut style = Style::default()
                .fg(ui_color(cell.fgcolor()))
                .bg(ui_color(cell.bgcolor()));
            if cell.bold() {
                style = style.add_modifier(Modifier::BOLD);
            }
            if cell.dim() {
                style = style.add_modifier(Modifier::DIM);
            }
            if cell.italic() {
                style = style.add_modifier(Modifier::ITALIC);
            }
            if cell.underline() {
                style = style.add_modifier(Modifier::UNDERLINED);
            }
            if cell.inverse() {
                style = style.add_modifier(Modifier::REVERSED);
            }
            let dst = &mut cells.content[row_start + usize::from(col)];
            if cell.has_contents() {
                dst.set_symbol(cell.contents());
            } else {
                dst.set_symbol(" ");
            }
            dst.set_style(style);
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
/// Sessions being torn down on a worker.
static STOPPING: AtomicUsize = AtomicUsize::new(0);
/// Stop `s` off the UI thread: killing a tree walks the process table.
pub fn stop_in_background(s: Session) {
    STOPPING.fetch_add(1, Ordering::SeqCst);
    thread::spawn(move || {
        drop(s);
        STOPPING.fetch_sub(1, Ordering::SeqCst);
    });
}
/// Wait (up to `limit`) for those workers. Call before exit, or quitting
/// right after a stop could leave a half-killed process tree behind.
pub fn wait_for_stops(limit: Duration) {
    let end = Instant::now() + limit;
    while STOPPING.load(Ordering::SeqCst) > 0 && Instant::now() < end {
        thread::sleep(Duration::from_millis(10));
    }
}
pub fn slot_running(sessions: &[Option<Session>], pi: usize) -> bool {
    sessions
        .get(pi)
        .and_then(|o| o.as_ref())
        .is_some_and(|s| s.status.is_running())
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
    /// group may be swept. Windows keeps walking the tree: the pty child
    /// holds an open handle to the root, so its pid is not reused while this
    /// session lives (and `kill_trees` re-checks each descendant's identity).
    fn root_reaped(&mut self) -> bool {
        cfg!(unix) && self.child.try_wait().ok().flatten().is_some()
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if let Some(f) = &self.hook_file {
            let _ = std::fs::remove_file(f);
        }
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
