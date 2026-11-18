use std::collections::BTreeSet;

use crate::purity::sources;
use crate::tokens::repo;

const NAMES: [&str; 13] = [
    "wisp_web",
    "wisp-web",
    "learn_web",
    "learn-web",
    "--output web",
    "OutputMode::Web",
    "Presenter::Web",
    "Requested::Web",
    "Section::Web",
    "WEB_PORT",
    "web.port",
    "web_port",
    "interactive_education",
];

const CLIENT_NAMES: [&str; 5] = [
    "AudioLane",
    "voice::attach",
    "wisp-plugin-page",
    "wisp_plugin_page",
    "show_page",
];

const CLIENTS: [&str; 2] = ["wisp-cli/", "wisp-tui/"];

const ALLOWED: [(&str, &str); 3] = [
    (
        "wisp-tui/tests/dependencies.rs",
        "the clients' dependency check keeps learn-web protocol-only",
    ),
    (
        "wisp-core/tests/instruction_files.rs",
        "the instruction-file check reads the flow's prompts",
    ),
    (
        "wisp-core/tests/source_guards/app_names.rs",
        "this guard lists the names",
    ),
];

fn named(path: &str, text: &str) -> Vec<String> {
    let client = CLIENTS.iter().any(|c| path.starts_with(c));
    let names = NAMES.iter().chain(CLIENT_NAMES.iter().filter(|_| client));
    let names: Vec<&str> = names.copied().collect();
    text.lines()
        .enumerate()
        .flat_map(|(at, line)| {
            let names = names.clone();
            names
                .into_iter()
                .filter(move |name| line.contains(name))
                .map(move |name| format!("{path}:{}: {name}", at + 1))
        })
        .collect()
}

#[test]
fn wisp_crates_name_nothing_only_learn_uses() {
    let crates = repo().join("crates");
    let mut files = sources(&crates, "rs", &[], 1000);
    let manifests = sources(&crates, "toml", &[], 1);
    files.extend(manifests.into_iter().filter(|p| p.ends_with("Cargo.toml")));
    let files: Vec<(String, String)> = files
        .iter()
        .map(|p| {
            let path = p.strip_prefix(&crates).unwrap().to_string_lossy();
            (path.replace('\\', "/"), std::fs::read_to_string(p).unwrap())
        })
        .collect();
    for client in CLIENTS {
        assert!(files.iter().any(|(p, _)| p.starts_with(client)), "{client}");
    }
    let mut found = Vec::new();
    let mut allowed = BTreeSet::new();
    for (path, text) in &files {
        let hits = named(path, text);
        match ALLOWED.iter().find(|(site, _)| site == path) {
            Some((site, _)) if !hits.is_empty() => {
                allowed.insert(*site);
            }
            Some(_) => {}
            None => found.extend(hits),
        }
    }
    assert_eq!(found, Vec::<String>::new());
    let stale: Vec<&str> = ALLOWED
        .iter()
        .map(|(site, _)| *site)
        .filter(|site| !allowed.contains(site))
        .collect();
    assert_eq!(stale, Vec::<&str>::new());
}

#[test]
fn each_planted_name_fails_and_a_client_name_passes_outside_the_clients() {
    for name in NAMES.iter().chain(&CLIENT_NAMES) {
        let line = format!("let planted = \"{name}\";");
        assert_eq!(
            named("wisp-cli/src/planted.rs", &line),
            [format!("wisp-cli/src/planted.rs:1: {name}")]
        );
        let expected = usize::from(NAMES.contains(name));
        assert_eq!(
            named("wisp-core/src/planted.rs", &line).len(),
            expected,
            "{name}"
        );
    }
    assert!(named("wisp-client/src/planted.rs", "show_page").is_empty());
}
