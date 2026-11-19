//! Confirmations, the right-click menu, orders and briefings.

use super::*;

impl App {
    /// Open the one-line prompt for whom agent `pi` reports to (empty clears
    /// it), prefilled with the current commander. Shared by `u` and the menu.
    pub(super) fn begin_set_superior(&mut self, pi: usize) {
        let p = &self.cfg.projects[pi];
        if p.kind != Kind::Agent {
            self.message = (
                "Only agents report to a commander".to_string(),
                Notice::Info,
            );
            return;
        }
        self.send_buf.clear();
        for ch in p.reports_to.clone().unwrap_or_default().chars() {
            self.send_buf.insert(ch);
        }
        self.mode = Mode::Send;
        self.send_to = SendTo::Under;
    }

    /// Point agent `pi` at a new commander (empty clears it), saving to the
    /// config file first so a failed save changes nothing.
    pub(super) fn set_superior(&mut self, pi: usize, under: &str) -> (String, Notice) {
        let name = self.cfg.projects[pi].name.clone();
        let stored = if under.is_empty() {
            None
        } else {
            match check_reassign(&self.cfg.projects, &name, under) {
                Ok(c) => Some(c),
                Err(e) => return (e, Notice::Error),
            }
        };
        if let Err(e) = persist_under(&name, stored.as_deref()) {
            return (format!("Save failed: {e:#}"), Notice::Error);
        }
        let old = std::mem::replace(&mut self.cfg.projects[pi].reports_to, stored.clone());
        enable_chain_ledger(&mut self.cfg.projects, pi);
        ledger::report_transfer(&self.cfg.projects, &name, old.as_deref());
        ledger::write_rosters(&self.cfg.projects);
        // The new tree must be visible, so drop any filter and follow the agent.
        self.filter_buf.clear();
        self.refresh_prefixes();
        self.refresh_filter();
        self.selected = self.order.iter().position(|&x| x == pi).unwrap_or(0);
        let msg = match stored {
            Some(c) => format!("{name} now reports to {c}"),
            None => format!("{name} now reports to no one"),
        };
        (msg, Notice::Info)
    }

    /// Type `line` as an order into every running agent below commander `ci`.
    /// An agent showing a question (`?`) is skipped: the Enter that ends the
    /// order could answer it.
    pub(super) fn issue_order(&mut self, ci: usize, line: &str) -> (String, Notice) {
        let from = self.cfg.projects[ci].name.clone();
        let mut bytes = format!("Order from \"{from}\": {line}").into_bytes();
        bytes.push(b'\r');
        let (mut sent, mut held) = (0, 0);
        for j in all_subordinates(&self.cfg.projects, ci) {
            let delivered = match self.sessions[j].as_mut().filter(|s| s.status.is_running()) {
                Some(s) if s.phase() != AgentPhase::NeedsInput => {
                    let ok = s.input(&bytes).is_ok();
                    if ok {
                        s.log_event(&format!("order from {from}"));
                    }
                    ok
                }
                _ => false,
            };
            if delivered {
                sent += 1;
            } else {
                held += 1;
            }
        }
        let note = if held > 0 {
            format!("Order sent to {sent}; {held} not running or waiting on a question")
        } else {
            format!("Order sent to {sent}")
        };
        (note, if sent > 0 { Notice::Info } else { Notice::Warn })
    }

    pub(super) fn key_confirm_update(&mut self, k: KeyEvent) {
        match confirm_key(k.code) {
            Some(true) => {
                self.confirm_update = false;
                if let Some(tag) = self.update_offer.take() {
                    self.updating = true;
                    self.message = (format!("Updating to {tag}..."), Notice::Info);
                    let tx = self.upd_tx.clone();
                    thread::spawn(move || {
                        let m = match install_update(&tag) {
                            Ok(()) => UpdateMsg::Installed(tag),
                            Err(e) => UpdateMsg::Failed(format!("{e:#}")),
                        };
                        let _ = tx.send(m);
                    });
                }
            }
            Some(false) => self.dismiss_update(),
            None => {}
        }
    }

    pub(super) fn dismiss_update(&mut self) {
        self.confirm_update = false;
        self.update_offer = None;
        self.message = (
            "Update skipped; squad asks again next launch.".to_string(),
            Notice::Info,
        );
    }

    /// Enter in the edit form: save the new path, command and autostart. A
    /// new folder renames an agent after it (see `relocate`).
    pub(crate) fn save_edit(&mut self) {
        let Some(mut name) = self.add_edit.clone() else {
            return;
        };
        let Some(pi) = self
            .cfg
            .projects
            .iter()
            .position(|p| p.name.eq_ignore_ascii_case(&name))
        else {
            return;
        };
        // A missing folder is made on the spot (the field says so).
        if self.add_state == PathState::Missing {
            if let Err(e) = create_missing_dir(&self.add_path.as_str()) {
                self.add_err = format!("{e:#}");
                return;
            }
        }
        let new_path = match resolve_dir(clean_path(&self.add_path.as_str())) {
            Ok((path, _)) => path,
            Err(e) => {
                self.add_err = e;
                return;
            }
        };
        let moved = !same_path(&new_path, &self.cfg.projects[pi].path);
        // An agent whose name drifted from its folder (older registrations,
        // shared-folder suffixes that no longer clash) takes the folder's name
        // on save even when the path is unchanged, as the form previews.
        let renames = self.cfg.projects[pi].kind == Kind::Agent && {
            let mut probe = self.cfg.projects.clone();
            relocate(&mut probe, pi, new_path.clone()) != name
        };
        let was_running = slot_running(&self.sessions, pi);
        if moved || renames {
            if self.is_starting(pi) {
                self.add_err = format!("{name} is still starting; wait a moment before moving it.");
                return;
            }
            let new_name = match persist_move(&name, &new_path) {
                Ok(n) => n,
                Err(e) => {
                    self.add_err = format!("Save failed: {e:#}");
                    return;
                }
            };
            // Follow the authoritative name returned by disk so subordinates in
            // memory stay aligned even if another process claimed the base name.
            apply_relocate(&mut self.cfg.projects, pi, new_path, &new_name);
            name = new_name;
            ledger::ensure_agent(&self.cfg.projects[pi]);
            ledger::write_rosters(&self.cfg.projects);
            // The new name must be visible, so drop any filter and follow it.
            self.filter_buf.clear();
            self.refresh_prefixes();
            self.order = display_order(&self.cfg.projects);
            self.selected = self.order.iter().position(|&x| x == pi).unwrap_or(0);
        }
        let cmd = split_command(&self.add_cmd.as_str());
        if let Err(e) = persist_edit(&name, cmd.clone(), self.add_autostart) {
            self.add_err = format!("Save failed: {e:#}");
            return;
        }
        let autostart = self.add_autostart;
        let path = {
            let p = &mut self.cfg.projects[pi];
            p.command = if cmd.is_empty() { shell() } else { cmd };
            p.autostart = autostart;
            p.path.clone()
        };
        let current_dotenv = envvault::read_dotenv(&path);
        let env_note = match self.sync_project_dotenv(&path, &self.add_env, &current_dotenv) {
            Ok((0, 0, 0)) => String::new(),
            Ok((added, updated, removed)) => format!("; .env (+{added}, ~{updated}, -{removed})"),
            Err(e) => {
                self.add_err = e;
                return;
            }
        };
        let current_skills = skills::read_project_skills(&path, &self.skills_vault);
        let skills_note = match skills::sync_project_skills(
            &path,
            &self.skills_vault,
            &self.add_skills,
            &current_skills,
        ) {
            Ok((0, 0, 0)) => String::new(),
            Ok((added, updated, removed)) => {
                format!("; skills (+{added}, ~{updated}, -{removed})")
            }
            Err(e) => {
                self.add_err = format!("skills failed: {e:#}");
                return;
            }
        };
        let current_tools = tools::read_project_tools(&path, &self.tools_vault);
        let tools_note = match tools::sync_project_tools(
            &path,
            &self.tools_vault,
            &self.add_tools,
            &current_tools,
        ) {
            Ok((0, 0, 0)) => String::new(),
            Ok((added, updated, removed)) => {
                format!("; tools (+{added}, ~{updated}, -{removed})")
            }
            Err(e) => {
                self.add_err = format!("tools failed: {e:#}");
                return;
            }
        };
        let current_refs = refs::read_project_refs(&path, &self.refs_vault);
        let refs_note =
            match refs::sync_project_refs(&path, &self.refs_vault, &self.add_refs, &current_refs) {
                Ok((0, 0, 0)) => String::new(),
                Ok((added, updated, removed)) => {
                    format!("; refs (+{added}, ~{updated}, -{removed})")
                }
                Err(e) => {
                    self.add_err = format!("refs failed: {e:#}");
                    return;
                }
            };
        self.add_edit = None;
        self.add_env.clear();
        self.add_skills.clear();
        self.add_tools.clear();
        self.add_refs.clear();
        self.mode = Mode::Manager;
        let note = match (was_running, moved) {
            (true, true) => "; restart it to use the new folder and command",
            (true, false) => "; restart it to use the new command",
            _ => "",
        };
        self.message = (
            format!("Saved {name}{note}{env_note}{skills_note}{tools_note}{refs_note}"),
            Notice::Info,
        );
    }

    pub(crate) fn apply_add_env(
        &self,
        dir: &std::path::Path,
    ) -> Result<Option<(usize, usize)>, String> {
        if self.add_env.is_empty() {
            return Ok(None);
        }
        let keys: Vec<String> = self.add_env.iter().cloned().collect();
        envvault::apply_dotenv_keys(dir, &self.env_vault, &keys)
            .map(Some)
            .map_err(|e| format!(".env failed: {e:#}"))
    }

    pub(crate) fn apply_add_skills(
        &self,
        dir: &std::path::Path,
    ) -> Result<Option<(usize, usize)>, String> {
        if self.add_skills.is_empty() {
            return Ok(None);
        }
        let names: Vec<String> = self.add_skills.iter().cloned().collect();
        skills::apply_skill_names(dir, &self.skills_vault, &names)
            .map(Some)
            .map_err(|e| format!("skills failed: {e:#}"))
    }

    pub(crate) fn apply_add_tools(
        &self,
        dir: &std::path::Path,
    ) -> Result<Option<(usize, usize)>, String> {
        if self.add_tools.is_empty() {
            return Ok(None);
        }
        let names: Vec<String> = self.add_tools.iter().cloned().collect();
        tools::apply_tool_names(dir, &self.tools_vault, &names)
            .map(Some)
            .map_err(|e| format!("tools failed: {e:#}"))
    }

    pub(crate) fn apply_add_refs(
        &self,
        dir: &std::path::Path,
    ) -> Result<Option<(usize, usize)>, String> {
        if self.add_refs.is_empty() {
            return Ok(None);
        }
        let names: Vec<String> = self.add_refs.iter().cloned().collect();
        refs::apply_ref_names(dir, &self.refs_vault, &names)
            .map(Some)
            .map_err(|e| format!("refs failed: {e:#}"))
    }

    /// Write checked vault keys whose value is missing/changed and remove
    /// unchecked vault keys from `<dir>/.env`, leaving non-vault keys untouched.
    pub(crate) fn sync_project_dotenv(
        &self,
        dir: &std::path::Path,
        picked: &BTreeSet<String>,
        current: &BTreeMap<String, String>,
    ) -> Result<(usize, usize, usize), String> {
        let write: Vec<String> = picked
            .iter()
            .filter(|k| {
                let want = self.env_vault.vars.iter().find(|e| &e.key == *k);
                want.map(|e| &e.value) != current.get(*k)
            })
            .cloned()
            .collect();
        let remove: Vec<String> = self
            .env_vault
            .vars
            .iter()
            .filter(|e| current.contains_key(&e.key) && !picked.contains(&e.key))
            .map(|e| e.key.clone())
            .collect();
        if write.is_empty() && remove.is_empty() {
            return Ok((0, 0, 0));
        }
        envvault::apply_dotenv_keys(dir, &self.env_vault, &write)
            .and_then(|(a, u)| envvault::remove_dotenv(dir, &remove).map(|r| (a, u, r)))
            .map_err(|e| format!(".env failed: {e:#}"))
    }

    /// `f`: pin or unpin the selected project. Pinned ones list first in
    /// their group; the selection stays on the same project.
    pub(super) fn toggle_pin(&mut self) {
        let Some(&pi) = self.order.get(self.selected) else {
            return;
        };
        let (name, pinned) = {
            let p = &self.cfg.projects[pi];
            (p.name.clone(), !p.pinned)
        };
        if let Err(e) = persist_pin(&name, pinned) {
            self.message = (format!("Save failed: {e:#}"), Notice::Error);
            return;
        }
        self.cfg.projects[pi].pinned = pinned;
        self.refresh_prefixes();
        self.refresh_filter();
        self.selected = self.order.iter().position(|&x| x == pi).unwrap_or(0);
        let what = if pinned { "Pinned" } else { "Unpinned" };
        self.message = (format!("{what} {name}"), Notice::Info);
    }

    /// `G`: search every session's scrollback without leaving the manager.
    /// Typing narrows the hit list live; `Enter` jumps to the picked
    /// session with the same query loaded into its own find (`F`).
    pub(super) fn open_gsearch(&mut self) {
        self.gsearch = Some(GSearch {
            query: String::new(),
            hits: Vec::new(),
            sel: 0,
        });
        self.dirty = true;
    }

    /// Recompute the global hit list for the current query, in sidebar order.
    /// Sessions keep their scroll position: `search_scrollback` restores it.
    pub(super) fn refresh_gsearch(&mut self) {
        let query = self
            .gsearch
            .as_ref()
            .map(|g| g.query.clone())
            .unwrap_or_default();
        let order = self.order.clone();
        let mut hits = Vec::new();
        if !query.is_empty() {
            for &pi in &order {
                let n = self
                    .sessions
                    .get_mut(pi)
                    .and_then(|o| o.as_mut())
                    .map(|s| s.search_scrollback(&query).len())
                    .unwrap_or(0);
                if n > 0 {
                    hits.push(GHit { pi, count: n });
                }
            }
        }
        if let Some(g) = self.gsearch.as_mut() {
            g.hits = hits;
            g.sel = g.sel.min(g.hits.len().saturating_sub(1));
        }
        self.dirty = true;
    }

    /// Jump to the picked global hit: select its project and load the same
    /// query into that session's own find, scrolled to the newest match.
    /// A project filtered out of the sidebar clears the filter first.
    pub(super) fn jump_gsearch(&mut self) {
        let (pi, query, total) = match self.gsearch.as_ref() {
            Some(g) if !g.query.is_empty() => match g.hits.get(g.sel) {
                Some(hit) => (
                    hit.pi,
                    g.query.clone(),
                    g.hits.iter().map(|h| h.count).sum::<usize>(),
                ),
                None => return,
            },
            _ => return,
        };
        self.gsearch = None;
        if !self.order.contains(&pi) {
            self.filter_buf.clear();
            self.refresh_filter();
        }
        if let Some(pos) = self.order.iter().position(|&x| x == pi) {
            self.selected = pos;
        }
        self.search_buf.set(&query);
        self.search_pi = Some(pi);
        self.sync_search(true);
        let name = self
            .cfg
            .projects
            .get(pi)
            .map(|p| p.name.as_str())
            .unwrap_or("?");
        self.message = (
            format!("{total} matches for \"{query}\" ({name} selected)"),
            Notice::Info,
        );
        self.dirty = true;
    }

    pub(super) fn key_gsearch(&mut self, k: KeyEvent) {
        if self.gsearch.is_none() {
            return;
        }
        match k.code {
            KeyCode::Esc => self.gsearch = None,
            KeyCode::Enter => self.jump_gsearch(),
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(g) = self.gsearch.as_mut() {
                    g.sel = (g.sel + 1).min(g.hits.len().saturating_sub(1));
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(g) = self.gsearch.as_mut() {
                    g.sel = g.sel.saturating_sub(1);
                }
            }
            KeyCode::Backspace => {
                if let Some(g) = self.gsearch.as_mut() {
                    g.query.pop();
                }
                self.refresh_gsearch();
            }
            KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(g) = self.gsearch.as_mut() {
                    g.query.push(c);
                }
                self.refresh_gsearch();
            }
            _ => {}
        }
        self.dirty = true;
    }

    /// `h`: read the selected project's SITREPs without leaving squad.
    pub(super) fn open_history(&mut self) {
        let Some(p) = self
            .order
            .get(self.selected)
            .and_then(|&pi| self.cfg.projects.get(pi))
        else {
            return;
        };
        // Agent-written SITREPs and squad's own entries, newest first.
        let items = ledger::sitreps(&p.path);
        if items.is_empty() {
            self.message = (format!("No history in {} yet", p.name), Notice::Warn);
        } else {
            self.history = Some(History::open(&p.name, items));
        }
    }

    pub(super) fn key_history(&mut self, k: KeyEvent) {
        let Some(h) = self.history.as_mut() else {
            return;
        };
        // While filtering, printable keys narrow the entry list.
        if h.typing {
            match k.code {
                KeyCode::Esc => {
                    h.query.clear();
                    h.typing = false;
                    h.refilter();
                }
                KeyCode::Enter => h.typing = false,
                KeyCode::Backspace => {
                    h.query.pop();
                    h.refilter();
                }
                KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => {
                    h.query.push(c);
                    h.refilter();
                }
                _ => {}
            }
            self.dirty = true;
            return;
        }
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('h') => self.history = None,
            KeyCode::Char('/') => {
                h.query.clear();
                h.typing = true;
                h.refilter();
            }
            KeyCode::Down | KeyCode::Char('j') => h.select(h.sel + 1),
            KeyCode::Up | KeyCode::Char('k') => h.select(h.sel.saturating_sub(1)),
            KeyCode::Char('J') | KeyCode::Char(']') => h.scroll_by(1),
            KeyCode::Char('K') | KeyCode::Char('[') => h.scroll_by(-1),
            KeyCode::PageDown | KeyCode::Char(' ') => h.scroll_by(10),
            KeyCode::PageUp => h.scroll_by(-10),
            KeyCode::Home => h.select(0),
            KeyCode::End => h.select(usize::MAX),
            _ => {}
        }
        self.dirty = true;
    }

    pub(crate) fn selected_vault_target(&self) -> VaultTarget {
        self.order
            .get(self.selected)
            .copied()
            .map_or(VaultTarget::VaultOnly, VaultTarget::Project)
    }

    /// Open Management row `m`: `0` Memory monitor, `1` Env vault, `2` Skills,
    /// `3` Tools, `4` References (scoped to the selected project, or vault-only when none exist).
    pub(super) fn open_mgmt(&mut self, m: usize) {
        self.mode = Mode::Manager;
        let target = self.selected_vault_target();
        match m {
            0 => self.open_procs(),
            1 => self.open_env_modal(target),
            2 => self.open_skill_modal(target),
            3 => self.open_tool_modal(target),
            4 => self.open_ref_modal(target),
            _ => {}
        }
    }

    pub(super) fn open_procs(&mut self) {
        let mut p = ProcList {
            items: top_processes(PROC_ROWS),
            sel: 0,
            confirm: None,
            top: Default::default(),
            cmd: None,
        };
        p.load_cmd();
        self.procs = Some(p);
    }

    pub(super) fn key_procs(&mut self, k: KeyEvent) {
        let Some(p) = self.procs.as_mut() else {
            return;
        };
        self.dirty = true;
        // A pending kill takes only the answer; anything but `y` cancels.
        if let Some(t) = p.confirm.take() {
            if matches!(k.code, KeyCode::Char('y') | KeyCode::Enter) {
                // Re-resolve the PID now: the row may be stale and PIDs get
                // recycled, so a stale row must not kill an unrelated tree.
                let recorded = p
                    .cmd
                    .as_ref()
                    .and_then(|(cpid, cmd)| (*cpid == t.pid).then_some(cmd.as_deref()).flatten());
                match kill_recheck(t.pid, recorded) {
                    Some(why) => {
                        self.message = (format!("Not killing {}: {why}", t.name), Notice::Warn);
                        p.refresh();
                    }
                    None => {
                        kill_trees(&[t.pid]);
                        self.message = (format!("Killed {} ({})", t.name, t.pid), Notice::Info);
                        p.refresh();
                    }
                }
            }
            return;
        }
        let last = p.items.len().saturating_sub(1);
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('M') => self.procs = None,
            KeyCode::Down | KeyCode::Char('j') => p.sel = (p.sel + 1).min(last),
            KeyCode::Up | KeyCode::Char('k') => p.sel = p.sel.saturating_sub(1),
            KeyCode::PageDown => p.sel = (p.sel + 10).min(last),
            KeyCode::PageUp => p.sel = p.sel.saturating_sub(10),
            KeyCode::Home => p.sel = 0,
            KeyCode::End => p.sel = last,
            KeyCode::Char('r') => {
                p.refresh();
                self.message = ("Process list refreshed.".to_string(), Notice::Info);
            }
            KeyCode::Char('x') | KeyCode::Char('d') | KeyCode::Delete => {
                if let Some(t) = p.items.get(p.sel) {
                    match kill_refusal(t.pid) {
                        Some(why) => {
                            self.message = (format!("Not killing {}: {why}", t.name), Notice::Warn)
                        }
                        None => p.confirm = Some(t.clone()),
                    }
                }
            }
            _ => {}
        }
        if let Some(p) = self.procs.as_mut() {
            p.load_cmd();
        }
    }

    /// The squad project whose session `it` runs inside, if any.
    pub(super) fn proc_owner(&self, it: &ProcInfo) -> Option<&str> {
        self.sessions.iter().enumerate().find_map(|(pi, slot)| {
            let pid = slot.as_ref()?.child.process_id()?;
            (pid == it.pid || it.ancestors.contains(&pid))
                .then(|| self.cfg.projects.get(pi).map(|p| p.name.as_str()))?
        })
    }

    /// The update is installed: `y` restarts squad into it (stopping the
    /// sessions), `n` keeps the old build running until the next launch.
    pub(super) fn key_confirm_apply(&mut self, k: KeyEvent) {
        match confirm_key(k.code) {
            Some(true) => {
                self.confirm_apply = false;
                self.relaunch = true;
                self.quit = true;
            }
            Some(false) => self.dismiss_apply(),
            None => {}
        }
    }

    pub(super) fn dismiss_apply(&mut self) {
        self.confirm_apply = false;
        if let Some(tag) = self.applied.take() {
            self.message = (
                format!("{tag} is installed; it runs the next time you start squad."),
                Notice::Info,
            );
        }
    }

    pub(super) fn key_confirm_quit(&mut self, k: KeyEvent) {
        match confirm_key(k.code) {
            Some(true) => {
                self.quit = true;
            }
            Some(false) => self.confirm_quit = false,
            None => {}
        }
    }

    pub(super) fn key_confirm_start_all(&mut self, k: KeyEvent, rows: u16, cols: u16) {
        match confirm_key(k.code) {
            Some(true) => {
                // Each starts on its own worker: one slow start stalls
                // neither the others nor the keyboard.
                let todo: Vec<usize> = self
                    .order
                    .iter()
                    .copied()
                    .filter(|&pi| !slot_running(&self.sessions, pi))
                    .collect();
                for &pi in &todo {
                    self.begin_start(pi, rows, cols, Vec::new());
                }
                self.message = if todo.is_empty() {
                    ("All sessions already running.".to_string(), Notice::Info)
                } else {
                    (
                        format!("Starting {}.", plural_sessions(todo.len())),
                        Notice::Info,
                    )
                };
                self.confirm_start_all = false;
            }
            Some(false) => {
                self.confirm_start_all = false;
                self.message = ("Cancelled.".to_string(), Notice::Info);
            }
            None => {}
        }
    }

    pub(super) fn key_confirm_stop(&mut self, k: KeyEvent) {
        match confirm_key(k.code) {
            Some(true) => {
                if let Some(pi) = self.confirm_stop.take() {
                    self.stop_slot(pi, "stopped by user");
                    self.message = (
                        "Session stopped. Enter restarts it.".to_string(),
                        Notice::Warn,
                    );
                }
            }
            Some(false) => {
                self.confirm_stop = None;
            }
            None => {}
        }
    }

    pub(super) fn key_confirm_restart(&mut self, k: KeyEvent, rows: u16, cols: u16) {
        match confirm_key(k.code) {
            Some(true) => {
                if let Some(pi) = self.confirm_restart.take() {
                    self.restart(pi, rows, cols);
                }
            }
            Some(false) => self.confirm_restart = None,
            None => {}
        }
    }

    pub(super) fn key_confirm_remove(&mut self, k: KeyEvent) {
        match confirm_key(k.code) {
            Some(true) => {
                if let Some(pi) = self.confirm_remove.take() {
                    // Save first: a failed save leaves the project and
                    // its running session untouched.
                    let name = self.cfg.projects[pi].name.clone();
                    match persist_remove(&name) {
                        Ok(()) => {
                            self.stop_slot(pi, "project removed");
                            remove_project(&mut self.cfg.projects, &mut self.sessions, pi);
                            self.refresh_prefixes();
                            // Keep an active filter across the removal.
                            self.refresh_filter();
                            // Whoever shared its folder or reported to it reads a current roster.
                            ledger::write_rosters(&self.cfg.projects);
                            self.message = (format!("Removed {name}."), Notice::Warn);
                        }
                        Err(e) => {
                            self.message = (format!("Remove failed: {e:#}"), Notice::Error);
                        }
                    }
                }
            }
            Some(false) => {
                self.confirm_remove = None;
            }
            None => {}
        }
    }

    /// Flip crash auto-restart for a service, effective on the next crash.
    pub(super) fn toggle_restart_on_crash(&mut self, pi: usize) {
        let Some(p) = self.cfg.projects.get(pi) else {
            return;
        };
        if p.kind != Kind::Service {
            return;
        }
        let name = p.name.clone();
        let next = !p.restart_on_crash;
        match persist_restart_on_crash(&name, next) {
            Ok(()) => {
                if let Some(q) = self.cfg.projects.iter_mut().find(|q| q.name == name) {
                    q.restart_on_crash = next;
                }
                self.message = (
                    format!("{name} auto-restart {}", if next { "on" } else { "off" }),
                    Notice::Info,
                );
            }
            Err(e) => {
                self.message = (format!("Save failed: {e:#}"), Notice::Error);
            }
        }
        self.dirty = true;
    }

    /// Run one row of the right-click quick-action menu. `action` is the
    /// row index into the kind-aware menu (services get a trailing
    /// auto-restart row past the base rows).
    pub(super) fn run_context_action(&mut self, pi: usize, action: usize, rows: u16, cols: u16) {
        if self.cfg.projects.get(pi).is_none() {
            return;
        }
        // Select the project the menu was opened for, so the action and
        // the sidebar agree even if the selection moved meanwhile.
        if let Some(pos) = self.order.iter().position(|&x| x == pi) {
            self.selected = pos;
        }
        // Services get one trailing row: the crash auto-restart toggle.
        let service_extra = self
            .cfg
            .projects
            .get(pi)
            .is_some_and(|p| p.kind == Kind::Service);
        if slot_running(&self.sessions, pi) {
            match action {
                13 if service_extra => self.toggle_restart_on_crash(pi),
                0 => self.mode = Mode::Focus,
                1 => {
                    self.mode = Mode::Send;
                    self.send_to = SendTo::One;
                    self.send_buf.clear();
                }
                2 => self.confirm_stop = Some(pi),
                3 => self.confirm_restart = Some(pi),
                4 => self.open_edit(),
                5 => self.open_history(),
                6 => self.toggle_pin(),
                7 => self.begin_set_superior(pi),
                8 => self.confirm_remove = Some(pi),
                9 => self.open_env_info(pi),
                10 => self.open_skill_modal(VaultTarget::Project(pi)),
                11 => self.open_tool_modal(VaultTarget::Project(pi)),
                12 => self.open_ref_modal(VaultTarget::Project(pi)),
                _ => {}
            }
            return;
        }
        match action {
            11 if service_extra => self.toggle_restart_on_crash(pi),
            0 => self.open_session(pi, rows, cols),
            1 => {
                self.mode = Mode::Send;
                self.send_to = SendTo::One;
                self.send_buf.clear();
            }
            2 => self.open_edit(),
            3 => self.open_history(),
            4 => self.toggle_pin(),
            5 => self.begin_set_superior(pi),
            6 => self.confirm_remove = Some(pi),
            7 => self.open_env_info(pi),
            8 => self.open_skill_modal(VaultTarget::Project(pi)),
            9 => self.open_tool_modal(VaultTarget::Project(pi)),
            10 => self.open_ref_modal(VaultTarget::Project(pi)),
            _ => {}
        }
    }

    /// Keys while the quick-action menu is open: arrows/`j`/`k` move highlight,
    /// `Enter` runs the highlighted row, shortcut keys run directly, `Esc` closes.
    pub(super) fn key_context_menu(&mut self, k: KeyEvent, rows: u16, cols: u16) {
        let Some(pi) = self.context_menu else {
            return;
        };
        let (kind, restart_on_crash) = self
            .cfg
            .projects
            .get(pi)
            .map(|p| (p.kind, p.restart_on_crash))
            .unwrap_or((Kind::Agent, false));
        let items =
            context_menu_items_for(slot_running(&self.sessions, pi), kind, restart_on_crash);
        let count = items.len();
        match k.code {
            KeyCode::Esc => self.context_menu = None,
            KeyCode::Down | KeyCode::Char('j') if count > 0 => {
                self.context_sel = (self.context_sel + 1) % count;
            }
            KeyCode::Up | KeyCode::Char('k') if count > 0 => {
                self.context_sel = (self.context_sel + count - 1) % count;
            }
            KeyCode::Enter => {
                let action = self.context_sel.min(count.saturating_sub(1));
                self.context_menu = None;
                self.run_context_action(pi, action, rows, cols);
            }
            KeyCode::Char(c) => {
                if let Some(idx) = items.iter().position(|(key, _)| *key == c) {
                    self.context_menu = None;
                    self.run_context_action(pi, idx, rows, cols);
                }
            }
            _ => {}
        }
    }

    /// Keys in the read-only environment popup.
    pub(super) fn key_env_info(&mut self, k: KeyEvent) {
        let Some(info) = self.env_info.as_mut() else {
            return;
        };
        match k.code {
            KeyCode::Char('v') => info.reveal = !info.reveal,
            KeyCode::Char('a') => {
                let pi = info.pi;
                self.env_info = None;
                self.open_env_modal(EnvTarget::Project(pi));
                if let Some(m) = self.env_modal.as_mut() {
                    if m.editor.is_none() {
                        m.editor = Some(EnvEditor::blank());
                    }
                }
            }
            KeyCode::Char('e' | 'E') => {
                let pi = info.pi;
                self.env_info = None;
                self.open_env_modal(EnvTarget::Project(pi));
            }
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => self.env_info = None,
            _ => {}
        }
    }

    pub(super) fn key_env_modal(&mut self, k: KeyEvent) {
        let Some(modal) = self.env_modal.as_mut() else {
            return;
        };
        if let Some(target_key) = modal.confirm_delete.clone() {
            match confirm_key(k.code) {
                Some(true) => {
                    modal.confirm_delete = None;
                    let mut next_vault = self.env_vault.clone();
                    envvault::remove_env(&mut next_vault, &target_key);
                    match envvault::save_env(&next_vault) {
                        Ok(()) => {
                            self.env_vault = next_vault;
                            modal.picked.remove(&target_key);
                            self.add_env.remove(&target_key);
                            let len = self.env_vault.vars.len();
                            modal.sel = modal.sel.min(len.saturating_sub(1));
                        }
                        Err(e) => {
                            self.message = (format!("Save failed: {e:#}"), Notice::Error);
                        }
                    }
                }
                Some(false) => modal.confirm_delete = None,
                None => {}
            }
            return;
        }
        if let Some(ed) = modal.editor.as_mut() {
            match k.code {
                KeyCode::Esc => {
                    modal.editor = None;
                }
                KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down => {
                    ed.field = 1 - ed.field;
                }
                KeyCode::Enter => {
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
                        } else if let Err(e) =
                            envvault::upsert_env(&mut next_vault, &valid_key, &val)
                        {
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
                    if modal.target != EnvTarget::VaultOnly {
                        modal.picked.insert(valid_key);
                    }
                    modal.editor = None;
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
            }
            return;
        }
        let kh = hotkey_event(k);
        let len = self.env_vault.vars.len();
        let can_pick = modal.target != EnvTarget::VaultOnly;
        match kh.code {
            KeyCode::Esc | KeyCode::Char('E') => {
                self.env_modal = None;
            }
            // The rows are the variables, then "+ New variable" (index `len`).
            KeyCode::Up | KeyCode::Char('k') => {
                modal.sel = (modal.sel + len) % (len + 1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                modal.sel = (modal.sel + 1) % (len + 1);
            }
            KeyCode::Enter | KeyCode::Char(' ') if modal.sel >= len => {
                modal.editor = Some(EnvEditor::blank());
            }
            KeyCode::Char(' ') if can_pick && len > 0 => {
                if let Some(entry) = self.env_vault.vars.get(modal.sel) {
                    if !modal.picked.remove(&entry.key) {
                        modal.picked.insert(entry.key.clone());
                    }
                }
            }
            KeyCode::Char('A') if can_pick && len > 0 => {
                if modal.picked.len() == len {
                    modal.picked.clear();
                } else {
                    for entry in &self.env_vault.vars {
                        modal.picked.insert(entry.key.clone());
                    }
                }
            }
            KeyCode::Char('v') => {
                modal.reveal = !modal.reveal;
            }
            KeyCode::Char('a' | 'n') => {
                modal.editor = Some(EnvEditor::blank());
            }
            KeyCode::Char('e') if len > 0 => {
                if let Some(entry) = self.env_vault.vars.get(modal.sel) {
                    let mut key_in = LineInput::new();
                    key_in.set(&entry.key);
                    let mut val_in = LineInput::new();
                    val_in.set(&entry.value);
                    modal.editor = Some(EnvEditor {
                        key: key_in,
                        value: val_in,
                        field: 1,
                        editing_key: Some(entry.key.clone()),
                        err: String::new(),
                    });
                }
            }
            KeyCode::Char('d') | KeyCode::Delete if len > 0 => {
                if let Some(entry) = self.env_vault.vars.get(modal.sel) {
                    modal.confirm_delete = Some(entry.key.clone());
                }
            }
            KeyCode::Enter => {
                let target = modal.target;
                let picked: Vec<String> = modal.picked.iter().cloned().collect();
                match target {
                    EnvTarget::AddForm => {
                        self.add_env = picked.into_iter().collect();
                        self.env_modal = None;
                    }
                    EnvTarget::Project(pi) => {
                        let picked_set = modal.picked.clone();
                        let current_env = modal.project_dotenv.clone();
                        let Some(p) = self.cfg.projects.get(pi) else {
                            self.env_modal = None;
                            return;
                        };
                        match self.sync_project_dotenv(&p.path, &picked_set, &current_env) {
                            Ok((0, 0, 0)) => {
                                self.env_modal = None;
                            }
                            Ok((added, updated, removed)) => {
                                self.message = (
                                    format!(
                                        "Updated {}/.env (+{added} added, {updated} updated, -{removed} removed)",
                                        p.name
                                    ),
                                    Notice::Info,
                                );
                                self.env_modal = None;
                            }
                            Err(e) => {
                                self.message = (e, Notice::Error);
                            }
                        }
                    }
                    VaultTarget::VaultOnly => {
                        self.env_modal = None;
                    }
                }
            }
            _ => {}
        }
    }

    pub(super) fn key_skill_modal(&mut self, k: KeyEvent) {
        let Some(modal) = self.skill_modal.as_mut() else {
            return;
        };
        if let Some(target_name) = modal.confirm_delete.clone() {
            match confirm_key(k.code) {
                Some(true) => {
                    modal.confirm_delete = None;
                    let mut next_vault = self.skills_vault.clone();
                    skills::remove_skill(&mut next_vault, &target_name);
                    match skills::save_skills(&next_vault) {
                        Ok(()) => {
                            self.skills_vault = next_vault;
                            modal.picked.remove(&target_name);
                            self.add_skills.remove(&target_name);
                            let len = self.skills_vault.skills.len();
                            modal.sel = modal.sel.min(len.saturating_sub(1));
                        }
                        Err(e) => {
                            self.message = (format!("Save failed: {e:#}"), Notice::Error);
                        }
                    }
                }
                Some(false) => modal.confirm_delete = None,
                None => {}
            }
            return;
        }
        if let Some(ed) = modal.editor.as_mut() {
            match k.code {
                KeyCode::Esc => modal.editor = None,
                KeyCode::Tab | KeyCode::Down => ed.field = (ed.field + 1) % 3,
                KeyCode::BackTab | KeyCode::Up => ed.field = (ed.field + 2) % 3,
                KeyCode::Enter => {
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
                            if let Some(pos) = next_vault.skills.iter().position(|s| s.name == old)
                            {
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
                    modal.editor = None;
                }
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
            }
            return;
        }
        let kh = hotkey_event(k);
        let len = self.skills_vault.skills.len();
        let can_pick = modal.target != VaultTarget::VaultOnly;
        match kh.code {
            KeyCode::Esc | KeyCode::Char('K') => self.skill_modal = None,
            KeyCode::Up | KeyCode::Char('k') => modal.sel = (modal.sel + len) % (len + 1),
            KeyCode::Down | KeyCode::Char('j') => modal.sel = (modal.sel + 1) % (len + 1),
            KeyCode::Enter | KeyCode::Char(' ') if modal.sel >= len => {
                modal.editor = Some(SkillEditor::blank());
            }
            KeyCode::Char(' ') if can_pick && len > 0 => {
                if let Some(entry) = self.skills_vault.skills.get(modal.sel) {
                    if !modal.picked.remove(&entry.name) {
                        modal.picked.insert(entry.name.clone());
                    }
                }
            }
            KeyCode::Char('A') if can_pick && len > 0 => {
                toggle_all_picked(
                    &mut modal.picked,
                    self.skills_vault.skills.iter().map(|s| &s.name),
                );
            }
            KeyCode::Char('a' | 'n') => modal.editor = Some(SkillEditor::blank()),
            KeyCode::Char('e') if len > 0 => {
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
                    modal.editor = Some(SkillEditor {
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
            KeyCode::Char('d') | KeyCode::Delete if len > 0 => {
                if let Some(entry) = self.skills_vault.skills.get(modal.sel) {
                    modal.confirm_delete = Some(entry.name.clone());
                }
            }
            KeyCode::Enter => {
                let target = modal.target;
                let picked = modal.picked.clone();
                match target {
                    VaultTarget::AddForm => {
                        self.add_skills = picked;
                        self.skill_modal = None;
                    }
                    VaultTarget::Project(pi) => {
                        let current = modal.project_state.clone();
                        let Some(p) = self.cfg.projects.get(pi) else {
                            self.skill_modal = None;
                            return;
                        };
                        match skills::sync_project_skills(
                            &p.path,
                            &self.skills_vault,
                            &picked,
                            &current,
                        ) {
                            Ok((0, 0, 0)) => self.skill_modal = None,
                            Ok((added, updated, removed)) => {
                                self.message = (
                                    format!(
                                        "Updated {} skills (+{added} added, {updated} updated, -{removed} removed)",
                                        p.name
                                    ),
                                    Notice::Info,
                                );
                                self.skill_modal = None;
                            }
                            Err(e) => {
                                self.message = (format!("Skills failed: {e:#}"), Notice::Error);
                            }
                        }
                    }
                    VaultTarget::VaultOnly => self.skill_modal = None,
                }
            }
            _ => {}
        }
    }

    pub(super) fn key_tool_modal(&mut self, k: KeyEvent) {
        let Some(modal) = self.tool_modal.as_mut() else {
            return;
        };
        if let Some(target_name) = modal.confirm_delete.clone() {
            match confirm_key(k.code) {
                Some(true) => {
                    modal.confirm_delete = None;
                    let mut next_vault = self.tools_vault.clone();
                    tools::remove_tool(&mut next_vault, &target_name);
                    match tools::save_tools(&next_vault) {
                        Ok(()) => {
                            self.tools_vault = next_vault;
                            modal.picked.remove(&target_name);
                            self.add_tools.remove(&target_name);
                            let len = self.tools_vault.tools.len();
                            modal.sel = modal.sel.min(len.saturating_sub(1));
                        }
                        Err(e) => {
                            self.message = (format!("Save failed: {e:#}"), Notice::Error);
                        }
                    }
                }
                Some(false) => modal.confirm_delete = None,
                None => {}
            }
            return;
        }
        if let Some(ed) = modal.editor.as_mut() {
            match k.code {
                KeyCode::Esc => modal.editor = None,
                KeyCode::Tab | KeyCode::Down => ed.field = (ed.field + 1) % 4,
                KeyCode::BackTab | KeyCode::Up => ed.field = (ed.field + 3) % 4,
                KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') if ed.field == 0 => {
                    ed.kind = ed.kind.toggled();
                    ed.err.clear();
                }
                KeyCode::Enter => {
                    let valid_name = match tools::validate_tool_name(&ed.name.as_str()) {
                        Ok(n) => n,
                        Err(e) => {
                            ed.err = e;
                            return;
                        }
                    };
                    let valid_cmd =
                        match tools::validate_tool_command(ed.kind, &ed.command.as_str()) {
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
                    modal.editor = None;
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
            }
            return;
        }
        let kh = hotkey_event(k);
        let len = self.tools_vault.tools.len();
        let can_pick = modal.target != VaultTarget::VaultOnly;
        match kh.code {
            KeyCode::Esc | KeyCode::Char('T') => self.tool_modal = None,
            KeyCode::Up | KeyCode::Char('k') => modal.sel = (modal.sel + len) % (len + 1),
            KeyCode::Down | KeyCode::Char('j') => modal.sel = (modal.sel + 1) % (len + 1),
            KeyCode::Enter | KeyCode::Char(' ') if modal.sel >= len => {
                modal.editor = Some(ToolEditor::blank());
            }
            KeyCode::Char(' ') if can_pick && len > 0 => {
                if let Some(entry) = self.tools_vault.tools.get(modal.sel) {
                    if !modal.picked.remove(&entry.name) {
                        modal.picked.insert(entry.name.clone());
                    }
                }
            }
            KeyCode::Char('A') if can_pick && len > 0 => {
                toggle_all_picked(
                    &mut modal.picked,
                    self.tools_vault.tools.iter().map(|t| &t.name),
                );
            }
            KeyCode::Char('a' | 'n') => modal.editor = Some(ToolEditor::blank()),
            KeyCode::Char('e') if len > 0 => {
                if let Some(entry) = self.tools_vault.tools.get(modal.sel) {
                    let mut name_in = LineInput::new();
                    name_in.set(&entry.name);
                    let mut cmd_in = LineInput::new();
                    cmd_in.set(&entry.command);
                    let mut desc_in = LineInput::new();
                    desc_in.set(&entry.description);
                    modal.editor = Some(ToolEditor {
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
            KeyCode::Char('d') | KeyCode::Delete if len > 0 => {
                if let Some(entry) = self.tools_vault.tools.get(modal.sel) {
                    modal.confirm_delete = Some(entry.name.clone());
                }
            }
            KeyCode::Enter => {
                let target = modal.target;
                let picked = modal.picked.clone();
                match target {
                    VaultTarget::AddForm => {
                        self.add_tools = picked;
                        self.tool_modal = None;
                    }
                    VaultTarget::Project(pi) => {
                        let current = modal.project_state.clone();
                        let Some(p) = self.cfg.projects.get(pi) else {
                            self.tool_modal = None;
                            return;
                        };
                        match tools::sync_project_tools(
                            &p.path,
                            &self.tools_vault,
                            &picked,
                            &current,
                        ) {
                            Ok((0, 0, 0)) => self.tool_modal = None,
                            Ok((added, updated, removed)) => {
                                self.message = (
                                    format!(
                                        "Updated {} tools (+{added} added, {updated} updated, -{removed} removed)",
                                        p.name
                                    ),
                                    Notice::Info,
                                );
                                self.tool_modal = None;
                            }
                            Err(e) => {
                                self.message = (format!("Tools failed: {e:#}"), Notice::Error);
                            }
                        }
                    }
                    VaultTarget::VaultOnly => self.tool_modal = None,
                }
            }
            _ => {}
        }
    }

    pub(super) fn key_ref_modal(&mut self, k: KeyEvent) {
        let Some(modal) = self.ref_modal.as_mut() else {
            return;
        };
        if let Some(target_name) = modal.confirm_delete.clone() {
            match confirm_key(k.code) {
                Some(true) => {
                    modal.confirm_delete = None;
                    let mut next_vault = self.refs_vault.clone();
                    refs::remove_ref(&mut next_vault, &target_name);
                    match refs::save_refs(&next_vault) {
                        Ok(()) => {
                            self.refs_vault = next_vault;
                            modal.picked.remove(&target_name);
                            self.add_refs.remove(&target_name);
                            let len = self.refs_vault.references.len();
                            modal.sel = modal.sel.min(len.saturating_sub(1));
                        }
                        Err(e) => {
                            self.message = (format!("Save failed: {e:#}"), Notice::Error);
                        }
                    }
                }
                Some(false) => modal.confirm_delete = None,
                None => {}
            }
            return;
        }
        if let Some(ed) = modal.editor.as_mut() {
            match k.code {
                KeyCode::Esc => modal.editor = None,
                KeyCode::Tab | KeyCode::Down => ed.field = (ed.field + 1) % 3,
                KeyCode::BackTab | KeyCode::Up => ed.field = (ed.field + 2) % 3,
                KeyCode::Enter => {
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
                            modal.project_state =
                                refs::read_project_refs(&p.path, &self.refs_vault);
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
                    modal.editor = None;
                }
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
            }
            return;
        }
        let kh = hotkey_event(k);
        let len = self.refs_vault.references.len();
        let can_pick = modal.target != VaultTarget::VaultOnly;
        match kh.code {
            KeyCode::Esc | KeyCode::Char('R') => self.ref_modal = None,
            KeyCode::Up | KeyCode::Char('k') => modal.sel = (modal.sel + len) % (len + 1),
            KeyCode::Down | KeyCode::Char('j') => modal.sel = (modal.sel + 1) % (len + 1),
            KeyCode::Enter | KeyCode::Char(' ') if modal.sel >= len => {
                modal.editor = Some(RefEditor::blank());
            }
            KeyCode::Char(' ') if can_pick && len > 0 => {
                if let Some(entry) = self.refs_vault.references.get(modal.sel) {
                    if !modal.picked.remove(&entry.name) {
                        modal.picked.insert(entry.name.clone());
                    }
                }
            }
            KeyCode::Char('A') if can_pick && len > 0 => {
                toggle_all_picked(
                    &mut modal.picked,
                    self.refs_vault.references.iter().map(|r| &r.name),
                );
            }
            KeyCode::Char('a' | 'n') => modal.editor = Some(RefEditor::blank()),
            KeyCode::Char('e') if len > 0 => {
                if let Some(entry) = self.refs_vault.references.get(modal.sel) {
                    let mut name_in = LineInput::new();
                    name_in.set(&entry.name);
                    let mut url_in = LineInput::new();
                    url_in.set(&entry.url);
                    let mut desc_in = LineInput::new();
                    desc_in.set(&entry.description);
                    modal.editor = Some(RefEditor {
                        name: name_in,
                        url: url_in,
                        desc: desc_in,
                        field: 1,
                        editing_name: Some(entry.name.clone()),
                        err: String::new(),
                    });
                }
            }
            KeyCode::Char('d') | KeyCode::Delete if len > 0 => {
                if let Some(entry) = self.refs_vault.references.get(modal.sel) {
                    modal.confirm_delete = Some(entry.name.clone());
                }
            }
            KeyCode::Enter => {
                let target = modal.target;
                let picked = modal.picked.clone();
                match target {
                    VaultTarget::AddForm => {
                        self.add_refs = picked;
                        self.ref_modal = None;
                    }
                    VaultTarget::Project(pi) => {
                        let current = modal.project_state.clone();
                        let Some(p) = self.cfg.projects.get(pi) else {
                            self.ref_modal = None;
                            return;
                        };
                        match refs::sync_project_refs(&p.path, &self.refs_vault, &picked, &current)
                        {
                            Ok((0, 0, 0)) => self.ref_modal = None,
                            Ok((added, updated, removed)) => {
                                self.message = (
                                    format!(
                                        "Updated {} refs (+{added} added, {updated} updated, -{removed} removed)",
                                        p.name
                                    ),
                                    Notice::Info,
                                );
                                self.ref_modal = None;
                            }
                            Err(e) => {
                                self.message = (format!("Refs failed: {e:#}"), Notice::Error);
                            }
                        }
                    }
                    VaultTarget::VaultOnly => self.ref_modal = None,
                }
            }
            _ => {}
        }
    }

    /// `b`: type the ledger brief into the selected agent. A person presses
    /// it when the agent is at its prompt; squad never types it on its own,
    /// where it could answer a trust or permission dialog by accident.
    pub(super) fn brief_selected(&mut self) {
        let Some(&pi) = self.order.get(self.selected) else {
            return;
        };
        ledger::write_rosters(&self.cfg.projects);
        let Some(p) = self.cfg.projects.get(pi) else {
            return;
        };
        self.message = if !p.has_ledger() {
            (
                format!(
                    "Operations log is off for {}: set ledger = true in its config entry",
                    p.name
                ),
                Notice::Info,
            )
        } else {
            match self
                .sessions
                .get_mut(pi)
                .and_then(|o| o.as_mut())
                .filter(|s| s.status.is_running())
            {
                None => (
                    format!("Start {} first, then press b to brief it", p.name),
                    Notice::Info,
                ),
                Some(s) => {
                    let projects = &self.cfg.projects;
                    let commander = superior(projects, pi).map(|c| projects[c].name.as_str());
                    let subs: Vec<&str> = subordinates(projects, pi)
                        .into_iter()
                        .map(|j| projects[j].name.as_str())
                        .collect();
                    let mut line = ledger::brief_line(&p.name, commander, &subs);
                    if p.path.join(ledger::DIR).join("TOOLS.md").is_file() {
                        line.push_str(
                            " Available project tools are listed in .squad/TOOLS.md: read it before running commands.",
                        );
                    }
                    let mut bytes = line.into_bytes();
                    bytes.push(b'\r');
                    match s.input(&bytes) {
                        Ok(()) => {
                            s.log_event("brief");
                            (format!("{} briefed.", p.name), Notice::Info)
                        }
                        Err(e) => (e.to_string(), Notice::Error),
                    }
                }
            }
        };
    }

    /// Copy the selected pane text to the clipboard and say so in the status bar.
    pub(super) fn copy_selection(&mut self) {
        let Some(sel) = self.sel else { return };
        let ((r1, c1), (r2, c2)) = sel.span();
        let Some(Some(s)) = self.sessions.get(sel.pi) else {
            return;
        };
        let text = s.parser.screen().contents_between(r1, c1, r2, c2 + 1);
        let text = text.trim_end();
        self.message = if text.is_empty() {
            ("Nothing to copy".to_string(), Notice::Warn)
        } else {
            match set_clipboard(text) {
                Ok(()) => (
                    format!("Copied {} characters", text.chars().count()),
                    Notice::Info,
                ),
                Err(e) => (format!("Copy failed: {e}"), Notice::Error),
            }
        };
        self.dirty = true;
    }

    pub(crate) fn waiting_projects(&self) -> Vec<usize> {
        self.order
            .iter()
            .copied()
            .filter(|&pi| {
                self.cfg
                    .projects
                    .get(pi)
                    .is_some_and(|p| p.kind == Kind::Agent)
                    && self
                        .sessions
                        .get(pi)
                        .and_then(|o| o.as_ref())
                        .is_some_and(|s| {
                            s.status.is_running() && s.phase() == AgentPhase::NeedsInput
                        })
            })
            .collect()
    }

    /// `(name, question)` for agents waiting on input, in sidebar order,
    /// for the sidebar's bottom strip.
    pub(crate) fn waiting_list(&self) -> Vec<(String, String)> {
        self.waiting_projects()
            .into_iter()
            .filter_map(|pi| {
                let p = self.cfg.projects.get(pi)?;
                let q = self
                    .sessions
                    .get(pi)
                    .and_then(|o| o.as_ref())
                    .and_then(|s| s.prompt_hint.clone())
                    .unwrap_or_else(|| "Awaiting orders".to_string());
                Some((p.name.clone(), q))
            })
            .collect()
    }

    pub(crate) fn open_quick_reply(&mut self) {
        let waiting = self.waiting_projects();
        if waiting.is_empty() {
            self.message = ("No agent is waiting for input.".to_string(), Notice::Info);
            return;
        }
        let sel_pi = self.order.get(self.selected).copied();
        let pi = sel_pi.filter(|p| waiting.contains(p)).unwrap_or(waiting[0]);
        if let Some(pos) = self.order.iter().position(|&x| x == pi) {
            self.selected = pos;
        }
        let tail = self
            .sessions
            .get_mut(pi)
            .and_then(|o| o.as_mut())
            .map_or_else(Vec::new, |s| s.tail_lines_snapshot(10));
        self.quick_reply = Some(QuickReply {
            pi,
            tail,
            typing: false,
            input: LineInput::new(),
        });
        self.dirty = true;
    }

    pub(crate) fn send_quick_reply(&mut self, pi: usize, bytes: &[u8]) {
        let name = self
            .cfg
            .projects
            .get(pi)
            .map_or("?", |p| p.name.as_str())
            .to_string();
        let sent = if let Some(Some(s)) = self.sessions.get_mut(pi) {
            s.scroll_live();
            match s.input(bytes) {
                Ok(()) => {
                    s.refresh_screen_flags();
                    self.message = (format!("Replied to {name}"), Notice::Info);
                    true
                }
                Err(e) => {
                    self.message = (e.to_string(), Notice::Error);
                    false
                }
            }
        } else {
            false
        };
        if sent {
            if let Some(next_pi) = self.waiting_projects().into_iter().find(|&w| w != pi) {
                if let Some(pos) = self.order.iter().position(|&x| x == next_pi) {
                    self.selected = pos;
                }
                let tail = self.sessions[next_pi]
                    .as_mut()
                    .map_or_else(Vec::new, |s| s.tail_lines_snapshot(10));
                self.quick_reply = Some(QuickReply {
                    pi: next_pi,
                    tail,
                    typing: false,
                    input: LineInput::new(),
                });
            } else {
                self.quick_reply = None;
            }
        } else {
            self.quick_reply = None;
        }
        self.dirty = true;
    }

    pub(super) fn key_quick_reply(&mut self, k: KeyEvent) {
        let Some(qr) = self.quick_reply.as_mut() else {
            return;
        };
        self.dirty = true;
        if qr.typing {
            match k.code {
                KeyCode::Esc => {
                    qr.typing = false;
                }
                KeyCode::Enter => {
                    let pi = qr.pi;
                    let text = qr.input.take();
                    let mut bytes = text.into_bytes();
                    bytes.push(b'\r');
                    self.send_quick_reply(pi, &bytes);
                }
                _ => {
                    qr.input.edit_key(k);
                }
            }
            return;
        }
        let pi = qr.pi;
        let kh = hotkey_event(k);
        match kh.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('w') => {
                self.quick_reply = None;
            }
            KeyCode::Tab => {
                let waiting = self.waiting_projects();
                if let Some(cur) = waiting.iter().position(|&w| w == pi) {
                    let next_pi = waiting[(cur + 1) % waiting.len()];
                    if let Some(pos) = self.order.iter().position(|&x| x == next_pi) {
                        self.selected = pos;
                    }
                    let tail = self.sessions[next_pi]
                        .as_mut()
                        .map_or_else(Vec::new, |s| s.tail_lines_snapshot(10));
                    if let Some(q) = self.quick_reply.as_mut() {
                        q.pi = next_pi;
                        q.tail = tail;
                        q.input.clear();
                    }
                }
            }
            KeyCode::Char('t' | 'i' | '/') => {
                qr.typing = true;
            }
            KeyCode::Char('o' | 'f') => {
                self.quick_reply = None;
                if let Some(pos) = self.order.iter().position(|&x| x == pi) {
                    self.selected = pos;
                }
                self.mode = Mode::Focus;
            }
            KeyCode::Enter => {
                self.send_quick_reply(pi, b"\r");
            }
            KeyCode::Char('c') => {
                self.send_quick_reply(pi, b"\x1b");
            }
            KeyCode::Char(c @ ('y' | 'Y' | 'n' | 'N' | '1'..='9')) => {
                let b = [c.to_ascii_lowercase() as u8, b'\r'];
                self.send_quick_reply(pi, &b);
            }
            _ => {}
        }
    }

    pub(super) fn open_git_view(&mut self) {
        let Some(&pi) = self.order.get(self.selected) else {
            return;
        };
        let Some(p) = self.cfg.projects.get(pi) else {
            return;
        };
        if !p.path.is_dir() {
            self.message = (format!("Folder of {} is missing", p.name), Notice::Warn);
            return;
        }
        let Some(branch) = ledger::repo_branch(&p.path) else {
            self.message = (format!("{} is not a git repository", p.name), Notice::Info);
            return;
        };
        self.git_view = Some(GitView::open(pi, &p.name, &p.path, branch));
        self.dirty = true;
    }

    pub(super) fn key_git_view(&mut self, k: KeyEvent) {
        let Some(cur_pi) = self.git_view.as_ref().map(|g| g.pi) else {
            return;
        };
        if k.code == KeyCode::Tab && !self.order.is_empty() {
            let n = self.order.len();
            let start = self
                .order
                .iter()
                .position(|&x| x == cur_pi)
                .unwrap_or(self.selected);
            for off in 1..=n {
                let pos = (start + off) % n;
                let pi = self.order[pos];
                if let Some(p) = self.cfg.projects.get(pi) {
                    if p.path.is_dir() {
                        if let Some(branch) = ledger::repo_branch(&p.path) {
                            self.selected = pos;
                            self.git_view = Some(GitView::open(pi, &p.name, &p.path, branch));
                            self.dirty = true;
                            return;
                        }
                    }
                }
            }
            return;
        }
        let Some(g) = self.git_view.as_mut() else {
            return;
        };
        match hotkey_event(k).code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('g') => self.git_view = None,
            KeyCode::Down | KeyCode::Char('j') => g.select(g.sel + 1),
            KeyCode::Up | KeyCode::Char('k') => g.select(g.sel.saturating_sub(1)),
            KeyCode::Char('J') | KeyCode::Char(']') => g.scroll_by(1),
            KeyCode::Char('K') | KeyCode::Char('[') => g.scroll_by(-1),
            KeyCode::PageDown | KeyCode::Char(' ') => g.scroll_by(10),
            KeyCode::PageUp => g.scroll_by(-10),
            KeyCode::Home => g.select(0),
            KeyCode::End => g.select(usize::MAX),
            KeyCode::Char('r') => {
                g.refresh();
                self.message = ("Git status refreshed.".to_string(), Notice::Info);
            }
            _ => {}
        }
        self.dirty = true;
    }

    pub(super) fn open_search(&mut self) {
        let Some(&pi) = self.order.get(self.selected) else {
            return;
        };
        if !self.sessions.get(pi).is_some_and(|o| o.is_some()) {
            self.message = ("No session output to search yet.".to_string(), Notice::Info);
            return;
        }
        if self.search_pi != Some(pi) {
            self.search_buf.clear();
            self.search_pi = Some(pi);
        }
        self.mode = Mode::Search;
        self.sync_search(true);
    }

    pub(super) fn sync_search(&mut self, jump_newest: bool) {
        let Some(&pi) = self.order.get(self.selected) else {
            self.clear_search();
            return;
        };
        let query = self.search_buf.as_str();
        if query.is_empty() {
            self.search_matches.clear();
            self.search_idx = 0;
            self.search_active = false;
            return;
        }
        let Some(Some(s)) = self.sessions.get_mut(pi) else {
            self.clear_search();
            return;
        };
        self.search_pi = Some(pi);
        self.search_matches = s.search_scrollback(&query);
        self.search_active = true;
        if self.search_matches.is_empty() {
            self.search_idx = 0;
        } else {
            self.search_idx = if jump_newest {
                self.search_matches.len() - 1
            } else {
                self.search_idx.min(self.search_matches.len() - 1)
            };
            let line = self.search_matches[self.search_idx].line;
            s.scroll_to_logical_line(line);
        }
    }

    pub(super) fn step_search(&mut self, older: bool) {
        let Some(&pi) = self.order.get(self.selected) else {
            return;
        };
        let query = self.search_buf.as_str();
        if query.is_empty() {
            return;
        }
        let Some(Some(s)) = self.sessions.get_mut(pi) else {
            return;
        };
        self.search_pi = Some(pi);
        self.search_matches = s.search_scrollback(&query);
        self.search_active = true;
        let len = self.search_matches.len();
        if len == 0 {
            self.search_idx = 0;
            return;
        }
        self.search_idx = if older {
            if self.search_idx == 0 || self.search_idx >= len {
                len - 1
            } else {
                self.search_idx - 1
            }
        } else {
            (self.search_idx + 1) % len
        };
        let line = self.search_matches[self.search_idx].line;
        s.scroll_to_logical_line(line);
    }

    pub(super) fn clear_search(&mut self) {
        self.search_buf.clear();
        self.search_matches.clear();
        self.search_idx = 0;
        self.search_active = false;
        self.search_pi = None;
    }
}

fn toggle_all_picked<'a>(picked: &mut BTreeSet<String>, names: impl Iterator<Item = &'a String>) {
    let all: Vec<String> = names.cloned().collect();
    if all.iter().all(|n| picked.contains(n)) {
        picked.clear();
    } else {
        picked.extend(all);
    }
}
