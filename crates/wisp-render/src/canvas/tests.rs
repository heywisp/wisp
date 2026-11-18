use super::*;
use crate::theme::Token;

fn spans(lines: &[StyledLine]) -> Vec<Vec<(&str, Look)>> {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|s| (s.text.as_str(), s.look))
                .collect()
        })
        .collect()
}

#[test]
fn every_pattern_round_trips_in_unicode_dot_order() {
    assert_eq!(glyph(0), ' ');
    assert_eq!(
        [
            bit(0, 0),
            bit(0, 1),
            bit(0, 2),
            bit(1, 0),
            bit(1, 1),
            bit(1, 2),
            bit(0, 3),
            bit(1, 3)
        ],
        [0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80]
    );
    assert_eq!(bit(2, 0) | bit(0, 4), 0);
    for bits in 0..=u8::MAX {
        let rebuilt = dots(bits).fold(0, |acc, (x, y)| acc | bit(x, y));
        assert_eq!(rebuilt, bits);
        assert_eq!(dots(bits).count(), bits.count_ones() as usize);
        if bits != 0 {
            assert_eq!(u32::from(glyph(bits)), 0x2800 + u32::from(bits));
            assert_eq!(braille(glyph(bits)), Some(bits));
        }
    }
    assert_eq!(braille('*'), None);
    assert_eq!(braille('\u{2900}'), None);
}

#[test]
fn the_higher_rank_keeps_the_look_and_text_wins_over_bits() {
    let (line, light, dot) = (
        Look::fg(Token::Line),
        Look::fg(Token::Accent),
        Look::fg(Token::Ok),
    );
    let mut canvas = Canvas::new(3, 1);
    canvas.bits((0, 0), 0x01, 0, line);
    canvas.bits((0, 0), 0x08, 1, light);
    canvas.bits((0, 0), 0x02, 0, line);
    canvas.bits((1, 0), 0x40, 1, light);
    canvas.bits((1, 0), 0x80, 1, line);
    canvas.text((2, 0), "o", dot);
    canvas.bits((2, 0), 0x01, 9, line);
    canvas.bits((9, 9), 0x01, 0, line);
    canvas.text((9, 9), "x", dot);
    assert_eq!(
        canvas.cell_mut((0, 0)).copied().flatten(),
        Some(Cell {
            mark: Mark::Bits {
                bits: 0x0B,
                rank: 1
            },
            look: light
        })
    );
    assert_eq!(
        spans(&canvas.lines()),
        [vec![("\u{280B}", light), ("\u{28C0}", line), ("o", dot)]]
    );
    canvas.text((0, 0), "@", dot);
    assert_eq!(
        spans(&canvas.lines()),
        [vec![("@", dot), ("\u{28C0}", line), ("o", dot)]]
    );
}

#[test]
fn a_crop_keeps_its_window_and_lines_merge_runs_of_one_look() {
    let (a, b) = (Look::fg(Token::Line), Look::fg(Token::Ok));
    let mut canvas = Canvas::new(6, 3);
    canvas.text((0, 0), "x", a);
    canvas.text((3, 0), "y", a);
    canvas.text((4, 0), "z", a);
    canvas.bits((1, 1), 0x01, 0, a);
    canvas.text((2, 1), "q", b);
    canvas.text((5, 2), "r", b);
    assert_eq!(
        spans(&canvas.lines()),
        [
            vec![("x", a), ("  ", Look::default()), ("yz", a)],
            vec![(" ", Look::default()), ("\u{2801}", a), ("q", b)],
            vec![("     ", Look::default()), ("r", b)],
        ]
    );
    let window = canvas.crop((1, 1), 3, 5);
    assert_eq!(
        spans(&window.lines()),
        [vec![("\u{2801}", a), ("q", b)], vec![]]
    );
    assert_eq!(canvas.crop((1, 0), 2, 1).lines()[0].spans, Vec::new());
    assert!(canvas.crop((9, 9), 3, 3).lines().is_empty());
    assert_eq!(Canvas::new(4, 2).lines().len(), 2);
}
