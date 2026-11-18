use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::session::claim_log;

use super::check_dir;

pub(crate) fn remove_old_outputs(
    temp: &Path,
    logs: &Path,
    running: &str,
    max_age: Duration,
    now: SystemTime,
) {
    let Ok(entries) = fs::read_dir(temp) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name();
        let Some(session) = session_of(&name) else {
            continue;
        };
        let path = entry.path();
        if session != running && check_dir(&path).is_ok() && is_old(&path, max_age, now) {
            remove_unclaimed(logs, session, &path);
        }
    }
}

fn session_of(name: &OsStr) -> Option<&str> {
    let id = name.to_str()?.strip_prefix("wisp-")?;
    let canonical = uuid::Uuid::try_parse(id).ok()?.hyphenated().to_string();
    (canonical == id).then_some(id)
}

fn is_old(path: &Path, max_age: Duration, now: SystemTime) -> bool {
    newest_change(path)
        .ok()
        .and_then(|newest| now.duration_since(newest).ok())
        .is_some_and(|age| age > max_age)
}

fn newest_change(path: &Path) -> io::Result<SystemTime> {
    let mut newest = fs::symlink_metadata(path)?.modified()?;
    for entry in fs::read_dir(path)? {
        newest = newest.max(entry?.metadata()?.modified()?);
    }
    Ok(newest)
}

fn remove_unclaimed(logs: &Path, session: &str, path: &Path) {
    if let Some(_log) = claim_log(logs, session) {
        let _ = fs::remove_dir_all(path);
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests;
