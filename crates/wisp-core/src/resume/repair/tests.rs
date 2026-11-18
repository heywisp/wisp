use wisp_protocol::log::LOG_SCHEMA_VERSION;
use wisp_protocol::{NotRunReason, QuestionOption};

use super::*;

fn record(event: LogEvent) -> LogRecord {
    LogRecord {
        schema_version: LOG_SCHEMA_VERSION,
        seq: 0,
        at: "2026-10-26T09:00:00Z".into(),
        event,
    }
}

fn started(call_id: &str) -> LogRecord {
    record(LogEvent::ToolCallStarted {
        turn: 1,
        call_id: call_id.into(),
        cwd: "~/work".into(),
        movable: false,
        file: None,
    })
}

fn output(tail: &str, bytes: u64, file: Option<&str>) -> ToolOutputView {
    ToolOutputView {
        tail: tail.into(),
        bytes,
        lines: u64::try_from(tail.lines().count()).unwrap(),
        file: file.map(str::to_owned),
        file_problem: None,
        inlined: u64::try_from(tail.len()).unwrap() == bytes,
    }
}

fn finished(
    call_id: &str,
    outcome: ToolOutcome,
    output: ToolOutputView,
    result: Option<&str>,
    notes: Option<Vec<&str>>,
) -> LogRecord {
    record(LogEvent::ToolCallFinished {
        turn: 1,
        call_id: call_id.into(),
        outcome,
        output,
        facts: Vec::new(),
        state: None,
        duration_ms: 10,
        result: result.map(str::to_owned),
        process: None,
        attachments: Vec::new(),
        notes: notes.map(|n| n.into_iter().map(str::to_owned).collect()),
        typed: None,
    })
}

fn exit(code: i32) -> ToolOutcome {
    ToolOutcome::Completed {
        is_error: code != 0,
        exit: Some(ExitStatus::Code { code }),
    }
}

fn repaired_with(records: &[LogRecord], tool: &str, file_exists: bool) -> RepairedCall {
    let exists = move |_: &str| file_exists;
    let saves_output = |name: &str| name != "quiet";
    let context = Context {
        home: Some(Path::new("/home/me")),
        exists: &exists,
        saves_output: &saves_output,
    };
    let calls = [("c1".to_owned(), tool.to_owned())];
    repair_calls(records, &calls, &context).remove(0)
}

fn repaired(records: &[LogRecord]) -> RepairedCall {
    repaired_with(records, "bash", true)
}

fn questions() -> Vec<Question> {
    vec![Question {
        header: "Plan".into(),
        question: "Which plan?".into(),
        options: vec![
            QuestionOption {
                label: "Keep".into(),
                description: "Carry on.".into(),
            },
            QuestionOption {
                label: "Change".into(),
                description: "Rethink.".into(),
            },
        ],
        multi_select: false,
        source: None,
    }]
}

#[test]
fn a_call_that_never_started_is_not_run_and_one_that_never_finished_is_unknown() {
    let never = repaired(&[]);
    assert_eq!((never.text.as_str(), never.is_error), (NOT_RUN, true));
    let cut = repaired(&[started("c1")]);
    assert_eq!((cut.text.as_str(), cut.is_error), (UNKNOWN, true));
    assert!(!cut.interrupted_question);
    let other = repaired(&[started("c2")]);
    assert_eq!(other.text, NOT_RUN, "another call's records don't count");
}

#[test]
fn a_logged_result_is_sent_verbatim() {
    let call = repaired(&[
        started("c1"),
        finished("c1", exit(1), output("", 0, None), Some("own words"), None),
    ]);
    assert_eq!((call.text.as_str(), call.is_error), ("own words", true));
    let hook = record(LogEvent::HookApplied {
        hook: "test/wrap".into(),
        point: AFTER_TOOL.into(),
        turn: Some(1),
        step: Some(1),
        call_id: Some("c1".into()),
        input_id: None,
        change: HookChange::Result {
            before: "old".into(),
            after: "hooked".into(),
        },
    });
    let call = repaired(&[
        started("c1"),
        hook,
        finished(
            "c1",
            exit(0),
            output("", 0, None),
            Some("final [redacted]"),
            Some(vec![HELD_KEY_NOTE]),
        ),
    ]);
    assert_eq!(call.text, with_held_key_note("final [redacted]"));
}

#[test]
fn logged_notes_rebuild_the_exact_text() {
    let notes = vec![
        "Working directory: ~/work/sub",
        "Working directory not tracked: gone",
    ];
    let whole = output("ok\n", 3, Some("/home/me/out/0001-bash.out"));
    let call = repaired(&[
        started("c1"),
        finished("c1", exit(0), whole.clone(), None, Some(notes.clone())),
    ]);
    let captured = Captured {
        head: "ok\n".into(),
        tail: "ok\n".into(),
        bytes: 3,
        lines: 1,
        path: None,
        problem: None,
    };
    let owned: Vec<String> = notes.iter().map(|n| (*n).to_owned()).collect();
    assert_eq!(call.text, result_text(&exit(0), &captured, None, &owned));
    assert!(
        call.text
            .ends_with("\nWorking directory: ~/work/sub\nWorking directory not tracked: gone")
    );

    let not_run = ToolOutcome::NotRun {
        reason: NotRunReason::TurnStopped,
    };
    let call = repaired(&[finished(
        "c1",
        not_run.clone(),
        output("", 0, None),
        None,
        Some(vec![]),
    )]);
    assert_eq!(call.text, not_run_text(&NotRunReason::TurnStopped));
    let invalid = ToolOutcome::InvalidInput {
        message: "`ms` must be a whole number".into(),
    };
    let call = repaired(&[finished(
        "c1",
        invalid,
        output("", 0, None),
        None,
        Some(vec![]),
    )]);
    assert_eq!(call.text, "Invalid input: `ms` must be a whole number");
    let failed = ToolOutcome::Failed {
        message: "the shell is gone".into(),
        before_start: false,
    };
    let call = repaired(&[finished(
        "c1",
        failed,
        output("", 0, None),
        None,
        Some(vec!["n"]),
    )]);
    assert_eq!(call.text, "Failed: the shell is gone\nn");
}

#[test]
fn an_inlined_output_longer_than_its_logged_tail_is_recovered() {
    let mut view = output("line 900\n", 10_000, Some("/home/me/out/0004-bash.out"));
    view.inlined = true;
    let call = repaired(&[
        started("c1"),
        finished("c1", exit(0), view, None, Some(vec![])),
    ]);
    assert_eq!(
        call.text,
        "Recovered after Wisp stopped: the command exited with code 0, but its result wasn't saved.\nLast lines of its output:\nline 900\nFull output: ~/out/0004-bash.out"
    );
}

#[test]
fn a_failure_without_output_is_exact_for_a_tool_that_saves_none() {
    let failed = ToolOutcome::Failed {
        message: "no answer".into(),
        before_start: false,
    };
    let call = repaired_with(
        &[
            started("c1"),
            finished("c1", failed, output("", 0, None), None, Some(vec!["n"])),
        ],
        "quiet",
        true,
    );
    assert_eq!(
        (call.text.as_str(), call.is_error),
        ("Failed: no answer\nn", true)
    );
}

#[test]
fn output_cut_to_a_tail_is_recovered_with_its_last_lines() {
    let long = output(
        "line 811\nline 812\n",
        40_000,
        Some("/home/me/out/0003-bash.out"),
    );
    let call = repaired(&[
        started("c1"),
        finished("c1", exit(2), long.clone(), None, Some(vec!["note"])),
    ]);
    assert_eq!(
        call.text,
        "Recovered after Wisp stopped: the command exited with code 2, but its result wasn't saved.\nLast lines of its output:\nline 811\nline 812\nnote\nFull output: ~/out/0003-bash.out"
    );
    assert!(call.is_error);
    let gone = repaired_with(
        &[started("c1"), finished("c1", exit(2), long, None, None)],
        "bash",
        false,
    );
    assert_eq!(
        gone.text,
        "Recovered after Wisp stopped: the command exited with code 2, but its result wasn't saved.\nLast lines of its output:\nline 811\nline 812",
        "a file that is gone isn't cited"
    );
}

#[test]
fn an_older_record_without_notes_is_recovered_whatever_its_outcome() {
    let recovered = |outcome: ToolOutcome, view: ToolOutputView| {
        repaired(&[started("c1"), finished("c1", outcome, view, None, None)]).text
    };
    assert_eq!(
        recovered(exit(0), output("", 0, None)),
        "Recovered after Wisp stopped: the command exited with code 0, but its result wasn't saved.\nIt printed no output."
    );
    assert_eq!(
        recovered(
            ToolOutcome::TimedOut { after_ms: 2_000 },
            output("tick\n", 5, None)
        ),
        "Recovered after Wisp stopped: the call timed out after 2 s and was stopped.\nLast lines of its output:\ntick"
    );
    assert_eq!(
        recovered(
            ToolOutcome::Failed {
                message: "no shell".into(),
                before_start: false,
            },
            output("", 0, None)
        ),
        "Recovered after Wisp stopped: the call failed: no shell"
    );
    assert_eq!(
        recovered(
            ToolOutcome::InvalidInput {
                message: "bad".into()
            },
            output("", 0, None)
        ),
        "Recovered after Wisp stopped: the input was rejected: bad"
    );
    assert_eq!(
        recovered(ToolOutcome::Cancelled, output("half\n", 5, None)),
        "Recovered after Wisp stopped: the call was stopped by the user.\nLast lines of its output:\nhalf"
    );
    let quiet = repaired_with(
        &[
            started("c1"),
            finished("c1", exit(0), output("x\n", 2, None), None, None),
        ],
        "quiet",
        true,
    );
    assert_eq!(
        quiet.text,
        "Recovered after Wisp stopped: the command exited with code 0, but its result wasn't saved.",
        "a tool that keeps no output shows none"
    );
}

#[test]
fn an_open_question_is_interrupted_and_a_closed_one_keeps_its_answer() {
    let asked = record(LogEvent::QuestionAsked {
        turn: 1,
        call_id: "c1".into(),
        questions: questions(),
    });
    let open = repaired_with(&[started("c1"), asked.clone()], "ask_user", true);
    assert_eq!(open.text, INTERRUPTED);
    assert!(open.is_error && open.interrupted_question);

    let closed = record(LogEvent::QuestionClosed {
        turn: 1,
        call_id: "c1".into(),
        how: QuestionEnd::Stopped,
    });
    let stopped = repaired_with(&[started("c1"), asked.clone(), closed], "ask_user", true);
    assert_eq!(
        stopped.text,
        question::result_text(&questions(), &QuestionEnd::Stopped)
    );
    assert!(stopped.is_error && !stopped.interrupted_question);

    let older = record(LogEvent::QuestionClosed {
        turn: 1,
        call_id: "c1".into(),
        how: QuestionEnd::MessageInstead,
    });
    let instead = repaired_with(&[started("c1"), asked.clone(), older], "ask_user", true);
    assert_eq!(instead.text, question::MESSAGE_INSTEAD);
    assert!(!instead.is_error && !instead.interrupted_question);
    let answered = record(LogEvent::QuestionClosed {
        turn: 1,
        call_id: "c1".into(),
        how: QuestionEnd::Answered {
            answers: vec![wisp_protocol::Answer::own("Earlier answer")],
        },
    });
    let later = repaired_with(
        &[started("c1"), asked.clone(), answered, asked],
        "ask_user",
        true,
    );
    assert_eq!(later.text, INTERRUPTED);
    assert!(later.interrupted_question);
}

#[test]
fn a_call_cut_short_after_it_became_a_process_names_the_process() {
    let process: LogEvent = serde_json::from_value(serde_json::json!({
        "type": "process_started",
        "turn": 1,
        "id": 4,
        "call_id": "c1",
        "reason": "started",
        "tty": false,
        "stdin": false,
    }))
    .unwrap();
    let cut = repaired(&[started("c1"), record(process)]);
    assert_eq!(
        (cut.text.as_str(), cut.is_error),
        (
            "Unknown: Wisp stopped while this call was running; its effects are unknown. It was running as process 4.",
            true
        )
    );
}

#[test]
fn an_unfinished_server_card_never_repairs_its_answer_into_the_model() {
    for asked_by in [
        wisp_protocol::AskedBy::Server {
            name: "linear".into(),
        },
        wisp_protocol::AskedBy::Unknown,
    ] {
        let mut questions = questions();
        questions[0].source = Some(wisp_protocol::QuestionSource {
            asked_by,
            typed: true,
        });
        let asked = record(LogEvent::QuestionAsked {
            turn: 1,
            call_id: "c1".into(),
            questions,
        });
        let open = repaired_with(&[started("c1"), asked.clone()], "mcp__linear__echo", true);
        assert_eq!(open.text, UNKNOWN);
        assert!(open.interrupted_question);
        let closed = record(LogEvent::QuestionClosed {
            turn: 1,
            call_id: "c1".into(),
            how: QuestionEnd::Answered {
                answers: vec![wisp_protocol::Answer::own("Private typed words")],
            },
        });
        let answered = repaired_with(
            &[started("c1"), asked.clone(), closed.clone()],
            "mcp__linear__echo",
            true,
        );
        assert_eq!(answered.text, UNKNOWN);
        assert!(!answered.interrupted_question);
        let later = repaired_with(
            &[started("c1"), asked.clone(), closed, asked],
            "mcp__linear__echo",
            true,
        );
        assert_eq!(later.text, UNKNOWN);
        assert!(later.interrupted_question);
    }
}
