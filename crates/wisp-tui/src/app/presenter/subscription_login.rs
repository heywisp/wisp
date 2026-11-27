use std::time::Duration;

use ratatui_core::buffer::Buffer;
use wisp_protocol::settings::{EndpointSummary, SettingsSnapshot};
use wisp_protocol::subscription_auth::{
    AuthCommand, AuthEvent, AuthOutcome, Followed, LocalState, PresetGone, Stayed, Unusable, Way,
};
use wisp_protocol::{AgentEvent, ClientCommand, RequestId};
use wisp_render::chrome::account::{
    self, AccountKind, Asked, Kept, Move, Moved, Named, Said, SignedOut, Start, Who,
};
use wisp_render::chrome::{CardHit, CardRows, Credential, EndpointFacts, Rebuild, short_model};
use wisp_render::markdown::Layout;
use wisp_render::styled::{StyledLine, StyledSpan};
use wisp_render::theme::glyphs::Glyphs;

use super::gone::GoingOff;
use super::{AppError, Presenter, paint_lines};
use crate::app::endpoint_card::endpoint_facts_of;
use crate::app::logout::{self, Choice, Leaving, SignOut, WayRow};
use crate::app::picker::{Picked, display_effort};
use crate::app::settings::Answer;
use crate::app::subscription_login::sign_in::{Reaction, SignIn, Wire};
use crate::app::subscription_login::{self as login, Route, Wizard};
use crate::term::clipboard::Purpose;
use crate::term::{Console, Key, KeyCode, Mouse, MouseKind};

#[derive(Debug, Default)]
pub(super) struct Authentication {
    pub(super) sign_in: Option<SignIn>,
    pub(super) sign_out: Option<SignOut>,
    pub(super) going_off: Option<GoingOff>,
    pub(super) off_refusal: Option<RequestId>,
    describing: Option<(RequestId, bool)>,
    pub(super) none_left: bool,
    pending: Vec<(RequestId, Operation)>,
    scroll: usize,
    last: usize,
    page: usize,
    hits: Vec<(u16, CardHit)>,
    pub(super) link: Option<crate::term::screen::Link>,
    pub(super) then: Option<(Option<String>, Then)>,
    pub(super) gone: Option<(Vec<String>, Vec<Way>)>,
    pub(super) behind: Option<Then>,
    pub(super) drawn_gone: Vec<std::collections::BTreeSet<String>>,
    pub(super) held_gone: Option<Moved>,
    pub(super) gone_priced: (std::collections::BTreeSet<String>, Vec<(String, Rebuild)>),
    pub(super) unlisted: Vec<String>,
}

#[derive(Debug)]
pub(super) enum Then {
    Pick(Picked),
    Retry,
    Send,
    Release,
}

#[derive(Debug, Default)]
pub(super) struct Sent {
    pub(super) names: Vec<String>,
    pub(super) gone: bool,
    confirmed: bool,
    accept_api: bool,
    nothing_left: bool,
    moved: Option<Moved>,
    priced: Vec<(String, Rebuild)>,
}

#[derive(Debug)]
enum Operation {
    Accounts,
    Logout(Box<Sent>),
    Preview(Box<Sent>),
}

impl Presenter {
    pub(super) fn auth_open(&self) -> bool {
        self.authentication.sign_in.is_some() || self.authentication.sign_out.is_some()
    }

    pub(super) fn auth_deadline(&self) -> Option<Duration> {
        let sign_in = self.authentication.sign_in.as_ref();
        sign_in.and_then(|s| s.wizard.deadline())
    }

    pub(super) fn login_command(&mut self, words: &str, at: Duration) {
        if self.busy() || self.auth_open() {
            self.hint("Sign in between turns.", at);
            return;
        }
        let endpoints = login::endpoints(&self.facts.endpoints);
        let route = match login::route(words, &endpoints) {
            Ok(route) => route,
            Err(message) => {
                self.hint(&message, at);
                return;
            }
        };
        if route == Route::Status {
            self.open_quoted(self.chat_models(), at);
            return;
        }
        let request = self.next_request();
        match Wizard::new(route, endpoints, request) {
            Ok((wizard, intent)) => {
                self.inputs.unfocus();
                self.clear_selection();
                self.select.forget_clicks();
                self.select.drop_open();
                self.picker = None;
                self.authentication.sign_in = Some(SignIn::new(wizard));
                self.authentication.scroll = 0;
                if let Some(intent) = intent {
                    self.signing(at, |s, wire| s.act(intent, wire));
                }
            }
            Err(message) => self.hint(message, at),
        }
    }

    pub(super) fn ask_accounts(&mut self, at: Duration) {
        let subscribed = self
            .facts
            .endpoints
            .iter()
            .any(|e| e.credential.and_then(Credential::plan).is_some());
        if !subscribed {
            return;
        }
        let request = self.next_request();
        let command = AuthCommand::Status {
            request,
            alias: None,
        };
        if self.command(ClientCommand::SubscriptionAuth { command }, at) {
            self.authentication
                .pending
                .push((request, Operation::Accounts));
        }
    }

    pub(super) fn sign_in_then(&mut self, account: Option<String>, then: Then, at: Duration) {
        self.then_after(account.clone(), then);
        self.login_command(account.as_deref().unwrap_or_default(), at);
    }

    pub(super) fn then_after(&mut self, account: Option<String>, then: Then) {
        self.authentication.then = Some((account, then));
    }

    pub(super) fn signed_in_again(&mut self, alias: &str, at: Duration) {
        self.authentication.none_left = false;
        self.refused_keys.remove(alias);
        if let Some(facts) = self.facts.endpoints.iter_mut().find(|e| e.name == alias) {
            facts.expired = false;
        }
        match self.authentication.then.take() {
            Some((account, then)) if account.as_deref().is_none_or(|a| a == alias) => match then {
                Then::Pick(picked) => self.picked(picked, at),
                Then::Retry => self.retry_on(alias, at),
                Then::Send => self.send(wisp_protocol::DeliveryMode::Queue, at),
                Then::Release => {
                    self.command(ClientCommand::Release, at);
                }
            },
            other => self.authentication.then = other,
        }
    }

    pub(super) fn logout_command(&mut self, words: &str, at: Duration) {
        self.dismiss_preview();
        match words.trim() {
            "" | "all" => {
                if let Some(request) = self.ask_described(at) {
                    self.authentication.describing = Some((request, words.trim() == "all"));
                }
            }
            name => {
                let sent = Sent {
                    names: vec![name.to_owned()],
                    ..Sent::default()
                };
                self.send_logout(sent, None, Vec::new(), at);
            }
        }
    }

    pub(super) fn logout_described(&mut self, snapshot: &SettingsSnapshot, at: Duration) {
        let Some((_, all)) = self
            .authentication
            .describing
            .take_if(|(request, _)| *request == snapshot.answers)
        else {
            return;
        };
        let accounts = &snapshot.endpoints;
        let chat = self.tracker.wanted().endpoint;
        let agents = self.agents.rows().to_vec();
        let glyphs = Glyphs::of(self.glyphs);
        let sep = format!(" {} ", glyphs.separator);
        let file = self.keys_file_shown();
        let choice = |e: &EndpointSummary| {
            let users: Vec<String> = agents
                .iter()
                .filter(|row| row.entry.endpoint.as_deref() == Some(e.name.as_str()))
                .map(|row| row.entry.path.as_str().trim_start_matches('/').to_owned())
                .collect();
            let notes = account::row_notes(
                e.name == chat,
                e.default,
                e.keys_file.then_some(file.as_str()),
                &users,
            );
            let kind = endpoint_facts_of(e).account_kind();
            let kind = kind.map_or_else(String::new, |k| account::kind_words(k, &glyphs));
            let detail = |notes: &[String]| {
                std::iter::once(kind.clone())
                    .chain(notes.iter().cloned())
                    .collect::<Vec<_>>()
                    .join(&sep)
            };
            Choice {
                name: e.name.clone(),
                mark: e.mark,
                details: vec![detail(&notes), detail(&notes[..notes.len().min(1)])],
            }
        };
        let open = |card: SignOut, this: &mut Self| {
            this.inputs.unfocus();
            this.picker = None;
            this.authentication.sign_out = Some(card);
        };
        let names: Vec<String> = accounts
            .iter()
            .filter(|e| !e.keys_file)
            .map(|e| e.name.clone())
            .collect();
        match names.as_slice() {
            _ if accounts.is_empty() => self.transcript.note(account::NO_ACCOUNT),
            _ if !all => {
                let card = SignOut::which(accounts.iter().map(choice).collect());
                open(card, self);
            }
            _ => {
                let names: Vec<_> = if names.is_empty() {
                    accounts.iter().map(|e| e.name.clone()).collect()
                } else {
                    names
                };
                let request = self.next_request();
                let command = AuthCommand::PreviewLogout {
                    request,
                    names: names.clone(),
                };
                if self.command(ClientCommand::SubscriptionAuth { command }, at) {
                    let sent = Sent {
                        nothing_left: names.len() == accounts.len(),
                        names,
                        ..Sent::default()
                    };
                    self.authentication
                        .pending
                        .push((request, Operation::Preview(Box::new(sent))));
                }
            }
        }
    }

    pub(super) fn send_logout(
        &mut self,
        sent: Sent,
        chat: Option<String>,
        follow: Vec<String>,
        at: Duration,
    ) {
        let request = self.next_request();
        let mut command = AuthCommand::logout(request, &sent.names, sent.confirmed, chat, follow);
        if let AuthCommand::Logout {
            gone, accept_api, ..
        } = &mut command
        {
            *gone = sent.gone;
            *accept_api = sent.accept_api;
        }
        if self.command(ClientCommand::SubscriptionAuth { command }, at) {
            self.authentication
                .pending
                .push((request, Operation::Logout(Box::new(sent))));
        }
    }

    pub(super) fn held_move(&mut self, answer: Answer, before: &str) -> Answer {
        let sent = self
            .authentication
            .pending
            .iter_mut()
            .find_map(|(_, op)| match op {
                Operation::Logout(sent) => Some(sent),
                _ => None,
            });
        match (answer, sent) {
            (Answer::Moved(mut moved, _), Some(sent)) => {
                moved.how = Move::Continuing;
                sent.moved = Some(*moved);
                Answer::Other
            }
            (Answer::Moved(mut moved, _), None)
                if self
                    .authentication
                    .unlisted
                    .iter()
                    .any(|name| name == before) =>
            {
                moved.how = Move::Continuing;
                self.authentication.held_gone = Some(*moved);
                Answer::Other
            }
            (answer, _) => answer,
        }
    }

    pub(super) fn way_rows(&self, ways: &[Way], at: Duration) -> Vec<WayRow> {
        let from = self.facts.billing_on(&self.tracker.wanted().endpoint);
        let agents: Vec<_> = (self.agents.rows().iter())
            .filter(|r| self.agent_models.has(&r.entry.path))
            .map(|r| {
                let (facts, tracker) = self.pair_for(Some(&r.entry.path));
                let facts = facts.into_owned();
                logout::Mover {
                    entry: &r.entry,
                    facts,
                    tracker,
                }
            })
            .collect();
        logout::way_rows(
            &self.facts,
            &self.tracker,
            self.glyphs,
            ways,
            Leaving::Chat(from),
            Some(&agents),
            at,
        )
    }

    fn logout_needs(
        &mut self,
        (sent, preview): (Sent, bool),
        (keys, ways, presets, fixed): (Vec<String>, &[Way], Vec<PresetGone>, bool),
        at: Duration,
    ) {
        let rows = self.way_rows(ways, at);
        let named = |name: &String| (name.clone(), self.facts.mark_of(name));
        let names: Vec<Named> = sent.names.iter().map(named).collect();
        let keys: Vec<Named> = keys.iter().map(named).collect();
        let card = if fixed || preview || !keys.is_empty() || sent.names.len() > 1 {
            let row = match rows.as_slice() {
                [only] => Some(only),
                _ => None,
            };
            let moving =
                row.map(|w| w.moving(self.facts.account_state(&w.chat), !w.live || w.chat_moves));
            let behind = if fixed {
                Vec::new()
            } else {
                row.and_then(|row| ways.iter().find(|w| w.chat.as_deref() == Some(&row.chat)))
                    .into_iter()
                    .flat_map(|w| {
                        let to = w
                            .chat
                            .as_deref()
                            .and_then(|name| self.kind_of(name))
                            .map_or("", AccountKind::provider_word);
                        w.stay
                            .iter()
                            .map(move |a| (a.clone(), account::cant_move(to)))
                    })
                    .collect()
            };
            let way = row.map(|_| 0);
            SignOut::yes(
                Asked {
                    names,
                    moving,
                    behind,
                    keys,
                    presets,
                    nothing_left: sent.nothing_left,
                    ..Asked::default()
                },
                rows,
                way,
            )
        } else if !rows.is_empty() {
            SignOut::place(names, rows, !sent.confirmed).presets(presets)
        } else {
            return;
        };
        self.inputs.unfocus();
        self.picker = None;
        self.authentication.sign_out = Some(card);
    }

    pub(super) fn outstanding_previews(&self) -> usize {
        self.authentication
            .pending
            .iter()
            .filter(|(_, op)| matches!(op, Operation::Preview(_)))
            .count()
    }

    pub(super) fn dismiss_preview(&mut self) {
        self.authentication
            .pending
            .retain(|(_, op)| !matches!(op, Operation::Preview(_)));
    }

    pub(super) fn follower_receipt(
        &mut self,
        request: RequestId,
        to: &str,
        moved: &[Followed],
        priced: Option<&[(String, Rebuild)]>,
    ) {
        let priced = priced.or_else(|| {
            self.authentication
                .pending
                .iter()
                .find_map(|(id, op)| match op {
                    Operation::Logout(sent) if *id == request => Some(sent.priced.as_slice()),
                    _ => None,
                })
        });
        let Some(priced) = priced else {
            return;
        };
        let agents = self.followed(moved, to, priced);
        self.transcript.signed_out(SignedOut {
            agents,
            ..SignedOut::default()
        });
    }

    fn logged_out(&mut self, sent: Sent, outcome: &AuthOutcome, at: Duration) {
        let AuthOutcome::LoggedOut {
            alias,
            also,
            left_sign_ins,
            left_keys,
            new_start,
            moved,
            stay,
            stopped,
            presets,
            ..
        } = outcome
        else {
            return;
        };
        let gone: Vec<String> = std::iter::once(alias).chain(also).cloned().collect();
        for name in &gone {
            self.invalidate_usage_alias(name, at);
        }
        if sent.gone {
            self.draws_gone(&gone);
            let names = gone.iter().cloned().collect();
            self.authentication.gone_priced = (names, sent.priced.clone());
        }
        let dest = sent
            .moved
            .as_ref()
            .map_or_else(|| self.tracker.wanted().endpoint, |m| m.name.clone());
        let again = stopped.is_none()
            && (sent.nothing_left
                || (!self.facts.endpoints.is_empty()
                    && self.facts.endpoints.iter().all(|e| gone.contains(&e.name))));
        let out = SignedOut {
            gone: gone
                .iter()
                .map(|name| {
                    (
                        self.kind_of(name)
                            .unwrap_or(AccountKind::ClaudeSubscription),
                        name.clone(),
                    )
                })
                .collect(),
            left_sign_ins: left_sign_ins.len(),
            left_keys: left_keys.clone(),
            start_mark: new_start.as_ref().and_then(|name| self.facts.mark_of(name)),
            start: new_start.as_ref().map(|name| {
                let how = if *name == dest && sent.moved.is_some() {
                    Start::Too
                } else {
                    Start::Now
                };
                (name.clone(), self.facts.account_state(name), how)
            }),
            agents: self.followed(moved, &dest, &sent.priced),
            kept: self.kept(stay, &dest),
            presets: presets
                .iter()
                .map(|p| (p.clone(), login::login_hint(&p.account)))
                .collect(),
            again: again && !sent.nothing_left,
            chat: sent.moved,
            elsewhere: sent.gone,
            ..SignedOut::default()
        };
        let elsewhere = out.elsewhere;
        self.transcript.signed_out(out);
        if let Some(line) = stopped {
            self.transcript.note(line);
        }
        if elsewhere && matches!(self.authentication.then, Some((None, Then::Send))) {
            self.authentication.then = None;
            self.send(wisp_protocol::DeliveryMode::Queue, at);
        }
        self.settings_endpoints_changed();
        self.authentication.none_left |= again;
        if again && sent.nothing_left {
            self.login_command("", at);
        }
    }

    pub(super) fn followed(
        &self,
        moved: &[Followed],
        dest: &str,
        priced: &[(String, Rebuild)],
    ) -> Vec<(String, Moved, Option<Said>)> {
        moved
            .iter()
            .map(|f| {
                let to = f.to.as_deref().unwrap_or(dest);
                let state = self.facts.account_state(to);
                let moved = Moved {
                    how: Move::Switched,
                    name: to.to_owned(),
                    mark: self.facts.mark_of(to),
                    state,
                    from: None,
                    model: short_model(&f.model),
                    effort: display_effort(&self.facts, &f.model, Some(f.effort))
                        .map(|e| e.as_str().to_owned()),
                    rebuild: priced
                        .iter()
                        .find(|(agent, rebuild)| *agent == f.agent && *rebuild != Rebuild::Unknown)
                        .map(|(_, rebuild)| *rebuild)
                        .or_else(|| f.rebuild_usd.map(|usd| account::quoted(usd, state))),
                };
                let reason = f.from.as_ref().map(|u| {
                    let mark = self.facts.mark_of(&u.account);
                    Said::State(Who::Account(u.account.clone(), mark), u.state.clone())
                });
                (f.agent.clone(), moved, reason)
            })
            .collect()
    }

    pub(super) fn kind_of(&self, name: &str) -> Option<AccountKind> {
        self.facts
            .endpoint_facts(name)
            .and_then(EndpointFacts::account_kind)
    }

    pub(super) fn not_resumed(
        &self,
        stay: &[Stayed],
    ) -> Vec<(String, Unusable, Option<u8>, Option<String>)> {
        stay.iter()
            .filter_map(|s| {
                let from = s.from.clone()?;
                let fix = from.sign_in.then(|| login::login_hint(&from.account));
                let mark = self.facts.mark_of(&from.account);
                Some((s.agent.clone(), from, mark, fix))
            })
            .collect()
    }

    pub(super) fn kept(&self, stay: &[Stayed], dest: &str) -> Vec<Kept> {
        let to = self.kind_of(dest).map_or("", AccountKind::provider_word);
        stay.iter()
            .filter(|s| s.from.is_none())
            .map(|s| Kept {
                agent: s.agent.clone(),
                to: to.to_owned(),
                account: s.account.clone(),
                charges: s.charges,
                busy: s
                    .busy
                    .then(|| (dest.to_owned(), self.facts.account_state(dest))),
            })
            .collect()
    }

    fn sign_out_intent(&mut self, intent: logout::Intent, at: Duration) {
        let card = self.authentication.sign_out.take();
        self.authentication.hits.clear();
        if self.going_off_intent(&intent, card.as_ref(), at) {
            return;
        }
        if intent == logout::Intent::Close {
            self.authentication
                .then
                .take_if(|(_, then)| matches!(then, Then::Send | Then::Release));
        }
        if let logout::Intent::Send {
            names,
            confirmed,
            chat,
            follow,
            gone,
            accept_api,
        } = intent
        {
            let sent = Sent {
                names,
                gone,
                confirmed,
                accept_api,
                nothing_left: card.as_ref().is_some_and(SignOut::nothing_left),
                priced: card
                    .map(|card| card.movers_of(chat.as_deref()))
                    .unwrap_or_default(),
                ..Sent::default()
            };
            self.send_logout(sent, chat, follow, at);
        }
    }

    fn close_auth(&mut self) {
        if let Some(SignIn { wizard, .. }) = self.authentication.sign_in.take() {
            let glyphs = Glyphs::of(self.glyphs);
            let mark = self.facts.mark_of(wizard.alias());
            for line in wizard.signed_in_lines(mark, &glyphs) {
                self.transcript.note_line(line);
            }
            if let Some(refused) = wizard.refused() {
                self.transcript.same_account(refused.clone());
            }
        }
        self.authentication.behind = None;
        self.authentication.hits.clear();
    }

    fn signing(
        &mut self,
        at: Duration,
        act: impl FnOnce(&mut SignIn, &mut Wire<'_>) -> Option<Reaction>,
    ) {
        let Some(sign_in) = self.authentication.sign_in.as_mut() else {
            return;
        };
        let mut wire = Wire {
            requests: &mut self.requests,
            commands: self.commands.as_ref(),
            unsent: false,
        };
        let reaction = act(sign_in, &mut wire);
        if wire.unsent {
            self.hint(super::AGENT_STOPPED, at);
        }
        match reaction {
            Some(Reaction::Close) => {
                self.authentication.then = None;
                let behind = self.authentication.behind.take();
                self.close_auth();
                if let Some(then) = behind {
                    self.gone_card(then, at);
                }
            }
            Some(Reaction::AddKey) => {
                self.close_auth();
                self.open_endpoint_card(at);
            }
            Some(Reaction::Use(alias)) => {
                let behind = self.authentication.behind.take();
                self.close_auth();
                self.change_settings(Some(alias), None, None, at);
                if matches!(behind, Some(Then::Send)) {
                    self.send(wisp_protocol::DeliveryMode::Queue, at);
                }
            }
            Some(Reaction::Ready {
                alias,
                reauthorizing,
            }) => self.ready(&alias, reauthorizing, at),
            Some(Reaction::Saved(_)) | None => {}
        }
    }

    fn ready(&mut self, alias: &str, reauthorizing: bool, at: Duration) {
        let here = alias == self.tracker.session().endpoint;
        if let Some(sign_in) = self.authentication.sign_in.as_mut() {
            sign_in.wizard.offer_use(&self.tracker.session().endpoint);
        }
        if reauthorizing {
            self.invalidate_usage_alias(alias, at);
            if here {
                self.subscribe_usage(at);
            }
            self.close_auth();
        } else {
            self.ask_described(at);
        }
        self.settings_endpoints_changed();
        self.signed_in_again(alias, at);
        if reauthorizing {
            self.usage_page_lookup(alias, at);
        }
    }

    pub(super) fn auth_copy<C: Console>(
        &mut self,
        console: &mut C,
        at: Duration,
    ) -> Result<(), AppError> {
        let sign_in = self.authentication.sign_in.as_mut();
        let Some(link) = sign_in.and_then(|s| s.wizard.take_copy()) else {
            return Ok(());
        };
        self.copy_text(console, link, Purpose::Link, at)
    }

    pub(super) fn auth_tick(&mut self, at: Duration) {
        if self
            .authentication
            .sign_in
            .as_ref()
            .is_some_and(|s| s.wizard.deadline().is_some_and(|deadline| at >= deadline))
        {
            self.signing(at, |s, wire| s.tick(at, wire));
            self.frames.mark();
        }
    }

    pub(super) fn auth_close(&mut self, at: Duration) {
        self.authentication.sign_out = None;
        self.signing(at, SignIn::cancel);
        self.close_auth();
    }

    pub(super) fn auth_key(&mut self, key: Key, at: Duration) {
        self.auth_tick(at);
        match key.code {
            KeyCode::PageUp => {
                self.authentication.scroll = self
                    .authentication
                    .scroll
                    .saturating_sub(self.authentication.page.max(1))
            }
            KeyCode::PageDown => {
                self.authentication.scroll = self
                    .authentication
                    .scroll
                    .saturating_add(self.authentication.page.max(1))
                    .min(self.authentication.last)
            }
            _ => {
                if let Some(intent) = self
                    .authentication
                    .sign_out
                    .as_mut()
                    .and_then(|c| c.key(key))
                {
                    self.sign_out_intent(intent, at);
                } else {
                    self.signing(at, |s, wire| s.key(key, at, wire));
                }
            }
        }
    }

    pub(super) fn auth_paste(&mut self, text: &str, at: Duration) {
        self.auth_tick(at);
        if let Some(sign_in) = &mut self.authentication.sign_in {
            sign_in.wizard.paste(text);
        }
    }

    pub(super) fn auth_mouse(&mut self, mouse: Mouse, at: Duration) {
        self.auth_tick(at);
        match mouse.kind {
            MouseKind::WheelUp => {
                self.authentication.scroll = self.authentication.scroll.saturating_sub(1)
            }
            MouseKind::WheelDown => {
                self.authentication.scroll = self
                    .authentication
                    .scroll
                    .saturating_add(1)
                    .min(self.authentication.last)
            }
            MouseKind::Press => {
                let hit = super::card::hit_at(&self.authentication.hits, mouse.row);
                match hit {
                    None => {}
                    Some(hit) if self.authentication.sign_out.is_some() => {
                        let card = self.authentication.sign_out.as_mut();
                        if let Some(intent) = card.and_then(|card| card.click(hit)) {
                            self.sign_out_intent(intent, at);
                        }
                    }
                    Some(hit) => {
                        self.signing(at, |s, wire| s.click(hit, mouse.modified, at, wire));
                    }
                }
            }
            MouseKind::Drag | MouseKind::Release | MouseKind::RightPress | MouseKind::Move => {}
        }
    }

    pub(super) fn auth_event(&mut self, event: &AgentEvent, at: Duration) -> bool {
        let AgentEvent::SubscriptionAuth {
            event: AuthEvent { request, outcome },
        } = event
        else {
            return false;
        };
        if let Some(index) = self
            .authentication
            .pending
            .iter()
            .position(|(pending, _)| pending == request)
        {
            let (_, operation) = self.authentication.pending.remove(index);
            let preview = matches!(operation, Operation::Preview(_));
            match (operation, outcome) {
                (Operation::Accounts, AuthOutcome::Status { endpoints }) => {
                    for endpoint in endpoints {
                        if let Some(facts) = self
                            .facts
                            .endpoints
                            .iter_mut()
                            .find(|e| e.name == endpoint.alias)
                        {
                            facts.expired = endpoint.state == LocalState::Expired;
                        }
                    }
                }
                (Operation::Accounts, AuthOutcome::Failed { .. }) => {}
                (Operation::Logout(sent), AuthOutcome::LoggedOut { .. }) => {
                    self.logged_out(*sent, outcome, at);
                }
                (
                    Operation::Logout(sent) | Operation::Preview(sent),
                    AuthOutcome::LogoutNeeds {
                        fixed,
                        deletes_keys_of,
                        ways,
                        presets,
                        ..
                    },
                ) => {
                    let keys = deletes_keys_of.clone();
                    self.logout_needs((*sent, preview), (keys, ways, presets.clone(), *fixed), at);
                }
                (
                    Operation::Logout(sent) | Operation::Preview(sent),
                    AuthOutcome::Failed { message, .. },
                ) => {
                    if let Some(moved) = sent.moved {
                        self.transcript.moved(moved);
                    }
                    self.transcript.note(message);
                }
                _ => {}
            }
        } else {
            self.signing(at, |s, wire| s.event(*request, outcome, at, wire));
        }
        self.view.sync(self.transcript.cells().len());
        self.frames.mark();
        true
    }

    pub(super) fn auth_card(&self, layout: Layout) -> Option<CardRows> {
        match (&self.authentication.sign_in, &self.authentication.sign_out) {
            (Some(sign_in), _) => {
                let mark = self.facts.mark_of(sign_in.wizard.alias());
                Some(sign_in.wizard.render(usize::MAX, layout, mark))
            }
            (None, Some(card)) => Some(card.render(usize::MAX, layout)),
            (None, None) => None,
        }
    }

    pub(super) fn draw_auth(
        &mut self,
        buffer: &mut Buffer,
        view_rows: u16,
        layout: Layout,
    ) -> Option<u16> {
        self.authentication.hits.clear();
        let rows = self.auth_card(layout)?;
        let room = usize::from(view_rows);
        if room == 0 {
            return None;
        }
        let overflow = rows.lines.len() > room;
        let page = room.saturating_sub(usize::from(overflow)).max(1);
        self.authentication.page = page;
        self.authentication.last = rows.lines.len().saturating_sub(page);
        self.authentication.scroll = self.authentication.scroll.min(self.authentication.last);
        let start = if overflow {
            self.authentication.scroll
        } else {
            0
        };
        let shown = rows.lines.len().saturating_sub(start).min(page);
        let height = shown.saturating_add(usize::from(overflow && room > 1));
        let top = view_rows.saturating_sub(u16::try_from(height).unwrap_or(u16::MAX));
        let lines = rows.lines.iter().zip(&rows.hits).skip(start).take(shown);
        self.authentication.hits = super::card::paint_rows(buffer, top, lines, &self.theme);
        if let Some(card) = &mut self.authentication.sign_out {
            card.drawn = true;
        }
        let sign_in = self.authentication.sign_in.as_ref();
        self.authentication.link = sign_in.and_then(|s| s.link(&rows, top, start..start + shown));
        if overflow && room > 1 {
            let more = if start + shown < rows.lines.len() {
                "more below"
            } else {
                "more above"
            };
            let line = StyledLine::new(vec![StyledSpan::plain(format!(
                "{more} · PgUp/PgDn scroll"
            ))]);
            let row = view_rows - 1;
            paint_lines(buffer, row, buffer.area.width, [&line], &self.theme);
        }
        Some(top)
    }
}
