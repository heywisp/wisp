#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests fail on an unexpected parse"
)]

use wisp_plugin_api::manifest::{Manifest, Origin, Place, Problem, Slot, Switch};

mod support;

use support::place;

const CONTEXT: &str = r#"id = "context"
version = "0.1.0"
world = "0.2.0"
summary = "Tells the model how full its context is"
settings = ["context.context_note"]

[[slots]]
slot = "sidebar"
name = "usage"
title = "Context"
after = []
before = []
default = "off"
marks = ["after-response", "after-compact", "rewound", "model-switched", "settings-changed"]
"#;

const HEAD: &str = r#"id = "acme"
version = "0.1.0"
world = "0.2.0"
summary = "Acme"

[[points]]
name = "ready"
kind = "observe"
version = "1"

[[points]]
name = "lead"
kind = "contribute"
version = "1"
"#;

fn parse(text: &str) -> Result<Manifest, Problem> {
    Manifest::parse(text, Origin::External)
}

fn problem(text: &str) -> Problem {
    support::problem(text, Origin::External)
}

fn section(keys: &str) -> String {
    format!("{HEAD}\n[[slots]]\nslot = \"sidebar\"\nname = \"jobs\"\ntitle = \"Jobs\"\n{keys}")
}

fn refused_at(text: &str, needle: &str, message: &str) {
    support::refused_at(text, Origin::External, needle, message);
}

#[test]
fn the_context_section_from_the_plan_parses_under_world_0_2_0() {
    let manifest = Manifest::parse(CONTEXT, Origin::Bundled).unwrap();
    assert_eq!(
        manifest.slots,
        [Slot {
            slot: Place::Sidebar,
            name: "usage".to_owned(),
            title: "Context".to_owned(),
            after: Vec::new(),
            before: Vec::new(),
            default: Switch::Off,
            marks: [
                "after-response",
                "after-compact",
                "rewound",
                "model-switched",
                "settings-changed"
            ]
            .map(str::to_owned)
            .to_vec(),
            refresh_s: None,
        }]
    );
    assert!(manifest.notes().is_empty());
}

#[test]
fn a_section_with_only_its_required_keys_is_on_and_has_no_marks() {
    let manifest = parse(&section("")).unwrap();
    let slot = &manifest.slots[0];
    assert_eq!(slot.default, Switch::On);
    assert!(slot.after.is_empty() && slot.before.is_empty() && slot.marks.is_empty());
    assert_eq!(slot.refresh_s, None);
    for key in ["slot", "name", "title"] {
        let text: String = section("")
            .lines()
            .filter(|line| !line.starts_with(&format!("{key} = ")))
            .map(|line| format!("{line}\n"))
            .collect();
        let problem = problem(&text);
        assert!(
            problem.message.contains(&format!("missing field `{key}`")),
            "{key}: {problem}"
        );
        assert!(problem.line.is_some(), "{key}");
    }
}

#[test]
fn slots_are_an_unknown_key_under_world_0_1_0() {
    let text = CONTEXT.replace("world = \"0.2.0\"", "world = \"0.1.0\"");
    let problem = problem(&text);
    assert!(
        problem.message.starts_with("unknown field `slots`"),
        "{problem}"
    );
    assert_eq!(problem.line, Some(7), "{problem}");
    assert!(problem.column.is_some());
}

#[test]
fn world_0_2_0_reads_every_0_1_0_manifest_the_same() {
    let old = "id = \"acme\"\nversion = \"0.1.0\"\nworld = \"0.1.0\"\nsummary = \"Acme\"\n\n[[hooks]]\nname = \"h\"\npoint = \"acme/ready\"\nbackground = true\n\n[[points]]\nname = \"ready\"\nkind = \"observe\"\nversion = \"1\"\n";
    let before = parse(old).unwrap();
    let after = parse(&old.replace("\"0.1.0\"\nsummary", "\"0.2.0\"\nsummary")).unwrap();
    assert_eq!(before.world, "0.1.0");
    assert_eq!(after.world, "0.2.0");
    assert_eq!(
        Manifest {
            world: "0.1.0".to_owned(),
            ..after
        },
        before
    );
    assert!(before.slots.is_empty());
}

#[test]
fn world_0_3_0_declares_sections_marked_at_agent_points_as_0_2_0_does() {
    let text = section("marks = [\"agent-spawned\", \"agent-state\", \"agent-ended\"]\n");
    let older = parse(&text).unwrap();
    let newer = parse(&text.replace("world = \"0.2.0\"", "world = \"0.3.0\"")).unwrap();
    assert_eq!(newer.world, "0.3.0");
    assert_eq!(newer.slots, older.slots);
    assert_eq!(
        newer.slots[0].marks,
        ["agent-spawned", "agent-state", "agent-ended"]
    );
    assert!(newer.notes().is_empty());
    let bundled = CONTEXT.replace("world = \"0.2.0\"", "world = \"0.3.0\"");
    assert_eq!(
        Manifest::parse(&bundled, Origin::Bundled).unwrap().slots,
        Manifest::parse(CONTEXT, Origin::Bundled).unwrap().slots
    );
}

#[test]
fn a_slot_this_wisp_lacks_is_refused_where_it_is_written() {
    for other in ["footer", "header", "Sidebar", ""] {
        let text = section("").replace("slot = \"sidebar\"", &format!("slot = \"{other}\""));
        refused_at(
            &text,
            &format!("\"{other}\"\nname"),
            &format!("this Wisp has no slot named `{other}`"),
        );
    }
}

#[test]
fn section_names_follow_the_id_grammar_and_are_unique() {
    let long = "a".repeat(33);
    for bad in [
        "j",
        "Jobs",
        "1jobs",
        "-jobs",
        "jobs_list",
        "jo bs",
        long.as_str(),
    ] {
        let text = section("").replace("name = \"jobs\"", &format!("name = \"{bad}\""));
        refused_at(
            &text,
            &format!("\"{bad}\"\ntitle"),
            &format!(
                "section name `{bad}` must be 2 to 32 characters of a-z, 0-9 and -, starting with a letter"
            ),
        );
    }
    for good in ["jo", "jobs-2", &"a".repeat(32)] {
        let text = section("").replace("name = \"jobs\"", &format!("name = \"{good}\""));
        assert!(parse(&text).is_ok(), "{good}");
    }
    let twice = format!(
        "{}\n[[slots]]\nslot = \"sidebar\"\nname = \"jobs\" \ntitle = \"Again\"\n",
        section("")
    );
    refused_at(&twice, "\"jobs\" \n", "section `jobs` is declared twice");
}

#[test]
fn titles_are_one_short_line_measured_in_characters() {
    let cases = [
        ("\"\"", "section `jobs`'s title is blank".to_owned()),
        ("\"   \"", "section `jobs`'s title is blank".to_owned()),
        (
            "\"\\u200B \\uFEFF\\u2060\\u202E\"",
            "section `jobs`'s title is blank".to_owned(),
        ),
        (
            "\"Jobs\\nand more\"",
            "section `jobs`'s title holds a control character; it must be one line of text"
                .to_owned(),
        ),
        (
            "\"Jobs\\tnow\"",
            "section `jobs`'s title holds a control character; it must be one line of text"
                .to_owned(),
        ),
        (
            "\"Jobs\\u001b[31m\"",
            "section `jobs`'s title holds a control character; it must be one line of text"
                .to_owned(),
        ),
        (
            &format!("\"{}\"", "x".repeat(25)),
            "section `jobs`'s title is 25 characters long; the limit is 24".to_owned(),
        ),
        (
            &format!("\"{}\"", "日".repeat(25)),
            "section `jobs`'s title is 25 characters long; the limit is 24".to_owned(),
        ),
    ];
    for (title, message) in cases {
        let text = section("").replace("title = \"Jobs\"", &format!("title = {title}"));
        refused_at(&text, &format!("{title}\n"), &message);
    }
    for title in ["x".repeat(24), "日".repeat(12), "Agents · état".to_owned()] {
        let text = section("").replace("title = \"Jobs\"", &format!("title = \"{title}\""));
        let manifest = parse(&text).unwrap_or_else(|p| panic!("{title}: {p}"));
        assert_eq!(manifest.slots[0].title, title);
    }
}

#[test]
fn refresh_s_is_one_second_to_an_hour() {
    for (seconds, good) in [(0, false), (1, true), (3600, true), (3601, false)] {
        let text = section(&format!("refresh_s = {seconds}\n"));
        if good {
            assert_eq!(parse(&text).unwrap().slots[0].refresh_s, Some(seconds));
        } else {
            refused_at(
                &text,
                &format!("{seconds}\n"),
                &format!("section `jobs` refreshes every {seconds} s; refresh_s must be 1 to 3600"),
            );
        }
    }
    let negative = problem(&section("refresh_s = -1\n"));
    assert!(negative.message.starts_with("invalid value"), "{negative}");
    assert_eq!(
        (negative.line, negative.column),
        place(&section("refresh_s = -1\n"), "-1")
    );
}

#[test]
fn after_and_before_hold_full_section_names() {
    for word in ["after", "before"] {
        for bad in [
            "usage",
            "Context/usage",
            "context/u",
            "context/",
            "/usage",
            "a/b/c",
        ] {
            let text = section(&format!("{word} = [\"other/fine\", \"{bad}\"]\n"));
            refused_at(
                &text,
                &format!("\"{bad}\""),
                &format!(
                    "section `jobs` is ordered {word} `{bad}`, which isn't a section name; one is written <plugin>/<section>"
                ),
            );
        }
        let text = section(&format!("{word} = [\"context/usage\", \"acme/missing\"]\n"));
        let manifest = parse(&text).unwrap();
        assert!(manifest.notes().is_empty());
    }
}

#[test]
fn marks_name_observe_points_only() {
    for gate in [
        "user-input",
        "before-turn",
        "before-tool",
        "before-compact",
        "compaction-head",
        "context-threshold",
        "before-end",
        "acme/lead",
    ] {
        let text = section(&format!("marks = [\"after-turn\", \"{gate}\"]\n"));
        refused_at(
            &text,
            &format!("\"{gate}\""),
            &format!("section `jobs` is marked at `{gate}`, which isn't an observe point"),
        );
    }
    for own in ["ready", "lead"] {
        let text = section(&format!("marks = [\"after-turn\", \"{own}\"]\n"));
        refused_at(
            &text,
            &format!("\"{own}\"]"),
            &format!(
                "section `jobs` is marked at `{own}`, this plugin's own point; write it in full: `acme/{own}`"
            ),
        );
    }
    let undeclared = section("marks = [\"acme/gone\"]\n");
    refused_at(
        &undeclared,
        "\"acme/gone\"",
        "`acme/gone` names this plugin, which declares no point `gone`",
    );
    for malformed in ["Bad", "bad/P", "x/y/z", ""] {
        let text = section(&format!("marks = [\"{malformed}\"]\n"));
        refused_at(
            &text,
            &format!("\"{malformed}\""),
            &format!(
                "`{malformed}` isn't a point of world 0.2.0; a plugin point is written <plugin>/<point>"
            ),
        );
    }
}

#[test]
fn marks_take_wisps_observe_points_and_the_plugins_own_in_full() {
    let marks = [
        "session-start",
        "session-end",
        "after-turn",
        "before-request",
        "after-response",
        "after-compact",
        "rewound",
        "command",
        "tools-changed",
        "settings-changed",
        "model-switched",
        "agent-spawned",
        "agent-state",
        "agent-mail",
        "agent-reported",
        "agent-stopped",
        "agent-ended",
        "acme/ready",
    ];
    let list = marks.map(|m| format!("\"{m}\"")).join(", ");
    let manifest = parse(&section(&format!("marks = [{list}]\n"))).unwrap();
    assert_eq!(manifest.slots[0].marks, marks);
    assert!(manifest.notes().is_empty());
}

#[test]
fn an_unknown_mark_loads_with_a_doctor_note() {
    let text = section("marks = [\"agent-woke\", \"after-turn\", \"agents/spawned\"]\n");
    let manifest = parse(&text).unwrap();
    assert_eq!(
        manifest.notes(),
        [
            "section `acme/jobs` is marked at `agent-woke`, a point this Wisp doesn't fire; that mark is ignored",
            "section `acme/jobs` is marked at `agents/spawned`, another plugin's point; only Wisp's points and the plugin's own mark a section, so that mark is ignored",
        ]
    );
}

#[test]
fn an_unknown_key_in_slots_is_refused_where_it_is_written() {
    let text = section("colour = \"red\"\n");
    let problem = problem(&text);
    assert!(
        problem.message.starts_with("unknown field `colour`"),
        "{problem}"
    );
    assert_eq!((problem.line, problem.column), place(&text, "colour"));
    let default = problem_for_default();
    assert!(
        default.message.starts_with("unknown variant `maybe`"),
        "{default}"
    );
}

fn problem_for_default() -> Problem {
    problem(&section("default = \"maybe\"\n"))
}
