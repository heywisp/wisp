use std::collections::BTreeMap;
use std::io::Cursor;

use serde_json::{Map, Value, json};
use wisp_plugin_api::host::{CallContext, Host, Mode, SpawnSpec};
use wisp_plugin_api::http::{HttpError, Request};
use wisp_plugin_api::tools::{
    ClientTool, Effect, Prepared, Purpose, Tags, Timeouts, ToolDef, ToolKind, ToolMeta,
};
use wisp_plugin_api::types::{
    Action, AskReply, Attachment, BackgroundReason, Completed, Display, ExitStatus, Fact, Failure,
    Outcome, OutputKind, Question, QuestionOption, ToolRun,
};
use wisp_tool_text::question::{MESSAGE_INSTEAD, STOPPED, UNAVAILABLE, UNAVAILABLE_MESSAGE};

use crate::{Call, fallback_display, parse};

pub const PDF: &[u8] = b"%PDF-1.4\n\
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n\
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] >> endobj\n\
trailer << /Root 1 0 R >>\n\
%%EOF\n";

pub fn png() -> Vec<u8> {
    let image = image::RgbImage::from_pixel(64, 32, image::Rgb([200, 30, 30]));
    let mut bytes = Cursor::new(Vec::new());
    match image::DynamicImage::ImageRgb8(image).write_to(&mut bytes, image::ImageFormat::Png) {
        Ok(()) => bytes.into_inner(),
        Err(_) => Vec::new(),
    }
}

pub fn question() -> Question {
    Question {
        header: "Plan".into(),
        question: "Which plan should go ahead?".into(),
        options: vec![
            QuestionOption {
                label: "Keep the plan".into(),
                description: "Carry on as written.".into(),
            },
            QuestionOption {
                label: "Change the plan".into(),
                description: "Stop and rethink first.".into(),
            },
        ],
        multi_select: false,
    }
}

struct Spec {
    name: &'static str,
    description: &'static str,
    schema: Value,
    meta: ToolMeta,
}

fn meta(default_ms: u64, max_ms: Option<u64>) -> ToolMeta {
    ToolMeta {
        effect: Effect::ReadOnly,
        tags: Tags::default(),
        parallel_safe: true,
        timeouts: Timeouts { default_ms, max_ms },
        saves_output: false,
        needs_workspace: false,
        ends_reply: false,
        asks_user: false,
        reports_jobs: false,
        writing: None,
        purpose: Purpose::Run,
    }
}

fn job_meta() -> ToolMeta {
    ToolMeta {
        effect: Effect::NonIdempotent,
        parallel_safe: false,
        saves_output: true,
        needs_workspace: true,
        ..meta(60_000, Some(60_000))
    }
}

fn specs() -> Vec<Spec> {
    vec![
        Spec {
            name: "confirm",
            description: "Ask the user which plan should go ahead.",
            schema: json!({"type": "object", "properties": {}, "additionalProperties": false}),
            meta: ToolMeta {
                parallel_safe: false,
                ends_reply: true,
                asks_user: true,
                ..meta(60_000, None)
            },
        },
        Spec {
            name: "echo",
            description: "Write the given text as the tool's output.",
            schema: json!({
                "type": "object",
                "properties": {"text": {"type": "string"}},
                "required": ["text"],
                "additionalProperties": false
            }),
            meta: ToolMeta {
                saves_output: true,
                needs_workspace: true,
                ..meta(10_000, Some(60_000))
            },
        },
        Spec {
            name: "fetch",
            description: "Fetch a URL through the host's http import and show the response.",
            schema: json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string"},
                    "method": {"type": "string"},
                    "headers": {"type": "array", "items": {"type": "array", "items": {"type": "string"}}},
                    "body": {"type": "string"},
                    "timeout_ms": {"type": "integer"}
                },
                "required": ["url"],
                "additionalProperties": false
            }),
            meta: meta(60_000, Some(600_000)),
        },
        Spec {
            name: "instantly",
            description: "Start a background command and return at once.",
            schema: json!({"type": "object", "properties": {}}),
            meta: job_meta(),
        },
        Spec {
            name: "limited",
            description: "Wait ms milliseconds under limit_ms.",
            schema: json!({"type": "object", "properties": {"ms": {"type": "integer"}, "limit_ms": {"type": "integer"}}}),
            meta: ToolMeta {
                parallel_safe: false,
                ..meta(1_000, Some(1_000))
            },
        },
        Spec {
            name: "panic",
            description: "Panic, at once or after waiting ms milliseconds.",
            schema: json!({"type": "object", "properties": {"ms": {"type": "integer"}}}),
            meta: meta(60_000, Some(60_000)),
        },
        Spec {
            name: "sleep",
            description: "Write text, then wait ms milliseconds.",
            schema: json!({
                "type": "object",
                "properties": {
                    "ms": {"type": "integer"},
                    "text": {"type": "string"},
                    "cd": {"type": "string"},
                    "exit": {"type": "integer"},
                    "stubborn": {"type": "boolean"}
                },
                "required": ["ms"],
                "additionalProperties": false
            }),
            meta: ToolMeta {
                tags: Tags {
                    reversible: true,
                    ..Tags::default()
                },
                saves_output: true,
                needs_workspace: true,
                writing: Some("a nap".to_owned()),
                ..meta(60_000, Some(600_000))
            },
        },
        Spec {
            name: "snap",
            description: "Attach a fixed picture, or a one-page PDF.",
            schema: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "pdf": {"type": "boolean"},
                    "wait": {"type": "boolean"},
                    "stubborn": {"type": "boolean"},
                    "type": {"type": "string"},
                    "size": {"type": "integer"}
                },
                "additionalProperties": false
            }),
            meta: meta(10_000, Some(60_000)),
        },
        Spec {
            name: "starter",
            description: "Start a command and return once it prints.",
            schema: json!({"type": "object", "properties": {"command": {"type": "string"}}}),
            meta: job_meta(),
        },
        Spec {
            name: "stubborn",
            description: "Run a command, move it when asked, then ignore every stop.",
            schema: json!({"type": "object", "properties": {"command": {"type": "string"}}}),
            meta: job_meta(),
        },
    ]
}

pub(crate) fn defs(limits: &BTreeMap<String, u64>) -> Vec<ToolDef> {
    specs()
        .into_iter()
        .map(|spec| {
            let mut meta = spec.meta;
            if let Some(ms) = limits.get(spec.name) {
                meta.timeouts = Timeouts {
                    default_ms: *ms,
                    max_ms: Some(*ms),
                };
            }
            ToolDef {
                name: spec.name.to_owned(),
                kind: ToolKind::Client(ClientTool {
                    description: spec.description.to_owned(),
                    input_schema: spec.schema.to_string(),
                    meta,
                }),
                system_line: None,
                context_line: None,
            }
        })
        .collect()
}

pub(crate) fn time_limit_ms(
    tool: &str,
    input: &str,
    limits: &BTreeMap<String, u64>,
) -> Option<u64> {
    if tool == "limited" {
        return parse(input).ok()?.get("limit_ms")?.as_u64();
    }
    if let Some(ms) = limits.get(tool) {
        return Some(*ms);
    }
    specs()
        .into_iter()
        .find(|s| s.name == tool)
        .and_then(|s| s.meta.timeouts.max_ms)
}

fn object(input: &str) -> Result<Map<String, Value>, String> {
    match parse(input)? {
        Value::Object(map) => Ok(map),
        _ => Err("input must be an object".to_owned()),
    }
}

fn only(map: &Map<String, Value>, known: &[&str]) -> Result<(), String> {
    match map.keys().find(|k| !known.contains(&k.as_str())) {
        Some(key) => Err(format!("unknown field `{key}`")),
        None => Ok(()),
    }
}

fn display(action: Action, target: String, output: OutputKind) -> Display {
    Display {
        action,
        verbs: None,
        target,
        output,
        view: None,
        title: None,
    }
}

#[derive(Debug, Clone, Default)]
struct SleepInput {
    ms: u64,
    text: String,
    cd: Option<String>,
    exit: Option<i32>,
    stubborn: bool,
}

struct SnapInput {
    name: Option<String>,
    pdf: bool,
    wait: bool,
    stubborn: bool,
    media_type: Option<String>,
    size: Option<usize>,
    slice: Option<wisp_plugin_api::types::FileSlice>,
}

impl SnapInput {
    fn target(&self) -> String {
        match (&self.name, self.pdf) {
            (Some(name), _) => name.clone(),
            (None, true) => "snap.pdf".to_owned(),
            (None, false) => "snap.png".to_owned(),
        }
    }
}

fn prepare_sleep(input: &str) -> Result<Box<dyn Prepared>, String> {
    let map = object(input)?;
    only(&map, &["ms", "text", "cd", "exit", "stubborn"])?;
    let ms = map
        .get("ms")
        .and_then(Value::as_u64)
        .ok_or("`ms` must be a whole number")?;
    let parsed = SleepInput {
        ms,
        text: map
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        cd: map.get("cd").and_then(Value::as_str).map(str::to_owned),
        exit: map
            .get("exit")
            .and_then(Value::as_i64)
            .and_then(|n| i32::try_from(n).ok()),
        stubborn: map
            .get("stubborn")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    };
    let shown = display(
        Action::Run,
        format!("sleep {}", parsed.ms),
        OutputKind::Text,
    );
    Ok(Call::boxed(shown, parsed))
}

fn prepare_fetch(input: &str) -> Result<Box<dyn Prepared>, String> {
    let map = object(input)?;
    only(&map, &["url", "method", "headers", "body", "timeout_ms"])?;
    let url = map
        .get("url")
        .and_then(Value::as_str)
        .ok_or("`url` must be a string")?
        .to_owned();
    let mut headers = Vec::new();
    for pair in map
        .get("headers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match pair.as_array().map(Vec::as_slice) {
            Some([Value::String(name), Value::String(value)]) => {
                headers.push((name.clone(), value.clone()));
            }
            _ => return Err("each header must be a [name, value] pair of strings".to_owned()),
        }
    }
    let request = Request {
        method: map
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("GET")
            .to_owned(),
        url: url.clone(),
        headers,
        body: map
            .get("body")
            .and_then(Value::as_str)
            .map(|body| body.as_bytes().to_vec()),
        timeout_ms: map.get("timeout_ms").and_then(Value::as_u64),
    };
    let shown = display(Action::Other("fetch".into()), url, OutputKind::Text);
    Ok(Call::boxed(shown, Job::Fetch(request)))
}

fn prepare_snap(input: &str) -> Result<Box<dyn Prepared>, String> {
    let map = object(input)?;
    only(
        &map,
        &["name", "pdf", "wait", "stubborn", "type", "size", "slice"],
    )?;
    let flag = |key: &str| map.get(key).and_then(Value::as_bool).unwrap_or(false);
    let parsed = SnapInput {
        name: map.get("name").and_then(Value::as_str).map(str::to_owned),
        pdf: flag("pdf"),
        wait: flag("wait"),
        stubborn: flag("stubborn"),
        media_type: map.get("type").and_then(Value::as_str).map(str::to_owned),
        size: map
            .get("size")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok()),
        slice: map
            .get("slice")
            .map(|value| {
                let pages = value
                    .as_str()
                    .ok_or("slice must be a page or page range string")?;
                wisp_tool_text::files::parse_pages(pages)
            })
            .transpose()?,
    };
    let shown = display(Action::Read, parsed.target(), OutputKind::None);
    Ok(Call::boxed(shown, parsed))
}

pub(crate) fn prepare(tool: &str, input: &str) -> Result<Box<dyn Prepared>, String> {
    match tool {
        "confirm" => {
            let map = object(input)?;
            if let Some(key) = map.keys().next() {
                return Err(format!("unknown field `{key}`"));
            }
            let shown = display(
                Action::Other("confirm".into()),
                question().header,
                OutputKind::None,
            );
            Ok(Call::boxed(shown, Job::Confirm))
        }
        "echo" => {
            let map = object(input)?;
            only(&map, &["text"])?;
            let text = map
                .get("text")
                .and_then(Value::as_str)
                .ok_or("`text` must be a string")?
                .to_owned();
            let target = text.lines().next().unwrap_or_default().to_owned();
            let shown = display(Action::Other("echo".into()), target, OutputKind::Text);
            Ok(Call::boxed(shown, Job::Echo(text)))
        }
        "fetch" => prepare_fetch(input),
        "sleep" => prepare_sleep(input),
        "snap" => prepare_snap(input),
        "limited" => {
            let ms = parse(input)?.get("ms").and_then(Value::as_u64).unwrap_or(0);
            Ok(Call::boxed(fallback_display(tool, input), Job::Limited(ms)))
        }
        "panic" => {
            let ms = parse(input)?.get("ms").and_then(Value::as_u64);
            Ok(Call::boxed(fallback_display(tool, input), Job::Panic(ms)))
        }
        "starter" | "stubborn" => {
            let command = parse(input)?
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let job = if tool == "starter" {
                Job::Starter(command)
            } else {
                Job::Stubborn(command)
            };
            Ok(Call::boxed(fallback_display(tool, input), job))
        }
        "instantly" => Ok(Call::boxed(fallback_display(tool, input), Job::Instantly)),
        other => Err(format!("there is no tool named `{other}`")),
    }
}

enum Job {
    Confirm,
    Echo(String),
    Fetch(Request),
    Sleep(SleepInput),
    Snap(SnapInput),
    Limited(u64),
    Panic(Option<u64>),
    Starter(String),
    Stubborn(String),
    Instantly,
}

fn take(call: Box<dyn Prepared>) -> Option<Job> {
    let any = call.into_any();
    let any = match any.downcast::<Job>() {
        Ok(job) => return Some(*job),
        Err(any) => any,
    };
    let any = match any.downcast::<SleepInput>() {
        Ok(input) => return Some(Job::Sleep(*input)),
        Err(any) => any,
    };
    any.downcast::<SnapInput>()
        .ok()
        .map(|input| Job::Snap(*input))
}

async fn either<A, B>(first: A, second: B) -> Result<A::Output, B::Output>
where
    A: std::future::Future,
    B: std::future::Future,
{
    match futures::future::select(Box::pin(first), Box::pin(second)).await {
        futures::future::Either::Left((out, _)) => Ok(out),
        futures::future::Either::Right((out, _)) => Err(out),
    }
}

fn join(cwd: &str, dir: &str) -> String {
    std::path::Path::new(cwd).join(dir).display().to_string()
}

pub(crate) async fn run(
    host: &dyn Host,
    call: Box<dyn Prepared>,
    ctx: &dyn CallContext,
) -> ToolRun {
    let Some(job) = take(call) else {
        return ToolRun::failed_to_start(wisp_tool_text::output::WRONG_INPUT);
    };
    match job {
        Job::Confirm => confirm(ctx).await,
        Job::Echo(text) => {
            ctx.write_output(text.as_bytes());
            ToolRun::completed()
        }
        Job::Fetch(request) => fetch(host, request, ctx).await,
        Job::Sleep(input) => sleep(host, input, ctx).await,
        Job::Snap(input) => snap(input, ctx).await,
        Job::Limited(ms) => match either(host.sleep(ms), ctx.wait_cancelled()).await {
            Ok(()) => ToolRun::completed(),
            Err(()) => ToolRun::new(Outcome::Cancelled),
        },
        Job::Panic(ms) => {
            if let Some(ms) = ms {
                host.sleep(ms).await;
            }
            panic_now()
        }
        Job::Starter(command) => starter(host, command, ctx).await,
        Job::Stubborn(command) => stubborn(host, command, ctx).await,
        Job::Instantly => instantly(host, ctx).await,
    }
}

#[allow(
    clippy::panic,
    reason = "the panic tool exists to test how the kernel contains a plugin panic"
)]
fn panic_now() -> ToolRun {
    std::panic::panic_any("the testkit's panic tool panicked")
}

async fn confirm(ctx: &dyn CallContext) -> ToolRun {
    let reply = either(ctx.wait_cancelled(), ctx.ask(vec![question()])).await;
    match reply {
        Ok(()) => ToolRun::new(Outcome::Cancelled).with_result(STOPPED),
        Err(AskReply::Answered(answers)) => ToolRun::completed().with_result(answered(&answers)),
        Err(AskReply::MessageInstead) => ToolRun::completed().with_result(MESSAGE_INSTEAD),
        Err(
            AskReply::Unavailable
            | AskReply::Attached(_)
            | AskReply::Declined
            | AskReply::ProcessEnded(_)
            | AskReply::Stopped,
        ) => ToolRun::new(Outcome::Failed(Failure {
            message: UNAVAILABLE_MESSAGE.into(),
            before_start: false,
        }))
        .with_result(UNAVAILABLE),
    }
}

pub fn answered(answers: &[wisp_plugin_api::types::Answer]) -> String {
    let question = question();
    let lines: Vec<String> = answers
        .iter()
        .map(|answer| {
            let chosen = answer.chosen.iter().filter_map(|i| {
                usize::try_from(*i)
                    .ok()
                    .and_then(|i| question.options.get(i))
                    .map(|o| o.label.clone())
            });
            let own = answer
                .own
                .iter()
                .map(|own| format!("In their own words: {own}"));
            let parts: Vec<String> = chosen.chain(own).collect();
            if parts.is_empty() {
                "Skipped".to_owned()
            } else {
                parts.join("\n")
            }
        })
        .collect();
    format!("The user answered:\n{}", lines.join("\n"))
}

async fn sleep(host: &dyn Host, input: SleepInput, ctx: &dyn CallContext) -> ToolRun {
    ctx.write_output(input.text.as_bytes());
    let cancelled = if input.stubborn {
        host.sleep(input.ms).await;
        false
    } else {
        either(host.sleep(input.ms), ctx.wait_cancelled())
            .await
            .is_err()
    };
    let mut run = if cancelled {
        ToolRun::new(Outcome::Cancelled)
    } else {
        ToolRun::new(Outcome::Completed(Completed {
            is_error: false,
            exit: input.exit.map(ExitStatus::Code),
        }))
    };
    if let Some(code) = input.exit {
        run.facts.push(Fact::Exit(code));
    }
    if let Some(dir) = input.cd {
        run.cwd = Some(join(&ctx.cwd(), &dir));
    }
    run
}

const FETCH_READ: u32 = 4_096;

async fn fetch(host: &dyn Host, request: Request, ctx: &dyn CallContext) -> ToolRun {
    let response = match host.fetch(request).await {
        Ok(response) => response,
        Err(HttpError::Cancelled) => return ToolRun::new(Outcome::Cancelled),
        Err(HttpError::TimedOut) => return ToolRun::failed_to_start("the request timed out"),
        Err(HttpError::NotAllowed(why)) => {
            return ToolRun::failed_to_start(format!("not allowed: {why}"));
        }
        Err(HttpError::Failed(why)) => return ToolRun::failed_to_start(why),
    };
    let mut body = Vec::new();
    loop {
        match response.body.read(FETCH_READ).await {
            Ok(Some(chunk)) => body.extend(chunk),
            Ok(None) => break,
            Err(_) if ctx.cancelled() => return ToolRun::new(Outcome::Cancelled),
            Err(why) => return ToolRun::failed_to_start(format!("reading the body: {why}")),
        }
    }
    let mut text = format!("HTTP {}\n", response.status);
    for (name, value) in &response.headers {
        text.push_str(&format!("{name}: {value}\n"));
    }
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&body));
    ToolRun::completed().with_result(text)
}

async fn snap(input: SnapInput, ctx: &dyn CallContext) -> ToolRun {
    let (media_type, bytes) = match (&input.media_type, input.size, input.pdf) {
        (Some(text), size, _) => (text.clone(), vec![0; size.unwrap_or(16)]),
        (None, Some(size), _) => ("application/pdf".to_owned(), vec![0; size]),
        (None, None, true) => ("application/pdf".to_owned(), PDF.to_vec()),
        (None, None, false) => ("image/png".to_owned(), png()),
    };
    let n = bytes.len() as u64;
    let text = format!(
        "{}: {media_type}, {n} bytes, attached as is.",
        input.target()
    );
    let attachment = Attachment {
        media_type: media_type.clone(),
        bytes,
        slice: input.slice,
    };
    if input.wait {
        ctx.wait_cancelled().await;
        let mut run = ToolRun::new(Outcome::Cancelled).with_result(text);
        run.attachments.push(attachment);
        return run;
    }
    if input.stubborn {
        ctx.wait_cancelled().await;
    }
    let mut run = ToolRun::completed().with_result(text);
    run.facts = vec![Fact::Media(media_type), Fact::Bytes(n)];
    run.attachments.push(attachment);
    run
}

fn bash_spec(command: String, ctx: &dyn CallContext, background: bool, label: &str) -> SpawnSpec {
    SpawnSpec {
        program: "bash".to_owned(),
        args: vec!["-c".to_owned(), command],
        cwd: ctx.cwd(),
        env: Vec::new(),
        mode: Mode::Pipe,
        size: None,
        stdin: false,
        background,
        limit_ms: None,
        label: label.to_owned(),
        keep: false,
    }
}

async fn spawn(
    host: &dyn Host,
    spec: SpawnSpec,
    ctx: &dyn CallContext,
) -> Result<Box<dyn wisp_plugin_api::host::Job>, String> {
    let spec = SpawnSpec {
        program: host.which(&spec.program).unwrap_or(spec.program),
        ..spec
    };
    host.spawn(spec, ctx)
        .await
        .map_err(|error| format!("{error:?}"))
}

async fn starter(host: &dyn Host, command: String, ctx: &dyn CallContext) -> ToolRun {
    let spec = SpawnSpec {
        limit_ms: Some(600_000),
        ..bash_spec(command.clone(), ctx, false, &command)
    };
    let job = match spawn(host, spec, ctx).await {
        Ok(job) => job,
        Err(e) => return ToolRun::failed_to_start(e),
    };
    while job.info().await.bytes == 0 {
        host.sleep(10).await;
    }
    ToolRun::completed()
}

async fn stubborn(host: &dyn Host, command: String, ctx: &dyn CallContext) -> ToolRun {
    let job = match spawn(host, bash_spec(command, ctx, false, "stubborn"), ctx).await {
        Ok(job) => job,
        Err(e) => return ToolRun::failed_to_start(e),
    };
    while !ctx.moved() {
        host.sleep(10).await;
    }
    if job.info().await.id.is_none() {
        return ToolRun::failed_to_start("the moved job has no number");
    }
    std::future::pending::<ToolRun>().await
}

async fn instantly(host: &dyn Host, ctx: &dyn CallContext) -> ToolRun {
    let spec = bash_spec("exec sleep 30".to_owned(), ctx, true, "exec sleep 30");
    let job = match spawn(host, spec, ctx).await {
        Ok(job) => job,
        Err(e) => return ToolRun::failed_to_start(e),
    };
    let Some(id) = job.info().await.id else {
        return ToolRun::failed_to_start("the background job has no number");
    };
    let mut run = ToolRun::new(Outcome::Backgrounded(BackgroundReason::Started))
        .with_result(format!("Process {id} started."));
    run.facts.push(Fact::Process(id));
    run
}
