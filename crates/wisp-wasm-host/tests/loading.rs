#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a failed step fails the test"
)]

mod support;

use support::{Rig, fixture_bytes, fixture_manifest, install_fixture, place, sha256};

const EMPTY_COMPONENT: [u8; 8] = [0x00, 0x61, 0x73, 0x6d, 0x0d, 0x00, 0x01, 0x00];

fn manifest_as(id: &str, extra: &str) -> String {
    fixture_manifest().replacen("id = \"wasm-fixture\"", &format!("id = \"{id}\""), 1) + extra
}

#[test]
fn wasm_plugins_name_the_world_the_host_binds_which_is_frozen() {
    for world in wisp_wasm_host::BOUND {
        assert!(wisp_plugin_api::manifest::frozen(world));
    }
}

#[test]
fn the_fixture_loads_with_its_sha256_and_manifest() {
    let rig = Rig::new("loads");
    let found = rig.fixture();
    assert_eq!(found.sha256, sha256(&fixture_bytes()));
    assert_eq!(found.manifest, fixture_manifest());
    let folder = rig.dirs.data.join("plugins/wasm-fixture");
    let one = wisp_wasm_host::read(&rig.dirs, &folder)
        .map_err(|(_, e)| e)
        .unwrap();
    assert_eq!((one.id, one.sha256), (found.id, found.sha256));
    assert_eq!(
        wisp_wasm_host::stems(&folder).unwrap(),
        [sha256(&fixture_bytes())]
    );
    let gone = wisp_wasm_host::read(&rig.dirs, &rig.dirs.data.join("plugins/gone"));
    assert!(
        matches!(gone, Err((id, e)) if id == "gone" && e.starts_with("its folder can't be read"))
    );
}

#[test]
fn each_broken_plugin_is_left_out_with_a_problem_and_the_rest_load() {
    let rig = Rig::new("broken");
    let data = &rig.dirs.data;
    install_fixture(data);
    let wasm = fixture_bytes();
    let good = sha256(&wasm);
    place(
        data,
        "misnamed",
        &manifest_as("misnamed", ""),
        &wasm,
        &"0".repeat(64),
    );
    let tools_only = manifest_as("old-world", "");
    let tools_only = tools_only
        .split("\n[[hooks]]")
        .next()
        .unwrap()
        .replace("index = true\n", "");
    let old = tools_only.replace("world = \"0.3.0\"", "world = \"0.1.0\"");
    place(data, "old-world", &old, &wasm, &good);
    let older = tools_only.replace("old-world", "older-world");
    let older = older.replace("world = \"0.3.0\"", "world = \"0.2.0\"");
    place(data, "older-world", &older, &wasm, &good);
    let empty = sha256(&EMPTY_COMPONENT);
    assert_eq!(
        empty,
        "24eaf3439a60c96764c4f2c92a0a81b52bd4fe5d5b91f090184a0f6486b1bf29"
    );
    place(
        data,
        "other-world",
        &manifest_as("other-world", ""),
        &EMPTY_COMPONENT,
        &empty,
    );
    place(data, "bundled", &manifest_as("shell", ""), &wasm, &good);
    place(data, "garbled", "id = ", &wasm, &good);
    std::os::unix::fs::symlink(data.join("plugins/gone"), data.join("plugins/locked")).unwrap();
    let installed = rig.installed();
    let loaded: Vec<String> = installed
        .plugins
        .iter()
        .map(|found| found.manifest.lines().next().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(loaded, ["id = \"wasm-fixture\""]);
    let problems: Vec<(&str, &str)> = installed
        .problems
        .iter()
        .map(|(id, message)| (id.as_str(), message.as_str()))
        .collect();
    let expect = [
        ("bundled", "reserved for a bundled plugin"),
        ("garbled", ""),
        ("locked", "its folder can't be read"),
        (
            "misnamed",
            "its component's SHA-256 doesn't match its file name",
        ),
        ("old-world", "WASM plugins name world 0.2.0 or 0.3.0"),
        ("older-world", "its component doesn't fit world 0.2.0"),
        ("other-world", "its component doesn't fit world 0.3.0"),
    ];
    assert_eq!(problems.len(), expect.len(), "{problems:?}");
    for ((id, message), (want_id, want)) in problems.iter().zip(expect) {
        assert_eq!(*id, want_id);
        assert!(message.contains(want), "{id}: {message}");
    }
}

#[test]
fn the_listing_order_on_disk_changes_nothing() {
    let first = Rig::new("order-a");
    let second = Rig::new("order-b");
    let wasm = fixture_bytes();
    let good = sha256(&wasm);
    for id in ["zeta", "alpha", "mid"] {
        place(&first.dirs.data, id, &manifest_as(id, ""), &wasm, &good);
    }
    for id in ["mid", "zeta", "alpha"] {
        place(&second.dirs.data, id, &manifest_as(id, ""), &wasm, &good);
    }
    let manifests = |rig: &Rig| -> Vec<String> {
        rig.installed()
            .plugins
            .into_iter()
            .map(|found| found.manifest)
            .collect()
    };
    let listed = manifests(&first);
    assert_eq!(listed.len(), 3);
    assert!(listed[0].contains("alpha") && listed[2].contains("zeta"));
    assert_eq!(listed, manifests(&second));
}

#[test]
fn no_plugins_folder_finds_nothing() {
    let rig = Rig::new("none");
    let installed = rig.installed();
    assert!(installed.plugins.is_empty() && installed.problems.is_empty());
}

#[test]
fn dirs_debug_names_the_variables_and_hides_their_values() {
    let mut rig = Rig::new("debug");
    rig.dirs.env = vec![("API_KEY".to_owned(), "sk-secret-value".to_owned())];
    let shown = format!("{:?}", rig.dirs);
    assert!(shown.contains("API_KEY"), "{shown}");
    assert!(!shown.contains("sk-secret-value"), "{shown}");
}
