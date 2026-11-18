use futures::FutureExt;
use futures::future::BoxFuture;
use tokio::sync::Semaphore;
use wisp_protocol::settings::{
    FileState, Layer, SavedEntry, SettingChange, SettingName, SettingTarget, SettingValue,
    SettingsSnapshot, site_path,
};
use wisp_protocol::{DeliveryMode, InputId, RequestId, WithdrawnBy};

use super::*;
use crate::config_file::SiteEdit;
use crate::secrets::LOCK_WAIT;
use crate::settings::host::{HostError, SavedFile, SavedRead, SettingsHost, WrittenSite};
use crate::test_guard::until_event;

const SHOWN: &str = "~/.config/wisp/config.toml";
const SONNET: &str = "claude-sonnet-5-5";
const QUIET: Duration = Duration::from_millis(300);

struct Host {
    gate: Semaphore,
    fail: bool,
    saves: Mutex<Vec<Vec<SiteEdit>>>,
    used: Mutex<Vec<String>>,
}

impl Host {
    fn new(fail: bool) -> Arc<Self> {
        Arc::new(Self {
            gate: Semaphore::new(0),
            fail,
            saves: Mutex::new(Vec::new()),
            used: Mutex::new(Vec::new()),
        })
    }

    fn open(&self) {
        self.gate.add_permits(Semaphore::MAX_PERMITS / 2);
    }

    fn saves(&self) -> Vec<Vec<SiteEdit>> {
        self.saves.lock().unwrap().clone()
    }
}

impl SettingsHost for Host {
    fn used(&self, names: Vec<String>) -> BoxFuture<'_, ()> {
        async move {
            self.used.lock().unwrap().extend(names);
        }
        .boxed()
    }

    fn save(&self, edits: Vec<SiteEdit>) -> BoxFuture<'_, Result<SavedFile, HostError>> {
        async move {
            drop(self.gate.acquire().await);
            self.saves.lock().unwrap().push(edits.clone());
            if self.fail {
                return Err(HostError("config.toml is locked by another Wisp".into()));
            }
            Ok(SavedFile {
                path_display: SHOWN.into(),
                written: edits
                    .iter()
                    .map(|edit| WrittenSite {
                        path: edit.path().clone(),
                        value: match edit {
                            SiteEdit::Set(_, value) => Some(value.clone()),
                            SiteEdit::Unset(_) => None,
                        },
                        line: Some(1),
                    })
                    .collect(),
                notes: Vec::new(),
            })
        }
        .boxed()
    }

    fn read(&self) -> BoxFuture<'_, Result<SavedRead, HostError>> {
        async move {
            Ok(SavedRead {
                often: self.used.lock().unwrap().clone(),
                saved: vec![wisp_protocol::settings::SavedValue::of(
                    site_path("effort").unwrap(),
                    SettingValue::Text("high".into()),
                    Some(1),
                )],
                file: FileState::Ok {
                    path_display: SHOWN.into(),
                },
            })
        }
        .boxed()
    }
}

fn saved(request: u64, name: &str, value: &str) -> ClientCommand {
    ClientCommand::ChangeSettings {
        request: RequestId(request),
        changes: vec![SettingChange::named(name, SettingValue::Text(value.into())).unwrap()],
        save: true,
    }
}

fn effort(request: u64, level: Effort) -> ClientCommand {
    ClientCommand::shortcut(RequestId(request), None, None, Some(level))
}

struct Live {
    commands: Option<mpsc::Sender<ClientCommand>>,
    events: mpsc::Receiver<AgentEvent>,
    task: AgentTask,
    _env: Env,
}

impl Live {
    fn start(provider: Arc<Fake>, host: Option<Arc<Host>>) -> Self {
        Self::with(provider, host, |agent| agent)
    }

    fn with(
        provider: Arc<Fake>,
        host: Option<Arc<Host>>,
        shape: impl FnOnce(Agent) -> Agent,
    ) -> Self {
        let env = Env::new("settings-queue");
        let mut agent = shape(super::agent(provider, &env));
        if let Some(host) = host {
            agent = agent.with_settings_host(host);
        }
        let (commands, events, task) = spawn(agent);
        Self {
            commands: Some(commands),
            events,
            task,
            _env: env,
        }
    }

    async fn send(&self, command: ClientCommand) {
        if let Some(commands) = &self.commands {
            commands.send(command).await.unwrap();
        }
    }

    async fn quiet(&mut self) -> Vec<AgentEvent> {
        let mut seen = Vec::new();
        while let Ok(Some(event)) = tokio::time::timeout(QUIET, self.events.recv()).await {
            seen.push(event);
        }
        seen
    }

    async fn until(&mut self, stop: impl Fn(&AgentEvent) -> bool) -> Vec<AgentEvent> {
        let mut seen = Vec::new();
        until_event(&mut self.events, &mut seen, "the event", stop).await;
        seen
    }

    async fn end(&mut self) -> Vec<AgentEvent> {
        self.commands = None;
        let mut seen = Vec::new();
        while let Some(event) = self.events.recv().await {
            seen.push(event);
        }
        seen
    }
}

fn answers(events: &[AgentEvent]) -> Vec<(u64, bool)> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::SettingsChanged {
                answers: Some(request),
                ..
            } => Some((request.0, true)),
            AgentEvent::Rejected {
                request: Some(request),
                ..
            } => Some((request.0, false)),
            AgentEvent::SettingsSnapshot(snapshot) => Some((snapshot.answers.0, true)),
            _ => None,
        })
        .collect()
}

fn reasons(events: &[AgentEvent]) -> Vec<String> {
    pick!(
        events,
        AgentEvent::Rejected {
            reason,
            request: Some(_),
            ..
        } => reason.clone()
    )
}

fn turn_started(events: &[AgentEvent]) -> bool {
    events
        .iter()
        .any(|e| matches!(e, AgentEvent::TurnStarted { .. }))
}

#[tokio::test]
async fn a_save_goes_to_the_host_and_every_answer_keeps_command_order() {
    let host = Host::new(false);
    let mut live = Live::start(scripted(Vec::new()), Some(host.clone()));
    live.send(saved(1, "model", SONNET)).await;
    live.send(effort(2, Effort::Low)).await;
    assert!(answers(&live.quiet().await).is_empty());
    assert_eq!(*live.task.worst.borrow(), LOCK_WAIT);
    assert_eq!(live.task.limit(), LOCK_WAIT + QUIT_MARGIN);
    host.open();
    let events = live
        .until(|e| answers(std::slice::from_ref(e)) == [(2, true)])
        .await;
    assert_eq!(answers(&events), [(1, true), (2, true)]);
    let AgentEvent::SettingsChanged { model, changes, .. } = &events[0] else {
        panic!("{events:?}");
    };
    assert_eq!(model, SONNET);
    let saved = changes[0].saved.as_ref().unwrap();
    assert_eq!(saved.path_display, SHOWN);
    assert_eq!(saved.line, Some(1));
    assert_eq!(changes[0].layer, Layer::Session);
    assert_eq!(
        host.saves(),
        [vec![SiteEdit::Set(
            site_path("model").unwrap(),
            SettingValue::Text(SONNET.into())
        )]]
    );
    assert_eq!(*live.task.worst.borrow(), Duration::ZERO);
    live.end().await;
}

#[tokio::test]
async fn a_failed_save_is_refused_before_the_next_command_is_judged() {
    let host = Host::new(true);
    let mut live = Live::start(scripted(Vec::new()), Some(host.clone()));
    live.send(saved(1, "model", SONNET)).await;
    live.send(effort(2, Effort::Low)).await;
    host.open();
    let events = live
        .until(|e| answers(std::slice::from_ref(e)) == [(2, true)])
        .await;
    assert_eq!(answers(&events), [(1, false), (2, true)]);
    assert_eq!(reasons(&events), ["config.toml is locked by another Wisp"]);
    let AgentEvent::SettingsChanged { model, .. } = events.last().unwrap() else {
        panic!("{events:?}");
    };
    assert_eq!(model, DEFAULT_MODEL);
    live.end().await;
}

#[tokio::test]
async fn a_message_sent_during_a_save_waits_and_runs_on_the_saved_model() {
    let host = Host::new(false);
    let provider = scripted(vec![done("hi")]);
    let mut live = Live::start(provider.clone(), Some(host.clone()));
    live.send(saved(1, "model", SONNET)).await;
    live.send(send("tui-1", "hello")).await;
    let waiting = live.quiet().await;
    assert!(!turn_started(&waiting), "{waiting:?}");
    assert!(waiting.iter().any(|e| matches!(
        e,
        AgentEvent::InputAccepted { id, front: true, .. } if id.as_str() == "tui-1"
    )));
    host.open();
    let events = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let changed = events
        .iter()
        .position(|e| matches!(e, AgentEvent::SettingsChanged { .. }))
        .unwrap();
    let started = events
        .iter()
        .position(|e| matches!(e, AgentEvent::TurnStarted { .. }))
        .unwrap();
    assert!(changed < started);
    assert_eq!(provider.sent()[0].session.model, SONNET);
    live.end().await;
}

#[tokio::test]
async fn cancel_while_waiting_withdraws_steers_and_leaves_settings_alone() {
    let host = Host::new(false);
    let mut live = Live::start(scripted(Vec::new()), Some(host.clone()));
    live.send(saved(1, "effort", "high")).await;
    live.send(send("tui-1", "hello")).await;
    live.quiet().await;
    live.send(ClientCommand::Cancel).await;
    let events = live.quiet().await;
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::InputWithdrawn { ids, by: WithdrawnBy::TurnStopped } if ids == &[InputId("tui-1".into())]
    )));
    host.open();
    let events = live.quiet().await;
    assert_eq!(answers(&events), [(1, true)]);
    assert!(!turn_started(&events));
    live.end().await;
}

#[tokio::test]
async fn the_session_answers_every_queued_command_before_it_ends() {
    let host = Host::new(false);
    let mut live = Live::start(scripted(Vec::new()), Some(host.clone()));
    live.send(saved(1, "effort", "high")).await;
    live.send(ClientCommand::DescribeSettings {
        request: RequestId(2),
    })
    .await;
    live.commands = None;
    tokio::time::sleep(QUIET).await;
    host.open();
    let events = live.end().await;
    assert_eq!(answers(&events), [(1, true), (2, true)]);
}

#[tokio::test]
async fn describe_settings_answers_with_the_layers_and_the_file() {
    let host = Host::new(false);
    let mut live = Live::start(scripted(Vec::new()), Some(host.clone()));
    live.send(effort(1, Effort::Low)).await;
    live.until(|e| matches!(e, AgentEvent::SettingsChanged { .. }))
        .await;
    live.send(ClientCommand::SettingsUsed {
        names: vec!["time".into()],
    })
    .await;
    assert!(live.quiet().await.is_empty());
    assert_eq!(*host.used.lock().unwrap(), ["time"]);
    live.send(ClientCommand::DescribeSettings {
        request: RequestId(2),
    })
    .await;
    let events = live
        .until(|e| matches!(e, AgentEvent::SettingsSnapshot(_)))
        .await;
    let Some(AgentEvent::SettingsSnapshot(snapshot)) = events.last() else {
        panic!("{events:?}");
    };
    let SettingsSnapshot {
        answers,
        context,
        settings,
        saved,
        file,
        ..
    } = snapshot;
    assert_eq!(*answers, RequestId(2));
    assert_eq!(snapshot.often, ["time"]);
    assert_eq!(context.model, DEFAULT_MODEL);
    let row = |name: &str| {
        settings
            .iter()
            .find(|s| s.name == SettingName::parse(name).unwrap())
            .unwrap()
    };
    assert_eq!(row("effort").value, Some(SettingValue::Text("low".into())));
    assert_eq!(row("effort").layer, Layer::Session);
    assert_eq!(row("effort").shadowed[0].layer, Layer::Flag);
    assert_eq!(
        row("model").value,
        Some(SettingValue::Text(DEFAULT_MODEL.into()))
    );
    assert!(settings.iter().all(|s| s.name.as_str() != "key"));
    let names: Vec<&str> = settings.iter().map(|s| s.name.as_str()).collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted);
    assert_eq!(
        saved[0].entry,
        SavedEntry::Value {
            value: SettingValue::Text("high".into())
        }
    );
    assert_eq!(
        *file,
        FileState::Ok {
            path_display: SHOWN.into()
        }
    );
    live.end().await;
}

async fn search_reason(config: &str, search: bool) -> Option<String> {
    let host = Host::new(false);
    let mut live = Live::with(scripted(Vec::new()), Some(host), |agent| {
        agent
            .with_catalog(crate::tool::testkit::catalog())
            .with_model_settings(crate::test_guard::models(config))
            .with_web_search(search)
    });
    live.send(ClientCommand::DescribeSettings {
        request: RequestId(1),
    })
    .await;
    let events = live
        .until(|e| matches!(e, AgentEvent::SettingsSnapshot(_)))
        .await;
    let Some(AgentEvent::SettingsSnapshot(snapshot)) = events.last() else {
        panic!("{events:?}");
    };
    let reason = snapshot
        .plugins
        .iter()
        .flat_map(|p| &p.tools)
        .find(|t| t.name == "web_search")
        .and_then(|t| t.unavailable.clone());
    live.end().await;
    reason
}

#[tokio::test]
async fn the_snapshots_catalog_says_why_the_next_pair_has_no_search_and_nothing_where_it_is_sent() {
    let by_model = format!("[models.\"{DEFAULT_MODEL}\"]\nweb_search = []\n");
    let by_account = "[endpoints.env]\nnative_web_search = false\n";
    assert_eq!(search_reason("", true).await, None);
    assert_eq!(search_reason("", false).await, None);
    assert_eq!(
        search_reason(&by_model, true).await.as_deref(),
        Some("not on this model")
    );
    assert_eq!(
        search_reason(by_account, true).await.as_deref(),
        Some("off for this account")
    );
    assert_eq!(search_reason(by_account, false).await, None);
}

#[tokio::test]
async fn settings_the_kernel_cant_apply_are_refused_by_name() {
    let mut live = Live::start(scripted(Vec::new()), None);
    let tools = SettingChange::named("agents.max_agents", SettingValue::Int(10)).unwrap();
    let time = SettingChange {
        target: SettingTarget::Path(wisp_protocol::settings::SitePath::new(
            wisp_protocol::settings::Place::Agent("reviewer".into()),
            wisp_protocol::settings::find("time").unwrap(),
        )),
        value: Some(SettingValue::Text("utc".into())),
    };
    live.send(ClientCommand::ChangeSettings {
        request: RequestId(1),
        changes: vec![tools],
        save: false,
    })
    .await;
    live.send(ClientCommand::ChangeSettings {
        request: RequestId(2),
        changes: vec![time],
        save: true,
    })
    .await;
    live.send(saved(3, "effort", "high")).await;
    live.send(ClientCommand::ChangeSettings {
        request: RequestId(5),
        changes: vec![
            SettingChange::named("voice.voice", SettingValue::Text("cove".into())).unwrap(),
        ],
        save: false,
    })
    .await;
    live.send(ClientCommand::ChangeSettings {
        request: RequestId(4),
        changes: vec![SettingChange {
            target: SettingTarget::Path(site_path("effort").unwrap()),
            value: Some(SettingValue::Text("high".into())),
        }],
        save: false,
    })
    .await;
    let events = live.quiet().await;
    assert_eq!(
        answers(&events),
        [(1, false), (2, false), (3, false), (5, false), (4, false)]
    );
    assert_eq!(
        reasons(&events),
        [
            "agents.max_agents applies to new chats, so it can only be saved.",
            "time can't be changed in a running session yet; use wisp config set time",
            NO_HOST,
            "voice.voice applies from the next call, so it can only be saved.",
            "effort names a saved place, so it can only be saved.",
        ]
    );
    live.end().await;
}

#[tokio::test]
async fn a_change_through_the_queue_appends_and_never_edits_the_prefix() {
    let host = Host::new(false);
    host.open();
    let provider = scripted(vec![done("one"), done("two")]);
    let mut live = Live::start(provider.clone(), Some(host));
    live.send(send("tui-1", "first")).await;
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    live.send(saved(1, "effort", "high")).await;
    live.send(send("tui-2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let requests = provider.sent();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].session.system, requests[1].session.system);
    assert_eq!(
        serde_json::to_string(&requests[0].tools).unwrap(),
        serde_json::to_string(&requests[1].tools).unwrap()
    );
    let first = serde_json::to_value(&requests[0].messages).unwrap();
    let second = serde_json::to_value(&requests[1].messages).unwrap();
    let first = first.as_array().unwrap();
    assert_eq!(first[..], second.as_array().unwrap()[..first.len()]);
    live.end().await;
}

fn queued(id: &str, text: &str) -> ClientCommand {
    ClientCommand::Send {
        id: InputId(id.into()),
        text: text.into(),
        images: Vec::new(),
        mode: DeliveryMode::Queue,
        turn: None,
        replaces: None,
    }
}

const PER_TURN: [&str; 3] = ["Sent: ", "Context: ", "<environment>"];

fn per_turn(text: &str) -> bool {
    PER_TURN.iter().any(|prefix| text.starts_with(prefix))
}

fn last_user_text(request: &Sent) -> Vec<String> {
    let last = request
        .messages
        .iter()
        .rev()
        .find(|m| m.role == Role::User)
        .unwrap();
    last.content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } if !per_turn(text) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn held_messages_unhold_into_the_front_and_start_before_the_new_one() {
    let host = Host::new(false);
    let provider = scripted(vec![hanging(&[B::Text("thinking")]), done("ok")]);
    let mut live = Live::start(provider.clone(), Some(host.clone()));
    live.send(send("tui-1", "first")).await;
    live.until(|e| matches!(e, AgentEvent::TurnStarted { .. }))
        .await;
    live.send(queued("tui-2", "queued")).await;
    live.until(|e| matches!(e, AgentEvent::InputAccepted { .. }))
        .await;
    live.send(ClientCommand::Cancel).await;
    let stopped = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(stopped.iter().any(|e| matches!(
        e,
        AgentEvent::InputHeld { ids, .. } if ids == &[InputId("tui-2".into())]
    )));
    live.send(saved(1, "effort", "high")).await;
    live.send(send("tui-3", "new")).await;
    let waiting = live.quiet().await;
    let fronts: Vec<(String, bool)> = waiting
        .iter()
        .filter_map(|e| match e {
            AgentEvent::InputUnheld { ids, front } => Some((ids[0].as_str().to_owned(), *front)),
            AgentEvent::InputAccepted { id, front, .. } => Some((id.as_str().to_owned(), *front)),
            _ => None,
        })
        .collect();
    assert_eq!(
        fronts,
        [("tui-2".to_owned(), true), ("tui-3".to_owned(), true)]
    );
    host.open();
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let requests = provider.sent();
    assert_eq!(last_user_text(&requests[1]), ["queued", "new"]);
    live.end().await;
}

#[tokio::test]
async fn a_second_send_while_waiting_joins_behind_the_front() {
    let host = Host::new(false);
    let provider = scripted(vec![done("ok")]);
    let mut live = Live::start(provider.clone(), Some(host.clone()));
    live.send(saved(1, "effort", "high")).await;
    live.send(queued("tui-1", "queued first")).await;
    live.send(send("tui-2", "steer second")).await;
    let waiting = live.quiet().await;
    let fronts: Vec<bool> = pick!(&waiting, AgentEvent::InputAccepted { front, .. } => *front);
    assert_eq!(fronts, [true, false]);
    host.open();
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert_eq!(
        last_user_text(&provider.sent()[0]),
        ["queued first", "steer second"]
    );
    live.end().await;
}

fn changed_before_end(events: &[AgentEvent]) -> bool {
    let changed = events
        .iter()
        .position(|e| matches!(e, AgentEvent::SettingsChanged { .. }));
    let ended = events
        .iter()
        .position(|e| matches!(e, AgentEvent::TurnEnded { .. }));
    match (changed, ended) {
        (Some(changed), Some(ended)) => changed < ended,
        (Some(_), None) => true,
        _ => false,
    }
}

async fn save_during(live: &mut Live, host: &Host) {
    live.send(saved(1, "effort", "high")).await;
    live.quiet().await;
    host.open();
    let events = live.quiet().await;
    assert!(changed_before_end(&events), "{events:?}");
    assert_eq!(answers(&events), [(1, true)]);
    live.send(ClientCommand::Cancel).await;
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    live.end().await;
}

#[tokio::test]
async fn a_save_is_answered_while_the_reply_streams() {
    let host = Host::new(false);
    let provider = scripted(vec![hanging(&[B::Text("streaming")])]);
    let mut live = Live::start(provider, Some(host.clone()));
    live.send(send("tui-1", "go")).await;
    live.until(|e| matches!(e, AgentEvent::TextDelta { .. }))
        .await;
    save_during(&mut live, &host).await;
}

#[tokio::test]
async fn a_save_is_answered_while_a_tool_runs() {
    let host = Host::new(false);
    let provider = scripted(vec![uses(&[call(
        "a",
        "sleep",
        json!({"ms": 60_000, "text": "working\n"}),
    )])]);
    let mut live = Live::start(provider, Some(host.clone()));
    live.send(send("tui-1", "go")).await;
    live.until(|e| matches!(e, AgentEvent::ToolCallStarted { .. }))
        .await;
    save_during(&mut live, &host).await;
}

#[tokio::test]
async fn a_save_is_answered_while_a_retry_waits() {
    let host = Host::new(false);
    let provider = scripted(vec![turned_away(), done("ok")]);
    let mut live = Live::with(provider, Some(host.clone()), |agent| {
        agent.with_retry_policy(crate::agent::retry::RetryPolicy {
            base_delay: Duration::from_secs(60),
            jitter: || 0.0,
            ..crate::agent::retry::RetryPolicy::default()
        })
    });
    live.send(send("tui-1", "go")).await;
    live.until(|e| matches!(e, AgentEvent::Retrying { .. }))
        .await;
    save_during(&mut live, &host).await;
}

#[tokio::test]
async fn inputs_due_at_turn_end_join_the_front_while_a_save_waits() {
    let host = Host::new(false);
    let provider = scripted(vec![
        uses(&[call("a", "sleep", json!({"ms": 600, "text": "working\n"}))]),
        done("one"),
        done("two"),
    ]);
    let mut live = Live::start(provider.clone(), Some(host.clone()));
    live.send(send("tui-1", "first")).await;
    live.until(|e| matches!(e, AgentEvent::ToolCallStarted { .. }))
        .await;
    live.send(saved(1, "effort", "high")).await;
    live.send(queued("tui-2", "queued")).await;
    let ended = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(answers(&ended).is_empty(), "{ended:?}");
    live.send(send("tui-3", "new")).await;
    let waiting = live.quiet().await;
    assert!(!turn_started(&waiting), "{waiting:?}");
    host.open();
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let requests = provider.sent();
    assert_eq!(requests.len(), 3);
    assert_eq!(last_user_text(&requests[2]), ["queued", STEER_NOTE, "new"]);
    live.end().await;
}

#[tokio::test]
async fn changing_one_setting_twice_in_one_request_is_refused() {
    let mut live = Live::start(scripted(Vec::new()), None);
    live.send(ClientCommand::ChangeSettings {
        request: RequestId(1),
        changes: vec![
            SettingChange::named("effort", SettingValue::Text("low".into())).unwrap(),
            SettingChange::named("effort", SettingValue::Text("high".into())).unwrap(),
        ],
        save: false,
    })
    .await;
    let events = live.quiet().await;
    assert_eq!(answers(&events), [(1, false)]);
    assert_eq!(
        reasons(&events),
        ["effort appears twice in one change; send it once"]
    );
    live.end().await;
}

#[tokio::test]
async fn the_agent_task_joins_once_the_queue_settles() {
    let host = Host::new(false);
    let mut live = Live::start(scripted(Vec::new()), Some(host.clone()));
    live.send(saved(1, "effort", "high")).await;
    live.quiet().await;
    let Live {
        commands,
        mut events,
        task,
        _env,
    } = live;
    drop(commands);
    let drain = tokio::spawn(async move { while events.recv().await.is_some() {} });
    assert!(task.save_waiting());
    let opener = host.clone();
    tokio::spawn(async move {
        tokio::time::sleep(QUIET).await;
        opener.open();
    });
    assert!(task.join().await);
    drain.await.unwrap();
    assert_eq!(host.saves().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn the_agent_task_gives_up_at_its_bound() {
    let host = Host::new(false);
    let live = Live::start(scripted(Vec::new()), Some(host.clone()));
    live.send(saved(1, "effort", "high")).await;
    tokio::time::sleep(QUIET).await;
    let Live {
        commands,
        mut events,
        task,
        _env,
    } = live;
    drop(commands);
    tokio::spawn(async move { while events.recv().await.is_some() {} });
    let limit = task.limit();
    assert_eq!(limit, LOCK_WAIT + QUIT_MARGIN);
    let started = tokio::time::Instant::now();
    let joined = tokio::time::timeout(limit * 2, task.join()).await;
    assert_eq!(joined, Ok(false));
    assert!(started.elapsed() >= limit);
    assert!(host.saves().is_empty());
}

mod endpoint;
mod kernel;
mod plugin;
mod rewind;
mod wakes;
