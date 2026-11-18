#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "each guard uses part of these helpers, which fail the test when cargo metadata or rustc -vV can't be read"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

use serde_json::Value;

pub(crate) fn cargo_metadata(extra: &[&str]) -> Value {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
    let output = Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--offline"])
        .args(extra)
        .args(["--manifest-path", manifest])
        .output()
        .expect("run cargo metadata");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

pub(crate) fn host_triple() -> String {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(rustc)
        .arg("-vV")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run rustc -vV");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    text.lines()
        .find_map(|line| line.strip_prefix("host: "))
        .expect("rustc -vV names its host")
        .trim()
        .to_owned()
}

pub(crate) fn normal_graph(metadata: &Value) -> BTreeMap<String, Vec<String>> {
    let mut graph = BTreeMap::new();
    for node in metadata["resolve"]["nodes"].as_array().unwrap() {
        let deps = node["deps"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|d| {
                d["dep_kinds"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|k| k["kind"].is_null())
            })
            .map(|d| d["pkg"].as_str().unwrap().to_owned())
            .collect();
        graph.insert(node["id"].as_str().unwrap().to_owned(), deps);
    }
    graph
}

pub(crate) fn closure<'a>(
    start: Vec<String>,
    deps: impl Fn(&str) -> Option<&'a Vec<String>>,
) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut stack = start;
    while let Some(id) = stack.pop() {
        for dep in deps(&id).into_iter().flatten() {
            if seen.insert(dep.clone()) {
                stack.push(dep.clone());
            }
        }
    }
    seen
}

pub(crate) fn normal_dependencies(name: &str) -> Vec<Value> {
    let metadata = cargo_metadata(&["--no-deps"]);
    let package = metadata["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == name)
        .unwrap_or_else(|| panic!("{name} is not in the workspace"));
    package["dependencies"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["kind"].is_null())
        .cloned()
        .collect()
}

pub(crate) fn depends_only_on(allowed: &[&str], workspace: &[&str]) {
    let package = env!("CARGO_PKG_NAME");
    let dependencies = normal_dependencies(package);
    let mut names: Vec<&str> = dependencies
        .iter()
        .map(|d| d["name"].as_str().unwrap())
        .collect();
    names.sort_unstable();
    assert_eq!(names, allowed, "{package}");
    for dependency in &dependencies {
        let name = dependency["name"].as_str().unwrap();
        assert_eq!(
            dependency["path"].is_null(),
            !workspace.contains(&name),
            "{package} -> {name}: a path dependency exactly when it is a workspace crate"
        );
    }
}

pub(crate) fn built_without_defaults(name: &str, features: &[&str]) -> Value {
    let dependency = normal_dependencies(env!("CARGO_PKG_NAME"))
        .into_iter()
        .find(|d| d["name"] == name)
        .unwrap_or_else(|| panic!("{name} is not a dependency"));
    assert_eq!(dependency["uses_default_features"], false, "{name}");
    let mut found: Vec<&str> = dependency["features"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap())
        .collect();
    found.sort_unstable();
    assert_eq!(found, features, "{name}");
    dependency
}
