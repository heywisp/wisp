use wisp_plugin_api::host::Host;
use wisp_plugin_api::private_files::{
    ChangeAction, CreateCap, Expected, PrivateFileError as Error, Status,
};
use wisp_plugin_api::secret_check::Verdict;
use wisp_plugin_api::tools::{DoctorCell, DoctorRow};

use crate::note::{self, FILE_MAX, Note, SUFFIX, Scope};

const SCOPE_CAP: u32 = 128;
const SCAN_CAP: u32 = SCOPE_CAP * 2;
pub(super) const LOCAL_ONLY: &str =
    "Inspect or remove hidden notes locally in Wisp data / plugin-data / memory.";

pub(super) const WORKER_REFUSAL: &str = "Suggest the note in your report.";

pub(super) const UNAVAILABLE: &str = "Saved notes are unavailable here.";

pub(super) fn is_root(host: &dyn Host) -> bool {
    host.current_agent()
        .is_some_and(|caller| caller.parent.is_none())
}

pub(super) const NOT_FOUND: &str = "No saved note found.";

pub(super) struct Inspected {
    pub(super) note: Note,
    pub(super) raw: String,
    pub(super) body: String,
}

#[derive(Default)]
pub(super) struct Scan {
    pub(super) saved: Vec<Inspected>,
    pub(super) refused: usize,
    pub(super) unreadable: usize,
    pub(super) at_cap: bool,
    capacity_unknown: bool,
}

pub(super) struct Store<'a> {
    pub(super) host: &'a dyn Host,
    pub(super) root: bool,
    project: Option<String>,
    pub(super) project_probe: Result<bool, Error>,
}

impl<'a> Store<'a> {
    pub(super) fn reading(host: &'a dyn Host) -> Self {
        let project_probe = match host.private_files().list("projects", 1) {
            Ok(entries) => Ok(!entries.is_empty()),
            Err(Error::OverLimit) => Ok(true),
            Err(Error::NotFound) => Ok(false),
            Err(error) => Err(error),
        };
        Self {
            host,
            root: is_root(host),
            project: (project_probe == Ok(true))
                .then(|| host.project().key())
                .flatten(),
            project_probe,
        }
    }

    pub(super) fn writing(host: &'a dyn Host, scope: Scope) -> Self {
        let root = is_root(host);
        Self {
            host,
            root,
            project_probe: Ok(false),
            project: (root && scope == Scope::Project)
                .then(|| host.project().key())
                .flatten(),
        }
    }

    fn directory(&self, scope: Scope) -> Option<String> {
        match scope {
            Scope::You => self.root.then(|| "user".to_owned()),
            Scope::Project => self.project.as_ref().map(|key| format!("projects/{key}")),
        }
    }

    pub(super) fn scan(&self) -> Vec<Note> {
        self.notes().map(|file| file.note).collect()
    }

    pub(super) fn notes(&self) -> impl Iterator<Item = Inspected> {
        Scope::ALL
            .into_iter()
            .filter_map(|scope| self.scan_scope(scope).ok().flatten())
            .flat_map(|scan| scan.saved)
    }

    pub(super) fn scan_scope(&self, scope: Scope) -> Result<Option<Scan>, Error> {
        let Some(dir) = self.directory(scope) else {
            return Ok(None);
        };
        let mut scan = Scan::default();
        let entries = match self.host.private_files().list(&dir, SCAN_CAP) {
            Ok(entries) => entries,
            Err(Error::NotFound) => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut regular = 0;
        let mut ambiguous = 0;
        for entry in entries {
            let Some(name) = entry.name.strip_suffix(SUFFIX) else {
                continue;
            };
            regular += u32::from(entry.status == Status::Regular);
            ambiguous += u32::from(entry.status == Status::Refused);
            if !note::slug(name) {
                scan.unreadable += 1;
                continue;
            }
            let status = if entry.status == Status::Regular {
                self.inspect_file(scope, &dir, name)
            } else if entry.status == Status::Refused {
                NoteStatus::Refused
            } else {
                NoteStatus::Unreadable
            };
            match status {
                NoteStatus::Valid(file) => scan.saved.push(file),
                NoteStatus::Refused => scan.refused += 1,
                NoteStatus::Unreadable => scan.unreadable += 1,
                NoteStatus::Absent => (),
                NoteStatus::Unavailable => return Err(Error::Unavailable),
            }
        }
        scan.at_cap = regular >= SCOPE_CAP;
        scan.capacity_unknown = !scan.at_cap && regular + ambiguous >= SCOPE_CAP;
        scan.saved
            .sort_unstable_by(|a, b| a.note.id.name.cmp(&b.note.id.name));
        Ok(Some(scan))
    }

    pub(super) fn inspect(&self, scope: Scope, name: &str) -> NoteStatus {
        if scope == Scope::Project
            && let Err(error) = self.project_probe
        {
            return if error == Error::Unavailable {
                NoteStatus::Unavailable
            } else {
                NoteStatus::Unreadable
            };
        }
        match self.directory(scope) {
            Some(dir) if note::slug(name) => self.inspect_file(scope, &dir, name),
            _ => NoteStatus::Absent,
        }
    }

    fn inspect_file(&self, scope: Scope, dir: &str, name: &str) -> NoteStatus {
        if !safe_name(self.host, scope, name) {
            return NoteStatus::Refused;
        }
        let raw = match self.host.private_files().read(&path(dir, name), FILE_MAX) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(raw) => raw,
                Err(_) => return NoteStatus::Unreadable,
            },
            Err(Error::NotFound) => return NoteStatus::Absent,
            Err(Error::Refused) => return NoteStatus::Refused,
            Err(Error::Unavailable) => return NoteStatus::Unavailable,
            Err(_) => return NoteStatus::Unreadable,
        };
        if !safe(self.host, &raw) {
            return NoteStatus::Refused;
        }
        match note::decode(scope, name, &raw) {
            Some((note, body)) if safe_content(self.host, &note.fact, &body) => {
                NoteStatus::Valid(Inspected { note, raw, body })
            }
            Some(_) => NoteStatus::Refused,
            None => NoteStatus::Unreadable,
        }
    }

    pub(super) fn save(
        &self,
        scope: Scope,
        name: &str,
        bytes: Vec<u8>,
    ) -> Result<bool, &'static str> {
        let dir = self
            .directory(scope)
            .ok_or("This chat has no project memory.")?;
        let (expected, cap) = match self.inspect_file(scope, &dir, name) {
            NoteStatus::Absent => (
                Expected::Absent,
                Some(CreateCap {
                    suffix: SUFFIX.to_owned(),
                    max_regular: SCOPE_CAP,
                }),
            ),
            NoteStatus::Valid(file) => (Expected::hash(file.note.id.sha256, FILE_MAX), None),
            NoteStatus::Unavailable => return Err(UNAVAILABLE),
            NoteStatus::Refused | NoteStatus::Unreadable => return Err(LOCAL_ONLY),
        };
        let replacing = cap.is_none();
        self.host
            .private_files()
            .change(&path(&dir, name), expected, ChangeAction::Write(bytes), cap)
            .map_err(map_error)?;
        Ok(replacing)
    }

    pub(super) fn forget(&self, scope: Scope, name: &str) -> Result<(), &'static str> {
        let file = match self.inspect(scope, name) {
            NoteStatus::Valid(file) => file,
            NoteStatus::Absent => return Err(NOT_FOUND),
            NoteStatus::Unavailable => return Err(UNAVAILABLE),
            NoteStatus::Refused | NoteStatus::Unreadable => return Err(LOCAL_ONLY),
        };
        let dir = self.directory(scope).ok_or(NOT_FOUND)?;
        self.host
            .private_files()
            .change(
                &path(&dir, name),
                Expected::hash(file.note.id.sha256, FILE_MAX),
                ChangeAction::Delete,
                None,
            )
            .map_err(map_error)
    }
}

pub(super) enum NoteStatus {
    Absent,
    Valid(Inspected),
    Refused,
    Unreadable,
    Unavailable,
}

fn path(dir: &str, name: &str) -> String {
    format!("{dir}/{name}{SUFFIX}")
}

pub(super) fn safe(host: &dyn Host, text: &str) -> bool {
    host.secret_check().check(text) == Verdict::Safe
}

pub(super) fn safe_content(host: &dyn Host, fact: &str, body: &str) -> bool {
    safe(host, fact) && (body.is_empty() || safe(host, body))
}

pub(super) fn safe_name(host: &dyn Host, scope: Scope, name: &str) -> bool {
    safe(host, &format!("{name}{SUFFIX}")) && safe(host, &scope.key(name))
}

fn map_error(error: Error) -> &'static str {
    match error {
        Error::Changed => "The note changed before it could be saved or removed.",
        Error::CapReached => "This scope is full. Forget an existing note before saving a new one.",
        Error::Unavailable => UNAVAILABLE,
        Error::Refused => LOCAL_ONLY,
        _ => "Couldn't change the saved note.",
    }
}

pub(super) fn doctor(host: &dyn Host) -> Vec<DoctorRow> {
    let store = Store::reading(host);
    if store.project_probe == Err(Error::Unavailable) {
        return vec![doctor_row("memory", ["unavailable".to_owned()])];
    }
    Scope::ALL
        .into_iter()
        .filter_map(|scope| {
            let key = match scope {
                Scope::Project => "memory project",
                Scope::You => "memory you",
            };
            let scan = if scope == Scope::Project {
                store.project_probe.and_then(|_| store.scan_scope(scope))
            } else {
                store.scan_scope(scope)
            };
            Some(match scan {
                Ok(Some(scan)) => {
                    let mut counts = vec![
                        format!("{} saved", scan.saved.len()),
                        format!("{} refused", scan.refused),
                        format!("{} unreadable", scan.unreadable),
                    ];
                    if scan.at_cap {
                        counts.push("scope at cap".into());
                    } else if scan.capacity_unknown {
                        counts.push("scope capacity unknown".into());
                    }
                    doctor_row(key, counts)
                }
                Ok(None) => return None,
                Err(Error::OverLimit) => doctor_row(key, ["directory scan limit reached".into()]),
                Err(Error::Unavailable) => doctor_row(key, ["unavailable".into()]),
                Err(_) => doctor_row(key, ["directory unreadable".into()]),
            })
        })
        .collect()
}

fn doctor_row(key: &str, cells: impl IntoIterator<Item = String>) -> DoctorRow {
    DoctorRow {
        key: key.to_owned(),
        cells: cells.into_iter().map(DoctorCell::Text).collect(),
    }
}
