#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a failed step fails the test"
)]

mod support;

use std::path::{Path, PathBuf};

use support::{Rig, fixture_bytes, fixture_manifest, sha256};
use wisp_wasm_guests::{WASM_FIXTURE, WASM_FIXTURE_MANIFEST};
use wisp_wasm_host::{Install, install};

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).map_err(|e| e.to_string())
}

fn add(rig: &Rig, manifest: &Path, wasm: &Path) -> Result<Install<String>, String> {
    install(manifest, wasm, &rig.dirs, |found| Ok(found.id), write)
}

fn folder(rig: &Rig) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(rig.dirs.data.join("plugins/wasm-fixture"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn newer(rig: &Rig) -> (PathBuf, PathBuf, String) {
    let mut wasm = fixture_bytes();
    wasm.extend([0, 4, 3, b'n', b'e', b'w']);
    let sha = sha256(&wasm);
    let path = rig.scratch.root.join("newer.wasm");
    std::fs::write(&path, wasm).unwrap();
    let manifest = rig.scratch.root.join("newer.toml");
    std::fs::write(&manifest, fixture_manifest().replacen("0.1.0", "0.2.0", 1)).unwrap();
    (manifest, path, sha)
}

#[test]
fn an_install_writes_the_pair_that_loads_and_a_newer_one_replaces_it() {
    let rig = Rig::new("install");
    let (manifest, wasm) = (Path::new(WASM_FIXTURE_MANIFEST), Path::new(WASM_FIXTURE));
    let old = sha256(&fixture_bytes());
    assert!(matches!(add(&rig, manifest, wasm), Ok(Install::Added(id)) if id == "wasm-fixture"));
    assert_eq!(folder(&rig), [format!("{old}.toml"), format!("{old}.wasm")]);
    assert_eq!(rig.installed().plugins[0].sha256, old);
    assert!(matches!(
        add(&rig, manifest, wasm),
        Ok(Install::Unchanged(_))
    ));
    let (manifest, wasm, new) = newer(&rig);
    assert!(matches!(add(&rig, &manifest, &wasm), Ok(Install::Added(_))));
    assert_eq!(folder(&rig), [format!("{new}.toml"), format!("{new}.wasm")]);
    let installed = rig.installed();
    assert!(installed.problems.is_empty(), "{:?}", installed.problems);
    assert_eq!(installed.plugins[0].sha256, new);
    assert!(installed.plugins[0].manifest.contains("0.2.0"));
}

#[test]
fn a_refused_check_or_write_leaves_the_installed_pair_alone() {
    let rig = Rig::new("install-refused");
    let (manifest, wasm) = (Path::new(WASM_FIXTURE_MANIFEST), Path::new(WASM_FIXTURE));
    assert!(add(&rig, manifest, wasm).is_ok());
    let before = folder(&rig);
    let (manifest, wasm, _) = newer(&rig);
    let checked = install(
        &manifest,
        &wasm,
        &rig.dirs,
        |_| Err::<(), _>("its tool `count` clashes".to_owned()),
        write,
    );
    assert_eq!(
        checked.err().unwrap(),
        "wasm-fixture: its tool `count` clashes; nothing was added"
    );
    assert_eq!(folder(&rig), before);
    let written = install(
        &manifest,
        &wasm,
        &rig.dirs,
        |_| Ok(()),
        |path, bytes| match path.extension().and_then(|e| e.to_str()) {
            Some("toml") => Err("disk full".to_owned()),
            _ => write(path, bytes),
        },
    );
    assert_eq!(
        written.err().unwrap(),
        "wasm-fixture: its files can't be written: disk full; nothing was added"
    );
    assert_eq!(folder(&rig).len(), 3);
    let installed = rig.installed();
    assert!(installed.problems.is_empty(), "{:?}", installed.problems);
    assert!(
        installed.plugins[0]
            .manifest
            .contains("version = \"0.1.0\"")
    );
}
