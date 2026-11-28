//! Mouse, paste and resize handling.

use super::*;

impl App {
    pub(crate) fn on_paste(&mut self, data: String) {
        self.dirty = true;
        if self.mode == Mode::Focus {
            if let Some(&pi) = self.order.get(self.selected) {
                if let Some(Some(s)) = self.sessions.get_mut(pi) {
                    s.scroll_live();
                    let bytes = paste_bytes(&data, s.parser.screen().bracketed_paste());
                    if let Err(e) = s.input(&bytes) {
                        self.message = (e.to_string(), Notice::Error);
                        self.mode = Mode::Manager;
                    }
                } else if let Some(typed) = self.starting.get_mut(&self.cfg.projects[pi].name) {
                    typed.extend(paste_bytes(&data, false));
                }
            }
        } else if let Some(qr) = self.quick_reply.as_mut().filter(|q| q.typing) {
            for ch in data.chars().filter(|&c| c != '\r' && c != '\n') {
                qr.input.insert(ch);
            }
        } else if self.mode == Mode::Send {
            for ch in data.chars().filter(|&c| c != '\r' && c != '\n') {
                self.send_buf.insert(ch);
            }
        } else if self.mode == Mode::Filter {
            for ch in data.chars().filter(|&c| c != '\r' && c != '\n') {
                self.filter_buf.insert(ch);
            }
            self.refresh_filter();
            self.selected = 0;
        } else if self.mode == Mode::Search {
            for ch in data.chars().filter(|&c| c != '\r' && c != '\n') {
                self.search.buf.insert(ch);
            }
            self.sync_search(true);
        } else if self.paste_vault_modal(&data) {
        } else if self.mode == Mode::Add {
            for ch in data.chars().filter(|&c| c != '\r' && c != '\n') {
                if let Some(input) = self.add_input() {
                    input.insert(ch);
                }
            }
            self.add_edited();
        }
    }

    /// The pane cell under the pointer, clamped to the pane so a drag that
    /// leaves it still extends the selection to the edge.
    fn pane_cell(&self, m: MouseEvent) -> (u16, u16) {
        let inner = self.pane_rect.inner(ratatui::layout::Margin::new(1, 1));
        let row = m.row.max(inner.y).min(inner.bottom().saturating_sub(1));
        let col = m.column.max(inner.x).min(inner.right().saturating_sub(1));
        (row.saturating_sub(inner.y), col.saturating_sub(inner.x))
    }

    pub(crate) fn on_mouse(&mut self, m: MouseEvent, area: Size, rows: u16, cols: u16) {
        if matches!(m.kind, MouseEventKind::Down(_)) || wheel_lines(m.kind).is_some() {
            self.sel = None; // the cells would no longer match the text
        }
        if let Some(lines) = wheel_lines(m.kind).filter(|_| self.vault_modal.is_some()) {
            self.wheel_vault_modal(lines > 0);
            self.dirty = true;
        } else if let (Some(lines), Some(p)) = (wheel_lines(m.kind), self.procs.as_mut()) {
            // The memory monitor is modal: the wheel moves its pick.
            p.confirm = None;
            p.sel = if lines > 0 {
                p.sel.saturating_sub(1)
            } else {
                (p.sel + 1).min(p.items.len().saturating_sub(1))
            };
            p.load_cmd();
            self.dirty = true;
        } else if let (Some(lines), Some(h)) = (wheel_lines(m.kind), self.history.as_mut()) {
            let popup = history_popup(area.into());
            let (left_col, _) = history_cols(popup);
            let at = ratatui::layout::Position::new(m.column, m.row);
            if left_col.contains(at) {
                if lines > 0 {
                    h.select(h.sel.saturating_sub(1));
                } else {
                    h.select(h.sel + 1);
                }
            } else {
                h.scroll_by(-lines);
            }
            self.dirty = true;
        } else if let (Some(lines), Some(g)) = (wheel_lines(m.kind), self.git_view.as_mut()) {
            let popup = history_popup(area.into());
            let (left_col, _) = history_cols(popup);
            let at = ratatui::layout::Position::new(m.column, m.row);
            if left_col.contains(at) {
                if lines > 0 {
                    g.select(g.sel.saturating_sub(1));
                } else {
                    g.select(g.sel + 1);
                }
            } else {
                g.scroll_by(-lines);
            }
            self.dirty = true;
        } else if wheel_lines(m.kind).is_some() && self.mode == Mode::Add {
            if let Some(lines) = wheel_lines(m.kind) {
                if self.add.field == 0 && !self.add.picks.is_empty() {
                    self.wheel_path_list(lines < 0);
                    self.dirty = true;
                }
            }
        } else if wheel_lines(m.kind).is_some()
            && (self.mode == Mode::Help
                || self.quick_reply.is_some()
                || self.gsearch.is_some()
                || self.history.is_some()
                || self.git_view.is_some()
                || self.context_menu.is_some()
                || !self.confirm_modals().is_empty())
        {
            // Keep modal popups isolated: wheel events do not leak into the dimmed background.
        } else if let Some(lines) = wheel_lines(m.kind) {
            let over_sidebar = self.mode != Mode::Focus
                && self.side_rect.width > 0
                && self
                    .side_rect
                    .contains(ratatui::layout::Position::new(m.column, m.row));
            if over_sidebar {
                self.mgmt_sel = None;
                if !self.order.is_empty() {
                    if lines > 0 {
                        self.selected = (self.selected + self.order.len() - 1) % self.order.len();
                    } else {
                        self.selected = (self.selected + 1) % self.order.len();
                    }
                }
            } else if let Some(s) = self
                .order
                .get(self.selected)
                .and_then(|&pi| self.sessions.get_mut(pi))
                .and_then(|o| o.as_mut())
            {
                let is_alt = s.parser.screen().alternate_screen();
                let scrollback_len = s.parser.screen().scrollback_len();

                if is_alt || (self.mode == Mode::Focus && scrollback_len == 0) {
                    let mouse_mode = s.parser.screen().mouse_protocol_mode();
                    if mouse_mode != vt100::MouseProtocolMode::None {
                        // Terminal coordinates are 1-based; the pane's
                        // border takes one column and one row.
                        let col = m
                            .column
                            .saturating_sub(self.pane_rect.x)
                            .clamp(1, cols.max(1));
                        let row = m.row.saturating_sub(self.pane_rect.y).clamp(1, rows.max(1));
                        let seq = mouse_wheel_seq(
                            s.parser.screen().mouse_protocol_encoding(),
                            lines > 0,
                            col,
                            row,
                        );
                        let _ = s.input(&seq);
                    } else {
                        // Alternate scroll mode:
                        // In fullscreen/alternate screen mode (like Claude Code, vim, less),
                        // translate wheel notches into PageUp/PageDown so the viewport scrolls naturally.
                        let seq: &[u8] = if lines > 0 {
                            b"\x1b[5~" // PageUp
                        } else {
                            b"\x1b[6~" // PageDown
                        };
                        let _ = s.input(seq);
                    }
                } else {
                    s.scroll_by(lines);
                }
            }
            self.dirty = true;
        } else if m.kind == MouseEventKind::Down(MouseButton::Left) {
            // Click landed outside the open confirmation popup.
            let outside_modal = self.confirm_modals().first().is_some_and(|(_, _, lines)| {
                !modal_rect(lines, area.into())
                    .contains(ratatui::layout::Position::new(m.column, m.row))
            });
            // A click on the hint row answers like the key it names.
            let answer = self.confirm_modals().last().and_then(|(_, _, lines)| {
                confirm_click(lines, modal_rect(lines, area.into()), m.column, m.row)
            });
            if self.vault_modal.is_some() {
                self.click_vault_modal(area, m);
                self.dirty = true;
            } else if let Some(qr) = self.quick_reply.as_mut() {
                let popup = quick_reply_popup(area.into(), qr.tail.len());
                let at = ratatui::layout::Position::new(m.column, m.row);
                if !popup.contains(at) {
                    self.quick_reply = None;
                } else if !qr.typing {
                    if let Some(ch) = quick_reply_button_hit(popup, m.column, m.row) {
                        if ch == 't' {
                            qr.typing = true;
                        } else {
                            let pi = qr.pi;
                            let bytes: &[u8] = if ch == '\r' {
                                b"\r"
                            } else {
                                &[ch as u8, b'\r']
                            };
                            self.send_quick_reply(pi, bytes);
                        }
                    }
                }
                self.dirty = true;
            } else if let Some(g) = self.git_view.as_mut() {
                let popup = history_popup(area.into());
                let at = ratatui::layout::Position::new(m.column, m.row);
                let (left_col, _) = history_cols(popup);
                if !popup.contains(at) {
                    self.git_view = None;
                } else if left_col.contains(at) {
                    let i = g.top.get() + usize::from(m.row - left_col.y);
                    if i < g.files.len() {
                        g.select(i);
                    }
                }
                self.dirty = true;
            } else if let Some(h) = self.history.as_mut() {
                let popup = history_popup(area.into());
                let at = ratatui::layout::Position::new(m.column, m.row);
                let (left_col, _) = history_cols(popup);
                if !popup.contains(at) {
                    self.history = None;
                } else if left_col.contains(at) {
                    let i = h.top.get() + usize::from(m.row - left_col.y);
                    if i < h.view.len() {
                        h.select(i);
                    }
                }
                self.dirty = true;
            } else if self.gsearch.is_some() {
                let shown = self
                    .gsearch
                    .as_ref()
                    .map(|g| g.hits.len().clamp(1, 10))
                    .unwrap_or(1);
                let popup = gsearch_popup(area.into(), shown);
                let at = ratatui::layout::Position::new(m.column, m.row);
                if !popup.contains(at) {
                    self.gsearch = None;
                    self.dirty = true;
                } else {
                    // With an empty query the first inner row is a hint,
                    // so hit rows start one row lower.
                    let hint_rows =
                        usize::from(self.gsearch.as_ref().is_some_and(|g| g.query.is_empty()));
                    let pick = (m.row >= popup.y + 1 + hint_rows as u16)
                        .then(|| usize::from(m.row - popup.y - 1 - hint_rows as u16));
                    let same = pick.is_some_and(|i| {
                        self.gsearch
                            .as_ref()
                            .is_some_and(|g| i < g.hits.len().min(10) && g.sel == i)
                    });
                    if same {
                        self.jump_gsearch();
                    } else if let Some(i) = pick {
                        if let Some(g) = self.gsearch.as_mut() {
                            if i < g.hits.len().min(10) {
                                g.sel = i;
                                self.dirty = true;
                            }
                        }
                    }
                }
            } else if let Some(p) = self.procs.as_mut() {
                let popup = procs_popup(area.into());
                let at = ratatui::layout::Position::new(m.column, m.row);
                let list = procs_list_rect(popup);
                if !popup.contains(at) {
                    self.procs = None;
                } else if list.contains(at) {
                    let i = p.top.get() + usize::from(m.row - list.y);
                    if i < p.items.len() {
                        p.confirm = None;
                        p.sel = i;
                        p.load_cmd();
                    }
                }
                self.dirty = true;
            } else if self.mode == Mode::Help {
                // Clicks inside the popup (e.g. selecting text) keep it open.
                if !help_popup(area.into())
                    .contains(ratatui::layout::Position::new(m.column, m.row))
                {
                    self.mode = Mode::Manager;
                    self.dirty = true;
                }
            } else if let Some(yes) = answer.filter(|_| self.context_menu.is_none()) {
                let code = if yes {
                    KeyCode::Char('y')
                } else {
                    KeyCode::Esc
                };
                self.key_manager_or_confirm(KeyEvent::new(code, KeyModifiers::NONE), rows, cols);
                self.dirty = true;
            } else if self.confirm_stop.is_some() {
                if outside_modal {
                    self.confirm_stop = None;
                    self.dirty = true;
                }
            } else if self.confirm_remove.is_some() {
                if outside_modal {
                    self.confirm_remove = None;
                    self.dirty = true;
                }
            } else if self.confirm_restart.is_some() {
                if outside_modal {
                    self.confirm_restart = None;
                    self.dirty = true;
                }
            } else if self.confirm_quit {
                if outside_modal {
                    self.confirm_quit = false;
                    self.dirty = true;
                }
            } else if self.confirm_start_all {
                if outside_modal {
                    self.confirm_start_all = false;
                    self.dirty = true;
                }
            } else if self.confirm_update {
                if outside_modal {
                    self.dismiss_update();
                    self.dirty = true;
                }
            } else if self.confirm_apply {
                if outside_modal {
                    self.dismiss_apply();
                    self.dirty = true;
                }
            } else if let Some(pi) = self.context_menu {
                // Quick-action menu is open: a click outside closes it,
                // a click on a numbered row runs that action.
                let data = self.cfg.projects.get(pi).map(|p| {
                    (
                        p.name.clone(),
                        slot_running(&self.sessions, pi),
                        p.kind,
                        p.restart_on_crash,
                    )
                });
                match data {
                    None => self.context_menu = None,
                    Some((name, running, kind, restart_on_crash)) => {
                        let lines = context_menu_lines_for(&name, running, kind, restart_on_crash);
                        let popup = modal_rect(&lines, area.into());
                        let clicked = ratatui::layout::Position::new(m.column, m.row);
                        if !popup.contains(clicked) {
                            self.context_menu = None;
                        } else {
                            // Row 0 names the project; rows 1.. are actions.
                            let rel = m.row.saturating_sub(popup.y + 1) as usize;
                            let items = context_menu_items_for(running, kind, restart_on_crash);
                            if let Some(&(key, _)) = rel.checked_sub(1).and_then(|i| items.get(i)) {
                                self.context_menu = None;
                                self.run_context_action(pi, key, rows, cols);
                            }
                        }
                    }
                }
                self.dirty = true;
            } else if self.mode == Mode::Add {
                let popup = self.add_popup(area.into());
                let clicked = ratatui::layout::Position::new(m.column, m.row);
                if !popup.contains(clicked) {
                    if self.add.path.is_empty() && self.add.cmd.is_empty() {
                        self.mode = Mode::Manager;
                    }
                } else {
                    let rel_y = m.row.saturating_sub(popup.y + 1);
                    let list_h = self.add_list_h();
                    let editing = self.add.edit.is_some();
                    // The Server row sits above the rest of the form.
                    match rel_y.checked_sub(1) {
                        None => {
                            self.add.field = 9;
                            self.cycle_server(true);
                        }
                        Some(0) => self.add.field = 0,
                        Some(r) if r <= list_h => {
                            let win = self.add_pick_window();
                            let row = usize::from(r.saturating_sub(2));
                            if r >= 2 && row < win.len() {
                                self.pick_path(win.start + row);
                            }
                        }
                        Some(r) => match r - list_h {
                            1 if !editing => {
                                self.add.field = 1;
                                self.add.kind = self.add.kind.toggled();
                            }
                            2 => self.add.field = 2,
                            3 => {
                                self.add.field = 3;
                                self.add.autostart = !self.add.autostart;
                            }
                            4 if !editing && self.add.kind == Kind::Agent => self.add.field = 4,
                            5 => {
                                self.add.field = 5;
                                self.open_env_modal(VaultTarget::AddForm);
                            }
                            6 => {
                                self.add.field = 6;
                                self.open_skill_modal(VaultTarget::AddForm);
                            }
                            7 => {
                                self.add.field = 7;
                                self.open_tool_modal(VaultTarget::AddForm);
                            }
                            8 => {
                                self.add.field = 8;
                                self.open_ref_modal(VaultTarget::AddForm);
                            }
                            _ => {}
                        },
                    }
                }
                self.dirty = true;
            } else if self.mode == Mode::Send {
                // Keep text input focus in Send mode
            } else if sidebar_toggle_click(self.side_rect, self.sidebar_collapsed, m.column, m.row)
            {
                self.toggle_sidebar();
            } else if let Some(i) = sidebar_click(
                self.side_rect,
                self.list_offset,
                &self.row_targets,
                m.column,
                m.row,
            ) {
                if matches!(self.mode, Mode::Filter | Mode::Search) {
                    self.mode = Mode::Manager;
                }
                // Coming from a Management row, the project row was not
                // highlighted, so this is a first click on it.
                let was_mgmt = self.mgmt_sel.take().is_some();
                if let Some(m) = mgmt_row_index(i) {
                    self.mgmt_sel = Some(m);
                    self.open_mgmt(m);
                } else if self.selected == i && !was_mgmt && self.mode == Mode::Focus {
                    // Clicking the manager while typing in a terminal hands
                    // the keyboard back to the manager.
                    self.mode = Mode::Manager;
                } else if self.selected == i && !was_mgmt {
                    // Re-clicking selected project: start if stopped, focus if running
                    if let Some(&pi) = self.order.get(self.selected) {
                        self.open_session(pi, rows, cols);
                    }
                } else {
                    // One click switches: a live session takes focus at once.
                    self.selected = i;
                    let live = self.order.get(i).is_some_and(|&pi| {
                        slot_running(&self.sessions, pi) || self.is_starting(pi)
                    });
                    self.mode = if live { Mode::Focus } else { Mode::Manager };
                }
                self.dirty = true;
            } else if self
                .side_rect
                .contains(ratatui::layout::Position::new(m.column, m.row))
            {
                // Header, group label, blank space or border of the manager:
                // from a terminal, hand the keyboard back to the manager;
                // from the manager, hand it to the selected session, starting
                // it first if it is stopped (same as re-clicking its row).
                if self.mode == Mode::Focus {
                    self.mode = Mode::Manager;
                } else if let Some(&pi) = self.order.get(self.selected) {
                    self.open_session(pi, rows, cols);
                }
                self.dirty = true;
            } else if self
                .pane_rect
                .contains(ratatui::layout::Position::new(m.column, m.row))
            {
                // Pressing in the terminal pane focuses a live session and may
                // start a drag-selection. A session that is not running keeps
                // its last output on screen (to copy from) and restarts only
                // when released without a drag. With zoom on in Manager mode,
                // defer focusing a slot until MouseUp so a drag does not widen
                // `pane_rect` mid-selection.
                if self.cfg.projects.is_empty() {
                    self.open_add();
                } else if let Some(&pi) = self.order.get(self.selected) {
                    let slot = self.sessions.get(pi).is_some_and(|o| o.is_some());
                    let live = slot && slot_running(&self.sessions, pi);
                    let inner = self.pane_rect.inner(ratatui::layout::Margin::new(1, 1));
                    let can_select =
                        slot && inner.contains(ratatui::layout::Position::new(m.column, m.row));
                    if can_select {
                        self.sel = Some(Selection::new(pi, self.pane_cell(m)));
                    }
                    let defer_zoom = can_select && self.zoom && self.mode == Mode::Manager;
                    if !defer_zoom && (live || !slot) {
                        self.open_session(pi, rows, cols);
                    }
                }
                self.dirty = true;
            } else if self
                .wait_badge_rect
                .contains(ratatui::layout::Position::new(m.column, m.row))
            {
                self.open_quick_reply();
            } else if m.row >= area.height.saturating_sub(1) && m.column < 12 {
                // Clicking the status-bar mode chip toggles help
                self.mode = if self.mode == Mode::Help {
                    Mode::Manager
                } else {
                    Mode::Help
                };
                self.dirty = true;
            }
        } else if m.kind == MouseEventKind::Moved {
            let next = if self.sidebar_collapsed && !self.has_modal() {
                sidebar_click(
                    self.side_rect,
                    self.list_offset,
                    &self.row_targets,
                    m.column,
                    m.row,
                )
            } else {
                None
            };
            if next != self.hover_row {
                self.hover_row = next;
                self.dirty = true;
            }
        } else if m.kind == MouseEventKind::Drag(MouseButton::Left) {
            let at = self.pane_cell(m);
            if let Some(sel) = self.sel.as_mut() {
                sel.drag_to(at);
                self.dirty = true;
            }
        } else if m.kind == MouseEventKind::Up(MouseButton::Left) {
            match self.sel {
                // A drag copies the moment the button comes up.
                Some(sel) if sel.moved => self.copy_selection(),
                // A plain click on a stopped session restarts it, or focuses a
                // deferred zoomed pane.
                Some(sel) => {
                    self.sel = None;
                    if !slot_running(&self.sessions, sel.pi)
                        || (self.zoom && self.mode == Mode::Manager)
                    {
                        self.open_session(sel.pi, rows, cols);
                    }
                }
                None => {}
            }
            self.dirty = true;
        } else if m.kind == MouseEventKind::Down(MouseButton::Middle) && self.mode == Mode::Focus {
            // Mouse twin of Ctrl+Q: leave the terminal without a key.
            self.mode = Mode::Manager;
            self.dirty = true;
        } else if m.kind == MouseEventKind::Down(MouseButton::Right)
            // From a terminal, the filter line or help too: the click hands
            // the keyboard to the manager first. Send and the add form keep
            // their typed text, so they are left alone.
            && matches!(
                self.mode,
                Mode::Manager | Mode::Focus | Mode::Filter | Mode::Search | Mode::Help
            )
            && !self.has_overlay()
        {
            // Right-click on a sidebar row opens its quick-action menu, a
            // Management row opens that tool. Only sidebar rows react, so a
            // right-click in the terminal pane never steals its keyboard, and
            // nothing runs until a menu row is picked.
            if let Some(i) = sidebar_click(
                self.side_rect,
                self.list_offset,
                &self.row_targets,
                m.column,
                m.row,
            ) {
                self.mode = Mode::Manager;
                if let Some(m) = mgmt_row_index(i) {
                    self.mgmt_sel = Some(m);
                    self.open_mgmt(m);
                } else {
                    self.mgmt_sel = None;
                    self.selected = i;
                    if let Some(&pi) = self.order.get(self.selected) {
                        self.context_menu = Some(pi);
                        self.context_sel = 0;
                    }
                }
                self.dirty = true;
            }
        }
    }
}
