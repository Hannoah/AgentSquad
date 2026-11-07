use crate::config::{pty_env, size, Kind, Project};
use crate::sys::{cpu_percent, kill_tree_by_pid, Usage};
use crate::ui::{pane_status_text, spinner_frame, status_glyph, status_style, uptime_label};
use anyhow::{Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty};
use ratatui::style::{Color, Style};
use std::{
    io::{Read, Write},
    num::NonZero,
    sync::mpsc::{self, Receiver},
    thread,
    time::Instant,
};

pub fn pane_title_name(p: &Project) -> String {
    if p.kind == Kind::Service && !p.command.is_empty() {
        p.command.join(" ")
    } else {
        p.name.clone()
    }
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgentPhase {
    Working,
    NeedsInput,
    Done,
    Idle,
}

impl AgentPhase {
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

pub fn is_needs_input(screen_text: &str) -> bool {
    for line in screen_text
        .lines()
        .rev()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(5)
    {
        let lower = line.to_ascii_lowercase();
        if lower.contains("[y/n]")
            || lower.contains("(y/n)")
            || lower.contains("[y/n/a]")
            || lower.contains("[y/n/d]")
            || lower.contains("(y/n/a)")
            || lower.contains("[yes/no]")
            || lower.contains("(yes/no)")
            || lower.contains("(y)es / (n)o")
        {
            return true;
        }
        if lower.contains("do you want to")
            || lower.contains("are you sure")
            || lower.contains("allow tool")
            || lower.contains("allow execution")
            || lower.contains("allow once")
            || lower.contains("allow always")
            || lower.contains("don't ask again")
            || lower.contains("esc to cancel")
            || lower.contains("proceed?")
            || lower.contains("approve?")
            || lower.contains("confirm?")
            || lower.contains("permission to")
            || lower.contains("press enter to continue")
            || lower.contains("press enter to proceed")
            || lower.contains("press any key")
        {
            return true;
        }
    }
    false
}

pub fn is_working_text(screen_text: &str) -> bool {
    for line in screen_text
        .lines()
        .rev()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(4)
    {
        let lower = line.to_ascii_lowercase();
        if lower.contains("esc to interrupt")
            || lower.contains("ctrl+c to interrupt")
            || lower.contains("thinking")
            || lower.contains("running tool")
            || lower.contains("executing")
            || lower.contains("searching")
            || lower.contains("generating")
            || lower.contains("compiling")
            || lower.contains("building")
            || lower.contains("reading")
            || lower.contains("writing")
            || lower.contains("editing")
            || lower.contains("analyzing")
            || lower.contains("pondering")
            || lower.contains("synthesizing")
        {
            return true;
        }
    }
    false
}

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
                    format!("{}{uptime_str}", spinner_frame(tick)),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ),
                AgentPhase::NeedsInput => (
                    format!("?{uptime_str}"),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ),
                AgentPhase::Done => (
                    format!("✔{uptime_str}"),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ),
                AgentPhase::Idle => (
                    format!("○{uptime_str}"),
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
/// Status segment for the terminal pane title: "◐ Running 7s".
/// Reuses the sidebar glyph and color language so both panels read as one.
pub fn stop_target<T>(order: &[usize], selected: usize, slots: &[Option<T>]) -> Option<usize> {
    order
        .get(selected)
        .copied()
        .filter(|&pi| slots.get(pi).and_then(|s| s.as_ref()).is_some())
}
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
pub struct Session {
    pub master: Box<dyn MasterPty + Send>,
    pub child: Box<dyn Child + Send + Sync>,
    pub writer: Box<dyn Write + Send>,
    pub rx: Receiver<Vec<u8>>,
    pub parser: vt100::Parser,
    pub status: String,
    pub scroll: usize,
    pub started: Option<Instant>,
    pub announced_exit: bool,
    pub usage: Option<Usage>,
    pub cpu_prev: u64,
    pub sampled: Option<Instant>,
    pub last_output: Option<Instant>,
    pub last_input: Option<Instant>,
    pub had_activity: bool,
    pub dismissed_exit: bool,
    pub cached_needs_input: bool,
    pub cached_working_text: bool,
}
impl Session {
    pub fn spawn(p: &Project, rows: u16, cols: u16) -> Result<Self> {
        let pair = native_pty_system().openpty(size(rows, cols))?;
        let mut cmd = CommandBuilder::new(&p.command[0]);
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
            .with_context(|| format!("Cannot start {}", p.command[0]))?;
        drop(pair.slave);
        // Bounded queue: at most 32 * 4096 bytes of pending output per session.
        let (tx, rx) = mpsc::sync_channel(32);
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
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
                500,
            ),
            status: "Running".into(),
            scroll: 0,
            started: Some(Instant::now()),
            announced_exit: false,
            usage: None,
            cpu_prev: 0,
            sampled: None,
            last_output: None,
            last_input: None,
            had_activity: false,
            dismissed_exit: false,
            cached_needs_input: false,
            cached_working_text: false,
        })
    }
    pub fn refresh_screen_flags(&mut self) {
        let screen_text = self.parser.screen().contents();
        self.cached_needs_input = is_needs_input(&screen_text);
        self.cached_working_text = is_working_text(&screen_text);
    }
    pub fn update(&mut self) -> bool {
        let mut changed = false;
        let mut received_output = false;
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
        }
        if received_output {
            self.refresh_screen_flags();
        }
        if self.status == "Running" {
            match self.child.try_wait() {
                Ok(Some(exit)) => {
                    self.status = format!("Exited ({})", exit.exit_code());
                    changed = true;
                }
                Err(e) => {
                    self.status = format!("Error: {e}");
                    changed = true;
                }
                _ => {}
            }
        }
        changed
    }
    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        self.master.resize(size(rows, cols))?;
        self.parser.set_size(
            NonZero::new(rows.max(1)).unwrap(),
            NonZero::new(cols.max(1)).unwrap(),
        );
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
        if self.status != "Running" {
            return AgentPhase::Idle;
        }
        let user_just_typed = self
            .last_input
            .is_some_and(|t| t.elapsed().as_millis() < 800);
        if !user_just_typed && self.cached_needs_input {
            return AgentPhase::NeedsInput;
        }
        let cpu_active = self.usage.is_some_and(|u| u.cpu > 5.0);
        let recent_output = self
            .last_output
            .is_some_and(|t| t.elapsed().as_millis() < 2500);
        let recent_input = self
            .last_input
            .is_some_and(|t| t.elapsed().as_millis() < 2500);
        if self.cached_working_text || recent_output || recent_input || cpu_active {
            return AgentPhase::Working;
        }
        if self.had_activity && self.last_input.is_some() {
            return AgentPhase::Done;
        }
        AgentPhase::Idle
    }
    pub fn scroll_by(&mut self, delta_rows: isize) {
        let max = self.parser.screen().scrollback_len() as isize;
        self.scroll = (self.scroll as isize + delta_rows).clamp(0, max) as usize;
        self.parser.screen_mut().set_scrollback(self.scroll);
    }
    /// Record a measurement of this session's process tree taken at `at`.
    /// CPU needs two samples, so it reads 0% on the first one. Results for
    /// a previous process (the session was restarted meanwhile) are dropped.
    pub fn apply_usage(&mut self, pid: u32, cpu_time: u64, mem: u64, at: Instant, cores: usize) {
        if self.status != "Running" || self.child.process_id() != Some(pid) {
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
    /// Forget usage once the session stops running.
    pub fn clear_usage(&mut self) {
        self.usage = None;
        self.sampled = None;
    }
    pub fn scroll_live(&mut self) {
        self.scroll = 0;
        self.parser.screen_mut().set_scrollback(0);
    }
}
pub fn scroll_lines_selected(
    sessions: &mut [Option<Session>],
    order: &[usize],
    selected: usize,
    lines: isize,
) {
    if let Some(&pi) = order.get(selected) {
        if let Some(Some(s)) = sessions.get_mut(pi) {
            if s.parser.screen().alternate_screen() {
                let seq: &[u8] = if lines > 0 { b"\x1b[5~" } else { b"\x1b[6~" };
                let _ = s.input(seq);
            } else {
                s.scroll_by(lines);
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
/// Start the project session unless it is already running.
/// Returns an error message when the spawn fails.
pub fn start_session(
    sessions: &mut [Option<Session>],
    projects: &[Project],
    pi: usize,
    rows: u16,
    cols: u16,
) -> Option<String> {
    let running = sessions
        .get(pi)
        .and_then(|o| o.as_ref())
        .map(|s| s.status == "Running")
        .unwrap_or(false);
    if running {
        return None;
    }
    restart_session(sessions, projects, pi, rows, cols)
}
impl Drop for Session {
    fn drop(&mut self) {
        // Sweep the whole tree first: the direct child may already be dead
        // while orphaned grandchildren (the real agent) live on.
        #[cfg(windows)]
        if let Some(pid) = self.child.process_id() {
            kill_tree_by_pid(pid);
        }
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
