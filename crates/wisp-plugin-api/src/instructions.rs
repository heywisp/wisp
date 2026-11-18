#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    Project,
    Default,
    Preset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProblemKind {
    NotAFile,
    NotUtf8,
    Unreadable,
    ImportMissing,
    ImportTooDeep,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub source: Option<String>,
    pub kind: ProblemKind,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Diagnostic {
    Problem(Problem),
    Large(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceInfo {
    pub scope: Scope,
    pub name: String,
    pub display_path: String,
    pub bytes: u64,
    pub sha256: String,
    pub imported_by: Option<String>,
    pub external: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateInfo {
    pub scope: Scope,
    pub display_path: String,
    pub bytes: Option<u64>,
    pub mtime: Option<i64>,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub chosen: Option<SourceInfo>,
    pub imports: Vec<SourceInfo>,
    pub candidates: Vec<CandidateInfo>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadReport {
    pub chosen: Option<Scope>,
    pub summary: Summary,
    pub project_root: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub scope: Scope,
    pub name: String,
    pub display_path: String,
    pub old_sha256: Option<String>,
    pub new_sha256: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Change {
    pub from: Option<SourceInfo>,
    pub to: Option<SourceInfo>,
    pub changed: Vec<FileChange>,
    pub summary: Summary,
}

pub trait InstructionsReport {
    fn loaded(&self, report: LoadReport);
    fn warning(&self, diagnostic: Diagnostic);
    fn changed(&self, change: Change, text: String);
}
