use std::fs;

use futures::executor::block_on;
use wisp_instructions::skills as frontmatter;
use wisp_plugin_api::hooks::{BeforeTurn, CompactedInfo, RewindInfo};
use wisp_plugin_testkit::host::FakeHost;

use super::*;

#[path = "tests/tree.rs"]
mod tree;

use tree::{Tree, skill};

fn plugin(host: FakeHost) -> Skills {
    let context = BuildContext {
        platform: "unix".to_owned(),
        capabilities: host.capabilities.clone(),
    };
    let plugin = Skills {
        host: Arc::new(host),
        state: Mutex::default(),
    };
    block_on(plugin.describe(&context));
    plugin
}

fn before(typed: &str) -> Event {
    Event::BeforeTurn(BeforeTurn {
        turn: 1,
        lead: Vec::new(),
        typed: typed.to_owned(),
        images: 0,
        retry: false,
    })
}

#[test]
fn plain_frontmatter_keeps_colons_and_the_body() {
    let parsed = frontmatter::parse("---\nname: guide\ndescription: Read first: use before writing.\nlicense: MIT\n---\n# Guide\n\nBody.\n").unwrap();
    assert_eq!(parsed.name, "guide");
    assert_eq!(parsed.description, "Read first: use before writing.");
    assert_eq!(parsed.body, "# Guide\n\nBody.\n");
}

#[test]
fn quoted_fields_support_single_quotes_and_double_quote_escapes() {
    let parsed =
        frontmatter::parse("---\nname: \"guide\"\ndescription: 'The user''s guide'\n---\n")
            .unwrap();
    assert_eq!(parsed.description, "The user's guide");
    let parsed = frontmatter::parse(
        r#"---
name: guide
description: "A \"quoted\" guide"
---
"#,
    )
    .unwrap();
    assert_eq!(parsed.description, "A \"quoted\" guide");
}

#[test]
fn folded_descriptions_join_their_lines() {
    let parsed = frontmatter::parse("---\nname: guide\ndescription: >-\n  First line.\n  Read before writing.\nmetadata:\n  author: placeholder\n---\nBody\n").unwrap();
    assert_eq!(parsed.description, "First line. Read before writing.");
    assert_eq!(parsed.body, "Body\n");
}

#[test]
fn literal_descriptions_keep_line_breaks_and_accept_crlf() {
    let parsed = frontmatter::parse("---\r\nname: guide\r\ndescription: |\r\n  First line.\r\n  Second line.\r\n---\r\nBody\r\n").unwrap();
    assert_eq!(parsed.description, "First line.\nSecond line.");
    assert_eq!(parsed.body, "Body\r\n");
}

#[test]
fn missing_or_broken_frontmatter_is_refused() {
    for text in [
        "Body",
        "---\nname: guide\n",
        "---\nname: guide\n---\nBody",
        "---\nname: guide\ndescription: ''\n---\n",
        "---\nname: guide\ndescription: 'not closed\n---\n",
        "---\nname: guide\ndescription: [not, text]\n---\n",
    ] {
        assert!(frontmatter::parse(text).is_err(), "{text:?}");
    }
}

#[test]
fn project_then_shared_then_config_sources_win_deterministically() {
    let tree = Tree::new();
    tree.write(
        "config",
        "guide",
        &skill("guide", "Config description", "Config body"),
    );
    tree.write(
        "yours",
        "guide",
        &skill("guide", "Shared description", "Shared body"),
    );
    tree.write(
        "project",
        "guide",
        &skill("guide", "Project description", "Project body"),
    );
    tree.write(
        "config",
        "config-only",
        &skill("config-only", "Config skill", "Body"),
    );
    let loaded = scan::load_all(&tree.host());
    assert_eq!(loaded.skills.len(), 2);
    assert_eq!(loaded.skills["guide"].origin, "project");
    assert_eq!(
        loaded.skills["guide"].text.description,
        "Project description"
    );
    assert_eq!(loaded.problems.len(), 2);
}

#[test]
#[cfg(unix)]
fn a_symlinked_skill_folder_is_followed() {
    let tree = Tree::new();
    let target = tree.write("config", "linked", &skill("linked", "Linked skill", "Body"));
    let root = tree.project.join(".agents/skills");
    fs::create_dir_all(&root).unwrap();
    std::os::unix::fs::symlink(target, root.join("linked")).unwrap();
    let loaded = scan::load_all(&tree.host());
    assert_eq!(loaded.skills["linked"].origin, "project");
    assert_eq!(loaded.problems.len(), 1);
}

#[test]
fn a_name_mismatch_warns_but_a_missing_description_skips() {
    let tree = Tree::new();
    tree.write("project", "folder", &skill("guide", "Useful guide", "Body"));
    tree.write("project", "broken", "---\nname: broken\n---\nBody");
    let loaded = scan::load_all(&tree.host());
    assert!(loaded.skills.contains_key("guide"));
    assert!(!loaded.skills.contains_key("broken"));
    assert!(
        loaded
            .problems
            .iter()
            .any(|problem| problem.message.contains("differs"))
    );
    assert!(
        loaded
            .problems
            .iter()
            .any(|problem| problem.message.contains("description"))
    );
}

#[test]
fn opening_returns_only_body_folder_and_file_names_with_a_short_home() {
    let tree = Tree::new();
    let folder = tree.write(
        "yours",
        "guide",
        &skill(
            "guide",
            "How to write",
            "    first command\n    second command  ",
        ),
    );
    fs::create_dir_all(folder.join("references")).unwrap();
    fs::write(folder.join("references/notes.md"), "UNREAD FILE CONTENT").unwrap();
    for hidden in [".git", ".venv", "node_modules"] {
        fs::create_dir_all(folder.join(hidden)).unwrap();
        fs::write(folder.join(hidden).join("hidden.md"), "unread").unwrap();
    }
    fs::write(folder.join(".hidden"), "unread").unwrap();
    let plugin = plugin(tree.host());
    let text = block_on(plugin.open_entry("guide")).unwrap();
    assert!(text.contains("Folder: ~/.agents/skills/guide"), "{text}");
    assert!(text.contains("references/notes.md"));
    assert!(
        text.ends_with("\n\n    first command\n    second command  \n"),
        "{text}"
    );
    assert!(
        !text.contains("hidden.md") && !text.contains(".hidden"),
        "{text}"
    );
    assert!(!text.contains("description:") && !text.contains("UNREAD FILE CONTENT"));
    assert!(!text.contains(&tree.home.display().to_string()));
    assert!(block_on(plugin.open_entry("missing")).is_err());
    for n in 0..60 {
        fs::write(folder.join(format!("file-{n:02}.md")), "unread").unwrap();
    }
    let files = scan::file_names(&folder).unwrap();
    assert_eq!(files.len(), 51);
    assert_eq!(files.last().unwrap(), "and 12 more");
    assert!(files[..50].windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn an_open_skill_is_not_repeated_until_a_rewind_or_compaction() {
    let tree = Tree::new();
    tree.write("project", "guide", &skill("guide", "Guide", "Body"));
    let plugin = plugin(tree.host());
    assert!(
        block_on(plugin.open_entry("guide"))
            .unwrap()
            .contains("Body")
    );
    assert_eq!(
        block_on(plugin.open_entry("guide")).unwrap(),
        "Skill `guide` is already in view."
    );
    assert_eq!(
        block_on(plugin.on_hook("nudge", before("Guide"))),
        Reply::None
    );
    block_on(plugin.on_hook("rewound", Event::Rewound(RewindInfo { turn: 1, epoch: 1 })));
    assert!(
        block_on(plugin.open_entry("guide"))
            .unwrap()
            .contains("Body")
    );
    block_on(plugin.on_hook(
        "compacted",
        Event::AfterCompact(CompactedInfo {
            epoch: 2,
            trigger: "auto".to_owned(),
            engine: "summary".to_owned(),
            before: 100,
            after: 50,
            summary: None,
            cost_micros: None,
            partial: false,
            failed: None,
        }),
    ));
    assert!(
        block_on(plugin.open_entry("guide"))
            .unwrap()
            .contains("Body")
    );
}

#[test]
fn the_nudge_has_an_absolute_bar_not_just_a_shared_word() {
    let tree = Tree::new();
    tree.write(
        "project",
        "rust-code",
        &skill(
            "rust-code",
            "Use Rust toolchain dependency lint checks",
            "Body",
        ),
    );
    tree.write(
        "project",
        "writing",
        &skill("writing", "Use prose spelling grammar paragraphs", "Body"),
    );
    let plugin = plugin(tree.host());
    block_on(plugin.on_hook("scan", before("Rust toolchain dependency lint checks")));
    let reply = block_on(plugin.on_hook("nudge", before("Rust toolchain dependency lint checks")));
    assert_eq!(
        reply,
        Reply::Append(vec![
            "Skills that may fit: rust-code. Read one with use.".to_owned()
        ])
    );
    assert_eq!(
        block_on(plugin.on_hook("nudge", before("Use a toaster"))),
        Reply::None
    );
}

#[test]
fn the_nudge_is_silent_while_search_is_off_in_the_current_snapshot() {
    let tree = Tree::new();
    tree.write(
        "project",
        "rust-code",
        &skill("rust-code", "Rust toolchain dependency lint checks", "Body"),
    );
    let mut host = tree.host();
    host.capabilities.clear();
    let plugin = plugin(host);
    block_on(plugin.describe(&BuildContext {
        platform: "unix".to_owned(),
        capabilities: vec![SEARCH_CAPABILITY.to_owned()],
    }));
    block_on(plugin.on_hook("scan", before("Rust toolchain dependency lint checks")));
    assert_eq!(
        block_on(plugin.on_hook("nudge", before("Rust toolchain dependency lint checks"))),
        Reply::None
    );
}

#[test]
fn doctor_names_broken_files_and_shadowed_copies() {
    let tree = Tree::new();
    tree.write("project", "guide", &skill("guide", "Guide", "Body"));
    tree.write("yours", "guide", &skill("guide", "Shared guide", "Body"));
    tree.write("config", "broken", "---\nname: broken\n---\nBody");
    let plugin = plugin(tree.host());
    let rows = block_on(plugin.doctor(&BuildContext {
        platform: "unix".to_owned(),
        capabilities: Vec::new(),
    }));
    let text = format!("{rows:?}");
    assert!(text.contains("skill guide") && text.contains("broken/SKILL.md"));
    assert!(text.contains("earlier folder") && text.contains("description"));
    assert!(!text.contains(&tree.home.display().to_string()));
}

#[test]
fn skills_with_nothing_found_matches_c1s_empty_answer() {
    let tree = Tree::new();
    let plugin = plugin(tree.host());
    let mut title = span("Skills", Tone::Text);
    title.bold = true;
    assert_eq!(
        block_on(plugin.run_command("skills", "")),
        CommandReply::Lines(vec![
            CommandLine::Row(CommandRow {
                left: vec![title],
                right: vec![span("0", Tone::Text)]
            }),
            CommandLine::Text(vec![span(
                "none yet: put a skill's folder in .agents/skills/ here, or in ~/.agents/skills/ for every project",
                Tone::Text
            )]),
        ])
    );
}

#[test]
fn skills_lists_only_its_folders_and_says_when_search_is_off() {
    let tree = Tree::new();
    tree.write(
        "project",
        "rust-code",
        &skill("rust-code", "Rust rules for Wisp", "Body"),
    );
    tree.write(
        "yours",
        "ponytail",
        &skill("ponytail", "The smallest complete change", "Body"),
    );
    let mut host = tree.host();
    host.capabilities.clear();
    host.entries.push(Entry {
        kind: Kind::Skill,
        name: "issue-style".to_owned(),
        summary: "Server prompt".to_owned(),
        text: String::new(),
    });
    let plugin = plugin(host);
    let CommandReply::Lines(lines) = block_on(plugin.run_command("skills", "")) else {
        panic!("expected lines")
    };
    let mut title = span("Skills", Tone::Text);
    title.bold = true;
    assert_eq!(
        lines,
        vec![
            CommandLine::Row(CommandRow {
                left: vec![title],
                right: vec![span("2", Tone::Text)]
            }),
            CommandLine::Row(CommandRow {
                left: vec![
                    span("ponytail   ", Tone::Text),
                    span("yours     ", Tone::Muted),
                    span("on", Tone::Ok)
                ],
                right: Vec::new()
            }),
            CommandLine::Row(CommandRow {
                left: vec![span("  The smallest complete change", Tone::Muted)],
                right: Vec::new()
            }),
            CommandLine::Row(CommandRow {
                left: vec![
                    span("rust-code  ", Tone::Text),
                    span("project   ", Tone::Muted),
                    span("on", Tone::Ok)
                ],
                right: Vec::new()
            }),
            CommandLine::Row(CommandRow {
                left: vec![span("  Rust rules for Wisp", Tone::Muted)],
                right: Vec::new()
            }),
            CommandLine::Text(Vec::new()),
            CommandLine::Text(vec![span(
                "search is off, so the model can't find these: turn it on in /config, Tools",
                Tone::Text
            )]),
        ]
    );
}

#[test]
fn a_turned_off_skill_reads_off_gets_no_nudge_and_skills_says_where_to_turn_it_on() {
    let tree = Tree::new();
    tree.write(
        "project",
        "rust-code",
        &skill("rust-code", "Rust toolchain dependency lint checks", "Body"),
    );
    tree.write(
        "yours",
        "ponytail",
        &skill("ponytail", "The smallest complete change", "Body"),
    );
    let mut host = tree.host();
    host.settings = vec![(TURNED_OFF.to_owned(), "rust-code".to_owned())];
    let plugin = plugin(host);
    let typed = before("Rust toolchain dependency lint checks");
    block_on(plugin.on_hook("scan", typed.clone()));
    assert_eq!(block_on(plugin.on_hook("nudge", typed)), Reply::None);
    let CommandReply::Lines(lines) = block_on(plugin.run_command("skills", "")) else {
        panic!("expected lines")
    };
    assert_eq!(
        lines[3],
        CommandLine::Row(CommandRow {
            left: vec![
                span("rust-code  ", Tone::Text),
                span("project   ", Tone::Muted),
                span("off", Tone::Muted)
            ],
            right: Vec::new()
        })
    );
    assert_eq!(
        lines[lines.len() - 2..],
        [
            CommandLine::Text(Vec::new()),
            CommandLine::Text(vec![span(
                wisp_tool_text::manage::TURN_ON_WHERE,
                Tone::Muted
            )])
        ]
    );
    let manifest = wisp_plugin_api::manifest::Manifest::parse(
        MANIFEST,
        wisp_plugin_api::manifest::Origin::Bundled,
    )
    .unwrap();
    assert!(format!("{:?}", manifest.settings).contains(TURNED_OFF));
}

#[test]
fn an_unchanged_opening_does_not_dirty_the_index_and_a_gather_reloads() {
    let tree = Tree::new();
    let folder = tree.write(
        "project",
        "guide",
        &skill("guide", "First", "Placeholder first."),
    );
    let changes = Arc::new(Mutex::new(0));
    let changed = changes.clone();
    let plugin = plugin(FakeHost {
        index_changed: Some(Arc::new(move || *changed.lock().unwrap() += 1)),
        ..tree.host()
    });
    assert_eq!(block_on(plugin.entries())[0].text, "First");
    assert_eq!(*changes.lock().unwrap(), 0);
    block_on(plugin.on_hook("scan", before("Guide")));
    assert_eq!(*changes.lock().unwrap(), 0);
    fs::write(
        folder.join("SKILL.md"),
        skill("guide", "Second", "Placeholder second."),
    )
    .unwrap();
    assert!(
        block_on(plugin.open_entry("guide"))
            .unwrap()
            .contains("Placeholder first.")
    );
    assert_eq!(*changes.lock().unwrap(), 0);
    assert_eq!(block_on(plugin.entries())[0].text, "Second");
    assert_eq!(*changes.lock().unwrap(), 0);
    assert!(
        block_on(plugin.open_entry("guide"))
            .unwrap()
            .contains("Placeholder second.")
    );
    fs::write(
        folder.join("SKILL.md"),
        skill("guide", "Third", "Placeholder third."),
    )
    .unwrap();
    block_on(plugin.on_hook("scan", before("Guide")));
    assert_eq!(*changes.lock().unwrap(), 1);
    assert_eq!(block_on(plugin.entries())[0].text, "Third");
    assert_eq!(*changes.lock().unwrap(), 1);
}
