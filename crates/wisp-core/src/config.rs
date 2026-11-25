use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use toml::de::{DeTable, DeValue};
use wisp_protocol::SECRET_FIELDS;
use wisp_protocol::settings::{
    AGENTS_GROUP, Kind, PRESETS_TABLE, Place, SETTINGS, SavedEntry, SavedValue, Secrecy,
    SettingSpec, SettingValue, Site, SitePath, TokensOrPercent, find, names, parse_tokens,
    parse_usd,
};

use crate::config_file::SiteEdit;
use crate::context::CompactAt;
use crate::endpoints::{line_of, not_toml};
use crate::models::{ConfigModels, ModelRow};

#[derive(Clone, PartialEq, Eq)]
pub struct Saved {
    pub value: SettingValue,
    pub line: usize,
}

impl Saved {
    pub fn shown(&self, spec: &SettingSpec) -> String {
        shown_value(spec, &self.value)
    }
}

pub fn shown_value(spec: &SettingSpec, value: &SettingValue) -> String {
    spec.shown(value)
}

impl fmt::Debug for Saved {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Saved")
            .field("value", &self.value.kind_name())
            .field("line", &self.line)
            .finish()
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct SavedSettings {
    values: BTreeMap<SitePath, Saved>,
    named: BTreeSet<String>,
}

fn place_endpoint(place: &Place) -> Option<&str> {
    match place {
        Place::Endpoint(endpoint) | Place::EndpointModel { endpoint, .. } => Some(endpoint),
        _ => None,
    }
}

fn on_endpoint(path: &SitePath, name: &str) -> SitePath {
    let place = match &path.place {
        Place::Endpoint(_) => Place::Endpoint(name.to_owned()),
        Place::EndpointModel { model, .. } => Place::EndpointModel {
            endpoint: name.to_owned(),
            model: model.clone(),
        },
        other => other.clone(),
    };
    SitePath::new(place, path.spec())
}

pub fn shown_for_keys_file(saved: &mut [SavedValue], keys_file: Option<&str>) {
    let env = crate::endpoints::ENV;
    for value in saved {
        if place_endpoint(&value.path.place) == Some(env) {
            value.path = on_endpoint(&value.path, keys_file.unwrap_or(wisp_protocol::CLAUDE_API));
        }
        if let (Place::Global, true, SavedEntry::Value { value }) = (
            &value.path.place,
            value.path.spec().kind == Kind::EndpointName,
            &mut value.entry,
        ) {
            shown_endpoint(value, Some(keys_file_shown(keys_file)));
        }
    }
}

pub fn keys_file_of(endpoints: &[wisp_protocol::settings::EndpointSummary]) -> Option<&str> {
    endpoints
        .iter()
        .find(|endpoint| endpoint.keys_file)
        .map(|endpoint| endpoint.name.as_str())
}

pub fn keys_file_shown(keys_file: Option<&str>) -> &str {
    keys_file.unwrap_or(wisp_protocol::KEYS_FILE_ACCOUNT)
}

pub fn shown_endpoint(value: &mut SettingValue, keys_file: Option<&str>) {
    if let Some(keys_file) = keys_file
        && value.text() == crate::endpoints::ENV
    {
        *value = SettingValue::Text(keys_file.to_owned());
    }
}

pub fn table_path(path: &SitePath, named: impl Fn(&str) -> bool) -> Option<SitePath> {
    let endpoint = place_endpoint(&path.place)?;
    crate::endpoints::keys_file_table(endpoint, named)
        .then(|| on_endpoint(path, crate::endpoints::ENV))
}

impl fmt::Debug for SavedSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(
                self.values
                    .iter()
                    .map(|(path, saved)| (path.to_string(), saved.shown(path.spec()))),
            )
            .finish()
    }
}

impl SavedSettings {
    pub fn insert(&mut self, path: SitePath, value: SettingValue, line: usize) {
        self.values.insert(path, Saved { value, line });
    }

    pub fn remove(&mut self, path: &SitePath) -> Option<Saved> {
        self.values.remove(path)
    }

    fn keyed<'a>(&self, path: &'a SitePath) -> Cow<'a, SitePath> {
        match table_path(path, |name| self.named.contains(name)) {
            Some(table) => Cow::Owned(table),
            None => Cow::Borrowed(path),
        }
    }

    pub fn get(&self, path: &SitePath) -> Option<&Saved> {
        self.values.get(self.keyed(path).as_ref())
    }

    pub fn edit(&mut self, edit: &SiteEdit, line: Option<u64>) {
        match edit {
            SiteEdit::Set(path, value) => {
                let line = line.and_then(|l| usize::try_from(l).ok()).unwrap_or(0);
                self.insert(self.keyed(path).into_owned(), value.clone(), line);
            }
            SiteEdit::Unset(path) => {
                self.remove(&self.keyed(path));
            }
        }
    }

    pub fn from_models(models: &ConfigModels) -> Self {
        let mut saved = Self::default();
        saved.name_tables(models.tables.iter().map(String::as_str));
        let at_models = models
            .models
            .iter()
            .map(|row| (Place::Model(row.id.clone()), row));
        let at_endpoints = models.endpoints.iter().flat_map(|(endpoint, rows)| {
            rows.iter().map(move |row| {
                let (endpoint, model) = (endpoint.clone(), row.id.clone());
                (Place::EndpointModel { endpoint, model }, row)
            })
        });
        for (place, row) in at_models.chain(at_endpoints) {
            for spec in SETTINGS
                .iter()
                .filter(|s| s.is_built() && s.has_site(place.site()))
            {
                if let Some(value) = row_value(row, spec.leaf()) {
                    saved.insert(SitePath::new(place.clone(), spec), value, 0);
                }
            }
        }
        if let (Some(at), Some(spec)) = (models.compact_at, find(names::COMPACT_AT)) {
            let value = SettingValue::from(at);
            saved.insert(SitePath::new(Place::Global, spec), value, 0);
        }
        saved
    }

    pub fn table<'a>(&self, endpoint: &'a str) -> &'a str {
        crate::endpoints::table_name(endpoint, |name| self.named.contains(name))
    }

    pub fn name_tables<'a>(&mut self, names: impl IntoIterator<Item = &'a str>) {
        self.named = names.into_iter().map(str::to_owned).collect();
    }

    pub fn iter(&self) -> impl Iterator<Item = (&SitePath, &Saved)> {
        self.values.iter()
    }

    pub fn of<'a>(&'a self, name: &'a str) -> impl Iterator<Item = (&'a SitePath, &'a Saved)> {
        self.values
            .iter()
            .filter(move |(path, _)| path.spec().name == name)
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn global(&self, name: &str) -> Option<&SettingValue> {
        self.values
            .iter()
            .find(|(path, _)| path.place == Place::Global && path.spec().name == name)
            .map(|(_, saved)| &saved.value)
    }
}

pub fn global_spec(key: &str) -> Option<&'static SettingSpec> {
    SETTINGS.iter().find(|s| {
        s.is_built()
            && s.has_site(Site::Global)
            && s.group().is_none()
            && s.file_key() == key
            && s.secrecy == Secrecy::Plain
    })
}

pub fn grouped_spec(group: &str, key: &str) -> Option<&'static SettingSpec> {
    SETTINGS.iter().find(|s| {
        s.is_built() && s.has_site(Site::Global) && s.name.split_once('.') == Some((group, key))
    })
}

pub fn is_group(key: &str) -> bool {
    SETTINGS
        .iter()
        .any(|s| s.is_built() && s.has_site(Site::Global) && s.group() == Some(key))
}

pub fn site_spec(site: Site, key: &str) -> Option<&'static SettingSpec> {
    SETTINGS
        .iter()
        .find(|s| s.is_built() && s.has_site(site) && s.leaf() == key)
}

#[derive(Clone, PartialEq, Eq)]
pub struct McpServer {
    pub keys: Vec<String>,
    pub command: String,
    pub args: Vec<String>,
}

impl fmt::Debug for McpServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpServer")
            .field("arguments", &self.args.len())
            .finish_non_exhaustive()
    }
}

pub const MCP_CONFIG_FILE: &str = "the --mcp-config file";
pub(crate) const MCP_SERVERS_RULE: &str = "mcp.servers must hold named tables with a nonempty command and optional lists of args and keys";

pub fn mcp_servers_in(text: &str) -> Result<BTreeMap<String, McpServer>, String> {
    let only_servers = || format!("{MCP_CONFIG_FILE} may hold only [mcp.servers.<name>] tables");
    let root = DeTable::parse(text)
        .map_err(|e| not_toml(MCP_CONFIG_FILE, e.span().map(|span| line_of(text, span))))?;
    let table = root.get_ref();
    if table.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mcp = table.get("mcp").ok_or_else(only_servers)?.get_ref();
    if table.len() != 1
        || mcp
            .as_table()
            .is_none_or(|table| table.keys().any(|key| key.get_ref() != "servers"))
    {
        return Err(only_servers());
    }
    mcp_servers(mcp).ok_or_else(|| format!("{MCP_CONFIG_FILE}: {MCP_SERVERS_RULE}"))
}

pub(crate) fn mcp_servers(value: &DeValue<'_>) -> Option<BTreeMap<String, McpServer>> {
    let table = value.as_table()?;
    if table.len() > 1 {
        return None;
    }
    let Some(servers) = table.get("servers") else {
        return table.is_empty().then(BTreeMap::new);
    };
    servers
        .get_ref()
        .as_table()?
        .iter()
        .map(|(name, value)| {
            let name: &str = name.get_ref();
            if name.is_empty() || name.chars().any(char::is_control) {
                return None;
            }
            let fields = value.get_ref().as_table()?;
            if fields
                .keys()
                .any(|key| !matches!(&**key.get_ref(), "command" | "args" | "keys"))
            {
                return None;
            }
            let command = fields.get("command")?.get_ref().as_str()?.to_owned();
            if command.trim().is_empty() || command.contains('\0') {
                return None;
            }
            let args = match fields.get("args") {
                Some(value) => strings(value.get_ref())?,
                None => Vec::new(),
            };
            if args.iter().any(|arg| arg.contains('\0')) {
                return None;
            }
            let keys = match fields.get("keys") {
                Some(value) => strings(value.get_ref())?,
                None => Vec::new(),
            };
            if keys.iter().any(|key| !wisp_protocol::plain_variable(key)) {
                return None;
            }
            Some((
                name.to_owned(),
                McpServer {
                    command,
                    args,
                    keys,
                },
            ))
        })
        .collect()
}

pub const TABLES: [&str; 4] = ["endpoints", "models", "plugins", "mcp"];

pub fn top_keys() -> Vec<&'static str> {
    let mut keys: BTreeSet<&'static str> = TABLES.into_iter().collect();
    for spec in SETTINGS
        .iter()
        .filter(|s| s.is_built() && s.has_site(Site::Global))
    {
        keys.insert(spec.group().unwrap_or(spec.file_key()));
    }
    keys.into_iter().collect()
}

pub fn top_fields() -> String {
    top_keys().join(", ")
}

pub fn endpoint_keys() -> Vec<&'static str> {
    let mut keys: BTreeSet<&'static str> = BTreeSet::from(["mark", "models"]);
    for spec in SETTINGS
        .iter()
        .filter(|s| s.is_built() && s.has_site(Site::Endpoint))
    {
        if !SECRET_FIELDS.contains(&spec.name) {
            keys.insert(spec.leaf());
        }
    }
    keys.into_iter().collect()
}

pub fn endpoint_fields() -> String {
    endpoint_keys().join(", ")
}

pub const GROUP_TABLES: [(&str, &str); 1] = [(AGENTS_GROUP, PRESETS_TABLE)];

pub fn group_keys(group: &str) -> Vec<&'static str> {
    let mut keys: Vec<&'static str> = SETTINGS
        .iter()
        .filter(|s| s.is_built() && s.has_site(Site::Global) && s.group() == Some(group))
        .filter_map(|s| s.name.split_once('.').map(|(_, key)| key))
        .chain(
            GROUP_TABLES
                .iter()
                .filter(|(owner, _)| *owner == group)
                .map(|(_, table)| *table),
        )
        .collect();
    keys.sort_unstable();
    keys
}

pub fn group_fields(group: &str) -> String {
    group_keys(group).join(", ")
}

pub(crate) fn integer(value: &DeValue<'_>) -> Option<i128> {
    let int = value.as_integer()?;
    i128::from_str_radix(int.as_str(), int.radix()).ok()
}

pub(crate) fn strings(value: &DeValue<'_>) -> Option<Vec<String>> {
    value
        .as_array()?
        .iter()
        .map(|item| item.get_ref().as_str().map(str::to_owned))
        .collect()
}

pub fn from_toml(spec: &SettingSpec, value: &DeValue<'_>) -> Option<SettingValue> {
    match spec.kind {
        Kind::Bool => value.as_bool().map(SettingValue::Bool),
        Kind::Int { none, .. } if none && value.as_str() == Some("none") => {
            Some(SettingValue::Text("none".to_owned()))
        }
        Kind::Int { min, max, .. } => integer(value)
            .and_then(|n| i64::try_from(n).ok())
            .filter(|n| (min..=max).contains(n))
            .map(SettingValue::Int),
        Kind::Tokens { min, max } => {
            let n = match value {
                DeValue::Integer(_) => integer(value).and_then(|n| u64::try_from(n).ok()),
                DeValue::String(text) => parse_tokens(text),
                _ => None,
            };
            n.filter(|n| (min..=max).contains(n))
                .map(SettingValue::Tokens)
        }
        Kind::TokensOrPercent => match value {
            DeValue::Integer(_) => integer(value)
                .and_then(|n| u64::try_from(n).ok())
                .map(SettingValue::Tokens),
            DeValue::String(text) => TokensOrPercent::parse(text).map(SettingValue::from),
            _ => None,
        },
        Kind::Usd { none } => match value {
            DeValue::String(text) if none && text.as_ref() == "none" => {
                Some(SettingValue::Text("none".to_owned()))
            }
            _ => usd_micros(value).map(|micros| SettingValue::Usd { micros }),
        },
        Kind::Preload => match value.as_str() {
            Some(text) => spec.parse(text).ok(),
            None => strings(value).and_then(|items| spec.parse_list(&items).ok()),
        },
        Kind::List(_) | Kind::ModelIds | Kind::ToolNames | Kind::Names | Kind::SectionNames => {
            match value {
                DeValue::String(text) if text.as_ref() == "none" => {
                    Some(SettingValue::List(vec![]))
                }
                _ => strings(value).and_then(|items| spec.parse_list(&items).ok()),
            }
        }
        Kind::Choice(_)
        | Kind::ModelId
        | Kind::EndpointName
        | Kind::AgentName
        | Kind::VarName
        | Kind::VoiceName
        | Kind::Language
        | Kind::ClientVersion
        | Kind::Path
        | Kind::PluginId => value.as_str().and_then(|text| spec.parse(text).ok()),
        Kind::Secret => None,
    }
}

pub fn usd_micros(value: &DeValue<'_>) -> Option<u64> {
    match value {
        DeValue::String(text) => parse_usd(text),
        DeValue::Integer(_) => integer(value)
            .and_then(|n| u64::try_from(n).ok())
            .and_then(|n| n.checked_mul(wisp_protocol::settings::MICROS_PER_USD)),
        DeValue::Float(f) => parse_usd(f.as_str()),
        _ => None,
    }
}

pub fn row_value(row: &ModelRow, field: &str) -> Option<SettingValue> {
    match field {
        "context_window" => row.context_window.map(SettingValue::Tokens),
        "compact_at" => row.compact_at.map(SettingValue::from),
        "native_compaction" => row.native_compaction.map(SettingValue::Bool),
        "background_compaction" => row.background_compaction.map(SettingValue::Bool),
        "background_tokens" => row.background_tokens.map(SettingValue::Tokens),
        "web_search" => row.web_search.clone().map(SettingValue::List),
        "aliases" => Some(SettingValue::List(row.aliases.clone())),
        "max_output" => row.max_output.map(|n| SettingValue::Tokens(u64::from(n))),
        "price_input" => row.price_input.map(|micros| SettingValue::Usd { micros }),
        "price_output" => row.price_output.map(|micros| SettingValue::Usd { micros }),
        "price_cache_read" => row
            .price_cache_read
            .map(|micros| SettingValue::Usd { micros }),
        _ => None,
    }
}

pub fn set_row_value(row: &mut ModelRow, field: &str, value: &SettingValue) {
    match (field, value) {
        ("context_window", SettingValue::Tokens(tokens)) => row.context_window = Some(*tokens),
        ("compact_at", value) => row.compact_at = CompactAt::of(value).or(row.compact_at),
        ("native_compaction", SettingValue::Bool(on)) => row.native_compaction = Some(*on),
        ("background_compaction", SettingValue::Bool(on)) => {
            row.background_compaction = Some(*on);
        }
        ("background_tokens", SettingValue::Tokens(tokens)) => {
            row.background_tokens = Some(*tokens);
        }
        ("web_search", SettingValue::List(versions)) => row.web_search = Some(versions.clone()),
        ("aliases", SettingValue::List(aliases)) => row.aliases.clone_from(aliases),
        ("max_output", SettingValue::Tokens(tokens)) => {
            row.max_output = u32::try_from(*tokens).ok();
        }
        ("price_input", SettingValue::Usd { micros }) => row.price_input = Some(*micros),
        ("price_output", SettingValue::Usd { micros }) => row.price_output = Some(*micros),
        ("price_cache_read", SettingValue::Usd { micros }) => row.price_cache_read = Some(*micros),
        _ => {}
    }
}

pub fn expects_string(spec: &SettingSpec) -> bool {
    matches!(
        spec.kind,
        Kind::Choice(_)
            | Kind::ModelId
            | Kind::EndpointName
            | Kind::AgentName
            | Kind::VarName
            | Kind::VoiceName
            | Kind::Language
            | Kind::ClientVersion
            | Kind::Path
            | Kind::PluginId
    )
}

#[cfg(test)]
mod tests;
