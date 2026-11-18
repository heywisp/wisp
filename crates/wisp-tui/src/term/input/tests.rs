use std::io::Write;

use crossterm::event::{KeyEventState, MediaKeyCode};

use crate::term::probe::{ProbePlan, ProbeReport};

use super::*;

fn press(code: CtKey, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

#[test]
fn keys_convert_with_their_modifiers_and_releases_are_dropped() {
    assert_eq!(
        convert(press(CtKey::Char('c'), KeyModifiers::CONTROL)),
        Some(Input::Key(Key::ctrl('c')))
    );
    assert_eq!(
        convert(press(CtKey::Enter, KeyModifiers::ALT)),
        Some(Input::Key(Key::alt(KeyCode::Enter)))
    );
    assert_eq!(
        convert(press(CtKey::Enter, KeyModifiers::SHIFT)),
        Some(Input::Key(Key::shift(KeyCode::Enter)))
    );
    assert_eq!(
        convert(press(CtKey::Media(MediaKeyCode::Play), KeyModifiers::NONE)),
        Some(Input::Key(Key::plain(KeyCode::Other)))
    );
    let release = Event::Key(KeyEvent {
        code: CtKey::Char('a'),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Release,
        state: KeyEventState::NONE,
    });
    assert_eq!(convert(release), None);
    for (modifiers, modified) in [
        (KeyModifiers::NONE, false),
        (KeyModifiers::CONTROL, true),
        (KeyModifiers::ALT, true),
        (KeyModifiers::SHIFT, true),
        (KeyModifiers::SUPER, false),
    ] {
        let Event::Mouse(event) = mouse(MouseEventKind::Down(MouseButton::Left)) else {
            unreachable!()
        };
        assert_eq!(
            convert(Event::Mouse(MouseEvent { modifiers, ..event })),
            Some(Input::Mouse(Mouse {
                kind: MouseKind::Press,
                column: 7,
                row: 3,
                modified,
            })),
            "{modifiers:?}"
        );
    }
}

fn mouse(kind: MouseEventKind) -> Event {
    Event::Mouse(MouseEvent {
        kind,
        column: 7,
        row: 3,
        modifiers: KeyModifiers::NONE,
    })
}

#[test]
fn a_right_press_converts_and_its_drag_and_release_are_dropped() {
    assert_eq!(
        convert(mouse(MouseEventKind::Down(MouseButton::Right))),
        Some(Input::Mouse(Mouse {
            kind: MouseKind::RightPress,
            column: 7,
            row: 3,
            modified: false
        }))
    );
    assert_eq!(convert(mouse(MouseEventKind::Up(MouseButton::Right))), None);
    assert_eq!(
        convert(mouse(MouseEventKind::Drag(MouseButton::Right))),
        None
    );
    assert_eq!(
        convert(mouse(MouseEventKind::Down(MouseButton::Middle))),
        None
    );
    assert_eq!(
        convert(mouse(MouseEventKind::Down(MouseButton::Left))).map(|i| match i {
            Input::Mouse(m) => Some(m.kind),
            _ => None,
        }),
        Some(Some(MouseKind::Press))
    );
}

#[test]
fn pastes_resizes_and_focus_convert() {
    assert_eq!(
        convert(Event::Paste("a\nb".to_owned())),
        Some(Input::Paste("a\nb".to_owned()))
    );
    assert_eq!(
        convert(Event::Resize(100, 30)),
        Some(Input::Resize(Size {
            width: 100,
            height: 30
        }))
    );
    assert_eq!(convert(Event::FocusLost), Some(Input::Focus(false)));
}

#[test]
fn typed_bytes_replay_as_text_and_drop_control_sequences() {
    let got = typed(b"hi\x1b[Ax\x01\ty\xc3\xa9");
    let want: Vec<Input> = ['h', 'i', 'x']
        .into_iter()
        .map(|c| Input::Key(Key::plain(KeyCode::Char(c))))
        .chain([Input::Key(Key::plain(KeyCode::Tab))])
        .chain(
            ['y', '\u{e9}']
                .into_iter()
                .map(|c| Input::Key(Key::plain(KeyCode::Char(c)))),
        )
        .collect();
    assert_eq!(got, want);
}

#[test]
fn raw_key_bytes_give_the_input_crossterms_event_gives() {
    let plain = |code| press(code, KeyModifiers::NONE);
    let ctrl = |c| press(CtKey::Char(c), KeyModifiers::CONTROL);
    let table: [(&[u8], Event); 31] = [
        (b"\r", plain(CtKey::Enter)),
        (b"\t", plain(CtKey::Tab)),
        (b"\x7f", plain(CtKey::Backspace)),
        (b"\x08", ctrl('h')),
        (b"\n", ctrl('j')),
        (b"\x03", ctrl('c')),
        (b"\x00", ctrl(' ')),
        (b"\x1c", ctrl('4')),
        (b"\x1b", plain(CtKey::Esc)),
        (b"\x1b\x1b", plain(CtKey::Esc)),
        (b"\x1bx", press(CtKey::Char('x'), KeyModifiers::ALT)),
        (b"\x1b\r", press(CtKey::Enter, KeyModifiers::ALT)),
        (b"A", press(CtKey::Char('A'), KeyModifiers::SHIFT)),
        (b"\xc3\xa9", plain(CtKey::Char('\u{e9}'))),
        (b"\x1b[A", plain(CtKey::Up)),
        (b"\x1b[B", plain(CtKey::Down)),
        (b"\x1b[C", plain(CtKey::Right)),
        (b"\x1b[D", plain(CtKey::Left)),
        (b"\x1b[H", plain(CtKey::Home)),
        (b"\x1b[F", plain(CtKey::End)),
        (b"\x1b[Z", press(CtKey::BackTab, KeyModifiers::SHIFT)),
        (b"\x1bOA", plain(CtKey::Up)),
        (b"\x1bOH", plain(CtKey::Home)),
        (b"\x1bOP", plain(CtKey::F(1))),
        (b"\x1b[1~", plain(CtKey::Home)),
        (b"\x1b[2~", plain(CtKey::Insert)),
        (b"\x1b[3~", plain(CtKey::Delete)),
        (b"\x1b[4~", plain(CtKey::End)),
        (b"\x1b[5~", plain(CtKey::PageUp)),
        (b"\x1b[6~", plain(CtKey::PageDown)),
        (b"\x1b[24~", plain(CtKey::F(12))),
    ];
    for (bytes, event) in table {
        assert_eq!(
            keys(bytes, End::Final),
            (convert(event).into_iter().collect(), bytes.len()),
            "{bytes:?}"
        );
    }
    let Event::Mouse(down) = mouse(MouseEventKind::ScrollDown) else {
        panic!("not a mouse event")
    };
    let clicks: [(&[u8], Event); 8] = [
        (
            b"\x1b[<0;8;4M",
            mouse(MouseEventKind::Down(MouseButton::Left)),
        ),
        (
            b"\x1b[<0;8;4m",
            mouse(MouseEventKind::Up(MouseButton::Left)),
        ),
        (
            b"\x1b[<32;8;4M",
            mouse(MouseEventKind::Drag(MouseButton::Left)),
        ),
        (
            b"\x1b[<2;8;4M",
            mouse(MouseEventKind::Down(MouseButton::Right)),
        ),
        (b"\x1b[<64;8;4M", mouse(MouseEventKind::ScrollUp)),
        (
            b"\x1b[<69;8;4M",
            Event::Mouse(MouseEvent {
                modifiers: KeyModifiers::SHIFT,
                ..down
            }),
        ),
        (b"\x1b[I", Event::FocusGained),
        (b"\x1b[O", Event::FocusLost),
    ];
    for (bytes, event) in clicks {
        assert_eq!(
            keys(bytes, End::Final),
            (convert(event).into_iter().collect(), bytes.len()),
            "{bytes:?}"
        );
    }
    let dropped: &[u8] =
        b"\x1b[M !!\x1b[<1;3;4M\x1b[<35;3;4M\x1b[1;5C\x1b[15~\x1b]11;x\x07\x1bPq\x1b\\";
    assert_eq!(keys(dropped, End::More), (vec![], dropped.len()));
}

#[test]
fn a_paste_is_one_input_and_split_bytes_wait_for_the_rest() {
    let paste = b"\x1b[200~one\r\ntwo\x1b[A\x1b[201~";
    assert_eq!(
        keys(paste, End::More),
        (
            vec![Input::Paste("one\r\ntwo\x1b[A".to_owned())],
            paste.len()
        )
    );
    for cut in 2..paste.len() {
        assert_eq!(keys(&paste[..cut], End::More), (vec![], 0), "{cut}");
    }
    let enter = Input::Key(Key::plain(KeyCode::Enter));
    for split in [&b"\r\x1b[1"[..], b"\r\x1bO", b"\r\xc3", b"\r\x1b]11;x\x1b"] {
        assert_eq!(
            keys(split, End::More),
            (vec![enter.clone()], 1),
            "{split:?}"
        );
    }
    assert_eq!(
        keys(b"\x1b[200~ab", End::Final),
        (vec![Input::Paste("ab".to_owned())], 8)
    );
    assert_eq!(
        keys(b"\x1b", End::More),
        (vec![Input::Key(Key::plain(KeyCode::Esc))], 1)
    );
}

fn settled(parts: &[(u64, &[u8])]) -> Vec<Input> {
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    let (winch, _) = UnixStream::pair().unwrap();
    let mut watch = Watch {
        tty: OwnedFd::from(ours),
        winch,
        sender: None,
        registered: None,
        pending: false,
    };
    let plan = ProbePlan {
        deadline: Duration::ZERO,
        sync_query: false,
        scroll_query: false,
        width_test: false,
        pictures: false,
    };
    let start = Instant::now();
    let grace_ends_soon = start.checked_sub(Duration::from_millis(800)).unwrap();
    let mut late = LateReplies::after(&ProbeReport::new(plan), grace_ends_soon);
    let parts: Vec<(u64, Vec<u8>)> = parts.iter().map(|(at, b)| (*at, b.to_vec())).collect();
    let writer = std::thread::spawn(move || {
        for (at, bytes) in parts {
            std::thread::sleep(
                (start + Duration::from_millis(at)).saturating_duration_since(Instant::now()),
            );
            theirs.write_all(&bytes).unwrap();
        }
        theirs
    });
    let sent = Mutex::new(vec![]);
    drop(watch.settle(&control(), &mut late, &|input| {
        sent.lock().unwrap().push(input);
        true
    }));
    drop(writer.join());
    sent.into_inner().unwrap()
}

#[test]
fn the_grace_drain_keeps_read_boundaries_and_finishes_an_open_paste() {
    let esc = Input::Key(Key::plain(KeyCode::Esc));
    let x = Input::Key(Key::plain(KeyCode::Char('x')));
    assert_eq!(settled(&[(20, b"\x1b"), (80, b"x")]), [esc.clone(), x]);
    assert_eq!(
        settled(&[(20, b"\x1bx")]),
        [Input::Key(Key::alt(KeyCode::Char('x')))]
    );
    assert_eq!(
        settled(&[(20, b"\x1b"), (80, b"\x1b[A")]),
        [esc, Input::Key(Key::plain(KeyCode::Up))]
    );
    assert_eq!(
        settled(&[(20, b"\x1b[200~hello"), (300, b"\rworld\x1b[201~")]),
        [Input::Paste("hello\rworld".to_owned())]
    );
}

#[test]
fn typed_bytes_drop_ss3_finals_and_string_payloads() {
    let got = typed(b"a\x1bOPb\x1b]11;rgb:0/0/0\x07c\x1b]10;x\x1b\\d\x1bPq\x1b\\e\x1b[1;5Cf\x1bxg");
    let want: Vec<Input> = ['a', 'b', 'c', 'd', 'e', 'f', 'g']
        .into_iter()
        .map(|c| Input::Key(Key::plain(KeyCode::Char(c))))
        .collect();
    assert_eq!(got, want);
}

fn control() -> Shared {
    Arc::new((
        Mutex::new(Control {
            want: Want::Run,
            paused: false,
            finished: false,
        }),
        Condvar::new(),
    ))
}

#[test]
fn a_failing_or_panicking_reader_reports_that_input_closed() {
    let sent = Mutex::new(vec![]);
    let send = |input: Input| {
        sent.lock().unwrap().push(input);
        true
    };
    let mut calls = 0;
    read_loop(
        &control(),
        || {
            calls += 1;
            if calls == 1 {
                Ok(Some(Event::Paste("x".to_owned())))
            } else {
                Err(io::Error::other("tty gone"))
            }
        },
        &send,
    );
    assert_eq!(
        *sent.lock().unwrap(),
        [
            Input::Paste("x".to_owned()),
            Input::Closed("tty gone".to_owned())
        ]
    );
    type Next = fn() -> io::Result<Option<Event>>;
    let panics: [(Next, &str); 3] = [
        (
            || panic!("reader blew up"),
            "the input reader panicked: reader blew up",
        ),
        (
            || std::panic::panic_any(String::from("owned text")),
            "the input reader panicked: owned text",
        ),
        (|| std::panic::panic_any(7_u8), "the input reader panicked"),
    ];
    for (next, closed) in panics {
        sent.lock().unwrap().clear();
        read_loop(&control(), next, &send);
        assert_eq!(*sent.lock().unwrap(), [Input::Closed(closed.to_owned())]);
    }
}

#[test]
fn a_stopped_reader_does_not_report_closing() {
    let shared = control();
    lock(&shared.0).want = Want::Stop;
    let sent = Mutex::new(vec![]);
    read_loop(&shared, || Ok(None), &|input| {
        sent.lock().unwrap().push(input);
        true
    });
    assert!(sent.lock().unwrap().is_empty());
}

#[test]
fn a_tty_whose_other_end_closed_reads_as_hung_up() {
    use rustix::fs::{Mode, OFlags};
    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC).unwrap();
    grantpt(&master).unwrap();
    unlockpt(&master).unwrap();
    let name = ptsname(&master, Vec::new()).unwrap();
    let slave = rustix::fs::open(
        name.as_c_str(),
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .unwrap();
    check_hangup(&slave).unwrap();
    rustix::io::write(&master, b"x").unwrap();
    check_hangup(&slave).unwrap();
    drop(master);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let error = loop {
        match check_hangup(&slave) {
            Err(error) => break error,
            Ok(()) => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the hangup never showed after the master closed"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    };
    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
}

#[test]
fn a_kitty_error_reply_is_a_refusal_and_is_never_typed() {
    let refusal = b"\x1b_Gi=1073741824;EINVAL:bad\x1b\\";
    assert_eq!(keys(refusal, End::Final).0, [Input::PictureRefused]);
    assert_eq!(keys(b"\x1b_Gi=31;OK\x1b\\", End::Final).0, []);
    let alt = KeyModifiers::ALT;
    let reply = |said: &str, gap: usize| {
        let mut events = vec![
            press(CtKey::Char('_'), alt),
            press(CtKey::Char('G'), KeyModifiers::SHIFT),
        ];
        let typed = format!("i=1073741824;{said}");
        events.extend(
            typed
                .chars()
                .map(|c| press(CtKey::Char(c), KeyModifiers::NONE)),
        );
        events.push(press(CtKey::Char('\\'), alt));
        let mut apc = Apc::default();
        let mut out = Vec::new();
        for (i, e) in events.into_iter().enumerate() {
            out.extend(apc.step(Some(e)));
            if i == gap {
                out.extend(apc.step(None));
            }
        }
        out
    };
    assert_eq!(reply("ENOENT:gone", usize::MAX), [Input::PictureRefused]);
    assert_eq!(reply("EINVAL:split", 6), [Input::PictureRefused]);
    assert_eq!(reply("OK", usize::MAX), []);
    let mut apc = Apc::default();
    let underscore = press(CtKey::Char('_'), alt);
    assert_eq!(apc.step(Some(underscore.clone())), []);
    assert_eq!(apc.step(None), [convert(underscore.clone()).unwrap()]);
    apc.step(Some(underscore.clone()));
    let x = press(CtKey::Char('x'), KeyModifiers::NONE);
    let both: Vec<Input> = [underscore, x].into_iter().filter_map(convert).collect();
    assert_eq!(
        apc.step(Some(press(CtKey::Char('x'), KeyModifiers::NONE))),
        both
    );
    apc.step(Some(press(CtKey::Char('_'), alt)));
    apc.step(Some(press(CtKey::Char('G'), KeyModifiers::SHIFT)));
    let resize = Event::Resize(100, 30);
    assert_eq!(apc.step(Some(resize.clone())), [convert(resize).unwrap()]);
    let x = press(CtKey::Char('x'), KeyModifiers::NONE);
    assert_eq!(apc.step(Some(x.clone())), [convert(x).unwrap()]);
}
