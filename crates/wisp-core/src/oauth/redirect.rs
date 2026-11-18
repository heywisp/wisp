use std::sync::Arc;
use std::time::Duration;

use reqwest::Url;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use wisp_protocol::Secret;

use super::{AuthError, LOGIN_TIMEOUT, parse_redirect_url};

#[derive(Clone)]
pub(crate) struct RedirectReturn {
    pub(super) url: String,
    pub(super) state: Secret,
    pub(super) listener: Option<Arc<TcpListener>>,
    pub(super) started: tokio::time::Instant,
}

pub(crate) struct Returned {
    pub code: Secret,
    pub issuer: Option<String>,
    socket: Option<TcpStream>,
}

impl Returned {
    pub(crate) async fn respond(mut self, page: &str) {
        if let Some(mut socket) = self.socket.take() {
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
                page.len()
            );
            let _ = tokio::time::timeout(
                Duration::from_secs(5),
                socket.write_all(response.as_bytes()),
            )
            .await;
        }
    }
}

impl RedirectReturn {
    pub(crate) fn new(url: String, state: Secret, listener: Option<Arc<TcpListener>>) -> Self {
        Self {
            url,
            state,
            listener,
            started: tokio::time::Instant::now(),
        }
    }

    pub(crate) async fn bind() -> Self {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .ok();
        let port = listener
            .as_ref()
            .and_then(|listener| listener.local_addr().ok())
            .map_or(8123, |address| address.port());
        Self::new(
            format!("http://127.0.0.1:{port}/oauth-callback"),
            Secret::new(""),
            listener.map(Arc::new),
        )
    }

    pub(crate) fn url(&self) -> &str {
        &self.url
    }
    pub(crate) fn state(&self) -> &str {
        self.state.expose()
    }
    pub(crate) fn set_state(&mut self, state: String) {
        self.state = Secret::new(state);
    }
    pub(crate) fn uses_loopback(&self) -> bool {
        self.listener.is_some()
    }
    pub(crate) fn deadline(&self) -> tokio::time::Instant {
        self.started + LOGIN_TIMEOUT
    }
    pub(crate) fn check(&self) -> Result<(), AuthError> {
        if tokio::time::Instant::now() >= self.deadline() {
            Err(AuthError::Expired)
        } else {
            Ok(())
        }
    }

    pub(crate) fn pasted(&self, text: &str) -> Result<Returned, AuthError> {
        self.check()?;
        let path = Url::parse(&self.url)
            .map_err(|_| AuthError::Callback)?
            .path()
            .to_owned();
        let code = parse_redirect_url(text, self.state(), &path)?;
        let issuer = Url::parse(text.trim()).ok().and_then(|url| {
            url.query_pairs()
                .find(|(name, _)| name == "iss")
                .map(|(_, value)| value.into_owned())
        });
        Ok(Returned {
            code: Secret::new(code),
            issuer,
            socket: None,
        })
    }

    pub(crate) async fn receive(&self) -> Result<Returned, AuthError> {
        self.check()?;
        let listener = self.listener.as_ref().ok_or(AuthError::Callback)?;
        tokio::time::timeout_at(self.deadline(), self.callback(listener))
            .await
            .map_err(|_| AuthError::Expired)?
    }

    pub(super) async fn callback(&self, listener: &TcpListener) -> Result<Returned, AuthError> {
        loop {
            let (mut socket, peer) = listener.accept().await.map_err(|_| AuthError::Callback)?;
            if !peer.ip().is_loopback() {
                continue;
            }
            let request = tokio::time::timeout(Duration::from_secs(5), async {
                let mut bytes = Vec::new();
                loop {
                    let mut chunk = [0; 1024];
                    let count = socket.read(&mut chunk).await.ok()?;
                    if count == 0 || bytes.len().saturating_add(count) > 8192 {
                        return None;
                    }
                    bytes.extend_from_slice(&chunk[..count]);
                    if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                String::from_utf8(bytes).ok()
            })
            .await
            .ok()
            .flatten();
            let target = request
                .as_deref()
                .and_then(|request| request.lines().next())
                .and_then(|line| line.strip_prefix("GET "))
                .and_then(|line| line.strip_suffix(" HTTP/1.1"));
            let Some(target) =
                target.filter(|target| target.starts_with('/') && !target.starts_with("//"))
            else {
                continue;
            };
            let url = format!("http://localhost{target}");
            match self.pasted(&url) {
                Ok(mut returned) => {
                    returned.socket = Some(socket);
                    return Ok(returned);
                }
                Err(AuthError::Declined) => {
                    Returned {
                        code: Secret::new(""),
                        issuer: None,
                        socket: Some(socket),
                    }
                    .respond("Sign-in declined. Return to Wisp.")
                    .await;
                    return Err(AuthError::Declined);
                }
                _ => {}
            }
        }
    }
}
