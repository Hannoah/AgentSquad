//! Keyboard input for the manager, terminal focus and the text modes.

use super::*;
use std::time::{Duration, Instant};

impl App {
    pub(crate) fn on_key(&mut self, k: KeyEvent, rows: u16, cols: u16) {
        self.dirty = true;
        let k = normalize_ctrl_alt_key(k);
        if self.wants_ascii_ime()
            && matches!(k.code, KeyCode::Char(c) if hangul_to_qwerty(c).is_some())
        {
            self.hangul_hotkey_seen = true;
        }
        // Any key drops the highlight. Ctrl+C on a selection copies it and is
        // not sent on; with nothing selected it reaches the session as before.
        if self.sel.is_some_and(|s| s.moved)
            && k.code == KeyCode::Char('c')
            && k.modifiers.contains(KeyModifiers::CONTROL)
        {
            self.copy_selection();
            self.sel = None;
            return;
        }
        self.sel = None;
        if self.env_modal.is_some() {
            self.key_env_modal(k);
            return;
        }
        if self.skill_modal.is_some() {
            self.key_skill_modal(k);
            return;
        }
        if self.tool_modal.is_some() {
            self.key_tool_modal(k);
            return;
        }
        if self.ref_modal.is_some() {
            self.key_ref_modal(k);
            return;
        }
        if self.quick_reply.is_some() {
            self.key_quick_reply(k);
            return;
        }
        match self.mode {
            Mode::Focus => self.key_focus(k, rows),
            Mode::Send => self.key_send(k, rows, cols),
            Mode::Filter => self.key_filter(k),
            Mode::Search => self.key_search(k),
            Mode::Add => self.key_add(k),
            Mode::Help => self.key_help(k),
            Mode::Manager => self.key_manager_or_confirm(k, rows, cols),
        }
    }

    /// Manager keys, or the answer to whichever confirmation is open.
    pub(super) fn key_manager_or_confirm(&mut self, k: KeyEvent, rows: u16, cols: u16) {
        let k = hotkey_event(k);
        if self.context_menu.is_some() {
            self.key_context_menu(k, rows, cols);
        } else if self.env_info.is_some() {
            self.key_env_info(k);
        } else if self.history.is_some() {
            self.key_history(k);
        } else if self.gsearch.is_some() {
            self.key_gsearch(k);
        } else if self.git_view.is_some() {
            self.key_git_view(k);
        } else if self.procs.is_some() {
            self.key_procs(k);
        } else if self.confirm_update {
            self.key_confirm_update(k);
        } else if self.confirm_apply {
            self.key_confirm_apply(k);
        } else if self.confirm_quit {
            self.key_confirm_quit(k);
        } else if self.confirm_start_all {
            self.key_confirm_start_all(k, rows, cols);
        } else if self.confirm_stop.is_some() {
            self.key_confirm_stop(k);
        } else if self.confirm_restart.is_some() {
            self.key_confirm_restart(k, rows, cols);
        } else if self.confirm_remove.is_some() {
            self.key_confirm_remove(k);
        } else {
            self.key_manager(k, rows, cols);
        }
    }

    pub(super) fn key_focus(&mut self, k: KeyEvent, rows: u16) {
        if focus_exit_key(k.code, k.modifiers) {
            self.mode = Mode::Manager;
            return;
        }
        if let Some(i) = focus_switch_key(k.code, k.modifiers, self.order.len()) {
            self.selected = i;
            // A stopped project has nothing to type into: land on the manager.
            // One that is starting keeps the keys typed until it is up.
            if !self
                .order
                .get(i)
                .is_some_and(|&pi| slot_running(&self.sessions, pi) || self.is_starting(pi))
            {
                self.mode = Mode::Manager;
            }
            return;
        }
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        match k.code {
            KeyCode::PageUp if shift => {
                scroll_selected(
                    &mut self.sessions,
                    &self.order,
                    self.selected,
                    rows as usize,
                    1,
                );
                return;
            }
            KeyCode::PageDown if shift => {
                scroll_selected(
                    &mut self.sessions,
                    &self.order,
                    self.selected,
                    rows as usize,
                    -1,
                );
                return;
            }
            KeyCode::End if shift => {
                scroll_live_selected(&mut self.sessions, &self.order, self.selected);
                return;
            }
            _ => {}
        }
        if let Some(&pi) = self.order.get(self.selected) {
            if let Some(Some(s)) = self.sessions.get_mut(pi) {
                s.scroll_live();
                let bytes = key_bytes(k, s.parser.screen().application_cursor());
                if let Err(e) = s.input(&bytes) {
                    self.message = (e.to_string(), Notice::Error);
                    self.mode = Mode::Manager;
                }
            } else if let Some(typed) = self.starting.get_mut(&self.cfg.projects[pi].name) {
                // Still starting: keep the keys for it instead of dropping them.
                typed.extend(key_bytes(k, false));
            }
        }
    }

    pub(super) fn key_send(&mut self, k: KeyEvent, rows: u16, cols: u16) {
        match k.code {
            KeyCode::Esc => {
                self.mode = Mode::Manager;
                self.send_buf.clear();
            }
            KeyCode::Tab if self.send_to == SendTo::Under => {
                if let Some(&pi) = self.order.get(self.selected) {
                    if let Some(next) =
                        complete_agent_name(&self.cfg.projects, &self.send_buf.as_str(), Some(pi))
                    {
                        self.send_buf.set(&next);
                    }
                }
            }
            KeyCode::Enter => {
                let line = self.send_buf.take();
                self.mode = Mode::Manager;
                if self.send_to == SendTo::Under {
                    if let Some(&pi) = self.order.get(self.selected) {
                        self.message = self.set_superior(pi, line.trim());
                    }
                } else if line.is_empty() {
                    self.message = ("Empty. Nothing sent.".to_string(), Notice::Info);
                } else if let (SendTo::All, Some(&pi)) =
                    (self.send_to, self.order.get(self.selected))
                {
                    self.message = self.issue_order(pi, &line);
                } else if let Some(&pi) = self.order.get(self.selected) {
                    let name = self.cfg.projects[pi].name.clone();
                    let mut bytes = line.into_bytes();
                    // CR is Enter; LF reads as Shift+Enter (newline) to agents.
                    bytes.push(b'\r');
                    let live = self
                        .sessions
                        .get_mut(pi)
                        .and_then(|o| o.as_mut())
                        .filter(|s| s.status.is_running());
                    self.message = match live {
                        Some(s) => match s.input(&bytes) {
                            Ok(()) => (format!("Sent to {name}"), Notice::Info),
                            Err(e) => (e.to_string(), Notice::Error),
                        },
                        None => {
                            self.begin_start(pi, rows, cols, bytes);
                            (format!("Starting {name}; the line follows"), Notice::Info)
                        }
                    };
                }
            }
            _ => {
                self.send_buf.edit_key(k);
            }
        }
    }

    pub(super) fn key_filter(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Esc => {
                self.mode = Mode::Manager;
                self.clear_filter();
            }
            KeyCode::Enter => self.mode = Mode::Manager,
            KeyCode::Down if !self.order.is_empty() => {
                self.selected = (self.selected + 1) % self.order.len();
            }
            KeyCode::Up if !self.order.is_empty() => {
                self.selected = (self.selected + self.order.len() - 1) % self.order.len();
            }
            _ => {
                if self.filter_buf.edit_key(k) == Some(true) {
                    self.refresh_filter();
                    self.selected = 0;
                }
            }
        }
    }

    pub(super) fn key_search(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        match k.code {
            KeyCode::Esc => {
                self.mode = Mode::Manager;
                self.clear_search();
            }
            KeyCode::Enter => {
                self.mode = Mode::Manager;
                if self.search_buf.is_empty() {
                    self.clear_search();
                }
            }
            KeyCode::Up => self.step_search(true),
            KeyCode::Down => self.step_search(false),
            KeyCode::F(3) => self.step_search(shift),
            KeyCode::Char('p') if ctrl => self.step_search(true),
            KeyCode::Char('n') if ctrl => self.step_search(false),
            _ => {
                if self.search_buf.edit_key(k) == Some(true) {
                    self.sync_search(true);
                }
            }
        }
    }

    /// The text input behind the active add-form field (path, command or
    /// commander); the toggle fields have none.
    pub(super) fn add_input(&mut self) -> Option<&mut LineInput> {
        match self.add_field {
            0 => Some(&mut self.add_path),
            2 => Some(&mut self.add_cmd),
            4 => Some(&mut self.add_under),
            _ => None,
        }
    }

    /// The Path field changed: re-check it, relist the folders under it and
    /// drop stale errors.
    pub(super) fn path_edited(&mut self) {
        // No shell here: expand a leading `~` in the field itself, or a new
        // folder would be made as a literal `~` under the current directory.
        let home = if cfg!(windows) {
            std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))
        } else {
            std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
        };
        if let Some(full) =
            home.and_then(|h| pathpick::expand_home(&self.add_path.as_str(), &h.to_string_lossy()))
        {
            self.add_path.set(&full);
        }
        let text = self.add_path.as_str();
        self.add_picks = if text.is_empty() {
            pathpick::roots(&self.cfg.projects)
        } else {
            pathpick::suggest(&text)
        };
        self.add_sel = None;
        self.add_top.set(0);
        self.add_state = pathpick::check(&text);
        self.add_err.clear();
    }

    /// A text field of the form changed.
    pub(super) fn add_edited(&mut self) {
        if self.add_field == 0 {
            self.path_edited();
        } else {
            self.add_err.clear();
        }
    }

    fn set_path(&mut self, text: &str) {
        self.add_path.set(text);
        self.path_edited();
    }

    /// Fill the Path field with list row `i`; its subfolders then list, so
    /// repeating this walks down the tree.
    pub(super) fn pick_path(&mut self, i: usize) {
        if let Some((_, text)) = self.add_picks.get(i) {
            let text = text.clone();
            self.set_path(&text);
        }
    }

    /// Tab in the Path field: take the highlighted folder; otherwise an
    /// empty field takes the first suggested parent and a typed one completes
    /// its last segment against the disk.
    fn complete_path(&mut self) {
        if let Some(i) = self.add_sel {
            return self.pick_path(i);
        }
        if self.add_path.is_empty() {
            return self.pick_path(0);
        }
        match pathpick::complete(&self.add_path.as_str()) {
            Some(text) => self.set_path(&text),
            None => self.add_err = "No folder matches".into(),
        }
    }

    /// Down/Up in the Path field step through the folder list; past either
    /// end they move to the neighbouring field.
    pub(super) fn step_path_list(&mut self, down: bool) {
        let n = self.add_picks.len();
        match (down, self.add_sel) {
            (true, None) if n > 0 => self.add_sel = Some(0),
            (true, Some(i)) if i + 1 < n => self.add_sel = Some(i + 1),
            (false, Some(0)) => self.add_sel = None,
            (false, Some(i)) => self.add_sel = Some(i - 1),
            (true, _) => self.add_field = if self.add_edit.is_some() { 2 } else { 1 },
            (false, None) => {
                self.add_field = 8;
            }
        }
    }

    /// Mouse wheel in the Path field steps through the folder list without
    /// jumping to another field at the ends.
    pub(super) fn wheel_path_list(&mut self, down: bool) {
        let n = self.add_picks.len();
        if n == 0 {
            return;
        }
        match (down, self.add_sel) {
            (true, None) => self.add_sel = Some(0),
            (true, Some(i)) => self.add_sel = Some((i + 1).min(n - 1)),
            (false, Some(0) | None) => self.add_sel = None,
            (false, Some(i)) => self.add_sel = Some(i - 1),
        }
    }

    pub(super) fn key_add(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Esc => {
                self.add_edit = None;
                self.mode = Mode::Manager;
            }
            // A highlighted folder fills the path instead of saving.
            KeyCode::Enter if self.add_field == 0 && self.add_sel.is_some() => {
                self.pick_path(self.add_sel.unwrap_or(0));
            }
            KeyCode::Enter | KeyCode::Char(' ') if self.add_field == 5 => {
                self.open_env_modal(VaultTarget::AddForm);
            }
            KeyCode::Enter | KeyCode::Char(' ') if self.add_field == 6 => {
                self.open_skill_modal(VaultTarget::AddForm);
            }
            KeyCode::Enter | KeyCode::Char(' ') if self.add_field == 7 => {
                self.open_tool_modal(VaultTarget::AddForm);
            }
            KeyCode::Enter | KeyCode::Char(' ') if self.add_field == 8 => {
                self.open_ref_modal(VaultTarget::AddForm);
            }
            KeyCode::Enter if self.add_edit.is_some() => self.save_edit(),
            KeyCode::Tab if self.add_edit.is_some() && self.add_field == 0 => self.complete_path(),
            KeyCode::Up if self.add_edit.is_some() && self.add_field == 0 => {
                self.step_path_list(false)
            }
            KeyCode::Down if self.add_edit.is_some() && self.add_field == 0 => {
                self.step_path_list(true)
            }
            // Editing reaches Path, Command, Autostart, Env (.env), Skills, Tools, and Refs.
            KeyCode::Tab | KeyCode::Down if self.add_edit.is_some() => {
                self.add_field = match self.add_field {
                    0 => 2,
                    2 => 3,
                    3 => 5,
                    5 => 6,
                    6 => 7,
                    7 => 8,
                    _ => 0,
                };
            }
            KeyCode::BackTab | KeyCode::Up if self.add_edit.is_some() => {
                self.add_field = match self.add_field {
                    8 => 7,
                    7 => 6,
                    6 => 5,
                    5 => 3,
                    3 => 2,
                    2 => 0,
                    _ => 8,
                };
            }
            KeyCode::Enter => {
                self.add_err.clear();
                match load() {
                    Err(e) => self.add_err = format!("Config reload failed: {e:#}"),
                    Ok(disk) => {
                        merge_projects(&mut self.cfg, &mut self.sessions, disk);
                        self.refresh_prefixes();
                        self.refresh_filter();
                    }
                }
                // A missing folder is made on the spot (the field says so).
                if self.add_err.is_empty() && self.add_state == PathState::Missing {
                    if let Err(e) = create_missing_dir(&self.add_path.as_str()) {
                        self.add_err = format!("{e:#}");
                    }
                }
                if self.add_err.is_empty() {
                    match build_project(
                        &self.cfg.projects,
                        &self.add_path.as_str(),
                        self.add_kind,
                        split_command(&self.add_cmd.as_str()),
                    ) {
                        Err(e) => self.add_err = e,
                        Ok(mut p) => {
                            p.autostart = self.add_autostart;
                            if p.kind == Kind::Agent {
                                let under = self.add_under.as_str();
                                let under = under.trim();
                                if !under.is_empty() {
                                    match check_superior(&self.cfg.projects, p.kind, under) {
                                        Ok(c) => p.reports_to = Some(c),
                                        Err(e) => {
                                            self.add_err = e;
                                            return;
                                        }
                                    }
                                }
                            }
                            let name = p.name.clone();
                            let kind_label = p.kind.as_str();
                            let proj_path = p.path.clone();
                            self.cfg.projects.push(p);
                            self.sessions.push(None);
                            match persist_add(&self.cfg.projects[self.cfg.projects.len() - 1]) {
                                Err(e) => {
                                    self.cfg.projects.pop();
                                    self.sessions.pop();
                                    self.refresh_prefixes();
                                    self.refresh_filter();
                                    self.add_err = format!("Save failed: {e:#}");
                                }
                                Ok(()) => {
                                    self.cfg.projects.last().map(ledger::ensure_agent);
                                    // A new project must be visible, so drop any filter.
                                    self.filter_buf.clear();
                                    self.refresh_prefixes();
                                    self.order = display_order(&self.cfg.projects);
                                    self.selected = self
                                        .order
                                        .iter()
                                        .position(|&x| x == self.cfg.projects.len() - 1)
                                        .unwrap_or(0);
                                    self.mode = Mode::Manager;
                                    let last = self.cfg.projects.len() - 1;
                                    enable_chain_ledger(&mut self.cfg.projects, last);
                                    ledger::report_transfer(&self.cfg.projects, &name, None);
                                    ledger::write_rosters(&self.cfg.projects);
                                    let mut cap_notes = Vec::new();
                                    let mut notice = Notice::Info;
                                    let skills_res = self.apply_add_skills(&proj_path);
                                    let skills_kept = skills_res
                                        .as_ref()
                                        .ok()
                                        .and_then(|o| o.as_ref().map(|r| r.2.clone()))
                                        .unwrap_or_default();
                                    for (label, res) in [
                                        (".env", self.apply_add_env(&proj_path)),
                                        ("skills", skills_res.map(|o| o.map(|(a, u, _)| (a, u)))),
                                        ("tools", self.apply_add_tools(&proj_path)),
                                        ("refs", self.apply_add_refs(&proj_path)),
                                    ] {
                                        match res {
                                            Ok(Some((added, updated))) => {
                                                cap_notes
                                                    .push(format!("{label}: +{added}, ~{updated}"));
                                            }
                                            Ok(None) => {}
                                            Err(e) => {
                                                cap_notes.push(e);
                                                notice = Notice::Warn;
                                            }
                                        }
                                    }
                                    if !skills_kept.is_empty() {
                                        // The folder already had its own skills by these names.
                                        cap_notes.push(format!(
                                            "kept own skills {} (K overwrites after asking)",
                                            skills_kept.join(", ")
                                        ));
                                        notice = Notice::Warn;
                                    }
                                    let cap_note = if cap_notes.is_empty() {
                                        String::new()
                                    } else {
                                        format!(" ({})", cap_notes.join("; "))
                                    };
                                    self.add_env.clear();
                                    self.add_skills.clear();
                                    self.add_tools.clear();
                                    self.add_refs.clear();
                                    self.message = (
                                        format!("Registered {name} ({kind_label}){cap_note}"),
                                        notice,
                                    );
                                }
                            }
                        }
                    }
                }
            }
            KeyCode::Tab if self.add_field == 0 => self.complete_path(),
            KeyCode::Tab if self.add_field == 4 => {
                if let Some(next) =
                    complete_agent_name(&self.cfg.projects, &self.add_under.as_str(), None)
                {
                    self.add_under.set(&next);
                    self.add_err.clear();
                }
            }
            KeyCode::Tab => {
                self.add_field = next_add_field(self.add_field, self.add_kind, true);
            }
            KeyCode::Up if self.add_field == 0 => self.step_path_list(false),
            KeyCode::Down if self.add_field == 0 => self.step_path_list(true),
            KeyCode::PageDown if self.add_field == 0 && !self.add_picks.is_empty() => {
                let last = self.add_picks.len() - 1;
                self.add_sel = Some(self.add_sel.map_or(0, |i| (i + 6).min(last)));
            }
            KeyCode::PageUp if self.add_field == 0 && !self.add_picks.is_empty() => {
                self.add_sel = match self.add_sel {
                    Some(0) | None => None,
                    Some(i) => Some(i.saturating_sub(6)),
                };
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.add_field = next_add_field(self.add_field, self.add_kind, false);
            }
            KeyCode::Down => {
                self.add_field = next_add_field(self.add_field, self.add_kind, true);
            }
            // The toggle fields flip on Space or a sideways arrow.
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') if self.add_field == 1 => {
                self.add_kind = self.add_kind.toggled();
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') if self.add_field == 3 => {
                self.add_autostart = !self.add_autostart;
            }
            _ => {
                if self.add_input().and_then(|input| input.edit_key(k)) == Some(true) {
                    self.add_edited();
                }
            }
        }
    }

    pub(super) fn key_help(&mut self, k: KeyEvent) {
        match hotkey_event(k).code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?') => self.mode = Mode::Manager,
            _ => {}
        }
    }

    pub(super) fn key_manager(&mut self, k: KeyEvent, rows: u16, cols: u16) {
        // A pending multi-digit jump only survives more digits.
        if !matches!(k.code, KeyCode::Char(c) if c.is_ascii_digit()) {
            self.jump_buf.clear();
        }
        const MGMT_LAST: usize = 4;
        if let Some(m) = self.mgmt_sel {
            match k.code {
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('n') => {
                    self.mgmt_sel = (m < MGMT_LAST).then_some(m + 1);
                    if self.mgmt_sel.is_none() && self.order.is_empty() {
                        self.mgmt_sel = Some(0);
                    }
                    self.selected = 0;
                }
                KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('p') => {
                    self.mgmt_sel = m.checked_sub(1);
                    if self.mgmt_sel.is_none() && self.order.is_empty() {
                        self.mgmt_sel = Some(MGMT_LAST);
                    }
                    self.selected = self.order.len().saturating_sub(1);
                }
                KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.open_mgmt(m),
                KeyCode::Esc => self.mgmt_sel = None,
                // No project is highlighted, so project actions have no target.
                KeyCode::Char(
                    'm' | 'u' | 'o' | 'b' | 'h' | 'g' | 'e' | 'f' | 'r' | 's' | 'x' | 'd',
                )
                | KeyCode::Delete => {}
                // Anything else (global keys, jumps) returns to the project list.
                _ => {
                    self.mgmt_sel = None;
                    self.key_manager(k, rows, cols);
                }
            }
            return;
        }
        match k.code {
            KeyCode::Char('q') | KeyCode::Char('c')
                if k.code == KeyCode::Char('q') || k.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                if self.updating {
                    // Quitting mid-swap could leave no squad.exe behind.
                    self.message = (
                        "Update in progress; quit once it finishes.".to_string(),
                        Notice::Warn,
                    );
                } else if live_session_count(&self.sessions) > 0 {
                    self.confirm_quit = true;
                } else {
                    self.quit = true;
                }
            }
            // `n`/`p` stay bound for muscle memory; `j`/`k` and arrows are documented.
            // Past either end of the list the arrows step onto Management.
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('n') => {
                if self.selected + 1 < self.order.len() {
                    self.selected += 1;
                } else {
                    self.mgmt_sel = Some(0);
                }
            }
            KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('p') => {
                if self.selected > 0 && !self.order.is_empty() {
                    self.selected -= 1;
                } else {
                    self.mgmt_sel = Some(MGMT_LAST);
                }
            }
            KeyCode::Tab if !self.order.is_empty() => self.jump_attention(),
            KeyCode::Char('m') if !self.order.is_empty() => {
                self.mode = Mode::Send;
                self.send_to = SendTo::One;
                self.send_buf.clear();
            }
            // Change whom the selected agent reports to (empty clears it).
            KeyCode::Char('u') if !self.order.is_empty() => {
                self.begin_set_superior(self.order[self.selected]);
            }
            // Orders: one line to every agent under the selected commander.
            KeyCode::Char('o') if !self.order.is_empty() => {
                let pi = self.order[self.selected];
                if all_subordinates(&self.cfg.projects, pi).is_empty() {
                    self.message = (
                        format!("{} has no agents under it", self.cfg.projects[pi].name),
                        Notice::Info,
                    );
                } else {
                    self.mode = Mode::Send;
                    self.send_to = SendTo::All;
                    self.send_buf.clear();
                }
            }
            KeyCode::Char('b') if !self.order.is_empty() => self.brief_selected(),
            KeyCode::Char('h') if !self.order.is_empty() => self.open_history(),
            KeyCode::Char('g') if !self.order.is_empty() => self.open_git_view(),
            KeyCode::Char('w') => self.open_quick_reply(),
            KeyCode::Char('M') => self.open_procs(),
            KeyCode::Char('E') => self.open_env_modal(self.selected_vault_target()),
            KeyCode::Char('K') => self.open_skill_modal(self.selected_vault_target()),
            KeyCode::Char('T') => self.open_tool_modal(self.selected_vault_target()),
            KeyCode::Char('R') => self.open_ref_modal(self.selected_vault_target()),
            KeyCode::Char('e') if !self.order.is_empty() => self.open_edit(),
            KeyCode::Char('f' | 'F') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                self.open_search();
            }
            KeyCode::Char('F') => self.open_search(),
            KeyCode::Char('G') => self.open_gsearch(),
            KeyCode::F(3) if self.search_active => {
                self.step_search(k.modifiers.contains(KeyModifiers::SHIFT));
            }
            KeyCode::Char('f') if !self.order.is_empty() => self.toggle_pin(),
            KeyCode::Char('z') => {
                self.zoom = !self.zoom;
                let note = if self.zoom {
                    "Zoom on: a focused terminal takes the full width (z turns it off)"
                } else {
                    "Zoom off"
                };
                self.message = (note.to_string(), Notice::Info);
            }
            KeyCode::Char('[' | ']' | '<' | '>') => self.toggle_sidebar(),
            KeyCode::Char('?') => self.mode = Mode::Help,
            // Open the update prompt on demand; it also pops up by
            // itself whenever the manager has the keyboard.
            KeyCode::Char('U') if self.update_offer.is_some() && !self.updating => {
                self.confirm_update = true;
            }
            KeyCode::Char('a') => self.open_add(),
            KeyCode::Char(c) if c.is_ascii_digit() => {
                if self.jump_at.elapsed() > Duration::from_millis(1500) {
                    self.jump_buf.clear();
                }
                self.jump_buf.push(c);
                self.jump_at = Instant::now();
                let buf = self.jump_buf.clone();
                match jump_number(self.order.len(), &buf) {
                    Some(i) => {
                        self.selected = i;
                        // A single digit that could start a longer row
                        // number (`1` of `12`) stays pending briefly.
                        let keep = buf.len() == 1
                            && buf
                                .parse::<usize>()
                                .is_ok_and(|n| n.saturating_mul(10) <= self.order.len());
                        if keep {
                            self.message = (
                                format!("Jump {buf}… (1–{})", self.order.len()),
                                Notice::Info,
                            );
                        } else {
                            self.jump_buf.clear();
                        }
                    }
                    None => {
                        self.message = (
                            format!("No row {buf} (1–{})", self.order.len()),
                            Notice::Warn,
                        );
                        self.jump_buf.clear();
                    }
                }
            }
            // Reopening keeps the current text so a filter can be refined,
            // and works even when it matched nothing.
            KeyCode::Char('/') if !self.cfg.projects.is_empty() => {
                self.mode = Mode::Filter;
            }
            // Esc in the manager drops kept search, kept filter, or exit banner.
            KeyCode::Esc
                if self.search_active
                    && self.search_pi == self.order.get(self.selected).copied() =>
            {
                self.clear_search();
            }
            KeyCode::Esc if !self.filter_buf.is_empty() => {
                self.clear_filter();
            }
            KeyCode::Esc => {
                if let Some(&pi) = self.order.get(self.selected) {
                    if let Some(Some(s)) = self.sessions.get_mut(pi) {
                        if !s.status.is_running() {
                            s.dismissed_exit = true;
                        }
                    }
                }
            }
            // Before plain End: Shift+End returns the pane to live output,
            // as it does inside a terminal.
            KeyCode::End if k.modifiers.contains(KeyModifiers::SHIFT) => {
                scroll_live_selected(&mut self.sessions, &self.order, self.selected);
            }
            KeyCode::Home if !self.order.is_empty() => self.selected = 0,
            KeyCode::End if !self.order.is_empty() => self.selected = self.order.len() - 1,
            // With or without Shift, so the terminal's scroll keys work here too.
            KeyCode::PageUp | KeyCode::PageDown => {
                let pages = if k.code == KeyCode::PageUp { 1 } else { -1 };
                scroll_selected(
                    &mut self.sessions,
                    &self.order,
                    self.selected,
                    rows as usize,
                    pages,
                );
            }
            KeyCode::Char('r') => {
                if let Some(&pi) = self.order.get(self.selected) {
                    if slot_running(&self.sessions, pi) {
                        self.confirm_restart = Some(pi);
                    } else {
                        self.restart(pi, rows, cols);
                    }
                }
            }
            KeyCode::Char('S') if !self.order.is_empty() => {
                let stopped_count = self.stopped_count();
                if stopped_count == 0 {
                    self.message = ("All sessions already running.".to_string(), Notice::Info);
                } else {
                    self.confirm_start_all = true;
                }
            }
            // `x` stays bound to stop for muscle memory; `s` is the documented key.
            KeyCode::Char('s') | KeyCode::Char('x') => {
                let target = stop_target(&self.order, self.selected, &self.sessions);
                let is_running = target
                    .and_then(|pi| self.sessions.get(pi))
                    .and_then(|o| o.as_ref())
                    .is_some_and(|s| s.status.is_running());
                if is_running {
                    self.confirm_stop = target;
                } else if target.is_none() {
                    if let Some(&pi) = self.order.get(self.selected) {
                        self.say_if_starting(pi);
                    }
                } else {
                    // `s` never clears the exit banner: `Esc` dismisses it
                    // and `Enter` restarts the session. Say so instead of
                    // going quiet.
                    self.message = (
                        "Session already stopped. Enter restarts it; Esc clears this banner."
                            .to_string(),
                        Notice::Info,
                    );
                }
            }
            KeyCode::Delete | KeyCode::Char('d') => {
                self.confirm_remove = self.order.get(self.selected).copied();
            }
            KeyCode::Enter => {
                if self.cfg.projects.is_empty() {
                    self.open_add();
                } else if self.order.is_empty() {
                    self.mode = Mode::Filter;
                } else if let Some(&pi) = self.order.get(self.selected) {
                    self.open_session(pi, rows, cols);
                }
            }
            _ => {}
        }
    }
}

fn next_add_field(cur: usize, kind: Kind, forward: bool) -> usize {
    if kind == Kind::Service {
        match (cur, forward) {
            (0, true) => 1,
            (1, true) => 2,
            (2, true) => 3,
            (3, true) => 5,
            (5, true) => 6,
            (6, true) => 7,
            (7, true) => 8,
            (8, true) => 0,
            (8, false) => 7,
            (7, false) => 6,
            (6, false) => 5,
            (5, false) => 3,
            (3, false) => 2,
            (2, false) => 1,
            (1, false) => 0,
            _ => 8,
        }
    } else if forward {
        (cur + 1) % 9
    } else {
        (cur + 8) % 9
    }
}
