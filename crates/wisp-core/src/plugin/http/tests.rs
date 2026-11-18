use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::net::TcpListener;
use wisp_plugin_api::http::Http;

use super::*;
use crate::plugin::{KernelHost, PluginHost, Snapshot};
use crate::policy::GateCall;
use crate::test_guard::http::{Heard, Reply, body, header_lines, response, serve};
use crate::test_guard::refusing_addr;

fn values(request: &str, name: &str) -> Vec<String> {
    header_lines(request)
        .into_iter()
        .filter(|(n, _)| n == name)
        .map(|(_, v)| v)
        .collect()
}

fn names(request: &str) -> Vec<String> {
    let mut names: Vec<String> = header_lines(request).into_iter().map(|(n, _)| n).collect();
    names.sort();
    names.dedup();
    names
}

pub(crate) fn wisp_agent() -> String {
    format!("Wisp/{}", env!("CARGO_PKG_VERSION"))
}

#[derive(Default)]
pub(crate) struct Recording(pub(crate) Mutex<Vec<(String, Capability, String)>>);

impl Gate for Recording {
    fn check(&self, _call: &GateCall<'_>) -> Verdict {
        Verdict::Allow
    }

    fn check_host(&self, call: &HostCall<'_>) -> Verdict {
        self.0.lock().unwrap().push((
            call.plugin.to_owned(),
            call.capability,
            call.target.to_owned(),
        ));
        Verdict::Allow
    }
}

fn host() -> (PluginHost, Arc<Recording>) {
    let kernel = Arc::new(KernelHost::new());
    let gate = Arc::new(Recording::default());
    kernel.publish(Snapshot {
        gate: gate.clone(),
        ..Snapshot::default()
    });
    (PluginHost::new("probe", "0.2.0", &[], kernel), gate)
}

fn get(url: String) -> Request {
    Request {
        method: "GET".to_owned(),
        url,
        ..Request::default()
    }
}

async fn read_all(response: &Response, max: u32) -> (Vec<u8>, usize) {
    let mut body = Vec::new();
    let mut reads = 0;
    while let Some(chunk) = response.body.read(max).await.unwrap() {
        assert!(chunk.len() <= max as usize, "{} > {max}", chunk.len());
        body.extend(chunk);
        reads += 1;
    }
    (body, reads)
}

#[tokio::test]
async fn the_method_headers_and_body_go_out_and_the_status_headers_and_body_come_back() {
    let long = "0123456789".repeat(100);
    let mut server = serve(vec![Reply::Send(response(
        "201 Created",
        &[("x-reply", "one"), ("x-reply", "two")],
        &long,
    ))])
    .await;
    let (host, _) = host();
    let sent = host
        .fetch(Request {
            method: "PUT".to_owned(),
            url: format!("{}/things/7?mode=full", server.url),
            headers: vec![
                ("X-One".to_owned(), "a".to_owned()),
                ("x-two".to_owned(), "b".to_owned()),
                ("x-two".to_owned(), "c".to_owned()),
                ("Content-Type".to_owned(), "text/plain".to_owned()),
            ],
            body: Some(b"the body".to_vec()),
            timeout_ms: None,
        })
        .await
        .unwrap();
    let Heard::Request(request) = server.next().await else {
        panic!("no request");
    };
    assert!(
        request.starts_with("PUT /things/7?mode=full HTTP/1.1\r\n"),
        "{request}"
    );
    assert_eq!(values(&request, "x-one"), ["a"]);
    assert_eq!(values(&request, "x-two"), ["b", "c"]);
    assert_eq!(values(&request, "content-type"), ["text/plain"]);
    assert_eq!(body(&request), "the body");
    assert_eq!(sent.status, 201);
    let replies: Vec<&str> = sent
        .headers
        .iter()
        .filter(|(name, _)| name == "x-reply")
        .map(|(_, value)| value.as_str())
        .collect();
    assert_eq!(replies, ["one", "two"]);
    let (body, reads) = read_all(&sent, 64).await;
    assert_eq!(String::from_utf8(body).unwrap(), long);
    assert!(reads >= 16, "{reads}");
    assert_eq!(sent.body.read(64).await, Ok(None));
}

#[tokio::test]
async fn the_user_agent_is_wisp_and_its_version_whatever_the_plugin_sends() {
    let ok = || Reply::Send(response("200 OK", &[], ""));
    let mut server = serve(vec![ok(), ok()]).await;
    let (host, _) = host();
    let own = Request {
        headers: vec![
            ("User-Agent".to_owned(), "someone@example.com".to_owned()),
            ("USER-AGENT".to_owned(), "curl/8".to_owned()),
        ],
        ..get(server.url.clone())
    };
    for request in [own, get(server.url.clone())] {
        host.fetch(request).await.unwrap();
        let Heard::Request(seen) = server.next().await else {
            panic!("no request");
        };
        assert_eq!(values(&seen, "user-agent"), [wisp_agent()], "{seen}");
        assert!(!seen.contains("example.com"), "{seen}");
        assert!(!seen.contains("curl"), "{seen}");
    }
    assert_eq!(USER_AGENT, wisp_agent());
    assert!(!USER_AGENT.contains(' '));
}

#[tokio::test]
async fn the_host_adds_no_cookie_auth_or_other_header() {
    let mut server = serve(vec![
        Reply::Send(response(
            "200 OK",
            &[("set-cookie", "session=CANARYCOOKIE; Path=/")],
            "",
        )),
        Reply::Send(response("200 OK", &[], "")),
    ])
    .await;
    let (host, _) = host();
    let first = host.fetch(get(server.url.clone())).await.unwrap();
    assert!(
        first
            .headers
            .iter()
            .any(|(n, v)| n == "set-cookie" && v.contains("CANARYCOOKIE"))
    );
    server.next().await;
    host.fetch(get(format!("{}/again", server.url)))
        .await
        .unwrap();
    let Heard::Request(second) = server.next().await else {
        panic!("no request");
    };
    assert!(!second.contains("CANARYCOOKIE"), "{second}");
    assert_eq!(names(&second), ["accept", "host", "user-agent"], "{second}");
    assert_eq!(values(&second, "accept"), ["*/*"]);
}

#[tokio::test]
async fn a_redirect_comes_back_as_the_response_and_nothing_reaches_its_target() {
    let mut target = serve(vec![Reply::Send(response("200 OK", &[], "landed"))]).await;
    let location = format!("{}/landing", target.url);
    let statuses = [
        "301 Moved Permanently",
        "302 Found",
        "303 See Other",
        "307 Temporary Redirect",
        "308 Permanent Redirect",
    ];
    let replies = statuses
        .iter()
        .map(|status| Reply::Send(response(status, &[("location", &location)], "moved")))
        .collect();
    let mut server = serve(replies).await;
    let (host, gate) = host();
    for status in statuses {
        let sent = host
            .fetch(Request {
                headers: vec![("authorization".to_owned(), "Bearer CANARY".to_owned())],
                timeout_ms: Some(10_000),
                ..get(format!("{}/start", server.url))
            })
            .await
            .unwrap();
        let code: u16 = status[..3].parse().unwrap();
        assert_eq!(sent.status, code);
        server.next().await;
        let found: Vec<&str> = sent
            .headers
            .iter()
            .filter(|(n, _)| n == "location")
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(found, [location.as_str()]);
        let (body, _) = read_all(&sent, 1024).await;
        assert_eq!(body, b"moved");
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(300), target.heard.recv())
            .await
            .is_err(),
        "the redirect's target was reached"
    );
    let targets: Vec<String> = gate.0.lock().unwrap().iter().map(|c| c.2.clone()).collect();
    assert_eq!(targets, vec![format!("{}/start", server.url); 5]);
}

#[tokio::test]
async fn a_same_origin_redirect_is_not_followed_either() {
    let mut server = serve(vec![
        Reply::Send(response("302 Found", &[("location", "/landing")], "")),
        Reply::Send(response("200 OK", &[], "landed")),
    ])
    .await;
    let (host, gate) = host();
    let sent = host
        .fetch(Request {
            timeout_ms: Some(10_000),
            ..get(format!("{}/start", server.url))
        })
        .await
        .unwrap();
    assert_eq!(sent.status, 302);
    let Heard::Request(first) = server.next().await else {
        panic!("no request");
    };
    assert!(first.starts_with("GET /start "), "{first}");
    let landed = host
        .fetch(get(format!("{}/landing", server.url)))
        .await
        .unwrap();
    let (body, _) = read_all(&landed, 1024).await;
    assert_eq!(body, b"landed", "the host had already sent /landing");
    let Heard::Request(second) = server.next().await else {
        panic!("no request");
    };
    assert!(second.starts_with("GET /landing "), "{second}");
    assert_eq!(gate.0.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn a_request_times_out_at_its_own_limit_or_the_callers_deadline() {
    let mut server = serve(vec![Reply::Hang, Reply::Hang]).await;
    let (host, _) = host();
    let started = tokio::time::Instant::now();
    let out = host
        .fetch(Request {
            timeout_ms: Some(1_500),
            ..get(server.url.clone())
        })
        .await;
    assert_eq!(out.err(), Some(HttpError::TimedOut));
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(matches!(server.next().await, Heard::Request(_)));
    assert_eq!(server.next().await, Heard::Closed);
    let started = tokio::time::Instant::now();
    let caller = Caller::call(
        CancellationToken::new(),
        Some(tokio::time::Instant::now() + Duration::from_millis(1_500)),
    );
    let out = caller
        .scope(host.fetch(Request {
            timeout_ms: Some(60_000),
            ..get(server.url.clone())
        }))
        .await;
    assert_eq!(out.err(), Some(HttpError::TimedOut));
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(matches!(server.next().await, Heard::Request(_)));
    assert_eq!(server.next().await, Heard::Closed);
}

fn stalling() -> Reply {
    Reply::HeadThenHang("HTTP/1.1 200 OK\r\ncontent-length: 100\r\n\r\nfirst".to_owned())
}

#[tokio::test]
async fn a_body_that_stalls_times_out_while_reading_and_drops_the_connection() {
    let mut server = serve(vec![stalling()]).await;
    let (host, _) = host();
    let sent = host
        .fetch(Request {
            timeout_ms: Some(1_500),
            ..get(server.url.clone())
        })
        .await
        .unwrap();
    assert_eq!(sent.body.read(1024).await, Ok(Some(b"first".to_vec())));
    assert_eq!(sent.body.read(1024).await, Err(BODY_TIMED_OUT.to_owned()));
    assert!(matches!(server.next().await, Heard::Request(_)));
    assert_eq!(server.next().await, Heard::Closed);
    assert_eq!(sent.body.read(1024).await, Err(BODY_TIMED_OUT.to_owned()));
}

#[tokio::test]
async fn cancelling_during_a_body_read_ends_it_at_once_and_drops_the_connection() {
    let mut server = serve(vec![stalling()]).await;
    let (host, _) = host();
    let token = CancellationToken::new();
    let caller = Caller::call(token.clone(), None);
    let sent = caller
        .clone()
        .scope(host.fetch(get(server.url.clone())))
        .await
        .unwrap();
    assert_eq!(sent.body.read(1024).await, Ok(Some(b"first".to_vec())));
    assert!(matches!(server.next().await, Heard::Request(_)));
    let reading = caller.scope(sent.body.read(1024));
    let cancel = async {
        tokio::task::yield_now().await;
        token.cancel();
    };
    let (read, ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(reading, cancel)
    })
    .await
    .unwrap();
    assert_eq!(read, Err(BODY_CANCELLED.to_owned()));
    assert_eq!(server.next().await, Heard::Closed);
    assert_eq!(sent.body.read(1024).await, Err(BODY_CANCELLED.to_owned()));
    assert_eq!(sent.status, 200);
}

#[tokio::test]
async fn cancelling_the_caller_ends_the_request_and_closes_the_connection() {
    let mut server = serve(vec![Reply::Hang]).await;
    let (host, _) = host();
    let token = CancellationToken::new();
    let caller = Caller::call(token.clone(), None);
    let fetch = tokio::spawn({
        let url = server.url.clone();
        async move { caller.scope(host.fetch(get(url))).await.err() }
    });
    assert!(matches!(server.next().await, Heard::Request(_)));
    let started = tokio::time::Instant::now();
    token.cancel();
    assert_eq!(fetch.await.unwrap(), Some(HttpError::Cancelled));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(server.next().await, Heard::Closed);
}

#[tokio::test]
async fn dropping_the_fetch_closes_the_connection() {
    let mut server = serve(vec![Reply::Hang]).await;
    let (host, _) = host();
    let fetch = tokio::spawn({
        let url = server.url.clone();
        async move { host.fetch(get(url)).await.is_ok() }
    });
    assert!(matches!(server.next().await, Heard::Request(_)));
    fetch.abort();
    assert_eq!(server.next().await, Heard::Closed);
}

#[tokio::test]
async fn a_cancelled_caller_sends_nothing() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let (host, _) = host();
    let token = CancellationToken::new();
    token.cancel();
    let out = Caller::call(token, None).scope(host.fetch(get(url))).await;
    assert_eq!(out.err(), Some(HttpError::Cancelled));
    assert!(
        tokio::time::timeout(Duration::from_millis(300), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn http_is_refused_inside_a_slot_render_and_a_start_render_and_works_outside() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let (host, gate) = host();
    for (render, message) in [
        (Render::Slot, IN_SLOT_RENDER),
        (Render::Start, IN_START_RENDER),
    ] {
        let out = Caller::rendering(render)
            .scope(host.fetch(get(url.clone())))
            .await;
        assert_eq!(out.err(), Some(HttpError::NotAllowed(message.to_owned())));
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(300), listener.accept())
            .await
            .is_err(),
        "a render reached the network"
    );
    assert!(gate.0.lock().unwrap().is_empty());
    drop(listener);
    let mut server = serve(vec![Reply::Send(response("200 OK", &[], "held"))]).await;
    let sent = host.fetch(get(server.url.clone())).await.unwrap();
    assert_eq!(sent.status, 200);
    assert!(matches!(server.next().await, Heard::Request(_)));
    for (render, message) in [
        (Render::Slot, IN_SLOT_RENDER),
        (Render::Start, IN_START_RENDER),
    ] {
        let read = tokio::time::timeout(
            Duration::from_secs(10),
            Caller::rendering(render).scope(sent.body.read(1024)),
        )
        .await
        .unwrap();
        assert_eq!(read, Err(message.to_owned()));
    }
    let (body, _) = read_all(&sent, 1024).await;
    assert_eq!(body, b"held");
}

#[tokio::test]
async fn a_held_body_read_inside_a_render_is_refused_without_waiting() {
    let mut server = serve(vec![stalling()]).await;
    let (host, _) = host();
    let sent = host.fetch(get(server.url.clone())).await.unwrap();
    assert_eq!(sent.body.read(1024).await, Ok(Some(b"first".to_vec())));
    assert!(matches!(server.next().await, Heard::Request(_)));
    let read = tokio::time::timeout(
        Duration::from_secs(5),
        Caller::rendering(Render::Slot).scope(sent.body.read(1024)),
    )
    .await
    .expect("a render waited on the network");
    assert_eq!(read, Err(IN_SLOT_RENDER.to_owned()));
}

#[tokio::test]
async fn every_request_asks_the_gate_with_the_net_capability() {
    let mut server = serve(vec![
        Reply::Send(response("200 OK", &[], "")),
        Reply::Send(response("404 Not Found", &[], "")),
    ])
    .await;
    let (host, gate) = host();
    let first = format!("{}/a", server.url);
    let second = format!("{}/b?x=1", server.url);
    assert_eq!(host.fetch(get(first.clone())).await.unwrap().status, 200);
    assert_eq!(
        host.fetch(Request {
            method: "POST".to_owned(),
            body: Some(Vec::new()),
            ..get(second.clone())
        })
        .await
        .unwrap()
        .status,
        404
    );
    server.next().await;
    server.next().await;
    assert_eq!(
        *gate.0.lock().unwrap(),
        [
            ("probe".to_owned(), Capability::Net, first),
            ("probe".to_owned(), Capability::Net, second),
        ]
    );
}

#[tokio::test]
async fn only_http_and_https_are_fetched_and_bad_input_fails_before_sending() {
    let (host, gate) = host();
    for url in [
        "file:///etc/hostname",
        "ftp://127.0.0.1/x",
        "data:text/plain,hi",
        "ws://127.0.0.1:1/",
    ] {
        assert_eq!(
            host.fetch(get(url.to_owned())).await.err(),
            Some(HttpError::NotAllowed(ONLY_HTTP.to_owned())),
            "{url}"
        );
    }
    assert!(matches!(
        host.fetch(get("not a url".to_owned())).await.err(),
        Some(HttpError::Failed(_))
    ));
    assert!(gate.0.lock().unwrap().is_empty());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let bad = [
        Request {
            method: "GE T".to_owned(),
            ..get(url.clone())
        },
        Request {
            headers: vec![("bad name".to_owned(), "v".to_owned())],
            ..get(url.clone())
        },
        Request {
            headers: vec![("x-ok".to_owned(), "line\nbreak".to_owned())],
            ..get(url.clone())
        },
    ];
    for request in bad {
        assert!(matches!(
            host.fetch(request).await.err(),
            Some(HttpError::Failed(_))
        ));
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(200), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_failure_gives_its_reason_but_never_the_url() {
    let (_socket, addr) = refusing_addr();
    let url = format!("http://{addr}/search?key=CANARYKEY");
    let (host, _) = host();
    match host.fetch(get(url)).await.err() {
        Some(HttpError::Failed(message)) => {
            assert!(!message.is_empty());
            assert!(!message.contains("CANARYKEY"), "{message}");
            assert!(!message.contains("/search"), "{message}");
        }
        other => panic!("{other:?}"),
    }
    for bad in ["http://exa mple.com/?key=CANARYKEY", "CANARYKEY"] {
        match host.fetch(get(bad.to_owned())).await.err() {
            Some(HttpError::Failed(message)) => {
                assert!(!message.contains("CANARYKEY"), "{message}");
            }
            other => panic!("{other:?}"),
        }
    }
}

#[tokio::test]
async fn the_gate_sees_the_url_without_its_user_and_password() {
    let mut server = serve(vec![Reply::Send(response("200 OK", &[], ""))]).await;
    let (host, gate) = host();
    let address = server.url.trim_start_matches("http://").to_owned();
    host.fetch(get(format!("http://someone:CANARYPASS@{address}/x?q=1")))
        .await
        .unwrap();
    server.next().await;
    assert_eq!(
        *gate.0.lock().unwrap(),
        [(
            "probe".to_owned(),
            Capability::Net,
            format!("http://{address}/x?q=1")
        )]
    );
}

#[tokio::test]
async fn adding_an_mcp_server_asks_the_exec_gate_after_the_secret_guard_and_before_starting() {
    use wisp_plugin_api::mcp::Mcp;
    let kernel = Arc::new(KernelHost::new());
    let gate = Arc::new(Recording::default());
    kernel.publish(Snapshot {
        gate: gate.clone(),
        ..Snapshot::default()
    });
    let host = PluginHost::new("mcp", "0.4.0", &[], kernel)
        .origin(wisp_plugin_api::manifest::Origin::Bundled);
    let secret = "CANARY_NOT_A_REAL_KEY".to_owned();
    assert!(
        host.add("local", "server", &["--token".into(), secret])
            .await
            .is_err()
    );
    assert!(gate.0.lock().unwrap().is_empty());
    assert!(
        host.add("local", "wisp-mcp-missing", &["--stdio".into()])
            .await
            .is_err()
    );
    assert_eq!(
        *gate.0.lock().unwrap(),
        [(
            "mcp".into(),
            Capability::Exec,
            "wisp-mcp-missing --stdio".into()
        )]
    );
}
