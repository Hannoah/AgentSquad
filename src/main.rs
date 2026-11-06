use anyhow::{bail, Context, Result};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, Paragraph},
    Terminal,
};
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    io::{self, IsTerminal, Read, Write},
    num::NonZero,
    path::PathBuf,
    str::FromStr,
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
#[serde(rename_all = "lowercase")]
enum Kind {
    #[default]
    Agent,
    Service,
}
impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Agent => "agent",
            Kind::Service => "service",
        }
    }
}
impl FromStr for Kind {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "agent" => Ok(Kind::Agent),
            "service" => Ok(Kind::Service),
            other => bail!("Unknown kind: {other}. Use agent or service"),
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
struct Project {
    name: String,
    path: PathBuf,
    command: Vec<String>,
    #[serde(default)]
    kind: Kind,
}
#[derive(Default, Serialize, Deserialize)]
struct Config {
    #[serde(default)]
    projects: Vec<Project>,
}
fn config_path() -> Result<PathBuf> {
    if let Some(p) = env::var_os("AGENTSQUAD_CONFIG") {
        return Ok(p.into());
    }
    let base = env::var_os("LOCALAPPDATA")
        .or_else(|| env::var_os("XDG_CONFIG_HOME"))
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))
        .context("Set AGENTSQUAD_CONFIG to a writable config file")?;
    Ok(base.join("AgentSquad").join("config.toml"))
}
fn load() -> Result<Config> {
    let p = config_path()?;
    if !p.exists() {
        return Ok(Config::default());
    }
    let text = fs::read_to_string(&p).with_context(|| format!("Cannot read {}", p.display()))?;
    toml::from_str(&text).with_context(|| format!("Cannot parse {}", p.display()))
}
fn save(c: &Config) -> Result<()> {
    let p = config_path()?;
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(p, toml::to_string_pretty(c)?)?;
    Ok(())
}
fn shell() -> Vec<String> {
    if cfg!(windows) {
        vec!["cmd.exe".into()]
    } else {
        vec!["sh".into()]
    }
}
/// Fold in projects registered by another process (e.g. `squad add` in a
/// second terminal) so a later save does not clobber them. Only appends
/// names we don't have, keeping `sessions` aligned by index.
fn merge_projects(c: &mut Config, sessions: &mut Vec<Option<Session>>, disk: Config) {
    for p in disk.projects {
        if !c
            .projects
            .iter()
            .any(|q| q.name.eq_ignore_ascii_case(&p.name))
        {
            c.projects.push(p);
            sessions.push(None);
        }
    }
}
/// Environment advertised to sessions. `COLORTERM` unlocks 24-bit color
/// for apps like Claude that downgrade without it; the parser and the
/// renderer already pass RGB through.
fn pty_env() -> [(&'static str, &'static str); 2] {
    [("TERM", "xterm-256color"), ("COLORTERM", "truecolor")]
}
fn size(rows: u16, cols: u16) -> PtySize {
    PtySize {
        rows: rows.max(1),
        cols: cols.max(1),
        pixel_width: 0,
        pixel_height: 0,
    }
}
/// Sidebar order: all agents first, then all services. Positions in this
/// vector are what the manager selection index refers to.
fn display_order(projects: &[Project]) -> Vec<usize> {
    let mut order = Vec::with_capacity(projects.len());
    order.extend((0..projects.len()).filter(|&i| projects[i].kind == Kind::Agent));
    order.extend((0..projects.len()).filter(|&i| projects[i].kind == Kind::Service));
    order
}
/// Validate raw add-form input into a Project without touching disk state.
fn build_project(
    existing: &[Project],
    path_raw: &str,
    kind: Kind,
    command_raw: &str,
) -> Result<Project, String> {
    let trimmed = path_raw.trim();
    if trimmed.is_empty() {
        return Err("Path is empty.".into());
    }
    let path = fs::canonicalize(trimmed).map_err(|e| format!("Bad path: {e}"))?;
    if !path.is_dir() {
        return Err("Project path must be a directory.".into());
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if name.is_empty() {
        return Err("Cannot use filesystem root as project.".into());
    }
    if existing.iter().any(|p| p.name.eq_ignore_ascii_case(&name)) {
        return Err(format!("Project name already registered: {name}"));
    }
    let parts: Vec<String> = command_raw.split_whitespace().map(str::to_string).collect();
    let command = if parts.is_empty() { shell() } else { parts };
    Ok(Project {
        name,
        path,
        command,
        kind,
    })
}
/// Filtered view: display order narrowed to projects whose name or
/// command contains the query (case-insensitive). Empty query = all.
fn apply_filter(projects: &[Project], query: &str) -> Vec<usize> {
    let q = query.to_ascii_lowercase();
    display_order(projects)
        .into_iter()
        .filter(|&i| {
            q.is_empty()
                || projects[i].name.to_ascii_lowercase().contains(&q)
                || projects[i]
                    .command
                    .join(" ")
                    .to_ascii_lowercase()
                    .contains(&q)
        })
        .collect()
}
fn fit(s: &str, w: usize) -> String {
    s.chars().take(w).collect()
}
/// Single-line editor with a movable cursor for the manager's input fields.
/// Stored as chars so cursor motion never splits UTF-8.
#[derive(Default)]
struct LineInput {
    text: Vec<char>,
    cursor: usize,
}
impl LineInput {
    fn new() -> Self {
        Self::default()
    }
    fn as_str(&self) -> String {
        self.text.iter().collect()
    }
    fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
    fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }
    fn take(&mut self) -> String {
        let out = self.as_str();
        self.clear();
        out
    }
    fn insert(&mut self, ch: char) {
        self.text.insert(self.cursor.min(self.text.len()), ch);
        self.cursor = (self.cursor + 1).min(self.text.len());
    }
    fn backspace(&mut self) {
        if self.cursor > 0 && !self.text.is_empty() {
            self.cursor -= 1;
            self.text.remove(self.cursor.min(self.text.len()));
        }
    }
    fn delete(&mut self) {
        if self.cursor < self.text.len() {
            self.text.remove(self.cursor);
        }
    }
    fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }
    fn move_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.text.len());
    }
    fn move_home(&mut self) {
        self.cursor = 0;
    }
    fn move_end(&mut self) {
        self.cursor = self.text.len();
    }
    /// (before, under-cursor, after) for rendering a block cursor.
    fn render_parts(&self) -> (String, char, String) {
        let at = self.text.get(self.cursor).copied().unwrap_or(' ');
        let before: String = self.text[..self.cursor.min(self.text.len())]
            .iter()
            .collect();
        let after: String = if self.cursor < self.text.len() {
            self.text[self.cursor + 1..].iter().collect()
        } else {
            String::new()
        };
        (before, at, after)
    }
}
const SPINNER: [char; 4] = ['◐', '◑', '◒', '◓'];
fn spinner_frame(tick: u64) -> char {
    SPINNER[(tick as usize) % SPINNER.len()]
}
fn status_glyph(status: &str, tick: u64) -> char {
    if status == "Running" {
        spinner_frame(tick)
    } else {
        '●'
    }
}
/// Short uptime label for a running session: "7s", "12m", "3h".
fn uptime_label(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h", secs / 3600)
    }
}
fn status_style(status: &str) -> Style {
    if status == "Running" {
        Style::default().fg(Color::Green)
    } else if status.starts_with("Exited") {
        Style::default().fg(Color::Yellow)
    } else if status.starts_with("Error") {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}
fn help_lines() -> Vec<&'static str> {
    vec![
        "Enter      start / focus session",
        "/          send a line to session",
        "a          add project",
        "f          filter projects",
        "Shift+PgUp/Dn scroll history",
        "r          restart session",
        "s          start all stopped sessions",
        "x          stop session (asks to confirm)",
        "n/p, arrows  select project",
        "1-9        jump to project",
        "Esc        terminal back to manager",
        "Ctrl+G,Esc send Esc to terminal",
        "q          quit (confirms when sessions exist)",
        "?/Esc      close this help",
    ]
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Manager,
    Focus,
    Send,
    Filter,
    Add,
    Help,
}
/// Status-bar input row: prompt + editable text with a block cursor + hint.
fn input_line(prompt: String, input: &LineInput, hint: String) -> Vec<Span<'static>> {
    let (before, at, after) = input.render_parts();
    vec![
        Span::raw(prompt),
        Span::raw(before),
        Span::styled(
            at.to_string(),
            Style::default().add_modifier(ratatui::style::Modifier::REVERSED),
        ),
        Span::raw(after + &hint),
    ]
}
fn mode_of(focus: bool, sending: bool, filtering: bool, adding: bool, show_help: bool) -> Mode {
    if show_help {
        Mode::Help
    } else if adding {
        Mode::Add
    } else if filtering {
        Mode::Filter
    } else if sending {
        Mode::Send
    } else if focus {
        Mode::Focus
    } else {
        Mode::Manager
    }
}
fn mode_chip(m: Mode) -> (&'static str, Style) {
    match m {
        Mode::Manager => (" MANAGER ", Style::default().fg(Color::Cyan)),
        Mode::Focus => (" INPUT ", Style::default().fg(Color::Green)),
        Mode::Send => (" SEND ", Style::default().fg(Color::Yellow)),
        Mode::Filter => (" FILTER ", Style::default().fg(Color::Magenta)),
        Mode::Add => (" ADD ", Style::default().fg(Color::Blue)),
        Mode::Help => (" HELP ", Style::default().fg(Color::White)),
    }
}
fn mode_hints(m: Mode) -> &'static str {
    match m {
        Mode::Manager => "Enter focus | / send | a add | f filter | r restart | s start all | n/p select | x stop | q quit | ? help",
        Mode::Focus => "Esc manager | type to input",
        Mode::Send => "Enter sends | Esc cancels",
        Mode::Filter => "type to narrow | Enter keeps | Esc clears",
        Mode::Add => "Tab next | Space kind | Enter save | Esc cancel",
        Mode::Help => "?/Esc closes",
    }
}
/// Digit key to selection index within the visible order.
fn jump_to(order_len: usize, ch: char) -> Option<usize> {
    if ch.is_ascii_digit() && ch != '0' {
        let n = (ch as u8 - b'0') as usize;
        if n <= order_len.min(9) {
            return Some(n - 1);
        }
    }
    None
}
/// Key outcome while a confirmation modal is pending:
/// `Some(true)` confirms, `Some(false)` cancels, `None` ignores the key.
fn confirm_key(code: KeyCode) -> Option<bool> {
    match code {
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => Some(true),
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => Some(false),
        _ => None,
    }
}
fn confirm_stop_prompt(name: &str) -> String {
    format!("Stop session '{name}'?")
}
fn confirm_quit_prompt(n: usize) -> String {
    format!("Quit and discard {n} session(s)?")
}
/// Sessions that quitting would discard (running or finished output).
fn live_session_count<T>(sessions: &[Option<T>]) -> usize {
    sessions.iter().flatten().count()
}
fn empty_state_lines(has_projects: bool) -> Vec<String> {
    if has_projects {
        vec![
            "No session running.".into(),
            "".into(),
            "Press Enter to start the selected project.".into(),
        ]
    } else {
        vec![
            "No projects yet.".into(),
            "".into(),
            "Press Enter or a, then type a folder path to add your first project.".into(),
            "Or from your shell: squad add <path> [-- command]".into(),
            "".into(),
            "Then press Enter to start it.".into(),
        ]
    }
}
fn centered_rect(w_pct: u16, h_pct: u16, area: ratatui::layout::Rect) -> ratatui::layout::Rect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - h_pct) / 2),
            Constraint::Percentage(h_pct),
            Constraint::Percentage(100 - h_pct - (100 - h_pct) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - w_pct) / 2),
            Constraint::Percentage(w_pct),
            Constraint::Percentage(100 - w_pct - (100 - w_pct) / 2),
        ])
        .split(v[1])[1]
}
struct Session {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    rx: Receiver<Vec<u8>>,
    parser: vt100::Parser,
    status: String,
    scroll: usize,
    started: Option<Instant>,
}
impl Session {
    fn spawn(p: &Project, rows: u16, cols: u16) -> Result<Self> {
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
        })
    }
    fn update(&mut self) -> bool {
        let mut changed = false;
        for _ in 0..32 {
            match self.rx.try_recv() {
                Ok(bytes) => {
                    self.parser.process(&bytes);
                    changed = true;
                }
                Err(_) => break,
            }
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
    fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        self.master.resize(size(rows, cols))?;
        self.parser.set_size(
            NonZero::new(rows.max(1)).unwrap(),
            NonZero::new(cols.max(1)).unwrap(),
        );
        Ok(())
    }
    fn input(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()?;
        Ok(())
    }
    fn scroll_by(&mut self, delta_rows: isize) {
        let max = self.parser.screen().scrollback_len() as isize;
        self.scroll = (self.scroll as isize + delta_rows).clamp(0, max) as usize;
        self.parser.screen_mut().set_scrollback(self.scroll);
    }
    fn scroll_live(&mut self) {
        self.scroll = 0;
        self.parser.screen_mut().set_scrollback(0);
    }
}
/// Move the visible viewport of the selected session's scrollback.
/// Positive `pages` scrolls up (into history); negative scrolls down.
fn scroll_selected(
    sessions: &mut [Option<Session>],
    order: &[usize],
    selected: usize,
    page_rows: usize,
    pages: isize,
) {
    if let Some(&pi) = order.get(selected) {
        if let Some(Some(s)) = sessions.get_mut(pi) {
            s.scroll_by((page_rows.max(1) as isize) * pages);
        }
    }
}
/// Sidebar title: plain normally, `AgentSquad vX.Y.Z (shown/total)`
/// while a filter hides projects.
fn list_title(total: usize, shown: usize) -> String {
    let base = format!("AgentSquad v{}", env!("CARGO_PKG_VERSION"));
    if shown == total {
        base
    } else {
        format!("{base} ({shown}/{total})")
    }
}
/// Group color, reused for headers and the terminal title kind chip.
fn kind_color(kind: Kind) -> Color {
    match kind {
        Kind::Agent => Color::Cyan,
        Kind::Service => Color::Magenta,
    }
}
/// (running, total) projects of one kind. `running[i]` mirrors
/// `projects[i]` and reports whether that project is up right now.
fn group_counts(projects: &[Project], running: &[bool], kind: Kind) -> (usize, usize) {
    let mut total = 0;
    let mut up = 0;
    for (i, p) in projects.iter().enumerate() {
        if p.kind == kind {
            total += 1;
            if running.get(i).copied().unwrap_or(false) {
                up += 1;
            }
        }
    }
    (up, total)
}
/// Mouse wheel notch to scrolled lines: up moves into history.
fn wheel_lines(kind: MouseEventKind) -> Option<isize> {
    match kind {
        MouseEventKind::ScrollUp => Some(3),
        MouseEventKind::ScrollDown => Some(-3),
        _ => None,
    }
}
fn scroll_lines_selected(
    sessions: &mut [Option<Session>],
    order: &[usize],
    selected: usize,
    lines: isize,
) {
    if let Some(&pi) = order.get(selected) {
        if let Some(Some(s)) = sessions.get_mut(pi) {
            s.scroll_by(lines);
        }
    }
}
fn scroll_live_selected(sessions: &mut [Option<Session>], order: &[usize], selected: usize) {
    if let Some(&pi) = order.get(selected) {
        if let Some(Some(s)) = sessions.get_mut(pi) {
            s.scroll_live();
        }
    }
}
/// Stop any existing session for `pi` and spawn a fresh one.
/// Returns an error message when the spawn fails.
fn restart_session(
    sessions: &mut [Option<Session>],
    projects: &[Project],
    pi: usize,
    rows: u16,
    cols: u16,
) -> Option<String> {
    if let Some(slot) = sessions.get_mut(pi) {
        *slot = None;
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
fn start_session(
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
/// Best-effort kill of the whole process tree.
///
/// Agent sessions are usually grandchildren (e.g. `cmd.exe -> node.exe` for
/// `cmd /d /c claude`). Terminating only the direct child orphans the real
/// agent, which keeps running (and keeps its ports). `taskkill /T /F` takes
/// down the tree; on failure the caller falls back to `Child::kill`.
#[cfg(windows)]
fn kill_tree(child: &dyn Child) -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let Some(pid) = child.process_id() else {
        return false;
    };
    std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
impl Drop for Session {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            #[cfg(windows)]
            let tree_killed = kill_tree(self.child.as_ref());
            #[cfg(not(windows))]
            let tree_killed = false;
            if !tree_killed {
                let _ = self.child.kill();
            }
            let _ = self.child.wait();
        }
    }
}
fn color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(n) => Color::Indexed(n),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}
/// CSI cursor key: modified keys always use the `CSI 1 ; param` form,
/// unmodified keys honor application-cursor mode.
fn arrow_bytes(letter: u8, app_cursor: bool, shift: bool, ctrl: bool) -> Vec<u8> {
    if shift || ctrl {
        let param = 1 + (shift as u8) + 4 * (ctrl as u8);
        format!("\x1b[1;{param}{}", letter as char).into_bytes()
    } else if app_cursor {
        vec![27, b'O', letter]
    } else {
        vec![27, b'[', letter]
    }
}
/// `CSI n ~` key (Delete/Insert/PageUp/PageDown) with xterm modifiers.
fn tilde_bytes(n: u8, shift: bool, ctrl: bool) -> Vec<u8> {
    if shift || ctrl {
        let param = 1 + (shift as u8) + 4 * (ctrl as u8);
        format!("\x1b[{n};{param}~").into_bytes()
    } else {
        format!("\x1b[{n}~").into_bytes()
    }
}
/// Plain Esc leaves terminal focus; modified Esc still reaches the app.
fn focus_exit_key(code: KeyCode, mods: KeyModifiers) -> bool {
    code == KeyCode::Esc && mods == KeyModifiers::NONE
}
fn key_bytes(k: KeyEvent, app_cursor: bool) -> Vec<u8> {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let shift = k.modifiers.contains(KeyModifiers::SHIFT);
    let mut bytes = match k.code {
        KeyCode::Char(c) if ctrl && c.is_ascii() => vec![(c.to_ascii_lowercase() as u8) & 0x1f],
        KeyCode::Char(c) => c.to_string().into_bytes(),
        KeyCode::Enter => vec![13],
        KeyCode::Backspace => vec![127],
        KeyCode::Tab => vec![9],
        KeyCode::Esc => vec![27],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Up => arrow_bytes(b'A', app_cursor, shift, ctrl),
        KeyCode::Down => arrow_bytes(b'B', app_cursor, shift, ctrl),
        KeyCode::Right => arrow_bytes(b'C', app_cursor, shift, ctrl),
        KeyCode::Left => arrow_bytes(b'D', app_cursor, shift, ctrl),
        KeyCode::Home => arrow_bytes(b'H', false, shift, ctrl),
        KeyCode::End => arrow_bytes(b'F', false, shift, ctrl),
        KeyCode::Delete => tilde_bytes(3, shift, ctrl),
        KeyCode::Insert => tilde_bytes(2, shift, ctrl),
        KeyCode::PageUp => tilde_bytes(5, shift, ctrl),
        KeyCode::PageDown => tilde_bytes(6, shift, ctrl),
        _ => Vec::new(),
    };
    if k.modifiers.contains(KeyModifiers::ALT) && !bytes.is_empty() {
        bytes.insert(0, 27);
    }
    bytes
}
struct ScreenGuard;
impl Drop for ScreenGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
    }
}
fn run(mut c: Config, initial: Option<usize>) -> Result<()> {
    if !io::stdin().is_terminal() {
        bail!("squad needs an interactive terminal");
    }
    enable_raw_mode()?;
    let _guard = ScreenGuard;
    execute!(io::stdout(), EnterAlternateScreen)?;
    execute!(io::stdout(), EnableMouseCapture)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut sessions: Vec<Option<Session>> = (0..c.projects.len()).map(|_| None).collect();
    let mut order = display_order(&c.projects);
    let mut selected = initial
        .and_then(|i| order.iter().position(|&x| x == i))
        .unwrap_or(0);
    let mut focus = false;
    let mut prefix = false;
    let mut sending = false;
    let mut send_buf = LineInput::new();
    let mut show_help = false;
    let mut filtering = false;
    let mut filter_buf = LineInput::new();
    let mut adding = false;
    let mut add_field: usize = 0;
    let mut add_path = LineInput::new();
    let mut add_kind = Kind::Agent;
    let mut add_cmd = LineInput::new();
    let mut add_err = String::new();
    let mut confirm_stop: Option<usize> = None;
    let mut confirm_quit = false;
    let mut message = mode_hints(Mode::Manager).to_string();
    let mut dirty = true;
    let mut first = initial.is_some();
    let mut tick: u64 = 0;
    loop {
        let area = terminal.size()?;
        let sidebar = if area.width >= 60 { 24 } else { 0 };
        let rows = area.height.saturating_sub(3).max(1);
        let cols = area.width.saturating_sub(sidebar).saturating_sub(2).max(1);
        tick = tick.wrapping_add(1);
        if tick.is_multiple_of(3) {
            dirty = true;
        }
        if first {
            first = false;
            if let Some(&pi) = order.get(selected) {
                match start_session(&mut sessions, &c.projects, pi, rows, cols) {
                    None => focus = true,
                    Some(e) => message = e,
                }
            }
        }
        for s in sessions.iter_mut().flatten() {
            dirty |= s.update();
        }
        if dirty {
            terminal.draw(|f| {
                let vertical = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(1), Constraint::Length(1)])
                    .split(f.area());
                let panels = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Length(sidebar), Constraint::Min(1)])
                    .split(vertical[0]);
                if sidebar > 0 {
                    let sel_pi = order.get(selected).copied();
                    let inner_w = (sidebar as usize).saturating_sub(2);
                    let mut items: Vec<ListItem> = Vec::new();
                    let mut n = 0;
                    let running: Vec<bool> = sessions
                        .iter()
                        .map(|o| o.as_ref().map(|s| s.status == "Running").unwrap_or(false))
                        .collect();
                    for (title, k) in [("Agents", Kind::Agent), ("Services", Kind::Service)] {
                        let (up, total) = group_counts(&c.projects, &running, k);
                        items.push(
                            ListItem::new(format!("{title} ({up}/{total})")).style(
                                Style::default()
                                    .fg(kind_color(k))
                                    .add_modifier(ratatui::style::Modifier::BOLD),
                            ),
                        );
                        let mut any = false;
                        for &pi in order.iter().filter(|&&i| c.projects[i].kind == k) {
                            any = true;
                            n += 1;
                            let p = &c.projects[pi];
                            let status = sessions[pi]
                                .as_ref()
                                .map(|s| s.status.as_str())
                                .unwrap_or("Stopped");
                            let mut second = format!("  {status}");
                            if status == "Running" {
                                if let Some(s) = sessions[pi].as_ref() {
                                    if let Some(started) = s.started {
                                        second.push(' ');
                                        second.push_str(&uptime_label(started.elapsed().as_secs()));
                                    }
                                }
                            }
                            if k == Kind::Service {
                                second.push(' ');
                                second.push_str(&p.command.join(" "));
                            }
                            let mark = if Some(pi) == sel_pi { ">" } else { " " };
                            let row_style = if Some(pi) == sel_pi {
                                Style::default()
                                    .fg(Color::Cyan)
                                    .bg(Color::DarkGray)
                                    .add_modifier(ratatui::style::Modifier::BOLD)
                            } else if status == "Running" {
                                Style::default().add_modifier(ratatui::style::Modifier::BOLD)
                            } else {
                                Style::default().add_modifier(ratatui::style::Modifier::DIM)
                            };
                            let status_span_style = if Some(pi) == sel_pi {
                                row_style
                            } else {
                                status_style(status)
                            };
                            let prefix = format!("{mark}{n} ");
                            let name_w = inner_w.saturating_sub(prefix.chars().count() + 2);
                            items.push(ListItem::new(Line::from(vec![
                                Span::styled(prefix, row_style),
                                Span::styled(
                                    format!("{} ", status_glyph(status, tick)),
                                    status_span_style,
                                ),
                                Span::styled(fit(&p.name, name_w), row_style),
                                Span::styled(fit(&second, inner_w), status_span_style),
                            ])));
                        }
                        if !any {
                            items.push(ListItem::new("  (none)").style(
                                Style::default().add_modifier(ratatui::style::Modifier::DIM),
                            ));
                        }
                    }
                    f.render_widget(
                        List::new(items).block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_type(BorderType::Rounded)
                                .title(list_title(c.projects.len(), order.len())),
                        ),
                        panels[0],
                    );
                }
                let sel_pi = order.get(selected).copied();
                let title = sel_pi
                    .and_then(|pi| c.projects.get(pi))
                    .map(|p| {
                        let scroll_note = sel_pi
                            .and_then(|pi| sessions.get(pi))
                            .and_then(|o| o.as_ref())
                            .map(|s| s.parser.screen().scrollback())
                            .filter(|&n| n > 0)
                            .map(|n| format!(" · ⇅{n}"))
                            .unwrap_or_default();
                        Line::from(vec![
                            Span::styled(
                                p.name.clone(),
                                Style::default().add_modifier(ratatui::style::Modifier::BOLD),
                            ),
                            Span::raw(" ["),
                            Span::styled(
                                p.kind.as_str(),
                                Style::default()
                                    .fg(kind_color(p.kind))
                                    .add_modifier(ratatui::style::Modifier::BOLD),
                            ),
                            Span::styled(
                                format!(" · {}", if focus { "INPUT" } else { "MANAGER" }),
                                if focus {
                                    Style::default()
                                        .fg(Color::Green)
                                        .add_modifier(ratatui::style::Modifier::BOLD)
                                } else {
                                    Style::default().fg(Color::DarkGray)
                                },
                            ),
                            Span::raw(format!("{scroll_note}]")),
                        ])
                    })
                    .unwrap_or_else(|| Line::from("No projects: squad add <path>"));
                let block = Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(if focus {
                        Style::default().fg(Color::Green)
                    } else {
                        Style::default()
                    })
                    .title(title);
                let inner = block.inner(panels[1]);
                f.render_widget(block, panels[1]);
                if let Some(s) = sel_pi.and_then(|pi| sessions.get(pi).and_then(|o| o.as_ref())) {
                    let screen = s.parser.screen();
                    let mut lines = Vec::new();
                    for row in 0..inner.height {
                        let mut spans = Vec::new();
                        for col in 0..inner.width {
                            if let Some(cell) = screen.cell(row, col) {
                                if cell.is_wide_continuation() {
                                    continue;
                                }
                                let mut style = Style::default()
                                    .fg(color(cell.fgcolor()))
                                    .bg(color(cell.bgcolor()));
                                if cell.bold() {
                                    style = style.add_modifier(ratatui::style::Modifier::BOLD);
                                }
                                if cell.dim() {
                                    style = style.add_modifier(ratatui::style::Modifier::DIM);
                                }
                                if cell.italic() {
                                    style = style.add_modifier(ratatui::style::Modifier::ITALIC);
                                }
                                if cell.underline() {
                                    style =
                                        style.add_modifier(ratatui::style::Modifier::UNDERLINED);
                                }
                                if cell.inverse() {
                                    style = style.add_modifier(ratatui::style::Modifier::REVERSED);
                                }
                                spans.push(Span::styled(
                                    if cell.has_contents() {
                                        cell.contents()
                                    } else {
                                        " "
                                    },
                                    style,
                                ));
                            }
                        }
                        lines.push(Line::from(spans));
                    }
                    f.render_widget(Paragraph::new(lines), inner);
                    if focus && !screen.hide_cursor() {
                        let (r, c) = screen.cursor_position();
                        if r < inner.height && c < inner.width {
                            f.set_cursor_position((inner.x + c, inner.y + r));
                        }
                    }
                } else {
                    let body = empty_state_lines(!c.projects.is_empty());
                    let pad = inner.height.saturating_sub(body.len() as u16) / 2;
                    let mut lines: Vec<Line> = (0..pad).map(|_| Line::from("")).collect();
                    lines.extend(body.iter().map(|l| Line::from(l.as_str())));
                    f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), inner);
                }
                let mode = mode_of(focus, sending, filtering, adding, show_help);
                let (chip_text, chip_style) = mode_chip(mode);
                let mut bar = vec![
                    Span::styled(
                        chip_text,
                        chip_style.add_modifier(ratatui::style::Modifier::BOLD),
                    ),
                    Span::raw(" "),
                ];
                if prefix {
                    bar.push(Span::raw(
                        "Ctrl+G prefix: g = send Ctrl+G, Esc = send Esc; any other key = manager",
                    ));
                } else if filtering {
                    bar.extend(input_line(
                        "Filter: ".to_string(),
                        &filter_buf,
                        format!(" ({} matches, Enter keeps, Esc clears)", order.len()),
                    ));
                } else if sending {
                    let name = sel_pi
                        .and_then(|pi| c.projects.get(pi))
                        .map(|p| p.name.as_str())
                        .unwrap_or("?");
                    bar.extend(input_line(
                        format!("Send to {name}: "),
                        &send_buf,
                        " (Enter sends, Esc cancels)".to_string(),
                    ));
                } else if adding {
                    bar.push(Span::raw(mode_hints(Mode::Add)));
                } else {
                    bar.push(Span::raw(message.clone()));
                }
                f.render_widget(Paragraph::new(Line::from(bar)), vertical[1]);
                if show_help {
                    let popup = centered_rect(60, 50, f.area());
                    f.render_widget(ratatui::widgets::Clear, popup);
                    let lines: Vec<Line> = help_lines()
                        .iter()
                        .map(|l| Line::from(l.to_string()))
                        .collect();
                    f.render_widget(
                        Paragraph::new(lines).block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_type(BorderType::Double)
                                .title(Line::from(Span::styled(
                                    "Help (?/Esc closes)",
                                    Style::default()
                                        .fg(Color::White)
                                        .add_modifier(ratatui::style::Modifier::BOLD),
                                ))),
                        ),
                        popup,
                    );
                }
                if let Some(pi) = confirm_stop {
                    let name = c.projects.get(pi).map(|p| p.name.as_str()).unwrap_or("?");
                    let popup = centered_rect(60, 30, f.area());
                    f.render_widget(ratatui::widgets::Clear, popup);
                    f.render_widget(
                        Paragraph::new(vec![
                            Line::from(confirm_stop_prompt(name)),
                            Line::from(""),
                            Line::from("y confirms, n or Esc cancels"),
                        ])
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_type(BorderType::Double)
                                .title(Line::from(Span::styled(
                                    "Stop session",
                                    Style::default()
                                        .fg(Color::Red)
                                        .add_modifier(ratatui::style::Modifier::BOLD),
                                ))),
                        ),
                        popup,
                    );
                }
                if confirm_quit {
                    let popup = centered_rect(60, 30, f.area());
                    f.render_widget(ratatui::widgets::Clear, popup);
                    f.render_widget(
                        Paragraph::new(vec![
                            Line::from(confirm_quit_prompt(live_session_count(&sessions))),
                            Line::from(""),
                            Line::from("y quits, n or Esc cancels"),
                        ])
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_type(BorderType::Double)
                                .title(Line::from(Span::styled(
                                    "Quit",
                                    Style::default()
                                        .fg(Color::Red)
                                        .add_modifier(ratatui::style::Modifier::BOLD),
                                ))),
                        ),
                        popup,
                    );
                }
                if adding {
                    let popup = centered_rect(70, 60, f.area());
                    f.render_widget(ratatui::widgets::Clear, popup);
                    let kind_hint = if add_kind == Kind::Agent {
                        "agent (Space toggles)"
                    } else {
                        "service (Space toggles)"
                    };
                    let cmd_text = if add_cmd.is_empty() {
                        "(default shell)".to_string()
                    } else {
                        add_cmd.as_str()
                    };
                    let fields = [
                        ("Path", add_path.as_str()),
                        ("Kind", kind_hint.to_string()),
                        ("Command", cmd_text),
                    ];
                    let mut lines = Vec::new();
                    for (i, (label, val)) in fields.iter().enumerate() {
                        let active = i == add_field;
                        let mark = if active { ">" } else { " " };
                        let label_style = if active {
                            Style::default().add_modifier(ratatui::style::Modifier::BOLD)
                        } else {
                            Style::default().fg(Color::DarkGray)
                        };
                        let mut row = vec![Span::styled(format!("{mark} {label}: "), label_style)];
                        let editing = active && i != 1;
                        if editing {
                            let input = if i == 0 { &add_path } else { &add_cmd };
                            let (left, cur, right) = input.render_parts();
                            let value_style =
                                Style::default().add_modifier(ratatui::style::Modifier::BOLD);
                            row.push(Span::styled(left, value_style));
                            row.push(Span::styled(
                                cur.to_string(),
                                value_style.bg(Color::White).fg(Color::Black),
                            ));
                            row.push(Span::styled(right, value_style));
                            // When the command field is empty the display shows a
                            // hint instead of the raw buffer; keep the hint visible
                            // after the cursor block.
                            if i == 2 && add_cmd.is_empty() {
                                row.push(Span::styled(
                                    "(default shell)".to_string(),
                                    Style::default().fg(Color::DarkGray),
                                ));
                            }
                        } else {
                            row.push(Span::styled(
                                val.clone(),
                                if active {
                                    Style::default().add_modifier(ratatui::style::Modifier::BOLD)
                                } else {
                                    Style::default()
                                },
                            ));
                        }
                        lines.push(Line::from(row));
                    }
                    if !add_err.is_empty() {
                        lines.push(Line::from(Span::styled(
                            format!("Error: {add_err}"),
                            Style::default().fg(Color::Red),
                        )));
                    }
                    f.render_widget(
                        Paragraph::new(lines).block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_type(BorderType::Double)
                                .title(Line::from(Span::styled(
                                    "Add project (Enter save, Esc cancel)",
                                    Style::default()
                                        .fg(Color::Blue)
                                        .add_modifier(ratatui::style::Modifier::BOLD),
                                ))),
                        ),
                        popup,
                    );
                }
            })?;
            dirty = false;
        }
        // Short poll so child echo appears within ~10 ms of a keypress.
        // A long poll here is what made typing feel laggy.
        if !event::poll(Duration::from_millis(10))? {
            continue;
        }
        match event::read()? {
            Event::Resize(_, _) => {
                let a = terminal.size()?;
                let side = if a.width >= 60 { 24 } else { 0 };
                for s in sessions.iter_mut().flatten() {
                    if let Err(e) = s.resize(
                        a.height.saturating_sub(3).max(1),
                        a.width.saturating_sub(side).saturating_sub(2).max(1),
                    ) {
                        message = e.to_string();
                    }
                }
                dirty = true;
            }
            Event::Mouse(m) => {
                if let Some(lines) = wheel_lines(m.kind) {
                    scroll_lines_selected(&mut sessions, &order, selected, lines);
                    dirty = true;
                }
            }
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                dirty = true;
                if focus {
                    if k.code == KeyCode::Char('g') && k.modifiers.contains(KeyModifiers::CONTROL) {
                        focus = false;
                        prefix = true;
                        continue;
                    }
                    if focus_exit_key(k.code, k.modifiers) {
                        focus = false;
                        continue;
                    }
                    let shift = k.modifiers.contains(KeyModifiers::SHIFT);
                    match k.code {
                        KeyCode::PageUp if shift => {
                            scroll_selected(&mut sessions, &order, selected, rows as usize, 1);
                            continue;
                        }
                        KeyCode::PageDown if shift => {
                            scroll_selected(&mut sessions, &order, selected, rows as usize, -1);
                            continue;
                        }
                        KeyCode::End if shift => {
                            scroll_live_selected(&mut sessions, &order, selected);
                            continue;
                        }
                        _ => {}
                    }
                    if let Some(&pi) = order.get(selected) {
                        if let Some(Some(s)) = sessions.get_mut(pi) {
                            s.scroll_live();
                            let bytes = key_bytes(k, s.parser.screen().application_cursor());
                            if let Err(e) = s.input(&bytes) {
                                message = e.to_string();
                                focus = false;
                            }
                        }
                    }
                    continue;
                }
                if sending {
                    match k.code {
                        KeyCode::Esc => {
                            sending = false;
                            send_buf.clear();
                        }
                        KeyCode::Enter => {
                            let line = send_buf.take();
                            sending = false;
                            if line.is_empty() {
                                message = "Empty. Nothing sent.".into();
                            } else if let Some(&pi) = order.get(selected) {
                                let alive = sessions
                                    .get(pi)
                                    .and_then(|o| o.as_ref())
                                    .map(|s| s.status == "Running")
                                    .unwrap_or(false);
                                if !alive {
                                    if let Some(e) =
                                        start_session(&mut sessions, &c.projects, pi, rows, cols)
                                    {
                                        message = e;
                                    }
                                }
                                if let Some(Some(s)) = sessions.get_mut(pi) {
                                    let mut bytes = line.into_bytes();
                                    bytes.push(b'\n');
                                    match s.input(&bytes) {
                                        Ok(()) => {
                                            message = format!("Sent to {}", c.projects[pi].name)
                                        }
                                        Err(e) => message = e.to_string(),
                                    }
                                }
                            }
                        }
                        KeyCode::Backspace => {
                            send_buf.backspace();
                        }
                        KeyCode::Delete => {
                            send_buf.delete();
                        }
                        KeyCode::Left => send_buf.move_left(),
                        KeyCode::Right => send_buf.move_right(),
                        KeyCode::Home => send_buf.move_home(),
                        KeyCode::End => send_buf.move_end(),
                        KeyCode::Char(ch)
                            if !k.modifiers.contains(KeyModifiers::CONTROL)
                                && !k.modifiers.contains(KeyModifiers::ALT) =>
                        {
                            send_buf.insert(ch)
                        }
                        _ => {}
                    }
                    continue;
                }
                if filtering {
                    match k.code {
                        KeyCode::Esc => {
                            filtering = false;
                            filter_buf.clear();
                            order = display_order(&c.projects);
                            selected = 0;
                        }
                        KeyCode::Enter => filtering = false,
                        KeyCode::Backspace => {
                            filter_buf.backspace();
                            order = apply_filter(&c.projects, &filter_buf.as_str());
                            selected = 0;
                        }
                        KeyCode::Delete => {
                            filter_buf.delete();
                            order = apply_filter(&c.projects, &filter_buf.as_str());
                            selected = 0;
                        }
                        KeyCode::Left => filter_buf.move_left(),
                        KeyCode::Right => filter_buf.move_right(),
                        KeyCode::Home => filter_buf.move_home(),
                        KeyCode::End => filter_buf.move_end(),
                        KeyCode::Char(ch)
                            if !k.modifiers.contains(KeyModifiers::CONTROL)
                                && !k.modifiers.contains(KeyModifiers::ALT) =>
                        {
                            filter_buf.insert(ch);
                            order = apply_filter(&c.projects, &filter_buf.as_str());
                            selected = 0;
                        }
                        _ => {}
                    }
                    continue;
                }
                if adding {
                    match k.code {
                        KeyCode::Esc => adding = false,
                        KeyCode::Enter => {
                            add_err.clear();
                            match load() {
                                Err(e) => add_err = format!("Config reload failed: {e:#}"),
                                Ok(disk) => {
                                    merge_projects(&mut c, &mut sessions, disk);
                                    order = display_order(&c.projects);
                                }
                            }
                            if add_err.is_empty() {
                                match build_project(
                                    &c.projects,
                                    &add_path.as_str(),
                                    add_kind,
                                    &add_cmd.as_str(),
                                ) {
                                    Err(e) => add_err = e,
                                    Ok(p) => {
                                        let name = p.name.clone();
                                        let kind_label = p.kind.as_str();
                                        c.projects.push(p);
                                        sessions.push(None);
                                        match save(&c) {
                                            Err(e) => {
                                                c.projects.pop();
                                                sessions.pop();
                                                order = display_order(&c.projects);
                                                selected = 0;
                                                add_err = format!("Save failed: {e:#}");
                                            }
                                            Ok(()) => {
                                                order = display_order(&c.projects);
                                                selected = order
                                                    .iter()
                                                    .position(|&x| x == c.projects.len() - 1)
                                                    .unwrap_or(0);
                                                adding = false;
                                                message =
                                                    format!("Registered {name} ({kind_label})");
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        KeyCode::Tab => add_field = (add_field + 1) % 3,
                        KeyCode::Up => add_field = (add_field + 2) % 3,
                        KeyCode::Down => add_field = (add_field + 1) % 3,
                        KeyCode::Backspace => {
                            match add_field {
                                0 => {
                                    add_path.backspace();
                                }
                                2 => {
                                    add_cmd.backspace();
                                }
                                _ => {}
                            }
                            add_err.clear();
                        }
                        KeyCode::Delete => {
                            match add_field {
                                0 => {
                                    add_path.delete();
                                }
                                2 => {
                                    add_cmd.delete();
                                }
                                _ => {}
                            }
                            add_err.clear();
                        }
                        KeyCode::Left => match add_field {
                            0 => add_path.move_left(),
                            2 => add_cmd.move_left(),
                            _ => {}
                        },
                        KeyCode::Right => match add_field {
                            0 => add_path.move_right(),
                            2 => add_cmd.move_right(),
                            _ => {}
                        },
                        KeyCode::Home => match add_field {
                            0 => add_path.move_home(),
                            2 => add_cmd.move_home(),
                            _ => {}
                        },
                        KeyCode::End => match add_field {
                            0 => add_path.move_end(),
                            2 => add_cmd.move_end(),
                            _ => {}
                        },
                        KeyCode::Char(' ') if add_field == 1 => {
                            add_kind = if add_kind == Kind::Agent {
                                Kind::Service
                            } else {
                                Kind::Agent
                            };
                        }
                        KeyCode::Char(ch)
                            if !k.modifiers.contains(KeyModifiers::CONTROL)
                                && !k.modifiers.contains(KeyModifiers::ALT) =>
                        {
                            match add_field {
                                0 => add_path.insert(ch),
                                2 => add_cmd.insert(ch),
                                _ => {}
                            }
                            add_err.clear();
                        }
                        _ => {}
                    }
                    continue;
                }
                if show_help {
                    match k.code {
                        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?') => show_help = false,
                        _ => {}
                    }
                    continue;
                }
                if prefix {
                    prefix = false;
                    if k.code == KeyCode::Char('g') {
                        if let Some(&pi) = order.get(selected) {
                            if let Some(Some(s)) = sessions.get_mut(pi) {
                                let _ = s.input(&[7]);
                                focus = true;
                            }
                        }
                        continue;
                    }
                    if k.code == KeyCode::Esc {
                        if let Some(&pi) = order.get(selected) {
                            if let Some(Some(s)) = sessions.get_mut(pi) {
                                let _ = s.input(&[27]);
                                focus = true;
                            }
                        }
                        continue;
                    }
                }
                if confirm_quit {
                    match confirm_key(k.code) {
                        Some(true) => break,
                        Some(false) => confirm_quit = false,
                        None => {}
                    }
                    continue;
                }
                if confirm_stop.is_some() {
                    match confirm_key(k.code) {
                        Some(true) => {
                            if let Some(pi) = confirm_stop.take() {
                                if let Some(slot) = sessions.get_mut(pi) {
                                    *slot = None;
                                }
                                message = "Session stopped. Enter restarts it.".into();
                            }
                        }
                        Some(false) => {
                            confirm_stop = None;
                        }
                        None => {}
                    }
                    continue;
                }
                match k.code {
                    KeyCode::Char('q') => {
                        if live_session_count(&sessions) > 0 {
                            confirm_quit = true;
                        } else {
                            break;
                        }
                    }
                    KeyCode::Down | KeyCode::Char('n') if !order.is_empty() => {
                        selected = (selected + 1) % order.len()
                    }
                    KeyCode::Up | KeyCode::Char('p') if !order.is_empty() => {
                        selected = (selected + order.len() - 1) % order.len()
                    }
                    KeyCode::Char('/') if !order.is_empty() => {
                        sending = true;
                        send_buf.clear();
                    }
                    KeyCode::Char('?') => show_help = true,
                    KeyCode::Char('a') => {
                        adding = true;
                        show_help = false;
                        add_field = 0;
                        add_path.clear();
                        add_kind = Kind::Agent;
                        add_cmd.clear();
                        add_err.clear();
                    }
                    KeyCode::Char(c) if c.is_ascii_digit() => {
                        if let Some(i) = jump_to(order.len(), c) {
                            selected = i;
                        }
                    }
                    KeyCode::Char('f') if !order.is_empty() => {
                        filtering = true;
                        filter_buf.clear();
                    }
                    KeyCode::PageUp if k.modifiers.contains(KeyModifiers::SHIFT) => {
                        scroll_selected(&mut sessions, &order, selected, rows as usize, 1);
                    }
                    KeyCode::PageDown if k.modifiers.contains(KeyModifiers::SHIFT) => {
                        scroll_selected(&mut sessions, &order, selected, rows as usize, -1);
                    }
                    KeyCode::End if k.modifiers.contains(KeyModifiers::SHIFT) => {
                        scroll_live_selected(&mut sessions, &order, selected);
                    }
                    KeyCode::PageUp if !order.is_empty() => {
                        scroll_selected(&mut sessions, &order, selected, rows as usize, 1);
                    }
                    KeyCode::PageDown if !order.is_empty() => {
                        scroll_selected(&mut sessions, &order, selected, rows as usize, -1);
                    }
                    KeyCode::Char('r') => {
                        if let Some(&pi) = order.get(selected) {
                            match restart_session(&mut sessions, &c.projects, pi, rows, cols) {
                                None => {
                                    message = format!("Restarted {}", c.projects[pi].name);
                                }
                                Some(e) => message = e,
                            }
                        }
                    }
                    KeyCode::Char('s') | KeyCode::Char('S') if !order.is_empty() => {
                        let mut started_n = 0;
                        let mut failed: Option<String> = None;
                        for &pi in order.iter() {
                            let running = sessions
                                .get(pi)
                                .and_then(|o| o.as_ref())
                                .map(|s| s.status == "Running")
                                .unwrap_or(false);
                            if running {
                                continue;
                            }
                            match start_session(&mut sessions, &c.projects, pi, rows, cols) {
                                None => started_n += 1,
                                Some(e) => failed = Some(e),
                            }
                        }
                        message = match failed {
                            Some(e) => format!("Started {started_n}; error: {e}"),
                            None if started_n == 0 => "All sessions already running.".into(),
                            None => format!("Started {started_n} session(s)."),
                        };
                    }
                    KeyCode::Char('x') => {
                        if let Some(&pi) = order.get(selected) {
                            if sessions.get(pi).and_then(|o| o.as_ref()).is_some() {
                                confirm_stop = Some(pi);
                            }
                        }
                    }
                    KeyCode::Enter => {
                        if order.is_empty() {
                            adding = true;
                            show_help = false;
                            add_field = 0;
                            add_path.clear();
                            add_kind = Kind::Agent;
                            add_cmd.clear();
                            add_err.clear();
                        } else if let Some(&pi) = order.get(selected) {
                            if let Some(e) =
                                start_session(&mut sessions, &c.projects, pi, rows, cols)
                            {
                                message = e;
                                continue;
                            }
                            focus = true;
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    drop(sessions);
    Ok(())
}
fn main() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    if matches!(
        args.first().map(String::as_str),
        Some("--help" | "-h" | "help")
    ) {
        println!("AgentSquad v{}\n\nsquad                         Open manager\nsquad add <path> [--kind agent|service] [-- cmd args] Register project (default: shell)\nsquad list                    List registered projects\nsquad remove <name>           Unregister (does not delete files)\nsquad start <name>            Open manager and start project\nsquad --version\n\nManager: n/p or arrows select, Enter starts/focuses, a adds, / sends a line, f filters, r restarts, x stops, 1-9 jumps, Shift+PgUp scrolls, q quits, ? help.\nTerminal: Ctrl+G returns to manager; then g sends Ctrl+G.\nQuitting stops managed child processes; no detached daemon.", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("--version") {
        println!("squad {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let mut c = load()?;
    match args.first().map(String::as_str) {
        None => run(c, None),
        Some("add") => {
            let path =
                fs::canonicalize(args.get(1).context(
                    "Usage: squad add <path> [--kind agent|service] [-- command args...]",
                )?)?;
            if !path.is_dir() {
                bail!("Project path must be a directory");
            }
            let name = path
                .file_name()
                .context("Cannot use filesystem root as project")?
                .to_string_lossy()
                .into_owned();
            if c.projects
                .iter()
                .any(|p| p.name.eq_ignore_ascii_case(&name))
            {
                bail!("Project name already registered: {name}");
            }
            let mut kind = Kind::default();
            let mut command: Option<Vec<String>> = None;
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--kind" => {
                        let v = args.get(i + 1).context(
                            "Usage: squad add <path> [--kind agent|service] [-- command args...]",
                        )?;
                        kind = v.parse()?;
                        i += 2;
                    }
                    "--" => {
                        if i + 1 >= args.len() {
                            bail!("Use -- before command: squad add <path> -- claude");
                        }
                        command = Some(args[i + 1..].to_vec());
                        break;
                    }
                    other => {
                        bail!("Unknown option: {other}. Use --kind agent|service or -- command")
                    }
                }
            }
            let command = command.unwrap_or_else(shell);
            c.projects.push(Project {
                name: name.clone(),
                path,
                command,
                kind,
            });
            save(&c)?;
            println!("Registered {name} ({})", kind.as_str());
            Ok(())
        }
        Some("list") => {
            // Same agents-first order as the manager sidebar.
            for &pi in display_order(&c.projects).iter() {
                let p = &c.projects[pi];
                println!(
                    "{}\t{}\t{}\t{}",
                    p.name,
                    p.kind.as_str(),
                    p.path.display(),
                    p.command.join(" ")
                );
            }
            Ok(())
        }
        Some("remove") => {
            let name = args.get(1).context("Usage: squad remove <name>")?;
            let old = c.projects.len();
            c.projects.retain(|p| !p.name.eq_ignore_ascii_case(name));
            if old == c.projects.len() {
                bail!("Unknown project: {name}");
            }
            save(&c)?;
            println!("Unregistered {name}; project files unchanged");
            Ok(())
        }
        Some("start") => {
            let name = args.get(1).context("Usage: squad start <name>")?;
            let i = c
                .projects
                .iter()
                .position(|p| p.name.eq_ignore_ascii_case(name))
                .with_context(|| format!("Project not registered: {name}"))?;
            run(c, Some(i))
        }
        Some(other) => bail!("Unknown command: {other}. Use squad --help"),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn config_roundtrip() {
        let c = Config {
            projects: vec![Project {
                name: "demo".into(),
                path: PathBuf::from("demo"),
                command: shell(),
                kind: Kind::Agent,
            }],
        };
        let decoded: Config = toml::from_str(&toml::to_string(&c).unwrap()).unwrap();
        assert_eq!(decoded.projects[0].name, "demo");
    }
    #[test]
    fn kind_defaults_to_agent() {
        let c: Config =
            toml::from_str("[[projects]]\nname = \"demo\"\npath = \"demo\"\ncommand = [\"sh\"]")
                .unwrap();
        assert_eq!(c.projects[0].kind, Kind::Agent);
    }
    #[test]
    fn kind_parses_service() {
        let c: Config = toml::from_str("[[projects]]\nname = \"api\"\npath = \"api\"\ncommand = [\"node\", \"server.js\"]\nkind = \"service\"").unwrap();
        assert_eq!(c.projects[0].kind, Kind::Service);
    }
    #[test]
    fn kind_from_str() {
        assert_eq!("Service".parse::<Kind>().unwrap(), Kind::Service);
        assert!("bot".parse::<Kind>().is_err());
    }
    #[test]
    fn order_groups_agents_first() {
        let mk = |kind| Project {
            name: "x".into(),
            path: PathBuf::from("x"),
            command: vec!["sh".into()],
            kind,
        };
        let ps = vec![mk(Kind::Service), mk(Kind::Agent), mk(Kind::Service)];
        assert_eq!(display_order(&ps), vec![1, 0, 2]);
    }
    #[test]
    fn status_colors() {
        assert_eq!(status_style("Running"), Style::default().fg(Color::Green));
        assert_eq!(
            status_style("Exited (0)"),
            Style::default().fg(Color::Yellow)
        );
        assert_eq!(status_style("Error: boom"), Style::default().fg(Color::Red));
        assert_eq!(
            status_style("Stopped"),
            Style::default().fg(Color::DarkGray)
        );
    }
    #[test]
    fn help_covers_keys() {
        let h = help_lines().join("\n");
        for k in [
            "Enter", "/", "n/p", "filter", "restart", "add", "scroll", "1-9", "x", "q", "?",
            "Ctrl+G",
        ] {
            assert!(h.contains(k), "help missing {k}");
        }
    }
    #[test]
    fn mode_chip_and_hints() {
        assert_eq!(mode_of(false, false, false, false, false), Mode::Manager);
        assert_eq!(mode_of(true, false, false, false, false), Mode::Focus);
        assert_eq!(mode_of(false, true, false, false, false), Mode::Send);
        assert_eq!(mode_of(false, false, true, false, false), Mode::Filter);
        assert_eq!(mode_of(false, false, false, true, false), Mode::Add);
        assert_eq!(mode_of(false, false, false, false, true), Mode::Help);
        assert_eq!(mode_of(true, true, true, true, true), Mode::Help);
        assert!(mode_hints(Mode::Manager).contains("Enter"));
        assert!(mode_hints(Mode::Send).contains("Esc"));
        let chips: Vec<&str> = [
            Mode::Manager,
            Mode::Focus,
            Mode::Send,
            Mode::Filter,
            Mode::Add,
            Mode::Help,
        ]
        .iter()
        .map(|m| mode_chip(*m).0.trim())
        .collect();
        assert_eq!(
            chips,
            vec!["MANAGER", "INPUT", "SEND", "FILTER", "ADD", "HELP"]
        );
    }
    #[test]
    fn empty_state_guides_first_run() {
        let fresh = empty_state_lines(false).join("\n");
        assert!(fresh.contains("No projects"));
        assert!(fresh.contains("squad add"));
        let idle = empty_state_lines(true).join("\n");
        assert!(idle.contains("Enter"));
        assert!(!idle.contains("squad add"));
    }
    #[test]
    fn spinner_cycles() {
        assert_eq!(
            (0..8).map(spinner_frame).collect::<Vec<char>>(),
            vec!['◐', '◑', '◒', '◓', '◐', '◑', '◒', '◓']
        );
        assert_eq!(status_glyph("Running", 0), '◐');
        assert_eq!(status_glyph("Running", 99), spinner_frame(99));
        assert_eq!(status_glyph("Stopped", 99), '●');
        assert_eq!(status_glyph("Exited (0)", 99), '●');
        assert_eq!(status_glyph("Error: boom", 99), '●');
    }
    #[test]
    fn scrollback_view() {
        let mut p = vt100::Parser::new(NonZero::new(4).unwrap(), NonZero::new(20).unwrap(), 10);
        for i in 0..7 {
            p.process(format!("line{i}\r\n").as_bytes());
        }
        assert!(p.screen().scrollback_len() > 0);
        p.screen_mut().set_scrollback(2);
        assert_eq!(p.screen().scrollback(), 2);
        let top: String = p.screen().contents();
        assert!(top.contains("line"));
        p.screen_mut().set_scrollback(0);
        assert_eq!(p.screen().scrollback(), 0);
    }
    #[cfg(windows)]
    fn ps_stdout(script: &str) -> String {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let out = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", script])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map(|o| o.stdout)
            .unwrap_or_default();
        String::from_utf8_lossy(&out).into_owned()
    }
    #[cfg(windows)]
    fn child_pids(parent: u32) -> Vec<u32> {
        ps_stdout(&format!(
            "Get-CimInstance Win32_Process -Filter 'ParentProcessId={parent}' | Select-Object -ExpandProperty ProcessId"
        ))
        .lines()
        .filter_map(|l| l.trim().parse::<u32>().ok())
        .collect()
    }
    #[cfg(windows)]
    fn pid_alive(pid: u32) -> bool {
        let out = ps_stdout(&format!("tasklist /FI \"PID eq {pid}\" /FO CSV /NH"));
        out.contains(&format!("\"{pid}\""))
    }
    #[test]
    #[cfg(windows)]
    fn stop_kills_process_tree() {
        let p = Project {
            name: "treekill".into(),
            path: env::current_dir().unwrap(),
            command: vec![
                "cmd.exe".into(),
                "/d".into(),
                "/c".into(),
                "timeout".into(),
                "/t".into(),
                "30".into(),
                "/nobreak".into(),
            ],
            kind: Kind::Agent,
        };
        let s = Session::spawn(&p, 24, 80).unwrap();
        let pid = s.child.process_id().expect("pty child pid");
        let mut grandkids = Vec::new();
        for _ in 0..50 {
            grandkids = child_pids(pid);
            if !grandkids.is_empty() {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        assert!(!grandkids.is_empty(), "grandchild never appeared");
        drop(s);
        for _ in 0..50 {
            thread::sleep(Duration::from_millis(100));
            if !pid_alive(pid) && grandkids.iter().all(|g| !pid_alive(*g)) {
                return;
            }
        }
        panic!(
            "process tree survived stop: parent alive={} kids={:?}",
            pid_alive(pid),
            grandkids
                .iter()
                .map(|g| (*g, pid_alive(*g)))
                .collect::<Vec<_>>()
        );
    }
    #[test]
    fn jump_keys() {
        assert_eq!(jump_to(5, '1'), Some(0));
        assert_eq!(jump_to(5, '5'), Some(4));
        assert_eq!(jump_to(5, '6'), None);
        assert_eq!(jump_to(0, '1'), None);
        assert_eq!(jump_to(12, '9'), Some(8));
        assert_eq!(jump_to(12, '0'), None);
        assert_eq!(jump_to(3, 'x'), None);
    }
    #[test]
    fn build_project_validates() {
        let dir = env::temp_dir();
        let dir_str = dir.to_str().unwrap();
        let p = build_project(&[], dir_str, Kind::Service, "node server.js").unwrap();
        assert_eq!(p.kind, Kind::Service);
        assert_eq!(p.command, vec!["node".to_string(), "server.js".to_string()]);
        let existing = vec![p];
        assert!(build_project(&existing, dir_str, Kind::Agent, "").is_err());
        assert!(build_project(&[], "", Kind::Agent, "").is_err());
        assert!(build_project(&[], "C:/definitely/not/here/xyz", Kind::Agent, "").is_err());
        let shell_p = build_project(&[], dir_str, Kind::Agent, "   ").unwrap();
        assert_eq!(shell_p.command, shell());
    }
    #[test]
    fn filter_matches_name_and_command() {
        let mk = |name: &str, cmd: &[&str], kind| Project {
            name: name.into(),
            path: PathBuf::from("x"),
            command: cmd.iter().map(|s| s.to_string()).collect(),
            kind,
        };
        let ps = vec![
            mk("api", &["node", "server.js"], Kind::Service),
            mk("web", &["npm", "run", "dev"], Kind::Agent),
            mk("agent-alpha", &["claude"], Kind::Agent),
        ];
        assert_eq!(apply_filter(&ps, ""), vec![1, 2, 0]);
        assert_eq!(apply_filter(&ps, "api"), vec![0]);
        assert_eq!(apply_filter(&ps, "node"), vec![0]);
        assert_eq!(apply_filter(&ps, "AGENT"), vec![2]);
        assert!(apply_filter(&ps, "zzz").is_empty());
    }
    #[test]
    fn keyboard() {
        assert_eq!(
            key_bytes(
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                false
            ),
            vec![3]
        );
        assert_eq!(
            key_bytes(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), true),
            b"\x1bOA"
        );
    }
    #[test]
    fn focus_exit_key_is_plain_esc() {
        use crossterm::event::KeyModifiers as M;
        assert!(focus_exit_key(KeyCode::Esc, M::NONE));
        assert!(!focus_exit_key(KeyCode::Esc, M::SHIFT));
        assert!(!focus_exit_key(KeyCode::Esc, M::CONTROL));
        assert!(!focus_exit_key(KeyCode::Enter, M::NONE));
        assert_eq!(key_bytes(KeyEvent::new(KeyCode::Esc, M::NONE), false), [27]);
    }
    #[test]
    fn key_bytes_encodes_modifiers() {
        use crossterm::event::KeyModifiers as M;
        let ev = |code, mods| KeyEvent::new(code, mods);
        assert_eq!(key_bytes(ev(KeyCode::Left, M::NONE), false), b"\x1b[D");
        assert_eq!(key_bytes(ev(KeyCode::Left, M::SHIFT), false), b"\x1b[1;2D");
        assert_eq!(key_bytes(ev(KeyCode::Up, M::CONTROL), true), b"\x1b[1;5A");
        assert_eq!(key_bytes(ev(KeyCode::Home, M::SHIFT), false), b"\x1b[1;2H");
        assert_eq!(
            key_bytes(ev(KeyCode::Delete, M::CONTROL), false),
            b"\x1b[3;5~"
        );
        // Alt keeps the ESC-prefix form on top of the base sequence.
        assert_eq!(key_bytes(ev(KeyCode::Up, M::ALT), false), b"\x1b\x1b[A");
    }
    #[test]
    fn terminal_parser() {
        let mut p = vt100::Parser::new(NonZero::new(10).unwrap(), NonZero::new(40).unwrap(), 50);
        p.process(b"hello\r\nworld");
        assert!(p.screen().contents().contains("world"));
    }
    #[test]
    fn pty_smoke() {
        let p = Project {
            name: "smoke".into(),
            path: env::current_dir().unwrap(),
            command: if cfg!(windows) {
                vec![
                    "cmd.exe".into(),
                    "/d".into(),
                    "/c".into(),
                    "echo AGENTSQUAD_OK".into(),
                ]
            } else {
                vec!["sh".into(), "-c".into(), "echo AGENTSQUAD_OK".into()]
            },
            kind: Kind::Agent,
        };
        let mut s = Session::spawn(&p, 24, 80).unwrap();
        for _ in 0..100 {
            s.update();
            if s.parser.screen().contents().contains("AGENTSQUAD_OK") {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        panic!(
            "PTY output not received; status={} screen={:?}",
            s.status,
            s.parser.screen().contents()
        );
    }
    #[test]
    fn line_input_editing() {
        let mut l = LineInput::default();
        for ch in "hello".chars() {
            l.insert(ch);
        }
        assert_eq!(l.as_str(), "hello");
        l.move_left();
        l.move_left();
        l.insert('X');
        assert_eq!(l.as_str(), "helXlo");
        l.backspace();
        assert_eq!(l.as_str(), "hello");
        l.delete();
        assert_eq!(l.as_str(), "helo");
        l.move_home();
        l.insert('S');
        assert_eq!(l.as_str(), "Shelo");
        l.move_end();
        l.insert('!');
        assert_eq!(l.as_str(), "Shelo!");
        l.clear();
        assert!(l.is_empty());
        assert_eq!(l.as_str(), "");
    }
    #[test]
    fn line_input_cursor_parts() {
        let mut l = LineInput::default();
        for ch in "ab".chars() {
            l.insert(ch);
        }
        // Cursor at end: cursor block shows a space.
        assert_eq!(l.render_parts(), ("ab".to_string(), ' ', String::new()));
        l.move_left();
        assert_eq!(l.render_parts(), ("a".to_string(), 'b', String::new()));
        l.move_home();
        assert_eq!(l.render_parts(), (String::new(), 'a', "b".to_string()));
    }
    #[test]
    fn stop_asks_to_confirm() {
        let yes = |c: KeyCode| confirm_key(c);
        assert_eq!(yes(KeyCode::Char('y')), Some(true));
        assert_eq!(yes(KeyCode::Char('Y')), Some(true));
        assert_eq!(yes(KeyCode::Enter), Some(true));
        assert_eq!(yes(KeyCode::Esc), Some(false));
        assert_eq!(yes(KeyCode::Char('n')), Some(false));
        assert_eq!(yes(KeyCode::Char('N')), Some(false));
        assert_eq!(yes(KeyCode::Char('q')), None);
        let prompt = confirm_stop_prompt("web");
        assert!(prompt.contains("web"), "prompt names the session");
    }
    #[test]
    fn pty_env_advertises_truecolor() {
        let env = pty_env();
        assert!(env.contains(&("TERM", "xterm-256color")));
        assert!(env.contains(&("COLORTERM", "truecolor")));
    }
    #[test]
    fn list_title_shows_filter_counts() {
        let base = format!("AgentSquad v{}", env!("CARGO_PKG_VERSION"));
        assert_eq!(list_title(5, 5), base);
        assert_eq!(list_title(0, 0), base);
        assert_eq!(list_title(5, 2), format!("{base} (2/5)"));
    }
    #[test]
    fn kind_colors_are_distinct() {
        assert_eq!(kind_color(Kind::Agent), Color::Cyan);
        assert_eq!(kind_color(Kind::Service), Color::Magenta);
        assert_ne!(kind_color(Kind::Agent), kind_color(Kind::Service));
    }
    #[test]
    fn group_counts_split_by_kind() {
        let mk = |name: &str, kind: Kind| Project {
            name: name.into(),
            path: PathBuf::from("/tmp"),
            command: vec!["sh".into()],
            kind,
        };
        let projects = vec![
            mk("a1", Kind::Agent),
            mk("a2", Kind::Agent),
            mk("s1", Kind::Service),
        ];
        let running = [true, false, true];
        assert_eq!(group_counts(&projects, &running, Kind::Agent), (1, 2));
        assert_eq!(group_counts(&projects, &running, Kind::Service), (1, 1));
        assert_eq!(group_counts(&projects, &[], Kind::Agent), (0, 2));
    }
    #[test]
    fn wheel_maps_to_scroll_lines() {
        use crossterm::event::MouseEventKind as M;
        assert_eq!(wheel_lines(M::ScrollUp), Some(3));
        assert_eq!(wheel_lines(M::ScrollDown), Some(-3));
        assert_eq!(
            wheel_lines(M::Down(crossterm::event::MouseButton::Left)),
            None
        );
        assert_eq!(wheel_lines(M::Moved), None);
    }
    #[test]
    fn quit_counts_discardable_sessions() {
        let sessions: Vec<Option<u8>> = vec![None, Some(1), None, Some(2)];
        assert_eq!(live_session_count(&sessions), 2);
        let empty: Vec<Option<u8>> = vec![None];
        assert_eq!(live_session_count(&empty), 0);
        let prompt = confirm_quit_prompt(2);
        assert!(prompt.contains('2'), "prompt names the count");
    }
    #[test]
    fn uptime_labels_scale() {
        assert_eq!(uptime_label(0), "0s");
        assert_eq!(uptime_label(7), "7s");
        assert_eq!(uptime_label(59), "59s");
        assert_eq!(uptime_label(60), "1m");
        assert_eq!(uptime_label(125), "2m");
        assert_eq!(uptime_label(3599), "59m");
        assert_eq!(uptime_label(3600), "1h");
        assert_eq!(uptime_label(9000), "2h");
    }
    #[test]
    fn start_session_starts_and_reports() {
        let dir = env::current_dir().unwrap();
        let shell: Vec<String> = if cfg!(windows) {
            vec![
                "cmd.exe".into(),
                "/d".into(),
                "/c".into(),
                "echo AGENTSQUAD_OK".into(),
            ]
        } else {
            vec!["sh".into(), "-c".into(), "echo AGENTSQUAD_OK".into()]
        };
        let projects = vec![
            Project {
                name: "ok".into(),
                path: dir.clone(),
                command: shell,
                kind: Kind::Agent,
            },
            Project {
                name: "bad".into(),
                path: dir,
                command: vec!["agentsquad-no-such-binary-xyz".into()],
                kind: Kind::Service,
            },
        ];
        let mut sessions: Vec<Option<Session>> = vec![None, None];
        // Stopped session starts.
        assert!(start_session(&mut sessions, &projects, 0, 24, 80).is_none());
        assert!(sessions[0].is_some());
        // Already running: no-op, no respawn, no error.
        assert!(start_session(&mut sessions, &projects, 0, 24, 80).is_none());
        assert!(sessions[0].is_some());
        // Bad binary surfaces an error and leaves no session behind.
        assert!(start_session(&mut sessions, &projects, 1, 24, 80).is_some());
        assert!(sessions[1].is_none());
        // Unknown index reports instead of panicking.
        assert!(start_session(&mut sessions, &projects, 7, 24, 80).is_some());
    }
    #[test]
    fn merge_appends_missing_projects() {
        let mk = |name: &str| Project {
            name: name.into(),
            path: PathBuf::from("/tmp"),
            command: vec!["sh".into()],
            kind: Kind::Agent,
        };
        let mut c = Config {
            projects: vec![mk("a")],
        };
        let mut sessions: Vec<Option<Session>> = vec![None];
        merge_projects(
            &mut c,
            &mut sessions,
            Config {
                projects: vec![mk("a"), mk("B")],
            },
        );
        assert_eq!(c.projects.len(), 2);
        assert_eq!(sessions.len(), 2);
        assert_eq!(c.projects[1].name, "B");
        // Idempotent: a second merge adds nothing.
        merge_projects(
            &mut c,
            &mut sessions,
            Config {
                projects: vec![mk("A"), mk("b")],
            },
        );
        assert_eq!(c.projects.len(), 2);
        assert_eq!(sessions.len(), 2);
    }
}
