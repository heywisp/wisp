use std::collections::BTreeMap;
use std::path::Path;
use std::sync::PoisonError;

use serde::{Deserialize, Serialize};
use wisp_protocol::Secret;

use super::{McpServer, Service, State, saved, text};
use crate::oauth;
use crate::secrets::StoreKind;

const STORE_ERROR: &str = "couldn't use the server's key store";

#[derive(Serialize, Deserialize)]
struct Kept {
    identity: String,
    keys: BTreeMap<String, Secret>,
}

fn profile(config: &Path, name: &str) -> Result<std::path::PathBuf, String> {
    oauth::mcp_profile(config, &format!("keys/{name}")).map_err(|_| STORE_ERROR.into())
}

fn read(config: &Path, name: &str, server: &McpServer) -> Result<BTreeMap<String, Secret>, String> {
    let path = profile(config, name)?;
    let kept: Option<Kept> = oauth::read_value(&path).map_err(|_| STORE_ERROR)?;
    crate::session::register_secrets(
        kept.iter()
            .flat_map(|kept| kept.keys.values())
            .map(|value| value.expose().to_owned()),
    );
    Ok(kept
        .filter(|kept| kept.identity == saved::identity(name, server))
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
    let mut keys = read(config, name, server)?;
    keys.extend(typed);
    oauth::persist_value_locked(
        &path,
        &Kept {
            identity: saved::identity(name, server),
            keys,
        },
        store,
        &locks,
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

impl Service {
    pub(super) async fn keys_for(
        &self,
        name: &str,
        server: &McpServer,
    ) -> Result<BTreeMap<String, Secret>, String> {
        if server.keys.is_empty() {
            return Ok(BTreeMap::new());
        }
        let (config, _) = self.credentials();
        let Some(config) = config else {
            return Ok(BTreeMap::new());
        };
        let name = name.to_owned();
        let server = server.clone();
        tokio::task::spawn_blocking(move || read(&config, &name, &server))
            .await
            .map_err(|_| STORE_ERROR.to_owned())?
    }

    pub(crate) async fn missing_keys(
        &self,
        name: &str,
        server: &McpServer,
    ) -> Result<Vec<String>, String> {
        Ok(missing(server, &self.keys_for(name, server).await?))
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
        for (name, server) in servers {
            if super::is_address(&server.config.command) {
                if let (Some(config), _) = self.credentials() {
                    let _ = tokio::task::spawn_blocking(move || {
                        super::sign_in::hold_stored(&config, &name);
                    })
                    .await;
                }
                continue;
            }
            if server.config.keys.is_empty() {
                continue;
            }
            let mut found = self
                .keys_for(&name, &server.config)
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
        let names: Vec<_> = typed.keys().cloned().collect();
        let mut config = old.config.clone();
        for key in &names {
            if !config.keys.contains(key) {
                config.keys.push(key.clone());
            }
        }
        let mut found = self.keys_for(name, &config).await?;
        found.extend(
            old.keys
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
        );
        found.extend(typed.clone());
        *old.connection.lock().await = None;
        let staged = self.staged(name, &config, found);
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
