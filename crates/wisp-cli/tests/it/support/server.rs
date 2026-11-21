use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use serde_json::Value;

pub(crate) struct Seen {
    pub(crate) head: String,
    pub(crate) body: String,
}

pub(crate) struct Server {
    pub(crate) port: u16,
    pub(crate) seen: Arc<Mutex<Vec<Seen>>>,
    pub(crate) named: Arc<Mutex<Vec<Seen>>>,
}

pub(crate) fn is_naming(body: &str) -> bool {
    body.contains(wisp_core::agent::naming::INSTRUCTION)
}

pub(crate) fn not_named() -> String {
    json(
        "400 Bad Request",
        r#"{"type":"error","error":{"type":"invalid_request_error","message":"no names in tests"}}"#,
    )
}

impl Server {
    pub(crate) fn start(answer: impl Fn(&Seen) -> String + Send + Sync + 'static) -> Server {
        Self::naming(answer, not_named())
    }

    pub(crate) fn replying(reply: String) -> Server {
        Self::start(move |_| reply.clone())
    }

    pub(crate) fn naming(
        answer: impl Fn(&Seen) -> String + Send + Sync + 'static,
        name: String,
    ) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let named = Arc::new(Mutex::new(Vec::new()));
        let logs = (Arc::clone(&seen), Arc::clone(&named));
        let answer = Arc::new(answer);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let (recorded, naming) = (Arc::clone(&logs.0), Arc::clone(&logs.1));
                let (answer, name) = (Arc::clone(&answer), name.clone());
                std::thread::spawn(move || {
                    let mut writer = stream.try_clone().unwrap();
                    let mut reader = BufReader::new(stream);
                    while let Some(request) = read_request(&mut reader) {
                        let (reply, log) = if request.head.starts_with("GET /v1/models") {
                            (json("404 Not Found", "{}"), None)
                        } else if is_naming(&request.body) {
                            (name.clone(), Some(&naming))
                        } else {
                            (answer(&request), Some(&recorded))
                        };
                        if let Some(log) = log {
                            log.lock().unwrap().push(request);
                        }
                        if writer.write_all(reply.as_bytes()).is_err() {
                            return;
                        }
                    }
                });
            }
        });
        Server { port, seen, named }
    }

    pub(crate) fn named(&self) -> Vec<(String, String)> {
        let named = self.named.lock().unwrap();
        named
            .iter()
            .map(|s| (s.head.clone(), s.body.clone()))
            .collect()
    }

    pub(crate) fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub(crate) fn heads(&self) -> Vec<String> {
        let seen = self.seen.lock().unwrap();
        seen.iter().map(|s| s.head.clone()).collect()
    }

    pub(crate) fn bodies(&self) -> Vec<String> {
        let seen = self.seen.lock().unwrap();
        seen.iter().map(|s| s.body.clone()).collect()
    }
}

pub(crate) fn read_request(reader: &mut BufReader<TcpStream>) -> Option<Seen> {
    let mut head = String::new();
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        if line.trim_end().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().ok()?;
        }
        head.push_str(&line);
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).ok()?;
    Some(Seen {
        head,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

pub(crate) fn sse(events: &[Value]) -> String {
    let body: String = events
        .iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
        .collect();
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    )
}

pub(crate) fn json(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    )
}

pub(crate) fn counted() -> String {
    json("200 OK", r#"{"input_tokens":5}"#)
}

pub(crate) fn message_start(id: &str, model: &str, output: u64) -> Value {
    serde_json::json!({"type": "message_start", "message": {
        "id": id, "type": "message", "role": "assistant", "model": model,
        "content": [], "stop_reason": null, "stop_sequence": null,
        "usage": {"input_tokens": 10, "output_tokens": output}}})
}

pub(crate) fn text_reply(id: &str, model: &str, text: &str, (start, end): (u64, u64)) -> String {
    sse(&[
        message_start(id, model, start),
        serde_json::json!({"type": "content_block_start", "index": 0,
            "content_block": {"type": "text", "text": ""}}),
        serde_json::json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": text}}),
        serde_json::json!({"type": "content_block_stop", "index": 0}),
        serde_json::json!({"type": "message_delta",
            "delta": {"stop_reason": "end_turn", "stop_sequence": null},
            "usage": {"output_tokens": end}}),
        serde_json::json!({"type": "message_stop"}),
    ])
}

pub(crate) fn tool_reply(id: &str, name: &str, input: &Value) -> String {
    sse_reply(
        &serde_json::json!({"type": "tool_use", "id": id, "name": name, "input": {}}),
        &input.to_string(),
    )
}

pub(crate) fn sse_reply(block: &Value, delta: &str) -> String {
    let tool = block["type"] == "tool_use";
    let delta = if tool {
        serde_json::json!({"type": "input_json_delta", "partial_json": delta})
    } else {
        serde_json::json!({"type": "text_delta", "text": delta})
    };
    let events = [
        message_start("msg_golden", "claude-opus-5-5", 1),
        serde_json::json!({"type": "content_block_start", "index": 0,
            "content_block": block}),
        serde_json::json!({"type": "content_block_delta", "index": 0, "delta": delta}),
        serde_json::json!({"type": "content_block_stop", "index": 0}),
        serde_json::json!({"type": "message_delta",
            "delta": {"stop_reason": if tool { "tool_use" } else { "end_turn" }, "stop_sequence": null},
            "usage": {"output_tokens": 5}}),
        serde_json::json!({"type": "message_stop"}),
    ];
    sse(&events)
}

pub(crate) fn is_count(head: &str) -> bool {
    head.starts_with("POST /v1/messages/count_tokens ")
}

fn first_text(body: &str) -> String {
    let request: Value = serde_json::from_str(body).unwrap();
    request["messages"][0]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

pub(crate) fn is_child(body: &str) -> bool {
    first_text(body).contains("\nParent: ")
}

pub(crate) fn agent_of(body: &str) -> String {
    match first_text(body)
        .lines()
        .find_map(|line| line.strip_prefix("Agent: "))
    {
        Some(path) if is_child(body) => path.to_owned(),
        _ => "/".to_owned(),
    }
}

pub(crate) fn free_port() -> u16 {
    std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
