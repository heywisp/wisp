use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

fn ticking(ticks: Arc<AtomicU64>, stop_at: Option<(u64, Flag)>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let n = ticks.fetch_add(1, Ordering::SeqCst) + 1;
            if let Some((at, flag)) = &stop_at
                && n == *at
            {
                flag.set();
            }
            tokio::task::yield_now().await;
        }
    })
}

fn large(rig: &Rig) {
    rig.sized("large.txt", "line\n".repeat(2_000).as_bytes(), 60_000_000);
}

#[tokio::test(flavor = "current_thread")]
async fn a_task_beside_a_large_count_keeps_ticking() {
    let rig = Rig::new();
    large(&rig);
    let ticks = Arc::new(AtomicU64::new(0));
    let ticker = ticking(ticks.clone(), None);
    let viewed = rig.view("large.txt").await;
    let seen = ticks.load(Ordering::SeqCst);
    ticker.abort();
    assert!(viewed.completed().contains("lines 1\u{2013}2,000 below"));
    assert!(seen > 100, "the ticking task ran {seen} times");
}

#[tokio::test(flavor = "current_thread")]
async fn esc_stops_the_count_at_the_next_chunk() {
    let rig = Rig::new();
    large(&rig);
    let ticks = Arc::new(AtomicU64::new(0));
    let ticker = ticking(ticks.clone(), Some((50, rig.cancel.clone())));
    let viewed = rig.view("large.txt").await;
    let seen = ticks.load(Ordering::SeqCst);
    ticker.abort();
    assert_eq!(viewed.run.outcome, Outcome::Cancelled);
    assert_eq!(viewed.result(), STOPPED);
    assert!((50..=52).contains(&seen), "stopped after {seen} ticks");
}

#[tokio::test(flavor = "current_thread")]
async fn a_large_attachment_is_read_in_chunks() {
    let rig = Rig::new();
    let start = png(4, 4);
    rig.sized("big.png", &start, 4_000_000);
    let ticks = Arc::new(AtomicU64::new(0));
    let ticker = ticking(ticks.clone(), None);
    let viewed = rig.view("big.png").await;
    let seen = ticks.load(Ordering::SeqCst);
    ticker.abort();
    assert_eq!(viewed.run.attachments.len(), 1);
    assert!(seen > 30, "the ticking task ran {seen} times");
}
