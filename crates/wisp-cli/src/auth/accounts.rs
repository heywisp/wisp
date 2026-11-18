use std::collections::BTreeMap;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use wisp_core::clock::{Clock, SystemClock};
use wisp_core::endpoints::{Availability, ConfigFile, EndpointName, ResolvedEndpoint};
use wisp_core::plan_usage::PlanUsageCache;
use wisp_core::provider::Provider;
use wisp_core::provider::credential::Source;
use wisp_protocol::plan_usage::{PlanUsageError, UsageState};
use wisp_protocol::settings::listed;
use wisp_protocol::subscription_auth::{SubscriptionProvider, Unusable};
use wisp_render::chrome::Endpoint;
use wisp_render::chrome::account::{self, AccountKind, Ending, Surface, fit_command, login_again};
use wisp_render::chrome::accounts::{AccountRow, Cells, accounts_panel};
use wisp_render::styled::{StyledLine, StyledSpan};
use wisp_render::text::wrap_plain;
use wisp_render::theme::glyphs::Glyphs;
use wisp_tool_text::config;
use wisp_tui::term::print::Stream;

use crate::manage::Context;
pub(crate) use wisp_start::resume::Health;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Held {
    Subscription(SubscriptionProvider),
    SetupToken,
    Stored,
    Variable { var: String, keys_file: bool },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Account {
    pub(crate) name: String,
    pub(crate) kind: AccountKind,
    pub(crate) held: Held,
    pub(crate) health: Health,
    pub(crate) host: Option<String>,
    pub(crate) mark: Option<u8>,
    pub(crate) off: Option<(String, String)>,
}

impl Account {
    fn needs(&self) -> Option<(String, bool)> {
        if let Some((words, _)) = &self.off {
            return Some((words.clone(), false));
        }
        let var = match &self.held {
            Held::Variable { var, .. } => Some(var.as_str()),
            _ => None,
        };
        let words = self.health.words(var)?;
        Some((
            words,
            matches!(self.health, Health::Expired | Health::KeyMissing),
        ))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Accounts {
    pub(crate) list: Vec<Account>,
    pub(crate) default: Option<String>,
    pub(crate) keys_file: String,
    pub(crate) shadowed: Option<String>,
}

impl Accounts {
    pub(crate) fn variable_line(&self, name: &str, ending: Ending) -> Option<String> {
        let Held::Variable { var, keys_file } = &self.get(name)?.held else {
            return None;
        };
        let file = keys_file.then_some(self.keys_file.as_str());
        Some(account::variable_line(name, var, file, ending))
    }

    pub(crate) fn unusable(&self) -> Vec<Unusable> {
        self.list
            .iter()
            .filter_map(|account| {
                let (state, sign_in) = account.needs()?;
                Some(Unusable {
                    account: account.name.clone(),
                    state,
                    sign_in,
                })
            })
            .collect()
    }

    pub(crate) fn get(&self, name: &str) -> Option<&Account> {
        self.list.iter().find(|account| account.name == name)
    }

    pub(crate) fn of(&self, provider: SubscriptionProvider) -> Vec<&Account> {
        self.list
            .iter()
            .filter(|account| account.held == Held::Subscription(provider))
            .collect()
    }

    pub(crate) fn is_first(&self, name: &str) -> bool {
        self.list.iter().all(|account| account.name == name)
    }

    pub(crate) fn taken(&self, name: &str) -> bool {
        self.get(name).is_some()
    }
}

fn variable_held(ready: Option<&ResolvedEndpoint>, var: String) -> Held {
    Held::Variable {
        var,
        keys_file: ready.is_none_or(|ready| matches!(ready.source, Source::File(_))),
    }
}

fn subscription_held(
    ctx: &Context<'_>,
    path: Option<&std::path::Path>,
    config: Option<&ConfigFile>,
    name: &EndpointName,
    provider: SubscriptionProvider,
) -> (Held, Option<Health>) {
    let (Some(path), Some(config)) = (path, config) else {
        return (Held::Subscription(provider), Some(Health::Unreadable));
    };
    use wisp_core::subscription_login::ProfileError;
    match wisp_core::subscription_login::profile(path, config, name, ctx.keychain.access()) {
        Ok((profile, kind)) => (
            Held::Subscription(provider),
            Some(match wisp_core::oauth::status(&profile, kind) {
                Ok(status) if status.refused => Health::Expired,
                Ok(_) => Health::SignedIn,
                Err(wisp_core::oauth::AuthError::LoggedOut) => Health::Expired,
                Err(_) => Health::Unreadable,
            }),
        ),
        Err(ProfileError::StaticToken) => (Held::SetupToken, None),
        Err(ProfileError::NoSignIn) => (Held::Subscription(provider), Some(Health::Expired)),
        Err(_) => (Held::Subscription(provider), Some(Health::Unreadable)),
    }
}

pub(crate) fn gather(ctx: &Context<'_>) -> Result<Accounts, String> {
    if ctx.config.is_some_and(|path| !path.exists()) {
        return Ok(Accounts::default());
    }
    let mut sources = ctx.sources();
    let off = std::mem::take(&mut sources.off);
    let entries = sources.entries()?;
    let config = sources.config.clone()?;
    let path = ctx.target(false).map(|(path, _)| path).ok();
    let marks = sources.marks(&entries);
    let mut list = Vec::new();
    for (name, entry) in wisp_core::endpoints::listed(&entries) {
        let Some(facts) = crate::endpoints::entry_facts(name, entry, &[]) else {
            continue;
        };
        let Some(credential) = facts.credential else {
            continue;
        };
        let (ready, cause) = match entry {
            Availability::Ready(ready) => (Some(ready), None),
            Availability::Unavailable(unready) => (None, Some(unready.cause)),
            Availability::SameAs(_) => continue,
        };
        let definition = config
            .as_ref()
            .and_then(|config| config.endpoints.get(name));
        let (held, health) = match (definition, ready, credential.plan()) {
            (Some(definition), _, Some(plan)) if definition.store().is_some() => {
                subscription_held(ctx, path.as_deref(), config.as_ref(), name, plan)
            }
            (Some(_), _, Some(_)) => (Held::SetupToken, None),
            (Some(definition), _, None) => match definition.key_env() {
                Some(var) => (variable_held(ready, var.to_string()), None),
                None => (Held::Stored, None),
            },
            (None, Some(ready), _) => (
                variable_held(Some(ready), ready.key_var().to_string()),
                None,
            ),
            (None, None, _) => continue,
        };
        let health = health.unwrap_or(Health::of(cause));
        let host = facts.host.or_else(|| {
            definition
                .and_then(|definition| definition.url.as_deref())
                .and_then(wisp_core::endpoints::url_host)
        });
        list.push(Account {
            mark: marks.get(name).copied(),
            name: facts.name,
            kind: AccountKind::of(
                credential,
                facts.reach.unwrap_or(Endpoint::Official),
                &facts.provider,
            ),
            off: ready.and_then(|ready| {
                off.get(ready.provider.as_str()).map(|removed| {
                    (
                        wisp_core::plugin::registry::off_words(ready.provider.as_str(), *removed),
                        wisp_core::plugin::registry::provider_fix(
                            ready.provider.as_str(),
                            *removed,
                        ),
                    )
                })
            }),
            held,
            health,
            host,
        });
    }
    Ok(Accounts {
        list,
        default: config
            .as_ref()
            .and_then(|config| config.default.as_ref())
            .map(|name| wisp_core::endpoints::account(&entries, name))
            .filter(|name| !name.is_env())
            .map(ToString::to_string),
        keys_file: sources.file_words(),
        shadowed: sources.shadowed(),
    })
}

pub(crate) type Looked = BTreeMap<String, Option<UsageState>>;

const LOOKUP_LIMIT: Duration = Duration::from_secs(3);

fn cells<'a>(account: &Account, accounts: &Accounts, looked: &'a Looked) -> Cells<'a> {
    if let Some((words, fix)) = &account.off {
        return Cells::Needs {
            words: words.clone(),
            fix: Some(fix.clone()),
        };
    }
    let needs = |words: &str, fix: bool| Cells::Needs {
        words: words.to_owned(),
        fix: fix.then(|| {
            let short = super::login::reaches(&account.name, accounts);
            login_again(Surface::Terminal, &account.name, short)
        }),
    };
    if let Some((words, fix)) = account.needs() {
        return needs(&words, fix);
    }
    match looked.get(&account.name) {
        None => Cells::Blank,
        Some(Some(UsageState {
            error: Some(PlanUsageError::Unsupported | PlanUsageError::Disabled),
            ..
        })) => Cells::Blank,
        Some(Some(UsageState {
            error: None,
            stale: false,
            snapshot: Some(usage),
            ..
        })) => Cells::Usage(usage),
        Some(_) => Cells::Unavailable,
    }
}

pub(crate) fn panel(
    accounts: &Accounts,
    only: Option<&str>,
    looked: &Looked,
    room: usize,
    glyphs: &Glyphs,
    marked: bool,
    now: i64,
) -> Vec<StyledLine> {
    let start = accounts
        .default
        .clone()
        .or_else(|| match accounts.list.as_slice() {
            [one] => Some(one.name.clone()),
            _ => None,
        });
    let rows: Vec<AccountRow<'_>> = accounts
        .list
        .iter()
        .filter(|account| only.is_none_or(|only| account.name == only))
        .map(|account| {
            let cells = cells(account, accounts, looked);
            let resets = match cells {
                Cells::Usage(found) => found
                    .windows
                    .iter()
                    .filter_map(|window| {
                        let time = config::reset_time(window.resets_at, now)?;
                        Some((window.key.as_str(), (config::RESET_IN, time)))
                    })
                    .collect(),
                _ => BTreeMap::new(),
            };
            AccountRow {
                name: &account.name,
                kind: account.kind,
                host: account.host.as_deref(),
                cells,
                resets,
                starts: start.as_deref() == Some(account.name.as_str()),
                mark: account.mark,
            }
        })
        .collect();
    let plain = |line: String| StyledLine::new(vec![StyledSpan::plain(line)]);
    if rows.is_empty() {
        return fit_command(
            "No accounts yet.",
            "Run wisp login to sign in.",
            room,
            glyphs,
        )
        .into_iter()
        .map(plain)
        .collect();
    }
    let mut lines = accounts_panel(&rows, room, glyphs.set, marked);
    if only.is_none()
        && let Some(line) = &accounts.shadowed
    {
        lines.extend(
            wrap_plain(line, room, "", "", glyphs)
                .into_iter()
                .map(plain),
        );
    }
    lines
}

pub(crate) async fn limits(
    accounts: &Accounts,
    only: Option<&str>,
    claude_usage: bool,
    cache: PlanUsageCache,
    connect: impl Fn(&str) -> Option<Arc<dyn Provider>>,
    deadline: tokio::time::Instant,
) -> Looked {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let lookups = accounts
        .list
        .iter()
        .filter(|account| only.is_none_or(|only| account.name == only))
        .filter(|account| matches!(account.held, Held::Subscription(_)))
        .filter(|account| account.health == Health::SignedIn && account.off.is_none())
        .filter(|account| claude_usage || account.kind != AccountKind::ClaudeSubscription)
        .map(|account| {
            let provider = connect(&account.name);
            let (cache, clock, name) = (cache.clone(), clock.clone(), account.name.clone());
            async move {
                let Some(provider) = provider else {
                    return (name, None);
                };
                provider.configure_plan_usage(true);
                let lookup = cache.lookup(&name, provider, clock);
                let state = tokio::time::timeout_at(deadline, lookup)
                    .await
                    .ok()
                    .flatten();
                (name, state)
            }
        })
        .collect::<Vec<_>>();
    futures::future::join_all(lookups)
        .await
        .into_iter()
        .collect()
}

pub(crate) fn no_such_account(name: &str, accounts: &Accounts) -> String {
    let names: Vec<&str> = accounts.list.iter().map(|a| a.name.as_str()).collect();
    match names.as_slice() {
        [] => format!("there's no account called {name}; run wisp login to sign in"),
        [one] => format!("there's no account called {name}; yours is {one}"),
        many => format!(
            "there's no account called {name}; yours are {}",
            listed(many, "and")
        ),
    }
}

pub(crate) fn show(
    ctx: &Context<'_>,
    accounts: &Accounts,
    only: Option<&str>,
    stream: Stream,
) -> Result<(), String> {
    let runtime = wisp_core::runtime::build(&mut tokio::runtime::Builder::new_current_thread())
        .map_err(|e| format!("could not look up usage: {e}"))?;
    let entries = ctx.sources().entries().unwrap_or_default();
    let connect = |name: &str| match entries.iter().find(|(key, _)| key.as_str() == name) {
        Some((_, Availability::Ready(ready))) => {
            ready.provider.connect(ready, ctx.models.as_deref()).ok()
        }
        _ => None,
    };
    let deadline = tokio::time::Instant::now() + LOOKUP_LIMIT;
    let looked = runtime.block_on(limits(
        accounts,
        only,
        ctx.claude_usage,
        crate::usage_cache(),
        connect,
        deadline,
    ));
    let now = SystemClock.now().as_second();
    let written = gather(ctx).and_then(|accounts| {
        let room = wisp_tui::term::prompt::terminal_columns();
        let glyphs = Glyphs::of(ctx.caps.glyphs.value);
        let marked = ctx.caps.depth_on(stream) != wisp_render::theme::Depth::None;
        let lines = panel(&accounts, only, &looked, room, &glyphs, marked, now);
        super::login::print_lines(&lines, &ctx.caps, stream, room)
            .map_err(|e| format!("could not write the list: {e}"))
    });
    runtime.block_on(wisp_core::oauth::settle());
    written
}

pub(crate) fn status(ctx: &Context<'_>, name: Option<&str>) -> ExitCode {
    let shown = gather(ctx).and_then(|accounts| match name {
        Some(name) if !accounts.taken(name) => {
            Err(format!("{}.", no_such_account(name, &accounts)))
        }
        _ => show(ctx, &accounts, name, Stream::Stdout),
    });
    match shown {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("wisp: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests;
