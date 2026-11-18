use regex::{Regex, RegexBuilder};
use serde_json::{Map, Value};
use wisp_plugin_api::host::Size;
use wisp_plugin_api::types::{Key, ReadKind, SignalKind};
use wisp_tool_text::jobs::no_process;
use wisp_tool_text::output::group;
use wisp_tool_text::terminal::{
    MAX_COLS, MAX_ROWS, MIN_COLS, MIN_ROWS, key_names, parse_size, size_allowed,
};

pub const MAX_INPUT: usize = 64 * 1024;
pub const MAX_WAIT_MS: u64 = 600_000;
pub const MAX_CONTEXT: u64 = 10;
pub const MAX_MATCHES: u64 = 500;
pub(crate) const REGEX_LIMIT: usize = 1024 * 1024;
pub const TAIL_LINES: u64 = 50;
pub const SEARCH_LINES: u64 = 50;
pub const RANGE_LINES: u64 = 100;
pub const INPUT_WAIT_MS: u64 = 30_000;
pub const INPUT_QUIET_MS: u64 = 1_000;
pub const INTERRUPT_WAIT_MS: u64 = 2_000;
pub const INTERRUPT_QUIET_MS: u64 = 500;
pub const NO_ID: &str = "give `id` to act on a process; without it the call lists this session's processes and takes nothing but read and description";
pub const UNTIL_IGNORED: &str = "until and quiet_ms only apply while waiting; give wait_ms.";

pub const MAX_ATTACH_REASON: usize = 200;
pub const ATTACH_ALONE: &str = "only `read` and `description` may accompany `ask_user_to_attach`";

const FIELDS: [&str; 16] = [
    "ask_user_to_attach",
    "context",
    "description",
    "from",
    "id",
    "input",
    "keys",
    "lines",
    "pattern",
    "quiet_ms",
    "read",
    "signal",
    "size",
    "timeout_ms",
    "until",
    "wait_ms",
];

#[derive(Debug, Clone)]
pub(crate) struct Call {
    pub id: Option<u32>,
    pub input: Option<String>,
    pub keys: Vec<Key>,
    pub size: Option<Size>,
    pub timeout_ms: Option<u64>,
    pub signal: Option<SignalKind>,
    pub wait_ms: u64,
    pub quiet_ms: Option<u64>,
    pub until: Option<(Regex, String)>,
    pub read: Option<ReadKind>,
    pub pattern: Option<(Regex, String)>,
    pub from: Option<u64>,
    pub lines: Option<u64>,
    pub context: u64,
    pub notes: Vec<String>,
    pub attach: Option<String>,
    pub description: Option<String>,
}

impl Call {
    fn list(description: Option<String>) -> Self {
        Self {
            id: None,
            input: None,
            keys: Vec::new(),
            size: None,
            timeout_ms: None,
            signal: None,
            wait_ms: 0,
            quiet_ms: None,
            until: None,
            read: None,
            pattern: None,
            from: None,
            lines: None,
            context: 0,
            notes: Vec::new(),
            attach: None,
            description,
        }
    }

    pub(crate) fn writes(&self) -> bool {
        self.input.is_some() || !self.keys.is_empty()
    }

    pub(crate) fn changes(&self) -> bool {
        self.writes() || self.signal.is_some() || self.timeout_ms.is_some() || self.size.is_some()
    }

    pub(crate) fn read_kind(&self) -> ReadKind {
        self.read.unwrap_or(ReadKind::New)
    }
}

fn whole(map: &Map<String, Value>, key: &str) -> Result<Option<i128>, String> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => match (value.as_u64(), value.as_i64()) {
            (Some(n), _) => Ok(Some(i128::from(n))),
            (None, Some(n)) => Ok(Some(i128::from(n))),
            (None, None) => Err(err(format!("`{key}` must be a whole number"))),
        },
    }
}

fn text(map: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(err(format!("`{key}` must be a string"))),
    }
}

fn clamped(
    map: &Map<String, Value>,
    key: &str,
    low: u64,
    high: u64,
    notes: &mut Vec<String>,
) -> Result<Option<u64>, String> {
    let Some(asked) = whole(map, key)? else {
        return Ok(None);
    };
    let used = asked.clamp(i128::from(low), i128::from(high));
    if used != asked {
        notes.push(format!(
            "{key} {asked} is outside {} to {}, so {} was used.",
            group(low),
            group(high),
            group(u64::try_from(used).unwrap_or(high))
        ));
    }
    Ok(Some(u64::try_from(used).unwrap_or(high)))
}

pub(crate) fn compile(pattern: &str, key: &str) -> Result<Regex, String> {
    RegexBuilder::new(pattern)
        .size_limit(REGEX_LIMIT)
        .build()
        .map_err(|e| err(format!("`{key}` is not a valid regular expression: {e}")))
}

fn read_kind(map: &Map<String, Value>) -> Result<Option<ReadKind>, String> {
    match text(map, "read")? {
        None => Ok(None),
        Some(asked) => ReadKind::named(&asked).map(Some).ok_or_else(|| {
            err(format!(
                "`read` must be new, tail, lines, search or screen, not `{asked}`"
            ))
        }),
    }
}

fn keys(map: &Map<String, Value>) -> Result<Vec<Key>, String> {
    let Some(value) = map.get("keys").filter(|v| !v.is_null()) else {
        return Ok(Vec::new());
    };
    let Some(items) = value.as_array() else {
        return Err(err("`keys` must be a list of key names"));
    };
    let mut keys = Vec::new();
    for item in items {
        let Some(name) = item.as_str() else {
            return Err(err("`keys` must be a list of key names"));
        };
        let Some(key) = Key::parse(name) else {
            return Err(err(format!(
                "unknown key `{name}`; the keys are {}",
                key_names()
            )));
        };
        keys.push(key);
    }
    if let Some(at) = keys.iter().position(|k| *k == Key::Eof)
        && at + 1 != keys.len()
    {
        return Err(err("EOF closes stdin, so it must be the last key"));
    }
    Ok(keys)
}

fn size(map: &Map<String, Value>) -> Result<Option<Size>, String> {
    let Some(asked) = text(map, "size")? else {
        return Ok(None);
    };
    let Some(size) = parse_size(&asked) else {
        return Err(err(format!(
            "`size` must be COLUMNSxROWS, such as 120x40, not `{asked}`"
        )));
    };
    if !size_allowed(size) {
        return Err(err(format!(
            "`size` must be from {MIN_COLS}x{MIN_ROWS} to {MAX_COLS}x{MAX_ROWS}, not {asked}"
        )));
    }
    Ok(Some(size))
}

fn regex(map: &Map<String, Value>, key: &str) -> Result<Option<(Regex, String)>, String> {
    text(map, key)?
        .map(|source| compile(&source, key).map(|re| (re, source)))
        .transpose()
}

pub(crate) fn parse(input: &Value) -> Result<Call, String> {
    let map = input
        .as_object()
        .ok_or_else(|| err("input must be an object"))?;
    if let Some(key) = map.keys().find(|k| !FIELDS.contains(&k.as_str())) {
        return Err(err(format!("unknown field `{key}`")));
    }
    let description = text(map, "description")?
        .map(|given| given.trim().to_owned())
        .filter(|given| !given.is_empty());
    if map.get("id").is_none_or(Value::is_null) {
        if map
            .keys()
            .any(|k| !matches!(k.as_str(), "read" | "id" | "description"))
        {
            return Err(err(NO_ID));
        }
        read_kind(map)?;
        return Ok(Call::list(description));
    }
    let mut notes = Vec::new();
    let attach = attach_reason(map)?;
    let id = match whole(map, "id")? {
        None => None,
        Some(n) => Some(
            u32::try_from(n)
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| err(no_process(n)))?,
        ),
    };
    let input_text = text(map, "input")?;
    if let Some(text) = &input_text
        && text.len() > MAX_INPUT
    {
        return Err(err(format!(
            "`input` is {} bytes; the limit is {} bytes",
            group(text.len() as u64),
            group(MAX_INPUT as u64)
        )));
    }
    let keys = keys(map)?;
    let size = size(map)?;
    let timeout_ms = match whole(map, "timeout_ms")? {
        None => None,
        Some(n) if n < 0 => {
            return Err(err("`timeout_ms` must be 0 or more milliseconds"));
        }
        Some(n) => Some(u64::try_from(n).unwrap_or(u64::MAX)),
    };
    let signal = match text(map, "signal")?.as_deref() {
        None => None,
        Some("interrupt") => Some(SignalKind::Interrupt),
        Some("kill") => Some(SignalKind::Kill),
        Some(other) => {
            return Err(err(format!(
                "`signal` must be interrupt or kill, not `{other}`"
            )));
        }
    };
    let read = read_kind(map)?;
    let pattern = regex(map, "pattern")?;
    let until = regex(map, "until")?;
    let from = match whole(map, "from")? {
        None => None,
        Some(n) if n < 1 => return Err(err("`from` must be 1 or more")),
        Some(n) => Some(u64::try_from(n).unwrap_or(u64::MAX)),
    };
    let searching = read == Some(ReadKind::Search);
    if from.is_some() && !matches!(read, Some(ReadKind::Lines | ReadKind::Search)) {
        return Err(err("`from` only applies to read lines or read search"));
    }
    if searching && pattern.is_none() {
        return Err(err("read search needs a `pattern`"));
    }
    if !searching && pattern.is_some() {
        return Err(err("`pattern` only applies to read search"));
    }
    if map.contains_key("context") && !searching {
        return Err(err("`context` only applies to read search"));
    }
    let context = clamped(map, "context", 0, MAX_CONTEXT, &mut notes)?.unwrap_or(0);
    let lines = if searching {
        clamped(map, "lines", 1, MAX_MATCHES, &mut notes)?
    } else {
        match whole(map, "lines")? {
            None => None,
            Some(_) if !matches!(read, Some(ReadKind::Tail | ReadKind::Lines)) => {
                return Err(err("`lines` only applies to read tail, lines or search"));
            }
            Some(n) if n < 1 => return Err(err("`lines` must be 1 or more")),
            Some(n) => Some(u64::try_from(n).unwrap_or(u64::MAX)),
        }
    };
    let wait_given = clamped(map, "wait_ms", 0, MAX_WAIT_MS, &mut notes)?;
    let quiet_given = clamped(map, "quiet_ms", 0, MAX_WAIT_MS, &mut notes)?;
    let writes = input_text.is_some() || !keys.is_empty();
    let (wait_default, quiet_default) = match signal {
        Some(SignalKind::Interrupt) => (INTERRUPT_WAIT_MS, Some(INTERRUPT_QUIET_MS)),
        _ if writes => (INPUT_WAIT_MS, Some(INPUT_QUIET_MS)),
        _ => (0, None),
    };
    let wait_ms = wait_given.unwrap_or(wait_default);
    let (until, quiet_ms) = if wait_ms == 0 && (until.is_some() || quiet_given.is_some()) {
        notes.push(UNTIL_IGNORED.to_owned());
        (None, None)
    } else {
        (until, quiet_given.or(quiet_default))
    };
    let call = Call {
        id,
        input: input_text,
        keys,
        size,
        timeout_ms,
        signal,
        wait_ms,
        quiet_ms,
        until,
        read,
        pattern,
        from,
        lines,
        context,
        notes,
        attach,
        description,
    };
    Ok(call)
}

fn attach_reason(map: &Map<String, Value>) -> Result<Option<String>, String> {
    let Some(reason) = text(map, "ask_user_to_attach")? else {
        return Ok(None);
    };
    if map.iter().any(|(k, v)| {
        !matches!(
            k.as_str(),
            "id" | "read" | "ask_user_to_attach" | "description"
        ) && !v.is_null()
    }) {
        return Err(err(ATTACH_ALONE));
    }
    if matches!(read_kind(map)?, Some(ReadKind::Search)) {
        return Err(err(ATTACH_ALONE));
    }
    let reason = reason.trim();
    let chars = reason.chars().count();
    if chars == 0 || chars > MAX_ATTACH_REASON || reason.contains(['\n', '\r']) {
        return Err(err(format!(
            "`ask_user_to_attach` must be a reason of 1 to {MAX_ATTACH_REASON} characters on one line"
        )));
    }
    Ok(Some(reason.to_owned()))
}

fn err(message: impl Into<String>) -> String {
    message.into()
}
