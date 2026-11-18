use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use reqwest::header::{HeaderMap, LOCATION};
use serde::Serialize;
use serde_json::Value;
use wisp_protocol::subscription_auth::SubscriptionProvider;
use wisp_protocol::{Billing, ErrorKind};
use wisp_tool_text::voice as words;

use super::{CHATGPT_BASE_URL, OpenAiProvider, plan_usage};
use crate::provider::ProviderError;
use crate::provider::classify::{self, Classified, error_object};
use crate::provider::voice::{LiveEvent, LiveSay, OpenCall, Opened, VoiceRoute};

const PROVIDER: &str = SubscriptionProvider::ChatGpt.title();
const CALLS: &str = "/codex/realtime/calls?intent=quicksilver&architecture=avas";
const MODEL: &str = "gpt-live-1-codex";
const VOICES: [&str; 25] = [
    "cove", "juniper", "maple", "spruce", "ember", "vale", "breeze", "arbor", "sol", "aube",
    "bubbie", "haetsal", "hanul", "himari", "leher", "miko", "neer", "porto", "rindo", "rio",
    "rivage", "selva", "sonna", "tinta", "viola",
];
const CHUNK: usize = 500;
pub(super) const WAIT: Duration = Duration::from_secs(15);

pub(super) struct Route {
    provider: OpenAiProvider,
    pub(super) url: String,
}

impl Route {
    pub(super) fn new(provider: OpenAiProvider) -> Self {
        Self {
            provider,
            url: format!("{CHATGPT_BASE_URL}{CALLS}"),
        }
    }
}

#[derive(Serialize)]
struct Create<'a> {
    sdp: &'a str,
    session: Session<'a>,
}

#[derive(Serialize)]
struct Session<'a> {
    instructions: &'a str,
    audio: Audio<'a>,
    delegation: Typed,
    model: &'static str,
}

#[derive(Serialize)]
struct Audio<'a> {
    output: Output<'a>,
}

#[derive(Serialize)]
struct Output<'a> {
    voice: &'a str,
}

#[derive(Serialize)]
struct Typed {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum Append<'a> {
    #[serde(rename = "delegation.context.append")]
    Delegation {
        delegation_item_id: &'a str,
        channel: Channel,
        content: [Text<'a>; 1],
    },
    #[serde(rename = "session.context.append")]
    Session {
        channel: Channel,
        content: [Text<'a>; 1],
    },
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum Channel {
    Speakable,
    Commentary,
}

#[derive(Serialize)]
struct Text<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    text: &'a str,
}

impl VoiceRoute for Route {
    fn provider(&self) -> &str {
        PROVIDER
    }

    fn model(&self) -> &str {
        MODEL
    }

    fn voices(&self) -> &[&'static str] {
        &VOICES
    }

    fn billing(&self) -> Billing {
        self.provider.credential.billing()
    }

    fn open(&self, call: OpenCall) -> BoxFuture<'static, Result<Opened, ProviderError>> {
        let provider = self.provider.clone();
        let url = self.url.clone();
        async move {
            let token = token(&provider, PROVIDER).await?;
            let body = Create {
                sdp: &call.offer,
                session: Session {
                    instructions: &call.instructions,
                    audio: Audio {
                        output: Output { voice: &call.voice },
                    },
                    delegation: Typed { kind: "client" },
                    model: MODEL,
                },
            };
            let body = serde_json::to_string(&body);
            let body = body.map_err(|_| own(ErrorKind::InvalidRequest, PROVIDER))?;
            let request = provider
                .client
                .post(url)
                .bearer_auth(token)
                .header("Content-Type", "application/json")
                .header("openai-alpha", "quicksilver=v2")
                .header("session-id", uuid::Uuid::new_v4().to_string())
                .timeout(WAIT)
                .body(body);
            let now = provider.clock.now().as_second();
            let request = plan_usage::identity(request);
            answered(request, PROVIDER, |refusal| refused(refusal, now)).await
        }
        .boxed()
    }

    fn read(&self, frame: &str) -> Vec<LiveEvent> {
        let Ok(frame) = serde_json::from_str::<Value>(frame) else {
            return vec![LiveEvent::Unknown(String::new())];
        };
        let kind = frame["type"].as_str().unwrap_or_default();
        vec![match kind {
            "turn.done" => spoken(&frame["turn"]),
            "delegation.created" => {
                delegated(&frame["item"]).unwrap_or_else(|| LiveEvent::Unknown(kind.to_owned()))
            }
            "error" => failed(&frame, |kind, said| failure(kind, None, said)),
            "input_transcript.added" => LiveEvent::Interrupted { cut: None },
            "output_transcript.added" => LiveEvent::TurnDone,
            "session.started"
            | "session.updated"
            | "session.usage.updated"
            | "output_audio.delta"
            | "turn.created"
            | "turn.delta"
            | "delegation.context.appended" => LiveEvent::Ignored,
            _ => LiveEvent::Unknown(kind.to_owned()),
        }]
    }

    fn write(&self, say: &LiveSay) -> Vec<String> {
        let (delegation, channel, text) = match say {
            LiveSay::Reply { delegation, text } => (delegation.as_ref(), Channel::Speakable, text),
            LiveSay::Note { delegation, text } => (delegation.as_ref(), Channel::Commentary, text),
            LiveSay::HangUp => return vec![r#"{"type":"session.close"}"#.to_owned()],
        };
        chunks(text)
            .filter_map(|text| {
                let content = [Text {
                    kind: "input_text",
                    text,
                }];
                let append = match delegation {
                    Some(id) => Append::Delegation {
                        delegation_item_id: id,
                        channel,
                        content,
                    },
                    None => Append::Session { channel, content },
                };
                serde_json::to_string(&append).ok()
            })
            .collect()
    }

    fn close(&self, _: &Opened) -> BoxFuture<'static, Result<(), ProviderError>> {
        futures::future::ready(Ok(())).boxed()
    }
}

fn spoken(turn: &Value) -> LiveEvent {
    let text = turn["transcript"]
        .as_str()
        .map(str::trim)
        .unwrap_or_default();
    match turn["role"].as_str() {
        Some("assistant") if text.is_empty() => LiveEvent::TurnDone,
        _ if text.is_empty() => LiveEvent::Ignored,
        Some("user") => LiveEvent::Heard(text.to_owned()),
        Some("assistant") => LiveEvent::Said(text.to_owned()),
        _ => LiveEvent::Unknown("turn.done".to_owned()),
    }
}

fn delegated(item: &Value) -> Option<LiveEvent> {
    if item["type"] != "delegation" || item["target"] != "client" {
        return None;
    }
    let id = item["id"].as_str()?.to_owned();
    let request: String = item["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|part| part["type"] == "input_text")
        .filter_map(|part| part["text"].as_str())
        .collect();
    let request = Some(request).filter(|r| !r.trim().is_empty());
    Some(LiveEvent::Delegated { id, request })
}

fn chunks(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let (chunk, tail) = rest.split_at(rest.floor_char_boundary(CHUNK));
        rest = tail;
        Some(chunk)
    })
}

fn handle(headers: &HeaderMap) -> String {
    let location = headers.get(LOCATION).and_then(|l| l.to_str().ok());
    let path = location
        .and_then(|l| l.split('?').next())
        .unwrap_or_default();
    path.rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or_default()
        .to_owned()
}

pub(super) struct Refusal {
    status: u16,
    headers: HeaderMap,
    said: String,
    pub(super) raw: Value,
    pub(super) classified: Classified,
}

pub(super) async fn answered(
    request: reqwest::RequestBuilder,
    provider: &str,
    refuse: impl FnOnce(Refusal) -> ProviderError,
) -> Result<Opened, ProviderError> {
    let response = sent(request, provider).await?;
    if !response.status().is_success() {
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let said = crate::provider::body::read_text(response).await;
        let raw = serde_json::from_str::<Value>(&said).unwrap_or_default();
        let classified = classify::http_error(status, &headers, error_object(&raw).as_ref());
        return Err(refuse(Refusal {
            status,
            headers,
            said,
            raw,
            classified,
        }));
    }
    let handle = handle(response.headers());
    let answer = crate::provider::body::read_text(response).await;
    if !answer.starts_with("v=") {
        return Err(own(ErrorKind::InvalidResponse, provider));
    }
    Ok(Opened { answer, handle })
}

fn refused(refusal: Refusal, now: i64) -> ProviderError {
    let Classified { kind, mut detail } = refusal.classified;
    let capped = if refusal.status == 429 {
        plan_usage::refusal(&refusal.raw, &refusal.headers, now, &mut detail)
    } else {
        None
    };
    let (kind, sentence) = failure(kind, capped, &refusal.said);
    ProviderError::classified(kind, sentence, detail)
}

pub(super) fn failed(
    frame: &Value,
    word: impl FnOnce(ErrorKind, &str) -> (ErrorKind, String),
) -> LiveEvent {
    let kind = error_object(frame).and_then(|e| e.kind());
    let (kind, message) = word(kind.unwrap_or(ErrorKind::Other), &frame.to_string());
    LiveEvent::Failed { kind, message }
}

fn failure(kind: ErrorKind, capped: Option<ErrorKind>, said: &str) -> (ErrorKind, String) {
    let kind = match (capped, kind) {
        (Some(capped), _) => capped,
        (None, ErrorKind::ContextOverflow) => ErrorKind::InvalidRequest,
        (None, kind) => kind,
    };
    let said = said.to_lowercase();
    let says = |marks: &[&str]| marks.iter().any(|mark| said.contains(mark));
    let own = if capped.is_some() || says(&["usage_limit", "usage limit", "usage_not_included"]) {
        Some(words::NO_VOICE_TIME)
    } else if says(&["no_matching_rule"]) {
        Some(words::NOT_OPEN)
    } else if says(&["session.model"]) {
        Some(words::VOICE_MODEL)
    } else if kind == ErrorKind::Auth && says(&["account", "workspace"]) {
        Some(words::CALL_REFUSED)
    } else {
        None
    };
    let sentence = own.map_or_else(|| kind.sentence(PROVIDER), str::to_owned);
    (kind, sentence)
}

pub(super) async fn token(provider: &OpenAiProvider, name: &str) -> Result<String, ProviderError> {
    let token = provider.credential.access_token().await;
    token.map_err(|e| worded(e, name))
}

pub(super) async fn sent(
    request: reqwest::RequestBuilder,
    provider: &str,
) -> Result<reqwest::Response, ProviderError> {
    let response = request.send().await;
    response.map_err(|e| worded(crate::provider::sse::send_failure(e), provider))
}

fn worded(mut error: ProviderError, provider: &str) -> ProviderError {
    error.message = error.kind.sentence(provider);
    error
}

pub(super) fn own(kind: ErrorKind, provider: &str) -> ProviderError {
    ProviderError::new(kind, kind.sentence(provider))
}
