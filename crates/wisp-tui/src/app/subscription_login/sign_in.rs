use std::time::Duration;

use tokio::sync::mpsc;
use wisp_protocol::subscription_auth::{AuthCommand, AuthFailureKind, AuthOutcome};
use wisp_protocol::{ClientCommand, RequestId};
use wisp_render::chrome::{CardHit, CardRows};

use super::{Intent, Wizard};
use crate::app::presenter::AGENT_STOPPED;
use crate::term::screen::Link;

const LINK_CLICK_GAP: Duration = Duration::from_secs(1);

pub struct Wire<'a> {
    pub requests: &'a mut u64,
    pub commands: Option<&'a mpsc::Sender<ClientCommand>>,
    pub unsent: bool,
}

impl Wire<'_> {
    pub fn request(&mut self) -> RequestId {
        *self.requests += 1;
        RequestId(*self.requests)
    }

    fn send(&mut self, command: AuthCommand) -> bool {
        let command = ClientCommand::SubscriptionAuth { command };
        let sent = self.commands.is_some_and(|c| c.try_send(command).is_ok());
        self.unsent |= !sent;
        sent
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Reaction {
    Close,
    AddKey,
    Use(String),
    Ready { alias: String, reauthorizing: bool },
    Saved(String),
}

#[derive(Debug)]
pub struct SignIn {
    pub wizard: Wizard,
    opens: Vec<(RequestId, RequestId)>,
    link_click: Option<Duration>,
}

impl SignIn {
    pub fn new(wizard: Wizard) -> Self {
        Self {
            wizard,
            opens: Vec::new(),
            link_click: None,
        }
    }

    pub fn act(&mut self, intent: Intent, wire: &mut Wire<'_>) -> Option<Reaction> {
        let intent = match intent {
            Intent::Close => return Some(Reaction::Close),
            Intent::AddKey => return Some(Reaction::AddKey),
            Intent::Use { alias } => return Some(Reaction::Use(alias)),
            other => other,
        };
        let request = wire.request();
        let command = match intent {
            Intent::Close | Intent::AddKey | Intent::Use { .. } => return None,
            Intent::Open { login } => {
                if wire.send(AuthCommand::Open { request, login }) {
                    self.opens.push((request, login));
                } else {
                    self.wizard.open_failed(login);
                }
                return None;
            }
            Intent::Start { selection, .. } => {
                self.wizard.start_sent(request);
                AuthCommand::Start {
                    request,
                    provider: selection.provider,
                    alias: Some(selection.alias),
                    reauthorize: selection.reauthorize,
                }
            }
            Intent::Finish { login, response } => AuthCommand::Finish {
                request,
                login,
                response,
                alias: None,
            },
            Intent::Cancel { login } => AuthCommand::Cancel { request, login },
            Intent::Reload { alias } => AuthCommand::Reload { request, alias },
        };
        self.wizard.waits_on(request);
        if !wire.send(command) {
            self.wizard.failure(
                request,
                AuthFailureKind::Unavailable,
                None,
                AGENT_STOPPED,
                None,
            );
        }
        None
    }

    pub fn key(
        &mut self,
        key: crate::term::Key,
        at: Duration,
        wire: &mut Wire<'_>,
    ) -> Option<Reaction> {
        let intent = self.wizard.key(key, at)?;
        self.act(intent, wire)
    }

    pub fn click(
        &mut self,
        hit: CardHit,
        modified: bool,
        at: Duration,
        wire: &mut Wire<'_>,
    ) -> Option<Reaction> {
        let intent = match hit {
            CardHit::Link if modified => None,
            CardHit::Link => {
                if self
                    .link_click
                    .is_some_and(|last| at < last.saturating_add(LINK_CLICK_GAP))
                {
                    return None;
                }
                self.link_click = Some(at);
                self.wizard.link()
            }
            hit => self.wizard.click(hit, at),
        };
        self.act(intent?, wire)
    }

    pub fn tick(&mut self, at: Duration, wire: &mut Wire<'_>) -> Option<Reaction> {
        let intent = self.wizard.tick(at)?;
        self.act(intent, wire)
    }

    pub fn cancel(&mut self, wire: &mut Wire<'_>) -> Option<Reaction> {
        let intent = self.wizard.cancel();
        self.act(intent, wire)
    }

    pub fn event(
        &mut self,
        request: RequestId,
        outcome: &AuthOutcome,
        at: Duration,
        wire: &mut Wire<'_>,
    ) -> Option<Reaction> {
        if let Some(index) = self.opens.iter().position(|(sent, _)| *sent == request) {
            let (_, login) = self.opens.remove(index);
            match outcome {
                AuthOutcome::Challenge { challenge } => {
                    self.wizard.reopened(challenge.login, challenge.opened);
                }
                AuthOutcome::Failed { .. } => self.wizard.open_failed(login),
                _ => {}
            }
            return None;
        }
        let wizard = &mut self.wizard;
        let intent = match outcome {
            AuthOutcome::Challenge { challenge }
                if wizard.selection_matches(&challenge.alias, challenge.provider) =>
            {
                wizard.challenged(
                    request,
                    challenge.login,
                    challenge.kind,
                    challenge.authorization_url.clone(),
                    challenge.opened,
                    Duration::from_secs(challenge.expires_in_secs),
                    at,
                )
            }
            AuthOutcome::Ready {
                endpoint,
                previous_cleanup_pending,
            } if wizard.selection_matches(&endpoint.alias, endpoint.provider) => {
                let reauthorizing = wizard.reauthorizing();
                return wizard
                    .ready(request, *previous_cleanup_pending)
                    .then(|| Reaction::Ready {
                        alias: endpoint.alias.clone(),
                        reauthorizing,
                    });
            }
            AuthOutcome::SavedNotReady {
                endpoint,
                previous_cleanup_pending,
            } if wizard.selection_matches(&endpoint.alias, endpoint.provider) => {
                wizard.saved_not_ready(
                    request,
                    endpoint.storage.clone(),
                    endpoint.is_default,
                    *previous_cleanup_pending,
                );
                return Some(Reaction::Saved(endpoint.alias.clone()));
            }
            AuthOutcome::Failed {
                kind,
                message,
                problem,
                same_account,
            } => wizard
                .failure(request, *kind, *problem, message, same_account.as_deref())
                .then(|| wizard.release())
                .flatten(),
            AuthOutcome::Cancelled { login } => {
                wizard.cancelled(request, *login);
                None
            }
            _ => None,
        };
        self.act(intent?, wire)
    }

    pub fn link(&self, rows: &CardRows, top: u16, shown: std::ops::Range<usize>) -> Option<Link> {
        let cells = rows.link.filter(|cells| shown.contains(&cells.line))?;
        let url = self.wizard.live_link()?;
        let row = top.saturating_add(u16::try_from(cells.line - shown.start).unwrap_or(u16::MAX));
        let first = u16::try_from(cells.start).unwrap_or(u16::MAX);
        let last = u16::try_from(cells.start + cells.width).unwrap_or(u16::MAX);
        Link::new(row, first..last, url.expose())
    }
}
