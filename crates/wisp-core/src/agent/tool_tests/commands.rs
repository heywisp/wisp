use std::collections::{BTreeMap, BTreeSet};

use tokio::sync::Notify;
use wisp_plugin_api::commands::{CommandLine, CommandReply, CommandRow};
use wisp_plugin_api::hooks::{CommandInfo, Event, Reply};
use wisp_plugin_api::host::Host;
use wisp_plugin_api::slots::{TextSpan, Tone};
use wisp_plugin_testkit::hooks::{self as kit, Calls, Run};
use wisp_protocol::{
    CommandOutcome, CommandRefusal, CommandWait, RejectKind, RequestId, WithdrawnBy,
};

use super::*;
use crate::plugin::Make;
use crate::plugin::testing::{TestTool, fallback, hook_body, with_test_hooks};
use crate::test_guard::until_event;

const ID: &str = "testkit";

type Pair = (Arc<Notify>, Arc<Notify>);

#[derive(Clone, Default)]
struct Gates {
    pairs: Arc<Mutex<BTreeMap<String, Pair>>>,
    fetched: Arc<Mutex<Vec<String>>>,
}

impl Gates {
    fn pair(&self, key: &str) -> Pair {
        self.pairs
            .lock()
            .unwrap()
            .entry(key.to_owned())
            .or_default()
            .clone()
    }

    async fn started(&self, key: &str) {
        within("the command's start", self.pair(key).0.notified()).await;
    }

    fn release(&self, key: &str) {
        self.pair(key).1.notify_one();
    }
}

fn body() -> String {
    [
        kit::command("note", ""),
        kit::command("lines", ""),
        kit::command("none", ""),
        kit::command("send", ""),
        kit::command("panic", ""),
        kit::command("wait", ""),
        kit::command("brief", "timeout_ms = 100\n"),
        kit::command("fetch", "timeout_ms = 400\n"),
        kit::command("long-fetch", ""),
        kit::command("facts", ""),
        kit::command(
            "tidy",
            "busy = { between_turns = \"Tidy runs between turns.\" }\n",
        ),
    ]
    .concat()
}

fn reply_of(kind: &str, text: &str) -> CommandReply {
    match kind {
        "send" => CommandReply::Send(text.to_owned()),
        "none" => CommandReply::None,
        _ => CommandReply::Note(text.to_owned()),
    }
}

fn runner(gates: Gates) -> Run {
    Arc::new(move |host: Arc<dyn Host>, name: String, args: String| {
        let gates = gates.clone();
        Box::pin(async move {
            match name.as_str() {
                "fetch" | "long-fetch" => {
                    let request = wisp_plugin_api::http::Request {
                        method: "GET".to_owned(),
                        url: args,
                        headers: Vec::new(),
                        body: None,
                        timeout_ms: None,
                    };
                    let got = host.fetch(request).await.err();
                    gates.fetched.lock().unwrap().push(format!("{got:?}"));
                    CommandReply::Note("fetched".to_owned())
                }
                "note" => CommandReply::Note(args),
                "lines" => {
                    let text = TextSpan {
                        text: args,
                        tone: Tone::Muted,
                        bold: true,
                    };
                    CommandReply::Lines(vec![
                        CommandLine::Row(CommandRow {
                            left: vec![text.clone()],
                            right: vec![text.clone()],
                        }),
                        CommandLine::Text(vec![text]),
                    ])
                }
                "send" => CommandReply::Send(args),
                "panic" => panic!("a testkit command panics"),
                "wait" => {
                    let mut words = args.splitn(3, ' ');
                    let key = words.next().unwrap_or_default().to_owned();
                    let kind = words.next().unwrap_or_default().to_owned();
                    let text = words.next().unwrap_or_default().to_owned();
                    let (started, release) = gates.pair(&key);
                    started.notify_one();
                    release.notified().await;
                    reply_of(&kind, &text)
                }
                "brief" => std::future::pending().await,
                "tidy" => CommandReply::Note("tidied".to_owned()),
                "facts" => CommandReply::Note(host.get("time").unwrap_or_default()),
                _ => CommandReply::None,
            }
        })
    })
}

fn kit_source(extra: &str, script: kit::Script, gates: &Gates, calls: &Calls) -> (String, Make) {
    let make = kit::commanding(script, runner(gates.clone()), calls.clone());
    (kit::manifest_in("0.2.0", ID, &(body() + extra)), make)
}

struct GateTool(Gates);

impl TestTool for GateTool {
    fn name(&self) -> String {
        "gate".to_owned()
    }

    fn meta(&self) -> wisp_plugin_api::tools::ToolMeta {
        crate::plugin::testing::meta(60_000, None)
    }

    fn prepare(&self, input: &str) -> Result<Box<dyn wisp_plugin_api::tools::Prepared>, String> {
        Ok(crate::plugin::testing::Call::boxed(
            fallback("gate", input),
            (),
        ))
    }

    fn run<'a>(
        &'a self,
        _call: Box<dyn wisp_plugin_api::tools::Prepared>,
        _ctx: &'a dyn wisp_plugin_api::host::CallContext,
    ) -> futures::future::BoxFuture<'a, wisp_plugin_api::types::ToolRun> {
        Box::pin(async move {
            let (started, release) = self.0.pair("tool");
            started.notify_one();
            release.notified().await;
            wisp_plugin_api::types::ToolRun::completed()
        })
    }
}

fn says(text: &'static str) -> Step {
    done(text)
}

fn held(text: &'static str, gate: &Arc<Notify>) -> Step {
    done(text).held(Wait::Notified(gate.clone()))
}

fn calls_gate(gate: Option<&Arc<Notify>>) -> Step {
    let step = uses(&[call("g", "gate", json!({}))]);
    match gate {
        Some(gate) => step.held(Wait::Notified(gate.clone())),
        None => step,
    }
}

fn build(
    provider: Arc<dyn Provider>,
    env: &Env,
    sources: Vec<(String, Make)>,
    gates: &Gates,
) -> Agent {
    let toolbox = crate::plugin::testing::toolbox(vec![Arc::new(GateTool(gates.clone()))]);
    with_test_hooks(agent_with(provider, env, toolbox, GRACE), sources)
}

struct Live {
    commands: mpsc::Sender<ClientCommand>,
    events: mpsc::Receiver<AgentEvent>,
    seen: Vec<AgentEvent>,
    ends: usize,
    _task: AgentTask,
}

impl Live {
    fn start(agent: Agent) -> Self {
        let (commands, events, task) = spawn(agent);
        Self {
            commands,
            events,
            seen: Vec::new(),
            ends: 0,
            _task: task,
        }
    }

    async fn send(&self, command: ClientCommand) {
        self.commands.send(command).await.unwrap();
    }

    async fn until(&mut self, what: &str, pick: impl Fn(&AgentEvent) -> bool) -> AgentEvent {
        until_event(&mut self.events, &mut self.seen, what, pick).await
    }

    async fn answer(&mut self, request: u64) -> CommandOutcome {
        let earlier = self.seen.iter().find_map(|e| match e {
            AgentEvent::CommandDone {
                answers, outcome, ..
            } if answers.0 == request => Some(outcome.clone()),
            _ => None,
        });
        if let Some(outcome) = earlier {
            return outcome;
        }
        let event = self
            .until(
                &format!("the answer to {request}"),
                |e| matches!(e, AgentEvent::CommandDone { answers, .. } if answers.0 == request),
            )
            .await;
        let AgentEvent::CommandDone { outcome, .. } = event else {
            unreachable!()
        };
        outcome
    }

    async fn ended(&mut self) -> TurnOutcome {
        let ends = |seen: &[AgentEvent]| {
            seen.iter()
                .filter(|e| matches!(e, AgentEvent::TurnEnded { .. }))
                .count()
        };
        if ends(&self.seen) > self.ends {
            self.ends += 1;
            let found = self
                .seen
                .iter()
                .filter_map(|e| match e {
                    AgentEvent::TurnEnded { outcome, .. } => Some(outcome.clone()),
                    _ => None,
                })
                .nth(self.ends - 1);
            if let Some(outcome) = found {
                return outcome;
            }
        }
        self.ends += 1;
        let event = self
            .until("the turn's end", |e| {
                matches!(e, AgentEvent::TurnEnded { .. })
            })
            .await;
        let AgentEvent::TurnEnded { outcome, .. } = event else {
            unreachable!()
        };
        outcome
    }

    async fn quiet(&mut self, ms: u64) {
        let until = tokio::time::Instant::now() + Duration::from_millis(ms);
        while let Ok(Some(e)) = tokio::time::timeout_at(until, self.events.recv()).await {
            self.seen.push(e);
        }
    }

    async fn close(mut self) -> Vec<AgentEvent> {
        drop(self.commands);
        while let Some(e) = self.events.recv().await {
            self.seen.push(e);
        }
        self.seen
    }
}

fn run(request: u64, name: &str, args: &str) -> ClientCommand {
    ClientCommand::RunPluginCommand {
        name: format!("{ID}:{name}"),
        args: wisp_protocol::Secret::new(args),
        request: RequestId(request),
    }
}

fn note(text: &str) -> CommandOutcome {
    CommandOutcome::Note {
        text: text.to_owned(),
    }
}

fn sent(n: u64) -> CommandOutcome {
    CommandOutcome::Sent {
        input: InputId(format!("wisp-command-{n}")),
    }
}

fn index_of(seen: &[AgentEvent], pick: impl Fn(&AgentEvent) -> bool) -> usize {
    seen.iter()
        .position(pick)
        .unwrap_or_else(|| panic!("not seen in {seen:#?}"))
}

fn done_at(seen: &[AgentEvent], request: u64) -> usize {
    index_of(
        seen,
        |e| matches!(e, AgentEvent::CommandDone { answers, .. } if answers.0 == request),
    )
}

fn deferrals(seen: &[AgentEvent]) -> Vec<(u64, CommandWait)> {
    pick!(seen, AgentEvent::CommandDeferred { request, until } => (request.0, *until))
}

fn answers(seen: &[AgentEvent]) -> Vec<u64> {
    pick!(seen, AgentEvent::CommandDone { answers, .. } => answers.0)
}

fn accepted(seen: &[AgentEvent], id: &str) -> Option<DeliveryMode> {
    seen.iter().find_map(|e| match e {
        AgentEvent::InputAccepted { id: got, mode, .. } if got.0 == id => Some(*mode),
        _ => None,
    })
}

fn user_texts(message: &Message) -> Vec<String> {
    message
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text }
                if !["Sent: ", "Context: ", "<environment>"]
                    .iter()
                    .any(|lead| text.starts_with(lead)) =>
            {
                Some(text.clone())
            }
            _ => None,
        })
        .collect()
}

fn last_user(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .rev()
        .find(|m| m.role == Role::User)
        .map(user_texts)
        .unwrap_or_default()
}

fn ran_names(calls: &Calls) -> Vec<String> {
    calls.commands().into_iter().map(|(name, _)| name).collect()
}

fn plain(provider: Arc<dyn Provider>, env: &Env, gates: &Gates, calls: &Calls) -> Agent {
    build(
        provider,
        env,
        vec![kit_source("", kit::none(), gates, calls)],
        gates,
    )
}

#[tokio::test]
async fn each_reply_answers_its_own_request_and_the_session_goes_on() {
    let env = Env::new("commands-replies");
    let (gates, calls) = (Gates::default(), Calls::default());
    let provider = Fake::new(vec![says("hello")]);
    let mut live = Live::start(plain(provider, &env, &gates, &calls));
    live.send(run(1, "note", "a note")).await;
    assert_eq!(live.answer(1).await, note("a note"));
    live.send(run(2, "none", "")).await;
    assert_eq!(live.answer(2).await, CommandOutcome::None);
    live.send(run(3, "panic", "")).await;
    assert_eq!(
        live.answer(3).await,
        CommandOutcome::Failed {
            message: "the testkit plugin panicked".to_owned()
        }
    );
    live.send(run(4, "nosuch", "")).await;
    assert_eq!(
        live.answer(4).await,
        CommandOutcome::Refused {
            kind: CommandRefusal::Unknown,
            hint: "there is no command /testkit:nosuch".to_owned()
        }
    );
    live.send(run(5, "send", "  \n")).await;
    assert_eq!(
        live.answer(5).await,
        CommandOutcome::Refused {
            kind: CommandRefusal::Empty,
            hint: "empty message: nothing to send".to_owned()
        }
    );
    live.send(run(6, "lines", "rows")).await;
    let text = wisp_protocol::slot::SlotSpan::Text {
        text: "rows".into(),
        tone: wisp_protocol::slot::SlotTone::Muted,
        bold: true,
    };
    assert_eq!(
        live.answer(6).await,
        CommandOutcome::Lines {
            lines: vec![
                wisp_protocol::CommandLine::Row {
                    left: vec![text.clone()],
                    right: vec![text.clone()]
                },
                wisp_protocol::CommandLine::Text { spans: vec![text] },
            ]
        }
    );
    live.send(ClientCommand::send("m", "hi")).await;
    assert!(matches!(live.ended().await, TurnOutcome::Completed { .. }));
    let seen = live.close().await;
    assert_eq!(
        ran_names(&calls),
        ["note", "none", "panic", "send", "lines"]
    );
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, AgentEvent::InputAccepted { id, .. } if id.0.starts_with("wisp-command-"))),
        "{seen:#?}"
    );
    assert!(deferrals(&seen).is_empty(), "{seen:#?}");
}

#[tokio::test]
async fn a_send_reply_between_turns_opens_a_turn_as_a_queued_input_and_reaches_the_model() {
    let env = Env::new("commands-send-idle");
    let (gates, calls) = (Gates::default(), Calls::default());
    let provider = Fake::new(vec![says("got it")]);
    let mut live = Live::start(plain(provider.clone(), &env, &gates, &calls));
    live.send(run(1, "send", "C")).await;
    assert_eq!(live.answer(1).await, sent(1));
    live.ended().await;
    let seen = live.close().await;
    assert_eq!(accepted(&seen, "wisp-command-1"), Some(DeliveryMode::Queue));
    assert_eq!(last_user(&provider.sent()[0].messages), ["C"]);
}

#[tokio::test]
async fn a_slow_command_holds_up_neither_messages_nor_model_changes_and_answers_later() {
    let env = Env::new("commands-lane");
    let (gates, calls) = (Gates::default(), Calls::default());
    let provider = Fake::new(vec![says("one")]);
    let mut live = Live::start(plain(provider, &env, &gates, &calls));
    live.send(run(1, "wait", "k note slow")).await;
    gates.started("k").await;
    live.send(ClientCommand::send("m", "hi")).await;
    live.ended().await;
    live.send(ClientCommand::shortcut(
        RequestId(9),
        None,
        Some("claude-sonnet-5-5".to_owned()),
        None,
    ))
    .await;
    live.until("the model change", |e| {
        matches!(e, AgentEvent::SettingsChanged { .. })
    })
    .await;
    assert!(answers(&live.seen).is_empty(), "{:#?}", live.seen);
    gates.release("k");
    assert_eq!(live.answer(1).await, note("slow"));
    live.close().await;
}

#[tokio::test]
async fn two_commands_run_one_after_the_other_in_arrival_order() {
    let env = Env::new("commands-order");
    let (gates, calls) = (Gates::default(), Calls::default());
    let mut live = Live::start(plain(Fake::new(Vec::new()), &env, &gates, &calls));
    live.send(run(1, "wait", "k note first")).await;
    gates.started("k").await;
    live.send(run(2, "note", "second")).await;
    live.until("the second's wait", |e| {
        matches!(e, AgentEvent::CommandDeferred { .. })
    })
    .await;
    assert_eq!(ran_names(&calls), ["wait"]);
    gates.release("k");
    assert_eq!(live.answer(1).await, note("first"));
    assert_eq!(live.answer(2).await, note("second"));
    let seen = live.close().await;
    assert_eq!(deferrals(&seen), [(2, CommandWait::Command)]);
    assert_eq!(answers(&seen), [1, 2]);
    assert_eq!(ran_names(&calls), ["wait", "note"]);
}

#[tokio::test]
async fn a_command_past_its_timeout_reports_timed_out() {
    let env = Env::new("commands-timeout");
    let (gates, calls) = (Gates::default(), Calls::default());
    let mut live = Live::start(plain(Fake::new(Vec::new()), &env, &gates, &calls));
    live.send(run(1, "brief", "")).await;
    assert_eq!(live.answer(1).await, CommandOutcome::TimedOut);
    live.send(run(2, "note", "after")).await;
    assert_eq!(live.answer(2).await, note("after"));
    live.close().await;
}

#[tokio::test]
async fn esc_cancels_the_running_command_and_every_waiting_one_once() {
    let env = Env::new("commands-esc");
    let (gates, calls) = (Gates::default(), Calls::default());
    let mut live = Live::start(plain(Fake::new(Vec::new()), &env, &gates, &calls));
    live.send(run(1, "wait", "k send late")).await;
    gates.started("k").await;
    live.send(run(2, "note", "never")).await;
    live.send(ClientCommand::Cancel).await;
    assert_eq!(live.answer(1).await, CommandOutcome::Cancelled);
    assert_eq!(live.answer(2).await, CommandOutcome::Cancelled);
    gates.release("k");
    live.quiet(300).await;
    let seen = live.close().await;
    assert_eq!(answers(&seen), [1, 2]);
    assert_eq!(ran_names(&calls), ["wait"]);
    assert!(accepted(&seen, "wisp-command-1").is_none(), "{seen:#?}");
}

#[tokio::test]
async fn a_between_turns_command_sent_during_a_turn_is_refused_with_its_hint() {
    let env = Env::new("commands-busy");
    let (gates, calls) = (Gates::default(), Calls::default());
    let gate = Arc::new(Notify::new());
    let provider = Fake::new(vec![held("one", &gate)]);
    let mut live = Live::start(plain(provider, &env, &gates, &calls));
    live.send(run(3, "wait", "k none")).await;
    gates.started("k").await;
    live.send(ClientCommand::send("m", "hi")).await;
    live.until("the turn's start", |e| {
        matches!(e, AgentEvent::TurnStarted { .. })
    })
    .await;
    live.send(run(1, "tidy", "")).await;
    assert_eq!(
        live.answer(1).await,
        CommandOutcome::Refused {
            kind: CommandRefusal::Busy,
            hint: "Tidy runs between turns.".to_owned()
        }
    );
    assert!(deferrals(&live.seen).is_empty(), "{:#?}", live.seen);
    gates.release("k");
    assert_eq!(live.answer(3).await, CommandOutcome::None);
    gate.notify_one();
    live.ended().await;
    live.send(run(2, "tidy", "")).await;
    assert_eq!(live.answer(2).await, note("tidied"));
    live.close().await;
    assert_eq!(ran_names(&calls), ["wait", "tidy"]);
}

#[tokio::test]
async fn a_between_turns_command_waiting_behind_a_slow_one_is_refused_when_a_turn_has_started() {
    let env = Env::new("commands-busy-later");
    let (gates, calls) = (Gates::default(), Calls::default());
    let gate = Arc::new(Notify::new());
    let provider = Fake::new(vec![held("one", &gate)]);
    let agent = hooked_at(provider, &env, "command", kit::none(), &gates, &calls);
    let mut live = Live::start(agent);
    live.send(run(1, "wait", "k note first")).await;
    gates.started("k").await;
    live.send(run(2, "tidy", "")).await;
    live.send(ClientCommand::send("m", "hi")).await;
    live.until("the turn's start", |e| {
        matches!(e, AgentEvent::TurnStarted { .. })
    })
    .await;
    gates.release("k");
    assert_eq!(live.answer(1).await, note("first"));
    assert!(matches!(
        live.answer(2).await,
        CommandOutcome::Refused {
            kind: CommandRefusal::Busy,
            ..
        }
    ));
    gate.notify_one();
    live.ended().await;
    live.quiet(300).await;
    live.close().await;
    assert_eq!(ran_names(&calls), ["wait"]);
    let fired: Vec<String> = calls
        .of("h")
        .into_iter()
        .filter_map(|c| match c.event {
            Event::Command(info) => Some(info.name),
            _ => None,
        })
        .collect();
    assert_eq!(fired, ["testkit:wait"]);
}

fn gated_hook(gates: &Gates, key: &'static str) -> kit::Script {
    let gates = gates.clone();
    Arc::new(move |_, hook, event| {
        let gates = gates.clone();
        Box::pin(async move {
            let steer = matches!(&event, Event::UserInput(input) if input.steer);
            if hook != "h" || steer {
                return Reply::None;
            }
            let (started, release) = gates.pair(key);
            started.notify_one();
            release.notified().await;
            match event {
                Event::UserInput(input) if input.text.starts_with("block") => {
                    Reply::Block("no blocks".to_owned())
                }
                _ => Reply::None,
            }
        })
    })
}

fn hooked_at(
    provider: Arc<dyn Provider>,
    env: &Env,
    point: &str,
    script: kit::Script,
    gates: &Gates,
    calls: &Calls,
) -> Agent {
    build(
        provider,
        env,
        vec![kit_source(&hook_body("h", point, ""), script, gates, calls)],
        gates,
    )
}

#[tokio::test]
async fn commands_sent_during_a_slow_hook_start_in_order_as_it_ends_before_the_stream_ends() {
    let env = Env::new("commands-during-hook");
    let (gates, calls) = (Gates::default(), Calls::default());
    let gate = Arc::new(Notify::new());
    let provider = Fake::new(vec![held("one", &gate)]);
    let agent = hooked_at(
        provider,
        &env,
        "before-request",
        gated_hook(&gates, "hook"),
        &gates,
        &calls,
    );
    let mut live = Live::start(agent);
    live.send(ClientCommand::send("m", "hi")).await;
    gates.started("hook").await;
    live.send(run(1, "note", "a")).await;
    live.send(run(2, "note", "b")).await;
    live.until(
        "the second wait",
        |e| matches!(e, AgentEvent::CommandDeferred { request, .. } if request.0 == 2),
    )
    .await;
    assert!(ran_names(&calls).is_empty());
    gates.release("hook");
    assert_eq!(live.answer(1).await, note("a"));
    assert_eq!(live.answer(2).await, note("b"));
    gate.notify_one();
    live.ended().await;
    let seen = live.close().await;
    assert_eq!(
        deferrals(&seen),
        [
            (1, CommandWait::Hook),
            (2, CommandWait::Hook),
            (2, CommandWait::Command)
        ]
    );
    assert_eq!(answers(&seen), [1, 2]);
    assert_eq!(ran_names(&calls), ["note", "note"]);
}

#[tokio::test]
async fn commands_sent_during_a_slow_call_run_once_in_order_after_it() {
    let env = Env::new("commands-during-call");
    let (gates, calls) = (Gates::default(), Calls::default());
    let provider = Fake::new(vec![calls_gate(None), says("done")]);
    let mut live = Live::start(plain(provider, &env, &gates, &calls));
    live.send(ClientCommand::send("m", "hi")).await;
    gates.started("tool").await;
    live.send(run(1, "note", "a")).await;
    live.send(run(2, "note", "b")).await;
    live.until(
        "the second wait",
        |e| matches!(e, AgentEvent::CommandDeferred { request, .. } if request.0 == 2),
    )
    .await;
    live.quiet(100).await;
    assert!(ran_names(&calls).is_empty());
    assert!(answers(&live.seen).is_empty());
    gates.release("tool");
    live.ended().await;
    let seen = live.close().await;
    assert_eq!(
        deferrals(&seen),
        [
            (1, CommandWait::Call),
            (2, CommandWait::Call),
            (2, CommandWait::Command)
        ]
    );
    assert_eq!(answers(&seen), [1, 2]);
    let finished = index_of(&seen, |e| matches!(e, AgentEvent::ToolCallFinished { .. }));
    assert!(finished < done_at(&seen, 1), "{seen:#?}");
    assert_eq!(ran_names(&calls), ["note", "note"]);
}

#[tokio::test]
async fn a_command_queued_behind_one_that_ends_during_a_call_starts_only_after_the_call() {
    let env = Env::new("commands-after-call");
    let (gates, calls) = (Gates::default(), Calls::default());
    let provider = Fake::new(vec![calls_gate(None), says("done")]);
    let mut live = Live::start(plain(provider, &env, &gates, &calls));
    live.send(run(1, "wait", "k note first")).await;
    gates.started("k").await;
    live.send(run(2, "note", "second")).await;
    live.send(ClientCommand::send("m", "hi")).await;
    gates.started("tool").await;
    gates.release("k");
    assert_eq!(live.answer(1).await, note("first"));
    live.quiet(100).await;
    assert_eq!(ran_names(&calls), ["wait"]);
    gates.release("tool");
    assert_eq!(live.answer(2).await, note("second"));
    let seen = live.close().await;
    let finished = index_of(&seen, |e| matches!(e, AgentEvent::ToolCallFinished { .. }));
    assert!(finished < done_at(&seen, 2), "{seen:#?}");
}

fn steer_text(id: &str, text: &str) -> ClientCommand {
    ClientCommand::send(id, text)
}

#[tokio::test]
async fn a_reply_finishing_mid_turn_lands_between_the_steers_typed_before_and_after_it() {
    let env = Env::new("commands-mid-turn");
    let (gates, calls) = (Gates::default(), Calls::default());
    let gate = Arc::new(Notify::new());
    let provider = Fake::new(vec![held("one", &gate), says("two")]);
    let mut live = Live::start(plain(provider.clone(), &env, &gates, &calls));
    live.send(ClientCommand::send("m", "go")).await;
    live.until("the turn's start", |e| {
        matches!(e, AgentEvent::TurnStarted { .. })
    })
    .await;
    live.send(steer_text("a", "A")).await;
    live.send(run(1, "wait", "k send C")).await;
    live.send(steer_text("b", "B")).await;
    gates.started("k").await;
    live.until(
        "B's acceptance",
        |e| matches!(e, AgentEvent::InputAccepted { id, .. } if id.0 == "b"),
    )
    .await;
    gates.release("k");
    assert_eq!(live.answer(1).await, sent(1));
    gate.notify_one();
    live.ended().await;
    let seen = live.close().await;
    assert_eq!(accepted(&seen, "wisp-command-1"), Some(DeliveryMode::Steer));
    assert_eq!(
        last_user(&provider.sent()[1].messages),
        ["A", "C", "B", STEER_NOTE]
    );
}

#[tokio::test]
async fn a_reply_after_a_delivered_steer_goes_before_the_later_one() {
    let env = Env::new("commands-mid-turn-delivered");
    let (gates, calls) = (Gates::default(), Calls::default());
    let gate = Arc::new(Notify::new());
    let provider = Fake::new(vec![calls_gate(None), held("two", &gate), says("three")]);
    let mut live = Live::start(plain(provider.clone(), &env, &gates, &calls));
    live.send(ClientCommand::send("m", "go")).await;
    gates.started("tool").await;
    live.send(steer_text("a", "A")).await;
    live.until(
        "A's acceptance",
        |e| matches!(e, AgentEvent::InputAccepted { id, .. } if id.0 == "a"),
    )
    .await;
    gates.release("tool");
    live.until("A's delivery", |e| {
        matches!(e, AgentEvent::InputDelivered { .. })
    })
    .await;
    live.send(run(1, "wait", "k send C")).await;
    live.send(steer_text("b", "B")).await;
    gates.started("k").await;
    live.until(
        "B's acceptance",
        |e| matches!(e, AgentEvent::InputAccepted { id, .. } if id.0 == "b"),
    )
    .await;
    gates.release("k");
    assert_eq!(live.answer(1).await, sent(1));
    gate.notify_one();
    live.ended().await;
    live.close().await;
    let requests = provider.sent();
    assert!(last_user(&requests[1].messages).contains(&"A".to_owned()));
    assert_eq!(last_user(&requests[2].messages), ["C", "B", STEER_NOTE]);
}

#[tokio::test]
async fn a_reply_to_a_command_sent_before_a_message_arrives_as_a_steer_of_that_messages_turn() {
    let env = Env::new("commands-between");
    let (gates, calls) = (Gates::default(), Calls::default());
    let gate = Arc::new(Notify::new());
    let provider = Fake::new(vec![held("one", &gate), says("two")]);
    let mut live = Live::start(plain(provider.clone(), &env, &gates, &calls));
    live.send(run(1, "wait", "k send C")).await;
    gates.started("k").await;
    live.send(ClientCommand::send("m", "B")).await;
    live.until("B's turn", |e| matches!(e, AgentEvent::TurnStarted { .. }))
        .await;
    gates.release("k");
    assert_eq!(live.answer(1).await, sent(1));
    gate.notify_one();
    live.ended().await;
    let seen = live.close().await;
    assert_eq!(accepted(&seen, "wisp-command-1"), Some(DeliveryMode::Steer));
    let requests = provider.sent();
    assert_eq!(last_user(&requests[0].messages), ["B"]);
    assert_eq!(last_user(&requests[1].messages), ["C", STEER_NOTE]);
}

#[tokio::test]
async fn a_reply_over_the_waiting_cap_is_refused_and_enters_nothing() {
    let env = Env::new("commands-too-many");
    let (gates, calls) = (Gates::default(), Calls::default());
    let gate = Arc::new(Notify::new());
    let provider = Fake::new(vec![held("one", &gate)]);
    let mut live = Live::start(plain(provider, &env, &gates, &calls));
    live.send(ClientCommand::send("m", "go")).await;
    live.until("the turn's start", |e| {
        matches!(e, AgentEvent::TurnStarted { .. })
    })
    .await;
    for n in 0..crate::agent::inbox::MAX_WAITING {
        live.send(ClientCommand::queue(format!("q{n}"), "later"))
            .await;
    }
    live.send(run(1, "send", "C")).await;
    assert_eq!(
        live.answer(1).await,
        CommandOutcome::Refused {
            kind: CommandRefusal::TooMany,
            hint: "32 messages are already waiting. Send this after the turn.".to_owned()
        }
    );
    let seen = live.close().await;
    assert!(accepted(&seen, "wisp-command-1").is_none());
}

#[tokio::test]
async fn the_command_point_fires_for_plugin_commands_and_context_but_not_model_or_refusals() {
    let env = Env::new("commands-point");
    let (gates, calls) = (Gates::default(), Calls::default());
    let agent = hooked_at(
        Fake::new(Vec::new()),
        &env,
        "command",
        kit::none(),
        &gates,
        &calls,
    );
    let mut live = Live::start(agent);
    let fired = |point: &str| {
        let point = point.to_owned();
        move |e: &AgentEvent| matches!(e, AgentEvent::HookRan { point: p, .. } if *p == point)
    };
    live.send(run(1, "note", "x y")).await;
    live.answer(1).await;
    live.until("the command hook", fired("command")).await;
    live.send(ClientCommand::ContextReport {
        detail: wisp_protocol::context::ReportDetail::All,
        request: RequestId(2),
    })
    .await;
    live.until("the second command hook", fired("command"))
        .await;
    live.send(ClientCommand::shortcut(
        RequestId(3),
        None,
        Some("claude-sonnet-5-5".to_owned()),
        None,
    ))
    .await;
    live.until("the model change", |e| {
        matches!(e, AgentEvent::SettingsChanged { .. })
    })
    .await;
    live.send(ClientCommand::Compact {
        focus: None,
        request: RequestId(4),
    })
    .await;
    live.send(run(5, "nosuch", "")).await;
    live.answer(5).await;
    live.quiet(300).await;
    live.close().await;
    let seen: Vec<CommandInfo> = calls
        .of("h")
        .into_iter()
        .filter_map(|c| match c.event {
            Event::Command(info) => Some(info),
            _ => None,
        })
        .collect();
    assert_eq!(
        seen,
        [
            CommandInfo {
                name: "testkit:note".to_owned(),
                args: "x y".to_owned()
            },
            CommandInfo {
                name: "context".to_owned(),
                args: "all".to_owned()
            },
        ]
    );
}

#[tokio::test]
async fn a_command_hook_for_context_during_a_call_fires_after_the_call() {
    let env = Env::new("commands-point-call");
    let (gates, calls) = (Gates::default(), Calls::default());
    let provider = Fake::new(vec![calls_gate(None), says("done")]);
    let agent = hooked_at(provider, &env, "command", kit::none(), &gates, &calls);
    let mut live = Live::start(agent);
    live.send(ClientCommand::send("m", "go")).await;
    gates.started("tool").await;
    live.send(ClientCommand::ContextReport {
        detail: wisp_protocol::context::ReportDetail::Summary,
        request: RequestId(2),
    })
    .await;
    live.until("the report", |e| matches!(e, AgentEvent::ContextReport(_)))
        .await;
    live.quiet(100).await;
    assert_eq!(calls.count("h"), 0);
    gates.release("tool");
    live.ended().await;
    live.close().await;
    assert_eq!(calls.count("h"), 1);
}

#[tokio::test]
async fn a_command_during_a_turn_start_input_hook_waits_and_runs_once_the_turn_opens() {
    let env = Env::new("commands-pre-turn");
    let (gates, calls) = (Gates::default(), Calls::default());
    let provider = Fake::new(vec![says("one")]);
    let agent = hooked_at(
        provider,
        &env,
        "user-input",
        gated_hook(&gates, "input"),
        &gates,
        &calls,
    );
    let mut live = Live::start(agent);
    live.send(ClientCommand::send("m", "go")).await;
    gates.started("input").await;
    live.send(run(1, "note", "x")).await;
    live.until("the wait", |e| {
        matches!(e, AgentEvent::CommandDeferred { .. })
    })
    .await;
    gates.release("input");
    assert_eq!(live.answer(1).await, note("x"));
    live.ended().await;
    let seen = live.close().await;
    assert_eq!(deferrals(&seen), [(1, CommandWait::Hook)]);
    assert_eq!(ran_names(&calls), ["note"]);
}

#[tokio::test]
async fn a_held_command_runs_from_the_idle_loop_when_every_input_is_blocked() {
    let env = Env::new("commands-pre-turn-blocked");
    let (gates, calls) = (Gates::default(), Calls::default());
    let agent = hooked_at(
        Fake::new(Vec::new()),
        &env,
        "user-input",
        gated_hook(&gates, "input"),
        &gates,
        &calls,
    );
    let mut live = Live::start(agent);
    live.send(ClientCommand::send("m", "block me")).await;
    gates.started("input").await;
    live.send(run(1, "note", "x")).await;
    live.until("the wait", |e| {
        matches!(e, AgentEvent::CommandDeferred { .. })
    })
    .await;
    gates.release("input");
    assert_eq!(live.answer(1).await, note("x"));
    let seen = live.close().await;
    let rejected = index_of(&seen, |e| {
        matches!(
            e,
            AgentEvent::Rejected {
                kind: Some(RejectKind::InputBlocked),
                ..
            }
        )
    });
    assert!(rejected < done_at(&seen, 1));
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, AgentEvent::TurnStarted { .. }))
    );
}

#[tokio::test]
async fn esc_during_a_turn_start_input_hook_cancels_a_held_command() {
    let env = Env::new("commands-pre-turn-esc");
    let (gates, calls) = (Gates::default(), Calls::default());
    let agent = hooked_at(
        Fake::new(Vec::new()),
        &env,
        "user-input",
        gated_hook(&gates, "input"),
        &gates,
        &calls,
    );
    let mut live = Live::start(agent);
    live.send(ClientCommand::send("m", "go")).await;
    gates.started("input").await;
    live.send(run(1, "note", "x")).await;
    live.until("the wait", |e| {
        matches!(e, AgentEvent::CommandDeferred { .. })
    })
    .await;
    live.send(ClientCommand::Cancel).await;
    assert_eq!(live.answer(1).await, CommandOutcome::Cancelled);
    live.quiet(200).await;
    live.close().await;
    assert!(ran_names(&calls).is_empty());
}

#[tokio::test]
async fn a_reply_finishing_during_a_turn_start_input_hook_waits_as_a_steer_of_that_turn() {
    let env = Env::new("commands-pre-turn-reply");
    let (gates, calls) = (Gates::default(), Calls::default());
    let provider = Fake::new(vec![says("one"), says("two")]);
    let agent = hooked_at(
        provider.clone(),
        &env,
        "user-input",
        gated_hook(&gates, "input"),
        &gates,
        &calls,
    );
    let mut live = Live::start(agent);
    live.send(run(1, "wait", "k send C")).await;
    gates.started("k").await;
    live.send(ClientCommand::send("m", "go")).await;
    gates.started("input").await;
    gates.release("k");
    live.quiet(200).await;
    assert!(answers(&live.seen).is_empty(), "{:#?}", live.seen);
    gates.release("input");
    assert_eq!(live.answer(1).await, sent(1));
    live.ended().await;
    let seen = live.close().await;
    assert_eq!(accepted(&seen, "wisp-command-1"), Some(DeliveryMode::Steer));
    let texts: Vec<String> = provider
        .sent()
        .iter()
        .flat_map(|r| last_user(&r.messages))
        .collect();
    assert!(texts.contains(&"C".to_owned()), "{texts:?}");
}

#[tokio::test]
async fn esc_during_a_turn_start_input_hook_gives_a_finished_reply_back_after_its_answer() {
    let env = Env::new("commands-pre-turn-reply-esc");
    let (gates, calls) = (Gates::default(), Calls::default());
    let agent = hooked_at(
        Fake::new(Vec::new()),
        &env,
        "user-input",
        gated_hook(&gates, "input"),
        &gates,
        &calls,
    );
    let mut live = Live::start(agent);
    live.send(run(1, "wait", "k send C")).await;
    gates.started("k").await;
    live.send(ClientCommand::send("m", "go")).await;
    gates.started("input").await;
    gates.release("k");
    live.quiet(200).await;
    live.send(ClientCommand::Cancel).await;
    assert_eq!(live.answer(1).await, sent(1));
    let withdrawn = live
        .until("the reply given back", |e| {
            matches!(e, AgentEvent::InputWithdrawn { ids, .. } if ids.iter().any(|i| i.0 == "wisp-command-1"))
        })
        .await;
    let AgentEvent::InputWithdrawn { by, .. } = withdrawn else {
        unreachable!()
    };
    assert_eq!(by, WithdrawnBy::TurnStopped);
    live.close().await;
}

#[tokio::test]
async fn closing_with_a_command_running_answers_it_cancelled_before_session_end() {
    let env = Env::new("commands-close");
    let (gates, calls) = (Gates::default(), Calls::default());
    let agent = hooked_at(
        Fake::new(Vec::new()),
        &env,
        "session-end",
        kit::none(),
        &gates,
        &calls,
    );
    let live = Live::start(agent);
    live.send(run(1, "wait", "k send C")).await;
    gates.started("k").await;
    let seen = live.close().await;
    let ended = index_of(
        &seen,
        |e| matches!(e, AgentEvent::HookRan { point, .. } if point == "session-end"),
    );
    assert!(done_at(&seen, 1) < ended, "{seen:#?}");
    assert!(matches!(
        &seen[done_at(&seen, 1)],
        AgentEvent::CommandDone {
            outcome: CommandOutcome::Cancelled,
            ..
        }
    ));
    gates.release("k");
}

#[tokio::test]
async fn a_plugin_command_sent_to_a_child_is_refused_with_its_request() {
    let env = Env::new("commands-child");
    let (gates, calls) = (Gates::default(), Calls::default());
    let mut live = Live::start(plain(Fake::new(Vec::new()), &env, &gates, &calls));
    live.send(ClientCommand::Child {
        agent: wisp_protocol::AgentPath("/worker".to_owned()),
        command: Box::new(run(1, "note", "x")),
    })
    .await;
    let rejected = live
        .until("the refusal", |e| matches!(e, AgentEvent::Rejected { .. }))
        .await;
    let AgentEvent::Rejected { request, .. } = rejected else {
        unreachable!()
    };
    assert_eq!(request, Some(RequestId(1)));
    live.close().await;
    assert!(ran_names(&calls).is_empty());
}

fn rewind(turn: u64, request: u64) -> ClientCommand {
    ClientCommand::Rewind {
        to: wisp_protocol::RewindPoint::Turn { turn },
        request: RequestId(request),
    }
}

#[tokio::test]
async fn a_rewind_cancels_the_commands_that_arrived_after_its_point_and_keeps_earlier_ones() {
    let env = Env::new("commands-rewind");
    let (gates, calls) = (Gates::default(), Calls::default());
    let provider = Fake::new(vec![says("one"), says("two"), says("three")]);
    let mut live = Live::start(plain(provider, &env, &gates, &calls));
    live.send(ClientCommand::send("a", "a")).await;
    live.ended().await;
    live.send(run(1, "wait", "ka send CA")).await;
    gates.started("ka").await;
    live.send(ClientCommand::send("b", "b")).await;
    live.ended().await;
    live.send(run(2, "wait", "kb send CB")).await;
    live.until("the second's wait", |e| {
        matches!(e, AgentEvent::CommandDeferred { .. })
    })
    .await;
    live.send(rewind(2, 10)).await;
    assert_eq!(live.answer(2).await, CommandOutcome::Cancelled);
    gates.release("ka");
    assert_eq!(live.answer(1).await, sent(1));
    live.ended().await;
    live.send(run(3, "wait", "kc send CC")).await;
    gates.started("kc").await;
    live.send(rewind(3, 11)).await;
    assert_eq!(live.answer(3).await, CommandOutcome::Cancelled);
    gates.release("kc");
    live.quiet(300).await;
    let seen = live.close().await;
    assert_eq!(answers(&seen), [2, 1, 3]);
    assert!(accepted(&seen, "wisp-command-2").is_none(), "{seen:#?}");
    assert_eq!(ran_names(&calls), ["wait", "wait"]);
}

struct Saves(Arc<Notify>);

impl crate::settings::host::SettingsHost for Saves {
    fn save(
        &self,
        _edits: Vec<crate::config_file::SiteEdit>,
    ) -> futures::future::BoxFuture<
        '_,
        Result<crate::settings::host::SavedFile, crate::settings::host::HostError>,
    > {
        Box::pin(async move {
            self.0.notified().await;
            Ok(crate::settings::host::SavedFile {
                path_display: "config.toml".to_owned(),
                written: Vec::new(),
                notes: Vec::new(),
            })
        })
    }

    fn read(
        &self,
    ) -> futures::future::BoxFuture<
        '_,
        Result<crate::settings::host::SavedRead, crate::settings::host::HostError>,
    > {
        Box::pin(async { Err(crate::settings::host::HostError("unread".to_owned())) })
    }
}

#[tokio::test]
async fn a_command_finishing_while_a_save_is_in_flight_leaves_the_save_its_own_answer() {
    let env = Env::new("commands-save");
    let (gates, calls) = (Gates::default(), Calls::default());
    let saving = Arc::new(Notify::new());
    let agent = plain(Fake::new(Vec::new()), &env, &gates, &calls)
        .with_settings_host(Arc::new(Saves(saving.clone())));
    let mut live = Live::start(agent);
    live.send(run(1, "wait", "k note x")).await;
    gates.started("k").await;
    live.send(ClientCommand::ChangeSettings {
        request: RequestId(2),
        changes: vec![
            wisp_protocol::settings::SettingChange::named(
                "effort",
                wisp_protocol::settings::SettingValue::Text("low".into()),
            )
            .unwrap(),
        ],
        save: true,
    })
    .await;
    live.quiet(100).await;
    gates.release("k");
    assert_eq!(live.answer(1).await, note("x"));
    saving.notify_one();
    live.until("the save's answer", |e| {
        matches!(
            e,
            AgentEvent::SettingsChanged {
                answers: Some(RequestId(2)),
                ..
            }
        )
    })
    .await;
    live.close().await;
}

fn logged_agent(provider: Arc<dyn Provider>, env: &Env, store: &MemoryStore) -> Agent {
    let context = log_context();
    Agent::new(provider, default_session())
        .with_clock(TickingClock::new())
        .with_platform("Linux")
        .with_resources(None)
        .with_tools(crate::plugin::testing::toolbox(Vec::new()), env.workspace())
        .with_log(SessionLog::new(Box::new(store.clone()), "s1"), context)
}

#[tokio::test]
async fn a_reply_after_a_resume_gets_a_fresh_id() {
    let env = Env::new("commands-resume");
    let (gates, calls) = (Gates::default(), Calls::default());
    let store = MemoryStore::default();
    let source = || vec![kit_source("", kit::none(), &gates, &calls)];
    let agent = with_test_hooks(
        logged_agent(Fake::new(vec![says("one")]), &env, &store),
        source(),
    );
    let mut live = Live::start(agent);
    live.send(run(1, "send", "C")).await;
    assert_eq!(live.answer(1).await, sent(1));
    live.ended().await;
    live.close().await;
    let records = records(&store);
    let resumed = logged_agent(Fake::new(vec![says("two")]), &env, &store)
        .resume(&records, 0)
        .unwrap();
    let mut live = Live::start(with_test_hooks(resumed, source()));
    live.send(run(2, "send", "D")).await;
    assert_eq!(live.answer(2).await, sent(2));
    live.ended().await;
    live.close().await;
    let ids: Vec<String> = log_events(&store)
        .into_iter()
        .filter_map(|e| match e {
            LogEvent::InputAccepted { id, .. } if id.0.starts_with("wisp-command-") => Some(id.0),
            _ => None,
        })
        .collect();
    assert_eq!(ids, ["wisp-command-1", "wisp-command-2"]);
}

fn renders(calls: &Calls) -> usize {
    calls.rendered("sec", wisp_plugin_api::slots::SlotView::Panel)
}

#[tokio::test]
async fn with_no_hook_a_command_mark_renders_after_a_plugin_command_context_and_a_call() {
    let env = Env::new("commands-render");
    let (gates, calls, drawn) = (Gates::default(), Calls::default(), Calls::default());
    let count = Arc::new(AtomicUsize::new(0));
    let render: kit::Render = Arc::new(move |_, _, _| {
        let n = count.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Ok(kit::line(&format!("drawn {n}"))) })
    });
    let section = (
        kit::slotted("aa", &kit::section("sec", "marks = [\"command\"]\n")),
        kit::sections(kit::none(), render, drawn.clone()),
    );
    let provider = Fake::new(vec![calls_gate(None), says("done")]);
    let agent = build(
        provider,
        &env,
        vec![section, kit_source("", kit::none(), &gates, &calls)],
        &gates,
    );
    let mut live = Live::start(agent);
    let content = |e: &AgentEvent| matches!(e, AgentEvent::SlotContent { .. });
    live.send(ClientCommand::SlotsShown {
        sections: vec!["aa/sec".to_owned()],
        pages: Vec::new(),
    })
    .await;
    live.until("the first render", content).await;
    live.quiet(200).await;
    assert_eq!(renders(&drawn), 1);
    live.send(run(1, "note", "x")).await;
    live.answer(1).await;
    live.until("the render after the command", content).await;
    live.quiet(200).await;
    assert_eq!(renders(&drawn), 2);
    live.send(ClientCommand::ContextReport {
        detail: wisp_protocol::context::ReportDetail::Summary,
        request: RequestId(2),
    })
    .await;
    live.until("the render after context", content).await;
    live.quiet(200).await;
    assert_eq!(renders(&drawn), 3);
    live.send(run(3, "wait", "k note y")).await;
    gates.started("k").await;
    live.send(ClientCommand::send("m", "go")).await;
    gates.started("tool").await;
    gates.release("k");
    live.answer(3).await;
    live.quiet(200).await;
    assert_eq!(renders(&drawn), 3);
    gates.release("tool");
    live.ended().await;
    live.quiet(200).await;
    assert_eq!(renders(&drawn), 4);
    let seen = live.close().await;
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, AgentEvent::HookRan { point, .. } if point == "command")),
        "{seen:#?}"
    );
}

#[tokio::test]
async fn a_commands_fetch_answers_cancelled_on_esc_and_timed_out_at_its_deadline() {
    use crate::test_guard::http::{Reply as Served, serve};
    let server = serve(vec![Served::Hang, Served::Hang]).await;
    let env = Env::new("commands-fetch");
    let (gates, calls) = (Gates::default(), Calls::default());
    let mut live = Live::start(plain(Fake::new(Vec::new()), &env, &gates, &calls));
    live.send(run(1, "long-fetch", &server.url)).await;
    live.quiet(100).await;
    live.send(ClientCommand::Cancel).await;
    assert_eq!(live.answer(1).await, CommandOutcome::Cancelled);
    live.quiet(100).await;
    assert_eq!(*gates.fetched.lock().unwrap(), ["Some(Cancelled)"]);
    live.send(run(2, "fetch", &server.url)).await;
    assert_eq!(live.answer(2).await, CommandOutcome::TimedOut);
    live.quiet(100).await;
    assert_eq!(
        *gates.fetched.lock().unwrap(),
        ["Some(Cancelled)", "Some(TimedOut)"]
    );
    live.close().await;
}

fn native_compaction() -> Step {
    let raw = json!({"type": "compaction", "content": "So far."});
    let mut script = vec![
        Ok(ProviderEvent::Sending),
        Ok(start_on(DEFAULT_MODEL, 1_200)),
    ];
    script.extend(opaque(0, raw));
    script.extend(ended(
        StopReason::Compaction,
        usage(1_200, 0, 300),
        Vec::new(),
    ));
    Step::from(script)
}

#[tokio::test]
async fn a_compact_that_ran_fires_the_command_point_once_with_its_focus() {
    let env = Env::new("commands-point-compact");
    let (gates, calls) = (Gates::default(), Calls::default());
    let provider = Fake::new(vec![says("one"), native_compaction()]);
    let agent = hooked_at(provider, &env, "command", kit::none(), &gates, &calls);
    let mut live = Live::start(agent);
    live.send(ClientCommand::send("m", "a")).await;
    live.ended().await;
    live.send(ClientCommand::Compact {
        focus: Some("the parser".to_owned()),
        request: RequestId(2),
    })
    .await;
    live.until("the compaction", |e| {
        matches!(e, AgentEvent::Compacted { .. })
    })
    .await;
    live.until(
        "the command hook",
        |e| matches!(e, AgentEvent::HookRan { point, .. } if point == "command"),
    )
    .await;
    live.quiet(200).await;
    live.close().await;
    let seen: Vec<Event> = calls.of("h").into_iter().map(|c| c.event).collect();
    assert_eq!(
        seen,
        [Event::Command(CommandInfo {
            name: "compact".to_owned(),
            args: "the parser".to_owned()
        })]
    );
}

#[tokio::test]
async fn a_waiting_command_reads_the_settings_the_last_turn_applied_when_it_starts() {
    let env = Env::new("commands-fresh-setting");
    let (gates, calls) = (Gates::default(), Calls::default());
    let make = kit::commanding(kit::none(), runner(gates.clone()), calls.clone());
    let manifest = kit::manifest_in(
        "0.2.0",
        ID,
        &("settings = [\"time\"]\n".to_owned() + &body()),
    );
    let provider = Fake::new(vec![says("one")]);
    let agent = build(provider, &env, vec![(manifest, make)], &gates);
    let mut live = Live::start(agent);
    live.send(run(1, "wait", "k none")).await;
    gates.started("k").await;
    live.send(run(2, "facts", "")).await;
    live.send(ClientCommand::ChangeSettings {
        request: RequestId(3),
        changes: vec![
            wisp_protocol::settings::SettingChange::named(
                "time",
                wisp_protocol::settings::SettingValue::Text("off".into()),
            )
            .unwrap(),
        ],
        save: false,
    })
    .await;
    live.until("the change", |e| {
        matches!(e, AgentEvent::SettingsChanged { .. })
    })
    .await;
    live.send(ClientCommand::send("m", "hi")).await;
    live.ended().await;
    gates.release("k");
    assert_eq!(live.answer(1).await, CommandOutcome::None);
    assert_eq!(live.answer(2).await, note("off"));
    live.close().await;
}

#[tokio::test]
async fn esc_during_a_call_cancels_the_commands_waiting_for_it_and_none_runs() {
    let env = Env::new("commands-esc-call");
    let (gates, calls) = (Gates::default(), Calls::default());
    let provider = Fake::new(vec![calls_gate(None), says("done")]);
    let mut live = Live::start(plain(provider, &env, &gates, &calls));
    live.send(ClientCommand::send("m", "hi")).await;
    gates.started("tool").await;
    live.send(run(1, "note", "a")).await;
    live.send(run(2, "note", "b")).await;
    live.until(
        "the second wait",
        |e| matches!(e, AgentEvent::CommandDeferred { request, .. } if request.0 == 2),
    )
    .await;
    live.send(ClientCommand::Cancel).await;
    assert_eq!(live.answer(1).await, CommandOutcome::Cancelled);
    assert_eq!(live.answer(2).await, CommandOutcome::Cancelled);
    gates.release("tool");
    live.ended().await;
    live.quiet(300).await;
    let seen = live.close().await;
    assert_eq!(answers(&seen), [1, 2]);
    assert!(ran_names(&calls).is_empty(), "{:?}", ran_names(&calls));
}

#[tokio::test]
async fn a_rewind_to_a_turn_cancels_a_command_held_by_its_opening_hook() {
    let env = Env::new("commands-rewind-held");
    let (gates, calls) = (Gates::default(), Calls::default());
    let agent = hooked_at(
        Fake::new(vec![says("one")]),
        &env,
        "user-input",
        gated_hook(&gates, "input"),
        &gates,
        &calls,
    );
    let mut live = Live::start(agent);
    live.send(ClientCommand::send("m", "go")).await;
    gates.started("input").await;
    live.send(run(1, "wait", "k none")).await;
    live.until("the wait", |e| {
        matches!(e, AgentEvent::CommandDeferred { .. })
    })
    .await;
    gates.release("input");
    gates.started("k").await;
    live.ended().await;
    live.send(rewind(1, 10)).await;
    assert_eq!(live.answer(1).await, CommandOutcome::Cancelled);
    gates.release("k");
    live.quiet(300).await;
    let seen = live.close().await;
    assert_eq!(answers(&seen), [1]);
}

#[tokio::test]
async fn a_rewind_to_a_turn_that_took_mail_along_cancels_a_command_sent_after_the_mail() {
    let env = Env::new("commands-rewind-mail");
    let (gates, calls) = (Gates::default(), Calls::default());
    let gate = Arc::new(Notify::new());
    let provider = Fake::new(vec![held("one", &gate), says("two")]);
    let agent = plain(provider, &env, &gates, &calls);
    let mail = agent.mailbox.sender();
    let mut live = Live::start(agent);
    live.send(ClientCommand::send("a", "a")).await;
    live.until("the first turn", |e| {
        matches!(e, AgentEvent::TurnStarted { .. })
    })
    .await;
    mail.send(crate::agent::mail::Mail {
        id: wisp_protocol::agents::MailId("1".into()),
        from: wisp_protocol::agents::AgentPath::new("/c"),
        to: wisp_protocol::agents::AgentPath::root(),
        kind: wisp_protocol::agents::MailKind::Message,
        wake: false,
        text: "a letter".into(),
    })
    .unwrap();
    live.until(
        "the mail",
        |e| matches!(e, AgentEvent::InputAccepted { text, .. } if text.contains("a letter")),
    )
    .await;
    live.send(run(1, "wait", "k none")).await;
    gates.started("k").await;
    gate.notify_one();
    live.ended().await;
    live.send(ClientCommand::send("b", "b")).await;
    live.ended().await;
    live.send(rewind(2, 10)).await;
    assert_eq!(live.answer(1).await, CommandOutcome::Cancelled);
    gates.release("k");
    live.quiet(300).await;
    let seen = live.close().await;
    assert_eq!(answers(&seen), [1]);
}

#[tokio::test]
async fn closing_during_a_turn_start_input_hook_answers_a_finished_reply_cancelled() {
    let env = Env::new("commands-pre-turn-close");
    let (gates, calls) = (Gates::default(), Calls::default());
    let agent = hooked_at(
        Fake::new(vec![says("one")]),
        &env,
        "user-input",
        gated_hook(&gates, "input"),
        &gates,
        &calls,
    );
    let mut live = Live::start(agent);
    live.send(run(1, "wait", "k send C")).await;
    gates.started("k").await;
    live.send(ClientCommand::send("m", "go")).await;
    gates.started("input").await;
    gates.release("k");
    live.quiet(200).await;
    assert!(answers(&live.seen).is_empty(), "{:#?}", live.seen);
    let seen = live.close().await;
    assert_eq!(answers(&seen), [1], "{seen:#?}");
    let outcome = seen.iter().find_map(|e| match e {
        AgentEvent::CommandDone { outcome, .. } => Some(outcome.clone()),
        _ => None,
    });
    assert_eq!(outcome, Some(CommandOutcome::Cancelled));
    assert!(accepted(&seen, "wisp-command-1").is_none(), "{seen:#?}");
}

#[tokio::test]
async fn a_rewind_to_a_wake_turn_cancels_a_command_sent_during_it() {
    let a = ExitGate::new("a.go", 2);
    let env = Env::new("commands-rewind-wake");
    let (gates, calls) = (Gates::default(), Calls::default());
    let gate = Arc::new(Notify::new());
    let background = uses(&[call(
        "a",
        "bash",
        json!({"command": &a.command, "run_in_background": true}),
    )]);
    let provider = Fake::new(vec![background, says("started"), held("woke", &gate)]);
    let agent = Agent::new(provider, default_session())
        .with_clock(TickingClock::new())
        .with_tools(
            crate::tool::testkit::shell(crate::tool::testkit::timing(300, 3_000)),
            env.workspace(),
        )
        .with_platform("Linux")
        .with_resources(None);
    let agent = with_test_hooks(agent, vec![kit_source("", kit::none(), &gates, &calls)]);
    let mut live = Live::start(agent);
    live.send(ClientCommand::send("m", "go")).await;
    live.ended().await;
    a.open(&env.work);
    live.until("the wake turn", |e| {
        matches!(
            e,
            AgentEvent::TurnStarted {
                origin: Some(wisp_protocol::TurnOrigin::ProcessExit),
                ..
            }
        )
    })
    .await;
    live.send(run(1, "wait", "k none")).await;
    gates.started("k").await;
    gate.notify_one();
    live.ended().await;
    live.send(rewind(2, 10)).await;
    assert_eq!(live.answer(1).await, CommandOutcome::Cancelled);
    gates.release("k");
    live.quiet(300).await;
    let seen = live.close().await;
    assert_eq!(answers(&seen), [1]);
}

#[tokio::test]
async fn a_rewind_to_a_turn_from_before_a_resume_cancels_every_command() {
    let env = Env::new("commands-rewind-resumed");
    let (gates, calls) = (Gates::default(), Calls::default());
    let store = MemoryStore::default();
    let source = || vec![kit_source("", kit::none(), &gates, &calls)];
    let agent = with_test_hooks(
        logged_agent(Fake::new(vec![says("one")]), &env, &store),
        source(),
    );
    let mut live = Live::start(agent);
    live.send(ClientCommand::send("a", "a")).await;
    live.ended().await;
    live.close().await;
    let records = records(&store);
    let resumed = logged_agent(Fake::new(Vec::new()), &env, &store)
        .resume(&records, 0)
        .unwrap();
    let mut live = Live::start(with_test_hooks(resumed, source()));
    live.send(run(1, "wait", "k none")).await;
    gates.started("k").await;
    live.send(rewind(1, 10)).await;
    assert_eq!(live.answer(1).await, CommandOutcome::Cancelled);
    gates.release("k");
    live.quiet(300).await;
    let seen = live.close().await;
    assert_eq!(answers(&seen), [1]);
}

fn refused_with(seen: &[AgentEvent], request: u64) -> Option<(RejectKind, String)> {
    seen.iter().find_map(|e| match e {
        AgentEvent::Rejected {
            kind: Some(kind),
            request: Some(r),
            reason,
            ..
        } if r.0 == request => Some((kind.clone(), reason.clone())),
        _ => None,
    })
}

#[tokio::test]
async fn a_reply_between_turns_ends_the_retry_offer_and_refuses_a_waiting_rewind() {
    let env = Env::new("commands-offer");
    let (gates, calls) = (Gates::default(), Calls::default());
    let saving = Arc::new(Notify::new());
    let error = classified(
        ErrorKind::Server,
        "server happened",
        FailurePhase::Response,
        RetryClass::Never,
    );
    let provider = Fake::new(vec![
        Step::from(vec![Ok(ProviderEvent::Sending), Err(error)]),
        says("later"),
    ]);
    let agent =
        plain(provider, &env, &gates, &calls).with_settings_host(Arc::new(Saves(saving.clone())));
    let mut live = Live::start(agent);
    live.send(ClientCommand::send("m", "go")).await;
    live.ended().await;
    live.send(ClientCommand::ChangeSettings {
        request: RequestId(2),
        changes: vec![
            wisp_protocol::settings::SettingChange::named(
                "effort",
                wisp_protocol::settings::SettingValue::Text("low".into()),
            )
            .unwrap(),
        ],
        save: true,
    })
    .await;
    live.send(ClientCommand::Retry {
        turn: 1,
        endpoint: None,
        request: RequestId(3),
    })
    .await;
    live.send(rewind(1, 4)).await;
    live.quiet(100).await;
    live.send(run(1, "send", "instead")).await;
    assert_eq!(live.answer(1).await, sent(1));
    saving.notify_one();
    live.ended().await;
    live.quiet(200).await;
    let seen = live.close().await;
    assert_eq!(
        refused_with(&seen, 4),
        Some((
            RejectKind::NotRewindable,
            wisp_protocol::SENT_BEFORE_REWIND.to_owned()
        ))
    );
    assert_eq!(
        refused_with(&seen, 3),
        Some((
            RejectKind::NotRetryable,
            crate::agent::choice::SENT_AFTER.to_owned()
        ))
    );
}

#[tokio::test]
async fn a_between_turns_command_during_a_manual_compaction_is_refused() {
    let env = Env::new("commands-busy-compact");
    let (gates, calls) = (Gates::default(), Calls::default());
    let gate = Arc::new(Notify::new());
    let provider = Fake::new(vec![
        says("one"),
        native_compaction().held(Wait::Notified(gate.clone())),
    ]);
    let mut live = Live::start(plain(provider, &env, &gates, &calls));
    live.send(ClientCommand::send("m", "a")).await;
    live.ended().await;
    live.send(ClientCommand::Compact {
        focus: None,
        request: RequestId(2),
    })
    .await;
    live.until("the compaction's start", |e| {
        matches!(e, AgentEvent::CompactionStarted { .. })
    })
    .await;
    live.send(run(1, "tidy", "")).await;
    assert!(matches!(
        live.answer(1).await,
        CommandOutcome::Refused {
            kind: CommandRefusal::Busy,
            ..
        }
    ));
    gate.notify_one();
    live.until("the compaction", |e| {
        matches!(e, AgentEvent::Compacted { .. })
    })
    .await;
    live.send(run(3, "tidy", "")).await;
    assert_eq!(live.answer(3).await, note("tidied"));
    live.close().await;
    assert_eq!(ran_names(&calls), ["tidy"]);
}

fn gated_once(gates: &Gates, key: &'static str) -> kit::Section {
    let gates = gates.clone();
    let once = Arc::new(std::sync::atomic::AtomicBool::new(true));
    Arc::new(move |_, _| {
        let gates = gates.clone();
        let first = once.swap(false, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move {
            if first {
                let (started, release) = gates.pair(key);
                started.notify_one();
                release.notified().await;
            }
            wisp_plugin_api::prompt::Rendered {
                snapshot: "s".to_owned(),
                text: None,
            }
        })
    })
}

#[tokio::test]
async fn a_command_sent_during_a_section_render_waits_for_it_and_runs_after() {
    let env = Env::new("commands-during-section");
    let (gates, calls) = (Gates::default(), Calls::default());
    let provider = Fake::new(vec![says("done")]);
    let notes: Make = kit::prompted(kit::none(), gated_once(&gates, "section"), Calls::default());
    let sources = vec![
        kit_source("", kit::none(), &gates, &calls),
        (
            kit::manifest_in("0.2.0", "bb", &kit::prompt_section("notes", "lead", "")),
            notes,
        ),
    ];
    let mut live = Live::start(build(provider, &env, sources, &gates));
    live.send(ClientCommand::send("m", "hi")).await;
    gates.started("section").await;
    live.send(run(1, "note", "a")).await;
    live.until(
        "the wait",
        |e| matches!(e, AgentEvent::CommandDeferred { request, .. } if request.0 == 1),
    )
    .await;
    assert!(ran_names(&calls).is_empty());
    gates.release("section");
    assert_eq!(live.answer(1).await, note("a"));
    live.ended().await;
    let seen = live.close().await;
    assert_eq!(deferrals(&seen), [(1, CommandWait::Hook)]);
    assert_eq!(ran_names(&calls), ["note"]);
}

mod switches;

use crate::mcp_test as mcp_http;

#[tokio::test]
async fn mcp_sign_in_keeps_its_private_link_and_typed_code_out_of_the_session_log_and_hooks() {
    let env = Env::new("commands-mcp-sign-in");
    let server = mcp_http::Server::start(true, None).await;
    let service = crate::mcp::Service::new(
        BTreeMap::new(),
        env.root.clone(),
        Some(env.root.join("mcp")),
    )
    .with_config(
        env.root.join("config.toml"),
        crate::endpoints::PathShown::Given,
    );
    service
        .add(
            "local",
            crate::config::McpServer {
                keys: Vec::new(),
                command: server.address(),
                args: Vec::new(),
            },
        )
        .await
        .unwrap();
    let (gates, calls) = (Gates::default(), Calls::default());
    let (manifest, make) = kit_source(&hook_body("h", "command", ""), kit::none(), &gates, &calls);
    let sources = vec![
        crate::plugin::Source::bundled(wisp_plugin_mcp::bundled()),
        crate::plugin::Source::in_process(
            manifest,
            make,
            wisp_plugin_api::manifest::Origin::Bundled,
        ),
    ];
    let (registry, problems) =
        crate::plugin::Registry::from_origins(sources, &crate::plugin::PluginConfig::default());
    assert!(problems.is_empty());
    let host = Arc::new(crate::plugin::KernelHost::new());
    host.set_mcp(service.clone());
    let started = registry.start(&BTreeSet::new(), host).await;
    assert!(started.problems.is_empty());
    let store = MemoryStore::default();
    let agent = logged_agent(Fake::new(Vec::new()), &env, &store)
        .with_tools(started.toolbox, env.workspace())
        .with_hooks(started.hooks, started.host);
    let mut live = Live::start(agent);
    let command = |request, args| ClientCommand::RunPluginCommand {
        name: "mcp:mcp".into(),
        args: wisp_protocol::Secret::new(args),
        request: RequestId(request),
    };
    live.send(command(1, "sign-in local".to_owned())).await;
    let answer = live.answer(1).await;
    assert!(matches!(answer, CommandOutcome::Lines { .. }));
    assert!(
        !serde_json::to_string(&answer)
            .unwrap()
            .contains(mcp_http::CLIENT)
    );
    let link = service.sign_in_start("local").await.unwrap().unwrap();
    let address = mcp_http::redirected(&link, None).replace("code=", "%63ode=");
    let typed = format!("sign-in local '{address}' # {address}");
    live.send(command(2, typed.clone())).await;
    let answer = live.answer(2).await;
    assert_eq!(answer, CommandOutcome::None);
    assert!(!serde_json::to_string(&answer).unwrap().contains("CANARY"));
    live.quiet(100).await;
    let seen = live.close().await;
    let ended = |event: &AgentEvent| matches!(event, AgentEvent::PluginsChanged { notes, .. } if notes.iter().any(|note| note.plugin == "mcp" && note.message == "local signed in · 1 tool"));
    assert_eq!(seen.iter().filter(|event| ended(event)).count(), 1);
    assert!(
        seen.iter().position(ended).unwrap()
            < seen
                .iter()
                .position(|event| matches!(
                    event,
                    AgentEvent::CommandDone {
                        answers: RequestId(2),
                        ..
                    }
                ))
                .unwrap()
    );
    assert!(
        !seen
            .iter()
            .any(|event| matches!(event, AgentEvent::InputAccepted { .. }))
    );
    let logged_records = records(&store);
    assert_eq!(logged_records.iter().filter(|record| matches!(&record.event, LogEvent::PluginsChanged { notes, .. } if notes.iter().any(|note| note.message == "local signed in · 1 tool"))).count(), 1);
    let logged = serde_json::to_string(&logged_records).unwrap();
    let links: Vec<_> = seen
        .iter()
        .enumerate()
        .flat_map(|(index, event)| match event {
            AgentEvent::PluginsChanged { links, .. } => links
                .iter()
                .flatten()
                .map(move |note| (index, note))
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].1.link.expose(), link);
    assert!(links[0].1.help.is_empty());
    assert!(
        links[0].0
            < seen
                .iter()
                .position(|event| matches!(
                    event,
                    AgentEvent::CommandDone {
                        answers: RequestId(1),
                        ..
                    }
                ))
                .unwrap()
    );
    let safe_events = serde_json::to_string(
        &seen
            .into_iter()
            .filter_map(AgentEvent::without_links)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    for private in [
        link.as_str(),
        mcp_http::CLIENT,
        mcp_http::CLIENT_SECRET,
        mcp_http::ACCESS,
        mcp_http::REFRESH,
        mcp_http::CODE,
    ] {
        assert!(!logged.contains(private));
        assert!(!safe_events.contains(private));
    }
    let hooks: Vec<_> = calls
        .of("h")
        .into_iter()
        .filter_map(|call| match call.event {
            Event::Command(info) => Some(info.args),
            _ => None,
        })
        .collect();
    assert_eq!(hooks.len(), 2);
    assert_eq!(hooks[1], wisp_protocol::masked_command(&typed).unwrap());
    assert!(!hooks[1].contains(mcp_http::CODE));
}

#[tokio::test]
async fn held_command_args_never_reach_observers_even_without_a_plugin_host() {
    const HELD: &str = "CANARY-held-local-command-argument";
    crate::session::register_secrets([HELD.to_owned()]);
    let env = Env::new("command-args-check");
    let (gates, calls) = (Gates::default(), Calls::default());
    let mut agent = with_test_hooks(
        logged_agent(Fake::new(vec![]), &env, &MemoryStore::default()),
        vec![kit_source(
            &hook_body("h", "command", ""),
            kit::none(),
            &gates,
            &calls,
        )],
    );
    agent.hooks.host = None;
    agent.defer_command("testkit:none".into(), HELD.into());
    let home = env.workspace().home.unwrap();
    agent.defer_command(
        "testkit:none".into(),
        home.join("sample").display().to_string(),
    );
    assert!(agent.hooks.deferred.is_empty());
    agent.defer_command("testkit:none".into(), "tile".into());
    assert!(
        matches!(agent.hooks.deferred.front(), Some(crate::agent::hooks::Deferred::Point { event: Event::Command(info), .. }) if info.args == "tile")
    );
}

#[tokio::test]
async fn command_observers_in_both_drains_omit_args_held_after_enqueue() {
    for (in_turn, word) in [
        (false, "CANARY-late-idle-command-arg"),
        (true, "CANARY-late-turn-command-arg"),
    ] {
        let env = Env::new("late-command-args");
        let (gates, calls) = (Gates::default(), Calls::default());
        let mut agent = with_test_hooks(
            logged_agent(Fake::new(vec![]), &env, &MemoryStore::default()),
            vec![kit_source(
                &hook_body("h", "command", ""),
                kit::none(),
                &gates,
                &calls,
            )],
        );
        agent.hooks.host = None;
        assert!(agent.lookup_is_safe(word));
        agent.defer_command("testkit:none".into(), word.into());
        agent.defer_command("testkit:none".into(), "tile".into());
        assert_eq!(agent.hooks.deferred.len(), 2);
        crate::session::register_secrets([word.to_owned()]);
        assert!(!agent.lookup_is_safe(word));
        let (events, _received) = mpsc::channel(64);
        if in_turn {
            let (_send, mut commands) = mpsc::channel(1);
            agent.track = Some(crate::agent::TurnTrack::new(1));
            agent.drain_deferred(&mut commands, &events).await;
        } else {
            agent.drain_idle(&events).await;
        }
        let observed: Vec<_> = calls
            .of("h")
            .into_iter()
            .filter_map(|call| match call.event {
                Event::Command(info) => Some(info.args),
                _ => None,
            })
            .collect();
        assert_eq!(observed, ["tile"]);
        assert!(agent.hooks.deferred.is_empty());
    }
}

#[tokio::test]
async fn a_later_command_observer_does_not_receive_args_held_by_the_first() {
    const HELD: &str = "CANARY-held-between-command-observers";
    let env = Env::new("between-command-observers");
    let first = Calls::default();
    let second = Calls::default();
    let source = crate::plugin::testing::hook_source;
    let mut agent = with_test_hooks(
        logged_agent(Fake::new(vec![]), &env, &MemoryStore::default()),
        vec![
            source(
                "first-observer",
                &hook_body("first", "command", ""),
                Arc::new(|_, _, event| {
                    Box::pin(async move {
                        if matches!(event, Event::Command(info) if info.args == HELD) {
                            crate::session::register_secrets([HELD.to_owned()]);
                        }
                        Reply::None
                    })
                }),
                &first,
            ),
            source(
                "second-observer",
                &hook_body("second", "command", "after = [\"first-observer/first\"]\n"),
                kit::none(),
                &second,
            ),
        ],
    );
    assert!(agent.lookup_is_safe(HELD));
    agent.defer_command("testkit:none".into(), HELD.into());
    agent.defer_command("testkit:none".into(), "tile".into());
    let (events, _received) = mpsc::channel(64);
    agent.drain_idle(&events).await;
    assert!(!agent.lookup_is_safe(HELD));
    let args = |calls: &Calls, hook| {
        calls
            .of(hook)
            .into_iter()
            .filter_map(|call| match call.event {
                Event::Command(info) => Some(info.args),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(args(&first, "first"), [HELD, "tile"]);
    assert_eq!(args(&second, "second"), ["tile"]);
}
