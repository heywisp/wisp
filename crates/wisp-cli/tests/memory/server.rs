use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

const BUFFERS: usize = 12;
const BODY_CAPACITY: usize = 16 * 1024 * 1024;
const HEAD_CAPACITY: usize = 16 * 1024;
const NATIVE_ASK: &[u8] = br#""compaction":{"type":"summarize""#;
const WISP_ASK: &[u8] = b"Write a summary of the conversation so far";
const LAST_USER: &[u8] = br#""role":"user""#;
pub(crate) const MARK: &str = "MQ-";

pub(crate) struct Step {
    pub(crate) order: usize,
    pub(crate) response: String,
    pub(crate) served: AtomicBool,
}

pub(crate) struct Script {
    pub(crate) steps: BTreeMap<String, Step>,
    pub(crate) native: String,
    pub(crate) summary: String,
    pub(crate) fallback: String,
}

impl Script {
    pub(crate) fn reset(&self) {
        for step in self.steps.values() {
            step.served.store(false, Ordering::SeqCst);
        }
    }

    pub(crate) fn served(&self, marker: &str) -> bool {
        self.steps
            .get(marker)
            .is_some_and(|s| s.served.load(Ordering::SeqCst))
    }
}

#[derive(Default)]
pub(crate) struct Seen {
    pub(crate) bytes: AtomicU64,
    pub(crate) largest: AtomicU64,
    pub(crate) requests: AtomicU64,
    pub(crate) native: AtomicU64,
    pub(crate) summaries: AtomicU64,
    pub(crate) fallbacks: AtomicU64,
}

impl Seen {
    pub(crate) fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::SeqCst)
    }

    pub(crate) fn largest(&self) -> u64 {
        self.largest.load(Ordering::SeqCst)
    }
}

struct Buffer {
    head: String,
    body: Vec<u8>,
}

pub(crate) struct Server {
    pub(crate) url: String,
    pub(crate) seen: Arc<Seen>,
}

pub(crate) fn serve(script: Arc<Script>) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let seen = Arc::new(Seen::default());
    let pool: Arc<Mutex<Vec<Buffer>>> = Arc::new(Mutex::new(
        (0..BUFFERS)
            .map(|_| Buffer {
                head: String::with_capacity(HEAD_CAPACITY),
                body: Vec::with_capacity(BODY_CAPACITY),
            })
            .collect(),
    ));
    let counting = Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let script = Arc::clone(&script);
            let seen = Arc::clone(&counting);
            let pool = Arc::clone(&pool);
            std::thread::spawn(move || connection(stream, &script, &seen, &pool));
        }
    });
    Server { url, seen }
}

fn connection(stream: TcpStream, script: &Script, seen: &Seen, pool: &Mutex<Vec<Buffer>>) {
    let taken = pool.lock().unwrap_or_else(PoisonError::into_inner).pop();
    let mut buffer = taken.unwrap_or_else(|| Buffer {
        head: String::new(),
        body: Vec::new(),
    });
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    while read_request(&mut reader, &mut buffer) {
        let length = buffer.body.len() as u64;
        seen.bytes.fetch_add(length, Ordering::SeqCst);
        seen.largest.fetch_max(length, Ordering::SeqCst);
        seen.requests.fetch_add(1, Ordering::SeqCst);
        let reply = answer(&buffer, script, seen);
        if writer.write_all(reply.as_bytes()).is_err() {
            break;
        }
    }
    pool.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(buffer);
}

fn read_request(reader: &mut BufReader<TcpStream>, buffer: &mut Buffer) -> bool {
    buffer.head.clear();
    let mut length = 0usize;
    loop {
        let start = buffer.head.len();
        match reader.read_line(&mut buffer.head) {
            Ok(0) | Err(_) => return false,
            Ok(_) => {}
        }
        let line = &buffer.head[start..];
        if line.trim_end().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    buffer.body.clear();
    buffer.body.resize(length, 0);
    reader.read_exact(&mut buffer.body).is_ok()
}

fn answer<'a>(buffer: &Buffer, script: &'a Script, seen: &Seen) -> &'a str {
    if buffer.head.starts_with("POST /v1/messages/count_tokens ") {
        return COUNTED;
    }
    let body = buffer.body.as_slice();
    if find(body, NATIVE_ASK).is_some() {
        seen.native.fetch_add(1, Ordering::SeqCst);
        return &script.native;
    }
    let last = rfind(body, LAST_USER).map_or(body, |at| &body[at..]);
    if find(last, WISP_ASK).is_some() {
        seen.summaries.fetch_add(1, Ordering::SeqCst);
        return &script.summary;
    }
    if let Some(step) = newest_step(last, script)
        && !step.served.swap(true, Ordering::SeqCst)
    {
        return &step.response;
    }
    seen.fallbacks.fetch_add(1, Ordering::SeqCst);
    &script.fallback
}

const COUNTED: &str = "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 20\r\n\r\n{\"input_tokens\":900}";

fn newest_step<'a>(text: &[u8], script: &'a Script) -> Option<&'a Step> {
    let mut newest: Option<&Step> = None;
    let mut rest = text;
    while let Some(at) = find(rest, MARK.as_bytes()) {
        let tail = &rest[at..];
        let end = tail
            .iter()
            .position(|b| !(b.is_ascii_alphanumeric() || *b == b'-'))
            .unwrap_or(tail.len());
        if let Ok(marker) = std::str::from_utf8(&tail[..end])
            && let Some(step) = script.steps.get(marker)
            && newest.is_none_or(|n| step.order > n.order)
        {
            newest = Some(step);
        }
        rest = &tail[MARK.len()..];
    }
    newest
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).rposition(|w| w == needle)
}
