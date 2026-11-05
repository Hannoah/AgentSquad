use anyhow::{bail, Context, Result};
use crossterm::{event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers}, execute, terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen}};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use ratatui::{backend::CrosstermBackend, layout::{Constraint, Direction, Layout}, style::{Color, Style}, text::{Line, Span}, widgets::{Block, Borders, List, ListItem, Paragraph}, Terminal};
use serde::{Deserialize, Serialize};
use std::{env, fs, io::{self, Read, Write}, path::PathBuf, sync::mpsc::{self, Receiver}, thread, time::Duration};

#[derive(Clone, Serialize, Deserialize, Debug)]
struct Project { name: String, path: PathBuf, command: Vec<String> }
#[derive(Default, Serialize, Deserialize)]
struct Config { #[serde(default)] projects: Vec<Project> }
fn config_path() -> Result<PathBuf> {
    if let Some(p) = env::var_os("AGENTSQUAD_CONFIG") { return Ok(p.into()); }
    let base = env::var_os("LOCALAPPDATA").or_else(|| env::var_os("XDG_CONFIG_HOME")).map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))
        .context("Set AGENTSQUAD_CONFIG to a writable config file")?;
    Ok(base.join("AgentSquad").join("config.toml"))
}
fn load() -> Result<Config> { let p = config_path()?; if !p.exists() { return Ok(Config::default()); } Ok(toml::from_str(&fs::read_to_string(p)?)?) }
fn save(c: &Config) -> Result<()> { let p = config_path()?; if let Some(parent) = p.parent() { fs::create_dir_all(parent)?; } fs::write(p, toml::to_string_pretty(c)?)?; Ok(()) }
fn shell() -> Vec<String> { if cfg!(windows) { vec!["cmd.exe".into()] } else { vec!["sh".into()] } }
fn size(rows: u16, cols: u16) -> PtySize { PtySize { rows: rows.max(1), cols: cols.max(1), pixel_width: 0, pixel_height: 0 } }
struct Session {
    master: Box<dyn MasterPty + Send>, child: Box<dyn Child + Send + Sync>, writer: Box<dyn Write + Send>,
    rx: Receiver<Vec<u8>>, parser: vt100::Parser, status: String,
}
impl Session {
    fn spawn(p: &Project, rows: u16, cols: u16) -> Result<Self> {
        let pair = native_pty_system().openpty(size(rows, cols))?;
        let mut cmd = CommandBuilder::new(&p.command[0]);
        cmd.args(&p.command[1..]); cmd.cwd(&p.path); cmd.env("TERM", "xterm-256color");
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let child = pair.slave.spawn_command(cmd).with_context(|| format!("Cannot start {}", p.command[0]))?;
        drop(pair.slave);
        // Bounded queue: at most 32 * 4096 bytes of pending output per session.
        let (tx, rx) = mpsc::sync_channel(32);
        thread::spawn(move || { let mut buf = [0u8; 4096]; loop { match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => if tx.send(buf[..n].to_vec()).is_err() { break; },
        } } });
        Ok(Self { master: pair.master, child, writer, rx, parser: vt100::Parser::new(rows.max(1), cols.max(1), 500), status: "Running".into() })
    }
    fn update(&mut self) -> bool {
        let mut changed = false;
        for _ in 0..32 { match self.rx.try_recv() { Ok(bytes) => { self.parser.process(&bytes); changed = true; }, Err(_) => break } }
        if self.status == "Running" { match self.child.try_wait() {
            Ok(Some(exit)) => { self.status = format!("Exited ({})", exit.exit_code()); changed = true; },
            Err(e) => { self.status = format!("Error: {e}"); changed = true; }, _ => {}
        } }
        changed
    }
    fn resize(&mut self, rows: u16, cols: u16) -> Result<()> { self.master.resize(size(rows, cols))?; self.parser.set_size(rows.max(1), cols.max(1)); Ok(()) }
    fn input(&mut self, bytes: &[u8]) -> Result<()> { self.writer.write_all(bytes)?; self.writer.flush()?; Ok(()) }
}
impl Drop for Session { fn drop(&mut self) { if self.child.try_wait().ok().flatten().is_none() { let _ = self.child.kill(); let _ = self.child.wait(); } } }
fn color(c: vt100::Color) -> Color { match c { vt100::Color::Default => Color::Reset, vt100::Color::Idx(n) => Color::Indexed(n), vt100::Color::Rgb(r,g,b) => Color::Rgb(r,g,b) } }
fn key_bytes(k: KeyEvent, app_cursor: bool) -> Vec<u8> {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let mut bytes = match k.code {
        KeyCode::Char(c) if ctrl && c.is_ascii() => vec![(c.to_ascii_lowercase() as u8) & 0x1f],
        KeyCode::Char(c) => c.to_string().into_bytes(),
        KeyCode::Enter => vec![13], KeyCode::Backspace => vec![127], KeyCode::Tab => vec![9], KeyCode::Esc => vec![27],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Up => if app_cursor { b"\x1bOA" } else { b"\x1b[A" }.to_vec(),
        KeyCode::Down => if app_cursor { b"\x1bOB" } else { b"\x1b[B" }.to_vec(),
        KeyCode::Right => if app_cursor { b"\x1bOC" } else { b"\x1b[C" }.to_vec(),
        KeyCode::Left => if app_cursor { b"\x1bOD" } else { b"\x1b[D" }.to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(), KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(), KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(), KeyCode::PageDown => b"\x1b[6~".to_vec(), _ => Vec::new(),
    };
    if k.modifiers.contains(KeyModifiers::ALT) && !bytes.is_empty() { bytes.insert(0, 27); }
    bytes
}
struct ScreenGuard;
impl Drop for ScreenGuard { fn drop(&mut self) { let _ = disable_raw_mode(); let _ = execute!(io::stdout(), LeaveAlternateScreen); } }
fn run(c: Config, initial: Option<usize>) -> Result<()> {
    enable_raw_mode()?; let _guard = ScreenGuard; execute!(io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut sessions: Vec<Option<Session>> = (0..c.projects.len()).map(|_| None).collect();
    let mut selected = initial.unwrap_or(0); let mut focus = false; let mut prefix = false;
    let mut message = "Enter: start / focus | Ctrl+G: manager | n/p: select | x: stop | q: quit".to_string();
    let mut dirty = true; let mut first = initial.is_some();
    loop {
        let area = terminal.size()?;
        let sidebar = if area.width >= 60 { 24 } else { 0 };
        let rows = area.height.saturating_sub(3).max(1); let cols = area.width.saturating_sub(sidebar).saturating_sub(2).max(1);
        if first { first = false; if let Some(p) = c.projects.get(selected) { match Session::spawn(p, rows, cols) { Ok(s) => { sessions[selected] = Some(s); focus = true; }, Err(e) => message = e.to_string() } } }
        for s in sessions.iter_mut().flatten() { dirty |= s.update(); }
        if dirty {
            terminal.draw(|f| {
                let vertical = Layout::default().direction(Direction::Vertical).constraints([Constraint::Min(1), Constraint::Length(1)]).split(f.area());
                let panels = Layout::default().direction(Direction::Horizontal).constraints([Constraint::Length(sidebar), Constraint::Min(1)]).split(vertical[0]);
                if sidebar > 0 {
                    let items: Vec<ListItem> = c.projects.iter().enumerate().map(|(i,p)| {
                        let status = sessions[i].as_ref().map(|s| s.status.as_str()).unwrap_or("Stopped");
                        ListItem::new(format!("{} {}\n  {}", if i == selected { ">" } else { " " }, p.name, status)).style(if i == selected { Style::default().fg(Color::Cyan) } else { Style::default() })
                    }).collect();
                    f.render_widget(List::new(items).block(Block::default().borders(Borders::ALL).title("AgentSquad")), panels[0]);
                }
                let title = c.projects.get(selected).map(|p| format!("{} [{}]", p.name, if focus { "INPUT" } else { "MANAGER" })).unwrap_or("No projects: squad add <path>".into());
                let block = Block::default().borders(Borders::ALL).title(title); let inner = block.inner(panels[1]); f.render_widget(block, panels[1]);
                if let Some(Some(s)) = sessions.get(selected) {
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
                            spans.push(Span::styled(if cell.has_contents() { cell.contents() } else { " ".into() }, style));
                        }
                    } lines.push(Line::from(spans)); }
                    f.render_widget(Paragraph::new(lines), inner);
                    if focus && !screen.hide_cursor() { let (r,c) = screen.cursor_position(); if r < inner.height && c < inner.width { f.set_cursor_position((inner.x+c,inner.y+r)); } }
                } else { f.render_widget(Paragraph::new("Enter to start. Ctrl+G returns to manager.\nRegister projects with: squad add <path> [-- command args...]"), inner); }
                f.render_widget(Paragraph::new(if prefix { "Ctrl+G prefix: g = send Ctrl+G; any other key = manager" } else { &message }), vertical[1]);
            })?;
            dirty = false;
        }
        if !event::poll(Duration::from_millis(100))? { continue; }
        match event::read()? {
            Event::Resize(_,_) => { let a = terminal.size()?; let side = if a.width >= 60 {24} else {0}; for s in sessions.iter_mut().flatten() { if let Err(e) = s.resize(a.height.saturating_sub(3).max(1), a.width.saturating_sub(side).saturating_sub(2).max(1)) { message = e.to_string(); } } dirty = true; },
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                dirty = true;
                if focus {
                    if k.code == KeyCode::Char('g') && k.modifiers.contains(KeyModifiers::CONTROL) { focus = false; prefix = true; continue; }
                    if let Some(Some(s)) = sessions.get_mut(selected) { let bytes = key_bytes(k, s.parser.screen().application_cursor()); if let Err(e) = s.input(&bytes) { message = e.to_string(); focus = false; } }
                    continue;
                }
                if prefix { prefix = false; if k.code == KeyCode::Char('g') { if let Some(Some(s)) = sessions.get_mut(selected) { let _ = s.input(&[7]); focus = true; } continue; } }
                match k.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Down | KeyCode::Char('n') if !c.projects.is_empty() => selected = (selected+1)%c.projects.len(),
                    KeyCode::Up | KeyCode::Char('p') if !c.projects.is_empty() => selected = (selected+c.projects.len()-1)%c.projects.len(),
                    KeyCode::Char('x') if selected < sessions.len() => { sessions[selected] = None; message = "Session stopped. Enter restarts it.".into(); },
                    KeyCode::Enter if selected < sessions.len() => {
                        if sessions[selected].as_ref().map(|s| s.status != "Running").unwrap_or(true) {
                            sessions[selected] = None;
                            match Session::spawn(&c.projects[selected],rows,cols) { Ok(s) => sessions[selected] = Some(s), Err(e) => { message = format!("{e:#}"); continue; } }
                        }
                        focus = true;
                    }, _ => {}
                }
            }, _ => {}
        }
    }
    drop(sessions); Ok(())
}
fn main() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    if matches!(args.first().map(String::as_str), Some("--help" | "-h" | "help")) {
        println!("AgentSquad v0.1\n\nsquad                         Open manager\nsquad add <path> [-- cmd args] Register project (default: shell)\nsquad list                    List registered projects\nsquad remove <name>           Unregister (does not delete files)\nsquad start <name>            Open manager and start project\nsquad --version\n\nManager: n/p or arrows select, Enter starts/focuses, x stops, q quits.\nTerminal: Ctrl+G returns to manager; then g sends Ctrl+G.\nQuitting stops managed child processes; no detached daemon."); return Ok(());
    }
    if args.first().map(String::as_str) == Some("--version") { println!("squad {}", env!("CARGO_PKG_VERSION")); return Ok(()); }
    let mut c = load()?;
    match args.first().map(String::as_str) {
        None => run(c,None),
        Some("add") => {
            let path = fs::canonicalize(args.get(1).context("Usage: squad add <path> [-- command args...]")?)?;
            if !path.is_dir() { bail!("Project path must be a directory"); }
            let name = path.file_name().context("Cannot use filesystem root as project")?.to_string_lossy().into_owned();
            if c.projects.iter().any(|p| p.name.eq_ignore_ascii_case(&name)) { bail!("Project name already registered: {name}"); }
            let command = if args.len() > 2 { if args[2] != "--" || args.len() < 4 { bail!("Use -- before command: squad add <path> -- claude"); } args[3..].to_vec() } else { shell() };
            c.projects.push(Project {name: name.clone(),path,command}); save(&c)?; println!("Registered {name}"); Ok(())
        },
        Some("list") => { for p in c.projects { println!("{}\t{}\t{}",p.name,p.path.display(),p.command.join(" ")); } Ok(()) },
        Some("remove") => { let name = args.get(1).context("Usage: squad remove <name>")?; let old = c.projects.len(); c.projects.retain(|p| !p.name.eq_ignore_ascii_case(name)); if old == c.projects.len() { bail!("Unknown project: {name}"); } save(&c)?; println!("Unregistered {name}; project files unchanged"); Ok(()) },
        Some("start") => { let name = args.get(1).context("Usage: squad start <name>")?; let i = c.projects.iter().position(|p| p.name.eq_ignore_ascii_case(name)).context("Project not registered")?; run(c,Some(i)) },
        Some(other) => bail!("Unknown command: {other}. Use squad --help"),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn config_roundtrip() { let c = Config { projects: vec![Project { name:"demo".into(),path:PathBuf::from("demo"),command:shell() }] }; let decoded: Config = toml::from_str(&toml::to_string(&c).unwrap()).unwrap(); assert_eq!(decoded.projects[0].name,"demo"); }
    #[test] fn keyboard() { assert_eq!(key_bytes(KeyEvent::new(KeyCode::Char('c'),KeyModifiers::CONTROL),false),vec![3]); assert_eq!(key_bytes(KeyEvent::new(KeyCode::Up,KeyModifiers::NONE),true),b"\x1bOA"); }
    #[test] fn terminal_parser() { let mut p = vt100::Parser::new(10,40,50); p.process(b"hello\r\nworld"); assert!(p.screen().contents().contains("world")); }
    #[test] fn pty_smoke() { let p = Project {name:"smoke".into(),path:env::current_dir().unwrap(),command:if cfg!(windows) {vec!["cmd.exe".into(),"/d".into(),"/c".into(),"echo AGENTSQUAD_OK".into()]} else {vec!["sh".into(),"-c".into(),"echo AGENTSQUAD_OK".into()]}}; let mut s = Session::spawn(&p,24,80).unwrap(); for _ in 0..100 { s.update(); if s.parser.screen().contents().contains("AGENTSQUAD_OK") { return; } thread::sleep(Duration::from_millis(50)); } panic!("PTY output not received"); }
}
