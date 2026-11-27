use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tokio::sync::mpsc;
use wisp_protocol::log::{FeatureState, LogEvent};
use wisp_protocol::settings::{
    AppliedChange, Landed, LoggedChange, Remembered, Restored, SettingTarget, SettingValue, When,
    Withdrawal, names,
};
use wisp_protocol::{
    AgentEvent, CacheEffect, Effort, EffortMarker, Message, RebuildReason, RequestId, SessionConfig,
};

use super::choice::Continuation;
use super::{Agent, emit};
use crate::endpoints::{EndpointName, unknown_endpoint};
use crate::provider::{
    Feature, FeatureUse, Features, Provider, Request, UNRENDERED, pair_features,
};
use crate::settings::Given;
use crate::settings::host::StartLayers;

mod change;
mod kernel;

pub use change::{KERNEL_NAMES, not_mid_session, not_yet};
pub(super) use change::{Projected, Refusal, Verdict};
pub(crate) use change::{chat, checked};
use kernel::KernelSettings;

pub use wisp_protocol::MAX_MODEL_ID;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairState {
    Documented,
    Unknown,
    Proven,
    FallenBack,
}

impl From<FeatureState> for PairState {
    fn from(state: FeatureState) -> Self {
        match state {
            FeatureState::Proven => Self::Proven,
            FeatureState::FallenBack => Self::FallenBack,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Pending {
    pub(super) endpoint: usize,
    pub(super) model: String,
    pub(super) effort: Effort,
}

#[derive(Debug, Clone)]
pub(super) struct SettingsState {
    start_effort: Effort,
    max_tokens: u32,
    pub(super) pending: Option<Pending>,
    pub(super) pending_account: Option<crate::agents::endpoints::ExactAccount>,
    states: BTreeMap<(String, String, Feature), PairState>,
    pub(super) binding_controls: bool,
    pub(super) turn_binding: bool,
    thinking_models: BTreeSet<String>,
    pub(super) layers: StartLayers,
    pub(super) session: Vec<Given>,
    pub(super) kernel: KernelSettings,
    pub(super) withdraw_after: Option<Withdrawal>,
}

impl SettingsState {
    pub(super) fn new(session: &SessionConfig) -> Self {
        Self {
            start_effort: session.effort,
            max_tokens: session.max_tokens,
            pending: None,
            pending_account: None,
            states: BTreeMap::new(),
            binding_controls: false,
            turn_binding: false,
            thinking_models: BTreeSet::new(),
            layers: StartLayers::default(),
            session: Vec::new(),
            kernel: KernelSettings::default(),
            withdraw_after: None,
        }
    }

    pub(super) fn thought(&mut self, model: &str) {
        self.thinking_models.insert(model.to_owned());
    }

    pub(super) fn resumed(
        &mut self,
        start_effort: Effort,
        states: BTreeMap<(String, String, Feature), PairState>,
    ) {
        self.start_effort = start_effort;
        self.states = states;
    }

    pub(super) fn carried(&mut self, key: (String, String, Feature), state: PairState) {
        self.states.insert(key, state);
    }

    pub(super) fn hold_deferral(&mut self, endpoint: &str, model: &str) {
        let key = (
            endpoint.to_owned(),
            model.to_owned(),
            Feature::DeferredTools,
        );
        self.states.insert(key, PairState::Unknown);
    }
}

pub fn check_model_id(id: &str) -> Result<(), String> {
    wisp_protocol::check_model_id(id).map_err(|problem| problem.to_string())
}

pub(in crate::agent) fn state_record(
    feature: Feature,
    endpoint: &str,
    model: &str,
    state: FeatureState,
) -> LogEvent {
    let model = model.to_owned();
    let endpoint_name = Some(endpoint.to_owned());
    match feature {
        Feature::PerMessageEffort => LogEvent::PerMessageEffort {
            model,
            state,
            endpoint_name,
        },
        Feature::BindingControls => LogEvent::BindingControls {
            model,
            state,
            endpoint_name,
        },
        Feature::NativeCompaction => LogEvent::NativeCompaction {
            model,
            state,
            endpoint_name,
        },
        Feature::InlineTools => LogEvent::InlineTools {
            model,
            state,
            endpoint_name,
        },
        Feature::DeferredTools => LogEvent::DeferredTools {
            model,
            state,
            endpoint_name,
        },
        Feature::NativeWebSearch => LogEvent::NativeWebSearch {
            endpoint_name: endpoint.to_owned(),
            state,
        },
    }
}

pub(crate) fn state_event(
    feature: Feature,
    endpoint: &str,
    model: &str,
    state: FeatureState,
) -> AgentEvent {
    AgentEvent::FeatureStateChanged {
        endpoint: endpoint.to_owned(),
        model: model.to_owned(),
        feature,
        state,
    }
}

impl Agent {
    #[must_use]
    pub fn with_binding_controls(mut self, on: bool) -> Self {
        self.settings.binding_controls = on;
        self
    }

    #[must_use]
    pub fn with_start_layers(mut self, layers: StartLayers) -> Self {
        self.settings.layers = layers;
        self.configure_plan_usage();
        self.note_tool_baseline();
        self.note_deferred();
        self
    }

    pub(super) fn next_target(&self) -> (Arc<dyn Provider>, String) {
        match &self.settings.pending {
            Some(pending) => (
                self.provider_at(pending.endpoint).clone(),
                pending.model.clone(),
            ),
            None => (self.provider().clone(), self.session.model.clone()),
        }
    }

    pub fn pending_settings(&self) -> Option<(&str, &str, Effort)> {
        self.settings
            .pending
            .as_ref()
            .map(|p| (self.connection_name(p.endpoint), p.model.as_str(), p.effort))
    }

    pub fn pair_state(&self, endpoint: &str, model: &str, feature: Feature) -> PairState {
        let state = self
            .settings
            .states
            .get(&(endpoint.to_owned(), model.to_owned(), feature))
            .copied();
        if state == Some(PairState::FallenBack) {
            return PairState::FallenBack;
        }
        let documented = self
            .connections
            .iter()
            .find(|c| c.name == endpoint)
            .is_some_and(|c| c.provider.features_documented());
        if documented {
            return PairState::Documented;
        }
        state.unwrap_or(PairState::Unknown)
    }

    pub(super) fn restored_pairs(&self) -> BTreeMap<(String, String, Feature), PairState> {
        self.settings
            .states
            .keys()
            .map(|key @ (endpoint, model, feature)| {
                (key.clone(), self.pair_state(endpoint, model, *feature))
            })
            .collect()
    }

    pub(super) fn deferral_held(&self, endpoint: &str, model: &str) -> bool {
        let key = (
            endpoint.to_owned(),
            model.to_owned(),
            Feature::DeferredTools,
        );
        matches!(
            self.settings.states.get(&key),
            Some(PairState::Unknown | PairState::FallenBack)
        )
    }

    pub(super) fn feature_use(&self, model: &str, feature: Feature) -> Option<FeatureUse> {
        self.feature_use_on(
            self.provider().as_ref(),
            self.endpoint_name(),
            model,
            feature,
        )
    }

    pub(super) fn feature_use_on(
        &self,
        provider: &dyn Provider,
        endpoint: &str,
        model: &str,
        feature: Feature,
    ) -> Option<FeatureUse> {
        let applies = match feature {
            Feature::NativeCompaction => self.models.native_compaction(endpoint, model),
            Feature::PerMessageEffort => provider.capabilities(model)?.per_message_effort,
            Feature::InlineTools => provider.capabilities(model)?.inline_tools,
            Feature::DeferredTools => {
                let deferred = self.deferred_on(endpoint, model);
                return (!deferred.is_empty()).then_some(FeatureUse::Established);
            }
            Feature::BindingControls => {
                (self.settings.binding_controls || self.tools_moved().is_some())
                    && provider.capabilities(model)?.adaptive_thinking
            }
            Feature::NativeWebSearch => return None,
        };
        if !applies {
            return None;
        }
        match self.pair_state(endpoint, model, feature) {
            PairState::Documented | PairState::Proven => Some(FeatureUse::Established),
            PairState::Unknown => Some(FeatureUse::Trial),
            PairState::FallenBack => None,
        }
    }

    pub(super) fn binding_usable(&self, endpoint: &str, model: &str) -> bool {
        self.provider_for(Some(endpoint))
            .capabilities(model)
            .is_some_and(|row| row.adaptive_thinking)
            && self.pair_state(endpoint, model, Feature::BindingControls) != PairState::FallenBack
    }

    pub(super) fn binding_usable_now(&self) -> bool {
        self.binding_usable(self.endpoint_name(), &self.session.model)
    }

    pub(super) fn binding_from_start(&self) -> bool {
        self.connections.len() > 1 || self.binding_usable_now()
    }

    pub(super) fn keeps_thinking(&mut self) -> bool {
        let usable = self.binding_usable_now();
        if usable {
            self.settings.binding_controls = true;
            self.keep_break_through(self.history.len());
        }
        usable
    }

    pub(super) fn next_endpoint_model(&self) -> (usize, String) {
        match &self.settings.pending {
            Some(pending) => (pending.endpoint, pending.model.clone()),
            None => (self.endpoint, self.session.model.clone()),
        }
    }

    pub(super) fn takes_block(&self, endpoint: usize, model: &str) -> bool {
        self.feature_use_on(
            self.provider_at(endpoint).as_ref(),
            self.connection_name(endpoint),
            model,
            Feature::NativeCompaction,
        )
        .is_some()
    }

    pub(super) fn threading(&self) -> bool {
        let (session, features) = self.bare_request_parts();
        let request = Request {
            session: &session,
            tools: &[],
            deferred: &[],
            by_value: &[],
            server_tools: &[],
            messages: &[],
            previous_response_id: None,
            features: Features {
                native_compaction: None,
                ..features
            },
            options: self.request_options(),
            rendering: &UNRENDERED,
            compaction: None,
        };
        self.threads.usable(self.endpoint) && self.provider().threads(&request)
    }

    pub(super) fn request_parts(&self) -> (SessionConfig, Features) {
        let features = self.request_features();
        let mut session = self.session.clone();
        session.effort = self.request_effort(features);
        (session, features)
    }

    pub(super) fn bare_request_parts(&self) -> (SessionConfig, Features) {
        let features = self.request_features();
        let s = &self.session;
        let session = SessionConfig {
            model: s.model.clone(),
            system: None,
            effort: self.request_effort(features),
            cache_ttl: s.cache_ttl,
            max_tokens: s.max_tokens,
            message_time: s.message_time,
            instructions: None,
            context_note: s.context_note,
        };
        (session, features)
    }

    fn request_effort(&self, features: Features) -> Effort {
        if features.per_message_effort.is_some() {
            self.landing_effort(
                self.endpoint,
                &self.session.model,
                self.settings.start_effort,
            )
        } else {
            self.session.effort
        }
    }

    fn request_features(&self) -> Features {
        let model = &self.session.model;
        Features {
            per_message_effort: self.feature_use(model, Feature::PerMessageEffort),
            binding_controls: self.feature_use(model, Feature::BindingControls),
            native_compaction: self
                .head_holds_block()
                .then(|| self.feature_use(model, Feature::NativeCompaction))
                .flatten(),
            inline_tools: self.feature_use(model, Feature::InlineTools),
            deferred_tools: self.feature_use(model, Feature::DeferredTools),
            documented: self.provider().features_documented(),
            thread: None,
        }
    }

    pub(super) fn set_pending(&mut self, pending: Option<Pending>) {
        self.settings.pending_account = pending
            .as_ref()
            .filter(|next| next.endpoint != self.endpoint || self.child_resume.unbound())
            .and_then(|next| {
                self.pending_account_for(next.endpoint)
                    .cloned()
                    .or_else(|| {
                        self.connections
                            .get(next.endpoint)
                            .map(crate::agents::endpoints::ExactAccount::connected)
                    })
            });
        self.settings.pending = pending;
        self.publish_entry(Some(self.pending_displacement()));
    }

    pub(super) fn pending_account_for(
        &self,
        endpoint: usize,
    ) -> Option<&crate::agents::endpoints::ExactAccount> {
        self.settings
            .pending
            .as_ref()
            .filter(|pair| pair.endpoint == endpoint)?;
        self.settings.pending_account.as_ref()
    }

    pub(crate) fn pending_displacement(
        &self,
    ) -> Option<wisp_protocol::subscription_auth::WayOrigin> {
        let Some(pair) = self.settings.pending.as_ref() else {
            return self
                .binding_detached
                .then(|| self.way_origin(&self.session_pair()));
        };
        (self.bound_endpoint().is_none()
            || pair.endpoint != self.endpoint
            || pair.model != self.session.model
            || pair.effort != self.session.effort)
            .then(|| self.way_origin(pair))
    }

    pub(super) fn way_origin(&self, pair: &Pending) -> wisp_protocol::subscription_auth::WayOrigin {
        let provider = self.provider_at(pair.endpoint);
        let accepted = self
            .pending_account_for(pair.endpoint)
            .or_else(|| self.accepted_account(self.connection_name(pair.endpoint)));
        wisp_protocol::subscription_auth::WayOrigin {
            account: self.connection_name(pair.endpoint).to_owned(),
            provider: accepted
                .map_or(Some(provider.kind().as_str()), |a| a.kind.shown())
                .map(str::to_owned),
            billing: accepted.map_or(Some(provider.billing()), |a| a.billing),
            model: pair.model.clone(),
            effort: pair.effort,
        }
    }

    pub(super) fn apply_pending_binding(&mut self) -> Result<(), String> {
        if let Some(pending) = self.settings.pending.clone() {
            let binding = self.reserve_binding(pending.endpoint)?;
            self.commit_pending_binding(pending, binding, None);
        }
        Ok(())
    }

    pub(super) fn commit_pending_binding(
        &mut self,
        pending: Pending,
        binding: Option<crate::endpoints::generation::Binding>,
        selected: Option<crate::agents::endpoints::ExactAccount>,
    ) {
        let switching = pending.endpoint != self.endpoint;
        let moved = switching || pending.model != self.session.model;
        self.adopt_binding(pending.endpoint, binding, selected);
        self.settings.binding_controls |= switching;
        self.pending_endpoint_binding = None;
        self.session.model = pending.model;
        self.session.effort = pending.effort;
        self.set_pending(None);
        if moved {
            self.search.array = None;
            self.search.pickup = false;
        }
    }

    pub(super) async fn bind_unbound(
        &mut self,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Result<(), String> {
        if self.child_resume.unbound() {
            let selected = self.resumed_selection_is(self.next_endpoint());
            self.apply_pending_binding()?;
            if self.child_resume.unbound() {
                self.use_connection(self.endpoint)?;
            }
            self.settle_entry();
            if selected {
                self.log_compaction_account(events).await;
            }
        }
        Ok(())
    }

    pub(super) async fn log_compaction_account(&mut self, events: &mpsc::Sender<AgentEvent>) {
        let turn = self.running_turn().unwrap_or(self.turns + 1);
        self.log(
            events,
            LogEvent::RebuildMarked {
                turn,
                reason: crate::resume::RESUMED_COMPACTION_ACCOUNT.to_owned(),
            },
        )
        .await;
    }

    pub(super) fn begin_turn_settings(&mut self) -> Result<Option<EffortMarker>, String> {
        self.apply_pending_binding()?;
        if matches!(self.child_resume, crate::kit::ChildResume::Pending { .. }) {
            self.child_resume = crate::kit::ChildResume::Legacy;
        }
        self.begin_kernel_turn();
        Ok(self.turn_limits())
    }

    pub(super) fn hold_settings_after_retry(&mut self) {
        if self.settings.pending.is_none() {
            self.set_pending(Some(self.session_pair()));
        }
    }

    pub(super) fn begin_retry_settings(
        &mut self,
        retry: &Continuation,
        opens: bool,
    ) -> Result<Option<EffortMarker>, String> {
        let next = self.next_pair();
        self.check_live_account(&retry.account, retry.endpoint)?;
        let binding = self.reserve_generation(retry.endpoint)?;
        if retry.endpoint != self.endpoint {
            self.search.array = None;
            self.search.pickup = false;
        }
        self.adopt_binding(retry.endpoint, binding, Some(retry.account.clone()));
        let unchanged = next.endpoint == self.endpoint
            && next.model == self.session.model
            && next.effort == self.session.effort;
        self.set_pending((!unchanged).then_some(next));
        if self.settings.pending.is_none() {
            self.pending_endpoint_binding = None;
        }
        let marker = self.turn_limits();
        let corrective = !retry.same_pair
            && marker.is_some_and(|m| m.rendered)
            && self.effort_marker_in_effect(true).is_some_and(|m| {
                self.provider()
                    .capabilities(&self.session.model)
                    .is_some_and(|row| !row.takes_effort(m.level))
            });
        Ok(marker.filter(|_| opens || corrective))
    }

    pub(super) fn next_endpoint(&self) -> usize {
        self.settings
            .pending
            .as_ref()
            .map_or(self.endpoint, |p| p.endpoint)
    }

    pub(super) async fn switch_for_retry(
        &mut self,
        target: Pending,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        if self.compaction.background.running() {
            let turn = Some(self.turns.saturating_add(1));
            self.discard_background(wisp_protocol::DiscardReason::Superseded, turn, events)
                .await;
        }
        if let Some(connection) = self.connections.get(target.endpoint) {
            let exact = crate::agents::endpoints::ExactAccount::connected(connection);
            self.child_resume.reselect(exact);
        }
        let cache = self.pending_cache(&target);
        let account_bound = self.account_bound(target.endpoint);
        let unchanged = target.endpoint == self.endpoint
            && target.model == self.session.model
            && target.effort == self.session.effort;
        self.set_pending((!unchanged).then(|| target.clone()));
        self.pending_endpoint_binding = None;
        self.settings_changed(
            target,
            cache,
            account_bound,
            None,
            Default::default(),
            events,
        )
        .await;
    }

    fn turn_limits(&mut self) -> Option<EffortMarker> {
        self.session.max_tokens = self.max_tokens_on(self.endpoint, &self.session.model);
        self.turn_marker()
    }

    pub(super) fn max_tokens_on(&self, endpoint: usize, model: &str) -> u32 {
        let documented = self.provider_at(endpoint).capabilities(model);
        let documented = documented.and_then(|r| r.max_output);
        self.models.reply_limit(
            self.connection_name(endpoint),
            model,
            documented,
            self.settings.max_tokens,
        )
    }

    pub(super) fn turn_marker(&self) -> Option<EffortMarker> {
        let renders = self
            .feature_use(&self.session.model, Feature::PerMessageEffort)
            .is_some();
        let in_effect = self
            .effort_marker_in_effect(renders)
            .map_or(self.settings.start_effort, |m| m.level);
        (self.session.effort != in_effect).then_some(EffortMarker {
            level: self.session.effort,
            rendered: renders,
        })
    }

    fn effort_marker_in_effect(&self, renders: bool) -> Option<EffortMarker> {
        self.history
            .iter()
            .skip(self.view_from())
            .rev()
            .filter_map(Message::effort_marker)
            .find(|m| m.rendered || !renders)
    }

    pub(super) fn summary_marker(&self, view: &[Message], wanted: Effort) -> Option<EffortMarker> {
        let proven = self.feature_use(&self.session.model, Feature::PerMessageEffort)
            == Some(FeatureUse::Established);
        let in_force = view
            .iter()
            .rev()
            .filter_map(Message::effort_marker)
            .find(|m| m.rendered)
            .map_or(self.settings.start_effort, |m| m.level);
        (proven && in_force > wanted).then_some(EffortMarker {
            level: wanted,
            rendered: true,
        })
    }

    pub(super) fn cache_effect(&self, endpoint: usize, model: &str, effort: Effort) -> CacheEffect {
        let from = (
            self.endpoint,
            self.session.model.as_str(),
            self.session.effort,
        );
        self.cache_effect_between(from, (endpoint, model, effort))
    }

    pub(super) fn cache_effect_between(
        &self,
        from: (usize, &str, Effort),
        to: (usize, &str, Effort),
    ) -> CacheEffect {
        let (endpoint, model, effort) = to;
        if endpoint != from.0 {
            return CacheEffect::Rebuilt {
                reason: RebuildReason::EndpointSwitch,
            };
        }
        if model != from.1 {
            return CacheEffect::Rebuilt {
                reason: RebuildReason::ModelSwitch,
            };
        }
        if effort == from.2 {
            return CacheEffect::Kept;
        }
        let provider = self.provider_at(endpoint);
        let per_message = provider
            .capabilities(model)
            .is_some_and(|r| r.per_message_effort);
        match (
            per_message,
            self.pair_state(
                self.connection_name(endpoint),
                model,
                Feature::PerMessageEffort,
            ),
        ) {
            (true, PairState::Documented | PairState::Proven) => CacheEffect::Kept,
            (true, PairState::Unknown) => CacheEffect::KeptIfPassedOn,
            _ => CacheEffect::Rebuilt {
                reason: RebuildReason::EffortChange,
            },
        }
    }

    pub(super) fn next_effort(&self) -> Effort {
        self.settings
            .pending
            .as_ref()
            .map_or(self.session.effort, |p| p.effort)
    }

    pub(super) fn start_effort(&self) -> Effort {
        self.settings.start_effort
    }

    pub(super) fn find_endpoint(&self, name: &str) -> Result<usize, String> {
        if let Some((_, reason)) = self.gone.iter().find(|(n, _)| n == name) {
            return Err(reason.clone());
        }
        if let Some(index) = self.connections.iter().position(|c| c.name == name) {
            return Ok(index);
        }
        if let Some((_, reason)) = self.unavailable.iter().find(|(n, _)| n == name) {
            return Err(crate::endpoints::unavailable_words(
                &EndpointName::parse(name).unwrap_or_else(EndpointName::env),
                reason,
            ));
        }
        let mut available: Vec<EndpointName> = self
            .connections
            .iter()
            .filter_map(|c| EndpointName::parse(&c.name))
            .collect();
        available.sort();
        Err(unknown_endpoint(
            EndpointName::parse(name).as_ref(),
            &available,
        ))
    }

    pub(super) fn runs_on(&self, endpoint: usize, from: &Pending) -> String {
        self.lands_on(endpoint, from, None).model
    }

    fn same_landing(&self, endpoint: usize, from: &Pending) -> bool {
        endpoint == from.endpoint
            && self.check_resume_destination(endpoint).is_ok()
            && !(self.child_resume.unbound()
                && self
                    .provider_at(endpoint)
                    .complete_model_list()
                    .is_some_and(|list| list.find(&from.model).is_none()))
    }

    fn lands_on(&self, endpoint: usize, from: &Pending, last: Option<&Remembered>) -> Landed {
        let entries = self.model_entries();
        let listing = self.account_listing(self.connection_name(endpoint));
        wisp_protocol::settings::lands_on(
            &wisp_protocol::settings::offers(&entries),
            listing.landing(self.provider_at(endpoint).kind().as_str()),
            self.same_landing(endpoint, from),
            (chat(&from.model), Some(from.effort)),
            last,
        )
    }

    fn remembered_pair(
        &self,
        base: &Pending,
        endpoint: usize,
        effort: Option<Effort>,
    ) -> Option<Pending> {
        let last = self
            .settings
            .layers
            .remembered
            .of(self.connection_name(endpoint));
        let landed = self.lands_on(endpoint, base, last.as_ref());
        if !landed.remembered {
            return None;
        }
        let held = Pending {
            endpoint: base.endpoint,
            model: landed.model,
            effort: landed.effort.unwrap_or(base.effort),
        };
        if self.runs_on(endpoint, &held) != held.model {
            return None;
        }
        self.validate_pair(held, endpoint, None, effort).ok()
    }

    pub(super) fn landing_effort(&self, endpoint: usize, model: &str, inherited: Effort) -> Effort {
        let effort = self
            .provider_at(endpoint)
            .capabilities(model)
            .and_then(|row| wisp_protocol::settings::effort_on(row.efforts, Some(inherited)))
            .unwrap_or(inherited);
        crate::models::ModelTable::shared().own_effort(model, effort)
    }

    #[cfg(test)]
    fn validate_settings(
        &self,
        endpoint: Option<String>,
        model: Option<String>,
        effort: Option<Effort>,
    ) -> Result<Pending, String> {
        let (target, check_model) = self.proposed_settings(endpoint, model, effort)?;
        self.checked_pair(target, check_model, effort)
    }

    fn proposed_settings(
        &self,
        endpoint: Option<String>,
        model: Option<String>,
        effort: Option<Effort>,
    ) -> Result<(Pending, bool), String> {
        let base = self.next_pair();
        let endpoint = match &endpoint {
            Some(name) => self.find_endpoint(name)?,
            None => base.endpoint,
        };
        if let Some(reason) = self.child_move_refused(endpoint) {
            return Err(reason);
        }
        if model.is_none()
            && let Some(pair) = self.remembered_pair(&base, endpoint, effort)
        {
            return Ok((pair, false));
        }
        self.proposed_pair(base, endpoint, model, effort)
    }

    pub(super) fn validate_pair(
        &self,
        base: Pending,
        endpoint: usize,
        model: Option<String>,
        effort: Option<Effort>,
    ) -> Result<Pending, String> {
        let (target, check_model) = self.proposed_pair(base, endpoint, model, effort)?;
        self.checked_pair(target, check_model, effort)
    }

    pub(super) fn proposed_pair(
        &self,
        base: Pending,
        endpoint: usize,
        model: Option<String>,
        effort: Option<Effort>,
    ) -> Result<(Pending, bool), String> {
        if let Some(id) = &model {
            check_model_id(id)?;
        }
        let named = model.is_some();
        let moved = !self.same_landing(endpoint, &base);
        let target = model.unwrap_or_else(|| self.runs_on(endpoint, &base));
        let mapped =
            effort.and_then(|level| crate::models::ModelTable::shared().sibling(&target, level));
        let named = named || mapped.is_some_and(|id| id != target);
        let target = mapped.unwrap_or(&target).to_owned();
        let level = match effort {
            None if !named && moved => self.landing_effort(endpoint, &target, base.effort),
            _ => effort.unwrap_or(base.effort),
        };
        Ok((
            Pending {
                endpoint,
                model: target,
                effort: level,
            },
            named || moved,
        ))
    }

    pub(super) fn checked_pair(
        &self,
        mut target: Pending,
        check_model: bool,
        effort: Option<Effort>,
    ) -> Result<Pending, String> {
        let provider = self.provider_at(target.endpoint);
        if check_model {
            crate::agents::endpoints::provider_mismatch(provider.as_ref(), &target.model)?;
            crate::agents::endpoints::check_model(
                provider.as_ref(),
                self.connection_name(target.endpoint),
                &target.model,
            )
            .map_err(|e| e.to_string())?;
        }
        if let Some(row) = provider.capabilities(&target.model) {
            row.check_selectable(provider.kind(), &target.model)?;
            if !row.accepts_effort(effort, target.effort) {
                return Err(wisp_protocol::settings::effort_refusal(
                    &target.model,
                    target.effort,
                    effort.is_some(),
                    row.efforts,
                ));
            }
        }
        target.effort =
            crate::models::ModelTable::shared().own_effort(&target.model, target.effort);
        Ok(target)
    }

    fn account_bound(&self, endpoint: usize) -> Vec<String> {
        if endpoint == self.endpoint {
            return Vec::new();
        }
        let provider = self.provider_at(endpoint);
        self.settings
            .thinking_models
            .iter()
            .filter(|m| provider.account_bound_thinking(m))
            .cloned()
            .collect()
    }

    pub async fn change_settings(
        &mut self,
        endpoint: Option<String>,
        model: Option<String>,
        effort: Option<Effort>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        self.start_log(events).await;
        let changes = wisp_protocol::ClientCommand::shortcut_changes(endpoint, model, effort);
        self.enqueue(
            super::queue::Queued::Change {
                request: None,
                changes,
                save: false,
                projected: Projected::default(),
                by: None,
                answer: None,
            },
            events,
        )
        .await;
    }

    pub(super) async fn connection_replaced(&mut self, events: &mpsc::Sender<AgentEvent>) {
        self.epoch.rebuilt(super::epoch::MarkReason::EndpointSwitch);
        if self.next_endpoint() != self.endpoint || self.bound_endpoint().is_none() {
            self.log_mark(events).await;
            return;
        }
        let account_bound = self
            .settings
            .thinking_models
            .iter()
            .filter(|model| self.provider().account_bound_thinking(model))
            .cloned()
            .collect();
        self.settings_changed(
            self.next_pair(),
            CacheEffect::Rebuilt {
                reason: RebuildReason::EndpointSwitch,
            },
            account_bound,
            None,
            (Vec::new(), Vec::new()),
            events,
        )
        .await;
        self.log_mark(events).await;
    }

    async fn settings_changed(
        &mut self,
        target: Pending,
        cache: CacheEffect,
        account_bound: Vec<String>,
        answers: Option<RequestId>,
        changes: (Vec<AppliedChange>, Vec<LoggedChange>),
        events: &mpsc::Sender<AgentEvent>,
    ) {
        self.settings_event(target, cache, account_bound, answers, changes, None, events)
            .await;
    }

    pub(super) async fn finish_withdrawal(&mut self, events: &mpsc::Sender<AgentEvent>) {
        if let Some(why) = self.settings.withdraw_after.take() {
            self.withdraw_switch(why, events).await;
        }
    }

    pub(super) async fn withdraw_switch(
        &mut self,
        why: Withdrawal,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        if self.bound_endpoint().is_none() {
            return;
        }
        let wanted = self
            .settings
            .pending
            .as_ref()
            .map_or(self.session.effort, |p| p.effort);
        let takes = self
            .provider()
            .capabilities(&self.session.model)
            .is_none_or(|row| row.accepts_effort(None, wanted));
        let effort = if takes { wanted } else { self.session.effort };
        let effort = crate::models::ModelTable::shared().own_effort(&self.session.model, effort);
        let target = Pending {
            endpoint: self.endpoint,
            model: self.session.model.clone(),
            effort,
        };
        self.set_pending((effort != self.session.effort).then(|| target.clone()));
        self.pending_endpoint_binding = None;
        let mut restated = vec![
            (
                names::MODEL,
                SettingValue::Text(self.session.model.clone()),
                CacheEffect::Kept,
            ),
            (
                names::ENDPOINT,
                SettingValue::Text(self.endpoint_name().to_owned()),
                CacheEffect::Kept,
            ),
        ];
        if !takes || effort != wanted {
            restated.push((
                names::EFFORT,
                SettingValue::Text(self.session.effort.as_str().to_owned()),
                CacheEffect::Kept,
            ));
        }
        for (name, value, _) in &restated {
            self.restate_session(name, value.clone());
        }
        let options = self
            .pending_option_values()
            .into_iter()
            .map(|(name, value, reason)| (name, value, CacheEffect::Rebuilt { reason }));
        let (applied, logged): (Vec<AppliedChange>, Vec<LoggedChange>) = restated
            .into_iter()
            .chain(options)
            .filter_map(|(name, value, cache)| {
                let spec = wisp_protocol::settings::find(name)?;
                let change = AppliedChange {
                    target: SettingTarget::Name(wisp_protocol::settings::SettingName::of(spec)),
                    value: Some(value),
                    layer: crate::settings::Layer::Session.wire(),
                    when: When::NextTurn,
                    cache: Some(cache),
                    consequence: None,
                    saved: None,
                    overridden: None,
                    warnings: Vec::new(),
                };
                let logged = LoggedChange::of(&change, None);
                Some((change, logged))
            })
            .unzip();
        let cache = self.pending_cache(&target);
        let restored = Restored {
            effort: self.session.effort,
            cache_ttl: self.session.cache_ttl,
        };
        self.settings_event(
            target,
            cache,
            Vec::new(),
            None,
            (applied, logged),
            Some((why, restored)),
            events,
        )
        .await;
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one record and its event share every field"
    )]
    async fn settings_event(
        &mut self,
        target: Pending,
        cache: CacheEffect,
        account_bound: Vec<String>,
        answers: Option<RequestId>,
        (changes, logged): (Vec<AppliedChange>, Vec<LoggedChange>),
        withdrawal: Option<(Withdrawal, Restored)>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let (withdrawn, restored) = withdrawal.unzip();
        let endpoint = self.connection_name(target.endpoint).to_owned();
        let (tools, tools_sha256) = self.tools_to_log(&endpoint, &target.model);
        let context = self.connections.get(target.endpoint).map(|c| &c.context);
        let record = LogEvent::SettingsChanged {
            model: target.model.clone(),
            effort: target.effort,
            cache: cache.clone(),
            endpoint_name: Some(endpoint.clone()),
            provider: Some(self.provider_at(target.endpoint).kind().as_str().to_owned()),
            endpoint: context.map(|c| c.endpoint.clone()),
            credential_source: context.map(|c| c.credential_source.clone()),
            account_bound: account_bound.clone(),
            tools,
            tools_sha256,
            changes: logged,
            withdrawn: withdrawn.clone(),
        };
        self.log(events, record).await;
        self.refresh_plan_usage_for(&endpoint, &target.model);
        emit(
            events,
            AgentEvent::SettingsChanged {
                answers,
                endpoint,
                model: target.model,
                effort: target.effort,
                cache,
                account_bound,
                changes,
                withdrawn,
                restored,
            },
        )
        .await;
    }

    pub(super) async fn report_search_refused(&mut self, events: &mpsc::Sender<AgentEvent>) {
        let target = self.next_pair();
        let cache = CacheEffect::Rebuilt {
            reason: RebuildReason::NativeSearchRefused,
        };
        self.settings_changed(
            target,
            cache,
            Vec::new(),
            None,
            (Vec::new(), Vec::new()),
            events,
        )
        .await;
    }

    pub(super) fn falls_back(
        &self,
        features: Features,
        rejected: &[Feature],
        spent: &[Feature],
    ) -> Vec<Feature> {
        rejected
            .iter()
            .copied()
            .filter(|f| features.get(*f).is_some() && !spent.contains(f))
            .collect()
    }

    pub(super) async fn fall_back(
        &mut self,
        rejected: &[Feature],
        marker: &mut Option<EffortMarker>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let pair = (self.endpoint_name().to_owned(), self.session.model.clone());
        self.fall_back_on(pair, rejected, marker, events).await;
    }

    pub(super) async fn fall_back_on(
        &mut self,
        (endpoint, model): (String, String),
        rejected: &[Feature],
        marker: &mut Option<EffortMarker>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        for feature in rejected {
            self.settings.states.insert(
                (endpoint.clone(), model.clone(), *feature),
                PairState::FallenBack,
            );
            self.log(
                events,
                state_record(*feature, &endpoint, &model, FeatureState::FallenBack),
            )
            .await;
            emit(
                events,
                state_event(*feature, &endpoint, &model, FeatureState::FallenBack),
            )
            .await;
        }
        if rejected.contains(&Feature::InlineTools) {
            self.inline_fell_back();
        }
        if !rejected.contains(&Feature::PerMessageEffort) {
            return;
        }
        let dropped = marker.as_mut().filter(|m| m.rendered).is_some_and(|m| {
            m.rendered = false;
            true
        });
        let correction = match self.settings.pending.clone() {
            Some(pending)
                if self.connection_name(pending.endpoint) != endpoint || pending.model != model =>
            {
                None
            }
            Some(pending) => Some(pending),
            None => dropped.then_some(Pending {
                endpoint: self.endpoint,
                model,
                effort: self.session.effort,
            }),
        };
        if let Some(target) = correction {
            let cache = CacheEffect::Rebuilt {
                reason: RebuildReason::PerMessageEffortRejected,
            };
            self.settings_changed(target, cache, Vec::new(), None, Default::default(), events)
                .await;
        }
    }

    pub(super) async fn prove(
        &mut self,
        features: Features,
        messages: &[Message],
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let model = self.session.model.clone();
        let endpoint = self.endpoint_name().to_owned();
        let marker_sent = messages
            .iter()
            .filter_map(Message::effort_marker)
            .any(|m| m.rendered);
        let rendering = self.kept_rendering(messages.len());
        let changes_sent = crate::provider::carries_tool_changes(messages, &rendering, |block| {
            self.provider().tool_changes(block)
        });
        for feature in pair_features() {
            let proven = match feature {
                Feature::PerMessageEffort => marker_sent,
                Feature::InlineTools => changes_sent,
                Feature::DeferredTools | Feature::NativeWebSearch => false,
                Feature::BindingControls | Feature::NativeCompaction => true,
            };
            if proven && features.get(feature) == Some(FeatureUse::Trial) {
                self.settings.states.insert(
                    (endpoint.clone(), model.clone(), feature),
                    PairState::Proven,
                );
                let held = feature == Feature::InlineTools
                    && !self.deferred_on(&endpoint, &model).is_empty();
                if feature == Feature::InlineTools {
                    self.settings.states.insert(
                        (endpoint.clone(), model.clone(), Feature::DeferredTools),
                        PairState::Unknown,
                    );
                }
                let record = state_record(feature, &endpoint, &model, FeatureState::Proven);
                self.log(events, record).await;
                if held {
                    let hold = state_record(
                        Feature::DeferredTools,
                        &endpoint,
                        &model,
                        FeatureState::FallenBack,
                    );
                    self.log(events, hold).await;
                }
                let event = state_event(feature, &endpoint, &model, FeatureState::Proven);
                emit(events, event).await;
            }
        }
    }
}

#[cfg(test)]
mod live;
#[cfg(test)]
mod tests;
