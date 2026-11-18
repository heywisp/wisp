use std::collections::BTreeMap;

use wisp_protocol::settings::{
    AMBIGUOUS, COLORS, Default, FileState, Kind, Layer, MOTIONS, Place, ResolvedSetting,
    SavedEntry, SavedValue, Section, SettingSpec, SettingValue, SettingsSnapshot, SitePath, THEMES,
    TokensOrPercent, find, names, thousands, word_of,
};

use super::detail::chat_layer;
use super::edit::{Context, compact_value, layer_site};
use crate::term::caps::{DEPTHS, GLYPH_SETS, MODES, MOTION_POLICIES, Reach};
use crate::term::{Caps, Choice, Env, Flags, SavedDisplay, Streams};

pub const ORDER: &[&str] = &[
    names::MODEL,
    names::EFFORT,
    names::ENDPOINT,
    names::CACHE_TTL,
    names::DEBUG_CACHE,
    names::REFUSAL_FALLBACK,
    names::MESSAGE_THREADS,
    names::RETRY_BUDGET_USD,
    names::TIME,
    names::CONTEXT_CONTEXT_NOTE,
    names::SELF_CHECK_BEFORE_END,
    names::INSTRUCTIONS_SOURCE,
    names::INSTRUCTIONS_EXTRA,
    names::INSTRUCTIONS_WARN_TOKENS,
    names::CONTEXT_AUTO_COMPACT,
    names::COMPACT_AT,
    names::CONTEXT_VERBATIM_USER_TOKENS,
    names::CONTEXT_BACKGROUND_COMPACTION,
    names::CONTEXT_BACKGROUND_TOKENS,
    names::CONTEXT_COMPACT_EFFORT,
    names::CONTEXT_STRATEGY,
    names::CONTEXT_COUNT_TOKENS,
    names::TOOLS,
    names::TURNED_OFF,
    names::PRELOAD,
    names::AGENTS_MAX_RUNNING,
    names::AGENTS_MAX_TREE_RUNNING,
    names::AGENTS_MAX_AGENTS,
    names::AGENTS_MAX_DEPTH,
    names::AGENTS_WARM_REFRESHES,
    names::KEEP_WARM,
    names::AGENTS_WAKE_WHEN_STUCK,
    names::PRESET,
    names::VOICE_ENDPOINT,
    names::VOICE_VOICE,
    names::VOICE_LANGUAGE,
    names::VOICE_INSTRUCTIONS,
    names::LOG,
    names::KEYCHAIN,
    names::TOOL_OUTPUT_MAX_AGE_DAYS,
    names::CLAUDE_USAGE,
    names::DISPLAY_THEME,
    names::DISPLAY_COLOR,
    names::DISPLAY_MOTION,
    names::DISPLAY_AMBIGUOUS,
    names::DISPLAY_MOUSE,
    names::DISPLAY_SCROLL_SPEED,
    names::DISPLAY_SIDEBAR,
    names::DISPLAY_SIDEBAR_FOLDED,
    names::DISPLAY_SIDEBAR_PINNED,
    names::DISPLAY_COPY_ON_SELECT,
    names::DISPLAY_FULL_REPAINT,
    names::DISPLAY_PICTURES,
    names::DISPLAY_VERBOSE,
];

pub const EVERYDAY: &[&str] = &[
    names::MODEL,
    names::EFFORT,
    names::ENDPOINT,
    names::TOOLS,
    names::COMPACT_AT,
    names::DISPLAY_THEME,
];

pub const ENTRY_PANES: &[&str] = &[
    names::CONTEXT_WINDOW,
    names::PRICING,
    names::ALIASES,
    names::AUTH,
    names::KEY_ENV,
    names::KEY_STORE,
    names::MAX_IN_FLIGHT,
    names::MAX_OUTPUT,
    names::NATIVE_COMPACTION,
    names::PRICE_CACHE_READ,
    names::PRICE_INPUT,
    names::PRICE_OUTPUT,
    names::PROVIDER,
    names::URL_ENV,
];

pub const OFF_SCREEN: &[&str] = &[names::NATIVE_WEB_SEARCH, names::WEB_SEARCH];

pub const DETECTED: &str = "detected";
pub const RULE: &str = "rule";
pub const DEFAULT: &str = "default";

pub(super) fn count(on: usize, total: usize) -> String {
    if on == 0 {
        "none on".to_owned()
    } else if on == total {
        "all on".to_owned()
    } else {
        format!("{on} of {total}")
    }
}

pub fn placed() -> impl Iterator<Item = (Section, &'static SettingSpec)> {
    ORDER
        .iter()
        .filter_map(|name| find(name).map(|spec| (spec.section, spec)))
        .chain(
            wisp_protocol::settings::all()
                .filter(|spec| spec.is_declared())
                .map(|spec| (spec.section, spec)),
        )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Set {
    Layer(Layer),
    Detected,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub set: Set,
    pub value: String,
    pub origin: String,
    pub line: Option<u64>,
}

impl Set {
    pub fn site(&self) -> Option<&'static str> {
        match self {
            Set::Layer(layer) => layer_site(layer),
            Set::Detected => None,
        }
    }
}

impl Found {
    pub fn word(&self) -> String {
        match &self.set {
            Set::Layer(layer) => layer.word().to_owned(),
            Set::Detected => DETECTED.to_owned(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub spec: &'static SettingSpec,
    pub section: Section,
    pub value: String,
    pub source: String,
    pub winner: Option<Found>,
    pub shadowed: Vec<Found>,
    pub default: String,
    pub choices: Vec<String>,
    pub checks: Vec<super::edit::CheckItem>,
    pub applies: bool,
    pub path: SitePath,
    pub changed: Option<Changed>,
    pub current: Option<SettingValue>,
    pub new_chat: Option<SettingValue>,
    pub saved_here: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Changed {
    Now(String),
    Removed,
}

pub fn shown(spec: &SettingSpec, value: &SettingValue) -> String {
    if spec.name == names::AUTH {
        return match value.text().as_str() {
            wisp_protocol::settings::AUTH_API_KEY => {
                wisp_render::chrome::account::API_KEY.to_owned()
            }
            wisp_protocol::settings::AUTH_BEARER => "bearer".to_owned(),
            wisp_protocol::settings::AUTH_ANTHROPIC_SUBSCRIPTION
            | wisp_protocol::settings::AUTH_CHATGPT_SUBSCRIPTION => {
                wisp_protocol::SUBSCRIPTION_WORD.to_owned()
            }
            _ => value.shown(),
        };
    }
    match value {
        SettingValue::Text(text) if text == wisp_protocol::NOT_SHOWN => text.clone(),
        _ => spec.shown(value),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub value: SettingValue,
    pub source: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Display {
    rows: BTreeMap<&'static str, Resolved>,
    session: BTreeMap<&'static str, String>,
    sections: BTreeMap<String, Resolved>,
}

impl Display {
    pub fn from_caps(caps: &Caps, copy_on_select: &Choice<bool>) -> Display {
        let mut out = Display::default();
        let text = |word: Option<&str>| SettingValue::Text(word.unwrap_or_default().to_owned());
        out.set(
            names::DISPLAY_THEME,
            text(word_of(THEMES, &MODES, &Some(caps.mode.value))),
            &caps.mode.source,
        );
        out.set(
            names::DISPLAY_COLOR,
            text(word_of(COLORS, &DEPTHS, &Some(caps.depth.value))),
            &caps.depth.source,
        );
        out.set(
            names::DISPLAY_MOTION,
            text(word_of(MOTIONS, &MOTION_POLICIES, &Some(caps.motion.value))),
            &caps.motion.source,
        );
        out.set(
            names::DISPLAY_AMBIGUOUS,
            text(word_of(AMBIGUOUS, &GLYPH_SETS, &Some(caps.glyphs.value)).or(Some("ascii only"))),
            &caps.glyphs.source,
        );
        out.set(
            names::DISPLAY_MOUSE,
            SettingValue::Bool(caps.mouse.value),
            &caps.mouse.source,
        );
        out.set(
            names::DISPLAY_SCROLL_SPEED,
            SettingValue::Int(i64::from(caps.scroll_speed.value)),
            &caps.scroll_speed.source,
        );
        out.set(
            names::DISPLAY_FULL_REPAINT,
            SettingValue::Bool(caps.full_repaint.value),
            &caps.full_repaint.source,
        );
        out.set(
            names::DISPLAY_PICTURES,
            SettingValue::Bool(caps.picture_setting.value),
            &caps.picture_setting.source,
        );
        out.set(
            names::DISPLAY_COPY_ON_SELECT,
            SettingValue::Bool(copy_on_select.value),
            &copy_on_select.source,
        );
        out.set(
            names::DISPLAY_SIDEBAR,
            text(Some(caps.sidebar.value.word())),
            &caps.sidebar.source,
        );
        for (name, choice) in [
            (names::DISPLAY_SIDEBAR_FOLDED, &caps.sidebar_folded),
            (names::DISPLAY_SIDEBAR_PINNED, &caps.sidebar_pinned),
        ] {
            out.set(
                name,
                SettingValue::List(choice.value.clone()),
                &choice.source,
            );
        }
        out
    }

    pub fn set(&mut self, name: &str, value: SettingValue, source: &str) {
        if let Some(spec) = find(name) {
            self.rows.insert(
                spec.name,
                Resolved {
                    value,
                    source: source.to_owned(),
                },
            );
        }
    }

    pub fn get(&self, name: &str) -> Option<&Resolved> {
        self.rows.get(name)
    }

    pub fn set_section(&mut self, path: &str, on: bool, source: &str) {
        self.sections.insert(
            path.to_owned(),
            Resolved {
                value: SettingValue::Bool(on),
                source: source.to_owned(),
            },
        );
    }

    pub fn section(&self, path: &str) -> Option<&Resolved> {
        self.sections.get(path)
    }

    pub fn forced(&self) -> Vec<(&'static str, Resolved)> {
        self.rows
            .iter()
            .filter(|(_, row)| display_set(&row.source) == Set::Layer(Layer::Forced))
            .map(|(name, row)| (*name, row.clone()))
            .collect()
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.rows.keys().copied().collect()
    }

    pub fn set_session(&mut self, name: &str, value: &str) {
        if let Some(spec) = find(name) {
            self.session.insert(spec.name, value.to_owned());
        }
    }

    fn session_of(&self, name: &str) -> Option<&str> {
        self.session.get(name).map(String::as_str)
    }
}

pub fn forced_display(env: &Env, streams: Streams, flags: Flags) -> Vec<(&'static str, Resolved)> {
    let forced = |saved: &SavedDisplay| {
        let caps = Caps::resolve_saved(env, streams, flags, saved, None);
        let copy = caps
            .copy_on_select
            .clone()
            .unwrap_or_else(|| Choice::new(false, "default"));
        Display::from_caps(&caps, &copy).forced()
    };
    let bare = forced(&SavedDisplay::default());
    let mut pinned = SavedDisplay::default();
    for (name, _) in &bare {
        if let Some(value) = stand_in(name) {
            let reach = Reach {
                session: true,
                saved: false,
            };
            pinned.changed(name, Some(value), reach);
        }
    }
    let still: Vec<&str> = forced(&pinned).into_iter().map(|(name, _)| name).collect();
    bare.into_iter()
        .filter(|(name, _)| still.contains(name))
        .collect()
}

fn stand_in(name: &str) -> Option<&'static str> {
    match name {
        names::DISPLAY_THEME => Some("dark"),
        names::DISPLAY_COLOR => Some("16"),
        names::DISPLAY_MOTION => Some("calm"),
        names::DISPLAY_AMBIGUOUS => Some("wide"),
        _ => None,
    }
}

pub fn display_set(source: &str) -> Set {
    let forced = matches!(
        source,
        "NO_COLOR is set" | "TERM=dumb" | "stdout is not a terminal"
    ) || source.ends_with(" is not UTF-8");
    if forced {
        Set::Layer(Layer::Forced)
    } else if source == crate::term::caps::SESSION_SOURCE {
        Set::Layer(Layer::Session)
    } else if source == "--ascii" || source.starts_with("--set ") {
        Set::Layer(Layer::Flag)
    } else if source.starts_with("WISP_")
        || source.contains("FORCE_COLOR=")
        || source.contains("CLICOLOR_FORCE=")
        || source.starts_with("CLICOLOR=")
    {
        Set::Layer(Layer::Env)
    } else if source.starts_with(concat!(wisp_protocol::config_file!(), " ")) {
        Set::Layer(Layer::Config)
    } else if source.starts_with(DEFAULT) {
        Set::Layer(Layer::Default)
    } else {
        Set::Detected
    }
}

fn names(path: &SitePath, origin: &str) -> bool {
    path.to_string() == origin || path.shown() == origin
}

fn saved_by<'a>(snapshot: &'a SettingsSnapshot, origin: &str) -> Option<&'a SavedValue> {
    snapshot.saved.iter().find(|s| names(&s.path, origin))
}

fn saved_line(snapshot: &SettingsSnapshot, origin: &str) -> Option<u64> {
    saved_by(snapshot, origin).and_then(|s| s.line)
}

fn found(
    spec: &SettingSpec,
    layer: &Layer,
    value: &SettingValue,
    origin: &str,
    snapshot: &SettingsSnapshot,
) -> Found {
    Found {
        set: Set::Layer(layer.clone()),
        value: shown(spec, value),
        origin: origin.to_owned(),
        line: saved_line(snapshot, origin),
    }
}

fn unset(spec: &SettingSpec, snapshot: &SettingsSnapshot) -> (String, String) {
    match spec.default {
        Default::Rule(_) if spec.name == names::COMPACT_AT => {
            match Context::of(snapshot).point(None) {
                Some(point) => (thousands(point.tokens), RULE.to_owned()),
                None => (super::edit::NOT_SET.to_owned(), RULE.to_owned()),
            }
        }
        Default::Rule(rule) => (rule.to_owned(), RULE.to_owned()),
        Default::Const(text) if matches!(spec.kind, Kind::Tokens { .. }) => (
            spec.parse(text)
                .map_or_else(|_| text.to_owned(), |value| shown(spec, &value)),
            DEFAULT.to_owned(),
        ),
        Default::Detected(text) | Default::Const(text) => (text.to_owned(), DEFAULT.to_owned()),
        Default::None => ("none".to_owned(), DEFAULT.to_owned()),
    }
}

fn default_words(spec: &SettingSpec, snapshot: &SettingsSnapshot) -> String {
    if spec.name == names::COMPACT_AT && snapshot.context.window.is_none() {
        return String::new();
    }
    match spec.default {
        Default::Detected(_) => spec.default_words(),
        _ => unset(spec, snapshot).0,
    }
}

fn here(
    spec: &'static SettingSpec,
    resolved: &ResolvedSetting,
    snapshot: &SettingsSnapshot,
) -> SitePath {
    if layer_site(&resolved.layer).is_some()
        && let Some(saved) = saved_by(snapshot, &resolved.origin)
    {
        return saved.path.clone();
    }
    let context = &snapshot.context;
    let place = if spec.has_site(wisp_protocol::settings::Site::Global) {
        Place::Global
    } else if spec.has_site(wisp_protocol::settings::Site::Model) {
        Place::Model(context.model.clone())
    } else {
        Place::Endpoint(context.endpoint.clone())
    };
    SitePath::new(place, spec)
}

fn applies_here(place: &Place, snapshot: &SettingsSnapshot) -> bool {
    let context = &snapshot.context;
    match place {
        Place::Global => true,
        Place::Endpoint(e) => *e == context.endpoint,
        Place::Model(m) => *m == context.model,
        Place::EndpointModel { endpoint, model } => {
            *endpoint == context.endpoint && *model == context.model
        }
        Place::Agent(a) => context.agent.as_deref() == Some(a.as_str()),
        Place::PluginHook { .. } | Place::PluginSlot { .. } | Place::Plugin(_) => false,
    }
}

fn changed(
    spec: &'static SettingSpec,
    resolved: &ResolvedSetting,
    snapshot: &SettingsSnapshot,
) -> Option<Changed> {
    if !matches!(snapshot.file, FileState::Ok { .. } | FileState::Missing) {
        return None;
    }
    let mut loaded: Vec<(&str, &SettingValue)> = vec![];
    if layer_site(&resolved.layer).is_some()
        && let Some(value) = &resolved.value
    {
        loaded.push((resolved.origin.as_str(), value));
    }
    loaded.extend(
        resolved
            .shadowed
            .iter()
            .filter(|s| layer_site(&s.layer).is_some())
            .map(|s| (s.origin.as_str(), &s.value)),
    );
    let file: Vec<(&SitePath, &SettingValue)> = snapshot
        .saved
        .iter()
        .filter(|s| s.path.spec().name == spec.name && applies_here(&s.path.place, snapshot))
        .filter_map(|s| match &s.entry {
            SavedEntry::Value { value } => Some((&s.path, value)),
            SavedEntry::Secret { .. } | SavedEntry::Host { .. } | SavedEntry::Other(_) => None,
        })
        .collect();
    for (path, value) in &file {
        match loaded.iter().find(|(origin, _)| names(path, origin)) {
            Some((_, was)) if was == value => {}
            _ => return Some(Changed::Now(shown(spec, value))),
        }
    }
    let gone = loaded
        .iter()
        .any(|(origin, _)| !file.iter().any(|(path, _)| names(path, origin)));
    gone.then_some(Changed::Removed)
}

fn merged(row: &mut Row, display: &Display, snapshot: &SettingsSnapshot) {
    let Some(resolved) = display.get(row.spec.name) else {
        return;
    };
    let set = display_set(&resolved.source);
    let value = shown(row.spec, &resolved.value);
    if !matches!(&set, Set::Layer(layer) if chat_layer(layer)) {
        row.new_chat = Some(resolved.value.clone());
    }
    if let Some(old) = row.winner.take()
        && old.set != set
    {
        row.shadowed.insert(0, old);
    }
    row.shadowed.retain(|s| s.set != set);
    let session = Set::Layer(Layer::Session);
    if let Some(text) = display.session_of(row.spec.name)
        && set != session
    {
        row.shadowed.retain(|s| s.set != session);
        row.shadowed.insert(
            0,
            Found {
                set: session,
                value: text.to_owned(),
                origin: crate::term::caps::SESSION_SOURCE.to_owned(),
                line: None,
            },
        );
    }
    row.value = value.clone();
    row.current = Some(resolved.value.clone());
    if set == Set::Layer(Layer::Default) {
        row.source = DEFAULT.to_owned();
        return;
    }
    let origin = match set {
        Set::Layer(Layer::Config) => row.spec.name.to_owned(),
        _ => resolved.source.clone(),
    };
    let line = saved_line(snapshot, &origin);
    let winner = Found {
        set,
        value,
        origin,
        line,
    };
    row.source = winner.word();
    row.winner = Some(winner);
}

pub(super) fn saved_value(
    spec: &SettingSpec,
    path: &SitePath,
    snapshot: &SettingsSnapshot,
) -> Option<SettingValue> {
    snapshot
        .saved
        .iter()
        .filter(|s| s.path == *path && s.path.spec().name == spec.name)
        .find_map(|s| match &s.entry {
            SavedEntry::Value { value } => Some(value.clone()),
            SavedEntry::Secret { .. } | SavedEntry::Host { .. } | SavedEntry::Other(_) => None,
        })
}

fn saved_at(spec: &SettingSpec, path: &SitePath, snapshot: &SettingsSnapshot) -> Option<String> {
    saved_value(spec, path, snapshot).map(|value| shown(spec, &value))
}

fn row(
    section: Section,
    spec: &'static SettingSpec,
    resolved: &ResolvedSetting,
    snapshot: &SettingsSnapshot,
    display: &Display,
) -> Row {
    let winner = match (&resolved.value, &resolved.layer) {
        (Some(_), Layer::Default) | (None, _) => None,
        (Some(value), layer) => Some(found(spec, layer, value, &resolved.origin, snapshot)),
    };
    let shadowed = resolved
        .shadowed
        .iter()
        .filter(|s| s.layer != Layer::Default)
        .map(|s| found(spec, &s.layer, &s.value, &s.origin, snapshot))
        .collect();
    let (value, source) = match (&winner, &resolved.value) {
        (Some(f), _) => (f.value.clone(), f.word()),
        (None, Some(value)) => (shown(spec, value), DEFAULT.to_owned()),
        (None, None) => unset(spec, snapshot),
    };
    let choices = match (resolved.choices.is_empty(), spec.kind) {
        (false, _) => resolved.choices.clone(),
        (true, Kind::Bool) => vec!["on".to_owned(), "off".to_owned()],
        (true, _) => vec![],
    };
    let path = here(spec, resolved, snapshot);
    let checks = match (spec.kind, snapshot.plugins.is_empty()) {
        (Kind::ToolNames, false) => super::edit::tool_checks(snapshot),
        (Kind::SectionNames, false) => super::edit::section_checks(&snapshot.plugins),
        _ => choices
            .iter()
            .map(|c| super::edit::CheckItem::plain(c))
            .collect(),
    };
    let mut out = Row {
        spec,
        section,
        value,
        source,
        winner,
        shadowed,
        default: default_words(spec, snapshot),
        choices,
        checks,
        applies: resolved.applies,
        saved_here: saved_at(spec, &path, snapshot),
        path,
        changed: changed(spec, resolved, snapshot),
        current: resolved.value.clone(),
        new_chat: if chat_layer(&resolved.layer) {
            resolved
                .shadowed
                .iter()
                .find(|s| !chat_layer(&s.layer))
                .map(|s| s.value.clone())
        } else {
            resolved.value.clone()
        }
        .or_else(|| spec.default_value()),
    };
    merged(&mut out, display, snapshot);
    if spec.name == names::COMPACT_AT
        && let Some(value) = compact_value(
            resolved.value.as_ref().and_then(TokensOrPercent::of),
            Context::of(snapshot),
        )
    {
        out.value = value;
    }
    if spec.name == names::TOOLS {
        let marks = super::edit::marks_of(spec, &out.checks, resolved.value.as_ref());
        let checks: Vec<_> = out
            .checks
            .iter()
            .zip(marks)
            .filter(|(c, _)| !c.by_row && c.follows.is_none() && !c.kept)
            .collect();
        let on = checks.iter().filter(|(_, marked)| *marked).count();
        out.value = count(on, checks.len());
    }
    out
}

pub fn rows(snapshot: &SettingsSnapshot, display: &Display) -> Vec<Row> {
    scrubbed_rows(&super::scrub::scrubbed(snapshot), display)
}

pub(super) fn scrubbed_rows(snapshot: &SettingsSnapshot, display: &Display) -> Vec<Row> {
    placed()
        .filter_map(|(section, spec)| {
            let resolved = snapshot
                .settings
                .iter()
                .find(|r| r.name.spec().name == spec.name)?;
            Some(row(section, spec, resolved, snapshot, display))
        })
        .collect()
}
