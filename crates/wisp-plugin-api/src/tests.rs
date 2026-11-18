use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll, Wake, Waker};

use super::yield_now;
use crate::types::{Completed, Failure, Key, Outcome, ToolRun};

struct Counter(AtomicUsize);

impl Wake for Counter {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn yield_now_wakes_itself_once_and_then_finishes() {
    let counter = Arc::new(Counter(AtomicUsize::new(0)));
    let waker = Waker::from(counter.clone());
    let mut cx = Context::from_waker(&waker);
    let mut future = pin!(yield_now());
    assert_eq!(future.as_mut().poll(&mut cx), Poll::Pending);
    assert_eq!(counter.0.load(Ordering::SeqCst), 1);
    assert_eq!(future.as_mut().poll(&mut cx), Poll::Ready(()));
    assert_eq!(counter.0.load(Ordering::SeqCst), 1);
}

fn empty_run(outcome: Outcome) -> ToolRun {
    ToolRun {
        outcome,
        facts: Vec::new(),
        cwd: None,
        trailer: None,
        result: None,
        attachments: Vec::new(),
        typed: None,
    }
}

#[test]
fn the_tool_run_builders_fill_only_what_they_name() {
    assert_eq!(
        ToolRun::new(Outcome::Cancelled),
        empty_run(Outcome::Cancelled)
    );
    assert_eq!(
        ToolRun::failed_to_start("broke"),
        empty_run(Outcome::Failed(Failure {
            message: "broke".to_owned(),
            before_start: true,
        }))
    );
    assert_eq!(
        ToolRun::completed(),
        empty_run(Outcome::Completed(Completed {
            is_error: false,
            exit: None,
        }))
    );
    assert_eq!(
        ToolRun::new(Outcome::Cancelled).with_result("stopped"),
        ToolRun {
            result: Some("stopped".to_owned()),
            ..empty_run(Outcome::Cancelled)
        }
    );
}

#[test]
fn key_names_parse_back_to_themselves() {
    for name in ["Enter", "PageDown", "F1", "F12", "C-a", "C-z", "EOF"] {
        assert_eq!(Key::parse(name).map(|k| k.name()).as_deref(), Some(name));
    }
    assert_eq!(Key::parse("C-C"), Some(Key::Ctrl('c')));
    for wrong in ["F0", "F13", "F01", "C-1", "C-ab", "enter", ""] {
        assert_eq!(Key::parse(wrong), None, "{wrong}");
    }
}
