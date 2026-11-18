use std::time::Duration;

use super::banner::{MIN_PATH, WORDMARK, banner_with, model_and_effort, shorten_path};
use super::facts::Facts;
use super::menu::key_row;
use super::one_line;
use super::row::{KeyHint, WELCOME_KEYS, clip};
use super::summary::home_short;
use crate::canvas;
use crate::markdown::Layout;
use crate::motion::{
    Cell, FRAME_MIN, MotionCtx, MotionPolicy, comet_glow, glow_ink, idle_clock, idle_core,
    idle_lap, ms, quantise, reveal, reveal_next_change,
};
use crate::styled::{Joined, Look, StyledLine, StyledSpan};
use crate::text::{WidthMode, spans_width, trim_end, width, wrap};
use crate::theme::glyphs::{GlyphSet, Glyphs};
use crate::theme::{Ink, Milli, Token, Tone};
use crate::transcript::copy;

pub const WELCOME_MAX: usize = 100;
pub const SPLASH_FROM: usize = 60;
pub const BOX_FROM: usize = 44;
pub const WELCOME_SETTLED: Duration = Duration::MAX;
pub const SPLASH_ASCII: [&str; 3] = [" .-''  .", ":  **  :", " '-..-' "];

const PAD: usize = 3;
const RIGHT: usize = 1;
const SPLASH_W: usize = 8;
pub(super) const SPLASH_GAP: usize = 3;
const CORE: [(usize, usize, char); 2] = [(3, 1, '\u{28BE}'), (4, 1, '\u{2877}')];
const LIGHTS: [(u8, usize, usize, u8); 16] = [
    (0, 4, 0, 0x01),
    (1, 5, 0, 0x01),
    (4, 7, 1, 0x02),
    (5, 7, 1, 0x04),
    (6, 6, 2, 0x04),
    (7, 6, 2, 0x08),
    (8, 5, 2, 0x40),
    (9, 4, 2, 0x40),
    (10, 2, 2, 0x80),
    (11, 1, 2, 0x20),
    (12, 1, 2, 0x01),
    (13, 0, 1, 0x20),
    (14, 0, 1, 0x10),
    (15, 1, 0, 0x40),
    (16, 1, 0, 0x10),
    (17, 2, 0, 0x08),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame {
    pub top_left: &'static str,
    pub top_right: &'static str,
    pub bottom_left: &'static str,
    pub bottom_right: &'static str,
    pub across: &'static str,
    pub down: &'static str,
    pub left_tee: &'static str,
    pub right_tee: &'static str,
    pub rule_left: &'static str,
    pub rule_right: &'static str,
    pub dotted: &'static str,
}

pub const ROUNDED: Frame = Frame {
    top_left: "\u{256D}",
    top_right: "\u{256E}",
    bottom_left: "\u{2570}",
    bottom_right: "\u{256F}",
    across: "\u{2500}",
    down: "\u{2502}",
    left_tee: "\u{251C}",
    right_tee: "\u{2524}",
    rule_left: "\u{251C}",
    rule_right: "\u{2524}",
    dotted: "\u{2504}",
};

pub const PLAIN: Frame = Frame {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    across: "-",
    down: "|",
    left_tee: "+",
    right_tee: "+",
    rule_left: "|",
    rule_right: "|",
    dotted: "-",
};

pub const fn frame_for(set: GlyphSet) -> Frame {
    match set {
        GlyphSet::Unicode => ROUNDED,
        GlyphSet::AmbiguousWide | GlyphSet::Ascii => PLAIN,
    }
}

fn still(cx: MotionCtx) -> bool {
    cx.ascii || matches!(cx.policy, MotionPolicy::Reduced)
}

const fn lap_ms(cx: MotionCtx) -> u64 {
    OPENING_LAP_MS * cx.slowdown()
}

const fn step_ms(cx: MotionCtx) -> u64 {
    OPENING_STEP_MS * cx.slowdown()
}

pub fn opening_lap(cx: MotionCtx) -> Duration {
    Duration::from_millis(lap_ms(cx))
}

pub fn welcome_next_change(t: Duration, cx: MotionCtx, layout: Layout) -> Option<Duration> {
    match welcome_art(layout).kind {
        ArtKind::Splash => match OPENING {
            Opening::Comet => opening_next_change(t, cx),
        },
        ArtKind::Orb => reveal_next_change(t, cx),
    }
}

pub fn opening_next_change(t: Duration, cx: MotionCtx) -> Option<Duration> {
    if still(cx) {
        return None;
    }
    let now = ms(t);
    let lap = lap_ms(cx);
    if now >= lap {
        return None;
    }
    let next = quantise(now, step_ms(cx)) + step_ms(cx);
    Some(Duration::from_millis(next.min(lap)).max(t.saturating_add(FRAME_MIN)))
}

pub fn opening_art(opening: Opening, t: Duration, cx: MotionCtx) -> [Vec<StyledSpan>; 3] {
    match opening {
        Opening::Comet => splash(t, cx),
    }
}

pub fn splash(t: Duration, cx: MotionCtx) -> [Vec<StyledSpan>; 3] {
    if cx.ascii {
        let ring = Look::fg(Token::OrbRing);
        let core = Look::fg(Token::Accent);
        let [top, _, bottom] = SPLASH_ASCII;
        return [
            vec![StyledSpan::new(top, ring)],
            vec![
                StyledSpan::new(":  ", ring),
                StyledSpan::new("**", core),
                StyledSpan::new("  :", ring),
            ],
            vec![StyledSpan::new(bottom, ring)],
        ];
    }
    let cells = if still(cx) || ms(t) >= lap_ms(cx) {
        splash_rest(cx)
    } else {
        opening(t, cx)
    };
    cells.map(|row| {
        row.iter()
            .map(|cell| match cell {
                Some(c) => StyledSpan::new(c.ch.to_string(), Look::fg(c.ink)),
                None => StyledSpan::plain(" "),
            })
            .collect()
    })
}

const OPENING_LAP_MS: u64 = 1200;
const OPENING_STEP_MS: u64 = 100;
const WORDS_FROM_MS: u64 = 300;
const WORDS_APART_MS: u64 = 100;
const WORDS_LAST_MS: u64 = 900;
const WORDS_FADE_MS: u64 = 300;
const SPLASH_LIGHTS: f64 = 18.0;
const SPLASH_START: u8 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ArtKind {
    Splash,
    Orb,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WelcomeArt {
    pub row: usize,
    pub col: usize,
    pub kind: ArtKind,
}

pub fn welcome_art(layout: Layout) -> WelcomeArt {
    if layout.width < BOX_FROM {
        return WelcomeArt {
            row: 0,
            col: 1,
            kind: ArtKind::Orb,
        };
    }
    let kind = if layout.width >= SPLASH_FROM {
        ArtKind::Splash
    } else {
        ArtKind::Orb
    };
    WelcomeArt {
        row: 2,
        col: 1 + PAD,
        kind,
    }
}

pub type SplashCells = [[Option<Cell>; SPLASH_W]; 3];

fn place_of(light: u8) -> f64 {
    f64::from(light.wrapping_add(18).wrapping_sub(SPLASH_START) % 18)
}

fn draw_splash(
    turn: Option<f64>,
    drawn: impl Fn(u8) -> bool,
    core: Ink,
    cx: MotionCtx,
) -> SplashCells {
    let mut bits = [[0u8; SPLASH_W]; 3];
    let mut glow = [[0.0f64; SPLASH_W]; 3];
    for (light, col, row, bit) in LIGHTS {
        if !drawn(light) {
            continue;
        }
        bits[row][col] |= bit;
        let lit = turn.map_or(0.0, |turn| {
            comet_glow(turn - place_of(light) / SPLASH_LIGHTS)
        });
        glow[row][col] = glow[row][col].max(lit);
    }
    let mut out: SplashCells = [[None; SPLASH_W]; 3];
    for (row, cells) in out.iter_mut().enumerate() {
        for (col, cell) in cells.iter_mut().enumerate() {
            *cell = match CORE.iter().find(|c| c.0 == col && c.1 == row) {
                Some((_, _, ch)) => Some(Cell { ch: *ch, ink: core }),
                None if bits[row][col] == 0 => None,
                None => Some(Cell {
                    ch: canvas::glyph(bits[row][col]),
                    ink: glow_ink(glow[row][col], cx),
                }),
            };
        }
    }
    out
}

fn splash_rest(cx: MotionCtx) -> SplashCells {
    draw_splash(None, |_| true, Ink::Token(Token::Accent), cx)
}

pub fn splash_idle(t: Duration, cx: MotionCtx) -> SplashCells {
    let lap = ms(idle_lap(cx)) as f64;
    let clock = idle_clock(t, cx);
    let turn = (clock as f64 % lap) / lap;
    draw_splash(Some(turn), |_| true, idle_core(clock, cx), cx)
}

pub fn opening(t: Duration, cx: MotionCtx) -> SplashCells {
    if still(cx) {
        return splash_rest(cx);
    }
    let lap = lap_ms(cx);
    let step = step_ms(cx);
    let clock = quantise(ms(t), step);
    if clock >= lap {
        return splash_idle(t, cx);
    }
    let idle = ms(idle_lap(cx));
    let end = SPLASH_LIGHTS * (lap % idle) as f64 / idle as f64;
    let speed = SPLASH_LIGHTS * lap as f64 / idle as f64;
    let start = end - SPLASH_LIGHTS;
    let ease = speed - SPLASH_LIGHTS;
    let u = clock as f64 / lap as f64;
    let run = (SPLASH_LIGHTS - ease) * u + ease * u * u;
    let turn = (start + run) / SPLASH_LIGHTS;
    let drawn = |light: u8| (place_of(light) - start).rem_euclid(SPLASH_LIGHTS) <= run;
    let breath = idle_core(clock, cx);
    let core = match breath.tone() {
        Some(over) if cx.blend => Ink::Mix {
            over,
            under: Tone::Token(Token::Bg),
            u: Milli::new((clock + step) as f64 / (lap + step) as f64),
            still: Token::Accent,
        },
        _ => breath,
    };
    draw_splash(Some(turn), drawn, core, cx)
}

pub fn words_fade(row: usize, t: Duration, cx: MotionCtx) -> Option<f64> {
    let lap = lap_ms(cx);
    let clock = quantise(ms(t), step_ms(cx));
    if still(cx) || !cx.blend || clock >= lap {
        return None;
    }
    let k = cx.slowdown();
    let apart = u64::try_from(row)
        .unwrap_or(u64::MAX)
        .saturating_mul(WORDS_APART_MS);
    let from = WORDS_FROM_MS.saturating_add(apart).min(WORDS_LAST_MS) * k;
    let fade = clock.saturating_sub(from) as f64 / (WORDS_FADE_MS * k) as f64;
    Some(fade.min(1.0))
}

fn faded(spans: Vec<StyledSpan>, fade: Option<f64>) -> Vec<StyledSpan> {
    let Some(u) = fade.filter(|u| *u < 1.0) else {
        return spans;
    };
    spans
        .into_iter()
        .map(|mut span| {
            if let Some(Ink::Token(t)) = span.look.fg {
                span.look.fg = Some(Ink::mix(t, Token::Bg, u, t));
            }
            span
        })
        .collect()
}

type Row = (Vec<StyledSpan>, Option<Joined>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opening {
    Comet,
}

pub const OPENING: Opening = Opening::Comet;

fn wrapped(spans: &[StyledSpan], room: usize, mode: WidthMode) -> Vec<Row> {
    wrap(spans, room, mode)
        .into_iter()
        .map(|l| (l.spans, l.joined))
        .collect()
}

fn muted(text: String, room: usize, mode: WidthMode) -> Vec<Row> {
    wrapped(&[StyledSpan::new(text, Token::TextMuted)], room, mode)
}

fn name_row(facts: &Facts, room: usize, mode: WidthMode) -> (Vec<StyledSpan>, (usize, usize)) {
    let version = one_line(&facts.version);
    let name = width(WORDMARK, mode);
    let mut spans = vec![StyledSpan::new(WORDMARK, Look::fg(Token::Text).bold())];
    let version_w = width(&version, mode);
    let mut padding = (name, name);
    if !version.trim().is_empty() && name + 1 + version_w <= room {
        let gap = room - name - version_w;
        spans.push(StyledSpan::plain(" ".repeat(gap)));
        spans.push(StyledSpan::new(version, Token::TextFaint));
        padding = (name, name + gap);
    }
    (spans, padding)
}

fn tagline() -> Vec<StyledSpan> {
    vec![
        StyledSpan::new(format!("{} ", copy::TAGLINE_OWN), Token::TextMuted),
        StyledSpan::new(copy::TAGLINE_COMPUTER, Look::fg(Token::Text).bold()),
    ]
}

fn talking_to(facts: &Facts, glyphs: &Glyphs, mode: WidthMode) -> Vec<StyledSpan> {
    let sep = format!(" {} ", glyphs.separator);
    let mut spans = vec![];
    if let Some(chip) = facts.chip() {
        spans.extend(chip.spans(usize::MAX, glyphs, mode));
    }
    let said = model_and_effort(facts);
    if !said.is_empty() {
        if !spans.is_empty() {
            spans.push(StyledSpan::new(sep, Token::TextFaint));
        }
        spans.push(StyledSpan::new(said, Token::Text));
    }
    spans
}

fn place(facts: &Facts, room: usize, glyphs: &Glyphs, mode: WidthMode) -> String {
    let sep = format!(" {} ", glyphs.separator);
    let label = match facts.chip() {
        Some(_) => String::new(),
        None => facts.credential_label(usize::MAX, glyphs, mode),
    };
    let cwd = one_line(&facts.cwd);
    if cwd.trim().is_empty() {
        return label;
    }
    if label.is_empty() {
        return shorten_path(&cwd, room, glyphs.more, mode);
    }
    let left = room.saturating_sub(width(&sep, mode) + width(&label, mode));
    let fit = if left >= MIN_PATH { left } else { room };
    format!("{}{sep}{label}", shorten_path(&cwd, fit, glyphs.more, mode))
}

pub fn detail_lines(facts: &Facts, glyphs: &Glyphs, detail: bool) -> Vec<String> {
    let sep = format!(" {} ", glyphs.separator);
    let search = facts.search_line(glyphs);
    if !detail {
        return search
            .map(|_| copy::search_off_hint(&sep, facts.search.chat_only))
            .into_iter()
            .collect();
    }
    let log = match &facts.log {
        Some(path) => copy::logging_to(&one_line(&home_short(path, facts.home.as_deref()))),
        None => copy::NOT_LOGGING.to_owned(),
    };
    let cache = format!("{}{sep}{log}", copy::cache_named(facts.cache_ttl.as_str()));
    let note = facts.endpoint_note.as_deref().map(one_line);
    [Some(cache), search, note].into_iter().flatten().collect()
}

fn card_rows(
    facts: &Facts,
    head: Vec<StyledSpan>,
    room: usize,
    layout: Layout,
    detail: bool,
) -> Vec<Row> {
    let glyphs = Glyphs::of(layout.glyphs);
    let mode = layout.width_mode;
    let mut out: Vec<Row> = vec![(head, None)];
    out.extend(wrapped(&tagline(), room, mode));
    let talk = talking_to(facts, &glyphs, mode);
    if !talk.is_empty() {
        out.extend(wrapped(&talk, room, mode));
    }
    out.extend(muted(place(facts, room, &glyphs, mode), room, mode));
    for line in detail_lines(facts, &glyphs, detail) {
        out.extend(muted(line, room, mode));
    }
    if let Some(resumed) = facts.resumed.as_deref() {
        out.extend(muted(one_line(resumed), room, mode));
    }
    out
}

pub fn key_line(keys: &[KeyHint], layout: Layout) -> Option<StyledLine> {
    let (last, rest) = keys.split_last()?;
    let mut keys = rest.to_vec();
    keys.push(last.clone().as_exit());
    let width = layout.width.saturating_sub(1).min(WELCOME_MAX);
    let row = key_row(
        &keys,
        "",
        None,
        WELCOME_KEYS,
        "  ",
        Layout { width, ..layout },
    );
    Some(
        trim_end(row)
            .holding(2, usize::MAX)
            .led_by(&[StyledSpan::plain("  ")]),
    )
}

fn boxed(
    content: Vec<StyledSpan>,
    inner: usize,
    frame: Frame,
    mode: WidthMode,
    more: &str,
) -> StyledLine {
    let line = Look::fg(Token::Line);
    let room = inner.saturating_sub(PAD + RIGHT);
    let content = clip(content, room, more, mode);
    let used: usize = spans_width(&content, mode);
    let mut spans = vec![
        StyledSpan::new(frame.down, line),
        StyledSpan::plain(" ".repeat(PAD)),
    ];
    spans.extend(content);
    spans.push(StyledSpan::plain(
        " ".repeat(inner.saturating_sub(PAD + used)),
    ));
    spans.push(StyledSpan::new(frame.down, line));
    StyledLine::new(spans)
}

fn edge_row(left: &str, right: &str, across: &str, inner: usize) -> StyledLine {
    StyledLine::new(vec![StyledSpan::new(
        format!("{left}{}{right}", across.repeat(inner)),
        Token::Line,
    )])
}

pub fn welcome(
    facts: &Facts,
    keys: &[KeyHint],
    t: Duration,
    cx: MotionCtx,
    layout: Layout,
) -> Vec<StyledLine> {
    let mut out = panel(facts, t, cx, layout, false);
    out.extend(key_line(keys, layout));
    out
}

pub fn welcome_in_detail(facts: &Facts, cx: MotionCtx, layout: Layout) -> Vec<StyledLine> {
    panel(facts, WELCOME_SETTLED, cx, layout, true)
}

fn panel(
    facts: &Facts,
    t: Duration,
    cx: MotionCtx,
    layout: Layout,
    detail: bool,
) -> Vec<StyledLine> {
    if layout.width < BOX_FROM {
        banner_with(facts, t, cx, layout, detail)
    } else {
        card(facts, t, cx, layout, detail)
    }
}

fn card(
    facts: &Facts,
    t: Duration,
    cx: MotionCtx,
    layout: Layout,
    detail: bool,
) -> Vec<StyledLine> {
    let glyphs = Glyphs::of(layout.glyphs);
    let mode = layout.width_mode;
    let frame = frame_for(layout.glyphs);
    let outer = layout.width.saturating_sub(1).min(WELCOME_MAX);
    let inner = outer.saturating_sub(2);
    let room = inner.saturating_sub(PAD + RIGHT);
    let mut body: Vec<(Vec<StyledSpan>, usize, Option<Joined>)> = vec![];
    let padding;
    if layout.width >= SPLASH_FROM {
        let lead = SPLASH_W + SPLASH_GAP;
        let text_room = room.saturating_sub(lead);
        let (head, gap) = name_row(facts, text_room, mode);
        padding = gap;
        let text = card_rows(facts, head, text_room, layout, detail);
        let art = opening_art(OPENING, t, cx);
        for (i, (row, joined)) in text.into_iter().enumerate() {
            let mut spans = match art.get(i) {
                Some(cells) => cells.clone(),
                None => vec![StyledSpan::plain(" ".repeat(SPLASH_W))],
            };
            spans.push(StyledSpan::plain(" ".repeat(SPLASH_GAP)));
            spans.extend(faded(row, words_fade(i, t, cx)));
            body.push((spans, lead, joined));
        }
    } else {
        let mut head = reveal(t, cx).spans();
        head.push(StyledSpan::plain(" "));
        let mark: usize = spans_width(&head, mode);
        let (name, gap) = name_row(facts, room.saturating_sub(mark), mode);
        padding = gap;
        head.extend(name);
        let mut rows = card_rows(facts, head, room, layout, detail).into_iter();
        if let Some((first, joined)) = rows.next() {
            body.push((first, mark, joined));
        }
        body.extend(rows.map(|(r, joined)| (r, 0, joined)));
    }
    let text = |art: usize| 1 + PAD + art;
    let end = 1 + inner;
    let mut out =
        vec![edge_row(frame.top_left, frame.top_right, frame.across, inner).holding(0, 0)];
    let blank = || boxed(vec![], inner, frame, mode, glyphs.more).holding(0, 0);
    out.push(blank());
    for (i, (row, art, joined)) in body.into_iter().enumerate() {
        let line = boxed(row, inner, frame, mode, glyphs.more)
            .holding(text(art), end)
            .continuing(joined);
        out.push(if i == 0 {
            line.padded(text(art) + padding.0, text(art) + padding.1)
        } else {
            line
        });
    }
    out.push(blank());
    out.push(edge_row(frame.bottom_left, frame.bottom_right, frame.across, inner).holding(0, 0));
    out
}
