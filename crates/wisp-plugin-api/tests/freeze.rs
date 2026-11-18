#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a missing folder or an accepted manifest fails the test"
)]

use std::path::Path;

use sha2::{Digest, Sha256};
use wisp_plugin_api::manifest::{Manifest, Origin, WORLDS};

const WIT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../wit");

fn folder(world: &str) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<(String, Vec<u8>)> =
        std::fs::read_dir(Path::new(WIT).join(format!("v{world}")))
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                let name = entry.file_name().into_string().unwrap();
                (name, std::fs::read(entry.path()).unwrap())
            })
            .collect();
    files.sort();
    files
}

fn digest(files: &[(String, Vec<u8>)]) -> String {
    let mut hash = Sha256::new();
    for (name, bytes) in files {
        hash.update(name.as_bytes());
        hash.update([0]);
        hash.update(bytes);
        hash.update([0]);
    }
    wisp_plugin_api::hex::encode(&hash.finalize())
}

fn frozen() -> impl Iterator<Item = (&'static str, &'static str)> {
    WORLDS
        .iter()
        .filter_map(|(world, hash)| hash.map(|hash| (*world, hash)))
}

#[test]
fn every_world_folder_is_listed_and_a_frozen_one_holds_exactly_its_hashed_bytes() {
    let mut folders: Vec<String> = std::fs::read_dir(WIT)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    folders.sort();
    let listed: Vec<String> = WORLDS
        .iter()
        .map(|(world, _)| format!("v{world}"))
        .collect();
    assert_eq!(folders, listed);
    assert_eq!(frozen().count(), 3);
    for (world, hash) in frozen() {
        let files = folder(world);
        assert_eq!(
            digest(&files),
            hash,
            "wit/v{world} is frozen: put the change in a new world's folder"
        );
        for at in 0..files.len() {
            let mut copy = files.clone();
            copy[at].1[0] ^= 1;
            assert_ne!(digest(&copy), hash, "{world} {}", copy[at].0);
        }
    }
}

fn expected_keys(world: &str, table: &str) -> Vec<String> {
    let text = format!(
        "id = \"acme\"\nversion = \"0.1.0\"\nworld = \"{world}\"\nsummary = \"Acme\"\n{table}zzz = 1\n"
    );
    let message = match Manifest::parse(&text, Origin::External) {
        Ok(manifest) => panic!("accepted: {manifest:?}"),
        Err(problem) => problem.message,
    };
    let (_, list) = message
        .split_once("expected ")
        .unwrap_or_else(|| panic!("{world} {table:?}: {message}"));
    let list = list.trim_start_matches("one of ").replace(" or ", ", ");
    list.split(", ")
        .map(|key| key.trim().trim_matches('`').to_owned())
        .collect()
}

const TOP: [&str; 11] = [
    "id",
    "version",
    "world",
    "summary",
    "settings",
    "system",
    "tools",
    "commands",
    "hooks",
    "points",
    "capabilities",
];
const HOOKS: [&str; 11] = [
    "name",
    "point",
    "after",
    "before",
    "requires",
    "timeout_ms",
    "on_failure",
    "background",
    "switch",
    "reserve",
    "from",
];

fn pinned(world: &str) -> Vec<(&'static str, Vec<&'static str>)> {
    let new = world != "0.1.0";
    let mut top = TOP.to_vec();
    let mut hooks = HOOKS.to_vec();
    if new {
        top.extend(["slots", "prompt_sections", "strategy"]);
        hooks.insert(2, "in");
        hooks.extend(["retry", "job_news", "continues"]);
    }
    if world == "0.3.0" {
        top.push("index");
    }
    let mut tool_keys = vec![
        "name",
        "default",
        "provides",
        "follows",
        "platforms",
        "native",
    ];
    if world == "0.3.0" {
        tool_keys.push("forwards");
    }
    let mut tables = vec![
        ("", top),
        (
            "[system]\ntext = \"Hi.\"\n",
            vec!["text", "after", "before"],
        ),
        ("[[tools]]\nname = \"t\"\n", tool_keys),
        (
            "[[commands]]\nname = \"c\"\nsummary = \"S\"\n",
            vec!["name", "aliases", "summary", "args", "busy", "timeout_ms"],
        ),
        (
            "[[commands]]\nname = \"c\"\nsummary = \"S\"\n[[commands.args]]\nname = \"a\"\nvalues = \"free\"\n",
            vec!["name", "values"],
        ),
        ("[[hooks]]\nname = \"h\"\npoint = \"after-turn\"\n", hooks),
        (
            "[[points]]\nname = \"p\"\nkind = \"observe\"\nversion = \"1\"\n",
            vec!["name", "kind", "version"],
        ),
        ("[capabilities]\nrequested = []\n", vec!["requested"]),
    ];
    if new {
        tables.extend([
            (
                "[[slots]]\n",
                vec![
                    "slot",
                    "name",
                    "title",
                    "after",
                    "before",
                    "default",
                    "marks",
                    "refresh_s",
                ],
            ),
            (
                "[[prompt_sections]]\n",
                vec!["name", "place", "in", "after", "before", "continues"],
            ),
            ("[strategy]\n", vec!["summary"]),
        ]);
    }
    if world == "0.3.0" {
        tables.push((
            "[[settings]]\n",
            vec!["name", "label", "help", "kind", "min", "max", "default"],
        ));
    }
    tables
}

#[test]
fn each_frozen_world_defines_the_same_manifest_keys_in_every_table() {
    for (world, _) in frozen() {
        for (table, keys) in pinned(world) {
            assert_eq!(expected_keys(world, table), keys, "{world} {table:?}");
        }
    }
}
