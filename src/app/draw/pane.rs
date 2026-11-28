use super::*;

impl App {
    pub(crate) fn draw_pane(&self, f: &mut Frame, panel: Rect) {
        let sel_pi = self.order.get(self.selected).copied();
        let sel_session = sel_pi
            .and_then(|pi| self.sessions.get(pi))
            .and_then(|o| o.as_ref());
        let (sel_status_text, sel_uptime): (std::borrow::Cow<'static, str>, Option<u64>) =
            sel_session
                .map(|s| (s.status.label(), s.started.map(|t| t.elapsed().as_secs())))
                .unwrap_or((
                    std::borrow::Cow::Borrowed(sel_pi.map_or("Stopped", |pi| self.idle_word(pi))),
                    None,
                ));
        let sel_status: &str = &sel_status_text;
        let sel_fg = sel_session.and_then(|s| s.fg_name.as_deref());
        let usage_note = sel_session.and_then(|s| s.usage).map(pane_usage_note);
        let scroll_note = sel_session.and_then(|s| {
            let n = s.parser.screen().scrollback();
            let fresh = s
                .last_output
                .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(5));
            scroll_note_text(n, fresh)
        });
        let zoom_note = (self.zoom && self.mode == Mode::Focus).then(|| "ZOOM".to_string());
        // The mode shows in the border color and the status-bar chip, so the
        // title keeps only what the session is and how it is doing.
        let title = sel_pi
            .and_then(|pi| self.cfg.projects.get(pi))
            .map(|p| {
                let sel_phase = sel_session.map(|s| s.phase());
                let (disp_status, disp_style) =
                    pane_status_display(sel_status, sel_uptime, self.tick, p.kind, sel_phase);
                Line::from(vec![
                    Span::styled(
                        sanitize_chrome_line(&live_pane_title(p, sel_fg)),
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
        // Usage and zoom ride in the top-right title; scroll position goes in
        // the bottom-right border so standard 80-column terminals can still see it.
        let top_right = [zoom_note.clone(), usage_note.clone()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        if !top_right.is_empty() && panel.width >= 48 {
            block = block.title_top(
                Line::from(Span::styled(
                    format!(" {top_right} "),
                    Style::default().add_modifier(ratatui::style::Modifier::DIM),
                ))
                .right_aligned(),
            );
        } else if zoom_note.is_some() && panel.width >= 24 {
            block = block.title_top(
                Line::from(Span::styled(
                    " ZOOM ",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ))
                .right_aligned(),
            );
        }
        if let Some(scroll) = scroll_note.filter(|_| panel.width >= 30) {
            block = block.title_bottom(
                Line::from(Span::styled(
                    format!(" {scroll} "),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ))
                .right_aligned(),
            );
        }
        if self.search.active && self.search.pi == sel_pi && !self.search.buf.is_empty() {
            let label = if self.search.matches.is_empty() {
                format!(" Find \"{}\": 0 ", fit(&self.search.buf.as_str(), 12))
            } else {
                format!(
                    " Find \"{}\": {}/{} ",
                    fit(&self.search.buf.as_str(), 12),
                    self.search.idx + 1,
                    self.search.matches.len()
                )
            };
            block = block.title_bottom(Line::from(Span::styled(
                label,
                Style::default()
                    .fg(if self.search.matches.is_empty() {
                        Color::Red
                    } else {
                        Color::Black
                    })
                    .bg(if self.search.matches.is_empty() {
                        Color::Reset
                    } else {
                        Color::Yellow
                    })
                    .add_modifier(ratatui::style::Modifier::BOLD),
            )));
        }
        let inner = block.inner(panel);
        f.render_widget(block, panel);
        if let Some(s) = sel_session {
            let screen = s.parser.screen();
            s.paint(f.buffer_mut(), inner);
            if self.search.active && self.search.pi == sel_pi && !self.search.buf.is_empty() {
                let active_match = self.search.matches.get(self.search.idx).copied();
                let other_hit = Style::default().bg(Color::Yellow).fg(Color::Black);
                let cur_hit = Style::default()
                    .bg(Color::Cyan)
                    .fg(Color::Black)
                    .add_modifier(ratatui::style::Modifier::BOLD);
                for (vis_row, logical_line, cs, ce) in
                    s.visible_search_hits(&self.search.buf.as_str())
                {
                    if vis_row >= inner.height {
                        continue;
                    }
                    let is_cur =
                        active_match.is_some_and(|m| m.line == logical_line && m.col_start == cs);
                    let style = if is_cur { cur_hit } else { other_hit };
                    for col in cs..ce.min(inner.width) {
                        if let Some(cell) =
                            f.buffer_mut().cell_mut((inner.x + col, inner.y + vis_row))
                        {
                            cell.set_style(style);
                        }
                    }
                }
            }
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
        } else if let Some(pi) = sel_pi.filter(|&pi| !self.is_starting(pi)) {
            let p = &self.cfg.projects[pi];
            let max_val = usize::from(inner.width).saturating_sub(14).max(10);
            let mut rows = vec![
                (
                    "Folder   ",
                    shorten_middle(&p.path.display().to_string(), max_val),
                ),
                ("Command  ", fit(&join_command(&p.command), max_val)),
            ];
            if let Some(sup) = &p.reports_to {
                rows.push(("Reports  ", sup.clone()));
            }
            if p.autostart {
                rows.push(("Auto     ", "starts with manager".to_string()));
            }
            let row_w = rows
                .iter()
                .map(|(k, v)| k.chars().count() + v.chars().count())
                .max()
                .unwrap_or(0);
            let mut card = vec![
                Line::from(vec![
                    Span::styled(
                        p.name.clone(),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(ratatui::style::Modifier::BOLD),
                    ),
                    Span::raw(" "),
                    Span::styled(
                        format!("[{}]", p.kind.as_str()),
                        Style::default()
                            .fg(kind_color(p.kind))
                            .add_modifier(ratatui::style::Modifier::BOLD),
                    ),
                ]),
                Line::from(""),
            ];
            for (k, v) in rows {
                let pad_r = row_w.saturating_sub(k.chars().count() + v.chars().count());
                card.push(Line::from(vec![
                    Span::styled(k, Style::default().fg(Color::DarkGray)),
                    Span::raw(v),
                    Span::raw(" ".repeat(pad_r)),
                ]));
            }
            let mut hints = vec![("Enter", "start"), ("m", "message"), ("e", "edit")];
            if p.has_ledger() {
                hints.push(("h", "history"));
            }
            card.push(Line::from(""));
            card.push(Line::from(hint_spans(hints_that_fit(
                &hints,
                usize::from(inner.width),
            ))));
            let pad = inner.height.saturating_sub(card.len() as u16) / 2;
            let mut lines: Vec<Line> = (0..pad).map(|_| Line::from("")).collect();
            lines.extend(card);
            f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), inner);
        } else {
            let body = if sel_pi.is_some_and(|pi| self.is_starting(pi)) {
                vec![format!("{} Starting…", spinner_frame(self.tick))]
            } else if !self.cfg.projects.is_empty() && self.order.is_empty() {
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
}
