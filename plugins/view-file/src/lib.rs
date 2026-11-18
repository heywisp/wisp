mod look;
mod words;

use std::any::Any;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};
use wisp_plugin_api::host::{CallContext, Host, TokenCount};
use wisp_plugin_api::tools::{
    BuildContext, ClientTool, Effect, Prepared, Purpose, Tags, Timeouts, ToolDef, ToolKind,
    ToolMeta,
};
use wisp_plugin_api::types::{
    Action, Attachment, Completed, Display, Fact, Outcome, OutputKind, Part, ToolRun,
};
use wisp_plugin_api::{BoxFuture, Bundled, Plugin};
use wisp_tool_text::output::group;

use look::{Job, Looked, TextView, Total};

pub const NAME: &str = "view_file";

pub const SHELL: &str = "shell";

pub const MAX_PATH_BYTES: usize = 4096;
pub const MAX_VIEW_TOKENS: u64 = 50_000;
pub const STOPPED: &str = "Stopped before the file was attached.";
pub const SYSTEM_LINE: &str =
    "You can attach a file on the user's computer to your context with the view_file tool.";
const TIME_LIMIT_MS: u64 = 30_000;
const COUNT_FOR_MS: u64 = 20_000;
pub const TOKEN_COUNT_LIMIT_MS: u64 = 10_000;

pub const MANIFEST: &str = include_str!("../wisp-plugin.toml");

pub fn bundled() -> Bundled {
    Bundled {
        manifest: MANIFEST,
        instantiate,
    }
}

fn instantiate(host: Arc<dyn Host>) -> Arc<dyn Plugin> {
    Arc::new(ViewFile::new(host))
}

pub struct ViewFile {
    host: Arc<dyn Host>,
    bash: AtomicBool,
}

impl ViewFile {
    pub fn new(host: Arc<dyn Host>) -> Self {
        Self {
            host,
            bash: AtomicBool::new(true),
        }
    }

    fn bash(&self) -> bool {
        self.bash.load(Ordering::Relaxed)
    }
}

pub fn description(bash: bool) -> String {
    let limit = words::text_limit();
    let edge = group(u64::from(wisp_media::MAX_LONG_EDGE));
    if bash {
        format!(
            "Attach a file from the user's computer to this tool's result so you \
can inspect it in your next step: a screenshot you took, a chart or page a script drew, a \
document, a log or a source file. Text comes back as text: up to {limit}, or else \
the start of the file and the bash command that reads the next part. Any other file is attached \
whole, as it is, with its media type, and the provider decides whether it can read it. If it \
can't, or the file is too large, the result says so: convert or cut it with bash (a smaller \
image, the pages you need, extracted text) and view that instead. Keep images you attach at \
most {edge} px on their long edge; resize a copy with bash first. Attached files stay in the \
conversation, so attach only what you need. A relative path is resolved against the working \
directory, and ~ is the user's home."
        )
    } else {
        format!(
            "Attach a file from the user's computer to this tool's \
result so you can inspect it in your next step: a screenshot you took, a chart or page a script \
drew, a document, a log or a source file. Text comes back as text: up to {limit}, \
or else only the start of the file. Any other file is attached whole, as it is, with its media \
type, and the provider decides whether it can read it. If it can't, or the file is too large, the \
result says so, and that file can't be read in this session. Images over {edge} px on their long \
edge can't be attached. Attached files stay in the conversation, so attach only what you need. A \
relative path is resolved against the working directory, and ~ is the user's home."
        )
    }
}

pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {"type": "string", "description": "The file to look at."}
        },
        "required": ["path"],
        "additionalProperties": false
    })
}

pub fn meta() -> ToolMeta {
    ToolMeta {
        effect: Effect::ReadOnly,
        tags: Tags {
            reads_untrusted: true,
            ..Tags::default()
        },
        parallel_safe: true,
        timeouts: Timeouts {
            default_ms: TIME_LIMIT_MS,
            max_ms: Some(TIME_LIMIT_MS),
        },
        saves_output: true,
        needs_workspace: true,
        ends_reply: false,
        asks_user: false,
        reports_jobs: false,
        writing: Some("a file path".to_owned()),
        purpose: Purpose::Run,
    }
}

pub fn def(bash: bool) -> ToolDef {
    ToolDef {
        name: NAME.to_owned(),
        kind: ToolKind::Client(ClientTool {
            description: description(bash),
            input_schema: schema().to_string(),
            meta: meta(),
        }),
        system_line: Some(SYSTEM_LINE.to_owned()),
        context_line: None,
    }
}

struct ViewInput {
    path: String,
}

impl Prepared for ViewInput {
    fn display(&self) -> Display {
        Display {
            action: Action::Read,
            verbs: None,
            target: self.path.clone(),
            output: OutputKind::None,
            view: None,
            title: None,
        }
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> {
        Box::new(self.path)
    }
}

fn parse(input: &str) -> Result<ViewInput, String> {
    let value: Value =
        serde_json::from_str(input).map_err(|e| format!("input is not JSON: {e}"))?;
    let map = value.as_object().ok_or("input must be an object")?;
    if let Some(key) = map.keys().find(|k| k.as_str() != "path") {
        return Err(format!("unknown field `{key}`"));
    }
    let path = map
        .get("path")
        .and_then(Value::as_str)
        .ok_or("`path` must be a string")?;
    if path.trim().is_empty() {
        return Err("`path` is empty".to_owned());
    }
    if path.len() > MAX_PATH_BYTES {
        return Err(format!(
            "`path` is {} bytes; the limit is {} bytes",
            group(path.len() as u64),
            group(MAX_PATH_BYTES as u64)
        ));
    }
    if path.contains('\0') {
        return Err("`path` contains a NUL byte".to_owned());
    }
    Ok(ViewInput {
        path: path.to_owned(),
    })
}

pub fn resolve(written: &str, cwd: &Path, home: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(rest) = written.strip_prefix('~')
        && (rest.is_empty() || rest.starts_with('/'))
    {
        let Some(home) = home else {
            return Err(format!(
                "{written} starts with ~, but the user's home directory isn't known; use an \
absolute path."
            ));
        };
        let rest = rest.trim_start_matches('/');
        return Ok(if rest.is_empty() {
            home.to_path_buf()
        } else {
            home.join(rest)
        });
    }
    let path = Path::new(written);
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    })
}

fn refused(text: String) -> ToolRun {
    ToolRun::new(Outcome::Completed(Completed {
        is_error: true,
        exit: None,
    }))
    .with_result(text)
}

fn stopped() -> ToolRun {
    ToolRun::new(Outcome::Cancelled).with_result(STOPPED)
}

async fn view(host: &dyn Host, written: String, bash: bool, ctx: &dyn CallContext) -> ToolRun {
    let home = host.home().map(PathBuf::from);
    let resolved = match resolve(&written, Path::new(&ctx.cwd()), home.as_deref()) {
        Ok(path) => path,
        Err(text) => return refused(text),
    };
    let shown = host.shorten(&resolved.display().to_string());
    let job = Job {
        written: written.clone(),
        resolved,
        shown: shown.clone(),
        media_types: host.media_types(),
        host,
        ctx,
        count_until_ms: host.elapsed_ms().saturating_add(COUNT_FOR_MS),
        bash,
    };
    if ctx.cancelled() {
        return stopped();
    }
    match look::look(&job).await {
        Looked::Stopped => stopped(),
        Looked::Refused(text) => refused(text),
        Looked::Text(text) => text_run(&shown, &text, bash, ctx),
        Looked::Media { media_type, bytes } => {
            media_run(host, &written, &shown, media_type, bytes, bash, ctx).await
        }
    }
}

fn text_run(shown: &str, view: &TextView, bash: bool, ctx: &dyn CallContext) -> ToolRun {
    ctx.write_output(view.text.as_bytes());
    let tokens = words::about_tokens(view.text.len() as u64);
    let mut run = ToolRun::completed().with_result(words::text_result(shown, view, tokens, bash));
    run.facts = vec![part_fact(view.lines, view.total), Fact::Tokens(tokens)];
    run
}

fn part_fact(sent: u64, total: Total) -> Fact {
    match total {
        Total::Exact(n) => Fact::Part(Part {
            sent,
            total: n.max(sent),
            more_than: false,
        }),
        Total::MoreThan(n) => Fact::Part(Part {
            sent,
            total: n.max(sent.saturating_sub(1)),
            more_than: true,
        }),
    }
}

enum Counted {
    Stopped,
    Count(TokenCount),
}

struct CountRace<'a> {
    cancelled: BoxFuture<'a, ()>,
    count: BoxFuture<'a, TokenCount>,
    limit: BoxFuture<'a, ()>,
}

impl std::future::Future for CountRace<'_> {
    type Output = Counted;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Counted> {
        use std::task::Poll;
        if self.cancelled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Counted::Stopped);
        }
        if let Poll::Ready(count) = self.count.as_mut().poll(cx) {
            return Poll::Ready(Counted::Count(count));
        }
        if self.limit.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Counted::Count(TokenCount::Failed(format!(
                "no count within {} seconds",
                TOKEN_COUNT_LIMIT_MS / 1000
            ))));
        }
        Poll::Pending
    }
}

async fn media_run(
    host: &dyn Host,
    written: &str,
    shown: &str,
    media_type: String,
    bytes: Vec<u8>,
    bash: bool,
    ctx: &dyn CallContext,
) -> ToolRun {
    if let Some(edge) = host.image_edge()
        && media_type.starts_with("image/")
        && let Ok((width, height)) = wisp_media::dimensions(&bytes)
        && width.max(height) > edge
    {
        return refused(words::too_wide(written, (width, height), edge, bash));
    }
    let counted = CountRace {
        cancelled: ctx.wait_cancelled(),
        count: host.count_tokens(media_type.clone(), bytes.clone()),
        limit: host.sleep(TOKEN_COUNT_LIMIT_MS),
    }
    .await;
    let counted = match counted {
        Counted::Stopped => return stopped(),
        Counted::Count(counted) => counted,
    };
    let tokens = match counted {
        TokenCount::Counted(tokens) if tokens > MAX_VIEW_TOKENS => {
            return refused(words::too_many_tokens(written, tokens, bash));
        }
        TokenCount::Counted(tokens) => Some(tokens),
        TokenCount::Unsupported | TokenCount::Failed(_) => None,
    };
    let n = bytes.len() as u64;
    let mut run =
        ToolRun::completed().with_result(words::media_result(shown, &media_type, n, tokens));
    run.facts = vec![Fact::Media(media_type.clone()), Fact::Bytes(n)];
    run.facts.extend(tokens.map(Fact::Tokens));
    run.attachments.push(Attachment { media_type, bytes });
    run
}

impl Plugin for ViewFile {
    fn describe<'a>(&'a self, ctx: &'a BuildContext) -> BoxFuture<'a, Vec<ToolDef>> {
        let bash = ctx.capabilities.iter().any(|c| c == SHELL);
        self.bash.store(bash, Ordering::Relaxed);
        Box::pin(async move { vec![def(bash)] })
    }

    fn prepare<'a>(
        &'a self,
        _tool: &'a str,
        input: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Prepared>, String>> {
        Box::pin(async move {
            let call: Box<dyn Prepared> = Box::new(parse(input)?);
            Ok(call)
        })
    }

    fn time_limit_ms<'a>(&'a self, _tool: &'a str, _input: &'a str) -> BoxFuture<'a, Option<u64>> {
        Box::pin(std::future::ready(Some(TIME_LIMIT_MS)))
    }

    fn run<'a>(
        &'a self,
        call: Box<dyn Prepared>,
        ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, ToolRun> {
        Box::pin(async move {
            match call.into_any().downcast::<String>() {
                Ok(path) => view(self.host.as_ref(), *path, self.bash(), ctx).await,
                Err(_) => ToolRun::failed_to_start(wisp_tool_text::output::WRONG_INPUT),
            }
        })
    }
}

#[cfg(test)]
mod tests;
