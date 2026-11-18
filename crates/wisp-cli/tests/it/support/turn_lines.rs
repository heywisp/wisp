use std::collections::BTreeSet;
use std::sync::Arc;

use wisp_core::agent::Agent;
use wisp_core::plugin::{KernelHost, PluginConfig, Registry, Started};

pub(crate) fn started() -> Started {
    let (registry, problems) = Registry::new(wisp_plugins::bundled(), &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(registry.start(&BTreeSet::new(), Arc::new(KernelHost::new())))
    })
    .join()
    .unwrap()
}

pub(crate) fn with_turn_lines(agent: Agent) -> Agent {
    let started = started();
    agent.with_hooks(started.hooks, started.host)
}
