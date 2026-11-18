use std::io::{self, Read};

use super::{
    JsonlStore, Line, LockedFile, LogLock, OpenLog, invalid_id, open_existing, open_read_nofollow,
    read_lines_parallel, valid_id,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeLock {
    Required,
    Optional,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Reopened {
    pub lines: Vec<Line>,
    pub next_seq: u64,
    pub truncated_bytes: u64,
    pub lock: LogLock,
}

impl JsonlStore {
    pub fn probe(&self, session: &str) -> io::Result<LogLock> {
        if !valid_id(session) {
            return Err(invalid_id());
        }
        let probed = LockedFile::take(open_read_nofollow(&self.path(session))?, session)?;
        Ok(probed.lock.clone())
    }

    pub fn reopen(&mut self, session: &str, required: ResumeLock) -> io::Result<Reopened> {
        if !valid_id(session) {
            return Err(invalid_id());
        }
        let path = self.path(session);
        let log = LockedFile::take(open_existing(&path)?, session)?;
        let lock = log.lock.clone();
        if let (LogLock::Unsupported(reason), ResumeLock::Required) = (&lock, required) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "this session's log is on a filesystem without file locks ({reason}), so Wisp can't tell whether it is still open"
                ),
            ));
        }
        let mut bytes = Vec::new();
        open_read_nofollow(&path)?.read_to_end(&mut bytes)?;
        let end = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        let parsed = read_lines_parallel(&bytes[..end]);
        if parsed.newer > 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "a newer Wisp wrote {} of this log's records, so only that version can resume it",
                    parsed.newer
                ),
            ));
        }
        let kept = u64::try_from(end).unwrap_or(u64::MAX);
        let truncated_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX) - kept;
        self.open = Some(OpenLog {
            session: session.to_owned(),
            log,
            cut: (truncated_bytes > 0).then_some(kept),
        });
        let next_seq = u64::try_from(parsed.lines.len()).unwrap_or(u64::MAX);
        Ok(Reopened {
            lines: parsed.lines,
            next_seq,
            truncated_bytes,
            lock,
        })
    }
}
