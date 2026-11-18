use std::any::Any;
use std::sync::Arc;

use serde_json::{Map, Value, json};
use wisp_plugin_api::host::{CallContext, Host, SecretAsk, SecretEnd, SecretRefusal};
use wisp_plugin_api::tools::{
    BuildContext, ClientTool, Effect, Prepared, Purpose, Tags, Timeouts, ToolDef, ToolKind,
    ToolMeta,
};
use wisp_plugin_api::types::{Action, Display, Fact, Failure, Outcome, OutputKind, ToolRun, Verbs};
use wisp_plugin_api::{BoxFuture, Bundled, Plugin};
use wisp_tool_text::jobs;
use wisp_tool_text::question::SECRET_TOOL;

pub const MANIFEST: &str = include_str!("../wisp-plugin.toml");

pub fn bundled() -> Bundled {
    Bundled {
        manifest: MANIFEST,
        instantiate,
    }
}

fn instantiate(host: Arc<dyn Host>) -> Arc<dyn Plugin> {
    Arc::new(AskSecret { host })
}

pub const NAME: &str = SECRET_TOOL;
pub const PROCESS_MIN: u32 = 1;
pub const SHOW_MIN: u8 = 0;
pub const SHOW_MAX: u8 = 16;
pub const NAME_CHARS: usize = 40;
pub const REASON_CHARS: usize = 200;
const SHOW_TAIL: u8 = 4;
const PREFIX_EXAMPLE: &str = "sk-proj-";

const FIELDS: [&str; 6] = [
    "process",
    "name",
    "reason",
    "end_input",
    "show_start",
    "show_end",
];

const DESCRIPTION: &str = "Ask the user for a secret (a password, an API key, a token) and have Wisp type it straight into a running process. A box opens where the user types or pastes it; you never see it and it never enters this conversation or the session log, so use this instead of asking in the chat. Give the number of the process that will read it. For a program that asks at a prompt (sudo, ssh, a login), run it with bash and tty, and call this once it waits at its prompt with echo off: the secret is typed and Enter pressed. For a command that reads the secret from standard input, such as wisp account add NAME --key-stdin or a password manager's command line, start it with bash and run_in_background, without tty, then call this with end_input true. For a command that wants the secret in an environment variable or inside other text, start a short script the same way that reads one line and passes it on (IFS= read -r S; TOKEN=\"$S\" exec the-command), and call this without end_input, since the script reads a whole line; never use one that puts the secret on a command line or prints it. If the result says the terminal would show what is typed, start the command again without tty so that it reads standard input; don't ask the user to take the terminal over for a secret. Make this the last call of your reply: calls after it in the same reply are not run. The result says only whether the secret was typed, never what it was.";

pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "process": {
                "type": "integer",
                "minimum": PROCESS_MIN,
                "description": "The number of the running process that reads the secret."
            },
            "name": {
                "type": "string",
                "description": format!("What you are asking for, in a few words, shown as the box's title, such as \"sudo password\" or \"OpenAI API key\". At most {NAME_CHARS} characters.")
            },
            "reason": {
                "type": "string",
                "description": format!("One sentence shown to the user: why it is needed and what will be done with it. At most {REASON_CHARS} characters.")
            },
            "end_input": {
                "type": "boolean",
                "description": "Close the process's input after the secret, with no line break, for a command that reads standard input to its end. Defaults to false: the secret is followed by Enter."
            },
            "show_start": {
                "type": "integer",
                "minimum": SHOW_MIN,
                "maximum": SHOW_MAX,
                "description": format!("How many characters at the start of a pasted secret the box shows, so the user can check it is the right one. Leave this and show_end out for a password or passphrase: the box then shows only stars. For a key or token with a public prefix, give the prefix's length here ({} for {PREFIX_EXAMPLE}) and {SHOW_TAIL} in show_end. For one with no public prefix, give {SHOW_MIN} here and {SHOW_TAIL} in show_end. Wisp shows less when the secret is short, and only stars for one typed by hand. Defaults to {SHOW_MIN}.", PREFIX_EXAMPLE.len())
            },
            "show_end": {
                "type": "integer",
                "minimum": SHOW_MIN,
                "maximum": SHOW_MAX,
                "description": format!("How many characters at the end of a pasted secret the box shows. Defaults to {SHOW_MIN}.")
            }
        },
        "required": ["name", "process", "reason"],
        "additionalProperties": false
    })
}

fn line(map: &Map<String, Value>, field: &str, most: usize) -> Result<String, String> {
    map.get(field)
        .and_then(Value::as_str)
        .filter(|text| {
            !text.trim().is_empty()
                && text.chars().count() <= most
                && !text.chars().any(char::is_control)
        })
        .map(str::to_owned)
        .ok_or_else(|| {
            format!("`{field}` must be one line of text, not empty, at most {most} characters")
        })
}

fn shown(map: &Map<String, Value>, field: &str) -> Result<u8, String> {
    match map.get(field) {
        None => Ok(SHOW_MIN),
        Some(value) => value
            .as_u64()
            .and_then(|n| u8::try_from(n).ok())
            .filter(|n| (SHOW_MIN..=SHOW_MAX).contains(n))
            .ok_or_else(|| {
                format!("`{field}` must be a whole number from {SHOW_MIN} to {SHOW_MAX}")
            }),
    }
}

pub fn parse(input: &Value) -> Result<SecretAsk, String> {
    let map = input.as_object().ok_or("input must be an object")?;
    if let Some(key) = map.keys().find(|key| !FIELDS.contains(&key.as_str())) {
        return Err(format!("unknown field `{key}`"));
    }
    let process = map
        .get("process")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n >= PROCESS_MIN)
        .ok_or_else(|| format!("`process` must be a whole number, {PROCESS_MIN} or more"))?;
    let end_input = match map.get("end_input") {
        None => false,
        Some(value) => value.as_bool().ok_or("`end_input` must be true or false")?,
    };
    Ok(SecretAsk {
        process,
        name: line(map, "name", NAME_CHARS)?,
        reason: line(map, "reason", REASON_CHARS)?,
        end_input,
        show_start: shown(map, "show_start")?,
        show_end: shown(map, "show_end")?,
    })
}

pub fn parse_text(input: &str) -> Result<SecretAsk, String> {
    let value: Value =
        serde_json::from_str(input).map_err(|e| format!("input is not JSON: {e}"))?;
    parse(&value)
}

pub fn display(ask: &SecretAsk) -> Display {
    Display {
        action: Action::Other(NAME.to_owned()),
        verbs: Some(Verbs {
            doing: "Asking for".to_owned(),
            done: "Entered".to_owned(),
            failed: "couldn't ask".to_owned(),
        }),
        target: ask.name.clone(),
        output: OutputKind::None,
        view: None,
        title: None,
    }
}

fn refused(refusal: SecretRefusal, n: u32) -> String {
    match refusal {
        SecretRefusal::NoProcess => format!("Not asked: {}.", jobs::no_process(n)),
        SecretRefusal::Ended => format!("Not asked: process {n} has ended."),
        SecretRefusal::ShowsInput => format!("Not typed: process {n}'s terminal would show what is typed, so the secret would land in its output. If it hasn't reached its prompt yet, wait for the prompt and call again; otherwise start the command again without tty so that it reads the secret from standard input."),
        SecretRefusal::NoInput => format!("Not asked: process {n} takes no input. Start the command with run_in_background, which keeps its standard input open, and call again."),
        SecretRefusal::Attached => format!("Not asked: the user is typing into process {n} themselves."),
        SecretRefusal::TooLong => "Not typed: the secret is longer than one terminal line can take. Start the command again without tty so that it reads standard input.".to_owned(),
    }
}

fn failed(sentence: String) -> ToolRun {
    ToolRun::new(Outcome::Failed(Failure {
        message: sentence.clone(),
        before_start: false,
    }))
    .with_result(sentence)
}

pub fn finish(ask: &SecretAsk, end: SecretEnd) -> ToolRun {
    let n = ask.process;
    match end {
        SecretEnd::Typed => {
            let after = if ask.end_input {
                " and closed its input"
            } else {
                ", then pressed Enter"
            };
            let mut run = ToolRun::completed().with_result(format!("The user entered the secret and Wisp typed it into process {n}{after}. You were not shown it; check the process to see whether it was accepted."));
            run.facts.push(Fact::Process(n));
            run
        }
        SecretEnd::ProcessEnded => failed(format!(
            "Process {n} ended before the user entered the secret, so nothing was typed."
        )),
        SecretEnd::Unavailable => failed("Not asked: no one can enter a secret in this session. Don't ask for it in the chat or from another agent; say what the process needs.".to_owned()),
        SecretEnd::Stopped => ToolRun::new(Outcome::Cancelled).with_result("Not entered: the user stopped the turn while the secret box was open."),
        SecretEnd::Refused(refusal) => failed(refused(refusal, n)),
        SecretEnd::Failed => failed(format!("The secret could not be typed into process {n}: its input closed or the write stopped part-way. Check the process, and ask again if it still waits.")),
    }
}

pub fn meta() -> ToolMeta {
    ToolMeta {
        effect: Effect::NonIdempotent,
        tags: Tags::default(),
        parallel_safe: false,
        timeouts: Timeouts {
            default_ms: 0,
            max_ms: None,
        },
        saves_output: false,
        needs_workspace: false,
        ends_reply: true,
        asks_user: true,
        reports_jobs: false,
        writing: Some("a secret request".to_owned()),
        purpose: Purpose::Run,
    }
}

pub fn def() -> ToolDef {
    ToolDef {
        name: NAME.to_owned(),
        kind: ToolKind::Client(ClientTool {
            description: DESCRIPTION.to_owned(),
            input_schema: schema().to_string(),
            meta: meta(),
        }),
        system_line: None,
        context_line: None,
    }
}

pub struct AskSecret {
    host: Arc<dyn Host>,
}

impl AskSecret {
    pub fn new(host: Arc<dyn Host>) -> Self {
        Self { host }
    }
}

struct Asking(SecretAsk);

impl Prepared for Asking {
    fn display(&self) -> Display {
        display(&self.0)
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> {
        Box::new(self.0)
    }
}

impl Plugin for AskSecret {
    fn describe<'a>(&'a self, _ctx: &'a BuildContext) -> BoxFuture<'a, Vec<ToolDef>> {
        Box::pin(async { vec![def()] })
    }

    fn prepare<'a>(
        &'a self,
        _tool: &'a str,
        input: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Prepared>, String>> {
        Box::pin(async move {
            let call: Box<dyn Prepared> = Box::new(Asking(parse_text(input)?));
            Ok(call)
        })
    }

    fn run<'a>(
        &'a self,
        call: Box<dyn Prepared>,
        ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, ToolRun> {
        Box::pin(async move {
            match call.into_any().downcast::<SecretAsk>() {
                Ok(ask) => {
                    let end = self.host.type_into((*ask).clone(), ctx).await;
                    finish(&ask, end)
                }
                Err(_) => ToolRun::failed_to_start(wisp_tool_text::output::WRONG_INPUT),
            }
        })
    }
}

#[cfg(test)]
mod tests;
