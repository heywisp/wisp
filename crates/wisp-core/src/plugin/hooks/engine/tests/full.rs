use wisp_plugin_api::hooks::{BeforeEnd, ToolCall, ToolResult};

use super::*;
use crate::plugin::hooks::points::{
    AFTER_TOOL, BEFORE_END, BEFORE_TOOL, MAX_APPEND_CHARS, MAX_CONTINUES, USER_INPUT,
};

fn user_input(text: &str, images: u32) -> Fire {
    let event = wisp_plugin_testkit::hooks::user_input(text, images);
    Fire::new(USER_INPUT, What::Event(event), At::default())
}

fn before_tool(input: &str) -> Fire {
    let event = Event::BeforeTool(ToolCall {
        call_id: "c1".to_owned(),
        tool: "echo".to_owned(),
        input: input.to_owned(),
    });
    Fire::new(BEFORE_TOOL, What::Event(event), At::default())
}

fn after_tool(result: &str) -> Fire {
    let event = Event::AfterTool(ToolResult {
        call_id: "c1".to_owned(),
        tool: "echo".to_owned(),
        input: "{}".to_owned(),
        result: result.to_owned(),
        is_error: false,
    });
    Fire::new(AFTER_TOOL, What::Event(event), At::default())
}

fn before_end(continues: u32) -> Fire {
    let event = Event::BeforeEnd(BeforeEnd {
        turn: 1,
        step: 1,
        reply: "done".to_owned(),
        continues,
    });
    Fire::new(BEFORE_END, What::Event(event), At::default())
}

fn suffixing(point: &'static str) -> Script {
    Arc::new(move |_, hook, event| {
        let reply = match event {
            Event::UserInput(input) => Reply::Rewrite(format!("{}+{hook}", input.text)),
            Event::AfterTool(result) => Reply::Rewrite(format!("{}+{hook}", result.result)),
            Event::BeforeTool(call) => {
                let mut value: serde_json::Value = serde_json::from_str(&call.input).unwrap();
                let seen = value["text"].as_str().unwrap_or_default().to_owned();
                value["text"] = serde_json::Value::String(format!("{seen}+{hook}"));
                Reply::Rewrite(value.to_string())
            }
            _ => panic!("{point}"),
        };
        Box::pin(async move { reply })
    })
}

async fn chained(point: &'static str) -> (Arc<Engine>, Calls) {
    let calls = Calls::default();
    let body = hook_body("one", point, "") + &hook_body("two", point, "after = [\"aa/one\"]\n");
    let (engine, _finished) = engine(vec![source("aa", &body, suffixing(point), &calls)]).await;
    (engine, calls)
}

async fn one(point: &str, reply_with: Reply, fire: Fire) -> (Progress, Vec<Ran>) {
    let calls = Calls::default();
    let (engine, _finished) = engine(vec![source(
        "aa",
        &hook_body("h", point, ""),
        reply(reply_with),
        &calls,
    )])
    .await;
    let progress = fired(&engine, fire).await;
    (progress, engine.take_records())
}

#[tokio::test]
async fn two_chained_rewrites_at_each_transform_point_see_each_other() {
    let (engine, calls) = chained(USER_INPUT).await;
    let progress = fired(&engine, user_input("hi", 0)).await;
    assert_eq!(progress.value.as_deref(), Some("hi+one+two"));
    let Event::UserInput(seen) = &calls.of("two")[0].event else {
        panic!();
    };
    assert_eq!(seen.text, "hi+one");
    let changes: Vec<(&str, &str, &str)> = progress
        .changes
        .iter()
        .map(|c| (c.hook.as_str(), c.before.as_str(), c.after.as_str()))
        .collect();
    assert_eq!(
        changes,
        [
            ("aa/one", "hi", "hi+one"),
            ("aa/two", "hi+one", "hi+one+two")
        ]
    );

    let (engine, _) = chained(BEFORE_TOOL).await;
    let progress = fired(&engine, before_tool(r#"{"text":"a"}"#)).await;
    assert_eq!(progress.value.as_deref(), Some(r#"{"text":"a+one+two"}"#));

    let (engine, _) = chained(AFTER_TOOL).await;
    let progress = fired(&engine, after_tool("out")).await;
    assert_eq!(progress.value.as_deref(), Some("out+one+two"));
    assert_eq!(progress.changes.len(), 2);
}

#[tokio::test]
async fn a_rewritten_tool_input_is_canonicalised() {
    let (progress, records) = one(
        BEFORE_TOOL,
        Reply::Rewrite("{ \"b\": 1, \"a\": 2 }".to_owned()),
        before_tool("{}"),
    )
    .await;
    assert_eq!(progress.value.as_deref(), Some(r#"{"a":2,"b":1}"#));
    assert_eq!(records[0].outcome, HookOutcome::Applied);
}

#[tokio::test]
async fn each_rule_rejects_its_reply_and_keeps_the_original() {
    let big = |n: usize| "x".repeat(n);
    let cases: Vec<(&str, Reply, Fire, &str)> = vec![
        (
            USER_INPUT,
            Reply::Rewrite(" \n".into()),
            user_input("typed", 0),
            "blank",
        ),
        (
            USER_INPUT,
            Reply::Rewrite(big(5 + MAX_APPEND_CHARS + 1)),
            user_input("typed", 0),
            "too_large",
        ),
        (
            BEFORE_TOOL,
            Reply::Rewrite("[1]".into()),
            before_tool("{}"),
            "not_json",
        ),
        (
            BEFORE_TOOL,
            Reply::Rewrite("nope".into()),
            before_tool("{}"),
            "not_json",
        ),
        (
            AFTER_TOOL,
            Reply::Rewrite(big(16 * 1024 + 1)),
            after_tool("out"),
            "too_large",
        ),
        (
            AFTER_TOOL,
            Reply::Append(vec![" ".into()]),
            after_tool("out"),
            "blank",
        ),
        (
            AFTER_TOOL,
            Reply::Append(vec![big(6_000), big(4_001)]),
            after_tool("out"),
            "too_large",
        ),
        (
            BEFORE_END,
            Reply::Continue("\n".into()),
            before_end(0),
            "blank",
        ),
        (
            BEFORE_END,
            Reply::Continue(big(MAX_APPEND_CHARS + 1)),
            before_end(0),
            "too_large",
        ),
        (
            BEFORE_END,
            Reply::Continue("go on".into()),
            before_end(MAX_CONTINUES),
            "cap_reached",
        ),
        (
            BEFORE_TURN,
            Reply::Append(vec![big(6_000), big(4_001)]),
            before_turn(),
            "too_large",
        ),
    ];
    for (point, reply_with, fire, rule) in cases {
        let (progress, records) = one(point, reply_with, fire).await;
        assert_eq!(records[0].outcome, rejected(rule), "{point} {rule}");
        assert!(progress.changes.is_empty(), "{point} {rule}");
        assert!(progress.continued.is_none(), "{point} {rule}");
        assert!(!progress.lead.iter().any(|e| e.by == "aa/h"), "{point}");
    }
}

#[tokio::test]
async fn the_rules_take_the_largest_reply_they_allow() {
    let big = |n: usize| "x".repeat(n);
    let fits: Vec<(&str, Reply, Fire)> = vec![
        (
            USER_INPUT,
            Reply::Rewrite(big(5 + MAX_APPEND_CHARS)),
            user_input("typed", 0),
        ),
        (
            USER_INPUT,
            Reply::Rewrite(String::new()),
            user_input("typed", 1),
        ),
        (
            AFTER_TOOL,
            Reply::Rewrite(big(16 * 1024)),
            after_tool("out"),
        ),
        (
            AFTER_TOOL,
            Reply::Append(vec![big(6_000), big(4_000)]),
            after_tool("out"),
        ),
        (
            BEFORE_END,
            Reply::Continue(big(MAX_APPEND_CHARS)),
            before_end(MAX_CONTINUES - 1),
        ),
    ];
    for (point, reply_with, fire) in fits {
        let (_, records) = one(point, reply_with, fire).await;
        let expected = if point == BEFORE_END {
            HookOutcome::Continued
        } else {
            HookOutcome::Applied
        };
        assert_eq!(records[0].outcome, expected, "{point}");
    }
}

#[tokio::test]
async fn a_second_rewrite_is_capped_by_the_typed_length_not_the_first_rewrite() {
    let calls = Calls::default();
    let grow: Script = Arc::new(|_, _, event| {
        let Event::UserInput(input) = event else {
            panic!();
        };
        Box::pin(async move { Reply::Rewrite(format!("{}{}", input.text, "y".repeat(6_000))) })
    });
    let body =
        hook_body("one", USER_INPUT, "") + &hook_body("two", USER_INPUT, "after = [\"aa/one\"]\n");
    let (engine, _finished) = engine(vec![source("aa", &body, grow, &calls)]).await;
    let progress = fired(&engine, user_input("typed", 0)).await;
    assert_eq!(progress.changes.len(), 1);
    assert_eq!(
        outcomes(&engine.take_records()),
        vec![
            ("aa/one".to_owned(), HookOutcome::Applied),
            ("aa/two".to_owned(), rejected("too_large")),
        ]
    );
}

#[tokio::test]
async fn after_tool_notes_follow_the_result_and_a_later_rewrite_sees_them() {
    let calls = Calls::default();
    let script: Script = Arc::new(|_, hook, event| {
        let Event::AfterTool(result) = event else {
            panic!();
        };
        let reply = if hook == "notes" {
            Reply::Append(vec!["N1".to_owned(), "N2".to_owned()])
        } else {
            Reply::Rewrite(format!("[{}]", result.result))
        };
        Box::pin(async move { reply })
    });
    let body = hook_body("notes", AFTER_TOOL, "")
        + &hook_body("wrap", AFTER_TOOL, "after = [\"aa/notes\"]\n");
    let (engine, _finished) = engine(vec![source("aa", &body, script, &calls)]).await;
    let progress = fired(&engine, after_tool("out")).await;
    assert_eq!(progress.value.as_deref(), Some("[out\nN1\nN2]"));
    assert_eq!(progress.changes[0].after, "out\nN1\nN2");
}

#[tokio::test]
async fn a_long_block_reason_is_cut_and_still_blocks() {
    let reason = "r".repeat(MAX_APPEND_CHARS + 50);
    let (progress, records) = one(USER_INPUT, Reply::Block(reason), user_input("hi", 0)).await;
    let (hook, cut) = progress.blocked.unwrap();
    assert_eq!(hook, "aa/h");
    assert_eq!(cut.chars().count(), MAX_APPEND_CHARS);
    assert!(cut.ends_with('…'));
    assert_eq!(records[0].outcome, HookOutcome::Blocked { reason: cut });
}

#[tokio::test]
async fn a_continue_is_taken_with_its_hook() {
    let (progress, records) = one(BEFORE_END, Reply::Continue("more".into()), before_end(0)).await;
    assert_eq!(
        progress.continued,
        Some(("aa/h".to_owned(), "more".to_owned()))
    );
    assert_eq!(records[0].outcome, HookOutcome::Continued);
}

#[tokio::test(start_paused = true)]
async fn on_failure_block_blocks_at_both_gate_points_on_a_failure_and_a_timeout() {
    for point in [USER_INPUT, BEFORE_TOOL] {
        for (script, why) in [(panicking(), "failed"), (sleeping(), "it timed out")] {
            let calls = Calls::default();
            let body = hook_body("h", point, "on_failure = \"block\"\ntimeout_ms = 10\n");
            let (engine, _finished) = engine(vec![source("aa", &body, script, &calls)]).await;
            let fire = if point == USER_INPUT {
                user_input("hi", 0)
            } else {
                before_tool("{}")
            };
            let progress = fired(&engine, fire).await;
            let (hook, reason) = progress.blocked.unwrap();
            assert_eq!(hook, "aa/h");
            assert!(reason.contains(why) || why == "failed", "{reason}");
        }
    }
}
