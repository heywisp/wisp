use wisp_protocol::Scope;

use super::*;
use crate::agent::InstructionParts;
use crate::instructions::{DEFAULT_WARN_TOKENS, Disk, Extra, Paths};

#[derive(Debug, Clone)]
struct Parts {
    config_dir: PathBuf,
    parts: InstructionParts,
}

trait WithParts {
    fn with_instructions(self, parts: Parts) -> Self;
}

impl WithParts for Agent {
    fn with_instructions(self, parts: Parts) -> Self {
        self.with_test_instructions(parts.parts, Some(&parts.config_dir))
    }
}

fn load_in(env: &Env, source: &InstructionsSource) -> Parts {
    Parts {
        config_dir: env.root.join("config"),
        parts: InstructionParts {
            source: source.clone(),
            warn_tokens: DEFAULT_WARN_TOKENS,
            extra: None,
            preset: None,
        },
    }
}

fn load(env: &Env) -> Parts {
    load_in(env, &InstructionsSource::Auto)
}

fn files_at(env: &Env, start: PathBuf) -> wisp_instructions::Loaded {
    wisp_instructions::load(
        &Paths {
            start,
            config_dir: Some(env.root.join("config")),
            home: Some(env.root.clone()),
        },
        wisp_instructions::Options {
            source: wisp_instructions::Source::Auto,
            warn_tokens: DEFAULT_WARN_TOKENS,
        },
        &wisp_instructions::LoadAll,
        &Disk,
    )
}

fn files(env: &Env) -> wisp_instructions::Loaded {
    files_at(env, env.work.clone())
}

fn body_json(body: &str) -> Value {
    serde_json::from_str(body).unwrap()
}

fn chat_agent(provider: Arc<Fake>) -> Agent {
    Agent::new(provider, default_session()).with_clock(TickingClock::new())
}

#[tokio::test]
async fn the_project_block_is_a_second_system_block_with_its_own_breakpoint() {
    let env = Env::new("block");
    fs::write(env.work.join("AGENTS.md"), "AGENTS-RULES-CANARY\n").unwrap();
    let provider = scripted(vec![
        uses(&[call("a", "sleep", json!({"ms": 0}))]),
        done("one"),
        done("two"),
    ]);
    let mut agent = agent(provider.clone(), &env).with_instructions(load(&env));
    turn(&mut agent, "first").await;
    turn(&mut agent, "second").await;
    let plain = scripted(vec![done("one")]);
    let mut without = super::agent(plain.clone(), &env);
    turn(&mut without, "first").await;
    let base = body_json(&plain.texts()[0])["system"].clone();
    let bodies = provider.texts();
    for body in &bodies {
        let json = body_json(body);
        let system = json["system"].as_array().unwrap();
        assert_eq!(system.len(), 2, "{body}");
        assert_eq!(system[0], base[0], "the base block is unchanged");
        assert_eq!(
            system[1]["cache_control"],
            json!({"type": "ephemeral", "ttl": "1h"})
        );
        assert_eq!(
            system[0]["cache_control"],
            json!({"type": "ephemeral", "ttl": "1h"})
        );
        let block = system[1]["text"].as_str().unwrap();
        assert!(block.starts_with("<instructions>\n"));
        assert!(block.contains("AGENTS-RULES-CANARY"));
        let messages = serde_json::to_string(&json["messages"]).unwrap();
        assert!(!messages.contains("CANARY"), "{messages}");
        assert!(!messages.contains("<instructions"), "{messages}");
    }
    for pair in bodies.windows(2) {
        let earlier = pair[0].strip_suffix('}').unwrap();
        let earlier = earlier.strip_suffix(']').unwrap();
        assert!(
            pair[1].starts_with(earlier),
            "each request extends the last"
        );
    }
}

#[tokio::test]
async fn with_no_file_every_request_is_unchanged() {
    let env = Env::new("nofile");
    assert!(files(&env).block().is_none());
    let loaded = load(&env);
    let with = scripted(vec![done("one"), done("two")]);
    let without = scripted(vec![done("one"), done("two")]);
    let mut a = agent(with.clone(), &env).with_instructions(loaded.clone());
    let mut b = agent(without.clone(), &env);
    for text in ["first", "second"] {
        turn(&mut a, text).await;
        turn(&mut b, text).await;
    }
    assert_eq!(with.texts(), without.texts());
    let none = load_in(&env, &InstructionsSource::None);
    fs::write(env.work.join("AGENTS.md"), "rules\n").unwrap();
    let off = scripted(vec![done("one")]);
    let mut c = agent(off.clone(), &env).with_instructions(none);
    turn(&mut c, "first").await;
    assert_eq!(off.texts()[0], without.texts()[0]);
}

#[tokio::test]
async fn two_projects_share_the_base_block_and_two_clones_share_everything() {
    let a = Env::new("clone-a");
    let b = Env::new("clone-b");
    let c = Env::new("other");
    for env in [&a, &b] {
        fs::write(env.work.join("AGENTS.md"), "shared rules @docs/x.md\n").unwrap();
        fs::create_dir_all(env.work.join("docs")).unwrap();
        fs::write(env.work.join("docs/x.md"), "x\n").unwrap();
    }
    fs::write(c.work.join("AGENTS.md"), "other rules\n").unwrap();
    let mut systems = Vec::new();
    for env in [&a, &b, &c] {
        let provider = scripted(vec![done("one")]);
        let mut agent = agent(provider.clone(), env).with_instructions(load(env));
        turn(&mut agent, "hi").await;
        systems.push(body_json(&provider.texts()[0])["system"].clone());
    }
    assert_eq!(systems[0], systems[1]);
    assert_eq!(systems[0][0], systems[2][0]);
    assert_ne!(systems[0][1], systems[2][1]);
}

#[tokio::test]
async fn a_session_without_tools_loads_instructions_but_sends_no_context() {
    let env = Env::new("chat");
    let store = MemoryStore::default();
    let provider = scripted(vec![done("one"), done("two")]);
    let mut agent = chat_agent(provider.clone())
        .with_workspace(env.workspace())
        .with_instructions(load(&env))
        .with_log(
            SessionLog::new(Box::new(store.clone()), "s1"),
            log_context(),
        );
    turn(&mut agent, "hello").await;
    let body = &provider.texts()[0];
    assert!(!body.contains("environment"));
    assert!(!body.contains(r#""tools""#));
    assert_eq!(body_json(body)["system"].as_array().unwrap().len(), 1);
    let LogEvent::SessionStarted {
        cwd, output_dir, ..
    } = &log_events(&store)[0]
    else {
        panic!("not session_started");
    };
    assert_eq!(cwd.as_deref(), Some("~/work"));
    assert_eq!(output_dir, &None);

    fs::write(env.work.join("AGENTS.md"), "chat rules\n").unwrap();
    let mut agent = chat_agent(provider.clone())
        .with_workspace(env.workspace())
        .with_instructions(load(&env));
    turn(&mut agent, "again").await;
    let body = body_json(&provider.texts()[1]);
    assert_eq!(body["system"].as_array().unwrap().len(), 2);
    let first = &body["messages"][0]["content"];
    assert_eq!(
        first.as_array().unwrap().len(),
        3,
        "the stamp, the context line and the text only"
    );
    assert!(
        !serde_json::to_string(first)
            .unwrap()
            .contains("environment")
    );
}

#[tokio::test]
async fn the_first_entry_announces_the_instructions_after_session_started() {
    let env = Env::new("announce");
    fs::write(env.work.join("AGENTS.md"), "rules\n").unwrap();
    let store = MemoryStore::default();
    let provider = scripted(vec![done("one")]);
    let agent = logged(provider, &env, &store).with_instructions(load(&env));
    let (commands, mut events, _task) = spawn(agent);
    let first = events.recv().await.unwrap();
    let AgentEvent::InstructionsLoaded { turn, chosen, .. } = &first else {
        panic!("{first:?}");
    };
    assert_eq!(*turn, None);
    assert_eq!(
        chosen.as_ref().map(|c| c.scope.clone()),
        Some(Scope::Project)
    );
    commands
        .send(ClientCommand::Send {
            id: InputId("i1".into()),
            text: "hello".into(),
            images: Vec::new(),
            mode: DeliveryMode::Steer,
            turn: None,
            replaces: None,
        })
        .await
        .unwrap();
    let mut rest = Vec::new();
    until_turn_end(&mut events, &mut rest).await;
    drop(commands);
    while events.recv().await.is_some() {}
    assert!(
        !rest
            .iter()
            .any(|e| matches!(e, AgentEvent::InstructionsLoaded { .. }))
    );
    let records = log_events(&store);
    assert!(matches!(records[0], LogEvent::SessionStarted { .. }));
    let LogEvent::InstructionsLoaded { turn, chosen, .. } = &records[1] else {
        panic!("{:?}", records[1]);
    };
    assert_eq!(*turn, None);
    assert_eq!(chosen.as_ref().unwrap().name, "AGENTS.md");
    let line = serde_json::to_value(&test_guard::records(&store)[1]).unwrap();
    assert_eq!(line["turn"], Value::Null);
    assert_eq!(
        records
            .iter()
            .filter(|r| matches!(r, LogEvent::InstructionsLoaded { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn without_a_log_the_event_still_comes_exactly_once() {
    let env = Env::new("nolog");
    fs::write(env.work.join("AGENTS.md"), "rules\n").unwrap();
    let provider = scripted(vec![done("one"), done("two")]);
    let mut agent = agent(provider, &env).with_instructions(load(&env));
    let (_, first) = turn(&mut agent, "first").await;
    let (_, second) = turn(&mut agent, "second").await;
    let count = |events: &[AgentEvent]| {
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::InstructionsLoaded { turn: None, .. }))
            .count()
    };
    assert_eq!(count(&first), 1);
    assert_eq!(count(&second), 0);
    let plain = scripted(vec![done("one")]);
    let mut without = super::agent(plain, &env);
    let (_, events) = turn(&mut without, "first").await;
    assert_eq!(count(&events), 0, "no with_instructions, no event");
}

#[tokio::test]
async fn an_edit_before_the_first_message_keeps_the_construction_time_text() {
    let env = Env::new("edit");
    fs::write(env.work.join("AGENTS.md"), "version one\n").unwrap();
    let sha = files(&env).summary.chosen.as_ref().unwrap().sha256.clone();
    let loaded = load(&env);
    let provider = scripted(vec![done("one")]);
    let mut agent = agent(provider.clone(), &env).with_instructions(loaded);
    fs::write(env.work.join("AGENTS.md"), "version two\n").unwrap();
    let (_, events) = turn(&mut agent, "first").await;
    let block = provider.sent()[0].session.instructions.clone().unwrap();
    assert!(block.contains("version one") && !block.contains("version two"));
    let announced = events.iter().find_map(|e| match e {
        AgentEvent::InstructionsLoaded { chosen, .. } => chosen.clone(),
        _ => None,
    });
    assert_eq!(announced.map(|c| c.sha256), Some(sha));
}

#[tokio::test]
async fn the_stamp_stays_in_the_turn_message() {
    let env = Env::new("stamp");
    fs::write(env.work.join("AGENTS.md"), "rules\n").unwrap();
    let provider = scripted(vec![done("one")]);
    let mut agent = agent(provider.clone(), &env).with_instructions(load(&env));
    turn(&mut agent, "first").await;
    let request = &provider.sent()[0];
    let system = request.session.system.clone().unwrap();
    let instructions = request.session.instructions.clone().unwrap();
    assert!(!system.contains("Sent:") || system.contains("'Sent:'"));
    assert!(!instructions.contains("Sent:"));
    let content = &request.messages[0].content;
    assert!(
        matches!(&content[1], ContentBlock::Text { text } if text.starts_with("Sent: ")),
        "{content:?}"
    );

    let mut session = default_session();
    session.message_time = MessageTime::Off;
    let off = scripted(vec![done("one")]);
    let mut agent = Agent::new(off.clone(), session.clone())
        .with_clock(TickingClock::new())
        .with_tools(test_toolbox(), env.workspace())
        .with_platform("Linux")
        .with_resources(None)
        .with_instructions(load(&env));
    turn(&mut agent, "first").await;
    let plain = scripted(vec![done("one")]);
    let mut base = Agent::new(plain.clone(), session)
        .with_clock(TickingClock::new())
        .with_tools(test_toolbox(), env.workspace())
        .with_platform("Linux")
        .with_resources(None);
    turn(&mut base, "first").await;
    let request = &off.sent()[0];
    assert_eq!(request.session.system, plain.sent()[0].session.system);
    assert!(
        !serde_json::to_string(&request.messages)
            .unwrap()
            .contains("Sent:")
    );
}

#[tokio::test]
async fn the_project_root_line_names_the_root_when_started_below_it() {
    let env = Env::new("root");
    crate::instructions::make_repository(&env.work).unwrap();
    fs::write(env.work.join("AGENTS.md"), "rules\n").unwrap();
    let sub = env.work.join("crates");
    fs::create_dir_all(&sub).unwrap();
    let workspace = Workspace {
        root: sub.clone(),
        ..env.workspace()
    };
    assert!(files_at(&env, sub).project_root().is_some());
    let loaded = load(&env);
    let provider = scripted(vec![done("one")]);
    let mut agent = Agent::new(provider.clone(), default_session())
        .with_clock(TickingClock::new())
        .with_tools(test_toolbox(), workspace)
        .with_platform("Linux")
        .with_resources(None)
        .with_instructions(loaded);
    turn(&mut agent, "first").await;
    assert_eq!(
        provider.sent()[0].messages[0].content[0],
        ContentBlock::Text {
            text: "<environment>\nWorking directory: ~/work/crates\nProject root: ~/work\nPlatform: Linux\n</environment>\n".into()
        }
    );
    let block = provider.sent()[0].session.instructions.clone().unwrap();
    assert!(block.contains("name=\"AGENTS.md\""));
    assert!(!block.contains("work"));
}

#[tokio::test]
async fn a_failed_first_request_counts_the_project_block() {
    let env = Env::new("failed");
    fs::write(env.work.join("AGENTS.md"), "x".repeat(4000)).unwrap();
    let loaded = files(&env);
    let block_len = loaded.block().unwrap().len() as u64;
    let session = default_session();
    let with = retry::request_tokens(
        session.system.as_deref(),
        loaded.block(),
        &[],
        &[],
        &[],
        &[Message::user_text("hi")],
    );
    let without = retry::request_tokens(
        session.system.as_deref(),
        None,
        &[],
        &[],
        &[],
        &[Message::user_text("hi")],
    );
    assert_eq!(with, without + block_len);
}

fn resumed_in(
    env: &Env,
    records: &[wisp_protocol::log::LogRecord],
) -> (Agent, MemoryStore, Arc<Fake>) {
    let (store, log) = continued(records);
    let provider = scripted(vec![done("two")]);
    let agent = agent(provider.clone(), env)
        .with_instructions(load(env))
        .with_log(log, log_context())
        .resume(records, 0)
        .unwrap();
    (agent, store, provider)
}

fn announced(events: &[AgentEvent]) -> Vec<Option<String>> {
    pick!(
        events,
        AgentEvent::InstructionsLoaded {
            turn: None, chosen, ..
        } => chosen.as_ref().map(|c| c.sha256.clone())
    )
}

#[tokio::test]
async fn a_resumed_session_keeps_its_logged_instructions_and_announces_them_once() {
    let env = Env::new("resumed");
    fs::write(env.work.join("AGENTS.md"), "version one\n").unwrap();
    let store = MemoryStore::default();
    let sha = files(&env).summary.chosen.as_ref().unwrap().sha256.clone();
    let first = load(&env);
    let mut live = logged(scripted(vec![done("one")]), &env, &store).with_instructions(first);
    turn(&mut live, "first").await;
    let records = records(&store);
    fs::write(env.work.join("AGENTS.md"), "version two\n").unwrap();

    let (mut again, written, provider) = resumed_in(&env, &records);
    let notice = again.resume_notice_text().unwrap().to_owned();
    assert!(!notice.contains("changed"), "{notice}");
    assert!(!again.settings.binding_controls);
    let restored = again.instructions.as_ref().unwrap();
    assert_eq!(
        restored.summary.chosen.as_ref().map(|c| c.sha256.clone()),
        Some(sha.clone())
    );
    let (_, events) = turn(&mut again, "second").await;
    assert_eq!(announced(&events), [Some(sha)]);
    let block = provider.sent()[0].session.instructions.clone().unwrap();
    assert!(block.contains("version one") && !block.contains("version two"));
    let loaded_records = test_guard::records(&written)
        .iter()
        .filter(|r| matches!(r.event, LogEvent::InstructionsLoaded { .. }))
        .count();
    assert_eq!(loaded_records, 1, "no second instructions_loaded record");
}

#[tokio::test]
async fn a_damaged_project_block_falls_back_to_the_current_files() {
    let env = Env::new("damaged");
    fs::write(env.work.join("AGENTS.md"), "version one\n").unwrap();
    let store = MemoryStore::default();
    let mut live = logged(scripted(vec![done("one")]), &env, &store).with_instructions(load(&env));
    turn(&mut live, "first").await;
    let mut records = records(&store);
    if let LogEvent::SessionStarted { instructions, .. } = &mut records[0].event {
        *instructions = Some("tampered".into());
    }
    fs::write(env.work.join("AGENTS.md"), "version two\n").unwrap();
    let now = files(&env).summary.chosen.unwrap().sha256;

    let (mut again, store, provider) = resumed_in(&env, &records);
    let notice = again.resume_notice_text().unwrap().to_owned();
    assert!(
        notice.contains(" Wisp's prompt or tools changed since this session: the cache is rebuilt"),
        "{notice}"
    );
    assert!(again.settings.binding_controls);
    let (_, events) = turn(&mut again, "second").await;
    assert_eq!(announced(&events), [Some(now)]);
    let block = provider.sent()[0].session.instructions.clone().unwrap();
    assert!(block.contains("version two"));

    let records = test_guard::records(&store);
    let (mut third, _, provider) = resumed_in(&env, &records);
    let notice = third.resume_notice_text().unwrap().to_owned();
    assert!(!notice.contains("prompt or tools changed"), "{notice}");
    turn(&mut third, "third").await;
    let block = provider.sent()[0].session.instructions.clone().unwrap();
    assert!(block.contains("version two"), "the block the last run sent");
}

#[tokio::test]
async fn a_damaged_project_block_keeps_the_logged_source() {
    let env = Env::new("damaged-source");
    fs::write(env.work.join("AGENTS.md"), "project rules\n").unwrap();
    let store = MemoryStore::default();
    let mut live = logged(scripted(vec![done("one")]), &env, &store).with_instructions(load(&env));
    turn(&mut live, "first").await;
    let mut records = records(&store);
    if let LogEvent::SessionStarted { instructions, .. } = &mut records[0].event {
        *instructions = Some("tampered".into());
    }
    let (_, log) = continued(&records);
    let again = agent(scripted(Vec::new()), &env)
        .with_instructions(load_in(&env, &InstructionsSource::None))
        .with_log(log, log_context())
        .resume(&records, 0)
        .unwrap();
    assert_eq!(
        again.instructions.as_ref().map(|s| s.source.clone()),
        Some(InstructionsSource::Auto)
    );
}

#[tokio::test]
async fn a_resumed_session_keeps_its_logged_source_whatever_this_run_loads() {
    let env = Env::new("source");
    fs::write(env.work.join("AGENTS.md"), "project rules\n").unwrap();
    let store = MemoryStore::default();
    let mut live = logged(scripted(vec![done("one")]), &env, &store).with_instructions(load(&env));
    turn(&mut live, "first").await;
    let records = records(&store);
    let (_, log) = continued(&records);
    let provider = scripted(vec![done("two")]);
    let mut again = agent(provider.clone(), &env)
        .with_instructions(load_in(&env, &InstructionsSource::None))
        .with_log(log, log_context())
        .resume(&records, 0)
        .unwrap();
    assert_eq!(
        again.instructions.as_ref().map(|s| s.source.clone()),
        Some(InstructionsSource::Auto)
    );
    turn(&mut again, "second").await;
    let block = provider.sent()[0].session.instructions.clone().unwrap();
    assert!(block.contains("project rules"));
}

#[tokio::test]
async fn a_block_an_earlier_resume_replaced_is_not_restored() {
    let env = Env::new("replaced");
    fs::write(env.work.join("AGENTS.md"), "version one\n").unwrap();
    let store = MemoryStore::default();
    let mut live = logged(scripted(vec![done("one")]), &env, &store).with_instructions(load(&env));
    turn(&mut live, "first").await;
    let records = records(&store);
    fs::write(env.work.join("AGENTS.md"), "version two\n").unwrap();
    let probe = scripted(vec![done("probe")]);
    let mut fresh =
        logged(probe.clone(), &env, &MemoryStore::default()).with_instructions(load(&env));
    turn(&mut fresh, "probe").await;
    let two = probe.sent()[0].session.instructions.clone().unwrap();

    let (mut again, resumed_store, _) = resumed_in(&env, &records);
    turn(&mut again, "second").await;
    let mut records = test_guard::records(&resumed_store);
    for record in &mut records {
        if let LogEvent::SessionResumed {
            instructions_sha256,
            ..
        } = &mut record.event
        {
            *instructions_sha256 = Some(crate::session::sha256_hex(&two));
        }
    }
    let (mut third, _, provider) = resumed_in(&env, &records);
    let notice = third.resume_notice_text().unwrap().to_owned();
    assert!(!notice.contains("prompt or tools changed"), "{notice}");
    turn(&mut third, "third").await;
    let block = provider.sent()[0].session.instructions.clone().unwrap();
    assert_eq!(block, two, "the block the last run sent");
}

fn flagged_resume(
    env: &Env,
    records: &[wisp_protocol::log::LogRecord],
    source: &str,
) -> (Agent, MemoryStore, Arc<Fake>) {
    use wisp_protocol::settings::{SettingValue, Target, find};
    let (store, log) = continued(records);
    let provider = scripted(vec![done("two")]);
    let flags = vec![crate::settings::Given {
        target: Target::Name(find("instructions.source").unwrap()),
        value: SettingValue::Text(source.into()),
        origin: "--instructions".into(),
    }];
    let agent = agent(provider.clone(), env)
        .with_instructions(load_in(env, &InstructionsSource::parse(source).unwrap()))
        .with_start_layers(crate::settings::host::StartLayers {
            flags,
            ..Default::default()
        })
        .with_log(log, log_context())
        .resume(records, 0)
        .unwrap();
    (agent, store, provider)
}

#[tokio::test]
async fn an_instructions_flag_on_resume_replaces_the_block_once_and_the_next_resume_keeps_it() {
    let env = Env::new("flag-none");
    fs::write(env.work.join("AGENTS.md"), "project rules\n").unwrap();
    let store = MemoryStore::default();
    let mut live = logged(scripted(vec![done("one")]), &env, &store).with_instructions(load(&env));
    turn(&mut live, "first").await;
    let records = records(&store);

    let (mut again, written, provider) = flagged_resume(&env, &records, "none");
    let notice = again.resume_notice_text().unwrap().to_owned();
    assert!(
        notice.contains(" Your flags change the project instructions: the cache is rebuilt"),
        "{notice}"
    );
    assert!(!notice.contains("Wisp's prompt"), "{notice}");
    assert!(again.settings.binding_controls);
    turn(&mut again, "second").await;
    assert_eq!(provider.sent()[0].session.instructions, None);
    let events: Vec<LogEvent> = log_events(&written);
    let resumed = events
        .iter()
        .find_map(|e| match e {
            LogEvent::SessionResumed {
                instructions_source,
                instructions,
                instructions_sha256,
                ..
            } => Some((
                instructions_source.clone(),
                instructions.clone(),
                instructions_sha256.clone(),
            )),
            _ => None,
        })
        .unwrap();
    assert_eq!(resumed, (Some(InstructionsSource::None), None, None));
    let loaded = events
        .iter()
        .filter(|e| matches!(e, LogEvent::InstructionsLoaded { .. }))
        .count();
    assert_eq!(loaded, 2, "a changed block writes instructions_loaded");

    let records = test_guard::records(&written);
    let (mut third, _, provider) = resumed_in(&env, &records);
    let notice = third.resume_notice_text().unwrap().to_owned();
    assert!(!notice.contains("changed"), "{notice}");
    assert!(!notice.contains("flags"), "{notice}");
    assert_eq!(
        third.instructions.as_ref().map(|s| s.source.clone()),
        Some(InstructionsSource::None)
    );
    turn(&mut third, "third").await;
    assert_eq!(provider.sent()[0].session.instructions, None);
}

#[tokio::test]
async fn an_instructions_flag_equal_to_the_logged_source_is_a_plain_resume() {
    let env = Env::new("flag-same");
    fs::write(env.work.join("AGENTS.md"), "version one\n").unwrap();
    let store = MemoryStore::default();
    let mut live = logged(scripted(vec![done("one")]), &env, &store).with_instructions(load(&env));
    turn(&mut live, "first").await;
    let records = records(&store);
    fs::write(env.work.join("AGENTS.md"), "version two\n").unwrap();
    let (mut again, written, provider) = flagged_resume(&env, &records, "auto");
    let notice = again.resume_notice_text().unwrap().to_owned();
    assert!(!notice.contains("changed"), "{notice}");
    assert!(!notice.contains("next message"), "{notice}");
    turn(&mut again, "second").await;
    let block = provider.sent()[0].session.instructions.clone().unwrap();
    assert!(block.contains("version one"), "{block}");
    let wrote = log_events(&written).into_iter().any(|e| {
        matches!(
            e,
            LogEvent::SessionResumed {
                instructions: Some(_),
                ..
            } | LogEvent::SessionResumed {
                instructions_source: Some(_),
                ..
            }
        )
    });
    assert!(!wrote);
}

#[tokio::test]
async fn a_log_from_before_the_instructions_copy_resumes_as_before() {
    let env = Env::new("schema-31");
    fs::write(env.work.join("AGENTS.md"), "version one\n").unwrap();
    let store = MemoryStore::default();
    let mut live = logged(scripted(vec![done("one")]), &env, &store).with_instructions(load(&env));
    turn(&mut live, "first").await;
    let mut records = records(&store);
    for record in &mut records {
        record.schema_version = 31;
    }
    fs::write(env.work.join("AGENTS.md"), "version two\n").unwrap();
    let (mut again, _, provider) = resumed_in(&env, &records);
    let notice = again.resume_notice_text().unwrap().to_owned();
    assert!(!notice.contains("changed"), "{notice}");
    turn(&mut again, "second").await;
    let block = provider.sent()[0].session.instructions.clone().unwrap();
    assert!(block.contains("version one"), "{block}");
}

#[tokio::test]
async fn a_damaged_block_read_back_from_the_files_is_logged_so_the_next_resume_restores_it() {
    let env = Env::new("damaged-exact");
    fs::write(env.work.join("AGENTS.md"), "version one\n").unwrap();
    let store = MemoryStore::default();
    let mut live = logged(scripted(vec![done("one")]), &env, &store).with_instructions(load(&env));
    turn(&mut live, "first").await;
    let mut records = records(&store);
    if let LogEvent::SessionStarted { instructions, .. } = &mut records[0].event {
        *instructions = Some("tampered".into());
    }
    let (mut again, written, _) = resumed_in(&env, &records);
    let notice = again.resume_notice_text().unwrap().to_owned();
    assert!(!notice.contains("changed"), "{notice}");
    turn(&mut again, "second").await;
    fs::write(env.work.join("AGENTS.md"), "version two\n").unwrap();
    let records = test_guard::records(&written);
    let (mut third, _, provider) = resumed_in(&env, &records);
    let notice = third.resume_notice_text().unwrap().to_owned();
    assert!(!notice.contains("changed"), "{notice}");
    turn(&mut third, "third").await;
    let block = provider.sent()[0].session.instructions.clone().unwrap();
    assert!(
        block.contains("version one") && !block.contains("version two"),
        "{block}"
    );
}

#[tokio::test]
async fn a_model_flag_beside_a_damaged_block_names_both_causes() {
    use wisp_protocol::settings::{SettingValue, Target, find};
    let env = Env::new("damaged-flag");
    fs::write(env.work.join("AGENTS.md"), "version one\n").unwrap();
    let store = MemoryStore::default();
    let mut live = logged(scripted(vec![done("one")]), &env, &store).with_instructions(load(&env));
    turn(&mut live, "first").await;
    let mut records = records(&store);
    if let LogEvent::SessionStarted { instructions, .. } = &mut records[0].event {
        *instructions = Some("tampered".into());
    }
    fs::write(env.work.join("AGENTS.md"), "version two\n").unwrap();
    let (_, log) = continued(&records);
    let flags = vec![crate::settings::Given {
        target: Target::Name(find("model").unwrap()),
        value: SettingValue::Text("claude-sonnet-5-5".into()),
        origin: "--model".into(),
    }];
    let again = agent(scripted(Vec::new()), &env)
        .with_instructions(load(&env))
        .with_start_layers(crate::settings::host::StartLayers {
            flags,
            ..Default::default()
        })
        .with_log(log, log_context())
        .resume(&records, 0)
        .unwrap();
    let notice = again.resume_notice_text().unwrap().to_owned();
    assert!(
        notice.contains(
            " Your flags change the model to claude-sonnet-5-5, and Wisp's prompt changed since this session: the cache is rebuilt"
        ),
        "{notice}"
    );
    assert!(
        notice.ends_with(" and the model's earlier reasoning is dropped."),
        "{notice}"
    );
}

fn with_extra(mut loaded: Parts) -> Parts {
    loaded.parts.extra = Some(Extra::Autonomous);
    loaded
}

#[tokio::test]
async fn an_instructions_flag_on_a_session_with_extra_instructions_keeps_the_extra_alone() {
    use wisp_protocol::settings::{SettingValue, Target, find};
    let extra = Extra::Autonomous.block();
    let env = Env::new("flag-extra");
    fs::write(env.work.join("AGENTS.md"), "project rules\n").unwrap();
    let store = MemoryStore::default();
    let mut live =
        logged(scripted(vec![done("one")]), &env, &store).with_instructions(with_extra(load(&env)));
    turn(&mut live, "first").await;
    let records = records(&store);

    let (written, log) = continued(&records);
    let flags = vec![crate::settings::Given {
        target: Target::Name(find("instructions.source").unwrap()),
        value: SettingValue::Text("none".into()),
        origin: "--instructions".into(),
    }];
    let provider = scripted(vec![done("two")]);
    let mut again = agent(provider.clone(), &env)
        .with_instructions(with_extra(load_in(&env, &InstructionsSource::None)))
        .with_start_layers(crate::settings::host::StartLayers {
            flags,
            ..Default::default()
        })
        .with_log(log, log_context())
        .resume(&records, 0)
        .unwrap();
    let notice = again.resume_notice_text().unwrap().to_owned();
    assert!(
        notice.contains(" Your flags change the project instructions: the cache is rebuilt"),
        "{notice}"
    );
    turn(&mut again, "second").await;
    assert_eq!(
        provider.sent()[0].session.instructions.as_deref(),
        Some(extra.as_str())
    );
    let flagged_system = body_json(&provider.texts()[0])["system"].clone();
    let rewrites = test_guard::records(&written)
        .into_iter()
        .filter(|r| {
            matches!(
                &r.event,
                LogEvent::SessionResumed {
                    instructions: Some(text),
                    ..
                } if *text == extra
            )
        })
        .count();
    assert_eq!(rewrites, 1);

    let records = test_guard::records(&written);
    let (mut third, _, provider) = resumed_in(&env, &records);
    let notice = third.resume_notice_text().unwrap().to_owned();
    assert!(!notice.contains("changed"), "{notice}");
    assert!(!notice.contains("flags"), "{notice}");
    turn(&mut third, "third").await;
    assert_eq!(
        provider.sent()[0].session.instructions.as_deref(),
        Some(extra.as_str())
    );
    assert_eq!(body_json(&provider.texts()[0])["system"], flagged_system);
}

#[tokio::test]
async fn an_unknown_logged_extra_is_kept_without_a_flag_and_rewritten_once_with_one() {
    use wisp_protocol::settings::{SettingValue, Target, find};
    let env = Env::new("unknown-extra");
    fs::write(env.work.join("AGENTS.md"), "project rules\n").unwrap();
    let store = MemoryStore::default();
    let mut live = logged(scripted(vec![done("one")]), &env, &store).with_instructions(load(&env));
    turn(&mut live, "first").await;
    let mut records = records(&store);
    for record in &mut records {
        if let LogEvent::SessionStarted {
            instructions_extra, ..
        } = &mut record.event
        {
            *instructions_extra = Some("weird".into());
        }
    }
    for flagged in [false, true] {
        let (written, log) = continued(&records);
        let flags = if flagged {
            vec![crate::settings::Given {
                target: Target::Name(find("instructions.extra").unwrap()),
                value: SettingValue::Text("autonomous".into()),
                origin: "--instructions-extra".into(),
            }]
        } else {
            Vec::new()
        };
        let loaded = if flagged {
            with_extra(load(&env))
        } else {
            load(&env)
        };
        let mut again = agent(scripted(vec![done("two")]), &env)
            .with_instructions(loaded)
            .with_start_layers(crate::settings::host::StartLayers {
                flags,
                ..Default::default()
            })
            .with_log(log, log_context())
            .resume(&records, 0)
            .unwrap();
        let notice = again.resume_notice_text().unwrap().to_owned();
        assert_eq!(
            notice.contains(
                " Your flags change the extra instructions (autonomous): the cache is rebuilt"
            ),
            flagged,
            "{notice}"
        );
        assert_eq!(
            notice.matches("the cache is rebuilt").count(),
            usize::from(flagged)
        );
        turn(&mut again, "second").await;
        let logged_extra: Vec<Option<String>> = pick!(
            log_events(&written),
            LogEvent::SessionResumed {
                instructions_extra, ..
            } => instructions_extra
        );
        assert_eq!(logged_extra, [flagged.then(|| "autonomous".to_owned())]);
    }
}

async fn extra_resumed(
    env: &Env,
    records: &[wisp_protocol::log::LogRecord],
    name: &str,
) -> (String, Vec<wisp_protocol::log::LogRecord>) {
    use wisp_protocol::settings::{SettingValue, Target, find};
    let (written, log) = continued(records);
    let flags = vec![crate::settings::Given {
        target: Target::Name(find("instructions.extra").unwrap()),
        value: SettingValue::Text(name.into()),
        origin: "--instructions-extra".into(),
    }];
    let loaded = if name == "none" {
        load(env)
    } else {
        with_extra(load(env))
    };
    let mut again = agent(scripted(vec![done("again")]), env)
        .with_instructions(loaded)
        .with_start_layers(crate::settings::host::StartLayers {
            flags,
            ..Default::default()
        })
        .with_log(log, log_context())
        .resume(records, 0)
        .unwrap();
    let notice = again.resume_notice_text().unwrap().to_owned();
    turn(&mut again, "again").await;
    (notice, test_guard::records(&written))
}

#[tokio::test]
async fn extra_instructions_turned_off_then_on_again_rewrite_once_each_time() {
    let env = Env::new("extra-off-on");
    fs::write(env.work.join("AGENTS.md"), "project rules\n").unwrap();
    let store = MemoryStore::default();
    let mut live =
        logged(scripted(vec![done("one")]), &env, &store).with_instructions(with_extra(load(&env)));
    turn(&mut live, "first").await;
    let records = records(&store);
    let (off, records) = extra_resumed(&env, &records, "none").await;
    assert!(
        off.contains(" Your flags change the extra instructions (none): the cache is rebuilt"),
        "{off}"
    );
    let logged = crate::resume::LoggedSession::read(&records).unwrap();
    assert_eq!(logged.instructions_extra.as_deref(), Some("none"));
    let (on, records) = extra_resumed(&env, &records, "autonomous").await;
    assert!(
        on.contains(" Your flags change the extra instructions (autonomous): the cache is rebuilt"),
        "{on}"
    );
    let logged_extra: Vec<Option<String>> = records
        .into_iter()
        .filter_map(|r| match r.event {
            LogEvent::SessionResumed {
                instructions_extra, ..
            } => Some(instructions_extra),
            _ => None,
        })
        .collect();
    assert_eq!(
        logged_extra,
        [Some("none".to_owned()), Some("autonomous".to_owned())]
    );
}

#[tokio::test]
async fn large_instructions_warn_with_their_cost_unless_the_threshold_is_raised() {
    let env = Env::new("large");
    fs::write(env.work.join("AGENTS.md"), "x".repeat(80_000)).unwrap();
    let loaded_diagnostics = |events: &[AgentEvent]| {
        events
            .iter()
            .find_map(|e| match e {
                AgentEvent::InstructionsLoaded { diagnostics, .. } => Some(diagnostics.clone()),
                _ => None,
            })
            .unwrap()
    };
    let provider = scripted(vec![done("one")]);
    let mut warned = agent(provider, &env).with_instructions(load(&env));
    let (_, events) = turn(&mut warned, "first").await;
    let diagnostics = loaded_diagnostics(&events);
    let large = diagnostics.last().unwrap();
    assert_eq!(large.kind, wisp_protocol::DiagnosticKind::Large);
    assert_eq!(large.source, None);
    assert_eq!(
        large.message,
        "instructions are large: about 20,000 tokens, ~$0.004 per request to read and ~$0.16 per cache write (claude-opus-5-5)"
    );
    let mut raised = load(&env);
    raised.parts.warn_tokens = 20_001;
    let provider = scripted(vec![done("one")]);
    let mut quiet = agent(provider, &env).with_instructions(raised);
    let (_, events) = turn(&mut quiet, "first").await;
    assert_eq!(loaded_diagnostics(&events), Vec::new());
}
