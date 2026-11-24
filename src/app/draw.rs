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
                    format!(" {title} "),
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
    pub(crate) fn draw(&mut self, f: &mut Frame) {
        let sidebar = self.side_w(f.area().width);
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
        if self.sidebar_collapsed && !self.has_modal() && sidebar > 0 {
            self.draw_collapsed_tooltip(f);
        }
        if self.has_modal() {
            let full_area = f.area();
            dim_area(f.buffer_mut(), full_area);
        }
        if self.mode == Mode::Help {
            self.draw_help(f);
        }
        self.draw_history(f);
        self.draw_git_view(f);
        self.draw_procs(f);
        self.draw_gsearch(f);
        self.draw_quick_reply(f);
        self.draw_confirms(f);
        self.draw_context_menu(f);
        if let Some(lines) = self.env_info_lines() {
            modal(f, "Environment", Color::Cyan, lines);
        }
        if self.mode == Mode::Add {
            self.draw_add(f);
        }
        self.draw_env_modal(f);
        self.draw_skill_modal(f);
        self.draw_tool_modal(f);
        self.draw_ref_modal(f);
    }

    fn draw_collapsed_tooltip(&self, f: &mut Frame) {
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
            let label = match m {
                0 => " Memory monitor (M) ",
                1 => " Env vault (E) ",
                2 => " Skills (K) ",
                3 => " Tools (T) ",
                _ => " References (R) ",
            };
            spans.push(Span::styled(
                label,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            ));
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
                    spans.push(Span::styled(
                        format!("{} ", s.status),
                        status_style(&s.status.label()),
                    ));
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

    fn draw_git_view(&self, f: &mut Frame) {
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
                Line::from(Span::styled(
                    " ↑↓/j/k pick · PgUp/PgDn/J/K/wheel scroll · r refresh · Tab project · Esc closes ",
                    Style::default().add_modifier(ratatui::style::Modifier::DIM),
                ))
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
    fn draw_gsearch(&self, f: &mut Frame) {
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
        let footer = if g.query.is_empty() {
            " type to search · Esc closes "
        } else {
            " ↑↓/j/k pick · Enter jump · Esc closes "
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
                Line::from(Span::styled(
                    footer,
                    Style::default().add_modifier(ratatui::style::Modifier::DIM),
                ))
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

    fn draw_quick_reply(&self, f: &mut Frame) {
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
    fn draw_history(&self, f: &mut Frame) {
        let Some(h) = &self.history else {
            return;
        };
        let popup = history_popup(f.area());
        f.render_widget(ratatui::widgets::Clear, popup);
        let count_note = if h.query.is_empty() {
            format!(
                " History: {} ({}) ",
                sanitize_chrome_line(&h.project),
                h.items.len()
            )
        } else {
            format!(
                " History: {} ({}/{} · /{} ) ",
                h.project,
                h.view.len(),
                h.items.len(),
                h.query
            )
        };
        let footer = if h.typing {
            " type to filter · Enter keeps · Esc clears "
        } else {
            " ↑↓/j/k pick · / filter · PgUp/PgDn/J/K/wheel scroll · Esc closes "
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
                Line::from(Span::styled(
                    footer,
                    Style::default().add_modifier(ratatui::style::Modifier::DIM),
                ))
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
                " Memory: biggest processes ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            )))
            .title_bottom(
                Line::from(Span::styled(
                    " ↑↓/PgUp/PgDn pick · x kill · r refresh · Esc closes ",
                    Style::default().add_modifier(ratatui::style::Modifier::DIM),
                ))
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

    pub(super) fn draw_context_menu(&self, f: &mut Frame) {
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

    /// The environment popup's rows, shared by drawing and click mapping.
    pub(super) fn env_info_lines(&self) -> Option<Vec<Line<'static>>> {
        let info = self.env_info.as_ref()?;
        let p = self.cfg.projects.get(info.pi)?;
        Some(env_info_lines(p, &info.dotenv, info.reveal))
    }

    /// What a project without a session is doing: waiting, or on its way up.
    fn idle_word(&self, pi: usize) -> &'static str {
        if self.is_starting(pi) {
            "Starting"
        } else {
            "Stopped"
        }
    }

    pub(super) fn draw_sidebar(&mut self, f: &mut Frame, panel: Rect) {
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
        let mgmt_rows = [
            (
                0usize,
                "Memory monitor",
                "M",
                MEMORY_ROW,
                self.procs.is_some(),
            ),
            (1usize, "Env vault", "E", ENV_ROW, self.env_modal.is_some()),
            (
                2usize,
                "Skills",
                "K",
                SKILLS_ROW,
                self.skill_modal.is_some(),
            ),
            (3usize, "Tools", "T", TOOLS_ROW, self.tool_modal.is_some()),
            (
                4usize,
                "References",
                "R",
                REFS_ROW,
                self.ref_modal.is_some(),
            ),
        ];
        for (idx, label, key_tag, row_id, open) in mgmt_rows {
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

    pub(super) fn draw_pane(&self, f: &mut Frame, panel: Rect) {
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
        if self.search_active && self.search_pi == sel_pi && !self.search_buf.is_empty() {
            let label = if self.search_matches.is_empty() {
                format!(" Find \"{}\": 0 ", fit(&self.search_buf.as_str(), 12))
            } else {
                format!(
                    " Find \"{}\": {}/{} ",
                    fit(&self.search_buf.as_str(), 12),
                    self.search_idx + 1,
                    self.search_matches.len()
                )
            };
            block = block.title_bottom(Line::from(Span::styled(
                label,
                Style::default()
                    .fg(if self.search_matches.is_empty() {
                        Color::Red
                    } else {
                        Color::Black
                    })
                    .bg(if self.search_matches.is_empty() {
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
            if self.search_active && self.search_pi == sel_pi && !self.search_buf.is_empty() {
                let active_match = self.search_matches.get(self.search_idx).copied();
                let other_hit = Style::default().bg(Color::Yellow).fg(Color::Black);
                let cur_hit = Style::default()
                    .bg(Color::Cyan)
                    .fg(Color::Black)
                    .add_modifier(ratatui::style::Modifier::BOLD);
                for (vis_row, logical_line, cs, ce) in
                    s.visible_search_hits(&self.search_buf.as_str())
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

    pub(super) fn draw_status(&mut self, f: &mut Frame, bar_area: Rect) {
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
            let status = if self.search_buf.is_empty() {
                " (type to search, Esc clears)".to_string()
            } else if self.search_matches.is_empty() {
                " (0 matches · Esc clears)".to_string()
            } else {
                format!(
                    " ({}/{} · ↑↓ step · Enter keeps · Esc clears)",
                    self.search_idx + 1,
                    self.search_matches.len()
                )
            };
            bar.extend(input_line(
                "Find: ".to_string(),
                &self.search_buf,
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
            bar.extend(fitted_hints(Mode::Add, input_w));
        } else if self.mode == Mode::Help && self.message.0 == self.hints {
            bar.extend(fitted_hints(Mode::Help, input_w));
        } else if self.message.0 == self.hints {
            if self.sidebar_collapsed && self.mode == Mode::Manager {
                let prefix = if let Some(m) = self.mgmt_sel {
                    let label = match m {
                        0 => "Memory monitor (M)",
                        1 => "Env vault (E)",
                        2 => "Skills (K)",
                        3 => "Tools (T)",
                        _ => "References (R)",
                    };
                    Some((label.to_string(), Color::Cyan))
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
    pub(super) fn confirm_modals(&self) -> Vec<(&'static str, Color, Vec<Line<'static>>)> {
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
    fn add_mark(&self, modal_w: usize) -> (&'static str, Color) {
        let compact = modal_w < 60;
        match self.add_state {
            PathState::Folder => ("✓ folder", Color::Green),
            PathState::Missing => (
                if compact {
                    "✗ missing"
                } else {
                    "✗ not found · Enter creates"
                },
                Color::Red,
            ),
            PathState::NotFolder => (
                if compact {
                    "✗ not dir"
                } else {
                    "✗ not a folder"
                },
                Color::Red,
            ),
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

    /// The rows of `add_picks` on screen: at most `PICK_ROWS`, scrolled smoothly
    /// via `keep_visible` so stepping up/down never jumps early.
    pub(super) fn add_pick_window(&self) -> std::ops::Range<usize> {
        if self.add_field != 0 {
            return 0..0;
        }
        let n = self.add_picks.len();
        let max_start = n.saturating_sub(PICK_ROWS);
        let start = match self.add_sel {
            Some(s) => keep_visible(self.add_top.get().min(max_start), s, PICK_ROWS),
            None => 0,
        };
        self.add_top.set(start);
        start..(start + PICK_ROWS).min(n)
    }

    fn add_err_lines(&self, max_w: usize) -> Vec<Line<'static>> {
        if self.add_err.is_empty() {
            return Vec::new();
        }
        let msg = format!("Error: {}", self.add_err);
        let w = max_w.max(10);
        let mut out = Vec::new();
        let mut rest = msg.as_str();
        while !rest.is_empty() {
            let chunk = take_cols(rest.chars(), w);
            let len = chunk.len().max(1);
            out.push(Line::from(Span::styled(
                rest[..len].to_string(),
                Style::default().fg(Color::Red),
            )));
            rest = &rest[len..];
        }
        out
    }

    /// The add form's popup; its height follows the rows it shows.
    pub(super) fn add_popup(&self, area: Rect) -> Rect {
        let modal_w = usize::from(72u16.min(area.width)).saturating_sub(2);
        let err_rows = self.add_err_lines(modal_w).len() as u16;
        let rows = 9 + self.add_list_h() + u16::from(self.add_status().is_some()) + err_rows;
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
        let noun = if n == 1 { "folder" } else { "folders" };
        let header = match self.add_picks.first() {
            None => String::new(),
            Some(_) if typed.is_empty() => {
                format!("  your project folders · {count_note} · ↑↓/wheel Enter")
            }
            Some((label, full)) => {
                let dir = full.strip_suffix(label.as_str()).unwrap_or("");
                let dir = shorten_middle(dir.trim_start_matches('"'), modal_w.saturating_sub(40));
                if dir.is_empty() {
                    format!("  {count_note} {noun} · ↑↓/wheel Enter")
                } else {
                    format!("  in {dir} · {count_note} {noun} · ↑↓/wheel Enter")
                }
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
        } else if self.add_kind == Kind::Service {
            "(agents only)".to_string()
        } else {
            self.add_under.as_str()
        };
        let env_text = picked_summary(&self.add_env, self.env_vault.vars.len());
        let skills_text = picked_summary(&self.add_skills, self.skills_vault.skills.len());
        let tools_text = picked_summary(&self.add_tools, self.tools_vault.tools.len());
        let refs_text = picked_summary(&self.add_refs, self.refs_vault.references.len());
        let fields = [
            (path_label, self.add_path.as_str()),
            ("Kind", kind_hint.to_string()),
            ("Command", cmd_text),
            ("Autostart", autostart_hint),
            ("Reports to", under_text),
            ("Env (.env)", env_text),
            ("Skills", skills_text),
            ("Tools", tools_text),
            ("References", refs_text),
        ];
        let (path_mark, path_mark_color) = self.add_mark(modal_w);
        let mark_w = if path_mark.is_empty() {
            0
        } else {
            path_mark.chars().count() + 1
        };
        let mut lines = Vec::new();
        for (i, (label, val)) in fields.iter().enumerate() {
            let active = i == self.add_field;
            let locked = (editing && !matches!(i, 0 | 2 | 3 | 5 | 6 | 7 | 8))
                || (!editing && self.add_kind == Kind::Service && i == 4);
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
                } else if i == 4 && self.add_under.is_empty() {
                    row.push(Span::styled(
                        "(optional · Tab completes)".to_string(),
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
        lines.extend(self.add_err_lines(modal_w));
        f.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .title(Line::from(Span::styled(
                        if editing {
                            " Edit project: path, cmd, autostart, capabilities (Enter save, Esc cancel) "
                        } else {
                            " Add project (Enter save, Esc cancel) "
                        },
                        Style::default()
                            .fg(Color::Blue)
                            .add_modifier(ratatui::style::Modifier::BOLD),
                    ))),
            ),
            popup,
        );
    }

    fn draw_env_modal(&self, f: &mut Frame) {
        let Some(m) = &self.env_modal else {
            return;
        };
        let title = match m.target {
            VaultTarget::AddForm => " Shared Env Vault -> new project .env ".to_string(),
            VaultTarget::Project(pi) => {
                let name = self
                    .cfg
                    .projects
                    .get(pi)
                    .map(|p| sanitize_chrome_line(&p.name))
                    .unwrap_or_else(|| "?".to_string());
                format!(" Shared Env Vault -> {name}/.env ")
            }
            VaultTarget::VaultOnly => " Shared Env Vault ".to_string(),
        };
        let (popup, parts, has_editor) = vault_modal_layout(f, &title, m.editor.is_some(), 5);
        let Some(parts) = parts else {
            return;
        };
        let inner_w = usize::from(parts[1].width);
        let can_pick = m.target != VaultTarget::VaultOnly;
        let key_w = 22usize.min(inner_w / 3);
        let badge_w = if matches!(m.target, VaultTarget::Project(_)) || !m.project_dotenv.is_empty()
        {
            9usize
        } else {
            0
        };
        if parts[0].height > 0 {
            let prefix = if can_pick { "    " } else { "  " };
            let head = if badge_w > 0 {
                format!(
                    "{prefix}{:<kw$}  {:<vw$}  .env",
                    "Key",
                    "Value",
                    kw = key_w,
                    vw = inner_w.saturating_sub(prefix.len() + key_w + badge_w + 5)
                )
            } else {
                format!("{prefix}{:<kw$}  Value", "Key", kw = key_w)
            };
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    head,
                    Style::default().add_modifier(ratatui::style::Modifier::DIM),
                ))),
                parts[0],
            );
        }
        let rows = usize::from(parts[1].height).max(1);
        let vars = &self.env_vault.vars;
        let local_only: Vec<(&String, &String)> = m
            .project_dotenv
            .iter()
            .filter(|(k, _)| !vars.iter().any(|v| v.key == **k))
            .collect();
        let val_w = inner_w
            .saturating_sub(if can_pick { 6 } else { 4 } + key_w + badge_w + 3)
            .max(6);
        if vars.is_empty() {
            let mut empty_lines = vec![Line::from(Span::styled(
                "  No shared env vars registered yet. Press a to create one.",
                Style::default().add_modifier(ratatui::style::Modifier::DIM),
            ))];
            for (k, v) in local_only.iter().take(rows.saturating_sub(1)) {
                let shown_val = if m.reveal {
                    (*v).clone()
                } else {
                    envvault::mask_value(v)
                };
                empty_lines.push(Line::from(Span::styled(
                    format!(
                        "    {:<kw$}  {:<vw$}  local",
                        fit(k, key_w),
                        fit(&shown_val, val_w),
                        kw = key_w,
                        vw = val_w
                    ),
                    Style::default().add_modifier(ratatui::style::Modifier::DIM),
                )));
            }
            f.render_widget(Paragraph::new(empty_lines), parts[1]);
        } else {
            // One row past the variables: "+ New variable".
            let total = vars.len() + 1;
            let start = keep_visible(m.top.get(), m.sel.min(total - 1), rows);
            m.top.set(start);
            let mut list: Vec<Line> = vars
                .iter()
                .enumerate()
                .skip(start)
                .take(rows)
                .map(|(i, entry)| {
                    let active = i == m.sel;
                    let checked = m.picked.contains(&entry.key);
                    let row_style = vault_row_style(active);
                    let mut spans = vault_row_prefix(can_pick, active, checked, row_style);
                    let shown_val = if m.reveal {
                        entry.value.clone()
                    } else {
                        envvault::mask_value(&entry.value)
                    };
                    let k_fit = format!("{:<kw$}", fit(&entry.key, key_w), kw = key_w);
                    let v_fit = format!("{:<vw$}", fit(&shown_val, val_w), vw = val_w);
                    spans.push(Span::styled(
                        k_fit,
                        row_style.add_modifier(ratatui::style::Modifier::BOLD),
                    ));
                    spans.push(Span::styled("  ", row_style));
                    spans.push(Span::styled(
                        v_fit,
                        if active || m.reveal {
                            row_style
                        } else {
                            row_style.add_modifier(ratatui::style::Modifier::DIM)
                        },
                    ));
                    if badge_w > 0 {
                        let (badge, b_color) = match m.project_dotenv.get(&entry.key) {
                            Some(_) if !checked => ("  removes", Color::Red),
                            Some(cur) if cur == &entry.value => ("  in .env", Color::Green),
                            Some(_) => ("  differs", Color::Yellow),
                            None => ("         ", Color::Reset),
                        };
                        let b_style = if active {
                            row_style
                        } else {
                            Style::default().fg(b_color)
                        };
                        spans.push(Span::styled(badge, b_style));
                    }
                    Line::from(spans)
                })
                .collect();
            if (start..start + rows).contains(&vars.len()) {
                list.push(vault_new_row(
                    "variable",
                    m.sel >= vars.len() && m.editor.is_none(),
                ));
            }
            if !local_only.is_empty() && list.len() < rows {
                let dim = Style::default().add_modifier(ratatui::style::Modifier::DIM);
                for (k, v) in local_only.iter().take(rows - list.len()) {
                    let shown_val = if m.reveal {
                        (*v).clone()
                    } else {
                        envvault::mask_value(v)
                    };
                    list.push(Line::from(Span::styled(
                        format!(
                            "    {:<kw$}  {:<vw$}  local",
                            fit(k, key_w),
                            fit(&shown_val, val_w),
                            kw = key_w,
                            vw = val_w
                        ),
                        dim,
                    )));
                }
            }
            f.render_widget(Paragraph::new(list), parts[1]);
            if total > rows {
                scrollbar(f, popup, total, m.sel);
            }
        }
        if let (true, Some(ed)) = (has_editor, m.editor.as_ref()) {
            let ed_title = if ed.editing_key.is_some() {
                " Edit Shared Env Var "
            } else {
                " New Shared Env Var "
            };
            let ed_block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow))
                .title(Line::from(Span::styled(
                    ed_title,
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                )));
            let ed_inner = ed_block.inner(parts[2]);
            f.render_widget(ed_block, parts[2]);
            let avail = usize::from(ed_inner.width).saturating_sub(10).max(4);
            let mut ed_lines = Vec::with_capacity(3);
            for (fi, (label, input)) in [("Key", &ed.key), ("Value", &ed.value)].iter().enumerate()
            {
                ed_lines.push(render_editor_input_row(
                    label,
                    5,
                    input,
                    ed.field == fi,
                    avail,
                ));
            }
            ed_lines.push(editor_hint_or_err(
                &ed.err,
                usize::from(ed_inner.width),
                false,
            ));
            f.render_widget(Paragraph::new(ed_lines), ed_inner);
        }
        let foot = if let Some(k) = &m.confirm_delete {
            vault_delete_confirm_line("env", k)
        } else if m.editor.is_some() {
            Line::from(Span::styled(
                "Saved entries stay in your shared vault for any project",
                Style::default().fg(Color::DarkGray),
            ))
        } else {
            vault_footer_line(m.target, "write .env", can_pick, true, parts[3].width)
        };
        f.render_widget(Paragraph::new(foot), parts[3]);
    }

    fn draw_skill_modal(&self, f: &mut Frame) {
        let Some(m) = &self.skill_modal else {
            return;
        };
        let title = match m.target {
            VaultTarget::AddForm => " Shared Skills Vault -> new project ".to_string(),
            VaultTarget::Project(pi) => {
                let name = self
                    .cfg
                    .projects
                    .get(pi)
                    .map(|p| sanitize_chrome_line(&p.name))
                    .unwrap_or_else(|| "?".to_string());
                format!(" Shared Skills -> {name} (.agents/skills + .claude/skills) ")
            }
            VaultTarget::VaultOnly => " Shared Skills Vault ".to_string(),
        };
        let (popup, parts, has_editor) = vault_modal_layout(f, &title, m.editor.is_some(), 6);
        let Some(inner_parts) = parts else {
            return;
        };
        let can_pick = m.target != VaultTarget::VaultOnly;
        let show_badge =
            matches!(m.target, VaultTarget::Project(_)) || !m.project_state.vault_state.is_empty();
        let name_w = 20usize.min(usize::from(inner_parts[1].width) / 3);
        let badge_w = if show_badge { 11usize } else { 0 };
        let prefix = if can_pick { "    " } else { "  " };
        let desc_w = usize::from(inner_parts[1].width)
            .saturating_sub(if can_pick { 6 } else { 4 } + name_w + badge_w + 3)
            .max(8);
        if inner_parts[0].height > 0 {
            let head = if badge_w > 0 {
                format!(
                    "{prefix}{:<nw$}  {:<dw$}  status",
                    "Skill",
                    "Description / Source",
                    nw = name_w,
                    dw = desc_w
                )
            } else {
                format!(
                    "{prefix}{:<nw$}  Description / Source",
                    "Skill",
                    nw = name_w
                )
            };
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    head,
                    Style::default().add_modifier(ratatui::style::Modifier::DIM),
                ))),
                inner_parts[0],
            );
        }
        let rows = usize::from(inner_parts[1].height).max(1);
        let items = &self.skills_vault.skills;
        if items.is_empty() {
            let mut empty_lines = vec![Line::from(Span::styled(
                "  No shared skills registered yet. Press a to add one.",
                Style::default().add_modifier(ratatui::style::Modifier::DIM),
            ))];
            for name in m
                .project_state
                .local_only
                .iter()
                .take(rows.saturating_sub(1))
            {
                empty_lines.push(Line::from(Span::styled(
                    format!(
                        "    {:<nw$}  {:<dw$}  local",
                        fit(name, name_w),
                        "(project skill)",
                        nw = name_w,
                        dw = desc_w
                    ),
                    Style::default().add_modifier(ratatui::style::Modifier::DIM),
                )));
            }
            f.render_widget(Paragraph::new(empty_lines), inner_parts[1]);
        } else {
            let total = items.len() + 1;
            let start = keep_visible(m.top.get(), m.sel.min(total - 1), rows);
            m.top.set(start);
            let mut list: Vec<Line> = items
                .iter()
                .enumerate()
                .skip(start)
                .take(rows)
                .map(|(i, entry)| {
                    let active = i == m.sel;
                    let checked = m.picked.contains(&entry.name);
                    let row_style = vault_row_style(active);
                    let mut spans = vault_row_prefix(can_pick, active, checked, row_style);
                    let detail = if !entry.description.is_empty() {
                        entry.description.clone()
                    } else if let Some(src) = &entry.source_path {
                        src.clone()
                    } else {
                        entry
                            .instructions
                            .lines()
                            .next()
                            .unwrap_or("")
                            .trim()
                            .to_string()
                    };
                    spans.push(Span::styled(
                        format!("{:<nw$}", fit(&entry.name, name_w), nw = name_w),
                        row_style.add_modifier(ratatui::style::Modifier::BOLD),
                    ));
                    spans.push(Span::styled("  ", row_style));
                    spans.push(Span::styled(
                        format!("{:<dw$}", fit(&detail, desc_w), dw = desc_w),
                        if active {
                            row_style
                        } else {
                            row_style.add_modifier(ratatui::style::Modifier::DIM)
                        },
                    ));
                    if badge_w > 0 {
                        spans.push(vault_sync_badge(
                            m.project_state.vault_state.get(&entry.name).copied(),
                            checked,
                            active,
                            row_style,
                        ));
                    }
                    Line::from(spans)
                })
                .collect();
            if (start..start + rows).contains(&items.len()) {
                list.push(vault_new_row(
                    "skill",
                    m.sel >= items.len() && m.editor.is_none(),
                ));
            }
            if !m.project_state.local_only.is_empty() && list.len() < rows {
                let dim = Style::default().add_modifier(ratatui::style::Modifier::DIM);
                for name in m.project_state.local_only.iter().take(rows - list.len()) {
                    list.push(Line::from(Span::styled(
                        format!(
                            "    {:<nw$}  {:<dw$}  local",
                            fit(name, name_w),
                            "(project skill)",
                            nw = name_w,
                            dw = desc_w
                        ),
                        dim,
                    )));
                }
            }
            f.render_widget(Paragraph::new(list), inner_parts[1]);
            if total > rows {
                scrollbar(f, popup, total, m.sel);
            }
        }
        if let (true, Some(ed)) = (has_editor, m.editor.as_ref()) {
            let ed_title = if ed.editing_name.is_some() {
                " Edit Shared Skill "
            } else {
                " New Shared Skill "
            };
            let ed_block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow))
                .title(Line::from(Span::styled(
                    ed_title,
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                )));
            let ed_inner = ed_block.inner(inner_parts[2]);
            f.render_widget(ed_block, inner_parts[2]);
            let avail = usize::from(ed_inner.width).saturating_sub(14).max(4);
            let mut ed_lines = Vec::with_capacity(4);
            for (fi, (label, input)) in [
                ("Name", &ed.name),
                ("Desc", &ed.desc),
                ("Path/Body", &ed.body),
            ]
            .iter()
            .enumerate()
            {
                ed_lines.push(render_editor_input_row(
                    label,
                    9,
                    input,
                    ed.field == fi,
                    avail,
                ));
            }
            ed_lines.push(editor_hint_or_err(
                &ed.err,
                usize::from(ed_inner.width),
                false,
            ));
            f.render_widget(Paragraph::new(ed_lines), ed_inner);
        }
        let foot = if let Some(name) = &m.confirm_delete {
            vault_delete_confirm_line("skill", name)
        } else if m.editor.is_some() {
            Line::from(Span::styled(
                "Applies to .agents/skills + .claude/skills (agy, claude, muse, codex)",
                Style::default().fg(Color::DarkGray),
            ))
        } else {
            vault_footer_line(
                m.target,
                "sync skills",
                can_pick,
                false,
                inner_parts[3].width,
            )
        };
        f.render_widget(Paragraph::new(foot), inner_parts[3]);
    }

    fn draw_tool_modal(&self, f: &mut Frame) {
        let Some(m) = &self.tool_modal else {
            return;
        };
        let title = match m.target {
            VaultTarget::AddForm => " Shared Tools Vault -> new project ".to_string(),
            VaultTarget::Project(pi) => {
                let name = self
                    .cfg
                    .projects
                    .get(pi)
                    .map(|p| sanitize_chrome_line(&p.name))
                    .unwrap_or_else(|| "?".to_string());
                format!(" Shared Tools -> {name} (MCP + .squad/TOOLS.md) ")
            }
            VaultTarget::VaultOnly => " Shared Tools Vault ".to_string(),
        };
        let (popup, parts, has_editor) = vault_modal_layout(f, &title, m.editor.is_some(), 7);
        let Some(inner_parts) = parts else {
            return;
        };
        let can_pick = m.target != VaultTarget::VaultOnly;
        let show_badge =
            matches!(m.target, VaultTarget::Project(_)) || !m.project_state.vault_state.is_empty();
        let kind_w = 5usize;
        let name_w = 18usize.min(usize::from(inner_parts[1].width) / 4);
        let badge_w = if show_badge { 11usize } else { 0 };
        let prefix = if can_pick { "    " } else { "  " };
        let cmd_w = usize::from(inner_parts[1].width)
            .saturating_sub(if can_pick { 6 } else { 4 } + kind_w + name_w + badge_w + 5)
            .max(8);
        if inner_parts[0].height > 0 {
            let head = if badge_w > 0 {
                format!(
                    "{prefix}{:<kw$} {:<nw$}  {:<cw$}  status",
                    "Type",
                    "Tool",
                    "Command / URL",
                    kw = kind_w,
                    nw = name_w,
                    cw = cmd_w
                )
            } else {
                format!(
                    "{prefix}{:<kw$} {:<nw$}  Command / URL",
                    "Type",
                    "Tool",
                    kw = kind_w,
                    nw = name_w
                )
            };
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    head,
                    Style::default().add_modifier(ratatui::style::Modifier::DIM),
                ))),
                inner_parts[0],
            );
        }
        let rows = usize::from(inner_parts[1].height).max(1);
        let items = &self.tools_vault.tools;
        if items.is_empty() {
            let mut empty_lines = vec![Line::from(Span::styled(
                "  No shared tools registered yet. Press a to add an MCP or CLI tool.",
                Style::default().add_modifier(ratatui::style::Modifier::DIM),
            ))];
            for (name, kind) in m
                .project_state
                .local_only
                .iter()
                .take(rows.saturating_sub(1))
            {
                empty_lines.push(Line::from(Span::styled(
                    format!(
                        "    {:<kw$} {:<nw$}  {:<cw$}  local",
                        kind.as_str(),
                        fit(name, name_w),
                        "(project tool)",
                        kw = kind_w,
                        nw = name_w,
                        cw = cmd_w
                    ),
                    Style::default().add_modifier(ratatui::style::Modifier::DIM),
                )));
            }
            f.render_widget(Paragraph::new(empty_lines), inner_parts[1]);
        } else {
            let total = items.len() + 1;
            let start = keep_visible(m.top.get(), m.sel.min(total - 1), rows);
            m.top.set(start);
            let mut list: Vec<Line> = items
                .iter()
                .enumerate()
                .skip(start)
                .take(rows)
                .map(|(i, entry)| {
                    let active = i == m.sel;
                    let checked = m.picked.contains(&entry.name);
                    let row_style = vault_row_style(active);
                    let mut spans = vault_row_prefix(can_pick, active, checked, row_style);
                    let kind_color = match entry.kind {
                        ToolKind::Mcp => Color::Cyan,
                        ToolKind::Cli => Color::Magenta,
                    };
                    spans.push(Span::styled(
                        format!("{:<kw$} ", entry.kind.as_str(), kw = kind_w),
                        if active {
                            row_style
                        } else {
                            Style::default().fg(kind_color)
                        },
                    ));
                    spans.push(Span::styled(
                        format!("{:<nw$}", fit(&entry.name, name_w), nw = name_w),
                        row_style.add_modifier(ratatui::style::Modifier::BOLD),
                    ));
                    spans.push(Span::styled("  ", row_style));
                    spans.push(Span::styled(
                        format!("{:<cw$}", fit(&entry.command, cmd_w), cw = cmd_w),
                        if active {
                            row_style
                        } else {
                            row_style.add_modifier(ratatui::style::Modifier::DIM)
                        },
                    ));
                    if badge_w > 0 {
                        spans.push(vault_sync_badge(
                            m.project_state.vault_state.get(&entry.name).copied(),
                            checked,
                            active,
                            row_style,
                        ));
                    }
                    Line::from(spans)
                })
                .collect();
            if (start..start + rows).contains(&items.len()) {
                list.push(vault_new_row(
                    "tool",
                    m.sel >= items.len() && m.editor.is_none(),
                ));
            }
            if !m.project_state.local_only.is_empty() && list.len() < rows {
                let dim = Style::default().add_modifier(ratatui::style::Modifier::DIM);
                for (name, kind) in m.project_state.local_only.iter().take(rows - list.len()) {
                    list.push(Line::from(Span::styled(
                        format!(
                            "    {:<kw$} {:<nw$}  {:<cw$}  local",
                            kind.as_str(),
                            fit(name, name_w),
                            "(project tool)",
                            kw = kind_w,
                            nw = name_w,
                            cw = cmd_w
                        ),
                        dim,
                    )));
                }
            }
            f.render_widget(Paragraph::new(list), inner_parts[1]);
            if total > rows {
                scrollbar(f, popup, total, m.sel);
            }
        }
        if let (true, Some(ed)) = (has_editor, m.editor.as_ref()) {
            let ed_title = if ed.editing_name.is_some() {
                " Edit Shared Tool "
            } else {
                " New Shared Tool "
            };
            let ed_block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow))
                .title(Line::from(Span::styled(
                    ed_title,
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                )));
            let ed_inner = ed_block.inner(inner_parts[2]);
            f.render_widget(ed_block, inner_parts[2]);
            let avail = usize::from(ed_inner.width).saturating_sub(14).max(4);
            let kind_val = match ed.kind {
                ToolKind::Mcp => "mcp (Space toggles)",
                ToolKind::Cli => "cli (Space toggles)",
            };
            let kind_lbl_style = if ed.field == 0 {
                Style::default().add_modifier(ratatui::style::Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            let mut ed_lines = vec![Line::from(vec![
                Span::styled(
                    format!("{} {:<8}: ", if ed.field == 0 { ">" } else { " " }, "Kind"),
                    kind_lbl_style,
                ),
                Span::styled(
                    kind_val,
                    if ed.field == 0 {
                        Style::default().add_modifier(ratatui::style::Modifier::BOLD)
                    } else {
                        Style::default()
                    },
                ),
            ])];
            for (fi, (label, input)) in [
                (1usize, ("Name", &ed.name)),
                (2usize, ("Cmd/URL", &ed.command)),
                (3usize, ("Desc", &ed.desc)),
            ] {
                ed_lines.push(render_editor_input_row(
                    label,
                    8,
                    input,
                    ed.field == fi,
                    avail,
                ));
            }
            ed_lines.push(editor_hint_or_err(
                &ed.err,
                usize::from(ed_inner.width),
                ed.field == 0,
            ));
            f.render_widget(Paragraph::new(ed_lines), ed_inner);
        }
        let foot = if let Some(name) = &m.confirm_delete {
            vault_delete_confirm_line("tool", name)
        } else if m.editor.is_some() {
            Line::from(Span::styled(
                "MCP syncs .mcp.json + .agents/mcp_config.json + .codex/config.toml; CLI writes .squad/TOOLS.md",
                Style::default().fg(Color::DarkGray),
            ))
        } else {
            vault_footer_line(
                m.target,
                "sync tools",
                can_pick,
                false,
                inner_parts[3].width,
            )
        };
        f.render_widget(Paragraph::new(foot), inner_parts[3]);
    }

    fn draw_ref_modal(&self, f: &mut Frame) {
        let Some(m) = &self.ref_modal else {
            return;
        };
        let title = match m.target {
            VaultTarget::AddForm => " Shared References Vault -> new project ".to_string(),
            VaultTarget::Project(pi) => {
                let name = self
                    .cfg
                    .projects
                    .get(pi)
                    .map(|p| sanitize_chrome_line(&p.name))
                    .unwrap_or_else(|| "?".to_string());
                format!(" Shared References -> {name} (CLAUDE.md + AGENTS.md) ")
            }
            VaultTarget::VaultOnly => " Shared References Vault ".to_string(),
        };
        let (popup, parts, has_editor) = vault_modal_layout(f, &title, m.editor.is_some(), 6);
        let Some(inner_parts) = parts else {
            return;
        };
        let can_pick = m.target != VaultTarget::VaultOnly;
        let show_badge =
            matches!(m.target, VaultTarget::Project(_)) || !m.project_state.vault_state.is_empty();
        let name_w = 20usize.min(usize::from(inner_parts[1].width) / 3);
        let badge_w = if show_badge { 11usize } else { 0 };
        let prefix = if can_pick { "    " } else { "  " };
        let detail_w = usize::from(inner_parts[1].width)
            .saturating_sub(if can_pick { 6 } else { 4 } + name_w + badge_w + 5)
            .max(8);
        if inner_parts[0].height > 0 {
            let head = if badge_w > 0 {
                format!(
                    "{prefix}{:<nw$}  {:<dw$}  status",
                    "Name",
                    "URL / Description",
                    nw = name_w,
                    dw = detail_w
                )
            } else {
                format!("{prefix}{:<nw$}  URL / Description", "Name", nw = name_w)
            };
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    head,
                    Style::default().add_modifier(ratatui::style::Modifier::DIM),
                ))),
                inner_parts[0],
            );
        }
        let rows = usize::from(inner_parts[1].height).max(1);
        let items = &self.refs_vault.references;
        if items.is_empty() {
            let mut empty_lines = vec![Line::from(Span::styled(
                "  No shared references registered yet. Press a to add a GitHub link.",
                Style::default().add_modifier(ratatui::style::Modifier::DIM),
            ))];
            for name in m
                .project_state
                .local_only
                .iter()
                .take(rows.saturating_sub(1))
            {
                empty_lines.push(Line::from(Span::styled(
                    format!(
                        "    {:<nw$}  {:<dw$}  local",
                        fit(name, name_w),
                        "(project reference)",
                        nw = name_w,
                        dw = detail_w
                    ),
                    Style::default().add_modifier(ratatui::style::Modifier::DIM),
                )));
            }
            f.render_widget(Paragraph::new(empty_lines), inner_parts[1]);
        } else {
            let total = items.len() + 1;
            let start = keep_visible(m.top.get(), m.sel.min(total - 1), rows);
            m.top.set(start);
            let mut list: Vec<Line> = items
                .iter()
                .enumerate()
                .skip(start)
                .take(rows)
                .map(|(i, entry)| {
                    let active = i == m.sel;
                    let checked = m.picked.contains(&entry.name);
                    let row_style = vault_row_style(active);
                    let mut spans = vault_row_prefix(can_pick, active, checked, row_style);
                    spans.push(Span::styled(
                        format!("{:<nw$}", fit(&entry.name, name_w), nw = name_w),
                        row_style.add_modifier(ratatui::style::Modifier::BOLD),
                    ));
                    spans.push(Span::styled("  ", row_style));
                    let detail = if entry.description.is_empty() {
                        entry.url.clone()
                    } else {
                        format!("{} — {}", entry.url, entry.description)
                    };
                    spans.push(Span::styled(
                        format!("{:<dw$}", fit(&detail, detail_w), dw = detail_w),
                        if active {
                            row_style
                        } else {
                            row_style.add_modifier(ratatui::style::Modifier::DIM)
                        },
                    ));
                    if badge_w > 0 {
                        spans.push(vault_sync_badge(
                            m.project_state.vault_state.get(&entry.name).copied(),
                            checked,
                            active,
                            row_style,
                        ));
                    }
                    Line::from(spans)
                })
                .collect();
            if (start..start + rows).contains(&items.len()) {
                list.push(vault_new_row(
                    "reference",
                    m.sel >= items.len() && m.editor.is_none(),
                ));
            }
            if !m.project_state.local_only.is_empty() && list.len() < rows {
                let dim = Style::default().add_modifier(ratatui::style::Modifier::DIM);
                for name in m.project_state.local_only.iter().take(rows - list.len()) {
                    list.push(Line::from(Span::styled(
                        format!(
                            "    {:<nw$}  {:<dw$}  local",
                            fit(name, name_w),
                            "(project reference)",
                            nw = name_w,
                            dw = detail_w
                        ),
                        dim,
                    )));
                }
            }
            f.render_widget(Paragraph::new(list), inner_parts[1]);
            if total > rows {
                scrollbar(f, popup, total, m.sel);
            }
        }
        if let (true, Some(ed)) = (has_editor, m.editor.as_ref()) {
            let ed_title = if ed.editing_name.is_some() {
                " Edit Shared Reference "
            } else {
                " New Shared Reference "
            };
            let ed_block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow))
                .title(Line::from(Span::styled(
                    ed_title,
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                )));
            let ed_inner = ed_block.inner(inner_parts[2]);
            f.render_widget(ed_block, inner_parts[2]);
            let avail = usize::from(ed_inner.width).saturating_sub(11).max(4);
            let mut ed_lines = Vec::with_capacity(4);
            for (fi, (label, input)) in [("Name", &ed.name), ("URL", &ed.url), ("Desc", &ed.desc)]
                .iter()
                .enumerate()
            {
                ed_lines.push(render_editor_input_row(
                    label,
                    5,
                    input,
                    ed.field == fi,
                    avail,
                ));
            }
            ed_lines.push(editor_hint_or_err(
                &ed.err,
                usize::from(ed_inner.width),
                false,
            ));
            f.render_widget(Paragraph::new(ed_lines), ed_inner);
        }
        let foot = if let Some(name) = &m.confirm_delete {
            vault_delete_confirm_line("reference", name)
        } else if m.editor.is_some() {
            Line::from(Span::styled(
                "Syncs CLAUDE.md + AGENTS.md (+ GEMINI.md if present); blank Name uses URL slug",
                Style::default().fg(Color::DarkGray),
            ))
        } else {
            vault_footer_line(m.target, "sync refs", can_pick, false, inner_parts[3].width)
        };
        f.render_widget(Paragraph::new(foot), inner_parts[3]);
    }

    // ---- input ----
}

fn picked_summary(set: &BTreeSet<String>, vault_len: usize) -> String {
    if set.is_empty() {
        if vault_len == 0 {
            "(none · Space/Enter adds)".to_string()
        } else {
            format!("({vault_len} shared · Space/Enter picks)")
        }
    } else {
        let first = set.iter().next().map(String::as_str).unwrap_or("");
        if set.len() == 1 {
            format!("{first} (Space/Enter edits)")
        } else {
            format!("{first} +{} (Space/Enter edits)", set.len() - 1)
        }
    }
}

fn vault_modal_layout(
    f: &mut Frame,
    title: &str,
    has_editor_flag: bool,
    editor_rows: u16,
) -> (Rect, Option<std::rc::Rc<[Rect]>>, bool) {
    let popup = vault_popup(f.area());
    f.render_widget(ratatui::widgets::Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Line::from(Span::styled(
            title.to_string(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(ratatui::style::Modifier::BOLD),
        )));
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    if inner.height == 0 {
        return (popup, None, false);
    }
    let has_editor = has_editor_flag && inner.height >= editor_rows + 4;
    let header_h: u16 = u16::from(inner.height >= 4);
    let editor_h: u16 = if has_editor { editor_rows } else { 0 };
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_h),
            Constraint::Min(1),
            Constraint::Length(editor_h),
            Constraint::Length(1),
        ])
        .split(inner);
    (popup, Some(parts), has_editor)
}

fn vault_row_style(active: bool) -> Style {
    if active {
        Style::default().bg(Color::Cyan).fg(Color::Black)
    } else {
        Style::default()
    }
}

fn vault_row_prefix(
    can_pick: bool,
    active: bool,
    checked: bool,
    row_style: Style,
) -> Vec<Span<'static>> {
    if can_pick {
        let mark = if checked { "[x] " } else { "[ ] " };
        let mark_style = if active {
            row_style.add_modifier(ratatui::style::Modifier::BOLD)
        } else if checked {
            Style::default()
                .fg(Color::Green)
                .add_modifier(ratatui::style::Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        vec![Span::styled(mark, mark_style)]
    } else {
        vec![Span::styled(if active { "▸ " } else { "  " }, row_style)]
    }
}

fn vault_sync_badge(
    state: Option<SyncState>,
    checked: bool,
    active: bool,
    row_style: Style,
) -> Span<'static> {
    let (badge, b_color) = match state {
        Some(_) if !checked => ("  removes  ", Color::Red),
        Some(SyncState::Installed) => ("  installed", Color::Green),
        Some(SyncState::Differs) => ("  differs  ", Color::Yellow),
        None => ("           ", Color::Reset),
    };
    let b_style = if active {
        row_style
    } else {
        Style::default().fg(b_color)
    };
    Span::styled(badge, b_style)
}

fn vault_new_row(noun: &str, active: bool) -> Line<'static> {
    let style = if active {
        Style::default().bg(Color::Cyan).fg(Color::Black)
    } else {
        Style::default().fg(Color::Green)
    };
    Line::from(vec![
        Span::styled(
            format!("{} + New {noun}…", if active { "▸" } else { " " }),
            style.add_modifier(ratatui::style::Modifier::BOLD),
        ),
        Span::styled(
            "  (a)",
            if active {
                style
            } else {
                Style::default().add_modifier(ratatui::style::Modifier::DIM)
            },
        ),
    ])
}

fn render_editor_input_row(
    label: &str,
    label_w: usize,
    input: &LineInput,
    active: bool,
    avail: usize,
) -> Line<'static> {
    let mark = if active { ">" } else { " " };
    let lbl_style = if active {
        Style::default().add_modifier(ratatui::style::Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let mut row = vec![Span::styled(
        format!("{mark} {label:<lw$}: ", lw = label_w),
        lbl_style,
    )];
    if active {
        let (l, c, r) = input.render_parts();
        let (l, c, r) = input_window(&l, c, &r, avail);
        let vs = Style::default().add_modifier(ratatui::style::Modifier::BOLD);
        row.push(Span::styled(l, vs));
        row.push(Span::styled(
            c.to_string(),
            vs.bg(Color::White).fg(Color::Black),
        ));
        row.push(Span::styled(r, vs));
    } else {
        row.push(Span::raw(fit(&input.as_str(), avail)));
    }
    Line::from(row)
}

fn editor_hint_or_err(err: &str, max_w: usize, has_toggle: bool) -> Line<'static> {
    if !err.is_empty() {
        Line::from(Span::styled(
            fit(err, max_w),
            Style::default().fg(Color::Red),
        ))
    } else if has_toggle {
        Line::from(hint_spans(&[
            ("Space", "toggle"),
            ("Tab", "next"),
            ("Enter", "save"),
            ("Esc", "cancel"),
        ]))
    } else {
        Line::from(hint_spans(&[
            ("Tab", "switch field"),
            ("Enter", "save"),
            ("Esc", "cancel"),
        ]))
    }
}

fn vault_delete_confirm_line(kind: &str, name: &str) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!("Delete shared {kind} '{name}'?  "),
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(ratatui::style::Modifier::BOLD),
    )];
    spans.extend(hint_spans(&[
        ("y / Enter", "deletes"),
        ("n / Esc", "cancels"),
    ]));
    Line::from(spans)
}

fn vault_footer_line(
    target: VaultTarget,
    project_verb: &str,
    can_pick: bool,
    has_reveal: bool,
    width: u16,
) -> Line<'static> {
    let mut hints: Vec<(&str, &str)> = Vec::new();
    match target {
        VaultTarget::AddForm => hints.push(("Enter", "done")),
        VaultTarget::Project(_) => hints.push(("Enter", project_verb)),
        VaultTarget::VaultOnly => {}
    }
    if can_pick {
        hints.push(("Space", "toggle"));
    }
    hints.extend([("a", "add"), ("e", "edit"), ("d", "del")]);
    if can_pick {
        hints.push(("A", "all"));
    }
    if has_reveal {
        hints.push(("v", "reveal"));
    }
    hints.push(("Esc", "close"));
    Line::from(hint_spans(hints_that_fit(&hints, usize::from(width))))
}
