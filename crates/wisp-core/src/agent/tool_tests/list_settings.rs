use wisp_protocol::RequestId;
use wisp_protocol::settings::names::{VOICE_ENDPOINT, VOICE_VOICE};
use wisp_protocol::settings::{
    AUTH_ANTHROPIC_SUBSCRIPTION, CredentialKind, EndpointSummary, Form, Place, SettingValue,
    SitePath, Target,
};

use super::*;
use crate::settings::Given;
use crate::settings::host::StartLayers;
use wisp_plugin_menus::NAME;

pub(super) const KEY: &str = "sk-ant-CANARYKEYLISTED01";
pub(super) const KEY_SHAPED: &str = "sk-ant-api03-canary012345CANARYcdef";

pub(super) fn connection(name: &str, provider: Arc<Fake>) -> Connection {
    Connection {
        name: name.into(),
        provider,
        context: LogContext {
            endpoint_name: name.into(),
            ..log_context()
        },
        priced: true,
    }
}

fn key_at(endpoint: &str) -> SitePath {
    SitePath::new(
        Place::Endpoint(endpoint.into()),
        wisp_protocol::settings::find("key").unwrap(),
    )
}

pub(super) fn layers() -> StartLayers {
    let mut saved = crate::config::SavedSettings::default();
    saved.insert(key_at(KEY_SHAPED), SettingValue::Text(KEY.into()), 4);
    let max_agents = wisp_protocol::settings::find("agents.max_agents").unwrap();
    saved.insert(
        SitePath::global(max_agents),
        max_agents.parse("7").unwrap(),
        2,
    );
    let endpoint = wisp_protocol::settings::find("endpoint").unwrap();
    saved.insert(
        SitePath::global(endpoint),
        SettingValue::Text("env".into()),
        3,
    );
    StartLayers {
        env: vec![
            Given {
                target: Target::Path(key_at(KEY_SHAPED)),
                value: SettingValue::Text(KEY.into()),
                origin: "WISP_CANARY_KEY".into(),
            },
            Given {
                target: Target::Name(wisp_protocol::settings::find("effort").unwrap()),
                value: SettingValue::Text("high".into()),
                origin: "WISP_EFFORT".into(),
            },
        ],
        saved,
        ..StartLayers::default()
    }
}

fn listing(provider: Arc<Fake>, env: &Env) -> Agent {
    Agent::new(provider.clone(), default_session())
        .with_clock(TickingClock::new())
        .with_tools(crate::tool::testkit::bundled(&[NAME]), env.workspace())
        .with_platform("Linux")
        .with_resources(None)
        .with_cancel_grace(Duration::from_millis(300))
        .with_connections(
            vec![connection(KEY_SHAPED, provider)],
            Vec::new(),
            KEY_SHAPED,
        )
        .with_start_layers(StartLayers {
            endpoints: vec![EndpointSummary {
                name: KEY_SHAPED.into(),
                credential: Some(CredentialKind::Other(AUTH_ANTHROPIC_SUBSCRIPTION.into())),
                ..EndpointSummary::default()
            }],
            ..layers()
        })
}

pub(super) fn results(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCallFinished { result, .. } => result.clone(),
            _ => None,
        })
        .collect()
}

fn names(result: &str) -> Vec<&str> {
    result
        .lines()
        .filter_map(|l| l.strip_prefix("- "))
        .filter_map(|l| l.split_once(':').map(|(name, _)| name))
        .collect()
}

#[tokio::test]
async fn only_a_menus_call_gets_a_view() {
    let env = Env::new("list-settings-view");
    let agent = listing(scripted(Vec::new()), &env);
    assert!(agent.call_view(wisp_plugin_menus::ID).is_some());
    for other in ["agents", "shell", "testkit", "show-diff", "todos"] {
        assert!(agent.call_view(other).is_none(), "{other}");
    }
}

#[tokio::test]
async fn list_settings_reads_the_rows_config_reads_and_never_a_secret_or_an_origin() {
    crate::plugin::testing::declared(
        "listed-row",
        wisp_protocol::plugin::RowKind::Int {
            min: 1,
            max: 60,
            default: 10,
        },
    );
    let env = Env::new("list-settings");
    let scripted = scripted(vec![
        uses(&[call("s1", NAME, json!({}))]),
        uses(&[call("s2", NAME, json!({"filter": "stamp"}))]),
        uses(&[call("s3", NAME, json!({"filter": "xyz"}))]),
        done("listed"),
    ]);
    let mut agent = listing(scripted.clone(), &env);
    let (outcome, events) = turn(&mut agent, "what are my settings").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{events:#?}"
    );
    let found = results(&events);
    assert_eq!(found.len(), 3, "{events:#?}");
    let snapshot = agent.snapshot(RequestId(1), None);
    let shown: Vec<&str> = snapshot.settings.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names(&found[0]), shown);
    assert!(
        found[0].starts_with(&format!(
            "{} settings, with where each value comes from:\n",
            shown.len()
        )),
        "{}",
        found[0]
    );
    for line in [
        "- agents.max_agents: 7 (config)",
        "- effort: high (variable)",
        "- endpoint: env (config)",
        "- max_in_flight: 12 (default)",
        "- time: local (default)",
        "- plugins.listed-row.minutes: 10 (default)",
    ] {
        assert!(
            found[0].lines().any(|l| l == line),
            "{line} in {}",
            found[0]
        );
    }
    let time = wisp_protocol::settings::find("time").unwrap();
    assert_eq!(
        found[1],
        format!(
            "1 setting, with where each value comes from:\n- time: local (default)\n  {}\n  {}",
            time.help,
            time.describe(Form::Rule)
        )
    );
    assert_eq!(
        found[2],
        "No setting matches `xyz`; leave filter out to list them all."
    );
    for text in &found {
        for hidden in [
            KEY,
            "CANARY",
            KEY_SHAPED,
            "WISP_",
            "config.toml",
            "line 2",
            "line 4",
        ] {
            assert!(!text.contains(hidden), "{hidden} in {text}");
        }
    }
    let requests = scripted.sent();
    assert_eq!(requests.len(), 4);
    let first = &requests[0];
    assert_eq!(
        first
            .tools
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        [NAME]
    );
    for recorded in &requests {
        let body = recorded.text();
        assert!(!body.contains(KEY), "{body}");
        assert!(!body.contains("WISP_CANARY_KEY"), "{body}");
        assert_eq!(
            serde_json::to_string(&recorded.tools).unwrap(),
            serde_json::to_string(&first.tools).unwrap()
        );
        assert_eq!(recorded.session.system, first.session.system);
    }
    for (later, earlier) in requests.iter().skip(1).zip(&requests) {
        assert_eq!(
            later.messages[..earlier.messages.len()],
            earlier.messages[..],
            "a call appends only its result"
        );
        assert_eq!(later.messages.len(), earlier.messages.len() + 2);
    }
    assert!(
        crate::tool::testkit::bundled(&[NAME])
            .system_lines()
            .is_empty()
    );
}

#[test]
fn the_voice_choices_are_the_accounts_that_offer_voice_and_the_saved_ones_voices() {
    let plain = scripted(Vec::new());
    let knobs = Knobs {
        voice: Some(Arc::new(crate::voice::testing::Route::default())),
        ..Knobs::default()
    };
    let voiced = Fake::with(knobs, []);
    let mut layers = StartLayers::default();
    let spec = wisp_protocol::settings::find(VOICE_ENDPOINT).unwrap();
    layers
        .saved
        .insert(SitePath::global(spec), SettingValue::Text("talk".into()), 1);
    let connections = vec![
        connection("work", plain.clone()),
        connection("talk", voiced),
    ];
    let agent = Agent::new(plain, default_session())
        .with_connections(connections, Vec::new(), "work")
        .with_start_layers(layers);
    let snapshot = agent.snapshot(RequestId(1), None);
    let choices = |name: &str| {
        let row = snapshot
            .settings
            .iter()
            .find(|r| r.name.spec().name == name);
        row.map(|r| r.choices.clone()).unwrap()
    };
    assert_eq!(choices(VOICE_ENDPOINT), ["talk"]);
    assert_eq!(choices(VOICE_VOICE), ["cove", "marin"]);
}
