#![allow(
    dead_code,
    clippy::unwrap_used,
    reason = "each guard uses part of these helpers, which fail the test on a missing file"
)]

#[path = "rust_source.rs"]
pub(crate) mod rust_source;

use std::path::{Path, PathBuf};

use rust_source::{test_ranges, tokens};

pub(crate) const SRC: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src");

pub(crate) const TEST_MODULES: [&str; 2] = ["tests", "testing"];

pub(crate) fn sources(
    dir: &Path,
    extension: &str,
    test_modules: &[&str],
    at_least: usize,
) -> Vec<PathBuf> {
    fn walk(dir: &Path, extension: &str, test_modules: &[&str], out: &mut Vec<PathBuf>) {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            if path
                .file_stem()
                .is_some_and(|s| test_modules.iter().any(|m| s == *m))
            {
                continue;
            }
            if path.is_dir() {
                walk(&path, extension, test_modules, out);
            } else if path.extension().is_some_and(|e| e == extension) {
                out.push(path);
            }
        }
    }
    let mut files = vec![];
    walk(dir, extension, test_modules, &mut files);
    assert!(files.len() >= at_least, "found only {files:?}");
    files
}

pub(crate) fn relative(path: &Path) -> String {
    path.strip_prefix(SRC)
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/")
}

pub(crate) fn non_test_code(path: &Path) -> String {
    without_test_modules(&std::fs::read_to_string(path).unwrap())
}

pub(crate) fn without_test_modules(text: &str) -> String {
    let tokens = tokens(text);
    let mut out = String::with_capacity(text.len());
    let mut start = 0;
    for range in test_ranges(&tokens, true) {
        let from = tokens[range.start].span.start;
        let to = tokens[range.end - 1].span.end;
        out.push_str(&text[start..from]);
        start = to;
    }
    out.push_str(&text[start..]);
    out
}

pub(crate) fn collapse(code: &str) -> String {
    let joined = code.split_whitespace().collect::<Vec<_>>().join(" ");
    joined
        .replace("{ ", "{")
        .replace(" }", "}")
        .replace(" (", "(")
        .replace("( ", "(")
        .replace(":: ", "::")
        .replace(" ::", "::")
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

pub(crate) fn word_count(hay: &str, word: &str) -> usize {
    hay.match_indices(word)
        .filter(|(i, _)| {
            let before_ok =
                !word.starts_with(is_ident) || !hay[..*i].chars().next_back().is_some_and(is_ident);
            let after_ok = !word.ends_with(is_ident)
                || !hay[i + word.len()..].chars().next().is_some_and(is_ident);
            before_ok && after_ok
        })
        .count()
}

fn use_groups(code: &str) -> Vec<String> {
    let mut out = vec![];
    for (start, _) in code.match_indices("std::{") {
        let mut depth = 0;
        let mut group = String::new();
        for c in code[start + 5..].chars() {
            match c {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
            group.push(c);
            if depth == 0 {
                break;
            }
        }
        out.push(group);
    }
    out
}

pub(crate) fn impurities(code: &str, io_modules: &[&str], words: &[&str]) -> Vec<String> {
    let code = collapse(code);
    let mut found = vec![];
    for module in io_modules {
        if word_count(&code, &format!("std::{module}")) > 0
            || word_count(&code, &format!("{module}::")) > 0
            || use_groups(&code).iter().any(|g| word_count(g, module) > 0)
        {
            found.push(format!("std::{module}"));
        }
    }
    for word in words {
        if word_count(&code, word) > 0 {
            found.push((*word).to_owned());
        }
    }
    if code.contains(".elapsed(") {
        found.push(".elapsed(".to_owned());
    }
    found
}

pub(crate) fn assert_test_modules_are_files(test_modules: &[&str], files: &[PathBuf]) {
    for path in files {
        let text = std::fs::read_to_string(path).unwrap();
        let markers = text.matches("#[cfg(test)]").count();
        let modules: usize = test_modules
            .iter()
            .map(|m| text.matches(&format!("#[cfg(test)]\nmod {m};")).count())
            .sum();
        assert_eq!(markers, modules, "{}", relative(path));
    }
}
