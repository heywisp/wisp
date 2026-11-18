pub(crate) mod input;
pub(crate) mod restored;
pub(crate) mod run;
pub mod view;

use serde_json::{Value, json};
use wisp_plugin_api::host::Host;
use wisp_plugin_api::tools::{
    ClientTool, Effect, Purpose, Tags, Timeouts, ToolDef, ToolKind, ToolMeta,
};
use wisp_plugin_api::types::{
    Action, Display, Key, OutputKind, ProcessView, ReadKind, SignalKind, View,
};

use wisp_tool_text::question::SECRET_TOOL;

use crate::PROCESS;
use input::Call;

pub const SEARCH_ALLOWANCE_MS: u64 = 120_000;
pub const KILL_ALLOWANCE_MS: u64 = 4_000;
pub const STEERED_NOTE: &str = "The wait ended early because the user sent a message.";
const HIDDEN_LINE: &str = "The terminal has echo off, so it is probably asking for a password. Don't type secrets yourself: ";

const DESCRIPTION: &str = "Work with a process that a bash result gave a number: one started with run_in_background, handed back while waiting in a terminal, moved to the background by the user, or finished with more to read than its result showed. Give id to act on one; omit it to list this session's processes. Without input, keys, signal or wait_ms it answers at once with the output that is new since you last looked. timeout_ms gives the process a new time limit counted from now, or 0 for none. read tail shows the last lines, read lines a range from a line number, read search the lines matching a regular expression anywhere in the output, with their line numbers, and read screen a terminal's current screen. Waiting with wait_ms (up to 600000) costs one call however long it lasts, so prefer one long wait with until or quiet_ms over repeated short calls, or end your turn: you are told when a process ends. Line numbers count from 1 over the whole output and match the output file, whose path the results give, so they also work with sed -n on it.";

pub(crate) const REPEATED_CHARS: usize = 4;

pub(crate) fn schema(secret: bool) -> Value {
    let ask = if secret {
        format!(
            "Ask the user to type into this terminal process themselves, for anything only a person can do other than entering a secret, which {SECRET_TOOL} does, with a short reason shown to them."
        )
    } else {
        "Ask the user to type into this terminal process themselves, for a password or anything only a person can do, with a short reason shown to them.".to_owned()
    };
    json!({
        "type": "object",
        "properties": {
            "ask_user_to_attach": {
                "type": "string",
                "description": format!("{ask} The call waits until they have typed and detached, decline, or the process ends.")
            },
            "context": {
                "type": "integer",
                "description": "With read search: how many lines to show before and after each match, 0 to 10. Defaults to 0."
            },
            "description": {
                "type": "string",
                "description": "Five to ten words on what this call is for, shown to the user, such as Check the dev server's output or Ask the reviewer for its findings. Give one on every call, and describe the purpose, not the text you type."
            },
            "from": {
                "type": "integer",
                "description": "With read lines: the first line to show. With read search: the first line to search, to continue after an earlier search's last match. Defaults to 1."
            },
            "id": {
                "type": "integer",
                "description": "The process number from a bash result."
            },
            "input": {
                "type": "string",
                "description": "Text to write to the process's stdin, exactly as given; no newline is added. A terminal in line mode keeps at most 4095 bytes per line, so a longer line is refused there: write long text to a file, or use a process without tty."
            },
            "keys": {
                "type": "array",
                "items": {"type": "string"},
                "description": "Keys to press after input, by name: Enter, Tab, Space, Esc, Backspace, Delete, Up, Down, Left, Right, Home, End, PageUp, PageDown, F1 to F12, C-a to C-z (C-c interrupts and C-d ends input in a terminal) and EOF, which closes stdin."
            },
            "lines": {
                "type": "integer",
                "description": "With read tail or lines: how many lines to show. With read search: the most matching lines to show, at most 500. Defaults to 50 for tail and search and 100 for lines."
            },
            "pattern": {
                "type": "string",
                "description": "With read search: a regular expression matched against each line; (?i) at the start ignores case."
            },
            "quiet_ms": {
                "type": "integer",
                "description": "Stop waiting once the output (or the terminal screen) has not changed for this long. Defaults to 1000 after input or keys and 500 after signal interrupt."
            },
            "read": {
                "type": "string",
                "enum": ["new", "tail", "lines", "search", "screen"],
                "description": "What to show: new (output since you last looked; the default), tail (the last lines), lines (from a line number), search (lines matching pattern), or screen (a terminal's current screen; the default for a terminal showing a full-screen program)."
            },
            "signal": {
                "type": "string",
                "enum": ["interrupt", "kill"],
                "description": "interrupt is Ctrl-C: in a terminal it reaches the program in the foreground, otherwise every process the command started gets SIGINT. kill stops the process and everything it started."
            },
            "size": {
                "type": "string",
                "description": "Resize a terminal, as COLUMNSxROWS such as 120x40."
            },
            "timeout_ms": {
                "type": "integer",
                "description": "A new time limit for the process in milliseconds, counted from now; 0 removes its limit."
            },
            "until": {
                "type": "string",
                "description": "A regular expression: stop waiting as soon as new output, or a line of the terminal screen, matches it."
            },
            "wait_ms": {
                "type": "integer",
                "description": "The longest to wait, in milliseconds, at most 600000, for the process to exit, until to match or quiet_ms to pass. Defaults to 30000 after input or keys, 2000 after signal interrupt, otherwise 0."
            }
        },
        "additionalProperties": false
    })
}

fn meta() -> ToolMeta {
    ToolMeta {
        effect: Effect::NonIdempotent,
        tags: Tags {
            irreversible: true,
            external_message: true,
            reads_untrusted: true,
            ..Tags::default()
        },
        parallel_safe: false,
        timeouts: Timeouts {
            default_ms: 0,
            max_ms: Some(input::MAX_WAIT_MS + SEARCH_ALLOWANCE_MS),
        },
        saves_output: false,
        needs_workspace: false,
        ends_reply: false,
        asks_user: true,
        reports_jobs: true,
        writing: Some("input for a process".to_owned()),
        purpose: Purpose::Run,
    }
}

pub(crate) fn def(secret: bool) -> ToolDef {
    let ask = if secret {
        format!(
            "ask_user_to_attach asks the user to type into a terminal process themselves, for a login or a program you can't drive, and waits until they are done; for a password or any other secret call {SECRET_TOOL} instead, and never ask for one in the chat."
        )
    } else {
        "ask_user_to_attach asks the user to type into a terminal process themselves, for a password prompt, a login or a program you can't drive, and waits until they are done; never ask for a secret in the chat.".to_owned()
    };
    ToolDef {
        name: PROCESS.to_owned(),
        kind: ToolKind::Client(ClientTool {
            description: format!("{DESCRIPTION} {ask}"),
            input_schema: schema(secret).to_string(),
            meta: meta(),
        }),
        system_line: None,
        context_line: None,
    }
}

fn view(call: &Call) -> View {
    View::Process(ProcessView {
        id: call.id,
        text: call.input.clone(),
        keys: call.keys.clone(),
        signal: call.signal,
        read: call.read,
        pattern: call.pattern.as_ref().map(|(_, source)| source.clone()),
    })
}

fn action(
    id: Option<u32>,
    signal: Option<SignalKind>,
    writes: bool,
    waits: bool,
    read: Option<ReadKind>,
) -> Action {
    match (id, signal) {
        (None, _) => Action::Other(PROCESS.to_owned()),
        (Some(_), Some(kind)) => Action::Signal(kind),
        (Some(_), None) if writes => Action::Input,
        (Some(_), None) if waits => Action::Wait,
        (Some(_), None) if read == Some(ReadKind::Search) => Action::Search,
        (Some(_), None) => Action::Read,
    }
}

fn title(description: Option<&str>, typed: Option<&str>) -> Option<String> {
    let description = description.map(str::trim).filter(|d| !d.is_empty())?;
    let repeats = typed.is_some_and(|typed| {
        typed
            .split(char::is_control)
            .chain(typed.split(['\r', '\n']))
            .map(str::trim)
            .filter(|piece| piece.chars().count() >= REPEATED_CHARS)
            .any(|piece| description.contains(piece))
    });
    (!repeats).then(|| description.to_owned())
}

pub(crate) fn display(call: &Call) -> Display {
    let title = title(call.description.as_deref(), call.input.as_deref());
    if let (Some(id), Some(reason)) = (call.id, &call.attach) {
        return Display {
            action: Action::Attach,
            verbs: None,
            target: format!("{id} \u{b7} {reason}"),
            output: OutputKind::Text,
            view: Some(view(call)),
            title,
        };
    }
    Display {
        action: action(
            call.id,
            call.signal,
            call.writes(),
            call.wait_ms > 0,
            Some(call.read_kind()),
        ),
        verbs: None,
        target: call.id.map(|id| id.to_string()).unwrap_or_default(),
        output: OutputKind::Text,
        view: Some(view(call)),
        title,
    }
}

pub(crate) fn rejected(input: &Value) -> Display {
    let map = input.as_object();
    let field = |key: &str| map.and_then(|m| m.get(key)).filter(|v| !v.is_null());
    let id = field("id")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n > 0);
    let keys: Vec<Key> = field("keys")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .filter_map(Key::parse)
                .collect()
        })
        .unwrap_or_default();
    let signal = match field("signal").and_then(Value::as_str) {
        Some("interrupt") => Some(SignalKind::Interrupt),
        Some("kill") => Some(SignalKind::Kill),
        _ => None,
    };
    let read = field("read")
        .and_then(Value::as_str)
        .and_then(ReadKind::named);
    let pattern = field("pattern").and_then(Value::as_str).map(str::to_owned);
    let writes = field("input").is_some() || !keys.is_empty();
    let waits = field("wait_ms")
        .and_then(Value::as_u64)
        .is_some_and(|ms| ms > 0);
    let action = match id {
        Some(_) if field("ask_user_to_attach").is_some() => Action::Attach,
        _ => action(id, signal, writes, waits, read),
    };
    Display {
        action,
        verbs: None,
        target: id.map(|id| id.to_string()).unwrap_or_default(),
        output: OutputKind::Text,
        view: Some(View::Process(ProcessView {
            id,
            text: None,
            keys,
            signal,
            read,
            pattern,
        })),
        title: title(
            field("description").and_then(Value::as_str),
            field("input").and_then(Value::as_str),
        ),
    }
}

pub(crate) fn echo_off_line(echo_off: bool, host: &dyn Host) -> Option<String> {
    echo_off.then(|| hidden_line(crate::secret_on(&host.current().capabilities)))
}

pub fn hidden_line(secret: bool) -> String {
    let ask = if secret {
        format!("ask the user for it with {SECRET_TOOL}, which types it into this process.")
    } else {
        "ask the user to type it (process with ask_user_to_attach).".to_owned()
    };
    format!("{HIDDEN_LINE}{ask}")
}

pub(crate) fn allowance_ms(call: &Call) -> u64 {
    let mut limit = call.wait_ms;
    if call.read_kind() == ReadKind::Search {
        limit += SEARCH_ALLOWANCE_MS;
    }
    if call.signal == Some(SignalKind::Kill) {
        limit += KILL_ALLOWANCE_MS;
    }
    limit
}

pub(crate) fn time_limit_ms(input: &Value) -> Option<u64> {
    match input::parse(input) {
        Ok(call) if call.attach.is_some() => None,
        Ok(call) => Some(allowance_ms(&call)),
        Err(_) => Some(0),
    }
}

pub(crate) fn interrupted_result(input: &Value) -> Option<String> {
    let call = input::parse(input).ok()?;
    call.attach.as_ref()?;
    let id = call.id?;
    Some(format!(
        "Not answered: Wisp stopped while the user was asked to type into process {id}."
    ))
}
