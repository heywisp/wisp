use std::any::Any;
use std::sync::Arc;

use serde_json::{Map, Value, json};
use wisp_plugin_api::host::{CallContext, Host};
use wisp_plugin_api::tools::{
    BuildContext, ClientTool, Effect, Prepared, Purpose, Tags, Timeouts, ToolDef, ToolKind,
    ToolMeta,
};
use wisp_plugin_api::types::{
    Action, AskReply, Display, Failure, Outcome, OutputKind, Question, QuestionOption, ToolRun,
};
use wisp_plugin_api::{BoxFuture, Bundled, Plugin};
use wisp_tool_text::output::cut_chars;
use wisp_tool_text::question::{
    MESSAGE_INSTEAD, STOPPED, UNAVAILABLE, UNAVAILABLE_MESSAGE, answered_text,
};

pub const MANIFEST: &str = include_str!("../wisp-plugin.toml");

pub fn bundled() -> Bundled {
    Bundled {
        manifest: MANIFEST,
        instantiate,
    }
}

fn instantiate(_host: Arc<dyn Host>) -> Arc<dyn Plugin> {
    Arc::new(AskUser)
}

fn invalid(message: impl Into<String>) -> String {
    message.into()
}

pub const NAME: &str = "ask_user";
pub const MIN_QUESTIONS: usize = 1;
pub const MAX_QUESTIONS: usize = 4;
pub const MIN_OPTIONS: usize = 2;
pub const MAX_OPTIONS: usize = 4;
pub const HEADER_CHARS: usize = 12;

pub const TOO_FEW_OPTIONS: &str = "A question needs at least two options. If only one choice makes sense, don't ask: take it and say so.";

const DESCRIPTION: &str = "Ask the user one to four multiple-choice questions and wait for the answers. Use it only when you are blocked on a decision that is genuinely the user's to make: one you can't settle from the request, the code or a sensible default, such as choosing between approaches or settling an ambiguous requirement. Don't use it for facts you can find out yourself, to ask permission or whether to proceed, or for choices with a conventional default: take the obvious one, say so and go on. Ask everything you need in one call, as the last call of your reply: calls after it in the same reply are not run. The user may see only your questions, not the text before the call, so each must stand alone: give the facts needed to answer it and never refer to \"above\". Give each question two to four distinct options; put the one you recommend first and end its label with \"(Recommended)\". The user can always type their own answer, so never add an \"Other\" option. Read the answers carefully and follow what they actually say; if the user skipped a question, decide yourself and say what you assumed.";

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
            }
        },
        "required": ["questions"],
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

pub fn parse(input: &Value) -> Result<Vec<Question>, String> {
    let map = object(input, "input")?;
    only_fields(map, &["questions"], "")?;
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
    Ok(questions)
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

pub fn finish(questions: &[Question], reply: Option<AskReply>) -> ToolRun {
    match reply {
        None => ToolRun::new(Outcome::Cancelled).with_result(STOPPED),
        Some(AskReply::Answered(answers)) => {
            ToolRun::completed().with_result(answered_text(questions, &answers))
        }
        Some(AskReply::MessageInstead) => ToolRun::completed().with_result(MESSAGE_INSTEAD),
        Some(AskReply::Stopped) => ToolRun::new(Outcome::Cancelled).with_result(STOPPED),
        Some(
            AskReply::Unavailable
            | AskReply::Attached(_)
            | AskReply::Declined
            | AskReply::ProcessEnded(_),
        ) => ToolRun::new(Outcome::Failed(Failure {
            message: UNAVAILABLE_MESSAGE.to_owned(),
            before_start: false,
        }))
        .with_result(UNAVAILABLE),
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
            description: DESCRIPTION.to_owned(),
            input_schema: schema().to_string(),
            meta: meta(),
        }),
        system_line: None,
        context_line: None,
    }
}

pub struct AskUser;

struct Asking(Vec<Question>);

impl Prepared for Asking {
    fn display(&self) -> Display {
        display(&self.0)
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> {
        Box::new(self.0)
    }
}

pub fn parse_text(input: &str) -> Result<Vec<Question>, String> {
    let value: Value =
        serde_json::from_str(input).map_err(|e| format!("input is not JSON: {e}"))?;
    parse(&value)
}

async fn ask(questions: Vec<Question>, ctx: &dyn CallContext) -> ToolRun {
    let asked = futures_select(ctx.wait_cancelled(), ctx.ask(questions.clone())).await;
    finish(&questions, asked)
}

async fn futures_select(
    cancelled: BoxFuture<'_, ()>,
    asked: BoxFuture<'_, AskReply>,
) -> Option<AskReply> {
    Racing { cancelled, asked }.await
}

struct Racing<'a> {
    cancelled: BoxFuture<'a, ()>,
    asked: BoxFuture<'a, AskReply>,
}

impl std::future::Future for Racing<'_> {
    type Output = Option<AskReply>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<AskReply>> {
        if self.cancelled.as_mut().poll(cx).is_ready() {
            return std::task::Poll::Ready(None);
        }
        match self.asked.as_mut().poll(cx) {
            std::task::Poll::Ready(reply) => std::task::Poll::Ready(Some(reply)),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
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
            match call.into_any().downcast::<Vec<Question>>() {
                Ok(questions) => ask(*questions, ctx).await,
                Err(_) => ToolRun::failed_to_start(wisp_tool_text::output::WRONG_INPUT),
            }
        })
    }
}

#[cfg(test)]
mod tests;
