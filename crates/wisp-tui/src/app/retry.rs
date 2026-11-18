use std::time::Duration;

use wisp_protocol::{
    AgentEvent, ClientCommand, Effort, ErrorKind, GaveUp, RejectKind, RequestId, RetryClass,
    RetryFocus, RetryOffer, TurnOutcome, starts_charging,
};
use wisp_render::chrome::{
    Alive, Choice as MenuChoice, Credential, Dock, Facts, KeyHint, Menu, Rebuild, Scope, Shape,
    Under, account, model_line, model_name,
};
use wisp_render::theme::glyphs::{GlyphSet, Glyphs};
use wisp_render::transcript::{Choice as Shown, ChoiceRow, copy};

use super::picker::{FOLDED_MODELS_FROM, accounts, display_effort, model_on, shown_effort};
use super::settings::{Chosen, Tracker};
use crate::app::list::{ListCursor, next_enabled};
use crate::term::{Key, KeyCode};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Retry,
    Switch,
    SignIn,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Retry,
    SignIn,
    NewKey,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failed {
    pub turn: u64,
    pub offer: RetryOffer,
    pub committed_steps: u32,
    pub wait_ms: Option<u64>,
    pub endpoint: String,
    pub model: String,
    pub effort: Option<Effort>,
    pub kind: ErrorKind,
    pub limited: bool,
    pub resets_at: Option<i64>,
}

impl Failed {
    pub fn of(turn: u64, outcome: &TurnOutcome, failed_on: &Chosen) -> Option<Failed> {
        let TurnOutcome::Error {
            kind,
            offer: Some(offer),
            committed_steps,
            gave_up,
            detail,
            ..
        } = outcome
        else {
            return None;
        };
        let limit = detail.as_ref().and_then(|d| d.limit_reached.as_deref());
        let ended_wait = gave_up == &Some(GaveUp::RetryAfterTooLong)
            || gave_up.is_none()
                && detail
                    .as_ref()
                    .is_some_and(|d| d.retry != RetryClass::Never);
        let wait_ms = match (gave_up, detail.as_ref().map(|d| d.retry)) {
            (Some(GaveUp::RetryAfterTooLong), Some(RetryClass::After { ms }))
                if limit.is_none() =>
            {
                Some(ms)
            }
            _ => None,
        };
        Some(Failed {
            turn,
            offer: offer.clone(),
            committed_steps: *committed_steps,
            wait_ms,
            endpoint: failed_on.endpoint.clone(),
            model: failed_on.model.clone(),
            effort: failed_on.effort,
            kind: *kind,
            limited: limit.is_some(),
            resets_at: limit.and_then(|w| w.resets_at).filter(|_| !ended_wait),
        })
    }

    pub fn rows(&self, next_endpoint: &str, wall: i64) -> Vec<Row> {
        let mut rows = Vec::new();
        let holds = self.limited && self.resets_at.is_some_and(|at| wall < at);
        if !holds && self.offer.allows(next_endpoint == self.endpoint) {
            rows.push(Row::Retry);
        }
        if self.offer.switch {
            rows.push(Row::Switch);
        } else if self.limited {
            rows.push(Row::SignIn);
        }
        rows
    }

    pub fn route(&self, facts: &Facts) -> Route {
        let account = facts.endpoint_facts(&self.endpoint);
        match (self.kind, account.and_then(|a| a.credential)) {
            (
                ErrorKind::Auth,
                Some(
                    Credential::AnthropicPlan
                    | Credential::ChatGptPlan
                    | Credential::AntigravityPlan,
                ),
            ) => Route::SignIn,
            (ErrorKind::Auth, _) if account.is_some_and(|a| a.key_var.is_none()) => Route::NewKey,
            _ => Route::Retry,
        }
    }

    fn step(&self) -> Option<u32> {
        (self.committed_steps > 0).then(|| self.committed_steps.saturating_add(1))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    Stay,
    Retry { endpoint: Option<String> },
    Pick,
    SignIn { account: Option<String> },
    NewKey { account: String },
    Dismissed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Sent {
    request: RequestId,
    on: Option<String>,
    cost: String,
    account_bound: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Stage {
    Open,
    Sent(Sent),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Pick {
    selected: Option<usize>,
    checking: Option<RequestId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seen {
    Nothing,
    Lapsed,
    Retried { used: String, note: Option<String> },
    Refused(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Choice {
    failed: Option<Failed>,
    stage: Option<Stage>,
    focus: Option<Row>,
    checking: Option<RequestId>,
    pick: Option<Pick>,
    cell: u32,
    turn: u64,
}

pub struct Ctx<'a> {
    pub facts: &'a Facts,
    pub tracker: &'a Tracker,
    pub at: Duration,
    pub wall: i64,
    pub set: GlyphSet,
}

impl Ctx<'_> {
    fn sep(&self) -> String {
        format!(" {} ", Glyphs::of(self.set).separator)
    }

    fn priced(&self, endpoint: &str) -> bool {
        self.facts.endpoint_facts(endpoint).is_none_or(|e| e.priced)
    }

    fn rewrite(&self, endpoint: &str, model: &str) -> Rebuild {
        let rates = self.facts.rates_on(endpoint, model);
        let alive = Alive {
            prefix: true,
            tail: true,
        };
        let rebuild = Rebuild::estimate(
            self.tracker.cache_size(),
            Scope::Exact,
            alive,
            rates,
            self.facts.writes_on(endpoint, model, self.facts.cache_ttl),
        )
        .billed(self.facts.billing_on(endpoint));
        if self.priced(endpoint) {
            rebuild
        } else {
            rebuild.unpriced()
        }
    }

    fn rewrite_amount(&self, endpoint: &str, model: &str) -> String {
        self.rewrite(endpoint, model).amount(self.set)
    }

    fn warm(&self, endpoint: &str, model: &str) -> Option<String> {
        let total = self.tracker.cache_size().total;
        if total == 0 {
            return Some(copy::NOTHING_CACHED_SHORT.to_owned());
        }
        if !self.tracker.alive(self.at).tail {
            return None;
        }
        let rates = self.facts.rates_on(endpoint, model);
        Some(match rates.filter(|_| self.priced(endpoint)) {
            Some(rates) => {
                let usd = total as f64 * rates.cache_read_for(total) / 1_000_000.0;
                format!(
                    "{} {}",
                    copy::CACHE_WARM,
                    wisp_protocol::marked_amount(&copy::usd(usd), true, false)
                )
            }
            None => format!("{} ({})", copy::CACHE_WARM, copy::NO_PRICES),
        })
    }

    fn retry_cost(&self, endpoint: &str, model: &str) -> String {
        self.warm(endpoint, model).unwrap_or_else(|| {
            format!(
                "{} {}",
                copy::CACHE_MAY_HAVE_EXPIRED,
                self.rewrite_amount(endpoint, model)
            )
        })
    }

    fn switch_cost(&self, endpoint: &str, model: &str) -> String {
        match self.rewrite(endpoint, model) {
            Rebuild::Unpriced(_) => format!("{} ({})", copy::SWITCH_REBUILDS, copy::NO_PRICES),
            rebuild => rebuild.line(self.set),
        }
    }
}

impl Choice {
    pub fn failed(&self) -> Option<&Failed> {
        self.failed.as_ref()
    }

    pub fn is_open(&self) -> bool {
        self.failed.is_some() && self.stage == Some(Stage::Open)
    }

    pub fn is_picking(&self) -> bool {
        self.is_open() && self.pick.is_some()
    }

    pub fn cell(&self) -> u32 {
        self.cell
    }

    pub fn turn(&self) -> u64 {
        self.turn
    }

    pub fn offered(&mut self, failed: Failed, cell: u32, checking: Option<RequestId>) {
        self.focus = Some(match failed.offer.focus {
            RetryFocus::Retry => Row::Retry,
            RetryFocus::Switch => Row::Switch,
        });
        self.failed = Some(failed);
        self.stage = Some(Stage::Open);
        self.checking = checking;
        self.pick = None;
        self.cell = cell;
    }

    pub fn clear(&mut self) {
        *self = Choice {
            cell: self.cell,
            turn: self.turn,
            ..Choice::default()
        };
    }

    pub fn answered(&mut self, request: RequestId) {
        if self.checking == Some(request) {
            self.checking = None;
        }
        if let Some(pick) = &mut self.pick
            && pick.checking == Some(request)
        {
            pick.checking = None;
        }
    }

    pub fn unpick(&mut self) {
        self.pick = None;
    }

    pub fn picking(&mut self, checking: Option<RequestId>) {
        if self.is_open() {
            self.pick = Some(Pick {
                selected: None,
                checking,
            });
        }
    }

    pub fn expects_switch(&self, endpoint: &str) -> bool {
        matches!(&self.stage, Some(Stage::Sent(sent)) if sent.on.as_deref() == Some(endpoint))
    }

    pub fn switched(&mut self, account_bound: &[String]) {
        if let Some(Stage::Sent(sent)) = &mut self.stage {
            sent.account_bound = account_bound.to_vec();
        }
    }

    pub fn sent_request(&self) -> Option<RequestId> {
        match &self.stage {
            Some(Stage::Sent(sent)) => Some(sent.request),
            _ => None,
        }
    }

    fn rows_of(&self, cx: &Ctx<'_>) -> Vec<Row> {
        self.failed
            .as_ref()
            .map_or_else(Vec::new, |f| f.rows(&cx.tracker.wanted().endpoint, cx.wall))
    }

    fn focused(&self, rows: &[Row]) -> usize {
        self.focus
            .and_then(|f| rows.iter().position(|r| *r == f))
            .unwrap_or(0)
    }

    fn retry_text(&self, failed: &Failed, cx: &Ctx<'_>) -> String {
        let sep = cx.sep();
        let next = cx.tracker.wanted().endpoint;
        let moved = next != failed.endpoint;
        let detail = if moved {
            copy::on_endpoint(&next)
        } else {
            failed
                .step()
                .map_or_else(|| copy::SENDS_AGAIN.to_owned(), copy::from_step)
        };
        let cost = if let Some(ms) = failed.wait_ms {
            copy::asked_to_wait(&copy::duration(ms))
        } else if self.checking.is_some() {
            copy::checking(cx.set)
        } else if moved {
            let model = model_on(cx.facts, &failed.endpoint, &next, &failed.model);
            cx.switch_cost(&next, &model)
        } else {
            cx.retry_cost(&failed.endpoint, &failed.model)
        };
        format!("{detail}{sep}{cost}")
    }

    pub fn shown(&self, cx: &Ctx<'_>, dimmed: bool) -> Option<Shown> {
        let failed = self.failed.as_ref()?;
        match self.stage.as_ref()? {
            Stage::Sent(sent) => Some(Shown::Used(copy::retried(sent.on.as_deref()))),
            Stage::Open => {
                let rows = self.rows_of(cx);
                if rows.is_empty() {
                    return None;
                }
                let focus = self.focused(&rows);
                let route = failed.route(cx.facts);
                let rows = rows
                    .iter()
                    .map(|row| match (row, route) {
                        (Row::Retry, Route::Retry) => ChoiceRow {
                            label: copy::RETRY.to_owned(),
                            text: self.retry_text(failed, cx),
                        },
                        (Row::Retry, Route::SignIn) => ChoiceRow {
                            label: copy::SIGN_IN_THEN_RETRY.to_owned(),
                            text: String::new(),
                        },
                        (Row::Retry, Route::NewKey) => ChoiceRow {
                            label: copy::NEW_KEY_THEN_RETRY.to_owned(),
                            text: String::new(),
                        },
                        (Row::SignIn, _) => ChoiceRow {
                            label: account::sign_in_row(&Glyphs::of(cx.set)),
                            text: String::new(),
                        },
                        (Row::Switch, _) => ChoiceRow {
                            label: copy::SWITCH_ACCOUNT.to_owned(),
                            text: format!(
                                "{}{}{}",
                                copy::PICK_THEN_RETRY,
                                cx.sep(),
                                copy::SWITCH_REBUILDS
                            ),
                        },
                    })
                    .collect();
                Some(Shown::Open {
                    rows,
                    focus,
                    dimmed: dimmed || self.pick.is_some(),
                })
            }
        }
    }

    fn pick_rows(&self, cx: &Ctx<'_>) -> Vec<(String, bool, bool)> {
        let Some(failed) = &self.failed else {
            return Vec::new();
        };
        let from = cx.facts.billing_on(&failed.endpoint);
        cx.facts
            .endpoints
            .iter()
            .filter(|e| e.name != failed.endpoint)
            .map(|e| {
                let charges = starts_charging(Some(from), cx.facts.billing_on(&e.name));
                (e.name.clone(), e.available(), charges)
            })
            .collect()
    }

    fn pick_selection(&self, rows: &[(String, bool, bool)]) -> Option<usize> {
        let failed = self.failed.as_ref()?;
        if let Some(i) = self.pick.as_ref().and_then(|p| p.selected)
            && rows.get(i).is_some_and(|(_, enabled, _)| *enabled)
        {
            return Some(i);
        }
        let free = |(_, enabled, charges): &(String, bool, bool)| *enabled && !*charges;
        let suggested = failed
            .offer
            .suggest
            .as_deref()
            .and_then(|name| rows.iter().position(|row| row.0 == name && free(row)));
        suggested.or_else(|| rows.iter().position(free))
    }

    fn pick_charges(&self, cx: &Ctx<'_>) -> Option<bool> {
        let rows = self.pick_rows(cx);
        self.pick_selection(&rows)
            .and_then(|i| rows.get(i))
            .map(|row| row.2)
    }

    pub fn dock(&self, cx: &Ctx<'_>) -> Dock {
        let keys = copy::pick_keys(
            self.pick_charges(cx),
            copy::use_the_api(),
            KeyHint::new(copy::ENTER_KEY, copy::RETRY_WORD),
            Some(copy::esc(copy::BACK_WORD)),
            cx.set,
        );
        Dock {
            name: copy::RETRY_ON_TITLE.to_owned(),
            column: String::new(),
            keys,
            ..Dock::default()
        }
    }

    pub fn pick_on(&mut self, name: &str, cx: &Ctx<'_>) {
        let selected = self.pick_rows(cx).iter().position(|row| row.0 == name);
        if self.is_open() {
            self.pick = Some(Pick {
                selected,
                checking: None,
            });
        }
    }

    pub fn menu(&self, cx: &Ctx<'_>) -> Option<Menu> {
        let failed = self.failed.as_ref()?;
        let pick = self.pick.as_ref()?;
        let sep = cx.sep();
        let rows = self.pick_rows(cx);
        let selected = self.pick_selection(&rows);
        let glyphs = Glyphs::of(cx.set);
        let all = accounts(cx.facts, cx.tracker);
        let from = all.iter().find(|f| f.name == failed.endpoint);
        let mut heads = Vec::new();
        let mut unders = Vec::new();
        let choices = rows
            .iter()
            .enumerate()
            .map(|(i, (name, enabled, _))| {
                let Some(a) = all.iter().find(|a| a.name == *name) else {
                    return MenuChoice::default();
                };
                let model = model_on(cx.facts, &failed.endpoint, name, &failed.model);
                let (choice, head, model) =
                    a.folded(cx.facts, cx.tracker, i, &model, failed.effort, &glyphs);
                heads.push(head);
                let rewrite = cx.rewrite(name, &model);
                let price = if pick.checking.is_some() {
                    copy::checking(cx.set)
                } else if matches!(rewrite, Rebuild::Empty | Rebuild::Expired) {
                    rewrite.cell()
                } else {
                    rewrite.short(cx.set)
                };
                if *enabled {
                    unders.push((
                        i,
                        Under {
                            mark: None,
                            first: Vec::new(),
                            parts: vec![rewrite.parts(cx.set, false)],
                            after: a.no_carry(from).into_iter().collect(),
                        },
                    ));
                }
                MenuChoice {
                    effect: format!("{}{sep}{price}", choice.effect),
                    enabled: *enabled,
                    ..choice
                }
            })
            .collect();
        let under = unders
            .iter()
            .find(|(i, _)| Some(*i) == selected)
            .map(|(_, u)| u.clone());
        Some(Menu {
            titles: Default::default(),
            choices,
            selected,
            notes: Vec::new(),
            shape: Shape {
                heads,
                under,
                reserve: unders.into_iter().map(|(_, u)| u).collect(),
                effect_from: FOLDED_MODELS_FROM,
            },
        })
    }

    fn step_pick(&mut self, rows: &[(String, bool, bool)], down: bool) {
        let enabled: Vec<bool> = rows.iter().map(|row| row.1).collect();
        let to = next_enabled(self.pick_selection(rows), &enabled, down);
        if let (Some(pick), Some(to)) = (&mut self.pick, to) {
            pick.selected = Some(to);
        }
    }

    fn pick_key(&mut self, key: Key, cx: &Ctx<'_>) -> Act {
        let rows = self.pick_rows(cx);
        match key.code {
            KeyCode::Esc => {
                self.pick = None;
                Act::Stay
            }
            KeyCode::Char('c' | 'C') if key.ctrl => {
                self.pick = None;
                Act::Stay
            }
            _ if key.ctrl || key.alt => Act::Stay,
            KeyCode::Up | KeyCode::BackTab => {
                self.step_pick(&rows, false);
                Act::Stay
            }
            KeyCode::Down | KeyCode::Tab => {
                self.step_pick(&rows, true);
                Act::Stay
            }
            KeyCode::Enter => match self.pick_selection(&rows).and_then(|i| rows.get(i)) {
                Some((name, ..)) => Act::Retry {
                    endpoint: Some(name.clone()),
                },
                None => Act::Stay,
            },
            _ => Act::Stay,
        }
    }

    pub fn key(&mut self, key: Key, cx: &Ctx<'_>) -> Option<Act> {
        if !self.is_open() {
            return None;
        }
        if self.pick.is_some() {
            return Some(self.pick_key(key, cx));
        }
        let rows = self.rows_of(cx);
        if rows.is_empty() || key.ctrl || key.alt {
            return None;
        }
        let at = self.focused(&rows);
        match key.code {
            code if let Some(to) = ListCursor::WRAP.key(code, at, rows.len()) => {
                self.focus = rows.get(to).copied();
                Some(Act::Stay)
            }
            KeyCode::Esc => {
                self.stage = None;
                Some(Act::Dismissed)
            }
            KeyCode::Enter => {
                let failed = self.failed.as_ref()?;
                match (rows.get(at), failed.route(cx.facts)) {
                    (Some(Row::Retry), Route::Retry) => Some(Act::Retry { endpoint: None }),
                    (Some(Row::Retry), Route::SignIn) => Some(Act::SignIn {
                        account: Some(failed.endpoint.clone()),
                    }),
                    (Some(Row::Retry), Route::NewKey) => Some(Act::NewKey {
                        account: failed.endpoint.clone(),
                    }),
                    (Some(Row::Switch), _) => Some(Act::Pick),
                    (Some(Row::SignIn), _) => Some(Act::SignIn { account: None }),
                    (None, _) => None,
                }
            }
            _ => None,
        }
    }

    pub fn command(&self, endpoint: Option<String>, request: RequestId) -> Option<ClientCommand> {
        let failed = self.failed.as_ref()?;
        Some(ClientCommand::Retry {
            turn: failed.turn,
            endpoint,
            request,
        })
    }

    pub fn sent(&mut self, endpoint: Option<&str>, request: RequestId, cx: &Ctx<'_>) {
        let Some(failed) = &self.failed else {
            return;
        };
        let wanted = cx.tracker.wanted().endpoint;
        let target = endpoint.unwrap_or(&wanted).to_owned();
        let same = target == failed.endpoint;
        let model = model_on(cx.facts, &failed.endpoint, &target, &failed.model);
        let cost = if same {
            cx.retry_cost(&target, &model)
        } else {
            cx.switch_cost(&target, &model)
        };
        self.pick = None;
        self.stage = Some(Stage::Sent(Sent {
            request,
            on: (!same).then_some(target),
            cost,
            account_bound: Vec::new(),
        }));
    }

    fn note(&self, sent: &Sent, cx: &Ctx<'_>) -> Option<String> {
        let failed = self.failed.as_ref()?;
        let sep = cx.sep();
        let endpoint = sent.on.as_deref().unwrap_or(&failed.endpoint);
        let model = model_on(cx.facts, &failed.endpoint, endpoint, &failed.model);
        let effort = if endpoint == failed.endpoint {
            display_effort(cx.facts, &model, failed.effort)
        } else {
            shown_effort(cx.facts, &model, failed.effort)
        };
        let mut parts =
            copy::retrying_on(failed.step(), endpoint, &model, effort.map(Effort::as_str)).to_vec();
        parts.push(sent.cost.clone());
        if !sent.account_bound.is_empty() {
            let names: Vec<String> = sent.account_bound.iter().map(|m| model_name(m)).collect();
            parts.push(copy::reasoning_may_not_carry(&names));
        }
        let next = cx.tracker.next();
        let model_waits = next.model != model;
        let next_effort = display_effort(cx.facts, &next.model, next.effort);
        let effort_waits = next_effort != effort;
        let waiting = match (model_waits, effort_waits, next_effort) {
            (true, true, effort) => Some(copy::model_at(&next.model, effort.map(Effort::as_str))),
            (true, false, _) => Some(model_line(&next.model)),
            (false, true, Some(effort)) => Some(effort.as_str().to_owned()),
            _ => None,
        };
        if let Some(change) = waiting {
            parts.push(copy::applies_next(&change));
        }
        Some(parts.join(&sep))
    }

    pub fn event(&mut self, event: &AgentEvent, cx: &Ctx<'_>) -> Seen {
        match event {
            AgentEvent::TurnStarted {
                turn,
                retry_of: Some(_),
                answers: Some(answers),
                ..
            } if self.sent_request() == Some(*answers) => {
                let seen = match &self.stage {
                    Some(Stage::Sent(sent)) => Seen::Retried {
                        used: copy::retried(sent.on.as_deref()),
                        note: self.note(sent, cx),
                    },
                    _ => Seen::Nothing,
                };
                self.turn = *turn;
                self.clear();
                seen
            }
            AgentEvent::TurnStarted {
                turn,
                retry_of: Some(_),
                ..
            } if let Some(failed) = &self.failed => {
                let on = cx.tracker.session();
                let effort = display_effort(cx.facts, &on.model, on.effort);
                let seen = Seen::Retried {
                    used: copy::retried(
                        Some(on.endpoint.as_str()).filter(|e| *e != failed.endpoint),
                    ),
                    note: Some(
                        copy::retrying_on(
                            failed.step(),
                            &on.endpoint,
                            &on.model,
                            effort.map(Effort::as_str),
                        )
                        .join(&cx.sep()),
                    ),
                };
                self.turn = *turn;
                self.clear();
                seen
            }
            AgentEvent::SessionResumed { turns, .. } => {
                self.turn = *turns;
                Seen::Nothing
            }
            AgentEvent::TurnStarted { turn, .. } => {
                self.turn = *turn;
                let live = self.failed.is_some();
                self.clear();
                if live { Seen::Lapsed } else { Seen::Nothing }
            }
            AgentEvent::Compacted { .. } if self.is_open() => {
                self.clear();
                Seen::Lapsed
            }
            AgentEvent::Rejected {
                kind: Some(RejectKind::NotRetryable),
                request: Some(request),
                reason,
                ..
            } if self.sent_request() == Some(*request) => {
                self.clear();
                Seen::Refused(copy::retry_refused(reason))
            }
            AgentEvent::SettingsSnapshot(snapshot) => {
                self.answered(snapshot.answers);
                Seen::Nothing
            }
            _ => Seen::Nothing,
        }
    }
}

#[cfg(test)]
mod tests;
