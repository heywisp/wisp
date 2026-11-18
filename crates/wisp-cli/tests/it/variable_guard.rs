#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a source scan fails the test on any read error"
)]

#[path = "../../../../tests/support/purity.rs"]
mod purity;

use std::path::{Path, PathBuf};

use wisp_protocol::settings::SETTINGS;
use wisp_tui::term::caps::ENV_KEYS;

use purity::{TEST_MODULES, sources};

fn read_all(crates: &Path, dirs: &[&str], extension: &str, skip: &[&str]) -> Vec<(String, String)> {
    let mut files = Vec::new();
    for dir in dirs {
        let dir = crates.join(dir);
        if dir.is_dir() {
            files.extend(sources(&dir, extension, skip, 0));
        }
    }
    files
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path).unwrap();
            let shown = path
                .strip_prefix(crates)
                .unwrap_or(&path)
                .display()
                .to_string();
            (shown, text)
        })
        .collect()
}

fn crates() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn non_test_sources() -> Vec<(String, String)> {
    let crates = crates();
    let dirs: Vec<String> = std::fs::read_dir(&crates)
        .unwrap()
        .flatten()
        .map(|entry| format!("{}/src", entry.file_name().to_string_lossy()))
        .collect();
    let dirs: Vec<&str> = dirs.iter().map(String::as_str).collect();
    read_all(&crates, &dirs, "rs", &TEST_MODULES)
}

fn literal_reads(text: &str, name: &str) -> usize {
    [
        format!("std::env::var(\"{name}\")"),
        format!("std::env::var_os(\"{name}\")"),
    ]
    .iter()
    .map(|call| text.matches(call.as_str()).count())
    .sum()
}

#[test]
fn every_setting_variable_is_read_in_one_known_place() {
    let files = non_test_sources();
    let names: Vec<(&str, &str)> = SETTINGS
        .iter()
        .flat_map(|spec| spec.env.iter().map(move |var| (spec.name, *var)))
        .collect();
    assert!(!names.is_empty());
    for (setting, var) in names {
        let reads: usize = files.iter().map(|(_, text)| literal_reads(text, var)).sum();
        if setting.starts_with("display.") {
            assert!(
                ENV_KEYS.contains(&var),
                "{var} is read by Env::current through ENV_KEYS"
            );
            assert_eq!(
                reads, 0,
                "{var} is read only through ENV_KEYS and display_env's SETTINGS names"
            );
        } else if var == "WISP_ACCOUNT" || var == "WISP_ENDPOINT" {
            let (_, endpoints) = files
                .iter()
                .find(|(path, _)| path.ends_with("wisp-start/src/endpoints.rs"))
                .unwrap();
            assert!(endpoints.contains("const CHOICE_VAR: &str = \"WISP_ACCOUNT\";"));
            assert_eq!(literal_reads(endpoints, var), 1, "{var}");
            let quoted = format!("\"{var}\"");
            let elsewhere: Vec<&str> = files
                .iter()
                .filter(|(path, text)| {
                    !path.ends_with("wisp-start/src/endpoints.rs")
                        && !path.ends_with("wisp-protocol/src/settings/registry.rs")
                        && text.contains(&quoted)
                })
                .map(|(path, _)| path.as_str())
                .collect();
            assert_eq!(elsewhere, Vec::<&str>::new(), "{var}");
            assert_eq!(reads, 1, "{var}");
        } else {
            assert_eq!(reads, 1, "{var} must be read with one literal call");
        }
    }
}

#[test]
fn settings_code_reads_no_variable_by_a_computed_name() {
    for (path, text) in non_test_sources() {
        let settings = path.ends_with("wisp-cli/src/settings.rs")
            || path.ends_with("wisp-start/src/settings.rs")
            || path.ends_with("wisp-core/src/settings.rs")
            || path.ends_with("wisp-core/src/config.rs")
            || path.contains("wisp-protocol/src/settings");
        if !settings {
            continue;
        }
        for banned in [
            "use std::env",
            "std::env::vars",
            "std::env::var(",
            "real_env",
        ] {
            assert!(!text.contains(banned), "{banned} in {path}");
        }
    }
}

#[test]
fn no_snapshot_shows_env_or_endpoint() {
    let snapshots = read_all(
        &crates(),
        &["wisp-render", "wisp-tui", "wisp-cli"],
        "snap",
        &[],
    );
    assert!(snapshots.len() > 100, "{}", snapshots.len());
    let kept = ["--endpoint", "endpoints.", "set endpoint", "endpoint = "];
    let typed = "\u{203a} /endpoint zz";
    let banned = [wisp_protocol::KEYS_FILE_ALIAS, "endpoint", "endpoints"];
    let mut shown: Vec<String> = Vec::new();
    for (path, text) in &snapshots {
        let body = text.splitn(3, "---\n").nth(2).unwrap_or(text);
        for line in body.lines() {
            let line = line.trim_start_matches('|');
            if line.starts_with("== ") || path.contains("suggest-args-none") && line.contains(typed)
            {
                continue;
            }
            let line = kept
                .iter()
                .fold(line.to_owned(), |l, form| l.replace(form, ""));
            let slips = wisp_render::chrome::account::vocabulary_slips(&line);
            if slips.iter().any(|slip| banned.contains(slip)) {
                shown.push(format!("{path}: {line}"));
            }
        }
    }
    assert_eq!(shown, Vec::<String>::new());
}
