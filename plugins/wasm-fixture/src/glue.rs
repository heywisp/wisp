use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use serde_json::{Map, Value, json};

use crate::bindings::exports::wisp::plugin0_1_0::hooks;
use crate::bindings::exports::wisp::plugin0_2_0::{agent_hooks, prompt_sections, strategy};
use crate::bindings::exports::wisp::plugin0_3_0::tools::{
    self, BuildContext, ClientTool, DoctorRow, Effect, Purpose, Tags, Timeouts, ToolDef, ToolKind,
    ToolMeta,
};
use crate::bindings::exports::wisp::plugin0_3_0::{commands, index_source, slot_render};
use crate::bindings::wisp::plugin0_1_0::call::CallContext;
use crate::bindings::wisp::plugin0_1_0::clock;
use crate::bindings::wisp::plugin0_1_0::hook_types::{Event, Reply};
use crate::bindings::wisp::plugin0_1_0::jobs::{self, Mode, SpawnSpec, WaitFor};
use crate::bindings::wisp::plugin0_1_0::points;
use crate::bindings::wisp::plugin0_1_0::types::{Action, Completed, Failure, Outcome, OutputKind};
use crate::bindings::wisp::plugin0_2_0::agent;
use crate::bindings::wisp::plugin0_2_0::slot_types::{NodeState, Span, TextSpan, Tone};
use crate::bindings::wisp::plugin0_3_0::slot_types::{Block, Topology, TopologyNode};
use crate::bindings::wisp::plugin0_3_0::types::{CodeView, Display, Fact, Hit, ToolRun, View};
use crate::bindings::wisp::plugin0_3_0::{
    agent_rename, agent_states, index, screen_report, secrets,
};

pub(crate) struct Fixture;

static CALLS: AtomicU32 = AtomicU32::new(0);
static DESCRIBED_AS: Mutex<Option<String>> = Mutex::new(None);
static SET_UP: Mutex<Option<bool>> = Mutex::new(None);
static SPAWNED: AtomicU32 = AtomicU32::new(0);
static DRAWN: AtomicBool = AtomicBool::new(false);

const BLOCKED: &str = "block me";
const REWRITTEN: &str = "rewrite me";

const FOREVER_MS: u64 = 3_600_000;

fn meta(saves_output: bool) -> ToolMeta {
    ToolMeta {
        effect: Effect::ReadOnly,
        tags: Tags::empty(),
        parallel_safe: true,
        timeouts: Timeouts {
            default_ms: if saves_output { 10_000 } else { 60_000 },
            max_ms: Some(if saves_output { 60_000 } else { 600_000 }),
        },
        saves_output,
        needs_workspace: saves_output,
        ends_reply: false,
        asks_user: false,
        reports_jobs: false,
        writing: None,
        purpose: Purpose::Run,
    }
}

fn tool(name: &str, description: &str, properties: Value, saves_output: bool) -> ToolDef {
    let schema = json!({"type": "object", "properties": properties, "additionalProperties": false});
    ToolDef {
        name: name.to_owned(),
        kind: ToolKind::Client(ClientTool {
            description: description.to_owned(),
            input_schema: schema.to_string(),
            meta: meta(saves_output),
        }),
        system_line: None,
        context_line: None,
    }
}

fn definitions() -> Vec<ToolDef> {
    let text = json!({"text": {"type": "string"}});
    let ms = json!({
        "ms": {"type": "integer"},
        "limit_ms": {"type": "integer"},
        "stubborn": {"type": "boolean"}
    });
    vec![
        tool("count", "Count this instance's calls.", json!({}), false),
        tool(
            "echo",
            "Write the given text as the tool's output.",
            text,
            true,
        ),
        tool(
            "files",
            "Write text to a file in the call's folder and read it back.",
            json!({"name": {"type": "string"}, "text": {"type": "string"}}),
            false,
        ),
        tool(
            "spawn",
            "Run a program as a job and return its output.",
            json!({"program": {"type": "string"}, "args": {"type": "array", "items": {"type": "string"}}}),
            false,
        ),
        tool(
            "probe",
            "Probe the newer host imports.",
            json!({"code": {"type": "string"}}),
            false,
        ),
        tool("spin", "Loop without end.", json!({}), false),
        tool("stall", "Never finish preparing.", json!({}), false),
        tool(
            "stream",
            "Write chunks of output, waiting between them.",
            json!({"chunks": {"type": "integer"}, "ms": {"type": "integer"}}),
            true,
        ),
        tool("trap", "Trap at once.", json!({}), false),
        tool("wait", "Wait ms milliseconds.", ms, false),
        tool(
            "whoami",
            "Name the agent and the call this runs under.",
            json!({}),
            false,
        ),
    ]
}

fn object(input: &str) -> Result<Map<String, Value>, String> {
    match serde_json::from_str(input) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err("input must be an object".to_owned()),
        Err(e) => Err(format!("input is not JSON: {e}")),
    }
}

fn number(map: &Map<String, Value>, key: &str) -> u64 {
    map.get(key).and_then(Value::as_u64).unwrap_or_default()
}

fn text(map: &Map<String, Value>, key: &str) -> String {
    map.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn display(tool: &str, target: String) -> Display {
    Display {
        action: Action::Other(tool.to_owned()),
        verbs: None,
        target,
        output: OutputKind::Text,
        view: None,
        title: None,
    }
}

fn run_with(outcome: Outcome, result: Option<String>) -> ToolRun {
    ToolRun {
        outcome,
        facts: Vec::new(),
        cwd: None,
        trailer: None,
        result,
        attachments: Vec::new(),
        typed: None,
    }
}

fn completed(result: Option<String>) -> ToolRun {
    run_with(
        Outcome::Completed(Completed {
            is_error: false,
            exit: None,
        }),
        result,
    )
}

fn current_agent() -> String {
    agent::current().map_or_else(|| "none".to_owned(), |identity| identity.path)
}

pub(crate) struct Call {
    tool: String,
    input: Map<String, Value>,
    shown: Display,
}

impl tools::GuestPrepared for Call {
    fn display(&self) -> Display {
        self.shown.clone()
    }
}

async fn wait(ms: u64, stubborn: bool, ctx: &CallContext) -> ToolRun {
    if stubborn {
        clock::sleep(ms).await;
        return completed(Some("waited".to_owned()));
    }
    let mut slept = std::pin::pin!(clock::sleep(ms));
    let mut stopped = std::pin::pin!(ctx.wait_cancelled());
    std::future::poll_fn(|cx| {
        if slept.as_mut().poll(cx).is_ready() {
            return std::task::Poll::Ready(completed(Some("waited".to_owned())));
        }
        if stopped.as_mut().poll(cx).is_ready() {
            return std::task::Poll::Ready(run_with(
                Outcome::Cancelled,
                Some("saw the call cancelled".to_owned()),
            ));
        }
        std::task::Poll::Pending
    })
    .await
}

async fn stream(chunks: u64, ms: u64, ctx: &CallContext) -> ToolRun {
    for chunk in 1..=chunks {
        ctx.write_output(format!("chunk {chunk}\n").as_bytes());
        clock::sleep(ms).await;
    }
    completed(None)
}

fn files(name: &str, text: &str, ctx: &CallContext) -> ToolRun {
    let path = format!("{}/{name}", ctx.cwd());
    let read = std::fs::write(&path, text).and_then(|()| std::fs::read_to_string(&path));
    match read {
        Ok(back) => completed(Some(back)),
        Err(e) => run_with(
            Outcome::Failed(Failure {
                message: e.to_string(),
                before_start: false,
            }),
            None,
        ),
    }
}

async fn spawn(input: &Map<String, Value>, ctx: &CallContext) -> ToolRun {
    let args = input
        .get("args")
        .and_then(Value::as_array)
        .map(|args| {
            args.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let spec = SpawnSpec {
        program: text(input, "program"),
        args,
        cwd: ctx.cwd(),
        env: Vec::new(),
        mode: Mode::Pipe,
        size: None,
        stdin: false,
        background: false,
        limit_ms: None,
        label: "spawn".to_owned(),
        keep: false,
    };
    let job = match jobs::spawn(spec, ctx).await {
        Ok(job) => job,
        Err(e) => return completed(Some(format!("{e:?}"))),
    };
    let how = WaitFor {
        timeout_ms: 10_000,
        until: None,
        quiet_ms: None,
    };
    let waited = job.wait(how).await;
    let output = job.output().await;
    completed(Some(format!("{waited:?}\n{}", output.tail)))
}

fn whoami(ctx: &CallContext) -> ToolRun {
    let described = DESCRIBED_AS
        .lock()
        .ok()
        .and_then(|seen| seen.clone())
        .unwrap_or_default();
    completed(Some(format!(
        "run: {}\ndescribe: {described}\ncall: {}\ncwd: {}",
        current_agent(),
        ctx.id(),
        ctx.cwd()
    )))
}

async fn probe(ctx: &CallContext) -> ToolRun {
    let states = agent_states::states();
    let report = screen_report::wait(ctx.id(), 1).await;
    let secret = secrets::type_into(
        secrets::SecretAsk {
            process: 7,
            name: "Placeholder".to_owned(),
            reason: "Fixture probe".to_owned(),
            end_input: false,
            show_start: 0,
            show_end: 0,
        },
        ctx,
    )
    .await;
    let renamed = agent_rename::rename_agent("old", "new", ctx);
    let entries = index::entries();
    let mut run = completed(Some(format!(
        "states: {states:?}\nreport: {report:?}\nsecret: {secret:?}\nrename: {renamed:?}\nentries: {entries:?}"
    )));
    run.facts.push(Fact::Hits(
        entries
            .into_iter()
            .map(|entry| Hit {
                kind: entry.kind,
                name: entry.name,
            })
            .collect(),
    ));
    run
}

impl tools::Guest for Fixture {
    type Prepared = Call;

    async fn describe(_ctx: BuildContext) -> Vec<ToolDef> {
        if let Ok(mut seen) = DESCRIBED_AS.lock() {
            *seen = Some(current_agent());
        }
        definitions()
    }

    async fn doctor(_ctx: BuildContext) -> Vec<DoctorRow> {
        Vec::new()
    }

    async fn prepare(tool: String, input: String) -> Result<tools::Prepared, String> {
        if !definitions().iter().any(|def| def.name == tool) {
            return Err(format!("there is no tool named `{tool}`"));
        }
        let input = object(&input)?;
        if tool == "stall" {
            loop {
                clock::sleep(FOREVER_MS).await;
            }
        }
        let target = match tool.as_str() {
            "echo" => input
                .get("text")
                .and_then(Value::as_str)
                .and_then(|text| text.lines().next())
                .unwrap_or_default()
                .to_owned(),
            _ => tool.clone(),
        };
        let mut shown = display(&tool, target);
        if tool == "probe" {
            shown.view = Some(View::Code(CodeView {
                language: Some("text".to_owned()),
                code: text(&input, "code"),
            }));
        }
        Ok(tools::Prepared::new(Call { tool, input, shown }))
    }

    async fn rejected_display(tool: String, _input: String) -> Option<Display> {
        (tool == "wait").then(|| display(&tool, "rejected".to_owned()))
    }

    async fn time_limit_ms(tool: String, input: String) -> Option<u64> {
        let limit = object(&input).ok().map(|map| number(&map, "limit_ms"));
        limit.filter(|ms| tool == "wait" && *ms > 0)
    }

    async fn movable(tool: String, _input: String) -> bool {
        tool == "wait"
    }

    async fn interrupted_result(tool: String, _input: String) -> Option<String> {
        (tool == "wait").then(|| "the wait was interrupted".to_owned())
    }

    async fn run(call: tools::Prepared, ctx: &CallContext) -> ToolRun {
        let Call { tool, input, .. } = call.into_inner();
        match tool.as_str() {
            "count" => {
                let n = CALLS.fetch_add(1, Ordering::SeqCst) + 1;
                completed(Some(n.to_string()))
            }
            "echo" => {
                if let Some(Value::String(text)) = input.get("text") {
                    ctx.write_output(text.as_bytes());
                }
                completed(None)
            }
            "files" => files(&text(&input, "name"), &text(&input, "text"), ctx),
            "spawn" => spawn(&input, ctx).await,
            "spin" => loop {
                std::hint::spin_loop();
            },
            "stream" => stream(number(&input, "chunks"), number(&input, "ms"), ctx).await,
            "trap" => core::arch::wasm32::unreachable(),
            "wait" => {
                let stubborn = input.get("stubborn").and_then(Value::as_bool) == Some(true);
                wait(number(&input, "ms"), stubborn, ctx).await
            }
            "whoami" => whoami(ctx),
            "probe" => probe(ctx).await,
            _ => completed(None),
        }
    }
}

impl commands::Guest for Fixture {
    async fn run(name: String, args: String) -> commands::CommandReply {
        if name == "lines" {
            let span = TextSpan {
                text: args,
                tone: Tone::Accent,
                bold: true,
            };
            commands::CommandReply::Lines(vec![
                commands::CommandLine::Row(commands::CommandRow {
                    left: vec![span.clone()],
                    right: vec![span.clone()],
                }),
                commands::CommandLine::Text(vec![span]),
            ])
        } else {
            commands::CommandReply::Send(args)
        }
    }
}

impl index_source::Guest for Fixture {
    async fn entries() -> Vec<index::Entry> {
        clock::sleep(0).await;
        vec![index::Entry {
            kind: index::Kind::Document,
            name: "fixture-note".to_owned(),
            summary: "Placeholder entry".to_owned(),
            text: "Placeholder search text".to_owned(),
        }]
    }

    async fn open_entry(name: String) -> Result<String, String> {
        if name == "fixture-note" {
            Ok("Placeholder entry body".to_owned())
        } else {
            Err(format!("nothing named `{name}` was found"))
        }
    }
}

fn lead_line() -> String {
    let state = match SET_UP.lock().ok().and_then(|set_up| *set_up) {
        Some(resumed) => format!("set up (resumed: {resumed})"),
        None => "not set up".to_owned(),
    };
    let spawned = SPAWNED.load(Ordering::SeqCst);
    format!("wasm-fixture {state}, {spawned} agents spawned\n")
}

impl hooks::Guest for Fixture {
    async fn on_hook(hook: String, ev: hooks::Event) -> hooks::Reply {
        match (hook.as_str(), ev) {
            ("setup", Event::SessionStart(start)) => {
                if let Ok(mut set_up) = SET_UP.lock() {
                    *set_up = Some(start.resumed);
                }
                Reply::None
            }
            ("lead", Event::BeforeTurn(_)) => Reply::Append(vec![lead_line()]),
            ("block", Event::BeforeTool(call)) if call.input.contains(BLOCKED) => {
                Reply::Block("the fixture blocks this input".to_owned())
            }
            ("rewrite", Event::AfterTool(ran)) if ran.result.ends_with(REWRITTEN) => {
                Reply::Rewrite("rewritten by the fixture".to_owned())
            }
            ("compact", Event::BeforeCompact(_)) => {
                Reply::Append(vec!["Keep the fixture's notes.".to_owned()])
            }
            ("fires", Event::UserInput(input)) if input.text == "fire" => {
                points::fire("ping".to_owned(), String::new()).await;
                Reply::None
            }
            ("loops", Event::UserInput(input)) if input.text == "loop" => loop {
                std::hint::spin_loop();
            },
            ("traps", Event::UserInput(input)) if input.text == "trap" => {
                core::arch::wasm32::unreachable()
            }
            ("relay", Event::Custom(_)) => {
                points::fire("ping".to_owned(), String::new()).await;
                Reply::None
            }
            _ => Reply::None,
        }
    }
}

fn drawn(text: String) -> slot_render::Content {
    let span = Span::Text(TextSpan {
        text,
        tone: Tone::Text,
        bold: false,
    });
    slot_render::Content {
        note: Vec::new(),
        blocks: vec![Block::Line(vec![span])],
    }
}

impl slot_render::Guest for Fixture {
    async fn render_slot(
        name: String,
        view: slot_render::SlotView,
    ) -> Result<slot_render::Content, String> {
        let view = match view {
            slot_render::SlotView::Panel => "panel",
            slot_render::SlotView::Page => "page",
        };
        match name.as_str() {
            "map" => Ok(slot_render::Content {
                note: Vec::new(),
                blocks: vec![Block::Topology(Topology {
                    root: Some("main".to_owned()),
                    nodes: vec![TopologyNode {
                        key: "main".to_owned(),
                        parent: None,
                        state: NodeState::Working,
                    }],
                    flows: Vec::new(),
                    counted: None,
                })],
            }),
            "status" => {
                clock::sleep(60_000).await;
                Ok(drawn(format!("wasm-fixture status: {view}")))
            }
            "loops" if DRAWN.swap(true, Ordering::SeqCst) => loop {
                std::hint::spin_loop();
            },
            _ => Ok(drawn(format!("wasm-fixture {name}: {view}"))),
        }
    }
}

impl prompt_sections::Guest for Fixture {
    async fn render_section(
        _name: String,
        at: prompt_sections::At,
        previous: prompt_sections::Prior,
        _target: Option<String>,
    ) -> prompt_sections::Rendered {
        if let prompt_sections::At::Start = at {
            let text = format!("wasm-fixture clock: {}", clock::now().seconds);
            return prompt_sections::Rendered {
                snapshot: text.clone(),
                text: Some(text),
            };
        }
        let snapshot = match previous {
            prompt_sections::Prior::Snapshot(snapshot) => snapshot,
            _ => String::new(),
        };
        prompt_sections::Rendered {
            snapshot,
            text: None,
        }
    }
}

impl agent_hooks::Guest for Fixture {
    async fn on_agent_hook(hook: String, ev: agent_hooks::TreeEvent) {
        if let ("spawned", agent_hooks::TreeEvent::AgentSpawned(_)) = (hook.as_str(), ev) {
            SPAWNED.fetch_add(1, Ordering::SeqCst);
        }
    }
}

impl strategy::Guest for Fixture {
    async fn instruction(_info: strategy::StrategyInfo) -> Option<String> {
        Some("Summarise the conversation for the WASM fixture.".to_owned())
    }

    async fn accept(_info: strategy::StrategyInfo, _summary: String) -> Result<(), String> {
        Ok(())
    }

    async fn head(
        _info: strategy::StrategyInfo,
        view: &strategy::View,
        summary: Option<String>,
    ) -> Vec<String> {
        let said = view.said().len();
        vec![format!(
            "The fixture's summary of {said} messages:\n{}",
            summary.unwrap_or_default()
        )]
    }
}
