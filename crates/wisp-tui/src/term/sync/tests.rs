use super::*;

#[test]
fn a_frame_is_wrapped_in_dec_2026_with_the_cursor_hidden() {
    let out = frame(b"row", CursorAfter::Hidden);
    assert_eq!(out, b"\x1b[?2026h\x1b[?25lrow\x1b[?2026l");
    let shown = frame(b"row", CursorAfter::Shown);
    assert_eq!(shown, b"\x1b[?2026h\x1b[?25lrow\x1b[?25h\x1b[?2026l");
    assert_eq!(begin(), "\x1b[?2026h");
    assert_eq!(end(), "\x1b[?2026l");
}
