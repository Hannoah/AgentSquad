//! Keyboard input for the manager, terminal focus and the text modes.

use super::*;

impl App {
    pub(super) fn on_key(&mut self, k: KeyEvent, rows: u16, cols: u16) {
        self.dirty = true;
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
        match self.mode {
            Mode::Focus => self.key_focus(k, rows),
            Mode::Send => self.key_send(k, rows, cols),
            Mode::Filter => self.key_filter(k),
            Mode::Add => self.key_add(k),
            Mode::Help => self.key_help(k),
            Mode::Manager => self.key_manager_or_confirm(k, rows, cols),
        }
    }

    /// Manager keys, or the answer to whichever confirmation is open.
    pub(super) fn key_manager_or_confirm(&mut self, k: KeyEvent, rows: u16, cols: u16) {
        if self.context_menu.is_some() {
            self.key_context_menu(k, rows, cols);
        } else if self.history.is_some() {
            self.key_history(k);
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
            if !self
                .order
                .get(i)
                .is_some_and(|&pi| slot_running(&self.sessions, pi))
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
            }
        }
    }

    pub(super) fn key_send(&mut self, k: KeyEvent, rows: u16, cols: u16) {
        match k.code {
            KeyCode::Esc => {
                self.mode = Mode::Manager;
                self.send_buf.clear();
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
                    let alive = self
                        .sessions
                        .get(pi)
                        .and_then(|o| o.as_ref())
                        .map(|s| s.status.is_running())
                        .unwrap_or(false);
                    if !alive {
                        if let Some(e) =
                            start_session(&mut self.sessions, &self.cfg.projects, pi, rows, cols)
                        {
                            self.message = (e, Notice::Error);
                        }
                    }
                    if let Some(Some(s)) = self.sessions.get_mut(pi) {
                        let mut bytes = line.into_bytes();
                        // CR is Enter; LF reads as Shift+Enter (newline) to agents.
                        bytes.push(b'\r');
                        match s.input(&bytes) {
                            Ok(()) => {
                                self.message = (
                                    format!("Sent to {}", self.cfg.projects[pi].name),
                                    Notice::Info,
                                )
                            }
                            Err(e) => self.message = (e.to_string(), Notice::Error),
                        }
                    }
                }
            }
            KeyCode::Backspace => {
                self.send_buf.backspace();
            }
            KeyCode::Delete => {
                self.send_buf.delete();
            }
            KeyCode::Left => self.send_buf.move_left(),
            KeyCode::Right => self.send_buf.move_right(),
            KeyCode::Home => self.send_buf.move_home(),
            KeyCode::End => self.send_buf.move_end(),
            KeyCode::Char(ch)
                if !k.modifiers.contains(KeyModifiers::CONTROL)
                    && !k.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.send_buf.insert(ch)
            }
            _ => {}
        }
    }

    pub(super) fn key_filter(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Esc => {
                self.mode = Mode::Manager;
                self.filter_buf.clear();
                self.order = display_order(&self.cfg.projects);
                self.selected = 0;
            }
            KeyCode::Enter => self.mode = Mode::Manager,
            KeyCode::Backspace => {
                self.filter_buf.backspace();
                self.order = apply_filter(&self.cfg.projects, &self.filter_buf.as_str());
                self.selected = 0;
            }
            KeyCode::Delete => {
                self.filter_buf.delete();
                self.order = apply_filter(&self.cfg.projects, &self.filter_buf.as_str());
                self.selected = 0;
            }
            KeyCode::Left => self.filter_buf.move_left(),
            KeyCode::Right => self.filter_buf.move_right(),
            KeyCode::Home => self.filter_buf.move_home(),
            KeyCode::End => self.filter_buf.move_end(),
            KeyCode::Char(ch)
                if !k.modifiers.contains(KeyModifiers::CONTROL)
                    && !k.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.filter_buf.insert(ch);
                self.order = apply_filter(&self.cfg.projects, &self.filter_buf.as_str());
                self.selected = 0;
            }
            _ => {}
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
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
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
    fn step_path_list(&mut self, down: bool) {
        let n = self.add_picks.len();
        match (down, self.add_sel) {
            (true, None) if n > 0 => self.add_sel = Some(0),
            (true, Some(i)) if i + 1 < n => self.add_sel = Some(i + 1),
            (false, Some(0)) => self.add_sel = None,
            (false, Some(i)) => self.add_sel = Some(i - 1),
            (true, _) => self.add_field = 1,
            (false, None) => self.add_field = 4,
        }
    }

    pub(super) fn key_add(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Esc => {
                self.add_edit = None;
                self.mode = Mode::Manager;
            }
            KeyCode::Enter if self.add_edit.is_some() => self.save_edit(),
            // Editing only reaches Command and Autostart.
            KeyCode::Tab | KeyCode::Down | KeyCode::Up | KeyCode::BackTab
                if self.add_edit.is_some() =>
            {
                self.add_field = if self.add_field == 2 { 3 } else { 2 };
            }
            // A highlighted folder fills the path instead of saving.
            KeyCode::Enter if self.add_field == 0 && self.add_sel.is_some() => {
                self.pick_path(self.add_sel.unwrap_or(0));
            }
            KeyCode::Enter => {
                self.add_err.clear();
                // A new project must be visible, so drop any filter.
                self.filter_buf.clear();
                match load() {
                    Err(e) => self.add_err = format!("Config reload failed: {e:#}"),
                    Ok(disk) => {
                        merge_projects(&mut self.cfg, &mut self.sessions, disk);
                        self.order = display_order(&self.cfg.projects);
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
                            let name = p.name.clone();
                            let kind_label = p.kind.as_str();
                            self.cfg.projects.push(p);
                            self.sessions.push(None);
                            match persist_add(&self.cfg.projects[self.cfg.projects.len() - 1]) {
                                Err(e) => {
                                    self.cfg.projects.pop();
                                    self.sessions.pop();
                                    self.order = display_order(&self.cfg.projects);
                                    self.selected = 0;
                                    self.add_err = format!("Save failed: {e:#}");
                                }
                                Ok(()) => {
                                    self.cfg.projects.last().map(ledger::ensure_agent);
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
                                    self.message =
                                        (format!("Registered {name} ({kind_label})"), Notice::Info);
                                }
                            }
                        }
                    }
                }
            }
            KeyCode::Tab if self.add_field == 0 => self.complete_path(),
            KeyCode::Tab => self.add_field = (self.add_field + 1) % 5,
            KeyCode::Up if self.add_field == 0 => self.step_path_list(false),
            KeyCode::Down if self.add_field == 0 => self.step_path_list(true),
            KeyCode::BackTab | KeyCode::Up => self.add_field = (self.add_field + 4) % 5,
            KeyCode::Down => self.add_field = (self.add_field + 1) % 5,
            KeyCode::Backspace => {
                if let Some(input) = self.add_input() {
                    input.backspace();
                }
                self.add_edited();
            }
            KeyCode::Delete => {
                if let Some(input) = self.add_input() {
                    input.delete();
                }
                self.add_edited();
            }
            KeyCode::Left | KeyCode::Right => {
                let left = k.code == KeyCode::Left;
                match self.add_field {
                    1 => self.add_kind = self.add_kind.toggled(),
                    3 => self.add_autostart = !self.add_autostart,
                    _ => {
                        if let Some(input) = self.add_input() {
                            if left {
                                input.move_left()
                            } else {
                                input.move_right()
                            }
                        }
                    }
                }
            }
            KeyCode::Home => {
                if let Some(input) = self.add_input() {
                    input.move_home();
                }
            }
            KeyCode::End => {
                if let Some(input) = self.add_input() {
                    input.move_end();
                }
            }
            KeyCode::Char(' ') if self.add_field == 1 => {
                self.add_kind = self.add_kind.toggled();
            }
            KeyCode::Char(' ') if self.add_field == 3 => {
                self.add_autostart = !self.add_autostart;
            }
            KeyCode::Char(ch)
                if !k.modifiers.contains(KeyModifiers::CONTROL)
                    && !k.modifiers.contains(KeyModifiers::ALT) =>
            {
                if let Some(input) = self.add_input() {
                    input.insert(ch);
                }
                self.add_edited();
            }
            _ => {}
        }
    }

    pub(super) fn key_help(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?') => self.mode = Mode::Manager,
            _ => {}
        }
    }

    pub(super) fn key_manager(&mut self, k: KeyEvent, rows: u16, cols: u16) {
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
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('n') if !self.order.is_empty() => {
                self.selected = (self.selected + 1) % self.order.len()
            }
            KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('p') if !self.order.is_empty() => {
                self.selected = (self.selected + self.order.len() - 1) % self.order.len()
            }
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
            KeyCode::Char('M') => self.open_procs(),
            KeyCode::Char('e') if !self.order.is_empty() => self.open_edit(),
            KeyCode::Char('f') if !self.order.is_empty() => self.toggle_pin(),
            KeyCode::Char('?') => self.mode = Mode::Help,
            // Open the update prompt on demand; it also pops up by
            // itself whenever the manager has the keyboard.
            KeyCode::Char('U') if self.update_offer.is_some() && !self.updating => {
                self.confirm_update = true;
            }
            KeyCode::Char('a') => self.open_add(),
            KeyCode::Char(c) if c.is_ascii_digit() => {
                if let Some(i) = jump_to(self.order.len(), c) {
                    self.selected = i;
                }
            }
            // Reopening keeps the current text so a filter can be refined,
            // and works even when it matched nothing.
            KeyCode::Char('/') if !self.cfg.projects.is_empty() => {
                self.mode = Mode::Filter;
            }
            // Esc in the manager drops a kept filter, or dismisses exit banner.
            KeyCode::Esc if !self.filter_buf.is_empty() => {
                self.filter_buf.clear();
                self.order = display_order(&self.cfg.projects);
                self.selected = 0;
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
            KeyCode::Home if !self.order.is_empty() => self.selected = 0,
            KeyCode::End if !self.order.is_empty() => self.selected = self.order.len() - 1,
            KeyCode::PageUp if k.modifiers.contains(KeyModifiers::SHIFT) => {
                scroll_selected(
                    &mut self.sessions,
                    &self.order,
                    self.selected,
                    rows as usize,
                    1,
                );
            }
            KeyCode::PageDown if k.modifiers.contains(KeyModifiers::SHIFT) => {
                scroll_selected(
                    &mut self.sessions,
                    &self.order,
                    self.selected,
                    rows as usize,
                    -1,
                );
            }
            KeyCode::End if k.modifiers.contains(KeyModifiers::SHIFT) => {
                scroll_live_selected(&mut self.sessions, &self.order, self.selected);
            }
            KeyCode::PageUp if !self.order.is_empty() => {
                scroll_selected(
                    &mut self.sessions,
                    &self.order,
                    self.selected,
                    rows as usize,
                    1,
                );
            }
            KeyCode::PageDown if !self.order.is_empty() => {
                scroll_selected(
                    &mut self.sessions,
                    &self.order,
                    self.selected,
                    rows as usize,
                    -1,
                );
            }
            KeyCode::Char('r') => {
                if let Some(&pi) = self.order.get(self.selected) {
                    if slot_running(&self.sessions, pi) {
                        self.confirm_restart = Some(pi);
                    } else {
                        match restart_session(
                            &mut self.sessions,
                            &self.cfg.projects,
                            pi,
                            rows,
                            cols,
                        ) {
                            None => {
                                self.message = (
                                    format!("Restarted {}", self.cfg.projects[pi].name),
                                    Notice::Info,
                                );
                            }
                            Some(e) => self.message = (e, Notice::Error),
                        }
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
                } else if target.is_some() {
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
                if self.order.is_empty() {
                    self.open_add();
                } else if let Some(&pi) = self.order.get(self.selected) {
                    if let Some(e) =
                        start_session(&mut self.sessions, &self.cfg.projects, pi, rows, cols)
                    {
                        self.message = (e, Notice::Error);
                        return;
                    }
                    self.mode = Mode::Focus;
                }
            }
            _ => {}
        }
    }
}
