use tokio::sync::mpsc;
use wisp_protocol::log::{BlockPosition, Cut, FeatureState, LogEvent};
use wisp_protocol::{
    AgentEvent, Feature, HistoryChange, Message, RenderingForm, ToolChangeVia, UnfinishedReason,
};

use super::epoch::MarkReason;
use super::forms::echoed_positions;
use super::search::{Probe, WHOLE, holds_thinking_from};
use super::settings::{state_event, state_record};
use super::{Agent, emit};
use crate::endpoints::ProviderKind;
use crate::provider::forms::{Layout, is_thinking, native, renders, server_call, server_result};
use crate::provider::{Rendering, ServerToolSpec};
use crate::search::{NoSearch, SearchFacts, SearchForm};
use crate::session::sha256_hex;
use crate::tool::{ToolSpec, array_json, tool_names, tools_sha256};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ToolSet {
    pub(super) names: Vec<String>,
    pub(super) digest: String,
    pub(super) sha256: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SearchRefusal {
    Resend,
    EndTurn,
}

fn first_native(message: &Message) -> Option<usize> {
    message
        .content
        .iter()
        .position(|b| server_call(b).is_some() || server_result(b).is_some())
}

impl Agent {
    pub fn search_facts(&self, endpoint: &str, model: &str) -> SearchFacts {
        let wire = self.wire_of(endpoint).unwrap_or_default();
        let listed = self.listed_row(endpoint, model);
        SearchFacts::resolve(&self.models, endpoint, model, wire, listed.as_ref())
    }

    pub(super) fn wire_of(&self, endpoint: &str) -> Option<ProviderKind> {
        let connection = self.connections.iter().find(|c| c.name == endpoint)?;
        Some(connection.provider.kind())
    }

    pub(super) fn search_tools(
        &self,
        endpoint: &str,
        model: &str,
        form: SearchForm,
    ) -> &'static [ServerToolSpec] {
        self.search_facts(endpoint, model).server_tools(
            form,
            self.toolbox.runner(crate::search::WEB_SEARCH).is_some(),
        )
    }

    pub fn search_form(&self, endpoint: &str, model: &str) -> SearchForm {
        self.unkept_form(endpoint, model, self.toolbox.web_search())
            .kept(
                self.search
                    .kept
                    .get(&(endpoint.to_owned(), model.to_owned()))
                    .copied(),
            )
    }

    pub(super) fn unkept_form(&self, endpoint: &str, model: &str, requested: bool) -> SearchForm {
        let probing = self
            .search
            .probe
            .as_ref()
            .is_some_and(|p| p.endpoint == endpoint);
        self.search_facts(endpoint, model)
            .form(requested, probing || self.search.refused.contains(endpoint))
    }

    pub(super) fn flip_search(&mut self, wanted: bool) {
        self.toolbox.set_web_search(wanted);
        if wanted {
            self.search.kept.clear();
        }
    }

    pub(super) fn search_waits(&self) -> bool {
        self.search_form(self.endpoint_name(), &self.session.model)
            == SearchForm::None(NoSearch::NotStarted)
    }

    pub(super) fn server_tools(&self) -> &'static [ServerToolSpec] {
        let (endpoint, model) = (self.endpoint_name(), &self.session.model);
        self.search_tools(endpoint, model, self.search_form(endpoint, model))
    }

    pub(super) fn wire<'a>(
        &self,
        specs: &[ToolSpec],
        entries: &'a [ServerToolSpec],
    ) -> (Vec<ToolSpec>, &'a [ServerToolSpec]) {
        crate::search::wire(
            specs,
            entries,
            self.toolbox
                .runner(crate::search::WEB_SEARCH)
                .map(|runner| runner.spec()),
        )
    }

    pub(super) fn tool_set(&self, endpoint: &str, model: &str) -> ToolSet {
        let deferred = self.deferred_on(endpoint, model);
        let server = self.search_tools(endpoint, model, self.search_form(endpoint, model));
        self.tools_with(server, deferred)
    }

    fn tools_with(&self, server: &[ServerToolSpec], deferred: &[ToolSpec]) -> ToolSet {
        let (specs, server) = self.wire(&self.sent, server);
        ToolSet {
            names: tool_names(&specs, server),
            digest: sha256_hex(&array_json(&specs, deferred, server)),
            sha256: tools_sha256(&specs, deferred, server),
        }
    }

    fn pair_deferred(&self) -> &[ToolSpec] {
        self.deferred_on(self.endpoint_name(), &self.session.model)
    }

    fn kept_tools(&self) -> Option<&'static [ServerToolSpec]> {
        self.open_track().and_then(|t| t.kept_tools)
    }

    pub(super) fn step_server_tools(&self) -> &'static [ServerToolSpec] {
        self.kept_tools().unwrap_or_else(|| self.server_tools())
    }

    pub(super) fn current_tools(&self) -> ToolSet {
        match self.kept_tools() {
            Some(kept) => self.tools_with(kept, self.pair_deferred()),
            None => self.pair_tools(),
        }
    }

    pub(super) fn pair_tools(&self) -> ToolSet {
        self.tool_set(self.endpoint_name(), &self.session.model)
    }

    pub(super) fn keep_for_retry(&mut self, failed: &'static [ServerToolSpec]) -> Option<String> {
        let refused = self.search.refused.contains(self.endpoint_name());
        if !refused || failed == self.server_tools() {
            return None;
        }
        self.track().kept_tools = Some(failed);
        Some(self.tools_with(failed, self.pair_deferred()).digest)
    }

    pub(super) fn end_kept_for_overflow(&mut self) {
        if self.kept_tools().is_none() {
            return;
        }
        if self.search.built_with.is_none() {
            self.search.built_with = Some(self.current_tools().digest);
        }
        self.end_kept_tools();
    }

    pub(super) fn end_kept_tools(&mut self) -> bool {
        self.open_track_mut()
            .is_some_and(|t| t.kept_tools.take().is_some())
    }

    pub(super) async fn close_kept_tail(&mut self, events: &mpsc::Sender<AgentEvent>) {
        if self.end_kept_tools() && self.tail_live() {
            let billed = self.track().carried_billed.clone();
            self.close_tail(UnfinishedReason::Refused, &billed, events)
                .await;
        }
    }

    pub(super) fn tools_to_log(
        &mut self,
        endpoint: &str,
        model: &str,
    ) -> (Vec<String>, Option<String>) {
        let set = self.tool_set(endpoint, model);
        if self.search.logged_tools.as_deref() == Some(set.digest.as_str()) {
            return (Vec::new(), None);
        }
        self.search.logged_tools = Some(set.digest.clone());
        (set.names, Some(set.digest))
    }

    pub(super) fn tools_moved(&self) -> Option<String> {
        let built = self.search.built_with.as_deref()?;
        let now = self.current_tools().digest;
        (built != now).then_some(now)
    }

    pub(super) fn keep_break_through(&mut self, until: usize) {
        self.search.kept_break = self.search.kept_break.max(Some(until));
    }

    pub(super) fn carries_kept_break(&self) -> bool {
        let Some(until) = self.search.kept_break else {
            return false;
        };
        let (start, until) = match self.compaction.head.as_ref() {
            Some(h) if h.leading() => (h.cut, until),
            Some(h) => (h.from, until),
            None => (0, until),
        };
        let from = BlockPosition {
            message: start,
            block: 0,
        };
        until > start && holds_thinking_from(&self.history, from, until - 1)
    }

    pub(super) fn whole_cut(&self) -> Option<(Cut, Option<String>)> {
        if self.binding_usable_now() {
            return None;
        }
        let moved = self.tools_moved();
        (moved.is_some() || self.carries_kept_break()).then(|| {
            let cut = Cut {
                from: WHOLE,
                through: self.history.len().saturating_sub(1),
                tools: moved.is_some(),
            };
            (cut, moved)
        })
    }

    pub(super) async fn keep_tools(&mut self, events: &mpsc::Sender<AgentEvent>) {
        if self.tools_moved().is_some() {
            self.keeps_thinking();
        }
        if let Some((cut, tools_sha256)) = self.whole_cut() {
            self.record_cut(cut, tools_sha256, events).await;
            self.search.kept_break = None;
        }
        self.search.built_with = Some(self.current_tools().digest);
    }

    pub(super) async fn search_not_run(&mut self, model: &str, events: &mpsc::Sender<AgentEvent>) {
        let endpoint = self.endpoint_name().to_owned();
        if !self.search.not_run.insert(endpoint.clone()) {
            return;
        }
        let event = AgentEvent::SearchNotRun {
            endpoint_name: endpoint,
            model: model.to_owned(),
        };
        self.emit(events, event).await;
    }

    pub(super) fn note_sent_tools(&mut self) {
        self.search.sent_tools = Some(self.pair_tools().names);
    }

    pub(super) async fn announce_tools(&mut self, events: &mpsc::Sender<AgentEvent>) {
        let Some(sent) = self.search.sent_tools.clone() else {
            return;
        };
        let now = self.pair_tools().names;
        let added: Vec<String> = now.iter().filter(|n| !sent.contains(n)).cloned().collect();
        let removed: Vec<String> = sent.iter().filter(|n| !now.contains(n)).cloned().collect();
        if added.is_empty() && removed.is_empty() {
            return;
        }
        self.search.sent_tools = Some(now);
        self.tools_changed(
            added,
            removed,
            ToolChangeVia::Rebuild,
            None,
            Box::default(),
            events,
        )
        .await;
    }

    pub(super) async fn refuse_search(
        &mut self,
        events: &mpsc::Sender<AgentEvent>,
    ) -> SearchRefusal {
        self.mark_search_refused(events).await;
        self.close_live_tail(events).await
    }

    async fn close_live_tail(&mut self, events: &mpsc::Sender<AgentEvent>) -> SearchRefusal {
        if !self.tail_live() {
            return SearchRefusal::Resend;
        }
        let paused = self.paused_tail();
        let billed = self.search.failed_tail.take().unwrap_or_else(|| {
            self.track
                .as_ref()
                .map(|t| t.carried_billed.clone())
                .unwrap_or_default()
        });
        self.close_tail(UnfinishedReason::Refused, &billed, events)
            .await;
        if paused {
            SearchRefusal::EndTurn
        } else {
            SearchRefusal::Resend
        }
    }

    pub(super) fn start_probe(&mut self) -> bool {
        let endpoint = self.endpoint_name().to_owned();
        if self.search.probe.is_some()
            || crate::search::sent(self.step_server_tools()).is_empty()
            || self.tail_live()
        {
            return false;
        }
        let built_with = self.search.built_with.clone();
        if built_with.is_none() {
            self.search.built_with = Some(self.current_tools().digest);
        }
        self.search.probe = Some(Probe {
            endpoint,
            built_with,
            epoch: self.epoch.clone(),
        });
        true
    }

    pub(super) fn probing(&self) -> bool {
        self.search.probe.is_some()
    }

    pub(super) fn end_probe(&mut self) {
        if let Some(probe) = self.search.probe.take() {
            self.search.built_with = probe.built_with;
            self.epoch = probe.epoch;
            self.note_sent_tools();
        }
    }

    pub(super) fn keep_recovery_in_probe(&mut self) {
        if let Some(probe) = self.search.probe.as_mut() {
            probe.epoch.clear();
        }
    }

    pub(super) fn mark_changed_from(&mut self, message: usize, reason: MarkReason) {
        self.epoch.changed_from(message, reason);
        if let Some(probe) = self.search.probe.as_mut() {
            probe.epoch.changed_from(message, reason);
        }
    }

    pub(super) async fn recover_refused_thinking(
        &mut self,
        messages: &[Message],
        rendering: &Rendering,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Option<SearchRefusal> {
        let first = self.first_sent_thinking(messages, rendering)?;
        let through = self.history.len().checked_sub(1)?;
        let refusal = self.close_live_tail(events).await;
        let cut = Cut {
            from: WHOLE,
            through,
            tools: false,
        };
        self.record_cut(cut, None, events).await;
        self.search.kept_break = None;
        self.settings.binding_controls = self.settings.turn_binding;
        self.mark_changed_from(first, MarkReason::ThinkingCut);
        self.adapted(HistoryChange::ThinkingRefused, events).await;
        Some(refusal)
    }

    fn first_sent_thinking(&self, messages: &[Message], rendering: &Rendering) -> Option<usize> {
        let provider = self.provider().as_ref();
        let layout = Layout::new(messages, rendering, |content, earlier| {
            echoed_positions(provider, content, earlier)
        });
        messages.iter().enumerate().find_map(|(i, m)| {
            let stripped = match rendering.form {
                RenderingForm::Text => false,
                RenderingForm::WithoutThinking => layout.rendered(i) || layout.tool_turns(i),
                RenderingForm::Flattened => layout.flattened(i),
            };
            let sends = layout
                .echoed(i)
                .iter()
                .any(|b| m.content.get(*b).is_some_and(is_thinking) && !rendering.cut(i, *b));
            (!stripped && sends).then_some(i)
        })
    }

    pub(super) async fn commit_probe(&mut self, events: &mpsc::Sender<AgentEvent>) {
        if self.search.probe.take().is_some() {
            self.mark_search_refused(events).await;
            self.log_mark(events).await;
        }
    }

    pub(super) async fn mark_search_refused(&mut self, events: &mpsc::Sender<AgentEvent>) {
        let endpoint = self.endpoint_name().to_owned();
        let model = self.session.model.clone();
        let before = self.current_tools();
        if self.search.built_with.is_none() {
            self.search.built_with = Some(before.digest.clone());
        }
        self.end_kept_tools();
        if self.search.refused.insert(endpoint.clone()) {
            let state = FeatureState::FallenBack;
            let record = state_record(Feature::NativeWebSearch, &endpoint, &model, state);
            self.log(events, record).await;
            let event = state_event(Feature::NativeWebSearch, &endpoint, &model, state);
            emit(events, event).await;
            self.report_search_refused(events).await;
            let now = self.current_tools().names;
            let removed: Vec<String> = before
                .names
                .iter()
                .filter(|n| !now.contains(n))
                .cloned()
                .collect();
            self.tools_changed(
                Vec::new(),
                removed,
                ToolChangeVia::Rebuild,
                None,
                Box::default(),
                events,
            )
            .await;
        }
        self.note_sent_tools();
    }

    pub(super) async fn render_foreign_searches(
        &mut self,
        events: &mpsc::Sender<AgentEvent>,
    ) -> bool {
        let endpoint = self.endpoint_name().to_owned();
        if self.search.rendered_on.contains(&endpoint) {
            return false;
        }
        let Some(from) = self.first_foreign_native(&endpoint, 0) else {
            return false;
        };
        let record = LogEvent::SearchHistoryRendered {
            endpoint_name: endpoint.clone(),
        };
        self.log(events, record).await;
        self.search.rendered_on.insert(endpoint.clone());
        self.search
            .rendered_through
            .insert(endpoint, self.history.len());
        self.mark_changed_from(
            self.view_index(from.message),
            MarkReason::SearchHistoryRendered,
        );
        self.searches_as_text(from, events).await;
        true
    }

    pub(super) async fn recover_unreadable(&mut self, events: &mpsc::Sender<AgentEvent>) -> bool {
        self.render_foreign_searches(events).await
            || self.render_unreadable_searches(events).await.is_some()
    }

    pub(super) async fn render_unreadable_searches(
        &mut self,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Option<SearchRefusal> {
        let from = self.first_sent_native()?;
        let refusal = self.close_live_tail(events).await;
        self.convert_unreadable(from, events).await;
        Some(refusal)
    }

    async fn convert_unreadable(&mut self, from: BlockPosition, events: &mpsc::Sender<AgentEvent>) {
        let endpoint = self.endpoint_name().to_owned();
        let record = LogEvent::SearchHistoryUnreadable {
            endpoint_name: endpoint.clone(),
        };
        self.log(events, record).await;
        self.search.unreadable.insert(endpoint, self.history.len());
        self.mark_changed_from(
            self.view_index(from.message),
            MarkReason::SearchHistoryUnreadable,
        );
        self.searches_as_text(from, events).await;
    }

    async fn searches_as_text(&mut self, from: BlockPosition, events: &mpsc::Sender<AgentEvent>) {
        let thinking_cut = self.cut_thinking_from(from, events).await;
        self.search.drops_covered = !thinking_cut;
        self.adapted(HistoryChange::SearchesAsText { thinking_cut }, events)
            .await;
    }

    fn first_sent_native(&self) -> Option<BlockPosition> {
        let messages = self.view_messages();
        let rendering = self.kept_rendering(messages.len());
        let provider = self.provider().as_ref();
        let layout = Layout::new(&messages, &rendering, |content, earlier| {
            echoed_positions(provider, content, earlier)
        });
        messages.iter().enumerate().find_map(|(message, m)| {
            layout
                .echoed(message)
                .iter()
                .copied()
                .find(|b| {
                    m.content
                        .get(*b)
                        .is_some_and(|block| native(block) && !renders(block, message, &rendering))
                })
                .map(|block| self.to_history(BlockPosition { message, block }))
        })
    }

    async fn cut_thinking_from(
        &mut self,
        from: BlockPosition,
        events: &mpsc::Sender<AgentEvent>,
    ) -> bool {
        let keeps = self.keeps_thinking();
        let Some(through) = self.history.len().checked_sub(1) else {
            return false;
        };
        let cuts = !keeps && holds_thinking_from(&self.history, from, through);
        if cuts {
            let cut = Cut {
                from,
                through,
                tools: false,
            };
            self.record_cut(cut, None, events).await;
        }
        cuts
    }

    fn first_foreign_native(&self, endpoint: &str, start: usize) -> Option<BlockPosition> {
        self.history
            .iter()
            .enumerate()
            .skip(start)
            .find_map(|(message, m)| {
                let foreign = self
                    .search
                    .endpoints
                    .get(message)
                    .is_some_and(|e| e != endpoint);
                foreign
                    .then(|| first_native(m))
                    .flatten()
                    .map(|block| BlockPosition { message, block })
            })
    }

    pub(super) fn rendering_cut(&self) -> Option<Cut> {
        if self.binding_usable_now() {
            return None;
        }
        self.rendering_edit()
    }

    fn rendering_edit(&self) -> Option<Cut> {
        let endpoint = self.endpoint_name();
        let start = *self.search.rendered_through.get(endpoint)?;
        let through = self.history.len().checked_sub(1)?;
        let from = self.first_foreign_native(endpoint, start)?;
        holds_thinking_from(&self.history, from, through).then_some(Cut {
            from,
            through,
            tools: false,
        })
    }

    pub(super) async fn keep_rendering(&mut self, events: &mpsc::Sender<AgentEvent>) {
        let endpoint = self.endpoint_name().to_owned();
        if !self.search.rendered_through.contains_key(&endpoint) {
            return;
        }
        if let Some(cut) = self.rendering_edit().filter(|_| !self.keeps_thinking()) {
            self.record_cut(cut, None, events).await;
        }
        self.search
            .rendered_through
            .insert(endpoint, self.history.len());
    }
}
