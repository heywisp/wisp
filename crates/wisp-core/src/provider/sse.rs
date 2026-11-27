use std::collections::VecDeque;
use std::future::Future;
use std::time::Duration;

use futures::stream::{self, BoxStream};
use futures::{StreamExt, TryStreamExt};
use wisp_protocol::{ErrorKind, FailurePhase, RetryClass};

use super::{ProviderError, ProviderEvent, classify};

pub(crate) const MAX_EVENT: usize = 16 * 1024 * 1024;
pub(crate) const IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SseEvent {
    pub(crate) event: Option<String>,
    pub(crate) data: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EventTooLarge;

#[derive(Debug, Default)]
pub(crate) struct SseParser {
    buf: Vec<u8>,
    event: Option<String>,
    data: Option<String>,
}

impl SseParser {
    pub(crate) fn pending_bytes(&self) -> usize {
        self.buf
            .len()
            .saturating_add(self.data.as_ref().map_or(0, String::len))
    }

    pub(crate) fn push(&mut self, chunk: &[u8]) -> Vec<Result<SseEvent, EventTooLarge>> {
        if self.pending_bytes().saturating_add(chunk.len()) > MAX_EVENT {
            return vec![Err(EventTooLarge)];
        }
        let mut scan = self.buf.len();
        let mut buf = std::mem::take(&mut self.buf);
        buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        let mut start = 0;
        while let Some(pos) = buf[scan..].iter().position(|&b| b == b'\n') {
            let end = scan + pos;
            let mut line = &buf[start..end];
            if let [rest @ .., b'\r'] = line {
                line = rest;
            }
            let line = String::from_utf8_lossy(line);
            start = end + 1;
            scan = start;
            if let Some(event) = self.line(&line) {
                if event.data.len() > MAX_EVENT {
                    out.push(Err(EventTooLarge));
                    break;
                }
                out.push(Ok(event));
            }
        }
        buf.drain(..start);
        self.buf = buf;
        out
    }

    pub(crate) fn has_partial(&self) -> bool {
        !self.buf.is_empty() || self.data.is_some() || self.event.is_some()
    }

    fn line(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            let event = self.event.take();
            return self.data.take().map(|data| SseEvent { event, data });
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = Some(value.to_owned()),
            "data" => match &mut self.data {
                Some(data) => {
                    data.push('\n');
                    data.push_str(value);
                }
                None => self.data = Some(value.to_owned()),
            },
            _ => {}
        }
        None
    }
}

pub(crate) type Item = Result<ProviderEvent, ProviderError>;

pub(crate) trait Decoder: Send + 'static {
    fn push(&mut self, chunk: &[u8]) -> Vec<Item>;
    fn finish(&self) -> Option<ProviderError>;
    fn request_id(&self) -> Option<String>;
    fn ended(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Waits {
    pub(crate) first_byte: Duration,
    pub(crate) idle: Duration,
}

pub(crate) type Opened<D> = (
    Vec<ProviderEvent>,
    Result<(reqwest::Response, D), ProviderError>,
);

enum Stage<R, O, D> {
    Request(R, O),
    Send(Box<reqwest::RequestBuilder>, O),
    Read(Box<Reading<D>>),
    Refused(VecDeque<Item>),
    Done,
}

struct Reading<D> {
    body: BoxStream<'static, reqwest::Result<Vec<u8>>>,
    decoder: D,
    queue: VecDeque<Item>,
    started: bool,
}

pub(crate) fn drive<R, O, F, D>(
    request: R,
    open: O,
    waits: Waits,
    redactions_generation: Option<u64>,
) -> BoxStream<'static, Item>
where
    R: Future<Output = Result<reqwest::RequestBuilder, ProviderError>> + Send + 'static,
    O: FnOnce(reqwest::Response) -> F + Send + 'static,
    F: Future<Output = Opened<D>> + Send + 'static,
    D: Decoder,
{
    stream::unfold(Stage::Request(request, open), move |mut stage| async move {
        loop {
            stage = match stage {
                Stage::Done => return None,
                Stage::Request(request, open) => {
                    return Some(
                        match request.await.and_then(|call| {
                            super::ensure_fresh_redactions(redactions_generation, false)?;
                            Ok(call)
                        }) {
                            Ok(call) => (
                                Ok(ProviderEvent::Sending),
                                Stage::Send(Box::new(call), open),
                            ),
                            Err(error) => (Err(error), Stage::Done),
                        },
                    );
                }
                Stage::Send(call, open) => {
                    let response =
                        match send(*call, waits.first_byte, redactions_generation, false).await {
                            Ok(response) => response,
                            Err(error) => return Some((Err(error), Stage::Done)),
                        };
                    let (lead, opened) = open(response).await;
                    let mut queue: VecDeque<Item> = lead.into_iter().map(Ok).collect();
                    match opened {
                        Ok((response, decoder)) => Stage::Read(Box::new(Reading {
                            body: response.bytes_stream().map_ok(|b| b.to_vec()).boxed(),
                            decoder,
                            queue,
                            started: false,
                        })),
                        Err(error) => {
                            queue.push_back(Err(error));
                            Stage::Refused(queue)
                        }
                    }
                }
                Stage::Refused(mut queue) => {
                    return queue.pop_front().map(|item| (item, Stage::Refused(queue)));
                }
                Stage::Read(mut reading) => {
                    let error = match reading.queue.pop_front() {
                        Some(Ok(event)) => return Some((Ok(event), Stage::Read(reading))),
                        Some(Err(error)) => error,
                        None if reading.decoder.ended() => return None,
                        None => {
                            let wait = if reading.started {
                                waits.idle
                            } else {
                                waits.first_byte
                            };
                            match tokio::time::timeout(wait, reading.body.next()).await {
                                Ok(Some(Ok(chunk))) => {
                                    reading.started = true;
                                    let items = reading.decoder.push(&chunk);
                                    reading.queue.extend(items);
                                    stage = Stage::Read(reading);
                                    continue;
                                }
                                Ok(Some(Err(e))) => interrupted(e),
                                Ok(None) => reading.decoder.finish()?,
                                Err(_) => stalled(wait),
                            }
                        }
                    };
                    let request_id = reading.decoder.request_id();
                    return Some((
                        Err(after_200(billed_in_stream(error), request_id)),
                        Stage::Done,
                    ));
                }
            };
        }
    })
    .boxed()
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_HTTP_SEND: std::cell::Cell<Option<Box<dyn FnOnce()>>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn before_http_send() {
    if let Some(before) = BEFORE_HTTP_SEND.take() {
        before();
    }
}

pub(crate) async fn send(
    call: reqwest::RequestBuilder,
    first_byte: Duration,
    redactions_generation: Option<u64>,
    after_auth_refresh: bool,
) -> Result<reqwest::Response, ProviderError> {
    let request = match redactions_generation {
        Some(generation) => crate::session::send_if_generation_current(generation, call, |call| {
            #[cfg(test)]
            before_http_send();
            call.send()
        })
        .map_err(|_| super::stale_redactions(after_auth_refresh))?,
        None => call.send(),
    };
    match tokio::time::timeout(first_byte, request).await {
        Ok(response) => response.map_err(send_failure),
        Err(_) => Err(no_response()),
    }
}

pub(crate) fn broken(kind: ErrorKind, message: impl Into<String>) -> ProviderError {
    ProviderError::classified(
        kind,
        message,
        classify::local(FailurePhase::Stream, RetryClass::Transient, true),
    )
}

pub(crate) fn interrupted(e: reqwest::Error) -> ProviderError {
    let kind = if e.is_timeout() {
        ErrorKind::Timeout
    } else {
        ErrorKind::Network
    };
    broken(kind, format!("stream interrupted: {}", e.without_url()))
}

fn stalled(wait: Duration) -> ProviderError {
    broken(
        ErrorKind::Timeout,
        format!(
            "the stream stalled: nothing arrived for {} s",
            wait.as_secs_f64().ceil()
        ),
    )
}

fn no_response() -> ProviderError {
    ProviderError::classified(
        ErrorKind::Timeout,
        "no response before the timeout",
        classify::local(FailurePhase::Response, RetryClass::Transient, true),
    )
}

pub(crate) fn send_failure(e: reqwest::Error) -> ProviderError {
    let e = e.without_url();
    if e.is_builder() {
        return ProviderError::classified(
            ErrorKind::InvalidRequest,
            format!("could not build the request (check the base URL): {e}"),
            classify::local(FailurePhase::Connect, RetryClass::Never, false),
        );
    }
    if e.is_connect() {
        return ProviderError::classified(
            ErrorKind::Network,
            format!("could not connect: {e}"),
            classify::local(FailurePhase::Connect, RetryClass::Transient, false),
        );
    }
    if e.is_timeout() {
        return no_response();
    }
    ProviderError::classified(
        ErrorKind::Network,
        format!("the connection failed after sending: {e}"),
        classify::local(FailurePhase::Response, RetryClass::Transient, true),
    )
}

pub(crate) fn billed_in_stream(error: ProviderError) -> ProviderError {
    match error.detail.phase {
        FailurePhase::Stream => error.possibly_billed(),
        FailurePhase::Connect | FailurePhase::Response => error,
    }
}

pub(crate) fn after_200(mut error: ProviderError, request_id: Option<String>) -> ProviderError {
    error.detail.status.get_or_insert(200);
    if error.detail.request_id.is_none() {
        classify::set_request_id(&mut error.detail, request_id.as_deref());
    }
    error
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(event: &str, data: &str) -> SseEvent {
        SseEvent {
            event: Some(event.into()),
            data: data.into(),
        }
    }

    fn events(items: Vec<Result<SseEvent, EventTooLarge>>) -> Vec<SseEvent> {
        items.into_iter().map(Result::unwrap).collect()
    }

    #[test]
    fn parses_events_and_skips_comments() {
        let mut p = SseParser::default();
        let out =
            events(p.push(b": hello\nevent: ping\ndata: {}\n\nevent: a\ndata: 1\ndata: 2\n\n"));
        assert_eq!(out, vec![ev("ping", "{}"), ev("a", "1\n2")]);
        assert!(!p.has_partial());
    }

    #[test]
    fn handles_crlf_and_arbitrary_chunking() {
        let input = "event: x\r\ndata: héllo wörld\r\n\r\nevent: y\r\ndata:no-space\r\n\r\n";
        let bytes = input.as_bytes();
        for split in 0..bytes.len() {
            let mut p = SseParser::default();
            let mut out = events(p.push(&bytes[..split]));
            out.extend(events(p.push(&bytes[split..])));
            assert_eq!(
                out,
                vec![ev("x", "héllo wörld"), ev("y", "no-space")],
                "split {split}"
            );
        }
    }

    #[test]
    fn small_pushes_split_lf_and_crlf_delimiters_anywhere() {
        let input = "event: a\ndata: 1\n\nevent: b\r\ndata: 2\r\ndata: 3\r\n\r\n: c\r\nevent: d\ndata: é\r\n\n";
        let bytes = input.as_bytes();
        let whole = events(SseParser::default().push(bytes));
        assert_eq!(whole, vec![ev("a", "1"), ev("b", "2\n3"), ev("d", "é")]);
        for size in [1, 2, 3] {
            for offset in 0..size {
                let mut p = SseParser::default();
                let mut out = events(p.push(&bytes[..offset]));
                for chunk in bytes[offset..].chunks(size) {
                    out.extend(events(p.push(chunk)));
                }
                assert_eq!(out, whole, "size {size} offset {offset}");
                assert!(!p.has_partial());
            }
        }
        for first in 0..bytes.len() {
            for second in first..bytes.len() {
                let mut p = SseParser::default();
                let mut out = events(p.push(&bytes[..first]));
                out.extend(events(p.push(&bytes[first..second])));
                out.extend(events(p.push(&bytes[second..])));
                assert_eq!(out, whole, "splits {first} {second}");
            }
        }
    }

    #[test]
    fn reports_partial_event() {
        let mut p = SseParser::default();
        assert!(p.push(b"event: x\ndata: {\"a\":").is_empty());
        assert!(p.has_partial());
    }

    #[test]
    fn event_without_data_is_not_dispatched() {
        let mut p = SseParser::default();
        assert!(p.push(b"event: x\n\n").is_empty());
    }

    #[test]
    fn exactly_the_cap_is_held_and_one_byte_more_is_refused() {
        let mut p = SseParser::default();
        assert!(p.push(&vec![b'a'; MAX_EVENT]).is_empty());
        assert_eq!(p.pending_bytes(), MAX_EVENT);
        assert_eq!(p.push(b"a"), vec![Err(EventTooLarge)]);
        let mut p = SseParser::default();
        assert_eq!(p.push(&vec![b'a'; MAX_EVENT + 1]), vec![Err(EventTooLarge)]);
    }

    #[test]
    fn data_lines_count_toward_the_cap_until_the_blank_line() {
        let mut p = SseParser::default();
        let line = format!("data: {}\n", "a".repeat(1024 * 1024 - 7));
        for _ in 0..16 {
            assert!(p.push(line.as_bytes()).is_empty());
        }
        assert!(p.pending_bytes() < MAX_EVENT);
        assert_eq!(p.push(line.as_bytes()), vec![Err(EventTooLarge)]);
    }

    struct Quiet;

    impl Decoder for Quiet {
        fn push(&mut self, _: &[u8]) -> Vec<Item> {
            Vec::new()
        }
        fn finish(&self) -> Option<ProviderError> {
            None
        }
        fn request_id(&self) -> Option<String> {
            None
        }
    }

    #[tokio::test]
    async fn drive_waits_the_first_byte_wait_for_headers_and_for_the_first_chunk() {
        let waits = Waits {
            first_byte: Duration::from_millis(50),
            idle: Duration::from_secs(10),
        };
        let ok = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n";
        for (head, phase, status) in [
            (String::new(), FailurePhase::Response, None),
            (ok.to_owned(), FailurePhase::Stream, Some(200)),
        ] {
            let (addr, server) = crate::test_guard::holding_server(head).await;
            let call = reqwest::Client::new().post(format!("http://{addr}"));
            let stream = drive(
                async { Ok(call) },
                |response| async { (Vec::new(), Ok((response, Quiet))) },
                waits,
                None,
            );
            let items: Vec<_> = tokio::time::timeout(Duration::from_secs(5), stream.collect())
                .await
                .unwrap();
            server.abort();
            let [Ok(ProviderEvent::Sending), Err(error)] = items.as_slice() else {
                panic!("{items:?}")
            };
            let detail = &error.detail;
            assert_eq!(
                (error.kind, detail.phase, detail.retry, detail.status),
                (ErrorKind::Timeout, phase, RetryClass::Transient, status)
            );
            assert!(error.possibly_billed);
        }
    }

    #[test]
    fn an_event_whose_text_outgrows_its_bytes_is_refused_in_order() {
        let mut p = SseParser::default();
        let mut chunk = b"event: a\ndata: 1\n\ndata: ".to_vec();
        chunk.extend(vec![0xff; MAX_EVENT / 2]);
        chunk.extend_from_slice(b"\n\nevent: b\ndata: 2\n\n");
        assert_eq!(p.push(&chunk), vec![Ok(ev("a", "1")), Err(EventTooLarge)]);
    }

    #[tokio::test]
    async fn send_orders_http_handoff_with_secret_registration_drops_guard_before_network_io_and_skips_lock_when_unchecked()
     {
        use std::sync::atomic::{AtomicBool, Ordering};
        use tokio::io::AsyncWriteExt;

        crate::session::reset_held_for_test();
        let canary = "CANARYOpaqueAtSseHttpSend";
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let client = reqwest::Client::new();

        let (at_send_tx, at_send_rx) = std::sync::mpsc::channel();
        let (probed_tx, probed_rx) = std::sync::mpsc::channel();
        let (reg_done_tx, reg_done_rx) = tokio::sync::oneshot::channel();
        let worker = std::thread::spawn(move || {
            at_send_rx.recv().unwrap();
            let blocked = crate::session::secrets_write_blocked();
            probed_tx.send(()).unwrap();
            crate::session::register_secrets([canary.to_owned()]);
            let _ = reg_done_tx.send(());
            blocked
        });
        BEFORE_HTTP_SEND.set(Some(Box::new(move || {
            at_send_tx.send(()).unwrap();
            probed_rx.recv().unwrap();
        })));
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = crate::test_guard::http::read_request(&mut socket)
                .await
                .unwrap();
            assert!(request.starts_with("POST / "), "{request}");
            reg_done_rx.await.unwrap();
            let blocked_after_reg = crate::session::secrets_write_blocked();
            socket
                .write_all(crate::test_guard::http::response("200 OK", &[], "").as_bytes())
                .await
                .unwrap();
            socket.flush().await.unwrap();
            socket.shutdown().await.unwrap();
            blocked_after_reg
        });
        let generation = crate::session::held_generation();
        let response = send(
            client.post(format!("http://{addr}")),
            Duration::from_secs(5),
            Some(generation),
            false,
        )
        .await
        .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert!(response.bytes().await.unwrap().is_empty());
        assert!(worker.join().unwrap());
        assert!(!server.await.unwrap());

        let stale_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let stale_addr = stale_listener.local_addr().unwrap();
        for after_auth_refresh in [false, true] {
            let hit = std::sync::Arc::new(AtomicBool::new(false));
            let seen_hit = hit.clone();
            BEFORE_HTTP_SEND.set(Some(Box::new(move || {
                seen_hit.store(true, Ordering::Release);
            })));
            let err = send(
                client.post(format!("http://{stale_addr}")),
                Duration::from_secs(5),
                Some(generation),
                after_auth_refresh,
            )
            .await
            .unwrap_err();
            assert_eq!(
                err.refused,
                Some(crate::provider::Refused::StaleRequest { after_auth_refresh })
            );
            assert!(!err.possibly_billed);
            assert!(!hit.load(Ordering::Acquire));
            assert!(futures::poll!(std::pin::pin!(stale_listener.accept())).is_pending());
        }

        let unchecked_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let unchecked_addr = unchecked_listener.local_addr().unwrap();
        let ((), snapshots, reads) = crate::session::count_held_loads(|| {
            use std::future::Future;
            let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
            let mut fut = std::pin::pin!(send(
                client.post(format!("http://{unchecked_addr}")),
                Duration::from_secs(5),
                None,
                false,
            ));
            let _ = fut.as_mut().poll(&mut cx);
        });
        assert_eq!((snapshots, reads), (0, 0));
    }
}
