use wisp_protocol::settings::{SettingChange, SettingValue, When};
use wisp_protocol::{CacheEffect, ToolChangeVia};

use super::super::keep_warm::an_hour_later;
use super::*;
use crate::agent::tests::kinds as logged_kinds;
use crate::provider::anthropic::{COMPACTION_BETA, FALLBACK_BETA, INLINE_TOOLS_BETA};
use crate::test_guard::until_event;

const CAPABLE: &str = "claude-opus-5-5";
const PLAIN: &str = "claude-opus-4-7";

fn switching(server: &Server, env: &Env, store: &MemoryStore) -> Kit {
    Kit {
        start_layers: StartLayers {
            full_screen: true,
            ..StartLayers::default()
        },
        tools: Arc::new(crate::plugin::Tools::new(
            registry(Tools::Default),
            registry(Tools::Default).default_on(),
        )),
        ..kit_for(server, env, store)
    }
}

fn on<'a>(model: &str, start: kit::Start<'a>) -> kit::Spec<'a> {
    let mut spec = spec(start);
    spec.choices.model = model.into();
    spec
}

fn without(tool: &str) -> Vec<String> {
    registry(Tools::Default)
        .default_on()
        .into_iter()
        .filter(|name| name != tool)
        .collect()
}

fn configured(server: &Server, env: &Env, store: &MemoryStore, names: Vec<String>) -> Kit {
    let mut kit = switching(server, env, store);
    kit.start_layers.flags = vec![crate::settings::Given {
        target: wisp_protocol::settings::Target::Name(
            wisp_protocol::settings::find("tools").unwrap(),
        ),
        value: SettingValue::List(names.clone()),
        origin: "--tools".into(),
    }];
    kit.tools = Arc::new(crate::plugin::Tools::new(
        registry(Tools::Default),
        registry(Tools::Default).on_tools_with(
            &names.into_iter().collect(),
            &std::collections::BTreeSet::new(),
        ),
    ));
    kit
}

fn unmarked(messages: &Value) -> Vec<Value> {
    let mut messages = messages.as_array().unwrap().clone();
    for message in &mut messages {
        if let Some(content) = message["content"].as_array_mut() {
            for block in content {
                if let Some(block) = block.as_object_mut() {
                    block.remove("cache_control");
                }
            }
        }
    }
    messages
}

fn kept_prefix(server: &Server, logged: usize, resumed: usize) {
    let (before, after) = (body(server, logged), body(server, resumed));
    assert_eq!(
        before["system"], after["system"],
        "the logged system is sent"
    );
    assert_eq!(before["tools"], after["tools"], "the logged tools are sent");
    let (old, new) = (unmarked(&before["messages"]), unmarked(&after["messages"]));
    assert!(new.len() > old.len(), "{new:#?}");
    assert_eq!(new[..old.len()], old[..], "the logged messages lead");
}

fn appends(server: &Server, n: usize) {
    let (before, after) = (body(server, n), body(server, n + 1));
    for key in ["system", "tools", "cache_control", "output_config", "model"] {
        assert_eq!(before[key], after[key], "request {n}: {key}");
    }
    let sent = before["messages"].as_array().unwrap();
    assert_eq!(
        sent[..],
        after["messages"].as_array().unwrap()[..sent.len()],
        "request {} extends request {n}",
        n + 1
    );
    assert_eq!(betas(server, n), betas(server, n + 1));
}

fn tool_message(request: &Value) -> Option<Value> {
    let (_, last) = last_two(request);
    (last["role"] == "system").then_some(last)
}

fn result_text(request: &Value, id: &str) -> String {
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| m["content"].as_array().cloned().unwrap_or_default())
        .find(|b| b["type"] == "tool_result" && b["tool_use_id"] == id)
        .map(|b| b["content"].to_string())
        .unwrap_or_else(|| panic!("no result for {id}"))
}

fn every_default() -> Vec<String> {
    registry(Tools::Default).default_on().into_iter().collect()
}

struct Session {
    commands: Option<mpsc::Sender<ClientCommand>>,
    events: mpsc::Receiver<AgentEvent>,
    task: JoinHandle<Agent>,
}

impl Session {
    fn start(agent: Agent) -> Self {
        let (commands, mut inbox) = mpsc::channel(64);
        let (sink, events) = mpsc::channel(8_192);
        let task = tokio::spawn(async move {
            let mut agent = agent;
            agent.serve(&mut inbox, &sink).await;
            agent
        });
        Self {
            commands: Some(commands),
            events,
            task,
        }
    }

    async fn send(&self, command: ClientCommand) {
        self.commands.as_ref().unwrap().send(command).await.unwrap();
    }

    async fn until(&mut self, stop: impl Fn(&AgentEvent) -> bool) -> Vec<AgentEvent> {
        let mut seen = Vec::new();
        until_event(&mut self.events, &mut seen, "the event", stop).await;
        seen
    }

    async fn switched(&mut self, request: u64, id: &str, key: &str, on: bool) -> Vec<AgentEvent> {
        self.send(crate::plugin::testing::plugin_switch(request, id, key, on))
            .await;
        self.until(|e| {
            matches!(e, AgentEvent::SettingsChanged { answers, .. } if *answers == Some(RequestId(request)))
        })
        .await
    }

    async fn refreshed(&mut self) {
        an_hour_later().await;
        self.until(|e| matches!(e, AgentEvent::CacheRefreshed { .. }))
            .await;
    }

    async fn turn(&mut self, id: &str, text: &str) -> Vec<AgentEvent> {
        self.send(ClientCommand::send(id, text)).await;
        let events = self
            .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
            .await;
        if let Some(AgentEvent::TurnEnded { outcome, .. }) = events.last() {
            assert!(completed(outcome), "{outcome:?}");
        }
        events
    }

    async fn tools(&mut self, request: u64, names: Vec<String>) -> Vec<AgentEvent> {
        self.send(ClientCommand::ChangeSettings {
            request: RequestId(request),
            changes: vec![SettingChange::named("tools", SettingValue::List(names)).unwrap()],
            save: false,
        })
        .await;
        self.until(|e| match e {
            AgentEvent::SettingsChanged { answers, .. } => *answers == Some(RequestId(request)),
            AgentEvent::Rejected { request: r, .. } => *r == Some(RequestId(request)),
            _ => false,
        })
        .await
    }

    async fn end(mut self) -> Agent {
        self.commands = None;
        while self.events.recv().await.is_some() {}
        within("the agent's end", self.task).await.unwrap()
    }
}

fn answered(events: &[AgentEvent]) -> (When, Option<CacheEffect>) {
    events
        .iter()
        .find_map(|e| match e {
            AgentEvent::SettingsChanged { changes, .. } if !changes.is_empty() => {
                Some((changes[0].when.clone(), changes[0].cache.clone()))
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("no answer: {events:#?}"))
}

type Changed = (Vec<String>, Vec<String>, ToolChangeVia, Option<CacheEffect>);

fn changed(events: &[AgentEvent]) -> Vec<Changed> {
    pick!(
        events,
        AgentEvent::ToolsChanged {
            added,
            removed,
            via,
            cache,
        } => (added.clone(), removed.clone(), *via, cache.clone())
    )
}

fn betas(server: &Server, n: usize) -> Vec<String> {
    server.seen().heads[n]
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("anthropic-beta:")
                .map(|v| v.trim().split(',').map(str::to_owned).collect())
        })
        .unwrap_or_default()
}

fn added_by_name(agent: &Agent, tool: &str) -> bool {
    agent.history().iter().any(|m| {
        m.role == Role::System
            && m.content.iter().any(|b| {
                matches!(
                    b,
                    ContentBlock::ToolAddition {
                        tool: wisp_protocol::ToolRef::ToolReference { name }
                    } if name == tool
                )
            })
    })
}

fn last_two(request: &Value) -> (Value, Value) {
    let messages = request["messages"].as_array().unwrap();
    (
        messages[messages.len() - 2].clone(),
        messages[messages.len() - 1].clone(),
    )
}

fn texts(message: &Value) -> Vec<String> {
    message["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|b| b["text"].as_str().map(str::to_owned))
        .collect()
}

fn kinds(message: &Value) -> Vec<String> {
    message["content"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| {
            let kind = b["type"].as_str().unwrap();
            match b["tool"]["type"].as_str() {
                Some("tool_reference") => format!("{kind} {}", b["tool"]["name"].as_str().unwrap()),
                Some(_) => format!(
                    "{kind} {} by value",
                    b["tool"]["definition"]["name"].as_str().unwrap()
                ),
                None => kind.to_owned(),
            }
        })
        .collect()
}

fn deferred_in(request: &Value) -> Vec<String> {
    request["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter(|t| t["defer_loading"] == json!(true))
                .map(|t| t["name"].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

async fn rebuilds(store: &MemoryStore, agent: &Agent) {
    let records = records(store);
    let rebuilt = rebuild_checked(store, &records, None)
        .unwrap()
        .unwrap()
        .messages;
    assert_eq!(
        serde_json::to_string(&rebuilt).unwrap(),
        serde_json::to_string(&*agent.history()).unwrap(),
        "the history rebuilt from the log differs from the live one"
    );
}

fn three() -> Vec<Reply> {
    vec![
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
        says("msg_03", 640, "Three."),
    ]
}

#[tokio::test]
async fn a_tool_turned_off_on_a_capable_model_goes_inline_by_name_and_keeps_the_array() {
    let env = Env::new("switch-inline-off");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = switching(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    let answer = session.tools(1, without("show_diff")).await;
    assert_eq!(answered(&answer), (When::NextTurn, Some(CacheEffect::Kept)));
    assert!(changed(&answer).is_empty(), "nothing applies at the change");
    let second = session.turn("u2", "two").await;
    assert_eq!(
        changed(&second),
        [(
            Vec::new(),
            vec!["show_diff".to_owned()],
            ToolChangeVia::Inline,
            Some(CacheEffect::Kept)
        )]
    );
    let third = session.turn("u3", "three").await;
    assert!(
        changed(&third).is_empty(),
        "an unchanged opening changes nothing"
    );
    let agent = session.end().await;

    let (first, second, third) = (body(&server, 0), body(&server, 1), body(&server, 2));
    assert_eq!(first["tools"], second["tools"]);
    assert_eq!(second["tools"], third["tools"]);
    assert!(
        !deferred_in(&third).contains(&"show_diff".to_owned()),
        "a tool turned off mid-session stays declared as it was"
    );
    assert_eq!(first["system"], second["system"]);
    let (user, system) = last_two(&second);
    assert_eq!(user["role"], "user");
    assert_eq!(system["role"], "system");
    assert_eq!(kinds(&system), ["tool_removal show_diff"]);
    assert!(!texts(&user).iter().any(|t| t.contains("Tools turned")));
    let messages = third["messages"].as_array().unwrap();
    let systems: Vec<&Value> = messages.iter().filter(|m| m["role"] == "system").collect();
    assert_eq!(systems.len(), 1, "the change is sent once and kept");
    assert_eq!(
        messages[..messages.len() - 2],
        second["messages"].as_array().unwrap()[..]
    );
    for n in 0..3 {
        assert!(
            betas(&server, n).contains(&INLINE_TOOLS_BETA.to_owned()),
            "an established pair sends the beta from the first request"
        );
    }
    let logged = logged_kinds(&store);
    assert_eq!(logged.iter().filter(|k| *k == "tools_changed").count(), 1);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_tool_turned_off_and_on_where_the_model_cant_take_blocks_is_noted() {
    let env = Env::new("switch-noted");
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        calls(
            "msg_02",
            520,
            vec![call("toolu_01", "show_diff", json!({"files": []}))],
        ),
        says("msg_03", 640, "Two."),
        says("msg_04", 760, "Three."),
        says("msg_05", 880, "Four."),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = switching(&server, &env, &store);
    let built = kit.build(on(PLAIN, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, without("show_diff")).await;
    let second = session.turn("u2", "two").await;
    assert_eq!(
        changed(&second),
        [(
            Vec::new(),
            vec!["show_diff".to_owned()],
            ToolChangeVia::TurnedOff,
            Some(CacheEffect::Kept)
        )]
    );
    session.turn("u3", "three").await;
    session.tools(2, every_default()).await;
    let fourth = session.turn("u4", "four").await;
    assert_eq!(
        changed(&fourth),
        [(
            vec!["show_diff".to_owned()],
            Vec::new(),
            ToolChangeVia::TurnedOff,
            Some(CacheEffect::Kept)
        )]
    );
    let agent = session.end().await;

    let bodies: Vec<Value> = (0..5).map(|n| body(&server, n)).collect();
    for later in &bodies[1..] {
        assert_eq!(later["tools"], bodies[0]["tools"]);
        assert_eq!(later["system"], bodies[0]["system"]);
        assert!(
            later["messages"]
                .as_array()
                .unwrap()
                .iter()
                .all(|m| m["role"] != "system")
        );
    }
    let user = |n: usize| {
        let messages = bodies[n]["messages"].as_array().unwrap();
        texts(&messages[messages.len() - 1])
    };
    assert_eq!(user(1)[0], "Tools turned off: show_diff.\n");
    let refused = &bodies[2]["messages"].as_array().unwrap().last().unwrap()["content"][0];
    assert_eq!(refused["type"], "tool_result");
    assert_eq!(refused["is_error"], true);
    assert!(
        refused.to_string().contains("this tool is turned off"),
        "{refused}"
    );
    assert!(!user(3).iter().any(|t| t.contains("Tools turned")));
    assert_eq!(user(4)[0], "Tools turned on: show_diff.\n");
    for n in 0..5 {
        assert!(!betas(&server, n).contains(&INLINE_TOOLS_BETA.to_owned()));
    }
    rebuilds(&store, &agent).await;
}

fn without_show_diff(server: &Server, env: &Env, store: &MemoryStore) -> Kit {
    let mut kit = switching(server, env, store);
    kit.start_layers.flags = vec![crate::settings::Given {
        target: wisp_protocol::settings::Target::Name(
            wisp_protocol::settings::find("tools").unwrap(),
        ),
        value: SettingValue::List(without("show_diff")),
        origin: "--tools".into(),
    }];
    kit.tools = Arc::new(crate::plugin::Tools::new(
        registry(Tools::Default),
        registry(Tools::Default).on_tools_with(
            &without("show_diff").into_iter().collect(),
            &std::collections::BTreeSet::new(),
        ),
    ));
    kit
}

#[tokio::test]
async fn a_tool_off_at_start_is_declared_deferred_and_turned_on_by_name_with_its_sentence() {
    let env = Env::new("switch-by-value");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = without_show_diff(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    let mut appended = without("show_diff");
    appended.push("show_diff".to_owned());
    assert_ne!(appended, every_default(), "the client's order, not sorted");
    let answer = session.tools(1, appended).await;
    assert_eq!(answered(&answer), (When::NextTurn, Some(CacheEffect::Kept)));
    let second = session.turn("u2", "two").await;
    assert_eq!(
        changed(&second),
        [(
            vec!["show_diff".to_owned()],
            Vec::new(),
            ToolChangeVia::Inline,
            Some(CacheEffect::Kept)
        )]
    );
    session.turn("u3", "three").await;
    let agent = session.end().await;
    let (first, second) = (body(&server, 0), body(&server, 1));
    assert_eq!(first["tools"], second["tools"]);
    assert!(deferred_in(&first).contains(&"show_diff".to_owned()));
    assert!(!deferred_in(&first).contains(&"manage".to_owned()));
    let (_, system) = last_two(&second);
    assert_eq!(kinds(&system), ["tool_addition show_diff", "text"]);
    assert!(texts(&system)[0].contains("show_diff"));
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_main_presets_tools_off_stay_off_when_the_tools_change_names_them() {
    let env = Env::new("switch-preset-off");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = configured(&server, &env, &store, without("view_file"));
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent.with_preset_off(["view_file".into()].into()));
    session.turn("u1", "one").await;
    session.tools(1, without("show_diff")).await;
    let second = session.turn("u2", "two").await;
    assert_eq!(
        changed(&second),
        [(
            Vec::new(),
            vec!["show_diff".to_owned()],
            ToolChangeVia::Inline,
            Some(CacheEffect::Kept)
        )]
    );
    session.end().await;
}

async fn resumed(kit: &Kit, store: &MemoryStore, server: &Server, model: &str) -> Agent {
    let (log, recovered, logged, rebuilt) = reopen(store, server);
    kit.build(on(
        model,
        kit::Start::Resume(kit::Resume {
            log,
            recovered: &recovered,
            truncated_bytes: 0,
            logged: logged.as_ref(),
            rebuilt,
        }),
    ))
    .await
    .unwrap()
    .agent
}

#[tokio::test]
async fn a_resume_after_an_inline_change_sends_the_same_prefix_and_changes_nothing() {
    let env = Env::new("switch-resume-inline");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = switching(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, without("show_diff")).await;
    session.turn("u2", "two").await;
    session.end().await;

    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(
        !agent
            .resume_notice_text()
            .unwrap_or_default()
            .contains("request headers"),
        "{:?}",
        agent.resume_notice_text()
    );
    let mut session = Session::start(agent);
    let third = session.turn("u3", "three").await;
    assert!(changed(&third).is_empty(), "{:?}", changed(&third));
    let agent = session.end().await;
    let (second, third) = (body(&server, 1), body(&server, 2));
    assert_eq!(second["tools"], third["tools"]);
    let messages = third["messages"].as_array().unwrap();
    assert_eq!(
        messages[..messages.len() - 2],
        second["messages"].as_array().unwrap()[..]
    );
    assert_eq!(messages.iter().filter(|m| m["role"] == "system").count(), 1);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_resume_after_a_noted_change_keeps_the_note_and_repeats_nothing() {
    let env = Env::new("switch-resume-noted");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = switching(&server, &env, &store);
    let built = kit.build(on(PLAIN, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, without("show_diff")).await;
    session.turn("u2", "two").await;
    session.end().await;

    let agent = resumed(&kit, &store, &server, PLAIN).await;
    let mut session = Session::start(agent);
    session.turn("u3", "three").await;
    let agent = session.end().await;
    let third = body(&server, 2);
    let messages = third["messages"].as_array().unwrap();
    let noted = messages
        .iter()
        .filter(|m| texts(m).iter().any(|t| t.starts_with("Tools turned")))
        .count();
    assert_eq!(noted, 1, "the note is sent once, before the resume");
    assert_eq!(
        messages[..messages.len() - 2],
        body(&server, 1)["messages"].as_array().unwrap()[..]
    );
    rebuilds(&store, &agent).await;
}

fn with_steps(store: &MemoryStore, edit: impl Fn(&mut serde_json::Value)) -> MemoryStore {
    let old = MemoryStore::default();
    let mut log = old.clone();
    for record in records(store) {
        let mut value = serde_json::to_value(&record).unwrap();
        if value["type"] == "step_ended" {
            edit(&mut value);
        }
        let record: LogRecord = serde_json::from_value(value).unwrap();
        log.append("s1", &record).unwrap();
    }
    old
}

fn before_36(store: &MemoryStore) -> MemoryStore {
    with_steps(store, |value| {
        value["schema_version"] = json!(35);
        if let Some(fields) = value.as_object_mut() {
            fields.remove("betas");
        }
    })
}

fn books_headers(agent: &crate::agent::Agent) -> bool {
    agent
        .resume_notice_text()
        .unwrap_or_default()
        .contains("request headers changed since this session")
}

#[tokio::test]
async fn the_inline_beta_alone_books_nothing_on_resume_and_another_beta_books_once() {
    let env = Env::new("switch-beta-resume");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = switching(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.end().await;
    let logged = records(&store);
    let step = logged
        .iter()
        .find(|r| matches!(r.event, LogEvent::StepEnded { .. }))
        .unwrap();
    assert!(
        serde_json::to_string(step)
            .unwrap()
            .contains(INLINE_TOOLS_BETA)
    );

    let same = resumed(&kit, &store, &server, CAPABLE).await;
    assert!(!books_headers(&same), "a 36 log on the same pair");
    assert!(same.cache_rebuild().is_none());

    let older = resumed(&kit, &before_36(&store), &server, CAPABLE).await;
    assert!(!books_headers(&older), "{:?}", older.resume_notice_text());
    assert!(older.cache_rebuild().is_none());

    let without = with_steps(&store, |value| {
        if let Some(betas) = value["betas"].as_array_mut() {
            betas.retain(|b| b != INLINE_TOOLS_BETA);
        }
    });
    let lacking = resumed(&kit, &without, &server, CAPABLE).await;
    assert!(
        !books_headers(&lacking),
        "{:?}",
        lacking.resume_notice_text()
    );
    assert!(lacking.cache_rebuild().is_none());

    let other = with_steps(&store, |value| {
        value["betas"] = json!([INLINE_TOOLS_BETA, "retired-beta-2026-01-01"]);
    });
    let changed = resumed(&kit, &other, &server, CAPABLE).await;
    assert!(
        books_headers(&changed),
        "{:?}",
        changed.resume_notice_text()
    );
    assert!(changed.cache_rebuild().is_some());
}

#[tokio::test]
async fn the_compaction_and_fallback_betas_book_nothing_on_resume() {
    let env = Env::new("switch-beta-compaction");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = switching(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.end().await;
    let compacting = with_steps(&store, |value| {
        if let Some(betas) = value["betas"].as_array_mut() {
            assert!(betas.contains(&json!(FALLBACK_BETA)));
            betas.retain(|b| b != FALLBACK_BETA);
            betas.push(json!(COMPACTION_BETA));
        }
    });
    let resumed = resumed(&kit, &compacting, &server, CAPABLE).await;
    assert!(
        !books_headers(&resumed),
        "{:?}",
        resumed.resume_notice_text()
    );
    assert!(resumed.cache_rebuild().is_none());
}

#[tokio::test]
async fn a_session_logged_before_36_resumed_without_the_capability_books_nothing() {
    let env = Env::new("switch-beta-plain");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = switching(&server, &env, &store);
    let built = kit.build(on(PLAIN, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.end().await;
    let plain = resumed(&kit, &before_36(&store), &server, PLAIN).await;
    assert!(
        !plain
            .resume_notice_text()
            .unwrap_or_default()
            .contains("request headers"),
        "{:?}",
        plain.resume_notice_text()
    );
    assert!(plain.cache_rebuild().is_none());
}

#[tokio::test]
async fn a_log_with_no_committed_step_compares_no_betas() {
    let env = Env::new("switch-beta-cold");
    let server = Server::start(Vec::new()).await;
    let store = MemoryStore::default();
    let kit = switching(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    Session::start(built.agent).end().await;
    assert!(!records(&store).is_empty());
    let agent = resumed(&kit, &before_36(&store), &server, CAPABLE).await;
    assert!(
        !agent
            .resume_notice_text()
            .unwrap_or_default()
            .contains("request headers")
    );
}

fn waits(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .rev()
        .find_map(|e| match e {
            AgentEvent::CatalogChanged { catalog } => Some(
                catalog
                    .plugins
                    .iter()
                    .flat_map(|p| p.tools.iter())
                    .filter(|t| t.waits)
                    .map(|t| t.name.clone())
                    .collect(),
            ),
            _ => None,
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn a_mixed_tools_change_with_web_search_off_rebuilds_once_naming_both() {
    for by_call in [false, true] {
        let env = Env::new("switch-mixed");
        let mut script = vec![says("msg_01", 400, "Off.")];
        if by_call {
            script.push(calls(
                "msg_02",
                520,
                vec![call("toolu_01", "web_search", json!({"query": "wisp"}))],
            ));
        }
        script.push(reply(
            "msg_search",
            100,
            vec![
                Part::Search("srvtoolu_01", json!({"query": "wisp"}), json!([])),
                Part::Text("No results."),
            ],
            "end_turn",
        ));
        if by_call {
            script.push(says("msg_03", 640, "On."));
        }
        script.push(says("msg_04", 760, "Off again."));
        let server = Server::start(script).await;
        let store = MemoryStore::default();
        let kit = configured(&server, &env, &store, without(crate::search::WEB_SEARCH));
        let mut agent = kit
            .build(on(CAPABLE, kit::Start::Fresh))
            .await
            .unwrap()
            .agent;
        assert!(agent.toolbox.runner(crate::search::WEB_SEARCH).is_some());
        if !by_call {
            agent.toolbox.drop_plugin("web-search");
        }
        assert!(!agent.toolbox.web_search());
        let mut session = Session::start(agent);
        session.turn("u1", "one").await;
        let expected_cache = if by_call {
            CacheEffect::Kept
        } else {
            CacheEffect::Rebuilt {
                reason: wisp_protocol::RebuildReason::ToolsChange,
            }
        };
        let enabled = session.tools(1, every_default()).await;
        assert_eq!(
            answered(&enabled),
            (When::NextTurn, Some(expected_cache.clone()))
        );
        let second = session.turn("u2", "search").await;
        assert!(second.iter().any(|event| matches!(
            event,
            AgentEvent::SearchEnded {
                outcome: wisp_protocol::SearchEnd::Done(_),
                ..
            }
        )));
        assert_eq!(
            second
                .iter()
                .any(|event| matches!(event, AgentEvent::ModelAsked { .. })),
            by_call
        );
        let only_search = session.tools(2, without(crate::search::WEB_SEARCH)).await;
        assert_eq!(
            answered(&only_search),
            (When::NextTurn, Some(expected_cache.clone()))
        );
        assert!(waits(&only_search).is_empty());
        let mixed: Vec<String> = without(crate::search::WEB_SEARCH)
            .into_iter()
            .filter(|n| n != "show_diff")
            .collect();
        let answer = session.tools(3, mixed).await;
        assert_eq!(
            answered(&answer),
            (When::NextTurn, Some(expected_cache.clone()))
        );
        let third = session.turn("u3", "three").await;
        assert_eq!(
            changed(&third)
                .into_iter()
                .map(|(_, removed, _, _)| removed)
                .collect::<Vec<_>>(),
            [vec!["show_diff".to_owned(), "web_search".to_owned()]]
        );
        for events in [&second, &third] {
            assert!(waits(events).is_empty());
            assert_eq!(changed(events).len(), 1);
            assert_eq!(changed(events)[0].3, Some(expected_cache.clone()));
        }
        session.end().await;
        let off_at = if by_call { 4 } else { 2 };
        let has = |at| {
            body(&server, at)["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["name"] == "web_search")
        };
        if by_call {
            assert_eq!((has(0), has(1), has(off_at)), (false, false, false));
            kept_prefix(&server, 0, 1);
            kept_prefix(&server, 1, off_at);
            assert!(
                tool_message(&body(&server, 1))
                    .unwrap()
                    .to_string()
                    .contains("web_search")
            );
            assert!(
                tool_message(&body(&server, off_at))
                    .unwrap()
                    .to_string()
                    .contains("web_search")
            );
        } else {
            assert_eq!((has(0), has(1), has(off_at)), (false, true, false));
            let tool = body(&server, 1)["tools"]
                .as_array()
                .unwrap()
                .iter()
                .find(|tool| tool["name"] == "web_search")
                .unwrap()
                .clone();
            assert!(tool.get("type").is_some());
        }
    }
}

fn on_a_gateway(server: &Server, env: &Env, store: &MemoryStore) -> Kit {
    let provider: Arc<dyn Provider> = Arc::new(
        AnthropicProvider::new(&Credential::ApiKey("sk-golden".into()), &server.base).unwrap(),
    );
    on_provider(switching(server, env, store), provider)
}

fn on_provider(kit: Kit, provider: Arc<dyn Provider>) -> Kit {
    let ready = vec![Connection {
        name: "env".into(),
        provider: provider.clone(),
        context: log_context(),
        priced: true,
    }];
    Kit {
        connect: Arc::new(move |_: &str| {
            Ok(kit::Connected {
                provider: provider.clone(),
                ready: ready.clone(),
                unavailable: unavailable(),
                active: "env".into(),
                env_account: None,
            })
        }),
        ..kit
    }
}

fn refused_removal() -> Reply {
    Reply {
        response: http::json(
            "400 Bad Request",
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"messages.3.content.0.tool_removal: Extra inputs are not permitted"}}"#,
        ),
        before: Vec::new(),
        gate: None,
    }
}

fn marked(store: &MemoryStore) -> Vec<String> {
    pick!(
        &log_events(store),
        LogEvent::RebuildMarked { reason, .. } => serde_json::to_value(reason).unwrap().to_string()
    )
}

#[tokio::test]
async fn a_trial_that_passes_marks_nothing_and_keeps_the_beta() {
    let env = Env::new("switch-trial-passed");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = on_a_gateway(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    let answer = session.tools(1, without("show_diff")).await;
    assert_eq!(
        answered(&answer),
        (When::NextTurn, Some(CacheEffect::KeptIfPassedOn))
    );
    let second = session.turn("u2", "two").await;
    assert_eq!(changed(&second)[0].3, Some(CacheEffect::KeptIfPassedOn));
    session.turn("u3", "three").await;
    session.end().await;
    assert!(!betas(&server, 0).contains(&INLINE_TOOLS_BETA.to_owned()));
    assert!(betas(&server, 1).contains(&INLINE_TOOLS_BETA.to_owned()));
    assert!(betas(&server, 2).contains(&INLINE_TOOLS_BETA.to_owned()));
    assert!(marked(&store).is_empty(), "{:?}", marked(&store));
    assert!(logged_kinds(&store).contains(&"inline_tools".to_owned()));
}

#[tokio::test]
async fn a_refused_trial_resends_without_blocks_marks_nothing_and_notes_at_the_next_opening() {
    let env = Env::new("switch-trial-refused");
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        refused_removal(),
        says("msg_02", 520, "Two."),
        says("msg_03", 640, "Three."),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = on_a_gateway(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, without("show_diff")).await;
    session.turn("u2", "two").await;
    session.turn("u3", "three").await;
    let agent = session.end().await;
    let refused = body(&server, 1);
    assert_eq!(kinds(&last_two(&refused).1), ["tool_removal show_diff"]);
    let resent = body(&server, 2);
    assert!(
        resent["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["role"] != "system"),
        "the resend carries no blocks"
    );
    assert!(!betas(&server, 2).contains(&INLINE_TOOLS_BETA.to_owned()));
    let third = body(&server, 3);
    let messages = third["messages"].as_array().unwrap();
    assert!(messages.iter().all(|m| m["role"] != "system"));
    assert_eq!(
        texts(messages.last().unwrap())[0],
        "Tools turned off: show_diff.\n"
    );
    assert!(marked(&store).is_empty(), "{:?}", marked(&store));
    rebuilds(&store, &agent).await;
}

fn watching() -> (
    Arc<crate::plugin::Registry>,
    wisp_plugin_testkit::hooks::Calls,
) {
    use wisp_plugin_testkit::hooks as testkit;
    let calls = testkit::Calls::default();
    let mut sources: Vec<(String, crate::plugin::Make)> = wisp_plugins::bundled()
        .into_iter()
        .map(|b| {
            let make: crate::plugin::Make = if b.manifest == wisp_plugin_shell::MANIFEST {
                crate::tool::testkit::shell_make(wisp_plugin_shell::Timing::default())
            } else {
                Arc::new(move |host| (b.instantiate)(host))
            };
            (b.manifest.to_owned(), make)
        })
        .collect();
    sources.push((
        testkit::manifest(
            "watcher",
            "\n[[hooks]]\nname = \"seen\"\npoint = \"tools-changed\"\n",
        ),
        testkit::instantiate(testkit::none(), calls.clone()),
    ));
    let (watched, problems) =
        crate::plugin::Registry::from_sources(sources, &crate::plugin::PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    (Arc::new(watched), calls)
}

#[tokio::test]
async fn the_tools_changed_point_fires_once_for_an_applied_change() {
    use wisp_plugin_api::hooks::{Event, ToolsChange};
    let (watched, calls) = watching();
    let env = Env::new("switch-hook");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let mut kit = switching(&server, &env, &store);
    kit.tools = Arc::new(crate::plugin::Tools::new(
        watched.clone(),
        watched.default_on(),
    ));
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, without("show_diff")).await;
    session.turn("u2", "two").await;
    session.turn("u3", "three").await;
    session.end().await;
    let seen: Vec<Event> = calls.of("seen").into_iter().map(|c| c.event).collect();
    assert_eq!(
        seen,
        [Event::ToolsChanged(ToolsChange {
            added: Vec::new(),
            removed: vec!["show_diff".into()],
            via: "inline".into(),
        })]
    );
}

fn counted_names(agent: &Agent) -> Vec<String> {
    agent.counted_tools().into_iter().map(|s| s.name).collect()
}

#[tokio::test]
async fn a_deferred_tool_surfaced_turned_off_and_on_again_is_counted_once() {
    let env = Env::new("switch-by-value-twice");
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
        says("msg_03", 640, "Three."),
        says("msg_04", 760, "Four."),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = without_show_diff(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let array: Vec<String> = built
        .agent
        .counted_tools()
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert!(!array.contains(&"show_diff".to_owned()));
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, every_default()).await;
    session.turn("u2", "two").await;
    session.tools(2, without("show_diff")).await;
    session.turn("u3", "three").await;
    session.tools(3, every_default()).await;
    session.turn("u4", "four").await;
    let agent = session.end().await;
    let system = |n: usize| kinds(&last_two(&body(&server, n)).1);
    assert_eq!(system(1), ["tool_addition show_diff", "text"]);
    assert_eq!(system(2), ["tool_removal show_diff"]);
    assert_eq!(
        system(3),
        ["tool_addition show_diff"],
        "a deferred tool comes back by name, and its sentence was stated"
    );
    for n in 0..4 {
        assert_eq!(body(&server, n)["tools"], body(&server, 0)["tools"]);
    }
    let counted = counted_names(&agent);
    assert_eq!(
        counted.iter().filter(|n| *n == "show_diff").count(),
        1,
        "{counted:?}"
    );
    assert_eq!(counted.len(), array.len() + 1);
    rebuilds(&store, &agent).await;
}

const SUMMARY: &str = "## Goal and standing instructions\nSay hello.\n\n## Decisions\nNone yet.\n\n## State of the work\nDone: the greeting.\n\n## Files, branches and artefacts\nNone.\n\n## Commands, errors and fixes\nNone.\n\n## Open questions\nNone.";

impl Session {
    async fn compact(&mut self, request: u64) {
        self.send(ClientCommand::Compact {
            focus: None,
            request: RequestId(request),
        })
        .await;
        let events = self
            .until(|e| match e {
                AgentEvent::Compacted { answers, .. }
                | AgentEvent::CompactionFailed { answers, .. } => {
                    *answers == Some(RequestId(request))
                }
                AgentEvent::Rejected { request: r, .. } => *r == Some(RequestId(request)),
                _ => false,
            })
            .await;
        assert!(
            matches!(events.last(), Some(AgentEvent::Compacted { .. })),
            "{:?}",
            events.last()
        );
    }
}

#[tokio::test]
async fn an_addition_dropped_by_a_wisp_compaction_is_restated_once_with_no_change() {
    let env = Env::new("switch-compacted");
    let server = Server::start(vec![
        says("msg_01", 40_000, "One."),
        says("msg_02", 40_100, "Two."),
        reply("msg_03", 40_200, vec![Part::Text(SUMMARY)], "end_turn"),
        says("msg_04", 640, "Three."),
        says("msg_05", 760, "Four."),
    ])
    .await;
    let store = MemoryStore::default();
    let mut kit = without_show_diff(&server, &env, &store);
    let registry = registry_without(&["time"]);
    kit.tools = Arc::new(crate::plugin::Tools::new(
        registry.clone(),
        registry.on_tools_with(
            &without("show_diff").into_iter().collect(),
            &std::collections::BTreeSet::new(),
        ),
    ));
    kit.start_layers.flags.push(removed_flag("time"));
    kit.models = models("[models.\"claude-opus-5-5\"]\nnative_compaction = false\n");
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, every_default()).await;
    session
        .switched(2, "time", wisp_protocol::settings::INSTALLED_KEY, true)
        .await;
    session.turn("u2", "two").await;
    session.compact(2).await;
    let third = session.turn("u3", "three").await;
    assert!(changed(&third).is_empty(), "a re-statement is no change");
    let fourth = session.turn("u4", "four").await;
    assert!(changed(&fourth).is_empty());
    let agent = session.end().await;
    let after = body(&server, 3);
    let (user, system) = last_two(&after);
    assert_eq!(kinds(&system), ["tool_addition show_diff", "text"]);
    assert!(texts(&system)[0].contains("'Sent:' line"));
    assert!(!texts(&user).iter().any(|t| t.contains("Tools turned")));
    let next = body(&server, 4);
    let messages = next["messages"].as_array().unwrap();
    assert_eq!(
        messages.iter().filter(|m| m["role"] == "system").count(),
        1,
        "restated once"
    );
    assert!(counted_names(&agent).contains(&"show_diff".to_owned()));
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_rewind_to_before_an_addition_restates_it_at_the_next_turn() {
    let env = Env::new("switch-rewound");
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
        says("msg_03", 520, "Two again."),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = without_show_diff(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, every_default()).await;
    session.turn("u2", "two").await;
    session
        .send(ClientCommand::Rewind {
            to: wisp_protocol::RewindPoint::Turn { turn: 2 },
            request: RequestId(2),
        })
        .await;
    session
        .until(|e| {
            matches!(e, AgentEvent::Rewound { .. })
                || matches!(e, AgentEvent::Rejected { request: Some(r), .. } if *r == RequestId(2))
        })
        .await;
    let again = session.turn("u3", "two, again").await;
    assert!(changed(&again).is_empty(), "{:?}", changed(&again));
    let agent = session.end().await;
    let (second, third) = (body(&server, 1), body(&server, 2));
    assert_eq!(kinds(&last_two(&second).1), kinds(&last_two(&third).1));
    assert_eq!(
        kinds(&last_two(&third).1),
        ["tool_addition show_diff", "text"]
    );
    assert_eq!(second["tools"], third["tools"]);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_head_built_at_an_opening_restates_the_tool_change_beside_a_late_marker() {
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        says("msg_02", 100_000, "Two."),
        reply_error(
            "400 Bad Request",
            "invalid_request_error",
            "prompt is too long",
        ),
        reply("msg_03", 100_000, vec![Part::Text(SUMMARY)], "end_turn"),
        says("msg_04", 300, "Three."),
        says("msg_05", 400, "Four."),
    ])
    .await;
    let env = Env::new("switch-late-marker");
    let store = MemoryStore::default();
    let mut kit = switching(&server, &env, &store);
    kit.models = models("[models.\"claude-opus-5-5\"]\nnative_compaction = false\n");
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session
        .send(ClientCommand::ChangeSettings {
            request: RequestId(1),
            changes: vec![
                SettingChange::named("effort", SettingValue::Text("low".into())).unwrap(),
                SettingChange::named("tools", SettingValue::List(without("show_diff"))).unwrap(),
            ],
            save: false,
        })
        .await;
    session
        .until(|e| {
            matches!(
                e,
                AgentEvent::SettingsChanged {
                    answers: Some(RequestId(1)),
                    ..
                }
            )
        })
        .await;
    session.turn("u2", "two").await;
    let (_, switched) = last_two(&body(&server, 1));
    assert_eq!(kinds(&switched), ["tool_removal show_diff"]);
    assert_eq!(switched["output_config"]["effort"], "low");
    session.turn("u3", "three").await;
    session.turn("u4", "four").await;
    let agent = session.end().await;
    let resent = body(&server, 4);
    let (_, restated) = last_two(&resent);
    assert_eq!(kinds(&restated), ["tool_removal show_diff"]);
    assert_eq!(restated["output_config"]["effort"], "low", "a late marker");
    let (_, later) = last_two(&body(&server, 5));
    assert_eq!(later["role"], "user");
    rebuilds(&store, &agent).await;
}

#[cfg(unix)]
#[tokio::test]
async fn an_instructions_update_and_a_tools_note_share_one_opening_as_separate_blocks() {
    let env = Env::new("switch-with-instructions");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = switching(&server, &env, &store);
    let built = kit.build(on(PLAIN, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    crate::instructions::make_repository(&env.work()).unwrap();
    std::fs::write(env.work().join("AGENTS.md"), "Answer briefly.\n").unwrap();
    session.tools(1, without("show_diff")).await;
    session.turn("u2", "two").await;
    session.turn("u3", "three").await;
    let agent = session.end().await;
    let (_, opened) = last_two(&body(&server, 1));
    let opened = texts(&opened);
    let update = opened
        .iter()
        .position(|t| t.starts_with("<instructions-update>"))
        .unwrap_or_else(|| panic!("{opened:#?}"));
    let note = opened
        .iter()
        .position(|t| t == "Tools turned off: show_diff.\n")
        .unwrap_or_else(|| panic!("{opened:#?}"));
    assert_eq!((update, note), (0, 1), "{opened:#?}");
    assert!(opened[2].starts_with("Sent: "), "{opened:#?}");
    let (_, third) = last_two(&body(&server, 2));
    assert!(
        texts(&third)
            .iter()
            .all(|t| !t.starts_with("<instructions-update>") && !t.starts_with("Tools turned")),
        "neither is stated again"
    );
    let events = log_events(&store);
    let lead = events
        .iter()
        .find_map(|e| match e {
            LogEvent::TurnStarted { turn: 2, lead, .. } => Some(lead.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        lead.text(wisp_protocol::log::LEAD_TOOLS),
        Some("Tools turned off: show_diff.\n")
    );
    assert!(lead.text("instructions/instructions").is_some());
    assert!(
        events
            .iter()
            .any(|e| matches!(*e, LogEvent::InstructionsChanged { turn: Some(2), .. }))
    );
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn each_request_after_a_tool_system_message_strictly_appends_to_the_one_before() {
    let env = Env::new("switch-strict-append");
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
        says("msg_03", 640, "Three."),
        says("msg_04", 760, "Four."),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = without_show_diff(&server, &env, &store);
    let built = kit
        .build(on("claude-haiku-5-5", kit::Start::Fresh))
        .await
        .unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    let mut names = without("show_diff");
    names.retain(|n| n != "view_file");
    session.tools(1, names.clone()).await;
    session.turn("u2", "two").await;
    names.push("show_diff".into());
    session.tools(2, names.clone()).await;
    session.turn("u3", "three").await;
    names.retain(|n| n != "show_diff");
    session.tools(3, names).await;
    session.turn("u4", "four").await;
    let agent = session.end().await;
    for n in 1..3 {
        let sent = body(&server, n)["messages"].as_array().unwrap().clone();
        assert_eq!(sent.last().unwrap()["role"], "system", "request {n}");
        appends(&server, n);
    }
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_native_compaction_over_a_by_value_addition_restates_its_sentence_and_turns_go_on() {
    let now = started(Tools::Default)
        .toolbox
        .definition("show_diff")
        .cloned()
        .unwrap();
    let addition = json!({
        "type": "tool_addition",
        "tool": {
            "type": "tool_definition",
            "definition": now,
        }
    });
    let env = Env::new("switch-native-compacted");
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
        reply(
            "msg_03",
            1_200,
            vec![Part::Opaque(json!({
                "type": "compaction",
                "content": SUMMARY,
                "tool_changes": [addition]
            }))],
            "compaction",
        ),
        says("msg_04", 300, "Three."),
        says("msg_05", 400, "Four."),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = without_show_diff(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, every_default()).await;
    session.turn("u2", "two").await;
    session.compact(2).await;
    let third = session.turn("u3", "three").await;
    let fourth = session.turn("u4", "four").await;
    let agent = session.end().await;
    for events in [&third, &fourth] {
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::TurnEnded {
                    outcome: TurnOutcome::Completed { .. },
                    ..
                }
            )),
            "{events:#?}"
        );
    }
    let after = body(&server, 3);
    let block = after["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| m["content"].as_array().unwrap())
        .find(|b| b["type"] == "compaction")
        .unwrap();
    assert_eq!(
        block["tool_changes"],
        json!([{"type": "tool_addition", "tool": {"type": "tool_reference", "name": "show_diff"}}]),
        "a deferred tool is never declared by value beside its deferred entry"
    );
    assert!(deferred_in(&after).contains(&"show_diff".to_owned()));
    let (_, restated) = last_two(&after);
    assert_eq!(restated["role"], "system");
    assert_eq!(kinds(&restated), ["text"]);
    assert!(texts(&restated)[0].contains("show_diff"));
    let (_, later) = last_two(&body(&server, 4));
    assert_eq!(later["role"], "user");
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_steer_waiting_at_a_native_compaction_between_steps_goes_before_the_restated_sentence() {
    let now = started(Tools::Default)
        .toolbox
        .definition("show_diff")
        .cloned()
        .unwrap();
    let addition = json!({
        "type": "tool_addition",
        "tool": {
            "type": "tool_definition",
            "definition": now,
        }
    });
    let env = Env::new("switch-native-compacted-steered");
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
        says("msg_03", 40_000, "Three.").gated(&gate),
        reply(
            "msg_04",
            1_200,
            vec![Part::Opaque(json!({
                "type": "compaction",
                "content": SUMMARY,
                "tool_changes": [addition]
            }))],
            "compaction",
        ),
        says("msg_05", 300, "Four."),
        says("msg_06", 400, "Five."),
    ])
    .await;
    let store = MemoryStore::default();
    let mut kit = without_show_diff(&server, &env, &store);
    kit.models = models("[models.\"claude-opus-5-5\"]\ncompact_at = 20000\n");
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, every_default()).await;
    session.turn("u2", "two").await;
    session.send(ClientCommand::send("u3", "three")).await;
    session
        .until(|e| matches!(e, AgentEvent::StepStarted { step: 1 }))
        .await;
    session.send(ClientCommand::send("s1", "And this.")).await;
    session
        .until(|e| matches!(e, AgentEvent::InputAccepted { .. }))
        .await;
    gate.add_permits(1);
    let third = session
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(
        matches!(third.last(), Some(AgentEvent::TurnEnded { outcome, .. }) if completed(outcome)),
        "{third:#?}"
    );
    assert!(
        third
            .iter()
            .any(|e| matches!(e, AgentEvent::Compacted { turn: Some(3), .. }))
    );
    session.turn("u4", "four").await;
    let agent = session.end().await;
    let (steer, restated) = last_two(&body(&server, 4));
    assert_eq!(steer["role"], "user");
    assert!(texts(&steer).iter().any(|t| t.contains("And this.")));
    assert_eq!(restated["role"], "system");
    assert_eq!(kinds(&restated), ["text"]);
    appends(&server, 4);
    rebuilds(&store, &agent).await;
}

async fn change(session: &mut Session, request: u64, changes: Vec<SettingChange>) {
    session
        .send(ClientCommand::ChangeSettings {
            request: RequestId(request),
            changes,
            save: false,
        })
        .await;
    session
        .until(|e| {
            matches!(
                e,
                AgentEvent::SettingsChanged { answers: Some(RequestId(r)), .. } if *r == request
            )
        })
        .await;
}

#[tokio::test]
async fn a_head_built_at_an_opening_restates_the_tool_change_beside_a_marker_sent_on_time() {
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        says("msg_02", 100_000, "Two."),
        reply_error(
            "400 Bad Request",
            "invalid_request_error",
            "prompt is too long",
        ),
        reply("msg_03", 100_000, vec![Part::Text(SUMMARY)], "end_turn"),
        says("msg_04", 300, "Three."),
        says("msg_05", 400, "Four."),
    ])
    .await;
    let env = Env::new("switch-marker-on-time");
    let store = MemoryStore::default();
    let mut kit = switching(&server, &env, &store);
    kit.models = models("[models.\"claude-opus-5-5\"]\nnative_compaction = false\n");
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    change(
        &mut session,
        1,
        vec![SettingChange::named("tools", SettingValue::List(without("show_diff"))).unwrap()],
    )
    .await;
    session.turn("u2", "two").await;
    change(
        &mut session,
        2,
        vec![SettingChange::named("effort", SettingValue::Text("low".into())).unwrap()],
    )
    .await;
    session.turn("u3", "three").await;
    session.turn("u4", "four").await;
    let agent = session.end().await;
    let (_, first) = last_two(&body(&server, 2));
    assert_eq!(first["output_config"]["effort"], "low");
    assert!(kinds(&first).is_empty(), "{first}");
    let (_, restated) = last_two(&body(&server, 4));
    assert_eq!(restated["output_config"]["effort"], "low");
    assert_eq!(kinds(&restated), ["tool_removal show_diff"]);
    let (_, later) = last_two(&body(&server, 5));
    assert_eq!(later["role"], "user");
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn step_ended_logs_the_betas_sorted_though_the_header_is_not() {
    let env = Env::new("switch-betas-sorted");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = switching(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.end().await;
    let header = betas(&server, 0);
    let logged: Vec<String> = log_events(&store)
        .iter()
        .find_map(|e| match e {
            LogEvent::StepEnded { betas, .. } => Some(betas.clone()),
            _ => None,
        })
        .unwrap();
    let mut sorted = header.clone();
    sorted.sort();
    assert_ne!(header, sorted, "the header's own order isn't sorted");
    assert_eq!(logged, sorted);
}

#[tokio::test]
async fn a_retry_that_opens_its_message_again_applies_the_switches_made_since() {
    let env = Env::new("switch-retry-reopens");
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        server_error(),
        says("msg_02", 520, "Two."),
        says("msg_03", 640, "Three."),
    ])
    .await;
    let store = MemoryStore::default();
    let mut kit = switching(&server, &env, &store);
    kit.retry_budget = Some(0.0);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, without("show_diff")).await;
    session.send(ClientCommand::send("u2", "two")).await;
    let failed = session
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(
        failed.iter().any(|e| matches!(
            e,
            AgentEvent::TurnEnded {
                outcome: TurnOutcome::Error { offer: Some(_), .. },
                ..
            }
        )),
        "{failed:#?}"
    );
    let mut fewer = without("show_diff");
    fewer.retain(|n| n != "view_file");
    session.tools(2, fewer).await;
    session
        .send(ClientCommand::Retry {
            turn: 2,
            endpoint: None,
            request: RequestId(3),
        })
        .await;
    let retried = session
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(
        retried.iter().any(|e| matches!(
            e,
            AgentEvent::TurnEnded {
                outcome: TurnOutcome::Completed { .. },
                ..
            }
        )),
        "{retried:#?}"
    );
    session.turn("u3", "three").await;
    let agent = session.end().await;
    let (_, failed_system) = last_two(&body(&server, 1));
    assert_eq!(kinds(&failed_system), ["tool_removal show_diff"]);
    let (_, retried_system) = last_two(&body(&server, 2));
    assert_eq!(
        kinds(&retried_system),
        ["tool_removal show_diff", "tool_removal view_file"]
    );
    let logged: Vec<usize> = pick!(
        &log_events(&store),
        LogEvent::TurnStarted {
            retry_of: Some(2),
            tool_message,
            ..
        } => tool_message.len()
    );
    assert_eq!(logged, [2], "the retry logs its own tool message");
    rebuilds(&store, &agent).await;
}

fn model(name: &str) -> Vec<SettingChange> {
    vec![SettingChange::named("model", SettingValue::Text(name.into())).unwrap()]
}

#[tokio::test]
async fn each_pair_reads_what_is_offered_in_its_own_currency_across_model_switches() {
    let env = Env::new("switch-pairs");
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
        says("msg_03", 640, "Three."),
        says("msg_04", 760, "Four."),
        says("msg_05", 880, "Five."),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = switching(&server, &env, &store);
    let built = kit.build(on(PLAIN, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, without("show_diff")).await;
    session.turn("u2", "two").await;
    change(&mut session, 2, model(CAPABLE)).await;
    session.turn("u3", "three").await;
    change(&mut session, 3, model(PLAIN)).await;
    session.turn("u4", "four").await;
    change(&mut session, 4, model(CAPABLE)).await;
    session.turn("u5", "five").await;
    let agent = session.end().await;
    let noted = |n: usize| {
        let (user, system) = last_two(&body(&server, n));
        let message = if system["role"] == "system" {
            user
        } else {
            system
        };
        texts(&message)
            .iter()
            .filter(|t| t.starts_with("Tools turned"))
            .count()
    };
    let system_kinds = |n: usize| {
        let (_, last) = last_two(&body(&server, n));
        if last["role"] == "system" {
            kinds(&last)
        } else {
            Vec::new()
        }
    };
    assert_eq!(noted(1), 1, "the plain pair notes the change");
    assert_eq!(system_kinds(1), Vec::<String>::new());
    assert_eq!(noted(2), 0);
    assert_eq!(
        system_kinds(2),
        ["tool_removal show_diff"],
        "a note doesn't count on a blocks pair"
    );
    assert_eq!(noted(3), 0, "the note is still in view");
    assert_eq!(system_kinds(3), Vec::<String>::new());
    assert_eq!(noted(4), 0);
    assert_eq!(
        system_kinds(4),
        Vec::<String>::new(),
        "the block is in view"
    );
    rebuilds(&store, &agent).await;
}

fn notes_in(body: &Value) -> usize {
    notes_naming(body, "show_diff")
}

fn notes_naming(body: &Value, tools: &str) -> usize {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "user")
        .flat_map(texts)
        .filter(|t| *t == format!("Tools turned off: {tools}.\n"))
        .count()
}

#[tokio::test]
async fn a_note_dropped_by_a_wisp_compaction_is_restated_once() {
    let env = Env::new("switch-note-compacted");
    let server = Server::start(vec![
        says("msg_01", 40_000, "One."),
        says("msg_02", 40_100, "Two."),
        reply("msg_03", 40_200, vec![Part::Text(SUMMARY)], "end_turn"),
        says("msg_04", 640, "Three."),
        says("msg_05", 760, "Four."),
    ])
    .await;
    let store = MemoryStore::default();
    let mut kit = switching(&server, &env, &store);
    kit.models = models("[models.\"claude-opus-4-7\"]\nnative_compaction = false\n");
    let built = kit.build(on(PLAIN, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, without("show_diff")).await;
    session.turn("u2", "two").await;
    session.compact(2).await;
    let third = session.turn("u3", "three").await;
    assert!(changed(&third).is_empty(), "a re-statement is no change");
    session.turn("u4", "four").await;
    let agent = session.end().await;
    assert_eq!(notes_in(&body(&server, 1)), 1);
    assert_eq!(notes_in(&body(&server, 3)), 1, "restated after the head");
    assert_eq!(notes_in(&body(&server, 4)), 1, "and only once");
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_note_dropped_by_a_rewind_is_restated_at_the_next_turn() {
    let env = Env::new("switch-note-rewound");
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
        says("msg_03", 520, "Two again."),
        says("msg_04", 640, "Three."),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = switching(&server, &env, &store);
    let built = kit.build(on(PLAIN, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, without("show_diff")).await;
    session.turn("u2", "two").await;
    session
        .send(ClientCommand::Rewind {
            to: wisp_protocol::RewindPoint::Turn { turn: 2 },
            request: RequestId(2),
        })
        .await;
    session
        .until(|e| {
            matches!(e, AgentEvent::Rewound { .. })
                || matches!(e, AgentEvent::Rejected { request: Some(r), .. } if *r == RequestId(2))
        })
        .await;
    let again = session.turn("u3", "two, again").await;
    assert!(changed(&again).is_empty(), "{:?}", changed(&again));
    session.turn("u4", "three").await;
    let agent = session.end().await;
    assert_eq!(notes_in(&body(&server, 1)), 1);
    assert_eq!(notes_in(&body(&server, 2)), 1, "restated after the rewind");
    assert_eq!(notes_in(&body(&server, 3)), 1, "and only once");
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn the_tools_note_is_written_with_no_plugin_hooks_at_the_turn() {
    let kept = ["id = \"show-diff\"", "id = \"view-file\"", "id = \"todos\""];
    let sources = wisp_plugins::bundled()
        .into_iter()
        .filter(|b| kept.iter().any(|k| b.manifest.starts_with(k)))
        .map(|b| {
            let make: crate::plugin::Make = Arc::new(move |host| (b.instantiate)(host));
            (b.manifest.to_owned(), make)
        })
        .collect();
    let (bare, problems) =
        crate::plugin::Registry::from_sources(sources, &crate::plugin::PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let bare = Arc::new(bare);
    let env = Env::new("switch-no-hooks");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let mut kit = switching(&server, &env, &store);
    kit.tools = Arc::new(crate::plugin::Tools::new(bare.clone(), bare.default_on()));
    let built = kit.build(on(PLAIN, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    let mut names: Vec<String> = bare.default_on().into_iter().collect();
    names.retain(|n| n != "show_diff");
    session.tools(1, names).await;
    session.turn("u2", "two").await;
    session.turn("u3", "three").await;
    let agent = session.end().await;
    let opened = texts(&last_two(&body(&server, 1)).1);
    assert_eq!(opened, ["Tools turned off: show_diff.\n", "two"]);
    assert_eq!(notes_in(&body(&server, 2)), 1);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn keep_warm_refreshes_a_request_that_ends_in_a_tool_message_byte_for_byte() {
    let env = Env::new("switch-keep-warm");
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
        super::super::keep_warm::refreshes_read("msg_r1", 520),
        says("msg_03", 640, "Three."),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = switching(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent.with_keep_warm(8));
    session.turn("u1", "one").await;
    session.tools(1, without("show_diff")).await;
    session.turn("u2", "two").await;
    session.refreshed().await;
    session.turn("u3", "three").await;
    let agent = session.end().await;
    let (turn, refresh) = (body(&server, 1), body(&server, 2));
    assert_eq!(kinds(&last_two(&turn).1), ["tool_removal show_diff"]);
    let strip = |mut value: Value| {
        if let Some(map) = value.as_object_mut() {
            map.remove("max_tokens");
            map.remove("fallbacks");
        }
        value
    };
    assert_eq!(strip(refresh.clone()), strip(turn));
    assert_eq!(refresh["max_tokens"], 1);
    let sent: Vec<String> = betas(&server, 1)
        .into_iter()
        .filter(|b| b != "server-side-fallback-2026-07-01")
        .collect();
    assert_eq!(betas(&server, 2), sent);
    assert!(sent.contains(&INLINE_TOOLS_BETA.to_owned()));
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn every_whole_copy_hashes_to_its_own_tools_digest_after_inline_changes() {
    let env = Env::new("switch-whole-copy");
    let mut replies = three();
    replies.push(says("msg_04", 760, "Four."));
    let server = Server::start(replies).await;
    let store = MemoryStore::default();
    let kit = without_show_diff(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    let mut added = without("show_diff");
    added.push("show_diff".to_owned());
    session.tools(1, added).await;
    session.turn("u2", "two").await;
    let off: Vec<String> = every_default()
        .into_iter()
        .filter(|name| name != "view_file")
        .collect();
    session.tools(2, off).await;
    let third = session.turn("u3", "three").await;
    assert_eq!(
        changed(&third),
        [(
            Vec::new(),
            vec!["view_file".to_owned()],
            ToolChangeVia::Inline,
            Some(CacheEffect::Kept)
        )]
    );
    session.end().await;
    let line_mode = Kit {
        start_layers: StartLayers::default(),
        ..switching(&server, &env, &store)
    };
    let agent = resumed(&line_mode, &store, &server, CAPABLE).await;
    let mut session = Session::start(agent);
    session.turn("u4", "four").await;
    session.end().await;

    let copies: Vec<_> = pick!(
        log_events(&store),
        LogEvent::SessionStarted {
            tools,
            tools_sha256,
            whole,
            ..
        }
        | LogEvent::SessionResumed {
            tools,
            tools_sha256,
            whole,
            ..
        } => (
            tools,
            tools_sha256,
            whole.tool_definitions,
            whole.deferred_definitions,
        )
    );
    assert_eq!(copies.len(), 2);
    let started = registry(Tools::Default)
        .start(
            &registry(Tools::Default).default_on(),
            Arc::new(crate::plugin::KernelHost::new()),
        )
        .await;
    let runner = serde_json::to_value(
        started
            .toolbox
            .runner(crate::search::WEB_SEARCH)
            .unwrap()
            .spec(),
    )
    .unwrap();
    for (tools, digest, definitions, deferred) in &copies {
        assert!(!definitions.is_empty());
        let mut all: Vec<serde_json::Value> = definitions.clone();
        all.extend(deferred.iter().cloned().map(|mut d| {
            d["defer_loading"] = json!(true);
            d
        }));
        all.extend(
            tools
                .iter()
                .filter(|name| !definitions.iter().any(|d| d["name"] == **name))
                .map(|_| runner.clone()),
        );
        assert_eq!(
            all.len(),
            definitions.len() + deferred.len() + 1,
            "{tools:?}"
        );
        all.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        let json = serde_json::to_string(&all).unwrap();
        assert_eq!(
            digest.as_deref(),
            Some(crate::session::sha256_hex(&json).as_str()),
            "{tools:?}"
        );
    }
    let sent = |index: usize| -> Vec<String> {
        body(&server, index)["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| t.get("defer_loading").is_none())
            .filter_map(|t| t["name"].as_str().map(str::to_owned))
            .collect()
    };
    assert_eq!(copies[0].0, sent(0));
    assert_eq!(copies[1].0, sent(3));
    assert!(!sent(0).contains(&"show_diff".to_owned()));
    assert!(deferred_in(&body(&server, 0)).contains(&"show_diff".to_owned()));
    assert!(copies[0].3.iter().any(|d| d["name"] == "show_diff"));
}

#[tokio::test]
async fn the_summary_fork_after_an_inline_change_sends_the_turns_tools_system_and_betas() {
    let env = Env::new("switch-summary-fork");
    let server = Server::start(vec![
        says("msg_01", 40_000, "One."),
        says("msg_02", 40_100, "Two."),
        reply("msg_03", 40_200, vec![Part::Text(SUMMARY)], "end_turn"),
        says("msg_04", 640, "Three."),
    ])
    .await;
    let store = MemoryStore::default();
    let mut kit = switching(&server, &env, &store);
    kit.models = models("[models.\"claude-opus-5-5\"]\nnative_compaction = false\n");
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, without("show_diff")).await;
    session.turn("u2", "two").await;
    session.compact(2).await;
    session.turn("u3", "three").await;
    let agent = session.end().await;
    let (turn, fork) = (body(&server, 1), body(&server, 2));
    assert_eq!(fork["tools"], turn["tools"]);
    assert_eq!(fork["system"], turn["system"]);
    assert!(betas(&server, 1).contains(&INLINE_TOOLS_BETA.to_owned()));
    assert_eq!(betas(&server, 2), betas(&server, 1));
    let unmarked = |messages: &serde_json::Value| -> Vec<serde_json::Value> {
        let mut messages = messages.as_array().unwrap().clone();
        for block in messages.iter_mut().flat_map(|m| {
            m["content"]
                .as_array_mut()
                .map(|c| c.iter_mut())
                .into_iter()
                .flatten()
        }) {
            if let Some(block) = block.as_object_mut() {
                block.remove("cache_control");
            }
        }
        messages
    };
    let shared = unmarked(&turn["messages"]);
    assert_eq!(unmarked(&fork["messages"])[..shared.len()], shared[..]);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn turning_bash_off_takes_its_shell_line_out_of_the_next_environment() {
    let env = Env::new("switch-shell-line");
    let server = Server::start(vec![
        says("msg_01", 40_000, "One."),
        says("msg_02", 40_100, "Two."),
        reply("msg_03", 40_200, vec![Part::Text(SUMMARY)], "end_turn"),
        says("msg_04", 640, "Three."),
    ])
    .await;
    let store = MemoryStore::default();
    let mut kit = switching(&server, &env, &store);
    kit.models = models("[models.\"claude-opus-5-5\"]\nnative_compaction = false\n");
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, without("bash")).await;
    session.turn("u2", "two").await;
    session.compact(2).await;
    session.turn("u3", "three").await;
    let agent = session.end().await;
    let environments = |n: usize| -> Vec<String> {
        body(&server, n)["messages"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(texts)
            .filter(|t| t.starts_with("<environment>"))
            .collect()
    };
    let shell = "Shell for the bash tool";
    assert!(environments(0)[0].contains(shell));
    assert_eq!(environments(1), environments(0), "the sent block stays");
    let after = environments(3);
    assert_eq!(after.len(), 1, "{after:?}");
    assert!(!after[0].contains(shell), "{after:?}");
    assert!(after[0].contains("Working directory"));
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_note_whose_message_a_rewind_replaced_is_no_longer_read() {
    let env = Env::new("switch-note-replaced");
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
        says("msg_03", 520, "Two again."),
        says("msg_04", 640, "Three."),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = switching(&server, &env, &store);
    let built = kit.build(on(PLAIN, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, without("show_diff")).await;
    session.turn("u2", "two").await;
    session.tools(2, every_default()).await;
    session
        .send(ClientCommand::Rewind {
            to: wisp_protocol::RewindPoint::Turn { turn: 2 },
            request: RequestId(3),
        })
        .await;
    session
        .until(|e| {
            matches!(e, AgentEvent::Rewound { .. })
                || matches!(e, AgentEvent::Rejected { request: Some(r), .. } if *r == RequestId(3))
        })
        .await;
    session.turn("u3", "two, again").await;
    let third = session.turn("u4", "three").await;
    assert!(changed(&third).is_empty(), "{:?}", changed(&third));
    let agent = session.end().await;
    assert_eq!(notes_in(&body(&server, 1)), 1);
    for n in 2..4 {
        let noted: Vec<String> = body(&server, n)["messages"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(texts)
            .filter(|t| t.starts_with("Tools turned"))
            .collect();
        assert!(noted.is_empty(), "request {n}: {noted:?}");
    }
    rebuilds(&store, &agent).await;
}

fn no_deferral(request: &Value) -> bool {
    !request.to_string().contains("defer_loading")
}

fn declared(request: &Value) -> Vec<Value> {
    request["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["defer_loading"] != json!(true))
        .cloned()
        .collect()
}

fn removed_flag(id: &str) -> crate::settings::Given {
    crate::plugin::testing::plugin_flag(id, wisp_protocol::settings::INSTALLED_KEY, false)
}

fn registry_without(ids: &[&str]) -> Arc<crate::plugin::Registry> {
    registry_with(ids, None)
}

fn registry_with(
    ids: &[&str],
    show_diff: Option<crate::plugin::Make>,
) -> Arc<crate::plugin::Registry> {
    use crate::plugin::{PluginConfig, PluginSwitches, Registry};
    let sources = wisp_plugins::bundled()
        .into_iter()
        .map(|b| {
            let make: crate::plugin::Make = if b.manifest == wisp_plugin_shell::MANIFEST {
                crate::tool::testkit::shell_make(wisp_plugin_shell::Timing::default())
            } else if let Some(make) = show_diff
                .clone()
                .filter(|_| b.manifest == wisp_plugin_show_diff::MANIFEST)
            {
                make
            } else {
                Arc::new(move |host| (b.instantiate)(host))
            };
            (b.manifest.to_owned(), make)
        })
        .collect();
    let config = PluginConfig {
        switches: PluginSwitches {
            off: std::collections::BTreeSet::new(),
            removed: ids.iter().map(|id| (*id).to_owned()).collect(),
        },
        ..PluginConfig::default()
    };
    let (registry, problems) = Registry::from_sources(sources, &config);
    assert!(problems.is_empty(), "{problems:?}");
    Arc::new(registry)
}

fn plugin_gone(server: &Server, env: &Env, store: &MemoryStore, ids: &[&str]) -> Kit {
    plugins_kit(server, env, store, ids, None)
}

fn plugins_kit(
    server: &Server,
    env: &Env,
    store: &MemoryStore,
    ids: &[&str],
    show_diff: Option<crate::plugin::Make>,
) -> Kit {
    let registry = registry_with(ids, show_diff);
    let on = registry.default_on();
    let mut kit = Kit {
        tools: Arc::new(crate::plugin::Tools::new(registry, on)),
        ..switching(server, env, store)
    };
    kit.start_layers
        .flags
        .extend(ids.iter().map(|id| removed_flag(id)));
    kit
}

fn client_names(request: &Value) -> Vec<String> {
    request["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str().map(str::to_owned))
        .collect()
}

#[tokio::test]
async fn tool_on_by_reference() {
    let env = Env::new("tool-on-by-reference");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = without_show_diff(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let key = built.agent.prefix_key();
    assert!(!counted_names(&built.agent).contains(&"show_diff".to_owned()));
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.tools(1, every_default()).await;
    let second = session.turn("u2", "two").await;
    assert_eq!(
        changed(&second),
        [(
            vec!["show_diff".to_owned()],
            Vec::new(),
            ToolChangeVia::Inline,
            Some(CacheEffect::Kept)
        )]
    );
    session.turn("u3", "three").await;
    let agent = session.end().await;
    let bodies: Vec<Value> = (0..3).map(|n| body(&server, n)).collect();
    for request in &bodies {
        assert_eq!(request["tools"], bodies[0]["tools"]);
        assert_eq!(request["system"], bodies[0]["system"]);
    }
    assert!(deferred_in(&bodies[0]).contains(&"show_diff".to_owned()));
    assert!(
        bodies[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| t.get("defer_loading").is_some())
            .all(|t| t.get("cache_control").is_none())
    );
    assert_eq!(
        client_names(&bodies[0])
            .iter()
            .filter(|n| *n == "show_diff")
            .count(),
        1
    );
    let (_, system) = last_two(&bodies[1]);
    assert_eq!(kinds(&system), ["tool_addition show_diff", "text"]);
    assert!(added_by_name(&agent, "show_diff"));
    let earlier = bodies[1]["messages"].to_string();
    assert!(
        bodies[2]["messages"]
            .to_string()
            .starts_with(earlier.strip_suffix(']').unwrap()),
        "the request after the surfacing strictly appends"
    );
    assert_eq!(agent.prefix_key(), key, "the prefix key holds");
    assert!(marked(&store).is_empty(), "{:?}", marked(&store));
    let counted = counted_names(&agent);
    assert_eq!(counted.iter().filter(|n| *n == "show_diff").count(), 1);
    for name in deferred_in(&bodies[0]).iter().filter(|n| *n != "show_diff") {
        assert!(!counted.contains(name), "{name} was never surfaced");
    }
    let seen = server.seen();
    assert!(seen.counts.iter().all(|c| !c.contains("defer_loading")));
    rebuilds(&store, &agent).await;
    check("tool_on_by_reference", &seen);
}

#[tokio::test]
async fn a_pair_without_deferred_tools_leaves_them_off_and_a_tool_turned_on_there_rebuilds_out_of_the_set()
 {
    let env = Env::new("switch-deferred-pairs");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = without_show_diff(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    change(&mut session, 1, model(PLAIN)).await;
    session.tools(2, every_default()).await;
    let plain = session.turn("u2", "two").await;
    assert_eq!(
        changed(&plain),
        [(
            vec!["show_diff".to_owned()],
            Vec::new(),
            ToolChangeVia::Rebuild,
            Some(CacheEffect::Rebuilt {
                reason: wisp_protocol::RebuildReason::ToolsChange,
            })
        )]
    );
    change(&mut session, 3, model(CAPABLE)).await;
    let back = session.turn("u3", "three").await;
    assert!(changed(&back).is_empty(), "{:?}", changed(&back));
    let agent = session.end().await;
    let (first, second, third) = (body(&server, 0), body(&server, 1), body(&server, 2));
    assert!(deferred_in(&first).contains(&"show_diff".to_owned()));
    assert!(no_deferral(&second), "a plain pair sends no deferred entry");
    assert!(client_names(&second).contains(&"show_diff".to_owned()));
    assert_eq!(
        deferred_in(&third),
        ["show_page"],
        "the rebuild took show_diff out of the set"
    );
    assert_eq!(declared(&third), declared(&second));
    assert!(
        third["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["role"] != "system")
    );
    rebuilds(&store, &agent).await;
}

fn refused_deferral() -> Reply {
    Reply {
        response: http::json(
            "400 Bad Request",
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"tools.4.custom.defer_loading: Extra inputs are not permitted"}}"#,
        ),
        before: Vec::new(),
        gate: None,
    }
}

#[tokio::test]
async fn a_refused_deferral_resends_once_without_deferred_entries_and_tools_then_go_by_value() {
    let env = Env::new("switch-deferral-refused");
    let server = Server::start(vec![
        refused_deferral(),
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = without_show_diff(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    let first = session.turn("u1", "one").await;
    assert!(
        first.iter().any(|e| matches!(
            e,
            AgentEvent::FeatureStateChanged {
                feature: wisp_protocol::Feature::DeferredTools,
                state: wisp_protocol::log::FeatureState::FallenBack,
                ..
            }
        )),
        "{first:#?}"
    );
    session.tools(1, every_default()).await;
    session.turn("u2", "two").await;
    let agent = session.end().await;
    assert_eq!(server.seen().bodies.len(), 3, "resent once");
    let (refused, resent, later) = (body(&server, 0), body(&server, 1), body(&server, 2));
    assert!(deferred_in(&refused).contains(&"show_diff".to_owned()));
    assert!(no_deferral(&resent));
    let declared: Vec<Value> = refused["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t.get("defer_loading").is_none())
        .cloned()
        .collect();
    assert_eq!(resent["tools"], json!(declared));
    assert_eq!(resent["messages"], refused["messages"]);
    assert_eq!(later["tools"], resent["tools"]);
    let (_, system) = last_two(&later);
    assert_eq!(kinds(&system), ["tool_addition show_diff by value", "text"]);
    assert_eq!(
        logged_kinds(&store)
            .iter()
            .filter(|k| *k == "deferred_tools")
            .count(),
        1
    );
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_refused_deferral_with_a_tool_turned_on_first_resends_its_definition_by_value() {
    let env = Env::new("switch-deferral-refused-on");
    let server = Server::start(vec![
        refused_deferral(),
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = without_show_diff(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.tools(1, every_default()).await;
    session.turn("u1", "one").await;
    session.turn("u2", "two").await;
    let agent = session.end().await;
    assert_eq!(server.seen().bodies.len(), 3, "resent once");
    let (refused, resent, later) = (body(&server, 0), body(&server, 1), body(&server, 2));
    assert!(deferred_in(&refused).contains(&"show_diff".to_owned()));
    let (_, system) = last_two(&refused);
    assert_eq!(kinds(&system), ["tool_addition show_diff", "text"]);
    assert!(no_deferral(&resent));
    let (_, system) = last_two(&resent);
    assert_eq!(kinds(&system), ["tool_addition show_diff by value", "text"]);
    assert_eq!(system["content"][1], last_two(&refused).1["content"][1]);
    assert_eq!(later["tools"], resent["tools"]);
    let later_systems = later["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "system")
        .count();
    assert_eq!(later_systems, 1, "no new addition on the next turn");
    assert!(added_by_name(&agent, "show_diff"));
    assert_eq!(
        counted_names(&agent)
            .iter()
            .filter(|n| *n == "show_diff")
            .count(),
        1
    );
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_pair_without_inline_tools_never_defers() {
    let env = Env::new("switch-plain-defers-nothing");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let kit = without_show_diff(&server, &env, &store);
    let built = kit.build(on(PLAIN, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.end().await;
    let first = body(&server, 0);
    assert!(no_deferral(&first));
    assert!(!client_names(&first).contains(&"show_diff".to_owned()));
}

#[tokio::test]
async fn an_agent_that_cannot_switch_defers_nothing() {
    let env = Env::new("switch-unswitchable");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let mut kit = without_show_diff(&server, &env, &store);
    kit.start_layers.full_screen = false;
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    assert!(built.agent.deferred.is_empty());
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session.end().await;
    assert!(no_deferral(&body(&server, 0)));

    let root = without_show_diff(&server, &env, &store)
        .build(on(CAPABLE, kit::Start::Fresh))
        .await
        .unwrap()
        .agent;
    assert!(!root.deferred.is_empty(), "a full-screen root defers");
    let child = root.with_parent(wisp_protocol::ParentLink {
        session_id: "parent".into(),
        agent: wisp_protocol::AgentPath::root(),
        depth: 1,
        config: Default::default(),
        todos: Vec::new(),
    });
    assert!(child.deferred.is_empty(), "a child defers nothing");
}

#[tokio::test]
async fn the_tools_digest_covers_the_deferred_flags() {
    let env = Env::new("switch-deferred-digest");
    let server = Server::start(Vec::new()).await;
    let store = MemoryStore::default();
    let deferring = without_show_diff(&server, &env, &store)
        .build(on(CAPABLE, kit::Start::Fresh))
        .await
        .unwrap()
        .agent;
    let declared = switching(&server, &env, &store)
        .build(on(CAPABLE, kit::Start::Fresh))
        .await
        .unwrap()
        .agent;
    assert!(
        deferring
            .deferred
            .iter()
            .any(|spec| spec.name == "show_diff")
    );
    assert!(declared.sent.iter().any(|spec| spec.name == "show_diff"));
    assert!(
        !deferring.prefix_key().same_bytes(&declared.prefix_key()),
        "a deferred tool and the same tool declared are different arrays"
    );
    assert_ne!(
        deferring.current_tools().sha256,
        declared.current_tools().sha256
    );
}

#[tokio::test]
async fn a_root_with_nothing_to_defer_sends_the_array_a_plain_pair_sends() {
    let env = Env::new("switch-default-defers-nothing");
    let server = Server::start(vec![
        says("msg_01", 400, "One."),
        says("msg_02", 400, "One."),
    ])
    .await;
    let store = MemoryStore::default();
    let tui_default = |store: &MemoryStore| {
        let mut kit = switching(&server, &env, store);
        kit.tools = Arc::new(crate::plugin::Tools::new(
            registry_without(&["page"]),
            registry_without(&["page"]).on_tools_with(
                &every_default().into_iter().collect(),
                &std::collections::BTreeSet::new(),
            ),
        ));
        kit.start_layers.flags.push(removed_flag("page"));
        kit
    };
    let capable = tui_default(&store)
        .build(on(CAPABLE, kit::Start::Fresh))
        .await
        .unwrap()
        .agent;
    assert_eq!(
        capable.toolbox.held_names(),
        ["manage", "rename_chat", "report"]
    );
    assert!(
        capable.deferred.is_empty(),
        "{:?}: report can't be turned on in a root",
        capable.toolbox.held_names()
    );
    one_turn(capable, "u1").await;
    let plain = tui_default(&MemoryStore::default())
        .build(on(PLAIN, kit::Start::Fresh))
        .await
        .unwrap()
        .agent;
    one_turn(plain, "u1").await;
    let seen = server.seen();
    let tools = |n: usize| {
        let body = &seen.bodies[n];
        let from = body.find(r#""tools":["#).unwrap();
        let to = body.find(r#"],"system":"#).unwrap();
        body[from..to].to_owned()
    };
    assert!(!seen.bodies[0].contains("defer_loading"));
    assert!(!seen.bodies[0].contains("\"report\""));
    assert_eq!(tools(0), tools(1));
}

#[tokio::test]
async fn a_background_summary_sends_the_turns_tools_with_the_deferred_entries() {
    let env = Env::new("switch-deferred-background");
    let server = Server::start(vec![
        says("msg_01", 19_500, "Started."),
        reply(
            "msg_02",
            1_200,
            vec![Part::Compaction("So far: a start.")],
            "compaction",
        ),
        says("msg_03", 300, "Again."),
    ])
    .await;
    let store = MemoryStore::default();
    let mut kit = without_show_diff(&server, &env, &store);
    kit.models = models("[models.\"claude-opus-5-5\"]\ncompact_at = 20000\n");
    kit.start_layers.saved = background_layers(1_000).saved;
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "one").await;
    session
        .until(|e| matches!(e, AgentEvent::CompactionPrepared { .. }))
        .await;
    session.turn("u2", "two").await;
    session.end().await;
    assert_eq!(server.seen().bodies.len(), 3);
    let (turn, summary) = (body(&server, 0), body(&server, 1));
    assert!(deferred_in(&turn).contains(&"show_diff".to_owned()));
    assert!(summary.get("compaction").is_some());
    assert_eq!(summary["tools"], turn["tools"]);
}

mod plugins;
mod rebuild;
mod resumed;

mod openai;

mod found;
mod manage;
mod rename;
