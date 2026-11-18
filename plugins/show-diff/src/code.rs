use serde_json::{Value, json};
use wisp_plugin_api::tools::{ClientTool, ToolDef, ToolKind};
use wisp_plugin_api::types::{Action, CodeView, Display, Fact, OutputKind, View};
use wisp_tool_text::output::cut_chars;

use super::{
    FALLBACK_TARGET_CHARS, SHOWN, Shown, first_line, invalid, limits, line_count, lines_noun, meta,
    object, optional_text, title_of,
};

pub const NAME: &str = "show_code";
pub const SYSTEM_LINE: &str = "You can show the user code to copy with the show_code tool.";
pub const LANGUAGE_CHARS: usize = 40;
pub const BLANK: &str = "nothing to show: code is blank";

const DESCRIPTION: &str = "Show the user code in the chat, with a button that copies it. Use it \
for code the user will copy and use: a command to run, a file to save, a snippet to paste. It \
only displays the code: it runs nothing and changes no files. The user sees it with syntax \
colours, so don't repeat it in your reply. For a line or two inside an explanation, a Markdown \
code block is enough.\n\nGive `language` for syntax colours, such as rust or bash.";

pub fn description() -> String {
    format!("{DESCRIPTION} Limits: {}.", limits())
}

pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "code": {
                "type": "string",
                "description": "The code, exactly as the user should copy it."
            },
            "language": {
                "type": "string",
                "description": "The language for syntax colours, such as rust or bash."
            },
            "title": {
                "type": "string",
                "description": "A short heading, such as what the code does."
            }
        },
        "required": ["code"],
        "additionalProperties": false
    })
}

fn without_trailing_breaks(code: &str) -> &str {
    let mut code = code;
    while let Some(rest) = code.strip_suffix('\n') {
        code = rest.strip_suffix('\r').unwrap_or(rest);
    }
    code
}

pub fn show(input: &Value) -> Result<Shown, String> {
    let map = object(input, "input")?;
    if let Some(key) = map
        .keys()
        .find(|k| !["code", "language", "title"].contains(&k.as_str()))
    {
        return Err(invalid(format!("unknown field `{key}`")));
    }
    let code = optional_text(map, "code", "")?.ok_or_else(|| invalid("`code` is required"))?;
    let language = optional_text(map, "language", "")?
        .and_then(|l| first_line(&l))
        .map(|l| cut_chars(&l, LANGUAGE_CHARS));
    optional_text(map, "title", "")?;
    let code = without_trailing_breaks(&code);
    if code.trim().is_empty() {
        return Err(invalid(BLANK));
    }
    let lines = line_count(code);
    wisp_tool_text::code::check(code)?;
    let noun = lines_noun(u32::try_from(lines).unwrap_or(u32::MAX)).0;
    let result = match &language {
        Some(language) => format!("{SHOWN}{lines} {noun} of {language}."),
        None => format!("{SHOWN}{lines} {noun}."),
    };
    let display = Display {
        view: Some(View::Code(CodeView {
            language,
            code: code.to_owned(),
        })),
        ..rejected(input)
    };
    Ok(Shown {
        display,
        result,
        facts: vec![Fact::Lines(lines as u64)],
    })
}

pub fn rejected(input: &Value) -> Display {
    let title = input
        .get("title")
        .and_then(Value::as_str)
        .and_then(title_of);
    let target = title.clone().unwrap_or_else(|| "code".to_owned());
    Display {
        action: Action::Show,
        verbs: None,
        target: cut_chars(&target, FALLBACK_TARGET_CHARS),
        output: OutputKind::None,
        view: None,
        title,
    }
}

pub fn def() -> ToolDef {
    ToolDef {
        name: NAME.to_owned(),
        kind: ToolKind::Client(ClientTool {
            description: description(),
            input_schema: schema().to_string(),
            meta: meta("code"),
        }),
        system_line: Some(SYSTEM_LINE.to_owned()),
        context_line: None,
    }
}
