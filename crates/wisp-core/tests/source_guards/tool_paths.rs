use std::path::{Path, PathBuf};

use regex::Regex;

use crate::purity::{SRC, sources};

fn scanned() -> Vec<PathBuf> {
    let src = Path::new(SRC);
    let mut files = vec![
        src.join("jobs.rs"),
        src.join("call.rs"),
        src.join("agent/processes.rs"),
    ];
    files.extend(sources(&src.join("jobs"), "rs", &[], 1));
    files.extend(sources(&src.join("call"), "rs", &[], 1));
    files
}

fn tool_paths(source: &str) -> Vec<String> {
    let direct = Regex::new(r"\b(crate|super)(\s*::\s*super)*\s*::\s*tool\b").unwrap();
    let grouped = Regex::new(r"\b(crate|super)(\s*::\s*super)*\s*::\s*\{[^}]*\btool\b").unwrap();
    direct
        .find_iter(source)
        .chain(grouped.find_iter(source))
        .map(|m| m.as_str().to_owned())
        .collect()
}

#[test]
fn the_scan_finds_tool_paths_however_they_are_written() {
    for found in [
        "use crate::tool::cut_chars;",
        "use crate::{agent, tool::Toolbox};",
        "let x = crate :: tool :: Toolbox::default();",
        "use super::super::tool::process;",
    ] {
        assert!(!tool_paths(found).is_empty(), "{found}");
    }
    for clean in [
        "use wisp_tool_text::output::count;",
        "fn tool_uses() {}",
        "use crate::jobs::Processes;",
    ] {
        assert!(tool_paths(clean).is_empty(), "{clean}");
    }
}

#[test]
fn the_jobs_service_the_call_service_and_the_process_notice_never_reach_the_tools() {
    let files = scanned();
    assert!(files.len() >= 20, "{files:?}");
    let mut failures = Vec::new();
    for file in files {
        let source = std::fs::read_to_string(&file).unwrap();
        for found in tool_paths(&source) {
            failures.push(format!("{}: {found}", file.display()));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
