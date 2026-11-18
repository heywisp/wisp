#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test when cargo metadata can't be read"
)]

#[path = "../../../tests/support/dependencies.rs"]
mod dependencies;

use std::collections::{BTreeMap, BTreeSet};

use dependencies::{cargo_metadata, host_triple, normal_graph};

const CLIENTS: [&str; 4] = ["wisp-tui", "wisp-render", "learn-web", "wisp-client"];

const CONTAIN: &str = "wisp-contain";

const START: &str = "wisp-start";

type Graph = BTreeMap<String, (String, Vec<String>)>;

fn graph() -> Graph {
    let metadata = cargo_metadata(&["--filter-platform", &host_triple()]);
    let names: BTreeMap<&str, &str> = metadata["packages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["id"].as_str().unwrap(), p["name"].as_str().unwrap()))
        .collect();
    normal_graph(&metadata)
        .into_iter()
        .map(|(id, deps)| {
            let name = names[id.as_str()].to_owned();
            (id, (name, deps))
        })
        .collect()
}

fn closure(graph: &Graph, name: &str) -> BTreeSet<String> {
    let start = graph
        .iter()
        .filter(|(_, (n, _))| n == name)
        .map(|(id, _)| id.clone())
        .collect();
    dependencies::closure(start, |id| graph.get(id).map(|(_, d)| d))
        .iter()
        .map(|id| graph[id].0.clone())
        .collect()
}

fn forbidden(name: &str) -> bool {
    matches!(
        name,
        "wisp-plugin-api" | "wisp-tool-text" | "wisp-plugins" | "wasmtime"
    ) || name.starts_with("wisp-plugin-")
}

fn violations(graph: &Graph) -> Vec<String> {
    let mut found = Vec::new();
    for client in CLIENTS {
        for name in closure(graph, client) {
            if forbidden(&name) {
                found.push(format!("{client} depends on {name}"));
            }
        }
    }
    for name in closure(graph, START) {
        if matches!(name.as_str(), "wisp-tui" | "wisp-cli") {
            found.push(format!("{START} depends on {name}"));
        }
    }
    let contain = closure(graph, CONTAIN);
    if !contain.is_empty() {
        found.push(format!("{CONTAIN} depends on {contain:?}"));
    }
    found
}

#[test]
fn the_clients_never_reach_the_plugin_api_or_a_plugin() {
    let graph = graph();
    assert!(closure(&graph, "wisp-tui").contains("wisp-render"));
    assert!(closure(&graph, START).contains("wisp-core"));
    let found = violations(&graph);
    assert!(found.is_empty(), "{}", found.join("\n"));
    assert_eq!(
        wisp_render::transcript::CONTEXT_THRESHOLD,
        wisp_plugin_api::manifest::CONTEXT_THRESHOLD
    );
}

#[test]
fn the_check_catches_each_forbidden_crate_and_a_dependency_of_wisp_contain() {
    let node = |name: &str, deps: &[&str]| {
        (
            name.to_owned(),
            (
                name.to_owned(),
                deps.iter().map(|d| (*d).to_owned()).collect(),
            ),
        )
    };
    let graph: Graph = [
        node("wisp-tui", &["wisp-render", "wisp-contain"]),
        node("wisp-render", &["wisp-tool-text"]),
        node("wisp-tool-text", &["wisp-plugin-api"]),
        node("wisp-plugin-api", &[]),
        node("wisp-contain", &["libc"]),
        node("libc", &[]),
        node("wisp-start", &["wisp-tui"]),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        violations(&graph),
        [
            "wisp-tui depends on wisp-plugin-api",
            "wisp-tui depends on wisp-tool-text",
            "wisp-render depends on wisp-plugin-api",
            "wisp-render depends on wisp-tool-text",
            "wisp-start depends on wisp-tui",
            "wisp-contain depends on {\"libc\"}",
        ]
    );
    for name in ["wisp-plugins", "wisp-plugin-shell", "wasmtime"] {
        assert!(forbidden(name), "{name}");
    }
    assert!(!forbidden(CONTAIN));
}
