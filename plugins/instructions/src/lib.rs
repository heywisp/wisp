use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use wisp_instructions::{
    DEFAULT_WARN_TOKENS, Diagnostic, Files, Kind, LoadAll, Loaded, Meta, Options, Paths, Source,
};
use wisp_plugin_api::host::{FileKind, Host};
use wisp_plugin_api::prompt::{At, Prior, Rendered};
use wisp_plugin_api::tools::{BuildContext, DoctorRow};
use wisp_plugin_api::{BoxFuture, Bundled, Plugin};

mod doctor;
mod update;

use update::{CachedFiles, State};

pub const MANIFEST: &str = include_str!("../wisp-plugin.toml");

pub const SECTION: &str = "instructions";

pub const SOURCE: &str = "instructions.source";

pub const WARN_TOKENS: &str = "instructions.warn_tokens";

pub fn bundled() -> Bundled {
    Bundled {
        manifest: MANIFEST,
        instantiate,
    }
}

fn instantiate(host: Arc<dyn Host>) -> Arc<dyn Plugin> {
    Arc::new(InstructionsPlugin {
        host,
        state: Mutex::default(),
    })
}

struct HostFiles<'a> {
    host: &'a dyn Host,
}

impl Files for HostFiles<'_> {
    fn metadata(&self, path: &Path) -> io::Result<Meta> {
        let meta = std::fs::metadata(path)?;
        let kind = if meta.is_file() {
            Kind::File
        } else if meta.is_dir() {
            Kind::Directory
        } else {
            Kind::Other
        };
        Ok(Meta {
            kind,
            len: meta.len(),
            mtime: meta.modified().ok().and_then(wisp_instructions::mtime),
        })
    }

    fn exists(&self, path: &Path) -> bool {
        std::fs::symlink_metadata(path).is_ok()
    }

    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        std::fs::canonicalize(path)
    }

    fn read(&self, path: &Path) -> io::Result<Option<Vec<u8>>> {
        if self.host.kind_of(&path.display().to_string()) != Some(FileKind::Regular) {
            return Ok(None);
        }
        std::fs::read(path).map(Some)
    }

    fn read_capped(&self, path: &Path, limit: u64) -> io::Result<Option<Vec<u8>>> {
        if self.host.kind_of(&path.display().to_string()) != Some(FileKind::Regular) {
            return Ok(None);
        }
        let mut raw = Vec::new();
        std::fs::File::open(path)?
            .take(limit)
            .read_to_end(&mut raw)?;
        Ok(Some(raw))
    }
}

pub fn load(host: &dyn Host) -> Option<Loaded> {
    load_from(host, &HostFiles { host })
}

fn load_from(host: &dyn Host, files: &dyn Files) -> Option<Loaded> {
    let root = host.root();
    if root.is_empty() {
        return None;
    }
    let source = host
        .get(SOURCE)
        .and_then(|s| Source::parse(&s))
        .unwrap_or_default();
    let warn_tokens = host
        .get(WARN_TOKENS)
        .and_then(|t| t.parse().ok())
        .unwrap_or(DEFAULT_WARN_TOKENS);
    let paths = Paths {
        start: PathBuf::from(root),
        config_dir: host.config_dir().map(PathBuf::from),
        home: host.home().map(PathBuf::from),
    };
    let options = Options {
        source,
        warn_tokens,
    };
    Some(wisp_instructions::load(&paths, options, &LoadAll, files))
}

fn report(host: &dyn Host, loaded: &Loaded) {
    host.loaded(loaded.report());
    if let Some(tokens) = loaded.large {
        host.warning(Diagnostic::Large(tokens));
    }
}

pub use wisp_instructions::delta::snapshot;

pub struct InstructionsPlugin {
    host: Arc<dyn Host>,
    state: Mutex<State>,
}

impl InstructionsPlugin {
    fn load_cached(&self, state: &mut State) -> Option<Loaded> {
        let host = self.host.as_ref();
        let files = CachedFiles::new(HostFiles { host }, state);
        let loaded = load_from(host, &files);
        files.finish(state);
        if let Some(loaded) = &loaded {
            state.keep(loaded);
        }
        loaded
    }

    fn render_start(&self) -> Rendered {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(loaded) = self.load_cached(&mut state) else {
            return Rendered::default();
        };
        report(self.host.as_ref(), &loaded);
        Rendered {
            snapshot: snapshot(&loaded.files),
            text: loaded.block.clone(),
        }
    }

    fn render_update(&self, previous: &Prior, target: Option<&str>, at: At) -> Rendered {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(loaded) = self.load_cached(&mut state) else {
            return Rendered::default();
        };
        let rendered = update::render(self.host.as_ref(), &mut state, &loaded, previous, target);
        if at == At::Turn {
            state.prune(previous, &rendered.0);
        }
        if let (Some(text), Some(change)) = (&rendered.0.text, rendered.1) {
            self.host.changed(change, text.clone());
        }
        rendered.0
    }
}

impl Plugin for InstructionsPlugin {
    fn doctor<'a>(&'a self, _ctx: &'a BuildContext) -> BoxFuture<'a, Vec<DoctorRow>> {
        let rows = match load(self.host.as_ref()) {
            Some(loaded) => {
                report(self.host.as_ref(), &loaded);
                doctor::rows(&loaded)
            }
            None => Vec::new(),
        };
        Box::pin(async move { rows })
    }

    fn render_section<'a>(
        &'a self,
        name: &'a str,
        at: At,
        previous: Prior,
        target: Option<&'a str>,
    ) -> BoxFuture<'a, Rendered> {
        let rendered = match (name, at) {
            (SECTION, At::Start) => self.render_start(),
            (SECTION, At::Turn) => self.render_update(&previous, None, At::Turn),
            (SECTION, At::Head) => self.render_update(&previous, target, At::Head),
            _ => Rendered::default(),
        };
        Box::pin(async move { rendered })
    }
}

#[cfg(test)]
mod tests;
