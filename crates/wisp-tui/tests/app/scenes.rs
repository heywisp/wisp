use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::rc::Rc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::sleep;
use wisp_protocol::{
    AgentEvent, BlockKind, CacheTtl, Cost, DeliveryMode, ErrorDetail, ErrorKind, FailurePhase,
    FailureSource, InputId, RejectKind, RetryClass, StopReason, TurnOutcome, WithdrawnBy,
};
use wisp_render::markdown::{Layout, Renderer};
use wisp_render::motion::MotionPolicy;
use wisp_render::styled::StyledLine;
use wisp_render::theme::glyphs::GlyphSet;
use wisp_render::theme::{Depth, Mode, Theme, Token};
use wisp_render::transcript::{Config, Transcript, UserPart, View as TranscriptView, copy};
use wisp_tui::app::highlight::Start;
use wisp_tui::app::{Credential, Options, Session, SessionFacts, run};
use wisp_tui::term::{Input, Key, KeyCode, Mouse, MouseKind};

use super::agents::main_spent;
use super::models::{models, multipliers};
use super::scratch::ScratchDir;
use super::{SCRATCH, Screen, ctrl, described};
use crate::Drive;
use crate::events::{Changed, End, Ending, cancelled, cost, ended, outcome, usage};
use crate::shared::endpoint;
use wisp_protocol::{NoSearch, SearchForm};
use wisp_render::chrome::{Endpoint, EndpointFacts, SearchForms, SearchPair};

#[derive(Clone, Copy, Debug)]
struct Look {
    mode: Mode,
    depth: Depth,
    glyphs: GlyphSet,
    width: u16,
    rows: u16,
    motion: MotionPolicy,
}

impl Look {
    fn name(self) -> String {
        let mode = match self.mode {
            Mode::Dark => "dark",
            Mode::Light => "light",
        };
        let depth = match self.depth {
            Depth::TrueColor => "truecolor",
            Depth::Ansi256 => "256",
            Depth::Ansi16 => "16",
            Depth::None => "none",
        };
        let glyphs = match self.glyphs {
            GlyphSet::Unicode => "unicode",
            GlyphSet::AmbiguousWide => "ambiguous",
            GlyphSet::Ascii => "ascii",
        };
        let base = if self.rows == ROWS {
            format!("{mode}-{depth}-{glyphs}-{}", self.width)
        } else {
            format!("{mode}-{depth}-{glyphs}-{}x{}", self.width, self.rows)
        };
        match self.motion {
            MotionPolicy::Full => base,
            MotionPolicy::Calm => format!("{base}-calm"),
            MotionPolicy::Reduced => format!("{base}-reduced"),
        }
    }
}

const ROWS: u16 = 50;

fn facts() -> SessionFacts {
    SessionFacts {
        version: "0.1.0".to_owned(),
        provider: "Anthropic".to_owned(),
        model: "claude-sonnet-5-5".to_owned(),
        effort: "low".to_owned(),
        cache_ttl: CacheTtl::OneHour,
        credential: Some(Credential::ApiKey),
        host: None,
        log: Some("~/.local/share/wisp/sessions/0193a.jsonl".to_owned()),
        home: None,
        cwd: "~/Projects/Wisp".to_owned(),
        endpoint: Endpoint::Official,
        models: models(),
        on_accounts: Vec::new(),
        write_multipliers: multipliers(2.0),
        next: None,
        max_tokens: 64_000,
        endpoint_name: None,
        endpoints: vec![crate::session_account(Endpoint::Official)],
        endpoint_note: None,
        resume_refused: false,
        instructions: Default::default(),
        search: Default::default(),
        providers_off: Default::default(),
        resumed: None,
        claude_usage: false,
        start_pick: None,
    }
}

fn completed(stop_reason: StopReason, committed: bool, cost: Option<Cost>) -> TurnOutcome {
    outcome(End {
        stop_reason,
        model: "claude-sonnet-5-5".to_owned(),
        usage: Some(usage()),
        cost,
        committed,
        ..End::default()
    })
}

fn tag(cell: &vt100::Cell) -> String {
    let mut parts = vec![];
    if cell.fgcolor() != vt100::Color::Default {
        parts.push(format!("fg {:?}", cell.fgcolor()));
    }
    if cell.bgcolor() != vt100::Color::Default {
        parts.push(format!("bg {:?}", cell.bgcolor()));
    }
    for (on, name) in [
        (cell.bold(), "bold"),
        (cell.dim(), "dim"),
        (cell.italic(), "italic"),
        (cell.underline(), "underline"),
        (cell.inverse(), "inverse"),
    ] {
        if on {
            parts.push(name.to_owned());
        }
    }
    parts.join(" ")
}

fn dump_row(screen: &vt100::Screen, row: u16, cols: u16) -> String {
    let mut text = String::new();
    let mut runs: Vec<(u16, u16, String)> = vec![];
    for col in 0..cols {
        let Some(cell) = screen.cell(row, col) else {
            continue;
        };
        if cell.is_wide_continuation() {
            continue;
        }
        text.push_str(if cell.has_contents() {
            cell.contents()
        } else {
            " "
        });
        let tag = tag(cell);
        match runs.last_mut() {
            Some((_, end, last)) if *last == tag && *end == col => *end = col + 1,
            _ => runs.push((col, col + 1, tag)),
        }
    }
    let mut out = format!("|{}\n", text.trim_end());
    let styled: Vec<String> = runs
        .into_iter()
        .filter(|(_, _, tag)| !tag.is_empty())
        .map(|(a, b, tag)| format!("{a}..{b} {tag}"))
        .collect();
    if !styled.is_empty() {
        out.push_str(&format!("  ~ {}\n", styled.join("; ")));
    }
    out
}

fn collapse_blank_runs(rows: Vec<String>) -> String {
    let mut out = String::new();
    let mut blank = 0;
    let flush = |out: &mut String, blank: &mut usize| {
        match *blank {
            0 => {}
            1 | 2 => out.push_str(&"|\n".repeat(*blank)),
            n => out.push_str(&format!("|({n} blank rows)\n")),
        }
        *blank = 0;
    };
    for row in rows {
        if row == "|\n" {
            blank += 1;
        } else {
            flush(&mut out, &mut blank);
            out.push_str(&row);
        }
    }
    flush(&mut out, &mut blank);
    out
}

impl Screen {
    pub(super) fn styled_rows(&self) -> Vec<String> {
        let state = self.0.borrow();
        let screen = state.parser.screen();
        let (rows, cols) = screen.size();
        (0..rows.get())
            .map(|row| dump_row(screen, row, cols.get()))
            .collect()
    }

    pub(super) fn styled(&self) -> String {
        let state = self.0.borrow();
        let screen = state.parser.screen();
        let (rows, cols) = screen.size();
        let out: Vec<String> = (0..rows.get())
            .map(|row| dump_row(screen, row, cols.get()))
            .collect();
        collapse_blank_runs(out)
    }

    fn status_row(&self) -> String {
        let state = self.0.borrow();
        let screen = state.parser.screen();
        let (rows, cols) = screen.size();
        let row = (0..rows.get())
            .rev()
            .find(|&row| {
                screen
                    .rows(0, cols.get())
                    .nth(usize::from(row))
                    .is_some_and(|l| !l.trim().is_empty())
            })
            .unwrap_or(0);
        let shown: Vec<String> = screen.rows(0, cols.get()).collect();
        let row = if super::framed_status(&shown[..=usize::from(row)]) {
            row - 1
        } else {
            row
        };
        dump_row(screen, row, cols.get())
    }
}

type Shots = Rc<RefCell<Vec<(String, String)>>>;

struct Stage {
    screen: Screen,
    inputs: mpsc::UnboundedSender<Input>,
    events: mpsc::Sender<AgentEvent>,
    shots: Shots,
    sent: std::cell::Cell<u64>,
    turns: std::cell::Cell<u64>,
}

impl Stage {
    async fn wait(&self, ms: u64) -> &Stage {
        sleep(Duration::from_millis(ms)).await;
        tokio::task::yield_now().await;
        self
    }

    async fn open_transcript(&self, gap: u64) -> &Stage {
        self.input(Input::Key(Key::ctrl('o'))).wait(gap).await;
        if super::picker_shown(&self.screen.visible()) {
            self.input(Input::Key(Key::ctrl('o')));
        }
        self
    }

    async fn event(&self, e: AgentEvent) -> &Stage {
        for e in super::agents::with_spend(e) {
            self.events.send(e).await.unwrap();
        }
        self
    }

    async fn block(&self, index: usize, kind: BlockKind) -> &Stage {
        self.event(AgentEvent::BlockStarted { index, kind }).await
    }

    async fn think(&self, index: usize, text: &str) -> &Stage {
        self.block(index, BlockKind::Thinking).await;
        self.event(AgentEvent::ThinkingDelta {
            index,
            text: text.to_owned(),
        })
        .await
    }

    async fn text(&self, index: usize, text: &str) -> &Stage {
        self.event(AgentEvent::TextDelta {
            index,
            text: text.to_owned(),
        })
        .await
    }

    async fn answer(&self, index: usize, text: &str) -> &Stage {
        self.block(index, BlockKind::Text).await;
        self.text(index, text).await
    }

    async fn stop(&self, index: usize) -> &Stage {
        self.event(AgentEvent::BlockStopped { index }).await
    }

    async fn send(&self, text: &str) -> &Stage {
        self.say(text).wait(1).await;
        self.deliver().await
    }

    async fn deliver(&self) -> &Stage {
        self.sent.set(self.sent.get() + 1);
        self.turns.set(self.turns.get() + 1);
        self.event(AgentEvent::TurnStarted {
            turn: self.turns.get(),
            origin: None,
            answers: None,
            retry_of: None,
            ends: Vec::new(),
        })
        .await;
        self.event(AgentEvent::InputDelivered {
            ids: vec![InputId(format!("tui-{}", self.sent.get()))],
            step: 1,
        })
        .await
    }

    async fn accepted(&self, mode: DeliveryMode, text: &str) -> InputId {
        self.sent.set(self.sent.get() + 1);
        let id = InputId(format!("tui-{}", self.sent.get()));
        self.wait(1).await;
        self.event(AgentEvent::InputAccepted {
            opening: false,
            front: false,
            id: id.clone(),
            mode,
            text: text.to_owned(),
            images: Vec::new(),
            replaces: None,
            held: false,
            from: None,
        })
        .await;
        id
    }

    fn bump(&self) -> InputId {
        self.sent.set(self.sent.get() + 1);
        InputId(format!("tui-{}", self.sent.get()))
    }

    async fn edited(&self, old: &InputId, mode: DeliveryMode, text: &str, held: bool) -> InputId {
        let id = self.bump();
        self.wait(1).await;
        self.event(AgentEvent::InputAccepted {
            opening: false,
            front: false,
            id: id.clone(),
            mode,
            text: text.to_owned(),
            images: Vec::new(),
            replaces: Some(old.clone()),
            held,
            from: None,
        })
        .await;
        self.event(AgentEvent::InputWithdrawn {
            ids: vec![old.clone()],
            by: WithdrawnBy::Replaced,
        })
        .await;
        id
    }

    async fn steer(&self, text: &str) -> InputId {
        self.say(text);
        self.accepted(DeliveryMode::Steer, text).await
    }

    async fn queue(&self, text: &str) -> InputId {
        self.paste(text).key(KeyCode::Tab);
        self.accepted(DeliveryMode::Queue, text).await
    }

    fn shot(&self, name: &str) {
        let screen = self.screen.styled();
        self.shots.borrow_mut().push((name.to_owned(), screen));
    }

    fn status(&self, name: &str) {
        let row = self.screen.status_row();
        self.shots.borrow_mut().push((name.to_owned(), row));
    }
}

impl Drive for Stage {
    fn inputs(&self) -> &mpsc::UnboundedSender<Input> {
        &self.inputs
    }
}

async fn staged<D, F>(look: Look, drive: D) -> Vec<(String, String)>
where
    D: FnOnce(Stage) -> F,
    F: Future<Output = ()>,
{
    staged_on(Screen::new(look.rows, look.width), look, drive).await
}

async fn staged_on<D, F>(screen: Screen, look: Look, drive: D) -> Vec<(String, String)>
where
    D: FnOnce(Stage) -> F,
    F: Future<Output = ()>,
{
    staged_with(screen, look, facts(), drive).await
}

async fn staged_with<D, F>(
    screen: Screen,
    look: Look,
    facts: SessionFacts,
    drive: D,
) -> Vec<(String, String)>
where
    D: FnOnce(Stage) -> F,
    F: Future<Output = ()>,
{
    staged_tuned(screen, look, facts, |_| {}, drive).await
}

async fn staged_tuned<D, F>(
    screen: Screen,
    look: Look,
    facts: SessionFacts,
    tune: impl FnOnce(&mut Options),
    drive: D,
) -> Vec<(String, String)>
where
    D: FnOnce(Stage) -> F,
    F: Future<Output = ()>,
{
    staged_session(screen, look, facts, None, tune, drive).await
}

async fn staged_session<D, F>(
    screen: Screen,
    look: Look,
    facts: SessionFacts,
    resumed: Option<wisp_tui::app::resumed::Resumed>,
    tune: impl FnOnce(&mut Options),
    drive: D,
) -> Vec<(String, String)>
where
    D: FnOnce(Stage) -> F,
    F: Future<Output = ()>,
{
    let (commands, sent) = super::without_terminal(64);
    let (events_in, events) = mpsc::channel(1024);
    let (inputs, received) = mpsc::unbounded_channel();
    let shots: Shots = Rc::default();
    let session = Session {
        leave_note: None,
        attached: None,
        alerts: Vec::new(),
        facts,
        events,
        commands,
        resumed,
    };
    let mut options = Options::new(Theme::new(look.mode, look.depth), look.glyphs);
    options.code = Start::Stalled;
    options.motion = look.motion;
    options.zone = jiff::tz::TimeZone::UTC;
    options.now = super::fixed_now;
    tune(&mut options);
    let mut console = screen.clone();
    let app = run(&mut console, received, session, options);
    let stage = Stage {
        screen: screen.clone(),
        inputs,
        events: events_in,
        shots: Rc::clone(&shots),
        sent: std::cell::Cell::new(0),
        turns: std::cell::Cell::new(0),
    };
    let driver = async move {
        sleep(Duration::from_millis(400)).await;
        drive(stage).await;
        drop(sent);
    };
    let (exit, ()) = tokio::join!(app, driver);
    exit.unwrap();
    shots.take()
}

fn snapshot(scene: &str, look: Look, shots: &[(String, String)]) {
    let body: String = shots
        .iter()
        .map(|(name, shot)| format!("== {name}\n{shot}"))
        .collect();
    let mut settings = insta::Settings::clone_current();
    settings.set_prepend_module_to_snapshot(false);
    settings.set_snapshot_path("../snapshots/chrome");
    settings.bind(|| {
        insta::assert_snapshot!(format!("{scene}-{}", look.name()), body);
    });
}

#[derive(Default)]
struct Twins {
    full: BTreeMap<String, Vec<(String, String)>>,
}

impl Twins {
    fn check(&mut self, scene: &str, look: Look, shots: &[(String, String)]) {
        let name = format!("{scene}-{}", look.name());
        if !matches!(look.motion, MotionPolicy::Reduced) {
            snapshot(scene, look, shots);
            self.full.insert(name, shots.to_vec());
            return;
        }
        let full = Look {
            motion: MotionPolicy::Full,
            ..look
        };
        let Some(twin) = self.full.get(&format!("{scene}-{}", full.name())) else {
            panic!("{name} has no full look checked before it");
        };
        assert_eq!(shots, twin.as_slice(), "{name} differs from its full look");
    }
}

const ANSWER: &str = "Misses now say which part of the prefix changed: the system prompt, the tools or the messages. The prefix-stability test covers each case.\n\n- the system prompt\n- the tool list\n- the messages\n";

const CODE: &str = "Here is the change:\n\n```rust\nuse std::collections::BTreeMap;\n\npub struct Fingerprint {\n    system: u64,\n    tools: u64,\n    messages: Vec<u64>,\n}\n\nimpl Fingerprint {\n    pub fn of(req: &Request) -> Result<Fingerprint, Error> {\n        let system = hash(&req.system);\n        let tools = hash(&req.tools);\n        let messages = req.messages.iter().map(hash).collect();\n        Ok(Fingerprint { system, tools, messages })\n    }\n\n    pub fn cause(&self, before: &Fingerprint) -> Option<&'static str> {\n        if self.system != before.system {\n            return Some(\"system changed\");\n        }\n        if self.tools != before.tools {\n            return Some(\"tools changed\");\n        }\n        None\n    }\n}\n```\n\nThe test covers each cause.\n";

const TABLE: &str = "The two TTLs compare like this:\n\n| TTL | Write price | Use |\n|:----|----:|-----|\n| 5m | 1.25x | continuous loops |\n| 1h | 2x | interactive sessions |\n| none | 1x | one-shot calls such as titles |\n";

const THOUGHTS: &str = "Placeholder thought line one, wrapped narrowly.\nPlaceholder thought line two, longer than the first.\nPlaceholder thought, 3rd.\nPlaceholder thought line 4, middling.\nPlaceholder thought line five, medium.\nPlaceholder thought line six, rather long.\nPlaceholder thought seven.\nPlaceholder 8th.\nPlaceholder 9.\n";

const STREAMING: &str = "I'll start with how request prefixes are built:\n\n```rust\nlet key = Fingerprint::of(req)?;\n```\n\nMisses now say which part of the prefix changed: the system prompt, the tools or the";

async fn turn(s: Stage) {
    s.send("Make cache misses name their cause").await;
    s.wait(100).await;
    s.think(
        0,
        "Placeholder thought sentence, long enough to wrap at eighty columns, filling it.\n",
    )
    .await;
    s.wait(4000).await.stop(0).await;
    s.answer(1, ANSWER).await.stop(1).await;
    s.wait(13_900).await;
    s.event(ended(
        completed(StopReason::EndTurn, true, Some(cost(0.014, 0.014, false))),
        18_000,
    ))
    .await;
    s.wait(500).await.shot("done");
    s.wait(3000).await.shot("settled");
}

async fn thinking(s: Stage) {
    s.send("Why do misses lose their cause?").await;
    s.wait(100).await.think(0, THOUGHTS).await;
    s.wait(6000).await.stop(0).await;
    s.answer(1, "It drops the reason at the call site.\n")
        .await
        .stop(1)
        .await;
    s.event(ended(
        completed(StopReason::EndTurn, true, Some(cost(0.01, 0.01, false))),
        6500,
    ))
    .await;
    s.wait(4000).await.send("And how do we fix it?").await;
    s.wait(100).await.think(0, THOUGHTS).await;
    s.wait(4400).await.shot("thinking");
}

async fn code(s: Stage) {
    s.send("Show me the fingerprint").await;
    s.wait(500).await.answer(0, CODE).await.stop(0).await;
    s.event(ended(
        completed(StopReason::EndTurn, true, Some(cost(0.021, 0.021, false))),
        9_000,
    ))
    .await;
    s.wait(9000).await.shot("code");
}

async fn table(s: Stage) {
    s.send("Compare the cache TTLs").await;
    s.wait(500).await.answer(0, TABLE).await.stop(0).await;
    s.event(ended(
        completed(StopReason::EndTurn, true, Some(cost(0.004, 0.004, false))),
        3_000,
    ))
    .await;
    s.wait(3000).await.shot("table");
}

async fn footers(s: Stage) {
    let lower = completed(StopReason::EndTurn, true, Some(cost(0.5, 0.6, true)));
    let outcomes: Vec<(TurnOutcome, bool)> = vec![
        (
            completed(StopReason::EndTurn, true, Some(cost(0.014, 0.014, false))),
            false,
        ),
        (
            completed(StopReason::EndTurn, true, Some(cost(0.02, 0.034, false))),
            true,
        ),
        (
            completed(StopReason::MaxTokens, true, Some(cost(0.03, 0.064, false))),
            false,
        ),
        (
            completed(StopReason::EndTurn, false, Some(cost(0.01, 0.074, false))),
            false,
        ),
        (completed(StopReason::EndTurn, true, None), false),
        (lower, false),
        (cancelled(), false),
        (
            outcome(End {
                ending: Ending::Error(ErrorKind::Overloaded),
                message: "overloaded_error: try again".to_owned(),
                ..End::default()
            }),
            false,
        ),
    ];
    for (i, (outcome, miss)) in outcomes.into_iter().enumerate() {
        s.send(&format!("Question {}", i + 1)).await;
        if miss {
            s.event(AgentEvent::CacheMiss {
                previous_prompt_tokens: 9_000,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 9_000,
                diagnostics: None,
                cause: None,
            })
            .await;
        }
        s.wait(300)
            .await
            .answer(0, "An answer.\n")
            .await
            .stop(0)
            .await;
        s.wait(2100).await.event(ended(outcome, 2400)).await;
        s.wait(2600).await;
    }
    s.shot("footers");
}

fn failure(phase: FailurePhase, status: u16, error_type: &str, request: &str) -> ErrorDetail {
    ErrorDetail {
        limit_reached: None,
        model_limit: None,
        status: Some(status),
        error_type: Some(error_type.to_owned()),
        request_id: Some(request.to_owned()),
        ..ErrorDetail::new(phase, FailureSource::Endpoint, RetryClass::Transient)
    }
}

async fn details(s: Stage) {
    s.send("Question 1").await;
    s.event(AgentEvent::Retrying {
        attempt: 1,
        delay_ms: 2_000,
        reason: "overloaded".to_owned(),
        step: 1,
        kind: ErrorKind::Overloaded,
        detail: Some(failure(
            FailurePhase::Response,
            529,
            "overloaded_error",
            "req_011Retry",
        )),
        max_attempts: 5,
        discarded_blocks: 0,
        discarded_chars: 0,
        usage: None,
        model: None,
        cost_usd: None,
        lower_bound: false,
        resend: None,
        busy: None,
    })
    .await;
    s.wait(2100).await;
    s.event(AgentEvent::CacheMiss {
        previous_prompt_tokens: 9_000,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 9_000,
        diagnostics: Some(serde_json::json!({"changed": "tools"})),
        cause: None,
    })
    .await;
    s.wait(300)
        .await
        .answer(0, "An answer.\n")
        .await
        .stop(0)
        .await;
    let capped = completed(StopReason::MaxTokens, false, Some(cost(0.5, 0.6, true)));
    s.wait(2100).await.event(ended(capped, 2400)).await;
    s.wait(2600).await;
    s.send("Question 2").await;
    s.wait(300)
        .await
        .answer(0, "Partial.\n")
        .await
        .stop(0)
        .await;
    let stopped = TurnOutcome::Cancelled {
        usage: Some(usage()),
        cost: Some(cost(0.003, 0.603, true)),
        committed_steps: 0,
    };
    s.wait(500).await.event(ended(stopped, 900)).await;
    s.wait(2600).await;
    s.send("Question 3").await;
    let failed = outcome(End {
        ending: Ending::Error(ErrorKind::Server),
        message: "Upstream provider stream was interrupted.".to_owned(),
        cost: Some(cost(0.2, 0.803, true)),
        detail: Some(failure(
            FailurePhase::Stream,
            200,
            "api_error",
            "req_011Stream",
        )),
        ..End::default()
    });
    s.wait(300).await.event(ended(failed, 1200)).await;
    s.wait(2600).await;
    s.shot("main");
    s.input(ctrl('o'));
    s.wait(300).await;
    s.shot("ctrl-o");
    s.key(KeyCode::Esc);
    s.wait(300).await;
}

async fn showcase(s: Stage) {
    s.send("Make cache misses name their cause").await;
    s.wait(100).await.think(0, THOUGHTS).await;
    s.wait(4000).await.stop(0).await;
    s.answer(1, STREAMING).await;
    s.wait(500).await.shot("answering");
}

async fn states(s: Stage) {
    s.status("ready");
    s.paste("draft").wait(1000).await.status("listening");
    s.send("draft").await;
    s.wait(4500).await.status("thinking");
    s.event(AgentEvent::Retrying {
        attempt: 1,
        delay_ms: 8000,
        reason: "overloaded".to_owned(),
        step: 1,
        kind: wisp_protocol::ErrorKind::Overloaded,
        detail: None,
        max_attempts: 5,
        discarded_blocks: 0,
        discarded_chars: 0,
        usage: None,
        model: None,
        cost_usd: None,
        lower_bound: false,
        resend: None,
        busy: None,
    })
    .await;
    s.wait(40).await.status("retrying");
    s.answer(0, "Hi").await.wait(100).await.status("answering");
    s.block(
        1,
        BlockKind::ToolUse {
            writing: None,
            name: "web_search".to_owned(),
        },
    )
    .await;
    s.wait(1000).await.status("working");
    s.stop(1).await;
    s.event(ended(
        completed(StopReason::EndTurn, true, Some(cost(0.014, 0.21, false))),
        18_000,
    ))
    .await;
    s.wait(1000).await.status("done");
    s.wait(3000).await.send("again").await;
    s.answer(0, "Hi").await.wait(500).await;
    s.event(ended(cancelled(), 1_000)).await;
    s.wait(100).await.status("stopped");
}

async fn pickers(s: Stage) {
    s.send("Make cache misses name their cause").await;
    s.wait(100).await.answer(0, ANSWER).await.stop(0).await;
    s.event(AgentEvent::StepEnded {
        step: 1,
        usage: Some(usage()),
        cost_usd: Some(0.014),
        lower_bound: false,
    })
    .await;
    s.event(ended(
        completed(StopReason::EndTurn, true, Some(cost(0.014, 0.014, false))),
        2000,
    ))
    .await;
    s.wait(4000).await.say("/model");
    s.wait(20).await.event(described(1)).await;
    s.wait(100).await.shot("models");
    s.input(Input::Key(Key::plain(KeyCode::Down)));
    s.wait(100).await.shot("moved");
    s.paste("opus");
    s.wait(100).await.shot("filtered");
    for _ in 0..4 {
        s.input(Input::Key(Key::plain(KeyCode::Backspace)));
    }
    s.paste("my-gateway/model");
    s.wait(100).await.shot("free text");
    s.input(Input::Key(Key::plain(KeyCode::Esc)));
    s.say("/effort");
    s.wait(100).await.shot("efforts");
    s.input(Input::Key(Key::plain(KeyCode::Esc)));
    s.event(crate::events::settings_changed(Changed {
        endpoint: "claude-api".into(),
        model: "claude-opus-5-5".to_owned(),
        effort: wisp_protocol::Effort::Medium,
        cache: wisp_protocol::CacheEffect::Rebuilt {
            reason: wisp_protocol::RebuildReason::ModelSwitch,
        },
        ..Changed::default()
    }))
    .await;
    s.wait(100).await.shot("pending");
}

pub(super) fn provider_of(credential: Credential) -> &'static str {
    if credential == Credential::ChatGptPlan {
        "openai"
    } else {
        "anthropic"
    }
}

pub(super) fn on_accounts(base: SessionFacts, accounts: &[(&str, Credential)]) -> SessionFacts {
    let endpoints = accounts
        .iter()
        .map(|&(name, credential)| EndpointFacts {
            provider: provider_of(credential).into(),
            ..endpoint(name, Endpoint::Official, credential, None, None)
        })
        .collect();
    SessionFacts {
        credential: Some(accounts[0].1),
        endpoint_name: Some(accounts[0].0.to_owned()),
        endpoints,
        ..base
    }
}

fn on_manifest() -> SessionFacts {
    SessionFacts {
        credential: Some(Credential::AuthToken),
        host: Some("gw.example.com".to_owned()),
        endpoint: Endpoint::Custom,
        endpoint_name: Some("manifest".to_owned()),
        endpoints: vec![
            endpoint("direct", Endpoint::Official, Credential::ApiKey, None, None),
            endpoint(
                "gateway",
                Endpoint::Custom,
                Credential::AuthToken,
                Some("gw2.example.com"),
                None,
            ),
            endpoint(
                "manifest",
                Endpoint::Custom,
                Credential::AuthToken,
                Some("gw.example.com"),
                None,
            ),
            endpoint(
                "work",
                Endpoint::Official,
                Credential::ApiKey,
                None,
                Some("WORK_KEY is not set"),
            ),
        ],
        ..facts()
    }
}

async fn endpoints(s: Stage) {
    s.wait(900).await.shot("welcome");
    s.send("Make cache misses name their cause").await;
    s.wait(100).await.answer(0, ANSWER).await.stop(0).await;
    s.event(AgentEvent::StepEnded {
        step: 1,
        usage: Some(usage()),
        cost_usd: Some(0.014),
        lower_bound: false,
    })
    .await;
    s.event(ended(
        completed(StopReason::EndTurn, true, Some(cost(0.014, 0.014, false))),
        2000,
    ))
    .await;
    s.wait(4000).await.say("/endpoint");
    s.wait(20).await.event(described(1)).await;
    s.wait(100).await.shot("endpoints");
    s.input(Input::Key(Key::plain(KeyCode::Up)));
    s.wait(100).await.shot("moved");
    s.paste("ga");
    s.wait(100).await.shot("filtered");
    s.input(Input::Key(Key::plain(KeyCode::Esc)));
    s.event(crate::events::settings_changed(Changed {
        endpoint: "direct".into(),
        model: "claude-sonnet-5-5".to_owned(),
        cache: wisp_protocol::CacheEffect::Rebuilt {
            reason: wisp_protocol::RebuildReason::EndpointSwitch,
        },
        account_bound: vec!["claude-sonnet-5-5".to_owned()],
        ..Changed::default()
    }))
    .await;
    s.wait(100).await.shot("pending");
}

fn searching_on_manifest() -> SessionFacts {
    let facts = on_manifest();
    let mut pairs = vec![];
    for endpoint in ["direct", "gateway", "manifest"] {
        for model in &facts.models {
            let form = if endpoint == "direct" {
                SearchForm::Native
            } else {
                SearchForm::None(NoSearch::TurnedOff)
            };
            pairs.push(SearchPair {
                endpoint: endpoint.to_owned(),
                model: model.id.clone(),
                form,
            });
        }
    }
    SessionFacts {
        search: SearchForms {
            requested: true,
            chat_only: false,
            pairs,
            refused: Vec::new(),
            kept: None,
        },
        ..facts
    }
}

fn searching_on_direct() -> SessionFacts {
    let manifest = searching_on_manifest();
    let pairs = manifest
        .search
        .pairs
        .iter()
        .filter(|p| p.model != "claude-sonnet-4-6")
        .cloned()
        .collect();
    SessionFacts {
        credential: Some(Credential::ApiKey),
        host: None,
        endpoint: Endpoint::Official,
        endpoint_name: Some("direct".to_owned()),
        search: SearchForms {
            pairs,
            ..manifest.search.clone()
        },
        ..manifest
    }
}

fn settings_changed(endpoint: &str, reason: wisp_protocol::RebuildReason) -> AgentEvent {
    crate::events::settings_changed(Changed {
        endpoint: endpoint.into(),
        model: "claude-sonnet-5-5".to_owned(),
        cache: wisp_protocol::CacheEffect::Rebuilt { reason },
        ..Changed::default()
    })
}

async fn finished_turn(s: &Stage, ask: &str) {
    s.send(ask).await;
    s.wait(100).await;
    s.think(0, "Placeholder one-line thought of a past turn.\n")
        .await
        .stop(0)
        .await;
    s.answer(1, ANSWER).await.stop(1).await;
    s.event(AgentEvent::StepEnded {
        step: 1,
        usage: Some(usage()),
        cost_usd: Some(0.014),
        lower_bound: false,
    })
    .await;
    s.event(ended(
        completed(StopReason::EndTurn, true, Some(cost(0.014, 0.014, false))),
        2000,
    ))
    .await;
}

async fn search_notes(s: Stage) {
    s.wait(900).await.shot("welcome");
    finished_turn(&s, "Make cache misses name their cause").await;
    s.wait(4000).await.say("/model");
    s.wait(20).await.event(described(1)).await;
    s.wait(100).await.shot("models");
    s.key(KeyCode::Esc).say("/endpoint");
    s.wait(20).await.event(described(2)).await;
    s.wait(100).await.shot("endpoints");
    s.key(KeyCode::Esc);
    s.event(settings_changed(
        "direct",
        wisp_protocol::RebuildReason::EndpointSwitch,
    ))
    .await;
    s.wait(100).await.shot("pending");
    s.send("Search for the cache docs").await;
    s.event(AgentEvent::ToolsChanged {
        added: vec!["web_search".to_owned()],
        removed: Vec::new(),
        via: wisp_protocol::ToolChangeVia::Rebuild,
        cache: None,
    })
    .await;
    s.event(AgentEvent::FeatureStateChanged {
        endpoint: "direct".to_owned(),
        model: "claude-sonnet-5-5".to_owned(),
        feature: wisp_protocol::Feature::NativeWebSearch,
        state: wisp_protocol::log::FeatureState::FallenBack,
    })
    .await;
    s.event(settings_changed(
        "direct",
        wisp_protocol::RebuildReason::NativeSearchRefused,
    ))
    .await;
    s.event(AgentEvent::ToolsChanged {
        added: Vec::new(),
        removed: vec!["web_search".to_owned()],
        via: wisp_protocol::ToolChangeVia::Rebuild,
        cache: None,
    })
    .await;
    s.wait(100)
        .await
        .answer(0, "Here is what the docs say.\n")
        .await
        .stop(0)
        .await;
    s.event(ended(
        completed(StopReason::EndTurn, true, Some(cost(0.014, 0.028, false))),
        2000,
    ))
    .await;
    s.wait(4000).await.shot("refused");
    s.say("/model");
    s.wait(20).await.event(described(3)).await;
    s.wait(100).await.shot("refused models");
    s.key(KeyCode::Esc).say("/endpoint");
    s.wait(20).await.event(described(4)).await;
    s.wait(100).await.shot("refused endpoints");
    s.key(KeyCode::Esc);
}

async fn search_from_native(s: Stage) {
    s.wait(900).await.shot("welcome");
    finished_turn(&s, "Make cache misses name their cause").await;
    s.wait(4000).await.say("/model");
    s.wait(20).await.event(described(1)).await;
    s.wait(100).await.shot("models");
    s.key(KeyCode::Esc).say("/endpoint");
    s.wait(20).await.event(described(2)).await;
    s.wait(100).await.shot("endpoints");
    s.key(KeyCode::Esc);
    s.event(settings_changed(
        "manifest",
        wisp_protocol::RebuildReason::EndpointSwitch,
    ))
    .await;
    s.wait(100).await.shot("pending");
}

type Scene = fn(Stage) -> std::pin::Pin<Box<dyn Future<Output = ()>>>;

const MODES: [Mode; 1] = [Mode::Dark];
const WIDTHS: [u16; 2] = [80, 120];
const GLYPHS: [GlyphSet; 3] = [GlyphSet::Unicode, GlyphSet::AmbiguousWide, GlyphSet::Ascii];

fn truecolor(mode: Mode, glyphs: GlyphSet, width: u16) -> Look {
    Look {
        mode,
        depth: Depth::TrueColor,
        glyphs,
        width,
        rows: ROWS,
        motion: MotionPolicy::Full,
    }
}

#[tokio::test(start_paused = true)]
async fn every_scene_at_80_and_120_in_light_and_dark() {
    let scenes: [(&str, Scene); 5] = [
        ("turn", |s| Box::pin(turn(s))),
        ("thinking", |s| Box::pin(thinking(s))),
        ("code", |s| Box::pin(code(s))),
        ("table", |s| Box::pin(table(s))),
        ("footers", |s| Box::pin(footers(s))),
    ];
    for mode in MODES {
        for width in WIDTHS {
            let look = truecolor(mode, GlyphSet::Unicode, width);
            for (name, scene) in scenes {
                snapshot(name, look, &staged(look, scene).await);
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn the_footers_and_the_ctrl_o_view_are_pure_ascii_with_ascii_glyphs() {
    for width in WIDTHS {
        let look = truecolor(Mode::Dark, GlyphSet::Ascii, width);
        let shots = staged(look, details).await;
        for (name, shot) in &shots {
            assert!(shot.is_ascii(), "{name} at {width}:\n{shot}");
        }
        let detail = shots
            .iter()
            .find(|(name, _)| name == "ctrl-o")
            .map(|(_, shot)| shot.as_str())
            .unwrap_or_default();
        for want in [
            "- max_tokens - ",
            "x cancelled - ",
            "x failed - ",
            "+ - dropped from history",
            "HTTP 529 - ",
        ] {
            assert!(detail.contains(want), "{want:?} at {width}:\n{detail}");
        }
        snapshot("details", look, &shots);
    }
}

#[tokio::test(start_paused = true)]
async fn every_orb_state_in_each_glyph_set_at_80_and_120_in_light_and_dark() {
    for mode in MODES {
        for glyphs in GLYPHS {
            for width in WIDTHS {
                let look = truecolor(mode, glyphs, width);
                let shots = staged(look, states).await;
                snapshot("status", look, &shots);
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn the_pickers_in_each_glyph_set_at_80_and_120_in_light_and_dark() {
    for mode in MODES {
        for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
            for width in WIDTHS {
                let look = truecolor(mode, glyphs, width);
                snapshot(
                    "pickers",
                    look,
                    &staged(look, |s| Box::pin(pickers(s))).await,
                );
            }
        }
    }
    let narrow = truecolor(Mode::Dark, GlyphSet::Unicode, 40);
    snapshot(
        "pickers",
        narrow,
        &staged(narrow, |s| Box::pin(pickers(s))).await,
    );
}

async fn short_pickers(s: Stage) {
    s.send("Make cache misses name their cause").await;
    s.wait(100).await.answer(0, ANSWER).await.stop(0).await;
    s.event(ended(
        completed(StopReason::EndTurn, true, Some(cost(0.014, 0.014, false))),
        2000,
    ))
    .await;
    s.wait(4000).await.say("/model");
    s.wait(20).await.event(described(1)).await;
    s.wait(100).await.shot("models");
    for _ in 0..3 {
        s.input(Input::Key(Key::plain(KeyCode::Down)));
    }
    s.wait(100).await.shot("moved");
}

#[tokio::test(start_paused = true)]
async fn the_model_picker_on_short_terminals() {
    for mode in MODES {
        for rows in [7u16, 10, 13] {
            let look = Look {
                rows,
                ..truecolor(mode, GlyphSet::Unicode, 80)
            };
            snapshot(
                "pickers-short",
                look,
                &staged(look, |s| Box::pin(short_pickers(s))).await,
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn the_endpoint_picker_at_40_80_and_120_in_each_glyph_set_in_light_and_dark() {
    for mode in MODES {
        for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
            for width in [40, 80, 120] {
                let look = truecolor(mode, glyphs, width);
                let screen = Screen::new(look.rows, look.width);
                let shots =
                    staged_with(screen, look, on_manifest(), |s| Box::pin(endpoints(s))).await;
                snapshot("endpoints", look, &shots);
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn the_search_notes_in_each_glyph_set_at_80_and_120_in_light_and_dark() {
    for mode in MODES {
        for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
            for width in WIDTHS {
                let look = truecolor(mode, glyphs, width);
                let screen = Screen::new(look.rows, look.width);
                let shots = staged_with(screen, look, searching_on_manifest(), |s| {
                    Box::pin(search_notes(s))
                })
                .await;
                snapshot("search", look, &shots);
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn the_search_notes_from_a_native_pair_in_each_glyph_set_at_80_and_120_in_light_and_dark() {
    for mode in MODES {
        for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
            for width in WIDTHS {
                let look = truecolor(mode, glyphs, width);
                let screen = Screen::new(look.rows, look.width);
                let shots = staged_with(screen, look, searching_on_direct(), |s| {
                    Box::pin(search_from_native(s))
                })
                .await;
                snapshot("search-native", look, &shots);
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn narrow_terminals_at_60_and_40_columns() {
    for mode in MODES {
        for width in [60, 40] {
            let look = truecolor(mode, GlyphSet::Unicode, width);
            snapshot("narrow", look, &staged(look, showcase).await);
            snapshot("status", look, &staged(look, states).await);
        }
    }
}

#[tokio::test(start_paused = true)]
async fn a_24_row_terminal_in_each_glyph_set() {
    for mode in MODES {
        for glyphs in GLYPHS {
            for width in WIDTHS {
                let look = Look {
                    rows: 24,
                    ..truecolor(mode, glyphs, width)
                };
                snapshot("showcase", look, &staged(look, showcase).await);
            }
        }
    }
}

async fn selecting_in_the_box(s: Stage) {
    let mouse = |kind, column, row| {
        Input::Mouse(Mouse {
            kind,
            column,
            row,
            modified: false,
        })
    };
    s.paste(
        "Rename the cache fingerprint module so its name says what it compares, \
then update every caller and the prefix-stability test to match.",
    )
    .wait(300)
    .await;
    s.input(mouse(MouseKind::Press, 11, 8))
        .input(mouse(MouseKind::Drag, 30, 8))
        .input(mouse(MouseKind::Drag, 24, 9))
        .wait(300)
        .await
        .shot("selecting");
}

#[tokio::test(start_paused = true)]
async fn a_selection_in_the_composer_box_tints_only_the_input() {
    for mode in MODES {
        let look = Look {
            rows: 12,
            ..truecolor(mode, GlyphSet::Unicode, 80)
        };
        snapshot(
            "composer-select",
            look,
            &staged(look, selecting_in_the_box).await,
        );
    }
}

async fn draft(s: Stage) {
    s.paste("A draft").wait(500).await.shot("box");
}

#[tokio::test(start_paused = true)]
async fn the_composer_box_spans_the_width_and_drops_at_20_columns() {
    for glyphs in GLYPHS {
        for width in [21u16, 20] {
            let look = Look {
                rows: 12,
                ..truecolor(Mode::Dark, glyphs, width)
            };
            let shots = staged(look, draft).await;
            snapshot("composer-box", look, &shots);
            let shot = &shots[0].1;
            let (top, divider, bottom, side) = match glyphs {
                GlyphSet::Unicode => (
                    ("\u{256D}", "\u{2500}", "\u{256E}"),
                    ("\u{251C}", "\u{2500}", "\u{2524}"),
                    ("\u{2570}", "\u{2500}", "\u{256F}"),
                    "\u{2502}",
                ),
                GlyphSet::AmbiguousWide | GlyphSet::Ascii => {
                    (("+", "-", "+"), ("+", "-", "+"), ("+", "-", "+"), "|")
                }
            };
            let across = usize::from(width) - 2;
            let edge = |(l, a, r): (&str, &str, &str)| format!("|{l}{}{r}", a.repeat(across));
            let lines: Vec<&str> = shot.lines().collect();
            let at = lines.iter().position(|l| *l == edge(top));
            if width > 20 {
                let at = at.unwrap_or_else(|| panic!("{shot}"));
                let input = lines[at + 6];
                assert!(input.starts_with(&format!("|{side} ")), "{shot}");
                assert!(input.contains("A draft") && input.ends_with(side), "{shot}");
                assert_eq!(lines[at + 4], edge(divider), "{shot}");
                assert_eq!(lines[at + 8], edge(divider), "{shot}");
                assert_eq!(lines[at + 12], edge(bottom), "{shot}");
            } else {
                assert!(at.is_none(), "{shot}");
                assert!(!lines.contains(&edge(bottom).as_str()), "{shot}");
            }
        }
    }
    let look = Look {
        rows: 12,
        ..truecolor(Mode::Dark, GlyphSet::Unicode, 80)
    };
    let shots = staged(look, draft).await;
    let shot = &shots[0].1;
    let lines: Vec<&str> = shot.lines().collect();
    let prompt = lines.iter().position(|l| l.contains("A draft")).unwrap();
    assert!(lines[prompt - 2].starts_with("|\u{251C}\u{2500}"), "{shot}");
    assert!(lines[prompt - 1].contains("~ 0..80 fg"), "{shot}");
    assert!(lines[prompt + 2].starts_with("|\u{251C}\u{2500}"), "{shot}");
}

#[tokio::test(start_paused = true)]
async fn the_showcase_across_the_full_matrix() {
    for mode in MODES {
        for depth in [Depth::TrueColor, Depth::Ansi256, Depth::Ansi16, Depth::None] {
            for glyphs in GLYPHS {
                for width in WIDTHS {
                    let look = Look {
                        mode,
                        depth,
                        glyphs,
                        width,
                        rows: ROWS,
                        motion: MotionPolicy::Full,
                    };
                    snapshot("showcase", look, &staged(look, showcase).await);
                }
            }
        }
    }
}

fn row_text(shot: &str) -> &str {
    shot.lines()
        .next()
        .and_then(|l| l.strip_prefix('|'))
        .unwrap_or_default()
}

#[tokio::test(start_paused = true)]
async fn every_status_scene_names_its_state() {
    let look = truecolor(Mode::Dark, GlyphSet::Unicode, 120);
    let shots = staged(look, states).await;
    let words: Vec<(&str, &str)> = shots
        .iter()
        .map(|(name, shot)| (name.as_str(), row_text(shot)))
        .collect();
    let answering = format!(
        "Kindling\u{2026} 4s \u{b7} \u{2193} \u{2248}1 token \u{b7} {}",
        copy::esc_to_stop().text()
    );
    let expected = [
        ("ready", "Ready"),
        ("listening", "sonnet-5-5"),
        (
            "thinking",
            "Kindling\u{2026} 4s \u{b7} thinking with low effort",
        ),
        ("retrying", "Retrying"),
        ("answering", answering.as_str()),
        ("working", "Writing a "),
        ("done", "Done"),
        ("stopped", "Stopped"),
    ];
    assert_eq!(words.len(), expected.len(), "{words:#?}");
    for ((name, row), (want, word)) in words.iter().zip(expected) {
        assert_eq!(*name, want);
        assert!(row.contains(word), "{name}: {row:?}");
    }
    assert!(!words[1].1.contains("Ready"), "{:?}", words[1]);
    assert!(words[5].1.contains(" call \u{b7} "), "{:?}", words[5]);
    for takes_effort in [false, true] {
        let mut f = facts();
        let mut model = f.models.iter().find(|m| m.id == f.model).unwrap().clone();
        model.id = "gemini-4-carbon-high".to_owned();
        model.caps.as_mut().unwrap().efforts = if takes_effort {
            vec![wisp_protocol::Effort::Low, wisp_protocol::Effort::High]
        } else {
            Vec::new()
        };
        f.model.clone_from(&model.id);
        f.effort = "xhigh".to_owned();
        f.models.push(model);
        let shots = staged_with(Screen::new(ROWS, look.width), look, f, |s| async move {
            s.shot("banner");
            s.say("/config effort").wait(20).await;
            s.event(AgentEvent::SettingsSnapshot(
                super::settings_screen::snapshot_of(wisp_protocol::RequestId(1)),
            ))
            .await;
            s.wait(100).await.shot("config");
            assert!(s.screen.visible().iter().any(|r| r.contains("Effort")));
            s.key(KeyCode::Esc).wait(20).await;
            states(s).await;
        })
        .await;
        for (name, shot) in &shots {
            if !takes_effort {
                assert!(!shot.contains("xhigh"), "{name}: {shot}");
                assert!(!shot.contains("thinking with"), "{name}: {shot}");
            } else if ["banner", "config", "ready", "listening", "thinking"]
                .contains(&name.as_str())
            {
                assert!(shot.contains("xhigh"), "{name}: {shot}");
            }
        }
        if !takes_effort {
            snapshot("no-effort-status", look, &shots[1..5]);
        }
    }
}

fn caret_ends(line: &str) -> bool {
    line.trim_end().ends_with('\u{2836}')
}

#[tokio::test(start_paused = true)]
async fn the_banner_reveal_plays_then_settles_as_the_first_item() {
    let look = truecolor(Mode::Dark, GlyphSet::Unicode, 80);
    let screen = Screen::new(ROWS, look.width);
    let (commands, sent) = super::without_terminal(8);
    let (events_in, events) = mpsc::channel(8);
    let (inputs, received) = mpsc::unbounded_channel::<Input>();
    let session = Session {
        leave_note: None,
        attached: None,
        alerts: Vec::new(),
        facts: facts(),
        events,
        commands,
        resumed: None,
    };
    let mut options = Options::new(Theme::new(look.mode, look.depth), look.glyphs);
    options.code = Start::Stalled;
    let mut console = screen.clone();
    let app = run(&mut console, received, session, options);
    let watched = screen.clone();
    let driver = async move {
        let mut marks = vec![];
        for _ in 0..8 {
            let lines = watched.visible();
            let mark = lines.iter().take(5).cloned().collect::<Vec<_>>().join("\n");
            marks.push(mark);
            sleep(Duration::from_millis(40)).await;
        }
        sleep(Duration::from_millis(1000)).await;
        let frames = watched.frames();
        let settled: Vec<String> = watched
            .visible()
            .into_iter()
            .take(ROWS as usize - 2)
            .collect();
        sleep(Duration::from_secs(5)).await;
        let moved = watched.frames() - frames;
        assert_eq!(moved, 0, "an idle screen drew {moved} frames");
        let after: Vec<String> = watched
            .visible()
            .into_iter()
            .take(ROWS as usize - 2)
            .collect();
        assert_eq!(after, settled, "the banner kept changing");
        drop(inputs);
        drop(events_in);
        drop(sent);
        marks
    };
    let (exit, marks) = tokio::join!(app, driver);
    exit.unwrap();
    let distinct: std::collections::BTreeSet<&String> = marks.iter().collect();
    assert!(distinct.len() >= 3, "{marks:#?}");
    let lines = screen.visible();
    let banners = lines
        .iter()
        .filter(|l| l.contains(copy::TAGLINE_OWN))
        .count();
    assert_eq!(banners, 1, "{lines:#?}");
    assert!(lines[0].starts_with('\u{256D}'), "{lines:#?}");
    assert!(
        lines[2].starts_with("\u{2502}    \u{2850}\u{2808} \u{2801}\u{2801}     wisp "),
        "{lines:#?}"
    );
    assert!(
        lines[3].starts_with(&format!(
            "\u{2502}   \u{2830}  \u{28BE}\u{2877}  \u{2806}   {}",
            copy::TAGLINE_OWN
        )),
        "{lines:#?}"
    );
}

#[tokio::test(start_paused = true)]
async fn the_caret_shows_only_while_a_reply_streams() {
    let look = truecolor(Mode::Dark, GlyphSet::Unicode, 80);
    let shots = staged(look, |s| async move {
        s.send("Hi").await;
        s.answer(0, "One line.\nTwo line").await;
        for chunk in [" more.\n", "Three.\n", "Four"] {
            s.wait(100).await.shot("streaming");
            s.text(0, chunk).await;
        }
        s.wait(100).await.shot("streaming");
        s.stop(0).await;
        s.event(ended(
            completed(StopReason::EndTurn, true, Some(cost(0.01, 0.01, false))),
            900,
        ))
        .await;
        s.wait(4000).await.shot("after");
    })
    .await;
    let rows = |shot: &str| -> Vec<String> {
        shot.lines()
            .filter_map(|l| l.strip_prefix('|'))
            .map(str::to_owned)
            .collect()
    };
    let carets = shots
        .iter()
        .filter(|(n, _)| n == "streaming")
        .filter(|(_, s)| rows(s).iter().any(|l| caret_ends(l)))
        .count();
    assert!(carets > 0, "the caret never showed in the live tail");
    let (_, after) = shots.last().unwrap();
    let after = rows(after);
    let status = after.len() - 1 - usize::from(super::framed_status(&after));
    for (i, row) in after.iter().enumerate() {
        assert!(!caret_ends(row), "{row:?}");
        if i != status && !row.contains(" wisp ") {
            assert!(!row.contains("\u{288E}\u{2836}\u{2870}"), "{row:?}");
        }
    }
    assert!(after.iter().any(|l| l.contains("Four")), "{after:#?}");
}

fn user_band_rows(width: usize, bands: bool) -> Vec<StyledLine> {
    let r = Renderer::new(Layout::new(width, GlyphSet::Unicode));
    let mut tr = Transcript::new(Config {
        provider: "Anthropic".to_owned(),
        cache_ttl: CacheTtl::OneHour,
        logging: true,
    })
    .with_view(TranscriptView {
        bands,
        ..TranscriptView::default()
    });
    tr.apply(&AgentEvent::turn_started(1), Duration::from_millis(5));
    tr.deliver(vec![UserPart::Text("Hi there".to_owned())], None);
    tr.frame(&r).commit
}

fn painted(
    theme: &Theme,
    line: &StyledLine,
    width: u16,
) -> Vec<Option<ratatui_core::style::Color>> {
    use ratatui_core::buffer::Buffer;
    use ratatui_core::layout::Rect;
    let mut row = Buffer::empty(Rect::new(0, 0, width, 1));
    row.set_line(0, 0, &theme.line(line), width);
    (0..width)
        .map(|x| {
            row[(x, 0)]
                .style()
                .bg
                .filter(|c| *c != ratatui_core::style::Color::Reset)
        })
        .collect()
}

#[test]
fn the_user_message_sits_on_a_band_to_the_content_edge_on_its_text_rows_only() {
    for (width, edge) in [
        (40usize, 40usize),
        (80, 80),
        (120, 120),
        (200, 200),
        (250, 250),
    ] {
        let rows = user_band_rows(width, true);
        let text = crate::testing::texts(&rows);
        assert_eq!(text, [" \u{203A} Hi there"], "{width}");
        for mode in MODES {
            for depth in [Depth::TrueColor, Depth::Ansi256] {
                let theme = Theme::new(mode, depth);
                assert!(theme.fills(Token::Surface));
                let surface = theme.bg(Token::Surface).bg;
                assert!(surface.is_some());
                for row in &rows {
                    let bg = painted(&theme, row, u16::try_from(width).unwrap());
                    for (x, cell) in bg.iter().enumerate() {
                        let want = if x < edge { surface } else { None };
                        assert_eq!(*cell, want, "{width} {mode:?} {depth:?} column {x}");
                    }
                }
            }
        }
    }
}

#[test]
fn the_user_message_has_no_band_in_ansi_16_or_no_color() {
    for mode in MODES {
        for depth in [Depth::Ansi16, Depth::None] {
            let theme = Theme::new(mode, depth);
            assert!(!theme.fills(Token::Surface), "{mode:?} {depth:?}");
            let rows = user_band_rows(80, theme.fills(Token::Surface));
            assert_eq!(rows.len(), 1, "{depth:?}: no band rows");
            for row in &rows {
                assert!(row.band.is_none());
                let line = theme.line(row);
                let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                assert_eq!(text, text.trim_end(), "{depth:?}: trailing spaces");
                assert!(
                    painted(&theme, row, 80).iter().all(Option::is_none),
                    "{mode:?} {depth:?}"
                );
            }
        }
    }
}

mod tool_events {
    use serde_json::json;
    use wisp_protocol::{
        AgentEvent, BlockKind, ExitStatus, Fact, NotRunReason, StopReason, ToolOutcome,
        ToolOutputView,
    };

    use crate::events::{Finished, Requested, tool_finished, tool_requested};
    pub(super) use crate::events::{ok, output, step_ended};

    pub(super) fn block(index: usize) -> AgentEvent {
        AgentEvent::BlockStarted {
            index,
            kind: BlockKind::ToolUse {
                writing: Some("a command".to_owned()),
                name: "shell".to_owned(),
            },
        }
    }

    pub(super) fn requested(step: u32, block: usize, id: &str, command: &str) -> AgentEvent {
        tool_requested(Requested {
            step,
            block,
            call_id: id.to_owned(),
            input: json!({"command": command, "description": "Checks the workspace"}),
            target: command.lines().next().unwrap_or_default().to_owned(),
            ..Requested::default()
        })
    }

    pub(super) fn started(id: &str) -> AgentEvent {
        AgentEvent::call_started(id.to_owned(), "~/Projects/Wisp")
    }

    pub(super) fn step(step: u32) -> AgentEvent {
        AgentEvent::StepStarted { step }
    }

    pub(super) fn lines(n: u64, word: &str) -> (String, u64) {
        let text: String = (1..=n).map(|i| format!("{word} {i} ... ok\n")).collect();
        (text, n)
    }

    pub(super) fn finished(
        id: &str,
        outcome: ToolOutcome,
        tail: &str,
        lines: u64,
        ms: u64,
    ) -> AgentEvent {
        let facts = match &outcome {
            ToolOutcome::Completed {
                exit: Some(ExitStatus::Code { code }),
                ..
            } => vec![Fact::Exit { code: *code }],
            _ => vec![],
        };
        tool_finished(Finished {
            call_id: id.to_owned(),
            outcome,
            output: ToolOutputView {
                tail: tail.to_owned(),
                bytes: tail.len() as u64,
                lines,
                file: Some(format!("/tmp/wisp-0193a/{id}-shell.out")),
                file_problem: None,
                inlined: true,
            },
            facts,
            duration_ms: ms,
            ..Finished::default()
        })
    }

    pub(super) fn with_file(mut event: AgentEvent, path: &str) -> AgentEvent {
        if let AgentEvent::ToolCallFinished { output, .. } = &mut event {
            output.file = Some(path.to_owned());
        }
        event
    }

    pub(super) fn not_run(reason: NotRunReason) -> ToolOutcome {
        ToolOutcome::NotRun { reason }
    }

    pub(super) fn max_tokens() -> NotRunReason {
        NotRunReason::ReplyStopped {
            stop_reason: StopReason::MaxTokens,
        }
    }
}

use tool_events as te;

async fn tools_live(s: Stage) {
    s.send("Run the checks").await;
    s.event(te::step(1)).await;
    s.wait(300).await.block(0, BlockKind::Thinking).await;
    s.stop(0).await;
    s.event(te::block(1)).await;
    s.wait(300).await.shot("preparing");
    s.event(te::requested(1, 1, "a", "cargo test -p wisp-core"))
        .await;
    s.event(te::block(2)).await;
    s.event(te::requested(1, 2, "b", "cargo clippy --workspace"))
        .await;
    s.event(main_spent((0.004, Some(0.0), false), &[])).await;
    s.event(te::step_ended(1, 0.004)).await;
    s.wait(300).await.shot("queued");
    s.event(te::started("a")).await;
    s.event(te::output(
        "a",
        "   Compiling wisp-core\ntest cache::fingerprint::names_segment ... ok\ntest cache::miss_reason ... ok\nrunning 41 te",
    ))
    .await;
    s.wait(7_200).await.shot("running");
}

pub(super) struct Seen {
    pub(super) chat: Vec<String>,
    pub(super) mark_cursor_row: u16,
    pub(super) mark: Vec<(String, String)>,
    pub(super) orb: Vec<(String, String)>,
}

impl Seen {
    pub(super) fn of(screen: &Screen) -> Seen {
        let rows = screen.visible();
        assert!(super::framed_status(&rows), "{rows:#?}");
        let top = super::frame_top(&rows);
        let start = (rows[..top].iter())
            .position(|r| r.starts_with('\u{2570}'))
            .map_or(0, |i| i + 1);
        let cells = |row: usize, from: u16| -> Vec<(String, String)> {
            let row = u16::try_from(row).unwrap();
            (from..from + 3)
                .map(|col| (screen.symbol_at(row, col), screen.fg_at(row, col)))
                .collect()
        };
        Seen {
            chat: rows[start..top].to_vec(),
            mark_cursor_row: u16::try_from(top).unwrap(),
            mark: cells(top - 1, 0),
            orb: cells(rows.len() - 2, 2),
        }
    }

    pub(super) fn marked(&self) -> bool {
        self.mark == self.orb && self.chat[self.chat.len() - 2].is_empty()
    }

    pub(super) fn row_of(&self, text: &str) -> usize {
        self.chat.iter().rposition(|r| r.contains(text)).unwrap()
    }
}

async fn seen(s: &Stage, ms: u64) -> Seen {
    Seen::of(&s.wait(ms).await.screen)
}

#[tokio::test(start_paused = true)]
async fn a_thinking_turn_holds_the_orb_and_mark_still_and_draws_at_most_5_frames_in_4_s_with_full_repaint()
 {
    let look = truecolor(Mode::Dark, GlyphSet::Unicode, 80);
    let full = |options: &mut Options| options.full_repaint = true;
    let screen = Screen::new(look.rows, look.width);
    staged_tuned(screen, look, facts(), full, |s| async move {
        s.send("Hi").await;
        let first = seen(&s, 100).await;
        assert!(first.marked(), "{:#?}", first.chat);
        let before = s.screen.frames();
        for step in 0..40 {
            let now = seen(&s, 100).await;
            assert_eq!(now.orb, first.orb, "step {step}");
            assert_eq!(now.mark, first.mark, "step {step}");
        }
        let drawn = s.screen.frames() - before;
        assert!(drawn <= 5, "{drawn} frames in 4 s");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_chat_mark_moves_in_step_with_the_status_orb_and_writes_only_their_cells() {
    let look = truecolor(Mode::Dark, GlyphSet::Unicode, 80);
    staged(look, |s| async move {
        s.send("Hi").await;
        let first = seen(&s, 100).await;
        assert!(first.marked(), "{:#?}", first.chat);
        let mark_row = first.mark_cursor_row;
        let status_row = ROWS - 1;
        let before = s.screen.frames();
        let mut shapes = BTreeSet::new();
        for step in 0..40 {
            let now = seen(&s, 100).await;
            assert_eq!(now.mark, now.orb, "step {step}");
            assert_eq!(
                now.chat[..now.chat.len() - 1],
                first.chat[..first.chat.len() - 1]
            );
            shapes.insert(now.mark);
        }
        assert!(shapes.len() >= 4, "{shapes:#?}");
        let drawn = s.screen.frames() - before;
        assert!(drawn <= 41, "{drawn} frames in 4 s");
        let mut parked = BTreeSet::new();
        for body in s.screen.bodies_since(before) {
            let mut moves = super::idle_orb::cursor_moves(&body);
            if moves
                .last()
                .is_some_and(|(row, _)| *row != mark_row && *row != status_row)
            {
                parked.extend(moves.pop());
            }
            assert!(
                moves
                    .iter()
                    .all(|(row, col)| (*row == mark_row && *col <= 3) || *row == status_row),
                "{:?}",
                String::from_utf8_lossy(&body)
            );
        }
        assert!(parked.len() <= 1, "the cursor parks on one row: {parked:?}");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_chat_moves_only_when_a_row_lands_through_a_turn() {
    let look = truecolor(Mode::Dark, GlyphSet::Unicode, 80);
    let ask = "Run the checks";
    let shots = Rc::new(RefCell::new(vec![]));
    let rows = Rc::clone(&shots);
    staged(look, |s| async move {
        let idle = seen(&s, 100).await;
        s.say(ask);
        let at_enter = seen(&s, 100).await;
        assert_eq!(at_enter.chat, idle.chat);
        let held = seen(&s, 2000).await;
        assert_eq!(held.chat, idle.chat);
        s.deliver().await;
        s.event(te::step(1)).await;
        let landed = seen(&s, 50).await;
        assert!(landed.marked(), "{:#?}", landed.chat);
        let asked = landed.row_of(ask);
        assert_eq!(asked, landed.chat.len() - 3, "{:#?}", landed.chat);
        for ms in [100, 300] {
            let later = seen(&s, ms).await;
            assert!(later.marked());
            assert_eq!(later.chat[..asked + 2], landed.chat[..asked + 2]);
        }
        s.block(0, BlockKind::Thinking).await;
        let thought = seen(&s, 100).await;
        assert!(thought.marked(), "{:#?}", thought.chat);
        assert_eq!(thought.row_of(ask), asked - 2, "{:#?}", thought.chat);
        s.stop(0).await;
        s.event(te::block(1)).await;
        s.event(te::requested(1, 1, "a", "cargo test")).await;
        s.event(te::block(2)).await;
        s.event(te::requested(1, 2, "b", "cargo clippy")).await;
        s.event(te::step_ended(1, 0.004)).await;
        s.event(te::started("a")).await;
        let calls = seen(&s, 100).await;
        assert!(calls.marked(), "{:#?}", calls.chat);
        s.event(te::finished("a", te::ok(0), "", 0, 1000)).await;
        let between = seen(&s, 100).await;
        assert!(between.marked(), "{:#?}", between.chat);
        assert_eq!(between.row_of(ask), calls.row_of(ask));
        s.event(te::started("b")).await;
        s.event(te::finished("b", te::ok(0), "", 0, 1000)).await;
        s.event(te::step(2)).await;
        s.event(te::block(0)).await;
        s.event(te::requested(2, 0, "c", "cargo fmt")).await;
        s.event(te::step_ended(2, 0.004)).await;
        s.event(te::started("c")).await;
        let joined = seen(&s, 100).await;
        assert!(joined.marked(), "{:#?}", joined.chat);
        assert_eq!(
            joined.row_of(ask),
            between.row_of(ask) - 1,
            "{:#?}",
            joined.chat
        );
        s.event(te::finished("c", te::ok(0), "", 0, 1000)).await;
        s.event(te::step(3)).await;
        s.answer(0, "Checking the output.\n").await;
        let words = seen(&s, 100).await;
        assert_eq!(words.row_of(ask), joined.row_of(ask), "{:#?}", words.chat);
        s.stop(0).await;
        let ended_words = seen(&s, 100).await;
        assert_eq!(ended_words.chat, words.chat);
        s.event(te::block(1)).await;
        s.event(te::requested(3, 1, "d", "cargo doc")).await;
        let call = seen(&s, 100).await;
        assert!(call.marked(), "{:#?}", call.chat);
        s.event(te::step_ended(3, 0.004)).await;
        s.event(te::started("d")).await;
        s.event(te::finished("d", te::ok(0), "", 0, 1000)).await;
        s.event(te::step(4)).await;
        let ran = seen(&s, 100).await;
        assert!(ran.marked(), "{:#?}", ran.chat);
        s.block(0, BlockKind::Text).await;
        let answering = seen(&s, 100).await;
        assert!(answering.marked(), "{:#?}", answering.chat);
        assert_eq!(
            answering.chat[..answering.chat.len() - 1],
            ran.chat[..ran.chat.len() - 1]
        );
        s.text(0, "All checks pass.\n").await;
        let reply = seen(&s, 100).await;
        assert_eq!(
            reply.row_of(ask),
            answering.row_of(ask),
            "{:#?}",
            reply.chat
        );
        assert!(reply.chat.last().unwrap().contains("All checks pass"));
        assert_eq!(reply.mark[1].0, answering.mark[1].0);
        s.stop(0).await;
        s.event(ended(
            completed(StopReason::EndTurn, true, Some(cost(0.01, 0.01, false))),
            900,
        ))
        .await;
        let footer = seen(&s, 500).await;
        assert!(!footer.marked(), "{:#?}", footer.chat);
        let all = [
            &landed,
            &thought,
            &calls,
            &between,
            &joined,
            &words,
            &ended_words,
            &call,
            &ran,
            &answering,
            &reply,
            &footer,
        ];
        rows.borrow_mut()
            .extend(all.iter().map(|seen| seen.row_of(ask)));
    })
    .await;
    let rows = shots.take();
    assert_eq!(rows.len(), 12);
    assert!(rows.windows(2).all(|w| w[1] <= w[0]), "{rows:?}");
}

#[tokio::test(start_paused = true)]
async fn a_cancel_an_error_a_question_and_a_withdrawn_send_never_move_the_chat_for_the_mark() {
    use question_scenes::{asked, closed, done, naming};
    let look = truecolor(Mode::Dark, GlyphSet::Unicode, 80);
    let failed = outcome(End {
        ending: Ending::Error(ErrorKind::Network),
        message: "reset".to_owned(),
        ..End::default()
    });
    for ending in [cancelled(), failed] {
        staged(look, |s| async move {
            s.send("Run the checks").await;
            let waiting = seen(&s, 100).await;
            assert!(waiting.marked(), "{:#?}", waiting.chat);
            s.event(ended(ending, 900)).await;
            let after = seen(&s, 500).await;
            assert!(after.row_of("Run the checks") <= waiting.row_of("Run the checks"));
            assert!(!after.marked(), "{:#?}", after.chat);
            assert!(!after.chat.last().unwrap().is_empty(), "{:#?}", after.chat);
            let later = seen(&s, 3000).await;
            assert_eq!(later.chat, after.chat);
        })
        .await;
    }
    staged(look, |s| async move {
        s.send("Name the crate").await;
        asked(&s, 1, "q", vec![naming()], 0.004).await;
        let open = seen(&s, 700).await;
        let card = open.row_of("? Name");
        assert!(
            open.chat[card - 1].contains("Asking Name"),
            "{:#?}",
            open.chat
        );
        assert_eq!(
            open.chat[card - 3],
            " \u{203A} Name the crate",
            "{:#?}",
            open.chat
        );
        assert_ne!(open.mark, open.orb);
        s.event(closed(
            "q",
            wisp_protocol::QuestionEnd::Answered {
                answers: vec![wisp_protocol::Answer::choose(0)],
            },
        ))
        .await;
        s.event(done("q")).await;
        let answered = seen(&s, 500).await;
        assert!(answered.marked(), "{:#?}", answered.chat);
    })
    .await;
    staged(look, |s| async move {
        let idle = seen(&s, 100).await;
        s.say("Never mind");
        let id = s.accepted(DeliveryMode::Queue, "Never mind").await;
        let accepted = seen(&s, 300).await;
        assert!(!accepted.marked(), "{:#?}", accepted.chat);
        s.event(AgentEvent::InputWithdrawn {
            ids: vec![id],
            by: WithdrawnBy::User,
        })
        .await;
        let withdrawn = seen(&s, 500).await;
        assert_eq!(withdrawn.chat, idle.chat);
    })
    .await;
}

async fn tools_done(s: Stage) {
    let (ok_text, ok_lines) = te::lines(214, "test");
    let tail: String = ok_text.lines().skip(174).collect::<Vec<_>>().join("\n");
    let (bad, bad_lines) = te::lines(9, "error");
    s.send("Run the checks").await;
    s.event(te::step(1)).await;
    for (i, (id, cmd)) in [
        ("a", "cargo test -p wisp-core"),
        ("b", "cargo clippy --workspace"),
        ("c", "./slow-server"),
        ("d", "{\"cmd\": 3}"),
        ("e", "ls"),
    ]
    .into_iter()
    .enumerate()
    {
        s.event(te::block(i)).await;
        s.event(te::requested(1, i, id, cmd)).await;
    }
    s.event(te::step_ended(1, 0.004)).await;
    s.event(te::started("a")).await;
    s.wait(100)
        .await
        .event(te::finished("a", te::ok(0), &tail, ok_lines, 12_000))
        .await;
    s.event(te::started("b")).await;
    s.event(te::finished("b", te::ok(101), &bad, bad_lines, 2_000))
        .await;
    s.event(te::started("c")).await;
    s.event(te::finished(
        "c",
        wisp_protocol::ToolOutcome::TimedOut { after_ms: 120_000 },
        "listening on :8080\n",
        1,
        120_000,
    ))
    .await;
    s.event(te::finished(
        "d",
        wisp_protocol::ToolOutcome::InvalidInput {
            message: "`command` must be a string".to_owned(),
        },
        "",
        0,
        0,
    ))
    .await;
    s.event(te::started("e")).await;
    s.event(te::finished(
        "e",
        wisp_protocol::ToolOutcome::Failed {
            message: "could not start the shell: No such file or directory".to_owned(),
            before_start: true,
        },
        "",
        0,
        3,
    ))
    .await;
    s.event(te::step(2)).await;
    s.event(te::block(0)).await;
    s.event(te::requested(
        2,
        0,
        "f",
        "cargo build --release \\\n  --locked \\\n  --all-features",
    ))
    .await;
    s.event(te::step_ended(2, 0.003)).await;
    s.event(te::started("f")).await;
    s.event(te::finished("f", te::ok(0), "", 0, 31_000)).await;
    s.event(te::step(3)).await;
    s.event(te::block(0)).await;
    s.event(te::requested(3, 0, "g", "cargo doc")).await;
    s.event(te::step_ended(3, 0.002)).await;
    s.event(te::finished("g", te::not_run(te::max_tokens()), "", 0, 0))
        .await;
    s.event(ended(
        completed(StopReason::MaxTokens, true, Some(cost(0.009, 0.009, false))),
        170_000,
    ))
    .await;
    s.wait(4000).await.send("Build it again").await;
    s.event(te::step(1)).await;
    s.event(te::requested(1, 0, "h", "cargo build --release"))
        .await;
    s.event(te::requested(1, 1, "i", "cargo test --release"))
        .await;
    s.event(main_spent((0.011, Some(0.0), false), &[])).await;
    s.event(te::step_ended(1, 0.002)).await;
    s.event(te::started("h")).await;
    s.wait(1200).await;
    s.event(te::finished(
        "h",
        wisp_protocol::ToolOutcome::Cancelled,
        "   Compiling serde\n",
        1,
        1_200,
    ))
    .await;
    s.event(te::finished(
        "i",
        te::not_run(wisp_protocol::NotRunReason::TurnStopped),
        "",
        0,
        0,
    ))
    .await;
    s.event(ended(
        TurnOutcome::Cancelled {
            usage: None,
            cost: Some(cost(0.002, 0.011, true)),
            committed_steps: 1,
        },
        1_400,
    ))
    .await;
    s.wait(500).await.shot("done");
}

const TOOL_ROWS: u16 = 70;

fn tool_looks() -> Vec<Look> {
    let mut looks = vec![];
    for mode in MODES {
        for width in WIDTHS {
            looks.push(Look {
                rows: TOOL_ROWS,
                ..truecolor(mode, GlyphSet::Unicode, width)
            });
        }
    }
    for width in WIDTHS {
        looks.push(Look {
            rows: TOOL_ROWS,
            ..truecolor(Mode::Dark, GlyphSet::Ascii, width)
        });
        looks.push(Look {
            rows: TOOL_ROWS,
            motion: MotionPolicy::Reduced,
            ..truecolor(Mode::Dark, GlyphSet::Unicode, width)
        });
    }
    looks
}

#[tokio::test(start_paused = true)]
async fn tool_rows_in_every_state_at_80_and_120_in_light_dark_ascii_and_reduced_motion() {
    let mut twins = Twins::default();
    for look in tool_looks() {
        snapshot("tools-live", look, &staged(look, tools_live).await);
        twins.check("tools-done", look, &staged(look, tools_done).await);
    }
}

fn text_rows(shot: &str) -> Vec<&str> {
    shot.lines()
        .filter_map(|l| l.strip_prefix('|'))
        .filter(|l| !l.starts_with('(') || !l.ends_with("blank rows)"))
        .collect()
}

fn row_spans(shot: &str, index: usize) -> &str {
    let lines: Vec<&str> = shot.lines().collect();
    let rows: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| {
            l.strip_prefix('|')
                .is_some_and(|r| !r.starts_with('(') || !r.ends_with("blank rows)"))
        })
        .map(|(i, _)| i)
        .collect();
    lines
        .get(rows[index] + 1)
        .and_then(|l| l.trim_start().strip_prefix("~ "))
        .unwrap_or("")
}

fn last_column(row: &str) -> usize {
    row.trim_end().chars().count()
}

#[tokio::test(start_paused = true)]
async fn at_200_columns_facts_end_at_the_screen_edge_like_the_status_row() {
    for mode in MODES {
        let tools = Look {
            rows: TOOL_ROWS,
            ..truecolor(mode, GlyphSet::Unicode, 200)
        };
        let shots = staged(tools, tools_done).await;
        snapshot("tools-done", tools, &shots);
        let shot = &shots.last().unwrap().1;
        let rows = text_rows(shot);
        let heads: Vec<&&str> = rows
            .iter()
            .filter(|r| r.contains("cargo clippy --workspace") || r.contains("./slow-server"))
            .collect();
        assert!(!heads.is_empty(), "{shot}");
        for head in heads {
            assert_eq!(last_column(head), 199, "{head:?}");
        }
        let bottom = rows
            .iter()
            .rposition(|r| r.starts_with('\u{2570}'))
            .unwrap();
        let status = rows[bottom - 1];
        let strip = rows.iter().find(|r| r.contains("total $")).unwrap();
        for framed in [status, strip] {
            let inside = framed.strip_suffix('\u{2502}').unwrap_or_default();
            assert_eq!(last_column(framed), 200, "{framed:?}");
            assert_eq!(last_column(inside), 198, "{framed:?}");
        }
        let look = truecolor(mode, GlyphSet::Unicode, 200);
        let shots = staged(look, showcase).await;
        snapshot("showcase", look, &shots);
        for (name, shot) in &shots {
            let rows = text_rows(shot);
            let (bottom, rest) = rows.split_last().unwrap();
            assert!(bottom.starts_with('\u{2570}'), "{name}: {bottom:?}");
            let (status, rest) = rest.split_last().unwrap();
            let inside = status.strip_suffix('\u{2502}').unwrap_or_default();
            assert_eq!(last_column(inside), 198, "{name}: {status:?}");
            for row in rest {
                let chrome = ['\u{2500}', '\u{256d}', '\u{2502}', '\u{2570}', '\u{251c}']
                    .iter()
                    .any(|c| row.starts_with(*c));
                assert!(chrome || last_column(row) <= 199, "{name}: {row:?}");
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn the_status_row_counts_requests_within_a_turn_and_shows_no_cost() {
    let look = truecolor(Mode::Dark, GlyphSet::Unicode, 120);
    let shots = staged(look, |s| async move {
        s.send("Run it").await;
        s.event(te::step(1)).await;
        s.event(te::requested(1, 0, "a", "ls")).await;
        s.event(te::step_ended(1, 0.004)).await;
        s.wait(100).await.status("one");
        s.event(te::started("a")).await;
        s.event(te::finished("a", te::ok(0), "", 0, 10)).await;
        s.event(te::step(2)).await;
        s.event(te::step_ended(2, 0.006)).await;
        s.wait(100).await.status("two");
        s.event(te::started("a")).await;
        s.wait(3_000).await.status("working");
    })
    .await;
    let row = |name: &str| {
        shots
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, s)| row_text(s).to_owned())
            .unwrap_or_default()
    };
    assert!(!row("one").contains('$'), "{}", row("one"));
    assert!(!row("one").contains("request"), "{}", row("one"));
    assert!(row("two").contains("2 requests"), "{}", row("two"));
    assert!(!row("two").contains('$'), "{}", row("two"));
}

#[tokio::test(start_paused = true)]
async fn the_status_row_names_a_running_call_and_its_clock() {
    let look = truecolor(Mode::Dark, GlyphSet::Unicode, 120);
    let shots = staged(look, |s| async move {
        s.send("Run it").await;
        s.event(te::step(1)).await;
        s.event(te::block(0)).await;
        s.wait(500).await.status("preparing");
        s.event(te::requested(1, 0, "a", "cargo test")).await;
        s.event(te::step_ended(1, 0.004)).await;
        s.event(te::started("a")).await;
        s.wait(7_400).await.status("running");
    })
    .await;
    assert!(
        row_text(&shots[0].1).contains("Writing a command \u{b7} 0s"),
        "{shots:?}"
    );
    assert!(
        row_text(&shots[1].1).contains("Running cargo test \u{b7} 7s"),
        "{shots:?}"
    );
}

fn rows_with(shot: &str, needle: &str) -> Vec<String> {
    let lines: Vec<&str> = shot.lines().collect();
    lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.starts_with('|') && l.contains(needle))
        .map(|(i, l)| {
            let style = lines
                .get(i + 1)
                .filter(|n| n.starts_with("  ~"))
                .copied()
                .unwrap_or_default();
            format!("{l}\n{style}")
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn a_tool_row_is_drawn_the_same_in_the_transcript_and_in_transcript_mode() {
    for look in [
        truecolor(Mode::Dark, GlyphSet::Unicode, 80),
        truecolor(Mode::Light, GlyphSet::Ascii, 120),
    ] {
        let (bad, bad_lines) = te::lines(9, "error");
        let shots = staged(look, |s| async move {
            s.send("Run the checks").await;
            s.event(te::requested(1, 0, "a", "cargo test -p wisp-core"))
                .await;
            s.event(te::requested(1, 1, "b", "cargo clippy --workspace"))
                .await;
            s.event(te::requested(1, 2, "c", "sleep 100")).await;
            s.event(te::step_ended(1, 0.004)).await;
            s.event(te::started("a")).await;
            s.event(te::finished("a", te::ok(0), "ok\n", 1, 12_000))
                .await;
            s.event(te::started("b")).await;
            s.event(te::finished("b", te::ok(101), &bad, bad_lines, 2_000))
                .await;
            s.event(te::started("c")).await;
            s.wait(2_050).await.shot("main");
            s.open_transcript(1).await.wait(1).await.shot("pager");
            s.input(Input::Key(Key::plain(KeyCode::Char('q'))))
                .wait(100)
                .await;
            s.event(te::finished(
                "c",
                wisp_protocol::ToolOutcome::Cancelled,
                "",
                0,
                2_000,
            ))
            .await;
            s.event(ended(cancelled(), 2_100)).await;
        })
        .await;
        let main = &shots[0].1;
        let pager = &shots[1].1;
        for needle in [
            "cargo test -p wisp-core",
            "cargo clippy --workspace",
            "Running sleep 100",
        ] {
            let in_main: Vec<String> = rows_with(main, needle)
                .into_iter()
                .filter(|r| !r.contains(&copy::esc_to_stop().text()))
                .collect();
            assert_eq!(in_main.len(), 1, "{needle}: {main}");
            assert_eq!(
                in_main.first(),
                rows_with(pager, needle).first(),
                "{needle}: {main}\n{pager}"
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn an_idle_running_call_redraws_only_its_row_and_the_status_row() {
    for motion in [MotionPolicy::Full, MotionPolicy::Reduced] {
        let look = Look {
            motion,
            depth: Depth::Ansi16,
            ..truecolor(Mode::Dark, GlyphSet::Unicode, 80)
        };
        let screen = Screen::new(look.rows, look.width);
        let probe = screen.clone();
        let changes = Rc::new(RefCell::new((0usize, 0usize, BTreeSet::new())));
        let seen = Rc::clone(&changes);
        staged_on(screen, look, |s| async move {
            s.send("Run it").await;
            s.event(te::requested(1, 0, "a", "sleep 100")).await;
            s.event(te::step_ended(1, 0.004)).await;
            s.event(te::started("a")).await;
            s.wait(1_000).await;
            let rows = probe.visible();
            let tool = rows.iter().position(|l| l.contains("sleep 100")).unwrap();
            let status = rows.iter().rposition(|l| !l.trim().is_empty()).unwrap()
                - usize::from(super::framed_status(&rows));
            let before = probe.frames();
            let mut last = rows;
            for _ in 0..200 {
                s.wait(15).await;
                let now = probe.visible();
                for (i, (a, b)) in last.iter().zip(&now).enumerate() {
                    if a != b {
                        seen.borrow_mut().2.insert(i);
                    }
                }
                last = now;
            }
            {
                let mut c = seen.borrow_mut();
                c.0 = probe.frames() - before;
                c.1 = tool * 1000 + status;
            }
            s.event(te::finished(
                "a",
                wisp_protocol::ToolOutcome::Cancelled,
                "",
                0,
                4_000,
            ))
            .await;
            s.event(ended(cancelled(), 4_000)).await;
        })
        .await;
        let (frames, rows, changed) = changes.take();
        let (tool, status) = (rows / 1000, rows % 1000);
        let allowed: BTreeSet<usize> = [tool, status].into_iter().collect();
        assert!(
            changed.is_subset(&allowed),
            "{motion:?}: {changed:?} vs {allowed:?}"
        );
        assert!(changed.contains(&tool), "{motion:?}: {changed:?}");
        let budget = match motion {
            MotionPolicy::Reduced => 3 + 1,
            _ => 3000 / 150 + 3 + 1,
        };
        assert!(frames <= budget, "{motion:?}: {frames} frames in 3 s");
    }
}

#[tokio::test(start_paused = true)]
async fn transcript_mode_shows_the_whole_output_file() {
    let dir = ScratchDir::new(SCRATCH, "pager-files");
    let whole: String = (1..=30).map(|i| format!("file line {i}\n")).collect();
    let whole = dir
        .write("whole.out", &whole)
        .to_string_lossy()
        .into_owned();
    let gone = dir.root.join("gone.out").to_string_lossy().into_owned();
    let hostile = dir
        .write("hostile.out", "safe \x1b]52;c;cHduZWQ=\x07\x1b[2Jtext\n")
        .to_string_lossy()
        .into_owned();
    let look = Look {
        rows: 200,
        ..truecolor(Mode::Dark, GlyphSet::Unicode, 100)
    };
    let shots = staged(look, |s| async move {
        s.send("Run them").await;
        for (i, (id, path)) in [("a", &whole), ("b", &gone), ("c", &hostile)]
            .into_iter()
            .enumerate()
        {
            s.event(te::requested(1, i, id, &format!("./gen-{id}")))
                .await;
            s.event(te::started(id)).await;
            let finished = te::finished(id, te::ok(0), "only the tail\n", 30, 10);
            s.event(te::with_file(finished, path)).await;
        }
        s.event(ended(
            completed(StopReason::EndTurn, true, Some(cost(0.01, 0.01, false))),
            100,
        ))
        .await;
        s.wait(300).await.shot("main");
        s.open_transcript(100).await.wait(100).await.shot("pager");
        s.input(Input::Key(Key::plain(KeyCode::Char('q'))))
            .wait(100)
            .await;
    })
    .await;
    let main = &shots[0].1;
    let pager = &shots[1].1;
    assert!(!main.contains("file line 1\n"), "{main}");
    for want in [
        "$ ./gen-a",
        "in ~/Projects/Wisp",
        "Output saved to ",
        "file line 1\n",
        "file line 30",
        "couldn't be read",
        "only the tail",
        "safe text",
    ] {
        assert!(pager.contains(want), "{want}: {pager}");
    }
    assert!(!pager.contains("cHduZWQ"), "{pager}");
}

#[tokio::test(start_paused = true)]
async fn an_open_transcript_mode_shows_a_call_finishing() {
    let look = Look {
        rows: 40,
        ..truecolor(Mode::Dark, GlyphSet::Unicode, 100)
    };
    let shots = staged(look, |s| async move {
        s.send("Run it").await;
        s.event(te::requested(1, 0, "a", "./check")).await;
        s.event(te::step_ended(1, 0.004)).await;
        s.event(te::started("a")).await;
        s.wait(500).await;
        s.input(Input::Key(Key::ctrl('o'))).wait(100).await;
        s.input(Input::Key(Key::ctrl('o')))
            .wait(100)
            .await
            .shot("open");
        s.event(te::finished("a", te::ok(3), "bad\n", 1, 600)).await;
        s.wait(200).await.shot("finished");
        s.input(Input::Key(Key::plain(KeyCode::Char('q'))))
            .wait(100)
            .await;
        s.event(ended(cancelled(), 900)).await;
    })
    .await;
    assert!(!shots[0].1.contains("exit 3"), "{}", shots[0].1);
    assert!(
        shots[1]
            .1
            .contains("in ~/Projects/Wisp \u{b7} exit 3 \u{b7} 0s"),
        "{}",
        shots[1].1
    );
    assert!(
        shots[1].1.contains("RETURN") || shots[1].1.contains("q esc"),
        "{}",
        shots[1].1
    );
}

#[tokio::test(start_paused = true)]
async fn transcript_mode_scrolled_to_a_grown_reply_shows_all_of_it() {
    let look = Look {
        rows: 20,
        ..truecolor(Mode::Dark, GlyphSet::Unicode, 80)
    };
    let lines = |from: usize, to: usize| -> String {
        (from..to).map(|i| format!("Reply line {i}.\n\n")).collect()
    };
    let shots = staged(look, |s| async move {
        s.send("First").await;
        s.answer(0, &lines(1000, 1100)).await;
        s.stop(0).await;
        s.event(ended(
            completed(StopReason::EndTurn, true, Some(cost(0.01, 0.01, false))),
            9_000,
        ))
        .await;
        s.wait(500).await;
        s.send("Write it").await;
        s.answer(0, &lines(0, 100)).await;
        s.wait(500).await;
        s.open_transcript(100).await;
        s.input(Input::Key(Key::plain(KeyCode::Char('g'))))
            .wait(100)
            .await
            .shot("top");
        s.text(0, &lines(100, 130)).await;
        s.wait(500).await.shot("grown");
        s.input(Input::Key(Key::plain(KeyCode::Char('G'))))
            .wait(100)
            .await
            .shot("bottom");
        s.input(Input::Key(Key::plain(KeyCode::Char('q'))))
            .wait(100)
            .await;
    })
    .await;
    for shot in &shots[..2] {
        assert!(!shot.1.contains("Reply line 99."), "{}", shot.1);
    }
    let bottom = &shots[2].1;
    assert!(bottom.contains("Reply line 129."), "{bottom}");
    let (shown, total) = bottom
        .split_whitespace()
        .find_map(|w| {
            let (range, total) = w.split_once('/')?;
            let (_, last) = range.split_once('-')?;
            Some((last.parse::<usize>().ok()?, total.parse::<usize>().ok()?))
        })
        .unwrap();
    assert_eq!(shown, total, "{bottom}");
}

const STEER_ROWS: u16 = 24;

fn steering_looks() -> Vec<Look> {
    let mut looks = vec![];
    for mode in MODES {
        for width in WIDTHS {
            looks.push(Look {
                rows: STEER_ROWS,
                ..truecolor(mode, GlyphSet::Unicode, width)
            });
        }
    }
    looks.push(Look {
        rows: STEER_ROWS,
        ..truecolor(Mode::Dark, GlyphSet::Ascii, 80)
    });
    looks.push(Look {
        rows: STEER_ROWS,
        motion: MotionPolicy::Reduced,
        ..truecolor(Mode::Dark, GlyphSet::Unicode, 80)
    });
    looks
}

fn long_paste() -> String {
    (0..12)
        .map(|i| format!("log line {i}"))
        .collect::<Vec<_>>()
        .join("\n")
}

async fn waiting(s: Stage) {
    s.send("Study how other harnesses do compaction").await;
    s.event(AgentEvent::StepStarted { step: 1 }).await;
    s.wait(100)
        .await
        .answer(0, "Reading the Pi and Codex sources first.\n")
        .await;
    s.steer("or even if we can study claude code itself").await;
    s.wait(100).await.shot("one");
    s.paste("then write it up as research/compaction.md");
    s.wait(100).await.shot("typing");
    s.key(KeyCode::Tab);
    s.accepted(
        DeliveryMode::Queue,
        "then write it up as research/compaction.md",
    )
    .await;
    s.paste("check this log: ");
    s.paste(&long_paste());
    s.wait(100).await.shot("chip");
    s.key(KeyCode::Tab);
    s.wait(100).await.shot("expanded");
    s.input(Input::Key(Key::ctrl('u')));
    s.paste(" too").key(KeyCode::Tab);
    s.accepted(DeliveryMode::Queue, "check this log").await;
    s.event(AgentEvent::InputAccepted {
        opening: false,
        front: false,
        id: InputId("cli-1".to_owned()),
        mode: DeliveryMode::Queue,
        text: "and this screenshot".to_owned(),
        images: vec![wisp_protocol::log::ImageRef {
            sha256: "3fa2".to_owned(),
            media_type: wisp_protocol::ImageType::Png,
            bytes: 10,
            width: 4,
            height: 4,
        }],
        replaces: None,
        held: false,
        from: None,
    })
    .await;
    s.queue("and open an MR for it").await;
    s.wait(100).await.shot("five");
    s.steer("keep it short").await;
    s.queue("then tell me what it cost").await;
    s.wait(100).await.shot("seven");
    s.input(Input::Key(Key::ctrl('o')))
        .wait(100)
        .await
        .shot("transcript mode");
    s.input(Input::Key(Key::plain(KeyCode::Char('q'))))
        .wait(100)
        .await;
    s.say("/model");
    s.wait(20).await.event(described(1)).await;
    s.wait(100).await.shot("picker");
    s.key(KeyCode::Esc);
    s.wait(100).await;
    s.key(KeyCode::Esc);
    s.wait(10).await;
    s.event(AgentEvent::InputWithdrawn {
        ids: vec![
            InputId("tui-1".to_owned()),
            InputId("tui-2".to_owned()),
            InputId("tui-6".to_owned()),
        ],
        by: WithdrawnBy::TurnStopped,
    })
    .await;
    s.event(AgentEvent::InputHeld {
        ids: ["tui-3", "tui-4", "cli-1", "tui-5", "tui-7"]
            .into_iter()
            .map(|i| InputId(i.to_owned()))
            .collect(),
        by: WithdrawnBy::TurnStopped,
    })
    .await;
    s.event(ended(cancelled(), 4000)).await;
    s.wait(4000).await.shot("held");
}

async fn delivered(s: Stage) {
    s.send("Run the checks").await;
    s.event(AgentEvent::StepStarted { step: 1 }).await;
    s.wait(100).await.answer(0, "Running them now.\n").await;
    s.stop(0).await;
    let steer = s.steer("also print the date").await;
    let queued = s.queue("then commit").await;
    let other = s.queue("and push").await;
    s.event(AgentEvent::StepStarted { step: 2 }).await;
    s.event(AgentEvent::InputDelivered {
        ids: vec![steer],
        step: 2,
    })
    .await;
    s.wait(100).await.answer(1, "The date is Friday.\n").await;
    s.stop(1).await;
    s.event(ended(
        completed(StopReason::EndTurn, true, Some(cost(0.02, 0.02, false))),
        3000,
    ))
    .await;
    s.wait(100).await.shot("busy between turns");
    s.event(AgentEvent::turn_started(2)).await;
    s.event(AgentEvent::StepStarted { step: 1 }).await;
    s.event(AgentEvent::InputDelivered {
        ids: vec![queued.clone(), other.clone()],
        step: 1,
    })
    .await;
    s.wait(100).await.answer(0, "Committing.\n").await;
    s.stop(0).await;
    s.key(KeyCode::Esc);
    s.wait(10).await;
    s.event(AgentEvent::InputWithdrawn {
        ids: vec![queued, other],
        by: WithdrawnBy::TurnStopped,
    })
    .await;
    s.event(ended(cancelled(), 1000)).await;
    s.wait(4000).await.shot("stopped");
}

#[tokio::test(start_paused = true)]
async fn waiting_rows_at_40_columns() {
    for mode in MODES {
        for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
            let look = Look {
                rows: STEER_ROWS,
                ..truecolor(mode, glyphs, 40)
            };
            snapshot(
                "waiting",
                look,
                &staged(look, |s| Box::pin(waiting(s))).await,
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn waiting_rows_at_80_and_120_in_light_dark_ascii_and_reduced_motion() {
    for look in steering_looks() {
        snapshot(
            "waiting",
            look,
            &staged(look, |s| Box::pin(waiting(s))).await,
        );
        snapshot(
            "delivered",
            look,
            &staged(look, |s| Box::pin(delivered(s))).await,
        );
    }
}

async fn editing(s: Stage) {
    s.send("Study how other harnesses do compaction").await;
    s.event(AgentEvent::StepStarted { step: 1 }).await;
    s.wait(100)
        .await
        .answer(0, "Reading the Pi and Codex sources first.\n")
        .await;
    let steer = s.steer("or even if we can study claude code itself").await;
    let write = s.queue("then write it up as research/compaction.md").await;
    let mr = s.queue("and open an MR for it").await;
    s.wait(100).await.shot("waiting");
    s.paste("my draft").key(KeyCode::Up);
    s.wait(100).await.shot("focused");
    s.key(KeyCode::Up).key(KeyCode::Enter);
    s.paste(" with sources");
    s.wait(100).await.shot("editing");
    s.key(KeyCode::Enter);
    let write = s
        .edited(
            &write,
            DeliveryMode::Queue,
            "then write it up as research/compaction.md with sources",
            false,
        )
        .await;
    s.wait(100).await.shot("saved");
    for _ in 0.."my draft".len() {
        s.key(KeyCode::Backspace);
    }
    let mut more = vec![];
    for text in [
        "keep the summary short",
        "cite each source",
        "note what Pi does differently",
        "then tell me what it cost",
    ] {
        more.push(s.queue(text).await);
    }
    s.wait(10).await.key(KeyCode::Up);
    s.wait(100).await.shot("scrolled");
    s.key(KeyCode::Esc);
    s.paste("my draft").key(KeyCode::Up);
    for _ in 0..6 {
        s.key(KeyCode::Up);
    }
    s.key(KeyCode::Enter);
    s.paste(", briefly");
    s.wait(100).await.shot("editing a steer");
    s.event(AgentEvent::StepStarted { step: 2 }).await;
    s.event(AgentEvent::InputDelivered {
        ids: vec![steer],
        step: 2,
    })
    .await;
    s.key(KeyCode::Enter);
    let late = s.bump();
    s.wait(10).await;
    s.event(AgentEvent::Rejected {
        request: None,
        reason: "Wisp already read that message, so it stays sent.".to_owned(),
        id: Some(late),
        kind: Some(RejectKind::AlreadyDelivered),
    })
    .await;
    s.wait(100).await.shot("too late");
    s.key(KeyCode::Esc);
    s.wait(10).await;
    let mut held = vec![write, mr];
    held.extend(more);
    s.event(AgentEvent::InputHeld {
        ids: held,
        by: WithdrawnBy::TurnStopped,
    })
    .await;
    s.event(ended(cancelled(), 4000)).await;
    s.wait(4000).await.shot("held");
    s.input(Input::Key(Key::ctrl('c')));
    s.key(KeyCode::Up).key(KeyCode::Tab);
    s.wait(100).await.shot("nothing to steer");
}

fn editing_looks() -> Vec<Look> {
    let mut looks = steering_looks();
    for mode in MODES {
        looks.push(Look {
            rows: STEER_ROWS,
            ..truecolor(mode, GlyphSet::Unicode, 40)
        });
    }
    looks
}

#[tokio::test(start_paused = true)]
async fn editing_waiting_rows_at_40_80_and_120_in_light_dark_ascii_and_reduced_motion() {
    for look in editing_looks() {
        snapshot(
            "editing",
            look,
            &staged(look, |s| Box::pin(editing(s))).await,
        );
    }
}

#[path = "question_scenes.rs"]
mod question_scenes;

#[path = "endpoint_scenes.rs"]
mod endpoint_scenes;

#[path = "compact_scenes.rs"]
mod compact_scenes;
#[path = "reader_scenes.rs"]
mod reader_scenes;
#[path = "retry_scenes.rs"]
mod retry_scenes;
#[path = "typed_scenes.rs"]
mod typed_scenes;

#[tokio::test(start_paused = true)]
async fn a_tool_group_sits_tight_on_screen_with_blank_rows_only_around_it() {
    for width in [80, 120] {
        let look = Look {
            rows: TOOL_ROWS,
            ..truecolor(Mode::Dark, GlyphSet::Unicode, width)
        };
        let shots = staged(look, tools_done).await;
        let shot = &shots.last().unwrap().1;
        let rows = text_rows(shot);
        let at = |needle: &str| {
            rows.iter()
                .position(|r| r.contains(needle))
                .unwrap_or_else(|| panic!("{needle}: {shot}"))
        };
        let (asked, first, last) = (
            at("Run the checks"),
            at("Ran cargo test -p wisp-core"),
            at("\u{25E6} cargo doc"),
        );
        assert_eq!(first, asked + 2, "{shot}");
        assert!(rows[asked + 1].trim().is_empty(), "{shot}");
        assert!(!row_spans(shot, asked + 1).contains(" bg "), "{shot}");
        for row in &rows[first..=last] {
            assert!(!row.trim().is_empty(), "{width}: {shot}");
        }
        assert!(rows[last + 1].trim().is_empty(), "{shot}");
        assert!(rows[last + 2].contains("3 requests"), "{shot}");
    }
}

async fn withdrawn_note(s: Stage) {
    s.send("What does this screenshot show?").await;
    s.event(AgentEvent::StepStarted { step: 1 }).await;
    s.event(AgentEvent::AttachmentsWithdrawn {
        step: 1,
        items: vec![wisp_protocol::WithdrawnAttachment {
            call_id: None,
            ordinal: 0,
            kind: wisp_protocol::AttachmentKind::Image,
            label: "#1".to_owned(),
            reason: "Could not process image".to_owned(),
            note: "[image #1 couldn't be processed: Could not process image]".to_owned(),
        }],
    })
    .await;
    s.wait(100)
        .await
        .answer(0, "The image didn't reach me; describe it and I'll help.\n")
        .await;
    s.stop(0).await;
    s.event(ended(
        completed(StopReason::EndTurn, true, Some(cost(0.004, 0.004, false))),
        2000,
    ))
    .await;
    s.wait(4000).await.shot("withdrawn");
}

#[tokio::test(start_paused = true)]
async fn a_withdrawn_attachment_note_sits_under_its_own_message_with_and_without_bands() {
    let banded = truecolor(Mode::Dark, GlyphSet::Unicode, 80);
    let plain = Look {
        depth: Depth::None,
        glyphs: GlyphSet::Ascii,
        ..banded
    };
    for look in [banded, plain] {
        let shots = staged(look, |s| Box::pin(withdrawn_note(s))).await;
        let shot = &shots[0].1;
        let rows: Vec<&str> = shot.lines().filter(|l| l.starts_with('|')).collect();
        let message = rows
            .iter()
            .position(|l| l.contains("What does this screenshot show?"))
            .unwrap_or_else(|| panic!("{shot}"));
        let note = rows
            .iter()
            .position(|l| l.contains("image #1 couldn't be processed"))
            .unwrap_or_else(|| panic!("{shot}"));
        let answer = rows
            .iter()
            .position(|l| l.contains("The image didn't reach me"))
            .unwrap_or_else(|| panic!("{shot}"));
        assert!(message < note && note < answer, "{shot}");
        snapshot("withdrawn-note", look, &shots);
    }
}

fn hook_ran(hook: &str, point: &str, outcome: wisp_protocol::HookOutcome) -> AgentEvent {
    AgentEvent::HookRan {
        hook: hook.to_owned(),
        point: point.to_owned(),
        turn: None,
        step: None,
        call_id: None,
        outcome,
        ms: 2000,
    }
}

async fn hook_failed(s: Stage) {
    s.send("Make cache misses name their cause").await;
    s.wait(100).await;
    s.event(hook_ran(
        "aa/fetch",
        "before-turn",
        wisp_protocol::HookOutcome::Failed {
            message: "the aa plugin panicked".to_owned(),
        },
    ))
    .await;
    s.event(hook_ran(
        "bb/slow",
        "before-request",
        wisp_protocol::HookOutcome::TimedOut,
    ))
    .await;
    s.event(hook_ran(
        "cc/long",
        "before-turn",
        wisp_protocol::HookOutcome::Rejected {
            rule: "too_large".to_owned(),
        },
    ))
    .await;
    s.event(hook_ran(
        "dd/quiet",
        "before-turn",
        wisp_protocol::HookOutcome::Applied,
    ))
    .await;
    s.wait(100).await.answer(0, ANSWER).await.stop(0).await;
    s.event(ended(
        completed(StopReason::EndTurn, true, Some(cost(0.014, 0.014, false))),
        2000,
    ))
    .await;
    s.wait(4000).await.shot("done");
}

async fn hook_session_start(s: Stage) {
    s.event(hook_ran(
        "aa/start",
        "session-start",
        wisp_protocol::HookOutcome::Failed {
            message: "the aa plugin panicked".to_owned(),
        },
    ))
    .await;
    s.wait(900).await.shot("welcome");
}

#[tokio::test(start_paused = true)]
async fn a_failing_hook_adds_one_muted_line_in_light_and_dark() {
    for mode in MODES {
        let look = truecolor(mode, GlyphSet::Unicode, 80);
        snapshot(
            "hook-failed",
            look,
            &staged(look, |s| Box::pin(hook_failed(s))).await,
        );
    }
    let look = truecolor(Mode::Dark, GlyphSet::Unicode, 80);
    snapshot(
        "hook-session-start",
        look,
        &staged(look, |s| Box::pin(hook_session_start(s))).await,
    );
}
async fn hook_compaction_request(s: Stage) {
    s.send("Tidy the parser").await;
    s.wait(100).await;
    let hook = Some("aa/ask".to_owned());
    s.event(AgentEvent::CompactionStarted {
        trigger: wisp_protocol::CompactionTrigger::Auto,
        turn: Some(1),
        switch: None,
        requested_by: hook.clone(),
    })
    .await;
    s.event(AgentEvent::CompactionFailed {
        trigger: wisp_protocol::CompactionTrigger::Auto,
        engine: Some(wisp_protocol::CompactionEngine::Native),
        kind: wisp_protocol::CompactionFailure::Error(wisp_protocol::ErrorKind::Overloaded),
        message: "overloaded".to_owned(),
        usage: None,
        cost: None,
        answers: None,
        turn: Some(1),
        paused: false,
        requested_by: hook,
    })
    .await;
    for _ in 0..2 {
        s.event(AgentEvent::HookRan {
            hook: "aa/ask".to_owned(),
            point: "context-threshold".to_owned(),
            turn: Some(1),
            step: Some(1),
            call_id: None,
            outcome: wisp_protocol::HookOutcome::Rejected {
                rule: "request_failed".to_owned(),
            },
            ms: 4,
        })
        .await;
    }
    s.wait(100).await.answer(0, ANSWER).await.stop(0).await;
    s.event(ended(
        completed(StopReason::EndTurn, true, Some(cost(0.014, 0.014, false))),
        2000,
    ))
    .await;
    s.wait(4000).await.shot("done");
}

#[tokio::test(start_paused = true)]
async fn a_compaction_request_its_failure_and_one_refusal_each_add_a_line_in_light_and_dark() {
    for mode in MODES {
        let look = truecolor(mode, GlyphSet::Unicode, 80);
        snapshot(
            "hook-compaction-request",
            look,
            &staged(look, |s| Box::pin(hook_compaction_request(s))).await,
        );
    }
}

#[path = "instructions_scenes.rs"]
mod instructions_scenes;
