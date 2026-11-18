#[path = "../../../tests/support/dependencies.rs"]
mod dependencies;

use std::collections::BTreeMap;

use dependencies::cargo_metadata;

const KERNEL: &str = "wisp-core";

const MAY_USE_KERNEL: &[&str] = &[KERNEL, "wisp-cli", "wisp-start", "learn"];

#[derive(Debug, Clone, PartialEq, Eq)]
struct Dep {
    name: String,
    kind: String,
    outside: Option<String>,
}

type Graph = BTreeMap<String, Vec<Dep>>;

fn workspace_graph() -> Result<Graph, String> {
    let metadata = cargo_metadata(&["--no-deps"]);
    let packages = metadata["packages"].as_array().ok_or("no packages array")?;
    let members: Vec<&str> = packages.iter().filter_map(|p| p["name"].as_str()).collect();
    let mut graph = Graph::new();
    for package in packages {
        let name = package["name"].as_str().ok_or("a package has no name")?;
        let deps = package["dependencies"]
            .as_array()
            .ok_or_else(|| format!("{name} has no dependencies array"))?
            .iter()
            .filter_map(|d| {
                let dep = d["name"].as_str()?;
                let path = d["path"].as_str();
                let outside = match (members.contains(&dep), path) {
                    (true, _) => None,
                    (false, Some(path)) => Some(path.to_owned()),
                    (false, None) => return None,
                };
                Some(Dep {
                    name: dep.to_owned(),
                    kind: d["kind"].as_str().unwrap_or("normal").to_owned(),
                    outside,
                })
            })
            .collect();
        graph.insert(name.to_owned(), deps);
    }
    Ok(graph)
}

fn check(graph: &Graph, root: &str) -> Result<(), String> {
    let mut stack = vec![vec![root.to_owned()]];
    let mut seen = vec![root.to_owned()];
    while let Some(path) = stack.pop() {
        let Some(last) = path.last() else { continue };
        if last == KERNEL {
            return Err(format!("{root} depends on {KERNEL}: {}", path.join(" -> ")));
        }
        let is_root = path.len() == 1;
        for dep in graph.get(last).into_iter().flatten() {
            if !is_root && dep.kind == "dev" {
                continue;
            }
            if let Some(dir) = &dep.outside {
                return Err(format!(
                    "{} -> {} is a path crate outside the workspace ({dir}), so this test \
                     can't see whether it reaches {KERNEL}; make it a workspace member",
                    path.join(" -> "),
                    dep.name
                ));
            }
            if !seen.contains(&dep.name) {
                seen.push(dep.name.clone());
                let mut next = path.clone();
                next.push(dep.name.clone());
                stack.push(next);
            }
        }
    }
    Ok(())
}

#[test]
fn clients_never_depend_on_the_kernel() {
    let graph = workspace_graph().unwrap();
    assert!(
        graph.contains_key(KERNEL),
        "{KERNEL} is not in the workspace: {:?}",
        graph.keys()
    );
    assert!(graph.contains_key("wisp-render"), "{:?}", graph.keys());
    for allowed in MAY_USE_KERNEL {
        assert!(
            graph.contains_key(*allowed),
            "{allowed} is not in the workspace"
        );
    }
    let failures: Vec<String> = graph
        .keys()
        .filter(|name| !MAY_USE_KERNEL.contains(&name.as_str()))
        .filter_map(|name| check(&graph, name).err())
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn the_walk_finds_transitive_and_hidden_kernel_dependencies() {
    let edge = |to: &str, kind: &str| Dep {
        name: to.to_owned(),
        kind: kind.to_owned(),
        outside: None,
    };
    let outside = |to: &str, kind: &str| Dep {
        outside: Some(format!("/elsewhere/{to}")),
        ..edge(to, kind)
    };
    let graph = Graph::from([
        ("a".to_owned(), vec![edge("b", "normal")]),
        ("b".to_owned(), vec![edge("c", "build")]),
        ("c".to_owned(), vec![edge(KERNEL, "normal")]),
        ("d".to_owned(), vec![edge("e", "normal")]),
        (
            "e".to_owned(),
            vec![edge(KERNEL, "dev"), outside("x", "dev")],
        ),
        ("f".to_owned(), vec![edge(KERNEL, "dev")]),
        ("g".to_owned(), vec![edge("h", "normal")]),
        ("h".to_owned(), vec![outside("x", "normal")]),
    ]);
    let err = check(&graph, "a").unwrap_err();
    assert!(err.ends_with("a -> b -> c -> wisp-core"), "{err}");
    assert_eq!(check(&graph, "d"), Ok(()));
    assert!(check(&graph, "f").unwrap_err().ends_with("f -> wisp-core"));
    let err = check(&graph, "g").unwrap_err();
    assert!(
        err.starts_with("g -> h -> x is a path crate outside"),
        "{err}"
    );
}
