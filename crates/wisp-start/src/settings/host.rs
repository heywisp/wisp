use std::path::{Path, PathBuf};

use futures::FutureExt;
use futures::future::BoxFuture;
use wisp_core::config_file::{SiteEdit, save};
use wisp_core::endpoints::{ConfigFile, GIVEN_CONFIG, PathShown};
use wisp_core::secrets::LOCK_WAIT;
use wisp_core::settings::host::{HostError, SavedFile, SavedRead, SettingsHost, WrittenSite};
use wisp_protocol::settings::{FileState, SavedValue, SettingValue, names, site_path};

use crate::endpoints::{self, note_words};

use super::used::Used;

pub struct FileHost {
    pub used_path: Option<PathBuf>,
    pub path: PathBuf,
    pub shown: PathShown,
    pub home: Option<String>,
}

impl FileHost {
    fn display(&self) -> String {
        shown_path(&self.path, self.shown, self.home.as_deref())
    }
}

pub fn shown_path(path: &Path, shown: PathShown, home: Option<&str>) -> String {
    match shown {
        PathShown::Given => GIVEN_CONFIG.to_owned(),
        PathShown::Default => endpoints::short(path, home),
    }
}

fn line_of(config: Option<&ConfigFile>, edit: &SiteEdit) -> Option<u64> {
    config
        .and_then(|c| c.settings.get(edit.path()))
        .and_then(|saved| u64::try_from(saved.line).ok())
}

pub fn write(
    path: &Path,
    shown: PathShown,
    home: Option<&str>,
    edits: &[SiteEdit],
) -> Result<SavedFile, HostError> {
    let outcome = save(path, shown, edits, LOCK_WAIT).map_err(|e| HostError(e.to_string()))?;
    let after = endpoints::read_config(path, shown == PathShown::Given)
        .ok()
        .flatten();
    let written = edits
        .iter()
        .map(|edit| WrittenSite {
            path: edit.path().clone(),
            value: match edit {
                SiteEdit::Set(_, value) => Some(value.clone()),
                SiteEdit::Unset(_) => None,
            },
            line: line_of(after.as_ref(), edit),
        })
        .collect();
    Ok(SavedFile {
        path_display: shown_path(path, shown, home),
        written,
        notes: outcome
            .notes
            .iter()
            .map(|note| note_words(note, home))
            .collect(),
    })
}

pub fn saved_values(config: &ConfigFile) -> Vec<SavedValue> {
    let mut saved: Vec<SavedValue> = config
        .settings
        .iter()
        .map(|(path, entry)| {
            SavedValue::of(
                path.clone(),
                entry.value.clone(),
                u64::try_from(entry.line).ok(),
            )
        })
        .collect();
    if let (Some(name), Some(path)) = (&config.default, site_path(names::ENDPOINT)) {
        saved.push(SavedValue::of(
            path,
            SettingValue::Text(name.to_string()),
            None,
        ));
    }
    saved.sort_by(|a, b| a.path.cmp(&b.path));
    saved
}

pub fn read(path: &Path, shown: PathShown, home: Option<&str>) -> SavedRead {
    read_with_config(path, shown, home).0
}

pub fn read_with_config(
    path: &Path,
    shown: PathShown,
    home: Option<&str>,
) -> (SavedRead, Option<ConfigFile>) {
    let missing = SavedRead {
        often: Vec::new(),
        saved: Vec::new(),
        file: FileState::Missing,
    };
    if std::fs::symlink_metadata(path).is_err() {
        return (missing, None);
    }
    match endpoints::read_config(path, shown == PathShown::Given) {
        Ok(Some(config)) => (
            SavedRead {
                often: Vec::new(),
                saved: saved_values(&config),
                file: FileState::Ok {
                    path_display: shown_path(path, shown, home),
                },
            },
            Some(config),
        ),
        Ok(None) => (missing, None),
        Err(reason) => (
            SavedRead {
                often: Vec::new(),
                saved: Vec::new(),
                file: FileState::Broken { line: None, reason },
            },
            None,
        ),
    }
}

fn today() -> u64 {
    u64::try_from(jiff::Timestamp::now().as_second()).unwrap_or_default() / 86_400
}

impl SettingsHost for FileHost {
    fn used(&self, names: Vec<String>) -> BoxFuture<'_, ()> {
        let path = self.used_path.clone();
        async move {
            if let Some(path) = path {
                let _ =
                    tokio::task::spawn_blocking(move || Used::bump(&path, &names, today())).await;
            }
        }
        .boxed()
    }

    fn save(&self, edits: Vec<SiteEdit>) -> BoxFuture<'_, Result<SavedFile, HostError>> {
        let path = self.path.clone();
        let shown = self.shown;
        let home = self.home.clone();
        let display = self.display();
        async move {
            tokio::task::spawn_blocking(move || write(&path, shown, home.as_deref(), &edits))
                .await
                .map_err(|e| HostError(format!("could not save {display}: {e}")))?
        }
        .boxed()
    }

    fn read(&self) -> BoxFuture<'_, Result<SavedRead, HostError>> {
        let used_path = self.used_path.clone();
        let path = self.path.clone();
        let shown = self.shown;
        let home = self.home.clone();
        let display = self.display();
        async move {
            tokio::task::spawn_blocking(move || {
                let mut read = read(&path, shown, home.as_deref());
                if let Some(path) = used_path {
                    read.often = Used::load(&path, today()).ranked();
                }
                read
            })
            .await
            .map_err(|e| HostError(format!("could not read {display}: {e}")))
        }
        .boxed()
    }
}

#[cfg(test)]
mod tests;
