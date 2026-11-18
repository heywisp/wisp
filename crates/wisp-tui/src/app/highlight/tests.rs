use wisp_render::styled::Look;
use wisp_render::theme::Token;

use super::*;

fn delta(fence: usize, lang: &str, from: usize, lines: &[&str]) -> FenceDelta {
    FenceDelta {
        fence,
        lang: Some(lang.to_owned()),
        from,
        lines: lines.iter().map(|l| (*l).to_owned()).collect(),
    }
}

fn feed(deltas: Vec<FenceDelta>) -> CodeFeed {
    CodeFeed {
        deltas,
        retire_below: 0,
        restart: false,
    }
}

fn next_updates(h: &Highlights) -> Vec<CodeUpdate> {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        match h.try_recv() {
            Some(Reply::Updates { updates, .. }) => return updates,
            Some(Reply::Failed(e)) => panic!("{e}"),
            Some(Reply::Ready(_)) => {}
            None => std::thread::sleep(Duration::from_millis(2)),
        }
    }
    panic!("no reply from the worker");
}

fn highlighted(line: &CodeLine) -> bool {
    line.spans.iter().any(|s| s.look != Look::default())
}

#[test]
fn the_worker_loads_grammars_and_highlights_fences() {
    let mut h = Highlights::spawn();
    h.send(
        3,
        feed(vec![delta(0, "rust", 0, &["fn main() {}", "let x = 1;"])]),
    );
    let updates = next_updates(&h);
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].lines.len(), 2);
    assert!(updates[0].lines.iter().all(highlighted));
}

#[test]
fn a_panicking_feed_returns_plain_lines_and_the_worker_carries_on() {
    let boom: Feed = Box::new(|worker, delta, cancel| {
        if delta.lang.as_deref() == Some("boom") {
            panic!("grammar data blew up");
        }
        worker.apply_with_cancel(delta, cancel)
    });
    let mut h = Highlights::start(Start::Worker(boom));
    h.send(0, feed(vec![delta(0, "boom", 0, &["a", "b"])]));
    let updates = next_updates(&h);
    assert_eq!(updates[0].lines.len(), 2);
    assert!(!updates[0].lines.iter().any(highlighted));
    h.send(0, feed(vec![delta(0, "boom", 2, &["c"])]));
    let updates = next_updates(&h);
    assert_eq!(updates[0].first, 2);
    h.send(0, feed(vec![delta(1, "rust", 0, &["fn f() {}"])]));
    let updates = next_updates(&h);
    assert!(updates[0].lines.iter().all(highlighted));
}

#[test]
fn requests_coalesce_to_one_per_fence_and_retire_drops_old_fences() {
    let mut h = Highlights::start(Start::Stalled);
    h.send(0, feed(vec![delta(0, "rust", 0, &["a"])]));
    h.send(0, feed(vec![delta(0, "rust", 1, &["b"])]));
    h.send(0, feed(vec![delta(1, "rust", 0, &["c"])]));
    assert_eq!(h.queued(), 2);
    h.send(
        0,
        CodeFeed {
            deltas: vec![],
            retire_below: 1,
            restart: false,
        },
    );
    assert_eq!(h.queued(), 1);
    h.finish(0);
    assert_eq!(h.queued(), 0);
}

#[test]
fn a_cancelled_stream_comes_back_plain() {
    let mut h = Highlights::spawn();
    h.cancel(5);
    h.send(5, feed(vec![delta(0, "rust", 0, &["fn main() {}"])]));
    let updates = next_updates(&h);
    assert!(!updates[0].lines.iter().any(highlighted));
}

#[test]
fn a_revision_inside_a_batch_replaces_the_lines_it_restyles() {
    let mut h = Highlights::spawn();
    let lines = [
        "@@",
        " ctx",
        "-alpha beta gamma",
        "+alpha beta delta",
        " end",
    ];
    h.send(0, feed(vec![delta(0, "diff", 0, &lines)]));
    let updates = next_updates(&h);
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].first, 0);
    let texts: Vec<&str> = updates[0].lines.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(texts, lines);
    let word = |line: &CodeLine, token: Token, text: &str| {
        line.spans
            .iter()
            .any(|s| s.look.bg == Some(token.into()) && s.text == text)
    };
    assert!(word(&updates[0].lines[2], Token::DiffDelWord, "gamma"));
    assert!(word(&updates[0].lines[3], Token::DiffAddWord, "delta"));
}

#[test]
fn a_restart_takes_fences_below_the_old_retire_mark_and_a_fresh_cancel_flag() {
    let mut h = Highlights::spawn();
    h.send(
        5,
        CodeFeed {
            deltas: vec![],
            retire_below: 3,
            restart: false,
        },
    );
    h.cancel(5);
    h.send(5, feed(vec![delta(0, "rust", 0, &["fn a() {}"])]));
    assert_eq!(h.queued(), 0);
    h.send(
        5,
        CodeFeed {
            deltas: vec![delta(0, "rust", 0, &["fn main() {}", "let x = 1;"])],
            retire_below: 0,
            restart: true,
        },
    );
    assert_eq!(h.queued(), 1);
    let updates = next_updates(&h);
    assert_eq!(updates[0].fence, 0);
    assert_eq!(updates[0].lines.len(), 2);
    assert!(updates[0].lines.iter().all(highlighted));
    assert!(h.streams().contains(&5));
}

fn next_reply(h: &Highlights) -> (usize, Vec<CodeUpdate>) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        match h.try_recv() {
            Some(Reply::Updates { stream, updates }) => return (stream, updates),
            Some(Reply::Failed(e)) => panic!("{e}"),
            Some(Reply::Ready(_)) => {}
            None => std::thread::sleep(Duration::from_millis(2)),
        }
    }
    panic!("no reply from the worker");
}

fn slow_feed(per_line: Duration, real: Arc<std::sync::atomic::AtomicUsize>) -> Feed {
    Box::new(move |worker, delta, cancel| {
        if !cancel.load(Ordering::SeqCst) {
            real.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(per_line);
        }
        worker.apply_with_cancel(delta, cancel)
    })
}

fn restart(deltas: Vec<FenceDelta>) -> CodeFeed {
    CodeFeed {
        deltas,
        retire_below: 0,
        restart: true,
    }
}

#[test]
fn a_later_slice_after_a_restart_is_answered_under_the_new_generation() {
    let mut h = Highlights::spawn();
    h.send(5, restart(vec![delta(0, "rust", 0, &["fn a() {}"])]));
    let (stream, first) = next_reply(&h);
    assert_eq!((stream, first[0].fence), (5, 0));
    h.send(5, feed(vec![delta(1, "rust", 0, &["fn b() {}"])]));
    let (stream, second) = next_reply(&h);
    assert_eq!((stream, second[0].fence), (5, 1));
    assert!(second[0].lines.iter().all(highlighted));
}

#[test]
fn spans_made_after_an_esc_are_marked_cancelled() {
    let mut h = Highlights::spawn();
    h.cancel(5);
    h.send(
        5,
        feed(vec![delta(0, "rust", 0, &["fn a() {}", "let x = 1;"])]),
    );
    let updates = next_updates(&h);
    assert_eq!(updates[0].cancelled, Some(0));
    h.send(6, feed(vec![delta(0, "rust", 0, &["fn a() {}"])]));
    assert_eq!(next_updates(&h)[0].cancelled, None);
}

#[test]
fn a_fence_the_governor_stopped_stays_plain_after_a_restart() {
    let real = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut h = Highlights::start(Start::Worker(slow_feed(
        Duration::from_millis(400),
        Arc::clone(&real),
    )));
    let lines = [
        "fn a() {}",
        "let x = 1;",
        "let y = 2;",
        "let z = 3;",
        "let w = 4;",
    ];
    h.send(5, feed(vec![delta(0, "rust", 0, &lines)]));
    let mut got = vec![];
    let mut given_up = false;
    while got.len() < lines.len() {
        let updates = next_updates(&h);
        for u in updates {
            given_up |= u.given_up;
            got.truncate(u.first);
            got.extend(u.lines);
        }
    }
    assert!(given_up);
    assert!(got.last().is_some_and(|l| !highlighted(l)));
    let spent = real.load(Ordering::SeqCst);
    assert!(spent < lines.len(), "{spent}");
    h.send(5, restart(vec![delta(0, "rust", 0, &lines)]));
    let again = next_updates(&h);
    assert_eq!(again[0].lines.len(), lines.len());
    assert!(again[0].lines.iter().all(|l| !highlighted(l)));
    assert_eq!(again[0].cancelled, None);
    assert!(again[0].given_up);
    assert_eq!(real.load(Ordering::SeqCst), spent);
}

#[test]
fn a_newer_stream_is_served_between_an_older_streams_fences() {
    let real = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut h = Highlights::start(Start::Worker(slow_feed(
        Duration::from_millis(60),
        Arc::clone(&real),
    )));
    let fences: Vec<FenceDelta> = (0..6)
        .map(|f| delta(f, "rust", 0, &["fn a() {}"]))
        .collect();
    h.send(1, restart(fences));
    let (stream, _) = next_reply(&h);
    assert_eq!(stream, 1);
    h.send(9, feed(vec![delta(0, "rust", 0, &["fn live() {}"])]));
    let mut older = BTreeSet::new();
    loop {
        let (stream, updates) = next_reply(&h);
        if stream == 9 {
            break;
        }
        older.extend(updates.iter().map(|u| u.fence));
    }
    assert!(older.len() < 5, "{older:?}");
    while older.len() < 5 {
        let (stream, updates) = next_reply(&h);
        assert_eq!(stream, 1);
        older.extend(updates.iter().map(|u| u.fence));
    }
}

#[test]
fn finishing_a_stream_forgets_its_generation() {
    let mut h = Highlights::start(Start::Stalled);
    h.send(5, restart(vec![delta(0, "rust", 0, &["fn a() {}"])]));
    h.send(6, restart(vec![delta(0, "rust", 0, &["fn b() {}"])]));
    assert_eq!(h.generations.get(&5), Some(&1));
    assert_eq!(h.generations.get(&6), Some(&2));
    h.finish(5);
    assert!(!h.generations.contains_key(&5));
    h.send(5, restart(vec![]));
    assert_eq!(h.generations.get(&5), Some(&3));
}

struct OpenOnDrop(Vec<Arc<AtomicBool>>);

impl Drop for OpenOnDrop {
    fn drop(&mut self) {
        for gate in &self.0 {
            gate.store(true, Ordering::SeqCst);
        }
    }
}

#[test]
fn a_reply_to_the_request_a_restart_replaced_is_dropped() {
    let open = Arc::new(AtomicBool::new(false));
    let gate = Arc::clone(&open);
    let gated: Feed = Box::new(move |worker, delta, cancel| {
        while !gate.load(Ordering::SeqCst) && !cancel.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
        worker.apply_with_cancel(delta, cancel)
    });
    let mut h = Highlights::start(Start::Worker(gated));
    let _open = OpenOnDrop(vec![Arc::clone(&open)]);
    h.send(5, feed(vec![delta(0, "rust", 0, &["fn old() {}"])]));
    let deadline = Instant::now() + Duration::from_secs(20);
    while h.queued() > 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    h.send(5, restart(vec![delta(1, "rust", 0, &["fn new() {}"])]));
    open.store(true, Ordering::SeqCst);
    let (stream, updates) = next_reply(&h);
    assert_eq!(stream, 5);
    assert_eq!(updates.iter().map(|u| u.fence).collect::<Vec<_>>(), vec![1]);
}

#[test]
fn lines_the_governor_stopped_are_not_marked_cancelled_after_an_esc() {
    let real = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut h = Highlights::start(Start::Worker(slow_feed(
        Duration::from_millis(400),
        Arc::clone(&real),
    )));
    let lines = ["fn a() {}", "let x = 1;", "let y = 2;", "let z = 3;"];
    h.send(5, feed(vec![delta(0, "rust", 0, &lines)]));
    let mut got = 0;
    while got < lines.len() {
        for u in next_updates(&h) {
            got = u.first + u.lines.len();
            assert!(u.cancelled.is_none());
        }
    }
    h.cancel(5);
    h.send(5, feed(vec![delta(0, "rust", 4, &["let w = 4;"])]));
    let after = next_updates(&h);
    assert_eq!(after[0].cancelled, None);
    assert!(after[0].given_up);
}

#[test]
fn a_put_back_request_merges_with_deltas_queued_since() {
    let first = Arc::new(AtomicBool::new(false));
    let second = Arc::new(AtomicBool::new(false));
    let (g0, g5) = (Arc::clone(&first), Arc::clone(&second));
    let gated: Feed = Box::new(move |worker, delta, cancel| {
        let gate = match delta.fence {
            0 => Some(&g0),
            5 => Some(&g5),
            _ => None,
        };
        while gate.is_some_and(|g| !g.load(Ordering::SeqCst)) && !cancel.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
        worker.apply_with_cancel(delta, cancel)
    });
    let mut h = Highlights::start(Start::Worker(gated));
    let _open = OpenOnDrop(vec![Arc::clone(&first), Arc::clone(&second)]);
    h.send(
        1,
        feed(vec![
            delta(0, "rust", 0, &["fn a() {}"]),
            delta(3, "rust", 0, &["fn b() {"]),
        ]),
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    while h.queued() > 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    h.send(9, feed(vec![delta(5, "rust", 0, &["fn live() {}"])]));
    h.send(1, feed(vec![delta(3, "rust", 1, &["    let x = 1;"])]));
    first.store(true, Ordering::SeqCst);
    let queued_for = |h: &Highlights, id: usize| -> Option<Vec<FenceDelta>> {
        let (m, _) = &*h.shared;
        lock(m).streams.get(&id).map(|r| r.deltas.clone())
    };
    while queued_for(&h, 9).is_some_and(|d| !d.is_empty()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    let back = queued_for(&h, 1).unwrap();
    assert_eq!(back.len(), 1, "{back:?}");
    assert_eq!(
        (back[0].fence, back[0].from, back[0].lines.len()),
        (3, 0, 2)
    );
    h.send(1, feed(vec![delta(3, "rust", 2, &["    let y = 2;"])]));
    second.store(true, Ordering::SeqCst);
    let mut fence3: Vec<CodeLine> = vec![];
    while fence3.len() < 3 && Instant::now() < deadline {
        let (stream, updates) = next_reply(&h);
        for u in updates.into_iter().filter(|u| stream == 1 && u.fence == 3) {
            fence3.truncate(u.first);
            fence3.extend(u.lines);
        }
    }
    assert_eq!(fence3.len(), 3);
    assert!(fence3.iter().all(highlighted), "{fence3:?}");
}
