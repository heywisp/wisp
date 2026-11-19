use std::collections::BTreeSet;
use std::io;
use std::path::{Component, Path, PathBuf};

pub use wisp_plugin_api::instructions::{
    CandidateInfo, Diagnostic, LoadReport, Problem, ProblemKind, Scope, SourceInfo, Summary,
};
pub use wisp_tool_text::output::group;
use wisp_tool_text::paths::shorten_path;

pub mod delta;
mod imports;
mod preset;
pub mod skills;

pub use imports::{Import, Scan, scan};
pub use preset::{Holder, PresetFiles, load_preset, resolve_preset};

pub const FILE_NAME: &str = "AGENTS.md";
pub const MAX_DEPTH: usize = 4;
pub const DEFAULT_WARN_TOKENS: u64 = 8_000;
pub const BYTES_PER_TOKEN: u64 = 4;

const PROJECT_OPENING: &str = "Instructions for this project follow, from its AGENTS.md. File names are relative to the project root. The user's messages take precedence over them.";
macro_rules! preset_opening {
    () => {
        "Instructions for your preset follow, from the file named by Preset instructions in <environment>. File names are relative to that file's directory."
    };
}
const MAIN_PRESET_OPENING: &str = preset_opening!();
const PRESET_OPENING: &str = concat!(
    preset_opening!(),
    " Your parent's messages take precedence over them."
);
const DEFAULT_OPENING: &str = "The user's default instructions follow. File names are relative to the user's Wisp config directory. The user's messages take precedence over them.";

pub fn scope_name(scope: Scope) -> &'static str {
    match scope {
        Scope::Project => "project",
        Scope::Default => "default",
        Scope::Preset => "preset",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Source {
    #[default]
    Auto,
    Default,
    None,
}

impl Source {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "auto" => Some(Self::Auto),
            "default" => Some(Self::Default),
            "none" => Some(Self::None),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Directory,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meta {
    pub kind: Kind,
    pub len: u64,
    pub mtime: Option<i64>,
}

pub trait Files {
    fn metadata(&self, path: &Path) -> io::Result<Meta>;
    fn exists(&self, path: &Path) -> bool;
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf>;
    fn read(&self, path: &Path) -> io::Result<Option<Vec<u8>>>;
    fn read_capped(&self, path: &Path, limit: u64) -> io::Result<Option<Vec<u8>>> {
        let Some(mut raw) = self.read(path)? else {
            return Ok(None);
        };
        if let Ok(cap) = usize::try_from(limit)
            && raw.len() > cap
        {
            raw.truncate(cap);
        }
        Ok(Some(raw))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    Load,
}

#[derive(Debug, Clone, Copy)]
pub struct Candidate<'a> {
    pub scope: Scope,
    pub path: &'a Path,
    pub external: bool,
    pub imported_by: Option<&'a Path>,
}

pub trait SourcePolicy: Send + Sync {
    fn admit(&self, source: &Candidate<'_>) -> Admission;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LoadAll;

impl SourcePolicy for LoadAll {
    fn admit(&self, _source: &Candidate<'_>) -> Admission {
        Admission::Load
    }
}

pub fn config_home(xdg_config_home: Option<PathBuf>, home: Option<&Path>) -> Option<PathBuf> {
    xdg_config_home
        .filter(|p| p.is_absolute())
        .or_else(|| home.filter(|h| h.is_absolute()).map(|h| h.join(".config")))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub start: PathBuf,
    pub config_dir: Option<PathBuf>,
    pub home: Option<PathBuf>,
}

impl Paths {
    pub fn config_dir_from(
        xdg_config_home: Option<PathBuf>,
        home: Option<&Path>,
    ) -> Option<PathBuf> {
        Some(config_home(xdg_config_home, home)?.join("wisp"))
    }

    pub fn default_file(&self) -> Option<PathBuf> {
        self.config_dir.as_ref().map(|d| d.join(FILE_NAME))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Options {
    pub source: Source,
    pub warn_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedFile {
    pub scope: Scope,
    pub name: String,
    pub path: PathBuf,
    pub display_path: String,
    pub text: String,
    pub imported_by: Option<String>,
    pub external: bool,
    pub section: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    pub start: PathBuf,
    pub root: PathBuf,
    pub source: Source,
    pub chosen: Option<Scope>,
    pub block: Option<String>,
    pub files: Vec<LoadedFile>,
    pub summary: Summary,
    pub large: Option<u64>,
}

impl Loaded {
    pub fn block(&self) -> Option<&str> {
        self.block.as_deref()
    }

    pub fn project_root(&self) -> Option<&Path> {
        let loaded = self.chosen == Some(Scope::Project) && self.block.is_some();
        (loaded && self.root != self.start).then_some(self.root.as_path())
    }

    pub fn total_bytes(&self) -> u64 {
        total_bytes(&self.summary)
    }

    pub fn report(&self) -> LoadReport {
        LoadReport {
            chosen: self.chosen,
            summary: self.summary.clone(),
            project_root: self.project_root().map(|p| p.display().to_string()),
        }
    }
}

pub fn total_bytes(summary: &Summary) -> u64 {
    summary
        .chosen
        .iter()
        .chain(summary.imports.iter())
        .map(|s| s.bytes)
        .fold(0, u64::saturating_add)
}

pub fn project_root(start: &Path, files: &dyn Files) -> PathBuf {
    repository(start, files).unwrap_or_else(|| start.to_owned())
}

pub fn repository(start: &Path, files: &dyn Files) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| is_repository(dir, files))
        .map(Path::to_owned)
}

pub const GITFILE: &[u8] = b"gitdir:";
const GITFILE_MAX_BYTES: u64 = 1 << 20;

pub fn is_repository(dir: &Path, files: &dyn Files) -> bool {
    let marker = dir.join(".git");
    match files.metadata(&marker).map(|meta| meta.kind) {
        Ok(Kind::Directory) => is_git_dir(&marker, files),
        Ok(Kind::File) => pointed(&marker, GITFILE, files).is_some(),
        _ => false,
    }
}

pub fn is_git_dir(dir: &Path, files: &dyn Files) -> bool {
    ["HEAD", "objects", "refs"]
        .iter()
        .all(|entry| files.exists(&dir.join(entry)))
}

pub fn pointed(file: &Path, prefix: &[u8], files: &dyn Files) -> Option<Vec<u8>> {
    let meta = files.metadata(file).ok()?;
    if meta.kind != Kind::File || meta.len > GITFILE_MAX_BYTES {
        return None;
    }
    let raw = files.read_capped(file, GITFILE_MAX_BYTES + 1).ok()??;
    if u64::try_from(raw.len()).unwrap_or(u64::MAX) > GITFILE_MAX_BYTES {
        return None;
    }
    Some(raw.strip_prefix(prefix)?.to_vec())
}

pub fn mtime(modified: std::time::SystemTime) -> Option<i64> {
    let since = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    i64::try_from(since.as_nanos()).ok()
}

pub fn estimate_tokens(bytes: u64) -> u64 {
    bytes / BYTES_PER_TOKEN
}

pub fn large_tokens(bytes: u64, warn_tokens: u64) -> Option<u64> {
    let tokens = estimate_tokens(bytes);
    (tokens > warn_tokens).then_some(tokens)
}

pub fn sha256_hex(text: &str) -> String {
    wisp_plugin_api::hex::encode(&hmac_sha256::Hash::hash(text.as_bytes()))
}

enum Fetched {
    Text { text: String, bytes: u64 },
    Missing,
    Problem(ProblemKind, String),
}

pub fn normalise_text(raw: &str) -> String {
    let text = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let mut text = text.replace("\r\n", "\n").replace('\r', "\n");
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

fn fetch(path: &Path, files: &dyn Files) -> Fetched {
    let meta = match files.metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if files.exists(path) {
                return Fetched::Problem(
                    ProblemKind::Unreadable,
                    "it is a link to a file that doesn't exist".to_owned(),
                );
            }
            return Fetched::Missing;
        }
        Err(e) => return Fetched::Problem(ProblemKind::Unreadable, e.to_string()),
    };
    if meta.kind != Kind::File {
        return Fetched::Problem(ProblemKind::NotAFile, String::new());
    }
    let raw = match files.read(path) {
        Ok(Some(raw)) => raw,
        Ok(None) => return Fetched::Problem(ProblemKind::NotAFile, String::new()),
        Err(e) => return Fetched::Problem(ProblemKind::Unreadable, e.to_string()),
    };
    let bytes = raw.len() as u64;
    match String::from_utf8(raw) {
        Ok(text) => Fetched::Text {
            text: normalise_text(&text),
            bytes,
        },
        Err(_) => Fetched::Problem(ProblemKind::NotUtf8, String::new()),
    }
}

fn problem_message(kind: ProblemKind, shown: &str, detail: &str) -> String {
    match kind {
        ProblemKind::NotAFile => {
            format!(
                "{shown} isn't a regular file; it was skipped. Instructions must be regular files."
            )
        }
        ProblemKind::NotUtf8 => {
            format!("{shown} isn't valid UTF-8; it was skipped. Save it as UTF-8.")
        }
        _ => format!(
            "{shown} can't be read ({detail}); it was skipped. Check that Wisp can read it."
        ),
    }
}

fn problem(source: Option<String>, kind: ProblemKind, message: String) -> Diagnostic {
    Diagnostic::Problem(Problem {
        source,
        kind,
        message,
    })
}

pub fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::Prefix(_) | Component::RootDir => out.push(part.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            Component::Normal(name) => out.push(name),
        }
    }
    out
}

pub fn normalise_name(name: &str) -> String {
    let (anchor, rest) = if name == "~" {
        (Some("~"), "")
    } else if let Some(rest) = name.strip_prefix("~/") {
        (Some("~"), rest)
    } else if let Some(rest) = name.strip_prefix('/') {
        (Some("/"), rest)
    } else {
        (None, name)
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in rest.split('/') {
        match part {
            "" | "." => {}
            ".." => match parts.last() {
                Some(last) if *last != ".." => {
                    parts.pop();
                }
                _ if anchor == Some("/") => {}
                _ => parts.push(".."),
            },
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    match anchor {
        Some("~") => format!("~/{joined}"),
        Some(_) => format!("/{joined}"),
        None if joined.is_empty() => ".".to_owned(),
        None => joined,
    }
}

fn dir_of(name: &str) -> &str {
    name.rfind('/')
        .map_or("", |i| if i == 0 { "/" } else { &name[..i] })
}

fn relative_name(root: &Path, target: &Path) -> Option<String> {
    let rest = target.strip_prefix(root).ok()?;
    if rest.as_os_str().is_empty() {
        return None;
    }
    let parts: Vec<String> = rest
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    Some(parts.join("/"))
}

fn import_name(root: &Path, target: &Path, written: &str, importer: &str) -> String {
    if let Some(name) = relative_name(root, target) {
        return name;
    }
    if written.starts_with('/') || written.starts_with("~/") {
        return written.to_owned();
    }
    let dir = dir_of(importer);
    let joined = match dir {
        "" => written.to_owned(),
        "/" => format!("/{written}"),
        dir => format!("{dir}/{written}"),
    };
    normalise_name(&joined)
}

struct Walker<'a> {
    scope: Scope,
    scope_root: PathBuf,
    project: Option<PathBuf>,
    home: Option<&'a Path>,
    policy: &'a dyn SourcePolicy,
    fs: &'a dyn Files,
    seen: BTreeSet<PathBuf>,
    files: Vec<LoadedFile>,
    infos: Vec<SourceInfo>,
    diagnostics: Vec<Diagnostic>,
}

struct Visit<'a> {
    path: PathBuf,
    name: String,
    depth: usize,
    importer: Option<(&'a str, &'a Path)>,
}

impl Walker<'_> {
    fn shown(&self, path: &Path) -> String {
        shorten_path(path, self.home)
    }

    fn external(&self, path: &Path) -> bool {
        let Some(project) = &self.project else {
            return false;
        };
        self.fs
            .canonicalize(path)
            .is_ok_and(|real| !real.starts_with(project))
    }

    fn resolve(&self, written: &str, importer: &Path) -> Option<PathBuf> {
        let target = if let Some(rest) = written.strip_prefix("~/") {
            self.home?.join(rest)
        } else if Path::new(written).is_absolute() {
            PathBuf::from(written)
        } else {
            importer.parent().unwrap_or(Path::new("")).join(written)
        };
        Some(lexical(&target))
    }

    fn visit(&mut self, visit: Visit<'_>) {
        let path = visit.path;
        let external =
            self.scope == Scope::Project && visit.importer.is_some() && self.external(&path);
        let admitted = self.policy.admit(&Candidate {
            scope: self.scope,
            path: &path,
            external,
            imported_by: visit.importer.map(|(_, p)| p),
        });
        match admitted {
            Admission::Load => {}
        }
        let key = self.fs.canonicalize(&path).unwrap_or_else(|_| path.clone());
        if !self.seen.insert(key) {
            return;
        }
        let shown = self.shown(&path);
        let (text, bytes) = match fetch(&path, self.fs) {
            Fetched::Text { text, bytes } => (text, bytes),
            Fetched::Missing => {
                let message = match (visit.importer, self.scope) {
                    (Some((importer, _)), Scope::Preset) => {
                        format!(
                            "instructions import {shown}, imported by {importer}, doesn't exist"
                        )
                    }
                    (Some((importer, _)), _) => format!(
                        "{shown}, imported by {importer}, doesn't exist; the import was skipped."
                    ),
                    (None, Scope::Preset) => format!("instructions {shown} doesn't exist"),
                    (None, _) => format!("{shown} doesn't exist."),
                };
                self.diagnostics
                    .push(problem(Some(shown), ProblemKind::ImportMissing, message));
                return;
            }
            Fetched::Problem(kind, detail) => {
                let message = if self.scope == Scope::Preset {
                    preset::problem_message(kind, &shown, visit.importer.is_some())
                } else {
                    problem_message(kind, &shown, &detail)
                };
                self.diagnostics.push(problem(Some(shown), kind, message));
                return;
            }
        };
        let scanned = scan(&text);
        let section = !scanned.only_imports;
        self.infos.push(SourceInfo {
            scope: self.scope,
            name: visit.name.clone(),
            display_path: shown.clone(),
            bytes,
            sha256: sha256_hex(&text),
            imported_by: visit.importer.map(|(name, _)| name.to_owned()),
            external,
        });
        self.files.push(LoadedFile {
            scope: self.scope,
            name: visit.name.clone(),
            path: path.clone(),
            display_path: shown,
            text,
            imported_by: visit.importer.map(|(name, _)| name.to_owned()),
            external,
            section,
        });
        for import in scanned.imports {
            let Some(target) = self.resolve(&import.path, &path) else {
                self.diagnostics.push(problem(
                    Some(import.path.clone()),
                    ProblemKind::ImportMissing,
                    if self.scope == Scope::Preset {
                        format!(
                            "instructions import {}, imported by {}, can't be found: there is no home directory",
                            import.path, visit.name
                        )
                    } else {
                        format!(
                            "{}, imported by {}, can't be found: there is no home directory; the import was skipped.",
                            import.path, visit.name
                        )
                    },
                ));
                continue;
            };
            let name = import_name(&self.scope_root, &target, &import.path, &visit.name);
            let key = self
                .fs
                .canonicalize(&target)
                .unwrap_or_else(|_| target.clone());
            if self.seen.contains(&key) {
                continue;
            }
            if visit.depth + 1 > MAX_DEPTH {
                let shown = self.shown(&target);
                let message = if self.scope == Scope::Preset {
                    format!(
                        "instructions import {shown}, imported by {}, is more than {MAX_DEPTH} imports deep",
                        visit.name
                    )
                } else {
                    format!(
                        "{shown}, imported by {}, is more than {MAX_DEPTH} imports deep; it was skipped. Import it from a file nearer the top.",
                        visit.name
                    )
                };
                self.diagnostics
                    .push(problem(Some(shown), ProblemKind::ImportTooDeep, message));
                continue;
            }
            self.visit(Visit {
                path: target,
                name,
                depth: visit.depth + 1,
                importer: Some((&visit.name, &path)),
            });
        }
    }
}

fn candidate(scope: Scope, path: &Path, home: Option<&Path>, fs: &dyn Files) -> CandidateInfo {
    let meta = fs.metadata(path).ok().filter(|m| m.kind == Kind::File);
    CandidateInfo {
        scope,
        display_path: shorten_path(path, home),
        bytes: meta.map(|m| m.len),
        mtime: meta.and_then(|m| m.mtime),
        sha256: None,
    }
}

pub fn render(opening: &str, files: &[LoadedFile]) -> Option<String> {
    let sections: Vec<&LoadedFile> = files.iter().filter(|f| f.section).collect();
    if sections.is_empty() {
        return None;
    }
    let mut block = format!("<instructions>\n{opening}\n");
    for file in sections {
        block.push_str(&format!(
            "<file scope=\"{}\" name=\"{}\"",
            scope_name(file.scope),
            file.name
        ));
        if let Some(by) = &file.imported_by {
            block.push_str(&format!(" imported_by=\"{by}\""));
        }
        if file.external {
            block.push_str(" external=\"true\"");
        }
        block.push_str(">\n");
        block.push_str(&file.text);
        block.push_str("</file>\n");
    }
    block.push_str("</instructions>");
    Some(block)
}

pub fn load(paths: &Paths, options: Options, policy: &dyn SourcePolicy, fs: &dyn Files) -> Loaded {
    let root = project_root(&paths.start, fs);
    let home = paths.home.as_deref().filter(|h| h.is_absolute());
    let project_file = root.join(FILE_NAME);
    let default_file = paths.default_file();
    let mut candidates = vec![candidate(Scope::Project, &project_file, home, fs)];
    if let Some(path) = &default_file {
        candidates.push(candidate(Scope::Default, path, home, fs));
    }
    let project = fs
        .exists(&project_file)
        .then_some((Scope::Project, project_file.clone()));
    let default = default_file
        .as_ref()
        .filter(|p| fs.exists(p))
        .map(|p| (Scope::Default, p.clone()));
    let chosen = match options.source {
        Source::None => None,
        Source::Default => default,
        Source::Auto => project.or(default),
    };
    let mut summary = Summary {
        candidates,
        ..Summary::default()
    };
    let Some((scope, path)) = chosen else {
        return Loaded {
            start: paths.start.clone(),
            root,
            source: options.source,
            chosen: None,
            block: None,
            files: Vec::new(),
            summary,
            large: None,
        };
    };
    let scope_root = match scope {
        Scope::Project => root.clone(),
        Scope::Default | Scope::Preset => paths.config_dir.clone().unwrap_or_default(),
    };
    let mut walker = Walker {
        scope,
        scope_root,
        project: match scope {
            Scope::Project => fs.canonicalize(&root).ok(),
            Scope::Default | Scope::Preset => None,
        },
        home,
        policy,
        fs,
        seen: BTreeSet::new(),
        files: Vec::new(),
        infos: Vec::new(),
        diagnostics: Vec::new(),
    };
    walker.visit(Visit {
        path: path.clone(),
        name: FILE_NAME.to_owned(),
        depth: 0,
        importer: None,
    });
    let mut infos = walker.infos.into_iter();
    let chosen_info = infos.next().filter(|i| i.imported_by.is_none());
    if let Some(info) = &chosen_info
        && let Some(c) = summary
            .candidates
            .iter_mut()
            .find(|c| c.scope == info.scope)
    {
        c.bytes = Some(info.bytes);
        c.sha256 = Some(info.sha256.clone());
    }
    summary.chosen = chosen_info;
    summary.imports = infos.collect();
    summary.diagnostics = walker.diagnostics;
    let large = large_tokens(total_bytes(&summary), options.warn_tokens);
    let opening = match scope {
        Scope::Project => PROJECT_OPENING,
        Scope::Default => DEFAULT_OPENING,
        Scope::Preset => PRESET_OPENING,
    };
    let block = render(opening, &walker.files);
    Loaded {
        start: paths.start.clone(),
        root,
        source: options.source,
        chosen: Some(scope),
        block,
        files: walker.files,
        summary,
        large,
    }
}

#[cfg(test)]
mod testing;
#[cfg(test)]
mod tests;
