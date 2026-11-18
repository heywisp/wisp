use std::os::fd::AsFd;
use std::time::Duration;

use rustix::termios::{OptionalActions, tcsetattr};
use tokio::time::Instant;

use super::*;
use crate::jobs::screen::{Terminal, WriteEnd, Written};
use crate::test_guard::until;

const TYPED: usize = 40_960;
const PATIENCE: Duration = Duration::from_secs(5);

fn raw_pair() -> Pair {
    let pair = open(TermSize { cols: 80, rows: 24 }).unwrap();
    let mut termios = tcgetattr(&pair.subordinate).unwrap();
    termios.make_raw();
    tcsetattr(&pair.subordinate, OptionalActions::Now, &termios).unwrap();
    pair
}

fn typed(port: &PtyPort) -> Written {
    let (job, written) = WriteJob::new(vec![b'a'; TYPED], false);
    port.write(job);
    written
}

async fn gone(alive: &std::sync::Weak<()>) -> bool {
    until(|| alive.upgrade().is_none()).await
}

fn drain(fd: impl AsFd) -> usize {
    let flags = fcntl_getfl(&fd).unwrap();
    fcntl_setfl(&fd, flags | OFlags::NONBLOCK).unwrap();
    let mut buf = vec![0u8; 8192];
    let mut total = 0;
    loop {
        match rustix::io::read(&fd, &mut buf) {
            Ok(0) => return total,
            Ok(n) => total += n,
            Err(Errno::INTR) => {}
            Err(_) => return total,
        }
    }
}

#[tokio::test]
async fn a_stopped_write_counts_exactly_the_bytes_the_terminal_took() {
    let _serial = TERMINAL_TESTS.lock().await;
    let pair = raw_pair();
    let port = PtyPort::new(pair.master).unwrap();
    let end = typed(&port)
        .until(Instant::now() + Duration::from_millis(300))
        .await;
    let WriteEnd::Partly { left } = end else {
        panic!("{end:?}");
    };
    assert!(left > 0 && left < TYPED, "{left}");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut got = drain(&pair.subordinate);
    tokio::time::sleep(Duration::from_millis(200)).await;
    got += drain(&pair.subordinate);
    assert_eq!(got, TYPED - left);
}

#[tokio::test]
async fn blocked_input_fails_and_its_thread_ends_when_the_program_side_closes() {
    let _serial = TERMINAL_TESTS.lock().await;
    let pair = raw_pair();
    let port = PtyPort::new(pair.master).unwrap();
    let alive = port.alive.clone();
    let written = typed(&port);
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(pair.subordinate);
    let end = written.until(Instant::now() + PATIENCE).await;
    assert!(matches!(end, WriteEnd::Failed(_)), "{end:?}");
    assert!(gone(&alive).await);
    drop(port);
}

#[tokio::test]
async fn closing_the_terminal_ends_blocked_input_and_frees_the_pty() {
    let _serial = TERMINAL_TESTS.lock().await;
    let pair = raw_pair();
    let port = PtyPort::new(pair.master).unwrap();
    let alive = port.alive.clone();
    let written = typed(&port);
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(port);
    let end = written.until(Instant::now() + PATIENCE).await;
    assert!(matches!(end, WriteEnd::Failed(_)), "{end:?}");
    assert!(gone(&alive).await);
    drain(&pair.subordinate);
    let mut fds = [PollFd::new(&pair.subordinate, PollFlags::IN)];
    poll(
        &mut fds,
        Some(&Timespec {
            tv_sec: 2,
            tv_nsec: 0,
        }),
    )
    .unwrap();
    assert!(
        fds[0].revents().contains(PollFlags::HUP),
        "{:?}",
        fds[0].revents()
    );
}

fn read_until(fd: impl AsFd, got: &mut Vec<u8>, done: impl Fn(&[u8]) -> bool) {
    let start = std::time::Instant::now();
    let mut buf = vec![0u8; 8192];
    while !done(got) {
        assert!(
            start.elapsed() < PATIENCE,
            "only {} bytes arrived",
            got.len()
        );
        let mut fds = [PollFd::new(&fd, PollFlags::IN)];
        poll(&mut fds, Some(&WRITE_POLL)).unwrap();
        if fds[0].revents().contains(PollFlags::IN) {
            let n = rustix::io::read(&fd, &mut buf).unwrap();
            got.extend_from_slice(&buf[..n]);
        }
    }
}

#[tokio::test]
async fn the_cap_on_held_replies_counts_answers_not_chunks() {
    let _serial = TERMINAL_TESTS.lock().await;
    let pair = raw_pair();
    let port = PtyPort::new(pair.master).unwrap();
    let _written = typed(&port);
    let terminal = Terminal::new(TermSize { cols: 80, rows: 24 }, Box::new(port));
    terminal.feed(&b"\x1b[5n".repeat(2 * QUEUED_REPLIES));
    let held = TYPED + QUEUED_REPLIES * b"\x1b[0n".len();
    let mut got = Vec::new();
    read_until(&pair.subordinate, &mut got, |got| got.len() >= held);
    terminal.feed(b"\x1b[6n");
    read_until(&pair.subordinate, &mut got, |got| got.ends_with(b"R"));
    assert_eq!(got.iter().filter(|b| **b == b'a').count(), TYPED);
    let answers = got.windows(4).filter(|w| *w == b"\x1b[0n").count();
    assert_eq!(answers, QUEUED_REPLIES);
    assert!(got.ends_with(b"\x1b[0n\x1b[1;1R"), "{:?}", &got[held - 8..]);
    assert_eq!(got.len(), held + b"\x1b[1;1R".len());
}
