use std::sync::Arc;

use tokio::sync::mpsc;
use wisp_plugin_api::config::AccountRow;
use wisp_protocol::settings::{
    AppliedChange, Applies, By, Chat, Dynamic, FileState, Form, Kind, LoggedChange, ModelEntry,
    Overridden, OverrideScope, Owner, Place, Remembered, ResolvedSetting, Saved, SavedValue,
    Secrecy, SettingChange, SettingSpec, SettingTarget, SettingValue, SettingsSnapshot, Site,
    SitePath, SnapshotContext, SwitchTarget, Target, When, names, offered, offers,
};
use wisp_protocol::subscription_auth::Way;
use wisp_protocol::{
    AgentEvent, CacheEffect, Effort, PluginEntry, RebuildReason, RequestId, SearchForm, shown_model,
};

use super::Pending;
use super::kernel::Applied;
use crate::agent::Agent;
use crate::agents::endpoints;
use crate::config::SavedSettings;
use crate::config_file::SiteEdit;
use crate::endpoints::ProviderKind;
use crate::models::{ModelRow, ModelTable};
use crate::plugin::host::accounts::{self, AccountsView};
use crate::plugin::host::{CallView, MENUS_PLUGIN};
use crate::settings::host::{SavedFile, SavedRead};
use crate::settings::{
    Current, EndpointRow, Given, Layer, Layers, START_ORIGIN, resolve_one, snapshot_rows,
};

const SESSION_ORIGIN: &str = "session";
pub const KERNEL_NAMES: [&str; 27] = [
    names::CACHE_TTL,
    names::CLAUDE_USAGE,
    names::COMPACT_AT,
    names::CONTEXT_AUTO_COMPACT,
    names::CONTEXT_BACKGROUND_COMPACTION,
    names::CONTEXT_BACKGROUND_TOKENS,
    names::CONTEXT_CONTEXT_NOTE,
    names::CONTEXT_COUNT_TOKENS,
    names::CONTEXT_STRATEGY,
    names::CONTEXT_VERBATIM_USER_TOKENS,
    names::CONTEXT_WINDOW,
    names::DEBUG_CACHE,
    names::EFFORT,
    names::ENDPOINT,
    names::INSTRUCTIONS_SOURCE,
    names::INSTRUCTIONS_WARN_TOKENS,
    names::MODEL,
    "plugins.<id>.enabled",
    "plugins.<id>.hooks.<hook>",
    "plugins.<id>.installed",
    names::PRICING,
    names::REFUSAL_FALLBACK,
    names::RETRY_BUDGET_USD,
    names::SELF_CHECK_BEFORE_END,
    names::TIME,
    names::TOOLS,
    names::TURNED_OFF,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Pair,
    Kernel,
    Client,
    NextSession,
}

pub(super) fn is_kernel(spec: &SettingSpec) -> bool {
    spec.is_declared() || KERNEL_NAMES.contains(&spec.name)
}

fn class_of(spec: &SettingSpec) -> Result<Class, String> {
    if spec.applies == Applies::Connection {
        return Err(connection_words(spec));
    }
    if names::PAIR.contains(&spec.name) {
        return Ok(Class::Pair);
    }
    if spec.owner == Owner::Client {
        return Ok(Class::Client);
    }
    if !spec.mid_session {
        return Ok(Class::NextSession);
    }
    if is_kernel(spec) {
        return Ok(Class::Kernel);
    }
    Err(not_yet(spec))
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::agent) struct Projected {
    pub(in crate::agent) endpoint: String,
    pub(in crate::agent) model: String,
}

impl Projected {
    pub(in crate::agent) fn follow(&mut self, changes: &[SettingChange]) {
        for change in changes {
            let (SettingTarget::Name(_), Some(SettingValue::Text(text))) =
                (&change.target, &change.value)
            else {
                continue;
            };
            match change.target.spec().name {
                names::MODEL => self.model.clone_from(text),
                names::ENDPOINT => self.endpoint.clone_from(text),
                _ => {}
            }
        }
    }
}

#[derive(Debug, Clone)]
struct Choice {
    target: SettingTarget,
    spec: &'static SettingSpec,
    value: Option<SettingValue>,
    class: Class,
    named: bool,
    saved: bool,
    place: Place,
}

impl Choice {
    fn path(&self) -> SitePath {
        SitePath::new(self.place.clone(), self.spec)
    }

    fn settles(&self) -> bool {
        self.saved && self.spec.settles_on_save()
    }

    fn session(&self) -> bool {
        self.named && matches!(self.class, Class::Pair | Class::Kernel) && !self.settles()
    }

    fn given_target(&self) -> Target {
        match &self.place {
            Place::Global => Target::Name(self.spec),
            _ => Target::Path(self.path()),
        }
    }

    fn applied_target(&self) -> SettingTarget {
        match (&self.target, &self.place) {
            (SettingTarget::Name(_), Place::Global) => self.target.clone(),
            _ => SettingTarget::Path(self.path()),
        }
    }

    fn switches(&self, target: &Pending, endpoint: usize, model: &str) -> bool {
        self.class == Class::Pair
            && match self.spec.name {
                names::ENDPOINT => target.endpoint != endpoint,
                names::MODEL | names::EFFORT => target.model != model,
                _ => false,
            }
    }

    fn layer(&self) -> Layer {
        if self.named && self.class != Class::NextSession && !self.settles() {
            return Layer::Session;
        }
        match self.place {
            Place::Global
            | Place::PluginHook { .. }
            | Place::PluginSlot { .. }
            | Place::Plugin(_) => Layer::Global,
            Place::Endpoint(_) => Layer::Endpoint,
            Place::Model(_) => Layer::Model,
            Place::EndpointModel { .. } => Layer::EndpointModel,
            Place::Agent(_) => Layer::Agent,
        }
    }
}

fn bind(
    spec: &'static SettingSpec,
    target: &SettingTarget,
    projected: &Projected,
) -> Result<Place, String> {
    let place = match target {
        SettingTarget::Path(path) => path.place.clone(),
        SettingTarget::Name(_) if spec.has_site(Site::Global) => Place::Global,
        SettingTarget::Name(_) if spec.has_site(Site::Model) => {
            Place::Model(projected.model.clone())
        }
        SettingTarget::Name(_) if spec.has_site(Site::Endpoint) => {
            Place::Endpoint(projected.endpoint.clone())
        }
        SettingTarget::Name(_) => return Err(not_yet(spec)),
    };
    if matches!(place, Place::Agent(_)) {
        return Err(not_yet(spec));
    }
    Ok(place)
}

fn connection_words(spec: &SettingSpec) -> String {
    format!(
        "{} is part of how Wisp connects to an account; change it with wisp account edit",
        spec.name
    )
}

#[derive(Debug, Clone)]
pub(in crate::agent) struct Verdict {
    target: Pending,
    binding: Option<crate::endpoints::generation::Binding>,
    choices: Vec<Choice>,
    pub(in crate::agent) edits: Vec<SiteEdit>,
    pub(in crate::agent) by: Option<By>,
}

#[derive(Debug)]
pub(in crate::agent) struct Refusal {
    pub(in crate::agent) line: String,
    pub(in crate::agent) ways: Vec<Way>,
}

impl From<String> for Refusal {
    fn from(line: String) -> Self {
        Self {
            line,
            ways: Vec::new(),
        }
    }
}

pub fn not_mid_session(spec: &SettingSpec) -> String {
    let when = spec.applies.words();
    let applies = if spec.applies == Applies::NextSession {
        format!("to {when}")
    } else {
        format!("from the {when}")
    };
    format!("{} applies {applies}, so it can only be saved.", spec.name)
}

fn twice_words(target: &SettingTarget) -> String {
    format!("{target} appears twice in one change; send it once")
}

pub fn not_yet(spec: &SettingSpec) -> String {
    format!(
        "{} can't be changed in a running session yet; use wisp config set {}",
        spec.name, spec.name
    )
}

fn canonical(value: &SettingValue) -> SettingValue {
    match value {
        SettingValue::List(items) => {
            let mut items = items.clone();
            items.sort();
            items.dedup();
            SettingValue::List(items)
        }
        other => other.clone(),
    }
}

pub(crate) fn checked(spec: &SettingSpec, value: &SettingValue) -> Result<SettingValue, String> {
    match (spec.kind, value) {
        (Kind::ModelId, SettingValue::Text(id)) => {
            super::check_model_id(id)?;
            return Ok(value.clone());
        }
        (Kind::EndpointName, SettingValue::Text(_)) => return Ok(value.clone()),
        _ => {}
    }
    let parsed = match (spec.kind, value) {
        (Kind::SectionNames | Kind::Names, SettingValue::List(items)) => spec.parse_list(items),
        _ => spec.parse(&value.text()),
    }
    .map_err(|e| format!("{} {}", spec.name, e.rule))?;
    if parsed == *value || parsed == canonical(value) {
        Ok(parsed)
    } else {
        Err(format!("{} {}", spec.name, spec.describe(Form::Rule)))
    }
}

fn same_site(a: &Target, b: &Target) -> bool {
    let place = |t: &Target| match t {
        Target::Name(_) => Place::Global,
        Target::Path(path) => path.place.clone(),
    };
    a.spec().name == b.spec().name && place(a) == place(b)
}

fn set_given(list: &mut Vec<Given>, target: Target, value: Option<SettingValue>) {
    let every_place = matches!(target, Target::Name(_));
    list.retain(|g| {
        !same_site(&g.target, &target)
            && !(every_place && g.target.spec().name == target.spec().name)
    });
    if let Some(value) = value {
        list.push(Given {
            target,
            value,
            origin: SESSION_ORIGIN.to_owned(),
        });
    }
    list.sort_by_key(|g| {
        let place = match &g.target {
            Target::Name(_) => String::new(),
            Target::Path(path) => path.to_string(),
        };
        (g.target.spec().name, place)
    });
}

fn text_of(value: Option<&SettingValue>) -> Option<String> {
    value.map(SettingValue::text)
}

impl Agent {
    pub(in crate::agent) fn start_given(&self) -> Vec<Given> {
        let mut flags = Vec::new();
        for (name, value) in [
            (names::MODEL, self.session.model.clone()),
            (names::EFFORT, self.session.effort.as_str().to_owned()),
        ] {
            if let Some(spec) = wisp_protocol::settings::find(name) {
                flags.push(Given {
                    target: Target::Name(spec),
                    value: SettingValue::Text(value),
                    origin: START_ORIGIN.to_owned(),
                });
            }
        }
        flags
    }

    pub(in crate::agent) fn explicit_flags(&self) -> impl Iterator<Item = &Given> {
        let root = self.parent.is_none();
        self.settings
            .layers
            .flags
            .iter()
            .filter(move |g| root && g.origin != START_ORIGIN)
    }

    pub(in crate::agent) fn restore_session(
        &mut self,
        target: &SettingTarget,
        value: Option<SettingValue>,
    ) -> bool {
        let target = match target {
            SettingTarget::Name(name) => Target::Name(name.spec()),
            SettingTarget::Path(path) => Target::Path(path.clone()),
        };
        let name = target.spec().name;
        let flagged = self.explicit_flags().any(|g| {
            same_site(&g.target, &target)
                || matches!(&g.target, Target::Name(spec) if spec.name == name)
        });
        if !flagged {
            set_given(&mut self.settings.session, target, value);
        }
        !flagged
    }

    pub(in crate::agent) fn session_pair(&self) -> Pending {
        Pending {
            endpoint: self.endpoint,
            model: self.session.model.clone(),
            effort: self.session.effort,
        }
    }

    pub(in crate::agent) fn next_pair(&self) -> Pending {
        self.settings
            .pending
            .clone()
            .unwrap_or_else(|| self.session_pair())
    }

    pub(in crate::agent) fn next_names(&self) -> (String, String) {
        let pair = self.next_pair();
        (self.connection_name(pair.endpoint).to_owned(), pair.model)
    }

    pub(in crate::agent) fn projected_by(&self, verdict: &Verdict) -> Projected {
        Projected {
            endpoint: self.connection_name(verdict.target.endpoint).to_owned(),
            model: verdict.target.model.clone(),
        }
    }

    pub(in crate::agent) fn projected(&self) -> Projected {
        let (endpoint, model) = self.next_names();
        let mut projected = Projected { endpoint, model };
        if let Some(flight) = self.queue.flight() {
            projected.clone_from(flight);
        }
        for changes in self.queue.waiting_changes() {
            projected.follow(changes);
        }
        projected
    }

    fn current_for(&self, pair: &Pending) -> Current {
        Current {
            endpoint: Some(self.connection_name(pair.endpoint).to_owned()),
            model: pair.model.clone(),
        }
    }

    fn effective(
        &self,
        spec: &'static SettingSpec,
        session: &[Given],
        saved: &SavedSettings,
        current: &Current,
    ) -> Option<SettingValue> {
        let layers = Layers {
            session: session.to_vec(),
            flags: self.settings.layers.flags.clone(),
            env: self.settings.layers.env.clone(),
            saved: Some(saved),
            shipped: None,
        };
        resolve_one(spec, &layers, current).value().cloned()
    }

    fn window_floor(&self, choice: &Choice) -> Result<(), String> {
        let (Some(SettingValue::Tokens(window)), names::CONTEXT_WINDOW) =
            (&choice.value, choice.spec.name)
        else {
            return Ok(());
        };
        let model = match &choice.place {
            Place::Model(model) | Place::EndpointModel { model, .. } => model.clone(),
            _ => return Ok(()),
        };
        let mut pair = self.next_pair();
        pair.model.clone_from(&model);
        let max_tokens = self.max_tokens_on(pair.endpoint, &pair.model);
        crate::context::check_limit(&model, *window, max_tokens).map_err(|e| e.to_string())
    }

    pub(in crate::agent) fn verdict(
        &self,
        changes: &[SettingChange],
        save: bool,
        projected: &Projected,
    ) -> Result<Verdict, Refusal> {
        let (verdict, check_model, effort) = self.proposed_verdict(changes, save, projected)?;
        self.checked_verdict(verdict, check_model, effort)
    }

    pub(in crate::agent) async fn refreshed_verdict(
        &mut self,
        changes: &[SettingChange],
        save: bool,
        projected: &Projected,
    ) -> Result<Verdict, Refusal> {
        let (verdict, check_model, effort) = self.proposed_verdict(changes, save, projected)?;
        let target = &verdict.target;
        let before = self.next_pair();
        if let Some(catalog) = &self.endpoint_catalog
            && (target.model != before.model || target.endpoint != before.endpoint)
            && catalog
                .refetch_unlisted(self.connection_name(target.endpoint), &target.model)
                .await
        {
            return self.verdict(changes, save, projected);
        }
        self.checked_verdict(verdict, check_model, effort)
    }

    fn proposed_verdict(
        &self,
        changes: &[SettingChange],
        save: bool,
        projected: &Projected,
    ) -> Result<(Verdict, bool, Option<Effort>), Refusal> {
        let base = self.next_pair();
        let current = self.current_for(&base);
        let mut session = self.settings.session.clone();
        let mut saved = self.settings.layers.saved.clone();
        let mut choices = Vec::new();
        let mut edits = Vec::new();
        if let Some(twice) = changes.iter().enumerate().find_map(|(i, c)| {
            changes[..i]
                .iter()
                .any(|e| e.target == c.target)
                .then_some(c)
        }) {
            return Err(twice_words(&twice.target).into());
        }
        for change in changes {
            let spec = change.target.spec();
            if !spec.is_built() || spec.secrecy != Secrecy::Plain {
                return Err(
                    format!("{} can't be changed with a settings change", spec.name).into(),
                );
            }
            let class = class_of(spec)?;
            let place = bind(spec, &change.target, projected)?;
            if class == Class::Pair && place != Place::Global {
                return Err(not_yet(spec).into());
            }
            if class == Class::NextSession && !save {
                return Err(not_mid_session(spec).into());
            }
            let value = match &change.value {
                Some(value) => Some(checked(spec, value)?),
                None => None,
            };
            if let (Kind::Preload, Some(SettingValue::List(names))) = (spec.kind, &value) {
                self.catalog()
                    .checked_preload(names)
                    .map_err(|error| format!("{}: {error}", spec.name))?;
            }
            if let (Kind::ToolNames, Some(SettingValue::List(names))) = (spec.kind, &value) {
                let kept = match saved.get(&SitePath::new(place.clone(), spec)) {
                    Some(crate::config::Saved {
                        value: SettingValue::List(kept),
                        ..
                    }) => kept.as_slice(),
                    _ => &[],
                };
                let catalog = self.catalog();
                let fresh: Vec<String> = catalog
                    .chosen(names)
                    .into_iter()
                    .filter(|n| !kept.contains(n))
                    .collect();
                catalog
                    .checked_names(&fresh)
                    .map_err(|e| format!("{}: {e}", spec.name))?;
            }
            let defaulted = save
                && place == Place::Global
                && self.saves_as_default(spec, value.as_ref(), &saved);
            let value = if defaulted { None } else { value };
            if let Place::PluginHook { plugin, hook } = &place {
                self.hooks
                    .engine
                    .table()
                    .checked_switch(plugin, hook)
                    .map_err(|e| format!("{}: {e}", change.target))?;
            }
            if let Place::PluginSlot { plugin, section } = &place {
                self.catalog()
                    .checked_section(plugin, section)
                    .map_err(|e| format!("{}: {e}", change.target))?;
            }
            if let Place::Plugin(plugin) = &place {
                self.checked_plugin(plugin, spec, value.as_ref())
                    .map_err(|e| format!("{}: {e}", change.target))?;
            }
            let named = matches!(change.target, SettingTarget::Name(_))
                || matches!(
                    place,
                    Place::PluginHook { .. } | Place::PluginSlot { .. } | Place::Plugin(_)
                );
            if !named && !save {
                return Err(format!(
                    "{} names a saved place, so it can only be saved.",
                    change.target
                )
                .into());
            }
            let choice = Choice {
                target: change.target.clone(),
                spec,
                value,
                class,
                named,
                saved: save,
                place,
            };
            self.window_floor(&choice)?;
            if choice.session() {
                set_given(&mut session, choice.given_target(), choice.value.clone());
            }
            if save && !(defaulted && saved.get(&choice.path()).is_none()) {
                let edit = match &choice.value {
                    Some(value) => SiteEdit::Set(choice.path(), value.clone()),
                    None => SiteEdit::Unset(choice.path()),
                };
                saved.edit(&edit, None);
                edits.push(edit);
            }
            choices.push(choice);
        }
        let touched = |name: &str| {
            choices
                .iter()
                .any(|c| c.spec.name == name && c.class == Class::Pair)
        };
        let after = |name: &str| {
            wisp_protocol::settings::find(name)
                .and_then(|spec| self.effective(spec, &session, &saved, &current))
        };
        let endpoint = if touched(names::ENDPOINT) {
            Some(
                text_of(after(names::ENDPOINT).as_ref())
                    .unwrap_or_else(|| self.connection_name(base.endpoint).to_owned()),
            )
        } else {
            None
        };
        let model = if touched(names::MODEL) {
            Some(text_of(after(names::MODEL).as_ref()).unwrap_or_else(|| base.model.clone()))
        } else {
            None
        };
        let effort = if touched(names::EFFORT) {
            Some(
                text_of(after(names::EFFORT).as_ref())
                    .and_then(|t| t.parse::<Effort>().ok())
                    .unwrap_or(base.effort),
            )
        } else {
            None
        };
        let before = model.clone().unwrap_or_else(|| base.model.clone());
        let named_model = model.is_some();
        let (target, check_model) = self.proposed_settings(endpoint, model, effort)?;
        if save
            && effort.is_some()
            && (target.endpoint == base.endpoint || named_model)
            && target.model != before
            && let Some(model_path) = wisp_protocol::settings::site_path(names::MODEL)
        {
            let layers = Layers {
                env: self.settings.layers.env.clone(),
                saved: Some(&saved),
                ..Layers::default()
            };
            let next_account = wisp_protocol::settings::find(names::ENDPOINT)
                .and_then(|spec| text_of(resolve_one(spec, &layers, &current).value()))
                .or_else(|| {
                    self.settings
                        .layers
                        .endpoints
                        .iter()
                        .find(|e| e.default)
                        .map(|e| e.name.clone())
                });
            if next_account.as_deref() == Some(self.connection_name(target.endpoint))
                && saved
                    .get(&model_path)
                    .is_none_or(|entry| entry.value == SettingValue::Text(before.clone()))
            {
                let edit = SiteEdit::Set(model_path, SettingValue::Text(target.model.clone()));
                saved.edit(&edit, None);
                edits.push(edit);
            }
        }
        let switches = crate::plugin::PluginSwitches::placed(self.running_ids(), |id, key| {
            SitePath::plugin_switch(id, key)
                .and_then(|path| {
                    crate::settings::path_value(
                        &session,
                        &self.settings.layers.flags,
                        &self.settings.layers.env,
                        &saved,
                        &path,
                    )
                })
                .is_none_or(|(value, _)| !crate::settings::is_off(&value))
        });
        self.provider_refusal(
            &crate::plugin::registry::providers_off(&switches),
            target.endpoint,
        )?;
        Ok((
            Verdict {
                binding: None,
                target,
                choices,
                edits,
                by: None,
            },
            check_model,
            effort,
        ))
    }

    fn checked_verdict(
        &self,
        mut verdict: Verdict,
        check_model: bool,
        effort: Option<Effort>,
    ) -> Result<Verdict, Refusal> {
        verdict.target = self.checked_pair(verdict.target, check_model, effort)?;
        if verdict.target.endpoint != self.endpoint
            || verdict
                .choices
                .iter()
                .any(|choice| choice.class == Class::Pair && choice.spec.name == names::ENDPOINT)
        {
            verdict.binding = self.reserve_binding(verdict.target.endpoint)?;
        }
        Ok(verdict)
    }

    fn change_cache(&self, spec: &SettingSpec, target: &Pending) -> CacheEffect {
        match spec.name {
            names::ENDPOINT if target.endpoint != self.endpoint => CacheEffect::Rebuilt {
                reason: RebuildReason::EndpointSwitch,
            },
            names::MODEL if target.model != self.session.model => CacheEffect::Rebuilt {
                reason: RebuildReason::ModelSwitch,
            },
            names::EFFORT => self.cache_effect(self.endpoint, &self.session.model, target.effort),
            _ => CacheEffect::Kept,
        }
    }

    fn model_table_verbatim(&self, choice: &Choice) -> Option<Overridden> {
        if choice.spec.name != names::CONTEXT_VERBATIM_USER_TOKENS {
            return None;
        }
        let rows = self
            .models
            .layered(self.endpoint_name(), &self.session.model);
        let sets = |row: Option<&ModelRow>| row.is_some_and(|r| r.verbatim_user_tokens.is_some());
        let layer = if sets(rows.endpoint) {
            Layer::EndpointModel
        } else if sets(rows.config) {
            Layer::Model
        } else {
            return None;
        };
        Some(Overridden {
            by: layer.wire(),
            scope: OverrideScope::ThisSession,
        })
    }

    fn saves_as_default(
        &self,
        spec: &SettingSpec,
        value: Option<&SettingValue>,
        saved: &SavedSettings,
    ) -> bool {
        let Some(SettingValue::List(names)) = value else {
            return false;
        };
        let layers = &self.settings.layers;
        let above = [&layers.forced, &layers.flags, &layers.env]
            .into_iter()
            .flatten()
            .any(|g| g.target.spec().name == spec.name);
        spec.kind == Kind::ToolNames
            && !above
            && crate::settings::saves_as_default(self.catalog(), names, saved)
    }

    fn overridden(&self, choice: &Choice, saved: bool) -> Option<Overridden> {
        if let Some(table) = self.model_table_verbatim(choice) {
            return Some(table);
        }
        if self
            .settings
            .layers
            .forced
            .iter()
            .any(|g| g.target.spec().name == choice.spec.name)
        {
            return Some(Overridden {
                by: Layer::Forced.wire(),
                scope: OverrideScope::ThisSession,
            });
        }
        if !saved {
            return None;
        }
        if !choice.named || choice.class == Class::NextSession {
            let current = self.current_at(&choice.place);
            let winner = |above: bool| {
                let layers = Layers {
                    session: if above {
                        self.settings.session.clone()
                    } else {
                        Vec::new()
                    },
                    flags: if above {
                        self.settings.layers.flags.clone()
                    } else {
                        Vec::new()
                    },
                    env: self.settings.layers.env.clone(),
                    saved: Some(&self.settings.layers.saved),
                    shipped: None,
                };
                resolve_one(choice.spec, &layers, &current)
                    .winner()
                    .map(|f| f.layer)
            };
            if winner(false) == Some(Layer::Environment) {
                return Some(Overridden {
                    by: Layer::Environment.wire(),
                    scope: OverrideScope::NewSessions,
                });
            }
            if let Some(layer) = winner(true).filter(|l| *l < choice.layer()) {
                return Some(Overridden {
                    by: layer.wire(),
                    scope: OverrideScope::ThisSession,
                });
            }
        }
        let defers = choice.spec.defers(choice.value.as_ref());
        let wins = |g: &Given| {
            g.target.spec().name == choice.spec.name && !g.target.spec().defers(Some(&g.value))
        };
        if defers && self.settings.layers.flags.iter().any(wins) {
            return Some(Overridden {
                by: Layer::Flag.wire(),
                scope: OverrideScope::ThisSession,
            });
        }
        let scope = if defers {
            OverrideScope::ThisSession
        } else {
            OverrideScope::NewSessions
        };
        self.settings
            .layers
            .env
            .iter()
            .any(wins)
            .then_some(Overridden {
                by: Layer::Environment.wire(),
                scope,
            })
    }

    fn remember(&self, target: &Pending) -> Option<String> {
        let store = &self.settings.layers.remembered;
        let landed = store.keep(
            self.connection_name(target.endpoint),
            &Remembered {
                model: target.model.clone(),
                effort: target.effort,
            },
        );
        let left = self.next_pair();
        let seeded = if left.endpoint != target.endpoint
            && store.of(self.connection_name(left.endpoint)).is_none()
        {
            store.keep(
                self.connection_name(left.endpoint),
                &Remembered {
                    model: left.model,
                    effort: left.effort,
                },
            )
        } else {
            Ok(())
        };
        landed
            .and(seeded)
            .err()
            .map(|_| "Could not remember the model and effort for this account.".to_owned())
    }

    pub(in crate::agent) async fn apply_verdict(
        &mut self,
        request: Option<RequestId>,
        verdict: Verdict,
        file: Option<SavedFile>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let Verdict {
            target,
            choices,
            binding,
            by,
            ..
        } = verdict;
        let mut remembered = if request.is_some()
            && !self.is_child()
            && choices.iter().any(|c| c.class == Class::Pair)
        {
            self.remember(&target)
        } else {
            None
        };
        let off_before = crate::agent::manage::session_names(&self.settings.session);
        let account_bound = self.account_bound(target.endpoint);
        let defaults_before = self.endpoint_defaults();
        for choice in choices.iter().filter(|c| c.settles()) {
            let keeps = |g: &Given| g.target.spec().name != choice.spec.name;
            self.settings.session.retain(keeps);
            self.settings.layers.flags.retain(keeps);
        }
        for choice in choices.iter().filter(|c| c.session()) {
            set_given(
                &mut self.settings.session,
                choice.given_target(),
                choice.value.clone(),
            );
        }
        let off_after = crate::agent::manage::session_names(&self.settings.session);
        crate::agent::manage::fold_own(
            &mut self.tool_switches.own,
            &off_before,
            &off_after,
            by.is_some(),
        );
        if let Some(file) = &file {
            for written in &file.written {
                let edit = match &written.value {
                    Some(value) => SiteEdit::Set(written.path.clone(), value.clone()),
                    None => SiteEdit::Unset(written.path.clone()),
                };
                self.settings.layers.saved.edit(&edit, written.line);
                if written.path.spec().name == names::ENDPOINT
                    && written.path.place.site() == Site::Global
                {
                    let named = text_of(written.value.as_ref());
                    for endpoint in &mut self.settings.layers.endpoints {
                        endpoint.default = named.as_deref() == Some(endpoint.name.as_str());
                    }
                }
            }
        }
        let unchanged = target.endpoint == self.endpoint
            && target.model == self.session.model
            && target.effort == self.session.effort;
        self.settings.pending = (!unchanged).then(|| target.clone());
        self.pending_endpoint_binding = if target.endpoint != self.endpoint {
            binding
        } else {
            None
        };
        let mut built = Vec::new();
        let mut warnings = Vec::new();
        for choice in &choices {
            let path = choice.path();
            let saved = file.as_ref().map(|f| Saved {
                path_display: f.path_display.clone(),
                line: f
                    .written
                    .iter()
                    .find(|w| w.path == path)
                    .and_then(|w| w.line),
                notes: f.notes.clone(),
            });
            let mut moved = None;
            let (when, cache) = match choice.class {
                Class::Pair => (
                    When::NextTurn,
                    Some(self.change_cache(choice.spec, &target)),
                ),
                Class::Client => (When::Now, None),
                Class::NextSession if choice.spec.applies == Applies::NextCall => {
                    (When::NextCall, None)
                }
                Class::NextSession => (When::NextSession, None),
                Class::Kernel => {
                    let how = Applied {
                        session: choice.session(),
                        saved: saved.is_some(),
                    };
                    let effect = self.apply_kernel(&path, how, choice.value.as_ref());
                    warnings.extend(effect.warning);
                    if moves_point(choice.spec.name) {
                        moved = self.next_message_compaction();
                    }
                    (effect.when, effect.cache)
                }
            };
            let change = AppliedChange {
                target: choice.applied_target(),
                value: choice.value.clone(),
                layer: choice.layer().wire(),
                when,
                cache,
                consequence: moved,
                overridden: self.overridden(choice, saved.is_some()),
                saved,
                warnings: if choice.spec.settles_on_save() {
                    self.background_point_note(&choice.place)
                        .into_iter()
                        .collect()
                } else {
                    let mut notes = self.unknown_sections(choice.spec, choice.value.as_ref());
                    notes.extend(self.add_back_notes(choice));
                    notes.extend(self.stays_note(choice));
                    notes
                },
            };
            let resolved = match (&choice.value, choice.class) {
                (None, Class::Pair | Class::Kernel) => self.resolved_at(choice.spec, &choice.place),
                _ => None,
            };
            let switches = choice.switches(&target, self.endpoint, &self.session.model);
            built.push((change, resolved, switches));
        }
        let quote = self.switch_quote(target.endpoint, &target.model);
        let switching = built.iter().any(|(_, _, switches)| *switches);
        let notice = switching
            .then(|| self.past_point_notice(target.endpoint, &target.model))
            .flatten();
        let mut unlisted = switching
            .then(|| {
                self.unlisted_note(&target)
                    .or_else(|| self.listed_only_note(&target))
            })
            .flatten();
        let now: Vec<String> = choices
            .iter()
            .zip(&built)
            .filter(|(choice, (change, _, _))| {
                change.when == When::Now && choice.class != Class::Client
            })
            .map(|(_, (change, _, _))| change.target.to_string())
            .collect();
        self.note_changed(
            built
                .iter()
                .filter(|(change, _, _)| change.when == When::NextTurn)
                .map(|(change, _, _)| change.target.to_string()),
        );
        self.defer_settings_changed(now);
        let mut applied = Vec::new();
        let mut logged = Vec::new();
        for (mut change, resolved, switches) in built {
            if [names::ENDPOINT, names::MODEL, names::EFFORT].contains(&change.target.spec().name)
                && let Some(warning) = remembered.take()
            {
                change.warnings.push(warning);
            }
            if switches {
                change.consequence = quote.clone().or(change.consequence.take());
                if let Some(note) = unlisted.take() {
                    change.warnings.insert(0, note);
                }
            }
            logged.push(LoggedChange {
                by,
                ..LoggedChange::of(&change, resolved)
            });
            applied.push(change);
        }
        if self.endpoint_defaults() != defaults_before {
            self.endpoints_changed(events).await;
        }
        if choices.iter().any(|c| {
            [names::TOOLS, names::TURNED_OFF].contains(&c.spec.name)
                || matches!(c.place, Place::Plugin(_))
        }) {
            self.mark_waiting(events).await;
        }
        let provider_switch = choices
            .iter()
            .any(|choice| choice.path().provider_switch().is_some());
        let cache = self.pending_cache(&target);
        self.settings_changed(
            target,
            cache,
            account_bound,
            request,
            (applied, logged),
            events,
        )
        .await;
        if provider_switch {
            Box::pin(self.sync_catalog(events)).await;
        }
        if let Some(notice) = notice {
            super::emit(events, notice).await;
        }
        for diagnostic in warnings {
            super::emit(events, AgentEvent::InstructionsWarning { diagnostic }).await;
        }
        self.background_after_change(events).await;
    }

    fn unknown_sections(&self, spec: &SettingSpec, value: Option<&SettingValue>) -> Vec<String> {
        let (Kind::SectionNames, Some(SettingValue::List(names))) = (spec.kind, value) else {
            return Vec::new();
        };
        let known = self.catalog().section_names();
        names
            .iter()
            .filter(|name| !known.contains(name))
            .map(|name| format!("no sidebar section named {name}; kept"))
            .collect()
    }

    fn unlisted_note(&self, target: &Pending) -> Option<String> {
        let list = self.provider_at(target.endpoint).model_list()?;
        list.find(&target.model).is_none().then(|| {
            format!(
                "{} doesn't list {}, so the request may be refused.",
                self.connection_name(target.endpoint),
                shown_model(&target.model)
            )
        })
    }

    fn listed_only_note(&self, target: &Pending) -> Option<String> {
        let configured = self.models.named_models().contains(&target.model)
            || self
                .model_entries()
                .iter()
                .any(|entry| entry.id == target.model && entry.configured);
        crate::context::listed_only_warning(
            self.provider_at(target.endpoint).as_ref(),
            &target.model,
            configured,
        )
    }

    fn endpoint_defaults(&self) -> Vec<bool> {
        self.settings
            .layers
            .endpoints
            .iter()
            .map(|e| e.default)
            .collect()
    }

    async fn endpoints_changed(&mut self, events: &mpsc::Sender<AgentEvent>) {
        let endpoints = self.settings.layers.endpoints.clone();
        self.emit(events, AgentEvent::EndpointsChanged { endpoints })
            .await;
    }

    pub(in crate::agent) fn pending_cache(&self, target: &Pending) -> CacheEffect {
        let pair = self.cache_effect(target.endpoint, &target.model, target.effort);
        if matches!(pair, CacheEffect::Rebuilt { .. }) {
            return pair;
        }
        match self.pending_option() {
            Some(reason) => CacheEffect::Rebuilt { reason },
            None => pair,
        }
    }

    fn choices_of(&self, spec: &SettingSpec, pair: &Pending) -> Vec<String> {
        let Some(dynamic) = spec.dynamic else {
            return spec
                .kind
                .choices()
                .iter()
                .map(|c| (*c).to_owned())
                .collect();
        };
        let live = || self.connections.iter().filter(|c| !self.is_gone(&c.name));
        match dynamic {
            Dynamic::Endpoints => live()
                .filter(|c| self.may_move_to(&c.name))
                .map(|c| c.name.clone())
                .collect(),
            Dynamic::Models => self.model_entries().into_iter().map(|m| m.id).collect(),
            Dynamic::Efforts => {
                let provider = self.provider_at(pair.endpoint);
                match provider.capabilities(&pair.model) {
                    Some(row) => row.efforts.iter().map(|e| e.as_str().to_owned()).collect(),
                    None => Effort::ALL.iter().map(|e| e.as_str().to_owned()).collect(),
                }
            }
            Dynamic::Tools => self.catalog().leader_names(),
            Dynamic::Sections => self.catalog().section_names(),
            Dynamic::VoiceEndpoints => live()
                .filter(|c| c.provider.voice().is_some())
                .map(|c| c.name.clone())
                .collect(),
            Dynamic::Voices => self
                .global(names::VOICE_ENDPOINT)
                .and_then(|name| live().find(|c| c.name == name.text()))
                .and_then(|c| c.provider.voice())
                .map(|route| route.voices().iter().map(|v| (*v).to_owned()).collect())
                .unwrap_or_default(),
        }
    }

    pub(in crate::agent) fn switch_targets(
        &self,
        pair: &Pending,
    ) -> Vec<(SwitchTarget, usize, String, Option<Remembered>)> {
        let entries = self.model_entries();
        let offers = offers(&entries);
        let chat = chat(&pair.model);
        let live: Vec<(usize, &crate::agent::Connection)> = self
            .connections
            .iter()
            .enumerate()
            .filter(|(_, c)| !self.is_gone(&c.name) && self.may_move_to(&c.name))
            .collect();
        let listing = self.account_listing(self.connection_name(pair.endpoint));
        let mut ids: Vec<&str> = entries.iter().map(|m| m.id.as_str()).collect();
        for m in &listing.models {
            if !ids.contains(&m.id.as_str()) {
                ids.push(&m.id);
            }
        }
        let models = ids.into_iter().map(|id| {
            (
                SwitchTarget::Model(id.to_owned()),
                pair.endpoint,
                id.to_owned(),
                None,
            )
        });
        let endpoints = live.iter().map(|(index, c)| {
            let held = self
                .remembered_pair(pair, *index, None)
                .map(|p| Remembered {
                    model: p.model,
                    effort: p.effort,
                });
            let model = held
                .as_ref()
                .map_or_else(|| self.runs_on(*index, pair), |p| p.model.clone());
            (SwitchTarget::Endpoint(c.name.clone()), *index, model, held)
        });
        let pairs = live
            .iter()
            .filter(|(index, _)| *index != pair.endpoint)
            .flat_map(|(index, c)| {
                let account = c.provider.kind().as_str();
                let listing = self.account_listing(&c.name);
                wisp_protocol::settings::reachable(&offers, listing.landing(account), chat)
                    .into_iter()
                    .map(move |model| {
                        (
                            SwitchTarget::Pair {
                                endpoint: c.name.clone(),
                                model: model.clone(),
                            },
                            *index,
                            model,
                            None,
                        )
                    })
            });
        models.chain(endpoints).chain(pairs).collect()
    }

    fn is_gone(&self, name: &str) -> bool {
        self.gone.iter().any(|(gone, _)| gone == name)
    }

    fn may_move_to(&self, name: &str) -> bool {
        let index = self.connections.iter().position(|c| c.name == name);
        !self.is_child() || index.is_some_and(|index| self.child_move_refused(index).is_none())
    }

    pub(in crate::agent) fn restate_session(&mut self, name: &str, value: SettingValue) {
        if let Some(spec) = wisp_protocol::settings::find(name) {
            set_given(&mut self.settings.session, Target::Name(spec), Some(value));
        }
    }

    pub(in crate::agent) fn model_entries(&self) -> Vec<ModelEntry> {
        self.settings.layers.model_entries()
    }

    pub(in crate::agent) fn setting_rows(&self, keys_file: Option<&str>) -> Vec<ResolvedSetting> {
        let pair = self.next_pair();
        let current = self.current_for(&pair);
        let shipped = ModelTable::shipped().ok();
        let layers = Layers {
            session: self.settings.session.clone(),
            flags: self.settings.layers.flags.clone(),
            env: self.settings.layers.env.clone(),
            saved: Some(&self.settings.layers.saved),
            shipped: shipped.as_ref(),
        };
        let full_screen = self.settings.layers.full_screen;
        let name = self.connection_name(pair.endpoint);
        let endpoint = EndpointRow {
            value: name.to_owned(),
            layer: Layer::Default.wire(),
            origin: wisp_protocol::settings::find(names::ENDPOINT)
                .map(|spec| spec.default_words())
                .unwrap_or_default(),
            keys_file: keys_file.map(str::to_owned),
            effort: pair.effort,
            credential: self
                .settings
                .layers
                .endpoints
                .iter()
                .find(|summary| summary.name == name)
                .and_then(|summary| summary.credential.clone()),
            wire: self.wire_of(name),
        };
        snapshot_rows(&layers, &current, full_screen, &endpoint, &|spec| {
            self.choices_of(spec, &pair)
        })
    }

    pub(in crate::agent) fn call_view(&self, plugin: &str) -> Option<Arc<CallView>> {
        (plugin == MENUS_PLUGIN).then(|| {
            Arc::new(CallView {
                settings: self
                    .setting_rows(crate::config::keys_file_of(&self.settings.layers.endpoints)),
                accounts: self.accounts_view(),
            })
        })
    }

    pub(in crate::agent) fn account_basics(
        &self,
    ) -> Vec<(
        wisp_protocol::settings::EndpointSummary,
        Option<accounts::UsageAccount>,
    )> {
        let pair = self.next_pair();
        let yours = self.connection_name(pair.endpoint);
        let summaries = match &self.endpoint_catalog {
            Some(catalog) if self.is_child() => catalog.view().endpoints,
            _ => self.settings.layers.endpoints.clone(),
        };
        let usage_on = self.claude_usage();
        summaries
            .into_iter()
            .map(|summary| {
                let usage = endpoints::summary_inputs(&summary)
                    .filter(|(_, billing, _)| !billing.is_metered())
                    .map(|(provider, ..)| accounts::UsageAccount {
                        name: summary.name.clone(),
                        yours: summary.name == yours,
                        off: provider == ProviderKind::Anthropic && !usage_on,
                        expired: summary.expired,
                        unavailable: summary.unavailable.is_some(),
                    });
                (summary, usage)
            })
            .collect()
    }

    fn accounts_view(&self) -> AccountsView {
        let pair = self.next_pair();
        let yours = self.connection_name(pair.endpoint);
        let entries = self.model_entries();
        let offers = offers(&entries);
        let chat = chat(&pair.model);
        let now = self.clock.now().as_second();
        let rows = self
            .account_basics()
            .into_iter()
            .map(|(summary, usage)| {
                let inputs = endpoints::summary_inputs(&summary);
                let limits = usage.map(|account| {
                    accounts::limits(self.plan_usage.state(&summary.name, now), account.off)
                });
                let row = AccountRow {
                    name: endpoints::plain(&summary.name).to_owned(),
                    kind: inputs.map(|(provider, billing, gateway)| {
                        endpoints::account_kind(provider, billing, gateway).to_owned()
                    }),
                    unavailable: summary.unavailable.is_some(),
                    sign_in: None,
                    yours: summary.name == yours,
                    start: summary.default,
                    models: offered(
                        &offers,
                        self.account_listing(&summary.name)
                            .landing(&summary.provider),
                        chat,
                    )
                    .iter()
                    .map(|id| shown_model(id).to_owned())
                    .collect(),
                    limits,
                };
                (summary.name.clone(), row)
            })
            .collect();
        AccountsView {
            model: shown_model(&pair.model).to_owned(),
            effort: pair.effort.as_str().to_owned(),
            rows,
            auth: self
                .queue
                .endpoints
                .clone()
                .or_else(|| self.agents.endpoint_host()),
        }
    }

    pub(in crate::agent) fn snapshot(
        &self,
        request: RequestId,
        read: Option<SavedRead>,
    ) -> SettingsSnapshot {
        let pair = self.next_pair();
        let keys_file = crate::config::keys_file_of(&self.settings.layers.endpoints);
        let settings = self.setting_rows(Some(crate::config::keys_file_shown(keys_file)));
        let (saved, file, often) = match read {
            Some(read) => (read.saved, read.file, read.often),
            None => (
                self.settings
                    .layers
                    .saved
                    .iter()
                    .map(|(path, entry)| {
                        SavedValue::of(
                            path.clone(),
                            entry.value.clone(),
                            u64::try_from(entry.line).ok(),
                        )
                    })
                    .collect(),
                FileState::Missing,
                Vec::new(),
            ),
        };
        let mut saved = saved;
        crate::config::shown_for_keys_file(&mut saved, keys_file);
        let mut endpoints = self.settings.layers.endpoints.clone();
        endpoints.retain(|e| self.may_move_to(&e.name));
        SettingsSnapshot {
            often,
            answers: request,
            context: SnapshotContext {
                endpoint: self.connection_name(pair.endpoint).to_owned(),
                model: pair.model.clone(),
                agent: None,
                max_tokens: self.max_tokens_on(pair.endpoint, &pair.model),
                window: self.next_window_and_point().0,
                room: self.next_window_room(),
            },
            settings,
            saved,
            endpoints,
            models: self.model_entries(),
            file,
            switches: self.switch_quotes(self.switch_targets(&pair)),
            compaction: self.compaction_quote(self.max_tokens_on(pair.endpoint, &pair.model)),
            plugins: self.search_marked(self.catalog().plugins.clone(), &pair),
            placed: self.placed(),
        }
    }

    fn search_marked(&self, mut plugins: Vec<PluginEntry>, pair: &Pending) -> Vec<PluginEntry> {
        let form = self.search_form(self.connection_name(pair.endpoint), &pair.model);
        let SearchForm::None(why) = form else {
            return plugins;
        };
        let tools = plugins.iter_mut().flat_map(|plugin| &mut plugin.tools);
        for tool in tools.filter(|t| t.is_search() && t.unavailable.is_none()) {
            tool.unavailable = why.why().map(str::to_owned);
        }
        plugins
    }

    fn stays_note(&self, choice: &Choice) -> Option<String> {
        let path = choice.path();
        let (provider, key) = path.provider_switch()?;
        if !self.path().is_root() || self.plugin_key_on(provider, key) {
            return None;
        }
        let agents: Vec<String> = self
            .agents
            .on_accounts(&self.provider_accounts(provider))
            .into_iter()
            .filter(|a| !a.working)
            .map(|a| a.agent)
            .collect();
        if agents.is_empty() {
            return None;
        }
        let wait = if agents.len() == 1 { "waits" } else { "wait" };
        let until = if !self.plugin_key_on(provider, wisp_protocol::settings::INSTALLED_KEY) {
            "added back"
        } else {
            "on again"
        };
        Some(format!(
            "{} {wait} until {provider} is {until}",
            wisp_tool_text::output::and_list(&agents)
        ))
    }

    fn add_back_notes(&self, choice: &Choice) -> Vec<String> {
        let Place::Plugin(plugin) = &choice.place else {
            return Vec::new();
        };
        if choice.spec.name != wisp_protocol::settings::PLUGIN_INSTALLED
            || choice.value.as_ref().is_some_and(crate::settings::is_off)
        {
            return Vec::new();
        }
        self.hooks
            .registry
            .as_ref()
            .and_then(|r| r.add_back(plugin).ok())
            .unwrap_or_default()
    }

    fn checked_plugin(
        &self,
        plugin: &str,
        spec: &SettingSpec,
        value: Option<&SettingValue>,
    ) -> Result<(), String> {
        let entry = self.catalog().plugins.iter().find(|p| p.id == plugin);
        if entry.is_none() && !self.running_ids().contains(plugin) {
            return Err(crate::plugin::unknown_plugin(plugin, self.catalog()));
        }
        let adds_back = spec.name == wisp_protocol::settings::PLUGIN_INSTALLED
            && entry.is_some_and(|e| !e.installed)
            && value.is_none_or(|v| !crate::settings::is_off(v));
        match (&self.hooks.registry, adds_back) {
            (Some(registry), true) => registry.add_back(plugin).map(drop),
            _ => Ok(()),
        }
    }
}

fn moves_point(name: &str) -> bool {
    matches!(
        name,
        names::CONTEXT_AUTO_COMPACT | names::CONTEXT_WINDOW | names::COMPACT_AT
    )
}

pub(crate) fn chat(model: &str) -> Chat<'_> {
    Chat {
        provider: crate::endpoints::goes::model_provider(model).map(ProviderKind::as_str),
        model,
    }
}

#[cfg(test)]
mod tests;
