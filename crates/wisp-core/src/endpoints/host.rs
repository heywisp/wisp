use std::time::Duration;

use futures::future::BoxFuture;
use tokio::sync::watch;
use wisp_protocol::endpoint::{
    CheckResult, ConnectionField, EndpointEdit, EndpointSpec, NotSavedReason, StoreChoice,
    StoreKind, UrlEdit, VariableEdit,
};
use wisp_protocol::settings::EndpointSummary;
use wisp_protocol::{Secret, check_key};

use crate::agent::Connection;
use crate::secrets::LOCK_WAIT;
use crate::secrets::keyring::SHORT_BUDGET;
pub use crate::settings::host::HostError;

use super::{CHECK_TIMEOUT, EndpointName, ProviderKind, http_url};

pub const SAVE_WORST: Duration = CHECK_TIMEOUT
    .saturating_add(LOCK_WAIT)
    .saturating_add(SHORT_BUDGET);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    pub name: String,
    pub store: StoreKind,
    pub check: CheckResult,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NotSaved {
    pub name: String,
    pub why: NotSavedReason,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SaveOutcome {
    Saved(Saved),
    NotSaved(NotSaved),
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edited {
    pub name: String,
    pub changed: Vec<ConnectionField>,
    pub store: Option<StoreKind>,
    pub check: CheckResult,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EditOutcome {
    Edited(Edited),
    NotSaved(NotSaved),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removed {
    pub name: String,
    pub kept_item: bool,
    pub default_cleared: bool,
    pub notes: Vec<String>,
    pub left: bool,
    pub new_start: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RemoveOutcome {
    Removed(Removed),
    NotSaved(NotSaved),
}

#[derive(Debug, Clone, Default)]
pub struct Reconnected {
    pub ready: Vec<Connection>,
    pub unavailable: Vec<(String, String)>,
    pub endpoints: Vec<EndpointSummary>,
}

impl Reconnected {
    pub(crate) fn lists(&self, alias: &str) -> bool {
        self.ready.iter().any(|connection| connection.name == alias)
            || self.unavailable.iter().any(|(name, _)| name == alias)
            || self.endpoints.iter().any(|endpoint| endpoint.name == alias)
    }
}

pub fn auth_alias_of(
    command: &wisp_protocol::subscription_auth::AuthCommand,
) -> Option<(String, bool)> {
    use wisp_protocol::subscription_auth::AuthCommand;
    match command {
        AuthCommand::Start {
            alias: Some(alias),
            reauthorize,
            ..
        } => Some((alias.clone(), *reauthorize)),
        AuthCommand::Logout { alias, .. } => Some((alias.clone(), true)),
        AuthCommand::Reload { alias, .. } => Some((alias.clone(), false)),
        _ => None,
    }
}

pub fn unavailable() -> wisp_protocol::subscription_auth::AuthOutcome {
    wisp_protocol::subscription_auth::AuthOutcome::Failed {
        kind: wisp_protocol::subscription_auth::AuthFailureKind::Unavailable,
        message: "subscription authentication is unavailable in this session".to_owned(),
        problem: None,
        same_account: None,
    }
}

pub trait EndpointHost: Send + Sync {
    fn cancel_auth(&self) {}

    fn catalog(&self) -> Option<super::catalog::LiveCatalog> {
        None
    }

    fn auth_alias(
        &self,
        command: &wisp_protocol::subscription_auth::AuthCommand,
    ) -> Option<(String, bool)> {
        auth_alias_of(command)
    }

    fn check_logout(
        &self,
        _names: Vec<String>,
        _chat: Option<String>,
        _confirmed: bool,
    ) -> BoxFuture<'_, Option<wisp_protocol::subscription_auth::AuthOutcome>> {
        Box::pin(async { Some(unavailable()) })
    }

    fn auth(
        &self,
        command: wisp_protocol::subscription_auth::AuthCommand,
    ) -> BoxFuture<'_, wisp_protocol::subscription_auth::AuthEvent> {
        Box::pin(async move {
            wisp_protocol::subscription_auth::AuthEvent {
                request: command.request(),
                outcome: unavailable(),
            }
        })
    }

    fn save(
        &self,
        spec: EndpointSpec,
        key: Secret,
        check: bool,
        cancel: watch::Receiver<bool>,
    ) -> BoxFuture<'_, Result<SaveOutcome, HostError>>;

    fn edit(
        &self,
        name: String,
        edit: EndpointEdit,
        store: Option<StoreChoice>,
        check: bool,
    ) -> BoxFuture<'_, Result<EditOutcome, HostError>>;

    fn remove(
        &self,
        name: String,
        keep_key: bool,
    ) -> BoxFuture<'_, Result<RemoveOutcome, HostError>>;

    fn connections(&self) -> BoxFuture<'_, Result<Reconnected, HostError>>;
}

pub const NOTHING_TO_CHANGE: &str = "nothing to change: the edit sets no field";
pub fn no_table(verb: &str) -> String {
    format!(
        "{} has no table to {verb}",
        wisp_protocol::KEYS_FILE_ACCOUNT
    )
}

pub fn reserved_name() -> String {
    let sentence = wisp_protocol::account_name_reserved();
    let mut chars = sentence.trim_end_matches('.').chars();
    chars
        .next()
        .map(|first| first.to_lowercase().chain(chars).collect())
        .unwrap_or_default()
}

pub fn key_shaped_variable(field: &str) -> String {
    format!(
        "{field} needs a variable name such as WORK_API_KEY; what was given looks like a key, so it wasn't saved or shown"
    )
}

pub fn check_name(name: &str) -> Result<EndpointName, String> {
    match EndpointName::parse(name) {
        Some(name) if name.is_env() => Err(no_table("change")),
        Some(name) => Ok(name),
        None => Err(wisp_protocol::bad_account_name()),
    }
}

pub fn unknown_provider() -> String {
    format!(
        "that provider isn't known; Wisp accepts {}",
        ProviderKind::accepted()
    )
}

pub fn check_edit(
    name: &str,
    edit: &EndpointEdit,
    store: Option<StoreChoice>,
) -> Result<EndpointName, String> {
    let name = check_name(name)?;
    let moves = store.and_then(crate::secrets::StoreKind::chosen).is_some();
    if edit.is_empty() && !moves {
        return Err(NOTHING_TO_CHANGE.to_owned());
    }
    if let Some(provider) = &edit.provider
        && ProviderKind::parse(provider).is_none()
    {
        return Err(unknown_provider());
    }
    if let Some(UrlEdit::Set(url)) = &edit.url
        && http_url(url.expose()).is_none()
    {
        return Err(NOT_HTTP.to_owned());
    }
    if let Some(key) = &edit.key {
        check_key(key.expose()).map_err(|problem| problem.to_string())?;
    }
    for (field, variable) in [("key_env", &edit.key_env), ("url_env", &edit.url_env)] {
        if let Some(VariableEdit::Set(variable)) = variable
            && variable.looks_like_key()
        {
            return Err(key_shaped_variable(field));
        }
    }
    if edit.url.is_some() && matches!(edit.url_env, Some(VariableEdit::Set(_))) {
        return Err("url and url_env are two sources for the URL; give one".to_owned());
    }
    Ok(name)
}

pub const NOT_HTTP: &str = "the URL isn't an http or https URL (it isn't shown)";

pub fn check_spec(spec: &EndpointSpec, key: &Secret) -> Result<ProviderKind, String> {
    match EndpointName::parse(&spec.name) {
        Some(name) if name.is_env() => return Err(reserved_name()),
        Some(_) => {}
        None => return Err(wisp_protocol::bad_account_name()),
    }
    let Some(provider) = ProviderKind::parse(&spec.provider) else {
        return Err(unknown_provider());
    };
    if provider.auth(&spec.auth).is_none() {
        return Err(format!(
            "{} takes its key as one of: {}",
            provider.title(),
            provider
                .auths()
                .iter()
                .map(|a| a.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if provider
        .auth(&spec.auth)
        .and_then(super::AuthKind::subscription)
        .is_some()
        && spec.url.is_some()
    {
        return Err("subscription accounts do not accept a URL override".to_owned());
    }
    if let Some(url) = &spec.url
        && http_url(url.expose()).is_none()
    {
        return Err(NOT_HTTP.to_owned());
    }
    if let Some(auth) = provider.auth(&spec.auth).filter(|a| a.requires_oauth()) {
        return Err(provider_login_hint(auth));
    }
    check_key(key.expose()).map_err(|problem| problem.to_string())?;
    Ok(provider)
}

fn provider_login_hint(auth: super::AuthKind) -> String {
    match auth.subscription() {
        Some(kind) => {
            wisp_protocol::subscription_auth::SubscriptionProvider::from(kind).login_hint()
        }
        None => "subscription sign-in uses wisp login".to_owned(),
    }
}

#[cfg(test)]
mod tests;
