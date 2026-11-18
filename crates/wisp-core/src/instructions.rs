use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use wisp_instructions::{Files, Kind, LoadedFile, Meta};
use wisp_plugin_api::instructions as mirror;
use wisp_protocol::{
    CacheTtl, CandidateInfo, Diagnostic, DiagnosticKind, InstructionsSource, InstructionsSummary,
    SourceInfo,
};

use crate::pricing::{self, ModelInfo};

pub use wisp_instructions::{
    BYTES_PER_TOKEN, DEFAULT_WARN_TOKENS, FILE_NAME, Holder, MAX_DEPTH, Paths, config_home,
    estimate_tokens, is_repository,
};
pub use wisp_protocol::settings::{parse_tokens, thousands};

const AUTONOMOUS_TEXT: &str = include_str!("instructions/autonomous.md");

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Extra {
    Autonomous,
}

impl Extra {
    pub const ALL: [Self; 1] = [Self::Autonomous];

    pub fn name(self) -> &'static str {
        match self {
            Self::Autonomous => "autonomous",
        }
    }

    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|extra| extra.name() == name)
    }

    pub fn text(self) -> &'static str {
        match self {
            Self::Autonomous => AUTONOMOUS_TEXT,
        }
    }

    pub fn block(self) -> String {
        format!(
            "<extra_instructions name=\"{}\">\n{}</extra_instructions>",
            self.name(),
            self.text()
        )
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Disk;

impl Files for Disk {
    fn metadata(&self, path: &Path) -> io::Result<Meta> {
        let meta = std::fs::metadata(path)?;
        Ok(meta_of(&meta))
    }

    fn exists(&self, path: &Path) -> bool {
        std::fs::symlink_metadata(path).is_ok()
    }

    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        std::fs::canonicalize(path)
    }

    fn read(&self, path: &Path) -> io::Result<Option<Vec<u8>>> {
        let mut file = open_nonblocking(path)?;
        if !file.metadata()?.is_file() {
            return Ok(None);
        }
        let mut raw = Vec::new();
        file.read_to_end(&mut raw)?;
        Ok(Some(raw))
    }
}

fn meta_of(meta: &std::fs::Metadata) -> Meta {
    let kind = if meta.is_file() {
        Kind::File
    } else if meta.is_dir() {
        Kind::Directory
    } else {
        Kind::Other
    };
    Meta {
        kind,
        len: meta.len(),
        mtime: meta.modified().ok().and_then(wisp_instructions::mtime),
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn make_repository(dir: &Path) -> io::Result<()> {
    let marker = dir.join(".git");
    std::fs::create_dir_all(marker.join("objects"))?;
    std::fs::create_dir_all(marker.join("refs"))?;
    std::fs::write(marker.join("HEAD"), "ref: refs/heads/main\n")
}

#[cfg(unix)]
fn open_nonblocking(path: &Path) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags};
    let fd = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC | OFlags::NOCTTY,
        Mode::empty(),
    )?;
    Ok(File::from(fd))
}

#[cfg(not(unix))]
fn open_nonblocking(path: &Path) -> io::Result<File> {
    File::open(path)
}

pub fn total_bytes(summary: &InstructionsSummary) -> u64 {
    summary
        .chosen
        .iter()
        .chain(summary.imports.iter())
        .map(|s| s.bytes)
        .fold(0, u64::saturating_add)
}

pub fn large_message(tokens: u64, priced: Option<(&str, &ModelInfo)>) -> Diagnostic {
    let count = thousands(tokens);
    let message = match priced {
        Some((model, info)) => {
            let (read, write) = pricing::prefix_quote(info, tokens, CacheTtl::OneHour);
            format!(
                "instructions are large: about {count} tokens, {} per request to read and {} per cache write ({})",
                wisp_protocol::marked_amount(&wisp_protocol::format_usd(read), true, false),
                wisp_protocol::marked_amount(&wisp_protocol::format_usd(write), true, false),
                wisp_protocol::shown_model(model)
            )
        }
        None => format!("instructions are large: about {count} tokens"),
    };
    Diagnostic {
        source: None,
        kind: DiagnosticKind::Large,
        message,
    }
}

pub fn large_warning(
    bytes: u64,
    warn_tokens: u64,
    priced: Option<(&str, &ModelInfo)>,
) -> Option<Diagnostic> {
    wisp_instructions::large_tokens(bytes, warn_tokens).map(|tokens| large_message(tokens, priced))
}

pub fn scope(scope: mirror::Scope) -> wisp_protocol::Scope {
    match scope {
        mirror::Scope::Project => wisp_protocol::Scope::Project,
        mirror::Scope::Default => wisp_protocol::Scope::Default,
        mirror::Scope::Preset => wisp_protocol::Scope::Preset,
    }
}

pub fn source(source: &InstructionsSource) -> wisp_instructions::Source {
    match source {
        InstructionsSource::Default => wisp_instructions::Source::Default,
        InstructionsSource::None => wisp_instructions::Source::None,
        InstructionsSource::Auto | InstructionsSource::Other(_) => wisp_instructions::Source::Auto,
    }
}

pub fn source_info(info: mirror::SourceInfo) -> SourceInfo {
    SourceInfo {
        scope: scope(info.scope),
        name: info.name,
        display_path: info.display_path,
        bytes: info.bytes,
        sha256: info.sha256,
        imported_by: info.imported_by,
        external: info.external,
    }
}

fn candidate_info(info: mirror::CandidateInfo) -> CandidateInfo {
    CandidateInfo {
        scope: scope(info.scope),
        display_path: info.display_path,
        bytes: info.bytes,
        mtime: info.mtime,
        sha256: info.sha256,
    }
}

pub fn diagnostic(
    diagnostic: mirror::Diagnostic,
    priced: Option<(&str, &ModelInfo)>,
) -> Diagnostic {
    match diagnostic {
        mirror::Diagnostic::Large(tokens) => large_message(tokens, priced),
        mirror::Diagnostic::Problem(problem) => Diagnostic {
            source: problem.source,
            kind: match problem.kind {
                mirror::ProblemKind::NotAFile => DiagnosticKind::NotAFile,
                mirror::ProblemKind::NotUtf8 => DiagnosticKind::NotUtf8,
                mirror::ProblemKind::Unreadable => DiagnosticKind::Unreadable,
                mirror::ProblemKind::ImportMissing => DiagnosticKind::ImportMissing,
                mirror::ProblemKind::ImportTooDeep => DiagnosticKind::ImportTooDeep,
            },
            message: problem.message,
        },
    }
}

pub fn summary(
    summary: mirror::Summary,
    priced: Option<(&str, &ModelInfo)>,
) -> InstructionsSummary {
    InstructionsSummary {
        chosen: summary.chosen.map(source_info),
        imports: summary.imports.into_iter().map(source_info).collect(),
        candidates: summary.candidates.into_iter().map(candidate_info).collect(),
        diagnostics: summary
            .diagnostics
            .into_iter()
            .map(|d| diagnostic(d, priced))
            .collect(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetFiles {
    pub name: String,
    pub path: PathBuf,
    pub block: String,
    pub files: Vec<LoadedFile>,
    pub sources: Vec<SourceInfo>,
    pub large: Option<Diagnostic>,
}

impl PresetFiles {
    pub fn bytes(&self) -> u64 {
        self.sources
            .iter()
            .map(|s| s.bytes)
            .fold(0, u64::saturating_add)
    }
}

pub fn resolve_preset(
    written: &str,
    config_dir: Option<&Path>,
    home: Option<&Path>,
) -> Result<PathBuf, String> {
    wisp_instructions::resolve_preset(written, config_dir, home, &Disk)
}

pub fn load_preset(
    file: &Path,
    home: Option<&Path>,
    preset: &str,
    warn_tokens: u64,
    priced: Option<(&str, &ModelInfo)>,
    holder: Holder,
) -> Result<PresetFiles, String> {
    let files = wisp_instructions::load_preset(file, home, warn_tokens, holder, &Disk)?;
    Ok(PresetFiles {
        name: preset.to_owned(),
        path: files.path,
        block: files.block,
        files: files.files,
        sources: files.sources.into_iter().map(source_info).collect(),
        large: files.large.map(|tokens| {
            let mut large = large_message(tokens, priced);
            large.message = format!("preset {preset}'s {}", large.message);
            large
        }),
    })
}

#[cfg(test)]
mod tests;
