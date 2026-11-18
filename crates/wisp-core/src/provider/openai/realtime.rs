use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::{Value, json};
use wisp_protocol::{Billing, ErrorKind};
use wisp_tool_text::voice as words;

use super::voice::{WAIT, answered, failed, own, sent, token};
use super::{OpenAiProvider, message_item, text_part};
use crate::endpoints::ProviderKind;
use crate::provider::classify::error_object;
use crate::provider::voice::{LiveEvent, LiveSay, OpenCall, Opened, Spent, VoiceRoute};
use crate::provider::{ProviderError, scrub_error_text_capped, scrub_error_type};

const PROVIDER: &str = ProviderKind::OpenAi.title();
const MODEL: &str = "gpt-realtime-2.1-mini";
const LIMIT: Duration = Duration::from_secs(60 * 60);
const VOICES: [&str; 10] = [
    "marin", "cedar", "alloy", "ash", "ballad", "coral", "echo", "sage", "shimmer", "verse",
];
const AUDIO_TOKENS_PER_SECOND: f64 = 20.0;
const HEARD_LAG: Duration = Duration::from_millis(300);
const CANCEL: &str = "wisp_cancel";
const CLEAR: &str = "wisp_clear";
const CREATE: &str = "wisp_create";
const KNOWN: [&str; 17] = [
    "session.created",
    "session.updated",
    "input_audio_buffer.speech_stopped",
    "input_audio_buffer.committed",
    "conversation.item.input_audio_transcription.delta",
    "response.output_audio_transcript.delta",
    "response.output_audio_transcript.done",
    "response.output_audio.delta",
    "response.output_audio.done",
    "response.output_item.added",
    "response.output_item.done",
    "response.content_part.added",
    "response.content_part.done",
    "conversation.item.added",
    "conversation.item.done",
    "conversation.item.truncated",
    "rate_limits.updated",
];

pub(super) struct Route {
    provider: OpenAiProvider,
    pub(super) url: String,
    reading: Mutex<Reading>,
}

#[derive(Default)]
pub(super) struct Reading {
    queue: VecDeque<String>,
    running: Option<Running>,
    unplayed: Vec<Unplayed>,
    pub(super) since: Option<Instant>,
    held: Option<Vec<String>>,
}

struct Running {
    text: String,
    id: Option<String>,
    cancelled: bool,
}

struct Unplayed {
    text: String,
    seconds: Option<f64>,
}

impl Route {
    pub(super) fn new(provider: OpenAiProvider) -> Option<Self> {
        let url = provider.api_url("realtime/calls")?;
        let reading = Mutex::default();
        Some(Self {
            provider,
            url,
            reading,
        })
    }

    pub(super) fn reading(&self) -> MutexGuard<'_, Reading> {
        self.reading.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn session(voice: &str, language: Option<&str>) -> Value {
    let turns = json!({"type": "semantic_vad", "eagerness": "high", "create_response": false,
        "interrupt_response": true});
    let mut transcription = json!({"model": crate::pricing::voice_transcriber(MODEL)});
    if let Some(language) = language {
        transcription["language"] = json!(language);
    }
    json!({"type": "realtime", "model": MODEL, "instructions": words::READER,
        "output_modalities": ["audio"], "reasoning": {"effort": "minimal"},
        "audio": {"input": {"transcription": transcription, "turn_detection": turns,
            "noise_reduction": {"type": "far_field"}}, "output": {"voice": voice}}})
}

fn create(text: &str) -> String {
    let mut message = message_item("user", text_part("user", text));
    message["type"] = json!("message");
    let response =
        json!({"conversation": "none", "output_modalities": ["audio"], "input": [message]});
    json!({"type": "response.create", "event_id": CREATE, "response": response}).to_string()
}

fn reported(error: &Value) -> LiveEvent {
    failed(error, |kind, _| (kind, worded(kind, error)))
}

fn worded(kind: ErrorKind, error: &Value) -> String {
    let object = error_object(error).unwrap_or_default();
    let said = object
        .message
        .map(|m| scrub_error_text_capped(&m, words::REPORTED_CHARS));
    let code = object.code.or(object.error_type);
    let code = code.as_deref().map(scrub_error_type);
    words::reported(&kind.sentence(PROVIDER), said.as_deref(), code.as_deref())
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

    fn answers_alone(&self) -> bool {
        false
    }

    fn limit(&self) -> Option<Duration> {
        Some(LIMIT)
    }

    fn busy(&self) -> bool {
        let reading = self.reading();
        reading.running.is_some()
            || !reading.queue.is_empty()
            || reading.since.is_some()
            || reading.held.is_some()
    }

    fn open(&self, call: OpenCall) -> BoxFuture<'static, Result<Opened, ProviderError>> {
        let provider = self.provider.clone();
        let url = self.url.clone();
        async move {
            let token = token(&provider, PROVIDER).await?;
            let form = reqwest::multipart::Form::new()
                .text("sdp", call.offer)
                .text(
                    "session",
                    session(&call.voice, call.language.as_deref()).to_string(),
                );
            let request = provider.client.post(url).bearer_auth(token);
            answered(request.timeout(WAIT).multipart(form), PROVIDER, |refusal| {
                let kind = refusal.classified.kind;
                let message = worded(kind, &refusal.raw);
                ProviderError::classified(kind, message, refusal.classified.detail)
            })
            .await
        }
        .boxed()
    }

    fn read(&self, frame: &str) -> Vec<LiveEvent> {
        let Ok(frame) = serde_json::from_str::<Value>(frame) else {
            return vec![LiveEvent::Unknown(String::new())];
        };
        let kind = frame["type"].as_str().unwrap_or_default();
        let mut reading = self.reading();
        match kind {
            "conversation.item.input_audio_transcription.completed" => {
                let transcribed_seconds = frame["usage"]["seconds"].as_f64().unwrap_or_default();
                let spent = Spent {
                    transcribed_seconds,
                    ..Spent::default()
                };
                let text = frame["transcript"].as_str().unwrap_or_default();
                [LiveEvent::Spent(spent)]
                    .into_iter()
                    .chain(reading.heard(text, Instant::now()))
                    .collect()
            }
            "conversation.item.input_audio_transcription.failed" => {
                reading.heard("", Instant::now())
            }
            "input_audio_buffer.speech_started" => reading.flush(Instant::now()),
            "response.created" => {
                if let Some(running) = reading.running.as_mut() {
                    running.id = frame["response"]["id"].as_str().map(str::to_owned);
                }
                vec![LiveEvent::Ignored]
            }
            "response.done" => reading.done(&frame["response"]),
            "output_audio_buffer.started" => {
                reading.since.get_or_insert_with(Instant::now);
                vec![LiveEvent::Ignored]
            }
            "output_audio_buffer.stopped" => {
                reading.played();
                vec![LiveEvent::TurnDone]
            }
            "output_audio_buffer.cleared" => {
                reading.played();
                vec![LiveEvent::Ignored]
            }
            "error" if caused(&frame, &[CANCEL, CLEAR]) => vec![LiveEvent::Ignored],
            "error" if caused(&frame, &[CREATE]) => reading.refused(&frame),
            "error" => vec![reported(&frame)],
            _ if KNOWN.contains(&kind) => vec![LiveEvent::Ignored],
            _ => vec![LiveEvent::Unknown(kind.to_owned())],
        }
    }

    fn write(&self, say: &LiveSay) -> Vec<String> {
        let LiveSay::Reply { text, .. } = say else {
            return Vec::new();
        };
        let mut reading = self.reading();
        reading.queue.push_back(text.clone());
        reading.next().into_iter().collect()
    }

    fn close(&self, call: &Opened) -> BoxFuture<'static, Result<(), ProviderError>> {
        let provider = self.provider.clone();
        let url = format!("{}/{}/hangup", self.url, call.handle);
        let called = !call.handle.is_empty();
        async move {
            if !called {
                return Ok(());
            }
            let token = token(&provider, PROVIDER).await?;
            let request = provider.client.post(url).bearer_auth(token).timeout(WAIT);
            let response = sent(request, PROVIDER).await?;
            if response.status().is_success() {
                return Ok(());
            }
            let status = response.status().as_u16();
            let kind = crate::provider::classify::http_error(status, response.headers(), None).kind;
            Err(own(kind, PROVIDER))
        }
        .boxed()
    }
}

impl Reading {
    fn next(&mut self) -> Option<String> {
        if self.running.is_some() || self.held.is_some() {
            return None;
        }
        let text = self.queue.pop_front()?;
        let frame = create(&text);
        self.unplayed.push(Unplayed {
            text: text.clone(),
            seconds: None,
        });
        self.running = Some(Running {
            text,
            id: None,
            cancelled: false,
        });
        Some(frame)
    }

    fn flush(&mut self, now: Instant) -> Vec<LiveEvent> {
        if self.held.is_some() {
            return Vec::new();
        }
        let since = self
            .since
            .take()
            .map(|since| now.saturating_duration_since(since));
        let elapsed = since.unwrap_or_default().saturating_sub(HEARD_LAG);
        let lengths: Vec<Option<f64>> = self.unplayed.iter().map(|r| r.seconds).collect();
        let at = playing(&lengths, elapsed.as_secs_f64());
        let mut frames = Vec::new();
        if let Some(running) = self.running.as_mut() {
            running.cancelled = true;
            let mut cancel = json!({"type": "response.cancel", "event_id": CANCEL});
            if let Some(id) = &running.id {
                cancel["response_id"] = json!(id);
            }
            frames.push(cancel);
        }
        if !self.unplayed.is_empty() {
            frames.push(json!({"type": "output_audio_buffer.clear", "event_id": CLEAR}));
        }
        self.held = Some(self.unplayed.drain(..).skip(at).map(|r| r.text).collect());
        frames
            .into_iter()
            .map(|frame| LiveEvent::Send(frame.to_string()))
            .collect()
    }

    fn heard(&mut self, text: &str, now: Instant) -> Vec<LiveEvent> {
        let words = !text.trim().is_empty();
        let mut events = if words { self.flush(now) } else { Vec::new() };
        let kept = self.held.take().unwrap_or_default();
        if !words {
            for text in kept.into_iter().rev() {
                self.queue.push_front(text);
            }
        } else {
            self.queue.clear();
            events.push(LiveEvent::Interrupted {
                cut: kept.into_iter().next(),
            });
            events.push(LiveEvent::Heard(text.trim().to_owned()));
        }
        events.extend(self.next().map(LiveEvent::Send));
        events
    }

    fn done(&mut self, response: &Value) -> Vec<LiveEvent> {
        let usage = &response["usage"];
        let mut events: Vec<LiveEvent> = usage
            .is_object()
            .then(|| LiveEvent::Spent(spent(usage)))
            .into_iter()
            .collect();
        let running = self.running.take();
        let audio = usage["output_token_details"]["audio_tokens"].as_f64();
        let last = self.unplayed.last_mut().filter(|r| r.seconds.is_none());
        if let Some(last) = last.filter(|_| running.is_some()) {
            last.seconds = Some(audio.unwrap_or_default() / AUDIO_TOKENS_PER_SECOND);
        }
        match response["status"].as_str() {
            Some("failed") => events.extend(self.broke(&response["status_details"], running)),
            Some("cancelled") => {}
            _ => {
                if let Some(running) = running.filter(|running| !running.cancelled) {
                    let said = transcript(response);
                    if words_of(&said) != words_of(&running.text) {
                        events.push(LiveEvent::Said(said));
                    }
                }
            }
        }
        events.extend(self.next().map(LiveEvent::Send));
        events
    }

    fn refused(&mut self, error: &Value) -> Vec<LiveEvent> {
        let running = self.running.take();
        if running.is_some() && self.unplayed.last().is_some_and(|r| r.seconds.is_none()) {
            self.unplayed.pop();
        }
        self.broke(error, running)
    }

    fn broke(&mut self, error: &Value, running: Option<Running>) -> Vec<LiveEvent> {
        self.queue.clear();
        let cut = running.map(|running| running.text);
        vec![reported(error), LiveEvent::Interrupted { cut }]
    }

    fn played(&mut self) {
        self.since = None;
        let running = self.running.is_some();
        self.unplayed.retain(|r| running && r.seconds.is_none());
    }
}

pub(super) fn playing(lengths: &[Option<f64>], elapsed: f64) -> usize {
    let mut end = 0.0;
    let reached = |length: &Option<f64>| match length {
        Some(seconds) => {
            end += seconds;
            end > elapsed
        }
        None => true,
    };
    lengths.iter().position(reached).unwrap_or(lengths.len())
}

fn caused(error: &Value, ids: &[&str]) -> bool {
    let id = error["error"]["event_id"].as_str();
    id.is_some_and(|id| ids.contains(&id))
}

fn spent(usage: &Value) -> Spent {
    let count = |value: &Value| value.as_u64().unwrap_or_default();
    let input = &usage["input_token_details"];
    let cached = &input["cached_tokens_details"];
    let output = &usage["output_token_details"];
    let fresh = |kind: &str| count(&input[kind]).saturating_sub(count(&cached[kind]));
    Spent {
        text_in: fresh("text_tokens"),
        text_cached: count(&cached["text_tokens"]),
        text_out: count(&output["text_tokens"]),
        audio_in: fresh("audio_tokens"),
        audio_cached: count(&cached["audio_tokens"]),
        audio_out: count(&output["audio_tokens"]),
        transcribed_seconds: 0.0,
    }
}

fn transcript(response: &Value) -> String {
    let items = response["output"].as_array().into_iter().flatten();
    let parts = items.flat_map(|item| item["content"].as_array().into_iter().flatten());
    parts
        .filter_map(|part| part["transcript"].as_str())
        .collect()
}

fn words_of(text: &str) -> Vec<String> {
    let text = text.to_lowercase().replace(['\'', '\u{2019}'], "");
    let words = text.split(|c: char| !c.is_alphanumeric());
    words.filter(|w| !w.is_empty()).map(str::to_owned).collect()
}
