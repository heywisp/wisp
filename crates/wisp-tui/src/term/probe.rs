use std::io;
use std::time::{Duration, Instant};

use wisp_render::theme::Rgb;

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const LONGEST_REPLY: usize = 256;
const SYNC_MODE: u16 = 2026;
const ALTERNATE_SCROLL_MODE: u16 = 1007;

pub const WIDTH_GLYPH: &str = "\u{b7}";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProbePlan {
    pub deadline: Duration,
    pub sync_query: bool,
    pub scroll_query: bool,
    pub width_test: bool,
    pub pictures: bool,
}

impl ProbePlan {
    pub fn query(&self) -> Vec<u8> {
        let mut q = String::new();
        q.push_str("\x1b]10;?\x1b\\");
        q.push_str("\x1b]11;?\x1b\\");
        if self.sync_query {
            q.push_str("\x1b[?2026$p");
        }
        if self.scroll_query {
            q.push_str("\x1b[?1007$p");
        }
        q.push_str("\x1b[?u");
        q.push_str("\x1b[6n");
        if self.width_test {
            q.push_str("\x1b7");
            q.push_str(WIDTH_GLYPH);
            q.push_str("\x1b[6n");
            q.push_str("\x1b8\x1b[2X");
        }
        if self.pictures {
            q.push_str("\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[16t");
        }
        q.push_str("\x1b[c");
        q.into_bytes()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Position {
    pub row: u16,
    pub col: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Foreground(Rgb),
    Background(Rgb),
    ModeReport { mode: u16, status: u16 },
    KittyKeyboard(u8),
    CursorPosition(Position),
    DeviceAttributes(Vec<u16>),
    Graphics(bool),
    CellPixels { width: u16, height: u16 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    Reply(Reply),
    Typed(Vec<u8>),
    Dropped(Vec<u8>),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReplyParser {
    pending: Vec<u8>,
    strict: bool,
}

enum Scan {
    Need,
    Take(usize, Option<Item>),
}

impl ReplyParser {
    pub fn new() -> ReplyParser {
        ReplyParser::default()
    }

    pub fn strict() -> ReplyParser {
        ReplyParser::resuming(vec![])
    }

    pub fn resuming(pending: Vec<u8>) -> ReplyParser {
        ReplyParser {
            pending,
            strict: true,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Item> {
        self.feed_raw(bytes)
            .into_iter()
            .map(|(item, _)| item)
            .collect()
    }

    pub fn feed_raw(&mut self, bytes: &[u8]) -> Vec<(Item, Vec<u8>)> {
        self.pending.extend_from_slice(bytes);
        let mut items = vec![];
        let mut at = 0;
        while at < self.pending.len() {
            match scan(&self.pending[at..], self.strict) {
                Scan::Need => break,
                Scan::Take(n, item) => {
                    let n = n.max(1).min(self.pending.len() - at);
                    if let Some(item) = item {
                        items.push((item, self.pending[at..at + n].to_vec()));
                    }
                    at += n;
                }
            }
        }
        self.pending.drain(..at);
        items
    }

    pub fn take_pending(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending)
    }

    pub fn finish(self) -> Item {
        let rest = self.pending;
        if rest.starts_with(b"\x1b]") || rest.starts_with(b"\x1b[?") {
            Item::Dropped(rest)
        } else {
            Item::Typed(rest)
        }
    }
}

fn scan(buf: &[u8], strict: bool) -> Scan {
    if buf[0] != ESC {
        let n = buf.iter().position(|b| *b == ESC).unwrap_or(buf.len());
        return Scan::Take(n, Some(Item::Typed(buf[..n].to_vec())));
    }
    match buf.get(1) {
        None => Scan::Need,
        Some(b']') if strict && !could_be_colour_reply(&buf[2..]) => {
            Scan::Take(1, Some(Item::Typed(vec![ESC])))
        }
        Some(b'_') if strict && buf.get(2).is_some_and(|b| *b != b'G') => {
            Scan::Take(1, Some(Item::Typed(vec![ESC])))
        }
        Some(b']') => scan_string(buf, osc_item),
        Some(b'_') => scan_string(buf, apc_item),
        Some(b'[') => scan_csi(buf),
        Some(_) => Scan::Take(1, Some(Item::Typed(vec![ESC]))),
    }
}

fn could_be_colour_reply(body: &[u8]) -> bool {
    [b"10;".as_slice(), b"11;"]
        .iter()
        .any(|code| body.iter().zip(code.iter()).all(|(a, b)| a == b))
}

fn scan_string(buf: &[u8], item: fn(&[u8], &[u8]) -> Item) -> Scan {
    let mut i = 2;
    while i < buf.len() {
        match buf[i] {
            BEL => return Scan::Take(i + 1, Some(item(&buf[2..i], &buf[..=i]))),
            ESC => {
                return match buf.get(i + 1) {
                    None => Scan::Need,
                    Some(b'\\') => Scan::Take(i + 2, Some(item(&buf[2..i], &buf[..i + 2]))),
                    Some(_) => Scan::Take(i, Some(Item::Dropped(buf[..i].to_vec()))),
                };
            }
            _ => i += 1,
        }
    }
    if buf.len() > LONGEST_REPLY {
        Scan::Take(buf.len(), Some(Item::Dropped(buf.to_vec())))
    } else {
        Scan::Need
    }
}

fn osc_item(body: &[u8], whole: &[u8]) -> Item {
    let text = String::from_utf8_lossy(body);
    let reply = text.split_once(';').and_then(|(code, value)| {
        let rgb = parse_color(value)?;
        match code {
            "10" => Some(Reply::Foreground(rgb)),
            "11" => Some(Reply::Background(rgb)),
            _ => None,
        }
    });
    match reply {
        Some(r) => Item::Reply(r),
        None => Item::Dropped(whole.to_vec()),
    }
}

fn apc_item(body: &[u8], whole: &[u8]) -> Item {
    let text = String::from_utf8_lossy(body);
    match text.strip_prefix('G').and_then(|g| g.split_once(';')) {
        Some((_, message)) => Item::Reply(Reply::Graphics(message == "OK")),
        None => Item::Dropped(whole.to_vec()),
    }
}

pub fn parse_color(value: &str) -> Option<Rgb> {
    let value = value.trim();
    if let Some(hex) = value.strip_prefix('#') {
        if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let v = u32::from_str_radix(hex, 16).ok()?;
        return Some(Rgb::hex(v));
    }
    let channels = value
        .strip_prefix("rgb:")
        .or_else(|| value.strip_prefix("rgba:"))?;
    let parts: Vec<&str> = channels.split('/').collect();
    if parts.len() < 3 || parts.len() > 4 {
        return None;
    }
    let channel = |s: &str| -> Option<u8> {
        if s.is_empty() || s.len() > 4 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let v = u32::from_str_radix(s, 16).ok()?;
        let max = (1u32 << (4 * s.len())) - 1;
        u8::try_from((v * 255 + max / 2) / max).ok()
    };
    Some(Rgb {
        r: channel(parts[0])?,
        g: channel(parts[1])?,
        b: channel(parts[2])?,
    })
}

fn scan_csi(buf: &[u8]) -> Scan {
    let mut i = 2;
    let params_start = i;
    while i < buf.len() && (0x30..=0x3f).contains(&buf[i]) {
        i += 1;
    }
    let params_end = i;
    while i < buf.len() && (0x20..=0x2f).contains(&buf[i]) {
        i += 1;
    }
    let inter_end = i;
    let Some(&last) = buf.get(i) else {
        return if buf.len() > LONGEST_REPLY {
            Scan::Take(2, Some(Item::Typed(buf[..2].to_vec())))
        } else {
            Scan::Need
        };
    };
    if !(0x40..=0x7e).contains(&last) {
        return Scan::Take(2, Some(Item::Typed(buf[..2].to_vec())));
    }
    let whole = &buf[..=i];
    let params = &buf[params_start..params_end];
    let inter = &buf[params_end..inter_end];
    Scan::Take(i + 1, Some(csi_item(params, inter, last, whole)))
}

fn numbers(params: &[u8]) -> Option<Vec<u16>> {
    std::str::from_utf8(params)
        .ok()?
        .split(';')
        .map(|p| {
            if p.is_empty() {
                Some(0)
            } else {
                p.parse().ok()
            }
        })
        .collect()
}

fn csi_item(params: &[u8], inter: &[u8], last: u8, whole: &[u8]) -> Item {
    let private = params.first() == Some(&b'?');
    let reply = match (private, inter, last) {
        (true, b"$", b'y') => numbers(&params[1..]).and_then(|n| match n.as_slice() {
            [mode, status] => Some(Reply::ModeReport {
                mode: *mode,
                status: *status,
            }),
            _ => None,
        }),
        (true, b"", b'u') => numbers(&params[1..])
            .and_then(|n| match n.as_slice() {
                [flags] => u8::try_from(*flags).ok(),
                _ => None,
            })
            .map(Reply::KittyKeyboard),
        (true, b"", b'c') => numbers(&params[1..]).map(Reply::DeviceAttributes),
        (false, b"", b'R') => numbers(params).and_then(|n| match n.as_slice() {
            [row, col] => Some(Reply::CursorPosition(Position {
                row: *row,
                col: *col,
            })),
            _ => None,
        }),
        (false, b"", b't') => numbers(params).and_then(|n| match n.as_slice() {
            [6, height, width] => Some(Reply::CellPixels {
                width: *width,
                height: *height,
            }),
            _ => None,
        }),
        _ => None,
    };
    match reply {
        Some(r) => Item::Reply(r),
        None if private => Item::Dropped(whole.to_vec()),
        None => Item::Typed(whole.to_vec()),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Answered,
    TimedOut,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SyncSupport {
    #[default]
    NotAsked,
    NoReply,
    Supported,
    Unsupported,
}

impl SyncSupport {
    pub const fn supported(self) -> Option<bool> {
        match self {
            SyncSupport::Supported => Some(true),
            SyncSupport::Unsupported => Some(false),
            SyncSupport::NotAsked | SyncSupport::NoReply => None,
        }
    }

    const fn from_status(status: u16) -> SyncSupport {
        match status {
            1..=3 => SyncSupport::Supported,
            _ => SyncSupport::Unsupported,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeReport {
    pub plan: ProbePlan,
    pub outcome: Outcome,
    pub elapsed: Duration,
    pub foreground: Option<Rgb>,
    pub background: Option<Rgb>,
    pub synchronized_output: SyncSupport,
    pub alternate_scroll: Option<u16>,
    pub kitty_keyboard: Option<u8>,
    pub origin: Option<Position>,
    pub dot_width: Option<u16>,
    pub device_attributes: Option<Vec<u16>>,
    pub graphics: Option<bool>,
    pub cell_pixels: Option<(u16, u16)>,
    pub typed: Vec<u8>,
    pub dropped: usize,
    pub unfinished: Vec<u8>,
}

impl ProbeReport {
    pub fn new(plan: ProbePlan) -> ProbeReport {
        ProbeReport {
            plan,
            outcome: Outcome::TimedOut,
            elapsed: Duration::ZERO,
            foreground: None,
            background: None,
            synchronized_output: if plan.sync_query {
                SyncSupport::NoReply
            } else {
                SyncSupport::NotAsked
            },
            alternate_scroll: None,
            kitty_keyboard: None,
            origin: None,
            dot_width: None,
            device_attributes: None,
            graphics: None,
            cell_pixels: None,
            typed: vec![],
            dropped: 0,
            unfinished: vec![],
        }
    }

    fn take(&mut self, item: Item, positions: &mut Vec<Position>) {
        match item {
            Item::Typed(bytes) => self.typed.extend(bytes),
            Item::Dropped(bytes) => self.dropped += bytes.len(),
            Item::Reply(Reply::Foreground(c)) => self.foreground = Some(c),
            Item::Reply(Reply::Background(c)) => self.background = Some(c),
            Item::Reply(Reply::ModeReport { mode, status }) if mode == SYNC_MODE => {
                self.synchronized_output = SyncSupport::from_status(status);
            }
            Item::Reply(Reply::ModeReport { mode, status }) if mode == ALTERNATE_SCROLL_MODE => {
                self.alternate_scroll = Some(status);
            }
            Item::Reply(Reply::ModeReport { .. }) => self.dropped += 1,
            Item::Reply(Reply::KittyKeyboard(flags)) => self.kitty_keyboard = Some(flags),
            Item::Reply(Reply::CursorPosition(p)) => {
                positions.push(p);
                self.origin = positions.first().copied();
                if self.plan.width_test
                    && let [before, after, ..] = positions.as_slice()
                    && before.row == after.row
                {
                    self.dot_width = after.col.checked_sub(before.col);
                }
            }
            Item::Reply(Reply::Graphics(ok)) => self.graphics = Some(ok),
            Item::Reply(Reply::CellPixels { width, height }) => {
                self.cell_pixels = Some((width, height));
            }
            Item::Reply(Reply::DeviceAttributes(params)) => {
                self.device_attributes = Some(params);
                self.outcome = Outcome::Answered;
            }
        }
    }
}

pub trait ProbeIo {
    fn send(&mut self, bytes: &[u8]) -> io::Result<()>;
    fn receive(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize>;
}

pub fn run(io: &mut impl ProbeIo, plan: ProbePlan) -> io::Result<ProbeReport> {
    let start = Instant::now();
    let mut report = ProbeReport::new(plan);
    let mut parser = ReplyParser::new();
    let mut positions = vec![];
    io.send(&plan.query())?;
    let mut buf = [0u8; 1024];
    while report.outcome == Outcome::TimedOut {
        let left = plan.deadline.saturating_sub(start.elapsed());
        if left.is_zero() {
            break;
        }
        let n = io.receive(&mut buf, left)?;
        for item in parser.feed(&buf[..n]) {
            report.take(item, &mut positions);
        }
    }
    if report.outcome == Outcome::TimedOut {
        report.unfinished = parser.take_pending();
    } else {
        report.take(parser.finish(), &mut positions);
    }
    report.elapsed = start.elapsed();
    Ok(report)
}

pub const LATE_GRACE: Duration = Duration::from_secs(1);
const LATE_POLL: Duration = Duration::from_millis(20);

pub fn drain(
    mut receive: impl FnMut(&mut [u8], Duration) -> io::Result<usize>,
    late: &mut LateReplies,
    mut each: impl FnMut(Vec<u8>) -> bool,
) -> io::Result<()> {
    let mut buf = [0u8; 1024];
    while late.active(Instant::now()) {
        let n = receive(&mut buf, LATE_POLL)?;
        if !each(late.filter(&buf[..n], Instant::now())) {
            return Ok(());
        }
    }
    each(late.close());
    Ok(())
}

pub(super) const PASTE_START: &[u8] = b"\x1b[200~";
pub(super) const PASTE_END: &[u8] = b"\x1b[201~";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Owed {
    pub foreground: u8,
    pub background: u8,
    pub mode_report: u8,
    pub scroll_report: u8,
    pub kitty: u8,
    pub cursor_positions: u8,
    pub device_attributes: u8,
    pub graphics: u8,
    pub cell_pixels: u8,
}

impl Owed {
    pub fn by(report: &ProbeReport) -> Owed {
        if report.outcome == Outcome::Answered {
            return Owed::default();
        }
        let asked_cprs = 1 + u8::from(report.plan.width_test);
        let got_cprs = u8::from(report.origin.is_some()) + u8::from(report.dot_width.is_some());
        Owed {
            foreground: u8::from(report.foreground.is_none()),
            background: u8::from(report.background.is_none()),
            mode_report: u8::from(report.synchronized_output == SyncSupport::NoReply),
            scroll_report: u8::from(report.plan.scroll_query && report.alternate_scroll.is_none()),
            kitty: u8::from(report.kitty_keyboard.is_none()),
            cursor_positions: asked_cprs.saturating_sub(got_cprs),
            device_attributes: u8::from(report.device_attributes.is_none()),
            graphics: u8::from(report.plan.pictures && report.graphics.is_none()),
            cell_pixels: u8::from(report.plan.pictures && report.cell_pixels.is_none()),
        }
    }

    fn take(&mut self, reply: &Reply) -> bool {
        let slot = match reply {
            Reply::Foreground(_) => &mut self.foreground,
            Reply::Background(_) => &mut self.background,
            Reply::ModeReport { mode, .. } if *mode == SYNC_MODE => &mut self.mode_report,
            Reply::ModeReport { mode, .. } if *mode == ALTERNATE_SCROLL_MODE => {
                &mut self.scroll_report
            }
            Reply::ModeReport { .. } => return false,
            Reply::KittyKeyboard(_) => &mut self.kitty,
            Reply::CursorPosition(_) => &mut self.cursor_positions,
            Reply::DeviceAttributes(_) => &mut self.device_attributes,
            Reply::Graphics(_) => &mut self.graphics,
            Reply::CellPixels { .. } => &mut self.cell_pixels,
        };
        if *slot == 0 {
            return false;
        }
        *slot -= 1;
        true
    }
}

pub(super) fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn partial_suffix(hay: &[u8], needle: &[u8]) -> usize {
    (1..needle.len())
        .rev()
        .find(|n| hay.ends_with(&needle[..*n]))
        .unwrap_or(0)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LateReplies {
    parser: ReplyParser,
    until: Option<Instant>,
    owed: Owed,
    pasting: bool,
    held: Vec<u8>,
}

impl LateReplies {
    pub fn after(report: &ProbeReport, now: Instant) -> LateReplies {
        LateReplies {
            parser: ReplyParser::resuming(report.unfinished.clone()),
            until: (report.outcome == Outcome::TimedOut)
                .then(|| now.checked_add(LATE_GRACE).unwrap_or(now)),
            owed: Owed::by(report),
            pasting: false,
            held: vec![],
        }
    }

    pub fn active(&self, now: Instant) -> bool {
        self.until.is_some_and(|until| now < until)
    }

    pub fn filter(&mut self, bytes: &[u8], now: Instant) -> Vec<u8> {
        if !self.active(now) {
            let mut out = self.close();
            out.extend_from_slice(bytes);
            return out;
        }
        let mut out = vec![];
        self.consume(bytes.to_vec(), now, &mut out);
        if !self.active(now) {
            out.extend(self.close());
        }
        out
    }

    fn consume(&mut self, mut input: Vec<u8>, now: Instant, out: &mut Vec<u8>) {
        loop {
            if self.pasting {
                let mut buf = std::mem::take(&mut self.held);
                buf.extend(input);
                match find(&buf, PASTE_END) {
                    Some(i) => {
                        let end = i + PASTE_END.len();
                        out.extend_from_slice(&buf[..end]);
                        self.pasting = false;
                        input = buf[end..].to_vec();
                        continue;
                    }
                    None => {
                        let keep = partial_suffix(&buf, PASTE_END);
                        out.extend_from_slice(&buf[..buf.len() - keep]);
                        self.held = buf[buf.len() - keep..].to_vec();
                        return;
                    }
                }
            }
            let mut items = self.parser.feed_raw(&input).into_iter();
            let mut rest = None;
            for (item, raw) in items.by_ref() {
                if raw == PASTE_START {
                    out.extend(raw);
                    self.pasting = true;
                    rest = Some(());
                    break;
                }
                match item {
                    Item::Reply(reply) if self.owed.take(&reply) => {
                        if matches!(reply, Reply::DeviceAttributes(_)) {
                            self.until = Some(now);
                        }
                    }
                    Item::Typed(keys) => out.extend(keys),
                    Item::Reply(_) | Item::Dropped(_) => out.extend(raw),
                }
            }
            if rest.is_none() {
                return;
            }
            let mut remaining: Vec<u8> = items.flat_map(|(_, raw)| raw).collect();
            remaining.extend(self.parser.take_pending());
            input = remaining;
        }
    }

    pub fn close(&mut self) -> Vec<u8> {
        self.until = None;
        let mut out = std::mem::take(&mut self.held);
        out.extend(self.parser.take_pending());
        out
    }
}

#[cfg(test)]
mod tests;
