use std::collections::BTreeSet;
use std::time::Duration;
use wisp_protocol::settings::names;

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Position;
use wisp_protocol::plan_usage::{PlanUsageError, UsageAction, UsageState};
use wisp_protocol::{AgentEvent, Billing, ClientCommand};
use wisp_render::chrome::account::AccountKind;
use wisp_render::chrome::{Credential, EndpointFacts};
use wisp_render::markdown::Layout;
use wisp_render::plan_usage::{AccountView, Refused, UsageHint, UsagePage};

use super::Presenter;
use crate::app::list::ListCursor;
use crate::app::usage_spend::{all_chats, endpoint_view, page_total};
use crate::term::{Key, KeyCode};

#[derive(Default)]
pub(super) struct Page {
    focus: String,
    scroll: usize,
    max_scroll: usize,
    observed_now: i64,
}

fn empty() -> UsageState {
    UsageState {
        snapshot: None,
        loading: false,
        stale: false,
        error: None,
    }
}

fn billing(endpoint: &EndpointFacts) -> Billing {
    endpoint
        .credential
        .map_or(Billing::Metered, Credential::billing)
}

impl Presenter {
    pub(super) fn subscribe_usage(&mut self, at: Duration) {
        let endpoint = self.tracker.session().endpoint.clone();
        if self.usage_endpoint.as_ref() == Some(&endpoint) {
            return;
        }
        if let Some(previous) = self.usage_endpoint.take()
            && !self.usage_refreshes.values().any(|name| name == &previous)
        {
            self.release_usage(previous, at);
        }
        if !self
            .usage_endpoints()
            .iter()
            .any(|facts| facts.name == endpoint && self.usage_eligible(facts))
        {
            return;
        }
        let request = self.next_request();
        if self.command(
            ClientCommand::PlanUsage {
                request,
                endpoint: endpoint.clone(),
                action: UsageAction::Subscribe,
            },
            at,
        ) {
            self.usage_initial = Some(request);
            self.usage_endpoint = Some(endpoint);
        }
    }

    fn release_usage(&mut self, endpoint: String, at: Duration) {
        let request = self.next_request();
        self.command(
            ClientCommand::PlanUsage {
                request,
                endpoint,
                action: UsageAction::Release,
            },
            at,
        );
    }

    pub(super) fn usage_endpoints(&self) -> Vec<EndpointFacts> {
        let mut endpoints = self.facts.endpoints.clone();
        let active = &self.tracker.session().endpoint;
        if !self.usage_catalog
            && !active.is_empty()
            && !endpoints.iter().any(|endpoint| endpoint.name == *active)
        {
            endpoints.push(EndpointFacts {
                models: Vec::new(),
                mark: None,
                name: active.clone(),
                provider: self.facts.provider.clone(),
                credential: self.facts.credential,
                reach: Some(self.facts.endpoint),
                host: self.facts.host.clone(),
                unavailable: None,
                priced: true,
                expired: false,
                key_var: None,
                complete: false,
                default_model: None,
            });
        }
        endpoints.sort_by_key(|endpoint| {
            (
                endpoint.name != *active,
                billing(endpoint) == Billing::Metered,
            )
        });
        endpoints
    }

    fn usage_eligible(&self, endpoint: &EndpointFacts) -> bool {
        billing(endpoint) != Billing::Metered
            && endpoint.unavailable.is_none()
            && !endpoint.expired
            && !(endpoint.credential == Some(Credential::AnthropicPlan) && !self.claude_usage)
            && !self.plan_usage.get(&endpoint.name).is_some_and(|state| {
                matches!(
                    state.error,
                    Some(PlanUsageError::AuthenticationRequired | PlanUsageError::Disabled)
                )
            })
    }

    fn refresh_usage(&mut self, name: Option<&str>, action: UsageAction, at: Duration) {
        for endpoint in self.usage_endpoints() {
            if name.is_some_and(|name| name != endpoint.name)
                || !self.usage_eligible(&endpoint)
                || self
                    .usage_refreshes
                    .values()
                    .any(|name| name == &endpoint.name)
            {
                continue;
            }
            let request = self.next_request();
            if self.command(
                ClientCommand::PlanUsage {
                    request,
                    endpoint: endpoint.name.clone(),
                    action,
                },
                at,
            ) {
                self.usage_refreshes.insert(request, endpoint.name.clone());
                self.plan_usage
                    .entry(endpoint.name)
                    .or_insert_with(empty)
                    .loading = true;
            }
        }
        self.frames.mark();
    }

    fn ask_spend(&mut self, at: Duration) {
        if self.spend_request.is_some() {
            self.spend_again = true;
            return;
        }
        let request = self.next_request();
        if self.command(ClientCommand::ListSpend { request }, at) {
            self.spend_request = Some(request);
        }
    }

    pub(super) fn usage_command(&mut self, words: &str, at: Duration) {
        let endpoints = self.usage_endpoints();
        if !words.is_empty() && !endpoints.iter().any(|endpoint| endpoint.name == words) {
            self.hint("Unknown account; /account lists your accounts", at);
            return;
        }
        self.spend_request = None;
        self.spend_again = false;
        self.ask_spend(at);
        if words.is_empty() {
            self.ask_agents();
        }
        let focus = if words.is_empty() {
            endpoints
                .iter()
                .find(|e| self.account_view(e, &self.usage_state(e)).signs_in())
                .or(endpoints.first())
                .map(|e| e.name.clone())
                .unwrap_or_default()
        } else {
            words.to_owned()
        };
        self.usage_page = Some(Page {
            focus,
            scroll: 0,
            max_scroll: 0,
            observed_now: (self.now)().as_second(),
        });
        self.refresh_usage(None, UsageAction::Subscribe, at);
        self.usage_focus_visible();
        self.frames.mark();
    }

    pub(super) fn refresh_usage_page(&mut self) {
        let now = (self.now)().as_second();
        if let Some(page) = &mut self.usage_page
            && page.observed_now != now
        {
            page.observed_now = now;
            self.frames.mark();
        }
    }

    fn close_usage_page(&mut self, at: Duration) {
        self.usage_page = None;
        let active = self.tracker.session().endpoint.clone();
        let names: BTreeSet<_> = self
            .usage_refreshes
            .values()
            .filter(|name| **name != active)
            .cloned()
            .collect();
        self.usage_refreshes.retain(|_, name| *name == active);
        for name in names {
            if let Some(state) = self.plan_usage.get_mut(&name) {
                state.loading = false;
            }
            self.release_usage(name, at);
        }
        self.usage_status();
        self.frames.mark();
    }

    pub(super) fn usage_page_key(&mut self, key: Key, at: Duration) {
        if key.ctrl && matches!(key.code, KeyCode::Char('c' | 'C')) {
            if self.status.is_running() {
                self.stop();
            } else {
                self.close_usage_page(at);
            }
            return;
        }
        if key.ctrl || key.alt {
            return;
        }
        if key.code == KeyCode::Esc {
            self.close_usage_page(at);
            return;
        }
        let endpoints = self.usage_endpoints();
        let Some(page) = self.usage_page.as_mut() else {
            return;
        };
        page.scroll = page.scroll.min(page.max_scroll);
        let current = endpoints
            .iter()
            .position(|e| e.name == page.focus)
            .unwrap_or(0);
        let cursor = ListCursor {
            ends: true,
            ..ListCursor::CLAMP
        };
        let next = cursor.key(key.code, current, endpoints.len());
        if let Some(next) = next {
            if let Some(endpoint) = endpoints.get(next) {
                page.focus = endpoint.name.clone();
            }
            self.usage_focus_visible();
        } else {
            match key.code {
                KeyCode::Enter => {
                    let focus = page.focus.clone();
                    let can = endpoints.iter().find(|e| e.name == focus).map(|endpoint| {
                        let state = self.usage_state(endpoint);
                        let view = self.account_view(endpoint, &state);
                        (view.signs_in(), view.changes_key())
                    });
                    match can {
                        Some((true, _)) => {
                            let words = wisp_render::chrome::account::login_words(
                                &focus,
                                crate::app::subscription_login::reaches(&focus),
                            );
                            self.login_command(&words, at);
                        }
                        Some((_, true)) => self.open_edit_card(&focus, false, at),
                        _ => {}
                    }
                }
                KeyCode::PageUp => {
                    page.scroll = page
                        .scroll
                        .saturating_sub(usize::from(self.size.height / 2).max(1))
                }
                KeyCode::PageDown => {
                    page.scroll = page
                        .scroll
                        .saturating_add(usize::from(self.size.height / 2).max(1))
                        .min(page.max_scroll)
                }
                KeyCode::Char('r') => {
                    self.refresh_usage(None, UsageAction::Refresh, at);
                    self.ask_spend(at);
                }
                _ => {}
            }
        }
        self.frames.mark();
    }

    pub(super) fn usage_page_wheel(&mut self, down: bool, rows: usize) {
        if let Some(page) = &mut self.usage_page {
            page.scroll = page.scroll.min(page.max_scroll);
            page.scroll = if down {
                page.scroll.saturating_add(rows).min(page.max_scroll)
            } else {
                page.scroll.saturating_sub(rows)
            };
            self.frames.mark();
        }
    }

    fn usage_state(&self, endpoint: &EndpointFacts) -> UsageState {
        let mut state = self
            .plan_usage
            .get(&endpoint.name)
            .cloned()
            .unwrap_or_else(empty);
        if endpoint.expired {
            state = UsageState {
                error: Some(PlanUsageError::AuthenticationRequired),
                ..empty()
            };
        } else if endpoint.credential == Some(Credential::AnthropicPlan) && !self.claude_usage {
            state = UsageState {
                error: Some(PlanUsageError::Disabled),
                ..empty()
            };
        }
        state
    }

    fn account_view<'a>(
        &self,
        endpoint: &'a EndpointFacts,
        state: &'a UsageState,
    ) -> AccountView<'a> {
        AccountView {
            off: self.facts.providers_off.contains(&endpoint.provider),
            alias: &endpoint.name,
            mark: endpoint.mark,
            kind: endpoint.account_kind().unwrap_or(AccountKind::ClaudeApi),
            state,
            unavailable: endpoint.unavailable.as_deref(),
            spend: endpoint_view(&self.agents, &endpoint.name, endpoint.priced),
            active: endpoint.name == self.tracker.session().endpoint,
            focused: self
                .usage_page
                .as_ref()
                .is_some_and(|page| page.focus == endpoint.name),
            refused: self.refused_keys.contains(&endpoint.name).then(|| Refused {
                fix: self.key_fix(&endpoint.name),
            }),
            saved_key: self
                .facts
                .endpoint_facts(&endpoint.name)
                .is_some_and(|facts| facts.available() && facts.key_var.is_none()),
        }
    }

    fn usage_rows(&self, room: usize, layout: Layout) -> wisp_render::plan_usage::PageRows {
        let endpoints = self.usage_endpoints();
        let states: Vec<_> = endpoints.iter().map(|e| self.usage_state(e)).collect();
        let accounts: Vec<_> = endpoints
            .iter()
            .zip(&states)
            .map(|(endpoint, state)| self.account_view(endpoint, state))
            .collect();
        let all = self.spend_answer.as_ref().and_then(|(days, window)| {
            all_chats(accounts.iter().map(|account| account.alias), days, window)
        });
        let now = (self.now)().as_second();
        let view = UsagePage {
            accounts: &accounts,
            total: page_total(&self.agents),
            all: all.as_ref(),
            now,
            scroll: self.usage_page.as_ref().map_or(0, |page| page.scroll),
        };
        wisp_render::plan_usage::page_rows(&view, room, layout, |at| {
            super::super::reset_time::view(&self.zone, now, at)
        })
    }

    fn usage_focus_visible(&mut self) {
        let layout = Layout::new(usize::from(self.size.width), self.glyphs);
        let focus = self.usage_rows(usize::MAX, layout).focus;
        if let Some(page) = self.usage_page.as_mut() {
            page.scroll = focus;
        }
    }

    pub(super) fn draw_usage_page(
        &mut self,
        buffer: &mut Buffer,
        room: u16,
        layout: Layout,
    ) -> Option<u16> {
        let rendered = self.usage_rows(usize::from(room), layout);
        if let Some(page) = self.usage_page.as_mut() {
            page.scroll = rendered.scroll;
            page.max_scroll = rendered.max_scroll;
        }
        let rows = rendered.rows;
        let top = room.saturating_sub(u16::try_from(rows.len()).unwrap_or(room));
        for y in top..room {
            for x in buffer.area.x..buffer.area.right() {
                if let Some(cell) = buffer.cell_mut(Position { x, y }) {
                    cell.reset();
                }
            }
        }
        for (index, row) in rows.iter().enumerate() {
            let y = top.saturating_add(u16::try_from(index).unwrap_or(room));
            buffer.set_line(0, y, &self.theme.line(row), buffer.area.width);
        }
        Some(top)
    }

    pub(super) fn invalidate_usage_alias(&mut self, alias: &str, at: Duration) {
        let interested = self.usage_endpoint.as_deref() == Some(alias)
            || self.usage_refreshes.values().any(|name| name == alias);
        self.plan_usage.remove(alias);
        self.usage_refreshes.retain(|_, name| name != alias);
        if self.usage_endpoint.as_deref() == Some(alias) {
            self.usage_endpoint = None;
            self.usage_initial = None;
        }
        let asked_again = self
            .usage_endpoints()
            .iter()
            .any(|endpoint| endpoint.name == alias && self.usage_eligible(endpoint));
        if interested && !asked_again {
            self.release_usage(alias.to_owned(), at);
        }
    }

    pub(super) fn usage_catalog_event(&mut self, previous: Vec<EndpointFacts>, at: Duration) {
        self.usage_catalog = true;
        let next = self.facts.endpoints.clone();
        let changed: Vec<_> = previous
            .iter()
            .filter(|old| !next.contains(old))
            .map(|old| old.name.clone())
            .collect();
        for alias in changed {
            self.invalidate_usage_alias(&alias, at);
        }
        for endpoint in next.iter().filter(|endpoint| !previous.contains(endpoint)) {
            self.usage_page_lookup(&endpoint.name, at);
        }
        if let Some(page) = &mut self.usage_page
            && !next.iter().any(|e| e.name == page.focus)
        {
            page.focus = next.first().map(|e| e.name.clone()).unwrap_or_default();
            page.scroll = 0;
        }
    }

    pub(super) fn usage_page_lookup(&mut self, alias: &str, at: Duration) {
        if self.usage_page.is_some() {
            self.refresh_usage(Some(alias), UsageAction::Subscribe, at);
        }
    }

    pub(super) fn usage_setting(&mut self, enabled: bool) {
        self.claude_usage = enabled;
        for endpoint in self
            .usage_endpoints()
            .into_iter()
            .filter(|e| e.credential == Some(Credential::AnthropicPlan))
        {
            if enabled {
                self.plan_usage.remove(&endpoint.name);
            } else {
                self.plan_usage.insert(
                    endpoint.name,
                    UsageState {
                        error: Some(PlanUsageError::Disabled),
                        ..empty()
                    },
                );
            }
        }
    }

    pub(super) fn usage_event(&mut self, event: &AgentEvent, at: Duration) {
        if matches!(event, AgentEvent::StepEnded { usage: Some(_), .. }) {
            self.refused_keys.remove(&self.tracker.session().endpoint);
        }
        if let AgentEvent::SettingsChanged { changes, .. } = event {
            for change in changes {
                if change.target.spec().name == names::CLAUDE_USAGE
                    && let Some(wisp_protocol::settings::SettingValue::Bool(enabled)) = change.value
                {
                    self.usage_setting(enabled);
                }
            }
        }
        match event {
            AgentEvent::SpendChanged { .. } | AgentEvent::AgentStateChanged { .. }
                if self.usage_page.is_some() =>
            {
                self.ask_spend(at)
            }
            AgentEvent::SpendListed {
                request,
                days,
                window,
            } if self.spend_request == Some(*request) => {
                self.spend_answer = Some((days.clone(), window.clone()));
                self.spend_request = None;
                if std::mem::take(&mut self.spend_again) && self.usage_page.is_some() {
                    self.ask_spend(at);
                }
            }
            AgentEvent::PlanUsageChanged {
                request,
                endpoint,
                state,
            } => {
                let correlated = request
                    .is_some_and(|request| self.usage_refreshes.get(&request) == Some(endpoint));
                let active = self.usage_endpoint.as_ref() == Some(endpoint);
                if !correlated && (!active || (request.is_some() && *request != self.usage_initial))
                {
                    return;
                }
                if !state.loading && *request == self.usage_initial {
                    self.usage_initial = None;
                }
                let shown = if !self.claude_usage
                    && self.usage_endpoints().iter().any(|facts| {
                        facts.name == *endpoint
                            && facts.credential == Some(Credential::AnthropicPlan)
                    }) {
                    UsageState {
                        error: Some(PlanUsageError::Disabled),
                        ..empty()
                    }
                } else {
                    state.clone()
                };
                self.plan_usage.insert(endpoint.clone(), shown);
                if !state.loading && correlated {
                    if let Some(request) = request {
                        self.usage_refreshes.remove(request);
                    }
                    if !active {
                        self.release_usage(endpoint.clone(), at);
                    }
                }
            }
            AgentEvent::Rejected {
                request: Some(request),
                ..
            } => {
                if let Some(endpoint) = self.usage_refreshes.remove(request) {
                    let state = self
                        .plan_usage
                        .entry(endpoint.clone())
                        .or_insert_with(empty);
                    state.loading = false;
                    state.stale = state.snapshot.is_some();
                    state.error = Some(PlanUsageError::Unsupported);
                    if self.usage_endpoint.as_ref() != Some(&endpoint) {
                        self.release_usage(endpoint, at);
                    }
                }
            }
            AgentEvent::PlanUsageWarning {
                endpoint, window, ..
            } => {
                let now = (self.now)().as_second();
                let rows = wisp_render::plan_usage::warning_rows(
                    endpoint,
                    window,
                    Layout::new(usize::from(self.size.width), self.glyphs),
                    |at| super::super::reset_time::view(&self.zone, now, at),
                );
                let text = rows
                    .iter()
                    .map(|row| {
                        row.spans
                            .iter()
                            .map(|s| s.text.as_str())
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                self.transcript.ephemeral_notice(&text);
            }
            _ => {}
        }
        self.subscribe_usage(at);
        self.usage_status();
        if self.usage_page.is_some() {
            self.frames.mark();
        }
    }

    pub(super) fn usage_status(&mut self) {
        let candidates = self
            .plan_usage
            .get(&self.tracker.session().endpoint)
            .map(|state| {
                if state.error == Some(PlanUsageError::AuthenticationRequired)
                    && let Some(endpoint) = self
                        .usage_endpoints()
                        .into_iter()
                        .find(|endpoint| endpoint.name == self.tracker.session().endpoint)
                {
                    let hint = crate::app::subscription_login::login_hint(
                        &wisp_render::chrome::one_line(&endpoint.name),
                    );
                    let sep = wisp_render::theme::glyphs::Glyphs::of(self.glyphs).separator;
                    return vec![wisp_render::plan_usage::UsageCandidate {
                        line: wisp_render::styled::StyledLine::new(vec![
                            wisp_render::styled::StyledSpan::new(
                                format!("sign-in expired {sep} {hint}"),
                                wisp_render::theme::Token::TextMuted,
                            ),
                        ]),
                        measured: false,
                        warns: false,
                    }];
                }
                wisp_render::plan_usage::mini_candidates(
                    state,
                    &self.tracker.session().model,
                    self.facts
                        .endpoint_facts(&self.tracker.session().endpoint)
                        .map(|endpoint| endpoint.provider.as_str()),
                    (self.now)().as_second(),
                    if self.usage_page.is_some() {
                        UsageHint::PageOpen
                    } else {
                        UsageHint::Show
                    },
                    Layout::new(usize::from(self.size.width), self.glyphs),
                )
            })
            .unwrap_or_default();
        if self.status.usage_candidates(candidates) {
            self.frames.mark();
        }
    }
}
