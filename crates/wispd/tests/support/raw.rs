#![allow(
    clippy::unwrap_used,
    reason = "scripted link helpers fail the test on transport or frame errors"
)]

use std::path::Path;
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use wisp_protocol::{jsonl, link};

pub(crate) const WAIT: Duration = Duration::from_secs(5);

pub(crate) struct Raw<S = UnixStream>(pub(crate) BufReader<S>);

impl Raw<UnixStream> {
    pub(crate) async fn connect(path: &Path) -> Self {
        Self(BufReader::new(wisp_client::link::dial(path).await.unwrap()))
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> Raw<S> {
    pub(crate) async fn send<T: Serialize>(&mut self, frame: &T) {
        self.line(&jsonl::line(frame).unwrap()).await;
    }

    pub(crate) async fn line(&mut self, line: &str) {
        self.0.write_all(line.as_bytes()).await.unwrap();
    }

    pub(crate) async fn next<T: DeserializeOwned>(&mut self) -> T {
        let mut line = String::new();
        assert!(
            tokio::time::timeout(WAIT, self.0.read_line(&mut line))
                .await
                .unwrap()
                .unwrap()
                > 0
        );
        link::read(&line).unwrap()
    }
}
