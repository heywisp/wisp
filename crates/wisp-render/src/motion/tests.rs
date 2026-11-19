use std::time::Duration;

use super::*;
use crate::testing::millis as d;
use crate::theme::Tone;

const FULL: MotionCtx = MotionCtx {
    policy: MotionPolicy::Full,
    ascii: false,
    blend: true,
};
const CALM: MotionCtx = MotionCtx {
    policy: MotionPolicy::Calm,
    ..FULL
};
const REDUCED: MotionCtx = MotionCtx {
    policy: MotionPolicy::Reduced,
    ..FULL
};
const SIXTEEN: MotionCtx = MotionCtx {
    blend: false,
    ..FULL
};
const ASCII: MotionCtx = MotionCtx {
    ascii: true,
    ..FULL
};

fn at(state: Presence, ms: u64, cx: MotionCtx) -> OrbFrame {
    pose(state, d(ms), cx)
}

fn u(ink: Ink) -> f64 {
    match ink {
        Ink::Mix { u, .. } => u.get(),
        Ink::Token(t) => panic!("expected a blend, got {}", t.name()),
    }
}

fn blend(ink: Ink) -> (Tone, Tone, f64) {
    match ink {
        Ink::Mix { over, under, u, .. } => (over, under, u.get()),
        Ink::Token(t) => panic!("expected a blend, got {}", t.name()),
    }
}

fn answering(chunk: u64) -> Presence {
    Presence::Answering {
        last_chunk: Some(d(chunk)),
    }
}

const WORKING: Presence = Presence::Working { progress: None };

#[test]
fn the_ring_halves_follow_the_bit_table() {
    assert_eq!(ring_cells(REST), ('\u{288E}', '\u{2870}'));
    assert_eq!(ring_cells(ALL), ('\u{288E}', '\u{2871}'));
    assert_eq!(ring_cells(0), (' ', ' '));
    assert_eq!(ring_cells(1 << 2), (' ', '\u{2810}'));
}

#[test]
fn ready_listening_and_muted_are_still_poses() {
    let ready = at(Presence::Ready, 0, FULL);
    assert_eq!(ready.text(), "⢎⠶⡰");
    assert_eq!(ready.cells[0].ink, Ink::Token(Token::OrbRing));
    assert_eq!(ready.core(), Ink::Token(Token::Accent));
    let listening = at(Presence::Listening, 0, FULL);
    assert_eq!(listening.text(), "⢎⠶⡱");
    assert_eq!(listening.cells[2].ink, Ink::Token(Token::OrbRingListen));
    assert_eq!(listening.core(), Ink::Token(Token::OrbCoreDim));
    let muted = at(Presence::Muted, 0, FULL);
    assert_eq!(muted.text(), ready.text());
    assert_eq!(muted.cells[2].ink, Ink::Token(Token::OrbRingListen));
    assert_eq!(muted.core(), Ink::Token(Token::OrbCoreDim));
    assert_eq!(at(Presence::Ready, 0, ASCII).text(), "(*)");
}

#[test]
fn thinking_turns_two_arcs_every_500_ms() {
    let frames = [
        (0, "⢆⠶⠱"),
        (499, "⢆⠶⠱"),
        (500, "⠎⠶⡰"),
        (1000, "⢊⠶⡡"),
        (1250, "⢊⠶⡡"),
        (1500, "⢌⠶⡑"),
        (2000, "⢆⠶⠱"),
    ];
    for (t, want) in frames {
        assert_eq!(at(Presence::Thinking, t, FULL).text(), want, "at {t} ms");
    }
    assert_eq!(
        at(Presence::Thinking, 0, FULL).cells[0].ink,
        Ink::Token(Token::Text)
    );
    assert_eq!(at(Presence::Thinking, 999, CALM).text(), "⢆⠶⠱");
    assert_eq!(at(Presence::Thinking, 1000, CALM).text(), "⠎⠶⡰");
    let ascii: Vec<String> = [0, 500, 1000, 1500]
        .map(|t| at(Presence::Thinking, t, ASCII).text())
        .to_vec();
    assert_eq!(ascii, ["(-)", "(\\)", "(|)", "(/)"]);
}

#[test]
fn the_thinking_core_sparkles_on_a_100_ms_grid() {
    let s: Vec<f64> = (0..=8)
        .map(|i| u(at(Presence::Thinking, i * 100, FULL).core()))
        .collect();
    assert_eq!(s, [0.0, 0.129, 0.5, 0.871, 1.0, 0.871, 0.5, 0.129, 0.0]);
    for t in [900, 1200, 1599] {
        assert_eq!(u(at(Presence::Thinking, t, FULL).core()), 0.0);
    }
    assert_eq!(
        at(Presence::Thinking, 150, FULL).core(),
        at(Presence::Thinking, 100, FULL).core()
    );
    assert_eq!(u(at(Presence::Thinking, 1700, FULL).core()), 0.129);
    let (over, under, _) = blend(at(Presence::Thinking, 400, FULL).core());
    assert_eq!(
        (over, under),
        (Tone::Token(Token::Accent), Tone::Token(Token::OrbCoreDim))
    );
    assert_eq!(u(at(Presence::Thinking, 800, CALM).core()), 0.5);
    assert_eq!(u(at(Presence::Thinking, 400, CALM).core()), 0.25);
}

#[test]
fn thinking_holds_still_without_motion_or_blends() {
    for t in [0, 700, 1300] {
        let frame = at(Presence::Thinking, t, REDUCED);
        assert_eq!(frame.text(), "⢆⠶⠱");
        assert_eq!(frame.core(), Ink::Token(Token::OrbCoreDim));
    }
    assert_eq!(at(Presence::Thinking, 500, SIXTEEN).text(), "⠎⠶⡰");
    assert_eq!(
        at(Presence::Thinking, 400, SIXTEEN).core(),
        Ink::Token(Token::OrbCoreDim)
    );
}

#[test]
fn answering_lights_the_core_per_chunk_then_breathes() {
    let s: Vec<f64> = (0..=4)
        .map(|i| u(at(answering(1000), 1000 + i * 100, FULL).core()))
        .collect();
    assert_eq!(s, [1.0, 0.622, 0.315, 0.093, 0.0]);
    assert_eq!(u(at(answering(1000), 1050, FULL).core()), 1.0);
    assert_eq!(u(at(answering(1000), 1500, FULL).core()), 0.0);
    assert_eq!(u(at(answering(1000), 1600, FULL).core()), 0.0);
    assert_eq!(u(at(answering(1000), 3600, FULL).core()), 1.0);
    assert_eq!(u(at(answering(1000), 5600, FULL).core()), 0.0);
    assert_eq!(u(at(answering(1000), 5600, CALM).core()), 0.5);
    assert_eq!(u(at(answering(1000), 1100, CALM).core()), 0.622);
    let frame = at(answering(0), 0, FULL);
    assert_eq!(frame.text(), "⢎⠶⡰");
    assert_eq!(frame.cells[0].ink, Ink::Token(Token::OrbRingSpeak));
    let waiting = Presence::Answering { last_chunk: None };
    assert_eq!(u(at(waiting, 0, FULL).core()), 0.0);
    assert_eq!(u(at(waiting, 2600, FULL).core()), 1.0);
    assert_eq!(
        at(answering(0), 100, REDUCED).core(),
        Ink::Token(Token::Accent)
    );
    assert_eq!(
        at(answering(0), 100, SIXTEEN).core(),
        Ink::Token(Token::Accent)
    );
}

#[test]
fn working_pulses_the_ring() {
    let p: Vec<f64> = (0..=10)
        .map(|i| u(at(WORKING, i * 100, FULL).cells[0].ink))
        .collect();
    assert_eq!(
        p,
        [
            0.0, 0.024, 0.095, 0.206, 0.345, 0.5, 0.655, 0.794, 0.905, 0.976, 1.0
        ]
    );
    assert_eq!(u(at(WORKING, 1900, FULL).cells[2].ink), 0.024);
    assert_eq!(u(at(WORKING, 2000, CALM).cells[0].ink), 0.5);
    let (over, under, _) = blend(at(WORKING, 300, FULL).cells[0].ink);
    assert_eq!(
        (over, under),
        (Tone::Token(Token::Text), Tone::Token(Token::OrbRing))
    );
    assert_eq!(at(WORKING, 0, FULL).core(), Ink::Token(Token::Accent));
    assert_eq!(
        at(WORKING, 500, REDUCED).cells[0].ink,
        Ink::Token(Token::OrbRing)
    );
}

#[test]
fn known_progress_fills_the_ring_clockwise() {
    let with = |p: f64| {
        at(
            Presence::Working {
                progress: Some(Milli::new(p)),
            },
            0,
            FULL,
        )
    };
    assert_eq!(with(0.0).text(), "⠈⠶ ");
    assert_eq!(with(0.3).text(), "⠈⠶⠑");
    assert_eq!(with(1.0).text(), "⢎⠶⡱");
    assert_eq!(with(0.5).cells[0].ink, Ink::Token(Token::Text));
}

#[test]
fn done_draws_a_check_with_one_glint() {
    let ok = Ink::Token(Token::Ok);
    let glint = Ink::mix(Token::Ok, Token::Text, 0.45, Token::Ok);
    let faded = Ink::mix(Token::Ok, Token::Bg, 0.5, Token::Ok);
    let table = [
        (0, [faded, faded, faded]),
        (119, [faded, faded, faded]),
        (120, [ok, ok, ok]),
        (399, [ok, ok, ok]),
        (400, [glint, ok, ok]),
        (600, [ok, glint, ok]),
        (800, [ok, ok, glint]),
        (1000, [ok, ok, ok]),
        (2199, [ok, ok, ok]),
    ];
    for (t, inks) in table {
        let frame = at(Presence::Done, t, FULL);
        assert_eq!(frame.text(), "⠢⡠⠊", "at {t} ms");
        assert_eq!(frame.cells.map(|c| c.ink), inks, "at {t} ms");
    }
    let morphing = at(Presence::Done, 2200, FULL);
    assert_eq!(morphing.text(), "⢎⠶⡰");
    assert_eq!(u(morphing.cells[0].ink), 0.401);
    assert_eq!(u(at(Presence::Done, 2280, FULL).cells[0].ink), 0.919);
    assert_eq!(at(Presence::Done, 2360, FULL), at(Presence::Ready, 0, FULL));

    assert_eq!(at(Presence::Done, 799, CALM).cells[0].ink, ok);
    assert_eq!(at(Presence::Done, 800, CALM).cells[0].ink, glint);
    assert_eq!(at(Presence::Done, 4399, CALM).text(), "⠢⡠⠊");
    assert_eq!(at(Presence::Done, 4400, CALM).text(), "⢎⠶⡰");

    assert_eq!(
        at(Presence::Done, 0, REDUCED).cells.map(|c| c.ink),
        [ok, ok, ok]
    );
    assert_eq!(
        at(Presence::Done, 500, REDUCED).cells.map(|c| c.ink),
        [ok, ok, ok]
    );
    assert_eq!(at(Presence::Done, 1199, REDUCED).text(), "⠢⡠⠊");
    assert_eq!(
        at(Presence::Done, 1200, REDUCED),
        at(Presence::Ready, 0, REDUCED)
    );
    assert_eq!(
        at(Presence::Done, 500, SIXTEEN).cells.map(|c| c.ink),
        [ok, ok, ok]
    );
    assert_eq!(at(Presence::Done, 0, ASCII).text(), "(+)");
}

#[test]
fn stopped_dims_the_core_for_300_ms() {
    let frame = at(Presence::Stopped, 299, FULL);
    assert_eq!(frame.text(), "⢎⠶⡰");
    assert_eq!(frame.cells[0].ink, Ink::Token(Token::OrbRing));
    assert_eq!(frame.core(), Ink::Token(Token::OrbCoreDim));
    let settling = at(Presence::Stopped, 300, FULL);
    assert_eq!(settling.text(), "⢎⠶⡰");
    let (over, under, step) = blend(settling.core());
    assert_eq!(
        (over, under, step),
        (
            Tone::Token(Token::Accent),
            Tone::Token(Token::OrbCoreDim),
            0.401
        )
    );
    assert_eq!(u(at(Presence::Stopped, 380, FULL).core()), 0.919);
    assert_eq!(
        at(Presence::Stopped, 460, FULL),
        at(Presence::Ready, 0, FULL)
    );
    assert_eq!(
        at(Presence::Stopped, 300, SIXTEEN),
        at(Presence::Ready, 0, SIXTEEN)
    );
    let orb = Orb::new(Presence::Stopped);
    let times: Vec<u128> = [0, 300, 380]
        .map(|t| orb.next_change(d(t), FULL).map_or(0, |n| n.as_millis()))
        .to_vec();
    assert_eq!(times, [300, 380, 460]);
    assert_eq!(orb.next_change(d(460), FULL), None);
    assert_eq!(
        at(Presence::Stopped, 0, REDUCED),
        at(Presence::Ready, 0, REDUCED)
    );
}

#[test]
fn a_state_change_morphs_colours_in_three_80_ms_steps() {
    let orb = Orb::new(Presence::Ready).change(Presence::Thinking, d(5000), FULL);
    let target = pose(Presence::Thinking, d(0), FULL);
    let first = orb.frame(d(0), FULL);
    assert_eq!(first.text(), target.text());
    let (over, under, step) = blend(first.cells[0].ink);
    assert_eq!(over, Tone::Token(Token::Text));
    assert_eq!(under, Tone::Token(Token::OrbRing));
    assert_eq!(step, 0.401);
    assert_eq!(u(orb.frame(d(79), FULL).cells[0].ink), 0.401);
    assert_eq!(u(orb.frame(d(80), FULL).cells[0].ink), 0.919);
    assert_eq!(
        orb.frame(d(160), FULL),
        pose(Presence::Thinking, d(160), FULL)
    );
    assert_eq!(
        orb.frame(d(240), FULL),
        pose(Presence::Thinking, d(240), FULL)
    );

    let (over, under, _) = blend(orb.frame(d(0), FULL).core());
    assert_eq!(
        over,
        Tone::Mix(Token::Accent, Token::OrbCoreDim, Milli::ZERO)
    );
    assert_eq!(under, Tone::Token(Token::Accent));

    for cx in [REDUCED, SIXTEEN] {
        let orb = Orb::new(Presence::Ready).change(Presence::Thinking, d(0), cx);
        assert_eq!(orb.frame(d(0), cx), pose(Presence::Thinking, d(0), cx));
    }
}

#[test]
fn the_reveal_assembles_the_logo_in_280_ms() {
    let table = [
        (0, " ⠶ "),
        (40, " ⠶⠐"),
        (80, " ⠶⠰"),
        (120, " ⠶⡰"),
        (160, "⢀⠶⡰"),
        (200, "⢄⠶⡰"),
        (240, "⢆⠶⡰"),
        (279, "⢆⠶⡰"),
        (280, "⢎⠶⡰"),
    ];
    for (t, want) in table {
        let frame = reveal(d(t), FULL);
        assert_eq!(frame.text(), want, "at {t} ms");
        assert_eq!(frame.cells[0].ink, Ink::Token(Token::Text));
    }
    for (t, want) in [(0, 0.0), (40, 0.143), (120, 0.429), (240, 0.857)] {
        assert_eq!(u(reveal(d(t), FULL).core()), want, "at {t} ms");
    }
    assert_eq!(reveal(d(280), FULL).core(), Ink::Token(Token::Accent));
    assert_eq!(reveal(d(0), REDUCED), reveal(d(280), FULL));
    assert_eq!(reveal(d(0), ASCII).text(), "(*)");
    assert_eq!(reveal(d(40), SIXTEEN).core(), Ink::Token(Token::Accent));

    let mut t = d(0);
    let mut changes = vec![];
    while let Some(next) = reveal_next_change(t, FULL) {
        changes.push(next.as_millis());
        t = next;
    }
    assert_eq!(changes, [40, 80, 120, 160, 200, 240, 280]);
    assert_eq!(reveal_next_change(d(0), REDUCED), None);
}

#[test]
fn still_states_never_ask_for_a_frame() {
    for cx in [FULL, CALM, REDUCED, SIXTEEN] {
        for state in [Presence::Ready, Presence::Listening, Presence::Muted] {
            assert_eq!(Orb::new(state).next_change(d(0), cx), None);
        }
    }
    for state in [Presence::Thinking, answering(0), WORKING] {
        assert_eq!(Orb::new(state).next_change(d(0), REDUCED), None);
    }
    assert_eq!(
        Orb::new(Presence::Done)
            .next_change(d(2200), FULL)
            .map(|t| t.as_millis()),
        Some(2280)
    );
    assert_eq!(Orb::new(Presence::Done).next_change(d(2400), FULL), None);
}

#[test]
fn next_change_lands_on_the_grids() {
    let next = |orb: Orb, t: u64, cx| orb.next_change(d(t), cx).map(|n| n.as_millis());
    let thinking = Orb::new(Presence::Thinking);
    assert_eq!(next(thinking, 0, FULL), Some(100));
    assert_eq!(next(thinking, 800, FULL), Some(1000));
    assert_eq!(next(thinking, 0, SIXTEEN), Some(500));
    assert_eq!(
        next(
            thinking,
            0,
            MotionCtx {
                blend: false,
                ..CALM
            }
        ),
        Some(1000)
    );
    let streaming = Orb::new(answering(0)).chunk(d(1030));
    assert_eq!(next(streaming, 1090, FULL), Some(1130));
    assert_eq!(next(streaming, 1100, FULL), Some(1133));
    let done = Orb::new(Presence::Done);
    assert_eq!(next(done, 0, FULL), Some(120));
    assert_eq!(next(done, 120, FULL), Some(400));
    assert_eq!(next(done, 1000, FULL), Some(2200));
    let morphing = Orb::new(Presence::Ready).change(Presence::Listening, d(0), FULL);
    assert_eq!(next(morphing, 0, FULL), Some(80));
    assert_eq!(next(morphing, 80, FULL), Some(160));
    assert_eq!(next(morphing, 160, FULL), None);
}

fn changes_in(orb: Orb, cx: MotionCtx, window: u64) -> Vec<u64> {
    let mut t = d(0);
    let mut out = vec![];
    while let Some(next) = orb.next_change(t, cx) {
        if next.as_millis() > u128::from(window) {
            break;
        }
        assert!(next > t);
        out.push(ms(next));
        t = next;
    }
    out
}

#[test]
fn the_frame_budget_holds() {
    for cx in [FULL, CALM, REDUCED, SIXTEEN, ASCII] {
        assert!(changes_in(Orb::new(Presence::Ready), cx, 10_000).is_empty());
        let thinking = changes_in(Orb::new(Presence::Thinking), cx, 10_000);
        assert!(thinking.len() <= 100, "{} thinking frames", thinking.len());
        for state in [answering(0), WORKING, Presence::Done, Presence::Stopped] {
            let morph = Orb::new(Presence::Ready).change(state, d(0), cx);
            let times = changes_in(morph, cx, 10_000);
            let mut last = 0;
            for t in &times {
                assert!(*t - last >= 33, "{state:?}: {t} after {last}");
                last = *t;
            }
            assert!(times.len() <= 300);
        }
    }
}

#[test]
fn every_change_is_real() {
    let orb = Orb::new(Presence::Thinking);
    let mut t = d(0);
    for _ in 0..40 {
        let Some(next) = orb.next_change(t, FULL) else {
            break;
        };
        assert_ne!(orb.frame(next, FULL), orb.frame(t, FULL));
        let before = next - d(1);
        assert_eq!(orb.frame(before, FULL), orb.frame(t, FULL));
        t = next;
    }
}

#[test]
fn the_caret_is_the_core() {
    let ink = Ink::mix(Token::Accent, Token::OrbCoreDim, 0.3, Token::Accent);
    let span = caret("Hello", ink, FULL);
    assert_eq!(span.text, " ⠶");
    assert_eq!(span.look.fg, Some(ink));
    assert_eq!(caret("Hello ", ink, FULL).text, "⠶");
    assert_eq!(caret("", ink, FULL).text, "⠶");
    assert_eq!(caret("Hello", ink, ASCII).text, " _");
    let still = pose(answering(0), d(100), REDUCED).core();
    assert_eq!(
        caret("x", still, REDUCED).look.fg,
        Some(Ink::Token(Token::Accent))
    );
}

#[test]
fn the_waiting_label_appears_after_300_ms_and_counts_from_2_s() {
    assert_eq!(waiting_label(d(0)), WaitingLabel::Hidden);
    assert_eq!(waiting_label(d(299)), WaitingLabel::Hidden);
    assert_eq!(waiting_label(d(300)), WaitingLabel::Thinking);
    assert_eq!(waiting_label(d(1999)), WaitingLabel::Thinking);
    assert_eq!(
        waiting_label(d(2000)),
        WaitingLabel::ThinkingFor { secs: 2 }
    );
    assert_eq!(
        waiting_label(d(4999)),
        WaitingLabel::ThinkingFor { secs: 4 }
    );
    assert_eq!(waiting_label_next_change(d(0)), d(300));
    assert_eq!(waiting_label_next_change(d(300)), d(2000));
    assert_eq!(waiting_label_next_change(d(2000)), d(3000));
    assert_eq!(waiting_label_next_change(d(3500)), d(4000));
}

#[test]
fn motion_ctx_follows_depth_and_glyphs() {
    use crate::theme::Depth;
    use crate::theme::glyphs::GlyphSet;
    let cx = MotionCtx::new(MotionPolicy::Calm, GlyphSet::Ascii, Depth::Ansi16);
    assert_eq!(
        cx,
        MotionCtx {
            policy: MotionPolicy::Calm,
            ascii: true,
            blend: false
        }
    );
    assert!(MotionCtx::new(MotionPolicy::Full, GlyphSet::Unicode, Depth::Ansi256).blend);
}

#[test]
fn settling_times_live_in_one_table() {
    assert_eq!(settles(Presence::Done, FULL), Some(d(2200)));
    assert_eq!(settles(Presence::Done, CALM), Some(d(4400)));
    assert_eq!(settles(Presence::Done, REDUCED), Some(d(1200)));
    assert_eq!(settles(Presence::Stopped, FULL), Some(d(300)));
    assert_eq!(settles(Presence::Stopped, CALM), Some(d(300)));
    assert_eq!(settles(Presence::Stopped, REDUCED), Some(d(0)));
    for state in [Presence::Ready, Presence::Thinking, answering(0), WORKING] {
        assert_eq!(settles(state, FULL), None);
    }
}

#[test]
fn a_same_state_change_keeps_the_morph_and_the_clock() {
    let orb = Orb::new(Presence::Ready).change(Presence::Thinking, d(0), FULL);
    assert_eq!(orb.change(Presence::Thinking, d(40), FULL), orb);
    let streaming = orb.change(answering(0), d(100), FULL);
    assert_eq!(streaming.change(answering(900), d(50), FULL), streaming);
}

#[test]
fn chunks_are_recorded_at_the_time_given() {
    let entered = Orb::new(Presence::Thinking).change(answering(5000), d(10), FULL);
    assert_eq!(entered.state(), answering(0));
    let quiet =
        Orb::new(Presence::Thinking).change(Presence::Answering { last_chunk: None }, d(10), FULL);
    assert_eq!(quiet.state(), Presence::Answering { last_chunk: None });
    let lit = quiet.chunk(d(700));
    assert_eq!(lit.state(), answering(700));
    assert_eq!(lit.chunk(d(600)).state(), answering(700));
    assert_eq!(u(lit.frame(d(700), FULL).core()), 1.0);
    assert_eq!(
        Orb::new(Presence::Ready).chunk(d(5)),
        Orb::new(Presence::Ready)
    );
    let working = Orb::new(WORKING).progress(Some(Milli::new(1.0)));
    assert_eq!(working.frame(d(0), FULL).text(), "⢎⠶⡱");
}

#[test]
fn an_interrupted_morph_starts_from_the_frame_on_screen() {
    let first = Orb::new(Presence::Ready).change(Presence::Listening, d(0), FULL);
    let on_screen = first.frame(d(90), FULL);
    assert_eq!(u(on_screen.cells[0].ink), 0.919);
    let second = first.change(Presence::Thinking, d(90), FULL);
    let (_, under, _) = blend(second.frame(d(0), FULL).cells[0].ink);
    assert_eq!(
        under,
        Tone::Mix(Token::OrbRingListen, Token::OrbRing, Milli::new(0.919))
    );
    let (_, under, _) = blend(second.frame(d(0), FULL).core());
    assert_eq!(
        under,
        Tone::Mix(Token::OrbCoreDim, Token::Accent, Milli::new(0.919))
    );

    let thinking = Orb::new(Presence::Ready).change(Presence::Thinking, d(0), FULL);
    let nested = thinking.frame(d(0), FULL).core();
    assert_eq!(nested.tone(), None);
    let third = thinking.change(Presence::Listening, d(0), FULL);
    let (_, under, _) = blend(third.frame(d(0), FULL).core());
    assert_eq!(under, Tone::Token(Token::Accent));
}

#[test]
fn next_change_never_overflows() {
    let thinking = Orb::new(Presence::Thinking);
    assert_eq!(
        thinking.next_change(Duration::from_millis(u64::MAX - 10), FULL),
        None
    );
    assert_eq!(
        Orb::new(Presence::Done).next_change(Duration::MAX, FULL),
        None
    );
}

#[test]
fn the_comet_steps_every_150_ms_clockwise_and_degrades() {
    use crate::theme::glyphs::{ASCII as ASCII_GLYPHS, UNICODE};
    let frames: Vec<&str> = (0..9)
        .map(|i| comet(d(i * 150 + 149), FULL, &UNICODE))
        .collect();
    assert_eq!(
        frames,
        [
            "\u{2803}", "\u{2809}", "\u{2818}", "\u{2830}", "\u{28A0}", "\u{28C0}", "\u{2844}",
            "\u{2806}", "\u{2803}"
        ]
    );
    assert_eq!(comet(d(299), CALM, &UNICODE), "\u{2803}");
    assert_eq!(comet(d(300), CALM, &UNICODE), "\u{2809}");
    assert_eq!(comet(d(5000), REDUCED, &UNICODE), "\u{25CC}");
    let ascii: Vec<&str> = (0..5)
        .map(|i| comet(d(i * 150), ASCII, &ASCII_GLYPHS))
        .collect();
    assert_eq!(ascii, ["-", "\\", "|", "/", "-"]);
    assert_eq!(comet(d(5000), REDUCED, &ASCII_GLYPHS), "o");
    assert_eq!(comet_next_change(d(0), FULL), Some(d(150)));
    assert_eq!(comet_next_change(d(149), FULL), Some(d(150)));
    assert_eq!(comet_next_change(d(150), FULL), Some(d(300)));
    assert_eq!(comet_next_change(d(10), CALM), Some(d(300)));
    assert_eq!(comet_next_change(d(10), REDUCED), None);
}

fn idle_changes(cx: MotionCtx, from: u64, window: u64) -> Vec<u64> {
    let mut out = vec![];
    let mut t = from;
    while let Some(next) = idle_next_change(d(t), cx) {
        let next = ms(next);
        if next > from + window {
            break;
        }
        out.push(next);
        t = next;
    }
    out
}

fn ring(frame: OrbFrame) -> (char, char) {
    (frame.cells[0].ch, frame.cells[2].ch)
}

const LAP: [(u64, char, char); 8] = [
    (0, '\u{288E}', '\u{2810}'),
    (800, '\u{288E}', '\u{2830}'),
    (1500, '\u{288E}', '\u{2860}'),
    (2300, '\u{2880}', '\u{2870}'),
    (3000, '\u{2884}', '\u{2870}'),
    (3800, '\u{2806}', '\u{2870}'),
    (4500, '\u{280A}', '\u{2870}'),
    (5300, '\u{288E}', '\u{2870}'),
];

#[test]
fn the_idle_comet_starts_at_once_and_laps_the_ring_every_6_s() {
    for (t, left, right) in LAP {
        assert_eq!(ring(idle(d(t), FULL)), (left, right), "{t}");
        assert_eq!(ring(idle(d(t + 6000), FULL)), (left, right), "{t}");
        assert_eq!(idle(d(t), FULL).cells[1].ch, '\u{2836}');
    }
    let first = idle(d(0), FULL);
    assert!(u(first.cells[0].ink) < 0.5, "the last lap's tail fades");
    assert_eq!(idle(d(1500), FULL).cells[0].ink, Ink::Token(Token::OrbRing));
    let (over, under, glow) = blend(first.cells[2].ink);
    assert_eq!(over, Tone::Token(Token::Text));
    assert_eq!(under, Tone::Token(Token::OrbRing));
    assert!((glow - 1.0).abs() < 1e-9, "{glow}");
    assert_eq!(first.cells[2].ink.still(), Token::OrbRing);
    assert_eq!(first.core(), Ink::Token(Token::Accent));
    assert_eq!(idle(d(5300), FULL).cells[2].ink, Ink::Token(Token::OrbRing));
}

#[test]
fn the_idle_core_breathes_to_full_depth_every_6_s() {
    let (over, under, depth) = blend(idle(d(3000), FULL).core());
    assert_eq!(over, Tone::Token(Token::OrbCoreDim));
    assert_eq!(under, Tone::Token(Token::Accent));
    assert!((depth - 1.0).abs() < 1e-9, "{depth}");
    assert_eq!(idle(d(3000), FULL).core().still(), Token::Accent);
    assert_eq!(idle(d(6000), FULL).core(), Ink::Token(Token::Accent));
    let rising: Vec<f64> = (1..=30).map(|i| u(idle(d(i * 100), FULL).core())).collect();
    assert!(rising.windows(2).all(|w| w[1] > w[0]), "{rising:?}");
}

#[test]
fn calm_idles_half_as_fast_and_half_as_deep() {
    assert_eq!(
        [399, 400, 800, 1200, 1600].map(|ms| picture_frame(d(ms), CALM)),
        [0, 1, 2, 3, 0]
    );
    assert_eq!(picture_frame(d(200), FULL), 1);
    assert_eq!(ring(idle(d(1500), CALM)), ('\u{288E}', '\u{2810}'));
    assert_eq!(ring(idle(d(1600), CALM)), ('\u{288E}', '\u{2830}'));
    let (_, _, depth) = blend(idle(d(6000), CALM).core());
    assert!((depth - 0.5).abs() < 1e-9, "{depth}");
    let (_, _, glow) = blend(idle(d(0), CALM).cells[2].ink);
    assert!((glow - 0.5).abs() < 1e-9, "{glow}");
    let changes = idle_changes(CALM, 5000, 10_000);
    assert!(changes.iter().all(|t| t % 200 == 0), "{changes:?}");
    assert!((25..=50).contains(&changes.len()), "{}", changes.len());
}

#[test]
fn the_idle_writes_at_most_ten_frames_a_second() {
    let changes = idle_changes(FULL, 5100, 10_000);
    assert!(changes.iter().all(|t| t % 100 == 0), "{changes:?}");
    assert!((60..=100).contains(&changes.len()), "{}", changes.len());
    for pair in changes.windows(2) {
        assert_ne!(idle(d(pair[0]), FULL), idle(d(pair[1]), FULL));
    }
    assert_eq!(idle_next_change(d(0), FULL), Some(d(100)));
}

#[test]
fn without_blends_the_comet_still_turns_and_reduced_or_ascii_stay_still() {
    assert_eq!(picture_frame(d(600), REDUCED), 0);
    assert_eq!(picture_frame(d(600), ASCII), picture_frame(d(600), FULL));
    for cx in [REDUCED, ASCII] {
        assert!(!idles(cx));
        assert_eq!(idle_next_change(d(0), cx), None);
        for t in [0, 4000, 6000, 7500] {
            assert_eq!(idle(d(t), cx), at(Presence::Ready, 0, cx), "{t}");
        }
    }
    assert!(idles(FULL) && idles(CALM) && idles(SIXTEEN));
    for (t, left, right) in LAP {
        assert_eq!(ring(idle(d(t), SIXTEEN)), (left, right), "{t}");
    }
    let first = idle(d(0), SIXTEEN);
    assert_eq!(first.cells[2].ink, Ink::Token(Token::Text));
    assert_eq!(first.cells[0].ink, Ink::Token(Token::OrbRing));
    assert_eq!(idle(d(3000), SIXTEEN).core(), Ink::Token(Token::Accent));
    let changes = idle_changes(SIXTEEN, 5100, 10_000);
    assert!((10..=30).contains(&changes.len()), "{changes:?}");
}

#[test]
fn the_comet_glow_follows_the_brand_keyframes() {
    let r3 = |v: f64| (v * 1000.0).round() / 1000.0;
    assert_eq!(r3(comet_glow(0.0)), 1.0);
    assert_eq!(r3(comet_glow(0.06)), r3(0.52 / 0.72));
    assert_eq!(r3(comet_glow(0.18)), 0.0);
    assert_eq!(r3(comet_glow(0.5)), 0.0);
    assert_eq!(r3(comet_glow(-0.94)), r3(0.52 / 0.72));
}

#[test]
fn pacing_keeps_the_chosen_motion_whatever_the_focus() {
    let mut p = Pacing::new(FULL);
    assert_eq!(p.ctx(), FULL);
    assert!(!p.unfocused());
    p.focus(false);
    assert!(p.unfocused());
    assert_eq!(p.ctx(), FULL);
    p.focus(true);
    assert!(!p.unfocused());
    assert_eq!(p.ctx(), FULL);
    assert_eq!(Pacing::new(CALM).ctx(), CALM);
}

#[test]
fn a_film_is_one_loop_of_the_states_frames_and_stopped_and_a_chunk_play_once() {
    let ready = film(Presence::Ready, FULL);
    assert_eq!(ready.length, IDLE_LAP);
    assert!(!ready.once);
    assert_eq!(
        ready.frames[0],
        (Duration::ZERO, idle(Duration::ZERO, FULL))
    );
    assert_eq!(idle(ready.length, FULL), idle(Duration::ZERO, FULL));
    assert!(ready.frames.len() > 8, "{}", ready.frames.len());
    for state in [Presence::Ready, Presence::Thinking] {
        let film = film(state, FULL);
        assert!(film.frames.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(film.frames.windows(2).all(|w| w[0].1 != w[1].1));
        assert!(film.frames.iter().all(|(at, _)| *at < film.length));
    }
    for cx in [FULL, CALM] {
        for (state, start) in [
            (Presence::Ready, 0),
            (Presence::Thinking, 0),
            (Presence::Answering { last_chunk: None }, BREATH_REST_MS),
        ] {
            let film = film(state, cx);
            let live = |t: u64| {
                let t = Duration::from_millis(t);
                if state == Presence::Ready {
                    idle(t, cx)
                } else {
                    Orb::new(state).frame(t, cx)
                }
            };
            let length = ms(film.length);
            let times = (0..length)
                .step_by(100)
                .chain(film.frames.iter().map(|(at, _)| ms(*at)));
            for t in times {
                let shown = film
                    .frames
                    .iter()
                    .rfind(|(at, _)| ms(*at) <= t)
                    .map(|(_, f)| *f);
                assert_eq!(shown, Some(live(start + t)), "{state:?} {t}");
                assert_eq!(shown, Some(live(start + length + t)), "{state:?} {t}");
            }
        }
    }
    let stopped = film(Presence::Stopped, FULL);
    assert_eq!(stopped.frames[0].1, at(Presence::Stopped, 0, FULL));
    assert!(stopped.once);
    assert_eq!(
        stopped.frames.last().map(|(_, f)| *f),
        Some(at(Presence::Ready, 0, FULL))
    );
    let chunk = Presence::Answering {
        last_chunk: Some(Duration::ZERO),
    };
    let lit = film(chunk, FULL);
    assert_eq!(ms(lit.length), BREATH_REST_MS);
    assert!(lit.once);
    for t in (0..BREATH_REST_MS).step_by(100) {
        let shown = lit.frames.iter().rfind(|(at, _)| ms(*at) <= t);
        let live = Orb::new(chunk).frame(Duration::from_millis(t), FULL);
        assert_eq!(shown.map(|(_, f)| *f), Some(live), "{t}");
    }
    let breath = film(Presence::Answering { last_chunk: None }, FULL);
    assert!(!breath.once);
    let after = Orb::new(chunk).frame(lit.length, FULL);
    assert_eq!(Some(after), breath.frames.first().map(|(_, f)| *f));
    for state in [
        Presence::Ready,
        Presence::Listening,
        Presence::Muted,
        Presence::Thinking,
        Presence::Answering { last_chunk: None },
        chunk,
        Presence::Stopped,
    ] {
        assert_eq!(film(state, REDUCED).frames.len(), 1, "{state:?}");
    }
    for still in [Presence::Listening, Presence::Muted] {
        assert_eq!(film(still, FULL).frames.len(), 1, "{still:?}");
    }
}

#[test]
fn the_dots_are_the_braille_cells_on_a_six_by_four_grid() {
    let rest = at(Presence::Ready, 0, FULL);
    let mut ring: Vec<(u8, u8)> = rest
        .dots()
        .iter()
        .filter(|(_, _, ink, part)| *ink == Ink::Token(Token::OrbRing) && *part == Part::Ring)
        .map(|(x, y, _, _)| (*x, *y))
        .collect();
    ring.sort_unstable();
    assert_eq!(
        ring,
        [(0, 1), (0, 2), (1, 0), (1, 3), (4, 3), (5, 1), (5, 2)]
    );
    let core: Vec<(u8, u8, Ink, Part)> = rest
        .dots()
        .into_iter()
        .filter(|d| d.0 == 2 || d.0 == 3)
        .collect();
    assert_eq!(
        core,
        [(2, 1), (2, 2), (3, 1), (3, 2)].map(|(x, y)| (
            x,
            y,
            Ink::Token(Token::Accent),
            Part::Core
        ))
    );
    let listening = at(Presence::Listening, 0, FULL).dots();
    assert_eq!(listening.len(), 12);
    let ring = listening.iter().filter(|d| d.3 == Part::Ring).count();
    assert_eq!(ring, 8);
    assert!(at(Presence::Ready, 0, ASCII).dots().is_empty());
}
