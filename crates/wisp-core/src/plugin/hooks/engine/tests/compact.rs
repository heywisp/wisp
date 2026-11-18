use wisp_plugin_api::hooks::{CompactInfo, HeadInfo};

use super::*;
use crate::plugin::hooks::points::{
    BEFORE_COMPACT, COMPACTION_HEAD, MAX_APPEND_CHARS, MAX_HEAD_CHARS, MAX_INSTRUCTION_CHARS,
};

fn compacting(instruction: &str) -> Fire {
    let event = Event::BeforeCompact(CompactInfo {
        trigger: "auto".to_owned(),
        engine: "native".to_owned(),
        turn: Some(2),
        focus: None,
        estimate: 9_000,
        point: Some(8_000),
        epoch: 0,
        requested_by: None,
        instruction: instruction.to_owned(),
    });
    Fire::new(BEFORE_COMPACT, What::Event(event), At::default())
}

fn heading(room: Option<u64>) -> Fire {
    let event = Event::CompactionHead(HeadInfo {
        epoch: 1,
        trigger: "manual".to_owned(),
        engine: "native".to_owned(),
        first_turn: 1,
        last_turn: 2,
    });
    Fire::new(COMPACTION_HEAD, What::Event(event), At::default()).within(room)
}

fn by_hook(replies: Vec<(&'static str, Reply)>) -> Script {
    Arc::new(move |_, hook, _| {
        let reply = replies
            .iter()
            .find(|(name, _)| *name == hook)
            .map_or(Reply::None, |(_, r)| r.clone());
        Box::pin(async move { reply })
    })
}

fn chain(names: &[&str], point: &str, extra: &str) -> String {
    let mut body = String::new();
    let mut last: Option<&str> = None;
    for name in names {
        let after = last.map_or(String::new(), |l| format!("after = [\"aa/{l}\"]\n"));
        body += &hook_body(name, point, &format!("{after}{extra}"));
        last = Some(name);
    }
    body
}

fn seen_instruction(calls: &Calls, hook: &str) -> String {
    match &calls.of(hook)[0].event {
        Event::BeforeCompact(info) => info.instruction.clone(),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn two_chained_rewrites_and_an_append_each_see_the_instruction_as_it_stands() {
    let calls = Calls::default();
    let script: Script = Arc::new(|_, hook, event| {
        let Event::BeforeCompact(info) = event else {
            panic!("{event:?}");
        };
        let reply = match hook.as_str() {
            "three" => Reply::Append(vec!["and three".to_owned()]),
            _ => Reply::Rewrite(format!("{}+{hook}", info.instruction)),
        };
        Box::pin(async move { reply })
    });
    let body = chain(&["one", "two", "three"], BEFORE_COMPACT, "");
    let (eng, _finished) = engine(vec![source("aa", &body, script, &calls)]).await;
    let progress = fired(&eng, compacting("sum up")).await;
    assert_eq!(seen_instruction(&calls, "two"), "sum up+one");
    assert_eq!(seen_instruction(&calls, "three"), "sum up+one+two");
    assert_eq!(progress.value.as_deref(), Some("sum up+one+two\nand three"));
    assert!(progress.rewritten);
    let hooks: Vec<&str> = progress.changes.iter().map(|c| c.hook.as_str()).collect();
    assert_eq!(hooks, ["aa/one", "aa/two", "aa/three"]);
}

#[tokio::test]
async fn an_append_alone_leaves_the_rewrite_flag_off() {
    let calls = Calls::default();
    let body = hook_body("one", BEFORE_COMPACT, "");
    let script = reply(Reply::Append(vec!["keep the plan".to_owned()]));
    let (eng, _finished) = engine(vec![source("aa", &body, script, &calls)]).await;
    let progress = fired(&eng, compacting("sum up")).await;
    assert_eq!(progress.value.as_deref(), Some("sum up\nkeep the plan"));
    assert!(!progress.rewritten);
}

#[tokio::test]
async fn each_instruction_rule_rejects_the_reply_and_keeps_the_instruction() {
    let long = "x".repeat(MAX_INSTRUCTION_CHARS + 1);
    let near = "x".repeat(MAX_INSTRUCTION_CHARS - 10);
    let cases = [
        (Reply::Rewrite("  \n".to_owned()), "sum up", "blank"),
        (Reply::Rewrite(long), "sum up", "too_large"),
        (
            Reply::Append(vec!["y".repeat(MAX_APPEND_CHARS + 1)]),
            "sum up",
            "too_large",
        ),
        (
            Reply::Append(vec!["0123456789".to_owned()]),
            near.as_str(),
            "too_large",
        ),
    ];
    for (reply_with, instruction, rule) in cases {
        let calls = Calls::default();
        let body = hook_body("one", BEFORE_COMPACT, "");
        let (eng, _finished) = engine(vec![source("aa", &body, reply(reply_with), &calls)]).await;
        let progress = fired(&eng, compacting(instruction)).await;
        assert_eq!(progress.value.as_deref(), Some(instruction));
        assert!(progress.changes.is_empty());
        assert_eq!(eng.take_records()[0].outcome, rejected(rule));
    }
}

#[tokio::test(start_paused = true)]
async fn on_failure_block_vetoes_a_compaction_on_a_failure_and_a_timeout() {
    for (script, outcome) in [(panicking(), "failed"), (sleeping(), "timed_out")] {
        let calls = Calls::default();
        let body = hook_body(
            "h",
            BEFORE_COMPACT,
            "on_failure = \"block\"\ntimeout_ms = 10\n",
        );
        let (eng, _finished) = engine(vec![source("aa", &body, script, &calls)]).await;
        let progress = fired(&eng, compacting("sum up")).await;
        assert_eq!(progress.blocked.map(|b| b.0).as_deref(), Some("aa/h"));
        let records = eng.take_records();
        assert_eq!(records.len(), 1, "{outcome}");
    }
}

#[tokio::test(start_paused = true)]
async fn at_the_ceiling_no_veto_stands_and_the_later_hooks_still_run() {
    let calls = Calls::default();
    let script: Script = Arc::new(|_, hook, _| {
        Box::pin(async move {
            match hook.as_str() {
                "stop" => Reply::Block("not now".to_owned()),
                "boom" => panic!("hook payload"),
                "slow" => {
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                    Reply::None
                }
                _ => Reply::Append(vec!["still asked".to_owned()]),
            }
        })
    });
    let body = chain(
        &["stop", "boom", "slow", "last"],
        BEFORE_COMPACT,
        "on_failure = \"block\"\ntimeout_ms = 10\n",
    );
    let (eng, _finished) = engine(vec![source("aa", &body, script, &calls)]).await;
    let progress = fired(&eng, compacting("sum up").at_ceiling(true)).await;
    assert_eq!(progress.blocked, None);
    assert_eq!(progress.value.as_deref(), Some("sum up\nstill asked"));
    let records = eng.take_records();
    let outcomes: Vec<(String, HookOutcome)> = outcomes(&records);
    assert_eq!(outcomes[0], ("aa/stop".to_owned(), rejected("ceiling")));
    assert!(matches!(outcomes[1].1, HookOutcome::Failed { .. }));
    assert_eq!(outcomes[2].1, HookOutcome::TimedOut);
    assert_eq!(outcomes[3].1, HookOutcome::Applied);
    assert_eq!(calls.count("last"), 1);

    let calls = Calls::default();
    let body = chain(
        &["stop", "last"],
        BEFORE_COMPACT,
        "on_failure = \"block\"\ntimeout_ms = 10\n",
    );
    let script = by_hook(vec![("stop", Reply::Block("not now".to_owned()))]);
    let (eng, _finished) = engine(vec![source("aa", &body, script, &calls)]).await;
    let progress = fired(&eng, compacting("sum up")).await;
    assert_eq!(
        progress.blocked,
        Some(("aa/stop".to_owned(), "not now".to_owned()))
    );
    assert_eq!(calls.count("last"), 0);
}

#[tokio::test]
async fn a_head_text_is_kept_while_the_head_fits_its_room_and_the_cap() {
    let calls = Calls::default();
    let body = chain(&["one", "two"], COMPACTION_HEAD, "");
    let script = by_hook(vec![
        ("one", Reply::Append(vec!["a".repeat(40)])),
        ("two", Reply::Append(vec!["b".repeat(40)])),
    ]);
    let (eng, _finished) = engine(vec![source("aa", &body, script, &calls)]).await;
    let progress = fired(&eng, heading(Some(15))).await;
    assert_eq!(progress.head, ["a".repeat(40)]);
    let records = eng.take_records();
    assert_eq!(records[0].outcome, HookOutcome::Applied);
    assert_eq!(records[1].outcome, rejected("not_smaller"));

    let calls = Calls::default();
    let body = chain(&["one", "two", "three"], COMPACTION_HEAD, "");
    let script = by_hook(vec![
        ("one", Reply::Append(vec!["a".repeat(MAX_APPEND_CHARS)])),
        ("two", Reply::Append(vec!["b".repeat(MAX_APPEND_CHARS)])),
        ("three", Reply::Append(vec!["c".to_owned()])),
    ]);
    let (eng, _finished) = engine(vec![source("aa", &body, script, &calls)]).await;
    let progress = fired(&eng, heading(None)).await;
    let kept: usize = progress.head.iter().map(String::len).sum();
    assert_eq!(kept, MAX_HEAD_CHARS);
    let records = eng.take_records();
    assert_eq!(records[2].outcome, rejected("too_large"));

    let calls = Calls::default();
    let body = hook_body("one", COMPACTION_HEAD, "");
    let script = reply(Reply::Append(vec!["a".repeat(MAX_APPEND_CHARS + 1)]));
    let (eng, _finished) = engine(vec![source("aa", &body, script, &calls)]).await;
    let progress = fired(&eng, heading(None)).await;
    assert!(progress.head.is_empty());
    assert_eq!(eng.take_records()[0].outcome, rejected("too_large"));
}

#[tokio::test]
async fn the_doctor_notes_a_long_before_compact_timeout_and_what_a_block_does_there() {
    let calls = Calls::default();
    let body = hook_body("gate", BEFORE_COMPACT, "timeout_ms = 10000\n")
        + &hook_body(
            "veto",
            BEFORE_COMPACT,
            "timeout_ms = 10000\non_failure = \"block\"\n",
        )
        + &hook_body("quick", BEFORE_COMPACT, "timeout_ms = 1000\n");
    let (eng, _finished) = engine(vec![source("aa", &body, hooks::none(), &calls)]).await;
    let problems: Vec<String> = eng
        .table()
        .problems()
        .into_iter()
        .map(|p| p.message)
        .collect();
    assert_eq!(
        problems,
        [
            "its hook `aa/gate` at `before-compact` also runs outside a turn, before a background compaction, where its timeout is cut to 2 s",
            "its hook `aa/veto` at `before-compact` also runs outside a turn, before a background compaction, where its timeout is cut to 2 s, and a timeout there starts no background compaction",
        ]
    );
}

#[tokio::test]
async fn a_head_text_that_reaches_the_room_exactly_is_rejected_and_one_token_less_is_kept() {
    let text = "a".repeat(40);
    let tokens = crate::context::text_tokens(&text);
    for (room, kept) in [(tokens, false), (tokens + 1, true)] {
        let calls = Calls::default();
        let body = hook_body("one", COMPACTION_HEAD, "");
        let script = reply(Reply::Append(vec![text.clone()]));
        let (eng, _finished) = engine(vec![source("aa", &body, script, &calls)]).await;
        let progress = fired(&eng, heading(Some(room))).await;
        assert_eq!(progress.head.is_empty(), !kept, "{room}");
        let expected = if kept {
            HookOutcome::Applied
        } else {
            rejected("not_smaller")
        };
        assert_eq!(eng.take_records()[0].outcome, expected, "{room}");
    }
}
