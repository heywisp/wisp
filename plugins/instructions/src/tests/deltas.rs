use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use wisp_instructions::delta::{NOTHING_APPLIES, OPENING, UNREADABLE};
use wisp_instructions::sha256_hex;
use wisp_plugin_api::host::Instant;
use wisp_plugin_api::instructions::Change;

use super::*;

const T0: i64 = 1_800_000_000;

fn render(plugin: &Arc<dyn Plugin>, at: At, previous: &Prior, target: Option<&str>) -> Rendered {
    block_on(plugin.render_section(SECTION, at, previous.clone(), target))
}

fn turn(plugin: &Arc<dyn Plugin>, previous: &Prior) -> Rendered {
    render(plugin, At::Turn, previous, None)
}

fn seen(rendered: &Rendered) -> Prior {
    Prior::Snapshot(rendered.snapshot.clone())
}

fn update(body: &str) -> String {
    format!("<instructions-update>\n{OPENING}\n{body}</instructions-update>\n")
}

fn file(scope: &str, name: &str, path: &str, text: &str) -> String {
    format!("<file scope=\"{scope}\" name=\"{name}\" path=\"{path}\">\n{text}</file>\n")
}

fn changes(host: &FakeHost) -> Vec<(Change, String)> {
    reports(host)
        .into_iter()
        .filter_map(|r| match r {
            Report::Changed(change, text) => Some((*change, text)),
            _ => None,
        })
        .collect()
}

struct Plugged {
    host: FakeHost,
    plugin: Arc<dyn Plugin>,
    start: Rendered,
}

fn plugged(t: &Tree, settings: &[(&str, &str)]) -> Plugged {
    let host = t.host("proj", settings);
    let plugin = instantiate(Arc::new(host.clone()));
    let start = render(&plugin, At::Start, &Prior::None, None);
    Plugged {
        host,
        plugin,
        start,
    }
}

#[test]
fn an_unchanged_file_and_an_edit_to_the_unused_default_send_nothing() {
    let t = Tree::new();
    t.write("home/proj/AGENTS.md", "Answer briefly.\n");
    t.write("home/.config/wisp/AGENTS.md", "Default.\n");
    let p = plugged(&t, &[]);
    let base = seen(&p.start);
    assert_eq!(
        turn(&p.plugin, &base),
        Rendered {
            snapshot: p.start.snapshot.clone(),
            text: None
        }
    );
    t.write("home/.config/wisp/AGENTS.md", "Default, edited.\n");
    assert_eq!(turn(&p.plugin, &base).text, None);
    assert!(changes(&p.host).is_empty());
}

#[test]
fn an_edit_sends_the_whole_new_text_and_reports_the_change_with_it() {
    let t = Tree::new();
    t.write("home/proj/AGENTS.md", "Answer briefly.\n");
    let p = plugged(&t, &[]);
    t.write("home/proj/AGENTS.md", "Answer at length.\n");
    let edited = turn(&p.plugin, &seen(&p.start));
    let text = update(&file(
        "project",
        "AGENTS.md",
        "~/proj/AGENTS.md",
        "Answer at length.\n",
    ));
    assert_eq!(edited.text.as_deref(), Some(text.as_str()));
    assert_eq!(
        edited.snapshot,
        format!("{} project AGENTS.md\n", sha256_hex("Answer at length.\n"))
    );
    let [(change, sent)] = changes(&p.host).try_into().unwrap();
    assert_eq!(sent, text);
    assert_eq!(change.changed.len(), 1);
    assert_eq!(
        change.changed[0].old_sha256,
        Some(sha256_hex("Answer briefly.\n"))
    );
    assert_eq!(change.from.unwrap().sha256, sha256_hex("Answer briefly.\n"));
    assert_eq!(change.to.unwrap().sha256, sha256_hex("Answer at length.\n"));
    assert_eq!(turn(&p.plugin, &seen(&edited)).text, None);
    assert_eq!(
        turn(&p.plugin, &seen(&p.start)).text.as_deref(),
        Some(text.as_str()),
        "the base moves only with the committed snapshot"
    );
}

#[test]
fn a_project_file_created_replaces_the_default_and_deleted_brings_it_back() {
    let t = Tree::new();
    t.write("home/.config/wisp/AGENTS.md", "Default.\n");
    let p = plugged(&t, &[]);
    t.write("home/proj/AGENTS.md", "Project.\n");
    let created = turn(&p.plugin, &seen(&p.start));
    assert_eq!(
        created.text.unwrap(),
        update(&format!(
            "{}<removed scope=\"default\" path=\"~/.config/wisp/AGENTS.md\"/>\n",
            file("project", "AGENTS.md", "~/proj/AGENTS.md", "Project.\n")
        ))
    );
    std::fs::remove_file(t.home().join("proj/AGENTS.md")).unwrap();
    let deleted = turn(&p.plugin, &Prior::Snapshot(created.snapshot));
    assert_eq!(
        deleted.text.unwrap(),
        update(&format!(
            "{}<removed scope=\"project\" path=\"~/proj/AGENTS.md\"/>\n",
            file(
                "default",
                "AGENTS.md",
                "~/.config/wisp/AGENTS.md",
                "Default.\n"
            )
        ))
    );
    assert_eq!(deleted.snapshot, p.start.snapshot);
}

#[test]
fn an_import_changed_added_and_removed_sends_only_that_import() {
    let t = Tree::new();
    t.write("home/proj/AGENTS.md", "Rules.\n@a.md\n");
    t.write("home/proj/a.md", "A.\n");
    t.write("home/proj/b.md", "B.\n");
    let p = plugged(&t, &[]);
    t.write("home/proj/a.md", "A, edited.\n");
    let changed = turn(&p.plugin, &seen(&p.start));
    assert_eq!(
        changed.text.unwrap(),
        update(&file("project", "a.md", "~/proj/a.md", "A, edited.\n"))
    );
    let base = Prior::Snapshot(changed.snapshot);
    t.write("home/proj/AGENTS.md", "Rules.\n@a.md\n@b.md\n");
    let added = turn(&p.plugin, &base);
    assert_eq!(
        added.text.unwrap(),
        update(&format!(
            "{}{}",
            file(
                "project",
                "AGENTS.md",
                "~/proj/AGENTS.md",
                "Rules.\n@a.md\n@b.md\n"
            ),
            file("project", "b.md", "~/proj/b.md", "B.\n")
        ))
    );
    t.write("home/proj/AGENTS.md", "Rules.\n@b.md\n");
    let removed = turn(&p.plugin, &Prior::Snapshot(added.snapshot));
    assert_eq!(
        removed.text.unwrap(),
        update(&format!(
            "{}<removed scope=\"project\" path=\"~/proj/a.md\"/>\n",
            file(
                "project",
                "AGENTS.md",
                "~/proj/AGENTS.md",
                "Rules.\n@b.md\n"
            )
        ))
    );
}

#[test]
fn an_unreadable_file_is_removed_with_its_line_and_comes_back_whole_once_fixed() {
    let t = Tree::new();
    t.write("home/proj/AGENTS.md", "Rules.\n@a.md\n");
    t.write("home/proj/a.md", "A.\n");
    t.write("home/.config/wisp/AGENTS.md", "Default.\n");
    let p = plugged(&t, &[]);
    std::fs::write(t.home().join("proj/a.md"), [0xff, 0xfe, b'\n']).unwrap();
    let broken = turn(&p.plugin, &seen(&p.start));
    assert_eq!(
        broken.text.unwrap(),
        update(&format!(
            "<removed scope=\"project\" path=\"~/proj/a.md\"/>\n{UNREADABLE}\n"
        ))
    );
    let [(change, _)] = changes(&p.host).try_into().unwrap();
    assert!(
        change.summary.diagnostics.iter().any(|d| matches!(
            d,
            Diagnostic::Problem(Problem {
                kind: ProblemKind::NotUtf8,
                ..
            })
        )),
        "{change:?}"
    );
    std::fs::write(t.home().join("proj/AGENTS.md"), [0xff, b'\n']).unwrap();
    let chosen = turn(&p.plugin, &seen(&p.start));
    assert_eq!(
        chosen.text.unwrap(),
        update(&format!(
            "<removed scope=\"project\" path=\"~/proj/AGENTS.md\"/>\n{UNREADABLE}\n<removed scope=\"project\" path=\"~/proj/a.md\"/>\n{NOTHING_APPLIES}\n"
        )),
        "the default stays unused"
    );
    t.write("home/proj/AGENTS.md", "Rules.\n@a.md\n");
    t.write("home/proj/a.md", "A, fixed.\n");
    let fixed = turn(&p.plugin, &Prior::Snapshot(broken.snapshot));
    assert_eq!(
        fixed.text.unwrap(),
        update(&file("project", "a.md", "~/proj/a.md", "A, fixed.\n"))
    );
}

#[test]
fn none_removes_everything_and_auto_again_sends_the_whole_set() {
    let t = Tree::new();
    t.write("home/proj/AGENTS.md", "Rules.\n@a.md\n");
    t.write("home/proj/a.md", "A.\n");
    let p = plugged(&t, &[]);
    let none = plugged(&t, &[(SOURCE, "none")]);
    assert_eq!(none.start, Rendered::default());
    let off = turn(&none.plugin, &seen(&p.start));
    assert_eq!(
        off.text.unwrap(),
        update(&format!(
            "<removed scope=\"project\" path=\"~/proj/AGENTS.md\"/>\n<removed scope=\"project\" path=\"~/proj/a.md\"/>\n{NOTHING_APPLIES}\n"
        ))
    );
    assert_eq!(off.snapshot, "");
    let back = turn(&p.plugin, &Prior::Snapshot(off.snapshot));
    assert_eq!(
        back.text.unwrap(),
        update(&format!(
            "{}{}",
            file(
                "project",
                "AGENTS.md",
                "~/proj/AGENTS.md",
                "Rules.\n@a.md\n"
            ),
            file("project", "a.md", "~/proj/a.md", "A.\n")
        ))
    );
    assert_eq!(back.snapshot, p.start.snapshot);
}

#[test]
fn an_empty_project_file_with_no_base_sends_nothing() {
    let t = Tree::new();
    t.write("home/proj/AGENTS.md", "");
    t.write("home/.config/wisp/AGENTS.md", "Default.\n");
    let p = plugged(&t, &[]);
    assert_eq!(p.start, Rendered::default());
    assert_eq!(turn(&p.plugin, &Prior::None).text, None);
    assert_eq!(turn(&p.plugin, &Prior::Legacy(None)).text, None);
}

fn counting(t: &Tree, reads: &Arc<AtomicUsize>) -> FakeHost {
    let counted = reads.clone();
    FakeHost {
        now: Some(Arc::new(|| Instant {
            seconds: T0,
            nanos: 0,
        })),
        kind_of: Some(Arc::new(move |path: &str| {
            counted.fetch_add(1, Ordering::SeqCst);
            Some(if std::path::Path::new(path).is_file() {
                FileKind::Regular
            } else {
                FileKind::Other
            })
        })),
        ..t.host("proj", &[])
    }
}

fn stamp(t: &Tree, rel: &str, text: &str, before_now: u64) {
    t.write(rel, text);
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(T0.unsigned_abs() - before_now);
    std::fs::File::options()
        .write(true)
        .open(t.base.join(rel))
        .unwrap()
        .set_modified(at)
        .unwrap();
}

#[test]
fn a_same_size_rewrite_near_the_read_is_read_again_and_one_well_before_it_is_trusted() {
    let t = Tree::new();
    stamp(&t, "home/proj/AGENTS.md", "Rules A.\n", 1);
    let reads = Arc::new(AtomicUsize::new(0));
    let host = counting(&t, &reads);
    let plugin = instantiate(Arc::new(host.clone()));
    let base = Prior::Snapshot(render(&plugin, At::Turn, &Prior::None, None).snapshot);
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    stamp(&t, "home/proj/AGENTS.md", "Rules B.\n", 1);
    let racy = turn(&plugin, &base);
    assert_eq!(reads.load(Ordering::SeqCst), 2, "a racy file is read again");
    assert_eq!(
        racy.text.unwrap(),
        update(&file(
            "project",
            "AGENTS.md",
            "~/proj/AGENTS.md",
            "Rules B.\n"
        ))
    );
    stamp(&t, "home/proj/AGENTS.md", "Rules C.\n", 3);
    let base = Prior::Snapshot(turn(&plugin, &Prior::None).snapshot);
    assert_eq!(reads.load(Ordering::SeqCst), 3);
    stamp(&t, "home/proj/AGENTS.md", "Rules D.\n", 3);
    assert_eq!(turn(&plugin, &base).text, None);
    assert_eq!(
        reads.load(Ordering::SeqCst),
        3,
        "an unchanged size and time well before the read isn't read"
    );
    stamp(&t, "home/proj/AGENTS.md", "Rules E, longer.\n", 3);
    assert!(turn(&plugin, &base).text.is_some());
    assert_eq!(reads.load(Ordering::SeqCst), 4);
}

#[test]
fn a_head_goes_to_its_target_from_the_store_and_by_hash_after_a_restart() {
    let t = Tree::new();
    t.write("home/proj/AGENTS.md", "Rules.\n@a.md\n");
    t.write("home/proj/a.md", "A1.\n");
    let p = plugged(&t, &[]);
    let start = seen(&p.start);
    t.write("home/proj/a.md", "A2.\n");
    let pending = turn(&p.plugin, &start);
    t.write("home/proj/a.md", "A3.\n");
    let head = render(&p.plugin, At::Head, &start, Some(&pending.snapshot));
    assert_eq!(head.snapshot, pending.snapshot);
    assert_eq!(
        head.text, pending.text,
        "the head carries the target, not A3"
    );
    let current = render(&p.plugin, At::Head, &start, None);
    assert_eq!(
        current.text.unwrap(),
        update(&file("project", "a.md", "~/proj/a.md", "A3.\n"))
    );
    assert_eq!(
        render(&p.plugin, At::Head, &start, Some(&p.start.snapshot)).text,
        None
    );
    t.write("home/proj/a.md", "A2.\n");
    let fresh = plugged(&t, &[]);
    let by_hash = render(&fresh.plugin, At::Head, &start, Some(&pending.snapshot));
    assert_eq!(by_hash.text, pending.text);
    t.write("home/proj/a.md", "A4.\n");
    let later = plugged(&t, &[]);
    let moved = render(&later.plugin, At::Head, &start, Some(&pending.snapshot));
    assert_eq!(moved.snapshot, pending.snapshot);
    assert_eq!(
        moved.text.unwrap(),
        update(&file("project", "a.md", "~/proj/a.md", "A4.\n"))
    );
    std::fs::remove_file(t.home().join("proj/a.md")).unwrap();
    t.write("home/proj/AGENTS.md", "Rules.\n");
    let gone = plugged(&t, &[]);
    let missing = render(&gone.plugin, At::Head, &start, Some(&pending.snapshot));
    assert_eq!(
        missing.text.unwrap(),
        update(&format!(
            "{}<removed scope=\"project\" path=\"~/proj/a.md\"/>\n",
            file("project", "AGENTS.md", "~/proj/AGENTS.md", "Rules.\n")
        )),
        "a version the store lacks goes in at its current text"
    );
    assert!(
        changes(&p.host)
            .iter()
            .all(|(change, _)| change.summary.diagnostics.is_empty())
    );
}

#[test]
fn a_legacy_base_reads_only_the_project_and_default_files_of_the_logged_block() {
    let t = Tree::new();
    t.write("home/proj/AGENTS.md", "Answer briefly.\n");
    let p = plugged(&t, &[]);
    let block = format!(
        "<instructions>\nopening\n{}</instructions>\n<instructions>\npreset\n<file scope=\"preset\" name=\"AGENTS.md\">\nPreset.\n</file>\n</instructions>",
        "<file scope=\"project\" name=\"AGENTS.md\">\nAnswer briefly.\n</file>\n"
    );
    let legacy = Prior::Legacy(Some(block.clone()));
    assert_eq!(turn(&p.plugin, &legacy).text, None);
    t.write("home/proj/AGENTS.md", "Answer at length.\n");
    assert_eq!(
        turn(&p.plugin, &legacy).text.unwrap(),
        update(&file(
            "project",
            "AGENTS.md",
            "~/proj/AGENTS.md",
            "Answer at length.\n"
        ))
    );
}

#[test]
fn an_old_snapshot_line_of_an_only_imports_file_is_not_a_removal() {
    let t = Tree::new();
    t.write("home/proj/AGENTS.md", "@a.md\n");
    t.write("home/proj/a.md", "A.\n");
    let p = plugged(&t, &[]);
    let old = format!(
        "{} AGENTS.md\n{} a.md\n",
        sha256_hex("@a.md\n"),
        sha256_hex("A.\n")
    );
    assert_eq!(p.start.snapshot.lines().count(), 1);
    assert_eq!(turn(&p.plugin, &Prior::Snapshot(old)).text, None);
}

#[test]
fn a_same_size_edit_with_a_new_time_well_before_the_read_and_one_at_exactly_two_seconds_are_read() {
    let t = Tree::new();
    stamp(&t, "home/proj/AGENTS.md", "Rule 1.\n", 10);
    let reads = Arc::new(AtomicUsize::new(0));
    let host = counting(&t, &reads);
    let plugin = instantiate(Arc::new(host.clone()));
    let base = Prior::Snapshot(turn(&plugin, &Prior::None).snapshot);
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    stamp(&t, "home/proj/AGENTS.md", "Rule 2.\n", 5);
    assert_eq!(
        turn(&plugin, &base).text.unwrap(),
        update(&file(
            "project",
            "AGENTS.md",
            "~/proj/AGENTS.md",
            "Rule 2.\n"
        )),
        "one digit changed, a new time"
    );
    assert_eq!(reads.load(Ordering::SeqCst), 2);
    stamp(&t, "home/proj/AGENTS.md", "Rule 3.\n", 2);
    let base = Prior::Snapshot(turn(&plugin, &Prior::None).snapshot);
    assert_eq!(reads.load(Ordering::SeqCst), 3);
    stamp(&t, "home/proj/AGENTS.md", "Rule 4.\n", 2);
    assert!(
        turn(&plugin, &base).text.is_some(),
        "a time exactly 2 s before the read is racy"
    );
    assert_eq!(reads.load(Ordering::SeqCst), 4);
}

#[test]
fn a_legacy_target_is_read_as_its_files_so_the_head_goes_to_the_opened_set() {
    let t = Tree::new();
    t.write("home/proj/AGENTS.md", "Answer briefly.\n");
    let p = plugged(&t, &[]);
    let block = "<instructions>\nThese are the project's instructions.\n<file scope=\"project\" name=\"AGENTS.md\">\nAnswer briefly.\n</file>\n</instructions>".to_owned();
    let legacy = Prior::Legacy(Some(block.clone()));
    t.write("home/proj/AGENTS.md", "Answer at length.\n");
    let head = render(&p.plugin, At::Head, &legacy, Some(&block));
    assert_eq!(head.text, None, "{head:?}");
    assert_eq!(head.snapshot, p.start.snapshot);
    let moved = render(&p.plugin, At::Head, &Prior::None, Some(&block));
    assert_eq!(
        moved.text.unwrap(),
        update(&file(
            "project",
            "AGENTS.md",
            "~/proj/AGENTS.md",
            "Answer briefly.\n"
        )),
        "the opened text, from the block, not the newer file"
    );
}

#[test]
fn each_check_keeps_only_the_committed_and_new_versions_in_the_store() {
    let t = Tree::new();
    t.write("home/proj/AGENTS.md", "A1.\n");
    let p = plugged(&t, &[]);
    t.write("home/proj/AGENTS.md", "A2.\n");
    let second = turn(&p.plugin, &seen(&p.start));
    t.write("home/proj/AGENTS.md", "A3.\n");
    let third = turn(&p.plugin, &seen(&second));
    let back = render(&p.plugin, At::Head, &seen(&third), Some(&second.snapshot));
    assert_eq!(
        back.text.unwrap(),
        update(&file("project", "AGENTS.md", "~/proj/AGENTS.md", "A2.\n")),
        "the committed version stays"
    );
    let first = render(&p.plugin, At::Head, &seen(&third), Some(&p.start.snapshot));
    assert_eq!(
        first.text, None,
        "A1 was pruned, so the current A3 stands in"
    );
}

#[test]
fn a_switch_from_a_chosen_file_of_only_imports_names_where_it_came_from() {
    let t = Tree::new();
    t.write("home/.config/wisp/AGENTS.md", "Default.\n");
    t.write("home/proj/AGENTS.md", "@a.md\n");
    t.write("home/proj/a.md", "A.\n");
    let p = plugged(&t, &[]);
    std::fs::remove_file(t.home().join("proj/AGENTS.md")).unwrap();
    assert!(turn(&p.plugin, &seen(&p.start)).text.is_some());
    let (change, _) = changes(&p.host).pop().unwrap();
    assert_eq!(
        (change.from.map(|f| f.scope), change.to.map(|t| t.scope)),
        (Some(Scope::Project), Some(Scope::Default))
    );
}

#[test]
fn a_second_check_before_the_message_is_saved_keeps_the_unsaved_version_for_a_head() {
    let t = Tree::new();
    t.write("home/proj/AGENTS.md", "A1.\n");
    let p = plugged(&t, &[]);
    let start = seen(&p.start);
    t.write("home/proj/AGENTS.md", "A2.\n");
    let pending = turn(&p.plugin, &start);
    t.write("home/proj/AGENTS.md", "A3.\n");
    assert!(turn(&p.plugin, &start).text.is_some());
    let head = render(&p.plugin, At::Head, &start, Some(&pending.snapshot));
    assert_eq!(head.text, pending.text, "the head carries A2, not A3");
    t.write("home/proj/AGENTS.md", "A4.\n");
    let saved = turn(&p.plugin, &seen(&pending));
    assert!(saved.text.is_some());
    let first = render(&p.plugin, At::Head, &seen(&saved), Some(&p.start.snapshot));
    assert_eq!(first.text, None, "once saved, the older versions go");
}
