use std::time::Duration;

use unicode_width::UnicodeWidthChar;
use wisp_render::motion::{MotionCtx, MotionPolicy, Orb, OrbFrame, Presence, reveal};
use wisp_render::theme::glyphs::{AMBIGUOUS_WIDE, ASCII, GlyphSet, Glyphs, UNICODE};
use wisp_render::theme::{Depth, Milli};

const EMOJI: &[(u32, u32)] = &[
    (0x00A9, 0x00A9),
    (0x00AE, 0x00AE),
    (0x203C, 0x203C),
    (0x2049, 0x2049),
    (0x2122, 0x2122),
    (0x2139, 0x2139),
    (0x2194, 0x2199),
    (0x21A9, 0x21AA),
    (0x231A, 0x231B),
    (0x2328, 0x2328),
    (0x23CF, 0x23CF),
    (0x23E9, 0x23F3),
    (0x23F8, 0x23FA),
    (0x24C2, 0x24C2),
    (0x25AA, 0x25AB),
    (0x25B6, 0x25B6),
    (0x25C0, 0x25C0),
    (0x25FB, 0x25FE),
    (0x2600, 0x27BF),
    (0x2934, 0x2935),
    (0x2B05, 0x2B07),
    (0x2B1B, 0x2B1C),
    (0x2B50, 0x2B50),
    (0x2B55, 0x2B55),
    (0x3030, 0x3030),
    (0x303D, 0x303D),
    (0x3297, 0x3297),
    (0x3299, 0x3299),
];

const DINGBATS_WITHOUT_EMOJI: &[char] = &['\u{2713}'];

const SPEC_NEVER: &str = "⏺⏸⏹✔✳▪▫◻◼↪▶◀‼⁉⚪⚫⭕";

#[derive(Debug, PartialEq)]
enum Width {
    Narrow,
    Ambiguous,
    Wide,
}

fn width(c: char) -> Width {
    match (c.width(), c.width_cjk()) {
        (Some(1), Some(1)) => Width::Narrow,
        (Some(1), Some(2)) => Width::Ambiguous,
        _ => Width::Wide,
    }
}

fn emoji(c: char) -> bool {
    let code = u32::from(c);
    !DINGBATS_WITHOUT_EMOJI.contains(&c)
        && !c.is_ascii()
        && EMOJI.iter().any(|(lo, hi)| (*lo..=*hi).contains(&code))
}

#[test]
fn the_emoji_check_catches_the_spec_list() {
    for c in SPEC_NEVER.chars() {
        assert!(emoji(c), "{c} U+{:04X}", u32::from(c));
    }
}

#[test]
fn every_glyph_is_narrow_or_ambiguous_and_never_emoji() {
    for set in [UNICODE, AMBIGUOUS_WIDE, ASCII] {
        for (name, glyph) in set.all() {
            assert!(!glyph.is_empty(), "{name}");
            for c in glyph.chars() {
                assert_ne!(width(c), Width::Wide, "{name} {c:?} in {:?}", set.set);
                assert!(!emoji(c), "{name} {c:?} has the emoji property");
                assert_ne!(c, '\u{2800}', "{name}: an empty braille cell");
            }
        }
    }
}

#[test]
fn every_ambiguous_glyph_has_a_narrow_replacement() {
    let unicode = UNICODE.all();
    let wide = AMBIGUOUS_WIDE.all();
    assert_eq!(unicode.len(), wide.len());
    let mut replaced = 0;
    for ((name, glyph), (wide_name, replacement)) in unicode.iter().zip(&wide) {
        assert_eq!(name, wide_name);
        assert!(
            replacement.chars().all(|c| width(c) == Width::Narrow),
            "{name}: {replacement:?}"
        );
        if glyph.chars().any(|c| width(c) == Width::Ambiguous) {
            assert_ne!(glyph, replacement, "{name}");
            replaced += 1;
        } else {
            assert_eq!(glyph, replacement, "{name} changed without need");
        }
    }
    assert_eq!(replaced, 17);
}

#[test]
fn the_ascii_set_is_ascii() {
    for (name, glyph) in ASCII.all() {
        assert!(glyph.is_ascii(), "{name}: {glyph:?}");
    }
    assert_eq!(Glyphs::of(GlyphSet::Ascii), ASCII);
}

fn orb_frames(cx: MotionCtx) -> Vec<OrbFrame> {
    let states = [
        Presence::Ready,
        Presence::Listening,
        Presence::Muted,
        Presence::Thinking,
        Presence::Answering { last_chunk: None },
        Presence::Working { progress: None },
        Presence::Working {
            progress: Some(Milli::new(0.4)),
        },
        Presence::Done,
        Presence::Stopped,
    ];
    let mut out = vec![];
    for state in states {
        let orb = Orb::new(Presence::Ready).change(state, Duration::ZERO, cx);
        for t in (0..5000).step_by(40) {
            out.push(orb.frame(Duration::from_millis(t), cx));
        }
    }
    for t in (0..400).step_by(40) {
        out.push(reveal(Duration::from_millis(t), cx));
    }
    out
}

#[test]
fn every_orb_frame_is_three_narrow_cells() {
    for set in [GlyphSet::Unicode, GlyphSet::Ascii] {
        for policy in [
            MotionPolicy::Full,
            MotionPolicy::Calm,
            MotionPolicy::Reduced,
        ] {
            let cx = MotionCtx::new(policy, set, Depth::TrueColor);
            for frame in orb_frames(cx) {
                for cell in frame.cells {
                    assert_eq!(width(cell.ch), Width::Narrow, "{:?}", frame.text());
                    assert!(!emoji(cell.ch));
                    assert_ne!(cell.ch, '\u{2800}');
                    if set == GlyphSet::Ascii {
                        assert!(cell.ch.is_ascii());
                    }
                }
            }
        }
    }
}
