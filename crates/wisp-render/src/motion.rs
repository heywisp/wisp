mod curve;
mod pacing;

use std::time::Duration;

use crate::canvas;
use crate::styled::{Look, StyledSpan};
use crate::theme::glyphs::{self, GlyphSet, Glyphs};
use crate::theme::{Depth, Ink, Milli, Token, Tone};

use curve::{ease_in_out, ease_out, morph_curve, wave};

pub use pacing::{MAX_FPS, Pacing, next_frame};

pub const FRAME_MIN: Duration = Duration::from_millis(33);

pub const MORPH: Duration = Duration::from_millis(MORPH_MS);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum MotionPolicy {
    #[default]
    Full,
    Calm,
    Reduced,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MotionCtx {
    pub policy: MotionPolicy,
    pub ascii: bool,
    pub blend: bool,
}

impl MotionCtx {
    pub const fn new(policy: MotionPolicy, glyphs: GlyphSet, depth: Depth) -> MotionCtx {
        MotionCtx {
            policy,
            ascii: matches!(glyphs, GlyphSet::Ascii),
            blend: depth.blends(),
        }
    }

    pub const fn slowdown(self) -> u64 {
        self.k()
    }

    const fn k(self) -> u64 {
        match self.policy {
            MotionPolicy::Calm => 2,
            _ => 1,
        }
    }

    const fn amp(self) -> f64 {
        match self.policy {
            MotionPolicy::Calm => 0.5,
            _ => 1.0,
        }
    }

    pub const fn reduced(self) -> bool {
        matches!(self.policy, MotionPolicy::Reduced)
    }

    const fn blends(self) -> bool {
        self.blend && !self.reduced()
    }

    pub const fn pulses(self) -> bool {
        self.blends()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Presence {
    Ready,
    Listening,
    Muted,
    Thinking,
    Answering { last_chunk: Option<Duration> },
    Working { progress: Option<Milli> },
    Done,
    Stopped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Part {
    Ring,
    Core,
}

impl Part {
    pub const fn name(self) -> &'static str {
        match self {
            Part::Ring => "ring",
            Part::Core => "core",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cell {
    pub ch: char,
    pub ink: Ink,
}

pub const ORB_CELLS: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OrbFrame {
    pub cells: [Cell; ORB_CELLS],
}

impl OrbFrame {
    pub fn text(&self) -> String {
        self.cells.iter().map(|c| c.ch).collect()
    }

    pub const fn core(&self) -> Ink {
        self.cells[1].ink
    }

    pub fn spans(&self) -> Vec<StyledSpan> {
        self.cells
            .iter()
            .map(|c| StyledSpan::new(c.ch.to_string(), Look::fg(c.ink)))
            .collect()
    }

    pub fn dots(&self) -> Vec<(u8, u8, Ink, Part)> {
        self.cells
            .iter()
            .zip([(0u8, Part::Ring), (2, Part::Core), (4, Part::Ring)])
            .flat_map(|(cell, (left, part))| {
                canvas::dots(canvas::braille(cell.ch).unwrap_or(0))
                    .map(move |(x, y)| (left + x, y, cell.ink, part))
            })
            .collect()
    }
}

const CHECK: [char; 3] = ['\u{2822}', '\u{2860}', '\u{280A}'];
const ALL: u8 = 0xFF;
const REST: u8 = ALL & !(1 << 1);
const RING: [(usize, u8); 8] = [
    (0, 0x08),
    (2, 0x01),
    (2, 0x10),
    (2, 0x20),
    (2, 0x40),
    (0, 0x80),
    (0, 0x04),
    (0, 0x02),
];
const REVEAL_ORDER: [u8; 7] = [2, 3, 4, 5, 6, 7, 0];

pub fn ring_cells(on: u8) -> (char, char) {
    let mut bits = [0u8; 3];
    for (i, (cell, bit)) in RING.iter().enumerate() {
        if on & (1 << i) != 0 {
            bits[*cell] |= *bit;
        }
    }
    (canvas::glyph(bits[0]), canvas::glyph(bits[2]))
}

fn first(glyph: &str) -> char {
    glyph.chars().next().unwrap_or(' ')
}

fn wisp() -> char {
    first(glyphs::UNICODE.wisp)
}

pub(crate) fn ms(t: Duration) -> u64 {
    u64::try_from(t.as_millis()).unwrap_or(u64::MAX)
}

pub(crate) const fn quantise(t: u64, step: u64) -> u64 {
    t - t % step
}

fn cells(on: u8, ring: Ink, core: Ink, core_ch: char, cx: MotionCtx) -> OrbFrame {
    let (left, right) = if cx.ascii {
        let paren = |c| if on == 0 { ' ' } else { c };
        (paren('('), paren(')'))
    } else {
        ring_cells(on)
    };
    let core_ch = if cx.ascii && core_ch == wisp() {
        first(glyphs::ASCII.wisp)
    } else {
        core_ch
    };
    OrbFrame {
        cells: [
            Cell {
                ch: left,
                ink: ring,
            },
            Cell {
                ch: core_ch,
                ink: core,
            },
            Cell {
                ch: right,
                ink: ring,
            },
        ],
    }
}

fn tok(t: Token) -> Ink {
    Ink::Token(t)
}

fn on_screen(ink: Ink) -> Option<Tone> {
    match (ink.tone(), ink) {
        (Some(tone), _) => Some(tone),
        (None, Ink::Mix { over, under, u, .. }) => Some(if u.get() >= 0.5 { over } else { under }),
        (None, Ink::Token(t)) => Some(Tone::Token(t)),
    }
}

fn morph(to: OrbFrame, from: OrbFrame, dt: u64) -> OrbFrame {
    let step = dt / 80;
    if step >= 2 {
        return to;
    }
    let u = Milli::new(morph_curve((step + 1) as f64 / 3.0));
    let mut out = to;
    for (cell, old) in out.cells.iter_mut().zip(from.cells) {
        if let (Some(over), Some(under)) = (cell.ink.tone(), on_screen(old.ink))
            && cell.ink != old.ink
        {
            cell.ink = Ink::Mix {
                over,
                under,
                u,
                still: cell.ink.still(),
            };
        }
    }
    out
}

pub fn pose(state: Presence, t: Duration, cx: MotionCtx) -> OrbFrame {
    let t = ms(t);
    if let Some(settle) = settle_ms(state, cx)
        && t >= settle
    {
        let ready = pose(Presence::Ready, Duration::ZERO, cx);
        if cx.blends() && settle > 0 && t - settle < MORPH_MS {
            let last = pose(state, Duration::from_millis(settle - 1), cx);
            return morph(ready, last, t - settle);
        }
        return ready;
    }
    let (k, amp) = (cx.k(), cx.amp());
    match state {
        Presence::Ready => cells(REST, tok(Token::OrbRing), tok(Token::Accent), wisp(), cx),
        Presence::Listening => cells(
            ALL,
            tok(Token::OrbRingListen),
            tok(Token::OrbCoreDim),
            wisp(),
            cx,
        ),
        Presence::Muted => cells(
            REST,
            tok(Token::OrbRingListen),
            tok(Token::OrbCoreDim),
            wisp(),
            cx,
        ),
        Presence::Thinking => {
            let step = if cx.reduced() {
                0
            } else {
                (t / (ARC_STEP_MS * k)) % 4
            };
            let on = ALL & !(1 << step) & !(1 << (step + 4));
            let core = if cx.blends() {
                let u = quantise(t, 100) % (SPARKLE_MS * k);
                let rise = (400 * k) as f64;
                let u = u as f64;
                let s = if u < rise {
                    ease_in_out(u / rise)
                } else if u < 2.0 * rise {
                    1.0 - ease_in_out((u - rise) / rise)
                } else {
                    0.0
                };
                Ink::mix(Token::Accent, Token::OrbCoreDim, s * amp, Token::OrbCoreDim)
            } else {
                tok(Token::OrbCoreDim)
            };
            let core_ch = if cx.ascii {
                ['-', '\\', '|', '/'][step as usize]
            } else {
                wisp()
            };
            cells(on, tok(Token::Text), core, core_ch, cx)
        }
        Presence::Answering { last_chunk } => {
            let core = if cx.blends() {
                let chunk = last_chunk.map(ms).filter(|c| *c <= t);
                let since = quantise(t - chunk.unwrap_or(0), 100);
                let s = if chunk.is_some() && since <= 400 {
                    1.0 - ease_out(since as f64 / 400.0)
                } else if since < BREATH_REST_MS {
                    0.0
                } else {
                    wave(since - BREATH_REST_MS, BREATH_MS * k) * amp
                };
                Ink::mix(Token::Accent, Token::OrbCoreDim, s, Token::Accent)
            } else {
                tok(Token::Accent)
            };
            cells(REST, tok(Token::OrbRingSpeak), core, wisp(), cx)
        }
        Presence::Working { progress: Some(p) } => {
            let n = ((p.get() * 8.0).ceil() as u32).clamp(1, 8);
            let on = if n == 8 { ALL } else { (1u8 << n) - 1 };
            cells(on, tok(Token::Text), tok(Token::Accent), wisp(), cx)
        }
        Presence::Working { progress: None } => {
            let ring = if cx.blends() {
                let p = wave(quantise(t, 100), WORKING_WAVE_MS * k) * amp;
                Ink::mix(Token::Text, Token::OrbRing, p, Token::OrbRing)
            } else {
                tok(Token::OrbRing)
            };
            cells(REST, ring, tok(Token::Accent), wisp(), cx)
        }
        Presence::Done => {
            let ok = tok(Token::Ok);
            if cx.ascii {
                return OrbFrame {
                    cells: [
                        Cell { ch: '(', ink: ok },
                        Cell { ch: '+', ink: ok },
                        Cell { ch: ')', ink: ok },
                    ],
                };
            }
            let cell = |i: u64| {
                let ink = if cx.blends() && t < 120 {
                    Ink::mix(Token::Ok, Token::Bg, 0.5, Token::Ok)
                } else if cx.blends() && (400 + 200 * i) * k <= t && t < (600 + 200 * i) * k {
                    Ink::mix(Token::Ok, Token::Text, 0.45, Token::Ok)
                } else {
                    ok
                };
                Cell {
                    ch: CHECK[i as usize],
                    ink,
                }
            };
            OrbFrame {
                cells: [cell(0), cell(1), cell(2)],
            }
        }
        Presence::Stopped => cells(
            REST,
            tok(Token::OrbRing),
            tok(Token::OrbCoreDim),
            wisp(),
            cx,
        ),
    }
}

const BREATH_REST_MS: u64 = 600;
const BREATH_MS: u64 = 4000;
const WORKING_WAVE_MS: u64 = 2000;
const ARC_STEP_MS: u64 = 500;
const SPARKLE_MS: u64 = 1600;
const DONE_MS: u64 = 2200;
const DONE_REDUCED_MS: u64 = 1200;
const STOPPED_MS: u64 = 300;
const MORPH_MS: u64 = 240;

const fn settle_ms(state: Presence, cx: MotionCtx) -> Option<u64> {
    match state {
        Presence::Done if cx.reduced() => Some(DONE_REDUCED_MS),
        Presence::Done => Some(DONE_MS * cx.k()),
        Presence::Stopped if cx.reduced() => Some(0),
        Presence::Stopped => Some(STOPPED_MS),
        _ => None,
    }
}

pub fn settles(state: Presence, cx: MotionCtx) -> Option<Duration> {
    settle_ms(state, cx).map(Duration::from_millis)
}

fn grid(state: Presence, cx: MotionCtx) -> Option<(u64, u64)> {
    match state {
        Presence::Ready | Presence::Listening | Presence::Muted => None,
        Presence::Thinking if cx.reduced() => None,
        Presence::Thinking if cx.blends() => Some((100, 0)),
        Presence::Thinking => Some((500 * cx.k(), 0)),
        Presence::Answering { last_chunk } if cx.blends() => {
            Some((100, last_chunk.map_or(0, |c| ms(c) % 100)))
        }
        Presence::Working { progress: None } if cx.blends() => Some((100, 0)),
        Presence::Answering { .. } | Presence::Working { .. } => None,
        Presence::Done => Some((40, 0)),
        Presence::Stopped if cx.reduced() => None,
        Presence::Stopped => Some((20, 0)),
    }
}

const HORIZON: u64 = 20_000;

fn scan(
    t: u64,
    limit: u64,
    grids: &[(u64, u64)],
    frame: impl Fn(u64) -> OrbFrame,
) -> Option<Duration> {
    if grids.is_empty() {
        return None;
    }
    let now = frame(t);
    let next_on = |step: u64, phase: u64, after: u64| {
        let base = (after.saturating_sub(phase) / step * step).saturating_add(phase);
        if base > after {
            base
        } else {
            base.saturating_add(step)
        }
    };
    let mut at = t;
    while at < limit {
        let next = grids.iter().map(|(s, p)| next_on(*s, *p, at)).min()?;
        if next > limit {
            return None;
        }
        if frame(next) != now {
            let earliest = t.saturating_add(ms(FRAME_MIN));
            return Some(Duration::from_millis(next.max(earliest)));
        }
        at = next;
    }
    None
}

pub const IDLE_LAP: Duration = Duration::from_millis(IDLE_LAP_MS);
pub const IDLE_STEP: Duration = Duration::from_millis(IDLE_STEP_MS);

const IDLE_LAP_MS: u64 = 6000;
const IDLE_STEP_MS: u64 = 100;
const IDLE_PATH: [usize; 8] = [2, 3, 4, 5, 6, 7, 0, 1];
const GAP: usize = 1;
const HEAD_HOLD: f64 = 0.06;
const TAIL_END: f64 = 0.18;
const HEAD_OP: f64 = 1.0;
const HOLD_OP: f64 = 0.8;
const REST_OP: f64 = 0.28;
const BRIGHT: f64 = 0.5;
const COMET_LIGHTS: f64 = 18.0;

pub const fn idles(cx: MotionCtx) -> bool {
    !cx.reduced() && !cx.ascii
}

pub const fn idle_lap(cx: MotionCtx) -> Duration {
    Duration::from_millis(IDLE_LAP_MS * cx.k())
}

const fn idle_step(cx: MotionCtx) -> u64 {
    IDLE_STEP_MS * cx.k()
}

pub fn idle_clock(t: Duration, cx: MotionCtx) -> u64 {
    quantise(ms(t), idle_step(cx))
}

pub fn comet_glow(tau: f64) -> f64 {
    let tau = tau.rem_euclid(1.0);
    let op = if tau < HEAD_HOLD {
        HEAD_OP - (HEAD_OP - HOLD_OP) * tau / HEAD_HOLD
    } else if tau < TAIL_END {
        HOLD_OP - (HOLD_OP - REST_OP) * (tau - HEAD_HOLD) / (TAIL_END - HEAD_HOLD)
    } else {
        REST_OP
    };
    (op - REST_OP) / (HEAD_OP - REST_OP)
}

pub fn glow_ink(glow: f64, cx: MotionCtx) -> Ink {
    let s = Milli::new(glow * cx.amp());
    if !cx.blends() {
        return if glow >= BRIGHT {
            tok(Token::Text)
        } else {
            tok(Token::OrbRing)
        };
    }
    if s == Milli::ZERO {
        return tok(Token::OrbRing);
    }
    Ink::Mix {
        over: Tone::Token(Token::Text),
        under: Tone::Token(Token::OrbRing),
        u: s,
        still: Token::OrbRing,
    }
}

pub fn idle_core(clock: u64, cx: MotionCtx) -> Ink {
    if !cx.blends() {
        return tok(Token::Accent);
    }
    let u = Milli::new(cx.amp() * wave(clock, ms(idle_lap(cx))));
    if u == Milli::ZERO {
        return tok(Token::Accent);
    }
    Ink::Mix {
        over: Tone::Token(Token::OrbCoreDim),
        under: Tone::Token(Token::Accent),
        u,
        still: Token::Accent,
    }
}

fn side_of(light: usize) -> usize {
    RING.get(light).map_or(0, |(cell, _)| *cell)
}

pub fn idle(t: Duration, cx: MotionCtx) -> OrbFrame {
    let rest = pose(Presence::Ready, Duration::ZERO, cx);
    if !idles(cx) {
        return rest;
    }
    let lap = ms(idle_lap(cx));
    let clock = idle_clock(t, cx);
    let stride = lap / IDLE_PATH.len() as u64;
    let step = usize::try_from((clock % lap) / stride).unwrap_or(0) % IDLE_PATH.len();
    let head = IDLE_PATH[step];
    let tail = IDLE_PATH[(step + IDLE_PATH.len() - 1) % IDLE_PATH.len()];
    let lights = IDLE_PATH.len() as f64;
    let glow = |light: usize| {
        let at = IDLE_PATH.iter().position(|l| *l == light).unwrap_or(0) as f64;
        let behind = ((clock % lap) as f64 / lap as f64 - at / lights).rem_euclid(1.0);
        comet_glow(behind * lights / COMET_LIGHTS)
    };
    let mut on = 0u8;
    let mut inks = [tok(Token::OrbRing); 2];
    for (slot, side) in [0usize, 2].into_iter().enumerate() {
        let lights: Vec<usize> = if head != GAP && side_of(head) == side {
            [head, tail]
                .into_iter()
                .filter(|l| *l != GAP && side_of(*l) == side)
                .collect()
        } else {
            (0..RING.len())
                .filter(|l| REST & (1 << l) != 0 && side_of(*l) == side)
                .collect()
        };
        let bright = lights.iter().map(|l| glow(*l)).fold(0.0, f64::max);
        for l in &lights {
            on |= 1 << l;
        }
        inks[slot] = glow_ink(bright, cx);
    }
    let (left, right) = ring_cells(on);
    OrbFrame {
        cells: [
            Cell {
                ch: left,
                ink: inks[0],
            },
            Cell {
                ch: wisp(),
                ink: idle_core(clock, cx),
            },
            Cell {
                ch: right,
                ink: inks[1],
            },
        ],
    }
}

pub fn idle_next_change(t: Duration, cx: MotionCtx) -> Option<Duration> {
    if !idles(cx) {
        return None;
    }
    let t = ms(t);
    scan(t, t.saturating_add(HORIZON), &[(idle_step(cx), 0)], |at| {
        idle(Duration::from_millis(at), cx)
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Orb {
    state: Presence,
    from: Option<OrbFrame>,
}

impl Orb {
    pub const fn new(state: Presence) -> Orb {
        Orb {
            state: entered(state),
            from: None,
        }
    }

    pub const fn state(&self) -> Presence {
        self.state
    }

    pub fn change(&self, to: Presence, t: Duration, cx: MotionCtx) -> Orb {
        if std::mem::discriminant(&to) == std::mem::discriminant(&self.state) {
            return *self;
        }
        let from = cx.blends().then(|| self.frame(t, cx));
        Orb {
            state: entered(to),
            from,
        }
    }

    pub fn chunk(&self, t: Duration) -> Orb {
        match self.state {
            Presence::Answering { last_chunk } => Orb {
                state: Presence::Answering {
                    last_chunk: Some(last_chunk.map_or(t, |c| c.max(t))),
                },
                from: self.from,
            },
            _ => *self,
        }
    }

    pub fn progress(&self, progress: Option<Milli>) -> Orb {
        match self.state {
            Presence::Working { .. } => Orb {
                state: Presence::Working { progress },
                from: self.from,
            },
            _ => *self,
        }
    }

    pub fn frame(&self, t: Duration, cx: MotionCtx) -> OrbFrame {
        let to = pose(self.state, t, cx);
        match self.from {
            Some(from) if t < MORPH => morph(to, from, ms(t)),
            _ => to,
        }
    }

    pub fn next_change(&self, t: Duration, cx: MotionCtx) -> Option<Duration> {
        let t = ms(t);
        let morphing = self.from.is_some() && t < MORPH_MS;
        if let Some(settle) = settle_ms(self.state, cx)
            && t >= settle.saturating_add(MORPH_MS)
            && !morphing
        {
            return None;
        }
        let mut grids = Vec::with_capacity(2);
        grids.extend(grid(self.state, cx));
        if morphing {
            grids.push((80, 0));
        }
        scan(t, t.saturating_add(HORIZON), &grids, |at| {
            self.frame(Duration::from_millis(at), cx)
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Film {
    pub frames: Vec<(Duration, OrbFrame)>,
    pub length: Duration,
    pub once: bool,
}

pub fn film(state: Presence, cx: MotionCtx) -> Film {
    let orb = Orb::new(state);
    let ready = state == Presence::Ready;
    let frame = |t| if ready { idle(t, cx) } else { orb.frame(t, cx) };
    let next = |t| {
        if ready {
            idle_next_change(t, cx)
        } else {
            orb.next_change(t, cx)
        }
    };
    let (start, length) = lap(state, cx);
    let end = start + length;
    let mut frames = vec![(Duration::ZERO, frame(start))];
    while let Some(at) = frames
        .last()
        .and_then(|(t, _)| next(start + *t))
        .filter(|at| *at < end)
    {
        frames.push((at - start, frame(at)));
    }
    let chunk = matches!(
        state,
        Presence::Answering {
            last_chunk: Some(_)
        }
    );
    let once = chunk || settle_ms(state, cx).is_some();
    Film {
        frames,
        length,
        once,
    }
}

const fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

fn lap(state: Presence, cx: MotionCtx) -> (Duration, Duration) {
    let (k, arcs) = (cx.k(), 4 * ARC_STEP_MS);
    let (start, length) = match (state, settle_ms(state, cx)) {
        (_, Some(settle)) => (0, settle + MORPH_MS),
        (Presence::Ready, None) => (0, ms(idle_lap(cx))),
        (Presence::Thinking, None) => (0, arcs / gcd(arcs, SPARKLE_MS) * SPARKLE_MS * k),
        (
            Presence::Answering {
                last_chunk: Some(_),
            },
            None,
        ) => (0, BREATH_REST_MS),
        (Presence::Answering { .. }, None) => (BREATH_REST_MS, BREATH_MS * k),
        (Presence::Working { .. }, None) => (0, WORKING_WAVE_MS * k),
        (Presence::Listening | Presence::Muted | Presence::Done | Presence::Stopped, None) => {
            (0, 0)
        }
    };
    (Duration::from_millis(start), Duration::from_millis(length))
}

const fn entered(state: Presence) -> Presence {
    match state {
        Presence::Answering {
            last_chunk: Some(_),
        } => Presence::Answering {
            last_chunk: Some(Duration::ZERO),
        },
        other => other,
    }
}

pub fn reveal(t: Duration, cx: MotionCtx) -> OrbFrame {
    let t = if cx.reduced() || cx.ascii {
        ms(REVEAL)
    } else {
        quantise(ms(t), REVEAL_STEP_MS)
    };
    let lit = usize::try_from(t / REVEAL_STEP_MS)
        .unwrap_or(usize::MAX)
        .min(REVEAL_ORDER.len());
    let on = REVEAL_ORDER[..lit].iter().fold(0u8, |m, i| m | (1 << i));
    let core = if t >= 280 || !cx.blends() {
        tok(Token::Accent)
    } else {
        Ink::mix(
            Token::Accent,
            Token::OrbCoreDim,
            t as f64 / 280.0,
            Token::Accent,
        )
    };
    cells(on, tok(Token::Text), core, wisp(), cx)
}

pub const REVEAL: Duration = Duration::from_millis(280);
pub const REVEAL_STEP_MS: u64 = 40;

pub fn reveal_next_change(t: Duration, cx: MotionCtx) -> Option<Duration> {
    if cx.reduced() || cx.ascii {
        return None;
    }
    scan(ms(t), ms(REVEAL), &[(REVEAL_STEP_MS, 0)], |at| {
        reveal(Duration::from_millis(at), cx)
    })
}

pub fn caret(before: &str, core: Ink, cx: MotionCtx) -> StyledSpan {
    let glyph = first(if cx.ascii {
        glyphs::ASCII.caret
    } else {
        glyphs::UNICODE.caret
    });
    let mut text = String::with_capacity(4);
    if !before.is_empty() && !before.ends_with(' ') {
        text.push(' ');
    }
    text.push(glyph);
    StyledSpan::new(text, Look::fg(core))
}

pub const TREE_PULSE: Duration = Duration::from_millis(2000);
const TREE_PULSE_DEPTH: f64 = 0.4;
const PULSE_GRID_MS: u64 = 100;
const TREE_GLOW: (f64, f64) = (0.15, 0.25);

fn swell(t: Duration, cx: MotionCtx) -> f64 {
    let period = ms(TREE_PULSE).saturating_mul(cx.k());
    wave(quantise(ms(t), PULSE_GRID_MS), period)
}

pub fn tree_pulse(role: Token, t: Duration, cx: MotionCtx) -> Ink {
    if !cx.blends() {
        return tok(role);
    }
    Ink::mix(
        role,
        Token::Bg,
        1.0 - TREE_PULSE_DEPTH * swell(t, cx) * cx.amp(),
        role,
    )
}

pub fn tree_glow(t: Duration, cx: MotionCtx) -> Option<f64> {
    let (low, high) = TREE_GLOW;
    let swell = if cx.blends() {
        swell(t, cx) * cx.amp()
    } else {
        0.0
    };
    cx.blend.then_some(high - (high - low) * swell)
}

pub fn picture_pulse(role: Token, t: Duration, cx: MotionCtx) -> Ink {
    let step = quantise(ms(t), 2 * PULSE_GRID_MS);
    tree_pulse(role, Duration::from_millis(step), cx)
}

pub const COMET_STEP: Duration = Duration::from_millis(150);

fn comet_step(cx: MotionCtx) -> u64 {
    ms(COMET_STEP).saturating_mul(cx.k())
}

pub fn comet(t: Duration, cx: MotionCtx, glyphs: &Glyphs) -> &'static str {
    if cx.reduced() || glyphs.comet.is_empty() {
        return glyphs.running;
    }
    let frame = ms(t) / comet_step(cx);
    let index = usize::try_from(frame % glyphs.comet.len() as u64).unwrap_or(0);
    glyphs.comet.get(index).copied().unwrap_or(glyphs.running)
}

pub fn comet_next_change(t: Duration, cx: MotionCtx) -> Option<Duration> {
    if cx.reduced() {
        return None;
    }
    let step = comet_step(cx);
    Some(Duration::from_millis(
        quantise(ms(t), step).saturating_add(step),
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WaitingLabel {
    Hidden,
    Thinking,
    ThinkingFor { secs: u64 },
}

pub fn waiting_label(t: Duration) -> WaitingLabel {
    match ms(t) {
        0..300 => WaitingLabel::Hidden,
        300..2000 => WaitingLabel::Thinking,
        t => WaitingLabel::ThinkingFor { secs: t / 1000 },
    }
}

pub fn waiting_label_next_change(t: Duration) -> Duration {
    Duration::from_millis(match ms(t) {
        0..300 => 300,
        300..2000 => 2000,
        t => quantise(t, 1000) + 1000,
    })
}

#[cfg(test)]
mod tests;
