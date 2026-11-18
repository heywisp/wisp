use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::Duration;

use wisp_protocol::log::FeatureState;
use wisp_protocol::settings::{
    AppliedChange, OverrideScope, PROVIDER_ANTHROPIC, PROVIDER_GEMINI, PROVIDER_OPENAI, Restored,
    SettingTarget, SettingValue, SwitchTarget, Withdrawal, names, takes_no_effort,
};
use wisp_protocol::{
    AgentEvent, BlockKind, CacheEffect, CacheTtl, Effort, Feature, NoSearch, RebuildReason,
    RequestId, SearchForm, TurnOutcome,
};
use wisp_render::chrome::{
    Alive, CacheSize, Change, Effect, Endpoint, Facts, NextTurn, Rebuild, Scope, Told, change_note,
    changes_search, model_line, one_line, saved_parts, search_change, search_row, setting_note,
    short_model,
};

use wisp_render::chrome::account::{Move, Moved};
use wisp_render::num;
use wisp_render::theme::glyphs::{GlyphSet, Glyphs};
use wisp_render::transcript::copy;

use super::picker::{Picked, display_effort, shown_effort, switch_to};

mod switch;

use switch::{Switching, Work};

pub const CYCLE_HINT: Duration = Duration::from_secs(2);
const PREFIX_TTL: Duration = Duration::from_secs(3600);

pub use wisp_protocol::settings::fallback_effort;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pair {
    Documented,
    Unknown,
    Proven,
    FallenBack,
    NotPerMessage,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chosen {
    pub endpoint: String,
    pub model: String,
    pub effort: Option<Effort>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    Note(String),
    Notes(Vec<String>),
    Moved(Box<Moved>, Vec<String>),
    Refused(String),
    Hint(String),
    Other,
}

#[cfg(test)]
impl Answer {
    pub(crate) fn moved_texts(moved: &Moved, after: &[String]) -> Vec<String> {
        let body =
            wisp_render::chrome::account::moved_body(moved, &Glyphs::of(GlyphSet::Unicode), 400);
        let mut texts: Vec<String> = body
            .iter()
            .map(|line| crate::app::fixture::unglued(line))
            .collect();
        texts.extend(after.iter().cloned());
        texts
    }
}

#[derive(Clone, Debug)]
struct Asked {
    request: RequestId,
    chosen: Chosen,
    writes_config: bool,
    told: Option<Told>,
    quiet: bool,
    holds: bool,
}

#[derive(Clone, Debug)]
struct Deferred {
    endpoint: String,
    model: String,
    effort: Effort,
    cache: CacheEffect,
    endpoint_changed: bool,
    model_changed: bool,
    effort_changed: bool,
    account_bound: Vec<String>,
    search_refused_on: Option<String>,
    saved: Option<String>,
    warnings: Vec<String>,
    work: Option<Work>,
}

pub const fn ttl_of(ttl: CacheTtl) -> Duration {
    match ttl {
        CacheTtl::FiveMinutes => Duration::from_secs(300),
        CacheTtl::OneHour => Duration::from_secs(3600),
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Reasoning {
    kept: bool,
    last: bool,
    step: bool,
    ended: u32,
}

impl Reasoning {
    const fn held(self) -> bool {
        self.kept || self.last || self.step
    }

    fn apply(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::BlockStarted {
                kind: BlockKind::Thinking | BlockKind::RedactedThinking,
                ..
            } => self.step = true,
            AgentEvent::StepEnded { .. } => {
                self.kept |= std::mem::replace(&mut self.last, std::mem::take(&mut self.step));
                self.ended += 1;
            }
            AgentEvent::TurnStarted { .. } => {
                *self = Reasoning {
                    kept: self.kept,
                    ..Reasoning::default()
                };
            }
            AgentEvent::TurnEnded { outcome, .. } => {
                let committed = match outcome {
                    TurnOutcome::Completed { committed, .. } => *committed,
                    TurnOutcome::Cancelled {
                        committed_steps, ..
                    }
                    | TurnOutcome::Error {
                        committed_steps, ..
                    } => *committed_steps >= self.ended,
                };
                *self = Reasoning {
                    kept: self.kept || (committed && self.last),
                    ..Reasoning::default()
                };
            }
            AgentEvent::ToolsChanged {
                via: wisp_protocol::ToolChangeVia::Rebuild,
                ..
            } => {
                *self = Reasoning {
                    ended: self.ended,
                    ..Reasoning::default()
                };
            }
            _ => {}
        }
    }
}

#[derive(Clone, Debug)]
pub struct Tracker {
    ttl: CacheTtl,
    session: Chosen,
    next: Chosen,
    states: BTreeMap<(String, String), Pair>,
    running: bool,
    deferred: Vec<Deferred>,
    prefix: Option<u64>,
    prompt: u64,
    last_request: Option<Duration>,
    asked: VecDeque<Asked>,
    held: Vec<String>,
    search_refused_on: Option<String>,
    refused: BTreeSet<String>,
    reasoning: Reasoning,
    start_ttl: CacheTtl,
    pending_ttl: Option<CacheTtl>,
    saved_switch: SavedSwitch,
    switching: Switching,
    start_pick: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SavedSwitch {
    model: bool,
    endpoint: bool,
}

impl Tracker {
    pub fn at(facts: &mut Facts, now: wisp_protocol::Now) -> Tracker {
        facts.endpoints = now
            .endpoints
            .iter()
            .map(wisp_render::chrome::endpoint_facts_of)
            .collect();
        let mut current = facts.clone();
        current.endpoint_name = Some(now.endpoint);
        current.model = now.model;
        current.effort = now.effort.as_str().to_owned();
        current.cache_ttl = now.cache_ttl;
        current.next = now.next;
        let tracker = Self::new(&current);
        tracker.show(facts);
        tracker
    }

    pub fn new(facts: &Facts) -> Tracker {
        let start = Chosen {
            endpoint: facts
                .endpoint_name
                .clone()
                .unwrap_or_else(|| wisp_protocol::CLAUDE_API.to_owned()),
            model: facts.model.clone(),
            effort: facts.effort_level(),
        };
        let mut next = start.clone();
        let mut pending_ttl = None;
        if let Some(pending) = &facts.next {
            if let Some(endpoint) = &pending.endpoint {
                next.endpoint.clone_from(endpoint);
            }
            if let Some(model) = &pending.model {
                next.model.clone_from(model);
            }
            next.effort = pending.effort.or(next.effort);
            pending_ttl = pending.cache_ttl.filter(|ttl| *ttl != facts.cache_ttl);
        }
        Tracker {
            ttl: facts.cache_ttl,
            session: start.clone(),
            next,
            states: BTreeMap::new(),
            running: false,
            deferred: vec![],
            prefix: None,
            prompt: 0,
            last_request: None,
            asked: VecDeque::new(),
            held: Vec::new(),
            search_refused_on: None,
            refused: BTreeSet::new(),
            reasoning: Reasoning::default(),
            start_ttl: facts.cache_ttl,
            pending_ttl,
            saved_switch: SavedSwitch::default(),
            switching: Switching::default(),
            start_pick: false,
        }
    }

    pub fn picks_start(&mut self) {
        self.start_pick = true;
    }

    pub fn cache_ttl(&self) -> CacheTtl {
        self.ttl
    }

    fn next_ttl(&self) -> CacheTtl {
        self.pending_ttl.unwrap_or(self.ttl)
    }

    pub fn session(&self) -> &Chosen {
        &self.session
    }

    pub fn next(&self) -> &Chosen {
        &self.next
    }

    pub fn wanted(&self) -> Chosen {
        self.asked
            .back()
            .map(|asked| asked.chosen.clone())
            .unwrap_or_else(|| self.next.clone())
    }

    fn chosen(
        &self,
        endpoint: Option<&str>,
        model: Option<&str>,
        effort: Option<Effort>,
    ) -> Chosen {
        let base = self.wanted();
        Chosen {
            endpoint: endpoint.map_or(base.endpoint, str::to_owned),
            model: model.map_or(base.model, str::to_owned),
            effort: effort.or(base.effort),
        }
    }

    pub fn asked_settings(
        &mut self,
        request: RequestId,
        endpoint: Option<&str>,
        model: Option<&str>,
        effort: Option<Effort>,
    ) {
        let chosen = self.chosen(endpoint, model, effort);
        self.ask(request, chosen, false, None);
    }

    pub fn asked_holding(&mut self, request: RequestId, endpoint: &str) {
        let chosen = self.chosen(Some(endpoint), None, None);
        self.asked.push_back(Asked {
            request,
            chosen,
            writes_config: false,
            told: None,
            quiet: false,
            holds: true,
        });
    }

    pub fn take_held(&mut self) -> Vec<String> {
        std::mem::take(&mut self.held)
    }

    pub fn forget(&mut self, request: RequestId) {
        self.answered(request);
    }

    pub fn asked_saved_pair(&mut self, request: RequestId, picked: &Picked, told: Told) {
        let chosen = match picked {
            Picked::Settings {
                endpoint,
                model,
                effort,
            } => self.chosen(endpoint.as_deref(), model.as_deref(), *effort),
            Picked::Effort { .. } => self.wanted(),
        };
        self.ask(request, chosen, true, Some(told));
    }

    pub fn asked_saved(&mut self, request: RequestId, told: Option<Told>) {
        let chosen = self.wanted();
        self.ask(request, chosen, true, told);
    }

    pub fn asked_endpoint(&mut self, request: RequestId) {
        let chosen = self.wanted();
        self.ask(request, chosen, true, None);
    }

    fn endpoint_answer(&mut self, event: &AgentEvent) -> Option<Answer> {
        use wisp_render::chrome::endpoint_card as words;
        use wisp_render::text::sanitize;
        let answers = match event {
            AgentEvent::EndpointEdited { answers, .. }
            | AgentEvent::EndpointRemoved { answers, .. }
            | AgentEvent::EndpointNotSaved { answers, .. } => *answers,
            _ => return None,
        };
        self.answered(answers)?;
        let mut notes = match event {
            AgentEvent::EndpointEdited {
                name,
                changed,
                notes,
                ..
            } => {
                let fields: Vec<String> = changed.iter().map(words::field_words).collect();
                let current = *name == self.session.endpoint;
                let mut out = vec![words::edited_note(&sanitize(name), &fields, current)];
                out.extend(notes.iter().map(|n| sanitize(n)));
                out
            }
            AgentEvent::EndpointRemoved {
                name,
                kept_item,
                default_cleared,
                notes,
                ..
            } => {
                let mut out = vec![words::removed_note(
                    &sanitize(name),
                    *kept_item,
                    *default_cleared,
                )];
                out.extend(notes.iter().map(|n| sanitize(n)));
                out
            }
            AgentEvent::EndpointNotSaved { name, why, .. } => {
                let why = crate::app::config_screen::not_saved_words(why);
                vec![words::not_saved_note(&sanitize(name), &why)]
            }
            _ => vec![],
        };
        Some(match notes.len() {
            1 => Answer::Note(notes.remove(0)),
            _ => Answer::Notes(notes),
        })
    }

    pub fn asked_described(&mut self, request: RequestId) {
        let chosen = self.wanted();
        self.ask(request, chosen, false, None);
    }

    fn ask(&mut self, request: RequestId, chosen: Chosen, writes_config: bool, told: Option<Told>) {
        self.asked.push_back(Asked {
            request,
            chosen,
            writes_config,
            told,
            quiet: false,
            holds: false,
        });
    }

    pub fn asked_quietly(&mut self, request: RequestId) {
        let chosen = self.wanted();
        self.asked.push_back(Asked {
            request,
            chosen,
            writes_config: true,
            told: None,
            quiet: true,
            holds: false,
        });
    }

    fn answered(&mut self, request: RequestId) -> Option<Asked> {
        let at = self.asked.iter().position(|a| a.request == request)?;
        self.asked.remove(at)
    }

    pub fn saving(&self) -> bool {
        self.asked.iter().any(|a| a.writes_config)
    }

    pub fn quit_status(&self) -> Option<&'static str> {
        if self.saving() {
            Some(copy::SAVING_SETTINGS)
        } else if self.asked.is_empty() {
            None
        } else {
            Some(copy::APPLYING_SETTINGS)
        }
    }

    pub fn pair(&self, facts: &Facts, model: &str) -> Pair {
        self.pair_on(facts, &self.wanted().endpoint, model)
    }

    pub fn pair_on(&self, facts: &Facts, endpoint: &str, model: &str) -> Pair {
        let per_message = facts.model_facts(model).is_some_and(|m| m.per_message());
        if !per_message {
            return Pair::NotPerMessage;
        }
        if facts.reach_of(endpoint) == Endpoint::Official {
            return Pair::Documented;
        }
        self.states
            .get(&(endpoint.to_owned(), model.to_owned()))
            .copied()
            .unwrap_or(Pair::Unknown)
    }

    pub fn search_form(&self, facts: &Facts, endpoint: &str, model: &str) -> Option<SearchForm> {
        let form = facts.search.form(endpoint, model)?;
        Some(if self.refused.contains(endpoint) {
            SearchForm::None(NoSearch::Refused)
        } else {
            form
        })
    }

    fn session_search(&self, facts: &Facts) -> Option<SearchForm> {
        self.search_form(facts, &self.session.endpoint, &self.session.model)
    }

    pub fn search_note(
        &self,
        facts: &Facts,
        endpoint: &str,
        model: &str,
        set: GlyphSet,
    ) -> Option<String> {
        let from = self.session_search(facts)?;
        let to = self.search_form(facts, endpoint, model)?;
        search_row(from, to, &Glyphs::of(set))
    }

    pub fn changes_search(&self, facts: &Facts, endpoint: &str, model: &str) -> bool {
        let from = self.session_search(facts);
        let to = self.search_form(facts, endpoint, model);
        from.zip(to).is_some_and(|(f, t)| changes_search(f, t))
    }

    pub fn holds_reasoning(&self) -> bool {
        self.reasoning.held()
    }

    pub fn drops_reasoning(&self, facts: &Facts, endpoint: &str, model: &str) -> bool {
        self.holds_reasoning() && self.changes_search(facts, endpoint, model)
    }

    fn search_shift(&self, facts: &Facts, d: &Deferred, glyphs: &Glyphs) -> Option<String> {
        let correction = matches!(
            d.cache,
            CacheEffect::Rebuilt {
                reason: RebuildReason::PerMessageEffortRejected
                    | RebuildReason::NativeSearchRefused,
            }
        );
        if correction {
            return None;
        }
        let from = self.session_search(facts)?;
        let to = self.search_form(facts, &d.endpoint, &d.model)?;
        search_change(from, to, self.reasoning.held(), glyphs)
    }

    pub fn sized(&mut self, tokens: u64) {
        self.prefix = Some(tokens);
        self.prompt = tokens;
    }

    pub fn cache_size(&self) -> CacheSize {
        CacheSize {
            prefix: self.prefix.unwrap_or(0),
            total: self.prompt,
        }
    }

    fn ttl(&self) -> Duration {
        ttl_of(self.ttl)
    }

    pub fn alive(&self, at: Duration) -> Alive {
        let idle = self
            .last_request
            .map_or(Duration::ZERO, |last| at.saturating_sub(last));
        Alive {
            prefix: idle <= PREFIX_TTL,
            tail: idle <= self.ttl(),
        }
    }

    pub fn rebuild(&self, facts: &Facts, model: &str, scope: Scope, at: Duration) -> Rebuild {
        self.rebuild_on(facts, &self.wanted().endpoint, model, scope, at)
    }

    pub fn rebuild_on(
        &self,
        facts: &Facts,
        endpoint: &str,
        model: &str,
        scope: Scope,
        at: Duration,
    ) -> Rebuild {
        let rates = facts.rates_on(endpoint, model);
        let rebuild = Rebuild::estimate(
            self.cache_size(),
            scope,
            self.alive(at),
            rates,
            facts.writes_on(endpoint, model, self.next_ttl()),
        );
        if priced(facts, endpoint) {
            rebuild.billed(facts.billing_on(endpoint))
        } else {
            rebuild.unpriced()
        }
    }

    pub fn effort_effect(&self, facts: &Facts, model: &str, level: Effort, at: Duration) -> Effect {
        if self.wanted().endpoint != self.session.endpoint {
            return Effect::WithEndpointSwitch;
        }
        if let Some(sibling) = facts.sibling(model, level)
            && sibling != model
        {
            return self.model_effect(facts, sibling, at);
        }
        self.effort_on(facts, &self.session.endpoint, model, level, at)
    }

    fn effort_on(
        &self,
        facts: &Facts,
        endpoint: &str,
        model: &str,
        level: Effort,
        at: Duration,
    ) -> Effect {
        if model != self.session.model {
            return Effect::WithSwitch;
        }
        if Some(level) == self.session.effort {
            return Effect::Kept;
        }
        match self.pair_on(facts, endpoint, model) {
            Pair::Documented | Pair::Proven => Effect::Kept,
            Pair::Unknown => Effect::KeptIfPassedOn,
            Pair::FallenBack | Pair::NotPerMessage => {
                Effect::Rebuild(self.rebuild_on(facts, endpoint, model, Scope::UpTo, at))
            }
        }
    }

    pub fn endpoint_effect(&self, facts: &Facts, endpoint: &str, at: Duration) -> Effect {
        if facts
            .endpoint_facts(endpoint)
            .is_some_and(|e| !e.available())
        {
            return Effect::Unavailable;
        }
        let wanted = self.wanted();
        if endpoint == wanted.endpoint {
            return Effect::Current;
        }
        let pair = super::picker::shown_pair(facts, self, endpoint);
        let model = pair.model;
        if endpoint != self.session.endpoint {
            let rebuild = self.rebuild_on(facts, endpoint, &model, Scope::Exact, at);
            let target = SwitchTarget::Endpoint(endpoint.to_owned());
            return self.quoted_switch(facts, &target, (endpoint, &model), rebuild);
        }
        if model != self.session.model {
            return Effect::Rebuild(self.rebuild_on(facts, endpoint, &model, Scope::Exact, at));
        }
        match pair.effort {
            Some(level) => self.effort_on(facts, endpoint, &model, level, at),
            None => Effect::Kept,
        }
    }

    pub fn model_effect(&self, facts: &Facts, model: &str, at: Duration) -> Effect {
        let wanted = self.wanted();
        let selectable = facts.model_selectable(model, Some(&wanted.endpoint));
        if model == wanted.model {
            Effect::Current
        } else if !selectable {
            Effect::Unsupported
        } else if wanted.endpoint != self.session.endpoint {
            Effect::WithEndpointSwitch
        } else if model == self.session.model {
            let level = match switch_to(model, facts, wanted.effort, None) {
                Picked::Effort { preselect, .. } => preselect,
                Picked::Settings { .. } => {
                    shown_effort(facts, model, wanted.effort).or(wanted.effort)
                }
            };
            level.map_or_else(
                || Effect::Rebuild(self.rebuild(facts, model, Scope::UpTo, at)),
                |level| self.effort_effect(facts, model, level, at),
            )
        } else {
            let rebuild = self.rebuild(facts, model, Scope::Exact, at);
            let target = SwitchTarget::Model(model.to_owned());
            self.quoted_switch(facts, &target, (&wanted.endpoint, model), rebuild)
        }
    }

    pub fn pair_effect(&self, facts: &Facts, endpoint: &str, model: &str, at: Duration) -> Effect {
        if endpoint == self.wanted().endpoint {
            return self.model_effect(facts, model, at);
        }
        if !facts.model_selectable(model, Some(endpoint)) {
            return Effect::Unsupported;
        }
        let rebuild = self.rebuild_on(facts, endpoint, model, Scope::Exact, at);
        let target = SwitchTarget::Pair {
            endpoint: endpoint.to_owned(),
            model: model.to_owned(),
        };
        self.quoted_switch(facts, &target, (endpoint, model), rebuild)
    }

    pub fn cycle(
        &self,
        facts: &Facts,
        at: Duration,
        set: GlyphSet,
    ) -> Result<(Effort, String), String> {
        let glyphs = Glyphs::of(set);
        let sep = format!(" {} ", glyphs.separator);
        let wanted = self.wanted();
        let short = short_model(&wanted.model);
        let amount = || {
            self.rebuild(facts, &wanted.model, Scope::UpTo, at)
                .amount(set)
        };
        let caps = facts
            .model_facts(&wanted.model)
            .and_then(|m| m.caps.as_ref());
        let Some(caps) = caps else {
            return Err(copy::effort_rebuilds(&short, &amount()));
        };
        if takes_no_effort(Some(&caps.efforts)) {
            return Err(copy::no_effort_levels(&short));
        }
        match self.pair(facts, &wanted.model) {
            Pair::Documented | Pair::Proven => {}
            Pair::Unknown => return Err(copy::gateway_unproven(&amount())),
            Pair::FallenBack => return Err(copy::gateway_fell_back(&short, &amount())),
            Pair::NotPerMessage => return Err(copy::effort_rebuilds(&short, &amount())),
        }
        let at_level = wanted
            .effort
            .and_then(|e| caps.efforts.iter().position(|l| *l == e));
        let to = match at_level {
            Some(i) => caps.efforts.get((i + 1) % caps.efforts.len()).copied(),
            None => fallback_effort(&caps.efforts, wanted.effort),
        };
        let Some(to) = to else {
            return Err(copy::no_effort_levels(&short));
        };
        let effect = self.effort_effect(facts, &wanted.model, to, at).text(set);
        let from = wanted.effort.map_or("?", Effort::as_str);
        let hint = copy::effort_cycled(glyphs.arrow, from, to.as_str(), &sep, &effect);
        Ok((to, hint))
    }

    pub fn next_turn(&self) -> Option<NextTurn> {
        let endpoint =
            (self.next.endpoint != self.session.endpoint).then(|| self.next.endpoint.clone());
        let model = (self.next.model != self.session.model).then(|| self.next.model.clone());
        let effort = (self.next.effort != self.session.effort)
            .then_some(self.next.effort)
            .flatten();
        (endpoint.is_some() || model.is_some() || effort.is_some()).then_some(NextTurn {
            endpoint,
            model,
            effort,
            cache_ttl: self.pending_ttl,
        })
    }

    pub fn search_refused_before(&mut self, endpoint: &str) {
        self.refused.insert(endpoint.to_owned());
    }

    pub fn set_pair(&mut self, endpoint: &str, model: &str, state: FeatureState) {
        let pair = match state {
            FeatureState::Proven => Pair::Proven,
            FeatureState::FallenBack => Pair::FallenBack,
        };
        self.states
            .insert((endpoint.to_owned(), model.to_owned()), pair);
    }

    pub fn show(&self, facts: &mut Facts) -> bool {
        let glyphs = Glyphs::of(GlyphSet::Ascii);
        let before = (
            facts.provider.clone(),
            facts.model.clone(),
            facts.effort.clone(),
            facts.credential(),
            facts.search_line(&glyphs),
            facts.cache_ttl,
        );
        facts.search.refused = self.refused.iter().cloned().collect();
        facts.cache_ttl = self.ttl;
        facts.write_multipliers = facts.write_multipliers.at(self.ttl);
        let now = if self.running {
            &self.session
        } else {
            &self.next
        };
        facts.model = now.model.clone();
        facts.effort = display_effort(facts, &now.model, now.effort)
            .map_or_else(String::new, |e| e.as_str().to_owned());
        let shown = facts
            .endpoint_name
            .as_deref()
            .unwrap_or(wisp_protocol::CLAUDE_API);
        if shown != now.endpoint {
            facts.endpoint_name = Some(now.endpoint.clone());
            if let Some(endpoint) = facts.endpoint_facts(&now.endpoint).cloned() {
                if let Some(reach) = endpoint.reach {
                    facts.endpoint = reach;
                }
                if endpoint.credential.is_some() && facts.credential.is_some() {
                    facts.credential = endpoint.credential;
                }
                facts.host = endpoint.host;
            }
        }
        if let Some(provider) = facts
            .endpoint_facts(&now.endpoint)
            .map(|endpoint| endpoint.provider.clone())
        {
            facts.provider = provider_title(&provider).map_or(provider, str::to_owned);
        }
        facts.next = self.next_turn().filter(|_| self.running).map(|mut next| {
            next.effort = display_effort(
                facts,
                next.model.as_deref().unwrap_or(&now.model),
                next.effort,
            );
            next
        });
        before
            != (
                facts.provider.clone(),
                facts.model.clone(),
                facts.effort.clone(),
                facts.credential(),
                facts.search_line(&glyphs),
                facts.cache_ttl,
            )
    }

    pub fn apply(
        &mut self,
        event: &AgentEvent,
        facts: &Facts,
        at: Duration,
        set: GlyphSet,
    ) -> Answer {
        let noted = self.switch_event(event, facts, set);
        let answer = self.applied(event, facts, at, set);
        match noted {
            Some(note) => joined(answer, vec![note]),
            None => answer,
        }
    }

    fn applied(
        &mut self,
        event: &AgentEvent,
        facts: &Facts,
        at: Duration,
        set: GlyphSet,
    ) -> Answer {
        self.reasoning.apply(event);
        if let Some(answer) = self.endpoint_answer(event) {
            return answer;
        }
        match event {
            AgentEvent::TurnStarted {
                retry_of: Some(_), ..
            } => {
                self.session.model = super::picker::model_on(
                    facts,
                    &self.session.endpoint,
                    &self.next.endpoint,
                    &self.session.model,
                );
                if self.session.endpoint != self.next.endpoint {
                    self.session.effort =
                        shown_effort(facts, &self.session.model, self.session.effort)
                            .or(self.session.effort);
                }
                self.session.endpoint.clone_from(&self.next.endpoint);
                self.running = true;
                Answer::Other
            }
            AgentEvent::TurnStarted { .. } => {
                self.session = self.next.clone();
                if let Some(ttl) = self.pending_ttl.take() {
                    self.ttl = ttl;
                }
                self.saved_switch = SavedSwitch::default();
                self.running = true;
                Answer::Other
            }
            AgentEvent::StepEnded { usage: Some(u), .. } => {
                if self.prefix.is_none() {
                    let one_hour = match (u.cache_creation_1h_input_tokens, self.ttl) {
                        (Some(n), _) => n,
                        (None, CacheTtl::OneHour) => u.cache_creation_input_tokens,
                        (None, CacheTtl::FiveMinutes) => 0,
                    };
                    self.prefix = Some(one_hour + u.cache_read_input_tokens);
                }
                self.prompt = u.prompt_tokens();
                self.last_request = Some(at);
                Answer::Other
            }
            AgentEvent::Rejected {
                reason,
                request: Some(request),
                ..
            } => match self.answered(*request) {
                Some(asked) if asked.quiet => Answer::Hint(copy::settings_refused(reason)),
                Some(_) => Answer::Refused(copy::settings_refused(reason)),
                None => Answer::Other,
            },
            AgentEvent::SettingsSnapshot(snapshot) => {
                self.answered(snapshot.answers);
                Answer::Other
            }
            AgentEvent::SettingsChanged {
                withdrawn: Some(why),
                restored,
                endpoint,
                model,
                effort,
                changes,
                ..
            } => self.withdrawn(
                why,
                restored.as_ref(),
                endpoint,
                model,
                *effort,
                changes,
                (facts, set),
            ),
            AgentEvent::SettingsChanged {
                endpoint,
                model,
                effort,
                cache,
                account_bound,
                answers,
                changes,
                ..
            } => {
                let asked = answers.and_then(|request| self.answered(request));
                let told = asked.as_ref().and_then(|a| a.told.as_ref());
                let mut notes = if asked.as_ref().is_some_and(|a| a.quiet) {
                    Vec::new()
                } else {
                    self.setting_notes(changes, facts, (at, set), told)
                };
                if asked.as_ref().is_some_and(|a| a.holds) {
                    self.held.append(&mut notes);
                }
                let saved = changes
                    .iter()
                    .filter(|c| is_pair(&c.target) && c.saved.is_some())
                    .map(|c| saved_parts(c, told))
                    .next()
                    .map(|parts| parts.join(&format!(" {} ", Glyphs::of(set).separator)));
                let paired = changes.is_empty() || changes.iter().any(|c| is_pair(&c.target));
                if asked.as_ref().is_some_and(|a| a.writes_config) {
                    self.saved_switch.model |= *model != self.next.model;
                    self.saved_switch.endpoint |= *endpoint != self.next.endpoint;
                }
                let switched = *model != self.next.model || *endpoint != self.next.endpoint;
                let work = self.changed_work(facts, changes, switched);
                let d = Deferred {
                    work,
                    endpoint: endpoint.clone(),
                    model: model.clone(),
                    effort: *effort,
                    cache: cache.clone(),
                    endpoint_changed: *endpoint != self.next.endpoint
                        || *cache
                            == CacheEffect::Rebuilt {
                                reason: RebuildReason::EndpointSwitch,
                            },
                    model_changed: *model != self.next.model,
                    effort_changed: Some(*effort) != self.next.effort,
                    account_bound: account_bound.clone(),
                    search_refused_on: match cache {
                        CacheEffect::Rebuilt {
                            reason: RebuildReason::NativeSearchRefused,
                        } => self.search_refused_on.take(),
                        _ => None,
                    },
                    saved,
                    warnings: changes
                        .iter()
                        .filter(|c| is_pair(&c.target))
                        .flat_map(|c| &c.warnings)
                        .map(|w| one_line(w))
                        .collect(),
                };
                let pair = self.changed(d, facts, at, set, paired);
                joined(pair, notes)
            }
            AgentEvent::FeatureStateChanged {
                endpoint,
                model,
                feature: Feature::PerMessageEffort,
                state,
            } => {
                self.set_pair(endpoint, model, *state);
                Answer::Other
            }
            AgentEvent::FeatureStateChanged {
                endpoint,
                feature: Feature::NativeWebSearch,
                state: FeatureState::FallenBack,
                ..
            } => {
                self.search_refused_on = Some(endpoint.clone());
                self.refused.insert(endpoint.clone());
                Answer::Other
            }
            AgentEvent::TurnEnded { .. } => {
                self.running = false;
                let notes: Vec<String> = std::mem::take(&mut self.deferred)
                    .into_iter()
                    .flat_map(|d| std::iter::once(self.note(&d, facts, at, set)).chain(d.warnings))
                    .collect();
                if notes.is_empty() {
                    Answer::Other
                } else {
                    Answer::Notes(notes)
                }
            }
            _ => Answer::Other,
        }
    }

    fn changed(
        &mut self,
        d: Deferred,
        facts: &Facts,
        at: Duration,
        set: GlyphSet,
        noted: bool,
    ) -> Answer {
        let effort_correction = d.cache
            == CacheEffect::Rebuilt {
                reason: RebuildReason::PerMessageEffortRejected,
            };
        let search_refused = d.cache
            == CacheEffect::Rebuilt {
                reason: RebuildReason::NativeSearchRefused,
            };
        let correction = effort_correction || search_refused;
        if !effort_correction {
            self.next = Chosen {
                endpoint: d.endpoint.clone(),
                model: d.model.clone(),
                effort: Some(d.effort),
            };
        }
        if !noted {
            return Answer::Other;
        }
        let switch = !correction && (d.endpoint_changed || d.model_changed);
        if switch {
            let how = if std::mem::take(&mut self.start_pick) && self.last_request.is_none() {
                Move::Started
            } else if self.running || d.work.is_some() {
                Move::Switching
            } else {
                Move::Switched
            };
            return self.moved(&d, how, facts, at, set);
        }
        if self.running && !correction {
            self.deferred.push(d);
            return Answer::Other;
        }
        let note = self.note(&d, facts, at, set);
        if d.warnings.is_empty() {
            Answer::Note(note)
        } else {
            Answer::Notes(std::iter::once(note).chain(d.warnings).collect())
        }
    }

    fn moved(&self, d: &Deferred, how: Move, facts: &Facts, at: Duration, set: GlyphSet) -> Answer {
        let glyphs = Glyphs::of(set);
        let rebuild = match d.work {
            Some(w) => self.head_rebuild(facts, &d.endpoint, &d.model, w.head_tokens),
            None => self.rebuild_on(facts, &d.endpoint, &d.model, Scope::Exact, at),
        };
        let moved = Moved {
            how,
            name: d.endpoint.clone(),
            mark: facts.mark_of(&d.endpoint),
            state: facts.account_state(&d.endpoint),
            from: (d.endpoint_changed && how != Move::Started)
                .then(|| facts.account_state(&self.session.endpoint)),
            model: facts.model_label(&d.model),
            effort: display_effort(facts, &d.model, Some(d.effort)).map(|e| e.as_str().to_owned()),
            rebuild: (how != Move::Started).then_some(rebuild),
        };
        let mut after = d.warnings.clone();
        after.extend(d.work.map(|w| w.work.text(set)));
        after.extend(self.search_shift(facts, d, &glyphs));
        let bound: Vec<String> = d
            .account_bound
            .iter()
            .map(|id| wisp_render::chrome::model_name(&wisp_render::chrome::one_line(id)))
            .collect();
        if !bound.is_empty() {
            after.push(copy::reasoning_may_not_carry(&bound));
        }
        after.extend(d.saved.clone());
        let cap = facts
            .max_output_on(&d.endpoint, &d.model)
            .filter(|cap| *cap < facts.max_tokens && d.model_changed);
        after.extend(cap.map(|cap| copy::output_capped(&num(u64::from(cap)))));
        Answer::Moved(Box::new(moved), after)
    }

    fn setting_notes(
        &mut self,
        changes: &[AppliedChange],
        facts: &Facts,
        (at, set): (Duration, GlyphSet),
        told: Option<&Told>,
    ) -> Vec<String> {
        let glyphs = Glyphs::of(set);
        let mut notes = Vec::new();
        for change in changes.iter().filter(|c| !is_pair(&c.target)) {
            self.follow_ttl(change);
            let cost = change.cache.as_ref().map(|cache| match cache {
                CacheEffect::Kept => wisp_protocol::settings::CACHE_KEPT.to_owned(),
                CacheEffect::KeptIfPassedOn => wisp_protocol::settings::CACHE_KEPT_TOOLS.to_owned(),
                CacheEffect::Rebuilt { .. } => {
                    let wanted = self.wanted();
                    self.rebuild_on(facts, &wanted.endpoint, &wanted.model, Scope::UpTo, at)
                        .line(set)
                }
            });
            notes.push(setting_note(
                change,
                cost,
                &self.session.model,
                &glyphs,
                told,
            ));
        }
        notes
    }

    fn follow_ttl(&mut self, change: &AppliedChange) {
        if change.target.spec().name != names::CACHE_TTL {
            return;
        }
        let shadowed = change
            .overridden
            .as_ref()
            .is_some_and(|o| o.scope == OverrideScope::ThisSession);
        match &change.cache {
            _ if shadowed => {}
            Some(CacheEffect::Rebuilt { .. }) => {
                self.pending_ttl = Some(self.ttl_of_change(change)).filter(|t| *t != self.ttl);
            }
            Some(_) => self.pending_ttl = None,
            None => {}
        }
    }

    fn ttl_of_change(&self, change: &AppliedChange) -> CacheTtl {
        match &change.value {
            Some(SettingValue::Text(text)) => text.parse().unwrap_or(self.ttl),
            _ => self.start_ttl,
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the withdrawal's fields, taken apart where the event is matched"
    )]
    fn withdrawn(
        &mut self,
        why: &Withdrawal,
        restored: Option<&Restored>,
        endpoint: &str,
        model: &str,
        effort: Effort,
        changes: &[AppliedChange],
        (facts, set): (&Facts, GlyphSet),
    ) -> Answer {
        let glyphs = Glyphs::of(set);
        let sep = format!(" {} ", glyphs.separator);
        let wanted = self.wanted();
        let restored_effort = restored.map_or(self.session.effort, |r| Some(r.effort));
        if let Some(restored) = restored {
            self.ttl = restored.cache_ttl;
        }
        self.session = Chosen {
            endpoint: endpoint.to_owned(),
            model: model.to_owned(),
            effort: restored_effort,
        };
        self.next = Chosen {
            endpoint: endpoint.to_owned(),
            model: model.to_owned(),
            effort: Some(effort),
        };
        self.pending_ttl = None;
        for change in changes {
            self.follow_ttl(change);
        }
        let to_model = (wanted.model != model).then(|| model_line(&wanted.model));
        let to_endpoint = (wanted.endpoint != endpoint).then(|| one_line(&wanted.endpoint));
        let failed = matches!(why, Withdrawal::SwitchCompactionFailed);
        let cause = match why {
            Withdrawal::SwitchCompactionCancelled => copy::COMPACTION_CANCELLED.to_owned(),
            Withdrawal::SwitchCompactionFailed => copy::COMPACTION_FAILED.to_owned(),
            Withdrawal::Other(other) => one_line(&other.replace('_', " ")),
        };
        let mut parts = match (&to_model, &to_endpoint) {
            (Some(m), None) => vec![
                copy::switch_withdrawn(m, &cause),
                copy::choose_again(&["/model"]),
            ],
            (None, Some(e)) => vec![
                copy::switch_withdrawn(e, &cause),
                copy::choose_again(&["/account"]),
            ],
            (Some(m), Some(e)) => vec![
                copy::switch_withdrawn(&copy::model_on(m, e), &cause),
                copy::choose_again(&["/model", "/account"]),
            ],
            (None, None) => vec![copy::nameless_withdrawn(failed)],
        };
        if let Some((asked, effort)) = display_effort(facts, &wanted.model, wanted.effort)
            .zip(display_effort(facts, model, Some(effort)))
            && asked != effort
        {
            parts.push(copy::effort_back(
                effort.as_str(),
                &facts.model_label(model),
                asked.as_str(),
            ));
        }
        if self.saved_switch.model && to_model.is_some() {
            parts.push(copy::STILL_SAVED_MODEL.to_owned());
        }
        if self.saved_switch.endpoint && to_endpoint.is_some() {
            parts.push(copy::STILL_SAVED_ENDPOINT.to_owned());
        }
        self.saved_switch = SavedSwitch::default();
        Answer::Note(parts.join(&sep))
    }

    fn note(&self, d: &Deferred, facts: &Facts, at: Duration, set: GlyphSet) -> String {
        let scope = match d.cache {
            CacheEffect::Rebuilt {
                reason: RebuildReason::ModelSwitch | RebuildReason::EndpointSwitch,
            } => Scope::Exact,
            _ => Scope::UpTo,
        };
        let glyphs = Glyphs::of(set);
        let rebuilt_on = d.search_refused_on.as_deref().unwrap_or(&d.endpoint);
        let search = self.search_shift(facts, d, &glyphs);
        let change = Change {
            endpoint: d.endpoint_changed.then_some(d.endpoint.as_str()),
            account_bound: &d.account_bound,
            model: &d.model,
            effort: display_effort(facts, &d.model, Some(d.effort)),
            cache: &d.cache,
            model_changed: d.model_changed,
            effort_changed: d.effort_changed,
            rebuild: match d.work {
                Some(w) => self.head_rebuild(facts, rebuilt_on, &d.model, w.head_tokens),
                None => self.rebuild_on(facts, rebuilt_on, &d.model, scope, at),
            },
            search_refused_on: Some(rebuilt_on),
            search_refused_gateway: facts.reach_of(rebuilt_on) == Endpoint::Custom,
            search: search.as_deref(),
            work: d.work.map(|w| w.work),
        };
        let mut note = change_note(&change, &glyphs);
        if let Some(saved) = &d.saved {
            note = format!("{note} {} {saved}", glyphs.separator);
        }
        let cap = facts
            .max_output_on(&d.endpoint, &d.model)
            .filter(|cap| *cap < facts.max_tokens);
        match cap {
            Some(cap) if d.model_changed => format!(
                "{note} {} {}",
                glyphs.separator,
                copy::output_capped(&num(u64::from(cap)))
            ),
            _ => note,
        }
    }
}

pub fn provider_title(provider: &str) -> Option<&'static str> {
    match provider {
        PROVIDER_ANTHROPIC => Some("Anthropic"),
        PROVIDER_OPENAI => Some("OpenAI"),
        PROVIDER_GEMINI => Some("Gemini"),
        _ => None,
    }
}

fn is_pair(target: &SettingTarget) -> bool {
    names::PAIR.contains(&target.spec().name)
}

fn joined(pair: Answer, notes: Vec<String>) -> Answer {
    let mut all = match pair {
        Answer::Moved(moved, mut after) => {
            after.extend(notes);
            return Answer::Moved(moved, after);
        }
        Answer::Note(note) => vec![note],
        Answer::Notes(notes) => notes,
        Answer::Refused(_) | Answer::Hint(_) | Answer::Other if notes.is_empty() => return pair,
        Answer::Refused(_) | Answer::Hint(_) | Answer::Other => Vec::new(),
    };
    all.extend(notes);
    match all.len() {
        0 => Answer::Other,
        1 => all.pop().map_or(Answer::Other, Answer::Note),
        _ => Answer::Notes(all),
    }
}

fn priced(facts: &Facts, endpoint: &str) -> bool {
    facts.endpoint_facts(endpoint).is_none_or(|e| e.priced)
}

#[cfg(test)]
pub(crate) mod tests;
