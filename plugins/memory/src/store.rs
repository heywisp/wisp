use wisp_plugin_api::host::Host;
use wisp_plugin_api::private_files::{
    ChangeAction, CreateCap, Expected, PrivateFileError as Error, Status,
};
use wisp_plugin_api::secret_check::Verdict;

use crate::note::{self, FILE_MAX, Note, SUFFIX, Scope};

const SCOPE_CAP: u32 = 128;
const SCAN_CAP: u32 = SCOPE_CAP * 2;
const LOCAL_ONLY: &str =
    "Can't change this note. Inspect or remove it locally in Wisp data / plugin-data / memory.";

const UNAVAILABLE: &str = "Saved notes are unavailable here.";

pub(super) fn is_root(host: &dyn Host) -> bool {
    host.current_agent()
        .is_some_and(|caller| caller.parent.is_none())
}

pub(super) struct Store<'a> {
    pub(super) host: &'a dyn Host,
    pub(super) root: bool,
    project: Option<String>,
}

impl<'a> Store<'a> {
    pub(super) fn reading(host: &'a dyn Host) -> Self {
        let has_projects = match host.private_files().list("projects", 1) {
            Ok(entries) => !entries.is_empty(),
            Err(Error::OverLimit) => true,
            Err(_) => false,
        };
        Self {
            host,
            root: is_root(host),
            project: has_projects.then(|| host.project().key()).flatten(),
        }
    }

    pub(super) fn writing(host: &'a dyn Host, scope: Scope) -> Self {
        let root = is_root(host);
        Self {
            host,
            root,
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
        [Scope::Project, Scope::You]
            .into_iter()
            .flat_map(|scope| self.scan_scope(scope))
            .collect()
    }

    fn scan_scope(&self, scope: Scope) -> Vec<Note> {
        let Some(dir) = self.directory(scope) else {
            return Vec::new();
        };
        let Ok(entries) = self.host.private_files().list(&dir, SCAN_CAP) else {
            return Vec::new();
        };
        entries
            .into_iter()
            .filter(|entry| entry.status == Status::Regular)
            .filter_map(|entry| {
                let name = entry.name.strip_suffix(SUFFIX)?;
                if !note::slug(name) {
                    return None;
                }
                match self.inspect_file(scope, &dir, name) {
                    NoteStatus::Valid(note) => Some(note),
                    _ => None,
                }
            })
            .collect()
    }

    pub(super) fn inspect(&self, scope: Scope, name: &str) -> NoteStatus {
        match self.directory(scope) {
            Some(dir) if note::slug(name) => self.inspect_file(scope, &dir, name),
            _ => NoteStatus::Unreadable,
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
            Err(Error::Unavailable) => return NoteStatus::Unavailable,
            Err(_) => return NoteStatus::Unreadable,
        };
        if !safe(self.host, &raw) {
            return NoteStatus::Refused;
        }
        match note::decode(scope, name, &raw) {
            Some((note, body)) if safe_content(self.host, &note.fact, &body) => {
                NoteStatus::Valid(note)
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
            NoteStatus::Valid(note) => (Expected::hash(note.id.sha256, FILE_MAX), None),
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
        let dir = self
            .directory(scope)
            .ok_or("This chat has no project memory.")?;
        let note = match self.inspect_file(scope, &dir, name) {
            NoteStatus::Valid(note) => note,
            NoteStatus::Absent => return Err("No saved note found."),
            NoteStatus::Unavailable => return Err(UNAVAILABLE),
            NoteStatus::Refused | NoteStatus::Unreadable => return Err(LOCAL_ONLY),
        };
        self.host
            .private_files()
            .change(
                &path(&dir, name),
                Expected::hash(note.id.sha256, FILE_MAX),
                ChangeAction::Delete,
                None,
            )
            .map_err(map_error)
    }
}

pub(super) enum NoteStatus {
    Absent,
    Valid(Note),
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
