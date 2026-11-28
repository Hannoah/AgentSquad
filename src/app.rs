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
use crate::models::{self, ModelVault, Provider};
use crate::pathpick::{self, PathState, Pick};
use crate::refs::{self, ProjectReferences, ReferenceVault};
use crate::select::Selection;
use crate::session::*;
use crate::skills::{self, ProjectSkills, SkillVault};
use crate::sys::*;
use crate::tools::{self, ProjectTools, ToolKind, ToolVault};
use crate::ui::*;
use crate::updater::*;

pub(crate) mod actions;
pub(crate) mod draw;
pub(crate) mod keys;
pub(crate) mod mouse;
pub(crate) mod vault;
pub(crate) use vault::*;

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

/// Crash-log size cap: a crash loop must not fill the disk.
pub(crate) const CRASH_LOG_MAX_BYTES: u64 = 1_000_000;
/// True when the crash log at `len` bytes should start over.
pub(crate) fn crash_log_needs_rotation(len: u64) -> bool {
    len > CRASH_LOG_MAX_BYTES
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
            let log = dir.join("crash.log");
            // A crash loop must not fill the disk: start over past the cap.
            if crash_log_needs_rotation(std::fs::metadata(&log).map(|m| m.len()).unwrap_or(0)) {
                let _ = std::fs::remove_file(&log);
            }
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log)
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

/// Remote quick-reply popup for an agent waiting on user input (`?`).
pub(crate) struct QuickReply {
    pub(crate) pi: usize,
    pub(crate) tail: Vec<String>,
    pub(crate) typing: bool,
    pub(crate) input: LineInput,
}

/// Live Git status & diff viewer (`g`) for one project.
pub(crate) struct GitView {
    pub(crate) pi: usize,
    pub(crate) project: String,
    pub(crate) branch: String,
    pub(crate) files: Vec<(String, std::path::PathBuf)>,
    pub(crate) sel: usize,
    pub(crate) top: std::cell::Cell<usize>,
    pub(crate) scroll: u16,
    pub(crate) diff: String,
    pub(crate) wrap_w: std::cell::Cell<u16>,
    /// The file list is being read; `branch`/`files` are the previous ones.
    pub(crate) refreshing: bool,
    /// The picked file's diff is being read; `diff` is the previous one.
    pub(crate) loading_diff: bool,
    status_req: u64,
    diff_req: u64,
    tx: mpsc::Sender<GitReq>,
    rx: mpsc::Receiver<GitReply>,
}

type GitFiles = Vec<(String, std::path::PathBuf)>;
/// A diff and the changed file it shows (None: the tree is clean).
type GitDiff = (Option<std::path::PathBuf>, String);

/// Work for a git viewer's worker. Each carries its request number, so a
/// reply that a newer request overtook is dropped.
enum GitReq {
    /// Branch and changed files; `want` (else row `sel`) is diffed too.
    Status {
        req: u64,
        want: Option<std::path::PathBuf>,
        sel: usize,
    },
    Diff {
        req: u64,
        entry: Option<(String, std::path::PathBuf)>,
    },
}
enum GitReply {
    Status {
        req: u64,
        branch: String,
        files: GitFiles,
        diff: GitDiff,
    },
    Diff {
        req: u64,
        diff: GitDiff,
    },
}

/// Runs git for one viewer, off the UI thread: `git status -uall` and
/// `git diff` take seconds in a big repository. Ends when the viewer closes.
fn git_worker(path: std::path::PathBuf, rx: mpsc::Receiver<GitReq>, tx: mpsc::Sender<GitReply>) {
    let diff_of = |entry: Option<&(String, std::path::PathBuf)>| -> GitDiff {
        entry.map_or(
            (None, "Working tree clean.".to_string()),
            |(status, rel)| (Some(rel.clone()), ledger::file_diff(&path, status, rel)),
        )
    };
    while let Ok(first) = rx.recv() {
        // Holding j/k queues a request per row: only the newest of each kind matters.
        let (mut status, mut diff) = (None, None);
        for r in std::iter::once(first).chain(rx.try_iter()) {
            if matches!(r, GitReq::Status { .. }) {
                status = Some(r);
            } else {
                diff = Some(r);
            }
        }
        if let Some(GitReq::Status { req, want, sel }) = status {
            let branch = ledger::repo_branch(&path).unwrap_or_else(|| "HEAD".to_string());
            let files = ledger::status_entries(&path).unwrap_or_default();
            let at = want
                .and_then(|w| files.iter().position(|(_, f)| *f == w))
                .unwrap_or_else(|| sel.min(files.len().saturating_sub(1)));
            let diff = diff_of(files.get(at));
            let reply = GitReply::Status {
                req,
                branch,
                files,
                diff,
            };
            if tx.send(reply).is_err() {
                return;
            }
        }
        if let Some(GitReq::Diff { req, entry }) = diff {
            let diff = diff_of(entry.as_ref());
            if tx.send(GitReply::Diff { req, diff }).is_err() {
                return;
            }
        }
    }
}

impl GitView {
    pub(crate) fn open(pi: usize, project: &str, path: &std::path::Path, branch: String) -> Self {
        let (tx, worker_rx) = mpsc::channel();
        let (worker_tx, rx) = mpsc::channel();
        let path = path.to_path_buf();
        thread::spawn(move || git_worker(path, worker_rx, worker_tx));
        let mut g = Self {
            pi,
            project: project.to_string(),
            branch,
            files: Vec::new(),
            sel: 0,
            top: Default::default(),
            scroll: 0,
            diff: String::new(),
            wrap_w: std::cell::Cell::new(u16::MAX),
            refreshing: false,
            loading_diff: false,
            status_req: 0,
            diff_req: 0,
            tx,
            rx,
        };
        g.refresh();
        g
    }

    fn current_path(&self) -> Option<std::path::PathBuf> {
        self.files.get(self.sel).map(|(_, p)| p.clone())
    }

    /// Re-read the branch and changed files (and the picked file's diff).
    pub(crate) fn refresh(&mut self) {
        self.status_req += 1;
        self.refreshing = true;
        let _ = self.tx.send(GitReq::Status {
            req: self.status_req,
            want: self.current_path(),
            sel: self.sel,
        });
    }

    /// Ask for the picked file's diff; the old text stays until it arrives,
    /// so stepping through files does not flash.
    pub(crate) fn load(&mut self) {
        self.scroll = 0;
        self.diff_req += 1;
        self.loading_diff = true;
        let _ = self.tx.send(GitReq::Diff {
            req: self.diff_req,
            entry: self.files.get(self.sel).cloned(),
        });
    }

    /// A reply is still on its way.
    pub(crate) fn busy(&self) -> bool {
        self.refreshing || self.loading_diff
    }

    /// Take the worker's replies. True when something on screen changed.
    pub(crate) fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(reply) = self.rx.try_recv() {
            match reply {
                GitReply::Status {
                    req,
                    branch,
                    files,
                    diff,
                } if req == self.status_req => {
                    let keep = self.current_path();
                    self.branch = branch;
                    self.files = files;
                    self.sel = keep
                        .and_then(|p| self.files.iter().position(|(_, f)| *f == p))
                        .unwrap_or_else(|| self.sel.min(self.files.len().saturating_sub(1)));
                    self.refreshing = false;
                    if diff.0 == self.current_path() {
                        // Newer than any diff still in flight for an older pick.
                        self.diff_req += 1;
                        self.loading_diff = false;
                        self.diff = diff.1;
                        self.scroll = 0;
                    } else {
                        self.load();
                    }
                    changed = true;
                }
                GitReply::Diff { req, diff } if req == self.diff_req => {
                    self.loading_diff = false;
                    self.diff = diff.1;
                    self.scroll = 0;
                    changed = true;
                }
                _ => {}
            }
        }
        changed
    }

    pub(crate) fn select(&mut self, i: usize) {
        let i = i.min(self.files.len().saturating_sub(1));
        if i != self.sel {
            self.sel = i;
            self.load();
        }
    }

    pub(crate) fn rows(&self) -> usize {
        wrapped_rows(&self.diff, self.wrap_w.get())
    }

    pub(crate) fn max_scroll(&self) -> u16 {
        self.rows().saturating_sub(1).min(u16::MAX as usize) as u16
    }

    pub(crate) fn scroll_by(&mut self, delta: isize) {
        let next = (self.scroll as isize + delta).clamp(0, self.max_scroll() as isize);
        self.scroll = next as u16;
    }
}

pub(crate) struct AddForm {
    pub(crate) field: usize,
    pub(crate) path: LineInput,
    /// Server row of the form: `0` is this PC, `i` is `servers_vault.vars[i - 1]`.
    pub(crate) server: usize,
    pub(crate) kind: Kind,
    pub(crate) cmd: LineInput,
    pub(crate) under: LineInput,
    pub(crate) autostart: bool,
    /// Shared env keys selected in Add/Edit mode to apply to `.env` on save.
    pub(crate) env: BTreeSet<String>,
    /// Shared skills selected in Add/Edit mode to install on save.
    pub(crate) skills: BTreeSet<String>,
    /// Shared tools selected in Add/Edit mode to install on save.
    pub(crate) tools: BTreeSet<String>,
    /// Shared references selected in Add/Edit mode to sync on save.
    pub(crate) refs: BTreeSet<String>,
    /// Name of the project the form is editing; None when it adds one.
    pub(crate) edit: Option<String>,
    pub(crate) err: String,
    /// What the Path field points at (checked on edits, not per frame).
    pub(crate) state: PathState,
    /// The folders listed under the Path field: the parents your projects live in
    /// while it is empty, otherwise the subfolders matching what is typed.
    pub(crate) picks: Vec<Pick>,
    /// The highlighted row of `picks`; None until the user steps into the list.
    pub(crate) sel: Option<usize>,
    /// First visible row of `picks`, kept steady while stepping with arrows.
    pub(crate) top: std::cell::Cell<usize>,
}

impl AddForm {
    pub(crate) fn new() -> Self {
        Self {
            field: 0,
            path: LineInput::new(),
            server: 0,
            kind: Kind::Agent,
            cmd: LineInput::new(),
            under: LineInput::new(),
            autostart: false,
            env: BTreeSet::new(),
            skills: BTreeSet::new(),
            tools: BTreeSet::new(),
            refs: BTreeSet::new(),
            edit: None,
            err: String::new(),
            state: PathState::Blank,
            picks: Vec::new(),
            sel: None,
            top: Default::default(),
        }
    }
}

/// Terminal scrollback search state (`F` / `Ctrl+F`).
pub(crate) struct SearchState {
    pub(crate) buf: LineInput,
    pub(crate) pi: Option<usize>,
    pub(crate) matches: Vec<SearchMatch>,
    pub(crate) idx: usize,
    pub(crate) active: bool,
}

impl SearchState {
    pub(crate) fn new() -> Self {
        Self {
            buf: LineInput::new(),
            pi: None,
            matches: Vec::new(),
            idx: 0,
            active: false,
        }
    }
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
    pub(crate) add: AddForm,
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
    /// Model tests (`t` in Management O): `(name, reply or error)`.
    pub(crate) model_tx: mpsc::Sender<(String, Result<String, String>)>,
    pub(crate) model_rx: mpsc::Receiver<(String, Result<String, String>)>,
    /// Off-thread `git status` checks for non-hooked agent task completions: project name.
    pub(crate) nudge_tx: mpsc::Sender<String>,
    pub(crate) nudge_rx: mpsc::Receiver<String>,
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
    /// Global search over all sessions (`G`), open over the manager.
    pub(crate) gsearch: Option<GSearch>,
    /// The memory monitor (`M`), open over the manager.
    pub(crate) procs: Option<ProcList>,
    /// Shared vaults & active unified vault modal (`E`/`K`/`T`/`R` or Add form).
    pub(crate) env_vault: EnvVault,
    pub(crate) skills_vault: SkillVault,
    pub(crate) tools_vault: ToolVault,
    pub(crate) refs_vault: ReferenceVault,
    pub(crate) models_vault: ModelVault,
    /// Registered servers (`V`): name -> ssh address.
    pub(crate) servers_vault: EnvVault,
    pub(crate) vault_modal: Option<VaultModal>,
    /// Projects whose session is starting on a worker, each with the keys
    /// typed at it meanwhile (sent once the session is up).
    pub(crate) starting: HashMap<String, Vec<u8>>,
    /// Crash-loop tracking for service auto-restart: project name to
    /// (consecutive auto-restarts, last schedule). Keyed by name like
    /// `starting`, so removing a project cannot shift a streak onto another.
    /// Entries vanish once a session runs healthy long enough, stops cleanly,
    /// is stopped, or its project is gone.
    pub(crate) auto_restarts: HashMap<String, (u8, Instant)>,
    pub(crate) start_tx: mpsc::Sender<Started>,
    pub(crate) start_rx: mpsc::Receiver<Started>,
    /// Rows and columns the sessions were last sized to.
    pub(crate) pane: (u16, u16),
    /// `z`: a focused terminal takes the sidebar's columns too.
    pub(crate) zoom: bool,
    /// `[` / `]` or `[<]` / `[>]`: collapse the sidebar into a compact mini-rail.
    pub(crate) sidebar_collapsed: bool,
    /// Row target hovered in the collapsed mini-sidebar (`order` index or a Management row).
    pub(crate) hover_row: Option<usize>,
    /// Status-bar rect of the yellow `? N` waiting indicator, for click-to-reply.
    pub(crate) wait_badge_rect: Rect,
    /// Remote quick-reply popup for a waiting (`?`) agent.
    pub(crate) quick_reply: Option<QuickReply>,
    /// Live Git status & diff popup (`g`).
    pub(crate) git_view: Option<GitView>,
    /// Terminal scrollback search state (`F` / `Ctrl+F`).
    pub(crate) search: SearchState,
    pub(crate) hangul_hotkey_seen: bool,
    /// Arrow keys past the last project land on the Management rows:
    /// `0` = Memory monitor, `1` = Environment Variable, `2` = Skills, `3` = Tools,
    /// `4` = References, `5` = Lessons, `6` = LLM models, `7` = Servers.
    pub(crate) mgmt_sel: Option<usize>,
    /// Multi-digit row jump: digits typed within ~1.5s accumulate here.
    pub(crate) jump_buf: String,
    pub(crate) jump_at: Instant,
    pub(crate) quit: bool,
}

/// Crash-loop cap for service auto-restart: give up after this many tries.
pub(crate) const AUTO_RESTART_MAX_TRIES: u8 = 5;
/// Delay between a crash and its auto-restart, so a flapping service
/// does not spin the PTY worker several times a second.
pub(crate) const AUTO_RESTART_COOLDOWN: Duration = Duration::from_secs(5);
/// A session running this long breaks the crash streak and resets the tries.
pub(crate) const AUTO_RESTART_RESET_AFTER: Duration = Duration::from_secs(60);

/// Whether a crashed service with `tries` prior auto-restarts and
/// `since_change` elapsed since the last schedule may be restarted now.
pub(crate) fn auto_restart_due(tries: u8, since_change: Duration) -> bool {
    tries < AUTO_RESTART_MAX_TRIES && since_change >= AUTO_RESTART_COOLDOWN
}

impl App {
    pub(crate) fn new(cfg: Config, initial: Option<usize>) -> Self {
        ledger::write_rosters(&cfg.projects);
        let sessions = (0..cfg.projects.len()).map(|_| None).collect();
        let rank_prefixes = compute_rank_prefixes(&cfg.projects);
        let order = display_order(&cfg.projects);
        let env_vault = envvault::load_env().unwrap_or_default();
        let skills_vault = skills::load_skills().unwrap_or_default();
        let tools_vault = tools::load_tools().unwrap_or_default();
        let refs_vault = refs::load_refs().unwrap_or_default();
        let models_vault = models::load_models().unwrap_or_default();
        let servers_vault = load_servers();
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
        let (model_tx, model_rx) = mpsc::channel();
        let (nudge_tx, nudge_rx) = mpsc::channel();
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
            add: AddForm::new(),
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
            model_tx,
            model_rx,
            nudge_tx,
            nudge_rx,
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
            gsearch: None,
            procs: None,
            env_vault,
            skills_vault,
            tools_vault,
            refs_vault,
            models_vault,
            servers_vault,
            vault_modal: None,
            starting: HashMap::new(),
            auto_restarts: HashMap::new(),
            start_tx,
            start_rx,
            pane: (0, 0),
            zoom: false,
            sidebar_collapsed: false,
            hover_row: None,
            wait_badge_rect: Rect::default(),
            quick_reply: None,
            git_view: None,
            search: SearchState::new(),
            hangul_hotkey_seen: false,
            mgmt_sel: None,
            jump_buf: String::new(),
            jump_at: Instant::now() - Duration::from_secs(10),
            quit: false,
        }
    }

    /// True when any overlay popup (other than `Mode::Help` / `Mode::Add`) is open.
    pub(crate) fn has_overlay(&self) -> bool {
        self.vault_modal.is_some()
            || self.history.is_some()
            || self.git_view.is_some()
            || self.gsearch.is_some()
            || self.quick_reply.is_some()
            || self.procs.is_some()
            || self.context_menu.is_some()
            || self.confirm_quit
            || self.confirm_start_all
            || self.confirm_stop.is_some()
            || self.confirm_remove.is_some()
            || self.confirm_restart.is_some()
            || self.confirm_update
            || self.confirm_apply
    }

    pub(crate) fn has_modal(&self) -> bool {
        matches!(self.mode, Mode::Help | Mode::Add) || self.has_overlay()
    }

    /// True when the active UI state only accepts ASCII/navigation shortcuts
    /// (no Korean prose composition needed), so the OS IME can temporarily
    /// switch to English to avoid holding consonants in a preedit box.
    pub(crate) fn wants_ascii_ime(&self) -> bool {
        if self
            .vault_modal
            .as_ref()
            .is_some_and(VaultModal::wants_text_input)
            || self.quick_reply.as_ref().is_some_and(|q| q.typing)
        {
            return false;
        }
        if self.has_overlay() {
            return true;
        }
        match self.mode {
            Mode::Manager | Mode::Help => true,
            Mode::Add => matches!(self.add.field, 1 | 3 | 5 | 6 | 7 | 8 | 9),
            Mode::Focus | Mode::Send | Mode::Filter | Mode::Search => false,
        }
    }

    /// Columns the sidebar takes of a frame `area_w` wide.
    fn side_w(&self, area_w: u16) -> u16 {
        if self.zoom && self.mode == Mode::Focus {
            0
        } else if self.sidebar_collapsed {
            if area_w < 48 {
                0
            } else {
                COLLAPSED_SIDEBAR_WIDTH
            }
        } else {
            sidebar_width(area_w)
        }
    }

    pub(crate) fn toggle_sidebar(&mut self) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
        if !self.sidebar_collapsed {
            self.hover_row = None;
        }
        let note = if self.sidebar_collapsed {
            "Sidebar collapsed (] or > expands)"
        } else {
            "Sidebar expanded ([ or < collapses)"
        };
        self.message = (note.to_string(), Notice::Info);
        self.dirty = true;
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

    /// Live session facts for project `pi`, for status filter tokens.
    pub(crate) fn live_facts(&self, pi: usize) -> FilterLive {
        let slot = self.sessions.get(pi).and_then(|o| o.as_ref());
        let running = slot.is_some_and(|s| s.status.is_running());
        FilterLive {
            running,
            waiting: slot
                .is_some_and(|s| s.status.is_running() && s.phase() == AgentPhase::NeedsInput),
            done_unseen: slot.is_some_and(|s| {
                s.status.is_running() && s.phase() == AgentPhase::Done && !s.acked
            }),
            failed: slot.is_some_and(|s| s.status.needs_attention()),
            exited: slot.is_some_and(|s| matches!(s.status, SessionStatus::Exited(_))),
        }
    }

    /// Recompute the filtered sidebar order from the filter text and live
    /// session state, keeping the selection when it still matches.
    pub(crate) fn refresh_filter(&mut self) {
        let live: Vec<FilterLive> = (0..self.cfg.projects.len())
            .map(|pi| self.live_facts(pi))
            .collect();
        let query = self.filter_buf.as_str().to_string();
        let sel_pi = self.order.get(self.selected).copied();
        self.order = apply_filter_live(&self.cfg.projects, &live, &query);
        self.selected = sel_pi
            .and_then(|pi| self.order.iter().position(|&x| x == pi))
            .unwrap_or(0);
        self.jump_buf.clear();
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

    pub(super) fn target_dir(&self, target: VaultTarget) -> Option<std::path::PathBuf> {
        match target {
            VaultTarget::AddForm => self
                .add
                .edit
                .is_some()
                .then(|| {
                    let raw = self.add.path.as_str();
                    std::path::PathBuf::from(clean_path(&raw))
                })
                .filter(|p| p.is_dir()),
            VaultTarget::Project(pi) => self.cfg.projects.get(pi).map(|p| p.path.clone()),
            VaultTarget::VaultOnly => None,
        }
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
        if self.git_view.as_mut().is_some_and(GitView::poll) {
            self.dirty = true;
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
        while let Ok((name, r)) = self.model_rx.try_recv() {
            let one_line = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
            self.message = match r {
                Ok(reply) => (
                    format!("Model {name} works: {}", one_line(reply)),
                    Notice::Info,
                ),
                Err(e) => (
                    format!("Model {name} failed: {}", one_line(e)),
                    Notice::Error,
                ),
            };
            self.dirty = true;
        }
        while let Ok(name) = self.nudge_rx.try_recv() {
            if let Some(pi) = self
                .cfg
                .projects
                .iter()
                .position(|p| p.name.eq_ignore_ascii_case(&name))
            {
                if let Some(Some(s)) = self.sessions.get_mut(pi) {
                    if s.last_phase == AgentPhase::Done
                        && s.send_nudge(crate::knowledge::dir().ok().as_deref())
                    {
                        self.dirty = true;
                    }
                }
            }
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
        let keyboard_free = self.mode == Mode::Manager && !self.has_modal();
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
                if changed {
                    if let Some(qr) = self.quick_reply.as_mut().filter(|q| q.pi == pi) {
                        qr.tail = s.tail_lines_snapshot(10);
                        self.dirty = true;
                    }
                }
                if self.cfg.projects[pi].kind == Kind::Agent && phase_now != s.last_phase {
                    // No bell for the session you are typing into.
                    let watching = selected && self.mode == Mode::Focus;
                    self.bell |= !watching && rings_bell(phase_now);
                    // A finished task is reported to the commander's REPORTS.md.
                    if phase_now == AgentPhase::Done && s.last_phase == AgentPhase::Working {
                        // Agents without hooks get the Stop hook's reminder typed in.
                        // A bare shell would run the text as a command, unless an
                        // agent CLI started at its prompt holds the foreground.
                        let cmd = &self.cfg.projects[pi].command;
                        if !is_bare_shell(cmd) || s.fg_name.as_deref().is_some_and(is_agent_cli) {
                            if let Some(dir) = s.nudge_check_dir() {
                                let tx = self.nudge_tx.clone();
                                let name = self.cfg.projects[pi].name.clone();
                                thread::spawn(move || {
                                    if ledger::unrecorded(&dir) {
                                        let _ = tx.send(name);
                                    }
                                });
                            }
                        }
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
                    // The work history: squad files it once a task completes (not on intermediate questions).
                    if phase_now == AgentPhase::Working {
                        s.work_started.get_or_insert_with(SystemTime::now);
                    } else if phase_now == AgentPhase::Done
                        || (phase_now == AgentPhase::Idle && s.last_phase == AgentPhase::Working)
                    {
                        file_history(s, &self.cfg.projects[pi], "task complete".into());
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
        self.auto_restart_crashed();
    }

    /// Restart crashed services that opted in (`restart_on_crash`), a few
    /// seconds after the crash and at most a few times in a row. Clean
    /// exits, user stops (`s`), and agents never restart: only a failed
    /// session slot still sitting in the manager qualifies, and a session
    /// that stays up breaks the streak.
    fn auto_restart_crashed(&mut self) {
        let mut start: Vec<(usize, u8)> = Vec::new();
        let mut give_up: Vec<usize> = Vec::new();
        let projects = &self.cfg.projects;
        self.auto_restarts
            .retain(|name, _| projects.iter().any(|p| p.name == *name));
        for (pi, p) in self.cfg.projects.iter().enumerate() {
            let crashed = self
                .sessions
                .get(pi)
                .and_then(|o| o.as_ref())
                .is_some_and(|s| !s.status.is_running() && s.status.needs_attention());
            let wanted =
                p.kind == Kind::Service && p.restart_on_crash && crashed && !self.is_starting(pi);
            if wanted {
                match self.auto_restarts.get(&p.name).copied() {
                    Some((tries, _)) if tries >= AUTO_RESTART_MAX_TRIES => {
                        if tries == AUTO_RESTART_MAX_TRIES {
                            // Complain once, then leave the dead session alone.
                            give_up.push(pi);
                            self.auto_restarts
                                .insert(p.name.clone(), (tries + 1, Instant::now()));
                        }
                    }
                    Some((tries, last)) if auto_restart_due(tries, last.elapsed()) => {
                        start.push((pi, tries + 1));
                    }
                    None => {
                        // Arm the cooldown; the restart fires on a later pass.
                        self.auto_restarts
                            .insert(p.name.clone(), (0, Instant::now()));
                    }
                    _ => {}
                }
            } else {
                // Forget crash streaks for gone or clean sessions, and for
                // sessions healthy long enough to break the streak.
                let settled = match self.sessions.get(pi).and_then(|o| o.as_ref()) {
                    None => true,
                    Some(s) if s.status.is_running() => s
                        .started
                        .is_some_and(|t| t.elapsed() >= AUTO_RESTART_RESET_AFTER),
                    Some(_) => true,
                };
                if settled {
                    self.auto_restarts.remove(&p.name);
                }
            }
        }
        let (rows, cols) = self.pane;
        let changed = !start.is_empty() || !give_up.is_empty();
        for (pi, tries) in start {
            let name = self.cfg.projects[pi].name.clone();
            self.auto_restarts.insert(name, (tries, Instant::now()));
            self.begin_start(pi, rows, cols, Vec::new());
            self.message = (
                format!(
                    "Restarting {} (auto-restart #{tries})",
                    self.cfg.projects[pi].name
                ),
                Notice::Info,
            );
        }
        for pi in give_up {
            self.message = (
                format!(
                    "Gave up auto-restarting {}; it keeps crashing",
                    self.cfg.projects[pi].name
                ),
                Notice::Warn,
            );
        }
        if changed {
            self.dirty = true;
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
            || self.git_view.as_ref().is_some_and(GitView::busy)
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
        if let Ok(fresh) = envvault::load_env() {
            self.env_vault = fresh;
        }
        if let Ok(fresh) = skills::load_skills() {
            self.skills_vault = fresh;
        }
        if let Ok(fresh) = tools::load_tools() {
            self.tools_vault = fresh;
        }
        if let Ok(fresh) = refs::load_refs() {
            self.refs_vault = fresh;
        }
        let dotenv = envvault::read_dotenv(&p.path);
        self.add.env = self
            .env_vault
            .vars
            .iter()
            .filter(|e| dotenv.contains_key(&e.key))
            .map(|e| e.key.clone())
            .collect();
        self.add.skills = skills::read_project_skills(&p.path, &self.skills_vault)
            .vault_state
            .into_keys()
            .collect();
        self.add.tools = tools::read_project_tools(&p.path, &self.tools_vault)
            .vault_state
            .into_keys()
            .collect();
        self.add.refs = refs::read_project_refs(&p.path, &self.refs_vault)
            .vault_state
            .into_keys()
            .collect();
        self.add.edit = Some(p.name);
        self.servers_vault = load_servers();
        let path = p.path.display().to_string();
        // A folder on a registered server shows as that server plus its dir.
        let on = remote(&p.path).and_then(|(host, dir)| {
            let i = self
                .servers_vault
                .vars
                .iter()
                .position(|s| s.value == host)?;
            Some((i + 1, dir.to_string()))
        });
        let (server, dir) = on.unwrap_or((0, path));
        self.add.server = server;
        self.add.path.set(&dir);
        self.add.kind = p.kind;
        if p.command != shell() {
            self.add.cmd.set(&join_command(&p.command));
        }
        self.add.under.set(p.reports_to.as_deref().unwrap_or(""));
        self.add.autostart = p.autostart;
        self.add.field = 2;
        self.path_edited();
    }

    fn open_add(&mut self) {
        self.add.edit = None;
        self.mode = Mode::Add;
        self.servers_vault = load_servers();
        self.add.server = 0;
        // With servers registered the form opens on the Server row.
        self.add.field = if self.servers_vault.vars.is_empty() {
            0
        } else {
            9
        };
        self.add.path.clear();
        self.add.kind = Kind::Agent;
        self.add.cmd.clear();
        self.add.under.clear();
        self.add.autostart = false;
        self.add.env.clear();
        self.add.skills.clear();
        self.add.tools.clear();
        self.add.refs.clear();
        self.add.err.clear();
        self.add.state = PathState::Blank;
        self.add.top.set(0);
        self.path_edited();
    }

    /// The server picked in the form: `(name, address)`, or `None` for this PC.
    pub(crate) fn add_server_entry(&self) -> Option<&envvault::EnvEntry> {
        self.add
            .server
            .checked_sub(1)
            .and_then(|i| self.servers_vault.vars.get(i))
    }

    /// The Path field as it will be stored: `host:dir` on a server.
    pub(crate) fn add_full_path(&self) -> String {
        let addr = self.add_server_entry().map_or("", |s| s.value.as_str());
        server_path(addr, &self.add.path.as_str())
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

/// The registered servers (`V`); none when the file is missing or broken.
pub(crate) fn load_servers() -> EnvVault {
    servers_path()
        .and_then(|p| envvault::load_env_from(&p))
        .unwrap_or_default()
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
    let prompts = s.take_task_prompts();
    // Typing the next question reads as work (keys, echo) but is none: no
    // prompt was sent and it lasted seconds. Its screen lines are not
    // captured here, so the next real entry still carries them.
    if prompts.is_empty() && took < history::TYPING_ONLY && outcome == "task complete" {
        return;
    }
    let command = match &s.fg_name {
        Some(fg) if is_bare_shell(&p.command) => fg.clone(),
        _ => join_command(&p.command),
    };
    history::record(history::Task {
        project: p.name.clone(),
        path: p.path.clone(),
        command,
        started,
        took,
        outcome,
        prompts,
        // A Claude Code agent is logged from its transcript; skip the
        // scrollback walk on the UI thread.
        output: if s.transcript.is_some() {
            String::new()
        } else {
            s.capture_new_output()
        },
        transcript: s.transcript.clone(),
    });
}

/// A project's SITREPs open for reading: the list, the picked one's text and
/// how far it is scrolled.
/// One row of the global search (`G`): project `pi` with `count` matches.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct GHit {
    pub(crate) pi: usize,
    pub(crate) count: usize,
}

/// Global search over every session's scrollback (`G` in the manager).
/// Typing narrows the hit list live; `Enter` jumps to the picked session
/// with the same query loaded into its own find (`F`).
pub(crate) struct GSearch {
    pub(crate) query: String,
    pub(crate) hits: Vec<GHit>,
    pub(crate) sel: usize,
}

pub(crate) struct History {
    /// Popup title word: "History" for SITREPs, "Lessons" for shared lessons.
    pub(crate) kind: &'static str,
    /// Project name, or the lessons folder path.
    project: String,
    items: Vec<(String, std::path::PathBuf)>,
    /// Visible rows into `items` when `query` filters the list.
    view: Vec<usize>,
    /// List filter text (`/` focuses it, `Enter` keeps, `Esc` clears).
    query: String,
    typing: bool,
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
    fn open(kind: &'static str, project: &str, items: Vec<(String, std::path::PathBuf)>) -> Self {
        let view: Vec<usize> = (0..items.len()).collect();
        let mut h = Self {
            kind,
            project: project.to_string(),
            items,
            view,
            query: String::new(),
            typing: false,
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
    fn current(&self) -> Option<&(String, std::path::PathBuf)> {
        self.view.get(self.sel).and_then(|&i| self.items.get(i))
    }
    fn load(&mut self) {
        self.scroll = 0;
        self.text = self.current().map_or_else(String::new, |(_, p)| {
            let t = std::fs::read_to_string(p).unwrap_or_else(|e| format!("Cannot read: {e}"));
            // Agent-written files are untrusted terminal input: strip control
            // characters so escape sequences cannot reach the outer terminal.
            let t = sanitize_chrome_text(&t);
            // Daily logs grow at the end: keep the newest part, not the oldest.
            let skip = t.chars().count().saturating_sub(200_000);
            t.chars().skip(skip).collect()
        });
    }
    fn select(&mut self, i: usize) {
        let i = i.min(self.view.len().saturating_sub(1));
        if i != self.sel {
            self.sel = i;
            self.load();
        }
    }
    /// Recompute the visible rows after the filter text changed.
    fn refilter(&mut self) {
        self.view = crate::ledger::filter_sitrep_names(&self.items, &self.query);
        self.sel = 0;
        self.load();
    }
    /// Screen rows the text takes once wrapped. Long lines (an agent's last
    /// output) wrap, and counting them as one row left the end out of reach.
    // ponytail: counts column wrapping; word wrap can add a row or two, well
    // inside the page that is still on screen at the last scroll position.
    fn rows(&self) -> usize {
        wrapped_rows(&self.text, self.wrap_w.get())
    }
    fn max_scroll(&self) -> u16 {
        self.rows().saturating_sub(1).min(u16::MAX as usize) as u16
    }
    fn scroll_by(&mut self, delta: isize) {
        let next = (self.scroll as isize + delta).clamp(0, self.max_scroll() as isize);
        self.scroll = next as u16;
    }
}

pub(crate) fn wrapped_rows(text: &str, wrap_w: u16) -> usize {
    let w = usize::from(wrap_w).max(1);
    text.lines().map(|l| width(l).div_ceil(w).max(1)).sum()
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
    // The shared lessons folder, and the global instructions that point codex
    // and agy at it (they have no SessionStart hook).
    if let Ok(dir) = crate::knowledge::ensure() {
        crate::knowledge::install_global_at(&crate::knowledge::global_targets(), &dir);
    }
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
