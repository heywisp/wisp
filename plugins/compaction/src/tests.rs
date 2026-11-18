use futures::executor::block_on;
use wisp_plugin_api::hooks::CompactInfo;
use wisp_plugin_api::host::{JobEnd, Mode, StoppedBy};
use wisp_plugin_api::manifest::{Manifest, Origin};
use wisp_plugin_testkit::host::FakeHost;

use super::*;

const WRITTEN: &str = "## Goal and standing instructions\nSay hello.\n\n## Decisions\nNone yet.\n\n## State of the work\nDone: the greeting.\n\n## Files, branches and artefacts\nNone.\n\n## Commands, errors and fixes\nNone.\n\n## Open questions\nNone.";

fn with_heading(heading: &str, variant: &str) -> String {
    WRITTEN.replace(&format!("## {heading}\n"), &format!("{variant}\n"))
}

#[test]
fn a_section_heading_matches_whatever_its_punctuation_and_case() {
    let variants = [
        (
            "Files, branches and artefacts",
            "## Files, branches, and artefacts",
        ),
        (
            "Files, branches and artefacts",
            "## Files, Branches & Artefacts",
        ),
        (
            "Commands, errors and fixes",
            "## Commands, errors, and fixes",
        ),
        ("Commands, errors and fixes", "## Commands, Errors & Fixes"),
        ("Decisions", "## 2. Decisions"),
        ("Decisions", "## Decisions and rationale"),
        (
            "Goal and standing instructions",
            "## Goal & standing instructions",
        ),
        ("State of the work", "**State of the Work:**"),
        ("Open questions", "### 6) OPEN QUESTIONS"),
    ];
    for (heading, variant) in variants {
        let summary = with_heading(heading, variant);
        assert_ne!(summary, WRITTEN, "{variant}");
        assert_eq!(missing_section(&summary), None, "{variant}");
    }
    assert_eq!(missing_section(WRITTEN), None);
    let subgoal = with_heading("Goal and standing instructions", "## Subgoals");
    assert_eq!(
        missing_section(&subgoal),
        Some("Goal and standing instructions")
    );
    for (heading, _) in SECTIONS {
        let without = WRITTEN.replace(&format!("## {heading}\n"), "## Notes\n");
        assert_eq!(missing_section(&without), Some(heading), "{heading}");
    }
}

#[test]
fn the_instruction_names_each_heading_word_for_word() {
    assert!(SUMMARY_INSTRUCTION.contains("word for word"));
    for (number, (heading, _)) in SECTIONS.iter().enumerate() {
        assert!(
            SUMMARY_INSTRUCTION.contains(&format!("{}. {heading}:", number + 1)),
            "{heading}"
        );
    }
}

fn info(focus: Option<&str>, native: bool, verbatim_tokens: u64) -> StrategyInfo {
    StrategyInfo {
        compact: CompactInfo {
            trigger: "manual".to_owned(),
            engine: "wisp".to_owned(),
            turn: None,
            focus: focus.map(str::to_owned),
            estimate: 0,
            point: None,
            epoch: 0,
            requested_by: None,
            instruction: "core".to_owned(),
        },
        epoch: 2,
        first_turn: 1,
        last_turn: 3,
        native,
        cut_off: None,
        verbatim_tokens,
    }
}

fn said(turn: u64, text: &str, seq: Option<u64>) -> SaidEntry {
    SaidEntry {
        turn,
        text: text.to_owned(),
        seq,
    }
}

fn job(id: u32, label: &str, unread: u64) -> JobInfo {
    JobInfo {
        id: Some(id),
        call_id: None,
        label: label.to_owned(),
        end: None,
        mode: Mode::Pipe,
        lines: 0,
        bytes: 0,
        restored: false,
        echo_off: false,
        full_screen: false,
        unread,
        limit_ms: None,
        left_ms: None,
        size: None,
        keep: false,
        log: None,
    }
}

struct Viewed {
    said: Vec<SaidEntry>,
    calls: Vec<CallEntry>,
}

impl View for Viewed {
    fn said(&self) -> Vec<SaidEntry> {
        self.said.clone()
    }

    fn calls(&self) -> Vec<CallEntry> {
        self.calls.clone()
    }

    fn log_pointer(&self) -> Option<String> {
        Some("s1.jsonl in Wisp's sessions folder".to_owned())
    }

    fn output_pointer(&self) -> Option<String> {
        None
    }
}

#[test]
fn the_manifest_offers_a_strategy_and_owns_the_verbatim_setting() {
    let manifest = Manifest::parse(MANIFEST, Origin::Bundled).unwrap();
    assert_eq!(manifest.id, "compaction");
    assert!(manifest.strategy.is_some());
    assert_eq!(
        manifest.names().collect::<Vec<_>>(),
        ["context.verbatim_user_tokens"]
    );
}

#[test]
fn the_instruction_is_the_six_section_one_with_its_focus() {
    let plugin = instantiate(Arc::new(FakeHost::default()));
    let plain = block_on(plugin.instruction(info(None, false, 0)));
    assert_eq!(plain, Some(summary_instruction(None)));
    assert!(summary_instruction(None).starts_with(SUMMARY_INSTRUCTION));
    let focused = block_on(plugin.instruction(info(Some("the parser"), false, 0))).unwrap();
    assert!(focused.contains("\nFocus: the parser\n"), "{focused}");
}

#[test]
fn accept_refuses_a_summary_missing_a_section_with_wisps_sentence() {
    let plugin = instantiate(Arc::new(FakeHost::default()));
    assert_eq!(
        block_on(plugin.accept(info(None, false, 0), WRITTEN.to_owned())),
        Ok(())
    );
    let without = WRITTEN.replace("## Decisions\n", "## Notes\n");
    assert_eq!(
        block_on(plugin.accept(info(None, false, 0), without)),
        Err(
            "The summary has no \u{201c}Decisions\u{201d} section, so Wisp kept the conversation as it was."
                .to_owned()
        )
    );
}

#[test]
fn verbatim_cuts_by_bytes_at_a_character_boundary() {
    let text = format!("ab{}", "é".repeat(10));
    let lines = verbatim(&[said(4, &text, Some(9))], 1);
    assert_eq!(
        lines,
        ["Turn 4: ab [cut here; the whole message is record 9 in the session log]"]
    );
    let lines = verbatim(&[said(4, &text, None)], 2);
    assert_eq!(lines, ["Turn 4: abéé [cut here]"]);
}

#[test]
fn verbatim_keeps_the_newest_messages_first_within_the_budget() {
    let all = [
        said(1, "old", None),
        said(2, "newer", None),
        said(3, "newest", None),
    ];
    assert_eq!(verbatim(&all, 4), ["Turn 3: newest", "Turn 2: newer"]);
    assert!(verbatim(&all, 0).is_empty());
}

#[test]
fn the_running_line_skips_ended_and_restored_processes() {
    let mut ended = job(2, "make", 0);
    ended.end = Some(JobEnd::Stopped(StoppedBy::Model));
    let mut restored = job(3, "tail -f log", 0);
    restored.restored = true;
    let jobs = [
        job(1, "sleep 60", 0),
        ended,
        restored,
        job(4, "npm run dev", 2),
    ];
    assert_eq!(
        running_line(&jobs).as_deref(),
        Some("Running processes: 1 (sleep 60), 4 (npm run dev, 2 unread lines).")
    );
    assert_eq!(running_line(&[]), None);
}

#[test]
fn the_head_lays_out_the_summary_messages_calls_running_line_and_pointers() {
    let host = FakeHost {
        jobs: vec![job(1, "sleep 60", 0)],
        ..FakeHost::default()
    };
    let plugin = instantiate(Arc::new(host));
    let view = Viewed {
        said: vec![said(1, "hello", Some(3))],
        calls: vec![CallEntry {
            turn: 1,
            tool: "bash".to_owned(),
            target: "ls".to_owned(),
            status: Some("exit 0".to_owned()),
            cwd: Some("~/w".to_owned()),
            saved: None,
            attached: vec!["image/png".to_owned()],
            withdrawn: false,
        }],
    };
    let mut written = info(None, false, 8_000);
    written.cut_off = Some("half a reply".to_owned());
    let texts = block_on(plugin.head(written, &view, Some("The summary.\n".to_owned())));
    assert_eq!(
        texts,
        [format!(
            "<compacted_context epoch=\"2\" turns=\"1\u{2013}3\">\n<summary>\nThe summary.\n</summary>\n<user_messages>\nThe user's recent messages, word for word, newest first:\nTurn 1: hello\n</user_messages>\n<tool_calls>\nThe last tool calls, oldest first:\n- turn 1: bash ls \u{b7} exit 0 \u{b7} in ~/w \u{b7} attached image/png\n{ATTACHED_LEFT}\n</tool_calls>\n<cut_off_reply>\n{CUT_OFF_INTRO}\nhalf a reply\n</cut_off_reply>\nRunning processes: 1 (sleep 60).\n<pointers>\nThe full earlier conversation can be read in the session log, s1.jsonl in Wisp's sessions folder.\n</pointers>\n</compacted_context>\n"
        )]
    );
    let native = block_on(plugin.head(info(None, true, 0), &view, None));
    assert!(!native[0].contains("<summary>"), "{native:?}");
    assert!(!native[0].contains("<user_messages>"), "{native:?}");
}

#[test]
fn pointers_name_what_there_is() {
    assert_eq!(pointers(None, None), None);
    assert_eq!(
        pointers(None, Some("~/out".to_owned())).as_deref(),
        Some("Every tool output is saved under ~/out, where it can be read.")
    );
    assert_eq!(
        pointers(Some("~/s.jsonl".to_owned()), Some("~/out".to_owned())).as_deref(),
        Some(
            "The full earlier conversation can be read in the session log, ~/s.jsonl, and every tool output under ~/out."
        )
    );
}
