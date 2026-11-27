use std::any::Any;

use serde::{Deserialize, Deserializer};
use serde_json::json;
use wisp_plugin_api::host::Host;
use wisp_plugin_api::index::{Entry, Kind};
use wisp_plugin_api::tools::{
    ClientTool, Effect, Prepared, Purpose, Tags, Timeouts, ToolDef, ToolKind, ToolMeta,
};
use wisp_plugin_api::types::{Action, Display, Failure, Outcome, OutputKind, ToolRun, Verbs};

use crate::note::{self, FILE_MAX, Scope};
use crate::store::{self, Store};

pub(super) const NAME: &str = "remember";
pub(super) const CAPABILITY: &str = "memory";
const BAD_INPUT: &str = "Invalid remember input.";

pub(super) fn definition() -> ToolDef {
    let schema = json!({
        "type": "object",
        "properties": {
            "action": {"type": "string", "enum": ["save", "forget"]},
            "scope": {"type": "string", "enum": ["you", "project"]},
            "name": {"type": "string", "description": "A lowercase slug for the note.", "minLength": note::SLUG_MIN, "maxLength": note::SLUG_MAX, "pattern": "^[a-z0-9][a-z0-9-]*$"},
            "fact": {"type": "string", "description": format!("For save, one line up to {} UTF-8 bytes. Omit for forget.", note::FACT_MAX)},
            "body": {"type": "string", "description": format!("Optional Markdown detail for save only; the whole note fits in {} KiB.", FILE_MAX.div_euclid(1024))}
        },
        "required": ["action", "scope", "name"],
        "additionalProperties": false
    });
    ToolDef {
        name: NAME.into(),
        kind: ToolKind::Client(ClientTool {
            description: "Save or forget a note when the user asks. Save a durable fact, not instructions, task state, private details or facts already in the repository. The same name in the same scope replaces the note.".into(),
            input_schema: schema.to_string(),
            meta: ToolMeta {
                effect: Effect::NonIdempotent,
                tags: Tags::default(),
                parallel_safe: false,
                timeouts: Timeouts { default_ms: 20_000, max_ms: Some(20_000) },
                saves_output: false,
                needs_workspace: false,
                ends_reply: false,
                asks_user: false,
                reports_jobs: false,
                writing: None,
                purpose: Purpose::Run,
            },
        }),
        system_line: Some("Use remember to save or forget only on the user's request. Never infer a save. In a worker, suggest the note in your report instead.".into()),
        context_line: None,
    }
}

pub(super) fn fixed_display() -> Display {
    Display {
        action: Action::Other(NAME.into()),
        verbs: Some(Verbs {
            doing: "Updating saved notes".into(),
            done: "Updated saved notes".into(),
            failed: "Couldn't update saved notes".into(),
        }),
        target: String::new(),
        output: OutputKind::None,
        view: None,
        title: None,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Mutation {
    Save,
    Forget,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RememberInput {
    action: Mutation,
    scope: Scope,
    name: String,
    #[serde(default, deserialize_with = "present_text")]
    fact: Option<String>,
    #[serde(default, deserialize_with = "present_text")]
    body: Option<String>,
}

fn present_text<'de, D: Deserializer<'de>>(value: D) -> Result<Option<String>, D::Error> {
    String::deserialize(value).map(Some)
}

impl Prepared for RememberInput {
    fn display(&self) -> Display {
        fixed_display()
    }
    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> {
        self
    }
}

pub(super) fn parse(input: &str) -> Result<Box<dyn Prepared>, String> {
    let input: RememberInput = serde_json::from_str(input).map_err(|_| BAD_INPUT.to_owned())?;
    if !note::slug(&input.name) {
        return Err(BAD_INPUT.to_owned());
    }
    let valid = match input.action {
        Mutation::Save => input.fact.as_deref().is_some_and(|fact| {
            let body = input.body.as_deref().unwrap_or_default();
            note::valid_fact(fact)
                && body.len() <= FILE_MAX as usize
                && note::encode(&input.name, fact, "0001-01-01", body).len() <= FILE_MAX as usize
        }),
        Mutation::Forget => input.fact.is_none() && input.body.is_none(),
    };
    if !valid {
        return Err(BAD_INPUT.to_owned());
    }
    Ok(Box::new(input))
}

fn apply(host: &dyn Host, call: &RememberInput) -> Result<String, &'static str> {
    if !store::is_root(host) {
        return Err(store::WORKER_REFUSAL);
    }
    let RememberInput {
        action,
        scope,
        name,
        fact,
        body,
    } = call;
    match action {
        Mutation::Save => {
            let fact = fact.as_deref().ok_or(BAD_INPUT)?;
            let body = body.as_deref().unwrap_or_default();
            let refused = "Not saved: possible private detail.";
            if !store::safe_name(host, *scope, name) || !store::safe_content(host, fact, body) {
                return Err(refused);
            }
            let day = note::utc_day(host.now()).ok_or("Couldn't date the note.")?;
            let raw = note::encode(name, fact, day, body);
            if !store::safe(host, &raw) {
                return Err(refused);
            }
            let replaced = Store::writing(host, *scope).save(*scope, name, raw.into_bytes())?;
            let done = if replaced { "Replaced." } else { "Saved." };
            Ok(similar(host, *scope, name, fact, done))
        }
        Mutation::Forget => {
            Store::reading(host).forget(*scope, name)?;
            Ok("Forgotten.".into())
        }
    }
}

pub(super) fn run(host: &dyn Host, prepared: Box<dyn Prepared>) -> ToolRun {
    let Ok(call) = prepared.into_any().downcast::<RememberInput>() else {
        return ToolRun::failed_to_start(BAD_INPUT).with_result(BAD_INPUT);
    };
    match apply(host, &call) {
        Ok(result) => ToolRun::completed().with_result(result),
        Err(message) => ToolRun::new(Outcome::Failed(Failure {
            message: message.into(),
            before_start: false,
        }))
        .with_result(message),
    }
}

fn similar(host: &dyn Host, scope: Scope, name: &str, fact: &str, done: &str) -> String {
    let matches = Store::reading(host)
        .scan()
        .into_iter()
        .filter(|note| (note.id.scope, note.id.name.as_str()) != (scope, name))
        .map(|note| Entry {
            name: note.id.scope.key(&note.id.name),
            kind: Kind::Document,
            summary: String::new(),
            text: note.fact,
        });
    let hits = wisp_index::Index::new(matches.collect()).search_above(
        fact,
        Some(Kind::Document),
        Some(3),
        1.0,
    );
    if !hits.is_empty() {
        let names: Vec<_> = hits.iter().map(|hit| hit.name.as_str()).collect();
        let result = format!("{done} Similar notes: {}.", names.join(", "));
        if store::safe(host, &result) {
            return result;
        }
    }
    done.to_owned()
}
