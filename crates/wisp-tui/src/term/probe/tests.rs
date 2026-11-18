use std::collections::VecDeque;

use super::*;

const PLAN: ProbePlan = ProbePlan {
    deadline: Duration::from_millis(250),
    sync_query: true,
    scroll_query: true,
    width_test: true,
    pictures: true,
};

const OSC10: &[u8] = b"\x1b]10;rgb:d0d0/d0d0/d0d0\x1b\\";
const OSC11: &[u8] = b"\x1b]11;rgb:0c0c/0c0c/0c0c\x07";
const DECRPM: &[u8] = b"\x1b[?2026;2$y";
const KITTY: &[u8] = b"\x1b[?0u";
const CPR_ORIGIN: &[u8] = b"\x1b[5;1R";
const CPR_AFTER: &[u8] = b"\x1b[5;2R";
const DA1: &[u8] = b"\x1b[?62;22c";
const GRAPHICS: &[u8] = b"\x1b_Gi=31;OK\x1b\\";
const CELLS: &[u8] = b"\x1b[6;19;9t";

struct Script {
    sent: Vec<u8>,
    chunks: VecDeque<Vec<u8>>,
    waited: Duration,
}

impl Script {
    fn new(chunks: Vec<Vec<u8>>) -> Script {
        Script {
            sent: vec![],
            chunks: chunks.into(),
            waited: Duration::ZERO,
        }
    }
}

impl ProbeIo for Script {
    fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.sent.extend_from_slice(bytes);
        Ok(())
    }

    fn receive(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        match self.chunks.pop_front() {
            Some(chunk) => {
                let n = chunk.len().min(buf.len());
                buf[..n].copy_from_slice(&chunk[..n]);
                if n < chunk.len() {
                    self.chunks.push_front(chunk[n..].to_vec());
                }
                Ok(n)
            }
            None => {
                let nap = timeout.min(Duration::from_millis(5));
                std::thread::sleep(nap);
                self.waited += nap;
                Ok(0)
            }
        }
    }
}

const SCROLL_SET: &[u8] = b"\x1b[?1007;1$y";

fn all_replies() -> Vec<u8> {
    [
        OSC10, OSC11, DECRPM, SCROLL_SET, KITTY, CPR_ORIGIN, CPR_AFTER, GRAPHICS, CELLS, DA1,
    ]
    .concat()
}

fn check_full(r: &ProbeReport) {
    assert_eq!(r.outcome, Outcome::Answered);
    assert_eq!(r.foreground, Some(Rgb::hex(0xD0D0D0)));
    assert_eq!(r.background, Some(Rgb::hex(0x0C0C0C)));
    assert_eq!(r.synchronized_output, SyncSupport::Supported);
    assert_eq!(r.kitty_keyboard, Some(0));
    assert_eq!(r.origin, Some(Position { row: 5, col: 1 }));
    assert_eq!(r.dot_width, Some(1));
    assert_eq!(r.device_attributes, Some(vec![62, 22]));
    assert_eq!(r.graphics, Some(true));
    assert_eq!(r.cell_pixels, Some((9, 19)));
}

#[test]
fn the_query_is_one_write_with_da1_last() {
    let q = String::from_utf8(PLAN.query()).unwrap();
    assert!(q.starts_with("\x1b]10;?\x1b\\\x1b]11;?\x1b\\"), "{q:?}");
    assert!(q.contains("\x1b[?2026$p"));
    assert!(q.contains("\x1b[?u"));
    assert!(q.ends_with("\x1b[c"));
    assert_eq!(q.matches("\x1b[6n").count(), 2);
    assert!(q.contains("\x1b7\u{b7}\x1b[6n\x1b8"));
    assert!(q.contains("\x1b[?1007$p"));
    assert!(!q.contains("1004"), "{q:?}");
    assert!(
        q.ends_with("\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[16t\x1b[c"),
        "{q:?}"
    );
    let apple = ProbePlan {
        sync_query: false,
        scroll_query: false,
        width_test: false,
        pictures: false,
        ..PLAN
    };
    let q = String::from_utf8(apple.query()).unwrap();
    assert!(!q.contains("2026"), "{q:?}");
    assert!(!q.contains("1007"), "{q:?}");
    assert!(!q.contains("1004"), "{q:?}");
    assert!(!q.contains('\u{b7}'));
    assert!(!q.contains("\x1b_") && !q.contains("16t"), "{q:?}");
    assert_eq!(q.matches("\x1b[6n").count(), 1);
    assert!(q.ends_with("\x1b[c"));

    let mut io = Script::new(vec![all_replies()]);
    run(&mut io, PLAN).unwrap();
    assert_eq!(io.sent, PLAN.query());
}

#[test]
fn every_reply_in_one_chunk() {
    let mut io = Script::new(vec![all_replies()]);
    let r = run(&mut io, PLAN).unwrap();
    check_full(&r);
    assert!(r.typed.is_empty());
    assert_eq!(r.dropped, 0);
}

#[test]
fn replies_split_at_every_byte() {
    let chunks = all_replies().into_iter().map(|b| vec![b]).collect();
    let mut io = Script::new(chunks);
    check_full(&run(&mut io, PLAN).unwrap());
}

#[test]
fn replies_out_of_order() {
    let shuffled = [
        CELLS, KITTY, OSC11, CPR_ORIGIN, GRAPHICS, DECRPM, CPR_AFTER, OSC10, DA1,
    ]
    .concat();
    let mut io = Script::new(vec![shuffled]);
    check_full(&run(&mut io, PLAN).unwrap());
}

#[test]
fn keys_typed_during_the_probe_are_kept_for_replay() {
    let bytes = [
        b"he".as_slice(),
        OSC10,
        b"\x1b[A",
        OSC11,
        b"l\x1bx",
        DECRPM,
        KITTY,
        b"\r",
        CPR_ORIGIN,
        CPR_AFTER,
        GRAPHICS,
        b"\x1bOP",
        CELLS,
        DA1,
        b"lo",
    ]
    .concat();
    let mut io = Script::new(vec![bytes]);
    let r = run(&mut io, PLAN).unwrap();
    check_full(&r);
    assert_eq!(r.typed, b"he\x1b[Al\x1bx\r\x1bOPlo");
}

#[test]
fn garbage_and_partial_replies_never_hang_or_leak() {
    let bytes = [
        b"\x1b]11;rgb:zz/zz/zz\x1b\\".as_slice(),
        b"\x1b]4;1;rgb:ff/00/00\x07",
        b"\x1b[?2027;0$y",
        b"\x1b[?9999999u",
        b"\x1b]11;rgb:0c0c/0c0c\x1b[q",
        b"\x1b[\x01",
        b"\x1b_Gi=31\x1b\\\x1b_Xjunk\x07",
        OSC11,
        DA1,
    ]
    .concat();
    let mut io = Script::new(vec![bytes]);
    let r = run(&mut io, PLAN).unwrap();
    assert_eq!(r.outcome, Outcome::Answered);
    assert_eq!(r.background, Some(Rgb::hex(0x0C0C0C)));
    assert_eq!(r.foreground, None);
    assert_eq!(r.synchronized_output, SyncSupport::NoReply);
    assert_eq!(r.kitty_keyboard, None);
    assert_eq!(r.graphics, None);
    assert!(r.dropped > 0);
    let mut io = Script::new(vec![
        [b"\x1b_Gi=31;EINVAL:bad\x1b\\".as_slice(), DA1].concat(),
    ]);
    assert_eq!(run(&mut io, PLAN).unwrap().graphics, Some(false));
    assert_eq!(r.typed, b"\x1b[q\x1b[\x01");
}

#[test]
fn the_deadline_ends_a_silent_probe() {
    let plan = ProbePlan {
        deadline: Duration::from_millis(40),
        ..PLAN
    };
    let mut io = Script::new(vec![]);
    let r = run(&mut io, plan).unwrap();
    assert_eq!(r.outcome, Outcome::TimedOut);
    assert!(r.elapsed >= plan.deadline, "{:?}", r.elapsed);
    assert!(io.waited <= plan.deadline + Duration::from_millis(5));
    assert_eq!(r.synchronized_output, SyncSupport::NoReply);
    assert_eq!(r.background, None);
}

#[test]
fn a_partial_reply_at_the_deadline_is_dropped_and_keys_kept() {
    let plan = ProbePlan {
        deadline: Duration::from_millis(30),
        ..PLAN
    };
    let mut io = Script::new(vec![[OSC10, b"x", b"\x1b]11;rgb:0c"].concat()]);
    let r = run(&mut io, plan).unwrap();
    assert_eq!(r.outcome, Outcome::TimedOut);
    assert_eq!(r.foreground, Some(Rgb::hex(0xD0D0D0)));
    assert_eq!(r.background, None);
    assert_eq!(r.typed, b"x");
    assert_eq!(r.unfinished, b"\x1b]11;rgb:0c");

    let mut io = Script::new(vec![b"ab\x1b".to_vec()]);
    let r = run(&mut io, plan).unwrap();
    assert_eq!(r.typed, b"ab");
    assert_eq!(r.unfinished, b"\x1b");
    let t0 = Instant::now();
    let mut late = LateReplies::after(&r, t0);
    assert_eq!(late.filter(b"[A", t0), b"\x1b[A");
}

#[test]
fn a_terminal_without_2026_or_kitty() {
    let bytes = [OSC11, b"\x1b[?2026;0$y", CPR_ORIGIN, b"\x1b[5;3R", DA1].concat();
    let mut io = Script::new(vec![bytes]);
    let r = run(&mut io, PLAN).unwrap();
    assert_eq!(r.synchronized_output, SyncSupport::Unsupported);
    assert_eq!(r.kitty_keyboard, None);
    assert_eq!(r.dot_width, Some(2));
    let mut io = Script::new(vec![[b"\x1b[?2026;4$y".as_slice(), DA1].concat()]);
    assert_eq!(
        run(&mut io, PLAN).unwrap().synchronized_output,
        SyncSupport::Unsupported
    );
}

#[test]
fn colour_formats() {
    let cases = [
        ("rgb:ffff/0000/8080", Some(Rgb::hex(0xFF0080))),
        ("rgb:ff/00/80", Some(Rgb::hex(0xFF0080))),
        ("rgb:f/0/8", Some(Rgb::hex(0xFF0088))),
        ("rgba:0c0c/0c0c/0c0c/ffff", Some(Rgb::hex(0x0C0C0C))),
        ("#0C0C0C", Some(Rgb::hex(0x0C0C0C))),
        ("rgb:fffff/0/0", None),
        ("rgb:ff/00", None),
        ("rgb://", None),
        ("#12345", None),
        ("blue", None),
    ];
    for (text, want) in cases {
        assert_eq!(parse_color(text), want, "{text}");
    }
}

fn timed_out() -> ProbeReport {
    let mut io = Script::new(vec![]);
    run(
        &mut io,
        ProbePlan {
            deadline: Duration::from_millis(1),
            ..PLAN
        },
    )
    .unwrap()
}

#[test]
fn late_replies_are_stripped_and_keys_pass_in_order() {
    let t0 = Instant::now();
    let mut late = LateReplies::after(&timed_out(), t0);
    let stream = [
        b"a".as_slice(),
        b"\x1b]11;rgb:0c0c/0c0c/0c0c\x1b\\",
        b"b\x1b[A",
        b"\x1b[?2026;2$y\x1b[?1u",
        b"\x1b[5;1R",
        b"c",
        b"\x1b]10;rgb:d0/d0/d0\x07",
        CELLS,
        GRAPHICS,
        b"\x1b_x",
    ]
    .concat();
    let mut out = vec![];
    for chunk in stream.chunks(3) {
        out.extend(late.filter(chunk, t0));
    }
    assert_eq!(out, b"ab\x1b[Ac\x1b_x");
    assert!(late.active(t0));
    let mut tail = late.filter(b"d\x1b[?62;22ce\x1b]11;rgb:1/2/3\x07", t0);
    assert!(!late.active(t0));
    tail.extend(late.filter(b"\x1b]11;x\x07", t0));
    assert_eq!(tail, b"de\x1b]11;rgb:1/2/3\x07\x1b]11;x\x07");
}

#[test]
fn late_filtering_ends_with_the_grace_window() {
    let t0 = Instant::now();
    let mut late = LateReplies::after(&timed_out(), t0);
    assert_eq!(late.filter(b"x\x1b", t0), b"x");
    let later = t0 + LATE_GRACE;
    assert!(!late.active(later));
    assert_eq!(late.filter(b"\x1b[5;1Ry", later), b"\x1b\x1b[5;1Ry");

    let answered = {
        let mut io = Script::new(vec![all_replies()]);
        run(&mut io, PLAN).unwrap()
    };
    let mut none = LateReplies::after(&answered, t0);
    assert!(!none.active(t0));
    assert_eq!(none.filter(b"\x1b[5;1R", t0), b"\x1b[5;1R");
}

fn late_run(report: &ProbeReport, input: &[&[u8]]) -> Vec<u8> {
    let t0 = Instant::now();
    let mut late = LateReplies::after(report, t0);
    let mut out = vec![];
    for chunk in input {
        out.extend(late.filter(chunk, t0));
    }
    out.extend(late.close());
    out
}

#[test]
fn late_filtering_never_eats_real_keys() {
    let owed = timed_out();
    let cases: [(&[&[u8]], &[u8]); 7] = [
        (&[b"\x1b]hello"], b"\x1b]hello"),
        (&[b"\x1b]hello\x1b[Aworld"], b"\x1b]hello\x1b[Aworld"),
        (&[b"\x1b]hi\x07there"], b"\x1b]hi\x07there"),
        (
            &[b"\x1b]1", b"0;not a colour\x07!"],
            b"\x1b]10;not a colour\x07!",
        ),
        (&[b"\x1b]11;rgb:0c0c/0c"], b"\x1b]11;rgb:0c0c/0c"),
        (
            &[b"paste: rgb:0c0c/0c0c/0c0c ]11; ok"],
            b"paste: rgb:0c0c/0c0c/0c0c ]11; ok",
        ),
        (
            &[
                b"\x1b[200~pasted \x1b]11;rgb:0c0c/0c0c/0c0c\x07 \x1b[1;2R",
                b" text\x1b[20",
                b"1~after",
            ],
            b"\x1b[200~pasted \x1b]11;rgb:0c0c/0c0c/0c0c\x07 \x1b[1;2R text\x1b[201~after",
        ),
    ];
    for (input, want) in cases {
        let out = late_run(&owed, input);
        assert_eq!(
            String::from_utf8_lossy(&out),
            String::from_utf8_lossy(want),
            "{input:?}"
        );
    }
}

#[test]
fn late_filtering_swallows_only_replies_still_owed() {
    let mut partial = timed_out();
    partial.origin = Some(Position { row: 1, col: 1 });
    partial.dot_width = Some(1);
    partial.background = Some(Rgb::hex(0x0C0C0C));
    let out = late_run(
        &partial,
        &[b"\x1b[1;2R\x1b]11;rgb:0c0c/0c0c/0c0c\x07\x1b]10;rgb:d0/d0/d0\x07\x1b[1;2R"],
    );
    assert_eq!(
        String::from_utf8_lossy(&out),
        "\x1b[1;2R\x1b]11;rgb:0c0c/0c0c/0c0c\x07\x1b[1;2R"
    );

    let owed = timed_out();
    let out = late_run(
        &owed,
        &[&[
            b"\x1b[5;1R\x1b[5;2R\x1b[1;2R".as_slice(),
            GRAPHICS,
            CELLS,
            CELLS,
            b"\x1b[?62c\x1b[?62c",
        ]
        .concat()],
    );
    assert_eq!(
        String::from_utf8_lossy(&out),
        "\x1b[1;2R\x1b[6;19;9t\x1b[?62c"
    );
    assert_eq!(
        Owed::by(&owed),
        Owed {
            foreground: 1,
            background: 1,
            mode_report: 1,
            scroll_report: 1,
            kitty: 1,
            cursor_positions: 2,
            device_attributes: 1,
            graphics: 1,
            cell_pixels: 1,
        }
    );
}

#[test]
fn a_reply_split_across_the_deadline_is_recognised_whole() {
    let plan = ProbePlan {
        deadline: Duration::from_millis(30),
        ..PLAN
    };
    let splits: [(&[u8], &[u8]); 5] = [
        (b"\x1b_Gi=31;O", b"K\x1b\\k"),
        (b"\x1b[6;1", b"9;9tk"),
        (b"\x1b]11;rgb:0c0c/0c", b"0c/0c0c\x1b\\k"),
        (b"\x1b[?62;2", b"2ck"),
        (b"\x1b[5;", b"1Rk"),
    ];
    for (head, tail) in splits {
        let mut io = Script::new(vec![head.to_vec()]);
        let report = run(&mut io, plan).unwrap();
        assert_eq!(report.outcome, Outcome::TimedOut);
        assert!(report.typed.is_empty(), "{head:?}");
        assert_eq!(report.dropped, 0, "{head:?}");
        assert_eq!(report.unfinished, head);
        let t0 = Instant::now();
        let mut late = LateReplies::after(&report, t0);
        let mut out = late.filter(tail, t0);
        out.extend(late.close());
        assert_eq!(out, b"k", "{head:?} + {tail:?}");
    }
}

#[test]
fn the_alternate_scroll_mode_report_is_kept() {
    let mut io = Script::new(vec![all_replies()]);
    let r = run(&mut io, PLAN).unwrap();
    assert_eq!(r.alternate_scroll, Some(1));
    let mut io = Script::new(vec![
        [OSC10, OSC11, DECRPM, KITTY, CPR_ORIGIN, CPR_AFTER, DA1].concat(),
    ]);
    let r = run(&mut io, PLAN).unwrap();
    assert_eq!(r.alternate_scroll, None);
}
