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
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use support::pty::{self, Pty};
use support::server::{is_naming, not_named, read_request};
use support::{
    WISP,
    home::{Home, both, command_in},
};

fn output(items: Vec<Value>) -> String {
    [json!({"type":"response.created","response":{"id":"resp_fixture","model":"gpt-6.1-sol"}}),json!({"type":"response.completed","response":{"id":"resp_fixture","model":"gpt-6.1-sol","status":"completed","output":items,"usage":{"input_tokens":100,"input_tokens_details":{"cached_tokens":30},"output_tokens":10}}})]
        .iter().map(|value|format!("event: {}\ndata: {value}\n\n",value["type"].as_str().unwrap())).collect()
}

fn reply() -> String {
    output(vec![
        json!({"type":"message","id":"msg_final","role":"assistant","phase":"final_answer","status":"completed","content":[{"type":"output_text","text":"OPENAI-FIXTURE-DONE","annotations":[]}]}),
    ])
}

fn serve(replies: Vec<String>) -> (String, Arc<Mutex<Vec<String>>>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let captured = seen.clone();
    let thread = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(45);
        for body in replies {
            let (mut socket, request) = loop {
                let mut socket = match listener.accept() {
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
                let _ = socket.write_all(refused.as_bytes());
            };
            assert!(request.head.starts_with("POST /v1/responses "));
            assert!(request.head.contains("Bearer synthetic-openai-key"));
            assert!(
                !request
                    .head
                    .to_ascii_lowercase()
                    .contains("chatgpt-account-id")
            );
            captured.lock().unwrap().push(request.body);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            for chunk in response.as_bytes().chunks(7) {
                if socket.write_all(chunk).is_err() {
                    break;
                }
            }
        }
    });
    (format!("http://{address}"), seen, thread)
}

fn command(home: &Path, config: &Path) -> Command {
    let mut cmd = Pty::command(home);
    cmd.env_remove("LANG")
        .env("WORK_OPENAI_KEY", "synthetic-openai-key")
        .arg("--config")
        .arg(config)
        .args([
            "--output",
            "tui",
            "--endpoint",
            "openai",
            "--instructions",
            "none",
            "--time",
            "off",
            "--no-auto-compact",
            "--tools",
            "bash,web_search",
        ]);
    cmd
}

fn config(home: &Path, url: &str) -> std::path::PathBuf {
    let path = home.join("config.toml");
    std::fs::write(&path,format!("default = \"openai\"\n[endpoints.openai]\nprovider = \"openai\"\nauth = \"api_key\"\nkey_env = \"WORK_OPENAI_KEY\"\nurl = \"{url}\"\n")).unwrap();
    path
}

#[test]
fn cli_selects_gpt_default_executes_one_tool_and_resumes_native_items() {
    let home = Home::new("openai-tool");
    let tool = output(vec![
        json!({"type":"reasoning","id":"rs_first","summary":[],"encrypted_content":"synthetic-ciphertext"}),
        json!({"type":"function_call","id":"fc_fixture","call_id":"call_fixture","name":"bash","arguments":"{ \"command\": \"printf once >> tool-count; printf tool-result\" }","status":"completed","future_field":"preserved"}),
    ]);
    let search = json!({"type":"web_search_call","id":"ws_fixture","status":"completed","action":{"type":"search","query":"fixture query","sources":[{"type":"url","url":"https://example.com/fixture-source"}]}});
    let searched = reply().replacen("\"output\":[", &format!("\"output\":[{search},"), 1);
    let (url, seen, server) = serve(vec![
        tool,
        searched,
        reply().replace("OPENAI-FIXTURE-DONE", "OPENAI-FIXTURE-RESUMED"),
    ]);
    let path = config(&home.0, &url);
    let first = pty::run(
        &mut command(&home.0, &path),
        "Hello\n",
        "OPENAI-FIXTURE-DONE",
        "synthetic-openai-key",
    );
    assert!(first.contains("OPENAI-FIXTURE-DONE"), "{first}");
    assert_eq!(
        std::fs::read_to_string(home.0.join("tool-count")).unwrap(),
        "once"
    );
    let bodies = seen.lock().unwrap().clone();
    let requests: Vec<Value> = bodies
        .iter()
        .map(|body| serde_json::from_str(body).unwrap())
        .collect();
    super::golden_requests::check_body("chatgpt_search_on", &bodies[0]);
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["model"], "gpt-6.1-sol");
    for request in requests.iter() {
        assert!(
            request["tools"]
                .as_array()
                .unwrap()
                .contains(&json!({"type":"web_search"}))
        );
        assert_eq!(request["include"][1], "web_search_call.action.sources");
    }
    let input = requests[1]["input"].as_array().unwrap();
    assert_eq!(
        input
            .iter()
            .filter(|item| item["type"] == "function_call_output")
            .count(),
        1
    );
    let call = input
        .iter()
        .find(|item| item["type"] == "function_call")
        .unwrap();
    assert_eq!(
        call["arguments"],
        "{ \"command\": \"printf once >> tool-count; printf tool-result\" }"
    );
    assert_eq!(call["future_field"], "preserved");
    let second = pty::run(
        command(&home.0, &path).arg("--resume"),
        "Continue\n",
        "OPENAI-FIXTURE-RESUMED",
        "synthetic-openai-key",
    );
    assert!(second.contains("OPENAI-FIXTURE-RESUMED"), "{second}");
    server.join().unwrap();
    let requests: Vec<Value> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|body| serde_json::from_str(body).unwrap())
        .collect();
    assert_eq!(requests.len(), 3);
    let input = requests[2]["input"].as_array().unwrap();
    assert_eq!(
        input
            .iter()
            .filter(|item| item["id"] == "fc_fixture")
            .count(),
        1
    );
    assert!(
        input
            .iter()
            .any(|item| item["encrypted_content"] == "synthetic-ciphertext")
    );
    let replayed = requests[2]["input"].to_string();
    assert!(
        replayed.contains(r#"[web search: \"fixture query\"]"#),
        "{replayed}"
    );
    assert!(!replayed.contains("https://example.com/fixture-source"));
    assert_eq!(
        std::fs::read_to_string(home.0.join("tool-count")).unwrap(),
        "once"
    );
}

#[test]
fn subscription_resume_never_falls_back_to_api_billing_without_explicit_choice() {
    use wisp_protocol::log::LOG_SCHEMA_VERSION;
    for case in ["logged-out", "removed", "same-name-api"] {
        let home = Home::new(case);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let config = home.0.join("config.toml");
        let mut text = format!(
            "store_id = \"0123456789abcdef\"\ndefault = \"{}\"\n[endpoints.api]\nprovider = \"openai\"\nauth = \"api_key\"\nkey_env = \"WORK_OPENAI_KEY\"\nurl = \"http://{address}\"\n",
            if case == "same-name-api" {
                "chatgpt"
            } else {
                "api"
            }
        );
        if case == "logged-out" {
            text.push_str("[endpoints.chatgpt]\nprovider = \"openai\"\nauth = \"chatgpt_subscription\"\nkey_store = \"file\"\n");
            std::fs::write(home.0.join("credentials.json"),json!({"version":1,"stores":{"0123456789abcdef":{"chatgpt":{"key":"oauth:00000000000000000000000000000000"}}}}).to_string()).unwrap();
        } else if case == "same-name-api" {
            text.push_str(&format!("[endpoints.chatgpt]\nprovider = \"openai\"\nauth = \"api_key\"\nkey_env = \"WORK_OPENAI_KEY\"\nurl = \"http://{address}\"\n"));
        }
        std::fs::write(&config, text).unwrap();
        let sessions = home.0.join(".local/share/wisp/sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let id = "01a167d6-0000-7000-8000-000000000000";
        let start = json!({"schema_version":LOG_SCHEMA_VERSION,"seq":0,"at":"2026-10-25T10:00:00Z","type":"session_started","session_id":id,"wisp_version":"0.1.0","model":"gpt-6.1-sol","effort":"medium","cache_ttl":"one_hour","endpoint":"https://chatgpt.com","credential_source":{"mode":{"billing":"chat_gpt_plan","source":"credential_file"}},"endpoint_name":"chatgpt","cwd":home.0.to_str().unwrap()});
        std::fs::write(sessions.join(format!("{id}.jsonl")), format!("{start}\n")).unwrap();
        let mut command = command_in(&home.0, WISP);
        command
            .env("WORK_OPENAI_KEY", "synthetic-openai-key")
            .arg("--config")
            .arg(&config)
            .args([
                "--output",
                "jsonl",
                "--instructions",
                "none",
                "--resume",
                id,
            ]);
        let result = support::home::run(&mut command, "Hello\n");
        let all = both(&result);
        assert!(!result.status.success(), "{case}: {all}");
        if case == "same-name-api" {
            assert!(
                all.contains("this session used your ChatGPT subscription; chatgpt uses the API"),
                "{case}: {all}"
            );
            assert!(!all.contains("authentication required"), "{case}: {all}");
            assert!(
                all.contains("rerun with --account chatgpt"),
                "{case}: {all}"
            );
        } else {
            let why = if case == "logged-out" {
                ", which can't be used now ("
            } else {
                "; "
            };
            assert!(
                all.contains(&format!("wisp: session 01a167d6-0000 ran on chatgpt{why}")),
                "{case}: {all}"
            );
            assert!(
                all.contains("; rerun with --account api\n"),
                "{case}: {all}"
            );
            assert!(!all.contains("continuing on"), "{case}: {all}");
            assert!(!all.contains("--endpoint chatgpt"), "{case}: {all}");
        }
        assert!(
            listener.accept().is_err(),
            "{case}: contacted the API endpoint"
        );
    }
}

#[test]
fn explicit_resume_override_persists_billing_choice_before_any_inference() {
    use wisp_protocol::log::LOG_SCHEMA_VERSION;
    let home = Home::new("openai-override");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let config = config(&home.0, &format!("http://{address}"));
    let sessions = home.0.join(".local/share/wisp/sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let id = "01a167d6-0000-7000-8000-000000000000";
    let log = sessions.join(format!("{id}.jsonl"));
    let start = json!({"schema_version":LOG_SCHEMA_VERSION,"seq":0,"at":"2026-10-25T10:00:00Z","type":"session_started","session_id":id,"wisp_version":"0.1.0","model":"gpt-6.1-sol","effort":"medium","cache_ttl":"one_hour","endpoint":"https://chatgpt.com","credential_source":{"mode":{"billing":"chat_gpt_plan","source":"credential_file"}},"endpoint_name":"chatgpt","cwd":home.0.to_str().unwrap()});
    std::fs::write(&log, format!("{start}\n")).unwrap();
    for explicit in [true, false] {
        let mut command = command_in(&home.0, WISP);
        command
            .env("WORK_OPENAI_KEY", "synthetic-openai-key")
            .arg("--config")
            .arg(&config)
            .args([
                "--output",
                "jsonl",
                "--instructions",
                "none",
                "--resume",
                id,
            ]);
        if explicit {
            command.args(["--endpoint", "openai"]);
        }
        let result = command.stdin(Stdio::null()).output().unwrap();
        let all = both(&result);
        assert!(result.status.success(), "explicit={explicit}: {all}");
        assert!(listener.accept().is_err());
        let records = std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert!(
            records
                .iter()
                .rev()
                .any(|record| record["type"] == "session_resumed"
                    && record["endpoint_name"] == "openai"
                    && record["credential_source"]["mode"]["billing"] == "metered")
        );
    }
}
