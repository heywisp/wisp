use std::time::Duration;

use tokio::time::sleep;
use wisp_protocol::{
    AgentEvent, Answer, BlockKind, ClientCommand, DeliveryMode, OutputKind, Question, QuestionEnd,
    QuestionOption, RejectKind, ToolAction,
};
use wisp_render::chrome::account::AccountKind;
use wisp_render::transcript::copy;
use wisp_tui::app::highlight::Start;
use wisp_tui::app::{Exit, Options, replay};
use wisp_tui::term::clipboard::{Clipboard, Route};
use wisp_tui::term::{Input, Key, KeyCode, Mouse, MouseKind};

use super::steering::{
    Agent, attached, cancelled, position, row_with, steering, steering_finished, steering_on,
};
use super::{Screen, ctrl, ctrl_key, eventually, key, options, real_wait, session};
use crate::Drive;
use crate::events::{Requested, tool_requested};
use crate::processes::finish;
use crate::spots::row_spot;

const FIXTURE: &str = include_str!("../fixtures/ask.jsonl");

pub(crate) fn option(label: &str, description: &str) -> QuestionOption {
    QuestionOption {
        label: label.to_owned(),
        description: description.to_owned(),
    }
}

pub(super) fn database() -> Question {
    Question {
        header: "Database".to_owned(),
        question: "Which database should the service use?".to_owned(),
        options: vec![
            option(
                "PostgreSQL (Recommended)",
                "Relational; the billing service runs it.",
            ),
            option("SQLite", "One file, no server."),
        ],
        multi_select: false,
        source: None,
    }
}

fn process_question(id: u32) -> Question {
    Question {
        header: format!("Process {id}"),
        question: format!("Stop process {id} (sudo apt install x)?"),
        options: vec![option("Stop", ""), option("Keep running", "")],
        multi_select: false,
        source: None,
    }
}

fn features() -> Question {
    Question {
        header: "Features".to_owned(),
        question: "Which features should ship first?".to_owned(),
        options: vec![
            option("Auth", ""),
            option("Search", ""),
            option("Export", ""),
        ],
        multi_select: true,
        source: None,
    }
}

fn deploy() -> Question {
    Question {
        header: "Deploy".to_owned(),
        question: "Where will it run?".to_owned(),
        options: vec![option("Fly.io", ""), option("A VPS", "")],
        multi_select: false,
        source: None,
    }
}

async fn ask(a: &Agent, id: &str, questions: Vec<Question>) {
    a.emit(AgentEvent::BlockStarted {
        index: 1,
        kind: BlockKind::ToolUse {
            writing: None,
            name: "ask_user".to_owned(),
        },
    })
    .await;
    a.emit(tool_requested(Requested {
        block: 1,
        call_id: id.to_owned(),
        tool: "ask_user".to_owned(),
        action: ToolAction::Other {
            name: "ask_user".to_owned(),
        },
        target: "Database".to_owned(),
        output: OutputKind::None,
        ..Requested::default()
    }))
    .await;
    a.emit(AgentEvent::StepEnded {
        step: 1,
        usage: None,
        cost_usd: None,
        lower_bound: false,
    })
    .await;
    a.emit(AgentEvent::call_started(id.to_owned(), "")).await;
    a.emit(AgentEvent::QuestionAsked {
        call_id: id.to_owned(),
        questions,
    })
    .await;
}

async fn begun(a: &mut Agent, questions: Vec<Question>) {
    begun_as(a, "q1", questions).await;
}

async fn begun_as(a: &mut Agent, id: &str, questions: Vec<Question>) {
    a.start("plan it", 1).await;
    sleep(Duration::from_millis(600)).await;
    ask(a, id, questions).await;
    sleep(Duration::from_millis(50)).await;
}

fn answered(command: &ClientCommand) -> (String, Vec<Answer>) {
    match command {
        ClientCommand::Answer { call_id, answers } => (call_id.clone(), answers.clone()),
        other => panic!("not an answer: {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn the_ask_replay_fixture_answers_through_the_card() {
    let events = replay::parse(FIXTURE).unwrap();
    let screen = Screen::new(30, 80);
    session(
        screen.clone(),
        vec![events],
        Start::Stalled,
        |run| async move {
            sleep(Duration::from_millis(100)).await;
            run.say("plan the service");
            sleep(Duration::from_millis(100)).await;
            run.say("use plan B");
            sleep(Duration::from_secs(2)).await;
            let lines = run.screen.visible();
            assert!(
                lines.iter().any(|l| l.starts_with(" ? Database")),
                "{lines:#?}"
            );
            assert!(
                lines
                    .iter()
                    .any(|l| l.contains(&copy::waiting_for_answer())),
                "{lines:#?}"
            );
            run.send(key(KeyCode::Char('1')));
            run.send(key(KeyCode::Char('1')));
            run.send(key(KeyCode::Char('3')));
            run.send(Input::Paste("Webhooks for new notes".to_owned()));
            run.send(key(KeyCode::Enter));
            run.send(key(KeyCode::Tab));
            sleep(Duration::from_millis(100)).await;
            let lines = run.screen.visible();
            assert!(
                lines
                    .iter()
                    .any(|l| l.contains("Deploy     not answered yet")),
                "{lines:#?}"
            );
            run.send(key(KeyCode::Enter));
            sleep(Duration::from_secs(2)).await;
            let lines = run.screen.visible();
            for want in [
                "\u{2713} Asked Database, Features, Deploy",
                "Which features should ship first?",
                "\u{203a} PostgreSQL (Recommended)",
                "\u{203a} Auth \u{b7} Export \u{b7} your own: Webhooks for new notes",
                "\u{203a} skipped",
                "Thanks. I'll go with your answers",
            ] {
                assert!(lines.iter().any(|l| l.contains(want)), "{want}: {lines:#?}");
            }
            let not_run = lines
                .iter()
                .find(|l| l.contains(copy::NOT_RUN_FACT))
                .unwrap_or_else(|| panic!("{lines:#?}"));
            assert!(not_run.contains("cargo new notes-service"), "{not_run}");
            assert!(not_run.ends_with(copy::AFTER_QUESTION), "{not_run}");
            let asked = lines.iter().position(|l| l.contains("\u{2713} Asked"));
            let steer = lines.iter().position(|l| l.contains("use plan B"));
            assert!(asked.is_some() && asked < steer, "{lines:#?}");
            assert!(!lines.iter().any(|l| l.starts_with(" ? ")), "{lines:#?}");
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn one_key_answers_a_lone_question_and_the_card_closes_on_the_kernels_word() {
    steering(|mut a| async move {
        begun(&mut a, vec![database()]).await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "question 1 of 1").is_some(), "{lines:#?}");
        assert!(
            row_with(&lines, "Waiting for your answer").is_some(),
            "{lines:#?}"
        );
        a.key(KeyCode::Char('2'));
        let (id, answers) = answered(&a.next().await);
        assert_eq!(id, "q1");
        assert_eq!(answers, vec![Answer::choose(1)]);
        let lines = a.settle().await;
        assert!(
            row_with(&lines, "sending your answers").is_some(),
            "{lines:#?}"
        );
        a.key(KeyCode::Char('1'));
        a.nothing_sent().await;
        a.emit(AgentEvent::QuestionClosed {
            call_id: "q1".to_owned(),
            how: QuestionEnd::Answered { answers },
        })
        .await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "question 1 of 1").is_none(), "{lines:#?}");
        assert!(row_with(&lines, "Asked Database").is_some(), "{lines:#?}");
        assert!(row_with(&lines, "\u{203a} SQLite").is_some(), "{lines:#?}");
        assert!(row_with(&lines, "Asking").is_none(), "{lines:#?}");
        finish(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_card_seen_before_transcript_mode_takes_the_keys_back_at_once() {
    steering(|mut a| async move {
        begun(&mut a, vec![database()]).await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "question 1 of 1").is_some(), "{lines:#?}");
        a.ctrl('o');
        a.settle().await;
        assert!(a.screen.in_pager());
        a.key(KeyCode::Char('q'))
            .key(KeyCode::Char('2'))
            .key(KeyCode::Enter);
        let (id, answers) = answered(&a.next().await);
        assert_eq!(id, "q1");
        assert_eq!(answers, vec![Answer::choose(1)]);
        a.nothing_sent().await;
        finish(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_new_card_asked_under_transcript_mode_waits_once_it_closes() {
    steering(|mut a| async move {
        begun(&mut a, vec![database()]).await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "question 1 of 1").is_some(), "{lines:#?}");
        a.ctrl('o');
        a.settle().await;
        assert!(a.screen.in_pager());
        a.emit(AgentEvent::QuestionClosed {
            call_id: "q1".to_owned(),
            how: QuestionEnd::Stopped,
        })
        .await;
        a.emit(AgentEvent::QuestionAsked {
            call_id: "q2".to_owned(),
            questions: vec![database()],
        })
        .await;
        sleep(Duration::from_millis(600)).await;
        a.key(KeyCode::Char('q')).key(KeyCode::Char('2'));
        let lines = a.settle().await;
        assert!(!a.screen.in_pager(), "{lines:#?}");
        assert!(row_with(&lines, "question 1 of 1").is_some(), "{lines:#?}");
        a.nothing_sent().await;
        a.key(KeyCode::Backspace);
        sleep(Duration::from_millis(600)).await;
        a.key(KeyCode::Char('1'));
        let (id, answers) = answered(&a.next().await);
        assert_eq!(id, "q2");
        assert_eq!(answers, vec![Answer::choose(0)]);
        finish(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn keys_typed_within_the_settle_go_to_the_composer() {
    steering(|mut a| async move {
        a.start("plan it", 1).await;
        sleep(Duration::from_millis(600)).await;
        a.paste("draf");
        ask(&a, "q1", vec![database()]).await;
        a.key(KeyCode::Char('t'));
        sleep(Duration::from_millis(300)).await;
        a.key(KeyCode::Char('1'));
        let lines = a.settle().await;
        assert!(
            row_with(&lines, copy::CARD_SETTLING).is_some(),
            "{lines:#?}"
        );
        a.nothing_sent().await;
        assert!(a.screen.caret_line().contains("draft1"), "{lines:#?}");
        sleep(Duration::from_millis(500)).await;
        let lines = a.settle().await;
        assert!(
            row_with(&lines, copy::CARD_SETTLING).is_some(),
            "{lines:#?}"
        );
        for _ in 0.."draft1".len() {
            a.key(KeyCode::Backspace);
        }
        sleep(Duration::from_millis(600)).await;
        let lines = a.settle().await;
        assert!(
            row_with(&lines, "1\u{2013}3 choose").is_some(),
            "{lines:#?}"
        );
        assert!(!super::chat_row_shown(&lines), "{lines:#?}");
        a.key(KeyCode::Char('2'));
        assert_eq!(answered(&a.next().await).1, vec![Answer::choose(1)]);
        finish(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn enter_within_the_settle_steers_instead_of_answering() {
    steering(|mut a| async move {
        a.start("plan it", 1).await;
        sleep(Duration::from_millis(600)).await;
        a.paste("use sqlite");
        ask(&a, "q1", vec![database()]).await;
        sleep(Duration::from_millis(300)).await;
        a.key(KeyCode::Enter);
        match a.next().await {
            ClientCommand::Send { text, mode, .. } => {
                assert_eq!(text, "use sqlite");
                assert_eq!(mode, DeliveryMode::Steer);
            }
            other => panic!("not a steer: {other:?}"),
        }
        sleep(Duration::from_millis(300)).await;
        a.key(KeyCode::Char('2'));
        a.nothing_sent().await;
        assert!(a.screen.caret_line().contains("\u{203A} 2"));
        a.key(KeyCode::Backspace);
        sleep(Duration::from_millis(600)).await;
        let lines = a.settle().await;
        assert!(
            row_with(&lines, "1\u{2013}3 choose").is_some(),
            "{lines:#?}"
        );
        assert!(!super::chat_row_shown(&lines), "{lines:#?}");
        finish(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn shift_tab_pages_questions_instead_of_cycling_effort() {
    steering(|mut a| async move {
        begun(&mut a, vec![database(), features(), deploy()]).await;
        a.key(KeyCode::Tab).key(KeyCode::Tab);
        let lines = a.settle().await;
        assert!(row_with(&lines, "3 of 3").is_some(), "{lines:#?}");
        a.key(KeyCode::BackTab);
        a.inputs
            .send(Input::Key(Key {
                shift: true,
                ..Key::plain(KeyCode::Tab)
            }))
            .unwrap();
        let lines = a.settle().await;
        assert!(row_with(&lines, "1 of 3").is_some(), "{lines:#?}");
        a.nothing_sent().await;
        a.key(KeyCode::Right).key(KeyCode::Left).key(KeyCode::Right);
        let lines = a.settle().await;
        assert!(row_with(&lines, "2 of 3").is_some(), "{lines:#?}");
        a.nothing_sent().await;
        finish(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn esc_stops_the_turn_and_returns_typed_text_to_the_composer() {
    for server in [false, true] {
        steering(|mut a| async move {
            let mut question = database();
            if server {
                question.source = Some(wisp_protocol::QuestionSource {
                    asked_by: wisp_protocol::AskedBy::Server {
                        name: "linear".into(),
                    },
                    typed: true,
                });
                question.options = vec![option("Not now", "")];
            }
            begun(&mut a, vec![question, deploy()]).await;
            a.paste("MySQL, please");
            a.key(KeyCode::Esc);
            assert_eq!(a.next().await, ClientCommand::Cancel);
            a.emit(AgentEvent::QuestionClosed {
                call_id: "q1".to_owned(),
                how: QuestionEnd::Stopped,
            })
            .await;
            a.emit(cancelled()).await;
            let lines = a.settle().await;
            assert!(row_with(&lines, " ? Database").is_none(), "{lines:#?}");
            assert_eq!(
                row_with(&lines, copy::ANSWER_BACK).is_some(),
                !server,
                "{lines:#?}"
            );
            assert_eq!(
                a.screen.caret_line().contains("MySQL, please"),
                !server,
                "{lines:#?}"
            );
            assert_eq!(
                lines.iter().filter(|l| l.contains("MySQL, please")).count(),
                usize::from(!server),
                "{lines:#?}"
            );
            let head = row_with(&lines, copy::NOT_ANSWERED).unwrap();
            assert!(head.contains("Asked Database, Deploy"), "{head}");
            assert!(head.ends_with(copy::ANSWER_STOPPED), "{head}");
            a.ctrl('c').ctrl('c').ctrl('c');
        })
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn ctrl_c_stops_the_turn_when_nothing_is_selected() {
    steering(|mut a| async move {
        begun(&mut a, vec![database()]).await;
        a.ctrl('c');
        assert_eq!(a.next().await, ClientCommand::Cancel);
        finish(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn ctrl_c_copies_a_selection_while_a_question_is_open() {
    let screen = Screen::new(30, 80);
    let mut opts = options(Start::Stalled);
    opts.clipboard = Clipboard {
        route: Route::Terminal,
        ..Clipboard::none()
    };
    opts.copy_on_release = false;
    steering_on(screen.clone(), opts, |mut a| async move {
        begun(&mut a, vec![database()]).await;
        let lines = a.settle().await;
        let (col, row) = row_spot(&lines, copy::TAGLINE_OWN).unwrap();
        for (kind, column) in [
            (MouseKind::Press, col),
            (MouseKind::Drag, col + 7),
            (MouseKind::Release, col + 7),
        ] {
            a.inputs
                .send(Input::Mouse(Mouse {
                    kind,
                    column,
                    row,
                    modified: false,
                }))
                .unwrap();
        }
        a.ctrl('c');
        a.nothing_sent().await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "question 1 of 1").is_some(), "{lines:#?}");
        select_rows(&a, row, row);
        a.settle().await;
        let (column, row) = row_spot(&lines, "question 1 of 1").unwrap();
        super::sidebar::mouse(&a, MouseKind::Press, column, row);
        super::sidebar::mouse(&a, MouseKind::Release, column, row);
        a.settle().await;
        a.ctrl('c');
        assert_eq!(a.next().await, ClientCommand::Cancel);
        finish(&a).await;
    })
    .await;
    assert_eq!(
        screen.sent_to_clipboard(),
        vec![b"\x1b]52;c;WW91ciBvd24=\x07".to_vec()]
    );
}

#[tokio::test(start_paused = true)]
async fn a_click_picks_only_an_uncovered_option() {
    for covering in [None, Some(false), Some(true)] {
        steering(|mut a| async move {
            begun(&mut a, vec![database()]).await;
            let lines = a.settle().await;
            let row = u16::try_from(position(&lines, "2 SQLite")).unwrap();
            if let Some(secret) = covering {
                a.emit(super::processes::waiting_terminal(5)).await;
                a.emit(if secret {
                    super::secrets::asked()
                } else {
                    super::processes::asked(5)
                })
                .await;
                sleep(Duration::from_millis(600)).await;
                let covered = a.settle().await;
                assert!(!covered[usize::from(row)].contains("SQLite"), "{covered:?}");
            }
            super::sidebar::mouse(&a, MouseKind::Press, 6, row);
            if covering.is_none() {
                assert_eq!(answered(&a.next().await).1, vec![Answer::choose(1)]);
            } else {
                let sent = super::sidebar::drain(&mut a).await;
                assert!(
                    !sent
                        .iter()
                        .any(|command| matches!(command, ClientCommand::Answer { .. })),
                    "covering={covering:?}: hidden option answered: {sent:?}"
                );
            }
            finish(&a).await;
        })
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_refused_answer_opens_the_card_again() {
    steering(|mut a| async move {
        begun(&mut a, vec![database()]).await;
        a.key(KeyCode::Char('1'));
        answered(&a.next().await);
        a.emit(AgentEvent::Rejected {
            request: None,
            reason: "answer for q1 refused: answer 1 chooses option 7".to_owned(),
            id: None,
            kind: Some(RejectKind::BadAnswer),
        })
        .await;
        let lines = a.settle().await;
        assert!(
            row_with(&lines, "1\u{2013}3 choose").is_some(),
            "{lines:#?}"
        );
        a.key(KeyCode::Char('2'));
        assert_eq!(answered(&a.next().await).1, vec![Answer::choose(1)]);
        finish(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_question_rings_the_bell_only_when_the_window_is_unfocused() {
    let away = Screen::new(30, 80);
    steering_on(away.clone(), options(Start::Stalled), |mut a| async move {
        a.inputs.send(Input::Focus(false)).unwrap();
        begun(&mut a, vec![database()]).await;
        a.settle().await;
        finish(&a).await;
    })
    .await;
    assert_eq!(away.spied(), vec!["bell".to_owned()]);
    let here = Screen::new(30, 80);
    steering_on(here.clone(), options(Start::Stalled), |mut a| async move {
        a.inputs.send(Input::Focus(true)).unwrap();
        begun(&mut a, vec![database()]).await;
        a.settle().await;
        finish(&a).await;
    })
    .await;
    assert_eq!(here.spied(), Vec::<String>::new());
}

#[tokio::test(start_paused = true)]
async fn model_text_in_a_question_cannot_inject_escapes() {
    let screen = Screen::new(30, 80);
    steering_on(
        screen.clone(),
        options(Start::Stalled),
        |mut a| async move {
            let hostile = Question {
                header: "H\u{1b}]0;pwned\u{7}".to_owned(),
                question: "Q\u{1b}[2J\u{1b}]52;c;cHduZWQ=\u{7}?".to_owned(),
                options: vec![
                    option("L\u{1b}]2;title\u{7}", "D\u{9b}31m\u{1b}[?1049l"),
                    option("ok", "\u{1b}]8;;http://x\u{7}link\u{1b}]8;;\u{7}"),
                ],
                multi_select: false,
                source: None,
            };
            begun(&mut a, vec![hostile]).await;
            let lines = a.settle().await;
            assert!(row_with(&lines, "Q^[[2J^[]52").is_some(), "{lines:#?}");
            assert!(row_with(&lines, "1 L^[]2;title^G").is_some(), "{lines:#?}");
            a.key(KeyCode::Char('3'));
            a.ctrl('o');
            let pager = a.settle().await;
            assert!(row_with(&pager, "[H^[]0;pw").is_some(), "{pager:#?}");
            a.key(KeyCode::Char('q'));
            finish(&a).await;
        },
    )
    .await;
    assert_eq!(screen.spied(), Vec::<String>::new());
    assert!(screen.on_alternate());
}

#[tokio::test(start_paused = true)]
async fn a_draft_keeps_the_keys_and_the_chat_row_hides_once_it_is_sent() {
    steering(|mut a| async move {
        a.start("plan it", 1).await;
        a.paste("my draft");
        sleep(Duration::from_millis(600)).await;
        ask(&a, "q1", vec![database()]).await;
        sleep(Duration::from_millis(600)).await;
        let lines = a.settle().await;
        assert!(
            row_with(&lines, copy::CARD_SETTLING).is_some(),
            "{lines:#?}"
        );
        assert!(a.screen.caret_line().contains("my draft"), "{lines:#?}");
        let row = u16::try_from(position(&lines, "2 SQLite")).unwrap();
        a.inputs
            .send(Input::Mouse(Mouse {
                kind: MouseKind::Press,
                column: 6,
                row,
                modified: false,
            }))
            .unwrap();
        a.nothing_sent().await;
        a.key(KeyCode::Tab);
        match a.next().await {
            ClientCommand::Send { text, mode, .. } => {
                assert_eq!((text.as_str(), mode), ("my draft", DeliveryMode::Queue));
            }
            other => panic!("not queued: {other:?}"),
        }
        sleep(Duration::from_millis(600)).await;
        let lines = a.settle().await;
        assert!(
            row_with(&lines, "1\u{2013}3 choose").is_some(),
            "{lines:#?}"
        );
        assert!(!super::chat_row_shown(&lines), "{lines:#?}");
        a.key(KeyCode::Char('2'));
        answered(&a.next().await);
        finish(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_cut_keeps_a_question_draft_only_for_the_same_questions() {
    for (id, changed, closed, second_cut, sending) in [
        (Some("q1"), false, false, false, false),
        (Some("q2"), false, false, false, false),
        (Some("q1"), true, false, false, false),
        (Some("q1"), false, true, false, false),
        (None, false, false, false, false),
        (Some("q1"), false, false, true, false),
        (Some("q1"), false, false, false, true),
    ] {
        let mut opts = options(Start::Stalled);
        let ended = attached(&mut opts, wisp_protocol::link::REQUEST_RANGE);
        let finished = steering_finished(
            Screen::new(30, 80),
            opts,
            super::facts(),
            |mut a| async move {
                begun(&mut a, vec![database(), deploy()]).await;
                a.key(KeyCode::Char('2'))
                    .paste("a reply")
                    .key(KeyCode::Left);
                let lines = a.settle().await;
                assert!(row_with(&lines, "2 of 2").is_some(), "{lines:#?}");
                assert!(a.screen.caret_line().contains("a reply"), "{lines:#?}");
                if sending {
                    a.key(KeyCode::Enter).key(KeyCode::Enter);
                    assert_eq!(
                        answered(&a.next().await).1,
                        vec![Answer::choose(1), Answer::own("a reply")]
                    );
                }
                a.cut(ended).await;
            },
        )
        .await;
        assert_eq!(finished.exit.unwrap(), Exit::Cut);
        assert!(finished.unsent.is_some());
        let mut unsent = finished.unsent;
        if second_cut {
            let mut opts = options(Start::Stalled);
            opts.unsent = unsent;
            let ended = attached(&mut opts, wisp_protocol::link::REQUEST_RANGE);
            let finished =
                steering_finished(Screen::new(30, 80), opts, super::facts(), |a| async move {
                    let lines = a.settle().await;
                    assert!(row_with(&lines, "a reply").is_none(), "{lines:#?}");
                    a.cut(ended).await;
                })
                .await;
            assert_eq!(finished.exit.unwrap(), Exit::Cut);
            unsent = finished.unsent;
            assert!(unsent.is_some());
        }
        let mut opts = options(Start::Stalled);
        opts.unsent = unsent;
        steering_on(Screen::new(30, 80), opts, |mut a| async move {
            let lines = a.settle().await;
            assert!(row_with(&lines, "2 of 2").is_none(), "{lines:#?}");
            a.nothing_sent().await;
            if closed {
                a.emit(AgentEvent::QuestionClosed {
                    call_id: "q1".to_owned(),
                    how: QuestionEnd::Stopped,
                })
                .await;
            }
            let Some(id) = id else {
                assert!(row_with(&lines, "a reply").is_none(), "{lines:#?}");
                return;
            };
            let questions = if changed {
                vec![database(), features()]
            } else {
                vec![database(), deploy()]
            };
            ask(&a, id, questions).await;
            let lines = a.settle().await;
            if id != "q1" || changed || closed {
                assert!(row_with(&lines, "1 of 2").is_some(), "{lines:#?}");
                assert!(row_with(&lines, "a reply").is_none(), "{lines:#?}");
                a.nothing_sent().await;
                return;
            }
            if sending {
                a.key(KeyCode::Enter);
                assert_eq!(
                    answered(&a.next().await).1,
                    vec![Answer::choose(1), Answer::own("a reply")]
                );
                finish(&a).await;
                return;
            }
            assert!(row_with(&lines, "2 of 2").is_some(), "{lines:#?}");
            assert!(a.screen.caret_line().contains("a reply"), "{lines:#?}");
            a.paste("!").key(KeyCode::Enter).key(KeyCode::Enter);
            assert_eq!(
                answered(&a.next().await),
                (
                    "q1".into(),
                    vec![Answer::choose(1), Answer::own("a repl!y")]
                )
            );
            finish(&a).await;
        })
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_picker_waits_until_the_question_is_answered() {
    steering(|mut a| async move {
        a.start("plan it", 1).await;
        sleep(Duration::from_millis(600)).await;
        a.paste("/model");
        ask(&a, "q1", vec![database()]).await;
        a.key(KeyCode::Enter);
        let lines = a.settle().await;
        assert!(
            row_with(&lines, copy::CARD_NO_PICKER).is_some(),
            "{lines:#?}"
        );
        assert!(row_with(&lines, "question 1 of 1").is_some(), "{lines:#?}");
        finish(&a).await;
    })
    .await;
}

fn styled_prose() -> String {
    (1..=40)
        .map(|i| format!("**Bold line {i} of the earlier answer, written long.**\n\n"))
        .collect()
}

async fn fill_view(a: &mut Agent) {
    a.start("plan it", 1).await;
    a.emit(AgentEvent::TextDelta {
        index: 0,
        text: styled_prose(),
    })
    .await;
    a.emit(AgentEvent::BlockStopped { index: 0 }).await;
    sleep(Duration::from_millis(600)).await;
}

fn kept_selection() -> Options {
    let mut opts = options(Start::Stalled);
    opts.copy_on_release = false;
    opts
}

async fn select_all_prose(a: &Agent) {
    let lines = a.settle().await;
    let to = u16::try_from(position(&lines, "Bold line 40")).unwrap();
    select_rows(a, 2, to);
}

fn select_rows(a: &Agent, from: u16, to: u16) {
    for (kind, column, row) in [
        (MouseKind::Press, 2, from),
        (MouseKind::Drag, 60, to),
        (MouseKind::Release, 60, to),
    ] {
        a.inputs
            .send(Input::Mouse(Mouse {
                kind,
                column,
                row,
                modified: false,
            }))
            .unwrap();
    }
}

fn cell_looks(screen: &Screen, row: usize) -> Vec<String> {
    let state = screen.0.borrow();
    let shown = state.parser.screen();
    let cols = shown.size().1.get();
    let row = u16::try_from(row).unwrap();
    (0..cols)
        .map(|col| {
            shown.cell(row, col).map_or_else(String::new, |c| {
                format!(
                    "{:?} {:?} {} {} {} {}",
                    c.fgcolor(),
                    c.bgcolor(),
                    c.bold(),
                    c.dim(),
                    c.italic(),
                    c.inverse()
                )
            })
        })
        .collect()
}

type Overlay = Vec<(String, Vec<String>)>;

async fn overlay(styled: bool, picker: bool) -> Overlay {
    let seen: std::rc::Rc<std::cell::RefCell<Overlay>> = std::rc::Rc::default();
    let out = std::rc::Rc::clone(&seen);
    steering_on(Screen::new(30, 80), kept_selection(), |mut a| async move {
        a.start("plan it", 1).await;
        if styled {
            a.emit(AgentEvent::TextDelta {
                index: 0,
                text: styled_prose(),
            })
            .await;
        }
        a.emit(AgentEvent::BlockStopped { index: 0 }).await;
        sleep(Duration::from_millis(600)).await;
        if styled {
            select_all_prose(&a).await;
        }
        let first = if picker {
            a.paste("/model").key(KeyCode::Enter);
            a.describe().await;
            format!("claude-api  {}", AccountKind::ClaudeApi.label())
        } else {
            ask(&a, "q1", vec![database()]).await;
            " ? Database".to_owned()
        };
        let lines = a.settle().await;
        let top = position(&lines, &first);
        let rule = super::frame_top(&lines);
        if styled {
            let above = cell_looks(&a.screen, position(&lines, "Bold line 35"));
            assert!(above.iter().any(|l| l.contains(" true ")), "{lines:#?}");
            if !picker {
                assert!(above.iter().any(|l| !l.contains(" Default ")), "{lines:#?}");
            }
        }
        *out.borrow_mut() = (top..rule)
            .map(|row| (lines[row].clone(), cell_looks(&a.screen, row)))
            .collect();
        if picker {
            a.key(KeyCode::Esc);
        } else {
            a.key(KeyCode::Char('2'));
            answered(&a.next().await);
        }
        finish(&a).await;
    })
    .await;
    seen.take()
}

#[tokio::test(start_paused = true)]
async fn the_card_never_shows_the_styles_of_the_transcript_beneath_it() {
    let over = overlay(true, false).await;
    let bare = overlay(false, false).await;
    assert!(over.len() > 8, "{over:#?}");
    assert_eq!(over, bare);
}

#[tokio::test(start_paused = true)]
async fn the_model_picker_never_shows_the_styles_of_the_transcript_beneath_it() {
    let over = overlay(true, true).await;
    let bare = overlay(false, true).await;
    assert!(over.len() > 5, "{over:#?}");
    assert_eq!(over, bare);
}

#[tokio::test(start_paused = true)]
async fn scroll_keys_move_the_view_and_esc_still_stops_the_turn_while_scrolled() {
    steering(|mut a| async move {
        fill_view(&mut a).await;
        ask(&a, "q1", vec![database()]).await;
        sleep(Duration::from_millis(50)).await;
        a.key(KeyCode::Char('3'));
        a.paste("abc");
        let bottom = a.settle().await;
        let call = position(&bottom, "Asking Database");
        let (column, row) = row_spot(&bottom, "question 1 of 1").unwrap();
        super::sidebar::mouse(&a, MouseKind::WheelUp, column, row);
        let wheeled = a.settle().await;
        assert_ne!(
            wheeled[..call],
            bottom[..call],
            "wheel over the card scrolls the chat"
        );
        a.inputs.send(ctrl_key(KeyCode::End)).unwrap();
        a.settle().await;
        a.key(KeyCode::PageUp);
        let up = a.settle().await;
        assert_ne!(up[..call], bottom[..call], "{up:#?}");
        assert!(row_with(&up, "Your own: abc").is_some(), "{up:#?}");
        a.inputs.send(ctrl_key(KeyCode::End)).unwrap();
        let back = a.settle().await;
        assert_eq!(back[..call], bottom[..call]);
        a.inputs.send(ctrl_key(KeyCode::Home)).unwrap();
        let top = a.settle().await;
        assert!(row_with(&top, copy::TAGLINE_OWN).is_some(), "{top:#?}");
        a.key(KeyCode::PageDown);
        let scrolled = a.settle().await;
        assert_ne!(scrolled[..call], bottom[..call], "{scrolled:#?}");
        a.key(KeyCode::Esc);
        assert_eq!(a.next().await, ClientCommand::Cancel);
        a.emit(AgentEvent::QuestionClosed {
            call_id: "q1".to_owned(),
            how: QuestionEnd::Stopped,
        })
        .await;
        a.emit(cancelled()).await;
        let lines = a.settle().await;
        assert!(row_with(&lines, " ? Database").is_none(), "{lines:#?}");
        assert!(a.screen.caret_line().contains("abc"), "{lines:#?}");
        assert!(row_with(&lines, "Your own: abc").is_none(), "{lines:#?}");
        a.ctrl('c').ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_reply_before_a_question_stays_in_view_above_the_card() {
    steering(|mut a| async move {
        fill_view(&mut a).await;
        ask(&a, "q1", vec![database()]).await;
        sleep(Duration::from_millis(600)).await;
        let lines = a.settle().await;
        let card = position(&lines, " ? Database");
        let above: Vec<&str> = lines[..card]
            .iter()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect();
        assert!(
            above[above.len() - 2].starts_with("Bold line 40"),
            "{lines:#?}"
        );
        assert!(
            above[above.len() - 1].contains("Asking Database"),
            "{lines:#?}"
        );
        a.key(KeyCode::PageUp);
        let up = a.settle().await;
        let step = card / 2;
        assert_eq!(up[step..card], lines[..card - step], "{up:#?}");
        a.inputs.send(ctrl_key(KeyCode::Home)).unwrap();
        a.key(KeyCode::Char('1'));
        answered(&a.next().await);
        a.emit(AgentEvent::QuestionClosed {
            call_id: "q1".to_owned(),
            how: QuestionEnd::Answered {
                answers: vec![Answer::choose(0)],
            },
        })
        .await;
        let closed = a.settle().await;
        assert!(row_with(&closed, " ? Database").is_none(), "{closed:#?}");
        assert!(position(&closed, "Bold line 40") >= card, "{closed:#?}");
        finish(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_question_leaves_an_open_picker_working() {
    steering(|mut a| async move {
        a.start("plan it", 1).await;
        sleep(Duration::from_millis(600)).await;
        a.paste("/model").key(KeyCode::Enter);
        a.describe().await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "sonnet-5-5").is_some(), "{lines:#?}");
        ask(&a, "q1", vec![database()]).await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "sonnet-5-5").is_some(), "{lines:#?}");
        assert!(row_with(&lines, " ? Database").is_none(), "{lines:#?}");
        a.key(KeyCode::Down);
        a.key(KeyCode::Esc);
        a.nothing_sent().await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "sonnet-5-5").is_none(), "{lines:#?}");
        assert!(row_with(&lines, " ? Database").is_some(), "{lines:#?}");
        sleep(Duration::from_millis(500)).await;
        a.key(KeyCode::Char('2'));
        assert_eq!(answered(&a.next().await).1, vec![Answer::choose(1)]);
        finish(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_answer_carries_the_raw_call_id() {
    steering(|mut a| async move {
        let id = "toolu_\u{1b}[31m\u{7}01";
        begun_as(&mut a, id, vec![database()]).await;
        a.key(KeyCode::Char('1'));
        let (sent, _) = answered(&a.next().await);
        assert_eq!(sent, id);
        a.emit(AgentEvent::QuestionClosed {
            call_id: id.to_owned(),
            how: QuestionEnd::Answered {
                answers: vec![Answer::choose(0)],
            },
        })
        .await;
        let lines = a.settle().await;
        assert!(row_with(&lines, " ? Database").is_none(), "{lines:#?}");
        finish(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_question_unfocuses_a_waiting_row_and_the_next_keys_go_to_the_card() {
    steering(|mut a| async move {
        a.start("plan it", 1).await;
        a.paste("for later").key(KeyCode::Tab);
        let queued = a.next().await;
        a.accept(&queued).await;
        sleep(Duration::from_millis(600)).await;
        a.key(KeyCode::Up);
        let lines = a.settle().await;
        assert!(row_with(&lines, "enter edit").is_some(), "{lines:#?}");
        ask(&a, "q1", vec![database()]).await;
        a.key(KeyCode::Delete);
        a.nothing_sent().await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "enter edit").is_none(), "{lines:#?}");
        assert!(
            row_with(&lines, copy::CARD_SETTLING).is_some(),
            "{lines:#?}"
        );
        sleep(Duration::from_millis(500)).await;
        a.key(KeyCode::Enter);
        assert_eq!(answered(&a.next().await).1, vec![Answer::choose(0)]);
        finish(&a).await;
    })
    .await;
}

async fn no_clipboard_read(dir: &super::images::Dir) {
    real_wait(300).await;
    assert!(
        super::images::calls(dir).is_empty(),
        "{:?}",
        super::images::calls(dir)
    );
}

#[tokio::test(start_paused = true)]
async fn right_click_never_pastes_into_a_question_card_or_while_quitting() {
    let dir = super::images::Dir::new("right-question");
    dir.put("text", b"pasted words");
    let options = super::images::right_pasting(&dir, true);
    steering_on(Screen::new(30, 80), options, |mut a| async move {
        begun(&mut a, vec![database()]).await;
        sleep(Duration::from_secs(2)).await;
        assert!(
            a.screen
                .visible()
                .iter()
                .any(|l| l.contains("Which database"))
        );
        a.inputs.send(super::images::right_click()).unwrap();
        no_clipboard_read(&dir).await;
        assert!(
            !a.screen
                .visible()
                .iter()
                .any(|l| l.contains("pasted words"))
        );
        a.key(KeyCode::Esc);
        sleep(Duration::from_millis(50)).await;
        while a.commands.try_recv().is_ok() {}
        a.emit(cancelled()).await;
        sleep(Duration::from_millis(200)).await;
        a.start("again", 2).await;
        a.paste("/quit").key(KeyCode::Enter);
        sleep(Duration::from_millis(100)).await;
        a.inputs.send(super::images::right_click()).unwrap();
        no_clipboard_read(&dir).await;
        a.emit(cancelled()).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_question_waits_while_the_ctrl_o_picker_is_open_and_with_it_showing_ctrl_o_opens_transcript_mode()
 {
    steering(|mut a| async move {
        a.start("plan it", 1).await;
        a.emit(AgentEvent::BlockStarted {
            index: 2,
            kind: BlockKind::Thinking,
        })
        .await;
        a.emit(AgentEvent::ThinkingDelta {
            index: 2,
            text: "Placeholder thought.\n".to_owned(),
        })
        .await;
        a.emit(AgentEvent::BlockStopped { index: 2 }).await;
        a.settle().await;
        a.ctrl('o');
        let lines = a.settle().await;
        let purpose = wisp_render::transcript::copy::EVERY_CELL;
        assert!(row_with(&lines, purpose).is_some(), "{lines:#?}");
        ask(&a, "q1", vec![database()]).await;
        sleep(Duration::from_millis(600)).await;
        let lines = a.settle().await;
        assert!(row_with(&lines, purpose).is_some(), "{lines:#?}");
        let call = position(&lines, "Asking Database");
        assert!(lines[call + 1].starts_with('─'), "{lines:#?}");
        assert!(row_with(&lines, "Which database").is_none(), "{lines:#?}");
        a.key(KeyCode::Char('1'));
        a.nothing_sent().await;
        a.key(KeyCode::Esc);
        let lines = a.settle().await;
        assert!(row_with(&lines, purpose).is_none(), "{lines:#?}");
        assert!(row_with(&lines, "Which database").is_some(), "{lines:#?}");
        a.key(KeyCode::Char('1'));
        a.nothing_sent().await;
        a.ctrl('o');
        let lines = a.settle().await;
        assert!(
            lines
                .last()
                .unwrap()
                .contains(&wisp_tui::app::pager::return_keys().text()),
            "{lines:#?}"
        );
        a.key(KeyCode::Char('q'));
        finish(&a).await;
    })
    .await;
}

#[tokio::test]
async fn a_card_settles_while_each_frame_outlasts_the_next_timer() {
    let screen = Screen::new(30, 80);
    screen.slow_frames(Duration::from_millis(200));
    steering_on(screen, options(Start::Stalled), |mut a| async move {
        a.start("plan it", 1).await;
        sleep(Duration::from_millis(600)).await;
        a.key(KeyCode::Backspace);
        a.emit(AgentEvent::QuestionAsked {
            call_id: "q1".to_owned(),
            questions: vec![database()],
        })
        .await;
        let held =
            eventually(|| row_with(&a.screen.visible(), "1\u{2013}3 choose").is_some()).await;
        assert!(held, "{:#?}", a.screen.visible());
        a.screen.slow_frames(Duration::ZERO);
        finish(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn an_unsettled_question_keeps_keys_out_of_the_usage_page() {
    let mut opts = options(Start::Stalled);
    opts.card_setup = Some(super::endpoints::card_setup());
    let mut facts = super::facts();
    facts.endpoints = vec![super::session_account(
        wisp_render::chrome::Endpoint::Official,
    )];
    super::steering::steering_facts(Screen::new(30, 100), opts, facts, |mut a| async move {
        a.start("plan it", 1).await;
        a.paste("/usage").key(KeyCode::Enter);
        a.settle().await;
        super::sidebar::drain(&mut a).await;
        ask(&a, "q1", vec![database()]).await;
        a.settle().await;
        a.key(KeyCode::Enter);
        a.nothing_sent().await;
        sleep(Duration::from_millis(600)).await;
        a.key(KeyCode::Char('2'));
        let (_, answers) = answered(&a.next().await);
        a.emit(AgentEvent::QuestionClosed {
            call_id: "q1".to_owned(),
            how: QuestionEnd::Answered { answers },
        })
        .await;
        let lines = a.settle().await;
        assert!(
            lines.iter().any(|line| line.contains("r refresh")),
            "{lines:?}"
        );
        sleep(Duration::from_millis(600)).await;
        a.key(KeyCode::Esc);
        let lines = a.settle().await;
        assert!(
            !lines.iter().any(|line| line.contains("r refresh")),
            "{lines:?}"
        );
        finish(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_question_sets_aside_the_process_list_without_changing_its_frame() {
    for (rows, cols) in [(30, 40), (30, 80), (30, 120), (12, 80), (8, 80), (6, 80)] {
        for glyphs in [
            wisp_render::theme::glyphs::GlyphSet::Unicode,
            wisp_render::theme::glyphs::GlyphSet::Ascii,
        ] {
            let mut shots = Vec::new();
            for opened in [false, true] {
                let mut opts = options(Start::Stalled);
                opts.glyphs = glyphs;
                opts.open_processes = opened;
                let shots = &mut shots;
                steering_on(Screen::new(rows, cols), opts, |mut a| async move {
                    a.start("check the program", 1).await;
                    a.emit(super::processes::waiting_terminal(5)).await;
                    sleep(Duration::from_millis(600)).await;
                    ask(&a, "q1", vec![process_question(5)]).await;
                    shots.push(a.settle().await);
                    a.nothing_sent().await;
                    finish(&a).await;
                })
                .await;
            }
            assert_eq!(shots[0], shots[1], "{cols}x{rows}, {glyphs:?}");
            assert!(
                shots[1].iter().any(|line| line.contains("Process 5")),
                "{cols}x{rows}: {:?}",
                shots[1]
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn a_question_drops_process_list_keys_and_clears_the_old_stop_confirmation() {
    for watching in [false, true] {
        let mut opts = options(Start::Stalled);
        opts.open_processes = true;
        steering_on(Screen::new(30, 80), opts, |mut a| async move {
            a.start("check the program", 1).await;
            a.emit(super::processes::waiting_terminal(5)).await;
            a.emit(super::processes::waiting_terminal(6)).await;
            a.key(KeyCode::Down);
            if watching {
                a.key(KeyCode::Enter);
                a.settle().await;
            } else {
                a.key(KeyCode::Char('x'));
                a.nothing_sent().await;
            }
            ask(&a, "q1", vec![process_question(6)]).await;
            for _ in 0..3 {
                a.key(KeyCode::Char('x'))
                    .key(KeyCode::Char('x'))
                    .key(KeyCode::Enter);
                a.nothing_sent().await;
                sleep(Duration::from_millis(300)).await;
            }
            sleep(Duration::from_millis(600)).await;
            a.key(KeyCode::Char('2'));
            let (_, answers) = answered(&a.next().await);
            a.emit(AgentEvent::QuestionClosed {
                call_id: "q1".to_owned(),
                how: QuestionEnd::Answered { answers },
            })
            .await;
            let lines = a.settle().await;
            if watching {
                assert!(row_with(&lines, "Process 6").is_some(), "{lines:?}");
                a.key(KeyCode::Esc);
                a.settle().await;
            } else {
                assert!(row_with(&lines, "Processes").is_some(), "{lines:?}");
            }
            assert!(!a.screen.caret_line().contains("xx"), "{lines:?}");
            a.key(KeyCode::Char('x'));
            a.nothing_sent().await;
            a.key(KeyCode::Char('x'));
            assert_eq!(a.next().await, ClientCommand::StopProcess { id: 6 });
            finish(&a).await;
        })
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_pasted_own_answer_reaches_the_question_over_the_process_list() {
    let mut opts = options(Start::Stalled);
    opts.open_processes = true;
    steering_on(Screen::new(30, 100), opts, |mut a| async move {
        a.start("check the program", 1).await;
        a.emit(super::processes::waiting_terminal(5)).await;
        sleep(Duration::from_millis(600)).await;
        ask(&a, "q1", vec![database()]).await;
        sleep(Duration::from_millis(600)).await;
        a.paste("MySQL, please").key(KeyCode::Enter);
        assert_eq!(
            answered(&a.next().await).1,
            vec![Answer::own("MySQL, please")]
        );
        finish(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_returned_draft_under_a_question_keeps_edit_and_stop_keys() {
    for ctrl in [false, true] {
        let mut opts = options(Start::Stalled);
        opts.open_processes = true;
        steering_on(Screen::new(30, 100), opts, |mut a| async move {
            a.start("check the program", 1).await;
            a.emit(super::processes::waiting_terminal(5)).await;
            sleep(Duration::from_millis(600)).await;
            ask(&a, "q1", vec![database()]).await;
            sleep(Duration::from_millis(600)).await;
            a.key(KeyCode::Char('3'))
                .key(KeyCode::Char('d'))
                .key(KeyCode::Char('b'));
            a.settle().await;
            a.emit(AgentEvent::QuestionAsked {
                call_id: "q2".to_owned(),
                questions: vec![database()],
            })
            .await;
            sleep(Duration::from_millis(600)).await;
            a.key(KeyCode::Backspace);
            a.settle().await;
            assert!(
                a.screen
                    .visible()
                    .iter()
                    .any(|line| line.contains("› d") && !line.contains("› db")),
                "{:?}",
                a.screen.visible()
            );
            if ctrl {
                a.ctrl('c');
            } else {
                a.key(KeyCode::Esc);
            }
            assert_eq!(a.next().await, ClientCommand::Cancel);
            finish(&a).await;
        })
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_typed_server_question_opens_on_typing_and_up_reaches_not_now() {
    for not_now in [false, true] {
        steering(|mut a| async move {
            let question = Question {
                header: "Needs you".into(),
                question: "What should the issue be called?".into(),
                options: vec![option("Not now", "")],
                multi_select: false,
                source: Some(wisp_protocol::QuestionSource {
                    asked_by: wisp_protocol::AskedBy::Server {
                        name: "linear".into(),
                    },
                    typed: true,
                }),
            };
            begun(&mut a, vec![question]).await;
            let lines = a.settle().await;
            assert!(row_with(&lines, "Type your answer").is_some(), "{lines:?}");
            if not_now {
                a.key(KeyCode::Up);
            } else {
                a.paste("Flaky test in ci.yml");
            }
            a.key(KeyCode::Enter);
            let (_, answers) = answered(&a.next().await);
            assert_eq!(
                answers,
                vec![if not_now {
                    Answer::choose(0)
                } else {
                    Answer::own("Flaky test in ci.yml")
                }]
            );
            finish(&a).await;
        })
        .await;
    }
}
