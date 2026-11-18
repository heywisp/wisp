use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use wisp_core::agent::{Connection, account_listing};
use wisp_core::agents::endpoints::EndpointView;
use wisp_core::endpoints::catalog::LiveCatalog;
use wisp_core::endpoints::{
    AuthKind, Availability, Choice, ConfigFile, EndpointBook, EndpointError, EndpointName,
    GIVEN_CONFIG, PathShown, ResolvedEndpoint, StartReason, default_model,
};
use wisp_core::models::list::SavedList;
use wisp_core::provider::Provider;
use wisp_core::provider::credential::{Credential, GIVEN_CREDENTIALS, Source};
use wisp_core::secrets::{
    CREDENTIALS, LockKind, PathNote, PathNoteKind, Prefetched, readable_by_others,
};
use wisp_core::session::Redactions;
use wisp_protocol::settings::{CredentialKind, EndpointSummary, KeyVariable};
use wisp_protocol::{CONFIG_FILE, IN_KEYCHAIN, KEYS_FILE, LOCK_FILE};
use wisp_render::chrome::{Endpoint, EndpointFacts, endpoint_facts_of};

use crate::keychain::{KeychainInputs, keychain};

pub const CHOICE_VAR: &str = "WISP_ACCOUNT";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chosen {
    pub variable: &'static str,
    pub name: String,
}

pub fn chosen_origin() -> &'static str {
    chosen_variable().map_or(CHOICE_VAR, |c| c.variable)
}

pub fn choice(flag: Option<&str>) -> (Option<String>, StartReason) {
    match (flag, chosen_variable()) {
        (Some(name), _) => (Some(name.to_owned()), StartReason::Flag),
        (None, Some(chosen)) => (Some(chosen.name), StartReason::Variable),
        (None, None) => (None, StartReason::Flag),
    }
}

pub fn config_path(
    name: &str,
    explicit: Option<&Path>,
    xdg_config_home: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return Some(path.to_owned());
    }
    let config = wisp_core::instructions::config_home(xdg_config_home, home.as_deref())?;
    Some(config.join("wisp").join(name))
}

pub fn missing(kind: std::io::ErrorKind) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    )
}

pub fn missing_given(path: &Path) -> String {
    let folder = path.parent().filter(|p| !p.as_os_str().is_empty());
    if folder.is_some_and(|f| !f.is_dir()) {
        format!("the folder of {GIVEN_CONFIG} doesn't exist")
    } else {
        format!("{GIVEN_CONFIG} doesn't exist")
    }
}

pub fn read_config(path: &Path, explicit: bool) -> Result<Option<ConfigFile>, String> {
    crate::tools::declare_settings();
    let shown = if explicit {
        GIVEN_CONFIG.to_owned()
    } else {
        wisp_core::workspace::home_shown(path)
    };
    let text = match std::fs::read(path) {
        Ok(bytes) => String::from_utf8(bytes).map_err(|_| format!("{shown} is not valid UTF-8"))?,
        Err(e) if missing(e.kind()) && !explicit => return Ok(None),
        Err(e) if missing(e.kind()) => return Err(missing_given(path)),
        Err(e) => return Err(format!("could not read {shown}: {}", e.kind())),
    };
    let origin = if explicit {
        PathShown::Given
    } else {
        PathShown::Default
    };
    ConfigFile::parse(path, origin, &text)
        .map(Some)
        .map_err(|e| e.to_string())
}

pub struct Flags<'a> {
    pub env_file: Option<&'a Path>,
    pub config: Option<&'a Path>,
    pub endpoint: Option<&'a str>,
    pub keychain: &'a crate::keychain::Keychain,
    pub set: &'a [wisp_core::settings::Given],
}

pub struct Sources {
    pub inputs: KeychainInputs,
    pub config_path: Option<PathBuf>,
    pub config_shown: PathShown,
    pub default_config: Option<PathBuf>,
    pub config: Result<Option<ConfigFile>, String>,
    pub secrets: Prefetched,
    pub choice: Option<String>,
    pub choice_reason: StartReason,
    pub variable: Option<Chosen>,
    pub set: Vec<wisp_core::settings::Given>,
    pub off: wisp_core::plugin::registry::ProvidersOff,
}

pub enum Failure {
    Files(String),
    Resolve(EndpointError),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Files(e) => f.write_str(e),
            Self::Resolve(e) => write!(f, "{e}"),
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    pub static TEST_CHOICE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

fn chosen_variable() -> Option<Chosen> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(name) = cleaned_choice(TEST_CHOICE.with(|c| c.borrow().clone())) {
        return Some(Chosen {
            variable: CHOICE_VAR,
            name,
        });
    }
    #[cfg(any(test, feature = "test-support"))]
    if !crate::paths::process_env_used() {
        return None;
    }
    let read = |variable: &'static str, value: Option<String>| {
        cleaned_choice(value).map(|name| Chosen { variable, name })
    };
    read(CHOICE_VAR, std::env::var("WISP_ACCOUNT").ok())
        .or_else(|| read("WISP_ENDPOINT", std::env::var("WISP_ENDPOINT").ok()))
}

fn cleaned_choice(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

impl Sources {
    pub fn load(
        flags: &Flags<'_>,
        xdg_config_home: Option<PathBuf>,
        home: Option<PathBuf>,
    ) -> Self {
        Self::load_with(flags, xdg_config_home, home, false)
    }

    pub fn load_with(
        flags: &Flags<'_>,
        xdg_config_home: Option<PathBuf>,
        home: Option<PathBuf>,
        mid_session: bool,
    ) -> Self {
        let default_config = config_path(CONFIG_FILE, None, xdg_config_home.clone(), home.clone());
        let config_path = config_path(
            CONFIG_FILE,
            flags.config,
            xdg_config_home.clone(),
            home.clone(),
        );
        let config = match &config_path {
            Some(path) => read_config(path, flags.config.is_some()),
            None => Ok(None),
        };
        let home_text = home.as_ref().map(|h| h.display().to_string());
        let env_path = self::config_path(KEYS_FILE, flags.env_file, xdg_config_home, home);
        let inputs = KeychainInputs::load(
            flags.keychain.detected().available(),
            env_path,
            flags.env_file.is_some(),
            home_text.as_deref(),
        );
        let config_shown = if flags.config.is_some() {
            PathShown::Given
        } else {
            PathShown::Default
        };
        let (choice, choice_reason) = choice(flags.endpoint);
        let parsed = config.as_ref().ok().and_then(Option::as_ref);
        let mut secrets = Prefetched::load(parsed);
        if let Some(config) = parsed {
            let exported = keychain(&inputs)
                .ok()
                .and_then(|k| k.variables().map(wisp_core::endpoints::exported))
                .unwrap_or(false);
            let records = if mid_session {
                flags.keychain.prefetch_short(config)
            } else {
                flags.keychain.prefetch(config, choice.as_deref(), exported)
            };
            secrets = secrets.with_keyring(records);
        }
        Self {
            inputs,
            config_path,
            config_shown,
            default_config,
            off: wisp_core::plugin::registry::providers_off(&wisp_core::settings::start_switches(
                flags.set,
                &parsed
                    .map(|config| config.settings.clone())
                    .unwrap_or_default(),
            )),
            config,
            secrets,
            choice,
            choice_reason,
            variable: chosen_variable(),
            set: flags.set.to_vec(),
        }
    }

    pub fn resolve(&self) -> Result<EndpointBook, Failure> {
        let keychain = keychain(&self.inputs).map_err(Failure::Files)?;
        let config = self.config.clone().map_err(Failure::Files)?;
        let choice = self.choice.as_deref().map(|name| Choice {
            name,
            reason: self.choice_reason,
        });
        let mut entries = EndpointBook::entries(config.as_ref(), &self.secrets, &keychain);
        wisp_core::endpoints::without_providers(&mut entries, &self.off);
        EndpointBook::pick_among(config.as_ref(), entries, &keychain, choice)
            .and_then(|book| book.without_providers(&self.off))
            .map(|book| book.with_flags(&self.set).with_marks(config.as_ref()))
            .map_err(Failure::Resolve)
    }

    pub fn book(&self) -> Result<EndpointBook, String> {
        self.resolve().map_err(|e| e.to_string())
    }

    pub fn entries(&self) -> Result<BTreeMap<EndpointName, Availability>, String> {
        let keychain = keychain(&self.inputs)?;
        let config = self.config.as_ref().map_err(Clone::clone)?;
        let mut entries = EndpointBook::entries(config.as_ref(), &self.secrets, &keychain);
        wisp_core::endpoints::price_flagged(&mut entries, None, &self.set);
        wisp_core::endpoints::without_providers(&mut entries, &self.off);
        Ok(entries)
    }

    pub fn marks(
        &self,
        entries: &BTreeMap<EndpointName, Availability>,
    ) -> BTreeMap<EndpointName, u8> {
        let config = self.config.as_ref().ok().and_then(Option::as_ref);
        wisp_core::endpoints::marks::pick(config, wisp_core::endpoints::keys_file_account(entries))
    }

    pub fn shadowed(&self) -> Option<String> {
        let entries = self.entries().ok()?;
        wisp_core::endpoints::shadowed(&entries).map(str::to_owned)
    }

    pub fn redactions(&self) -> Redactions {
        let config = self.config.as_ref().ok().and_then(Option::as_ref);
        self.inputs.redactions(config)
    }

    pub fn file_words(&self) -> String {
        self.inputs.keys_file.clone()
    }

    pub fn off_stop(&self, name: &EndpointName) -> Option<String> {
        let entries = self.entries().ok()?;
        let target = wisp_core::endpoints::account(&entries, name);
        let Availability::Unavailable(unready) = entries.get(target)? else {
            return None;
        };
        if unready.cause != wisp_core::endpoints::Cause::ProviderOff {
            return None;
        }
        let removed = *self.off.get(unready.provider.as_str())?;
        Some(wisp_core::plugin::registry::off_stop(
            unready.provider.as_str(),
            removed,
        ))
    }

    pub fn start_failure(&self, failure: &Failure) -> String {
        if let Failure::Resolve(EndpointError::DefaultUnavailable { name, .. }) = failure
            && let Some(reason) = self.off_stop(name)
        {
            return format!(
                "wisp: {}",
                wisp_core::endpoints::unavailable_words(name, &reason)
            );
        }
        let generic = format!("wisp: {failure}");
        if !self.inputs.available() || self.choice.is_some() {
            return generic;
        }
        let Failure::Resolve(error) = failure else {
            return generic;
        };
        let config = self.config.as_ref().ok().and_then(Option::as_ref);
        let stored = config.is_some_and(|c| c.endpoints.values().any(|e| e.store().is_some()));
        let nothing = matches!(
            error,
            EndpointError::NoneSetUp
                | EndpointError::DefaultUnavailable { .. }
                | EndpointError::NoEndpointAvailable { .. }
        );
        if stored || !nothing {
            return generic;
        }
        let presence = crate::presence::Presence::scan(&self.inputs, config);
        match presence.unread_message(&self.file_words()) {
            Some(message) => format!("wisp: {message}"),
            None if matches!(error, EndpointError::NoneSetUp) => {
                format!("wisp: {KEYCHAIN_NOTHING_FOUND}")
            }
            None => generic,
        }
    }

    pub fn sign_in_helps(&self, failure: &Failure) -> bool {
        let Failure::Resolve(error) = failure else {
            return false;
        };
        let config = self.config.as_ref().ok().and_then(Option::as_ref);
        let subscription = |name: &EndpointName| {
            config.and_then(|c| c.endpoints.get(name)).is_some_and(|e| {
                e.auth.subscription().is_some() && !self.off.contains_key(e.provider.as_str())
            })
        };
        match error {
            EndpointError::NoneSetUp => true,
            EndpointError::NoEndpointAvailable { reasons } => {
                reasons.iter().any(|(name, _)| subscription(name))
            }
            EndpointError::DefaultUnavailable { name, .. }
            | EndpointError::EndpointUnavailable { name, .. } => subscription(name),
            EndpointError::Anthropic(_)
            | EndpointError::OpenAi(_)
            | EndpointError::Gemini(_)
            | EndpointError::File(_)
            | EndpointError::UnknownEndpoint { .. }
            | EndpointError::NoEndpointChosen { .. } => false,
        }
    }
}

pub const KEYCHAIN_NOTHING_FOUND: &str = "no account is set up yet. Run \"wisp setup\" in a terminal, or store a key with:\nprintenv VAR | wisp account add NAME --key-stdin";

pub const ENV_FILE_REFUSED: &str = "--env-file supplies variables, which Wisp doesn't read on a machine with a keychain. Store its keys once with: wisp account import --env-file FILE";

pub fn connections(
    entries: &BTreeMap<EndpointName, Availability>,
    models: Option<&Path>,
) -> (Vec<Connection>, Vec<(String, String)>) {
    let mut ready = Vec::new();
    let mut unavailable = Vec::new();
    wisp_core::session::register(&wisp_core::endpoints::held_secrets(entries));
    for (name, entry) in wisp_core::endpoints::listed(entries) {
        match entry {
            Availability::Ready(endpoint) => match endpoint.provider.connect(endpoint, models) {
                Ok(provider) => ready.push(Connection {
                    name: name.to_string(),
                    provider,
                    context: endpoint.log_context(),
                    priced: endpoint.priced(),
                }),
                Err(e) => {
                    wisp_core::session::register(&wisp_core::endpoints::redaction_items(
                        &endpoint.credential,
                        &endpoint.base_url,
                    ));
                    unavailable.push((name.to_string(), e.to_string()));
                }
            },
            Availability::Unavailable(why) => {
                unavailable.push((name.to_string(), why.reason.clone()));
            }
            Availability::SameAs(_) => {}
        }
    }
    (ready, unavailable)
}

#[derive(Default)]
pub struct Due {
    pub missing: Vec<Arc<dyn Provider>>,
    pub stale: Vec<Arc<dyn Provider>>,
    pub listed: Vec<String>,
}

pub use wisp_core::models::list::{LIST_WAIT, ROUND_EVERY};

pub fn due(entries: &BTreeMap<EndpointName, Availability>, folder: &Path, now: i64) -> Due {
    let mut due = Due::default();
    for (name, entry) in wisp_core::endpoints::listed(entries) {
        let Availability::Ready(endpoint) = entry else {
            continue;
        };
        if expired(endpoint) {
            continue;
        }
        let Ok(provider) = endpoint.provider.connect(endpoint, None) else {
            continue;
        };
        let Some(saved) = provider.list_key().map(|key| SavedList::load(folder, key)) else {
            continue;
        };
        due.listed.push(name.to_string());
        match saved {
            None if endpoint.credential.subscription().is_some() => due.missing.push(provider),
            None => due.stale.push(provider),
            Some(saved) if saved.stale(now) => due.stale.push(provider),
            Some(_) => {}
        }
    }
    due
}

pub fn fetches(providers: Vec<Arc<dyn Provider>>, folder: &Path) -> Vec<BoxFuture<'static, ()>> {
    let fetch = |provider: Arc<dyn Provider>| provider.refresh_list(folder.to_owned(), false);
    providers.into_iter().map(fetch).collect()
}

pub fn keep_lists_fresh(catalog: LiveCatalog, names: Vec<String>) {
    tokio::spawn(async move {
        loop {
            let round = names.iter().map(|name| catalog.refetch(name, false));
            futures::future::join_all(round).await;
            tokio::time::sleep(ROUND_EVERY).await;
        }
    });
}

pub async fn fetch_due(fetches: Vec<BoxFuture<'static, ()>>, deadline: Duration) {
    let running: Vec<_> = fetches.into_iter().map(tokio::spawn).collect();
    let _ = tokio::time::timeout(deadline, futures::future::join_all(running)).await;
}

pub fn view(
    entries: &BTreeMap<EndpointName, Availability>,
    ready: &[Connection],
    unavailable: &[(String, String)],
) -> EndpointView {
    let env_account = wisp_core::endpoints::env_account(entries);
    EndpointView::new(ready, unavailable, env_account.map(EndpointName::as_str))
}

pub fn credential_kind(credential: &Credential) -> CredentialKind {
    auth_credential(AuthKind::of(credential)).wire()
}

pub fn reach(endpoint: &ResolvedEndpoint) -> Endpoint {
    if endpoint.official() {
        Endpoint::Official
    } else {
        Endpoint::Custom
    }
}

pub fn auth_credential(auth: AuthKind) -> wisp_render::chrome::Credential {
    match auth {
        AuthKind::ApiKey => wisp_render::chrome::Credential::ApiKey,
        AuthKind::Bearer => wisp_render::chrome::Credential::AuthToken,
        AuthKind::ChatGptSubscription => wisp_render::chrome::Credential::ChatGptPlan,
        AuthKind::AnthropicSubscription => wisp_render::chrome::Credential::AnthropicPlan,
        AuthKind::AntigravitySubscription => wisp_render::chrome::Credential::AntigravityPlan,
    }
}

pub fn expired(endpoint: &ResolvedEndpoint) -> bool {
    matches!(&endpoint.credential, Credential::Subscription { profile: Some(profile), .. } if wisp_core::oauth::refused(profile))
}

fn entry_summary(
    name: &EndpointName,
    entry: &Availability,
    refused: &[(String, String)],
) -> Option<EndpointSummary> {
    match entry {
        Availability::Ready(r) => {
            let reason = refused
                .iter()
                .find(|(refused, _)| *refused == name.as_str())
                .map(|(_, why)| why.clone());
            Some(EndpointSummary {
                mark: None,
                models: Vec::new(),
                default: false,
                keys_file: false,
                voice_model: None,
                name: name.to_string(),
                provider: r.provider.as_str().to_owned(),
                credential: Some(credential_kind(&r.credential)),
                official: Some(r.official()),
                host: r.gateway_host(),
                priced: reason.is_none() && r.priced(),
                expired: expired(r),
                key_var: matches!(r.source, Source::Environment | Source::File(_)).then(|| {
                    KeyVariable {
                        name: r.key_var().shown().to_owned(),
                        in_keys_file: matches!(r.source, Source::File(_)),
                    }
                }),
                unavailable: reason,
                complete: false,
                default_model: Some(r.default_model(None)),
            })
        }
        Availability::Unavailable(u) => Some(EndpointSummary {
            mark: None,
            models: Vec::new(),
            default: false,
            keys_file: false,
            voice_model: None,
            name: name.to_string(),
            provider: u.provider.as_str().to_owned(),
            credential: u.auth.map(|auth| auth_credential(auth).wire()),
            official: u.official,
            host: None,
            unavailable: Some(u.reason.clone()),
            priced: false,
            expired: false,
            key_var: None,
            complete: false,
            default_model: Some(default_model(u.provider, u.auth).to_owned()),
        }),
        Availability::SameAs(_) => None,
    }
}

pub fn entry_facts(
    name: &EndpointName,
    entry: &Availability,
    refused: &[(String, String)],
) -> Option<EndpointFacts> {
    entry_summary(name, entry, refused)
        .as_ref()
        .map(endpoint_facts_of)
}

pub fn endpoint_facts(
    entries: &BTreeMap<EndpointName, Availability>,
    marks: &BTreeMap<EndpointName, u8>,
    refused: &[(String, String)],
    ready: &[Connection],
) -> Vec<EndpointFacts> {
    summaries(entries, marks, refused, None, ready)
        .iter()
        .map(endpoint_facts_of)
        .collect()
}

pub fn summaries(
    entries: &BTreeMap<EndpointName, Availability>,
    marks: &BTreeMap<EndpointName, u8>,
    refused: &[(String, String)],
    default: Option<&str>,
    ready: &[Connection],
) -> Vec<EndpointSummary> {
    let keys_file = wisp_core::endpoints::keys_file_account(entries);
    let default = default
        .and_then(EndpointName::parse)
        .map(|name| wisp_core::endpoints::account(entries, &name).to_string());
    wisp_core::endpoints::listed(entries)
        .filter_map(|(name, entry)| {
            let summary = entry_summary(name, entry, refused)?;
            let listing = account_listing(ready, name.as_str());
            Some(EndpointSummary {
                voice_model: ready
                    .iter()
                    .find(|c| c.name == name.as_str())
                    .and_then(|c| Some(c.provider.voice()?.model().to_owned())),
                mark: marks.get(name).copied(),
                models: listing.models,
                complete: listing.complete,
                default: default.as_deref() == Some(name.as_str()),
                keys_file: keys_file.is_some_and(|key| key == name),
                default_model: listing.default_model.or(summary.default_model),
                ..summary
            })
        })
        .collect()
}

pub const READABLE: &str = "is readable by other users (chmod 600)";

pub fn mode_warnings(paths: &[(Option<&Path>, PathShown)], home: Option<&str>) -> Vec<String> {
    paths
        .iter()
        .filter_map(|(path, shown)| path.map(|p| (p, *shown)))
        .filter(|(p, _)| readable_by_others(p))
        .map(|(p, shown)| {
            let shown = match shown {
                PathShown::Default => short(p, home),
                PathShown::Given => GIVEN_CONFIG.to_owned(),
            };
            format!("{shown} {READABLE}")
        })
        .collect()
}

pub fn short(path: &Path, home: Option<&str>) -> String {
    wisp_render::chrome::home_short(&path.display().to_string(), home)
}

pub fn shown_source(source: &Source, home: Option<&str>) -> String {
    match source {
        Source::Environment => "the environment".to_owned(),
        Source::File(path) => format!("Wisp's keys file ({})", short(path, home)),
        Source::Keyring => IN_KEYCHAIN.to_owned(),
        Source::CredentialFile {
            path,
            shown: PathShown::Default,
        } => short(path, home),
        Source::CredentialFile {
            shown: PathShown::Given,
            ..
        } => GIVEN_CREDENTIALS.to_owned(),
    }
}

const KEEP_LOCK_OUT: &str = concat!(
    "it holds nothing, but add a ",
    wisp_protocol::lock_file!(),
    " line to its .gitignore to keep it out of commits"
);

pub fn note_words(note: &PathNote, home: Option<&str>) -> String {
    let path = short(&note.path, home);
    let repo = note
        .repo
        .as_deref()
        .map_or_else(String::new, |r| short(r, home));
    match (note.kind, note.shown) {
        (PathNoteKind::CredentialsReadable, PathShown::Default) => {
            format!("{path} {READABLE}")
        }
        (PathNoteKind::CredentialsReadable, PathShown::Given) => {
            format!("{GIVEN_CREDENTIALS} {READABLE}")
        }
        (PathNoteKind::CredentialsInRepo { through_link }, PathShown::Default) => {
            let link = if through_link {
                " (reached through a link)"
            } else {
                ""
            };
            format!(
                "{CREDENTIALS} is in {path}, inside the git repository at {repo}{link}; keep it out of commits with a .gitignore line"
            )
        }
        (PathNoteKind::CredentialsInRepo { .. }, PathShown::Given) => format!(
            "{GIVEN_CREDENTIALS} is inside a git repository; keep it out of commits with a .gitignore line"
        ),
        (PathNoteKind::LockInRepo(LockKind::Config), PathShown::Default) => format!(
            "{CONFIG_FILE}'s lock file {LOCK_FILE} is in {path}, inside the git repository at {repo}; {KEEP_LOCK_OUT}"
        ),
        (PathNoteKind::LockInRepo(LockKind::Config), PathShown::Given) => format!(
            "the lock file {LOCK_FILE} beside {GIVEN_CONFIG} (links resolved) is inside a git repository; {KEEP_LOCK_OUT}"
        ),
        (PathNoteKind::LockInRepo(LockKind::Credentials), PathShown::Default) => format!(
            "the lock file {LOCK_FILE} beside {CREDENTIALS} is in {path}, inside the git repository at {repo}; {KEEP_LOCK_OUT}"
        ),
        (PathNoteKind::LockInRepo(LockKind::Credentials), PathShown::Given) => format!(
            "the lock file {LOCK_FILE} beside {GIVEN_CREDENTIALS} is inside a git repository; {KEEP_LOCK_OUT}"
        ),
        (PathNoteKind::CredentialsLinked, _) => {
            format!("{CREDENTIALS} is a symbolic link; Wisp keeps keys only in a regular file")
        }
        (PathNoteKind::TargetNotWritable, PathShown::Default) => {
            format!("{path}: Wisp can't write that folder")
        }
        (PathNoteKind::TargetNotWritable, PathShown::Given) => {
            format!("{GIVEN_CONFIG} (a link) points into a folder Wisp can't write")
        }
        (PathNoteKind::CredentialsHardLinks, PathShown::Default) => {
            format!(
                "{path} had other hard links; they still hold the old keys, including any removed one"
            )
        }
        (PathNoteKind::CredentialsHardLinks, PathShown::Given) => {
            format!(
                "{GIVEN_CREDENTIALS} had other hard links; they still hold the old keys, including any removed one"
            )
        }
        (PathNoteKind::HardLinks, _) => {
            format!("{CONFIG_FILE} had other hard links; they still hold the old text")
        }
    }
}

pub fn credential_warnings(sources: &Sources, home: Option<&str>) -> Vec<String> {
    let Some(path) = &sources.config_path else {
        return Vec::new();
    };
    wisp_core::secrets::path_notes(path, sources.config_shown)
        .iter()
        .filter(|n| n.kind == PathNoteKind::CredentialsReadable)
        .map(|n| note_words(n, home))
        .collect()
}

pub fn startup_recovery(error: impl std::fmt::Display) -> String {
    format!("{error}; sign in outside chat with wisp login, then start Wisp again")
}
