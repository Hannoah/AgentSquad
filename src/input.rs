use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
use ratatui::{style::Style, text::Span};

fn is_word_sep(c: char) -> bool {
    c.is_whitespace() || matches!(c, '/' | '\\')
}

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
    /// Move the cursor left by one word or path segment (`Ctrl+Left`).
    pub fn move_word_left(&mut self) {
        let mut i = self.cursor.min(self.text.len());
        while i > 0 && is_word_sep(self.text[i - 1]) {
            i -= 1;
        }
        while i > 0 && !is_word_sep(self.text[i - 1]) {
            i -= 1;
        }
        self.cursor = i;
    }
    /// Move the cursor right by one word or path segment (`Ctrl+Right`).
    pub fn move_word_right(&mut self) {
        let len = self.text.len();
        let mut i = self.cursor.min(len);
        while i < len && is_word_sep(self.text[i]) {
            i += 1;
        }
        while i < len && !is_word_sep(self.text[i]) {
            i += 1;
        }
        self.cursor = i;
    }
    /// Delete the word or path segment before the cursor (`Ctrl+W` / `Ctrl+Backspace`).
    pub fn delete_word_back(&mut self) {
        let end = self.cursor.min(self.text.len());
        self.move_word_left();
        self.text.drain(self.cursor..end);
    }
    /// Delete from the start of the line up to the cursor (`Ctrl+U`).
    pub fn clear_to_start(&mut self) {
        let end = self.cursor.min(self.text.len());
        self.text.drain(..end);
        self.cursor = 0;
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
/// Fit `s` into `width` chars by replacing the middle with `…`, so a long path
/// keeps its drive/root and its last folder.
pub fn shorten_middle(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n <= width || width < 3 {
        return s.to_string();
    }
    let head = (width - 1) / 3;
    let tail = width - 1 - head;
    let start: String = s.chars().take(head).collect();
    let end: String = s.chars().skip(n - tail).collect();
    format!("{start}…{end}")
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
/// Map a Korean 2-beolsik (두벌식) Jamo or composed Hangul syllable to the
/// physical QWERTY key that produced its last keystroke. This lets single-key
/// shortcuts (`j`, `k`, `m`, `s`, `q`, `y`, `n`, …) and `Ctrl+<key>` chords
/// work without toggling the `한/영` IME key.
pub fn hangul_to_qwerty(ch: char) -> Option<char> {
    let mapped = match ch {
        'ㄱ' | '\u{1100}' | '\u{11A8}' | 'ㄺ' | '\u{11B0}' => 'r',
        'ㄲ' | '\u{1101}' | '\u{11A9}' => 'R',
        'ㄴ' | '\u{1102}' | '\u{11AB}' => 's',
        'ㄷ' | '\u{1103}' | '\u{11AE}' => 'e',
        'ㄸ' | '\u{1104}' => 'E',
        'ㄹ' | '\u{1105}' | '\u{11AF}' => 'f',
        'ㅁ' | '\u{1106}' | '\u{11B7}' | 'ㄻ' | '\u{11B1}' => 'a',
        'ㅂ' | '\u{1107}' | '\u{11B8}' | 'ㄼ' | '\u{11B2}' => 'q',
        'ㅃ' | '\u{1108}' => 'Q',
        'ㅅ' | '\u{1109}' | '\u{11BA}' | 'ㄳ' | '\u{11AA}' | 'ㄽ' | '\u{11B3}' | 'ㅄ'
        | '\u{11B9}' => 't',
        'ㅆ' | '\u{110A}' | '\u{11BB}' => 'T',
        'ㅇ' | '\u{110B}' | '\u{11BC}' => 'd',
        'ㅈ' | '\u{110C}' | '\u{11BD}' | 'ㄵ' | '\u{11AC}' => 'w',
        'ㅉ' | '\u{110D}' => 'W',
        'ㅊ' | '\u{110E}' | '\u{11BE}' => 'c',
        'ㅋ' | '\u{110F}' | '\u{11BF}' => 'z',
        'ㅌ' | '\u{1110}' | '\u{11C0}' | 'ㄾ' | '\u{11B4}' => 'x',
        'ㅍ' | '\u{1111}' | '\u{11C1}' | 'ㄿ' | '\u{11B5}' => 'v',
        'ㅎ' | '\u{1112}' | '\u{11C2}' | 'ㄶ' | '\u{11AD}' | 'ㅀ' | '\u{11B6}' => 'g',
        'ㅏ' | '\u{1161}' | 'ㅘ' | '\u{116A}' => 'k',
        'ㅐ' | '\u{1162}' | 'ㅙ' | '\u{116B}' => 'o',
        'ㅑ' | '\u{1163}' => 'i',
        'ㅒ' | '\u{1164}' => 'O',
        'ㅓ' | '\u{1165}' | 'ㅝ' | '\u{116F}' => 'j',
        'ㅔ' | '\u{1166}' | 'ㅞ' | '\u{1170}' => 'p',
        'ㅕ' | '\u{1167}' => 'u',
        'ㅖ' | '\u{1168}' => 'P',
        'ㅗ' | '\u{1169}' => 'h',
        'ㅚ' | '\u{116C}' | 'ㅟ' | '\u{1171}' | 'ㅢ' | '\u{1174}' | 'ㅣ' | '\u{1175}' => 'l',
        'ㅛ' | '\u{116D}' => 'y',
        'ㅜ' | '\u{116E}' => 'n',
        'ㅠ' | '\u{1172}' => 'b',
        'ㅡ' | '\u{1173}' => 'm',
        '가'..='힣' => {
            const JUNG: [char; 21] = [
                'k', 'o', 'i', 'O', 'j', 'p', 'u', 'P', 'h', 'k', 'o', 'l', 'y', 'n', 'j', 'p',
                'l', 'b', 'm', 'l', 'l',
            ];
            const JONG: [char; 27] = [
                'r', 'R', 't', 's', 'w', 'g', 'e', 'f', 'r', 'a', 'q', 't', 'x', 'v', 'g', 'a',
                'q', 't', 't', 'T', 'd', 'w', 'c', 'z', 'x', 'v', 'g',
            ];
            let idx = (ch as u32 - 0xAC00) as usize;
            let jong = idx % 28;
            if jong > 0 {
                JONG[jong - 1]
            } else {
                JUNG[(idx / 28) % 21]
            }
        }
        _ => return None,
    };
    Some(mapped)
}
/// Normalize a key event for shortcut dispatch: maps Hangul 2-beolsik input
/// to its QWERTY equivalent, preserving `SHIFT` as uppercase.
pub fn hotkey_code(code: KeyCode, mods: KeyModifiers) -> KeyCode {
    match code {
        KeyCode::Char(c) => match hangul_to_qwerty(c) {
            Some(q) if mods.contains(KeyModifiers::SHIFT) => KeyCode::Char(q.to_ascii_uppercase()),
            Some(q) => KeyCode::Char(q),
            None => code,
        },
        other => other,
    }
}
pub fn hotkey_event(mut k: KeyEvent) -> KeyEvent {
    k.code = hotkey_code(k.code, k.modifiers);
    k
}
/// Normalize only modified chords (`Ctrl+<key>` / `Alt+<key>`) so prose input
/// modes still type Korean characters while `Ctrl+W`, `Ctrl+U`, `Ctrl+C`, etc.
/// work regardless of IME state.
pub fn normalize_ctrl_alt_key(mut k: KeyEvent) -> KeyEvent {
    if k.modifiers.contains(KeyModifiers::CONTROL) || k.modifiers.contains(KeyModifiers::ALT) {
        k.code = hotkey_code(k.code, k.modifiers);
    }
    k
}
/// Key outcome while a confirmation modal is pending:
/// `Some(true)` confirms, `Some(false)` cancels, `None` ignores the key.
pub fn confirm_key(code: KeyCode) -> Option<bool> {
    match hotkey_code(code, KeyModifiers::NONE) {
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => Some(true),
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => Some(false),
        _ => None,
    }
}
/// Destructive confirmations (`stop`, `remove`, `restart`, `quit`): `Enter`
/// counts as `y` by request, so a stray Enter on an open prompt confirms.
pub fn confirm_destructive_key(code: KeyCode) -> Option<bool> {
    match hotkey_code(code, KeyModifiers::NONE) {
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => Some(true),
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => Some(false),
        _ => None,
    }
}
/// `y` or `Enter` accepts. An update prompt can pop up while the user is about
/// to press Enter, which then starts the download.
pub fn update_key(code: KeyCode) -> Option<bool> {
    match hotkey_code(code, KeyModifiers::NONE) {
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => Some(true),
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => Some(false),
        _ => None,
    }
}
/// Map a left click to the sidebar's position in `order`, if it landed on
/// a project row (borders, headers and "(none)" placeholders miss).
/// Sidebar row target for the Management section's memory monitor entry;
/// never a valid position in `order`.
pub const MEMORY_ROW: usize = usize::MAX;
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
    hotkey_code(code, KeyModifiers::NONE) == KeyCode::Char('q') && mods == KeyModifiers::CONTROL
}
/// What Option+1..9 types on a US Mac keyboard, in digit order. macOS
/// terminals send these characters instead of an Alt modifier unless
/// "Use Option as Meta key" is on.
const MAC_OPTION_DIGITS: [char; 9] = ['¡', '™', '£', '¢', '∞', '§', '¶', '•', 'ª'];
/// The digit an Option+digit character stands for.
pub fn mac_option_digit(c: char) -> Option<char> {
    let i = MAC_OPTION_DIGITS.iter().position(|&m| m == c)?;
    char::from_digit(i as u32 + 1, 10)
}
/// Alt+1..9 inside a terminal switches to that project (same numbering as
/// the manager's `1`-`9`). On macOS the Option+digit characters count too,
/// so it works with the default terminal settings. Returns the position in
/// the sidebar order.
pub fn focus_switch_key(code: KeyCode, mods: KeyModifiers, order_len: usize) -> Option<usize> {
    match code {
        KeyCode::Char(c) if mods == KeyModifiers::ALT => jump_to(order_len, c),
        KeyCode::Char(c) if cfg!(target_os = "macos") && mods == KeyModifiers::NONE => {
            jump_to(order_len, mac_option_digit(c)?)
        }
        _ => None,
    }
}
pub fn key_bytes(k: KeyEvent, app_cursor: bool) -> Vec<u8> {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let shift = k.modifiers.contains(KeyModifiers::SHIFT);
    let mut bytes = match k.code {
        KeyCode::Char(c) if ctrl => match hangul_to_qwerty(c).unwrap_or(c) {
            a if a.is_ascii() => vec![(a.to_ascii_lowercase() as u8) & 0x1f],
            other => other.to_string().into_bytes(),
        },
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
