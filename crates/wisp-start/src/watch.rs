use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::FutureExt;
use futures::future::BoxFuture;
use wisp_core::endpoints::host::Reconnected;
use wisp_core::endpoints::watch::{AccountsWatch, Look, Stamp, prints};
use wisp_core::endpoints::{Availability, EndpointName, PathShown};

use crate::endpoints::{self, Flags, Sources};
use crate::keychain::Keychain;

const LOOK_WAIT: std::time::Duration = std::time::Duration::from_millis(200);

#[derive(Clone)]
pub struct SourcesWatch {
    pub env_file: Option<PathBuf>,
    pub config: Option<PathBuf>,
    pub endpoint: Option<String>,
    pub xdg_config_home: Option<PathBuf>,
    pub home: Option<PathBuf>,
    pub keychain: Arc<Keychain>,
    pub set: Vec<wisp_core::settings::Given>,
    pub models: Option<PathBuf>,
}

impl SourcesWatch {
    pub fn sources(&self) -> Sources {
        let flags = Flags {
            env_file: self.env_file.as_deref(),
            config: self.config.as_deref(),
            endpoint: self.endpoint.as_deref(),
            keychain: &self.keychain,
            set: &self.set,
        };
        Sources::load_with(
            &flags,
            self.xdg_config_home.clone(),
            self.home.clone(),
            true,
        )
    }

    fn watched(&self) -> [Option<PathBuf>; 3] {
        let at = |name, explicit| {
            endpoints::config_path(
                name,
                explicit,
                self.xdg_config_home.clone(),
                self.home.clone(),
            )
        };
        let config = at(wisp_protocol::CONFIG_FILE, self.config.as_deref());
        let store = config.as_deref().map(wisp_core::secrets::credentials_path);
        [
            config,
            store,
            at(wisp_protocol::KEYS_FILE, self.env_file.as_deref()),
        ]
    }

    fn look(&self, off: wisp_core::plugin::registry::ProvidersOff) -> Result<Look, String> {
        let [config, ..] = self.watched();
        let _locks = config
            .filter(|path| path.exists())
            .map(|path| {
                let folder = wisp_core::config_file::lock_folder(&path);
                let kind = wisp_core::secrets::LockKind::Config;
                wisp_core::secrets::lock(&[(&folder, kind)], PathShown::Default, LOOK_WAIT)
            })
            .transpose()
            .map_err(|error| error.to_string())?;
        let mut sources = self.sources();
        sources.off = off;
        let (entries, marks) = match sources.book() {
            Ok(book) => (book.entries, book.marks),
            Err(_) => {
                let entries = sources.entries()?;
                let marks = sources.marks(&entries);
                (entries, marks)
            }
        };
        sources.secrets.records().map_err(ToString::to_string)?;
        Ok(Look {
            prints: prints(&entries),
            view: view(&sources, &entries, &marks, self.models.as_deref()),
        })
    }
}

impl AccountsWatch for SourcesWatch {
    fn stamp(&self) -> Stamp {
        Stamp::of(self.watched().iter().map(Option::as_deref))
    }

    fn look(
        &self,
        off: wisp_core::plugin::registry::ProvidersOff,
    ) -> BoxFuture<'_, Result<Look, String>> {
        let watch = self.clone();
        async move {
            match tokio::task::spawn_blocking(move || watch.look(off)).await {
                Ok(result) => result,
                Err(e) => Err(format!("the save stopped: {e}")),
            }
        }
        .boxed()
    }
}

pub fn view(
    sources: &Sources,
    entries: &BTreeMap<EndpointName, Availability>,
    marks: &BTreeMap<EndpointName, u8>,
    models: Option<&Path>,
) -> Reconnected {
    let (ready, unavailable) = endpoints::connections(entries, models);
    let config = sources.config.as_ref().ok().and_then(Option::as_ref);
    let default = config.and_then(|c| c.default.as_ref()).map(|d| d.as_str());
    let endpoints = endpoints::summaries(entries, marks, &unavailable, default, &ready);
    Reconnected {
        ready,
        unavailable,
        endpoints,
    }
}
