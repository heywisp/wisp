use wisp_protocol::{AttachKey, AttachKeyCode};

use super::*;

fn key(code: AttachKeyCode) -> AttachKey {
    AttachKey::new(code, false, false, false).unwrap()
}

fn with(code: AttachKeyCode, shift: bool, alt: bool, ctrl: bool) -> AttachKey {
    AttachKey::new(code, shift, alt, ctrl).unwrap()
}

const NORMAL: Modes = Modes {
    application_cursor: false,
    bracketed_paste: false,
};
const APPLICATION: Modes = Modes {
    application_cursor: true,
    bracketed_paste: false,
};

#[test]
fn plain_keys_have_their_terminal_bytes() {
    let cases: [(AttachKeyCode, &[u8]); 9] = [
        (AttachKeyCode::Enter, b"\r"),
        (AttachKeyCode::Tab, b"\t"),
        (AttachKeyCode::Esc, b"\x1b"),
        (AttachKeyCode::Backspace, b"\x7f"),
        (AttachKeyCode::Char { char: ' ' }, b" "),
        (AttachKeyCode::Char { char: 'é' }, "é".as_bytes()),
        (AttachKeyCode::Insert, b"\x1b[2~"),
        (AttachKeyCode::Delete, b"\x1b[3~"),
        (AttachKeyCode::PageDown, b"\x1b[6~"),
    ];
    for (code, bytes) in cases {
        assert_eq!(encode(key(code), NORMAL), bytes, "{code:?}");
    }
}

#[test]
fn arrows_home_and_end_follow_application_cursor_mode() {
    let cases: [(AttachKeyCode, &[u8], &[u8]); 6] = [
        (AttachKeyCode::Up, b"\x1b[A", b"\x1bOA"),
        (AttachKeyCode::Down, b"\x1b[B", b"\x1bOB"),
        (AttachKeyCode::Right, b"\x1b[C", b"\x1bOC"),
        (AttachKeyCode::Left, b"\x1b[D", b"\x1bOD"),
        (AttachKeyCode::Home, b"\x1b[H", b"\x1bOH"),
        (AttachKeyCode::End, b"\x1b[F", b"\x1bOF"),
    ];
    for (code, normal, application) in cases {
        assert_eq!(encode(key(code), NORMAL), normal);
        assert_eq!(encode(key(code), APPLICATION), application);
    }
    let shifted = with(AttachKeyCode::Up, true, false, false);
    assert_eq!(encode(shifted, APPLICATION), b"\x1b[1;2A");
    let ctrl_right = with(AttachKeyCode::Right, false, false, true);
    assert_eq!(encode(ctrl_right, NORMAL), b"\x1b[1;5C");
}

#[test]
fn function_keys_use_xterm_sequences() {
    let expected: [&[u8]; 12] = [
        b"\x1bOP",
        b"\x1bOQ",
        b"\x1bOR",
        b"\x1bOS",
        b"\x1b[15~",
        b"\x1b[17~",
        b"\x1b[18~",
        b"\x1b[19~",
        b"\x1b[20~",
        b"\x1b[21~",
        b"\x1b[23~",
        b"\x1b[24~",
    ];
    for (n, bytes) in (1..=12).zip(expected) {
        assert_eq!(encode(key(AttachKeyCode::F { n }), NORMAL), bytes, "F{n}");
    }
    let shift_f5 = with(AttachKeyCode::F { n: 5 }, true, false, false);
    assert_eq!(encode(shift_f5, NORMAL), b"\x1b[15;2~");
    let ctrl_f1 = with(AttachKeyCode::F { n: 1 }, false, false, true);
    assert_eq!(encode(ctrl_f1, NORMAL), b"\x1b[1;5P");
}

#[test]
fn modifiers_on_characters_and_editing_keys() {
    let ctrl_c = with(AttachKeyCode::Char { char: 'c' }, false, false, true);
    assert_eq!(encode(ctrl_c, NORMAL), b"\x03");
    let alt_x = with(AttachKeyCode::Char { char: 'x' }, false, true, false);
    assert_eq!(encode(alt_x, NORMAL), b"\x1bx");
    let shift_tab = with(AttachKeyCode::Tab, true, false, false);
    assert_eq!(encode(shift_tab, NORMAL), b"\x1b[Z");
    let ctrl_backspace = with(AttachKeyCode::Backspace, false, false, true);
    assert_eq!(encode(ctrl_backspace, NORMAL), b"\x08");
    let alt_enter = with(AttachKeyCode::Enter, false, true, false);
    assert_eq!(encode(alt_enter, NORMAL), b"\x1b\r");
}

#[test]
fn a_multi_line_paste_is_bracketed_only_when_the_program_asked() {
    let bracketed = Modes {
        application_cursor: false,
        bracketed_paste: true,
    };
    assert_eq!(paste("a\nb", bracketed), b"\x1b[200~a\nb\x1b[201~");
    assert_eq!(paste("one line", bracketed), b"one line");
    assert_eq!(paste("a\nb", NORMAL), b"a\nb");
}

#[test]
fn a_user_paste_sends_returns_or_a_bracket_that_cannot_end_early() {
    assert_eq!(user_paste("a\nb\r\nc", NORMAL), b"a\rb\rc");
    let bracketed = Modes {
        application_cursor: false,
        bracketed_paste: true,
    };
    assert_eq!(
        user_paste("x\x1b[201~y\nz", bracketed),
        b"\x1b[200~xy\nz\x1b[201~"
    );
    assert_eq!(user_paste("q\x1b[201~", NORMAL), b"q");
}

#[test]
fn only_some_keys_reach_a_pipe() {
    assert_eq!(
        Key::Named(NamedKey::Enter).pipe_bytes(),
        Some(b"\n".to_vec())
    );
    assert_eq!(Key::Ctrl('c').pipe_bytes(), Some(vec![3]));
    assert_eq!(Key::Named(NamedKey::Up).pipe_bytes(), None);
    assert_eq!(Key::Function(1).pipe_bytes(), None);
    assert_eq!(Key::Eof.pipe_bytes(), None);
}
