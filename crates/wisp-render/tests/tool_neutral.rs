#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test on a missing file"
)]
#[path = "../../../tests/support/purity.rs"]
mod purity;

use std::path::Path;

use purity::sources;

const CRATES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/..");

const NAMES: [&str; 6] = [
    "bash",
    "ask_user",
    "web_search",
    "view_file",
    "show_diff",
    "show_code",
];

const LITERAL_NAMES: [&str; 2] = ["process", "bash"];

const PROTOCOL_FIELDS: [&str; 1] = ["web_search_requests"];

const SETTING_KEYS: [&str; 1] = ["native_web_search"];

const ALLOWED: [(&str, &str, &str); 7] = [
    (
        "wisp-render/src/highlight.rs",
        "bash",
        "code fences name the bash grammar; this is not the bash tool",
    ),
    (
        "wisp-render/src/highlight/tests.rs",
        "bash",
        "tests the bash grammar of code fences",
    ),
    (
        "wisp-render/src/transcript.rs",
        "web_search",
        "the raw provider block types needed to fold opaque blocks into the search row, until the kernel maps them to a neutral kind",
    ),
    (
        "wisp-tui/src/app/config_screen/rows.rs",
        "web_search",
        "names the web_search setting among those no page shows; a setting's name, not the tool",
    ),
    (
        "wisp-tui/src/app/config_screen/entries.rs",
        "web_search",
        "names the web_search setting whose saved empty list stops search on a model; a setting's name, not the tool",
    ),
    (
        "wisp-tui/src/app/config_screen/tests/edit/search.rs",
        "web_search",
        "builds the catalog's search tool and the saved web_search lists that the Web search row reads",
    ),
    (
        "wisp-render/src/transcript/search/tests.rs",
        "web_search",
        "feeds the raw provider block types that transcript.rs folds into the search row",
    ),
];

fn allowed(file: &str, tool: &str) -> bool {
    ALLOWED
        .iter()
        .any(|(path, named, _)| *path == file && *named == tool)
}

fn named_tools(file: &str, source: &str) -> Vec<String> {
    let mut code = source.to_ascii_lowercase();
    for field in PROTOCOL_FIELDS.into_iter().chain(SETTING_KEYS) {
        code = code.replace(field, "");
    }
    let mut found = vec![];
    for tool in NAMES {
        if !allowed(file, tool) && code.contains(tool) {
            found.push(format!("{file}: {tool}"));
        }
    }
    for tool in LITERAL_NAMES {
        let literal = format!("\"{tool}\"");
        if !allowed(file, tool) && code.contains(&literal) {
            found.push(format!("{file}: {literal}"));
        }
    }
    found
}

#[test]
fn a_tool_name_is_refused_only_as_a_whole_string_literal() {
    let file = "wisp-tui/src/app/rows.rs";
    let fine = "use std::process::Command;\n\
        let event = AgentEvent::ProcessOutput { id: 3, text, skipped: 0 };\n\
        let fact = \"process 3\";\n\
        std::process::exit(1);";
    assert_eq!(named_tools(file, fine), Vec::<String>::new());
    assert_eq!(
        named_tools(file, "if tool == \"process\" { return; }"),
        [format!("{file}: \"process\"")]
    );
    assert_eq!(
        named_tools(file, "match name { \"Process\" => 1, _ => 0 }"),
        [format!("{file}: \"process\"")]
    );
}

#[test]
fn the_client_crates_never_name_a_tool() {
    let root = Path::new(CRATES);
    let mut files = vec![];
    for krate in ["wisp-render", "wisp-tui"] {
        files.extend(sources(&root.join(krate).join("src"), "rs", &[], 1));
    }
    assert!(files.len() > 20, "{files:?}");
    let mut found = vec![];
    for path in files {
        let name = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let code = std::fs::read_to_string(&path).unwrap();
        found.extend(named_tools(&name, &code));
    }
    assert!(found.is_empty(), "{found:?}");
}
