use std::path::{Path, PathBuf};
use std::time::Duration;

use wisp_core::endpoints::{
    Availability, ConfigFile, EndpointBook, EndpointName, GIVEN_CONFIG, KeyFrom, PathShown,
    PricingMode, ProviderKind, ResolvedEndpoint, StartReason, UrlSource, marks::Mark,
};
use wisp_core::provider::credential::{Credential, GIVEN_CREDENTIALS, Source};
use wisp_core::secrets::{
    CREDENTIALS, DetachNote, FileStore, OwnerState, PathNoteKind, StoreKind, VariableName,
};
use wisp_protocol::{ACCOUNT_FLAG, CONFIG_FILE, IN_KEYCHAIN};
use wisp_render::chrome::account::AccountKind;
use wisp_render::chrome::endpoint_card::plain_http_risk;

pub(crate) use wisp_start::endpoints::*;

pub(crate) struct Owners<'a> {
    pub(crate) this: Option<PathBuf>,
    pub(crate) default: Option<PathBuf>,
    pub(crate) default_shown: Option<&'a Path>,
    pub(crate) home: Option<&'a str>,
}

impl Owners<'_> {
    fn slot(&self, owner: &Path) -> String {
        if self.this.as_deref() == Some(owner) {
            return "this config file".to_owned();
        }
        match (&self.default, self.default_shown) {
            (Some(default), Some(shown)) if default == owner => short(shown, self.home),
            _ => "another config file".to_owned(),
        }
    }

    pub(crate) fn detach_words(&self, note: &DetachNote) -> String {
        let named: Vec<&PathBuf> = note.from.iter().flatten().collect();
        let by_hand = note.from.iter().any(Option::is_none);
        let lead = format!("This {CONFIG_FILE} now keeps its own copy of its keys");
        if named.is_empty() {
            return format!("{lead} (they were written by hand).");
        }
        let all_default = match (&self.default, self.default_shown) {
            (Some(default), Some(_)) => named.iter().all(|o| *o == default),
            _ => false,
        };
        let owner = match (all_default, self.default_shown) {
            (true, Some(shown)) => short(shown, self.home),
            _ => "another config file".to_owned(),
        };
        let some = if by_hand {
            " (some were written by hand)"
        } else {
            ""
        };
        format!("{lead} (they were written by {owner}){some}.")
    }
}

const NEXT_CHANGE: &str = "its next change gives this config its own copy";
const UNREACHABLE_ORPHANS: &str =
    "wisp account remove can't reach them, so delete them by hand or leave them";
const UNREACHABLE_ORPHAN: &str =
    "wisp account remove can't reach it, so delete it by hand or leave it";

fn ownership_warnings(
    sources: &Sources,
    config: &ConfigFile,
    store: &FileStore,
    home: Option<&str>,
) -> Vec<String> {
    let Ok(every) = store.read_every() else {
        return Vec::new();
    };
    let found = wisp_core::secrets::ownership(config, &every);
    let owners = Owners {
        this: std::fs::canonicalize(&config.path).ok(),
        default: sources
            .default_config
            .as_deref()
            .and_then(|p| std::fs::canonicalize(p).ok()),
        default_shown: sources.default_config.as_deref(),
        home,
    };
    let mut lines = Vec::new();
    for item in &found.not_owned {
        let owner = owners.slot(&item.owner);
        let what = match item.state {
            OwnerState::Uses => format!("shares its key with {owner}, which it was copied from"),
            OwnerState::Gone => "owned by another config file, which is gone".to_owned(),
            OwnerState::NoLongerUses => format!("owned by {owner}, which no longer uses it"),
            OwnerState::Unreadable => {
                format!("owned by {owner}, which can't be read or doesn't parse right now")
            }
        };
        lines.push(format!("{}: {what}; {NEXT_CHANGE}", item.name));
    }
    for name in &found.hand_written {
        lines.push(format!("{name}: written by hand; {NEXT_CHANGE}"));
    }
    for name in &found.unnamed {
        lines.push(format!(
            "{CREDENTIALS} holds a key for \"{name}\", which {CONFIG_FILE} doesn't name"
        ));
    }
    for name in &found.variable {
        lines.push(format!(
            "{CREDENTIALS} holds a key for \"{name}\"; {CONFIG_FILE} names it but reads its key from a variable"
        ));
    }
    for group in &found.orphans {
        let one = group.count == 1;
        let keys = if one {
            "1 key".to_owned()
        } else {
            format!("{} keys", group.count)
        };
        let (them, hint) = if one {
            ("it", UNREACHABLE_ORPHAN)
        } else {
            ("them", UNREACHABLE_ORPHANS)
        };
        let line = match (&group.owner, group.state) {
            (None, _) => {
                let keys = if one {
                    "1 hand-written key".to_owned()
                } else {
                    format!("{} hand-written keys", group.count)
                };
                format!(
                    "{CREDENTIALS} holds {keys} under a store this config doesn't use; another config in this folder may use {them}"
                )
            }
            (Some(_), Some(OwnerState::Gone)) => format!(
                "{CREDENTIALS} holds {keys} owned by another config file, which is gone; {hint}"
            ),
            (Some(owner), Some(OwnerState::Unreadable)) => format!(
                "{CREDENTIALS} holds {keys} owned by {}, which can't be read or doesn't parse right now, so doctor can't tell whether {} still used",
                owners.slot(owner),
                if one { "it is" } else { "they are" }
            ),
            (Some(owner), _) => format!(
                "{CREDENTIALS} holds {keys} owned by {}, which no longer uses {them}; {hint}",
                owners.slot(owner)
            ),
        };
        lines.push(line);
    }
    lines
}

fn credentials_row(sources: &Sources, config: &ConfigFile, home: Option<&str>) -> Option<String> {
    let store = FileStore::beside(&config.path, config.shown);
    if !store.exists() && !config.stores(StoreKind::File) {
        return None;
    }
    let shown = match config.shown {
        PathShown::Default => short(store.path(), home),
        PathShown::Given => GIVEN_CREDENTIALS.to_owned(),
    };
    let count = match (&config.store_id, sources.secrets.records()) {
        (Some(_), Ok(records)) if config.stores(StoreKind::File) => records.len(),
        (Some(id), _) => match store.read_all(id) {
            Ok(records) => records.len(),
            Err(e) => return Some(format!("{shown} (error: {e})")),
        },
        (None, _) => match store.read_every() {
            Ok(_) => 0,
            Err(e) => return Some(format!("{shown} (error: {e})")),
        },
    };
    let noun = if count == 1 { "key" } else { "keys" };
    Some(format!("{shown} ({count} {noun})"))
}

pub(crate) fn start_words(
    book: &EndpointBook,
    config: Option<&ConfigFile>,
    home: Option<&str>,
) -> String {
    let file = match config {
        Some(c) if c.shown == PathShown::Given => GIVEN_CONFIG.to_owned(),
        _ => config.and_then(|c| c.path.file_name()).map_or_else(
            || CONFIG_FILE.to_owned(),
            |n| n.to_string_lossy().into_owned(),
        ),
    };
    match book.start_reason {
        StartReason::Flag => ACCOUNT_FLAG.to_owned(),
        StartReason::Variable => chosen_origin().to_owned(),
        StartReason::Exported | StartReason::EnvFile => {
            let implicit = book.implicit.as_ref().unwrap_or(&book.active);
            let from = match &implicit.source {
                Source::File(path) => short(path, home),
                other => shown_source(other, home),
            };
            format!("{} from {from}", implicit.key_var())
        }
        StartReason::Default => format!("default in {file}"),
        StartReason::OnlyOne => "the only available account".to_owned(),
    }
}

fn config_words(path: &Path, shown: PathShown, home: Option<&str>) -> String {
    let linked = std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink());
    match (shown, linked) {
        (PathShown::Given, false) => GIVEN_CONFIG.to_owned(),
        (PathShown::Given, true) => format!("{GIVEN_CONFIG} (a link)"),
        (PathShown::Default, false) => short(path, home),
        (PathShown::Default, true) => match std::fs::canonicalize(path) {
            Ok(target) => format!("{} \u{2192} {}", short(path, home), short(&target, home)),
            Err(_) => format!("{} (a link)", short(path, home)),
        },
    }
}

fn row(key: &str, value: &str) -> String {
    let value = wisp_tui::term::doctor::clean(value);
    format!("  {key:<18} {value}\n")
}

pub(crate) struct Report {
    pub(crate) text: String,
    pub(crate) broken: bool,
}

pub(crate) fn doctor(
    sources: &Sources,
    keychain: &crate::keychain::Keychain,
    variables: Result<&wisp_core::endpoints::Keychain<'_>, &str>,
    home: Option<&str>,
) -> Report {
    let short = |p: &Path| wisp_render::chrome::home_short(&p.display().to_string(), home);
    let mut text = String::from("\naccounts\n");
    let mut broken = false;
    let mut warnings = vec![];
    let mut environment = None;
    let config_line = match (&sources.config_path, &sources.config) {
        (Some(path), Ok(Some(_))) => config_words(path, sources.config_shown, home),
        (Some(path), Ok(None)) => format!("none ({} not found)", short(path)),
        (None, _) => "none (no home directory)".to_owned(),
        (Some(_), Err(e)) => {
            broken = true;
            format!("error: {e}")
        }
    };
    text.push_str(&row("config", &config_line));
    if let Ok(Some(config)) = &sources.config {
        let entries = sources.entries().unwrap_or_default();
        let keys_file = wisp_core::endpoints::keys_file_account(&entries).map(EndpointName::as_str);
        for (i, line) in crate::settings::doctor_rows(&config.settings, keys_file)
            .iter()
            .enumerate()
        {
            text.push_str(&row(if i == 0 { "settings" } else { "" }, line));
        }
    }
    let available = sources.inputs.available();
    let file_words = sources.file_words();
    let parsed = sources.config.as_ref().ok().and_then(Option::as_ref);
    if available {
        if sources.inputs.explicit_env_file {
            let line = match (&sources.inputs.env_path, sources.inputs.env_text()) {
                (Some(path), Some(_)) => {
                    let path = short(path);
                    format!(
                        "{path} (not read here: this machine has a keychain; wisp account import --env-file {path} stores its keys)"
                    )
                }
                _ => format!(
                    "{} (not read here: this machine has a keychain)",
                    wisp_core::secrets::GIVEN_ENV_FILE
                ),
            };
            text.push_str(&row("keys file", &line));
        }
        let presence = crate::presence::Presence::scan(&sources.inputs, parsed);
        let set: Vec<String> = crate::presence::provider_names()
            .into_iter()
            .filter(|n| presence.env.contains(n))
            .map(|n| n.to_string())
            .collect();
        if !set.is_empty() {
            let verb = if set.len() == 1 { "is" } else { "are" };
            environment = Some(format!(
                "{} {verb} set, not read: this machine has a keychain",
                wisp_core::secrets::import::and_words(&set)
            ));
        }
        if let Some(problem) = crate::presence::scan_problem(&sources.inputs, &file_words) {
            warnings.push(problem);
        } else if !presence.file.is_empty() {
            warnings.push(format!(
                "{file_words} holds keys Wisp doesn't read on this machine"
            ));
        }
    } else {
        let env_line = match (&sources.inputs.env_path, sources.inputs.env_file()) {
            (Some(path), Ok(Some(_))) => short(path),
            (Some(path), Ok(None)) => format!("none ({} not found)", short(path)),
            (None, _) => "none (no home directory)".to_owned(),
            (Some(_), Err(e)) => {
                broken = true;
                format!("error: {e}")
            }
        };
        text.push_str(&row("keys file", &env_line));
    }
    let keys = sources
        .secrets
        .keyring_records()
        .values()
        .filter(|r| matches!(r, Ok(Some(_))))
        .count();
    let keychain_row = row("keychain", &keychain.row(keys));
    let (Ok(config), Ok(variables)) = (&sources.config, variables) else {
        text.push_str(&keychain_row);
        if let Some(line) = &environment {
            text.push_str(&row("environment", line));
        }
        return Report { text, broken };
    };
    if let Some(config) = config
        && let Some(line) = credentials_row(sources, config, home)
    {
        text.push_str(&row("credentials", &line));
    }
    text.push_str(&keychain_row);
    if let Some(line) = &environment {
        text.push_str(&row("environment", line));
    }
    let book = sources.book();
    match &book {
        Ok(book) => {
            let start = format!(
                "{} ({})",
                book.start,
                start_words(book, config.as_ref(), home)
            );
            text.push_str(&row("would start on", &start));
            if let Some(over) = &book.overridden {
                warnings.push(over.note());
            }
        }
        Err(e) => text.push_str(&row("would start on", &format!("nothing: {e}"))),
    }
    let mut entries = EndpointBook::entries(config.as_ref(), &sources.secrets, variables);
    wisp_core::endpoints::without_providers(&mut entries, &sources.off);
    for (name, entry) in wisp_core::endpoints::listed(&entries) {
        let line = match entry {
            Availability::Ready(r) => {
                let host = r.host().unwrap_or_else(|| "(no host)".to_owned());
                let word = match r.credential {
                    Credential::ApiKey(_) => wisp_render::chrome::account::API_KEY,
                    Credential::AuthToken(_) => "auth token",
                    Credential::Subscription { .. } => "key",
                };
                let key = match (&r.source, &r.url_source) {
                    (Source::Keyring, UrlSource::Stored(_)) => {
                        format!("{word} and URL in {IN_KEYCHAIN}")
                    }
                    (Source::Keyring, _) => format!("{word} in {IN_KEYCHAIN}"),
                    (Source::CredentialFile { .. }, UrlSource::Stored(_)) => {
                        format!("{word} and URL in {CREDENTIALS}")
                    }
                    (Source::CredentialFile { .. }, _) => format!("{word} in {CREDENTIALS}"),
                    (Source::Environment | Source::File(_), _) => r.key_var().to_string(),
                };
                let kind = AccountKind::of(
                    auth_credential(wisp_core::endpoints::AuthKind::of(&r.credential)),
                    reach(r),
                    r.provider.as_str(),
                );
                let mut line = format!("{} · {key} \u{2192} {host}", kind.label());
                if let UrlSource::Variable(var, _) = &r.url_source {
                    line.push_str(&format!(" (url from {var})"));
                }
                line.push_str(&format!(" · from {}", shown_source(&r.source, home)));
                line.push_str(match r.pricing {
                    PricingMode::List => " · list prices",
                    PricingMode::None => " · no prices",
                });
                if available && matches!(r.source, Source::CredentialFile { .. }) {
                    line.push_str(&format!(
                        " · a keychain is available: wisp account edit {name} --store keyring moves it"
                    ));
                }
                if r.plain_http() {
                    warnings.push(plain_http_risk(name.as_ref(), &host));
                }
                line
            }
            Availability::Unavailable(u) => format!("unavailable: {}", u.reason),
            Availability::SameAs(twin) => format!("same as {twin}"),
        };
        text.push_str(&row(name.as_str(), &line));
    }
    if let Some(config) = config {
        for (name, endpoint) in &config.endpoints {
            let named = [
                (
                    "key_env",
                    match &endpoint.key {
                        KeyFrom::Variable(var) => Some(var),
                        _ => None,
                    },
                ),
                ("url_env", endpoint.url_env.as_ref()),
            ];
            for (field, var) in named {
                if var.is_some_and(VariableName::looks_like_key) {
                    warnings.push(format!(
                        "{name}'s {field} looks like a key, not a variable name; move the key out of {CONFIG_FILE}"
                    ));
                }
            }
            if endpoint.url_has_credentials() {
                let advice = url_advice(name, endpoint.store().is_some(), available, &file_words);
                warnings.push(format!(
                    "{name}: the url holds user info or a query; {advice}"
                ));
            }
        }
        if config.env.is_some() {
            let unused = format!(
                "{} in {CONFIG_FILE} is unused",
                wisp_protocol::KEYS_FILE_ACCOUNT
            );
            let implicit = wisp_core::endpoints::ProviderKind::IMPLICIT;
            match entries.get(&EndpointName::env()) {
                _ if available => warnings.push(format!(
                    "{unused}: {}",
                    wisp_core::endpoints::unread_env(implicit, &file_words)
                )),
                Some(Availability::SameAs(twin)) if config.endpoints.contains_key(twin) => {
                    warnings.push(format!(
                        "{unused}: the {} key is {twin}'s, whose settings apply",
                        implicit.variable_family()
                    ));
                }
                _ => {}
            }
        }
        let keys_file = wisp_core::endpoints::keys_file_account(&entries);
        for (name, _) in config.marks.iter().filter(|(_, m)| *m == Mark::Bad) {
            if let Some(name) = if name.is_env() { keys_file } else { Some(name) } {
                warnings.push(format!(
                    "{name}'s mark isn't 1 to 6, so Wisp picks its colour"
                ));
            }
        }
        warnings.extend(wisp_core::endpoints::shadowed(&entries).map(str::to_owned));
        if let Some(default) = &config.default
            && !matches!(
                entries.get(default),
                Some(Availability::Ready(_) | Availability::SameAs(_))
            )
        {
            warnings.push(wisp_core::endpoints::unavailable(default, true));
        }
    }
    let config_file = config.as_ref().map(|c| c.path.as_path());
    let env_file_path = sources
        .inputs
        .env_file()
        .ok()
        .flatten()
        .map(|f| f.path.as_path());
    warnings.extend(mode_warnings(
        &[
            (config_file, sources.config_shown),
            (env_file_path, PathShown::Default),
        ],
        home,
    ));
    if let Some(path) = &sources.config_path {
        warnings.extend(
            wisp_core::secrets::path_notes(path, sources.config_shown)
                .iter()
                .filter(|n| n.kind != PathNoteKind::CredentialsLinked)
                .map(|n| note_words(n, home)),
        );
    }
    if let Some(config) = config {
        let store = FileStore::beside(&config.path, config.shown);
        if store.exists() {
            warnings.extend(ownership_warnings(sources, config, &store, home));
        }
    }
    for warning in warnings {
        text.push_str(&row("warning", &warning));
    }
    Report { text, broken }
}

pub(crate) fn url_advice(name: &EndpointName, stored: bool, available: bool, file: &str) -> String {
    match (stored, available) {
        (true, _) => format!(
            "store the URL with the key instead: run wisp account edit {name} and change the URL"
        ),
        (false, false) => format!("put credentials in {file} and use url_env"),
        (false, true) => "store the key and URL with wisp account import".to_owned(),
    }
}

pub(crate) enum Target {
    Probe(Box<ResolvedEndpoint>),
    Twin(EndpointName, Box<ResolvedEndpoint>),
    Skip { why: String, fatal: bool },
}

pub(crate) fn check_skipped(file: &str) -> String {
    format!("skipped: the config or {file} has errors")
}

pub(crate) fn check_targets(
    sources: &Sources,
    variables: Result<&wisp_core::endpoints::Keychain<'_>, &str>,
    names: &[String],
) -> Option<Vec<(String, Target)>> {
    let (Ok(config), Ok(variables)) = (&sources.config, variables) else {
        return None;
    };
    let mut entries = EndpointBook::entries(config.as_ref(), &sources.secrets, variables);
    wisp_core::endpoints::without_providers(&mut entries, &sources.off);
    let named = !names.is_empty();
    let target = |entry: &Availability| match entry {
        Availability::Ready(r) => Target::Probe(Box::new(r.clone())),
        Availability::Unavailable(u) => Target::Skip {
            why: format!("not checked: {}", u.reason),
            fatal: named,
        },
        Availability::SameAs(twin) => match (named, entries.get(twin)) {
            (true, Some(Availability::Ready(r))) => Target::Twin(twin.clone(), Box::new(r.clone())),
            _ => Target::Skip {
                why: format!("same as {twin}"),
                fatal: false,
            },
        },
    };
    if !named {
        return Some(
            wisp_core::endpoints::listed(&entries)
                .map(|(name, entry)| (name.to_string(), target(entry)))
                .collect(),
        );
    }
    Some(
        names
            .iter()
            .map(|asked| match EndpointName::parse(asked) {
                Some(name) => match entries.get(&name) {
                    Some(entry) => (name.to_string(), target(entry)),
                    None => (
                        name.to_string(),
                        Target::Skip {
                            why: "no such account".to_owned(),
                            fatal: true,
                        },
                    ),
                },
                None => (
                    "(invalid name)".to_owned(),
                    Target::Skip {
                        why: "not an account name".to_owned(),
                        fatal: true,
                    },
                ),
            })
            .collect(),
    )
}

pub(crate) fn anthropic_probe_model<'a>(endpoint: &ResolvedEndpoint, model: &'a str) -> &'a str {
    if endpoint.official()
        && wisp_core::endpoints::goes::model_provider(model)
            .is_some_and(|p| p != ProviderKind::Anthropic)
    {
        ProviderKind::Anthropic.default_model()
    } else {
        model
    }
}

pub(crate) fn check_summary(targets: &[(String, Target)], model: &str) -> (String, String) {
    const GATEWAY_NOTE: &str = "a gateway may count it";
    let mut requests: Vec<String> = Vec::new();
    let mut costs: Vec<&'static str> = Vec::new();
    let mut gateway_note = false;
    let mut saw_local_sub = false;
    for (_, target) in targets {
        let endpoint = match target {
            Target::Probe(ep) | Target::Twin(_, ep) => ep.as_ref(),
            Target::Skip { .. } => continue,
        };
        let subscription = matches!(endpoint.credential, Credential::Subscription { .. });
        let (req, cost) = match (endpoint.provider, subscription) {
            (ProviderKind::Anthropic, true) | (ProviderKind::OpenAi, true) => {
                saw_local_sub = true;
                continue;
            }
            (ProviderKind::Anthropic, false) => {
                gateway_note = true;
                let probe_model = anthropic_probe_model(endpoint, model);
                (
                    format!(
                        "count_tokens with {}",
                        wisp_protocol::shown_model(probe_model)
                    ),
                    "free on Anthropic's side",
                )
            }
            (ProviderKind::OpenAi, false) => {
                gateway_note = true;
                ("models list".to_owned(), "free on OpenAI's side")
            }
            (ProviderKind::Gemini, true) => (
                "fetchAvailableModels".to_owned(),
                "included in your subscription",
            ),
            (ProviderKind::Gemini, false) => {
                gateway_note = true;
                ("models list".to_owned(), "free on Google's side")
            }
        };
        if !requests.contains(&req) {
            requests.push(req);
        }
        if !costs.contains(&cost) {
            costs.push(cost);
        }
    }
    if requests.is_empty() {
        if saw_local_sub {
            return ("none".to_owned(), "none".to_owned());
        }
        return (
            format!("count_tokens with {}", wisp_protocol::shown_model(model)),
            format!("free on Anthropic's side; {GATEWAY_NOTE}"),
        );
    }
    if gateway_note {
        costs.push(GATEWAY_NOTE);
    }
    (requests.join("; "), costs.join("; "))
}

pub(crate) async fn check(
    targets: Vec<(String, Target)>,
    model: &str,
    timeout: Duration,
    models: Option<&std::path::Path>,
) -> Report {
    let (request_words, cost_words) = check_summary(&targets, model);
    let mut text = String::from("\naccount check\n");
    text.push_str(&row("request", &request_words));
    text.push_str(&row("cost", &cost_words));
    let mut broken = false;
    for (name, target) in targets {
        let (endpoint, lead) = match target {
            Target::Probe(endpoint) => (endpoint, String::new()),
            Target::Twin(twin, endpoint) => (endpoint, format!("same as {twin}: ")),
            Target::Skip { why, fatal } => {
                broken |= fatal;
                text.push_str(&row(&name, &why));
                continue;
            }
        };
        let probe_model = if endpoint.provider == ProviderKind::Anthropic {
            anthropic_probe_model(&endpoint, model)
        } else {
            model
        };
        let line = match endpoint
            .provider
            .probe(&endpoint, probe_model, timeout, models)
            .await
        {
            Ok(outcome) => {
                broken |= outcome.fails();
                outcome.to_string()
            }
            Err(e) => {
                broken = true;
                format!("not checked: {e}")
            }
        };
        text.push_str(&row(&name, &format!("{lead}{line}")));
    }
    Report { text, broken }
}

#[cfg(test)]
mod tests;
