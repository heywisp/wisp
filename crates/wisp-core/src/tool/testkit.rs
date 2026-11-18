use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use futures::FutureExt;
use serde_json::{Value, json};
use wisp_plugin_api::Plugin;
use wisp_plugin_api::host::Captured;
use wisp_plugin_testkit::{MANIFEST, Testkit};

use super::{ToolEntry, Toolbox};
use crate::plugin::{KernelHost, Make, PluginConfig, Registry};

pub(crate) const BASH_VERSION: &str = "GNU bash, version 5.2.21(1)-release (x86_64-pc-linux-gnu)\n";

pub(crate) fn fake_capture_host() -> KernelHost {
    KernelHost::new().with_capture(Captured::Output(BASH_VERSION.as_bytes().to_vec()))
}

pub(crate) fn start(sources: Vec<(String, Make)>, on: &[&str]) -> Toolbox {
    start_on(sources, on, Arc::new(fake_capture_host()))
}

pub(crate) fn start_on(
    sources: Vec<(String, Make)>,
    on: &[&str],
    host: Arc<KernelHost>,
) -> Toolbox {
    let (registry, problems) = Registry::from_sources(sources, &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let on: BTreeSet<String> = on.iter().map(|s| (*s).to_owned()).collect();
    let started = registry.start(&on, host).now_or_never().unwrap();
    assert!(started.problems.is_empty(), "{:?}", started.problems);
    started.toolbox
}

pub(crate) fn shell_make(timing: wisp_plugin_shell::Timing) -> Make {
    Arc::new(move |host| {
        Arc::new(wisp_plugin_shell::Shell::with_timing(host, timing)) as Arc<dyn Plugin>
    })
}

pub(crate) fn shell_source(timing: wisp_plugin_shell::Timing) -> (String, Make) {
    (wisp_plugin_shell::MANIFEST.to_owned(), shell_make(timing))
}

pub(crate) fn secret_source() -> (String, Make) {
    let secret = wisp_plugin_secret::bundled();
    (secret.manifest.to_owned(), Arc::new(secret.instantiate))
}

pub(crate) fn shell_on(host: Arc<KernelHost>, timing: wisp_plugin_shell::Timing) -> Toolbox {
    let sources = vec![shell_source(timing), secret_source()];
    start_on(sources, &["bash", wisp_plugin_secret::NAME], host)
}

pub(crate) fn shell(timing: wisp_plugin_shell::Timing) -> Toolbox {
    shell_on(Arc::new(fake_capture_host()), timing)
}

pub(crate) fn bash_entry() -> ToolEntry {
    shell(wisp_plugin_shell::Timing::default())
        .get("bash")
        .cloned()
        .unwrap()
}

pub(crate) fn timing(quiet_ms: u64, max_ms: u64) -> wisp_plugin_shell::Timing {
    wisp_plugin_shell::Timing {
        start_quiet_ms: quiet_ms,
        start_max_ms: max_ms,
        ..wisp_plugin_shell::Timing::default()
    }
}

pub(crate) fn make_with(limits: BTreeMap<String, u64>) -> Make {
    Arc::new(move |host| {
        let mut kit = Testkit::new(host);
        for (tool, ms) in &limits {
            kit = kit.with_limit(tool, *ms);
        }
        Arc::new(kit) as Arc<dyn Plugin>
    })
}

pub(crate) fn toolbox(on: &[&str]) -> Toolbox {
    start(vec![(MANIFEST.to_owned(), make_with(BTreeMap::new()))], on)
}

pub(crate) fn entry(name: &str) -> ToolEntry {
    toolbox(&[name]).get(name).cloned().unwrap()
}

pub(crate) fn limited_entry(name: &str, ms: u64) -> ToolEntry {
    let limits = BTreeMap::from([(name.to_owned(), ms)]);
    start(vec![(MANIFEST.to_owned(), make_with(limits))], &[name])
        .get(name)
        .cloned()
        .unwrap()
}

pub(crate) struct Sample {
    pub(crate) tool: ToolEntry,
    pub(crate) input: Value,
}

pub(crate) fn samples() -> Vec<Sample> {
    vec![
        Sample {
            tool: entry("echo"),
            input: json!({"text": "hello from echo\n"}),
        },
        Sample {
            tool: entry("sleep"),
            input: json!({"ms": 0, "text": "slept\n"}),
        },
    ]
}

pub(crate) fn bundled(on: &[&str]) -> Toolbox {
    bundled_with(on, false)
}

pub(crate) fn bundled_exact(names: &[&str]) -> Toolbox {
    bundled_with(names, true)
}

fn bundled_with(on: &[&str], exact: bool) -> Toolbox {
    let (registry, problems) = Registry::new(wisp_plugins::bundled(), &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let on: BTreeSet<String> = on.iter().map(|s| (*s).to_owned()).collect();
    let host = Arc::new(fake_capture_host());
    let started = if exact {
        registry.start_exact(&on, host).now_or_never().unwrap()
    } else {
        registry.start(&on, host).now_or_never().unwrap()
    };
    assert!(started.problems.is_empty(), "{:?}", started.problems);
    started.toolbox
}

pub(crate) const ALL: &[&str] = &[
    "ask_secret",
    "ask_user",
    "bash",
    "list_accounts",
    "list_settings",
    "show_diff",
    "view_file",
];

pub(crate) fn bundled_ids() -> Vec<&'static str> {
    let mut ids: Vec<&str> = wisp_plugin_api::manifest::BUNDLED_IDS
        .into_iter()
        .filter(|id| cfg!(unix) || *id != "shell")
        .collect();
    ids.sort_unstable();
    ids
}

pub(crate) fn catalog() -> wisp_protocol::Catalog {
    let (registry, problems) = Registry::new(
        crate::plugin::testing::bundled_but_idle(),
        &PluginConfig::default(),
    );
    assert!(problems.is_empty(), "{problems:?}");
    registry.catalog(&registry.default_on(), &[])
}

pub(crate) fn bundled_entry(on: &[&str], name: &str) -> ToolEntry {
    bundled(on).get(name).cloned().unwrap()
}

pub(crate) fn ask_user() -> ToolEntry {
    bundled_entry(&["ask_user"], "ask_user")
}

pub(crate) fn show_diff() -> ToolEntry {
    bundled_entry(&["show_diff"], "show_diff")
}

#[cfg(unix)]
pub(crate) fn view_file() -> ToolEntry {
    bundled_entry(&["bash", "view_file"], "view_file")
}

#[cfg(unix)]
pub(crate) fn view_file_without_bash() -> ToolEntry {
    bundled_entry(&["view_file"], "view_file")
}
