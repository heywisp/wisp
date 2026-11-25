use futures::StreamExt;
use serde_json::Value;
use std::time::Duration;
use wisp_protocol::ErrorKind;
use wisp_protocol::plan_usage::PlanUsageError;

use super::ProviderError;

pub(super) const LIMIT: usize = 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Unread {
    TooLarge,
    Unreadable,
    TimedOut,
}

pub(super) async fn read(response: reqwest::Response) -> Option<Vec<u8>> {
    read_bounded(response).await.ok()
}

async fn read_bounded(response: reqwest::Response) -> Result<Vec<u8>, Unread> {
    tokio::time::timeout(Duration::from_secs(5), read_capped(response, LIMIT))
        .await
        .map_err(|_| Unread::TimedOut)?
}

pub(super) async fn read_text(response: reqwest::Response) -> String {
    read(response)
        .await
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_default()
}

pub(super) async fn read_capped(
    response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, Unread> {
    if response
        .content_length()
        .is_some_and(|size| size > limit as u64)
    {
        return Err(Unread::TooLarge);
    }
    let mut source = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = source.next().await {
        let chunk = chunk.map_err(|e| {
            if e.is_timeout() {
                Unread::TimedOut
            } else {
                Unread::Unreadable
            }
        })?;
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(Unread::TooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub(super) async fn fetch_json(
    call: reqwest::RequestBuilder,
    forbidden: PlanUsageError,
    clock: &dyn crate::clock::Clock,
) -> Result<(Value, i64), PlanUsageError> {
    let response = call
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|_| PlanUsageError::Network)?;
    let now = clock.now().as_second();
    match response.status().as_u16() {
        200..=299 => {}
        401 => return Err(PlanUsageError::AuthenticationRequired),
        403 => return Err(forbidden),
        429 => {
            return Err(PlanUsageError::Throttled {
                retry_at: super::limit::retry_at(response.headers(), now),
            });
        }
        _ => return Err(PlanUsageError::InvalidResponse),
    }
    let bytes = read_bounded(response).await.map_err(|error| match error {
        Unread::TooLarge => PlanUsageError::InvalidResponse,
        Unread::TimedOut | Unread::Unreadable => PlanUsageError::Network,
    })?;
    let raw = serde_json::from_slice(&bytes).map_err(|_| PlanUsageError::InvalidResponse)?;
    Ok((raw, now))
}

impl From<&ProviderError> for PlanUsageError {
    fn from(error: &ProviderError) -> Self {
        match error.kind {
            ErrorKind::Network | ErrorKind::Timeout | ErrorKind::Server | ErrorKind::Other => {
                Self::Network
            }
            ErrorKind::Auth => Self::AuthenticationRequired,
            ErrorKind::RateLimit => Self::Throttled { retry_at: None },
            _ => Self::InvalidResponse,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn nonstreaming_bodies_are_bounded_with_or_without_content_length() {
        let size = LIMIT + 1;
        let mut chunked = format!("{size:x}\r\n").into_bytes();
        chunked.extend(vec![b'x'; size]);
        chunked.extend_from_slice(b"\r\n0\r\n\r\n");
        let short = b"short".to_vec();
        let cases = [
            (
                format!("Content-Length: {size}"),
                vec![b'x'; size],
                false,
                Unread::TooLarge,
            ),
            (
                "Transfer-Encoding: chunked".into(),
                chunked,
                false,
                Unread::TooLarge,
            ),
            (
                "Content-Length: 10".into(),
                short.clone(),
                false,
                Unread::Unreadable,
            ),
            ("Content-Length: 10".into(), short, true, Unread::TimedOut),
        ];
        for (header, body, stall, want) in cases {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                assert!(socket.read(&mut request).await.unwrap() > 0);
                let head = format!("HTTP/1.1 200 OK\r\n{header}\r\nConnection: close\r\n\r\n");
                socket.write_all(head.as_bytes()).await.unwrap();
                let _ = socket.write_all(&body).await;
                if stall {
                    let _ = socket.read(&mut request).await;
                }
            });
            let mut call = reqwest::Client::new().get(format!("http://{address}"));
            if stall {
                call = call.timeout(Duration::from_millis(300));
            }
            let response = call.send().await.unwrap();
            assert_eq!(read_capped(response, LIMIT).await, Err(want));
            server.await.unwrap();
        }
    }
}
