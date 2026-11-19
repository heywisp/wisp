use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::FutureExt;
use futures::future::BoxFuture;
use tokio::sync::watch;
use wisp_core::config_file::{ConfigText, NewEndpoint, canonical_config};
use wisp_core::endpoints::host::{
    EditOutcome, EndpointHost, HostError, NotSaved, Reconnected, RemoveOutcome, SaveOutcome, Saved,
    check_spec,
};
use wisp_core::endpoints::{
    AuthKind, CHECK_TIMEOUT, CheckOutcome, EndpointError, EndpointName, PathShown, PricingMode,
    ProviderKind, ResolvedEndpoint, UrlSource, default_model,
};
use wisp_core::provider::credential::Source;
use wisp_core::secrets::keyring::Budget;
use wisp_core::secrets::{
    Applied, Change, ChangeError, FileStore, KeyringError, Outcome, Secret, StoreKind,
    StoredRecord, keychain_off_for_key,
};
use wisp_protocol::endpoint::{
    CheckKind, CheckResult, EndpointEdit, EndpointSpec, NotSavedReason, StoreChoice as Wanted,
};

use super::{
    Context, Picked, change_error, credentials_words, early_store_check, pick_store_and_note,
    try_change,
};
use crate::endpoints::{self, Owners, Sources, note_words};
use crate::keychain::Keychain;
use wisp_start::watch::{SourcesWatch, view};

pub(crate) const REFUSED_REQUEST: &str = "refused the request";

const NOT_USABLE_YET: &str = "saved, not usable yet; retry loading this account";

pub(crate) fn unavailable_view(
    catalog: &wisp_core::endpoints::catalog::LiveCatalog,
    alias: &str,
    provider: Option<ProviderKind>,
    auth: Option<AuthKind>,
) -> Reconnected {
    let mut view = catalog.view();
    view.ready.retain(|connection| connection.name != alias);
    view.unavailable.retain(|(name, _)| name != alias);
    view.unavailable
        .push((alias.to_owned(), NOT_USABLE_YET.to_owned()));
    if let Some(summary) = view
        .endpoints
        .iter_mut()
        .find(|summary| summary.name == alias)
    {
        summary.unavailable = Some(NOT_USABLE_YET.to_owned());
    } else if let Some(provider) = provider {
        view.endpoints
            .push(wisp_protocol::settings::EndpointSummary {
                models: Vec::new(),
                mark: None,
                name: alias.to_owned(),
                provider: provider.as_str().to_owned(),
                credential: auth.map(|auth| endpoints::auth_credential(auth).wire()),
                official: None,
                host: None,
                unavailable: Some(NOT_USABLE_YET.to_owned()),
                priced: true,
                default: false,
                keys_file: false,
                expired: false,
                key_var: None,
                voice_model: None,
                complete: false,
                default_model: Some(default_model(provider, auth).to_owned()),
            });
    }
    view
}

pub(crate) fn unconfirmed_words(name: &EndpointName) -> String {
    format!(
        "the keychain didn't confirm the write, so {name}'s key may have been saved there; check your keychain app. Nothing else was changed"
    )
}

pub(crate) fn after_unconfirmed_words(which: &str) -> String {
    format!(
        "the keychain {which}, so nothing was saved. An earlier keychain write this run wasn't confirmed, so Wisp won't keep the key in {} instead; check your keychain app, then run wisp again",
        wisp_protocol::CREDENTIALS
    )
}

#[derive(Clone)]
pub(crate) struct CliEndpointHost {
    pub(crate) catalog: Option<wisp_core::endpoints::catalog::LiveCatalog>,
    pub(crate) auth: crate::auth::SessionAuth,
    pub(crate) env_file: Option<PathBuf>,
    pub(crate) config: Option<PathBuf>,
    pub(crate) endpoint: Option<String>,
    pub(crate) model: String,
    pub(crate) xdg_config_home: Option<PathBuf>,
    pub(crate) home: Option<PathBuf>,
    pub(crate) keychain: Arc<Keychain>,
    pub(crate) budget: Budget,
    pub(crate) set: Vec<wisp_core::settings::Given>,
    pub(crate) models: Option<PathBuf>,
}

struct Prepared {
    path: PathBuf,
    shown: PathShown,
    picked: Picked,
    notes: Vec<String>,
}

struct Draft {
    name: EndpointName,
    provider: ProviderKind,
    auth: AuthKind,
    spec: EndpointSpec,
    key: Secret,
}

fn cancelled(cancel: &watch::Receiver<bool>) -> bool {
    *cancel.borrow()
}

pub(crate) fn check_result(outcome: Result<CheckOutcome, EndpointError>) -> CheckResult {
    match outcome {
        Ok(CheckOutcome::Ok) => CheckResult::of(CheckKind::Ok),
        Ok(CheckOutcome::KeyRefused) => CheckResult::of(CheckKind::KeyRefused),
        Ok(CheckOutcome::Forbidden) => CheckResult::of(CheckKind::Forbidden),
        Ok(CheckOutcome::NotOffered) => CheckResult::of(CheckKind::NotOffered),
        Ok(other @ CheckOutcome::Unexpected { .. }) => CheckResult {
            outcome: CheckKind::Unexpected,
            reason: Some(other.to_string()),
        },
        Ok(other @ CheckOutcome::Unreachable(_)) => CheckResult {
            outcome: CheckKind::Unreachable,
            reason: Some(other.to_string()),
        },
        Err(e) => CheckResult {
            outcome: CheckKind::Unexpected,
            reason: Some(format!("not checked: {e}")),
        },
    }
}

fn probe_target(draft: &Draft, path: &Path, shown: PathShown) -> ResolvedEndpoint {
    let key = draft.key.expose().to_owned();
    let credential = draft.auth.credential(key);
    let (base_url, url_source) = match &draft.spec.url {
        Some(url) => (url.expose().to_owned(), UrlSource::Stored(StoreKind::File)),
        None => (draft.provider.default_url().to_owned(), UrlSource::Official),
    };
    ResolvedEndpoint {
        name: EndpointName::env(),
        provider: draft.provider,
        credential,
        base_url,
        source: Source::CredentialFile {
            path: wisp_core::secrets::credentials_path(path),
            shown,
        },
        key_env: None,
        url_source,
        pricing: PricingMode::List,
    }
}

impl CliEndpointHost {
    pub(crate) fn of(
        ctx: &Context<'_>,
        endpoint: Option<&str>,
        keychain: Arc<Keychain>,
        budget: Budget,
        set: &[wisp_core::settings::Given],
    ) -> CliEndpointHost {
        CliEndpointHost {
            catalog: None,
            auth: crate::auth::SessionAuth::default(),
            env_file: ctx.env_file.map(Path::to_path_buf),
            config: ctx.config.map(Path::to_path_buf),
            endpoint: endpoint.map(str::to_owned),
            model: ctx.model.to_owned(),
            xdg_config_home: ctx.xdg_config_home.clone(),
            home: ctx.home.clone(),
            keychain,
            budget,
            set: set.to_vec(),
            models: ctx.models.clone(),
        }
    }

    fn change_catalog(
        &self,
        alias: &str,
        removing: bool,
    ) -> Result<Option<wisp_core::endpoints::catalog::CatalogChange>, HostError> {
        self.catalog
            .as_ref()
            .map(|catalog| {
                if removing {
                    catalog.begin_logout(alias)
                } else {
                    catalog.begin(alias)
                }
            })
            .transpose()
            .map_err(|error| HostError(error.to_string()))
    }

    pub(crate) async fn remove_for(
        &self,
        name: String,
        keep_key: bool,
        chat: Option<EndpointName>,
        change: Option<wisp_core::endpoints::catalog::CatalogChange>,
    ) -> Result<RemoveOutcome, HostError> {
        let result = self.remove_endpoint(name.clone(), keep_key, chat).await?;
        if matches!(result, RemoveOutcome::Removed(_))
            && let Some(change) = change
        {
            self.publish_catalog(change, &name, None, None, true).await;
        }
        Ok(result)
    }

    pub(crate) async fn publish_catalog(
        &self,
        change: wisp_core::endpoints::catalog::CatalogChange,
        alias: &str,
        provider: Option<ProviderKind>,
        auth: Option<AuthKind>,
        removed: bool,
    ) {
        let mut view = match self.blocking(|host| host.reconnect()).await {
            Ok(view) => view,
            Err(_) => {
                let Some(catalog) = &self.catalog else {
                    return;
                };
                if !removed {
                    unavailable_view(catalog, alias, provider, auth)
                } else {
                    let mut view = catalog.view();
                    view.ready.retain(|connection| connection.name != alias);
                    view.unavailable.retain(|(name, _)| name != alias);
                    view
                }
            }
        };
        if removed {
            view.endpoints.retain(|endpoint| endpoint.name != alias);
        }
        change.publish(view);
    }

    pub(crate) fn context(&self) -> Context<'_> {
        Context {
            env_file: self.env_file.as_deref(),
            config: self.config.as_deref(),
            model: &self.model,
            caps: crate::prompt_caps(&wisp_tui::term::SavedDisplay::default()),
            claude_usage: false,
            xdg_config_home: self.xdg_config_home.clone(),
            home: self.home.clone(),
            keychain: &self.keychain,
            models: self.models.clone(),
        }
    }

    fn prepare(&self, spec: &EndpointSpec, name: &EndpointName) -> Result<Prepared, String> {
        let ctx = self.context();
        let home = ctx.home_text();
        let (path, shown) = ctx.target(false)?;
        let config = ConfigText::read(&path, shown)
            .and_then(|t| t.current())
            .map_err(|e| e.to_string())?;
        if config.endpoints.contains_key(name) {
            return Err(format!(
                "an account named \"{name}\" already exists; choose another name"
            ));
        }
        let store = FileStore::beside(&path, shown);
        early_store_check(&store, home.as_deref())?;
        let file = credentials_words(&store, home.as_deref());
        let (picked, note) = pick_store_and_note(Some(spec.store), &self.keychain, &file)
            .map_err(|origin| keychain_off_for_key(&origin))?;
        Ok(Prepared {
            path,
            shown,
            picked,
            notes: note.into_iter().collect(),
        })
    }

    fn written_notes(&self, applied: &Applied, path: &Path, shown: PathShown) -> Vec<String> {
        let ctx = self.context();
        let home = ctx.home_text();
        let (default, default_shown) = ctx.default_config();
        let owners = Owners {
            this: canonical_config(path),
            default,
            default_shown: default_shown.as_deref(),
            home: home.as_deref(),
        };
        let mut notes = Vec::new();
        if let Some(note) = &applied.detached {
            notes.push(owners.detach_words(note));
        }
        for note in &applied.notes {
            notes.push(format!("note: {}", note_words(note, home.as_deref())));
        }
        if let Outcome::Added { created: true, .. } = applied.outcome
            && shown == PathShown::Given
        {
            notes.push(format!("Created {}.", wisp_core::endpoints::GIVEN_CONFIG));
        }
        notes
    }

    fn write(
        &self,
        prepared: Prepared,
        draft: Draft,
        check: CheckResult,
    ) -> Result<SaveOutcome, String> {
        let Draft {
            name,
            provider,
            auth,
            spec,
            key,
        } = draft;
        let record = StoredRecord::Key {
            key,
            url: spec.url.clone(),
        };
        let make = |store: StoreKind| Change::Add {
            name: name.clone(),
            endpoint: NewEndpoint {
                provider,
                auth,
                store,
                pricing: PricingMode::default(),
            },
            record: record.clone(),
            make_default: spec.default,
        };
        let kind = match prepared.picked {
            Picked::Kind(kind) => kind,
            Picked::Auto => StoreKind::Keyring,
        };
        let keys = self.keychain.budgeted(self.budget);
        let home = self.context().home_text();
        match try_change(&prepared.path, prepared.shown, make(kind), &keys) {
            Ok(applied) => {
                let mut notes = prepared.notes;
                notes.extend(self.written_notes(&applied, &prepared.path, prepared.shown));
                Ok(SaveOutcome::Saved(Saved {
                    name: name.to_string(),
                    store: kind.into(),
                    check,
                    notes,
                }))
            }
            Err(failed) => {
                let mut reason = match (&failed.error, &prepared.picked) {
                    (
                        ChangeError::KeychainWrite {
                            error: KeyringError::Unconfirmed { name, .. },
                            ..
                        },
                        _,
                    ) => {
                        self.keychain.left_unconfirmed();
                        unconfirmed_words(name)
                    }
                    (ChangeError::KeychainWrite { error, .. }, Picked::Auto) => {
                        let which = error.which().unwrap_or_else(|| REFUSED_REQUEST.to_owned());
                        if !self.keychain.has_unconfirmed() {
                            return Ok(SaveOutcome::NotSaved(NotSaved {
                                name: name.to_string(),
                                why: NotSavedReason::KeychainFailed(which),
                            }));
                        }
                        after_unconfirmed_words(&which)
                    }
                    (error, _) => change_error(error, home.as_deref()),
                };
                for note in &failed.notes {
                    reason.push_str(&format!(" (note: {})", note_words(note, home.as_deref())));
                }
                Err(reason)
            }
        }
    }

    pub(crate) async fn fetch_list(&self, alias: &str) {
        let Some(folder) = self.models.clone() else {
            return;
        };
        let Ok(mut entries) = self.blocking(|host| host.sources().entries()).await else {
            return;
        };
        entries.retain(|name, _| name.as_str() == alias);
        let due = endpoints::due(&entries, &folder, jiff::Timestamp::now().as_second());
        let all = due.missing.into_iter().chain(due.stale).collect();
        endpoints::fetch_due(endpoints::fetches(all, &folder), endpoints::LIST_WAIT).await;
    }

    pub(crate) fn paths(&self) -> SourcesWatch {
        SourcesWatch {
            env_file: self.env_file.clone(),
            config: self.config.clone(),
            endpoint: self.endpoint.clone(),
            xdg_config_home: self.xdg_config_home.clone(),
            home: self.home.clone(),
            keychain: Arc::clone(&self.keychain),
            set: self.set.clone(),
            models: self.models.clone(),
        }
    }

    fn sources(&self) -> Sources {
        let mut sources = self.paths().sources();
        if let Some(catalog) = &self.catalog {
            sources.off = catalog.providers_off();
        }
        sources
    }

    pub(crate) fn reconnect(&self) -> Result<Reconnected, String> {
        let sources = self.sources();
        let entries = sources.entries()?;
        let marks = sources.marks(&entries);
        let models = self.models.as_deref();
        Ok(view(&sources, &entries, &marks, models))
    }

    async fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce(CliEndpointHost) -> Result<T, String> + Send + 'static,
    ) -> Result<T, HostError> {
        let host = self.clone();
        match tokio::task::spawn_blocking(move || work(host)).await {
            Ok(result) => result.map_err(HostError),
            Err(e) => Err(HostError(format!("the save stopped: {e}"))),
        }
    }

    async fn save_now(
        &self,
        spec: EndpointSpec,
        key: Secret,
        check: bool,
        cancel: watch::Receiver<bool>,
    ) -> Result<SaveOutcome, HostError> {
        if cancelled(&cancel) {
            return Ok(SaveOutcome::Cancelled);
        }
        let provider = check_spec(&spec, &key).map_err(HostError)?;
        let (Some(name), Some(auth)) = (EndpointName::parse(&spec.name), provider.auth(&spec.auth))
        else {
            return Err(HostError(wisp_protocol::bad_account_name()));
        };
        let wanted = spec.clone();
        let parsed = name.clone();
        let prepared = self
            .blocking(move |host| host.prepare(&wanted, &parsed))
            .await?;
        let draft = Draft {
            name,
            provider,
            auth,
            spec,
            key,
        };
        let result = if check {
            if cancelled(&cancel) {
                return Ok(SaveOutcome::Cancelled);
            }
            let target = probe_target(&draft, &prepared.path, prepared.shown);
            let outcome = provider
                .probe(&target, &self.model, CHECK_TIMEOUT, self.models.as_deref())
                .await;
            if cancelled(&cancel) {
                return Ok(SaveOutcome::Cancelled);
            }
            let result = check_result(outcome);
            if result.outcome != CheckKind::Ok {
                return Ok(SaveOutcome::NotSaved(NotSaved {
                    name: draft.name.to_string(),
                    why: NotSavedReason::Check(result),
                }));
            }
            result
        } else {
            CheckResult::skipped()
        };
        if cancelled(&cancel) {
            return Ok(SaveOutcome::Cancelled);
        }
        self.blocking(move |host| host.write(prepared, draft, result))
            .await
    }
}

impl EndpointHost for CliEndpointHost {
    fn cancel_auth(&self) {
        self.auth.pending().retain(|_, pending| {
            if pending.committing {
                true
            } else {
                pending.cancel.send_replace(true);
                false
            }
        });
    }

    fn catalog(&self) -> Option<wisp_core::endpoints::catalog::LiveCatalog> {
        self.catalog.clone()
    }
    fn auth_alias(
        &self,
        command: &wisp_protocol::subscription_auth::AuthCommand,
    ) -> Option<(String, bool)> {
        self.auth.alias(command, self.catalog.as_ref())
    }
    fn auth(
        &self,
        command: wisp_protocol::subscription_auth::AuthCommand,
    ) -> BoxFuture<'_, wisp_protocol::subscription_auth::AuthEvent> {
        Box::pin(crate::auth::session_auth(self, command))
    }

    fn check_logout(
        &self,
        command: wisp_protocol::subscription_auth::AuthCommand,
    ) -> BoxFuture<'_, Option<wisp_protocol::subscription_auth::AuthOutcome>> {
        let host = self.clone();
        async move {
            tokio::task::spawn_blocking(move || crate::auth::check_logout(&host, command))
                .await
                .unwrap_or_else(|_| Some(wisp_core::endpoints::host::unavailable()))
        }
        .boxed()
    }

    fn save(
        &self,
        spec: EndpointSpec,
        key: Secret,
        check: bool,
        cancel: watch::Receiver<bool>,
    ) -> BoxFuture<'_, Result<SaveOutcome, HostError>> {
        async move {
            let alias = spec.name.clone();
            let provider = ProviderKind::parse(&spec.provider);
            let auth = provider.and_then(|provider| provider.auth(&spec.auth));
            let change = self.change_catalog(&alias, false)?;
            let result = self.save_now(spec, key, check, cancel).await?;
            if matches!(result, SaveOutcome::Saved(_))
                && let Some(change) = change
            {
                self.publish_catalog(change, &alias, provider, auth, false)
                    .await;
            }
            Ok(result)
        }
        .boxed()
    }

    fn edit(
        &self,
        name: String,
        edit: EndpointEdit,
        store: Option<Wanted>,
        check: bool,
    ) -> BoxFuture<'_, Result<EditOutcome, HostError>> {
        async move {
            let change = self.change_catalog(&name, false)?;
            let result = self.edit_now(name.clone(), edit, store, check).await?;
            if matches!(result, EditOutcome::Edited(_))
                && let Some(change) = change
            {
                self.publish_catalog(change, &name, None, None, false).await;
            }
            Ok(result)
        }
        .boxed()
    }

    fn remove(
        &self,
        name: String,
        keep_key: bool,
    ) -> BoxFuture<'_, Result<RemoveOutcome, HostError>> {
        async move {
            let change = self.change_catalog(&name, true)?;
            self.remove_for(name, keep_key, None, change).await
        }
        .boxed()
    }

    fn connections(&self) -> BoxFuture<'_, Result<Reconnected, HostError>> {
        async move {
            match &self.catalog {
                Some(catalog) => Ok(catalog.view()),
                None => self.blocking(|host| host.reconnect()).await,
            }
        }
        .boxed()
    }
}

mod change;
#[cfg(test)]
mod tests;
