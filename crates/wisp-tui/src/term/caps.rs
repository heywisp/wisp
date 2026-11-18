use std::collections::BTreeMap;
use std::time::Duration;
use wisp_protocol::settings::{
    self, AMBIGUOUS, COLORS, MOTIONS, SIDEBAR_MODES, THEMES, color_word, names, word_of,
};

use wisp_render::motion::{MotionCtx, MotionPolicy};
use wisp_render::theme::glyphs::GlyphSet;
use wisp_render::theme::{Depth, Mode, Rgb, Theme, oklab_lightness};

use super::modes::ScrollBack;
use super::print::Stream;
use super::probe::{ProbePlan, ProbeReport};

pub const ENV_KEYS: [&str; 34] = [
    "TERM",
    "COLORTERM",
    "NO_COLOR",
    "FORCE_COLOR",
    "CLICOLOR",
    "CLICOLOR_FORCE",
    "TERM_PROGRAM",
    "COLORFGBG",
    "WT_SESSION",
    "WSL_DISTRO_NAME",
    "ZELLIJ",
    "TMUX",
    "STY",
    "SSH_CONNECTION",
    "SSH_CLIENT",
    "SSH_TTY",
    "WAYLAND_DISPLAY",
    "DISPLAY",
    "LC_ALL",
    "LC_CTYPE",
    "LANG",
    "WISP_THEME",
    "WISP_COLOR",
    "WISP_MOTION",
    "WISP_AMBIGUOUS",
    "WISP_MOUSE",
    "WISP_SCROLL_SPEED",
    "WISP_FULL_REPAINT",
    "WISP_COPY_ON_SELECT",
    "WISP_SIDEBAR",
    "BROWSER",
    "WSL_INTEROP",
    "CI",
    "VSCODE_IPC_HOOK_CLI",
];

pub const PRESENCE_ONLY: [&str; 10] = [
    "WT_SESSION",
    "ZELLIJ",
    "TMUX",
    "STY",
    "SSH_CONNECTION",
    "SSH_CLIENT",
    "SSH_TTY",
    "WAYLAND_DISPLAY",
    "DISPLAY",
    "VSCODE_IPC_HOOK_CLI",
];

pub const LOCAL_DEADLINE: Duration = Duration::from_millis(250);
pub const VS_CODE_IMAGES: &str =
    "; turn on terminal.integrated.enableImages in VS Code, then restart Wisp";
pub const SSH_DEADLINE: Duration = Duration::from_millis(600);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Env {
    vars: BTreeMap<&'static str, String>,
}

impl Env {
    pub fn current() -> Env {
        let mut vars = BTreeMap::new();
        for key in ENV_KEYS {
            if let Some(value) = std::env::var_os(key) {
                vars.insert(key, value.to_string_lossy().into_owned());
            }
        }
        Env { vars }
    }

    pub fn from_pairs<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Env {
        let mut vars = BTreeMap::new();
        for (key, value) in pairs {
            if let Some(known) = ENV_KEYS.iter().find(|k| **k == key) {
                vars.insert(*known, value.to_owned());
            }
        }
        Env { vars }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.vars.get(key).map(String::as_str)
    }

    pub fn set(&self, key: &str) -> Option<&str> {
        self.get(key).filter(|v| !v.is_empty())
    }

    fn lower(&self, key: &str) -> Option<String> {
        self.set(key).map(|v| v.trim().to_ascii_lowercase())
    }

    pub fn ssh(&self) -> bool {
        ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"]
            .iter()
            .any(|k| self.set(k).is_some())
    }

    pub fn present(&self, key: &str) -> bool {
        self.get(key).is_some_and(|v| !v.trim().is_empty())
    }

    pub fn tmux(&self) -> bool {
        self.present("TMUX")
    }

    pub fn zellij(&self) -> bool {
        self.present("ZELLIJ")
    }

    pub fn screen(&self) -> bool {
        self.present("STY") || (!self.tmux() && self.term_starts("screen"))
    }

    pub fn vscode(&self) -> bool {
        self.get("TERM_PROGRAM") == Some("vscode")
    }

    pub fn windows_terminal(&self) -> bool {
        self.present("WT_SESSION")
    }

    fn term_starts(&self, prefix: &str) -> bool {
        self.get("TERM").is_some_and(|t| t.starts_with(prefix))
    }

    pub fn multiplexer(&self) -> Option<String> {
        let named = if self.tmux() {
            "tmux"
        } else if self.present("STY") {
            "GNU screen"
        } else if self.zellij() {
            "Zellij"
        } else if self.get("TERM_PROGRAM") == Some("herdr") {
            "Herdr"
        } else if self.term_starts("tmux") || self.screen() {
            return self.get("TERM").map(|t| format!("TERM={}", terminfo(t)));
        } else {
            return None;
        };
        Some(named.to_owned())
    }

    pub fn dumb(&self) -> bool {
        self.lower("TERM").as_deref() == Some("dumb")
    }

    pub fn apple_terminal(&self) -> bool {
        self.get("TERM_PROGRAM") == Some("Apple_Terminal")
    }

    pub fn locale(&self) -> Option<(&'static str, &str)> {
        ["LC_ALL", "LC_CTYPE", "LANG"]
            .into_iter()
            .find_map(|k| self.set(k).map(|v| (k, v)))
    }

    pub fn utf8(&self) -> Option<bool> {
        self.locale().map(|(_, v)| {
            let v = v.to_ascii_lowercase();
            v.contains("utf-8") || v.contains("utf8")
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Streams {
    pub stdin_tty: bool,
    pub stdout_tty: bool,
    pub stderr_tty: bool,
}

impl Streams {
    pub fn current() -> Streams {
        Streams {
            stdin_tty: rustix::termios::isatty(std::io::stdin()),
            stdout_tty: rustix::termios::isatty(std::io::stdout()),
            stderr_tty: rustix::termios::isatty(std::io::stderr()),
        }
    }
}

pub const SESSION_SOURCE: &str = "set this session";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reach {
    pub session: bool,
    pub saved: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SavedDisplay {
    values: BTreeMap<String, String>,
    flags: BTreeMap<String, String>,
    session: BTreeMap<String, String>,
}

fn pairs_map<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> BTreeMap<String, String> {
    pairs
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

fn on_off(value: &str) -> Option<bool> {
    match value {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    }
}

pub(crate) const DEPTHS: [Option<Depth>; COLORS.len()] = [
    None,
    Some(Depth::TrueColor),
    Some(Depth::Ansi256),
    Some(Depth::Ansi16),
    Some(Depth::None),
];
pub(crate) const MODES: [Option<Mode>; THEMES.len()] = [None, Some(Mode::Dark), Some(Mode::Light)];
pub(crate) const GLYPH_SETS: [Option<GlyphSet>; AMBIGUOUS.len()] =
    [None, Some(GlyphSet::AmbiguousWide), Some(GlyphSet::Unicode)];
pub(crate) const MOTION_POLICIES: [Option<MotionPolicy>; MOTIONS.len()] = [
    None,
    Some(MotionPolicy::Full),
    Some(MotionPolicy::Calm),
    Some(MotionPolicy::Reduced),
];
const SIDEBARS: [SidebarSetting; SIDEBAR_MODES.len()] =
    [SidebarSetting::Auto, SidebarSetting::Hidden];

fn hint<T>(words: &[&str], values: &[Option<T>]) -> String {
    let set: Vec<&str> = words
        .iter()
        .zip(values)
        .filter_map(|(w, v)| v.as_ref().map(|_| *w))
        .collect();
    set.join(", ")
}

fn depth_named(value: &str) -> Option<Depth> {
    settings::named(COLORS, &DEPTHS, value).flatten()
}

fn mode_named(value: &str) -> Option<Mode> {
    settings::named(THEMES, &MODES, value).flatten()
}

fn glyphs_named(value: &str) -> Option<GlyphSet> {
    settings::named(AMBIGUOUS, &GLYPH_SETS, value).flatten()
}

fn motion_named(value: &str) -> Option<MotionPolicy> {
    settings::named(MOTIONS, &MOTION_POLICIES, value).flatten()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SidebarSetting {
    #[default]
    Auto,
    Hidden,
}

impl SidebarSetting {
    pub fn word(self) -> &'static str {
        word_of(SIDEBAR_MODES, &SIDEBARS, &self).unwrap_or_default()
    }
}

fn sidebar_named(value: &str) -> Option<SidebarSetting> {
    settings::named(SIDEBAR_MODES, &SIDEBARS, value)
}

fn speed_named(value: &str) -> Option<u16> {
    value
        .parse::<u16>()
        .ok()
        .filter(|n| (1..=SCROLL_SPEED_MAX).contains(n))
}

impl SavedDisplay {
    pub fn from_pairs<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> SavedDisplay {
        SavedDisplay {
            values: pairs_map(pairs),
            flags: BTreeMap::new(),
            session: BTreeMap::new(),
        }
    }

    pub fn changed(&mut self, name: &str, value: Option<&str>, reach: Reach) {
        let maps = [
            (reach.session, &mut self.session),
            (reach.saved, &mut self.values),
        ];
        for (_, map) in maps.into_iter().filter(|(touched, _)| *touched) {
            match value {
                Some(value) => {
                    map.insert(name.to_owned(), value.to_owned());
                }
                None => {
                    map.remove(name);
                }
            }
        }
    }

    pub fn session(&self, name: &str) -> Option<&str> {
        self.session.get(name).map(String::as_str)
    }

    pub fn with_flags<'a>(
        mut self,
        pairs: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> SavedDisplay {
        self.flags = pairs_map(pairs);
        self
    }

    fn pick<T>(
        map: &BTreeMap<String, String>,
        name: &str,
        parse: impl Fn(&str) -> Option<T>,
        source: impl Fn(&str) -> String,
    ) -> Option<Choice<T>> {
        let value = map.get(name).map(String::as_str)?;
        parse(value).map(|v| Choice::new(v, source(value)))
    }

    fn flag<T>(&self, name: &str, parse: impl Fn(&str) -> Option<T>) -> Option<Choice<T>> {
        let set = |value: &str| (value != "auto").then(|| parse(value)).flatten();
        self.flag_as(name, set)
    }

    fn flag_as<T>(&self, name: &str, parse: impl Fn(&str) -> Option<T>) -> Option<Choice<T>> {
        Self::pick(&self.session, name, &parse, |_| SESSION_SOURCE.to_owned())
            .or_else(|| Self::pick(&self.flags, name, parse, |v| format!("--set {name}={v}")))
    }

    fn saved<T>(&self, name: &str, parse: impl Fn(&str) -> Option<T>) -> Option<Choice<T>> {
        let set = |value: &str| (value != "auto").then(|| parse(value)).flatten();
        self.saved_as(name, set)
    }

    fn saved_as<T>(&self, name: &str, parse: impl Fn(&str) -> Option<T>) -> Option<Choice<T>> {
        Self::pick(&self.values, name, parse, |v| {
            format!("{} {name} = {v}", wisp_protocol::CONFIG_FILE)
        })
    }

    pub fn section_paths(&self) -> std::collections::BTreeSet<String> {
        let slots = format!("{}.", wisp_protocol::settings::PLUGINS_GROUP);
        let table = format!(".{}.", wisp_protocol::settings::SLOTS_TABLE);
        [&self.values, &self.flags, &self.session]
            .into_iter()
            .flat_map(|map| map.keys())
            .filter(|key| key.starts_with(&slots) && key.contains(&table))
            .cloned()
            .collect()
    }

    pub fn section(&self, path: &str) -> Option<Choice<bool>> {
        self.flag_as(path, on_off)
            .or_else(|| self.saved_as(path, on_off))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags {
    pub ascii: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice<T> {
    pub value: T,
    pub source: String,
}

impl<T> Choice<T> {
    pub(crate) fn new(value: T, source: impl Into<String>) -> Choice<T> {
        Choice {
            value,
            source: source.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeSkip {
    StdinNotTerminal,
    StdoutNotTerminal,
    DumbTerminal,
}

impl ProbeSkip {
    pub const fn reason(self) -> &'static str {
        match self {
            ProbeSkip::StdinNotTerminal => "stdin is not a terminal",
            ProbeSkip::StdoutNotTerminal => "stdout is not a terminal",
            ProbeSkip::DumbTerminal => "TERM is dumb",
        }
    }
}

pub fn probe_plan(env: &Env, streams: Streams, flags: Flags) -> Result<ProbePlan, ProbeSkip> {
    if !streams.stdout_tty {
        return Err(ProbeSkip::StdoutNotTerminal);
    }
    if !streams.stdin_tty {
        return Err(ProbeSkip::StdinNotTerminal);
    }
    if env.dumb() {
        return Err(ProbeSkip::DumbTerminal);
    }
    Ok(ProbePlan {
        deadline: if env.ssh() {
            SSH_DEADLINE
        } else {
            LOCAL_DEADLINE
        },
        sync_query: !env.apple_terminal(),
        scroll_query: !env.apple_terminal(),
        width_test: !flags.ascii && env.utf8() != Some(false),
        pictures: !env.apple_terminal() && env.multiplexer().is_none(),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pictures {
    Braille,
    Kitty,
    Sixel,
}

const CELL_PIXELS: std::ops::RangeInclusive<u16> = 4..=128;

#[derive(Clone, Debug, PartialEq)]
pub struct Caps {
    pub depth: Choice<Depth>,
    pub stderr_depth: Choice<Depth>,
    pub mode: Choice<Mode>,
    pub glyphs: Choice<GlyphSet>,
    pub motion: Choice<MotionPolicy>,
    pub alternate_screen: Choice<bool>,
    pub mouse: Choice<bool>,
    pub alternate_scroll: Choice<ScrollBack>,
    pub scroll_speed: Choice<u16>,
    pub full_repaint: Choice<bool>,
    pub picture_setting: Choice<bool>,
    pub pictures: Choice<Pictures>,
    pub cell_pixels: Option<(u16, u16)>,
    pub kitty_keyboard: bool,
    pub synchronized_output: Option<bool>,
    pub copy_on_select: Option<Choice<bool>>,
    pub verbose: Choice<bool>,
    pub sidebar: Choice<SidebarSetting>,
    pub sidebar_folded: Choice<Vec<String>>,
    pub sidebar_pinned: Choice<Vec<String>>,
    pub background: Option<Rgb>,
    pub warnings: Vec<String>,
}

pub const COPY_ON_SELECT: &str = "WISP_COPY_ON_SELECT";

impl Caps {
    pub fn resolve(env: &Env, streams: Streams, flags: Flags, probe: Option<&ProbeReport>) -> Caps {
        Self::resolve_saved(env, streams, flags, &SavedDisplay::default(), probe)
    }

    pub fn resolve_saved(
        env: &Env,
        streams: Streams,
        flags: Flags,
        saved: &SavedDisplay,
        probe: Option<&ProbeReport>,
    ) -> Caps {
        let mut warnings = vec![];
        let mut caps = Caps {
            depth: depth(
                env,
                Stream::Stdout,
                streams.stdout_tty,
                saved,
                &mut warnings,
            ),
            stderr_depth: depth(env, Stream::Stderr, streams.stderr_tty, saved, &mut vec![]),
            mode: mode(env, saved, probe, &mut warnings),
            glyphs: glyphs(env, flags, saved, probe, &mut warnings),
            motion: motion(env, streams, saved, &mut warnings),
            alternate_screen: alternate_screen(env),
            mouse: mouse(env, saved, &mut warnings),
            alternate_scroll: alternate_scroll(env, probe),
            scroll_speed: scroll_speed(env, saved, &mut warnings),
            full_repaint: full_repaint(env, saved, &mut warnings),
            picture_setting: saved
                .flag(names::DISPLAY_PICTURES, on_off)
                .or_else(|| saved.saved(names::DISPLAY_PICTURES, on_off))
                .unwrap_or_else(|| Choice::new(true, "default")),
            pictures: Choice::new(Pictures::Braille, ""),
            cell_pixels: probe
                .and_then(|p| p.cell_pixels)
                .filter(|(w, h)| CELL_PIXELS.contains(w) && CELL_PIXELS.contains(h)),
            copy_on_select: saved
                .flag(names::DISPLAY_COPY_ON_SELECT, on_off)
                .or_else(|| {
                    switch(env, COPY_ON_SELECT, &mut warnings)
                        .map(|(on, source)| Choice::new(on, source))
                })
                .or_else(|| saved.saved(names::DISPLAY_COPY_ON_SELECT, on_off)),
            verbose: saved
                .flag(names::DISPLAY_VERBOSE, on_off)
                .or_else(|| saved.saved(names::DISPLAY_VERBOSE, on_off))
                .unwrap_or_else(|| Choice::new(false, "default")),
            sidebar: sidebar(env, saved, &mut warnings),
            sidebar_folded: sections(saved, names::DISPLAY_SIDEBAR_FOLDED),
            sidebar_pinned: sections(saved, names::DISPLAY_SIDEBAR_PINNED),
            kitty_keyboard: probe.is_some_and(|p| p.kitty_keyboard.is_some()),
            synchronized_output: probe.and_then(|p| p.synchronized_output.supported()),
            background: probe.and_then(|p| p.background),
            warnings,
        };
        caps.pictures = pictures(env, &caps, probe);
        caps
    }

    pub fn picture(&self) -> Option<(Pictures, (u16, u16))> {
        let method = self.pictures.value;
        self.cell_pixels
            .filter(|_| method != Pictures::Braille)
            .map(|cell| (method, cell))
    }

    pub fn theme(&self) -> Theme {
        self.theme_on(Stream::Stdout)
    }

    pub fn theme_on(&self, stream: Stream) -> Theme {
        Theme::new(self.mode.value, self.depth_on(stream)).with_background(self.background)
    }

    pub const fn depth_on(&self, stream: Stream) -> Depth {
        match stream {
            Stream::Stdout => self.depth.value,
            Stream::Stderr => self.stderr_depth.value,
        }
    }

    pub fn with_theme(mut self, theme: Theme) -> Caps {
        self.depth.value = theme.depth();
        self.mode.value = theme.mode();
        self
    }

    pub const fn motion_ctx(&self) -> MotionCtx {
        MotionCtx::new(self.motion.value, self.glyphs.value, self.depth.value)
    }
}

pub const GNU_SCREEN_ALTERNATE: &str = "inside GNU screen, which shows the alternate screen only with `altscreen on` and does not report it";

fn alternate_screen(env: &Env) -> Choice<bool> {
    if env.present("STY") {
        return Choice::new(false, GNU_SCREEN_ALTERNATE);
    }
    Choice::new(true, "default")
}

pub const VALUE_HIDDEN: &str = "(not shown)";
pub const SIMPLE_MAX: usize = 16;
pub const NAMED_MAX: usize = 32;
pub const TERM_SEGMENT_MAX: usize = 12;

fn plain(value: &str, max: usize, extra: &[char]) -> bool {
    value.chars().count() <= max
        && value.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '.' | '_' | '+' | ';' | '-')
                || extra.contains(&c)
        })
}

fn quoted(value: &str) -> &str {
    if plain(value, SIMPLE_MAX, &[]) {
        value
    } else {
        VALUE_HIDDEN
    }
}

fn named(value: &str) -> &str {
    if plain(value, NAMED_MAX, &['@']) {
        value
    } else {
        VALUE_HIDDEN
    }
}

fn names_a_place(key: &str) -> bool {
    key == "LANG" || key == "WSL_DISTRO_NAME" || key.starts_with("LC_")
}

fn terminfo(value: &str) -> &str {
    let shaped = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '+' | '-'))
        && value
            .split('-')
            .all(|segment| segment.chars().count() <= TERM_SEGMENT_MAX);
    if shaped { value } else { VALUE_HIDDEN }
}

pub fn shown_env<'a>(key: &str, value: &'a str) -> &'a str {
    if key == "TERM" {
        terminfo(value)
    } else if names_a_place(key) {
        named(value)
    } else {
        quoted(value)
    }
}

fn ignored(warnings: &mut Vec<String>, key: &str, value: &str, expected: &str) {
    let value = quoted(value);
    warnings.push(format!("{key}={value} is not one of {expected}; ignored"));
}

fn detected_depth(env: &Env) -> Choice<Depth> {
    if let Some(v) = env.lower("COLORTERM")
        && color_word(&v) == "truecolor"
    {
        return Choice::new(Depth::TrueColor, format!("COLORTERM={v}"));
    }
    if env.tmux() {
        return Choice::new(Depth::Ansi256, "inside tmux, without COLORTERM=truecolor");
    }
    if env.windows_terminal() {
        return Choice::new(Depth::TrueColor, "WT_SESSION is set (Windows Terminal)");
    }
    match env.set("TERM") {
        Some(t) if t.contains("256") => {
            Choice::new(Depth::Ansi256, format!("TERM={}", terminfo(t)))
        }
        Some(t) if !env.dumb() => Choice::new(Depth::Ansi16, format!("TERM={}", terminfo(t))),
        Some(_) => Choice::new(Depth::Ansi16, "TERM=dumb, but colour is forced"),
        None => Choice::new(Depth::Ansi16, "default"),
    }
}

fn falsy(v: &str) -> bool {
    matches!(v, "0" | "false" | "no" | "off")
}

fn depth(
    env: &Env,
    stream: Stream,
    tty: bool,
    saved: &SavedDisplay,
    warnings: &mut Vec<String>,
) -> Choice<Depth> {
    if env.set("NO_COLOR").is_some() {
        for key in ["WISP_COLOR", "FORCE_COLOR", "CLICOLOR_FORCE"] {
            if let Some(v) = env.set(key) {
                let v = quoted(v);
                warnings.push(format!("NO_COLOR is set, so {key}={v} is ignored"));
            }
        }
        return Choice::new(Depth::None, "NO_COLOR is set");
    }
    if let Some(choice) = saved.flag(names::DISPLAY_COLOR, depth_named) {
        return choice;
    }
    if let Some(v) = env.lower("WISP_COLOR") {
        match depth_named(color_word(&v)) {
            Some(d) => return Choice::new(d, format!("WISP_COLOR={v}")),
            None => ignored(warnings, "WISP_COLOR", &v, &hint(COLORS, &DEPTHS)),
        }
    }
    if let Some(choice) = saved.saved(names::DISPLAY_COLOR, depth_named) {
        return choice;
    }
    if let Some(raw) = env.get("FORCE_COLOR") {
        let v = raw.trim().to_ascii_lowercase();
        let level = match v.as_str() {
            "" | "1" | "true" | "yes" | "on" => Some(None),
            "2" => Some(Some(Depth::Ansi256)),
            "3" => Some(Some(Depth::TrueColor)),
            _ if falsy(&v) => {
                return Choice::new(Depth::None, format!("FORCE_COLOR={v}"));
            }
            _ => None,
        };
        match level {
            Some(Some(d)) => return Choice::new(d, format!("FORCE_COLOR={v}")),
            Some(None) => {
                let found = detected_depth(env);
                return Choice::new(found.value, format!("{}; FORCE_COLOR={v}", found.source));
            }
            None => ignored(warnings, "FORCE_COLOR", &v, "0, 1, 2, 3"),
        }
    }
    if let Some(v) = env.lower("CLICOLOR_FORCE")
        && !falsy(&v)
    {
        let found = detected_depth(env);
        let v = quoted(&v);
        return Choice::new(found.value, format!("{}; CLICOLOR_FORCE={v}", found.source));
    }
    if !tty {
        return Choice::new(Depth::None, format!("{} is not a terminal", stream.name()));
    }
    if let Some(v) = env.lower("CLICOLOR")
        && falsy(&v)
    {
        return Choice::new(Depth::None, format!("CLICOLOR={v}"));
    }
    if env.dumb() {
        return Choice::new(Depth::None, "TERM=dumb");
    }
    detected_depth(env)
}

fn colorfgbg(value: &str) -> Option<Mode> {
    let bg: u8 = value.rsplit(';').next()?.trim().parse().ok()?;
    match bg {
        0..=6 | 8 => Some(Mode::Dark),
        7 | 9..=15 => Some(Mode::Light),
        _ => None,
    }
}

fn mode(
    env: &Env,
    saved: &SavedDisplay,
    probe: Option<&ProbeReport>,
    warnings: &mut Vec<String>,
) -> Choice<Mode> {
    if let Some(choice) = saved.flag(names::DISPLAY_THEME, mode_named) {
        return choice;
    }
    if let Some(v) = env.lower("WISP_THEME")
        && v != "auto"
    {
        match mode_named(&v) {
            Some(m) => return Choice::new(m, format!("WISP_THEME={v}")),
            None => {
                let expected = format!("{}, auto", hint(THEMES, &MODES));
                ignored(warnings, "WISP_THEME", &v, &expected);
            }
        }
    }
    if let Some(choice) = saved.saved(names::DISPLAY_THEME, mode_named) {
        return choice;
    }
    if let Some(bg) = probe.and_then(|p| p.background) {
        let l = oklab_lightness(bg);
        return Choice::new(
            Mode::of(bg),
            format!("OSC 11 background {bg}, OKLab L {l:.2}"),
        );
    }
    if let Some(v) = env.set("COLORFGBG") {
        match colorfgbg(v) {
            Some(mode) => return Choice::new(mode, format!("COLORFGBG={v}")),
            None => warnings.push(format!(
                "COLORFGBG={} has no background index; ignored",
                quoted(v)
            )),
        }
    }
    Choice::new(Mode::Dark, "default")
}

pub fn plain_glyphs(env: &Env) -> GlyphSet {
    glyphs(
        env,
        Flags::default(),
        &SavedDisplay::default(),
        None,
        &mut Vec::new(),
    )
    .value
}

fn glyphs(
    env: &Env,
    flags: Flags,
    saved: &SavedDisplay,
    probe: Option<&ProbeReport>,
    warnings: &mut Vec<String>,
) -> Choice<GlyphSet> {
    if flags.ascii {
        return Choice::new(GlyphSet::Ascii, "--ascii");
    }
    if env.dumb() {
        return Choice::new(GlyphSet::Ascii, "TERM=dumb");
    }
    if let Some((key, value)) = env.locale()
        && env.utf8() == Some(false)
    {
        let value = shown_env(key, value);
        return Choice::new(GlyphSet::Ascii, format!("{key}={value} is not UTF-8"));
    }
    if let Some(choice) = saved.flag(names::DISPLAY_AMBIGUOUS, glyphs_named) {
        return choice;
    }
    if let Some(v) = env.lower("WISP_AMBIGUOUS") {
        match glyphs_named(&v) {
            Some(g) => return Choice::new(g, format!("WISP_AMBIGUOUS={v}")),
            None => ignored(
                warnings,
                "WISP_AMBIGUOUS",
                &v,
                &hint(AMBIGUOUS, &GLYPH_SETS),
            ),
        }
    }
    if let Some(choice) = saved.saved(names::DISPLAY_AMBIGUOUS, glyphs_named) {
        return choice;
    }
    if let Some(width) = probe.and_then(|p| p.dot_width) {
        if width == 2 {
            return Choice::new(
                GlyphSet::AmbiguousWide,
                "the probe measured U+00B7 as 2 cells",
            );
        }
        return Choice::new(
            GlyphSet::Unicode,
            format!("the probe measured U+00B7 as {width} cell"),
        );
    }
    Choice::new(GlyphSet::Unicode, "default")
}

fn motion(
    env: &Env,
    streams: Streams,
    saved: &SavedDisplay,
    warnings: &mut Vec<String>,
) -> Choice<MotionPolicy> {
    let requested = env.lower("WISP_MOTION");
    let forced = if env.dumb() {
        Some("TERM=dumb")
    } else if !streams.stdout_tty {
        Some("stdout is not a terminal")
    } else {
        None
    };
    if let Some(reason) = forced {
        return Choice::new(MotionPolicy::Reduced, reason);
    }
    if let Some(choice) = saved.flag(names::DISPLAY_MOTION, motion_named) {
        return choice;
    }
    if let Some(v) = requested {
        match motion_named(&v) {
            Some(m) => return Choice::new(m, format!("WISP_MOTION={v}")),
            None => ignored(
                warnings,
                "WISP_MOTION",
                &v,
                &hint(MOTIONS, &MOTION_POLICIES),
            ),
        }
    }
    if let Some(choice) = saved.saved(names::DISPLAY_MOTION, motion_named) {
        return choice;
    }
    Choice::new(MotionPolicy::Full, "default")
}

pub const SCROLL_SPEED_MAX: u16 = 20;

pub(crate) fn switch(env: &Env, key: &str, warnings: &mut Vec<String>) -> Option<(bool, String)> {
    let v = env.lower(key)?;
    match v.as_str() {
        "1" | "true" | "yes" | "on" => Some((true, format!("{key}={v}"))),
        "0" | "false" | "no" | "off" => Some((false, format!("{key}={v}"))),
        _ => {
            ignored(warnings, key, &v, "1, 0");
            None
        }
    }
}

fn layered_switch(
    env: &Env,
    key: &str,
    saved: &SavedDisplay,
    name: &str,
    warnings: &mut Vec<String>,
) -> Option<Choice<bool>> {
    saved
        .flag(name, on_off)
        .or_else(|| switch(env, key, warnings).map(|(on, source)| Choice::new(on, source)))
        .or_else(|| saved.saved(name, on_off))
}

fn mouse(env: &Env, saved: &SavedDisplay, warnings: &mut Vec<String>) -> Choice<bool> {
    layered_switch(env, "WISP_MOUSE", saved, names::DISPLAY_MOUSE, warnings)
        .unwrap_or_else(|| Choice::new(true, "default"))
}

fn full_repaint(env: &Env, saved: &SavedDisplay, warnings: &mut Vec<String>) -> Choice<bool> {
    layered_switch(
        env,
        "WISP_FULL_REPAINT",
        saved,
        names::DISPLAY_FULL_REPAINT,
        warnings,
    )
    .unwrap_or_else(|| Choice::new(false, "default: only changed cells are written"))
}

fn pictures(env: &Env, caps: &Caps, probe: Option<&ProbeReport>) -> Choice<Pictures> {
    let braille = Pictures::Braille;
    if !caps.picture_setting.value {
        return Choice::new(braille, "the setting is off");
    }
    if caps.full_repaint.value {
        return Choice::new(braille, "full repaint rewrites every cell each frame");
    }
    if !caps.depth.value.blends() {
        return Choice::new(braille, "fewer than 256 colours");
    }
    if let Some(name) = env.multiplexer() {
        return Choice::new(braille, format!("inside a multiplexer, {name}"));
    }
    if env.get("TERM_PROGRAM") == Some("WarpTerminal") {
        return Choice::new(braille, "Warp loses mouse clicks over pictures");
    }
    let attributes = probe.and_then(|p| p.device_attributes.as_deref());
    let sixel = attributes.is_some_and(|a| a.iter().skip(1).any(|&n| n == 4));
    let reported = caps
        .background
        .is_some_and(|bg| Mode::of(bg) == caps.mode.value);
    match (probe.and_then(|p| p.graphics), caps.cell_pixels) {
        (Some(true), Some((w, h))) => Choice::new(
            Pictures::Kitty,
            format!("the kitty graphics query said OK, cells {w}x{h} px"),
        ),
        (Some(true), None) => Choice::new(braille, "no usable cell size"),
        _ if !sixel && env.vscode() => {
            Choice::new(braille, format!("no image reply{VS_CODE_IMAGES}"))
        }
        _ if !sixel => Choice::new(braille, "no image reply"),
        _ if !env.windows_terminal() => Choice::new(
            braille,
            "Sixel is drawn only where WT_SESSION is set (Windows Terminal)",
        ),
        _ if !reported => Choice::new(
            braille,
            "Sixel needs the background reported in the theme's mode",
        ),
        (_, None) => Choice::new(braille, "no usable cell size"),
        (_, Some((w, h))) => Choice::new(
            Pictures::Sixel,
            format!("DA1 lists Sixel in Windows Terminal, cells {w}x{h} px"),
        ),
    }
}

fn scroll_speed(env: &Env, saved: &SavedDisplay, warnings: &mut Vec<String>) -> Choice<u16> {
    if let Some(choice) = saved.flag(names::DISPLAY_SCROLL_SPEED, speed_named) {
        return choice;
    }
    if let Some(v) = env.lower("WISP_SCROLL_SPEED") {
        match v.parse::<u16>() {
            Ok(n) if (1..=SCROLL_SPEED_MAX).contains(&n) => {
                return Choice::new(n, format!("WISP_SCROLL_SPEED={v}"));
            }
            _ => ignored(warnings, "WISP_SCROLL_SPEED", &v, "1 to 20"),
        }
    }
    if let Some(choice) = saved.saved(names::DISPLAY_SCROLL_SPEED, speed_named) {
        return choice;
    }
    Choice::new(1, "default")
}

fn sidebar(env: &Env, saved: &SavedDisplay, warnings: &mut Vec<String>) -> Choice<SidebarSetting> {
    if let Some(choice) = saved.flag_as(names::DISPLAY_SIDEBAR, sidebar_named) {
        return choice;
    }
    if let Some(v) = env.lower("WISP_SIDEBAR") {
        match sidebar_named(&v) {
            Some(setting) => return Choice::new(setting, format!("WISP_SIDEBAR={v}")),
            None => ignored(warnings, "WISP_SIDEBAR", &v, &SIDEBAR_MODES.join(", ")),
        }
    }
    if let Some(choice) = saved.saved_as(names::DISPLAY_SIDEBAR, sidebar_named) {
        return choice;
    }
    Choice::new(SidebarSetting::Auto, "default")
}

fn section_names(value: &str) -> Option<Vec<String>> {
    if value.is_empty() {
        return Some(Vec::new());
    }
    match wisp_protocol::settings::find(names::DISPLAY_SIDEBAR_FOLDED)?.parse(value) {
        Ok(wisp_protocol::settings::SettingValue::List(names)) => Some(names),
        _ => None,
    }
}

fn sections(saved: &SavedDisplay, name: &str) -> Choice<Vec<String>> {
    saved
        .flag_as(name, section_names)
        .or_else(|| saved.saved_as(name, section_names))
        .unwrap_or_else(|| Choice::new(Vec::new(), "default"))
}

fn alternate_scroll(env: &Env, probe: Option<&ProbeReport>) -> Choice<ScrollBack> {
    match probe.and_then(|p| p.alternate_scroll) {
        Some(1 | 3) => Choice::new(ScrollBack::Set, "DECRQM 1007 reported it set"),
        Some(2 | 4) => Choice::new(ScrollBack::Untouched, "DECRQM 1007 reported it reset"),
        _ if env.windows_terminal() => Choice::new(
            ScrollBack::Set,
            "WT_SESSION is set (Windows Terminal turns it on by default)",
        ),
        _ => Choice::new(ScrollBack::Untouched, "unknown, so it is left alone"),
    }
}

#[cfg(test)]
mod tests;
