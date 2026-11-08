use crate::config::{display_order, is_bare_shell, Kind, Project};
use crate::session::Session;
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    text::{Line, Span},
};

pub fn confirm_stop_prompt(name: &str) -> String {
    format!("Stop session '{name}'?")
}

pub fn pane_status_text(status: &str, uptime_secs: Option<u64>, tick: u64) -> String {
    let mut text = format!("{} {status}", status_glyph(status, tick));
    if status == "Running" {
        if let Some(secs) = uptime_secs {
            text.push(' ');
            text.push_str(&uptime_label(secs));
        }
    }
    text
}

pub const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
pub fn spinner_frame(tick: u64) -> char {
    SPINNER[(tick as usize) % SPINNER.len()]
}
pub fn status_glyph(status: &str, tick: u64) -> char {
    if status == "Running" {
        spinner_frame(tick)
    } else {
        '●'
    }
}
/// Short uptime label for a running session: "7s", "12m", "3h".
pub fn uptime_label(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h", secs / 3600)
    }
}
pub fn status_style(status: &str) -> Style {
    if status == "Running" {
        Style::default().fg(Color::Green)
    } else if status.starts_with("Exited") {
        // Clean exits fade away; crashes glow red.
        if status.ends_with("(0)") {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(Color::Red)
        }
    } else if status.starts_with("Error") {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}
pub fn help_lines() -> Vec<&'static str> {
    vec![
        "Enter      open session (starts it if stopped)",
        "Click      select a project / focus its terminal",
        "j/k, arrows  select project",
        "1-9        jump to project",
        "Home/End   first / last project",
        "/          filter projects (Esc clears)",
        "a          add project",
        "m          message: send a line to session",
        "s          stop session (asks to confirm)",
        "r          restart session",
        "d/Del      remove project (asks to confirm)",
        "S          start all stopped sessions",
        "U          install the update squad found",
        "PgUp/PgDn  scroll history (Shift+ inside a terminal)",
        "Ctrl+Q, middle-click  terminal back to manager",
        "Alt+1-9    switch project from inside a terminal",
        "q, Ctrl+C  quit (confirms when sessions exist)",
        "?/Esc      close this help",
    ]
}
/// Split a help row into its key column and description at the first
/// run of two or more spaces, so the key can be highlighted.
pub fn split_help_line(line: &str) -> (&str, &str) {
    if let Some(i) = line.find("  ") {
        (line[..i].trim_end(), line[i..].trim_start())
    } else {
        (line, "")
    }
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Manager,
    Focus,
    Send,
    Filter,
    Add,
    Help,
}
/// Severity of the one-line status bar message, so errors and exit
/// notices read differently from idle hints and confirmations.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Notice {
    Info,
    Warn,
    Error,
}
/// How long a status-bar message stays before the idle hints return.
/// Problems linger longer than confirmations.
pub fn message_ttl(kind: Notice) -> std::time::Duration {
    std::time::Duration::from_secs(match kind {
        Notice::Info => 4,
        Notice::Warn | Notice::Error => 8,
    })
}
pub fn notice_style(kind: Notice) -> Style {
    match kind {
        Notice::Error => Style::default()
            .fg(Color::Red)
            .add_modifier(ratatui::style::Modifier::BOLD),
        Notice::Warn => Style::default().fg(Color::Yellow),
        Notice::Info => Style::default(),
    }
}
pub fn mode_chip(m: Mode) -> (&'static str, Style) {
    // Solid-background chips so the current mode reads at a glance.
    let (label, fg, bg) = match m {
        Mode::Manager => (" MANAGER ", Color::Black, Color::Cyan),
        Mode::Focus => (" INPUT ", Color::Black, Color::Green),
        Mode::Send => (" SEND ", Color::Black, Color::Yellow),
        Mode::Filter => (" FILTER ", Color::Black, Color::Magenta),
        Mode::Add => (" ADD ", Color::White, Color::Blue),
        Mode::Help => (" HELP ", Color::Black, Color::Gray),
    };
    (label, Style::default().fg(fg).bg(bg))
}
/// Status bar segments as (key, description) pairs. An empty key means
/// the text is a plain instruction, not a key press.
pub fn mode_hint_parts(m: Mode) -> &'static [(&'static str, &'static str)] {
    match m {
        Mode::Manager => &[
            ("Enter", "open"),
            ("j/k", "select"),
            ("/", "filter"),
            ("a", "add"),
            ("m", "message"),
            ("s", "stop"),
            ("r", "restart"),
            ("d/Del", "remove"),
            ("S", "start all"),
            ("q", "quit"),
            ("?", "help"),
        ],
        Mode::Focus => &[
            ("Ctrl+Q", "manager"),
            ("Alt+1-9", "switch"),
            ("", "other keys go to the session"),
        ],
        Mode::Send => &[("Enter", "sends"), ("Esc", "cancels")],
        Mode::Filter => &[
            ("", "type to narrow"),
            ("Enter", "keeps"),
            ("Esc", "clears"),
        ],
        Mode::Add => &[
            ("Tab", "next"),
            ("Space", "kind"),
            ("Enter", "save"),
            ("Esc", "cancel"),
        ],
        Mode::Help => &[("?/Esc", "closes")],
    }
}
pub fn mode_hints(m: Mode) -> String {
    mode_hint_parts(m)
        .iter()
        .map(|(k, d)| match (*k, *d) {
            ("", d) => d.to_string(),
            (k, "") => k.to_string(),
            (k, d) => format!("{k} {d}"),
        })
        .collect::<Vec<_>>()
        .join(" | ")
}
/// Hint pairs in discovery order: most essential first. Narrow bars show
/// a leading subset so `q` never disappears while trivia survives.
pub fn priority_hint_parts(m: Mode) -> &'static [(&'static str, &'static str)] {
    match m {
        Mode::Manager => &[
            ("Enter", "open"),
            ("/", "filter"),
            ("q", "quit"),
            ("s", "stop"),
            ("?", "help"),
            ("m", "message"),
            ("a", "add"),
            ("d/Del", "remove"),
            ("r", "restart"),
            ("S", "start all"),
            ("j/k", "select"),
        ],
        Mode::Add => &[
            ("Enter", "save"),
            ("Esc", "cancel"),
            ("Tab", "next"),
            ("Space", "kind"),
        ],
        _ => &[],
    }
}
/// Rendered length of hint pairs as joined by `mode_hints`/`hint_spans`.
pub fn hint_text_len(parts: &[(&str, &str)]) -> usize {
    parts
        .iter()
        .map(|(k, d)| k.len() + d.len() + usize::from(!k.is_empty() && !d.is_empty()))
        .sum::<usize>()
        + 3 * parts.len().saturating_sub(1)
}
/// Full hints when they fit; otherwise the longest leading run of the
/// priority order that fits, so narrow bars degrade gracefully instead
/// of clipping mid-hint. Modes without a priority table are already
/// short enough to show in full.
pub fn fitted_hints(m: Mode, width: usize) -> Vec<Span<'static>> {
    let full = mode_hint_parts(m);
    if hint_text_len(full) <= width {
        return hint_spans(full);
    }
    let order = priority_hint_parts(m);
    if order.is_empty() {
        return hint_spans(full);
    }
    let mut take = 0usize;
    let mut len = 0usize;
    for (i, (k, d)) in order.iter().enumerate() {
        let part = k.len() + d.len() + usize::from(!k.is_empty() && !d.is_empty());
        let add = part + if i == 0 { 0 } else { 3 };
        if len + add > width {
            break;
        }
        len += add;
        take = i + 1;
    }
    hint_spans(&order[..take])
}
/// Styled status bar segments: bright keys, dimmed descriptions.
pub fn hint_spans(parts: &[(&str, &str)]) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    for (i, (key, desc)) in parts.iter().enumerate() {
        if i > 0 {
            out.push(Span::styled(
                " | ".to_string(),
                Style::default().fg(Color::DarkGray),
            ));
        }
        if !key.is_empty() {
            out.push(Span::styled(
                key.to_string(),
                Style::default().add_modifier(ratatui::style::Modifier::BOLD),
            ));
        }
        if !key.is_empty() && !desc.is_empty() {
            out.push(Span::raw(" ".to_string()));
        }
        if !desc.is_empty() {
            out.push(Span::styled(
                desc.to_string(),
                Style::default().add_modifier(ratatui::style::Modifier::DIM),
            ));
        }
    }
    out
}

pub fn confirm_quit_prompt(n: usize) -> String {
    format!("Quit and discard {}?", plural_sessions(n))
}
/// Count noun with a correct plural: "1 session", "3 sessions".
pub fn plural_sessions(n: usize) -> String {
    if n == 1 {
        "1 session".to_string()
    } else {
        format!("{n} sessions")
    }
}
/// Stop-confirm modal body: the prompt in alarming red, key hints in
/// the shared status-bar style.
pub fn confirm_stop_lines(name: &str) -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            confirm_stop_prompt(name),
            Style::default()
                .fg(Color::Red)
                .add_modifier(ratatui::style::Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(hint_spans(&[("y", "confirms"), ("n / Esc", "cancels")])),
    ]
}
/// Restart-confirm modal body (only shown for a running session).
pub fn confirm_restart_lines(name: &str) -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            format!("Restart session '{name}'?"),
            Style::default()
                .fg(Color::Red)
                .add_modifier(ratatui::style::Modifier::BOLD),
        )),
        Line::from("The running session and its output are discarded."),
        Line::from(""),
        Line::from(hint_spans(&[("y", "restarts"), ("n / Esc", "cancels")])),
    ]
}
/// Quit-confirm modal body: same key-hint styling as the stop modal.
pub fn confirm_quit_lines(n: usize) -> Vec<Line<'static>> {
    vec![
        Line::from(confirm_quit_prompt(n)),
        Line::from(""),
        Line::from(hint_spans(&[("y", "quits"), ("n / Esc", "cancels")])),
    ]
}
pub fn confirm_remove_prompt(name: &str) -> String {
    format!("Remove project '{name}'?")
}
/// Remove-confirm modal body: same key-hint styling as the stop modal.
pub fn confirm_remove_lines(name: &str) -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            confirm_remove_prompt(name),
            Style::default()
                .fg(Color::Red)
                .add_modifier(ratatui::style::Modifier::BOLD),
        )),
        Line::from("Running sessions are stopped; files are untouched."),
        Line::from(""),
        Line::from(hint_spans(&[("y", "removes"), ("n / Esc", "cancels")])),
    ]
}
pub fn confirm_start_all_prompt(n: usize) -> String {
    format!("Start all {}?", plural_sessions(n))
}
pub fn confirm_start_all_lines(n: usize) -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            confirm_start_all_prompt(n),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(ratatui::style::Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(hint_spans(&[
            ("y / Enter", "starts"),
            ("n / Esc", "cancels"),
        ])),
    ]
}
pub fn dim_area(buf: &mut ratatui::buffer::Buffer, area: ratatui::layout::Rect) {
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_style(cell.style().add_modifier(ratatui::style::Modifier::DIM));
            }
        }
    }
}
pub fn empty_state_lines(has_projects: bool) -> Vec<String> {
    if has_projects {
        vec![
            "No session running.".into(),
            "".into(),
            "Press Enter to start the selected project.".into(),
        ]
    } else {
        vec![
            "No projects yet.".into(),
            "".into(),
            "Press Enter or a, then type a folder path to add your first project.".into(),
            "Or from your shell: squad add <path> [-- command]".into(),
            "".into(),
            "Then press Enter to start it.".into(),
        ]
    }
}
pub fn centered_rect(w_pct: u16, h_pct: u16, area: ratatui::layout::Rect) -> ratatui::layout::Rect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - h_pct) / 2),
            Constraint::Percentage(h_pct),
            Constraint::Percentage(100 - h_pct - (100 - h_pct) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - w_pct) / 2),
            Constraint::Percentage(w_pct),
            Constraint::Percentage(100 - w_pct - (100 - w_pct) / 2),
        ])
        .split(v[1])[1]
}

pub fn confirm_update_lines(tag: &str) -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            format!(
                "squad {tag} is out (you have v{}).",
                env!("CARGO_PKG_VERSION")
            ),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(ratatui::style::Modifier::BOLD),
        )),
        Line::from("Download and install it now?"),
        Line::from("Running sessions keep running; restart squad to switch."),
        Line::from(""),
        Line::from(hint_spans(&[("y", "updates"), ("n / Esc", "later")])),
    ]
}
/// Centered rect of an exact size, clamped to `area`.
pub fn fixed_rect(w: u16, h: u16, area: ratatui::layout::Rect) -> ratatui::layout::Rect {
    let (w, h) = (w.min(area.width), h.min(area.height));
    ratatui::layout::Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}
pub fn cli_help() -> String {
    format!(
        "AgentSquad v{} — manage Agent and Service terminal sessions in one TUI.

Usage:
  squad                                              Open manager
  squad add [<path>] [--kind agent|service] [--autostart] [-- cmd]
                                                     Register project (path optional for services)
  squad list                                         List registered projects
  squad remove <name>                                Unregister (does not delete files)
  squad start <name>                                 Open manager and start project
  squad update [--check]                             Install the newest release (--check only looks)
  squad --version                                    Print version

Manager keys:
  j/k, arrows  select · Enter  open · 1-9  jump · Home/End  first/last
  /  filter · a  add · m  message (send a line)
  s  stop · r  restart · d/Del  remove · S  start all
  PgUp/PgDn  scroll · q  quit · ?  help

Terminal keys:
  Ctrl+Q  back to manager (or middle-click) · Alt+1-9  switch project
  every other key, Esc included, goes to the session

Quitting stops managed child processes; no detached daemon.",
        env!("CARGO_PKG_VERSION")
    )
}
/// Severity of a session-exit notice: errors alarm, crashes warn,
/// clean exits stay quiet like any other confirmation.
pub fn exit_notice_kind(status: &str) -> Notice {
    if status.starts_with("Error") {
        Notice::Error
    } else if needs_attention(status) {
        Notice::Warn
    } else {
        Notice::Info
    }
}
/// One-time notice when a session leaves Running. Returns the message
/// the first time only; `announced` flips so repeats stay silent.
pub fn exit_notice(
    name: &str,
    was_running: bool,
    now: &str,
    announced: &mut bool,
) -> Option<String> {
    if was_running && now != "Running" && !*announced {
        *announced = true;
        Some(format!("Session '{name}' {now}; Enter restarts it"))
    } else {
        None
    }
}
/// Move the visible viewport of the selected session's scrollback.
/// Positive `pages` scrolls up (into history); negative scrolls down.
pub fn scroll_selected(
    sessions: &mut [Option<Session>],
    order: &[usize],
    selected: usize,
    page_rows: usize,
    pages: isize,
) {
    if let Some(&pi) = order.get(selected) {
        if let Some(Some(s)) = sessions.get_mut(pi) {
            if s.parser.screen().alternate_screen() {
                let seq: &[u8] = if pages > 0 { b"\x1b[5~" } else { b"\x1b[6~" };
                let _ = s.input(seq);
            } else {
                s.scroll_by((page_rows.max(1) as isize) * pages);
            }
        }
    }
}
/// First-run hint when `squad list` has nothing to show. Stderr only,
/// so stdout stays empty and machine-readable.
pub fn empty_list_hint() -> &'static str {
    "No projects yet. Run: squad add <path> [-- command]"
}
/// Paint the kind column of `squad list` with the same hue as the TUI
/// sidebar. Plain text when `enabled` is false so piped output stays clean.
pub fn paint_kind(cell: &str, kind: Kind, enabled: bool) -> String {
    if !enabled {
        return cell.to_string();
    }
    use crossterm::style::Stylize;
    match kind {
        Kind::Agent => cell.cyan().to_string(),
        Kind::Service => cell.magenta().to_string(),
    }
}
/// `squad list` rows in manager (agents-first) order, with the name
/// and kind columns padded so they line up in a terminal.
pub fn list_table(projects: &[Project], color: bool) -> Vec<String> {
    let order = display_order(projects);
    let name_w = order
        .iter()
        .map(|&i| projects[i].name.chars().count())
        .max()
        .unwrap_or(0);
    let kind_w = order
        .iter()
        .map(|&i| projects[i].kind.as_str().chars().count())
        .max()
        .unwrap_or(0);
    order
        .iter()
        .map(|&i| {
            let p = &projects[i];
            let kind = paint_kind(&format!("{:<kind_w$}", p.kind.as_str()), p.kind, color);
            format!(
                "{:<name_w$}  {kind}  {}  {}",
                p.name,
                p.path.display(),
                p.command.join(" ")
            )
        })
        .collect()
}
/// Sidebar width in columns: hidden on narrow terminals, a quarter of
/// the width (clamped) otherwise so names and commands stay readable.
pub fn sidebar_width(area_w: u16) -> u16 {
    if area_w < 60 {
        0
    } else {
        (area_w / 4).clamp(24, 38)
    }
}
/// Right-hand status bar summary: live sessions over registered projects.
pub fn fleet_summary(running: usize, total: usize) -> String {
    format!("● {running}/{total}")
}
/// Width of the right-hand fleet summary so the layout reserves enough
/// room for the failure alert instead of clipping it.
pub fn summary_width(running: usize, total: usize, failed: usize) -> u16 {
    let mut w = fleet_summary(running, total).chars().count();
    if failed > 0 {
        w += format!(" ✖ {failed}").chars().count();
    }
    (w + 1) as u16
}
/// Sidebar border: bright cyan while navigating, dimmed once typing
/// moves into the terminal pane. Mirrors `pane_border_style` so exactly
/// one panel looks active at a time.
pub fn sidebar_border_style(focus: bool) -> Style {
    if focus {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(Color::Cyan)
    }
}
/// Terminal pane border: bright green while typing, dimmed in manager
/// mode so the focus state reads at a glance.
pub fn pane_border_style(focus: bool) -> Style {
    if focus {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}
/// Sessions that need attention: crashed exits and errors.
/// Clean exits and stopped slots stay quiet.
pub fn needs_attention(status: &str) -> bool {
    status.starts_with("Error") || (status.starts_with("Exited") && !status.ends_with("(0)"))
}
/// Keep the selected sidebar row inside the visible window: scroll down
/// when it runs past the bottom, scroll up when it runs past the top.
pub fn keep_visible(offset: usize, sel_row: usize, visible: usize) -> usize {
    if visible == 0 {
        return 0;
    }
    if sel_row < offset {
        sel_row
    } else if sel_row + 1 > offset + visible {
        sel_row + 1 - visible
    } else {
        offset
    }
}
/// Selected sidebar row: bright cyan on dark gray so it wins over every
/// other hue in the row.
pub fn selected_row_style() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .bg(Color::DarkGray)
        .add_modifier(ratatui::style::Modifier::BOLD)
}
/// Sidebar project name: selection highlight wins; otherwise the kind
/// hue, bright while running and dimmed while idle, so Agents and
/// Services read apart at a glance.
pub fn row_name_style(selected: bool, status: &str, kind: Kind) -> Style {
    if selected {
        selected_row_style()
    } else if status == "Running" {
        Style::default()
            .fg(kind_color(kind))
            .add_modifier(ratatui::style::Modifier::BOLD)
    } else {
        Style::default()
            .fg(kind_color(kind))
            .add_modifier(ratatui::style::Modifier::DIM)
    }
}
/// Split a sidebar row's inner width into (status, name, detail) budgets.
/// The list widget clips anything past the row width, so the status head
/// is reserved first; the name takes what it needs and the detail gets
/// the rest only when at least a few columns remain.
pub fn row_widths(
    inner_w: usize,
    prefix_len: usize,
    status_len: usize,
    name_len: usize,
) -> (usize, usize, usize) {
    let status_w = status_len.min(inner_w.saturating_sub(prefix_len + 1 + 4));
    let rest = inner_w.saturating_sub(prefix_len + status_w + 1);
    if rest.saturating_sub(name_len) >= 5 {
        (status_w, name_len, rest - name_len)
    } else {
        (status_w, rest, 0)
    }
}
/// Sidebar title: plain normally, `AgentSquad vX.Y.Z (shown/total)`
/// while a filter hides projects.
pub fn list_title(total: usize, shown: usize) -> String {
    let base = format!("AgentSquad v{}", env!("CARGO_PKG_VERSION"));
    if shown == total {
        base
    } else {
        format!("{base} ({shown}/{total})")
    }
}
/// Group color, reused for headers and the terminal title kind chip.
pub fn kind_color(kind: Kind) -> Color {
    match kind {
        Kind::Agent => Color::Cyan,
        Kind::Service => Color::Magenta,
    }
}
/// Sidebar group header: bright kind-colored name, dimmed live/total
/// count so the numbers read as metadata instead of shouting.
pub fn group_header(title: &str, kind: Kind, up: usize, total: usize) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            title.to_string(),
            Style::default()
                .fg(kind_color(kind))
                .add_modifier(ratatui::style::Modifier::BOLD),
        ),
        Span::styled(
            format!(" ({up}/{total})"),
            Style::default().add_modifier(ratatui::style::Modifier::DIM),
        ),
    ])
}
/// (running, total) projects of one kind. `running[i]` mirrors
/// `projects[i]` and reports whether that project is up right now.
pub fn group_counts(projects: &[Project], running: &[bool], kind: Kind) -> (usize, usize) {
    let mut total = 0;
    let mut up = 0;
    for (i, p) in projects.iter().enumerate() {
        if p.kind == kind {
            total += 1;
            if running.get(i).copied().unwrap_or(false) {
                up += 1;
            }
        }
    }
    (up, total)
}
/// First command word for agent rows, so agents with the same status
/// stay distinguishable. Skipped when it is just the default shell.
pub fn agent_tag(p: &Project) -> Option<&str> {
    if is_bare_shell(&p.command) {
        return None;
    }
    p.command.first().map(String::as_str)
}
/// The terminal screen as styled lines, `width` x `height` cells, with
/// adjacent cells of one style merged into a single span.
pub fn screen_lines(screen: &vt100::Screen, width: u16, height: u16) -> Vec<Line<'static>> {
    use ratatui::style::Modifier;
    let mut lines = Vec::with_capacity(height as usize);
    for row in 0..height {
        let mut spans = Vec::with_capacity(8);
        let mut current_text = String::with_capacity(width as usize);
        let mut current_style: Option<Style> = None;
        for col in 0..width {
            let Some(cell) = screen.cell(row, col) else {
                continue;
            };
            if cell.is_wide_continuation() {
                continue;
            }
            let mut style = Style::default()
                .fg(color(cell.fgcolor()))
                .bg(color(cell.bgcolor()));
            if cell.bold() {
                style = style.add_modifier(Modifier::BOLD);
            }
            if cell.dim() {
                style = style.add_modifier(Modifier::DIM);
            }
            if cell.italic() {
                style = style.add_modifier(Modifier::ITALIC);
            }
            if cell.underline() {
                style = style.add_modifier(Modifier::UNDERLINED);
            }
            if cell.inverse() {
                style = style.add_modifier(Modifier::REVERSED);
            }
            let ch = if cell.has_contents() {
                cell.contents()
            } else {
                " "
            };
            if current_style == Some(style) {
                current_text.push_str(ch);
            } else {
                if let Some(prev_style) = current_style {
                    spans.push(Span::styled(
                        std::mem::replace(&mut current_text, String::with_capacity(32)),
                        prev_style,
                    ));
                }
                current_style = Some(style);
                current_text.push_str(ch);
            }
        }
        if let Some(prev_style) = current_style {
            if !current_text.is_empty() {
                spans.push(Span::styled(current_text, prev_style));
            }
        }
        lines.push(Line::from(spans));
    }
    lines
}
pub fn color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(n) => Color::Indexed(n),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}
