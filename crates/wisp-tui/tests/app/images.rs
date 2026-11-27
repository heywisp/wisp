use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use wisp_protocol::{
    CacheEffect, ClientCommand, DeliveryMode, Dropped, ImageData, InputId, MissingImage, PointInfo,
    RejectKind, ReturnedInput, RewindPoint, RewindQuote, RewindReason, Warmth,
};
use wisp_tui::term::clipboard::reader::Setup;
use wisp_tui::term::clipboard::{Native, Tools};

use super::scratch::ScratchDir;
use super::steering::{attached, sent_id, steering_finished};
use super::*;

const WL_PASTE: &str = r#"#!/bin/sh
d="$(dirname "$0")"
if [ -t 0 ] || [ -t 1 ] || [ -t 2 ]; then echo tty >> "$d/tty"; fi
if [ -f "$d/hang" ]; then /bin/sleep 30; fi
if [ "$1" = "--list-types" ]; then printf 'text/plain\nimage/bmp\n'; exit 0; fi
/bin/cat "$d/image.bmp"
"#;

const MODEL: &str = "claude-sonnet-5-5";

pub(super) struct Dir {
    pub(super) path: ScratchDir,
}

impl Dir {
    pub(super) fn new(name: &str) -> Dir {
        Dir {
            path: ScratchDir::new(SCRATCH, name),
        }
    }

    pub(super) fn put(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.path.root.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn tool(&self, name: &str, script: &str) {
        fake_tool::install(&self.path.root.join(name), script);
    }

    fn setup(&self, natives: &[Native], timeout: Duration) -> Setup {
        Setup {
            ssh: false,
            wsl: false,
            natives: natives.to_vec(),
            tools: Tools {
                path: Some(self.path.root.clone().into_os_string()),
                powershell: vec![],
                setsid: None,
                timeout,
            },
            home: None,
        }
    }
}

fn bmp(width: u32, height: u32, seed: u8) -> Vec<u8> {
    let row = (width * 3).div_ceil(4) * 4;
    let pixels = row * height;
    let mut out = b"BM".to_vec();
    for word in [54 + pixels, 0, 54, 40, width, height] {
        out.extend(word.to_le_bytes());
    }
    out.extend(1u16.to_le_bytes());
    out.extend(24u16.to_le_bytes());
    for word in [0, pixels, 2835, 2835, 0, 0] {
        out.extend(word.to_le_bytes());
    }
    for i in 0..pixels {
        out.push(seed.wrapping_add(u8::try_from(i % 251).unwrap()));
    }
    out
}

fn image_facts(model: &str) -> SessionFacts {
    SessionFacts {
        model: model.to_owned(),
        ..facts()
    }
}

fn alt_v() -> Input {
    Input::Key(Key::alt(KeyCode::Char('v')))
}

async fn until(screen: &Screen, what: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let held = eventually(|| {
        lines = screen.visible();
        lines.iter().any(|l| l.contains(what))
    })
    .await;
    assert!(held, "no {what:?} in {lines:#?}");
    lines
}

async fn until_caret(screen: &Screen, what: &str) -> String {
    let mut line = String::new();
    let held = eventually(|| {
        line = screen.caret_line();
        line.contains(what)
    })
    .await;
    assert!(held, "no {what:?} at the caret: {line:?}");
    line
}

#[derive(Clone)]
struct Sent {
    screen: Screen,
    commands: Rc<RefCell<mpsc::Receiver<ClientCommand>>>,
    seen: Rc<RefCell<Vec<ClientCommand>>>,
}

impl Sent {
    async fn next(&self) -> ClientCommand {
        let mut sent = None;
        eventually(|| {
            sent = std::iter::from_fn(|| self.commands.borrow_mut().try_recv().ok())
                .find(|command| *command != ClientCommand::Cancel);
            sent.is_some()
        })
        .await;
        let command =
            sent.unwrap_or_else(|| panic!("nothing was sent: {:#?}", self.screen.visible()));
        self.seen.borrow_mut().push(command.clone());
        command
    }

    fn all(&self) -> Vec<ClientCommand> {
        let mut out = self.seen.borrow().clone();
        while let Ok(command) = self.commands.borrow_mut().try_recv() {
            if command != ClientCommand::Cancel {
                out.push(command);
            }
        }
        out
    }
}

async fn answered(run: &Run, events: &mpsc::Sender<AgentEvent>) {
    for event in answer(["Seen.".to_owned()]) {
        events.send(event).await.unwrap();
    }
    until(&run.screen, "cost unknown").await;
}

async fn with_commands<D, F>(
    screen: Screen,
    options: Options,
    facts: SessionFacts,
    drive: D,
) -> Vec<ClientCommand>
where
    D: FnOnce(Run, mpsc::Sender<AgentEvent>, Sent) -> F,
    F: Future<Output = ()>,
{
    let (commands, received_commands) = super::without_terminal(16);
    let sent = Sent {
        screen: screen.clone(),
        commands: Rc::new(RefCell::new(received_commands)),
        seen: Rc::default(),
    };
    let (events, received_events) = mpsc::channel(16);
    let (inputs, received) = mpsc::unbounded_channel();
    let mut console = screen.clone();
    let session = Session {
        leave_note: None,
        attached: None,
        alerts: Vec::new(),
        facts,
        events: received_events,
        commands,
        resumed: None,
    };
    let app = run_to_end(&mut console, received, session, options);
    let watched = sent.clone();
    let driver = async move {
        drive(
            Run {
                screen: screen.clone(),
                inputs,
            },
            events.clone(),
            watched,
        )
        .await;
        drop(events);
    };
    let (finished, ()) = tokio::join!(app, driver);
    assert!(finished.exit.is_ok(), "{:?}", finished.exit.err());
    sent.all()
}

fn quoted(path: &Path) -> String {
    format!("'{}'", path.display())
}

#[tokio::test]
async fn dropped_paths_attach_as_chips_and_are_sent_in_chip_order() {
    let dir = Dir::new("order");
    let first = bmp(10, 40, 1);
    let second = bmp(30, 20, 2);
    let first_path = dir.put("one.bmp", &first);
    let second_path = dir.put("my second.BMP", &second);
    let mut opts = options(Start::Stalled);
    opts.images = dir.setup(&[], Duration::from_secs(5));
    let screen = Screen::new(24, 220);
    let missing = dir.path.root.join("missing.png").display().to_string();
    let typed = missing.clone();
    let commands = with_commands(
        screen.clone(),
        opts,
        image_facts(MODEL),
        |run, events, sent| async move {
            run.send(Input::Paste(format!("{}\n", first_path.display())));
            until(
                &run.screen,
                "[Image #1 \u{b7} 10\u{d7}40 \u{b7} \u{2248}2 tokens]",
            )
            .await;
            run.send(Input::Paste(" against ".to_owned()));
            run.send(Input::Paste(quoted(&second_path)));
            until(
                &run.screen,
                "[Image #2 \u{b7} 30\u{d7}20 \u{b7} \u{2248}2 tokens]",
            )
            .await;
            run.send(Input::Paste(typed));
            until_caret(&run.screen, "missing.png").await;
            run.send(key(KeyCode::Enter));
            sent.next().await;
            for event in [
                AgentEvent::turn_started(1),
                AgentEvent::InputDelivered {
                    ids: vec![InputId("tui-1".to_owned())],
                    step: 1,
                },
            ] {
                events.send(event).await.unwrap();
            }
            let lines = until(&run.screen, "\u{203A} [Image #1").await;
            assert!(
                lines.iter().any(|l| l.contains("against [Image #2")),
                "{lines:#?}"
            );
            run.quit();
        },
    )
    .await;
    assert_eq!(
        commands,
        vec![ClientCommand::Send {
            id: InputId("tui-1".to_owned()),
            mode: DeliveryMode::Steer,
            turn: None,
            replaces: None,
            text: format!(" against {missing}"),
            images: vec![
                ImageData {
                    data: wisp_media::encode_base64(&first).into(),
                },
                ImageData {
                    data: wisp_media::encode_base64(&second).into(),
                },
            ],
        }]
    );
}

#[tokio::test]
async fn a_cut_keeps_unsent_image_and_pasted_text_chips() {
    let refused = |id| AgentEvent::Rejected {
        request: None,
        reason: "account unavailable".into(),
        id: Some(id),
        kind: Some(RejectKind::AccountGone),
    };
    let dir = Dir::new("reconnected-chips");
    let image = bmp(10, 40, 1);
    let path = dir.put("one.bmp", &image);
    let text = "p".repeat(900);
    let pasted = text.clone();
    let rewound = super::rewind::rewound(
        wisp_protocol::RequestId(1),
        1,
        vec![super::rewind::returned(
            &InputId("returned-1".into()),
            "returned words",
        )],
    );
    let accepted = AgentEvent::InputAccepted {
        opening: false,
        front: false,
        id: InputId("withdrawn-1".into()),
        mode: DeliveryMode::Steer,
        text: "withdrawn words".into(),
        images: Vec::new(),
        replaces: None,
        held: false,
        from: None,
        checklist_spans: Vec::new(),
    };
    let withdrawn = AgentEvent::InputWithdrawn {
        ids: vec![InputId("withdrawn-1".into())],
        by: wisp_protocol::WithdrawnBy::TurnStopped,
    };
    let returned = [rewound, accepted, withdrawn];
    let replayed = returned.clone();
    let mut opts = options(Start::Stalled);
    opts.images = dir.setup(&[], Duration::from_secs(5));
    let ended = attached(&mut opts, wisp_protocol::link::REQUEST_RANGE);
    let finished = steering_finished(
        Screen::new(24, 220),
        opts,
        image_facts(MODEL),
        |mut a| async move {
            a.paste(&quoted(&path));
            until(&a.screen, "Image #1").await;
            a.paste(&pasted);
            until(&a.screen, "Pasted text #1").await;
            for event in returned {
                a.emit(event).await;
            }
            a.key(KeyCode::Enter);
            let id = sent_id(&a.next().await);
            a.emit(refused(id)).await;
            until_caret(&a.screen, "Pasted text #1").await;
            a.cut(ended).await;
        },
    )
    .await;
    assert_eq!(finished.exit.unwrap(), Exit::Cut);
    assert!(finished.unsent.is_some());
    let mut opts = options(Start::Stalled);
    opts.unsent = finished.unsent;
    let finished = steering_finished(
        Screen::new(24, 80),
        opts,
        image_facts(MODEL),
        |mut a| async move {
            let lines = a.settle().await;
            assert!(lines.iter().any(|l| l.contains("Image #1")), "{lines:#?}");
            assert!(
                lines.iter().any(|l| l.contains("Pasted text #1")),
                "{lines:#?}"
            );
            a.nothing_sent().await;
            for event in replayed {
                a.emit(event).await;
            }
            a.key(KeyCode::Enter);
            let ClientCommand::Send {
                id,
                text: sent,
                images,
                ..
            } = a.next().await
            else {
                panic!("expected the reconnected chips to be sent");
            };
            assert_eq!(sent, format!("withdrawn words\n\nreturned words\n\n{text}"));
            assert_eq!(
                images,
                vec![ImageData {
                    data: wisp_media::encode_base64(&image).into()
                }]
            );
            a.emit(refused(id)).await;
            until_caret(&a.screen, "Pasted text #1").await;
        },
    )
    .await;
    assert!(finished.unsent.is_none());
}

#[tokio::test]
async fn alt_v_reads_the_clipboard_off_the_ui_thread_and_attaches_its_image() {
    let dir = Dir::new("alt-v");
    dir.tool("wl-paste", WL_PASTE);
    let image = bmp(56, 28, 5);
    dir.put("image.bmp", &image);
    let mut opts = options(Start::Stalled);
    opts.images = dir.setup(&[Native::Wayland], Duration::from_secs(5));
    let screen = Screen::new(24, 100);
    let commands = with_commands(
        screen.clone(),
        opts,
        image_facts(MODEL),
        |run, events, sent| async move {
            run.send(alt_v());
            until(
                &run.screen,
                "[Image #1 \u{b7} 56\u{d7}28 \u{b7} \u{2248}2 tokens]",
            )
            .await;
            run.send(Input::Key(Key::ctrl('v')));
            until(&run.screen, "[Image #2 ").await;
            run.send(key(KeyCode::Enter));
            sent.next().await;
            for id in ["tui-0", "tui-1"] {
                events
                    .send(AgentEvent::Rejected {
                        request: None,
                        reason: format!("Image 1: refused for {id}."),
                        id: Some(InputId(id.to_owned())),
                        kind: Some(RejectKind::Image),
                    })
                    .await
                    .unwrap();
            }
            until(
                &run.screen,
                "This message wasn't sent: Image 1: refused for tui-1.",
            )
            .await;
            let line = until_caret(&run.screen, "[Image #2 ").await;
            assert!(line.contains("[Image #1 "), "{line:?}");
            let lines = run.screen.visible();
            assert!(
                !lines.iter().any(|l| l.contains("refused for tui-0")),
                "{lines:#?}"
            );
            run.send(ctrl('c'));
            run.quit();
        },
    )
    .await;
    let data: Arc<str> = wisp_media::encode_base64(&image).into();
    assert_eq!(
        commands,
        vec![ClientCommand::Send {
            id: InputId("tui-1".to_owned()),
            mode: DeliveryMode::Steer,
            turn: None,
            replaces: None,
            text: String::new(),
            images: vec![ImageData { data: data.clone() }, ImageData { data }],
        }]
    );
    assert!(
        !dir.path.root.join("tty").exists(),
        "the reader saw the tty"
    );
}

#[tokio::test]
async fn a_hanging_clipboard_never_blocks_typing_and_its_failure_is_a_sentence() {
    let dir = Dir::new("hang");
    dir.tool("wl-paste", WL_PASTE);
    dir.put("hang", b"");
    let mut opts = options(Start::Stalled);
    opts.images = dir.setup(&[Native::Wayland], Duration::from_secs(2));
    let screen = Screen::new(24, 100);
    with_commands(
        screen.clone(),
        opts,
        image_facts(MODEL),
        |run, _, _| async move {
            run.send(alt_v());
            let started = Instant::now();
            run.send(Input::Paste("still typing".to_owned()));
            until(&run.screen, "still typing").await;
            assert!(started.elapsed() < Duration::from_secs(1));
            until(&run.screen, &copy::reading_clipboard("\u{2026}")).await;
            run.send(alt_v());
            until(&run.screen, "Still reading the clipboard.").await;
            until(
                &run.screen,
                "Couldn't read the clipboard: wl-paste took longer than 2s.",
            )
            .await;
            run.send(ctrl('c'));
            run.quit();
        },
    )
    .await;
}

#[tokio::test]
async fn every_refusal_is_a_sentence_in_the_status_row() {
    let dir = Dir::new("refusals");
    let mut ssh = dir.setup(&[Native::Wayland], Duration::from_secs(5));
    ssh.ssh = true;
    for (setup, model, wanted) in [
        (ssh, MODEL, copy::NO_CLIPBOARD_OVER_SSH.to_owned()),
        (
            dir.setup(&[], Duration::from_secs(5)),
            MODEL,
            copy::NO_CLIPBOARD.to_owned(),
        ),
        (
            dir.setup(&[Native::Wayland], Duration::from_secs(5)),
            MODEL,
            "Couldn't read the clipboard: install wl-clipboard.".to_owned(),
        ),
        (
            dir.setup(&[Native::Wayland], Duration::from_secs(5)),
            "test-model",
            "test-model can't take images: its limits are unknown.".to_owned(),
        ),
    ] {
        let mut opts = options(Start::Stalled);
        opts.images = setup;
        let screen = Screen::new(24, 120);
        with_commands(
            screen.clone(),
            opts,
            image_facts(model),
            |run, _, _| async move {
                run.send(alt_v());
                until(&run.screen, &wanted).await;
                run.quit();
            },
        )
        .await;
    }
}

#[tokio::test]
async fn escape_sequences_in_dropped_file_names_stay_inert() {
    let dir = Dir::new("names");
    let escaped = dir.put("\u{1b}]0;pwn\u{7}\u{1b}[2Jx.png", b"not an image");
    let flipped = dir.put("evil\u{202e}gpj.png", b"not an image");
    let mut opts = options(Start::Stalled);
    opts.images = dir.setup(&[], Duration::from_secs(5));
    let screen = Screen::new(24, 160);
    with_commands(
        screen.clone(),
        opts,
        image_facts(MODEL),
        |run, _, _| async move {
            run.send(Input::Paste(quoted(&flipped)));
            until(
            &run.screen,
            "Couldn't attach evil<U+202E>gpj.png. This isn't a PNG, JPEG, GIF, WebP or BMP image.",
        )
        .await;
            run.send(Input::Paste(quoted(&escaped)));
            until(&run.screen, "^[[2Jx.png").await;
            run.send(ctrl('c'));
            run.quit();
        },
    )
    .await;
    assert!(screen.spied().is_empty(), "{:?}", screen.spied());
    assert!(
        screen
            .seen()
            .iter()
            .all(|l| !l.contains('\u{202e}') && !l.contains("pwn\u{7}")),
        "{:#?}",
        screen.seen()
    );
}

#[tokio::test]
async fn a_path_that_cannot_attach_is_pasted_as_text_with_the_reason() {
    let dir = Dir::new("refused-text");
    let notes = dir.put("notes.png", b"plain text, not a picture");
    let mut opts = options(Start::Stalled);
    opts.images = dir.setup(&[], Duration::from_secs(5));
    let screen = Screen::new(24, 200);
    let pasted = quoted(&notes);
    let typed = pasted.clone();
    let commands = with_commands(
        screen.clone(),
        opts,
        image_facts(MODEL),
        |run, _, sent| async move {
            run.send(Input::Paste(typed));
            until(
                &run.screen,
                "Couldn't attach notes.png. This isn't a PNG, JPEG, GIF, WebP or BMP image.",
            )
            .await;
            until_caret(&run.screen, "notes.png").await;
            run.send(key(KeyCode::Enter));
            sent.next().await;
            run.quit();
        },
    )
    .await;
    assert_eq!(
        commands,
        vec![ClientCommand::Send {
            id: InputId("tui-1".to_owned()),
            mode: DeliveryMode::Steer,
            turn: None,
            replaces: None,
            text: pasted,
            images: vec![],
        }]
    );
}

const GATED_WSLPATH: &str = r#"#!/bin/sh
d="$(dirname "$0")"
while [ ! -f "$d/go" ] && [ -d "$d" ]; do /bin/sleep 0.02; done
case "$2" in
  *a.bmp) printf '%s\n' "$d/a.bmp" ;;
  *) printf '%s\n' "$d/b.bmp" ;;
esac
"#;

#[tokio::test]
async fn paths_that_would_pass_the_image_limit_when_they_land_are_pasted_as_text() {
    let dir = Dir::new("limit");
    dir.tool("wslpath", GATED_WSLPATH);
    dir.put("a.bmp", &bmp(4, 4, 1));
    dir.put("b.bmp", &bmp(4, 4, 2));
    let nine: Vec<String> = (0..9u8)
        .map(|n| quoted(&dir.put(&format!("{n}.bmp"), &bmp(4, 4, n))))
        .collect();
    let mut opts = options(Start::Stalled);
    opts.images = dir.setup(&[], Duration::from_secs(30));
    opts.images.wsl = true;
    let screen = Screen::new(24, 400);
    let windows = "C:\\a.bmp C:\\b.bmp";
    let go = dir.path.root.join("go");
    let commands = with_commands(
        screen.clone(),
        opts,
        image_facts(MODEL),
        |run, events, sent| async move {
            run.send(Input::Paste(nine.join(" ")));
            until(&run.screen, "[Image #9 ").await;
            run.send(Input::Paste(" marker".to_owned()));
            run.send(key(KeyCode::Enter));
            sent.next().await;
            answered(&run, &events).await;
            run.send(Input::Paste(windows.to_owned()));
            run.send(key(KeyCode::Up));
            until_caret(&run.screen, "marker").await;
            std::fs::write(&go, b"").unwrap();
            until(&run.screen, &copy::too_many_images(10)).await;
            until_caret(&run.screen, "b.bmp").await;
            run.send(key(KeyCode::Enter));
            sent.next().await;
            run.quit();
        },
    )
    .await;
    let [first, second] = commands.as_slice() else {
        panic!("{commands:#?}");
    };
    for (command, text) in [
        (first, " marker".to_owned()),
        (second, format!(" marker{windows}")),
    ] {
        let ClientCommand::Send {
            text: sent, images, ..
        } = command
        else {
            panic!("{command:?}");
        };
        assert_eq!(sent, &text);
        assert_eq!(images.len(), 9);
    }
}

#[tokio::test]
async fn a_late_rejection_of_an_image_message_comes_back_with_its_images_after_a_text_message() {
    let dir = Dir::new("late");
    let shot = dir.put("shot.bmp", &bmp(8, 8, 3));
    let mut opts = options(Start::Stalled);
    opts.images = dir.setup(&[], Duration::from_secs(5));
    let screen = Screen::new(24, 200);
    let commands = with_commands(
        screen.clone(),
        opts,
        image_facts(MODEL),
        |run, events, sent| async move {
            run.send(Input::Paste(quoted(&shot)));
            until(&run.screen, "[Image #1 ").await;
            run.send(Input::Paste(" look".to_owned()));
            run.send(key(KeyCode::Enter));
            sent.next().await;
            answered(&run, &events).await;
            run.say("hello");
            sent.next().await;
            events
                .send(AgentEvent::Rejected {
                    request: None,
                    reason: "Image 1: refused.".to_owned(),
                    id: Some(InputId("tui-1".to_owned())),
                    kind: Some(RejectKind::Image),
                })
                .await
                .unwrap();
            until(&run.screen, "This message wasn't sent: Image 1: refused.").await;
            let line = until_caret(&run.screen, "look").await;
            assert!(line.contains("[Image #1 "), "{line:?}");
            run.send(ctrl('c'));
            run.quit();
        },
    )
    .await;
    assert_eq!(commands.len(), 2, "{commands:#?}");
}

const WL_PASTE_TEXT: &str = r#"#!/bin/sh
if [ "$1" = "--list-types" ]; then printf 'text/plain\n'; exit 0; fi
printf 'words'
"#;

fn empty_paste() -> Input {
    Input::Paste(String::new())
}

#[tokio::test]
async fn an_empty_paste_reads_the_clipboard_like_alt_v() {
    let dir = Dir::new("empty-paste");
    dir.tool("wl-paste", WL_PASTE);
    let image = bmp(56, 28, 7);
    dir.put("image.bmp", &image);
    let mut opts = options(Start::Stalled);
    opts.images = dir.setup(&[Native::Wayland], Duration::from_secs(5));
    let screen = Screen::new(24, 100);
    let commands = with_commands(
        screen.clone(),
        opts,
        image_facts(MODEL),
        |run, _, sent| async move {
            run.send(empty_paste());
            until(
                &run.screen,
                "[Image #1 \u{b7} 56\u{d7}28 \u{b7} \u{2248}2 tokens]",
            )
            .await;
            run.send(key(KeyCode::Enter));
            sent.next().await;
            run.quit();
        },
    )
    .await;
    assert_eq!(
        commands,
        vec![ClientCommand::Send {
            id: InputId("tui-1".to_owned()),
            mode: DeliveryMode::Steer,
            turn: None,
            replaces: None,
            text: String::new(),
            images: vec![ImageData {
                data: wisp_media::encode_base64(&image).into(),
            }],
        }]
    );
    assert!(
        !dir.path.root.join("tty").exists(),
        "the reader saw the tty"
    );
}

#[tokio::test]
async fn an_empty_paste_with_no_image_on_the_clipboard_says_nothing() {
    let dir = Dir::new("empty-paste-text");
    dir.tool("wl-paste", WL_PASTE_TEXT);
    let mut opts = options(Start::Stalled);
    opts.images = dir.setup(&[Native::Wayland], Duration::from_secs(5));
    let screen = Screen::new(24, 100);
    let commands = with_commands(
        screen.clone(),
        opts,
        image_facts(MODEL),
        |run, _, _| async move {
            run.send(empty_paste());
            run.send(empty_paste());
            sleep(Duration::from_secs(2)).await;
            let seen = run.screen.seen();
            assert!(
                !seen.iter().any(|l| l.contains(copy::NO_CLIPBOARD_IMAGE)
                    || l.contains(copy::STILL_READING_CLIPBOARD)),
                "{seen:#?}"
            );
            assert!(!run.screen.caret_line().contains("words"));
            run.send(alt_v());
            until(&run.screen, copy::NO_CLIPBOARD_IMAGE).await;
            run.quit();
        },
    )
    .await;
    assert!(commands.is_empty(), "{commands:?}");
}

#[tokio::test]
async fn an_empty_paste_still_names_a_problem_the_user_can_fix() {
    let dir = Dir::new("empty-paste-refusals");
    let mut opts = options(Start::Stalled);
    opts.images = dir.setup(&[Native::Wayland], Duration::from_secs(5));
    let screen = Screen::new(24, 120);
    with_commands(
        screen.clone(),
        opts,
        image_facts(MODEL),
        |run, _, _| async move {
            run.send(empty_paste());
            until(
                &run.screen,
                "Couldn't read the clipboard: install wl-clipboard.",
            )
            .await;
            run.quit();
        },
    )
    .await;
}

#[tokio::test]
async fn a_whitespace_paste_stays_text() {
    let dir = Dir::new("blank-paste");
    dir.tool("wl-paste", WL_PASTE);
    dir.put("image.bmp", &bmp(4, 4, 1));
    let mut opts = options(Start::Stalled);
    opts.images = dir.setup(&[Native::Wayland], Duration::from_secs(5));
    let screen = Screen::new(24, 100);
    let commands = with_commands(
        screen.clone(),
        opts,
        image_facts(MODEL),
        |run, _, sent| async move {
            run.send(Input::Paste("  \n".to_owned()));
            run.send(key(KeyCode::Char('x')));
            run.send(key(KeyCode::Enter));
            sent.next().await;
            run.quit();
        },
    )
    .await;
    assert_eq!(
        commands,
        vec![ClientCommand::Send {
            id: InputId("tui-1".to_owned()),
            mode: DeliveryMode::Steer,
            turn: None,
            replaces: None,
            text: "  \nx".to_owned(),
            images: vec![],
        }]
    );
}

#[tokio::test]
async fn an_empty_paste_is_ignored_in_a_picker_and_the_transcript() {
    let dir = Dir::new("empty-paste-ignored");
    dir.tool("wl-paste", WL_PASTE);
    dir.put("image.bmp", &bmp(56, 28, 9));
    let mut opts = options(Start::Stalled);
    opts.images = dir.setup(&[Native::Wayland], Duration::from_secs(5));
    let screen = Screen::new(24, 100);
    with_commands(
        screen.clone(),
        opts,
        image_facts(MODEL),
        |run, events, _| async move {
            run.say("/model");
            sleep(Duration::from_millis(100)).await;
            events.send(described(1)).await.unwrap();
            until(&run.screen, "type to filter").await;
            run.send(empty_paste());
            run.send(key(KeyCode::Esc));
            run.send(ctrl('o'));
            assert!(
                eventually(|| run.screen.in_pager()).await,
                "the transcript never opened"
            );
            run.send(empty_paste());
            run.send(key(KeyCode::Char('q')));
            sleep(Duration::from_secs(2)).await;
            let seen = run.screen.seen();
            assert!(!seen.iter().any(|l| l.contains("[Image #")), "{seen:#?}");
            run.send(alt_v());
            until(&run.screen, "[Image #1 ").await;
            run.send(ctrl('c'));
            run.quit();
        },
    )
    .await;
}

#[tokio::test]
async fn a_paste_in_the_transcript_pager_never_reaches_the_composer() {
    let (dir, opts) = pasting("in-pager");
    let screen = Screen::new(24, 100);
    with_commands(
        screen.clone(),
        opts,
        image_facts(MODEL),
        |run, _, _| async move {
            run.send(ctrl('o'));
            assert!(
                eventually(|| run.screen.in_pager()).await,
                "the transcript never opened"
            );
            paste_every_way(&run.inputs).await;
            run.send(key(KeyCode::Char('q')));
            assert!(
                eventually(|| !run.screen.in_pager()).await,
                "the transcript never closed"
            );
            sleep(Duration::from_millis(300)).await;
            assert_eq!(leaks(&run.screen, &dir), NO_LEAKS);
            run.quit();
        },
    )
    .await;
}

#[tokio::test]
async fn an_empty_paste_attaches_while_a_turn_runs() {
    let dir = Dir::new("empty-paste-turn");
    dir.tool("wl-paste", WL_PASTE);
    dir.put("image.bmp", &bmp(56, 28, 3));
    let mut opts = options(Start::Stalled);
    opts.images = dir.setup(&[Native::Wayland], Duration::from_secs(5));
    let screen = Screen::new(24, 100);
    with_commands(
        screen.clone(),
        opts,
        image_facts(MODEL),
        |run, events, sent| async move {
            run.say("first");
            sent.next().await;
            for event in [
                AgentEvent::turn_started(1),
                AgentEvent::InputDelivered {
                    ids: vec![InputId("tui-1".to_owned())],
                    step: 1,
                },
            ] {
                events.send(event).await.unwrap();
            }
            until(&run.screen, "\u{203A} first").await;
            run.send(empty_paste());
            until_caret(&run.screen, "[Image #1 ").await;
            answered(&run, &events).await;
            run.send(ctrl('c'));
            run.quit();
        },
    )
    .await;
}

#[tokio::test]
async fn an_empty_paste_takes_the_focus_off_a_waiting_row() {
    let dir = Dir::new("empty-paste-row");
    dir.tool("wl-paste", WL_PASTE);
    dir.put("image.bmp", &bmp(56, 28, 4));
    let mut opts = options(Start::Stalled);
    opts.images = dir.setup(&[Native::Wayland], Duration::from_secs(5));
    let screen = Screen::new(24, 100);
    with_commands(
        screen.clone(),
        opts,
        image_facts(MODEL),
        |run, events, sent| async move {
            run.say("first");
            sent.next().await;
            for event in [
                AgentEvent::turn_started(1),
                AgentEvent::InputDelivered {
                    ids: vec![InputId("tui-1".to_owned())],
                    step: 1,
                },
            ] {
                events.send(event).await.unwrap();
            }
            until(&run.screen, "\u{203A} first").await;
            run.send(Input::Paste("later".to_owned()));
            run.send(key(KeyCode::Tab));
            let queued = sent.next().await;
            let ClientCommand::Send { id, mode, text, .. } = queued else {
                panic!("not a send: {queued:?}");
            };
            assert_eq!(mode, DeliveryMode::Queue);
            events
                .send(AgentEvent::InputAccepted {
                    opening: false,
                    id,
                    mode,
                    text,
                    images: Vec::new(),
                    replaces: None,
                    held: false,
                    front: false,
                    from: None,
                    checklist_spans: Vec::new(),
                })
                .await
                .unwrap();
            until(&run.screen, "\u{2191} edit waiting").await;
            run.send(key(KeyCode::Up));
            until(&run.screen, "enter edit").await;
            run.send(empty_paste());
            until_caret(&run.screen, "[Image #1 ").await;
            run.send(key(KeyCode::Char('x')));
            let line = until_caret(&run.screen, "]x").await;
            assert!(line.contains("[Image #1 "), "{line:?}");
            let withdrawn: Vec<_> = sent
                .all()
                .into_iter()
                .filter(|c| matches!(c, ClientCommand::Withdraw { .. }))
                .collect();
            assert!(withdrawn.is_empty(), "{withdrawn:?}");
            answered(&run, &events).await;
            run.send(ctrl('c'));
            run.quit();
        },
    )
    .await;
}

const WL_RIGHT: &str = r#"#!/bin/sh
d="$(dirname "$0")"
printf '%s\n' "$*" >> "$d/calls"
if [ -t 0 ] || [ -t 1 ] || [ -t 2 ]; then echo tty >> "$d/tty"; fi
if [ -f "$d/hang" ]; then /bin/sleep 30; fi
if [ -f "$d/held" ]; then until [ -f "$d/go" ] || [ ! -d "$d" ]; do /bin/sleep 0.02; done; fi
if [ "$1" = "--no-newline" ]; then
  if [ -f "$d/text" ]; then /bin/cat "$d/text"; exit 0; fi
  exit 1
fi
if [ "$1" = "--list-types" ]; then printf 'image/bmp\n'; exit 0; fi
/bin/cat "$d/image.bmp"
"#;

pub(super) fn right_click() -> Input {
    Input::Mouse(Mouse {
        kind: MouseKind::RightPress,
        column: 3,
        row: 2,
        modified: false,
    })
}

pub(super) fn right_pasting(dir: &Dir, pastes: bool) -> Options {
    dir.tool("wl-paste", WL_RIGHT);
    let mut opts = options(Start::Stalled);
    opts.images = dir.setup(&[Native::Wayland], Duration::from_secs(5));
    opts.right_paste = pastes;
    opts
}

pub(super) fn calls(dir: &Dir) -> Vec<String> {
    std::fs::read_to_string(dir.path.root.join("calls"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

pub(super) async fn land_held_read(dir: &Dir, screen: &Screen) {
    let reading = copy::reading_clipboard("");
    eventually(|| shows(&screen.visible(), &reading)).await;
    let lines = screen.visible();
    assert!(shows(&lines, &reading), "{lines:#?}");
    dir.put("go", b"");
    eventually(|| !shows(&screen.visible(), &reading)).await;
    let lines = screen.visible();
    assert!(!shows(&lines, &reading), "the hint never went: {lines:#?}");
}

pub(super) const PASTED: &str = "PASTED";

const CLIPBOARD_TEXT: &str = "clipboard words";

pub(super) fn seeing(facts: SessionFacts) -> SessionFacts {
    SessionFacts {
        model: MODEL.to_owned(),
        ..facts
    }
}

pub(super) fn pasting_into(dir: &Dir, mut opts: Options) -> Options {
    dir.tool("wl-paste", WL_RIGHT);
    dir.put("text", CLIPBOARD_TEXT.as_bytes());
    opts.images = dir.setup(&[Native::Wayland], Duration::from_secs(5));
    opts.right_paste = true;
    opts
}

pub(super) fn pasting(name: &str) -> (Dir, Options) {
    let dir = Dir::new(name);
    let opts = pasting_into(&dir, options(Start::Stalled));
    (dir, opts)
}

pub(super) async fn paste_from_clipboard(inputs: &mpsc::UnboundedSender<Input>) {
    inputs.send(empty_paste()).unwrap();
    real_wait(400).await;
    inputs.send(right_click()).unwrap();
    real_wait(400).await;
}

pub(super) async fn paste_every_way(inputs: &mpsc::UnboundedSender<Input>) {
    inputs.send(Input::Paste(PASTED.to_owned())).unwrap();
    paste_from_clipboard(inputs).await;
}

pub(super) fn clipboard_leaks(screen: &Screen, dir: &Dir) -> Vec<&'static str> {
    let calls = calls(dir);
    let called = |flag: &str| calls.iter().any(|call| call.starts_with(flag));
    let shown = screen
        .seen()
        .iter()
        .any(|line| line.contains(CLIPBOARD_TEXT));
    [
        ("image paste", called("--list-types")),
        ("right-click paste", called("--no-newline") || shown),
    ]
    .into_iter()
    .filter_map(|(kind, leaked)| leaked.then_some(kind))
    .collect()
}

pub(super) fn leaks(screen: &Screen, dir: &Dir) -> Vec<&'static str> {
    let text = screen.seen().iter().any(|line| line.contains(PASTED));
    let mut kinds = clipboard_leaks(screen, dir);
    if text {
        kinds.insert(0, "text paste");
    }
    kinds
}

pub(super) const NO_LEAKS: [&str; 0] = [];

fn sent_text(command: &ClientCommand) -> String {
    match command {
        ClientCommand::Send { text, .. } => text.clone(),
        other => panic!("not a send: {other:?}"),
    }
}

#[tokio::test]
async fn right_click_with_nothing_selected_pastes_the_text_as_a_bracketed_paste_would() {
    let dir = Dir::new("right-text");
    let opts = right_pasting(&dir, true);
    let pasted = "one \u{1b}[31mred\u{1b}[0m\r\ntwo\ttabbed\u{7}";
    dir.put("text", pasted.as_bytes());
    let long: String = (1..=12).map(|n| format!("line {n}\n")).collect();
    let screen = Screen::new(24, 100);
    let commands = with_commands(
        screen.clone(),
        opts,
        image_facts(MODEL),
        |run, events, sent| async move {
            run.send(Input::Paste(pasted.to_owned()));
            run.send(key(KeyCode::Enter));
            sent.next().await;
            answered(&run, &events).await;
            run.send(right_click());
            until_caret(&run.screen, "tabbed").await;
            run.send(key(KeyCode::Enter));
            sent.next().await;
            answered(&run, &events).await;
            dir.put("text", long.as_bytes());
            run.send(right_click());
            let chip = copy::pasted_text(1, 12, long.chars().count());
            until_caret(&run.screen, &chip).await;
            run.send(ctrl('c'));
            run.quit();
            assert!(
                !dir.path.root.join("tty").exists(),
                "the reader saw the tty"
            );
        },
    )
    .await;
    assert_eq!(commands.len(), 2, "{commands:#?}");
    assert_eq!(sent_text(&commands[0]), sent_text(&commands[1]));
    assert!(!sent_text(&commands[1]).contains('\u{1b}'));
}

#[tokio::test]
async fn right_click_with_only_an_image_on_the_clipboard_attaches_it_as_alt_v_does() {
    let dir = Dir::new("right-image");
    let opts = right_pasting(&dir, true);
    let image = bmp(56, 28, 11);
    dir.put("image.bmp", &image);
    let screen = Screen::new(24, 100);
    let commands = with_commands(
        screen.clone(),
        opts,
        image_facts(MODEL),
        |run, _, sent| async move {
            run.send(right_click());
            until_caret(
                &run.screen,
                "[Image #1 \u{b7} 56\u{d7}28 \u{b7} \u{2248}2 tokens]",
            )
            .await;
            run.send(key(KeyCode::Enter));
            sent.next().await;
            run.quit();
        },
    )
    .await;
    assert_eq!(
        calls(&dir),
        vec![
            "--no-newline --type text".to_owned(),
            "--list-types".to_owned(),
            "--type image/bmp".to_owned(),
        ]
    );
    assert_eq!(
        commands,
        vec![ClientCommand::Send {
            id: InputId("tui-1".to_owned()),
            mode: DeliveryMode::Steer,
            turn: None,
            replaces: None,
            text: String::new(),
            images: vec![ImageData {
                data: wisp_media::encode_base64(&image).into(),
            }],
        }]
    );
}

#[tokio::test]
async fn right_click_never_pastes_where_the_terminal_would_not() {
    let dir = Dir::new("right-elsewhere");
    let opts = right_pasting(&dir, false);
    dir.put("text", b"should not paste");
    dir.put("image.bmp", &bmp(4, 4, 1));
    let screen = Screen::new(24, 100);
    let commands = with_commands(
        screen.clone(),
        opts,
        image_facts(MODEL),
        |run, _, _| async move {
            run.send(right_click());
            sleep(Duration::from_secs(1)).await;
            let seen = run.screen.seen();
            assert!(
                !seen
                    .iter()
                    .any(|l| l.contains("should not") || l.contains("[Image #")),
                "{seen:#?}"
            );
            run.quit();
        },
    )
    .await;
    assert!(calls(&dir).is_empty(), "{:?}", calls(&dir));
    assert!(commands.is_empty(), "{commands:?}");
}

#[tokio::test]
async fn right_click_is_ignored_in_a_picker_and_the_transcript() {
    let dir = Dir::new("right-ignored");
    let opts = right_pasting(&dir, true);
    dir.put("text", b"pasted words");
    let screen = Screen::new(24, 100);
    with_commands(
        screen.clone(),
        opts,
        image_facts(MODEL),
        |run, events, _| async move {
            run.say("/model");
            sleep(Duration::from_millis(100)).await;
            events.send(described(1)).await.unwrap();
            until(&run.screen, "type to filter").await;
            run.send(right_click());
            sleep(Duration::from_millis(500)).await;
            run.send(key(KeyCode::Esc));
            run.send(ctrl('o'));
            assert!(
                eventually(|| run.screen.in_pager()).await,
                "the transcript never opened"
            );
            run.send(right_click());
            sleep(Duration::from_millis(500)).await;
            run.send(key(KeyCode::Char('q')));
            sleep(Duration::from_secs(1)).await;
            assert!(calls(&dir).is_empty(), "{:?}", calls(&dir));
            assert!(!run.screen.caret_line().contains("pasted words"));
            run.send(right_click());
            until_caret(&run.screen, "pasted words").await;
            run.send(ctrl('c'));
            run.quit();
        },
    )
    .await;
}

#[tokio::test]
async fn a_slow_right_click_paste_says_it_is_reading_and_never_blocks_typing() {
    let dir = Dir::new("right-slow");
    let mut opts = right_pasting(&dir, true);
    opts.images = dir.setup(&[Native::Wayland], Duration::from_secs(2));
    dir.put("hang", b"");
    let screen = Screen::new(24, 100);
    with_commands(
        screen.clone(),
        opts,
        image_facts(MODEL),
        |run, _, _| async move {
            run.send(right_click());
            let started = Instant::now();
            run.send(Input::Paste("still typing".to_owned()));
            until(&run.screen, "still typing").await;
            assert!(started.elapsed() < Duration::from_secs(1));
            until(&run.screen, &copy::reading_clipboard("\u{2026}")).await;
            until(
                &run.screen,
                "Couldn't read the clipboard: wl-paste took longer than 2s.",
            )
            .await;
            run.send(ctrl('c'));
            run.quit();
        },
    )
    .await;
}

#[tokio::test]
async fn a_right_click_result_that_lands_in_a_picker_is_dropped() {
    for text in [true, false] {
        let dir = Dir::new(&format!("right-late-{text}"));
        let opts = right_pasting(&dir, true);
        if text {
            dir.put("text", b"late words");
        }
        dir.put("image.bmp", &bmp(56, 28, 2));
        dir.put("held", b"");
        let screen = Screen::new(24, 100);
        with_commands(
            screen.clone(),
            opts,
            image_facts(MODEL),
            |run, events, _| async move {
                run.send(right_click());
                run.say("/model");
                sleep(Duration::from_millis(100)).await;
                events.send(described(1)).await.unwrap();
                until(&run.screen, "type to filter").await;
                assert!(
                    eventually(|| !calls(&dir).is_empty()).await,
                    "the read never ran"
                );
                land_held_read(&dir, &run.screen).await;
                run.send(key(KeyCode::Esc));
                sleep(Duration::from_millis(500)).await;
                let seen = run.screen.seen();
                assert!(
                    !seen
                        .iter()
                        .any(|l| l.contains("late words") || l.contains("[Image #")),
                    "{seen:#?}"
                );
                assert_eq!(calls(&dir), vec!["--no-newline --type text".to_owned()]);
                run.send(ctrl('c'));
                run.quit();
            },
        )
        .await;
    }
}

#[tokio::test]
async fn a_right_clicked_image_path_attaches_as_a_pasted_one_does() {
    let dir = Dir::new("right-path");
    let opts = right_pasting(&dir, true);
    let shot = dir.put("shot.bmp", &bmp(56, 28, 6));
    dir.put("text", quoted(&shot).as_bytes());
    let screen = Screen::new(24, 100);
    with_commands(
        screen.clone(),
        opts,
        image_facts(MODEL),
        |run, _, _| async move {
            run.send(right_click());
            let line = until_caret(
                &run.screen,
                "[Image #1 \u{b7} 56\u{d7}28 \u{b7} \u{2248}2 tokens]",
            )
            .await;
            assert!(!line.contains("shot.bmp"), "{line:?}");
            run.send(ctrl('c'));
            run.quit();
        },
    )
    .await;
}

#[tokio::test]
async fn right_click_is_ignored_on_the_settings_screen_and_during_a_hand_off() {
    let dir = Dir::new("right-screens");
    let opts = right_pasting(&dir, true);
    dir.put("text", b"pasted words");
    let screen = Screen::new(24, 100);
    with_commands(
        screen.clone(),
        opts,
        image_facts(MODEL),
        |run, _, _| async move {
            run.say("/config");
            until(&run.screen, "Loading settings").await;
            run.send(right_click());
            sleep(Duration::from_millis(500)).await;
            run.send(key(KeyCode::Esc));
            run.send(ctrl('o'));
            assert!(
                eventually(|| run.screen.in_pager()).await,
                "the transcript never opened"
            );
            run.send(key(KeyCode::Char('[')));
            assert!(
                eventually(|| !run.screen.handed().is_empty()).await,
                "never handed off"
            );
            run.send(right_click());
            sleep(Duration::from_millis(500)).await;
            assert!(calls(&dir).is_empty(), "{:?}", calls(&dir));
            run.send(key(KeyCode::Esc));
            run.send(key(KeyCode::Char('q')));
            sleep(Duration::from_millis(500)).await;
            assert!(!run.screen.caret_line().contains("pasted words"));
            run.send(right_click());
            until_caret(&run.screen, "pasted words").await;
            run.send(ctrl('c'));
            run.quit();
        },
    )
    .await;
}

async fn rewound_with_the_second_image_refused(held: bool) -> Vec<String> {
    let dir = Dir::new(if held {
        "rewind-held"
    } else {
        "rewind-resumed"
    });
    let images: Vec<Vec<u8>> = (1..=3u8).map(|i| bmp(10 * u32::from(i), 10, i)).collect();
    let paths: Vec<PathBuf> = images
        .iter()
        .enumerate()
        .map(|(i, image)| dir.put(&format!("{}.bmp", i + 1), image))
        .collect();
    let mut opts = options(Start::Stalled);
    opts.images = dir.setup(&[], Duration::from_secs(5));
    let screen = Screen::new(24, 160);
    let seen = Rc::new(RefCell::new(Vec::new()));
    let kept = Rc::clone(&seen);
    with_commands(
        screen.clone(),
        opts,
        image_facts(MODEL),
        |run, events, sent| async move {
            for (i, path) in paths.iter().enumerate() {
                run.send(Input::Paste(format!("{} ", quoted(path))));
                until(&run.screen, &format!("[Image #{} ", i + 1)).await;
            }
            run.send(Input::Paste("compare them".to_owned()));
            until_caret(&run.screen, "compare them").await;
            run.send(key(KeyCode::Enter));
            let id = match sent.next().await {
                ClientCommand::Send { id, images, .. } => {
                    assert_eq!(images.len(), 3);
                    id
                }
                other => panic!("expected a Send, got {other:?}"),
            };
            for event in [
                AgentEvent::turn_started(1),
                AgentEvent::InputDelivered {
                    ids: vec![id.clone()],
                    step: 1,
                },
                completed(),
            ] {
                events.send(event).await.unwrap();
            }
            until(&run.screen, "cost unknown").await;
            run.send(key(KeyCode::Esc));
            run.send(key(KeyCode::Esc));
            let request = match sent.next().await {
                ClientCommand::RewindPoints { request } => request,
                other => panic!("expected RewindPoints, got {other:?}"),
            };
            events
                .send(AgentEvent::RewindPoints {
                    answers: request,
                    points: vec![rewind_point(1, "compare them")],
                })
                .await
                .unwrap();
            until(&run.screen, "cache warm").await;
            run.send(key(KeyCode::Enter));
            until(&run.screen, copy::FILES_STAY).await;
            run.send(key(KeyCode::Enter));
            let request = match sent.next().await {
                ClientCommand::Rewind { request, .. } => request,
                other => panic!("expected Rewind, got {other:?}"),
            };
            let returned = ReturnedInput {
                id: if held {
                    id
                } else {
                    InputId("resumed-1".to_owned())
                },
                text: "compare them".to_owned(),
                images: [&images[0], &images[2]]
                    .into_iter()
                    .map(|image| ImageData {
                        data: wisp_media::encode_base64(image).into(),
                    })
                    .collect(),
                missing: vec![MissingImage {
                    ordinal: 2,
                    sha256: "b".repeat(64),
                }],
            };
            events
                .send(AgentEvent::Rewound {
                    answers: Some(request),
                    point: RewindPoint::Turn { turn: 1 },
                    reason: RewindReason::User,
                    dropped: Dropped {
                        turns: vec![1],
                        messages: 2,
                    },
                    returned: vec![returned],
                    cache: CacheEffect::Kept,
                })
                .await
                .unwrap();
            let lines = until(&run.screen, &copy::image_not_restored(&[2])).await;
            *kept.borrow_mut() = lines;
            let caret = run.screen.caret_line();
            assert!(caret.contains("compare them"), "{caret:?}");
            run.send(ctrl('c'));
            until(&run.screen, "Ready").await;
            let lines = run.screen.visible();
            assert!(
                !lines
                    .iter()
                    .any(|l| l.contains(&copy::image_not_restored(&[2]))),
                "{lines:#?}"
            );
            run.quit();
        },
    )
    .await;
    seen.take()
}

fn rewind_point(turn: u64, label: &str) -> PointInfo {
    PointInfo {
        point: RewindPoint::Turn { turn },
        label: label.to_owned(),
        at: "2026-10-26T14:02:00Z".to_owned(),
        inputs_preview: Vec::new(),
        quote: RewindQuote {
            prefix_tokens: 1_000,
            warm: Warmth::Warm,
            read_micros: Some(40),
            rewrite_micros: Some(700),
        },
        compaction: None,
        processes: Vec::new(),
    }
}

fn composer_text(lines: &[String]) -> String {
    let top = lines.iter().rposition(|l| l.starts_with('\u{256d}'));
    let bottom = lines.iter().rposition(|l| l.starts_with('\u{2570}'));
    match (top, bottom) {
        (Some(top), Some(bottom)) if top < bottom => lines[top + 1..bottom].join("\n"),
        _ => String::new(),
    }
}

#[tokio::test]
async fn a_rewind_brings_back_images_one_and_three_and_names_the_refused_second() {
    for held in [true, false] {
        let lines = rewound_with_the_second_image_refused(held).await;
        let composer = composer_text(&lines);
        assert!(composer.contains("[Image #1 "), "{held}: {lines:#?}");
        assert!(composer.contains("[Image #2 "), "{held}: {lines:#?}");
        assert!(!composer.contains("[Image #3"), "{held}: {lines:#?}");
        assert!(composer.contains("compare them"), "{held}: {lines:#?}");
        if held {
            assert!(composer.contains("10\u{d7}10"), "{lines:#?}");
            assert!(composer.contains("30\u{d7}10"), "{lines:#?}");
            assert!(!composer.contains("20\u{d7}10"), "{lines:#?}");
        }
        let note = lines
            .iter()
            .position(|l| l.contains("image #2 wasn't restored: it couldn't be processed"));
        let rule = lines
            .iter()
            .rposition(|l| l.contains("[Image #1 "))
            .unwrap_or(0);
        assert!(note.is_some_and(|n| n < rule), "{held}: {lines:#?}");
    }
}
