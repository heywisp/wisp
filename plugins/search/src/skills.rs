use std::fs;
use std::path::{Component, Path, PathBuf};

use wisp_plugin_api::host::{CallContext, Host};
use wisp_plugin_api::manage::{Action, Change};

use wisp_instructions::skills;

const UNSUPPORTED_FILE: &str = "skill folders cannot contain symlinks or special files";

struct PartialCopy(Option<PathBuf>);

impl Drop for PartialCopy {
    fn drop(&mut self) {
        if let Some(folder) = self.0.take() {
            let _ = fs::remove_dir_all(folder);
        }
    }
}

pub(super) async fn change(
    host: &dyn Host,
    change: &Change,
    ctx: &dyn CallContext,
) -> Result<String, String> {
    if ctx.cancelled() {
        return Err(wisp_tool_text::manage::NO_ANSWER.into());
    }
    let name = &change.name;
    let path = Path::new(name);
    if name.starts_with('.')
        || name.contains(['/', '\\'])
        || path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
    {
        return Err("a skill name must be one folder name".into());
    }
    let root = config_root(host)?;
    let target = root.join(name);
    let result = match change.action {
        Action::Add => {
            let from = change
                .from
                .as_deref()
                .filter(|from| !from.trim().is_empty())
                .ok_or("add skill needs from: the skill's folder")?;
            add(host, name, &source_path(host, from)?, &root, &target, ctx).await
        }
        Action::Remove => remove(host, name, &target),
        Action::On | Action::Off => {
            return Err(wisp_tool_text::manage::nothing_named("skill", name));
        }
    };
    host.index().changed();
    result?;
    Ok(format!(
        "{name} {}",
        if change.action == Action::Add {
            "added"
        } else {
            "removed"
        }
    ))
}

fn config_root(host: &dyn Host) -> Result<PathBuf, String> {
    let config = host
        .config_dir()
        .filter(|config| !config.is_empty())
        .ok_or("Wisp's config folder is unavailable")?;
    let root = skills::config_folder(&config);
    if fs::symlink_metadata(&root).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err("the skills folder must not be a symlink".into());
    }
    Ok(root)
}

fn source_path(host: &dyn Host, from: &str) -> Result<PathBuf, String> {
    if let Some(relative) = from.strip_prefix("~/") {
        return host
            .home()
            .map(|home| Path::new(&home).join(relative))
            .ok_or_else(|| "the home folder is unavailable".into());
    }
    let path = Path::new(from);
    if path.is_absolute() {
        return Ok(path.to_owned());
    }
    let project = host.root();
    if project.is_empty() {
        return Err("a relative skill folder needs a project".into());
    }
    Ok(Path::new(&project).join(path))
}

fn validate(folder: &Path, name: &str) -> Result<skills::Parsed, String> {
    let meta =
        fs::symlink_metadata(folder.join(skills::FILE_NAME)).map_err(|error| error.to_string())?;
    if !meta.is_file() {
        return Err(UNSUPPORTED_FILE.into());
    }
    let text = skills::read(folder, |file| fs::read_to_string(file))?;
    if text.name != name {
        return Err("SKILL.md name must match the skill name".into());
    }
    Ok(text)
}

async fn add(
    host: &dyn Host,
    name: &str,
    source: &Path,
    root: &Path,
    target: &Path,
    ctx: &dyn CallContext,
) -> Result<(), String> {
    let text = validate(source, name)?;
    if let Some(problem) = skills::name_problem(source, &text) {
        return Err(problem);
    }
    if fs::symlink_metadata(target).is_ok() || existing(host, name)?.is_some() {
        return Err(already_exists(name));
    }
    let source = fs::canonicalize(source).map_err(|error| error.to_string())?;
    fs::create_dir_all(root).map_err(|error| error.to_string())?;
    let root = fs::canonicalize(root).map_err(|error| error.to_string())?;
    if root.starts_with(&source) {
        return Err("the source folder cannot contain Wisp's skills folder".into());
    }
    let staging = stage(&root)?;
    let mut partial = PartialCopy(Some(staging.clone()));
    let copied = copy(&source, &staging, ctx)
        .await
        .and_then(|()| publish(host, name, &staging, target));
    partial.0 = None;
    if let Err(error) = copied {
        fs::remove_dir_all(&staging).map_err(|cleanup| format!("{error}; {cleanup}"))?;
        return Err(error);
    }
    Ok(())
}

fn stage(root: &Path) -> Result<PathBuf, String> {
    let mut number = 0_u64;
    loop {
        let folder = root.join(format!(".skill-add-{number}"));
        match fs::create_dir(&folder) {
            Ok(()) => return Ok(folder),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                number = number.checked_add(1).ok_or_else(|| error.to_string())?;
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

fn publish(host: &dyn Host, name: &str, staging: &Path, target: &Path) -> Result<(), String> {
    validate(staging, name)?;
    let taken = || already_exists(name);
    if fs::symlink_metadata(target).is_ok() || existing(host, name)?.is_some() {
        return Err(taken());
    }
    fs::rename(staging, target).map_err(|error| {
        if fs::symlink_metadata(target).is_ok() {
            taken()
        } else {
            error.to_string()
        }
    })
}

async fn copy(source: &Path, target: &Path, ctx: &dyn CallContext) -> Result<(), String> {
    let mut pending = vec![(source.to_owned(), target.to_owned())];
    while let Some((source, target)) = pending.pop() {
        let mut entries = fs::read_dir(&source)
            .and_then(|entries| entries.collect::<Result<Vec<_>, _>>())
            .map_err(|error| error.to_string())?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            if !skills::included(&entry.file_name()) {
                continue;
            }
            wisp_plugin_api::yield_now().await;
            if ctx.cancelled() {
                return Err(wisp_tool_text::manage::NO_ANSWER.into());
            }
            let kind = entry.file_type().map_err(|error| error.to_string())?;
            let destination = target.join(entry.file_name());
            if kind.is_dir() {
                fs::create_dir(&destination).map_err(|error| error.to_string())?;
                pending.push((entry.path(), destination));
            } else if kind.is_file() {
                fs::copy(entry.path(), destination).map_err(|error| error.to_string())?;
            } else {
                return Err(UNSUPPORTED_FILE.into());
            }
        }
    }
    if ctx.cancelled() {
        return Err(wisp_tool_text::manage::NO_ANSWER.into());
    }
    Ok(())
}

fn remove(host: &dyn Host, name: &str, target: &Path) -> Result<(), String> {
    let refusal = || format!("skill `{name}` is not in Wisp's skills folder; turn it off instead");
    if existing(host, name)?.is_some_and(|folder| folder != target) {
        return Err(refusal());
    }
    let meta = fs::symlink_metadata(target).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            refusal()
        } else {
            error.to_string()
        }
    })?;
    if meta.file_type().is_symlink() {
        fs::remove_file(target)
    } else if meta.is_dir() {
        fs::remove_dir_all(target)
    } else {
        return Err(refusal());
    }
    .map_err(|error| error.to_string())
}

fn existing(host: &dyn Host, name: &str) -> Result<Option<PathBuf>, String> {
    let visits = skills::walk(
        skills::folders(
            &host.root(),
            host.home().as_deref(),
            host.config_dir().as_deref(),
        ),
        |root| {
            fs::read_dir(root)?
                .map(|entry| {
                    entry.map(|entry| skills::Entry {
                        path: entry.path(),
                        is_dir: entry.path().is_dir(),
                    })
                })
                .collect()
        },
        |file| fs::read_to_string(file),
    );
    for visit in visits {
        let visit = visit.map_err(|problem| problem.message)?;
        if visit.text.is_ok_and(|text| text.name == name) {
            return Ok(Some(visit.folder));
        }
    }
    Ok(None)
}

fn already_exists(name: &str) -> String {
    format!("skill `{name}` already exists")
}
