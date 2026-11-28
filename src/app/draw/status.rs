use super::*;

impl App {
    pub(crate) fn draw_status(&mut self, f: &mut Frame, bar_area: Rect) {
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
        let fail_w = if failed_n > 0 {
            format!(" ✖ {failed_n}").chars().count() as u16
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
        // A notice (copied, a session died, memory limit) shows in a terminal
        // too; the hints return when it expires.
        if self.mode == Mode::Focus && self.message.0 == self.hints {
            bar.extend(fitted_hints(Mode::Focus, input_w));
        } else if self.mode == Mode::Filter {
            bar.extend(input_line(
                "Filter: ".to_string(),
                &self.filter_buf,
                format!(" ({} matches, Enter keeps, Esc clears)", self.order.len()),
                input_w,
            ));
        } else if self.mode == Mode::Search {
            let status = if self.search.buf.is_empty() {
                " (type to search, Esc clears)".to_string()
            } else if self.search.matches.is_empty() {
                " (0 matches · Esc clears)".to_string()
            } else {
                format!(
                    " ({}/{} · ↑↓ step · Enter keeps · Esc clears)",
                    self.search.idx + 1,
                    self.search.matches.len()
                )
            };
            bar.extend(input_line(
                "Find: ".to_string(),
                &self.search.buf,
                status,
                input_w,
            ));
        } else if self.mode == Mode::Send {
            let name = sel_pi
                .and_then(|pi| self.cfg.projects.get(pi))
                .map(|p| sanitize_chrome_line(&p.name))
                .unwrap_or_else(|| "?".to_string());
            let (label, hint) = match self.send_to {
                SendTo::One => (format!("Send to {name}: "), " (Enter sends, Esc cancels)"),
                SendTo::All => {
                    // Preview who actually gets the order: running agents
                    // that are not waiting on a question.
                    let n = sel_pi
                        .map(|ci| {
                            all_subordinates(&self.cfg.projects, ci)
                                .into_iter()
                                .filter(|&j| {
                                    self.sessions
                                        .get(j)
                                        .and_then(|o| o.as_ref())
                                        .is_some_and(|s| {
                                            s.status.is_running()
                                                && s.phase() != AgentPhase::NeedsInput
                                        })
                                })
                                .count()
                        })
                        .unwrap_or(0);
                    (
                        format!("Order to {n} under {name}: "),
                        " (Enter sends, Esc cancels)",
                    )
                }
                SendTo::Under => (
                    format!("{name} reports to (empty clears): "),
                    " (Tab completes, Enter saves, Esc cancels)",
                ),
            };
            bar.extend(input_line(label, &self.send_buf, hint.to_string(), input_w));
        } else if self.mode == Mode::Add {
            bar.extend(add_field_hints(self.add.field, input_w));
        } else if self.mode == Mode::Help && self.message.0 == self.hints {
            bar.extend(fitted_hints(Mode::Help, input_w));
        } else if self.message.0 == self.hints {
            if self.sidebar_collapsed && self.mode == Mode::Manager {
                let prefix = if let Some(m) = self.mgmt_sel {
                    MGMT_ITEMS
                        .iter()
                        .find(|it| it.idx == m)
                        .map(|it| (format!("{} ({})", it.label, it.key), Color::Cyan))
                } else if let Some(pi) = sel_pi {
                    let p = &self.cfg.projects[pi];
                    let sess = self.sessions.get(pi).and_then(|o| o.as_ref());
                    let fg = sess.and_then(|s| s.fg_name.as_deref());
                    let name = live_name(p, fg);
                    let detail = match sess {
                        Some(s)
                            if s.status.is_running()
                                && p.kind == Kind::Agent
                                && s.phase() == AgentPhase::NeedsInput =>
                        {
                            format!(
                                "? {}",
                                s.prompt_hint.as_deref().unwrap_or("Awaiting orders")
                            )
                        }
                        Some(s) if s.status.is_running() && p.kind == Kind::Agent => {
                            s.phase().word().to_string()
                        }
                        Some(s) => s.status.label().into_owned(),
                        None => self.idle_word(pi).to_string(),
                    };
                    let c = if sess.is_some_and(|s| s.phase() == AgentPhase::NeedsInput) {
                        Color::Yellow
                    } else {
                        kind_color(p.kind)
                    };
                    Some((format!("[{}] {name} · {detail}", self.selected + 1), c))
                } else {
                    None
                };
                if let Some((text, color)) = prefix {
                    let max_p = input_w.min(44);
                    let shown = fit(&text, max_p);
                    let used = width(&shown) + 3;
                    bar.push(Span::styled(
                        shown,
                        Style::default()
                            .fg(color)
                            .add_modifier(ratatui::style::Modifier::BOLD),
                    ));
                    if input_w > used + 8 {
                        bar.push(Span::styled(
                            " | ".to_string(),
                            Style::default().fg(Color::DarkGray),
                        ));
                        bar.extend(fitted_hints(Mode::Manager, input_w - used));
                    }
                } else {
                    bar.extend(fitted_hints(Mode::Manager, input_w));
                }
            } else {
                // Idle hints recede so action feedback stands out.
                bar.extend(fitted_hints(Mode::Manager, input_w));
            }
        } else {
            // Status messages carry project names, paths and tool errors:
            // strip control characters so none of them can inject escape
            // sequences into the outer terminal.
            bar.push(Span::styled(
                fit(&sanitize_chrome_line(&self.message.0), input_w),
                notice_style(self.message.1),
            ));
        }
        f.render_widget(Paragraph::new(Line::from(bar)), status_row[0]);
        self.wait_badge_rect = if wait_w > 0 {
            let right_x = status_row[1]
                .right()
                .saturating_sub(fail_w + upd_w + wait_w);
            Rect::new(right_x, status_row[1].y, wait_w, 1)
        } else {
            Rect::default()
        };
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
}
