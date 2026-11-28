//! Drawing: sidebar, terminal pane, status bar and popups.

use super::*;

/// A bordered, centered popup with a bold colored title.
pub(super) fn modal(f: &mut Frame, title: &str, color: Color, lines: Vec<Line<'static>>) {
    let popup = modal_rect(&lines, f.area());
    f.render_widget(ratatui::widgets::Clear, popup);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(Line::from(Span::styled(
                    format!(" {title} "),
                    Style::default()
                        .fg(color)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ))),
        ),
        popup,
    );
}

/// The scrollbar on the right border of `area`: `len` rows, now at `pos`.
pub(crate) fn scrollbar(f: &mut Frame, area: Rect, len: usize, pos: usize) {
    f.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(Some("░"))
            .thumb_symbol("█"),
        area,
        &mut ScrollbarState::new(len).position(pos),
    );
}

/// Folder rows the add form shows at once; the list scrolls past this.
pub(super) const PICK_ROWS: usize = 6;

impl App {
    pub(crate) fn draw(&mut self, f: &mut Frame) {
        let sidebar = self.side_w(f.area().width);
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(f.area());
        let panels = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(sidebar), Constraint::Min(1)])
            .split(vertical[0]);
        self.side_rect = panels[0];
        self.pane_rect = panels[1];
        if sidebar > 0 {
            self.draw_sidebar(f, panels[0]);
        }
        self.draw_pane(f, panels[1]);
        self.draw_status(f, vertical[1]);
        if self.sidebar_collapsed && !self.has_modal() && sidebar > 0 {
            self.draw_collapsed_tooltip(f);
        }
        if self.has_modal() {
            let full_area = f.area();
            dim_area(f.buffer_mut(), full_area);
        }
        if self.mode == Mode::Help {
            self.draw_help(f);
        }
        self.draw_history(f);
        self.draw_git_view(f);
        self.draw_procs(f);
        self.draw_gsearch(f);
        self.draw_quick_reply(f);
        self.draw_confirms(f);
        self.draw_context_menu(f);
        if self.mode == Mode::Add {
            self.draw_add(f);
        }
        self.draw_vault_modal(f);
    }

    /// What a project without a session is doing: waiting, or on its way up.
    pub(super) fn idle_word(&self, pi: usize) -> &'static str {
        if self.is_starting(pi) {
            "Starting"
        } else {
            "Stopped"
        }
    }
}

pub(super) struct MgmtItem {
    pub(super) idx: usize,
    pub(super) label: &'static str,
    pub(super) key: &'static str,
    pub(super) row_id: usize,
}

pub(super) const MGMT_ITEMS: [MgmtItem; 8] = [
    MgmtItem {
        idx: 0,
        label: "Memory monitor",
        key: "M",
        row_id: MEMORY_ROW,
    },
    MgmtItem {
        idx: 1,
        label: "Environment Variable",
        key: "E",
        row_id: ENV_ROW,
    },
    MgmtItem {
        idx: 2,
        label: "Skills",
        key: "K",
        row_id: SKILLS_ROW,
    },
    MgmtItem {
        idx: 3,
        label: "Tools",
        key: "T",
        row_id: TOOLS_ROW,
    },
    MgmtItem {
        idx: 4,
        label: "References",
        key: "R",
        row_id: REFS_ROW,
    },
    MgmtItem {
        idx: 5,
        label: "Lessons",
        key: "L",
        row_id: LESSONS_ROW,
    },
    MgmtItem {
        idx: 6,
        label: "LLM models",
        key: "O",
        row_id: MODELS_ROW,
    },
    MgmtItem {
        idx: 7,
        label: "Servers",
        key: "V",
        row_id: SERVERS_ROW,
    },
];

mod add;
mod pane;
mod popups;
mod sidebar;
mod status;
