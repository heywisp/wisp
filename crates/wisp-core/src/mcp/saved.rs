use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use wisp_plugin_api::mcp::PromptArgument;

use crate::config::McpServer;
use crate::plan_usage::record::{self, Claim};
use crate::session::sha256_hex;
use crate::tool::{ToolSpec, canonical_specs};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct List {
    pub tools: Vec<ToolSpec>,
    pub prompts: Vec<Prompt>,
    pub resources: Vec<Resource>,
    pub observed: u64,
    pub ttl_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prompt {
    pub name: String,
    pub description: String,
    pub arguments: Vec<PromptArgument>,
}

impl From<rmcp::model::Prompt> for Prompt {
    fn from(prompt: rmcp::model::Prompt) -> Self {
        Self {
            name: prompt.name,
            description: prompt.description.unwrap_or_default(),
            arguments: prompt
                .arguments
                .unwrap_or_default()
                .into_iter()
                .map(|arg| PromptArgument {
                    name: arg.name,
                    required: arg.required.unwrap_or(false),
                    description: arg.description.unwrap_or_default(),
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resource {
    pub uri: String,
    pub title: String,
    pub description: String,
}

impl From<rmcp::model::Resource> for Resource {
    fn from(resource: rmcp::model::Resource) -> Self {
        Self {
            uri: resource.uri,
            title: resource.title.unwrap_or(resource.name),
            description: resource.description.unwrap_or_default(),
        }
    }
}

impl List {
    pub(super) fn new(
        tools: Vec<ToolSpec>,
        prompts: Vec<Prompt>,
        resources: Vec<Resource>,
        ttl_ms: Option<u64>,
    ) -> Self {
        Self {
            tools,
            prompts,
            resources,
            observed: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            ttl_ms,
        }
    }

    pub(super) fn differs(&self, old: &Self) -> bool {
        canonical_specs(&self.tools) != canonical_specs(&old.tools)
            || self.prompts != old.prompts
            || self.resources != old.resources
    }
}

#[derive(Debug)]
pub(super) struct Saved {
    folder: Option<PathBuf>,
    key: String,
}

impl Saved {
    pub(super) fn new(folder: Option<&Path>, name: &str, server: &McpServer) -> Self {
        Self {
            folder: folder.map(Path::to_owned),
            key: identity(name, server),
        }
    }

    pub(super) fn read(&self) -> Option<List> {
        record::read_limited(self.folder.as_deref()?, &self.key, u64::MAX)
    }

    async fn lock(&self) -> Result<std::fs::File, &'static str> {
        let folder = self
            .folder
            .as_deref()
            .ok_or("could not lock its saved tool list")?;
        loop {
            match record::claim(folder, &self.key) {
                Claim::Held(lock) => return Ok(lock),
                Claim::Busy => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
                Claim::Unshared => return Err("could not lock its saved tool list"),
            }
        }
    }

    pub(super) async fn remove(&self) -> Result<(), &'static str> {
        let Some(folder) = self.folder.as_deref() else {
            return Ok(());
        };
        let _lock = self.lock().await?;
        record::remove(folder, &self.key).map_err(|_| "could not remove its saved tool list")
    }

    pub(super) async fn commit(
        &self,
        list: Option<&List>,
        persist: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        let Some((folder, list)) = self.folder.as_deref().zip(list) else {
            return persist();
        };
        let _lock = self.lock().await?;
        let old = self.read();
        record::write(folder, &self.key, list).map_err(|_| "could not save its tool list")?;
        if let Err(error) = persist() {
            let _ = match old {
                Some(old) => record::write(folder, &self.key, &old),
                None => record::remove(folder, &self.key),
            };
            return Err(error);
        }
        Ok(())
    }

    pub(super) async fn write(&self, list: &List) -> Result<(), &'static str> {
        let Some(folder) = self.folder.as_deref() else {
            return Ok(());
        };
        let _lock = self.lock().await?;
        record::write(folder, &self.key, list).map_err(|_| "could not save its tool list")
    }
}

pub(super) fn identity(name: &str, server: &McpServer) -> String {
    sha256_hex(&serde_json::json!([name, server.command, server.args]).to_string())
}
