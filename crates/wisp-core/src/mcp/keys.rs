use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError};

use serde::{Deserialize, Serialize};
use wisp_protocol::Secret;

use super::{
    McpServer, Service, State, check_pinned_config, check_pinned_run, run_definition, saved, text,
};
use crate::config::MCP_CONFIG_FILE;
use crate::oauth;
use crate::secrets::StoreKind;
use crate::session::Redactions;

const STORE_ERROR: &str = "couldn't use the server's key store";

#[derive(Serialize, Deserialize)]
struct Kept {
    identity: String,
    keys: BTreeMap<String, Secret>,
}

fn profile(config: &Path, name: &str) -> Result<PathBuf, String> {
    oauth::mcp_profile(config, &format!("keys/{name}")).map_err(|_| STORE_ERROR.into())
}

fn read(path: &Path, identity: &str) -> Result<BTreeMap<String, Secret>, String> {
    let kept: Option<Kept> = oauth::read_value(path).map_err(|_| STORE_ERROR)?;
    crate::session::register_secrets(
        kept.iter()
            .flat_map(|kept| kept.keys.values())
            .map(|value| value.expose().to_owned()),
    );
    Ok(kept
        .filter(|kept| kept.identity == identity)
        .map(|kept| kept.keys)
        .unwrap_or_default())
}

fn keep(
    config: &Path,
    name: &str,
    server: &McpServer,
    typed: BTreeMap<String, Secret>,
    store: StoreKind,
) -> Result<(), String> {
    let path = profile(config, name)?;
    let locks = oauth::lock_profile(&path).map_err(|_| STORE_ERROR)?;
    keep_locked(&path, &saved::identity(name, server), typed, store, &locks)
}

fn keep_locked(
    path: &Path,
    identity: &str,
    typed: BTreeMap<String, Secret>,
    store: StoreKind,
    locks: &crate::secrets::Locks,
) -> Result<(), String> {
    let mut keys = read(path, identity)?;
    keys.extend(typed);
    oauth::persist_value_locked(
        path,
        &Kept {
            identity: identity.to_owned(),
            keys,
        },
        store,
        locks,
    )
    .map_err(|_| STORE_ERROR.into())
}

pub(super) fn clear(
    config: &Path,
    name: &str,
    locks: &crate::secrets::Locks,
) -> Result<(), String> {
    oauth::clear_value_locked(&profile(config, name)?, locks).map_err(|_| STORE_ERROR.into())
}

pub(super) fn missing(server: &McpServer, keys: &BTreeMap<String, Secret>) -> Vec<String> {
    server
        .keys
        .iter()
        .filter(|name| !keys.contains_key(*name) && !crate::secrets::is_set(name))
        .cloned()
        .collect()
}

pub(super) struct RunKeyOwner {
    identity: String,
    pub(super) run: PathBuf,
    pub(super) cwd: PathBuf,
    pub(super) config: Option<PathBuf>,
}

impl RunKeyOwner {
    pub(super) fn new(
        name: &str,
        server: &McpServer,
        cwd: PathBuf,
        run: PathBuf,
        config: Option<PathBuf>,
    ) -> Self {
        let identity = crate::session::sha256_bytes_hex(
            &[
                saved::identity(name, server).as_bytes(),
                &[0],
                cwd.as_os_str().as_encoded_bytes(),
                &[0],
                run.as_os_str().as_encoded_bytes(),
            ]
            .concat(),
        );
        Self {
            identity,
            run,
            cwd,
            config,
        }
    }

    pub(super) fn identity(&self) -> &str {
        &self.identity
    }

    pub(super) fn check_paths(&self) -> Result<(), String> {
        check_pinned_run(&self.run)?;
        if !std::fs::canonicalize(&self.cwd).is_ok_and(|cwd| cwd == self.cwd) {
            return Err("the server's working directory has moved".into());
        }
        if let Some(config) = &self.config {
            check_pinned_config(config)?;
        }
        Ok(())
    }

    pub(super) fn clear(&self, config: &Path, locks: &crate::secrets::Locks) -> Result<(), String> {
        let path = self.path(config)?;
        oauth::clear_value_locked(&path, locks).map_err(|_| STORE_ERROR.into())
    }

    fn path(&self, config: &Path) -> Result<PathBuf, String> {
        oauth::mcp_profile_at(config, &format!("run-keys/{}", self.identity))
            .map_err(|_| STORE_ERROR.into())
    }

    fn stored_path(&self, config: &Path) -> Result<Option<PathBuf>, String> {
        let folder = config
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        if !folder.try_exists().map_err(|_| STORE_ERROR)? {
            return Ok(None);
        }
        let path = self.path(config)?;
        match path.symlink_metadata() {
            Ok(_) => Ok(Some(path)),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(_) => Err(STORE_ERROR.into()),
        }
    }

    pub(super) fn lock(
        &self,
        path: &Path,
        config: &Path,
        name: &str,
        server: &McpServer,
    ) -> Result<crate::secrets::Locks, String> {
        let locks = oauth::lock_profile_with_config(path, Some(config)).map_err(|_| STORE_ERROR)?;
        self.check_paths()?;
        let current = run_definition(config, &self.run, name)?;
        if current.command != server.command || current.args != server.args {
            return Err(format!("the server has changed in {MCP_CONFIG_FILE}"));
        }
        Ok(locks)
    }
}

struct RunKeyStore {
    path: PathBuf,
    identity: String,
    store: StoreKind,
    locks: crate::secrets::Locks,
}

impl RunKeyStore {
    fn open(
        config: &Path,
        name: &str,
        server: &McpServer,
        owner: &RunKeyOwner,
        store: StoreKind,
    ) -> Result<Self, String> {
        owner.check_paths()?;
        let path = owner.path(config)?;
        let locks = owner.lock(&path, config, name, server)?;
        Ok(Self {
            path,
            identity: owner.identity.clone(),
            store,
            locks,
        })
    }

    fn keep(&self, typed: BTreeMap<String, Secret>) -> bool {
        keep_locked(&self.path, &self.identity, typed, self.store, &self.locks).is_ok()
    }
}

impl Service {
    pub(super) async fn commit_run_keys(
        &self,
        name: &str,
        server: &McpServer,
        owner: Arc<RunKeyOwner>,
        typed: BTreeMap<String, Secret>,
        cache: Option<(saved::Edit, saved::List)>,
        redactions: Redactions,
    ) -> Result<bool, String> {
        let (_, kind) = self.credentials();
        let (name, server) = (name.to_owned(), server.clone());
        tokio::task::spawn_blocking(move || {
            let store = owner
                .config
                .as_deref()
                .map(|config| RunKeyStore::open(config, &name, &server, &owner, kind))
                .transpose()?;
            let mut kept = typed.is_empty();
            let save = || {
                if let Some(store) = &store
                    && !typed.is_empty()
                {
                    kept = store.keep(typed);
                }
                Ok(())
            };
            match cache {
                Some((edit, list)) => edit.commit(&list, &redactions, save)?,
                None => save()?,
            }
            Ok(kept)
        })
        .await
        .map_err(|_| STORE_ERROR.to_owned())?
    }

    pub(super) async fn keys_for(
        &self,
        name: &str,
        server: &McpServer,
        owner: Option<Arc<RunKeyOwner>>,
    ) -> Result<BTreeMap<String, Secret>, String> {
        if server.keys.is_empty() {
            return Ok(BTreeMap::new());
        }
        let (saved_config, _) = self.credentials();
        let (name, server) = (name.to_owned(), server.clone());
        tokio::task::spawn_blocking(move || {
            if let Some(owner) = owner {
                owner.check_paths()?;
                let Some(config) = owner.config.as_deref() else {
                    return Ok(BTreeMap::new());
                };
                let Some(path) = owner.stored_path(config)? else {
                    return Ok(BTreeMap::new());
                };
                let _locks = owner.lock(&path, config, &name, &server)?;
                return read(&path, &owner.identity);
            }
            match saved_config {
                Some(config) => read(&profile(&config, &name)?, &saved::identity(&name, &server)),
                None => Ok(BTreeMap::new()),
            }
        })
        .await
        .map_err(|_| STORE_ERROR.to_owned())?
    }

    pub(crate) async fn missing_keys(
        &self,
        name: &str,
        server: &McpServer,
    ) -> Result<Vec<String>, String> {
        Ok(missing(server, &self.keys_for(name, server, None).await?))
    }

    pub(super) async fn keep_keys(
        &self,
        name: String,
        server: McpServer,
        typed: BTreeMap<String, Secret>,
    ) -> bool {
        if typed.is_empty() {
            return true;
        }
        let (config, store) = self.credentials();
        let Some(config) = config else {
            return false;
        };
        tokio::task::spawn_blocking(move || keep(&config, &name, &server, typed, store))
            .await
            .is_ok_and(|result| result.is_ok())
    }

    pub async fn refresh_keys(&self) {
        let servers: Vec<_> = self
            .servers()
            .iter()
            .map(|(name, server)| (name.clone(), server.clone()))
            .collect();
        for (name, server) in &servers {
            if super::is_address(&server.config.command) {
                if let (Some(config), _) = self.credentials_for(server) {
                    let server = server.clone();
                    let name = name.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        super::sign_in::hold_stored(
                            &config,
                            &name,
                            &server.config,
                            server.owner.as_deref(),
                        );
                    })
                    .await;
                }
            } else if !server.config.keys.is_empty() {
                let mut found = self
                    .keys_for(name, &server.config, server.owner.clone())
                    .await
                    .unwrap_or_default();
                found.extend(
                    server
                        .keys
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .clone(),
                );
                let missing = missing(&server.config, &found);
                *server.redactions() =
                    Redactions::new(found.values().map(|key| key.expose().to_owned()));
                let mut snapshot = server.snapshot();
                if matches!(snapshot.state, State::Stopped | State::NeedsKey(_)) {
                    snapshot.state = if missing.is_empty() {
                        State::Stopped
                    } else {
                        State::NeedsKey(missing)
                    };
                }
            }
        }
        for (_, server) in servers {
            if self.evict_catalog(&server).await.is_err() {
                continue;
            }
            let mut snapshot = server.snapshot();
            if snapshot
                .list
                .as_ref()
                .is_some_and(|list| list.check_secrets(&self.live_redactions(&server)).is_err())
            {
                snapshot.list = None;
                snapshot.listed = None;
                self.0
                    .revision
                    .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                self.0
                    .changes
                    .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            }
        }
    }

    pub async fn set_keys(
        &self,
        name: &str,
        typed: BTreeMap<String, Secret>,
    ) -> Result<super::KeyResult, String> {
        self.set_keys_until(name, typed, std::future::pending())
            .await
    }

    pub(crate) async fn set_keys_until(
        &self,
        name: &str,
        typed: BTreeMap<String, Secret>,
        cancelled: impl std::future::Future<Output = ()> + Send,
    ) -> Result<super::KeyResult, String> {
        let mutation = self.0.mutation.clone().lock_owned().await;
        let old = self.server(name)?;
        if super::is_address(&old.config.command) {
            return Err(text::ADDRESS_KEYS.into());
        }
        if typed.keys().any(|key| !wisp_protocol::plain_variable(key)) {
            return Err(text::KEY_VARIABLE.into());
        }
        if old.owner.is_some() && typed.keys().any(|key| !old.config.keys.contains(key)) {
            return Err(format!("name the server's keys in {MCP_CONFIG_FILE} first"));
        }
        let names: Vec<_> = typed.keys().cloned().collect();
        let mut config = old.config.clone();
        for key in &names {
            if !config.keys.contains(key) {
                config.keys.push(key.clone());
            }
        }
        let mut found = self.keys_for(name, &config, old.owner.clone()).await?;
        found.extend(
            old.keys
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
        );
        found.extend(typed.clone());
        *old.connection.lock().await = None;
        let staged = self.staged(name, &config, old.owner.clone(), found);
        let result = tokio::select! {
            biased;
            () = cancelled => return Err(wisp_tool_text::manage::NO_ANSWER.into()),
            result = staged.connect(name) => result,
        };
        if let Err(reason) = result {
            old.snapshot().state = State::Failed(reason.clone());
            return Err(text::rekey_failed(name, &names, &reason));
        }
        let service = self.clone();
        let name = name.to_owned();
        tokio::spawn(async move {
            let _mutation = mutation;
            let result = service
                .finish_server(&name, staged, typed, None, |_| {
                    service.persist(&name, super::Edit::Keys(&names), None)
                })
                .await;
            if let Err(reason) = &result {
                old.snapshot().state = State::Failed(reason.clone());
            }
            result
        })
        .await
        .map_err(|_| "could not set the server's key".to_owned())?
    }
}
