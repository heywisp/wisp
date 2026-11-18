use super::*;

fn entry(scope: Scope, name: &str, sha: &str) -> Entry {
    Entry {
        scope,
        name: name.to_owned(),
        sha256: sha.to_owned(),
    }
}

fn text(scope: Scope, name: &str, path: &str, text: &str) -> Item {
    Item {
        scope,
        name: name.to_owned(),
        display_path: path.to_owned(),
        now: Now::Text {
            sha256: sha256_hex(text),
            text: text.to_owned(),
        },
    }
}

fn nothing_left(_: &Entry) -> Option<(String, bool)> {
    None
}

#[test]
fn a_snapshot_reads_back_its_entries_with_their_scopes() {
    let entries = vec![
        entry(Scope::Default, "AGENTS.md", "aa"),
        entry(Scope::Default, "a b.md", "bb"),
    ];
    let snapshot = snapshot_of(&entries);
    assert_eq!(snapshot, "aa default AGENTS.md\nbb default a b.md\n");
    assert_eq!(super::entries(&snapshot), entries);
}

#[test]
fn a_two_field_line_takes_the_scope_it_is_given_and_reads_as_project_alone() {
    let old = "aa AGENTS.md\nbb docs/a b.md\n";
    assert_eq!(
        super::entries(old),
        [
            entry(Scope::Project, "AGENTS.md", "aa"),
            entry(Scope::Project, "docs/a b.md", "bb")
        ]
    );
    let scoped = with_scope(old, Scope::Default);
    assert_eq!(scoped, "aa default AGENTS.md\nbb default docs/a b.md\n");
    assert_eq!(with_scope(&scoped, Scope::Project), scoped);
}

#[test]
fn a_legacy_block_gives_only_its_project_and_default_files() {
    let block = "<instructions>\nopening\n<file scope=\"project\" name=\"AGENTS.md\">\nOne.\n</file>\n<file scope=\"project\" name=\"b.md\" imported_by=\"AGENTS.md\">\nTwo.\n</file>\n</instructions>\n<instructions>\npreset\n<file scope=\"preset\" name=\"AGENTS.md\">\nPreset.\n</file>\n</instructions>";
    assert_eq!(
        legacy_entries(block),
        [
            entry(Scope::Project, "AGENTS.md", &sha256_hex("One.\n")),
            entry(Scope::Project, "b.md", &sha256_hex("Two.\n")),
        ]
    );
    assert_eq!(
        files_in(block, &[Scope::Preset]),
        [(
            entry(Scope::Preset, "AGENTS.md", &sha256_hex("Preset.\n")),
            "Preset.\n".to_owned()
        )]
    );
    assert!(has_files(block));
    assert_eq!(block_scope(block), Some(Scope::Project));
    let preset_only =
        "<instructions>\n<file scope=\"preset\" name=\"AGENTS.md\">\nx\n</file>\n</instructions>";
    assert!(!has_files(preset_only));
    assert_eq!(block_scope(preset_only), None);
    assert!(legacy_entries(preset_only).is_empty());
}

#[test]
fn an_edit_sends_the_whole_new_text_in_the_update_block() {
    let old = [entry(Scope::Project, "AGENTS.md", &sha256_hex("Old.\n"))];
    let items = [text(Scope::Project, "AGENTS.md", "~/p/AGENTS.md", "New.\n")];
    let update = update(&old, &items, nothing_left).unwrap();
    assert_eq!(
        update.text,
        "<instructions-update>\nThese files changed since they were loaded. Each version below replaces the earlier one.\n<file scope=\"project\" name=\"AGENTS.md\" path=\"~/p/AGENTS.md\">\nNew.\n</file>\n</instructions-update>\n"
    );
    assert_eq!(
        update.changed,
        [FileChange {
            scope: Scope::Project,
            name: "AGENTS.md".into(),
            display_path: "~/p/AGENTS.md".into(),
            old_sha256: Some(sha256_hex("Old.\n")),
            new_sha256: Some(sha256_hex("New.\n")),
        }]
    );
}

#[test]
fn unchanged_files_send_nothing() {
    let old = [entry(Scope::Project, "AGENTS.md", &sha256_hex("Same.\n"))];
    let items = [text(
        Scope::Project,
        "AGENTS.md",
        "~/p/AGENTS.md",
        "Same.\n",
    )];
    assert_eq!(update(&old, &items, nothing_left), None);
    assert_eq!(update(&[], &[], nothing_left), None);
}

#[test]
fn a_removed_import_an_unreadable_file_and_nothing_left_are_worded() {
    let old = [
        entry(Scope::Project, "AGENTS.md", "aa"),
        entry(Scope::Project, "b.md", "bb"),
    ];
    let left = |e: &Entry| Some((format!("~/p/{}", e.name), e.name == "AGENTS.md"));
    let update = update(&old, &[], left).unwrap();
    assert_eq!(
        update.text,
        "<instructions-update>\nThese files changed since they were loaded. Each version below replaces the earlier one.\n<removed scope=\"project\" path=\"~/p/AGENTS.md\"/>\nThis file can no longer be read; its earlier instructions no longer apply.\n<removed scope=\"project\" path=\"~/p/b.md\"/>\nNo instructions apply from now on.\n</instructions-update>\n"
    );
    assert_eq!(update.changed.len(), 2);
}

#[test]
fn a_source_switch_names_the_new_file_and_removes_the_old() {
    let old = [entry(Scope::Default, "AGENTS.md", "aa")];
    let items = [text(
        Scope::Project,
        "AGENTS.md",
        "~/p/AGENTS.md",
        "Project.\n",
    )];
    let left = |_: &Entry| Some(("~/.config/wisp/AGENTS.md".to_owned(), false));
    let update = update(&old, &items, left).unwrap();
    assert_eq!(
        update.text,
        "<instructions-update>\nThese files changed since they were loaded. Each version below replaces the earlier one.\n<file scope=\"project\" name=\"AGENTS.md\" path=\"~/p/AGENTS.md\">\nProject.\n</file>\n<removed scope=\"default\" path=\"~/.config/wisp/AGENTS.md\"/>\n</instructions-update>\n"
    );
}

#[test]
fn a_target_file_that_is_gone_is_removed_once_and_skipped_when_never_seen() {
    let old = [entry(Scope::Project, "b.md", "bb")];
    let gone = |name: &str| Item {
        scope: Scope::Project,
        name: name.to_owned(),
        display_path: format!("~/p/{name}"),
        now: Now::Gone { unreadable: false },
    };
    let update = update(&old, &[gone("b.md"), gone("c.md")], nothing_left).unwrap();
    assert_eq!(update.changed.len(), 1);
    assert!(
        update
            .text
            .contains("<removed scope=\"project\" path=\"~/p/b.md\"/>\n")
    );
    assert!(!update.text.contains("c.md"));
}

#[test]
fn roots_place_each_scope_and_shorten_under_home() {
    let roots = Roots {
        project: Path::new("/h/p"),
        config_dir: Some(Path::new("/h/.config/wisp")),
        home: Some(Path::new("/h")),
    };
    assert_eq!(roots.shown(Scope::Project, "docs/../a.md"), "~/p/a.md");
    assert_eq!(
        roots.shown(Scope::Default, "AGENTS.md"),
        "~/.config/wisp/AGENTS.md"
    );
    assert_eq!(roots.shown(Scope::Project, "~/x.md"), "~/x.md");
    assert_eq!(roots.shown(Scope::Project, "/etc/x.md"), "/etc/x.md");
}

fn change(scope: Scope, name: &str, old: Option<&str>, new: Option<&str>) -> FileChange {
    FileChange {
        scope,
        name: name.to_owned(),
        display_path: format!("~/p/{name}"),
        old_sha256: old.map(str::to_owned),
        new_sha256: new.map(str::to_owned),
    }
}

#[test]
fn since_keeps_only_the_changes_the_baseline_has_not_seen_and_rebases_their_old_side() {
    let baseline = [
        entry(Scope::Project, "AGENTS.md", "a2"),
        entry(Scope::Project, "b.md", "b1"),
        entry(Scope::Project, "c.md", "c1"),
    ];
    let changed = vec![
        change(Scope::Project, "AGENTS.md", Some("a1"), Some("a2")),
        change(Scope::Project, "b.md", Some("b0"), Some("b2")),
        change(Scope::Project, "c.md", Some("c0"), None),
        change(Scope::Project, "d.md", Some("d1"), None),
        change(Scope::Default, "AGENTS.md", None, Some("a2")),
    ];
    assert_eq!(
        since(changed, &baseline),
        [
            change(Scope::Project, "b.md", Some("b1"), Some("b2")),
            change(Scope::Project, "c.md", Some("c1"), None),
            change(Scope::Default, "AGENTS.md", None, Some("a2")),
        ]
    );
}

#[test]
fn a_snapshot_is_told_from_a_logged_block_and_both_read_as_entries() {
    let sha = sha256_hex("Rules.\n");
    let snapshot = format!("{sha} project AGENTS.md\n");
    assert!(is_snapshot(&snapshot));
    assert!(is_snapshot(""));
    let block = "<instructions>\nThese are the project's instructions.\n<file scope=\"project\" name=\"AGENTS.md\">\nRules.\n</file>\n</instructions>";
    assert!(!is_snapshot(block));
    assert_eq!(
        legacy_files(block),
        [(
            entry(Scope::Project, "AGENTS.md", &sha),
            "Rules.\n".to_owned()
        )]
    );
    assert_eq!(
        prior_entries(&Prior::Legacy(Some(block.to_owned()))),
        Some(super::entries(&snapshot))
    );
    assert_eq!(prior_entries(&Prior::Legacy(None)), None);
    assert_eq!(prior_entries(&Prior::None), Some(Vec::new()));
}
