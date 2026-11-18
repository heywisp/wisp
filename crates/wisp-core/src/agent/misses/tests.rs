use jiff::{SignedDuration, Timestamp};
use serde_json::json;
use wisp_protocol::{
    CacheTtl, ContentBlock, Effort, HistoryChange, Message, MessageTime, MissCause, Role,
    SessionConfig,
};

use super::{Before, Print, Repeats, Sent, cause, dropped_thinking};
use crate::context::ttl_span;
use crate::provider::anthropic::{prefix_bytes, wire_print};
use crate::provider::{FeatureUse, Features, Rendering, Request, RequestOptions, ThinkingDrop};
use crate::tool::ToolSpec;
use crate::warmth::{Component, Fingerprint};

const TOOL_TURN: usize = 1;

type Change = fn(&mut Parts);

fn tool(name: &str) -> ToolSpec {
    ToolSpec {
        description: format!("The {name} tool."),
        input_schema: json!({"type": "object"}),
        name: name.into(),
    }
}

fn at() -> Timestamp {
    "2026-10-28T12:00:00Z".parse().unwrap()
}

#[derive(Clone)]
struct Parts {
    endpoint: &'static str,
    session: SessionConfig,
    tools: Vec<ToolSpec>,
    messages: Vec<Message>,
    features: Features,
    options: RequestOptions,
    at: Timestamp,
}

fn parts() -> Parts {
    let assistant = Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::Thinking {
                thinking: "Look first.".into(),
                signature: "sig-1".into(),
            },
            ContentBlock::ToolUse {
                id: "toolu_1".into(),
                name: "bash".into(),
                input: json!({"command": "ls"}),
            },
        ],
    };
    let result = Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: "toolu_1".into(),
            content: vec![ContentBlock::Text {
                text: "a.rs".into(),
            }],
            is_error: false,
        }],
    };
    Parts {
        endpoint: "env",
        session: SessionConfig {
            model: "claude-opus-5-5".into(),
            system: Some("You are Wisp.".into()),
            effort: Effort::High,
            cache_ttl: CacheTtl::OneHour,
            max_tokens: 64000,
            message_time: MessageTime::Local,
            instructions: None,
            context_note: true,
        },
        tools: vec![tool("bash"), tool("view_file")],
        messages: vec![Message::user_text("Look around."), assistant, result],
        features: Features::default(),
        options: RequestOptions::default(),
        at: at(),
    }
}

impl Parts {
    fn sent(&self) -> Sent {
        let rendering = Rendering::default();
        let request = Request {
            session: &self.session,
            tools: &self.tools,
            deferred: &[],
            by_value: &[],
            server_tools: &[],
            messages: &self.messages,
            previous_response_id: None,
            features: self.features,
            options: self.options,
            rendering: &rendering,
            compaction: None,
        };
        Sent {
            fingerprint: prefix_bytes(&request).map(|b| Fingerprint::new(self.endpoint, &b)),
            wire: wire_print(&request),
        }
    }

    fn print(&self, drops: &[&str]) -> Print {
        Print {
            sent: self.sent(),
            drops: dropped(drops),
        }
    }
}

fn dropped(paths: &[&str]) -> Vec<ThinkingDrop> {
    paths
        .iter()
        .map(|p| ThinkingDrop {
            path: (*p).to_owned(),
            reason: None,
        })
        .collect()
}

fn because(path: &str, reason: &str) -> ThinkingDrop {
    ThinkingDrop {
        path: path.to_owned(),
        reason: Some(reason.to_owned()),
    }
}

fn then(before: &Parts, change: Change) -> MissCause {
    then_dropping(before, &[], change, &[])
}

fn then_dropping(before: &Parts, earlier: &[&str], change: Change, drops: &[&str]) -> MissCause {
    let print = before.print(earlier);
    let mut after = before.clone();
    after.messages.push(Message::user_text("And again."));
    change(&mut after);
    let before = Before {
        print: Some(&print),
        started_at: Some(before.at),
        life: Some(ttl_span(CacheTtl::OneHour)),
        switch: None,
    };
    cause(
        None,
        &before,
        &after.sent(),
        Some(after.at),
        &dropped(drops),
    )
}

#[test]
fn each_prefix_component_names_itself() {
    let base = parts();
    let cases: [(Change, MissCause); 6] = [
        (|p| p.endpoint = "gw", MissCause::EndpointChanged),
        (
            |p| p.session.model = "claude-sonnet-5-5".into(),
            MissCause::ModelChanged,
        ),
        (|p| p.session.effort = Effort::Low, MissCause::EffortChanged),
        (
            |p| p.features.binding_controls = Some(FeatureUse::Established),
            MissCause::ThinkingChanged,
        ),
        (|p| p.tools.push(tool("show_diff")), MissCause::ToolsChanged),
        (
            |p| p.session.system = Some("You are Jarvis.".into()),
            MissCause::SystemChanged,
        ),
    ];
    for (change, expected) in cases {
        assert_eq!(then(&base, change), expected);
    }
}

#[test]
fn a_changed_beta_set_is_betas_changed() {
    let cause = then(&parts(), |p| {
        p.options = RequestOptions {
            debug_cache: true,
            refusal_fallback: false,
            message_threads: true,
        };
    });
    assert_eq!(cause, MissCause::BetasChanged);
}

#[test]
fn a_native_compaction_after_the_ttl_is_ttl_expired_not_betas_changed() {
    let cause = then(&parts(), |p| {
        p.features.native_compaction = Some(FeatureUse::Established);
        p.at = at().checked_add(SignedDuration::from_mins(61)).unwrap();
    });
    assert_eq!(cause, MissCause::TtlExpired { gap_s: 3660 });
}

#[test]
fn a_native_compaction_within_the_ttl_is_remote_loss_not_betas_changed() {
    let cause = then(&parts(), |p| {
        p.features.native_compaction = Some(FeatureUse::Established);
        p.at = at().checked_add(SignedDuration::from_mins(59)).unwrap();
    });
    assert_eq!(cause, MissCause::RemoteLoss);
}

#[test]
fn a_fallback_toggle_within_the_ttl_is_remote_loss_not_betas_changed() {
    let cause = then(&parts(), |p| {
        p.options.refusal_fallback = !p.options.refusal_fallback;
        p.at = at().checked_add(SignedDuration::from_mins(59)).unwrap();
    });
    assert_eq!(cause, MissCause::RemoteLoss);
}

#[test]
fn an_edited_earlier_message_names_its_index() {
    let cause = then(&parts(), |p| {
        p.messages[TOOL_TURN].content.remove(0);
    });
    assert_eq!(cause, MissCause::MessageChanged { index: TOOL_TURN });
}

#[test]
fn a_drop_where_wisp_sent_no_thinking_is_injected() {
    let cause = then_dropping(
        &parts(),
        &["messages.1.content.0"],
        |_| {},
        &["messages.1.content.0", "messages.1.content.1"],
    );
    assert_eq!(
        cause,
        MissCause::InjectedThinking {
            first_path: "messages.1.content.1".into(),
            count: 1,
        }
    );
}

#[test]
fn an_injected_block_that_expires_is_injected_too() {
    let cause = then_dropping(&parts(), &["messages.1.content.1"], |_| {}, &[]);
    assert_eq!(
        cause,
        MissCause::InjectedThinking {
            first_path: "messages.1.content.1".into(),
            count: 1,
        }
    );
}

#[test]
fn a_moved_drop_of_wisps_own_thinking_is_the_provider() {
    let cause = then_dropping(&parts(), &[], |_| {}, &["messages.1.content.0"]);
    assert_eq!(
        cause,
        MissCause::ProviderDroppedThinking {
            first_path: "messages.1.content.0".into(),
            count: 1,
        }
    );
}

#[test]
fn the_same_drop_list_is_a_remote_loss() {
    let listed = ["messages.1.content.0"];
    let cause = then_dropping(&parts(), &listed, |_| {}, &listed);
    assert_eq!(cause, MissCause::RemoteLoss);
}

#[test]
fn a_drop_in_a_new_message_is_a_remote_loss() {
    let cause = then_dropping(&parts(), &[], |_| {}, &["messages.3.content.0"]);
    assert_eq!(cause, MissCause::RemoteLoss);
}

#[test]
fn nothing_changed_inside_the_ttl_is_a_remote_loss() {
    assert_eq!(then(&parts(), |_| {}), MissCause::RemoteLoss);
}

#[test]
fn a_missing_part_leaves_a_miss_unexplained() {
    let base = parts();
    let mut print = base.print(&[]);
    print.sent.wire = None;
    let before = Before {
        print: Some(&print),
        started_at: Some(base.at),
        life: Some(ttl_span(CacheTtl::OneHour)),
        switch: None,
    };
    assert_eq!(
        cause(None, &before, &base.sent(), Some(base.at), &[]),
        MissCause::Unexplained
    );
    let print = base.print(&[]);
    let unknown_ttl = Before {
        print: Some(&print),
        started_at: Some(base.at),
        life: None,
        switch: None,
    };
    assert_eq!(
        cause(None, &unknown_ttl, &base.sent(), Some(base.at), &[]),
        MissCause::Unexplained
    );
    let timed = Before {
        print: Some(&print),
        started_at: Some(base.at),
        life: Some(ttl_span(CacheTtl::OneHour)),
        switch: None,
    };
    assert_eq!(
        cause(None, &timed, &base.sent(), None, &[]),
        MissCause::Unexplained
    );
}

#[test]
fn a_gap_past_the_ttl_is_ttl_expired() {
    let cause = then(&parts(), |p| {
        p.at = at().checked_add(SignedDuration::from_mins(61)).unwrap();
    });
    assert_eq!(cause, MissCause::TtlExpired { gap_s: 3660 });
}

#[test]
fn a_gap_past_a_routes_cache_life_is_ttl_expired() {
    let base = parts();
    let before = Before {
        print: None,
        started_at: Some(base.at),
        life: Some(SignedDuration::from_secs(240)),
        switch: None,
    };
    for (seconds, expected) in [
        (300, MissCause::TtlExpired { gap_s: 300 }),
        (240, MissCause::TtlExpired { gap_s: 240 }),
        (100, MissCause::Unexplained),
    ] {
        let at = base
            .at
            .checked_add(SignedDuration::from_secs(seconds))
            .unwrap();
        assert_eq!(cause(None, &before, &base.sent(), Some(at), &[]), expected);
    }
}

#[test]
fn a_gap_inside_the_ttl_is_a_remote_loss() {
    let cause = then(&parts(), |p| {
        p.at = at().checked_add(SignedDuration::from_mins(59)).unwrap();
    });
    assert_eq!(cause, MissCause::RemoteLoss);
}

#[test]
fn a_marked_rebuild_is_expected_before_any_diff() {
    let base = parts();
    let print = base.print(&[]);
    let mut after = base.clone();
    after.tools.push(tool("show_diff"));
    let before = Before {
        print: Some(&print),
        started_at: Some(base.at),
        life: Some(ttl_span(CacheTtl::OneHour)),
        switch: None,
    };
    assert_eq!(
        cause(
            Some("tools_change"),
            &before,
            &after.sent(),
            Some(after.at),
            &[]
        ),
        MissCause::Expected {
            reason: "tools_change".into()
        }
    );
}

#[test]
fn no_earlier_print_falls_back_to_the_clock() {
    let base = parts();
    let before = Before {
        print: None,
        started_at: Some(base.at),
        life: Some(ttl_span(CacheTtl::FiveMinutes)),
        switch: None,
    };
    let mut after = base.clone();
    after.at = at().checked_add(SignedDuration::from_mins(6)).unwrap();
    assert_eq!(
        cause(None, &before, &after.sent(), Some(after.at), &[]),
        MissCause::TtlExpired { gap_s: 360 }
    );
    assert_eq!(
        cause(None, &before, &base.sent(), Some(base.at), &[]),
        MissCause::Unexplained
    );
    assert_eq!(
        cause(None, &before, &after.sent(), None, &[]),
        MissCause::Unexplained
    );
}

#[test]
fn two_misses_in_a_row_with_one_cause_repeat() {
    let mut repeats = Repeats::default();
    assert_eq!(repeats.missed(&MissCause::ToolsChanged, Some(0.5)), None);
    assert_eq!(
        repeats.missed(&MissCause::ToolsChanged, Some(0.25)),
        Some((2, Some(0.75)))
    );
    assert_eq!(
        repeats.missed(&MissCause::ToolsChanged, None),
        Some((3, None))
    );
}

#[test]
fn a_hit_or_another_cause_starts_over() {
    let mut repeats = Repeats::default();
    assert_eq!(repeats.missed(&MissCause::RemoteLoss, Some(1.0)), None);
    assert_eq!(
        repeats.missed(&MissCause::RemoteLoss, Some(1.0)),
        Some((2, Some(2.0)))
    );
    repeats.hit();
    repeats.missed(&MissCause::Unexplained, Some(1.0));
    repeats.hit();
    assert_eq!(repeats.missed(&MissCause::Unexplained, Some(1.0)), None);
    assert_eq!(repeats.missed(&MissCause::ToolsChanged, Some(1.0)), None);
    let injected = |count| MissCause::InjectedThinking {
        first_path: format!("messages.{count}.content.0"),
        count,
    };
    assert_eq!(repeats.missed(&injected(1), None), None);
    assert_eq!(repeats.missed(&injected(2), None), Some((2, None)));
}

#[test]
fn expected_and_expired_misses_never_repeat() {
    for cause in [
        MissCause::Expected {
            reason: "tools_change".into(),
        },
        MissCause::TtlExpired { gap_s: 4000 },
    ] {
        let mut repeats = Repeats::default();
        assert_eq!(repeats.missed(&cause, Some(1.0)), None);
        assert_eq!(repeats.missed(&cause, Some(1.0)), None);
    }
}

fn grown(blocks: usize) -> MissCause {
    let base = parts();
    let print = base.print(&[]);
    let mut after = base.clone();
    for n in 0..blocks {
        let role = if n % 2 == 0 {
            Role::Assistant
        } else {
            Role::User
        };
        after.messages.push(Message {
            role,
            content: vec![ContentBlock::Text {
                text: format!("Line {n}."),
            }],
        });
    }
    let before = Before {
        print: Some(&print),
        started_at: Some(base.at),
        life: Some(ttl_span(CacheTtl::OneHour)),
        switch: None,
    };
    cause(None, &before, &after.sent(), Some(after.at), &[])
}

#[test]
fn growth_past_the_lookback_leaves_a_miss_unexplained() {
    assert_eq!(grown(super::LOOKBACK_BLOCKS), MissCause::RemoteLoss);
    assert_eq!(grown(super::LOOKBACK_BLOCKS + 1), MissCause::Unexplained);
}

fn switched(switch: Component, expected: Option<&str>) -> MissCause {
    let base = parts();
    let before = Before {
        print: None,
        started_at: Some(base.at),
        life: Some(ttl_span(CacheTtl::OneHour)),
        switch: Some(switch),
    };
    cause(expected, &before, &base.sent(), Some(base.at), &[])
}

#[test]
fn an_unannounced_switch_names_itself_without_prints() {
    assert_eq!(
        switched(Component::Endpoint, None),
        MissCause::EndpointChanged
    );
    assert_eq!(switched(Component::Model, None), MissCause::ModelChanged);
}

#[test]
fn an_announced_switch_is_expected() {
    assert_eq!(
        switched(Component::Model, Some("model_switch")),
        MissCause::Expected {
            reason: "model_switch".into()
        }
    );
}

#[test]
fn a_switch_with_nothing_else_changed_is_never_a_remote_loss() {
    let base = parts();
    let print = base.print(&[]);
    let before = Before {
        print: Some(&print),
        started_at: Some(base.at),
        life: Some(ttl_span(CacheTtl::OneHour)),
        switch: Some(Component::Endpoint),
    };
    assert_eq!(
        cause(None, &before, &base.sent(), Some(base.at), &[]),
        MissCause::EndpointChanged
    );
}

fn drops_after(earlier: &[&str], drops: &[ThinkingDrop]) -> Option<HistoryChange> {
    let base = parts();
    let print = base.print(earlier);
    let mut after = base.clone();
    after.messages.push(Message::user_text("And again."));
    let wire = after.sent().wire.unwrap();
    dropped_thinking(Some(&print), &wire, drops)
}

#[test]
fn a_new_drop_of_wisps_own_thinking_is_a_dropped_thinking_change_with_its_reason() {
    let drops = [because("messages.1.content.0", "model_binding_mismatch")];
    assert_eq!(
        drops_after(&[], &drops),
        Some(HistoryChange::ThinkingDropped {
            count: 1,
            reason: Some("model_binding_mismatch".into()),
        })
    );
}

#[test]
fn an_unchanged_drop_list_or_none_is_no_change() {
    assert_eq!(drops_after(&[], &[]), None);
    let drops = [because("messages.1.content.0", "prefix_binding_mismatch")];
    assert_eq!(drops_after(&["messages.1.content.0"], &drops), None);
}

#[test]
fn mixed_reasons_give_no_shared_reason() {
    let mut base = parts();
    base.messages[TOOL_TURN].content.insert(
        1,
        ContentBlock::Thinking {
            thinking: "Then act.".into(),
            signature: "sig-2".into(),
        },
    );
    let print = base.print(&[]);
    let wire = base.sent().wire.unwrap();
    let drops = [
        because("messages.1.content.0", "model_binding_mismatch"),
        because("messages.1.content.1", "organization_binding_mismatch"),
    ];
    assert_eq!(
        dropped_thinking(Some(&print), &wire, &drops),
        Some(HistoryChange::ThinkingDropped {
            count: 2,
            reason: None,
        })
    );
}

#[test]
fn a_drop_where_wisp_sent_no_thinking_is_no_dropped_thinking_change() {
    let drops = [because("messages.1.content.1", "prefix_binding_mismatch")];
    assert_eq!(drops_after(&[], &drops), None);
}
