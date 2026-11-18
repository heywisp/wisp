use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use wisp_tool_text::paths::shorten_path;

use super::{
    FILE_NAME, Files, Kind, LoadAll, LoadedFile, MAIN_PRESET_OPENING, PRESET_OPENING, ProblemKind,
    Scope, SourceInfo, Visit, Walker, large_tokens, lexical, render,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Holder {
    Main,
    Child,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetFiles {
    pub path: PathBuf,
    pub block: String,
    pub files: Vec<LoadedFile>,
    pub sources: Vec<SourceInfo>,
    pub large: Option<u64>,
}

impl PresetFiles {
    pub fn bytes(&self) -> u64 {
        self.sources
            .iter()
            .map(|s| s.bytes)
            .fold(0, u64::saturating_add)
    }
}

pub(super) fn problem_message(kind: ProblemKind, shown: &str, imported: bool) -> String {
    let what = if imported {
        "instructions import"
    } else {
        "instructions"
    };
    match kind {
        ProblemKind::NotAFile => format!("{what} {shown} isn't a regular file"),
        ProblemKind::NotUtf8 => format!("{what} {shown} isn't valid UTF-8"),
        _ => format!("{what} {shown} can't be read"),
    }
}

pub fn resolve_preset(
    written: &str,
    config_dir: Option<&Path>,
    home: Option<&Path>,
    fs: &dyn Files,
) -> Result<PathBuf, String> {
    let home = home.filter(|h| h.is_absolute());
    let path = if written == "~" {
        home.map(Path::to_owned)
    } else if let Some(rest) = written.strip_prefix("~/") {
        home.map(|h| h.join(rest))
    } else if Path::new(written).is_absolute() {
        Some(PathBuf::from(written))
    } else {
        config_dir.map(|dir| dir.join(written))
    };
    let Some(path) = path.map(|p| lexical(&p)) else {
        return Err(format!(
            "instructions {written} can't be found: there is no {}",
            if written.starts_with('~') {
                "home directory"
            } else {
                "config.toml directory"
            }
        ));
    };
    let shown = shorten_path(&path, home);
    match fs.metadata(&path) {
        Ok(meta) if meta.kind == Kind::Directory => {
            let file = path.join(FILE_NAME);
            if fs.exists(&file) {
                Ok(file)
            } else {
                Err(format!("instructions {shown} has no {FILE_NAME}"))
            }
        }
        Ok(_) => Ok(path),
        Err(_) if fs.exists(&path) => Ok(path),
        Err(_) => Err(format!("instructions {shown} doesn't exist")),
    }
}

pub fn load_preset(
    file: &Path,
    home: Option<&Path>,
    warn_tokens: u64,
    holder: Holder,
    fs: &dyn Files,
) -> Result<PresetFiles, String> {
    let home = home.filter(|h| h.is_absolute());
    let dir = file.parent().map(Path::to_owned).unwrap_or_default();
    let name = file.file_name().map_or_else(
        || FILE_NAME.to_owned(),
        |n| n.to_string_lossy().into_owned(),
    );
    let mut walker = Walker {
        scope: Scope::Preset,
        scope_root: dir,
        project: None,
        home,
        policy: &LoadAll,
        fs,
        seen: BTreeSet::new(),
        files: Vec::new(),
        infos: Vec::new(),
        diagnostics: Vec::new(),
    };
    walker.visit(Visit {
        path: file.to_owned(),
        name,
        depth: 0,
        importer: None,
    });
    if let Some(super::Diagnostic::Problem(problem)) = walker.diagnostics.first() {
        return Err(problem.message.clone());
    }
    let opening = match holder {
        Holder::Main => MAIN_PRESET_OPENING,
        Holder::Child => PRESET_OPENING,
    };
    let Some(block) = render(opening, &walker.files) else {
        return Err(format!(
            "instructions {} hold no text",
            shorten_path(file, home)
        ));
    };
    let bytes = walker
        .infos
        .iter()
        .map(|s| s.bytes)
        .fold(0, u64::saturating_add);
    Ok(PresetFiles {
        path: file.to_owned(),
        block,
        files: walker.files,
        sources: walker.infos,
        large: large_tokens(bytes, warn_tokens),
    })
}

#[cfg(test)]
mod tests;
