use super::*;

impl App {
    pub(super) fn draw_git_view(&self, f: &mut Frame) {
        let Some(g) = &self.git_view else {
            return;
        };
        let popup = history_popup(f.area());
        f.render_widget(ratatui::widgets::Clear, popup);
        let status_note = if g.busy() {
            "loading…".to_string()
        } else if g.files.is_empty() {
            "clean".to_string()
        } else {
            format!("{} changed", g.files.len())
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(Line::from(vec![
                Span::styled(
                    format!(
                        " Git: {} [{}] ",
                        sanitize_chrome_line(&g.project),
                        sanitize_chrome_line(&g.branch)
                    ),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ),
                Span::styled(
                    format!("({status_note}) "),
                    Style::default().fg(if g.busy() {
                        Color::DarkGray
                    } else if g.files.is_empty() {
                        Color::Green
                    } else {
                        Color::Yellow
                    }),
                ),
            ]))
            .title_bottom(
                popup_footer_line(
                    &[
                        ("↑↓/j/k", "pick"),
                        ("PgUp/PgDn/J/K/wheel", "scroll"),
                        ("r", "refresh"),
                        ("Tab", "project"),
                        ("Esc", "closes"),
                    ],
                    usize::from(popup.width.saturating_sub(4)),
                )
                .right_aligned(),
            );
        f.render_widget(block, popup);
        let (left_col, right_col) = history_cols(popup);
        let rows = usize::from(left_col.height).max(1);
        let start = keep_visible(g.top.get(), g.sel, rows);
        g.top.set(start);
        let list: Vec<Line> = if g.files.is_empty() && g.refreshing {
            vec![Line::from(Span::styled(
                "  reading git status…",
                Style::default().fg(Color::DarkGray),
            ))]
        } else if g.files.is_empty() {
            vec![Line::from(Span::styled(
                "  (clean)",
                Style::default().fg(Color::Green),
            ))]
        } else {
            g.files
                .iter()
                .enumerate()
                .skip(start)
                .take(rows)
                .map(|(i, (code, rel))| {
                    let active = i == g.sel;
                    let code_color = if code.contains('D') {
                        Color::Red
                    } else if code.contains('A') || code == "??" {
                        Color::Green
                    } else if code.contains('R') {
                        Color::Cyan
                    } else {
                        Color::Yellow
                    };
                    let row_style = if active {
                        Style::default()
                            .bg(Color::Cyan)
                            .fg(Color::Black)
                            .add_modifier(ratatui::style::Modifier::BOLD)
                    } else {
                        Style::default()
                    };
                    let tag_style = if active {
                        row_style
                    } else {
                        Style::default()
                            .fg(code_color)
                            .add_modifier(ratatui::style::Modifier::BOLD)
                    };
                    let mark = if active { "▸" } else { " " };
                    let avail = usize::from(left_col.width).saturating_sub(5);
                    let path_s =
                        shorten_middle(&sanitize_chrome_line(&rel.display().to_string()), avail);
                    Line::from(vec![
                        Span::styled(format!("{mark}{code} "), tag_style),
                        Span::styled(path_s, row_style),
                    ])
                })
                .collect()
        };
        f.render_widget(Paragraph::new(list), left_col);
        g.wrap_w.set(right_col.width.saturating_sub(1));
        let scroll = g.scroll.min(g.max_scroll());
        let line_count = g.rows();
        let mut right_block = Block::default().borders(Borders::LEFT);
        if line_count > usize::from(right_col.height) {
            let max_s = g.max_scroll().max(1);
            let pct = (u32::from(scroll) * 100 / u32::from(max_s)).min(100);
            right_block = right_block.title_bottom(
                Line::from(Span::styled(
                    format!(" {pct}% "),
                    Style::default().add_modifier(ratatui::style::Modifier::DIM),
                ))
                .right_aligned(),
            );
        }
        f.render_widget(
            Paragraph::new(git_diff_lines(&g.diff))
                .wrap(ratatui::widgets::Wrap { trim: false })
                .scroll((scroll, 0))
                .block(right_block),
            right_col,
        );
        if line_count > usize::from(right_col.height) {
            scrollbar(f, popup, line_count, usize::from(scroll));
        }
    }

    /// Global search results (`G`): per-session match counts, newest
    /// sessions first in sidebar order; `Enter` jumps to the picked one.
    pub(super) fn draw_gsearch(&self, f: &mut Frame) {
        let Some(g) = &self.gsearch else {
            return;
        };
        let shown = g.hits.len().min(10);
        let popup = gsearch_popup(f.area(), shown.max(1));
        f.render_widget(ratatui::widgets::Clear, popup);
        let total: usize = g.hits.iter().map(|h| h.count).sum();
        let title = if g.query.is_empty() {
            " Find in all sessions ".to_string()
        } else {
            format!(" Find: {} ({} hits) ", fit(&g.query, 24), total)
        };
        let footer_hints: &[(&str, &str)] = if g.query.is_empty() {
            &[("", "type to search"), ("Esc", "closes")]
        } else {
            &[("↑↓/j/k", "pick"), ("Enter", "jump"), ("Esc", "closes")]
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(Line::from(Span::styled(
                title,
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            )))
            .title_bottom(
                popup_footer_line(footer_hints, usize::from(popup.width.saturating_sub(4)))
                    .right_aligned(),
            );
        let inner = block.inner(popup);
        f.render_widget(block, popup);
        let inner_w = usize::from(inner.width);
        let mut lines: Vec<Line> = Vec::new();
        if g.query.is_empty() {
            lines.push(Line::from(Span::styled(
                "Every session's scrollback is searched as you type.",
                Style::default().add_modifier(ratatui::style::Modifier::DIM),
            )));
        } else if g.hits.is_empty() {
            lines.push(Line::from(Span::styled(
                "No matches anywhere.",
                Style::default().fg(Color::Yellow),
            )));
        }
        for (i, hit) in g.hits.iter().take(10).enumerate() {
            let name = self
                .cfg
                .projects
                .get(hit.pi)
                .map(|p| sanitize_chrome_line(&p.name))
                .unwrap_or_else(|| "?".to_string());
            let row = format!(
                "{} — {} {}",
                fit(&name, inner_w.saturating_sub(14)),
                hit.count,
                if hit.count == 1 { "match" } else { "matches" }
            );
            let style = if i == g.sel {
                Style::default()
                    .bg(Color::Cyan)
                    .fg(Color::Black)
                    .add_modifier(ratatui::style::Modifier::BOLD)
            } else {
                Style::default()
            };
            let mark = if i == g.sel { "▸ " } else { "  " };
            lines.push(Line::from(vec![
                Span::styled(mark, style),
                Span::styled(row, style),
            ]));
        }
        if g.hits.len() > 10 {
            lines.push(Line::from(Span::styled(
                format!("…and {} more sessions", g.hits.len() - 10),
                Style::default().add_modifier(ratatui::style::Modifier::DIM),
            )));
        }
        f.render_widget(Paragraph::new(lines), inner);
    }

    pub(super) fn draw_quick_reply(&self, f: &mut Frame) {
        let Some(qr) = &self.quick_reply else {
            return;
        };
        let popup = quick_reply_popup(f.area(), qr.tail.len());
        f.render_widget(ratatui::widgets::Clear, popup);
        let name = self
            .cfg
            .projects
            .get(qr.pi)
            .map(|p| sanitize_chrome_line(&p.name))
            .unwrap_or_else(|| "?".to_string());
        let waiting = self.waiting_projects();
        let queue_note = if waiting.len() > 1 {
            let pos = waiting
                .iter()
                .position(|&w| w == qr.pi)
                .map_or(1, |i| i + 1);
            format!(" ({pos}/{} waiting)", waiting.len())
        } else {
            String::new()
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Yellow))
            .title(Line::from(Span::styled(
                format!(" Quick Reply: {name}{queue_note} "),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            )));
        let inner = block.inner(popup);
        f.render_widget(block, popup);
        if inner.height < 4 {
            return;
        }
        let parts = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(inner);
        let max_tail = usize::from(parts[0].height);
        let skip = qr.tail.len().saturating_sub(max_tail);
        let max_w = usize::from(parts[0].width);
        let tail_lines: Vec<Line> = qr
            .tail
            .iter()
            .skip(skip)
            .map(|l| {
                let fitted = fit(l, max_w);
                let style = if l.contains('?') || l.trim_start().starts_with('❯') {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(ratatui::style::Modifier::BOLD)
                } else {
                    Style::default()
                };
                Line::from(Span::styled(fitted, style))
            })
            .collect();
        f.render_widget(Paragraph::new(tail_lines), parts[0]);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "─".repeat(usize::from(parts[1].width)),
                Style::default().fg(Color::DarkGray),
            ))),
            parts[1],
        );
        if qr.typing {
            let spans = input_line(
                "Reply: ".to_string(),
                &qr.input,
                String::new(),
                usize::from(parts[2].width),
            );
            f.render_widget(Paragraph::new(Line::from(spans)), parts[2]);
            f.render_widget(
                Paragraph::new(Line::from(hint_spans(&[
                    ("Enter", "sends"),
                    ("Esc", "back"),
                ]))),
                parts[3],
            );
        } else {
            f.render_widget(Paragraph::new(quick_reply_button_line()), parts[2]);
            let mut hints = vec![("o", "focus"), ("c", "cancel"), ("Esc", "closes")];
            if waiting.len() > 1 {
                hints.insert(0, ("Tab", "next"));
            }
            f.render_widget(Paragraph::new(Line::from(hint_spans(&hints))), parts[3]);
        }
    }

    /// The SITREP reader: filed entries on the left, the picked one's text on
    /// the right.
    pub(super) fn draw_history(&self, f: &mut Frame) {
        let Some(h) = &self.history else {
            return;
        };
        let popup = history_popup(f.area());
        f.render_widget(ratatui::widgets::Clear, popup);
        let count_note = if h.query.is_empty() {
            format!(
                " {}: {} ({}) ",
                h.kind,
                sanitize_chrome_line(&h.project),
                h.items.len()
            )
        } else {
            format!(
                " {}: {} ({}/{} · /{} ) ",
                h.kind,
                sanitize_chrome_line(&h.project),
                h.view.len(),
                h.items.len(),
                h.query
            )
        };
        let footer_hints: &[(&str, &str)] = if h.typing {
            &[
                ("", "type to filter"),
                ("Enter", "keeps"),
                ("Esc", "clears"),
            ]
        } else {
            &[
                ("↑↓/j/k", "pick"),
                ("/", "filter"),
                ("PgUp/PgDn/J/K/wheel", "scroll"),
                ("Esc", "closes"),
            ]
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(Line::from(Span::styled(
                count_note,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            )))
            .title_bottom(
                popup_footer_line(footer_hints, usize::from(popup.width.saturating_sub(4)))
                    .right_aligned(),
            );
        f.render_widget(block, popup);
        let (left_col, right_col) = history_cols(popup);
        let rows = usize::from(left_col.height).max(1);
        let start = keep_visible(h.top.get(), h.sel, rows);
        h.top.set(start);
        let list: Vec<Line> = h
            .view
            .iter()
            .copied()
            .enumerate()
            .skip(start)
            .take(rows)
            .map(|(i, pi)| {
                let name = h.items.get(pi).map(|(n, _)| n.as_str()).unwrap_or("?");
                let is_auto = crate::history::is_auto(name);
                let style = if i == h.sel {
                    Style::default()
                        .bg(Color::Cyan)
                        .fg(Color::Black)
                        .add_modifier(ratatui::style::Modifier::BOLD)
                } else if is_auto {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(ratatui::style::Modifier::BOLD)
                };
                let mark = if i == h.sel { "▸ " } else { "  " };
                let label = fit(
                    &crate::ledger::sitrep_label(name, h.offset),
                    usize::from(left_col.width).saturating_sub(2),
                );
                Line::from(Span::styled(format!("{mark}{label}"), style))
            })
            .collect();
        f.render_widget(Paragraph::new(list), left_col);
        h.wrap_w.set(right_col.width.saturating_sub(1)); // less the left border
        let scroll = h.scroll.min(h.max_scroll());
        let line_count = h.rows();
        let mut right_block = Block::default().borders(Borders::LEFT);
        if line_count > usize::from(right_col.height) {
            let max_s = h.max_scroll().max(1);
            let pct = (u32::from(scroll) * 100 / u32::from(max_s)).min(100);
            right_block = right_block.title_bottom(
                Line::from(Span::styled(
                    format!(" {pct}% "),
                    Style::default().add_modifier(ratatui::style::Modifier::DIM),
                ))
                .right_aligned(),
            );
        }
        f.render_widget(
            Paragraph::new(h.text.as_str())
                .wrap(ratatui::widgets::Wrap { trim: false })
                .scroll((scroll, 0))
                .block(right_block),
            right_col,
        );
        if line_count > usize::from(right_col.height) {
            scrollbar(f, popup, line_count, usize::from(scroll));
        }
    }

    /// The memory monitor: biggest processes first, the picked row marked.
    pub(super) fn draw_procs(&self, f: &mut Frame) {
        let Some(p) = &self.procs else {
            return;
        };
        let popup = procs_popup(f.area());
        f.render_widget(ratatui::widgets::Clear, popup);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(Line::from(Span::styled(
                " Memory: biggest processes ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            )))
            .title_bottom(
                popup_footer_line(
                    &[
                        ("↑↓/PgUp/PgDn", "pick"),
                        ("x", "kill"),
                        ("r", "refresh"),
                        ("Esc", "closes"),
                    ],
                    usize::from(popup.width.saturating_sub(4)),
                )
                .right_aligned(),
            );
        let inner = block.inner(popup);
        f.render_widget(block, popup);
        if inner.height == 0 {
            return;
        }
        let header_h: u16 = u16::from(inner.height >= 4);
        // Detail box for the picked process, then the footer row that carries
        // the kill question so it never hides in the list.
        let detail_h: u16 = if inner.height >= 12 { 5 } else { 0 };
        let parts = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(header_h),
                Constraint::Min(1),
                Constraint::Length(detail_h),
                Constraint::Length(1),
            ])
            .split(inner);
        if header_h > 0 {
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    format!("{:>7}  {:>8}  Process", "RAM", "PID"),
                    Style::default().add_modifier(ratatui::style::Modifier::DIM),
                ))),
                parts[0],
            );
        }
        let rows = usize::from(parts[1].height).max(1);
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
                // Process names come from the OS: strip control characters
                // so a hostile program name cannot inject escapes.
                let owner_tag = self
                    .proc_owner(it)
                    .map(|s| format!(" [{}]", sanitize_chrome_line(s)))
                    .unwrap_or_default();
                let clean_name = sanitize_chrome_line(&it.name);
                let name = take_cols(clean_name.chars(), name_w.saturating_sub(width(&owner_tag)));
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
        f.render_widget(Paragraph::new(list), parts[1]);
        if p.items.len() > rows {
            scrollbar(f, popup, p.items.len(), p.sel);
        }
        if let (true, Some(it)) = (detail_h > 0, p.items.get(p.sel)) {
            let owner = match self.proc_owner(it) {
                Some(name) => format!("squad session: {}", sanitize_chrome_line(name)),
                None => "not a squad session".to_string(),
            };
            let parent = if it.parent.is_empty() {
                it.ppid.to_string()
            } else {
                format!("{} ({})", it.ppid, sanitize_chrome_line(&it.parent))
            };
            let cmd = match &p.cmd {
                Some((pid, Some(c))) if *pid == it.pid => sanitize_chrome_line(c),
                _ => "(command line unavailable)".to_string(),
            };
            let text = vec![
                Line::from(format!(
                    "{} · PID {} · parent {parent}",
                    sanitize_chrome_line(&it.name),
                    it.pid
                )),
                Line::from(format!(
                    "RAM {} · CPU time {} · {owner}",
                    mem_label(it.mem),
                    uptime_label(it.cpu_secs)
                )),
                Line::from(Span::styled(cmd, Style::default().fg(Color::DarkGray))),
            ];
            f.render_widget(
                Paragraph::new(text)
                    .wrap(ratatui::widgets::Wrap { trim: false })
                    .block(Block::default().borders(Borders::TOP)),
                parts[2],
            );
        }
        let foot = parts[3];
        let line = match &p.confirm {
            Some(t) => {
                let mut spans = vec![Span::styled(
                    format!(
                        "Kill {} ({}) and its children?  ",
                        sanitize_chrome_line(&t.name),
                        t.pid
                    ),
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

    pub(crate) fn draw_context_menu(&self, f: &mut Frame) {
        if let Some(pi) = self.context_menu {
            if let Some(p) = self.cfg.projects.get(pi) {
                let lines = context_menu_lines_at_for(
                    &p.name,
                    slot_running(&self.sessions, pi),
                    p.kind,
                    p.restart_on_crash,
                    Some(self.context_sel),
                );
                modal(f, "Actions", Color::Cyan, lines);
            }
        }
    }

    pub(crate) fn draw_help(&self, f: &mut Frame) {
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
                        format!("{key:<13}"),
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
                        " Help (?/Esc closes) ",
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
    pub(crate) fn confirm_modals(&self) -> Vec<(&'static str, Color, Vec<Line<'static>>)> {
        let name_of = |pi: usize| {
            self.cfg
                .projects
                .get(pi)
                .map(|p| sanitize_chrome_line(&p.name))
                .unwrap_or_else(|| "?".to_string())
        };
        let mut v = Vec::new();
        if let Some(pi) = self.confirm_stop {
            v.push(("Stop session", Color::Red, confirm_stop_lines(&name_of(pi))));
        }
        if let Some(pi) = self.confirm_remove {
            v.push((
                "Remove project",
                Color::Red,
                confirm_remove_lines(&name_of(pi)),
            ));
        }
        if let Some(pi) = self.confirm_restart {
            v.push((
                "Restart session",
                Color::Red,
                confirm_restart_lines(&name_of(pi)),
            ));
        }
        if self.confirm_quit {
            let n = live_session_count(&self.sessions);
            let mut lines = confirm_quit_lines(n);
            // Name what quitting discards: running sessions are stopped.
            let names: Vec<String> = self
                .cfg
                .projects
                .iter()
                .enumerate()
                .filter(|(pi, _)| slot_running(&self.sessions, *pi))
                .map(|(_, p)| sanitize_chrome_line(&p.name))
                .collect();
            if !names.is_empty() {
                let shown = names.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
                let tail = if names.len() > 5 {
                    format!("{} …and {} more", shown, names.len() - 5)
                } else {
                    shown
                };
                lines.insert(
                    1,
                    Line::from(Span::styled(
                        tail,
                        Style::default().add_modifier(ratatui::style::Modifier::DIM),
                    )),
                );
            }
            v.push(("Quit", Color::Red, lines));
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

    pub(crate) fn draw_confirms(&self, f: &mut Frame) {
        for (title, color, lines) in self.confirm_modals() {
            modal(f, title, color, lines);
        }
    }
}
