use std::fmt;

use super::registry::{
    AGENTS_GROUP, AGENTS_NOTE, ENABLED_KEY, HOOKS_TABLE, INSTALLED_KEY, PLUGIN_ENABLED,
    PLUGIN_HOOK, PLUGIN_INSTALLED, PLUGIN_SLOT, PLUGINS_GROUP, PRESETS_TABLE, SETTINGS,
    SLOTS_TABLE, SettingSpec, Site, find, provider_plugin,
};
use crate::agents::{preset_table, shown_preset_name};
use crate::names::{CONFIG_FILE, check_model_id, model_or_placeholder, valid_endpoint_name};
use crate::variable::looks_like_key;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Place {
    Global,
    Endpoint(String),
    Model(String),
    EndpointModel { endpoint: String, model: String },
    Agent(String),
    PluginHook { plugin: String, hook: String },
    PluginSlot { plugin: String, section: String },
    Plugin(String),
}

impl Place {
    pub fn site(&self) -> Site {
        match self {
            Self::Global => Site::Global,
            Self::Endpoint(_) => Site::Endpoint,
            Self::Model(_) => Site::Model,
            Self::EndpointModel { .. } => Site::EndpointModel,
            Self::Agent(_) => Site::Agent,
            Self::PluginHook { .. } => Site::PluginHook,
            Self::PluginSlot { .. } => Site::PluginSlot,
            Self::Plugin(_) => Site::Plugin,
        }
    }

    pub fn section(&self) -> Option<String> {
        match self {
            Self::PluginSlot { plugin, section } => Some(format!("{plugin}/{section}")),
            _ => None,
        }
    }
}

#[derive(Clone, Copy)]
pub struct SpecRef(pub &'static SettingSpec);

impl fmt::Debug for SpecRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.name)
    }
}

impl PartialEq for SpecRef {
    fn eq(&self, other: &Self) -> bool {
        self.0.name == other.0.name
    }
}

impl Eq for SpecRef {}

impl PartialOrd for SpecRef {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SpecRef {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.name.cmp(other.0.name)
    }
}

impl std::hash::Hash for SpecRef {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.name.hash(state);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SitePath {
    pub place: Place,
    spec: SpecRef,
}

impl SitePath {
    pub fn new(place: Place, spec: &'static SettingSpec) -> Self {
        Self {
            place,
            spec: SpecRef(spec),
        }
    }

    pub fn global(spec: &'static SettingSpec) -> Self {
        Self::new(Place::Global, spec)
    }

    pub fn spec(&self) -> &'static SettingSpec {
        self.spec.0
    }

    fn leaf(&self) -> &'static str {
        match self.place {
            Place::Global => self.spec().file_key(),
            _ => self.spec().leaf(),
        }
    }

    pub fn segments(&self) -> Vec<String> {
        let leaf = self.leaf();
        let leaf_parts = leaf.split('.').map(str::to_owned);
        let mut out: Vec<String> = match &self.place {
            Place::Global => Vec::new(),
            Place::Endpoint(e) => vec!["endpoints".to_owned(), e.clone()],
            Place::Model(m) => vec!["models".to_owned(), m.clone()],
            Place::EndpointModel { endpoint, model } => vec![
                "endpoints".to_owned(),
                endpoint.clone(),
                "models".to_owned(),
                model.clone(),
            ],
            Place::Agent(a) => vec!["agents".to_owned(), a.clone()],
            Place::PluginHook { plugin, hook } => {
                return vec![
                    PLUGINS_GROUP.to_owned(),
                    plugin.clone(),
                    HOOKS_TABLE.to_owned(),
                    hook.clone(),
                ];
            }
            Place::PluginSlot { plugin, section } => {
                return vec![
                    PLUGINS_GROUP.to_owned(),
                    plugin.clone(),
                    SLOTS_TABLE.to_owned(),
                    section.clone(),
                ];
            }
            Place::Plugin(plugin) => {
                return vec![
                    PLUGINS_GROUP.to_owned(),
                    plugin.clone(),
                    self.plugin_key().to_owned(),
                ];
            }
        };
        out.extend(leaf_parts);
        out
    }

    pub fn plugin_key(&self) -> &'static str {
        self.spec().name.rsplit('.').next().unwrap_or_default()
    }

    pub fn plugin_switch(plugin: &str, key: &str) -> Option<Self> {
        let name = match key {
            ENABLED_KEY => PLUGIN_ENABLED,
            INSTALLED_KEY => PLUGIN_INSTALLED,
            _ => return None,
        };
        Some(Self::new(Place::Plugin(plugin.to_owned()), find(name)?))
    }

    pub fn provider_switch(&self) -> Option<(&str, &'static str)> {
        let Place::Plugin(id) = &self.place else {
            return None;
        };
        (provider_plugin(id) && [PLUGIN_ENABLED, PLUGIN_INSTALLED].contains(&self.spec().name))
            .then(|| (id.as_str(), self.plugin_key()))
    }

    pub fn plugin_hook(plugin: &str, hook: &str) -> Option<Self> {
        let place = Place::PluginHook {
            plugin: plugin.to_owned(),
            hook: hook.to_owned(),
        };
        Some(Self::new(place, find(PLUGIN_HOOK)?))
    }

    pub fn shown(&self) -> String {
        let leaf = self.spec().file_key();
        match &self.place {
            Place::Model(m) => format!("models.{}.{leaf}", quoted(model_or_placeholder(m))),
            Place::EndpointModel { endpoint, model } => format!(
                "endpoints.{endpoint}.models.{}.{leaf}",
                quoted(model_or_placeholder(model))
            ),
            _ => self.to_string(),
        }
    }
}

fn quoted(model: &str) -> String {
    format!("\"{model}\"")
}

impl fmt::Display for SitePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let leaf = self.leaf();
        match &self.place {
            Place::Global => f.write_str(leaf),
            Place::Endpoint(e) => write!(f, "endpoints.{e}.{leaf}"),
            Place::Model(m) => write!(f, "models.{}.{leaf}", quoted(m)),
            Place::EndpointModel { endpoint, model } => {
                write!(f, "endpoints.{endpoint}.models.{}.{leaf}", quoted(model))
            }
            Place::Agent(a) => write!(f, "agents.{a}.{leaf}"),
            Place::PluginHook { plugin, hook } => {
                write!(f, "{PLUGINS_GROUP}.{plugin}.{HOOKS_TABLE}.{hook}")
            }
            Place::PluginSlot { plugin, section } => {
                write!(f, "{PLUGINS_GROUP}.{plugin}.{SLOTS_TABLE}.{section}")
            }
            Place::Plugin(plugin) => {
                write!(f, "{PLUGINS_GROUP}.{plugin}.{}", self.plugin_key())
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Name(&'static SettingSpec),
    Path(SitePath),
}

impl Target {
    pub fn spec(&self) -> &'static SettingSpec {
        match self {
            Self::Name(spec) => spec,
            Self::Path(path) => path.spec(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetError {
    NotAName,
    Unknown {
        given: String,
        suggestion: Option<&'static str>,
    },
    Reserved {
        path: String,
    },
    Preset {
        name: Option<String>,
    },
    AgentType {
        path: String,
        name: Option<String>,
    },
    WrongSite {
        name: &'static str,
        site: Site,
    },
}

pub const NOT_A_NAME: &str = "that isn't a setting name; see wisp config list";

impl fmt::Display for TargetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAName => f.write_str(NOT_A_NAME),
            Self::Unknown {
                given,
                suggestion: Some(near),
            } => write!(f, "unknown setting {given}; did you mean {near}?"),
            Self::Unknown {
                given,
                suggestion: None,
            } => write!(f, "unknown setting {given}; see wisp config list"),
            Self::Reserved { path } => write!(f, "{path} is {AGENTS_NOTE}."),
            Self::Preset { name } => write!(
                f,
                "presets live only in {CONFIG_FILE}; edit {} there",
                preset_table(name.as_deref())
            ),
            Self::AgentType { path, name } => write!(
                f,
                "{path} is {AGENTS_NOTE}; a preset goes in {} in {CONFIG_FILE}",
                preset_table(name.as_deref())
            ),
            Self::WrongSite { name, site } => {
                let at = match site {
                    Site::Endpoint => "per account",
                    Site::Model => "per model",
                    Site::EndpointModel => "per account and model",
                    Site::Agent => "per agent",
                    Site::Global => "at the top level",
                    Site::Shipped => "in the shipped model file",
                    Site::PluginHook => "per plugin hook",
                    Site::PluginSlot => "per sidebar section",
                    Site::Plugin => "per plugin",
                };
                write!(f, "{name} can't be set {at}; see wisp config list")
            }
        }
    }
}

const SECRET_WORDS: [&str; 4] = ["key", "token", "secret", "password"];

pub fn holds_secret_word(name: &str) -> bool {
    secret_word(name).is_some()
}

pub fn secret_word(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    SECRET_WORDS
        .iter()
        .copied()
        .find(|word| lower.contains(word))
}

pub(crate) fn name_segment(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Segment {
    text: String,
    quoted: bool,
}

fn split(text: &str) -> Option<Vec<Segment>> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    loop {
        let segment = match chars.peek() {
            Some('"' | '\'') => {
                let close = chars.next()?;
                let mut inner = String::new();
                loop {
                    match chars.next()? {
                        c if c == close => break,
                        '\\' => return None,
                        c => inner.push(c),
                    }
                }
                Segment {
                    text: inner,
                    quoted: true,
                }
            }
            _ => {
                let mut bare = String::new();
                while let Some(c) = chars.peek().copied() {
                    if c == '.' {
                        break;
                    }
                    if !(c.is_ascii_alphanumeric() || c == '_' || c == '-') {
                        return None;
                    }
                    bare.push(c);
                    chars.next();
                }
                if bare.is_empty() {
                    return None;
                }
                Segment {
                    text: bare,
                    quoted: false,
                }
            }
        };
        out.push(segment);
        match chars.next() {
            None => return Some(out),
            Some('.') => {}
            Some(_) => return None,
        }
    }
}

fn kebab(text: &str, min: usize) -> bool {
    (min..=32).contains(&text.len())
        && text.starts_with(|c: char| c.is_ascii_lowercase())
        && text
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

pub fn plugin_id(text: &str) -> bool {
    kebab(text, 2)
}

pub fn hook_name(text: &str) -> bool {
    kebab(text, 1)
}

pub fn section_name(text: &str) -> bool {
    kebab(text, 2)
}

pub fn section_ref(text: &str) -> bool {
    text.split_once('/')
        .is_some_and(|(plugin, name)| plugin_id(plugin) && section_name(name))
}

const RENAMED_SECTIONS: [(&str, &str, &str); 1] = [("context", "usage", "window")];

pub fn current_slot<'a>(plugin: &str, section: &'a str) -> &'a str {
    RENAMED_SECTIONS
        .iter()
        .find(|(id, former, _)| *id == plugin && *former == section)
        .map_or(section, |(_, _, current)| current)
}

pub fn former_slots<'a>(
    plugin: &'a str,
    section: &'a str,
) -> impl Iterator<Item = &'static str> + 'a {
    RENAMED_SECTIONS
        .iter()
        .filter(move |(id, _, current)| *id == plugin && *current == section)
        .map(|(_, former, _)| *former)
}

fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            let above = row[j + 1];
            row[j + 1] = (prev + cost).min(above + 1).min(row[j] + 1);
            prev = above;
        }
    }
    row[b.len()]
}

pub fn nearest(given: &str) -> Option<&'static str> {
    super::all()
        .filter(|s| s.is_built() && s.secrecy != super::registry::Secrecy::Hidden)
        .map(|s| (distance(given, s.name), s.name))
        .filter(|(d, name)| *d <= 2.max(name.len() / 4))
        .min_by_key(|(d, _)| *d)
        .map(|(_, name)| name)
}

fn closest<'a, T: AsRef<str>>(given: &str, names: &'a [T]) -> Option<&'a T> {
    names
        .iter()
        .map(|name| (distance(given, name.as_ref()), name))
        .filter(|(d, name)| *d <= 2.max(name.as_ref().len() / 4))
        .min_by_key(|(d, _)| *d)
        .map(|(_, name)| name)
}

pub fn nearest_in(given: &str, names: &[&'static str]) -> Option<&'static str> {
    closest(given, names).copied()
}

pub fn nearest_name(given: &str, names: &[String]) -> Option<String> {
    closest(given, names).cloned()
}

pub fn quotable(name: &str) -> bool {
    !looks_like_key(name) && !holds_secret_word(name) && name_segment(&name.replace('.', "_"))
}

fn unknown(given: &str, checked: &str) -> TargetError {
    if !quotable(checked) {
        return TargetError::NotAName;
    }
    TargetError::Unknown {
        given: given.to_owned(),
        suggestion: nearest(given),
    }
}

fn leaf_spec(
    segments: &[Segment],
    site: Site,
    whole: &str,
    checked: &str,
) -> Result<&'static SettingSpec, TargetError> {
    if segments.iter().any(|s| s.quoted || !name_segment(&s.text)) {
        return Err(TargetError::NotAName);
    }
    let leaf = segments
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join(".");
    if let Some(spec) = SETTINGS
        .iter()
        .find(|s| s.is_built() && s.has_site(site) && s.leaf() == leaf && s.name != leaf)
    {
        return Ok(spec);
    }
    let spec = find(&leaf).ok_or_else(|| unknown(whole, checked))?;
    if !spec.has_site(site) {
        return Err(TargetError::WrongSite {
            name: spec.name,
            site,
        });
    }
    Ok(spec)
}

fn quoted_model(segment: &Segment) -> bool {
    segment.quoted && check_model_id(&segment.text).is_ok()
}

fn model_segment(segment: &Segment) -> Result<String, TargetError> {
    if (!segment.quoted && looks_like_key(&segment.text)) || check_model_id(&segment.text).is_err()
    {
        return Err(TargetError::NotAName);
    }
    Ok(segment.text.clone())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endpoints<'a> {
    Configured(&'a [&'a str]),
    Any,
}

impl Endpoints<'_> {
    fn named(self, segment: &Segment) -> bool {
        !segment.quoted
            && valid_endpoint_name(&segment.text)
            && match self {
                Self::Configured(names) => names.contains(&segment.text.as_str()),
                Self::Any => true,
            }
    }
}

const NO_ENDPOINTS: Endpoints<'static> = Endpoints::Configured(&[]);

fn endpoint_segment(segment: &Segment, endpoints: Endpoints<'_>) -> Result<String, TargetError> {
    if endpoints.named(segment) {
        return Ok(segment.text.clone());
    }
    if segment.quoted || looks_like_key(&segment.text) || !valid_endpoint_name(&segment.text) {
        return Err(TargetError::NotAName);
    }
    Ok(segment.text.clone())
}

fn names_endpoint(segments: &[Segment], at: usize, endpoints: Endpoints<'_>) -> bool {
    segments
        .first()
        .is_some_and(|first| !first.quoted && first.text == "endpoints")
        && endpoints.named(&segments[at])
}

fn checked_text(segments: &[Segment], whole: &str, endpoints: Endpoints<'_>) -> String {
    if !segments.get(1).is_some_and(|s| endpoints.named(s)) {
        return whole.to_owned();
    }
    segments
        .iter()
        .enumerate()
        .map(|(at, s)| match (at, s.quoted) {
            (1, _) => "endpoint".to_owned(),
            (_, true) => format!("\"{}\"", s.text),
            (_, false) => s.text.clone(),
        })
        .collect::<Vec<_>>()
        .join(".")
}

pub fn parse_target(text: &str) -> Result<Target, TargetError> {
    parse_target_among(text, NO_ENDPOINTS)
}

pub fn parse_target_among(text: &str, endpoints: Endpoints<'_>) -> Result<Target, TargetError> {
    let text = text.trim();
    let segments = split(text).ok_or(TargetError::NotAName)?;
    if let Some(spec) = find(text).filter(|spec| spec.is_declared()) {
        return Ok(Target::Name(spec));
    }
    if segments.iter().enumerate().any(|(at, s)| {
        !quoted_model(s) && !names_endpoint(&segments, at, endpoints) && looks_like_key(&s.text)
    }) {
        return Err(TargetError::NotAName);
    }
    let first = segments.first().ok_or(TargetError::NotAName)?;
    if !first.quoted {
        match (first.text.as_str(), segments.len()) {
            (AGENTS_GROUP, 2..) if !segments[1].quoted && segments[1].text == PRESETS_TABLE => {
                if segments
                    .iter()
                    .any(|s| s.quoted || !name_segment(&s.text.replace('-', "_")))
                {
                    return Err(TargetError::NotAName);
                }
                let name = segments
                    .get(2)
                    .and_then(|s| shown_preset_name(&s.text))
                    .map(str::to_owned);
                return Err(TargetError::Preset { name });
            }
            (AGENTS_GROUP, 3..)
                if !segments[1].quoted
                    && find(&format!("{AGENTS_GROUP}.{}", segments[1].text)).is_some() =>
            {
                return Err(unknown(text, text));
            }
            (AGENTS_GROUP, 3..) => {
                endpoint_segment(&segments[1], NO_ENDPOINTS)?;
                if segments[2..]
                    .iter()
                    .any(|s| s.quoted || !name_segment(&s.text))
                {
                    return Err(TargetError::NotAName);
                }
                return Err(TargetError::AgentType {
                    path: text.to_owned(),
                    name: shown_preset_name(&segments[1].text).map(str::to_owned),
                });
            }
            ("endpoints", 5..) if segments[2].text == "models" && !segments[2].quoted => {
                let endpoint = endpoint_segment(&segments[1], endpoints)?;
                let model = model_segment(&segments[3])?;
                let checked = checked_text(&segments, text, endpoints);
                let spec = leaf_spec(&segments[4..], Site::EndpointModel, text, &checked)?;
                return Ok(Target::Path(SitePath::new(
                    Place::EndpointModel { endpoint, model },
                    spec,
                )));
            }
            ("endpoints", 3..) => {
                let endpoint = endpoint_segment(&segments[1], endpoints)?;
                let checked = checked_text(&segments, text, endpoints);
                let spec = leaf_spec(&segments[2..], Site::Endpoint, text, &checked)?;
                return Ok(Target::Path(SitePath::new(Place::Endpoint(endpoint), spec)));
            }
            (PLUGINS_GROUP, 4) if !segments[2].quoted && segments[2].text == HOOKS_TABLE => {
                let (plugin, hook) = (&segments[1], &segments[3]);
                if plugin.quoted
                    || hook.quoted
                    || !plugin_id(&plugin.text)
                    || !hook_name(&hook.text)
                {
                    return Err(TargetError::NotAName);
                }
                let spec = find(PLUGIN_HOOK).ok_or(TargetError::NotAName)?;
                return Ok(Target::Path(SitePath::new(
                    Place::PluginHook {
                        plugin: plugin.text.clone(),
                        hook: hook.text.clone(),
                    },
                    spec,
                )));
            }
            (PLUGINS_GROUP, 4) if !segments[2].quoted && segments[2].text == SLOTS_TABLE => {
                let (plugin, section) = (&segments[1], &segments[3]);
                if plugin.quoted
                    || section.quoted
                    || !plugin_id(&plugin.text)
                    || !section_name(&section.text)
                {
                    return Err(TargetError::NotAName);
                }
                let spec = find(PLUGIN_SLOT).ok_or(TargetError::NotAName)?;
                return Ok(Target::Path(SitePath::new(
                    Place::PluginSlot {
                        plugin: plugin.text.clone(),
                        section: current_slot(&plugin.text, &section.text).to_owned(),
                    },
                    spec,
                )));
            }
            (PLUGINS_GROUP, 3)
                if !segments[2].quoted
                    && matches!(segments[2].text.as_str(), ENABLED_KEY | INSTALLED_KEY) =>
            {
                let plugin = &segments[1];
                if plugin.quoted || !plugin_id(&plugin.text) {
                    return Err(TargetError::NotAName);
                }
                let path = SitePath::plugin_switch(&plugin.text, &segments[2].text)
                    .ok_or(TargetError::NotAName)?;
                return Ok(Target::Path(path));
            }
            (PLUGINS_GROUP, 3) => {
                if segments.iter().any(|segment| segment.quoted) {
                    return Err(TargetError::NotAName);
                }
                let name = super::row_name(&segments[1].text, &segments[2].text)
                    .map_err(|_| TargetError::NotAName)?;
                return Err(unknown(&name, &segments[2].text));
            }
            ("models", 3..) => {
                let model = model_segment(&segments[1])?;
                let spec = leaf_spec(&segments[2..], Site::Model, text, text)?;
                return Ok(Target::Path(SitePath::new(Place::Model(model), spec)));
            }
            _ => {}
        }
    }
    if segments.iter().any(|s| s.quoted || !name_segment(&s.text)) {
        return Err(TargetError::NotAName);
    }
    let name = segments
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join(".");
    if holds_secret_word(&name) && find(&name).is_none() {
        return Err(TargetError::NotAName);
    }
    match find(&name) {
        Some(spec) => match spec.status {
            super::registry::Status::Reserved { .. } => Err(TargetError::Reserved { path: name }),
            super::registry::Status::Built => Ok(Target::Name(spec)),
        },
        None => Err(unknown(&name, &name)),
    }
}
