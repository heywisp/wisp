#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Regular,
    Directory,
    Refused,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub status: Status,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivateFileError {
    Unavailable,
    InvalidPath,
    NotFound,
    Refused,
    OverLimit,
    Changed,
    CapReached,
    Io,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expected {
    Absent,
    Hash(HashCheck),
}

impl Expected {
    pub fn hash(sha256: String, max_bytes: u32) -> Self {
        Self::Hash(HashCheck { sha256, max_bytes })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HashCheck {
    pub sha256: String,
    pub max_bytes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeAction {
    Write(Vec<u8>),
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateCap {
    pub suffix: String,
    pub max_regular: u32,
}

pub const LOCK_FILE: &str = ".wisp.lock";
pub const TEMP_PREFIX: &str = ".wisp-tmp-";

pub fn reserved(name: &str) -> bool {
    name.eq_ignore_ascii_case(LOCK_FILE)
        || name
            .get(..TEMP_PREFIX.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(TEMP_PREFIX))
}

pub fn component(name: &str) -> bool {
    !name.is_empty()
        && !matches!(name, "." | "..")
        && !reserved(name)
        && !name.contains(['/', '\\', ':'])
        && !name.chars().any(char::is_control)
}

pub fn components(path: &str) -> Result<Vec<&str>, PrivateFileError> {
    if path.is_empty() {
        return Ok(Vec::new());
    }
    let parts: Vec<_> = path.split('/').collect();
    if parts.iter().all(|part| component(part)) {
        Ok(parts)
    } else {
        Err(PrivateFileError::InvalidPath)
    }
}

pub fn validate_change(
    name: &str,
    before: &Expected,
    action: &ChangeAction,
    cap: Option<&CreateCap>,
) -> Result<(), PrivateFileError> {
    let creating = matches!(before, Expected::Absent);
    if (creating && !matches!(action, ChangeAction::Write(_)))
        || cap
            .is_some_and(|cap| !creating || !component(&cap.suffix) || !name.ends_with(&cap.suffix))
    {
        Err(PrivateFileError::InvalidPath)
    } else {
        Ok(())
    }
}

pub trait PrivateFiles: Send + Sync {
    fn list(&self, _path: &str, _max_entries: u32) -> Result<Vec<Entry>, PrivateFileError> {
        Err(PrivateFileError::Unavailable)
    }

    fn read(&self, _path: &str, _max_bytes: u32) -> Result<Vec<u8>, PrivateFileError> {
        Err(PrivateFileError::Unavailable)
    }

    fn change(
        &self,
        _path: &str,
        _before: Expected,
        _action: ChangeAction,
        _create_cap: Option<CreateCap>,
    ) -> Result<(), PrivateFileError> {
        Err(PrivateFileError::Unavailable)
    }
}

impl PrivateFiles for () {}
