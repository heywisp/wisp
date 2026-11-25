use std::borrow::Borrow;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use wisp_protocol::log::{
    BlobSource, DamageKind, ImageRef, LOG_SCHEMA_VERSION, LogEvent, LogRecord, RebuildError,
    Rebuilt,
};
use wisp_protocol::{BlobRef, ImageType, MediaSource, Mime, valid_id};

mod redaction;

use crate::runtime::in_parallel;
use crate::secrets::scan::is_token_char;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    pub len: u64,
    pub modified: Duration,
}

impl Stamp {
    pub fn of(metadata: &fs::Metadata) -> Option<Self> {
        Some(Self {
            len: metadata.len(),
            modified: metadata
                .modified()
                .ok()?
                .duration_since(SystemTime::UNIX_EPOCH)
                .ok()?,
        })
    }
}

pub type LogReader = Box<dyn FnOnce() -> io::Result<Vec<LogRecord>> + Send>;

pub trait SessionStore: Send {
    fn append(&mut self, session: &str, record: &LogRecord) -> io::Result<()>;
    fn sync(&mut self, session: &str) -> io::Result<()>;
    fn append_spend(&mut self, _day: jiff::civil::Date, _line: &str) -> io::Result<()> {
        Ok(())
    }
    fn list(&self) -> io::Result<Vec<String>>;
    fn read(&self, session: &str) -> io::Result<Vec<LogRecord>>;
    fn reader(&self, _session: &str, _before: u64) -> Option<LogReader> {
        None
    }
    fn put_blob(&mut self, sha256: &str, media_type: &Mime, bytes: &[u8]) -> io::Result<()>;
    fn get_blob(&self, sha256: &str, media_type: &Mime) -> io::Result<Blob>;
    fn location(&self, _session: &str) -> Option<PathBuf> {
        None
    }
    fn blob_path(&self, _sha256: &str, _media_type: &Mime) -> Option<PathBuf> {
        None
    }

    fn write_dropped(
        &mut self,
        _session: &str,
        _seq: u64,
        _text: &str,
    ) -> io::Result<Option<PathBuf>> {
        Ok(None)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blob {
    pub media_type: Mime,
    pub bytes: Vec<u8>,
}

pub const MAX_BLOB_BYTES: u64 = wisp_media::MAX_IMAGE_BASE64 as u64;

const MAX_EXTENSION: usize = 8;

pub fn blob_extension(media_type: &Mime) -> Option<&str> {
    let sub = media_type.subtype();
    (sub.len() <= MAX_EXTENSION && sub.chars().all(|c| c.is_ascii_alphanumeric())).then_some(sub)
}

pub fn blob_name(sha256: &str, media_type: &Mime) -> String {
    match blob_extension(media_type) {
        Some(extension) => format!("{sha256}.{extension}"),
        None => sha256.to_owned(),
    }
}

fn legacy_image(media_type: &Mime) -> Option<ImageType> {
    [ImageType::Png, ImageType::Jpeg]
        .into_iter()
        .find(|t| t.as_str() == media_type.as_str())
}

fn valid_sha256(sha256: &str) -> bool {
    sha256.len() == 64
        && sha256
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

fn invalid_hash() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "invalid attachment hash")
}

fn check_blob(sha256: &str, bytes: &[u8]) -> io::Result<()> {
    if !valid_sha256(sha256) {
        return Err(invalid_hash());
    }
    if sha256_bytes_hex(bytes) != sha256 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("the file doesn't match its hash {sha256}"),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FolderUsage {
    pub files: u64,
    pub bytes: u64,
}

pub fn images_usage(dir: &Path) -> io::Result<Option<FolderUsage>> {
    let meta = match fs::symlink_metadata(dir) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if meta.file_type().is_symlink() {
        return Err(io::Error::other("is a symlink"));
    }
    if !meta.is_dir() {
        return Err(io::Error::other("not a folder"));
    }
    let mut usage = FolderUsage::default();
    for entry in fs::read_dir(dir)? {
        let meta = entry?.metadata()?;
        if meta.is_file() {
            usage.files += 1;
            usage.bytes = usage.bytes.saturating_add(meta.len());
        }
    }
    Ok(Some(usage))
}

struct StoreBlobs<'s> {
    store: &'s dyn SessionStore,
    transient: Option<String>,
}

impl<'s> StoreBlobs<'s> {
    fn new(store: &'s dyn SessionStore) -> Self {
        Self {
            store,
            transient: None,
        }
    }

    fn load(&mut self, wanted: &BlobRef) -> Result<String, String> {
        read_blob(self.store, wanted).map_err(|e| {
            if e.raw_os_error().is_some() && e.kind() != io::ErrorKind::NotFound {
                self.transient = Some(e.kind().to_string());
            }
            e.to_string()
        })
    }
}

impl BlobSource for StoreBlobs<'_> {
    fn image(&mut self, image: &ImageRef) -> Result<String, String> {
        let blob = BlobRef {
            media_type: image.media_type.into(),
            sha256: image.sha256.clone(),
            bytes: image.bytes,
        };
        self.load(&blob)
    }

    fn media(&mut self, blob: &BlobRef) -> Result<String, String> {
        self.load(blob)
    }

    fn sha256(&mut self, base64: &str) -> Result<String, String> {
        Ok(base64_sha256(base64))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unrebuilt {
    pub error: RebuildError,
    pub transient: Option<String>,
}

pub fn rebuild_checked<R: Borrow<LogRecord>>(
    store: &dyn SessionStore,
    records: &[R],
    start: Option<usize>,
) -> Result<Option<Rebuilt>, Unrebuilt> {
    let mut blobs = StoreBlobs::new(store);
    let records = records.iter().map(Borrow::<LogRecord>::borrow);
    let rebuilt = match start {
        Some(start) => wisp_protocol::log::rebuild_log_from(records, start, &mut blobs),
        None => wisp_protocol::log::rebuild_log_with(records, &mut blobs).map(Some),
    };
    rebuilt.map_err(|error| Unrebuilt {
        error,
        transient: blobs.transient.take(),
    })
}

pub fn base64_sha256(base64: &str) -> String {
    match wisp_media::decode_base64(base64) {
        Ok(bytes) => sha256_bytes_hex(&bytes),
        Err(_) => sha256_bytes_hex(base64.as_bytes()),
    }
}

pub fn blob_ref(source: &MediaSource) -> Option<BlobRef> {
    base64_ref(source.media_type(), source.data())
}

pub(crate) fn base64_ref(media_type: &Mime, data: &str) -> Option<BlobRef> {
    let bytes = wisp_media::decode_base64(data).ok()?;
    Some(BlobRef {
        media_type: media_type.clone(),
        sha256: sha256_bytes_hex(&bytes),
        bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
    })
}

pub fn rebuild<'a>(
    store: &dyn SessionStore,
    events: impl IntoIterator<Item = &'a LogEvent>,
) -> Result<Rebuilt, RebuildError> {
    wisp_protocol::log::rebuild_history_with(events, StoreBlobs::new(store))
}

fn load_blob(store: &dyn SessionStore, wanted: &BlobRef) -> Result<String, String> {
    StoreBlobs::new(store).load(wanted)
}

pub(crate) fn read_blob(store: &dyn SessionStore, wanted: &BlobRef) -> io::Result<String> {
    let blob = store.get_blob(&wanted.sha256, &wanted.media_type)?;
    let len = u64::try_from(blob.bytes.len()).unwrap_or(u64::MAX);
    if len != wanted.bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "the file holds {len} bytes, but the log says {}",
                wanted.bytes
            ),
        ));
    }
    Ok(wisp_media::encode_base64(&blob.bytes))
}

pub fn session_uuid(text: &str) -> Option<String> {
    if !matches!(text.len(), 32 | 36) {
        return None;
    }
    Some(uuid::Uuid::try_parse(text).ok()?.hyphenated().to_string())
}

pub fn new_session_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

pub fn now_rfc3339() -> String {
    rfc3339(jiff::Timestamp::now())
}

pub fn rfc3339(at: jiff::Timestamp) -> String {
    at.strftime("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

pub fn sha256_hex(text: &str) -> String {
    sha256_bytes_hex(text.as_bytes())
}

pub fn sha256_bytes_hex(bytes: &[u8]) -> String {
    wisp_plugin_api::hex::encode(&Sha256::digest(bytes))
}

pub fn sha256_json_hex(value: &impl serde::Serialize) -> Option<String> {
    let mut hasher = HashWriter(Sha256::new());
    serde_json::to_writer(&mut hasher, value).ok()?;
    let digest = wisp_plugin_api::hex::encode(&hasher.0.finalize());
    #[cfg(test)]
    tests::matches_the_buffered_digest(value, &digest);
    Some(digest)
}

struct HashWriter(Sha256);

impl Write for HashWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn invalid_id() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        wisp_protocol::INVALID_SESSION_ID,
    )
}

pub(crate) fn known_type(kind: &str) -> bool {
    !matches!(
        serde_json::from_value(serde_json::json!({ "type": kind })),
        Ok(LogEvent::Unknown)
    )
}

fn corrupt(line: usize, why: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("line {line}: {why}"))
}

#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    pub records: Vec<LogRecord>,
    pub lines: u64,
    pub newer: u64,
}

pub fn parse_jsonl(bytes: impl AsRef<[u8]>) -> io::Result<Vec<LogRecord>> {
    parse_counted(bytes.as_ref()).map(|parsed| parsed.records)
}

pub fn parse_counted(bytes: &[u8]) -> io::Result<Parsed> {
    let lines: Vec<(usize, &[u8])> = complete_lines(bytes).collect();
    let mut records = Vec::new();
    let mut newer_lines = 0;
    for (i, &(n, line)) in lines.iter().enumerate() {
        let next_seq = u64::try_from(i).unwrap_or(u64::MAX);
        let read = read_line(line).map_err(|e| corrupt(n, e))?;
        if read.seq != Some(next_seq) {
            return Err(corrupt(
                n,
                format!("expected seq {next_seq}, found {:?}", read.seq),
            ));
        }
        newer_lines += u64::from(read.newer);
        match read.kept {
            Kept::Record(record) => records.push(*record),
            Kept::Skip => {}
            Kept::Invalid(why) => return Err(corrupt(n, why)),
        }
    }
    Ok(Parsed {
        records,
        lines: u64::try_from(lines.len()).unwrap_or(u64::MAX),
        newer: newer_lines,
    })
}

fn complete_lines(bytes: &[u8]) -> impl Iterator<Item = (usize, &[u8])> {
    let complete = match bytes.iter().rposition(|b| *b == b'\n') {
        Some(end) => &bytes[..end],
        None => &[][..],
    };
    complete
        .split(|b| *b == b'\n')
        .enumerate()
        .filter(|(_, l)| !l.iter().all(u8::is_ascii_whitespace))
        .map(|(index, line)| (index + 1, line))
}

fn envelope(value: &serde_json::Value) -> (Option<u64>, Option<u64>, Option<&str>) {
    let number = |key| value.get(key).and_then(serde_json::Value::as_u64);
    let kind = value.get("type").and_then(serde_json::Value::as_str);
    (number("seq"), number("schema_version"), kind)
}

pub(crate) fn newer(version: u64) -> bool {
    version > u64::from(LOG_SCHEMA_VERSION)
}

fn unknown_kind(kind: Option<&str>) -> bool {
    kind.is_some_and(|k| !known_type(k))
}

struct LineRead {
    seq: Option<u64>,
    newer: bool,
    kept: Kept,
}

enum Kept {
    Record(Box<LogRecord>),
    Skip,
    Invalid(String),
}

fn read_line(line: &[u8]) -> Result<LineRead, String> {
    let text = std::str::from_utf8(line).map_err(|e| e.to_string())?;
    if let Ok(record) = serde_json::from_str::<LogRecord>(text)
        && !matches!(record.event, LogEvent::Unknown)
    {
        let seq = Some(record.seq);
        let is_newer = newer(u64::from(record.schema_version));
        let kept = if is_newer {
            Kept::Skip
        } else {
            Kept::Record(Box::new(record))
        };
        return Ok(LineRead {
            seq,
            newer: is_newer,
            kept,
        });
    }
    let value: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let (seq, version, kind) = envelope(&value);
    let is_newer = version.is_some_and(newer);
    let kept = if is_newer || unknown_kind(kind) {
        Kept::Skip
    } else {
        match serde_json::from_value::<LogRecord>(value) {
            Ok(record) if matches!(record.event, LogEvent::Unknown) => {
                Kept::Invalid("a record type given as a number".to_owned())
            }
            Ok(record) => Kept::Record(Box::new(record)),
            Err(e) => Kept::Invalid(e.to_string()),
        }
    };
    Ok(LineRead {
        seq,
        newer: is_newer,
        kept,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub enum Line {
    Record(Box<LogRecord>),
    Misplaced(Box<LogRecord>),
    Damaged(DamageKind),
    Skipped,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Lines {
    pub lines: Vec<Line>,
    pub newer: u64,
}

pub fn read_lines(bytes: &[u8]) -> Lines {
    read_lines_where(bytes, |_| true)
}

pub fn read_lines_where(bytes: &[u8], keep: impl Fn(&str) -> bool) -> Lines {
    let mut lines = Vec::new();
    let mut newer_lines = 0;
    for (_, line) in complete_lines(bytes) {
        let (line, newer) = read_one(lines.len(), line, &keep);
        newer_lines += u64::from(newer);
        lines.push(line);
    }
    Lines {
        lines,
        newer: newer_lines,
    }
}

fn read_one(index: usize, line: &[u8], keep: impl Fn(&str) -> bool) -> (Line, bool) {
    if head_kind(line).is_some_and(|kind| !keep(kind)) {
        return (Line::Skipped, false);
    }
    let index = u64::try_from(index).unwrap_or(u64::MAX);
    let Ok(read) = read_line(line) else {
        return (Line::Damaged(DamageKind::Unreadable), false);
    };
    let line = match (read.seq == Some(index), read.kept) {
        _ if read.newer => Line::Skipped,
        (false, Kept::Record(record)) => Line::Misplaced(record),
        (false, _) => Line::Damaged(DamageKind::OutOfOrder),
        (true, Kept::Record(record)) => Line::Record(record),
        (true, Kept::Skip) => Line::Skipped,
        (true, Kept::Invalid(_)) => Line::Damaged(DamageKind::InvalidRecord),
    };
    (line, read.newer)
}

const PARSE_CHUNK: usize = 4096;

pub(crate) fn read_lines_parallel(bytes: &[u8]) -> Lines {
    read_lines_in(bytes, if cfg!(test) { 2 } else { PARSE_CHUNK })
}

fn read_lines_in(bytes: &[u8], chunk: usize) -> Lines {
    let all: Vec<&[u8]> = complete_lines(bytes).map(|(_, line)| line).collect();
    let starts: Vec<usize> = (0..all.len()).step_by(chunk).collect();
    let mut newer = 0;
    let lines = in_parallel(&starts, |&first| {
        (first..all.len().min(first + chunk))
            .map(|index| read_one(index, all[index], |_| true))
            .collect::<Vec<_>>()
    })
    .into_iter()
    .flatten()
    .map(|(line, was_newer)| {
        newer += u64::from(was_newer);
        line
    })
    .collect();
    Lines { lines, newer }
}

pub(crate) fn reread(records: &[LogRecord]) -> io::Result<Vec<Line>> {
    let text = records
        .iter()
        .map(jsonl_line)
        .collect::<io::Result<String>>()?;
    read_lines_parallel(text.as_bytes())
        .lines
        .into_iter()
        .map(|line| match line {
            Line::Record(_) => Ok(line),
            other => Err(io::Error::other(format!(
                "not a plain log record: {other:?}"
            ))),
        })
        .collect()
}

fn head_kind(line: &[u8]) -> Option<&str> {
    let rest = line.strip_prefix(br#"{"schema_version":"#)?;
    let (version, rest) = digits(rest)?;
    if newer(version) {
        return None;
    }
    let (_, rest) = digits(rest.strip_prefix(br#","seq":"#)?)?;
    let rest = rest.strip_prefix(br#","at":""#)?;
    let at_end = rest.iter().position(|b| *b == b'"')?;
    let rest = rest[at_end..].strip_prefix(br#"","type":""#)?;
    let kind_end = rest.iter().position(|b| *b == b'"')?;
    std::str::from_utf8(&rest[..kind_end]).ok()
}

fn digits(bytes: &[u8]) -> Option<(u64, &[u8])> {
    let end = bytes
        .iter()
        .position(|b| !b.is_ascii_digit())
        .unwrap_or(bytes.len());
    let number = std::str::from_utf8(&bytes[..end]).ok()?.parse().ok()?;
    Some((number, &bytes[end..]))
}

pub fn jsonl_line(record: &LogRecord) -> io::Result<String> {
    wisp_protocol::jsonl::line(record).map_err(io::Error::other)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogLock {
    Held,
    Unsupported(String),
}

impl LogLock {
    pub(crate) fn into_unlocked(self) -> Option<String> {
        match self {
            Self::Held => None,
            Self::Unsupported(reason) => Some(reason),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreshLog {
    #[cfg(unix)]
    path: PathBuf,
    #[cfg(unix)]
    identity: (u64, u64),
}

impl FreshLog {
    #[cfg(unix)]
    fn from_created(path: PathBuf, file: &File) -> Option<Self> {
        use std::os::unix::fs::MetadataExt;
        let meta = file.metadata().ok()?;
        Some(Self {
            path,
            identity: (meta.dev(), meta.ino()),
        })
    }

    #[cfg(not(unix))]
    fn from_created(_: PathBuf, _: &File) -> Option<Self> {
        None
    }

    pub fn discard_empty(self) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if fs::symlink_metadata(&self.path).is_ok_and(|meta| {
                meta.is_file() && meta.len() == 0 && (meta.dev(), meta.ino()) == self.identity
            }) {
                let _ = fs::remove_file(&self.path);
            }
        }
    }
}

pub fn lock_state(result: Result<(), fs::TryLockError>, session: &str) -> io::Result<LogLock> {
    match result {
        Ok(()) => Ok(LogLock::Held),
        Err(fs::TryLockError::WouldBlock) => Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            format!("session {} is open in another Wisp", short_id(session)),
        )),
        Err(fs::TryLockError::Error(e)) => Ok(LogLock::Unsupported(e.to_string())),
    }
}

pub(crate) fn claim_log(dir: &Path, session: &str) -> Option<File> {
    let file = open_read_nofollow(&log_path(dir, session)).ok()?;
    crate::file_lock::try_lock(&file).ok()?;
    Some(file)
}

const IMAGES: &str = "images";
const ATTACHMENTS: &str = "attachments";
const MAX_PENDING_SPEND: usize = 64 * 1024;
const SPEND_LOCK_RETRIES: u32 = 10;

pub fn beside(sessions: &Path, name: &str) -> PathBuf {
    sessions
        .parent()
        .map_or_else(|| sessions.join(name), |p| p.join(name))
}

fn log_path(dir: &Path, session: &str) -> PathBuf {
    dir.join(format!("{session}.jsonl"))
}

#[derive(Debug)]
pub struct JsonlStore {
    dir: PathBuf,
    attachments: PathBuf,
    images: PathBuf,
    home: Option<PathBuf>,
    open: Option<OpenLog>,
    spend_open: Option<(jiff::civil::Date, File)>,
    pending_spend: BTreeMap<jiff::civil::Date, String>,
}

impl JsonlStore {
    pub fn default_dir(xdg_data_home: Option<PathBuf>, home: Option<PathBuf>) -> Option<PathBuf> {
        Self::app_dir("wisp", xdg_data_home, home)
    }

    pub fn app_dir(
        app: &str,
        xdg_data_home: Option<PathBuf>,
        home: Option<PathBuf>,
    ) -> Option<PathBuf> {
        let data = xdg_data_home
            .filter(|p| p.is_absolute())
            .or_else(|| home.map(|h| h.join(".local").join("share")))?;
        Some(data.join(app).join("sessions"))
    }

    pub fn default_images_dir(
        xdg_data_home: Option<PathBuf>,
        home: Option<PathBuf>,
    ) -> Option<PathBuf> {
        Self::default_dir(xdg_data_home, home).map(|d| beside(&d, IMAGES))
    }

    pub fn default_attachments_dir(
        xdg_data_home: Option<PathBuf>,
        home: Option<PathBuf>,
    ) -> Option<PathBuf> {
        Self::default_dir(xdg_data_home, home).map(|d| beside(&d, ATTACHMENTS))
    }

    pub fn default_usage_dir(
        xdg_data_home: Option<PathBuf>,
        home: Option<PathBuf>,
    ) -> Option<PathBuf> {
        Self::default_dir(xdg_data_home, home).map(|d| beside(&d, "usage"))
    }

    pub fn default_models_dir(
        xdg_data_home: Option<PathBuf>,
        home: Option<PathBuf>,
    ) -> Option<PathBuf> {
        Self::default_dir(xdg_data_home, home).map(|d| beside(&d, "models"))
    }

    pub fn default_plugin_data_dir(
        xdg_data_home: Option<PathBuf>,
        home: Option<PathBuf>,
    ) -> Option<PathBuf> {
        Self::default_dir(xdg_data_home, home).map(|d| beside(&d, "plugin-data"))
    }

    pub fn open(dir: impl Into<PathBuf>) -> io::Result<Self> {
        let dir = dir.into();
        create_private_dir(&dir)?;
        Ok(Self {
            attachments: beside(&dir, ATTACHMENTS),
            images: beside(&dir, IMAGES),
            home: crate::workspace::home(),
            dir,
            open: None,
            spend_open: None,
            pending_spend: BTreeMap::new(),
        })
    }

    #[must_use]
    pub fn with_home(mut self, home: Option<PathBuf>) -> Self {
        self.home = home;
        self
    }

    fn shown(&self, path: &Path) -> String {
        crate::workspace::shorten_path(path, self.home.as_deref())
    }

    #[must_use]
    pub fn with_images_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.images = dir.into();
        self
    }

    #[must_use]
    pub fn with_attachments_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.attachments = dir.into();
        self
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn images_dir(&self) -> &Path {
        &self.images
    }

    pub fn attachments_dir(&self) -> &Path {
        &self.attachments
    }

    pub fn attachment_path(&self, sha256: &str, media_type: &Mime) -> PathBuf {
        self.attachments.join(blob_name(sha256, media_type))
    }

    pub fn legacy_image_path(&self, sha256: &str, media_type: ImageType) -> PathBuf {
        self.images
            .join(format!("{sha256}.{}", media_type.extension()))
    }

    pub fn has_log(dir: &Path, session: &str) -> bool {
        valid_id(session) && fs::symlink_metadata(log_path(dir, session)).is_ok()
    }

    pub fn path(&self, session: &str) -> PathBuf {
        log_path(&self.dir, session)
    }

    fn read_path(&self, session: &str) -> io::Result<PathBuf> {
        if !valid_id(session) {
            return Err(invalid_id());
        }
        Ok(self.path(session))
    }

    fn open_read(&self, session: &str) -> io::Result<File> {
        open_regular(&self.read_path(session)?, "the log")
    }

    pub fn stamp(&self, session: &str) -> Option<Stamp> {
        Stamp::of(&self.open_read(session).ok()?.metadata().ok()?)
    }

    pub fn read_bytes(&self, session: &str) -> io::Result<Vec<u8>> {
        let mut file = self.open_read(session)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    pub fn open_session(&mut self, session: &str) -> io::Result<LogLock> {
        if let Some(open) = self.open.as_ref().filter(|o| o.session == session) {
            return Ok(open.log.lock.clone());
        }
        self.hold_session(session, open_private(&self.read_path(session)?)?)
    }

    pub fn create_session(&mut self, session: &str) -> io::Result<(LogLock, Option<FreshLog>)> {
        let path = self.read_path(session)?;
        let file = append_options(true).open(&path)?;
        let fresh = FreshLog::from_created(path, &file);
        Ok((self.hold_session(session, file)?, fresh))
    }

    fn hold_session(&mut self, session: &str, file: File) -> io::Result<LogLock> {
        let log = LockedFile::take(file, session)?;
        let lock = log.lock.clone();
        self.open = Some(OpenLog {
            session: session.to_owned(),
            log,
            cut: None,
        });
        Ok(lock)
    }

    fn pending_spend_bytes(&self) -> usize {
        self.pending_spend.values().map(String::len).sum()
    }

    fn flush_spend(&mut self) -> io::Result<()> {
        let mut first_error = None;
        for (&day, lines) in &mut self.pending_spend {
            let result = (|| {
                if !self.spend_open.as_ref().is_some_and(|(open_day, file)| {
                    *open_day == day && unlinked(file).is_ok_and(|gone| !gone)
                }) {
                    self.spend_open = None;
                    let folder = crate::spend::folder(&self.dir);
                    refuse_link(&folder, self.home.as_deref())?;
                    create_private_dir(&folder)?;
                    self.spend_open =
                        Some((day, open_private(&crate::spend::file(&self.dir, day))?));
                }
                let Some((_, file)) = &mut self.spend_open else {
                    return Err(io::Error::other("spend file not open"));
                };
                append_spend_batch(file, lines)
            })();
            if let Err(error) = result {
                self.spend_open = None;
                first_error.get_or_insert(error);
            }
        }
        self.pending_spend.retain(|_, lines| !lines.is_empty());
        first_error.map_or(Ok(()), Err)
    }

    fn file(&mut self, session: &str) -> io::Result<&mut File> {
        self.open_session(session)?;
        let Some(open) = &mut self.open else {
            return Err(io::Error::other("session file not open"));
        };
        if let Some(len) = open.cut {
            open.log.file.set_len(len)?;
            open.log.file.sync_data()?;
            open.cut = None;
        }
        Ok(&mut open.log.file)
    }
}

#[derive(Debug)]
struct OpenLog {
    session: String,
    log: LockedFile,
    cut: Option<u64>,
}

#[derive(Debug)]
struct LockedFile {
    file: File,
    lock: LogLock,
}

impl LockedFile {
    fn take(file: File, session: &str) -> io::Result<LockedFile> {
        let lock = lock_state(crate::file_lock::try_lock(&file), session)?;
        Ok(LockedFile { file, lock })
    }
}

impl Drop for LockedFile {
    fn drop(&mut self) {
        if self.lock == LogLock::Held {
            let _ = self.file.unlock();
        }
    }
}

fn append_options(create: bool) -> OpenOptions {
    let mut o = OpenOptions::new();
    o.read(true).append(true);
    if create {
        o.create_new(true);
    }
    #[cfg(unix)]
    {
        use rustix::fs::OFlags;
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
        let flags = if create {
            OFlags::NOFOLLOW
        } else {
            OFlags::NOFOLLOW | OFlags::NONBLOCK
        };
        o.custom_flags(flags.bits().cast_signed());
    }
    o
}

fn open_private(path: &Path) -> io::Result<File> {
    match append_options(true).open(path) {
        Ok(file) => Ok(file),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => open_existing(path),
        Err(e) => Err(e),
    }
}

fn open_existing(path: &Path) -> io::Result<File> {
    let file = append_options(false).open(path)?;
    check_existing(&file, "session log")?;
    Ok(file)
}

#[cfg(unix)]
pub(crate) fn check_regular_owned(file: &File, what: &str) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::other(format!("{what} is not a regular file")));
    }
    if meta.uid() != rustix::process::geteuid().as_raw() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{what} is owned by another user"),
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn check_regular_owned(file: &File, what: &str) -> io::Result<()> {
    if file.metadata()?.is_file() {
        Ok(())
    } else {
        Err(io::Error::other(format!("{what} is not a regular file")))
    }
}

pub(crate) fn tighten_file(file: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = file;
    Ok(())
}

fn check_existing(file: &File, what: &str) -> io::Result<()> {
    check_regular_owned(file, what)?;
    tighten_file(file)
}

#[cfg(unix)]
fn unlinked(file: &File) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    Ok(file.metadata()?.nlink() == 0)
}

#[cfg(not(unix))]
fn unlinked(_file: &File) -> io::Result<bool> {
    Ok(false)
}

fn lock_spend(file: &File) -> io::Result<LogLock> {
    let mut retries = 0;
    loop {
        let result = crate::file_lock::try_lock(file);
        if matches!(result, Err(fs::TryLockError::WouldBlock)) && retries < SPEND_LOCK_RETRIES {
            retries += 1;
            std::thread::sleep(std::time::Duration::from_millis(2));
        } else {
            return lock_state(result, "spend");
        }
    }
}

fn write_counted(writer: &mut impl Write, bytes: &[u8], written: &mut usize) -> io::Result<()> {
    while *written < bytes.len() {
        match writer.write(&bytes[*written..]) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => *written += n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn retain_unwritten(lines: &mut String, written: usize) {
    let mut complete = 0;
    for line in lines.split_inclusive('\n') {
        if complete + line.len() > written.saturating_add(1) {
            break;
        }
        complete += line.len();
    }
    lines.drain(..complete);
}

fn append_spend_batch(file: &mut File, lines: &mut String) -> io::Result<()> {
    let held = lock_spend(file)? == LogLock::Held;
    let result = (|| {
        let len = file.metadata()?.len();
        let mut last = *b"\n";
        if len > 0 {
            file.seek(SeekFrom::Start(len - 1))?;
            file.read_exact(&mut last)?;
        }
        let prefix = usize::from(last[0] != b'\n');
        let batch = format!("{}{lines}", if prefix == 0 { "" } else { "\n" });
        let mut written = 0;
        if let Err(error) = write_counted(file, batch.as_bytes(), &mut written) {
            if !held || file.set_len(len).is_err() {
                retain_unwritten(lines, written.saturating_sub(prefix));
            }
            return Err(error);
        }
        lines.clear();
        Ok(())
    })();
    let unlocked = if held { file.unlock() } else { Ok(()) };
    result.and(unlocked)
}

pub(crate) fn open_regular(path: &Path, what: &str) -> io::Result<File> {
    let file = open_read_nofollow(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other(format!("{what} is not a regular file")));
    }
    Ok(file)
}

pub(crate) fn open_read_nofollow(path: &Path) -> io::Result<File> {
    let mut o = OpenOptions::new();
    o.read(true);
    #[cfg(unix)]
    {
        use rustix::fs::OFlags;
        use std::os::unix::fs::OpenOptionsExt;
        o.custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK).bits().cast_signed());
    }
    o.open(path)
}

pub(crate) fn refuse_link(path: &Path, home: Option<&Path>) -> io::Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if meta.file_type().is_symlink() {
        return Err(io::Error::other(format!(
            "{} is a symlink; Wisp won't write through it",
            crate::workspace::shorten_path(path, home)
        )));
    }
    Ok(())
}

fn read_capped(file: File) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.take(MAX_BLOB_BYTES + 1).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_BLOB_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the file is larger than any attachment Wisp sends",
        ));
    }
    Ok(bytes)
}

pub(crate) fn private_dir(dir: &Path, home: Option<&Path>, what: &str) -> io::Result<()> {
    refuse_link(dir, home)?;
    crate::secrets::create_folder(dir)?;
    let meta = fs::symlink_metadata(dir)?;
    if !meta.is_dir() {
        return Err(io::Error::other(format!(
            "{} is not a directory",
            crate::workspace::shorten_path(dir, home)
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if meta.uid() != rustix::process::geteuid().as_raw() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{what} is owned by another user"),
            ));
        }
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    let _ = what;
    Ok(())
}

fn existing_blob_verifies(path: &Path, sha256: &str, home: Option<&Path>) -> io::Result<bool> {
    refuse_link(path, home)?;
    let file = match open_read_nofollow(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    check_existing(&file, "attachment file")?;
    let bytes = match read_capped(file) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::InvalidData => return Ok(false),
        Err(e) => return Err(e),
    };
    Ok(sha256_bytes_hex(&bytes) == sha256)
}

pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    crate::secrets::create_folder(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn read_records(path: &Path, before: u64) -> io::Result<Vec<LogRecord>> {
    let bytes = fs::read(path)?;
    let end = if before == u64::MAX {
        bytes.len()
    } else {
        let lines = complete_lines(&bytes)
            .take(usize::try_from(before).unwrap_or(usize::MAX))
            .last()
            .map_or(0, |(number, _)| number);
        bytes
            .split_inclusive(|byte| *byte == b'\n')
            .take(lines)
            .map(<[u8]>::len)
            .sum()
    };
    parse_jsonl(&bytes[..end])
}

impl SessionStore for JsonlStore {
    fn append_spend(&mut self, day: jiff::civil::Date, line: &str) -> io::Result<()> {
        if line.len() > MAX_PENDING_SPEND {
            return Err(io::Error::other("spend line exceeds 64 KiB"));
        }
        if self.pending_spend_bytes() + line.len() > MAX_PENDING_SPEND {
            let _ = self.flush_spend();
            if self.pending_spend_bytes() + line.len() > MAX_PENDING_SPEND {
                return Err(io::Error::other("spend retry buffer is full"));
            }
        }
        self.pending_spend.entry(day).or_default().push_str(line);
        self.flush_spend()
    }

    fn location(&self, session: &str) -> Option<PathBuf> {
        Some(self.path(session))
    }

    fn write_dropped(
        &mut self,
        session: &str,
        seq: u64,
        text: &str,
    ) -> io::Result<Option<PathBuf>> {
        if !valid_id(session) {
            return Err(invalid_id());
        }
        let path = self.dir.join(format!("{session}.dropped-{seq}.md"));
        let mut o = OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        {
            use rustix::fs::OFlags;
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
            o.custom_flags(OFlags::NOFOLLOW.bits().cast_signed());
        }
        match o.open(&path) {
            Ok(mut file) => {
                file.write_all(text.as_bytes())?;
                file.sync_all()?;
                Ok(Some(path))
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                check_existing(&open_read_nofollow(&path)?, "dropped messages file")?;
                Ok(Some(path))
            }
            Err(e) => Err(e),
        }
    }

    fn append(&mut self, session: &str, record: &LogRecord) -> io::Result<()> {
        let line = jsonl_line(record)?;
        let file = self.file(session)?;
        file.write_all(line.as_bytes())?;
        file.flush()
    }

    fn sync(&mut self, session: &str) -> io::Result<()> {
        let _ = self.flush_spend();
        if let Some((_, file)) = &self.spend_open {
            let _ = file.sync_data();
        }
        self.file(session)?.sync_data()
    }

    fn list(&self) -> io::Result<Vec<String>> {
        let mut ids: Vec<String> = fs::read_dir(&self.dir)?
            .filter_map(Result::ok)
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                let id = name.strip_suffix(".jsonl")?;
                valid_id(id).then(|| id.to_owned())
            })
            .collect();
        ids.sort();
        Ok(ids)
    }

    fn read(&self, session: &str) -> io::Result<Vec<LogRecord>> {
        read_records(&self.read_path(session)?, u64::MAX)
    }

    fn reader(&self, session: &str, before: u64) -> Option<LogReader> {
        let path = self.read_path(session);
        Some(Box::new(move || read_records(&path?, before)))
    }

    fn put_blob(&mut self, sha256: &str, media_type: &Mime, bytes: &[u8]) -> io::Result<()> {
        check_blob(sha256, bytes)?;
        let home = self.home.as_deref();
        private_dir(&self.attachments, home, "the attachments folder")?;
        let path = self.attachment_path(sha256, media_type);
        if existing_blob_verifies(&path, sha256, home)? {
            return Ok(());
        }
        crate::secrets::replace_atomic(&path, bytes, 0o600, || refuse_link(&path, home))
    }

    fn get_blob(&self, sha256: &str, media_type: &Mime) -> io::Result<Blob> {
        if !valid_sha256(sha256) {
            return Err(invalid_hash());
        }
        let current = self.attachment_path(sha256, media_type);
        let legacy = legacy_image(media_type).map(|t| self.legacy_image_path(sha256, t));
        for path in std::iter::once(&current).chain(legacy.as_ref()) {
            let file = match open_regular(path, &self.shown(path)) {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            let bytes = read_capped(file)?;
            check_blob(sha256, &bytes)?;
            return Ok(Blob {
                media_type: media_type.clone(),
                bytes,
            });
        }
        let name = blob_name(sha256, media_type);
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            match legacy {
                Some(old) => format!(
                    "no file named {name} in {} or {}",
                    self.shown(&self.attachments),
                    self.shown(&old)
                ),
                None => format!("no file named {name} in {}", self.shown(&self.attachments)),
            },
        ))
    }

    fn blob_path(&self, sha256: &str, media_type: &Mime) -> Option<PathBuf> {
        Some(self.attachment_path(sha256, media_type))
    }
}

type Blobs = BTreeMap<String, Blob>;

#[derive(Debug, Clone, Default)]
pub struct MemoryStore {
    sessions: Arc<Mutex<BTreeMap<String, Vec<LogRecord>>>>,
    blobs: Arc<Mutex<Blobs>>,
    fail_writes: bool,
    spend: Arc<Mutex<Vec<String>>>,
}

impl MemoryStore {
    #[cfg(test)]
    pub(crate) fn sharing_blobs(&self) -> Self {
        Self {
            sessions: Arc::default(),
            blobs: Arc::clone(&self.blobs),
            fail_writes: false,
            spend: Arc::clone(&self.spend),
        }
    }

    pub fn failing() -> Self {
        Self {
            fail_writes: true,
            ..Self::default()
        }
    }

    fn lock(&self) -> io::Result<std::sync::MutexGuard<'_, BTreeMap<String, Vec<LogRecord>>>> {
        self.sessions
            .lock()
            .map_err(|_| io::Error::other("store lock poisoned"))
    }

    fn lock_blobs(&self) -> io::Result<std::sync::MutexGuard<'_, Blobs>> {
        self.blobs
            .lock()
            .map_err(|_| io::Error::other("store lock poisoned"))
    }

    #[cfg(test)]
    pub(crate) fn edit(&self, session: &str, edit: impl FnOnce(&mut Vec<LogRecord>)) {
        if let Ok(mut sessions) = self.lock()
            && let Some(records) = sessions.get_mut(session)
        {
            edit(records);
        }
    }

    #[cfg(test)]
    pub(crate) fn remove(&self, session: &str) {
        if let Ok(mut sessions) = self.lock() {
            sessions.remove(session);
        }
    }

    pub fn spend_lines(&self) -> Vec<String> {
        self.spend
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn blob_names(&self) -> io::Result<Vec<String>> {
        Ok(self
            .lock_blobs()?
            .iter()
            .map(|(sha, blob)| blob_name(sha, &blob.media_type))
            .collect())
    }

    pub fn replace_blob(&self, sha256: &str, blob: Blob) -> io::Result<()> {
        self.lock_blobs()?.insert(sha256.to_owned(), blob);
        Ok(())
    }

    pub fn remove_blob(&self, sha256: &str) -> io::Result<()> {
        self.lock_blobs()?.remove(sha256);
        Ok(())
    }
}

impl SessionStore for MemoryStore {
    fn append_spend(&mut self, _day: jiff::civil::Date, line: &str) -> io::Result<()> {
        if self.fail_writes {
            return Err(io::Error::other("disk full"));
        }
        self.spend
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(line.to_owned());
        Ok(())
    }

    fn append(&mut self, session: &str, record: &LogRecord) -> io::Result<()> {
        if self.fail_writes {
            return Err(io::Error::other("disk full"));
        }
        self.lock()?
            .entry(session.to_owned())
            .or_default()
            .push(record.clone());
        Ok(())
    }

    fn sync(&mut self, _session: &str) -> io::Result<()> {
        Ok(())
    }

    fn list(&self) -> io::Result<Vec<String>> {
        Ok(self.lock()?.keys().cloned().collect())
    }

    fn read(&self, session: &str) -> io::Result<Vec<LogRecord>> {
        self.lock()?
            .get(session)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such session"))
    }

    fn put_blob(&mut self, sha256: &str, media_type: &Mime, bytes: &[u8]) -> io::Result<()> {
        if self.fail_writes {
            return Err(io::Error::other("disk full"));
        }
        check_blob(sha256, bytes)?;
        let mut blobs = self.lock_blobs()?;
        let reusable = blobs
            .get(sha256)
            .is_some_and(|b| b.media_type == *media_type && check_blob(sha256, &b.bytes).is_ok());
        if !reusable {
            blobs.insert(
                sha256.to_owned(),
                Blob {
                    media_type: media_type.clone(),
                    bytes: bytes.to_vec(),
                },
            );
        }
        Ok(())
    }

    fn get_blob(&self, sha256: &str, _media_type: &Mime) -> io::Result<Blob> {
        let blob = self.lock_blobs()?.get(sha256).cloned().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, format!("no attachment {sha256}"))
        })?;
        check_blob(sha256, &blob.bytes)?;
        Ok(blob)
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct Redactions(Vec<Redaction>);

#[derive(Default)]
pub(crate) struct MaskTail {
    pub(crate) bytes: Vec<u8>,
    pub(crate) covered: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RedactionKind {
    LogOnly,
    Held,
    Masked,
}

#[derive(Clone, PartialEq, Eq)]
struct Redaction {
    value: String,
    kind: RedactionKind,
}

static SECRETS_GENERATION: AtomicU64 = AtomicU64::new(0);

pub(crate) fn held_generation() -> u64 {
    SECRETS_GENERATION.load(Ordering::Acquire)
}

fn process_secrets() -> &'static std::sync::Mutex<Redactions> {
    static SECRETS: std::sync::OnceLock<std::sync::Mutex<Redactions>> = std::sync::OnceLock::new();
    SECRETS.get_or_init(std::sync::Mutex::default)
}

pub fn register(redactions: &Redactions) {
    let mut secrets = process_secrets()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let updated = secrets.union([redactions]);
    if updated != *secrets {
        *secrets = updated;
        SECRETS_GENERATION.fetch_add(1, Ordering::Release);
    }
}

pub fn register_secrets(items: impl IntoIterator<Item = String>) {
    register(&Redactions::new(items));
}

pub fn held() -> Redactions {
    process_secrets()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

pub(crate) fn without_registered_secrets(text: &str) -> String {
    held().scrub(text)
}

pub(crate) fn maskable(value: &str) -> bool {
    value.chars().count() >= wisp_protocol::KEY_SHAPED_MIN
}

pub const REDACTED: &str = "[redacted]";

pub(crate) const MIN_REDACTION_CHARS: usize = 3;

const ESCAPE_LEVELS: usize = 4;

const ESCAPE_BYTES: usize = 12;

pub(crate) const ESCAPED_HOLD: usize = 16;

fn base64_forms(value: &str) -> [String; 3] {
    [0, 1, 2].map(|leading| {
        let mut bytes = vec![0; leading];
        bytes.extend_from_slice(value.as_bytes());
        bytes.truncate(bytes.len() / 3 * 3);
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        encoded[[0, 2, 3][leading]..].to_owned()
    })
}

fn redacted_text(text: &str, bytes: Option<Vec<u8>>) -> String {
    bytes.map_or_else(
        || text.to_owned(),
        |bytes| String::from_utf8_lossy(&bytes).into_owned(),
    )
}

fn found(
    entries: &[&[u8]],
    bytes: &[u8],
    cut: usize,
    mut runs: Vec<std::ops::Range<usize>>,
    pending: bool,
) -> (Vec<std::ops::Range<usize>>, usize) {
    let mut boundary = cut;
    let Some(longest) = entries.first() else {
        return (runs, boundary);
    };
    let mut starts = [false; 256];
    for value in entries {
        starts[usize::from(value[0])] = true;
    }
    for at in 0..bytes.len() {
        if !starts[usize::from(bytes[at])] {
            continue;
        }
        let rest = &bytes[at..];
        let head = &rest[..cut.saturating_sub(at)];
        if pending
            && !head.is_empty()
            && head.len() < longest.len()
            && entries
                .iter()
                .any(|value| value.len() > head.len() && value.starts_with(head))
        {
            boundary = boundary.min(at);
        }
        if let Some(value) = entries.iter().find(|value| rest.starts_with(value)) {
            joined(&mut runs, at..at + value.len());
        }
    }
    (runs, boundary)
}

fn joined(runs: &mut Vec<std::ops::Range<usize>>, run: std::ops::Range<usize>) {
    if let Some(last) = runs.last_mut()
        && run.start <= last.end
    {
        last.end = last.end.max(run.end);
    } else {
        runs.push(run);
    }
}

fn spliced(
    bytes: &[u8],
    runs: &[std::ops::Range<usize>],
    boundary: usize,
    covered: usize,
) -> Option<Vec<u8>> {
    let mut out: Option<Vec<u8>> = None;
    let mut copied = 0;
    for run in runs.iter().take_while(|run| run.start < boundary) {
        let out = out.get_or_insert_with(|| Vec::with_capacity(boundary));
        out.extend_from_slice(&bytes[copied..run.start]);
        if run.end > covered {
            out.extend_from_slice(REDACTED.as_bytes());
        }
        copied = run.end.min(boundary);
    }
    if let Some(out) = &mut out {
        out.extend_from_slice(&bytes[copied..boundary]);
    }
    out
}

fn for_each_escape_view(
    bytes: &[u8],
    pending: bool,
    mut visit: impl FnMut(&[u8], usize, Option<&[usize]>) -> bool,
) -> Option<Vec<usize>> {
    let (mut view, cut) = unescaped(bytes, pending);
    if visit(bytes, cut, None) {
        return None;
    }
    let mut starts = None;
    for level in 1..=ESCAPE_LEVELS {
        let Some((decoded, origin)) = view.take() else {
            break;
        };
        let (next, cut) = if level < ESCAPE_LEVELS {
            unescaped(&decoded, pending)
        } else {
            (None, decoded.len())
        };
        let stop = visit(&decoded, cut, Some(&origin));
        if !stop {
            view = next.and_then(|(next, from)| {
                let composed: Option<Vec<_>> =
                    from.into_iter().map(|at| origin.get(at).copied()).collect();
                Some((next, composed?))
            });
        }
        starts = Some(origin);
        if stop {
            break;
        }
    }
    starts
}

fn matched(
    entries: &[&[u8]],
    bytes: &[u8],
    covered: usize,
    pending: bool,
    floor: usize,
) -> (Vec<std::ops::Range<usize>>, usize, Option<Vec<usize>>) {
    let mut runs = Vec::new();
    let mut boundary = bytes.len();
    let starts = for_each_escape_view(bytes, pending, |view, cut, origin| {
        if let Some(origin) = origin {
            let (hits, prefix) = found(entries, view, cut, Vec::new(), pending);
            runs.extend(
                hits.into_iter()
                    .filter_map(|run| Some(*origin.get(run.start)?..*origin.get(run.end)?)),
            );
            if let Some(&prefix) = origin.get(prefix) {
                boundary = boundary.min(prefix);
            }
            if pending {
                boundary = snap_escape(origin, boundary, floor);
            }
        } else {
            let covered = (covered > 0).then_some(0..covered).into_iter().collect();
            (runs, boundary) = found(entries, view, cut, covered, pending);
        }
        false
    });
    runs.sort_by_key(|run| run.start);
    let runs = runs.into_iter().fold(Vec::new(), |mut merged, run| {
        joined(&mut merged, run);
        merged
    });
    (runs, boundary, starts)
}

fn is_token_bounded(view: &[u8], start: usize, end: usize) -> bool {
    let token_at = |at: Option<usize>| {
        at.and_then(|at| view.get(at))
            .is_some_and(|&b| is_token_char(char::from(b)))
    };
    (!token_at(Some(start)) || !token_at(start.checked_sub(1)))
        && (!token_at(end.checked_sub(1)) || !token_at(Some(end)))
}

fn has_bounded_match(entries: &[&[u8]], bytes: &[u8]) -> bool {
    if entries.is_empty() {
        return false;
    }
    let mut hit = false;
    for_each_escape_view(bytes, false, |view, _, _| {
        hit = entries.iter().any(|value| {
            (0..view.len()).any(|at| {
                view.get(at..).is_some_and(|tail| tail.starts_with(value))
                    && is_token_bounded(view, at, at + value.len())
            })
        });
        hit
    });
    hit
}

fn snap_escape(starts: &[usize], at: usize, floor: usize) -> usize {
    let snapped = starts[..starts.partition_point(|&start| start <= at)]
        .last()
        .copied()
        .unwrap_or(0);
    if snapped >= floor { snapped } else { at }
}

type Decoded = (Vec<u8>, Vec<usize>);

fn unescaped(text: &[u8], open: bool) -> (Option<Decoded>, usize) {
    if !text.contains(&b'\\') {
        return (None, text.len());
    }
    let mut out = Vec::with_capacity(text.len());
    let mut origin = Vec::with_capacity(text.len() + 1);
    let mut at = 0;
    while let Some(&byte) = text.get(at) {
        match escape(&text[at..]) {
            Some((decoded, used)) => {
                out.extend_from_slice(decoded.encode_utf8(&mut [0; 4]).as_bytes());
                origin.extend(std::iter::repeat_n(at, decoded.len_utf8()));
                at += used;
            }
            None if open && byte == b'\\' && unfinished(&text[at..]) => break,
            None => {
                out.push(byte);
                origin.push(at);
                at += 1;
            }
        }
    }
    origin.push(at);
    ((out.len() < text.len()).then_some((out, origin)), at)
}

fn unfinished(tail: &[u8]) -> bool {
    tail.len() < ESCAPE_BYTES
        && [&b"\\u0000"[..], b"\\u{0}", b"\\uD800\\uDC00"]
            .iter()
            .any(|whole| {
                (0..whole.len()).any(|from| escape(&[tail, &whole[from..]].concat()).is_some())
            })
}

fn escape(text: &[u8]) -> Option<(char, usize)> {
    let rest = text.strip_prefix(b"\\")?;
    let simple = match rest.first()? {
        b'"' => '"',
        b'\\' => '\\',
        b'/' => '/',
        b'b' => '\u{8}',
        b'f' => '\u{c}',
        b'n' => '\n',
        b'r' => '\r',
        b't' => '\t',
        b'u' => return code_point(&rest[1..]).map(|(c, used)| (c, used + 2)),
        _ => return None,
    };
    Some((simple, 2))
}

fn code_point(text: &[u8]) -> Option<(char, usize)> {
    let hex = |digits: &[u8]| {
        (!digits.is_empty())
            .then(|| {
                digits.iter().try_fold(0, |value: u32, &digit| {
                    Some(value * 16 + char::from(digit).to_digit(16)?)
                })
            })
            .flatten()
    };
    if let Some(braced) = text.strip_prefix(b"{") {
        let digits = braced.get(..braced.iter().take(7).position(|&b| b == b'}')?)?;
        return hex(digits)
            .and_then(char::from_u32)
            .map(|c| (c, digits.len() + 2));
    }
    let unit = |from: usize| text.get(from..from + 4).and_then(hex);
    let high = unit(0)?;
    if let Some(c) = char::from_u32(high) {
        return Some((c, 4));
    }
    let low = unit(6).filter(|_| text.get(4..6) == Some(b"\\u".as_slice()))?;
    let pair = (0xD800..0xDC00).contains(&high) && (0xDC00..0xE000).contains(&low);
    let c = pair
        .then(|| char::from_u32(0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00)))
        .flatten()?;
    Some((c, 10))
}

impl Redactions {
    pub fn new(items: impl IntoIterator<Item = String>) -> Self {
        Self::entries(items, |value| {
            if maskable(value) {
                RedactionKind::Masked
            } else {
                RedactionKind::Held
            }
        })
    }

    pub(crate) fn typed(items: impl IntoIterator<Item = String>) -> Self {
        Self::entries(items, |_| RedactionKind::Masked)
    }

    pub(crate) fn held_only(items: impl IntoIterator<Item = String>) -> Self {
        Self::entries(items, |_| RedactionKind::Held)
    }

    pub fn log_only(items: impl IntoIterator<Item = String>) -> Self {
        Self::entries(items, |_| RedactionKind::LogOnly)
    }

    fn entries(
        items: impl IntoIterator<Item = String>,
        kind: impl Fn(&str) -> RedactionKind,
    ) -> Self {
        Self::sorted(
            items
                .into_iter()
                .filter(|s| {
                    s.chars()
                        .filter(|c| !c.is_whitespace())
                        .take(MIN_REDACTION_CHARS)
                        .count()
                        == MIN_REDACTION_CHARS
                })
                .flat_map(|s| {
                    let kind = kind(&s);
                    let escaped_spelling = |quoted: String| {
                        quoted
                            .strip_prefix('"')
                            .and_then(|text| text.strip_suffix('"'))
                            .filter(|text| *text != s)
                            .map(str::to_owned)
                    };
                    let debug = escaped_spelling(format!("{s:?}"));
                    let json = serde_json::to_string(&s).ok().and_then(escaped_spelling);
                    let encoded =
                        (kind >= RedactionKind::Held && maskable(&s)).then(|| base64_forms(&s));
                    std::iter::once(s)
                        .chain(debug)
                        .chain(json)
                        .chain(encoded.into_iter().flatten())
                        .map(move |value| Redaction { value, kind })
                }),
        )
    }

    fn sorted(items: impl Iterator<Item = Redaction>) -> Self {
        let mut items: Vec<_> = items.collect();
        items.sort_by(|a, b| {
            b.value
                .len()
                .cmp(&a.value.len())
                .then_with(|| a.value.cmp(&b.value))
                .then_with(|| b.kind.cmp(&a.kind))
        });
        items.dedup_by(|a, b| a.value == b.value);
        Self(items)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[must_use]
    pub fn union<'a>(&self, others: impl IntoIterator<Item = &'a Redactions>) -> Redactions {
        let items = others
            .into_iter()
            .flat_map(|r| r.0.iter())
            .chain(self.0.iter())
            .cloned();
        Redactions::sorted(items)
    }

    fn values(&self, min: RedactionKind) -> Vec<&[u8]> {
        self.0
            .iter()
            .filter(|entry| entry.kind >= min)
            .map(|entry| entry.value.as_bytes())
            .collect()
    }

    pub(crate) fn has_match(&self, text: &str, min: RedactionKind) -> bool {
        let entries = self.values(min);
        !entries.is_empty() && !matched(&entries, text.as_bytes(), 0, false, 0).0.is_empty()
    }

    pub(crate) fn has_secret_check_match(&self, text: &str) -> bool {
        let mut long = Vec::new();
        let mut short = Vec::new();
        for entry in self
            .0
            .iter()
            .filter(|entry| entry.kind >= RedactionKind::Held)
        {
            let values = if entry.kind == RedactionKind::Masked || maskable(&entry.value) {
                &mut long
            } else {
                &mut short
            };
            values.push(entry.value.as_bytes());
        }
        let bytes = text.as_bytes();
        (!long.is_empty() && !matched(&long, bytes, 0, false, 0).0.is_empty())
            || has_bounded_match(&short, bytes)
    }

    fn replaced(
        &self,
        bytes: &[u8],
        min: RedactionKind,
        pending: bool,
        covered: usize,
    ) -> (Option<Vec<u8>>, usize, usize) {
        let entries = self.values(min);
        let Some(longest) = entries.first() else {
            return (
                (covered > 0).then(|| bytes[covered..].to_vec()),
                bytes.len(),
                0,
            );
        };
        let hold = longest.len().saturating_mul(ESCAPED_HOLD);
        let floor = bytes.len().saturating_sub(hold);
        let (runs, mut boundary, starts) = matched(&entries, bytes, covered, pending, floor);
        if pending {
            boundary = boundary.max(floor);
        }
        let prefix = boundary;
        let active = pending
            .then(|| {
                runs.iter()
                    .find(|run| run.start <= boundary && boundary <= run.end)
            })
            .flatten();
        if let Some(run) = active {
            boundary = starts
                .as_deref()
                .map_or(run.start, |starts| snap_escape(starts, run.start, floor));
        }
        if pending
            && bytes.len() - boundary > longest.len().saturating_mul(2)
            && let Some(run) = active
        {
            let keep = (longest.len() - 1).min(run.end - run.start);
            let from = prefix.min(run.end - keep);
            let shown = |at: usize| at > run.start || covered > run.start;
            let restart = starts
                .as_deref()
                .map_or(from, |starts| snap_escape(starts, from, floor));
            if shown(restart) {
                boundary = restart;
            }
        }
        let next_covered = if pending {
            runs.iter()
                .find(|run| (run.start < boundary || covered > run.start) && boundary < run.end)
                .map_or_else(
                    || covered.saturating_sub(boundary),
                    |run| run.end - boundary,
                )
        } else {
            0
        };
        (
            spliced(bytes, &runs, boundary, covered),
            boundary,
            next_covered,
        )
    }

    pub fn scrub(&self, text: &str) -> String {
        redacted_text(
            text,
            self.replaced(text.as_bytes(), RedactionKind::LogOnly, false, 0)
                .0,
        )
    }

    pub fn mask(&self, text: &str) -> String {
        redacted_text(text, self.mask_bytes(text.as_bytes()))
    }

    pub(crate) fn mask_bytes(&self, bytes: &[u8]) -> Option<Vec<u8>> {
        self.replaced(bytes, RedactionKind::Masked, false, 0).0
    }

    fn stream(&self, bytes: &[u8], pending: &mut MaskTail, last: bool) -> (Vec<u8>, bool) {
        let mut input = std::mem::take(&mut pending.bytes);
        input.extend_from_slice(bytes);
        let (replaced, boundary, covered) =
            self.replaced(&input, RedactionKind::Masked, !last, pending.covered);
        let masked = replaced.is_some();
        pending.bytes = input.split_off(boundary);
        pending.covered = covered;
        (replaced.unwrap_or(input), masked)
    }

    pub(crate) fn mask_chunk(&self, bytes: &[u8], pending: &mut MaskTail) -> (Vec<u8>, bool) {
        self.stream(bytes, pending, false)
    }

    pub(crate) fn release(&self, pending: &mut MaskTail) -> (Vec<u8>, bool) {
        self.stream(&[], pending, true)
    }

    pub(crate) fn scrub_value(&self, value: &mut serde_json::Value) {
        match value {
            serde_json::Value::String(s) => *s = self.scrub(s),
            serde_json::Value::Array(items) => items.iter_mut().for_each(|v| self.scrub_value(v)),
            serde_json::Value::Object(map) => map.values_mut().for_each(|v| self.scrub_value(v)),
            _ => {}
        }
    }

    fn scrub_event(&self, event: LogEvent) -> LogEvent {
        redaction::scrub(self, event)
    }
}

impl std::fmt::Debug for Redactions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Redactions({} items)", self.0.len())
    }
}

pub struct SessionLog {
    store: Box<dyn SessionStore>,
    id: String,
    seq: u64,
    failed: bool,
}

impl SessionLog {
    pub fn new(store: Box<dyn SessionStore>, id: impl Into<String>) -> Self {
        Self::continuing(store, id, 0)
    }

    pub fn continuing(store: Box<dyn SessionStore>, id: impl Into<String>, seq: u64) -> Self {
        Self {
            store,
            id: id.into(),
            seq,
            failed: false,
        }
    }

    pub fn media(&self, blob: &BlobRef) -> Result<String, String> {
        load_blob(self.store.as_ref(), blob)
    }

    pub fn store(&self) -> &dyn SessionStore {
        self.store.as_ref()
    }

    pub fn write_dropped(&mut self, seq: u64, text: &str) -> io::Result<Option<PathBuf>> {
        self.store.write_dropped(&self.id, seq, text)
    }

    pub fn image(&self, image: &ImageRef) -> Result<String, String> {
        StoreBlobs::new(self.store.as_ref()).image(image)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn next_seq(&self) -> u64 {
        self.seq
    }

    pub fn location(&self) -> Option<PathBuf> {
        self.store.location(&self.id)
    }

    pub fn spend(&mut self, day: jiff::civil::Date, mut record: crate::spend::Record) -> bool {
        let redactions = held();
        record.account = redactions.scrub(&record.account);
        record.model = redactions.scrub(&record.model);
        crate::spend::line(&record)
            .and_then(|line| self.store.append_spend(day, &line))
            .is_ok()
    }

    pub fn record(&mut self, event: LogEvent) -> Option<String> {
        if self.failed {
            return None;
        }
        let event = held().scrub_event(event);
        let record = LogRecord {
            schema_version: LOG_SCHEMA_VERSION,
            seq: self.seq,
            at: now_rfc3339(),
            event,
        };
        self.seq += 1;
        let result = self.store.append(&self.id, &record);
        self.fail(result)
    }

    pub fn sync(&mut self) -> Option<String> {
        if self.failed {
            return None;
        }
        let result = self.store.sync(&self.id);
        self.fail(result)
    }

    pub fn put_blob(&mut self, sha256: &str, media_type: &Mime, bytes: &[u8]) -> Option<String> {
        if self.failed {
            return None;
        }
        let e = self.store.put_blob(sha256, media_type, bytes).err()?;
        self.failed = true;
        Some(format!(
            "attachment could not be saved ({e}); logging is off for this session"
        ))
    }

    pub fn saved_path(&self, sha256: &str, media_type: &Mime) -> Option<String> {
        if self.failed {
            return None;
        }
        self.store
            .blob_path(sha256, media_type)
            .map(|path| path.display().to_string())
    }

    pub fn is_on(&self) -> bool {
        !self.failed
    }

    fn fail(&mut self, result: io::Result<()>) -> Option<String> {
        let e = result.err()?;
        self.failed = true;
        Some(format!(
            "session log write failed ({e}); logging is off for this session"
        ))
    }
}

pub(crate) mod mirror;
mod reopen;

pub use reopen::{Reopened, ResumeLock};
pub use wisp_protocol::short_id;

#[cfg(test)]
mod tests;
