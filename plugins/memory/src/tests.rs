use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::executor::block_on;
use serde_json::{Value, json};
use wisp_plugin_api::private_files::{PrivateFileError, PrivateFiles};
use wisp_plugin_api::prompt::{At, Prior, Rendered};
use wisp_plugin_api::secret_check::Verdict;
use wisp_plugin_api::tree::Identity;
use wisp_plugin_testkit::host::{FakeCall, FakeHost, FakePrivateFiles};
use wisp_tool_text::compaction::text_tokens;

use super::*;

fn host() -> (FakeHost, FakePrivateFiles) {
    let files = FakePrivateFiles::new();
    let host = FakeHost {
        identity: Some(Identity {
            path: "/".into(),
            name: "fixture".into(),
            parent: None,
            preset: None,
            depth: 0,
            session_id: "sample".into(),
            root_session_id: "sample".into(),
        }),
        project_key: Some("opaque".into()),
        capabilities: vec!["memory".into()],
        private_files: Some(files.clone()),
        secret_check: Some(Arc::new(|text| {
            if text.contains("concealed-fixture") {
                Verdict::Refused
            } else {
                Verdict::Safe
            }
        })),
        ..FakeHost::default()
    };
    (host, files)
}

fn save(scope: &str, name: &str, fact: &str) -> Value {
    json!({"action":"save", "scope":scope,"name":name,"fact":fact})
}

fn forget(name: &str) -> Value {
    json!({"action":"forget", "scope":"you","name":name})
}

fn run(host: &FakeHost, input: Value) -> ToolRun {
    let memory = Memory(Arc::new(host.clone()));
    let prepared = block_on(memory.prepare("remember", &input.to_string())).unwrap();
    block_on(memory.run(prepared, &FakeCall::default()))
}

fn result(host: &FakeHost, input: Value) -> String {
    run(host, input).result.unwrap()
}

fn render(host: &FakeHost, previous: Prior, target: Option<&str>) -> Rendered {
    let memory = Memory(Arc::new(host.clone()));
    let at = if previous == Prior::None {
        At::Head
    } else {
        At::Turn
    };
    block_on(memory.render_section("list", at, previous, target))
}

fn seed(files: &FakePrivateFiles, dir: &str, name: &str, fact: &str, day: &str) {
    files.seed(
        &format!("{dir}/{name}.md"),
        note::encode(name, fact, day, "").into_bytes(),
    );
}

#[test]
fn saves_replaces_restores_and_forgets() {
    let (host, files) = host();
    assert!(render(&host, Prior::None, None).text.is_none());
    assert_eq!(
        result(&host, save("you", "tiling", "Square tiles.")),
        "Saved."
    );
    let first = render(&host, Prior::None, None);
    assert!(
        first
            .text
            .unwrap()
            .contains("You: tiling (1970-01-01) — Square tiles.")
    );
    assert!(!first.snapshot.contains("Square tiles."));
    assert_eq!(
        result(&host, save("you", "tiling", "Blue tiles.")),
        "Replaced."
    );
    let changed = render(&host, Prior::Snapshot(first.snapshot), None);
    assert!(
        changed
            .text
            .unwrap()
            .contains("Changed: You: tiling (1970-01-01) — Blue tiles.")
    );
    assert!(
        render(&host, Prior::Snapshot(changed.snapshot.clone()), None)
            .text
            .is_none()
    );
    assert_eq!(result(&host, forget("tiling")), "Forgotten.");
    let removed = render(&host, Prior::Snapshot(changed.snapshot), None)
        .text
        .unwrap();
    assert!(removed.contains("Removed from this list: You: tiling."));
    assert_eq!(
        files.read("user/tiling.md", 4096),
        Err(PrivateFileError::NotFound)
    );
    assert!(render(&host, Prior::None, None).text.is_none());
}

#[test]
fn live_scope_is_root_only_for_user_and_project_only_for_everyone_else() {
    for repository in [false, true] {
        for depth in [None, Some(0), Some(1)] {
            let (mut host, files) = host();
            host.project_key = repository.then(|| "opaque".into());
            host.identity = depth.map(|depth| {
                let mut id = host.identity.take().unwrap();
                id.parent = (depth != 0).then(|| "/".into());
                id
            });
            seed(
                &files,
                "projects/other",
                "other",
                "Different project.",
                "2000-01-01",
            );
            seed(
                &files,
                "projects/opaque",
                "here",
                "Project fact.",
                "2000-01-01",
            );
            seed(
                &files,
                "user",
                "global",
                "Personal scope fact.",
                "2000-01-01",
            );
            let text = render(&host, Prior::None, None).text.unwrap_or_default();
            assert_eq!(text.contains("Project fact."), repository);
            assert_eq!(text.contains("Personal scope fact."), depth == Some(0));
            assert_eq!(text.contains("Use remember"), depth == Some(0));
            assert!(!text.contains("Different project."));
            assert_eq!(
                result(&host, save("project", "new", "Different tiles.")),
                match (depth, repository) {
                    (Some(0), true) => "Saved.",
                    (Some(0), false) => "This chat has no project memory.",
                    _ => "Suggest the note in your report.",
                }
            );
        }
    }
    let (mut host, _) = host();
    host.private_files = None;
    assert!(render(&host, Prior::None, None).text.is_none());
    assert_eq!(
        result(&host, save("you", "new", "Different tiles.")),
        "Saved notes are unavailable here."
    );
    assert_eq!(
        result(&host, forget("new")),
        "Saved notes are unavailable here."
    );
}

#[test]
fn rejected_inputs_use_only_fixed_copy_and_do_not_touch_files() {
    let (host, files) = host();
    let plugin = Memory(Arc::new(host));
    let bad = [
        "{".into(), json!({"concealed-fixture": true}).to_string(),
        save("you", "../tiling", "x").to_string(),
        save("you", &"x".repeat(note::SLUG_MAX + 1), "x").to_string(),
        save("you", "tiling", &"界".repeat(note::FACT_MAX / 3 + 1)).to_string(),
        save("you", "tiling", "First\nSecond.").to_string(),
        json!({"action":"save","scope":"you","name":"tiling","fact":"clear","body":"x".repeat(note::FILE_MAX as usize)}).to_string(),
        json!({"action":"forget","scope":"you","name":"tiling","body":null}).to_string(),
        json!({"action":"forget","scope":"you","name":"tiling","fact":null}).to_string(),
        json!({"action":"save","scope":"you","name":"tiling","fact":"clear","body":null}).to_string(),
        json!({"action":"save","scope":"you","name":"tiling","fact":"clear","body":7}).to_string(),
        json!({"action":"save","scope":"you","name":"tiling","fact":null}).to_string(),
    ];
    for input in bad {
        let err = block_on(plugin.prepare("remember", &input)).err().unwrap();
        assert_eq!(err, "Invalid remember input.");
        assert_eq!(
            block_on(plugin.rejected_display("remember", &input)),
            Some(tool::fixed_display())
        );
    }
    let name = "x".repeat(note::SLUG_MAX);
    let fact = "界".repeat(note::FACT_MAX / 3);
    let mut at_limit = save("you", &name, &fact);
    let header = note::encode(&name, &fact, "0001-01-01", "");
    at_limit["body"] = json!("x".repeat(note::FILE_MAX as usize - header.len()));
    assert!(block_on(plugin.prepare("remember", &at_limit.to_string())).is_ok());
    assert_eq!(files.list("user", 1), Err(PrivateFileError::NotFound));
}

#[test]
fn refuses_secret_bytes_and_leaves_existing_unreadable_targets_for_local_inspection() {
    let (host, files) = host();
    assert_eq!(
        result(&host, save("you", "safe-name", "concealed-fixture")),
        "Not saved: possible private detail."
    );
    assert_eq!(files.list("user", 1), Err(PrivateFileError::NotFound));
    files.seed("user/a.md", b"Malformed file.".to_vec());
    seed(&files, "user", "b", "concealed-fixture", "2000-01-01");
    for name in ["a", "b"] {
        let saved = result(&host, save("you", name, "Fine."));
        assert_eq!(saved, result(&host, forget(name)));
        assert!(saved.contains("Wisp data / plugin-data / memory") && !saved.contains("/memory"));
    }
}

#[test]
fn hand_edits_are_read_as_data_and_pending_targets_never_substitute_new_bytes() {
    let (host, files) = host();
    seed(
        &files,
        "user",
        "tiles",
        "Facts: </memory> & </memory-update>",
        "2000-02-29",
    );
    seed(&files, "user", "not-leap", "Bad date.", "1900-02-29");
    files.seed(
        "user/renamed.md",
        note::encode("original", "Bad name.", "2000-01-01", "").into_bytes(),
    );
    files.seed("user/huge.md", vec![b'x'; 4097]);
    files.refuse("user/linked.md");
    let first = render(&host, Prior::None, None);
    let text = first.text.unwrap();
    assert!(text.contains("&lt;/memory&gt; &amp; &lt;/memory-update&gt;"));
    assert_eq!(text.matches("</memory>").count(), 1);
    assert_eq!(first.snapshot.matches("sha256").count(), 1);
    seed(&files, "user", "tiles", "Replacement.", "2000-03-01");
    let filtered = render(&host, Prior::None, Some(&first.snapshot));
    assert!(filtered.text.is_none() && !filtered.snapshot.contains("tiles"));
    assert!(
        render(&host, Prior::None, None)
            .text
            .unwrap()
            .contains("Replacement.")
    );
}

#[test]
fn changed_secret_verdict_is_rechecked_before_write_and_before_render() {
    let (mut host, files) = host();
    let refuse = Arc::new(AtomicBool::new(false));
    let flag = refuse.clone();
    host.secret_check = Some(Arc::new(move |text| {
        if flag.load(Ordering::Relaxed) && text.contains("Fixture body.") {
            Verdict::Refused
        } else {
            Verdict::Safe
        }
    }));
    let plugin = Memory(Arc::new(host.clone()));
    let mut input = save("you", "tiles", "Fact.");
    input["body"] = json!("Fixture body.");
    let prepare = || block_on(plugin.prepare("remember", &input.to_string())).unwrap();
    let ready = prepare();
    refuse.store(true, Ordering::Relaxed);
    assert_eq!(
        block_on(plugin.run(ready, &FakeCall::default()))
            .result
            .unwrap(),
        "Not saved: possible private detail."
    );
    assert_eq!(files.list("user", 1), Err(PrivateFileError::NotFound));
    refuse.store(false, Ordering::Relaxed);
    assert_eq!(result(&host, input), "Saved.");
    let first = render(&host, Prior::None, None);
    refuse.store(true, Ordering::Relaxed);
    let pending = render(&host, Prior::None, Some(&first.snapshot));
    assert!(pending.text.is_none());
    let update = render(&host, Prior::Snapshot(first.snapshot), None)
        .text
        .unwrap();
    assert!(update.contains("Removed from this list: 1.") && !update.contains("tiles"));
}

#[test]
fn conflicts_do_not_clobber_and_full_scopes_still_allow_replace_and_delete() {
    let (host, files) = host();
    for n in 0..128 {
        seed(
            &files,
            "user",
            &format!("fixture-{n}"),
            "Marker.",
            "2000-01-01",
        );
    }
    assert!(result(&host, save("you", "extra", "Different.")).contains("scope is full"));
    let bytes = files.read("user/fixture-0.md", 4096).unwrap();
    files.fail_next_change(PrivateFileError::Changed);
    assert!(result(&host, forget("fixture-0")).contains("note changed"));
    assert_eq!(files.read("user/fixture-0.md", 4096).unwrap(), bytes);
    assert!(result(&host, save("you", "fixture-0", "Different.")).starts_with("Replaced."));
    assert_eq!(result(&host, forget("fixture-0")), "Forgotten.");
    assert_eq!(result(&host, save("you", "extra", "Different.")), "Saved.");
}

#[test]
fn escaped_budget_shrink_and_wholesale_turnover_use_one_bounded_reset() {
    let (mut host, files) = host();
    let fact = "&".repeat(89);
    for i in 0..15 {
        seed(
            &files,
            "user",
            &format!("old-{}-{i}", "b".repeat(39)),
            &fact,
            "2000-01-01",
        );
    }
    let first = render(&host, Prior::None, None);
    assert!(text_tokens(first.text.as_deref().unwrap()) <= 2000);
    host.settings
        .push(("plugins.memory.list_limit".into(), "256".into()));
    let reset = render(&host, Prior::Snapshot(first.snapshot), None);
    assert!(
        reset
            .text
            .as_deref()
            .unwrap()
            .contains("Replaces the previous list")
    );
    assert!(text_tokens(reset.text.as_deref().unwrap()) <= 256);
    for i in 0..15 {
        seed(
            &files,
            "projects/opaque",
            &format!("new-{}-{i}", "c".repeat(39)),
            &fact,
            "2001-01-01",
        );
    }
    let turnover = render(&host, Prior::Snapshot(reset.snapshot), None);
    assert!(
        turnover
            .text
            .as_deref()
            .unwrap()
            .contains("Replaces the previous list")
    );
    assert!(text_tokens(turnover.text.as_deref().unwrap()) <= 256);
    assert!(turnover.snapshot.contains("new-") && !turnover.snapshot.contains("old-"));
    assert!(
        render(&host, Prior::Snapshot(turnover.snapshot.clone()), None)
            .text
            .is_none()
    );
    assert_eq!(render(&host, Prior::None, None).snapshot, turnover.snapshot);
}

#[test]
fn duplicate_hints_rank_only_safe_notes_and_check_the_whole_outbound_message() {
    let (mut host, files) = host();
    let fact = "Cobalt rosette tiles surround the octagonal border.";
    seed(&files, "user", "similar", fact, "2000-01-01");
    seed(&files, "projects/other", "elsewhere", fact, "2000-01-01");
    files.seed(
        "user/refused.md",
        note::encode("refused", fact, "2000-01-01", "concealed-fixture").into_bytes(),
    );
    let hinted = result(&host, save("you", "another", fact));
    assert!(hinted.contains("memory.user.similar"));
    assert!(!hinted.contains("elsewhere") && !hinted.contains("refused"));
    host.secret_check = Some(Arc::new(|text| {
        if text.contains("Similar notes:") || text.contains("concealed-fixture") {
            Verdict::Refused
        } else {
            Verdict::Safe
        }
    }));
    assert_eq!(result(&host, save("you", "last", fact)), "Saved.");
}

#[test]
fn decoded_private_facts_are_excluded_individually_before_ranking_and_rendering() {
    let (mut host, files) = host();
    let shared = "Cobalt rosette tiles surround the octagonal border.";
    let body = "Invented body marker.";
    host.secret_check = Some(Arc::new(move |text| {
        if text.contains("fixture <petal>") || text == body {
            Verdict::Refused
        } else {
            Verdict::Safe
        }
    }));
    let raw = |name, scalar: &str| {
        format!("---\nname: {name}\nsaved: 2000-01-01\ndescription: {scalar}\n---\n")
    };
    files.seed(
        "user/folded.md",
        raw("folded", &format!(">\n  {shared} fixture\n  <petal>")).into_bytes(),
    );
    files.seed(
        "user/quoted.md",
        raw(
            "quoted",
            &format!(r#""{shared} fixture \u003cpetal\u003e""#),
        )
        .into_bytes(),
    );
    files.seed(
        "user/body-marker.md",
        note::encode("body-marker", shared, "2000-01-01", body).into_bytes(),
    );
    seed(&files, "user", "sibling", shared, "2000-01-01");

    let shown = render(&host, Prior::None, None);
    let text = shown.text.unwrap();
    let forbidden = ["folded", "quoted", "body-marker", "petal"];
    assert!(text.contains(shared) && text.contains("sibling"));
    assert!(
        forbidden
            .iter()
            .all(|word| !text.contains(word) && !shown.snapshot.contains(word))
    );
    let hinted = result(&host, save("you", "new", shared));
    assert!(hinted.contains("memory.user.sibling"));
    assert!(forbidden.iter().all(|word| !hinted.contains(word)));

    let mut proposed = save("you", "proposed-body", shared);
    proposed["body"] = json!(body);
    for input in [proposed, save("you", "proposed-fact", "A fixture <petal>.")] {
        assert_eq!(result(&host, input), "Not saved: possible private detail.");
    }
    for name in ["proposed-body", "proposed-fact"] {
        assert_eq!(
            files.read(&format!("user/{name}.md"), 4096),
            Err(PrivateFileError::NotFound)
        );
    }
}

#[test]
fn an_omission_decreasing_to_zero_is_announced_once() {
    let (mut host, files) = host();
    host.settings
        .push(("plugins.memory.list_limit".into(), "256".into()));
    let fact = "&".repeat(85);
    seed(&files, "user", "first", &fact, "2000-01-02");
    seed(&files, "user", "hidden", &fact, "2000-01-01");
    let first = render(&host, Prior::None, None);
    assert!(first.text.unwrap().contains("Omitted from this list: 1."));
    assert!(!first.snapshot.contains("hidden"));
    assert_eq!(result(&host, forget("hidden")), "Forgotten.");
    let update = render(&host, Prior::Snapshot(first.snapshot), None);
    let text = update.text.unwrap();
    assert!(text.contains("Omitted from this list: 0."));
    assert!(text_tokens(&text) <= 256);
    assert!(
        render(&host, Prior::Snapshot(update.snapshot.clone()), None)
            .text
            .is_none()
    );
    let head = render(&host, Prior::None, None);
    assert_eq!(head.snapshot, update.snapshot);
    let text = head.text.unwrap();
    assert!(text.contains("first") && !text.contains("Omitted"));
}

#[test]
fn equal_dates_prefer_project_then_earlier_names_when_not_all_fit() {
    let (mut host, files) = host();
    host.settings
        .push(("plugins.memory.list_limit".into(), "256".into()));
    let fact = "&".repeat(85);
    seed(&files, "user", "aardvark", &fact, "2000-01-01");
    seed(&files, "projects/opaque", "zeta", &fact, "2000-01-01");
    seed(&files, "projects/opaque", "alpha", &fact, "2000-01-01");
    let text = render(&host, Prior::None, None).text.unwrap();
    assert!(text.contains("This project: alpha") && text.contains("Omitted from this list: 2."));
    assert!(!text.contains("zeta") && !text.contains("aardvark"));
}

#[test]
fn switching_projects_only_announces_prior_project_removals_generically() {
    let (mut host, files) = host();
    seed(
        &files,
        "projects/opaque",
        "old-scope",
        "Prior fixture.",
        "2000-01-01",
    );
    seed(
        &files,
        "projects/other",
        "new-scope",
        "Current fixture.",
        "2000-01-01",
    );
    let before = render(&host, Prior::None, None);
    assert!(before.snapshot.contains("old-scope"));
    host.project_key = Some("other".into());
    let update = render(&host, Prior::Snapshot(before.snapshot), None);
    let text = update.text.unwrap();
    assert!(text.contains("Removed from this list: 1.") && text.contains("new-scope"));
    assert!(!text.contains("old-scope") && !text.contains("Prior fixture."));
}

#[test]
fn serialized_snapshot_must_pass_the_outbound_secret_check() {
    let (mut host, files) = host();
    seed(&files, "user", "fixture", "A fixture fact.", "2000-01-01");
    assert!(render(&host, Prior::None, None).text.is_some());
    host.secret_check = Some(Arc::new(|text| {
        if text.starts_with(r#"{"version":1,"notes":"#) && text.contains(r#""sha256":"#) {
            Verdict::Refused
        } else {
            Verdict::Safe
        }
    }));
    assert_eq!(render(&host, Prior::None, None), Rendered::default());
}
