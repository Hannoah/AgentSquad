use super::*;

impl App {
    fn vault_title(&self, m: &VaultModal) -> String {
        let project_name = |pi: usize| {
            self.cfg
                .projects
                .get(pi)
                .map(|p| sanitize_chrome_line(&p.name))
                .unwrap_or_else(|| "?".to_string())
        };
        match (&m.kind, m.target) {
            (VaultKindState::Env { .. }, VaultTarget::AddForm) => {
                " Environment Variable -> new project .env ".to_string()
            }
            (VaultKindState::Env { .. }, VaultTarget::Project(pi)) => {
                format!(" Environment Variable -> {}/.env ", project_name(pi))
            }
            (VaultKindState::Env { .. }, VaultTarget::VaultOnly) => {
                " Environment Variable ".to_string()
            }
            (VaultKindState::Skill { .. }, VaultTarget::AddForm) => {
                " Shared Skills Vault -> new project ".to_string()
            }
            (VaultKindState::Skill { .. }, VaultTarget::Project(pi)) => {
                format!(
                    " Shared Skills -> {} (.agents/skills + .claude/skills) ",
                    project_name(pi)
                )
            }
            (VaultKindState::Skill { .. }, VaultTarget::VaultOnly) => {
                " Shared Skills Vault ".to_string()
            }
            (VaultKindState::Tool { .. }, VaultTarget::AddForm) => {
                " Shared Tools Vault -> new project ".to_string()
            }
            (VaultKindState::Tool { .. }, VaultTarget::Project(pi)) => {
                format!(
                    " Shared Tools -> {} (MCP + .squad/TOOLS.md) ",
                    project_name(pi)
                )
            }
            (VaultKindState::Tool { .. }, VaultTarget::VaultOnly) => {
                " Shared Tools Vault ".to_string()
            }
            (VaultKindState::Ref { .. }, VaultTarget::AddForm) => {
                " Shared References Vault -> new project ".to_string()
            }
            (VaultKindState::Ref { .. }, VaultTarget::Project(pi)) => {
                format!(
                    " Shared References -> {} (CLAUDE.md + AGENTS.md) ",
                    project_name(pi)
                )
            }
            (VaultKindState::Ref { .. }, VaultTarget::VaultOnly) => {
                " Shared References Vault ".to_string()
            }
            (VaultKindState::Model { .. }, VaultTarget::Project(pi)) => {
                format!(" LLM Models -> {}/.env ", project_name(pi))
            }
            (VaultKindState::Model { .. }, _) => " LLM Models ".to_string(),
            (VaultKindState::Server { .. }, _) => " Servers ".to_string(),
        }
    }

    pub(crate) fn draw_vault_modal(&self, f: &mut Frame) {
        let Some(m) = &self.vault_modal else {
            return;
        };
        let title = self.vault_title(m);
        let (popup, parts, has_editor) =
            vault_modal_layout(f, &title, m.has_editor(), m.editor_rows());
        let Some(parts) = parts else {
            return;
        };

        self.draw_vault_list(f, m, popup, parts[0], parts[1]);

        if let (true, true) = (has_editor, m.has_editor()) {
            self.draw_vault_editor(f, m, parts[2]);
        }

        let can_pick = m.target != VaultTarget::VaultOnly;
        let foot = match &m.kind {
            VaultKindState::Skill {
                confirm_overwrite: Some(names),
                ..
            } => skill_overwrite_confirm_line(names),
            _ if m.confirm_delete.is_some() => vault_delete_confirm_line(
                m.delete_kind(),
                m.confirm_delete.as_deref().unwrap_or(""),
            ),
            _ if m.has_editor() => Line::from(Span::styled(
                m.editor_footer_hint(),
                Style::default().fg(Color::DarkGray),
            )),
            _ => vault_footer_line(
                m.target,
                m.footer_verb(),
                can_pick,
                m.is_env() || m.is_model(),
                m.is_model(),
                parts[3].width,
            ),
        };
        f.render_widget(Paragraph::new(foot), parts[3]);
    }

    fn draw_vault_list(
        &self,
        f: &mut Frame,
        m: &VaultModal,
        popup: Rect,
        header_area: Rect,
        list_area: Rect,
    ) {
        let inner_w = usize::from(list_area.width);
        let can_pick = m.target != VaultTarget::VaultOnly;
        let prefix = if can_pick { "    " } else { "  " };
        let prefix_cols = if can_pick { 6 } else { 4 };
        let rows = usize::from(list_area.height).max(1);
        let len = self.vault_len(&m.kind);

        let (kind_w, name_w, badge_w, detail_w, head_text) = match &m.kind {
            VaultKindState::Env { project_dotenv, .. } => {
                let show_badge =
                    matches!(m.target, VaultTarget::Project(_)) || !project_dotenv.is_empty();
                let kw = 22usize.min(inner_w / 3);
                let bw = if show_badge { 9usize } else { 0 };
                let vw = inner_w.saturating_sub(prefix_cols + kw + bw + 3).max(6);
                let head = if bw > 0 {
                    format!(
                        "{prefix}{:<kw$}  {:<vw$}  .env",
                        "Key",
                        "Value",
                        kw = kw,
                        vw = inner_w.saturating_sub(prefix.len() + kw + bw + 5)
                    )
                } else {
                    format!("{prefix}{:<kw$}  Value", "Key", kw = kw)
                };
                (0, kw, bw, vw, head)
            }
            VaultKindState::Skill { project_state, .. } => {
                let show_badge = matches!(m.target, VaultTarget::Project(_))
                    || !project_state.vault_state.is_empty();
                let nw = 20usize.min(inner_w / 3);
                let bw = if show_badge { 11usize } else { 0 };
                let dw = inner_w.saturating_sub(prefix_cols + nw + bw + 3).max(8);
                let head = if bw > 0 {
                    format!(
                        "{prefix}{:<nw$}  {:<dw$}  status",
                        "Skill",
                        "Description / Source",
                        nw = nw,
                        dw = dw
                    )
                } else {
                    format!("{prefix}{:<nw$}  Description / Source", "Skill", nw = nw)
                };
                (0, nw, bw, dw, head)
            }
            VaultKindState::Tool { project_state, .. } => {
                let show_badge = matches!(m.target, VaultTarget::Project(_))
                    || !project_state.vault_state.is_empty();
                let kw = 5usize;
                let nw = 18usize.min(inner_w / 4);
                let bw = if show_badge { 11usize } else { 0 };
                let cw = inner_w
                    .saturating_sub(prefix_cols + kw + nw + bw + 5)
                    .max(8);
                let head = if bw > 0 {
                    format!(
                        "{prefix}{:<kw$} {:<nw$}  {:<cw$}  status",
                        "Type",
                        "Tool",
                        "Command / URL",
                        kw = kw,
                        nw = nw,
                        cw = cw
                    )
                } else {
                    format!(
                        "{prefix}{:<kw$} {:<nw$}  Command / URL",
                        "Type",
                        "Tool",
                        kw = kw,
                        nw = nw
                    )
                };
                (kw, nw, bw, cw, head)
            }
            VaultKindState::Ref { project_state, .. } => {
                let show_badge = matches!(m.target, VaultTarget::Project(_))
                    || !project_state.vault_state.is_empty();
                let nw = 20usize.min(inner_w / 3);
                let bw = if show_badge { 11usize } else { 0 };
                let dw = inner_w.saturating_sub(prefix_cols + nw + bw + 5).max(8);
                let head = if bw > 0 {
                    format!(
                        "{prefix}{:<nw$}  {:<dw$}  status",
                        "Name",
                        "URL / Description",
                        nw = nw,
                        dw = dw
                    )
                } else {
                    format!("{prefix}{:<nw$}  URL / Description", "Name", nw = nw)
                };
                (0, nw, bw, dw, head)
            }
            VaultKindState::Model { .. } => {
                let show_badge = matches!(m.target, VaultTarget::Project(_));
                let kw = 9usize;
                let nw = 18usize.min(inner_w / 4);
                let bw = if show_badge { 11usize } else { 0 };
                let dw = inner_w
                    .saturating_sub(prefix_cols + kw + nw + bw + 5)
                    .max(8);
                let head = format!(
                    "{prefix}{:<kw$} {:<nw$}  {:<dw$}{}",
                    "Provider",
                    "Name",
                    "Model / Endpoint / Token",
                    if bw > 0 { "  .env" } else { "" },
                );
                (kw, nw, bw, dw, head)
            }
            VaultKindState::Server { .. } => {
                let nw = 20usize.min(inner_w / 3);
                let dw = inner_w.saturating_sub(prefix_cols + nw + 3).max(8);
                (0, nw, 0, dw, format!("{prefix}{:<nw$}  Address", "Name"))
            }
        };

        if header_area.height > 0 {
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    head_text,
                    Style::default().add_modifier(ratatui::style::Modifier::DIM),
                ))),
                header_area,
            );
        }

        let local_lines = self.vault_local_only_lines(m, kind_w, name_w, detail_w);
        if len == 0 {
            let mut empty_lines = vec![Line::from(Span::styled(
                m.empty_hint(),
                Style::default().add_modifier(ratatui::style::Modifier::DIM),
            ))];
            empty_lines.extend(local_lines.into_iter().take(rows.saturating_sub(1)));
            f.render_widget(Paragraph::new(empty_lines), list_area);
            return;
        }

        let total = len + 1;
        let start = keep_visible(m.top.get(), m.sel.min(total - 1), rows);
        m.top.set(start);

        let mut list: Vec<Line> = (start..len.min(start + rows))
            .map(|i| self.vault_entry_line(m, i, kind_w, name_w, detail_w, badge_w, can_pick))
            .collect();

        if (start..start + rows).contains(&len) {
            list.push(vault_new_row(m.noun(), m.sel >= len && !m.has_editor()));
        }
        if list.len() < rows {
            list.extend(local_lines.into_iter().take(rows - list.len()));
        }
        f.render_widget(Paragraph::new(list), list_area);
        if total > rows {
            crate::app::draw::scrollbar(f, popup, total, m.sel);
        }
    }

    fn vault_local_only_lines(
        &self,
        m: &VaultModal,
        kind_w: usize,
        name_w: usize,
        detail_w: usize,
    ) -> Vec<Line<'static>> {
        let dim = Style::default().add_modifier(ratatui::style::Modifier::DIM);
        match &m.kind {
            VaultKindState::Env {
                project_dotenv,
                reveal,
                ..
            } => project_dotenv
                .iter()
                .filter(|(k, _)| !self.env_vault.vars.iter().any(|v| v.key == **k))
                .map(|(k, v)| {
                    let shown_val = if *reveal {
                        v.clone()
                    } else {
                        envvault::mask_value(v)
                    };
                    Line::from(Span::styled(
                        format!(
                            "    {:<kw$}  {:<vw$}  local",
                            fit(k, name_w),
                            fit(&shown_val, detail_w),
                            kw = name_w,
                            vw = detail_w
                        ),
                        dim,
                    ))
                })
                .collect(),
            VaultKindState::Skill { project_state, .. } => project_state
                .local_only
                .iter()
                .map(|name| {
                    Line::from(Span::styled(
                        format!(
                            "    {:<nw$}  {:<dw$}  local",
                            fit(name, name_w),
                            "(project skill)",
                            nw = name_w,
                            dw = detail_w
                        ),
                        dim,
                    ))
                })
                .collect(),
            VaultKindState::Tool { project_state, .. } => project_state
                .local_only
                .iter()
                .map(|(name, kind)| {
                    Line::from(Span::styled(
                        format!(
                            "    {:<kw$} {:<nw$}  {:<cw$}  local",
                            kind.as_str(),
                            fit(name, name_w),
                            "(project tool)",
                            kw = kind_w,
                            nw = name_w,
                            cw = detail_w
                        ),
                        dim,
                    ))
                })
                .collect(),
            VaultKindState::Ref { project_state, .. } => project_state
                .local_only
                .iter()
                .map(|name| {
                    Line::from(Span::styled(
                        format!(
                            "    {:<nw$}  {:<dw$}  local",
                            fit(name, name_w),
                            "(project reference)",
                            nw = name_w,
                            dw = detail_w
                        ),
                        dim,
                    ))
                })
                .collect(),
            VaultKindState::Model { .. } | VaultKindState::Server { .. } => Vec::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn vault_entry_line(
        &self,
        m: &VaultModal,
        i: usize,
        kind_w: usize,
        name_w: usize,
        detail_w: usize,
        badge_w: usize,
        can_pick: bool,
    ) -> Line<'static> {
        let active = i == m.sel;
        let row_style = vault_row_style(active);
        match &m.kind {
            VaultKindState::Env {
                project_dotenv,
                reveal,
                ..
            } => {
                let entry = &self.env_vault.vars[i];
                let checked = m.picked.contains(&entry.key);
                let mut spans = vault_row_prefix(can_pick, active, checked, row_style);
                let shown_val = if *reveal {
                    entry.value.clone()
                } else {
                    envvault::mask_value(&entry.value)
                };
                spans.push(Span::styled(
                    format!("{:<kw$}", fit(&entry.key, name_w), kw = name_w),
                    row_style.add_modifier(ratatui::style::Modifier::BOLD),
                ));
                spans.push(Span::styled("  ", row_style));
                spans.push(Span::styled(
                    format!("{:<vw$}", fit(&shown_val, detail_w), vw = detail_w),
                    if active || *reveal {
                        row_style
                    } else {
                        row_style.add_modifier(ratatui::style::Modifier::DIM)
                    },
                ));
                if badge_w > 0 {
                    let (badge, b_color) = match project_dotenv.get(&entry.key) {
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
            }
            VaultKindState::Skill { project_state, .. } => {
                let entry = &self.skills_vault.skills[i];
                let checked = m.picked.contains(&entry.name);
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
                push_vault_name_detail(
                    &mut spans,
                    &entry.name,
                    &detail,
                    name_w,
                    detail_w,
                    active,
                    row_style,
                );
                if badge_w > 0 {
                    spans.push(vault_sync_badge(
                        project_state.vault_state.get(&entry.name).copied(),
                        checked,
                        active,
                        row_style,
                    ));
                }
                Line::from(spans)
            }
            VaultKindState::Tool { project_state, .. } => {
                let entry = &self.tools_vault.tools[i];
                let checked = m.picked.contains(&entry.name);
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
                push_vault_name_detail(
                    &mut spans,
                    &entry.name,
                    &entry.command,
                    name_w,
                    detail_w,
                    active,
                    row_style,
                );
                if badge_w > 0 {
                    spans.push(vault_sync_badge(
                        project_state.vault_state.get(&entry.name).copied(),
                        checked,
                        active,
                        row_style,
                    ));
                }
                Line::from(spans)
            }
            VaultKindState::Ref { project_state, .. } => {
                let entry = &self.refs_vault.references[i];
                let checked = m.picked.contains(&entry.name);
                let mut spans = vault_row_prefix(can_pick, active, checked, row_style);
                let detail = if entry.description.is_empty() {
                    entry.url.clone()
                } else {
                    format!("{} — {}", entry.url, entry.description)
                };
                push_vault_name_detail(
                    &mut spans,
                    &entry.name,
                    &detail,
                    name_w,
                    detail_w,
                    active,
                    row_style,
                );
                if badge_w > 0 {
                    spans.push(vault_sync_badge(
                        project_state.vault_state.get(&entry.name).copied(),
                        checked,
                        active,
                        row_style,
                    ));
                }
                Line::from(spans)
            }
            VaultKindState::Model {
                project_dotenv,
                reveal,
                ..
            } => {
                let entry = &self.models_vault.models[i];
                let checked = m.picked.contains(&entry.name);
                let mut spans = vault_row_prefix(can_pick, active, checked, row_style);
                spans.push(Span::styled(
                    format!("{:<kw$} ", entry.provider.as_str(), kw = kind_w),
                    if active {
                        row_style
                    } else {
                        Style::default().fg(Color::Cyan)
                    },
                ));
                let token = if *reveal {
                    entry.token.clone()
                } else {
                    envvault::mask_value(&entry.token)
                };
                let detail = [
                    entry.model.as_str(),
                    entry.endpoint.as_str(),
                    token.as_str(),
                ]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("  ");
                push_vault_name_detail(
                    &mut spans,
                    &entry.name,
                    &detail,
                    name_w,
                    detail_w,
                    active,
                    row_style,
                );
                if badge_w > 0 {
                    let applied = entry.applied(project_dotenv);
                    let (badge, color) = match (applied, checked) {
                        (true, true) => ("  in .env", Color::Green),
                        (true, false) => ("  removes", Color::Red),
                        (false, true) => ("  writes", Color::Yellow),
                        (false, false) => ("", Color::Reset),
                    };
                    spans.push(Span::styled(
                        badge,
                        if active {
                            row_style
                        } else {
                            Style::default().fg(color)
                        },
                    ));
                }
                Line::from(spans)
            }
            VaultKindState::Server { .. } => {
                let entry = &self.servers_vault.vars[i];
                let mut spans = vault_row_prefix(false, active, false, row_style);
                push_vault_name_detail(
                    &mut spans,
                    &entry.key,
                    &entry.value,
                    name_w,
                    detail_w,
                    active,
                    row_style,
                );
                Line::from(spans)
            }
        }
    }

    fn draw_vault_editor(&self, f: &mut Frame, m: &VaultModal, area: Rect) {
        let (ed_title, is_edit) = match &m.kind {
            VaultKindState::Env {
                editor: Some(ed), ..
            } => (
                if ed.editing_key.is_some() {
                    " Edit Shared Env Var "
                } else {
                    " New Shared Env Var "
                },
                ed.editing_key.is_some(),
            ),
            VaultKindState::Skill {
                editor: Some(ed), ..
            } => (
                if ed.editing_name.is_some() {
                    " Edit Shared Skill "
                } else {
                    " New Shared Skill "
                },
                ed.editing_name.is_some(),
            ),
            VaultKindState::Tool {
                editor: Some(ed), ..
            } => (
                if ed.editing_name.is_some() {
                    " Edit Shared Tool "
                } else {
                    " New Shared Tool "
                },
                ed.editing_name.is_some(),
            ),
            VaultKindState::Ref {
                editor: Some(ed), ..
            } => (
                if ed.editing_name.is_some() {
                    " Edit Shared Reference "
                } else {
                    " New Shared Reference "
                },
                ed.editing_name.is_some(),
            ),
            VaultKindState::Model {
                editor: Some(ed), ..
            } => (
                if ed.editing_name.is_some() {
                    " Edit LLM Model "
                } else {
                    " New LLM Model "
                },
                ed.editing_name.is_some(),
            ),
            VaultKindState::Server { editor: Some(ed) } => (
                if ed.editing_key.is_some() {
                    " Edit Server "
                } else {
                    " New Server "
                },
                ed.editing_key.is_some(),
            ),
            _ => return,
        };
        let _ = is_edit;
        let ed_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow))
            .title(Line::from(Span::styled(
                ed_title,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            )));
        let ed_inner = ed_block.inner(area);
        f.render_widget(ed_block, area);
        let ed_w = usize::from(ed_inner.width);
        let mut ed_lines = Vec::with_capacity(5);
        match &m.kind {
            VaultKindState::Env {
                editor: Some(ed), ..
            } => {
                let avail = ed_w.saturating_sub(10).max(4);
                for (fi, (label, input)) in
                    [("Key", &ed.key), ("Value", &ed.value)].iter().enumerate()
                {
                    ed_lines.push(render_editor_input_row(
                        label,
                        5,
                        input,
                        ed.field == fi,
                        avail,
                    ));
                }
                ed_lines.push(editor_hint_or_err(&ed.err, ed_w, false));
            }
            VaultKindState::Skill {
                editor: Some(ed), ..
            } => {
                let avail = ed_w.saturating_sub(14).max(4);
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
                ed_lines.push(editor_hint_or_err(&ed.err, ed_w, false));
            }
            VaultKindState::Tool {
                editor: Some(ed), ..
            } => {
                let avail = ed_w.saturating_sub(14).max(4);
                let kind_val = match ed.kind {
                    ToolKind::Mcp => "mcp (Space toggles)",
                    ToolKind::Cli => "cli (Space toggles)",
                };
                let kind_lbl_style = if ed.field == 0 {
                    Style::default().add_modifier(ratatui::style::Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                ed_lines.push(Line::from(vec![
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
                ]));
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
                ed_lines.push(editor_hint_or_err(&ed.err, ed_w, ed.field == 0));
            }
            VaultKindState::Ref {
                editor: Some(ed), ..
            } => {
                let avail = ed_w.saturating_sub(11).max(4);
                for (fi, (label, input)) in
                    [("Name", &ed.name), ("URL", &ed.url), ("Desc", &ed.desc)]
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
                ed_lines.push(editor_hint_or_err(&ed.err, ed_w, false));
            }
            VaultKindState::Model {
                editor: Some(ed), ..
            } => {
                let avail = ed_w.saturating_sub(14).max(4);
                let bold = Style::default().add_modifier(ratatui::style::Modifier::BOLD);
                let on = ed.field == 0;
                ed_lines.push(Line::from(vec![
                    Span::styled(
                        format!("{} {:<8}: ", if on { ">" } else { " " }, "Provider"),
                        if on {
                            bold
                        } else {
                            Style::default().fg(Color::DarkGray)
                        },
                    ),
                    Span::styled(
                        format!("{} (Space toggles)", ed.provider.as_str()),
                        if on { bold } else { Style::default() },
                    ),
                ]));
                for (fi, (label, input)) in [
                    (1usize, ("Name", &ed.name)),
                    (2usize, ("Model", &ed.model)),
                    (3usize, ("Endpoint", &ed.endpoint)),
                    (4usize, ("Token", &ed.token)),
                ] {
                    ed_lines.push(render_editor_input_row(
                        label,
                        8,
                        input,
                        ed.field == fi,
                        avail,
                    ));
                }
                ed_lines.push(editor_hint_or_err(&ed.err, ed_w, ed.field == 0));
            }
            VaultKindState::Server { editor: Some(ed) } => {
                let avail = ed_w.saturating_sub(12).max(4);
                for (fi, (label, input)) in [("Name", &ed.key), ("Address", &ed.value)]
                    .iter()
                    .enumerate()
                {
                    ed_lines.push(render_editor_input_row(
                        label,
                        7,
                        input,
                        ed.field == fi,
                        avail,
                    ));
                }
                ed_lines.push(editor_hint_or_err(&ed.err, ed_w, false));
            }
            _ => {}
        }
        f.render_widget(Paragraph::new(ed_lines), ed_inner);
    }
}

fn push_vault_name_detail(
    spans: &mut Vec<Span<'static>>,
    name: &str,
    detail: &str,
    name_w: usize,
    detail_w: usize,
    active: bool,
    row_style: Style,
) {
    spans.push(Span::styled(
        format!("{:<nw$}", fit(name, name_w), nw = name_w),
        row_style.add_modifier(ratatui::style::Modifier::BOLD),
    ));
    spans.push(Span::styled("  ", row_style));
    spans.push(Span::styled(
        format!("{:<dw$}", fit(detail, detail_w), dw = detail_w),
        if active {
            row_style
        } else {
            row_style.add_modifier(ratatui::style::Modifier::DIM)
        },
    ));
}

pub(super) fn toggle_all_picked<'a>(
    picked: &mut BTreeSet<String>,
    names: impl Iterator<Item = &'a String>,
) {
    let all: Vec<String> = names.cloned().collect();
    if all.iter().all(|n| picked.contains(n)) {
        picked.clear();
    } else {
        picked.extend(all);
    }
}

pub(super) fn step_modal_sel(cur: usize, len: usize, up: bool) -> usize {
    if up {
        cur.saturating_sub(1)
    } else {
        (cur + 1).min(len)
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

/// The skills sync's question: the project's own folders by these names
/// differ from the vault version, so replacing or deleting them needs a yes.
fn skill_overwrite_confirm_line(names: &[String]) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!(
            "{} in this project differ from the vault. Replace?  ",
            sanitize_chrome_line(&names.join(", "))
        ),
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(ratatui::style::Modifier::BOLD),
    )];
    spans.extend(hint_spans(&[
        ("y / Enter", "replace"),
        ("n", "keep mine"),
        ("Esc", "back"),
    ]));
    Line::from(spans)
}

fn vault_footer_line(
    target: VaultTarget,
    project_verb: &str,
    can_pick: bool,
    has_reveal: bool,
    has_test: bool,
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
    if has_test {
        hints.push(("t", "test"));
    }
    hints.push(("Esc", "close"));
    Line::from(hint_spans(hints_that_fit(&hints, usize::from(width))))
}
