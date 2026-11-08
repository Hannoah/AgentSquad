//! Confirmations, the right-click menu, orders and briefings.

use super::*;

impl App {
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
        match update_key(k.code) {
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
            Some(false) => {
                self.confirm_update = false;
                self.update_offer = None;
                self.message = (
                    "Update skipped; squad asks again next launch.".to_string(),
                    Notice::Info,
                );
            }
            None => {}
        }
    }

    pub(super) fn key_confirm_quit(&mut self, k: KeyEvent) {
        match confirm_destructive_key(k.code) {
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
                // Spawned in parallel: one slow start no longer
                // stalls the others (or the keyboard).
                let (started_n, failed) = start_sessions(
                    &mut self.sessions,
                    &self.cfg.projects,
                    &self.order,
                    rows,
                    cols,
                );
                self.message = match failed {
                    Some(e) => (format!("Started {started_n}; error: {e}"), Notice::Error),
                    None if started_n == 0 => {
                        ("All sessions already running.".to_string(), Notice::Info)
                    }
                    None => (
                        format!("Started {}.", plural_sessions(started_n)),
                        Notice::Info,
                    ),
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
        match confirm_destructive_key(k.code) {
            Some(true) => {
                if let Some(pi) = self.confirm_stop.take() {
                    if let Some(slot) = self.sessions.get_mut(pi) {
                        if let Some(old) = slot.take() {
                            thread::spawn(move || drop(old));
                        }
                    }
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
        match confirm_destructive_key(k.code) {
            Some(true) => {
                if let Some(pi) = self.confirm_restart.take() {
                    self.message = match restart_session(
                        &mut self.sessions,
                        &self.cfg.projects,
                        pi,
                        rows,
                        cols,
                    ) {
                        None => (
                            format!("Restarted {}", self.cfg.projects[pi].name),
                            Notice::Info,
                        ),
                        Some(e) => (e, Notice::Error),
                    };
                }
            }
            Some(false) => self.confirm_restart = None,
            None => {}
        }
    }

    pub(super) fn key_confirm_remove(&mut self, k: KeyEvent) {
        match confirm_destructive_key(k.code) {
            Some(true) => {
                if let Some(pi) = self.confirm_remove.take() {
                    // Save first: a failed save leaves the project and
                    // its running session untouched.
                    let name = self.cfg.projects[pi].name.clone();
                    match persist_remove(&name) {
                        Ok(()) => {
                            if let Some(Some(old)) = self.sessions.get_mut(pi).map(|o| o.take()) {
                                thread::spawn(move || drop(old));
                            }
                            remove_project(&mut self.cfg.projects, &mut self.sessions, pi);
                            // Keep an active filter across the removal.
                            self.order =
                                apply_filter(&self.cfg.projects, &self.filter_buf.as_str());
                            self.selected = self.selected.min(self.order.len().saturating_sub(1));
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

    /// Run one row of the right-click quick-action menu. `action` is the
    /// row index into `context_menu_items(running)`.
    pub(super) fn run_context_action(&mut self, pi: usize, action: usize, rows: u16, cols: u16) {
        if self.cfg.projects.get(pi).is_none() {
            return;
        }
        // Select the project the menu was opened for, so the action and
        // the sidebar agree even if the selection moved meanwhile.
        if let Some(pos) = self.order.iter().position(|&x| x == pi) {
            self.selected = pos;
        }
        if slot_running(&self.sessions, pi) {
            match action {
                0 => self.mode = Mode::Focus,
                1 => self.confirm_stop = Some(pi),
                2 => self.confirm_restart = Some(pi),
                3 => self.confirm_remove = Some(pi),
                _ => {}
            }
            return;
        }
        match action {
            0 => match start_session(&mut self.sessions, &self.cfg.projects, pi, rows, cols) {
                None => self.mode = Mode::Focus,
                Some(e) => self.message = (e, Notice::Error),
            },
            1 => {
                self.message =
                    match restart_session(&mut self.sessions, &self.cfg.projects, pi, rows, cols) {
                        None => (
                            format!("Restarted {}", self.cfg.projects[pi].name),
                            Notice::Info,
                        ),
                        Some(e) => (e, Notice::Error),
                    };
            }
            2 => self.confirm_remove = Some(pi),
            _ => {}
        }
    }

    /// Keys while the quick-action menu is open: number rows run, `Enter`
    /// runs the first row, `Esc` closes. Everything else is ignored so a
    /// key meant for a session cannot slip through.
    pub(super) fn key_context_menu(&mut self, k: KeyEvent, rows: u16, cols: u16) {
        let Some(pi) = self.context_menu else {
            return;
        };
        let count = context_menu_items(slot_running(&self.sessions, pi)).len();
        match k.code {
            KeyCode::Esc => self.context_menu = None,
            KeyCode::Enter => {
                self.context_menu = None;
                self.run_context_action(pi, 0, rows, cols);
            }
            KeyCode::Char(c) => {
                if let Some(d) = c.to_digit(10) {
                    let idx = d as usize;
                    if idx >= 1 && idx <= count {
                        self.context_menu = None;
                        self.run_context_action(pi, idx - 1, rows, cols);
                    }
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
        let Some(p) = self.cfg.projects.get(pi) else {
            return;
        };
        self.message = if !p.ledger {
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
                    let mut bytes = ledger::brief_line(&p.name, commander, &subs).into_bytes();
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
}
