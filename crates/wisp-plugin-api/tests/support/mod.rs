#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::panic,
    reason = "each test binary uses part of the shared helpers, and they fail on an unexpected parse"
)]

use wisp_plugin_api::manifest::{Manifest, Origin, Problem};

pub(crate) fn problem(text: &str, origin: Origin) -> Problem {
    match Manifest::parse(text, origin) {
        Ok(manifest) => panic!("accepted: {manifest:?}"),
        Err(problem) => problem,
    }
}

pub(crate) fn place(text: &str, needle: &str) -> (Option<usize>, Option<usize>) {
    let offset = text
        .find(needle)
        .unwrap_or_else(|| panic!("{needle} not in {text}"));
    position(text, offset)
}

pub(crate) fn position(text: &str, offset: usize) -> (Option<usize>, Option<usize>) {
    let before = &text[..offset];
    let line = before.matches('\n').count() + 1;
    let start = before.rfind('\n').map_or(0, |i| i + 1);
    (Some(line), Some(before[start..].chars().count() + 1))
}

pub(crate) fn refused_at(text: &str, origin: Origin, needle: &str, message: &str) {
    let problem = problem(text, origin);
    assert_eq!(problem.message, message, "{text}");
    assert_eq!(
        (problem.line, problem.column),
        place(text, needle),
        "{needle}: {problem}"
    );
}
