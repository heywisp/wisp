use std::num::NonZeroU16;

use ratatui_core::layout::Rect;
use ratatui_core::style::{Color, Modifier, Style};

use super::*;
use crate::term::picture::{Encoder, FIRST_ID, Tile};
use crate::testing::Rng;

fn parser(rows: u16, cols: u16) -> vt100::Parser {
    crossterm::style::Colored::set_ansi_color_disabled(false);
    vt100::Parser::new(
        NonZeroU16::new(rows).unwrap(),
        NonZeroU16::new(cols).unwrap(),
        0,
    )
}

fn send(painter: &mut Painter, view: &Buffer) -> Option<Vec<u8>> {
    send_linked(painter, view, None)
}

fn send_linked(painter: &mut Painter, view: &Buffer, link: Option<&Link>) -> Option<Vec<u8>> {
    let plan = painter.plan(view, link).unwrap()?;
    let bytes = plan.bytes().to_vec();
    painter.sent(plan, Duration::ZERO);
    Some(bytes)
}

fn rows(vt: &vt100::Parser, cols: u16) -> Vec<String> {
    vt.screen()
        .rows(0, cols)
        .map(|r| r.trim_end().to_owned())
        .collect()
}

fn texts(view: &Buffer) -> Vec<String> {
    let width = usize::from(view.area.width);
    view.content
        .chunks(width)
        .map(|row| {
            let mut out = String::new();
            let mut x = 0;
            while let Some(cell) = row.get(x) {
                out.push_str(cell.symbol());
                x += usize::from(cell.symbol().cell_width().max(1));
            }
            out
        })
        .collect()
}

#[test]
fn a_full_repaint_covers_every_cell_without_erasing() {
    let mut view = Buffer::empty(Rect::new(0, 0, 10, 3));
    view.set_string(0, 0, "top", Style::default());
    view.set_string(0, 2, "bottom", Style::default());
    let bytes = send(&mut Painter::new(false), &view).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        !text.contains("\x1b[2K") && !text.contains("\x1b[J"),
        "{text:?}"
    );
    assert!(text.starts_with("\x1b[?7l"), "{text:?}");
    assert!(text.ends_with("\x1b[?7h"), "{text:?}");
    let mut vt = parser(3, 10);
    vt.process(b"xxxxxxxxxx\r\nxxxxxxxxxx\r\nxxxxxxxxxx");
    vt.process(&bytes);
    assert_eq!(rows(&vt, 10), ["top", "", "bottom"]);
}

#[test]
fn an_unchanged_view_writes_nothing_and_a_change_writes_only_its_cells() {
    let mut painter = Painter::new(false);
    let mut view = Buffer::empty(Rect::new(0, 0, 40, 10));
    view.set_string(
        0,
        0,
        "a long first row that stays the same",
        Style::default(),
    );
    send(&mut painter, &view).unwrap();
    assert!(painter.plan(&view, None).unwrap().is_none());
    view.set_string(0, 9, "z", Style::default());
    let bytes = send(&mut painter, &view).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(!text.contains("first"), "{text:?}");
    assert!(text.contains('z'), "{text:?}");
    assert_eq!(painter.full_repaints(), 1);
    assert_eq!(painter.painted(), 2);
}

#[test]
fn invalidating_or_a_new_size_repaints_in_full_and_the_switch_always_does() {
    let mut painter = Painter::new(false);
    let view = Buffer::empty(Rect::new(0, 0, 8, 2));
    send(&mut painter, &view).unwrap();
    painter.invalidate();
    assert_eq!(
        painter.plan(&view, None).unwrap().unwrap().repaint(),
        Repaint::Full
    );
    send(&mut painter, &view).unwrap();
    let bigger = Buffer::empty(Rect::new(0, 0, 9, 2));
    assert_eq!(
        painter.plan(&bigger, None).unwrap().unwrap().repaint(),
        Repaint::Full
    );
    let mut always = Painter::new(true);
    send(&mut always, &view).unwrap();
    assert_eq!(
        always.plan(&view, None).unwrap().unwrap().repaint(),
        Repaint::Full
    );
    let link = Link::new(1, 0..4, "https://claude.ai/oauth/authorize?state=A");
    let moved = Link::new(1, 1..5, "https://claude.ai/oauth/authorize?state=A");
    let other = Link::new(1, 1..5, "https://claude.ai/oauth/authorize?state=B");
    send(&mut painter, &bigger).unwrap();
    for next in [&link, &moved, &other, &None] {
        assert_eq!(
            painter
                .plan(&bigger, next.as_ref())
                .unwrap()
                .unwrap()
                .repaint(),
            Repaint::Full
        );
        send_linked(&mut painter, &bigger, next.as_ref()).unwrap();
        assert!(painter.plan(&bigger, next.as_ref()).unwrap().is_none());
    }
    send_linked(&mut painter, &bigger, link.as_ref()).unwrap();
    let mut inside = bigger.clone();
    inside.set_string(2, 1, "x", Style::default());
    assert_eq!(
        painter
            .plan(&inside, link.as_ref())
            .unwrap()
            .unwrap()
            .repaint(),
        Repaint::Full
    );
    let mut outside = bigger.clone();
    outside.set_string(6, 1, "x", Style::default());
    assert_eq!(
        painter
            .plan(&outside, link.as_ref())
            .unwrap()
            .unwrap()
            .repaint(),
        Repaint::Changed
    );
}

const PAGE: &str = "https://claude.ai/oauth/authorize?code=true&state=URL_CANARY;x=1";

#[test]
fn the_link_text_is_written_inside_one_osc_8_link_and_a_hostile_url_gets_none() {
    for width in [40u16, 80, 120] {
        let mut view = Buffer::empty(Rect::new(0, 0, width, 3));
        view.set_string(0, 0, "top", Style::default());
        view.set_string(3, 1, "\u{2192} claude.ai/\u{2026}", Style::default());
        view.set_string(0, 2, "keys", Style::default());
        let link = Link::new(1, 3..16, PAGE);
        let bytes = send_linked(&mut Painter::new(false), &view, link.as_ref()).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        let open = format!("\x1b]8;id=wisp-login;{PAGE}\x1b\\");
        assert_eq!(text.matches(&open).count(), 1, "{text:?}");
        assert_eq!(text.matches("\x1b]8;;\x1b\\").count(), 1, "{text:?}");
        let (_, after) = text.split_once(&open).unwrap();
        let (inside, rest) = after.split_once("\x1b]8;;\x1b\\").unwrap();
        assert!(inside.contains("claude.ai/"), "{inside:?}");
        assert!(
            !inside.contains("keys") && rest.contains("keys"),
            "{text:?}"
        );
        let mut vt = parser(3, width);
        vt.process(&bytes);
        assert_eq!(rows(&vt, width)[1], "   \u{2192} claude.ai/\u{2026}");
    }
    for hostile in [
        "https://claude.ai/\u{1b}]0;x",
        "https://claude.ai/\u{1b}\\",
        "https://claude.ai/\u{7}",
        "https://claude.ai/\u{9c}",
        "https://claude.ai/\u{7f}",
        "https://claude.ai/a\nb",
        "http://claude.ai/oauth/authorize",
        "javascript:alert(1)",
    ] {
        assert!(Link::new(1, 3..16, hostile).is_none(), "{hostile:?}");
    }
    let link = Link::new(1, 3..16, PAGE);
    let mut painter = Painter::new(false);
    let view = Buffer::empty(Rect::new(0, 0, 20, 3));
    let plan = painter.plan(&view, link.as_ref()).unwrap().unwrap();
    assert!(!format!("{plan:?}").contains("claude") && !format!("{plan:?}").contains("CANARY"));
    painter.sent(plan, Duration::ZERO);
    let shown = format!("{painter:?} {link:?}");
    assert!(
        !shown.contains("claude") && !shown.contains("CANARY"),
        "{shown}"
    );
}

#[test]
fn control_characters_are_replaced_and_wide_symbols_keep_their_cells() {
    let mut view = Buffer::empty(Rect::new(0, 0, 6, 1));
    view.set_string(0, 0, "a\u{1b}]0;x\u{7}", Style::default());
    let bytes = send(&mut Painter::new(false), &view).unwrap();
    assert!(!bytes.windows(2).any(|w| w == b"\x1b]"), "{bytes:?}");
    assert!(!bytes.contains(&7));
    let mut wide = Buffer::empty(Rect::new(0, 0, 6, 1));
    wide.set_string(0, 0, "a\u{4e16}b", Style::default());
    let mut vt = parser(1, 6);
    vt.process(&send(&mut Painter::new(false), &wide).unwrap());
    assert_eq!(vt.screen().contents().trim_end(), "a\u{4e16}b");
}

#[test]
fn the_cell_pass_replaces_c0_c1_del_and_esc() {
    for code in (0u32..0x20).chain([0x7f]).chain(0x80..0xa0) {
        let c = char::from_u32(code).unwrap();
        let mut cell = Cell::new("x");
        cell.set_symbol(&format!("a{c}"));
        assert!(sanitize_cell(&mut cell), "{code:#x}");
        assert_eq!(cell.symbol(), REPLACEMENT);
    }
    let mut cell = Cell::new("\u{4e2d}");
    assert!(!sanitize_cell(&mut cell));
}

#[test]
fn a_symbol_that_overflows_its_row_is_replaced() {
    for symbol in ["\u{4e2d}", "abcdef", "\u{1f1fa}\u{1f1f8}"] {
        let mut view = Buffer::empty(Rect::new(0, 0, 4, 1));
        view.content[3].set_symbol(symbol).set_bg(Color::Blue);
        let mut painter = Painter::new(false);
        send(&mut painter, &view).unwrap();
        assert_eq!(painter.shown().unwrap().content[3].symbol(), REPLACEMENT);
        let mut next = Buffer::empty(Rect::new(0, 0, 4, 1));
        next.content[3].set_symbol("a");
        assert!(send(&mut painter, &next).is_some());
    }
}

const SYMBOLS: [&str; 9] = [
    "a",
    "b",
    " ",
    "\u{4e2d}",
    "\u{2836}",
    "\u{1b}",
    "\u{9b}",
    "e\u{301}",
    "\u{1f600}",
];

#[test]
fn successive_diffs_always_leave_the_screen_equal_to_the_last_view() {
    for seed in 1..=200u64 {
        let mut rng = Rng(seed.wrapping_mul(0x2545_f491_4f6c_dd1d) | 1);
        let (cols, rows_n) = (12u16, 5u16);
        let mut vt = parser(rows_n, cols);
        let mut painter = Painter::new(false);
        let mut view = Buffer::empty(Rect::new(0, 0, cols, rows_n));
        for _ in 0..12 {
            for _ in 0..rng.below(20) {
                let x = u16::try_from(rng.below(usize::from(cols))).unwrap();
                let y = u16::try_from(rng.below(usize::from(rows_n))).unwrap();
                let symbol = SYMBOLS[rng.below(9)];
                let style = if rng.below(2) == 0 {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default().add_modifier(Modifier::ITALIC)
                };
                view.set_string(x, y, symbol, style);
            }
            if rng.below(5) == 0 {
                painter.invalidate();
            }
            if let Some(bytes) = send(&mut painter, &view) {
                assert!(!bytes.windows(2).any(|w| w == b"\x1b]"), "seed {seed}");
                vt.process(&bytes);
            }
            let want = texts(painter.shown().unwrap());
            let got: Vec<String> = rows(&vt, cols)
                .into_iter()
                .zip(&want)
                .map(|(g, w)| format!("{g:<width$}", width = w.chars().count().max(g.len())))
                .collect();
            for (g, w) in got.iter().zip(&want) {
                assert_eq!(g.trim_end(), w.trim_end(), "seed {seed}");
            }
        }
    }
}

fn core(fg: Color) -> Cell {
    let mut cell = Cell::default();
    cell.set_char('\u{2836}');
    cell.fg = fg;
    cell
}

#[test]
fn a_patch_writes_one_cell_onto_the_shown_frame() {
    let size = Size {
        width: 20,
        height: 4,
    };
    let at = Position { x: 2, y: 3 };
    let mut painter = Painter::new(false);
    assert!(painter.patch(size, at, core(Color::Red)).is_none());
    let mut view = Buffer::empty(Rect::new(0, 0, 20, 4));
    view.set_string(0, 0, "hello", Style::default());
    view[at] = core(Color::Blue);
    let mut vt = parser(4, 20);
    vt.process(&send(&mut painter, &view).unwrap());
    assert!(matches!(
        painter.patch(size, at, core(Color::Blue)),
        Some(Ok(None))
    ));
    let plan = painter
        .patch(size, at, core(Color::Magenta))
        .unwrap()
        .unwrap()
        .unwrap();
    let bytes = plan.bytes().to_vec();
    assert_eq!(
        String::from_utf8_lossy(&bytes),
        "\x1b[4;3H\x1b[38;5;5m\u{2836}\x1b[39m"
    );
    assert_eq!(plan.repaint(), Repaint::Changed);
    painter.sent(plan, Duration::ZERO);
    vt.process(&bytes);
    let cell = vt.screen().cell(3, 2).unwrap();
    assert_eq!(cell.contents(), "\u{2836}");
    let mut linked = Painter::new(false);
    view[Position { x: 9, y: 3 }] = core(Color::Blue);
    send_linked(&mut linked, &view, Link::new(3, 0..4, PAGE).as_ref()).unwrap();
    assert!(linked.patch(size, at, core(Color::Magenta)).is_none());
    assert!(matches!(
        linked.patch(size, Position { x: 9, y: 3 }, core(Color::Magenta)),
        Some(Ok(Some(_)))
    ));
    assert_eq!(cell.fgcolor(), vt100::Color::Idx(5));
    assert_eq!(rows(&vt, 20)[0], "hello");
    assert_eq!(painter.shown().map(|b| b[at].fg), Some(Color::Magenta));
    let mut bold = core(Color::Green);
    bold.modifier = Modifier::BOLD;
    let plan = painter.patch(size, at, bold).unwrap().unwrap().unwrap();
    assert!(plan.bytes().starts_with(b"\x1b[?7l"));
    painter.sent(plan, Duration::ZERO);
    assert!(painter.shown().is_some());
    assert!(
        painter
            .patch(
                Size {
                    width: 21,
                    height: 4
                },
                at,
                core(Color::Red)
            )
            .is_none()
    );
    let mut other = core(Color::Red);
    other.set_char('\u{2026}');
    assert!(painter.patch(size, at, other).is_none());
    assert!(
        painter
            .patch(size, Position { x: 0, y: 3 }, core(Color::Red))
            .is_none()
    );
    let mut control = core(Color::Red);
    control.set_symbol("\x1b");
    assert!(painter.patch(size, at, control).is_none());
    let mut plain = Painter::new(false);
    let mut marked = Buffer::empty(Rect::new(0, 0, 20, 4));
    marked[Position { x: 5, y: 0 }].set_symbol(REPLACEMENT);
    send(&mut plain, &marked);
    let mut control = core(Color::Red);
    control.set_symbol("\x07");
    let plan = plain
        .patch(size, Position { x: 5, y: 0 }, control)
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!plan.bytes().contains(&0x07), "{:?}", plan.bytes());
    assert!(plan.bytes().ends_with(b"?\x1b[39m"), "{:?}", plan.bytes());
    let mut always = Painter::new(true);
    send(&mut always, &view);
    assert!(always.patch(size, at, core(Color::Red)).is_none());
}

#[test]
fn a_parked_cursor_goes_back_to_its_cell_after_a_patch() {
    let size = Size {
        width: 20,
        height: 4,
    };
    let at = Position { x: 2, y: 3 };
    let home = Position { x: 0, y: 1 };
    let mut painter = Painter::new(false);
    let mut view = Buffer::empty(Rect::new(0, 0, 20, 4));
    view[at] = core(Color::Blue);
    painter.park(Some(home));
    let mut vt = parser(4, 20);
    vt.process(&send(&mut painter, &view).unwrap());
    assert_eq!(vt.screen().cursor_position(), (1, 0));
    assert_eq!(painter.parked(), Some(home));
    let plan = painter
        .patch(size, at, core(Color::Magenta))
        .unwrap()
        .unwrap()
        .unwrap();
    let bytes = plan.bytes().to_vec();
    painter.sent(plan, Duration::ZERO);
    vt.process(&bytes);
    assert_eq!(vt.screen().cursor_position(), (1, 0));
    assert_eq!(painter.parked(), Some(home));
    let shown = painter.shown().unwrap().clone();
    assert!(send(&mut painter, &shown).is_none());
}

fn tile(x: u16, cells: u16, hash: u64) -> Tile {
    Tile {
        at: Position { x, y: 1 },
        cells,
        hash,
    }
}

fn pictured(painter: &mut Painter, view: &Buffer, list: &[Tile]) -> Option<String> {
    pictured_by(painter, view, list, Encoder::Kitty)
}

fn pictured_by(
    painter: &mut Painter,
    view: &Buffer,
    list: &[Tile],
    encoder: Encoder<'_>,
) -> Option<String> {
    let pixels = |_| Some(vec![0; 4]);
    let tiles = Tiles {
        list: list.to_vec(),
        cell: (1, 1),
        encoder,
        pixels: &pixels,
    };
    let plan = painter.plan_tiles(view, None, &tiles).unwrap()?;
    let text = String::from_utf8_lossy(plan.bytes()).into_owned();
    painter.sent(plan, Duration::ZERO);
    Some(text)
}

fn ids(text: &str, action: &str) -> Vec<u32> {
    text.split("\x1b_G")
        .filter(|part| part.starts_with(action))
        .filter_map(|part| part.split_once(",i=")?.1.split([',', ';', '\x1b']).next())
        .map(|id| id.parse().unwrap())
        .collect()
}

#[test]
fn an_unchanged_frame_sends_nothing_and_a_tile_only_change_sends_just_that_tile() {
    let mut painter = Painter::new(false);
    let mut view = Buffer::empty(Rect::new(0, 0, 10, 3));
    view.set_string(0, 0, "box", Style::default());
    let (a, b) = (tile(2, 2, 1), tile(6, 2, 1));
    let first = pictured(&mut painter, &view, &[a, b]).unwrap();
    assert_eq!(ids(&first, "a=T"), [FIRST_ID, FIRST_ID + 1]);
    assert!(painter.has_tiles());
    assert!(pictured(&mut painter, &view, &[a, b]).is_none());
    let a = Tile { hash: 2, ..a };
    let changed = pictured(&mut painter, &view, &[a, b]).unwrap();
    assert_eq!(ids(&changed, "a=T"), [FIRST_ID + 2]);
    assert_eq!(ids(&changed, "a=d"), [FIRST_ID]);
    assert!(!changed.contains("box"), "{changed:?}");
    view.set_string(7, 1, "x", Style::default());
    let written = pictured(&mut painter, &view, &[a, b]).unwrap();
    assert_eq!(ids(&written, "a=T"), [FIRST_ID + 3], "its cell was written");
    assert_eq!(ids(&written, "a=d"), [FIRST_ID + 1]);
}

#[test]
fn a_full_repaint_resends_every_tile_and_cells_left_bare_are_written_before_the_deletes() {
    let mut painter = Painter::new(false);
    let view = Buffer::empty(Rect::new(0, 0, 10, 3));
    let (a, b) = (tile(2, 2, 1), tile(6, 2, 1));
    pictured(&mut painter, &view, &[a, b]).unwrap();
    painter.invalidate();
    let full = pictured(&mut painter, &view, &[a, b]).unwrap();
    assert_eq!(ids(&full, "a=T"), [FIRST_ID + 2, FIRST_ID + 3]);
    assert_eq!(ids(&full, "a=d"), [FIRST_ID, FIRST_ID + 1]);
    let shrunk = Tile { cells: 1, ..a };
    let unsent = painter
        .plan_tiles(
            &view,
            None,
            &Tiles {
                list: vec![shrunk],
                cell: (0, 0),
                encoder: Encoder::Kitty,
                pixels: &|_| None,
            },
        )
        .unwrap();
    assert!(unsent.is_some());
    let bare = pictured(&mut painter, &view, &[shrunk]).unwrap();
    assert_eq!(
        ids(&bare, "a=T"),
        [FIRST_ID + 4],
        "the unsent plan kept the slots"
    );
    assert_eq!(ids(&bare, "a=d"), [FIRST_ID + 2, FIRST_ID + 3]);
    let mut vt = parser(3, 10);
    vt.process(b"\r\nzzzzzzzzzz");
    vt.process(bare.as_bytes());
    assert_eq!(rows(&vt, 10)[1], "zzz zz  zz", "{bare:?}");
    let deletes = bare.find("\x1b_Ga=d").unwrap();
    assert!(
        bare.find("\x1b[2;7H").is_some_and(|cell| cell < deletes),
        "{bare:?}"
    );
}

#[test]
fn full_repaint_switched_on_empties_every_slot() {
    let mut painter = Painter::new(false);
    let view = Buffer::empty(Rect::new(0, 0, 10, 3));
    pictured(&mut painter, &view, &[tile(2, 2, 1), tile(6, 2, 1)]).unwrap();
    painter.set_always_full(true);
    let full = pictured(&mut painter, &view, &[]).unwrap();
    assert_eq!(ids(&full, "a=d"), [FIRST_ID, FIRST_ID + 1]);
    assert!(!painter.has_tiles());
    assert!(painter.always_full());
}

#[test]
fn sixel_tiles_leave_the_last_row_put_the_cursor_back_and_go_by_their_cells() {
    let mut painter = Painter::new(false);
    let view = Buffer::empty(Rect::new(0, 0, 10, 3));
    let palette = [wisp_render::theme::Rgb::hex(0)];
    let sixel = Encoder::Sixel(&palette);
    let bottom = Tile {
        at: Position { x: 4, y: 2 },
        ..tile(4, 1, 1)
    };
    let a = tile(2, 1, 1);
    let first = pictured_by(&mut painter, &view, &[a, bottom], sixel).unwrap();
    assert_eq!(first.matches("\x1bP").count(), 1, "{first:?}");
    let (_, after) = first.split_once("\x1b[2;3H\x1bP").unwrap();
    assert!(
        after.contains("\x1b\\\x1b[2;3H"),
        "the cursor goes back: {first:?}"
    );
    let changed = pictured_by(&mut painter, &view, &[Tile { hash: 2, ..a }], sixel).unwrap();
    assert_eq!(changed.matches("\x1bP").count(), 1);
    assert!(!changed.contains("\x1b_G"), "no deletes: {changed:?}");
    let gone = pictured_by(&mut painter, &view, &[], sixel).unwrap();
    assert!(
        !gone.contains("\x1b_G") && !gone.contains("\x1bP"),
        "{gone:?}"
    );
    let mut vt = parser(3, 10);
    vt.process(b"\r\nzzzzzzzzzz");
    vt.process(gone.as_bytes());
    assert_eq!(
        rows(&vt, 10)[1],
        "zz zzzzzzz",
        "its cell is written: {gone:?}"
    );
}
