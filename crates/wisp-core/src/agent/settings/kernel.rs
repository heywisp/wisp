use wisp_protocol::settings::{
    Place, SettingSpec, SettingTarget, SettingValue, SitePath, Target, When, find, names,
};
use wisp_protocol::{
    CacheEffect, CacheTtl, Diagnostic, InstructionsSource, MessageTime, RebuildReason,
};

use super::change::is_kernel;
use crate::agent::Agent;
use crate::context::ModelSettings;
use crate::endpoints::PricingMode;
use crate::instructions::{large_warning, total_bytes};
use crate::provider::RequestOptions;
use crate::resume::{LaterOptions, LoggedSession, SessionValue};
use crate::settings::{Current, Layers, on, resolve_one, text};

const CACHE_TTL_CHANGE: &str = "cache_ttl_change";

#[derive(Debug, Clone, Default)]
pub(in crate::agent) struct KernelSettings {
    pub(in crate::agent) options: RequestOptions,
    later: Later,
}

#[derive(Debug, Clone, Default)]
struct Later {
    options: LaterOptions,
    models: Option<ModelSettings>,
    instructions_source: Option<InstructionsSource>,
}

pub(super) const RESTORED_NOW: [&str; 7] = [
    names::CLAUDE_USAGE,
    names::COMPACT_AT,
    names::CONTEXT_COUNT_TOKENS,
    names::CONTEXT_WINDOW,
    names::INSTRUCTIONS_WARN_TOKENS,
    names::PRICING,
    names::RETRY_BUDGET_USD,
];

fn path_of(target: &SettingTarget) -> SitePath {
    match target {
        SettingTarget::Name(name) => SitePath::new(Place::Global, name.spec()),
        SettingTarget::Path(path) => path.clone(),
    }
}

fn tokens(value: Option<&SettingValue>) -> Option<u64> {
    match value {
        Some(SettingValue::Tokens(tokens)) => Some(*tokens),
        _ => None,
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "a budget in micro-dollars, far below f64's exact range"
)]
fn budget(value: Option<&SettingValue>) -> Option<f64> {
    match value {
        Some(SettingValue::Usd { micros }) => Some(*micros as f64 / 1_000_000.0),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy)]
pub(in crate::agent) struct Applied {
    pub(in crate::agent) session: bool,
    pub(in crate::agent) saved: bool,
}

pub(in crate::agent) struct Effect {
    pub(in crate::agent) when: When,
    pub(in crate::agent) cache: Option<CacheEffect>,
    pub(in crate::agent) warning: Option<Diagnostic>,
}

impl Effect {
    fn now() -> Self {
        Self {
            when: When::Now,
            cache: None,
            warning: None,
        }
    }

    fn kept() -> Self {
        Self::rebuilt_if(None)
    }

    fn next_session() -> Self {
        Self {
            when: When::NextSession,
            cache: None,
            warning: None,
        }
    }

    fn rebuilt_if(reason: Option<RebuildReason>) -> Self {
        Self {
            when: When::NextTurn,
            cache: Some(reason.map_or(CacheEffect::Kept, |reason| CacheEffect::Rebuilt { reason })),
            warning: None,
        }
    }
}

impl Agent {
    #[must_use]
    pub fn with_request_options(mut self, options: RequestOptions) -> Self {
        self.settings.kernel.options = options;
        self
    }

    pub(in crate::agent) fn claude_usage(&self) -> bool {
        on(self.global(names::CLAUDE_USAGE).as_ref()).unwrap_or(false)
    }

    pub(in crate::agent) fn configure_plan_usage(&mut self) {
        if self.is_child() || self.path() != wisp_protocol::AgentPath::root() {
            return;
        }
        let enabled = self.claude_usage();
        for connection in &self.connections {
            if connection.provider.configure_plan_usage(enabled) {
                self.plan_usage
                    .reset_lookup(&connection.name, self.clock.now().as_second());
            }
        }
    }

    pub(in crate::agent) fn request_options(&self) -> RequestOptions {
        self.settings.kernel.options
    }

    pub(in crate::agent) fn begin_kernel_turn(&mut self) {
        self.pin_strategy();
        let later = std::mem::take(&mut self.settings.kernel.later);
        if let Some(ttl) = later.options.cache_ttl {
            let longer =
                crate::context::ttl_span(ttl) > crate::context::ttl_span(self.session.cache_ttl);
            if longer && self.epoch.expected().is_none() {
                self.epoch.expect(CACHE_TTL_CHANGE);
            }
            self.session.cache_ttl = ttl;
        }
        if let Some(on) = later.options.debug_cache {
            self.settings.kernel.options.debug_cache = on;
        }
        if let Some(on) = later.options.refusal_fallback {
            self.settings.kernel.options.refusal_fallback = on;
        }
        if let Some(time) = later.options.time {
            self.session.message_time = time;
        }
        if let Some(on) = later.options.context_note {
            self.session.context_note = on;
        }
        if let Some(on) = later.options.auto_compact {
            self.set_auto_compact(on);
        }
        if let Some(models) = later.models {
            self.models = models;
        }
        if let (Some(source), Some(state)) = (later.instructions_source, self.instructions.as_mut())
        {
            state.source = source;
        }
    }

    pub(in crate::agent) fn applied_value(&self, name: &str) -> Option<SettingValue> {
        match name {
            names::TIME | names::CONTEXT_CONTEXT_NOTE | names::CONTEXT_AUTO_COMPACT => self
                .now_options()
                .values()
                .into_iter()
                .find_map(|(option, value)| (option == name).then_some(value)),
            names::INSTRUCTIONS_SOURCE => self
                .instructions
                .as_ref()
                .map(|i| SettingValue::Text(i.source.as_str().to_owned())),
            names::INSTRUCTIONS_WARN_TOKENS => self
                .instructions
                .as_ref()
                .map(|i| SettingValue::Tokens(i.warn_tokens)),
            _ => None,
        }
    }

    pub(in crate::agent) fn now_options(&self) -> LaterOptions {
        let options = self.settings.kernel.options;
        LaterOptions {
            cache_ttl: Some(self.session.cache_ttl),
            debug_cache: Some(options.debug_cache),
            refusal_fallback: Some(options.refusal_fallback),
            time: Some(self.session.message_time),
            context_note: Some(self.session.context_note),
            auto_compact: Some(self.auto_compact()),
        }
    }

    pub(in crate::agent) fn pending_option_values(
        &self,
    ) -> Vec<(&'static str, SettingValue, RebuildReason)> {
        self.settings
            .kernel
            .later
            .options
            .rebuilds(&self.now_options())
    }

    pub(in crate::agent) fn next_auto_compact(&self) -> bool {
        self.settings
            .kernel
            .later
            .options
            .auto_compact
            .unwrap_or_else(|| self.auto_compact())
    }

    pub(in crate::agent) fn note_later_overflow(
        &mut self,
        endpoint: &str,
        model: &str,
        tokens: u64,
    ) {
        if let Some(later) = self.settings.kernel.later.models.as_mut() {
            later.restore_overflow(endpoint, model, tokens);
        }
    }

    pub(in crate::agent) fn next_model_settings(&self) -> &ModelSettings {
        self.settings
            .kernel
            .later
            .models
            .as_ref()
            .unwrap_or(&self.models)
    }

    pub(in crate::agent) fn flag_value(&self, name: &str) -> Option<&SettingValue> {
        self.explicit_flags()
            .find(|g| matches!(&g.target, Target::Name(spec) if spec.name == name))
            .map(|g| &g.value)
    }

    pub fn next_cache_ttl(&self) -> Option<CacheTtl> {
        self.settings.kernel.later.options.cache_ttl
    }

    pub fn next_message_time(&self) -> Option<MessageTime> {
        self.settings.kernel.later.options.time
    }

    pub fn next_context_note(&self) -> Option<bool> {
        self.settings.kernel.later.options.context_note
    }

    pub(in crate::agent) fn restore_kernel(&mut self, logged: &LoggedSession) {
        let restored: Vec<&SessionValue> = logged
            .session_values
            .iter()
            .filter(|v| is_kernel(v.target.spec()))
            .filter(|v| self.restore_session(&v.target, v.value.clone()))
            .collect();
        for value in &restored {
            if RESTORED_NOW.contains(&value.target.spec().name) {
                let how = Applied {
                    session: true,
                    saved: false,
                };
                self.apply_kernel(&path_of(&value.target), how, value.value.as_ref());
            }
        }
        self.tool_switches.own.clone_from(&logged.own_off);
        self.begin_kernel_turn();
        let later = self.with_flags(logged.later.clone());
        self.settings.kernel.later.options = later.beyond(&self.now_options());
        let source = logged
            .session_values
            .iter()
            .filter(|_| self.flag_value(names::INSTRUCTIONS_SOURCE).is_none())
            .find(|v| v.target.spec().name == names::INSTRUCTIONS_SOURCE)
            .and_then(|v| text(v.value.as_ref()))
            .and_then(InstructionsSource::parse);
        self.settings.kernel.later.instructions_source = self.source_change(source);
    }

    pub(in crate::agent) fn with_flags(&self, mut options: LaterOptions) -> LaterOptions {
        for flag in self.explicit_flags() {
            if let Target::Name(spec) = &flag.target {
                options.set(spec.name, Some(&flag.value));
            }
        }
        options
    }

    fn source_change(&self, wanted: Option<InstructionsSource>) -> Option<InstructionsSource> {
        let current = &self.instructions.as_ref()?.source;
        wanted.filter(|w| w != current)
    }

    pub(in crate::agent) fn pending_option(&self) -> Option<RebuildReason> {
        self.pending_option_values()
            .into_iter()
            .next()
            .map(|(.., reason)| reason)
    }

    pub(in crate::agent) fn current_at(&self, place: &Place) -> Current {
        let (endpoint, model) = self.next_names();
        match place {
            Place::Global
            | Place::Agent(_)
            | Place::PluginHook { .. }
            | Place::PluginSlot { .. }
            | Place::Plugin(_) => Current {
                endpoint: Some(endpoint),
                model,
            },
            Place::Endpoint(e) => Current {
                endpoint: Some(e.clone()),
                model,
            },
            Place::Model(m) => Current {
                endpoint: Some(endpoint),
                model: m.clone(),
            },
            Place::EndpointModel { endpoint, model } => Current {
                endpoint: Some(endpoint.clone()),
                model: model.clone(),
            },
        }
    }

    pub(in crate::agent) fn resolved_at(
        &self,
        spec: &'static SettingSpec,
        place: &Place,
    ) -> Option<SettingValue> {
        let layers = Layers {
            session: self.settings.session.clone(),
            flags: self.settings.layers.flags.clone(),
            env: self.settings.layers.env.clone(),
            saved: Some(&self.settings.layers.saved),
            shipped: None,
        };
        resolve_one(spec, &layers, &self.current_at(place))
            .value()
            .cloned()
    }

    pub(in crate::agent) fn global(&self, name: &str) -> Option<SettingValue> {
        find(name).and_then(|spec| self.resolved_at(spec, &Place::Global))
    }

    fn next_models(&mut self) -> &mut ModelSettings {
        let models = &self.models;
        self.settings
            .kernel
            .later
            .models
            .get_or_insert_with(|| models.clone())
    }

    fn model_field(&mut self, how: Applied) {
        let session = (how.session || how.saved).then(|| self.settings.session.clone());
        let flags = how.saved.then(|| self.settings.layers.flags.clone());
        let saved = how.saved.then(|| self.settings.layers.saved.clone());
        let models = self.next_models();
        if let Some(session) = session {
            models.set_session(session);
        }
        if let Some(flags) = flags {
            models.set_flags(&flags);
        }
        if let Some(saved) = saved {
            models.set_saved(saved);
        }
        if self.settings.kernel.later.models.as_ref() == Some(&self.models) {
            self.settings.kernel.later.models = None;
        }
    }

    fn tools_rebuilt() -> Effect {
        Effect {
            when: When::NextTurn,
            cache: Some(CacheEffect::Rebuilt {
                reason: RebuildReason::ToolsChange,
            }),
            warning: None,
        }
    }

    fn tools_effect(&self) -> Effect {
        match self.tools_waiting() {
            Some((_, _, true)) => Self::tools_rebuilt(),
            Some((false, true, false)) | None => Effect::next_session(),
            Some(_) => Effect {
                when: When::NextTurn,
                cache: Some(self.tools_cache()),
                warning: None,
            },
        }
    }

    fn apply_next_turn(&mut self, name: &str) -> Option<Effect> {
        let value = self.global(name);
        let now = self.now_options();
        let mut later = self.settings.kernel.later.options.clone();
        if !later.set(name, value.as_ref()) {
            return None;
        }
        let later = later.beyond(&now);
        let reason = later.rebuilds(&now).into_iter().find(|(n, ..)| *n == name);
        self.settings.kernel.later.options = later;
        Some(Effect::rebuilt_if(reason.map(|(.., reason)| reason)))
    }

    pub(in crate::agent) fn apply_kernel(
        &mut self,
        path: &SitePath,
        how: Applied,
        value: Option<&SettingValue>,
    ) -> Effect {
        if path.spec().is_declared() {
            return Effect::kept();
        }
        if let Some(effect) = self.apply_next_turn(path.spec().name) {
            return effect;
        }
        match path.spec().name {
            names::CLAUDE_USAGE => {
                self.configure_plan_usage();
                Effect::now()
            }
            names::INSTRUCTIONS_SOURCE => {
                let wanted = text(self.global(names::INSTRUCTIONS_SOURCE).as_ref())
                    .and_then(InstructionsSource::parse);
                self.settings.kernel.later.instructions_source = self.source_change(wanted);
                Effect::kept()
            }
            names::CONTEXT_STRATEGY => Effect::kept(),
            names::CONTEXT_WINDOW | names::COMPACT_AT => {
                self.model_field(how);
                Effect::kept()
            }
            names::CONTEXT_COUNT_TOKENS => {
                if let Some(on) = on(self.global(names::CONTEXT_COUNT_TOKENS).as_ref()) {
                    self.set_count_tokens(on);
                }
                Effect::now()
            }
            names::INSTRUCTIONS_WARN_TOKENS => {
                let warn = tokens(self.global(names::INSTRUCTIONS_WARN_TOKENS).as_ref());
                let shadowed = value.is_some() && tokens(value) != warn;
                let mut effect = Effect::now();
                if !shadowed && let (Some(warn), Some(loaded)) = (warn, self.instructions.as_mut())
                {
                    loaded.warn_tokens = warn;
                }
                if !shadowed && let (Some(warn), Some(loaded)) = (warn, self.instructions.as_ref())
                {
                    let info = self.priced_rates(self.endpoint, &self.session.model);
                    let price = info
                        .as_ref()
                        .map(|info| (self.session.model.as_str(), info));
                    effect.warning = large_warning(total_bytes(&loaded.summary), warn, price);
                }
                effect
            }
            names::RETRY_BUDGET_USD => {
                let spent = self.global(names::RETRY_BUDGET_USD);
                self.retry = self.retry.with_budget(budget(spent.as_ref()));
                Effect::now()
            }
            names::PRICING => {
                if let Place::Endpoint(endpoint) = &path.place {
                    let priced = text(self.resolved_at(path.spec(), &path.place).as_ref())
                        .is_none_or(|t| t == PricingMode::List.as_str());
                    if let Some(connection) =
                        self.connections.iter_mut().find(|c| c.name == *endpoint)
                    {
                        connection.priced = priced;
                    }
                }
                Effect::now()
            }
            wisp_protocol::settings::PLUGIN_ENABLED | wisp_protocol::settings::PLUGIN_INSTALLED
                if self.path().is_root() && path.provider_switch().is_some() =>
            {
                Effect::now()
            }
            wisp_protocol::settings::PLUGIN_ENABLED | wisp_protocol::settings::PLUGIN_INSTALLED
                if self.rebuild_due() =>
            {
                Self::tools_rebuilt()
            }
            wisp_protocol::settings::PLUGIN_HOOK
            | wisp_protocol::settings::PLUGIN_ENABLED
            | wisp_protocol::settings::PLUGIN_INSTALLED
            | names::SELF_CHECK_BEFORE_END => Effect::kept(),
            names::TOOLS | names::TURNED_OFF => self.tools_effect(),
            _ => Effect::now(),
        }
    }
}
