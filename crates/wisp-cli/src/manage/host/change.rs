use std::path::PathBuf;

use wisp_core::config_file::ConfigText;
use wisp_core::endpoints::host::{
    EditOutcome, Edited, HostError, NotSaved, RemoveOutcome, Removed, check_edit, check_name,
};
use wisp_core::endpoints::{
    AuthKind, CHECK_TIMEOUT, ConfigFile, EndpointConfig, EndpointName, KeyFrom, PathShown,
    ProviderKind,
};
use wisp_core::secrets::{
    Applied, Change, ChangeError, Edit, FileStore, KeyringAccess, KeyringError, Outcome, Secret,
    StoreKind, StoredRecord, keychain_off_for_key, keychain_words,
};
use wisp_protocol::endpoint::{
    CheckKind, CheckResult, ConnectionField, EndpointEdit, NotSavedReason, StoreChoice, UrlEdit,
    VariableEdit,
};

use super::{
    CliEndpointHost, REFUSED_REQUEST, after_unconfirmed_words, check_result, unconfirmed_words,
};

fn already_kept(name: &EndpointName, kind: StoreKind) -> String {
    let place = match kind {
        StoreKind::Keyring => wisp_protocol::IN_KEYCHAIN,
        _ => wisp_core::secrets::CREDENTIALS,
    };
    format!("{name}'s key is already kept in {place}; nothing was changed")
}

pub(super) fn safe_error(error: &ChangeError, home: Option<&str>) -> String {
    match error {
        ChangeError::Keychain {
            name,
            error: KeyringError::Unconfirmed { .. },
            ..
        }
        | ChangeError::KeychainWrite {
            name,
            error: KeyringError::Unconfirmed { .. },
        } => unconfirmed_words(name),
        other => change_error(other, home),
    }
}
use crate::manage::{Draft, change_error, early_store_check, probe_endpoint, try_change};

pub(crate) const STORED_NOT_VARIABLE: &str = "its key is stored, so it has no key or URL variable; remove it and add it again to read a variable instead";
pub(crate) const NO_KEY_VARIABLE_UNSET: &str =
    "it reads its key from a variable, so key_env can't be unset; give another variable name";
pub(crate) const LOST_URL: &str = "its URL was in its lost record; give the URL too";

pub(crate) fn variable_words(name: &EndpointName) -> String {
    format!(
        "{name} names its key as a variable; store it first with wisp account import, or change the variable with key_env"
    )
}

pub(crate) fn not_checked_words(name: &EndpointName) -> String {
    format!("Nothing was read or checked now; wisp doctor --check {name} checks it.")
}

pub(crate) fn left_words(name: &EndpointName, error: &wisp_core::secrets::StoreError) -> String {
    let wisp_core::secrets::StoreError::Keychain(error) = error else {
        return format!(
            "the old file-store record for {name} wasn't deleted: {error}; it still holds the key in {}",
            wisp_core::secrets::CREDENTIALS
        );
    };
    let which = error.which().unwrap_or_else(|| REFUSED_REQUEST.to_owned());
    format!(
        "the keychain {which}, so the old keychain item for {name} wasn't deleted; it still holds the key, so delete that item with your keychain app"
    )
}

struct Target {
    path: PathBuf,
    shown: PathShown,
    config: ConfigFile,
    endpoint: EndpointConfig,
}

pub(super) struct Probe {
    draft: Draft,
    key: String,
}

pub(super) enum Plan {
    Ready(Box<Planned>),
    NotSaved(NotSaved),
}

pub(super) struct Planned {
    name: EndpointName,
    path: PathBuf,
    shown: PathShown,
    edit: Edit,
    changed: Vec<ConnectionField>,
    store: Option<wisp_protocol::endpoint::StoreKind>,
    keyring: bool,
    notes: Vec<String>,
    pub(super) probe: Option<Probe>,
}

fn auth_for(provider: ProviderKind, text: &str) -> Result<AuthKind, String> {
    provider.auth(text).ok_or_else(|| {
        format!(
            "{} takes its key as one of: {}",
            provider.title(),
            provider
                .auths()
                .iter()
                .map(|a| a.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

impl CliEndpointHost {
    fn target(&self, name: &EndpointName) -> Result<Target, String> {
        let (path, shown) = self.context().target(true)?;
        if std::fs::symlink_metadata(&path).is_err() {
            return Err(ChangeError::NoSuchEndpoint(name.clone()).to_string());
        }
        let config = ConfigText::read(&path, shown)
            .and_then(|t| t.current())
            .map_err(|e| e.to_string())?;
        let Some(endpoint) = config.endpoints.get(name).cloned() else {
            return Err(ChangeError::NoSuchEndpoint(name.clone()).to_string());
        };
        Ok(Target {
            path,
            shown,
            config,
            endpoint,
        })
    }

    pub(super) fn plan_edit(
        &self,
        name: &EndpointName,
        edit: &EndpointEdit,
        store: Option<StoreChoice>,
        check: bool,
    ) -> Result<Plan, String> {
        let target = self.target(name)?;
        let provider = match &edit.provider {
            Some(text) => ProviderKind::parse(text)
                .ok_or_else(wisp_core::endpoints::host::unknown_provider)?,
            None => target.endpoint.provider,
        };
        let auth = edit
            .auth
            .as_deref()
            .map(|text| auth_for(provider, text))
            .transpose()?;
        let provider_change = edit.provider.as_ref().map(|_| provider);
        match &target.endpoint.key {
            KeyFrom::Variable(_) => self
                .plan_table(name, target, edit, store, provider_change, auth)
                .map(|planned| Plan::Ready(Box::new(planned))),
            KeyFrom::Store(_) => {
                self.plan_stored(name, target, edit, store, provider_change, auth, check)
            }
        }
    }

    fn plan_table(
        &self,
        name: &EndpointName,
        target: Target,
        edit: &EndpointEdit,
        store: Option<StoreChoice>,
        provider: Option<ProviderKind>,
        auth: Option<AuthKind>,
    ) -> Result<Planned, String> {
        if edit.key.is_some() || store.and_then(StoreKind::chosen).is_some() {
            return Err(variable_words(name));
        }
        let key_env = match &edit.key_env {
            Some(VariableEdit::Set(variable)) => Some(variable.clone()),
            Some(VariableEdit::Unset) => return Err(NO_KEY_VARIABLE_UNSET.to_owned()),
            None => None,
        };
        let (url, url_env) = match (&edit.url, &edit.url_env) {
            (Some(UrlEdit::Set(url)), _) => (Some(Some(url.expose().to_owned())), Some(None)),
            (Some(UrlEdit::ProviderDefault), _) => (Some(None), Some(None)),
            (None, Some(VariableEdit::Set(variable))) => (Some(None), Some(Some(variable.clone()))),
            (None, Some(VariableEdit::Unset)) => (None, Some(None)),
            (None, None) => (None, None),
        };
        let core = Edit {
            provider,
            auth,
            url,
            url_env,
            key_env: key_env.clone(),
            ..Edit::default()
        };
        let variable = key_env.as_ref().or(match &target.endpoint.key {
            KeyFrom::Variable(variable) => Some(variable),
            KeyFrom::Store(_) => None,
        });
        let mut note = not_checked_words(name);
        if let Some(variable) = variable.filter(|_| self.keychain.detected().available()) {
            note.push(' ');
            note.push_str(&crate::manage::unread_on_keychain(variable));
        }
        Ok(Planned {
            name: name.clone(),
            path: target.path,
            shown: target.shown,
            edit: core,
            changed: edit.fields(),
            store: None,
            keyring: false,
            notes: vec![note],
            probe: None,
        })
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the edit's parts are resolved by the caller once and passed as they are"
    )]
    fn plan_stored(
        &self,
        name: &EndpointName,
        target: Target,
        edit: &EndpointEdit,
        store: Option<StoreChoice>,
        provider: Option<ProviderKind>,
        auth: Option<AuthKind>,
        check: bool,
    ) -> Result<Plan, String> {
        if edit.key_env.is_some() || edit.url_env.is_some() {
            return Err(format!("{name}: {STORED_NOT_VARIABLE}"));
        }
        let home = self.context().home_text();
        let home = home.as_deref();
        let from = target.endpoint.store();
        let mut moved = store.and_then(StoreKind::chosen);
        if moved == from {
            if edit.is_empty()
                && let Some(kind) = moved
            {
                return Err(already_kept(name, kind));
            }
            moved = None;
        }
        if moved == Some(StoreKind::Keyring)
            && let Some(KeyringError::Off { origin }) = self.keychain.off_error()
        {
            return Err(keychain_off_for_key(&origin));
        }
        let file = FileStore::beside(&target.path, target.shown);
        if from == Some(StoreKind::File) || moved == Some(StoreKind::File) {
            early_store_check(&file, home)?;
        }
        let key_change = edit.key.is_some();
        let touches = key_change || edit.url.is_some() || provider.is_some() || moved.is_some();
        let keys = self.keychain.budgeted(self.budget);
        let record = match (from, &target.config.store_id) {
            (Some(StoreKind::Keyring), Some(id)) if touches => {
                keys.begin_phase();
                match keys.read(id, name) {
                    Ok(stored) => stored.map(|s| s.record),
                    Err(error) => return Err(keychain_words(name, &error, false)),
                }
            }
            (Some(StoreKind::Keyring), _) => None,
            (_, id) => id
                .as_ref()
                .and_then(|id| file.read_all(id).ok())
                .and_then(|records| records.get(name).map(|s| s.record.clone())),
        };
        let (current_key, record_url) = match &record {
            Some(StoredRecord::Key { key, url }) => (
                Some(key.expose().to_owned()),
                url.as_ref().map(|u| u.expose().to_owned()),
            ),
            None => (None, None),
        };
        let writes_record = key_change
            || matches!(edit.url, Some(UrlEdit::Set(_)))
            || (matches!(edit.url, Some(UrlEdit::ProviderDefault)) && record_url.is_some());
        let probes = check && (key_change || edit.url.is_some() || provider.is_some());
        let needs_key = writes_record || probes || moved.is_some();
        if current_key.is_none() && !key_change && needs_key {
            return Err(change_error(&ChangeError::NoRecord(name.clone()), home));
        }
        if key_change && record.is_none() && target.endpoint.url.is_none() && edit.url.is_none() {
            return Err(format!("{name}: {LOST_URL}"));
        }
        let key = edit
            .key
            .as_ref()
            .map(|key| key.expose().trim().to_owned())
            .or(current_key);
        let (new_record_url, config_url) = match &edit.url {
            None => (record_url.clone(), None),
            Some(UrlEdit::Set(url)) => (
                Some(url.expose().to_owned()),
                target.endpoint.url.as_ref().map(|_| None),
            ),
            Some(UrlEdit::ProviderDefault) => (None, target.endpoint.url.as_ref().map(|_| None)),
        };
        let probe_url = match &edit.url {
            Some(UrlEdit::Set(url)) => Some(url.expose().to_owned()),
            Some(UrlEdit::ProviderDefault) => None,
            None => record_url.clone().or_else(|| target.endpoint.url.clone()),
        };
        let new_record = match (writes_record, &key) {
            (true, Some(key)) => Some(StoredRecord::Key {
                key: Secret::new(key.clone()),
                url: new_record_url.map(Secret::new),
            }),
            (true, None) => return Err(change_error(&ChangeError::NoRecord(name.clone()), home)),
            (false, _) => None,
        };
        let core = Edit {
            provider,
            auth,
            record: new_record,
            url: config_url,
            store: moved,
            ..Edit::default()
        };
        let change = Change::Edit {
            name: name.clone(),
            edit: core.clone(),
        };
        let after = moved.or(from);
        let keyring = after == Some(StoreKind::Keyring);
        keys.begin_phase();
        if let Err(error) = wisp_core::secrets::check(&target.path, target.shown, &change, &keys) {
            return match self.keychain_refusal(&error, keyring, name) {
                Some(not) => Ok(Plan::NotSaved(not)),
                None => Err(safe_error(&error, home)),
            };
        }
        let probe = match (probes, key) {
            (true, Some(key)) => Some(Probe {
                draft: Draft {
                    provider: provider.unwrap_or(target.endpoint.provider),
                    url: probe_url,
                    auth: auth.unwrap_or(target.endpoint.auth),
                    key: None,
                },
                key,
            }),
            (true, None) => return Err(change_error(&ChangeError::NoRecord(name.clone()), home)),
            (false, _) => None,
        };
        let mut changed = edit.fields();
        if moved.is_some() {
            changed.push(ConnectionField::Store);
        }
        Ok(Plan::Ready(Box::new(Planned {
            name: name.clone(),
            path: target.path,
            shown: target.shown,
            edit: core,
            changed,
            store: after.map(Into::into),
            keyring,
            notes: Vec::new(),
            probe,
        })))
    }

    fn keychain_refusal(
        &self,
        error: &ChangeError,
        keyring: bool,
        name: &EndpointName,
    ) -> Option<NotSaved> {
        match error {
            ChangeError::KeychainWrite { error, .. }
                if keyring
                    && !matches!(error, KeyringError::Unconfirmed { .. })
                    && !self.keychain.has_unconfirmed() =>
            {
                let which = error.which().unwrap_or_else(|| REFUSED_REQUEST.to_owned());
                Some(NotSaved {
                    name: name.to_string(),
                    why: NotSavedReason::KeychainFailed(which),
                })
            }
            _ => None,
        }
    }

    pub(super) async fn probe_planned(&self, planned: &Planned) -> Option<CheckResult> {
        let probe = planned.probe.as_ref()?;
        let target = probe_endpoint(&probe.draft, &probe.key, &planned.path, planned.shown);
        let outcome = probe
            .draft
            .provider
            .probe(&target, &self.model, CHECK_TIMEOUT, self.models.as_deref())
            .await;
        Some(check_result(outcome))
    }

    pub(super) fn write_edit(
        &self,
        planned: Planned,
        check: CheckResult,
    ) -> Result<EditOutcome, String> {
        let home = self.context().home_text();
        let keys = self.keychain.budgeted(self.budget);
        let change = Change::Edit {
            name: planned.name.clone(),
            edit: planned.edit,
        };
        match try_change(&planned.path, planned.shown, change, &keys) {
            Ok(applied) => {
                let mut notes = planned.notes;
                notes.extend(self.applied_notes(&applied, &planned.path, planned.shown));
                Ok(EditOutcome::Edited(Edited {
                    name: planned.name.to_string(),
                    changed: planned.changed,
                    store: planned.store,
                    check,
                    notes,
                }))
            }
            Err(failed) => {
                let mut reason = match &failed.error {
                    ChangeError::KeychainWrite {
                        error: KeyringError::Unconfirmed { .. },
                        ..
                    } => {
                        self.keychain.left_unconfirmed();
                        unconfirmed_words(&planned.name)
                    }
                    error @ ChangeError::KeychainWrite { error: inner, .. } if planned.keyring => {
                        if let Some(not) = self.keychain_refusal(error, true, &planned.name) {
                            return Ok(EditOutcome::NotSaved(not));
                        }
                        let which = inner.which().unwrap_or_else(|| REFUSED_REQUEST.to_owned());
                        after_unconfirmed_words(&which)
                    }
                    error => safe_error(error, home.as_deref()),
                };
                for note in &failed.notes {
                    reason.push_str(&format!(
                        " (note: {})",
                        crate::endpoints::note_words(note, home.as_deref())
                    ));
                }
                Err(reason)
            }
        }
    }

    fn applied_notes(
        &self,
        applied: &Applied,
        path: &std::path::Path,
        shown: PathShown,
    ) -> Vec<String> {
        let mut notes = self.written_notes(applied, path, shown);
        for left in &applied.left {
            notes.push(left_words(&left.name, &left.error));
        }
        notes
    }

    pub(super) fn remove_now(
        &self,
        name: &EndpointName,
        keep_key: bool,
        chat: Option<EndpointName>,
    ) -> Result<RemoveOutcome, String> {
        let home = self.context().home_text();
        let home = home.as_deref();
        let target = self.target(name)?;
        let keys = self.keychain.budgeted(self.budget);
        let change = Change::Remove {
            name: name.clone(),
            keep_key,
            chat,
        };
        let files = target
            .config
            .endpoints
            .values()
            .any(|e| e.store() == Some(StoreKind::File));
        if files {
            early_store_check(&FileStore::beside(&target.path, target.shown), home)?;
        }
        let refused = |error: &ChangeError| match error {
            ChangeError::Keychain { error, .. } if !keep_key => {
                Some(RemoveOutcome::NotSaved(NotSaved {
                    name: name.to_string(),
                    why: NotSavedReason::KeychainFailed(
                        error.which().unwrap_or_else(|| REFUSED_REQUEST.to_owned()),
                    ),
                }))
            }
            _ => None,
        };
        keys.begin_phase();
        if let Err(error) = wisp_core::secrets::check(&target.path, target.shown, &change, &keys) {
            return match refused(&error) {
                Some(outcome) => Ok(outcome),
                None => Err(safe_error(&error, home)),
            };
        }
        let applied = match try_change(&target.path, target.shown, change, &keys) {
            Ok(applied) => applied,
            Err(failed) => {
                return match refused(&failed.error) {
                    Some(outcome) => Ok(outcome),
                    None => Err(safe_error(&failed.error, home)),
                };
            }
        };
        let notes = self.applied_notes(&applied, &target.path, target.shown);
        let left = applied.left.iter().any(|left| left.name == *name);
        let (kept_item, default_cleared, new_start) = match applied.outcome {
            Outcome::Removed {
                was_default,
                kept,
                new_start,
                ..
            } => (
                kept.is_some(),
                was_default,
                new_start.map(|n| n.to_string()),
            ),
            _ => (false, false, None),
        };
        Ok(RemoveOutcome::Removed(Removed {
            name: name.to_string(),
            kept_item,
            default_cleared,
            notes,
            left,
            new_start,
        }))
    }

    pub(super) async fn edit_now(
        &self,
        name: String,
        edit: EndpointEdit,
        store: Option<StoreChoice>,
        check: bool,
    ) -> Result<EditOutcome, HostError> {
        let parsed = check_edit(&name, &edit, store).map_err(HostError)?;
        let planned = match self
            .blocking(move |host| host.plan_edit(&parsed, &edit, store, check))
            .await?
        {
            Plan::Ready(planned) => *planned,
            Plan::NotSaved(not) => return Ok(EditOutcome::NotSaved(not)),
        };
        let result = match self.probe_planned(&planned).await {
            Some(result) if result.outcome != CheckKind::Ok => {
                return Ok(EditOutcome::NotSaved(NotSaved {
                    name: planned.name.to_string(),
                    why: NotSavedReason::Check(result),
                }));
            }
            Some(result) => result,
            None => CheckResult::skipped(),
        };
        self.blocking(move |host| host.write_edit(planned, result))
            .await
    }

    pub(super) async fn remove_endpoint(
        &self,
        name: String,
        keep_key: bool,
        chat: Option<EndpointName>,
    ) -> Result<RemoveOutcome, HostError> {
        let parsed = check_name(&name).map_err(HostError)?;
        self.blocking(move |host| host.remove_now(&parsed, keep_key, chat))
            .await
    }
}
