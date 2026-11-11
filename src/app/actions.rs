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
        self.order = display_order(&self.cfg.projects);
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

    /// Enter in the edit form: save the new command and autostart.
    pub(super) fn save_edit(&mut self) {
        let Some(name) = self.add_edit.clone() else {
            return;
        };
        let cmd = split_command(&self.add_cmd.as_str());
        if let Err(e) = persist_edit(&name, cmd.clone(), self.add_autostart) {
            self.add_err = format!("Save failed: {e:#}");
            return;
        }
        let autostart = self.add_autostart;
        let Some(pi) = self
            .cfg
            .projects
            .iter()
            .position(|p| p.name.eq_ignore_ascii_case(&name))
        else {
            return;
        };
        let p = &mut self.cfg.projects[pi];
        p.command = if cmd.is_empty() { shell() } else { cmd };
        p.autostart = autostart;
        self.add_edit = None;
        self.mode = Mode::Manager;
        let note = if slot_running(&self.sessions, pi) {
            "; restart it to use the new command"
        } else {
            ""
        };
        self.message = (format!("Saved {name}{note}"), Notice::Info);
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
        self.order = display_order(&self.cfg.projects);
        self.selected = self.order.iter().position(|&x| x == pi).unwrap_or(0);
        let what = if pinned { "Pinned" } else { "Unpinned" };
        self.message = (format!("{what} {name}"), Notice::Info);
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
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('h') => self.history = None,
            KeyCode::Down | KeyCode::Char('j') => h.select(h.sel + 1),
            KeyCode::Up | KeyCode::Char('k') => h.select(h.sel.saturating_sub(1)),
            KeyCode::PageDown | KeyCode::Char(' ') => h.scroll_by(10),
            KeyCode::PageUp => h.scroll_by(-10),
            KeyCode::Home => h.select(0),
            KeyCode::End => h.select(usize::MAX),
            _ => {}
        }
        self.dirty = true;
    }

    /// `M`: list the processes holding the most RAM on the machine.
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
                kill_trees(&[t.pid]);
                self.message = (format!("Killed {} ({})", t.name, t.pid), Notice::Info);
                p.items = top_processes(PROC_ROWS);
                p.sel = p.sel.min(p.items.len().saturating_sub(1));
                p.load_cmd();
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
                p.items = top_processes(PROC_ROWS);
                p.sel = p.sel.min(p.items.len().saturating_sub(1));
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
        match confirm_key(k.code) {
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
                            if let Some(Some(old)) = self.sessions.get_mut(pi).map(|o| o.take()) {
                                thread::spawn(move || drop(old));
                            }
                            remove_project(&mut self.cfg.projects, &mut self.sessions, pi);
                            // Keep an active filter across the removal.
                            self.order =
                                apply_filter(&self.cfg.projects, &self.filter_buf.as_str());
                            self.selected = self.selected.min(self.order.len().saturating_sub(1));
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
                4 => self.begin_set_superior(pi),
                _ => {}
            }
            return;
        }
        match action {
            0 => self.open_session(pi, rows, cols),
            1 => self.restart(pi, rows, cols),
            2 => self.confirm_remove = Some(pi),
            3 => self.begin_set_superior(pi),
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
}
