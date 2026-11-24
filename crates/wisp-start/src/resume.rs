use std::collections::BTreeMap;
use std::path::PathBuf;

use wisp_core::agent::{Agent, Connection, account_listing};
use wisp_core::endpoints::goes::{
    Account, Goes, Leaving, ModelGoes, billing_provider, chat_goes, model_goes, model_provider,
    shown as shown_name, start_asks, start_goes, start_order, starts_charging,
};
use wisp_core::endpoints::{
    AuthKind, Availability, Cause, EndpointError, EndpointName, ProviderKind, StartReason,
};
use wisp_core::instructions::Extra;
use wisp_core::kit::JsonlLogs;
use wisp_core::resume::{LoggedSession, Providers};
pub use wisp_core::resume::{
    Request, Resumed, authentication_required, billing_mode, check_billing_mode,
};

use wisp_core::workspace::Workspace;
use wisp_protocol::log::LogRecord;
use wisp_protocol::settings::{offered, reachable, runs_on};
use wisp_protocol::{ACCOUNT_FLAG, keys_file_name, shown_model};
use wisp_render::chrome::account::{self, Said, SaidWords};
use wisp_render::chrome::{ModelFacts, StartPick, chat_of, named_model, offers, short_model};
use wisp_render::transcript::copy::listed_or;

pub mod ask;
pub use ask::State;
use ask::Why;
pub use ask::{Left, Marks, Question, who};

pub fn here() -> Option<String> {
    Workspace::detect().ok().map(|w| w.shorten(&w.root))
}

pub fn open(
    dir: Option<PathBuf>,
    request: &Request<'_>,
    here: Option<&str>,
    providers: Providers<'_>,
) -> Result<Resumed, String> {
    let logs = JsonlLogs {
        on: true,
        dir,
        root_id: None,
    };
    let mut resumed = wisp_core::resume::open(&logs, request, here, providers)?;
    for warning in &mut resumed.warnings {
        *warning = format!("wisp: {warning}");
    }
    Ok(resumed)
}

pub const NO_DIRECTORY_NOTE: &str =
    "instructions: this session logged no directory; loaded from the current one";
pub const GONE_NOTE: &str =
    "instructions: the session's directory is gone; loaded from the current one";

pub enum Selected {
    Continue { name: String, left: Option<Left> },
    Ask(Question),
    Refuse(String),
}

impl Selected {
    pub fn first(&self) -> Option<&str> {
        match self {
            Self::Continue { name, .. } => Some(name),
            Self::Ask(question) => question.choices.first().map(String::as_str),
            Self::Refuse(_) => None,
        }
    }

    pub fn settle(self, at: Option<&str>, asks: bool) -> Result<Settled, String> {
        match (self, at) {
            (Self::Refuse(line), _) => Err(line),
            (Self::Continue { name, left }, at) => Ok(Settled::Go {
                name: at.map_or(name, str::to_owned),
                left,
            }),
            (Self::Ask(question), Some(at)) => Ok(Settled::Go {
                name: at.to_owned(),
                left: Some(question.left),
            }),
            (Self::Ask(question), None) if asks => Ok(Settled::Ask(question)),
            (Self::Ask(question), None) => Err(question.script()),
        }
    }
}

pub enum Settled {
    Go { name: String, left: Option<Left> },
    Ask(Question),
}

fn accounts(entries: &BTreeMap<EndpointName, Availability>) -> Vec<Account<'_>> {
    entries
        .iter()
        .filter_map(|(name, entry)| match entry {
            Availability::Ready(endpoint) => Some(Account {
                name: name.as_str(),
                provider: endpoint.provider,
                billing: endpoint.credential.billing(),
            }),
            Availability::Unavailable(_) | Availability::SameAs(_) => None,
        })
        .collect()
}

fn goes_to(entries: &BTreeMap<EndpointName, Availability>, goes: Goes<'_>, left: Left) -> Selected {
    match goes {
        Goes::Continue(to) => Selected::Continue {
            name: to.to_owned(),
            left: Some(left),
        },
        Goes::Ask(names) if names.is_empty() => {
            Selected::Refuse(format!("wisp: {}", authentication_required(&left.name)))
        }
        Goes::Ask(names) => Selected::Ask(Question::new(left, &names, entries)),
    }
}

fn unusable<'a>(
    entries: &'a BTreeMap<EndpointName, Availability>,
    name: &str,
) -> (Why, Option<&'a wisp_core::endpoints::Unready>) {
    match EndpointName::parse(name).and_then(|n| entries.get(&n)) {
        Some(Availability::Unavailable(unready)) => {
            (Why::Unusable(unready.reason.clone()), Some(unready))
        }
        _ => (Why::Gone, None),
    }
}

fn implicit(name: &str) -> Option<ProviderKind> {
    EndpointName::parse(name)
        .is_some_and(|name| name.names_keys_file())
        .then_some(ProviderKind::IMPLICIT)
}

pub fn select_endpoint(
    records: &[LogRecord],
    logged: &LoggedSession,
    name: &str,
    entries: &BTreeMap<EndpointName, Availability>,
    start: Option<&str>,
    asks: bool,
) -> Selected {
    let mode = billing_mode(records, name).unwrap_or_default();
    if let Some(endpoint) = EndpointName::parse(name).and_then(|n| ask::ready(entries, &n)) {
        let now = endpoint.credential.billing();
        if asks && crate::endpoints::expired(endpoint) {
            let leaving = Leaving {
                name,
                provider: Some(endpoint.provider),
                billing: Some(mode),
            };
            let left = Left {
                session: Some(logged.short_id().to_owned()),
                name: name.to_owned(),
                why: Why::Unusable(Health::Expired.words(None).unwrap_or_default()),
                billing: Some(mode),
                state: Some(State::Expired),
                keys_file: None,
            };
            let names = start_asks(&accounts(entries), &leaving);
            return placing(
                Selected::Ask(Question::new(left, &names, entries)),
                records,
                entries,
            );
        }
        return match check_billing_mode(Some(mode), now, name) {
            Err(refusal) if starts_charging(Some(mode), now) => {
                Selected::Refuse(format!("wisp: {refusal}"))
            }
            _ => Selected::Continue {
                name: name.to_owned(),
                left: None,
            },
        };
    }
    let (why, unready) = unusable(entries, name);
    let leaving = Leaving {
        name,
        provider: unready
            .map(|u| u.provider)
            .or_else(|| implicit(name))
            .or_else(|| billing_provider(mode))
            .or_else(|| model_provider(&logged.current.model)),
        billing: Some(mode),
    };
    let left = Left {
        session: Some(logged.short_id().to_owned()),
        name: name.to_owned(),
        why,
        billing: Some(mode),
        state: None,
        keys_file: None,
    };
    let goes = goes_to(
        entries,
        chat_goes(&accounts(entries), &leaving, start),
        left,
    );
    placing(goes, records, entries)
}

fn placing(
    selected: Selected,
    records: &[LogRecord],
    entries: &BTreeMap<EndpointName, Availability>,
) -> Selected {
    let Selected::Ask(mut question) = selected else {
        return selected;
    };
    let links = wisp_core::agents::fold::links(records);
    let mut agents: Vec<(String, Option<String>, String)> = links
        .spawned
        .into_iter()
        .filter(|s| !links.ended.contains_key(&s.path))
        .filter(|s| {
            let logged = s.endpoint.as_deref().and_then(EndpointName::parse);
            logged.is_none_or(|name| !entries.contains_key(&name))
        })
        .map(|s| {
            (
                s.path.as_str().trim_start_matches('/').to_owned(),
                s.provider,
                s.model,
            )
        })
        .collect();
    agents.sort();
    question.agents = agents;
    Selected::Ask(question)
}

pub fn start_gone(sources: &crate::endpoints::Sources, asks: bool) -> Option<Selected> {
    let (left, provider, entries) = start_left(sources, asks)?;
    let accounts = accounts(&entries);
    if accounts.iter().all(|a| a.name == left.name) {
        return None;
    }
    let leaving = Leaving {
        name: &left.name,
        provider,
        billing: left.billing,
    };
    if asks && matches!(left.state, Some(State::Expired | State::SignedOut)) {
        let names = start_asks(&accounts, &leaving);
        return Some(Selected::Ask(Question::new(left, &names, &entries)));
    }
    Some(goes_to(&entries, start_goes(&accounts, &leaving), left))
}

pub enum StartModel {
    Continue {
        name: String,
        left: Left,
        note: Said,
    },
    Pick {
        line: String,
        pick: StartPick,
    },
}

pub fn model_on(
    sources: &crate::endpoints::Sources,
    name: &str,
    model: &str,
    models: &[ModelFacts],
    ready: &[Connection],
) -> Option<String> {
    let entries = sources.entries().ok()?;
    let account = accounts(&entries).into_iter().find(|a| a.name == name)?;
    let chat = chat_of(models, model);
    Some(runs_on(
        &offers(models),
        account_listing(ready, name).landing(account.provider.as_str()),
        chat,
    ))
}

pub(crate) fn model_offered<'a>(
    typed: &str,
    models: &'a [ModelFacts],
    ready: &'a [Connection],
) -> impl Fn(&str, ProviderKind) -> bool + use<'a> {
    let offers = offers(models);
    let no_chat = chat_of(models, "");
    let named = named_model(models, typed).map(|m| m.id.as_str());
    move |name, provider| {
        named.is_some_and(|id| {
            reachable(
                &offers,
                account_listing(ready, name).landing(provider.as_str()),
                no_chat,
            )
            .iter()
            .any(|m| m == id)
        })
    }
}

pub fn start_model(
    sources: &crate::endpoints::Sources,
    typed: &str,
    models: &[ModelFacts],
    ready: &[Connection],
) -> Option<StartModel> {
    let entries = sources.entries().ok()?;
    let start = sources.book().ok()?.start.to_string();
    let accounts = accounts(&entries);
    let offered_on = model_offered(typed, models, ready);
    let typed = shown_model(typed);
    let marks = sources.marks(&entries);
    let marks = |name: &str| crate::run::marked_in(&marks, name);
    match model_goes(&accounts, &start, |a| offered_on(a.name, a.provider)) {
        ModelGoes::Here => None,
        ModelGoes::Continue(to) => {
            let left = Left {
                session: None,
                billing: accounts.iter().find(|a| a.name == start).map(|a| a.billing),
                name: start,
                why: Why::Lacks(typed.to_owned()),
                state: None,
                keys_file: None,
            };
            Some(StartModel::Continue {
                name: to.to_owned(),
                note: left.said(&marks),
                left,
            })
        }
        ModelGoes::Ask(names) => {
            let lacks = Said::Lacks(ask::who(&start, &marks), typed.to_owned());
            let names: Vec<&str> = names.into_iter().map(shown_name).collect();
            let add = match names.as_slice() {
                [one] => format!("add {ACCOUNT_FLAG} {one} to use it"),
                many => format!("add {ACCOUNT_FLAG} {} to choose one", listed_or(many)),
            };
            Some(StartModel::Pick {
                line: format!("wisp: {}; {add}.", lacks.plain()),
                pick: StartPick {
                    query: typed.to_owned(),
                    head: vec![lacks],
                },
            })
        }
        ModelGoes::NoneOffers => {
            let offers = offers(models);
            let no_chat = chat_of(models, "");
            let mut ordered: Vec<&Account<'_>> = accounts.iter().collect();
            start_order(&mut ordered, Some(&start));
            let mut available: Vec<String> = Vec::new();
            for id in ordered.iter().flat_map(|a| {
                offered(
                    &offers,
                    account_listing(ready, a.name).landing(a.provider.as_str()),
                    no_chat,
                )
            }) {
                let id = short_model(&id);
                if !available.contains(&id) {
                    available.push(id);
                }
            }
            Some(StartModel::Pick {
                line: format!(
                    "wisp: no account offers {typed}. Available models: {}.",
                    available.join(", ")
                ),
                pick: StartPick {
                    query: String::new(),
                    head: vec![
                        Said::Text(account::none_offers(typed)),
                        Said::Text(account::CHOOSE_A_MODEL.to_owned()),
                    ],
                },
            })
        }
    }
}

pub fn nothing_left(sources: &crate::endpoints::Sources) -> Option<String> {
    let (left, _, entries) = start_left(sources, false)?;
    if left.state == Some(State::Off) {
        return None;
    }
    accounts(&entries)
        .is_empty()
        .then(|| format!("{}.", left.lead(&|_| None).plain()))
}

pub fn start_left(
    sources: &crate::endpoints::Sources,
    asks: bool,
) -> Option<(
    Left,
    Option<ProviderKind>,
    BTreeMap<EndpointName, Availability>,
)> {
    if sources.choice.is_some() {
        return None;
    }
    let (name, reason, expired) = match sources.resolve() {
        Err(crate::endpoints::Failure::Resolve(EndpointError::DefaultUnavailable {
            name,
            reason,
        })) => (name, reason, false),
        Ok(book)
            if asks
                && book.start_reason == StartReason::Default
                && crate::endpoints::expired(&book.active) =>
        {
            (book.start, Health::Expired.words(None)?, true)
        }
        _ => return None,
    };
    let entries = sources.entries().ok()?;
    let name = wisp_core::endpoints::account(&entries, &name).clone();
    let unready = match entries.get(&name) {
        Some(Availability::Unavailable(unready)) => Some(unready),
        _ => None,
    };
    let state = match unready {
        _ if expired => Some(State::Expired),
        Some(unready) => Some(unusable_state(sources, &name, unready)),
        None if name.is_env() && sources.inputs.available() => Some(State::Unread(reason.clone())),
        None => Some(State::Gone),
    };
    let provider = unready
        .map(|u| u.provider)
        .or_else(|| implicit(name.as_str()));
    let left = Left {
        session: None,
        name: name.to_string(),
        why: Why::Unusable(reason),
        billing: unready
            .and_then(|u| u.auth)
            .map(AuthKind::billing)
            .or_else(|| implicit(name.as_str()).map(|_| wisp_protocol::Billing::Metered)),
        state,
        keys_file: name.is_env().then(|| {
            let file = sources.inputs.env_file().ok().flatten();
            let official = file.is_none_or(wisp_core::provider::anthropic::keys_file_official);
            (keys_file_name(official), sources.file_words())
        }),
    };
    Some((left, provider, entries))
}

fn unusable_state(
    sources: &crate::endpoints::Sources,
    name: &EndpointName,
    unready: &wisp_core::endpoints::Unready,
) -> State {
    if unready.cause == Cause::ProviderOff {
        return State::Off;
    }
    if unready.cause == Cause::Other && unready.auth.is_some_and(|a| a.subscription().is_some()) {
        return State::SignedOut;
    }
    let health = Health::of(Some(unready.cause));
    let var = sources
        .config
        .as_ref()
        .ok()
        .and_then(Option::as_ref)
        .and_then(|config| config.endpoints.get(name))
        .and_then(|config| config.key_env())
        .map(ToString::to_string);
    let words = health.words(var.as_deref()).unwrap_or_default();
    State::Unusable(health, words)
}

pub fn move_to(sources: &mut crate::endpoints::Sources, name: &str) {
    sources.choice = Some(name.to_owned());
    sources.choice_reason = wisp_core::endpoints::StartReason::Flag;
}

pub fn extra_kept(logged: &LoggedSession, flagged: bool) -> Option<String> {
    logged
        .instructions_extra
        .as_deref()
        .filter(|name| !flagged && *name != "none" && Extra::named(name).is_none())
        .map(|name| {
            format!("wisp: this session logged extra instructions \"{name}\", which this Wisp doesn't know; the logged instructions are kept, and would read as none only if the log were damaged")
        })
}

#[cfg(any(test, feature = "test-support"))]
pub fn instructions_settings(
    logged: &LoggedSession,
    current: crate::instructions::Settings,
) -> crate::instructions::Settings {
    match &logged.started.instructions_source {
        Some(source) => crate::instructions::Settings {
            source: source.clone(),
            ..current
        },
        None => current,
    }
}

pub fn refresh_priced<'a>(
    endpoints: impl IntoIterator<Item = (&'a str, &'a mut bool)>,
    agent: &Agent,
) {
    for (name, value) in endpoints {
        if let Some(priced) = agent.priced_on(name) {
            *value = priced;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    SignedIn,
    Expired,
    KeyMissing,
    Unset,
    Unreadable,
}

impl Health {
    pub fn of(cause: Option<Cause>) -> Self {
        match cause {
            None => Self::SignedIn,
            Some(Cause::NoKey) => Self::KeyMissing,
            Some(Cause::VariableUnset) => Self::Unset,
            Some(Cause::Other | Cause::Shadowed | Cause::ProviderOff) => Self::Unreadable,
        }
    }

    pub fn words(self, var: Option<&str>) -> Option<String> {
        match (self, var) {
            (Self::SignedIn, _) => None,
            (Self::Expired, _) => Some("sign-in expired".to_owned()),
            (Self::Unset, Some(var)) => Some(format!("{var} not set")),
            (Self::KeyMissing | Self::Unset, _) => Some("key missing".to_owned()),
            (Self::Unreadable, _) => Some(account::CANT_BE_READ.to_owned()),
        }
    }
}
