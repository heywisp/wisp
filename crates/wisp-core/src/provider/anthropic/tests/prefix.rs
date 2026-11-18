use serde_json::json;
use wisp_protocol::{CacheTtl, Effort, Message, SessionConfig};

use super::session;
use crate::agent::testing::text_of;
use crate::provider::anthropic::prefix_bytes;
use crate::provider::{
    CompactionAsk, FeatureUse, Features, PrefixBytes, Rendering, Request, RequestOptions,
    ServerToolSpec,
};
use crate::tool::ToolSpec;
use crate::warmth::{Component, Fingerprint};

fn tool(name: &str) -> ToolSpec {
    ToolSpec {
        description: format!("The {name} tool."),
        input_schema: json!({"type": "object", "properties": {"b": {}, "a": {}}}),
        name: name.into(),
    }
}

struct Parts {
    session: SessionConfig,
    tools: Vec<ToolSpec>,
    server_tools: Vec<ServerToolSpec>,
    messages: Vec<Message>,
    features: Features,
    rendering: Rendering,
    options: RequestOptions,
    compaction: Option<CompactionAsk>,
    previous: Option<&'static str>,
}

fn parts() -> Parts {
    let mut session = session(Some("You are Wisp."), CacheTtl::OneHour);
    session.instructions = Some("Answer briefly.".into());
    Parts {
        session,
        tools: vec![tool("view_file"), tool("bash")],
        server_tools: vec![ServerToolSpec {
            name: "web_search".into(),
            kind: "web_search_20250305".into(),
        }],
        messages: vec![Message::user_text("hello")],
        features: Features::default(),
        rendering: Rendering::default(),
        options: RequestOptions::default(),
        compaction: None,
        previous: None,
    }
}

impl Parts {
    fn request(&self) -> Request<'_> {
        Request {
            session: &self.session,
            tools: &self.tools,
            deferred: &[],
            by_value: &[],
            server_tools: &self.server_tools,
            messages: &self.messages,
            previous_response_id: self.previous,
            features: self.features,
            options: self.options,
            rendering: &self.rendering,
            compaction: self.compaction.as_ref(),
        }
    }

    fn bytes(&self) -> PrefixBytes {
        prefix_bytes(&self.request()).unwrap()
    }

    fn body(&self) -> String {
        text_of(&self.request())
    }

    fn print(&self) -> Fingerprint {
        Fingerprint::new("env", &self.bytes())
    }
}

type Change = fn(&mut Parts);

fn text(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).unwrap()
}

#[test]
fn each_component_is_the_bytes_the_body_sends() {
    let parts = parts();
    let bytes = parts.bytes();
    let body = parts.body();
    for (field, value) in [
        ("model", &bytes.model),
        ("thinking", &bytes.thinking),
        ("output_config", &bytes.effort),
        ("tools", &bytes.tools),
        ("system", &bytes.system),
    ] {
        let sent = format!("\"{field}\":{}", text(value));
        assert!(body.contains(&sent), "{sent} not in {body}");
    }
    let tools = text(&bytes.tools);
    let bash = tools.find(r#""name":"bash""#).unwrap();
    let view_file = tools.find(r#""name":"view_file""#).unwrap();
    assert!(bash < view_file, "{tools}");
    assert!(text(&bytes.tools).contains(r#""properties":{"a":{},"b":{}}"#));
    assert!(text(&bytes.system).contains("Answer briefly."));
}

#[test]
fn the_tail_and_request_options_leave_the_prefix_alone() {
    let base = parts().print();
    let changes: Vec<Change> = vec![
        |p| p.messages.push(Message::user_text("again")),
        |p| p.session.cache_ttl = CacheTtl::FiveMinutes,
        |p| p.session.max_tokens = 1_000,
        |p| p.previous = Some("msg_01"),
        |p| {
            p.options = RequestOptions {
                debug_cache: true,
                refusal_fallback: false,
                message_threads: true,
            };
        },
        |p| {
            p.compaction = Some(CompactionAsk {
                native_instructions: Some("Summarise.".into()),
                tail: None,
            });
        },
        |p| p.features.per_message_effort = Some(FeatureUse::Established),
    ];
    for change in changes {
        let mut changed = parts();
        change(&mut changed);
        assert_eq!(changed.print(), base);
    }
}

#[test]
fn each_prefix_input_changes_its_component() {
    let base = parts().print();
    let changes: Vec<(Change, Component)> = vec![
        (
            |p| p.session.model = "claude-sonnet-4-5".into(),
            Component::Model,
        ),
        (|p| p.session.effort = Effort::Low, Component::Effort),
        (
            |p| p.features.binding_controls = Some(FeatureUse::Trial),
            Component::Thinking,
        ),
        (|p| p.tools.push(tool("process")), Component::Tools),
        (|p| p.server_tools.clear(), Component::Tools),
        (
            |p| p.tools[0].description = "Read a file.".into(),
            Component::Tools,
        ),
        (
            |p| p.session.system = Some("You are Jarvis.".into()),
            Component::System,
        ),
        (|p| p.session.instructions = None, Component::System),
        (|p| p.rendering.drop_base_marker = true, Component::System),
    ];
    for (change, component) in changes {
        let mut changed = parts();
        change(&mut changed);
        let print = changed.print();
        assert_ne!(print.key(), base.key(), "{}", component.as_str());
        assert_eq!(print.changed(&base), vec![component]);
    }
}

#[test]
fn equal_requests_give_one_key() {
    let mut reordered = parts();
    reordered.tools.reverse();
    assert_eq!(reordered.print().key(), parts().print().key());
    assert_eq!(parts().bytes(), parts().bytes());
}

#[test]
fn found_tool_messages_and_their_compaction_forks_keep_the_exact_prefix() {
    let mut parts = parts();
    parts.features.inline_tools = Some(FeatureUse::Established);
    let original = parts.bytes();
    parts.messages.extend([
        Message {
            role: wisp_protocol::Role::Assistant,
            content: vec![wisp_protocol::ContentBlock::ToolUse {
                id: "find".into(),
                name: "view_file".into(),
                input: json!({}),
            }],
        },
        Message {
            role: wisp_protocol::Role::User,
            content: vec![wisp_protocol::ContentBlock::ToolResult {
                tool_use_id: "find".into(),
                content: vec![],
                is_error: false,
            }],
        },
        Message::turn_system(None, &[super::inline::gamma()]).unwrap(),
    ]);
    assert_eq!(parts.bytes(), original);
    let accepted = parts.body();
    let mut refresh = parts.request();
    let mut config = parts.session.clone();
    config.max_tokens = 1;
    refresh.session = &config;
    assert_eq!(prefix_bytes(&refresh).unwrap(), original);
    parts.compaction = Some(CompactionAsk {
        native_instructions: Some("Summarize.".into()),
        tail: None,
    });
    assert_eq!(parts.bytes(), original);
    let before: serde_json::Value = serde_json::from_str(&accepted).unwrap();
    let fork: serde_json::Value = serde_json::from_str(&parts.body()).unwrap();
    assert_eq!(before["messages"], fork["messages"]);
}
