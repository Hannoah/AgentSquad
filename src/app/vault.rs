//! Shared Vault modals (`Env`, `Skills`, `Tools`, `References`) and project sync UI.

use super::*;

/// Target for shared vault modals (`Env`, `Skills`, `Tools`, `References`, or the Add/Edit form).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum VaultTarget {
    /// Opened from the Add/Edit project modal; `Enter` confirms selection back to `add_*`.
    AddForm,
    /// Opened from Manager on project `pi`; `Enter` writes checked items to `<project>`.
    Project(usize),
    /// Opened from Manager with no projects registered; manages the shared vault only.
    VaultOnly,
}

/// Inline form inside `VaultModal` (`Env`) for registering or editing a shared env variable.
pub(crate) struct EnvEditor {
    pub(crate) key: LineInput,
    pub(crate) value: LineInput,
    /// Active input field: `0` = key, `1` = value.
    pub(crate) field: usize,
    /// Original key when editing an existing entry; `None` when creating a new one.
    pub(crate) editing_key: Option<String>,
    pub(crate) err: String,
}

impl EnvEditor {
    /// An empty form for a new variable.
    pub(crate) fn blank() -> Self {
        Self {
            key: LineInput::new(),
            value: LineInput::new(),
            field: 0,
            editing_key: None,
            err: String::new(),
        }
    }
}

/// Inline form inside `VaultModal` (`Skill`) for registering or editing a shared skill.
pub(crate) struct SkillEditor {
    pub(crate) name: LineInput,
    pub(crate) desc: LineInput,
    pub(crate) body: LineInput,
    /// Active input field: `0` = name, `1` = description, `2` = source path or instructions.
    pub(crate) field: usize,
    pub(crate) editing_name: Option<String>,
    /// Preserves multi-line TOML instructions when the body line wasn't modified in TUI.
    pub(crate) initial_body_preview: String,
    pub(crate) preserved_multiline: Option<String>,
    pub(crate) err: String,
}

impl SkillEditor {
    pub(crate) fn blank() -> Self {
        Self {
            name: LineInput::new(),
            desc: LineInput::new(),
            body: LineInput::new(),
            field: 0,
            editing_name: None,
            initial_body_preview: String::new(),
            preserved_multiline: None,
            err: String::new(),
        }
    }
}

/// Inline form inside `VaultModal` (`Tool`) for registering or editing a shared MCP or CLI tool.
pub(crate) struct ToolEditor {
    pub(crate) kind: ToolKind,
    pub(crate) name: LineInput,
    pub(crate) command: LineInput,
    pub(crate) desc: LineInput,
    /// Active input field: `0` = kind toggle, `1` = name, `2` = cmd/url, `3` = description.
    pub(crate) field: usize,
    pub(crate) editing_name: Option<String>,
    pub(crate) preserved_env: BTreeMap<String, String>,
    pub(crate) err: String,
}

impl ToolEditor {
    pub(crate) fn blank() -> Self {
        Self {
            kind: ToolKind::Mcp,
            name: LineInput::new(),
            command: LineInput::new(),
            desc: LineInput::new(),
            field: 0,
            editing_name: None,
            preserved_env: BTreeMap::new(),
            err: String::new(),
        }
    }
}

/// Inline form inside `VaultModal` (`Ref`) for registering or editing a shared reference link.
pub(crate) struct RefEditor {
    pub(crate) name: LineInput,
    pub(crate) url: LineInput,
    pub(crate) desc: LineInput,
    /// Active input field: `0` = name (optional; derived from URL if blank), `1` = url, `2` = desc.
    pub(crate) field: usize,
    pub(crate) editing_name: Option<String>,
    pub(crate) err: String,
}

impl RefEditor {
    pub(crate) fn blank() -> Self {
        Self {
            name: LineInput::new(),
            url: LineInput::new(),
            desc: LineInput::new(),
            // Start on URL so the user can paste a link right away; Name auto-fills if empty.
            field: 1,
            editing_name: None,
            err: String::new(),
        }
    }
}

/// Kind-specific state carried inside [`VaultModal`].
pub(crate) enum VaultKindState {
    Env {
        /// Existing keys/values parsed from the target project's `.env` file.
        project_dotenv: BTreeMap<String, String>,
        reveal: bool,
        editor: Option<EnvEditor>,
    },
    Skill {
        project_state: ProjectSkills,
        /// Skills whose same-named folders in the project differ from the vault
        /// version: the sync waits for y (overwrite), n (keep them) or Esc.
        confirm_overwrite: Option<Vec<String>>,
        editor: Option<SkillEditor>,
    },
    Tool {
        project_state: ProjectTools,
        editor: Option<ToolEditor>,
    },
    Ref {
        project_state: ProjectReferences,
        editor: Option<RefEditor>,
    },
}

/// Unified Shared Vault & project-sync popup (`Env`, `Skills`, `Tools`, `References`).
/// `sel` runs one past the last entry: that row is `"+ New <noun>..."`.
pub(crate) struct VaultModal {
    pub(crate) target: VaultTarget,
    pub(crate) sel: usize,
    pub(crate) top: std::cell::Cell<usize>,
    pub(crate) picked: BTreeSet<String>,
    pub(crate) confirm_delete: Option<String>,
    pub(crate) kind: VaultKindState,
}

impl VaultModal {
    fn new(target: VaultTarget, picked: BTreeSet<String>, kind: VaultKindState) -> Self {
        Self {
            target,
            sel: 0,
            top: Default::default(),
            picked,
            confirm_delete: None,
            kind,
        }
    }

    pub(crate) fn is_env(&self) -> bool {
        matches!(self.kind, VaultKindState::Env { .. })
    }

    pub(crate) fn is_skill(&self) -> bool {
        matches!(self.kind, VaultKindState::Skill { .. })
    }

    pub(crate) fn is_tool(&self) -> bool {
        matches!(self.kind, VaultKindState::Tool { .. })
    }

    pub(crate) fn is_ref(&self) -> bool {
        matches!(self.kind, VaultKindState::Ref { .. })
    }

    pub(crate) fn has_editor(&self) -> bool {
        match &self.kind {
            VaultKindState::Env { editor, .. } => editor.is_some(),
            VaultKindState::Skill { editor, .. } => editor.is_some(),
            VaultKindState::Tool { editor, .. } => editor.is_some(),
            VaultKindState::Ref { editor, .. } => editor.is_some(),
        }
    }

    pub(crate) fn wants_text_input(&self) -> bool {
        match &self.kind {
            VaultKindState::Env { editor, .. } => editor.is_some(),
            VaultKindState::Skill { editor, .. } => editor.is_some(),
            VaultKindState::Tool { editor, .. } => editor.as_ref().is_some_and(|e| e.field != 0),
            VaultKindState::Ref { editor, .. } => editor.is_some(),
        }
    }

    pub(crate) fn ensure_editor_open(&mut self) {
        match &mut self.kind {
            VaultKindState::Env { editor, .. } if editor.is_none() => {
                *editor = Some(EnvEditor::blank());
            }
            VaultKindState::Skill { editor, .. } if editor.is_none() => {
                *editor = Some(SkillEditor::blank());
            }
            VaultKindState::Tool { editor, .. } if editor.is_none() => {
                *editor = Some(ToolEditor::blank());
            }
            VaultKindState::Ref { editor, .. } if editor.is_none() => {
                *editor = Some(RefEditor::blank());
            }
            _ => {}
        }
    }

    fn open_blank_editor(&mut self) {
        match &mut self.kind {
            VaultKindState::Env { editor, .. } => *editor = Some(EnvEditor::blank()),
            VaultKindState::Skill { editor, .. } => *editor = Some(SkillEditor::blank()),
            VaultKindState::Tool { editor, .. } => *editor = Some(ToolEditor::blank()),
            VaultKindState::Ref { editor, .. } => *editor = Some(RefEditor::blank()),
        }
    }

    fn close_editor(&mut self) {
        match &mut self.kind {
            VaultKindState::Env { editor, .. } => *editor = None,
            VaultKindState::Skill { editor, .. } => *editor = None,
            VaultKindState::Tool { editor, .. } => *editor = None,
            VaultKindState::Ref { editor, .. } => *editor = None,
        }
    }

    fn paste_into_editor(&mut self, data: &str) -> bool {
        let chars = data.chars().filter(|&c| c != '\r' && c != '\n');
        match &mut self.kind {
            VaultKindState::Env {
                editor: Some(ed), ..
            } => {
                let input = if ed.field == 0 {
                    &mut ed.key
                } else {
                    &mut ed.value
                };
                for ch in chars {
                    input.insert(ch);
                }
                ed.err.clear();
                true
            }
            VaultKindState::Skill {
                editor: Some(ed), ..
            } => {
                let input = match ed.field {
                    0 => &mut ed.name,
                    1 => &mut ed.desc,
                    _ => &mut ed.body,
                };
                for ch in chars {
                    input.insert(ch);
                }
                ed.err.clear();
                true
            }
            VaultKindState::Tool {
                editor: Some(ed), ..
            } => {
                if ed.field != 0 {
                    let input = match ed.field {
                        1 => &mut ed.name,
                        2 => &mut ed.command,
                        _ => &mut ed.desc,
                    };
                    for ch in chars {
                        input.insert(ch);
                    }
                    ed.err.clear();
                }
                true
            }
            VaultKindState::Ref {
                editor: Some(ed), ..
            } => {
                let input = match ed.field {
                    0 => &mut ed.name,
                    1 => &mut ed.url,
                    _ => &mut ed.desc,
                };
                for ch in chars {
                    input.insert(ch);
                }
                ed.err.clear();
                true
            }
            _ => false,
        }
    }

    fn editor_rows(&self) -> u16 {
        match &self.kind {
            VaultKindState::Env { .. } => 5,
            VaultKindState::Skill { .. } => 6,
            VaultKindState::Tool { .. } => 7,
            VaultKindState::Ref { .. } => 6,
        }
    }

    fn close_hotkey(&self) -> char {
        match &self.kind {
            VaultKindState::Env { .. } => 'E',
            VaultKindState::Skill { .. } => 'K',
            VaultKindState::Tool { .. } => 'T',
            VaultKindState::Ref { .. } => 'R',
        }
    }

    fn noun(&self) -> &'static str {
        match &self.kind {
            VaultKindState::Env { .. } => "variable",
            VaultKindState::Skill { .. } => "skill",
            VaultKindState::Tool { .. } => "tool",
            VaultKindState::Ref { .. } => "reference",
        }
    }

    fn delete_kind(&self) -> &'static str {
        match &self.kind {
            VaultKindState::Env { .. } => "env",
            VaultKindState::Skill { .. } => "skill",
            VaultKindState::Tool { .. } => "tool",
            VaultKindState::Ref { .. } => "reference",
        }
    }

    fn footer_verb(&self) -> &'static str {
        match &self.kind {
            VaultKindState::Env { .. } => "write .env",
            VaultKindState::Skill { .. } => "sync skills",
            VaultKindState::Tool { .. } => "sync tools",
            VaultKindState::Ref { .. } => "sync refs",
        }
    }

    fn empty_hint(&self) -> &'static str {
        match &self.kind {
            VaultKindState::Env { .. } => {
                "  No shared env vars registered yet. Press a to create one."
            }
            VaultKindState::Skill { .. } => {
                "  No shared skills registered yet. Press a to add one."
            }
            VaultKindState::Tool { .. } => {
                "  No shared tools registered yet. Press a to add an MCP or CLI tool."
            }
            VaultKindState::Ref { .. } => {
                "  No shared references registered yet. Press a to add a GitHub link."
            }
        }
    }

    fn editor_footer_hint(&self) -> &'static str {
        match &self.kind {
            VaultKindState::Env { .. } => "Saved entries stay in your shared vault for any project",
            VaultKindState::Skill { .. } => {
                "Applies to .agents/skills + .claude/skills (agy, claude, muse, codex)"
            }
            VaultKindState::Tool { .. } => {
                "MCP syncs .mcp.json + .agents/mcp_config.json + .codex/config.toml; CLI writes .squad/TOOLS.md"
            }
            VaultKindState::Ref { .. } => {
                "Syncs CLAUDE.md + AGENTS.md (+ GEMINI.md if present); blank Name uses URL slug"
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn project_dotenv(&self) -> Option<&BTreeMap<String, String>> {
        match &self.kind {
            VaultKindState::Env { project_dotenv, .. } => Some(project_dotenv),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn skill_overwrite(&self) -> Option<&Vec<String>> {
        match &self.kind {
            VaultKindState::Skill {
                confirm_overwrite, ..
            } => confirm_overwrite.as_ref(),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn tool_kind(&self) -> Option<ToolKind> {
        match &self.kind {
            VaultKindState::Tool {
                editor: Some(ed), ..
            } => Some(ed.kind),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn ref_field(&self) -> Option<usize> {
        match &self.kind {
            VaultKindState::Ref {
                editor: Some(ed), ..
            } => Some(ed.field),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn ref_state(&self) -> Option<&ProjectReferences> {
        match &self.kind {
            VaultKindState::Ref { project_state, .. } => Some(project_state),
            _ => None,
        }
    }
}

impl App {
    pub(crate) fn open_env_modal(&mut self, target: VaultTarget) {
        if let Ok(fresh) = envvault::load_env() {
            self.env_vault = fresh;
        }
        let project_dotenv = self
            .target_dir(target)
            .map(|dir| envvault::read_dotenv(&dir))
            .unwrap_or_default();
        let picked = match target {
            VaultTarget::AddForm => self.add_env.clone(),
            VaultTarget::Project(_) => self
                .env_vault
                .vars
                .iter()
                .filter(|e| project_dotenv.contains_key(&e.key))
                .map(|e| e.key.clone())
                .collect(),
            VaultTarget::VaultOnly => BTreeSet::new(),
        };
        let editor = self.env_vault.vars.is_empty().then(EnvEditor::blank);
        self.vault_modal = Some(VaultModal::new(
            target,
            picked,
            VaultKindState::Env {
                project_dotenv,
                reveal: false,
                editor,
            },
        ));
    }

    pub(crate) fn open_skill_modal(&mut self, target: VaultTarget) {
        if let Ok(fresh) = skills::load_skills() {
            self.skills_vault = fresh;
        }
        let project_state = self
            .target_dir(target)
            .map(|dir| skills::read_project_skills(&dir, &self.skills_vault))
            .unwrap_or_default();
        let picked = match target {
            VaultTarget::AddForm => self.add_skills.clone(),
            VaultTarget::Project(_) => project_state.vault_state.keys().cloned().collect(),
            VaultTarget::VaultOnly => BTreeSet::new(),
        };
        let editor = self.skills_vault.skills.is_empty().then(SkillEditor::blank);
        self.vault_modal = Some(VaultModal::new(
            target,
            picked,
            VaultKindState::Skill {
                project_state,
                confirm_overwrite: None,
                editor,
            },
        ));
    }

    pub(crate) fn open_tool_modal(&mut self, target: VaultTarget) {
        if let Ok(fresh) = tools::load_tools() {
            self.tools_vault = fresh;
        }
        let project_state = self
            .target_dir(target)
            .map(|dir| tools::read_project_tools(&dir, &self.tools_vault))
            .unwrap_or_default();
        let picked = match target {
            VaultTarget::AddForm => self.add_tools.clone(),
            VaultTarget::Project(_) => project_state.vault_state.keys().cloned().collect(),
            VaultTarget::VaultOnly => BTreeSet::new(),
        };
        let editor = self.tools_vault.tools.is_empty().then(ToolEditor::blank);
        self.vault_modal = Some(VaultModal::new(
            target,
            picked,
            VaultKindState::Tool {
                project_state,
                editor,
            },
        ));
    }

    pub(crate) fn open_ref_modal(&mut self, target: VaultTarget) {
        if let Ok(fresh) = refs::load_refs() {
            self.refs_vault = fresh;
        }
        let project_state = self
            .target_dir(target)
            .map(|dir| refs::read_project_refs(&dir, &self.refs_vault))
            .unwrap_or_default();
        let picked = match target {
            VaultTarget::AddForm => self.add_refs.clone(),
            VaultTarget::Project(_) => project_state.vault_state.keys().cloned().collect(),
            VaultTarget::VaultOnly => BTreeSet::new(),
        };
        let editor = self.refs_vault.references.is_empty().then(RefEditor::blank);
        self.vault_modal = Some(VaultModal::new(
            target,
            picked,
            VaultKindState::Ref {
                project_state,
                editor,
            },
        ));
    }

    fn vault_len(&self, kind: &VaultKindState) -> usize {
        match kind {
            VaultKindState::Env { .. } => self.env_vault.vars.len(),
            VaultKindState::Skill { .. } => self.skills_vault.skills.len(),
            VaultKindState::Tool { .. } => self.tools_vault.tools.len(),
            VaultKindState::Ref { .. } => self.refs_vault.references.len(),
        }
    }

    fn vault_entry_name(&self, kind: &VaultKindState, idx: usize) -> Option<String> {
        match kind {
            VaultKindState::Env { .. } => self.env_vault.vars.get(idx).map(|e| e.key.clone()),
            VaultKindState::Skill { .. } => {
                self.skills_vault.skills.get(idx).map(|e| e.name.clone())
            }
            VaultKindState::Tool { .. } => self.tools_vault.tools.get(idx).map(|e| e.name.clone()),
            VaultKindState::Ref { .. } => {
                self.refs_vault.references.get(idx).map(|e| e.name.clone())
            }
        }
    }

    fn vault_all_names(&self, kind: &VaultKindState) -> Vec<String> {
        match kind {
            VaultKindState::Env { .. } => {
                self.env_vault.vars.iter().map(|e| e.key.clone()).collect()
            }
            VaultKindState::Skill { .. } => self
                .skills_vault
                .skills
                .iter()
                .map(|e| e.name.clone())
                .collect(),
            VaultKindState::Tool { .. } => self
                .tools_vault
                .tools
                .iter()
                .map(|e| e.name.clone())
                .collect(),
            VaultKindState::Ref { .. } => self
                .refs_vault
                .references
                .iter()
                .map(|e| e.name.clone())
                .collect(),
        }
    }

    /// Handle paste into the active vault editor, if one is open.
    pub(super) fn paste_vault_modal(&mut self, data: &str) -> bool {
        self.vault_modal
            .as_mut()
            .is_some_and(|m| m.paste_into_editor(data))
    }

    /// Move selection up/down on mouse wheel when the vault modal is open.
    pub(super) fn wheel_vault_modal(&mut self, up: bool) {
        let Some(mut modal) = self.vault_modal.take() else {
            return;
        };
        let len = self.vault_len(&modal.kind);
        modal.confirm_delete = None;
        modal.sel = step_modal_sel(modal.sel, len, up);
        self.vault_modal = Some(modal);
    }

    /// Handle a left mouse click when the vault modal is open.
    pub(super) fn click_vault_modal(&mut self, area: Size, m: MouseEvent) {
        let Some(mut modal) = self.vault_modal.take() else {
            return;
        };
        // A click while the skill overwrite question waits only dismisses it.
        if let VaultKindState::Skill {
            confirm_overwrite, ..
        } = &mut modal.kind
        {
            if confirm_overwrite.take().is_some() {
                self.vault_modal = Some(modal);
                return;
            }
        }
        let len = self.vault_len(&modal.kind);
        let editor_h = if modal.has_editor() {
            modal.editor_rows()
        } else {
            0
        };
        let popup = vault_popup(area.into());
        let at = ratatui::layout::Position::new(m.column, m.row);
        if !popup.contains(at) {
            return;
        }
        let list = vault_list_rect(popup, editor_h);
        if list.contains(at) {
            let i = modal.top.get() + usize::from(m.row - list.y);
            if i == len && !modal.has_editor() {
                modal.confirm_delete = None;
                modal.sel = i;
                modal.open_blank_editor();
            } else if i < len {
                modal.confirm_delete = None;
                modal.sel = i;
                if modal.target != VaultTarget::VaultOnly && !modal.has_editor() {
                    if let Some(name) = self.vault_entry_name(&modal.kind, i) {
                        if !modal.picked.remove(&name) {
                            modal.picked.insert(name);
                        }
                    }
                }
            }
        }
        self.vault_modal = Some(modal);
    }

    /// Route keyboard input to the open vault modal.
    pub(super) fn key_vault_modal(&mut self, k: KeyEvent) {
        let Some(mut modal) = self.vault_modal.take() else {
            return;
        };
        if self.handle_vault_key(&mut modal, k) {
            self.vault_modal = Some(modal);
        }
    }

    /// Returns `true` to keep `modal` open, `false` to close it.
    fn handle_vault_key(&mut self, modal: &mut VaultModal, k: KeyEvent) -> bool {
        if let Some(target_name) = modal.confirm_delete.clone() {
            match confirm_key(k.code) {
                Some(true) => {
                    modal.confirm_delete = None;
                    self.delete_vault_entry(modal, &target_name);
                }
                Some(false) => modal.confirm_delete = None,
                None => {}
            }
            return true;
        }
        if let VaultKindState::Skill {
            confirm_overwrite: over,
            ..
        } = &mut modal.kind
        {
            if over.is_some() {
                match confirm_key(k.code) {
                    Some(true) => {
                        *over = None;
                        return self.sync_skill_modal(modal, true);
                    }
                    // Esc goes back to the list; n syncs everything else.
                    Some(false) if k.code == KeyCode::Esc => *over = None,
                    Some(false) => {
                        *over = None;
                        return self.sync_skill_modal(modal, false);
                    }
                    None => {}
                }
                return true;
            }
        }
        if modal.has_editor() {
            self.handle_vault_editor_key(modal, k);
            return true;
        }

        let kh = hotkey_event(k);
        let len = self.vault_len(&modal.kind);
        let can_pick = modal.target != VaultTarget::VaultOnly;
        match kh.code {
            KeyCode::Esc => return false,
            KeyCode::Char(c) if c == modal.close_hotkey() => return false,
            // Rows are the vault items followed by "+ New <noun>..." at index `len`.
            KeyCode::Up | KeyCode::Char('k') => modal.sel = (modal.sel + len) % (len + 1),
            KeyCode::Down | KeyCode::Char('j') => modal.sel = (modal.sel + 1) % (len + 1),
            KeyCode::Enter | KeyCode::Char(' ') if modal.sel >= len => {
                modal.open_blank_editor();
            }
            KeyCode::Char(' ') if can_pick && len > 0 => {
                if let Some(name) = self.vault_entry_name(&modal.kind, modal.sel) {
                    if !modal.picked.remove(&name) {
                        modal.picked.insert(name);
                    }
                }
            }
            KeyCode::Char('A') if can_pick && len > 0 => {
                let names = self.vault_all_names(&modal.kind);
                toggle_all_picked(&mut modal.picked, names.iter());
            }
            KeyCode::Char('v') => {
                if let VaultKindState::Env { reveal, .. } = &mut modal.kind {
                    *reveal = !*reveal;
                }
            }
            KeyCode::Char('a' | 'n') => modal.open_blank_editor(),
            KeyCode::Char('e') if len > 0 => self.open_edit_in_vault(modal),
            KeyCode::Char('d') | KeyCode::Delete if len > 0 => {
                if let Some(name) = self.vault_entry_name(&modal.kind, modal.sel) {
                    modal.confirm_delete = Some(name);
                }
            }
            KeyCode::Enter => return self.commit_vault_modal(modal),
            _ => {}
        }
        true
    }

    fn delete_vault_entry(&mut self, modal: &mut VaultModal, target_name: &str) {
        match &mut modal.kind {
            VaultKindState::Env { .. } => {
                let mut next_vault = self.env_vault.clone();
                envvault::remove_env(&mut next_vault, target_name);
                match envvault::save_env(&next_vault) {
                    Ok(()) => {
                        self.env_vault = next_vault;
                        modal.picked.remove(target_name);
                        self.add_env.remove(target_name);
                        modal.sel = modal.sel.min(self.env_vault.vars.len().saturating_sub(1));
                    }
                    Err(e) => self.message = (format!("Save failed: {e:#}"), Notice::Error),
                }
            }
            VaultKindState::Skill { .. } => {
                let mut next_vault = self.skills_vault.clone();
                skills::remove_skill(&mut next_vault, target_name);
                match skills::save_skills(&next_vault) {
                    Ok(()) => {
                        self.skills_vault = next_vault;
                        modal.picked.remove(target_name);
                        self.add_skills.remove(target_name);
                        modal.sel = modal
                            .sel
                            .min(self.skills_vault.skills.len().saturating_sub(1));
                    }
                    Err(e) => self.message = (format!("Save failed: {e:#}"), Notice::Error),
                }
            }
            VaultKindState::Tool { .. } => {
                let mut next_vault = self.tools_vault.clone();
                tools::remove_tool(&mut next_vault, target_name);
                match tools::save_tools(&next_vault) {
                    Ok(()) => {
                        self.tools_vault = next_vault;
                        modal.picked.remove(target_name);
                        self.add_tools.remove(target_name);
                        modal.sel = modal
                            .sel
                            .min(self.tools_vault.tools.len().saturating_sub(1));
                    }
                    Err(e) => self.message = (format!("Save failed: {e:#}"), Notice::Error),
                }
            }
            VaultKindState::Ref { .. } => {
                let mut next_vault = self.refs_vault.clone();
                refs::remove_ref(&mut next_vault, target_name);
                match refs::save_refs(&next_vault) {
                    Ok(()) => {
                        self.refs_vault = next_vault;
                        modal.picked.remove(target_name);
                        self.add_refs.remove(target_name);
                        modal.sel = modal
                            .sel
                            .min(self.refs_vault.references.len().saturating_sub(1));
                    }
                    Err(e) => self.message = (format!("Save failed: {e:#}"), Notice::Error),
                }
            }
        }
    }

    fn open_edit_in_vault(&self, modal: &mut VaultModal) {
        match &mut modal.kind {
            VaultKindState::Env { editor, .. } => {
                if let Some(entry) = self.env_vault.vars.get(modal.sel) {
                    let mut key_in = LineInput::new();
                    key_in.set(&entry.key);
                    let mut val_in = LineInput::new();
                    val_in.set(&entry.value);
                    *editor = Some(EnvEditor {
                        key: key_in,
                        value: val_in,
                        field: 1,
                        editing_key: Some(entry.key.clone()),
                        err: String::new(),
                    });
                }
            }
            VaultKindState::Skill { editor, .. } => {
                if let Some(entry) = self.skills_vault.skills.get(modal.sel) {
                    let mut name_in = LineInput::new();
                    name_in.set(&entry.name);
                    let mut desc_in = LineInput::new();
                    desc_in.set(&entry.description);
                    let mut body_in = LineInput::new();
                    let (preview, preserved) = if let Some(src) = &entry.source_path {
                        (src.clone(), None)
                    } else if entry.instructions.contains('\n') || entry.instructions.contains('\r')
                    {
                        let single = entry
                            .instructions
                            .lines()
                            .map(str::trim)
                            .filter(|l| !l.is_empty())
                            .collect::<Vec<_>>()
                            .join(" ");
                        (single, Some(entry.instructions.clone()))
                    } else {
                        (entry.instructions.clone(), None)
                    };
                    body_in.set(&preview);
                    *editor = Some(SkillEditor {
                        name: name_in,
                        desc: desc_in,
                        body: body_in,
                        field: 1,
                        editing_name: Some(entry.name.clone()),
                        initial_body_preview: preview,
                        preserved_multiline: preserved,
                        err: String::new(),
                    });
                }
            }
            VaultKindState::Tool { editor, .. } => {
                if let Some(entry) = self.tools_vault.tools.get(modal.sel) {
                    let mut name_in = LineInput::new();
                    name_in.set(&entry.name);
                    let mut cmd_in = LineInput::new();
                    cmd_in.set(&entry.command);
                    let mut desc_in = LineInput::new();
                    desc_in.set(&entry.description);
                    *editor = Some(ToolEditor {
                        kind: entry.kind,
                        name: name_in,
                        command: cmd_in,
                        desc: desc_in,
                        field: 2,
                        editing_name: Some(entry.name.clone()),
                        preserved_env: entry.env.clone(),
                        err: String::new(),
                    });
                }
            }
            VaultKindState::Ref { editor, .. } => {
                if let Some(entry) = self.refs_vault.references.get(modal.sel) {
                    let mut name_in = LineInput::new();
                    name_in.set(&entry.name);
                    let mut url_in = LineInput::new();
                    url_in.set(&entry.url);
                    let mut desc_in = LineInput::new();
                    desc_in.set(&entry.description);
                    *editor = Some(RefEditor {
                        name: name_in,
                        url: url_in,
                        desc: desc_in,
                        field: 1,
                        editing_name: Some(entry.name.clone()),
                        err: String::new(),
                    });
                }
            }
        }
    }

    fn handle_vault_editor_key(&mut self, modal: &mut VaultModal, k: KeyEvent) {
        if k.code == KeyCode::Esc {
            modal.close_editor();
            return;
        }
        if k.code == KeyCode::Enter {
            match &mut modal.kind {
                VaultKindState::Env { .. } => self.save_env_editor(modal),
                VaultKindState::Skill { .. } => self.save_skill_editor(modal),
                VaultKindState::Tool { .. } => self.save_tool_editor(modal),
                VaultKindState::Ref { .. } => self.save_ref_editor(modal),
            }
            return;
        }
        match &mut modal.kind {
            VaultKindState::Env {
                editor: Some(ed), ..
            } => match k.code {
                KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down => {
                    ed.field = 1 - ed.field;
                }
                _ => {
                    let input = if ed.field == 0 {
                        &mut ed.key
                    } else {
                        &mut ed.value
                    };
                    if input.edit_key(k) == Some(true) {
                        ed.err.clear();
                    }
                }
            },
            VaultKindState::Skill {
                editor: Some(ed), ..
            } => match k.code {
                KeyCode::Tab | KeyCode::Down => ed.field = (ed.field + 1) % 3,
                KeyCode::BackTab | KeyCode::Up => ed.field = (ed.field + 2) % 3,
                _ => {
                    let input = match ed.field {
                        0 => &mut ed.name,
                        1 => &mut ed.desc,
                        _ => &mut ed.body,
                    };
                    if input.edit_key(k) == Some(true) {
                        ed.err.clear();
                    }
                }
            },
            VaultKindState::Tool {
                editor: Some(ed), ..
            } => match k.code {
                KeyCode::Tab | KeyCode::Down => ed.field = (ed.field + 1) % 4,
                KeyCode::BackTab | KeyCode::Up => ed.field = (ed.field + 3) % 4,
                KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') if ed.field == 0 => {
                    ed.kind = ed.kind.toggled();
                    ed.err.clear();
                }
                _ if ed.field != 0 => {
                    let input = match ed.field {
                        1 => &mut ed.name,
                        2 => &mut ed.command,
                        _ => &mut ed.desc,
                    };
                    if input.edit_key(k) == Some(true) {
                        ed.err.clear();
                    }
                }
                _ => {}
            },
            VaultKindState::Ref {
                editor: Some(ed), ..
            } => match k.code {
                KeyCode::Tab | KeyCode::Down => ed.field = (ed.field + 1) % 3,
                KeyCode::BackTab | KeyCode::Up => ed.field = (ed.field + 2) % 3,
                _ => {
                    let input = match ed.field {
                        0 => &mut ed.name,
                        1 => &mut ed.url,
                        _ => &mut ed.desc,
                    };
                    if input.edit_key(k) == Some(true) {
                        ed.err.clear();
                    }
                }
            },
            _ => {}
        }
    }

    fn save_env_editor(&mut self, modal: &mut VaultModal) {
        let VaultKindState::Env {
            editor: Some(ed), ..
        } = &mut modal.kind
        else {
            return;
        };
        let raw_key = ed.key.as_str();
        let val = ed.value.as_str();
        let valid_key = match envvault::validate_env_key(&raw_key) {
            Ok(k) => k,
            Err(e) => {
                ed.err = e;
                return;
            }
        };
        let mut next_vault = self.env_vault.clone();
        if let Some(old) = ed.editing_key.as_deref() {
            if old != valid_key {
                if next_vault.vars.iter().any(|v| v.key == valid_key) {
                    ed.err = format!("{valid_key} already exists.");
                    return;
                }
                if let Some(pos) = next_vault.vars.iter().position(|v| v.key == old) {
                    next_vault.vars[pos] = envvault::EnvEntry {
                        key: valid_key.clone(),
                        value: val,
                    };
                }
                modal.picked.remove(old);
                self.add_env.remove(old);
            } else if let Err(e) = envvault::upsert_env(&mut next_vault, &valid_key, &val) {
                ed.err = e;
                return;
            }
        } else if let Err(e) = envvault::upsert_env(&mut next_vault, &valid_key, &val) {
            ed.err = e;
            return;
        }
        if let Err(e) = envvault::save_env(&next_vault) {
            ed.err = format!("Save failed: {e:#}");
            return;
        }
        self.env_vault = next_vault;
        if let Some(pos) = self.env_vault.vars.iter().position(|v| v.key == valid_key) {
            modal.sel = pos;
        }
        if modal.target != VaultTarget::VaultOnly {
            modal.picked.insert(valid_key);
        }
        modal.close_editor();
    }

    fn save_skill_editor(&mut self, modal: &mut VaultModal) {
        let VaultKindState::Skill {
            editor: Some(ed), ..
        } = &mut modal.kind
        else {
            return;
        };
        let valid_name = match skills::validate_skill_name(&ed.name.as_str()) {
            Ok(n) => n,
            Err(e) => {
                ed.err = e;
                return;
            }
        };
        let body_raw = ed.body.as_str();
        let (source_path, instructions) = if let Some(orig) = ed
            .preserved_multiline
            .as_ref()
            .filter(|_| body_raw == ed.initial_body_preview)
        {
            (None, orig.clone())
        } else {
            match skills::classify_source_or_body(&body_raw) {
                Ok(pair) => pair,
                Err(e) => {
                    ed.err = e;
                    return;
                }
            }
        };
        let mut desc = ed.desc.as_str().trim().to_string();
        if desc.is_empty() {
            if let Some(src) = &source_path {
                if let Some(inferred) = skills::infer_description(src) {
                    desc = inferred;
                }
            }
        }
        let entry = skills::SkillEntry {
            name: valid_name.clone(),
            description: desc,
            source_path,
            instructions,
        };
        let mut next_vault = self.skills_vault.clone();
        if let Some(old) = ed.editing_name.as_deref() {
            if old != valid_name {
                if next_vault.skills.iter().any(|s| s.name == valid_name) {
                    ed.err = format!("{valid_name} already exists.");
                    return;
                }
                if let Some(pos) = next_vault.skills.iter().position(|s| s.name == old) {
                    next_vault.skills[pos] = entry;
                }
                modal.picked.remove(old);
                self.add_skills.remove(old);
            } else if let Err(e) = skills::upsert_skill(&mut next_vault, entry) {
                ed.err = e;
                return;
            }
        } else if let Err(e) = skills::upsert_skill(&mut next_vault, entry) {
            ed.err = e;
            return;
        }
        if let Err(e) = skills::save_skills(&next_vault) {
            ed.err = format!("Save failed: {e:#}");
            return;
        }
        self.skills_vault = next_vault;
        if let Some(pos) = self
            .skills_vault
            .skills
            .iter()
            .position(|s| s.name == valid_name)
        {
            modal.sel = pos;
        }
        if modal.target != VaultTarget::VaultOnly {
            modal.picked.insert(valid_name);
        }
        modal.close_editor();
    }

    fn save_tool_editor(&mut self, modal: &mut VaultModal) {
        let VaultKindState::Tool {
            editor: Some(ed), ..
        } = &mut modal.kind
        else {
            return;
        };
        let valid_name = match tools::validate_tool_name(&ed.name.as_str()) {
            Ok(n) => n,
            Err(e) => {
                ed.err = e;
                return;
            }
        };
        let valid_cmd = match tools::validate_tool_command(ed.kind, &ed.command.as_str()) {
            Ok(c) => c,
            Err(e) => {
                ed.err = e;
                return;
            }
        };
        let entry = tools::ToolEntry {
            name: valid_name.clone(),
            kind: ed.kind,
            command: valid_cmd,
            description: ed.desc.as_str().trim().to_string(),
            env: ed.preserved_env.clone(),
        };
        let mut next_vault = self.tools_vault.clone();
        if let Some(old) = ed.editing_name.as_deref() {
            if !old.eq_ignore_ascii_case(&valid_name)
                && next_vault
                    .tools
                    .iter()
                    .any(|t| t.name.eq_ignore_ascii_case(&valid_name))
            {
                ed.err = format!("{valid_name} already exists.");
                return;
            }
            if !old.eq_ignore_ascii_case(&valid_name) {
                if let Some(pos) = next_vault
                    .tools
                    .iter()
                    .position(|t| t.name.eq_ignore_ascii_case(old))
                {
                    next_vault.tools[pos] = entry;
                }
                modal.picked.remove(old);
                self.add_tools.remove(old);
            } else if let Err(e) = tools::upsert_tool(&mut next_vault, entry) {
                ed.err = e;
                return;
            }
        } else if let Err(e) = tools::upsert_tool(&mut next_vault, entry) {
            ed.err = e;
            return;
        }
        if let Err(e) = tools::save_tools(&next_vault) {
            ed.err = format!("Save failed: {e:#}");
            return;
        }
        self.tools_vault = next_vault;
        if let Some(pos) = self
            .tools_vault
            .tools
            .iter()
            .position(|t| t.name == valid_name)
        {
            modal.sel = pos;
        }
        if modal.target != VaultTarget::VaultOnly {
            modal.picked.insert(valid_name);
        }
        modal.close_editor();
    }

    fn save_ref_editor(&mut self, modal: &mut VaultModal) {
        let VaultKindState::Ref {
            project_state,
            editor: Some(ed),
        } = &mut modal.kind
        else {
            return;
        };
        let valid_url = match refs::normalize_url(&ed.url.as_str()) {
            Ok(u) => u,
            Err(e) => {
                ed.err = e;
                return;
            }
        };
        let raw_name = if ed.name.as_str().trim().is_empty() {
            refs::infer_name_from_url(&valid_url)
        } else {
            ed.name.as_str().to_string()
        };
        let valid_name = match refs::validate_ref_name(&raw_name) {
            Ok(n) => n,
            Err(e) => {
                ed.err = e;
                return;
            }
        };
        let entry = refs::ReferenceEntry {
            name: valid_name.clone(),
            url: valid_url,
            description: refs::sanitize_description(&ed.desc.as_str()),
        };
        let mut next_vault = self.refs_vault.clone();
        if let Some(old) = ed.editing_name.as_deref() {
            if !old.eq_ignore_ascii_case(&valid_name)
                && next_vault
                    .references
                    .iter()
                    .any(|r| r.name.eq_ignore_ascii_case(&valid_name))
            {
                ed.err = format!("{valid_name} already exists.");
                return;
            }
            if !old.eq_ignore_ascii_case(&valid_name) {
                if let Some(pos) = next_vault
                    .references
                    .iter()
                    .position(|r| r.name.eq_ignore_ascii_case(old))
                {
                    next_vault.references[pos] = entry;
                }
                modal.picked.remove(old);
                self.add_refs.remove(old);
            } else if let Err(e) = refs::upsert_ref(&mut next_vault, entry) {
                ed.err = e;
                return;
            }
        } else if let Err(e) = refs::upsert_ref(&mut next_vault, entry) {
            ed.err = e;
            return;
        }
        if let Err(e) = refs::save_refs(&next_vault) {
            ed.err = format!("Save failed: {e:#}");
            return;
        }
        self.refs_vault = next_vault;
        if let VaultTarget::Project(pi) = modal.target {
            if let Some(p) = self.cfg.projects.get(pi) {
                *project_state = refs::read_project_refs(&p.path, &self.refs_vault);
            }
        }
        if let Some(pos) = self
            .refs_vault
            .references
            .iter()
            .position(|r| r.name == valid_name)
        {
            modal.sel = pos;
        }
        if modal.target != VaultTarget::VaultOnly {
            modal.picked.insert(valid_name);
        }
        modal.close_editor();
    }

    /// Sync the skills modal's picks into its project (`false` closes modal;
    /// `true` keeps it open on failure).
    fn sync_skill_modal(&mut self, modal: &VaultModal, overwrite: bool) -> bool {
        let VaultKindState::Skill { project_state, .. } = &modal.kind else {
            return false;
        };
        let VaultTarget::Project(pi) = modal.target else {
            return false;
        };
        let Some(p) = self.cfg.projects.get(pi) else {
            return false;
        };
        let name = p.name.clone();
        match skills::sync_project_skills(
            &p.path,
            &self.skills_vault,
            &modal.picked,
            project_state,
            overwrite,
        ) {
            Ok((0, 0, 0, kept)) if kept.is_empty() => false,
            Ok((added, updated, removed, kept)) => {
                self.message = (
                    format!(
                        "Updated {name} skills (+{added} added, {updated} updated, -{removed} removed){}",
                        super::actions::kept_skills_note(&kept)
                    ),
                    Notice::Info,
                );
                false
            }
            Err(e) => {
                self.message = (format!("Skills failed: {e:#}"), Notice::Error);
                true
            }
        }
    }

    /// Apply picked entries to the Add form or target project. Returns `false` to close.
    fn commit_vault_modal(&mut self, modal: &mut VaultModal) -> bool {
        match modal.target {
            VaultTarget::AddForm => {
                let picked = modal.picked.clone();
                match &modal.kind {
                    VaultKindState::Env { .. } => self.add_env = picked,
                    VaultKindState::Skill { .. } => self.add_skills = picked,
                    VaultKindState::Tool { .. } => self.add_tools = picked,
                    VaultKindState::Ref { .. } => self.add_refs = picked,
                }
                false
            }
            VaultTarget::VaultOnly => false,
            VaultTarget::Project(pi) => {
                let Some(p) = self.cfg.projects.get(pi) else {
                    return false;
                };
                let project_name = p.name.clone();
                let project_path = p.path.clone();
                match &mut modal.kind {
                    VaultKindState::Env { project_dotenv, .. } => {
                        match self.sync_project_dotenv(&project_path, &modal.picked, project_dotenv)
                        {
                            Ok((0, 0, 0)) => false,
                            Ok((added, updated, removed)) => {
                                self.message = (
                                    format!(
                                        "Updated {project_name}/.env (+{added} added, {updated} updated, -{removed} removed)"
                                    ),
                                    Notice::Info,
                                );
                                false
                            }
                            Err(e) => {
                                self.message = (e, Notice::Error);
                                true
                            }
                        }
                    }
                    VaultKindState::Skill {
                        project_state,
                        confirm_overwrite,
                        ..
                    } => {
                        // A same-named folder that is not the vault version is
                        // the user's work: replacing or deleting it needs a yes.
                        let conflicts = skills::sync_conflicts(
                            &project_path,
                            &self.skills_vault,
                            &modal.picked,
                            project_state,
                        );
                        if conflicts.is_empty() {
                            self.sync_skill_modal(modal, false)
                        } else {
                            *confirm_overwrite = Some(conflicts);
                            true
                        }
                    }
                    VaultKindState::Tool { project_state, .. } => {
                        match tools::sync_project_tools(
                            &project_path,
                            &self.tools_vault,
                            &modal.picked,
                            project_state,
                        ) {
                            Ok((0, 0, 0)) => false,
                            Ok((added, updated, removed)) => {
                                self.message = (
                                    format!(
                                        "Updated {project_name} tools (+{added} added, {updated} updated, -{removed} removed)"
                                    ),
                                    Notice::Info,
                                );
                                false
                            }
                            Err(e) => {
                                self.message = (format!("Tools failed: {e:#}"), Notice::Error);
                                true
                            }
                        }
                    }
                    VaultKindState::Ref { project_state, .. } => {
                        match refs::sync_project_refs(
                            &project_path,
                            &self.refs_vault,
                            &modal.picked,
                            project_state,
                        ) {
                            Ok((0, 0, 0)) => false,
                            Ok((added, updated, removed)) => {
                                self.message = (
                                    format!(
                                        "Updated {project_name} refs (+{added} added, {updated} updated, -{removed} removed)"
                                    ),
                                    Notice::Info,
                                );
                                false
                            }
                            Err(e) => {
                                self.message = (format!("Refs failed: {e:#}"), Notice::Error);
                                true
                            }
                        }
                    }
                }
            }
        }
    }

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
                " Shared Env Vault -> new project .env ".to_string()
            }
            (VaultKindState::Env { .. }, VaultTarget::Project(pi)) => {
                format!(" Shared Env Vault -> {}/.env ", project_name(pi))
            }
            (VaultKindState::Env { .. }, VaultTarget::VaultOnly) => {
                " Shared Env Vault ".to_string()
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
        }
    }

    pub(super) fn draw_vault_modal(&self, f: &mut Frame) {
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
                m.is_env(),
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
            super::draw::scrollbar(f, popup, total, m.sel);
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

fn toggle_all_picked<'a>(picked: &mut BTreeSet<String>, names: impl Iterator<Item = &'a String>) {
    let all: Vec<String> = names.cloned().collect();
    if all.iter().all(|n| picked.contains(n)) {
        picked.clear();
    } else {
        picked.extend(all);
    }
}

fn step_modal_sel(cur: usize, len: usize, up: bool) -> usize {
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
