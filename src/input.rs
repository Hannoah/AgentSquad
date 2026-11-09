use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
use ratatui::{style::Style, text::Span};

/// Single-line editor with a movable cursor for the manager's input fields.
/// Stored as chars so cursor motion never splits UTF-8.
#[derive(Default)]
pub struct LineInput {
    pub text: Vec<char>,
    pub cursor: usize,
}
impl LineInput {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn as_str(&self) -> String {
        self.text.iter().collect()
    }
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }
    pub fn take(&mut self) -> String {
        let out = self.as_str();
        self.clear();
        out
    }
    pub fn insert(&mut self, ch: char) {
        self.text.insert(self.cursor.min(self.text.len()), ch);
        self.cursor = (self.cursor + 1).min(self.text.len());
    }
    /// Replace the text, cursor at the end.
    pub fn set(&mut self, s: &str) {
        self.text = s.chars().collect();
        self.cursor = self.text.len();
    }
    pub fn backspace(&mut self) {
        if self.cursor > 0 && !self.text.is_empty() {
            self.cursor -= 1;
            self.text.remove(self.cursor.min(self.text.len()));
        }
    }
    pub fn delete(&mut self) {
        if self.cursor < self.text.len() {
            self.text.remove(self.cursor);
        }
    }
    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }
    pub fn move_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.text.len());
    }
    pub fn move_home(&mut self) {
        self.cursor = 0;
    }
    pub fn move_end(&mut self) {
        self.cursor = self.text.len();
    }
    /// (before, under-cursor, after) for rendering a block cursor.
    pub fn render_parts(&self) -> (String, char, String) {
        let at = self.text.get(self.cursor).copied().unwrap_or(' ');
        let before: String = self.text[..self.cursor.min(self.text.len())]
            .iter()
            .collect();
        let after: String = if self.cursor < self.text.len() {
            self.text[self.cursor + 1..].iter().collect()
        } else {
            String::new()
        };
        (before, at, after)
    }
}
/// Visible slice of a single-line input: shows the tail end once the
/// text outgrows `width` so the block cursor never scrolls off screen.
pub fn input_window(before: &str, at: char, after: &str, width: usize) -> (String, char, String) {
    let b: Vec<char> = before.chars().collect();
    let a: Vec<char> = after.chars().collect();
    let total = b.len() + 1 + a.len();
    if width == 0 || total <= width {
        return (before.to_string(), at, after.to_string());
    }
    let full: Vec<char> = b
        .iter()
        .chain(std::iter::once(&at))
        .chain(a.iter())
        .copied()
        .collect();
    let cursor = b.len();
    let start = (cursor + 1).saturating_sub(width);
    let end = (start + width).min(full.len());
    let rel = cursor - start;
    (
        full[start..rel + start].iter().collect(),
        full[rel + start],
        full[rel + start + 1..end].iter().collect(),
    )
}
/// Status-bar input row: prompt + editable text with a block cursor + hint.
pub fn input_line(
    prompt: String,
    input: &LineInput,
    hint: String,
    width: usize,
) -> Vec<Span<'static>> {
    let (before, at, after) = input.render_parts();
    let prompt_w = prompt.chars().count();
    let (before, at, after) = input_window(&before, at, &after, width.saturating_sub(prompt_w));
    vec![
        Span::raw(prompt),
        Span::raw(before),
        Span::styled(
            at.to_string(),
            Style::default().add_modifier(ratatui::style::Modifier::REVERSED),
        ),
        Span::raw(after + &hint),
    ]
}
pub fn jump_to(order_len: usize, ch: char) -> Option<usize> {
    if ch.is_ascii_digit() && ch != '0' {
        let n = (ch as u8 - b'0') as usize;
        if n <= order_len.min(9) {
            return Some(n - 1);
        }
    }
    None
}
/// Key outcome while a confirmation modal is pending:
/// `Some(true)` confirms, `Some(false)` cancels, `None` ignores the key.
pub fn confirm_key(code: KeyCode) -> Option<bool> {
    match code {
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => Some(true),
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => Some(false),
        _ => None,
    }
}
/// Destructive confirmations (`stop`, `remove`, `restart`, `quit`) accept
/// only `y`: their hint rows already say `y`, and an `Enter` pressed for
/// something else must not destroy a session or project.
pub fn confirm_destructive_key(code: KeyCode) -> Option<bool> {
    match code {
        KeyCode::Char('y') | KeyCode::Char('Y') => Some(true),
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => Some(false),
        _ => None,
    }
}
/// Only `y` accepts: an update prompt can pop up while the user is about to
/// press Enter, so Enter must not start a download.
pub fn update_key(code: KeyCode) -> Option<bool> {
    match code {
        KeyCode::Char('y') | KeyCode::Char('Y') => Some(true),
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => Some(false),
        _ => None,
    }
}
/// Map a left click to the sidebar's position in `order`, if it landed on
/// a project row (borders, headers and "(none)" placeholders miss).
pub fn sidebar_click(
    side: ratatui::layout::Rect,
    offset: usize,
    targets: &[Option<usize>],
    col: u16,
    row: u16,
) -> Option<usize> {
    if col <= side.x
        || col + 1 >= side.x + side.width
        || row <= side.y
        || row + 1 >= side.y + side.height
    {
        return None;
    }
    targets
        .get((row - side.y - 1) as usize + offset)
        .copied()
        .flatten()
}
/// Mouse wheel notch to scrolled lines: up moves into history.
pub fn wheel_lines(kind: MouseEventKind) -> Option<isize> {
    match kind {
        MouseEventKind::ScrollUp => Some(3),
        MouseEventKind::ScrollDown => Some(-3),
        _ => None,
    }
}
/// Mouse wheel report for a program that asked for mouse events. `col` and
/// `row` are 1-based pane coordinates. SGR is used when the program asked
/// for it; otherwise the classic X10 form, which cannot address past 223.
pub fn mouse_wheel_seq(
    encoding: vt100::MouseProtocolEncoding,
    up: bool,
    col: u16,
    row: u16,
) -> Vec<u8> {
    let btn: u8 = if up { 64 } else { 65 };
    match encoding {
        vt100::MouseProtocolEncoding::Sgr => format!("\x1b[<{btn};{col};{row}M").into_bytes(),
        _ => vec![
            0x1b,
            b'[',
            b'M',
            32 + btn,
            (32 + col.min(223)) as u8,
            (32 + row.min(223)) as u8,
        ],
    }
}
/// CSI cursor key: modified keys always use the `CSI 1 ; param` form,
/// unmodified keys honor application-cursor mode.
pub fn arrow_bytes(letter: u8, app_cursor: bool, shift: bool, ctrl: bool) -> Vec<u8> {
    if shift || ctrl {
        let param = 1 + (shift as u8) + 4 * (ctrl as u8);
        format!("\x1b[1;{param}{}", letter as char).into_bytes()
    } else if app_cursor {
        vec![27, b'O', letter]
    } else {
        vec![27, b'[', letter]
    }
}
/// `CSI n ~` key (Delete/Insert/PageUp/PageDown) with xterm modifiers.
pub fn tilde_bytes(n: u8, shift: bool, ctrl: bool) -> Vec<u8> {
    if shift || ctrl {
        let param = 1 + (shift as u8) + 4 * (ctrl as u8);
        format!("\x1b[{n};{param}~").into_bytes()
    } else {
        format!("\x1b[{n}~").into_bytes()
    }
}
/// Ctrl+Q leaves terminal focus. Every other key, Esc and Ctrl+G included,
/// belongs to the session: agents use Esc to interrupt and rewind.
pub fn focus_exit_key(code: KeyCode, mods: KeyModifiers) -> bool {
    code == KeyCode::Char('q') && mods == KeyModifiers::CONTROL
}
/// Alt+1..9 inside a terminal switches to that project (same numbering as
/// the manager's `1`-`9`). Returns the position in the sidebar order.
pub fn focus_switch_key(code: KeyCode, mods: KeyModifiers, order_len: usize) -> Option<usize> {
    match code {
        KeyCode::Char(c) if mods == KeyModifiers::ALT => jump_to(order_len, c),
        _ => None,
    }
}
pub fn key_bytes(k: KeyEvent, app_cursor: bool) -> Vec<u8> {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let shift = k.modifiers.contains(KeyModifiers::SHIFT);
    let mut bytes = match k.code {
        KeyCode::Char(c) if ctrl && c.is_ascii() => vec![(c.to_ascii_lowercase() as u8) & 0x1f],
        KeyCode::Char(c) => c.to_string().into_bytes(),
        KeyCode::Enter if shift => vec![10],
        KeyCode::Enter => vec![13],
        KeyCode::Backspace => vec![127],
        KeyCode::Tab => vec![9],
        KeyCode::Esc => vec![27],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Up => arrow_bytes(b'A', app_cursor, shift, ctrl),
        KeyCode::Down => arrow_bytes(b'B', app_cursor, shift, ctrl),
        KeyCode::Right => arrow_bytes(b'C', app_cursor, shift, ctrl),
        KeyCode::Left => arrow_bytes(b'D', app_cursor, shift, ctrl),
        KeyCode::Home => arrow_bytes(b'H', false, shift, ctrl),
        KeyCode::End => arrow_bytes(b'F', false, shift, ctrl),
        KeyCode::Delete => tilde_bytes(3, shift, ctrl),
        KeyCode::Insert => tilde_bytes(2, shift, ctrl),
        KeyCode::PageUp => tilde_bytes(5, shift, ctrl),
        KeyCode::PageDown => tilde_bytes(6, shift, ctrl),
        // F1-F4 are SS3 letters; F5-F12 are `CSI n ~` with xterm's gapped numbers.
        KeyCode::F(n @ 1..=4) if !(shift || ctrl) => vec![27, b'O', b'P' + (n - 1)],
        KeyCode::F(n @ 1..=4) => format!(
            "\x1b[1;{}{}",
            1 + shift as u8 + 4 * ctrl as u8,
            (b'P' + (n - 1)) as char
        )
        .into_bytes(),
        KeyCode::F(n @ 5..=12) => {
            let code = [15, 17, 18, 19, 20, 21, 23, 24][(n - 5) as usize];
            tilde_bytes(code, shift, ctrl)
        }
        _ => Vec::new(),
    };
    if k.modifiers.contains(KeyModifiers::ALT) && !bytes.is_empty() {
        bytes.insert(0, 27);
    }
    bytes
}
