use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha2::{Digest, Sha256};
use wasmtime::Engine;
use wasmtime::component::{Component, Linker};
use wisp_plugin_api::Plugin;
use wisp_plugin_api::host::Host;
use wisp_plugin_api::manifest::{Manifest, Origin};

use crate::bindings::Served;
use crate::dirs::Dirs;
use crate::engine::engine;
use crate::exports::Pre;
use crate::instance::reason_of;
use crate::plugin::WasmPlugin;
use crate::state::{Imports, State};

pub const BOUND: [&str; 2] = ["0.2.0", "0.3.0"];

pub type Make = Arc<dyn Fn(Arc<dyn Host>) -> Arc<dyn Plugin> + Send + Sync>;

pub struct Found {
    pub id: String,
    pub manifest: String,
    pub make: Make,
    pub sha256: String,
}

fn linker(engine: &Engine) -> Result<Linker<State>, String> {
    let mut linker = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker).map_err(|e| reason_of(&e))?;
    Served::add_to_linker::<State, Imports>(&mut linker, |state| state)
        .map_err(|e| reason_of(&e))?;
    Ok(linker)
}

pub fn stems(folder: &Path) -> Result<Vec<String>, String> {
    let names: BTreeSet<String> = std::fs::read_dir(folder)
        .map_err(|e| format!("its folder can't be read: {e}"))?
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    Ok(names
        .iter()
        .filter_map(|name| name.strip_suffix(".toml"))
        .filter(|stem| names.contains(&format!("{stem}.wasm")))
        .map(str::to_owned)
        .collect())
}

pub(crate) fn pair(folder: &Path) -> Result<String, String> {
    match stems(folder)?.as_slice() {
        [stem] => Ok(stem.clone()),
        [] => Err("its folder doesn't hold a manifest and component pair".to_owned()),
        _ => Err("its folder holds more than one version; run wisp plugin add again".to_owned()),
    }
}

pub(crate) struct Loader<'a> {
    dirs: &'a Dirs,
    env: Arc<[(String, String)]>,
    linker: Option<Linker<State>>,
}

impl<'a> Loader<'a> {
    pub(crate) fn new(dirs: &'a Dirs) -> Self {
        Self {
            dirs,
            env: dirs.env.clone().into(),
            linker: None,
        }
    }

    fn linker(&mut self) -> Result<&Linker<State>, String> {
        if self.linker.is_none() {
            let engine = engine(self.dirs.compiled().as_deref())?;
            self.linker = Some(linker(engine)?);
        }
        self.linker
            .as_ref()
            .ok_or_else(|| "the WASM engine didn't start".to_owned())
    }

    fn load(&mut self, folder: &Path) -> Result<Found, (Option<String>, String)> {
        let stem = pair(folder).map_err(|message| (None, message))?;
        let text = std::fs::read_to_string(folder.join(format!("{stem}.toml")))
            .map_err(|e| (None, format!("its manifest can't be read: {e}")))?;
        let wasm = folder.join(format!("{stem}.wasm"));
        self.check(text, &wasm, Some(&stem)).map(|(found, _)| found)
    }

    pub(crate) fn check(
        &mut self,
        text: String,
        wasm: &Path,
        stem: Option<&str>,
    ) -> Result<(Found, Vec<u8>), (Option<String>, String)> {
        let manifest =
            Manifest::parse(&text, Origin::External).map_err(|p| (None, p.to_string()))?;
        let id = manifest.id.clone();
        let named = |message: String| (Some(id.clone()), message);
        if !BOUND.contains(&manifest.world.as_str()) {
            return Err(named(format!(
                "WASM plugins name world {}",
                BOUND.join(" or ")
            )));
        }
        let bytes =
            std::fs::read(wasm).map_err(|e| named(format!("its component can't be read: {e}")))?;
        let sha256 = wisp_plugin_api::hex::encode(&Sha256::digest(&bytes));
        if stem.is_some_and(|stem| stem != sha256) {
            return Err(named(
                "its component's SHA-256 doesn't match its file name".to_owned(),
            ));
        }
        let linker = self.linker().map_err(named)?;
        let component = Component::from_binary(linker.engine(), &bytes)
            .map_err(|e| named(format!("its component doesn't compile: {}", reason_of(&e))))?;
        let pre = linker
            .instantiate_pre(&component)
            .and_then(|instance| Pre::new(instance, &manifest.world))
            .map_err(|e| {
                named(format!(
                    "its component doesn't fit world {}: {}",
                    manifest.world,
                    reason_of(&e)
                ))
            })?;
        let env = self.env.clone();
        let plugin = id.clone();
        let make: Make = Arc::new(move |host| {
            Arc::new(WasmPlugin::new(
                plugin.clone(),
                pre.clone(),
                host,
                env.clone(),
            )) as Arc<dyn Plugin>
        });
        let found = Found {
            id,
            manifest: text,
            make,
            sha256,
        };
        Ok((found, bytes))
    }
}

pub fn folders(dirs: &Dirs) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dirs.plugins()) else {
        return Vec::new();
    };
    let folders: BTreeSet<(String, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_type()
                .is_ok_and(|t| t.is_dir() || t.is_symlink())
        })
        .map(|entry| {
            (
                entry.file_name().to_string_lossy().into_owned(),
                entry.path(),
            )
        })
        .collect();
    folders.into_iter().collect()
}

pub fn read(dirs: &Dirs, folder: &Path) -> Result<Found, (String, String)> {
    Loader::new(dirs).load(folder).map_err(|(id, message)| {
        let name = folder
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        (id.or(name).unwrap_or_default(), message)
    })
}
