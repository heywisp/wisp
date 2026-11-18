#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "each test binary uses part of it, and a failed setup fails the test"
)]

#[path = "../../../wisp-tui/tests/support/scratch.rs"]
mod scratch;

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use sha2::{Digest, Sha256};
use wisp_plugin_api::Plugin;
use wisp_plugin_api::hooks::{Custom, Event, SessionStart};
use wisp_plugin_api::host::Host;
use wisp_plugin_api::tools::BuildContext;
use wisp_plugin_api::tree::Identity;
use wisp_plugin_testkit::host::FakeHost;
use wisp_wasm_guests::{BOARD, BOARD_MANIFEST, WASM_FIXTURE, WASM_FIXTURE_MANIFEST};
use wisp_wasm_host::{Dirs, Found, folders, read};

pub(crate) use scratch::ScratchDir;

#[derive(Default)]
pub(crate) struct Installed {
    pub(crate) plugins: Vec<Found>,
    pub(crate) problems: Vec<(String, String)>,
}

pub(crate) const SCRATCH: &str = "wisp-wasm-";

pub(crate) fn sha256(bytes: &[u8]) -> String {
    wisp_plugin_api::hex::encode(&Sha256::digest(bytes))
}

pub(crate) fn place(data: &Path, folder: &str, manifest: &str, wasm: &[u8], stem: &str) {
    let dir = data.join("plugins").join(folder);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{stem}.toml")), manifest).unwrap();
    std::fs::write(dir.join(format!("{stem}.wasm")), wasm).unwrap();
}

pub(crate) fn fixture_bytes() -> Vec<u8> {
    std::fs::read(WASM_FIXTURE).unwrap()
}

pub(crate) fn fixture_manifest() -> String {
    std::fs::read_to_string(WASM_FIXTURE_MANIFEST).unwrap()
}

pub(crate) fn install_fixture(data: &Path) {
    let wasm = fixture_bytes();
    place(
        data,
        "wasm-fixture",
        &fixture_manifest(),
        &wasm,
        &sha256(&wasm),
    );
}

pub(crate) struct Rig {
    pub(crate) scratch: ScratchDir,
    pub(crate) dirs: Dirs,
}

impl Rig {
    pub(crate) fn new(name: &str) -> Self {
        let scratch = ScratchDir::new(SCRATCH, name);
        let dirs = Dirs {
            data: scratch.root.join("data"),
            cache: Some(Path::new(env!("CARGO_TARGET_TMPDIR")).join("wasm-host")),
            env: Vec::new(),
        };
        Self { scratch, dirs }
    }

    pub(crate) fn installed(&self) -> Installed {
        let mut installed = Installed::default();
        for (_, folder) in folders(&self.dirs) {
            match read(&self.dirs, &folder) {
                Ok(found) => installed.plugins.push(found),
                Err(problem) => installed.problems.push(problem),
            }
        }
        installed
    }

    pub(crate) fn fixture(&self) -> Found {
        install_fixture(&self.dirs.data);
        let mut found = self.installed();
        assert!(found.problems.is_empty(), "{:?}", found.problems);
        found.plugins.remove(0)
    }

    pub(crate) fn plugin(&self, host: impl Host + 'static) -> Arc<dyn Plugin> {
        (self.fixture().make)(Arc::new(host))
    }

    pub(crate) fn board(&self) -> Found {
        let wasm = std::fs::read(BOARD).unwrap();
        let manifest = std::fs::read_to_string(BOARD_MANIFEST).unwrap();
        place(&self.dirs.data, "board", &manifest, &wasm, &sha256(&wasm));
        let mut found = self.installed();
        assert!(found.problems.is_empty(), "{:?}", found.problems);
        found.plugins.retain(|found| found.id == "board");
        found.plugins.remove(0)
    }
}

pub(crate) const ROOT_ID: &str = "01a1aaaa-0000-7000-8000-000000000001";

pub(crate) fn board_host(rig: &Rig) -> FakeHost {
    FakeHost {
        identity: Some(Identity {
            path: "/".to_owned(),
            name: "main".to_owned(),
            parent: None,
            preset: None,
            depth: 0,
            session_id: ROOT_ID.to_owned(),
            root_session_id: ROOT_ID.to_owned(),
        }),
        data_dir: Some(
            rig.scratch
                .root
                .join("plugin-data/board")
                .display()
                .to_string(),
        ),
        ..FakeHost::default()
    }
}

pub(crate) fn build() -> BuildContext {
    BuildContext {
        platform: "unix".to_owned(),
        capabilities: Vec::new(),
    }
}

tokio::task_local! {
    pub(crate) static LABEL: String;
}

pub(crate) fn label() -> Option<String> {
    LABEL.try_with(Clone::clone).ok()
}

pub(crate) struct Labelled(Option<String>);

impl wisp_plugin_api::Scope for Labelled {
    fn run(&self, f: &mut dyn FnMut()) {
        match &self.0 {
            Some(label) => LABEL.sync_scope(label.clone(), f),
            None => f(),
        }
    }

    fn wrap<'a>(
        &self,
        f: futures::future::BoxFuture<'a, ()>,
    ) -> futures::future::BoxFuture<'a, ()> {
        match &self.0 {
            Some(label) => Box::pin(LABEL.scope(label.clone(), f)),
            None => f,
        }
    }
}

pub(crate) const SETUP: &str = "setup";

#[derive(Default)]
pub(crate) struct FakeKernel {
    pub(crate) plugin: OnceLock<Weak<dyn Plugin>>,
    pub(crate) restarts: AtomicUsize,
    pub(crate) relayed: AtomicUsize,
    pub(crate) gate: tokio::sync::RwLock<()>,
}

impl wisp_plugin_api::Kernel for FakeKernel {
    fn caller(&self) -> Arc<dyn wisp_plugin_api::Scope> {
        Arc::new(Labelled(label()))
    }

    fn restarted(&self) -> futures::future::BoxFuture<'_, ()> {
        Box::pin(async move {
            self.restarts.fetch_add(1, Ordering::SeqCst);
            if let Some(plugin) = self.plugin.get().and_then(Weak::upgrade) {
                let start = Event::SessionStart(SessionStart { resumed: true });
                plugin.on_hook(SETUP, start).await;
                let fire = wisp_plugin_testkit::hooks::user_input("fire", 0);
                plugin.on_hook("fires", fire).await;
            }
            drop(self.gate.read().await);
        })
    }
}

pub(crate) fn kernelled(rig: &Rig) -> (Arc<dyn Plugin>, Arc<FakeKernel>) {
    let kernel = Arc::new(FakeKernel::default());
    let relaying = kernel.clone();
    let host = wisp_plugin_testkit::host::FakeHost {
        kernel: Some(kernel.clone()),
        fire: Some(Arc::new(move |point, payload| {
            let kernel = relaying.clone();
            Box::pin(async move {
                let plugin = kernel.plugin.get().and_then(Weak::upgrade);
                if let Some(plugin) =
                    plugin.filter(|_| kernel.relayed.fetch_add(1, Ordering::SeqCst) == 0)
                {
                    let version = "1".to_owned();
                    let ping = Event::Custom(Custom {
                        point,
                        version,
                        payload,
                    });
                    plugin.on_hook("relay", ping).await;
                }
                Vec::new()
            })
        })),
        ..Default::default()
    };
    let plugin = rig.plugin(host);
    kernel.plugin.set(Arc::downgrade(&plugin)).ok();
    (plugin, kernel)
}

pub(crate) type Slept = Arc<std::sync::Mutex<Vec<(Option<String>, u64)>>>;

pub(crate) fn sleeping_host() -> (wisp_plugin_testkit::host::FakeHost, Slept) {
    let slept = Slept::default();
    let seen = slept.clone();
    let host = wisp_plugin_testkit::host::FakeHost {
        sleep: Some(Arc::new(move |ms| {
            seen.lock().unwrap().push((label(), ms));
            Box::pin(tokio::time::sleep(std::time::Duration::from_millis(ms)))
        })),
        kernel: Some(Arc::new(FakeKernel::default())),
        ..Default::default()
    };
    (host, slept)
}

pub(crate) async fn result(plugin: &dyn Plugin, tool: &str, input: &str) -> String {
    let prepared = plugin.prepare(tool, input).await.unwrap();
    let call = wisp_plugin_testkit::host::FakeCall::default();
    plugin.run(prepared, &call).await.result.unwrap_or_default()
}
