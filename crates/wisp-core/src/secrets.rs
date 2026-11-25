use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use wisp_protocol::endpoint::StoreChoice;
use wisp_protocol::settings::{KEY_STORE_FILE, KEY_STORE_KEYRING};
pub use wisp_protocol::{KeyProblem, Secret, check_key};

use crate::config_file::link_count;
pub use crate::endpoints::PathShown;
use crate::endpoints::{ConfigFile, EndpointName, KeyFrom};

mod change;
pub(crate) use change::apply_checked;
#[cfg(test)]
pub(crate) use change::apply_with;
pub mod detect;
mod env_file;
pub mod import;
pub mod keyring;
mod parse;
mod redaction;
pub(crate) mod scan;

pub use change::{
    Applied, Change, ChangeError, Edit, Failed, Left, Outcome, apply, apply_noting,
    cant_detach_keychain_words, cant_detach_words, check, keychain_off_for_flag,
    keychain_off_for_key, keychain_words,
};
pub use env_file::{
    GIVEN_ENV_FILE, env_file_names, env_text_problem, missing_env_file, read_env_file,
};
pub use keyring::{KeyringAccess, KeyringError, KeyringStore};
pub use parse::RecordProblem;
pub(crate) use redaction::Values;
#[cfg(any(test, feature = "test-support"))]
pub use redaction::live_redactions;
pub use redaction::{named_key_values, variable_redactions};
use wisp_protocol::IN_KEYCHAIN;
pub use wisp_protocol::{CREDENTIALS, LOCK_FILE, VariableName, looks_like_key, shown_variable};

pub const OAUTH: &str = "oauth";
pub const FILE_VERSION: u64 = 1;
pub const LOCK_WAIT: Duration = Duration::from_secs(10);
pub const OWNER_MAX: u64 = 1024 * 1024;
const TEMP_PREFIX: &str = ".wisp-tmp-";

pub type Every = BTreeMap<StoreId, BTreeMap<EndpointName, Stored>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum StoreKind {
    Keyring,
    File,
}

impl StoreKind {
    pub const ALL: [Self; 2] = [Self::File, Self::Keyring];

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == text)
    }

    pub fn chosen(choice: StoreChoice) -> Option<Self> {
        match choice {
            StoreChoice::Auto => None,
            StoreChoice::File => Some(Self::File),
            StoreChoice::Keyring => Some(Self::Keyring),
        }
    }

    pub fn words(self) -> &'static str {
        match self {
            Self::Keyring => IN_KEYCHAIN,
            Self::File => CREDENTIALS,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Keyring => KEY_STORE_KEYRING,
            Self::File => KEY_STORE_FILE,
        }
    }
}

impl From<StoreKind> for wisp_protocol::endpoint::StoreKind {
    fn from(kind: StoreKind) -> Self {
        match kind {
            StoreKind::Keyring => Self::Keyring,
            StoreKind::File => Self::File,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoredRecord {
    Key { key: Secret, url: Option<Secret> },
}

#[derive(Clone, PartialEq, Eq)]
pub struct Stored {
    pub record: StoredRecord,
    pub owner: Option<PathBuf>,
}

impl fmt::Debug for Stored {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Stored")
            .field("record", &self.record)
            .field("owner", &self.owner.as_ref().map(|_| Hidden("<path>")))
            .finish()
    }
}

struct Hidden(&'static str);

impl fmt::Debug for Hidden {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct StoreId(String);

impl StoreId {
    pub const LEN: usize = 16;
    pub const RULE: &str = "16 lowercase hex characters";

    pub fn parse(text: &str) -> Option<Self> {
        let valid = text.len() == Self::LEN
            && text
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        valid.then(|| Self(text.to_owned()))
    }

    pub fn new_random() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut hasher = Sha256::new();
        hasher.update(uuid::Uuid::now_v7().as_bytes());
        hasher.update(std::process::id().to_le_bytes());
        hasher.update(COUNTER.fetch_add(1, Ordering::Relaxed).to_le_bytes());
        let digest = hasher.finalize();
        Self(wisp_plugin_api::hex::encode(&digest[..Self::LEN / 2]))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for StoreId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StoreId(<hidden>)")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockKind {
    Config,
    Credentials,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathNoteKind {
    CredentialsInRepo { through_link: bool },
    LockInRepo(LockKind),
    CredentialsReadable,
    CredentialsLinked,
    TargetNotWritable,
    HardLinks,
    CredentialsHardLinks,
}

#[derive(Clone, PartialEq, Eq)]
pub struct PathNote {
    pub kind: PathNoteKind,
    pub path: PathBuf,
    pub repo: Option<PathBuf>,
    pub shown: PathShown,
}

impl fmt::Debug for PathNote {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PathNote")
            .field("kind", &self.kind)
            .field("path", &self.shown.path(&self.path))
            .field("repo", &self.repo.as_deref().map(|r| self.shown.path(r)))
            .field("shown", &self.shown)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct DetachNote {
    pub from: Vec<Option<PathBuf>>,
}

impl fmt::Debug for DetachNote {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let count = self.from.len();
        let noun = if count == 1 { "owner" } else { "owners" };
        write!(f, "DetachNote {{ from: {count} {noun} }}")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreError {
    Unreadable(io::ErrorKind),
    NotUtf8,
    NotAFile,
    Linked(PathNote),
    Syntax { line: usize },
    Record { line: usize, problem: RecordProblem },
    Newer,
    Refused,
    Busy,
    Lock(io::ErrorKind),
    NotLocked,
    Write(io::ErrorKind),
    Keychain(KeyringError),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable(kind) => write!(f, "{CREDENTIALS} can't be read ({kind})"),
            Self::NotUtf8 => write!(f, "{CREDENTIALS} is not valid UTF-8"),
            Self::NotAFile => write!(f, "{CREDENTIALS} is not a regular file"),
            Self::Linked(_) => write!(
                f,
                "{CREDENTIALS} is a symbolic link; Wisp keeps keys only in a regular file"
            ),
            Self::Syntax { line } => write!(f, "{CREDENTIALS} line {line}: not valid JSON"),
            Self::Record { line, problem } => write!(f, "{CREDENTIALS} line {line}: {problem}"),
            Self::Newer => write!(f, "{CREDENTIALS} is from a newer Wisp"),
            Self::Refused => write!(
                f,
                "fix or move aside {CREDENTIALS} first; nothing was changed"
            ),
            Self::Busy => f.write_str("another wisp is changing accounts; try again"),
            Self::Lock(kind) => write!(f, "could not take the account lock ({kind})"),
            Self::NotLocked => write!(f, "{CREDENTIALS} can only be written under its lock"),
            Self::Write(kind) => write!(f, "could not write {CREDENTIALS} ({kind})"),
            Self::Keychain(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for StoreError {}

pub trait SecretLookup {
    fn get(
        &self,
        store: StoreKind,
        name: &EndpointName,
    ) -> Result<Option<StoredRecord>, StoreError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoSecrets;

impl SecretLookup for NoSecrets {
    fn get(&self, _: StoreKind, _: &EndpointName) -> Result<Option<StoredRecord>, StoreError> {
        Ok(None)
    }
}

pub type KeyringRecords = BTreeMap<EndpointName, Result<Option<Stored>, KeyringError>>;

#[derive(Clone)]
pub struct Prefetched {
    file: Result<BTreeMap<EndpointName, Stored>, StoreError>,
    keyring: KeyringRecords,
}

impl fmt::Debug for Prefetched {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let keyring = self.keyring.len();
        match &self.file {
            Ok(records) => write!(
                f,
                "Prefetched({} records, {keyring} keychain lookups)",
                records.len()
            ),
            Err(e) => write!(f, "Prefetched({e:?}, {keyring} keychain lookups)"),
        }
    }
}

impl Default for Prefetched {
    fn default() -> Self {
        Self::from_records(BTreeMap::new())
    }
}

impl Prefetched {
    pub fn from_records(records: BTreeMap<EndpointName, Stored>) -> Self {
        Self {
            file: Ok(records),
            keyring: BTreeMap::new(),
        }
    }

    pub fn failing(error: StoreError) -> Self {
        Self {
            file: Err(error),
            keyring: BTreeMap::new(),
        }
    }

    pub fn load(config: Option<&ConfigFile>) -> Self {
        let Some(config) = config.filter(|c| c.stores(StoreKind::File)) else {
            return Self::default();
        };
        let Some(id) = &config.store_id else {
            return Self::default();
        };
        Self::from_file(FileStore::beside(&config.path, config.shown).read_all(id))
    }

    fn from_file(file: Result<BTreeMap<EndpointName, Stored>, StoreError>) -> Self {
        Self {
            file,
            keyring: BTreeMap::new(),
        }
    }

    pub fn with_keyring(mut self, keyring: KeyringRecords) -> Self {
        self.keyring = keyring;
        self
    }

    pub fn records(&self) -> Result<&BTreeMap<EndpointName, Stored>, &StoreError> {
        self.file.as_ref()
    }

    pub fn keyring_records(&self) -> &KeyringRecords {
        &self.keyring
    }
}

pub fn keyring_names(config: &ConfigFile) -> Vec<EndpointName> {
    config
        .endpoints
        .iter()
        .filter(|(_, e)| e.store() == Some(StoreKind::Keyring))
        .map(|(n, _)| n.clone())
        .collect()
}

impl SecretLookup for Prefetched {
    fn get(
        &self,
        store: StoreKind,
        name: &EndpointName,
    ) -> Result<Option<StoredRecord>, StoreError> {
        match store {
            StoreKind::Keyring => match self.keyring.get(name) {
                Some(Ok(found)) => Ok(found.as_ref().map(|s| s.record.clone())),
                Some(Err(e)) => Err(StoreError::Keychain(e.clone())),
                None => Ok(None),
            },
            StoreKind::File => match &self.file {
                Ok(records) => Ok(records.get(name).map(|s| s.record.clone())),
                Err(e) => Err(e.clone()),
            },
        }
    }
}

pub fn credentials_path(config_path: &Path) -> PathBuf {
    match config_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(folder) => folder.join(CREDENTIALS),
        None => PathBuf::from(CREDENTIALS),
    }
}

pub(crate) fn folder_of(path: &Path) -> PathBuf {
    match path.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(folder) => folder.to_owned(),
        None => PathBuf::from("."),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Written {
    pub notes: Vec<PathNote>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct FileStore {
    path: PathBuf,
    shown: PathShown,
}

impl fmt::Debug for FileStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileStore")
            .field("path", &self.shown.path(&self.path))
            .field("shown", &self.shown)
            .finish()
    }
}

impl FileStore {
    pub fn beside(config_path: &Path, shown: PathShown) -> Self {
        Self {
            path: credentials_path(config_path),
            shown,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn shown(&self) -> PathShown {
        self.shown
    }

    pub fn folder(&self) -> PathBuf {
        folder_of(&self.path)
    }

    pub fn exists(&self) -> bool {
        std::fs::symlink_metadata(&self.path).is_ok()
    }

    fn linked(&self) -> StoreError {
        StoreError::Linked(PathNote {
            kind: PathNoteKind::CredentialsLinked,
            path: self.path.clone(),
            repo: None,
            shown: self.shown,
        })
    }

    fn read_text(&self) -> Result<Option<String>, StoreError> {
        let meta = match std::fs::symlink_metadata(&self.path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(StoreError::Unreadable(e.kind())),
        };
        if meta.file_type().is_symlink() {
            return Err(self.linked());
        }
        if !meta.is_file() {
            return Err(StoreError::NotAFile);
        }
        let bytes = std::fs::read(&self.path).map_err(|e| StoreError::Unreadable(e.kind()))?;
        String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| StoreError::NotUtf8)
    }

    pub fn read_every(&self) -> Result<Every, StoreError> {
        match self.read_text()? {
            None => Ok(BTreeMap::new()),
            Some(text) => match parse::parse(&text)? {
                parse::Parsed::Records(every) => Ok(every),
                parse::Parsed::Newer => Err(StoreError::Newer),
            },
        }
    }

    pub fn read_all(&self, id: &StoreId) -> Result<BTreeMap<EndpointName, Stored>, StoreError> {
        Ok(self.read_every()?.remove(id).unwrap_or_default())
    }

    fn change(&self, locks: &Locks, apply: impl FnOnce(&mut Every)) -> Result<Written, StoreError> {
        let folder = self.folder();
        if !locks.covers(&folder) {
            return Err(StoreError::NotLocked);
        }
        let mut every = match self.read_every() {
            Ok(every) => every,
            Err(StoreError::Linked(note)) => return Err(StoreError::Linked(note)),
            Err(StoreError::Unreadable(kind)) => return Err(StoreError::Unreadable(kind)),
            Err(_) => return Err(StoreError::Refused),
        };
        apply(&mut every);
        let text = parse::render(&every)?;
        let hard_linked = std::fs::metadata(&self.path).is_ok_and(|meta| link_count(&meta) > 1);
        write_atomic(&self.path, text.as_bytes(), 0o600)
            .map_err(|e| StoreError::Write(e.kind()))?;
        let mut notes: Vec<PathNote> = work_tree(&folder)
            .map(|(real, repo)| PathNote {
                kind: PathNoteKind::CredentialsInRepo {
                    through_link: through_link(&folder, &repo),
                },
                path: real,
                repo: Some(repo),
                shown: self.shown,
            })
            .into_iter()
            .collect();
        if hard_linked {
            notes.push(PathNote {
                kind: PathNoteKind::CredentialsHardLinks,
                path: self.path.clone(),
                repo: None,
                shown: self.shown,
            });
        }
        Ok(Written { notes })
    }

    pub fn put(
        &self,
        locks: &Locks,
        id: &StoreId,
        name: &EndpointName,
        stored: Stored,
    ) -> Result<Written, StoreError> {
        self.change(locks, |every| {
            every
                .entry(id.clone())
                .or_default()
                .insert(name.clone(), stored);
        })
    }

    pub fn remove(
        &self,
        locks: &Locks,
        id: &StoreId,
        name: &EndpointName,
    ) -> Result<Written, StoreError> {
        self.change(locks, |every| {
            if let Some(store) = every.get_mut(id) {
                store.remove(name);
                if store.is_empty() {
                    every.remove(id);
                }
            }
        })
    }
}

#[cfg(unix)]
type FolderId = (u64, u64);

#[cfg(not(unix))]
type FolderId = PathBuf;

#[cfg(unix)]
fn folder_id(folder: &Path) -> io::Result<FolderId> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(folder)?;
    Ok((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn folder_id(folder: &Path) -> io::Result<FolderId> {
    std::fs::canonicalize(folder)
}

pub struct Locks {
    held: Vec<(FolderId, File)>,
    notes: Vec<PathNote>,
}

impl Locks {
    pub fn notes(&self) -> &[PathNote] {
        &self.notes
    }

    pub fn covers(&self, folder: &Path) -> bool {
        folder_id(folder).is_ok_and(|id| self.held.iter().any(|(held, _)| *held == id))
    }
}

impl Drop for Locks {
    fn drop(&mut self) {
        for (_, file) in &self.held {
            let _ = file.unlock();
        }
    }
}

pub(crate) fn create_folder(folder: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(folder)
}

pub fn lock(
    folders: &[(&Path, LockKind)],
    shown: PathShown,
    wait: Duration,
) -> Result<Locks, StoreError> {
    lock_noting(folders, shown, wait).map_err(|(error, _)| error)
}

pub fn lock_noting(
    folders: &[(&Path, LockKind)],
    shown: PathShown,
    wait: Duration,
) -> Result<Locks, (StoreError, Vec<PathNote>)> {
    let mut wanted: BTreeMap<FolderId, (PathBuf, LockKind)> = BTreeMap::new();
    for (folder, kind) in folders {
        let found = create_folder(folder)
            .and_then(|()| std::fs::canonicalize(folder))
            .and_then(|real| folder_id(&real).map(|id| (real, id)));
        let (real, id) = found.map_err(|e| (StoreError::Lock(e.kind()), Vec::new()))?;
        let entry = wanted.entry(id).or_insert((real, *kind));
        if *kind == LockKind::Config {
            entry.1 = LockKind::Config;
        }
    }
    let mut notes = Vec::new();
    let mut opened = Vec::new();
    for (id, (real, kind)) in wanted {
        let path = real.join(LOCK_FILE);
        let file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => {
                if let Some((real, repo)) = work_tree(&real) {
                    notes.push(PathNote {
                        kind: PathNoteKind::LockInRepo(kind),
                        path: real,
                        repo: Some(repo),
                        shown,
                    });
                }
                file
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                match std::fs::OpenOptions::new().write(true).open(&path) {
                    Ok(file) => file,
                    Err(e) => return Err((StoreError::Lock(e.kind()), notes)),
                }
            }
            Err(e) => return Err((StoreError::Lock(e.kind()), notes)),
        };
        opened.push((id, file));
    }
    let mut locks = Locks {
        held: opened,
        notes,
    };
    let deadline = Instant::now() + wait;
    for (_, file) in &locks.held {
        loop {
            let error = match crate::file_lock::try_lock(file) {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(25));
                    continue;
                }
                Err(std::fs::TryLockError::WouldBlock) => StoreError::Busy,
                Err(std::fs::TryLockError::Error(e)) => StoreError::Lock(e.kind()),
            };
            return Err((error, std::mem::take(&mut locks.notes)));
        }
    }
    Ok(locks)
}

pub fn work_tree(folder: &Path) -> Option<(PathBuf, PathBuf)> {
    let real = std::fs::canonicalize(folder).ok()?;
    let repo = crate::instructions::repository(&real, &crate::instructions::Disk)?;
    Some((real, repo))
}

pub fn through_link(folder: &Path, repo: &Path) -> bool {
    let Ok(given) = std::path::absolute(folder) else {
        return false;
    };
    let ancestors: Vec<&Path> = given.ancestors().collect();
    let Some(top) = ancestors
        .iter()
        .position(|a| std::fs::canonicalize(a).is_ok_and(|real| real == repo))
    else {
        return true;
    };
    ancestors[..top]
        .iter()
        .any(|a| std::fs::symlink_metadata(a).is_ok_and(|m| m.file_type().is_symlink()))
}

pub fn write_atomic(target: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let folder = folder_of(target);
    if let Ok(entries) = std::fs::read_dir(&folder) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(TEMP_PREFIX) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    replace_atomic(target, bytes, mode, || Ok(()))
}

pub(crate) fn replace_atomic(
    target: &Path,
    bytes: &[u8],
    mode: u32,
    before_rename: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let temp = staged(target, bytes, mode)?;
    before_rename()?;
    temp.persist(target).map_err(|e| e.error)?;
    synced(target)
}

pub(crate) fn create_atomic(target: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let temp = staged(target, bytes, mode)?;
    temp.persist_noclobber(target).map_err(|e| e.error)?;
    synced(target)
}

fn staged(target: &Path, bytes: &[u8], mode: u32) -> io::Result<tempfile::NamedTempFile> {
    let mut temp = tempfile::Builder::new()
        .prefix(TEMP_PREFIX)
        .tempfile_in(folder_of(target))?;
    temp.write_all(bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    temp.as_file().sync_all()?;
    Ok(temp)
}

fn synced(target: &Path) -> io::Result<()> {
    #[cfg(unix)]
    File::open(folder_of(target))?.sync_all()?;
    #[cfg(not(unix))]
    let _ = target;
    Ok(())
}

pub(crate) fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0; N];
    for chunk in bytes.chunks_mut(16) {
        chunk.copy_from_slice(&uuid::Uuid::new_v4().as_bytes()[..chunk.len()]);
    }
    bytes
}

#[cfg(unix)]
pub fn readable_by_others(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o044 != 0)
}

#[cfg(not(unix))]
pub fn readable_by_others(_path: &Path) -> bool {
    false
}

pub fn path_notes(config_path: &Path, shown: PathShown) -> Vec<PathNote> {
    let store = FileStore::beside(config_path, shown);
    let mut notes = Vec::new();
    let note = |kind, path: PathBuf, repo| PathNote {
        kind,
        path,
        repo,
        shown,
    };
    match std::fs::symlink_metadata(store.path()) {
        Ok(meta) if meta.file_type().is_symlink() => notes.push(note(
            PathNoteKind::CredentialsLinked,
            store.path().to_owned(),
            None,
        )),
        Ok(meta) if meta.is_file() => {
            if readable_by_others(store.path()) {
                notes.push(note(
                    PathNoteKind::CredentialsReadable,
                    store.path().to_owned(),
                    None,
                ));
            }
            if let Some((real, repo)) = work_tree(&store.folder()) {
                let through_link = through_link(&store.folder(), &repo);
                notes.push(note(
                    PathNoteKind::CredentialsInRepo { through_link },
                    real,
                    Some(repo),
                ));
            }
        }
        _ => {}
    }
    let config_folder = std::fs::canonicalize(config_path)
        .ok()
        .map(|real| folder_of(&real));
    let credentials_folder = std::fs::canonicalize(store.folder()).ok();
    let mut seen: Vec<FolderId> = Vec::new();
    for (folder, kind) in [
        (config_folder, LockKind::Config),
        (credentials_folder, LockKind::Credentials),
    ] {
        let Some(folder) = folder else { continue };
        let Ok(id) = folder_id(&folder) else { continue };
        if seen.contains(&id) {
            continue;
        }
        seen.push(id);
        if std::fs::symlink_metadata(folder.join(LOCK_FILE)).is_ok()
            && let Some((real, repo)) = work_tree(&folder)
        {
            notes.push(note(PathNoteKind::LockInRepo(kind), real, Some(repo)));
        }
    }
    notes
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum OwnerState {
    Uses,
    Gone,
    NoLongerUses,
    Unreadable,
}

pub fn owner_state(owner: &Path, id: &StoreId) -> OwnerState {
    owner_state_with(owner, id, &|| ())
}

fn owner_state_with(owner: &Path, id: &StoreId, between: &dyn Fn()) -> OwnerState {
    let meta = match std::fs::metadata(owner) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return OwnerState::Gone,
        Err(_) => return OwnerState::Unreadable,
    };
    if !meta.is_file() || meta.len() > OWNER_MAX {
        return OwnerState::Unreadable;
    }
    between();
    let Ok(file) = File::open(owner) else {
        return OwnerState::Unreadable;
    };
    let mut bytes = Vec::new();
    if file.take(OWNER_MAX + 1).read_to_end(&mut bytes).is_err() || bytes.len() as u64 > OWNER_MAX {
        return OwnerState::Unreadable;
    }
    let Ok(text) = String::from_utf8(bytes) else {
        return OwnerState::Unreadable;
    };
    match ConfigFile::parse(owner, PathShown::Default, &text) {
        Ok(config) if config.store_id.as_ref() == Some(id) => OwnerState::Uses,
        Ok(_) => OwnerState::NoLongerUses,
        Err(_) => OwnerState::Unreadable,
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct NotOwned {
    pub name: EndpointName,
    pub owner: PathBuf,
    pub state: OwnerState,
}

impl fmt::Debug for NotOwned {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NotOwned")
            .field("name", &self.name)
            .field("owner", &Hidden("<path>"))
            .field("state", &self.state)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Orphans {
    pub owner: Option<PathBuf>,
    pub state: Option<OwnerState>,
    pub count: usize,
}

impl fmt::Debug for Orphans {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Orphans")
            .field("owner", &self.owner.as_ref().map(|_| Hidden("<path>")))
            .field("state", &self.state)
            .field("count", &self.count)
            .finish()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ownership {
    pub not_owned: Vec<NotOwned>,
    pub hand_written: Vec<EndpointName>,
    pub unnamed: Vec<EndpointName>,
    pub variable: Vec<EndpointName>,
    pub orphans: Vec<Orphans>,
}

pub fn ownership(config: &ConfigFile, every: &Every) -> Ownership {
    let this = std::fs::canonicalize(&config.path).ok();
    let mut found = Ownership::default();
    let ours = config.store_id.as_ref();
    if let Some(id) = ours
        && let Some(store) = every.get(id)
    {
        for (name, stored) in store {
            match config.endpoints.get(name).map(|e| &e.key) {
                Some(KeyFrom::Store(StoreKind::File)) => {}
                Some(KeyFrom::Variable(_)) => {
                    found.variable.push(name.clone());
                    continue;
                }
                Some(KeyFrom::Store(StoreKind::Keyring)) | None => {
                    found.unnamed.push(name.clone());
                    continue;
                }
            }
            match &stored.owner {
                Some(owner) if this.as_ref() != Some(owner) => found.not_owned.push(NotOwned {
                    name: name.clone(),
                    owner: owner.clone(),
                    state: owner_state(owner, id),
                }),
                Some(_) => {}
                None => found.hand_written.push(name.clone()),
            }
        }
    }
    let mut groups: BTreeMap<(Option<PathBuf>, Option<OwnerState>), usize> = BTreeMap::new();
    for (id, store) in every {
        if Some(id) == ours {
            continue;
        }
        for stored in store.values() {
            let key = match &stored.owner {
                None => (None, None),
                Some(owner) => match owner_state(owner, id) {
                    OwnerState::Uses => continue,
                    state => (Some(owner.clone()), Some(state)),
                },
            };
            *groups.entry(key).or_default() += 1;
        }
    }
    found.orphans = groups
        .into_iter()
        .map(|((owner, state), count)| Orphans {
            owner,
            state,
            count,
        })
        .collect();
    found
}

#[cfg(test)]
mod tests;

pub fn is_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| v.to_str().is_some_and(|s| !s.trim().is_empty()))
}
