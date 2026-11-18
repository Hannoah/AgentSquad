use anyhow::{anyhow, bail, Context, Result};
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
    collections::{BTreeMap, BTreeSet, HashMap},
    io::{self, IsTerminal, Write},
    sync::mpsc,
    thread,
    time::{Duration, Instant, SystemTime},
};

use crate::config::*;
use crate::envvault::{self, EnvVault};
use crate::history;
use crate::input::*;
use crate::ledger;
use crate::pathpick::{self, PathState, Pick};
use crate::select::Selection;
use crate::session::*;
use crate::sys::*;
use crate::ui::*;
use crate::updater::*;

pub(crate) mod actions;
pub(crate) mod draw;
pub(crate) mod keys;
pub(crate) mod mouse;

/// The memory monitor: biggest processes, the picked row, and the process
/// waiting for a yes/no before it is killed.
pub(crate) struct ProcList {
    items: Vec<ProcInfo>,
    sel: usize,
    confirm: Option<ProcInfo>,
    /// First listed row; drawing moves it just enough to keep `sel` shown,
    /// so a click maps to the row it hit.
    top: std::cell::Cell<usize>,
    /// Full command line of the picked process, read when the pick changes.
    cmd: Option<(u32, Option<String>)>,
}
impl ProcList {
    /// Read the picked process's command line unless it is already held.
    fn load_cmd(&mut self) {
        let Some(it) = self.items.get(self.sel) else {
            return;
        };
        if self.cmd.as_ref().is_none_or(|(pid, _)| *pid != it.pid) {
            self.cmd = Some((it.pid, command_line(it.pid)));
        }
    }
    /// Reload the process list while keeping the same PID selected if still alive.
    fn refresh(&mut self) {
        let prev_pid = self.items.get(self.sel).map(|it| it.pid);
        self.items = top_processes(PROC_ROWS);
        self.sel = prev_pid
            .and_then(|pid| self.items.iter().position(|it| it.pid == pid))
            .unwrap_or_else(|| self.sel.min(self.items.len().saturating_sub(1)));
        self.load_cmd();
    }
}
/// Rows the memory monitor lists.
const PROC_ROWS: usize = 50;

/// Hand the terminal back as it was before `run_ui` took it over.
fn restore_screen() {
    let _ = disable_raw_mode();
    #[cfg(unix)]
    let _ = execute!(io::stdout(), event::DisableBracketedPaste);
    let _ = execute!(
        io::stdout(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        crossterm::cursor::Show
    );
}

/// Restores the terminal on exit, including the title it found at start.
pub struct ScreenGuard(Option<String>);
impl Drop for ScreenGuard {
    fn drop(&mut self) {
        restore_screen();
        if let Some(title) = self.0.take() {
            let _ = execute!(io::stdout(), crossterm::terminal::SetTitle(title));
        }
    }
}

/// Without this a panic prints into the alternate screen and vanishes with it.
/// Appends every panic to `crash.log` next to the config; a panic on the main
/// thread also leaves the alternate screen first so the message stays visible.
fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(dir) = config_path()
            .ok()
            .and_then(|p| p.parent().map(std::path::PathBuf::from))
        {
            let secs = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("crash.log"))
            {
                let _ = writeln!(
                    f,
                    "{} squad v{} thread '{}': {info}\n{}\n",
                    ledger::utc_stamp(secs),
                    env!("CARGO_PKG_VERSION"),
                    thread::current().name().unwrap_or("?"),
                    std::backtrace::Backtrace::force_capture()
                );
            }
        }
        // Background threads leave the screen alone: the manager keeps running.
        if thread::current().name() == Some("main") {
            restore_screen();
        }
        default(info);
    }));
}

const SAMPLE_EVERY: Duration = Duration::from_secs(2);
/// Longest one pass spends on queued input before it draws again. A paste on
/// Windows arrives as one key event per character.
const EVENT_BUDGET: Duration = Duration::from_millis(8);
/// A session start that finished on its worker: the project's name and how it went.
type Started = (String, Result<Session>);

/// Everything the manager keeps between frames. `run` owns the terminal;
/// `App` owns state, drawing and input handling.
/// Where the text typed into the status-bar input line goes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SendTo {
    /// A message for the selected session.
    One,
    /// An order for everyone under the selected commander.
    All,
    /// A new commander name for the selected agent.
    Under,
}

/// Target for the shared environment variable modal (`E` or Add form).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnvTarget {
    /// Opened from the Add/Edit project modal; `Enter` confirms selection back to `add_env`.
    AddForm,
    /// Opened from Manager on project `pi`; `Enter` writes checked keys to `<project>/.env`.
    Project(usize),
    /// Opened from Manager with no projects registered; manages the shared vault only.
    VaultOnly,
}

/// Inline form inside `EnvModal` for registering or editing a shared env variable.
pub(crate) struct EnvEditor {
    pub(crate) key: LineInput,
    pub(crate) value: LineInput,
    /// Active input field: `0` = key, `1` = value.
    pub(crate) field: usize,
    /// Original key when editing an existing entry; `None` when creating a new one.
    pub(crate) editing_key: Option<String>,
    pub(crate) err: String,
}

impl EnvEditor {
    /// An empty form for a new variable.
    pub(crate) fn blank() -> Self {
        EnvEditor {
            key: LineInput::new(),
            value: LineInput::new(),
            field: 0,
            editing_key: None,
            err: String::new(),
        }
    }
}

/// Shared Environment Variable Vault & `.env` assignment popup.
/// `sel` runs one past the last variable: that row is "+ New variable".
pub(crate) struct EnvModal {
    pub(crate) target: EnvTarget,
    pub(crate) sel: usize,
    pub(crate) top: std::cell::Cell<usize>,
    pub(crate) picked: BTreeSet<String>,
    /// Existing keys/values parsed from the target project's `.env` file.
    pub(crate) project_dotenv: BTreeMap<String, String>,
    pub(crate) reveal: bool,
    pub(crate) confirm_delete: Option<String>,
    pub(crate) editor: Option<EnvEditor>,
}

/// What the read-only environment popup shows: project `pi`'s `.env`, read
/// once when it opens.
pub(crate) struct EnvInfo {
    pub(crate) pi: usize,
    pub(crate) reveal: bool,
    pub(crate) dotenv: BTreeMap<String, String>,
}

pub(crate) struct App {
    pub(crate) cfg: Config,
    pub(crate) sessions: Vec<Option<Session>>,
    /// Sidebar order (filtered); `selected` indexes into it.
    pub(crate) order: Vec<usize>,
    /// Cached `rank_prefix` per project index in `cfg.projects` to avoid `O(N^2)`
    /// tree traversals on every redraw.
    pub(crate) rank_prefixes: Vec<String>,
    pub(crate) selected: usize,
    pub(crate) list_offset: usize,
    // Last drawn geometry, so mouse clicks can be mapped to rows:
    // `row_targets[i]` is the position in `order` of list item `i` (None for headers).
    pub(crate) side_rect: Rect,
    pub(crate) row_targets: Vec<Option<usize>>,
    pub(crate) pane_rect: Rect,
    pub(crate) mode: Mode,
    pub(crate) send_buf: LineInput,
    /// What the status-bar input line does with the text typed into it.
    pub(crate) send_to: SendTo,
    pub(crate) filter_buf: LineInput,
    pub(crate) add_field: usize,
    pub(crate) add_path: LineInput,
    pub(crate) add_kind: Kind,
    pub(crate) add_cmd: LineInput,
    pub(crate) add_under: LineInput,
    pub(crate) add_autostart: bool,
    /// Shared env keys selected in Add/Edit mode to apply to `.env` on save.
    pub(crate) add_env: BTreeSet<String>,
    /// Name of the project the form is editing; None when it adds one.
    pub(crate) add_edit: Option<String>,
    pub(crate) add_err: String,
    /// What the Path field points at (checked on edits, not per frame).
    pub(crate) add_state: PathState,
    /// The folders listed under the Path field: the parents your projects live in
    /// while it is empty, otherwise the subfolders matching what is typed.
    pub(crate) add_picks: Vec<Pick>,
    /// The highlighted row of `add_picks`; None until the user steps into the list.
    pub(crate) add_sel: Option<usize>,
    /// First visible row of `add_picks`, kept steady while stepping with arrows.
    pub(crate) add_top: std::cell::Cell<usize>,
    /// Text being selected (or just copied) in the terminal pane.
    pub(crate) sel: Option<Selection>,
    pub(crate) confirm_stop: Option<usize>,
    pub(crate) confirm_remove: Option<usize>,
    pub(crate) confirm_restart: Option<usize>,
    /// Right-click quick-action menu target, if open.
    pub(crate) context_menu: Option<usize>,
    pub(crate) context_sel: usize,
    pub(crate) confirm_quit: bool,
    pub(crate) confirm_start_all: bool,
    pub(crate) message: (String, Notice),
    /// Idle hint text, restored when a message expires.
    pub(crate) hints: String,
    /// Last message seen and when it first appeared, for expiry.
    pub(crate) msg_seen: String,
    pub(crate) msg_at: Instant,
    pub(crate) dirty: bool,
    /// `squad start <name>`: start and focus the first selection once.
    pub(crate) first: bool,
    pub(crate) cores: usize,
    pub(crate) sys: SysUsage,
    // Measuring walks the process table, so it runs on its own thread and
    // the UI only ever does a non-blocking `try_recv` for the result.
    pub(crate) req_tx: mpsc::Sender<Vec<(usize, u32)>>,
    pub(crate) res_rx: mpsc::Receiver<Measurement>,
    pub(crate) last_sample: Instant,
    pub(crate) sampling: bool,
    // Self-update: the check runs on a worker so it can never delay startup.
    pub(crate) upd_tx: mpsc::Sender<UpdateMsg>,
    pub(crate) upd_rx: mpsc::Receiver<UpdateMsg>,
    pub(crate) update_offer: Option<String>,
    pub(crate) confirm_update: bool,
    /// An installed update waiting for the decision to restart into it.
    pub(crate) applied: Option<String>,
    pub(crate) confirm_apply: bool,
    /// Restart squad into the new build once the manager has shut down.
    pub(crate) relaunch: bool,
    pub(crate) updating: bool,
    pub(crate) tick: u64,
    pub(crate) last_anim_tick: Instant,
    /// A background agent wants attention; `take_bell` decides if it rings.
    pub(crate) bell: bool,
    pub(crate) last_bell: Option<Instant>,
    /// Projects with `autostart` start once, on the first loop pass.
    pub(crate) autostart_pending: bool,
    /// Sessions to start again after an update restart (`squad resume`).
    pub(crate) resume: Vec<usize>,
    /// The SITREP reader (`h`), open over the manager.
    pub(crate) history: Option<History>,
    /// The memory monitor (`M`), open over the manager.
    pub(crate) procs: Option<ProcList>,
    /// Shared environment variable vault & `.env` assignment modal (`E` or Add form).
    pub(crate) env_vault: EnvVault,
    pub(crate) env_modal: Option<EnvModal>,
    /// Projects whose session is starting on a worker, each with the keys
    /// typed at it meanwhile (sent once the session is up).
    pub(crate) starting: HashMap<String, Vec<u8>>,
    pub(crate) start_tx: mpsc::Sender<Started>,
    pub(crate) start_rx: mpsc::Receiver<Started>,
    /// Rows and columns the sessions were last sized to.
    pub(crate) pane: (u16, u16),
    /// `z`: a focused terminal takes the sidebar's columns too.
    pub(crate) zoom: bool,
    pub(crate) hangul_hotkey_seen: bool,
    /// Arrow keys past the last project land on the Management rows:
    /// `0` = Memory monitor, `1` = Env vault. None while a project is picked.
    pub(crate) mgmt_sel: Option<usize>,
    /// Read-only environment popup opened from the right-click menu.
    pub(crate) env_info: Option<EnvInfo>,
    pub(crate) quit: bool,
}

impl App {
    pub(crate) fn new(cfg: Config, initial: Option<usize>) -> Self {
        ledger::write_rosters(&cfg.projects);
        let sessions = (0..cfg.projects.len()).map(|_| None).collect();
        let rank_prefixes = compute_rank_prefixes(&cfg.projects);
        let order = display_order(&cfg.projects);
        let env_vault = envvault::load_env().unwrap_or_default();
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
        let (start_tx, start_rx) = mpsc::channel::<Started>();
        let (upd_tx, upd_rx) = mpsc::channel::<UpdateMsg>();
        if update_check_enabled() {
            let tx = upd_tx.clone();
            thread::spawn(move || watch_for_updates(tx));
        }
        Self {
            cfg,
            sessions,
            order,
            rank_prefixes,
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
            add_env: BTreeSet::new(),
            add_edit: None,
            add_err: String::new(),
            add_state: PathState::Blank,
            add_picks: Vec::new(),
            add_sel: None,
            add_top: Default::default(),
            sel: None,
            confirm_stop: None,
            confirm_remove: None,
            confirm_restart: None,
            context_menu: None,
            context_sel: 0,
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
            procs: None,
            env_vault,
            env_modal: None,
            starting: HashMap::new(),
            start_tx,
            start_rx,
            pane: (0, 0),
            zoom: false,
            hangul_hotkey_seen: false,
            mgmt_sel: None,
            env_info: None,
            quit: false,
        }
    }

    /// True when the active UI state only accepts ASCII/navigation shortcuts
    /// (no Korean prose composition needed), so the OS IME can temporarily
    /// switch to English to avoid holding consonants in a preedit box.
    pub(crate) fn wants_ascii_ime(&self) -> bool {
        if self.env_modal.as_ref().is_some_and(|m| m.editor.is_some()) {
            return false;
        }
        if self.env_modal.is_some()
            || self.history.is_some()
            || self.procs.is_some()
            || self.context_menu.is_some()
            || self.env_info.is_some()
            || self.confirm_quit
            || self.confirm_start_all
            || self.confirm_stop.is_some()
            || self.confirm_remove.is_some()
            || self.confirm_restart.is_some()
            || self.confirm_update
            || self.confirm_apply
        {
            return true;
        }
        match self.mode {
            Mode::Manager | Mode::Help => true,
            Mode::Add => matches!(self.add_field, 1 | 3 | 5),
            Mode::Focus | Mode::Send | Mode::Filter => false,
        }
    }

    /// Columns the sidebar takes of a frame `area_w` wide.
    fn side_w(&self, area_w: u16) -> u16 {
        if self.zoom && self.mode == Mode::Focus {
            0
        } else {
            sidebar_width(area_w)
        }
    }

    /// Rows and columns left for a session's terminal inside the frame.
    fn pane_size(&self, a: Size) -> (u16, u16) {
        (
            a.height.saturating_sub(3).max(1),
            a.width
                .saturating_sub(self.side_w(a.width))
                .saturating_sub(2)
                .max(1),
        )
    }

    /// Is project `pi`'s session being started right now?
    fn is_starting(&self, pi: usize) -> bool {
        self.cfg
            .projects
            .get(pi)
            .is_some_and(|p| self.starting.contains_key(&p.name))
    }

    /// A start cannot be stopped or restarted half way: say so instead of
    /// going quiet. True when `pi` is starting.
    fn say_if_starting(&mut self, pi: usize) -> bool {
        let starting = self.is_starting(pi);
        if starting {
            self.message = (
                format!("{} is still starting.", self.cfg.projects[pi].name),
                Notice::Info,
            );
        }
        starting
    }

    /// Start project `pi` on a worker: opening a PTY takes about 150 ms, too
    /// long for the thread that reads keys. `finish_start` files the result.
    /// A session already in the slot is stopped first, on the same worker, so
    /// a restarted server finds its port free. `input` is typed into the new
    /// session.
    fn begin_start(&mut self, pi: usize, rows: u16, cols: u16, input: Vec<u8>) {
        let Some(p) = self.cfg.projects.get(pi).cloned() else {
            return;
        };
        if let Some(typed) = self.starting.get_mut(&p.name) {
            typed.extend(input);
            return;
        }
        let old = self.sessions.get_mut(pi).and_then(Option::take);
        self.starting.insert(p.name.clone(), input);
        let tx = self.start_tx.clone();
        thread::spawn(move || {
            drop(old);
            // A panic must still answer, or the project would read "Starting" for good.
            let spawned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                Session::spawn(&p, rows, cols)
            }))
            .unwrap_or_else(|_| Err(anyhow!("Starting {} panicked", p.name)));
            let _ = tx.send((p.name, spawned));
        });
        self.dirty = true;
    }

    /// A start begun by `begin_start` came back.
    fn finish_start(&mut self, (name, spawned): Started) {
        let typed = self.starting.remove(&name).unwrap_or_default();
        self.dirty = true;
        let pi = self.cfg.projects.iter().position(|p| p.name == name);
        match (pi, spawned) {
            (Some(pi), Ok(mut s)) => {
                // The window may have been resized (or zoomed) meanwhile.
                let (rows, cols) = s.parser.screen().size();
                if (rows.get(), cols.get()) != self.pane && self.pane != (0, 0) {
                    let _ = s.resize(self.pane.0, self.pane.1);
                }
                if !typed.is_empty() {
                    if let Err(e) = s.input(&typed) {
                        self.message = (e.to_string(), Notice::Error);
                    }
                }
                if let Some(old) = self.sessions.get_mut(pi).and_then(|o| o.replace(s)) {
                    stop_in_background(old);
                }
            }
            (Some(pi), Err(e)) => {
                self.message = (format!("{e:#}"), Notice::Error);
                // Nothing to type into: hand the keyboard back.
                if self.mode == Mode::Focus && self.order.get(self.selected) == Some(&pi) {
                    self.mode = Mode::Manager;
                }
            }
            // Removed or renamed while it started: no row is left to show it.
            (None, Ok(s)) => stop_in_background(s),
            (None, Err(_)) => {}
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

    /// Spinner clock; redraws at 200 ms only while spinners move (starting,
    /// running services, or working agents). Static running sessions refresh
    /// once per second for uptime/idle labels, saving idle CPU and frame copies.
    fn animate(&mut self) {
        if self.last_anim_tick.elapsed() < Duration::from_millis(200) {
            return;
        }
        self.last_anim_tick = Instant::now();
        self.tick = self.tick.wrapping_add(1);
        if self.has_active_spinner()
            || (self.tick.is_multiple_of(5)
                && self
                    .sessions
                    .iter()
                    .flatten()
                    .any(|s| s.status.is_running()))
        {
            self.dirty = true;
        }
    }

    fn has_active_spinner(&self) -> bool {
        if !self.starting.is_empty() {
            return true;
        }
        self.sessions.iter().enumerate().any(|(pi, slot)| {
            slot.as_ref().is_some_and(|s| {
                s.status.is_running()
                    && (self
                        .cfg
                        .projects
                        .get(pi)
                        .is_some_and(|p| p.kind == Kind::Service)
                        || s.last_phase == AgentPhase::Working)
            })
        })
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
                for &pi in &pis {
                    self.begin_start(pi, rows, cols, Vec::new());
                }
                self.message = (
                    format!("Autostarting {}.", plural_sessions(pis.len())),
                    Notice::Info,
                );
            }
        }
        if self.first {
            self.first = false;
            if let Some(&pi) = self.order.get(self.selected) {
                self.open_session(pi, rows, cols);
            }
        }
    }

    /// Start project `pi` unless it is running and hand it the keyboard at
    /// once: keys typed while it starts are kept for it. A failed start says
    /// why and returns to the manager (`finish_start`).
    fn open_session(&mut self, pi: usize, rows: u16, cols: u16) {
        if !slot_running(&self.sessions, pi) {
            self.begin_start(pi, rows, cols, Vec::new());
        }
        self.mode = Mode::Focus;
    }

    /// Replace project `pi`'s session with a fresh one; a failure is
    /// reported when it comes back.
    fn restart(&mut self, pi: usize, rows: u16, cols: u16) {
        if self.say_if_starting(pi) {
            return;
        }
        if let (Some(p), Some(Some(s))) = (self.cfg.projects.get(pi), self.sessions.get_mut(pi)) {
            if p.kind == Kind::Agent && s.status.is_running() {
                file_history(s, p, "restarted by user".into());
            }
        }
        if let Some(p) = self.cfg.projects.get(pi) {
            self.message = (format!("Restarting {}", p.name), Notice::Info);
        }
        self.begin_start(pi, rows, cols, Vec::new());
    }

    /// Drop any active filter while keeping the currently selected project selected.
    pub(crate) fn clear_filter(&mut self) {
        let sel_pi = self.order.get(self.selected).copied();
        self.filter_buf.clear();
        self.refresh_prefixes();
        self.order = display_order(&self.cfg.projects);
        self.selected = sel_pi
            .and_then(|pi| self.order.iter().position(|&x| x == pi))
            .unwrap_or(0);
    }

    pub(crate) fn refresh_prefixes(&mut self) {
        self.rank_prefixes = compute_rank_prefixes(&self.cfg.projects);
    }

    pub(crate) fn open_env_info(&mut self, pi: usize) {
        if let Some(p) = self.cfg.projects.get(pi) {
            let dotenv = envvault::read_dotenv(&p.path);
            self.env_info = Some(EnvInfo {
                pi,
                reveal: false,
                dotenv,
            });
        }
    }

    pub(crate) fn open_env_modal(&mut self, target: EnvTarget) {
        if let Ok(fresh) = envvault::load_env() {
            self.env_vault = fresh;
        }
        let (picked, project_dotenv) = match target {
            EnvTarget::AddForm => (self.add_env.clone(), BTreeMap::new()),
            EnvTarget::Project(pi) => {
                let dotenv = self
                    .cfg
                    .projects
                    .get(pi)
                    .map(|p| envvault::read_dotenv(&p.path))
                    .unwrap_or_default();
                // Checked = assigned: start from the vault keys this .env
                // already has, so unchecking one removes it on Enter.
                let picked = self
                    .env_vault
                    .vars
                    .iter()
                    .filter(|e| dotenv.contains_key(&e.key))
                    .map(|e| e.key.clone())
                    .collect();
                (picked, dotenv)
            }
            EnvTarget::VaultOnly => (BTreeSet::new(), BTreeMap::new()),
        };
        let editor = self.env_vault.vars.is_empty().then(EnvEditor::blank);
        self.env_modal = Some(EnvModal {
            target,
            sel: 0,
            top: Default::default(),
            picked,
            project_dotenv,
            reveal: false,
            confirm_delete: None,
            editor,
        });
    }

    /// Jump to the next project needing attention (`?`, failed/errored, or
    /// unseen finished task), falling back to the next running session.
    pub(crate) fn jump_attention(&mut self) {
        let n = self.order.len();
        if n <= 1 {
            return;
        }
        let needs_att = |pi: usize| {
            self.sessions
                .get(pi)
                .and_then(|o| o.as_ref())
                .is_some_and(|s| {
                    s.status.needs_attention()
                        || (s.status.is_running()
                            && matches!(
                                (s.phase(), s.acked),
                                (AgentPhase::NeedsInput, _) | (AgentPhase::Done, false)
                            ))
                })
        };
        let is_up = |pi: usize| slot_running(&self.sessions, pi);
        let next_matching = |pred: &dyn Fn(usize) -> bool| -> Option<usize> {
            (1..=n)
                .map(|off| (self.selected + off) % n)
                .find(|&pos| pred(self.order[pos]))
        };
        if let Some(pos) = next_matching(&needs_att).or_else(|| next_matching(&is_up)) {
            self.selected = pos;
        }
    }

    /// File work history if an agent session was mid-task, then stop it off-thread.
    pub(crate) fn stop_slot(&mut self, pi: usize, reason: &str) {
        if let Some(slot) = self.sessions.get_mut(pi) {
            if let Some(mut old) = slot.take() {
                if let Some(p) = self.cfg.projects.get(pi) {
                    if p.kind == Kind::Agent && old.status.is_running() {
                        file_history(&mut old, p, reason.to_string());
                    }
                }
                stop_in_background(old);
            }
        }
    }

    /// The pane changed size (window resize, zoom): tell every session.
    fn resize_sessions(&mut self, rows: u16, cols: u16) {
        self.sel = None;
        self.pane = (rows, cols);
        for s in self.sessions.iter_mut().flatten() {
            if let Err(e) = s.resize(rows, cols) {
                self.message = (e.to_string(), Notice::Error);
            }
        }
        self.dirty = true;
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
    pub(crate) fn pump(&mut self) {
        self.expire_message();
        while let Ok(started) = self.start_rx.try_recv() {
            self.finish_start(started);
        }
        while let Ok(m) = self.res_rx.try_recv() {
            self.sampling = false;
            if let Some(snap) = m.sys {
                self.sys.apply(snap);
            }
            for (pi, pid, cpu_time, mem) in m.procs {
                if let Some(Some(s)) = self.sessions.get_mut(pi) {
                    s.apply_usage(pid, cpu_time, mem, m.at, self.cores);
                    let limit = self.cfg.mem_limit_bytes();
                    if let Some(u) = s.usage {
                        if over_mem_limit(u, limit) && !s.mem_warned {
                            s.mem_warned = true;
                            self.message = (
                                format!(
                                    "{} uses {} (limit {}): restart it if it leaks",
                                    self.cfg.projects[pi].name,
                                    mem_label(u.mem),
                                    mem_label(limit)
                                ),
                                Notice::Warn,
                            );
                        } else if u.mem < limit / 10 * 9 {
                            s.mem_warned = false; // re-arm once it drops well below
                        }
                    }
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
            || self.env_modal.is_some()
            || self.history.is_some()
            || self.procs.is_some()
            || self.context_menu.is_some()
            || self.env_info.is_some()
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
                self.dirty |= phase_now != phase_before
                    || s.status.is_running() != was_running
                    || (changed && selected);
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
                    }
                    // The work history: squad files it, whatever the agent does.
                    if phase_now == AgentPhase::Working {
                        s.work_started.get_or_insert_with(SystemTime::now);
                    } else if matches!(phase_now, AgentPhase::Done | AgentPhase::NeedsInput)
                        || (phase_now == AgentPhase::Idle && s.last_phase == AgentPhase::Working)
                    {
                        // A task ends when the agent finishes, stops to ask, or returns to idle.
                        let how = match phase_now {
                            AgentPhase::Done => "task complete",
                            AgentPhase::NeedsInput => "waiting for your input",
                            _ => "task complete",
                        };
                        file_history(s, &self.cfg.projects[pi], how.into());
                    }
                    s.last_phase = phase_now;
                }
                let label = s.status.label();
                if was_running
                    && !s.status.is_running()
                    && self.cfg.projects[pi].kind == Kind::Agent
                {
                    file_history(
                        s,
                        &self.cfg.projects[pi],
                        format!("session ended ({label})"),
                    );
                }
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
    /// instant; 40 ms while spinners run; 150-200 ms when sessions are static
    /// or stopped so idle CPU stays near zero.
    fn poll_duration(&self) -> Duration {
        // Only the session on screen needs instant echo; a chatty background
        // session (a dev server logging) shows as a spinner, which the 40 ms
        // branch below already covers.
        let recently_active = self
            .order
            .get(self.selected)
            .and_then(|&pi| self.sessions.get(pi))
            .and_then(Option::as_ref)
            .is_some_and(|s| s.is_active(Duration::from_millis(1500)));
        if recently_active
            || self.sampling
            || self.updating
            || self.dirty
            || !self.starting.is_empty()
        {
            Duration::from_millis(10)
        } else if self.has_active_spinner() {
            Duration::from_millis(40)
        } else if self.message.0 != self.hints
            || self
                .sessions
                .iter()
                .flatten()
                .any(|s| s.status.is_running())
        {
            Duration::from_millis(150)
        } else {
            Duration::from_millis(200)
        }
    }

    /// `e`: the add form, filled with the selected project. The path, command
    /// and autostart can change here (a new path renames an agent after its
    /// folder); kind and chain of command keep their own keys (`u`).
    pub(crate) fn open_edit(&mut self) {
        let Some(&pi) = self.order.get(self.selected) else {
            return;
        };
        let p = self.cfg.projects[pi].clone();
        self.open_add();
        self.add_edit = Some(p.name);
        self.add_path.set(&p.path.display().to_string());
        self.add_kind = p.kind;
        if p.command != shell() {
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
        self.add_env.clear();
        self.add_err.clear();
        self.add_state = PathState::Blank;
        self.add_top.set(0);
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

fn compute_rank_prefixes(projects: &[Project]) -> Vec<String> {
    (0..projects.len())
        .map(|i| rank_prefix(projects, i))
        .collect()
}

/// Tab-complete an agent name for "Reports to" (`u` or the add form).
/// When `for_pi` is `Some(pi)`, candidates that would create a reporting loop
/// are excluded. Exact matches cycle to the next valid candidate so repeated
/// `Tab` steps through commanders.
pub(crate) fn complete_agent_name(
    projects: &[Project],
    typed: &str,
    for_pi: Option<usize>,
) -> Option<String> {
    let prefix = typed.trim();
    let lower = prefix.to_ascii_lowercase();
    let valid_for = |candidate: &str| match for_pi {
        Some(pi) => check_reassign(projects, &projects[pi].name, candidate).is_ok(),
        None => true,
    };
    let all_valid: Vec<&str> = projects
        .iter()
        .filter(|p| p.kind == Kind::Agent && valid_for(&p.name))
        .map(|p| p.name.as_str())
        .collect();
    if all_valid.is_empty() {
        return None;
    }
    if let Some(pos) = all_valid
        .iter()
        .position(|n| n.eq_ignore_ascii_case(prefix))
    {
        return Some(all_valid[(pos + 1) % all_valid.len()].to_string());
    }
    let matches: Vec<&str> = all_valid
        .into_iter()
        .filter(|n| n.to_ascii_lowercase().starts_with(&lower))
        .collect();
    if matches.is_empty() {
        return None;
    }
    if matches.len() == 1 {
        return Some(matches[0].to_string());
    }
    let first: Vec<char> = matches[0].chars().collect();
    let shared: String = (0..first.len())
        .take_while(|&i| {
            matches.iter().all(|n| {
                n.chars()
                    .nth(i)
                    .is_some_and(|c| c.eq_ignore_ascii_case(&first[i]))
            })
        })
        .map(|i| first[i])
        .collect();
    if shared.len() > prefix.len() {
        Some(shared)
    } else {
        Some(matches[0].to_string())
    }
}

/// Run the manager; when an update was installed and the user chose to
/// restart, start the new build in this console once the screen is restored.
/// File the work history entry for the task `s` just finished, or was cut off
/// in. Nothing when no task was running or it was only a blip.
fn file_history(s: &mut Session, p: &Project, outcome: String) {
    let Some(started) = s.work_started.take() else {
        return;
    };
    let took = started.elapsed().unwrap_or_default();
    if took < history::MIN_TASK {
        return;
    }
    history::record(history::Task {
        project: p.name.clone(),
        path: p.path.clone(),
        command: join_command(&p.command),
        started,
        took,
        outcome,
        tail: s.tail_text(20),
    });
}

/// A project's SITREPs open for reading: the list, the picked one's text and
/// how far it is scrolled.
pub(crate) struct History {
    project: String,
    items: Vec<(String, std::path::PathBuf)>,
    sel: usize,
    top: std::cell::Cell<usize>,
    scroll: u16,
    text: String,
    /// Columns the text is wrapped to, as last drawn.
    wrap_w: std::cell::Cell<u16>,
    /// Seconds local time is ahead of UTC, for the list's time labels.
    offset: i64,
}
impl History {
    fn open(project: &str, items: Vec<(String, std::path::PathBuf)>) -> Self {
        let mut h = Self {
            project: project.to_string(),
            items,
            sel: 0,
            top: Default::default(),
            scroll: 0,
            text: String::new(),
            wrap_w: std::cell::Cell::new(u16::MAX),
            offset: utc_offset_secs(),
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
    /// Screen rows the text takes once wrapped. Long lines (an agent's last
    /// output) wrap, and counting them as one row left the end out of reach.
    // ponytail: counts column wrapping; word wrap can add a row or two, well
    // inside the page that is still on screen at the last scroll position.
    fn rows(&self) -> usize {
        let w = usize::from(self.wrap_w.get()).max(1);
        self.text.lines().map(|l| width(l).div_ceil(w).max(1)).sum()
    }
    fn max_scroll(&self) -> u16 {
        self.rows().saturating_sub(1).min(u16::MAX as usize) as u16
    }
    fn scroll_by(&mut self, delta: isize) {
        let next = (self.scroll as isize + delta).clamp(0, self.max_scroll() as isize);
        self.scroll = next as u16;
    }
}

pub fn run(c: Config, initial: Option<usize>, resume: Vec<usize>) -> Result<()> {
    if let Some(args) = run_ui(c, initial, resume)? {
        let exe = install_path()
            .with_context(|| format!("Cannot find {} to restart", crate::updater::BIN))?;
        // Wait for it: the new squad owns this console until it exits.
        let status = std::process::Command::new(exe).args(args).status()?;
        std::process::exit(status.code().unwrap_or(0));
    }
    Ok(())
}

fn run_ui(c: Config, initial: Option<usize>, resume: Vec<usize>) -> Result<Option<Vec<String>>> {
    let _ = install_path(); // remember the path before an update renames the file
                            // Every agent project keeps a `.squad/`; add it where it is missing.
    c.projects.iter().for_each(ledger::ensure_agent);
    if !io::stdin().is_terminal() {
        bail!("squad needs an interactive terminal");
    }
    enable_raw_mode()?;
    if let Some(lines) = c.scrollback {
        set_scrollback_limit(lines);
    }
    install_panic_hook();
    let _guard = ScreenGuard(console_title());
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        crossterm::cursor::Hide
    )?;
    // A multi-line paste then arrives whole instead of as one Enter per line.
    // Unix only: the Windows console API has no paste event, and asking for
    // one there would put the raw markers into the key stream.
    #[cfg(unix)]
    execute!(io::stdout(), event::EnableBracketedPaste)?;
    let stdout = io::BufWriter::new(io::stdout());
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    let mut app = App::new(c, initial);
    app.resume = resume;
    let mut ime = crate::ime::ImeGuard::new();
    let mut title = String::new();
    loop {
        let area = terminal.size()?;
        // One resize per pass, however many resize events a window drag sent.
        let (rows, cols) = app.pane_size(area);
        if (rows, cols) != app.pane {
            app.resize_sessions(rows, cols);
        }
        app.animate();
        app.start_initial(rows, cols);
        app.pump();
        ime.sync(
            app.wants_ascii_ime(),
            std::mem::take(&mut app.hangul_hotkey_seen),
        );
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
        let batch = Instant::now();
        loop {
            match event::read()? {
                // The next pass sees the new size and resizes the sessions once.
                Event::Resize(_, _) => app.dirty = true,
                Event::Paste(data) => app.on_paste(data),
                Event::Mouse(m) => app.on_mouse(m, area, rows, cols),
                Event::Key(k) if k.kind != KeyEventKind::Release => app.on_key(k, rows, cols),
                _ => {}
            }
            if app.quit || batch.elapsed() >= EVENT_BUDGET || !event::poll(Duration::ZERO)? {
                break;
            }
        }
        ime.sync(
            app.wants_ascii_ime(),
            std::mem::take(&mut app.hangul_hotkey_seen),
        );
        if app.quit {
            break;
        }
    }
    // Starts still in flight join the rest, so shutdown stops them too.
    let end = Instant::now() + Duration::from_secs(2);
    while !app.starting.is_empty() {
        match app
            .start_rx
            .recv_timeout(end.saturating_duration_since(Instant::now()))
        {
            Ok(started) => app.finish_start(started),
            Err(_) => break,
        }
    }
    let relaunch = app.relaunch.then(|| app.relaunch_args());
    app.remember_selection();
    // Quitting mid-task ends the task too: file it before the screen is gone.
    for (pi, slot) in app.sessions.iter_mut().enumerate() {
        if let Some(s) = slot.as_mut().filter(|s| s.status.is_running()) {
            file_history(s, &app.cfg.projects[pi], "squad closed mid-task".into());
        }
    }
    shutdown_all(&mut app.sessions);
    wait_for_stops(Duration::from_secs(2));
    history::flush(Duration::from_secs(3));
    Ok(relaunch)
}
