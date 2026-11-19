use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use wisp_protocol::plan_usage::{PlanUsage, PlanUsageError};
use wisp_protocol::valid_endpoint_name;

const LARGEST: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct Record {
    pub(super) reading: Option<PlanUsage>,
    pub(super) observed: Option<i64>,
    pub(super) next_attempt: i64,
    pub(super) failures: u32,
    pub(super) failure: Option<Failure>,
    pub(super) checked: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Failure {
    pub(super) error: PlanUsageError,
    pub(super) at: i64,
}

pub(crate) enum Claim {
    Held(File),
    Busy,
    Unshared,
}

pub(crate) fn remove(folder: &Path, alias: &str) -> std::io::Result<()> {
    let path = path(folder, alias, "json").ok_or(std::io::ErrorKind::InvalidInput)?;
    match std::fs::remove_file(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn path(folder: &Path, alias: &str, extension: &str) -> Option<PathBuf> {
    let digest = alias.len() == 64 && alias.bytes().all(|b| b.is_ascii_hexdigit());
    (valid_endpoint_name(alias) || digest).then(|| folder.join(format!("{alias}.{extension}")))
}

pub(crate) fn read<T: DeserializeOwned>(folder: &Path, alias: &str) -> Option<T> {
    read_limited(folder, alias, LARGEST)
}

pub(crate) fn read_limited<T: DeserializeOwned>(
    folder: &Path,
    alias: &str,
    largest: u64,
) -> Option<T> {
    try_read_limited(folder, alias, largest).ok().flatten()
}

pub(crate) fn try_read<T: DeserializeOwned>(folder: &Path, alias: &str) -> io::Result<Option<T>> {
    try_read_limited(folder, alias, LARGEST)
}

fn try_read_limited<T: DeserializeOwned>(
    folder: &Path,
    alias: &str,
    largest: u64,
) -> io::Result<Option<T>> {
    let path = path(folder, alias, "json").ok_or(io::ErrorKind::InvalidInput)?;
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(largest).read_to_end(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes).ok())
}

pub(crate) fn write<T: Serialize>(folder: &Path, alias: &str, record: &T) -> std::io::Result<()> {
    let Some(target) = path(folder, alias, "json") else {
        return Ok(());
    };
    let bytes = serde_json::to_vec_pretty(record).map_err(std::io::Error::other)?;
    crate::secrets::replace_atomic(&target, &bytes, 0o600, || Ok(()))
}

pub(crate) fn claim(folder: &Path, alias: &str) -> Claim {
    let Some(target) = path(folder, alias, "lock") else {
        return Claim::Unshared;
    };
    let opened = crate::secrets::create_folder(folder).and_then(|()| {
        let mut options = std::fs::OpenOptions::new();
        options.create(true).truncate(false).write(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options.open(target)
    });
    let Ok(file) = opened else {
        return Claim::Unshared;
    };
    match crate::file_lock::try_lock(&file) {
        Ok(()) => Claim::Held(file),
        Err(std::fs::TryLockError::WouldBlock) => Claim::Busy,
        Err(_) => Claim::Unshared,
    }
}
