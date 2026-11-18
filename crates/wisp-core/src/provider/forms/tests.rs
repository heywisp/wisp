use serde_json::json;

use super::*;
use crate::provider::anthropic::echoed_blocks;
use crate::provider::anthropic::search::rule_r;
use crate::test_guard::opaque;
use crate::test_guard::text;
use crate::test_guard::{assistant, thinking};

fn call(id: &str) -> ContentBlock {
    opaque(
        json!({"type": "server_tool_use", "id": id, "name": "web_search", "input": {"query": id}}),
    )
}

fn found(id: &str) -> ContentBlock {
    opaque(json!({"type": "web_search_tool_result", "tool_use_id": id, "content": []}))
}

fn indices(content: &[ContentBlock], earlier: &ServerCalls) -> Vec<usize> {
    let echoed = echoed_blocks(content, earlier);
    content
        .iter()
        .enumerate()
        .filter(|(_, b)| echoed.iter().any(|e| std::ptr::eq(*e, *b)))
        .map(|(i, _)| i)
        .collect()
}

fn closed(ids: &[&str], paused: Vec<bool>) -> Rendering {
    Rendering {
        closed: ids.iter().map(|s| (*s).to_owned()).collect(),
        paused,
        ..Rendering::default()
    }
}

fn flags(layout: &Layout, len: usize, pick: fn(&Layout, usize) -> bool) -> Vec<bool> {
    (0..len).map(|i| pick(layout, i)).collect()
}

#[test]
fn a_rendered_message_pulls_in_the_message_whose_call_it_answers_and_the_one_answering_it() {
    let messages = vec![
        Message::user_text("go"),
        assistant(vec![call("s1")]),
        Message::user_text("results"),
        assistant(vec![found("s1"), call("s2"), call("s3")]),
        Message::user_text("more"),
        assistant(vec![found("s3")]),
        Message::user_text("later"),
        assistant(vec![text("unrelated")]),
    ];
    let rendering = closed(&["s2"], Vec::new());
    let layout = Layout::new(&messages, &rendering, indices);
    assert_eq!(
        flags(&layout, messages.len(), Layout::rendered),
        [false, false, false, true, false, false, false, false]
    );
    assert_eq!(
        flags(&layout, messages.len(), Layout::flattened),
        [false, true, false, true, false, true, false, false]
    );
    assert!(layout.carries_rendered());
    assert_eq!(layout.sent(1), ["s1".to_owned()]);
    assert_eq!(layout.sent(3), ["s3".to_owned()]);
}

#[test]
fn a_paused_reply_pairs_with_its_continuation_both_ways_but_never_with_a_user_message() {
    let messages = vec![
        Message::user_text("go"),
        assistant(vec![text("a")]),
        assistant(vec![text("b"), call("s1")]),
        assistant(vec![text("c")]),
        Message::user_text("after the cap"),
    ];
    let rendering = closed(&["s1"], vec![false, true, true, true, false]);
    let layout = Layout::new(&messages, &rendering, indices);
    assert_eq!(
        flags(&layout, messages.len(), Layout::flattened),
        [false, true, true, true, false]
    );
    let unpaused = closed(&["s1"], vec![false, false, false, false, false]);
    let layout = Layout::new(&messages, &unpaused, indices);
    assert_eq!(
        flags(&layout, messages.len(), Layout::flattened),
        [false, false, true, false, false]
    );
}

#[test]
fn nothing_is_rendered_without_a_closed_call_or_rule_r() {
    let messages = vec![Message::user_text("go"), assistant(vec![call("s1")])];
    let layout = Layout::new(&messages, &Rendering::default(), indices);
    assert!(!layout.carries_rendered());
    for form in [
        RenderingForm::Text,
        RenderingForm::WithoutThinking,
        RenderingForm::Flattened,
    ] {
        assert_eq!(
            layout.first_change(&messages, &Rendering::default(), form),
            None
        );
    }
    let as_text = Rendering {
        native_as_text: vec![false, true],
        ..Rendering::default()
    };
    assert!(Layout::new(&messages, &as_text, indices).carries_rendered());
}

#[test]
fn the_first_change_is_the_first_uncut_thinking_or_the_first_flattened_message() {
    let messages = vec![
        Message::user_text("go"),
        assistant(vec![text("a"), call("s1")]),
        Message::user_text("results"),
        assistant(vec![
            found("s1"),
            thinking("one"),
            thinking("two"),
            call("s2"),
        ]),
        Message::user_text("next"),
    ];
    let mut rendering = closed(&["s2"], Vec::new());
    let layout = Layout::new(&messages, &rendering, indices);
    let at = |message, block| Some(BlockPosition { message, block });
    assert_eq!(
        layout.first_change(&messages, &rendering, RenderingForm::Text),
        None
    );
    assert_eq!(
        layout.first_change(&messages, &rendering, RenderingForm::WithoutThinking),
        at(3, 1)
    );
    assert_eq!(
        layout.first_change(&messages, &rendering, RenderingForm::Flattened),
        at(1, 0)
    );
    let cut = |through| wisp_protocol::log::Cut {
        from: BlockPosition {
            message: 3,
            block: 2,
        },
        through,
        tools: false,
    };
    rendering.cuts = vec![cut(3)];
    assert_eq!(
        layout.first_change(&messages, &rendering, RenderingForm::WithoutThinking),
        at(3, 1),
        "only uncut thinking changes"
    );
    rendering.cuts = vec![cut(2)];
    assert_eq!(
        layout.first_change(&messages, &rendering, RenderingForm::WithoutThinking),
        at(3, 1)
    );
    rendering.cuts = vec![wisp_protocol::log::Cut {
        from: BlockPosition {
            message: 3,
            block: 0,
        },
        through: 3,
        tools: false,
    }];
    assert_eq!(
        layout.first_change(&messages, &rendering, RenderingForm::WithoutThinking),
        None,
        "thinking already cut changes nothing"
    );
}

fn used(id: &str) -> ContentBlock {
    ContentBlock::ToolUse {
        id: id.into(),
        name: "bash".into(),
        input: json!({}),
    }
}

#[test]
fn a_tool_turn_joins_only_from_a_rendered_message_with_a_client_call_and_uncut_thinking() {
    let messages = vec![
        Message::user_text("go"),
        assistant(vec![thinking("a"), call("s1")]),
        Message::user_text("results"),
        assistant(vec![found("s1"), thinking("b"), call("s2"), used("t1")]),
        Message::user_text("ran"),
        assistant(vec![thinking("c"), call("s3")]),
        Message::user_text("next"),
        assistant(vec![text("plain"), used("t2")]),
        Message::user_text("ran"),
    ];
    let mut rendering = closed(&["s2", "s3"], Vec::new());
    let layout = Layout::new(&messages, &rendering, indices);
    assert_eq!(
        flags(&layout, messages.len(), Layout::rendered),
        [false, false, false, true, false, true, false, false, false]
    );
    assert_eq!(
        flags(&layout, messages.len(), Layout::tool_turns),
        [false, true, false, true, false, false, false, false, false]
    );
    assert_eq!(
        flags(&layout, messages.len(), Layout::flattened),
        [false, true, false, true, false, true, false, false, false]
    );
    let at = |message, block| Some(BlockPosition { message, block });
    assert_eq!(
        layout.first_change(&messages, &rendering, RenderingForm::WithoutThinking),
        at(3, 1)
    );
    let trial = Rendering {
        cuts: vec![wisp_protocol::log::Cut {
            from: BlockPosition {
                message: 3,
                block: 1,
            },
            through: messages.len() - 1,
            tools: false,
        }],
        ..rendering.clone()
    };
    let cut = Layout::new(&messages, &trial, indices);
    assert_eq!(flags(&cut, messages.len(), Layout::tool_turns), [false; 9]);
    rendering.cuts = vec![wisp_protocol::log::Cut {
        from: BlockPosition {
            message: 3,
            block: 0,
        },
        through: 3,
        tools: false,
    }];
    let layout = Layout::new(&messages, &rendering, indices);
    assert_eq!(
        flags(&layout, messages.len(), Layout::tool_turns),
        [false; 9]
    );
    assert_eq!(
        layout.first_change(&messages, &rendering, RenderingForm::WithoutThinking),
        at(5, 0)
    );
    assert_eq!(
        layout.first_change(&messages, &rendering, RenderingForm::Flattened),
        at(1, 0)
    );
}

#[test]
fn the_native_predicate_matches_what_rule_r_renders() {
    let blocks = vec![
        call("s1"),
        found("s1"),
        opaque(
            json!({"type": "server_tool_use", "id": "x1", "name": "code_execution", "input": {}}),
        ),
        opaque(json!({"type": "web_search_tool_result", "tool_use_id": "", "content": []})),
        opaque(
            json!({"type": "text", "text": "cited", "citations": [{"type": "web_search_result_location", "url": "u"}]}),
        ),
        opaque(
            json!({"type": "text", "text": " ", "citations": [{"type": "web_search_result_location", "url": "u"}]}),
        ),
        opaque(json!({"type": "text", "text": "other", "citations": [{"type": "char_location"}]})),
        text("plain"),
        thinking("t"),
    ];
    for block in &blocks {
        assert_eq!(native(block), rule_r(block).is_some(), "{block:?}");
    }
}
