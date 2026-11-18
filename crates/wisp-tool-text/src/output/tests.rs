use wisp_plugin_api::types::BackgroundReason;

use super::*;

#[test]
fn numbers_are_grouped() {
    assert_eq!(group(0), "0");
    assert_eq!(group(999), "999");
    assert_eq!(group(1000), "1,000");
    assert_eq!(group(98_765), "98,765");
    assert_eq!(group(1_234_567), "1,234,567");
    assert_eq!(count(1, "line"), "1 line");
    assert_eq!(count(1234, "line"), "1,234 lines");
    assert_eq!(seconds(120_000), "120 s");
    assert_eq!(seconds(1500), "1,500 ms");
}

#[test]
fn text_is_cut_by_characters_with_an_ellipsis() {
    assert_eq!(cut_chars("short", 10), "short");
    assert_eq!(cut_chars("ééééé", 5), "ééééé");
    assert_eq!(cut_chars("éééééé", 5), "éééé…");
}

fn small() -> Captured {
    Captured {
        head: "partial\n".into(),
        tail: "partial\n".into(),
        bytes: 8,
        lines: 1,
        path: None,
        problem: None,
    }
}

#[test]
fn each_outcome_has_its_result_text() {
    assert_eq!(invalid_input_text("bad"), "Invalid input: bad");
    let captured = small();
    let text = |outcome: Outcome| result_text(&outcome, &captured, None, &["Note.".to_owned()]);
    assert_eq!(
        text(Outcome::Completed(Completed {
            is_error: false,
            exit: Some(ExitStatus::Code(3)),
        })),
        "Exit code: 3\nOutput:\npartial\nNote."
    );
    assert_eq!(
        text(Outcome::Completed(Completed {
            is_error: false,
            exit: None,
        })),
        "Output:\npartial\nNote."
    );
    assert_eq!(
        text(Outcome::TimedOut(120_000)),
        "Output:\npartial\nTimed out after 120 s; the call was stopped.\nNote."
    );
    assert_eq!(
        text(Outcome::Cancelled),
        "Output:\npartial\n[stopped by the user]\nNote."
    );
    assert_eq!(
        text(Outcome::InvalidInput("bad".into())),
        "Output:\npartial\nNote."
    );
    assert_eq!(
        text(Outcome::Failed(Failure {
            message: "it broke".into(),
            before_start: false,
        })),
        "Failed: it broke\nOutput:\npartial\nNote."
    );
    assert_eq!(
        text(Outcome::Backgrounded(BackgroundReason::Started)),
        "Output:\npartial\nNote."
    );
    let empty = Captured::default();
    assert_eq!(
        result_text(
            &Outcome::Failed(Failure {
                message: "spawn failed".into(),
                before_start: true,
            }),
            &empty,
            None,
            &[]
        ),
        "Failed: spawn failed"
    );
}

#[test]
fn the_held_key_note_is_exactly_one_final_line() {
    let text = format!("body\n{HELD_KEY_NOTE}\r\njob ended\n{HELD_KEY_NOTE}");
    let expected = format!("body\njob ended\n{HELD_KEY_NOTE}");
    assert_eq!(with_held_key_note(&text), expected);
    assert_eq!(with_held_key_note(&expected), expected);
    assert_eq!(without_held_key_note(&text), "body\njob ended");
    assert_eq!(with_held_key_note(""), HELD_KEY_NOTE);
}

#[test]
fn a_result_with_a_process_points_at_the_process_tool() {
    let captured = small();
    let style = ResultStyle {
        time_limit: true,
        process: Some(5),
        terminal: false,
    };
    assert_eq!(
        result_text_with(&Outcome::TimedOut(2000), &captured, None, &[], style),
        format!(
            "Output:\npartial\n{}\nUse the process tool with id 5 to read or search this output.",
            time_limit_text(2000)
        )
    );
    let terminal = ResultStyle {
        terminal: true,
        ..style
    };
    assert!(
        result_text_with(&Outcome::Cancelled, &captured, None, &[], terminal)
            .ends_with("to read or search this output or show its final screen.")
    );
}

#[test]
fn the_tools_doc_states_the_output_limits() {
    let docs = include_str!("../../../../docs/tools.md");
    let limits = format!(
        "up to {} KiB of cleaned output is put in the result whole. Larger output gets a head of at most {} KiB, a tail of at most {} KiB,",
        INLINE_LIMIT / 1024,
        HEAD_LIMIT / 1024,
        TAIL_LIMIT / 1024
    );
    assert!(docs.contains(&limits), "{limits}");
}
