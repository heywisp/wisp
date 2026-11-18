use ratatui_core::style::{Modifier, Style};
use ratatui_core::text::Span;

use super::*;

fn rows_on(bytes: &[u8], rows: u16, cols: u16) -> Vec<String> {
    let mut parser = vt100::Parser::new(
        std::num::NonZeroU16::new(rows).unwrap_or(std::num::NonZeroU16::MIN),
        std::num::NonZeroU16::new(cols).unwrap_or(std::num::NonZeroU16::MIN),
        100,
    );
    parser.process(b"$ shell line\r\n");
    parser.process(bytes);
    let screen = parser.screen();
    screen
        .rows(0, cols)
        .map(|r| r.trim_end().to_owned())
        .collect()
}

#[test]
fn rows_print_in_order_from_the_cursor_with_crlf_and_no_absolute_moves() {
    let lines = vec![
        Line::from(vec![
            Span::styled("red", Style::new().add_modifier(Modifier::BOLD)),
            Span::raw(" text"),
        ]),
        Line::from("漢字 wide"),
        Line::from(""),
        Line::from("last"),
    ];
    let bytes = encode(&lines, 20).unwrap_or_default();
    let text = String::from_utf8_lossy(&bytes);
    assert!(!text.contains('H'), "{text:?}");
    assert!(text.starts_with("\x1b[?7l"), "{text:?}");
    assert!(text.ends_with("\x1b[?7h"), "{text:?}");
    assert!(text.contains("\x1b[?2026h") && text.contains("\x1b[?2026l"));
    let shown = rows_on(&bytes, 8, 20);
    assert_eq!(
        shown[..5],
        ["$ shell line", "red text", "漢字 wide", "", "last"]
    );
}

#[test]
fn control_characters_never_reach_the_terminal() {
    let lines = vec![Line::from("a\u{1b}]52;c;Zm9v\u{7}b\u{9b}31m")];
    let bytes = encode(&lines, 40).unwrap_or_default();
    let text = String::from_utf8_lossy(&bytes);
    assert!(!text.contains("\x1b]"), "{text:?}");
    assert!(!text.contains('\u{7}'), "{text:?}");
    assert!(!text.contains('\u{9b}'), "{text:?}");
}

#[test]
fn long_transcripts_are_written_in_synchronized_chunks() {
    let lines: Vec<Line<'_>> = (0..CHUNK_ROWS * 2 + 1)
        .map(|i| Line::from(format!("row {i}")))
        .collect();
    let bytes = encode(&lines, 20).unwrap_or_default();
    let text = String::from_utf8_lossy(&bytes);
    assert_eq!(text.matches("\x1b[?2026h").count(), 3);
    assert_eq!(text.matches("\r\n").count(), lines.len());
}

#[test]
fn a_row_is_sanitised_here_whatever_put_controls_in_its_cells() {
    let mut row = Buffer::empty(Rect::new(0, 0, 6, 1));
    for (x, symbol) in ["a", "\u{1b}", "]", "\u{7}", "\u{9b}", "z"]
        .iter()
        .enumerate()
    {
        if let Some(cell) = row.cell_mut((u16::try_from(x).unwrap_or(0), 0)) {
            cell.set_symbol(symbol);
        }
    }
    let bytes = cells_bytes(row).unwrap_or_default();
    let text = String::from_utf8_lossy(&bytes);
    assert!(!text.contains("\u{1b}]"), "{text:?}");
    assert!(!text.contains('\u{7}'), "{text:?}");
    assert!(!text.contains('\u{9b}'), "{text:?}");
    assert_eq!(
        text.matches(crate::term::screen::REPLACEMENT).count(),
        3,
        "{text:?}"
    );
}
