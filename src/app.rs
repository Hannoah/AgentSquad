use anyhow::{bail, Result};
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
use crate::session::*;
use crate::sys::*;
use crate::ui::*;
use crate::updater::*;

pub struct ScreenGuard;
impl Drop for ScreenGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            crossterm::cursor::Show
        );
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

/// A bordered, centered popup with a bold colored title.
fn modal(f: &mut Frame, w: u16, h: u16, title: &str, color: Color, lines: Vec<Line<'static>>) {
    let popup = centered_rect(w, h, f.area());
    f.render_widget(ratatui::widgets::Clear, popup);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(Line::from(Span::styled(
                    title.to_string(),
                    Style::default()
                        .fg(color)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ))),
        ),
        popup,
    );
}

const SAMPLE_EVERY: Duration = Duration::from_secs(2);

/// Everything the manager keeps between frames. `run` owns the terminal;
/// `App` owns state, drawing and input handling.
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
    filter_buf: LineInput,
    add_field: usize,
    add_path: LineInput,
    add_kind: Kind,
    add_cmd: LineInput,
    add_err: String,
    confirm_stop: Option<usize>,
    confirm_remove: Option<usize>,
    confirm_restart: Option<usize>,
    confirm_quit: bool,
    confirm_start_all: bool,
    message: (String, Notice),
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
    updating: bool,
    tick: u64,
    last_anim_tick: Instant,
    quit: bool,
}

impl App {
    fn new(cfg: Config, initial: Option<usize>) -> Self {
        let sessions = (0..cfg.projects.len()).map(|_| None).collect();
        let order = display_order(&cfg.projects);
        let selected = initial
            .and_then(|i| order.iter().position(|&x| x == i))
            .unwrap_or(0);
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
            filter_buf: LineInput::new(),
            add_field: 0,
            add_path: LineInput::new(),
            add_kind: Kind::Agent,
            add_cmd: LineInput::new(),
            add_err: String::new(),
            confirm_stop: None,
            confirm_remove: None,
            confirm_restart: None,
            confirm_quit: false,
            confirm_start_all: false,
            message: (mode_hints(Mode::Manager).to_string(), Notice::Info),
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
            updating: false,
            tick: 0,
            last_anim_tick: Instant::now(),
            quit: false,
        }
    }

    /// Spinner clock; redraws only while something is running.
    fn animate(&mut self) {
        if self.mode != Mode::Focus && self.last_anim_tick.elapsed() >= Duration::from_millis(200) {
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

    /// Fold in measurements, update messages and finished sessions.
    fn pump(&mut self) {
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
            || self.confirm_quit
            || self.confirm_start_all
            || self.confirm_stop.is_some()
            || self.confirm_remove.is_some()
            || self.confirm_restart.is_some());
        if keyboard_free && !self.updating && !self.confirm_update && self.update_offer.is_some() {
            self.confirm_update = true;
            self.dirty = true;
        }
        for (pi, slot) in self.sessions.iter_mut().enumerate() {
            if let Some(s) = slot {
                let was_running = s.status.is_running();
                self.dirty |= s.update();
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

    fn open_add(&mut self) {
        self.mode = Mode::Add;
        self.add_field = 0;
        self.add_path.clear();
        self.add_kind = Kind::Agent;
        self.add_cmd.clear();
        self.add_err.clear();
    }

    fn stopped_count(&self) -> usize {
        self.order
            .iter()
            .filter(|&&pi| !slot_running(&self.sessions, pi))
            .count()
    }

    // ---- drawing ----

    fn draw(&mut self, f: &mut Frame) {
        let sidebar = sidebar_width(f.area().width);
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(f.area());
        let panels = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(sidebar), Constraint::Min(1)])
            .split(vertical[0]);
        self.side_rect = panels[0];
        self.pane_rect = panels[1];
        if sidebar > 0 {
            self.draw_sidebar(f, panels[0]);
        }
        self.draw_pane(f, panels[1]);
        self.draw_status(f, vertical[1]);
        let has_modal = matches!(self.mode, Mode::Help | Mode::Add)
            || self.confirm_stop.is_some()
            || self.confirm_remove.is_some()
            || self.confirm_restart.is_some()
            || self.confirm_update
            || self.confirm_quit
            || self.confirm_start_all;
        if has_modal {
            let full_area = f.area();
            dim_area(f.buffer_mut(), full_area);
        }
        if self.mode == Mode::Help {
            self.draw_help(f);
        }
        self.draw_confirms(f);
        if self.mode == Mode::Add {
            self.draw_add(f);
        }
    }

    fn draw_sidebar(&mut self, f: &mut Frame, panel: Rect) {
        let sel_pi = self.order.get(self.selected).copied();
        let inner_w = (panel.width as usize).saturating_sub(2);
        let mut items: Vec<ListItem> = Vec::new();
        let mut targets: Vec<Option<usize>> = Vec::new();
        let mut sel_row: Option<usize> = None;
        let mut n = 0;
        let running: Vec<bool> = self
            .sessions
            .iter()
            .map(|o| o.as_ref().map(|s| s.status.is_running()).unwrap_or(false))
            .collect();
        for (title, k) in [("Agents", Kind::Agent), ("Services", Kind::Service)] {
            let (up, total) = group_counts(&self.cfg.projects, &running, k);
            items.push(ListItem::new(group_header(title, k, up, total)));
            targets.push(None);
            let mut any = false;
            for &pi in self
                .order
                .iter()
                .filter(|&&i| self.cfg.projects[i].kind == k)
            {
                any = true;
                n += 1;
                let p = &self.cfg.projects[pi];
                let status_text = self.sessions[pi]
                    .as_ref()
                    .map(|s| s.status.label())
                    .unwrap_or(std::borrow::Cow::Borrowed("Stopped"));
                let status: &str = &status_text;
                let fg = self.sessions[pi]
                    .as_ref()
                    .and_then(|s| s.fg_name.as_deref());
                let shown_name = live_name(p, fg);
                let mut detail = String::new();
                if status == "Running" {
                    if let Some(s) = self.sessions[pi].as_ref() {
                        if let Some(started) = s.started {
                            detail.push(' ');
                            detail.push_str(&uptime_label(started.elapsed().as_secs()));
                        }
                        if let Some(u) = s.usage {
                            detail.push_str(" · ");
                            detail.push_str(&usage_label(u));
                        }
                    }
                }
                if k == Kind::Service && !is_bare_shell(&p.command) {
                    detail.push(' ');
                    detail.push_str(&p.command.join(" "));
                } else if let Some(tag) = agent_tag(p) {
                    detail.push(' ');
                    detail.push_str(tag);
                }
                let mark = if Some(pi) == sel_pi { "▎" } else { " " };
                let row_style = if Some(pi) == sel_pi {
                    selected_row_style()
                } else if status == "Running" {
                    Style::default().add_modifier(ratatui::style::Modifier::BOLD)
                } else {
                    Style::default().add_modifier(ratatui::style::Modifier::DIM)
                };
                let agent_phase = if p.kind == Kind::Agent && status == "Running" {
                    self.sessions[pi].as_ref().map(|s| s.phase())
                } else {
                    None
                };
                let status_span_style = {
                    let (base_fg, is_bold) = if let Some(ph) = agent_phase {
                        match ph {
                            AgentPhase::Working => (Color::Cyan, true),
                            AgentPhase::NeedsInput => (Color::Yellow, true),
                            AgentPhase::Done => (Color::Green, true),
                            AgentPhase::Idle => {
                                if Some(pi) == sel_pi {
                                    (Color::Gray, false)
                                } else {
                                    (Color::DarkGray, false)
                                }
                            }
                        }
                    } else if status == "Running" {
                        (Color::Green, true)
                    } else if status.starts_with("Error")
                        || (status.starts_with("Exited") && !status.ends_with("(0)"))
                    {
                        (Color::Red, true)
                    } else if Some(pi) == sel_pi {
                        (Color::Gray, false)
                    } else {
                        (Color::DarkGray, false)
                    };

                    let mut st = Style::default().fg(base_fg);
                    if is_bold {
                        st = st.add_modifier(ratatui::style::Modifier::BOLD);
                    }
                    if Some(pi) == sel_pi {
                        st = st.bg(Color::DarkGray);
                    }
                    st
                };
                let detail_style = if Some(pi) == sel_pi {
                    row_style
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                let prefix = format!("{mark}{n} ");
                let status_full = if let Some(ph) = agent_phase {
                    ph.icon(self.tick).0
                } else if status == "Running" {
                    format!("{}", status_glyph(status, self.tick))
                } else {
                    format!("{} {status}", status_glyph(status, self.tick))
                };
                let (status_w, name_w, detail_w) = row_widths(
                    inner_w,
                    prefix.chars().count(),
                    status_full.chars().count(),
                    shown_name.chars().count(),
                );
                if Some(pi) == sel_pi {
                    sel_row = Some(items.len());
                }
                targets.push(Some(n - 1));
                items.push(ListItem::new(Line::from(vec![
                    Span::styled(prefix, row_style),
                    Span::styled(fit(&status_full, status_w), status_span_style),
                    Span::raw(" "),
                    Span::styled(
                        fit(shown_name, name_w),
                        row_name_style(Some(pi) == sel_pi, status, p.kind),
                    ),
                    Span::styled(fit(&detail, detail_w), detail_style),
                ])));
            }
            if !any {
                targets.push(None);
                items.push(
                    ListItem::new("  (none)")
                        .style(Style::default().add_modifier(ratatui::style::Modifier::DIM)),
                );
            }
        }
        let visible_rows = (panel.height as usize).saturating_sub(2).max(1);
        if let Some(sr) = sel_row {
            self.list_offset = keep_visible(self.list_offset, sr, visible_rows);
        }
        self.list_offset = self.list_offset.min(items.len().saturating_sub(1));
        self.row_targets = targets;
        let mut window = items.split_off(self.list_offset);
        window.truncate(visible_rows);
        f.render_widget(
            List::new(window).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(sidebar_border_style(self.mode == Mode::Focus))
                    .title(list_title(self.cfg.projects.len(), self.order.len())),
            ),
            panel,
        );
    }

    fn draw_pane(&self, f: &mut Frame, panel: Rect) {
        let sel_pi = self.order.get(self.selected).copied();
        let (sel_status_text, sel_uptime): (std::borrow::Cow<'static, str>, Option<u64>) = sel_pi
            .and_then(|pi| self.sessions.get(pi).and_then(|o| o.as_ref()))
            .map(|s| (s.status.label(), s.started.map(|t| t.elapsed().as_secs())))
            .unwrap_or((std::borrow::Cow::Borrowed("Stopped"), None));
        let sel_status: &str = &sel_status_text;
        let sel_fg = sel_pi
            .and_then(|pi| self.sessions.get(pi).and_then(|o| o.as_ref()))
            .and_then(|s| s.fg_name.as_deref());
        let usage_note = sel_pi
            .and_then(|pi| self.sessions.get(pi))
            .and_then(|o| o.as_ref())
            .and_then(|s| s.usage)
            .map(|u| format!(" · {}", usage_label(u)))
            .unwrap_or_default();
        let title = sel_pi
            .and_then(|pi| self.cfg.projects.get(pi))
            .map(|p| {
                let scroll_note = sel_pi
                    .and_then(|pi| self.sessions.get(pi))
                    .and_then(|o| o.as_ref())
                    .map(|s| s.parser.screen().scrollback())
                    .filter(|&n| n > 0)
                    .map(|n| format!(" · ⇅{n}"))
                    .unwrap_or_default();
                let focus_label = if panel.width >= 60 {
                    if self.mode == Mode::Focus {
                        " · INPUT (Ctrl+Q to exit)"
                    } else {
                        " · PREVIEW (Enter to type)"
                    }
                } else if self.mode == Mode::Focus {
                    " · INPUT"
                } else {
                    " · MANAGER"
                };
                let sel_phase = sel_pi
                    .and_then(|pi| self.sessions.get(pi))
                    .and_then(|o| o.as_ref())
                    .map(|s| s.phase());
                let (disp_status, disp_style) =
                    pane_status_display(sel_status, sel_uptime, self.tick, p.kind, sel_phase);
                Line::from(vec![
                    Span::styled(
                        live_pane_title(p, sel_fg),
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
                    Span::styled(disp_status, disp_style),
                    Span::styled(
                        focus_label,
                        if self.mode == Mode::Focus {
                            Style::default()
                                .fg(Color::Green)
                                .add_modifier(ratatui::style::Modifier::BOLD)
                        } else {
                            Style::default().fg(Color::DarkGray)
                        },
                    ),
                    Span::raw(format!("{usage_note}{scroll_note}]")),
                ])
            })
            .unwrap_or_else(|| Line::from("No projects: squad add <path>"));
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(pane_border_style(self.mode == Mode::Focus))
            .title(title);
        let inner = block.inner(panel);
        f.render_widget(block, panel);
        if let Some(s) = sel_pi.and_then(|pi| self.sessions.get(pi).and_then(|o| o.as_ref())) {
            let screen = s.parser.screen();
            s.paint(f.buffer_mut(), inner);
            let scrollback_len = screen.scrollback_len();
            if scrollback_len > 0 {
                let mut scrollbar_state = ScrollbarState::new(scrollback_len)
                    .position(scrollback_len.saturating_sub(s.scroll));
                let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None)
                    .track_symbol(Some("░"))
                    .thumb_symbol("█");
                f.render_stateful_widget(scrollbar, panel, &mut scrollbar_state);
            }
            // A finished session leaves its last output on screen; say so
            // and say how to bring it back, right where the eyes are.
            if !s.status.is_running() && !s.dismissed_exit && inner.height > 0 {
                let bg = if s.status.needs_attention() {
                    Color::Red
                } else {
                    Color::DarkGray
                };
                f.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled(
                            format!(" {} ", s.status),
                            Style::default()
                                .fg(Color::White)
                                .bg(bg)
                                .add_modifier(ratatui::style::Modifier::BOLD),
                        ),
                        Span::styled(
                            " Enter/r restart · Esc/s dismiss ",
                            Style::default().add_modifier(ratatui::style::Modifier::DIM),
                        ),
                    ]))
                    .alignment(Alignment::Right),
                    ratatui::layout::Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1),
                );
            }
            if self.mode == Mode::Focus && s.scroll == 0 && !screen.hide_cursor() {
                let (r, c) = screen.cursor_position();
                if r < inner.height && c < inner.width {
                    f.set_cursor_position((inner.x + c, inner.y + r));
                }
            }
        } else {
            let body = empty_state_lines(!self.cfg.projects.is_empty());
            let pad = inner.height.saturating_sub(body.len() as u16) / 2;
            let mut lines: Vec<Line> = (0..pad).map(|_| Line::from("")).collect();
            for (idx, line) in body.iter().enumerate() {
                if idx == 0 {
                    lines.push(Line::from(Span::styled(
                        line.clone(),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(ratatui::style::Modifier::BOLD),
                    )));
                } else {
                    lines.push(Line::from(Span::styled(
                        line.clone(),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
            }
            f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), inner);
        }
    }

    fn draw_status(&self, f: &mut Frame, bar_area: Rect) {
        let sel_pi = self.order.get(self.selected).copied();
        let (chip_text, chip_style) = mode_chip(self.mode);
        let mut bar = vec![
            Span::styled(
                chip_text,
                chip_style.add_modifier(ratatui::style::Modifier::BOLD),
            ),
            Span::raw(" "),
        ];
        let running_n = self
            .sessions
            .iter()
            .flatten()
            .filter(|s| s.status.is_running())
            .count();
        let failed_n = self
            .sessions
            .iter()
            .flatten()
            .filter(|s| s.status.needs_attention())
            .count();
        let waiting_n = self
            .cfg
            .projects
            .iter()
            .enumerate()
            .filter(|&(pi, p)| {
                p.kind == Kind::Agent
                    && self
                        .sessions
                        .get(pi)
                        .and_then(|o| o.as_ref())
                        .is_some_and(|s| s.phase() == AgentPhase::NeedsInput)
            })
            .count();
        let wait_w = if waiting_n > 0 {
            format!(" ? {waiting_n}").chars().count() as u16
        } else {
            0
        };
        // Machine load is shown only when the bar has room for it.
        let sys_text = if f.area().width >= 70 && self.sys.total > 0 {
            self.sys.summary()
        } else {
            String::new()
        };
        let sys_w = if sys_text.is_empty() {
            0
        } else {
            sys_text.chars().count() as u16 + 3
        };
        // A found update stays visible even while the prompt waits for the
        // keyboard (e.g. you are typing into a session): `U` opens it.
        let upd_text = self
            .update_offer
            .as_ref()
            .filter(|_| !self.confirm_update && !self.updating)
            .map(|t| format!(" ⬆ {t} (U)"))
            .unwrap_or_default();
        let upd_w = upd_text.chars().count() as u16;
        let status_row = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(
                    summary_width(running_n, self.cfg.projects.len(), failed_n)
                        + wait_w
                        + sys_w
                        + upd_w,
                ),
            ])
            .split(bar_area);
        let input_w = (status_row[0].width as usize).saturating_sub(chip_text.chars().count() + 1);
        if self.mode == Mode::Focus {
            bar.extend(fitted_hints(Mode::Focus, input_w));
        } else if self.mode == Mode::Filter {
            bar.extend(input_line(
                "Filter: ".to_string(),
                &self.filter_buf,
                format!(" ({} matches, Enter keeps, Esc clears)", self.order.len()),
                input_w,
            ));
        } else if self.mode == Mode::Send {
            let name = sel_pi
                .and_then(|pi| self.cfg.projects.get(pi))
                .map(|p| p.name.as_str())
                .unwrap_or("?");
            bar.extend(input_line(
                format!("Send to {name}: "),
                &self.send_buf,
                " (Enter sends, Esc cancels)".to_string(),
                input_w,
            ));
        } else if self.mode == Mode::Add {
            bar.extend(fitted_hints(Mode::Add, input_w));
        } else if self.mode == Mode::Help && self.message.0 == mode_hints(Mode::Manager) {
            bar.extend(fitted_hints(Mode::Help, input_w));
        } else if self.message.0 == mode_hints(Mode::Manager) {
            // Idle hints recede so action feedback stands out.
            bar.extend(fitted_hints(Mode::Manager, input_w));
        } else {
            bar.push(Span::styled(
                self.message.0.clone(),
                notice_style(self.message.1),
            ));
        }
        f.render_widget(Paragraph::new(Line::from(bar)), status_row[0]);
        let mut right = Vec::new();
        if !sys_text.is_empty() {
            right.push(Span::styled(sys_text, self.sys.style()));
            right.push(Span::styled(
                " · ".to_string(),
                Style::default().fg(Color::DarkGray),
            ));
        }
        right.push(Span::styled(
            fleet_summary(running_n, self.cfg.projects.len()),
            if running_n > 0 {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::DarkGray)
            },
        ));
        if waiting_n > 0 {
            right.push(Span::styled(
                format!(" ? {waiting_n}"),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            ));
        }
        if failed_n > 0 {
            right.push(Span::styled(
                format!(" ✖ {failed_n}"),
                Style::default()
                    .fg(Color::Red)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            ));
        }
        if !upd_text.is_empty() {
            right.push(Span::styled(
                upd_text,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            ));
        }
        f.render_widget(
            Paragraph::new(Line::from(right)).alignment(Alignment::Right),
            status_row[1],
        );
    }

    fn draw_help(&self, f: &mut Frame) {
        // Fixed-size so every row fits on short terminals (a 50%
        // popup clipped the last help rows at 24 lines).
        let popup = fixed_rect(66, help_lines().len() as u16 + 2, f.area());
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
                    .border_type(BorderType::Rounded)
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

    fn draw_confirms(&self, f: &mut Frame) {
        let name_of = |pi: usize| self.cfg.projects.get(pi).map_or("?", |p| p.name.as_str());
        if let Some(pi) = self.confirm_stop {
            modal(
                f,
                60,
                30,
                "Stop session",
                Color::Red,
                confirm_stop_lines(name_of(pi)),
            );
        }
        if let Some(pi) = self.confirm_remove {
            modal(
                f,
                60,
                30,
                "Remove project",
                Color::Red,
                confirm_remove_lines(name_of(pi)),
            );
        }
        if let Some(pi) = self.confirm_restart {
            modal(
                f,
                60,
                30,
                "Restart session",
                Color::Red,
                confirm_restart_lines(name_of(pi)),
            );
        }
        if self.confirm_update {
            let tag = self.update_offer.as_deref().unwrap_or("?");
            modal(
                f,
                60,
                40,
                "Update available",
                Color::Yellow,
                confirm_update_lines(tag),
            );
        }
        if self.confirm_quit {
            let n = live_session_count(&self.sessions);
            modal(f, 60, 30, "Quit", Color::Red, confirm_quit_lines(n));
        }
        if self.confirm_start_all {
            let lines = confirm_start_all_lines(self.stopped_count());
            modal(f, 60, 30, "Start all sessions", Color::Cyan, lines);
        }
    }

    fn draw_add(&self, f: &mut Frame) {
        let popup = centered_rect(70, 40, f.area());
        let modal_w = (popup.width as usize).saturating_sub(2);
        f.render_widget(ratatui::widgets::Clear, popup);
        let kind_hint = if self.add_kind == Kind::Agent {
            "agent (Space toggles)"
        } else {
            "service (Space toggles)"
        };
        let cmd_text = if self.add_cmd.is_empty() {
            "(default shell)".to_string()
        } else {
            self.add_cmd.as_str()
        };
        let path_label = if self.add_kind == Kind::Service {
            "Path (optional)"
        } else {
            "Path"
        };
        let fields = [
            (path_label, self.add_path.as_str()),
            ("Kind", kind_hint.to_string()),
            ("Command", cmd_text),
        ];
        let mut lines = Vec::new();
        for (i, (label, val)) in fields.iter().enumerate() {
            let active = i == self.add_field;
            let mark = if active { ">" } else { " " };
            let label_style = if active {
                Style::default().add_modifier(ratatui::style::Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            let mut row = vec![Span::styled(format!("{mark} {label}: "), label_style)];
            let editing = active && i != 1;
            if editing {
                let input = if i == 0 {
                    &self.add_path
                } else {
                    &self.add_cmd
                };
                let (left, cur, right) = input.render_parts();
                let label_w = format!("{mark} {label}: ").chars().count();
                let (left, cur, right) =
                    input_window(&left, cur, &right, modal_w.saturating_sub(label_w));
                let value_style = Style::default().add_modifier(ratatui::style::Modifier::BOLD);
                row.push(Span::styled(left, value_style));
                row.push(Span::styled(
                    cur.to_string(),
                    value_style.bg(Color::White).fg(Color::Black),
                ));
                row.push(Span::styled(right, value_style));
                // When the command field is empty the display shows a
                // hint instead of the raw buffer; keep the hint visible
                // after the cursor block.
                if i == 2 && self.add_cmd.is_empty() {
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
        if !self.add_err.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("Error: {}", self.add_err),
                Style::default().fg(Color::Red),
            )));
        }
        f.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
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

    // ---- input ----

    fn on_resize(&mut self, a: Size) {
        let (rows, cols) = pane_size(a);
        for s in self.sessions.iter_mut().flatten() {
            if let Err(e) = s.resize(rows, cols) {
                self.message = (e.to_string(), Notice::Error);
            }
        }
        self.dirty = true;
    }

    fn on_paste(&mut self, data: String) {
        self.dirty = true;
        if self.mode == Mode::Focus {
            if let Some(&pi) = self.order.get(self.selected) {
                if let Some(Some(s)) = self.sessions.get_mut(pi) {
                    s.scroll_live();
                    if let Err(e) = s.input(data.as_bytes()) {
                        self.message = (e.to_string(), Notice::Error);
                        self.mode = Mode::Manager;
                    }
                }
            }
        } else if self.mode == Mode::Send {
            for ch in data.chars().filter(|&c| c != '\r' && c != '\n') {
                self.send_buf.insert(ch);
            }
        } else if self.mode == Mode::Filter {
            for ch in data.chars().filter(|&c| c != '\r' && c != '\n') {
                self.filter_buf.insert(ch);
            }
            self.order = apply_filter(&self.cfg.projects, &self.filter_buf.as_str());
            self.selected = 0;
        } else if self.mode == Mode::Add {
            for ch in data.chars().filter(|&c| c != '\r' && c != '\n') {
                match self.add_field {
                    0 => self.add_path.insert(ch),
                    2 => self.add_cmd.insert(ch),
                    _ => {}
                }
            }
            self.add_err.clear();
        }
    }

    fn on_mouse(&mut self, m: MouseEvent, area: Size, rows: u16, cols: u16) {
        if let Some(lines) = wheel_lines(m.kind) {
            let over_sidebar = self.mode != Mode::Focus
                && self.side_rect.width > 0
                && self
                    .side_rect
                    .contains(ratatui::layout::Position::new(m.column, m.row));
            if over_sidebar {
                if !self.order.is_empty() {
                    if lines > 0 {
                        self.selected = (self.selected + self.order.len() - 1) % self.order.len();
                    } else {
                        self.selected = (self.selected + 1) % self.order.len();
                    }
                }
            } else if let Some(s) = self
                .order
                .get(self.selected)
                .and_then(|&pi| self.sessions.get_mut(pi))
                .and_then(|o| o.as_mut())
            {
                let is_alt = s.parser.screen().alternate_screen();
                let scrollback_len = s.parser.screen().scrollback_len();

                if is_alt || (self.mode == Mode::Focus && scrollback_len == 0) {
                    let mouse_mode = s.parser.screen().mouse_protocol_mode();
                    if mouse_mode != vt100::MouseProtocolMode::None {
                        // Terminal coordinates are 1-based; the pane's
                        // border takes one column and one row.
                        let col = m
                            .column
                            .saturating_sub(self.pane_rect.x)
                            .clamp(1, cols.max(1));
                        let row = m.row.saturating_sub(self.pane_rect.y).clamp(1, rows.max(1));
                        let seq = mouse_wheel_seq(
                            s.parser.screen().mouse_protocol_encoding(),
                            lines > 0,
                            col,
                            row,
                        );
                        let _ = s.input(&seq);
                    } else {
                        // Alternate scroll mode:
                        // In fullscreen/alternate screen mode (like Claude Code, vim, less),
                        // translate wheel notches into PageUp/PageDown so the viewport scrolls naturally.
                        let seq: &[u8] = if lines > 0 {
                            b"\x1b[5~" // PageUp
                        } else {
                            b"\x1b[6~" // PageDown
                        };
                        let _ = s.input(seq);
                    }
                } else {
                    s.scroll_by(lines);
                }
            }
            self.dirty = true;
        } else if m.kind == MouseEventKind::Down(MouseButton::Left) {
            // Click landed outside a centered popup of w% x h%.
            let outside = |w: u16, h: u16| {
                !centered_rect(w, h, area.into())
                    .contains(ratatui::layout::Position::new(m.column, m.row))
            };
            if self.mode == Mode::Help {
                self.mode = Mode::Manager;
                self.dirty = true;
            } else if self.confirm_stop.is_some() {
                if outside(60, 30) {
                    self.confirm_stop = None;
                    self.dirty = true;
                }
            } else if self.confirm_remove.is_some() {
                if outside(60, 30) {
                    self.confirm_remove = None;
                    self.dirty = true;
                }
            } else if self.confirm_restart.is_some() {
                if outside(60, 30) {
                    self.confirm_restart = None;
                    self.dirty = true;
                }
            } else if self.confirm_quit {
                if outside(60, 30) {
                    self.confirm_quit = false;
                    self.dirty = true;
                }
            } else if self.confirm_start_all {
                if outside(60, 30) {
                    self.confirm_start_all = false;
                    self.dirty = true;
                }
            } else if self.confirm_update {
                if outside(60, 40) {
                    self.confirm_update = false;
                    self.dirty = true;
                }
            } else if self.mode == Mode::Add {
                let popup = centered_rect(70, 40, area.into());
                let clicked = ratatui::layout::Position::new(m.column, m.row);
                if !popup.contains(clicked) {
                    if self.add_path.is_empty() && self.add_cmd.is_empty() {
                        self.mode = Mode::Manager;
                    }
                } else {
                    let rel_y = m.row.saturating_sub(popup.y + 1);
                    match rel_y {
                        0 => self.add_field = 0,
                        1 => {
                            self.add_field = 1;
                            self.add_kind = self.add_kind.toggled();
                        }
                        2 => self.add_field = 2,
                        _ => {}
                    }
                }
                self.dirty = true;
            } else if matches!(self.mode, Mode::Send | Mode::Filter) {
                // In text input bar, keep input focus
            } else if let Some(i) = sidebar_click(
                self.side_rect,
                self.list_offset,
                &self.row_targets,
                m.column,
                m.row,
            ) {
                if self.selected == i {
                    // Re-clicking selected project: start if stopped, focus if running
                    if let Some(&pi) = self.order.get(self.selected) {
                        let live = self
                            .sessions
                            .get(pi)
                            .and_then(|o| o.as_ref())
                            .is_some_and(|s| s.status.is_running());
                        if live {
                            self.mode = Mode::Focus;
                        } else {
                            match start_session(
                                &mut self.sessions,
                                &self.cfg.projects,
                                pi,
                                rows,
                                cols,
                            ) {
                                None => self.mode = Mode::Focus,
                                Some(e) => self.message = (e, Notice::Error),
                            }
                        }
                    }
                } else {
                    self.selected = i;
                    self.mode = Mode::Manager;
                }
                self.dirty = true;
            } else if self
                .pane_rect
                .contains(ratatui::layout::Position::new(m.column, m.row))
            {
                // Clicking terminal pane: start if stopped, focus if running
                if let Some(&pi) = self.order.get(self.selected) {
                    let live = self
                        .sessions
                        .get(pi)
                        .and_then(|o| o.as_ref())
                        .is_some_and(|s| s.status.is_running());
                    if live {
                        self.mode = Mode::Focus;
                    } else {
                        match start_session(&mut self.sessions, &self.cfg.projects, pi, rows, cols)
                        {
                            None => self.mode = Mode::Focus,
                            Some(e) => self.message = (e, Notice::Error),
                        }
                    }
                }
                self.dirty = true;
            } else if m.row >= area.height.saturating_sub(1) && m.column < 12 {
                // Clicking the status-bar mode chip toggles help
                self.mode = if self.mode == Mode::Help {
                    Mode::Manager
                } else {
                    Mode::Help
                };
                self.dirty = true;
            }
        } else if m.kind == MouseEventKind::Down(MouseButton::Right)
            && !(matches!(
                self.mode,
                Mode::Help | Mode::Add | Mode::Send | Mode::Filter
            ) || self.confirm_quit
                || self.confirm_start_all
                || self.confirm_stop.is_some()
                || self.confirm_remove.is_some()
                || self.confirm_restart.is_some()
                || self.confirm_update)
        {
            // Right-click on sidebar: quick toggle (stop if running, start if stopped)
            if let Some(i) = sidebar_click(
                self.side_rect,
                self.list_offset,
                &self.row_targets,
                m.column,
                m.row,
            ) {
                self.selected = i;
                if let Some(&pi) = self.order.get(self.selected) {
                    let live = self
                        .sessions
                        .get(pi)
                        .and_then(|o| o.as_ref())
                        .is_some_and(|s| s.status.is_running());
                    if live {
                        self.confirm_stop = Some(pi);
                    } else {
                        match start_session(&mut self.sessions, &self.cfg.projects, pi, rows, cols)
                        {
                            None => self.mode = Mode::Focus,
                            Some(e) => self.message = (e, Notice::Error),
                        }
                    }
                }
                self.dirty = true;
            }
        }
    }

    fn on_key(&mut self, k: KeyEvent, rows: u16, cols: u16) {
        self.dirty = true;
        match self.mode {
            Mode::Focus => self.key_focus(k, rows),
            Mode::Send => self.key_send(k, rows, cols),
            Mode::Filter => self.key_filter(k),
            Mode::Add => self.key_add(k),
            Mode::Help => self.key_help(k),
            Mode::Manager => self.key_manager_or_confirm(k, rows, cols),
        }
    }

    /// Manager keys, or the answer to whichever confirmation is open.
    fn key_manager_or_confirm(&mut self, k: KeyEvent, rows: u16, cols: u16) {
        if self.confirm_update {
            self.key_confirm_update(k);
        } else if self.confirm_quit {
            self.key_confirm_quit(k);
        } else if self.confirm_start_all {
            self.key_confirm_start_all(k, rows, cols);
        } else if self.confirm_stop.is_some() {
            self.key_confirm_stop(k);
        } else if self.confirm_restart.is_some() {
            self.key_confirm_restart(k, rows, cols);
        } else if self.confirm_remove.is_some() {
            self.key_confirm_remove(k);
        } else {
            self.key_manager(k, rows, cols);
        }
    }

    fn key_focus(&mut self, k: KeyEvent, rows: u16) {
        if focus_exit_key(k.code, k.modifiers) {
            self.mode = Mode::Manager;
            return;
        }
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        match k.code {
            KeyCode::PageUp if shift => {
                scroll_selected(
                    &mut self.sessions,
                    &self.order,
                    self.selected,
                    rows as usize,
                    1,
                );
                return;
            }
            KeyCode::PageDown if shift => {
                scroll_selected(
                    &mut self.sessions,
                    &self.order,
                    self.selected,
                    rows as usize,
                    -1,
                );
                return;
            }
            KeyCode::End if shift => {
                scroll_live_selected(&mut self.sessions, &self.order, self.selected);
                return;
            }
            _ => {}
        }
        if let Some(&pi) = self.order.get(self.selected) {
            if let Some(Some(s)) = self.sessions.get_mut(pi) {
                s.scroll_live();
                let bytes = key_bytes(k, s.parser.screen().application_cursor());
                if let Err(e) = s.input(&bytes) {
                    self.message = (e.to_string(), Notice::Error);
                    self.mode = Mode::Manager;
                }
            }
        }
    }

    fn key_send(&mut self, k: KeyEvent, rows: u16, cols: u16) {
        match k.code {
            KeyCode::Esc => {
                self.mode = Mode::Manager;
                self.send_buf.clear();
            }
            KeyCode::Enter => {
                let line = self.send_buf.take();
                self.mode = Mode::Manager;
                if line.is_empty() {
                    self.message = ("Empty. Nothing sent.".to_string(), Notice::Info);
                } else if let Some(&pi) = self.order.get(self.selected) {
                    let alive = self
                        .sessions
                        .get(pi)
                        .and_then(|o| o.as_ref())
                        .map(|s| s.status.is_running())
                        .unwrap_or(false);
                    if !alive {
                        if let Some(e) =
                            start_session(&mut self.sessions, &self.cfg.projects, pi, rows, cols)
                        {
                            self.message = (e, Notice::Error);
                        }
                    }
                    if let Some(Some(s)) = self.sessions.get_mut(pi) {
                        let mut bytes = line.into_bytes();
                        // CR is Enter; LF reads as Shift+Enter (newline) to agents.
                        bytes.push(b'\r');
                        match s.input(&bytes) {
                            Ok(()) => {
                                self.message = (
                                    format!("Sent to {}", self.cfg.projects[pi].name),
                                    Notice::Info,
                                )
                            }
                            Err(e) => self.message = (e.to_string(), Notice::Error),
                        }
                    }
                }
            }
            KeyCode::Backspace => {
                self.send_buf.backspace();
            }
            KeyCode::Delete => {
                self.send_buf.delete();
            }
            KeyCode::Left => self.send_buf.move_left(),
            KeyCode::Right => self.send_buf.move_right(),
            KeyCode::Home => self.send_buf.move_home(),
            KeyCode::End => self.send_buf.move_end(),
            KeyCode::Char(ch)
                if !k.modifiers.contains(KeyModifiers::CONTROL)
                    && !k.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.send_buf.insert(ch)
            }
            _ => {}
        }
    }

    fn key_filter(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Esc => {
                self.mode = Mode::Manager;
                self.filter_buf.clear();
                self.order = display_order(&self.cfg.projects);
                self.selected = 0;
            }
            KeyCode::Enter => self.mode = Mode::Manager,
            KeyCode::Backspace => {
                self.filter_buf.backspace();
                self.order = apply_filter(&self.cfg.projects, &self.filter_buf.as_str());
                self.selected = 0;
            }
            KeyCode::Delete => {
                self.filter_buf.delete();
                self.order = apply_filter(&self.cfg.projects, &self.filter_buf.as_str());
                self.selected = 0;
            }
            KeyCode::Left => self.filter_buf.move_left(),
            KeyCode::Right => self.filter_buf.move_right(),
            KeyCode::Home => self.filter_buf.move_home(),
            KeyCode::End => self.filter_buf.move_end(),
            KeyCode::Char(ch)
                if !k.modifiers.contains(KeyModifiers::CONTROL)
                    && !k.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.filter_buf.insert(ch);
                self.order = apply_filter(&self.cfg.projects, &self.filter_buf.as_str());
                self.selected = 0;
            }
            _ => {}
        }
    }

    fn key_add(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Esc => self.mode = Mode::Manager,
            KeyCode::Enter => {
                self.add_err.clear();
                // A new project must be visible, so drop any filter.
                self.filter_buf.clear();
                match load() {
                    Err(e) => self.add_err = format!("Config reload failed: {e:#}"),
                    Ok(disk) => {
                        merge_projects(&mut self.cfg, &mut self.sessions, disk);
                        self.order = display_order(&self.cfg.projects);
                    }
                }
                if self.add_err.is_empty() {
                    match build_project(
                        &self.cfg.projects,
                        &self.add_path.as_str(),
                        self.add_kind,
                        split_command(&self.add_cmd.as_str()),
                    ) {
                        Err(e) => self.add_err = e,
                        Ok(p) => {
                            let name = p.name.clone();
                            let kind_label = p.kind.as_str();
                            self.cfg.projects.push(p);
                            self.sessions.push(None);
                            match persist_add(&self.cfg.projects[self.cfg.projects.len() - 1]) {
                                Err(e) => {
                                    self.cfg.projects.pop();
                                    self.sessions.pop();
                                    self.order = display_order(&self.cfg.projects);
                                    self.selected = 0;
                                    self.add_err = format!("Save failed: {e:#}");
                                }
                                Ok(()) => {
                                    self.order = display_order(&self.cfg.projects);
                                    self.selected = self
                                        .order
                                        .iter()
                                        .position(|&x| x == self.cfg.projects.len() - 1)
                                        .unwrap_or(0);
                                    self.mode = Mode::Manager;
                                    self.message =
                                        (format!("Registered {name} ({kind_label})"), Notice::Info);
                                }
                            }
                        }
                    }
                }
            }
            KeyCode::Tab => self.add_field = (self.add_field + 1) % 3,
            KeyCode::BackTab | KeyCode::Up => self.add_field = (self.add_field + 2) % 3,
            KeyCode::Down => self.add_field = (self.add_field + 1) % 3,
            KeyCode::Backspace => {
                match self.add_field {
                    0 => {
                        self.add_path.backspace();
                    }
                    2 => {
                        self.add_cmd.backspace();
                    }
                    _ => {}
                }
                self.add_err.clear();
            }
            KeyCode::Delete => {
                match self.add_field {
                    0 => {
                        self.add_path.delete();
                    }
                    2 => {
                        self.add_cmd.delete();
                    }
                    _ => {}
                }
                self.add_err.clear();
            }
            KeyCode::Left => match self.add_field {
                0 => self.add_path.move_left(),
                1 => {
                    self.add_kind = self.add_kind.toggled();
                }
                2 => self.add_cmd.move_left(),
                _ => {}
            },
            KeyCode::Right => match self.add_field {
                0 => self.add_path.move_right(),
                1 => {
                    self.add_kind = self.add_kind.toggled();
                }
                2 => self.add_cmd.move_right(),
                _ => {}
            },
            KeyCode::Home => match self.add_field {
                0 => self.add_path.move_home(),
                2 => self.add_cmd.move_home(),
                _ => {}
            },
            KeyCode::End => match self.add_field {
                0 => self.add_path.move_end(),
                2 => self.add_cmd.move_end(),
                _ => {}
            },
            KeyCode::Char(' ') if self.add_field == 1 => {
                self.add_kind = self.add_kind.toggled();
            }
            KeyCode::Char(ch)
                if !k.modifiers.contains(KeyModifiers::CONTROL)
                    && !k.modifiers.contains(KeyModifiers::ALT) =>
            {
                match self.add_field {
                    0 => self.add_path.insert(ch),
                    2 => self.add_cmd.insert(ch),
                    _ => {}
                }
                self.add_err.clear();
            }
            _ => {}
        }
    }

    fn key_help(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?') => self.mode = Mode::Manager,
            _ => {}
        }
    }

    fn key_confirm_update(&mut self, k: KeyEvent) {
        match update_key(k.code) {
            Some(true) => {
                self.confirm_update = false;
                if let Some(tag) = self.update_offer.take() {
                    self.updating = true;
                    self.message = (format!("Updating to {tag}..."), Notice::Info);
                    let tx = self.upd_tx.clone();
                    thread::spawn(move || {
                        let m = match install_update(&tag) {
                            Ok(()) => UpdateMsg::Installed(tag),
                            Err(e) => UpdateMsg::Failed(format!("{e:#}")),
                        };
                        let _ = tx.send(m);
                    });
                }
            }
            Some(false) => {
                self.confirm_update = false;
                self.update_offer = None;
                self.message = (
                    "Update skipped; squad asks again next launch.".to_string(),
                    Notice::Info,
                );
            }
            None => {}
        }
    }

    fn key_confirm_quit(&mut self, k: KeyEvent) {
        match confirm_key(k.code) {
            Some(true) => {
                self.quit = true;
            }
            Some(false) => self.confirm_quit = false,
            None => {}
        }
    }

    fn key_confirm_start_all(&mut self, k: KeyEvent, rows: u16, cols: u16) {
        match confirm_key(k.code) {
            Some(true) => {
                // Spawned in parallel: one slow start no longer
                // stalls the others (or the keyboard).
                let (started_n, failed) = start_sessions(
                    &mut self.sessions,
                    &self.cfg.projects,
                    &self.order,
                    rows,
                    cols,
                );
                self.message = match failed {
                    Some(e) => (format!("Started {started_n}; error: {e}"), Notice::Error),
                    None if started_n == 0 => {
                        ("All sessions already running.".to_string(), Notice::Info)
                    }
                    None => (
                        format!("Started {}.", plural_sessions(started_n)),
                        Notice::Info,
                    ),
                };
                self.confirm_start_all = false;
            }
            Some(false) => {
                self.confirm_start_all = false;
                self.message = ("Cancelled.".to_string(), Notice::Info);
            }
            None => {}
        }
    }

    fn key_confirm_stop(&mut self, k: KeyEvent) {
        match confirm_key(k.code) {
            Some(true) => {
                if let Some(pi) = self.confirm_stop.take() {
                    if let Some(slot) = self.sessions.get_mut(pi) {
                        if let Some(old) = slot.take() {
                            thread::spawn(move || drop(old));
                        }
                    }
                    self.message = (
                        "Session stopped. Enter restarts it.".to_string(),
                        Notice::Warn,
                    );
                }
            }
            Some(false) => {
                self.confirm_stop = None;
            }
            None => {}
        }
    }

    fn key_confirm_restart(&mut self, k: KeyEvent, rows: u16, cols: u16) {
        match confirm_key(k.code) {
            Some(true) => {
                if let Some(pi) = self.confirm_restart.take() {
                    self.message = match restart_session(
                        &mut self.sessions,
                        &self.cfg.projects,
                        pi,
                        rows,
                        cols,
                    ) {
                        None => (
                            format!("Restarted {}", self.cfg.projects[pi].name),
                            Notice::Info,
                        ),
                        Some(e) => (e, Notice::Error),
                    };
                }
            }
            Some(false) => self.confirm_restart = None,
            None => {}
        }
    }

    fn key_confirm_remove(&mut self, k: KeyEvent) {
        match confirm_key(k.code) {
            Some(true) => {
                if let Some(pi) = self.confirm_remove.take() {
                    // Save first: a failed save leaves the project and
                    // its running session untouched.
                    let name = self.cfg.projects[pi].name.clone();
                    match persist_remove(&name) {
                        Ok(()) => {
                            if let Some(Some(old)) = self.sessions.get_mut(pi).map(|o| o.take()) {
                                thread::spawn(move || drop(old));
                            }
                            remove_project(&mut self.cfg.projects, &mut self.sessions, pi);
                            // Keep an active filter across the removal.
                            self.order =
                                apply_filter(&self.cfg.projects, &self.filter_buf.as_str());
                            self.selected = self.selected.min(self.order.len().saturating_sub(1));
                            self.message = (format!("Removed {name}."), Notice::Warn);
                        }
                        Err(e) => {
                            self.message = (format!("Remove failed: {e:#}"), Notice::Error);
                        }
                    }
                }
            }
            Some(false) => {
                self.confirm_remove = None;
            }
            None => {}
        }
    }

    fn key_manager(&mut self, k: KeyEvent, rows: u16, cols: u16) {
        match k.code {
            KeyCode::Char('q') | KeyCode::Char('c')
                if k.code == KeyCode::Char('q') || k.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                if self.updating {
                    // Quitting mid-swap could leave no squad.exe behind.
                    self.message = (
                        "Update in progress; quit once it finishes.".to_string(),
                        Notice::Warn,
                    );
                } else if live_session_count(&self.sessions) > 0 {
                    self.confirm_quit = true;
                } else {
                    self.quit = true;
                }
            }
            // `n`/`p` stay bound for muscle memory; `j`/`k` and arrows are documented.
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('n') if !self.order.is_empty() => {
                self.selected = (self.selected + 1) % self.order.len()
            }
            KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('p') if !self.order.is_empty() => {
                self.selected = (self.selected + self.order.len() - 1) % self.order.len()
            }
            KeyCode::Char('m') if !self.order.is_empty() => {
                self.mode = Mode::Send;
                self.send_buf.clear();
            }
            KeyCode::Char('?') => self.mode = Mode::Help,
            // Open the update prompt on demand; it also pops up by
            // itself whenever the manager has the keyboard.
            KeyCode::Char('U') if self.update_offer.is_some() && !self.updating => {
                self.confirm_update = true;
            }
            KeyCode::Char('a') => self.open_add(),
            KeyCode::Char(c) if c.is_ascii_digit() => {
                if let Some(i) = jump_to(self.order.len(), c) {
                    self.selected = i;
                }
            }
            // Reopening keeps the current text so a filter can be refined,
            // and works even when it matched nothing.
            KeyCode::Char('/') if !self.cfg.projects.is_empty() => {
                self.mode = Mode::Filter;
            }
            // Esc in the manager drops a kept filter, or dismisses exit banner.
            KeyCode::Esc if !self.filter_buf.is_empty() => {
                self.filter_buf.clear();
                self.order = display_order(&self.cfg.projects);
                self.selected = 0;
            }
            KeyCode::Esc => {
                if let Some(&pi) = self.order.get(self.selected) {
                    if let Some(Some(s)) = self.sessions.get_mut(pi) {
                        if !s.status.is_running() {
                            s.dismissed_exit = true;
                        }
                    }
                }
            }
            KeyCode::Home if !self.order.is_empty() => self.selected = 0,
            KeyCode::End if !self.order.is_empty() => self.selected = self.order.len() - 1,
            KeyCode::PageUp if k.modifiers.contains(KeyModifiers::SHIFT) => {
                scroll_selected(
                    &mut self.sessions,
                    &self.order,
                    self.selected,
                    rows as usize,
                    1,
                );
            }
            KeyCode::PageDown if k.modifiers.contains(KeyModifiers::SHIFT) => {
                scroll_selected(
                    &mut self.sessions,
                    &self.order,
                    self.selected,
                    rows as usize,
                    -1,
                );
            }
            KeyCode::End if k.modifiers.contains(KeyModifiers::SHIFT) => {
                scroll_live_selected(&mut self.sessions, &self.order, self.selected);
            }
            KeyCode::PageUp if !self.order.is_empty() => {
                scroll_selected(
                    &mut self.sessions,
                    &self.order,
                    self.selected,
                    rows as usize,
                    1,
                );
            }
            KeyCode::PageDown if !self.order.is_empty() => {
                scroll_selected(
                    &mut self.sessions,
                    &self.order,
                    self.selected,
                    rows as usize,
                    -1,
                );
            }
            KeyCode::Char('r') => {
                if let Some(&pi) = self.order.get(self.selected) {
                    if slot_running(&self.sessions, pi) {
                        self.confirm_restart = Some(pi);
                    } else {
                        match restart_session(
                            &mut self.sessions,
                            &self.cfg.projects,
                            pi,
                            rows,
                            cols,
                        ) {
                            None => {
                                self.message = (
                                    format!("Restarted {}", self.cfg.projects[pi].name),
                                    Notice::Info,
                                );
                            }
                            Some(e) => self.message = (e, Notice::Error),
                        }
                    }
                }
            }
            KeyCode::Char('S') if !self.order.is_empty() => {
                let stopped_count = self.stopped_count();
                if stopped_count == 0 {
                    self.message = ("All sessions already running.".to_string(), Notice::Info);
                } else {
                    self.confirm_start_all = true;
                }
            }
            // `x` stays bound to stop for muscle memory; `s` is the documented key.
            KeyCode::Char('s') | KeyCode::Char('x') => {
                let target = stop_target(&self.order, self.selected, &self.sessions);
                let is_running = target
                    .and_then(|pi| self.sessions.get(pi))
                    .and_then(|o| o.as_ref())
                    .is_some_and(|s| s.status.is_running());
                if is_running {
                    self.confirm_stop = target;
                } else if let Some(pi) = target {
                    if let Some(Some(s)) = self.sessions.get_mut(pi) {
                        s.dismissed_exit = true;
                    }
                }
            }
            KeyCode::Delete | KeyCode::Char('d') => {
                self.confirm_remove = self.order.get(self.selected).copied();
            }
            KeyCode::Enter => {
                if self.order.is_empty() {
                    self.open_add();
                } else if let Some(&pi) = self.order.get(self.selected) {
                    if let Some(e) =
                        start_session(&mut self.sessions, &self.cfg.projects, pi, rows, cols)
                    {
                        self.message = (e, Notice::Error);
                        return;
                    }
                    self.mode = Mode::Focus;
                }
            }
            _ => {}
        }
    }
}

pub fn run(c: Config, initial: Option<usize>) -> Result<()> {
    if !io::stdin().is_terminal() {
        bail!("squad needs an interactive terminal");
    }
    enable_raw_mode()?;
    if let Some(lines) = c.scrollback {
        set_scrollback_limit(lines);
    }
    let _guard = ScreenGuard;
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        crossterm::cursor::Hide
    )?;
    let stdout = io::BufWriter::new(io::stdout());
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    let mut app = App::new(c, initial);
    loop {
        let area = terminal.size()?;
        let (rows, cols) = pane_size(area);
        app.animate();
        app.start_initial(rows, cols);
        app.pump();
        if app.dirty {
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
    shutdown_all(&mut app.sessions);
    Ok(())
}
