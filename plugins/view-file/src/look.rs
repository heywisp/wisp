use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::PathBuf;

use wisp_media::MAX_ATTACHMENT_BYTES;
use wisp_plugin_api::host::{CallContext, FileKind, Host};
use wisp_plugin_api::types::{FileSlice, SliceKind};
use wisp_plugin_api::yield_now;
use wisp_tool_text::files as sentences;

use super::words;

pub(crate) const SNIFF_BYTES: usize = 8 * 1024;
pub(crate) const MAX_LINES: u64 = 2_000;
pub(crate) const MAX_TEXT_BYTES: usize = 50 * 1024;
pub(crate) const MAX_LINE_CHARS: usize = 2_000;
pub(crate) const COUNT_BYTES: u64 = 64 * 1024 * 1024;
pub(crate) const CHUNK_BYTES: usize = 64 * 1024;
const LINE_KEEP_BYTES: usize = MAX_LINE_CHARS * 4 + 4;
const BOM: &[u8] = b"\xEF\xBB\xBF";
const PDF_HEADER: &[u8] = b"%PDF-";
const NAMES: [(&str, &str); 5] = [
    ("audio/x-aiff", "audio/aiff"),
    ("audio/x-flac", "audio/flac"),
    ("audio/x-wav", "audio/wav"),
    ("video/x-ms-wmv", "video/wmv"),
    ("video/x-msvideo", "video/avi"),
];
const OCTET_STREAM: &str = "application/octet-stream";
const OLE_MATCH: &str = "application/msword";
pub(crate) const OLE_FILE: &str =
    "an OLE compound file (such as an old Office document or an installer)";

pub(crate) struct Job<'a> {
    pub(crate) written: String,
    pub(crate) resolved: PathBuf,
    pub(crate) shown: String,
    pub(crate) media_types: Vec<String>,
    pub(crate) media_slices: Vec<SliceKind>,
    pub(crate) slice: Option<FileSlice>,
    pub(crate) host: &'a dyn Host,
    pub(crate) ctx: &'a dyn CallContext,
    pub(crate) count_until_ms: u64,
    pub(crate) bash: bool,
}

impl Job<'_> {
    fn named(&self) -> String {
        if self.shown == self.written {
            self.written.clone()
        } else {
            format!("{} (resolved to {})", self.written, self.shown)
        }
    }

    fn stopped(&self) -> bool {
        self.ctx.cancelled()
    }

    fn carried(&self) -> Vec<&str> {
        self.media_types.iter().map(String::as_str).collect()
    }

    fn shell(&self) -> Option<&'static str> {
        self.bash.then_some(BASH)
    }
}

const BASH: &str = "bash";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Total {
    Exact(u64),
    MoreThan(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TextView {
    pub(crate) text: String,
    pub(crate) lines: u64,
    pub(crate) total: Total,
    pub(crate) replaced: u64,
    pub(crate) cut: u64,
    pub(crate) raw: u64,
    pub(crate) size: u64,
}

impl TextView {
    pub(crate) fn whole(&self) -> bool {
        self.total == Total::Exact(self.lines) && self.cut == 0
    }

    pub(crate) fn every_line(&self) -> bool {
        self.total == Total::Exact(self.lines)
    }
}

pub(crate) enum Looked {
    Refused(String),
    Text(TextView),
    Media { media_type: String, bytes: Vec<u8> },
    Stopped,
}

enum Sniffed {
    Media(String),
    Text,
    Uncarried(String),
}

pub(crate) async fn look(job: &Job<'_>) -> Looked {
    let path = job.resolved.display().to_string();
    match job.host.kind_of(&path) {
        Some(FileKind::Regular) => {}
        Some(kind) => return Looked::Refused(not_a_file(job, kind)),
        None => {
            if let Err(e) = std::fs::metadata(&job.resolved) {
                return Looked::Refused(open_failed(job, &e));
            }
        }
    }
    let mut file = match File::open(&job.resolved) {
        Ok(file) => file,
        Err(e) => return Looked::Refused(open_failed(job, &e)),
    };
    let size = match file.metadata() {
        Ok(meta) if meta.is_file() => meta.len(),
        Ok(_) => {
            let kind = job.host.kind_of(&path).unwrap_or(FileKind::Other);
            return Looked::Refused(not_a_file(job, kind));
        }
        Err(e) => return Looked::Refused(unreadable(job, &e)),
    };
    let head = match read_head(&mut file) {
        Ok(head) => head,
        Err(e) => return Looked::Refused(unreadable(job, &e)),
    };
    if job.stopped() {
        return Looked::Stopped;
    }
    let carried = job.carried();
    match sniff(&head, &carried) {
        Sniffed::Uncarried(media_type) => Looked::Refused(sentences::uncarried(
            &job.written,
            &media_type,
            &carried,
            job.shell(),
        )),
        Sniffed::Media(media_type) => {
            if let Some(slice) = job.slice
                && let Some(reason) = sentences::slice_refusal(
                    &job.written,
                    &media_type,
                    slice.kind(),
                    &job.media_slices,
                    job.shell(),
                )
            {
                return Looked::Refused(reason);
            }
            read_media(job, file, size, media_type).await
        }
        Sniffed::Text => {
            if let Some(slice) = job.slice {
                return Looked::Refused(sentences::text_with_slice(
                    &job.written,
                    slice.kind(),
                    job.shell(),
                ));
            }
            read_text(job, file, size, head.starts_with(BOM)).await
        }
    }
}

fn open_failed(job: &Job<'_>, e: &io::Error) -> String {
    if e.kind() == io::ErrorKind::NotFound {
        let similar = job
            .resolved
            .parent()
            .zip(job.resolved.file_name())
            .map(|(dir, name)| words::similar_names(dir, &name.to_string_lossy()))
            .unwrap_or_default();
        return words::not_found(&job.written, &job.shown, &similar);
    }
    unreadable(job, e)
}

fn not_a_file(job: &Job<'_>, kind: FileKind) -> String {
    let named = job.named();
    match kind {
        FileKind::Directory if !job.bash => format!("{named} is a directory, not a file."),
        FileKind::Directory => format!("{named} is a directory; list it with bash (ls)."),
        FileKind::Fifo => format!("{named} is a FIFO, not a file."),
        FileKind::Socket => format!("{named} is a socket, not a file."),
        FileKind::Device => format!("{named} is a device, not a file."),
        FileKind::Regular | FileKind::Other => format!("{named} is not a regular file."),
    }
}

fn unreadable(job: &Job<'_>, e: &io::Error) -> String {
    let named = job.named();
    if e.kind() == io::ErrorKind::PermissionDenied {
        return format!("{named} can't be read: permission denied.");
    }
    format!("{named} can't be read: {}.", words::io_reason(e))
}

fn read_head(file: &mut File) -> io::Result<Vec<u8>> {
    let mut head = Vec::with_capacity(SNIFF_BYTES);
    file.by_ref()
        .take(SNIFF_BYTES as u64)
        .read_to_end(&mut head)?;
    Ok(head)
}

fn sniff(head: &[u8], carried: &[&str]) -> Sniffed {
    let found = infer::get(head).map(|t| t.mime_type());
    if found == Some(OLE_MATCH) {
        return Sniffed::Uncarried(OLE_FILE.to_owned());
    }
    if is_text(head)
        && !head
            .strip_prefix(BOM)
            .unwrap_or(head)
            .starts_with(PDF_HEADER)
    {
        return Sniffed::Text;
    }
    let name = named(found.unwrap_or(OCTET_STREAM));
    if found.is_some() && sentences::carries(carried, name) {
        return Sniffed::Media(name.to_owned());
    }
    Sniffed::Uncarried(name.to_owned())
}

fn named(media_type: &str) -> &str {
    NAMES
        .iter()
        .find_map(|(old, new)| (*old == media_type).then_some(*new))
        .unwrap_or(media_type)
}

pub(crate) fn is_text(head: &[u8]) -> bool {
    let body = head.strip_prefix(BOM).unwrap_or(head);
    if body.contains(&0) {
        return false;
    }
    match std::str::from_utf8(body) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none() && head.len() == SNIFF_BYTES,
    }
}

async fn read_media(job: &Job<'_>, file: File, size: u64, media_type: String) -> Looked {
    let cap = MAX_ATTACHMENT_BYTES as u64;
    let too_large = |bytes: u64| {
        Looked::Refused(sentences::too_large(
            &job.written,
            bytes,
            cap,
            "view that",
            job.shell(),
        ))
    };
    if size > cap {
        return too_large(size);
    }
    let mut file = file;
    if let Err(e) = file.seek(SeekFrom::Start(0)) {
        return Looked::Refused(unreadable(job, &e));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
    let mut limited = file.take(cap + 1);
    let mut chunk = vec![0; CHUNK_BYTES];
    loop {
        if job.stopped() {
            return Looked::Stopped;
        }
        match limited.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => bytes.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Looked::Refused(unreadable(job, &e)),
        }
        yield_now().await;
    }
    if job.stopped() {
        return Looked::Stopped;
    }
    let read = bytes.len() as u64;
    if read > cap {
        return too_large(read);
    }
    Looked::Media { media_type, bytes }
}

struct Line {
    bytes: Vec<u8>,
    ended: bool,
    cut: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Next {
    Line,
    Eof,
    Capped,
    Stopped,
}

impl Job<'_> {
    fn capped(&self, read: u64) -> bool {
        read >= COUNT_BYTES || self.host.elapsed_ms() >= self.count_until_ms
    }
}

async fn next_line(
    job: &Job<'_>,
    reader: &mut impl BufRead,
    line: &mut Line,
    read: &mut u64,
    from: u64,
) -> io::Result<Next> {
    line.bytes.clear();
    line.ended = false;
    line.cut = false;
    let mut any = false;
    loop {
        if job.stopped() {
            return Ok(Next::Stopped);
        }
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            return Ok(if any { Next::Line } else { Next::Eof });
        }
        if *read > from && job.capped(*read) {
            line.cut = any;
            return Ok(Next::Capped);
        }
        any = true;
        let found = buf.iter().position(|b| *b == b'\n').map(|i| i + 1);
        let limit = usize::try_from(COUNT_BYTES - *read).unwrap_or(usize::MAX);
        let len = found.unwrap_or(buf.len()).min(limit);
        let ended = found == Some(len);
        let room = LINE_KEEP_BYTES.saturating_sub(line.bytes.len());
        let keep = len.min(room);
        line.bytes.extend_from_slice(&buf[..keep]);
        line.cut |= keep < len;
        reader.consume(len);
        *read += len as u64;
        if ended {
            line.ended = true;
            return Ok(Next::Line);
        }
        yield_now().await;
    }
}

fn decoded(line: &Line) -> (String, u64, bool) {
    let body = match line.bytes.strip_suffix(b"\n") {
        Some(body) if line.ended && !line.cut => body,
        _ => &line.bytes[..],
    };
    let mut out = String::new();
    let mut chars = 0;
    let mut replaced = 0;
    let mut cut = line.cut;
    'chunks: for chunk in body.utf8_chunks() {
        for c in chunk.valid().chars() {
            if chars == MAX_LINE_CHARS {
                cut = true;
                break 'chunks;
            }
            out.push(c);
            chars += 1;
        }
        if !chunk.invalid().is_empty() {
            if chars == MAX_LINE_CHARS {
                cut = true;
                break;
            }
            out.push('\u{fffd}');
            chars += 1;
            replaced += 1;
        }
    }
    if cut {
        out.push_str(&words::line_cut());
    }
    if line.ended {
        out.push('\n');
    }
    (out, replaced, cut)
}

async fn read_text(job: &Job<'_>, file: File, size: u64, bom: bool) -> Looked {
    let mut file = file;
    let start = if bom { BOM.len() as u64 } else { 0 };
    if let Err(e) = file.seek(SeekFrom::Start(start)) {
        return Looked::Refused(unreadable(job, &e));
    }
    let mut reader = BufReader::with_capacity(CHUNK_BYTES, file);
    let mut read = start;
    let mut line = Line {
        bytes: Vec::new(),
        ended: false,
        cut: false,
    };
    let mut text = String::new();
    let mut lines = 0;
    let mut ended = 0;
    let mut replaced = 0;
    let mut cut = 0;
    let mut raw = 0;
    let mut held = 0;
    let mut capped = false;
    loop {
        let next = match next_line(job, &mut reader, &mut line, &mut read, start).await {
            Ok(next) => next,
            Err(e) => return Looked::Refused(unreadable(job, &e)),
        };
        match next {
            Next::Stopped => return Looked::Stopped,
            Next::Eof => break,
            Next::Capped if line.bytes.is_empty() => {
                capped = true;
                break;
            }
            Next::Line | Next::Capped => {}
        }
        capped = next == Next::Capped;
        let (shown, bad, was_cut) = decoded(&line);
        ended += u64::from(line.ended);
        if text.len() + shown.len() > MAX_TEXT_BYTES {
            held = 1;
            break;
        }
        text.push_str(&shown);
        lines += 1;
        replaced += bad;
        cut += u64::from(was_cut);
        raw = read - start;
        if capped || lines == MAX_LINES {
            break;
        }
    }
    let total = if capped {
        Total::MoreThan(ended)
    } else {
        match count_rest(job, &mut reader, read).await {
            Ok(Some(rest)) => rest.add(lines + held),
            Ok(None) => return Looked::Stopped,
            Err(e) => return Looked::Refused(unreadable(job, &e)),
        }
    };
    Looked::Text(TextView {
        text,
        lines,
        total,
        replaced,
        cut,
        raw,
        size,
    })
}

impl Total {
    fn add(self, n: u64) -> Self {
        match self {
            Self::Exact(m) => Self::Exact(m + n),
            Self::MoreThan(m) => Self::MoreThan(m + n),
        }
    }
}

async fn count_rest(
    job: &Job<'_>,
    reader: &mut impl BufRead,
    mut read: u64,
) -> io::Result<Option<Total>> {
    let mut newlines = 0;
    let mut at_line_start = true;
    loop {
        if job.stopped() {
            return Ok(None);
        }
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            let partial = u64::from(!at_line_start);
            return Ok(Some(Total::Exact(newlines + partial)));
        }
        if job.capped(read) {
            return Ok(Some(Total::MoreThan(newlines)));
        }
        let take = buf
            .len()
            .min(usize::try_from(COUNT_BYTES - read).unwrap_or(usize::MAX));
        let chunk = &buf[..take];
        newlines += chunk.iter().filter(|b| **b == b'\n').count() as u64;
        at_line_start = chunk.last() == Some(&b'\n');
        reader.consume(take);
        read += take as u64;
        yield_now().await;
    }
}
