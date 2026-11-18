pub mod glyphs;
mod tables;

use ratatui_core::style::{Color, Modifier, Style};
use ratatui_core::text::{Line, Span};

use crate::styled::{Look, StyledLine};

pub use tables::{ANSI16_DARK, ANSI16_LIGHT, ANSI256_DARK, ANSI256_LIGHT, DARK_PLUS, STONE_LIGHT};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Token {
    Text,
    TextMuted,
    TextFaint,
    TextDisabled,
    Accent,
    Thinking,
    User,
    Ok,
    Warn,
    WarnGlyph,
    WarnFill,
    WarnOn,
    Error,
    RoleCoordinator,
    RoleCoder,
    RoleReviewer,
    SynString,
    SynNumber,
    SynType,
    SynComment,
    SynFunction,
    SynVariable,
    SynControl,
    HueCool,
    HueWarm,
    Secret,
    OrbRing,
    OrbRingListen,
    OrbRingSpeak,
    OrbRingDim,
    OrbCoreDim,
    OrbOff,
    Bg,
    Surface,
    Sunken,
    Line,
    AccentTint,
    WarnTint,
    DiffAdd,
    DiffAddWord,
    DiffDel,
    DiffDelWord,
    Mark1,
    Mark2,
    Mark3,
    Mark4,
    Mark5,
    Mark6,
    Working,
    Gleam,
    Agent,
    OrbCore,
    Tool,
    ToolMeta,
    RoleResearcher,
    RoleDesigner,
    SynKeyword,
    MarkGone,
}

pub const BASE_TOKENS: usize = 50;

pub const TINT_FLOOR: f64 = 4.5;

pub const GLYPH_FLOOR: f64 = 3.0;

pub const DARK_BELOW: f64 = 0.60;

pub const BAND_PAIR: (Rgb, Rgb) = (Rgb::hex(0x252526), DARK_PLUS[Token::Bg.index()].1);

pub const FIT_STEPS: u16 = 10_000;

impl Token {
    pub const ALL: [Token; 58] = [
        Token::Text,
        Token::TextMuted,
        Token::TextFaint,
        Token::TextDisabled,
        Token::Accent,
        Token::Thinking,
        Token::User,
        Token::Ok,
        Token::Warn,
        Token::WarnGlyph,
        Token::WarnFill,
        Token::WarnOn,
        Token::Error,
        Token::RoleCoordinator,
        Token::RoleCoder,
        Token::RoleReviewer,
        Token::SynString,
        Token::SynNumber,
        Token::SynType,
        Token::SynComment,
        Token::SynFunction,
        Token::SynVariable,
        Token::SynControl,
        Token::HueCool,
        Token::HueWarm,
        Token::Secret,
        Token::OrbRing,
        Token::OrbRingListen,
        Token::OrbRingSpeak,
        Token::OrbRingDim,
        Token::OrbCoreDim,
        Token::OrbOff,
        Token::Bg,
        Token::Surface,
        Token::Sunken,
        Token::Line,
        Token::AccentTint,
        Token::WarnTint,
        Token::DiffAdd,
        Token::DiffAddWord,
        Token::DiffDel,
        Token::DiffDelWord,
        Token::Mark1,
        Token::Mark2,
        Token::Mark3,
        Token::Mark4,
        Token::Mark5,
        Token::Mark6,
        Token::Working,
        Token::Gleam,
        Token::Agent,
        Token::OrbCore,
        Token::Tool,
        Token::ToolMeta,
        Token::RoleResearcher,
        Token::RoleDesigner,
        Token::SynKeyword,
        Token::MarkGone,
    ];

    pub const MARKS: [Token; 6] = [
        Token::Mark1,
        Token::Mark2,
        Token::Mark3,
        Token::Mark4,
        Token::Mark5,
        Token::Mark6,
    ];

    pub fn is_mark(self) -> bool {
        Token::MARKS.contains(&self) || self == Token::MarkGone
    }

    pub const fn name(self) -> &'static str {
        match self {
            Token::Text => "text",
            Token::TextMuted => "text.muted",
            Token::TextFaint => "text.faint",
            Token::TextDisabled => "text.disabled",
            Token::Accent => "accent",
            Token::Thinking => "thinking",
            Token::User => "user",
            Token::Ok => "ok",
            Token::Warn => "warn",
            Token::WarnGlyph => "warn.glyph",
            Token::WarnFill => "warn.fill",
            Token::WarnOn => "warn.on",
            Token::Error => "error",
            Token::RoleCoordinator => "role.coordinator",
            Token::RoleCoder => "role.coder",
            Token::RoleReviewer => "role.reviewer",
            Token::SynString => "syn.string",
            Token::SynNumber => "syn.number",
            Token::SynType => "syn.type",
            Token::SynComment => "syn.comment",
            Token::SynFunction => "syn.function",
            Token::SynVariable => "syn.variable",
            Token::SynControl => "syn.control",
            Token::HueCool => "hue.cool",
            Token::HueWarm => "hue.warm",
            Token::OrbRing => "orb.ring",
            Token::OrbRingListen => "orb.ring.listen",
            Token::OrbRingSpeak => "orb.ring.speak",
            Token::OrbRingDim => "orb.ring.dim",
            Token::OrbCoreDim => "orb.core.dim",
            Token::OrbOff => "orb.off",
            Token::Bg => "bg",
            Token::Surface => "surface",
            Token::Sunken => "sunken",
            Token::Line => "line",
            Token::AccentTint => "accent.tint",
            Token::WarnTint => "warn.tint",
            Token::DiffAdd => "diff.add",
            Token::DiffAddWord => "diff.add.word",
            Token::DiffDel => "diff.del",
            Token::DiffDelWord => "diff.del.word",
            Token::Mark1 => "mark.1",
            Token::Mark2 => "mark.2",
            Token::Mark3 => "mark.3",
            Token::Mark4 => "mark.4",
            Token::Mark5 => "mark.5",
            Token::Mark6 => "mark.6",
            Token::Working => "working",
            Token::Gleam => "gleam",
            Token::Agent => "agent",
            Token::OrbCore => "orb.core",
            Token::Tool => "tool",
            Token::ToolMeta => "tool.meta",
            Token::RoleResearcher => "role.researcher",
            Token::RoleDesigner => "role.designer",
            Token::SynKeyword => "syn.keyword",
            Token::MarkGone => "mark.gone",
            Token::Secret => "secret",
        }
    }

    pub const fn base(self) -> Token {
        match self {
            Token::Agent | Token::OrbCore | Token::SynKeyword => Token::Accent,
            Token::Tool | Token::RoleResearcher | Token::RoleDesigner => Token::Text,
            Token::ToolMeta | Token::MarkGone => Token::TextMuted,
            other => other,
        }
    }

    pub const fn floor(self) -> Option<f64> {
        match self.base() {
            Token::Text
            | Token::TextMuted
            | Token::Accent
            | Token::Thinking
            | Token::Ok
            | Token::Warn
            | Token::Error
            | Token::RoleCoordinator
            | Token::RoleCoder
            | Token::RoleReviewer
            | Token::SynString
            | Token::SynNumber
            | Token::SynType
            | Token::SynComment
            | Token::SynFunction
            | Token::SynVariable
            | Token::SynControl
            | Token::HueCool
            | Token::HueWarm
            | Token::Secret
            | Token::OrbRingListen => Some(TINT_FLOOR),
            Token::TextFaint
            | Token::User
            | Token::WarnGlyph
            | Token::OrbRing
            | Token::OrbCoreDim
            | Token::Mark1
            | Token::Mark2
            | Token::Mark3
            | Token::Mark4
            | Token::Mark5
            | Token::Mark6
            | Token::Working
            | Token::Gleam => Some(GLYPH_FLOOR),
            _ => None,
        }
    }

    pub const fn on_selection(self) -> Token {
        match self {
            Token::TextFaint | Token::TextDisabled | Token::User => Token::TextMuted,
            other => other,
        }
    }

    fn modifiers(self) -> Modifier {
        match self {
            Token::Tool => Modifier::BOLD,
            Token::SynComment => Modifier::ITALIC,
            _ => Modifier::empty(),
        }
    }

    const fn index(self) -> usize {
        self.base() as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn hex(v: u32) -> Rgb {
        Rgb {
            r: (v >> 16) as u8,
            g: (v >> 8) as u8,
            b: v as u8,
        }
    }

    fn channels(self) -> [f64; 3] {
        [f64::from(self.r), f64::from(self.g), f64::from(self.b)]
    }

    fn from_channels(c: [f64; 3]) -> Rgb {
        let ch = |v: f64| v.clamp(0.0, 255.0).round_ties_even() as u8;
        Rgb {
            r: ch(c[0]),
            g: ch(c[1]),
            b: ch(c[2]),
        }
    }
}

impl std::fmt::Display for Rgb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
    }
}

pub fn screen_look(cell: &vt100::Cell) -> Look {
    let fg = match cell.fgcolor() {
        vt100::Color::Default => Token::Text,
        vt100::Color::Idx(0 | 8) => Token::TextMuted,
        vt100::Color::Idx(1 | 9) => Token::Error,
        vt100::Color::Idx(2 | 10) => Token::Ok,
        vt100::Color::Idx(3 | 11) => Token::HueWarm,
        vt100::Color::Idx(4 | 6 | 12 | 14) => Token::Accent,
        vt100::Color::Idx(5 | 13) => Token::Thinking,
        vt100::Color::Idx(_) | vt100::Color::Rgb(..) => Token::Text,
    };
    let mut look = Look::fg(fg);
    if cell.bgcolor() != vt100::Color::Default {
        look = look.on(Token::Surface);
    }
    if cell.bold() {
        look = look.bold();
    }
    if cell.italic() {
        look = look.italic();
    }
    if cell.underline() {
        look = look.underline();
    }
    if cell.inverse() {
        look = look.reverse();
    }
    look
}

pub fn mix(a: Rgb, b: Rgb, u: f64) -> Rgb {
    Rgb::from_channels(mix_channels(a.channels(), b.channels(), u))
}

fn mix_channels(a: [f64; 3], b: [f64; 3], u: f64) -> [f64; 3] {
    let u = u.clamp(0.0, 1.0);
    [0, 1, 2].map(|i| b[i] + (a[i] - b[i]) * u)
}

fn linear(v: f64) -> f64 {
    let v = v / 255.0;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

pub fn luminance(c: Rgb) -> f64 {
    let [r, g, b] = c.channels().map(linear);
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

pub fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

fn oklab(c: Rgb) -> [f64; 3] {
    let [r, g, b] = c.channels().map(linear);
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    [
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    ]
}

pub fn oklab_lightness(c: Rgb) -> f64 {
    oklab(c)[0]
}

pub fn fitted_band(over: Rgb, under: Rgb) -> Rgb {
    let target = contrast(BAND_PAIR.0, BAND_PAIR.1);
    (1..=FIT_STEPS)
        .map(|k| mix(over, under, f64::from(k) / f64::from(FIT_STEPS)))
        .find(|band| contrast(*band, under) >= target)
        .unwrap_or(over)
}

pub fn xterm(index: u8) -> Rgb {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    match index {
        16..=231 => {
            let i = usize::from(index - 16);
            Rgb {
                r: LEVELS[i / 36],
                g: LEVELS[(i / 6) % 6],
                b: LEVELS[i % 6],
            }
        }
        232..=255 => {
            let v = 8 + 10 * (index - 232);
            Rgb { r: v, g: v, b: v }
        }
        _ => Rgb { r: 0, g: 0, b: 0 },
    }
}

pub fn nearest256(c: Rgb) -> u8 {
    let target = oklab(c);
    let distance = |i: u8| {
        let p = oklab(xterm(i));
        (0..3).map(|k| (p[k] - target[k]).powi(2)).sum::<f64>()
    };
    let mut best = 16;
    let mut best_d = f64::INFINITY;
    for i in 16..=255u8 {
        let d = distance(i);
        if d < best_d {
            best = i;
            best_d = d;
        }
    }
    best
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Milli(u16);

impl Milli {
    pub const ZERO: Milli = Milli(0);
    pub const ONE: Milli = Milli(1000);

    pub fn new(v: f64) -> Milli {
        Milli((v.clamp(0.0, 1.0) * 1000.0).round() as u16)
    }

    pub fn get(self) -> f64 {
        f64::from(self.0) / 1000.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tone {
    Token(Token),
    Mix(Token, Token, Milli),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ink {
    Token(Token),
    Mix {
        over: Tone,
        under: Tone,
        u: Milli,
        still: Token,
    },
}

impl Ink {
    pub fn mix(over: Token, under: Token, u: f64, still: Token) -> Ink {
        Ink::Mix {
            over: Tone::Token(over),
            under: Tone::Token(under),
            u: Milli::new(u),
            still,
        }
    }

    pub const fn still(self) -> Token {
        match self {
            Ink::Token(t) | Ink::Mix { still: t, .. } => t,
        }
    }

    pub const fn tone(self) -> Option<Tone> {
        match self {
            Ink::Token(t) => Some(Tone::Token(t)),
            Ink::Mix {
                over: Tone::Token(a),
                under: Tone::Token(b),
                u,
                ..
            } => Some(Tone::Mix(a, b, u)),
            Ink::Mix { .. } => None,
        }
    }
}

impl From<Token> for Ink {
    fn from(t: Token) -> Ink {
        Ink::Token(t)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Depth {
    TrueColor,
    Ansi256,
    Ansi16,
    None,
}

impl Depth {
    pub const fn blends(self) -> bool {
        matches!(self, Depth::TrueColor | Depth::Ansi256)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    Dark,
    Light,
}

impl Mode {
    pub fn of(background: Rgb) -> Mode {
        if oklab_lightness(background) < DARK_BELOW {
            Mode::Dark
        } else {
            Mode::Light
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ansi16 {
    pub code: Option<u8>,
    pub bold: bool,
    pub italic: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Paint {
    Rgb(Rgb),
    Indexed(u8),
    Sgr(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Resolved {
    pub(crate) fg: Option<Paint>,
    pub(crate) bg: Option<Paint>,
    pub(crate) modifiers: Modifier,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Theme {
    depth: Depth,
    mode: Mode,
    bg: Rgb,
    surface: Option<(Rgb, u8)>,
}

impl Theme {
    pub const fn new(mode: Mode, depth: Depth) -> Theme {
        let (stone, ansi256) = match mode {
            Mode::Dark => (&DARK_PLUS, &ANSI256_DARK),
            Mode::Light => (&STONE_LIGHT, &ANSI256_LIGHT),
        };
        let surface = stone[Token::Surface.index()].1;
        let index = match ansi256[Token::Surface.index()].1 {
            Some(i) => i,
            None => 0,
        };
        Theme {
            depth,
            mode,
            bg: stone[Token::Bg.index()].1,
            surface: Some((surface, index)),
        }
    }

    pub fn with_background(self, reported: Option<Rgb>) -> Theme {
        let Some(bg) = reported.filter(|bg| Mode::of(*bg) == self.mode) else {
            return Theme {
                surface: None,
                ..self
            };
        };
        let band = fitted_band(self.rgb(Token::Text), bg);
        Theme {
            bg,
            surface: Some((band, nearest256(band))),
            ..self
        }
    }

    pub const fn depth(&self) -> Depth {
        self.depth
    }

    pub const fn mode(&self) -> Mode {
        self.mode
    }

    pub fn on_tint(&self, t: Token, text: bool) -> Token {
        let floor = if text { TINT_FLOOR } else { GLYPH_FLOOR };
        match t.on_selection() {
            t if self.reads(t.into(), Token::AccentTint.into(), floor) => t,
            _ => Token::Text,
        }
    }

    fn reads(&self, fg: Ink, bg: Ink, floor: f64) -> bool {
        contrast(self.painted(fg), self.painted(bg)) >= floor
    }

    pub fn rgb(&self, t: Token) -> Rgb {
        let table = match self.mode {
            Mode::Dark => &DARK_PLUS,
            Mode::Light => &STONE_LIGHT,
        };
        match t {
            Token::Bg => self.bg,
            Token::Surface => self.surface.map_or(self.bg, |(band, _)| band),
            t => table[t.index()].1,
        }
    }

    pub fn ink_rgb(&self, ink: Ink) -> Rgb {
        match ink {
            Ink::Token(t) => self.rgb(t),
            Ink::Mix { over, under, u, .. } => {
                Rgb::from_channels(mix_channels(self.tone(over), self.tone(under), u.get()))
            }
        }
    }

    fn tone(&self, tone: Tone) -> [f64; 3] {
        match tone {
            Tone::Token(t) => self.rgb(t).channels(),
            Tone::Mix(a, b, u) => {
                mix_channels(self.rgb(a).channels(), self.rgb(b).channels(), u.get())
            }
        }
    }

    pub fn ansi256(&self, t: Token) -> Option<u8> {
        let table = match self.mode {
            Mode::Dark => &ANSI256_DARK,
            Mode::Light => &ANSI256_LIGHT,
        };
        match (t, self.surface) {
            (Token::Surface, Some((_, index))) => Some(index),
            (Token::Surface, None) => table[Token::Bg.index()].1,
            (t, _) => table[t.index()].1,
        }
    }

    pub fn ansi16(&self, t: Token) -> Ansi16 {
        let table = match self.mode {
            Mode::Dark => &ANSI16_DARK,
            Mode::Light => &ANSI16_LIGHT,
        };
        table[t.index()].1
    }

    fn diff_fg(&self, look: &Look) -> Option<Ink> {
        let ink = look.fg?;
        if !look.diff_code {
            return Some(ink);
        }
        match (self.depth, look.bg) {
            (Depth::Ansi16 | Depth::None, _) => None,
            (_, Some(row)) if !self.reads(ink, row, TINT_FLOOR) => Some(Ink::Token(Token::Text)),
            _ => Some(ink),
        }
    }

    fn band_fg(&self, look: &Look, ink: Ink) -> Ink {
        match (self.depth, look.bg, ink) {
            (
                Depth::TrueColor | Depth::Ansi256,
                Some(Ink::Token(Token::AccentTint)),
                Ink::Token(t),
            ) => Ink::Token(self.on_tint(t, look.band_text)),
            _ => ink,
        }
    }

    pub(crate) fn resolve(&self, look: &Look) -> Resolved {
        let mut modifiers = Modifier::empty();
        for (on, m) in [
            (look.bold, Modifier::BOLD),
            (look.italic, Modifier::ITALIC),
            (look.underline, Modifier::UNDERLINED),
            (look.reverse, Modifier::REVERSED),
            (look.strike, Modifier::CROSSED_OUT),
        ] {
            if on {
                modifiers |= m;
            }
        }
        if let Some(Ink::Token(t)) = look.fg {
            modifiers |= t.modifiers();
        }
        if look.bold_without_colour && self.depth == Depth::None {
            modifiers |= Modifier::BOLD;
        }
        let fg = self
            .diff_fg(look)
            .map(|ink| self.band_fg(look, ink))
            .and_then(|ink| self.paint_fg(ink, &mut modifiers));
        let bg = look.bg.and_then(|ink| self.paint_bg(ink, &mut modifiers));
        Resolved { fg, bg, modifiers }
    }

    fn paint_fg(&self, ink: Ink, modifiers: &mut Modifier) -> Option<Paint> {
        match self.depth {
            Depth::TrueColor => Some(Paint::Rgb(self.ink_rgb(ink))),
            Depth::Ansi256 => Some(Paint::Indexed(self.index256(ink))),
            Depth::Ansi16 => {
                let a = self.ansi16(ink.still());
                if a.bold {
                    *modifiers |= Modifier::BOLD;
                }
                if a.italic {
                    *modifiers |= Modifier::ITALIC;
                }
                a.code.map(Paint::Sgr)
            }
            Depth::None => None,
        }
    }

    fn paint_bg(&self, ink: Ink, modifiers: &mut Modifier) -> Option<Paint> {
        if self.surface.is_none() && ink.still() == Token::Surface {
            return None;
        }
        match self.depth {
            Depth::TrueColor => Some(Paint::Rgb(self.ink_rgb(ink))),
            Depth::Ansi256 => {
                if self.mode == Mode::Dark && ink == Ink::Token(Token::DiffAddWord) {
                    *modifiers |= Modifier::BOLD;
                }
                Some(Paint::Indexed(self.index256(ink)))
            }
            Depth::Ansi16 | Depth::None => match (ink.still(), self.depth) {
                (Token::WarnFill, Depth::Ansi16) => Some(Paint::Sgr(33)),
                (
                    Token::WarnFill | Token::AccentTint | Token::DiffAddWord | Token::DiffDelWord,
                    _,
                ) => {
                    modifiers.toggle(Modifier::REVERSED);
                    None
                }
                _ => None,
            },
        }
    }

    pub fn painted(&self, ink: Ink) -> Rgb {
        match self.depth {
            Depth::Ansi256 => xterm(self.index256(ink)),
            _ => self.ink_rgb(ink),
        }
    }

    fn index256(&self, ink: Ink) -> u8 {
        match ink {
            Ink::Token(t) => self.ansi256(t).unwrap_or_else(|| nearest256(self.rgb(t))),
            Ink::Mix { .. } => nearest256(self.ink_rgb(ink)),
        }
    }

    #[allow(
        clippy::disallowed_methods,
        reason = "theme is the one module that puts colours on a ratatui style"
    )]
    pub fn style(&self, look: &Look) -> Style {
        let r = self.resolve(look);
        let mut style = Style::default().add_modifier(r.modifiers);
        if let Some(fg) = r.fg {
            style = style.fg(color(fg));
        }
        if let Some(bg) = r.bg {
            style = style.bg(color(bg));
        }
        style
    }

    pub fn fg(&self, t: Token) -> Style {
        self.style(&Look::fg(t))
    }

    pub fn bg(&self, t: Token) -> Style {
        self.style(&Look::default().on(t))
    }

    pub fn body(&self) -> Style {
        Style::default()
    }

    pub fn fills(&self, t: Token) -> bool {
        self.resolve(&Look::default().on(t)).bg.is_some()
    }

    pub fn line(&self, line: &StyledLine) -> Line<'static> {
        let mut spans: Vec<Span<'static>> = line
            .spans
            .iter()
            .map(|s| Span::styled(s.text.clone(), self.style(&s.look)))
            .collect();
        if let Some((pad, look)) = line.band_pad()
            && pad > 0
            && self.resolve(&look).bg.is_some()
        {
            spans.push(Span::styled(" ".repeat(pad), self.style(&look)));
        }
        Line::from(spans)
    }
}

#[allow(
    clippy::disallowed_methods,
    reason = "theme is the one module that builds colours for ratatui"
)]
fn color(paint: Paint) -> Color {
    match paint {
        Paint::Rgb(c) => Color::Rgb(c.r, c.g, c.b),
        Paint::Indexed(i) => Color::Indexed(i),
        Paint::Sgr(code) => match code {
            30 => Color::Black,
            31 => Color::Red,
            32 => Color::Green,
            33 => Color::Yellow,
            34 => Color::Blue,
            35 => Color::Magenta,
            36 => Color::Cyan,
            37 => Color::Gray,
            90 => Color::DarkGray,
            91 => Color::LightRed,
            92 => Color::LightGreen,
            93 => Color::LightYellow,
            94 => Color::LightBlue,
            95 => Color::LightMagenta,
            96 => Color::LightCyan,
            97 => Color::White,
            _ => Color::Reset,
        },
    }
}

pub fn css(theme: &Theme) -> String {
    let properties: String = Token::ALL
        .iter()
        .map(|t| format!("  --{}: {};\n", t.name().replace('.', "-"), theme.rgb(*t)))
        .collect();
    format!(":root {{\n{properties}}}\n")
}

#[cfg(test)]
mod tests;
