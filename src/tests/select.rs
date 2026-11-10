use crate::select::*;

#[test]
fn a_click_selects_nothing_and_a_drag_reads_like_text() {
    let mut s = Selection::new(0, (2, 5));
    s.drag_to((2, 5));
    assert!(!s.moved, "same cell is still a click");
    s.drag_to((4, 3));
    assert!(s.moved);
    assert_eq!(s.span(), ((2, 5), (4, 3)));
    // First row from its start cell, whole rows between, last row up to its end.
    assert!(!s.contains(2, 4) && s.contains(2, 5) && s.contains(2, 79));
    assert!(s.contains(3, 0) && s.contains(3, 79));
    assert!(s.contains(4, 3) && !s.contains(4, 4));
    assert!(!s.contains(1, 10) && !s.contains(5, 0));
}

#[test]
fn dragging_backwards_selects_the_same_cells() {
    let mut s = Selection::new(0, (4, 3));
    s.drag_to((2, 5));
    assert_eq!(s.span(), ((2, 5), (4, 3)));
    assert!(s.contains(3, 40));
}

#[test]
fn screen_text_between_cells_matches_the_selection() {
    // The copy path hands the span to vt100 with an exclusive end column.
    use std::num::NonZero;
    let mut p = vt100::Parser::new(NonZero::new(5).unwrap(), NonZero::new(20).unwrap(), 0);
    p.process(b"hello world\r\nsecond line\r\nthird");
    let mut s = Selection::new(0, (0, 6));
    s.drag_to((1, 5));
    let ((r1, c1), (r2, c2)) = s.span();
    assert_eq!(
        p.screen().contents_between(r1, c1, r2, c2 + 1),
        "world\nsecond"
    );
    let mut one = Selection::new(0, (0, 0));
    one.drag_to((0, 4));
    let ((r1, c1), (r2, c2)) = one.span();
    assert_eq!(p.screen().contents_between(r1, c1, r2, c2 + 1), "hello");
}

#[test]
#[ignore = "overwrites the real system clipboard"]
fn clipboard_roundtrip() {
    let text = "squad clipboard test ✓ 한글";
    crate::sys::set_clipboard(text).unwrap();
    #[cfg(windows)]
    let out = super::ps_stdout("[Console]::OutputEncoding=[Text.Encoding]::UTF8; Get-Clipboard");
    #[cfg(not(windows))]
    let out = String::from_utf8(
        std::process::Command::new("pbpaste")
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(out.trim(), text);
}
