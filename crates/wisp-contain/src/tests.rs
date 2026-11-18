use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll, Waker};

use super::*;

fn poll_once<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
    let mut cx = Context::from_waker(Waker::noop());
    future.poll(&mut cx)
}

#[test]
fn contain_sets_the_flag_only_while_the_work_runs() {
    assert!(is_contained());
    let inside = contain(is_contained).unwrap();
    assert!(inside);
    assert!(!is_contained());
}

#[test]
fn a_panic_is_caught_and_the_flag_is_cleared() {
    let caught = contain(|| -> u8 { panic!("plugin blew up") });
    let payload = caught.unwrap_err();
    assert_eq!(message(payload.as_ref()), Some("plugin blew up"));
    assert!(!is_contained());
}

#[test]
fn nesting_restores_the_outer_value() {
    let seen = contain(|| {
        let inner = contain(is_contained).unwrap();
        (inner, is_contained())
    })
    .unwrap();
    assert_eq!(seen, (true, true));
    assert!(!is_contained());
    let after_inner_panic = contain(|| {
        let _ = contain(|| -> u8 { panic!("inner") });
        is_contained()
    })
    .unwrap();
    assert!(after_inner_panic);
    assert!(!is_contained());
}

struct Twice {
    first: bool,
    seen: Arc<AtomicBool>,
    panics: bool,
}

impl Future for Twice {
    type Output = u8;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<u8> {
        self.seen.store(is_contained(), Ordering::SeqCst);
        if self.first {
            self.first = false;
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        assert!(!self.panics, "second poll blew up");
        Poll::Ready(7)
    }
}

#[test]
fn every_poll_is_contained_and_the_flag_is_cleared_between_polls() {
    let seen = Arc::new(AtomicBool::new(false));
    let mut future = Contained::new(Twice {
        first: true,
        seen: seen.clone(),
        panics: false,
    });
    assert!(poll_once(Pin::new(&mut future)).is_pending());
    assert!(seen.load(Ordering::SeqCst));
    assert!(!is_contained());
    seen.store(false, Ordering::SeqCst);
    let Poll::Ready(Ok(out)) = poll_once(Pin::new(&mut future)) else {
        panic!("the second poll finishes");
    };
    assert_eq!(out, 7);
    assert!(seen.load(Ordering::SeqCst));
    assert!(!is_contained());
}

#[test]
fn a_panic_in_a_later_poll_is_caught() {
    let mut future = Contained::new(Twice {
        first: true,
        seen: Arc::default(),
        panics: true,
    });
    assert!(poll_once(Pin::new(&mut future)).is_pending());
    let Poll::Ready(Err(payload)) = poll_once(Pin::new(&mut future)) else {
        panic!("the second poll panics");
    };
    assert_eq!(message(payload.as_ref()), Some("second poll blew up"));
    assert!(!is_contained());
    assert!(poll_once(Pin::new(&mut future)).is_pending());
}

#[test]
fn wisp_contain_has_no_dependencies() {
    let manifest = include_str!("../Cargo.toml");
    let deps = manifest
        .split("[dependencies]")
        .nth(1)
        .and_then(|rest| rest.split("\n[").next())
        .unwrap_or_default();
    assert!(deps.trim().is_empty(), "{deps}");
}
