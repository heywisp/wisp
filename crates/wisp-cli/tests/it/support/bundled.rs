use std::collections::BTreeSet;
use std::sync::Arc;

use wisp_core::plugin::{KernelHost, PluginConfig, Registry};
use wisp_core::tool::Toolbox;

pub(crate) fn toolbox(names: &[&str]) -> Toolbox {
    let (registry, problems) = Registry::new(wisp_plugins::bundled(), &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let on: BTreeSet<String> = names.iter().map(|n| (*n).to_owned()).collect();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(registry.start(&on, Arc::new(KernelHost::new())))
            .toolbox
    })
    .join()
    .unwrap()
}
