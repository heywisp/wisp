use std::fmt;

use futures::future::BoxFuture;
use wisp_protocol::settings::{
    EndpointSummary, FileState, ModelEntry, SavedValue, SettingValue, SitePath,
};

use super::Given;
use crate::config::SavedSettings;
use crate::config_file::SiteEdit;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenSite {
    pub path: SitePath,
    pub value: Option<SettingValue>,
    pub line: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedFile {
    pub path_display: String,
    pub written: Vec<WrittenSite>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedRead {
    pub often: Vec<String>,
    pub saved: Vec<SavedValue>,
    pub file: FileState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostError(pub String);

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HostError {}

pub trait SettingsHost: Send + Sync {
    fn used(&self, _names: Vec<String>) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
    fn save(&self, edits: Vec<SiteEdit>) -> BoxFuture<'_, Result<SavedFile, HostError>>;
    fn read(&self) -> BoxFuture<'_, Result<SavedRead, HostError>>;
}

#[derive(Debug, Clone, Default)]
pub struct StartLayers {
    pub remembered: super::remembered::RememberedStore,
    pub forced: Vec<Given>,
    pub flags: Vec<Given>,
    pub env: Vec<Given>,
    pub saved: SavedSettings,
    pub endpoints: Vec<EndpointSummary>,
    pub models: Vec<ModelEntry>,
    pub full_screen: bool,
}

impl StartLayers {
    pub(crate) fn model_entries(&self) -> Vec<ModelEntry> {
        if !self.models.is_empty() {
            return self.models.clone();
        }
        super::shipped_models()
    }
}
