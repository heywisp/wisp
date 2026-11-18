#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test on any socket error"
)]

use std::time::Duration;

use futures::StreamExt;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{Error, Message};
use tokio_tungstenite::{WebSocketStream, client_async};

pub(crate) type Socket = WebSocketStream<TcpStream>;

pub(crate) async fn get(port: u16, head: &str) -> (u16, String) {
    get_over(connect(port).await, head).await
}

pub(crate) async fn get_over(
    mut stream: impl AsyncRead + AsyncWrite + Unpin,
    head: &str,
) -> (u16, String) {
    let request = format!("{head}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    tokio::time::timeout(
        Duration::from_secs(10),
        stream.read_to_string(&mut response),
    )
    .await
    .unwrap()
    .unwrap();
    let code = response.split(' ').nth(1).unwrap().parse().unwrap();
    (code, response)
}

pub(crate) fn page(port: u16, path: &str) -> String {
    format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}")
}

pub(crate) async fn open(port: u16, host: &str, origin: Option<&str>) -> Result<Socket, Error> {
    open_over(connect(port).await, host, origin).await
}

pub(crate) async fn open_over<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    host: &str,
    origin: Option<&str>,
) -> Result<WebSocketStream<S>, Error> {
    let mut request = format!("ws://{host}/socket").into_client_request().unwrap();
    if let Some(origin) = origin {
        request
            .headers_mut()
            .insert("origin", origin.parse().unwrap());
    }
    client_async(request, stream)
        .await
        .map(|(socket, _)| socket)
}

pub(crate) async fn connect(port: u16) -> TcpStream {
    TcpStream::connect(("127.0.0.1", port)).await.unwrap()
}

pub(crate) async fn next_text(socket: &mut Socket) -> Option<Value> {
    loop {
        match tokio::time::timeout(Duration::from_secs(30), socket.next())
            .await
            .unwrap()
        {
            Some(Ok(Message::Text(text))) => return Some(serde_json::from_str(&text).unwrap()),
            Some(Ok(Message::Pong(_))) => return None,
            Some(Ok(_)) => {}
            Some(Err(_)) | None => panic!("the socket closed"),
        }
    }
}
