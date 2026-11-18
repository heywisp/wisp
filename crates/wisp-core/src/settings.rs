use std::collections::{BTreeMap, BTreeSet};

use wisp_protocol::settings::{
    CredentialKind, Kind, Place, SettingSpec, SettingValue, Site, SitePath, Status, Target, all,
    names,
};

use crate::config::{SavedSettings, shown_value};
use crate::endpoints::ProviderKind;
use crate::models::{ModelRow, ModelTable};

pub mod host;
pub mod remembered;

pub(crate) fn on(value: Option<&SettingValue>) -> Option<bool> {
    match value {
        Some(SettingValue::Bool(on)) => Some(*on),
        _ => None,
    }
}

pub(crate) fn text(value: Option<&SettingValue>) -> Option<&str> {
    match value {
        Some(SettingValue::Text(text)) => Some(text.as_str()),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layer {
    Forced,
    Session,
    Flag,
    Environment,
    Mode,
    Agent,
    EndpointModel,
    Model,
    Endpoint,
    Global,
    Shipped,
    Default,
}

impl Layer {
    pub fn wire(self) -> wisp_protocol::settings::Layer {
        use wisp_protocol::settings::Layer as Wire;
        match self {
            Self::Forced => Wire::Forced,
            Self::Session => Wire::Session,
            Self::Flag => Wire::Flag,
            Self::Environment => Wire::Env,
            Self::Mode => Wire::Mode,
            Self::Agent => Wire::Agent,
            Self::EndpointModel => Wire::EndpointModel,
            Self::Model => Wire::Model,
            Self::Endpoint => Wire::Endpoint,
            Self::Global => Wire::Config,
            Self::Shipped => Wire::Shipped,
            Self::Default => Wire::Default,
        }
    }

    pub fn source(self) -> &'static str {
        match self {
            Self::Forced => "forced",
            Self::Session => "session",
            Self::Flag => "flag",
            Self::Environment => "variable",
            Self::Mode => "mode",
            Self::Agent => "agent",
            Self::EndpointModel | Self::Global => "config",
            Self::Model => "model",
            Self::Endpoint => "account",
            Self::Shipped => "shipped",
            Self::Default => "default",
        }
    }
}

pub const START_ORIGIN: &str = "start";

#[derive(Clone, PartialEq, Eq)]
pub struct Given {
    pub target: Target,
    pub value: SettingValue,
    pub origin: String,
}

impl std::fmt::Debug for Given {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Given")
            .field("target", &self.target)
            .field("value", &shown_value(self.target.spec(), &self.value))
            .field("origin", &self.origin)
            .finish()
    }
}

fn at_endpoint(current: &Current, given: &str, saved: Option<&SavedSettings>) -> bool {
    current.endpoint.as_deref().is_some_and(|name| {
        let table = match saved {
            Some(saved) => saved.table(name),
            None => crate::endpoints::table_name(name, |_| false),
        };
        name == given || table == given
    })
}

impl Given {
    fn applies(
        &self,
        current: &Current,
        aliases: &[String],
        saved: Option<&SavedSettings>,
    ) -> bool {
        let names = |model: &str| ModelTable::shared().names_model(model, aliases, &current.model);
        match &self.target {
            Target::Name(_) => true,
            Target::Path(path) => match &path.place {
                Place::Global => true,
                Place::Endpoint(e) => at_endpoint(current, e, saved),
                Place::Model(m) => names(m),
                Place::EndpointModel { endpoint, model } => {
                    at_endpoint(current, endpoint, saved) && names(model)
                }
                Place::Agent(_)
                | Place::PluginHook { .. }
                | Place::PluginSlot { .. }
                | Place::Plugin(_) => false,
            },
        }
    }

    fn model(&self) -> Option<&str> {
        match &self.target {
            Target::Path(SitePath {
                place: Place::Model(m) | Place::EndpointModel { model: m, .. },
                ..
            }) => Some(m),
            _ => None,
        }
    }
}

fn aliases_given(list: &[Given], model: &str) -> Vec<String> {
    list.iter()
        .filter(|g| {
            g.target.spec().name == names::ALIASES
                && matches!(&g.target, Target::Path(p) if p.place == Place::Model(model.to_owned()))
        })
        .filter_map(|g| match &g.value {
            SettingValue::List(aliases) => Some(aliases.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

#[derive(Debug, Clone, Default)]
pub struct Layers<'a> {
    pub session: Vec<Given>,
    pub flags: Vec<Given>,
    pub env: Vec<Given>,
    pub saved: Option<&'a SavedSettings>,
    pub shipped: Option<&'a ModelTable>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Current {
    pub endpoint: Option<String>,
    pub model: String,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Found {
    pub layer: Layer,
    pub value: SettingValue,
    pub origin: String,
    pub line: Option<usize>,
}

impl std::fmt::Debug for Found {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Found")
            .field("layer", &self.layer)
            .field("value", &self.value.kind_name())
            .field("origin", &self.origin)
            .field("line", &self.line)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub spec: &'static SettingSpec,
    pub found: Vec<Found>,
}

impl Resolution {
    pub fn winner(&self) -> Option<&Found> {
        self.found.first()
    }

    pub fn shadowed(&self) -> &[Found] {
        self.found.get(1..).unwrap_or_default()
    }

    pub fn value(&self) -> Option<&SettingValue> {
        self.winner().map(|f| &f.value)
    }

    pub fn source(&self) -> &'static str {
        self.winner()
            .map_or(Layer::Default.source(), |f| f.layer.source())
    }

    #[must_use]
    pub fn shown_for_keys_file(mut self, keys_file: Option<&str>) -> Self {
        if self.spec.kind == Kind::EndpointName {
            for found in &mut self.found {
                crate::config::shown_endpoint(&mut found.value, keys_file);
            }
        }
        self
    }

    #[must_use]
    pub fn on_wire(mut self, wire: Option<ProviderKind>) -> Self {
        if let (names::WEB_SEARCH, Some(wire)) = (self.spec.name, wire) {
            let lists = self.found.iter().map(|f| match &f.value {
                SettingValue::List(list) => list.as_slice(),
                _ => &[],
            });
            let at = crate::search::deciding(wire, lists);
            if let Some(head) = self.found.get_mut(..=at) {
                head.rotate_right(1);
            }
        }
        self
    }
}

fn shipped_value(spec: &SettingSpec, row: &ModelRow) -> Option<SettingValue> {
    match spec.name {
        names::CONTEXT_WINDOW => row.context_window.map(SettingValue::Tokens),
        names::NATIVE_COMPACTION => row.native_compaction.map(SettingValue::Bool),
        names::WEB_SEARCH => row.web_search.clone().map(SettingValue::List),
        names::ALIASES if !row.aliases.is_empty() => Some(SettingValue::List(row.aliases.clone())),
        _ => None,
    }
}

fn specificity(target: &Target) -> u8 {
    match target {
        Target::Path(path) => match path.place {
            Place::EndpointModel { .. } => 0,
            Place::Model(_) => 1,
            Place::Endpoint(_) => 2,
            Place::Global
            | Place::Agent(_)
            | Place::PluginHook { .. }
            | Place::PluginSlot { .. }
            | Place::Plugin(_) => 3,
        },
        Target::Name(_) => 3,
    }
}

fn family(saved: Option<&SavedSettings>, flags: &[Given], name: &str) -> Vec<String> {
    let mut ids: Vec<String> = vec![name.to_owned()];
    let saved_ids = saved
        .into_iter()
        .flat_map(|s| s.iter())
        .filter_map(|(path, _)| match &path.place {
            Place::Model(id) => Some(id.as_str()),
            _ => None,
        });
    let flag_ids = flags.iter().filter_map(|g| match &g.target {
        Target::Path(SitePath {
            place: Place::Model(id),
            ..
        }) => Some(id.as_str()),
        _ => None,
    });
    let mut seen = BTreeSet::new();
    let owners: Vec<&str> = saved_ids
        .chain(flag_ids)
        .filter(|id| seen.insert(*id))
        .filter(|id| {
            *id == name
                || saved_aliases(saved, id).iter().any(|a| a == name)
                || aliases_given(flags, id).iter().any(|a| a == name)
        })
        .collect();
    for owner in owners {
        let linked = saved_aliases(saved, owner)
            .into_iter()
            .chain(aliases_given(flags, owner));
        for one in std::iter::once(owner.to_owned()).chain(linked) {
            if !ids.contains(&one) {
                ids.push(one);
            }
        }
    }
    ids
}

fn model_names(saved: &SavedSettings, flags: &[Given], model: &str) -> Vec<String> {
    let mut ids: Vec<String> = vec![model.to_owned()];
    let mut seen = BTreeSet::from([model]);
    for (path, _) in saved.iter() {
        let (Place::Model(id) | Place::EndpointModel { model: id, .. }) = &path.place else {
            continue;
        };
        if seen.insert(id.as_str())
            && ModelTable::shared().names_model(id, &family(Some(saved), flags, id), model)
        {
            ids.push(id.clone());
        }
    }
    ids
}

fn saved_aliases(saved: Option<&SavedSettings>, model: &str) -> Vec<String> {
    #[cfg(test)]
    tests::SAVED_ALIAS_CALLS.set(tests::SAVED_ALIAS_CALLS.get() + 1);
    let Some(spec) = wisp_protocol::settings::find(names::ALIASES) else {
        return Vec::new();
    };
    match saved
        .and_then(|s| s.get(&SitePath::new(Place::Model(model.to_owned()), spec)))
        .map(|entry| &entry.value)
    {
        Some(SettingValue::List(aliases)) => aliases.clone(),
        _ => Vec::new(),
    }
}

fn given(
    layer: Layer,
    list: &[Given],
    spec: &SettingSpec,
    current: &Current,
    saved: Option<&SavedSettings>,
) -> Vec<Found> {
    let aliases = |model: &str| family(saved, list, model);
    let mut list: Vec<&Given> = list
        .iter()
        .filter(|g| {
            g.target.spec().name == spec.name
                && g.applies(current, &g.model().map(aliases).unwrap_or_default(), saved)
        })
        .collect();
    list.sort_by_key(|g| specificity(&g.target));
    list.into_iter()
        .map(|g| Found {
            layer,
            value: g.value.clone(),
            origin: g.origin.clone(),
            line: None,
        })
        .collect()
}

pub fn resolve_one(
    spec: &'static SettingSpec,
    layers: &Layers<'_>,
    current: &Current,
) -> Resolution {
    let mut found = Vec::new();
    if spec.status != Status::Built {
        return Resolution { spec, found };
    }
    found.extend(given(
        Layer::Session,
        &layers.session,
        spec,
        current,
        layers.saved,
    ));
    found.extend(given(
        Layer::Flag,
        &layers.flags,
        spec,
        current,
        layers.saved,
    ));
    found.extend(given(
        Layer::Environment,
        &layers.env,
        spec,
        current,
        layers.saved,
    ));
    if let Some(saved) = layers.saved {
        let ids = model_names(saved, &layers.flags, &current.model);
        let mut places: Vec<(Layer, Vec<Place>)> = Vec::new();
        if let Some(endpoint) = &current.endpoint {
            places.push((
                Layer::EndpointModel,
                ids.iter()
                    .map(|model| Place::EndpointModel {
                        endpoint: endpoint.clone(),
                        model: model.clone(),
                    })
                    .collect(),
            ));
        }
        places.push((
            Layer::Model,
            ids.iter()
                .map(|model| Place::Model(model.clone()))
                .collect(),
        ));
        if let Some(endpoint) = &current.endpoint {
            places.push((Layer::Endpoint, vec![Place::Endpoint(endpoint.clone())]));
        }
        places.push((Layer::Global, vec![Place::Global]));
        for (layer, candidates) in places {
            let hit = candidates
                .into_iter()
                .filter(|place| spec.has_site(place.site()))
                .map(|place| SitePath::new(place, spec))
                .find_map(|path| saved.get(&path).map(|entry| (path, entry)));
            if let Some((path, entry)) = hit {
                found.push(Found {
                    layer,
                    value: entry.value.clone(),
                    origin: path.to_string(),
                    line: Some(entry.line),
                });
            }
        }
    }
    if spec.has_site(Site::Shipped)
        && let Some(row) = layers.shipped.and_then(|t| t.find(&current.model))
        && let Some(value) = shipped_value(spec, row)
    {
        found.push(Found {
            layer: Layer::Shipped,
            value,
            origin: wisp_protocol::settings::shipped_origin(&row.id),
            line: None,
        });
    }
    if let Some(value) = spec.default_value() {
        found.push(Found {
            layer: Layer::Default,
            value,
            origin: Layer::Default.source().to_owned(),
            line: None,
        });
    }
    Resolution { spec, found }
}

pub type Resolved = BTreeMap<&'static str, Resolution>;

pub fn background_band(layers: &Layers<'_>, current: &Current) -> Option<u64> {
    let value = |name: &str| {
        wisp_protocol::settings::find(name)
            .and_then(|spec| resolve_one(spec, layers, current).value().cloned())
    };
    let off = |name: &str| matches!(value(name), Some(SettingValue::Bool(false)));
    if off(names::CONTEXT_BACKGROUND_COMPACTION) || off(names::CONTEXT_AUTO_COMPACT) {
        return None;
    }
    match value(names::CONTEXT_BACKGROUND_TOKENS) {
        Some(SettingValue::Tokens(tokens)) if tokens > 0 => Some(tokens),
        _ => Some(crate::context::BACKGROUND_TOKENS),
    }
}

fn model_provider(id: &str) -> Option<String> {
    crate::endpoints::goes::model_provider(id).map(|p| p.as_str().to_owned())
}

pub fn shipped_models() -> Vec<wisp_protocol::settings::ModelEntry> {
    let table = crate::models::ModelTable::shipped().ok();
    crate::pricing::model_ids()
        .map(|id| {
            let row = table.as_ref().and_then(|t| t.find(id));
            wisp_protocol::settings::ModelEntry {
                id: id.to_owned(),
                shipped: true,
                configured: false,
                context_window: row.and_then(|r| r.context_window),
                listed: crate::models::ModelTable::shared().listed_rank(id),
                provider: model_provider(id),
            }
        })
        .collect()
}

pub struct EndpointRow {
    pub value: String,
    pub layer: wisp_protocol::settings::Layer,
    pub origin: String,
    pub keys_file: Option<String>,
    pub effort: wisp_protocol::Effort,
    pub credential: Option<CredentialKind>,
    pub wire: Option<ProviderKind>,
}

pub fn snapshot_rows(
    layers: &Layers<'_>,
    current: &Current,
    full_screen: bool,
    endpoint: &EndpointRow,
    choices: &dyn Fn(&'static SettingSpec) -> Vec<String>,
) -> Vec<wisp_protocol::settings::ResolvedSetting> {
    resolve(layers, current)
        .into_values()
        .filter(|r| {
            r.spec.secrecy == wisp_protocol::settings::Secrecy::Plain && !r.spec.per_plugin()
        })
        .map(|r| {
            r.shown_for_keys_file(endpoint.keys_file.as_deref())
                .on_wire(endpoint.wire)
        })
        .map(|r| {
            let spec = r.spec;
            let mut value = r.value().cloned();
            let mut layer = r.winner().map_or(Layer::Default, |f| f.layer).wire();
            let mut origin = r
                .winner()
                .map_or_else(|| spec.default_words(), |f| f.origin.clone());
            if spec.name == names::ENDPOINT && value.is_none() {
                value = Some(SettingValue::Text(endpoint.value.clone()));
                layer = endpoint.layer.clone();
                origin = endpoint.origin.clone();
            }
            if value.is_none() {
                value = spec.default_for(endpoint.credential.as_ref());
            }
            if spec.name == names::EFFORT && r.winner().is_none_or(|f| f.layer == Layer::Default) {
                value = Some(SettingValue::Text(endpoint.effort.as_str().to_owned()));
            }
            wisp_protocol::settings::ResolvedSetting {
                name: wisp_protocol::settings::SettingName::of(spec),
                value,
                layer,
                origin,
                shadowed: r
                    .shadowed()
                    .iter()
                    .map(|f| wisp_protocol::settings::Shadowed {
                        layer: f.layer.wire(),
                        value: f.value.clone(),
                        origin: f.origin.clone(),
                    })
                    .collect(),
                choices: choices(spec),
                applies: full_screen || spec.applies_to == wisp_protocol::settings::AppliesTo::All,
            }
        })
        .collect()
}

pub fn resolve(layers: &Layers<'_>, current: &Current) -> Resolved {
    all()
        .filter(|s| s.status == Status::Built)
        .map(|spec| (spec.name, resolve_one(spec, layers, current)))
        .collect()
}

pub fn path_value(
    session: &[Given],
    flags: &[Given],
    env: &[Given],
    saved: &SavedSettings,
    path: &SitePath,
) -> Option<(SettingValue, Layer)> {
    let exact = |list: &[Given]| {
        list.iter()
            .find(|g| matches!(&g.target, Target::Path(p) if p == path))
            .map(|g| g.value.clone())
    };
    exact(session)
        .map(|v| (v, Layer::Session))
        .or_else(|| exact(flags).map(|v| (v, Layer::Flag)))
        .or_else(|| exact(env).map(|v| (v, Layer::Environment)))
        .or_else(|| saved.get(path).map(|s| (s.value.clone(), Layer::Global)))
}

pub fn is_off(value: &SettingValue) -> bool {
    match value {
        SettingValue::Bool(on) => !on,
        SettingValue::Text(text) => text == "off",
        _ => false,
    }
}

pub fn start_switches(flags: &[Given], saved: &SavedSettings) -> crate::plugin::PluginSwitches {
    let ids: std::collections::BTreeSet<String> = flags
        .iter()
        .filter_map(|g| match &g.target {
            Target::Path(path) => Some(path),
            Target::Name(_) => None,
        })
        .chain(saved.iter().map(|(path, _)| path))
        .filter_map(|path| match &path.place {
            Place::Plugin(id) => Some(id.clone()),
            _ => None,
        })
        .collect();
    crate::plugin::PluginSwitches::placed(ids, |id, key| {
        SitePath::plugin_switch(id, key)
            .and_then(|path| path_value(&[], flags, &[], saved, &path))
            .is_none_or(|(value, _)| !is_off(&value))
    })
}

pub fn saves_as_default(
    catalog: &wisp_protocol::Catalog,
    names: &[String],
    saved: &SavedSettings,
) -> bool {
    let switches = start_switches(&[], saved);
    catalog.is_default(names, |id| switches.skips(id))
}

#[cfg(test)]
mod tests;
