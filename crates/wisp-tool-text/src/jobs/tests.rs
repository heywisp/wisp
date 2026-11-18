use wisp_plugin_api::host::{JobEnd, LostReason, StoppedBy};
use wisp_plugin_api::prompt::EndedJob;
use wisp_plugin_api::types::ExitStatus;

use super::*;

#[test]
fn a_missing_process_names_its_id() {
    assert_eq!(no_process(9), "there is no process 9");
    assert_eq!(no_process(0), "there is no process 0");
    assert_eq!(no_process(-2), "there is no process -2");
}

#[test]
fn each_end_has_its_sentence_and_its_words() {
    let table = [
        (
            JobEnd::Exited(ExitStatus::Code(2)),
            "Process 4 exited with code 2.",
            "exited with code 2",
        ),
        (
            JobEnd::Exited(ExitStatus::Signal(9)),
            "Process 4 was ended by signal 9.",
            "ended by signal 9",
        ),
        (
            JobEnd::Stopped(StoppedBy::Model),
            "Process 4 was stopped.",
            "stopped",
        ),
        (
            JobEnd::Stopped(StoppedBy::User),
            "Process 4 was stopped by the user.",
            "stopped by the user",
        ),
        (
            JobEnd::Stopped(StoppedBy::SessionEnded),
            "Process 4 was stopped when the session ended.",
            "stopped at the session's end",
        ),
        (
            JobEnd::Stopped(StoppedBy::TimeLimit(1500)),
            "Process 4 was stopped at its 1,500 ms time limit.",
            "stopped at its 1,500 ms limit",
        ),
        (
            JobEnd::Lost(LostReason::Gone),
            "Process 4 ended when Wisp restarted.",
            "ended when Wisp restarted",
        ),
        (
            JobEnd::Failed("no such file".into()),
            "Process 4 failed: no such file.",
            "failed",
        ),
    ];
    for (end, sentence, words) in table {
        assert_eq!(ended_sentence(4, &end), sentence);
        assert_eq!(state_sentence(4, Some(&end)), sentence);
        assert_eq!(state_words(Some(&end)), words);
        assert_eq!(state_words_for(Some(&end), true), words);
    }
    assert_eq!(state_sentence(4, None), "Process 4 is running.");
    assert_eq!(state_words(None), "running");
    assert_eq!(state_words_for(None, true), "running in a terminal");
    assert_eq!(state_words_for(None, false), "running");
}

#[test]
fn an_end_line_names_the_target_cut_and_counts_unread_lines() {
    let end = JobEnd::Exited(ExitStatus::Code(0));
    assert_eq!(
        end_line(3, "make test", &end, 0),
        "Process 3 (make test) exited with code 0."
    );
    assert_eq!(
        end_line(3, "make test", &end, 1_200),
        "Process 3 (make test) exited with code 0; 1,200 lines you haven't read."
    );
    let long = "x".repeat(80);
    let line = end_line(3, &long, &end, 1);
    assert!(
        line.starts_with(&format!("Process 3 ({}…)", "x".repeat(59))),
        "{line}"
    );
    assert!(line.ends_with("; 1 line you haven't read."), "{line}");
}

#[test]
fn a_typed_line_names_its_process() {
    assert_eq!(
        typed_line(7),
        "You typed into process 7 (the user attached to it)."
    );
}

#[test]
fn an_end_before_the_restart_names_its_command_and_the_restart() {
    let lost = |id, target: Option<&str>, end| {
        lost_line(&LostJob {
            id,
            target: target.map(str::to_owned),
            end,
        })
    };
    assert_eq!(
        lost(4, Some("cargo test"), JobEnd::Exited(ExitStatus::Code(1))),
        "Process 4 (cargo test) exited with code 1 before Wisp restarted."
    );
    assert_eq!(
        lost(2, None, JobEnd::Stopped(StoppedBy::Model)),
        "Process 2 was stopped before Wisp restarted."
    );
    assert_eq!(
        lost(3, Some("make"), JobEnd::Failed("no such file.".into())),
        "Process 3 (make) failed before Wisp restarted: no such file."
    );
    assert_eq!(
        lost(5, Some("server"), JobEnd::Lost(LostReason::Gone)),
        kept_gone_line(5, Some("server"))
    );
}

#[test]
fn each_news_item_has_its_sentence() {
    let kept = |id| KeptJob {
        id,
        log: format!("~/logs/{id}.log"),
    };
    let cases = [
        (
            Item::Restarted(vec![1]),
            "Process 1 was still running when Wisp exited, so it has stopped; its output file remains if it still exists.",
        ),
        (
            Item::Restarted(vec![1, 2, 3]),
            "Processes 1, 2 and 3 were still running when Wisp exited, so they have stopped; their output files remain where the files still exist.",
        ),
        (
            Item::Kept(vec![kept(4)]),
            "Process 4 keeps running from before the restart (started with keep_running); its output goes to ~/logs/4.log.",
        ),
        (
            Item::Kept(vec![kept(4), kept(5)]),
            "Processes 4 and 5 keep running from before the restart (started with keep_running); their output goes to their logs, listed by the process tool.",
        ),
        (
            Item::Unchecked(vec![6]),
            "Process 6 was still running when Wisp exited, and Wisp can't check here whether it still runs, so it wasn't stopped; its output file remains if it still exists.",
        ),
        (
            Item::Unchecked(vec![6, 7]),
            "Processes 6 and 7 were still running when Wisp exited, and Wisp can't check here whether they still run, so they weren't stopped; their output files remain where the files still exist.",
        ),
        (
            Item::Typed(8),
            "You typed into process 8 (the user attached to it).",
        ),
        (
            Item::Rewound(RewoundJob {
                id: 9,
                target: "exec sleep 30".to_owned(),
            }),
            "Process 9 (exec sleep 30) is running; it was started in turns the user rewound.",
        ),
    ];
    for (item, line) in cases {
        assert_eq!(item_line(&item).as_deref(), Some(line));
    }
    let ended = EndedJob {
        id: 3,
        target: "make".to_owned(),
        end: JobEnd::Exited(ExitStatus::Code(0)),
        unread: 2,
    };
    assert_eq!(
        item_line(&Item::Ended(ended.clone())),
        Some(end_line(3, "make", &ended.end, 2))
    );
    for empty in [
        Item::Restarted(Vec::new()),
        Item::Kept(Vec::new()),
        Item::Unchecked(Vec::new()),
    ] {
        assert_eq!(item_line(&empty), None);
    }
}

#[test]
fn a_rewound_line_cuts_a_long_target() {
    let line = rewound_line(&RewoundJob {
        id: 1,
        target: "x".repeat(200),
    });
    assert_eq!(
        line,
        format!(
            "Process 1 ({}) is running; {STARTED_IN_REWOUND_TURNS}.",
            cut_chars(&"x".repeat(200), LIST_TARGET_CHARS)
        )
    );
}
