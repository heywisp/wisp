use wisp_plugin_api::hooks::Reply;
use wisp_plugin_testkit::hooks::Calls;
use wisp_plugin_todos::NAME;
use wisp_protocol::{RequestId, RewindPoint, TodoItem, TodoStatus};

use super::*;
use crate::agent::testing::todo_item as item;
use crate::plugin::testing::{hook_body, hook_source, replying, with_test_hooks};

fn checklisting(provider: Arc<Fake>, env: &Env) -> Agent {
    Agent::new(provider, default_session())
        .with_clock(TickingClock::new())
        .with_tools(crate::tool::testkit::bundled(&[NAME]), env.workspace())
        .with_platform("Linux")
        .with_resources(None)
        .with_cancel_grace(Duration::from_millis(300))
}

fn prefix(recorded: &Sent) -> (String, String) {
    let body: Value = recorded.body();
    (body["system"].to_string(), body["tools"].to_string())
}

fn assert_one_prefix(requests: &[Sent]) {
    let first = prefix(&requests[0]);
    assert!(first.1.contains(NAME), "{}", first.1);
    for recorded in requests {
        assert_eq!(prefix(recorded), first);
    }
}

fn resume_from(env: &Env, records: &[wisp_protocol::log::LogRecord]) -> Agent {
    resume_with(env, records, scripted(vec![]), &[NAME])
}

fn resume_with(
    env: &Env,
    records: &[wisp_protocol::log::LogRecord],
    provider: Arc<Fake>,
    tools: &[&str],
) -> Agent {
    resume_into(env, records, provider, crate::tool::testkit::bundled(tools))
}

fn resume_into(
    env: &Env,
    records: &[wisp_protocol::log::LogRecord],
    provider: Arc<Fake>,
    toolbox: Toolbox,
) -> Agent {
    let (_, log) = continued(records);
    Agent::new(provider, default_session())
        .with_clock(TickingClock::new())
        .with_tools(toolbox, env.workspace())
        .with_platform("Linux")
        .with_resources(None)
        .with_cancel_grace(Duration::from_millis(300))
        .with_log(log, log_context())
        .resume(records, 0)
        .unwrap()
}

fn first_list() -> Vec<TodoItem> {
    vec![
        item("Read the plan", TodoStatus::InProgress),
        item("Write the code", TodoStatus::Pending),
    ]
}

fn second_list() -> Vec<TodoItem> {
    vec![
        TodoItem {
            section: Some("Done".to_owned()),
            ..item("Read the plan", TodoStatus::Completed)
        },
        TodoItem {
            section: Some("Next".to_owned()),
            ..item("Write the code", TodoStatus::InProgress)
        },
    ]
}

fn input(items: &[TodoItem]) -> Value {
    let todos: Vec<Value> = items
        .iter()
        .map(|i| {
            let mut value = json!({"text": i.text, "status": i.status.as_str()});
            if let Some(section) = &i.section {
                value["section"] = json!(section);
            }
            value
        })
        .collect();
    json!({ "todos": todos })
}

fn updates(events: &[AgentEvent]) -> Vec<(Option<String>, Vec<TodoItem>)> {
    pick!(events, AgentEvent::TodosUpdated { call_id, items } => (call_id.clone(), items.clone()))
}

fn two_turns() -> Arc<Fake> {
    scripted(vec![
        uses(&[call("t1", NAME, input(&first_list()))]),
        done("planned"),
        uses(&[call("t2", NAME, input(&second_list()))]),
        done("moved on"),
    ])
}

#[tokio::test]
async fn an_update_follows_its_finished_call_and_is_logged_after_it() {
    let env = Env::new("todos-update");
    let store = MemoryStore::default();
    let scripted = two_turns();
    let mut agent = checklisting(scripted.clone(), &env).with_log(
        SessionLog::new(Box::new(store.clone()), "s1"),
        log_context(),
    );
    let (outcome, events) = turn(&mut agent, "plan it").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{events:#?}"
    );
    assert_eq!(updates(&events), [(Some("t1".to_owned()), first_list())]);
    let finished = events
        .iter()
        .position(|e| matches!(e, AgentEvent::ToolCallFinished { .. }))
        .unwrap();
    assert!(matches!(
        events.get(finished + 1),
        Some(AgentEvent::TodosUpdated { .. })
    ));
    assert_eq!(agent.todos, first_list());

    let (outcome, _) = turn(&mut agent, "go on").await;
    assert!(matches!(outcome, TurnOutcome::Completed { .. }));
    let requests = scripted.sent();
    assert_eq!(requests.len(), 4);
    assert_one_prefix(&requests);
    let sent = requests[1].text();
    assert!(sent.contains("Todos updated: 0 of 2 completed."), "{sent}");

    let items = &updates(&events)[0].1;
    let progress = wisp_protocol::todos::progress(items);
    assert_eq!((progress.completed, progress.total), (0, 2));
    assert_eq!(
        wisp_protocol::todos::in_progress(items).map(|item| item.text.as_str()),
        Some("Read the plan")
    );

    let logged = log_events(&store);
    let kinds: Vec<&LogEvent> = logged
        .iter()
        .filter(|e| {
            matches!(
                e,
                LogEvent::ToolCallFinished { .. } | LogEvent::TodosUpdated { .. }
            )
        })
        .collect();
    assert!(matches!(kinds[0], LogEvent::ToolCallFinished { .. }));
    assert_eq!(
        kinds[1],
        &LogEvent::TodosUpdated {
            turn: 1,
            call_id: "t1".into(),
            items: first_list(),
        }
    );
}

#[tokio::test]
async fn a_refused_list_changes_nothing() {
    let env = Env::new("todos-refused");
    let key = format!(
        "CANARY-{} sk-ant-api03-CANARY-refused-checklist-field",
        "x".repeat(100)
    );
    let bad = json!({"todos": [{"text": "a", "status": "pending"}], (key): 1});
    let scripted = scripted(vec![uses(&[call("t1", NAME, bad)]), done("sorry")]);
    let mut agent = checklisting(scripted, &env);
    let (_, events) = turn(&mut agent, "plan it").await;
    assert!(updates(&events).is_empty(), "{events:#?}");
    assert!(agent.todos.is_empty());
    assert_eq!(
        finished(&events),
        [(
            "t1".to_owned(),
            ToolOutcome::InvalidInput {
                message: "unknown field".to_owned(),
            }
        )]
    );
    assert_eq!(
        pick!(&events, AgentEvent::ToolCallRequested { display, .. } => display.target.as_str()),
        [""]
    );
}

#[tokio::test]
async fn a_rewritten_checklist_keeps_private_fields_out_of_its_result_and_update() {
    const ACK: &str = "Todos updated: 0 of 1 completed.";
    let key = "sk-ant-api03-CANARY-checklist-rewrite-test-key";
    let email = ["CANARY-checklist-rewrite", "example.invalid"].join("@");
    let path = ["/home", "CANARY-checklist-rewrite", "work"].join("/");
    let private = format!("Check {key} for {email} in {path}");
    let rewritten = input(&[item(&private, TodoStatus::InProgress)]);
    let original = input(&[item("Review the plan", TodoStatus::InProgress)]);
    let provider = scripted(vec![uses(&[call("t1", NAME, original)]), done("ok")]);
    let env = Env::new("todos-private-rewrite");
    let store = MemoryStore::default();
    let hooks = vec![hook_source(
        "aa",
        &hook_body("h", "before-tool", ""),
        replying(Reply::Rewrite(rewritten.to_string())),
        &Calls::default(),
    )];
    let mut agent = with_test_hooks(checklisting(provider.clone(), &env), hooks).with_log(
        SessionLog::new(Box::new(store.clone()), "s1"),
        log_context(),
    );

    let (_, events) = turn(&mut agent, "plan it").await;
    let expected = vec![item(crate::session::REDACTED, TodoStatus::InProgress)];
    assert_eq!(
        updates(&events),
        [(Some("t1".to_owned()), expected.clone())]
    );
    assert_eq!(
        pick!(&events, AgentEvent::ToolCallFinished { result, .. } => result.as_deref()),
        [Some(ACK)]
    );
    let logged = log_events(&store);
    assert_eq!(
        pick!(&logged, LogEvent::ToolCallFinished { result, .. } => result.as_deref()),
        [Some(ACK)]
    );
    assert_eq!(
        pick!(&logged, LogEvent::TodosUpdated { items, .. } => items.as_slice()),
        [expected.as_slice()]
    );
    let requests = provider.sent();
    let next = &requests[1];
    assert_eq!(results(next.messages.last().unwrap()), [("t1", ACK, false)]);
    let sent = next.text();
    for value in [key, email.as_str(), path.as_str()] {
        assert!(!sent.contains(value), "{sent}");
    }
}

#[tokio::test]
async fn a_rewind_past_an_update_restores_the_earlier_list_and_a_resume_shows_the_latest() {
    let env = Env::new("todos-rewind");
    let store = MemoryStore::default();
    let mut agent = checklisting(two_turns(), &env).with_log(
        SessionLog::new(Box::new(store.clone()), "s1"),
        log_context(),
    );
    turn(&mut agent, "plan it").await;
    let (_, events) = turn(&mut agent, "go on").await;
    assert_eq!(updates(&events), [(Some("t2".to_owned()), second_list())]);
    assert_eq!(agent.todos, second_list());

    let records = store.read("s1").unwrap();
    let resumed = resume_from(&env, &records);
    assert_eq!(resumed.todos, second_list());
    let toolbox = crate::tool::testkit::bundled(&[NAME]);
    let shown = crate::resume::transcript(&records, &toolbox, None);
    assert_eq!(
        updates(&shown),
        [
            (Some("t1".to_owned()), first_list()),
            (Some("t2".to_owned()), second_list()),
        ]
    );

    let (tx, mut rx) = mpsc::channel(256);
    agent
        .rewind_command(RequestId(1), RewindPoint::Turn { turn: 2 }, false, &tx)
        .await;
    drop(tx);
    let mut seen = Vec::new();
    while let Some(e) = rx.recv().await {
        seen.push(e);
    }
    let rewound = seen
        .iter()
        .position(|e| matches!(e, AgentEvent::Rewound { .. }))
        .unwrap_or_else(|| panic!("{seen:#?}"));
    assert_eq!(updates(&seen[rewound..]), [(None, first_list())]);
    assert_eq!(agent.todos, first_list());

    let records = store.read("s1").unwrap();
    let resumed = resume_from(&env, &records);
    assert_eq!(resumed.todos, first_list());
    let shown = crate::resume::transcript(&records, &toolbox, None);
    let last = updates(&shown).pop();
    assert_eq!(last, Some((None, first_list())));
}

#[tokio::test]
async fn rewind_refreshes_todos_after_waiting_for_live_event_capacity() {
    use crate::session::REDACTED;

    let canary = "CANARYOpaqueDuringRewindEventBackpressure";
    let mut first = first_list();
    first[1].text = canary.into();
    let provider = scripted(vec![
        uses(&[call("t1", NAME, input(&first))]),
        done("planned"),
        uses(&[call("t2", NAME, input(&second_list()))]),
        done("moved on"),
    ]);
    let env = Env::new("todos-rewind-backpressure");
    let store = MemoryStore::default();
    let mut agent = checklisting(provider, &env).with_log(
        SessionLog::new(Box::new(store.clone()), "s1"),
        log_context(),
    );
    turn(&mut agent, "plan it").await;
    turn(&mut agent, "go on").await;
    assert!(!crate::secrets::scan::contains_secret_or_identity(
        canary, None
    ));
    let original = store.read("s1").unwrap();
    let (tx, mut rx) = mpsc::channel(1);
    let mut seen = Vec::new();
    {
        let rewind = agent.rewind_command(RequestId(1), RewindPoint::Turn { turn: 2 }, false, &tx);
        let mut rewind = std::pin::pin!(rewind);
        assert!(futures::poll!(rewind.as_mut()).is_pending());
        assert_eq!(tx.capacity(), 0);
        crate::session::register_secrets([canary.to_owned()]);
        assert!(matches!(rx.try_recv(), Ok(AgentEvent::Rewound { .. })));
        loop {
            tokio::select! {
                _ = rewind.as_mut() => break,
                Some(event) = rx.recv() => seen.push(event),
            }
        }
    }
    seen.extend(std::iter::from_fn(|| rx.try_recv().ok()));
    first[1].text = REDACTED.into();
    assert_eq!(updates(&seen), [(None, first.clone())]);
    assert_eq!(agent.todos, first);
    assert!(store.read("s1").unwrap().starts_with(&original));
}

#[tokio::test]
async fn describe_refreshes_checklists_after_waiting_for_snapshot_capacity() {
    use crate::session::{REDACTED, register_secrets};

    let canary = "CANARYOpaqueAtSessionSnapshotCapacity";
    let mut list = first_list();
    list[1].text = canary.into();
    let provider = scripted(vec![
        uses(&[call("t1", NAME, input(&list))]),
        done("planned"),
    ]);
    let env = Env::new("todos-snapshot-backpressure");
    let store = MemoryStore::default();
    let mut agent = checklisting(provider, &env).with_log(
        SessionLog::new(Box::new(store.clone()), "s1"),
        log_context(),
    );
    turn(&mut agent, "plan it").await;
    agent.cut_view();
    let original = store.read("s1").unwrap();
    assert_eq!(
        updates(&agent.view(&original).events),
        [(Some("t1".into()), list.clone())]
    );
    let (tx, mut rx) = mpsc::channel(1);
    tx.try_send(AgentEvent::LogDisabled {
        reason: "test capacity".into(),
    })
    .unwrap();
    let mut describe = std::pin::pin!(agent.describe(RequestId(1), &tx));
    assert!(futures::poll!(describe.as_mut()).is_pending());
    assert_eq!(tx.capacity(), 0);
    register_secrets([canary.to_owned()]);
    assert!(matches!(rx.try_recv(), Ok(AgentEvent::LogDisabled { .. })));
    let ((), event) = tokio::join!(describe, rx.recv());
    let Some(AgentEvent::SessionSnapshot { view, .. }) = event else {
        panic!("expected a session snapshot");
    };
    list[1].text = REDACTED.into();
    assert_eq!(updates(&view.events), [(Some("t1".into()), list)]);
    assert_eq!(store.read("s1").unwrap(), original);
}

#[tokio::test]
async fn a_resumed_agent_rewinds_to_the_list_it_rebuilt_and_keeps_one_prefix() {
    let env = Env::new("todos-resumed-rewind");
    let store = MemoryStore::default();
    let live = two_turns();
    let mut agent = checklisting(live.clone(), &env).with_log(
        SessionLog::new(Box::new(store.clone()), "s1"),
        log_context(),
    );
    turn(&mut agent, "plan it").await;
    turn(&mut agent, "go on").await;
    let records = store.read("s1").unwrap();
    let after = scripted(vec![done("again"), done("once more")]);
    let mut resumed = resume_with(&env, &records, after.clone(), &[NAME]);
    assert_eq!(resumed.todos, second_list());
    let (tx, mut rx) = mpsc::channel(256);
    resumed
        .rewind_command(RequestId(1), RewindPoint::Turn { turn: 2 }, false, &tx)
        .await;
    drop(tx);
    let mut seen = Vec::new();
    while let Some(e) = rx.recv().await {
        seen.push(e);
    }
    assert_eq!(updates(&seen), [(None, first_list())], "{seen:#?}");
    assert_eq!(resumed.todos, first_list());
    let (outcome, _) = turn(&mut resumed, "go on, differently").await;
    assert!(matches!(outcome, TurnOutcome::Completed { .. }));
    let mut all = live.sent();
    all.extend(after.sent());
    assert_eq!(all.len(), 5);
    assert_one_prefix(&all);
}

#[tokio::test]
async fn a_session_resumed_without_the_tool_shows_no_checklist() {
    let env = Env::new("todos-resumed-without");
    let store = MemoryStore::default();
    let mut agent = checklisting(two_turns(), &env).with_log(
        SessionLog::new(Box::new(store.clone()), "s1"),
        log_context(),
    );
    turn(&mut agent, "plan it").await;
    let records = store.read("s1").unwrap();
    let resumed = resume_with(&env, &records, scripted(vec![]), &["show_diff"]);
    assert!(resumed.todos.is_empty());
    let shown = crate::resume::transcript(
        &records,
        &crate::tool::testkit::bundled(&["show_diff"]),
        None,
    );
    assert_eq!(
        updates(&shown),
        [(Some("t1".to_owned()), first_list()), (None, Vec::new())]
    );
}

#[tokio::test]
async fn a_rewind_to_before_any_update_clears_the_list() {
    let env = Env::new("todos-clear");
    let store = MemoryStore::default();
    let mut agent = checklisting(two_turns(), &env).with_log(
        SessionLog::new(Box::new(store.clone()), "s1"),
        log_context(),
    );
    turn(&mut agent, "plan it").await;
    let (tx, mut rx) = mpsc::channel(256);
    agent
        .rewind_command(RequestId(1), RewindPoint::Turn { turn: 1 }, false, &tx)
        .await;
    drop(tx);
    let mut seen = Vec::new();
    while let Some(e) = rx.recv().await {
        seen.push(e);
    }
    assert_eq!(updates(&seen), [(None, Vec::new())], "{seen:#?}");
    assert!(agent.todos.is_empty());
    let records = store.read("s1").unwrap();
    let shown = crate::resume::transcript(&records, &crate::tool::testkit::bundled(&[NAME]), None);
    assert_eq!(updates(&shown).pop(), Some((None, Vec::new())));
}

struct Checklist;

impl crate::plugin::testing::TestTool for Checklist {
    fn name(&self) -> String {
        "checklist".to_owned()
    }

    fn prepare(&self, input: &str) -> Result<Box<dyn wisp_plugin_api::tools::Prepared>, String> {
        Ok(crate::plugin::testing::Call::boxed(
            crate::plugin::testing::fallback("checklist", input),
            (),
        ))
    }

    fn run<'a>(
        &'a self,
        _call: Box<dyn wisp_plugin_api::tools::Prepared>,
        _ctx: &'a dyn wisp_plugin_api::host::CallContext,
    ) -> futures::future::BoxFuture<'a, wisp_plugin_api::types::ToolRun> {
        Box::pin(async { wisp_plugin_api::types::ToolRun::completed() })
    }
}

#[tokio::test]
async fn the_checklist_follows_the_tool_providing_todos_whatever_its_name() {
    let env = Env::new("todos-provider");
    let store = MemoryStore::default();
    let mut agent = checklisting(two_turns(), &env).with_log(
        SessionLog::new(Box::new(store.clone()), "s1"),
        log_context(),
    );
    turn(&mut agent, "plan it").await;
    let records = store.read("s1").unwrap();
    let mut other = Toolbox::new([crate::plugin::testing::providing(
        Arc::new(Checklist),
        crate::tool::TODOS,
    )]);
    let shown = crate::resume::transcript(&records, &other, None);
    assert_eq!(updates(&shown), [(Some("t1".to_owned()), first_list())]);
    let renamed = resume_into(&env, &records, scripted(vec![]), other.clone());
    assert_eq!(renamed.todos, first_list());
    other.set_enabled("checklist", false);
    let shown = crate::resume::transcript(&records, &other, None);
    assert_eq!(
        updates(&shown),
        [(Some("t1".to_owned()), first_list()), (None, Vec::new())]
    );
    let mut off = crate::tool::testkit::bundled(&[NAME]);
    off.set_enabled(NAME, false);
    let resumed = resume_with(&env, &records, scripted(vec![]), &[NAME]);
    assert_eq!(resumed.todos, first_list());
    let shown = crate::resume::transcript(&records, &off, None);
    assert_eq!(
        updates(&shown),
        [(Some("t1".to_owned()), first_list()), (None, Vec::new())]
    );
}
