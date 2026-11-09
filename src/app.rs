use anyhow::{bail, Context, Result};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect, Size},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation,
        ScrollbarState,
    },
    Frame, Terminal,
};
use std::{
    io::{self, IsTerminal, Write},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use crate::config::*;
use crate::input::*;
use crate::ledger;
use crate::pathpick::{self, PathState, Pick};
use crate::select::Selection;
use crate::session::*;
use crate::sys::*;
use crate::ui::*;
use crate::updater::*;

mod actions;
mod draw;
mod keys;
mod mouse;

/// Restores the terminal on exit, including the title it found at start.
pub struct ScreenGuard(Option<String>);
impl Drop for ScreenGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            crossterm::cursor::Show
        );
        if let Some(title) = self.0.take() {
            let _ = execute!(io::stdout(), crossterm::terminal::SetTitle(title));
        }
    }
}

/// Rows and columns left for a session's terminal inside the frame.
fn pane_size(a: Size) -> (u16, u16) {
    let side = sidebar_width(a.width);
    (
        a.height.saturating_sub(3).max(1),
        a.width.saturating_sub(side).saturating_sub(2).max(1),
    )
}

const SAMPLE_EVERY: Duration = Duration::from_secs(2);

/// Everything the manager keeps between frames. `run` owns the terminal;
/// `App` owns state, drawing and input handling.
/// Where the text typed into the status-bar input line goes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SendTo {
    /// A message for the selected session.
    One,
    /// An order for everyone under the selected commander.
    All,
    /// A new commander name for the selected agent.
    Under,
}

struct App {
    cfg: Config,
    sessions: Vec<Option<Session>>,
    /// Sidebar order (filtered); `selected` indexes into it.
    order: Vec<usize>,
    selected: usize,
    list_offset: usize,
    // Last drawn geometry, so mouse clicks can be mapped to rows:
    // `row_targets[i]` is the position in `order` of list item `i` (None for headers).
    side_rect: Rect,
    row_targets: Vec<Option<usize>>,
    pane_rect: Rect,
    mode: Mode,
    send_buf: LineInput,
    /// What the status-bar input line does with the text typed into it.
    send_to: SendTo,
    filter_buf: LineInput,
    add_field: usize,
    add_path: LineInput,
    add_kind: Kind,
    add_cmd: LineInput,
    add_under: LineInput,
    add_autostart: bool,
    /// Name of the project the form is editing; None when it adds one.
    add_edit: Option<String>,
    add_err: String,
    /// What the Path field points at (checked on edits, not per frame).
    add_state: PathState,
    /// The folders listed under the Path field: the parents your projects live in
    /// while it is empty, otherwise the subfolders matching what is typed.
    add_picks: Vec<Pick>,
    /// The highlighted row of `add_picks`; None until the user steps into the list.
    add_sel: Option<usize>,
    /// Text being selected (or just copied) in the terminal pane.
    sel: Option<Selection>,
    confirm_stop: Option<usize>,
    confirm_remove: Option<usize>,
    confirm_restart: Option<usize>,
    /// Right-click quick-action menu target, if open.
    context_menu: Option<usize>,
    confirm_quit: bool,
    confirm_start_all: bool,
    message: (String, Notice),
    /// Idle hint text, restored when a message expires.
    hints: String,
    /// Last message seen and when it first appeared, for expiry.
    msg_seen: String,
    msg_at: Instant,
    dirty: bool,
    /// `squad start <name>`: start and focus the first selection once.
    first: bool,
    cores: usize,
    sys: SysUsage,
    // Measuring walks the process table, so it runs on its own thread and
    // the UI only ever does a non-blocking `try_recv` for the result.
    req_tx: mpsc::Sender<Vec<(usize, u32)>>,
    res_rx: mpsc::Receiver<Measurement>,
    last_sample: Instant,
    sampling: bool,
    // Self-update: the check runs on a worker so it can never delay startup.
    upd_tx: mpsc::Sender<UpdateMsg>,
    upd_rx: mpsc::Receiver<UpdateMsg>,
    update_offer: Option<String>,
    confirm_update: bool,
    /// An installed update waiting for the decision to restart into it.
    applied: Option<String>,
    confirm_apply: bool,
    /// Restart squad into the new build once the manager has shut down.
    relaunch: bool,
    updating: bool,
    tick: u64,
    last_anim_tick: Instant,
    /// A background agent wants attention; `take_bell` decides if it rings.
    bell: bool,
    last_bell: Option<Instant>,
    /// Projects with `autostart` start once, on the first loop pass.
    autostart_pending: bool,
    /// Sessions to start again after an update restart (`squad resume`).
    resume: Vec<usize>,
    /// The SITREP reader (`h`), open over the manager.
    history: Option<History>,
    quit: bool,
}

impl App {
    fn new(cfg: Config, initial: Option<usize>) -> Self {
        ledger::write_rosters(&cfg.projects);
        let sessions = (0..cfg.projects.len()).map(|_| None).collect();
        let order = display_order(&cfg.projects);
        let selected = start_selection(
            &cfg.projects,
            &order,
            initial,
            load_last_project().as_deref(),
        );
        let (req_tx, req_rx) = mpsc::channel::<Vec<(usize, u32)>>();
        let (res_tx, res_rx) = mpsc::channel::<Measurement>();
        thread::spawn(move || {
            for roots in req_rx {
                if res_tx.send(measure(&roots)).is_err() {
                    break;
                }
            }
        });
        let (upd_tx, upd_rx) = mpsc::channel::<UpdateMsg>();
        if update_check_enabled() {
            let tx = upd_tx.clone();
            thread::spawn(move || watch_for_updates(tx));
        }
        Self {
            cfg,
            sessions,
            order,
            selected,
            list_offset: 0,
            side_rect: Rect::default(),
            row_targets: Vec::new(),
            pane_rect: Rect::default(),
            mode: Mode::Manager,
            send_buf: LineInput::new(),
            send_to: SendTo::One,
            filter_buf: LineInput::new(),
            add_field: 0,
            add_path: LineInput::new(),
            add_kind: Kind::Agent,
            add_cmd: LineInput::new(),
            add_under: LineInput::new(),
            add_autostart: false,
            add_edit: None,
            add_err: String::new(),
            add_state: PathState::Blank,
            add_picks: Vec::new(),
            add_sel: None,
            sel: None,
            confirm_stop: None,
            confirm_remove: None,
            confirm_restart: None,
            context_menu: None,
            confirm_quit: false,
            confirm_start_all: false,
            message: (mode_hints(Mode::Manager).to_string(), Notice::Info),
            hints: mode_hints(Mode::Manager),
            msg_seen: String::new(),
            msg_at: Instant::now(),
            dirty: true,
            first: initial.is_some(),
            cores: thread::available_parallelism().map_or(1, |n| n.get()),
            sys: SysUsage::default(),
            req_tx,
            res_rx,
            // Defer the first process-table snapshot by one sample interval so AV
            // behavioral monitors (e.g. AhnLab V3 MDP) do not see CreateToolhelp32Snapshot
            // and OpenProcess calls at the exact millisecond the binary launches.
            last_sample: Instant::now(),
            sampling: false,
            upd_tx,
            upd_rx,
            update_offer: None,
            confirm_update: false,
            applied: None,
            confirm_apply: false,
            relaunch: false,
            updating: false,
            tick: 0,
            last_anim_tick: Instant::now(),
            bell: false,
            last_bell: None,
            autostart_pending: true,
            resume: Vec::new(),
            history: None,
            quit: false,
        }
    }

    /// Title for the terminal window: the selected project, named as the
    /// sidebar names it (a shell service reads as the program inside it).
    fn window_title(&self) -> String {
        let pi = self.order.get(self.selected).copied();
        let name = pi.and_then(|pi| {
            let fg = self
                .sessions
                .get(pi)?
                .as_ref()
                .and_then(|s| s.fg_name.as_deref());
            Some(live_name(self.cfg.projects.get(pi)?, fg))
        });
        window_title(name, self.attention_count())
    }

    /// Remember the selected project so the next launch opens on it.
    fn remember_selection(&self) {
        if let Some(p) = self
            .order
            .get(self.selected)
            .and_then(|&pi| self.cfg.projects.get(pi))
        {
            save_last_project(&p.name);
        }
    }

    /// Spinner clock; redraws only while something is running, in every
    /// mode so background rows never look frozen while typing in Focus.
    fn animate(&mut self) {
        if self.last_anim_tick.elapsed() >= Duration::from_millis(200) {
            self.last_anim_tick = Instant::now();
            self.tick = self.tick.wrapping_add(1);
            if self
                .sessions
                .iter()
                .flatten()
                .any(|s| s.status.is_running())
            {
                self.dirty = true;
            }
        }
    }

    fn start_initial(&mut self, rows: u16, cols: u16) {
        if self.autostart_pending {
            self.autostart_pending = false;
            let mut pis: Vec<usize> = (0..self.cfg.projects.len())
                .filter(|&i| self.cfg.projects[i].autostart)
                .collect();
            pis.append(&mut self.resume);
            pis.sort_unstable();
            pis.dedup();
            if !pis.is_empty() {
                let (n, failed) =
                    start_sessions(&mut self.sessions, &self.cfg.projects, &pis, rows, cols);
                self.message = match failed {
                    Some(e) => (format!("Autostart: {n} started; error: {e}"), Notice::Error),
                    None => (format!("Autostarted {}.", plural_sessions(n)), Notice::Info),
                };
                self.dirty = true;
            }
        }
        if self.first {
            self.first = false;
            if let Some(&pi) = self.order.get(self.selected) {
                match start_session(&mut self.sessions, &self.cfg.projects, pi, rows, cols) {
                    None => self.mode = Mode::Focus,
                    Some(e) => self.message = (e, Notice::Error),
                }
            }
        }
    }

    /// Messages fade back to the idle hints; otherwise the last one would
    /// hide the key hints for good.
    fn expire_message(&mut self) {
        if self.message.0 != self.msg_seen {
            self.msg_seen.clone_from(&self.message.0);
            self.msg_at = Instant::now();
        }
        if self.message.0 != self.hints && self.msg_at.elapsed() >= message_ttl(self.message.1) {
            self.message = (self.hints.clone(), Notice::Info);
            self.msg_seen.clone_from(&self.hints);
            self.dirty = true;
        }
    }

    /// Fold in measurements, update messages and finished sessions.
    fn pump(&mut self) {
        self.expire_message();
        while let Ok(m) = self.res_rx.try_recv() {
            self.sampling = false;
            if let Some(snap) = m.sys {
                self.sys.apply(snap);
            }
            for (pi, pid, cpu_time, mem) in m.procs {
                if let Some(Some(s)) = self.sessions.get_mut(pi) {
                    s.apply_usage(pid, cpu_time, mem, m.at, self.cores);
                }
            }
            for (pi, pid, label) in m.labels {
                if let Some(Some(s)) = self.sessions.get_mut(pi) {
                    s.apply_label(pid, label);
                }
            }
            self.dirty = true;
        }
        if !self.sampling && self.last_sample.elapsed() >= SAMPLE_EVERY {
            let mut roots = Vec::new();
            for (pi, slot) in self.sessions.iter_mut().enumerate() {
                if let Some(s) = slot {
                    match s.child.process_id().filter(|_| s.status.is_running()) {
                        Some(pid) => roots.push((pi, pid)),
                        None => s.clear_usage(),
                    }
                }
            }
            if self.req_tx.send(roots).is_ok() {
                self.sampling = true;
            }
            self.last_sample = Instant::now();
        }
        while let Ok(m) = self.upd_rx.try_recv() {
            match m {
                UpdateMsg::Available(tag) => {
                    self.message = (format!("Update {tag} available"), Notice::Warn);
                    self.update_offer = Some(tag);
                }
                UpdateMsg::Installed(tag) => {
                    self.updating = false;
                    self.message = (
                        format!(
                            "Updated to {tag}. Restart squad to use it; sessions keep running."
                        ),
                        Notice::Info,
                    );
                    // The prompt below asks whether to restart into it now.
                    self.applied = Some(tag);
                }
                UpdateMsg::Failed(e) => {
                    self.updating = false;
                    self.update_offer = None;
                    self.message = (format!("Update failed: {e}"), Notice::Error);
                }
            }
            self.dirty = true;
        }
        // Ask only when nothing else owns the keyboard, so a prompt never
        // swallows keystrokes meant for a session or another input.
        let keyboard_free = !(self.mode != Mode::Manager
            || self.history.is_some()
            || self.confirm_quit
            || self.confirm_start_all
            || self.confirm_stop.is_some()
            || self.confirm_remove.is_some()
            || self.confirm_restart.is_some());
        if keyboard_free && !self.updating && !self.confirm_update && self.update_offer.is_some() {
            self.confirm_update = true;
            self.dirty = true;
        }
        if keyboard_free && !self.confirm_update && !self.confirm_apply && self.applied.is_some() {
            self.confirm_apply = true;
            self.dirty = true;
        }
        for (pi, slot) in self.sessions.iter_mut().enumerate() {
            if let Some(s) = slot {
                if let Some(note) = s.ledger_note.take() {
                    self.message = note;
                    self.dirty = true;
                }
                let was_running = s.status.is_running();
                let phase_before = s.phase();
                let changed = s.update();
                // Output from a background session only shows in the sidebar
                // icon (the 200 ms spinner clock redraws the rest), so skip the
                // frame unless that icon or the status changed.
                let selected = self.order.get(self.selected) == Some(&pi);
                let phase_now = s.phase();
                // Opening a finished task counts as seeing it; new work resets it.
                let acked = phase_now == AgentPhase::Done && (s.acked || selected);
                self.dirty |= acked != s.acked;
                s.acked = acked;
                self.dirty |= changed
                    && (selected
                        || phase_now != phase_before
                        || s.status.is_running() != was_running);
                if self.cfg.projects[pi].kind == Kind::Agent && phase_now != s.last_phase {
                    // No bell for the session you are typing into.
                    let watching = selected && self.mode == Mode::Focus;
                    self.bell |= !watching && rings_bell(phase_now);
                    // A finished task is reported to the commander's REPORTS.md.
                    if phase_now == AgentPhase::Done && s.last_phase == AgentPhase::Working {
                        let projects = &self.cfg.projects;
                        if let Some(c) = superior(projects, pi) {
                            ledger::report(
                                &projects[c].path,
                                &projects[pi].path,
                                &projects[pi].name,
                                &projects[c].name,
                            );
                        }
                        // Reaches here when the Stop hook did not (not Claude
                        // Code, hook off, or not a git repository it can read).
                        if projects[pi].ledger && ledger::unrecorded(&projects[pi].path) {
                            self.message = (
                                format!("{} finished without filing a SITREP", projects[pi].name),
                                Notice::Warn,
                            );
                            self.dirty = true;
                        }
                    }
                    s.last_phase = phase_now;
                }
                let label = s.status.label();
                if let Some(note) = exit_notice(
                    &self.cfg.projects[pi].name,
                    was_running,
                    &label,
                    &mut s.announced_exit,
                ) {
                    self.message = (note, exit_notice_kind(&label));
                    // Typing into a dead terminal does nothing: hand the keyboard
                    // back to the manager so Enter can restart it.
                    if self.mode == Mode::Focus && self.order.get(self.selected) == Some(&pi) {
                        self.mode = Mode::Manager;
                    }
                }
            }
        }
    }

    /// Agents waiting for you (`?`) plus finished tasks you have not opened:
    /// the number shown in the window title, a silent way to get noticed.
    fn attention_count(&self) -> usize {
        self.sessions
            .iter()
            .flatten()
            .filter(|s| s.status.is_running())
            .filter(|s| match s.last_phase {
                AgentPhase::NeedsInput => true,
                AgentPhase::Done => !s.acked,
                _ => false,
            })
            .count()
    }

    /// True when the terminal bell should ring now. At most one ring per
    /// 10 s, so a flurry of questions cannot buzz.
    fn take_bell(&mut self) -> bool {
        let want = std::mem::take(&mut self.bell);
        let gap_ok = self
            .last_bell
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(10));
        if want && gap_ok && self.cfg.bell != Some(false) {
            self.last_bell = Some(Instant::now());
            return true;
        }
        false
    }

    /// Adaptive poll: 10 ms while output or typing is recent so echo feels
    /// instant; 40 ms when sessions are quiet, and keys still wake it
    /// immediately. Quieter polling keeps idle CPU low with many sessions.
    fn poll_duration(&self) -> Duration {
        let recently_active = self
            .sessions
            .iter()
            .flatten()
            .any(|s| s.is_active(Duration::from_millis(1500)));
        if recently_active || self.sampling || self.updating || self.dirty {
            Duration::from_millis(10)
        } else if self
            .sessions
            .iter()
            .flatten()
            .any(|s| s.status.is_running())
        {
            Duration::from_millis(40)
        } else {
            Duration::from_millis(50)
        }
    }

    /// `e`: the add form, filled with the selected project. Only the command
    /// and autostart can change here; path, kind and chain of command keep
    /// their own keys (`u`) so names and logs never go stale.
    fn open_edit(&mut self) {
        let Some(&pi) = self.order.get(self.selected) else {
            return;
        };
        let p = self.cfg.projects[pi].clone();
        self.open_add();
        self.add_edit = Some(p.name);
        self.add_path.set(&p.path.display().to_string());
        self.add_kind = p.kind;
        if !is_bare_shell(&p.command) {
            self.add_cmd.set(&join_command(&p.command));
        }
        self.add_under.set(p.reports_to.as_deref().unwrap_or(""));
        self.add_autostart = p.autostart;
        self.add_field = 2;
        self.path_edited();
    }

    fn open_add(&mut self) {
        self.add_edit = None;
        self.mode = Mode::Add;
        self.add_field = 0;
        self.add_path.clear();
        self.add_kind = Kind::Agent;
        self.add_cmd.clear();
        self.add_under.clear();
        self.add_autostart = false;
        self.add_err.clear();
        self.add_state = PathState::Blank;
        self.path_edited();
    }

    fn stopped_count(&self) -> usize {
        self.order
            .iter()
            .filter(|&&pi| !slot_running(&self.sessions, pi))
            .count()
    }

    /// Arguments for the new squad after an update restart: `resume` every
    /// session that was running (the selection itself is remembered on disk).
    fn relaunch_args(&self) -> Vec<String> {
        let running: Vec<String> = (0..self.cfg.projects.len())
            .filter(|&pi| slot_running(&self.sessions, pi))
            .map(|pi| self.cfg.projects[pi].name.clone())
            .collect();
        if running.is_empty() {
            return Vec::new();
        }
        std::iter::once("resume".to_string())
            .chain(running)
            .collect()
    }

    // ---- drawing ----
}

/// Run the manager; when an update was installed and the user chose to
/// restart, start the new build in this console once the screen is restored.
/// A project's SITREPs open for reading: the list, the picked one's text and
/// how far it is scrolled.
struct History {
    project: String,
    items: Vec<(String, std::path::PathBuf)>,
    sel: usize,
    scroll: u16,
    text: String,
}
impl History {
    fn open(project: &str, items: Vec<(String, std::path::PathBuf)>) -> Self {
        let mut h = Self {
            project: project.to_string(),
            items,
            sel: 0,
            scroll: 0,
            text: String::new(),
        };
        h.load();
        h
    }
    fn load(&mut self) {
        self.scroll = 0;
        self.text = self.items.get(self.sel).map_or_else(String::new, |(_, p)| {
            let t = std::fs::read_to_string(p).unwrap_or_else(|e| format!("Cannot read: {e}"));
            t.chars().take(20_000).collect() // a runaway file must not stall drawing
        });
    }
    fn select(&mut self, i: usize) {
        let i = i.min(self.items.len().saturating_sub(1));
        if i != self.sel {
            self.sel = i;
            self.load();
        }
    }
}

pub fn run(c: Config, initial: Option<usize>, resume: Vec<usize>) -> Result<()> {
    if let Some(args) = run_ui(c, initial, resume)? {
        let exe = install_path().context("Cannot find squad.exe to restart")?;
        // Wait for it: the new squad owns this console until it exits.
        let status = std::process::Command::new(exe).args(args).status()?;
        std::process::exit(status.code().unwrap_or(0));
    }
    Ok(())
}

fn run_ui(c: Config, initial: Option<usize>, resume: Vec<usize>) -> Result<Option<Vec<String>>> {
    let _ = install_path(); // remember the path before an update renames the file
    if !io::stdin().is_terminal() {
        bail!("squad needs an interactive terminal");
    }
    enable_raw_mode()?;
    if let Some(lines) = c.scrollback {
        set_scrollback_limit(lines);
    }
    let _guard = ScreenGuard(console_title());
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        crossterm::cursor::Hide
    )?;
    let stdout = io::BufWriter::new(io::stdout());
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    let mut app = App::new(c, initial);
    app.resume = resume;
    let mut title = String::new();
    loop {
        let area = terminal.size()?;
        let (rows, cols) = pane_size(area);
        app.animate();
        app.start_initial(rows, cols);
        app.pump();
        if app.take_bell() {
            let backend = terminal.backend_mut();
            let _ = backend.write_all(b"\x07");
            let _ = backend.flush();
        }
        if app.dirty {
            // Selection and the program a shell runs both mark the app dirty.
            let now = app.window_title();
            if now != title {
                let _ = execute!(terminal.backend_mut(), crossterm::terminal::SetTitle(&now));
                title = now;
            }
            let _ = terminal.backend_mut().write_all(b"\x1b[?2026h");
            terminal.draw(|f| app.draw(f))?;
            let backend = terminal.backend_mut();
            let _ = backend.write_all(b"\x1b[?2026l");
            let _ = backend.flush();
            app.dirty = false;
        }
        if !event::poll(app.poll_duration())? {
            continue;
        }
        for batch_idx in 0..16 {
            if batch_idx > 0 && !event::poll(Duration::ZERO)? {
                break;
            }
            match event::read()? {
                Event::Resize(_, _) => app.on_resize(terminal.size()?),
                Event::Paste(data) => app.on_paste(data),
                Event::Mouse(m) => app.on_mouse(m, area, rows, cols),
                Event::Key(k) if k.kind != KeyEventKind::Release => app.on_key(k, rows, cols),
                _ => {}
            }
            if app.quit {
                break;
            }
        }
        if app.quit {
            break;
        }
    }
    let relaunch = app.relaunch.then(|| app.relaunch_args());
    app.remember_selection();
    shutdown_all(&mut app.sessions);
    Ok(relaunch)
}
