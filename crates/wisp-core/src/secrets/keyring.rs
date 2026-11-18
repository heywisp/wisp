use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};
use wisp_protocol::Secret;
use wisp_protocol::settings::A_STRING;

use super::{StoreId, Stored, StoredRecord};
use crate::endpoints::EndpointName;

#[cfg(feature = "os-keyring")]
pub mod platform;

pub const SERVICE: &str = "wisp";
pub const ITEM_VERSION: u64 = 1;
pub const SHORT_BUDGET: Duration = Duration::from_secs(2);
pub const TERMINAL_DEADLINE: Duration = Duration::from_secs(60);
pub const HEADLESS_DEADLINE: Duration = Duration::from_secs(10);
pub const SLOW_NOTICE: Duration = Duration::from_secs(2);

pub fn account(id: &StoreId, name: &EndpointName) -> String {
    format!("{}/{name}", id.as_str())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendError {
    NoEntry,
    NoStorageAccess,
    PlatformFailure,
    TooLong,
    BadEncoding,
    BadDataFormat,
    Invalid,
    Ambiguous,
    Unsupported,
    Other,
}

pub trait KeyringBackend: Send {
    fn get(&mut self, account: &str) -> Result<String, BackendError>;
    fn set(&mut self, account: &str, secret: &str) -> Result<(), BackendError>;
    fn delete(&mut self, account: &str) -> Result<(), BackendError>;
}

pub type Opener = Box<dyn FnOnce() -> Result<Box<dyn KeyringBackend>, BackendError> + Send>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemProblem {
    NotJson,
    NotAnObject,
    NoVersion,
    BadVersion,
    UnknownField,
    NoKey,
    KeyNotString,
    OwnerNotString,
    UrlNotString,
}

impl fmt::Display for ItemProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotJson => "it isn't JSON",
            Self::NotAnObject => "a record must be a JSON object",
            Self::NoVersion => "a record needs v",
            Self::BadVersion => "v must be a whole number from 1",
            Self::UnknownField => "unknown field; allowed: v, key, owner, url",
            Self::NoKey => "a record needs a key",
            Self::KeyNotString => return write!(f, "key must be {A_STRING}"),
            Self::OwnerNotString => return write!(f, "owner must be {A_STRING}"),
            Self::UrlNotString => return write!(f, "url must be {A_STRING}"),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyringError {
    Off {
        origin: String,
    },
    Unsupported,
    TimedOut,
    NotRunning,
    Locked,
    Failed,
    BusSilent,
    TooLong,
    Refused,
    BadText {
        name: EndpointName,
    },
    BadItem {
        name: EndpointName,
        problem: ItemProblem,
    },
    Newer {
        name: EndpointName,
    },
    Unconfirmed {
        name: EndpointName,
        account: String,
    },
}

impl KeyringError {
    pub fn downs_session(&self) -> bool {
        matches!(
            self,
            Self::TimedOut | Self::NotRunning | Self::Locked | Self::Failed | Self::BusSilent
        )
    }

    pub fn which(&self) -> Option<String> {
        Some(match self {
            Self::Off { origin } => format!("is off ({origin})"),
            Self::Unsupported => {
                "this Wisp can't reach (it was built without os-keyring)".to_owned()
            }
            Self::TimedOut | Self::BusSilent => "didn't answer".to_owned(),
            Self::NotRunning => "isn't running".to_owned(),
            Self::Locked => "is locked or refused access".to_owned(),
            Self::Failed => "failed".to_owned(),
            Self::TooLong | Self::Refused => "refused the request".to_owned(),
            Self::BadText { .. }
            | Self::BadItem { .. }
            | Self::Newer { .. }
            | Self::Unconfirmed { .. } => return None,
        })
    }

    pub fn is_off(&self) -> bool {
        matches!(self, Self::Off { .. })
    }
}

impl fmt::Display for KeyringError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Off { origin } => write!(f, "the keychain is off ({origin})"),
            Self::Unsupported => f.write_str(
                "this Wisp was built without the os-keyring feature, so it can't reach a keychain",
            ),
            Self::TimedOut => f.write_str("the keychain didn't answer"),
            Self::NotRunning => f.write_str("the keychain isn't running"),
            Self::Locked => f.write_str("the keychain is locked or refused access"),
            Self::Failed => f.write_str("the keychain failed"),
            Self::BusSilent => f.write_str("the session bus didn't answer"),
            Self::TooLong => f.write_str("the key and URL are too long for the keychain"),
            Self::Refused => f.write_str("the keychain refused the request"),
            Self::BadText { name } => {
                write!(f, "the keychain item for {name} isn't valid text")
            }
            Self::BadItem { name, problem } => write!(
                f,
                "the keychain item for {name} isn't a valid Wisp record ({problem})"
            ),
            Self::Newer { name } => {
                write!(f, "the keychain item for {name} is from a newer Wisp")
            }
            Self::Unconfirmed { name, account } => write!(
                f,
                "the keychain didn't confirm the write, so {name}'s key may have been saved there (account {account})"
            ),
        }
    }
}

impl std::error::Error for KeyringError {}

pub fn encode(stored: &Stored) -> String {
    let StoredRecord::Key { key, url } = &stored.record;
    let mut map = Map::new();
    map.insert("v".to_owned(), Value::from(ITEM_VERSION));
    map.insert("key".to_owned(), Value::from(key.expose()));
    if let Some(owner) = &stored.owner {
        map.insert(
            "owner".to_owned(),
            Value::from(owner.to_string_lossy().into_owned()),
        );
    }
    if let Some(url) = url {
        map.insert("url".to_owned(), Value::from(url.expose()));
    }
    Value::Object(map).to_string()
}

pub fn decode(text: &str, name: &EndpointName) -> Result<Stored, KeyringError> {
    let bad = |problem| KeyringError::BadItem {
        name: name.clone(),
        problem,
    };
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return Err(bad(ItemProblem::NotJson));
    };
    let Value::Object(map) = value else {
        return Err(bad(ItemProblem::NotAnObject));
    };
    match map.get("v") {
        None => return Err(bad(ItemProblem::NoVersion)),
        Some(v) => match v.as_u64() {
            Some(ITEM_VERSION) => {}
            Some(n) if n > ITEM_VERSION => return Err(KeyringError::Newer { name: name.clone() }),
            _ => return Err(bad(ItemProblem::BadVersion)),
        },
    }
    if map
        .keys()
        .any(|k| !matches!(k.as_str(), "v" | "key" | "owner" | "url"))
    {
        return Err(bad(ItemProblem::UnknownField));
    }
    let key = match map.get("key") {
        None => return Err(bad(ItemProblem::NoKey)),
        Some(Value::String(key)) => key.clone(),
        Some(_) => return Err(bad(ItemProblem::KeyNotString)),
    };
    let owner = match map.get("owner") {
        None => None,
        Some(Value::String(owner)) => Some(PathBuf::from(owner)),
        Some(_) => return Err(bad(ItemProblem::OwnerNotString)),
    };
    let url = match map.get("url") {
        None => None,
        Some(Value::String(url)) => Some(Secret::new(url.clone())),
        Some(_) => return Err(bad(ItemProblem::UrlNotString)),
    };
    Ok(Stored {
        record: StoredRecord::Key {
            key: Secret::new(key),
            url,
        },
        owner,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Budget {
    Start,
    Short,
    Fresh(Duration),
}

#[derive(Clone, Copy)]
enum Limit {
    Start,
    Short,
    Until(Instant),
}

enum Op {
    Get(String),
    Set(String, String),
    Delete(String),
}

enum Reply {
    Got(String),
    Done,
}

enum Fault {
    Open,
    Call(BackendError),
}

struct Job {
    op: Op,
    reply: mpsc::Sender<Result<Reply, Fault>>,
}

#[derive(Clone)]
enum Cached {
    Present(Stored),
    Absent,
    Bad(KeyringError),
}

#[derive(Default)]
struct Shared {
    down: Option<KeyringError>,
    cache: BTreeMap<String, Cached>,
    sender: Option<mpsc::Sender<Job>>,
    opener: Option<Opener>,
}

pub type Notice = Box<dyn Fn() + Send + Sync>;

#[cfg(any(test, feature = "test-support"))]
pub const FAKE_SLICE: Duration = Duration::from_millis(2);

#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Debug)]
pub struct FakeClock {
    base: Instant,
    offset: std::sync::Arc<Mutex<Duration>>,
}

#[cfg(any(test, feature = "test-support"))]
impl Default for FakeClock {
    fn default() -> Self {
        Self {
            base: Instant::now(),
            offset: std::sync::Arc::default(),
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl FakeClock {
    fn offset(&self) -> MutexGuard<'_, Duration> {
        self.offset
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn now(&self) -> Instant {
        let offset = *self.offset();
        self.base.checked_add(offset).unwrap_or(self.base)
    }

    pub fn advance(&self, by: Duration) {
        let mut offset = self.offset();
        *offset = offset.saturating_add(by);
    }
}

#[derive(Clone, Debug)]
enum Clock {
    Real,
    #[cfg(any(test, feature = "test-support"))]
    Fake(FakeClock),
}

impl Clock {
    fn now(&self) -> Instant {
        match self {
            Self::Real => Instant::now(),
            #[cfg(any(test, feature = "test-support"))]
            Self::Fake(fake) => fake.now(),
        }
    }

    fn receive<T>(
        &self,
        answer: &mpsc::Receiver<T>,
        wait: Duration,
    ) -> Result<T, mpsc::RecvTimeoutError> {
        match self {
            Self::Real => answer.recv_timeout(wait),
            #[cfg(any(test, feature = "test-support"))]
            Self::Fake(fake) => {
                let started = fake.now();
                loop {
                    let spent = fake.now().saturating_duration_since(started);
                    if spent >= wait {
                        return answer.try_recv().map_err(|e| match e {
                            mpsc::TryRecvError::Empty => mpsc::RecvTimeoutError::Timeout,
                            mpsc::TryRecvError::Disconnected => {
                                mpsc::RecvTimeoutError::Disconnected
                            }
                        });
                    }
                    match answer.recv_timeout(FAKE_SLICE) {
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        other => return other,
                    }
                }
            }
        }
    }
}

pub struct KeyringStore {
    shared: Mutex<Shared>,
    deadline: Mutex<Instant>,
    phase: Duration,
    notice: Option<Notice>,
    told: AtomicBool,
    clock: Clock,
}

impl fmt::Debug for KeyringStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let shared = self.lock();
        f.debug_struct("KeyringStore")
            .field("down", &shared.down)
            .field("cached", &shared.cache.len())
            .finish()
    }
}

fn serve(opener: Opener, jobs: mpsc::Receiver<Job>) {
    let mut opener = Some(opener);
    let mut backend: Option<Box<dyn KeyringBackend>> = None;
    for job in jobs {
        if backend.is_none() {
            match opener.take().map(|open| open()) {
                Some(Ok(opened)) => backend = Some(opened),
                Some(Err(_)) | None => {
                    let _ = job.reply.send(Err(Fault::Open));
                    continue;
                }
            }
        }
        let Some(store) = backend.as_mut() else {
            let _ = job.reply.send(Err(Fault::Open));
            continue;
        };
        let answer = match job.op {
            Op::Get(account) => store.get(&account).map(Reply::Got),
            Op::Set(account, secret) => store.set(&account, &secret).map(|()| Reply::Done),
            Op::Delete(account) => store.delete(&account).map(|()| Reply::Done),
        };
        let _ = job.reply.send(answer.map_err(Fault::Call));
    }
}

fn session_error(error: BackendError, name: &EndpointName) -> KeyringError {
    match error {
        BackendError::NoStorageAccess => KeyringError::Locked,
        BackendError::PlatformFailure => KeyringError::Failed,
        BackendError::TooLong => KeyringError::TooLong,
        BackendError::BadEncoding | BackendError::BadDataFormat => {
            KeyringError::BadText { name: name.clone() }
        }
        BackendError::NoEntry
        | BackendError::Invalid
        | BackendError::Ambiguous
        | BackendError::Unsupported
        | BackendError::Other => KeyringError::Refused,
    }
}

impl KeyringStore {
    pub fn new(opener: Opener, deadline: Duration) -> Self {
        Self::on(opener, deadline, Clock::Real)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn with_clock(opener: Opener, deadline: Duration, clock: FakeClock) -> Self {
        Self::on(opener, deadline, Clock::Fake(clock))
    }

    fn on(opener: Opener, deadline: Duration, clock: Clock) -> Self {
        Self {
            shared: Mutex::new(Shared {
                opener: Some(opener),
                ..Shared::default()
            }),
            deadline: Mutex::new(clock.now() + deadline),
            phase: deadline,
            notice: None,
            told: AtomicBool::new(false),
            clock,
        }
    }

    pub fn with_notice(mut self, notice: Notice) -> Self {
        self.notice = Some(notice);
        self
    }

    pub fn down_from_start(self, reason: KeyringError) -> Self {
        self.lock().down = Some(reason);
        self
    }

    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn begin_phase(&self) {
        *self
            .deadline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = self.clock.now() + self.phase;
    }

    pub fn down(&self) -> Option<KeyringError> {
        self.lock().down.clone()
    }

    fn mark_down(&self, reason: KeyringError) -> KeyringError {
        let mut shared = self.lock();
        let reason = shared.down.get_or_insert(reason).clone();
        shared.sender = None;
        reason
    }

    fn sender(&self) -> Result<mpsc::Sender<Job>, KeyringError> {
        let mut shared = self.lock();
        if let Some(down) = &shared.down {
            return Err(down.clone());
        }
        if let Some(sender) = &shared.sender {
            return Ok(sender.clone());
        }
        let Some(opener) = shared.opener.take() else {
            return Err(KeyringError::Failed);
        };
        let (sender, jobs) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("wisp-keychain".to_owned())
            .spawn(move || serve(opener, jobs));
        if spawned.is_err() {
            shared.down = Some(KeyringError::Failed);
            return Err(KeyringError::Failed);
        }
        shared.sender = Some(sender.clone());
        Ok(sender)
    }

    fn limit(&self, budget: Budget) -> Limit {
        match budget {
            Budget::Start => Limit::Start,
            Budget::Short => Limit::Short,
            Budget::Fresh(limit) => Limit::Until(self.clock.now() + limit),
        }
    }

    fn wait(&self, limit: Limit) -> Duration {
        let deadline = *self
            .deadline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = self.clock.now();
        let left = deadline.saturating_duration_since(now);
        match limit {
            Limit::Start => left,
            Limit::Short => left.min(SHORT_BUDGET),
            Limit::Until(end) => end.saturating_duration_since(now),
        }
    }

    pub fn read_oauth_profile(&self, account: &str) -> Result<Option<Secret>, KeyringError> {
        self.get(
            account,
            self.limit(Budget::Fresh(TERMINAL_DEADLINE)),
            &EndpointName::env(),
        )
        .map(|value| value.map(Secret::new))
    }

    pub fn write_oauth_profile(&self, account: &str, value: &Secret) -> Result<(), KeyringError> {
        let limit = self.limit(Budget::Fresh(TERMINAL_DEADLINE));
        match self.raw(
            Op::Set(account.to_owned(), value.expose().to_owned()),
            limit,
            &EndpointName::env(),
        )? {
            Raw::Done => {}
            _ => return Err(KeyringError::Failed),
        }
        match self.raw(Op::Get(account.to_owned()), limit, &EndpointName::env())? {
            Raw::Got(back) if back == value.expose() => Ok(()),
            _ => Err(KeyringError::Unconfirmed {
                name: EndpointName::env(),
                account: account.to_owned(),
            }),
        }
    }

    pub fn delete_oauth_profile(&self, account: &str) -> Result<(), KeyringError> {
        match self.raw(
            Op::Delete(account.to_owned()),
            self.limit(Budget::Fresh(TERMINAL_DEADLINE)),
            &EndpointName::env(),
        )? {
            Raw::Done | Raw::Missing => Ok(()),
            _ => Err(KeyringError::Failed),
        }
    }

    pub fn read(
        &self,
        id: &StoreId,
        name: &EndpointName,
        budget: Budget,
    ) -> Result<Option<Stored>, KeyringError> {
        let account = account(id, name);
        if let Some(cached) = self.lock().cache.get(&account).cloned() {
            return match cached {
                Cached::Present(stored) => Ok(Some(stored)),
                Cached::Absent => Ok(None),
                Cached::Bad(e) => Err(e),
            };
        }
        let found = match self.get(&account, self.limit(budget), name)? {
            None => Cached::Absent,
            Some(text) => match decode(&text, name) {
                Ok(stored) => Cached::Present(stored),
                Err(e) => Cached::Bad(e),
            },
        };
        self.lock().cache.insert(account, found.clone());
        match found {
            Cached::Present(stored) => Ok(Some(stored)),
            Cached::Absent => Ok(None),
            Cached::Bad(e) => Err(e),
        }
    }

    fn get(
        &self,
        account: &str,
        limit: Limit,
        name: &EndpointName,
    ) -> Result<Option<String>, KeyringError> {
        match self.raw(Op::Get(account.to_owned()), limit, name)? {
            Raw::Missing => Ok(None),
            Raw::Got(text) => Ok(Some(text)),
            Raw::Done => Err(KeyringError::Failed),
        }
    }

    fn raw(&self, op: Op, limit: Limit, name: &EndpointName) -> Result<Raw, KeyringError> {
        let sender = self.sender()?;
        let wait = self.wait(limit);
        if wait.is_zero() {
            return Err(self.mark_down(KeyringError::TimedOut));
        }
        let (reply, answer) = mpsc::channel();
        if sender.send(Job { op, reply }).is_err() {
            return Err(self.mark_down(KeyringError::Failed));
        }
        let started = self.clock.now();
        let notice_due = self
            .notice
            .as_ref()
            .filter(|_| matches!(limit, Limit::Start) && !self.told.load(Ordering::Relaxed));
        let first = match notice_due {
            Some(_) => wait.min(SLOW_NOTICE),
            None => wait,
        };
        let mut got = self.clock.receive(&answer, first);
        if let (Err(mpsc::RecvTimeoutError::Timeout), Some(notice)) = (&got, notice_due)
            && first < wait
        {
            self.told.store(true, Ordering::Relaxed);
            notice();
            let spent = self.clock.now().saturating_duration_since(started);
            got = self.clock.receive(&answer, wait.saturating_sub(spent));
        }
        match got {
            Ok(Ok(Reply::Got(text))) => Ok(Raw::Got(text)),
            Ok(Ok(Reply::Done)) => Ok(Raw::Done),
            Ok(Err(Fault::Open)) => Err(self.mark_down(KeyringError::NotRunning)),
            Ok(Err(Fault::Call(BackendError::NoEntry))) => Ok(Raw::Missing),
            Ok(Err(Fault::Call(error))) => {
                let mapped = session_error(error, name);
                if mapped.downs_session() {
                    Err(self.mark_down(mapped))
                } else {
                    Err(mapped)
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => Err(self.mark_down(KeyringError::TimedOut)),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(self.mark_down(KeyringError::Failed)),
        }
    }
}

enum Raw {
    Got(String),
    Done,
    Missing,
}

impl KeyringStore {
    pub fn write(
        &self,
        id: &StoreId,
        name: &EndpointName,
        stored: &Stored,
        budget: Budget,
    ) -> Result<(), KeyringError> {
        let account = account(id, name);
        let text = encode(stored);
        self.lock().cache.remove(&account);
        let limit = self.limit(budget);
        match self.raw(Op::Set(account.clone(), text.clone()), limit, name)? {
            Raw::Done => {}
            Raw::Got(_) | Raw::Missing => return Err(KeyringError::Failed),
        }
        let unconfirmed = || KeyringError::Unconfirmed {
            name: name.clone(),
            account: account.clone(),
        };
        match self.raw(Op::Get(account.clone()), limit, name) {
            Ok(Raw::Got(back)) if back == text => {}
            Ok(Raw::Got(_) | Raw::Done | Raw::Missing) => {
                return match self.raw(Op::Delete(account.clone()), limit, name) {
                    Ok(Raw::Done | Raw::Missing) => Err(KeyringError::Failed),
                    Ok(Raw::Got(_)) | Err(_) => Err(unconfirmed()),
                };
            }
            Err(_) => return Err(unconfirmed()),
        }
        self.lock()
            .cache
            .insert(account, Cached::Present(stored.clone()));
        Ok(())
    }

    pub fn delete(
        &self,
        id: &StoreId,
        name: &EndpointName,
        budget: Budget,
    ) -> Result<bool, KeyringError> {
        let account = account(id, name);
        let deleted = match self.raw(Op::Delete(account.clone()), self.limit(budget), name)? {
            Raw::Done => true,
            Raw::Missing => false,
            Raw::Got(_) => return Err(KeyringError::Failed),
        };
        self.lock().cache.insert(account, Cached::Absent);
        Ok(deleted)
    }
}

pub trait KeyringAccess {
    fn fixed(&self) -> Option<KeyringError> {
        None
    }
    fn begin_phase(&self) {}
    fn read(&self, id: &StoreId, name: &EndpointName) -> Result<Option<Stored>, KeyringError>;
    fn write(&self, id: &StoreId, name: &EndpointName, stored: &Stored)
    -> Result<(), KeyringError>;
    fn delete(&self, id: &StoreId, name: &EndpointName) -> Result<bool, KeyringError>;
}

impl KeyringAccess for KeyringStore {
    fn fixed(&self) -> Option<KeyringError> {
        self.down()
    }

    fn begin_phase(&self) {
        KeyringStore::begin_phase(self);
    }

    fn read(&self, id: &StoreId, name: &EndpointName) -> Result<Option<Stored>, KeyringError> {
        KeyringStore::read(self, id, name, Budget::Start)
    }

    fn write(
        &self,
        id: &StoreId,
        name: &EndpointName,
        stored: &Stored,
    ) -> Result<(), KeyringError> {
        KeyringStore::write(self, id, name, stored, Budget::Start)
    }

    fn delete(&self, id: &StoreId, name: &EndpointName) -> Result<bool, KeyringError> {
        KeyringStore::delete(self, id, name, Budget::Start)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unreachable(pub KeyringError);

impl KeyringAccess for Unreachable {
    fn fixed(&self) -> Option<KeyringError> {
        Some(self.0.clone())
    }

    fn read(&self, _: &StoreId, _: &EndpointName) -> Result<Option<Stored>, KeyringError> {
        Err(self.0.clone())
    }

    fn write(&self, _: &StoreId, _: &EndpointName, _: &Stored) -> Result<(), KeyringError> {
        Err(self.0.clone())
    }

    fn delete(&self, _: &StoreId, _: &EndpointName) -> Result<bool, KeyringError> {
        Err(self.0.clone())
    }
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum MemoryMode {
    #[default]
    Working,
    Failing(BackendError),
    Unopenable,
    Hanging,
    Locked,
    ReadsHang,
    Garbling,
    ReadsFail(BackendError),
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct MemoryState {
    pub items: BTreeMap<String, String>,
    pub mode: MemoryMode,
    pub opened: usize,
    pub calls: usize,
    pub on_runtime_thread: Vec<bool>,
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Debug, Default)]
pub struct MemoryKeyring {
    state: std::sync::Arc<Mutex<MemoryState>>,
}

#[cfg(any(test, feature = "test-support"))]
impl MemoryKeyring {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> MutexGuard<'_, MemoryState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn set_mode(&self, mode: MemoryMode) {
        self.state().mode = mode;
    }

    pub fn items(&self) -> BTreeMap<String, String> {
        self.state().items.clone()
    }

    pub fn insert(&self, account: &str, secret: &str) {
        self.state()
            .items
            .insert(account.to_owned(), secret.to_owned());
    }

    pub fn opener(&self) -> Opener {
        let keyring = self.clone();
        Box::new(move || {
            let backend = MemoryBackend { keyring };
            let mode = backend.keyring.state().mode.clone();
            backend.keyring.state().opened += 1;
            match mode {
                MemoryMode::Unopenable => Err(BackendError::PlatformFailure),
                _ => Ok(Box::new(backend) as Box<dyn KeyringBackend>),
            }
        })
    }

    pub fn store(&self, deadline: Duration) -> KeyringStore {
        KeyringStore::new(self.opener(), deadline)
    }

    pub fn store_on(&self, deadline: Duration, clock: FakeClock) -> KeyringStore {
        KeyringStore::with_clock(self.opener(), deadline, clock)
    }
}

#[cfg(any(test, feature = "test-support"))]
struct MemoryBackend {
    keyring: MemoryKeyring,
}

#[cfg(any(test, feature = "test-support"))]
impl MemoryBackend {
    fn enter(&self) -> Result<(), BackendError> {
        let mode = {
            let mut state = self.keyring.state();
            state.calls += 1;
            let inside = tokio::runtime::Handle::try_current().is_ok();
            state.on_runtime_thread.push(inside);
            state.mode.clone()
        };
        match mode {
            MemoryMode::Working
            | MemoryMode::Unopenable
            | MemoryMode::Garbling
            | MemoryMode::ReadsHang
            | MemoryMode::ReadsFail(_) => Ok(()),
            MemoryMode::Failing(error) => Err(error),
            MemoryMode::Hanging => loop {
                std::thread::park();
            },
            MemoryMode::Locked => {
                while self.keyring.state().mode == MemoryMode::Locked {
                    std::thread::sleep(FAKE_SLICE);
                }
                Ok(())
            }
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl KeyringBackend for MemoryBackend {
    fn get(&mut self, account: &str) -> Result<String, BackendError> {
        self.enter()?;
        if self.keyring.state().mode == MemoryMode::ReadsHang {
            loop {
                std::thread::park();
            }
        }
        let state = self.keyring.state();
        let found = state.items.get(account).cloned();
        match (&state.mode, found) {
            (MemoryMode::Garbling, Some(text)) => Ok(format!("{text} ")),
            (MemoryMode::ReadsFail(error), _) => Err(*error),
            (_, found) => found.ok_or(BackendError::NoEntry),
        }
    }

    fn set(&mut self, account: &str, secret: &str) -> Result<(), BackendError> {
        self.enter()?;
        self.keyring
            .state()
            .items
            .insert(account.to_owned(), secret.to_owned());
        Ok(())
    }

    fn delete(&mut self, account: &str) -> Result<(), BackendError> {
        self.enter()?;
        match self.keyring.state().items.remove(account) {
            Some(_) => Ok(()),
            None => Err(BackendError::NoEntry),
        }
    }
}

#[cfg(test)]
mod tests;
