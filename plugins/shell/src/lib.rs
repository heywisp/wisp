mod bash;
mod notice;
mod process;

use std::any::Any;
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::Value;
use wisp_plugin_api::hooks::{Event, Reply};
use wisp_plugin_api::host::{CallContext, Captured, Host};
use wisp_plugin_api::tools::{BuildContext, DoctorCell, DoctorRow, Prepared, ToolDef};
use wisp_plugin_api::types::{Display, ToolRun};
use wisp_plugin_api::{BoxFuture, Bundled, Plugin};
use wisp_tool_text::question::SECRET_CAPABILITY;

pub use bash::{
    BashInput, DEFAULT_TIMEOUT_MS, MAX_COMMAND_BYTES, MAX_TIMEOUT_MS, NON_INTERACTIVE, SYSTEM_LINE,
    TERMINAL_ENV, first_line, parse_version, script,
};
pub use notice::{PROCESSES, notice};
pub use process::input::{
    ATTACH_ALONE, INPUT_QUIET_MS, INPUT_WAIT_MS, INTERRUPT_QUIET_MS, INTERRUPT_WAIT_MS,
    MAX_ATTACH_REASON, MAX_CONTEXT, MAX_INPUT, MAX_MATCHES, MAX_WAIT_MS, NO_ID, RANGE_LINES,
    SEARCH_LINES, TAIL_LINES, UNTIL_IGNORED,
};
pub use process::restored::POSITION_LOST;
pub use process::run::{
    LINE_LIMIT, NO_ONE_TO_ASK, line_limit_text, not_a_terminal, unwritten_before_interrupt,
};
pub use process::view as views;
pub use process::{KILL_ALLOWANCE_MS, SEARCH_ALLOWANCE_MS, STEERED_NOTE, hidden_line};

pub const MANIFEST: &str = include_str!("../wisp-plugin.toml");
pub const BASH: &str = "bash";
pub const PROCESS: &str = "process";
pub const SHELL: &str = "shell";
pub const VERSION_LIMIT_MS: u64 = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    pub start_quiet_ms: u64,
    pub start_max_ms: u64,
    pub hand_back_ms: u64,
    pub yield_ms: u64,
}

pub const START_QUIET_MS: u64 = 1_000;
pub const START_MAX_MS: u64 = 10_000;
pub const FOREGROUND_QUIET_MS: u64 = 10_000;
pub const FOREGROUND_YIELD_MS: u64 = 120_000;

impl Default for Timing {
    fn default() -> Self {
        Self {
            start_quiet_ms: START_QUIET_MS,
            start_max_ms: START_MAX_MS,
            hand_back_ms: FOREGROUND_QUIET_MS,
            yield_ms: FOREGROUND_YIELD_MS,
        }
    }
}

pub fn bundled() -> Bundled {
    Bundled {
        manifest: MANIFEST,
        instantiate,
    }
}

fn instantiate(host: Arc<dyn Host>) -> Arc<dyn Plugin> {
    Arc::new(Shell::new(host))
}

pub struct Shell {
    host: Arc<dyn Host>,
    timing: Timing,
    found: Mutex<Option<String>>,
}

pub fn described(captured: &Captured) -> String {
    match captured {
        Captured::NotFound => "not found on PATH".to_owned(),
        Captured::Failed(_) => "bash (version unknown)".to_owned(),
        Captured::Output(bytes) => parse_version(&String::from_utf8_lossy(bytes))
            .unwrap_or_else(|| "bash (version unknown)".to_owned()),
    }
}

pub fn context_line(captured: &Captured) -> String {
    format!("Shell for the bash tool: {}", described(captured))
}

enum Input {
    Bash(BashInput),
    Process(Box<process::input::Call>),
}

struct Call {
    display: Display,
    input: Input,
}

impl Prepared for Call {
    fn display(&self) -> Display {
        self.display.clone()
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> {
        Box::new(self.input)
    }
}

fn json(input: &str) -> Result<Value, String> {
    serde_json::from_str(input).map_err(|e| format!("input is not JSON: {e}"))
}

impl Shell {
    pub fn new(host: Arc<dyn Host>) -> Self {
        Self::with_timing(host, Timing::default())
    }

    pub fn with_timing(host: Arc<dyn Host>, timing: Timing) -> Self {
        Self {
            host,
            timing,
            found: Mutex::new(None),
        }
    }

    fn found(&self) -> Option<String> {
        self.found
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    async fn version(&self) -> Captured {
        self.host
            .capture(
                BASH.to_owned(),
                vec!["--version".to_owned()],
                VERSION_LIMIT_MS,
            )
            .await
    }
}

fn secret_on(capabilities: &[String]) -> bool {
    capabilities.iter().any(|c| c == SECRET_CAPABILITY)
}

impl Plugin for Shell {
    fn describe<'a>(&'a self, ctx: &'a BuildContext) -> BoxFuture<'a, Vec<ToolDef>> {
        Box::pin(async move {
            *self.found.lock().unwrap_or_else(PoisonError::into_inner) = self.host.which(BASH);
            let line = if ctx.capabilities.iter().any(|c| c == SHELL) {
                Some(context_line(&self.version().await))
            } else {
                None
            };
            let secret = secret_on(&ctx.capabilities);
            vec![bash::def(line, secret), process::def(secret)]
        })
    }

    fn doctor<'a>(&'a self, _ctx: &'a BuildContext) -> BoxFuture<'a, Vec<DoctorRow>> {
        Box::pin(async move {
            let version = described(&self.version().await);
            let value = match self.host.which(BASH) {
                Some(path) => format!("{} ({version})", self.host.shorten(&path)),
                None => version,
            };
            vec![DoctorRow {
                key: BASH.to_owned(),
                cells: vec![DoctorCell::Text(value)],
            }]
        })
    }

    fn prepare<'a>(
        &'a self,
        tool: &'a str,
        input: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Prepared>, String>> {
        Box::pin(async move {
            let value = json(input)?;
            let (display, input) = match tool {
                BASH => {
                    let parsed = bash::parse(&value)?;
                    (bash::display(&parsed, &value), Input::Bash(parsed))
                }
                PROCESS => {
                    let call = process::input::parse(&value)?;
                    (process::display(&call), Input::Process(Box::new(call)))
                }
                other => return Err(format!("there is no tool named `{other}`")),
            };
            let call: Box<dyn Prepared> = Box::new(Call { display, input });
            Ok(call)
        })
    }

    fn rejected_display<'a>(
        &'a self,
        tool: &'a str,
        input: &'a str,
    ) -> BoxFuture<'a, Option<Display>> {
        Box::pin(std::future::ready(
            (tool == PROCESS).then(|| process::rejected(&json(input).unwrap_or(Value::Null))),
        ))
    }

    fn time_limit_ms<'a>(&'a self, tool: &'a str, input: &'a str) -> BoxFuture<'a, Option<u64>> {
        Box::pin(std::future::ready({
            let value = json(input).unwrap_or(Value::Null);
            match tool {
                BASH => bash::time_limit_ms(&value, &self.timing),
                PROCESS => process::time_limit_ms(&value),
                _ => None,
            }
        }))
    }

    fn movable<'a>(&'a self, tool: &'a str, input: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(std::future::ready(
            tool == BASH && json(input).is_ok_and(|value| bash::movable(&value)),
        ))
    }

    fn interrupted_result<'a>(
        &'a self,
        tool: &'a str,
        input: &'a str,
    ) -> BoxFuture<'a, Option<String>> {
        Box::pin(async move {
            if tool != PROCESS {
                return None;
            }
            process::interrupted_result(&json(input).ok()?)
        })
    }

    fn run<'a>(
        &'a self,
        call: Box<dyn Prepared>,
        ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, ToolRun> {
        Box::pin(async move {
            match call.into_any().downcast::<Input>().map(|b| *b) {
                Ok(Input::Bash(input)) => {
                    let bash = bash::Bash {
                        host: self.host.as_ref(),
                        path: self.found(),
                        timing: self.timing,
                    };
                    bash.run(input, ctx).await
                }
                Ok(Input::Process(call)) => process::run::run(self.host.as_ref(), *call, ctx).await,
                Err(_) => ToolRun::failed_to_start(wisp_tool_text::output::WRONG_INPUT),
            }
        })
    }

    fn on_hook<'a>(&'a self, hook: &'a str, _event: Event) -> BoxFuture<'a, Reply> {
        let text = match hook {
            PROCESSES => self.host.opening().as_ref().and_then(notice),
            _ => None,
        };
        Box::pin(async move { text.map_or(Reply::None, |text| Reply::Append(vec![text])) })
    }
}

#[cfg(test)]
mod tests;
