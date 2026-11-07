use anyhow::{bail, Result};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        MouseButton, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation,
        ScrollbarState,
    },
    Terminal,
};
use std::{
    env,
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
pub fn run(mut c: Config, initial: Option<usize>) -> Result<()> {
    if !io::stdin().is_terminal() {
        bail!("squad needs an interactive terminal");
    }
    enable_raw_mode()?;
    let _guard = ScreenGuard;
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        crossterm::cursor::Hide
    )?;
    let stdout = io::BufWriter::new(io::stdout());
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    let mut sessions: Vec<Option<Session>> = (0..c.projects.len()).map(|_| None).collect();
    let mut order = display_order(&c.projects);
    let mut selected = initial
        .and_then(|i| order.iter().position(|&x| x == i))
        .unwrap_or(0);
    let mut list_offset = 0usize;
    // Last drawn sidebar geometry, so mouse clicks can be mapped to rows:
    // `row_targets[i]` is the position in `order` of list item `i` (None for headers).
    let mut side_rect = ratatui::layout::Rect::default();
    let mut row_targets: Vec<Option<usize>> = Vec::new();
    let mut pane_rect = ratatui::layout::Rect::default();
    let mut focus = false;
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
    let mut confirm_start_all = false;
    let mut message = (mode_hints(Mode::Manager).to_string(), Notice::Info);
    let mut dirty = true;
    let mut first = initial.is_some();
    let cores = thread::available_parallelism().map_or(1, |n| n.get());
    let mut sys = SysUsage::default();
    // Measuring walks the process table, so it runs on its own thread and
    // the UI only ever does a non-blocking `try_recv` for the result.
    let (req_tx, req_rx) = mpsc::channel::<Vec<(usize, u32)>>();
    let (res_tx, res_rx) = mpsc::channel::<Measurement>();
    thread::spawn(move || {
        for roots in req_rx {
            if res_tx.send(measure(&roots)).is_err() {
                break;
            }
        }
    });
    let sample_every = Duration::from_secs(2);
    // Start "overdue" so the first numbers appear immediately.
    let mut last_sample = Instant::now()
        .checked_sub(sample_every)
        .unwrap_or_else(Instant::now);
    let mut sampling = false;
    // Self-update: the check runs on a worker so it can never delay startup.
    let (upd_tx, upd_rx) = mpsc::channel::<UpdateMsg>();
    let mut update_offer: Option<String> = None;
    let mut confirm_update = false;
    let mut updating = false;
    if update_check_enabled() {
        let tx = upd_tx.clone();
        thread::spawn(move || {
            cleanup_old_binaries();
            if let Some(tag) = latest_tag().filter(|t| is_newer(env!("CARGO_PKG_VERSION"), t)) {
                let _ = tx.send(UpdateMsg::Available(tag));
            }
        });
    }
    let mut tick: u64 = 0;
    let mut last_anim_tick = Instant::now();
    loop {
        let area = terminal.size()?;
        let sidebar = sidebar_width(area.width);
        let rows = area.height.saturating_sub(3).max(1);
        let cols = area.width.saturating_sub(sidebar).saturating_sub(2).max(1);
        if !focus && last_anim_tick.elapsed() >= Duration::from_millis(200) {
            last_anim_tick = Instant::now();
            tick = tick.wrapping_add(1);
            if sessions.iter().flatten().any(|s| s.status == "Running") {
                dirty = true;
            }
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
        while let Ok(m) = res_rx.try_recv() {
            sampling = false;
            if let Some(snap) = m.sys {
                sys.apply(snap);
            }
            for (pi, pid, cpu_time, mem) in m.procs {
                if let Some(Some(s)) = sessions.get_mut(pi) {
                    s.apply_usage(pid, cpu_time, mem, m.at, cores);
                }
            }
            dirty = true;
        }
        if !sampling && last_sample.elapsed() >= sample_every {
            let mut roots = Vec::new();
            for (pi, slot) in sessions.iter_mut().enumerate() {
                if let Some(s) = slot {
                    match s.child.process_id().filter(|_| s.status == "Running") {
                        Some(pid) => roots.push((pi, pid)),
                        None => s.clear_usage(),
                    }
                }
            }
            if req_tx.send(roots).is_ok() {
                sampling = true;
            }
            last_sample = Instant::now();
        }
        while let Ok(m) = upd_rx.try_recv() {
            match m {
                UpdateMsg::Available(tag) => {
                    message = (format!("Update {tag} available"), Notice::Warn);
                    update_offer = Some(tag);
                }
                UpdateMsg::Installed(tag) => {
                    updating = false;
                    message = (
                        format!(
                            "Updated to {tag}. Restart squad to use it; sessions keep running."
                        ),
                        Notice::Info,
                    );
                }
                UpdateMsg::Failed(e) => {
                    updating = false;
                    update_offer = None;
                    message = (format!("Update failed: {e}"), Notice::Error);
                }
            }
            dirty = true;
        }
        // Ask only when nothing else owns the keyboard, so a prompt never
        // swallows keystrokes meant for a session or another input.
        let keyboard_free = !(focus
            || sending
            || filtering
            || adding
            || show_help
            || confirm_quit
            || confirm_start_all
            || confirm_stop.is_some()
            || confirm_remove.is_some());
        if keyboard_free && !updating && !confirm_update && update_offer.is_some() {
            confirm_update = true;
            dirty = true;
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
                    // Typing into a dead terminal does nothing: hand the keyboard
                    // back to the manager so Enter can restart it.
                    if focus && order.get(selected) == Some(&pi) {
                        focus = false;
                    }
                }
            }
        }
        if dirty {
            {
                let backend = terminal.backend_mut();
                let _ = backend.write_all(b"\x1b[?2026h");
            }
            terminal.draw(|f| {
                let vertical = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(1), Constraint::Length(1)])
                    .split(f.area());
                let panels = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Length(sidebar), Constraint::Min(1)])
                    .split(vertical[0]);
                side_rect = panels[0];
                pane_rect = panels[1];
                if sidebar > 0 {
                    let sel_pi = order.get(selected).copied();
                    let inner_w = (sidebar as usize).saturating_sub(2);
                    let mut items: Vec<ListItem> = Vec::new();
                    let mut targets: Vec<Option<usize>> = Vec::new();
                    let mut sel_row: Option<usize> = None;
                    let mut n = 0;
                    let running: Vec<bool> = sessions
                        .iter()
                        .map(|o| o.as_ref().map(|s| s.status == "Running").unwrap_or(false))
                        .collect();
                    for (title, k) in [("Agents", Kind::Agent), ("Services", Kind::Service)] {
                        let (up, total) = group_counts(&c.projects, &running, k);
                        items.push(ListItem::new(group_header(title, k, up, total)));
                        targets.push(None);
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
                                    if let Some(u) = s.usage {
                                        detail.push_str(" · ");
                                        detail.push_str(&usage_label(u));
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
                            let mark = if Some(pi) == sel_pi { "▎" } else { " " };
                            let row_style = if Some(pi) == sel_pi {
                                selected_row_style()
                            } else if status == "Running" {
                                Style::default().add_modifier(ratatui::style::Modifier::BOLD)
                            } else {
                                Style::default().add_modifier(ratatui::style::Modifier::DIM)
                            };
                            let agent_phase = if p.kind == Kind::Agent && status == "Running" {
                                sessions[pi].as_ref().map(|s| s.phase())
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
                                ph.icon(tick).0
                            } else if status == "Running" {
                                format!("{}", status_glyph(status, tick))
                            } else {
                                format!("{} {status}", status_glyph(status, tick))
                            };
                            let (status_w, name_w, detail_w) = row_widths(
                                inner_w,
                                prefix.chars().count(),
                                status_full.chars().count(),
                                p.name.chars().count(),
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
                                    fit(&p.name, name_w),
                                    row_name_style(Some(pi) == sel_pi, status, p.kind),
                                ),
                                Span::styled(fit(&detail, detail_w), detail_style),
                            ])));
                        }
                        if !any {
                            targets.push(None);
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
                    row_targets = targets;
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
                let usage_note = sel_pi
                    .and_then(|pi| sessions.get(pi))
                    .and_then(|o| o.as_ref())
                    .and_then(|s| s.usage)
                    .map(|u| format!(" · {}", usage_label(u)))
                    .unwrap_or_default();
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
                        let focus_label = if panels[1].width >= 60 {
                            if focus {
                                " · INPUT (Ctrl+Q to exit)"
                            } else {
                                " · PREVIEW (Enter to type)"
                            }
                        } else if focus {
                            " · INPUT"
                        } else {
                            " · MANAGER"
                        };
                        let sel_phase = sel_pi
                            .and_then(|pi| sessions.get(pi))
                            .and_then(|o| o.as_ref())
                            .map(|s| s.phase());
                        let (disp_status, disp_style) =
                            pane_status_display(sel_status, sel_uptime, tick, p.kind, sel_phase);
                        Line::from(vec![
                            Span::styled(
                                pane_title_name(p),
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
                                if focus {
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
                    .border_style(pane_border_style(focus))
                    .title(title);
                let inner = block.inner(panels[1]);
                f.render_widget(block, panels[1]);
                if let Some(s) = sel_pi.and_then(|pi| sessions.get(pi).and_then(|o| o.as_ref())) {
                    let screen = s.parser.screen();
                    let mut lines = Vec::with_capacity(inner.height as usize);
                    for row in 0..inner.height {
                        let mut spans = Vec::new();
                        let mut current_text = String::new();
                        let mut current_style: Option<Style> = None;
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
                                let ch = if cell.has_contents() {
                                    cell.contents()
                                } else {
                                    " "
                                };
                                if current_style == Some(style) {
                                    current_text.push_str(ch);
                                } else {
                                    if let Some(prev_style) = current_style {
                                        spans.push(Span::styled(
                                            std::mem::take(&mut current_text),
                                            prev_style,
                                        ));
                                    }
                                    current_style = Some(style);
                                    current_text.push_str(ch);
                                }
                            }
                        }
                        if let Some(prev_style) = current_style {
                            if !current_text.is_empty() {
                                spans.push(Span::styled(current_text, prev_style));
                            }
                        }
                        lines.push(Line::from(spans));
                    }
                    f.render_widget(Paragraph::new(lines), inner);
                    let scrollback_len = screen.scrollback_len();
                    if scrollback_len > 0 {
                        let mut scrollbar_state = ScrollbarState::new(scrollback_len)
                            .position(scrollback_len.saturating_sub(s.scroll));
                        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                            .begin_symbol(None)
                            .end_symbol(None)
                            .track_symbol(Some("░"))
                            .thumb_symbol("█");
                        f.render_stateful_widget(scrollbar, panels[1], &mut scrollbar_state);
                    }
                    // A finished session leaves its last output on screen; say so
                    // and say how to bring it back, right where the eyes are.
                    if s.status != "Running" && !s.dismissed_exit && inner.height > 0 {
                        let bg = if needs_attention(&s.status) {
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
                            ratatui::layout::Rect::new(
                                inner.x,
                                inner.y + inner.height - 1,
                                inner.width,
                                1,
                            ),
                        );
                    }
                    if focus && s.scroll == 0 && !screen.hide_cursor() {
                        let (r, c) = screen.cursor_position();
                        if r < inner.height && c < inner.width {
                            f.set_cursor_position((inner.x + c, inner.y + r));
                        }
                    }
                } else {
                    let body = empty_state_lines(!c.projects.is_empty());
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
                // Machine load is shown only when the bar has room for it.
                let sys_text = if area.width >= 70 && sys.total > 0 {
                    sys.summary()
                } else {
                    String::new()
                };
                let sys_w = if sys_text.is_empty() {
                    0
                } else {
                    sys_text.chars().count() as u16 + 3
                };
                let status_row = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([
                        Constraint::Min(1),
                        Constraint::Length(
                            summary_width(running_n, c.projects.len(), failed_n) + sys_w,
                        ),
                    ])
                    .split(vertical[1]);
                let input_w =
                    (status_row[0].width as usize).saturating_sub(chip_text.chars().count() + 1);
                if focus {
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
                let mut right = Vec::new();
                if !sys_text.is_empty() {
                    right.push(Span::styled(sys_text, sys.style()));
                    right.push(Span::styled(
                        " · ".to_string(),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
                right.push(Span::styled(
                    fleet_summary(running_n, c.projects.len()),
                    if running_n > 0 {
                        Style::default().fg(Color::Green)
                    } else {
                        Style::default().fg(Color::DarkGray)
                    },
                ));
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
                let has_modal = show_help
                    || confirm_stop.is_some()
                    || confirm_remove.is_some()
                    || confirm_update
                    || confirm_quit
                    || confirm_start_all
                    || adding;
                if has_modal {
                    let full_area = f.area();
                    dim_area(f.buffer_mut(), full_area);
                }
                if show_help {
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
                if let Some(pi) = confirm_stop {
                    let name = c.projects.get(pi).map(|p| p.name.as_str()).unwrap_or("?");
                    let popup = centered_rect(60, 30, f.area());
                    f.render_widget(ratatui::widgets::Clear, popup);
                    f.render_widget(
                        Paragraph::new(confirm_stop_lines(name)).block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_type(BorderType::Rounded)
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
                                .border_type(BorderType::Rounded)
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
                if confirm_update {
                    let tag = update_offer.as_deref().unwrap_or("?");
                    let popup = centered_rect(60, 40, f.area());
                    f.render_widget(ratatui::widgets::Clear, popup);
                    f.render_widget(
                        Paragraph::new(confirm_update_lines(tag)).block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_type(BorderType::Rounded)
                                .title(Line::from(Span::styled(
                                    "Update available",
                                    Style::default()
                                        .fg(Color::Yellow)
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
                                .border_type(BorderType::Rounded)
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
                if confirm_start_all {
                    let stopped_count = order
                        .iter()
                        .filter(|&&pi| {
                            sessions
                                .get(pi)
                                .and_then(|o| o.as_ref())
                                .is_none_or(|s| s.status != "Running")
                        })
                        .count();
                    let popup = centered_rect(60, 30, f.area());
                    f.render_widget(ratatui::widgets::Clear, popup);
                    f.render_widget(
                        Paragraph::new(confirm_start_all_lines(stopped_count)).block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_type(BorderType::Rounded)
                                .title(Line::from(Span::styled(
                                    "Start all sessions",
                                    Style::default()
                                        .fg(Color::Cyan)
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
            })?;
            {
                let backend = terminal.backend_mut();
                let _ = backend.write_all(b"\x1b[?2026l");
                let _ = backend.flush();
            }
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
                } else if m.kind == MouseEventKind::Down(MouseButton::Left) {
                    if show_help {
                        show_help = false;
                        dirty = true;
                    } else if let Some(pi) = confirm_stop {
                        let popup = centered_rect(60, 30, area.into());
                        let clicked = ratatui::layout::Position::new(m.column, m.row);
                        if popup.contains(clicked) {
                            if let Some(slot) = sessions.get_mut(pi) {
                                if let Some(old) = slot.take() {
                                    thread::spawn(move || drop(old));
                                }
                            }
                            message = (
                                "Session stopped. Enter restarts it.".to_string(),
                                Notice::Warn,
                            );
                        }
                        confirm_stop = None;
                        dirty = true;
                    } else if let Some(pi) = confirm_remove {
                        let popup = centered_rect(60, 30, area.into());
                        let clicked = ratatui::layout::Position::new(m.column, m.row);
                        if popup.contains(clicked) {
                            if let Some(Some(old)) = sessions.get_mut(pi).map(|o| o.take()) {
                                thread::spawn(move || drop(old));
                            }
                            if let Some(removed) =
                                remove_project(&mut c.projects, &mut sessions, pi)
                            {
                                match persist_remove(&removed.name) {
                                    Ok(()) => {
                                        order = apply_filter(&c.projects, &filter_buf.as_str());
                                        selected = selected.min(order.len().saturating_sub(1));
                                        message =
                                            (format!("Removed {}.", removed.name), Notice::Warn);
                                    }
                                    Err(e) => {
                                        let at = pi.min(c.projects.len());
                                        c.projects.insert(at, removed);
                                        if at <= sessions.len() {
                                            sessions.insert(at, None);
                                        }
                                        message = (format!("Cannot save: {e:#}"), Notice::Error);
                                    }
                                }
                            }
                        }
                        confirm_remove = None;
                        dirty = true;
                    } else if confirm_quit {
                        let popup = centered_rect(60, 30, area.into());
                        let clicked = ratatui::layout::Position::new(m.column, m.row);
                        if popup.contains(clicked) {
                            break;
                        } else {
                            confirm_quit = false;
                        }
                        dirty = true;
                    } else if confirm_start_all {
                        let popup = centered_rect(60, 30, area.into());
                        let clicked = ratatui::layout::Position::new(m.column, m.row);
                        if popup.contains(clicked) {
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
                                Some(e) => {
                                    (format!("Started {started_n}; error: {e}"), Notice::Error)
                                }
                                None if started_n == 0 => {
                                    ("All sessions already running.".to_string(), Notice::Info)
                                }
                                None => (
                                    format!("Started {}.", plural_sessions(started_n)),
                                    Notice::Info,
                                ),
                            };
                        }
                        confirm_start_all = false;
                        dirty = true;
                    } else if confirm_update {
                        let popup = centered_rect(60, 40, area.into());
                        let clicked = ratatui::layout::Position::new(m.column, m.row);
                        if popup.contains(clicked) {
                            if let Some(tag) = update_offer.take() {
                                updating = true;
                                message = (format!("Updating to {tag}..."), Notice::Warn);
                                let tx = upd_tx.clone();
                                thread::spawn(move || match install_update(&tag) {
                                    Ok(()) => {
                                        let _ = tx.send(UpdateMsg::Installed(tag));
                                    }
                                    Err(e) => {
                                        let _ = tx.send(UpdateMsg::Failed(e.to_string()));
                                    }
                                });
                            }
                        }
                        confirm_update = false;
                        dirty = true;
                    } else if adding || sending || filtering {
                        // In text input modals, keep input focus
                    } else if let Some(i) =
                        sidebar_click(side_rect, list_offset, &row_targets, m.column, m.row)
                    {
                        if selected == i {
                            // Re-clicking selected project: start if stopped, focus if running
                            if let Some(&pi) = order.get(selected) {
                                let live = sessions
                                    .get(pi)
                                    .and_then(|o| o.as_ref())
                                    .is_some_and(|s| s.status == "Running");
                                if live {
                                    focus = true;
                                } else {
                                    match start_session(&mut sessions, &c.projects, pi, rows, cols)
                                    {
                                        None => focus = true,
                                        Some(e) => message = (e, Notice::Error),
                                    }
                                }
                            }
                        } else {
                            selected = i;
                            focus = false;
                        }
                        dirty = true;
                    } else if pane_rect.contains(ratatui::layout::Position::new(m.column, m.row)) {
                        // Clicking terminal pane: start if stopped, focus if running
                        if let Some(&pi) = order.get(selected) {
                            let live = sessions
                                .get(pi)
                                .and_then(|o| o.as_ref())
                                .is_some_and(|s| s.status == "Running");
                            if live {
                                focus = true;
                            } else {
                                match start_session(&mut sessions, &c.projects, pi, rows, cols) {
                                    None => focus = true,
                                    Some(e) => message = (e, Notice::Error),
                                }
                            }
                        }
                        dirty = true;
                    } else if m.row >= area.height.saturating_sub(1) && m.column < 12 {
                        // Clicking the status-bar mode chip toggles help
                        show_help = !show_help;
                        dirty = true;
                    }
                } else if m.kind == MouseEventKind::Down(MouseButton::Right)
                    && !(adding
                        || show_help
                        || sending
                        || filtering
                        || confirm_quit
                        || confirm_stop.is_some()
                        || confirm_remove.is_some()
                        || confirm_update)
                {
                    // Right-click on sidebar: quick toggle (stop if running, start if stopped)
                    if let Some(i) =
                        sidebar_click(side_rect, list_offset, &row_targets, m.column, m.row)
                    {
                        selected = i;
                        if let Some(&pi) = order.get(selected) {
                            let live = sessions
                                .get(pi)
                                .and_then(|o| o.as_ref())
                                .is_some_and(|s| s.status == "Running");
                            if live {
                                confirm_stop = Some(pi);
                            } else {
                                match start_session(&mut sessions, &c.projects, pi, rows, cols) {
                                    None => focus = true,
                                    Some(e) => message = (e, Notice::Error),
                                }
                            }
                        }
                        dirty = true;
                    }
                }
            }
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                dirty = true;
                if focus {
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
                if confirm_update {
                    match update_key(k.code) {
                        Some(true) => {
                            confirm_update = false;
                            if let Some(tag) = update_offer.take() {
                                updating = true;
                                message = (format!("Updating to {tag}..."), Notice::Info);
                                let tx = upd_tx.clone();
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
                            confirm_update = false;
                            update_offer = None;
                            message = (
                                "Update skipped; squad asks again next launch.".to_string(),
                                Notice::Info,
                            );
                        }
                        None => {}
                    }
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
                if confirm_start_all {
                    match confirm_key(k.code) {
                        Some(true) => {
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
                                Some(e) => {
                                    (format!("Started {started_n}; error: {e}"), Notice::Error)
                                }
                                None if started_n == 0 => {
                                    ("All sessions already running.".to_string(), Notice::Info)
                                }
                                None => (
                                    format!("Started {}.", plural_sessions(started_n)),
                                    Notice::Info,
                                ),
                            };
                            confirm_start_all = false;
                        }
                        Some(false) => {
                            confirm_start_all = false;
                            message = ("Cancelled.".to_string(), Notice::Info);
                        }
                        None => {}
                    }
                    continue;
                }
                if confirm_stop.is_some() {
                    match confirm_key(k.code) {
                        Some(true) => {
                            if let Some(pi) = confirm_stop.take() {
                                if let Some(slot) = sessions.get_mut(pi) {
                                    if let Some(old) = slot.take() {
                                        thread::spawn(move || drop(old));
                                    }
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
                                if let Some(Some(old)) = sessions.get_mut(pi).map(|o| o.take()) {
                                    thread::spawn(move || drop(old));
                                }
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
                    KeyCode::Char('q') | KeyCode::Char('c')
                        if k.code == KeyCode::Char('q')
                            || k.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        if live_session_count(&sessions) > 0 {
                            confirm_quit = true;
                        } else {
                            break;
                        }
                    }
                    // `n`/`p` stay bound for muscle memory; `j`/`k` and arrows are documented.
                    KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('n')
                        if !order.is_empty() =>
                    {
                        selected = (selected + 1) % order.len()
                    }
                    KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('p') if !order.is_empty() => {
                        selected = (selected + order.len() - 1) % order.len()
                    }
                    KeyCode::Char('m') if !order.is_empty() => {
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
                    // Reopening keeps the current text so a filter can be refined,
                    // and works even when it matched nothing.
                    KeyCode::Char('/') if !c.projects.is_empty() => {
                        filtering = true;
                    }
                    // Esc in the manager drops a kept filter, or dismisses exit banner.
                    KeyCode::Esc if !filter_buf.is_empty() => {
                        filter_buf.clear();
                        order = display_order(&c.projects);
                        selected = 0;
                    }
                    KeyCode::Esc => {
                        if let Some(&pi) = order.get(selected) {
                            if let Some(Some(s)) = sessions.get_mut(pi) {
                                if s.status != "Running" {
                                    s.dismissed_exit = true;
                                }
                            }
                        }
                    }
                    KeyCode::Home if !order.is_empty() => selected = 0,
                    KeyCode::End if !order.is_empty() => selected = order.len() - 1,
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
                    KeyCode::Char('S') if !order.is_empty() => {
                        let stopped_count = order
                            .iter()
                            .filter(|&&pi| {
                                sessions
                                    .get(pi)
                                    .and_then(|o| o.as_ref())
                                    .is_none_or(|s| s.status != "Running")
                            })
                            .count();
                        if stopped_count == 0 {
                            message = ("All sessions already running.".to_string(), Notice::Info);
                        } else {
                            confirm_start_all = true;
                        }
                    }
                    // `x` stays bound to stop for muscle memory; `s` is the documented key.
                    KeyCode::Char('s') | KeyCode::Char('x') => {
                        confirm_stop = stop_target(&order, selected, &sessions);
                        if confirm_stop.is_none() {
                            if let Some(&pi) = order.get(selected) {
                                if let Some(Some(s)) = sessions.get_mut(pi) {
                                    if s.status != "Running" {
                                        s.dismissed_exit = true;
                                    }
                                }
                            }
                        }
                    }
                    KeyCode::Delete | KeyCode::Char('d') => {
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
