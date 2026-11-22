use std::any::Any;
use std::sync::Arc;

use serde_json::{Map, Value, json};
use wisp_plugin_api::host::{CallContext, Host};
use wisp_plugin_api::questions::Posted;
use wisp_plugin_api::tools::{
    BuildContext, ClientTool, Effect, Prepared, Purpose, Tags, Timeouts, ToolDef, ToolKind,
    ToolMeta,
};
use wisp_plugin_api::types::{
    Action, Display, Failure, Outcome, OutputKind, Question, QuestionOption, ToolRun, Verbs,
};
use wisp_plugin_api::{BoxFuture, Bundled, Plugin};
use wisp_tool_text::output::cut_chars;
use wisp_tool_text::question::{
    BUSY_MESSAGE, NOTHING_OPEN, NOTHING_OPEN_MESSAGE, QUESTION_TOOL, UNAVAILABLE,
    UNAVAILABLE_MESSAGE, WITHDRAW_FIELD, WITHDRAWN, busy, posted,
};

pub const MANIFEST: &str = include_str!("../wisp-plugin.toml");

pub fn bundled() -> Bundled {
    Bundled {
        manifest: MANIFEST,
        instantiate,
    }
}

fn instantiate(host: Arc<dyn Host>) -> Arc<dyn Plugin> {
    Arc::new(AskUser { host })
}

fn invalid(message: impl Into<String>) -> String {
    message.into()
}

pub const NAME: &str = QUESTION_TOOL;
pub const MIN_QUESTIONS: usize = 1;
pub const MAX_QUESTIONS: usize = 4;
pub const MIN_OPTIONS: usize = 2;
pub const MAX_OPTIONS: usize = 4;
pub const HEADER_CHARS: usize = 12;

pub const TOO_FEW_OPTIONS: &str = "A question needs at least two options. If only one choice makes sense, don't ask: take it and say so.";

pub fn description() -> String {
    format!(
        "Ask the user one to four multiple-choice questions. Asking doesn't wait for the answers: the call returns at once, the questions stay on the user's screen, and the answers arrive later in a message of their own, whenever the user replies. Use it only for a decision that is genuinely the user's to make: one you can't settle from the request, the code or a sensible default, such as choosing between approaches or settling an ambiguous requirement. Don't use it for facts you can find out yourself, to ask permission or whether to proceed, or for choices with a conventional default: take the obvious one, say so and go on. Ask everything you need in one call, as the last call of your reply: calls after it in the same reply are not run. Only one set of questions is open at a time; while it is, go on with any work the answers don't decide, and end your turn when nothing is left. The user may see only your questions, not the text before the call, so each must stand alone: give the facts needed to answer it and never refer to \"above\". Give each question two to four distinct options; put the one you recommend first and end its label with \"(Recommended)\". The user can always type their own answer, so never add an \"Other\" option. If the questions stop mattering, or you decide yourself, withdraw them by calling {QUESTION_TOOL} with {WITHDRAW_FIELD} set to true, and say what you decided. Read the answers carefully and follow what they actually say; if the user skipped a question, decide yourself and say what you assumed."
    )
}

pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "questions": {
                "type": "array",
                "description": "The questions, asked together.",
                "minItems": MIN_QUESTIONS,
                "maxItems": MAX_QUESTIONS,
                "items": {
                    "type": "object",
                    "properties": {
                        "header": {
                            "type": "string",
                            "description": "A short tag shown above the question, at most 12 characters, such as \"Database\"."
                        },
                        "question": {
                            "type": "string",
                            "description": "The whole question, ending with a question mark."
                        },
                        "options": {
                            "type": "array",
                            "description": "The choices.",
                            "minItems": MIN_OPTIONS,
                            "maxItems": MAX_OPTIONS,
                            "items": {
                                "type": "object",
                                "properties": {
                                    "label": {
                                        "type": "string",
                                        "description": "The choice, in one to five words."
                                    },
                                    "description": {
                                        "type": "string",
                                        "description": "What choosing this means, in one sentence."
                                    }
                                },
                                "required": ["description", "label"],
                                "additionalProperties": false
                            }
                        },
                        "multi_select": {
                            "type": "boolean",
                            "description": "Let the user choose more than one option. Defaults to false."
                        }
                    },
                    "required": ["header", "options", "question"],
                    "additionalProperties": false
                }
            },
            WITHDRAW_FIELD: {
                "type": "boolean",
                "description": "Set to true, with no other field, to withdraw your open questions instead of asking: the user no longer sees them."
            }
        },
        "additionalProperties": false
    })
}

pub fn cut_header(header: &str) -> String {
    cut_chars(header, HEADER_CHARS)
}

fn object<'a>(value: &'a Value, what: &str) -> Result<&'a Map<String, Value>, String> {
    value
        .as_object()
        .ok_or_else(|| invalid(format!("{what} must be an object")))
}

fn only_fields(map: &Map<String, Value>, known: &[&str], within: &str) -> Result<(), String> {
    match map.keys().find(|k| !known.contains(&k.as_str())) {
        Some(key) => Err(invalid(format!("unknown field `{key}`{within}"))),
        None => Ok(()),
    }
}

fn text(
    map: &Map<String, Value>,
    field: &str,
    owner: &str,
    blank_ok: bool,
) -> Result<String, String> {
    let value = map
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(format!("{owner}'s `{field}` must be a string")))?;
    if !blank_ok && value.trim().is_empty() {
        return Err(invalid(format!("{owner}'s `{field}` is blank")));
    }
    Ok(value.to_owned())
}

fn parse_option(value: &Value, n: usize, m: usize) -> Result<QuestionOption, String> {
    let owner = format!("option {m} of question {n}");
    let map = object(value, &owner)?;
    only_fields(map, &["description", "label"], &format!(" in {owner}"))?;
    Ok(QuestionOption {
        label: text(map, "label", &owner, false)?,
        description: text(map, "description", &owner, true)?,
    })
}

fn parse_question(value: &Value, n: usize) -> Result<Question, String> {
    let owner = format!("question {n}");
    let map = object(value, &owner)?;
    only_fields(
        map,
        &["header", "multi_select", "options", "question"],
        &format!(" in {owner}"),
    )?;
    let header = text(map, "header", &owner, false)?;
    let question = text(map, "question", &owner, false)?;
    let multi_select = match map.get("multi_select") {
        None => false,
        Some(value) => value
            .as_bool()
            .ok_or_else(|| invalid(format!("{owner}'s `multi_select` must be true or false")))?,
    };
    let items = map
        .get("options")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid(format!("{owner}'s `options` must be an array")))?;
    if items.len() < MIN_OPTIONS {
        return Err(invalid(TOO_FEW_OPTIONS));
    }
    if items.len() > MAX_OPTIONS {
        return Err(invalid(format!(
            "{owner} has {} options; give two to four",
            items.len()
        )));
    }
    let mut options: Vec<QuestionOption> = Vec::with_capacity(items.len());
    for (m, item) in items.iter().enumerate() {
        let option = parse_option(item, n, m + 1)?;
        if let Some(first) = options.iter().position(|o| o.label == option.label) {
            return Err(invalid(format!(
                "options {} and {} of {owner} have the same label; labels must differ",
                first + 1,
                m + 1
            )));
        }
        options.push(option);
    }
    Ok(Question {
        header,
        question,
        options,
        multi_select,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    Ask(Vec<Question>),
    Withdraw,
}

pub fn parse(input: &Value) -> Result<Call, String> {
    let map = object(input, "input")?;
    only_fields(map, &["questions", WITHDRAW_FIELD], "")?;
    let has_questions = map.contains_key("questions");
    let withdraw = map.get(WITHDRAW_FIELD);
    if has_questions && withdraw.is_some() {
        return Err(invalid(format!(
            "give either questions or {WITHDRAW_FIELD}, not both"
        )));
    }
    if let Some(value) = withdraw {
        if value.as_bool() == Some(true) {
            return Ok(Call::Withdraw);
        }
        return Err(invalid(format!(
            "`{WITHDRAW_FIELD}` must be true; to ask, give `questions` instead"
        )));
    }
    let items = map
        .get("questions")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("`questions` must be an array"))?;
    if items.len() < MIN_QUESTIONS || items.len() > MAX_QUESTIONS {
        return Err(invalid(format!(
            "ask one to four questions in one call, not {}",
            items.len()
        )));
    }
    let mut questions: Vec<Question> = Vec::with_capacity(items.len());
    for (n, item) in items.iter().enumerate() {
        let question = parse_question(item, n + 1)?;
        if let Some(first) = questions
            .iter()
            .position(|q| q.question == question.question)
        {
            return Err(invalid(format!(
                "questions {} and {} are the same; each question must differ",
                first + 1,
                n + 1
            )));
        }
        questions.push(question);
    }
    Ok(Call::Ask(questions))
}

pub fn display(questions: &[Question]) -> Display {
    let first = questions.first().map(|q| cut_header(&q.header));
    let target = match (first, questions.len()) {
        (None, _) => String::new(),
        (Some(header), 1) => header,
        (Some(header), n) => format!("{header} (+{})", n - 1),
    };
    Display {
        action: Action::Other(NAME.to_owned()),
        verbs: None,
        target,
        output: OutputKind::None,
        view: None,
        title: None,
    }
}

pub fn withdraw_display() -> Display {
    Display {
        action: Action::Other(NAME.to_owned()),
        verbs: Some(Verbs {
            doing: "Withdrawing".to_owned(),
            done: "Withdrew".to_owned(),
            failed: "couldn't withdraw".to_owned(),
        }),
        target: "the question".to_owned(),
        output: OutputKind::None,
        view: None,
        title: None,
    }
}

pub fn meta() -> ToolMeta {
    ToolMeta {
        effect: Effect::ReadOnly,
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
        writing: Some("a question".to_owned()),
        purpose: Purpose::Run,
    }
}

pub fn def() -> ToolDef {
    ToolDef {
        name: NAME.to_owned(),
        kind: ToolKind::Client(ClientTool {
            description: description(),
            input_schema: schema().to_string(),
            meta: meta(),
        }),
        system_line: None,
        context_line: None,
    }
}

pub struct AskUser {
    host: Arc<dyn Host>,
}

impl AskUser {
    pub fn new(host: Arc<dyn Host>) -> Self {
        Self { host }
    }
}

struct PreparedCall(Call);

impl Prepared for PreparedCall {
    fn display(&self) -> Display {
        match &self.0 {
            Call::Ask(questions) => display(questions),
            Call::Withdraw => withdraw_display(),
        }
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> {
        Box::new(self.0)
    }
}

pub fn parse_text(input: &str) -> Result<Call, String> {
    let value: Value =
        serde_json::from_str(input).map_err(|e| format!("input is not JSON: {e}"))?;
    parse(&value)
}

impl Plugin for AskUser {
    fn describe<'a>(&'a self, _ctx: &'a BuildContext) -> BoxFuture<'a, Vec<ToolDef>> {
        Box::pin(async { vec![def()] })
    }

    fn prepare<'a>(
        &'a self,
        _tool: &'a str,
        input: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Prepared>, String>> {
        Box::pin(async move {
            let call: Box<dyn Prepared> = Box::new(PreparedCall(parse_text(input)?));
            Ok(call)
        })
    }

    fn run<'a>(
        &'a self,
        call: Box<dyn Prepared>,
        ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, ToolRun> {
        Box::pin(async move {
            let Ok(call) = call.into_any().downcast::<Call>() else {
                return ToolRun::failed_to_start(wisp_tool_text::output::WRONG_INPUT);
            };
            match *call {
                Call::Ask(questions) => match self.host.questions().post(questions, ctx).await {
                    Posted::Open => ToolRun::completed().with_result(posted()),
                    Posted::Busy => ToolRun::new(Outcome::Failed(Failure {
                        message: BUSY_MESSAGE.to_owned(),
                        before_start: false,
                    }))
                    .with_result(busy()),
                    Posted::Unavailable => ToolRun::new(Outcome::Failed(Failure {
                        message: UNAVAILABLE_MESSAGE.to_owned(),
                        before_start: false,
                    }))
                    .with_result(UNAVAILABLE),
                },
                Call::Withdraw => {
                    if self.host.questions().withdraw(ctx).await {
                        ToolRun::completed().with_result(WITHDRAWN)
                    } else {
                        ToolRun::new(Outcome::Failed(Failure {
                            message: NOTHING_OPEN_MESSAGE.to_owned(),
                            before_start: false,
                        }))
                        .with_result(NOTHING_OPEN)
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests;
