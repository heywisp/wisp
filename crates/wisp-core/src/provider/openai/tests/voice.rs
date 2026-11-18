use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::sync::{Mutex, mpsc};
use wisp_protocol::plan_usage::UsageWindow;
use wisp_protocol::voice::{AudioFormat, AudioFrame, CallId, FRAME_MS};
use wisp_protocol::{Billing, ErrorKind};
use wisp_tool_text::voice as words;

use super::{endpoint, provider, subscription};
use crate::provider::Provider;
use crate::provider::credential::Credential;
use crate::provider::openai::voice::Route;
use crate::provider::openai::{CHATGPT_BASE_URL, DEFAULT_BASE_URL, OpenAiProvider, realtime};
use crate::provider::voice::{LiveEvent, LiveSay, OpenCall, Opened, Spent, VoiceRoute};
use crate::test_guard::{http, or_stop, refusing_addr};
use crate::voice::{Control, Leg, LegAudio};

fn at(origin: &str) -> Route {
    let mut route = Route::new(subscription());
    route.url = route.url.replacen(CHATGPT_BASE_URL, origin, 1);
    route
}

fn call() -> OpenCall {
    OpenCall {
        offer: "v=0 offer".into(),
        instructions: "Speak softly.".into(),
        voice: "cove".into(),
        language: None,
    }
}

fn headers(request: &str) -> BTreeMap<String, String> {
    http::header_lines(request).into_iter().collect()
}

fn error(error: Value) -> Value {
    json!({"type": "error", "error": error})
}

fn failed(kind: ErrorKind, message: &str) -> LiveEvent {
    LiveEvent::Failed {
        kind,
        message: message.into(),
    }
}

fn reported(kind: ErrorKind, said: &str) -> LiveEvent {
    failed(kind, &format!("{} {said}", kind.sentence("OpenAI")))
}

fn api(origin: &str) -> realtime::Route {
    let mut route = realtime::Route::new(provider()).unwrap();
    route.url = format!("{origin}/v1/realtime/calls");
    route
}

fn not_home(requests: &[String]) {
    if let Some(home) = std::env::var_os("HOME").and_then(|h| h.into_string().ok()) {
        assert!(!requests.iter().any(|r| r.contains(&home)));
    }
}

#[tokio::test]
async fn a_subscription_call_goes_only_to_chatgpt_with_codexs_markers_and_api_voice_only_to_openai()
{
    assert!(provider().voice().is_some());
    assert!(subscription().voice().is_some());
    let key = Credential::ApiKey("synthetic-openai-key".into());
    let gateway = endpoint(key, "https://gateway.example/v1");
    assert!(
        OpenAiProvider::from_endpoint(&gateway)
            .unwrap()
            .voice()
            .is_none()
    );
    assert_eq!(
        Route::new(subscription()).url,
        "https://chatgpt.com/backend-api/codex/realtime/calls?intent=quicksilver&architecture=avas"
    );
    let answer = "v=0\r\no=- 1 1 IN IP4 0.0.0.0\r\n";
    let created = || {
        let location = [("location", "/v1/live/call_7f3a?from=create")];
        http::Reply::Send(http::response("201 Created", &location, answer))
    };
    let server = http::serve(vec![created(), created()]).await;
    let route = at(&server.url);
    for _ in 0..2 {
        let opened = route.open(call()).await.unwrap();
        let handle = "call_7f3a".into();
        let answer = answer.into();
        assert_eq!(opened, Opened { answer, handle });
    }
    let requests = server.requests.lock().unwrap().clone();
    let path = "/codex/realtime/calls?intent=quicksilver&architecture=avas";
    assert!(requests[0].starts_with(&format!("POST {path} HTTP/1.1\r\n")));
    let sent = headers(&requests[0]);
    let names: Vec<&str> = sent.keys().map(String::as_str).collect();
    let expected = [
        "accept",
        "authorization",
        "content-length",
        "content-type",
        "host",
        "openai-alpha",
        "originator",
        "session-id",
        "user-agent",
    ];
    assert_eq!(names, expected);
    assert_eq!(sent["authorization"], "Bearer synthetic-oauth");
    assert_eq!(sent["content-type"], "application/json");
    assert_eq!(sent["openai-alpha"], "quicksilver=v2");
    assert_eq!(sent["originator"], "codex_cli_rs");
    let agent = "codex_cli_rs/0.0.0 (Unknown 0; unknown) unknown";
    assert_eq!(sent["user-agent"], agent);
    let again = headers(&requests[1]);
    assert!(uuid::Uuid::parse_str(&sent["session-id"]).is_ok());
    assert_ne!(sent["session-id"], again["session-id"]);
    let body: Value = serde_json::from_str(http::body(&requests[0])).unwrap();
    let session = json!({
        "instructions": "Speak softly.",
        "audio": {"output": {"voice": "cove"}},
        "delegation": {"type": "client"},
        "model": "gpt-live-1-codex",
    });
    assert_eq!(body, json!({"sdp": "v=0 offer", "session": session}));
    not_home(&requests);
}

fn form(request: &str) -> BTreeMap<String, String> {
    let kind = &headers(request)["content-type"];
    let boundary = format!("--{}", kind.split_once("boundary=").unwrap().1);
    let parts = http::body(request).split(&boundary).filter_map(|part| {
        let (head, value) = part.split_once("\r\n\r\n")?;
        let name = head.split_once("name=\"")?.1.split_once('"')?.0;
        Some((name.to_owned(), value.strip_suffix("\r\n")?.to_owned()))
    });
    parts.collect()
}

#[tokio::test]
async fn an_api_call_goes_to_realtime_calls_with_the_reader_session_and_hangs_up() {
    let route = realtime::Route::new(provider()).unwrap();
    assert_eq!(route.url, "https://api.openai.com/v1/realtime/calls");
    let answer = "v=0\r\no=- 1 1 IN IP4 0.0.0.0\r\n";
    let location = [("location", "/v1/realtime/calls/rtc_7f3a")];
    let created = http::Reply::Send(http::response("201 Created", &location, answer));
    let hung = http::Reply::Send(http::response("200 OK", &[], ""));
    let server = http::serve(vec![created, hung]).await;
    let route = api(&server.url);
    let call = OpenCall {
        language: Some("de".into()),
        ..call()
    };
    let opened = route.open(call).await.unwrap();
    let handle = "rtc_7f3a".into();
    assert_eq!(
        opened,
        Opened {
            answer: answer.into(),
            handle
        }
    );
    route.close(&opened).await.unwrap();
    let requests = server.requests.lock().unwrap().clone();
    assert!(requests[0].starts_with("POST /v1/realtime/calls HTTP/1.1\r\n"));
    let hang_up = "POST /v1/realtime/calls/rtc_7f3a/hangup HTTP/1.1\r\n";
    assert!(requests[1].starts_with(hang_up));
    let sent: Vec<BTreeMap<String, String>> = requests.iter().map(|r| headers(r)).collect();
    let names = |sent: &BTreeMap<String, String>| sent.keys().cloned().collect::<Vec<_>>();
    let expected = [
        "accept",
        "authorization",
        "content-length",
        "content-type",
        "host",
    ];
    assert_eq!(names(&sent[0]), expected);
    assert_eq!(names(&sent[1]), ["accept", "authorization", "host"]);
    assert!(
        sent.iter()
            .all(|s| s["authorization"] == "Bearer synthetic-openai-key")
    );
    let parts = form(&requests[0]);
    assert_eq!(parts.keys().collect::<Vec<_>>(), ["sdp", "session"]);
    assert_eq!(parts["sdp"], "v=0 offer");
    let session: Value = serde_json::from_str(&parts["session"]).unwrap();
    let turns = json!({"type": "semantic_vad", "eagerness": "high", "create_response": false,
        "interrupt_response": true});
    let input = json!({"transcription": {"model": "gpt-realtime-whisper", "language": "de"},
        "turn_detection": turns, "noise_reduction": {"type": "far_field"}});
    let expected = json!({"type": "realtime", "model": "gpt-realtime-2.1-mini",
        "instructions": words::READER, "output_modalities": ["audio"],
        "reasoning": {"effort": "minimal"},
        "audio": {"input": input, "output": {"voice": "cove"}}});
    assert_eq!(session, expected);
    not_home(&requests);
}

#[test]
fn every_event_the_checks_saw_is_read_or_known() {
    let route = Route::new(subscription());
    let facts = (
        route.provider(),
        route.model(),
        route.billing(),
        route.voices()[0],
    );
    assert_eq!(
        facts,
        ("ChatGPT", "gpt-live-1-codex", Billing::ChatGptPlan, "cove")
    );
    assert_eq!(route.voices().len(), 25);
    let turn = |role: &str, text: &str| json!({"type": "turn.done", "turn": {"role": role, "transcript": text}});
    let item = |target: &str, text: &str| {
        json!({"type": "delegation", "target": target, "id": "d1",
        "content": [{"type": "input_text", "text": text}]})
    };
    let delegated = |item: Value| json!({"type": "delegation.created", "item": item});
    let handed = |request: Option<&str>| LiveEvent::Delegated {
        id: "d1".into(),
        request: request.map(Into::into),
    };
    let capped = json!({"type": "error", "message": "You have reached your usage limit."});
    let rate = ErrorKind::RateLimit;
    let invalid = ErrorKind::InvalidRequest;
    let rows = [
        (
            turn("user", " Seven times eight? "),
            LiveEvent::Heard("Seven times eight?".into()),
        ),
        (
            turn("assistant", "Let me check."),
            LiveEvent::Said("Let me check.".into()),
        ),
        (turn("assistant", ""), LiveEvent::TurnDone),
        (turn("user", " "), LiveEvent::Ignored),
        (
            turn("system", "Hi."),
            LiveEvent::Unknown("turn.done".into()),
        ),
        (
            delegated(item("client", "the word")),
            handed(Some("the word")),
        ),
        (delegated(item("client", " ")), handed(None)),
        (
            delegated(item("server", "x")),
            LiveEvent::Unknown("delegation.created".into()),
        ),
        (
            error(json!({"code": "rate_limit_exceeded"})),
            failed(rate, &rate.sentence("ChatGPT")),
        ),
        (capped, failed(ErrorKind::Other, words::NO_VOICE_TIME)),
        (
            error(json!({"type": "usage_not_included"})),
            failed(ErrorKind::Other, words::NO_VOICE_TIME),
        ),
        (
            error(json!({"code": "context_length_exceeded"})),
            failed(invalid, &invalid.sentence("ChatGPT")),
        ),
        (
            json!({"type": "session.new"}),
            LiveEvent::Unknown("session.new".into()),
        ),
    ];
    for (frame, event) in rows {
        assert_eq!(route.read(&frame.to_string()), [event], "{frame}");
    }
    for known in [
        "session.started",
        "session.updated",
        "session.usage.updated",
        "input_transcript.added",
        "output_transcript.added",
        "output_audio.delta",
        "turn.created",
        "turn.delta",
        "delegation.context.appended",
    ] {
        let frame = json!({"type": known, "item": {"text": "a part"}}).to_string();
        let event = match known {
            "input_transcript.added" => LiveEvent::Interrupted { cut: None },
            "output_transcript.added" => LiveEvent::TurnDone,
            _ => LiveEvent::Ignored,
        };
        assert_eq!(route.read(&frame), [event], "{known}");
    }
    assert_eq!(route.read("not json"), [LiveEvent::Unknown(String::new())]);
}

fn create(text: &str) -> LiveEvent {
    let content = [json!({"type": "input_text", "text": text})];
    let message = json!({"type": "message", "role": "user", "content": content});
    let response =
        json!({"conversation": "none", "output_modalities": ["audio"], "input": [message]});
    let create =
        json!({"type": "response.create", "event_id": "wisp_create", "response": response});
    LiveEvent::Send(create.to_string())
}

fn done(status: &str, said: &str, audio: u64) -> String {
    let usage = json!({"input_token_details": {"text_tokens": 111, "audio_tokens": 0,
        "cached_tokens_details": {"text_tokens": 64, "audio_tokens": 0}},
        "output_token_details": {"text_tokens": 28, "audio_tokens": audio}});
    let output = [json!({"content": [{"type": "output_audio", "transcript": said}]})];
    let response = json!({"id": "resp_1", "status": status, "output": output, "usage": usage,
        "status_details": {"type": status, "error": {"type": "server_error"}}});
    json!({"type": "response.done", "response": response}).to_string()
}

fn spent(audio: u64) -> LiveEvent {
    let (text_in, text_cached, text_out, audio_out) = (47, 64, 28, audio);
    LiveEvent::Spent(Spent {
        text_in,
        text_cached,
        text_out,
        audio_out,
        ..Spent::default()
    })
}

fn heard(text: &str) -> String {
    json!({"type": "conversation.item.input_audio_transcription.completed", "transcript": text,
        "usage": {"type": "duration", "seconds": 2}})
    .to_string()
}

fn typed(kind: &str) -> String {
    json!({"type": kind}).to_string()
}

fn reply(route: &realtime::Route, text: &str) -> Vec<LiveEvent> {
    let say = LiveSay::Reply {
        delegation: Some("d1".into()),
        text: text.into(),
    };
    route.write(&say).into_iter().map(LiveEvent::Send).collect()
}

#[test]
fn the_api_route_reads_each_frame_and_checks_each_read_against_its_text() {
    let route = api("http://127.0.0.1:9");
    let facts = (
        route.provider(),
        route.model(),
        route.billing(),
        route.voices()[0],
    );
    assert_eq!(
        facts,
        ("OpenAI", "gpt-realtime-2.1-mini", Billing::Metered, "marin")
    );
    assert_eq!((route.voices().len(), route.answers_alone()), (10, false));
    assert_eq!(route.limit(), Some(Duration::from_secs(3600)));
    let quiet = LiveSay::Note {
        delegation: None,
        text: "Tapped: 7".into(),
    };
    assert!(route.write(&quiet).is_empty() && route.write(&LiveSay::HangUp).is_empty());
    let seconds = LiveEvent::Spent(Spent {
        transcribed_seconds: 2.0,
        ..Spent::default()
    });
    let words = [
        seconds,
        LiveEvent::Interrupted { cut: None },
        LiveEvent::Heard("Seven.".into()),
    ];
    assert_eq!(route.read(&heard(" Seven. ")), words);
    let rate = ErrorKind::RateLimit;
    let server = ErrorKind::Server;
    let rows = [
        (
            "Blue sky.",
            done("completed", "Blue sky!", 40),
            vec![spent(40)],
        ),
        (
            "You are 3/4 done.",
            done("completed", "You're three quarters done.", 40),
            vec![
                spent(40),
                LiveEvent::Said("You're three quarters done.".into()),
            ],
        ),
        ("Green.", done("cancelled", "Red.", 4), vec![spent(4)]),
        (
            "Seven.",
            done("failed", "", 0),
            vec![
                spent(0),
                reported(server, "(server_error)"),
                LiveEvent::Interrupted {
                    cut: Some("Seven.".into()),
                },
            ],
        ),
    ];
    for (text, frame, events) in rows {
        assert_eq!(reply(&route, text), [create(text)]);
        assert_eq!(route.read(&frame), events, "{text}");
    }
    let raced = error(json!({"code": "response_cancel_not_active", "event_id": "wisp_cancel"}));
    let unknown = format!("sk-proj-CANARY_ab1_cd2_ef3 {}", "界".repeat(64));
    let cases = [
        (raced, vec![LiveEvent::Ignored]),
        (
            error(json!({"code": unknown, "message": "Bearer 'CANARY.+/=_token' in {session}"})),
            vec![reported(
                ErrorKind::Other,
                &format!(
                    "Bearer '[redacted]' in {{session}} ([redacted] {})",
                    "界".repeat(53)
                ),
            )],
        ),
        (
            error(json!({"code": "rate_limit_exceeded"})),
            vec![reported(rate, "(rate_limit_exceeded)")],
        ),
        (
            error(
                json!({"type": "invalid_request_error", "code": "invalid_value",
                "message": " Unknown project proj_0123456789abcdefghijklmn. "}),
            ),
            vec![reported(
                ErrorKind::InvalidRequest,
                "Unknown project [redacted]. (invalid_value)",
            )],
        ),
        (
            json!({"type": "output_audio_buffer.stopped"}),
            vec![LiveEvent::TurnDone],
        ),
        (
            json!({"type": "rate_limits.updated"}),
            vec![LiveEvent::Ignored],
        ),
        (
            json!({"type": "response.output_audio_transcript.done"}),
            vec![LiveEvent::Ignored],
        ),
        (
            json!({"type": "conversation.item.truncated"}),
            vec![LiveEvent::Ignored],
        ),
        (
            json!({"type": "response.new"}),
            vec![LiveEvent::Unknown("response.new".into())],
        ),
    ];
    for (frame, events) in cases {
        assert_eq!(route.read(&frame.to_string()), events, "{frame}");
    }
    assert_eq!(route.read("not json"), [LiveEvent::Unknown(String::new())]);
}

#[test]
fn the_api_route_says_one_reply_at_a_time_and_stops_for_the_learner() {
    let lengths = [Some(2.0), Some(3.0), Some(1.0)];
    for (elapsed, index) in [(0.5, 0), (2.5, 1), (5.5, 2), (7.0, 3)] {
        assert_eq!(realtime::playing(&lengths, elapsed), index, "{elapsed}");
    }
    assert_eq!(realtime::playing(&[Some(2.0), None], 9.0), 1);
    let route = api("http://127.0.0.1:9");
    let started = |ago: u64| {
        route.reading().since = Instant::now().checked_sub(Duration::from_millis(ago));
    };
    let cancel = |id: Option<&str>| {
        let mut frame = json!({"type": "response.cancel", "event_id": "wisp_cancel"});
        if let Some(id) = id {
            frame["response_id"] = json!(id);
        }
        LiveEvent::Send(frame.to_string())
    };
    let clear = json!({"type": "output_audio_buffer.clear", "event_id": "wisp_clear"});
    let clear = LiveEvent::Send(clear.to_string());
    let next = |status: &str, said: &str, audio: u64, text: &str| {
        let frame = done(status, said, audio);
        assert_eq!(route.read(&frame), [spent(audio), create(text)], "{text}");
    };
    let cut = |text: &str| LiveEvent::Interrupted {
        cut: Some(text.into()),
    };
    let wait = LiveEvent::Heard("Wait.".into());
    let speech = typed("input_audio_buffer.speech_started");
    let noise = typed("conversation.item.input_audio_transcription.failed");
    assert_eq!(reply(&route, "A."), [create("A.")]);
    assert!(reply(&route, "B.").is_empty() && reply(&route, "C.").is_empty());
    next("completed", "A.", 40, "B.");
    next("completed", "B.", 20, "C.");
    let created = json!({"type": "response.created", "response": {"id": "resp_c"}});
    assert_eq!(route.read(&created.to_string()), [LiveEvent::Ignored]);
    started(2_150);
    let start = route.reading().since;
    assert_eq!(
        route.read(&typed("output_audio_buffer.started")),
        [LiveEvent::Ignored]
    );
    assert_eq!(route.reading().since, start);
    assert_eq!(route.read(&speech), [cancel(Some("resp_c")), clear.clone()]);
    assert!(route.read(&speech).is_empty());
    assert!(reply(&route, "D.").is_empty());
    assert!(route.read(&noise).is_empty());
    next("cancelled", "C", 10, "A.");
    next("completed", "A.", 40, "B.");
    next("completed", "B.", 20, "C.");
    assert_eq!(
        route.read(&typed("output_audio_buffer.stopped")),
        [LiveEvent::TurnDone]
    );
    assert_eq!(route.read(&speech), [cancel(None), clear.clone()]);
    assert!(route.read(&noise).is_empty());
    next("cancelled", "C", 4, "C.");
    let events = route.read(&heard("Wait."));
    assert_eq!(
        events[1..],
        [cancel(None), clear.clone(), cut("C."), wait.clone()]
    );
    assert_eq!(route.read(&done("cancelled", "C", 4)), [spent(4)]);
    assert_eq!(reply(&route, "F."), [create("F.")]);
    let failed = route.read(&done("failed", "", 0));
    assert_eq!(failed.last(), Some(&cut("F.")));
    assert_eq!(reply(&route, "G."), [create("G.")]);
    assert_eq!(route.read(&done("completed", "G.", 20)), [spent(20)]);
    started(500);
    assert_eq!(route.read(&speech), [clear]);
    assert_eq!(route.read(&heard("Wait."))[1..], [cut("G."), wait]);
    assert!(!route.busy());
    assert_eq!(reply(&route, "H."), [create("H.")]);
    assert!(route.busy() && reply(&route, "I.").is_empty());
    let refused = error(
        json!({"type": "invalid_request_error", "event_id": "wisp_create",
        "code": "conversation_already_has_active_response", "message": "Busy."}),
    );
    let said = "Busy. (conversation_already_has_active_response)";
    let failed = reported(ErrorKind::InvalidRequest, said);
    assert_eq!(route.read(&refused.to_string()), [failed, cut("H.")]);
    assert!(!route.busy());
    assert_eq!(reply(&route, "J."), [create("J.")]);
}

#[test]
fn every_routes_model_has_a_price() {
    let routes: [Box<dyn VoiceRoute>; 2] =
        [Box::new(Route::new(subscription())), Box::new(api(""))];
    for route in routes {
        let model = route.model();
        assert!(
            crate::pricing::voice_usd(model, 60, &[]).is_some(),
            "{model}"
        );
    }
}

#[test]
fn replies_and_notes_go_out_in_chunks_of_at_most_500_bytes() {
    let route = Route::new(subscription());
    let frames = |say: LiveSay| -> Vec<Value> {
        let written = route.write(&say);
        written
            .iter()
            .map(|f| serde_json::from_str(f).unwrap())
            .collect()
    };
    let append = |to: Option<&str>, channel: &str, text: &str| {
        let content = json!([{"type": "input_text", "text": text}]);
        match to {
            Some(id) => json!({"type": "delegation.context.append", "delegation_item_id": id,
                "channel": channel, "content": content}),
            None => {
                json!({"type": "session.context.append", "channel": channel, "content": content})
            }
        }
    };
    let (d1, text) = (Some("d1".to_owned()), "Blue.".to_owned());
    let reply = LiveSay::Reply {
        delegation: Some("d1".into()),
        text: text.clone(),
    };
    assert_eq!(frames(reply), [append(Some("d1"), "speakable", "Blue.")]);
    let unasked = LiveSay::Reply {
        delegation: None,
        text: text.clone(),
    };
    assert_eq!(frames(unasked), [append(None, "speakable", "Blue.")]);
    let quiet = LiveSay::Note {
        delegation: d1,
        text: text.clone(),
    };
    assert_eq!(frames(quiet), [append(Some("d1"), "commentary", "Blue.")]);
    let tap = LiveSay::Note {
        delegation: None,
        text,
    };
    assert_eq!(frames(tap), [append(None, "commentary", "Blue.")]);
    assert_eq!(frames(LiveSay::HangUp), [json!({"type": "session.close"})]);
    let long = format!("a{}", "é".repeat(300));
    let reply = LiveSay::Reply {
        delegation: Some("d2".into()),
        text: long.clone(),
    };
    let parts = frames(reply);
    let said: Vec<&str> = parts
        .iter()
        .map(|p| p["content"][0]["text"].as_str().unwrap())
        .collect();
    assert_eq!(said.iter().map(|s| s.len()).collect::<Vec<_>>(), [499, 102]);
    assert_eq!(said.concat(), long);
    assert!(parts.iter().all(|p| p["delegation_item_id"] == "d2"));
}

#[tokio::test]
async fn refusals_are_worded_for_the_user_and_carry_no_body() {
    let auth = ErrorKind::Auth;
    let invalid = ErrorKind::InvalidRequest;
    let depleted = "workspace_member_credits_depleted";
    let rows = [
        (
            "401",
            None,
            json!({"error": {"code": "no_matching_rule"}}),
            auth,
            Some(words::NOT_OPEN),
        ),
        (
            "403",
            None,
            json!({"detail": "This account can't call."}),
            auth,
            Some(words::CALL_REFUSED),
        ),
        (
            "400",
            None,
            json!({"error": {"message": "`session.model` is not allowed"}}),
            invalid,
            Some(words::VOICE_MODEL),
        ),
        (
            "429",
            None,
            json!({"error": {"type": "usage_limit_reached"}}),
            ErrorKind::RateLimit,
            Some(words::NO_VOICE_TIME),
        ),
        (
            "429",
            Some(depleted),
            json!({}),
            ErrorKind::Billing,
            Some(words::NO_VOICE_TIME),
        ),
        (
            "400",
            None,
            json!({"error": {"code": "context_length_exceeded"}}),
            invalid,
            None,
        ),
        ("401", None, json!({"detail": "Invalid token"}), auth, None),
        (
            "500",
            None,
            json!({"detail": "Down"}),
            ErrorKind::Server,
            None,
        ),
        (
            "201",
            None,
            json!({"answer": "not an SDP"}),
            ErrorKind::InvalidResponse,
            None,
        ),
    ];
    let replies = rows
        .iter()
        .map(|(status, reached, body, ..)| {
            let mut body = body.clone();
            body["canary"] = json!("CANARY");
            let mut headers = vec![("content-type", "application/json")];
            headers.extend(reached.map(|r| ("x-codex-rate-limit-reached-type", r)));
            let status = format!("{status} Status");
            http::Reply::Send(http::response(&status, &headers, &body.to_string()))
        })
        .collect();
    let server = http::serve(replies).await;
    let route = at(&server.url);
    for (status, _, _, kind, own) in rows {
        let error = route.open(call()).await.unwrap_err();
        let sentence = own.map_or_else(|| kind.sentence("ChatGPT"), str::to_owned);
        assert_eq!(
            (error.kind, &*error.message),
            (kind, &*sentence),
            "{status}"
        );
        assert!(!format!("{error:?}").contains("CANARY"), "{status}");
    }
    let (_held, refusing) = refusing_addr();
    let error = at(&format!("http://{refusing}"))
        .open(call())
        .await
        .unwrap_err();
    let failed = ErrorKind::Network.sentence("ChatGPT");
    assert_eq!((error.kind, error.message), (ErrorKind::Network, failed));
    let rows = [
        (
            "401",
            json!({"error": {"code": "invalid_api_key"}}),
            auth,
            " (invalid_api_key)",
        ),
        (
            "400",
            json!({"error": {"message": "`session.model` is bad"}}),
            invalid,
            " `session.model` is bad",
        ),
        (
            "429",
            json!({"error": {"type": "usage_limit_reached"}}),
            ErrorKind::Billing,
            " (usage_limit_reached)",
        ),
        ("500", json!({"detail": "Down"}), ErrorKind::Server, ""),
        (
            "201",
            json!({"answer": "not an SDP"}),
            ErrorKind::InvalidResponse,
            "",
        ),
    ];
    let replies = rows.iter().map(|(status, body, ..)| {
        let mut body = body.clone();
        body["canary"] = json!("CANARY");
        let body = body.to_string();
        let headers = [("content-type", "application/json")];
        http::Reply::Send(http::response(&format!("{status} Status"), &headers, &body))
    });
    let server = http::serve(replies.collect()).await;
    let route = api(&server.url);
    for (status, _, kind, said) in rows {
        let error = route.open(call()).await.unwrap_err();
        assert!(!format!("{error:?}").contains("CANARY"), "{status}");
        let sentence = format!("{}{said}", kind.sentence("OpenAI"));
        assert_eq!((error.kind, error.message), (kind, sentence), "{status}");
    }
    let error = api(&format!("http://{refusing}"))
        .open(call())
        .await
        .unwrap_err();
    let failed = ErrorKind::Network.sentence("OpenAI");
    assert_eq!((error.kind, error.message), (ErrorKind::Network, failed));
}

const INSTRUCTIONS: &str = "You are a test voice on a short call. Only your helper knows today's \
secret word. When anyone asks for it, hand the request to your helper at once, then say only \
the helper's answer.";
const HELLO: &str = "Tell the learner that the call is up and that the test word is marmalade.";
const QUESTION: &str = "The learner asks: what is today's secret word?";
const ANSWER: &str = "Today's secret word is pineapple.";

type Clip = Vec<Vec<i16>>;

struct Dialed {
    leg: Box<dyn Leg>,
    control: Control,
    heard: mpsc::Receiver<AudioFrame>,
    port: Option<u16>,
    mic: tokio::task::JoinHandle<()>,
    played: Arc<std::sync::Mutex<VecDeque<Vec<i16>>>>,
    opened: Opened,
}

fn candidate_port(line: &str) -> Option<u16> {
    line.strip_prefix("a=candidate:")?
        .split_whitespace()
        .nth(5)?
        .parse()
        .ok()
}

async fn dial(route: &dyn VoiceRoute) -> Dialed {
    let mut leg = crate::voice::legs()();
    let offer = or_stop(leg.offer().await, "the offer");
    let port = offer.lines().find_map(candidate_port);
    let (instructions, voice) = (INSTRUCTIONS.into(), route.voices()[0].into());
    let call = OpenCall {
        offer,
        instructions,
        voice,
        language: None,
    };
    let opened = route
        .open(call)
        .await
        .unwrap_or_else(|e| panic!("{}", e.message));
    let id = CallId("live".into());
    let (speaking, lane) = mpsc::channel(8);
    let (down, heard) = mpsc::channel(4096);
    let up = Arc::new(Mutex::new(lane));
    let audio = LegAudio {
        call: id.clone(),
        up,
        down,
    };
    let control = leg.answer(opened.answer.clone(), audio).await;
    let control = or_stop(control, "the answer");
    let played = Arc::new(std::sync::Mutex::new(VecDeque::new()));
    let queued = Arc::clone(&played);
    let mic = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(FRAME_MS.into()));
        let silence = vec![0; AudioFormat::Pcm16Mono48k.frame_samples()];
        loop {
            let samples = queued.lock().unwrap().pop_front();
            let samples = samples.unwrap_or_else(|| silence.clone());
            let frame = AudioFrame {
                call: id.clone(),
                seq: 0,
                samples,
            };
            if speaking.send(frame).await.is_err() {
                break;
            }
            tick.tick().await;
        }
    });
    Dialed {
        leg,
        control,
        heard,
        port,
        mic,
        played,
        opened,
    }
}

pub(super) fn signed_in() -> OpenAiProvider {
    or_stop(
        OpenAiProvider::from_endpoint(&crate::test_guard::signed_in()),
        "the account's provider",
    )
}

async fn used(provider: &OpenAiProvider) -> Vec<(String, Option<f64>)> {
    let usage = provider.plan_usage().await.ok().flatten();
    let windows = usage.map(|u| u.windows).unwrap_or_default();
    let bucket = |w: UsageWindow| (w.key, w.used_percent.map(|p| p.value()));
    windows.into_iter().map(bucket).collect()
}

async fn lingering(port: u16) -> Option<(u32, f64, u32)> {
    let since = Instant::now();
    let mut socket = None;
    while socket.is_none() && since.elapsed() < Duration::from_secs(2) {
        tokio::time::sleep(Duration::from_millis(20)).await;
        socket = tokio::net::UdpSocket::bind(("0.0.0.0", port)).await.ok();
    }
    let socket = socket?;
    let (mut packets, mut last, mut alerts) = (0, 0.0, 0);
    let mut buffer = [0u8; 2048];
    let quiet = Duration::from_secs(10);
    while since.elapsed() < Duration::from_secs(120)
        && let Ok(Ok((size, _))) = tokio::time::timeout(quiet, socket.recv_from(&mut buffer)).await
    {
        packets += 1;
        last = since.elapsed().as_secs_f64();
        alerts += u32::from(size > 0 && buffer[0] == 21);
    }
    Some((packets, last, alerts))
}

#[tokio::test]
#[ignore = "live: two short voice calls on the ChatGPT subscription; only with the owner's approval"]
async fn live_call_on_the_subscription() {
    let provider = signed_in();
    let route = or_stop(provider.voice().ok_or(()), "the account's voice");
    let before = used(&provider).await;
    let mut call = dial(&*route).await;
    let out = call.control.out.clone();
    let send = move |frames: Vec<String>| {
        for frame in frames {
            let _ = out.send(frame);
        }
    };
    let speak = |text: &str| {
        let append = json!({"type": "session.context.append", "channel": "speakable",
            "content": [{"type": "input_text", "text": text}]});
        vec![append.to_string()]
    };
    send(speak(HELLO));
    let (mut events, mut unknown, mut delegations, mut replies) = (0, 0, 0, 0);
    let (mut spoken, mut echoed, mut answered) = (false, false, false);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    while !answered
        && let Ok(Some(frame)) = tokio::time::timeout_at(deadline, call.control.frames.recv()).await
    {
        events += 1;
        match route.read(&frame).remove(0) {
            LiveEvent::Said(text) if !spoken && text.to_lowercase().contains("marmalade") => {
                spoken = true;
                send(speak(QUESTION));
            }
            LiveEvent::Said(text) if spoken => {
                answered = text.to_lowercase().contains("pineapple");
                replies += u32::from(echoed || answered);
                echoed = true;
            }
            LiveEvent::Delegated { id, .. } => {
                delegations += 1;
                let text = ANSWER.into();
                send(route.write(&LiveSay::Reply {
                    delegation: Some(id),
                    text,
                }));
            }
            LiveEvent::Unknown(_) => unknown += 1,
            _ => {}
        }
    }
    let mut audio = 0;
    while call.heard.try_recv().is_ok() {
        audio += 1;
    }
    println!("voice live: {events} events, {unknown} unknown, {audio} audio frames");
    let handed = match (delegations, replies) {
        (0, 0) => "inconclusive, nothing was said after the question".to_owned(),
        (count, _) => count.to_string(),
    };
    println!("voice live: append spoken {spoken}, hand-offs {handed}, answer said {answered}");
    call.mic.abort();
    call.leg.close().await;
    let after_hang_up = match call.port {
        Some(port) => lingering(port).await,
        None => None,
    };
    println!("voice live: after the hang-up, (packets, last at s, alerts) {after_hang_up:?}");
    let mut ending = dial(&*route).await;
    let asked = Instant::now();
    let _ = ending
        .control
        .out
        .send(json!({"type": "session.close"}).to_string());
    let closing = async { while ending.control.frames.recv().await.is_some() {} };
    let ended = tokio::time::timeout(Duration::from_secs(10), closing).await;
    let ended = ended.ok().map(|()| asked.elapsed().as_millis());
    println!("voice live: session.close ended the second call after {ended:?} ms");
    ending.mic.abort();
    ending.leg.close().await;
    let after = used(&provider).await;
    let percent = |used: Option<f64>| used.map_or("none".to_owned(), |u| format!("{u}%"));
    let changed: Vec<String> = after
        .iter()
        .filter_map(|(key, now)| {
            let was = before.iter().find(|(k, _)| k == key).and_then(|(_, u)| *u);
            (was != *now).then(|| format!("{} -> {}", percent(was), percent(*now)))
        })
        .collect();
    let counts = (before.len(), after.len());
    println!("voice live: usage buckets {counts:?}, changed {changed:?}");
    assert!(audio > 0 && spoken, "no audio, or the append wasn't spoken");
}

const READS: [&str; 4] = [
    "Today's secret word is pineapple.",
    "Three quarters of the class said yes.",
    "I think it is",
    "seven.",
];
const LONG: [&str; 3] = [
    "Our first story is about a small red boat that sails across a quiet blue lake every morning.",
    "In the middle of the lake there is a little island with one tall tree and a family of ducks.",
    "At the end of the day the boat comes home and rests beside the old wooden dock until sunrise.",
];
const TRANSCRIBED: [&str; 2] = [
    "conversation.item.input_audio_transcription.completed",
    "conversation.item.input_audio_transcription.failed",
];

struct Live {
    route: Arc<dyn VoiceRoute>,
    call: Dialed,
    t0: Instant,
    got: Vec<(f64, Value)>,
    events: Vec<(f64, LiveEvent)>,
    sent: Vec<(f64, Value)>,
    down: Vec<(f64, Vec<i16>)>,
}

fn loud(samples: &[i16]) -> bool {
    samples.iter().any(|s| s.unsigned_abs() > 1_000)
}

fn ms(seconds: f64) -> i64 {
    (seconds * 1000.0).round() as i64
}

fn of<'a>(list: &'a [(f64, Value)], kinds: &[&str], from: f64) -> Vec<(f64, &'a Value)> {
    let found = list
        .iter()
        .filter(|(at, f)| *at >= from && kinds.iter().any(|k| f["type"] == *k));
    found.map(|(at, f)| (*at, f)).collect()
}

impl Live {
    fn at(&self) -> f64 {
        self.t0.elapsed().as_secs_f64()
    }

    fn send(&mut self, frames: Vec<String>) {
        for frame in frames {
            let at = self.at();
            self.sent.push((at, serde_json::from_str(&frame).unwrap()));
            let _ = self.call.control.out.send(frame);
        }
    }

    async fn pump(&mut self, seconds: f64, done: impl Fn(&Live) -> bool) {
        let until = tokio::time::Instant::now() + Duration::from_secs_f64(seconds.max(0.0));
        while !done(self) {
            tokio::select! {
                frame = self.call.control.frames.recv() => {
                    let Some(frame) = frame else { return };
                    let at = self.at();
                    self.got.push((at, serde_json::from_str(&frame).unwrap_or_default()));
                    for event in self.route.read(&frame) {
                        match event {
                            LiveEvent::Send(frame) => self.send(vec![frame]),
                            event => self.events.push((at, event)),
                        }
                    }
                }
                audio = self.call.heard.recv() => {
                    let Some(audio) = audio else { return };
                    let at = self.at();
                    self.down.push((at, audio.samples));
                }
                () = tokio::time::sleep_until(until) => return,
            }
        }
    }

    fn timeline(&self, from: f64, kinds: &[&str]) -> Vec<(String, i64)> {
        let mut all = of(&self.got, kinds, from);
        all.extend(of(&self.sent, kinds, from));
        all.sort_by(|a, b| a.0.total_cmp(&b.0));
        let label = |f: &Value| {
            let detail = [&f["response"]["status"], &f["error"]["code"]];
            let detail = detail.iter().find_map(|d| d.as_str()).unwrap_or_default();
            format!("{} {detail}", f["type"].as_str().unwrap_or_default())
        };
        all.iter()
            .map(|(at, f)| (label(f), ms(at - from)))
            .collect()
    }

    fn sound(&self, from: f64) -> Option<f64> {
        let found = self.down.iter().find(|(at, s)| *at >= from && loud(s));
        found.map(|(at, _)| *at)
    }

    fn runs(&self, from: f64) -> Vec<(i64, i64)> {
        let mut runs: Vec<(f64, f64)> = Vec::new();
        for (at, _) in self.down.iter().filter(|(at, s)| *at >= from && loud(s)) {
            match runs.last_mut() {
                Some(run) if *at - run.1 < 0.25 => run.1 = *at,
                _ => runs.push((*at, *at)),
            }
        }
        runs.iter()
            .map(|r| (ms(r.0 - from), ms(r.1 - from)))
            .collect()
    }

    fn settled(&self, from: f64) -> bool {
        let count = |kinds: &[&str]| of(&self.got, kinds, from).len();
        let made = of(&self.sent, &["response.create"], from).len();
        let started = count(&["input_audio_buffer.speech_started"]);
        let heard = of(&self.got, &TRANSCRIBED, from)
            .into_iter()
            .map(|(at, _)| at);
        let sounds = self.down.iter().filter(|(at, s)| *at >= from && loud(s));
        let last = heard.chain(sounds.map(|(at, _)| *at)).fold(from, f64::max);
        let answered = made == count(&["response.done"]) && started <= count(&TRANSCRIBED);
        made + started > 0 && answered && self.at() - last > 1.5
    }

    fn lengths(&self, from: f64) -> Vec<f64> {
        let done = of(&self.got, &["response.done"], from);
        let audio =
            |f: &Value| f["response"]["usage"]["output_token_details"]["audio_tokens"].as_f64();
        done.iter()
            .map(|(_, f)| audio(f).unwrap_or_default() / 20.0)
            .collect()
    }

    fn index(texts: &[&str], text: Option<&str>) -> Option<usize> {
        texts.iter().position(|t| Some(*t) == text)
    }

    fn clip(&self, from: f64, lengths: &[f64], index: usize) -> Clip {
        let frames: Vec<&Vec<i16>> = self
            .down
            .iter()
            .filter(|(at, _)| *at >= from)
            .map(|(_, s)| s)
            .collect();
        let start = frames.iter().position(|s| loud(s)).unwrap_or_default();
        let edge = |n: usize| start + (lengths[..n].iter().sum::<f64>() * 50.0).round() as usize;
        let part = &frames[edge(index).min(frames.len())..edge(index + 1).min(frames.len())];
        let first = part.iter().position(|s| loud(s)).unwrap_or_default();
        let last = part.iter().rposition(|s| loud(s)).map_or(0, |l| l + 1);
        part[first..last.max(first)]
            .iter()
            .map(|s| (*s).clone())
            .collect()
    }

    async fn speak(&mut self, step: &str, texts: &[&str], clip: Clip) {
        let from = self.at();
        for text in texts {
            let say = LiveSay::Reply {
                delegation: None,
                text: (*text).into(),
            };
            let frames = self.route.write(&say);
            self.send(frames);
        }
        let playing = if texts.is_empty() {
            None
        } else {
            self.pump(10.0, |l| l.lengths(from).len() >= 2).await;
            let lengths = self.lengths(from);
            let first = or_stop(self.sound(from).ok_or(()), "the first sound");
            self.pump(first + lengths[0] + lengths[1] / 2.0 - self.at(), |_| false)
                .await;
            Some((first, lengths))
        };
        let end = self.at() + clip.len() as f64 * 0.02;
        let onset = self.play(clip);
        self.pump(40.0, |l| l.settled(from)).await;
        if let Some((first, lengths)) = playing {
            let played = self
                .down
                .iter()
                .filter(|(at, _)| *at >= first && *at < onset)
                .count();
            let known: Vec<Option<f64>> = lengths.iter().copied().map(Some).collect();
            let at = realtime::playing(&known, played as f64 * 0.02);
            println!(
                "api live {step}: reply {at} on the down lane at onset; sound after onset {:?} ms",
                self.runs(onset)
            );
            let kinds = [
                "input_audio_buffer.speech_started",
                "input_audio_buffer.committed",
                TRANSCRIBED[0],
                TRANSCRIBED[1],
                "response.cancel",
                "output_audio_buffer.clear",
                "output_audio_buffer.cleared",
                "response.done",
                "error",
            ];
            println!(
                "api live {step}: after onset {:?} ms",
                self.timeline(onset, &kinds)
            );
            let made = of(&self.sent, &["response.create"], onset);
            let made: Vec<Option<usize>> = made
                .iter()
                .map(|(_, f)| {
                    Self::index(
                        texts,
                        f["response"]["input"][0]["content"][0]["text"].as_str(),
                    )
                })
                .collect();
            let cuts: Vec<Option<usize>> = self
                .events
                .iter()
                .filter(|(at, _)| *at >= onset)
                .filter_map(|(_, e)| match e {
                    LiveEvent::Interrupted { cut } => Some(Self::index(texts, cut.as_deref())),
                    _ => None,
                })
                .collect();
            println!("api live {step}: cut {cuts:?}, replies made after onset {made:?}");
        }
        let heard = self
            .events
            .iter()
            .filter(|(at, _)| *at >= onset)
            .filter_map(|(at, e)| match e {
                LiveEvent::Heard(text) => Some((ms(at - end), text.as_str())),
                _ => None,
            });
        println!(
            "api live {step}: heard (ms after the clip's end) {:?}",
            heard.collect::<Vec<_>>()
        );
    }

    fn play(&self, clip: Clip) -> f64 {
        self.call.played.lock().unwrap().extend(clip);
        self.at()
    }
}

#[tokio::test]
#[ignore = "live: one short call on the OpenAI API, about $0.10; only with the owner's approval"]
async fn live_call_on_the_api() {
    let key = or_stop(
        std::env::var("WISP_LIVE_OPENAI_KEY"),
        "reading WISP_LIVE_OPENAI_KEY",
    );
    let endpoint = endpoint(Credential::ApiKey(key), DEFAULT_BASE_URL);
    let provider = or_stop(OpenAiProvider::from_endpoint(&endpoint), "the provider");
    let route = or_stop(provider.voice().ok_or(()), "the account's voice");
    let t0 = Instant::now();
    let call = dial(&*route).await;
    let port = call.port;
    let (got, events, sent, down) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut live = Live {
        route: Arc::clone(&route),
        call,
        t0,
        got,
        events,
        sent,
        down,
    };
    live.pump(10.0, |l| !of(&l.got, &["session.updated"], 0.0).is_empty())
        .await;
    if let Some((_, frame)) = of(&live.got, &["session.updated"], 0.0).first() {
        let (session, input) = (&frame["session"], &frame["session"]["audio"]["input"]);
        println!(
            "api live 1: turn detection {}, reasoning {}, transcription {}",
            input["turn_detection"], session["reasoning"], input["transcription"]
        );
    }

    let two = live.at();
    for text in READS {
        let frames = live.route.write(&LiveSay::Reply {
            delegation: None,
            text: text.into(),
        });
        live.send(frames);
    }
    live.pump(40.0, |l| l.settled(two)).await;
    let made = of(&live.sent, &["response.create"], two);
    let first: Vec<Option<i64>> = made
        .iter()
        .map(|(at, _)| live.sound(*at).map(|s| ms(s - at)))
        .collect();
    let kinds = [
        "response.create",
        "response.done",
        "output_audio_buffer.started",
        "output_audio_buffer.stopped",
    ];
    println!(
        "api live 2: {:?} ms; first sound after each create {first:?} ms",
        live.timeline(two, &kinds)
    );
    let lengths = live.lengths(two);
    println!(
        "api live 2: sound {:?} ms; lengths from tokens {lengths:?} s",
        live.runs(two)
    );
    let clips: Vec<Clip> = (0..READS.len())
        .map(|n| live.clip(two, &lengths, n))
        .collect();

    let pause = vec![vec![0; AudioFormat::Pcm16Mono48k.frame_samples()]; 40];
    let split = [clips[2].clone(), pause, clips[3].clone()].concat();
    live.speak("3", &[], split).await;
    live.speak("3, a short answer", &[], clips[1].clone()).await;
    live.speak("4", &LONG, clips[0][..25.min(clips[0].len())].to_vec())
        .await;
    live.speak("5", &LONG, clips[0].clone()).await;

    live.call.mic.abort();
    live.call.leg.close().await;
    let seconds = live.t0.elapsed().as_secs();
    let closed = route.close(&live.call.opened).await.is_ok();
    let after = match port {
        Some(port) => lingering(port).await,
        None => None,
    };
    println!("api live 6: hang-up ok {closed}; after it (packets, last at s, alerts) {after:?}");

    let created = of(&live.got, &["response.created"], 0.0).len();
    let made = of(&live.sent, &["response.create"], 0.0).len();
    let (mut unknown, mut said, mut spent) = (BTreeMap::new(), Vec::new(), Vec::new());
    for (_, event) in &live.events {
        match event {
            LiveEvent::Unknown(kind) => *unknown.entry(kind.clone()).or_insert(0) += 1,
            LiveEvent::Said(text) => said.push(text.clone()),
            LiveEvent::Spent(s) => spent.push(*s),
            _ => {}
        }
    }
    let cost = crate::pricing::voice_usd(route.model(), seconds, &spent);
    println!(
        "api live 7: {created} created for {made} sent; unknown {unknown:?}; said differently {said:?}"
    );
    println!("api live 7: {seconds} s, cost {cost:?} USD");
    assert!(live.sound(0.0).is_some(), "no sound came");
}
