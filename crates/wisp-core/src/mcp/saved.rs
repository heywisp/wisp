use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use wisp_plugin_api::mcp::PromptArgument;

use super::keys::RunKeyOwner;
use crate::config::McpServer;
use crate::plan_usage::record::{self, Claim};
use crate::session::{self, RedactionKind, Redactions, sha256_bytes_hex, sha256_hex};
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
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    pub description: String,
    pub arguments: Vec<PromptArgument>,
}

impl From<rmcp::model::Prompt> for Prompt {
    fn from(prompt: rmcp::model::Prompt) -> Self {
        Self {
            name: prompt.name,
            title: prompt.title.unwrap_or_default(),
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

pub(crate) const HELD_CREDENTIAL: &str = "the server's list contained a held credential";

pub(super) fn has_secret(text: &str, held: &Redactions, redactions: &Redactions) -> bool {
    held.has_match(text, RedactionKind::Masked)
        || redactions.has_match(text, RedactionKind::LogOnly)
}

pub(super) fn json_has_secret(value: &Value, contains: &impl Fn(&str) -> bool) -> bool {
    match value {
        Value::String(text) => contains(text),
        Value::Array(items) => items.iter().any(|value| json_has_secret(value, contains)),
        Value::Object(map) => map
            .iter()
            .any(|(key, value)| contains(key) || json_has_secret(value, contains)),
        other => contains(&other.to_string()),
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

    pub(super) fn check_secrets(&self, redactions: &Redactions) -> Result<(), &'static str> {
        let held = session::held();
        let contains = |text: &str| has_secret(text, &held, redactions);
        if self.tools.iter().any(|tool| {
            contains(&tool.name)
                || contains(&tool.description)
                || json_has_secret(&tool.input_schema, &contains)
        }) || self.prompts.iter().any(|prompt| {
            contains(&prompt.name)
                || contains(&prompt.title)
                || contains(&prompt.description)
                || prompt
                    .arguments
                    .iter()
                    .any(|arg| contains(&arg.name) || contains(&arg.description))
        }) || self.resources.iter().any(|resource| {
            contains(&resource.uri) || contains(&resource.title) || contains(&resource.description)
        }) {
            Err(HELD_CREDENTIAL)
        } else {
            Ok(())
        }
    }

    pub(super) fn differs(&self, old: &Self) -> bool {
        canonical_specs(&self.tools) != canonical_specs(&old.tools)
            || self.prompts != old.prompts
            || self.resources != old.resources
    }
}

#[derive(Clone, Debug)]
pub(super) struct Saved {
    folder: Option<PathBuf>,
    key: String,
}

const LOCK_ERROR: &str = "could not lock its saved tool list";
const REMOVE_ERROR: &str = "could not remove its saved tool list";

impl Saved {
    pub(super) fn new(
        folder: Option<&Path>,
        name: &str,
        server: &McpServer,
        owner: Option<&RunKeyOwner>,
    ) -> Self {
        let key = match owner {
            Some(owner) => sha256_bytes_hex(
                &[
                    b"run-cache\0".as_slice(),
                    owner.identity().as_bytes(),
                    b"\0",
                    owner
                        .config
                        .as_deref()
                        .map_or(&[][..], |path| path.as_os_str().as_encoded_bytes()),
                ]
                .concat(),
            ),
            None => identity(name, server),
        };
        Self {
            folder: folder.map(Path::to_owned),
            key,
        }
    }

    pub(super) fn key(&self) -> &str {
        &self.key
    }

    fn read(&self) -> Option<List> {
        record::read_limited(self.folder.as_deref()?, &self.key, u64::MAX)
    }

    pub(super) fn load_safe(&self, redactions: &Redactions) -> Result<Option<List>, &'static str> {
        let Some(folder) = self.folder.as_deref() else {
            return Ok(None);
        };
        if self.read().is_none() {
            return Ok(None);
        }
        let _lock = match record::claim(folder, &self.key) {
            Claim::Held(lock) => lock,
            Claim::Unshared if self.contaminated(redactions) => return Err(LOCK_ERROR),
            Claim::Unshared | Claim::Busy => return Ok(None),
        };
        let Some(list) = self.read() else {
            return Ok(None);
        };
        if list.check_secrets(redactions).is_err() {
            self.remove()?;
            return Ok(None);
        }
        Ok(self.can_remove_in(folder).then_some(list))
    }

    fn can_remove_in(&self, folder: &Path) -> bool {
        let probe = folder.join(format!("{}.probe", self.key));
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options
            .open(&probe)
            .and_then(|file| {
                drop(file);
                std::fs::remove_file(probe)
            })
            .is_ok()
    }

    fn remove(&self) -> Result<(), &'static str> {
        let Some(folder) = self.folder.as_deref() else {
            return Ok(());
        };
        record::remove(folder, &self.key).map_err(|_| REMOVE_ERROR)
    }

    fn contaminated(&self, redactions: &Redactions) -> bool {
        self.read()
            .is_some_and(|list| list.check_secrets(redactions).is_err())
    }

    pub(super) fn try_evict_if_contaminated(
        &self,
        redactions: &Redactions,
    ) -> Result<(), &'static str> {
        if !self.contaminated(redactions) {
            return Ok(());
        }
        let folder = self.folder.as_deref().ok_or(LOCK_ERROR)?;
        let Claim::Held(_lock) = record::claim(folder, &self.key) else {
            return Err(LOCK_ERROR);
        };
        if self.contaminated(redactions) {
            self.remove()?;
        }
        Ok(())
    }

    pub(super) async fn evict_if_contaminated(
        &self,
        redactions: &Redactions,
    ) -> Result<(), &'static str> {
        if self.contaminated(redactions) {
            let edit = self.edit().await?;
            if self.contaminated(redactions) {
                return edit.remove();
            }
        }
        Ok(())
    }

    async fn lock(&self) -> Result<std::fs::File, &'static str> {
        let folder = self.folder.as_deref().ok_or(LOCK_ERROR)?;
        loop {
            match record::claim(folder, &self.key) {
                Claim::Held(lock) => return Ok(lock),
                Claim::Busy => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
                Claim::Unshared => return Err(LOCK_ERROR),
            }
        }
    }

    pub(super) async fn edit(&self) -> Result<Edit, &'static str> {
        let lock = if self.folder.is_some() {
            Some(self.lock().await?)
        } else {
            None
        };
        Ok(Edit {
            saved: self.clone(),
            _lock: lock,
        })
    }

    pub(super) async fn write(
        &self,
        list: &List,
        redactions: &Redactions,
    ) -> Result<(), &'static str> {
        let Some(folder) = self.folder.as_deref() else {
            return Ok(());
        };
        let _lock = self.lock().await?;
        list.check_secrets(redactions)?;
        record::write(folder, &self.key, list).map_err(|_| "could not save its tool list")
    }
}

pub(super) struct Edit {
    saved: Saved,
    _lock: Option<std::fs::File>,
}

impl Edit {
    pub(super) fn remove(self) -> Result<(), &'static str> {
        self.saved.remove()
    }

    pub(super) fn commit(
        self,
        list: &List,
        redactions: &Redactions,
        persist: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        list.check_secrets(redactions)?;
        let Some(folder) = self.saved.folder.as_deref() else {
            return persist();
        };
        let key = &self.saved.key;
        let old = self
            .saved
            .read()
            .filter(|old| old.check_secrets(redactions).is_ok());
        record::write(folder, key, list).map_err(|_| "could not save its tool list")?;
        if let Err(error) = persist() {
            let _ = match old {
                Some(old) => record::write(folder, key, &old),
                None => record::remove(folder, key),
            };
            return Err(error);
        }
        Ok(())
    }
}

pub(super) fn identity(name: &str, server: &McpServer) -> String {
    sha256_hex(&serde_json::json!([name, server.command, server.args]).to_string())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "test setup must produce a readable catalog"
)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_staged_catalog_is_rechecked_at_commit() {
        let folder = tempfile::tempdir().unwrap();
        let cache = Saved::new(
            Some(folder.path()),
            "local",
            &McpServer {
                command: "adapter".into(),
                args: Vec::new(),
                keys: Vec::new(),
            },
            None,
        );
        let clean = List::new(Vec::new(), Vec::new(), Vec::new(), None);
        let redactions = Redactions::default();
        cache.write(&clean, &redactions).await.unwrap();
        let mut staged = clean.clone();
        const LATE: &str = "CANARY-discovery-credential-held-at-commit";
        staged.resources.push(Resource {
            uri: format!("test:///{LATE}"),
            title: "Notes".into(),
            description: String::new(),
        });
        assert!(staged.check_secrets(&redactions).is_ok());
        let edit = cache.edit().await.unwrap();
        session::register_secrets([LATE.to_owned()]);
        assert_eq!(
            edit.commit(&staged, &redactions, || Err("should not persist".into())),
            Err(HELD_CREDENTIAL.into())
        );
        assert_eq!(cache.read(), Some(clean));
    }
    #[test]
    fn loading_an_absent_list_does_not_create_cache_files() {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("mcp");
        let cache = Saved::new(
            Some(&folder),
            "local",
            &McpServer {
                command: "adapter".into(),
                args: Vec::new(),
                keys: Vec::new(),
            },
            None,
        );
        assert_eq!(cache.load_safe(&Redactions::default()), Ok(None));
        assert!(!folder.exists());
        std::fs::create_dir(&folder).unwrap();
        assert_eq!(cache.load_safe(&Redactions::default()), Ok(None));
        assert_eq!(std::fs::read_dir(folder).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn eviction_rechecks_the_catalog_after_a_concurrent_replacement() {
        let folder = tempfile::tempdir().unwrap();
        let cache = Saved::new(
            Some(folder.path()),
            "local",
            &McpServer {
                command: "adapter".into(),
                args: Vec::new(),
                keys: Vec::new(),
            },
            None,
        );
        let clean = List::new(Vec::new(), Vec::new(), Vec::new(), None);
        let mut contaminated = clean.clone();
        const KEY: &str = "CANARY:cache:credential";
        let redactions = Redactions::new([KEY.to_owned()]);
        contaminated.resources.push(Resource {
            uri: format!("test:///{KEY}"),
            title: String::new(),
            description: String::new(),
        });
        let edit = cache.edit().await.unwrap();
        record::write(folder.path(), &cache.key, &contaminated).unwrap();
        assert_eq!(
            cache.try_evict_if_contaminated(&redactions),
            Err(LOCK_ERROR)
        );
        assert_eq!(cache.read(), Some(contaminated));
        let mut evict = std::pin::pin!(cache.evict_if_contaminated(&redactions));
        assert!(futures::poll!(&mut evict).is_pending());
        edit.commit(&clean, &redactions, || Ok(())).unwrap();
        assert_eq!(evict.await, Ok(()));
        assert_eq!(cache.read(), Some(clean));
    }
}
