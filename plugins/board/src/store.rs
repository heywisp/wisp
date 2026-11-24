use std::collections::BTreeSet;
use std::fs;
use std::io::{self, ErrorKind, Write as _};
use std::path::Path;
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};

use crate::Refusal;
use crate::apply::Applied;
use crate::model::Board;
use crate::words;

pub(crate) const TRIES: usize = 8;
pub(crate) const RELIST_AFTER: SignedDuration = SignedDuration::from_secs(30);
pub(crate) const KEPT_FOR: Duration = Duration::from_secs(120);
pub(crate) const MAX_BYTES: usize = 256 * 1024;

pub(crate) struct Version {
    pub(crate) number: u64,
    pub(crate) board: Board,
    pub(crate) size: usize,
}

impl Version {
    pub(crate) fn live(self) -> Option<Board> {
        (!self.board.deleted).then_some(self.board)
    }
}

pub(crate) fn file(name: &str, number: u64) -> String {
    format!("{name}.{number}.json")
}

fn listed(folder: &Path) -> io::Result<Vec<(String, u64)>> {
    let mut tries = 1;
    loop {
        match scan(folder) {
            Err(e) if e.kind() == ErrorKind::NotFound && tries < TRIES => tries += 1,
            listing => return listing,
        }
    }
}

fn scan(folder: &Path) -> io::Result<Vec<(String, u64)>> {
    let entries = match fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut found = Vec::new();
    for entry in entries {
        let name = entry?.file_name();
        let parsed = name
            .to_str()
            .and_then(|name| name.strip_suffix(".json"))
            .and_then(|stem| stem.rsplit_once('.'))
            .and_then(|(board, n)| Some((board.to_owned(), n.parse().ok()?)));
        found.extend(parsed);
    }
    found.sort();
    Ok(found)
}

pub(crate) fn versions(folder: &Path, name: &str) -> io::Result<Vec<u64>> {
    Ok(listed(folder)?
        .into_iter()
        .filter(|(board, _)| board == name)
        .map(|(_, n)| n)
        .collect())
}

pub(crate) fn newest(folder: &Path, name: &str, given: &str) -> Result<Option<Version>, Refusal> {
    let failed = |kind| Refusal::Failed(words::unreadable(given, kind));
    for _ in 0..TRIES {
        let numbers = versions(folder, name).map_err(|e| failed(e.kind()))?;
        let Some(&number) = numbers.last() else {
            return Ok(None);
        };
        match fs::read(folder.join(file(name, number))) {
            Ok(bytes) => {
                let board =
                    serde_json::from_slice(&bytes).map_err(|_| failed(ErrorKind::InvalidData))?;
                return Ok(Some(Version {
                    number,
                    board,
                    size: bytes.len(),
                }));
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(failed(e.kind())),
        }
    }
    Err(Refusal::Failed(words::busy(given)))
}

pub(crate) fn boards(folder: &Path) -> Result<Vec<(String, Board)>, Refusal> {
    let names: BTreeSet<String> = listed(folder)
        .map_err(|e| Refusal::Failed(words::unlisted(e.kind())))?
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    let mut boards = Vec::new();
    for name in names {
        if let Some(board) = newest(folder, &name, &name)?.and_then(Version::live) {
            boards.push((name, board));
        }
    }
    Ok(boards)
}

fn link(
    folder: &Path,
    temp: &Path,
    target: &str,
    bytes: &[u8],
    fresh: impl Fn() -> bool,
) -> io::Result<bool> {
    fs::create_dir_all(folder)?;
    let _ = fs::remove_file(temp);
    let written = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp)
        .and_then(|mut out| out.write_all(bytes));
    let linked = match written {
        Ok(()) if fresh() => match fs::hard_link(temp, folder.join(target)) {
            Err(e) if e.kind() == ErrorKind::AlreadyExists => Ok(false),
            other => other.map(|()| true),
        },
        Ok(()) => Ok(false),
        Err(e) => Err(e),
    };
    let _ = fs::remove_file(temp);
    linked
}

fn clean(folder: &Path, name: &str, number: u64) {
    let modified = |n| fs::metadata(folder.join(file(name, n))).and_then(|m| m.modified());
    let Ok(linked) = modified(number) else {
        return;
    };
    for older in versions(folder, name).unwrap_or_default() {
        let stale = older < number
            && modified(older)
                .is_ok_and(|at| linked.duration_since(at).is_ok_and(|age| age > KEPT_FOR));
        if stale {
            let _ = fs::remove_file(folder.join(file(name, older)));
        }
    }
}

pub(crate) fn write(
    folder: &Path,
    name: &str,
    session: &str,
    now: &dyn Fn() -> Timestamp,
    mut change: impl FnMut(Option<&Board>, u64) -> Result<Applied, Refusal>,
) -> Result<String, Refusal> {
    let temp = folder.join(format!("{name}.{session}.tmp"));
    let mut read: Option<Option<u64>> = None;
    for _ in 0..TRIES {
        let since = now();
        let newest = newest(folder, name, name)?;
        let number = newest.as_ref().map_or(0, |v| v.number);
        let live = newest.as_ref().filter(|v| !v.board.deleted);
        match read {
            None => read = Some(live.map(|v| v.number)),
            Some(Some(first)) if live.is_none_or(|v| v.board.created > first) => {
                return Err(Refusal::Rule(words::deleted_meanwhile(name)));
            }
            Some(_) => {}
        }
        let applied = change(live.map(|v| &v.board), number + 1)?;
        if !applied.changed {
            return Ok(applied.line);
        }
        let bytes = applied.board.bytes();
        if bytes.len() > MAX_BYTES && bytes.len() > live.map_or(0, |v| v.size) {
            return Err(Refusal::Rule(words::too_big(name)));
        }
        let fresh = || now().duration_since(since) <= RELIST_AFTER;
        let target = file(name, number + 1);
        match link(folder, &temp, &target, &bytes, fresh) {
            Ok(true) => {
                clean(folder, name, number + 1);
                return Ok(applied.line);
            }
            Ok(false) => {}
            Err(e) => return Err(Refusal::Failed(words::unwritable(name, e.kind()))),
        }
    }
    Err(Refusal::Failed(words::busy(name)))
}
