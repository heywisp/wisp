use std::io;
use std::path::PathBuf;

use wisp_protocol::log::Rebuilt;

use crate::clock::{Clock, SystemClock};
use crate::kit::{JsonlLogs, ReopenedLog};
use crate::session::{ResumeLock, SessionLog, short_id};

use super::{LoggedSession, Providers, Recovered, Wanted, find, recover};

pub struct Request<'a> {
    pub id: Option<&'a str>,
    pub unlocked: bool,
}

impl Request<'_> {
    pub fn lock(&self) -> ResumeLock {
        if self.unlocked {
            ResumeLock::Optional
        } else {
            ResumeLock::Required
        }
    }
}

pub struct Resumed {
    pub log: SessionLog,
    pub path: PathBuf,
    pub recovered: Recovered,
    pub truncated_bytes: u64,
    pub logged: Option<LoggedSession>,
    pub rebuilt: Option<Box<Rebuilt>>,
    pub warnings: Vec<String>,
    pub unlocked: Option<String>,
}

pub fn unlocked_warning(reason: &str) -> String {
    format!(
        "the session log's filesystem doesn't support file locks ({reason}), so this session runs unlocked"
    )
}

fn reopen_error(e: &io::Error, id: &str) -> String {
    match e.kind() {
        io::ErrorKind::Unsupported => format!(
            "{e}. If you know it has ended, resume it with wisp --resume {} --resume-unlocked",
            short_id(id)
        ),
        _ => e.to_string(),
    }
}

pub fn open(
    logs: &JsonlLogs,
    request: &Request<'_>,
    here: Option<&str>,
    providers: Providers<'_>,
) -> Result<Resumed, String> {
    let mut store = logs.store()?;
    let now = SystemClock.now();
    let wanted = match (request.id, here) {
        (Some(id), _) => Wanted::Id(id),
        (None, Some(here)) => Wanted::Latest { here },
        (None, None) => {
            return Err("the working directory is unknown, so give the session's ID".to_owned());
        }
    };
    let found = find(&store, wanted)?;
    let mut warnings: Vec<String> = found
        .skipped
        .iter()
        .map(|id| format!("skipped session {id}, which is open in another Wisp"))
        .chain(
            found
                .unreadable
                .iter()
                .map(|(id, e)| format!("skipped session {id}, which can't be opened ({e})")),
        )
        .collect();
    let path = store.path(&found.id);
    let reopened = store
        .reopen(&found.id, request.lock())
        .map_err(|e| reopen_error(&e, &found.id))?;
    let ReopenedLog {
        log,
        lines,
        truncated_bytes,
        unlocked,
    } = logs.continuing(store, &found.id, reopened);
    let mut recovered = recover(lines, log.store(), providers)?;
    let (logged, rebuilt) = if recovered.fresh() {
        (None, None)
    } else {
        let (logged, rebuilt) = recovered.take_checked();
        (Some(logged?), rebuilt)
    };
    if let Some(reason) = &unlocked {
        warnings.push(unlocked_warning(reason));
        warnings.extend(logged.as_ref().and_then(|l| l.maybe_open(now)));
    }
    Ok(Resumed {
        unlocked,
        log,
        path,
        recovered,
        truncated_bytes,
        logged,
        rebuilt,
        warnings,
    })
}
