use std::future::Future;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

use super::within;

pub(crate) enum Reply {
    Send(String),
    Delayed(std::time::Duration, String),
    Hang,
    HeadThenHang(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Heard {
    Request(String),
    Closed,
}

pub(crate) struct Server {
    pub(crate) url: String,
    pub(crate) heard: mpsc::UnboundedReceiver<Heard>,
    pub(crate) requests: Arc<Mutex<Vec<String>>>,
}

impl Server {
    pub(crate) async fn next(&mut self) -> Heard {
        within("request at the server", self.heard.recv())
            .await
            .expect("the server stopped")
    }
}

pub(crate) async fn read_request(socket: &mut TcpStream) -> Option<String> {
    let mut seen = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        let n = socket.read(&mut buffer).await.ok()?;
        if n == 0 {
            return None;
        }
        seen.extend_from_slice(&buffer[..n]);
        let text = String::from_utf8_lossy(&seen).into_owned();
        if let Some(end) = text.find("\r\n\r\n") {
            let length = text[..end]
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            if seen.len() >= end + 4 + length {
                return Some(text);
            }
        }
    }
}

async fn until_closed(socket: &mut TcpStream) {
    let mut buffer = [0u8; 1024];
    while let Ok(n) = socket.read(&mut buffer).await {
        if n == 0 {
            return;
        }
    }
}

async fn answer(socket: &mut TcpStream, reply: Reply) -> Option<Heard> {
    let (delay, response) = match reply {
        Reply::Send(response) => (None, response),
        Reply::Delayed(delay, response) => (Some(delay), response),
        Reply::Hang => {
            until_closed(socket).await;
            return Some(Heard::Closed);
        }
        Reply::HeadThenHang(head) => {
            let _ = socket.write_all(head.as_bytes()).await;
            until_closed(socket).await;
            return Some(Heard::Closed);
        }
    };
    if let Some(delay) = delay {
        tokio::time::sleep(delay).await;
    }
    let _ = socket.write_all(response.as_bytes()).await;
    let _ = socket.shutdown().await;
    None
}

pub(crate) async fn serve(replies: Vec<Reply>) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (tx, heard) = mpsc::unbounded_channel();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let log = requests.clone();
    tokio::spawn(async move {
        for reply in replies {
            let (mut socket, _) = listener.accept().await.unwrap();
            let Some(request) = read_request(&mut socket).await else {
                let _ = tx.send(Heard::Closed);
                continue;
            };
            log.lock().unwrap().push(request.clone());
            let _ = tx.send(Heard::Request(request));
            if let Some(closed) = answer(&mut socket, reply).await {
                let _ = tx.send(closed);
            }
        }
    });
    Server {
        url,
        heard,
        requests,
    }
}

pub(crate) async fn gateway<A, F>(reply: A) -> String
where
    A: Fn(String) -> F + Send + Sync + 'static,
    F: Future<Output = Reply> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let reply = Arc::new(reply);
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let reply = reply.clone();
            tokio::spawn(async move {
                if let Some(request) = read_request(&mut socket).await {
                    answer(&mut socket, reply(request).await).await;
                }
            });
        }
    });
    url
}

fn head(status: &str, headers: &[(&str, &str)]) -> String {
    let mut text = format!("HTTP/1.1 {status}\r\n");
    for (name, value) in headers {
        text.push_str(&format!("{name}: {value}\r\n"));
    }
    text
}

pub(crate) fn response(status: &str, headers: &[(&str, &str)], body: &str) -> String {
    let head = head(status, headers);
    format!(
        "{head}content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

pub(crate) fn json(status: &str, body: &str) -> String {
    response(status, &[("content-type", "application/json")], body)
}

pub(crate) fn event(name: &str, data: impl std::fmt::Display) -> String {
    format!("event: {name}\ndata: {data}\n\n")
}

pub(crate) fn sse(body: &str) -> String {
    sse_with(&[], body)
}

pub(crate) fn sse_with(headers: &[(&str, &str)], body: &str) -> String {
    let mut all = vec![("content-type", "text/event-stream")];
    all.extend_from_slice(headers);
    format!("{}connection: close\r\n\r\n{body}", head("200 OK", &all))
}

#[test]
fn event_preserves_raw_data_and_value_display() {
    let data = r#"{"error":{"message":"bad gateway key"}}"#;
    let framed = "event: message\ndata: {\"error\":{\"message\":\"bad gateway key\"}}\n\n";
    assert_eq!(event("message", data), framed);
    let value: serde_json::Value = serde_json::from_str(data).unwrap();
    assert_eq!(event("message", value), framed);
}

pub(crate) fn header_lines(request: &str) -> Vec<(String, String)> {
    let head = &request[..request.find("\r\n\r\n").unwrap()];
    head.lines()
        .skip(1)
        .map(|line| {
            let (name, value) = line.split_once(':').unwrap();
            (name.to_ascii_lowercase(), value.trim().to_owned())
        })
        .collect()
}

pub(crate) fn body(request: &str) -> &str {
    request.split_once("\r\n\r\n").unwrap().1
}
