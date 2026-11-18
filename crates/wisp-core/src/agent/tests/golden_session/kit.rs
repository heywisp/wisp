use wisp_protocol::InstructionsSource;
use wisp_protocol::log::Rebuilt;

use super::*;
use crate::instructions::Paths;
use crate::kit::{self, Kit};
use crate::provider::RequestOptions;
use crate::resume::{LoggedSession, Recovered, recover};
use crate::session::reread;
use crate::settings::host::StartLayers;
use crate::test_guard::prefix;

const MODEL: &str = "claude-opus-4-1";

struct MemoryLogs(MemoryStore);

impl kit::OpenLog for MemoryLogs {
    fn open(&self, _path: &str) -> Result<Option<kit::OpenedLog>, String> {
        Ok(Some(kit::OpenedLog {
            log: SessionLog::new(Box::new(self.0.clone()), "s1"),
            path: PathBuf::from("s1.jsonl"),
            unlocked: None,
        }))
    }
}

fn choices() -> kit::Choices {
    kit::Choices {
        model: MODEL.into(),
        effort: Effort::High,
        cache_ttl: CacheTtl::OneHour,
        message_time: MessageTime::Utc,
        context_note: true,
    }
}

fn paths(env: &Env) -> Paths {
    Paths {
        start: env.work(),
        config_dir: Some(env.root.join("config")),
        home: Some(env.root.clone()),
    }
}

fn options() -> RequestOptions {
    RequestOptions {
        refusal_fallback: true,
        debug_cache: false,
        message_threads: true,
    }
}

fn ready(server: &Server) -> Vec<Connection> {
    vec![Connection {
        name: "env".into(),
        provider: server.provider(),
        context: log_context(),
        priced: true,
    }]
}

fn unavailable() -> Vec<(String, String)> {
    vec![("spare".into(), "no key is set".into())]
}

fn old_assembly(server: &Server, env: &Env, log: SessionLog) -> Agent {
    let choices = choices();
    let mut session = default_session();
    session.model = choices.model;
    session.effort = choices.effort;
    session.cache_ttl = choices.cache_ttl;
    session.message_time = choices.message_time;
    session.context_note = choices.context_note;
    let started = started(Tools::Default);
    let tools = started.toolbox;
    let agent = Agent::new(server.provider(), session)
        .with_request_options(options())
        .with_connections(ready(server), unavailable(), "env")
        .with_model_settings(ModelSettings::shipped_only())
        .with_retry_budget(Some(1.5))
        .with_count_tokens(true)
        .with_auto_compact(true)
        .with_web_search(tools.web_search())
        .with_user_input(true)
        .with_tools(tools, env.workspace())
        .with_tool_context(started.tool_context)
        .with_hooks(started.hooks, started.host)
        .with_instruction_parts(crate::agent::InstructionParts::auto())
        .with_config_dir(paths(env).config_dir.as_deref())
        .with_start_layers(StartLayers::default())
        .with_log(log, log_context())
        .with_clock(Arc::new(fixed_clock()));
    kit::pin_environment(agent)
}

fn kit_for(server: &Server, env: &Env, store: &MemoryStore) -> Kit {
    let provider = server.provider();
    let ready = ready(server);
    let paths = paths(env);
    Kit {
        connect: Arc::new(move |path: &str| {
            assert_eq!(path, kit::ROOT);
            Ok(kit::Connected {
                provider: provider.clone(),
                ready: ready.clone(),
                unavailable: unavailable(),
                active: "env".into(),
                env_account: None,
            })
        }),
        tools: Arc::new(plugin_tools(Tools::Default)),
        logs: Arc::new(MemoryLogs(store.clone())),
        log_context: log_context(),
        models: ModelSettings::shipped_only(),
        options: options(),
        retry_budget: Some(1.5),
        count_tokens: true,
        auto_compact: true,
        user_input: true,
        naming: false,
        workspace: Ok(env.workspace()),
        pin_test_environment: true,
        instructions: kit::InstructionsDirs {
            config_dir: paths.config_dir,
            home: paths.home,
        },
        plugin_data: None,
        start_layers: StartLayers::default(),
        settings_host: None,
        endpoint_host: None,
        clock: Arc::new(fixed_clock()),
        plan_usage: crate::plan_usage::PlanUsageCache::default(),
        usage_display: crate::plan_usage::Display::Disabled,
        warmth: crate::warmth::WarmthTable::default(),
        lanes: crate::lanes::Lanes::default(),
        mcp: crate::mcp::Service::default(),
    }
}

fn spec(start: kit::Start<'_>) -> kit::Spec<'_> {
    kit::Spec {
        path: kit::ROOT.into(),
        choices: choices(),
        instructions: kit::InstructionsChoice {
            source: InstructionsSource::Auto,
            warn_tokens: 8_000,
        },
        instructions_flag: None,
        extra: None,
        extra_flag: None,
        preset: None,
        start,
    }
}

async fn one_turn(agent: Agent, id: &str) -> Agent {
    let (commands, mut inbox) = mpsc::channel(64);
    let (sink, mut events) = mpsc::channel(8_192);
    let task = tokio::spawn(async move {
        let mut agent = agent;
        agent.serve(&mut inbox, &sink).await;
        agent
    });
    commands
        .send(ClientCommand::send(id, "hello"))
        .await
        .unwrap();
    let ended = until_turn_end(&mut events, &mut Vec::new()).await;
    let AgentEvent::TurnEnded { outcome, .. } = ended else {
        unreachable!()
    };
    assert!(completed(&outcome), "{outcome:?}");
    drop(commands);
    within("the agent's end", task).await.unwrap()
}

fn events(store: &MemoryStore) -> Vec<String> {
    records(store)
        .iter()
        .map(|r| {
            let mut event = serde_json::to_value(&r.event).unwrap();
            if let Some(fields) = event.as_object_mut() {
                fields.retain(|key, _| key != "stats" && key != "ms" && !key.ends_with("_ms"));
            }
            format!("{} {event}", r.seq)
        })
        .collect()
}

fn session_started(store: &MemoryStore) -> String {
    let records = records(store);
    let started = records
        .iter()
        .find(|r| matches!(r.event, LogEvent::SessionStarted { .. }))
        .unwrap();
    serde_json::to_string(&started.event).unwrap()
}

type Reopened = (
    SessionLog,
    Recovered,
    Option<LoggedSession>,
    Option<Box<Rebuilt>>,
);

fn reopen(store: &MemoryStore, server: &Server) -> Reopened {
    let records = records(store);
    let lines = reread(&records).unwrap();
    let provider = server.provider();
    let providers = |_: Option<&str>| Some(provider.clone());
    let mut recovered = recover(lines, store, &providers).unwrap();
    let (logged, rebuilt) = recovered.take_checked();
    let logged = (!recovered.fresh()).then(|| logged.unwrap());
    let seq = u64::try_from(records.len()).unwrap();
    let log = SessionLog::continuing(Box::new(store.clone()), "s1", seq);
    (log, recovered, logged, rebuilt)
}

fn script() -> Vec<Reply> {
    vec![says("msg_01", 400, "Hello."), says("msg_02", 520, "Again.")]
}

fn assert_pinned_environment(agent: &Agent) {
    let facts = agent
        .environment_facts()
        .expect("workspace facts for golden Kit tools");
    assert_eq!(facts.platform, "Linux");
    assert_eq!(facts.resources, None);
}

#[tokio::test]
async fn a_kit_built_agent_equals_the_old_assembly() {
    let env = Env::new("kit");
    fs::write(env.work().join("AGENTS.md"), "Answer briefly.\n").unwrap();

    let old_server = Server::start(script()).await;
    let old_store = MemoryStore::default();
    let log = SessionLog::new(Box::new(old_store.clone()), "s1");
    let (fresh, _) = old_assembly(&old_server, &env, log)
        .with_start_sections()
        .await;
    one_turn(fresh, "u1").await;

    let kit_server = Server::start(script()).await;
    let kit_store = MemoryStore::default();
    let kit = kit_for(&kit_server, &env, &kit_store);
    let built = kit.build(spec(kit::Start::Fresh)).await.unwrap();
    assert_pinned_environment(&built.agent);
    assert_eq!(
        built.log,
        kit::LogOutcome::Opened {
            path: PathBuf::from("s1.jsonl"),
            unlocked: None
        }
    );
    assert_eq!(built.workspace, None);
    assert_eq!(
        built.instructions.unwrap().chosen,
        Some(wisp_protocol::Scope::Project)
    );
    assert!(
        built
            .agent
            .session()
            .instructions
            .as_deref()
            .unwrap()
            .contains("Answer briefly.")
    );
    one_turn(built.agent, "u1").await;

    assert_eq!(session_started(&old_store), session_started(&kit_store));
    assert!(session_started(&kit_store).contains("Answer briefly."));
    let (old_seen, kit_seen) = (old_server.seen(), kit_server.seen());
    assert_eq!(old_seen.bodies.len(), 1);
    assert_eq!(old_seen.bodies[0], kit_seen.bodies[0]);
    assert_eq!(events(&old_store), events(&kit_store));

    let (log, recovered, logged, rebuilt) = reopen(&old_store, &old_server);
    assert!(logged.is_some());
    let (resumed, _, _) = old_assembly(&old_server, &env, log)
        .resume_rendered(&recovered, 0, logged.as_ref(), rebuilt)
        .await
        .unwrap();
    one_turn(resumed, "u2").await;

    let (log, recovered, logged, rebuilt) = reopen(&kit_store, &kit_server);
    let built = kit
        .build(spec(kit::Start::Resume(kit::Resume {
            log,
            recovered: &recovered,
            truncated_bytes: 0,
            logged: logged.as_ref(),
            rebuilt,
        })))
        .await
        .unwrap();
    assert_eq!(built.log, kit::LogOutcome::Resumed);
    assert_pinned_environment(&built.agent);
    one_turn(built.agent, "u2").await;

    let (old_seen, kit_seen) = (old_server.seen(), kit_server.seen());
    assert_eq!(old_seen.bodies.len(), 2);
    assert_eq!(old_seen.bodies, kit_seen.bodies);
    assert_eq!(old_seen.counts, kit_seen.counts);
    assert_eq!(events(&old_store), events(&kit_store));
}

async fn logged_once(case: &str) -> (Env, Server, MemoryStore, Kit) {
    let env = Env::new(case);
    fs::write(env.work().join("AGENTS.md"), "Answer briefly.\n").unwrap();
    let server = Server::start(script()).await;
    let store = MemoryStore::default();
    let kit = kit_for(&server, &env, &store);
    let built = kit.build(spec(kit::Start::Fresh)).await.unwrap();
    one_turn(built.agent, "u1").await;
    (env, server, store, kit)
}

#[tokio::test]
async fn a_resume_whose_directory_is_gone_runs_in_the_current_one() {
    let (env, server, store, mut kit) = logged_once("kit-gone").await;
    fs::remove_dir_all(env.work()).unwrap();
    kit.workspace = Ok(Workspace {
        root: env.root.clone(),
        home: Some(env.root.clone()),
        temp: env.root.join("tmp"),
    });
    let (log, recovered, logged, rebuilt) = reopen(&store, &server);
    assert_eq!(
        logged.as_ref().unwrap().started.cwd.as_deref(),
        Some("~/work")
    );
    let built = kit
        .build(spec(kit::Start::Resume(kit::Resume {
            log,
            recovered: &recovered,
            truncated_bytes: 0,
            logged: logged.as_ref(),
            rebuilt,
        })))
        .await
        .unwrap();
    assert_eq!(
        built.workspace,
        Some(kit::WorkspaceNote::Gone("~/work".into()))
    );
    assert_eq!(
        built.instructions.unwrap().summary.candidates[0].display_path,
        "~/AGENTS.md"
    );
}

#[tokio::test]
async fn a_resume_that_logged_no_directory_keeps_its_instructions_source() {
    let (_env, server, store, kit) = logged_once("kit-no-directory").await;
    let (_, _, logged, _) = reopen(&store, &server);
    assert_eq!(
        logged.unwrap().started.instructions_source,
        Some(InstructionsSource::Auto)
    );
    let store = sections::copied(&store, |record| {
        if let LogEvent::SessionStarted {
            cwd,
            instructions_source,
            ..
        } = &mut record.event
        {
            *cwd = None;
            *instructions_source = Some(InstructionsSource::None);
        }
    });
    let (log, recovered, logged, rebuilt) = reopen(&store, &server);
    let logged = logged.unwrap();
    let spec = spec(kit::Start::Resume(kit::Resume {
        log,
        recovered: &recovered,
        truncated_bytes: 0,
        logged: Some(&logged),
        rebuilt,
    }));
    assert_eq!(
        spec.instructions(),
        kit::InstructionsChoice {
            source: InstructionsSource::None,
            warn_tokens: 8_000,
        }
    );
    let built = kit.build(spec).await.unwrap();
    assert_eq!(built.workspace, Some(kit::WorkspaceNote::NoDirectory));
    let loaded = built.instructions.unwrap();
    assert_eq!(
        loaded.summary.candidates[0].display_path,
        "~/work/AGENTS.md"
    );
    assert_eq!(loaded.chosen, None);
}

#[tokio::test]
async fn a_main_preset_under_source_none_is_the_whole_block_with_no_precedence_clause() {
    let env = Env::new("kit-main-preset");
    fs::write(env.work().join("AGENTS.md"), "Answer briefly.\n").unwrap();
    let brain = env.root.join("config/BRAIN.md");
    fs::create_dir_all(env.root.join("config")).unwrap();
    fs::write(&brain, "Teach gently.\n").unwrap();
    let server = Server::start(script()).await;
    let store = MemoryStore::default();
    let kit = kit_for(&server, &env, &store);
    let mut fresh = spec(kit::Start::Fresh);
    fresh.instructions.source = InstructionsSource::None;
    fresh.preset = Some(kit::PresetSource {
        preset: "brain".into(),
        path: brain,
    });
    let built = kit.build(fresh).await.unwrap();
    assert_eq!(
        built.agent.session().instructions.as_deref(),
        Some(
            "<instructions>\nInstructions for your preset follow, from the file named by Preset instructions in <environment>. File names are relative to that file's directory.\n<file scope=\"preset\" name=\"BRAIN.md\">\nTeach gently.\n</file>\n</instructions>"
        )
    );
    one_turn(built.agent, "u1").await;
    assert!(server.seen().bodies[0].contains("Teach gently."));
    let (_, _, logged, _) = reopen(&store, &server);
    assert_eq!(logged.unwrap().started.preset.as_deref(), Some("brain"));
}

#[tokio::test]
async fn on_resume_the_instructions_flag_wins_over_the_log_and_config_never_does() {
    let (_env, server, store, _kit) = logged_once("kit-source-choice").await;
    let (log, recovered, logged, rebuilt) = reopen(&store, &server);
    let logged = logged.unwrap();
    assert_eq!(logged.instructions_source, Some(InstructionsSource::Auto));
    let mut spec = spec(kit::Start::Resume(kit::Resume {
        log,
        recovered: &recovered,
        truncated_bytes: 0,
        logged: Some(&logged),
        rebuilt,
    }));
    spec.instructions.source = InstructionsSource::Default;
    assert_eq!(spec.instructions().source, InstructionsSource::Auto);
    spec.instructions_flag = Some(InstructionsSource::None);
    assert_eq!(spec.instructions().source, InstructionsSource::None);
    let mut newer = logged.clone();
    newer.instructions_source = Some(InstructionsSource::Default);
    let (log, recovered, _, rebuilt) = reopen(&store, &server);
    let mut later = spec_resumed(log, &recovered, &newer, rebuilt);
    later.instructions.source = InstructionsSource::None;
    assert_eq!(later.instructions().source, InstructionsSource::Default);
    let mut fresh = self::spec(kit::Start::Fresh);
    fresh.instructions.source = InstructionsSource::Default;
    assert_eq!(fresh.instructions().source, InstructionsSource::Default);
    fresh.instructions_flag = Some(InstructionsSource::None);
    assert_eq!(fresh.instructions().source, InstructionsSource::None);
}

#[tokio::test]
async fn on_resume_the_extra_flag_wins_over_the_newest_logged_extra_and_config_never_does() {
    use crate::instructions::Extra;
    let (_env, server, store, _kit) = logged_once("kit-extra-choice").await;
    let (log, recovered, logged, rebuilt) = reopen(&store, &server);
    let mut logged = logged.unwrap();
    logged.started.instructions_extra = Some("autonomous".into());
    logged.instructions_extra = Some("autonomous".into());
    let mut spec = spec_resumed(log, &recovered, &logged, rebuilt);
    spec.extra = None;
    assert_eq!(spec.extra(), Some(Extra::Autonomous));
    spec.extra_flag = Some(None);
    assert_eq!(spec.extra(), None);
    let mut newer = logged.clone();
    newer.instructions_extra = Some("none".into());
    let (log, recovered, _, rebuilt) = reopen(&store, &server);
    let mut later = spec_resumed(log, &recovered, &newer, rebuilt);
    later.extra = Some(Extra::Autonomous);
    assert_eq!(later.extra(), None);
    later.extra_flag = Some(Some(Extra::Autonomous));
    assert_eq!(later.extra(), Some(Extra::Autonomous));
    let mut fresh = self::spec(kit::Start::Fresh);
    fresh.extra = Some(Extra::Autonomous);
    assert_eq!(fresh.extra(), Some(Extra::Autonomous));
}

fn spec_resumed<'a>(
    log: SessionLog,
    recovered: &'a Recovered,
    logged: &'a LoggedSession,
    rebuilt: Option<Box<Rebuilt>>,
) -> kit::Spec<'a> {
    spec(kit::Start::Resume(kit::Resume {
        log,
        recovered,
        truncated_bytes: 0,
        logged: Some(logged),
        rebuilt,
    }))
}

#[tokio::test]
async fn the_roots_requests_warm_the_kits_table() {
    let (_env, server, _store, kit) = logged_once("kit-warmth").await;
    assert_eq!(kit.warmth.heats(), vec![crate::warmth::Heat::Warm]);
    assert_eq!(server.seen().bodies.len(), 1);
}

fn sent_value<'a>(body: &'a [u8], key: &str, from: usize) -> (&'a [u8], usize) {
    let needle = format!("\"{key}\":");
    let at = from
        + body[from..]
            .windows(needle.len())
            .position(|w| w == needle.as_bytes())
            .unwrap()
        + needle.len();
    let mut values = serde_json::Deserializer::from_slice(&body[at..]).into_iter::<Value>();
    values.next().unwrap().unwrap();
    let end = at + values.byte_offset();
    (&body[at..end], end)
}

#[tokio::test]
async fn the_warmth_key_covers_the_tools_the_plugin_registry_built() {
    let (_env, server, _store, kit) = logged_once("kit-warmth-plugins").await;
    let body = server.seen().bodies[0].clone().into_bytes();
    let (model, _) = sent_value(&body, "model", 0);
    let (thinking, _) = sent_value(&body, "thinking", 0);
    let (effort, _) = sent_value(&body, "output_config", 0);
    let (tools, end) = sent_value(&body, "tools", 0);
    let (system, _) = sent_value(&body, "system", end);
    let sent = std::str::from_utf8(tools).unwrap();
    for name in crate::tool::testkit::ALL.iter().chain(&["process"]) {
        assert!(sent.contains(&format!("\"name\":\"{name}\"")), "{sent}");
    }
    let print = crate::warmth::Fingerprint::new(
        "env",
        &crate::provider::PrefixBytes {
            model: model.to_vec(),
            effort: effort.to_vec(),
            thinking: thinking.to_vec(),
            tools: tools.to_vec(),
            system: system.to_vec(),
        },
    );
    assert_eq!(kit.warmth.heat(print.key()), crate::warmth::Heat::Warm);
    assert_eq!(kit.warmth.heats(), vec![crate::warmth::Heat::Warm]);
}

#[tokio::test]
async fn a_failed_request_leaves_its_prefix_cold_and_a_later_one_warms_it() {
    let env = Env::new("kit-warmth-failed");
    let server = Server::start(vec![server_error(), says("msg_01", 400, "Hello.")]).await;
    let store = MemoryStore::default();
    let kit = kit_for(&server, &env, &store);
    let mut built = kit.build(spec(kit::Start::Fresh)).await.unwrap();
    built.agent = built
        .agent
        .with_retry_policy(crate::agent::retry::RetryPolicy {
            step_retries: Some(0),
            ..crate::agent::retry::RetryPolicy::default()
        });
    let (commands, mut inbox) = mpsc::channel(64);
    let (sink, mut events) = mpsc::channel(8_192);
    let task = tokio::spawn(async move {
        let mut agent = built.agent;
        agent.serve(&mut inbox, &sink).await;
    });
    let mut ended = async |id: &str| {
        commands
            .send(ClientCommand::send(id, "hello"))
            .await
            .unwrap();
        let ended = until_turn_end(&mut events, &mut Vec::new()).await;
        let AgentEvent::TurnEnded { outcome, .. } = ended else {
            unreachable!()
        };
        outcome
    };
    let failed = ended("u1").await;
    assert!(!completed(&failed), "{failed:?}");
    assert_eq!(kit.warmth.requests(), 1);
    assert_eq!(kit.warmth.heats(), Vec::new());
    let outcome = ended("u2").await;
    assert!(completed(&outcome), "{outcome:?}");
    assert_eq!(kit.warmth.heats(), vec![crate::warmth::Heat::Warm]);
    drop(ended);
    drop(commands);
    within("the agent's end", task).await.unwrap();
}

fn extra_spec(start: kit::Start<'_>, extra: Option<crate::instructions::Extra>) -> kit::Spec<'_> {
    kit::Spec {
        extra,
        ..spec(start)
    }
}

#[tokio::test]
async fn autonomous_instructions_are_logged_and_kept_across_turns_and_a_resume_with_another_flag() {
    use crate::instructions::Extra;
    let env = Env::new("kit-extra");
    fs::write(env.work().join("AGENTS.md"), "Answer briefly.\n").unwrap();
    let server = Server::start(vec![
        says("msg_01", 400, "Hello."),
        says("msg_02", 520, "Again."),
        says("msg_03", 640, "Still here."),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = kit_for(&server, &env, &store);
    let built = kit
        .build(extra_spec(kit::Start::Fresh, Some(Extra::Autonomous)))
        .await
        .unwrap();
    let block = built.agent.session().instructions.clone().unwrap();
    assert!(block.starts_with(&format!("{}\n<instructions>\n", Extra::Autonomous.block())));
    let agent = one_turn(built.agent, "u1").await;
    one_turn(agent, "u2").await;
    let started = session_started(&store);
    assert!(
        started.contains("\"instructions_extra\":\"autonomous\""),
        "{started}"
    );
    let (log, recovered, logged, rebuilt) = reopen(&store, &server);
    let resumed_spec = extra_spec(
        kit::Start::Resume(kit::Resume {
            log,
            recovered: &recovered,
            truncated_bytes: 0,
            logged: logged.as_ref(),
            rebuilt,
        }),
        None,
    );
    assert_eq!(resumed_spec.extra(), Some(Extra::Autonomous));
    let built = kit.build(resumed_spec).await.unwrap();
    one_turn(built.agent, "u3").await;
    let bodies = server.seen().bodies;
    assert_eq!(bodies.len(), 3);
    for pair in bodies.windows(2) {
        assert!(
            super::binding::extends(&pair[0], &pair[1]),
            "{}\n{}",
            pair[0],
            pair[1]
        );
    }
    let first: Value = serde_json::from_str(&bodies[0]).unwrap();
    assert_eq!(first["system"][1]["text"], json!(block));
}

#[tokio::test]
async fn an_extra_name_this_build_does_not_know_reads_as_none() {
    let (_env, server, store, _kit) = logged_once("kit-extra-unknown").await;
    let (log, recovered, logged, rebuilt) = reopen(&store, &server);
    let mut logged = logged.unwrap();
    assert_eq!(logged.started.instructions_extra, None);
    logged.started.instructions_extra = Some("someday".into());
    let spec = extra_spec(
        kit::Start::Resume(kit::Resume {
            log,
            recovered: &recovered,
            truncated_bytes: 0,
            logged: Some(&logged),
            rebuilt,
        }),
        Some(crate::instructions::Extra::Autonomous),
    );
    assert_eq!(spec.extra(), None);
}

#[tokio::test]
async fn without_extra_instructions_the_session_logs_no_name() {
    let (_env, _server, store, _kit) = logged_once("kit-extra-none").await;
    assert!(!session_started(&store).contains("instructions_extra"));
}

mod legacy;
mod sections;
mod switches;

const EDITED: &str = "Answer at length.\n";

fn start_renders(store: &MemoryStore) -> usize {
    records(store)
        .iter()
        .filter(|r| {
            matches!(&r.event, LogEvent::HookRan { hook, point, .. }
                if hook == "instructions/instructions" && point == "section-start")
        })
        .count()
}

fn chosen_sha(agent: &Agent) -> String {
    agent
        .instructions
        .as_ref()
        .unwrap()
        .summary
        .chosen
        .as_ref()
        .unwrap()
        .sha256
        .clone()
}

#[tokio::test]
async fn a_resume_renders_once_and_sends_the_logged_block_and_summary() {
    let (env, server, store, kit) = logged_once("kit-resume-once").await;
    let logged_block = logged_block(&store);
    fs::write(env.work().join("AGENTS.md"), EDITED).unwrap();
    let (log, recovered, logged, rebuilt) = reopen(&store, &server);
    let logged = logged.unwrap();
    let built = kit
        .build(spec_resumed(log, &recovered, &logged, rebuilt))
        .await
        .unwrap();
    assert_eq!(
        built.instructions.unwrap().summary.chosen.unwrap().sha256,
        wisp_instructions::sha256_hex(EDITED)
    );
    assert_eq!(built.agent.session().instructions, Some(logged_block));
    assert_eq!(
        chosen_sha(&built.agent),
        wisp_instructions::sha256_hex("Answer briefly.\n")
    );
    one_turn(built.agent, "u2").await;
    assert_eq!(start_renders(&store), 2);
}

fn flag(name: &str, value: &str) -> crate::settings::Given {
    crate::settings::Given {
        target: wisp_protocol::settings::Target::Name(wisp_protocol::settings::find(name).unwrap()),
        value: wisp_protocol::settings::SettingValue::Text(value.to_owned()),
        origin: format!("--set {name}"),
    }
}

fn logged_block(store: &MemoryStore) -> String {
    log_events(store)
        .iter()
        .find_map(|e| match e {
            LogEvent::SessionStarted { instructions, .. } => instructions.clone(),
            _ => None,
        })
        .unwrap()
}

#[derive(Clone, Copy, Debug)]
enum Fresh {
    Digest,
    Source,
    Extra,
}

#[tokio::test]
async fn a_damaged_digest_or_a_source_or_extra_flag_sends_the_fresh_block_and_summary() {
    use crate::instructions::Extra;
    for fresh in [Fresh::Digest, Fresh::Source, Fresh::Extra] {
        let (env, server, store, mut kit) = logged_once(&format!("kit-fresh-{fresh:?}")).await;
        fs::write(env.work().join("AGENTS.md"), EDITED).unwrap();
        let mut logged_store = store.clone();
        if let Fresh::Digest = fresh {
            let copy = MemoryStore::default();
            let mut writer = copy.clone();
            for mut record in records(&store) {
                if let LogEvent::SessionStarted {
                    instructions_sha256,
                    ..
                } = &mut record.event
                {
                    *instructions_sha256 = Some("damaged".into());
                }
                writer.append("s1", &record).unwrap();
            }
            logged_store = copy;
        }
        let (log, recovered, logged, rebuilt) = reopen(&logged_store, &server);
        let logged = logged.unwrap();
        let mut spec = spec_resumed(log, &recovered, &logged, rebuilt);
        match fresh {
            Fresh::Digest => {}
            Fresh::Source => {
                kit.start_layers.flags = vec![flag("instructions.source", "default")];
                spec.instructions_flag = Some(InstructionsSource::Default);
            }
            Fresh::Extra => {
                kit.start_layers.flags = vec![flag("instructions.extra", "autonomous")];
                spec.extra_flag = Some(Some(Extra::Autonomous));
            }
        }
        let built = kit.build(spec).await.unwrap();
        let block = built.agent.session().instructions.clone();
        let state = built.agent.instructions.as_ref().unwrap();
        match fresh {
            Fresh::Source => {
                assert_eq!(block, None, "{fresh:?}");
                assert_eq!(state.summary.chosen, None, "{fresh:?}");
                assert!(
                    logged
                        .instructions_loaded
                        .as_ref()
                        .unwrap()
                        .chosen
                        .is_some(),
                    "{fresh:?}"
                );
            }
            Fresh::Digest | Fresh::Extra => {
                let block = block.unwrap();
                assert!(block.contains("Answer at length."), "{fresh:?}: {block}");
                assert!(!block.contains("Answer briefly."), "{fresh:?}: {block}");
                assert_eq!(
                    chosen_sha(&built.agent),
                    wisp_instructions::sha256_hex(EDITED),
                    "{fresh:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn after_a_resume_whose_config_source_changed_the_renders_read_the_logged_source() {
    let (_env, server, store, mut kit) = logged_once("kit-config-source").await;
    let source = wisp_protocol::settings::find("instructions.source").unwrap();
    let mut saved = crate::config::SavedSettings::default();
    saved.insert(
        wisp_protocol::settings::SitePath::global(source),
        wisp_protocol::settings::SettingValue::Text("none".into()),
        1,
    );
    kit.start_layers.saved = saved;
    let (log, recovered, logged, rebuilt) = reopen(&store, &server);
    let mut spec = spec_resumed(log, &recovered, logged.as_ref().unwrap(), rebuilt);
    spec.instructions.source = InstructionsSource::None;
    let built = kit.build(spec).await.unwrap();
    assert_eq!(
        built.instructions.unwrap().chosen,
        Some(wisp_protocol::Scope::Project)
    );
    let agent = one_turn(built.agent, "u2").await;
    let applied = agent.hooks.applied.as_ref().unwrap();
    assert_eq!(
        applied.settings.get("instructions.source"),
        Some(&Some("auto".to_owned()))
    );
    let bodies = server.seen().bodies;
    let first = bodies.last().unwrap();
    assert!(
        !first.contains("<instructions-update>"),
        "the first resumed turn sends no delta: {first}"
    );
}

#[tokio::test]
async fn a_log_from_before_the_plugin_resumes_with_a_legacy_base_holding_the_logged_block() {
    use wisp_plugin_api::prompt::Prior;
    let (_env, server, store, kit) = logged_once("kit-legacy-base").await;
    let block = logged_block(&store);
    let older = MemoryStore::default();
    let mut writer = older.clone();
    for mut record in records(&store) {
        if let LogEvent::SessionStarted { sections, .. } = &mut record.event {
            assert_eq!(sections.len(), 1);
            sections.clear();
        }
        writer.append("s1", &record).unwrap();
    }
    let snapshot = Prior::Snapshot(format!(
        "{} project AGENTS.md\n",
        wisp_instructions::sha256_hex("Answer briefly.\n")
    ));
    let legacy = Prior::Legacy(Some(block.clone()));
    for (logs, want) in [(&older, legacy), (&store, snapshot)] {
        let (log, recovered, logged, rebuilt) = reopen(logs, &server);
        let logged = logged.unwrap();
        let built = kit
            .build(spec_resumed(log, &recovered, &logged, rebuilt))
            .await
            .unwrap();
        let agent = built.agent;
        assert_eq!(agent.session().instructions.as_ref(), Some(&block));
        assert_eq!(agent.section_base("instructions/instructions"), want);
        assert_eq!(
            agent.sections.committed().get("instructions/instructions"),
            Some(&want)
        );
    }
}

fn body(server: &Server, n: usize) -> Value {
    serde_json::from_str(&server.seen().bodies[n]).unwrap()
}

#[tokio::test]
async fn a_skill_found_and_read_appends_only_results_to_the_request() {
    let env = Env::new("skill-found-read");
    let folder = env.root.join(".agents/skills/guide");
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("SKILL.md"), "---\nname: guide\ndescription: Placeholder project guide\n---\n# Guide\n\nPlaceholder skill body.\n").unwrap();
    let server = Server::start(vec![
        calls(
            "msg_01",
            400,
            vec![call("toolu_01", "search", json!({"query":"guide"}))],
        ),
        calls(
            "msg_02",
            500,
            vec![call("toolu_02", "use", json!({"name":"guide"}))],
        ),
        says("msg_03", 600, "Done."),
    ])
    .await;
    let store = MemoryStore::default();
    let (registry, problems) = crate::plugin::Registry::new(
        vec![wisp_plugin_search::bundled(), wisp_plugin_skills::bundled()],
        &crate::plugin::PluginConfig::default(),
    );
    assert!(problems.is_empty(), "{problems:?}");
    let mut kit = kit_for(&server, &env, &store);
    kit.tools = Arc::new(crate::plugin::Tools::new(
        Arc::new(registry),
        ["search".to_owned(), "use".to_owned()]
            .into_iter()
            .collect(),
    ));
    let built = kit.build(spec(kit::Start::Fresh)).await.unwrap();
    let _agent = one_turn(built.agent, "u1").await;
    let seen = server.seen();
    let requests: Vec<Value> = seen
        .bodies
        .iter()
        .map(|body| serde_json::from_str(body).unwrap())
        .collect();
    assert_eq!(requests.len(), 3);
    prefix::assert_prefix_stable(&requests, None);
    assert!(
        !requests[0]["system"]
            .to_string()
            .contains("Placeholder project guide")
    );
    let tail = requests[2]["messages"].to_string();
    assert!(tail.contains("Placeholder skill body.") && tail.contains("~/.agents/skills/guide"));
    assert!(!tail.contains("description: Placeholder"));
    check("skill_found_read", &seen);
}
