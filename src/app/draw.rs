//! Drawing: sidebar, terminal pane, status bar and popups.

use super::*;

/// A bordered, centered popup with a bold colored title.
fn modal(f: &mut Frame, title: &str, color: Color, lines: Vec<Line<'static>>) {
    let popup = modal_rect(&lines, f.area());
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

/// The scrollbar on the right border of `area`: `len` rows, now at `pos`.
fn scrollbar(f: &mut Frame, area: Rect, len: usize, pos: usize) {
    f.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(Some("░"))
            .thumb_symbol("█"),
        area,
        &mut ScrollbarState::new(len).position(pos),
    );
}

/// Folder rows the add form shows at once; the list scrolls past this.
const PICK_ROWS: usize = 6;

impl App {
    pub(super) fn draw(&mut self, f: &mut Frame) {
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
            || self.history.is_some()
            || self.procs.is_some()
            || self.confirm_stop.is_some()
            || self.confirm_remove.is_some()
            || self.confirm_restart.is_some()
            || self.confirm_update
            || self.confirm_apply
            || self.confirm_quit
            || self.confirm_start_all
            || self.context_menu.is_some();
        if has_modal {
            let full_area = f.area();
            dim_area(f.buffer_mut(), full_area);
        }
        if self.mode == Mode::Help {
            self.draw_help(f);
        }
        self.draw_history(f);
        self.draw_procs(f);
        self.draw_confirms(f);
        self.draw_context_menu(f);
        if self.mode == Mode::Add {
            self.draw_add(f);
        }
    }

    /// The SITREP reader: filed entries on the left, the picked one's text on
    /// the right.
    fn draw_history(&self, f: &mut Frame) {
        let Some(h) = &self.history else {
            return;
        };
        let popup = history_popup(f.area());
        f.render_widget(ratatui::widgets::Clear, popup);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(Line::from(Span::styled(
                format!(
                    "History: {} ({}) ↑↓/click pick · PgUp/PgDn/wheel scroll · Esc closes",
                    h.project,
                    h.items.len()
                ),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            )));
        f.render_widget(block, popup);
        let (left_col, right_col) = history_cols(popup);
        let rows = usize::from(left_col.height).max(1);
        let start = keep_visible(h.top.get(), h.sel, rows);
        h.top.set(start);
        let list: Vec<Line> = h
            .items
            .iter()
            .enumerate()
            .skip(start)
            .take(rows)
            .map(|(i, (name, _))| {
                let style = if i == h.sel {
                    Style::default().bg(Color::Cyan).fg(Color::Black)
                } else {
                    Style::default().fg(Color::Cyan)
                };
                Line::from(Span::styled(crate::ledger::sitrep_label(name), style))
            })
            .collect();
        f.render_widget(Paragraph::new(list), left_col);
        h.wrap_w.set(right_col.width.saturating_sub(1)); // less the left border
        let scroll = h.scroll.min(h.max_scroll());
        f.render_widget(
            Paragraph::new(h.text.as_str())
                .wrap(ratatui::widgets::Wrap { trim: false })
                .scroll((scroll, 0))
                .block(Block::default().borders(Borders::LEFT)),
            right_col,
        );
        let line_count = h.rows();
        if line_count > usize::from(right_col.height) {
            scrollbar(f, popup, line_count, usize::from(scroll));
        }
    }

    /// The memory monitor: biggest processes first, the picked row marked.
    fn draw_procs(&self, f: &mut Frame) {
        let Some(p) = &self.procs else {
            return;
        };
        let popup = procs_popup(f.area());
        f.render_widget(ratatui::widgets::Clear, popup);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(Line::from(Span::styled(
                "Memory: biggest processes ↑↓/PgUp/PgDn pick · x kill · r refresh · Esc closes",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            )));
        let inner = block.inner(popup);
        f.render_widget(block, popup);
        if inner.height == 0 {
            return;
        }
        // Detail box for the picked process, then the footer row that carries
        // the kill question so it never hides in the list.
        let detail_h: u16 = if inner.height >= 12 { 5 } else { 0 };
        let parts = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(detail_h),
                Constraint::Length(1),
            ])
            .split(inner);
        let rows = usize::from(parts[0].height).max(1);
        let start = p.top.get().min(p.sel).max((p.sel + 1).saturating_sub(rows));
        p.top.set(start);
        let name_w = usize::from(inner.width).saturating_sub(18);
        let list: Vec<Line> = p
            .items
            .iter()
            .enumerate()
            .skip(start)
            .take(rows)
            .map(|(i, it)| {
                let owner_tag = self
                    .proc_owner(it)
                    .map(|s| format!(" [{s}]"))
                    .unwrap_or_default();
                let name = take_cols(it.name.chars(), name_w.saturating_sub(width(&owner_tag)));
                let style = if i == p.sel {
                    Style::default().bg(Color::Cyan).fg(Color::Black)
                } else {
                    Style::default().fg(Color::Cyan)
                };
                let mut spans = vec![Span::styled(
                    format!("{:>7}  {:>8}  {name}", mem_label(it.mem), it.pid),
                    style,
                )];
                if !owner_tag.is_empty() {
                    let tag_style = if i == p.sel {
                        style.add_modifier(ratatui::style::Modifier::BOLD)
                    } else {
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(ratatui::style::Modifier::BOLD)
                    };
                    spans.push(Span::styled(owner_tag, tag_style));
                }
                Line::from(spans)
            })
            .collect();
        f.render_widget(Paragraph::new(list), parts[0]);
        if p.items.len() > rows {
            scrollbar(f, popup, p.items.len(), p.sel);
        }
        if let (true, Some(it)) = (detail_h > 0, p.items.get(p.sel)) {
            let owner = match self.proc_owner(it) {
                Some(name) => format!("squad session: {name}"),
                None => "not a squad session".to_string(),
            };
            let parent = if it.parent.is_empty() {
                it.ppid.to_string()
            } else {
                format!("{} ({})", it.ppid, it.parent)
            };
            let cmd = match &p.cmd {
                Some((pid, Some(c))) if *pid == it.pid => c.as_str(),
                _ => "(command line unavailable)",
            };
            let text = vec![
                Line::from(format!("{} · PID {} · parent {parent}", it.name, it.pid)),
                Line::from(format!(
                    "RAM {} · CPU time {} · {owner}",
                    mem_label(it.mem),
                    uptime_label(it.cpu_secs)
                )),
                Line::from(Span::styled(
                    cmd.to_string(),
                    Style::default().fg(Color::DarkGray),
                )),
            ];
            f.render_widget(
                Paragraph::new(text)
                    .wrap(ratatui::widgets::Wrap { trim: false })
                    .block(Block::default().borders(Borders::TOP)),
                parts[1],
            );
        }
        let foot = parts[2];
        let line = match &p.confirm {
            Some(t) => {
                let mut spans = vec![Span::styled(
                    format!("Kill {} ({}) and its children?  ", t.name, t.pid),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                )];
                spans.extend(hint_spans(&[
                    ("y / Enter", "kills"),
                    ("n / Esc", "cancels"),
                ]));
                Line::from(spans)
            }
            None => Line::from(Span::styled(
                format!("{} processes by RAM (working set)", p.items.len()),
                Style::default().fg(Color::DarkGray),
            )),
        };
        f.render_widget(Paragraph::new(line), foot);
    }

    pub(super) fn draw_context_menu(&self, f: &mut Frame) {
        if let Some(pi) = self.context_menu {
            if let Some(p) = self.cfg.projects.get(pi) {
                let lines = context_menu_lines(&p.name, slot_running(&self.sessions, pi));
                modal(f, "Actions", Color::Cyan, lines);
            }
        }
    }

    pub(super) fn draw_sidebar(&mut self, f: &mut Frame, panel: Rect) {
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
                let tree = rank_prefix(&self.cfg.projects, pi);
                let pin = if p.pinned { "*" } else { "" };
                let shown = format!("{tree}{}{pin}", live_name(p, fg));
                let shown_name: &str = &shown;
                let mut detail = String::new();
                if status == "Running" {
                    if let Some(s) = self.sessions[pi].as_ref() {
                        if let Some(started) = s.started {
                            detail.push(' ');
                            detail.push_str(&uptime_label(started.elapsed().as_secs()));
                        }
                        if p.kind == Kind::Agent {
                            let quiet = s.last_output.map(|t| t.elapsed().as_secs());
                            if let Some(note) = idle_note(s.phase(), quiet) {
                                detail.push_str(" · ");
                                detail.push_str(&note);
                            }
                        }
                        if let Some(u) = s.usage {
                            detail.push_str(" · ");
                            detail.push_str(&usage_label(u));
                            if over_mem_limit(u, self.cfg.mem_limit_bytes()) {
                                detail.push('!');
                            }
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
                // Waiting agents get a yellow bar and name so they stand out
                // at a glance, even in rows that are not selected.
                let needs_input = agent_phase == Some(AgentPhase::NeedsInput) && Some(pi) != sel_pi;
                let mark = if needs_input { "▎" } else { mark };
                let mark_style = if needs_input {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(ratatui::style::Modifier::BOLD)
                } else {
                    row_style
                };
                // The selected row is one reversed bar: a colored glyph here
                // would turn its cell into a differently colored block, so
                // the glyph shape alone says what state it is in.
                let status_span_style = if Some(pi) == sel_pi {
                    row_style
                } else {
                    let (base_fg, is_bold) = if let Some(ph) = agent_phase {
                        match ph {
                            AgentPhase::Working => (Color::Cyan, true),
                            AgentPhase::NeedsInput => (Color::Yellow, true),
                            AgentPhase::Done => (Color::Green, true),
                            AgentPhase::Idle => (Color::DarkGray, false),
                        }
                    } else if status == "Running" {
                        (Color::Green, true)
                    } else if status.starts_with("Error")
                        || (status.starts_with("Exited") && !status.ends_with("(0)"))
                    {
                        (Color::Red, true)
                    } else {
                        (Color::DarkGray, false)
                    };

                    let mut st = Style::default().fg(base_fg);
                    if is_bold {
                        st = st.add_modifier(ratatui::style::Modifier::BOLD);
                    }
                    st
                };
                let detail_style = if Some(pi) == sel_pi {
                    row_style
                } else {
                    Style::default().add_modifier(ratatui::style::Modifier::DIM)
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
                    width(&status_full),
                    width(shown_name),
                );
                if Some(pi) == sel_pi {
                    sel_row = Some(items.len());
                }
                targets.push(Some(n - 1));
                // Tree lines recede; the star and the name keep the row's colors.
                let name_fit = fit(shown_name, name_w);
                let lines_w = tree.chars().take_while(|&c| c != '★').count();
                let split = name_fit
                    .char_indices()
                    .nth(lines_w)
                    .map_or(name_fit.len(), |(at, _)| at);
                let (name_head, name_tail) =
                    (name_fit[..split].to_string(), name_fit[split..].to_string());
                let tree_style = if Some(pi) == sel_pi {
                    row_style
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                let row = ListItem::new(Line::from(vec![
                    Span::styled(mark, mark_style),
                    Span::styled(format!("{n} "), row_style),
                    Span::styled(fit(&status_full, status_w), status_span_style),
                    Span::raw(" "),
                    Span::styled(name_head, tree_style),
                    Span::styled(
                        name_tail,
                        if needs_input {
                            mark_style
                        } else {
                            row_name_style(Some(pi) == sel_pi, status, p.kind)
                        },
                    ),
                    Span::styled(fit(&detail, detail_w), detail_style),
                ]));
                // Selection is a full-width reversed bar, not just the text.
                items.push(if Some(pi) == sel_pi {
                    row.style(selected_row_style())
                } else {
                    row
                });
                // What a waiting agent is asking, so it can be answered
                // without opening the session first.
                if let Some(ask) = self.sessions[pi]
                    .as_ref()
                    .filter(|_| agent_phase == Some(AgentPhase::NeedsInput))
                    .and_then(|s| s.prompt_hint.as_deref())
                {
                    targets.push(Some(n - 1));
                    items.push(ListItem::new(Line::from(vec![
                        Span::raw("   "),
                        Span::styled(
                            fit(ask, inner_w.saturating_sub(3)),
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(ratatui::style::Modifier::DIM),
                        ),
                    ])));
                }
            }
            if !any {
                targets.push(None);
                items.push(
                    ListItem::new("  (none)")
                        .style(Style::default().add_modifier(ratatui::style::Modifier::DIM)),
                );
            }
        }
        items.push(ListItem::new(Line::from(Span::styled(
            "Management",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(ratatui::style::Modifier::BOLD),
        ))));
        targets.push(None);
        let memory_style = if self.procs.is_some() {
            selected_row_style()
        } else {
            Style::default()
        };
        items.push(
            ListItem::new(Line::from(vec![
                Span::raw("  Memory monitor "),
                Span::styled(
                    "M",
                    Style::default().add_modifier(ratatui::style::Modifier::DIM),
                ),
            ]))
            .style(memory_style),
        );
        targets.push(Some(MEMORY_ROW));
        let total_items = items.len();
        let visible_rows = (panel.height as usize).saturating_sub(2).max(1);
        if let Some(sr) = sel_row {
            // Keep a prompt sub-row below the selection inside the viewport,
            // and bring the group header back into view when scrolling up.
            let has_prompt_sub = targets.get(sr + 1) == targets.get(sr) && targets[sr].is_some();
            if has_prompt_sub {
                self.list_offset = keep_visible(self.list_offset, sr + 1, visible_rows);
            }
            let top_target = if self.selected == 0 {
                0
            } else if sr > 0 && targets[sr - 1].is_none() {
                sr - 1
            } else {
                sr
            };
            self.list_offset = keep_visible(self.list_offset, top_target, visible_rows);
            self.list_offset = keep_visible(self.list_offset, sr, visible_rows);
        }
        self.list_offset = self.list_offset.min(total_items.saturating_sub(1));
        self.row_targets = targets;
        let mut window = items.split_off(self.list_offset);
        window.truncate(visible_rows);
        let mut block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(sidebar_border_style(self.mode == Mode::Focus))
            .title(list_title(self.cfg.projects.len(), self.order.len()));
        if !self.filter_buf.is_empty() && panel.width >= 28 {
            let q = fit(&self.filter_buf.as_str(), 8);
            block = block.title_top(
                Line::from(Span::styled(
                    format!(" /{q} "),
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ))
                .right_aligned(),
            );
        }
        f.render_widget(List::new(window).block(block), panel);
        if total_items > visible_rows {
            scrollbar(f, panel, total_items, sel_row.unwrap_or(self.list_offset));
        }
    }

    pub(super) fn draw_pane(&self, f: &mut Frame, panel: Rect) {
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
            .map(pane_usage_note);
        let scroll_note = sel_pi
            .and_then(|pi| self.sessions.get(pi))
            .and_then(|o| o.as_ref())
            .map(|s| s.parser.screen().scrollback())
            .filter(|&n| n > 0)
            .map(|n| format!("history {n}"));
        // The mode shows in the border color and the status-bar chip, so the
        // title keeps only what the session is and how it is doing.
        let title = sel_pi
            .and_then(|pi| self.cfg.projects.get(pi))
            .map(|p| {
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
                    Span::raw("]"),
                ])
            })
            .unwrap_or_else(|| {
                if !self.cfg.projects.is_empty() && !self.filter_buf.is_empty() {
                    Line::from(vec![
                        Span::styled(
                            format!("Filter: \"{}\"", self.filter_buf.as_str()),
                            Style::default().add_modifier(ratatui::style::Modifier::BOLD),
                        ),
                        Span::raw(" ["),
                        Span::styled(
                            "0 matches",
                            Style::default()
                                .fg(Color::Magenta)
                                .add_modifier(ratatui::style::Modifier::BOLD),
                        ),
                        Span::raw("]"),
                    ])
                } else {
                    Line::from("No projects: squad add <path>")
                }
            });
        let mut block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(pane_border_style(self.mode == Mode::Focus))
            .title(title);
        // Usage and scroll position ride on the right edge; on a narrow pane
        // they would collide with the title, so they step aside.
        let right_note = [usage_note, scroll_note]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        if !right_note.is_empty() && panel.width >= 60 {
            block = block.title_top(
                Line::from(Span::styled(
                    format!(" {right_note} "),
                    Style::default().add_modifier(ratatui::style::Modifier::DIM),
                ))
                .right_aligned(),
            );
        }
        let inner = block.inner(panel);
        f.render_widget(block, panel);
        if let Some(s) = sel_pi.and_then(|pi| self.sessions.get(pi).and_then(|o| o.as_ref())) {
            let screen = s.parser.screen();
            s.paint(f.buffer_mut(), inner);
            if let Some(sel) = self.sel.filter(|x| x.moved && Some(x.pi) == sel_pi) {
                // Drawn over the cached paint, so selecting never rebuilds it.
                let hit = Style::default().bg(Color::Blue).fg(Color::White);
                for row in 0..inner.height {
                    for col in 0..inner.width {
                        if sel.contains(row, col) {
                            if let Some(cell) =
                                f.buffer_mut().cell_mut((inner.x + col, inner.y + row))
                            {
                                cell.set_style(hit);
                            }
                        }
                    }
                }
            }
            let scrollback_len = screen.scrollback_len();
            if scrollback_len > 0 {
                scrollbar(
                    f,
                    panel,
                    scrollback_len,
                    scrollback_len.saturating_sub(s.scroll),
                );
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
                            " Enter/r restart · Esc dismiss ",
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
            let body = if !self.cfg.projects.is_empty() && self.order.is_empty() {
                filtered_empty_state_lines(&self.filter_buf.as_str())
            } else {
                empty_state_lines(!self.cfg.projects.is_empty())
            };
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

    pub(super) fn draw_status(&self, f: &mut Frame, bar_area: Rect) {
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
            let label = match self.send_to {
                SendTo::One => format!("Send to {name}: "),
                SendTo::All => format!("Order to all under {name}: "),
                SendTo::Under => format!("{name} reports to (empty clears): "),
            };
            bar.extend(input_line(
                label,
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

    pub(super) fn draw_help(&self, f: &mut Frame) {
        // Fixed-size so every row fits on short terminals (a 50%
        // popup clipped the last help rows at 24 lines).
        let popup = help_popup(f.area());
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

    /// The confirmation popups that are open, in the order a click outside
    /// them dismisses.
    pub(super) fn confirm_modals(&self) -> Vec<(&'static str, Color, Vec<Line<'static>>)> {
        let name_of = |pi: usize| self.cfg.projects.get(pi).map_or("?", |p| p.name.as_str());
        let mut v = Vec::new();
        if let Some(pi) = self.confirm_stop {
            v.push(("Stop session", Color::Red, confirm_stop_lines(name_of(pi))));
        }
        if let Some(pi) = self.confirm_remove {
            v.push((
                "Remove project",
                Color::Red,
                confirm_remove_lines(name_of(pi)),
            ));
        }
        if let Some(pi) = self.confirm_restart {
            v.push((
                "Restart session",
                Color::Red,
                confirm_restart_lines(name_of(pi)),
            ));
        }
        if self.confirm_quit {
            let n = live_session_count(&self.sessions);
            v.push(("Quit", Color::Red, confirm_quit_lines(n)));
        }
        if self.confirm_start_all {
            let lines = confirm_start_all_lines(self.stopped_count());
            v.push(("Start all sessions", Color::Cyan, lines));
        }
        if self.confirm_update {
            let tag = self.update_offer.as_deref().unwrap_or("?");
            v.push(("Update available", Color::Yellow, confirm_update_lines(tag)));
        }
        if self.confirm_apply {
            let tag = self.applied.as_deref().unwrap_or("?");
            let n = live_session_count(&self.sessions);
            v.push((
                "Update installed",
                Color::Green,
                confirm_apply_lines(tag, n),
            ));
        }
        v
    }

    pub(super) fn draw_confirms(&self, f: &mut Frame) {
        for (title, color, lines) in self.confirm_modals() {
            modal(f, title, color, lines);
        }
    }

    /// The name the add form would register, shown while typing. An edit
    /// shows it only when a new folder would rename the agent.
    pub(super) fn add_preview(&self) -> Option<String> {
        if let Some(old) = &self.add_edit {
            let i = self
                .cfg
                .projects
                .iter()
                .position(|p| p.name.eq_ignore_ascii_case(old))?;
            if self.cfg.projects[i].kind != Kind::Agent {
                return None; // a service keeps its name
            }
            let others: Vec<Project> = self
                .cfg
                .projects
                .iter()
                .enumerate()
                .filter(|&(j, _)| j != i)
                .map(|(_, p)| p.clone())
                .collect();
            let name = preview_name(&others, &self.add_path.as_str(), Kind::Agent, &[])?;
            return (!name.eq_ignore_ascii_case(old)).then_some(name);
        }
        let parts = split_command(&self.add_cmd.as_str());
        preview_name(
            &self.cfg.projects,
            &self.add_path.as_str(),
            self.add_kind,
            &parts,
        )
    }

    /// Does the typed path exist? Shown at the right end of the Path row.
    fn add_mark(&self) -> (&'static str, Color) {
        match self.add_state {
            PathState::Folder => ("✓ folder", Color::Green),
            PathState::Missing => ("✗ not found · Enter creates", Color::Red),
            PathState::NotFolder => ("✗ not a folder", Color::Red),
            PathState::Blank => ("", Color::Reset),
        }
    }

    /// The line under the fields: the name the project would get.
    fn add_status(&self) -> Option<Line<'static>> {
        let name = self.add_preview()?;
        let text = if self.add_edit.is_some() {
            format!("  Renames to: {name}")
        } else {
            format!("  Registers as: {name}")
        };
        Some(Line::from(Span::styled(
            text,
            Style::default().add_modifier(ratatui::style::Modifier::DIM),
        )))
    }

    /// Rows the folder list takes under the Path row: a header plus
    /// `PICK_ROWS`, reserved even when fewer match so the popup never jumps.
    /// Zero when another field is active.
    pub(super) fn add_list_h(&self) -> u16 {
        if self.add_field == 0 {
            1 + PICK_ROWS as u16
        } else {
            0
        }
    }

    /// The rows of `add_picks` on screen: at most `PICK_ROWS`, scrolled so the
    /// highlighted one stays visible. Only the Path field lists folders.
    pub(super) fn add_pick_window(&self) -> std::ops::Range<usize> {
        if self.add_field != 0 {
            return 0..0;
        }
        let n = self.add_picks.len();
        let start = self
            .add_sel
            .map_or(0, |s| (s + 1).saturating_sub(PICK_ROWS));
        start..(start + PICK_ROWS).min(n)
    }

    /// The add form's popup; its height follows the rows it shows.
    pub(super) fn add_popup(&self, area: Rect) -> Rect {
        let rows = 5
            + self.add_list_h()
            + u16::from(self.add_status().is_some())
            + u16::from(!self.add_err.is_empty());
        fixed_rect(72, rows + 2, area)
    }

    /// The folder list under the Path row: a header naming the folder it lists,
    /// then the visible rows (typed prefix in bold), padded to `add_list_h`.
    fn add_list_lines(&self, modal_w: usize) -> Vec<Line<'static>> {
        let h = usize::from(self.add_list_h());
        if h == 0 {
            return Vec::new();
        }
        let dim = Style::default().add_modifier(ratatui::style::Modifier::DIM);
        let typed = self.add_path.as_str();
        let n = self.add_picks.len();
        let win = self.add_pick_window();
        let count_note = if n > PICK_ROWS && !win.is_empty() {
            format!("{}-{} of {n}", win.start + 1, win.end)
        } else {
            format!("{n}")
        };
        let header = match self.add_picks.first() {
            None => String::new(),
            Some(_) if typed.is_empty() => {
                format!("  your project folders · {count_note} · ↑↓/wheel Enter")
            }
            Some((label, full)) => {
                let dir = full.strip_suffix(label.as_str()).unwrap_or("");
                let dir = shorten_middle(dir.trim_start_matches('"'), modal_w.saturating_sub(40));
                format!("  in {dir} · {count_note} folders · ↑↓/wheel Enter")
            }
        };
        let mut out = vec![Line::from(Span::styled(header, dim))];
        let prefix = typed
            .rsplit(['\\', '/'])
            .next()
            .map_or(0, |s| s.chars().count());
        for i in self.add_pick_window() {
            let (label, _) = &self.add_picks[i];
            let on = self.add_sel == Some(i);
            let style = if on {
                Style::default().bg(Color::Cyan).fg(Color::Black)
            } else {
                Style::default().fg(Color::Cyan)
            };
            // Parent suggestions are whole paths; subfolders drop their `\`.
            let name = if typed.is_empty() {
                label.as_str()
            } else {
                label.trim_end_matches(['\\', '/'])
            };
            let head: String = name.chars().take(prefix).collect();
            let tail: String = name.chars().skip(prefix).collect();
            out.push(Line::from(vec![
                Span::styled(format!("  {} ", if on { "▸" } else { " " }), style),
                Span::styled(head, style.add_modifier(ratatui::style::Modifier::BOLD)),
                Span::styled(tail, style),
            ]));
        }
        out.resize(h, Line::from(""));
        out
    }

    pub(super) fn draw_add(&self, f: &mut Frame) {
        let popup = self.add_popup(f.area());
        let modal_w = (popup.width as usize).saturating_sub(2);
        f.render_widget(ratatui::widgets::Clear, popup);
        let editing = self.add_edit.is_some();
        let kind_hint = match (self.add_kind == Kind::Agent, editing) {
            (true, false) => "agent (Space toggles)",
            (false, false) => "service (Space toggles)",
            (true, true) => "agent (fixed)",
            (false, true) => "service (fixed)",
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
        let autostart_hint = if self.add_autostart {
            "yes (Space toggles)".to_string()
        } else {
            "no (Space toggles)".to_string()
        };
        let under_text = if editing {
            let cur = self.add_under.as_str();
            if cur.is_empty() {
                "(none · use u in manager)".to_string()
            } else {
                format!("{cur} (use u in manager)")
            }
        } else {
            self.add_under.as_str()
        };
        let fields = [
            (path_label, self.add_path.as_str()),
            ("Kind", kind_hint.to_string()),
            ("Command", cmd_text),
            ("Autostart", autostart_hint),
            ("Reports to", under_text),
        ];
        let (path_mark, path_mark_color) = self.add_mark();
        let mark_w = if path_mark.is_empty() {
            0
        } else {
            path_mark.chars().count() + 1
        };
        let mut lines = Vec::new();
        for (i, (label, val)) in fields.iter().enumerate() {
            let active = i == self.add_field;
            let locked = editing && !matches!(i, 0 | 2 | 3);
            let mark = if active { ">" } else { " " };
            let label_style = if active {
                Style::default().add_modifier(ratatui::style::Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            let label_text = format!("{mark} {label}: ");
            let label_w = label_text.chars().count();
            // The Path row keeps its right end for the ✓/✗ mark.
            let avail = modal_w.saturating_sub(label_w + if i == 0 { mark_w } else { 0 });
            let mut used = label_w;
            let mut row = vec![Span::styled(label_text, label_style)];
            let editing_field = active && matches!(i, 0 | 2 | 4);
            if editing_field {
                let input = match i {
                    0 => &self.add_path,
                    2 => &self.add_cmd,
                    _ => &self.add_under,
                };
                let (left, cur, right) = input.render_parts();
                let (left, cur, right) = input_window(&left, cur, &right, avail);
                used += width(&left) + width(&right) + width(cur.encode_utf8(&mut [0; 4])).max(1);
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
                // A path too long for its row keeps both ends.
                let shown = if i == 0 {
                    shorten_middle(val, avail)
                } else {
                    val.clone()
                };
                used += width(&shown);
                row.push(Span::styled(
                    shown,
                    if active {
                        Style::default().add_modifier(ratatui::style::Modifier::BOLD)
                    } else if locked {
                        Style::default().fg(Color::DarkGray)
                    } else {
                        Style::default()
                    },
                ));
            }
            if i == 0 && mark_w > 0 {
                let pad = modal_w.saturating_sub(used + mark_w - 1).max(1);
                row.push(Span::raw(" ".repeat(pad)));
                row.push(Span::styled(
                    path_mark,
                    Style::default().fg(path_mark_color),
                ));
            }
            lines.push(Line::from(row));
            if i == 0 {
                lines.extend(self.add_list_lines(modal_w));
            }
        }
        lines.extend(self.add_status());
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
                        if editing {
                            "Edit project: path, command, autostart (Enter save, Esc cancel)"
                        } else {
                            "Add project (Enter save, Esc cancel)"
                        },
                        Style::default()
                            .fg(Color::Blue)
                            .add_modifier(ratatui::style::Modifier::BOLD),
                    ))),
            ),
            popup,
        );
    }

    // ---- input ----
}
