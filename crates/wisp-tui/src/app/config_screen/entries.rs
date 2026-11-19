use std::collections::BTreeSet;

use wisp_protocol::endpoint::{EndpointEdit, StoreChoice, UrlEdit, VariableEdit};
use wisp_protocol::plugin::{HookDisabled, HookEntry, SectionPlace, ToolEntry};
use wisp_protocol::settings::{
    Applies, Default, EndpointSummary, Kind, Layer, ModelEntry, Place, PlacedValue, SETTINGS,
    SavedEntry, Secrecy, Section, SettingSpec, SettingValue, SettingsSnapshot, Site, SitePath,
    Status, names, thousands,
};
use wisp_protocol::{NOT_A_MODEL_ID, NOT_SHOWN, NoSearch, PluginEntry, VariableName, model_hidden};
use wisp_render::chrome::account::ACCOUNT_TITLE;
use wisp_render::chrome::{Credential, KeyHint, endpoint_card, model_line, short_model};
use wisp_render::text::sanitize;
use wisp_render::transcript::copy::{ENTER_KEY, NEXT_TURN};

use super::detail::{CHOOSE_KEY, Keys, hint};
use super::edit::NOT_SET;
use super::rows::{DEFAULT, Found, OFF_SCREEN, Resolved, Row, Set, saved_value, shown};
use super::{CANCEL_KEY, SEP, pending};
use crate::app::endpoint_card::endpoint_facts_of;
use crate::app::sidebar::switch_path;
use wisp_protocol::slot::{SlotEntry, SlotPlace};

pub const PLUGIN_LEAD: &str = "Plugin";
pub(super) const SECTION_LEAD: &str = "Sidebar section";
pub(super) const SECTION_CYCLE: &str = "off (in a cycle)";
pub(super) const SWITCH_IN_TOOLS: &str = "switch in Tools";
pub const SEARCH_LEAD: &str = "Web search";
pub(super) const SEARCH_HELP: &str = "Lets the model look things up on the web.";
pub use wisp_protocol::settings::NEW_CHATS;
pub(super) const CHILD_AGENTS_ONLY: &str = "child agents only";
pub(super) const TURNED_OFF_NOTE: &str = "turned off";
pub(super) const PLUGIN_OFF: &str = "off (plugin off)";
pub(super) const PLUGIN_REMOVED: &str = "removed";
pub(super) const PLUGIN_KEY_ON: (&str, &str) = (ENTER_KEY, "turn on");
pub(super) const PLUGIN_KEY_OFF: (&str, &str) = (ENTER_KEY, "turn off");
pub(super) const PLUGIN_KEY_ADD: (&str, &str) = (ENTER_KEY, "add back");
pub(super) const SETTING_LEAD: &str = "setting:";
pub(super) const HOOK_LEAD: &str = "Hook";
pub(super) const HOOK_LEFT_OUT: &str = "off (left out at start)";
pub(super) const HOOK_KEY_SETTING: (&str, &str) = (ENTER_KEY, "its setting");
pub(super) const HOOK_FOLLOWS: &str = "follows";
pub(super) const PROMPT_SECTION_LEAD: &str = "prompt section:";
pub const ADD_MODEL: &str = "Add a model\u{2026}";
pub const WHEN_YOU_SWITCH: &str = "when you switch to it";
pub const PATH_HIDDEN: &str = "(path hidden)";
pub const PROVIDER_URL: &str = "the provider's URL";
pub const URL_FROM_VARIABLE: &str = "from its variable";
pub const SWITCH_FIRST: &str = "Switch to another account first.";
pub const NO_FIELDS: &str = "it has no fields to change here";
pub const NOT_REMOVABLE: &str = "it can't be removed";

pub(super) fn keys_file_only(file: &str, ending: &str) -> String {
    format!("Its key comes from {file} or your environment, so {ending}.")
}
pub const KEY_STAYS: &str =
    "A key can't be removed on its own; enter a new one, or remove the account.";
pub const NOT_RESETTABLE: &str = "This field can't be reset; choose another value instead.";
pub const NOTHING_FOR_MODEL: &str = "Nothing to remove: no settings are saved for this model.";
pub const MODEL_LEAD: &str = "Model";
pub const OPEN_KEY: (&str, &str) = ("\u{2192}", "its settings");
pub(super) const OPEN_PAGE_KEY: (&str, &str) = (ENTER_KEY, "open");
pub(super) const PLUGINS_HELP: &str = "Turn plugins on or off, or open their settings.";
pub const OPEN_MODEL_KEY: (&str, &str) = (ENTER_KEY, "its settings");
pub const ADD_KEY: (&str, &str) = (ENTER_KEY, "add");
pub const REMOVE_KEY: (&str, &str) = ("d", "remove");
pub const REMOVE_MODEL_KEY: (&str, &str) = ("d", "remove your settings");
pub const PROMPT_KEY: (&str, &str) = (ENTER_KEY, "type a new one");
pub const URL_RESET_KEY: (&str, &str) = ("d", "use the provider's URL");
pub const UNSET_KEY: (&str, &str) = ("d", "unset");
pub const ASK_KEYS: Keys = &[CANCEL_KEY, CHOOSE_KEY];
pub const ALL_ENDPOINTS: &str = "all accounts";
pub const ADD_ENDPOINT_HELP: &str = "A provider, its key and a name.";
pub const ADD_MODEL_HELP: &str =
    "Add settings for a model: choose it, or type its ID, then set its fields.";
pub const UNCHECKED_ASK: &str = "Save it anyway, without the check?";

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Page {
    #[default]
    Home,
    Accounts,
    Plugins,
    Plugin(String),
    More,
    Group(Section),
}

impl Page {
    pub fn parent(&self) -> Option<Page> {
        match self {
            Page::Home => None,
            Page::Accounts | Page::Plugins | Page::More => Some(Page::Home),
            Page::Plugin(_) => Some(Page::Plugins),
            Page::Group(_) => Some(Page::More),
        }
    }

    pub fn title(&self) -> &str {
        match self {
            Page::Home => "",
            Page::Accounts => Section::Endpoints.title(),
            Page::Plugins => Section::Plugins.title(),
            Page::Plugin(id) => id,
            Page::More => "More settings",
            Page::Group(section) => section.title(),
        }
    }

    pub fn setting(name: &str) -> Page {
        if super::EVERYDAY.contains(&name) {
            Page::Home
        } else {
            wisp_protocol::settings::find(name).map_or(Page::Home, |spec| match spec.section {
                Section::Endpoints => Page::Accounts,
                Section::Plugins => spec
                    .plugin
                    .map_or(Page::Home, |id| Page::Plugin(id.to_owned())),
                section => Page::Group(section),
            })
        }
    }

    pub fn counts(name: &str) -> bool {
        matches!(Self::setting(name), Self::Group(_) | Self::Plugin(_))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Spot {
    Open(Page),
    Setting(&'static str),
    Endpoint(String),
    AddEndpoint,
    Model(String),
    AddModel,
    Plugin(String),
    Section(String),
    Hook(String),
    Search,
}

impl Spot {
    pub fn setting(&self) -> Option<&'static str> {
        match self {
            Spot::Setting(name) => Some(name),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub spot: Spot,
    pub section: Section,
    pub label: String,
    pub value: String,
    pub aside: Option<String>,
}

pub fn table_only_question(which: &str) -> String {
    format!("The keychain {which}. Remove only the table and leave the keychain item?")
}

pub fn remove_model_question(id: &str) -> String {
    format!(
        "Remove your settings for {id}? {} numbers stay.",
        wisp_render::chrome::config_screen::WISPS_OWN
    )
}

pub fn heading(spot: &Spot) -> String {
    match spot {
        Spot::Endpoint(name) => format!("{ACCOUNT_TITLE} {name}"),
        Spot::Model(id) => format!("{MODEL_LEAD} {id}"),
        Spot::Plugin(id) => format!("{PLUGIN_LEAD} {id}"),
        Spot::Section(name) => format!("{SECTION_LEAD} {name}"),
        Spot::Hook(name) => format!("{HOOK_LEAD} {}", sanitize(name)),
        Spot::Open(page) => page.title().to_owned(),
        Spot::Setting(_) | Spot::AddEndpoint | Spot::AddModel | Spot::Search => String::new(),
    }
}

fn saved_at<'a>(
    snapshot: &'a SettingsSnapshot,
    path: &SitePath,
) -> Option<(&'a SettingValue, Option<u64>)> {
    snapshot
        .saved
        .iter()
        .filter(|s| s.path == *path)
        .find_map(|s| match &s.entry {
            SavedEntry::Value { value } => Some((value, s.line)),
            SavedEntry::Secret { .. } | SavedEntry::Host { .. } | SavedEntry::Other(_) => None,
        })
}

fn endpoint_text(snapshot: &SettingsSnapshot, name: &str, field: &str) -> Option<String> {
    let spec = wisp_protocol::settings::find(field)?;
    let path = SitePath::new(Place::Endpoint(name.to_owned()), spec);
    saved_at(snapshot, &path).map(|(value, _)| value.text())
}

pub fn store_of(snapshot: &SettingsSnapshot, name: &str) -> Option<String> {
    endpoint_text(snapshot, name, names::KEY_STORE)
}

pub(super) fn summary_of<'a>(
    snapshot: &'a SettingsSnapshot,
    name: &str,
) -> Option<&'a EndpointSummary> {
    snapshot.endpoints.iter().find(|e| e.name == name)
}

pub(super) fn keys_file(snapshot: &SettingsSnapshot, name: &str) -> bool {
    summary_of(snapshot, name).is_some_and(|summary| summary.keys_file)
}

fn kind_words(summary: &EndpointSummary) -> String {
    endpoint_facts_of(summary).kind().map_or_else(
        || sanitize(&summary.provider),
        |label| match &label.host {
            Some(host) => format!("{}{SEP}{}", label.name, sanitize(host)),
            None => label.name.to_owned(),
        },
    )
}

fn endpoint_item(summary: &EndpointSummary, current: &str) -> Item {
    let mut parts = vec![kind_words(summary)];
    if summary.default && summary.name != current {
        parts.push(NEW_CHATS.to_owned());
    }
    let value = parts.join(SEP);
    Item {
        spot: Spot::Endpoint(summary.name.clone()),
        section: Section::Endpoints,
        label: if summary.name == current {
            format!("{} \u{2713}", sanitize(&summary.name))
        } else {
            sanitize(&summary.name)
        },
        value,
        aside: summary.unavailable.as_deref().map(sanitize),
    }
}

fn model_ids(snapshot: &SettingsSnapshot) -> Vec<String> {
    let mut ids: BTreeSet<String> = snapshot.models.iter().map(|m| m.id.clone()).collect();
    for saved in &snapshot.saved {
        if let Place::Model(id) | Place::EndpointModel { model: id, .. } = &saved.path.place {
            ids.insert(id.clone());
        }
    }
    ids.into_iter()
        .filter(|id| !id.is_empty() && !id.contains(NOT_A_MODEL_ID))
        .collect()
}

pub(super) fn configured(snapshot: &SettingsSnapshot, id: &str) -> bool {
    snapshot.saved.iter().any(|s| match &s.path.place {
        Place::Model(m) | Place::EndpointModel { model: m, .. } => m == id,
        Place::Global
        | Place::Endpoint(_)
        | Place::Agent(_)
        | Place::PluginHook { .. }
        | Place::PluginSlot { .. }
        | Place::Plugin(_) => false,
    })
}

fn entry_of<'a>(snapshot: &'a SettingsSnapshot, id: &str) -> Option<&'a ModelEntry> {
    snapshot.models.iter().find(|m| m.id == id)
}

fn model_value(snapshot: &SettingsSnapshot, id: &str) -> String {
    let place = Place::Model(id.to_owned());
    let at = |name: &str| {
        wisp_protocol::settings::find(name)
            .and_then(|spec| saved_at(snapshot, &SitePath::new(place.clone(), spec)))
            .map(|(value, _)| value.clone())
    };
    let entry = entry_of(snapshot, id);
    let mut parts = vec![];
    let window = match at(names::CONTEXT_WINDOW) {
        Some(SettingValue::Tokens(n)) => Some(n),
        _ => entry.and_then(|e| e.context_window),
    };
    if let Some(window) = window {
        parts.push(format!("{} window", thousands(window)));
    }
    if let Some(point) = at(names::COMPACT_AT) {
        parts.push(format!("compacts at {}", point.shown()));
    }
    parts.join(SEP)
}

fn model_item(snapshot: &SettingsSnapshot, id: &str) -> Item {
    let shipped = entry_of(snapshot, id).is_some_and(|e| e.shipped);
    let label = if model_hidden(id) || id.contains(NOT_SHOWN) {
        NOT_SHOWN.to_owned()
    } else if shipped {
        short_model(id)
    } else {
        model_line(id)
    };
    Item {
        spot: Spot::Model(id.to_owned()),
        section: Section::Models,
        label,
        value: model_value(snapshot, id),
        aside: None,
    }
}

pub fn items(snapshot: &SettingsSnapshot, display: &super::Display) -> Vec<Item> {
    let mut endpoints: Vec<&EndpointSummary> = snapshot.endpoints.iter().collect();
    endpoints.sort_by(|a, b| a.name.cmp(&b.name));
    let mut out: Vec<Item> = endpoints
        .into_iter()
        .map(|e| endpoint_item(e, &snapshot.context.endpoint))
        .collect();
    out.push(Item {
        spot: Spot::AddEndpoint,
        section: Section::Endpoints,
        label: endpoint_card::ADD_TITLE.to_owned() + "\u{2026}",
        value: String::new(),
        aside: None,
    });
    out.extend(
        model_ids(snapshot)
            .iter()
            .map(|id| model_item(snapshot, id)),
    );
    out.push(Item {
        spot: Spot::AddModel,
        section: Section::Models,
        label: ADD_MODEL.to_owned(),
        value: String::new(),
        aside: None,
    });
    out.extend(SearchRow::of(snapshot).map(|row| row.item()));
    for plugin in &snapshot.plugins {
        out.push(plugin_item(plugin, snapshot));
        out.extend(
            plugin
                .slots
                .iter()
                .filter(|s| s.slot == SlotPlace::Sidebar)
                .map(|slot| section_item(plugin, slot, display)),
        );
        out.extend(plugin.hooks.iter().map(|hook| hook_item(hook, snapshot)));
    }
    out
}

pub(super) fn plugin_count(snapshot: &SettingsSnapshot) -> String {
    let on = snapshot
        .plugins
        .iter()
        .filter(|p| {
            let row = PluginRow::of(p, snapshot);
            row.installed && row.enabled
        })
        .count();
    super::rows::count(on, snapshot.plugins.len())
}

fn hook_item(entry: &HookEntry, snapshot: &SettingsSnapshot) -> Item {
    Item {
        spot: Spot::Hook(entry.hook.clone()),
        section: Section::Plugins,
        label: sanitize(&entry.hook),
        value: HookRow::of(entry, snapshot).state(entry),
        aside: None,
    }
}

pub(super) fn hook_placed<'a>(
    entry: &HookEntry,
    snapshot: &'a SettingsSnapshot,
) -> Option<&'a PlacedValue> {
    if entry.switch.is_some() {
        return None;
    }
    let (plugin, hook) = entry.hook.split_once('/')?;
    snapshot.placed(&SitePath::plugin_hook(plugin, hook)?)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HookRow {
    pub(super) value: Option<bool>,
    owned: Option<bool>,
}

fn setting_on(value: &SettingValue) -> bool {
    !matches!(value, SettingValue::Bool(false))
        && !matches!(value, SettingValue::Text(t) if t == "off")
}

impl HookRow {
    pub(super) fn of(entry: &HookEntry, snapshot: &SettingsSnapshot) -> Self {
        let on = || {
            hook_placed(entry, snapshot)
                .and_then(|p| p.value.as_ref())
                .is_none_or(|v| !matches!(v, SettingValue::Bool(false)))
        };
        let owned = entry.switch.as_deref().and_then(|switch| {
            snapshot
                .settings
                .iter()
                .find(|r| r.name.as_str() == switch)
                .and_then(|r| r.value.as_ref())
                .map(setting_on)
        });
        Self {
            value: entry.switch.is_none().then(on),
            owned,
        }
    }

    pub(super) fn state(self, entry: &HookEntry) -> String {
        let waiting = |word: &str| format!("{word}{SEP}{}", pending(NEXT_TURN));
        let switched_off = entry.disabled == Some(HookDisabled::SwitchedOff);
        match (&entry.switch, self.value) {
            (Some(switch), _) => {
                let word = match self.owned {
                    Some(false) if entry.on => waiting("off"),
                    Some(true) if switched_off => waiting("on"),
                    _ => hook_reason(entry),
                };
                format!("{word}{SEP}{HOOK_FOLLOWS} {}", sanitize(switch))
            }
            (None, Some(false)) if entry.on => waiting("off"),
            (None, Some(false)) => "off".to_owned(),
            (None, _) if switched_off => waiting("on"),
            (None, _) => hook_reason(entry),
        }
    }

    pub(super) fn keys(self, entry: &HookEntry) -> Vec<KeyHint> {
        let key = match (&entry.disabled, &entry.switch, self.value) {
            (Some(HookDisabled::LeftOut), _, _) => return Vec::new(),
            (_, Some(_), _) => HOOK_KEY_SETTING,
            (_, None, Some(false)) => PLUGIN_KEY_ON,
            (_, None, _) => PLUGIN_KEY_OFF,
        };
        vec![hint(key)]
    }
}

fn hook_reason(entry: &HookEntry) -> String {
    match &entry.disabled {
        None => "on".to_owned(),
        Some(HookDisabled::LeftOut) => HOOK_LEFT_OUT.to_owned(),
        Some(HookDisabled::Cycle { .. }) => SECTION_CYCLE.to_owned(),
        Some(HookDisabled::Missing { requires }) => {
            let names: Vec<String> = requires.iter().map(|n| sanitize(n)).collect();
            format!("off (requires {})", names.join(", "))
        }
        Some(HookDisabled::PluginOff) => PLUGIN_OFF.to_owned(),
        Some(HookDisabled::SwitchedOff | HookDisabled::Unknown) => "off".to_owned(),
    }
}

pub(super) fn section_on(slot: &SlotEntry, display: &super::Display) -> (bool, String) {
    let found = switch_path(&slot.name).and_then(|path| display.section(&path));
    match found {
        Some(Resolved {
            value: SettingValue::Bool(on),
            source,
        }) => (*on, source.clone()),
        _ => (slot.default_on, DEFAULT.to_owned()),
    }
}

fn section_item(plugin: &PluginEntry, slot: &SlotEntry, display: &super::Display) -> Item {
    let (on, _) = section_on(slot, display);
    let state = match (on, plugin.enabled, &slot.disabled) {
        (false, _, _) => "off",
        (true, false, _) => PLUGIN_OFF,
        (true, true, Some(_)) => SECTION_CYCLE,
        (true, true, None) => "on",
    };
    Item {
        spot: Spot::Section(slot.name.clone()),
        section: Section::Plugins,
        label: sanitize(&slot.name),
        value: state.to_owned(),
        aside: None,
    }
}

pub(super) const PLUGIN_STATE_WORD: &str = "State";

fn waiting(word: &str) -> String {
    format!("{word}{SEP}{}", pending(NEXT_TURN))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PluginRow {
    pub(super) enabled: bool,
    pub(super) installed: bool,
}

impl PluginRow {
    pub(super) fn of(plugin: &PluginEntry, snapshot: &SettingsSnapshot) -> Self {
        Self {
            enabled: snapshot.plugin_on(&plugin.id, wisp_protocol::settings::ENABLED_KEY),
            installed: snapshot.plugin_on(&plugin.id, wisp_protocol::settings::INSTALLED_KEY),
        }
    }

    fn word(self) -> &'static str {
        match (self.installed, self.enabled) {
            (false, _) => PLUGIN_REMOVED,
            (true, true) => "on",
            (true, false) => "off",
        }
    }

    pub(super) fn waits(self, plugin: &PluginEntry) -> bool {
        self.installed != plugin.installed || (plugin.installed && self.enabled != plugin.enabled)
    }

    pub(super) fn state(self, plugin: &PluginEntry) -> String {
        let word = self.word();
        if self.waits(plugin) {
            waiting(word)
        } else {
            word.to_owned()
        }
    }

    pub(super) fn keys(self) -> Vec<KeyHint> {
        if !self.installed {
            return vec![hint(PLUGIN_KEY_ADD)];
        }
        let enter = if self.enabled {
            PLUGIN_KEY_OFF
        } else {
            PLUGIN_KEY_ON
        };
        vec![hint(enter), hint(REMOVE_KEY)]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SearchState {
    Reason(String),
    Waiting,
    Now { on: bool, chat: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SearchRow {
    pub(super) plugin: String,
    pub(super) tool: String,
    pub(super) state: SearchState,
    pub(super) saved: Option<Vec<String>>,
    pub(super) asked: bool,
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

impl SearchRow {
    pub(super) fn of(snapshot: &SettingsSnapshot) -> Option<Self> {
        let (plugin, tool) = snapshot
            .plugins
            .iter()
            .find_map(|p| Some((p, p.tools.iter().find(|t| t.is_search())?)))?;
        let switch = PluginRow::of(plugin, snapshot);
        if !switch.installed {
            return None;
        }
        let spec = wisp_protocol::settings::find(names::TOOLS)?;
        let list = |value| match value {
            Some(SettingValue::List(list)) => Some(list),
            _ => None,
        };
        let saved = list(saved_value(spec, &SitePath::global(spec), snapshot));
        let current = snapshot
            .settings
            .iter()
            .find(|r| r.name.spec().name == spec.name)
            .and_then(|r| r.value.clone());
        let listed = saved
            .as_ref()
            .map_or(tool.default_on, |list| list.contains(&tool.name));
        let asked = switch.enabled && listed;
        let in_force = list(current).is_some_and(|list| list.contains(&tool.name));
        let put_back = asked && plugin.enabled && !tool.on && in_force;
        let state = match (&tool.unavailable, switch.waits(plugin) || put_back) {
            (Some(why), _) => SearchState::Reason(sanitize(why)),
            (None, true) => SearchState::Waiting,
            (None, false) => SearchState::Now {
                on: tool.on,
                chat: tool.on != asked,
            },
        };
        Some(Self {
            plugin: plugin.id.clone(),
            tool: tool.name.clone(),
            state,
            saved,
            asked,
        })
    }

    fn item(&self) -> Item {
        let value = match &self.state {
            SearchState::Reason(why) => why.clone(),
            SearchState::Waiting => waiting(on_off(self.asked)),
            SearchState::Now { on, .. } => on_off(*on).to_owned(),
        };
        Item {
            spot: Spot::Search,
            section: Section::Chat,
            label: SEARCH_LEAD.to_owned(),
            value,
            aside: None,
        }
    }

    pub(super) fn turns_on(&self) -> Option<bool> {
        match self.state {
            SearchState::Now { on, chat: false } => Some(!on),
            SearchState::Waiting => Some(!self.asked),
            SearchState::Reason(_) | SearchState::Now { chat: true, .. } => None,
        }
    }

    pub(super) fn chat_note(&self) -> Option<String> {
        let SearchState::Now { on, chat: true } = self.state else {
            return None;
        };
        Some(super::detail::chat_values(on_off(on), on_off(self.asked)))
    }

    pub(super) fn saved_switch(&self, snapshot: &SettingsSnapshot) -> Option<(SitePath, String)> {
        let SearchState::Reason(why) = &self.state else {
            return None;
        };
        let (endpoint, model) = (&snapshot.context.endpoint, &snapshot.context.model);
        let (name, off, whose, places) = if NoSearch::TurnedOff.why() == Some(why.as_str()) {
            let places = vec![Place::Endpoint(endpoint.clone())];
            (
                names::NATIVE_WEB_SEARCH,
                SettingValue::Bool(false),
                "account",
                places,
            )
        } else if NoSearch::NotSupported.why() == Some(why.as_str()) {
            let both = Place::EndpointModel {
                endpoint: endpoint.clone(),
                model: model.clone(),
            };
            let places = vec![both, Place::Model(model.clone())];
            (
                names::WEB_SEARCH,
                SettingValue::List(Vec::new()),
                "model",
                places,
            )
        } else {
            return None;
        };
        let spec = wisp_protocol::settings::find(name)?;
        let path = places
            .into_iter()
            .map(|place| SitePath::new(place, spec))
            .find(|path| saved_value(spec, path, snapshot).as_ref() == Some(&off))?;
        Some((
            path,
            format!(
                "{} for this {whose}: off.",
                wisp_render::chrome::config_screen::SAVED
            ),
        ))
    }

    pub(super) fn leaves_out(&self) -> Option<&[String]> {
        self.saved
            .as_deref()
            .filter(|list| !list.contains(&self.tool))
    }
}

pub(super) fn remove_plugin_question(id: &str) -> String {
    format!("Remove {id}?")
}

pub(super) fn removed_plugin_of(snapshot: &SettingsSnapshot, spec: &SettingSpec) -> bool {
    spec.plugin.is_some_and(|id| {
        !snapshot.plugin_on(id, wisp_protocol::settings::INSTALLED_KEY)
            && snapshot.plugins.iter().any(|p| p.id == id)
    })
}

fn plugin_item(plugin: &PluginEntry, snapshot: &SettingsSnapshot) -> Item {
    Item {
        spot: Spot::Plugin(plugin.id.clone()),
        section: Section::Plugins,
        label: sanitize(&plugin.id),
        value: PluginRow::of(plugin, snapshot).state(plugin),
        aside: plugin.problems.first().map(|p| sanitize(p)),
    }
}

pub(super) fn tool_state(tool: &ToolEntry, plugin: &PluginEntry) -> String {
    let state = match (&tool.unavailable, plugin.enabled) {
        (Some(why), _) => format!("off ({})", sanitize(why)),
        (None, false) => PLUGIN_OFF.to_owned(),
        (None, true) => on_off(tool.on || tool.held).to_owned(),
    };
    let state = if tool.waits {
        format!("{state}{SEP}{}", pending(NEW_CHATS))
    } else {
        state
    };
    match &tool.follows {
        Some(_) if tool.name == wisp_protocol::agents::REPORT => {
            format!("{state}{SEP}{CHILD_AGENTS_ONLY}")
        }
        Some(leader) if !tool.default_on => format!("{state}, follows {}", sanitize(leader)),
        Some(leader) => format!("{state}, with {}", sanitize(leader)),
        None if tool.is_search() => format!("{state}{SEP}switch in {SEARCH_LEAD}"),
        None => format!("{state}{SEP}{SWITCH_IN_TOOLS}"),
    }
}

fn section_place(place: SectionPlace) -> &'static str {
    match place {
        SectionPlace::System => "in the project block",
        SectionPlace::Lead => "in each turn's message",
        SectionPlace::Unknown => "placed by a newer Wisp",
    }
}

pub(super) fn plugin_notes(plugin: &PluginEntry) -> Vec<String> {
    let mut notes = vec![];
    notes.extend(
        plugin
            .setting_names()
            .map(|name| format!("{SETTING_LEAD} {}", sanitize(&name))),
    );
    notes.extend(plugin.prompt_sections.iter().map(|section| {
        format!(
            "{PROMPT_SECTION_LEAD} {}{SEP}{}",
            sanitize(&section.name),
            section_place(section.place)
        )
    }));
    notes.extend(plugin.problems.iter().map(|p| sanitize(p)));
    notes
}

pub(super) const ENDPOINT_FIELDS: &[&str] = &[
    names::PROVIDER,
    names::AUTH,
    names::PRICING,
    names::MAX_IN_FLIGHT,
    names::CLIENT_VERSION,
    names::KEY,
    names::URL,
    names::KEY_STORE,
    names::KEY_ENV,
    names::URL_ENV,
];

pub(super) fn is_connection(spec: &SettingSpec) -> bool {
    spec.applies == Applies::Connection
}

pub(super) fn prompts(spec: &SettingSpec) -> bool {
    is_connection(spec) && spec.secrecy == Secrecy::Secret
}

fn choices_of(spec: &SettingSpec) -> Vec<String> {
    match spec.kind {
        Kind::Bool => vec!["on".to_owned(), "off".to_owned()],
        kind => kind.choices().iter().map(|c| (*c).to_owned()).collect(),
    }
}

fn default_text(spec: &SettingSpec) -> String {
    if spec.name == names::COMPACT_AT {
        return String::new();
    }
    match spec.default {
        Default::Const(text) | Default::Detected(text) | Default::Rule(text) => text.to_owned(),
        Default::None => NOT_SET.to_owned(),
    }
}

fn set(place: &str) -> String {
    format!("set{SEP}{place}")
}

fn key_state(snapshot: &SettingsSnapshot, name: &str, summary: Option<&EndpointSummary>) -> String {
    let unavailable = summary.is_some_and(|s| s.unavailable.is_some());
    match store_of(snapshot, name).as_deref().map(StoreChoice::parse) {
        Some(Some(StoreChoice::Keyring)) if !unavailable => set("keychain"),
        Some(Some(StoreChoice::File)) if !unavailable => set(wisp_protocol::CREDENTIALS),
        Some(_) => NOT_SET.to_owned(),
        None if endpoint_text(snapshot, name, names::KEY_ENV).is_some() && !unavailable => {
            set("variable")
        }
        None => NOT_SET.to_owned(),
    }
}

fn url_state(snapshot: &SettingsSnapshot, name: &str, summary: Option<&EndpointSummary>) -> String {
    if endpoint_text(snapshot, name, names::URL_ENV).is_some() {
        return URL_FROM_VARIABLE.to_owned();
    }
    match summary {
        Some(s) if s.official != Some(true) => match &s.host {
            Some(host) => format!("{} {PATH_HIDDEN}", sanitize(host)),
            None => PROVIDER_URL.to_owned(),
        },
        _ => PROVIDER_URL.to_owned(),
    }
}

fn field_row(
    spec: &'static SettingSpec,
    section: Section,
    path: SitePath,
    value: Option<(SettingValue, Layer, String, Option<u64>)>,
    shipped: Option<(SettingValue, String)>,
) -> Row {
    let winner = value.as_ref().map(|(value, layer, origin, line)| Found {
        set: Set::Layer(layer.clone()),
        value: shown(spec, value),
        origin: wisp_protocol::hide_quoted_models(origin),
        line: *line,
    });
    let shipped_found = shipped.as_ref().map(|(value, origin)| Found {
        set: Set::Layer(Layer::Shipped),
        value: shown(spec, value),
        origin: origin.clone(),
        line: None,
    });
    let (winner, shadowed) = match (winner, shipped_found) {
        (Some(w), Some(s)) => (Some(w), vec![s]),
        (Some(w), None) => (Some(w), vec![]),
        (None, Some(s)) => (Some(s), vec![]),
        (None, None) => (None, vec![]),
    };
    let default = default_text(spec);
    let (shown_value, source) = match &winner {
        Some(found) => (found.value.clone(), found.word()),
        None => (
            if default.is_empty() {
                NOT_SET.to_owned()
            } else {
                default.clone()
            },
            DEFAULT.to_owned(),
        ),
    };
    let current = value
        .as_ref()
        .map(|(v, ..)| v.clone())
        .or_else(|| shipped.as_ref().map(|(value, _)| value.clone()))
        .or_else(|| spec.default_value());
    Row {
        spec,
        section,
        value: shown_value,
        source,
        saved_here: value.as_ref().map(|(v, ..)| shown(spec, v)),
        winner,
        shadowed,
        default,
        choices: choices_of(spec),
        checks: choices_of(spec)
            .iter()
            .map(|c| super::edit::CheckItem::plain(c))
            .collect(),
        applies: true,
        path,
        changed: None,
        new_chat: current.clone(),
        current,
    }
}

pub fn endpoint_rows(snapshot: &SettingsSnapshot, name: &str) -> Vec<Row> {
    let summary = summary_of(snapshot, name);
    let stored = store_of(snapshot, name).is_some();
    endpoint_specs()
        .filter(|spec| match spec.name {
            names::CLIENT_VERSION => summary
                .and_then(|summary| summary.credential.as_ref())
                .and_then(Credential::of)
                .and_then(Credential::plan)
                .is_some(),
            names::KEY_STORE => stored,
            names::KEY_ENV | names::URL_ENV => !stored,
            _ => true,
        })
        .map(|spec| {
            let path = SitePath::new(Place::Endpoint(name.to_owned()), spec);
            let mut row = match saved_at(snapshot, &path) {
                Some((value, line)) => field_row(
                    spec,
                    Section::Endpoints,
                    path.clone(),
                    Some((value.clone(), Layer::Endpoint, path.to_string(), line)),
                    None,
                ),
                None => field_row(spec, Section::Endpoints, path, None, None),
            };
            if let Some(value) = spec.default_for(summary.and_then(|s| s.credential.as_ref())) {
                row.default = shown(spec, &value);
                if row.winner.is_none() {
                    row.value = row.default.clone();
                    row.current = Some(value);
                }
            }
            match spec.name {
                names::KEY => {
                    row.value = key_state(snapshot, name, summary);
                    row.source = String::new();
                    row.winner = None;
                }
                names::URL => {
                    row.value = url_state(snapshot, name, summary);
                    row.source = String::new();
                    row.winner = None;
                }
                _ => {}
            }
            row
        })
        .collect()
}

pub(super) fn endpoint_specs() -> impl Iterator<Item = &'static SettingSpec> {
    let mut specs: Vec<_> = SETTINGS
        .iter()
        .filter(|s| {
            s.is_built()
                && s.has_site(Site::Endpoint)
                && !s.has_site(Site::Global)
                && !OFF_SCREEN.contains(&s.name)
        })
        .collect();
    specs.sort_by_key(|s| {
        ENDPOINT_FIELDS
            .iter()
            .position(|field| *field == s.name)
            .unwrap_or(ENDPOINT_FIELDS.len())
    });
    specs.into_iter()
}

pub(super) fn model_specs() -> impl Iterator<Item = &'static SettingSpec> {
    SETTINGS.iter().filter(|spec| {
        spec.status == Status::Built
            && spec.secrecy == Secrecy::Plain
            && spec.has_site(Site::Model)
            && !OFF_SCREEN.contains(&spec.name)
    })
}

fn shipped_value(spec: &SettingSpec, entry: Option<&ModelEntry>) -> Option<SettingValue> {
    let entry = entry?;
    match spec.name {
        names::CONTEXT_WINDOW => entry.context_window.map(SettingValue::Tokens),
        _ => None,
    }
}

pub fn model_rows(snapshot: &SettingsSnapshot, id: &str) -> Vec<Row> {
    let entry = entry_of(snapshot, id);
    model_specs()
        .map(|spec| {
            let path = SitePath::new(Place::Model(id.to_owned()), spec);
            let saved = saved_at(snapshot, &path)
                .map(|(value, line)| (value.clone(), Layer::Model, path.to_string(), line));
            field_row(
                spec,
                Section::Models,
                path,
                saved,
                shipped_value(spec, entry)
                    .map(|value| (value, wisp_protocol::settings::shipped_origin(id))),
            )
        })
        .collect()
}

pub(super) fn model_saved_paths(snapshot: &SettingsSnapshot, id: &str) -> Vec<SitePath> {
    snapshot
        .saved
        .iter()
        .filter(|s| match &s.path.place {
            Place::Model(m) | Place::EndpointModel { model: m, .. } => m == id,
            Place::Global
            | Place::Endpoint(_)
            | Place::Agent(_)
            | Place::PluginHook { .. }
            | Place::PluginSlot { .. }
            | Place::Plugin(_) => false,
        })
        .map(|s| s.path.clone())
        .collect()
}

pub(super) fn connection_edit(
    spec: &SettingSpec,
    value: Option<&SettingValue>,
) -> Option<(EndpointEdit, Option<StoreChoice>)> {
    let mut edit = EndpointEdit::default();
    let mut store = None;
    let text = value.map(SettingValue::text);
    match (spec.name, text) {
        (names::PROVIDER, Some(text)) => edit.provider = Some(text),
        (names::AUTH, Some(text)) => edit.auth = Some(text),
        (names::KEY_STORE, Some(text)) => {
            store = Some(StoreChoice::parse(&text).filter(|c| *c != StoreChoice::Auto)?);
        }
        (names::KEY_ENV, Some(text)) => {
            edit.key_env = Some(VariableEdit::Set(VariableName::parse(&text)?));
        }
        (names::URL_ENV, Some(text)) => {
            edit.url_env = Some(VariableEdit::Set(VariableName::parse(&text)?));
        }
        (names::KEY_ENV, None) => edit.key_env = Some(VariableEdit::Unset),
        (names::URL_ENV, None) => edit.url_env = Some(VariableEdit::Unset),
        (names::URL, None) => edit.url = Some(UrlEdit::ProviderDefault),
        _ => return None,
    }
    Some((edit, store))
}

pub(super) fn can_reset_connection(spec: &SettingSpec, row: &Row) -> bool {
    match spec.name {
        names::KEY_ENV | names::URL_ENV => row.saved_here.is_some(),
        names::URL => row.value != PROVIDER_URL,
        _ => false,
    }
}
