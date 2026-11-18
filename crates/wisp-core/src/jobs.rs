#[cfg(unix)]
pub mod exec;
pub mod keys;
pub mod read;
pub mod restored;
pub mod screen;
pub mod search;
#[cfg(unix)]
mod service;
pub mod state;
mod table;
pub mod wait;
pub mod write;

use wisp_plugin_api::host::{self, JobEnd};
use wisp_protocol::{LostReason, ProcessEnd, StoppedBy};

use crate::call::exit_status;

#[cfg(unix)]
pub use service::{JobRef, Jobs, SEARCH_STOPPED, Scope};
pub use state::{Cursor, Ended};
pub use table::{
    CallEnd, Carrier, Handle, Heard, Job, Kept, Processes, Restored, RestoredEnd, SHUTDOWN_LIMIT,
    SinkSlot, StopRefusal,
};

pub const REGEX_LIMIT: usize = 1024 * 1024;

pub fn job_end(end: &ProcessEnd) -> JobEnd {
    match end {
        ProcessEnd::Exited { exit } => JobEnd::Exited(exit_status(*exit)),
        ProcessEnd::Stopped { by } => JobEnd::Stopped(match by {
            StoppedBy::Model => host::StoppedBy::Model,
            StoppedBy::User => host::StoppedBy::User,
            StoppedBy::SessionEnded => host::StoppedBy::SessionEnded,
            StoppedBy::TimeLimit { after_ms } => host::StoppedBy::TimeLimit(*after_ms),
        }),
        ProcessEnd::Lost { reason } => JobEnd::Lost(match reason {
            LostReason::StoppedAtResume => host::LostReason::StoppedAtResume,
            LostReason::Gone => host::LostReason::Gone,
        }),
        ProcessEnd::Failed { message } => JobEnd::Failed(message.clone()),
    }
}

#[cfg(unix)]
pub fn doctor_rows() -> Vec<wisp_protocol::DoctorRow> {
    use crate::workspace::home_shown;
    let spawner = exec::HostSpawner::detect();
    let setsid = match (spawner.setsid(), exec::Spawner::isolation(&spawner)) {
        (Some(path), exec::Isolation::Session) => {
            format!(
                "{} (each command runs in its own session)",
                home_shown(path)
            )
        }
        _ => "not found: commands run in their own process group, and can still open the terminal"
            .to_owned(),
    };
    let ctty = if spawner.has_ctty() {
        "yes: tty commands run in a terminal".to_owned()
    } else {
        "no: tty commands are refused; install util-linux setsid".to_owned()
    };
    let output = std::env::temp_dir().join("wisp-<session id>");
    vec![
        wisp_protocol::DoctorRow::text("setsid", setsid),
        wisp_protocol::DoctorRow::text("setsid --ctty", ctty),
        wisp_protocol::DoctorRow::text("tool output", home_shown(&output)),
    ]
}

#[cfg(test)]
mod tests;
