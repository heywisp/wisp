use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::import::and_words;
use super::keyring::{KeyringAccess, KeyringError, account};
use super::{
    DetachNote, Every, FileStore, LockKind, Locks, PathNote, StoreError, StoreId, StoreKind,
    Stored, StoredRecord,
};
use crate::config_file::{
    ConfigText, ConfigWriteError, NewEndpoint, NewTable, canonical_config, lock_folder,
};
use crate::endpoints::{AuthKind, ConfigFile, EndpointName, PathShown, PricingMode, ProviderKind};
use crate::provider::credential::GIVEN_CREDENTIALS;
use wisp_protocol::{CONFIG_FILE, CREDENTIALS, VariableName};

#[derive(Clone, Debug)]
pub enum Change {
    Add {
        name: EndpointName,
        endpoint: NewEndpoint,
        record: StoredRecord,
        make_default: bool,
    },
    Remove {
        name: EndpointName,
        keep_key: bool,
        chat: Option<EndpointName>,
    },
    SetDefault {
        name: EndpointName,
    },
    Edit {
        name: EndpointName,
        edit: Edit,
    },
    AddTable {
        name: EndpointName,
        table: NewTable,
        make_default: bool,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Edit {
    pub make_default: bool,
    pub rename: Option<EndpointName>,
    pub provider: Option<ProviderKind>,
    pub auth: Option<AuthKind>,
    pub pricing: Option<PricingMode>,
    pub record: Option<StoredRecord>,
    pub url: Option<Option<String>>,
    pub key_env: Option<VariableName>,
    pub url_env: Option<Option<VariableName>>,
    pub store: Option<StoreKind>,
}

impl Edit {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Added {
        created: bool,
        default: bool,
    },
    Removed {
        store: Option<StoreKind>,
        deleted: bool,
        was_default: bool,
        kept: Option<String>,
        new_start: Option<EndpointName>,
    },
    LeftoverRemoved,
    DefaultSet,
    Edited {
        name: EndpointName,
    },
}

#[derive(Clone, PartialEq, Eq)]
pub struct Left {
    pub name: EndpointName,
    pub account: String,
    pub error: StoreError,
}

impl fmt::Debug for Left {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Left")
            .field("name", &self.name)
            .field("account", &"<hidden>")
            .field("error", &self.error)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    pub outcome: Outcome,
    pub notes: Vec<PathNote>,
    pub detached: Option<DetachNote>,
    pub left: Vec<Left>,
}

#[derive(Clone, PartialEq, Eq)]
pub enum ChangeError {
    Store(StoreError),
    Config(ConfigWriteError),
    NameTaken(EndpointName),
    NoSuchEndpoint(EndpointName),
    NoRecord(EndpointName),
    CantDetach {
        unreadable: Vec<EndpointName>,
        credentials: PathBuf,
        shown: PathShown,
    },
    CantDetachKeychain {
        unreadable: Vec<EndpointName>,
        error: KeyringError,
    },
    Keychain {
        name: EndpointName,
        error: KeyringError,
        keep_key: bool,
    },
    KeychainWrite {
        name: EndpointName,
        error: KeyringError,
    },
    KeepKeyNotKeychain {
        name: EndpointName,
        store: Option<StoreKind>,
    },
    Interrupted,
}

impl fmt::Debug for ChangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(e) => f.debug_tuple("Store").field(e).finish(),
            Self::Config(e) => f.debug_tuple("Config").field(e).finish(),
            Self::NameTaken(name) => f.debug_tuple("NameTaken").field(name).finish(),
            Self::NoSuchEndpoint(name) => f.debug_tuple("NoSuchEndpoint").field(name).finish(),
            Self::NoRecord(name) => f.debug_tuple("NoRecord").field(name).finish(),
            Self::CantDetach {
                unreadable,
                credentials,
                shown,
            } => f
                .debug_struct("CantDetach")
                .field("unreadable", unreadable)
                .field("credentials", &shown.path(credentials))
                .field("shown", shown)
                .finish(),
            Self::CantDetachKeychain { unreadable, error } => f
                .debug_struct("CantDetachKeychain")
                .field("unreadable", unreadable)
                .field("error", error)
                .finish(),
            Self::Keychain {
                name,
                error,
                keep_key,
            } => f
                .debug_struct("Keychain")
                .field("name", name)
                .field("error", error)
                .field("keep_key", keep_key)
                .finish(),
            Self::KeychainWrite { name, error } => f
                .debug_struct("KeychainWrite")
                .field("name", name)
                .field("error", error)
                .finish(),
            Self::KeepKeyNotKeychain { name, store } => f
                .debug_struct("KeepKeyNotKeychain")
                .field("name", name)
                .field("store", store)
                .finish(),
            Self::Interrupted => f.write_str("Interrupted"),
        }
    }
}

pub fn cant_detach_words(unreadable: &[EndpointName], file: &str) -> String {
    let (keys, they) = if unreadable.len() == 1 {
        ("key", "it isn't")
    } else {
        ("keys", "they aren't")
    };
    let mut text = format!(
        "nothing was changed: this {CONFIG_FILE} must get its own copy of its keys first, and the {keys} of {} can't be read ({they} in {file}). If this {CONFIG_FILE} was copied from another folder, copy that folder's {CREDENTIALS} here first",
        and_words(unreadable)
    );
    if let [only] = unreadable {
        text.push_str(&format!(
            "; or run wisp account remove {only} to give it up, or wisp account edit {only} --replace-key to give it a new key"
        ));
    }
    text.push('.');
    text
}

pub fn cant_detach_keychain_words(unreadable: &[EndpointName], error: &KeyringError) -> String {
    let keys = if unreadable.len() == 1 { "key" } else { "keys" };
    let names = and_words(unreadable);
    let (reason, next) = match (error, error.which()) {
        (KeyringError::Off { origin }, _) => (
            format!("the keychain is off: {origin}"),
            "Run this with --keychain on",
        ),
        (_, Some(which)) => (
            format!("the keychain {which}"),
            "Unlock the keychain and try again",
        ),
        (error, None) => (error.to_string(), "Fix or remove that keychain item first"),
    };
    format!(
        "nothing was changed: this {CONFIG_FILE} must get its own copy of its keys first, and the {keys} of {names} can't be read ({reason}). {next}; or, for a config copied from another folder, copy that folder's {CREDENTIALS} here first."
    )
}

pub fn keychain_off_for_flag(origin: &str) -> String {
    format!(
        "the keychain is off ({origin}), so --store keyring can't be used; run this with --keychain on"
    )
}

pub fn keychain_off_for_key(origin: &str) -> String {
    format!(
        "the keychain is off ({origin}), so the key can't be kept there; run wisp with --keychain on"
    )
}

pub fn keychain_words(name: &EndpointName, error: &KeyringError, keep_key: bool) -> String {
    let keep = if keep_key {
        ", or pass --keep-key to remove only the table"
    } else {
        ""
    };
    match (error, error.which()) {
        (KeyringError::Off { origin }, _) => format!(
            "{name}'s key is in the keychain, which is off ({origin}); run this with --keychain on{keep}"
        ),
        (_, Some(which)) => format!(
            "nothing was changed: {name}'s key is in the keychain, which {which}; unlock it and try again{keep}"
        ),
        (error, None) => {
            format!("nothing was changed: {error}; Wisp never rewrites or deletes it{keep}")
        }
    }
}

impl fmt::Display for ChangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(e) => write!(f, "{e}"),
            Self::Config(e) => write!(f, "{e}"),
            Self::NameTaken(name) => write!(
                f,
                "an account named \"{name}\" already exists; nothing was changed"
            ),
            Self::NoSuchEndpoint(name) => {
                write!(f, "there is no account \"{name}\"; nothing was changed")
            }
            Self::NoRecord(name) => write!(
                f,
                "nothing was changed: {name}'s key can't be read (it isn't in this folder's {CREDENTIALS}); give it a new key with wisp account edit {name} --replace-key"
            ),
            Self::CantDetach {
                unreadable, shown, ..
            } => {
                let file = match shown {
                    PathShown::Default => {
                        concat!("its folder's ", wisp_protocol::credentials_file!())
                    }
                    PathShown::Given => GIVEN_CREDENTIALS,
                };
                f.write_str(&cant_detach_words(unreadable, file))
            }
            Self::CantDetachKeychain { unreadable, error } => {
                f.write_str(&cant_detach_keychain_words(unreadable, error))
            }
            Self::Keychain {
                name,
                error,
                keep_key,
            } => f.write_str(&keychain_words(name, error, *keep_key)),
            Self::KeychainWrite { error, .. } => match error {
                KeyringError::Off { origin } => f.write_str(&keychain_off_for_flag(origin)),
                error @ KeyringError::Unconfirmed { .. } => {
                    write!(f, "{error}; nothing else was changed")
                }
                error => write!(f, "{error}, so nothing was saved"),
            },
            Self::KeepKeyNotKeychain { name, store } => match store {
                Some(StoreKind::File) => write!(
                    f,
                    "--keep-key is for keys in the keychain; {name}'s key is in {CREDENTIALS}"
                ),
                _ => write!(
                    f,
                    "--keep-key is for keys in the keychain; {name}'s key is in a variable, which remove never touches"
                ),
            },
            Self::Interrupted => f.write_str("the change was interrupted"),
        }
    }
}

impl std::error::Error for ChangeError {}

impl From<StoreError> for ChangeError {
    fn from(e: StoreError) -> Self {
        Self::Store(e)
    }
}

impl From<ConfigWriteError> for ChangeError {
    fn from(e: ConfigWriteError) -> Self {
        Self::Config(e)
    }
}

pub fn apply(
    config_path: &Path,
    shown: PathShown,
    change: Change,
    keys: &dyn KeyringAccess,
    wait: Duration,
) -> Result<Applied, ChangeError> {
    apply_noting(config_path, shown, change, keys, wait).map_err(|f| f.error)
}

struct Plan<'a> {
    store: FileStore,
    locks: Option<Locks>,
    text: ConfigText,
    config: ConfigFile,
    this: Option<PathBuf>,
    records: BTreeMap<EndpointName, Stored>,
    items: BTreeMap<EndpointName, Stored>,
    unreadable: BTreeMap<EndpointName, KeyringError>,
    files: Vec<EndpointName>,
    keyrings: Vec<EndpointName>,
    keys: &'a dyn KeyringAccess,
    between: &'a dyn Fn() -> Result<(), ChangeError>,
}

fn read_every(store: &FileStore) -> Result<Every, ChangeError> {
    match store.read_every() {
        Ok(every) => Ok(every),
        Err(e @ (StoreError::Linked(_) | StoreError::Unreadable(_))) => Err(e.into()),
        Err(_) => Err(StoreError::Refused.into()),
    }
}

struct EditShape {
    target: EndpointName,
    renamed: bool,
    from: Option<StoreKind>,
    to: Option<StoreKind>,
    record: Option<StoredRecord>,
    detach: bool,
}

impl Plan<'_> {
    fn locks(&self) -> Result<&Locks, ChangeError> {
        self.locks
            .as_ref()
            .ok_or(ChangeError::Store(StoreError::NotLocked))
    }

    fn lock_notes(&self) -> Vec<PathNote> {
        self.locks
            .as_ref()
            .map(|l| l.notes().to_vec())
            .unwrap_or_default()
    }

    fn stored_names(&self) -> Vec<EndpointName> {
        let mut names: Vec<EndpointName> =
            self.files.iter().chain(&self.keyrings).cloned().collect();
        names.sort();
        names
    }

    fn kind_of(&self, name: &EndpointName) -> Option<StoreKind> {
        self.config.endpoints.get(name).and_then(|e| e.store())
    }

    fn record(&self, kind: StoreKind, name: &EndpointName) -> Option<&Stored> {
        match kind {
            StoreKind::File => self.records.get(name),
            StoreKind::Keyring => self.items.get(name),
        }
    }

    fn current(&self, name: &EndpointName) -> Option<&Stored> {
        self.kind_of(name).and_then(|kind| self.record(kind, name))
    }

    fn validate(&self, change: &Change) -> Result<(), ChangeError> {
        match change {
            Change::Add { name, endpoint, .. } => {
                if name.is_env() || self.config.endpoints.contains_key(name) {
                    return Err(ChangeError::NameTaken(name.clone()));
                }
                self.keyring_usable(endpoint.store, name)?;
                if self.needs_detach(&self.stored_names(), Some((endpoint.store, name))) {
                    self.refuse_detach(None)?;
                }
            }
            Change::Remove { name, keep_key, .. } => {
                if !self.config.endpoints.contains_key(name) {
                    return self.leftover(name).map(|_| ());
                }
                self.remove_shape(name, *keep_key)?;
            }
            Change::SetDefault { name } => {
                if !name.names_keys_file() && !self.config.endpoints.contains_key(name) {
                    return Err(ChangeError::NoSuchEndpoint(name.clone()));
                }
                if self.needs_detach(&self.stored_names(), None) {
                    self.refuse_detach(None)?;
                }
            }
            Change::Edit { name, edit } => {
                self.edit_shape(name, edit)?;
            }
            Change::AddTable { name, .. } => {
                if name.is_env() || self.config.endpoints.contains_key(name) {
                    return Err(ChangeError::NameTaken(name.clone()));
                }
                if self.needs_detach(&self.stored_names(), None) {
                    self.refuse_detach(None)?;
                }
            }
        }
        Ok(())
    }

    fn keyring_usable(&self, kind: StoreKind, name: &EndpointName) -> Result<(), ChangeError> {
        match (kind, self.keys.fixed()) {
            (StoreKind::Keyring, Some(error)) => Err(ChangeError::KeychainWrite {
                name: name.clone(),
                error,
            }),
            _ => Ok(()),
        }
    }

    fn leftover(&self, name: &EndpointName) -> Result<StoreKind, ChangeError> {
        if self.config.store_id.is_none() {
            return Err(ChangeError::NoSuchEndpoint(name.clone()));
        }
        if self.records.get(name).is_some_and(|s| self.owned(s)) {
            return Ok(StoreKind::File);
        }
        if self.items.get(name).is_some_and(|s| self.owned(s)) {
            return Ok(StoreKind::Keyring);
        }
        match self.unreadable.get(name) {
            Some(error) if !error.is_off() && *error != KeyringError::Unsupported => {
                Err(ChangeError::Keychain {
                    name: name.clone(),
                    error: error.clone(),
                    keep_key: false,
                })
            }
            _ => Err(ChangeError::NoSuchEndpoint(name.clone())),
        }
    }

    fn remove_shape(&self, name: &EndpointName, keep_key: bool) -> Result<bool, ChangeError> {
        let kind = self.kind_of(name);
        if keep_key && kind != Some(StoreKind::Keyring) {
            return Err(ChangeError::KeepKeyNotKeychain {
                name: name.clone(),
                store: kind,
            });
        }
        let keep: Vec<EndpointName> = self
            .stored_names()
            .into_iter()
            .filter(|n| n != name)
            .collect();
        let detach = self.needs_detach(&keep, kind.map(|k| (k, name)));
        if detach {
            self.refuse_detach(Some(name))?;
        }
        if kind == Some(StoreKind::Keyring)
            && !keep_key
            && let Some(error) = self.unreadable.get(name)
        {
            return Err(ChangeError::Keychain {
                name: name.clone(),
                error: error.clone(),
                keep_key: true,
            });
        }
        Ok(detach)
    }

    fn edit_shape(&self, name: &EndpointName, edit: &Edit) -> Result<EditShape, ChangeError> {
        let Some(endpoint) = self.config.endpoints.get(name) else {
            return Err(ChangeError::NoSuchEndpoint(name.clone()));
        };
        if let Some(new) = &edit.rename
            && new != name
            && (new.is_env() || self.config.endpoints.contains_key(new))
        {
            return Err(ChangeError::NameTaken(new.clone()));
        }
        let from = endpoint.store();
        let to = edit.store.or(from);
        let target = edit.rename.clone().unwrap_or_else(|| name.clone());
        let renamed = target != *name;
        let moved = from.is_some() && to != from;
        let carries = (renamed || moved) && from.is_some();
        if let Some(StoreKind::Keyring) = from
            && let Some(error) = self.unreadable.get(name)
            && (edit.record.is_some() || carries || edit.provider.is_some())
        {
            return Err(ChangeError::Keychain {
                name: name.clone(),
                error: error.clone(),
                keep_key: false,
            });
        }
        let record = match (&edit.record, carries) {
            (Some(record), _) => Some(record.clone()),
            (None, true) => match self.current(name) {
                Some(stored) => Some(stored.record.clone()),
                None => return Err(ChangeError::NoRecord(name.clone())),
            },
            (None, false) => None,
        };
        if record.is_some()
            && let Some(kind) = to
        {
            self.keyring_usable(kind, name)?;
        }
        if moved
            && from == Some(StoreKind::Keyring)
            && let Some(error) = self.keys.fixed()
        {
            return Err(ChangeError::Keychain {
                name: name.clone(),
                error,
                keep_key: false,
            });
        }
        let replacing = record.is_some() && from == to && !renamed && self.current(name).is_some();
        let others: Vec<EndpointName> = if replacing {
            let keep = self.replacement_copies(name);
            self.check_copies(&keep)?;
            keep
        } else {
            self.stored_names()
                .into_iter()
                .filter(|n| record.is_none() || n != name)
                .collect()
        };
        let foreign = self.current(name).is_some_and(|s| !self.owned(s));
        let landing = match (&record, to) {
            (Some(_), Some(kind)) => Some((kind, &target)),
            _ => None,
        };
        let detach = self.needs_detach(&others, landing) || (record.is_some() && foreign);
        if detach && !replacing {
            let excused = edit.record.as_ref().map(|_| name);
            self.refuse_detach(excused)?;
        }
        Ok(EditShape {
            target,
            renamed,
            from,
            to,
            record,
            detach,
        })
    }

    fn owned(&self, stored: &Stored) -> bool {
        self.this.is_some() && stored.owner == self.this
    }

    fn detach_from(&self, names: &[EndpointName]) -> Option<DetachNote> {
        let mut from: Vec<Option<PathBuf>> = names
            .iter()
            .filter_map(|n| self.current(n))
            .filter(|s| !self.owned(s))
            .map(|s| s.owner.clone())
            .collect();
        from.dedup();
        (!from.is_empty()).then_some(DetachNote { from })
    }

    fn needs_detach(
        &self,
        names: &[EndpointName],
        target: Option<(StoreKind, &EndpointName)>,
    ) -> bool {
        let named = names.iter().any(|n| match self.kind_of(n) {
            Some(StoreKind::Keyring) => self.items.get(n).is_some_and(|s| !self.owned(s)),
            _ => self.current(n).is_none_or(|s| !self.owned(s)),
        });
        let taken = target
            .and_then(|(kind, n)| self.record(kind, n))
            .is_some_and(|s| !self.owned(s));
        named || taken
    }

    fn replacement_copies(&self, replacing: &EndpointName) -> Vec<EndpointName> {
        self.stored_names()
            .into_iter()
            .filter(|name| name != replacing)
            .filter(|name| self.current(name).is_some() || self.unreadable.contains_key(name))
            .collect()
    }

    fn check_copies(&self, keep: &[EndpointName]) -> Result<(), ChangeError> {
        for name in keep {
            if let Some(error) = self.unreadable.get(name) {
                return Err(ChangeError::Keychain {
                    name: name.clone(),
                    error: error.clone(),
                    keep_key: false,
                });
            }
        }
        Ok(())
    }

    fn missing(&self) -> Vec<EndpointName> {
        self.files
            .iter()
            .filter(|n| !self.records.contains_key(*n))
            .cloned()
            .collect()
    }

    fn refuse_detach(&self, excused: Option<&EndpointName>) -> Result<(), ChangeError> {
        let missing = self.missing();
        if let Some(error) = self.unreadable.values().next() {
            let mut unreadable: Vec<EndpointName> = missing
                .into_iter()
                .chain(self.unreadable.keys().cloned())
                .collect();
            unreadable.sort();
            unreadable.dedup();
            return Err(ChangeError::CantDetachKeychain {
                unreadable,
                error: error.clone(),
            });
        }
        let only_excused =
            matches!((missing.as_slice(), excused), ([one], Some(name)) if one == name);
        if missing.is_empty() || only_excused {
            return Ok(());
        }
        Err(ChangeError::CantDetach {
            unreadable: missing,
            credentials: self.store.path().to_owned(),
            shown: self.store.shown(),
        })
    }

    fn write_store(
        &self,
        notes: &mut Vec<PathNote>,
        apply: impl FnOnce(&mut Every),
    ) -> Result<(), ChangeError> {
        let written = self.store.change(self.locks()?, apply)?;
        notes.extend(written.notes);
        Ok(())
    }

    fn write_config(&self, notes: &mut Vec<PathNote>) -> Result<(), ChangeError> {
        match self.text.write() {
            Ok(written) => notes.extend(written),
            Err(error) => {
                if std::fs::read_to_string(self.text.path()).ok().as_deref()
                    != Some(self.text.text().as_str())
                {
                    return Err(error.into());
                }
            }
        }
        Ok(())
    }

    fn put(
        &self,
        notes: &mut Vec<PathNote>,
        kind: StoreKind,
        id: &StoreId,
        name: &EndpointName,
        stored: Stored,
    ) -> Result<(), ChangeError> {
        match kind {
            StoreKind::File => {
                let (id, name) = (id.clone(), name.clone());
                self.write_store(notes, move |every| {
                    every.entry(id).or_default().insert(name, stored);
                })
            }
            StoreKind::Keyring => {
                self.keys
                    .write(id, name, &stored)
                    .map_err(|error| ChangeError::KeychainWrite {
                        name: name.clone(),
                        error,
                    })
            }
        }
    }

    fn delete(
        &self,
        notes: &mut Vec<PathNote>,
        left: &mut Vec<Left>,
        kind: StoreKind,
        id: &StoreId,
        name: &EndpointName,
    ) -> Result<bool, ChangeError> {
        match kind {
            StoreKind::File => match self.store.remove(self.locks()?, id, name) {
                Ok(written) => {
                    notes.extend(written.notes);
                    Ok(true)
                }
                Err(error) => {
                    left.push(Left {
                        name: name.clone(),
                        account: account(id, name),
                        error,
                    });
                    Ok(false)
                }
            },
            StoreKind::Keyring => match self.keys.delete(id, name) {
                Ok(deleted) => Ok(deleted),
                Err(error) => {
                    left.push(Left {
                        name: name.clone(),
                        account: account(id, name),
                        error: StoreError::Keychain(error),
                    });
                    Ok(false)
                }
            },
        }
    }

    fn detach_copy(
        &self,
        notes: &mut Vec<PathNote>,
        keep: &[EndpointName],
        extra: Option<(StoreKind, EndpointName, Stored)>,
    ) -> Result<StoreId, ChangeError> {
        let id = StoreId::new_random();
        let owner = self.this.clone();
        let mine = |stored: &Stored| Stored {
            record: stored.record.clone(),
            owner: owner.clone(),
        };
        for name in keep {
            if self.kind_of(name) == Some(StoreKind::Keyring)
                && let Some(stored) = self.items.get(name)
            {
                self.put(notes, StoreKind::Keyring, &id, name, mine(stored))?;
            }
        }
        if let Some((StoreKind::Keyring, name, stored)) = &extra {
            self.put(notes, StoreKind::Keyring, &id, name, stored.clone())?;
        }
        let mut files: Vec<(EndpointName, Stored)> = keep
            .iter()
            .filter(|n| self.kind_of(n) == Some(StoreKind::File))
            .filter_map(|n| self.records.get(n).map(|s| (n.clone(), mine(s))))
            .collect();
        if let Some((StoreKind::File, name, stored)) = extra {
            files.push((name, stored));
        }
        if !files.is_empty() {
            let target = id.clone();
            self.write_store(notes, move |every| {
                let store = every.entry(target).or_default();
                for (name, stored) in files {
                    store.insert(name, stored);
                }
            })?;
        }
        Ok(id)
    }

    fn add(
        mut self,
        name: EndpointName,
        endpoint: NewEndpoint,
        record: StoredRecord,
        make_default: bool,
    ) -> Result<Applied, ChangeError> {
        if name.is_env() || self.config.endpoints.contains_key(&name) {
            return Err(ChangeError::NameTaken(name));
        }
        let kind = endpoint.store;
        self.keyring_usable(kind, &name)?;
        let detach = self.needs_detach(&self.stored_names(), Some((kind, &name)));
        if detach {
            self.refuse_detach(None)?;
        }
        let mut notes = self.lock_notes();
        let stored = Stored {
            record,
            owner: self.this.clone(),
        };
        let ours = self.config.store_id.clone();
        let (id, detached) = if detach {
            let keep = self.stored_names();
            let note = self.detach_from(&keep);
            let id = self.detach_copy(&mut notes, &keep, Some((kind, name.clone(), stored)))?;
            (id, note)
        } else {
            let id = ours.clone().unwrap_or_else(StoreId::new_random);
            self.put(&mut notes, kind, &id, &name, stored)?;
            (id, None)
        };
        (self.between)()?;
        if ours.as_ref() != Some(&id) {
            self.text.set_store_id(&id);
        }
        self.text.add_endpoint(&name, &endpoint);
        if make_default {
            self.text.set_default(Some(&name));
        }
        let created = !self.text.existed();
        self.write_config(&mut notes)?;
        Ok(Applied {
            outcome: Outcome::Added {
                created,
                default: make_default,
            },
            notes,
            detached,
            left: Vec::new(),
        })
    }

    fn remove(
        mut self,
        name: EndpointName,
        keep_key: bool,
        chat: Option<EndpointName>,
    ) -> Result<Applied, ChangeError> {
        let Some(endpoint) = self.config.endpoints.get(&name) else {
            return self.remove_leftover(name);
        };
        let store = endpoint.store();
        let was_default = self.config.default.as_ref() == Some(&name);
        let detach = self.remove_shape(&name, keep_key)?;
        let mut notes = self.lock_notes();
        let keep: Vec<EndpointName> = self
            .stored_names()
            .into_iter()
            .filter(|n| *n != name)
            .collect();
        let mut detached = None;
        let ours = self.config.store_id.clone();
        if detach {
            detached = self.detach_from(&keep);
            let id = self.detach_copy(&mut notes, &keep, None)?;
            (self.between)()?;
            self.text.set_store_id(&id);
        }
        let new_start = crate::endpoints::goes::start_in(
            &self.config,
            std::slice::from_ref(&name),
            chat.as_ref(),
        );
        self.text.remove_endpoint(&name);
        if was_default {
            self.text.set_default(new_start.as_ref());
        }
        self.write_config(&mut notes)?;
        if !detach {
            (self.between)()?;
        }
        let owned = self.current(&name).is_some_and(|s| self.owned(s));
        let mut deleted = false;
        let mut left = Vec::new();
        let kept = match (&ours, keep_key) {
            (Some(id), true) => Some(account(id, &name)),
            _ => None,
        };
        if let (Some(kind), true, false, Some(id)) = (store, owned, keep_key, &ours) {
            deleted = self.delete(&mut notes, &mut left, kind, id, &name)?;
        }
        Ok(Applied {
            outcome: Outcome::Removed {
                store,
                deleted,
                was_default,
                kept,
                new_start,
            },
            notes,
            detached,
            left,
        })
    }

    fn remove_leftover(self, name: EndpointName) -> Result<Applied, ChangeError> {
        let kind = self.leftover(&name)?;
        let Some(id) = self.config.store_id.clone() else {
            return Err(ChangeError::NoSuchEndpoint(name));
        };
        let mut notes = self.lock_notes();
        let mut left = Vec::new();
        self.delete(&mut notes, &mut left, kind, &id, &name)?;
        if let Some(gone) = left.pop() {
            return Err(match gone.error {
                StoreError::Keychain(error) => ChangeError::Keychain {
                    name,
                    error,
                    keep_key: false,
                },
                error => ChangeError::Store(error),
            });
        }
        Ok(Applied {
            outcome: Outcome::LeftoverRemoved,
            notes,
            detached: None,
            left,
        })
    }

    fn detach_for_write(
        &mut self,
        notes: &mut Vec<PathNote>,
    ) -> Result<Option<DetachNote>, ChangeError> {
        if !self.needs_detach(&self.stored_names(), None) {
            return Ok(None);
        }
        self.refuse_detach(None)?;
        let keep = self.stored_names();
        let detached = self.detach_from(&keep);
        let id = self.detach_copy(notes, &keep, None)?;
        (self.between)()?;
        self.text.set_store_id(&id);
        Ok(detached)
    }

    fn set_default(mut self, name: EndpointName) -> Result<Applied, ChangeError> {
        if !name.names_keys_file() && !self.config.endpoints.contains_key(&name) {
            return Err(ChangeError::NoSuchEndpoint(name));
        }
        let mut notes = self.lock_notes();
        let detached = self.detach_for_write(&mut notes)?;
        self.text.set_default(Some(&name));
        self.write_config(&mut notes)?;
        Ok(Applied {
            outcome: Outcome::DefaultSet,
            notes,
            detached,
            left: Vec::new(),
        })
    }

    fn add_table(
        mut self,
        name: EndpointName,
        table: NewTable,
        make_default: bool,
    ) -> Result<Applied, ChangeError> {
        if name.is_env() || self.config.endpoints.contains_key(&name) {
            return Err(ChangeError::NameTaken(name));
        }
        let mut notes = self.lock_notes();
        let detached = self.detach_for_write(&mut notes)?;
        self.text.add_table(&name, &table);
        if make_default {
            self.text.set_default(Some(&name));
        }
        let created = !self.text.existed();
        self.write_config(&mut notes)?;
        Ok(Applied {
            outcome: Outcome::Added {
                created,
                default: make_default,
            },
            notes,
            detached,
            left: Vec::new(),
        })
    }

    fn edit_text(&mut self, name: &EndpointName, target: &EndpointName, edit: &Edit) {
        if edit.make_default {
            self.text.set_default(Some(target));
        }
        if let Some(provider) = edit.provider {
            let text = (provider != ProviderKind::default()).then(|| provider.as_str());
            self.text.set_field(name, "provider", text);
        }
        if let Some(auth) = edit.auth {
            self.text.set_field(name, "auth", Some(auth.as_str()));
        }
        if let Some(pricing) = edit.pricing {
            let text = (pricing == PricingMode::None).then_some(PricingMode::None.as_str());
            self.text.set_field(name, "pricing", text);
        }
        if let Some(var) = &edit.key_env {
            self.text.set_field(name, "key_env", Some(var.raw()));
        }
        if let Some(var) = &edit.url_env {
            match var {
                Some(var) => {
                    self.text
                        .replace_fields(name, &["url"], "url_env", var.raw());
                }
                None => {
                    self.text.set_field(name, "url_env", None);
                }
            }
        }
        if let Some(url) = &edit.url {
            match url {
                Some(url) => {
                    self.text.replace_fields(name, &["url_env"], "url", url);
                }
                None => {
                    self.text.set_field(name, "url", None);
                }
            }
        }
        if let Some(store) = edit.store {
            self.text
                .replace_fields(name, &["key_env", "url_env"], "key_store", store.as_str());
        }
        if target != name {
            self.text.rename_endpoint(name, target);
            if self.config.default.as_ref() == Some(name) {
                self.text.set_default(Some(target));
            }
        }
    }

    fn edit(mut self, name: EndpointName, edit: Edit) -> Result<Applied, ChangeError> {
        let mut shape = self.edit_shape(&name, &edit)?;
        let mut profile_move = None;
        let replacing = shape.record.is_some()
            && shape.from == shape.to
            && !shape.renamed
            && self.current(&name).is_some();
        if shape.from != shape.to
            && edit.record.is_none()
            && let (Some(store), Some(stored), Some(config)) =
                (shape.to, self.current(&name), self.this.as_deref())
        {
            let StoredRecord::Key { key, .. } = &stored.record;
            if key.expose().starts_with("oauth:")
                && let Some(kind) = self
                    .config
                    .endpoints
                    .get(&name)
                    .and_then(|e| e.auth.subscription())
            {
                let staged =
                    crate::oauth::stage_move(config, key.expose(), kind, store).map_err(|_| {
                        ChangeError::Store(StoreError::Unreadable(std::io::ErrorKind::Other))
                    })?;
                shape.record = Some(StoredRecord::Key {
                    key: super::Secret::new(staged.handle()),
                    url: None,
                });
                profile_move = Some(staged);
            }
        }
        let mut notes = self.lock_notes();
        let ours = self.config.store_id.clone();
        let owner = self.this.clone();
        let mut detached = None;
        let mut id = ours.clone();
        let landing = shape.to.unwrap_or(StoreKind::File);
        if shape.detach || replacing {
            let keep = if replacing {
                self.replacement_copies(&name)
            } else {
                self.stored_names()
                    .into_iter()
                    .filter(|n| shape.record.is_none() || *n != name)
                    .collect()
            };
            detached = self.detach_from(&keep);
            let extra = shape.record.clone().map(|record| {
                (
                    landing,
                    shape.target.clone(),
                    Stored {
                        record,
                        owner: owner.clone(),
                    },
                )
            });
            id = Some(self.detach_copy(&mut notes, &keep, extra)?);
            (self.between)()?;
        } else if let Some(record) = shape.record.clone() {
            let written = ours.clone().unwrap_or_else(StoreId::new_random);
            let stored = Stored {
                record,
                owner: owner.clone(),
            };
            self.put(&mut notes, landing, &written, &shape.target, stored)?;
            id = Some(written);
            (self.between)()?;
        }
        if let Some(id) = &id
            && ours.as_ref() != Some(id)
        {
            self.text.set_store_id(id);
        }
        self.edit_text(&name, &shape.target, &edit);
        self.write_config(&mut notes)?;
        if let Some(staged) = profile_move {
            staged.commit();
        }
        let owned = self.current(&name).is_some_and(|s| self.owned(s));
        let moved = shape.from.is_some() && shape.to != shape.from;
        let mut left = Vec::new();
        if replacing
            && !shape.detach
            && let Some(old_id) = &ours
        {
            for old_name in self.stored_names() {
                if self.current(&old_name).is_some_and(|s| self.owned(s))
                    && let Some(kind) = self.kind_of(&old_name)
                {
                    self.delete(&mut notes, &mut left, kind, old_id, &old_name)?;
                }
            }
        }
        if (shape.renamed || moved)
            && !shape.detach
            && owned
            && let (Some(from), Some(id)) = (shape.from, &ours)
        {
            self.delete(&mut notes, &mut left, from, id, &name)?;
        }
        Ok(Applied {
            outcome: Outcome::Edited { name: shape.target },
            notes,
            detached,
            left,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failed {
    pub error: ChangeError,
    pub notes: Vec<PathNote>,
}

impl From<ChangeError> for Failed {
    fn from(error: ChangeError) -> Self {
        Self {
            error,
            notes: Vec::new(),
        }
    }
}

pub fn check(
    config_path: &Path,
    shown: PathShown,
    change: &Change,
    keys: &dyn KeyringAccess,
) -> Result<(), ChangeError> {
    unwritable_target(config_path, shown)?;
    let never = || Ok(());
    let plan = plan(config_path, shown, change, None, keys, &never)?;
    plan.validate(change)
}

pub fn apply_noting(
    config_path: &Path,
    shown: PathShown,
    change: Change,
    keys: &dyn KeyringAccess,
    wait: Duration,
) -> Result<Applied, Failed> {
    apply_with_notes(config_path, shown, change, keys, wait, &|| Ok(()), &|_| {
        Ok(())
    })
}

#[cfg(test)]
pub(crate) fn apply_with(
    config_path: &Path,
    shown: PathShown,
    change: Change,
    keys: &dyn KeyringAccess,
    wait: Duration,
    between: &dyn Fn() -> Result<(), ChangeError>,
) -> Result<Applied, ChangeError> {
    apply_with_notes(config_path, shown, change, keys, wait, between, &|_| Ok(()))
        .map_err(|f| f.error)
}

fn unwritable_target(config_path: &Path, shown: PathShown) -> Result<(), ChangeError> {
    crate::config_file::unwritable_target(config_path, shown).map_err(ChangeError::Config)
}

pub(crate) fn apply_checked(
    config_path: &Path,
    shown: PathShown,
    change: Change,
    keys: &dyn KeyringAccess,
    validate: &dyn Fn(&ConfigFile) -> Result<(), ChangeError>,
) -> Result<Applied, ChangeError> {
    apply_with_notes(
        config_path,
        shown,
        change,
        keys,
        super::LOCK_WAIT,
        &|| Ok(()),
        validate,
    )
    .map_err(|failure| failure.error)
}

fn apply_with_notes(
    config_path: &Path,
    shown: PathShown,
    change: Change,
    keys: &dyn KeyringAccess,
    wait: Duration,
    between: &dyn Fn() -> Result<(), ChangeError>,
    validate: &dyn Fn(&ConfigFile) -> Result<(), ChangeError>,
) -> Result<Applied, Failed> {
    unwritable_target(config_path, shown)?;
    let store = FileStore::beside(config_path, shown);
    let locks = super::lock_noting(
        &[
            (store.folder().as_path(), LockKind::Credentials),
            (lock_folder(config_path).as_path(), LockKind::Config),
        ],
        shown,
        wait,
    )
    .map_err(|(error, notes)| Failed {
        error: error.into(),
        notes,
    })?;
    let notes = locks.notes().to_vec();
    let failed = |error| Failed {
        error,
        notes: notes.clone(),
    };
    let plan = plan(config_path, shown, &change, Some(locks), keys, between).map_err(failed)?;
    validate(&plan.config).map_err(failed)?;
    plan.validate(&change).map_err(failed)?;
    match change {
        Change::Add {
            name,
            endpoint,
            record,
            make_default,
        } => plan.add(name, endpoint, record, make_default),
        Change::Remove {
            name,
            keep_key,
            chat,
        } => plan.remove(name, keep_key, chat),
        Change::SetDefault { name } => plan.set_default(name),
        Change::Edit { name, edit } => plan.edit(name, edit),
        Change::AddTable {
            name,
            table,
            make_default,
        } => plan.add_table(name, table, make_default),
    }
    .map_err(failed)
}

fn keyring_reads(config: &ConfigFile, change: &Change) -> Vec<EndpointName> {
    let mut names: Vec<EndpointName> = config
        .endpoints
        .iter()
        .filter(|(_, e)| e.store() == Some(StoreKind::Keyring))
        .map(|(n, _)| n.clone())
        .collect();
    match change {
        Change::Add { name, endpoint, .. } if endpoint.store == StoreKind::Keyring => {
            names.push(name.clone());
        }
        Change::Remove { name, .. } if !config.endpoints.contains_key(name) => {
            names.push(name.clone());
        }
        Change::Edit { name, edit } => {
            let target = edit.rename.clone().unwrap_or_else(|| name.clone());
            let lands = edit
                .store
                .or_else(|| config.endpoints.get(name).and_then(|e| e.store()));
            if lands == Some(StoreKind::Keyring) {
                names.push(target);
            }
        }
        _ => {}
    }
    names.sort();
    names.dedup();
    names
}

fn plan<'a>(
    config_path: &Path,
    shown: PathShown,
    change: &Change,
    locks: Option<Locks>,
    keys: &'a dyn KeyringAccess,
    between: &'a dyn Fn() -> Result<(), ChangeError>,
) -> Result<Plan<'a>, ChangeError> {
    keys.begin_phase();
    let store = FileStore::beside(config_path, shown);
    let text = ConfigText::read(config_path, shown)?;
    let config = text.current()?;
    let files: Vec<EndpointName> = config
        .endpoints
        .iter()
        .filter(|(_, e)| e.store() == Some(StoreKind::File))
        .map(|(n, _)| n.clone())
        .collect();
    let keyrings = super::keyring_names(&config);
    let leftover =
        matches!(change, Change::Remove { name, .. } if !config.endpoints.contains_key(name));
    let converts = matches!(change, Change::Edit { edit, .. } if edit.record.is_some() || edit.store == Some(StoreKind::File));
    let needs_store =
        !files.is_empty() || matches!(change, Change::Add { .. }) || leftover || converts;
    let every = if needs_store {
        read_every(&store)?
    } else {
        BTreeMap::new()
    };
    let records = config
        .store_id
        .as_ref()
        .and_then(|id| every.get(id))
        .cloned()
        .unwrap_or_default();
    let mut items = BTreeMap::new();
    let mut unreadable = BTreeMap::new();
    if let Some(id) = &config.store_id {
        for name in keyring_reads(&config, change) {
            match keys.read(id, &name) {
                Ok(Some(stored)) => {
                    items.insert(name, stored);
                }
                Ok(None) => {}
                Err(error) => {
                    unreadable.insert(name, error);
                }
            }
        }
    }
    Ok(Plan {
        store,
        locks,
        text,
        config,
        this: canonical_config(config_path),
        records,
        items,
        unreadable,
        files,
        keyrings,
        keys,
        between,
    })
}

#[cfg(test)]
mod tests;
