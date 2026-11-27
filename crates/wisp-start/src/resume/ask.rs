use std::collections::BTreeMap;

use super::Health;
use wisp_core::agents::endpoints::{LoggedKind, Ready};
use wisp_core::endpoints::goes::shown;
use wisp_core::endpoints::{Availability, ENV, EndpointName, ResolvedEndpoint};
use wisp_protocol::subscription_auth::Way;
use wisp_protocol::{ACCOUNT_FLAG, Billing, Continued, starts_charging};
use wisp_render::chrome::account::{self, Said, Who};
use wisp_render::transcript::copy::listed_or;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Why {
    Gone,
    Unusable(String),
    Lacks(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Expired,
    Off,
    SignedOut,
    Unusable(Health, String),
    Unread(String),
    Gone,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Left {
    pub session: Option<String>,
    pub name: String,
    pub why: Why,
    pub billing: Option<Billing>,
    pub state: Option<State>,
    pub keys_file: Option<(&'static str, String)>,
}

pub type Marks<'a> = &'a dyn Fn(&str) -> Option<u8>;

pub fn who(name: &str, marks: Marks<'_>) -> Who {
    match shown(name) {
        words if words != name => Who::Words(words.to_owned()),
        _ => Who::Account(name.to_owned(), marks(name)),
    }
}

impl Left {
    pub fn why(&self) -> String {
        match &self.why {
            Why::Gone => "is gone".to_owned(),
            Why::Unusable(_) if self.name == ENV => "can't be used now".to_owned(),
            Why::Unusable(reason) => wisp_core::endpoints::cant_be_used(reason),
            Why::Lacks(model) => account::doesnt_offer(model),
        }
    }

    pub fn told(&self) -> bool {
        self.session.is_none() || self.why != Why::Gone
    }

    fn what(&self) -> String {
        let name = shown(&self.name);
        match &self.session {
            Some(id) if !self.told() => format!("session {id} ran on {name}"),
            Some(id) => format!("session {id} ran on {name}, which {}", self.why()),
            None => format!("{name}, where new chats start, {}", self.why()),
        }
    }

    pub fn said(&self, marks: Marks<'_>) -> Said {
        Said::Is(who(&self.name, marks), self.why())
    }

    pub fn continued(&self, marks: Marks<'_>) -> Continued {
        Continued {
            reason: self.told().then(|| self.said(marks)),
            from: self.billing,
            quote: None,
        }
    }

    pub fn missing(&self, marks: Marks<'_>) -> (Who, Option<String>) {
        match &self.keys_file {
            Some((name, file)) => (who(name, marks), Some(file.clone())),
            None => (who(&self.name, marks), None),
        }
    }

    pub fn lead(&self, marks: Marks<'_>) -> Said {
        let name = who(&self.name, marks);
        match (&self.session, &self.state) {
            (_, Some(State::Expired)) => Said::SignInExpired(name),
            (Some(_), _) | (None, None) => Said::RanOn(name, self.told().then(|| self.why())),
            (None, Some(State::Off)) => self.said(marks),
            (None, Some(State::SignedOut)) => Said::SignedOut(name),
            (None, Some(State::Unusable(Health::Unreadable, _))) => Said::CantBeRead(name),
            (None, Some(State::Unread(note))) => Said::Text(note.clone()),
            (None, Some(State::Unusable(..) | State::Gone)) => {
                let (name, from) = self.missing(marks);
                Said::KeyMissing(name, from)
            }
        }
    }

    pub fn continuing(&self, to: &str) -> String {
        let to = shown(to);
        match &self.session {
            Some(_) => format!("wisp: {}; continuing on {to}.", self.what()),
            None => format!("wisp: {}; this chat starts on {to}.", self.what()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub left: Left,
    pub choices: Vec<String>,
    pub agents: Vec<(String, Option<String>, String)>,
}

pub fn ready<'a>(
    entries: &'a BTreeMap<EndpointName, Availability>,
    name: &EndpointName,
) -> Option<&'a ResolvedEndpoint> {
    match entries.get(name)? {
        Availability::Ready(endpoint) => Some(endpoint),
        Availability::SameAs(twin) => match entries.get(twin)? {
            Availability::Ready(endpoint) => Some(endpoint),
            _ => None,
        },
        Availability::Unavailable(_) => None,
    }
}

impl Question {
    pub fn new(left: Left, names: &[&str], entries: &BTreeMap<EndpointName, Availability>) -> Self {
        let choices = names
            .iter()
            .filter(|name| EndpointName::parse(name).is_some_and(|n| ready(entries, &n).is_some()))
            .map(|name| (*name).to_owned())
            .collect();
        Self {
            left,
            choices,
            agents: Vec::new(),
        }
    }

    pub fn script(&self) -> String {
        let names: Vec<&str> = self.choices.iter().map(|c| shown(c)).collect();
        let rerun = match names.as_slice() {
            [one] => format!("rerun with {ACCOUNT_FLAG} {one}"),
            many => format!("rerun with {ACCOUNT_FLAG} and one of {}", listed_or(many)),
        };
        format!("wisp: {}; {rerun}", self.left.what())
    }

    pub fn ways<'a>(&self, account: impl Fn(&str) -> (Option<&'a Ready>, Billing)) -> Vec<Way> {
        let names = |agents: Vec<&(String, Option<String>, String)>| -> Vec<String> {
            agents
                .into_iter()
                .map(|(agent, ..)| agent.clone())
                .collect()
        };
        self.choices
            .iter()
            .map(|name| {
                let (ready, billing) = account(name);
                let (follow, stay) = self.agents.iter().partition(|(_, logged, model)| {
                    ready.is_some_and(|r| LoggedKind::read(logged.as_deref()).takes(r, model))
                });
                Way {
                    chat: Some(name.clone()),
                    follow: names(follow),
                    stay: names(stay),
                    charges: starts_charging(self.left.billing, billing),
                    ..Way::default()
                }
            })
            .collect()
    }
}
