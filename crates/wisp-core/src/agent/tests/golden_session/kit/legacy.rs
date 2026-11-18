use wisp_plugin_api::prompt::Prior;
use wisp_protocol::RewindPoint;

use super::*;

const SECTION: &str = "instructions/instructions";
const OPENING: &str = "<instructions-update>\\nThese files changed since they were loaded.";
const SUMMARY: &str = "## Goal and standing instructions\nSay hello.\n\n## Decisions\nNone yet.\n\n## State of the work\nDone: the greeting.\n\n## Files, branches and artefacts\nNone.\n\n## Commands, errors and fixes\nNone.\n\n## Open questions\nNone.";

fn native_off() -> ModelSettings {
    models(&format!(
        "[models.\"{MODEL}\"]\nnative_compaction = false\n"
    ))
}

async fn logged_with(
    case: &str,
    files: &[(&str, &str)],
    script: Vec<Reply>,
    preset: Option<&str>,
) -> Logged {
    let env = Env::new(case);
    for (rel, text) in files {
        let path = env.root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    let server = Server::start(script).await;
    let store = MemoryStore::default();
    let mut kit = kit_for(&server, &env, &store);
    kit.models = native_off();
    let preset = preset.map(|rel| kit::PresetSource {
        preset: "brain".into(),
        path: env.root.join(rel),
    });
    let mut fresh = spec(kit::Start::Fresh);
    fresh.preset.clone_from(&preset);
    let built = kit.build(fresh).await.unwrap();
    let mut live = Live::on(built.agent, &server, Arc::new(step_clock()));
    live.completed("u1", "one").await;
    live.close().await;
    Logged {
        env,
        server,
        store,
        kit,
        preset,
    }
}

struct Logged {
    env: Env,
    server: Server,
    store: MemoryStore,
    kit: Kit,
    preset: Option<kit::PresetSource>,
}

impl Logged {
    fn rewrite(&mut self, change: impl Fn(&mut LogEvent)) {
        let records = records(&self.store);
        let mut writer = MemoryStore::default();
        for mut record in records {
            change(&mut record.event);
            writer.append("s1", &record).unwrap();
        }
        self.store = writer;
    }

    async fn built(&self) -> Agent {
        let (log, recovered, logged, rebuilt) = reopen(&self.store, &self.server);
        let mut resumed = spec_resumed(log, &recovered, logged.as_ref().unwrap(), rebuilt);
        resumed.preset.clone_from(&self.preset);
        self.kit.build(resumed).await.unwrap().agent
    }

    async fn resumed(&self) -> Live {
        Live::on(self.built().await, &self.server, Arc::new(step_clock()))
    }

    fn last_body(&self) -> String {
        self.server.seen().bodies.last().unwrap().clone()
    }
}

fn says_n(n: usize) -> Vec<Reply> {
    (1..=n)
        .map(|i| says(&format!("msg_{i:02}"), 400, "Said."))
        .collect()
}

fn base(agent: &Agent) -> (Prior, Option<Prior>) {
    (
        agent.section_base(SECTION),
        agent.sections.committed().get(SECTION).cloned(),
    )
}

#[tokio::test]
async fn a_legacy_base_holds_through_a_rewind_a_compacted_head_and_a_second_resume() {
    let mut script = says_n(3);
    script.push(reply(
        "msg_s",
        40_000,
        vec![Part::Text(SUMMARY)],
        "end_turn",
    ));
    script.extend(says_n(1));
    let mut logged = logged_with(
        "kit-legacy-paths",
        &[("work/AGENTS.md", "Answer briefly.\n")],
        script,
        None,
    )
    .await;
    let block = logged_block(&logged.store);
    logged.rewrite(|event| {
        if let LogEvent::SessionStarted { sections, .. } = event {
            sections.clear();
        }
    });
    let legacy = Prior::Legacy(Some(block.clone()));
    let mut live = logged.resumed().await;
    live.completed("u2", "two").await;
    assert!(!logged.last_body().contains(OPENING), "no spurious delta");
    fs::write(logged.env.work().join("AGENTS.md"), "Answer at length.\n").unwrap();
    live.completed("u3", "three").await;
    let edited = logged.last_body();
    assert_eq!(edited.matches(OPENING).count(), 1, "{edited}");
    assert!(edited.contains("Answer at length.\\n</file>"), "{edited}");
    live.send(ClientCommand::Rewind {
        to: RewindPoint::Turn { turn: 3 },
        request: request(1),
    })
    .await;
    let done = live
        .until(|e| matches!(e, AgentEvent::Rewound { .. } | AgentEvent::Rejected { .. }))
        .await;
    assert!(matches!(done, AgentEvent::Rewound { .. }), "{done:?}");
    let (agent, _) = live.close().await;
    assert_eq!(base(&agent), (legacy.clone(), Some(legacy.clone())));

    let mut again = logged.resumed().await;
    let (agent, _) = {
        again.compacted(2).await;
        again.completed("u4", "four").await;
        again.close().await
    };
    let head = logged.last_body();
    assert_eq!(head.matches(OPENING).count(), 1, "{head}");
    assert!(head.contains("Answer at length.\\n</file>"), "{head}");
    assert!(!head.contains("Answer briefly.\\n</file>\\n</instructions-update>"));
    assert_eq!(agent.section_base(SECTION), legacy);
    assert!(matches!(
        agent.sections.committed().get(SECTION),
        Some(Prior::Snapshot(_))
    ));
    let second = logged.resumed().await;
    let (agent, _) = second.close().await;
    assert_eq!(agent.section_base(SECTION), legacy, "a second resume");
}

#[tokio::test]
async fn a_default_only_session_with_two_field_snapshots_resumes_with_no_delta() {
    let mut logged = logged_with(
        "kit-two-field",
        &[
            ("config/AGENTS.md", "Prefer British spelling.\n@more.md\n"),
            ("config/more.md", "More.\n"),
        ],
        says_n(2),
        None,
    )
    .await;
    let old = format!(
        "{} AGENTS.md\n{} more.md\n",
        wisp_instructions::sha256_hex("Prefer British spelling.\n@more.md\n"),
        wisp_instructions::sha256_hex("More.\n")
    );
    let mut seen = false;
    logged.rewrite(|event| {
        if let LogEvent::SessionStarted { sections, .. } = event {
            for section in sections.iter_mut().filter(|s| s.name == SECTION) {
                section.snapshot.clone_from(&old);
            }
        }
    });
    for record in records(&logged.store) {
        if let LogEvent::SessionStarted { sections, .. } = &record.event {
            seen = sections.iter().any(|s| s.snapshot == old);
        }
    }
    assert!(seen);
    let mut live = logged.resumed().await;
    live.completed("u2", "two").await;
    let (agent, _) = live.close().await;
    assert!(
        !logged.last_body().contains(OPENING),
        "{}",
        logged.last_body()
    );
    assert_eq!(
        agent.section_base(SECTION),
        Prior::Snapshot(format!(
            "{} default AGENTS.md\n{} default more.md\n",
            wisp_instructions::sha256_hex("Prefer British spelling.\n@more.md\n"),
            wisp_instructions::sha256_hex("More.\n")
        ))
    );
}

const BRAIN: &str = "config/BRAIN.md";

fn preset_update(text: &str) -> String {
    let update = format!(
        "<instructions-update>\nThese files changed since they were loaded. Each version below replaces the earlier one.\n<file scope=\"preset\" name=\"BRAIN.md\" path=\"~/config/BRAIN.md\">\n{text}</file>\n</instructions-update>\n"
    );
    let quoted = serde_json::to_string(&update).unwrap();
    quoted[1..quoted.len() - 1].to_owned()
}

fn system_of(body: &str) -> Value {
    serde_json::from_str::<Value>(body).unwrap()["system"].clone()
}

#[tokio::test]
async fn a_resumed_main_agent_gets_its_edited_preset_once_and_again_in_a_head() {
    let mut script = says_n(4);
    script.push(reply(
        "msg_s",
        40_000,
        vec![Part::Text(SUMMARY)],
        "end_turn",
    ));
    script.extend(says_n(1));
    let files = [
        ("work/AGENTS.md", "Answer briefly.\n"),
        (BRAIN, "Teach gently.\n"),
    ];
    let logged = logged_with("kit-preset-edit", &files, script, Some(BRAIN)).await;
    assert!(session_started(&logged.store).contains("\"name\":\"core/preset\""));
    let system = system_of(&logged.last_body());
    assert!(!logged.last_body().contains(OPENING));
    let mut live = logged.resumed().await;
    live.completed("u2", "two").await;
    live.close().await;
    assert!(
        !logged.last_body().contains(OPENING),
        "{}",
        logged.last_body()
    );
    fs::write(logged.env.root.join(BRAIN), "Teach firmly.\n").unwrap();
    let mut live = logged.resumed().await;
    live.completed("u3", "three").await;
    let edited = logged.last_body();
    assert_eq!(edited.matches(OPENING).count(), 1, "{edited}");
    let at = edited.find(&preset_update("Teach firmly.\n")).unwrap();
    assert!(at < edited.rfind("Sent: ").unwrap(), "{edited}");
    live.completed("u4", "four").await;
    assert_eq!(logged.last_body().matches(OPENING).count(), 1);
    live.compacted(1).await;
    live.completed("u5", "five").await;
    let (_, seen) = live.close().await;
    let head = logged.last_body();
    assert_eq!(head.matches(OPENING).count(), 1, "{head}");
    assert_eq!(head.matches(&preset_update("Teach firmly.\n")).count(), 1);
    for body in logged.server.seen().bodies {
        assert_eq!(system_of(&body), system, "the system prompt changed");
    }
    assert_eq!(notes(&seen), [vec!["~/config/BRAIN.md".to_owned()]]);
}

fn notes(seen: &[AgentEvent]) -> Vec<Vec<String>> {
    seen.iter()
        .filter_map(|e| match e {
            AgentEvent::InstructionsChanged { changed, .. } if !changed.is_empty() => {
                Some(changed.iter().map(|c| c.display_path.clone()).collect())
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_retry_resends_the_preset_update_after_the_plugins() {
    let files = [
        ("work/AGENTS.md", "Answer briefly.\n"),
        (BRAIN, "Teach gently.\n"),
    ];
    let mut script = says_n(1);
    script.push(server_error());
    script.extend(says_n(2));
    let logged = logged_with("kit-preset-retry", &files, script, Some(BRAIN)).await;
    fs::write(logged.env.root.join(BRAIN), "Teach firmly.\n").unwrap();
    fs::write(logged.env.work().join("AGENTS.md"), "Answer at length.\n").unwrap();
    let agent = logged
        .built()
        .await
        .with_retry_policy(crate::agent::retry::RetryPolicy {
            step_retries: Some(0),
            ..crate::agent::retry::RetryPolicy::default()
        });
    let mut live = Live::on(agent, &logged.server, Arc::new(step_clock()));
    let failed = live.turn("u2", "two").await;
    assert!(
        matches!(failed, TurnOutcome::Error { offer: Some(_), .. }),
        "{failed:?}"
    );
    let sent = logged.last_body();
    let update = preset_update("Teach firmly.\n");
    let preset = sent.find(&update).unwrap();
    assert!(sent.find("Answer at length.").unwrap() < preset, "{sent}");
    assert!(preset < sent.rfind("Sent: ").unwrap(), "{sent}");
    live.send(ClientCommand::Retry {
        turn: 2,
        endpoint: None,
        request: request(1),
    })
    .await;
    assert!(completed(&live.ended().await));
    assert_eq!(logged.last_body().matches(&update).count(), 1);
    live.completed("u3", "three").await;
    assert_eq!(logged.last_body().matches(&update).count(), 1);
    let (_, seen) = live.close().await;
    assert_eq!(notes(&seen).len(), 2, "{:#?}", notes(&seen));
}

#[tokio::test]
async fn a_preset_logged_before_its_snapshot_sends_an_update_only_once_edited() {
    let files = [(BRAIN, "Teach gently.\n")];
    let mut logged = logged_with("kit-preset-legacy", &files, says_n(3), Some(BRAIN)).await;
    logged.rewrite(|event| {
        if let LogEvent::SessionStarted { sections, .. } = event {
            sections.retain(|s| s.name != "core/preset");
        }
    });
    assert!(!session_started(&logged.store).contains("core/preset"));
    let mut live = logged.resumed().await;
    live.completed("u2", "two").await;
    live.close().await;
    assert!(
        !logged.last_body().contains(OPENING),
        "{}",
        logged.last_body()
    );
    fs::write(logged.env.root.join(BRAIN), "Teach firmly.\n").unwrap();
    let mut live = logged.resumed().await;
    live.completed("u3", "three").await;
    live.close().await;
    let edited = logged.last_body();
    assert_eq!(edited.matches(OPENING).count(), 1, "{edited}");
    assert_eq!(edited.matches(&preset_update("Teach firmly.\n")).count(), 1);
}
