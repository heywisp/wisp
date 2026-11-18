use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rmcp::model::{ElicitRequestParams, ElicitResult, ElicitationAction};
use serde_json::{Map, Value};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use wisp_protocol::{
    Answer, AskedBy, Ends, Question, QuestionOption, QuestionSource, SecretDestination,
};
use wisp_tool_text::{manage, mcp as text};

use crate::call::user::{AskReply, Goes, SecretReply, UserChannel};
use crate::session::Redactions;

pub(super) type Flights = Arc<Mutex<Vec<Arc<Flight>>>>;

pub(super) struct Flight {
    pub user: UserChannel,
    pub server: String,
    pub cancel: CancellationToken,
    pub notes: Mutex<Vec<String>>,
    pub redactions: Mutex<Redactions>,
    pub received_secret: AtomicBool,
    pub failure: Mutex<Option<String>>,
    pub rounds: std::sync::atomic::AtomicUsize,
    clock: Mutex<Clock>,
    changed: tokio::sync::Notify,
}

struct Clock {
    spent: Duration,
    since: Instant,
    asking: usize,
}

pub(super) struct Registered(pub Arc<Flight>, Flights);

impl Drop for Registered {
    fn drop(&mut self) {
        self.0.cancel.cancel();
        self.1
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|flight| !Arc::ptr_eq(flight, &self.0));
    }
}

struct Pause<'a>(&'a Flight);

impl Drop for Pause<'_> {
    fn drop(&mut self) {
        let mut clock = self.0.clock.lock().unwrap_or_else(PoisonError::into_inner);
        clock.asking -= 1;
        if clock.asking == 0 {
            clock.since = Instant::now();
        }
        self.0.changed.notify_one();
    }
}

impl Flight {
    pub(super) fn register(
        flights: Flights,
        server: String,
        user: UserChannel,
        cancel: CancellationToken,
        redactions: Redactions,
    ) -> Registered {
        let flight = Arc::new(Self {
            user,
            server,
            cancel: cancel.child_token(),
            notes: Mutex::default(),
            redactions: Mutex::new(redactions),
            received_secret: AtomicBool::new(false),
            failure: Mutex::default(),
            rounds: std::sync::atomic::AtomicUsize::new(0),
            clock: Mutex::new(Clock {
                spent: Duration::ZERO,
                since: Instant::now(),
                asking: 0,
            }),
            changed: tokio::sync::Notify::new(),
        });
        flights
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(flight.clone());
        Registered(flight, flights)
    }

    fn pause(&self) -> Pause<'_> {
        let mut clock = self.clock.lock().unwrap_or_else(PoisonError::into_inner);
        if clock.asking == 0 {
            let elapsed = clock.since.elapsed();
            clock.spent += elapsed;
        }
        clock.asking += 1;
        self.changed.notify_one();
        Pause(self)
    }

    pub(super) async fn timed_out(&self, limit: Duration) {
        loop {
            let changed = self.changed.notified();
            let (spent, asking) = {
                let clock = self.clock.lock().unwrap_or_else(PoisonError::into_inner);
                (
                    clock.spent
                        + if clock.asking == 0 {
                            clock.since.elapsed()
                        } else {
                            Duration::ZERO
                        },
                    clock.asking != 0,
                )
            };
            if spent >= limit {
                return;
            }
            tokio::select! {
                () = changed => {},
                () = tokio::time::sleep(limit - spent), if !asking => {},
            }
        }
    }

    pub(super) fn scrub(&self, text: &str) -> String {
        self.redactions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .scrub(text)
    }
}

struct Field {
    name: String,
    title: String,
    description: Option<String>,
    kind: Kind,
    secret: bool,
}

enum Kind {
    Text,
    Number,
    Choices {
        labels: Vec<String>,
        values: Vec<Value>,
        multi: bool,
    },
}

fn secret_word(text: &str) -> bool {
    wisp_protocol::settings::holds_secret_word(text)
        || ["passphrase", "credential"]
            .iter()
            .any(|word| text.to_lowercase().contains(word))
}

fn short(text: &str, max: usize) -> bool {
    text.chars().count() <= max
}

fn fields(schema: &rmcp::model::ElicitationSchema) -> Option<Vec<Field>> {
    if schema.properties.len() > text::MAX_FORM_FIELDS {
        return None;
    }
    schema
        .properties
        .iter()
        .map(|(name, schema)| {
            let value = serde_json::to_value(schema).ok()?;
            let title = value["title"].as_str().unwrap_or(name);
            let description = value["description"].as_str();
            if !short(name, text::MAX_FORM_LABEL_CHARS)
                || !short(title, text::MAX_FORM_LABEL_CHARS)
                || description.is_some_and(|text| !short(text, text::MAX_FORM_DESCRIPTION_CHARS))
            {
                return None;
            }
            let kind = match value["type"].as_str()? {
                "number" | "integer" => Kind::Number,
                "boolean" => Kind::Choices {
                    labels: vec![text::YES.into(), text::NO.into()],
                    values: vec![Value::Bool(true), Value::Bool(false)],
                    multi: false,
                },
                _ => {
                    let multi = value["type"] == "array";
                    let choices = if multi { &value["items"] } else { &value };
                    if let Some(values) = choices["enum"].as_array() {
                        let labels = values
                            .iter()
                            .enumerate()
                            .map(|(i, v)| {
                                value["enumNames"]
                                    .get(i)
                                    .and_then(Value::as_str)
                                    .or_else(|| v.as_str())
                                    .map(str::to_owned)
                            })
                            .collect::<Option<Vec<_>>>()?;
                        Kind::Choices {
                            labels,
                            values: values.clone(),
                            multi,
                        }
                    } else if let Some(values) = choices["oneOf"]
                        .as_array()
                        .or_else(|| choices["anyOf"].as_array())
                    {
                        Kind::Choices {
                            labels: values
                                .iter()
                                .map(|v| v["title"].as_str().map(str::to_owned))
                                .collect::<Option<_>>()?,
                            values: values.iter().map(|v| v["const"].clone()).collect(),
                            multi,
                        }
                    } else {
                        Kind::Text
                    }
                }
            };
            if let Kind::Choices { labels, .. } = &kind
                && (labels.len() > text::MAX_FORM_CHOICES
                    || labels
                        .iter()
                        .any(|label| !short(label, text::MAX_FORM_LABEL_CHARS)))
            {
                return None;
            }
            let secret =
                !matches!(kind, Kind::Choices { .. }) && (secret_word(name) || secret_word(title));
            Some(Field {
                name: name.clone(),
                title: title.into(),
                description: description.map(str::to_owned),
                kind,
                secret,
            })
        })
        .collect()
}

fn questions(fields: &[Field], message: &str, flight: &Flight) -> Vec<Question> {
    fields
        .iter()
        .filter(|field| !field.secret)
        .enumerate()
        .map(|(i, field)| {
            let description = field.description.as_deref();
            let question = if i == 0 {
                description.map_or_else(
                    || message.to_owned(),
                    |description| format!("{message} {description}"),
                )
            } else {
                description.unwrap_or(&field.title).to_owned()
            };
            let (labels, multi_select) = match &field.kind {
                Kind::Choices { labels, multi, .. } => (labels.as_slice(), *multi),
                _ => (&[][..], false),
            };
            Question {
                header: flight.scrub(if fields.len() == 1 {
                    manage::HEADER
                } else {
                    &field.title
                }),
                question: flight.scrub(&question),
                options: labels
                    .iter()
                    .map(|label| QuestionOption {
                        label: flight.scrub(label),
                        description: String::new(),
                    })
                    .chain(std::iter::once(QuestionOption {
                        label: manage::NOT_NOW.into(),
                        description: String::new(),
                    }))
                    .collect(),
                multi_select,
                source: Some(QuestionSource {
                    asked_by: AskedBy::Server {
                        name: flight.server.clone(),
                    },
                    typed: !matches!(field.kind, Kind::Choices { .. }),
                }),
            }
        })
        .collect()
}

fn values(fields: &[Field], answers: &[Answer]) -> Result<Map<String, Value>, Option<String>> {
    let fields = fields
        .iter()
        .filter(|field| !field.secret)
        .collect::<Vec<_>>();
    if fields.len() != answers.len() {
        return Err(None);
    }
    let words = fields
        .iter()
        .zip(answers)
        .filter(|(field, _)| matches!(field.kind, Kind::Choices { .. }))
        .filter_map(|(_, answer)| answer.own.as_deref())
        .collect::<Vec<_>>();
    if !words.is_empty() {
        return Err(Some(words.join("\n")));
    }
    let mut content = Map::new();
    for (field, answer) in fields.into_iter().zip(answers) {
        if answer.is_skipped() {
            continue;
        }
        let value = match &field.kind {
            Kind::Choices { values, multi, .. } => {
                let chosen = answer
                    .chosen
                    .iter()
                    .map(|i| {
                        usize::try_from(*i)
                            .ok()
                            .and_then(|i| values.get(i))
                            .cloned()
                    })
                    .collect::<Option<Vec<_>>>()
                    .ok_or(None)?;
                if *multi {
                    Value::Array(chosen)
                } else {
                    chosen.into_iter().next().ok_or(None)?
                }
            }
            _ => {
                if !answer.chosen.is_empty() {
                    return Err(None);
                }
                let own = answer.own.clone().ok_or(None)?;
                typed_value(&field.kind, own)
            }
        };
        content.insert(field.name.clone(), value);
    }
    Ok(content)
}

fn typed_value(kind: &Kind, words: String) -> Value {
    if matches!(kind, Kind::Number) {
        words
            .parse::<serde_json::Number>()
            .map(Value::Number)
            .unwrap_or(Value::String(words))
    } else {
        Value::String(words)
    }
}

pub(super) async fn answer(
    flight: &Flight,
    request: ElicitRequestParams,
) -> Result<ElicitResult, String> {
    let ElicitRequestParams::FormElicitationParams {
        message,
        requested_schema,
        ..
    } = request
    else {
        return Err(text::INPUT_REQUIRED.into());
    };
    if !flight.user.is_available() {
        return Err(text::INPUT_REQUIRED.into());
    }
    let Some(fields) =
        fields(&requested_schema).filter(|_| short(&message, text::MAX_FORM_MESSAGE_CHARS))
    else {
        flight
            .notes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(text::form_too_large(&flight.server));
        return Ok(ElicitResult::new(ElicitationAction::Decline));
    };
    let _pause = flight.pause();
    let ask = async {
        let questions = questions(&fields, &message, flight);
        let mut content = if questions.is_empty() {
            Map::new()
        } else {
            match flight.user.ask(questions).await {
                AskReply::Answered { answers } => match values(&fields, &answers) {
                    Ok(content) => content,
                    Err(words) => {
                        if let Some(words) = words {
                            flight
                                .notes
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .push(text::declined_words(&flight.server, &words));
                        }
                        return ElicitResult::new(ElicitationAction::Decline);
                    }
                },
                AskReply::MessageInstead | AskReply::Declined => {
                    return ElicitResult::new(ElicitationAction::Decline);
                }
                _ => return ElicitResult::new(ElicitationAction::Cancel),
            }
        };
        for field in fields.iter().filter(|field| field.secret) {
            let reply = flight
                .user
                .secret(
                    Goes::Server(SecretDestination {
                        server: flight.server.clone(),
                        command: None,
                    }),
                    Some(AskedBy::Server {
                        name: flight.server.clone(),
                    }),
                    flight.scrub(&field.title),
                    flight.scrub(field.description.as_deref().unwrap_or(&message)),
                    Ends::default(),
                )
                .await;
            let SecretReply::Entered(secret) = reply else {
                return ElicitResult::new(ElicitationAction::Cancel);
            };
            flight.received_secret.store(true, Ordering::Release);
            let value = typed_value(&field.kind, secret.expose().to_owned());
            let number = match &value {
                Value::Number(number) => number.to_string(),
                _ => String::new(),
            };
            let mut redactions = flight
                .redactions
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            *redactions = redactions.union([&Redactions::new([secret.into_inner(), number])]);
            content.insert(field.name.clone(), value);
        }
        ElicitResult::new(ElicitationAction::Accept).with_content(Value::Object(content))
    };
    Ok(tokio::select! {
        biased;
        () = flight.cancel.cancelled() => ElicitResult::new(ElicitationAction::Cancel),
        result = ask => result,
    })
}

#[cfg(test)]
mod tests;
