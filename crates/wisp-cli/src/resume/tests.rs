use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clap::Parser;
use wisp_core::endpoints::{Availability, EndpointName, ProviderKind};
use wisp_core::resume::LoggedSession;
use wisp_protocol::log::LogRecord;
use wisp_render::chrome::{StartPick, named_model};
use wisp_start::resume::ask::Why;

use super::*;
use crate::Args;
use crate::turn_lines::with_turn_lines;
use wisp_core::session::JsonlStore;

#[test]
fn a_logged_directory_under_home_expands_from_home() {
    let home = Path::new("/home/someone");
    assert_eq!(root("~", Some(home)), PathBuf::from("/home/someone"));
    assert_eq!(
        root("~/Projects/wisp", Some(home)),
        PathBuf::from("/home/someone/Projects/wisp")
    );
    assert_eq!(root("/srv/app", Some(home)), PathBuf::from("/srv/app"));
    assert_eq!(root("~other/x", Some(home)), PathBuf::from("~other/x"));
    assert_eq!(root("~/x", None), PathBuf::from("~/x"));
}

#[test]
fn resume_takes_an_optional_id_and_refuses_no_log() {
    let args = Args::try_parse_from(["wisp", "--resume"]).unwrap();
    assert_eq!(args.resume, Some(None));
    let args = Args::try_parse_from(["wisp", "--resume", "01a167d6"]).unwrap();
    assert_eq!(args.resume, Some(Some("01a167d6".to_owned())));
    assert!(!args.resume_unlocked);
    let args = Args::try_parse_from(["wisp", "--resume", "01a1", "--resume-unlocked"]).unwrap();
    assert!(args.resume_unlocked);
    assert!(Args::try_parse_from(["wisp", "--resume", "--no-log"]).is_err());
    assert!(Args::try_parse_from(["wisp", "--resume-unlocked"]).is_err());
    assert_eq!(Args::try_parse_from(["wisp"]).unwrap().resume, None);
}

#[test]
fn resume_and_its_unlocked_form_are_in_the_help() {
    use clap::CommandFactory;
    let help = Args::command().render_long_help().to_string();
    assert!(help.contains("--resume [<ID>]"), "{help}");
    assert!(help.contains("--resume-unlocked"), "{help}");
    assert!(
        help.contains("Use it only when you know the session has ended"),
        "{help}"
    );
}

#[test]
fn the_unlocked_flag_states_its_risk() {
    assert!(UNLOCKED_HELP.contains("the whole log becomes unreadable"));
    assert!(UNLOCKED_HELP.contains("Use it only when you know the session has ended"));
}

fn logged(extra: &str) -> LoggedSession {
    let line = format!(
        r#"{{"schema_version":20,"seq":0,"at":"2026-10-25T10:00:00Z","type":"session_started","session_id":"01a167d6-0000-7000-8000-000000000000","wisp_version":"0.1.0","model":"claude-opus-5-5","effort":"medium","cache_ttl":"one_hour","endpoint":"https://api.anthropic.com","credential_source":"environment","endpoint_name":"work"{extra}}}
"#
    );
    let records = wisp_core::session::parse_jsonl(line).unwrap();
    LoggedSession::read(&records).unwrap()
}

#[test]
fn an_unknown_logged_extra_is_noted_only_without_a_flag() {
    let note = r#"wisp: this session logged extra instructions "someday", which this Wisp doesn't know; the logged instructions are kept, and would read as none only if the log were damaged"#;
    let unknown = logged(r#","instructions_extra":"someday""#);
    assert_eq!(extra_kept(&unknown, false).as_deref(), Some(note));
    assert_eq!(extra_kept(&unknown, true), None);
    assert_eq!(
        extra_kept(&logged(r#","instructions_extra":"autonomous""#), false),
        None
    );
    assert_eq!(extra_kept(&logged(""), false), None);
}

#[test]
fn a_resumed_session_loads_its_instructions_from_the_logged_source() {
    let current = crate::instructions::Settings {
        source: wisp_protocol::InstructionsSource::None,
        warn_tokens: 7,
    };
    let kept = instructions_settings(
        &logged(r#","instructions_source":"default""#),
        current.clone(),
    );
    assert_eq!(kept.source, wisp_protocol::InstructionsSource::Default);
    assert_eq!(kept.warn_tokens, 7);
    assert_eq!(instructions_settings(&logged(""), current.clone()), current);
}

pub(crate) struct Silent(pub(crate) Option<std::sync::Arc<wisp_core::models::list::ModelList>>);

impl wisp_core::provider::Provider for Silent {
    fn stream(
        &self,
        _request: &wisp_core::provider::Request<'_>,
    ) -> futures::stream::BoxStream<
        'static,
        Result<wisp_core::provider::ProviderEvent, wisp_core::provider::ProviderError>,
    > {
        use futures::StreamExt;
        futures::stream::empty().boxed()
    }

    fn echoed<'a>(
        &self,
        _content: &'a [wisp_protocol::ContentBlock],
        _earlier: &wisp_core::provider::ServerCalls,
    ) -> Vec<&'a wisp_protocol::ContentBlock> {
        Vec::new()
    }

    fn cache_prefix(&self, _request: &wisp_core::provider::Request<'_>) -> Option<String> {
        None
    }

    fn model_list(&self) -> Option<std::sync::Arc<wisp_core::models::list::ModelList>> {
        self.0.clone()
    }

    fn capabilities(&self, model: &str) -> Option<wisp_core::provider::ModelCapabilities> {
        let list = self.0.as_deref()?;
        wisp_core::provider::openai::listed_capabilities(Some(list), model)
    }

    fn kind(&self) -> wisp_core::endpoints::ProviderKind {
        let listed = wisp_core::endpoints::ProviderKind::OpenAi;
        self.0
            .as_ref()
            .map_or(wisp_core::endpoints::ProviderKind::IMPLICIT, |_| listed)
    }
}

pub(crate) fn listed(entries: &[(&str, &str, &str)]) -> wisp_core::models::list::ModelList {
    let entries: Vec<serde_json::Value> = entries
        .iter()
        .map(|(slug, visibility, default)| {
            serde_json::json!({
                "slug": slug,
                "visibility": visibility,
                "context_window": 272_000,
                "supported_reasoning_levels": [{ "effort": "low" }, { "effort": "medium" }],
                "default_reasoning_level": default,
                "input_modalities": ["text"],
                "supports_search_tool": true,
            })
        })
        .collect();
    let body = serde_json::json!({ "models": entries }).to_string();
    wisp_core::models::list::ModelList::parse(body.as_bytes()).unwrap()
}

pub(crate) fn connection(
    name: &str,
    list: Option<wisp_core::models::list::ModelList>,
) -> wisp_core::agent::Connection {
    wisp_core::agent::Connection {
        name: name.to_owned(),
        provider: std::sync::Arc::new(Silent(list.map(std::sync::Arc::new))),
        context: log_context(name),
        priced: true,
    }
}

fn log_context(name: &str) -> wisp_core::agent::LogContext {
    wisp_core::agent::LogContext {
        endpoint: "https://api.anthropic.com".to_owned(),
        endpoint_name: name.to_owned(),
        credential_source: wisp_protocol::log::CredentialSource::Environment,
        redactions: wisp_core::session::Redactions::default(),
    }
}

const STARTED: &str = r#"{"schema_version":20,"seq":0,"at":"2026-10-25T10:00:00Z","type":"session_started","session_id":"01a167d6-0000-7000-8000-000000000000","wisp_version":"0.1.0","model":"claude-opus-5-5","effort":"medium","cache_ttl":"one_hour","endpoint":"https://api.anthropic.com","credential_source":"environment","endpoint_name":"work"}"#;

fn switched_to(seq: u64, endpoint: &str, model: &str) -> String {
    format!(
        r#"{{"schema_version":20,"seq":{seq},"at":"2026-10-25T10:01:00Z","type":"settings_changed","model":"{model}","effort":"medium","cache":{{"type":"kept"}},"endpoint_name":"{endpoint}","changes":[]}}"#
    )
}

fn resumed_note(switches: &[(&str, &str)]) -> (Option<String>, Option<String>) {
    use wisp_core::session::SessionStore;
    let mut lines = vec![STARTED.to_owned()];
    for (seq, (endpoint, model)) in (1..).zip(switches) {
        lines.push(switched_to(seq, endpoint, model));
    }
    let records = wisp_core::session::parse_jsonl(lines.join("\n") + "\n").unwrap();
    let store = wisp_core::session::MemoryStore::default();
    let mut writer = store.clone();
    for record in &records {
        writer.append("s1", record).unwrap();
    }
    let mut session = wisp_core::agent::default_session();
    session.model = "claude-opus-5-5".to_owned();
    session.effort = wisp_protocol::Effort::Medium;
    let seq = u64::try_from(records.len()).unwrap();
    let agent = with_turn_lines(wisp_core::agent::Agent::new(
        std::sync::Arc::new(Silent(None)),
        session,
    ))
    .with_connections(
        vec![connection("work", None), connection("other", None)],
        vec![],
        "work",
    )
    .with_log(
        wisp_core::session::SessionLog::continuing(Box::new(store), "s1", seq),
        log_context("work"),
    )
    .resume(&records, 0)
    .unwrap();
    let mut facts = crate::tui::facts(
        crate::tui::Chosen {
            model: "claude-opus-5-5",
            effort: "medium",
            cache_ttl: wisp_protocol::CacheTtl::OneHour,
            endpoint: wisp_render::chrome::Endpoint::Official,
            max_tokens: 64_000,
            claude_usage: false,
        },
        None,
        None,
        None,
    );
    facts.next = agent.next_turn();
    let pending = facts.pending(&wisp_render::theme::glyphs::Glyphs::of(
        wisp_render::theme::glyphs::GlyphSet::Unicode,
    ));
    (facts.pending_chip().map(|chip| chip.name), pending)
}

#[test]
fn a_restored_switch_shows_as_the_next_turn_note_and_an_undone_one_shows_nothing() {
    let arrow =
        wisp_render::theme::glyphs::Glyphs::of(wisp_render::theme::glyphs::GlyphSet::Unicode).arrow;
    let note = |what: &str| Some(format!("{arrow} {what} next turn"));
    let other = Some("other".to_owned());
    assert_eq!(
        resumed_note(&[("work", "claude-sonnet-5")]),
        (None, note("sonnet-5"))
    );
    assert_eq!(
        resumed_note(&[("other", "claude-opus-5-5")]),
        (other.clone(), None)
    );
    assert_eq!(
        resumed_note(&[("other", "claude-sonnet-5")]),
        (other, note("sonnet-5"))
    );
    assert_eq!(
        resumed_note(&[("other", "claude-sonnet-5"), ("work", "claude-opus-5-5")]),
        (None, None)
    );
    assert_eq!(resumed_note(&[]), (None, None));
}

#[test]
fn a_fallback_picks_the_endpoint_and_model_of_the_kept_turns() {
    let dir = &crate::scratch_dir("fallback-pick").root;
    let store = JsonlStore::open(dir).unwrap();
    let id = "01a167d6-0000-7000-8000-000000000000";
    let v = wisp_protocol::log::LOG_SCHEMA_VERSION;
    let lines = [
        format!(
            r#"{{"schema_version":{v},"seq":0,"at":"2026-10-25T10:00:00Z","type":"session_started","session_id":"{id}","wisp_version":"0.1.0","model":"claude-opus-5-5","effort":"medium","cache_ttl":"one_hour","endpoint":"https://api.anthropic.com","credential_source":"environment","endpoint_name":"work","cwd":"/project"}}"#
        ),
        format!(
            r#"{{"schema_version":{v},"seq":1,"at":"2026-10-25T10:00:01Z","type":"turn_started","turn":1,"text":"one","model":"claude-opus-5-5","effort":"medium","endpoint_name":"work"}}"#
        ),
        format!(
            r#"{{"schema_version":{v},"seq":2,"at":"2026-10-25T10:00:02Z","type":"message_committed","turn":1,"message":{{"role":"assistant","content":[{{"type":"text","text":"One."}}]}}}}"#
        ),
        format!(
            r#"{{"schema_version":{v},"seq":3,"at":"2026-10-25T10:00:02Z","type":"turn_ended","turn":1,"outcome":{{"status":"completed","stop_reason":"end_turn","model":"claude-opus-5-5","usage":{{"input_tokens":10,"output_tokens":2}},"cost":{{"turn_usd":0.01,"session_usd":0.01,"turn_lower_bound":false,"session_lower_bound":false}},"committed":true,"steps":1,"tool_calls":0}}}}"#
        ),
        format!(
            r#"{{"schema_version":{v},"seq":4,"at":"2026-10-25T10:01:00Z","type":"turn_started","turn":2,"text":"two","model":"claude-opus-5-5","effort":"medium","endpoint_name":"work"}}"#
        ),
        "#garbled#".to_owned(),
        format!(
            r#"{{"schema_version":{v},"seq":6,"at":"2026-10-25T10:01:02Z","type":"settings_changed","model":"claude-sonnet-5-5","effort":"low","cache":{{"type":"kept"}},"endpoint_name":"other","changes":[]}}"#
        ),
        format!(
            r#"{{"schema_version":{v},"seq":7,"at":"2026-10-25T10:02:00Z","type":"turn_started","turn":3,"text":"three","model":"claude-sonnet-5-5","effort":"low","endpoint_name":"other"}}"#
        ),
    ];
    std::fs::write(store.path(id), lines.join("\n") + "\n").unwrap();
    let request = Request {
        id: Some("01a167d6"),
        unlocked: false,
    };
    let resumed = open(Some(dir.clone()), &request, None, &|_| None).unwrap();
    assert!(resumed.recovered.fell_back.is_some());
    assert!(resumed.rebuilt.is_none());
    let logged = resumed.logged.as_ref().unwrap();
    assert_eq!(logged.endpoint(), Some("work"));
    assert_eq!(logged.current.model, "claude-opus-5-5");
    assert_eq!(logged.current.effort, wisp_protocol::Effort::Medium);
    assert!(logged.pending.is_none(), "{logged:#?}");
    assert_eq!(logged.tools_pair(), (Some("work"), "claude-opus-5-5"));
    drop(resumed);
    let mark = |name: &str| {
        let marks = BTreeMap::from([(EndpointName::parse(name).unwrap(), 4)]);
        let rows = wisp_core::resume::chat_rows(
            dir.clone(),
            "/project",
            None,
            jiff::Timestamp::now(),
            |name| name == "work",
            |name| crate::marked_in(&marks, name),
        )
        .unwrap();
        rows.iter().map(|row| row.mark).collect::<Vec<_>>()
    };
    assert_eq!(mark("work"), [Some(4)]);
    assert_eq!(mark("other"), [None]);
}

#[test]
fn the_resumed_screen_lists_the_processes_from_before_the_restart() {
    use wisp_core::session::SessionStore;
    let process = r#"{"schema_version":27,"seq":1,"at":"2026-10-25T10:01:00Z","type":"process_started","turn":1,"id":1,"call_id":"c1","reason":"started","tty":false,"stdin":false,"pid":999999,"sid":999999,"start_ticks":1,"boot_id":"not-this-boot"}"#;
    let ended = r#"{"schema_version":27,"seq":2,"at":"2026-10-25T10:02:00Z","type":"process_ended","id":1,"end":{"type":"exited","exit":{"type":"code","code":3}},"bytes":0,"lines":0,"duration_ms":40}"#;
    let search = r#"{"schema_version":27,"seq":3,"at":"2026-10-25T10:03:00Z","type":"native_web_search","endpoint_name":"work","state":"fallen_back"}"#;
    let text = [STARTED, process, ended, search].join("\n") + "\n";
    let records = wisp_core::session::parse_jsonl(text).unwrap();
    let store = wisp_core::session::MemoryStore::default();
    let mut writer = store.clone();
    for record in &records {
        writer.append("s1", record).unwrap();
    }
    let seq = u64::try_from(records.len()).unwrap();
    let agent = with_turn_lines(wisp_core::agent::Agent::new(
        std::sync::Arc::new(Silent(None)),
        wisp_core::agent::default_session(),
    ))
    .with_log(
        wisp_core::session::SessionLog::continuing(Box::new(store), "s1", seq),
        log_context("work"),
    )
    .resume(&records, 0)
    .unwrap();
    let logged = LoggedSession::read(&records).unwrap();
    let view = agent.view(&records);
    assert_eq!(
        view.started.as_ref().map(|pair| (
            pair.endpoint.as_str(),
            pair.model.as_str(),
            pair.effort
        )),
        Some((
            logged
                .started
                .endpoint_name
                .as_deref()
                .unwrap_or_else(|| agent.endpoint_name()),
            logged.started.model.as_str(),
            logged.started.effort
        ))
    );
    let events = view.events;
    assert!(
        events.iter().any(|e| matches!(
            e,
            wisp_protocol::AgentEvent::FeatureStateChanged {
                feature: wisp_protocol::Feature::NativeWebSearch,
                state: wisp_protocol::log::FeatureState::FallenBack,
                ..
            }
        )),
        "{events:?}"
    );
    let restored: Vec<&wisp_protocol::AgentEvent> = events
        .iter()
        .filter(|e| matches!(e, wisp_protocol::AgentEvent::ProcessRestored { .. }))
        .collect();
    assert_eq!(restored.len(), 1, "{events:?}");
    let wisp_protocol::AgentEvent::ProcessRestored {
        id, call_id, end, ..
    } = restored[0]
    else {
        panic!("{restored:?}");
    };
    assert_eq!((*id, call_id.as_deref()), (1, Some("c1")));
    assert_eq!(
        *end,
        Some(wisp_protocol::ProcessEnd::Exited {
            exit: wisp_protocol::ExitStatus::Code { code: 3 }
        })
    );
}

#[test]
fn resume_billing_mode_tracks_zero_usage_selection_and_explicit_mode_changes() {
    use wisp_protocol::Billing;
    let id = "01a167d6-0000-7000-8000-000000000000";
    let records = |events: Vec<serde_json::Value>| {
        let text = events
            .into_iter()
            .enumerate()
            .map(|(seq, mut event)| {
                event["schema_version"] = wisp_protocol::log::LOG_SCHEMA_VERSION.into();
                event["seq"] = seq.into();
                event["at"] = "2026-10-25T10:00:00Z".into();
                format!("{event}\n")
            })
            .collect::<String>();
        wisp_core::session::parse_jsonl(text).unwrap()
    };
    let start = serde_json::json!({"type":"session_started","session_id":id,"wisp_version":"0.1.0","model":"gpt-6.1-sol","effort":"medium","cache_ttl":"one_hour","endpoint":"https://chatgpt.com","endpoint_name":"work","credential_source":{"mode":{"billing":"chat_gpt_plan","source":"keyring"}}});
    let plan = records(vec![start.clone()]);
    assert_eq!(billing_mode(&plan, "work"), Some(Billing::ChatGptPlan));
    assert!(matches!(
        check_billing_mode(Some(Billing::ChatGptPlan), Billing::Metered, "work"),
        Err(wisp_core::resume::Refusal::Billing { .. })
    ));
    assert!(check_billing_mode(None, Billing::Metered, "work").is_ok());
    let api = serde_json::json!({"type":"session_resumed","wisp_version":"0.1.0","endpoint":"https://api.openai.com","endpoint_name":"work","credential_source":{"mode":{"billing":"metered","source":"keyring"}}});
    let changed = records(vec![start, api]);
    assert_eq!(billing_mode(&changed, "work"), Some(Billing::Metered));
    assert!(check_billing_mode(billing_mode(&changed, "work"), Billing::Metered, "work").is_ok());
    let switch = serde_json::json!({"type":"settings_changed","model":"gpt-6.1-sol","effort":"medium","cache":wisp_protocol::CacheEffect::Rebuilt {reason:wisp_protocol::RebuildReason::EndpointSwitch},"endpoint_name":"plan","endpoint":"https://chatgpt.com","credential_source":{"mode":{"billing":"chat_gpt_plan","source":"keyring"}}});
    let switched = records(vec![switch]);
    assert_eq!(billing_mode(&switched, "plan"), Some(Billing::ChatGptPlan));
}

fn session_records(
    name: &str,
    model: &str,
    source: serde_json::Value,
    billing: wisp_protocol::Billing,
) -> Vec<LogRecord> {
    let events = [
        serde_json::json!({"type":"session_started","session_id":"01a167d6-0000-7000-8000-000000000000","wisp_version":"0.1.0","model":model,"effort":"medium","cache_ttl":"one_hour","endpoint":"https://api.anthropic.com","endpoint_name":name,"credential_source":source}),
        serde_json::json!({"type":"step_ended","turn":1,"step":1,"usage":wisp_protocol::Usage { input_tokens: 10, billing, ..Default::default() },"cost_usd":0.001,"lower_bound":false}),
    ];
    let text = events
        .into_iter()
        .enumerate()
        .map(|(seq, mut event)| {
            event["schema_version"] = wisp_protocol::log::LOG_SCHEMA_VERSION.into();
            event["seq"] = seq.into();
            event["at"] = "2026-10-25T10:00:00Z".into();
            format!("{event}\n")
        })
        .collect::<String>();
    wisp_core::session::parse_jsonl(text).unwrap()
}

fn spawned(
    mut records: Vec<LogRecord>,
    agents: &[(&str, &str, Option<&str>, &str)],
) -> Vec<LogRecord> {
    for (agent, endpoint, provider, model) in agents {
        let event = serde_json::json!({"type":"agent_spawned","turn":1,"agent":agent,"session_id":agent,"depth":1,"model":model,"effort":"high","cwd":null,"config":{},"endpoint":endpoint,"provider":provider,"schema_version":wisp_protocol::log::LOG_SCHEMA_VERSION,"seq":records.len(),"at":"2026-10-25T10:00:00Z"});
        records.push(serde_json::from_value(event).unwrap());
    }
    records
}

fn billing_records(source: serde_json::Value, billing: wisp_protocol::Billing) -> Vec<LogRecord> {
    session_records("work", "claude-opus-5-5", source, billing)
}

fn mode(billing: &str) -> serde_json::Value {
    serde_json::json!({"mode":{"billing":billing,"source":"keyring"}})
}

pub(crate) fn sources_with(config: &str) -> crate::endpoints::Sources {
    use wisp_core::endpoints::{ConfigFile, PathShown, StartReason};
    let config = ConfigFile::parse(Path::new("config.toml"), PathShown::Default, config).unwrap();
    let off = wisp_core::plugin::registry::providers_off(&wisp_core::settings::start_switches(
        &[],
        &config.settings,
    ));
    crate::endpoints::Sources {
        inputs: crate::keychain::KeychainInputs::load(false, None, false, None)
            .with_env(&|name| (name == "TEST_KEY").then(|| "fixture-key".to_owned())),
        config_path: None,
        config_shown: PathShown::Default,
        default_config: None,
        config: Ok(Some(config)),
        secrets: Default::default(),
        choice: None,
        choice_reason: StartReason::Flag,
        variable: None,
        set: Vec::new(),
        off,
    }
}

fn resume_sources() -> crate::endpoints::Sources {
    let mut sources = sources_with(
        "[endpoints.work]\nauth = \"api_key\"\nkey_env = \"TEST_KEY\"\n[endpoints.direct]\nauth = \"api_key\"\nkey_env = \"TEST_KEY\"\n",
    );
    sources.choice = Some("direct".to_owned());
    sources
}

type Entries = std::collections::BTreeMap<EndpointName, Availability>;

fn api_key() -> wisp_core::endpoints::ResolvedEndpoint {
    resume_sources()
        .resolve()
        .map_err(|e| e.to_string())
        .unwrap()
        .active
}

fn account(name: &str, kind: &str) -> (EndpointName, Availability) {
    use wisp_core::provider::credential::{Credential, SubscriptionKind};
    let mut endpoint = api_key();
    endpoint.name = EndpointName::parse(name).unwrap();
    match kind {
        "claude-subscription" => {
            endpoint.credential = Credential::Subscription {
                kind: SubscriptionKind::Anthropic,
                secret: "fixture-setup-token".into(),
                profile: None,
            };
        }
        "chatgpt" => {
            endpoint.provider = ProviderKind::OpenAi;
            endpoint.base_url = ProviderKind::OpenAi.default_url().to_owned();
            endpoint.credential = Credential::Subscription {
                kind: SubscriptionKind::ChatGpt,
                secret: "fixture-token".into(),
                profile: None,
            };
        }
        "gateway" => endpoint.base_url = "https://gw.example.com".to_owned(),
        "openai-api" => {
            endpoint.provider = ProviderKind::OpenAi;
            endpoint.base_url = ProviderKind::OpenAi.default_url().to_owned();
        }
        _ => {}
    }
    (endpoint.name.clone(), Availability::Ready(endpoint))
}

fn unready(name: &str, reason: &str) -> (EndpointName, Availability) {
    (
        EndpointName::parse(name).unwrap(),
        Availability::Unavailable(wisp_core::endpoints::Unready::new(
            reason.to_owned(),
            ProviderKind::Anthropic,
        )),
    )
}

fn entries(accounts: &[(&str, &str)]) -> Entries {
    accounts
        .iter()
        .map(|(name, kind)| account(name, kind))
        .collect()
}

fn line_of(selected: Selected) -> (String, Option<String>) {
    match selected {
        Selected::Continue { name, left } => {
            let line = left.map(|left| left.continuing(&name));
            (name, line)
        }
        Selected::Ask(question) => panic!("asked: {:?}", question.choices),
        Selected::Refuse(line) => panic!("refused: {line}"),
    }
}

fn question_of(selected: Selected) -> Question {
    match selected {
        Selected::Ask(question) => question,
        Selected::Continue { name, .. } => panic!("continued on {name}"),
        Selected::Refuse(line) => panic!("refused: {line}"),
    }
}

#[test]
fn the_owners_case_continues_on_the_only_subscription_with_one_line() {
    let records = session_records(
        "env",
        "claude-opus-5-5",
        "env_file".into(),
        wisp_protocol::Billing::Metered,
    );
    let logged = LoggedSession::read(&records).unwrap();
    let found = entries(&[("claude-subscription", "claude-subscription")]);
    let selected = select_endpoint(
        &records,
        &logged,
        "env",
        &found,
        Some("claude-subscription"),
        false,
    );
    let (name, line) = line_of(selected);
    assert_eq!(name, "claude-subscription");
    let line = line.unwrap();
    assert_eq!(
        line,
        "wisp: session 01a167d6-0000 ran on the keys-file account; continuing on claude-subscription."
    );
    assert!(!line.contains(" env"), "{line}");

    let keys_file = |name: &str| {
        let mut found = entries(&[
            (name, "claude-api"),
            ("claude-subscription", "claude-subscription"),
        ]);
        found.insert(
            EndpointName::env(),
            Availability::SameAs(EndpointName::parse(name).unwrap()),
        );
        found
    };
    let found = keys_file("claude-api");
    let selected = select_endpoint(
        &records,
        &logged,
        "env",
        &found,
        Some("claude-subscription"),
        false,
    );
    assert_eq!(line_of(selected), ("env".to_owned(), None));

    let renamed = session_records(
        "claude-api",
        "claude-opus-5-5",
        "env_file".into(),
        wisp_protocol::Billing::Metered,
    );
    let logged = LoggedSession::read(&renamed).unwrap();
    let mut found = keys_file("gateway");
    found.remove(&EndpointName::parse("claude-subscription").unwrap());
    let (name, line) = line_of(select_endpoint(
        &renamed,
        &logged,
        "claude-api",
        &found,
        None,
        false,
    ));
    assert_eq!(name, "gateway");
    assert_eq!(
        line.as_deref(),
        Some("wisp: session 01a167d6-0000 ran on claude-api; continuing on gateway.")
    );
}

#[test]
fn a_resumed_chat_goes_by_the_one_rule_and_scripts_get_one_line() {
    let three = [
        ("claude-api", "claude-api"),
        ("claude-subscription", "claude-subscription"),
        ("claude-work", "claude-subscription"),
    ];
    let chatgpt = session_records(
        "chatgpt",
        "gpt-6.1-sol",
        mode("chat_gpt_plan"),
        wisp_protocol::Billing::ChatGptPlan,
    );
    let logged = LoggedSession::read(&chatgpt).unwrap();
    let question = question_of(select_endpoint(
        &chatgpt,
        &logged,
        "chatgpt",
        &entries(&three),
        Some("claude-subscription"),
        false,
    ));
    assert_eq!(
        question.script(),
        "wisp: session 01a167d6-0000 ran on chatgpt; rerun with --account and one of claude-subscription, claude-work or claude-api"
    );
    assert_eq!(
        question.choices,
        ["claude-subscription", "claude-work", "claude-api",]
    );
    assert_eq!(
        question.left.continuing("claude-work"),
        "wisp: session 01a167d6-0000 ran on chatgpt; continuing on claude-work."
    );

    let subscription = session_records(
        "claude-subscription",
        "claude-opus-5-5",
        mode("anthropic_plan"),
        wisp_protocol::Billing::AnthropicPlan,
    );
    let logged = LoggedSession::read(&subscription).unwrap();
    let mut left = entries(&[
        ("chatgpt", "chatgpt"),
        ("claude-api", "claude-api"),
        ("gateway", "gateway"),
        ("openai-api", "openai-api"),
    ]);
    left.extend([unready("claude-team", "its sign-in can't be read now")]);
    let agents = [
        ("/scout", "chatgpt", Some("openai"), "m"),
        ("/coder", "claude-subscription", Some("anthropic"), "m"),
        ("/helper", "claude-gone", Some("anthropic"), "m"),
        ("/tester", "claude-team", Some("anthropic"), "m"),
        ("/scribe", "claude-gone", None, "claude-haiku-4-5"),
        ("/old", "claude-gone", None, "m"),
        ("/future", "claude-gone", Some("future"), "claude-haiku-4-5"),
    ];
    let question = question_of(select_endpoint(
        &spawned(subscription.clone(), &agents),
        &logged,
        "claude-subscription",
        &left,
        Some("chatgpt"),
        false,
    ));
    assert_eq!(
        question.choices,
        ["chatgpt", "claude-api", "gateway", "openai-api",]
    );
    let mut book = resume_sources()
        .resolve()
        .map_err(|e| e.to_string())
        .unwrap();
    book.entries = left;
    let (facts, view) = crate::ask_facts(
        &book,
        &question.left.name,
        "claude-opus-5-5",
        wisp_protocol::Effort::High,
        wisp_protocol::CacheTtl::OneHour,
        Vec::new(),
    );
    let ways: Vec<_> = question
        .clone()
        .asked(facts, &view, None)
        .ways
        .into_iter()
        .map(|way| (way.chat, way.follow, way.stay, way.charges))
        .collect();
    let names = |names: &[&str]| -> Vec<String> { names.iter().map(|n| (*n).to_owned()).collect() };
    let all = names(&["coder", "future", "helper", "old", "scribe"]);
    let (moving, staying, none) = (
        names(&["coder", "helper", "scribe"]),
        names(&["future", "old"]),
        Vec::new(),
    );
    assert_eq!(
        ways,
        [
            (Some("chatgpt".to_owned()), none.clone(), all.clone(), false),
            (
                Some("claude-api".to_owned()),
                moving.clone(),
                staying.clone(),
                true
            ),
            (Some("gateway".to_owned()), moving, staying, true),
            (Some("openai-api".to_owned()), none, all, true),
        ]
    );
    assert_eq!(
        question.script(),
        "wisp: session 01a167d6-0000 ran on claude-subscription; rerun with --account and one of chatgpt, claude-api, gateway or openai-api"
    );
    let only_api = entries(&[("claude-api", "claude-api")]);
    let question = question_of(select_endpoint(
        &subscription,
        &logged,
        "claude-subscription",
        &only_api,
        Some("claude-api"),
        false,
    ));
    assert_eq!(
        question.script(),
        "wisp: session 01a167d6-0000 ran on claude-subscription; rerun with --account claude-api"
    );
    let work = entries(&[
        ("chatgpt", "chatgpt"),
        ("claude-api", "claude-api"),
        ("claude-work", "claude-subscription"),
    ]);
    let (name, line) = line_of(select_endpoint(
        &subscription,
        &logged,
        "claude-subscription",
        &work,
        Some("chatgpt"),
        false,
    ));
    assert_eq!(name, "claude-work");
    assert_eq!(
        line.as_deref(),
        Some("wisp: session 01a167d6-0000 ran on claude-subscription; continuing on claude-work.")
    );

    let dir = crate::scratch_dir("expired-profile");
    let profile = dir.root.join("oauth.json");
    wisp_core::secrets::write_atomic(&profile.with_extension("refused"), &[], 0o600).unwrap();
    let mut expired = work;
    expired.extend([account("claude-subscription", "claude-subscription")]);
    if let Some(Availability::Ready(endpoint)) =
        expired.get_mut(&EndpointName::parse("claude-subscription").unwrap())
    {
        endpoint.credential = wisp_core::provider::credential::Credential::Subscription {
            kind: wisp_core::provider::credential::SubscriptionKind::Anthropic,
            secret: "fixture-setup-token".into(),
            profile: Some(profile),
        };
    }
    let resumed = |asks| {
        select_endpoint(
            &subscription,
            &logged,
            "claude-subscription",
            &expired,
            Some("chatgpt"),
            asks,
        )
    };
    assert_eq!(
        line_of(resumed(false)),
        ("claude-subscription".to_owned(), None)
    );
    let question = question_of(resumed(true));
    assert_eq!(question.choices, ["claude-work", "chatgpt", "claude-api"]);
    assert_eq!(
        question.left.lead(&|_| None).plain(),
        "claude-subscription's sign-in expired"
    );
    assert_eq!(question.left.state, Some(State::Expired));
}

#[test]
fn an_account_that_cant_be_used_now_names_why_and_a_working_one_stays() {
    let records = billing_records("environment".into(), wisp_protocol::Billing::Metered);
    let logged = LoggedSession::read(&records).unwrap();
    let mut found = entries(&[("claude-subscription", "claude-subscription")]);
    found.extend([unready("work", "TEST_KEY is not set in the environment")]);
    let (name, line) = line_of(select_endpoint(
        &records,
        &logged,
        "work",
        &found,
        Some("claude-subscription"),
        false,
    ));
    assert_eq!(name, "claude-subscription");
    assert_eq!(
        line.as_deref(),
        Some(
            "wisp: session 01a167d6-0000 ran on work, which can't be used now (TEST_KEY is not set in the environment); continuing on claude-subscription."
        )
    );
    let still = entries(&[("work", "claude-api"), ("direct", "claude-api")]);
    let (name, line) = line_of(select_endpoint(
        &records,
        &logged,
        "work",
        &still,
        Some("direct"),
        false,
    ));
    assert_eq!((name.as_str(), line), ("work", None));
}

#[test]
fn a_line_never_shows_the_sessions_model() {
    let key = "sk-ant-api03-AbCdEfGh123CANARY0XyZ";
    let records = session_records(
        "work",
        key,
        "environment".into(),
        wisp_protocol::Billing::Metered,
    );
    let logged = LoggedSession::read(&records).unwrap();
    let found = entries(&[("direct", "claude-api")]);
    let question = question_of(select_endpoint(
        &records,
        &logged,
        "work",
        &found,
        Some("direct"),
        false,
    ));
    let mut said = vec![question.script(), question.left.continuing("direct")];
    said.extend(question.choices);
    for line in said {
        assert!(!line.contains(key), "{line}");
    }
}

#[test]
fn legacy_metered_sources_resume_and_move_without_authorization() {
    use wisp_protocol::Billing;
    for source in [
        "environment",
        "env_file",
        "keyring",
        "credential_file",
        "older-source",
    ] {
        let records = billing_records(source.into(), Billing::Metered);
        let logged = LoggedSession::read(&records).unwrap();
        assert_eq!(billing_mode(&records, "work"), None, "{source}");
        let book = resume_sources()
            .resolve()
            .map_err(|e| e.to_string())
            .unwrap();
        let (name, line) = line_of(select_endpoint(
            &records,
            &logged,
            "work",
            &book.entries,
            Some(book.start.as_str()),
            false,
        ));
        assert_eq!((name.as_str(), line), ("work", None), "{source}");
        let mut sources = resume_sources();
        sources
            .config
            .as_mut()
            .unwrap()
            .as_mut()
            .unwrap()
            .endpoints
            .remove(&EndpointName::parse("work").unwrap());
        let book = sources.resolve().map_err(|e| e.to_string()).unwrap();
        let (name, line) = line_of(select_endpoint(
            &records,
            &logged,
            "work",
            &book.entries,
            Some(book.start.as_str()),
            false,
        ));
        assert_eq!(name, "direct");
        assert_eq!(
            line.as_deref(),
            Some("wisp: session 01a167d6-0000 ran on work; continuing on direct.")
        );
    }
}

#[test]
fn billing_changes_name_both_modes_and_the_exact_authorization_flag() {
    use wisp_protocol::Billing;
    for (plan, label) in [
        (Billing::ChatGptPlan, "your ChatGPT subscription"),
        (Billing::AnthropicPlan, "your Claude subscription"),
    ] {
        for (saved, current, was, now) in [
            (Some(plan), Billing::Metered, label, "the API"),
            (Some(Billing::Metered), plan, "the API", label),
            (None, plan, "the API", label),
        ] {
            let error = check_billing_mode(saved, current, "work").unwrap_err();
            assert!(matches!(error, wisp_core::resume::Refusal::Billing { .. }));
            assert_eq!(
                error.to_string(),
                format!(
                    "this session used {was}; work uses {now}; rerun with --account work to continue on it"
                )
            );
        }
        assert!(check_billing_mode(Some(plan), plan, "work").is_ok());
        let records = billing_records("environment".into(), plan);
        assert_eq!(billing_mode(&records, "work"), Some(plan));
    }
}

#[test]
fn mode_logs_keep_resolution_and_missing_plan_authentication_guards() {
    use wisp_protocol::Billing;
    for billing in [
        Billing::Metered,
        Billing::ChatGptPlan,
        Billing::AnthropicPlan,
    ] {
        let records = billing_records(
            serde_json::json!({"mode":{"billing":billing,"source":"keyring"}}),
            Billing::Metered,
        );
        assert_eq!(billing_mode(&records, "work"), Some(billing));
        let logged = LoggedSession::read(&records).unwrap();
        let book = resume_sources()
            .resolve()
            .map_err(|e| e.to_string())
            .unwrap();
        let selected = select_endpoint(
            &records,
            &logged,
            "work",
            &book.entries,
            Some("direct"),
            false,
        );
        if billing.is_metered() {
            assert_eq!(line_of(selected), ("work".to_owned(), None));
            continue;
        }
        let Selected::Refuse(error) = selected else {
            panic!("{billing:?} wasn't refused");
        };
        assert!(error.contains("rerun with --account work"), "{error}");
        assert!(!error.contains("authentication"), "{error}");
        let Selected::Refuse(error) = select_endpoint(
            &records,
            &logged,
            "work",
            &Entries::new(),
            Some("direct"),
            false,
        ) else {
            panic!("{billing:?} with nothing left wasn't refused");
        };
        assert!(
            error.contains("authentication required for saved account work"),
            "{error}"
        );
        assert!(error.contains("sign in to work again"), "{error}");
        assert!(!error.contains("--endpoint work"), "{error}");
        assert!(matches!(
            authentication_required("work"),
            wisp_core::resume::Refusal::SignIn { .. }
        ));
    }
}

#[test]
fn a_legacy_api_session_follows_onto_a_subscription_without_consent() {
    use wisp_protocol::Billing;
    let records = billing_records("environment".into(), Billing::Metered);
    let logged = LoggedSession::read(&records).unwrap();
    let found = entries(&[("direct", "claude-subscription")]);
    let (name, line) = line_of(select_endpoint(
        &records,
        &logged,
        "work",
        &found,
        Some("direct"),
        false,
    ));
    assert_eq!(name, "direct");
    assert_eq!(
        line.as_deref(),
        Some("wisp: session 01a167d6-0000 ran on work; continuing on direct.")
    );
    let other = entries(&[("direct", "chatgpt")]);
    let question = question_of(select_endpoint(
        &records,
        &logged,
        "work",
        &other,
        Some("direct"),
        false,
    ));
    assert_eq!(question.choices, ["direct"]);
}

#[test]
fn a_subscription_session_never_moves_to_the_api_without_asking() {
    use wisp_protocol::Billing;
    let records = billing_records(mode("anthropic_plan"), Billing::AnthropicPlan);
    let logged = LoggedSession::read(&records).unwrap();
    let book = resume_sources()
        .resolve()
        .map_err(|e| e.to_string())
        .unwrap();
    let mut found = book.entries.clone();
    found.remove(&EndpointName::parse("work").unwrap());
    let question = question_of(select_endpoint(
        &records,
        &logged,
        "work",
        &found,
        Some("direct"),
        false,
    ));
    assert_eq!(question.choices, ["direct"]);
}

#[test]
fn a_gone_account_whose_provider_cant_be_told_asks_before_another_provider() {
    let found = entries(&[("claude-subscription", "claude-subscription")]);
    for (model, asks) in [("someday-1", true), ("claude-opus-5-5", false)] {
        let records = session_records(
            "relay",
            model,
            "environment".into(),
            wisp_protocol::Billing::Metered,
        );
        let logged = LoggedSession::read(&records).unwrap();
        let selected = select_endpoint(
            &records,
            &logged,
            "relay",
            &found,
            Some("claude-subscription"),
            false,
        );
        if asks {
            assert_eq!(
                question_of(selected).choices,
                ["claude-subscription"],
                "{model}"
            );
        } else {
            assert_eq!(line_of(selected).0, "claude-subscription", "{model}");
        }
    }
}

fn said_of(note: Option<wisp_tui::app::resumed::StartNote>) -> Option<String> {
    match note? {
        wisp_tui::app::resumed::StartNote::Said(said) => Some(said.plain()),
        wisp_tui::app::resumed::StartNote::SignIn(_) => None,
    }
}

#[test]
fn only_a_missing_start_account_still_says_it_is_gone() {
    let start = Left {
        session: None,
        name: "claude-team".to_owned(),
        why: Why::Gone,
        billing: None,
        state: Some(State::Gone),
        keys_file: None,
    };
    assert_eq!(
        start.continuing("direct"),
        "wisp: claude-team, where new chats start, is gone; this chat starts on direct."
    );
    assert!(start.continued(&|_| None).reason.is_some());
}

#[test]
fn a_plain_start_whose_start_account_is_gone_starts_by_the_rule() {
    use wisp_render::chrome::account::{Said, Who};
    use wisp_tui::app::resumed::StartNote;
    let started = |selected: Selected| match selected {
        Selected::Continue {
            name,
            left: Some(left),
        } => said_of(left.started(&name, &|_| None)),
        _ => None,
    };
    let said = |line: &str| Some(line.to_owned());
    let api = "[endpoints.direct]\nauth = \"api_key\"\nkey_env = \"TEST_KEY\"\n";
    let unusable = sources_with(&format!(
        "default = \"claude-work\"\n{api}[endpoints.claude-work]\nauth = \"api_key\"\nkey_env = \"WORK_KEY\"\n"
    ));
    let (name, line) = line_of(start_gone(&unusable, false).unwrap());
    assert_eq!(name, "direct");
    let line = line.unwrap();
    assert!(
        line.starts_with("wisp: claude-work, where new chats start, can't be used now ("),
        "{line}"
    );
    assert!(line.ends_with("; this chat starts on direct."), "{line}");
    assert_eq!(
        started(start_gone(&unusable, true).unwrap()),
        said("claude-work: WORK_KEY not set.")
    );

    let keys_file = sources_with(&format!("default = \"env\"\n{api}"));
    let (name, line) = line_of(start_gone(&keys_file, false).unwrap());
    assert_eq!(name, "direct");
    assert_eq!(
        line.as_deref(),
        Some(
            "wisp: the keys-file account, where new chats start, can't be used now; this chat starts on direct."
        )
    );
    assert_eq!(
        started(start_gone(&keys_file, true).unwrap()),
        said(
            "claude-api's key is missing from ~/.config/wisp/.env, so new chats now start on direct."
        )
    );
    let Some(Selected::Continue {
        name,
        left: Some(left),
    }) = start_gone(&keys_file, true)
    else {
        panic!("the keys file's start didn't continue");
    };
    let marks = |name: &str| (name == "direct").then_some(3);
    assert_eq!(
        left.started(&name, &marks),
        Some(StartNote::Said(Said::StartsOn(
            Who::Account("claude-api".to_owned(), None),
            Some("~/.config/wisp/.env".to_owned()),
            Who::Account("direct".to_owned(), Some(3)),
        )))
    );
    let Some(Selected::Continue {
        left: Some(keys_left),
        ..
    }) = start_gone(&keys_file, true)
    else {
        panic!("the keys file's start didn't continue");
    };
    assert_eq!(
        keys_left.lead(&|_| None).plain(),
        "claude-api's key is missing from ~/.config/wisp/.env"
    );
    let mut keychain = sources_with(&format!("default = \"env\"\n{api}"));
    keychain.inputs = crate::keychain::KeychainInputs::load(true, None, false, None);
    let note = "~/.config/wisp/.env and the ANTHROPIC_* variables aren't read on a machine with a keychain; wisp account import --from-environment stores them";
    assert_eq!(nothing_left(&keychain), Some(format!("{note}.")));
    let (unread, ..) = start_left(&keychain, true).unwrap();
    assert_eq!(unread.lead(&|_| None).plain(), note);
    assert_ne!(unread.state, Some(State::Gone));
    assert_eq!(
        said_of(unread.started("direct", &|_| None)),
        said("the keys-file account: can't be used now.")
    );
    for line in [unread.lead(&|_| None).plain(), unread.continuing("direct")] {
        let env = line
            .replace(wisp_protocol::DEFAULT_KEYS_FILE, "")
            .split(|c: char| !c.is_alphanumeric())
            .any(|w| w == "env");
        assert!(!env && !line.contains("key is missing"), "{line}");
    }
    let dir = crate::scratch_dir("gateway-keys");
    let file = dir.root.join("keys.env");
    std::fs::write(&file, "ANTHROPIC_BASE_URL=https://gw.example.com\n").unwrap();
    let mut gateway = sources_with(&format!("default = \"env\"\n{api}"));
    gateway.inputs = crate::keychain::KeychainInputs::load(false, Some(file), true, None)
        .with_env(&|name| (name == "TEST_KEY").then(|| "fixture-key".to_owned()));
    let Some(Selected::Continue {
        left: Some(gateway_left),
        ..
    }) = start_gone(&gateway, true)
    else {
        panic!("the gateway keys file's start didn't continue");
    };
    let lead = gateway_left.lead(&|_| None).plain();
    assert!(lead.starts_with("gateway's key is missing from "), "{lead}");
    assert!(lead.ends_with("keys.env"), "{lead}");

    let subscription = sources_with(&format!(
        "default = \"claude-work\"\n{api}[endpoints.claude-work]\nauth = \"anthropic_subscription\"\nkey_env = \"WORK_TOKEN\"\n"
    ));
    let question = question_of(start_gone(&subscription, true).unwrap());
    assert_eq!(question.choices, ["direct"]);
    assert_eq!(
        question.left.lead(&|_| None).plain(),
        "claude-work's key is missing"
    );
    assert!(
        question.script().ends_with("; rerun with --account direct"),
        "{}",
        question.script()
    );
    let selected = start_gone(&subscription, true).unwrap();
    assert_eq!(selected.first(), Some("direct"));
    let settled = |at, asks| start_gone(&subscription, true).unwrap().settle(at, asks);
    assert!(matches!(settled(None, true), Ok(Settled::Ask(q)) if q.choices == ["direct"]));
    assert_eq!(settled(None, false).err(), Some(question.script()));
    let Ok(Settled::Go { name, left }) = settled(Some("direct"), false) else {
        panic!("the card's answer wasn't taken");
    };
    assert_eq!(name, "direct");
    let left = left.unwrap();
    assert_eq!(left.name, "claude-work");
    assert_eq!(
        said_of(left.started(&name, &|_| None)),
        said("claude-work: WORK_TOKEN not set.")
    );

    for (state, lead) in [
        (State::Expired, "claude-work's sign-in expired"),
        (State::SignedOut, "claude-work is signed out"),
    ] {
        let left = Left {
            state: Some(state),
            ..left.clone()
        };
        assert_eq!(left.lead(&|_| None).plain(), lead);
        assert_eq!(
            left.started("direct", &|_| None),
            Some(StartNote::SignIn("claude-work".to_owned()))
        );
    }

    let mut chosen = subscription;
    chosen.choice = Some("direct".to_owned());
    assert!(start_gone(&chosen, true).is_none());
    assert!(start_gone(&sources_with(api), true).is_none());

    let models = crate::tui::models(&[]);
    for (typed, id) in [
        ("opus-5-5", Some("claude-opus-5-5")),
        ("claude-opus-5-5", Some("claude-opus-5-5")),
        ("claude-opus-55", None),
        ("opus-55", None),
        ("gw-own-model", None),
    ] {
        let named = named_model(&models, typed).map(|m| m.id.as_str());
        assert_eq!(named, id, "{typed}");
    }
    let account = |name: &str, provider: &str, auth: &str| {
        format!(
            "[endpoints.{name}]\nprovider = \"{provider}\"\nauth = \"{auth}\"\nkey_env = \"TEST_KEY\"\n"
        )
    };
    let openai = account("openai-api", "openai", "api_key");
    let claude = account("claude-subscription", "anthropic", "anthropic_subscription");
    let work = account("claude-work", "anthropic", "anthropic_subscription");
    let claude_api = account("claude-api", "anthropic", "api_key");
    let on = |start: &str, accounts: &[&str]| {
        sources_with(&format!("default = \"{start}\"\n{}", accounts.concat()))
    };
    let picked = |sources: &crate::endpoints::Sources, typed: &str| match start_model(
        sources,
        typed,
        &models,
        &[],
    ) {
        Some(StartModel::Pick { line, pick }) => (line, pick),
        _ => panic!("no picker for {typed}"),
    };
    let marks = |sources: &crate::endpoints::Sources, name: &str| {
        let marks = sources.marks(&sources.entries().unwrap());
        EndpointName::parse(name).and_then(|n| marks.get(&n).copied())
    };
    let here = on("claude-api", &[&claude_api, &openai]);
    assert!(marks(&here, "claude-api").is_some());
    assert!(start_model(&here, "opus-5-5", &models, &[]).is_none());
    let Some(StartModel::Continue { name, left, note }) =
        start_model(&here, "gpt-6.1-sol", &models, &[])
    else {
        panic!("gpt-6.1-sol didn't continue on openai-api");
    };
    assert_eq!(name, "openai-api");
    assert_eq!(
        left.continuing(&name),
        "wisp: claude-api, where new chats start, doesn't offer gpt-6.1-sol; this chat starts on openai-api."
    );
    assert_eq!(
        note,
        Said::Is(
            Who::Account("claude-api".to_owned(), marks(&here, "claude-api")),
            "doesn't offer gpt-6.1-sol".to_owned()
        )
    );
    let several = on("openai-api", &[&openai, &claude, &work, &claude_api]);
    let (line, pick) = picked(&several, "opus-5-5");
    assert_eq!(
        line,
        "wisp: openai-api doesn't offer opus-5-5; add --account claude-subscription, claude-work or claude-api to choose one."
    );
    assert_eq!(
        pick,
        StartPick {
            query: "opus-5-5".to_owned(),
            head: vec![Said::Lacks(
                Who::Account("openai-api".to_owned(), marks(&several, "openai-api")),
                "opus-5-5".to_owned()
            )],
        }
    );
    let (line, _) = picked(&several, "opus-55");
    assert_eq!(
        line,
        "wisp: no account offers opus-55. Available models: gpt-6.2-sol, gpt-6.2-astra, gpt-6.1-sol, gpt-6-astra, gpt-6.1-terra, gpt-6-luna, opus-5-5, sonnet-5-5, haiku-5-5."
    );
    let subscription = on("claude-subscription", &[&claude, &openai]);
    let (line, _) = picked(&subscription, "gpt-6.1-sol");
    assert_eq!(
        line,
        "wisp: claude-subscription doesn't offer gpt-6.1-sol; add --account openai-api to use it."
    );
    let (line, pick) = picked(&subscription, "opus-55");
    assert_eq!(
        line,
        "wisp: no account offers opus-55. Available models: opus-5-5, sonnet-5-5, haiku-5-5, gpt-6.2-sol, gpt-6.2-astra, gpt-6.1-sol, gpt-6-astra, gpt-6.1-terra, gpt-6-luna."
    );
    assert_eq!(
        pick.head,
        ["No account offers opus-55", "choose a model"].map(|t| Said::Text(t.to_owned()))
    );
    assert!(pick.query.is_empty());
    assert_eq!(
        model_on(&several, "claude-api", "claude-sonnet-5-5", &models, &[]).as_deref(),
        Some("claude-sonnet-5-5")
    );
    assert_eq!(
        model_on(&several, "openai-api", "claude-sonnet-5-5", &models, &[]).as_deref(),
        Some("gpt-6.2-sol")
    );
}

#[test]
fn a_started_model_goes_where_an_accounts_own_list_offers_it() {
    use wisp_core::endpoints::{ConfigFile, PathShown};
    use wisp_core::secrets::{Prefetched, Secret, Stored, StoredRecord};
    let ready = [connection(
        "chatgpt",
        Some(listed(&[
            ("gpt-6.1-sol", "list", "low"),
            ("gpt-reserve", "hide", "medium"),
        ])),
    )];
    let models = crate::tui::models(&ready);
    let scratch = crate::scratch_dir("start-own-list");
    let path = scratch.root.join("config.toml");
    let handle = "oauth:00000000000000000000000000000007";
    crate::auth::write_profile(&path, handle);
    let accounts = "store_id = \"5f0c2a9e41d7b388\"\n[endpoints.openai-api]\nprovider = \"openai\"\nauth = \"api_key\"\nkey_env = \"TEST_KEY\"\n[endpoints.chatgpt]\nprovider = \"openai\"\nauth = \"chatgpt_subscription\"\nkey_store = \"file\"\n";
    let stored = Stored {
        record: StoredRecord::Key {
            key: Secret::new(handle),
            url: None,
        },
        owner: None,
    };
    let on = |start: &str| {
        let text = format!("default = \"{start}\"\n{accounts}");
        let mut sources = sources_with(&text);
        sources.config = Ok(Some(
            ConfigFile::parse(&path, PathShown::Default, &text).unwrap(),
        ));
        let chatgpt = EndpointName::parse("chatgpt").unwrap();
        sources.secrets = Prefetched::from_records([(chatgpt, stored.clone())].into());
        sources
    };
    let from_api = start_model(&on("openai-api"), "gpt-reserve", &models, &ready);
    assert!(
        matches!(&from_api, Some(StartModel::Continue { name, .. }) if name == "chatgpt"),
        "gpt-reserve didn't move to chatgpt"
    );
    for typed in ["gpt-5.5", "gpt-reserve", "gpt-6.1-sol"] {
        assert!(
            start_model(&on("chatgpt"), typed, &models, &ready).is_none(),
            "{typed} left chatgpt"
        );
    }
    assert_eq!(
        model_on(&on("openai-api"), "chatgpt", "gpt-reserve", &models, &ready).as_deref(),
        Some("gpt-reserve")
    );
    let sources = sources_with(
        "default = \"gemini\"\n[endpoints.gemini]\nprovider = \"gemini\"\nauth = \"api_key\"\nkey_env = \"TEST_KEY\"\n",
    );
    let endpoint = sources.resolve().map_err(|e| e.to_string()).unwrap().active;
    let provider = endpoint.provider.connect(&endpoint, None).unwrap();
    let folder = scratch.root.join("models");
    let saved = wisp_core::models::list::SavedList {
        attempted: 0,
        failed: false,
        list: listed(&[("gemini-4-carbon-high", "list", "low")]),
        default_model: None,
        web_search_models: Vec::new(),
    };
    saved.save(&folder, provider.list_key().unwrap()).unwrap();
    let (ready, unavailable) =
        crate::endpoints::connections(&sources.entries().unwrap(), Some(&folder));
    assert!(unavailable.is_empty(), "{unavailable:?}");
    assert!(ready[0].provider.complete_model_list().is_some());
    let models = crate::tui::models(&ready);
    assert!(start_model(&sources, "gemini-4-carbon-high", &models, &ready).is_none());
    for model in ["gemini-4-carbon", "gemini-99-missing"] {
        let Some(StartModel::Pick { line, .. }) = start_model(&sources, model, &models, &ready)
        else {
            panic!("{model} was not refused");
        };
        assert!(
            line.starts_with(&format!("wisp: no account offers {model}.")),
            "{line}"
        );
        assert!(line.contains("gemini-4-carbon-high"), "{line}");
    }
    assert_eq!(
        model_on(&sources, "gemini", "gemini-4-carbon", &models, &ready).as_deref(),
        Some("gemini-4-carbon-high")
    );
    let (ready, _) = crate::endpoints::connections(&sources.entries().unwrap(), None);
    assert_eq!(
        model_on(&sources, "gemini", "gemini-4-carbon", &models, &ready).as_deref(),
        Some("gemini-4-carbon")
    );
    assert!(start_model(&sources, "gemini-4-carbon", &models, &ready).is_none());
}

#[test]
fn moving_picks_the_account_like_a_flag() {
    let mut sources = sources_with(
        "[endpoints.direct]\nauth = \"api_key\"\nkey_env = \"TEST_KEY\"\n[endpoints.work]\nauth = \"api_key\"\nkey_env = \"TEST_KEY\"\n",
    );
    move_to(&mut sources, "work");
    let book = sources.resolve().map_err(|e| e.to_string()).unwrap();
    assert_eq!(book.start.as_str(), "work");
}

#[test]
fn mixed_billing_change_uses_natural_wording() {
    let error = check_billing_mode(
        Some(wisp_protocol::Billing::Mixed),
        wisp_protocol::Billing::Metered,
        "work",
    )
    .unwrap_err();
    assert!(matches!(error, wisp_core::resume::Refusal::Billing { .. }));
    assert!(
        error
            .to_string()
            .starts_with("this session used both a subscription and the API;")
    );
}
