use anyhow::{bail, Context, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, Paragraph},
    Terminal,
};
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    io::{self, Read, Write},
    num::NonZero,
    path::PathBuf,
    str::FromStr,
    sync::mpsc::{self, Receiver},
    thread,
    time::Duration,
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
    Ok(toml::from_str(&fs::read_to_string(p)?)?)
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
        "r          restart session",
        "x          stop session",
        "n/p, arrows  select project",
        "1-9        jump to project",
        "Ctrl+G     terminal back to manager",
        "q          quit",
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
        Mode::Manager => "Enter focus | / send | a add | f filter | r restart | n/p select | x stop | q quit | ? help",
        Mode::Focus => "type to input | Ctrl+G manager",
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
}
impl Session {
    fn spawn(p: &Project, rows: u16, cols: u16) -> Result<Self> {
        let pair = native_pty_system().openpty(size(rows, cols))?;
        let mut cmd = CommandBuilder::new(&p.command[0]);
        cmd.args(&p.command[1..]);
        cmd.cwd(&p.path);
        cmd.env("TERM", "xterm-256color");
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
}
impl Drop for Session {
    fn drop(&mut self) {
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
fn key_bytes(k: KeyEvent, app_cursor: bool) -> Vec<u8> {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let mut bytes = match k.code {
        KeyCode::Char(c) if ctrl && c.is_ascii() => vec![(c.to_ascii_lowercase() as u8) & 0x1f],
        KeyCode::Char(c) => c.to_string().into_bytes(),
        KeyCode::Enter => vec![13],
        KeyCode::Backspace => vec![127],
        KeyCode::Tab => vec![9],
        KeyCode::Esc => vec![27],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Up => if app_cursor { b"\x1bOA" } else { b"\x1b[A" }.to_vec(),
        KeyCode::Down => if app_cursor { b"\x1bOB" } else { b"\x1b[B" }.to_vec(),
        KeyCode::Right => if app_cursor { b"\x1bOC" } else { b"\x1b[C" }.to_vec(),
        KeyCode::Left => if app_cursor { b"\x1bOD" } else { b"\x1b[D" }.to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
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
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}
fn run(mut c: Config, initial: Option<usize>) -> Result<()> {
    enable_raw_mode()?;
    let _guard = ScreenGuard;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut sessions: Vec<Option<Session>> = (0..c.projects.len()).map(|_| None).collect();
    let mut order = display_order(&c.projects);
    let mut selected = initial
        .and_then(|i| order.iter().position(|&x| x == i))
        .unwrap_or(0);
    let mut focus = false;
    let mut prefix = false;
    let mut sending = false;
    let mut send_buf = String::new();
    let mut show_help = false;
    let mut filtering = false;
    let mut filter_buf = String::new();
    let mut adding = false;
    let mut add_field: usize = 0;
    let mut add_path = String::new();
    let mut add_kind = Kind::Agent;
    let mut add_cmd = String::new();
    let mut add_err = String::new();
    let mut message = mode_hints(Mode::Manager).to_string();
    let mut dirty = true;
    let mut first = initial.is_some();
    loop {
        let area = terminal.size()?;
        let sidebar = if area.width >= 60 { 24 } else { 0 };
        let rows = area.height.saturating_sub(3).max(1);
        let cols = area.width.saturating_sub(sidebar).saturating_sub(2).max(1);
        if first {
            first = false;
            if let Some(&pi) = order.get(selected) {
                match Session::spawn(&c.projects[pi], rows, cols) {
                    Ok(s) => {
                        sessions[pi] = Some(s);
                        focus = true;
                    }
                    Err(e) => message = e.to_string(),
                }
            }
        }
        for s in sessions.iter_mut().flatten() {
            dirty |= s.update();
        }
        if dirty {
            terminal.draw(|f| {
                let vertical = Layout::default().direction(Direction::Vertical).constraints([Constraint::Min(1), Constraint::Length(1)]).split(f.area());
                let panels = Layout::default().direction(Direction::Horizontal).constraints([Constraint::Length(sidebar), Constraint::Min(1)]).split(vertical[0]);
                if sidebar > 0 {
                    let sel_pi = order.get(selected).copied();
                    let inner_w = (sidebar as usize).saturating_sub(2);
                    let mut items: Vec<ListItem> = Vec::new();
                    let mut n = 0;
                    for (title, k) in [("Agents", Kind::Agent), ("Services", Kind::Service)] {
                        items.push(ListItem::new(title).style(Style::default().add_modifier(ratatui::style::Modifier::BOLD)));
                        let mut any = false;
                        for &pi in order.iter().filter(|&&i| c.projects[i].kind == k) {
                            any = true;
                            n += 1;
                            let p = &c.projects[pi];
                            let status = sessions[pi].as_ref().map(|s| s.status.as_str()).unwrap_or("Stopped");
                            let mut second = format!("  {status}");
                            if k == Kind::Service { second.push(' '); second.push_str(&p.command.join(" ")); }
                            let mark = if Some(pi) == sel_pi { ">" } else { " " };
                            let row_style = if Some(pi) == sel_pi {
                                Style::default().fg(Color::Cyan).bg(Color::DarkGray).add_modifier(ratatui::style::Modifier::BOLD)
                            } else {
                                Style::default()
                            };
                            let status_span_style = if Some(pi) == sel_pi { row_style } else { status_style(status) };
                            items.push(ListItem::new(Line::from(vec![
                                Span::styled(fit(&format!("{mark}{n} {}", p.name), inner_w), row_style),
                                Span::styled(fit(&second, inner_w), status_span_style),
                            ])));
                        }
                        if !any { items.push(ListItem::new("  (none)").style(Style::default().add_modifier(ratatui::style::Modifier::DIM))); }
                    }
                    f.render_widget(List::new(items).block(Block::default().borders(Borders::ALL).border_type(BorderType::Rounded).title("AgentSquad")), panels[0]);
                }
                let sel_pi = order.get(selected).copied();
                let title = sel_pi.and_then(|pi| c.projects.get(pi)).map(|p| format!("{} [{} · {}]", p.name, p.kind.as_str(), if focus { "INPUT" } else { "MANAGER" })).unwrap_or("No projects: squad add <path>".into());
                let block = Block::default().borders(Borders::ALL).border_type(BorderType::Rounded).title(title); let inner = block.inner(panels[1]); f.render_widget(block, panels[1]);
                if let Some(s) = sel_pi.and_then(|pi| sessions.get(pi).and_then(|o| o.as_ref())) {
                    let screen = s.parser.screen();
                    let mut lines = Vec::new();
                    for row in 0..inner.height { let mut spans = Vec::new(); for col in 0..inner.width {
                        if let Some(cell) = screen.cell(row,col) {
                            if cell.is_wide_continuation() { continue; }
                            let mut style = Style::default().fg(color(cell.fgcolor())).bg(color(cell.bgcolor()));
                            if cell.bold() { style = style.add_modifier(ratatui::style::Modifier::BOLD); }
                            if cell.italic() { style = style.add_modifier(ratatui::style::Modifier::ITALIC); }
                            if cell.underline() { style = style.add_modifier(ratatui::style::Modifier::UNDERLINED); }
                            if cell.inverse() { style = style.add_modifier(ratatui::style::Modifier::REVERSED); }
                            spans.push(Span::styled(if cell.has_contents() { cell.contents() } else { " " }, style));
                        }
                    } lines.push(Line::from(spans)); }
                    f.render_widget(Paragraph::new(lines), inner);
                    if focus && !screen.hide_cursor() { let (r,c) = screen.cursor_position(); if r < inner.height && c < inner.width { f.set_cursor_position((inner.x+c,inner.y+r)); } }
                } else { f.render_widget(Paragraph::new("Enter to start. Ctrl+G returns to manager.\nRegister projects with: squad add <path> [-- command args...]"), inner); }
                let mode = mode_of(focus, sending, filtering, adding, show_help);
                let (chip_text, chip_style) = mode_chip(mode);
                let content = if prefix {
                    "Ctrl+G prefix: g = send Ctrl+G; any other key = manager".to_string()
                } else if filtering {
                    format!("Filter: {filter_buf}_ ({} matches, Enter keeps, Esc clears)", order.len())
                } else if sending {
                    let name = sel_pi.and_then(|pi| c.projects.get(pi)).map(|p| p.name.as_str()).unwrap_or("?");
                    format!("Send to {name}: {send_buf}_ (Enter sends, Esc cancels)")
                } else if adding {
                    mode_hints(Mode::Add).to_string()
                } else {
                    message.clone()
                };
                f.render_widget(Paragraph::new(Line::from(vec![
                    Span::styled(chip_text, chip_style.add_modifier(ratatui::style::Modifier::BOLD)),
                    Span::raw(format!(" {content}")),
                ])), vertical[1]);
                if show_help {
                    let popup = centered_rect(60, 50, f.area());
                    f.render_widget(ratatui::widgets::Clear, popup);
                    let lines: Vec<Line> = help_lines().iter().map(|l| Line::from(l.to_string())).collect();
                    f.render_widget(Paragraph::new(lines).block(Block::default().borders(Borders::ALL).border_type(BorderType::Double).title("Help (?/Esc closes)")), popup);
                }
                if adding {
                    let popup = centered_rect(70, 60, f.area());
                    f.render_widget(ratatui::widgets::Clear, popup);
                    let kind_hint = if add_kind == Kind::Agent { "agent (Space toggles)" } else { "service (Space toggles)" };
                    let fields = [("Path", add_path.as_str()), ("Kind", kind_hint), ("Command", if add_cmd.is_empty() { "(default shell)" } else { add_cmd.as_str() })];
                    let mut lines = Vec::new();
                    for (i, (label, val)) in fields.iter().enumerate() {
                        let active = i == add_field;
                        let mark = if active { ">" } else { " " };
                        let cursor = if active { "_" } else { "" };
                        let label_style = if active {
                            Style::default().add_modifier(ratatui::style::Modifier::BOLD)
                        } else {
                            Style::default().fg(Color::DarkGray)
                        };
                        lines.push(Line::from(vec![
                            Span::styled(format!("{mark} {label}: "), label_style),
                            Span::styled(format!("{val}{cursor}"), if active { Style::default().add_modifier(ratatui::style::Modifier::BOLD) } else { Style::default() }),
                        ]));
                    }
                    if !add_err.is_empty() {
                        lines.push(Line::from(Span::styled(format!("Error: {add_err}"), Style::default().fg(Color::Red))));
                    }
                    f.render_widget(Paragraph::new(lines).block(Block::default().borders(Borders::ALL).border_type(BorderType::Double).title("Add project (Enter save, Esc cancel)")), popup);
                }
            })?;
            dirty = false;
        }
        if !event::poll(Duration::from_millis(100))? {
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
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                dirty = true;
                if focus {
                    if k.code == KeyCode::Char('g') && k.modifiers.contains(KeyModifiers::CONTROL) {
                        focus = false;
                        prefix = true;
                        continue;
                    }
                    if let Some(&pi) = order.get(selected) {
                        if let Some(Some(s)) = sessions.get_mut(pi) {
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
                            let line = std::mem::take(&mut send_buf);
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
                                    sessions[pi] = None;
                                    match Session::spawn(&c.projects[pi], rows, cols) {
                                        Ok(s) => sessions[pi] = Some(s),
                                        Err(e) => message = format!("{e:#}"),
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
                            send_buf.pop();
                        }
                        KeyCode::Char(ch)
                            if !k.modifiers.contains(KeyModifiers::CONTROL)
                                && !k.modifiers.contains(KeyModifiers::ALT) =>
                        {
                            send_buf.push(ch)
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
                            filter_buf.pop();
                            order = apply_filter(&c.projects, &filter_buf);
                            selected = 0;
                        }
                        KeyCode::Char(ch)
                            if !k.modifiers.contains(KeyModifiers::CONTROL)
                                && !k.modifiers.contains(KeyModifiers::ALT) =>
                        {
                            filter_buf.push(ch);
                            order = apply_filter(&c.projects, &filter_buf);
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
                            match build_project(&c.projects, &add_path, add_kind, &add_cmd) {
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
                                            message = format!("Registered {name} ({kind_label})");
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
                                    add_path.pop();
                                }
                                2 => {
                                    add_cmd.pop();
                                }
                                _ => {}
                            }
                            add_err.clear();
                        }
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
                                0 => add_path.push(ch),
                                2 => add_cmd.push(ch),
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
                }
                match k.code {
                    KeyCode::Char('q') => break,
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
                    KeyCode::Char('r') => {
                        if let Some(&pi) = order.get(selected) {
                            sessions[pi] = None;
                            match Session::spawn(&c.projects[pi], rows, cols) {
                                Ok(s) => {
                                    sessions[pi] = Some(s);
                                    message = format!("Restarted {}", c.projects[pi].name);
                                }
                                Err(e) => message = format!("{e:#}"),
                            }
                        }
                    }
                    KeyCode::Char('x') => {
                        if let Some(&pi) = order.get(selected) {
                            sessions[pi] = None;
                            message = "Session stopped. Enter restarts it.".into();
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(&pi) = order.get(selected) {
                            if sessions[pi]
                                .as_ref()
                                .map(|s| s.status != "Running")
                                .unwrap_or(true)
                            {
                                sessions[pi] = None;
                                match Session::spawn(&c.projects[pi], rows, cols) {
                                    Ok(s) => sessions[pi] = Some(s),
                                    Err(e) => {
                                        message = format!("{e:#}");
                                        continue;
                                    }
                                }
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
        println!("AgentSquad v0.1\n\nsquad                         Open manager\nsquad add <path> [--kind agent|service] [-- cmd args] Register project (default: shell)\nsquad list                    List registered projects\nsquad remove <name>           Unregister (does not delete files)\nsquad start <name>            Open manager and start project\nsquad --version\n\nManager: n/p or arrows select, Enter starts/focuses, a adds, / sends a line, f filters, r restarts, x stops, 1-9 jumps, q quits, ? help.\nTerminal: Ctrl+G returns to manager; then g sends Ctrl+G.\nQuitting stops managed child processes; no detached daemon.");
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
            for p in c.projects {
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
                .context("Project not registered")?;
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
            "Enter", "/", "n/p", "filter", "restart", "add", "1-9", "x", "q", "?", "Ctrl+G",
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
}
