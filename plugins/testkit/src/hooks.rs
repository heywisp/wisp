use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use futures::future::BoxFuture;
use wisp_plugin_api::Plugin;
use wisp_plugin_api::commands::CommandReply;
use wisp_plugin_api::hooks::{BeforeTurn, Event, Reply, UserInput};
use wisp_plugin_api::host::{CallContext, Host, SpawnInput, SpawnSpec, Todo, TodoStatus};
use wisp_plugin_api::http::Request;
use wisp_plugin_api::prompt::{At, Prior, Rendered};
use wisp_plugin_api::slots::{Block, Content, SlotView, Span, TextSpan, Tone};
use wisp_plugin_api::strategy::{CallEntry, SaidEntry, StrategyInfo, View};
use wisp_plugin_api::tools::{
    BuildContext, ClientTool, Effect, Prepared, Purpose, Tags, Timeouts, ToolDef, ToolKind,
    ToolMeta,
};
use wisp_plugin_api::tree::TreeEvent;
use wisp_plugin_api::types::Level;
use wisp_plugin_api::types::{Display, ToolRun};

pub type Script =
    Arc<dyn Fn(Arc<dyn Host>, String, Event) -> BoxFuture<'static, Reply> + Send + Sync>;

pub type Watch =
    Arc<dyn Fn(Arc<dyn Host>, String, TreeEvent) -> BoxFuture<'static, ()> + Send + Sync>;

pub type Instantiate = Arc<dyn Fn(Arc<dyn Host>) -> Arc<dyn Plugin> + Send + Sync>;

pub type Render = Arc<
    dyn Fn(Arc<dyn Host>, String, SlotView) -> BoxFuture<'static, Result<Content, String>>
        + Send
        + Sync,
>;

pub type Seen = Arc<Mutex<Vec<String>>>;

pub type Run =
    Arc<dyn Fn(Arc<dyn Host>, String, String) -> BoxFuture<'static, CommandReply> + Send + Sync>;

pub type Section =
    Arc<dyn Fn(Arc<dyn Host>, SectionCalled) -> BoxFuture<'static, Rendered> + Send + Sync>;

pub type Knob = Arc<Mutex<String>>;

pub type Strategy =
    Arc<dyn Fn(Arc<dyn Host>, StrategyCalled) -> BoxFuture<'static, Answer> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Viewed {
    pub said: Vec<SaidEntry>,
    pub calls: Vec<CallEntry>,
    pub log_pointer: Option<String>,
    pub output_pointer: Option<String>,
}

impl Viewed {
    fn of(view: &dyn View) -> Self {
        Self {
            said: view.said(),
            calls: view.calls(),
            log_pointer: view.log_pointer(),
            output_pointer: view.output_pointer(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrategyCalled {
    Instruction(StrategyInfo),
    Accept(StrategyInfo, String),
    Head(StrategyInfo, Viewed, Option<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Instruction(Option<String>),
    Accept(Result<(), String>),
    Head(Vec<String>),
}

pub fn strategy_body() -> String {
    "\n[strategy]\nsummary = \"Scripted test strategy\"\n".to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionCalled {
    pub instance: usize,
    pub name: String,
    pub at: At,
    pub previous: Prior,
    pub target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Called {
    pub instance: usize,
    pub hook: String,
    pub event: Event,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saw {
    pub instance: usize,
    pub hook: String,
    pub event: TreeEvent,
}

#[derive(Clone, Default)]
pub struct Calls {
    log: Arc<Mutex<Vec<Called>>>,
    tree: Arc<Mutex<Vec<Saw>>>,
    renders: Arc<Mutex<Vec<(usize, String, SlotView)>>>,
    commands: Arc<Mutex<Vec<(String, String)>>>,
    sections: Arc<Mutex<Vec<SectionCalled>>>,
    strategies: Arc<Mutex<Vec<StrategyCalled>>>,
    instances: Arc<AtomicUsize>,
}

impl Calls {
    pub fn all(&self) -> Vec<Called> {
        self.log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn of(&self, hook: &str) -> Vec<Called> {
        self.all().into_iter().filter(|c| c.hook == hook).collect()
    }

    pub fn count(&self, hook: &str) -> usize {
        self.of(hook).len()
    }

    pub fn renders(&self) -> Vec<(String, SlotView)> {
        self.renders
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(_, name, view)| (name.clone(), *view))
            .collect()
    }

    pub fn render_instances(&self) -> Vec<usize> {
        self.renders
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(instance, _, _)| *instance)
            .collect()
    }

    pub fn rendered(&self, name: &str, view: SlotView) -> usize {
        self.renders()
            .iter()
            .filter(|(n, v)| n == name && *v == view)
            .count()
    }

    pub fn sections(&self) -> Vec<SectionCalled> {
        self.sections
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn sections_at(&self, at: At) -> Vec<SectionCalled> {
        self.sections().into_iter().filter(|c| c.at == at).collect()
    }

    pub fn strategies(&self) -> Vec<StrategyCalled> {
        self.strategies
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn tree(&self) -> Vec<Saw> {
        self.tree
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn commands(&self) -> Vec<(String, String)> {
        self.commands
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn instances(&self) -> usize {
        self.instances.load(Ordering::SeqCst)
    }

    fn push(&self, called: Called) {
        self.log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(called);
    }
}

pub fn before_turn(turn: u64, typed: &str) -> BeforeTurn {
    BeforeTurn {
        turn,
        lead: Vec::new(),
        typed: typed.to_owned(),
        images: 0,
        retry: false,
    }
}

pub fn user_input(text: &str, images: u32) -> Event {
    Event::UserInput(UserInput {
        text: text.to_owned(),
        images,
        steer: false,
    })
}

pub fn manifest(id: &str, hooks: &str) -> String {
    manifest_in("0.1.0", id, hooks)
}

pub fn manifest_in(world: &str, id: &str, hooks: &str) -> String {
    format!(
        "id = \"{id}\"\nversion = \"0.1.0\"\nworld = \"{world}\"\nsummary = \"Scripted test hooks\"\n{hooks}"
    )
}

pub fn slotted(id: &str, body: &str) -> String {
    format!(
        "id = \"{id}\"\nversion = \"0.1.0\"\nworld = \"0.2.0\"\nsummary = \"Scripted test sections\"\n{body}"
    )
}

pub fn command(name: &str, extra: &str) -> String {
    format!("\n[[commands]]\nname = \"{name}\"\nsummary = \"Run {name}\"\n{extra}")
}

pub fn section(name: &str, extra: &str) -> String {
    format!("\n[[slots]]\nslot = \"sidebar\"\nname = \"{name}\"\ntitle = \"{name}\"\n{extra}")
}

pub fn prompt_section(name: &str, place: &str, extra: &str) -> String {
    format!("\n[[prompt_sections]]\nname = \"{name}\"\nplace = \"{place}\"\n{extra}")
}

pub fn replay_hook(name: &str) -> String {
    format!("\n[[hooks]]\nname = \"{name}\"\npoint = \"before-turn\"\nretry = \"replay\"\n")
}

pub fn knob(value: &str) -> Knob {
    Arc::new(Mutex::new(value.to_owned()))
}

pub fn turn_to(knob: &Knob, value: &str) {
    *knob.lock().unwrap_or_else(PoisonError::into_inner) = value.to_owned();
}

pub fn section_text(name: &str, value: &str) -> String {
    format!("<{name}>\n{value}\n</{name}>\n")
}

pub fn changing(knob: Knob) -> Section {
    Arc::new(move |_, call| {
        let value = knob.lock().unwrap_or_else(PoisonError::into_inner).clone();
        Box::pin(async move {
            let wanted = call.target.clone().unwrap_or(value);
            let text = match &call.previous {
                Prior::Snapshot(seen) if *seen == wanted => None,
                _ => Some(section_text(&call.name, &wanted)),
            };
            Rendered {
                snapshot: wanted,
                text,
            }
        })
    })
}

pub fn text(text: &str) -> Span {
    Span::Text(TextSpan {
        text: text.to_owned(),
        tone: Tone::Text,
        bold: false,
    })
}

pub fn line(words: &str) -> Content {
    Content {
        note: Vec::new(),
        blocks: vec![Block::Line(vec![text(words)])],
    }
}

pub fn views() -> Render {
    Arc::new(|_, name, view| {
        let content = match view {
            SlotView::Panel => line(&format!("panel {name}")),
            SlotView::Page => Content {
                note: vec![text("board")],
                blocks: vec![
                    Block::Line(vec![text(&format!("page {name}"))]),
                    Block::NextColumn,
                    Block::Line(vec![text("totals")]),
                ],
            },
        };
        Box::pin(async move { Ok(content) })
    })
}

pub const CANARY: &str = "SIDEBAR-CANARY";

pub fn every_point() -> String {
    let points: Vec<String> = wisp_plugin_api::manifest::OBSERVE_POINTS
        .iter()
        .map(|p| format!("\"{p}\""))
        .collect();
    format!("marks = [{}]\n", points.join(", "))
}

pub fn busy() -> Render {
    Arc::new(|host, name, view| {
        let fill = host.current_fill();
        let words = format!("{CANARY} {name} {view:?} {}", fill.estimate);
        let span = || text(&words);
        let content = Content {
            note: vec![span()],
            blocks: vec![
                Block::Line(vec![span(), text(&"x".repeat(15_000))]),
                Block::Rows(vec![wisp_plugin_api::slots::Row {
                    key: CANARY.to_owned(),
                    value: vec![span()],
                }]),
                Block::Gauge(wisp_plugin_api::slots::Gauge {
                    label: CANARY.to_owned(),
                    value: 1,
                    max: 2,
                    tone: Tone::Accent,
                    note: vec![span()],
                }),
                Block::Spark(wisp_plugin_api::slots::Spark {
                    label: CANARY.to_owned(),
                    values: vec![0, 1, 2],
                    max: 2,
                    tone: Tone::Ok,
                    note: vec![span()],
                }),
                Block::List(vec![vec![span()], vec![span()]]),
                Block::Tree(vec![
                    wisp_plugin_api::slots::Node {
                        depth: 0,
                        state: wisp_plugin_api::slots::NodeState::Working,
                        role: Some("main".to_owned()),
                        label: CANARY.to_owned(),
                        facts: vec![Span::Elapsed(wisp_plugin_api::slots::ElapsedSpan {
                            since_ms: 1,
                            tone: Tone::Muted,
                        })],
                    },
                    wisp_plugin_api::slots::Node {
                        depth: 1,
                        state: wisp_plugin_api::slots::NodeState::Done,
                        role: None,
                        label: CANARY.to_owned(),
                        facts: vec![Span::Usd(wisp_plugin_api::slots::UsdSpan {
                            micros: 5,
                            at_least: true,
                            tone: Tone::Faint,
                        })],
                    },
                ]),
                Block::NextColumn,
            ],
        };
        Box::pin(async move { Ok(content) })
    })
}

pub fn fires(point: &str) -> Script {
    let point = point.to_owned();
    Arc::new(move |host, _, _| {
        let point = point.clone();
        Box::pin(async move {
            host.fire(point, String::new()).await;
            Reply::None
        })
    })
}

pub fn refused_imports(seen: Seen, call_id: &str) -> Render {
    let call_id = call_id.to_owned();
    Arc::new(move |host, _, _| {
        let seen = seen.clone();
        let call_id = call_id.clone();
        Box::pin(async move {
            let ctx = crate::host::FakeCall {
                id: call_id,
                ..crate::host::FakeCall::default()
            };
            let mut out = Vec::new();
            let answers = host.fire("x".to_owned(), String::new()).await;
            out.push(format!("fire {}", answers.len()));
            let count = host
                .count_tokens("image/png".to_owned(), Vec::new(), None)
                .await;
            out.push(format!("count {count:?}"));
            host.sleep(10_000).await;
            out.push("slept".to_owned());
            let captured = host.capture("true".to_owned(), Vec::new(), 10_000).await;
            out.push(format!(
                "capture {}",
                matches!(captured, wisp_plugin_api::host::Captured::Failed(_))
            ));
            let spawned = host.spawn(spawn_spec(), &ctx).await;
            out.push(format!("spawn {}", spawned.is_err()));
            out.push(format!("adopt {}", host.adopt(1, &ctx).is_none()));
            host.mark_reported(1, &ctx);
            let todo = Todo {
                section: None,
                text: "t".to_owned(),
                status: TodoStatus::Pending,
            };
            out.push(format!("todos {}", host.replace(vec![todo], &ctx).is_err()));
            out.push(format!("list {}", host.list_agents(&ctx).is_err()));
            let started = host.spawn_agent(SpawnInput::default(), &ctx).await;
            out.push(format!("spawn_agent {}", started.is_err()));
            out.push(format!(
                "report {}",
                host.report("{}".to_owned(), &ctx).is_err()
            ));
            let sent = host.send_message("x".to_owned(), "y".to_owned(), false, &ctx);
            out.push(format!("send {}", sent.is_err()));
            let stopped = host.stop_agent("x".to_owned(), false, &ctx);
            out.push(format!("stop {}", stopped.is_err()));
            host.write(Level::Info, "dropped");
            let fetched = host.fetch(Request {
                method: "GET".to_owned(),
                url: "http://127.0.0.1:9/".to_owned(),
                headers: Vec::new(),
                body: None,
                timeout_ms: None,
            });
            out.push(format!("fetch {}", fetched.await.is_err()));
            out.push(format!(
                "facts {} {} {} {}",
                host.environment().is_none(),
                host.config_dir().is_none(),
                host.terminal().is_none(),
                host.opening().is_none()
            ));
            *seen.lock().unwrap_or_else(PoisonError::into_inner) = out;
            Ok(line("refused"))
        })
    })
}

pub fn eager(seen: Seen) -> Render {
    Arc::new(move |host, _, _| {
        let seen = seen.clone();
        let host: &'static Arc<dyn Host> = Box::leak(Box::new(host));
        let slept = host.sleep(10_000);
        let counted = host.count_tokens("image/png".to_owned(), Vec::new(), None);
        let fired = host.fire("x".to_owned(), String::new());
        Box::pin(async move {
            slept.await;
            let count = counted.await;
            let answers = fired.await;
            *seen.lock().unwrap_or_else(PoisonError::into_inner) = vec![
                format!("count {count:?}"),
                format!("fire {}", answers.len()),
            ];
            Ok(line("eager"))
        })
    })
}

fn spawn_spec() -> SpawnSpec {
    SpawnSpec {
        program: "true".to_owned(),
        args: Vec::new(),
        cwd: "/tmp".to_owned(),
        env: Vec::new(),
        mode: wisp_plugin_api::host::Mode::Pipe,
        size: None,
        stdin: false,
        background: false,
        limit_ms: None,
        label: "true".to_owned(),
        keep: false,
    }
}

pub fn none() -> Script {
    Arc::new(|_, _, _| Box::pin(async { Reply::None }))
}

pub fn append(text: &str) -> Script {
    let text = text.to_owned();
    Arc::new(move |_, _, _| {
        let text = text.clone();
        Box::pin(async move { Reply::Append(vec![text]) })
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    None,
    Fire,
    Seen,
}

pub fn instantiate(script: Script, calls: Calls) -> Instantiate {
    instance(script, None, calls, Tool::None)
}

pub fn prompted(script: Script, section: Section, calls: Calls) -> Instantiate {
    Arc::new(move |host: Arc<dyn Host>| {
        let instance = calls.instances.fetch_add(1, Ordering::SeqCst) + 1;
        Arc::new(Hooks {
            host,
            script: script.clone(),
            render: None,
            section: Some(section.clone()),
            strategy: None,
            watch: None,
            run: None,
            calls: calls.clone(),
            instance,
            tool: Tool::None,
        }) as Arc<dyn Plugin>
    })
}

pub fn observing(script: Script, watch: Watch, calls: Calls) -> Instantiate {
    Arc::new(move |host: Arc<dyn Host>| {
        let instance = calls.instances.fetch_add(1, Ordering::SeqCst) + 1;
        Arc::new(Hooks {
            host,
            script: script.clone(),
            render: None,
            section: None,
            strategy: None,
            watch: Some(watch.clone()),
            run: None,
            calls: calls.clone(),
            instance,
            tool: Tool::None,
        }) as Arc<dyn Plugin>
    })
}

pub fn firing(script: Script, calls: Calls) -> Instantiate {
    instance(script, None, calls, Tool::Fire)
}

pub fn seeing(script: Script, calls: Calls) -> Instantiate {
    instance(script, None, calls, Tool::Seen)
}

pub fn sections(script: Script, render: Render, calls: Calls) -> Instantiate {
    instance(script, Some(render), calls, Tool::None)
}

pub fn strategising(
    script: Script,
    section: Option<Section>,
    strategy: Strategy,
    calls: Calls,
) -> Instantiate {
    Arc::new(move |host: Arc<dyn Host>| {
        let instance = calls.instances.fetch_add(1, Ordering::SeqCst) + 1;
        Arc::new(Hooks {
            host,
            script: script.clone(),
            render: None,
            section: section.clone(),
            strategy: Some(strategy.clone()),
            watch: None,
            run: None,
            calls: calls.clone(),
            instance,
            tool: Tool::None,
        }) as Arc<dyn Plugin>
    })
}

pub fn commanding(script: Script, run: Run, calls: Calls) -> Instantiate {
    Arc::new(move |host: Arc<dyn Host>| {
        let instance = calls.instances.fetch_add(1, Ordering::SeqCst) + 1;
        Arc::new(Hooks {
            host,
            script: script.clone(),
            render: None,
            section: None,
            strategy: None,
            watch: None,
            run: Some(run.clone()),
            calls: calls.clone(),
            instance,
            tool: Tool::None,
        }) as Arc<dyn Plugin>
    })
}

fn instance(script: Script, render: Option<Render>, calls: Calls, tool: Tool) -> Instantiate {
    Arc::new(move |host: Arc<dyn Host>| {
        let instance = calls.instances.fetch_add(1, Ordering::SeqCst) + 1;
        Arc::new(Hooks {
            host,
            script: script.clone(),
            render: render.clone(),
            section: None,
            strategy: None,
            watch: None,
            run: None,
            calls: calls.clone(),
            instance,
            tool,
        }) as Arc<dyn Plugin>
    })
}

pub struct Hooks {
    host: Arc<dyn Host>,
    script: Script,
    render: Option<Render>,
    section: Option<Section>,
    strategy: Option<Strategy>,
    watch: Option<Watch>,
    run: Option<Run>,
    calls: Calls,
    instance: usize,
    tool: Tool,
}

impl Hooks {
    fn strategy_call(&self, called: StrategyCalled) -> Option<BoxFuture<'static, Answer>> {
        self.calls
            .strategies
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(called.clone());
        let strategy = self.strategy.as_ref()?;
        Some(strategy(self.host.clone(), called))
    }
}

pub const FIRE: &str = "fire";

pub const SEEN: &str = "seen";

pub fn seen(host: &dyn Host) -> String {
    let current = host
        .current_agent()
        .map_or_else(|| "none".to_owned(), |i| i.path);
    let tree: Vec<String> = host.snapshot().into_iter().map(|n| n.path).collect();
    format!("current {current}; tree [{}]", tree.join(", "))
}

fn tool_def(name: &str, description: String) -> ToolDef {
    ToolDef {
        name: name.to_owned(),
        kind: ToolKind::Client(ClientTool {
            description,
            input_schema: "{\"type\":\"object\",\"properties\":{}}".to_owned(),
            meta: ToolMeta {
                effect: Effect::ReadOnly,
                tags: Tags::default(),
                parallel_safe: true,
                timeouts: Timeouts {
                    default_ms: 10_000,
                    max_ms: None,
                },
                saves_output: false,
                needs_workspace: false,
                ends_reply: false,
                asks_user: false,
                reports_jobs: false,
                writing: None,
                purpose: Purpose::Run,
            },
        }),
        system_line: None,
        context_line: None,
    }
}

fn fired(result: Option<String>) -> ToolRun {
    ToolRun {
        result,
        ..ToolRun::completed()
    }
}

struct SeenCall {
    host: Arc<dyn Host>,
    prepared: String,
}

impl Prepared for SeenCall {
    fn display(&self) -> Display {
        let shown = format!("{} | {}", self.prepared, seen(self.host.as_ref()));
        crate::fallback_display(&shown, "{}")
    }

    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any + Send> {
        Box::new(())
    }
}

impl Plugin for Hooks {
    fn describe<'a>(&'a self, _ctx: &'a BuildContext) -> BoxFuture<'a, Vec<ToolDef>> {
        match self.tool {
            Tool::None => Box::pin(async { Vec::new() }),
            Tool::Fire => {
                let def = tool_def(FIRE, "Fire this plugin's point x.".to_owned());
                Box::pin(async move { vec![def] })
            }
            Tool::Seen => {
                let first = seen(self.host.as_ref());
                Box::pin(async move {
                    let then = seen(self.host.as_ref());
                    vec![tool_def(SEEN, format!("{first} | {then}"))]
                })
            }
        }
    }

    fn prepare<'a>(
        &'a self,
        tool: &'a str,
        input: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Prepared>, String>> {
        Box::pin(async move {
            if self.tool == Tool::Seen && tool == SEEN {
                let call: Box<dyn Prepared> = Box::new(SeenCall {
                    host: self.host.clone(),
                    prepared: seen(self.host.as_ref()),
                });
                return Ok(call);
            }
            if self.tool == Tool::Fire && tool == FIRE {
                return Ok(crate::Call::boxed(crate::fallback_display(tool, input), ()));
            }
            Err(format!("{tool} is no tool of this plugin"))
        })
    }

    fn run<'a>(
        &'a self,
        _call: Box<dyn Prepared>,
        _ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, ToolRun> {
        if self.tool == Tool::Seen {
            let result = seen(self.host.as_ref());
            return Box::pin(async move { fired(Some(result)) });
        }
        Box::pin(async move {
            self.host.fire("x".to_owned(), String::new()).await;
            fired(None)
        })
    }

    fn run_command<'a>(&'a self, name: &'a str, args: &'a str) -> BoxFuture<'a, CommandReply> {
        self.calls
            .commands
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((name.to_owned(), args.to_owned()));
        match &self.run {
            Some(run) => run(self.host.clone(), name.to_owned(), args.to_owned()),
            None => Box::pin(async { CommandReply::None }),
        }
    }

    fn on_hook<'a>(&'a self, hook: &'a str, event: Event) -> BoxFuture<'a, Reply> {
        self.calls.push(Called {
            instance: self.instance,
            hook: hook.to_owned(),
            event: event.clone(),
        });
        (self.script)(self.host.clone(), hook.to_owned(), event)
    }

    fn render_section<'a>(
        &'a self,
        name: &'a str,
        at: At,
        previous: Prior,
        target: Option<&'a str>,
    ) -> BoxFuture<'a, Rendered> {
        let call = SectionCalled {
            instance: self.instance,
            name: name.to_owned(),
            at,
            previous,
            target: target.map(str::to_owned),
        };
        self.calls
            .sections
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(call.clone());
        match &self.section {
            Some(section) => section(self.host.clone(), call),
            None => Box::pin(async { Rendered::default() }),
        }
    }

    fn instruction<'a>(&'a self, info: StrategyInfo) -> BoxFuture<'a, Option<String>> {
        let answer = self.strategy_call(StrategyCalled::Instruction(info));
        Box::pin(async move {
            match answer {
                Some(answer) => match answer.await {
                    Answer::Instruction(text) => text,
                    Answer::Accept(_) | Answer::Head(_) => None,
                },
                None => None,
            }
        })
    }

    fn accept<'a>(
        &'a self,
        info: StrategyInfo,
        summary: String,
    ) -> BoxFuture<'a, Result<(), String>> {
        let answer = self.strategy_call(StrategyCalled::Accept(info, summary));
        Box::pin(async move {
            match answer {
                Some(answer) => match answer.await {
                    Answer::Accept(verdict) => verdict,
                    Answer::Instruction(_) | Answer::Head(_) => Ok(()),
                },
                None => Ok(()),
            }
        })
    }

    fn head<'a>(
        &'a self,
        info: StrategyInfo,
        view: &'a dyn View,
        summary: Option<String>,
    ) -> BoxFuture<'a, Vec<String>> {
        let answer = self.strategy_call(StrategyCalled::Head(info, Viewed::of(view), summary));
        Box::pin(async move {
            match answer {
                Some(answer) => match answer.await {
                    Answer::Head(texts) => texts,
                    Answer::Instruction(_) | Answer::Accept(_) => Vec::new(),
                },
                None => Vec::new(),
            }
        })
    }

    fn on_agent_hook<'a>(&'a self, hook: &'a str, event: TreeEvent) -> BoxFuture<'a, ()> {
        self.calls
            .tree
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Saw {
                instance: self.instance,
                hook: hook.to_owned(),
                event: event.clone(),
            });
        match &self.watch {
            Some(watch) => watch(self.host.clone(), hook.to_owned(), event),
            None => Box::pin(async {}),
        }
    }

    fn render_slot<'a>(
        &'a self,
        name: &'a str,
        view: SlotView,
    ) -> BoxFuture<'a, Result<Content, String>> {
        self.calls
            .renders
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((self.instance, name.to_owned(), view));
        match &self.render {
            Some(render) => render(self.host.clone(), name.to_owned(), view),
            None => Box::pin(async { Ok(Content::default()) }),
        }
    }
}
