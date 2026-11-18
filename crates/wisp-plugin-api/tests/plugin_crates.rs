#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "the guard fails the test on anything it can't read"
)]

#[path = "../../../tests/support/dependencies.rs"]
mod dependencies;
#[path = "../../../tests/support/purity.rs"]
mod purity;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde_json::{Value, json};

use dependencies::{cargo_metadata, host_triple, normal_graph};
use purity::rust_source::{Kind, Token, tokens, without_test_items};
use purity::{TEST_MODULES, sources};

const ALLOWED_WORKSPACE: [&str; 6] = [
    "wisp-plugin-api",
    "wisp-tool-text",
    "wisp-diff",
    "wisp-media",
    "wisp-instructions",
    "wisp-index",
];

const ALLOWED_PURE: [&str; 11] = [
    "infer",
    "serde_json",
    "jiff",
    "regex",
    "unicode-segmentation",
    "unicode-width",
    "similar",
    "pulldown-cmark",
    "hmac-sha256",
    "wit-bindgen",
    "shlex",
];

const FORBIDDEN: [&str; 9] = [
    "wisp-core",
    "wisp-protocol",
    "wisp-contain",
    "tokio",
    "rustix",
    "nix",
    "libc",
    "reqwest",
    "wasmtime",
];

const FORBIDDEN_STD: [&str; 5] = ["process", "thread", "net", "os", "env"];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[derive(Debug, Clone)]
struct Package {
    name: String,
    workspace: bool,
    plugin: bool,
    dir: PathBuf,
    deps: Vec<String>,
}

#[derive(Debug, Default)]
struct Graph {
    packages: BTreeMap<String, Package>,
}

impl Graph {
    fn from_metadata(metadata: &Value) -> Graph {
        let plugins =
            lexical(&Path::new(metadata["workspace_root"].as_str().unwrap()).join("plugins"));
        let members: BTreeSet<&str> = metadata["workspace_members"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m.as_str().unwrap())
            .collect();
        let deps = normal_graph(metadata);
        let mut packages = BTreeMap::new();
        for package in metadata["packages"].as_array().unwrap() {
            let id = package["id"].as_str().unwrap();
            let dir = lexical(
                Path::new(package["manifest_path"].as_str().unwrap())
                    .parent()
                    .unwrap(),
            );
            let workspace = members.contains(id);
            packages.insert(
                id.to_owned(),
                Package {
                    name: package["name"].as_str().unwrap().to_owned(),
                    workspace,
                    plugin: workspace && dir.starts_with(&plugins),
                    dir,
                    deps: deps.get(id).cloned().unwrap_or_default(),
                },
            );
        }
        Graph { packages }
    }

    fn closure(&self, id: &str) -> BTreeSet<String> {
        dependencies::closure(vec![id.to_owned()], |id| {
            self.packages.get(id).map(|p| &p.deps)
        })
    }

    fn name(&self, id: &str) -> &str {
        &self.packages[id].name
    }

    fn allowed(&self) -> BTreeSet<String> {
        let mut allowed = BTreeSet::new();
        for (id, package) in &self.packages {
            let workspace_root =
                package.workspace && ALLOWED_WORKSPACE.contains(&package.name.as_str());
            let pure_root = !package.workspace && ALLOWED_PURE.contains(&package.name.as_str());
            if workspace_root || pure_root {
                allowed.insert(package.name.clone());
                allowed.extend(self.closure(id).iter().map(|d| self.name(d).to_owned()));
            }
        }
        allowed
    }

    fn plugins(&self) -> Vec<&str> {
        self.packages
            .iter()
            .filter(|(_, p)| p.plugin)
            .map(|(id, _)| id.as_str())
            .collect()
    }

    fn violations(&self, plugin: &str) -> Vec<String> {
        let allowed = self.allowed();
        let who = self.name(plugin);
        let mut found = Vec::new();
        for dep in self.closure(plugin) {
            let package = &self.packages[&dep];
            let name = package.name.as_str();
            if FORBIDDEN.contains(&name) || name.starts_with("wasmtime") {
                found.push(format!("{who} reaches {name}, which no plugin may use"));
            } else if package.workspace && !ALLOWED_WORKSPACE.contains(&name) {
                found.push(format!("{who} reaches the workspace crate {name}"));
            } else if !package.workspace && !allowed.contains(name) {
                found.push(format!(
                    "{who} reaches {name}, which isn't on the allow-list"
                ));
            }
        }
        found
    }
}

fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(part);
                }
            }
            other => out.push(other),
        }
    }
    out
}

fn is_punct(tokens: &[Token], i: usize, c: char) -> bool {
    tokens
        .get(i)
        .is_some_and(|t| t.kind == Kind::Punct && t.text.chars().eq([c]))
}

fn is_ident(tokens: &[Token], i: usize, word: &str) -> bool {
    tokens
        .get(i)
        .is_some_and(|t| t.kind == Kind::Ident && t.text == word)
}

fn is_path_sep(tokens: &[Token], i: usize) -> bool {
    is_punct(tokens, i, ':') && is_punct(tokens, i + 1, ':')
}

fn paths(tokens: &[Token], mut i: usize, prefix: &[String], out: &mut Vec<Vec<String>>) -> usize {
    let mut path = prefix.to_vec();
    loop {
        match tokens.get(i).map(|t| (&t.kind, t.text.as_str())) {
            Some((Kind::Ident, word)) => {
                path.push(word.to_owned());
                i += 1;
            }
            Some((Kind::Punct, "{")) => {
                i += 1;
                while i < tokens.len() && !is_punct(tokens, i, '}') {
                    i = paths(tokens, i, &path, out);
                    if is_punct(tokens, i, ',') {
                        i += 1;
                    }
                }
                return i + 1;
            }
            _ => {
                out.push(path);
                return i;
            }
        }
        if is_path_sep(tokens, i) {
            i += 2;
        } else {
            out.push(path);
            return i;
        }
    }
}

fn std_uses(source: &str) -> Vec<String> {
    let tokens = without_test_items(tokens(source));
    let mut found = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let build_env = ["env", "option_env"]
            .into_iter()
            .find(|word| is_ident(&tokens, i, word));
        if let Some(word) = build_env
            && is_punct(&tokens, i + 1, '!')
            && !is_punct(&tokens, i + 2, '=')
        {
            found.push(format!("{word}!"));
            i += 2;
            continue;
        }
        let starts = is_ident(&tokens, i, "std") && is_path_sep(&tokens, i + 1);
        if starts {
            let mut all = Vec::new();
            i = paths(&tokens, i, &[], &mut all);
            for path in all {
                let second = path.get(1).map(String::as_str).unwrap_or_default();
                let third = path.get(2).map(String::as_str);
                let bad = FORBIDDEN_STD.contains(&second)
                    || (second == "time" && third != Some("Duration"));
                if bad {
                    found.push(path.join("::"));
                }
            }
        } else {
            i += 1;
        }
    }
    found
}

fn hash_collections(source: &str) -> Vec<String> {
    tokens(source)
        .into_iter()
        .filter(|t| t.kind == Kind::Ident && matches!(t.text.as_str(), "HashMap" | "HashSet"))
        .map(|t| t.text)
        .collect()
}

struct Fake {
    graph: Graph,
}

impl Fake {
    fn new() -> Fake {
        let mut fake = Fake {
            graph: Graph::default(),
        };
        fake.workspace("wisp-plugin-api", &["futures", "serde", "toml"])
            .workspace("wisp-tool-text", &["wisp-plugin-api"])
            .workspace(
                "wisp-diff",
                &["similar", "unicode-segmentation", "unicode-width"],
            )
            .workspace(
                "wisp-media",
                &["image", "image-webp", "base64", "thiserror"],
            )
            .workspace("wisp-core", &["tokio", "wisp-protocol", "wisp-plugin-api"])
            .workspace("wisp-protocol", &["serde", "serde_json"])
            .workspace("wisp-render", &["wisp-protocol", "wisp-diff"])
            .registry("futures", &[])
            .registry("serde", &[])
            .registry("toml", &["serde", "winnow"])
            .registry("winnow", &[])
            .registry("similar", &[])
            .registry("unicode-segmentation", &[])
            .registry("unicode-width", &[])
            .registry("image", &["png", "zune-jpeg"])
            .registry("image-webp", &[])
            .registry("png", &["fdeflate"])
            .registry("fdeflate", &[])
            .registry("zune-jpeg", &[])
            .registry("base64", &[])
            .registry("thiserror", &[])
            .registry("serde_json", &["serde", "itoa"])
            .registry("itoa", &[])
            .registry("regex", &["regex-syntax"])
            .registry("regex-syntax", &[])
            .registry("tokio", &[])
            .registry("openssl", &[]);
        fake
    }

    fn add(&mut self, name: &str, workspace: bool, plugin: bool, deps: &[&str]) -> &mut Fake {
        self.graph.packages.insert(
            name.to_owned(),
            Package {
                name: name.to_owned(),
                workspace,
                plugin,
                dir: PathBuf::from(name),
                deps: deps.iter().map(|d| (*d).to_owned()).collect(),
            },
        );
        self
    }

    fn workspace(&mut self, name: &str, deps: &[&str]) -> &mut Fake {
        self.add(name, true, false, deps)
    }

    fn registry(&mut self, name: &str, deps: &[&str]) -> &mut Fake {
        self.add(name, false, false, deps)
    }

    fn plugin(&mut self, deps: &[&str]) -> Vec<String> {
        self.add("wisp-plugin-acme", true, true, deps);
        self.graph.violations("wisp-plugin-acme")
    }
}

#[test]
fn a_plugin_that_uses_wisp_media_and_its_whole_closure_passes() {
    let found = Fake::new().plugin(&[
        "wisp-plugin-api",
        "wisp-tool-text",
        "wisp-media",
        "wisp-diff",
        "image",
        "regex",
        "serde_json",
    ]);
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn wisp_core_is_refused_directly_and_through_another_crate() {
    let direct = Fake::new().plugin(&["wisp-plugin-api", "wisp-core"]);
    assert!(
        direct.contains(&"wisp-plugin-acme reaches wisp-core, which no plugin may use".to_owned()),
        "{direct:?}"
    );
    let mut fake = Fake::new();
    fake.workspace("wisp-tool-text", &["wisp-plugin-api", "wisp-core"]);
    let through = fake.plugin(&["wisp-tool-text"]);
    assert!(
        through.contains(&"wisp-plugin-acme reaches wisp-core, which no plugin may use".to_owned()),
        "{through:?}"
    );
    assert!(through.iter().any(|v| v.contains("tokio")), "{through:?}");
}

#[test]
fn tokio_is_refused_directly_and_deep_in_the_closure() {
    let direct = Fake::new().plugin(&["tokio"]);
    assert_eq!(
        direct,
        ["wisp-plugin-acme reaches tokio, which no plugin may use"]
    );
    let mut fake = Fake::new();
    fake.registry("regex-syntax", &["tokio"]);
    let deep = fake.plugin(&["regex"]);
    assert_eq!(
        deep,
        ["wisp-plugin-acme reaches tokio, which no plugin may use"]
    );
}

#[test]
fn a_forbidden_crate_stays_forbidden_inside_an_allowed_crates_closure() {
    let mut fake = Fake::new();
    fake.registry("png", &["fdeflate", "libc"])
        .registry("libc", &[]);
    let found = fake.plugin(&["wisp-media"]);
    assert_eq!(
        found,
        ["wisp-plugin-acme reaches libc, which no plugin may use"]
    );
    let mut fake = Fake::new();
    fake.workspace("wisp-media", &["image", "wisp-protocol"]);
    let found = fake.plugin(&["wisp-media"]);
    assert!(
        found.contains(
            &"wisp-plugin-acme reaches wisp-protocol, which no plugin may use".to_owned()
        ),
        "{found:?}"
    );
}

#[test]
fn other_workspace_crates_and_unlisted_crates_are_refused() {
    let found = Fake::new().plugin(&["wisp-render"]);
    assert!(
        found.contains(&"wisp-plugin-acme reaches the workspace crate wisp-render".to_owned()),
        "{found:?}"
    );
    let found = Fake::new().plugin(&["openssl"]);
    assert_eq!(
        found,
        ["wisp-plugin-acme reaches openssl, which isn't on the allow-list"]
    );
    let mut fake = Fake::new();
    fake.registry("wasmtime-wasi", &[]);
    assert_eq!(
        fake.plugin(&["wasmtime-wasi"]),
        ["wisp-plugin-acme reaches wasmtime-wasi, which no plugin may use"]
    );
}

fn fake_metadata(tokio_kind: Value, workspace_root: &str) -> Value {
    let node = |id: &str, deps: Value| json!({ "id": id, "deps": deps });
    let package = |id: &str, path: &str| json!({ "id": id, "name": id, "manifest_path": path });
    json!({
        "workspace_root": workspace_root,
        "workspace_members": ["acme", "wisp-plugin-api"],
        "packages": [
            package("acme", "/w/plugins/acme/Cargo.toml"),
            package("wisp-plugin-api", "/w/crates/wisp-plugin-api/Cargo.toml"),
            package("tokio", "/registry/tokio/Cargo.toml"),
        ],
        "resolve": { "nodes": [
            node("acme", json!([
                { "pkg": "wisp-plugin-api", "dep_kinds": [{ "kind": null, "target": null }] },
                { "pkg": "tokio", "dep_kinds": [{ "kind": tokio_kind, "target": null }] },
            ])),
            node("wisp-plugin-api", json!([])),
            node("tokio", json!([])),
        ]},
    })
}

#[test]
fn dev_and_build_dependencies_are_free_and_normal_ones_are_checked() {
    for kind in [json!("dev"), json!("build")] {
        let graph = Graph::from_metadata(&fake_metadata(kind.clone(), "/w"));
        assert_eq!(graph.plugins(), ["acme"]);
        assert!(graph.violations("acme").is_empty(), "{kind}");
    }
    let graph = Graph::from_metadata(&fake_metadata(Value::Null, "/w"));
    assert_eq!(
        graph.violations("acme"),
        ["acme reaches tokio, which no plugin may use"]
    );
}

#[test]
fn a_plugin_is_found_however_the_workspace_root_is_spelled() {
    for root in [
        "/w",
        "/w/",
        "/w/crates/wisp-plugin-api/../..",
        "/w/./crates/..",
    ] {
        let graph = Graph::from_metadata(&fake_metadata(Value::Null, root));
        assert_eq!(graph.plugins(), ["acme"], "{root}");
        assert_eq!(
            graph.violations("acme"),
            ["acme reaches tokio, which no plugin may use"],
            "{root}"
        );
    }
    let mut metadata = fake_metadata(Value::Null, "/w");
    metadata["packages"][0]["manifest_path"] = json!("/w/crates/x/../../plugins/acme/Cargo.toml");
    assert_eq!(Graph::from_metadata(&metadata).plugins(), ["acme"]);
    let elsewhere = Graph::from_metadata(&fake_metadata(Value::Null, "/other"));
    assert!(elsewhere.plugins().is_empty());
}

#[test]
fn the_source_scan_finds_forbidden_std_modules() {
    let cases = [
        ("use std::process::Command;", "std::process::Command"),
        ("let v = std::env::var(\"X\");", "std::env::var"),
        ("use std::{fs, thread};", "std::thread"),
        ("use std::time::{Duration, Instant};", "std::time::Instant"),
        ("use std::time;", "std::time"),
        ("use std::time::{self, Duration};", "std::time::self"),
        (
            "use std::os::unix::fs::FileTypeExt;",
            "std::os::unix::fs::FileTypeExt",
        ),
        (
            "let s = ::std::net::TcpStream::connect(a);",
            "std::net::TcpStream::connect",
        ),
        (
            "use std::{io::{Read, Write}, process::{Command, Stdio}};",
            "std::process::Command",
        ),
        (
            "let t = std::time::SystemTime::now();",
            "std::time::SystemTime::now",
        ),
        ("use ::std::process::Command;", "std::process::Command"),
        ("return ::std::env::var(\"A\");", "std::env::var"),
        ("pub use std::thread::spawn;", "std::thread::spawn"),
        ("let d = env!(\"CARGO_MANIFEST_DIR\");", "env!"),
        ("let k = option_env!(\"HOME\");", "option_env!"),
        (
            r"let c = b'\''; std::thread::yield_now();",
            "std::thread::yield_now",
        ),
        ("#[cfg(any(test, unix))] use std::process;", "std::process"),
        (
            "const V: &str = core::option_env!(\"V\").unwrap_or(\"\");",
            "option_env!",
        ),
    ];
    for (source, path) in cases {
        let found = std_uses(source);
        assert!(found.contains(&path.to_owned()), "{source}: {found:?}");
    }
}

#[test]
fn the_source_scan_skips_strings_test_code_and_allowed_paths() {
    for source in [
        "use std::time::Duration;",
        "let d = std::time::Duration::from_secs(1);",
        "use std::fs; use std::collections::BTreeMap; use std::io::Read;",
        "let s = \"std::process::Command\";",
        "let s = r#\"std::env::var\"#;",
        "let c = '\"'; let s = \"std::thread\";",
        "fn f<'a>(x: &'a str) -> &'a str { x } let environment = 1; fn process() {}",
        r###"let s = r"std::process"; let s = br##"std::env"##; let s = cr#"std::thread"#;"###,
        r#"let s = b"std::process"; let s = c"std::env"; let c = b'\''; let c = '\'';"#,
        "/****** std::process /* std::env */ std::thread */",
        "#[allow(dead_code)] #[cfg (test)] #[inline] pub(crate) fn f() { std::process::exit(0); }",
        "#[cfg(test)] const F: fn() = (|| { std::process::exit(0); });",
        "#[cfg(test)] fn f() -> Result<(), String> { std::process::exit(0); }",
        "#[cfg(test)] pub(crate) async fn f<A, B>() { std::process::exit(0); }",
        "#[cfg(test)] const M: Map<A, B> = std::process::id();",
        "#[cfg(test)] const F: [u8; { 1 }] = [std::process::id() as u8];",
        "#[cfg(test)]\nmod tests;",
        "#[cfg(test)]\nmod tests {\n    use std::process::Command;\n    fn f() { let _ = std::env::var(\"A\"); }\n}\n",
        "#[cfg(test)]\nuse std::time::Instant;",
        "mod process { pub fn env() {} } use self::process::env;",
        "let same = env != option_env; let s = \"env!(X)\";",
        "#[cfg(test)]\nmod tests { const D: &str = env!(\"CARGO_MANIFEST_DIR\"); }",
    ] {
        assert!(
            std_uses(source).is_empty(),
            "{source}: {:?}",
            std_uses(source)
        );
    }
    assert_eq!(tokens(r#"b'"'"#).len(), 1);
    let mixed = "#[cfg(test)]\nmod tests { use std::env; }\nuse std::thread::sleep;";
    assert_eq!(std_uses(mixed), ["std::thread::sleep"]);
    let mixed = r##"#[cfg(test)] #[allow(dead_code)] pub(in crate) mod t { let s = r#"}"#; std::env::var("X"); } std::thread::sleep();"##;
    assert_eq!(std_uses(mixed), ["std::thread::sleep"]);
}

#[test]
fn the_source_scan_skips_test_files_and_test_directories() {
    let dir = std::env::temp_dir().join(format!("wisp-plugin-crates-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src/tests")).unwrap();
    std::fs::create_dir_all(dir.join("src/run")).unwrap();
    for file in [
        "src/lib.rs",
        "src/tests.rs",
        "src/tests/case.rs",
        "src/run/tests.rs",
        "src/run/mod.rs",
    ] {
        std::fs::write(dir.join(file), "").unwrap();
    }
    let files = sources(&dir.join("src"), "rs", &["tests"], 1);
    std::fs::remove_dir_all(&dir).unwrap();
    let names: Vec<String> = files
        .iter()
        .map(|f| {
            f.strip_prefix(&dir)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    assert_eq!(names, ["src/lib.rs", "src/run/mod.rs"]);
}

fn has_target_table(manifest: &str) -> bool {
    manifest
        .parse::<toml::Table>()
        .unwrap()
        .contains_key("target")
}

#[test]
fn a_target_table_in_a_plugin_manifest_is_found() {
    assert!(has_target_table(
        "[target.'cfg(unix)'.dependencies]\nlibc = \"1\"\n"
    ));
    assert!(!has_target_table("[dependencies]\nserde = \"1\"\n"));
}

#[test]
fn every_plugin_crate_in_the_workspace_keeps_to_its_allowed_closure() {
    let graph = Graph::from_metadata(&cargo_metadata(&["--filter-platform", &host_triple()]));
    let on_disk = std::fs::read_dir(root().join("plugins"))
        .unwrap()
        .filter(|e| e.as_ref().unwrap().path().join("Cargo.toml").is_file())
        .count();
    assert_eq!(graph.plugins().len(), on_disk);
    let api: Vec<&str> = graph
        .packages
        .iter()
        .filter(|(_, p)| p.workspace && p.name == "wisp-plugin-api")
        .map(|(id, _)| id.as_str())
        .collect();
    assert_eq!(api.len(), 1);
    let media = graph
        .packages
        .iter()
        .find(|(_, p)| p.workspace && p.name == "wisp-media")
        .map(|(id, _)| id.clone())
        .unwrap();
    let closure: BTreeSet<&str> = graph
        .closure(&media)
        .iter()
        .map(|d| graph.name(d))
        .collect();
    for expected in ["image", "png", "zune-jpeg", "base64", "thiserror"] {
        assert!(
            closure.contains(expected),
            "{expected} missing from {closure:?}"
        );
    }
    let allowed = graph.allowed();
    for name in ["serde", "toml", "futures", "image", "png", "similar"] {
        assert!(allowed.contains(name), "{name}");
    }
    let mut failures = Vec::new();
    for plugin in graph.plugins() {
        failures.extend(graph.violations(plugin));
        let package = &graph.packages[plugin];
        let manifest = std::fs::read_to_string(package.dir.join("Cargo.toml")).unwrap();
        if has_target_table(&manifest) {
            failures.push(format!(
                "{}: a [target] table hides dependencies from this guard",
                package.name
            ));
        }
        let src = package.dir.join("src");
        let files = if src.is_dir() {
            sources(&src, "rs", &TEST_MODULES, 0)
        } else {
            vec![]
        };
        if files.is_empty() {
            failures.push(format!(
                "{plugin}: no source files found under {}",
                src.display()
            ));
        }
        for file in files {
            let source = std::fs::read_to_string(&file).unwrap();
            for path in std_uses(&source) {
                failures.push(format!("{}: {path}", file.display()));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn the_hash_scan_finds_hash_collections_outside_strings() {
    assert_eq!(
        hash_collections("use std::collections::HashMap;"),
        ["HashMap"]
    );
    assert_eq!(hash_collections("let s: HashSet<u8> = x;"), ["HashSet"]);
    assert_eq!(
        hash_collections(r"let c = b'\''; let s: HashSet<u8>;"),
        ["HashSet"]
    );
    assert_eq!(hash_collections("#[cfg(test)] use HashMap;"), ["HashMap"]);
    assert!(
        hash_collections(r###"r#"HashMap"#; br"HashSet"; cr##"HashMap"##; 'HashMap"###).is_empty()
    );
    assert!(hash_collections("let s = \"HashMap\"; let t: BTreeMap<u8, u8>;").is_empty());
}

#[test]
fn no_plugin_crate_the_api_or_a_host_service_uses_a_hash_collection() {
    let mut files = sources(&root().join("crates/wisp-plugin-api/src"), "rs", &[], 7);
    let before = files.len();
    for dir in [
        "crates/wisp-tool-text/src",
        "crates/wisp-instructions/src",
        "crates/wisp-index/src",
    ] {
        files.extend(sources(&root().join(dir), "rs", &[], 1));
    }
    let core = root().join("crates/wisp-core/src");
    for service in ["jobs", "call", "plugin"] {
        files.push(core.join(format!("{service}.rs")));
        files.extend(sources(&core.join(service), "rs", &[], 1));
    }
    files.push(core.join("agent/plugins.rs"));
    let protocol = root().join("crates/wisp-protocol/src");
    for module in ["plugin", "slot"] {
        files.push(protocol.join(format!("{module}.rs")));
        files.extend(sources(&protocol.join(module), "rs", &[], 1));
    }
    for dir in ["crates/wisp-plugins/src", "crates/wisp-contain/src"] {
        files.extend(sources(&root().join(dir), "rs", &[], 1));
    }
    for file in &files {
        assert!(file.is_file(), "{} is missing", file.display());
    }
    assert!(files.len() >= before + 45, "{files:?}");
    if let Ok(entries) = std::fs::read_dir(root().join("plugins")) {
        for entry in entries {
            let src = entry.unwrap().path().join("src");
            if src.is_dir() {
                files.extend(sources(&src, "rs", &[], 1));
            }
        }
    }
    let mut failures = Vec::new();
    for file in files {
        let found = hash_collections(&std::fs::read_to_string(&file).unwrap());
        if !found.is_empty() {
            failures.push(format!("{}: {found:?}", file.display()));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
