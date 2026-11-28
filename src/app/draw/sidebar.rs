use super::*;

impl App {
    pub(super) fn draw_collapsed_tooltip(&self, f: &mut Frame) {
        let Some(target) = self.hover_row else {
            return;
        };
        let Some(off) = self
            .row_targets
            .iter()
            .skip(self.list_offset)
            .position(|&t| t == Some(target))
        else {
            return;
        };
        let y = self.side_rect.y + 1 + off as u16;
        if y >= self.side_rect.bottom().saturating_sub(1) {
            return;
        }
        let mut spans: Vec<Span<'static>> = Vec::new();
        if let Some(m) = mgmt_row_index(target) {
            if let Some(item) = MGMT_ITEMS.iter().find(|it| it.idx == m) {
                spans.push(Span::styled(
                    format!(" {} ({}) ", item.label, item.key),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ));
            }
        } else if let Some(&pi) = self.order.get(target) {
            let Some(p) = self.cfg.projects.get(pi) else {
                return;
            };
            let sess = self.sessions.get(pi).and_then(|o| o.as_ref());
            let fg = sess.and_then(|s| s.fg_name.as_deref());
            let name = live_name(p, fg);
            spans.push(Span::styled(
                format!(" {} · {name} ", target + 1),
                Style::default()
                    .fg(kind_color(p.kind))
                    .add_modifier(ratatui::style::Modifier::BOLD),
            ));
            if let Some(s) = sess {
                if s.status.is_running() {
                    if p.kind == Kind::Agent && s.phase() == AgentPhase::NeedsInput {
                        let ask = s.prompt_hint.as_deref().unwrap_or("Awaiting orders");
                        spans.push(Span::styled(
                            format!("? {} ", fit(ask, 42)),
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(ratatui::style::Modifier::BOLD),
                        ));
                    } else if p.kind == Kind::Agent {
                        spans.push(Span::styled(
                            format!("{} ", s.phase().word()),
                            Style::default().fg(Color::Green),
                        ));
                    } else if let Some(u) = s.usage {
                        spans.push(Span::styled(
                            format!("{} ", usage_label(u)),
                            Style::default().fg(Color::Green),
                        ));
                    }
                } else {
                    let st = status_style(&s.status.label());
                    // Tooltip background is DarkGray: raise DarkGray text to Gray so Exited (0) stays readable.
                    let st = if st.fg == Some(Color::DarkGray) {
                        Style::default().fg(Color::Gray)
                    } else {
                        st
                    };
                    spans.push(Span::styled(format!("{} ", s.status), st));
                }
            } else {
                spans.push(Span::styled(
                    format!("{} ", self.idle_word(pi)),
                    Style::default().fg(Color::Gray),
                ));
            }
        } else {
            return;
        }
        let max_w = f.area().width.saturating_sub(self.side_rect.right());
        let tip_w = spans
            .iter()
            .map(|s| width(s.content.as_ref()))
            .sum::<usize>()
            .min(usize::from(max_w)) as u16;
        if tip_w < 3 {
            return;
        }
        let rect = Rect::new(self.side_rect.right(), y, tip_w, 1);
        f.render_widget(ratatui::widgets::Clear, rect);
        f.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(Color::DarkGray)),
            rect,
        );
    }

    pub(crate) fn draw_sidebar(&mut self, f: &mut Frame, panel: Rect) {
        // A Management pick lives only while the manager has the keyboard.
        if self.mode != Mode::Manager {
            self.mgmt_sel = None;
        }
        let sel_pi = if self.mgmt_sel.is_some() {
            None
        } else {
            self.order.get(self.selected).copied()
        };
        let collapsed = self.sidebar_collapsed;
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
            let hdr = if collapsed {
                compact_group_header(k, up, total)
            } else {
                group_header(title, k, up, total)
            };
            items.push(ListItem::new(hdr));
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
                let sess = self.sessions[pi].as_ref();
                let is_running = sess.is_some_and(|s| s.status.is_running());
                let needs_att = sess.is_some_and(|s| s.status.needs_attention());
                let status_text = sess
                    .map(|s| s.status.label())
                    .unwrap_or(std::borrow::Cow::Borrowed(self.idle_word(pi)));
                let status: &str = &status_text;
                let fg = sess.and_then(|s| s.fg_name.as_deref());
                let tree = self.rank_prefixes.get(pi).map(String::as_str).unwrap_or("");
                let pin = if p.pinned { "*" } else { "" };
                let retry = if p.kind == Kind::Service && p.restart_on_crash {
                    "~"
                } else {
                    ""
                };
                // Project names and live program names come from folders and
                // processes: strip control characters so a crafted name
                // cannot inject escape sequences into the outer terminal.
                let shown =
                    sanitize_chrome_line(&format!("{tree}{}{pin}{retry}", live_name(p, fg)));
                let shown_name: &str = &shown;
                let mut detail = String::new();
                if is_running {
                    if let Some(s) = sess {
                        if let Some(started) = s.started {
                            detail.push(' ');
                            detail.push_str(&uptime_label(started.elapsed().as_secs()));
                        }
                        if p.kind == Kind::Agent {
                            let quiet = s.last_output.map(|t| t.elapsed().as_secs());
                            if let Some(note) = idle_note(s.phase(), quiet) {
                                detail.push_str(" · ");
                                detail.push_str(&note);
                            } else if s.phase() == AgentPhase::Done && !s.acked {
                                detail.push_str(" · new");
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
                if let Some((host, _)) = remote(&p.path) {
                    let short_host = host.rsplit('@').next().unwrap_or(host);
                    detail.push_str(&format!(" @{short_host}"));
                }
                let detail = sanitize_chrome_line(&detail);
                let mark = if Some(pi) == sel_pi { "▎" } else { " " };
                let row_style = if Some(pi) == sel_pi {
                    selected_row_style()
                } else if is_running {
                    Style::default().add_modifier(ratatui::style::Modifier::BOLD)
                } else {
                    Style::default().add_modifier(ratatui::style::Modifier::DIM)
                };
                let agent_phase = if p.kind == Kind::Agent && is_running {
                    sess.map(|s| s.phase())
                } else {
                    None
                };
                // Waiting agents get a yellow bar, and unread finished agents
                // get a green bar so they stand out at a glance in the list.
                let needs_input = agent_phase == Some(AgentPhase::NeedsInput) && Some(pi) != sel_pi;
                let done_unacked = agent_phase == Some(AgentPhase::Done)
                    && sess.is_some_and(|s| !s.acked)
                    && Some(pi) != sel_pi;
                let mark = if needs_input || done_unacked {
                    "▎"
                } else {
                    mark
                };
                let mark_style = if needs_input {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(ratatui::style::Modifier::BOLD)
                } else if done_unacked {
                    Style::default()
                        .fg(Color::Green)
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
                    } else if is_running {
                        (Color::Green, true)
                    } else if status == "Starting" {
                        (Color::Cyan, true)
                    } else if needs_att {
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
                if Some(pi) == sel_pi {
                    sel_row = Some(items.len());
                }
                targets.push(Some(n - 1));
                let tree_style = if Some(pi) == sel_pi {
                    row_style
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                if collapsed {
                    let icon_str = if let Some(ph) = agent_phase {
                        ph.icon(self.tick).0
                    } else if is_running || status == "Starting" {
                        spinner_frame(self.tick).to_string()
                    } else {
                        status_glyph(status, self.tick).to_string()
                    };
                    let num_str = if n < 10 {
                        format!("{n} ")
                    } else {
                        format!("{n:>2}")
                    };
                    let badge_span = if needs_att {
                        Span::styled(" !", status_span_style)
                    } else if tree.contains('★') {
                        Span::styled(" ★", tree_style)
                    } else if p.pinned {
                        Span::styled(" *", row_style)
                    } else {
                        Span::raw("  ")
                    };
                    let num_style = if needs_input {
                        mark_style
                    } else {
                        row_name_style(Some(pi) == sel_pi, status, p.kind)
                    };
                    let row = ListItem::new(Line::from(vec![
                        Span::styled(mark, mark_style),
                        Span::styled(num_str, num_style),
                        Span::styled(icon_str, status_span_style),
                        badge_span,
                    ]));
                    items.push(if Some(pi) == sel_pi {
                        row.style(selected_row_style())
                    } else {
                        row
                    });
                    continue;
                }
                let prefix = format!("{mark}{n} ");
                let status_full = if let Some(ph) = agent_phase {
                    ph.icon(self.tick).0
                } else if is_running {
                    format!("{}", status_glyph(status, self.tick))
                } else if status == "Starting" {
                    format!("{} Starting", spinner_frame(self.tick))
                } else {
                    format!("{} {status}", status_glyph(status, self.tick))
                };
                let (status_w, name_w, detail_w) = row_widths(
                    inner_w,
                    prefix.chars().count(),
                    width(&status_full),
                    width(shown_name),
                );
                // Tree lines recede; the star and the name keep the row's colors.
                let name_fit = fit(shown_name, name_w);
                let lines_w = tree.chars().take_while(|&c| c != '★').count();
                let split = name_fit
                    .char_indices()
                    .nth(lines_w)
                    .map_or(name_fit.len(), |(at, _)| at);
                let (name_head, name_tail) =
                    (name_fit[..split].to_string(), name_fit[split..].to_string());
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
                    let sub_bar = if needs_input {
                        Span::styled(
                            "▎  ",
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(ratatui::style::Modifier::BOLD),
                        )
                    } else {
                        Span::raw("   ")
                    };
                    items.push(ListItem::new(Line::from(vec![
                        sub_bar,
                        Span::styled(
                            fit(ask, inner_w.saturating_sub(3)),
                            Style::default().fg(Color::Yellow),
                        ),
                    ])));
                }
            }
            if !any {
                targets.push(None);
                items.push(
                    ListItem::new(if collapsed { "  -" } else { "  (none)" })
                        .style(Style::default().add_modifier(ratatui::style::Modifier::DIM)),
                );
            }
        }
        items.push(ListItem::new(Line::from(Span::styled(
            if collapsed { "Mgmt" } else { "Management" },
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(ratatui::style::Modifier::BOLD),
        ))));
        targets.push(None);
        for item in MGMT_ITEMS {
            let open = match item.idx {
                0 => self.procs.is_some(),
                1 => self.vault_modal.as_ref().is_some_and(VaultModal::is_env),
                2 => self.vault_modal.as_ref().is_some_and(VaultModal::is_skill),
                3 => self.vault_modal.as_ref().is_some_and(VaultModal::is_tool),
                4 => self.vault_modal.as_ref().is_some_and(VaultModal::is_ref),
                5 => self.history.as_ref().is_some_and(|h| h.kind == "Lessons"),
                6 => self.vault_modal.as_ref().is_some_and(VaultModal::is_model),
                7 => self.vault_modal.as_ref().is_some_and(VaultModal::is_server),
                _ => false,
            };
            let (idx, label, key_tag, row_id) = (item.idx, item.label, item.key, item.row_id);
            if self.mgmt_sel == Some(idx) {
                sel_row = Some(items.len());
            }
            let row_style = if open || self.mgmt_sel == Some(idx) {
                selected_row_style()
            } else {
                Style::default()
            };
            let line = if collapsed {
                Line::from(format!("  {key_tag}"))
            } else {
                Line::from(vec![
                    Span::raw(format!("  {label} ")),
                    Span::styled(
                        key_tag,
                        Style::default().add_modifier(ratatui::style::Modifier::DIM),
                    ),
                ])
            };
            items.push(ListItem::new(line).style(row_style));
            targets.push(Some(row_id));
        }
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
            .border_style(sidebar_border_style(self.mode == Mode::Focus));
        if collapsed {
            block = block.title(Line::from(Span::styled(
                " [>] ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            )));
        } else {
            block = block.title(list_title(self.cfg.projects.len(), self.order.len()));
            let mut right_spans = Vec::new();
            if !self.filter_buf.is_empty() && panel.width >= 28 {
                let q = fit(&self.filter_buf.as_str(), 8);
                right_spans.push(Span::styled(
                    format!(" /{q} "),
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ));
            }
            right_spans.push(Span::styled(
                "[<]",
                Style::default().add_modifier(ratatui::style::Modifier::DIM),
            ));
            block = block.title_top(Line::from(right_spans).right_aligned());
            // Waiting questions stay readable when their rows scroll away.
            if let Some(note) = waiting_summary(&self.waiting_list(), usize::from(panel.width)) {
                block = block.title_bottom(Line::from(Span::styled(
                    note,
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                )));
            }
        }
        f.render_widget(List::new(window).block(block), panel);
        if total_items > visible_rows {
            scrollbar(f, panel, total_items, sel_row.unwrap_or(self.list_offset));
        }
    }
}
