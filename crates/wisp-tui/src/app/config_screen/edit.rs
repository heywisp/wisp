use wisp_protocol::context::{CompactPoint, PointSource, compact_at, window_for};
use wisp_protocol::settings::{
    Dynamic, Kind, Layer, Place, SettingSpec, SettingValue, SettingsSnapshot, Site, SitePath,
    TokensOrPercent, WindowRoom, find, names, share_point, thousands, usd_shown,
};
use wisp_protocol::{CONFIG_FILE, looks_like_key};
use wisp_render::chrome::config_screen::Check;
use wisp_render::chrome::{Told, Wins};

use super::detail::{chat_only, reset_value, winner_words};
use super::entries::{CHILD_AGENTS_ONLY, SEARCH_LEAD, TURNED_OFF_NOTE};
use super::rows::{Found, Row, Set, saved_value};
use wisp_render::text::sanitize;
use wisp_render::transcript::copy::{LAYER_ENDPOINT, LAYER_ENDPOINT_MODEL, LAYER_MODEL};

pub const THE_DEFAULT: &str = "the default";
pub const ALL_MODELS: &str = "all models";
pub const NOT_SET: &str = "not set";
pub const PRICING_PER_ENDPOINT: &str =
    "Pricing is set per named account; add one with wisp account add.";

pub fn key_shaped(text: &str) -> bool {
    looks_like_key(&text.replace('_', "-"))
}

pub(super) const HIDDEN_MODEL_LOCK: &str = concat!(
    "This model's ID looks like a key, so its settings can't be saved here; use wisp config set with the real ID, or edit ",
    wisp_protocol::config_file!(),
    "."
);

pub fn locked(spec: &SettingSpec, keys_file: bool) -> Option<&'static str> {
    (spec.name == names::PRICING && keys_file).then_some(PRICING_PER_ENDPOINT)
}

pub(super) fn names_hidden_model(place: &Place) -> bool {
    match place {
        Place::Model(model) | Place::EndpointModel { model, .. } => {
            wisp_protocol::model_hidden(model)
        }
        Place::Global
        | Place::Endpoint(_)
        | Place::Agent(_)
        | Place::PluginHook { .. }
        | Place::PluginSlot { .. }
        | Place::Plugin(_) => false,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    Model,
    Effort,
    Endpoint,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Widget {
    Toggle,
    Choice,
    Text,
    Checklist,
    Picker(Pick),
}

pub fn widget(spec: &SettingSpec) -> Option<Widget> {
    match (spec.dynamic, spec.kind) {
        (Some(Dynamic::Models), _) => Some(Widget::Picker(Pick::Model)),
        (Some(Dynamic::Efforts), _) => Some(Widget::Picker(Pick::Effort)),
        (Some(Dynamic::Endpoints), _) => Some(Widget::Picker(Pick::Endpoint)),
        (_, Kind::List(_) | Kind::ToolNames | Kind::SectionNames) => Some(Widget::Checklist),
        (Some(_), _) => None,
        (None, Kind::Bool) => Some(Widget::Toggle),
        (None, Kind::Preload) => Some(Widget::Choice),
        (None, Kind::Choice(list)) if !list.is_empty() => Some(Widget::Choice),
        (
            None,
            Kind::Int { .. }
            | Kind::Tokens { .. }
            | Kind::TokensOrPercent
            | Kind::Usd { .. }
            | Kind::VarName
            | Kind::PluginId
            | Kind::Language
            | Kind::Names
            | Kind::Path
            | Kind::ClientVersion,
        ) => Some(Widget::Text),
        _ => None,
    }
}

pub fn toggled(current: Option<&SettingValue>) -> SettingValue {
    match current {
        Some(SettingValue::Bool(on)) => SettingValue::Bool(!on),
        _ => SettingValue::Bool(true),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SiteChoice {
    pub path: SitePath,
    pub label: String,
    pub saved: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Field {
    Choice {
        at: usize,
    },
    Text {
        text: String,
        cursor: usize,
    },
    Checklist {
        items: Vec<CheckItem>,
        marks: Vec<bool>,
        at: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckItem {
    pub name: String,
    pub follows: Option<String>,
    pub note: Option<String>,
    pub default_on: bool,
    pub kept: bool,
    pub locked: Option<String>,
    pub by_row: bool,
    pub turned_off: bool,
}

impl CheckItem {
    pub fn plain(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            follows: None,
            note: None,
            default_on: true,
            kept: false,
            locked: None,
            by_row: false,
            turned_off: false,
        }
    }

    pub fn kept(name: &str, follows: Option<String>) -> Self {
        Self {
            name: name.to_owned(),
            follows,
            note: None,
            default_on: false,
            kept: true,
            locked: None,
            by_row: false,
            turned_off: false,
        }
    }

    pub fn checkable(&self) -> bool {
        self.follows.is_none() && !self.kept && self.locked.is_none()
    }
}

pub(super) fn section_checks(catalog: &[wisp_protocol::PluginEntry]) -> Vec<CheckItem> {
    let mut sections: Vec<&wisp_protocol::slot::SlotEntry> = catalog
        .iter()
        .flat_map(|p| &p.slots)
        .filter(|s| {
            s.slot == wisp_protocol::slot::SlotPlace::Sidebar
                && wisp_protocol::settings::section_ref(&s.name)
        })
        .collect();
    sections.sort_by(|a, b| (a.order, &a.name).cmp(&(b.order, &b.name)));
    sections
        .into_iter()
        .map(|s| CheckItem::plain(&s.name))
        .collect()
}

pub(super) struct Refusal {
    pub(super) short: String,
    pub(super) full: String,
}

pub(super) fn plugin_refusal(snapshot: &SettingsSnapshot, tool: &str) -> Option<Refusal> {
    let plugin = snapshot
        .plugins
        .iter()
        .find(|p| p.tools.iter().any(|t| t.name == tool))?;
    let id = sanitize(&plugin.id);
    let tool = sanitize(tool);
    let (word, hint) = if !snapshot.plugin_on(&plugin.id, wisp_protocol::settings::INSTALLED_KEY) {
        ("removed", "add it back in Plugins")
    } else if !snapshot.plugin_on(&plugin.id, wisp_protocol::settings::ENABLED_KEY) {
        ("off", "turn it on in Plugins")
    } else {
        return None;
    };
    Some(Refusal {
        short: format!("{id} is {word}"),
        full: format!("{tool} is a tool of {id}, which is {word}; {hint}"),
    })
}

pub(super) fn tool_checks(snapshot: &SettingsSnapshot) -> Vec<CheckItem> {
    let catalog = wisp_protocol::Catalog {
        plugins: snapshot.plugins.clone(),
    };
    let off = snapshot
        .settings
        .iter()
        .find(|s| s.name.as_str() == names::TURNED_OFF)
        .and_then(|s| s.value.as_ref());
    let label = find(names::TURNED_OFF).map_or(names::TURNED_OFF, |spec| spec.label);
    let item = |tool: &wisp_protocol::plugin::ToolEntry| {
        let by_row = tool.is_search();
        let turned_off =
            !by_row && matches!(off, Some(SettingValue::List(names)) if names.contains(&tool.name));
        let refused = plugin_refusal(snapshot, &tool.name).filter(|_| !by_row);
        CheckItem {
            name: tool.name.clone(),
            follows: tool.follows.clone(),
            note: if tool.follows.is_some() && tool.name == wisp_protocol::agents::REPORT {
                Some(CHILD_AGENTS_ONLY.to_owned())
            } else if by_row {
                None
            } else if turned_off {
                Some(TURNED_OFF_NOTE.to_owned())
            } else {
                refused
                    .as_ref()
                    .map(|r| r.short.clone())
                    .or_else(|| tool.unavailable.clone())
            },
            default_on: tool.default_on,
            kept: false,
            locked: if by_row {
                Some(format!("Switched by the {SEARCH_LEAD} row."))
            } else if turned_off {
                Some(format!(
                    "{} is in {label}; take it out there to turn it on",
                    sanitize(&tool.name)
                ))
            } else {
                refused.map(|r| r.full)
            },
            by_row,
            turned_off,
        }
    };
    catalog
        .leaders()
        .into_iter()
        .flat_map(|leader| {
            std::iter::once(item(leader))
                .chain(catalog.followers(&leader.name).into_iter().map(item))
        })
        .collect()
}

pub(super) fn mark_of(items: &[CheckItem], marks: &[bool], at: usize) -> bool {
    let Some(item) = items.get(at) else {
        return false;
    };
    if item.turned_off {
        return false;
    }
    match &item.follows {
        None => marks.get(at).copied().unwrap_or(false),
        Some(leader) => items
            .iter()
            .position(|i| i.name == *leader)
            .is_some_and(|lead| mark_of(items, marks, lead)),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub name: &'static str,
    pub field: Field,
    pub sites: Vec<SiteChoice>,
    pub site: usize,
}

fn byte_at(text: &str, chars: usize) -> usize {
    text.char_indices()
        .nth(chars)
        .map_or(text.len(), |(i, _)| i)
}

pub(super) fn marks_of(
    spec: &SettingSpec,
    items: &[CheckItem],
    current: Option<&SettingValue>,
) -> Vec<bool> {
    match current {
        Some(SettingValue::List(names)) => items
            .iter()
            .map(|i| (i.checkable() || i.locked.is_some()) && names.contains(&i.name))
            .collect(),
        _ => items
            .iter()
            .map(|i| {
                spec.dynamic == Some(Dynamic::Tools)
                    && (i.checkable() || i.locked.is_some())
                    && i.follows.is_none()
                    && i.default_on
            })
            .collect(),
    }
}

impl Edit {
    pub fn open(
        row: &Row,
        current: Option<&SettingValue>,
        saved: Option<&SettingValue>,
    ) -> Option<Edit> {
        let field = match widget(row.spec)? {
            Widget::Toggle | Widget::Picker(_) => return None,
            Widget::Choice => {
                let now = current.map(SettingValue::text);
                let at = row
                    .spec
                    .kind
                    .choices()
                    .iter()
                    .position(|c| now.as_deref() == Some(*c))
                    .unwrap_or(0);
                Field::Choice { at }
            }
            Widget::Text => {
                let text = current
                    .map(SettingValue::text)
                    .filter(|t| !key_shaped(t))
                    .unwrap_or_default();
                let cursor = text.chars().count();
                Field::Text { text, cursor }
            }
            Widget::Checklist => {
                if row.checks.is_empty() {
                    return None;
                }
                let mut items = row.checks.clone();
                if let (Kind::SectionNames, Some(SettingValue::List(names))) =
                    (row.spec.kind, saved)
                {
                    let lacking: Vec<CheckItem> = names
                        .iter()
                        .filter(|n| !items.iter().any(|i| i.name == **n))
                        .map(|n| CheckItem::kept(n, None))
                        .collect();
                    items.extend(lacking);
                }
                if let (Kind::ToolNames, Some(SettingValue::List(names))) = (row.spec.kind, saved) {
                    let lacking: Vec<CheckItem> = names
                        .iter()
                        .filter(|n| {
                            !items
                                .iter()
                                .any(|i| i.name == **n && (i.checkable() || i.by_row))
                        })
                        .filter(|n| {
                            !items.iter().any(|i| {
                                i.name == **n
                                    && i.follows.as_ref().is_some_and(|l| names.contains(l))
                            })
                        })
                        .map(|n| {
                            let follows = items
                                .iter()
                                .find(|i| i.name == *n)
                                .and_then(|i| i.follows.clone());
                            CheckItem::kept(n, follows)
                        })
                        .collect();
                    items.extend(lacking);
                }
                let marks = marks_of(row.spec, &items, current);
                Field::Checklist {
                    items,
                    marks,
                    at: 0,
                }
            }
        };
        Some(Edit {
            name: row.spec.name,
            field,
            sites: Vec::new(),
            site: 0,
        })
    }

    pub fn for_sites(mut self, sites: Vec<SiteChoice>) -> Self {
        self.site = sites.iter().rposition(|s| s.saved).unwrap_or(0);
        self.sites = sites;
        self
    }

    pub fn next_site(&mut self, by: isize) {
        let count = self.sites.len();
        if count > 0 {
            self.site = self.site.wrapping_add_signed(by).wrapping_add(count) % count;
        }
    }

    pub fn keeps(&self) -> bool {
        match &self.field {
            Field::Choice { .. } | Field::Checklist { .. } => true,
            Field::Text { text, .. } => !key_shaped(text.trim()),
        }
    }

    pub fn is_empty(&self) -> bool {
        match &self.field {
            Field::Choice { .. } | Field::Checklist { .. } => true,
            Field::Text { text, .. } => text.is_empty(),
        }
    }

    pub fn is_text(&self) -> bool {
        matches!(self.field, Field::Text { .. })
    }

    pub fn is_checklist(&self) -> bool {
        matches!(self.field, Field::Checklist { .. })
    }

    pub fn toggles(&self) -> bool {
        matches!(&self.field, Field::Checklist { items, at, .. } if items.get(*at).is_some_and(CheckItem::checkable))
    }

    pub fn site(&self) -> Option<&SiteChoice> {
        self.sites.get(self.site)
    }

    pub fn shift(&mut self, by: isize, spec: &SettingSpec) {
        let (at, count) = match &mut self.field {
            Field::Choice { at } => (at, spec.kind.choices().len()),
            Field::Checklist { items, at, .. } => (at, items.len()),
            Field::Text { .. } => return,
        };
        *at = at.saturating_add_signed(by).min(count.saturating_sub(1));
    }

    pub fn toggle(&mut self) {
        if let Field::Checklist { items, marks, at } = &mut self.field
            && items.get(*at).is_some_and(CheckItem::checkable)
            && let Some(mark) = marks.get_mut(*at)
        {
            *mark = !*mark;
        }
    }

    pub fn insert(&mut self, c: char) {
        if c.is_control() {
            return;
        }
        if let Field::Text { text, cursor } = &mut self.field {
            text.insert(byte_at(text, *cursor), c);
            *cursor += 1;
        }
    }

    pub fn paste(&mut self, pasted: &str) {
        for c in pasted.chars() {
            self.insert(c);
        }
    }

    pub fn backspace(&mut self) {
        if let Field::Text { text, cursor } = &mut self.field
            && *cursor > 0
        {
            *cursor -= 1;
            text.remove(byte_at(text, *cursor));
        }
    }

    pub fn delete(&mut self) {
        if let Field::Text { text, cursor } = &mut self.field
            && *cursor < text.chars().count()
        {
            text.remove(byte_at(text, *cursor));
        }
    }

    pub fn left(&mut self) {
        if let Field::Text { cursor, .. } = &mut self.field {
            *cursor = cursor.saturating_sub(1);
        }
    }

    pub fn right(&mut self) {
        if let Field::Text { text, cursor } = &mut self.field {
            *cursor = (*cursor + 1).min(text.chars().count());
        }
    }

    pub fn home(&mut self) {
        if let Field::Text { cursor, .. } = &mut self.field {
            *cursor = 0;
        }
    }

    pub fn end(&mut self) {
        if let Field::Text { text, cursor } = &mut self.field {
            *cursor = text.chars().count();
        }
    }

    pub fn clear(&mut self) {
        if let Field::Text { text, cursor } = &mut self.field {
            text.clear();
            *cursor = 0;
        }
    }

    pub fn value(&self, spec: &SettingSpec) -> Option<SettingValue> {
        match &self.field {
            Field::Choice { at } => spec
                .kind
                .choices()
                .get(*at)
                .and_then(|c| spec.parse(c).ok()),
            Field::Text { text, .. } => spec.parse(text).ok(),
            Field::Checklist { items, marks, .. } => {
                let names: Vec<String> = items
                    .iter()
                    .zip(marks)
                    .filter(|(item, on)| {
                        item.kept || (**on && (item.checkable() || item.by_row || item.turned_off))
                    })
                    .map(|(item, _)| item.name.clone())
                    .collect();
                spec.parse_list(&names).ok()
            }
        }
    }
}

fn site_words(site: Site, model: &str, snapshot: &SettingsSnapshot) -> Option<(Place, String)> {
    let context = &snapshot.context;
    match site {
        Site::Endpoint | Site::EndpointModel
            if super::entries::keys_file(snapshot, &context.endpoint) =>
        {
            None
        }
        Site::Model | Site::EndpointModel if wisp_protocol::model_hidden(model) => None,
        Site::Global => Some((Place::Global, ALL_MODELS.to_owned())),
        Site::Endpoint => None,
        Site::Model => Some((Place::Model(model.to_owned()), LAYER_MODEL.to_owned())),
        Site::EndpointModel => Some((
            Place::EndpointModel {
                endpoint: context.endpoint.clone(),
                model: model.to_owned(),
            },
            LAYER_ENDPOINT_MODEL.to_owned(),
        )),
        Site::Agent | Site::Shipped | Site::PluginHook | Site::PluginSlot | Site::Plugin => None,
    }
}

pub fn sites(row: &Row, snapshot: &SettingsSnapshot) -> Vec<SiteChoice> {
    let model = match &row.path.place {
        Place::Model(id) | Place::EndpointModel { model: id, .. } => id,
        _ => &snapshot.context.model,
    };
    row.spec
        .sites
        .iter()
        .filter_map(|site| site_words(*site, model, snapshot))
        .map(|(place, words)| {
            let path = SitePath::new(place, row.spec);
            SiteChoice {
                saved: saved_value(row.spec, &path, snapshot).is_some(),
                label: words,
                path,
            }
        })
        .filter(|site| site.path.place.site() != Site::EndpointModel || site.saved)
        .collect()
}

fn shown_value(spec: &SettingSpec, value: &SettingValue) -> String {
    super::rows::shown(spec, value)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Context {
    pub window: Option<u64>,
    pub room: Option<WindowRoom>,
    pub max_tokens: u32,
}

impl Context {
    pub fn of(snapshot: &SettingsSnapshot) -> Context {
        Context {
            window: snapshot.context.window,
            room: snapshot.context.room,
            max_tokens: snapshot.context.max_tokens,
        }
    }

    pub(super) fn base(self) -> Option<u64> {
        self.room.map(|room| room.base).or(self.window)
    }

    fn window_for(self, value: Option<TokensOrPercent>) -> Option<u64> {
        match self.room {
            Some(room) => Some(window_for(
                room.base,
                Some(room.max),
                self.max_tokens,
                value,
            )),
            None => self.window,
        }
    }

    pub(super) fn point(self, value: Option<TokensOrPercent>) -> Option<CompactPoint> {
        Some(compact_at(
            self.window_for(value)?,
            self.max_tokens,
            value.map(|value| (value, PointSource::Config)),
        ))
    }
}

fn point_of(value: &SettingValue, context: Context) -> Option<u64> {
    match value {
        SettingValue::Tokens(n) => Some(*n),
        SettingValue::Percent(p) => context.base().map(|w| share_point(w, *p)),
        _ => None,
    }
}

pub(super) fn compact_value(value: Option<TokensOrPercent>, context: Context) -> Option<String> {
    let point = context.point(value)?;
    Some(format!(
        "{} of {}",
        thousands(point.tokens),
        thousands(context.window_for(value)?)
    ))
}

pub fn preview(spec: &SettingSpec, value: &SettingValue, context: Context) -> String {
    if spec.name == names::COMPACT_AT
        && let Some(given) = TokensOrPercent::of(value)
        && let Some(value) = compact_value(Some(given), context)
    {
        return value;
    }
    match (value, point_of(value, context)) {
        (_, Some(n)) => format!("= {} tokens", thousands(n)),
        (SettingValue::Percent(p), None) => format!("= {p} of the window"),
        (SettingValue::Usd { micros }, None) => format!("= {}", usd_shown(*micros)),
        (other, None) => format!("= {}", other.shown()),
    }
}

pub fn check(spec: &SettingSpec, text: &str, context: Context) -> Check {
    if text.trim().is_empty() {
        return Check::Empty;
    }
    match spec.parse(text) {
        Ok(value) => Check::Preview(preview(spec, &value, context)),
        Err(error) => Check::Invalid(error.rule),
    }
}

pub(super) fn layer_site(layer: &Layer) -> Option<&'static str> {
    match layer {
        Layer::Config => Some(ALL_MODELS),
        Layer::Model => Some(LAYER_MODEL),
        Layer::EndpointModel => Some(LAYER_ENDPOINT_MODEL),
        Layer::Endpoint => Some(LAYER_ENDPOINT),
        _ => None,
    }
}

fn own(found: &Found, row: &Row) -> bool {
    match &found.set {
        Set::Layer(Layer::Session) => true,
        set if set.site().is_some() => found.origin == row.path.to_string(),
        _ => false,
    }
}

pub fn can_reset(row: &Row) -> bool {
    if chat_only(row) {
        return reset_value(row).is_some();
    }
    row.saved_here.is_some()
        || row
            .winner
            .iter()
            .chain(&row.shadowed)
            .any(|f| f.set == Set::Layer(Layer::Session))
}

pub fn after_reset(row: &Row) -> Option<&Found> {
    row.winner
        .iter()
        .chain(&row.shadowed)
        .find(|f| !own(f, row))
}

pub fn found_words(found: &Found) -> String {
    let value = &found.value;
    let origin = &found.origin;
    match &found.set {
        Set::Layer(Layer::Flag) => format!("{origin} {value} on the command line"),
        Set::Layer(Layer::Env) => format!("{origin}={value} in your environment"),
        set if set.site().is_some() => format!("{origin} = {value} in {CONFIG_FILE}"),
        Set::Layer(Layer::Shipped) => format!("Wisp's shipped value {value}"),
        other => format!("{} {value}", winner_words(other)),
    }
}

pub(super) fn held(
    row: &Row,
    place: &SitePath,
    snapshot: &SettingsSnapshot,
) -> Option<SettingValue> {
    saved_value(row.spec, place, snapshot)
}

pub fn told_at(row: &Row, place: &SitePath, snapshot: &SettingsSnapshot) -> Told {
    let shown = held(row, place, snapshot).map(|v| shown_value(row.spec, &v));
    Told {
        was: shown.clone().unwrap_or_else(|| NOT_SET.to_owned()),
        saved: shown,
        wins: None,
    }
}

pub fn told(row: &Row, reset: bool) -> Told {
    let wins = reset.then(|| {
        let by = after_reset(row).map_or_else(|| THE_DEFAULT.to_owned(), found_words);
        if row.spec.mid_session {
            Wins::ThisSession(by)
        } else {
            Wins::NewSessions(by)
        }
    });
    Told {
        was: unnested(&row.value),
        saved: row.saved_here.clone(),
        wins,
    }
}

pub fn unnested(value: &str) -> String {
    match value
        .strip_suffix(')')
        .and_then(|inner| inner.rsplit_once(" ("))
    {
        Some((value, note)) => format!("{value}, {note}"),
        None => value.to_owned(),
    }
}
