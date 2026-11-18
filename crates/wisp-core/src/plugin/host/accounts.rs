use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use wisp_plugin_api::config::{AccountList, AccountRow, LimitWindow, Limits, SignIn};
use wisp_plugin_api::usage::{AccountUsage, UsageState as State};
use wisp_protocol::RequestId;
use wisp_protocol::plan_usage::{Availability, PlanUsageError, UsageState};
use wisp_protocol::subscription_auth::{
    AuthCommand, AuthEvent, AuthOutcome, LocalState, SubscriptionEndpoint,
};

use crate::agents::endpoints;
use crate::endpoints::host::EndpointHost;
use crate::plan_usage::PlanUsageCache;

pub(crate) const SIGN_IN_WAIT: Duration = Duration::from_secs(3);

pub(crate) struct AccountsView {
    pub(crate) model: String,
    pub(crate) effort: String,
    pub(crate) rows: Vec<(String, AccountRow)>,
    pub(crate) auth: Option<Arc<dyn EndpointHost>>,
}

pub(crate) fn sign_in(state: LocalState) -> SignIn {
    match state {
        LocalState::Working => SignIn::Working,
        LocalState::Expired => SignIn::Expired,
        LocalState::Refreshable => SignIn::Refreshable,
        LocalState::Unavailable => SignIn::Unavailable,
        LocalState::SetupTokenExpiryUnknown => SignIn::ExpiryUnknown,
    }
}

pub(crate) fn limits(state: Option<UsageState>, usage_off: bool) -> Limits {
    if usage_off {
        return Limits {
            problem: Some(PlanUsageError::Disabled.to_string()),
            ..Limits::default()
        };
    }
    let Some(state) = state else {
        return Limits::default();
    };
    let (windows, limited) = state
        .snapshot
        .map_or_else(Default::default, |mut snapshot| {
            snapshot
                .windows
                .sort_by(|a, b| (a.duration_seconds, &a.key).cmp(&(b.duration_seconds, &b.key)));
            (
                snapshot
                    .windows
                    .into_iter()
                    .map(|window| LimitWindow {
                        label: window.label,
                        used_percent: window.used_percent.map(|p| p.value()),
                        resets_at: window.resets_at,
                        observed_at: window.observed_at,
                    })
                    .collect(),
                snapshot.availability == Some(Availability::Limited),
            )
        });
    Limits {
        windows,
        limited,
        problem: state.error.map(|error| error.to_string()),
    }
}

async fn signed_in(
    host: Arc<dyn EndpointHost>,
    cancel: &CancellationToken,
) -> Vec<SubscriptionEndpoint> {
    let read = tokio::spawn(async move {
        host.auth(AuthCommand::Status {
            request: RequestId(0),
            alias: None,
        })
        .await
    });
    tokio::select! {
        answered = tokio::time::timeout(SIGN_IN_WAIT, read) => match answered {
            Ok(Ok(AuthEvent { outcome: AuthOutcome::Status { endpoints }, .. })) => endpoints,
            _ => Vec::new(),
        },
        () = cancel.cancelled() => Vec::new(),
    }
}

impl AccountsView {
    pub(crate) async fn list(&self, cancel: &CancellationToken) -> AccountList {
        let subscribed = self.rows.iter().any(|(_, row)| row.limits.is_some());
        let endpoints = match &self.auth {
            Some(host) if subscribed => signed_in(host.clone(), cancel).await,
            _ => Vec::new(),
        };
        AccountList {
            model: self.model.clone(),
            effort: self.effort.clone(),
            rows: self
                .rows
                .iter()
                .map(|(alias, row)| AccountRow {
                    sign_in: endpoints
                        .iter()
                        .find(|endpoint| &endpoint.alias == alias)
                        .map(|endpoint| sign_in(endpoint.state)),
                    ..row.clone()
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct UsageAccount {
    pub name: String,
    pub yours: bool,
    pub off: bool,
    pub expired: bool,
    pub unavailable: bool,
}

pub(crate) fn state_of(account: &UsageAccount, state: Option<&UsageState>, awaited: bool) -> State {
    let error = state.and_then(|s| s.error);
    if account.off || error == Some(PlanUsageError::Disabled) {
        State::Off
    } else if account.expired
        || matches!(
            error,
            Some(PlanUsageError::AuthenticationRequired | PlanUsageError::ScopeRequired)
        )
    {
        State::SignedOut
    } else if account.unavailable
        || state.is_none_or(|s| {
            (s.snapshot.is_none() && (s.error.is_some() || (!s.loading && !awaited)))
                || s.error.is_some_and(PlanUsageError::needs_user)
        })
    {
        State::Unavailable
    } else if state.is_some_and(|s| s.snapshot.is_none()) {
        State::Checking
    } else if state.is_some_and(|s| s.stale) {
        State::Stale
    } else {
        State::Ok
    }
}

pub(crate) fn usage(
    accounts: &[UsageAccount],
    cache: &PlanUsageCache,
    now: i64,
) -> Vec<AccountUsage> {
    accounts
        .iter()
        .map(|account| {
            let reading = cache.state(&account.name, now);
            let state = state_of(account, reading.as_ref(), cache.awaited(&account.name));
            let retry_at = reading.as_ref().and_then(|s| match s.error {
                Some(PlanUsageError::Throttled { retry_at }) => retry_at,
                _ => None,
            });
            let limits = limits(reading, account.off);
            AccountUsage {
                name: endpoints::plain(&account.name).to_owned(),
                yours: account.yours,
                state,
                limited: limits.limited,
                retry_at,
                windows: if matches!(state, State::Ok | State::Stale) {
                    limits.windows
                } else {
                    Vec::new()
                },
            }
        })
        .collect()
}
