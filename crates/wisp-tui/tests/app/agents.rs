use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::time::sleep;
use wisp_protocol::slot::{NodeState, SlotBlock, SlotEntry, SlotFlow, SlotNoun, SlotTopologyNode};
use wisp_protocol::{
    Activity, AgentEntry, AgentEvent, AgentPath, AgentState, ClientCommand, ContextFill, Cost,
    DeliveryMode, Effort, Fact, HoldReason, IdleOutcome, InputId, MailFrom, MailId, MailKind,
    PresetChange, QueueReason, ReportOutcome, RequestId, TodoProgress, ToolAction, ToolOutcome,
    ToolView, TurnOrigin, TurnStats, Verbs, WakeSource, render_mail, report_text,
};
use wisp_render::chrome::EndpointFacts;
use wisp_render::theme::glyphs::{GlyphSet, UNICODE};
use wisp_render::theme::{Depth, Mode, Theme};
use wisp_render::transcript::copy;
use wisp_tui::app::highlight::Start;
use wisp_tui::app::resumed::Resumed;
use wisp_tui::app::sidebar::AGENTS_SECTION;
use wisp_tui::app::{AGAIN_TO_QUIT, Exit, Finished, Options, Session, SessionFacts, replay, run};
use wisp_tui::term::{Input, Key, KeyCode, RESIZE_DEBOUNCE};

use super::folds::click;
use super::images::{NO_LEAKS, leaks, paste_every_way, pasting, seeing};
use super::pages::now_ms;
use super::scenes::Seen;
use super::select::locate;
use super::sidebar::{content, drain, slot, snapshot};
use super::steering::{Agent, agent_session, cancelled, failed, steering_facts, steering_on};
use super::{Screen, completed, facts, key, options, session_with};
use crate::agent_keys::{bash, child, none_with, screen, take_over, watch, wrapped};
use crate::events::{
    End, Requested, cost, show_finished, show_requested, tool_requested, turn_ended,
};
use crate::shared::{catalog, plugin};
use crate::{Drive, frozen};

const FIXTURE: &str = include_str!("../fixtures/agents.jsonl");
const SEP: &str = " \u{b7} ";

pub(super) fn on_account(path: &str, account: &str, used: u64) -> AgentEntry {
    AgentEntry {
        endpoint: Some(account.to_owned()),
        provider: Some("anthropic".to_owned()),
        context: Some(ContextFill {
            used,
            window: None,
            limit: None,
            estimated: false,
            auto_compact: false,
        }),
        ..entry(path, idle(None), 0.0)
    }
}

pub(super) fn entry(path: &str, state: AgentState, spent: f64) -> AgentEntry {
    let path = AgentPath::new(path);
    AgentEntry {
        plan_usd: None,
        billing: Default::default(),
        name: path.name().to_owned(),
        session_id: format!("019a-{}", path.name()),
        parent: path.parent(),
        preset: None,
        model: "claude-opus-5-5".to_owned(),
        effort: Effort::High,
        depth: path.depth(),
        state,
        activity: Activity::Idle,
        spent_usd: spent,
        lower_bound: false,
        accounts: Default::default(),
        checklist: None,
        last_report: None,
        cwd: Some(format!("~/Projects/Wisp-worktrees/{}", path.name())),
        running_ms: None,
        since_ms: Some(now_ms() - 2_000),
        context: None,
        keep_warm: None,
        endpoint: None,
        provider: None,
        priced: true,
        path,
        preset_change: None,
    }
}

pub(super) fn spawned(entry: AgentEntry) -> AgentEvent {
    AgentEvent::AgentSpawned {
        request: None,
        session_id: entry.session_id.clone(),
        agent: entry,
        config: wisp_protocol::SpawnChoices::default(),
    }
}

pub(super) fn changed(path: &str, state: AgentState, activity: Activity, spent: f64) -> AgentEvent {
    AgentEvent::AgentStateChanged {
        plan_usd: None,
        billing: Default::default(),
        agent: AgentPath::new(path),
        state,
        activity,
        spent_usd: spent,
        lower_bound: false,
        accounts: Default::default(),
        since_ms: Some(now_ms()),
    }
}

pub(super) fn spent(entry: &AgentEntry, usd: f64, lower: bool) -> AgentEvent {
    let total = wisp_protocol::AccountSpend::of(usd, entry.plan_usd, entry.billing, lower);
    AgentEvent::AgentStateChanged {
        plan_usd: total.plan_usd,
        billing: entry.billing,
        agent: entry.path.clone(),
        state: entry.state,
        activity: entry.activity.clone(),
        spent_usd: usd,
        lower_bound: lower,
        accounts: entry
            .endpoint
            .iter()
            .map(|name| (name.clone(), total))
            .collect(),
        since_ms: entry.since_ms,
    }
}

pub(super) fn main_spent(
    total: (f64, Option<f64>, bool),
    split: &[(&str, f64, Option<f64>)],
) -> AgentEvent {
    let spend = |usd, plan_usd, lower_bound| wisp_protocol::AccountSpend {
        usd,
        plan_usd,
        lower_bound,
    };
    let accounts = split
        .iter()
        .map(|(name, usd, plan)| ((*name).to_owned(), spend(*usd, *plan, false)));
    AgentEvent::SpendChanged {
        total: spend(total.0, total.1, total.2),
        accounts: accounts.collect(),
    }
}

pub(super) fn with_spend(event: AgentEvent) -> Vec<AgentEvent> {
    let cost = match &event {
        AgentEvent::TurnEnded { outcome, .. } => outcome.cost(),
        AgentEvent::Compacted { cost, .. }
        | AgentEvent::CompactionPrepared { cost, .. }
        | AgentEvent::CompactionFailed { cost, .. }
        | AgentEvent::CacheRefreshed { cost, .. }
        | AgentEvent::SessionNamed { cost, .. } => *cost,
        _ => None,
    };
    let spent = cost.map(|c| {
        let (usd, plan, lower) = (c.session_usd, c.session_plan_usd, c.session_lower_bound);
        let s = wisp_protocol::AccountSpend::of(usd, plan, c.session_billing, lower);
        main_spent((s.usd, s.plan_usd, s.lower_bound), &[])
    });
    spent.into_iter().chain([event]).collect()
}

pub(super) fn reported(path: &str, text: &str) -> AgentEvent {
    AgentEvent::AgentReported {
        agent: AgentPath::new(path),
        turn: 1,
        outcome: ReportOutcome::Reported,
        report: Some(Value::String(text.to_owned())),
        cost: Some(0.84),
        duration_ms: 372_000,
    }
}

pub(super) fn idle(outcome: Option<IdleOutcome>) -> AgentState {
    AgentState::Idle { outcome }
}

pub(super) fn mail_id(id: &str) -> InputId {
    InputId(format!("mail-{id}"))
}

pub(super) fn mail(id: &str, from: &str, kind: MailKind, wake: bool, body: &str) -> AgentEvent {
    AgentEvent::InputAccepted {
        opening: false,
        id: mail_id(id),
        mode: DeliveryMode::Steer,
        text: render_mail(&AgentPath::new(from), kind, body),
        images: Vec::new(),
        replaces: None,
        held: false,
        front: false,
        from: Some(MailFrom {
            agent: AgentPath::new(from),
            kind,
            mail: MailId(id.to_owned()),
            wake,
            plugin: None,
        }),
    }
}

pub(super) fn delivered(ids: &[&str], step: u32) -> AgentEvent {
    AgentEvent::InputDelivered {
        ids: ids.iter().map(|id| mail_id(id)).collect(),
        step,
    }
}

pub(super) fn spawn_call(call: &str, name: &str) -> Vec<AgentEvent> {
    vec![
        tool_requested(Requested {
            block: 1,
            call_id: call.to_owned(),
            tool: "spawn_agent".to_owned(),
            input: json!({"name": name, "task": "Review the plan"}),
            action: ToolAction::Other {
                name: "spawn_agent".to_owned(),
            },
            verbs: Some(Verbs {
                doing: "Starting".to_owned(),
                done: "Started".to_owned(),
                failed: "couldn't start".to_owned(),
            }),
            target: name.to_owned(),
            ..Requested::default()
        }),
        AgentEvent::call_started(call, "~/Projects/Wisp"),
        AgentEvent::call_finished(
            call,
            ToolOutcome::Completed {
                is_error: false,
                exit: None,
            },
        ),
    ]
}

pub(super) fn priced_end(session: f64, agents: f64) -> AgentEvent {
    turn_ended(End {
        model: "test-model".to_owned(),
        cost: Some(Cost {
            agents_usd: Some(agents),
            ..cost(0.42, session, false)
        }),
        tool_calls: 1,
        stats: TurnStats {
            duration_ms: 2_100,
            ..TurnStats::default()
        },
        ..End::default()
    })
}

fn row_of(lines: &[String], needle: &str) -> usize {
    lines
        .iter()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} not in {lines:#?}"))
}

fn line_with(lines: &[String], needle: &str) -> String {
    let line = lines
        .iter()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} not in {lines:#?}"));
    super::unframed(line)
}

async fn run_agents<D, F>(resumed: Option<Resumed>, options: Options, drive: D)
where
    D: FnOnce(Agent) -> F,
    F: std::future::Future<Output = ()>,
{
    run_agents_on(screen(), facts(), resumed, options, drive).await;
}

async fn run_agents_on<D, F>(
    screen: Screen,
    facts: SessionFacts,
    resumed: Option<Resumed>,
    options: Options,
    drive: D,
) -> Finished
where
    D: FnOnce(Agent) -> F,
    F: std::future::Future<Output = ()>,
{
    let channel = super::without_terminal(64);
    let finished = agent_session(screen, options, facts, resumed, channel, drive).await;
    assert!(finished.exit.is_ok());
    finished
}

#[tokio::test(start_paused = true)]
async fn a_mail_delivered_mid_turn_is_a_mail_row_never_in_the_waiting_stack_or_recall_and_notes_left_unread_show_when_the_turn_ends()
 {
    steering_on(screen(), options(Start::Stalled), |mut a| async move {
        a.start("first", 1).await;
        let report = report_text("/review reported · 6m12s", "Plan ready: docs/plans/x.md");
        a.emit(mail("m1", "/review", MailKind::Report, true, &report))
            .await;
        let lines = a.settle().await;
        none_with(&lines, "Plan ready");
        none_with(&lines, "next step");
        a.emit(delivered(&["m1"], 2)).await;
        let lines = a.settle().await;
        line_with(&lines, "\u{25C7} report from /review");
        line_with(&lines, "/review reported · 6m12s");
        line_with(&lines, "Plan ready: docs/plans/x.md");
        a.emit(mail("m2", "/docs", MailKind::Message, false, "first note"))
            .await;
        a.emit(mail("m3", "/qa", MailKind::Message, false, "second note"))
            .await;
        none_with(&a.settle().await, "first note");
        a.emit(completed()).await;
        let lines = a.settle().await;
        let at = |needle: &str| row_of(&lines, needle);
        let first = at("note from /docs \u{b7} first note");
        assert!(at("cost unknown") < first, "{lines:#?}");
        assert_eq!(
            at("note from /qa \u{b7} second note"),
            first + 1,
            "{lines:#?}"
        );
        a.key(KeyCode::Up);
        let lines = a.settle().await;
        let composer = line_with(&lines, "first");
        assert!(!composer.contains("wisp:message"), "{lines:#?}");
        a.key(KeyCode::Up);
        let lines = a.settle().await;
        none_with(&lines, "wisp:message");
        a.ctrl('c').ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_strip_figure_moves_with_agent_state_while_the_root_is_idle() {
    steering_on(screen(), options(Start::Stalled), |mut a| async move {
        a.emit(spawned(entry("/review", AgentState::Running, 0.5)))
            .await;
        let lines = a.settle().await;
        let strip = line_with(&lines, "1 agent");
        assert!(strip.contains("1 agent · 1 working"), "{strip}");
        assert!(strip.ends_with("total $0.50+"), "{strip}");
        assert!(!strip.contains("/review"), "{strip}");
        a.start("Review it", 1).await;
        a.emit(AgentEvent::BlockStopped { index: 0 }).await;
        a.emit(completed()).await;
        sleep(Duration::from_secs(3)).await;
        let working = Seen::of(&a.screen);
        assert!(working.marked(), "{:#?}", working.chat);
        a.emit(changed(
            "/review",
            AgentState::Running,
            Activity::Streaming,
            1.25,
        ))
        .await;
        let lines = a.settle().await;
        assert!(
            line_with(&lines, "1 agent").ends_with("total $1.25+"),
            "{lines:#?}"
        );
        a.emit(changed("/review", idle(None), Activity::Idle, 1.5))
            .await;
        let lines = a.settle().await;
        let strip = line_with(&lines, "1 agent");
        assert!(strip.contains("total $1.50"), "{strip}");
        assert!(!strip.contains("working"), "{strip}");
        let idle = Seen::of(&a.screen);
        assert!(!idle.marked(), "{:#?}", idle.chat);
        assert_eq!(idle.chat[idle.chat.len() - 2..], ["", ""]);
        sleep(Duration::from_secs(5)).await;
        let before = (a.screen.sized(), a.screen.frames());
        sleep(Duration::from_secs(10)).await;
        assert_eq!(
            (a.screen.sized(), a.screen.frames()),
            before,
            "no draws after the last agent rests"
        );
        a.emit(changed(
            "/review",
            AgentState::Running,
            Activity::Streaming,
            1.5,
        ))
        .await;
        sleep(Duration::from_millis(300)).await;
        let before = a.screen.frames();
        sleep(Duration::from_secs(2)).await;
        assert!(a.screen.frames() > before, "the agent-work comet restarts");
        a.ctrl('c').ctrl('c').ctrl('c');
    })
    .await;
}

async fn mail_turn_on(a: &Agent, turn: u64, mail: &str) {
    a.emit(mail_turn(turn, None)).await;
    a.emit(AgentEvent::StepStarted { step: 1 }).await;
    a.emit(delivered(&[mail], 1)).await;
}

#[tokio::test(start_paused = true)]
async fn the_chat_moves_only_when_a_row_lands_while_agents_work() {
    for report_first in [true, false] {
        steering_on(screen(), options(Start::Stalled), |mut a| async move {
            let reply = "working on it";
            let words = |seen: &Seen| {
                let last = seen.chat.last().unwrap();
                last.contains(&copy::waiting_for_reports())
            };
            for path in ["/a", "/b"] {
                a.emit(spawned(entry(path, AgentState::Running, 0.1))).await;
            }
            a.start("Review the plan", 1).await;
            a.emit(AgentEvent::BlockStopped { index: 0 }).await;
            a.emit(completed()).await;
            sleep(Duration::from_millis(100)).await;
            let footer = Seen::of(&a.screen);
            assert!(footer.marked(), "{:#?}", footer.chat);
            assert_eq!(footer.mark[0].0, "\u{2822}", "Done's check in step");
            assert!(words(&footer), "{:#?}", footer.chat);
            for _ in 0..6 {
                sleep(Duration::from_millis(500)).await;
                let now = Seen::of(&a.screen);
                assert!(now.marked(), "{:#?}", now.chat);
                assert_eq!(now.row_of(reply), footer.row_of(reply));
            }
            a.emit(reported("/a", "Plan ready")).await;
            a.emit(changed(
                "/a",
                idle(Some(IdleOutcome::Reported)),
                Activity::Idle,
                0.2,
            ))
            .await;
            a.emit(mail(
                "m1",
                "/a",
                MailKind::Report,
                true,
                "/a reported\nPlan ready",
            ))
            .await;
            sleep(Duration::from_millis(100)).await;
            let report = Seen::of(&a.screen);
            assert!(report.marked(), "{:#?}", report.chat);
            assert!(report.row_of(reply) < footer.row_of(reply));
            assert!(report.chat[report.chat.len() - 3].contains("Plan ready"));
            assert!(!words(&report), "{:#?}", report.chat);
            mail_turn_on(&a, 2, "m1").await;
            sleep(Duration::from_millis(100)).await;
            let thinking = Seen::of(&a.screen);
            assert!(thinking.marked(), "{:#?}", thinking.chat);
            let above = report.chat.len() - 1;
            assert_eq!(thinking.chat[..above], report.chat[..above]);
            assert_ne!(thinking.mark, report.mark, "the mark turns to Thinking");
            assert!(!words(&thinking), "{:#?}", thinking.chat);
            a.emit(completed()).await;
            sleep(Duration::from_millis(100)).await;
            let between = Seen::of(&a.screen);
            assert!(between.marked() && words(&between), "{:#?}", between.chat);
            let last_mail = || mail("m2", "/b", MailKind::Report, true, "/b reported\nAll good");
            let last_idle =
                || changed("/b", idle(Some(IdleOutcome::Reported)), Activity::Idle, 0.3);
            if report_first {
                a.emit(last_mail()).await;
                a.emit(last_idle()).await;
            } else {
                a.emit(last_idle()).await;
                sleep(Duration::from_millis(50)).await;
                let gap = Seen::of(&a.screen);
                assert!(!gap.marked(), "{:#?}", gap.chat);
                assert_eq!(gap.chat[gap.chat.len() - 2..], ["", ""], "{:#?}", gap.chat);
                a.emit(last_mail()).await;
            }
            sleep(Duration::from_millis(100)).await;
            let last = Seen::of(&a.screen);
            assert!(last.marked(), "{:#?}", last.chat);
            assert!(last.chat[last.chat.len() - 3].contains("All good"));
            assert!(!words(&last), "{:#?}", last.chat);
            mail_turn_on(&a, 3, "m2").await;
            a.emit(spawned(entry("/c", AgentState::Running, 0.1))).await;
            a.emit(completed()).await;
            sleep(Duration::from_millis(100)).await;
            let stopping = Seen::of(&a.screen);
            assert!(stopping.marked(), "{:#?}", stopping.chat);
            let held = AgentState::Held {
                reason: HoldReason::Stopped,
            };
            let notice = "/c was stopped by the user";
            a.emit(mail("m3", "/c", MailKind::Notice, false, notice))
                .await;
            a.emit(changed("/c", held, Activity::Idle, 0.1)).await;
            sleep(Duration::from_millis(100)).await;
            sleep(Duration::from_secs(3)).await;
            let stopped = Seen::of(&a.screen);
            assert!(!stopped.marked(), "{:#?}", stopped.chat);
            let above = stopped.chat.len() - 2;
            assert_eq!(stopped.chat[..above], stopping.chat[..above]);
            assert!(
                stopped.chat[above + 1].contains(notice),
                "{:#?}",
                stopped.chat
            );
            a.start("Next", 4).await;
            sleep(Duration::from_millis(100)).await;
            let next = Seen::of(&a.screen);
            let n = next.chat.len();
            assert!(next.chat[n - 3].contains("Next"), "{:#?}", next.chat);
            assert!(next.chat[n - 1].contains(reply), "{:#?}", next.chat);
            a.emit(AgentEvent::BlockStopped { index: 0 }).await;
            a.emit(spawned(entry("/d", AgentState::Running, 0.1))).await;
            a.emit(completed()).await;
            sleep(Duration::from_millis(100)).await;
            let back = Seen::of(&a.screen);
            assert!(back.marked() && words(&back), "{:#?}", back.chat);
            a.ctrl('c').ctrl('c').ctrl('c');
        })
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_worker_under_an_idle_coordinator_is_listed_counted_and_arms_quit() {
    steering_on(screen(), options(Start::Stalled), |mut a| async move {
        a.emit(spawned(entry(
            "/lead",
            idle(Some(IdleOutcome::Waiting)),
            0.2,
        )))
        .await;
        a.emit(spawned(entry("/lead/coder", AgentState::Running, 0.3)))
            .await;
        let lines = a.settle().await;
        let strip = line_with(&lines, "2 agents");
        assert!(strip.contains("2 agents · 1 working"), "{strip}");
        assert!(!strip.contains("/lead"), "{strip}");
        a.start("first", 1).await;
        a.key(KeyCode::Esc);
        let lines = a.settle().await;
        line_with(&lines, &copy::stopped_main(1, SEP));
        assert_eq!(a.next().await, ClientCommand::Cancel);
        a.emit(cancelled()).await;
        a.settle().await;
        a.ctrl('d');
        let lines = a.settle().await;
        line_with(&lines, &copy::quit_with_agents(1));
        a.paste("/exit").key(KeyCode::Enter);
        let lines = a.settle().await;
        line_with(
            &lines,
            &copy::shutting_down(Some(&copy::stopping_agents(1)), &UNICODE),
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_held_agent_never_arms_quit() {
    steering_on(screen(), options(Start::Stalled), |a| async move {
        let held = AgentState::Held {
            reason: HoldReason::Stopped,
        };
        a.emit(spawned(entry("/review", held, 0.2))).await;
        let lines = a.settle().await;
        assert!(!line_with(&lines, "1 agent").contains("working"));
        a.ctrl('d');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn with_nothing_busy_ctrl_d_quits_at_once() {
    steering_on(screen(), options(Start::Stalled), |a| async move {
        a.ctrl('d');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn with_nothing_busy_exit_quits_at_once() {
    steering_on(screen(), options(Start::Stalled), |a| async move {
        a.paste("/exit").key(KeyCode::Enter);
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn with_nothing_busy_ctrl_c_arms_first() {
    steering_on(screen(), options(Start::Stalled), |a| async move {
        a.ctrl('c');
        let lines = a.settle().await;
        line_with(&lines, AGAIN_TO_QUIT);
        a.ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn mail_waiting_at_an_idle_root_says_what_to_do_until_it_is_delivered() {
    steering_on(screen(), options(Start::Stalled), |mut a| async move {
        a.emit(mail(
            "m1",
            "/review",
            MailKind::Notice,
            false,
            "/review was stopped by the user",
        ))
        .await;
        a.emit(mail(
            "m2",
            "/docs",
            MailKind::Report,
            true,
            "/docs reported",
        ))
        .await;
        let held = AgentEvent::WakeHeld {
            sources: vec![WakeSource::Mail {
                mail: MailId("m2".to_owned()),
                from: AgentPath::new("/docs"),
            }],
            reason: HoldReason::Stopped,
        };
        a.emit(held.clone()).await;
        let lines = a.settle().await;
        line_with(
            &lines,
            &copy::mail_waits(2, Some(HoldReason::Stopped)).join(SEP),
        );
        line_with(&lines, "notice from /review \u{b7} /review was stopped");
        line_with(&lines, "report from /docs");
        a.input(Input::Resize(a.screen.resize(30, 40)));
        sleep(RESIZE_DEBOUNCE).await;
        let narrow = a.settle().await;
        for part in copy::mail_waits(2, Some(HoldReason::Stopped)) {
            assert_eq!(line_with(&narrow, &part).trim(), part, "{narrow:#?}");
        }
        a.input(Input::Resize(a.screen.resize(30, 100)));
        sleep(RESIZE_DEBOUNCE).await;
        a.start("go on", 1).await;
        a.emit(delivered(&["m1", "m2"], 1)).await;
        a.emit(cancelled()).await;
        a.emit(held).await;
        let lines = a.settle().await;
        line_with(
            &lines,
            &copy::mail_waits(2, Some(HoldReason::Stopped)).join(SEP),
        );
        a.start("again", 2).await;
        a.emit(delivered(&["m1", "m2"], 1)).await;
        a.emit(completed()).await;
        let lines = a.settle().await;
        none_with(&lines, "send a message to deliver");
        assert_eq!(rows_with(&lines, "notice from /review"), 1, "{lines:#?}");
        assert_eq!(rows_with(&lines, "report from /docs"), 1, "{lines:#?}");
        a.ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_rewind_keeps_the_mail_rows_before_its_point() {
    steering_on(screen(), options(Start::Stalled), |mut a| async move {
        a.start("first", 1).await;
        a.emit(mail(
            "m1",
            "/review",
            MailKind::Report,
            true,
            "/review reported early",
        ))
        .await;
        a.emit(delivered(&["m1"], 2)).await;
        a.emit(completed()).await;
        a.start("second", 2).await;
        a.emit(mail("m2", "/docs", MailKind::Message, true, "a later note"))
            .await;
        a.emit(delivered(&["m2"], 2)).await;
        a.emit(completed()).await;
        let lines = a.settle().await;
        line_with(&lines, "message from /docs");
        a.emit(super::rewind::rewound(RequestId(90), 2, Vec::new()))
            .await;
        let lines = a.settle().await;
        line_with(&lines, "report from /review");
        line_with(&lines, "/review reported early");
        none_with(&lines, "message from /docs");
        a.ctrl('c').ctrl('c');
    })
    .await;
}

fn resumed_history() -> Resumed {
    Resumed {
        view: wisp_protocol::SessionView {
            events: vec![
                AgentEvent::SessionResumed {
                    turns: 2,
                    moved: Vec::new(),
                    stay: Vec::new(),
                },
                AgentEvent::turn_started(1),
                AgentEvent::StepStarted { step: 1 },
                mail(
                    "m1",
                    "/review",
                    MailKind::Report,
                    true,
                    &report_text("/review reported · 6m12s", "Plan ready"),
                ),
                mail(
                    "m2",
                    "/docs",
                    MailKind::Notice,
                    true,
                    "/docs was stopped by the user",
                ),
                delivered(&["m1", "m2"], 1),
                completed(),
                mail("m4", "/qa", MailKind::Message, false, "a quiet note"),
                AgentEvent::InputAccepted {
                    opening: false,
                    id: InputId("tui-1".to_owned()),
                    mode: DeliveryMode::Queue,
                    text: "carry on".to_owned(),
                    images: Vec::new(),
                    replaces: None,
                    held: false,
                    front: false,
                    from: None,
                },
                AgentEvent::turn_started(2),
                AgentEvent::InputDelivered {
                    ids: vec![InputId("tui-1".to_owned()), mail_id("m4")],
                    step: 1,
                },
                mail("m5", "/qa", MailKind::Message, false, "a mid-turn note"),
                mail("m6", "/docs", MailKind::Message, true, "a waking one"),
                AgentEvent::StepStarted { step: 2 },
                delivered(&["m6", "m5"], 2),
                completed(),
                AgentEvent::turn_started(3),
                AgentEvent::SessionResumed {
                    turns: 3,
                    moved: Vec::new(),
                    stay: Vec::new(),
                },
                mail("m7", "/qa", MailKind::Message, false, "after a restart"),
            ],
            ..Default::default()
        },
        ..Default::default()
    }
}

#[tokio::test(start_paused = true)]
async fn a_resumed_root_shows_its_past_mail_and_shows_mail_when_it_arrives() {
    run_agents(
        Some(resumed_history()),
        options(Start::Stalled),
        |a| async move {
            let lines = a.settle().await;
            line_with(&lines, "report from /review");
            line_with(&lines, "notice from /docs");
            assert_eq!(rows_with(&lines, "a quiet note"), 1, "{lines:#?}");
            assert!(
                row_of(&lines, "note from /qa") < row_of(&lines, "carry on"),
                "{lines:#?}"
            );
            let mid = row_of(&lines, "a mid-turn note");
            assert!(row_of(&lines, "a waking one") < mid, "{lines:#?}");
            line_with(&lines, "after a restart");
            a.emit(mail(
                "m3",
                "/review",
                MailKind::Message,
                false,
                "one more thing",
            ))
            .await;
            let lines = a.settle().await;
            line_with(&lines, "note from /review \u{b7} one more thing");
            none_with(&lines, "send a message to deliver");
            a.emit(AgentEvent::TurnStarted {
                turn: 4,
                origin: Some(TurnOrigin::Mail),
                retry_of: None,
                answers: None,
                ends: Vec::new(),
            })
            .await;
            a.emit(delivered(&["m3"], 1)).await;
            a.emit(completed()).await;
            let lines = a.settle().await;
            assert_eq!(rows_with(&lines, "one more thing"), 1, "{lines:#?}");
            a.ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_start_asks_for_the_tree_and_draws_its_snapshot() {
    let mut listing = options(Start::Stalled);
    listing.list_agents = true;
    run_agents(None, listing, |mut a| async move {
        let ClientCommand::ListAgents { request } = a.next().await else {
            panic!("expected ListAgents first");
        };
        a.emit(AgentEvent::AgentsSnapshot {
            request: Some(request),
            agents: vec![entry("/review", AgentState::Running, 2.0)],
        })
        .await;
        let lines = a.settle().await;
        line_with(&lines, "1 agent");
        a.ctrl('c').ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn ctrl_t_steps_through_main_and_the_live_agents_climbs_from_an_ended_one_and_ctrl_c_never_stops_main()
 {
    steering_on(screen(), options(Start::Stalled), |mut a| async move {
        let mut lead = entry("/lead", AgentState::Running, 1.0);
        lead.checklist = Some(TodoProgress {
            completed: 2,
            total: 5,
        });
        a.emit(spawned(lead)).await;
        a.emit(spawned(entry("/old", AgentState::Ended, 0.3))).await;
        a.start("first", 1).await;
        a.ctrl('t');
        let lines = a.settle().await;
        line_with(&lines, copy::AGENTS_TITLE);
        line_with(&lines, "agent-viz is off \u{b7} /config");
        line_with(&lines, " main   test-model low   running");
        a.key(KeyCode::Down).key(KeyCode::Down);
        let lines = a.settle().await;
        assert!(
            line_with(&lines, " lead   opus-5-5 high").contains("todo 2/5"),
            "{lines:#?}"
        );
        none_with(&lines, " old ");
        a.emit(changed("/lead", AgentState::Ended, Activity::Idle, 1.0))
            .await;
        let lines = a.settle().await;
        line_with(&lines, " main   test-model low   running");
        none_with(&lines, " lead");
        a.ctrl('c');
        let lines = a.settle().await;
        none_with(&lines, copy::AGENTS_TITLE);
        a.nothing_sent().await;
        a.ctrl('t').ctrl('d');
        let lines = a.settle().await;
        line_with(&lines, copy::AGENTS_TITLE);
        a.key(KeyCode::Esc);
        a.emit(cancelled()).await;
        a.ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_agents_fixture_replays_its_spawn_mail_and_spend() {
    let events = replay::parse(FIXTURE).unwrap();
    session_with(
        screen(),
        vec![events],
        options(Start::Stalled),
        |run| async move {
            run.say("review the plan");
            sleep(Duration::from_secs(5)).await;
            let lines = run.screen.visible();
            let row = line_with(&lines, "Started review");
            assert!(row.contains("in ~/Projects/Wisp-worktrees/review"), "{row}");
            line_with(&lines, "reported \u{b7} Plan ready: docs/plans/x.md");
            line_with(&lines, "\u{25C7} report from /review");
            line_with(&lines, "\u{25C7} notice from /docs");
            line_with(&lines, "2 agents");
            run.open_transcript().await;
            let lines = run.screen.visible();
            let footer = line_with(&lines, "$0.0208 \u{b7} opus-5-5");
            assert!(
                !footer.contains("session") && !footer.contains("agents $"),
                "{footer}"
            );
            run.send(key(KeyCode::Esc));
            sleep(Duration::from_millis(100)).await;
            run.quit();
        },
    )
    .await
    .unwrap();
}

const ROWS: u16 = 40;

async fn scene(mode: Mode, width: u16, glyphs: GlyphSet) -> Vec<(String, String)> {
    let screen = Screen::new(ROWS, width);
    let options = frozen(Options::new(Theme::new(mode, Depth::TrueColor), glyphs));
    let (commands, asked) = super::without_terminal(64);
    let (events_in, events) = mpsc::channel(1024);
    let (inputs, received) = mpsc::unbounded_channel();
    let session = Session {
        leave_note: None,
        attached: None,
        alerts: Vec::new(),
        facts: facts(),
        events,
        commands,
        resumed: None,
    };
    let mut console = screen.clone();
    let app = run(&mut console, received, session, options);
    let mut a = Agent {
        screen: screen.clone(),
        inputs,
        events: events_in,
        commands: asked,
    };
    let driver = async move {
        let mut shots = Vec::new();
        sleep(Duration::from_millis(400)).await;
        a.start("Review the plan with two agents", 1).await;
        for event in spawn_call("toolu_1", "review") {
            a.emit(event).await;
        }
        a.emit(spawned(entry("/review", AgentState::Running, 0.4)))
            .await;
        let docs = entry(
            "/docs",
            AgentState::Queued {
                reason: QueueReason::Concurrency,
            },
            0.0,
        );
        a.emit(spawned(docs)).await;
        let mut coder = entry("/review/coder", AgentState::Running, 0.8);
        coder.context = Some(ContextFill {
            used: 352_000,
            window: Some(1_000_000),
            limit: Some(400_000),
            estimated: false,
            auto_compact: false,
        });
        a.emit(spawned(coder)).await;
        let lane = QueueReason::Endpoint;
        let mut queued = changed(
            "/review/coder",
            AgentState::Queued { reason: lane },
            Activity::Queued { reason: lane },
            0.8,
        );
        if let AgentEvent::AgentStateChanged { since_ms, .. } = &mut queued {
            *since_ms = Some(now_ms() - 2_000);
        }
        a.emit(queued).await;
        a.emit_spent(priced_end(0.42, 1.2)).await;
        sleep(Duration::from_secs(2)).await;
        shots.push(("running".to_owned(), a.screen.styled()));
        a.emit(changed(
            "/review",
            AgentState::Stopping,
            Activity::Tool {
                name: "bash".to_owned(),
            },
            0.5,
        ))
        .await;
        sleep(Duration::from_millis(200)).await;
        shots.push(("stopping".to_owned(), a.screen.styled()));
        a.emit(reported(
            "/review",
            "Plan ready: docs/plans/x.md\nThe rest.",
        ))
        .await;
        a.emit(changed(
            "/review",
            idle(Some(IdleOutcome::Reported)),
            Activity::Idle,
            0.6,
        ))
        .await;
        a.emit(changed(
            "/docs",
            AgentState::Running,
            Activity::Streaming,
            0.1,
        ))
        .await;
        a.emit(mail(
            "m1",
            "/review",
            MailKind::Report,
            true,
            "/review reported · 6m12s · $0.60 in all\nPlan ready: docs/plans/x.md",
        ))
        .await;
        a.emit(mail(
            "m2",
            "/docs",
            MailKind::Message,
            true,
            "Which plan file do you mean?",
        ))
        .await;
        a.emit(AgentEvent::TurnStarted {
            turn: 2,
            origin: Some(TurnOrigin::Mail),
            retry_of: None,
            answers: None,
            ends: Vec::new(),
        })
        .await;
        a.emit(AgentEvent::StepStarted { step: 1 }).await;
        a.emit(delivered(&["m1", "m2"], 1)).await;
        a.emit_spent(priced_end(0.84, 1.4)).await;
        a.emit(mail(
            "m3",
            "/review/coder",
            MailKind::Notice,
            false,
            "/review/coder was stopped with its parent",
        ))
        .await;
        sleep(Duration::from_millis(200)).await;
        shots.push((
            "reported, mail and a quiet notice".to_owned(),
            a.screen.styled(),
        ));
        a.ctrl('c');
        sleep(Duration::from_millis(200)).await;
        shots.push(("quit armed".to_owned(), a.screen.styled()));
        a.key(KeyCode::Char('x')).key(KeyCode::Backspace);
        a.ctrl('t');
        sleep(Duration::from_millis(200)).await;
        shots.push(("agents".to_owned(), a.screen.styled()));
        a.key(KeyCode::Down);
        sleep(Duration::from_millis(200)).await;
        shots.push(("agents, a child focused".to_owned(), a.screen.styled()));
        a.key(KeyCode::Esc);
        let _ = a.commands.try_recv();
        a.inputs.send(Input::Key(Key::ctrl('c'))).unwrap();
        a.inputs.send(Input::Key(Key::ctrl('c'))).unwrap();
        shots
    };
    let (exit, shots) = tokio::join!(app, driver);
    exit.unwrap();
    shots
}

#[tokio::test(start_paused = true)]
async fn the_agent_rows_strip_mail_footer_quit_hint_and_list_at_40_80_and_120_in_every_look() {
    let mode = Mode::Dark;
    for (glyphs, set) in [(GlyphSet::Unicode, "unicode"), (GlyphSet::Ascii, "ascii")] {
        for width in [40u16, 80, 120] {
            let shots = scene(mode, width, glyphs).await;
            let mode_name = match mode {
                Mode::Dark => "dark",
                Mode::Light => "light",
            };
            snapshot(
                &format!("agents-{mode_name}-truecolor-{set}-{width}"),
                &shots,
            );
        }
    }
}

fn mail_turn(turn: u64, retry_of: Option<u64>) -> AgentEvent {
    AgentEvent::TurnStarted {
        turn,
        origin: Some(TurnOrigin::Mail),
        retry_of,
        answers: None,
        ends: Vec::new(),
    }
}

fn rows_with(lines: &[String], needle: &str) -> usize {
    lines.iter().filter(|l| l.contains(needle)).count()
}

const REVIEW_MAIL: &str = "report from /review";

#[tokio::test(start_paused = true)]
async fn a_mail_delivered_again_after_a_stopped_turn_keeps_one_row() {
    steering_on(screen(), options(Start::Stalled), |a| async move {
        a.emit(mail("m1", "/review", MailKind::Report, true, "Plan ready"))
            .await;
        a.emit(mail_turn(1, None)).await;
        a.emit(delivered(&["m1"], 1)).await;
        a.emit(cancelled()).await;
        a.emit(mail_turn(2, None)).await;
        a.emit(delivered(&["m1"], 1)).await;
        a.emit(completed()).await;
        let lines = a.settle().await;
        assert_eq!(rows_with(&lines, REVIEW_MAIL), 1, "{lines:#?}");
        a.ctrl('c').ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_mail_delivered_again_by_a_retry_keeps_one_row() {
    steering_on(screen(), options(Start::Stalled), |a| async move {
        a.emit(mail("m1", "/review", MailKind::Report, true, "Plan ready"))
            .await;
        a.emit(mail_turn(1, None)).await;
        a.emit(delivered(&["m1"], 1)).await;
        a.emit(failed()).await;
        a.emit(mail_turn(2, Some(1))).await;
        a.emit(delivered(&["m1"], 1)).await;
        a.emit(completed()).await;
        let lines = a.settle().await;
        assert_eq!(rows_with(&lines, REVIEW_MAIL), 1, "{lines:#?}");
        a.ctrl('c').ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_mail_the_resumed_transcript_drew_keeps_one_row_when_delivered_live() {
    let history = Resumed {
        view: wisp_protocol::SessionView {
            events: vec![
                AgentEvent::SessionResumed {
                    turns: 1,
                    moved: Vec::new(),
                    stay: Vec::new(),
                },
                mail_turn(1, None),
                AgentEvent::StepStarted { step: 1 },
                mail("m1", "/review", MailKind::Report, true, "Plan ready"),
                delivered(&["m1"], 1),
                delivered(&["m1"], 1),
            ],
            ..Default::default()
        },
        ..Default::default()
    };
    run_agents(Some(history), options(Start::Stalled), |a| async move {
        let lines = a.settle().await;
        assert_eq!(rows_with(&lines, REVIEW_MAIL), 1, "{lines:#?}");
        a.emit(mail("m1", "/review", MailKind::Report, true, "Plan ready"))
            .await;
        a.emit(mail_turn(2, None)).await;
        a.emit(delivered(&["m1"], 1)).await;
        a.emit(completed()).await;
        let lines = a.settle().await;
        assert_eq!(rows_with(&lines, REVIEW_MAIL), 1, "{lines:#?}");
        a.ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn with_agents_running_exit_quits_at_once_and_counts_them_live() {
    steering_on(
        Screen::new(30, 80),
        options(Start::Stalled),
        |mut a| async move {
            a.emit(spawned(entry("/review", AgentState::Running, 0.2)))
                .await;
            a.emit(spawned(entry("/coder", AgentState::Running, 0.3)))
                .await;
            a.settle().await;
            a.paste("/exit").key(KeyCode::Enter);
            let lines = a.settle().await;
            assert!(a.commands.recv().await.is_none());
            line_with(&lines, "Shutting down, please wait: stopping 2 agents…");
            none_with(&lines, &copy::quit_with_agents(2));
            a.emit(changed("/review", AgentState::Ended, Activity::Idle, 0.2))
                .await;
            let lines = a.settle().await;
            line_with(
                &lines,
                &copy::shutting_down(Some(&copy::stopping_agents(1)), &UNICODE),
            );
            a.emit(changed("/coder", AgentState::Ended, Activity::Idle, 0.3))
                .await;
            let lines = a.settle().await;
            line_with(&lines, &copy::shutting_down(None, &UNICODE));
            none_with(&lines, &copy::stopping_agents(1));
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_spawn_row_keeps_its_report_line_after_the_agent_is_ended() {
    steering_on(screen(), options(Start::Stalled), |mut a| async move {
        a.start("go", 1).await;
        for event in spawn_call("c1", "review") {
            a.emit(event).await;
        }
        a.emit(spawned(entry("/review", AgentState::Running, 0.2)))
            .await;
        a.emit(reported("/review", "Plan ready: 5 steps")).await;
        a.emit(changed(
            "/review",
            idle(Some(IdleOutcome::Reported)),
            Activity::Idle,
            0.4,
        ))
        .await;
        let lines = a.settle().await;
        line_with(&lines, "reported · Plan ready: 5 steps");
        a.emit(changed("/review", AgentState::Ended, Activity::Idle, 0.4))
            .await;
        let lines = a.settle().await;
        line_with(&lines, "reported · Plan ready: 5 steps");
        line_with(&lines, "ended");
        a.emit(completed()).await;
        a.ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn an_agent_ended_before_the_snapshot_keeps_its_report_line_on_the_spawn_row() {
    steering_on(screen(), options(Start::Stalled), |mut a| async move {
        a.start("go", 1).await;
        for event in spawn_call("c1", "review") {
            a.emit(event).await;
        }
        let mut review = entry("/review", AgentState::Ended, 0.4);
        review.last_report = Some("Plan ready: 5 steps".to_owned());
        a.emit(AgentEvent::AgentsSnapshot {
            request: None,
            agents: vec![review],
        })
        .await;
        let lines = a.settle().await;
        line_with(&lines, "reported · Plan ready: 5 steps");
        a.emit(completed()).await;
        a.ctrl('c').ctrl('c');
    })
    .await;
}

fn renamed(from: &str, to: &str) -> AgentEvent {
    AgentEvent::AgentRenamed {
        request: None,
        agent: AgentPath::new(from),
        to: AgentPath::new(to),
    }
}

#[tokio::test(start_paused = true)]
async fn an_agent_renamed_moves_in_its_chat_header_composer_agents_and_ps() {
    steering_on(screen(), options(Start::Stalled), |mut a| async move {
        a.emit(spawned(entry("/task-a", AgentState::Running, 0.0)))
            .await;
        a.emit(spawned(entry("/task-a/coder", AgentState::Running, 0.0)))
            .await;
        let mut program = super::processes::started(3);
        if let AgentEvent::ProcessStarted { target, .. } = &mut program {
            *target = "cargo test".to_owned();
        }
        a.emit(super::processes::started(3)).await;
        a.emit(wrapped("/task-a/coder", program)).await;
        a.emit(wrapped("/task-a/coder", super::processes::terminal(4)))
            .await;
        a.ctrl('t').key(KeyCode::Down).key(KeyCode::Down);
        a.key(KeyCode::Enter);
        assert_eq!(a.next().await, watch("/task-a/coder", true));
        a.paste("/ps").key(KeyCode::Enter);
        a.key(KeyCode::Down).key(KeyCode::Char('x'));
        line_with(&a.settle().await, &copy::press_x_again("3"));
        a.emit(renamed("/task-a/coder", "/task-a/fixer")).await;
        let lines = a.settle().await;
        line_with(&lines, "   /task-a/fixer");
        none_with(&lines, "/task-a/coder");
        a.key(KeyCode::Char('x'));
        let stop = ClientCommand::StopProcess { id: 3 };
        assert_eq!(a.next().await, child("/task-a/fixer", stop));
        a.key(KeyCode::Down).key(KeyCode::Char('a'));
        take_over(&mut a, "/task-a/fixer", 4).await;
        a.emit(renamed("/task-a", "/lead")).await;
        line_with(&a.settle().await, "Process 4  python3 -i");
        a.ctrl(']');
        let detach = ClientCommand::DetachProcess { id: 4 };
        assert_eq!(a.next().await, child("/lead/fixer", detach));
        a.key(KeyCode::Esc).key(KeyCode::Esc);
        let lines = a.settle().await;
        line_with(&lines, "/lead/fixer \u{b7} running");
        line_with(&lines, &copy::message_to("/lead/fixer"));
        none_with(&lines, "/task-a");
        a.key(KeyCode::Esc);
        assert_eq!(a.next().await, watch("/lead/fixer", false));
        a.ctrl('t').key(KeyCode::Down).key(KeyCode::Down);
        a.emit(renamed("/lead", "/team")).await;
        a.key(KeyCode::Enter);
        assert_eq!(a.next().await, watch("/team/fixer", true));
        line_with(&a.settle().await, &copy::message_to("/team/fixer"));
        a.key(KeyCode::Esc);
        assert_eq!(a.next().await, watch("/team/fixer", false));
        a.ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn an_event_under_the_old_path_lands_on_the_renamed_agent() {
    steering_on(screen(), options(Start::Stalled), |mut a| async move {
        a.start("go", 1).await;
        for event in spawn_call("c1", "coder") {
            a.emit(event).await;
        }
        a.emit(spawned(entry("/coder", AgentState::Running, 0.2)))
            .await;
        a.emit(renamed("/coder", "/fixer")).await;
        a.emit(reported("/coder", "Plan ready: 5 steps")).await;
        a.emit(changed(
            "/coder",
            idle(Some(IdleOutcome::Reported)),
            Activity::Idle,
            0.4,
        ))
        .await;
        let lines = a.settle().await;
        line_with(&lines, "reported \u{b7} Plan ready: 5 steps");
        a.emit(completed()).await;
        a.emit(warm_changed("/coder", warm(2, false))).await;
        a.emit(spawned(entry("/coder/scout", AgentState::Running, 0.0)))
            .await;
        a.emit(wrapped("/coder", bash("c9", "cargo build"))).await;
        a.ctrl('t').key(KeyCode::Down);
        let lines = a.settle().await;
        line_with(&lines, " fixer ");
        line_with(&lines, "keep-warm: 2 of 8 refreshes");
        a.key(KeyCode::Down);
        line_with(&a.settle().await, " scout ");
        a.key(KeyCode::Up).key(KeyCode::Enter);
        assert_eq!(a.next().await, watch("/fixer", true));
        line_with(&a.settle().await, "cargo build");
        a.key(KeyCode::Esc);
        assert_eq!(a.next().await, watch("/fixer", false));
        a.ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_renamed_agent_keeps_its_report_line_on_its_spawn_row_after_a_resume() {
    let mut events = vec![AgentEvent::turn_started(1)];
    events.extend(spawn_call("c1", "review"));
    events.extend([completed(), renamed("/review", "/planner")]);
    let resumed = Resumed {
        view: wisp_protocol::SessionView {
            events,
            ..Default::default()
        },
        ..Default::default()
    };
    run_agents(Some(resumed), options(Start::Stalled), |a| async move {
        let mut planner = entry("/planner", idle(Some(IdleOutcome::Reported)), 0.4);
        planner.last_report = Some("Plan ready: 5 steps".to_owned());
        a.emit(AgentEvent::AgentsSnapshot {
            request: None,
            agents: vec![planner],
        })
        .await;
        line_with(&a.settle().await, "reported \u{b7} Plan ready: 5 steps");
        a.emit(renamed("/planner", "/lead")).await;
        line_with(&a.settle().await, "reported \u{b7} Plan ready: 5 steps");
        a.ctrl('c').ctrl('c');
    })
    .await;
}

fn warm(refreshes: u32, paused: bool) -> wisp_protocol::KeepWarm {
    wisp_protocol::KeepWarm {
        refreshes,
        limit: 8,
        paused,
    }
}

fn warm_changed(path: &str, keep_warm: wisp_protocol::KeepWarm) -> AgentEvent {
    AgentEvent::KeepWarmChanged {
        agent: AgentPath::new(path),
        keep_warm,
    }
}

#[tokio::test(start_paused = true)]
async fn the_panel_shows_keep_warm_for_main_and_live_agents_and_never_for_an_ended_one() {
    steering_on(screen(), options(Start::Stalled), |a| async move {
        a.emit(warm_changed("/", warm(0, true))).await;
        let mut planner = entry("/planner", idle(Some(IdleOutcome::Reported)), 1.0);
        planner.keep_warm = Some(warm(0, false));
        a.emit(spawned(planner)).await;
        a.emit(warm_changed("/planner", warm(2, false))).await;
        let mut coder = entry("/coder", idle(Some(IdleOutcome::Reported)), 1.0);
        coder.keep_warm = Some(warm(1, false));
        a.emit(spawned(coder)).await;
        a.emit(warm_changed("/coder", warm(1, true))).await;
        let mut gone = entry("/old", idle(Some(IdleOutcome::Reported)), 0.3);
        gone.keep_warm = Some(warm(3, false));
        a.emit(spawned(gone)).await;
        a.emit(changed("/old", AgentState::Ended, Activity::Idle, 0.3))
            .await;
        a.ctrl('t');
        line_with(
            &a.settle().await,
            "   keep-warm: paused until its next turn",
        );
        a.key(KeyCode::Down);
        let lines = a.settle().await;
        line_with(&lines, " coder ");
        line_with(&lines, "keep-warm: paused until its next turn");
        a.key(KeyCode::Down);
        let lines = a.settle().await;
        line_with(&lines, " planner ");
        line_with(&lines, "keep-warm: 2 of 8 refreshes");
        a.key(KeyCode::Down);
        let lines = a.settle().await;
        none_with(&lines, " old ");
        none_with(&lines, "keep-warm: 3");
        a.key(KeyCode::Esc);
        a.ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn main_shows_no_keep_warm_without_its_event() {
    steering_on(screen(), options(Start::Stalled), |mut a| async move {
        a.start("first", 1).await;
        a.ctrl('t');
        let lines = a.settle().await;
        line_with(&lines, " main   test-model low");
        none_with(&lines, "keep-warm");
        a.key(KeyCode::Esc);
        a.emit(cancelled()).await;
        a.ctrl('c').ctrl('c');
    })
    .await;
}

fn on(mut entry: AgentEntry, endpoint: &str, provider: &str, priced: bool) -> AgentEntry {
    entry.endpoint = Some(endpoint.to_owned());
    entry.provider = Some(provider.to_owned());
    entry.priced = priced;
    entry
}

#[tokio::test(start_paused = true)]
async fn the_agents_list_shows_where_each_agent_runs_and_not_priced_without_priced_spend() {
    steering_on(screen(), options(Start::Stalled), |a| async move {
        let mut coder = on(entry("/coder", idle(None), 0.0), "gpt", "openai", false);
        coder.model = "gpt-6-sol".to_owned();
        a.emit(spawned(coder)).await;
        a.emit(spawned(entry("/legacy", idle(None), 0.2))).await;
        let mut moved = on(entry("/moved", idle(None), 0.5), "sub", "anthropic", false);
        moved.lower_bound = true;
        a.emit(spawned(moved)).await;
        a.emit(spawned(on(
            entry("/old", AgentState::Ended, 0.3),
            "direct",
            "anthropic",
            true,
        )))
        .await;
        a.ctrl('t').key(KeyCode::Down);
        let lines = a.settle().await;
        let coder = line_with(&lines, " coder   gpt-6-sol high");
        assert!(coder.contains("not priced"), "{lines:#?}");
        none_with(&lines, " on gpt");
        a.key(KeyCode::Down);
        let lines = a.settle().await;
        line_with(&lines, " legacy   opus-5-5 high");
        none_with(&lines, "claude-opus-5-5");
        a.key(KeyCode::Down).key(KeyCode::Down);
        let lines = a.settle().await;
        assert!(
            line_with(&lines, " moved   opus-5-5 high").contains("$0.50"),
            "{lines:#?}"
        );
        none_with(&lines, " old ");
        none_with(&lines, "(anthropic)");
        a.key(KeyCode::Esc);
        a.ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_chat_header_ends_with_its_cost_and_its_status_row_says_where_it_runs() {
    steering_on(screen(), options(Start::Stalled), |a| async move {
        let mut coder = on(
            entry("/coder", AgentState::Running, 0.0),
            "gpt",
            "openai",
            false,
        );
        coder.model = "gpt-6-sol".to_owned();
        a.emit(spawned(coder)).await;
        a.ctrl('t');
        a.key(KeyCode::Down).key(KeyCode::Enter);
        let lines = a.settle().await;
        let header = line_with(&lines, "/coder");
        assert!(
            header.contains("/coder \u{b7} running \u{b7} not priced"),
            "{lines:#?}"
        );
        let status = line_with(&lines, &copy::esc(copy::BACK_WORD).text());
        assert!(status.contains("gpt-6-sol high"), "{lines:#?}");
        let mut detail = wisp_protocol::ErrorDetail::new(
            wisp_protocol::FailurePhase::Response,
            wisp_protocol::FailureSource::Endpoint,
            wisp_protocol::RetryClass::After { ms: 240_000 },
        );
        detail.limit_reached = Some(Box::new(wisp_protocol::plan_usage::UsageWindow {
            key: "subscription-limit".into(),
            label: "Subscription limit".into(),
            duration_seconds: None,
            scope: wisp_protocol::plan_usage::UsageScope::All,
            used_percent: None,
            resets_at: Some(crate::shared::fixed_now().as_second() + 240),
            observed_at: crate::shared::fixed_now().as_second(),
            source: wisp_protocol::plan_usage::UsageSource::RetryAfter,
        }));
        for event in [
            AgentEvent::turn_started(1),
            AgentEvent::StepStarted { step: 1 },
            AgentEvent::Retrying {
                attempt: 1,
                delay_ms: 240_000,
                reason: "rate limited".into(),
                step: 1,
                kind: wisp_protocol::ErrorKind::RateLimit,
                detail: Some(detail),
                max_attempts: 0,
                discarded_blocks: 0,
                discarded_chars: 0,
                usage: None,
                model: None,
                cost_usd: None,
                lower_bound: false,
                resend: None,
                busy: None,
            },
        ] {
            a.emit(AgentEvent::Agent {
                agent: AgentPath::new("/coder"),
                event: Box::new(event),
            })
            .await;
        }
        let lines = a.settle().await;
        line_with(&lines, "Retrying step 1");
        none_with(&lines, "esc stops waiting");
        a.key(KeyCode::Esc);
        a.ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_childs_code_block_in_its_chat_names_its_language_with_no_copy() {
    steering_on(screen(), options(Start::Stalled), |a| async move {
        a.emit(spawned(entry("/coder", AgentState::Running, 0.0)))
            .await;
        let view = ToolView::Code {
            language: Some("rust".to_owned()),
            code: "fn main() {}".to_owned(),
        };
        let result = "Shown to the user: 1 line of rust.";
        for event in [
            AgentEvent::turn_started(1),
            AgentEvent::StepStarted { step: 1 },
            show_requested("c", "code", None, view),
            AgentEvent::call_started("c", "~"),
            show_finished("c", vec![Fact::Lines { n: 1 }], result),
        ] {
            let agent = AgentPath::new("/coder");
            let event = Box::new(event);
            a.emit(AgentEvent::Agent { agent, event }).await;
        }
        a.ctrl('t');
        a.key(KeyCode::Down).key(KeyCode::Enter);
        let lines = a.settle().await;
        let top = line_with(&lines, "\u{2500} rust \u{2500}");
        assert!(!top.contains(copy::COPY_WORD), "{lines:#?}");
        line_with(&lines, "fn main() {}");
        a.key(KeyCode::Esc);
        a.ctrl('c').ctrl('c');
    })
    .await;
}

pub(super) fn main_on(priced: bool) -> SessionFacts {
    let mut facts = facts();
    facts.endpoint_name = Some("direct".to_owned());
    facts.endpoints = vec![EndpointFacts {
        models: Vec::new(),
        mark: crate::mockup_mark("direct"),
        name: "direct".to_owned(),
        provider: "anthropic".to_owned(),
        credential: None,
        reach: None,
        host: None,
        unavailable: None,
        priced,
        expired: false,
        key_var: None,
        complete: false,
        default_model: None,
    }];
    facts
}

#[tokio::test(start_paused = true)]
async fn main_on_an_unpriced_endpoint_reads_not_priced() {
    run_agents_on(
        screen(),
        main_on(false),
        None,
        options(Start::Stalled),
        |a| async move {
            a.ctrl('t');
            let lines = a.settle().await;
            assert!(
                line_with(&lines, " main  ").contains("not priced"),
                "{lines:#?}"
            );
            a.key(KeyCode::Esc);
            a.ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn main_row_names_its_endpoint_and_provider_from_the_session_facts() {
    let mut facts = main_on(true);
    facts.endpoints[0].provider = "anthropic\u{202e}".to_owned();
    run_agents_on(
        screen(),
        facts,
        None,
        options(Start::Stalled),
        |a| async move {
            let mut hostile = entry("/hostile", idle(None), 0.0);
            hostile.endpoint = Some("gone\u{202e}\u{7}".to_owned());
            hostile.provider = Some("anthropic\u{202e}".to_owned());
            a.emit(spawned(hostile)).await;
            a.ctrl('t');
            let lines = a.settle().await;
            line_with(&lines, " main   test-model low");
            line_with(&lines, "   direct \u{b7} API");
            none_with(&lines, "not priced");
            a.key(KeyCode::Down);
            let lines = a.settle().await;
            line_with(&lines, "   gone<U+202E>^G \u{b7} gone");
            none_with(&lines, "can't move");
            a.key(KeyCode::Esc);
            a.ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn at_80_columns_the_panel_keeps_the_model_and_puts_the_account_on_its_second_row() {
    run_agents_on(
        Screen::new(30, 80),
        main_on(true),
        None,
        options(Start::Stalled),
        |a| async move {
            let reviewer = on(
                entry("/reviewer", idle(None), 0.2),
                "direct",
                "anthropic",
                true,
            );
            a.emit(spawned(reviewer)).await;
            a.ctrl('t');
            line_with(&a.settle().await, " main  test-model low  idle");
            a.key(KeyCode::Down);
            let lines = a.settle().await;
            line_with(&lines, " reviewer  opus-5-5 high  idle 2s  $0.20");
            line_with(&lines, "   direct \u{b7} API");
            a.key(KeyCode::Esc);
            a.ctrl('c').ctrl('c');
        },
    )
    .await;
}

const REBUILD_WARNING: &str = "Wisp's prompt or tools changed since this tree ran: 2 agents rebuild their cache on their next request (~$1.20), dropping earlier reasoning; the conversations are kept";

fn owners_night() -> Vec<AgentEntry> {
    let agent = |path: &str, preset: &str, change: Option<PresetChange>, spent: f64| {
        let mut e = entry(path, idle(None), spent);
        e.preset = Some(preset.to_owned());
        e.preset_change = change;
        e.session_id = "01a17f2c-5c96-70a1-8b16-369ab376bb1f".to_owned();
        e.endpoint = Some("direct".to_owned());
        e.provider = Some("anthropic".to_owned());
        e.model = "claude-opus-5-5".to_owned();
        e
    };
    let mut plugins = agent("/plugins", "coordinator", Some(PresetChange::Edited), 6.1);
    plugins.checklist = Some(TodoProgress {
        completed: 3,
        total: 7,
    });
    vec![
        agent(
            "/plugin-moves",
            "coordinator",
            Some(PresetChange::Edited),
            8.2,
        ),
        plugins,
        agent("/plugins/coderc2", "coder", None, 2.3),
        agent(
            "/plugins/review12a-b",
            "reviewer",
            Some(PresetChange::Edited),
            1.9,
        ),
    ]
}

fn long_history() -> Resumed {
    let mut events = Vec::new();
    for n in 1..=10 {
        events.extend(super::resumed::turn(n, &format!("question {n}")));
        events.extend(super::resumed::reply(&format!("answer {n}")));
        events.push(super::resumed::completed());
    }
    events.extend(super::resumed::turn(
        11,
        "Ship the fix for the flaky golden",
    ));
    events.extend(super::resumed::reply(
        "Done: the golden now waits for the report before it quits, and the rerun passed 20 of 20 under load.",
    ));
    events.push(super::resumed::completed());
    Resumed {
        view: wisp_protocol::SessionView {
            events,
            ..Default::default()
        },
        ..Default::default()
    }
}

async fn presets_scene(mode: Mode, glyphs: GlyphSet, width: u16) -> Vec<(String, String)> {
    let mut options = frozen(Options::new(Theme::new(mode, Depth::TrueColor), glyphs));
    options.list_agents = true;
    options.notes = vec![REBUILD_WARNING.to_owned(), copy::presets_changed(2)];
    let shots = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let taken = shots.clone();
    run_agents_on(
        Screen::new(24, width),
        facts(),
        Some(long_history()),
        options,
        |mut a| async move {
            let request = loop {
                if let ClientCommand::ListAgents { request } = a.next().await {
                    break request;
                }
            };
            a.emit(main_spent((4.25, Some(0.0), false), &[])).await;
            a.emit(AgentEvent::AgentsSnapshot {
                request: Some(request),
                agents: owners_night(),
            })
            .await;
            sleep(Duration::from_millis(200)).await;
            taken
                .borrow_mut()
                .push(("resumed".to_owned(), a.screen.styled()));
            a.ctrl('t');
            sleep(Duration::from_millis(200)).await;
            a.key(KeyCode::Down);
            sleep(Duration::from_millis(200)).await;
            taken
                .borrow_mut()
                .push(("agents, plugin-moves focused".to_owned(), a.screen.styled()));
            a.key(KeyCode::Esc);
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
    shots.take()
}

#[tokio::test(start_paused = true)]
async fn changed_presets_show_one_start_note_and_a_mark_in_agents_at_40_80_and_120() {
    let (mode, mode_name) = (Mode::Dark, "dark");
    for (glyphs, glyph_name) in [(GlyphSet::Unicode, "unicode"), (GlyphSet::Ascii, "ascii")] {
        for width in [40u16, 80, 120] {
            let shots = presets_scene(mode, glyphs, width).await;
            snapshot(
                &format!("presets-changed-{mode_name}-truecolor-{glyph_name}-{width}"),
                &shots,
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn a_paste_under_the_agents_list_never_reaches_the_composer() {
    let (dir, options) = pasting("under-agents");
    steering_facts(screen(), options, seeing(facts()), |mut a| async move {
        let held = AgentState::Held {
            reason: HoldReason::Stopped,
        };
        a.emit(spawned(entry("/review", held, 0.2))).await;
        a.ctrl('t');
        let lines = a.settle().await;
        line_with(&lines, copy::AGENTS_TITLE);
        paste_every_way(&a.inputs).await;
        a.key(KeyCode::Esc);
        let lines = a.settle().await;
        none_with(&lines, copy::AGENTS_TITLE);
        assert_eq!(leaks(&a.screen, &dir), NO_LEAKS);
        a.nothing_sent().await;
    })
    .await;
}

fn subscription(name: &str) -> EndpointFacts {
    EndpointFacts {
        models: Vec::new(),
        mark: crate::mockup_mark(name),
        name: name.to_owned(),
        provider: "anthropic".to_owned(),
        credential: Some(wisp_render::chrome::Credential::AnthropicPlan),
        reach: Some(wisp_render::chrome::Endpoint::Official),
        host: None,
        unavailable: None,
        priced: true,
        expired: false,
        key_var: None,
        complete: false,
        default_model: None,
    }
}

fn two_subscriptions() -> SessionFacts {
    let mut facts = facts();
    facts.model = "claude-sonnet-5-5".to_owned();
    facts.endpoint_name = Some("claude-subscription".to_owned());
    facts.credential = Some(wisp_render::chrome::Credential::AnthropicPlan);
    facts.endpoints = vec![
        subscription("claude-subscription"),
        subscription("claude-work"),
        EndpointFacts {
            credential: Some(wisp_render::chrome::Credential::ApiKey),
            ..subscription("claude-api")
        },
    ];
    facts
}

fn accounts_tree() -> Vec<AgentEntry> {
    let agent = |path: &str, account: &str, spent: f64| {
        let mut e = on(entry(path, idle(None), spent), account, "anthropic", true);
        e.billing = wisp_protocol::Billing::AnthropicPlan;
        e.model = "claude-sonnet-5-5".to_owned();
        e
    };
    let mut coder = agent("/coder", "claude-work", 0.8);
    coder.preset = Some("coder".to_owned());
    coder.checklist = Some(TodoProgress {
        completed: 2,
        total: 5,
    });
    coder.context = Some(ContextFill {
        used: 41_000,
        window: Some(1_000_000),
        limit: Some(850_000),
        estimated: false,
        auto_compact: true,
    });
    let mut reviewer = agent("/coder/reviewer", "claude-subscription", 0.42);
    reviewer.context = Some(ContextFill {
        used: 159_000,
        window: Some(400_000),
        limit: Some(336_000),
        estimated: false,
        auto_compact: true,
    });
    let mut scout = agent("/scout", "claude-api", 12.5);
    scout.billing = wisp_protocol::Billing::Metered;
    scout.model = "claude-haiku-5-5".to_owned();
    vec![coder, reviewer, scout]
}

async fn accounts_scene(glyphs: GlyphSet, width: u16) -> Vec<(String, String)> {
    let mut options = frozen(Options::new(
        Theme::new(Mode::Dark, Depth::TrueColor),
        glyphs,
    ));
    options.list_agents = true;
    let shots = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let taken = shots.clone();
    run_agents_on(
        Screen::new(24, width),
        two_subscriptions(),
        None,
        options,
        |mut a| async move {
            let request = loop {
                if let ClientCommand::ListAgents { request } = a.next().await {
                    break request;
                }
            };
            a.emit(AgentEvent::AgentsSnapshot {
                request: Some(request),
                agents: accounts_tree(),
            })
            .await;
            a.ctrl('t');
            sleep(Duration::from_millis(200)).await;
            taken
                .borrow_mut()
                .push(("agents, main focused".to_owned(), a.screen.styled()));
            a.key(KeyCode::Down);
            sleep(Duration::from_millis(200)).await;
            taken
                .borrow_mut()
                .push(("agents, coder focused".to_owned(), a.screen.styled()));
            a.key(KeyCode::Down);
            sleep(Duration::from_millis(200)).await;
            taken
                .borrow_mut()
                .push(("agents, reviewer focused".to_owned(), a.screen.styled()));
            a.key(KeyCode::Esc);
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
    shots.take()
}

#[tokio::test(start_paused = true)]
async fn a_three_level_tree_on_two_subscriptions_shows_each_agents_account_at_40_80_and_120() {
    for (glyphs, glyph_name) in [(GlyphSet::Unicode, "unicode"), (GlyphSet::Ascii, "ascii")] {
        for width in [40u16, 80, 120] {
            let shots = accounts_scene(glyphs, width).await;
            snapshot(
                &format!("agents-accounts-dark-truecolor-{glyph_name}-{width}"),
                &shots,
            );
        }
    }
}

fn subscription_end(session: f64) -> AgentEvent {
    turn_ended(End {
        model: "claude-sonnet-5-5".to_owned(),
        cost: Some(Cost {
            turn_plan_usd: Some(session),
            session_plan_usd: Some(session),
            turn_billing: wisp_protocol::Billing::AnthropicPlan,
            session_billing: wisp_protocol::Billing::AnthropicPlan,
            ..cost(session, session, false)
        }),
        stats: TurnStats {
            duration_ms: 2_100,
            ..TurnStats::default()
        },
        ..End::default()
    })
}

fn strip_tree() -> Vec<AgentEntry> {
    let agent = |path: &str, account: &str, state: AgentState, spent: f64| {
        let mut e = on(entry(path, state, spent), account, "anthropic", true);
        e.billing = wisp_protocol::Billing::AnthropicPlan;
        e
    };
    let mut coder = agent("/coder", "claude-api", AgentState::Running, 4.06);
    coder.billing = wisp_protocol::Billing::Metered;
    vec![
        coder,
        agent("/coder/reviewer", "claude-work", AgentState::Running, 0.0),
        agent(
            "/broken",
            "claude-work",
            idle(Some(IdleOutcome::Failed)),
            0.0,
        ),
        agent("/scout", "claude-subscription", idle(None), 0.0),
        agent("/docs", "claude-subscription", idle(None), 0.0),
    ]
}

async fn strip_scene(glyphs: GlyphSet, width: u16) -> Vec<(String, String)> {
    let options = frozen(Options::new(
        Theme::new(Mode::Dark, Depth::TrueColor),
        glyphs,
    ));
    let shots = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let taken = shots.clone();
    run_agents_on(
        Screen::new(16, width),
        two_subscriptions(),
        None,
        options,
        |a| async move {
            for e in strip_tree() {
                a.emit(spawned(e)).await;
            }
            a.emit(AgentEvent::turn_started(1)).await;
            a.emit_spent(subscription_end(40.0)).await;
            sleep(Duration::from_millis(200)).await;
            taken
                .borrow_mut()
                .push(("two working, one failed".to_owned(), a.screen.styled()));
            for path in ["/coder", "/coder/reviewer"] {
                let spent = if path == "/coder" { 4.06 } else { 0.0 };
                a.emit(changed(
                    path,
                    idle(Some(IdleOutcome::Reported)),
                    Activity::Idle,
                    spent,
                ))
                .await;
            }
            sleep(Duration::from_secs(10)).await;
            taken
                .borrow_mut()
                .push(("all idle, ready".to_owned(), a.screen.styled()));
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
    shots.take()
}

async fn strip_then_goodbye(resumed: Option<Resumed>, settle: bool) -> (String, Vec<String>) {
    let total = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
    let seen = total.clone();
    let mut options = options(Start::Stalled);
    options.glyphs = GlyphSet::Unicode;
    let finished = run_agents_on(
        Screen::new(16, 80),
        two_subscriptions(),
        resumed,
        options,
        |a| async move {
            for e in strip_tree() {
                a.emit(spawned(e)).await;
            }
            a.emit(AgentEvent::turn_started(2)).await;
            a.emit_spent(subscription_end(40.0)).await;
            if settle {
                for path in ["/coder", "/coder/reviewer"] {
                    let spent = if path == "/coder" { 4.06 } else { 0.0 };
                    let state = idle(Some(IdleOutcome::Reported));
                    a.emit(changed(path, state, Activity::Idle, spent)).await;
                }
            }
            sleep(Duration::from_secs(1)).await;
            let rows = a.settle().await;
            let strip = rows
                .iter()
                .find(|r| r.contains(" total "))
                .map(|r| super::unframed(r));
            *seen.borrow_mut() = strip
                .and_then(|r| r.split(" total ").nth(1).map(|t| t.trim().to_owned()))
                .unwrap_or_default();
            a.ctrl('c').ctrl('c');
            a.settle().await;
            for path in ["/coder", "/coder/reviewer"] {
                let spent = if path == "/coder" { 4.06 } else { 0.0 };
                a.emit(changed(path, AgentState::Ended, Activity::Idle, spent))
                    .await;
            }
        },
    )
    .await;
    assert_eq!(finished.exit.as_ref().ok(), Some(&Exit::Quit));
    (total.take(), crate::testing::texts(&finished.goodbye))
}

#[tokio::test(start_paused = true)]
async fn the_goodbye_shows_the_strips_total_and_the_agents_running_at_quit() {
    let earlier = Resumed {
        view: wisp_protocol::SessionView {
            events: vec![
                AgentEvent::SessionResumed {
                    turns: 1,
                    moved: Vec::new(),
                    stay: Vec::new(),
                },
                AgentEvent::turn_started(1),
                subscription_end(10.0),
            ],
            ..Default::default()
        },
        ..Default::default()
    };
    for (resumed, settle, stopped) in [(None, false, 2), (None, true, 0), (Some(earlier), false, 2)]
    {
        let (strip, rows) = strip_then_goodbye(resumed, settle).await;
        assert!(strip.starts_with("~$"), "{strip}");
        assert_eq!(strip.ends_with('+'), !settle, "{strip}");
        let settled = strip.trim_end_matches('+');
        assert!(
            rows[1].ends_with(&format!(", total {settled}.")),
            "{strip} {rows:#?}"
        );
        assert_eq!(rows.len(), 3, "{rows:#?}");
        match copy::goodbye_agents(stopped, false) {
            Some(agents) => assert!(rows[2].ends_with(&agents), "{rows:#?}"),
            None => assert_eq!(rows[2].chars().count(), 8, "{rows:#?}"),
        }
    }
}

#[tokio::test(start_paused = true)]
async fn the_strip_shows_counts_and_one_summed_total_and_the_status_line_no_cost_at_40_80_and_120()
{
    for (glyphs, glyph_name) in [(GlyphSet::Unicode, "unicode"), (GlyphSet::Ascii, "ascii")] {
        for width in [40u16, 80, 120] {
            let shots = strip_scene(glyphs, width).await;
            for (name, shot) in &shots {
                let status = shot.lines().rev().find(|l| l.starts_with('|')).unwrap();
                assert!(!status.contains('$'), "{name} {width}: {status}");
            }
            snapshot(
                &format!("strip-dark-truecolor-{glyph_name}-{width}"),
                &shots,
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn main_and_a_child_on_claude_sonnet_both_read_the_short_model() {
    for takes_effort in [true, false] {
        let mut f = two_subscriptions();
        if !takes_effort {
            f.effort = "xhigh".to_owned();
            f.models
                .iter_mut()
                .find(|m| m.id == f.model)
                .unwrap()
                .caps
                .as_mut()
                .unwrap()
                .efforts
                .clear();
        }
        run_agents_on(
            Screen::new(24, 120),
            f,
            None,
            options(Start::Stalled),
            |a| async move {
                for e in accounts_tree() {
                    a.emit(spawned(e)).await;
                }
                a.ctrl('t');
                let lines = a.settle().await;
                line_with(
                    &lines,
                    if takes_effort {
                        " main   sonnet-5-5 low"
                    } else {
                        " main   sonnet-5-5"
                    },
                );
                line_with(&lines, "   claude-subscription \u{b7} subscription");
                a.key(KeyCode::Down);
                let lines = a.settle().await;
                line_with(
                    &lines,
                    if takes_effort {
                        " coder   sonnet-5-5 high"
                    } else {
                        " coder   sonnet-5-5"
                    },
                );
                if !takes_effort {
                    none_with(&lines, "sonnet-5-5 high");
                    none_with(&lines, "xhigh");
                    snapshot(
                        "no-effort-agents",
                        &[("list".to_owned(), a.screen.styled())],
                    );
                }
                line_with(&lines, "   claude-work \u{b7} subscription");
                none_with(&lines, "claude-sonnet");
                a.key(KeyCode::Down);
                let lines = a.settle().await;
                line_with(&lines, " reviewer ");
                line_with(&lines, "   claude-subscription \u{b7} subscription");
                a.key(KeyCode::Esc);
                a.ctrl('t').key(KeyCode::Down).key(KeyCode::Enter);
                a.emit(super::agent_keys::wrapped(
                    "/coder",
                    AgentEvent::turn_started(1),
                ))
                .await;
                sleep(Duration::from_secs(5)).await;
                let lines = a.settle().await;
                assert_eq!(
                    lines
                        .iter()
                        .any(|r| r.contains("thinking with high effort")),
                    takes_effort,
                    "{lines:#?}"
                );
                if !takes_effort {
                    none_with(&lines, "sonnet-5-5 high");
                    snapshot(
                        "no-effort-child-thinking",
                        &[("thinking".to_owned(), a.screen.styled())],
                    );
                }
                a.key(KeyCode::Esc);
                a.ctrl('c').ctrl('c');
            },
        )
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn the_chip_takes_its_word_from_the_sessions_billing_rule() {
    let mut facts = two_subscriptions();
    facts.endpoints[1].credential = None;
    run_agents_on(
        Screen::new(24, 120),
        facts,
        None,
        options(Start::Stalled),
        |a| async move {
            for e in accounts_tree() {
                a.emit(spawned(e)).await;
            }
            a.ctrl('t').key(KeyCode::Down);
            line_with(&a.settle().await, "   claude-work \u{b7} subscription");
            a.key(KeyCode::Down).key(KeyCode::Down);
            let lines = a.settle().await;
            line_with(&lines, " scout ");
            line_with(&lines, "   claude-api \u{b7} API");
            a.key(KeyCode::Esc);
            a.ctrl('c').ctrl('c');
        },
    )
    .await;
}

fn topology(nodes: &[(&str, Option<&str>)]) -> AgentEvent {
    let nodes = nodes
        .iter()
        .map(|(key, parent)| SlotTopologyNode {
            key: (*key).to_owned(),
            parent: parent.map(str::to_owned),
            state: NodeState::Working,
        })
        .collect();
    let topology = SlotBlock::Topology {
        root: Some("main".into()),
        nodes,
        flows: Vec::new(),
        counted: Some(SlotNoun::Agents),
    };
    content(AGENTS_SECTION, vec![topology])
}

fn ticking_now() -> jiff::Timestamp {
    static START: std::sync::OnceLock<tokio::time::Instant> = std::sync::OnceLock::new();
    let start = *START.get_or_init(tokio::time::Instant::now);
    crate::shared::fixed_now() + jiff::SignedDuration::try_from(start.elapsed()).unwrap()
}

#[tokio::test(start_paused = true)]
async fn a_light_on_the_braille_page_asks_for_33_ms_frames_until_its_ring_has_gone() {
    for full in [false, true] {
        light_frames(full).await;
    }
}

async fn light_frames(full: bool) {
    let mut options = options(Start::Stalled);
    options.now = ticking_now;
    options.full_repaint = full;
    steering_on(screen(), options, |a| async move {
        let boxed = SlotEntry {
            default_on: false,
            ..slot(AGENTS_SECTION, "Agents")
        };
        a.emit(catalog(vec![plugin("agent-viz", true, vec![boxed])]))
            .await;
        a.ctrl('t');
        a.settle().await;
        let node = |key: &str, parent: Option<&str>| SlotTopologyNode {
            key: key.to_owned(),
            parent: parent.map(str::to_owned),
            state: NodeState::Done,
        };
        let topology = SlotBlock::Topology {
            root: Some("main".into()),
            nodes: vec![node("/lead", None), node("/lead/coder", Some("/lead"))],
            flows: vec![SlotFlow {
                from: Some("/lead/coder".to_owned()),
                to: Some("/lead".to_owned()),
                at_ms: u64::try_from(ticking_now().as_millisecond()).unwrap(),
            }],
            counted: Some(SlotNoun::Agents),
        };
        a.emit(content(AGENTS_SECTION, vec![topology])).await;
        let frames = async || {
            let before = a.screen.frames();
            sleep(Duration::from_millis(600)).await;
            a.screen.frames() - before
        };
        let flying = frames().await;
        if full {
            assert_eq!(flying, 0, "full repaint keeps the light still");
        } else {
            assert!(flying >= 8, "{flying} frames while the light flies");
        }
        sleep(Duration::from_millis(1000)).await;
        assert_eq!(frames().await, 0, "none once the ring has gone");
        a.key(KeyCode::Esc);
        a.ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn with_the_box_off_arrows_pick_a_dot_an_ended_pick_climbs_and_enter_opens_its_chat() {
    let mut f = facts();
    f.endpoint_name = Some("work".to_owned());
    f.endpoints = vec![wisp_render::chrome::EndpointFacts {
        name: "work".to_owned(),
        ..super::session_account(wisp_render::chrome::Endpoint::Official)
    }];
    steering_facts(screen(), options(Start::Stalled), f, |mut a| async move {
        let boxed = SlotEntry {
            default_on: false,
            ..slot(AGENTS_SECTION, "Agents")
        };
        a.emit(catalog(vec![plugin(
            "agent-viz",
            true,
            vec![boxed.clone()],
        )]))
        .await;
        a.emit(spawned(entry("/lead", AgentState::Running, 0.4)))
            .await;
        a.emit(spawned(entry("/lead/coder", AgentState::Running, 0.2)))
            .await;
        a.emit(topology(&[("/lead", None), ("/lead/coder", Some("/lead"))]))
            .await;
        let task = mail("t1", "/lead", MailKind::Task, true, "Build it");
        a.emit(AgentEvent::Agent {
            agent: AgentPath::new("/lead/coder"),
            event: Box::new(task),
        })
        .await;
        a.ctrl('t');
        let lines = a.settle().await;
        line_with(&lines, " main   test-model low");
        none_with(&lines, "agent-viz is off");
        a.key(KeyCode::Down);
        line_with(&a.settle().await, " lead   ");
        a.key(KeyCode::Down);
        let lines = a.settle().await;
        line_with(&lines, " coder   ");
        line_with(&lines, "task from /lead, 1 min ago");
        a.emit(changed(
            "/lead/coder",
            AgentState::Ended,
            Activity::Idle,
            0.2,
        ))
        .await;
        a.emit(topology(&[("/lead", None)])).await;
        line_with(&a.settle().await, " lead   ");
        a.key(KeyCode::Enter);
        let sent = drain(&mut a).await;
        assert!(sent.contains(&watch("/lead", true)), "{sent:?}");
        a.key(KeyCode::Esc);
        a.ctrl('t');
        a.settle().await;
        sleep(Duration::from_millis(450)).await;
        let (column, row) = locate(&a.screen, "\u{25cf}");
        drain(&mut a).await;
        super::select::account_chip(&a, "work");
        let ClientCommand::DescribeSettings { request } = a.next().await else {
            panic!("expected account picker settings");
        };
        let AgentEvent::SettingsSnapshot(mut snapshot) = super::described(request.0) else {
            unreachable!();
        };
        snapshot.plugins = vec![plugin("agent-viz", true, vec![boxed])];
        snapshot.endpoints = vec![super::endpoints::summary("work", false)];
        a.emit(AgentEvent::SettingsSnapshot(snapshot)).await;
        assert!(
            !a.settle()
                .await
                .iter()
                .any(|line| line.contains(" main   "))
        );
        click(&a, column, row);
        let sent = drain(&mut a).await;
        assert!(
            !sent.contains(&watch("/lead", true)),
            "hidden agent opened: {sent:?}"
        );
        a.settle().await;
        let (column, row) = locate(&a.screen, "\u{25cf}");
        click(&a, column, row);
        let sent = drain(&mut a).await;
        assert!(sent.contains(&watch("/lead", true)), "{sent:?}");
        a.key(KeyCode::Esc);
        a.ctrl('c').ctrl('c');
    })
    .await;
}
