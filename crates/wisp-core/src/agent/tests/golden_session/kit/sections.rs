use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use wisp_plugin_api::manifest::Origin;
use wisp_plugin_api::prompt::{At, Prior};
use wisp_plugin_testkit::hooks::{self as testkit, Calls, Knob, Section};
use wisp_protocol::log::SectionSnapshot;
use wisp_protocol::{AgentPath, ParentLink, RewindPoint};

use super::*;
use crate::instructions::Extra;
use crate::plugin::caller::Caller;
use crate::plugin::{Make, PluginConfig, Registry, Source};

const RULES: &str = "bb/rules";
const WISP: &str = "[models.\"claude-opus-4-1\"]\nnative_compaction = false\n";

struct Rules {
    knob: Knob,
    calls: Calls,
    read: Arc<Mutex<Vec<String>>>,
    hang: Arc<AtomicBool>,
    started: Arc<Notify>,
    origin: Origin,
}

impl Rules {
    fn new(origin: Origin) -> Self {
        Self {
            knob: testkit::knob("v1"),
            calls: Calls::default(),
            read: Arc::default(),
            hang: Arc::default(),
            started: Arc::default(),
            origin,
        }
    }

    fn turn_to(&self, value: &str) {
        testkit::turn_to(&self.knob, value);
    }

    fn section(&self) -> Section {
        let inner = testkit::changing(self.knob.clone());
        let read = self.read.clone();
        let hang = self.hang.clone();
        let started = self.started.clone();
        Arc::new(move |host, call| {
            if call.at == At::Start {
                read.lock()
                    .unwrap()
                    .push(format!("{:?} {}", host.get("time"), host.root()));
            }
            let hangs = call.at == At::Start && hang.load(Ordering::SeqCst);
            let inner = inner.clone();
            let started = started.clone();
            Box::pin(async move {
                if hangs {
                    started.notify_one();
                    std::future::pending::<()>().await;
                }
                inner(host, call).await
            })
        })
    }

    fn registry(&self) -> Arc<Registry> {
        let mut sources: Vec<Source> = wisp_plugins::bundled()
            .into_iter()
            .map(|b| {
                let make: Make = Arc::new(move |host| (b.instantiate)(host));
                Source::in_process(b.manifest.to_owned(), make, Origin::Bundled)
            })
            .collect();
        let body = format!(
            "settings = [\"time\"]\n{}",
            testkit::prompt_section("rules", "system", "")
        );
        let make: Make = testkit::prompted(testkit::none(), self.section(), self.calls.clone());
        sources.push(Source::in_process(
            testkit::manifest_in("0.2.0", "bb", &body),
            make,
            self.origin,
        ));
        let (registry, problems) = Registry::from_origins(sources, &PluginConfig::default());
        assert!(problems.is_empty(), "{problems:?}");
        Arc::new(registry)
    }

    fn kit(&self, server: &Server, env: &Env, store: &MemoryStore) -> Kit {
        let provider = server.provider();
        let ready = ready(server);
        let mut kit = kit_for(server, env, store);
        kit.connect = Arc::new(move |_: &str| {
            Ok(kit::Connected {
                provider: provider.clone(),
                ready: ready.clone(),
                unavailable: unavailable(),
                active: "env".into(),
                env_account: None,
            })
        });
        kit.tools = Arc::new(crate::plugin::Tools::new(self.registry(), BTreeSet::new()));
        kit
    }

    fn last_turn_previous(&self) -> Option<Prior> {
        self.calls
            .sections_at(At::Turn)
            .last()
            .map(|c| c.previous.clone())
    }

    fn read(&self) -> Vec<String> {
        self.read.lock().unwrap().clone()
    }

    fn cancel_when_started(&self) -> CancellationToken {
        let token = CancellationToken::new();
        let cancel = token.clone();
        let started = self.started.clone();
        tokio::spawn(async move {
            within("the hook's start", started.notified()).await;
            cancel.cancel();
        });
        token
    }
}

fn given(origin: &str, name: &str, value: &str) -> crate::settings::Given {
    use wisp_protocol::settings::{SettingValue, Target, find};
    crate::settings::Given {
        target: Target::Name(find(name).unwrap()),
        value: SettingValue::Text(value.to_owned()),
        origin: origin.to_owned(),
    }
}

fn source_none(kit: &mut Kit) {
    kit.start_layers.flags = vec![given("--instructions", "instructions.source", "none")];
}

fn rules_text(value: &str) -> String {
    testkit::section_text("rules", value)
}

fn logged_rules(value: &str) -> Vec<SectionSnapshot> {
    vec![SectionSnapshot {
        name: RULES.into(),
        snapshot: value.into(),
    }]
}

fn with_project_snapshot(mut sections: Vec<SectionSnapshot>) -> Vec<SectionSnapshot> {
    sections.push(SectionSnapshot {
        name: "instructions/instructions".into(),
        snapshot: format!(
            "{} project AGENTS.md\n",
            wisp_instructions::sha256_hex("Answer briefly.\n")
        ),
    });
    sections
}

fn system_of(body: &Value) -> String {
    body["system"].to_string()
}

fn rules_sent(body: &Value) -> usize {
    body["messages"].to_string().matches("<rules>").count()
}

fn sent_of(body: &Value, value: &str) -> usize {
    body["messages"]
        .to_string()
        .matches(&format!("<rules>\\n{value}\\n</rules>"))
        .count()
}

fn resumed_records(store: &MemoryStore) -> Vec<LogEvent> {
    log_events(store)
        .into_iter()
        .filter(|e| matches!(e, LogEvent::SessionResumed { .. }))
        .collect()
}

fn resumed_sections(store: &MemoryStore) -> Vec<Option<Vec<SectionSnapshot>>> {
    resumed_records(store)
        .into_iter()
        .map(|e| match e {
            LogEvent::SessionResumed { sections, .. } => sections,
            _ => unreachable!(),
        })
        .collect()
}

fn resumed_block(store: &MemoryStore) -> Vec<Option<String>> {
    resumed_records(store)
        .into_iter()
        .map(|e| match e {
            LogEvent::SessionResumed { instructions, .. } => instructions,
            _ => unreachable!(),
        })
        .collect()
}

fn says_n(n: usize) -> Vec<Reply> {
    (1..=n)
        .map(|i| says(&format!("msg_{i:02}"), 400 + 10 * i as u64, "Fine."))
        .collect()
}

fn damaged(store: &MemoryStore) -> MemoryStore {
    copied(store, |record| {
        if let LogEvent::SessionStarted {
            instructions_sha256,
            ..
        } = &mut record.event
        {
            *instructions_sha256 = Some("damaged".into());
        }
    })
}

pub(super) fn copied(store: &MemoryStore, change: impl Fn(&mut LogRecord)) -> MemoryStore {
    let copy = MemoryStore::default();
    let mut writer = copy.clone();
    for mut record in records(store) {
        change(&mut record);
        writer.append("s1", &record).unwrap();
    }
    copy
}

async fn resume_root(kit: &Kit, store: &MemoryStore, server: &Server, flags: Flags) -> kit::Built {
    let (log, recovered, logged, rebuilt) = reopen(store, server);
    let logged = logged.unwrap();
    let mut spec = spec_resumed(log, &recovered, &logged, rebuilt);
    match flags {
        Flags::Plain => {}
        Flags::Source => spec.instructions_flag = Some(InstructionsSource::None),
        Flags::Extra => spec.extra_flag = Some(Some(Extra::Autonomous)),
    }
    kit.build(spec).await.unwrap()
}

#[derive(Clone, Copy)]
enum Flags {
    Plain,
    Source,
    Extra,
}

#[derive(Clone, Copy)]
enum Fresh {
    Source,
    Extra,
    Digest,
}

async fn the_fresh_block_on_a_root_resume(case: &str, fresh: Fresh) {
    let rules = Rules::new(Origin::Bundled);
    let env = Env::new(case);
    fs::write(env.work().join("AGENTS.md"), "Answer briefly.\n").unwrap();
    let server = Server::start(says_n(3)).await;
    let first = MemoryStore::default();
    let plain = rules.kit(&server, &env, &first);
    let built = plain.build(spec(kit::Start::Fresh)).await.unwrap();
    one_turn(built.agent, "u1").await;
    rules.turn_to("v2");
    let mut flagged = rules.kit(&server, &env, &first);
    let (store, flags) = match fresh {
        Fresh::Source => {
            source_none(&mut flagged);
            (first.clone(), Flags::Source)
        }
        Fresh::Extra => {
            flagged.start_layers.flags = vec![given(
                "--instructions-extra",
                "instructions.extra",
                "autonomous",
            )];
            (first.clone(), Flags::Extra)
        }
        Fresh::Digest => (damaged(&first), Flags::Plain),
    };
    let built = resume_root(&flagged, &store, &server, flags).await;
    assert!(
        built.plugin_problems.is_empty(),
        "{:?}",
        built.plugin_problems
    );
    let block = built.agent.session.instructions.clone().unwrap();
    assert!(block.ends_with(&rules_text("v2")), "{block}");
    one_turn(built.agent, "u2").await;
    let fresh_sections = match fresh {
        Fresh::Source => logged_rules("v2"),
        Fresh::Extra | Fresh::Digest => with_project_snapshot(logged_rules("v2")),
    };
    assert_eq!(resumed_sections(&store), [Some(fresh_sections.clone())]);
    let resumed = body(&server, 1);
    assert!(
        system_of(&resumed).contains("<rules>\\nv2\\n</rules>"),
        "{resumed}"
    );
    assert!(!system_of(&resumed).contains("<rules>\\nv1"), "{resumed}");
    assert_eq!(rules_sent(&resumed), 0, "{resumed}");
    assert_eq!(
        rules.last_turn_previous(),
        Some(Prior::Snapshot("v2".into()))
    );

    let built = resume_root(&plain, &store, &server, Flags::Plain).await;
    one_turn(built.agent, "u3").await;
    assert_eq!(resumed_sections(&store), [Some(fresh_sections), None]);
    let later = body(&server, 2);
    assert_eq!(system_of(&later), system_of(&resumed));
    assert_eq!(rules_sent(&later), 0, "{later}");
    assert_eq!(
        rules.last_turn_previous(),
        Some(Prior::Snapshot("v2".into()))
    );
}

#[tokio::test]
async fn a_root_resume_with_a_source_flag_sends_the_fresh_block_and_logs_its_sections() {
    the_fresh_block_on_a_root_resume("kit-sections-source", Fresh::Source).await;
}

#[tokio::test]
async fn a_root_resume_with_an_extra_flag_sends_the_fresh_block_and_logs_its_sections() {
    the_fresh_block_on_a_root_resume("kit-sections-extra", Fresh::Extra).await;
}

#[tokio::test]
async fn a_root_resume_over_a_damaged_digest_sends_the_fresh_block_and_logs_its_sections() {
    the_fresh_block_on_a_root_resume("kit-sections-digest", Fresh::Digest).await;
}

#[tokio::test]
async fn a_root_resume_with_a_flag_resolving_to_the_same_text_keeps_the_cache_and_logs_its_sections()
 {
    let rules = Rules::new(Origin::Bundled);
    let env = Env::new("kit-sections-same");
    let server = Server::start(says_n(2)).await;
    let store = MemoryStore::default();
    let plain = rules.kit(&server, &env, &store);
    let built = plain.build(spec(kit::Start::Fresh)).await.unwrap();
    assert_eq!(
        built.agent.session.instructions.as_deref(),
        Some(rules_text("v1").as_str())
    );
    one_turn(built.agent, "u1").await;
    let mut flagged = rules.kit(&server, &env, &store);
    source_none(&mut flagged);
    let built = resume_root(&flagged, &store, &server, Flags::Source).await;
    let notice = built
        .agent
        .resume_notice_text()
        .unwrap_or_default()
        .to_owned();
    assert!(!notice.contains("rebuilt"), "{notice}");
    assert!(!notice.contains("flags"), "{notice}");
    assert!(built.agent.cache_rebuild.is_none());
    one_turn(built.agent, "u2").await;
    assert_eq!(resumed_sections(&store), [Some(logged_rules("v1"))]);
    assert_eq!(resumed_block(&store), [None]);
    assert_eq!(system_of(&body(&server, 1)), system_of(&body(&server, 0)));
    assert_eq!(rules_sent(&body(&server, 1)), 0);
}

fn child_at(cwd: PathBuf, source: InstructionsSource, time: MessageTime) -> kit::ChildSpec {
    kit::ChildSpec {
        path: AgentPath::new("/review"),
        model: MODEL.into(),
        effort: Effort::High,
        cache_ttl: CacheTtl::OneHour,
        message_time: time,
        context_note: false,
        instructions: kit::InstructionsChoice {
            source,
            warn_tokens: 8_000,
        },
        cwd,
        tools: Vec::new(),
        parent: ParentLink {
            session_id: "s-root".into(),
            agent: AgentPath::new("/review"),
            depth: 1,
            config: wisp_protocol::SpawnChoices::default(),
            todos: Vec::new(),
        },
        endpoint: "env".into(),
        preset_instructions: None,
        plugins: crate::plugin::PluginSwitches::default(),
    }
}

async fn resume_child(
    kit: &Kit,
    store: &MemoryStore,
    server: &Server,
    spec: kit::ChildSpec,
) -> Agent {
    let (log, recovered, logged, rebuilt) = reopen(store, server);
    let resume = kit::Resume {
        log,
        recovered: &recovered,
        truncated_bytes: 0,
        logged: logged.as_ref(),
        rebuilt,
    };
    let (agent, missing) = kit.resume_child(spec, resume).await.unwrap();
    assert!(missing.is_empty(), "{missing:?}");
    agent
}

#[tokio::test]
async fn a_childs_resume_over_a_damaged_digest_sends_the_fresh_block_and_logs_its_sections() {
    let rules = Rules::new(Origin::Bundled);
    let env = Env::new("kit-sections-child");
    fs::write(env.work().join("AGENTS.md"), "Answer briefly.\n").unwrap();
    let server = Server::start(says_n(3)).await;
    let first = MemoryStore::default();
    let plain = rules.kit(&server, &env, &first);
    let auto = || child_at(env.work(), InstructionsSource::Auto, MessageTime::Off);
    let child = plain.build_child(auto()).await.unwrap();
    let block = child.session.instructions.clone().unwrap();
    assert!(block.contains("Answer briefly."), "{block}");
    assert!(block.ends_with(&rules_text("v1")), "{block}");
    one_turn(child, "u1").await;
    rules.turn_to("v2");
    let store = damaged(&first);
    let plain = rules.kit(&server, &env, &store);
    let child = resume_child(&plain, &store, &server, auto()).await;
    let block = child.session.instructions.clone().unwrap();
    assert!(block.contains("Answer briefly."), "{block}");
    assert!(block.ends_with(&rules_text("v2")), "{block}");
    one_turn(child, "u2").await;
    assert_eq!(
        resumed_sections(&store),
        [Some(with_project_snapshot(logged_rules("v2")))]
    );
    let resumed = body(&server, 1);
    assert_eq!(rules_sent(&resumed), 0, "{resumed}");
    assert_eq!(
        rules.last_turn_previous(),
        Some(Prior::Snapshot("v2".into()))
    );
    let child = resume_child(&plain, &store, &server, auto()).await;
    one_turn(child, "u3").await;
    assert_eq!(
        resumed_sections(&store),
        [Some(with_project_snapshot(logged_rules("v2"))), None]
    );
    let later = body(&server, 2);
    assert_eq!(system_of(&later), system_of(&resumed));
    assert_eq!(rules_sent(&later), 0, "{later}");
    assert_eq!(
        rules.last_turn_previous(),
        Some(Prior::Snapshot("v2".into()))
    );
}

#[tokio::test]
async fn a_childs_resume_takes_no_flag_and_keeps_its_logged_block_and_cache() {
    let rules = Rules::new(Origin::Bundled);
    let env = Env::new("kit-sections-child-same");
    let server = Server::start(says_n(2)).await;
    let store = MemoryStore::default();
    let plain = rules.kit(&server, &env, &store);
    let child = plain
        .build_child(child_at(
            env.work(),
            InstructionsSource::Auto,
            MessageTime::Off,
        ))
        .await
        .unwrap();
    one_turn(child, "u1").await;
    let mut flagged = rules.kit(&server, &env, &store);
    source_none(&mut flagged);
    let none = child_at(env.work(), InstructionsSource::None, MessageTime::Off);
    let child = resume_child(&flagged, &store, &server, none).await;
    let notice = child.resume_notice_text().unwrap_or_default().to_owned();
    assert!(!notice.contains("rebuilt"), "{notice}");
    assert!(child.cache_rebuild.is_none());
    assert_eq!(
        child.session.instructions.as_deref(),
        Some(rules_text("v1").as_str())
    );
    one_turn(child, "u2").await;
    assert_eq!(resumed_sections(&store), [None]);
    assert_eq!(resumed_block(&store), [None]);
    assert_eq!(system_of(&body(&server, 1)), system_of(&body(&server, 0)));
}

#[tokio::test]
async fn the_start_render_reads_an_owned_setting_and_the_root_fresh_resumed_and_spawned() {
    let rules = Rules::new(Origin::Bundled);
    let env = Env::new("kit-sections-read");
    let server = Server::start(says_n(1)).await;
    let store = MemoryStore::default();
    let plain = rules.kit(&server, &env, &store);
    let mut local = spec(kit::Start::Fresh);
    local.choices.message_time = MessageTime::Local;
    let built = plain.build(local).await.unwrap();
    one_turn(built.agent, "u1").await;
    let mut flagged = rules.kit(&server, &env, &store);
    flagged.start_layers.flags = vec![
        given("--instructions", "instructions.source", "none"),
        given("--set time", "time", "off"),
    ];
    let resumed = resume_root(&flagged, &store, &server, Flags::Source).await;
    assert_eq!(
        resumed.agent.session.instructions.as_deref(),
        Some(rules_text("v1").as_str())
    );
    drop(resumed);
    let elsewhere = env.root.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    let child = plain
        .build_child(child_at(
            elsewhere.clone(),
            InstructionsSource::None,
            MessageTime::Off,
        ))
        .await
        .unwrap();
    drop(child);
    let root = env.work().display().to_string();
    assert_eq!(
        rules.read(),
        [
            format!("Some(\"local\") {root}"),
            format!("Some(\"local\") {root}"),
            format!("Some(\"off\") {}", elsewhere.display()),
        ]
    );
}

#[tokio::test]
async fn a_cancelled_start_render_warns_and_leaves_no_base_fresh_and_spawned() {
    let rules = Rules::new(Origin::External);
    rules.hang.store(true, Ordering::SeqCst);
    let env = Env::new("kit-sections-cancel");
    let server = Server::start(Vec::new()).await;
    let store = MemoryStore::default();
    let kit = rules.kit(&server, &env, &store);
    let token = rules.cancel_when_started();
    let built = Caller::call(token, None)
        .scope(kit.build(spec(kit::Start::Fresh)))
        .await
        .unwrap();
    assert_eq!(
        built
            .plugin_problems
            .iter()
            .map(|p| p.plugin.as_str())
            .collect::<Vec<_>>(),
        ["bb"]
    );
    assert_eq!(built.agent.session.instructions, None);
    assert_eq!(built.agent.section_base(RULES), Prior::None);
    assert!(built.agent.start_sections().is_empty());
    let token = rules.cancel_when_started();
    let child = Caller::call(token, None)
        .scope(kit.build_child(child_at(
            env.work(),
            InstructionsSource::Auto,
            MessageTime::Off,
        )))
        .await
        .unwrap();
    assert_eq!(child.session.instructions, None);
    assert_eq!(child.section_base(RULES), Prior::None);
    assert!(child.start_sections().is_empty());
}

#[tokio::test]
async fn a_cancelled_start_render_on_a_resume_sending_the_logged_block_keeps_the_logged_base() {
    let rules = Rules::new(Origin::External);
    let env = Env::new("kit-sections-cancel-resume");
    let server = Server::start(says_n(2)).await;
    let store = MemoryStore::default();
    let kit = rules.kit(&server, &env, &store);
    let built = kit.build(spec(kit::Start::Fresh)).await.unwrap();
    one_turn(built.agent, "u1").await;
    rules.hang.store(true, Ordering::SeqCst);
    let token = rules.cancel_when_started();
    let built = Caller::call(token, None)
        .scope(resume_root(&kit, &store, &server, Flags::Plain))
        .await;
    assert_eq!(
        built.plugin_problems.len(),
        1,
        "{:?}",
        built.plugin_problems
    );
    assert_eq!(
        built.agent.section_base(RULES),
        Prior::Snapshot("v1".into())
    );
    assert_eq!(
        built.agent.session.instructions.as_deref(),
        Some(rules_text("v1").as_str())
    );
    rules.hang.store(false, Ordering::SeqCst);
    one_turn(built.agent, "u2").await;
    assert_eq!(resumed_sections(&store), [None]);
    assert_eq!(system_of(&body(&server, 1)), system_of(&body(&server, 0)));
    assert_eq!(rules_sent(&body(&server, 1)), 0);
    assert_eq!(
        rules.last_turn_previous(),
        Some(Prior::Snapshot("v1".into()))
    );
}

fn bad_request() -> Reply {
    Reply {
        response: http::json(
            "400 Bad Request",
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"bad request"}}"#,
        ),
        before: Vec::new(),
        gate: None,
    }
}

fn live(agent: Agent, server: &Server) -> Live {
    Live::on(agent, server, Arc::new(step_clock()))
}

async fn rewind(live: &mut Live, turn: u64, n: u64) {
    live.send(ClientCommand::Rewind {
        to: RewindPoint::Turn { turn },
        request: request(n),
    })
    .await;
    let done = live
        .until(|e| {
            matches!(e, AgentEvent::Rewound { .. })
                || matches!(e, AgentEvent::Rejected { request: Some(r), .. } if *r == request(n))
        })
        .await;
    assert!(matches!(done, AgentEvent::Rewound { .. }), "{done:?}");
}

struct Rewinding {
    rules: Rules,
    env: Env,
    server: Server,
    store: MemoryStore,
}

impl Rewinding {
    async fn new(case: &str, script: Vec<Reply>) -> Self {
        let rules = Rules::new(Origin::Bundled);
        rules.turn_to("v0");
        Self {
            rules,
            env: Env::new(case),
            server: Server::start(script).await,
            store: MemoryStore::default(),
        }
    }

    fn kit(&self) -> Kit {
        let mut kit = self.rules.kit(&self.server, &self.env, &self.store);
        kit.models = models(WISP);
        kit
    }

    async fn started(&self) -> Live {
        let built = self.kit().build(spec(kit::Start::Fresh)).await.unwrap();
        live(built.agent, &self.server)
    }

    async fn resumed_fresh(&self) -> Live {
        let mut kit = self.kit();
        source_none(&mut kit);
        let built = resume_root(&kit, &self.store, &self.server, Flags::Source).await;
        live(built.agent, &self.server)
    }

    async fn resumed(&self) -> Live {
        let built = resume_root(&self.kit(), &self.store, &self.server, Flags::Plain).await;
        live(built.agent, &self.server)
    }

    fn previous(&self) -> Option<Prior> {
        self.rules.last_turn_previous()
    }

    fn last_sent(&self, value: &str) -> usize {
        let bodies = self.server.seen().bodies;
        sent_of(
            &serde_json::from_str(bodies.last().unwrap()).unwrap(),
            value,
        )
    }
}

#[tokio::test]
async fn a_rewind_to_turn_one_across_a_fresh_block_resume_starts_from_the_resumed_snapshot() {
    let rig = Rewinding::new("kit-sections-rewind-one", says_n(4)).await;
    let mut live = rig.started().await;
    rig.rules.turn_to("v1");
    live.completed("u1", "one").await;
    assert_eq!(rig.previous(), Some(Prior::Snapshot("v0".into())));
    assert_eq!(rig.last_sent("v1"), 1);
    live.close().await;
    rig.rules.turn_to("v2");
    let mut live = rig.resumed_fresh().await;
    live.completed("u2", "two").await;
    assert_eq!(resumed_sections(&rig.store), [Some(logged_rules("v2"))]);
    rewind(&mut live, 1, 1).await;
    live.completed("u3", "three").await;
    live.close().await;
    assert_eq!(rig.previous(), Some(Prior::Snapshot("v2".into())));
    assert_eq!(rig.last_sent("v1") + rig.last_sent("v2"), 0);
    let mut live = rig.resumed().await;
    rig.rules.turn_to("v3");
    live.completed("u4", "four").await;
    live.close().await;
    assert_eq!(rig.previous(), Some(Prior::Snapshot("v2".into())));
}

#[tokio::test]
async fn a_rewind_past_a_head_logged_before_a_fresh_block_resume_starts_from_the_resumed_snapshot()
{
    let script = vec![
        says("msg_01", 400, "One."),
        greeted("msg_02", 400),
        says("msg_03", 300, "Two."),
        says("msg_04", 320, "Three."),
        says("msg_05", 340, "Four."),
    ];
    let rig = Rewinding::new("kit-sections-rewind-head", script).await;
    let mut live = rig.started().await;
    rig.rules.turn_to("v1");
    live.completed("u1", "one").await;
    live.compacted(1).await;
    live.completed("u2", "two").await;
    assert_eq!(rig.previous(), Some(Prior::Snapshot("v1".into())));
    live.close().await;
    let heads: Vec<_> = pick!(
        log_events(&rig.store),
        LogEvent::ContextCompacted { sections, .. } => sections
    );
    assert_eq!(heads, [logged_rules("v1")]);
    rig.rules.turn_to("v2");
    let mut live = rig.resumed_fresh().await;
    live.completed("u3", "three").await;
    rewind(&mut live, 3, 2).await;
    live.completed("u4", "four").await;
    live.close().await;
    assert_eq!(rig.previous(), Some(Prior::Snapshot("v2".into())));
    assert_eq!((rig.last_sent("v1"), rig.last_sent("v2")), (1, 0));
}

#[tokio::test]
async fn a_failed_turn_and_a_rewound_turn_after_a_fresh_block_resume_never_move_the_snapshot() {
    let script = vec![
        says("msg_01", 400, "One."),
        bad_request(),
        says("msg_02", 420, "Three."),
        says("msg_03", 440, "Four."),
    ];
    let rig = Rewinding::new("kit-sections-rewind-failed", script).await;
    let mut live = rig.started().await;
    live.completed("u1", "one").await;
    live.close().await;
    rig.rules.turn_to("v2");
    let mut live = rig.resumed_fresh().await;
    rig.rules.turn_to("v3");
    let failed = live.turn("u2", "two").await;
    assert!(!completed(&failed), "{failed:?}");
    assert_eq!(rig.previous(), Some(Prior::Snapshot("v2".into())));
    rig.rules.turn_to("v4");
    live.completed("u3", "three").await;
    assert_eq!(rig.previous(), Some(Prior::Snapshot("v2".into())));
    rewind(&mut live, 3, 3).await;
    live.completed("u4", "four").await;
    live.close().await;
    assert_eq!(rig.previous(), Some(Prior::Snapshot("v2".into())));
    assert_eq!(rig.last_sent("v4"), 1);
}

async fn switch_rules(live: &mut Live, n: u64, on: bool) {
    live.send(crate::plugin::testing::plugin_switch(
        n,
        "bb",
        wisp_protocol::settings::ENABLED_KEY,
        on,
    ))
    .await;
    let done = live
        .until(|e| match e {
            AgentEvent::SettingsChanged { answers, .. } => *answers == Some(request(n)),
            AgentEvent::Rejected { request: r, .. } => *r == Some(request(n)),
            _ => false,
        })
        .await;
    assert!(
        matches!(done, AgentEvent::SettingsChanged { .. }),
        "{done:?}"
    );
}

fn started_sections(store: &MemoryStore) -> Vec<SectionSnapshot> {
    log_events(store)
        .into_iter()
        .find_map(|e| match e {
            LogEvent::SessionStarted { sections, .. } => Some(sections),
            _ => None,
        })
        .unwrap()
}

async fn resumed_with_no_rebuild(kit: &Kit, store: &MemoryStore, server: &Server) -> Value {
    let built = resume_root(kit, store, server, Flags::Plain).await;
    assert!(built.agent.cache_rebuild.is_none());
    let sent = server.seen().bodies.len();
    one_turn(built.agent, "u9").await;
    body(server, sent)
}

#[tokio::test]
async fn a_system_section_off_at_start_stays_out_of_the_block_and_comes_once_in_a_turn_when_on() {
    let rules = Rules::new(Origin::Bundled);
    let env = Env::new("kit-sections-off-at-start");
    let server = Server::start(says_n(4)).await;
    let store = MemoryStore::default();
    let mut kit = rules.kit(&server, &env, &store);
    kit.start_layers.flags = vec![crate::plugin::testing::plugin_flag(
        "bb",
        wisp_protocol::settings::ENABLED_KEY,
        false,
    )];
    let built = kit.build(spec(kit::Start::Fresh)).await.unwrap();
    let block = built.agent.session.instructions.clone().unwrap_or_default();
    assert!(!block.contains("<rules>"), "{block}");
    let mut live = live(built.agent, &server);
    live.completed("u1", "one").await;
    switch_rules(&mut live, 1, true).await;
    live.completed("u2", "two").await;
    live.completed("u3", "three").await;
    live.close().await;
    assert!(started_sections(&store).is_empty());
    let bodies: Vec<Value> = (0..3).map(|n| body(&server, n)).collect();
    assert!(!system_of(&bodies[0]).contains("<rules>"));
    assert_eq!(rules_sent(&bodies[0]), 0);
    for later in &bodies[1..] {
        assert_eq!(system_of(later), system_of(&bodies[0]));
        assert_eq!(sent_of(later, "v1"), 1, "{later}");
    }
    let resumed = resumed_with_no_rebuild(&kit, &store, &server).await;
    assert_eq!(system_of(&resumed), system_of(&bodies[0]));
    assert_eq!(sent_of(&resumed, "v1"), 1);
}

#[tokio::test]
async fn a_system_section_switched_off_keeps_the_block_and_sends_no_update() {
    let rules = Rules::new(Origin::Bundled);
    let env = Env::new("kit-sections-switched-off");
    let server = Server::start(says_n(3)).await;
    let store = MemoryStore::default();
    let kit = rules.kit(&server, &env, &store);
    let built = kit.build(spec(kit::Start::Fresh)).await.unwrap();
    let mut live = live(built.agent, &server);
    live.completed("u1", "one").await;
    switch_rules(&mut live, 1, false).await;
    rules.turn_to("v2");
    live.completed("u2", "two").await;
    live.close().await;
    assert_eq!(started_sections(&store), logged_rules("v1"));
    let (first, second) = (body(&server, 0), body(&server, 1));
    assert!(system_of(&first).contains("<rules>\\nv1\\n</rules>"));
    assert_eq!(system_of(&second), system_of(&first));
    assert_eq!(rules_sent(&second), 0, "{second}");
    let resumed = resumed_with_no_rebuild(&kit, &store, &server).await;
    assert_eq!(system_of(&resumed), system_of(&first));
    assert_eq!(rules_sent(&resumed), 0, "{resumed}");
}

#[tokio::test]
async fn a_child_renders_its_roots_block_and_reads_its_own_source() {
    let rules = Rules::new(Origin::Bundled);
    let env = Env::new("kit-sections-child-block");
    fs::write(env.work().join("AGENTS.md"), "Answer briefly.\n").unwrap();
    let server = Server::start(Vec::new()).await;
    let store = MemoryStore::default();
    let kit = rules.kit(&server, &env, &store);
    let root = kit.build(spec(kit::Start::Fresh)).await.unwrap();
    let auto = child_at(env.work(), InstructionsSource::Auto, MessageTime::Off);
    let child = kit.build_child(auto).await.unwrap();
    let block = child.session.instructions.clone().unwrap();
    assert!(block.contains("Answer briefly."), "{block}");
    assert_eq!(Some(block), root.agent.session.instructions);
    let none = child_at(env.work(), InstructionsSource::None, MessageTime::Off);
    let child = kit.build_child(none).await.unwrap();
    assert_eq!(
        child.session.instructions.as_deref(),
        Some(rules_text("v1").as_str())
    );
    assert_eq!(
        child.owned_value("instructions.source").as_deref(),
        Some("none")
    );
}

#[tokio::test]
async fn a_child_with_only_the_default_file_renders_it() {
    let rules = Rules::new(Origin::Bundled);
    let env = Env::new("kit-sections-child-default");
    fs::create_dir_all(env.root.join("config")).unwrap();
    fs::write(
        env.root.join("config/AGENTS.md"),
        "Prefer British spelling.\n",
    )
    .unwrap();
    let server = Server::start(Vec::new()).await;
    let store = MemoryStore::default();
    let kit = rules.kit(&server, &env, &store);
    let auto = child_at(env.work(), InstructionsSource::Auto, MessageTime::Off);
    let child = kit.build_child(auto).await.unwrap();
    let block = child.session.instructions.clone().unwrap();
    assert!(
        block.contains(
            "<file scope=\"default\" name=\"AGENTS.md\">\nPrefer British spelling.\n</file>"
        ),
        "{block}"
    );
    assert_eq!(
        child.instructions_facts().unwrap().chosen,
        Some(wisp_protocol::Scope::Default)
    );
}
