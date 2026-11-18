use std::time::Duration;

use wisp_protocol::settings::{INSTALLED_KEY, SettingChange, SettingTarget, SettingValue};
use wisp_protocol::subscription_auth::{Followed, Stayed, Way};
use wisp_protocol::{AgentEvent, ClientCommand, RejectKind, RequestId};
use wisp_render::chrome::account::{AccountKind, SignedOut, gone_title, message_waits, off_title};
use wisp_render::theme::glyphs::Glyphs;

use super::Presenter;
use super::subscription_login::Then;
use crate::app::logout::{Intent, SignOut};
use crate::app::subscription_login::cancel;

#[derive(Debug)]
pub(super) struct GoingOff {
    request: RequestId,
    changes: Vec<SettingChange>,
    plugin: String,
    removed: bool,
    step: Step,
}

#[derive(Debug, PartialEq, Eq)]
enum Step {
    Switch,
    Card,
    Move,
    Save,
}

impl GoingOff {
    pub(super) fn of(request: RequestId, changes: &[SettingChange]) -> Option<Self> {
        let (plugin, key) = changes.iter().find_map(|change| {
            let SettingTarget::Path(path) = &change.target else {
                return None;
            };
            (change.value == Some(SettingValue::Bool(false))).then(|| path.provider_switch())?
        })?;
        Some(Self {
            request,
            changes: changes.to_vec(),
            plugin: plugin.to_owned(),
            removed: key == INSTALLED_KEY,
            step: Step::Switch,
        })
    }
}

impl Presenter {
    pub(super) fn going_off_event(&mut self, event: &AgentEvent, at: Duration) -> bool {
        if let AgentEvent::Rejected {
            request: Some(request),
            ..
        } = event
            && self.authentication.off_refusal == Some(*request)
        {
            self.authentication.off_refusal = None;
            self.tracker.forget(*request);
            return true;
        }
        let request = match event {
            AgentEvent::LeaveNeeds { request, .. }
            | AgentEvent::SettingsChanged {
                answers: Some(request),
                ..
            }
            | AgentEvent::Rejected {
                request: Some(request),
                ..
            } => *request,
            _ => return false,
        };
        let Some(mut going) = self.authentication.going_off.take() else {
            return false;
        };
        if going.request != request {
            self.authentication.going_off = Some(going);
            return false;
        }
        match (&going.step, event) {
            (Step::Switch, AgentEvent::LeaveNeeds { ways, .. }) => {
                self.authentication.off_refusal = Some(request);
                self.set_aside_settings();
                let (title, prompt) = off_title(&going.plugin, going.removed);
                self.way_card(Vec::new(), (title, prompt, None), ways, at);
                going.step = Step::Card;
                self.authentication.going_off = Some(going);
                return true;
            }
            (Step::Move, AgentEvent::SettingsChanged { .. }) => {
                let next = self.next_request();
                if self.command(
                    ClientCommand::ChangeSettings {
                        request: next,
                        changes: going.changes.clone(),
                        save: true,
                    },
                    at,
                ) {
                    self.tracker.asked_saved(next, None);
                    going.request = next;
                    going.step = Step::Save;
                    self.authentication.going_off = Some(going);
                } else {
                    self.tracker.forget(request);
                }
            }
            (Step::Save, AgentEvent::SettingsChanged { .. }) => {
                self.tracker.take_held();
            }
            (Step::Save, AgentEvent::Rejected { .. }) => {
                for line in self.tracker.take_held() {
                    self.transcript.note(&line);
                }
            }
            (Step::Switch | Step::Move, AgentEvent::Rejected { .. })
            | (Step::Switch, AgentEvent::SettingsChanged { .. }) => {}
            _ => self.authentication.going_off = Some(going),
        }
        false
    }

    pub(super) fn going_off_intent(&mut self, intent: &Intent, at: Duration) -> bool {
        if !self
            .authentication
            .going_off
            .as_ref()
            .is_some_and(|going| going.step == Step::Card)
        {
            return false;
        }
        let Some(mut going) = self.authentication.going_off.take() else {
            return false;
        };
        if let Intent::Send {
            chat: Some(chat), ..
        } = intent
        {
            let next = self.next_request();
            let mut changes = ClientCommand::shortcut_changes(Some(chat.clone()), None, None);
            changes.extend(going.changes.iter().cloned());
            if self.command(
                ClientCommand::ChangeSettings {
                    request: next,
                    changes,
                    save: false,
                },
                at,
            ) {
                self.tracker.asked_holding(next, chat);
                going.request = next;
                going.step = Step::Move;
                self.authentication.going_off = Some(going);
            }
        }
        true
    }

    pub(super) fn gone_event(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::EndpointsChanged { endpoints } => {
                let listed = |name: &String| endpoints.iter().any(|e| e.name == *name);
                self.authentication
                    .drawn_gone
                    .retain(|names| !names.iter().any(listed));
                let dropped = self.facts.endpoints.iter().map(|e| e.name.clone());
                let dropped: Vec<String> = dropped.filter(|name| !listed(name)).collect();
                let auth = &mut self.authentication;
                auth.unlisted.retain(|name| !listed(name));
                auth.unlisted.extend(dropped);
            }
            AgentEvent::AccountsGone {
                names,
                moved,
                stay,
                ways,
            } => self.accounts_gone(names, moved, stay, ways),
            _ => {}
        }
    }

    fn accounts_gone(
        &mut self,
        names: &[String],
        moved: &[Followed],
        stay: &[Stayed],
        ways: &[Way],
    ) {
        let chat = self.tracker.wanted().endpoint;
        let held = self.authentication.held_gone.take();
        if names.contains(&chat) {
            let mut first = vec![chat.clone()];
            first.extend(names.iter().filter(|n| **n != chat).cloned());
            self.authentication.gone = Some((first, ways.to_vec()));
            if held.is_none() && moved.is_empty() && stay.is_empty() {
                return;
            }
        }
        let fresh = self.draws_gone(names);
        let (asked, priced) = &self.authentication.gone_priced;
        let same = names.len() == asked.len() && names.iter().all(|n| asked.contains(n));
        let priced = if same { priced.clone() } else { Vec::new() };
        let dest = held.as_ref().map_or(chat, |m| m.name.clone());
        let kind = |name: &str| {
            self.kind_of(name)
                .unwrap_or(AccountKind::ClaudeSubscription)
        };
        let out = SignedOut {
            gone: names
                .iter()
                .filter(|_| fresh)
                .map(|name| (kind(name), name.clone()))
                .collect(),
            chat: held,
            agents: self.followed(moved, &dest, &priced),
            kept: self.kept(stay, &dest),
            elsewhere: true,
            ..SignedOut::default()
        };
        self.transcript.signed_out(out);
    }

    pub(super) fn draws_gone(&mut self, names: &[String]) -> bool {
        let names: std::collections::BTreeSet<String> = names.iter().cloned().collect();
        let drawn = &mut self.authentication.drawn_gone;
        let fresh = !drawn.contains(&names);
        if fresh {
            drawn.push(names);
        }
        fresh
    }

    pub(super) fn gone_refused(&mut self, event: &AgentEvent, at: Duration) {
        let AgentEvent::Rejected {
            id,
            kind: Some(RejectKind::AccountGone),
            ..
        } = event
        else {
            return;
        };
        let sent = id.is_some();
        let Some((names, ways)) = self.authentication.gone.clone() else {
            return;
        };
        let then = if sent { Then::Send } else { Then::Release };
        if ways.is_empty() {
            self.login_command("", at);
            if let Some(sign_in) = &mut self.authentication.sign_in {
                sign_in.wizard.note =
                    message_waits(&names, &wisp_render::theme::glyphs::Glyphs::of(self.glyphs));
                self.then_after(None, then);
            }
            return;
        }
        if self.authentication.sign_in.is_some() {
            self.authentication.behind = Some(then);
            return;
        }
        self.gone_card(then, at);
    }

    pub(super) fn gone_card(&mut self, then: Then, at: Duration) {
        let Some((names, ways)) = self.authentication.gone.clone() else {
            return;
        };
        let (title, prompt) = gone_title(&names, &Glyphs::of(self.glyphs));
        self.way_card(names, (title, prompt, None), &ways, at);
        if matches!(then, Then::Send) {
            self.then_after(None, then);
        }
    }
    fn way_card(
        &mut self,
        names: Vec<String>,
        heading: (String, String, Option<u8>),
        ways: &[Way],
        at: Duration,
    ) {
        let rows = self.way_rows(ways, at);
        self.inputs.unfocus();
        self.picker = None;
        self.authentication.sign_out = Some(SignOut::gone(names, heading, rows, cancel()));
    }
}
