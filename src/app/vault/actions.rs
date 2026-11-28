use super::draw::{step_modal_sel, toggle_all_picked};
use super::*;

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
            VaultTarget::AddForm => self.add.env.clone(),
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
            VaultTarget::AddForm => self.add.skills.clone(),
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
            VaultTarget::AddForm => self.add.tools.clone(),
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
            VaultTarget::AddForm => self.add.refs.clone(),
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

    pub(crate) fn open_model_modal(&mut self, target: VaultTarget) {
        if let Ok(fresh) = models::load_models() {
            self.models_vault = fresh;
        }
        let project_dotenv = match target {
            VaultTarget::Project(_) => self
                .target_dir(target)
                .map(|dir| envvault::read_dotenv(&dir))
                .unwrap_or_default(),
            _ => BTreeMap::new(),
        };
        let picked = models::applied_names(&self.models_vault, &project_dotenv);
        // The add form has no model field: models are picked per project.
        let target = if target == VaultTarget::AddForm {
            VaultTarget::VaultOnly
        } else {
            target
        };
        let editor = self.models_vault.models.is_empty().then(ModelEditor::blank);
        self.vault_modal = Some(VaultModal::new(
            target,
            picked,
            VaultKindState::Model {
                project_dotenv,
                reveal: false,
                editor,
            },
        ));
    }

    /// `V`: the registered servers, picked per project in the Add form.
    pub(crate) fn open_server_modal(&mut self) {
        self.servers_vault = load_servers();
        let editor = self.servers_vault.vars.is_empty().then(EnvEditor::blank);
        self.vault_modal = Some(VaultModal::new(
            VaultTarget::VaultOnly,
            BTreeSet::new(),
            VaultKindState::Server { editor },
        ));
    }

    /// Space/click on a row. A model replaces any other picked model of its
    /// provider: a project's `.env` holds one per provider.
    pub(super) fn toggle_pick(&self, modal: &mut VaultModal, name: String) {
        if modal.picked.remove(&name) {
            return;
        }
        if modal.is_model() {
            let provider = |n: &str| {
                self.models_vault
                    .models
                    .iter()
                    .find(|m| m.name == n)
                    .map(|m| m.provider)
            };
            let mine = provider(&name);
            modal.picked.retain(|n| provider(n) != mine);
        }
        modal.picked.insert(name);
    }

    pub(crate) fn vault_len(&self, kind: &VaultKindState) -> usize {
        match kind {
            VaultKindState::Env { .. } => self.env_vault.vars.len(),
            VaultKindState::Skill { .. } => self.skills_vault.skills.len(),
            VaultKindState::Tool { .. } => self.tools_vault.tools.len(),
            VaultKindState::Ref { .. } => self.refs_vault.references.len(),
            VaultKindState::Model { .. } => self.models_vault.models.len(),
            VaultKindState::Server { .. } => self.servers_vault.vars.len(),
        }
    }

    pub(crate) fn vault_entry_name(&self, kind: &VaultKindState, idx: usize) -> Option<String> {
        match kind {
            VaultKindState::Env { .. } => self.env_vault.vars.get(idx).map(|e| e.key.clone()),
            VaultKindState::Skill { .. } => {
                self.skills_vault.skills.get(idx).map(|e| e.name.clone())
            }
            VaultKindState::Tool { .. } => self.tools_vault.tools.get(idx).map(|e| e.name.clone()),
            VaultKindState::Ref { .. } => {
                self.refs_vault.references.get(idx).map(|e| e.name.clone())
            }
            VaultKindState::Model { .. } => {
                self.models_vault.models.get(idx).map(|e| e.name.clone())
            }
            VaultKindState::Server { .. } => {
                self.servers_vault.vars.get(idx).map(|e| e.key.clone())
            }
        }
    }

    pub(crate) fn vault_all_names(&self, kind: &VaultKindState) -> Vec<String> {
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
            VaultKindState::Model { .. } => self
                .models_vault
                .models
                .iter()
                .map(|e| e.name.clone())
                .collect(),
            VaultKindState::Server { .. } => self
                .servers_vault
                .vars
                .iter()
                .map(|e| e.key.clone())
                .collect(),
        }
    }

    /// Handle paste into the active vault editor, if one is open.
    pub(crate) fn paste_vault_modal(&mut self, data: &str) -> bool {
        self.vault_modal
            .as_mut()
            .is_some_and(|m| m.paste_into_editor(data))
    }

    /// Move selection up/down on mouse wheel when the vault modal is open.
    pub(crate) fn wheel_vault_modal(&mut self, up: bool) {
        let Some(mut modal) = self.vault_modal.take() else {
            return;
        };
        let len = self.vault_len(&modal.kind);
        modal.confirm_delete = None;
        modal.sel = step_modal_sel(modal.sel, len, up);
        self.vault_modal = Some(modal);
    }

    /// Handle a left mouse click when the vault modal is open.
    pub(crate) fn click_vault_modal(&mut self, area: Size, m: MouseEvent) {
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
                        self.toggle_pick(&mut modal, name);
                    }
                }
            }
        }
        self.vault_modal = Some(modal);
    }

    /// Route keyboard input to the open vault modal.
    pub(crate) fn key_vault_modal(&mut self, k: KeyEvent) {
        let Some(mut modal) = self.vault_modal.take() else {
            return;
        };
        if self.handle_vault_key(&mut modal, k) {
            self.vault_modal = Some(modal);
        }
    }

    /// Returns `true` to keep `modal` open, `false` to close it.
    pub(crate) fn handle_vault_key(&mut self, modal: &mut VaultModal, k: KeyEvent) -> bool {
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
                    self.toggle_pick(modal, name);
                }
            }
            KeyCode::Char('A') if can_pick && len > 0 && !modal.is_model() => {
                let names = self.vault_all_names(&modal.kind);
                toggle_all_picked(&mut modal.picked, names.iter());
            }
            KeyCode::Char('v') => {
                if let VaultKindState::Env { reveal, .. } | VaultKindState::Model { reveal, .. } =
                    &mut modal.kind
                {
                    *reveal = !*reveal;
                }
            }
            KeyCode::Char('t') if modal.is_model() && len > 0 => {
                if let Some(m) = self.models_vault.models.get(modal.sel).cloned() {
                    self.message = (format!("Testing model {}...", m.name), Notice::Info);
                    let tx = self.model_tx.clone();
                    std::thread::spawn(move || {
                        let _ = tx.send((m.name.clone(), models::test_model(&m)));
                    });
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
                        self.add.env.remove(target_name);
                        modal.sel = modal.sel.min(self.env_vault.vars.len().saturating_sub(1));
                        self.propagate_env_change(modal, target_name, None);
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
                        self.add.skills.remove(target_name);
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
                        self.add.tools.remove(target_name);
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
                        self.add.refs.remove(target_name);
                        modal.sel = modal
                            .sel
                            .min(self.refs_vault.references.len().saturating_sub(1));
                    }
                    Err(e) => self.message = (format!("Save failed: {e:#}"), Notice::Error),
                }
            }
            // ponytail: projects keep the deleted model's variables in .env
            // until synced; deleting never rewrites other projects.
            VaultKindState::Model { .. } => {
                let mut next_vault = self.models_vault.clone();
                next_vault.models.retain(|m| m.name != target_name);
                match models::save_models(&next_vault) {
                    Ok(()) => {
                        self.models_vault = next_vault;
                        modal.picked.remove(target_name);
                        modal.sel = modal
                            .sel
                            .min(self.models_vault.models.len().saturating_sub(1));
                    }
                    Err(e) => self.message = (format!("Save failed: {e:#}"), Notice::Error),
                }
            }
            // ponytail: projects on a deleted server keep their user@host:dir
            // path and still open over ssh; nothing is rewritten.
            VaultKindState::Server { .. } => {
                let mut next = self.servers_vault.clone();
                next.vars.retain(|s| s.key != target_name);
                match servers_path().and_then(|p| envvault::save_env_to(&next, &p)) {
                    Ok(()) => {
                        self.servers_vault = next;
                        modal.sel = modal
                            .sel
                            .min(self.servers_vault.vars.len().saturating_sub(1));
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
            VaultKindState::Model { editor, .. } => {
                if let Some(entry) = self.models_vault.models.get(modal.sel) {
                    let mut ed = ModelEditor::blank();
                    ed.provider = entry.provider;
                    ed.name.set(&entry.name);
                    ed.model.set(&entry.model);
                    ed.endpoint.set(&entry.endpoint);
                    ed.token.set(&entry.token);
                    ed.field = 2;
                    ed.editing_name = Some(entry.name.clone());
                    *editor = Some(ed);
                }
            }
            VaultKindState::Server { editor } => {
                if let Some(entry) = self.servers_vault.vars.get(modal.sel) {
                    let mut ed = EnvEditor::blank();
                    ed.key.set(&entry.key);
                    ed.value.set(&entry.value);
                    ed.field = 1;
                    ed.editing_key = Some(entry.key.clone());
                    *editor = Some(ed);
                }
            }
        }
    }

    pub(crate) fn handle_vault_editor_key(&mut self, modal: &mut VaultModal, k: KeyEvent) {
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
                VaultKindState::Model { .. } => self.save_model_editor(modal),
                VaultKindState::Server { .. } => self.save_server_editor(modal),
            }
            return;
        }
        match &mut modal.kind {
            VaultKindState::Env {
                editor: Some(ed), ..
            }
            | VaultKindState::Server { editor: Some(ed) } => match k.code {
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
            VaultKindState::Model {
                editor: Some(ed), ..
            } => match k.code {
                KeyCode::Tab | KeyCode::Down => ed.field = (ed.field + 1) % 5,
                KeyCode::BackTab | KeyCode::Up => ed.field = (ed.field + 4) % 5,
                KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') if ed.field == 0 => {
                    ed.provider = ed.provider.next();
                    ed.err.clear();
                }
                _ => {
                    if let Some(input) = ed.input() {
                        if input.edit_key(k) == Some(true) {
                            ed.err.clear();
                        }
                    }
                }
            },
            _ => {}
        }
    }
}
