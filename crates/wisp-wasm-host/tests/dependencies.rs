#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test when cargo metadata or a source file can't be read"
)]

#[path = "../../../tests/support/dependencies.rs"]
mod dependencies;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use dependencies::{cargo_metadata, closure, host_triple, normal_graph};

const KERNEL_SIDE: [&str; 4] = ["wisp-core", "wisp-protocol", "wisp-tui", "wisp-render"];

const BINDINGS: [&str; 3] = [
    "crates/wisp-wasm-host/src/bindings.rs",
    "plugins/board/src/bindings.rs",
    "plugins/wasm-fixture/src/bindings.rs",
];

#[test]
fn the_host_never_reaches_the_kernel_or_a_client() {
    let metadata = cargo_metadata(&["--filter-platform", &host_triple()]);
    let names: BTreeMap<&str, &str> = metadata["packages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["id"].as_str().unwrap(), p["name"].as_str().unwrap()))
        .collect();
    let graph = normal_graph(&metadata);
    let start = names
        .iter()
        .filter(|(_, name)| **name == env!("CARGO_PKG_NAME"))
        .map(|(id, _)| (*id).to_owned())
        .collect();
    let reached: Vec<&str> = closure(start, |id| graph.get(id))
        .iter()
        .map(|id| names[id.as_str()])
        .collect();
    assert!(reached.contains(&"wasmtime"), "{reached:?}");
    for name in KERNEL_SIDE {
        assert!(!reached.contains(&name), "{name}");
    }
}

fn sources(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() && !path.ends_with("target") {
            sources(&path, found);
        } else if path.extension().is_some_and(|e| e == "rs") {
            found.push(path);
        }
    }
}

#[test]
fn only_the_bindings_modules_allow_unsafe_code() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut found = Vec::new();
    for top in ["crates", "plugins", "tests"] {
        sources(&root.join(top), &mut found);
    }
    let needle = ["allow(", "unsafe_code"].concat();
    let mut allowed: Vec<String> = found
        .iter()
        .filter(|path| {
            let source = std::fs::read_to_string(path).unwrap();
            source
                .split_whitespace()
                .collect::<String>()
                .contains(&needle)
        })
        .map(|path| {
            path.strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    allowed.sort();
    assert_eq!(allowed, BINDINGS);
}
