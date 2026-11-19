use std::cell::{OnceCell, RefCell};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use wisp_instructions::delta::{self, Entry, Item, Now, Roots};
use wisp_instructions::{FILE_NAME, Files, Loaded, Meta, ProblemKind, sha256_hex};
use wisp_plugin_api::host::Host;
use wisp_plugin_api::instructions::{Change, Diagnostic, Problem, SourceInfo};
use wisp_plugin_api::prompt::{Prior, Rendered};

use super::HostFiles;

pub(crate) const RACY_NANOS: i64 = 2_000_000_000;

#[derive(Debug, Clone)]
struct Read {
    len: u64,
    mtime: Option<i64>,
    read_at: Option<i64>,
    bytes: Vec<u8>,
}

impl Read {
    fn trusted(&self, meta: &Meta) -> bool {
        let clean = match (self.mtime, self.read_at) {
            (Some(mtime), Some(at)) => mtime < at.saturating_sub(RACY_NANOS),
            _ => false,
        };
        clean && self.len == meta.len && self.mtime == meta.mtime
    }
}

#[derive(Debug, Default)]
pub(crate) struct State {
    reads: BTreeMap<PathBuf, Read>,
    store: BTreeMap<String, String>,
    unsaved: Option<(Prior, Vec<String>)>,
}

impl State {
    pub(crate) fn keep(&mut self, loaded: &Loaded) {
        for file in &loaded.files {
            self.store
                .entry(sha256_hex(&file.text))
                .or_insert_with(|| file.text.clone());
        }
    }

    pub(crate) fn prune(&mut self, previous: &Prior, rendered: &Rendered) {
        let rendered_shas = delta::entries(&rendered.snapshot)
            .into_iter()
            .map(|e| e.sha256);
        let mut unsaved = match self.unsaved.take() {
            Some((base, shas)) if base == *previous => shas,
            _ => Vec::new(),
        };
        if rendered.text.is_some() {
            unsaved.extend(rendered_shas.clone());
        }
        let kept: Vec<String> = delta::prior_entries(previous)
            .unwrap_or_default()
            .into_iter()
            .map(|e| e.sha256)
            .chain(rendered_shas)
            .chain(unsaved.iter().cloned())
            .collect();
        self.store.retain(|sha, _| kept.contains(sha));
        self.unsaved = Some((previous.clone(), unsaved));
    }

    fn target(&mut self, target: &str) -> (Vec<Entry>, String) {
        if delta::is_snapshot(target) {
            return (delta::entries(target), target.to_owned());
        }
        let files = delta::legacy_files(target);
        let entries: Vec<Entry> = files
            .into_iter()
            .map(|(entry, text)| {
                self.store.entry(entry.sha256.clone()).or_insert(text);
                entry
            })
            .collect();
        let snapshot = delta::snapshot_of(&entries);
        (entries, snapshot)
    }
}

fn now_ns(host: &dyn Host) -> Option<i64> {
    let now = host.now();
    if now.seconds == 0 && now.nanos == 0 {
        return None;
    }
    now.seconds
        .checked_mul(1_000_000_000)?
        .checked_add(i64::from(now.nanos))
}

pub(crate) struct CachedFiles<'a> {
    files: HostFiles<'a>,
    reads: RefCell<BTreeMap<PathBuf, Read>>,
    now: OnceCell<Option<i64>>,
}

impl<'a> CachedFiles<'a> {
    pub(crate) fn new(files: HostFiles<'a>, state: &mut State) -> Self {
        Self {
            files,
            reads: RefCell::new(std::mem::take(&mut state.reads)),
            now: OnceCell::new(),
        }
    }

    pub(crate) fn finish(self, state: &mut State) {
        state.reads = self.reads.into_inner();
    }
}

impl Files for CachedFiles<'_> {
    fn metadata(&self, path: &Path) -> io::Result<Meta> {
        self.files.metadata(path)
    }

    fn exists(&self, path: &Path) -> bool {
        self.files.exists(path)
    }

    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        self.files.canonicalize(path)
    }

    fn read_capped(&self, path: &Path, limit: u64) -> io::Result<Option<Vec<u8>>> {
        self.files.read_capped(path, limit)
    }

    fn read(&self, path: &Path) -> io::Result<Option<Vec<u8>>> {
        let meta = self.files.metadata(path)?;
        if let Some(read) = self.reads.borrow().get(path)
            && read.trusted(&meta)
        {
            return Ok(Some(read.bytes.clone()));
        }
        let read_at = *self.now.get_or_init(|| now_ns(self.files.host));
        let bytes = self.files.read(path)?;
        match &bytes {
            Some(raw) => {
                self.reads.borrow_mut().insert(
                    path.to_owned(),
                    Read {
                        len: meta.len,
                        mtime: meta.mtime,
                        read_at,
                        bytes: raw.clone(),
                    },
                );
            }
            None => {
                self.reads.borrow_mut().remove(path);
            }
        }
        Ok(bytes)
    }
}

fn unreadable(loaded: &Loaded, shown: &str) -> bool {
    loaded.summary.diagnostics.iter().any(|d| {
        matches!(
            d,
            Diagnostic::Problem(Problem {
                source: Some(source),
                kind: ProblemKind::NotUtf8 | ProblemKind::Unreadable | ProblemKind::NotAFile,
                ..
            }) if source == shown
        )
    })
}

fn target_item(entry: &Entry, loaded: &Loaded, state: &State, roots: &Roots<'_>) -> Item {
    let file = loaded
        .files
        .iter()
        .find(|f| f.scope == entry.scope && f.name == entry.name);
    let display_path = file.map_or_else(
        || roots.shown(entry.scope, &entry.name),
        |f| f.display_path.clone(),
    );
    let now = match (state.store.get(&entry.sha256), file) {
        (Some(text), _) => Now::Text {
            sha256: entry.sha256.clone(),
            text: text.clone(),
        },
        (None, Some(f)) => Now::Text {
            sha256: sha256_hex(&f.text),
            text: f.text.clone(),
        },
        (None, None) => Now::Gone {
            unreadable: unreadable(loaded, &display_path),
        },
    };
    Item {
        scope: entry.scope,
        name: entry.name.clone(),
        display_path,
        now,
    }
}

fn from_info(old: &[Entry], state: &State, roots: &Roots<'_>) -> Option<SourceInfo> {
    let chosen = old
        .iter()
        .find(|e| e.name == FILE_NAME)
        .or_else(|| old.first())?;
    Some(SourceInfo {
        scope: chosen.scope,
        name: chosen.name.clone(),
        display_path: roots.shown(chosen.scope, &chosen.name),
        bytes: state
            .store
            .get(&chosen.sha256)
            .map_or(0, |t| t.len() as u64),
        sha256: chosen.sha256.clone(),
        imported_by: None,
        external: false,
    })
}

pub(crate) fn render(
    host: &dyn Host,
    state: &mut State,
    loaded: &Loaded,
    previous: &Prior,
    target: Option<&str>,
) -> (Rendered, Option<Change>) {
    let target = target.map(|t| state.target(t));
    let snapshot = target
        .as_ref()
        .map_or_else(|| delta::snapshot(&loaded.files), |(_, s)| s.clone());
    let state = &*state;
    let none = |snapshot: String| {
        (
            Rendered {
                snapshot,
                text: None,
            },
            None,
        )
    };
    let Some(old) = delta::prior_entries(previous) else {
        return none(snapshot);
    };
    let config_dir = host.config_dir().map(PathBuf::from);
    let home = host.home().map(PathBuf::from);
    let roots = Roots {
        project: &loaded.root,
        config_dir: config_dir.as_deref(),
        home: home.as_deref().filter(|h| h.is_absolute()),
    };
    let items: Vec<Item> = match &target {
        None => delta::items_of(&loaded.files),
        Some((entries, _)) => entries
            .iter()
            .map(|e| target_item(e, loaded, state, &roots))
            .collect(),
    };
    let left = |entry: &Entry| {
        let file = loaded
            .files
            .iter()
            .find(|f| f.scope == entry.scope && f.name == entry.name);
        if file.is_some_and(|f| sha256_hex(&f.text) == entry.sha256) {
            return None;
        }
        let shown = file.map_or_else(
            || roots.shown(entry.scope, &entry.name),
            |f| f.display_path.clone(),
        );
        let broken = unreadable(loaded, &shown);
        Some((shown, broken))
    };
    let Some(update) = delta::update(&old, &items, left) else {
        return none(snapshot);
    };
    let mut summary = loaded.summary.clone();
    if target.is_some() {
        summary.diagnostics.clear();
    }
    let change = Change {
        from: from_info(&old, state, &roots),
        to: loaded.summary.chosen.clone(),
        changed: update.changed,
        summary,
    };
    (
        Rendered {
            snapshot,
            text: Some(update.text),
        },
        Some(change),
    )
}
