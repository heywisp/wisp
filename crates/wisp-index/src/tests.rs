#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail directly on malformed fixtures or missing hits"
)]

use std::path::Path;

use serde_json::json;

use super::*;

fn entry(name: &str, kind: Kind, text: &str) -> Entry {
    Entry {
        kind,
        name: name.to_owned(),
        summary: first_line(text).to_owned(),
        text: format!("{name}\n{text}"),
    }
}

fn corpus() -> Vec<Entry> {
    let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("../wisp-core/tests/fixtures/tools");
    let mut paths: Vec<_> = std::fs::read_dir(folder)
        .unwrap()
        .map(|file| file.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .filter(|path| {
            !path
                .file_stem()
                .unwrap()
                .to_str()
                .unwrap()
                .contains("_without_")
        })
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let value: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            tool_entry(
                value["name"].as_str().unwrap(),
                value["description"].as_str().unwrap(),
                &value["input_schema"],
            )
        })
        .collect()
}

fn names(hits: &[Entry]) -> Vec<&str> {
    hits.iter().map(|entry| entry.name.as_str()).collect()
}

#[test]
fn names_are_indexed_whole_and_in_parts_with_case_and_plural_folding() {
    assert_eq!(
        words("mcp__github__create_issue createIssue HTTPServer WRITES s café"),
        [
            "mcp__github__create_issue",
            "mcp",
            "github",
            "create",
            "issue",
            "createissue",
            "create",
            "issue",
            "httpserver",
            "http",
            "server",
            "write",
            "s",
            "café"
        ]
    );
    assert!(words(" __ ... \n").is_empty());
    assert_eq!(first_line("\n  A line.  \r\nOther text"), "A line.");
    assert_eq!(first_line(" \n\r\n"), "");
}

#[test]
fn a_bare_name_finds_its_prefixed_tool() {
    let index = Index::new(vec![entry(
        "mcp__github__create_issue",
        Kind::Tool,
        "Open a ticket",
    )]);
    for query in [
        "create_issue",
        "createIssue",
        "CREATE ISSUES",
        "github",
        "mcp__github__create_issue",
    ] {
        assert_eq!(
            names(&index.search(query, None, None)),
            ["mcp__github__create_issue"],
            "{query}"
        );
    }
}

#[test]
fn queries_find_the_real_tool_definitions() {
    let entries = corpus();
    assert_eq!(entries.len(), 19);
    let cases = [
        ("secret password", "ask_secret"),
        ("multiple choice questions", "ask_user"),
        ("bash command stdout stderr", "bash"),
        ("subscription usage limits", "list_accounts"),
        ("reachable agents preset state", "list_agents"),
        ("theme compact settings", "list_settings"),
        ("turn a tool off", "manage"),
        ("sign in to a server", "manage"),
        ("set a server's key", "manage"),
        ("process stdin output", "process"),
        ("rename agent", "rename_agent"),
        ("rename this chat to Strada", "rename_chat"),
        ("change the session title", "rename_chat"),
        ("report task result", "report"),
        ("send message sibling", "send_message"),
        ("code copy syntax colours", "show_code"),
        ("diff changed words", "show_diff"),
        ("web page board html", "show_page"),
        ("start named child", "spawn_agent"),
        ("stop agent", "stop_agent"),
        ("attach file inspect", "view_file"),
        ("checklist steps progress", "write_todos"),
    ];
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<std::collections::BTreeSet<_>>(),
        cases
            .iter()
            .map(|(_, name)| *name)
            .collect::<std::collections::BTreeSet<_>>()
    );
    let index = Index::new(entries);
    for (query, expected) in cases {
        let hits = index.search(query, None, None);
        assert_eq!(
            hits.first().map(|entry| entry.name.as_str()),
            Some(expected),
            "{query}: {:?}",
            names(&hits)
        );
    }
}

#[test]
fn kinds_filter_the_results_and_round_trip_their_names() {
    let kinds = [Kind::Tool, Kind::Skill, Kind::Document];
    assert_eq!(kinds.map(Kind::name), ["tool", "skill", "document"]);
    let index = Index::new(
        kinds
            .iter()
            .map(|kind| entry(kind.name(), *kind, "shared"))
            .collect(),
    );
    for kind in kinds {
        assert_eq!(Kind::parse(kind.name()), Some(kind));
        let hits = index.search("shared", Some(kind), None);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, kind);
    }
    assert_eq!(index.search("shared", None, None).len(), 3);
    assert_eq!(Kind::parse("unknown"), None);
    assert_eq!(Kind::parse("Tool"), None);
}

#[test]
fn the_default_limit_is_five_and_hits_below_thirty_percent_are_cut() {
    let index = Index::new(
        ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"]
            .into_iter()
            .map(|name| entry(name, Kind::Tool, "needle"))
            .collect(),
    );
    assert_eq!(index.search("needle", None, None).len(), 5);
    assert_eq!(index.search("needle", None, Some(2)).len(), 2);
    assert_eq!(index.search("needle", None, Some(99)).len(), 6);
    assert!(index.search("needle", None, Some(0)).is_empty());
    let index = Index::new(vec![
        entry("best", Kind::Tool, &"needle ".repeat(20)),
        entry("near", Kind::Tool, "needle"),
        entry(
            "far",
            Kind::Tool,
            &format!("needle {}", "filler ".repeat(1000)),
        ),
    ]);
    let query = words("needle").into_iter().collect();
    let scores: BTreeMap<_, _> = index
        .entries
        .iter()
        .map(|(entry, terms)| (entry.name.as_str(), index.score(&query, terms)))
        .collect();
    assert!(scores["far"] < scores["best"] * 0.3);
    assert!(scores["near"] >= scores["best"] * 0.3);
    assert_eq!(names(&index.search("needle", None, None)), ["best", "near"]);
    assert_eq!(
        index.search_above("needle", None, None, 0.0),
        index.search("needle", None, None)
    );
    assert_eq!(
        names(&index.search_above("needle", None, None, scores["near"])),
        ["best"]
    );
    assert!(
        index
            .search_above("needle", None, None, f64::INFINITY)
            .is_empty()
    );
}

#[test]
fn names_count_double_and_equal_bm25_scores_tie_by_name() {
    let mut index = Index::new(vec![
        entry("zeta", Kind::Tool, "shared"),
        entry("alpha", Kind::Tool, "shared"),
    ]);
    index.entries.reverse();
    assert_eq!(
        names(&index.search("shared", None, None)),
        ["alpha", "zeta"]
    );
    for kinds in [[Kind::Tool, Kind::Skill], [Kind::Skill, Kind::Tool]] {
        let index = Index::new(
            kinds
                .into_iter()
                .map(|kind| entry("deploy", kind, "shared"))
                .collect(),
        );
        assert_eq!(
            index
                .search("shared", None, None)
                .iter()
                .map(|entry| entry.kind)
                .collect::<Vec<_>>(),
            [Kind::Tool, Kind::Skill]
        );
    }
    let index = Index::new(vec![
        entry("zebra", Kind::Tool, "alpha"),
        entry("alpha", Kind::Tool, "zebra"),
    ]);
    assert_eq!(
        names(&index.search("zebra", None, None)),
        ["zebra", "alpha"]
    );
    assert_eq!(index.entries[1].1["zebra"], 2);
    let index = Index::new(vec![
        entry("a", Kind::Tool, "needle"),
        entry("b", Kind::Tool, "needle filler filler"),
    ]);
    let query = words("needle").into_iter().collect();
    let expected = 1.2_f64.ln() * 2.2 / (1.0 + 1.2 * (0.25 + 0.75 * 3.0 / 4.0));
    assert!((index.score(&query, &index.entries[0].1) - expected).abs() < 1e-12);
}

#[test]
fn empty_unmatched_and_punctuation_only_queries_find_nothing() {
    let index = Index::new(corpus());
    for query in ["", " \n", "__...?!", "qzxvjk"] {
        assert!(index.search(query, None, None).is_empty(), "{query}");
    }
    assert!(
        Index::new(Vec::new())
            .search("needle", None, None)
            .is_empty()
    );
    assert!(
        Index::new(vec![entry("", Kind::Tool, "")])
            .search("needle", None, None)
            .is_empty()
    );
}

#[test]
fn tool_entries_search_argument_names_and_descriptions_at_every_depth() {
    let schema = json!({
        "type": "object",
        "properties": {
            "target_branch": {"type": "string", "description": "Destination reference"},
            "options": {
                "anyOf": [{"type": "object", "properties": {
                    "milestone": {"type": "string", "description": "Release checkpoint"}
                }}]
            },
            "items": {"type": "array", "items": {"type": "object", "properties": {
                "assignee": {"type": "string", "description": "Responsible colleague"}
            }}}
        }
    });
    let tool = tool_entry(
        "create_issue",
        "  Open a ticket. \nLonger description",
        &schema,
    );
    assert_eq!(tool.kind, Kind::Tool);
    assert_eq!(tool.summary, "Open a ticket.");
    assert!(tool.text.contains("create_issue"));
    assert!(tool.text.contains("Longer description"));
    let index = Index::new(vec![tool]);
    for query in [
        "target_branch",
        "destination",
        "milestone",
        "checkpoint",
        "assignee",
        "colleague",
    ] {
        assert_eq!(
            names(&index.search(query, None, None)),
            ["create_issue"],
            "{query}"
        );
    }
    let index = Index::new(corpus());
    assert_eq!(index.search("keep_running", None, None)[0].name, "bash");
    assert_eq!(tool_entry("empty", "", &Value::Null).summary, "");
}
