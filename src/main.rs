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
    path::{Path, PathBuf},
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
fn load_from(p: &Path) -> Result<Config> {
    if !p.exists() {
        return Ok(Config::default());
    }
    let text = fs::read_to_string(p).with_context(|| format!("Cannot read {}", p.display()))?;
    toml::from_str(&text).with_context(|| format!("Cannot parse {}", p.display()))
}
fn load() -> Result<Config> {
    let p = config_path()?;
    load_from(&p)
}
fn save_to(c: &Config, p: &Path) -> Result<()> {
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    // Write-then-rename so a crash mid-write never leaves a torn config.
    let tmp = p.with_extension("toml.tmp");
    fs::write(&tmp, toml::to_string_pretty(c)?)?;
    fs::rename(&tmp, p)?;
    Ok(())
}
fn save(c: &Config) -> Result<()> {
    let p = config_path()?;
    save_to(c, &p)
}
/// Register `p` on disk, re-reading the file first so projects added or
/// removed by another process are not clobbered.
fn persist_add(p: &Project) -> Result<()> {
    let mut disk = load()?;
    if !disk
        .projects
        .iter()
        .any(|q| q.name.eq_ignore_ascii_case(&p.name))
    {
        disk.projects.push(p.clone());
    }
    save(&disk)
}
/// Unregister `name` on disk, re-reading the file first (see `persist_add`).
fn persist_remove(name: &str) -> Result<()> {
    let mut disk = load()?;
    disk.projects.retain(|q| !q.name.eq_ignore_ascii_case(name));
    save(&disk)
}
/// Drop the Windows verbatim prefix `fs::canonicalize` adds, so stored
/// paths read `C:\x` instead of `\\?\C:\x` (many tools choke on the latter).
fn plain_path(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => p,
    }
}
/// Split a command line on whitespace, keeping "double quoted" runs whole.
fn split_command(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut has = false;
    for ch in s.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                has = true;
            }
            c if c.is_whitespace() && !quoted => {
                if has {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => {
                cur.push(c);
                has = true;
            }
        }
    }
    if has {
        out.push(cur);
    }
    out
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
///
/// Services may leave the path empty: they run in squad's own working
/// directory and take their name from the served program instead of a folder.
fn build_project(
    existing: &[Project],
    path_raw: &str,
    kind: Kind,
    parts: Vec<String>,
) -> Result<Project, String> {
    let trimmed = path_raw.trim();
    if trimmed.is_empty() && kind != Kind::Service {
        return Err("Path is empty.".into());
    }
    // An omitted service path means "right here".
    let anchor = if trimmed.is_empty() { "." } else { trimmed };
    let path = plain_path(fs::canonicalize(anchor).map_err(|e| format!("Bad path: {e}"))?);
    if !path.is_dir() {
        return Err("Project path must be a directory.".into());
    }
    let dir_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if dir_name.is_empty() {
        return Err("Cannot use filesystem root as project.".into());
    }
    let name = if trimmed.is_empty() {
        program_name(&parts).unwrap_or(dir_name)
    } else {
        dir_name
    };
    if existing.iter().any(|p| p.name.eq_ignore_ascii_case(&name)) {
        return Err(format!("Project name already registered: {name}"));
    }
    let command = if parts.is_empty() { shell() } else { parts };
    Ok(Project {
        name,
        path,
        command,
        kind,
    })
}
/// Program word of a command line for naming path-less services:
/// `ollama.exe` serves as `ollama`, `node` stays `node`.
fn program_name(parts: &[String]) -> Option<String> {
    let first = parts.first()?;
    let base = std::path::Path::new(first)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| first.clone());
    let lower = base.to_ascii_lowercase();
    for ext in [".exe", ".cmd", ".bat", ".com"] {
        if let Some(stem) = lower.strip_suffix(ext) {
            let stem = base[..stem.len()].to_string();
            if !stem.is_empty() {
                return Some(stem);
            }
        }
    }
    Some(base)
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
        // Clean exits fade away; crashes glow red.
        if status.ends_with("(0)") {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(Color::Red)
        }
    } else if status.starts_with("Error") {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}
/// Status segment for the terminal pane title: "◐ Running 7s".
/// Reuses the sidebar glyph and color language so both panels read as one.
fn pane_status_text(status: &str, uptime_secs: Option<u64>, tick: u64) -> String {
    let mut text = format!("{} {status}", status_glyph(status, tick));
    if status == "Running" {
        if let Some(secs) = uptime_secs {
            text.push(' ');
            text.push_str(&uptime_label(secs));
        }
    }
    text
}
fn help_lines() -> Vec<&'static str> {
    vec![
        "Enter      start / focus session",
        "/          send a line to session",
        "a          add project",
        "f          filter projects",
        "Shift+PgUp/Dn  scroll history",
        "r          restart session",
        "s          start all stopped sessions",
        "x          stop session (asks to confirm)",
        "Del        remove project (asks to confirm)",
        "n/p, arrows  select project",
        "1-9        jump to project",
        "Esc        terminal back to manager",
        "Ctrl+G,Esc  send Esc to terminal",
        "q          quit (confirms when sessions exist)",
        "?/Esc      close this help",
    ]
}
/// Split a help row into its key column and description at the first
/// run of two or more spaces, so the key can be highlighted.
fn split_help_line(line: &str) -> (&str, &str) {
    if let Some(i) = line.find("  ") {
        (line[..i].trim_end(), line[i..].trim_start())
    } else {
        (line, "")
    }
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
/// Visible slice of a single-line input: shows the tail end once the
/// text outgrows `width` so the block cursor never scrolls off screen.
fn input_window(before: &str, at: char, after: &str, width: usize) -> (String, char, String) {
    let b: Vec<char> = before.chars().collect();
    let a: Vec<char> = after.chars().collect();
    let total = b.len() + 1 + a.len();
    if width == 0 || total <= width {
        return (before.to_string(), at, after.to_string());
    }
    let full: Vec<char> = b
        .iter()
        .chain(std::iter::once(&at))
        .chain(a.iter())
        .copied()
        .collect();
    let cursor = b.len();
    let start = (cursor + 1).saturating_sub(width);
    let end = (start + width).min(full.len());
    let rel = cursor - start;
    (
        full[start..rel + start].iter().collect(),
        full[rel + start],
        full[rel + start + 1..end].iter().collect(),
    )
}
fn input_line(prompt: String, input: &LineInput, hint: String, width: usize) -> Vec<Span<'static>> {
    let (before, at, after) = input.render_parts();
    let prompt_w = prompt.chars().count();
    let (before, at, after) = input_window(&before, at, &after, width.saturating_sub(prompt_w));
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
/// Severity of the one-line status bar message, so errors and exit
/// notices read differently from idle hints and confirmations.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Notice {
    Info,
    Warn,
    Error,
}
fn notice_style(kind: Notice) -> Style {
    match kind {
        Notice::Error => Style::default()
            .fg(Color::Red)
            .add_modifier(ratatui::style::Modifier::BOLD),
        Notice::Warn => Style::default().fg(Color::Yellow),
        Notice::Info => Style::default(),
    }
}
fn mode_chip(m: Mode) -> (&'static str, Style) {
    // Solid-background chips so the current mode reads at a glance.
    let (label, fg, bg) = match m {
        Mode::Manager => (" MANAGER ", Color::Black, Color::Cyan),
        Mode::Focus => (" INPUT ", Color::Black, Color::Green),
        Mode::Send => (" SEND ", Color::Black, Color::Yellow),
        Mode::Filter => (" FILTER ", Color::Black, Color::Magenta),
        Mode::Add => (" ADD ", Color::White, Color::Blue),
        Mode::Help => (" HELP ", Color::Black, Color::Gray),
    };
    (label, Style::default().fg(fg).bg(bg))
}
/// Status bar segments as (key, description) pairs. An empty key means
/// the text is a plain instruction, not a key press.
fn mode_hint_parts(m: Mode) -> &'static [(&'static str, &'static str)] {
    match m {
        Mode::Manager => &[
            ("Enter", "focus"),
            ("/", "send"),
            ("a", "add"),
            ("f", "filter"),
            ("r", "restart"),
            ("s", "start all"),
            ("n/p", "select"),
            ("x", "stop"),
            ("Del", "remove"),
            ("q", "quit"),
            ("?", "help"),
        ],
        Mode::Focus => &[("Esc", "manager"), ("", "type to input")],
        Mode::Send => &[("Enter", "sends"), ("Esc", "cancels")],
        Mode::Filter => &[
            ("", "type to narrow"),
            ("Enter", "keeps"),
            ("Esc", "clears"),
        ],
        Mode::Add => &[
            ("Tab", "next"),
            ("Space", "kind"),
            ("Enter", "save"),
            ("Esc", "cancel"),
        ],
        Mode::Help => &[("?/Esc", "closes")],
    }
}
fn mode_hints(m: Mode) -> String {
    mode_hint_parts(m)
        .iter()
        .map(|(k, d)| match (*k, *d) {
            ("", d) => d.to_string(),
            (k, "") => k.to_string(),
            (k, d) => format!("{k} {d}"),
        })
        .collect::<Vec<_>>()
        .join(" | ")
}
/// Hint pairs in discovery order: most essential first. Narrow bars show
/// a leading subset so `q` never disappears while trivia survives.
fn priority_hint_parts(m: Mode) -> &'static [(&'static str, &'static str)] {
    match m {
        Mode::Manager => &[
            ("Enter", "focus"),
            ("/", "send"),
            ("q", "quit"),
            ("x", "stop"),
            ("?", "help"),
            ("f", "filter"),
            ("Del", "remove"),
            ("s", "start all"),
            ("a", "add"),
            ("n/p", "select"),
            ("r", "restart"),
        ],
        Mode::Add => &[
            ("Enter", "save"),
            ("Esc", "cancel"),
            ("Tab", "next"),
            ("Space", "kind"),
        ],
        _ => &[],
    }
}
/// Rendered length of hint pairs as joined by `mode_hints`/`hint_spans`.
fn hint_text_len(parts: &[(&str, &str)]) -> usize {
    parts
        .iter()
        .map(|(k, d)| k.len() + d.len() + usize::from(!k.is_empty() && !d.is_empty()))
        .sum::<usize>()
        + 3 * parts.len().saturating_sub(1)
}
/// Full hints when they fit; otherwise the longest leading run of the
/// priority order that fits, so narrow bars degrade gracefully instead
/// of clipping mid-hint. Modes without a priority table are already
/// short enough to show in full.
fn fitted_hints(m: Mode, width: usize) -> Vec<Span<'static>> {
    let full = mode_hint_parts(m);
    if hint_text_len(full) <= width {
        return hint_spans(full);
    }
    let order = priority_hint_parts(m);
    if order.is_empty() {
        return hint_spans(full);
    }
    let mut take = 0usize;
    let mut len = 0usize;
    for (i, (k, d)) in order.iter().enumerate() {
        let part = k.len() + d.len() + usize::from(!k.is_empty() && !d.is_empty());
        let add = part + if i == 0 { 0 } else { 3 };
        if len + add > width {
            break;
        }
        len += add;
        take = i + 1;
    }
    hint_spans(&order[..take])
}
/// Styled status bar segments: bright keys, dimmed descriptions.
fn hint_spans(parts: &[(&str, &str)]) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    for (i, (key, desc)) in parts.iter().enumerate() {
        if i > 0 {
            out.push(Span::styled(
                " | ".to_string(),
                Style::default().fg(Color::DarkGray),
            ));
        }
        if !key.is_empty() {
            out.push(Span::styled(
                key.to_string(),
                Style::default().add_modifier(ratatui::style::Modifier::BOLD),
            ));
        }
        if !key.is_empty() && !desc.is_empty() {
            out.push(Span::raw(" ".to_string()));
        }
        if !desc.is_empty() {
            out.push(Span::styled(
                desc.to_string(),
                Style::default().add_modifier(ratatui::style::Modifier::DIM),
            ));
        }
    }
    out
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
/// Manager-mode stop target shared by `x` and `Delete`: the selected
/// project, but only while it actually holds a session.
fn stop_target<T>(order: &[usize], selected: usize, slots: &[Option<T>]) -> Option<usize> {
    order
        .get(selected)
        .copied()
        .filter(|&pi| slots.get(pi).and_then(|s| s.as_ref()).is_some())
}
fn confirm_quit_prompt(n: usize) -> String {
    format!("Quit and discard {}?", plural_sessions(n))
}
/// Count noun with a correct plural: "1 session", "3 sessions".
fn plural_sessions(n: usize) -> String {
    if n == 1 {
        "1 session".to_string()
    } else {
        format!("{n} sessions")
    }
}
/// Stop-confirm modal body: the prompt in alarming red, key hints in
/// the shared status-bar style.
fn confirm_stop_lines(name: &str) -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            confirm_stop_prompt(name),
            Style::default()
                .fg(Color::Red)
                .add_modifier(ratatui::style::Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(hint_spans(&[("y", "confirms"), ("n / Esc", "cancels")])),
    ]
}
/// Quit-confirm modal body: same key-hint styling as the stop modal.
fn confirm_quit_lines(n: usize) -> Vec<Line<'static>> {
    vec![
        Line::from(confirm_quit_prompt(n)),
        Line::from(""),
        Line::from(hint_spans(&[("y", "quits"), ("n / Esc", "cancels")])),
    ]
}
fn confirm_remove_prompt(name: &str) -> String {
    format!("Remove project '{name}'?")
}
/// Remove-confirm modal body: same key-hint styling as the stop modal.
fn confirm_remove_lines(name: &str) -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            confirm_remove_prompt(name),
            Style::default()
                .fg(Color::Red)
                .add_modifier(ratatui::style::Modifier::BOLD),
        )),
        Line::from("Running sessions are stopped; files are untouched."),
        Line::from(""),
        Line::from(hint_spans(&[("y", "removes"), ("n / Esc", "cancels")])),
    ]
}
/// Sessions that quitting would discard (running or finished output).
fn live_session_count<T>(sessions: &[Option<T>]) -> usize {
    sessions.iter().flatten().count()
}
/// Unregister a project and drop its session slot, stopping a live
/// session through `Session`'s own teardown. Returns the removed project
/// so callers can roll back when persisting fails.
fn remove_project<T>(
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
    announced_exit: bool,
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
            announced_exit: false,
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
/// `squad --help` text: sectioned so commands and keys scan at a glance.
fn cli_help() -> String {
    format!(
        "AgentSquad v{} — manage Agent and Service terminal sessions in one TUI.

Usage:
  squad                                              Open manager
  squad add [<path>] [--kind agent|service] [-- cmd]  Register project (path optional for services)
  squad list                                         List registered projects
  squad remove <name>                                Unregister (does not delete files)
  squad start <name>                                 Open manager and start project
  squad --version                                    Print version

Manager keys:
  n/p, arrows  select · Enter  start/focus · a  add · /  send a line
  f  filter · r  restart · s  start all · x  stop · Del  remove · 1-9  jump
  Shift+PgUp/Dn  scroll · q  quit · ?  help

Terminal keys:
  Esc  back to manager · Ctrl+G, g  send Ctrl+G · Ctrl+G, Esc  send Esc

Quitting stops managed child processes; no detached daemon.",
        env!("CARGO_PKG_VERSION")
    )
}
/// One-time notice when a session leaves Running. Returns the message
/// the first time only; `announced` flips so repeats stay silent.
/// Severity of a session-exit notice: errors alarm, crashes warn,
/// clean exits stay quiet like any other confirmation.
fn exit_notice_kind(status: &str) -> Notice {
    if status.starts_with("Error") {
        Notice::Error
    } else if needs_attention(status) {
        Notice::Warn
    } else {
        Notice::Info
    }
}
fn exit_notice(name: &str, was_running: bool, now: &str, announced: &mut bool) -> Option<String> {
    if was_running && now != "Running" && !*announced {
        *announced = true;
        Some(format!("Session '{name}' {now}; Enter restarts it"))
    } else {
        None
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
/// First-run hint when `squad list` has nothing to show. Stderr only,
/// so stdout stays empty and machine-readable.
fn empty_list_hint() -> &'static str {
    "No projects yet. Run: squad add <path> [-- command]"
}
/// Paint the kind column of `squad list` with the same hue as the TUI
/// sidebar. Plain text when `enabled` is false so piped output stays clean.
fn paint_kind(cell: &str, kind: Kind, enabled: bool) -> String {
    if !enabled {
        return cell.to_string();
    }
    use crossterm::style::Stylize;
    match kind {
        Kind::Agent => cell.cyan().to_string(),
        Kind::Service => cell.magenta().to_string(),
    }
}
/// `squad list` rows in manager (agents-first) order, with the name
/// and kind columns padded so they line up in a terminal.
fn list_table(projects: &[Project], color: bool) -> Vec<String> {
    let order = display_order(projects);
    let name_w = order
        .iter()
        .map(|&i| projects[i].name.chars().count())
        .max()
        .unwrap_or(0);
    let kind_w = order
        .iter()
        .map(|&i| projects[i].kind.as_str().chars().count())
        .max()
        .unwrap_or(0);
    order
        .iter()
        .map(|&i| {
            let p = &projects[i];
            let kind = paint_kind(&format!("{:<kind_w$}", p.kind.as_str()), p.kind, color);
            format!(
                "{:<name_w$}  {kind}  {}  {}",
                p.name,
                p.path.display(),
                p.command.join(" ")
            )
        })
        .collect()
}
/// Sidebar width in columns: hidden on narrow terminals, a quarter of
/// the width (clamped) otherwise so names and commands stay readable.
fn sidebar_width(area_w: u16) -> u16 {
    if area_w < 60 {
        0
    } else {
        (area_w / 4).clamp(24, 38)
    }
}
/// Right-hand status bar summary: live sessions over registered projects.
fn fleet_summary(running: usize, total: usize) -> String {
    format!("● {running}/{total}")
}
/// Width of the right-hand fleet summary so the layout reserves enough
/// room for the failure alert instead of clipping it.
fn summary_width(running: usize, total: usize, failed: usize) -> u16 {
    let mut w = fleet_summary(running, total).chars().count();
    if failed > 0 {
        w += format!(" ✖ {failed}").chars().count();
    }
    (w + 1) as u16
}
/// Sidebar border: bright cyan while navigating, dimmed once typing
/// moves into the terminal pane. Mirrors `pane_border_style` so exactly
/// one panel looks active at a time.
fn sidebar_border_style(focus: bool) -> Style {
    if focus {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(Color::Cyan)
    }
}
/// Terminal pane border: bright green while typing, dimmed in manager
/// mode so the focus state reads at a glance.
fn pane_border_style(focus: bool) -> Style {
    if focus {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}
/// Sessions that need attention: crashed exits and errors.
/// Clean exits and stopped slots stay quiet.
fn needs_attention(status: &str) -> bool {
    status.starts_with("Error") || (status.starts_with("Exited") && !status.ends_with("(0)"))
}
/// Keep the selected sidebar row inside the visible window: scroll down
/// when it runs past the bottom, scroll up when it runs past the top.
fn keep_visible(offset: usize, sel_row: usize, visible: usize) -> usize {
    if visible == 0 {
        return 0;
    }
    if sel_row < offset {
        sel_row
    } else if sel_row + 1 > offset + visible {
        sel_row + 1 - visible
    } else {
        offset
    }
}
/// Selected sidebar row: bright cyan on dark gray so it wins over every
/// other hue in the row.
fn selected_row_style() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .bg(Color::DarkGray)
        .add_modifier(ratatui::style::Modifier::BOLD)
}
/// Sidebar project name: selection highlight wins; otherwise the kind
/// hue, bright while running and dimmed while idle, so Agents and
/// Services read apart at a glance.
fn row_name_style(selected: bool, status: &str, kind: Kind) -> Style {
    if selected {
        selected_row_style()
    } else if status == "Running" {
        Style::default()
            .fg(kind_color(kind))
            .add_modifier(ratatui::style::Modifier::BOLD)
    } else {
        Style::default()
            .fg(kind_color(kind))
            .add_modifier(ratatui::style::Modifier::DIM)
    }
}
/// Split a sidebar row's inner width into (status, name, detail) budgets.
/// The list widget clips anything past the row width, so the status head
/// is reserved first; the name takes what it needs and the detail gets
/// the rest only when at least a few columns remain.
fn row_widths(
    inner_w: usize,
    prefix_len: usize,
    status_len: usize,
    name_len: usize,
) -> (usize, usize, usize) {
    let status_w = status_len.min(inner_w.saturating_sub(prefix_len + 1 + 4));
    let rest = inner_w.saturating_sub(prefix_len + status_w + 1);
    if rest.saturating_sub(name_len) >= 5 {
        (status_w, name_len, rest - name_len)
    } else {
        (status_w, rest, 0)
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
/// Sidebar group header: bright kind-colored name, dimmed live/total
/// count so the numbers read as metadata instead of shouting.
fn group_header(title: &str, kind: Kind, up: usize, total: usize) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            title.to_string(),
            Style::default()
                .fg(kind_color(kind))
                .add_modifier(ratatui::style::Modifier::BOLD),
        ),
        Span::styled(
            format!(" ({up}/{total})"),
            Style::default().add_modifier(ratatui::style::Modifier::DIM),
        ),
    ])
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
/// First command word for agent rows, so agents with the same status
/// stay distinguishable. Skipped when it is just the default shell.
fn agent_tag(p: &Project) -> Option<&str> {
    if p.command == shell() {
        return None;
    }
    p.command.first().map(String::as_str)
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
/// Every PID in the process tree rooted at `root`, parents before children.
///
/// Agent sessions often outlive their wrapper: `cmd /d /c start ...` exits
/// at once while the real agent keeps running as a grandchild. Toolhelp32
/// still reports the original parent PID after the parent exits, so walking
/// the snapshot finds those orphans where a live-child-only kill misses them.
/// Returns empty when the snapshot cannot be taken.
#[cfg(windows)]
fn descendant_pids(root: u32) -> Vec<u32> {
    use std::collections::HashMap;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, FALSE, INVALID_HANDLE_VALUE},
        System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        },
    };
    let mut ordered = Vec::new();
    // SAFETY: process snapshot of our own machine; the handle is closed below.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return ordered;
    }
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    // SAFETY: dwSize is initialized; the API fills the remaining fields.
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
    // SAFETY: `entry` stays a valid buffer for the whole walk.
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) };
    while more != FALSE {
        children
            .entry(entry.th32ParentProcessID)
            .or_default()
            .push(entry.th32ProcessID);
        // SAFETY: same valid buffer as above.
        more = unsafe { Process32NextW(snapshot, &mut entry) };
    }
    unsafe { CloseHandle(snapshot) };
    let mut stack = vec![root];
    while let Some(pid) = stack.pop() {
        ordered.push(pid);
        if let Some(kids) = children.remove(&pid) {
            stack.extend(kids);
        }
    }
    ordered
}

/// Best-effort kill of a whole process tree by PID, leaves first.
///
/// Terminates every enumerated descendant (and the root) directly instead of
/// asking a live wrapper to take its tree down, so already-orphaned
/// grandchildren die too. Missing or denied PIDs are skipped silently.
#[cfg(windows)]
fn kill_tree_by_pid(root: u32) {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, FALSE},
        System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE},
    };
    let mut pids = descendant_pids(root);
    pids.reverse();
    for pid in pids {
        // SAFETY: terminating processes inside our own session tree; a null
        // handle only means the PID is already gone or out of reach.
        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE, FALSE, pid);
            if handle.is_null() {
                continue;
            }
            TerminateProcess(handle, 1);
            CloseHandle(handle);
        }
    }
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
        // F1-F4 are SS3 letters; F5-F12 are `CSI n ~` with xterm's gapped numbers.
        KeyCode::F(n @ 1..=4) if !(shift || ctrl) => vec![27, b'O', b'P' + (n - 1)],
        KeyCode::F(n @ 1..=4) => format!(
            "\x1b[1;{}{}",
            1 + shift as u8 + 4 * ctrl as u8,
            (b'P' + (n - 1)) as char
        )
        .into_bytes(),
        KeyCode::F(n @ 5..=12) => {
            let code = [15, 17, 18, 19, 20, 21, 23, 24][(n - 5) as usize];
            tilde_bytes(code, shift, ctrl)
        }
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
    let mut list_offset = 0usize;
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
    let mut confirm_remove: Option<usize> = None;
    let mut confirm_quit = false;
    let mut message = (mode_hints(Mode::Manager).to_string(), Notice::Info);
    let mut dirty = true;
    let mut first = initial.is_some();
    let mut tick: u64 = 0;
    loop {
        let area = terminal.size()?;
        let sidebar = sidebar_width(area.width);
        let rows = area.height.saturating_sub(3).max(1);
        let cols = area.width.saturating_sub(sidebar).saturating_sub(2).max(1);
        tick = tick.wrapping_add(1);
        // The periodic redraw only animates spinners and uptimes, so skip
        // it while nothing is running.
        if tick.is_multiple_of(3) && sessions.iter().flatten().any(|s| s.status == "Running") {
            dirty = true;
        }
        if first {
            first = false;
            if let Some(&pi) = order.get(selected) {
                match start_session(&mut sessions, &c.projects, pi, rows, cols) {
                    None => focus = true,
                    Some(e) => message = (e, Notice::Error),
                }
            }
        }
        for (pi, slot) in sessions.iter_mut().enumerate() {
            if let Some(s) = slot {
                let was_running = s.status == "Running";
                dirty |= s.update();
                if let Some(note) = exit_notice(
                    &c.projects[pi].name,
                    was_running,
                    &s.status,
                    &mut s.announced_exit,
                ) {
                    message = (note, exit_notice_kind(&s.status));
                }
            }
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
                    let mut sel_row: Option<usize> = None;
                    let mut n = 0;
                    let running: Vec<bool> = sessions
                        .iter()
                        .map(|o| o.as_ref().map(|s| s.status == "Running").unwrap_or(false))
                        .collect();
                    for (title, k) in [("Agents", Kind::Agent), ("Services", Kind::Service)] {
                        let (up, total) = group_counts(&c.projects, &running, k);
                        items.push(ListItem::new(group_header(title, k, up, total)));
                        let mut any = false;
                        for &pi in order.iter().filter(|&&i| c.projects[i].kind == k) {
                            any = true;
                            n += 1;
                            let p = &c.projects[pi];
                            let status = sessions[pi]
                                .as_ref()
                                .map(|s| s.status.as_str())
                                .unwrap_or("Stopped");
                            let mut detail = String::new();
                            if status == "Running" {
                                if let Some(s) = sessions[pi].as_ref() {
                                    if let Some(started) = s.started {
                                        detail.push(' ');
                                        detail.push_str(&uptime_label(started.elapsed().as_secs()));
                                    }
                                }
                            }
                            if k == Kind::Service {
                                detail.push(' ');
                                detail.push_str(&p.command.join(" "));
                            } else if let Some(tag) = agent_tag(p) {
                                detail.push(' ');
                                detail.push_str(tag);
                            }
                            let mark = if Some(pi) == sel_pi { ">" } else { " " };
                            let row_style = if Some(pi) == sel_pi {
                                selected_row_style()
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
                            let detail_style = if Some(pi) == sel_pi {
                                row_style
                            } else {
                                Style::default().fg(Color::DarkGray)
                            };
                            let prefix = format!("{mark}{n} ");
                            let status_full = format!("{} {status}", status_glyph(status, tick));
                            let (status_w, name_w, detail_w) = row_widths(
                                inner_w,
                                prefix.chars().count(),
                                status_full.chars().count(),
                                p.name.chars().count(),
                            );
                            if Some(pi) == sel_pi {
                                sel_row = Some(items.len());
                            }
                            items.push(ListItem::new(Line::from(vec![
                                Span::styled(prefix, row_style),
                                Span::styled(fit(&status_full, status_w), status_span_style),
                                Span::raw(" "),
                                Span::styled(
                                    fit(&p.name, name_w),
                                    row_name_style(Some(pi) == sel_pi, status, p.kind),
                                ),
                                Span::styled(fit(&detail, detail_w), detail_style),
                            ])));
                        }
                        if !any {
                            items.push(ListItem::new("  (none)").style(
                                Style::default().add_modifier(ratatui::style::Modifier::DIM),
                            ));
                        }
                    }
                    let visible_rows = (panels[0].height as usize).saturating_sub(2).max(1);
                    if let Some(sr) = sel_row {
                        list_offset = keep_visible(list_offset, sr, visible_rows);
                    }
                    list_offset = list_offset.min(items.len().saturating_sub(1));
                    let mut window = items.split_off(list_offset);
                    window.truncate(visible_rows);
                    f.render_widget(
                        List::new(window).block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_type(BorderType::Rounded)
                                .border_style(sidebar_border_style(focus))
                                .title(list_title(c.projects.len(), order.len())),
                        ),
                        panels[0],
                    );
                }
                let sel_pi = order.get(selected).copied();
                let (sel_status, sel_uptime): (&str, Option<u64>) = sel_pi
                    .and_then(|pi| sessions.get(pi).and_then(|o| o.as_ref()))
                    .map(|s| (s.status.as_str(), s.started.map(|t| t.elapsed().as_secs())))
                    .unwrap_or(("Stopped", None));
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
                            Span::raw(" · "),
                            Span::styled(
                                pane_status_text(sel_status, sel_uptime, tick),
                                status_style(sel_status)
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
                    .border_style(pane_border_style(focus))
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
                let running_n = sessions
                    .iter()
                    .flatten()
                    .filter(|s| s.status == "Running")
                    .count();
                let failed_n = sessions
                    .iter()
                    .flatten()
                    .filter(|s| needs_attention(&s.status))
                    .count();
                let status_row = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([
                        Constraint::Min(1),
                        Constraint::Length(summary_width(running_n, c.projects.len(), failed_n)),
                    ])
                    .split(vertical[1]);
                let input_w =
                    (status_row[0].width as usize).saturating_sub(chip_text.chars().count() + 1);
                if prefix {
                    bar.extend(hint_spans(&[
                        ("g", "send Ctrl+G"),
                        ("Esc", "send Esc"),
                        ("", "any other key = manager"),
                    ]));
                } else if focus {
                    bar.extend(fitted_hints(Mode::Focus, input_w));
                } else if filtering {
                    bar.extend(input_line(
                        "Filter: ".to_string(),
                        &filter_buf,
                        format!(" ({} matches, Enter keeps, Esc clears)", order.len()),
                        input_w,
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
                        input_w,
                    ));
                } else if adding {
                    bar.extend(fitted_hints(Mode::Add, input_w));
                } else if show_help && message.0 == mode_hints(Mode::Manager) {
                    bar.extend(fitted_hints(Mode::Help, input_w));
                } else if message.0 == mode_hints(Mode::Manager) {
                    // Idle hints recede so action feedback stands out.
                    bar.extend(fitted_hints(Mode::Manager, input_w));
                } else {
                    bar.push(Span::styled(message.0.clone(), notice_style(message.1)));
                }
                f.render_widget(Paragraph::new(Line::from(bar)), status_row[0]);
                let mut right = vec![Span::styled(
                    fleet_summary(running_n, c.projects.len()),
                    if running_n > 0 {
                        Style::default().fg(Color::Green)
                    } else {
                        Style::default().fg(Color::DarkGray)
                    },
                )];
                if failed_n > 0 {
                    right.push(Span::styled(
                        format!(" ✖ {failed_n}"),
                        Style::default()
                            .fg(Color::Red)
                            .add_modifier(ratatui::style::Modifier::BOLD),
                    ));
                }
                f.render_widget(
                    Paragraph::new(Line::from(right)).alignment(Alignment::Right),
                    status_row[1],
                );
                if show_help {
                    let popup = centered_rect(60, 50, f.area());
                    f.render_widget(ratatui::widgets::Clear, popup);
                    let lines: Vec<Line> = help_lines()
                        .iter()
                        .map(|l| {
                            let (key, desc) = split_help_line(l);
                            Line::from(vec![
                                Span::styled(
                                    key.to_string(),
                                    Style::default()
                                        .fg(Color::Cyan)
                                        .add_modifier(ratatui::style::Modifier::BOLD),
                                ),
                                Span::raw(format!("  {desc}")),
                            ])
                        })
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
                        Paragraph::new(confirm_stop_lines(name)).block(
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
                if let Some(pi) = confirm_remove {
                    let name = c.projects.get(pi).map(|p| p.name.as_str()).unwrap_or("?");
                    let popup = centered_rect(60, 30, f.area());
                    f.render_widget(ratatui::widgets::Clear, popup);
                    f.render_widget(
                        Paragraph::new(confirm_remove_lines(name)).block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_type(BorderType::Double)
                                .title(Line::from(Span::styled(
                                    "Remove project",
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
                        Paragraph::new(confirm_quit_lines(live_session_count(&sessions))).block(
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
                    let popup = centered_rect(70, 40, f.area());
                    let modal_w = (popup.width as usize).saturating_sub(2);
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
                    let path_label = if add_kind == Kind::Service {
                        "Path (optional)"
                    } else {
                        "Path"
                    };
                    let fields = [
                        (path_label, add_path.as_str()),
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
                            let label_w = format!("{mark} {label}: ").chars().count();
                            let (left, cur, right) =
                                input_window(&left, cur, &right, modal_w.saturating_sub(label_w));
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
                let side = sidebar_width(a.width);
                for s in sessions.iter_mut().flatten() {
                    if let Err(e) = s.resize(
                        a.height.saturating_sub(3).max(1),
                        a.width.saturating_sub(side).saturating_sub(2).max(1),
                    ) {
                        message = (e.to_string(), Notice::Error);
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
                                message = (e.to_string(), Notice::Error);
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
                                message = ("Empty. Nothing sent.".to_string(), Notice::Info);
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
                                        message = (e, Notice::Error);
                                    }
                                }
                                if let Some(Some(s)) = sessions.get_mut(pi) {
                                    let mut bytes = line.into_bytes();
                                    bytes.push(b'\n');
                                    match s.input(&bytes) {
                                        Ok(()) => {
                                            message = (
                                                format!("Sent to {}", c.projects[pi].name),
                                                Notice::Info,
                                            )
                                        }
                                        Err(e) => message = (e.to_string(), Notice::Error),
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
                            // A new project must be visible, so drop any filter.
                            filter_buf.clear();
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
                                    split_command(&add_cmd.as_str()),
                                ) {
                                    Err(e) => add_err = e,
                                    Ok(p) => {
                                        let name = p.name.clone();
                                        let kind_label = p.kind.as_str();
                                        c.projects.push(p);
                                        sessions.push(None);
                                        match persist_add(&c.projects[c.projects.len() - 1]) {
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
                                                message = (
                                                    format!("Registered {name} ({kind_label})"),
                                                    Notice::Info,
                                                );
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
                    // Any other key only cancels the prefix; it must not
                    // fire as a manager command (Ctrl+G, q would quit).
                    continue;
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
                                message = (
                                    "Session stopped. Enter restarts it.".to_string(),
                                    Notice::Warn,
                                );
                            }
                        }
                        Some(false) => {
                            confirm_stop = None;
                        }
                        None => {}
                    }
                    continue;
                }
                if confirm_remove.is_some() {
                    match confirm_key(k.code) {
                        Some(true) => {
                            if let Some(pi) = confirm_remove.take() {
                                if let Some(removed) =
                                    remove_project(&mut c.projects, &mut sessions, pi)
                                {
                                    match persist_remove(&removed.name) {
                                        Ok(()) => {
                                            // Keep an active filter across the removal.
                                            order = apply_filter(&c.projects, &filter_buf.as_str());
                                            selected = selected.min(order.len().saturating_sub(1));
                                            message = (
                                                format!("Removed {}.", removed.name),
                                                Notice::Warn,
                                            );
                                        }
                                        Err(e) => {
                                            // Roll back so a failed save never loses the project.
                                            let at = pi.min(c.projects.len());
                                            c.projects.insert(at, removed);
                                            if at <= sessions.len() {
                                                sessions.insert(at, None);
                                            }
                                            message =
                                                (format!("Remove failed: {e:#}"), Notice::Error);
                                        }
                                    }
                                }
                            }
                        }
                        Some(false) => {
                            confirm_remove = None;
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
                                    message = (
                                        format!("Restarted {}", c.projects[pi].name),
                                        Notice::Info,
                                    );
                                }
                                Some(e) => message = (e, Notice::Error),
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
                            Some(e) => (format!("Started {started_n}; error: {e}"), Notice::Error),
                            None if started_n == 0 => {
                                ("All sessions already running.".to_string(), Notice::Info)
                            }
                            None => (
                                format!("Started {}.", plural_sessions(started_n)),
                                Notice::Info,
                            ),
                        };
                    }
                    KeyCode::Char('x') => {
                        confirm_stop = stop_target(&order, selected, &sessions);
                    }
                    KeyCode::Delete => {
                        confirm_remove = order.get(selected).copied();
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
                                message = (e, Notice::Error);
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
        println!("{}", cli_help());
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
            // The path may be omitted for services: `squad add --kind service -- cmd`.
            const USAGE: &str =
                "Usage: squad add [<path>] [--kind agent|service] [-- command args...] (path optional for services)";
            let tail: &[String] = args.get(1..).unwrap_or(&[]);
            let (path_raw, tail) = match tail.first().map(String::as_str) {
                Some(flag) if flag.starts_with("--") => ("", tail),
                Some(_) => (tail[0].as_str(), &tail[1..]),
                None => ("", tail),
            };
            let mut kind = Kind::default();
            let mut command: Option<Vec<String>> = None;
            let mut i = 0;
            while i < tail.len() {
                match tail[i].as_str() {
                    "--kind" => {
                        let v = tail.get(i + 1).context(USAGE)?;
                        kind = v.parse()?;
                        i += 2;
                    }
                    "--" => {
                        if i + 1 >= tail.len() {
                            bail!("Use -- before command: squad add [<path>] -- claude");
                        }
                        command = Some(tail[i + 1..].to_vec());
                        break;
                    }
                    other => {
                        bail!("Unknown option: {other}. Use --kind agent|service or -- command")
                    }
                }
            }
            let p = build_project(&c.projects, path_raw, kind, command.unwrap_or_else(shell))
                .map_err(anyhow::Error::msg)?;
            let name = p.name.clone();
            c.projects.push(p);
            save(&c)?;
            println!("Registered {name} ({})", kind.as_str());
            Ok(())
        }
        Some("list") => {
            if c.projects.is_empty() {
                eprintln!("{}", empty_list_hint());
                return Ok(());
            }
            // Colors only on a real terminal; pipes and NO_COLOR stay plain.
            let color = io::stdout().is_terminal() && env::var("NO_COLOR").is_err();
            for line in list_table(&c.projects, color) {
                println!("{line}");
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
            Style::default().fg(Color::DarkGray)
        );
        assert_eq!(status_style("Exited (1)"), Style::default().fg(Color::Red));
        assert_eq!(
            status_style("Exited (137)"),
            Style::default().fg(Color::Red)
        );
        assert_eq!(status_style("Error: boom"), Style::default().fg(Color::Red));
        assert_eq!(
            status_style("Stopped"),
            Style::default().fg(Color::DarkGray)
        );
    }
    #[test]
    fn cli_help_covers_commands() {
        let h = cli_help();
        assert!(h.contains(env!("CARGO_PKG_VERSION")));
        for c in [
            "squad add",
            "squad list",
            "squad remove",
            "squad start",
            "--version",
            "Manager keys",
            "Terminal keys",
            "x  stop",
            "Del  remove",
            "Esc  back to manager",
        ] {
            assert!(h.contains(c), "help mentions {c}");
        }
    }
    #[test]
    fn help_covers_keys() {
        let h = help_lines().join("\n");
        for k in [
            "Enter", "/", "n/p", "filter", "restart", "add", "scroll", "1-9", "x", "Del", "q", "?",
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
    fn mode_chips_have_solid_backgrounds() {
        for m in [
            Mode::Manager,
            Mode::Focus,
            Mode::Send,
            Mode::Filter,
            Mode::Add,
            Mode::Help,
        ] {
            let style = mode_chip(m).1;
            assert!(style.bg.is_some(), "{m:?} chip needs a background");
            assert!(style.fg.is_some(), "{m:?} chip needs a foreground");
        }
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
    #[cfg(windows)]
    fn stop_kills_orphaned_grandchildren() {
        // A `start`ed grandchild outlives its wrapper: the direct child is
        // already dead when the session drops, yet the orphan must still die.
        let p = Project {
            name: "orphankill".into(),
            path: env::current_dir().unwrap(),
            command: vec![
                "cmd.exe".into(),
                "/d".into(),
                "/c".into(),
                "start".into(),
                "/min".into(),
                "ping".into(),
                // Long-lived on purpose: the stop poll below stretches over
                // tens of seconds (one powershell probe per PID per round),
                // so a short ping would die naturally and fake a pass.
                "-n".into(),
                "300".into(),
                "127.0.0.1".into(),
            ],
            kind: Kind::Agent,
        };
        let mut s = Session::spawn(&p, 24, 80).unwrap();
        let pid = s.child.process_id().expect("pty child pid");
        let mut orphans: Vec<u32> = Vec::new();
        for _ in 0..100 {
            let wrapper_dead = s.child.try_wait().ok().flatten().is_some();
            orphans = descendant_pids(pid)
                .into_iter()
                .filter(|p| *p != pid)
                .collect();
            if wrapper_dead && !orphans.is_empty() {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        assert!(!orphans.is_empty(), "orphan never appeared");
        drop(s);
        for _ in 0..50 {
            thread::sleep(Duration::from_millis(100));
            if orphans.iter().all(|g| !pid_alive(*g)) {
                return;
            }
        }
        panic!(
            "orphaned grandchild survived stop: {:?}",
            orphans
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
        let parts = |s: &str| -> Vec<String> { s.split_whitespace().map(str::to_string).collect() };
        let dir = env::temp_dir();
        let dir_str = dir.to_str().unwrap();
        let p = build_project(&[], dir_str, Kind::Service, parts("node server.js")).unwrap();
        assert_eq!(p.kind, Kind::Service);
        assert_eq!(p.command, vec!["node".to_string(), "server.js".to_string()]);
        let existing = vec![p];
        assert!(build_project(&existing, dir_str, Kind::Agent, parts("")).is_err());
        assert!(build_project(&[], "", Kind::Agent, parts("")).is_err());
        assert!(build_project(&[], "C:/definitely/not/here/xyz", Kind::Agent, parts("")).is_err());
        let shell_p = build_project(&[], dir_str, Kind::Agent, parts("   ")).unwrap();
        assert_eq!(shell_p.command, shell());
    }
    #[test]
    fn service_without_path_uses_current_dir() {
        // Services may skip the path: they run where squad runs, and the
        // project takes its name from the program being served.
        let parts = |s: &str| -> Vec<String> { s.split_whitespace().map(str::to_string).collect() };
        let p = build_project(&[], "", Kind::Service, parts("node server.js")).unwrap();
        assert_eq!(p.kind, Kind::Service);
        assert_eq!(p.name, "node");
        assert_eq!(p.command, vec!["node".to_string(), "server.js".to_string()]);
        assert_eq!(
            p.path,
            plain_path(fs::canonicalize(env::current_dir().unwrap()).unwrap())
        );
        assert!(!p.path.to_string_lossy().starts_with(r"\\?\"));
        // An .exe program names the project without its extension.
        let q = build_project(&[], "", Kind::Service, parts("ollama.exe serve")).unwrap();
        assert_eq!(q.name, "ollama");
        // Agents still require a path.
        assert!(build_project(&[], "", Kind::Agent, parts("claude")).is_err());
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
    fn plain_path_strips_verbatim_prefix() {
        assert_eq!(
            plain_path(PathBuf::from(r"\\?\C:\work\app")),
            PathBuf::from(r"C:\work\app")
        );
        // UNC shares and ordinary paths are left alone.
        let unc = PathBuf::from(r"\\?\UNC\srv\share");
        assert_eq!(plain_path(unc.clone()), unc);
        assert_eq!(plain_path(PathBuf::from("rel")), PathBuf::from("rel"));
    }
    #[test]
    fn split_command_honors_quotes() {
        assert_eq!(split_command("node  server.js"), ["node", "server.js"]);
        assert_eq!(
            split_command(r#""C:\Program Files\x.exe" --flag "a b""#),
            [r"C:\Program Files\x.exe", "--flag", "a b"]
        );
        assert_eq!(split_command(r#"echo """#), ["echo", ""]);
        assert!(split_command("   ").is_empty());
    }
    #[test]
    fn function_keys_encode_xterm() {
        let f = |n, m| key_bytes(KeyEvent::new(KeyCode::F(n), m), false);
        let none = KeyModifiers::NONE;
        assert_eq!(f(1, none), b"\x1bOP");
        assert_eq!(f(4, none), b"\x1bOS");
        assert_eq!(f(5, none), b"\x1b[15~");
        assert_eq!(f(10, none), b"\x1b[21~");
        assert_eq!(f(12, none), b"\x1b[24~");
        assert_eq!(f(1, KeyModifiers::SHIFT), b"\x1b[1;2P");
        assert_eq!(f(5, KeyModifiers::CONTROL), b"\x1b[15;5~");
    }
    #[test]
    fn persist_helpers_keep_other_writers_projects() {
        let file = env::temp_dir().join(format!("agentsquad-persist-{}.toml", std::process::id()));
        let mk = |name: &str| Project {
            name: name.into(),
            path: PathBuf::from("x"),
            command: vec!["sh".into()],
            kind: Kind::Agent,
        };
        // Single-threaded section: AGENTSQUAD_CONFIG is process-global, and
        // no other test reads it.
        env::set_var("AGENTSQUAD_CONFIG", &file);
        save_to(
            &Config {
                projects: vec![mk("a"), mk("b")],
            },
            &file,
        )
        .unwrap();
        // Another process registers "c" after we loaded; our remove of "a"
        // and add of "d" must keep it.
        let mut other = load_from(&file).unwrap();
        other.projects.push(mk("c"));
        save_to(&other, &file).unwrap();
        persist_remove("A").unwrap();
        persist_add(&mk("d")).unwrap();
        persist_add(&mk("D")).unwrap();
        let names: Vec<String> = load_from(&file)
            .unwrap()
            .projects
            .into_iter()
            .map(|p| p.name)
            .collect();
        env::remove_var("AGENTSQUAD_CONFIG");
        std::fs::remove_file(&file).ok();
        assert_eq!(names, ["b", "c", "d"]);
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
    fn remove_unregisters_project() {
        // `Delete` removes the selected project with confirmation, whether
        // or not it holds a session.
        let mk = |name: &str| Project {
            name: name.into(),
            path: PathBuf::from("x"),
            command: vec!["sh".into()],
            kind: Kind::Agent,
        };
        let mut projects = vec![mk("a"), mk("b")];
        let mut slots: Vec<Option<u8>> = vec![None, Some(1)];
        assert_eq!(
            remove_project(&mut projects, &mut slots, 1).map(|p| p.name),
            Some("b".to_string())
        );
        assert_eq!(projects.len(), 1);
        assert_eq!(slots.len(), 1);
        assert!(remove_project(&mut projects, &mut slots, 5).is_none());
        assert_eq!(projects.len(), 1);
        let text: String = confirm_remove_lines("b")
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(text.contains('b'), "remove modal names the project");
        assert!(text.contains('y') && text.contains("removes"));
    }
    #[test]
    fn remove_persists_to_config_file() {
        // A TUI removal must survive a restart: write with the real saver,
        // read back with the real loader, and confirm the project is gone.
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let file = std::env::temp_dir().join(format!(
            "agentsquad-remove-test-{}-{}.toml",
            std::process::id(),
            n
        ));
        let mk = |name: &str| Project {
            name: name.into(),
            path: PathBuf::from("x"),
            command: vec!["sh".into()],
            kind: Kind::Agent,
        };
        let config = Config {
            projects: vec![mk("a"), mk("b")],
        };
        save_to(&config, &file).unwrap();
        let mut loaded = load_from(&file).unwrap();
        assert_eq!(loaded.projects.len(), 2);
        let mut slots: Vec<Option<u8>> = vec![None, None];
        let removed = remove_project(&mut loaded.projects, &mut slots, 0).unwrap();
        assert_eq!(removed.name, "a");
        save_to(&loaded, &file).unwrap();
        let reread = load_from(&file).unwrap();
        assert_eq!(reread.projects.len(), 1);
        assert_eq!(reread.projects[0].name, "b");
        std::fs::remove_file(&file).ok();
    }
    #[test]
    fn stop_selects_live_session() {
        // `x` stops the selected project, but only while it actually
        // holds a session.
        let order = vec![0usize, 1, 2];
        let live: Vec<Option<u8>> = vec![None, Some(1), None];
        assert_eq!(stop_target(&order, 1, &live), Some(1));
        assert_eq!(stop_target(&order, 0, &live), None);
        assert_eq!(stop_target(&order, 5, &live), None);
        assert_eq!(stop_target(&[], 0, &live), None);
    }
    #[test]
    fn pty_env_advertises_truecolor() {
        let env = pty_env();
        assert!(env.contains(&("TERM", "xterm-256color")));
        assert!(env.contains(&("COLORTERM", "truecolor")));
    }
    #[test]
    fn exit_notice_fires_once() {
        let mut announced = false;
        assert_eq!(
            exit_notice("web", true, "Exited (1)", &mut announced),
            Some("Session 'web' Exited (1); Enter restarts it".to_string())
        );
        assert!(announced);
        assert_eq!(
            exit_notice("web", false, "Exited (1)", &mut announced),
            None
        );
        let mut fresh = false;
        assert_eq!(exit_notice("web", true, "Running", &mut fresh), None);
        assert!(!fresh);
        assert_eq!(
            exit_notice("api", true, "Error: boom", &mut fresh),
            Some("Session 'api' Error: boom; Enter restarts it".to_string())
        );
    }
    #[test]
    fn split_help_line_columns() {
        assert_eq!(
            split_help_line("Enter      start it"),
            ("Enter", "start it")
        );
        assert_eq!(split_help_line("no-split"), ("no-split", ""));
        assert_eq!(split_help_line("a   b  c"), ("a", "b  c"));
    }
    #[test]
    fn list_table_aligns_columns() {
        let mk = |name: &str, kind: Kind, cmd: &[&str]| Project {
            name: name.into(),
            path: PathBuf::from("/tmp"),
            command: cmd.iter().map(|s| s.to_string()).collect(),
            kind,
        };
        // Registration order is service-first; output is agents-first.
        let projects = vec![
            mk("api", Kind::Service, &["node", "server.js"]),
            mk("web", Kind::Agent, &["claude"]),
        ];
        assert_eq!(
            list_table(&projects, false),
            vec![
                "web  agent    /tmp  claude".to_string(),
                "api  service  /tmp  node server.js".to_string(),
            ]
        );
        assert!(list_table(&[], false).is_empty());
        let colored = list_table(&projects, true).join("\n");
        assert!(colored.contains("agent"), "kind text stays readable");
        assert!(
            colored.contains("\x1b["),
            "terminal output carries ANSI colors"
        );
    }
    #[test]
    fn keep_visible_tracks_selection() {
        assert_eq!(keep_visible(0, 0, 10), 0);
        assert_eq!(keep_visible(0, 9, 10), 0);
        assert_eq!(keep_visible(0, 10, 10), 1);
        assert_eq!(keep_visible(0, 15, 10), 6);
        assert_eq!(keep_visible(5, 8, 10), 5);
        assert_eq!(keep_visible(5, 3, 10), 3);
        assert_eq!(keep_visible(5, 5, 10), 5);
        assert_eq!(keep_visible(0, 5, 0), 0);
    }
    #[test]
    fn row_name_style_marks_kind() {
        use ratatui::style::Modifier as M;
        assert_eq!(
            row_name_style(true, "Running", Kind::Agent),
            selected_row_style()
        );
        assert_eq!(
            row_name_style(false, "Running", Kind::Service),
            Style::default().fg(Color::Magenta).add_modifier(M::BOLD)
        );
        assert_eq!(
            row_name_style(false, "Stopped", Kind::Agent),
            Style::default().fg(Color::Cyan).add_modifier(M::DIM)
        );
        assert_eq!(
            row_name_style(false, "Exited (1)", Kind::Service),
            Style::default().fg(Color::Magenta).add_modifier(M::DIM)
        );
    }
    #[test]
    fn sidebar_width_scales() {
        assert_eq!(sidebar_width(59), 0);
        assert_eq!(sidebar_width(60), 24);
        assert_eq!(sidebar_width(100), 25);
        assert_eq!(sidebar_width(152), 38);
        assert_eq!(sidebar_width(300), 38);
    }
    #[test]
    fn hint_text_matches_legacy_strings() {
        assert_eq!(
            mode_hints(Mode::Manager),
            "Enter focus | / send | a add | f filter | r restart | s start all | n/p select | x stop | Del remove | q quit | ? help"
        );
        assert_eq!(mode_hints(Mode::Focus), "Esc manager | type to input");
        assert_eq!(mode_hints(Mode::Send), "Enter sends | Esc cancels");
        assert_eq!(
            mode_hints(Mode::Filter),
            "type to narrow | Enter keeps | Esc clears"
        );
        assert_eq!(
            mode_hints(Mode::Add),
            "Tab next | Space kind | Enter save | Esc cancel"
        );
        assert_eq!(mode_hints(Mode::Help), "?/Esc closes");
    }
    #[test]
    fn fitted_hints_compact_on_narrow() {
        let text = |spans: Vec<Line>| {
            spans
                .iter()
                .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
                .collect::<String>()
        };
        let wide: String = text(vec![Line::from(fitted_hints(Mode::Manager, 200))]);
        assert!(wide.contains("start all"), "wide bar shows everything");
        let normal: String = text(vec![Line::from(fitted_hints(Mode::Manager, 59))]);
        assert!(normal.contains("q quit"), "normal bar keeps quit");
        assert!(normal.contains("f filter"), "normal bar keeps filter");
        assert!(!normal.contains("start all"), "normal bar drops the rest");
        let narrow: String = text(vec![Line::from(fitted_hints(Mode::Manager, 40))]);
        assert!(narrow.contains("q quit"), "narrow bar keeps quit");
        assert!(!narrow.contains("? help"), "narrow bar drops the rest");
        let add: String = text(vec![Line::from(fitted_hints(Mode::Add, 40))]);
        assert!(add.contains("Enter save") && !add.contains("Space"));
        let focus: String = text(vec![Line::from(fitted_hints(Mode::Focus, 10))]);
        assert!(focus.contains("type to input"), "short modes never compact");
        let help: String = text(vec![Line::from(fitted_hints(Mode::Help, 40))]);
        assert!(help.contains("closes"), "help hints show in help mode");
    }
    #[test]
    fn hint_spans_bold_keys_dim_descriptions() {
        let spans = hint_spans(&[("Enter", "focus"), ("", "type to input")]);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "Enter focus | type to input");
        assert_eq!(
            spans[0].style,
            Style::default().add_modifier(ratatui::style::Modifier::BOLD)
        );
        assert_eq!(
            spans[2].style,
            Style::default().add_modifier(ratatui::style::Modifier::DIM)
        );
    }
    #[test]
    fn row_widths_keep_status_visible() {
        // Narrow panel: status reserved first, name takes the rest.
        assert_eq!(row_widths(22, 3, 9, 10), (9, 9, 0));
        // Wide panel: name fits fully, detail gets the remainder.
        assert_eq!(row_widths(36, 3, 9, 10), (9, 10, 13));
        // Long error status is capped so the name keeps some room.
        let (sw, nw, dw) = row_widths(22, 3, 40, 6);
        assert!(sw + nw + dw + 3 < 22);
        assert!(sw >= 9, "status word must survive: {sw}");
    }
    #[test]
    fn input_window_keeps_cursor_visible() {
        // Short text passes through untouched.
        assert_eq!(
            input_window("ab", 'c', "de", 10),
            ("ab".into(), 'c', "de".into())
        );
        // Typing at the end shows the tail with the cursor last.
        assert_eq!(
            input_window("hello world", ' ', "", 5),
            ("orld".into(), ' ', "".into())
        );
        // Cursor at the front pins the window to the start.
        assert_eq!(
            input_window("", 'h', "ello world", 5),
            ("".into(), 'h', "ello".into())
        );
        // Cursor past the window trims the left side.
        assert_eq!(
            input_window("abcdef", 'g', "hij", 5),
            ("cdef".into(), 'g', "".into())
        );
    }
    #[test]
    fn exit_notice_kind_ranks_outcomes() {
        assert_eq!(exit_notice_kind("Error: boom"), Notice::Error);
        assert_eq!(exit_notice_kind("Exited (1)"), Notice::Warn);
        assert_eq!(exit_notice_kind("Exited (137)"), Notice::Warn);
        assert_eq!(exit_notice_kind("Exited (0)"), Notice::Info);
        assert_eq!(exit_notice_kind("Stopped"), Notice::Info);
    }
    #[test]
    fn plural_sessions_counts_nouns() {
        assert_eq!(plural_sessions(0), "0 sessions");
        assert_eq!(plural_sessions(1), "1 session");
        assert_eq!(plural_sessions(2), "2 sessions");
        assert_eq!(confirm_quit_prompt(1), "Quit and discard 1 session?");
    }
    #[test]
    fn confirm_modals_style_keys() {
        let stop = confirm_stop_lines("web");
        let stop_text: String = stop
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(stop_text.contains("web"), "stop modal names the session");
        assert!(stop_text.contains('y') && stop_text.contains("confirms"));
        let quit = confirm_quit_lines(2);
        let quit_text: String = quit
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(quit_text.contains('2'), "quit modal names the count");
        assert!(quit_text.contains('y') && quit_text.contains("quits"));
    }
    #[test]
    fn rendered_lines_compose_in_buffer() {
        use ratatui::{backend::TestBackend, widgets::Paragraph};
        fn row(line: Line<'static>, width: u16) -> String {
            let mut term = Terminal::new(TestBackend::new(width, 1)).unwrap();
            term.draw(|f| {
                f.render_widget(Paragraph::new(line), f.area());
            })
            .unwrap();
            let buf = term.backend().buffer();
            (0..width).map(|x| buf[(x, 0)].symbol()).collect()
        }
        let header = row(group_header("Agents", Kind::Agent, 2, 3), 30);
        assert!(header.starts_with("Agents (2/3)"), "got {header:?}");
        let stop = confirm_stop_lines("web").into_iter().next().unwrap();
        let first = row(stop, 40);
        assert!(first.contains("Stop session 'web'?"), "got {first:?}");
        let narrow = row(Line::from(fitted_hints(Mode::Manager, 38)), 40);
        assert!(narrow.contains("q quit"), "got {narrow:?}");
        let roomy = row(Line::from(fitted_hints(Mode::Manager, 50)), 60);
        assert!(roomy.contains("? help"), "got {roomy:?}");
    }
    #[test]
    fn notice_styles_rank_severity() {
        assert_eq!(notice_style(Notice::Info), Style::default());
        assert_eq!(
            notice_style(Notice::Warn),
            Style::default().fg(Color::Yellow)
        );
        assert_eq!(
            notice_style(Notice::Error),
            Style::default()
                .fg(Color::Red)
                .add_modifier(ratatui::style::Modifier::BOLD)
        );
    }
    #[test]
    fn fleet_summary_counts() {
        assert_eq!(fleet_summary(2, 5), "● 2/5");
        assert_eq!(fleet_summary(0, 0), "● 0/0");
    }
    #[test]
    fn summary_width_fits_alert() {
        assert_eq!(summary_width(2, 5, 0), ("● 2/5".chars().count() + 1) as u16);
        assert_eq!(
            summary_width(12, 34, 5),
            ("● 12/34".chars().count() + " ✖ 5".chars().count() + 1) as u16
        );
    }
    #[test]
    fn sidebar_border_marks_focus() {
        assert_eq!(
            sidebar_border_style(false),
            Style::default().fg(Color::Cyan)
        );
        assert_eq!(
            sidebar_border_style(true),
            Style::default().fg(Color::DarkGray)
        );
    }
    #[test]
    fn pane_border_marks_focus() {
        assert_eq!(pane_border_style(true), Style::default().fg(Color::Green));
        assert_eq!(
            pane_border_style(false),
            Style::default().fg(Color::DarkGray)
        );
    }
    #[test]
    fn failed_sessions_need_attention() {
        assert!(!needs_attention("Running"));
        assert!(!needs_attention("Stopped"));
        assert!(!needs_attention("Exited (0)"));
        assert!(needs_attention("Exited (1)"));
        assert!(needs_attention("Exited (137)"));
        assert!(needs_attention("Error: boom"));
    }
    #[test]
    fn empty_list_hint_guides_first_run() {
        assert!(empty_list_hint().contains("squad add"));
        assert!(list_table(&[], false).is_empty(), "stdout stays empty");
    }
    #[test]
    fn list_title_shows_filter_counts() {
        let base = format!("AgentSquad v{}", env!("CARGO_PKG_VERSION"));
        assert_eq!(list_title(5, 5), base);
        assert_eq!(list_title(0, 0), base);
        assert_eq!(list_title(5, 2), format!("{base} (2/5)"));
    }
    #[test]
    fn agent_tag_shows_program() {
        let mk = |command: Vec<String>| Project {
            name: "x".into(),
            path: PathBuf::from("/tmp"),
            command,
            kind: Kind::Agent,
        };
        assert_eq!(
            agent_tag(&mk(vec!["claude".into(), "--dangerous".into()])),
            Some("claude")
        );
        assert_eq!(agent_tag(&mk(shell())), None);
        assert_eq!(
            agent_tag(&mk(vec![])),
            None,
            "hand-written configs may omit the command"
        );
    }
    #[test]
    fn paint_kind_matches_tui_hues() {
        assert_eq!(paint_kind("agent  ", Kind::Agent, false), "agent  ");
        assert_eq!(paint_kind("service", Kind::Service, false), "service");
        let agent = paint_kind("agent  ", Kind::Agent, true);
        let service = paint_kind("service", Kind::Service, true);
        assert_ne!(agent, service, "kinds stay distinguishable");
        assert!(agent.contains("agent  "));
        assert!(service.contains("service"));
    }
    #[test]
    fn kind_colors_are_distinct() {
        assert_eq!(kind_color(Kind::Agent), Color::Cyan);
        assert_eq!(kind_color(Kind::Service), Color::Magenta);
        assert_ne!(kind_color(Kind::Agent), kind_color(Kind::Service));
    }
    #[test]
    fn group_header_dims_counts() {
        let line = group_header("Agents", Kind::Agent, 2, 3);
        assert_eq!(line.width(), "Agents (2/3)".len());
        assert_eq!(line.spans.len(), 2);
        assert_eq!(line.spans[0].content, "Agents");
        assert_eq!(
            line.spans[0].style,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(ratatui::style::Modifier::BOLD)
        );
        assert_eq!(line.spans[1].content, " (2/3)");
        assert_eq!(
            line.spans[1].style,
            Style::default().add_modifier(ratatui::style::Modifier::DIM)
        );
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
    fn pane_status_text_shows_state() {
        assert_eq!(
            pane_status_text("Running", Some(125), 0),
            format!("{} Running 2m", spinner_frame(0))
        );
        assert_eq!(
            pane_status_text("Running", None, 3),
            format!("{} Running", spinner_frame(3))
        );
        assert_eq!(pane_status_text("Stopped", None, 9), "● Stopped");
        assert_eq!(pane_status_text("Exited (1)", None, 9), "● Exited (1)");
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
