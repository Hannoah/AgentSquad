use super::*;

impl App {
    pub(super) fn save_env_editor(&mut self, modal: &mut VaultModal) {
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
        // A new entry whose key already exists updates it, so it propagates too.
        let old_key = ed.editing_key.clone().unwrap_or_else(|| valid_key.clone());
        let existed = self.env_vault.vars.iter().any(|v| v.key == old_key);
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
                self.add.env.remove(old);
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
        let entry = self
            .env_vault
            .vars
            .iter()
            .find(|v| v.key == valid_key)
            .cloned();
        if existed {
            self.propagate_env_change(modal, &old_key, entry.as_ref());
        }
        if modal.target != VaultTarget::VaultOnly {
            modal.picked.insert(valid_key);
        }
        modal.close_editor();
    }

    /// Push a vault edit (or delete, `new = None`) of `old` into every
    /// project `.env` holding it, then refresh the modal's `.env` snapshot.
    pub(super) fn propagate_env_change(
        &mut self,
        modal: &mut VaultModal,
        old: &str,
        new: Option<&envvault::EnvEntry>,
    ) {
        let dirs = self.cfg.projects.iter().map(|p| p.path.as_path());
        let (changed, errors) = envvault::propagate_env(dirs, old, new);
        if let VaultKindState::Env { project_dotenv, .. } = &mut modal.kind {
            *project_dotenv = self
                .target_dir(modal.target)
                .map(|d| envvault::read_dotenv(&d))
                .unwrap_or_default();
        }
        if !errors.is_empty() {
            self.message = (
                format!(".env update failed: {}", errors.join("; ")),
                Notice::Error,
            );
        } else if changed > 0 {
            self.message = (
                format!("Updated {old} in {changed} project .env file(s)"),
                Notice::Info,
            );
        }
    }

    pub(super) fn save_skill_editor(&mut self, modal: &mut VaultModal) {
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
                self.add.skills.remove(old);
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

    pub(super) fn save_tool_editor(&mut self, modal: &mut VaultModal) {
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
                self.add.tools.remove(old);
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

    pub(super) fn save_ref_editor(&mut self, modal: &mut VaultModal) {
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
                self.add.refs.remove(old);
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

    pub(super) fn save_model_editor(&mut self, modal: &mut VaultModal) {
        let VaultKindState::Model {
            editor: Some(ed), ..
        } = &mut modal.kind
        else {
            return;
        };
        let name = match models::validate_model_name(&ed.name.as_str()) {
            Ok(n) => n,
            Err(e) => {
                ed.err = e;
                return;
            }
        };
        let model = ed.model.as_str().trim().to_string();
        if model.is_empty() {
            ed.err = "Model cannot be empty.".into();
            ed.field = 2;
            return;
        }
        let entry = models::ModelEntry {
            name: name.clone(),
            provider: ed.provider,
            model,
            endpoint: ed.endpoint.as_str().trim().to_string(),
            token: ed.token.as_str().trim().to_string(),
        };
        let mut next_vault = self.models_vault.clone();
        let old = ed.editing_name.clone().unwrap_or_else(|| name.clone());
        if old != name && next_vault.models.iter().any(|m| m.name == name) {
            ed.err = format!("{name} already exists.");
            return;
        }
        match next_vault.models.iter().position(|m| m.name == old) {
            Some(pos) => next_vault.models[pos] = entry,
            None => next_vault.models.push(entry),
        }
        if let Err(e) = models::save_models(&next_vault) {
            ed.err = format!("Save failed: {e:#}");
            return;
        }
        self.models_vault = next_vault;
        modal.picked.remove(&old);
        if let Some(pos) = self.models_vault.models.iter().position(|m| m.name == name) {
            modal.sel = pos;
        }
        modal.close_editor();
        if modal.target != VaultTarget::VaultOnly {
            self.toggle_pick(modal, name);
        }
    }

    pub(super) fn save_server_editor(&mut self, modal: &mut VaultModal) {
        let VaultKindState::Server { editor: Some(ed) } = &mut modal.kind else {
            return;
        };
        let name = match models::validate_model_name(&ed.key.as_str()) {
            Ok(n) => n,
            Err(e) => {
                ed.err = e;
                ed.field = 0;
                return;
            }
        };
        let addr = match validate_server_addr(&ed.value.as_str()) {
            Ok(a) => a,
            Err(e) => {
                ed.err = e;
                ed.field = 1;
                return;
            }
        };
        let old = ed.editing_key.clone().unwrap_or_else(|| name.clone());
        let mut next = self.servers_vault.clone();
        if old != name && next.vars.iter().any(|s| s.key == name) {
            ed.err = format!("{name} already exists.");
            return;
        }
        // The address it had, so its projects can follow a change.
        let old_addr = next
            .vars
            .iter()
            .find(|s| s.key == old)
            .map(|s| s.value.clone())
            .filter(|a| *a != addr);
        let entry = envvault::EnvEntry {
            key: name.clone(),
            value: addr,
        };
        match next.vars.iter().position(|s| s.key == old) {
            Some(pos) => next.vars[pos] = entry,
            None => next.vars.push(entry),
        }
        if let Err(e) = servers_path().and_then(|p| envvault::save_env_to(&next, &p)) {
            ed.err = format!("Save failed: {e:#}");
            return;
        }
        self.servers_vault = next;
        if let Some(pos) = self.servers_vault.vars.iter().position(|s| s.key == name) {
            modal.sel = pos;
        }
        modal.close_editor();
        if let Some(old_addr) = old_addr {
            self.rehost_projects(&old_addr, &name);
        }
    }

    /// A server's address changed: its projects open at the new one.
    /// Running sessions keep their connection until restarted.
    fn rehost_projects(&mut self, old_addr: &str, name: &str) {
        let Some(new_addr) = self
            .servers_vault
            .vars
            .iter()
            .find(|s| s.key == name)
            .map(|s| s.value.clone())
        else {
            return;
        };
        match persist_rehost(old_addr, &new_addr) {
            Ok(0) => {}
            Ok(n) => {
                rehost(&mut self.cfg.projects, old_addr, &new_addr);
                self.message = (
                    format!("Moved {n} project(s) to {name} at {new_addr}; restart running ones to reconnect"),
                    Notice::Info,
                );
            }
            Err(e) => {
                self.message = (
                    format!("Server saved, but its projects were not moved: {e:#}"),
                    Notice::Error,
                );
            }
        }
    }

    /// Sync the skills modal's picks into its project (`false` closes modal;
    /// `true` keeps it open on failure).
    pub(super) fn sync_skill_modal(&mut self, modal: &VaultModal, overwrite: bool) -> bool {
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
                        crate::app::actions::kept_skills_note(&kept)
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
    pub(super) fn commit_vault_modal(&mut self, modal: &mut VaultModal) -> bool {
        match modal.target {
            VaultTarget::AddForm => {
                let picked = modal.picked.clone();
                match &modal.kind {
                    VaultKindState::Env { .. } => self.add.env = picked,
                    VaultKindState::Skill { .. } => self.add.skills = picked,
                    VaultKindState::Tool { .. } => self.add.tools = picked,
                    VaultKindState::Ref { .. } => self.add.refs = picked,
                    VaultKindState::Model { .. } | VaultKindState::Server { .. } => {}
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
                    VaultKindState::Model { project_dotenv, .. } => {
                        match models::sync_project_models(
                            &project_path,
                            &self.models_vault,
                            &modal.picked,
                            project_dotenv,
                        ) {
                            Ok((0, 0)) => false,
                            Ok((written, removed)) => {
                                self.message = (
                                    format!(
                                        "Updated {project_name}/.env models ({written} set, -{removed} removed); restart its session to use them"
                                    ),
                                    Notice::Info,
                                );
                                false
                            }
                            Err(e) => {
                                self.message = (format!("Models failed: {e:#}"), Notice::Error);
                                true
                            }
                        }
                    }
                    VaultKindState::Server { .. } => false,
                }
            }
        }
    }
}
