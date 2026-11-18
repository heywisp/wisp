use std::path::{Path, PathBuf};

use wisp_plugin_api::instructions::FileChange;
use wisp_plugin_api::prompt::Prior;
use wisp_tool_text::paths::shorten_path;

use super::{LoadedFile, Scope, lexical, scope_name, sha256_hex};

pub const OPENING: &str =
    "These files changed since they were loaded. Each version below replaces the earlier one.";
pub const UNREADABLE: &str =
    "This file can no longer be read; its earlier instructions no longer apply.";
pub const NOTHING_APPLIES: &str = "No instructions apply from now on.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub scope: Scope,
    pub name: String,
    pub sha256: String,
}

impl Entry {
    fn same_file(&self, other: &Self) -> bool {
        self.scope == other.scope && self.name == other.name
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Now {
    Text { sha256: String, text: String },
    Gone { unreadable: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub scope: Scope,
    pub name: String,
    pub display_path: String,
    pub now: Now,
}

impl Item {
    fn sha256(&self) -> Option<&str> {
        match &self.now {
            Now::Text { sha256, .. } => Some(sha256),
            Now::Gone { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    pub text: String,
    pub changed: Vec<FileChange>,
}

fn parse_scope(word: &str) -> Option<Scope> {
    match word {
        "project" => Some(Scope::Project),
        "default" => Some(Scope::Default),
        "preset" => Some(Scope::Preset),
        _ => None,
    }
}

pub fn snapshot_of(entries: &[Entry]) -> String {
    entries
        .iter()
        .map(|e| format!("{} {} {}\n", e.sha256, scope_name(e.scope), e.name))
        .collect()
}

pub fn entries_of(files: &[LoadedFile]) -> Vec<Entry> {
    files
        .iter()
        .filter(|f| f.section)
        .map(|f| Entry {
            scope: f.scope,
            name: f.name.clone(),
            sha256: sha256_hex(&f.text),
        })
        .collect()
}

pub fn items_of(files: &[LoadedFile]) -> Vec<Item> {
    files
        .iter()
        .filter(|f| f.section)
        .map(|f| Item {
            scope: f.scope,
            name: f.name.clone(),
            display_path: f.display_path.clone(),
            now: Now::Text {
                sha256: sha256_hex(&f.text),
                text: f.text.clone(),
            },
        })
        .collect()
}

pub fn snapshot(files: &[LoadedFile]) -> String {
    snapshot_of(&entries_of(files))
}

pub fn is_scoped(line: &str) -> bool {
    line.split_once(' ')
        .and_then(|(_, rest)| rest.split_once(' '))
        .and_then(|(word, _)| parse_scope(word))
        .is_some()
}

pub fn with_scope(snapshot: &str, scope: Scope) -> String {
    snapshot
        .lines()
        .map(|line| match line.split_once(' ') {
            Some((sha, name)) if !is_scoped(line) => {
                format!("{sha} {} {name}\n", scope_name(scope))
            }
            _ => format!("{line}\n"),
        })
        .collect()
}

pub fn entries(snapshot: &str) -> Vec<Entry> {
    snapshot
        .lines()
        .filter_map(|line| {
            let (sha, rest) = line.split_once(' ')?;
            let (scope, name) = match rest.split_once(' ') {
                Some((word, name)) if parse_scope(word).is_some() => (parse_scope(word)?, name),
                _ => (Scope::Project, rest),
            };
            Some(Entry {
                scope,
                name: name.to_owned(),
                sha256: sha.to_owned(),
            })
        })
        .collect()
}

pub fn is_snapshot(text: &str) -> bool {
    text.lines().all(|line| {
        line.split_once(' ')
            .is_some_and(|(sha, _)| sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()))
    })
}

pub fn prior_entries(previous: &Prior) -> Option<Vec<Entry>> {
    match previous {
        Prior::None => Some(Vec::new()),
        Prior::Snapshot(snapshot) => Some(entries(snapshot)),
        Prior::Legacy(Some(block)) => Some(legacy_entries(block)),
        Prior::Legacy(None) => None,
    }
}

pub fn since(changed: Vec<FileChange>, baseline: &[Entry]) -> Vec<FileChange> {
    changed
        .into_iter()
        .filter_map(|change| {
            let seen = baseline
                .iter()
                .find(|e| e.scope == change.scope && e.name == change.name)
                .map(|e| e.sha256.clone());
            (seen != change.new_sha256).then_some(FileChange {
                old_sha256: seen,
                ..change
            })
        })
        .collect()
}

pub fn block_scope(block: &str) -> Option<Scope> {
    let project = block.find("<file scope=\"project\"");
    let default = block.find("<file scope=\"default\"");
    match (project, default) {
        (Some(p), Some(d)) if d < p => Some(Scope::Default),
        (Some(_), _) => Some(Scope::Project),
        (None, Some(_)) => Some(Scope::Default),
        (None, None) => None,
    }
}

pub fn has_files(block: &str) -> bool {
    block.contains("<file scope=\"project\"") || block.contains("<file scope=\"default\"")
}

const LEGACY: [Scope; 2] = [Scope::Project, Scope::Default];

pub fn legacy_entries(block: &str) -> Vec<Entry> {
    entries_in(block, &LEGACY)
}

pub fn legacy_files(block: &str) -> Vec<(Entry, String)> {
    files_in(block, &LEGACY)
}

pub fn entries_in(block: &str, scopes: &[Scope]) -> Vec<Entry> {
    files_in(block, scopes)
        .into_iter()
        .map(|(e, _)| e)
        .collect()
}

pub fn files_in(block: &str, scopes: &[Scope]) -> Vec<(Entry, String)> {
    let mut out = Vec::new();
    let mut rest = block;
    while let Some(at) = rest.find("<file scope=\"") {
        rest = &rest[at + "<file scope=\"".len()..];
        let Some((word, after)) = rest.split_once('"') else {
            break;
        };
        let Some((head, body)) = after.split_once(">\n") else {
            break;
        };
        let Some((text, tail)) = body.split_once("</file>\n") else {
            break;
        };
        rest = tail;
        let Some(scope) = parse_scope(word).filter(|s| scopes.contains(s)) else {
            continue;
        };
        let Some(name) = head
            .split_once(" name=\"")
            .and_then(|(_, n)| n.split_once('"'))
            .map(|(n, _)| n.to_owned())
        else {
            continue;
        };
        out.push((
            Entry {
                scope,
                name,
                sha256: sha256_hex(text),
            },
            text.to_owned(),
        ));
    }
    out
}

pub struct Roots<'a> {
    pub project: &'a Path,
    pub config_dir: Option<&'a Path>,
    pub home: Option<&'a Path>,
}

impl Roots<'_> {
    pub fn path(&self, scope: Scope, name: &str) -> Option<PathBuf> {
        let path = if let Some(rest) = name.strip_prefix("~/") {
            self.home?.join(rest)
        } else if Path::new(name).is_absolute() {
            PathBuf::from(name)
        } else {
            match scope {
                Scope::Project => self.project.join(name),
                Scope::Default | Scope::Preset => self.config_dir?.join(name),
            }
        };
        Some(lexical(&path))
    }

    pub fn shown(&self, scope: Scope, name: &str) -> String {
        self.path(scope, name)
            .map_or_else(|| name.to_owned(), |p| shorten_path(&p, self.home))
    }
}

fn file_block(out: &mut String, item: &Item, text: &str) {
    out.push_str(&format!(
        "<file scope=\"{}\" name=\"{}\" path=\"{}\">\n{text}</file>\n",
        scope_name(item.scope),
        item.name,
        item.display_path
    ));
}

fn removed(out: &mut String, scope: Scope, display_path: &str, unreadable: bool) {
    out.push_str(&format!(
        "<removed scope=\"{}\" path=\"{}\"/>\n",
        scope_name(scope),
        display_path
    ));
    if unreadable {
        out.push_str(UNREADABLE);
        out.push('\n');
    }
}

pub fn update(
    old: &[Entry],
    items: &[Item],
    left: impl Fn(&Entry) -> Option<(String, bool)>,
) -> Option<Update> {
    let mut body = String::new();
    let mut changed = Vec::new();
    for item in items {
        let key = Entry {
            scope: item.scope,
            name: item.name.clone(),
            sha256: String::new(),
        };
        let before = old.iter().find(|o| o.same_file(&key));
        let old_sha = before.map(|o| o.sha256.clone());
        match &item.now {
            Now::Text { sha256, .. } if old_sha.as_deref() == Some(sha256.as_str()) => continue,
            Now::Text { sha256, text } => {
                file_block(&mut body, item, text);
                changed.push(FileChange {
                    scope: item.scope,
                    name: item.name.clone(),
                    display_path: item.display_path.clone(),
                    old_sha256: old_sha,
                    new_sha256: Some(sha256.clone()),
                });
            }
            Now::Gone { .. } if old_sha.is_none() => continue,
            Now::Gone { unreadable } => {
                removed(&mut body, item.scope, &item.display_path, *unreadable);
                changed.push(FileChange {
                    scope: item.scope,
                    name: item.name.clone(),
                    display_path: item.display_path.clone(),
                    old_sha256: old_sha,
                    new_sha256: None,
                });
            }
        }
    }
    for entry in old {
        let kept = items.iter().any(|i| {
            entry.same_file(&Entry {
                scope: i.scope,
                name: i.name.clone(),
                sha256: String::new(),
            })
        });
        if kept {
            continue;
        }
        let Some((display_path, unreadable)) = left(entry) else {
            continue;
        };
        removed(&mut body, entry.scope, &display_path, unreadable);
        changed.push(FileChange {
            scope: entry.scope,
            name: entry.name.clone(),
            display_path,
            old_sha256: Some(entry.sha256.clone()),
            new_sha256: None,
        });
    }
    if changed.is_empty() {
        return None;
    }
    if !items.iter().any(|i| i.sha256().is_some()) {
        body.push_str(NOTHING_APPLIES);
        body.push('\n');
    }
    Some(Update {
        text: format!("<instructions-update>\n{OPENING}\n{body}</instructions-update>\n"),
        changed,
    })
}

#[cfg(test)]
mod tests;
