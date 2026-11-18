use wisp_plugin_api::manifest::Capability;

use super::*;
use crate::plugin::http::tests::{Recording, wisp_agent};
use crate::test_guard::http::{Heard, Reply, header_lines, response, serve};

fn fetch_agent(provider: Arc<Fake>, env: &Env, grace: Duration) -> Agent {
    let toolbox = crate::tool::testkit::toolbox(&["fetch"]);
    agent_with(provider, env, toolbox, grace)
}

#[tokio::test]
async fn a_plugin_fetch_goes_through_the_agents_gate_and_sends_wisps_user_agent() {
    let mut server = serve(vec![Reply::Send(response(
        "200 OK",
        &[("x-from", "server")],
        "hello",
    ))])
    .await;
    let env = Env::new("plugin-http-gate");
    let url = format!("{}/page", server.url);
    let provider = scripted(vec![
        uses(&[call(
            "f",
            "fetch",
            json!({"url": url, "headers": [["User-Agent", "someone@example.com"]]}),
        )]),
        done("fetched"),
    ]);
    let gate = Arc::new(Recording::default());
    let mut agent = fetch_agent(provider, &env, GRACE).with_gate(gate.clone());
    let (outcome, events) = turn(&mut agent, "go").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{events:#?}"
    );
    let Heard::Request(request) = server.next().await else {
        panic!("no request");
    };
    let agents: Vec<String> = header_lines(&request)
        .into_iter()
        .filter(|(name, _)| name == "user-agent")
        .map(|(_, value)| value)
        .collect();
    assert_eq!(agents, [wisp_agent()]);
    assert_eq!(
        *gate.0.lock().unwrap(),
        [("testkit".to_owned(), Capability::Net, url)]
    );
    let history = agent.history();
    let (_, text, is_error) = results(&history[2])[0];
    assert!(!is_error, "{text}");
    assert!(text.starts_with("HTTP 200\n"), "{text}");
    assert!(text.contains("x-from: server\n"), "{text}");
    assert!(text.ends_with("\n\nhello"), "{text}");
}

#[tokio::test]
async fn esc_during_a_plugin_fetch_cancels_the_request_at_once() {
    let mut server = serve(vec![Reply::Hang]).await;
    let env = Env::new("plugin-http-esc");
    let provider = scripted(vec![
        uses(&[call("f", "fetch", json!({"url": server.url}))]),
        done("unused"),
    ]);
    let grace = Duration::from_secs(30);
    let mut agent = fetch_agent(provider, &env, grace);
    let (cmd_tx, mut cmd_rx) = mpsc::channel(4);
    let (ev_tx, mut ev_rx) = mpsc::channel(1024);
    let collector = tokio::spawn(async move {
        let mut seen = Vec::new();
        while let Some(e) = ev_rx.recv().await {
            seen.push(e);
        }
        seen
    });
    let esc = tokio::spawn(async move {
        assert!(matches!(server.next().await, Heard::Request(_)));
        let pressed = tokio::time::Instant::now();
        cmd_tx.send(ClientCommand::Cancel).await.unwrap();
        let closed = server.next().await;
        (closed, pressed, cmd_tx)
    });
    agent
        .run_turn("go".into(), &mut cmd_rx, &ev_tx)
        .await
        .unwrap();
    let ended = tokio::time::Instant::now();
    drop(ev_tx);
    let events = collector.await.unwrap();
    let (closed, pressed, _cmd_tx) = esc.await.unwrap();
    assert_eq!(closed, Heard::Closed);
    assert!(
        ended - pressed < Duration::from_secs(10),
        "{:?}",
        ended - pressed
    );
    assert_eq!(
        finished(&events),
        [("f".to_owned(), ToolOutcome::Cancelled)]
    );
    let history = agent.history();
    let (_, text, _) = results(&history[2])[0];
    assert!(!text.contains(crate::agent::ABANDONED_NOTE), "{text}");
}
