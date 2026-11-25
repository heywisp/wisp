#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "offline CLI fixtures fail on unexpected I/O"
)]

use crate::support;
use serde_json::{Value, json};
use std::io::{BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use support::home::Home;
use support::prefix::assert_gemini_prefix_stable;
use support::pty::{self, Pty};
use support::server::{is_naming, not_named, read_request};
use wisp_protocol::agents::MAIL_OPEN;
use wisp_tool_text::{CONTEXT_PREFIX, SENT_PREFIX};

const TEST_KEY: &str = "AIzaCANARY_gemini_cli_fixture_key";

pub(super) fn sse_chunk(model: &str, parts: Vec<Value>, finish: Option<&str>) -> String {
    let mut candidate = json!({
        "content": {
            "role": "model",
            "parts": parts
        }
    });
    if let Some(reason) = finish {
        candidate["finishReason"] = json!(reason);
    }
    let frame = json!({
        "candidates": [candidate],
        "modelVersion": format!("models/{model}"),
        "usageMetadata": {
            "promptTokenCount": 120,
            "cachedContentTokenCount": 40,
            "candidatesTokenCount": 16,
            "thoughtsTokenCount": 8
        }
    });
    format!("data: {frame}\n\n")
}

fn serve(replies: Vec<String>) -> (String, Arc<Mutex<Vec<String>>>, std::thread::JoinHandle<()>) {
    serve_model(
        "gemini-4-carbon",
        replies.into_iter().map(|body| ("200 OK", body)).collect(),
    )
}

fn serve_model(
    model: &'static str,
    replies: Vec<(&'static str, String)>,
) -> (String, Arc<Mutex<Vec<String>>>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&seen);
    let thread = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(45);
        for (status, body) in replies {
            let (mut socket, request) = loop {
                let socket = match listener.accept() {
                    Ok((socket, _)) => socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() > deadline {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(error) => panic!("accept: {error}"),
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let mut reader = BufReader::new(socket.try_clone().unwrap());
                let request = read_request(&mut reader).unwrap();
                if !is_naming(&request.body) {
                    break (socket, request);
                }
                let refused = not_named().replacen("\r\n", "\r\nConnection: close\r\n", 1);
                let mut s = socket;
                let _ = s.write_all(refused.as_bytes());
            };
            assert!(
                request.head.starts_with(&format!(
                    "POST /v1beta/models/{model}:streamGenerateContent?alt=sse "
                )),
                "{}",
                request.head
            );
            assert!(!request.head.contains("key="), "{}", request.head);
            let lower = request.head.to_ascii_lowercase();
            assert!(
                lower.contains(&format!(
                    "x-goog-api-key: {}",
                    TEST_KEY.to_ascii_lowercase()
                )),
                "{}",
                request.head
            );
            captured.lock().unwrap().push(request.body);
            let content_type = if status == "200 OK" {
                "text/event-stream"
            } else {
                "application/json"
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            for chunk in response.as_bytes().chunks(11) {
                if socket.write_all(chunk).is_err() {
                    break;
                }
            }
        }
    });
    (format!("http://{address}"), seen, thread)
}

fn command(home: &Path, config: &Path) -> Command {
    let quiet = [
        "--time",
        "off",
        "--no-auto-compact",
        "--context-note",
        "off",
    ];
    command_with(home, config, &quiet)
}

fn command_with(home: &Path, config: &Path, lines: &[&str]) -> Command {
    let mut cmd = Pty::command(home);
    cmd.env_remove("LANG")
        .env("WORK_GEMINI_KEY", TEST_KEY)
        .arg("--config")
        .arg(config)
        .args([
            "--output",
            "tui",
            "--endpoint",
            "gemini",
            "--instructions",
            "none",
        ])
        .args(lines)
        .args(["--effort", "high", "--tools", "bash,view_file,web_search"]);
    cmd
}

fn write_config_named(home: &Path, name: &str, url: &str, extra: &str) -> std::path::PathBuf {
    let path = home.join(name);
    std::fs::write(
        &path,
        format!(
            "default = \"gemini\"\n[endpoints.gemini]\nprovider = \"gemini\"\nauth = \"api_key\"\nkey_env = \"WORK_GEMINI_KEY\"\nurl = \"{url}\"\n{extra}"
        ),
    )
    .unwrap();
    path
}

fn write_config(home: &Path, url: &str) -> std::path::PathBuf {
    write_config_named(home, "config.toml", url, "")
}

#[test]
fn cli_search_continues_and_resumes_with_the_same_chat_prefix_and_tool_results() {
    let home = Home::new("gemini-search");
    let call = sse_chunk(
        "gemini-4-carbon",
        vec![json!({
            "functionCall": {"id": "s1", "name": "web_search", "args": {"query": "example query"}},
            "thoughtSignature": "placeholder",
        })],
        Some("STOP"),
    );
    let mut found = sse_chunk(
        "gemini-4-carbon",
        vec![json!({"text": "Facts found"})],
        Some("STOP"),
    );
    let mut chunk: Value =
        serde_json::from_str(found.trim().strip_prefix("data: ").unwrap()).unwrap();
    chunk["candidates"][0]["groundingMetadata"] = json!({
        "webSearchQueries": ["google first query", "google second query"],
        "groundingChunks": [{"web": {"uri": "https://example.com/source", "title": "Example"}}],
    });
    found = format!("data: {chunk}\n\n");
    let answer = sse_chunk(
        "gemini-4-carbon",
        vec![json!({"text": "GEMINI-SEARCH-ANSWER"})],
        Some("STOP"),
    );
    let next = sse_chunk(
        "gemini-4-carbon",
        vec![json!({"text": "GEMINI-SEARCH-RESUMED"})],
        Some("STOP"),
    );
    let (url, seen, server) = serve(vec![call, found, answer, next]);
    let config = write_config(&home.0, &url);
    let first = pty::run(
        &mut command(&home.0, &config),
        "Search now\n",
        "GEMINI-SEARCH-ANSWER",
        TEST_KEY,
    );
    assert!(first.contains("GEMINI-SEARCH-ANSWER"));
    assert!(first.contains("google first query"), "{first}");
    assert!(first.contains("1 result"), "{first}");
    assert!(first.contains("Sources"), "{first}");
    assert!(first.contains("https://example.com/source"), "{first}");
    pty::run(
        command(&home.0, &config).arg("--resume"),
        "Keep going\n",
        "GEMINI-SEARCH-RESUMED",
        TEST_KEY,
    );
    server.join().unwrap();
    let bodies = seen.lock().unwrap().clone();
    let requests: Vec<Value> = bodies
        .iter()
        .map(|body| serde_json::from_str(body).unwrap())
        .collect();
    super::golden_requests::check_body("gemini_search_on", &bodies[0]);
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[1]["tools"], json!([{"googleSearch": {}}]));
    assert_eq!(
        requests[1]["contents"],
        json!([{"role": "user", "parts": [{"text": "example query"}]}])
    );

    let chat = [
        requests[0].clone(),
        requests[2].clone(),
        requests[3].clone(),
    ];
    assert_gemini_prefix_stable(&chat);
    let turns = requests[2]["contents"].as_array().unwrap();
    let model = turns
        .iter()
        .position(|turn| turn["role"] == "model")
        .unwrap();
    assert_eq!(turns[model]["parts"][0]["functionCall"]["id"], "s1");
    assert_eq!(turns[model]["parts"][0]["thoughtSignature"], "placeholder");
    assert_eq!(turns[model + 1]["role"], "user");
    assert_eq!(turns[model + 1]["parts"][0]["functionResponse"]["id"], "s1");
    assert!(
        turns[model + 1]["parts"][0]["functionResponse"]["response"]["output"]
            .as_str()
            .unwrap()
            .contains("Facts found")
    );
    assert!(!requests[2].to_string().contains("\"sources\""));
}

#[test]
fn cli_resumed_gemini_chat_picks_up_search_once_and_keeps_prefix_after() {
    let home = Home::new("gemini-search-pickup");
    let r1 = sse_chunk(
        "gemini-4-carbon",
        vec![json!({"text": "GEM-ONE"})],
        Some("STOP"),
    );
    let r2 = sse_chunk(
        "gemini-4-carbon",
        vec![json!({"text": "GEM-TWO"})],
        Some("STOP"),
    );
    let r3 = sse_chunk(
        "gemini-4-carbon",
        vec![json!({"text": "GEM-THREE"})],
        Some("STOP"),
    );
    let (url, seen, server) = serve(vec![r1, r2, r3]);
    let old_cfg = write_config_named(
        &home.0,
        "old-config.toml",
        &url,
        "[models.\"gemini-4-carbon\"]\nweb_search = []\n",
    );
    let new_cfg = write_config(&home.0, &url);
    pty::run(
        &mut command(&home.0, &old_cfg),
        "First\n",
        "GEM-ONE",
        TEST_KEY,
    );
    let resumed = pty::run(
        command(&home.0, &new_cfg).arg("--resume"),
        "Second\n",
        "GEM-TWO",
        TEST_KEY,
    );
    assert!(resumed.contains("tools added: web_search"), "{resumed}");
    let again = pty::run(
        command(&home.0, &new_cfg).arg("--resume"),
        "Third\n",
        "GEM-THREE",
        TEST_KEY,
    );
    assert_eq!(
        again.matches("tools added: web_search").count(),
        1,
        "{again}"
    );
    server.join().unwrap();
    let requests: Vec<Value> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|body| serde_json::from_str(body).unwrap())
        .collect();
    assert_eq!(requests.len(), 3);
    let has_search = |req: &Value| {
        req["tools"][0]["functionDeclarations"]
            .as_array()
            .is_some_and(|decls| decls.iter().any(|d| d["name"] == "web_search"))
    };
    assert!(!has_search(&requests[0]));
    assert!(has_search(&requests[1]));
    assert!(has_search(&requests[2]));
    assert_gemini_prefix_stable(&[requests[1].clone(), requests[2].clone()]);
}

#[test]
fn cli_runs_two_resumed_gemini_tool_turns_with_stable_prefix_and_distinct_idless_calls() {
    let home = Home::new("gemini-tool");
    let turn1_call = sse_chunk(
        "gemini-4-carbon",
        vec![
            json!({"text": "planning first step", "thought": true, "thoughtSignature": "sig_t1_thought"}),
            json!({
                "functionCall": {
                    "name": "bash",
                    "args": {"command": "printf step1 >> tool-log; printf bash-out"}
                },
                "thoughtSignature": "sig_t1_call"
            }),
        ],
        Some("STOP"),
    );
    let turn1_done = sse_chunk(
        "gemini-4-carbon",
        vec![json!({"text": "GEMINI-FIXTURE-TURN-1"})],
        Some("STOP"),
    );
    let turn2_call = sse_chunk(
        "gemini-4-carbon",
        vec![
            json!({"text": "reading tool-log", "thought": true, "thoughtSignature": "sig_t2_thought"}),
            json!({
                "functionCall": {
                    "name": "view_file",
                    "args": {"path": "tool-log"}
                },
                "thoughtSignature": "sig_t2_call"
            }),
        ],
        Some("STOP"),
    );
    let turn2_done = sse_chunk(
        "gemini-4-carbon",
        vec![json!({"text": "GEMINI-FIXTURE-RESUMED"})],
        Some("STOP"),
    );

    let (url, seen, server) = serve(vec![turn1_call, turn1_done, turn2_call, turn2_done]);
    let cfg = write_config(&home.0, &url);

    let first = pty::run(
        &mut command(&home.0, &cfg),
        "Start work\n",
        "GEMINI-FIXTURE-TURN-1",
        TEST_KEY,
    );
    assert!(first.contains("GEMINI-FIXTURE-TURN-1"), "{first}");
    assert_eq!(
        std::fs::read_to_string(home.0.join("tool-log")).unwrap(),
        "step1"
    );

    let second = pty::run(
        command(&home.0, &cfg).arg("--resume"),
        "Read log\n",
        "GEMINI-FIXTURE-RESUMED",
        TEST_KEY,
    );
    assert!(second.contains("GEMINI-FIXTURE-RESUMED"), "{second}");
    server.join().unwrap();

    let bodies = seen.lock().unwrap().clone();
    let requests: Vec<Value> = bodies
        .iter()
        .map(|body| serde_json::from_str(body).unwrap())
        .collect();
    assert_eq!(requests.len(), 4);
    assert_gemini_prefix_stable(&requests);

    let top_keys: Vec<&str> = requests[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        top_keys,
        ["contents", "generationConfig", "systemInstruction", "tools"]
    );
    assert_eq!(
        requests[0]["generationConfig"]["stopSequences"],
        json!([
            format!("\nuser\n{SENT_PREFIX}"),
            format!("\nuser\n{CONTEXT_PREFIX}"),
            format!("\nuser\n{MAIL_OPEN}"),
            format!("\n{MAIL_OPEN} from=\""),
        ])
    );
    assert_eq!(
        requests[0]["generationConfig"]["thinkingConfig"],
        json!({"includeThoughts": true, "thinkingLevel": "HIGH"})
    );
    let decls = requests[0]["tools"][0]["functionDeclarations"]
        .as_array()
        .unwrap();
    let names: Vec<&str> = decls.iter().filter_map(|d| d["name"].as_str()).collect();
    assert!(
        names.contains(&"bash") && names.contains(&"view_file"),
        "{names:?}"
    );
    for decl in decls {
        assert!(decl.get("parametersJsonSchema").is_some(), "{decl}");
        assert!(decl.get("parameters").is_none(), "{decl}");
        assert!(decl["parametersJsonSchema"].get("$schema").is_none());
    }

    let final_contents = requests[3]["contents"].as_array().unwrap();
    let responses: Vec<&str> = final_contents
        .iter()
        .flat_map(|c| c["parts"].as_array().into_iter().flatten())
        .filter_map(|p| p["functionResponse"]["name"].as_str())
        .collect();
    assert_eq!(responses, ["bash", "view_file"]);

    let sigs: Vec<&str> = final_contents
        .iter()
        .flat_map(|c| c["parts"].as_array().into_iter().flatten())
        .filter_map(|p| p["thoughtSignature"].as_str())
        .collect();
    assert_eq!(
        sigs,
        [
            "sig_t1_thought",
            "sig_t1_call",
            "sig_t2_thought",
            "sig_t2_call"
        ]
    );
}

#[test]
fn cli_neon_turns_end_in_the_data_part_and_send_five_stop_strings_across_a_resume() {
    let home = Home::new("gemini-neon-form");
    let model = "gemini-4-neon";
    let reply = |text: &str| sse_chunk(model, vec![json!({"text": text})], Some("STOP"));
    let (url, seen, server) = serve_model(
        model,
        vec![
            ("200 OK", reply("NEON-FORM-ONE")),
            ("200 OK", reply("NEON-FORM-TWO")),
        ],
    );
    let cfg = write_config(&home.0, &url);
    let lines = ["--time", "local", "--context-note", "on"];
    let first = pty::run(
        command_with(&home.0, &cfg, &lines).args(["--model", model]),
        "Start work\n",
        "NEON-FORM-ONE",
        TEST_KEY,
    );
    assert!(first.contains("NEON-FORM-ONE"), "{first}");
    let second = pty::run(
        command_with(&home.0, &cfg, &lines).arg("--resume"),
        "Carry on\n",
        "NEON-FORM-TWO",
        TEST_KEY,
    );
    assert!(second.contains("NEON-FORM-TWO"), "{second}");
    server.join().unwrap();

    let requests: Vec<Value> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|body| serde_json::from_str(body).unwrap())
        .collect();
    assert_eq!(requests.len(), 2);
    assert_gemini_prefix_stable(&requests);
    let form = wisp_core::models::ModelTable::shared()
        .turn_form(model)
        .unwrap();
    let stops = json!([
        format!("\nuser\n{SENT_PREFIX}"),
        format!("\nuser\n{CONTEXT_PREFIX}"),
        format!("\nuser\n{MAIL_OPEN}"),
        format!("\n{MAIL_OPEN} from=\""),
        "\n<ADDITIONAL_METADATA>",
    ]);
    for request in &requests {
        assert_eq!(request["generationConfig"]["stopSequences"], stops);
        let turns = request["contents"].as_array().unwrap();
        for turn in turns.iter().filter(|c| c["role"] == "user") {
            let texts: Vec<&str> = turn["parts"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|p| p["text"].as_str())
                .collect();
            let bare = |t: &&str| t.starts_with(SENT_PREFIX) || t.starts_with(CONTEXT_PREFIX);
            assert!(!texts.iter().any(bare), "{texts:?}");
            let data = texts.last().unwrap();
            let inner = data
                .strip_prefix(form.data_open.as_str())
                .and_then(|t| t.strip_suffix(form.data_close.as_str()))
                .unwrap();
            let lines: Vec<&str> = inner.lines().collect();
            assert!(
                lines[0].starts_with("The current local time is: "),
                "{data}"
            );
            assert!(lines[1].starts_with("Context window: "), "{data}");
            assert!(lines[1].contains("; Wisp compacts at "), "{data}");
            assert_eq!(lines.len(), 2, "{data}");
        }
    }
}

#[test]
fn cli_neon_reply_cut_at_the_cap_is_resent_with_the_full_limit_and_a_stable_prefix() {
    let home = Home::new("gemini-neon-cap");
    let model = "gemini-4-neon";
    let cap = wisp_core::models::ModelTable::shared()
        .reply_cap(model)
        .unwrap();
    let full = wisp_core::agent::DEFAULT_MAX_TOKENS;
    let cut = sse_chunk(
        model,
        vec![json!({"text": "NEON-CUT-DRAFT"})],
        Some("MAX_TOKENS"),
    );
    let step1_call = sse_chunk(
        model,
        vec![json!({
            "functionCall": {"name": "bash", "args": {"command": "printf neon-ok"}},
            "thoughtSignature": "sig_neon_call"
        })],
        Some("STOP"),
    );
    let step2_done = sse_chunk(
        model,
        vec![json!({"text": "NEON-CAP-RECOVERED"})],
        Some("STOP"),
    );
    let (url, seen, server) = serve_model(
        model,
        vec![
            ("200 OK", cut),
            ("200 OK", step1_call),
            ("200 OK", step2_done),
        ],
    );
    let cfg = write_config(&home.0, &url);
    let out = pty::run(
        command(&home.0, &cfg).args(["--model", model]),
        "Run command\n",
        "NEON-CAP-RECOVERED",
        TEST_KEY,
    );
    assert!(out.contains("NEON-CAP-RECOVERED"), "{out}");
    server.join().unwrap();

    let requests: Vec<Value> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|body| serde_json::from_str(body).unwrap())
        .collect();
    assert_eq!(requests.len(), 3);
    let limits: Vec<u64> = requests
        .iter()
        .map(|r| r["generationConfig"]["maxOutputTokens"].as_u64().unwrap())
        .collect();
    assert_eq!(limits, [u64::from(cap), u64::from(full), u64::from(cap)]);
    let without_limit = |request: &Value| {
        let mut copy = request.clone();
        copy["generationConfig"]
            .as_object_mut()
            .unwrap()
            .remove("maxOutputTokens");
        copy
    };
    assert_eq!(without_limit(&requests[1]), without_limit(&requests[0]));
    assert!(!requests[1].to_string().contains("NEON-CUT-DRAFT"));
    assert!(!requests[2].to_string().contains("NEON-CUT-DRAFT"));
    let responses: Vec<&str> = requests[2]["contents"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|c| c["parts"].as_array().into_iter().flatten())
        .filter_map(|p| p["functionResponse"]["response"]["output"].as_str())
        .collect();
    assert_eq!(responses.len(), 1, "{responses:?}");
    assert!(responses[0].contains("neon-ok"), "{responses:?}");
    assert_gemini_prefix_stable(&[without_limit(&requests[1]), without_limit(&requests[2])]);
}

#[test]
fn cli_malformed_gemini_tool_call_retries_with_folded_note_and_stable_prefix() {
    let home = Home::new("gemini-malformed-note");
    let note = wisp_core::agent::retry::MALFORMED_CALL_NOTE;
    let step1_call = sse_chunk(
        "gemini-4-carbon",
        vec![json!({
            "functionCall": {"name": "bash", "args": {"command": "printf ok"}},
            "thoughtSignature": "sig_call_1"
        })],
        Some("STOP"),
    );
    let malformed = sse_chunk("gemini-4-carbon", vec![], Some("MALFORMED_FUNCTION_CALL"));
    let step2_done = sse_chunk(
        "gemini-4-carbon",
        vec![json!({"text": "GEMINI-MALFORMED-RECOVERED"})],
        Some("STOP"),
    );
    let resumed_done = sse_chunk(
        "gemini-4-carbon",
        vec![json!({"text": "GEMINI-MALFORMED-RESUMED"})],
        Some("STOP"),
    );
    let (url, seen, server) = serve(vec![
        step1_call,
        malformed.clone(),
        malformed,
        step2_done,
        resumed_done,
    ]);
    let cfg = write_config(&home.0, &url);
    let first = pty::run(
        &mut command(&home.0, &cfg),
        "Run command\n",
        "GEMINI-MALFORMED-RECOVERED",
        TEST_KEY,
    );
    assert!(first.contains("GEMINI-MALFORMED-RECOVERED"), "{first}");
    let second = pty::run(
        command(&home.0, &cfg).arg("--resume"),
        "Continue\n",
        "GEMINI-MALFORMED-RESUMED",
        TEST_KEY,
    );
    assert!(second.contains("GEMINI-MALFORMED-RESUMED"), "{second}");
    assert!(!second.contains("improperly formatted"), "{second}");
    server.join().unwrap();

    let requests: Vec<Value> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|body| serde_json::from_str(body).unwrap())
        .collect();
    assert_eq!(requests.len(), 5);
    assert!(!requests[1].to_string().contains(note));
    assert_eq!(
        requests[2]["contents"].as_array().unwrap()[..2],
        requests[1]["contents"].as_array().unwrap()[..2]
    );
    let folded = requests[2]["contents"][2]["parts"][0]["functionResponse"]["response"]["output"]
        .as_str()
        .unwrap();
    assert_eq!(folded.matches(note).count(), 1, "{folded}");
    assert_eq!(requests[3], requests[2]);
    assert_gemini_prefix_stable(&[
        requests[0].clone(),
        requests[2].clone(),
        requests[4].clone(),
    ]);
}

#[test]
fn cli_gemini_views_audio_and_a_refused_file_is_withdrawn_with_a_stable_prefix() {
    const MODEL: &str = "gemini-4-carbon";
    const WAV: &[u8] = b"RIFF\x00\x00\x00\x00WAVEfmt ";
    const PDF: &[u8] = b"%PDF-1.4\n%%EOF\n";
    let home = Home::new("gemini-attachment-withdrawal");
    std::fs::write(home.0.join("beep.wav"), WAV).unwrap();
    std::fs::write(home.0.join("manual.pdf"), PDF).unwrap();
    let call = |args, signature| {
        json!({
            "functionCall": {"name": "view_file", "args": args},
            "thoughtSignature": signature
        })
    };
    let accepted = sse_chunk(
        MODEL,
        vec![
            call(json!({"path": "beep.wav"}), "sig_wav"),
            call(json!({"path": "manual.pdf", "pages": "1-2"}), "sig_pdf"),
        ],
        Some("STOP"),
    );
    let rejected = sse_chunk(
        MODEL,
        vec![call(
            json!({"path": "manual.pdf", "pages": "99"}),
            "sig_bad_range",
        )],
        Some("STOP"),
    );
    let refusal = json!({"error": {
        "code": 400, "status": "INVALID_ARGUMENT",
        "message": "Could not process file: Invalid page range: endPage exceeds document length"
    }});
    let reply = |text| sse_chunk(MODEL, vec![json!({"text": text})], Some("STOP"));
    let (url, seen, server) = serve_model(
        MODEL,
        vec![
            ("200 OK", accepted),
            ("200 OK", rejected),
            ("400 Bad Request", refusal.to_string()),
            ("200 OK", reply("GEMINI-FILE-RECOVERED")),
            ("200 OK", reply("GEMINI-FILE-RESUMED")),
        ],
    );
    let cfg = write_config(&home.0, &url);
    let first = pty::run(
        &mut command(&home.0, &cfg),
        "Read beep.wav\n",
        "GEMINI-FILE-RECOVERED",
        TEST_KEY,
    );
    assert!(first.contains("GEMINI-FILE-RECOVERED"), "{first}");
    let second = pty::run(
        command(&home.0, &cfg).arg("--resume"),
        "Continue\n",
        "GEMINI-FILE-RESUMED",
        TEST_KEY,
    );
    assert!(second.contains("GEMINI-FILE-RESUMED"), "{second}");
    server.join().unwrap();

    let requests: Vec<Value> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|body| serde_json::from_str(body).unwrap())
        .collect();
    let [initial, committed, refused, retry, resumed] = requests.as_slice() else {
        panic!("expected five requests, got {}", requests.len());
    };
    assert_gemini_prefix_stable(&[
        initial.clone(),
        committed.clone(),
        retry.clone(),
        resumed.clone(),
    ]);
    for key in ["tools", "systemInstruction", "generationConfig"] {
        assert_eq!(refused[key], retry[key], "{key}");
    }
    let pdf = |start, end| {
        json!({
            "inlineData": {"mimeType": "application/pdf", "data": "JVBERi0xLjQKJSVFT0YK"},
            "documentMetadata": {"startPage": start, "endPage": end}
        })
    };
    for request in [committed, refused, retry, resumed] {
        let parts = request["contents"][2]["parts"].as_array().unwrap();
        assert!(
            parts
                .iter()
                .any(|part| part["functionResponse"]["parts"] == json!([pdf(1, 2)])),
            "{parts:#?}"
        );
        assert!(
            parts.contains(&json!({"inlineData": {
                "mimeType": "audio/wav", "data": "UklGRgAAAABXQVZFZm10IA=="
            }})),
            "{parts:#?}"
        );
    }
    let refused = refused["contents"].as_array().unwrap();
    let retry = retry["contents"].as_array().unwrap();
    assert_eq!(&refused[..4], &retry[..4]);
    let before = refused[4]["parts"].as_array().unwrap();
    assert_eq!(before.len(), 1);
    assert_eq!(before[0]["functionResponse"]["name"], "view_file");
    assert_eq!(before[0]["functionResponse"]["parts"], json!([pdf(99, 99)]));
    let after = retry[4]["parts"].as_array().unwrap();
    assert_eq!(after.len(), 1);
    assert!(after[0]["functionResponse"].get("parts").is_none());
    let output = after[0]["functionResponse"]["response"]["output"]
        .as_str()
        .unwrap();
    assert!(
        output.contains("manual.pdf (application/pdf, page 99) couldn't be processed"),
        "{output}"
    );
}

type CapturedHttp = Arc<Mutex<Vec<(String, String)>>>;

fn serve_http(
    replies: Vec<(&'static str, String)>,
) -> (String, CapturedHttp, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let heads = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&heads);
    let handle = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(20);
        for (expected_prefix, body) in replies {
            let (mut socket, request) = loop {
                match listener.accept() {
                    Ok((socket, _)) => {
                        socket
                            .set_read_timeout(Some(Duration::from_secs(10)))
                            .unwrap();
                        let mut reader = BufReader::new(socket.try_clone().unwrap());
                        let request = read_request(&mut reader).unwrap();
                        break (socket, request);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() > deadline {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(e) => panic!("accept: {e}"),
                }
            };
            assert!(
                request.head.starts_with(expected_prefix),
                "expected {expected_prefix:?}, got {}",
                request.head
            );
            captured.lock().unwrap().push((request.head, request.body));
            let reply = support::server::json("200 OK", &body).replacen(
                "\r\n",
                "\r\nConnection: close\r\n",
                1,
            );
            let _ = socket.write_all(reply.as_bytes());
        }
    });
    (format!("http://{address}"), heads, handle)
}

#[test]
fn startup_fetches_a_model_missing_from_a_fresh_saved_list_before_selecting_it() {
    use support::WISP;
    use support::home::{CONFIG, both, command_in, run};
    use support::server::{Server, json as http_json};
    use wisp_core::models::list::{SavedList, key};
    use wisp_core::provider::credential::{Credential, SubscriptionKind};

    const CACHED: &str = "gemini-4-carbon-high";
    const NEW: &str = "gemini-startup-fixture";
    assert!(wisp_core::models::ModelTable::shared().find(NEW).is_none());
    let server = Server::start(|request| {
        if request
            .head
            .starts_with("POST /v1internal:fetchAvailableModels ")
        {
            return http_json("200 OK", &json!({"models": { (NEW): {} }}).to_string());
        }
        assert!(
            request
                .head
                .starts_with("POST /v1internal:streamGenerateContent?alt=sse ")
        );
        let body: Value = serde_json::from_str(&request.body).unwrap();
        let reply = sse_chunk(
            body["model"].as_str().unwrap(),
            vec![json!({"text": "GEMINI-STARTUP-FIXTURE"})],
            Some("STOP"),
        );
        http_json("200 OK", &reply).replacen("application/json", "text/event-stream", 1)
    });
    let url = server.url();
    let home = Home::configured("startup-model-refresh");
    home.write(
        CONFIG,
        &format!(
            "store_id = \"0123456789abcdef\"\ndefault = \"direct\"\n[endpoints.direct]\nauth = \"api_key\"\nkey_env = \"WORK_TEST_KEY\"\nurl = \"{url}\"\n[endpoints.antigravity]\nprovider = \"gemini\"\nauth = \"antigravity_subscription\"\nkey_store = \"file\"\nurl = \"{url}\"\n"
        ),
    );
    let (profile, _) = home.antigravity(Some("CANARY_PROJECT"));
    let key = key(
        &url,
        &Credential::Subscription {
            kind: SubscriptionKind::Antigravity,
            secret: String::new(),
            profile: Some(profile),
        },
        "antigravity",
    )
    .unwrap();
    let folder = home.file(".local/share/wisp/models");
    let set = format!("model={NEW}");
    for (flags, expected, fetches) in [
        (&["--model", CACHED][..], CACHED, 0),
        (&["--model", NEW][..], NEW, 1),
        (&["--account", "antigravity", "--set", &set][..], NEW, 1),
    ] {
        let saved: SavedList = serde_json::from_value(json!({
            "attempted": jiff::Timestamp::now().as_second(),
            "default_model": CACHED,
            "list": [{"id": CACHED, "shown": true, "levels": [], "images": true, "search": false}]
        }))
        .unwrap();
        assert!(saved.list.find(NEW).is_none());
        saved.save(&folder, &key).unwrap();
        let mut command = command_in(&home.0, WISP);
        command
            .env("WORK_TEST_KEY", "CANARY_STARTUP_MODEL")
            .args([
                "--output",
                "jsonl",
                "--instructions",
                "none",
                "--tools",
                "none",
            ])
            .args(flags);
        let output = run(&mut command, "hello\n");
        assert!(output.status.success(), "{flags:?}: {}", both(&output));
        assert!(both(&output).contains("GEMINI-STARTUP-FIXTURE"));
        let seen = std::mem::take(&mut *server.seen.lock().unwrap());
        assert_eq!(seen.len(), fetches + 1, "{flags:?}: {}", both(&output));
        for request in &seen[..fetches] {
            assert!(
                request
                    .head
                    .starts_with("POST /v1internal:fetchAvailableModels ")
            );
        }
        let request: Value = serde_json::from_str(&seen[fetches].body).unwrap();
        assert_eq!(request["model"], expected, "{flags:?}");
    }
}

#[test]
fn doctor_check_resolves_antigravity_and_gemini_api_defaults_without_anthropic_wording() {
    use std::process::Stdio;
    use support::WISP;
    use support::home::{CONFIG, both, command_in, out};

    let ag_server = support::server::Server::start(|request| {
        if request
            .head
            .starts_with("POST /v1internal:fetchAvailableModels ")
        {
            return support::server::json("200 OK", &json!({"models": {}}).to_string());
        }
        assert!(
            request
                .head
                .starts_with("POST /v1internal:streamGenerateContent?alt=sse "),
            "{}",
            request.head
        );
        let body: Value = serde_json::from_str(&request.body).unwrap();
        let response = sse_chunk(
            body["model"].as_str().unwrap(),
            vec![json!({"text": "GEMINI-FIXTURE-RESUME"})],
            Some("STOP"),
        );
        support::server::json("200 OK", &response).replacen(
            "application/json",
            "text/event-stream",
            1,
        )
    });
    let ag_url = ag_server.url();
    let ag_heads = ag_server.seen.clone();
    let home = Home::configured("antigravity-doctor");
    home.write(
        CONFIG,
        &format!(
            "store_id = \"0123456789abcdef\"\ndefault = \"antigravity\"\n[endpoints.antigravity]\nprovider = \"gemini\"\nauth = \"antigravity_subscription\"\nkey_store = \"file\"\nurl = \"{ag_url}\"\n"
        ),
    );
    let (profile, tokens) = home.antigravity(Some("CANARY_PROJECT"));

    let output = command_in(&home.0, WISP)
        .args(["doctor", "--check"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let all = both(&output);
    let stdout = out(&output);
    assert!(output.status.success(), "{all}");
    assert_eq!(ag_heads.lock().unwrap().len(), 1, "{all}");
    assert_eq!(std::fs::read_to_string(&profile).unwrap(), tokens);

    let context_sec = stdout
        .split("\ncontext\n")
        .nth(1)
        .and_then(|s| s.split("\n\n").next())
        .unwrap_or_default();
    assert!(
        context_sec
            .lines()
            .any(|l| l == "  model              gemini-4-carbon-medium"),
        "{context_sec}"
    );

    let search_sec = stdout
        .split("\nweb search\n")
        .nth(1)
        .and_then(|s| s.split("\n\n").next())
        .unwrap_or_default();
    assert!(
        search_sec
            .lines()
            .any(|l| l == "  model              gemini-4-carbon-medium"),
        "{search_sec}"
    );
    assert!(
        search_sec
            .lines()
            .any(|l| l
                == "  antigravity        on (web_search: shipped; native_web_search: default)"),
        "{search_sec}"
    );

    let check_sec = stdout.split("\naccount check\n").nth(1).unwrap_or_default();
    for line in [
        "  request            fetchAvailableModels",
        "  cost               included in your subscription",
        "  antigravity        ok",
    ] {
        assert!(
            check_sec.lines().any(|l| l == line),
            "missing {line:?} in\n{check_sec}"
        );
    }
    for forbidden in ["claude-opus", "count_tokens", "Anthropic", "CANARY"] {
        assert!(!all.contains(forbidden), "{forbidden} found in\n{all}");
    }
    for line in stdout.lines() {
        let shown = ["--endpoint", "endpoints.", "endpoint = "]
            .iter()
            .fold(line.to_owned(), |l, form| l.replace(form, ""));
        assert_eq!(
            wisp_render::chrome::account::vocabulary_slips(&shown),
            Vec::<&str>::new(),
            "{line}"
        );
    }

    let key = wisp_core::models::list::key(
        &ag_url,
        &wisp_core::provider::credential::Credential::Subscription {
            kind: wisp_core::provider::credential::SubscriptionKind::Antigravity,
            secret: String::new(),
            profile: Some(profile),
        },
        "antigravity",
    )
    .unwrap();
    let cache = |model: &str| {
        let saved: wisp_core::models::list::SavedList = serde_json::from_value(json!({
            "attempted": jiff::Timestamp::now().as_second(),
            "default_model": model,
            "list": [{
                "id": model, "shown": true, "window": 1_048_576,
                "levels": [], "images": true, "search": false
            }]
        }))
        .unwrap();
        saved
            .save(&home.file(".local/share/wisp/models"), &key)
            .unwrap();
    };
    let models = ["gemini-4-carbon-high", "gemini-4-carbon-medium"];
    cache(models[0]);
    let config = home.read(CONFIG);
    let command = || {
        let mut command = command_in(&home.0, WISP);
        command.args([
            "--account",
            "antigravity",
            "--output",
            "jsonl",
            "--time",
            "off",
        ]);
        command
    };
    for configured in [false, true] {
        let model = if configured {
            "model = \"gemini-4-carbon\"\n"
        } else {
            ""
        };
        home.write(CONFIG, &format!("{model}{config}"));
        let start = || {
            let mut command = command();
            if !configured {
                command.args(["--model", "gemini-4-carbon"]);
            }
            command
        };
        let doctor = start().arg("doctor").output().unwrap();
        assert!(doctor.status.success(), "{}", both(&doctor));
        assert!(out(&doctor).contains("127.0.0.1:"), "{}", both(&doctor));
        let output = support::home::run(&mut start(), "hello\n");
        assert_eq!(output.status.code(), Some(1), "{}", both(&output));
        assert!(
            both(&output).contains("model gemini-4-carbon isn't one account antigravity serves"),
            "{}",
            both(&output)
        );
        assert_eq!(
            ag_heads.lock().unwrap().len(),
            2,
            "configured = {configured}"
        );
    }
    for (i, model) in models.iter().enumerate() {
        cache(model);
        let start = || {
            let mut command = command();
            if i != 0 {
                command.arg("--resume");
            }
            command.args([
                "--model",
                model,
                "--instructions",
                "none",
                "--tools",
                "none",
            ]);
            command
        };
        let doctor = start().arg("doctor").output().unwrap();
        assert!(doctor.status.success(), "{}", both(&doctor));
        assert!(out(&doctor).contains("127.0.0.1:"), "{}", both(&doctor));
        let output = support::home::run(&mut start(), "hello\n");
        assert!(output.status.success(), "{}", both(&output));
    }
    let sent: Vec<String> = ag_server
        .bodies()
        .iter()
        .filter_map(|body| {
            serde_json::from_str::<Value>(body).unwrap()["model"]
                .as_str()
                .map(str::to_owned)
        })
        .collect();
    assert_eq!(sent, models);

    let (api_url, api_heads, api_server) = serve_http(vec![(
        "GET /v1beta/models?pageSize=1 ",
        json!({"models": []}).to_string(),
    )]);
    let api_home = Home::configured("gemini-api-doctor");
    api_home.write(
        CONFIG,
        &format!(
            "default = \"gemini\"\n[endpoints.gemini]\nprovider = \"gemini\"\nauth = \"api_key\"\nkey_env = \"WORK_GEMINI_KEY\"\nurl = \"{api_url}\"\n[models.\"gemini-4-carbon\"]\nweb_search = [\"google_search\"]\n"
        ),
    );
    let api_out = command_in(&api_home.0, WISP)
        .env("WORK_GEMINI_KEY", TEST_KEY)
        .args(["doctor", "--check"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    api_server.join().unwrap();
    let api_all = both(&api_out);
    let api_stdout = out(&api_out);
    assert!(api_out.status.success(), "{api_all}");
    assert_eq!(api_heads.lock().unwrap().len(), 1, "{api_all}");
    assert!(
        api_stdout.contains("  model              gemini-4-carbon\n"),
        "{api_stdout}"
    );
    assert!(
        api_stdout
            .contains("  gemini             on (web_search: model; native_web_search: default)\n"),
        "{api_stdout}"
    );
    let api_check = api_stdout
        .split("\naccount check\n")
        .nth(1)
        .unwrap_or_default();
    for line in [
        "  request            models list",
        "  cost               free on Google's side; a gateway may count it",
        "  gemini             ok",
    ] {
        assert!(
            api_check.lines().any(|l| l == line),
            "missing {line:?} in\n{api_check}"
        );
    }
    for forbidden in ["claude-opus", "count_tokens", "Anthropic", "CANARY"] {
        assert!(
            !api_all.contains(forbidden),
            "{forbidden} found in\n{api_all}"
        );
    }
    for line in api_stdout.lines() {
        let shown = ["--endpoint", "endpoints.", "endpoint = "]
            .iter()
            .fold(line.to_owned(), |l, form| l.replace(form, ""));
        assert_eq!(
            wisp_render::chrome::account::vocabulary_slips(&shown),
            Vec::<&str>::new(),
            "{line}"
        );
    }
}

#[test]
fn doctor_check_with_antigravity_default_probes_anthropic_account_with_claude_model() {
    use std::process::Stdio;
    use support::WISP;
    use support::home::{CONFIG, both, command_in, out};

    let (ag_url, ag_seen, ag_server) = serve_http(vec![(
        "POST /v1internal:fetchAvailableModels ",
        json!({"models": {}}).to_string(),
    )]);
    let (claude_url, claude_seen, claude_server) = serve_http(vec![(
        "POST /v1/messages/count_tokens ",
        json!({"input_tokens": 8}).to_string(),
    )]);

    let home = Home::configured("antigravity-mixed-doctor");
    home.write(
        CONFIG,
        &format!(
            "store_id = \"0123456789abcdef\"\ndefault = \"antigravity\"\n[endpoints.antigravity]\nprovider = \"gemini\"\nauth = \"antigravity_subscription\"\nkey_store = \"file\"\nurl = \"{ag_url}\"\n[endpoints.claude]\nprovider = \"anthropic\"\nauth = \"api_key\"\nkey_env = \"WORK_CLAUDE_KEY\"\nurl = \"{claude_url}\"\n"
        ),
    );
    let (profile, tokens) = home.antigravity(None);

    let output = command_in(&home.0, WISP)
        .env("WORK_CLAUDE_KEY", "sk-ant-api03-CANARY-doc-key-0000")
        .args(["doctor", "--check"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    ag_server.join().unwrap();
    claude_server.join().unwrap();
    let all = both(&output);
    let stdout = out(&output);
    assert!(output.status.success(), "{all}");
    assert_eq!(ag_seen.lock().unwrap().len(), 1, "{all}");
    assert_eq!(std::fs::read_to_string(&profile).unwrap(), tokens);

    let claude_reqs = claude_seen.lock().unwrap();
    assert_eq!(claude_reqs.len(), 1, "{all}");
    let probe_body: Value = serde_json::from_str(&claude_reqs[0].1).unwrap();
    assert_eq!(probe_body["model"], "claude-opus-5-5", "{probe_body}");

    for section in ["\ncontext\n", "\nweb search\n"] {
        let block = stdout
            .split(section)
            .nth(1)
            .and_then(|s| s.split("\n\n").next())
            .unwrap_or_default();
        assert!(
            block
                .lines()
                .any(|l| l == "  model              gemini-4-carbon-medium"),
            "missing gemini-4-carbon-medium in {section}:\n{block}"
        );
    }

    let check_sec = stdout.split("\naccount check\n").nth(1).unwrap_or_default();
    for line in [
        "  request            fetchAvailableModels; count_tokens with claude-opus-5-5",
        "  cost               included in your subscription; free on Anthropic's side; a gateway may count it",
        "  antigravity        ok",
        "  claude             ok",
    ] {
        assert!(
            check_sec.lines().any(|l| l == line),
            "missing {line:?} in\n{check_sec}"
        );
    }
    assert!(!all.contains("CANARY"), "{all}");
    for line in stdout.lines() {
        let shown = ["--endpoint", "endpoints.", "endpoint = "]
            .iter()
            .fold(line.to_owned(), |l, form| l.replace(form, ""));
        assert_eq!(
            wisp_render::chrome::account::vocabulary_slips(&shown),
            Vec::<&str>::new(),
            "{line}"
        );
    }
}
