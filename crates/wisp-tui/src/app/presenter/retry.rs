use std::time::Duration;

use wisp_protocol::{AgentEvent, ErrorKind, starts_charging};
use wisp_render::chrome::account::{self, AccountState, Ending};
use wisp_render::chrome::{dock_room, docked_rows};
use wisp_render::markdown::Layout;
use wisp_render::styled::StyledLine;
use wisp_render::theme::glyphs::Glyphs;
use wisp_render::transcript::Choice as Shown;
use wisp_render::transcript::copy;

use super::Presenter;
use super::subscription_login::Then;
use crate::app::retry::{Act, Ctx, Failed, Route, Row, Seen};
use crate::app::settings::Answer;
use crate::term::Key;

impl Presenter {
    fn retry_covered(&self) -> bool {
        self.card.is_some() || self.adding.open()
    }

    pub(super) fn retry_holds(&self) -> bool {
        self.retry.is_picking()
            || (self.retry.is_open()
                && self.composer.is_empty()
                && !self.retry_covered()
                && self.inputs.focused().is_none()
                && self.inputs.editing().is_none()
                && !self.status.is_running())
    }

    pub(super) fn retry_key(&mut self, key: Key, at: Duration) -> bool {
        if !self.retry_holds() {
            return false;
        }
        let act = {
            let cx = Ctx {
                facts: &self.facts,
                tracker: &self.tracker,
                at,
                wall: (self.now)().as_second(),
                set: self.glyphs,
            };
            self.retry.key(key, &cx)
        };
        match act {
            None => return false,
            Some(Act::Stay) => {}
            Some(Act::Dismissed) => {
                self.transcript.set_choice(self.retry.cell(), None);
            }
            Some(Act::Pick) if !self.dock_fits() => self.hint(copy::PICKER_NO_ROOM, at),
            Some(Act::Pick) => {
                let checking = self.ask_described(at);
                self.retry.picking(checking);
            }
            Some(Act::Retry { endpoint }) => self.send_retry(endpoint, at),
            Some(Act::SignIn { account }) => self.sign_in_then(account, Then::Retry, at),
            Some(Act::NewKey { account }) => {
                self.then_after(Some(account.clone()), Then::Retry);
                self.open_edit_card(&account, false, at);
            }
        }
        self.sync_choice(at);
        self.frames.mark();
        true
    }

    pub(super) fn retry_on(&mut self, account: &str, at: Duration) {
        let Some(failed) = self.retry.failed().filter(|_| self.retry.is_open()) else {
            return;
        };
        let from = self.facts.billing_on(&failed.endpoint);
        let wall = (self.now)().as_second();
        if account == failed.endpoint {
            if failed.rows(account, wall).contains(&Row::Retry) {
                self.send_retry(None, at);
            }
            return;
        }
        if !starts_charging(Some(from), self.facts.billing_on(account)) {
            return self.send_retry(Some(account.to_owned()), at);
        }
        let cx = Ctx {
            facts: &self.facts,
            tracker: &self.tracker,
            at,
            wall,
            set: self.glyphs,
        };
        self.retry.pick_on(account, &cx);
        self.sync_choice(at);
    }

    fn send_retry(&mut self, endpoint: Option<String>, at: Duration) {
        let request = self.next_request();
        let Some(command) = self.retry.command(endpoint.clone(), request) else {
            return;
        };
        if !self.command(command, at) {
            return;
        }
        let cx = Ctx {
            facts: &self.facts,
            tracker: &self.tracker,
            at,
            wall: (self.now)().as_second(),
            set: self.glyphs,
        };
        self.retry.sent(endpoint.as_deref(), request, &cx);
        self.view.to_bottom();
        self.submitted(at);
    }

    pub(super) fn key_fix(&self, name: &str) -> Option<String> {
        let file = self.keys_file();
        self.facts
            .endpoint_facts(name)?
            .key_var
            .as_ref()
            .filter(|var| !var.in_keys_file || file.is_some())
            .map(|var| {
                let file = file.filter(|_| var.in_keys_file);
                account::variable_line(name, &var.name, file.as_deref(), Ending::Change)
            })
    }

    fn auth_title(&mut self, failed: &Failed) -> Option<(String, String)> {
        let name = failed.endpoint.as_str();
        let facts = self.facts.endpoint_facts(name)?;
        let kind = facts
            .account_kind()
            .filter(|_| failed.kind == ErrorKind::Auth)?;
        if failed.route(&self.facts) == Route::SignIn {
            return Some((account::sign_in_expired(name), String::new()));
        }
        self.refused_keys.insert(name.to_owned());
        let state = AccountState::of(self.facts.billing_on(name));
        let chip = account::chip(name, state, &Glyphs::of(self.glyphs));
        Some((
            account::key_refused(kind.provider_word(), &chip),
            self.key_fix(name).unwrap_or_default(),
        ))
    }

    pub(super) fn retry_answer(&mut self, event: &AgentEvent, answer: Answer) -> Answer {
        match event {
            AgentEvent::SettingsChanged {
                answers: None,
                withdrawn: None,
                endpoint,
                account_bound,
                ..
            } if self.retry.expects_switch(endpoint) => {
                self.retry.switched(account_bound);
                Answer::Other
            }
            _ => answer,
        }
    }

    pub(super) fn retry_event(&mut self, event: &AgentEvent, at: Duration) {
        if let AgentEvent::TurnEnded { outcome, .. } = event
            && let Some(failed) = Failed::of(self.retry.turn(), outcome, self.tracker.session())
        {
            if let Some((title, lines)) = self.auth_title(&failed) {
                self.transcript
                    .retitle(self.transcript.turn(), title, lines);
            }
            let checking = self.ask_described(at);
            self.retry.offered(failed, self.transcript.turn(), checking);
            self.set_aside_settings();
        }
        let seen = {
            let cx = Ctx {
                facts: &self.facts,
                tracker: &self.tracker,
                at,
                wall: (self.now)().as_second(),
                set: self.glyphs,
            };
            self.retry.event(event, &cx)
        };
        let cell = self.retry.cell();
        if seen != Seen::Nothing {
            self.transcript.set_retries(cell, false);
        }
        match seen {
            Seen::Nothing => {}
            Seen::Lapsed => {
                self.transcript.set_choice(cell, None);
            }
            Seen::Retried { used, note } => {
                self.transcript.set_choice(cell, Some(Shown::Used(used)));
                if let Some(note) = note {
                    self.transcript.retrying(&note);
                }
            }
            Seen::Refused(text) => {
                self.transcript.set_choice(cell, None);
                self.transcript.note(&text);
            }
        }
        self.sync_choice(at);
    }

    pub(super) fn sync_choice(&mut self, at: Duration) {
        let Some(failed) = self.retry.failed() else {
            return;
        };
        let armed =
            failed.offer.retries_at.is_some() && failed.endpoint == self.tracker.next().endpoint;
        let retries = self.transcript.set_retries(self.retry.cell(), armed);
        let shown = {
            let cx = Ctx {
                facts: &self.facts,
                tracker: &self.tracker,
                at,
                wall: (self.now)().as_second(),
                set: self.glyphs,
            };
            self.retry
                .shown(&cx, !self.composer.is_empty() || self.retry_covered())
        };
        if self.transcript.set_choice(self.retry.cell(), shown) || retries {
            self.view.sync(self.transcript.cells().len());
            self.frames.mark();
        }
    }

    pub(super) fn dock_open(&self, dock: Docked) -> bool {
        match dock {
            Docked::Rewind => self.rewind.is_some(),
            Docked::Chats => self.chats.is_some(),
            Docked::Picker => self.picker.is_some(),
            Docked::Opener => self.opener_open(),
            Docked::Retry => self.retry.is_picking(),
        }
    }

    pub(super) fn close_dock(&mut self, dock: Docked) {
        match dock {
            Docked::Rewind => self.rewind = None,
            Docked::Chats => self.chats = None,
            Docked::Picker => self.picker = None,
            Docked::Opener => self.close_opener(),
            Docked::Retry => self.retry.unpick(),
        }
    }

    pub(super) fn dock_lines(
        &self,
        view_rows: u16,
        chrome: Layout,
        at: Duration,
    ) -> Vec<StyledLine> {
        let room = dock_room(usize::from(view_rows));
        match Docked::ALL.into_iter().find(|&dock| self.dock_open(dock)) {
            Some(Docked::Rewind) => self.rewind_view(room, chrome).lines,
            Some(Docked::Chats) => self.chats_view(room, chrome),
            Some(Docked::Picker) => self.picker.as_ref().map_or_else(Vec::new, |picker| {
                let (facts, tracker) = self.chat_pair();
                let menu = picker.docked(&facts, tracker, at, self.glyphs);
                docked_rows(&menu, &picker.dock(&menu, chrome.glyphs), room, chrome)
            }),
            Some(Docked::Opener) => self.opener_view(room, chrome).lines,
            Some(Docked::Retry) | None => {
                let cx = Ctx {
                    facts: &self.facts,
                    tracker: &self.tracker,
                    at,
                    wall: (self.now)().as_second(),
                    set: self.glyphs,
                };
                self.retry
                    .menu(&cx)
                    .map(|menu| docked_rows(&menu, &self.retry.dock(&cx), room, chrome))
                    .unwrap_or_default()
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Docked {
    Rewind,
    Chats,
    Picker,
    Opener,
    Retry,
}

impl Docked {
    pub(super) const ALL: [Docked; 5] = [
        Docked::Rewind,
        Docked::Chats,
        Docked::Picker,
        Docked::Opener,
        Docked::Retry,
    ];
}
