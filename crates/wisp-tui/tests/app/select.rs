use wisp_protocol::{AgentPath, AgentState, ClientCommand};
use wisp_render::theme::Token;
use wisp_render::transcript::copy;
use wisp_tui::app::{
    AGAIN_TO_QUIT, NOTHING_TO_COPY, TERMINAL_PASTE_WINDOW, TOAST, copies_hint, handed_off_line,
};
use wisp_tui::term::clipboard::{
    Clipboard, Host, Native, POWERSHELL, Route, SCREEN_BLOCKS, Tools, right_click_pastes,
    terminal_pastes_on_right_click,
};

use super::agent_keys::watch;
use super::scratch::ScratchDir;
use super::*;
use crate::spots::row_spot;

const PBCOPY: &str = r#"#!/bin/sh
d="$(dirname "$0")"
if [ -t 0 ] || [ -t 1 ] || [ -t 2 ]; then echo tty >> "$d/tty"; fi
t="$(/bin/cat)"
while [ -f "$d/hold" ]; do /bin/sleep 0.02; done
printf '%s\n--\n' "$t" >> "$d/copied"
"#;

pub(super) struct Fake {
    dir: ScratchDir,
}

impl Fake {
    pub(super) fn new(name: &str) -> Fake {
        Fake::with(name, Some(PBCOPY))
    }

    fn with(name: &str, script: Option<&str>) -> Fake {
        let dir = ScratchDir::new(SCRATCH, name);
        if let Some(script) = script {
            fake_tool::install(&dir.root.join("pbcopy"), script);
        }
        Fake { dir }
    }

    pub(super) fn install_windows(&self) {
        fake_tool::install(
            &self.dir.root.join(POWERSHELL),
            include_str!("../support/windows-clipboard.sh"),
        );
    }

    pub(super) fn clipboard(&self) -> Clipboard {
        Clipboard {
            route: Route::Local {
                tmux: false,
                natives: vec![Native::Mac],
            },
            tools: Tools {
                path: Some(self.dir.root.clone().into_os_string()),
                powershell: vec![],
                setsid: None,
                timeout: Duration::from_secs(5),
            },
        }
    }

    pub(super) fn copies(&self) -> Vec<String> {
        let all = std::fs::read_to_string(self.dir.root.join("copied")).unwrap_or_default();
        let mut copies: Vec<String> = all.split("\n--\n").map(str::to_owned).collect();
        copies.pop();
        if let Ok(windows) = std::fs::read_to_string(self.dir.root.join("windows-copied")) {
            copies.extend(windows.lines().map(|frame| {
                String::from_utf8(wisp_media::decode_base64(frame).unwrap()).unwrap()
            }));
        }
        copies
    }

    pub(super) fn hold(&self) {
        std::fs::write(self.dir.root.join("hold"), "").unwrap();
    }

    pub(super) fn release(&self) {
        std::fs::remove_file(self.dir.root.join("hold")).unwrap();
    }

    fn touched_a_tty(&self) -> bool {
        self.dir.root.join("tty").exists()
    }
}

fn copying(fake: &Fake, on_release: bool) -> Options {
    let mut opts = options(Start::Stalled);
    opts.clipboard = fake.clipboard();
    opts.copy_on_release = on_release;
    opts
}

pub(super) fn at(kind: MouseKind, column: u16, row: u16) -> Input {
    Input::Mouse(Mouse {
        kind,
        column,
        row,
        modified: false,
    })
}

pub(super) fn locate(screen: &Screen, needle: &str) -> (u16, u16) {
    let rows = screen.visible();
    row_spot(&rows, needle).unwrap_or_else(|| panic!("{needle:?} is not on screen: {rows:#?}"))
}

pub(super) fn account_chip_spot(screen: &Screen, name: &str) -> (u16, u16) {
    let lines = screen.visible();
    let row = lines.iter().rposition(|line| line.contains(name)).unwrap();
    let prefix = &lines[row][..lines[row].rfind(name).unwrap()];
    let column = u16::try_from(prefix.chars().count()).unwrap();
    (column, u16::try_from(row).unwrap())
}

pub(super) fn account_chip(a: &super::steering::Agent, name: &str) {
    let (column, row) = account_chip_spot(&a.screen, name);
    super::folds::click(a, column, row);
}

fn tint() -> vt100::Color {
    let rgb = Theme::new(Mode::Dark, Depth::TrueColor).rgb(Token::AccentTint);
    vt100::Color::Rgb(rgb.r, rgb.g, rgb.b)
}

pub(super) fn tinted(screen: &Screen) -> Vec<String> {
    let state = screen.0.borrow();
    let s = state.parser.screen();
    let (rows, cols) = s.size();
    let mut out = vec![];
    for row in 0..rows.get() {
        let mut text = String::new();
        for col in 0..cols.get() {
            if let Some(cell) = s.cell(row, col)
                && cell.bgcolor() == tint()
            {
                text.push_str(if cell.has_contents() {
                    cell.contents()
                } else {
                    " "
                });
            }
        }
        if !text.is_empty() {
            out.push(text);
        }
    }
    out
}

pub(crate) fn status_row(screen: &Screen) -> String {
    super::status_line(&screen.visible())
}

async fn copied(run: &Run, fake: &Fake, count: usize) {
    copied_on(&run.screen, fake, count).await;
}

async fn copied_on(screen: &Screen, fake: &Fake, count: usize) {
    let copies = super::composer_box::copied(fake, count).await;
    let toast = || status_row(screen).contains("Copied");
    eventually(&toast).await;
    assert!(
        copies.len() >= count && toast(),
        "no copy toast: {:#?} {copies:?}",
        screen.visible()
    );
}

const PROSE: &str = "Alpha bravo charlie delta echo foxtrot golf hotel india juliett kilo lima \
mike november oscar papa quebec romeo sierra tango uniform victor whiskey xray yankee zulu.";

fn prose_turn() -> Vec<AgentEvent> {
    answer([format!("{PROSE}\n\nSecond paragraph stays apart.\n")])
}

async fn asked(run: &Run) {
    sleep(Duration::from_millis(100)).await;
    run.say("hi");
    sleep(Duration::from_secs(3)).await;
}

#[tokio::test(start_paused = true)]
async fn a_drag_highlights_the_text_and_copies_it_on_release_by_default() {
    let fake = Fake::new("release");
    let screen = Screen::new(24, 60);
    let exit = session_with(
        screen.clone(),
        vec![prose_turn()],
        copying(&fake, true),
        |run| async move {
            asked(&run).await;
            let (col, row) = locate(&run.screen, "charlie");
            let (end_col, end_row) = locate(&run.screen, "Second");
            run.send(at(MouseKind::Press, col, row));
            run.send(at(MouseKind::Drag, col + 3, row));
            run.send(at(MouseKind::Drag, end_col + 5, end_row));
            sleep(Duration::from_millis(50)).await;
            let shown = tinted(&run.screen);
            assert!(
                shown.first().is_some_and(|t| t.starts_with("charlie")),
                "{shown:#?}"
            );
            assert!(
                shown.last().is_some_and(|t| t.ends_with("Second")),
                "{shown:#?}"
            );
            assert!(fake.copies().is_empty());
            run.send(at(MouseKind::Release, end_col + 5, end_row));
            copied(&run, &fake, 1).await;
            let want = format!("{}\n\nSecond", &PROSE[PROSE.find("charlie").unwrap()..]);
            assert_eq!(fake.copies(), vec![want.clone()]);
            let toast = status_row(&run.screen);
            assert!(
                toast.contains(&format!(
                    "Copied {} characters \u{b7} clipboard",
                    want.chars().count()
                )),
                "{toast:?}"
            );
            assert!(!tinted(&run.screen).is_empty(), "the selection stays shown");
            sleep(TOAST + Duration::from_millis(100)).await;
            assert!(status_row(&run.screen).contains(&copy::copy_key().text()));
            assert!(!status_row(&run.screen).contains("Copied"));
            run.send(key(KeyCode::Esc));
            sleep(Duration::from_millis(50)).await;
            assert!(tinted(&run.screen).is_empty());
            assert!(!status_row(&run.screen).contains(&copy::copy_key().text()));
            assert!(!fake.touched_a_tty());
            run.send(ctrl('d'));
        },
    )
    .await;
    assert!(exit.is_ok(), "{exit:?}");
}

#[tokio::test(start_paused = true)]
async fn a_drag_in_an_agents_chat_copies_its_text_not_mains() {
    let fake = Fake::new("agent-chat");
    steering::steering_on(
        Screen::new(24, 60),
        copying(&fake, true),
        |mut a| async move {
            a.start("main works too", 1).await;
            let worker = agents::entry("/w", AgentState::Running, 0.2);
            a.emit(agents::spawned(worker)).await;
            let mut turn = prose_turn();
            turn.pop();
            for event in turn {
                let event = Box::new(event);
                let agent = AgentPath::new("/w");
                a.emit(AgentEvent::Agent { agent, event }).await;
            }
            let unselected = |screen: &Screen| {
                let status = status_row(screen);
                let copy_key = copy::copy_key().text();
                assert!(
                    tinted(screen).is_empty() && !status.contains(&copy_key),
                    "{status:?}"
                );
            };
            let (col, row) = locate(&a.screen, "working on it");
            a.input(at(MouseKind::Press, col, row));
            a.input(at(MouseKind::Drag, col + 7, row));
            a.input(at(MouseKind::Release, col + 7, row));
            copied_on(&a.screen, &fake, 1).await;
            a.ctrl('t').key(KeyCode::Down).key(KeyCode::Enter);
            assert_eq!(a.next().await, watch("/w", true));
            sleep(Duration::from_millis(100)).await;
            unselected(&a.screen);
            let (col, row) = locate(&a.screen, "charlie");
            let (end_col, end_row) = locate(&a.screen, "Second");
            a.input(at(MouseKind::Press, col, row));
            a.input(at(MouseKind::Drag, end_col + 5, end_row));
            a.input(at(MouseKind::Release, end_col + 5, end_row));
            copied_on(&a.screen, &fake, 2).await;
            let want = format!("{}\n\nSecond", &PROSE[PROSE.find("charlie").unwrap()..]);
            assert_eq!(fake.copies()[1], want);
            sleep(TOAST + Duration::from_millis(100)).await;
            let status = status_row(&a.screen);
            assert!(status.contains(&copy::copy_key().text()), "{status:?}");
            assert!(!status.contains(&copy::esc_to_stop().text()), "{status:?}");
            a.ctrl('t');
            assert_eq!(a.next().await, watch("/w", false));
            a.key(KeyCode::Esc);
            sleep(Duration::from_millis(100)).await;
            unselected(&a.screen);
            a.ctrl('t').key(KeyCode::Down).key(KeyCode::Enter);
            assert_eq!(a.next().await, watch("/w", true));
            sleep(Duration::from_millis(100)).await;
            unselected(&a.screen);
            a.key(KeyCode::Esc);
            assert_eq!(a.next().await, watch("/w", false));
            a.ctrl('c');
            assert_eq!(a.next().await, ClientCommand::Cancel);
            a.emit(steering::cancelled()).await;
            a.ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn without_copy_on_release_ctrl_c_copies_then_clears_and_only_then_arms_quit() {
    let fake = Fake::new("ctrl-c");
    let screen = Screen::new(24, 60);
    session_with(
        screen.clone(),
        vec![prose_turn()],
        copying(&fake, false),
        |run| async move {
            asked(&run).await;
            let (col, row) = locate(&run.screen, "delta");
            run.send(at(MouseKind::Press, col, row));
            run.send(at(MouseKind::Drag, col + 9, row));
            run.send(at(MouseKind::Release, col + 9, row));
            real_wait(260).await;
            assert!(fake.copies().is_empty(), "release copied");
            assert_eq!(tinted(&run.screen), vec!["delta echo".to_owned()]);
            assert!(status_row(&run.screen).contains(&copy::copy_key().text()));
            assert!(!status_row(&run.screen).contains(&copy::esc_to_stop().text()));
            run.send(ctrl('c'));
            copied(&run, &fake, 1).await;
            assert_eq!(fake.copies(), vec!["delta echo".to_owned()]);
            assert!(
                tinted(&run.screen).is_empty(),
                "ctrl-c clears the selection"
            );
            sleep(TOAST + Duration::from_millis(100)).await;
            assert!(!status_row(&run.screen).contains(&copy::copy_key().text()));
            run.send(ctrl('c'));
            sleep(Duration::from_millis(50)).await;
            assert!(status_row(&run.screen).contains(AGAIN_TO_QUIT));
            run.send(ctrl('c'));
        },
    )
    .await
    .unwrap();
}

fn unfinished() -> Vec<AgentEvent> {
    vec![
        AgentEvent::turn_started(1),
        AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Text,
        },
        AgentEvent::TextDelta {
            index: 0,
            text: format!("{PROSE}\n\nStill going"),
        },
    ]
    .into_iter()
    .chain((0..3000).map(|_| AgentEvent::TextDelta {
        index: 0,
        text: " w".to_owned(),
    }))
    .collect()
}

fn growing() -> Vec<AgentEvent> {
    let mut turn = answer([format!("{PROSE}\n\nNext")]);
    let end = turn.split_off(turn.len() - 2);
    turn.extend((0..200).map(|_| AgentEvent::TextDelta {
        index: 0,
        text: " w".to_owned(),
    }));
    turn.extend(end);
    turn
}

#[tokio::test(start_paused = true)]
async fn while_a_turn_runs_ctrl_c_copies_and_the_hint_says_esc_stops() {
    let fake = Fake::new("running");
    let screen = Screen::new(24, 60);
    session_with(
        screen.clone(),
        vec![unfinished()],
        copying(&fake, false),
        |run| async move {
            asked(&run).await;
            let (col, row) = locate(&run.screen, "golf");
            run.send(at(MouseKind::Press, col, row));
            run.send(at(MouseKind::Drag, col + 3, row));
            run.send(at(MouseKind::Release, col + 3, row));
            sleep(Duration::from_millis(50)).await;
            let status = status_row(&run.screen);
            assert!(status.contains(&copies_hint(true, "\u{b7}")), "{status:?}");
            let stop = copy::esc_to_stop().text();
            assert!(status.contains(&stop), "{status:?}");
            run.send(ctrl('c'));
            copied(&run, &fake, 1).await;
            assert_eq!(fake.copies(), vec!["golf".to_owned()]);
            sleep(TOAST + Duration::from_millis(100)).await;
            assert!(
                status_row(&run.screen).contains(&stop),
                "ctrl-c stopped the turn: {:?}",
                status_row(&run.screen)
            );
            run.send(key(KeyCode::Esc));
            sleep(Duration::from_millis(500)).await;
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn double_and_triple_clicks_select_a_word_and_a_row() {
    let fake = Fake::new("clicks");
    let screen = Screen::new(24, 60);
    session_with(
        screen.clone(),
        vec![prose_turn()],
        copying(&fake, true),
        |run| async move {
            asked(&run).await;
            let (col, row) = locate(&run.screen, "foxtrot");
            for _ in 0..2 {
                run.send(at(MouseKind::Press, col + 2, row));
                run.send(at(MouseKind::Release, col + 2, row));
            }
            copied(&run, &fake, 1).await;
            assert_eq!(tinted(&run.screen), vec!["foxtrot".to_owned()]);
            sleep(Duration::from_secs(1)).await;
            for _ in 0..3 {
                run.send(at(MouseKind::Press, col + 2, row));
                run.send(at(MouseKind::Release, col + 2, row));
            }
            let line = run.screen.visible()[usize::from(row)].clone();
            let content = line.trim().trim_start_matches('\u{2836}').trim_start();
            assert!(content.starts_with("Alpha"), "{content:?}");
            eventually(|| fake.copies().last().is_some_and(|c| c == content)).await;
            real_wait(200).await;
            let copies = fake.copies();
            assert_eq!(copies[0], "foxtrot");
            assert_eq!(
                copies.last().map(String::as_str),
                Some(content),
                "the row must be the last copy and carry no gutter: {copies:?}"
            );
            assert_eq!(tinted(&run.screen).len(), 1);
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_selection_stays_on_its_text_through_streaming_scrolling_and_height_changes() {
    let fake = Fake::new("survives");
    let screen = Screen::new(24, 60);
    let resized = screen.clone();
    session_with(
        screen.clone(),
        vec![growing()],
        copying(&fake, false),
        |run| async move {
            sleep(Duration::from_millis(100)).await;
            run.say("hi");
            sleep(Duration::from_millis(200)).await;
            let (col, row) = locate(&run.screen, "kilo");
            run.send(at(MouseKind::Press, col, row));
            run.send(at(MouseKind::Drag, col + 8, row));
            run.send(at(MouseKind::Release, col + 8, row));
            sleep(Duration::from_millis(50)).await;
            let want = vec!["kilo lima".to_owned()];
            assert_eq!(tinted(&run.screen), want);
            sleep(Duration::from_secs(3)).await;
            let rows = run.screen.visible();
            assert!(rows.iter().any(|l| l.ends_with(" w w")), "{rows:#?}");
            assert_ne!(locate(&run.screen, "kilo").1, row, "{rows:#?}");
            assert_eq!(tinted(&run.screen), want, "{rows:#?}");
            run.send(wheel(MouseKind::WheelUp));
            sleep(Duration::from_millis(50)).await;
            assert_eq!(tinted(&run.screen), want);
            let size = resized.resize(30, 60);
            run.send(Input::Resize(size));
            sleep(Duration::from_millis(300)).await;
            assert_eq!(tinted(&run.screen), want);
            let size = resized.resize(30, 70);
            run.send(Input::Resize(size));
            sleep(Duration::from_millis(300)).await;
            assert!(tinted(&run.screen).is_empty(), "a width change keeps it");
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn esc_typing_and_a_click_elsewhere_clear_a_selection() {
    let fake = Fake::new("clears");
    let screen = Screen::new(24, 60);
    session_with(
        screen.clone(),
        vec![prose_turn()],
        copying(&fake, false),
        |run| async move {
            asked(&run).await;
            let (col, row) = locate(&run.screen, "mike");
            let select = || {
                run.send(at(MouseKind::Press, col, row));
                run.send(at(MouseKind::Drag, col + 3, row));
                run.send(at(MouseKind::Release, col + 3, row));
            };
            select();
            sleep(Duration::from_millis(50)).await;
            assert!(!tinted(&run.screen).is_empty());
            run.send(key(KeyCode::Esc));
            sleep(Duration::from_millis(50)).await;
            assert!(tinted(&run.screen).is_empty(), "esc");
            select();
            sleep(Duration::from_millis(50)).await;
            run.send(key(KeyCode::Char('x')));
            sleep(Duration::from_millis(50)).await;
            assert!(tinted(&run.screen).is_empty(), "typing");
            assert!(run.screen.caret_line().contains('x'));
            select();
            sleep(Duration::from_millis(50)).await;
            run.send(at(MouseKind::Press, 2, 1));
            run.send(at(MouseKind::Release, 2, 1));
            sleep(Duration::from_millis(50)).await;
            assert!(tinted(&run.screen).is_empty(), "a click elsewhere");
            assert!(fake.copies().is_empty());
            run.send(ctrl('c'));
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn dragging_onto_the_top_row_scrolls_up_one_row_every_fifty_ms() {
    let fake = Fake::new("edge");
    let screen = Screen::new(19, 60);
    session_with(
        screen.clone(),
        numbered(6),
        copying(&fake, false),
        |run| async move {
            sleep(Duration::from_millis(100)).await;
            ask_all(&run, 6).await;
            let (col, row) = locate(&run.screen, "answer 5 begins");
            run.send(at(MouseKind::Press, col, row));
            run.send(at(MouseKind::Drag, col, 0));
            sleep(Duration::from_millis(30)).await;
            let before = run.screen.visible();
            sleep(Duration::from_millis(160)).await;
            let rows = run.screen.visible();
            assert_eq!(rows[3..9], before[..6], "three rows in 150 ms: {rows:#?}");
            let shown = tinted(&run.screen);
            assert!(shown.len() >= 8, "{shown:#?}");
            run.send(at(MouseKind::Release, col, 0));
            let held = run.screen.visible();
            sleep(Duration::from_millis(500)).await;
            assert_eq!(run.screen.visible(), held, "the scroll stops on release");
            run.send(ctrl('c'));
            copied(&run, &fake, 1).await;
            let copy = fake.copies().remove(0);
            assert!(copy.ends_with("\n\nquestion 5\n\na"), "{copy:?}");
            assert!(
                !copy.contains(prompt()) && !copy.contains('\u{2836}'),
                "{copy:?}"
            );
            assert!(copy.contains("answer 4 ends"), "{copy:?}");
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_click_in_the_composer_moves_the_caret_there() {
    let screen = Screen::new(24, 60);
    session(screen.clone(), vec![], Start::Stalled, |run| async move {
        sleep(Duration::from_millis(100)).await;
        run.send(Input::Paste("hello world".to_owned()));
        sleep(Duration::from_millis(50)).await;
        let (col, row) = locate(&run.screen, "world");
        run.send(at(MouseKind::Press, col, row));
        run.send(at(MouseKind::Release, col, row));
        run.send(key(KeyCode::Char('X')));
        sleep(Duration::from_millis(50)).await;
        assert!(
            run.screen.visible()[usize::from(row)].contains("hello Xworld"),
            "{:#?}",
            run.screen.visible()
        );
        run.send(at(MouseKind::Press, col + 30, row));
        run.send(key(KeyCode::Char('!')));
        sleep(Duration::from_millis(50)).await;
        assert!(run.screen.visible()[usize::from(row)].contains("hello Xworld!"));
        run.send(ctrl('c'));
        run.send(ctrl('d'));
    })
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn over_ssh_a_copy_goes_to_the_terminal_and_is_called_sent_not_copied() {
    let screen = Screen::new(24, 60);
    let mut opts = options(Start::Stalled);
    opts.clipboard = Clipboard {
        route: Route::Terminal,
        ..Clipboard::none()
    };
    opts.copy_on_release = false;
    session_with(screen.clone(), vec![prose_turn()], opts, |run| async move {
        asked(&run).await;
        let (col, row) = locate(&run.screen, "oscar");
        run.send(at(MouseKind::Press, col, row));
        run.send(at(MouseKind::Drag, col + 4, row));
        run.send(at(MouseKind::Release, col + 4, row));
        run.send(ctrl('c'));
        sleep(Duration::from_millis(50)).await;
        let status = status_row(&run.screen);
        assert!(
            status.contains("Sent 5 characters to your terminal (OSC 52)"),
            "{status:?}"
        );
        assert!(!status.contains("Copied"));
        run.send(ctrl('d'));
    })
    .await
    .unwrap();
    assert_eq!(
        screen.sent_to_clipboard(),
        vec![b"\x1b]52;c;b3NjYXI=\x07".to_vec()]
    );
    assert_eq!(screen.spied(), vec![format!("clipboard {:?}", b"b3NjYXI=")]);
}

#[tokio::test(start_paused = true)]
async fn a_copy_the_terminal_refuses_says_so_after_the_recovery() {
    let screen = Screen::new(24, 60);
    let mut opts = options(Start::Stalled);
    opts.clipboard = Clipboard {
        route: Route::Terminal,
        ..Clipboard::none()
    };
    opts.copy_on_release = false;
    session_with(screen.clone(), vec![prose_turn()], opts, |run| async move {
        asked(&run).await;
        let (col, row) = locate(&run.screen, "oscar");
        run.send(at(MouseKind::Press, col, row));
        run.send(at(MouseKind::Drag, col + 4, row));
        run.send(at(MouseKind::Release, col + 4, row));
        sleep(Duration::from_millis(50)).await;
        run.screen.refuse(Halted::Restored);
        run.send(ctrl('c'));
        sleep(Duration::from_millis(50)).await;
        let status = status_row(&run.screen);
        assert!(
            status.contains("Couldn't copy: the terminal couldn't be written"),
            "{status:?}"
        );
        run.send(ctrl('d'));
    })
    .await
    .unwrap();
    assert!(screen.sent_to_clipboard().is_empty());
    assert!(screen.reapplied() > 0);
}

#[tokio::test(start_paused = true)]
async fn a_blank_selection_says_there_is_nothing_to_copy() {
    let fake = Fake::new("blank");
    let screen = Screen::new(24, 60);
    session_with(
        screen.clone(),
        vec![prose_turn()],
        copying(&fake, false),
        |run| async move {
            asked(&run).await;
            let (_, row) = locate(&run.screen, "Second");
            run.send(at(MouseKind::Press, 40, row));
            run.send(at(MouseKind::Drag, 50, row));
            run.send(at(MouseKind::Release, 50, row));
            run.send(ctrl('c'));
            sleep(Duration::from_millis(50)).await;
            assert!(status_row(&run.screen).contains(NOTHING_TO_COPY));
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
    assert!(fake.copies().is_empty());
}

#[tokio::test(start_paused = true)]
async fn transcript_mode_selects_and_copies_and_shows_the_toast_in_its_hint_row() {
    let fake = Fake::new("pager");
    let screen = Screen::new(24, 60);
    session_with(
        screen.clone(),
        vec![prose_turn()],
        copying(&fake, false),
        |run| async move {
            asked(&run).await;
            run.send(ctrl('o'));
            sleep(Duration::from_millis(300)).await;
            assert!(run.screen.in_pager());
            let (col, row) = locate(&run.screen, "papa");
            run.send(at(MouseKind::Press, col, row));
            run.send(at(MouseKind::Drag, col + 10, row));
            run.send(at(MouseKind::Release, col + 10, row));
            sleep(Duration::from_millis(50)).await;
            assert_eq!(tinted(&run.screen), vec!["papa quebec".to_owned()]);
            assert!(status_row(&run.screen).contains(&copy::copy_key().text()));
            run.send(ctrl('c'));
            eventually(|| !fake.copies().is_empty() && status_row(&run.screen).contains("Copied"))
                .await;
            assert_eq!(fake.copies(), vec!["papa quebec".to_owned()]);
            assert!(run.screen.in_pager() || status_row(&run.screen).contains("Copied"));
            assert!(tinted(&run.screen).is_empty());
            sleep(TOAST + Duration::from_millis(100)).await;
            assert!(
                run.screen.in_pager(),
                "ctrl-c with a selection closed the pager"
            );
            run.send(key(KeyCode::Char('q')));
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn transcript_mode_copies_a_drag_that_scrolls_past_the_drawn_rows() {
    let fake = Fake::new("pager-edge");
    let screen = Screen::new(19, 60);
    session_with(
        screen.clone(),
        numbered(12),
        copying(&fake, false),
        |run| async move {
            sleep(Duration::from_millis(100)).await;
            ask_all(&run, 12).await;
            run.send(ctrl('o'));
            sleep(Duration::from_millis(300)).await;
            assert!(run.screen.in_pager());
            let (col, row) = locate(&run.screen, "answer 11 ends");
            run.send(at(MouseKind::Press, col, row));
            run.send(at(MouseKind::Drag, col, 0));
            sleep(Duration::from_millis(50 * 90)).await;
            run.send(at(MouseKind::Release, col, 0));
            run.send(ctrl('c'));
            copied(&run, &fake, 1).await;
            let copy = fake.copies().remove(0);
            assert!(copy.lines().count() > 3 * 18, "{copy:?}");
            let first = (0..12)
                .find(|i| copy.contains(&format!("answer {i} ends")))
                .unwrap();
            assert!(first < 6, "{copy:?}");
            for i in first + 1..12 {
                for part in [
                    format!("question {i}"),
                    format!("answer {i} begins"),
                    format!("middle of answer {i}"),
                ] {
                    assert!(copy.contains(&part), "{part}: {copy:?}");
                }
            }
            assert!(copy.ends_with("middle of answer 11\n\na"), "{copy:?}");
            run.send(key(KeyCode::Char('q')));
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn the_hand_off_prints_the_transcript_on_the_main_screen_and_esc_returns() {
    let screen = Screen::new(24, 60);
    session(
        screen.clone(),
        vec![prose_turn()],
        Start::Stalled,
        |run| async move {
            asked(&run).await;
            run.send(ctrl('o'));
            sleep(Duration::from_millis(300)).await;
            let frames = run.screen.frames();
            run.send(key(KeyCode::Char('[')));
            sleep(Duration::from_millis(100)).await;
            assert!(!run.screen.on_alternate());
            assert_eq!(run.screen.handed().len(), 1);
            let main = run.screen.visible();
            let last = main.iter().rposition(|l| !l.is_empty()).unwrap();
            assert_eq!(main[last].trim(), handed_off_line("\u{b7}"), "{main:#?}");
            assert!(words_in_order(&main, PROSE), "{main:#?}");
            assert!(main.iter().any(|l| l.contains("before")), "{main:#?}");
            run.send(wheel(MouseKind::WheelUp));
            run.send(at(MouseKind::Press, 3, 3));
            run.send(key(KeyCode::Char('x')));
            sleep(Duration::from_millis(300)).await;
            assert_eq!(run.screen.frames(), frames, "drew while handed off");
            assert_eq!(run.screen.visible(), main);
            run.send(key(KeyCode::Esc));
            sleep(Duration::from_millis(100)).await;
            assert_eq!(run.screen.taken_back(), 1);
            assert!(run.screen.on_alternate());
            assert!(run.screen.in_pager());
            assert!(run.screen.last_full());
            run.send(key(KeyCode::Char('[')));
            sleep(Duration::from_millis(100)).await;
            run.send(ctrl('o'));
            sleep(Duration::from_millis(100)).await;
            assert_eq!(run.screen.taken_back(), 2);
            run.send(key(KeyCode::Char('q')));
            sleep(Duration::from_millis(100)).await;
            assert!(!run.screen.in_pager());
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
    let main = screen.leave();
    assert!(words_in_order(&main, PROSE), "{main:#?}");
}

#[tokio::test(start_paused = true)]
async fn a_hand_off_right_after_a_resize_draws_nothing_until_it_returns() {
    let screen = Screen::new(24, 60);
    session(
        screen.clone(),
        vec![prose_turn()],
        Start::Stalled,
        |run| async move {
            asked(&run).await;
            run.send(ctrl('o'));
            sleep(Duration::from_millis(300)).await;
            let size = run.screen.resize(24, 50);
            run.send(Input::Resize(size));
            sleep(Duration::from_millis(30)).await;
            run.send(key(KeyCode::Char('[')));
            sleep(Duration::from_millis(100)).await;
            assert!(!run.screen.on_alternate());
            let sized = run.screen.sized();
            sleep(Duration::from_secs(2)).await;
            assert_eq!(run.screen.sized(), sized, "drew while handed off");
            run.send(key(KeyCode::Esc));
            sleep(Duration::from_millis(100)).await;
            assert!(run.screen.in_pager());
            run.send(key(KeyCode::Char('q')));
            sleep(Duration::from_millis(100)).await;
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn ctrl_c_while_handed_off_stops_a_running_reply() {
    let screen = Screen::new(24, 60);
    session(
        screen.clone(),
        vec![unfinished()],
        Start::Stalled,
        |run| async move {
            asked(&run).await;
            run.send(ctrl('o'));
            sleep(Duration::from_millis(300)).await;
            run.send(key(KeyCode::Char('[')));
            sleep(Duration::from_millis(100)).await;
            run.send(ctrl('c'));
            sleep(Duration::from_millis(500)).await;
            run.send(key(KeyCode::Char('q')));
            sleep(Duration::from_millis(100)).await;
            run.send(key(KeyCode::Char('q')));
            sleep(Duration::from_millis(100)).await;
            let status = status_row(&run.screen);
            assert!(!status.contains(&copy::esc_to_stop().text()), "{status:?}");
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn escape_sequences_stay_inert_when_selected_copied_and_handed_off() {
    let fake = Fake::new("hostile");
    let screen = Screen::new(60, 100);
    let hostile: String = CORPUS.iter().map(|s| format!("A{s}Z ")).collect();
    let mut turns = hostile_turns(&hostile);
    turns.truncate(1);
    session_with(
        screen.clone(),
        turns,
        copying(&fake, false),
        |run| async move {
            asked(&run).await;
            let rows = run.screen.visible();
            let first = rows.iter().position(|l| l.contains("pwned")).unwrap();
            let last = rows.iter().rposition(|l| l.contains("reversed")).unwrap();
            run.send(at(MouseKind::Press, 0, u16::try_from(first).unwrap()));
            run.send(at(MouseKind::Drag, 99, u16::try_from(last).unwrap()));
            run.send(at(MouseKind::Release, 99, u16::try_from(last).unwrap()));
            run.send(ctrl('c'));
            copied(&run, &fake, 1).await;
            let copy = fake.copies().remove(0);
            assert!(copy.contains('A') && copy.contains('Z'), "{copy:?}");
            assert!(
                !copy.chars().any(|c| c.is_control() && c != '\n'),
                "{copy:?}"
            );
            run.open_transcript().await;
            sleep(Duration::from_millis(100)).await;
            run.send(key(KeyCode::Char('[')));
            sleep(Duration::from_millis(100)).await;
            run.send(key(KeyCode::Esc));
            sleep(Duration::from_millis(100)).await;
            run.send(key(KeyCode::Char('q')));
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
    let handed = String::from_utf8_lossy(&screen.handed().concat()).into_owned();
    assert!(handed.contains("AZ") || handed.contains('A'), "{handed:?}");
    assert!(!handed.contains('\u{7}'), "{handed:?}");
    assert!(!handed.contains('\u{9b}'), "{handed:?}");
    for bad in ["]0;", "]2;", "]52;", "]8;", "P+q"] {
        assert!(
            !handed.contains(&format!("\u{1b}{bad}")),
            "{bad:?} in {handed:?}"
        );
    }
    assert_eq!(screen.spied(), Vec::<String>::new());
}

const PICKER_HEAD: &str = "type to filter";

fn picker_shown(screen: &Screen) -> bool {
    screen.visible().iter().any(|l| l.contains(PICKER_HEAD))
}

async fn select_word(run: &Run, word: &str) {
    let (col, row) = locate(&run.screen, word);
    let last = col + u16::try_from(word.chars().count()).unwrap() - 1;
    run.send(at(MouseKind::Press, col, row));
    run.send(at(MouseKind::Drag, last, row));
    run.send(at(MouseKind::Release, last, row));
    sleep(Duration::from_millis(50)).await;
}

#[tokio::test(start_paused = true)]
async fn a_click_or_drag_under_an_open_picker_closes_it_and_selects_nothing() {
    let fake = Fake::new("picker-click");
    let screen = Screen::new(24, 80);
    session_with(
        screen.clone(),
        vec![prose_turn()],
        copying(&fake, true),
        |run| async move {
            asked(&run).await;
            let (col, row) = locate(&run.screen, "Alpha");
            run.say("/model");
            sleep(Duration::from_millis(200)).await;
            assert!(picker_shown(&run.screen), "{:#?}", run.screen.visible());
            let (_, menu) = locate(&run.screen, PICKER_HEAD);
            run.send(at(MouseKind::Press, 4, menu + 1));
            run.send(at(MouseKind::Drag, 30, menu + 2));
            run.send(at(MouseKind::Release, 30, menu + 2));
            sleep(Duration::from_millis(100)).await;
            assert!(!picker_shown(&run.screen), "{:#?}", run.screen.visible());
            assert!(tinted(&run.screen).is_empty(), "{:#?}", tinted(&run.screen));
            run.say("/model");
            sleep(Duration::from_millis(200)).await;
            run.send(at(MouseKind::Press, col, row));
            run.send(at(MouseKind::Drag, col + 10, row));
            sleep(Duration::from_millis(50)).await;
            assert!(!picker_shown(&run.screen));
            assert!(tinted(&run.screen).is_empty(), "the closing drag selected");
            run.send(at(MouseKind::Release, col + 10, row));
            real_wait(160).await;
            assert!(fake.copies().is_empty(), "{:?}", fake.copies());
            select_word(&run, "Alpha").await;
            assert_eq!(tinted(&run.screen), vec!["Alpha".to_owned()]);
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn esc_closes_an_open_picker_first_and_then_clears_a_selection() {
    let fake = Fake::new("picker-esc");
    let screen = Screen::new(24, 80);
    session_with(
        screen.clone(),
        vec![prose_turn()],
        copying(&fake, false),
        |run| async move {
            asked(&run).await;
            select_word(&run, "bravo").await;
            assert_eq!(tinted(&run.screen), vec!["bravo".to_owned()]);
            run.say("/model");
            sleep(Duration::from_millis(200)).await;
            assert!(picker_shown(&run.screen), "{:#?}", run.screen.visible());
            assert!(
                tinted(&run.screen)
                    .iter()
                    .all(|row| row.starts_with(" \u{203A}")),
                "typing the command keeps it"
            );
            run.send(key(KeyCode::Esc));
            sleep(Duration::from_millis(50)).await;
            assert!(!picker_shown(&run.screen));
            select_word(&run, "charlie").await;
            run.say("/model");
            sleep(Duration::from_millis(200)).await;
            run.send(key(KeyCode::Esc));
            sleep(Duration::from_millis(50)).await;
            assert!(!picker_shown(&run.screen));
            select_word(&run, "delta").await;
            assert_eq!(tinted(&run.screen), vec!["delta".to_owned()]);
            run.send(key(KeyCode::Esc));
            sleep(Duration::from_millis(50)).await;
            assert!(tinted(&run.screen).is_empty());
            assert!(!picker_shown(&run.screen));
            assert!(fake.copies().is_empty());
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn ctrl_c_closes_an_open_picker_and_copies_once_it_is_closed() {
    let fake = Fake::new("picker-ctrl-c");
    let screen = Screen::new(24, 80);
    session_with(
        screen.clone(),
        vec![prose_turn()],
        copying(&fake, false),
        |run| async move {
            asked(&run).await;
            run.say("/model");
            sleep(Duration::from_millis(200)).await;
            assert!(picker_shown(&run.screen));
            run.send(ctrl('c'));
            sleep(Duration::from_millis(50)).await;
            assert!(!picker_shown(&run.screen));
            assert!(!status_row(&run.screen).contains(AGAIN_TO_QUIT));
            select_word(&run, "charlie").await;
            run.send(ctrl('c'));
            copied(&run, &fake, 1).await;
            assert_eq!(fake.copies(), vec!["charlie".to_owned()]);
            assert!(tinted(&run.screen).is_empty());
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_copy_toast_and_an_open_picker_share_the_screen_without_overlap() {
    let fake = Fake::new("picker-toast");
    let screen = Screen::new(24, 80);
    session_with(
        screen.clone(),
        vec![prose_turn()],
        copying(&fake, true),
        |run| async move {
            asked(&run).await;
            select_word(&run, "delta").await;
            copied(&run, &fake, 1).await;
            run.say("/model");
            sleep(Duration::from_millis(200)).await;
            let rows = run.screen.visible();
            assert!(picker_shown(&run.screen), "{rows:#?}");
            let status = status_row(&run.screen);
            assert!(
                status.contains("Copied 5 characters")
                    || !status.contains(&copy::copy_key().text()),
                "{status:?}"
            );
            let menu = rows.iter().position(|l| l.contains(PICKER_HEAD)).unwrap();
            let composer = composer_top(&rows);
            assert!(menu < composer, "{rows:#?}");
            assert!(
                rows[..composer].iter().all(|l| !l.contains("Copied")),
                "the toast drew over the picker: {rows:#?}"
            );
            sleep(TOAST + Duration::from_millis(200)).await;
            assert!(picker_shown(&run.screen));
            assert!(!status_row(&run.screen).contains("Copied"));
            assert!(!status_row(&run.screen).contains(&copy::copy_key().text()));
            run.send(key(KeyCode::Esc));
            sleep(Duration::from_millis(50)).await;
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_selection_hint_never_hides_a_picker_result() {
    let fake = Fake::new("picker-hint");
    let screen = Screen::new(24, 80);
    session_with(
        screen.clone(),
        vec![prose_turn()],
        copying(&fake, false),
        |run| async move {
            asked(&run).await;
            select_word(&run, "echo").await;
            assert!(status_row(&run.screen).contains(&copy::copy_key().text()));
            run.say("/effort extreme");
            sleep(Duration::from_millis(100)).await;
            let status = status_row(&run.screen);
            assert!(!status.contains(&copy::copy_key().text()), "{status:?}");
            assert!(status.contains("extreme"), "{status:?}");
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_copy_disarms_a_pending_quit() {
    let fake = Fake::new("disarm");
    let screen = Screen::new(24, 60);
    let exit = session_with(
        screen.clone(),
        vec![prose_turn()],
        copying(&fake, false),
        |run| async move {
            asked(&run).await;
            run.send(ctrl('c'));
            sleep(Duration::from_millis(50)).await;
            assert!(status_row(&run.screen).contains(AGAIN_TO_QUIT));
            select_word(&run, "hotel").await;
            assert!(!status_row(&run.screen).contains(AGAIN_TO_QUIT));
            run.send(ctrl('c'));
            copied(&run, &fake, 1).await;
            run.send(ctrl('c'));
            sleep(Duration::from_millis(50)).await;
            assert!(
                status_row(&run.screen).contains(AGAIN_TO_QUIT),
                "a ctrl-c after a copy quit at once: {:?}",
                status_row(&run.screen)
            );
            run.send(key(KeyCode::Char('x')));
            sleep(Duration::from_millis(50)).await;
            run.send(ctrl('c'));
            sleep(Duration::from_millis(50)).await;
            run.send(at(MouseKind::Press, 2, 1));
            run.send(at(MouseKind::Release, 2, 1));
            sleep(Duration::from_millis(50)).await;
            assert!(!status_row(&run.screen).contains(AGAIN_TO_QUIT));
            run.send(ctrl('d'));
        },
    )
    .await;
    assert!(matches!(exit, Ok(Exit::Quit)), "{exit:?}");
}

async fn failed_toast(script: Option<&str>, name: &str) -> String {
    let fake = Fake::with(name, script);
    let screen = Screen::new(24, 80);
    let shown = Rc::new(RefCell::new(String::new()));
    let seen = Rc::clone(&shown);
    session_with(
        screen.clone(),
        vec![prose_turn()],
        copying(&fake, false),
        |run| async move {
            asked(&run).await;
            select_word(&run, "india").await;
            run.send(ctrl('c'));
            eventually(|| status_row(&run.screen).contains("Couldn't copy")).await;
            *seen.borrow_mut() = status_row(&run.screen);
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
    shown.take()
}

#[tokio::test(start_paused = true)]
async fn a_failing_writer_and_a_missing_one_each_toast_why() {
    let failing = failed_toast(Some("#!/bin/sh\n/bin/cat > /dev/null\nexit 4\n"), "fails").await;
    assert!(
        failing.contains("Couldn't copy: pbcopy failed (exit 4)"),
        "{failing:?}"
    );
    let missing = failed_toast(None, "missing").await;
    assert!(
        missing.contains("Couldn't copy: pbcopy wasn't found"),
        "{missing:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn inside_gnu_screen_over_ssh_a_copy_explains_instead_of_sending() {
    let screen = Screen::new(24, 100);
    let mut opts = options(Start::Stalled);
    opts.clipboard = Clipboard {
        route: Route::Blocked(SCREEN_BLOCKS),
        ..Clipboard::none()
    };
    opts.copy_on_release = false;
    session_with(screen.clone(), vec![prose_turn()], opts, |run| async move {
        asked(&run).await;
        select_word(&run, "juliett").await;
        run.send(ctrl('c'));
        sleep(Duration::from_millis(50)).await;
        let status = status_row(&run.screen);
        assert!(status.contains("Couldn't copy: GNU screen"), "{status:?}");
        run.send(ctrl('d'));
    })
    .await
    .unwrap();
    assert!(screen.sent_to_clipboard().is_empty());
}

#[tokio::test(start_paused = true)]
async fn esc_in_transcript_mode_clears_a_selection_before_it_closes() {
    let fake = Fake::new("pager-esc");
    let screen = Screen::new(24, 60);
    session_with(
        screen.clone(),
        vec![prose_turn()],
        copying(&fake, false),
        |run| async move {
            asked(&run).await;
            run.send(ctrl('o'));
            sleep(Duration::from_millis(300)).await;
            select_word(&run, "kilo").await;
            assert_eq!(tinted(&run.screen), vec!["kilo".to_owned()]);
            run.send(key(KeyCode::Esc));
            sleep(Duration::from_millis(100)).await;
            assert!(run.screen.in_pager(), "esc closed with a selection showing");
            assert!(tinted(&run.screen).is_empty());
            run.send(key(KeyCode::Esc));
            sleep(Duration::from_millis(100)).await;
            assert!(!run.screen.in_pager());
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

fn pager_range(screen: &Screen) -> String {
    status_row(screen)
        .split_whitespace()
        .find(|t| t.starts_with(|c: char| c.is_ascii_digit()))
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test(start_paused = true)]
async fn the_mouse_during_a_hand_off_changes_nothing_in_wisp() {
    let fake = Fake::new("handed-mouse");
    let screen = Screen::new(12, 60);
    session_with(
        screen.clone(),
        numbered(3),
        copying(&fake, true),
        |run| async move {
            sleep(Duration::from_millis(100)).await;
            ask_all(&run, 3).await;
            run.send(ctrl('o'));
            sleep(Duration::from_millis(300)).await;
            run.send(key(KeyCode::PageUp));
            sleep(Duration::from_millis(100)).await;
            select_word(&run, "answer").await;
            copied(&run, &fake, 1).await;
            sleep(TOAST + Duration::from_millis(100)).await;
            let range = pager_range(&run.screen);
            let tint = tinted(&run.screen);
            assert_eq!(tint, vec!["answer".to_owned()]);
            run.send(key(KeyCode::Char('[')));
            sleep(Duration::from_millis(100)).await;
            assert!(!run.screen.on_alternate());
            for kind in [MouseKind::WheelUp, MouseKind::WheelUp, MouseKind::WheelDown] {
                run.send(wheel(kind));
            }
            run.send(at(MouseKind::Press, 5, 2));
            run.send(at(MouseKind::Drag, 20, 4));
            run.send(at(MouseKind::Release, 20, 4));
            real_wait(360).await;
            run.send(key(KeyCode::Char('q')));
            sleep(Duration::from_millis(200)).await;
            assert!(run.screen.in_pager());
            assert_eq!(
                pager_range(&run.screen),
                range,
                "{:#?}",
                run.screen.visible()
            );
            assert!(
                tinted(&run.screen).is_empty(),
                "the hand-off keeps no selection, and the mouse made one"
            );
            assert_eq!(fake.copies(), vec!["answer".to_owned()]);
            run.send(key(KeyCode::Char('q')));
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

fn quoted_turn() -> Vec<AgentEvent> {
    answer([format!("> {PROSE}\n\nAfter the quote.\n")])
}

fn thinking_turn() -> Vec<AgentEvent> {
    vec![
        AgentEvent::turn_started(1),
        AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Thinking,
        },
        AgentEvent::ThinkingDelta {
            index: 0,
            text: PROSE.to_owned(),
        },
        AgentEvent::BlockStopped { index: 0 },
    ]
    .into_iter()
    .chain(answer(["Done thinking.\n".to_owned()]).into_iter().skip(1))
    .collect()
}

async fn copy_rows_between(run: &Run, fake: &Fake, first: &str, last: &str) -> String {
    let (_, top) = locate(&run.screen, first);
    let (_, bottom) = locate(&run.screen, last);
    run.send(at(MouseKind::Press, 0, top));
    run.send(at(MouseKind::Drag, 59, bottom));
    run.send(at(MouseKind::Release, 59, bottom));
    run.send(ctrl('c'));
    copied(run, fake, 1).await;
    fake.copies().remove(0)
}

#[tokio::test(start_paused = true)]
async fn a_wrapped_blockquote_copies_as_one_line_without_its_bars() {
    let fake = Fake::new("quote");
    let screen = Screen::new(24, 60);
    session_with(
        screen.clone(),
        vec![quoted_turn()],
        copying(&fake, false),
        |run| async move {
            asked(&run).await;
            let copy = copy_rows_between(&run, &fake, "Alpha", "zulu").await;
            let words = PROSE.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(
                copy.ends_with(&words),
                "{copy:?} {:#?}",
                run.screen.visible()
            );
            assert_eq!(copy.matches('\n').count(), 0, "{copy:?}");
            assert_eq!(
                copy.len() - words.len(),
                copy.find("Alpha").unwrap_or(usize::MAX),
                "{copy:?}"
            );
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn wrapped_thinking_copies_as_one_line_without_its_rule_in_transcript_mode() {
    let fake = Fake::new("thinking");
    let screen = Screen::new(30, 60);
    session_with(
        screen.clone(),
        vec![thinking_turn()],
        copying(&fake, false),
        |run| async move {
            asked(&run).await;
            let main = run.screen.visible();
            let folded: Vec<&String> = main.iter().filter(|r| r.contains('\u{254E}')).collect();
            assert_eq!(folded.len(), 4, "{main:#?}");
            assert!(folded[0].ends_with("Thought for 1 s"), "{main:#?}");
            assert!(folded[3].contains("zulu."), "{main:#?}");
            run.open_transcript().await;
            sleep(Duration::from_millis(100)).await;
            assert!(run.screen.in_pager());
            let copy = copy_rows_between(&run, &fake, "Alpha", "zulu").await;
            let words = PROSE.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(
                copy.ends_with(&words),
                "{copy:?} {:#?}",
                run.screen.visible()
            );
            assert_eq!(copy.matches('\n').count(), 0, "{copy:?}");
            run.send(key(KeyCode::Char('q')));
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_click_jumps_the_welcome_reveal_to_its_final_frame_as_a_key_does() {
    let ring = "\u{2850}\u{2808} \u{2801}\u{2801}";
    let screen = Screen::new(24, 80);
    session(screen.clone(), vec![], Start::Stalled, |run| async move {
        sleep(Duration::from_millis(10)).await;
        assert!(!run.screen.visible()[2].contains(ring));
        run.send(at(MouseKind::Press, 40, 12));
        run.send(at(MouseKind::Release, 40, 12));
        sleep(Duration::from_millis(40)).await;
        let rows = run.screen.visible();
        assert!(rows[2].contains(ring), "{rows:#?}");
        run.send(ctrl('d'));
    })
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn the_welcome_panel_copies_its_words_without_the_box_or_the_orb() {
    let fake = Fake::new("welcome");
    let screen = Screen::new(24, 80);
    session_with(
        screen.clone(),
        vec![],
        copying(&fake, false),
        |run| async move {
            sleep(Duration::from_millis(500)).await;
            let rows = run.screen.visible();
            let bottom = rows.iter().position(|l| l.starts_with('\u{2570}')).unwrap();
            let keys = u16::try_from(bottom + 1).unwrap();
            run.send(at(MouseKind::Press, 0, 0));
            run.send(at(MouseKind::Drag, 79, keys));
            run.send(at(MouseKind::Release, 79, keys));
            run.send(ctrl('c'));
            copied(&run, &fake, 1).await;
            let copy = fake.copies().remove(0);
            let lines: Vec<&str> = copy.lines().collect();
            assert!(
                lines
                    .first()
                    .is_some_and(|l| l.starts_with("wisp") && l.ends_with("0.0.0")),
                "{copy:?}"
            );
            assert_eq!(lines.first(), Some(&"wisp 0.0.0"), "{copy:?}");
            assert!(lines.iter().all(|l| !l.is_empty()), "{copy:?}");
            let tagline = format!("{} {}", copy::TAGLINE_OWN, copy::TAGLINE_COMPUTER);
            assert!(lines.contains(&tagline.as_str()), "{copy:?}");
            assert_eq!(
                lines.last(),
                Some(&"enter send \u{b7} ctrl-j new line \u{b7} /help \u{b7} /model switch model"),
                "{copy:?}"
            );
            for chrome in ['\u{2502}', '\u{256d}', '\u{2570}', '\u{2500}'] {
                assert!(!copy.contains(chrome), "{chrome:?} in {copy:?}");
            }
            assert!(
                !copy.chars().any(|c| ('\u{2800}'..='\u{28ff}').contains(&c)),
                "the orb was copied: {copy:?}"
            );
            let (col, row) = locate(&run.screen, copy::TAGLINE_OWN);
            for _ in 0..3 {
                run.send(at(MouseKind::Press, col + 2, row));
                run.send(at(MouseKind::Release, col + 2, row));
            }
            run.send(ctrl('c'));
            copied(&run, &fake, 2).await;
            assert_eq!(
                fake.copies().last().map(String::as_str),
                Some(tagline.as_str())
            );
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn the_compact_banner_copies_from_its_wordmark_without_the_mark() {
    let fake = Fake::new("compact");
    let screen = Screen::new(24, 40);
    session_with(
        screen.clone(),
        vec![],
        copying(&fake, false),
        |run| async move {
            sleep(Duration::from_millis(500)).await;
            let (col, row) = locate(&run.screen, "wisp");
            for _ in 0..3 {
                run.send(at(MouseKind::Press, col + 1, row));
                run.send(at(MouseKind::Release, col + 1, row));
            }
            run.send(ctrl('c'));
            copied(&run, &fake, 1).await;
            let copy = fake.copies().remove(0);
            assert!(
                copy.starts_with("wisp 0.0.0"),
                "{copy:?} {:#?}",
                run.screen.visible()
            );
            assert!(
                !copy.chars().any(|c| ('\u{2800}'..='\u{28ff}').contains(&c)),
                "{copy:?}"
            );
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn the_key_line_copies_without_its_lead() {
    let fake = Fake::new("key-line");
    let screen = Screen::new(24, 80);
    session_with(
        screen.clone(),
        vec![],
        copying(&fake, false),
        |run| async move {
            sleep(Duration::from_millis(500)).await;
            let (_, row) = locate(&run.screen, "enter send");
            for _ in 0..3 {
                run.send(at(MouseKind::Press, 4, row));
                run.send(at(MouseKind::Release, 4, row));
            }
            run.send(ctrl('c'));
            copied(&run, &fake, 1).await;
            assert_eq!(
                fake.copies().last().map(String::as_str),
                Some("enter send \u{b7} ctrl-j new line \u{b7} /help \u{b7} /model switch model")
            );
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn the_welcome_name_row_copies_without_its_padding_at_every_card_width() {
    for cols in [44u16, 50, 59, 60, 80, 120] {
        let fake = Fake::new("welcome-name");
        let screen = Screen::new(24, cols);
        session_with(
            screen.clone(),
            vec![],
            copying(&fake, false),
            |run| async move {
                sleep(Duration::from_millis(500)).await;
                let (_, row) = locate(&run.screen, "wisp");
                run.send(at(MouseKind::Press, 0, row));
                run.send(at(MouseKind::Drag, cols - 1, row));
                run.send(at(MouseKind::Release, cols - 1, row));
                run.send(ctrl('c'));
                copied(&run, &fake, 1).await;
                assert_eq!(fake.copies().remove(0), "wisp 0.0.0", "{cols} columns");
                let (col, row) = locate(&run.screen, "0.0.0");
                run.send(at(MouseKind::Press, col.saturating_sub(3), row));
                run.send(at(MouseKind::Drag, cols - 1, row));
                run.send(at(MouseKind::Release, cols - 1, row));
                run.send(ctrl('c'));
                copied(&run, &fake, 2).await;
                assert_eq!(fake.copies().remove(1), "0.0.0", "{cols} columns");
                run.send(ctrl('d'));
            },
        )
        .await
        .unwrap();
    }
}

const PBPASTE: &str = r#"#!/bin/sh
d="$(dirname "$0")"
echo read >> "$d/pasted"
printf 'from the clipboard'
"#;

fn right_at(column: u16, row: u16) -> Input {
    at(MouseKind::RightPress, column, row)
}

fn right_copying(fake: &Fake, on_release: bool, pastes: bool) -> Options {
    fake_tool::install(&fake.dir.root.join("pbpaste"), PBPASTE);
    let mut opts = copying(fake, on_release);
    opts.right_paste = pastes;
    opts.images = wisp_tui::term::clipboard::reader::Setup {
        ssh: false,
        wsl: false,
        natives: vec![Native::Mac],
        tools: fake.clipboard().tools,
        home: None,
    };
    opts
}

fn clipboard_reads(fake: &Fake) -> usize {
    std::fs::read_to_string(fake.dir.root.join("pasted"))
        .unwrap_or_default()
        .lines()
        .count()
}

#[tokio::test(start_paused = true)]
async fn right_click_copies_a_selection_like_ctrl_c_and_clears_it_with_or_without_copy_on_select() {
    for on_release in [false, true] {
        let fake = Fake::new(&format!("right-copy-{on_release}"));
        let screen = Screen::new(24, 60);
        session_with(
            screen.clone(),
            vec![prose_turn()],
            right_copying(&fake, on_release, true),
            |run| async move {
                asked(&run).await;
                select_word(&run, "delta").await;
                if on_release {
                    copied(&run, &fake, 1).await;
                }
                real_wait(100).await;
                let before = usize::from(on_release);
                assert_eq!(fake.copies().len(), before, "{:?}", fake.copies());
                assert_eq!(tinted(&run.screen), vec!["delta".to_owned()]);
                let (col, row) = locate(&run.screen, "golf");
                run.send(right_at(col, row));
                copied(&run, &fake, before + 1).await;
                assert_eq!(fake.copies().last().map(String::as_str), Some("delta"));
                assert!(tinted(&run.screen).is_empty(), "right-click clears");
                assert_eq!(clipboard_reads(&fake), 0, "a copy never reads");
                assert!(!run.screen.caret_line().contains("from the clipboard"));
                select_word(&run, "echo").await;
                run.send(ctrl('c'));
                copied(&run, &fake, before + 2 + before).await;
                assert_eq!(fake.copies().last().map(String::as_str), Some("echo"));
                run.send(right_at(col, row));
                eventually(|| run.screen.caret_line().contains("from the clipboard")).await;
                assert!(
                    run.screen.caret_line().contains("from the clipboard"),
                    "{:?}",
                    run.screen.caret_line()
                );
                assert_eq!(clipboard_reads(&fake), 1);
                run.send(ctrl('c'));
                run.send(ctrl('d'));
            },
        )
        .await
        .unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn right_click_copies_in_transcript_mode_and_otherwise_does_nothing_there() {
    let fake = Fake::new("right-pager");
    let screen = Screen::new(24, 60);
    session_with(
        screen.clone(),
        vec![prose_turn()],
        right_copying(&fake, false, true),
        |run| async move {
            asked(&run).await;
            run.send(ctrl('o'));
            sleep(Duration::from_millis(300)).await;
            assert!(run.screen.in_pager());
            select_word(&run, "papa").await;
            let (col, row) = locate(&run.screen, "papa");
            run.send(right_at(col, row));
            eventually(|| !fake.copies().is_empty()).await;
            assert_eq!(fake.copies(), vec!["papa".to_owned()]);
            assert!(tinted(&run.screen).is_empty());
            run.send(right_at(col, row));
            real_wait(300).await;
            sleep(TOAST + Duration::from_millis(100)).await;
            assert!(run.screen.in_pager(), "{:#?}", run.screen.visible());
            assert_eq!(clipboard_reads(&fake), 0);
            run.send(key(KeyCode::Char('q')));
            sleep(Duration::from_millis(100)).await;
            assert!(!run.screen.caret_line().contains("from the clipboard"));
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn right_click_with_nothing_selected_does_nothing_where_the_terminal_would_not_paste() {
    let fake = Fake::new("right-none");
    let screen = Screen::new(24, 60);
    session_with(
        screen.clone(),
        vec![prose_turn()],
        right_copying(&fake, false, false),
        |run| async move {
            asked(&run).await;
            run.send(right_at(5, 5));
            real_wait(360).await;
            assert_eq!(clipboard_reads(&fake), 0);
            assert!(fake.copies().is_empty());
            select_word(&run, "delta").await;
            run.send(right_at(5, 5));
            copied(&run, &fake, 1).await;
            assert_eq!(fake.copies(), vec!["delta".to_owned()]);
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_right_press_during_a_left_drag_does_nothing() {
    let fake = Fake::new("right-drag");
    let screen = Screen::new(24, 60);
    session_with(
        screen.clone(),
        vec![prose_turn()],
        right_copying(&fake, false, true),
        |run| async move {
            asked(&run).await;
            let (col, row) = locate(&run.screen, "delta");
            run.send(at(MouseKind::Press, col, row));
            run.send(right_at(col, row));
            sleep(Duration::from_millis(50)).await;
            run.send(at(MouseKind::Drag, col + 4, row));
            run.send(right_at(col + 4, row));
            real_wait(360).await;
            assert_eq!(clipboard_reads(&fake), 0);
            assert!(fake.copies().is_empty(), "{:?}", fake.copies());
            assert_eq!(tinted(&run.screen), vec!["delta".to_owned()]);
            run.send(at(MouseKind::Release, col + 4, row));
            run.send(right_at(col + 4, row));
            copied(&run, &fake, 1).await;
            assert_eq!(fake.copies(), vec!["delta".to_owned()]);
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn losing_focus_mid_drag_ends_the_drag_so_right_click_works_again() {
    let fake = Fake::new("right-focus");
    let screen = Screen::new(24, 60);
    session_with(
        screen.clone(),
        vec![prose_turn()],
        right_copying(&fake, false, true),
        |run| async move {
            asked(&run).await;
            let (col, row) = locate(&run.screen, "delta");
            run.send(at(MouseKind::Press, col, row));
            run.send(Input::Focus(false));
            run.send(Input::Focus(true));
            run.send(right_at(col, row));
            eventually(|| run.screen.caret_line().contains("from the clipboard")).await;
            assert!(
                run.screen.caret_line().contains("from the clipboard"),
                "{:?}",
                run.screen.caret_line()
            );
            assert_eq!(clipboard_reads(&fake), 1);
            run.send(at(MouseKind::Drag, col + 4, row));
            sleep(Duration::from_millis(50)).await;
            assert!(tinted(&run.screen).is_empty(), "{:?}", tinted(&run.screen));
            run.send(ctrl('c'));
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

fn colours(screen: &Screen) -> Vec<Vec<(vt100::Color, vt100::Color, String)>> {
    let state = screen.0.borrow();
    let s = state.parser.screen();
    let (rows, cols) = s.size();
    (0..rows.get())
        .map(|row| {
            (0..cols.get())
                .filter_map(|col| s.cell(row, col))
                .map(|c| (c.fgcolor(), c.bgcolor(), c.contents().to_owned()))
                .collect()
        })
        .collect()
}

fn rgb(theme: &Theme, token: Token) -> vt100::Color {
    let c = theme.rgb(token);
    vt100::Color::Rgb(c.r, c.g, c.b)
}

#[tokio::test(start_paused = true)]
async fn a_mouse_selection_lifts_faint_ink_and_keeps_accent() {
    let fake = Fake::new("inks");
    let screen = Screen::new(24, 60);
    let mut opts = copying(&fake, false);
    let theme = Theme::new(Mode::Light, Depth::TrueColor);
    opts.theme = theme;
    session_with(screen.clone(), vec![prose_turn()], opts, |run| async move {
        asked(&run).await;
        let before = colours(&run.screen);
        let (_, top) = locate(&run.screen, "hi");
        let (_, end) = locate(&run.screen, "Second");
        let last = end + 2;
        run.send(at(MouseKind::Press, 0, top));
        run.send(at(MouseKind::Drag, 59, last));
        sleep(Duration::from_millis(50)).await;
        let after = colours(&run.screen);
        let tint = rgb(&theme, Token::AccentTint);
        let (faint, muted) = (rgb(&theme, Token::TextFaint), rgb(&theme, Token::TextMuted));
        let accent = rgb(&theme, Token::Accent);
        let mut seen = (0, 0);
        let rows = usize::from(top)..=usize::from(last);
        for (row, (was, now)) in before.iter().zip(&after).enumerate() {
            if !rows.contains(&row) {
                continue;
            }
            for ((fg, bg, sym), (fg2, bg2, sym2)) in was.iter().zip(now) {
                assert_eq!(sym, sym2);
                if *bg2 != tint {
                    assert_eq!((fg, bg), (fg2, bg2), "row {row} {sym:?}");
                    continue;
                }
                if sym.trim().is_empty() {
                    continue;
                }
                seen.1 += usize::from(*fg == accent);
                let want = if *fg == faint {
                    seen.0 += 1;
                    muted
                } else {
                    Token::ALL
                        .iter()
                        .find(|&&t| rgb(&theme, t) == *fg)
                        .map_or(*fg, |&t| rgb(&theme, theme.on_tint(t, false)))
                };
                assert_eq!(*fg2, want, "row {row} {sym:?}");
            }
        }
        assert!(
            seen.0 > 0 && seen.1 > 0,
            "{seen:?}: {:#?}",
            run.screen.visible()
        );
        run.send(ctrl('d'));
    })
    .await
    .unwrap();
}

fn steered_turn() -> Vec<AgentEvent> {
    let mut turn = answer(["Working".to_owned()]);
    let end = turn.split_off(turn.len() - 2);
    turn.extend((0..50).map(|_| AgentEvent::TextDelta {
        index: 0,
        text: " w".to_owned(),
    }));
    turn.extend([
        AgentEvent::BlockStopped { index: 0 },
        AgentEvent::StepStarted { step: 2 },
        AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Text,
        },
        AgentEvent::TextDelta {
            index: 0,
            text: "Done.".to_owned(),
        },
    ]);
    turn.extend(end);
    turn
}

#[tokio::test(start_paused = true)]
async fn copying_a_user_message_on_its_band_yields_only_its_text() {
    let words = "Copy only the words of this message, which wraps across two rows";
    let steer = "    Steer words only, indented, though this message ends in a tag";
    for cols in [40u16, 80, 160] {
        let fake = Fake::new("user-band");
        let screen = Screen::new(30, cols);
        session_with(
            screen.clone(),
            vec![steered_turn()],
            copying(&fake, false),
            |run| async move {
                sleep(Duration::from_millis(100)).await;
                run.say(words);
                sleep(Duration::from_millis(200)).await;
                run.say(steer);
                sleep(Duration::from_secs(3)).await;
                let rows = run.screen.visible();
                assert!(
                    rows.iter().any(|r| r.contains("sent while working")),
                    "{rows:#?}"
                );
                for (n, (start, said)) in [("Copy only", words), ("Steer words", steer)]
                    .into_iter()
                    .enumerate()
                {
                    let (_, text) = locate(&run.screen, start);
                    let last = (usize::from(text)..rows.len())
                        .take_while(|r| !rows[*r].trim().is_empty())
                        .last()
                        .unwrap();
                    let below = u16::try_from(last + 1).unwrap();
                    run.send(at(MouseKind::Press, 0, text));
                    run.send(at(MouseKind::Drag, cols - 1, below));
                    run.send(at(MouseKind::Release, cols - 1, below));
                    run.send(ctrl('c'));
                    copied(&run, &fake, n + 1).await;
                    assert_eq!(fake.copies().remove(n), said, "{cols} columns");
                }
                run.send(ctrl('d'));
            },
        )
        .await
        .unwrap();
    }
}

fn right_copying_by_osc(terminal_pastes: bool) -> Options {
    let mut opts = options(Start::Stalled);
    opts.clipboard = Clipboard {
        route: Route::Terminal,
        ..Clipboard::none()
    };
    opts.copy_on_release = false;
    opts.terminal_pastes_on_right_click = terminal_pastes;
    opts
}

const DELTA_BY_OSC: &[u8] = b"\x1b]52;c;ZGVsdGE=\x07";

#[tokio::test(start_paused = true)]
async fn in_vs_code_a_right_click_copy_drops_the_old_clipboard_vs_code_pastes_after_it() {
    let screen = Screen::new(24, 60);
    session_with(
        screen.clone(),
        vec![prose_turn()],
        right_copying_by_osc(true),
        |run| async move {
            asked(&run).await;
            select_word(&run, "delta").await;
            let (col, row) = locate(&run.screen, "golf");
            run.send(right_at(col, row));
            run.send(Input::Paste("old clipboard".to_owned()));
            run.send(Input::Paste("mine".to_owned()));
            sleep(Duration::from_millis(50)).await;
            let caret = run.screen.caret_line();
            assert!(!caret.contains("old clipboard"), "{caret:?}");
            assert!(caret.contains("mine"), "{caret:?}");
            assert!(tinted(&run.screen).is_empty(), "right-click clears");
            run.send(ctrl('c'));
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
    assert_eq!(screen.sent_to_clipboard(), vec![DELTA_BY_OSC.to_vec()]);
}

#[tokio::test(start_paused = true)]
async fn in_vs_code_a_paste_after_the_right_click_window_lands() {
    let screen = Screen::new(24, 60);
    session_with(
        screen.clone(),
        vec![prose_turn()],
        right_copying_by_osc(true),
        |run| async move {
            asked(&run).await;
            select_word(&run, "delta").await;
            let (col, row) = locate(&run.screen, "golf");
            run.send(right_at(col, row));
            sleep(TERMINAL_PASTE_WINDOW).await;
            run.send(Input::Paste("typed later".to_owned()));
            sleep(Duration::from_millis(50)).await;
            let caret = run.screen.caret_line();
            assert!(caret.contains("typed later"), "{caret:?}");
            run.send(ctrl('c'));
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
    assert_eq!(screen.sent_to_clipboard(), vec![DELTA_BY_OSC.to_vec()]);
}

#[tokio::test(start_paused = true)]
async fn outside_vs_code_a_paste_right_after_a_right_click_copy_lands() {
    let screen = Screen::new(24, 60);
    session_with(
        screen.clone(),
        vec![prose_turn()],
        right_copying_by_osc(false),
        |run| async move {
            asked(&run).await;
            select_word(&run, "delta").await;
            let (col, row) = locate(&run.screen, "golf");
            run.send(right_at(col, row));
            run.send(Input::Paste("pasted at once".to_owned()));
            sleep(Duration::from_millis(50)).await;
            let caret = run.screen.caret_line();
            assert!(caret.contains("pasted at once"), "{caret:?}");
            run.send(ctrl('c'));
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
    assert_eq!(screen.sent_to_clipboard(), vec![DELTA_BY_OSC.to_vec()]);
}

#[tokio::test(start_paused = true)]
async fn in_vs_code_a_right_click_with_nothing_selected_leaves_one_paste_to_vs_code() {
    let fake = Fake::new("vscode-right-paste");
    let screen = Screen::new(24, 60);
    let vscode_on_wsl = Host {
        vscode: true,
        wsl: true,
        ..Host::default()
    };
    let mut opts = right_copying(&fake, false, right_click_pastes(&vscode_on_wsl));
    opts.terminal_pastes_on_right_click = terminal_pastes_on_right_click(&vscode_on_wsl);
    session_with(screen.clone(), vec![prose_turn()], opts, |run| async move {
        asked(&run).await;
        let (col, row) = locate(&run.screen, "golf");
        run.send(right_at(col, row));
        run.send(Input::Paste("from vs code".to_owned()));
        real_wait(400).await;
        let caret = run.screen.caret_line();
        assert_eq!(caret.matches("from vs code").count(), 1, "{caret:?}");
        assert!(!caret.contains("from the clipboard"), "{caret:?}");
        assert_eq!(clipboard_reads(&fake), 0, "Wisp read the clipboard too");
        run.send(ctrl('c'));
        run.send(ctrl('d'));
    })
    .await
    .unwrap();
}
