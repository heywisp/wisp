use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tokio::sync::Notify;
use wisp_render::highlight::{
    CodeCursor, CodeLine, CodeUpdate, CodeWorker, FenceDelta, Grammars, Highlighter, Limits,
};
use wisp_render::stream::CodeFeed;

use crate::term::contain;

pub const GOVERNOR: Duration = Duration::from_secs(1);
pub const PARTIAL_EVERY: Duration = Duration::from_millis(32);

static STOP: AtomicBool = AtomicBool::new(true);

pub type Feed =
    Box<dyn FnMut(&mut CodeWorker, &FenceDelta, &AtomicBool) -> Option<CodeUpdate> + Send>;

pub fn default_feed() -> Feed {
    Box::new(|worker, delta, cancel| worker.apply_with_cancel(delta, cancel))
}

#[derive(Debug)]
pub enum Reply {
    Ready(Arc<Grammars>),
    Failed(String),
    Updates {
        stream: usize,
        updates: Vec<CodeUpdate>,
    },
}

#[derive(Default)]
struct Request {
    deltas: Vec<FenceDelta>,
    retire_below: usize,
    drop: bool,
    restart: bool,
    generation: u64,
}

#[derive(Default)]
struct Queue {
    streams: BTreeMap<usize, Request>,
    stop: bool,
}

type Shared = Arc<(Mutex<Queue>, Condvar)>;

fn lock(m: &Mutex<Queue>) -> MutexGuard<'_, Queue> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

pub struct Highlights {
    shared: Shared,
    replies: Receiver<(u64, Reply)>,
    wake: Arc<Notify>,
    cancels: BTreeMap<usize, Arc<AtomicBool>>,
    cancel_list: Arc<Mutex<BTreeMap<usize, Arc<AtomicBool>>>>,
    retired: BTreeMap<usize, usize>,
    generations: BTreeMap<usize, u64>,
    last_generation: u64,
    thread: Option<JoinHandle<()>>,
    _keep: Option<Sender<(u64, Reply)>>,
}

pub enum Start {
    Worker(Feed),
    Loaded(Arc<Grammars>, Feed),
    FailedButServing(Arc<Grammars>, Feed),
    Later(Receiver<Arc<Grammars>>, Feed),
    Stalled,
}

impl Highlights {
    pub fn spawn() -> Highlights {
        Highlights::start(Start::Worker(default_feed()))
    }

    pub fn start(start: Start) -> Highlights {
        let shared: Shared = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
        let (send, replies) = channel();
        let wake = Arc::new(Notify::new());
        let cancel_list = Arc::new(Mutex::new(BTreeMap::new()));
        let (preloaded, failed, later, start) = match start {
            Start::Loaded(grammars, feed) => (Some(grammars), false, None, Start::Worker(feed)),
            Start::FailedButServing(grammars, feed) => {
                (Some(grammars), true, None, Start::Worker(feed))
            }
            Start::Later(later, feed) => (None, false, Some(later), Start::Worker(feed)),
            other => (None, false, None, other),
        };
        if let Some(grammars) = &preloaded {
            let reply = match failed {
                true => Reply::Failed("the grammars failed to load".to_owned()),
                false => Reply::Ready(Arc::clone(grammars)),
            };
            drop(send.send((0, reply)));
        }
        let (thread, keep) = match start {
            Start::Stalled | Start::Loaded(..) | Start::FailedButServing(..) | Start::Later(..) => {
                (None, Some(send))
            }
            Start::Worker(feed) => {
                let worker = Worker {
                    grammars: preloaded,
                    later,
                    shared: Arc::clone(&shared),
                    send,
                    wake: Arc::clone(&wake),
                    cancels: Arc::clone(&cancel_list),
                    feed,
                };
                let thread = std::thread::Builder::new()
                    .name("wisp-tui-highlight".to_owned())
                    .spawn(move || worker.run())
                    .ok();
                (thread, None)
            }
        };
        Highlights {
            shared,
            replies,
            wake,
            cancels: BTreeMap::new(),
            cancel_list,
            retired: BTreeMap::new(),
            generations: BTreeMap::new(),
            last_generation: 0,
            thread,
            _keep: keep,
        }
    }

    pub fn wake(&self) -> Arc<Notify> {
        Arc::clone(&self.wake)
    }

    fn cancel_flag(&mut self, stream: usize) -> Arc<AtomicBool> {
        let flag = self
            .cancels
            .entry(stream)
            .or_insert_with(|| Arc::new(AtomicBool::new(false)));
        let flag = Arc::clone(flag);
        let mut list = self
            .cancel_list
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        list.entry(stream).or_insert_with(|| Arc::clone(&flag));
        flag
    }

    pub fn send(&mut self, stream: usize, feed: CodeFeed) {
        if feed.restart {
            self.restart(stream);
        }
        let retired = self.retired.get(&stream).copied().unwrap_or(0);
        if feed.deltas.is_empty() && feed.retire_below <= retired && !feed.restart {
            return;
        }
        self.retired.insert(stream, feed.retire_below.max(retired));
        drop(self.cancel_flag(stream));
        let (m, cv) = &*self.shared;
        let mut q = lock(m);
        let request = q.streams.entry(stream).or_default();
        request.generation = self.generations.get(&stream).copied().unwrap_or(0);
        request.retire_below = request.retire_below.max(feed.retire_below);
        let below = request.retire_below;
        request.deltas.retain(|d| d.fence >= below);
        for delta in feed.deltas {
            if delta.fence < below {
                continue;
            }
            queue_delta(&mut request.deltas, delta);
        }
        request.deltas.sort_by_key(|d| d.fence);
        cv.notify_all();
    }

    fn restart(&mut self, stream: usize) {
        if let Some(flag) = self.cancels.remove(&stream) {
            flag.store(true, Ordering::SeqCst);
        }
        self.retired.remove(&stream);
        self.cancel_list
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&stream);
        self.last_generation = self.last_generation.saturating_add(1);
        let generation = self.last_generation;
        self.generations.insert(stream, generation);
        let (m, _) = &*self.shared;
        lock(m).streams.insert(
            stream,
            Request {
                restart: true,
                generation,
                ..Request::default()
            },
        );
    }

    pub fn cancel(&mut self, stream: usize) {
        self.cancel_flag(stream).store(true, Ordering::SeqCst);
    }

    pub fn finish(&mut self, stream: usize) {
        if let Some(flag) = self.cancels.remove(&stream) {
            flag.store(true, Ordering::SeqCst);
        }
        self.retired.remove(&stream);
        self.generations.remove(&stream);
        self.cancel_list
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&stream);
        let (m, cv) = &*self.shared;
        let mut q = lock(m);
        let request = q.streams.entry(stream).or_default();
        request.deltas.clear();
        request.drop = true;
        cv.notify_all();
    }

    pub fn streams(&self) -> BTreeSet<usize> {
        self.retired.keys().copied().collect()
    }

    pub fn try_recv(&self) -> Option<Reply> {
        while let Ok((generation, reply)) = self.replies.try_recv() {
            if let Reply::Updates { stream, .. } = &reply
                && self.generations.get(stream).copied().unwrap_or(0) != generation
            {
                continue;
            }
            return Some(reply);
        }
        None
    }

    pub fn queued(&self) -> usize {
        let (m, _) = &*self.shared;
        lock(m).streams.values().map(|r| r.deltas.len()).sum()
    }
}

impl Drop for Highlights {
    fn drop(&mut self) {
        {
            let (m, cv) = &*self.shared;
            lock(m).stop = true;
            cv.notify_all();
        }
        for flag in self.cancels.values() {
            flag.store(true, Ordering::SeqCst);
        }
        if let Some(thread) = self.thread.take() {
            drop(thread.join());
        }
    }
}

struct StreamWork {
    worker: CodeWorker,
    spent: BTreeMap<usize, Duration>,
    broken: BTreeSet<usize>,
    gave_up: BTreeSet<usize>,
}

struct Worker {
    grammars: Option<Arc<Grammars>>,
    later: Option<Receiver<Arc<Grammars>>>,
    shared: Shared,
    send: Sender<(u64, Reply)>,
    wake: Arc<Notify>,
    cancels: Arc<Mutex<BTreeMap<usize, Arc<AtomicBool>>>>,
    feed: Feed,
}

fn queue_delta(deltas: &mut Vec<FenceDelta>, delta: FenceDelta) {
    match deltas.iter_mut().find(|d| d.fence == delta.fence) {
        Some(queued) => {
            if let Err(newer) = queued.merge(delta) {
                *queued = newer;
            }
        }
        None => deltas.push(delta),
    }
}

fn plain_lines(lang: Option<&str>, lines: &[String]) -> Vec<CodeLine> {
    let mut cursor = CodeCursor::new(None, lang, Limits::default());
    lines
        .iter()
        .map(|line| CodeLine {
            text: line.clone(),
            spans: cursor.line(line),
        })
        .collect()
}

impl Worker {
    fn reply(&self, reply: Reply) -> bool {
        self.reply_at(0, reply)
    }

    fn reply_at(&self, generation: u64, reply: Reply) -> bool {
        let sent = self.send.send((generation, reply)).is_ok();
        self.wake.notify_one();
        sent
    }

    fn waiting_above(&self, id: usize) -> bool {
        let (m, _) = &*self.shared;
        lock(m)
            .streams
            .range(id.saturating_add(1)..)
            .any(|(_, r)| r.drop || !r.deltas.is_empty())
    }

    fn requeue(&self, id: usize, request: &Request, from: usize) {
        let mut rest = request.deltas.get(from..).unwrap_or_default().to_vec();
        let (m, _) = &*self.shared;
        let mut q = lock(m);
        match q.streams.get_mut(&id) {
            Some(newer)
                if newer.drop || newer.restart || newer.generation != request.generation => {}
            Some(newer) => {
                for delta in std::mem::take(&mut newer.deltas) {
                    queue_delta(&mut rest, delta);
                }
                let below = newer.retire_below;
                rest.retain(|d| d.fence >= below);
                newer.deltas = rest;
            }
            None => {
                q.streams.insert(
                    id,
                    Request {
                        deltas: rest,
                        retire_below: request.retire_below,
                        generation: request.generation,
                        ..Request::default()
                    },
                );
            }
        }
    }

    fn take(&self) -> Option<(usize, Request)> {
        let (m, cv) = &*self.shared;
        let mut q = lock(m);
        loop {
            if q.stop {
                return None;
            }
            let next = q
                .streams
                .iter()
                .rev()
                .find(|(_, r)| r.drop || !r.deltas.is_empty())
                .map(|(id, _)| *id);
            if let Some(id) = next
                && let Some(request) = q.streams.remove(&id)
            {
                return Some((id, request));
            }
            q = cv.wait(q).unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn run(mut self) {
        if let Some(grammars) = self.grammars.take() {
            return self.serve(&grammars);
        }
        let grammars = match self.later.take() {
            Some(later) => loop {
                match later.recv_timeout(PARTIAL_EVERY) {
                    Ok(grammars) => break grammars,
                    Err(RecvTimeoutError::Timeout) if !lock(&self.shared.0).stop => {}
                    Err(_) => return,
                }
            },
            None => match contain(Grammars::load) {
                Ok(Ok(g)) => Arc::new(g),
                Ok(Err(e)) => {
                    self.reply(Reply::Failed(e.to_string()));
                    return;
                }
                Err(_) => {
                    self.reply(Reply::Failed("the grammar loader panicked".to_owned()));
                    return;
                }
            },
        };
        if !self.reply(Reply::Ready(Arc::clone(&grammars))) {
            return;
        }
        self.serve(&grammars);
    }

    fn serve(&mut self, grammars: &Arc<Grammars>) {
        let mut streams: BTreeMap<usize, StreamWork> = BTreeMap::new();
        while let Some((id, request)) = self.take() {
            if request.drop {
                streams.remove(&id);
                continue;
            }
            let kept = match request.restart {
                true => streams
                    .remove(&id)
                    .map(|old| (old.broken, old.gave_up))
                    .unwrap_or_default(),
                false => Default::default(),
            };
            let cancel = self
                .cancels
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&id)
                .cloned()
                .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
            let work = streams.entry(id).or_insert_with(|| StreamWork {
                worker: CodeWorker::new(Highlighter::ready(Arc::clone(grammars))),
                spent: BTreeMap::new(),
                broken: kept.0,
                gave_up: kept.1,
            });
            work.worker.retire(request.retire_below);
            work.spent = work.spent.split_off(&request.retire_below);
            work.broken = work.broken.split_off(&request.retire_below);
            let mut updates = vec![];
            let mut last_reply = Instant::now();
            for (n, delta) in request.deltas.iter().enumerate() {
                if n > 0 && self.waiting_above(id) {
                    self.requeue(id, &request, n);
                    break;
                }
                if let Some(update) = self.apply(work, delta, &cancel) {
                    updates.push(update);
                }
                if last_reply.elapsed() >= PARTIAL_EVERY && !updates.is_empty() {
                    let batch = std::mem::take(&mut updates);
                    if !self.reply_at(
                        request.generation,
                        Reply::Updates {
                            stream: id,
                            updates: batch,
                        },
                    ) {
                        return;
                    }
                    last_reply = Instant::now();
                }
            }
            if !updates.is_empty()
                && !self.reply_at(
                    request.generation,
                    Reply::Updates {
                        stream: id,
                        updates,
                    },
                )
            {
                return;
            }
        }
    }

    fn apply(
        &mut self,
        work: &mut StreamWork,
        delta: &FenceDelta,
        cancel: &AtomicBool,
    ) -> Option<CodeUpdate> {
        if work.broken.contains(&delta.fence) {
            return Some(CodeUpdate {
                fence: delta.fence,
                lang: delta.lang.clone(),
                first: delta.from,
                lines: plain_lines(delta.lang.as_deref(), &delta.lines),
                cancelled: None,
                given_up: true,
            });
        }
        let mut out: Option<CodeUpdate> = None;
        let pieces: Vec<FenceDelta> = if delta.lines.is_empty() {
            vec![delta.clone()]
        } else {
            delta
                .lines
                .iter()
                .enumerate()
                .map(|(i, line)| FenceDelta {
                    fence: delta.fence,
                    lang: delta.lang.clone(),
                    from: delta.from + i,
                    lines: vec![line.clone()],
                })
                .collect()
        };
        for (i, piece) in pieces.iter().enumerate() {
            let spent = work.spent.get(&delta.fence).copied().unwrap_or_default();
            let governed = work.gave_up.contains(&delta.fence)
                || (spent >= GOVERNOR && !cancel.load(Ordering::SeqCst));
            let flag: &AtomicBool = if governed || spent >= GOVERNOR {
                &STOP
            } else {
                cancel
            };
            let started = Instant::now();
            let feed = &mut self.feed;
            let worker = &mut work.worker;
            let result = contain(|| feed(worker, piece, flag));
            *work.spent.entry(delta.fence).or_default() += started.elapsed();
            if governed {
                work.gave_up.insert(delta.fence);
            }
            let cut = !governed && cancel.load(Ordering::SeqCst);
            match result {
                Ok(Some(mut update)) => {
                    if cut {
                        update.cancelled = Some(update.first);
                    }
                    update.given_up = governed;
                    match &mut out {
                        Some(acc) if acc.first < update.first => {
                            acc.cancelled = match acc.cancelled {
                                Some(at) if at < update.first => Some(at),
                                _ => update.cancelled,
                            };
                            acc.given_up |= update.given_up;
                            acc.lines.truncate(update.first - acc.first);
                            acc.lines.extend(update.lines);
                        }
                        _ => out = Some(update),
                    }
                }
                Ok(None) => break,
                Err(_) => {
                    work.broken.insert(delta.fence);
                    let rest = delta.lines.get(i..).unwrap_or_default();
                    let plain = plain_lines(delta.lang.as_deref(), rest);
                    match &mut out {
                        Some(acc) => acc.lines.extend(plain),
                        None => {
                            out = Some(CodeUpdate {
                                fence: delta.fence,
                                lang: delta.lang.clone(),
                                first: piece.from,
                                lines: plain,
                                cancelled: None,
                                given_up: true,
                            })
                        }
                    }
                    break;
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests;
