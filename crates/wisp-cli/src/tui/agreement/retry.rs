use std::sync::atomic::{AtomicUsize, Ordering};

use wisp_protocol::{RejectKind, RequestId, RetryOffer, TurnOutcome};
use wisp_render::chrome::Menu;
use wisp_tui::app::retry::{Choice, Ctx, Failed, Row as Offered};

use super::*;

fn refused(status: &str, kind: &str, extra: &str) -> String {
    let body = json!({"type": "error", "error": {"type": kind, "message": "no"}}).to_string();
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n{extra}content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn long_wait() -> String {
    refused(
        "429 Too Many Requests",
        "rate_limit_error",
        "retry-after: 3600\r\n",
    )
}

fn bad_key() -> String {
    refused("401 Unauthorized", "authentication_error", "")
}

impl Harness {
    async fn serve(&mut self, command: ClientCommand) -> Vec<AgentEvent> {
        let (tx, mut rx) = mpsc::channel(8);
        let (ev_tx, mut ev_rx) = mpsc::channel(1024);
        tx.send(command).await.unwrap();
        let mut seen = vec![];
        {
            let serve = self.agent.serve(&mut rx, &ev_tx);
            tokio::pin!(serve);
            loop {
                tokio::select! {
                    () = &mut serve => break,
                    event = ev_rx.recv() => {
                        let Some(event) = event else { break };
                        let done = matches!(
                            event,
                            AgentEvent::TurnEnded { .. } | AgentEvent::Rejected { .. }
                        );
                        seen.push(event);
                        if done {
                            break;
                        }
                    }
                }
            }
        }
        while let Ok(event) = ev_rx.try_recv() {
            seen.push(event);
        }
        drop(tx);
        self.feed(&seen);
        seen
    }

    async fn send(&mut self, text: &str, turn: u64) -> (u64, TurnOutcome) {
        let events = self
            .serve(ClientCommand::send(format!("tui-{turn}"), text))
            .await;
        ended_turn(&events)
    }

    fn ctx(&self) -> Ctx<'_> {
        Ctx {
            facts: &self.facts,
            tracker: &self.tracker,
            at: Duration::ZERO,
            wall: 0,
            set: UNI,
        }
    }

    fn choice(&self, turn: u64, outcome: &TurnOutcome) -> Choice {
        let failed = Failed::of(turn, outcome, self.tracker.session()).unwrap();
        let mut choice = Choice::default();
        choice.offered(failed, 0, None);
        choice
    }

    fn rows(&self, choice: &Choice) -> Vec<Offered> {
        choice
            .failed()
            .unwrap()
            .rows(&self.tracker.wanted().endpoint, 0)
    }

    fn picker(&self, choice: &mut Choice) -> Menu {
        choice.picking(None);
        choice.menu(&self.ctx()).unwrap()
    }

    async fn refuses(&mut self, turn: u64, endpoint: Option<&str>, label: &str) {
        let events = self
            .serve(ClientCommand::Retry {
                turn,
                endpoint: endpoint.map(str::to_owned),
                request: RequestId(90),
            })
            .await;
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::Rejected {
                    kind: Some(RejectKind::NotRetryable),
                    request: Some(RequestId(90)),
                    ..
                }
            )),
            "{label}: {events:?}"
        );
    }

    async fn retries(&mut self, choice: &Choice, endpoint: Option<&str>, label: &str) {
        let request = RequestId(91);
        let command = choice
            .command(endpoint.map(str::to_owned), request)
            .unwrap();
        let events = self.serve(command).await;
        let started = events.iter().find_map(|e| match e {
            AgentEvent::TurnStarted {
                retry_of, answers, ..
            } => Some((*retry_of, *answers)),
            _ => None,
        });
        let of = choice.failed().unwrap().turn;
        assert_eq!(
            started,
            Some((Some(of), Some(request))),
            "{label}: {events:?}"
        );
        let session = self.agent.session();
        let shown = self.tracker.session();
        assert_eq!(shown.endpoint, self.agent.endpoint_name(), "{label}");
        assert_eq!(shown.model, session.model, "{label}");
        assert_eq!(shown.effort, Some(session.effort), "{label}");
        let pending = self
            .agent
            .pending_settings()
            .map(|(e, m, f)| (e.to_owned(), m.to_owned(), f));
        let next = self.tracker.next();
        let shown_pending = (next != shown).then(|| {
            (
                next.endpoint.clone(),
                next.model.clone(),
                next.effort.unwrap_or(session.effort),
            )
        });
        assert_eq!(shown_pending, pending, "{label}");
        self.checked += 1;
    }
}

fn ended_turn(events: &[AgentEvent]) -> (u64, TurnOutcome) {
    let turn = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::TurnStarted { turn, .. } => Some(*turn),
            _ => None,
        })
        .unwrap();
    let outcome = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::TurnEnded { outcome, .. } => Some(outcome.clone()),
            _ => None,
        })
        .unwrap();
    (turn, outcome)
}

fn offer_of(outcome: &TurnOutcome) -> RetryOffer {
    match outcome {
        TurnOutcome::Error {
            offer: Some(offer), ..
        } => offer.clone(),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn the_retry_choice_offers_only_what_the_kernel_accepts() {
    let sides = [("direct", Endpoint::Official), ("gw", Endpoint::Custom)];
    let seq = Arc::new(AtomicUsize::new(0));
    let script = Arc::clone(&seq);
    let mut h = Harness::endpoints(
        &sides,
        "direct",
        OPUS,
        Effort::Medium,
        move |r, _| match script.fetch_add(1, Ordering::SeqCst) {
            1 | 5 => long_wait(),
            3 => bad_key(),
            n => answer(&model_of(r), n),
        },
    );
    h.agent = h.agent.with_retry_policy(wisp_core::agent::retry::quick());
    h.send("hello", 1).await;

    let (turn, outcome) = h.send("go", 2).await;
    let offer = offer_of(&outcome);
    let choice = h.choice(turn, &outcome);
    assert_eq!(h.rows(&choice), [Offered::Retry, Offered::Switch]);
    assert!(offer.retry && offer.switch);
    h.refuses(turn, Some(UNAVAILABLE), "an unavailable endpoint")
        .await;
    h.retries(&choice, None, "a long wait, retried where it failed")
        .await;

    let (turn, outcome) = h.send("again", 4).await;
    let mut choice = h.choice(turn, &outcome);
    assert_eq!(h.rows(&choice), [Offered::Retry, Offered::Switch]);
    let menu = h.picker(&mut choice);
    let glued = |name: &str| format!("\u{25cf}\u{a0}{name}");
    let enabled: Vec<(String, bool)> = menu
        .choices
        .iter()
        .map(|c| (c.label.trim_start().to_owned(), c.enabled))
        .collect();
    assert_eq!(enabled, [(glued("gw"), true), (glued(UNAVAILABLE), false)]);
    let picked = menu
        .selected
        .and_then(|i| menu.choices.get(i))
        .map(|c| c.label.trim_start().to_owned());
    assert_eq!(picked, Some(glued("gw")));
    h.retries(&choice, Some("gw"), "auth, switched to the picked endpoint")
        .await;

    let (turn, outcome) = h.send("once more", 6).await;
    h.change(None, Some(Effort::High)).await;
    h.switch("direct").await;
    let choice = h.choice(turn, &outcome);
    assert_eq!(h.rows(&choice), [Offered::Retry, Offered::Switch]);
    let shown = choice.shown(&h.ctx(), false);
    let Some(wisp_render::transcript::Choice::Open { rows, .. }) = shown else {
        panic!("{shown:?}");
    };
    assert!(rows[0].text.starts_with("on direct"), "{rows:?}");
    h.retries(&choice, None, "a pending endpoint, effort kept pending")
        .await;
    assert_eq!(h.agent.session().effort, Effort::Medium);
    assert_eq!(h.tracker.next().effort, Some(Effort::High));
    assert_eq!(seq.load(Ordering::SeqCst), 7);
    assert_eq!(h.checked, 3);
}
