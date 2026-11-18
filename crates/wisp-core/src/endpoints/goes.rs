pub use wisp_protocol::starts_charging;
use wisp_protocol::{Billing, KEYS_FILE_ACCOUNT};

use super::{ConfigFile, ENV, EndpointName, ProviderKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Account<'a> {
    pub name: &'a str,
    pub provider: ProviderKind,
    pub billing: Billing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Leaving<'a> {
    pub name: &'a str,
    pub provider: Option<ProviderKind>,
    pub billing: Option<Billing>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Goes<'a> {
    Continue(&'a str),
    Ask(Vec<&'a str>),
}

pub fn chat_goes<'a>(
    accounts: &[Account<'a>],
    from: &Leaving<'_>,
    start: Option<&str>,
) -> Goes<'a> {
    let start = start.filter(|name| *name != from.name);
    let mut others: Vec<&Account<'a>> = accounts.iter().filter(|a| a.name != from.name).collect();
    let fits: Vec<&str> = others
        .iter()
        .filter(|a| from.provider == Some(a.provider))
        .filter(|a| !starts_charging(from.billing, a.billing))
        .map(|a| a.name)
        .collect();
    if let Some(start) = fits.iter().find(|name| Some(**name) == start) {
        return Goes::Continue(start);
    }
    if let [only] = fits.as_slice() {
        return Goes::Continue(only);
    }
    start_order(&mut others, start);
    Goes::Ask(others.into_iter().map(|a| a.name).collect())
}

pub fn start_order(accounts: &mut [&Account<'_>], start: Option<&str>) {
    accounts.sort_by_key(|a| (Some(a.name) != start, a.billing.is_metered(), a.name));
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelGoes<'a> {
    Here,
    Continue(&'a str),
    Ask(Vec<&'a str>),
    NoneOffers,
}

pub fn model_goes<'a>(
    accounts: &[Account<'a>],
    start: &str,
    offers: impl Fn(&Account<'a>) -> bool,
) -> ModelGoes<'a> {
    let from = accounts.iter().find(|a| a.name == start);
    if from.is_some_and(&offers) {
        return ModelGoes::Here;
    }
    let mut others: Vec<&Account<'a>> = accounts.iter().filter(|a| offers(a)).collect();
    match others.as_slice() {
        [] => ModelGoes::NoneOffers,
        [only] if !starts_charging(from.map(|a| a.billing), only.billing) => {
            ModelGoes::Continue(only.name)
        }
        _ => {
            start_order(&mut others, Some(start));
            ModelGoes::Ask(others.into_iter().map(|a| a.name).collect())
        }
    }
}

fn start_after<'a>(
    accounts: &[Account<'a>],
    from: &Leaving<'_>,
    chat: Option<&'a str>,
) -> Option<&'a str> {
    let mut left: Vec<&Account<'a>> = accounts.iter().filter(|a| a.name != from.name).collect();
    left.sort_by_key(|a| a.name);
    let subscription = |a: &&&Account<'a>| !a.billing.is_metered();
    chat.filter(|name| *name != from.name)
        .or_else(|| {
            left.iter()
                .filter(subscription)
                .find(|a| from.provider == Some(a.provider))
                .map(|a| a.name)
        })
        .or_else(|| left.iter().find(subscription).map(|a| a.name))
        .or_else(|| left.first().map(|a| a.name))
}

pub fn start_goes<'a>(accounts: &[Account<'a>], from: &Leaving<'_>) -> Goes<'a> {
    let charges = |to: &str| {
        accounts
            .iter()
            .any(|a| a.name == to && starts_charging(from.billing, a.billing))
    };
    match start_after(accounts, from, None) {
        Some(to) if !charges(to) => Goes::Continue(to),
        _ => Goes::Ask(start_asks(accounts, from)),
    }
}

pub fn start_asks<'a>(accounts: &[Account<'a>], from: &Leaving<'_>) -> Vec<&'a str> {
    let mut left: Vec<&Account<'a>> = accounts.iter().filter(|a| a.name != from.name).collect();
    left.sort_by_key(|a| {
        (
            a.billing.is_metered(),
            from.provider != Some(a.provider),
            a.name,
        )
    });
    left.into_iter().map(|a| a.name).collect()
}

pub fn start_in(
    config: &ConfigFile,
    going: &[EndpointName],
    chat: Option<&EndpointName>,
) -> Option<EndpointName> {
    let start = config
        .default
        .as_ref()
        .filter(|start| going.contains(start))?;
    let from = config.endpoints.get(start);
    let accounts: Vec<Account<'_>> = config
        .endpoints
        .iter()
        .filter(|(name, _)| !going.contains(name))
        .map(|(name, endpoint)| Account {
            name: name.as_str(),
            provider: endpoint.provider,
            billing: endpoint.auth.billing(),
        })
        .collect();
    let chat = chat
        .filter(|chat| chat.names_keys_file() || accounts.iter().any(|a| a.name == chat.as_str()));
    start_after(
        &accounts,
        &Leaving {
            name: start.as_str(),
            provider: from.map(|endpoint| endpoint.provider),
            billing: from.map(|endpoint| endpoint.auth.billing()),
        },
        chat.map(EndpointName::as_str),
    )
    .and_then(EndpointName::parse)
}

pub fn answering_on(name: &str, next: &str) -> String {
    format!("This chat is answering on {name}. {next}")
}

pub fn working_on(agents: &[String], name: &str) -> String {
    let names: Vec<String> = agents
        .iter()
        .map(|a| a.trim_start_matches('/').to_owned())
        .collect();
    let (verb, pronoun) = if agents.len() == 1 {
        ("is", "it's")
    } else {
        ("are", "they're")
    };
    format!(
        "{} {verb} working on {name}. Try again once {pronoun} done.",
        wisp_tool_text::output::and_list(&names)
    )
}

pub fn provider_in_use(name: &str) -> String {
    format!("This chat is on {name}. Switch account first (/account).")
}

pub fn no_other_account() -> String {
    "No other account for this chat to go to. Add one first (/login).".to_owned()
}

pub fn gone_words(name: &str) -> String {
    format!("{name} is gone; /account chooses another.")
}

pub fn signed_out_elsewhere(name: &str) -> String {
    format!("{name} was signed out in another window.")
}

pub fn in_use(name: &str) -> String {
    format!("{name} is in use. Try again in a moment.")
}

pub fn model_provider(model: &str) -> Option<ProviderKind> {
    let claude = model.starts_with(crate::provider::anthropic::capabilities::ID_PREFIX);
    let gemini = model.starts_with(crate::provider::gemini::ID_PREFIX);
    let row = crate::models::ModelTable::shared().find(model);
    row.and_then(|row| row.provider)
        .or(claude.then_some(ProviderKind::Anthropic))
        .or(gemini.then_some(ProviderKind::Gemini))
}

pub fn billing_provider(billing: Billing) -> Option<ProviderKind> {
    match billing {
        Billing::AnthropicPlan => Some(ProviderKind::Anthropic),
        Billing::ChatGptPlan => Some(ProviderKind::OpenAi),
        Billing::AntigravityPlan => Some(ProviderKind::Gemini),
        Billing::Metered | Billing::Mixed => None,
    }
}

pub fn shown(name: &str) -> &str {
    if name == ENV { KEYS_FILE_ACCOUNT } else { name }
}

pub fn named(name: &str) -> String {
    if name == ENV {
        KEYS_FILE_ACCOUNT.to_owned()
    } else {
        format!("account {name}")
    }
}

#[cfg(test)]
pub(crate) mod tests;
