use super::*;

#[test]
fn jump_keys() {
    assert_eq!(jump_to(5, '1'), Some(0));
    assert_eq!(jump_to(5, '5'), Some(4));
    assert_eq!(jump_to(5, '6'), None);
    assert_eq!(jump_to(0, '1'), None);
    assert_eq!(jump_to(12, '9'), Some(8));
    assert_eq!(jump_to(12, '0'), None);
    assert_eq!(jump_to(3, 'x'), None);
}
#[test]
fn keyboard() {
    assert_eq!(
        key_bytes(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            false
        ),
        vec![3]
    );
    assert_eq!(
        key_bytes(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), true),
        b"\x1bOA"
    );
}
#[test]
fn sidebar_click_maps_rows() {
    use ratatui::layout::Rect;
    let side = Rect::new(0, 0, 30, 10);
    // header, two projects, header, (none)
    let t = [None, Some(0), Some(1), None, None];
    assert_eq!(sidebar_click(side, 0, &t, 5, 2), Some(0));
    assert_eq!(sidebar_click(side, 0, &t, 5, 3), Some(1));
    assert_eq!(sidebar_click(side, 0, &t, 5, 1), None, "header");
    assert_eq!(sidebar_click(side, 0, &t, 5, 5), None, "placeholder");
    assert_eq!(sidebar_click(side, 0, &t, 5, 0), None, "top border");
    assert_eq!(sidebar_click(side, 0, &t, 0, 2), None, "left border");
    assert_eq!(sidebar_click(side, 0, &t, 29, 2), None, "right border");
    assert_eq!(sidebar_click(side, 0, &t, 5, 9), None, "bottom border");
    // A scrolled list shifts the mapping by the offset.
    assert_eq!(sidebar_click(side, 1, &t, 5, 1), Some(0));
    assert_eq!(sidebar_click(side, 0, &t, 40, 2), None, "outside");
}
#[test]
fn function_keys_encode_xterm() {
    let f = |n, m| key_bytes(KeyEvent::new(KeyCode::F(n), m), false);
    let none = KeyModifiers::NONE;
    assert_eq!(f(1, none), b"\x1bOP");
    assert_eq!(f(4, none), b"\x1bOS");
    assert_eq!(f(5, none), b"\x1b[15~");
    assert_eq!(f(10, none), b"\x1b[21~");
    assert_eq!(f(12, none), b"\x1b[24~");
    assert_eq!(f(1, KeyModifiers::SHIFT), b"\x1b[1;2P");
    assert_eq!(f(5, KeyModifiers::CONTROL), b"\x1b[15;5~");
}
#[test]
fn update_prompt_accepts_only_y() {
    assert_eq!(confirm_key(KeyCode::Char('y')), Some(true));
    assert_eq!(confirm_key(KeyCode::Enter), Some(true));
    assert_eq!(confirm_key(KeyCode::Char(' ')), None);
    assert_eq!(confirm_key(KeyCode::Esc), Some(false));
    assert_eq!(confirm_key(KeyCode::Char('n')), Some(false));
    let text: String = confirm_update_lines("v9.9.9")
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
        .collect();
    assert!(text.contains("v9.9.9") && text.contains(env!("CARGO_PKG_VERSION")));
}
#[test]
fn only_ctrl_q_leaves_terminal_focus() {
    use crossterm::event::KeyModifiers as M;
    assert!(focus_exit_key(KeyCode::Char('q'), M::CONTROL));
    // Esc and Ctrl+G are the agent's, not ours.
    assert!(!focus_exit_key(KeyCode::Esc, M::NONE));
    assert!(!focus_exit_key(KeyCode::Char('g'), M::CONTROL));
    assert!(!focus_exit_key(KeyCode::Char('q'), M::NONE));
    assert!(!focus_exit_key(KeyCode::Char('q'), M::CONTROL | M::SHIFT));
    assert_eq!(key_bytes(KeyEvent::new(KeyCode::Esc, M::NONE), false), [27]);
    assert_eq!(
        key_bytes(KeyEvent::new(KeyCode::Char('g'), M::CONTROL), false),
        [7]
    );
}
#[test]
fn key_bytes_encodes_modifiers() {
    use crossterm::event::KeyModifiers as M;
    let ev = |code, mods| KeyEvent::new(code, mods);
    assert_eq!(key_bytes(ev(KeyCode::Left, M::NONE), false), b"\x1b[D");
    assert_eq!(key_bytes(ev(KeyCode::Left, M::SHIFT), false), b"\x1b[1;2D");
    assert_eq!(key_bytes(ev(KeyCode::Up, M::CONTROL), true), b"\x1b[1;5A");
    assert_eq!(key_bytes(ev(KeyCode::Home, M::SHIFT), false), b"\x1b[1;2H");
    assert_eq!(
        key_bytes(ev(KeyCode::Delete, M::CONTROL), false),
        b"\x1b[3;5~"
    );
    // Alt keeps the ESC-prefix form on top of the base sequence.
    assert_eq!(key_bytes(ev(KeyCode::Up, M::ALT), false), b"\x1b\x1b[A");
}
#[test]
fn line_input_editing() {
    let mut l = LineInput::default();
    for ch in "hello".chars() {
        l.insert(ch);
    }
    assert_eq!(l.as_str(), "hello");
    l.move_left();
    l.move_left();
    l.insert('X');
    assert_eq!(l.as_str(), "helXlo");
    l.backspace();
    assert_eq!(l.as_str(), "hello");
    l.delete();
    assert_eq!(l.as_str(), "helo");
    l.move_home();
    l.insert('S');
    assert_eq!(l.as_str(), "Shelo");
    l.move_end();
    l.insert('!');
    assert_eq!(l.as_str(), "Shelo!");
    l.clear();
    assert!(l.is_empty());
    assert_eq!(l.as_str(), "");
}
#[test]
fn line_input_cursor_parts() {
    let mut l = LineInput::default();
    for ch in "ab".chars() {
        l.insert(ch);
    }
    // Cursor at end: cursor block shows a space.
    assert_eq!(l.render_parts(), ("ab".to_string(), ' ', String::new()));
    l.move_left();
    assert_eq!(l.render_parts(), ("a".to_string(), 'b', String::new()));
    l.move_home();
    assert_eq!(l.render_parts(), (String::new(), 'a', "b".to_string()));
}
#[test]
fn stop_asks_to_confirm() {
    let yes = |c: KeyCode| confirm_key(c);
    assert_eq!(yes(KeyCode::Char('y')), Some(true));
    assert_eq!(yes(KeyCode::Char('Y')), Some(true));
    assert_eq!(yes(KeyCode::Enter), Some(true));
    assert_eq!(yes(KeyCode::Esc), Some(false));
    assert_eq!(yes(KeyCode::Char('n')), Some(false));
    assert_eq!(yes(KeyCode::Char('N')), Some(false));
    assert_eq!(yes(KeyCode::Char('q')), None);
    let prompt = confirm_stop_prompt("web");
    assert!(prompt.contains("web"), "prompt names the session");
}
#[test]
fn wheel_maps_to_scroll_lines() {
    use crossterm::event::MouseEventKind as M;
    assert_eq!(wheel_lines(M::ScrollUp), Some(3));
    assert_eq!(wheel_lines(M::ScrollDown), Some(-3));
    assert_eq!(
        wheel_lines(M::Down(crossterm::event::MouseButton::Left)),
        None
    );
    assert_eq!(wheel_lines(M::Moved), None);
}
#[test]
fn quit_counts_discardable_sessions() {
    let sessions: Vec<Option<u8>> = vec![None, Some(1), None, Some(2)];
    assert_eq!(live_session_count(&sessions), 2);
    let empty: Vec<Option<u8>> = vec![None];
    assert_eq!(live_session_count(&empty), 0);
    let prompt = confirm_quit_prompt(2);
    assert!(prompt.contains('2'), "prompt names the count");
}
#[test]
fn mouse_wheel_sgr_and_x10_encoding() {
    assert_eq!(
        mouse_wheel_seq(vt100::MouseProtocolEncoding::Sgr, true, 1, 1),
        b"\x1b[<64;1;1M"
    );
    assert_eq!(
        mouse_wheel_seq(vt100::MouseProtocolEncoding::Sgr, false, 10, 5),
        b"\x1b[<65;10;5M"
    );
    assert_eq!(
        mouse_wheel_seq(vt100::MouseProtocolEncoding::Default, true, 1, 1),
        vec![0x1b, b'[', b'M', 96, 33, 33]
    );
}
#[test]
fn alt_digit_switches_project_inside_terminal() {
    let alt = KeyModifiers::ALT;
    assert_eq!(focus_switch_key(KeyCode::Char('1'), alt, 3), Some(0));
    assert_eq!(focus_switch_key(KeyCode::Char('3'), alt, 3), Some(2));
    assert_eq!(
        focus_switch_key(KeyCode::Char('4'), alt, 3),
        None,
        "no such row"
    );
    assert_eq!(focus_switch_key(KeyCode::Char('0'), alt, 3), None);
    // Plain digits and other modifiers still reach the session.
    assert_eq!(
        focus_switch_key(KeyCode::Char('1'), KeyModifiers::NONE, 3),
        None
    );
    assert_eq!(
        focus_switch_key(KeyCode::Char('1'), KeyModifiers::CONTROL, 3),
        None
    );
    assert_eq!(
        focus_switch_key(KeyCode::Char('1'), alt | KeyModifiers::SHIFT, 3),
        None
    );
}

#[test]
fn destructive_confirms_accept_y_or_enter() {
    assert_eq!(confirm_key(KeyCode::Char('y')), Some(true));
    assert_eq!(confirm_key(KeyCode::Char('Y')), Some(true));
    assert_eq!(confirm_key(KeyCode::Enter), Some(true));
    assert_eq!(confirm_key(KeyCode::Esc), Some(false));
    // The non-destructive start-all keeps its Enter shortcut.
    assert_eq!(confirm_key(KeyCode::Enter), Some(true));
}

#[test]
fn help_popup_survives_inside_clicks() {
    use ratatui::layout::Rect;
    let area = Rect::new(0, 0, 100, 30);
    let popup = help_popup(area);
    assert!(popup.width <= 66 && popup.height == help_lines().len() as u16 + 2);
    assert!(popup.contains(ratatui::layout::Position::new(popup.x + 2, popup.y + 2)));
}

#[test]
fn mac_option_digits_map_to_their_digit() {
    let typed: Vec<char> = "¡™£¢∞§¶•ª".chars().collect();
    for (i, c) in typed.iter().enumerate() {
        assert_eq!(mac_option_digit(*c), char::from_digit(i as u32 + 1, 10));
    }
    assert_eq!(mac_option_digit('1'), None);
    // Only macOS reads these characters as a switch; elsewhere they are text.
    let got = focus_switch_key(KeyCode::Char('£'), KeyModifiers::NONE, 3);
    assert_eq!(
        got,
        if cfg!(target_os = "macos") {
            Some(2)
        } else {
            None
        }
    );
}

#[test]
fn line_input_word_motion_and_deletion() {
    let mut l = LineInput::default();
    l.set(r"C:\Users\kornah\Work\AgentSquad");
    l.delete_word_back();
    assert_eq!(l.as_str(), r"C:\Users\kornah\Work\");
    l.delete_word_back();
    assert_eq!(l.as_str(), r"C:\Users\kornah\");
    l.move_word_left();
    assert_eq!(l.cursor, 9);
    l.move_word_right();
    assert_eq!(l.cursor, 15);
    l.clear_to_start();
    assert_eq!(l.as_str(), r"\");
}

#[test]
fn hangul_ime_keys_map_to_qwerty_shortcuts() {
    for (ko, en) in [
        ('ㅂ', 'q'),
        ('ㅃ', 'Q'),
        ('ㅈ', 'w'),
        ('ㄷ', 'e'),
        ('ㄱ', 'r'),
        ('ㅅ', 't'),
        ('ㅛ', 'y'),
        ('ㅕ', 'u'),
        ('ㅑ', 'i'),
        ('ㅐ', 'o'),
        ('ㅔ', 'p'),
        ('ㅁ', 'a'),
        ('ㄴ', 's'),
        ('ㅇ', 'd'),
        ('ㄹ', 'f'),
        ('ㅎ', 'g'),
        ('ㅗ', 'h'),
        ('ㅓ', 'j'),
        ('ㅏ', 'k'),
        ('ㅣ', 'l'),
        ('ㅋ', 'z'),
        ('ㅌ', 'x'),
        ('ㅊ', 'c'),
        ('ㅍ', 'v'),
        ('ㅠ', 'b'),
        ('ㅜ', 'n'),
        ('ㅡ', 'm'),
        // Composed syllables extract the last keystroke pressed:
        ('라', 'k'), // ㄹ + ㅏ
        ('란', 's'), // ㄹ + ㅏ + ㄴ
        ('닭', 'r'), // ㄷ + ㅏ + ㄹ + ㄱ
        ('과', 'k'), // ㄱ + ㅗ + ㅏ
    ] {
        assert_eq!(hangul_to_qwerty(ko), Some(en), "failed on {ko}");
    }
    assert_eq!(
        hotkey_code(KeyCode::Char('ㄴ'), KeyModifiers::SHIFT),
        KeyCode::Char('S')
    );
    assert_eq!(
        hotkey_code(KeyCode::Char('ㅡ'), KeyModifiers::SHIFT),
        KeyCode::Char('M')
    );
    assert_eq!(confirm_key(KeyCode::Char('ㅛ')), Some(true));
    assert_eq!(confirm_key(KeyCode::Char('ㅜ')), Some(false));
    assert!(focus_exit_key(KeyCode::Char('ㅂ'), KeyModifiers::CONTROL));
    assert_eq!(
        key_bytes(
            KeyEvent::new(KeyCode::Char('ㅊ'), KeyModifiers::CONTROL),
            false
        ),
        vec![3]
    );
}
#[test]
fn hangul_takes_two_columns() {
    // 4 syllables are 8 columns: the name is cut by columns, not by chars.
    assert_eq!(fit("한글이름", 8), "한글이름");
    assert_eq!(fit("한글이름", 7), "한글이…");
    assert_eq!(fit("한글이름", 1), "");
    assert_eq!(shorten_middle("가나다라마바사", 9), "가…마바사");
    // The cursor stays on screen: left + cursor + right never exceed the width.
    let (left, at, right) = input_window("한글 메시지를 길게", ' ', "", 9);
    assert_eq!((left.as_str(), at, right.as_str()), ("를 길게", ' ', ""));
    assert!(width(&left) + 1 + width(&right) <= 9);
}
#[test]
fn paste_arrives_whole_or_as_typed() {
    // A program in bracketed-paste mode gets one block, line ends untouched.
    assert_eq!(paste_bytes("a\nb", true), b"\x1b[200~a\nb\x1b[201~");
    // Otherwise line ends are Enter, as if typed.
    assert_eq!(paste_bytes("a\r\nb\nc", false), b"a\rb\rc");
    // An end marker in the text cannot close the paste early.
    assert_eq!(
        paste_bytes("x\x1b[201~rm -rf /\n", true),
        b"\x1b[200~xrm -rf /\n\x1b[201~"
    );
}
#[test]
fn edit_key_reports_text_changes() {
    let key = |c, m| KeyEvent::new(c, m);
    let mut i = LineInput::new();
    i.set("one two");
    assert_eq!(
        i.edit_key(key(KeyCode::Left, KeyModifiers::NONE)),
        Some(false)
    );
    assert_eq!(
        i.edit_key(key(KeyCode::Char('w'), KeyModifiers::CONTROL)),
        Some(true)
    );
    assert_eq!(i.as_str(), "one o");
    assert_eq!(
        i.edit_key(key(KeyCode::Char('한'), KeyModifiers::NONE)),
        Some(true)
    );
    // Nothing left to delete at the end; other keys are not editing keys.
    i.move_end();
    assert_eq!(
        i.edit_key(key(KeyCode::Delete, KeyModifiers::NONE)),
        Some(false)
    );
    assert_eq!(i.edit_key(key(KeyCode::Tab, KeyModifiers::NONE)), None);
    assert_eq!(i.edit_key(key(KeyCode::Char('x'), KeyModifiers::ALT)), None);
}
