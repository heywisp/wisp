use std::num::NonZeroU16;

use super::*;

fn size(cols: u16, rows: u16) -> TermSize {
    TermSize { cols, rows }
}

fn client(rows: u16, cols: u16) -> vt100::Parser {
    vt100::Parser::new(
        NonZeroU16::new(rows).unwrap(),
        NonZeroU16::new(cols).unwrap(),
        0,
    )
}

fn client_view(parser: &vt100::Parser) -> (Vec<String>, (u16, u16), bool, bool, bool) {
    let screen = parser.screen();
    let (_, cols) = screen.size();
    let mut rows: Vec<String> = screen
        .rows(0, cols.get())
        .map(|r| r.trim_end().to_owned())
        .collect();
    while rows.last().is_some_and(String::is_empty) {
        rows.pop();
    }
    (
        rows,
        screen.cursor_position(),
        screen.alternate_screen(),
        screen.application_cursor(),
        screen.bracketed_paste(),
    )
}

fn kernel_view(screen: &Screen) -> (Vec<String>, (u16, u16), bool, bool, bool) {
    let view = screen.view();
    let modes = screen.parser.screen();
    (
        view.rows,
        view.cursor,
        view.alternate,
        modes.application_cursor(),
        modes.bracketed_paste(),
    )
}

#[test]
fn the_terminal_queries_are_answered() {
    let mut screen = Screen::new(size(40, 10));
    assert_eq!(screen.feed(b"abc\r\nde\x1b[6n"), [b"\x1b[2;3R"]);
    assert_eq!(screen.feed(b"\x1b[5n"), [b"\x1b[0n"]);
    assert_eq!(screen.feed(b"\x1b[c"), [b"\x1b[?62;22c"]);
    assert_eq!(screen.feed(b"\x1b[0c"), [b"\x1b[?62;22c"]);
    assert_eq!(
        screen.feed(b"\x1b[5nx\x1b[6n\x1b[5n"),
        [
            b"\x1b[0n".to_vec(),
            b"\x1b[2;4R".to_vec(),
            b"\x1b[0n".to_vec()
        ]
    );
    assert!(screen.feed(b"\x1b[>c\x1b[?6n\x1b[18t").is_empty());
}

#[test]
fn the_view_trims_rows_and_drops_trailing_blank_rows() {
    let mut screen = Screen::new(size(20, 5));
    screen.feed(b"one   \r\n\r\nthree\x1b[2;4H");
    let view = screen.view();
    assert_eq!(view.rows, ["one", "", "three"]);
    assert_eq!(view.cursor, (1, 3));
    assert_eq!(view.size, size(20, 5));
    assert!(!view.alternate);
    screen.feed(b"\x1b[?1049h\x1b[Hfull");
    let view = screen.view();
    assert_eq!(view.rows, ["full"]);
    assert!(view.alternate);
    screen.feed(b"\x1b[?1049l");
    assert_eq!(screen.view().rows, ["one", "", "three"]);
    assert!(Screen::new(size(20, 5)).view().rows.is_empty());
}

#[test]
fn a_resize_changes_the_size_and_keeps_the_text() {
    let mut screen = Screen::new(size(20, 5));
    screen.feed(b"kept");
    screen.resize(size(30, 8));
    let view = screen.view();
    assert_eq!(view.size, size(30, 8));
    assert_eq!(view.rows, ["kept"]);
}

#[test]
fn a_repaint_rebuilds_the_screen_on_a_fresh_or_stale_client() {
    let scenes: [&[u8]; 3] = [
        b"plain text\r\nsecond\x1b[1;3H",
        b"before\x1b[?1049h\x1b[2J\x1b[3;5Halt\x1b[?1h\x1b[?2004h",
        b"\x1b[31mred\x1b[0m line\r\n\x1b[?1049h\x1b[?1049l\x1b[2;2H",
    ];
    for scene in scenes {
        let mut screen = Screen::new(size(30, 6));
        screen.feed(scene);
        let repaint = screen.repaint();
        let mut fresh = client(6, 30);
        fresh.process(&repaint);
        assert_eq!(client_view(&fresh), kernel_view(&screen));
        let mut stale = client(6, 30);
        stale.process(b"\x1b[?1049hjunk\x1b[?1h\x1b[?2004h\x1b[5;5Hmore");
        stale.process(&repaint);
        assert_eq!(client_view(&stale), kernel_view(&screen));
    }
}

#[test]
fn a_program_leaving_the_alternate_screen_after_a_repaint_returns_the_client() {
    let mut screen = Screen::new(size(30, 6));
    screen.feed(b"\x1b[?1049h\x1b[Hfull screen");
    let mut client = client(6, 30);
    client.process(&screen.repaint());
    assert!(client.screen().alternate_screen());
    client.process(b"\x1b[?1049l");
    assert!(!client.screen().alternate_screen());
}

struct Recorded(std::sync::Arc<Mutex<Vec<Vec<u8>>>>);

impl TermPort for Recorded {
    fn resize(&self, _size: TermSize) -> io::Result<()> {
        Ok(())
    }

    fn write(&self, job: WriteJob) {
        self.0.lock().unwrap().push(job.bytes.clone());
        job.run(&mut Vec::new());
    }

    fn canonical(&self) -> Option<bool> {
        None
    }
}

#[test]
fn a_terminal_sends_replies_and_reports_an_unchanged_screen() {
    let sent = std::sync::Arc::new(Mutex::new(Vec::new()));
    let terminal = Terminal::new(size(20, 5), Box::new(Recorded(sent.clone())));
    terminal.feed(b"hi\x1b[6n");
    terminal.feed(b"no query");
    assert_eq!(*sent.lock().unwrap(), [b"\x1b[1;3R".to_vec()]);
    terminal.feed(b"\x1b[5n\x1b[5n");
    assert_eq!(
        sent.lock().unwrap().len(),
        3,
        "each answer is sent on its own"
    );
    let view = terminal.view();
    assert!(terminal.show(&view));
    assert!(!terminal.show(&terminal.view()));
    terminal.feed(b"!");
    assert!(terminal.show(&terminal.view()));
    terminal.resize(size(30, 6)).unwrap();
    assert_eq!(terminal.size(), size(30, 6));
    assert!(terminal.show(&terminal.view()));
}

#[test]
fn sizes_parse_and_are_bounded() {
    assert_eq!(parse_size("120x40"), Some(size(120, 40)));
    assert_eq!(parse_size(" 80X24 "), Some(size(80, 24)));
    assert_eq!(parse_size("80"), None);
    assert_eq!(parse_size("ax24"), None);
    assert!(size_allowed(size(20, 5)));
    assert!(size_allowed(size(500, 200)));
    assert!(!size_allowed(size(19, 5)));
    assert!(!size_allowed(size(20, 201)));
    assert_eq!(size_text(DEFAULT_TTY_SIZE), "100x30");
}

#[test]
fn a_pending_wrap_reports_the_last_column() {
    let mut screen = Screen::new(size(20, 5));
    let replies = screen.feed(b"abcdefghijklmnopqrst\x1b[6n");
    assert_eq!(replies, [b"\x1b[1;20R"]);
    assert_eq!(screen.view().cursor, (0, 19));
}

struct Moody {
    echoes: Vec<bool>,
    hidden: Vec<bool>,
    written: Vec<u8>,
}

impl Sink for Moody {
    fn put(&mut self, bytes: &[u8], _stop: &AtomicBool) -> io::Result<Option<usize>> {
        self.written.extend_from_slice(bytes);
        Ok(Some(bytes.len()))
    }

    fn echoes(&self) -> bool {
        let chunk = self.written.len() / WRITE_CHUNK;
        self.echoes.get(chunk).copied().unwrap_or(false)
    }

    fn hidden(&self) -> bool {
        let chunk = self.written.len() / WRITE_CHUNK;
        self.hidden.get(chunk).copied().unwrap_or(false)
    }
}

fn echoed_after(bytes: usize, echoes: &[bool]) -> bool {
    let (job, written) = WriteJob::new(vec![b'a'; bytes], false);
    let echo = written.progress();
    let mut out = Moody {
        echoes: echoes.to_vec(),
        hidden: Vec::new(),
        written: Vec::new(),
    };
    assert!(job.run(&mut out));
    assert_eq!(out.written.len(), bytes);
    assert_eq!(echo.done(), bytes);
    echo.echoed()
}

#[test]
fn a_write_counts_as_echoed_only_when_every_chunk_went_out_with_echo_on() {
    assert!(echoed_after(10, &[true]));
    assert!(echoed_after(WRITE_CHUNK * 2 + 1, &[true, true, true]));
    assert!(!echoed_after(WRITE_CHUNK * 2 + 1, &[true, false, true]));
    assert!(!echoed_after(WRITE_CHUNK + 1, &[false, true]));
    assert!(!echoed_after(0, &[true]));
    let (job, written) = WriteJob::new(b"abc".to_vec(), false);
    let echo = written.progress();
    assert!(job.run(&mut Vec::new()));
    assert!(!echo.echoed());
}

fn sent_when_hidden(bytes: usize, hidden: &[bool], echoes: &[bool]) -> usize {
    let (job, written) = WriteJob::new(vec![b'a'; bytes], true);
    let progress = written.progress();
    let mut out = Moody {
        echoes: echoes.to_vec(),
        hidden: hidden.to_vec(),
        written: Vec::new(),
    };
    assert!(job.run(&mut out));
    assert_eq!(progress.done(), out.written.len());
    out.written.len()
}

#[test]
fn a_hidden_write_sends_a_chunk_only_to_a_hidden_terminal() {
    let three = WRITE_CHUNK * 2 + 1;
    assert_eq!(sent_when_hidden(10, &[true], &[false]), 10);
    assert_eq!(sent_when_hidden(three, &[true; 3], &[false; 3]), three);
    assert_eq!(
        sent_when_hidden(three, &[true, false, true], &[false; 3]),
        WRITE_CHUNK
    );
    assert_eq!(
        sent_when_hidden(three, &[true, false, true], &[false, true, false]),
        WRITE_CHUNK
    );
    assert_eq!(sent_when_hidden(three, &[false, true, true], &[true; 3]), 0);
    assert_eq!(sent_when_hidden(10, &[false], &[false]), 0);
}
