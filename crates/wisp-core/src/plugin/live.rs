use std::sync::{Arc, Mutex, PoisonError};

use wisp_protocol::{Catalog, PluginEntry};

use super::registry::{Problem, Registry, Source};

pub trait Installed: Send + Sync {
    fn changed(&self) -> Option<(Vec<Source>, Vec<Problem>)>;
}

pub struct Live {
    registry: Mutex<Arc<Registry>>,
    installed: Option<Arc<dyn Installed>>,
}

impl Live {
    pub fn new(registry: Arc<Registry>, installed: Option<Arc<dyn Installed>>) -> Self {
        Self {
            registry: Mutex::new(registry),
            installed,
        }
    }

    pub fn registry(&self) -> Arc<Registry> {
        self.registry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn applied(&self, registry: Arc<Registry>) {
        *self.registry.lock().unwrap_or_else(PoisonError::into_inner) = registry;
    }

    pub(crate) async fn refresh(&self) -> Option<(Vec<Source>, Vec<Problem>)> {
        let installed = self.installed.clone()?;
        tokio::task::spawn_blocking(move || installed.changed())
            .await
            .ok()
            .flatten()
    }
}

pub fn merged_tools(
    saved: &[String],
    arrived: &[(Vec<String>, PluginEntry)],
    catalog: &Catalog,
) -> Result<Option<Vec<String>>, String> {
    if saved.is_empty() {
        return Ok(None);
    }
    let list = arrived.iter().fold(saved.to_vec(), |list, (older, entry)| {
        let new_on: Vec<String> = entry
            .tools
            .iter()
            .filter(|t| t.default_on && t.follows.is_none() && t.unavailable.is_none())
            .map(|t| t.name.clone())
            .filter(|name| !older.contains(name) && !list.contains(name))
            .collect();
        list.into_iter()
            .filter(|name| !older.contains(name) || entry.tools.iter().any(|t| &t.name == name))
            .chain(new_on)
            .collect()
    });
    if list == saved {
        return Ok(None);
    }
    catalog.checked_names(&list)?;
    Ok(Some(list))
}
