use std::sync::Arc;

use crate::agent::testing::{Caps, Fake, Knobs};
use crate::plugin::{KernelHost, PluginConfig, Registry};
use crate::provider::anthropic::capabilities;
use crate::tool::ToolEntry;

pub(crate) const SHELL: [&str; 2] = ["bash", "process"];

pub(crate) async fn shell_entries(
    only: Option<&[String]>,
    host: &Arc<KernelHost>,
) -> Vec<ToolEntry> {
    let names: Vec<String> = SHELL
        .iter()
        .map(|name| (*name).to_owned())
        .filter(|name| only.is_none_or(|only| only.contains(name)))
        .collect();
    if names.is_empty() {
        return Vec::new();
    }
    let source = crate::tool::testkit::shell_source(wisp_plugin_shell::Timing::default());
    let (registry, problems) = Registry::from_sources(vec![source], &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let on = names.iter().cloned().collect();
    let started = registry.start(&on, host.clone()).await;
    assert!(started.problems.is_empty(), "{:?}", started.problems);
    names
        .iter()
        .filter_map(|name| started.toolbox.get(name).cloned())
        .collect()
}

pub(crate) fn connection(
    name: &str,
    served: Option<&'static [&'static str]>,
) -> crate::agent::Connection {
    crate::agent::Connection {
        name: name.to_owned(),
        provider: Fake::with(
            Knobs {
                caps: Some(served.map_or(Caps::Fn(capabilities::row), Caps::Only)),
                hang_when_out: true,
                ..Knobs::default()
            },
            [],
        ),
        context: crate::agent::LogContext {
            endpoint_name: name.to_owned(),
            redactions: crate::session::Redactions::new(Vec::<String>::new()),
            ..crate::test_guard::log_context()
        },
        priced: true,
    }
}
