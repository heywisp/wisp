use serde_json::json;
use wisp_protocol::{Message, Role};

use super::*;

fn spec(name: &str) -> ToolSpec {
    ToolSpec {
        description: format!("{name} does a thing"),
        input_schema: json!({"type": "object"}),
        name: name.to_owned(),
    }
}

#[test]
fn the_block_order_is_removals_then_by_name_then_by_value_then_text() {
    let decision = Decision {
        removals: vec!["a_tool".to_owned(), "b_tool".to_owned()],
        by_name: vec!["c_tool".to_owned()],
        by_value: vec![spec("d_tool"), spec("e_tool")],
        sentences: vec!["Use d_tool for d.".to_owned()],
        ..Decision::default()
    };
    let kinds: Vec<String> = decision
        .blocks()
        .iter()
        .map(|b| match b {
            ContentBlock::ToolRemoval { tool } => format!("remove {}", tool.name()),
            ContentBlock::ToolAddition { tool } if tool.by_value() => {
                format!("define {}", tool.name())
            }
            ContentBlock::ToolAddition { tool } => format!("add {}", tool.name()),
            ContentBlock::Text { text } => format!("text {text}"),
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "remove a_tool",
            "remove b_tool",
            "add c_tool",
            "define d_tool",
            "define e_tool",
            "text Use d_tool for d."
        ]
    );
}

const SOURCES: [(&str, &str); 14] = [
    ("agent.rs", include_str!("../../agent.rs")),
    ("child.rs", include_str!("../child.rs")),
    ("compact.rs", include_str!("../compact.rs")),
    ("epoch.rs", include_str!("../epoch.rs")),
    ("fill.rs", include_str!("../fill.rs")),
    ("keep_warm.rs", include_str!("../keep_warm.rs")),
    ("misses.rs", include_str!("../misses.rs")),
    ("native.rs", include_str!("../native.rs")),
    ("resume.rs", include_str!("../resume.rs")),
    ("search.rs", include_str!("../search.rs")),
    ("settings.rs", include_str!("../settings.rs")),
    ("switches.rs", include_str!("../switches.rs")),
    ("tool_changes.rs", include_str!("../tool_changes.rs")),
    ("background.rs", include_str!("../compact/background.rs")),
];

#[test]
fn requests_and_logs_read_the_sent_copy_and_never_the_toolbox_definitions() {
    let taken = "self.sent = toolbox.specs().to_vec();";
    for (name, source) in SOURCES {
        for (n, line) in source.lines().enumerate() {
            let line = line.trim();
            let reads = line.contains("toolbox.specs()")
                || line.contains("toolbox().specs()")
                || line.contains(".canonical_json()")
                || line.contains("toolbox.definitions()")
                || line.contains("toolbox.sha256()");
            assert!(!reads || line == taken, "{name}:{}: {line}", n + 1);
        }
    }
    assert_eq!(
        SOURCES[0].1.matches(taken).count(),
        2,
        "with_tools and the test setter take the copy"
    );
}

#[test]
fn every_tool_part_shape_passes_the_layout_check_after_a_user_message() {
    let provider = crate::provider::anthropic::AnthropicProvider::new(
        &crate::provider::anthropic::Credential::ApiKey("x".into()),
        "http://127.0.0.1:9",
    )
    .unwrap();
    let sentence = || vec!["Use d_tool for d.".to_owned()];
    let shapes = [
        Decision {
            removals: vec!["a_tool".to_owned()],
            ..Decision::default()
        },
        Decision {
            by_name: vec!["c_tool".to_owned()],
            ..Decision::default()
        },
        Decision {
            by_value: vec![spec("d_tool")],
            sentences: sentence(),
            ..Decision::default()
        },
        Decision {
            sentences: sentence(),
            ..Decision::default()
        },
    ];
    let markers = [
        None,
        Some(wisp_protocol::EffortMarker {
            level: wisp_protocol::Effort::Low,
            rendered: true,
        }),
    ];
    let reply = Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Text { text: "a".into() }],
    };
    for decision in &shapes {
        for marker in markers {
            let system = Message::turn_system(marker, &decision.blocks()).unwrap();
            for messages in [
                vec![Message::user_text("q"), system.clone()],
                vec![
                    Message::user_text("q"),
                    system.clone(),
                    reply.clone(),
                    Message::user_text("r"),
                ],
            ] {
                assert!(
                    crate::agent::check_layout(&messages, &[], &Default::default(), &provider)
                        .is_ok(),
                    "{messages:?}"
                );
            }
        }
    }
}

fn switchable_with(on: &[&str], held: &[&str]) -> Agent {
    let provider = crate::provider::anthropic::AnthropicProvider::new(
        &crate::provider::anthropic::Credential::ApiKey("x".into()),
        "http://127.0.0.1:9",
    )
    .unwrap();
    switchable_on(
        std::sync::Arc::new(provider),
        crate::agent::default_session(),
        on,
        held,
    )
}

fn switchable_on(
    provider: std::sync::Arc<dyn crate::provider::Provider>,
    session: wisp_protocol::SessionConfig,
    on: &[&str],
    held: &[&str],
) -> Agent {
    let mut agent = Agent::new(provider, session);
    agent.settings.layers.full_screen = true;
    agent.tool_switches.baseline = Some(BTreeSet::new());
    let (registry, problems) = crate::plugin::Registry::from_sources(
        vec![(
            wisp_plugin_testkit::MANIFEST.to_owned(),
            crate::tool::testkit::make_with(Default::default()),
        )],
        &crate::plugin::PluginConfig::default(),
    );
    assert!(problems.is_empty(), "{problems:?}");
    agent.hooks.registry = Some(std::sync::Arc::new(registry));
    let toolbox = crate::tool::Toolbox::new(on.iter().map(|n| crate::tool::testkit::entry(n)))
        .with_held(held.iter().map(|n| crate::tool::testkit::entry(n)));
    agent.set_toolbox(toolbox);
    agent
}

#[test]
fn the_deferred_set_is_the_held_tools_and_never_every_client_tool() {
    let agent = switchable_with(&["echo"], &["sleep", "confirm"]);
    let names: Vec<&str> = agent.deferred.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["confirm", "sleep"]);
    let agent = switchable_with(&[], &["sleep", "confirm"]);
    assert!(agent.switchable());
    assert!(
        agent.deferred.is_empty(),
        "at least one client tool stays non-deferred, else nothing is deferred"
    );
}

#[test]
fn an_openai_pair_never_defers_and_its_provider_gets_no_deferred_tools() {
    let model = crate::models::ModelTable::shared()
        .provider(crate::endpoints::ProviderKind::OpenAi)
        .unwrap()
        .api_additional_tools
        .first()
        .unwrap();
    for (base_url, inline) in [
        (crate::provider::openai::DEFAULT_BASE_URL, true),
        ("https://gateway.example/v1", false),
    ] {
        let endpoint = crate::endpoints::ResolvedEndpoint {
            name: crate::endpoints::EndpointName::parse("openai-test").unwrap(),
            provider: crate::endpoints::ProviderKind::OpenAi,
            credential: crate::provider::credential::Credential::ApiKey("synthetic".into()),
            base_url: base_url.into(),
            source: crate::provider::credential::Source::Environment,
            key_env: None,
            url_source: crate::endpoints::UrlSource::Official,
            pricing: crate::endpoints::PricingMode::List,
        };
        let provider = crate::provider::openai::OpenAiProvider::from_endpoint(&endpoint).unwrap();
        let mut session = crate::agent::default_session();
        session.model = model.clone();
        let agent = switchable_on(
            std::sync::Arc::new(provider),
            session,
            &["echo"],
            &["sleep", "confirm"],
        );
        assert!(!agent.deferred.is_empty());
        assert!(agent.deferred_on(agent.endpoint_name(), model).is_empty());
        let (_, features) = agent.request_parts();
        assert_eq!(features.deferred_tools, None);
        assert!(deferred_for(&agent.deferred, features).is_empty());
        assert_eq!(features.inline_tools.is_some(), inline);
        assert_eq!(
            by_value_for(&agent.deferred, features),
            if inline { &agent.deferred[..] } else { &[] }
        );
    }
}

fn defined(name: &str, description: &str) -> ContentBlock {
    ContentBlock::ToolAddition {
        tool: ToolRef::ToolDefinition {
            definition: ToolDefinition {
                description: description.to_owned(),
                input_schema: json!({"type": "object"}),
                name: name.to_owned(),
            },
        },
    }
}

fn by_name(name: &str) -> ContentBlock {
    ContentBlock::ToolAddition {
        tool: ToolRef::ToolReference {
            name: name.to_owned(),
        },
    }
}

fn system(content: Vec<ContentBlock>) -> Message {
    Message {
        role: Role::System,
        content,
    }
}

fn voiding() -> Agent {
    let provider = crate::provider::anthropic::AnthropicProvider::new(
        &crate::provider::anthropic::Credential::ApiKey("x".into()),
        "http://127.0.0.1:9",
    )
    .unwrap();
    let mut session = crate::agent::default_session();
    session.model = "claude-opus-5-5".into();
    let mut agent = switchable_on(
        std::sync::Arc::new(provider),
        session,
        &["echo"],
        &["sleep"],
    );
    agent.history = vec![
        Message::user_text("one"),
        system(vec![defined("echo", "an older echo"), by_name("sleep")]),
        Message::user_text("two"),
    ];
    agent.rebuilt_at(3);
    agent
}

#[test]
fn a_by_value_addition_of_an_array_tool_before_the_point_is_void_in_every_read() {
    let agent = voiding();
    assert!(agent.inline_use().is_some());
    let array = agent.sent.first().cloned();
    let offer = agent.read_offer(true);
    assert_eq!(offer.latest.get("echo").cloned(), array);
    assert!(!offer.by_value.contains("echo"));
    let counted = agent.counted_tools();
    assert_eq!(counted.iter().find(|s| s.name == "echo").cloned(), array);
    let rendering = agent.kept_rendering(agent.history.len());
    assert_eq!(rendering.tools_point, 3);
    assert!(rendering.voided(1, &defined("echo", "an older echo")));
    assert!(!rendering.voided(1, &by_name("echo")));
    assert!(!rendering.voided(1, &defined("sleep", "sleep does a thing")));
    assert!(!rendering.voided(3, &defined("echo", "a newer echo")));
}

#[test]
fn an_addition_after_the_point_and_after_a_rewind_below_it_stays() {
    let mut agent = voiding();
    agent
        .history
        .push(system(vec![defined("echo", "a newer echo")]));
    let offer = agent.read_offer(true);
    assert_eq!(
        offer.latest.get("echo").map(|s| s.description.as_str()),
        Some("a newer echo")
    );
    let mut agent = voiding();
    agent.history.truncate(1);
    agent.rewound_tools(1);
    agent
        .history
        .push(system(vec![defined("echo", "a newer echo")]));
    agent.history.push(Message::user_text("two"));
    assert_eq!(agent.tools_point(), 1);
    assert!(agent.read_offer(true).by_value.contains("echo"));
    let rendering = agent.kept_rendering(agent.history.len());
    assert!(!rendering.voided(1, &defined("echo", "a newer echo")));
}

#[test]
fn a_fallback_marks_from_the_first_tool_block_on_the_wire_not_a_voided_addition() {
    let mut agent = voiding();
    agent.history = vec![
        Message::user_text("one"),
        system(vec![defined("echo", "an older echo")]),
        Message::user_text("two"),
        system(vec![by_name("sleep")]),
    ];
    agent.epoch.push(super::super::epoch::Entry {
        turn: 1,
        seq: None,
        view_len: 3,
        prompt_tokens: 0,
        used: 0,
        output_tokens: 0,
        started_at: jiff::Timestamp::UNIX_EPOCH,
        ttl: wisp_protocol::CacheTtl::OneHour,
        model: "claude-opus-5-5".into(),
        endpoint: 0,
        prefix: None,
        refreshed_at: None,
    });
    agent.inline_fell_back();
    assert_eq!(agent.epoch.marked_at(), (0, 1));
}

mod found;
