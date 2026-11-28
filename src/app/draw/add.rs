use super::*;

impl App {
    /// The name the add form would register, shown while typing. An edit
    /// shows it only when a new folder would rename the agent.
    pub(crate) fn add_preview(&self) -> Option<String> {
        if let Some(old) = &self.add.edit {
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
            let name = preview_name(&others, &self.add_full_path(), Kind::Agent, &[])?;
            return (!name.eq_ignore_ascii_case(old)).then_some(name);
        }
        let parts = split_command(&self.add.cmd.as_str());
        preview_name(
            &self.cfg.projects,
            &self.add_full_path(),
            self.add.kind,
            &parts,
        )
    }

    /// The Server row above Path: this PC or a registered server (`V`).
    fn add_server_line(&self, modal_w: usize) -> Line<'static> {
        let active = self.add.field == 9;
        let label_style = if active {
            Style::default().add_modifier(ratatui::style::Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let value = match self.add_server_entry() {
            Some(s) => format!("{} ({}) · ←→ picks", s.key, s.value),
            None if self.servers_vault.vars.is_empty() => {
                "local (this PC) · V in manager registers servers".to_string()
            }
            None => "local (this PC) · ←→ picks".to_string(),
        };
        let label = format!("{} Server: ", if active { ">" } else { " " });
        let value = fit(&value, modal_w.saturating_sub(label.chars().count()));
        Line::from(vec![
            Span::styled(label, label_style),
            Span::styled(
                value,
                if active {
                    label_style
                } else {
                    Style::default()
                },
            ),
        ])
    }

    /// Does the typed path exist? Shown at the right end of the Path row.
    fn add_mark(&self, modal_w: usize) -> (&'static str, Color) {
        let compact = modal_w < 60;
        match self.add.state {
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
        let text = if self.add.edit.is_some() {
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
    pub(crate) fn add_list_h(&self) -> u16 {
        if self.add.field == 0 && self.add_server_entry().is_none() {
            1 + PICK_ROWS as u16
        } else {
            0
        }
    }

    /// The rows of `add_picks` on screen: at most `PICK_ROWS`, scrolled smoothly
    /// via `keep_visible` so stepping up/down never jumps early.
    pub(crate) fn add_pick_window(&self) -> std::ops::Range<usize> {
        if self.add.field != 0 {
            return 0..0;
        }
        let n = self.add.picks.len();
        let max_start = n.saturating_sub(PICK_ROWS);
        let start = match self.add.sel {
            Some(s) => keep_visible(self.add.top.get().min(max_start), s, PICK_ROWS),
            None => 0,
        };
        self.add.top.set(start);
        start..(start + PICK_ROWS).min(n)
    }

    fn add_err_lines(&self, max_w: usize) -> Vec<Line<'static>> {
        if self.add.err.is_empty() {
            return Vec::new();
        }
        let msg = format!("Error: {}", self.add.err);
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
    pub(crate) fn add_popup(&self, area: Rect) -> Rect {
        let modal_w = usize::from(72u16.min(area.width)).saturating_sub(2);
        let err_rows = self.add_err_lines(modal_w).len() as u16;
        let rows = 10 + self.add_list_h() + u16::from(self.add_status().is_some()) + err_rows;
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
        let typed = self.add.path.as_str();
        let n = self.add.picks.len();
        let win = self.add_pick_window();
        let count_note = if n > PICK_ROWS && !win.is_empty() {
            format!("{}-{} of {n}", win.start + 1, win.end)
        } else {
            format!("{n}")
        };
        let noun = if n == 1 { "folder" } else { "folders" };
        let header = match self.add.picks.first() {
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
            let (label, _) = &self.add.picks[i];
            let on = self.add.sel == Some(i);
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

    pub(crate) fn draw_add(&self, f: &mut Frame) {
        let popup = self.add_popup(f.area());
        let modal_w = (popup.width as usize).saturating_sub(2);
        f.render_widget(ratatui::widgets::Clear, popup);
        let editing = self.add.edit.is_some();
        let kind_hint = match (self.add.kind == Kind::Agent, editing) {
            (true, false) => "agent (Space toggles)",
            (false, false) => "service (Space toggles)",
            (true, true) => "agent (fixed)",
            (false, true) => "service (fixed)",
        };
        let cmd_text = if self.add.cmd.is_empty() {
            "(default shell)".to_string()
        } else {
            self.add.cmd.as_str()
        };
        let path_label = match (self.add_server_entry(), self.add.kind) {
            (Some(_), _) => "Folder on server",
            (None, Kind::Service) => "Path (optional)",
            (None, _) => "Path",
        };
        let autostart_hint = if self.add.autostart {
            "yes (Space toggles)".to_string()
        } else {
            "no (Space toggles)".to_string()
        };
        let under_text = if editing {
            let cur = self.add.under.as_str();
            if cur.is_empty() {
                "(none · use u in manager)".to_string()
            } else {
                format!("{cur} (use u in manager)")
            }
        } else if self.add.kind == Kind::Service {
            "(agents only)".to_string()
        } else {
            self.add.under.as_str()
        };
        let env_text = picked_summary(&self.add.env, self.env_vault.vars.len());
        let skills_text = picked_summary(&self.add.skills, self.skills_vault.skills.len());
        let tools_text = picked_summary(&self.add.tools, self.tools_vault.tools.len());
        let refs_text = picked_summary(&self.add.refs, self.refs_vault.references.len());
        let fields = [
            (path_label, self.add.path.as_str()),
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
        let mut lines = vec![self.add_server_line(modal_w)];
        for (i, (label, val)) in fields.iter().enumerate() {
            let active = i == self.add.field;
            let locked = (editing && !matches!(i, 0 | 2 | 3 | 5 | 6 | 7 | 8 | 9))
                || (!editing && self.add.kind == Kind::Service && i == 4);
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
                    0 => &self.add.path,
                    2 => &self.add.cmd,
                    _ => &self.add.under,
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
                if i == 2 && self.add.cmd.is_empty() {
                    row.push(Span::styled(
                        "(default shell)".to_string(),
                        Style::default().fg(Color::DarkGray),
                    ));
                } else if i == 4 && self.add.under.is_empty() {
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
