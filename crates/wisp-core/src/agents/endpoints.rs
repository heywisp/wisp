use std::collections::BTreeMap;
use std::sync::Arc;

use wisp_protocol::settings::{Kind, SettingValue};
use wisp_protocol::{Billing, looks_like_key, shown_model};

use crate::agent::Connection;
use crate::endpoints::{ENV, EndpointName, ProviderKind};
use crate::kit::Connected;
use crate::models::ModelTable;
use crate::provider::{ModelCapabilities, Provider};

#[derive(Clone)]
pub struct Ready {
    pub name: String,
    pub provider: Arc<dyn Provider>,
    pub priced: bool,
    pub gateway: bool,
}

impl Ready {
    pub fn kind(&self) -> ProviderKind {
        self.provider.kind()
    }

    pub fn row(&self, model: &str) -> Option<ModelCapabilities> {
        self.provider.capabilities(model)
    }

    pub(crate) fn can_discover(&self, model: &str) -> bool {
        self.provider.list_key().is_some()
            && provider_mismatch(self.provider.as_ref(), model).is_ok()
    }

    pub fn serves(&self, model: &str) -> bool {
        self.row(model).is_some()
    }

    pub fn billing(&self) -> wisp_protocol::Billing {
        self.provider.billing()
    }

    pub fn keeps_warm(&self) -> bool {
        self.provider.keeps_warm()
    }

    pub fn account_kind(&self) -> &'static str {
        account_kind(self.kind(), self.billing(), self.gateway)
    }

    fn listed(&self) -> String {
        format!("{} ({})", plain(&self.name), self.account_kind())
    }
}

#[derive(Clone, Default)]
pub struct EndpointView {
    ready: Vec<Ready>,
    unavailable: Vec<(String, String)>,
    env_account: Option<String>,
}

impl std::fmt::Debug for EndpointView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ready: Vec<&str> = self.ready.iter().map(|r| r.name.as_str()).collect();
        let unavailable: Vec<&str> = self.unavailable.iter().map(|(n, _)| n.as_str()).collect();
        f.debug_struct("EndpointView")
            .field("ready", &ready)
            .field("unavailable", &unavailable)
            .finish()
    }
}

impl EndpointView {
    pub fn new(
        ready: &[Connection],
        unavailable: &[(String, String)],
        env_account: Option<&str>,
    ) -> Self {
        let mut ready: Vec<Ready> = ready
            .iter()
            .map(|c| Ready {
                name: c.name.clone(),
                provider: c.provider.clone(),
                priced: c.priced,
                gateway: is_gateway(c),
            })
            .collect();
        ready.sort_by(|a, b| a.name.cmp(&b.name));
        let mut unavailable = unavailable.to_vec();
        unavailable.sort();
        Self {
            ready,
            unavailable,
            env_account: env_account.map(str::to_owned),
        }
    }

    fn account<'a>(&'a self, name: &'a str) -> &'a str {
        match &self.env_account {
            Some(account) if name == ENV => account,
            _ => name,
        }
    }

    pub(crate) fn connected(connected: &Connected, own: &str) -> Self {
        let env_account = connected.env_account.as_deref();
        if !connected.ready.is_empty() {
            return Self::new(&connected.ready, &connected.unavailable, env_account);
        }
        let mut view = Self::new(&[], &connected.unavailable, env_account);
        view.ready.push(Ready {
            name: own.to_owned(),
            provider: connected.provider.clone(),
            priced: true,
            gateway: false,
        });
        view
    }

    pub fn is_empty(&self) -> bool {
        self.ready.is_empty() && self.unavailable.is_empty()
    }

    pub fn ready(&self, name: &str) -> Option<&Ready> {
        let name = self.account(name);
        self.ready.iter().find(|r| r.name == name)
    }

    pub fn unavailable(&self, name: &str) -> Option<&str> {
        let name = self.account(name);
        self.unavailable
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, reason)| reason.as_str())
    }

    pub(crate) fn can_discover(&self, model: &str) -> bool {
        self.ready.iter().any(|ready| ready.can_discover(model))
    }

    pub fn serving(&self, model: &str) -> Option<&Ready> {
        self.ready.iter().find(|r| r.serves(model))
    }

    pub fn serving_where(&self, model: &str, gone: &Gone) -> Option<&str> {
        self.ready
            .iter()
            .filter(|r| !gone.contains_key(&r.name) && r.serves(model))
            .find_map(|r| shown_endpoint(&r.name))
    }

    pub fn priced(&self, name: &str) -> bool {
        self.ready(name).is_none_or(|r| r.priced)
    }

    pub fn listed(&self, gone: &Gone) -> String {
        self.ready
            .iter()
            .filter(|r| !gone.contains_key(&r.name))
            .map(Ready::listed)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoggedKind {
    Known(ProviderKind),
    Unrecorded,
    Unknown,
}

impl LoggedKind {
    pub fn read(provider: Option<&str>) -> Self {
        match provider {
            None => Self::Unrecorded,
            Some(text) => ProviderKind::parse(text).map_or(Self::Unknown, Self::Known),
        }
    }

    pub fn shown(self) -> Option<&'static str> {
        match self {
            Self::Known(kind) => Some(kind.as_str()),
            Self::Unrecorded | Self::Unknown => None,
        }
    }

    pub fn takes(self, ready: &Ready, model: &str) -> bool {
        match self {
            Self::Known(kind) => ready.kind() == kind,
            Self::Unrecorded => ready.serves(model),
            Self::Unknown => false,
        }
    }
}

pub fn account_kind(
    provider: ProviderKind,
    billing: wisp_protocol::Billing,
    gateway: bool,
) -> &'static str {
    let shown = match provider {
        ProviderKind::Anthropic => wisp_tool_text::agents::AccountProvider::Anthropic,
        ProviderKind::OpenAi => wisp_tool_text::agents::AccountProvider::OpenAi,
        ProviderKind::Gemini => wisp_tool_text::agents::AccountProvider::Gemini,
    };
    wisp_tool_text::agents::account_kind(shown, !billing.is_metered(), gateway)
}

pub fn summary_inputs(
    summary: &wisp_protocol::settings::EndpointSummary,
) -> Option<(ProviderKind, wisp_protocol::Billing, bool)> {
    use wisp_protocol::settings::CredentialKind;
    let provider = ProviderKind::parse(&summary.provider)?;
    let auth = match summary.credential.as_ref()? {
        CredentialKind::ApiKey => crate::endpoints::AuthKind::ApiKey,
        CredentialKind::AuthToken => crate::endpoints::AuthKind::Bearer,
        CredentialKind::Other(text) => provider.auth(text)?,
    };
    let gateway = !summary.official?;
    Some((provider, auth.credential(String::new()).billing(), gateway))
}

fn is_gateway(connection: &Connection) -> bool {
    !crate::endpoints::is_official(connection.provider.kind(), &connection.context.endpoint)
}

pub fn connection_kind(connection: &Connection) -> &'static str {
    account_kind(
        connection.provider.kind(),
        connection.provider.billing(),
        is_gateway(connection),
    )
}

pub fn shown_endpoint(name: &str) -> Option<&str> {
    (EndpointName::parse(name).is_some() && !looks_like_key(name)).then_some(name)
}

pub fn plain(name: &str) -> &str {
    shown_endpoint(name).unwrap_or("(not shown)")
}

pub fn plain_setting(kind: Kind, value: &SettingValue) -> Option<String> {
    match (kind, value) {
        (Kind::EndpointName, SettingValue::Text(name)) => Some(plain(name).to_owned()),
        _ => None,
    }
}

pub fn served(ready: &Ready) -> Vec<&'static str> {
    ModelTable::shared()
        .rows()
        .iter()
        .map(|row| row.id.as_str())
        .filter(|id| ready.serves(id))
        .collect()
}

fn served_words(ready: &Ready) -> String {
    let served = served(ready);
    if served.is_empty() {
        "it serves none of the models Wisp knows".to_owned()
    } else {
        format!("name one it serves: {}", served.join(", "))
    }
}

pub type Gone = BTreeMap<String, Option<Billing>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fixed<'a> {
    Spawn,
    Preset(&'a str),
    Inherited,
}

pub fn provider_mismatch(provider: &dyn Provider, model: &str) -> Result<(), String> {
    if provider.official()
        && let Some(needs) = crate::endpoints::goes::model_provider(model)
        && needs != provider.kind()
    {
        let article = |kind: ProviderKind| match kind {
            ProviderKind::Anthropic | ProviderKind::OpenAi => "an",
            ProviderKind::Gemini => "a",
        };
        return Err(format!(
            "{model} needs {} {} account; choose {} {} model for this account",
            article(needs),
            needs.title(),
            article(provider.kind()),
            provider.kind().title()
        ));
    }
    Ok(())
}

pub fn limit_text(
    account: &str,
    model: Option<&str>,
    reset: Option<i64>,
    now: i64,
    cause: &wisp_tool_text::agents::Cause,
) -> String {
    let reset = wisp_tool_text::config::reset_time(reset, now);
    wisp_tool_text::agents::limit_reason(plain(account), model, reset.as_deref(), cause)
}

pub fn no_room_text(preset: &str, reasons: &[String]) -> String {
    format!(
        "no account preset {} names has room: {}",
        wisp_protocol::agents::shown_preset_name(preset).unwrap_or("(not shown)"),
        reasons.join("; ")
    )
}

pub fn used_up_text(
    account: &str,
    model: Option<&str>,
    reset: Option<i64>,
    now: i64,
    cause: &wisp_tool_text::agents::Cause,
) -> String {
    let reset = wisp_tool_text::config::reset_time(reset, now);
    wisp_tool_text::agents::used_up(plain(account), model, reset.as_deref(), cause)
}

pub fn unlisted_text(model: &str, elsewhere: Option<&str>) -> String {
    let set = elsewhere
        .map(|name| format!("; account {name} offers it (set endpoint)"))
        .unwrap_or_default();
    format!(
        "model {} isn't in the capability table{set}",
        shown_model(model)
    )
}

pub fn unknown_text(name: &str, listed: &str) -> String {
    let named = match shown_endpoint(name) {
        Some(name) => format!("no account \"{name}\""),
        None => "no account of that name".to_owned(),
    };
    format!("{named} in this chat; accounts: {listed}")
}

pub fn unavailable_text(name: &str, listed: &str) -> String {
    format!(
        "account {} can't be used right now; the user sees why in /account or wisp doctor; accounts you can use: {listed}",
        plain(name)
    )
}

pub fn gone_text(name: &str) -> String {
    format!(
        "account {} was removed in this chat, or edited so it no longer loads; agents can use it again from the next session",
        plain(name)
    )
}

fn target_words(ready: &Ready, fixed: Fixed<'_>) -> String {
    match fixed {
        Fixed::Preset(preset) => format!("account {} (preset {preset}'s)", plain(&ready.name)),
        Fixed::Spawn | Fixed::Inherited => format!("account {}", plain(&ready.name)),
    }
}

pub fn check_model(
    provider: &dyn Provider,
    endpoint: &str,
    model: &str,
) -> Result<(), super::PresetProblem> {
    if let Some(list) = provider.complete_model_list()
        && list.find(model).is_none()
    {
        return Err(super::PresetProblem::ModelNotServed {
            model: model.to_owned(),
            endpoint: endpoint.to_owned(),
        });
    }
    Ok(())
}

pub fn not_served_text(
    model: &str,
    target: &Ready,
    fixed: Fixed<'_>,
    preset_model: Option<&str>,
) -> String {
    let instead = preset_model
        .filter(|m| target.serves(m))
        .map(|m| {
            format!(
                "leave model out to use the preset's {}, or ",
                shown_model(m)
            )
        })
        .unwrap_or_default();
    format!(
        "model {} isn't one {} serves; {instead}{}",
        shown_model(model),
        target_words(target, fixed),
        served_words(target)
    )
}

#[cfg(test)]
mod tests;
