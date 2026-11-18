use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use wisp_protocol::{SignalKind, StoppedBy, TermSize, Typed};

use super::Handle;
use super::keys::{self, Key, KeyBytes};
use super::screen::{Terminal, WriteEnd, Written};
use super::state::{Control, JobState, ModelChange};
use super::wait::{WaitEnd, wait_for_end};

#[derive(Debug)]
pub enum PipeWrite {
    Closed,
    Cancelled { left: usize },
    Failed { error: std::io::Error, left: usize },
    Done { left: usize },
}

pub async fn to_pipe(
    state: &JobState,
    bytes: &[u8],
    eof: bool,
    within: Duration,
    cancel: &CancellationToken,
) -> PipeWrite {
    let mut stdin = state.stdin().lock().await;
    let Some(pipe) = stdin.as_mut() else {
        return PipeWrite::Closed;
    };
    let limit = Instant::now() + within;
    let mut done = 0;
    let mut failed = None;
    while done < bytes.len() {
        let wrote = tokio::select! {
            biased;
            () = cancel.cancelled() => {
                drop(stdin);
                return PipeWrite::Cancelled {
                    left: bytes.len() - done,
                };
            }
            () = tokio::time::sleep_until(limit) => break,
            wrote = pipe.write(&bytes[done..]) => wrote,
        };
        match wrote {
            Ok(0) => break,
            Ok(n) => done += n,
            Err(e) => {
                failed = Some(e);
                break;
            }
        }
    }
    if let Some(error) = failed {
        *stdin = None;
        state.live().stdin_open = false;
        return PipeWrite::Failed {
            error,
            left: bytes.len() - done,
        };
    }
    let _ = pipe.flush().await;
    if done < bytes.len() {
        return PipeWrite::Done {
            left: bytes.len() - done,
        };
    }
    if eof {
        *stdin = None;
        state.live().stdin_open = false;
    }
    PipeWrite::Done { left: 0 }
}

pub const LINE_LIMIT: usize = 4095;
pub const INTERRUPT: u8 = 0x03;

pub fn longest_line(input: &str) -> usize {
    input.split(['\n', '\r']).map(str::len).max().unwrap_or(0)
}

pub fn terminal_bytes(terminal: &Terminal, input: &str, keys: &[Key]) -> Result<Vec<u8>, usize> {
    if terminal.canonical() == Some(true) {
        let longest = longest_line(input);
        if longest > LINE_LIMIT {
            return Err(longest);
        }
    }
    let modes = terminal.modes();
    let mut bytes = keys::paste(input, modes);
    for key in keys {
        bytes.extend(key.terminal_bytes(modes));
    }
    Ok(bytes)
}

pub fn pipe_bytes(input: &str, keys: &[Key]) -> Result<(Vec<u8>, bool), Key> {
    let mut bytes = input.as_bytes().to_vec();
    let mut eof = false;
    for key in keys {
        if *key == Key::Eof {
            eof = true;
            continue;
        }
        match key.pipe_bytes() {
            Some(more) => bytes.extend(more),
            None => return Err(*key),
        }
    }
    Ok((bytes, eof))
}

pub struct Typing<'a> {
    pub size: Option<TermSize>,
    pub input: Option<&'a str>,
    pub keys: &'a [Key],
    pub signal: Option<SignalKind>,
    pub hidden: bool,
}

impl Typing<'_> {
    pub fn writes(&self) -> bool {
        self.input.is_some() || !self.keys.is_empty()
    }
}

#[derive(Debug)]
pub enum TermRefusal {
    LineTooLong(usize),
    Driven,
    Resize(std::io::Error),
}

#[derive(Debug)]
pub enum SentEnd {
    Written,
    Partly,
    Stopped,
    Failed(std::io::Error),
}

#[derive(Debug)]
pub struct Sent {
    pub total: usize,
    pub left: usize,
    pub end: SentEnd,
}

#[derive(Debug, Default)]
pub struct TermTyped {
    pub signature: Option<String>,
    pub sent: Option<Sent>,
    pub typed: Option<Typed>,
    pub end: Option<WaitEnd>,
}

pub async fn to_terminal(
    handle: &Handle,
    terminal: &Terminal,
    typing: &Typing<'_>,
    within: Duration,
    cancel: &CancellationToken,
) -> Result<TermTyped, TermRefusal> {
    let mut bytes = if typing.writes() {
        Some(
            terminal_bytes(terminal, typing.input.unwrap_or_default(), typing.keys)
                .map_err(TermRefusal::LineTooLong)?,
        )
    } else {
        None
    };
    if typing.signal == Some(SignalKind::Interrupt) {
        bytes.get_or_insert_with(Vec::new).push(INTERRUPT);
    }
    let change = ModelChange {
        size: typing.size,
        bytes,
        hidden: typing.hidden,
        stop: (typing.signal == Some(SignalKind::Kill)).then_some(StoppedBy::Model),
    };
    let mut done = TermTyped::default();
    if change.size.is_none() && change.bytes.is_none() && change.stop.is_none() {
        return Ok(done);
    }
    let stops = change.stop.is_some();
    let Some(applied) = handle.state.model_apply(terminal, change) else {
        return Err(TermRefusal::Driven);
    };
    applied.resized.map_err(TermRefusal::Resize)?;
    done.signature = applied
        .signature
        .filter(|_| typing.writes() || typing.signal.is_some());
    if let Some(written) = applied.written {
        let (sent, typed) = await_written(written, typing, within, cancel).await;
        let early = matches!(sent.end, SentEnd::Stopped | SentEnd::Failed(_));
        done.sent = Some(sent);
        done.typed = Some(typed);
        if early {
            return Ok(done);
        }
    }
    if stops {
        let typed = done.typed.get_or_insert(Typed {
            echoed: false,
            unsent: false,
            signalled: false,
        });
        typed.signalled = true;
        done.end = Some(wait_for_end(handle, cancel).await);
    }
    Ok(done)
}

async fn await_written(
    written: Written,
    typing: &Typing<'_>,
    within: Duration,
    cancel: &CancellationToken,
) -> (Sent, Typed) {
    let total = written.total();
    let limit = Instant::now() + within;
    let stop = written.stop_handle();
    let progress = written.progress();
    let echoed = || typing.writes() && progress.echoed();
    let interrupts = typing.signal == Some(SignalKind::Interrupt);
    let kills = typing.signal == Some(SignalKind::Kill);
    let end = tokio::select! {
        biased;
        () = cancel.cancelled() => {
            stop.stop();
            let left = total.saturating_sub(progress.done());
            let typed = Typed {
                echoed: echoed(),
                unsent: if interrupts { left > 1 } else { left > 0 },
                signalled: kills || (interrupts && left == 0),
            };
            return (Sent { total, left, end: SentEnd::Stopped }, typed);
        }
        end = written.until(limit) => end,
    };
    let typed = |unsent: bool, signalled: bool| Typed {
        echoed: echoed(),
        unsent,
        signalled,
    };
    match end {
        WriteEnd::Written => (
            Sent {
                total,
                left: 0,
                end: SentEnd::Written,
            },
            typed(false, interrupts),
        ),
        WriteEnd::Partly { left } => (
            Sent {
                total,
                left,
                end: SentEnd::Partly,
            },
            if interrupts {
                typed(left > 1, false)
            } else {
                typed(true, false)
            },
        ),
        WriteEnd::Failed(e) => (
            Sent {
                total,
                left: total.saturating_sub(progress.done()),
                end: SentEnd::Failed(e),
            },
            Typed {
                echoed: echoed(),
                unsent: true,
                signalled: kills,
            },
        ),
    }
}

#[derive(Debug)]
pub enum PipeRefusal {
    NoInput,
    TerminalKey(Key),
}

#[derive(Debug)]
pub struct PipeTyped {
    pub sent: Option<(usize, PipeWrite)>,
    pub typed: Option<Typed>,
    pub end: Option<WaitEnd>,
}

pub async fn to_pipe_job(
    handle: &Handle,
    typing: &Typing<'_>,
    within: Duration,
    cancel: &CancellationToken,
    kill: impl FnOnce() -> bool,
) -> Result<PipeTyped, PipeRefusal> {
    let mut done = PipeTyped {
        sent: None,
        typed: None,
        end: None,
    };
    let mut unsent = false;
    if typing.writes() {
        if !handle.state.piped() {
            return Err(PipeRefusal::NoInput);
        }
        let (bytes, eof) = pipe_bytes(typing.input.unwrap_or_default(), typing.keys)
            .map_err(PipeRefusal::TerminalKey)?;
        let wrote = to_pipe(&handle.state, &bytes, eof, within, cancel).await;
        let early = !matches!(wrote, PipeWrite::Done { .. });
        unsent = !matches!(wrote, PipeWrite::Done { left: 0 });
        if matches!(
            wrote,
            PipeWrite::Cancelled { .. } | PipeWrite::Failed { .. }
        ) {
            done.typed = Some(Typed {
                echoed: false,
                unsent: true,
                signalled: false,
            });
        }
        done.sent = Some((bytes.len(), wrote));
        if early {
            return Ok(done);
        }
    }
    let signalled = match typing.signal {
        Some(SignalKind::Interrupt) => handle.state.deliver(Control::Interrupt),
        Some(SignalKind::Kill) => {
            let sent = kill();
            done.end = Some(wait_for_end(handle, cancel).await);
            sent
        }
        None => false,
    };
    if typing.writes() || typing.signal.is_some() {
        done.typed = Some(Typed {
            echoed: false,
            unsent,
            signalled,
        });
    }
    Ok(done)
}
