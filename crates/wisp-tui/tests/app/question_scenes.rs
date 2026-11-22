use wisp_protocol::{
    AgentEvent, Answer, BlockKind, NotRunReason, OutputKind, Question, QuestionEnd, StopReason,
    ToolAction, ToolOutcome,
};
use wisp_render::motion::MotionPolicy;
use wisp_render::theme::Mode;
use wisp_render::theme::glyphs::GlyphSet;
use wisp_tui::term::{Input, KeyCode};

use super::{
    Look, MODES, Stage, cancelled, completed, cost, ended, snapshot, staged, te, truecolor,
};
use crate::Drive;
use crate::agents::main_spent;
use crate::events::{Requested, tool_requested};
use crate::questions::option;

const CARD_ROWS: u16 = 40;
const COMPACT_ROWS: u16 = 8;

fn database() -> Question {
    Question {
        header: "Database".to_owned(),
        question: "Which database should the notes service use?".to_owned(),
        options: vec![
            option(
                "PostgreSQL (Recommended)",
                "Relational; the billing service already runs it, so backups, monitoring and \
                 the on-call runbook carry over without new work.",
            ),
            option("SQLite", "One file, no server; fine for a single writer."),
            option("DynamoDB", "Managed, but ties the service to AWS."),
        ],
        multi_select: false,
        source: None,
    }
}

fn features() -> Question {
    Question {
        header: "Features".to_owned(),
        question: "Which features should ship first?".to_owned(),
        options: vec![
            option("Auth", "Email and passkeys."),
            option("Search", "Full-text over notes."),
            option("Export", "CSV and JSON."),
        ],
        multi_select: true,
        source: None,
    }
}

fn infrastructure() -> Question {
    Question {
        header: "Infrastructure".to_owned(),
        question: "Where will it run?".to_owned(),
        options: vec![
            option("Fly.io (Recommended)", "One command to deploy."),
            option("A VPS", "Cheapest, but you patch it yourself."),
        ],
        multi_select: false,
        source: None,
    }
}

pub(super) fn naming() -> Question {
    Question {
        header: "Name".to_owned(),
        question: "What should the crate be called?".to_owned(),
        options: vec![option("notes-service", ""), option("jot", "")],
        multi_select: false,
        source: None,
    }
}

fn four() -> Vec<Question> {
    vec![database(), features(), infrastructure(), naming()]
}

fn requested(step: u32, block: usize, id: &str, questions: &[Question]) -> AgentEvent {
    let headers: Vec<&str> = questions.iter().map(|q| q.header.as_str()).collect();
    tool_requested(Requested {
        step,
        block,
        call_id: id.to_owned(),
        tool: "ask_user".to_owned(),
        action: ToolAction::Other {
            name: "ask_user".to_owned(),
        },
        target: headers.join(", "),
        output: OutputKind::None,
        ..Requested::default()
    })
}

pub(super) fn closed(id: &str, how: QuestionEnd) -> AgentEvent {
    AgentEvent::QuestionClosed {
        call_id: id.to_owned(),
        how,
    }
}

pub(super) async fn asked(s: &Stage, step: u32, id: &str, questions: Vec<Question>, spent: f64) {
    asked_with(s, step, id, questions, None, spent).await;
}

async fn asked_with(
    s: &Stage,
    step: u32,
    id: &str,
    questions: Vec<Question>,
    also: Option<AgentEvent>,
    spent: f64,
) {
    s.event(te::step(step)).await;
    s.event(AgentEvent::BlockStarted {
        index: 0,
        kind: BlockKind::ToolUse {
            writing: None,
            name: "ask_user".to_owned(),
        },
    })
    .await;
    s.event(requested(step, 0, id, &questions)).await;
    if let Some(event) = also {
        s.event(event).await;
    }
    s.event(main_spent((spent, Some(0.0), false), &[])).await;
    s.event(te::step_ended(step, 0.004)).await;
    s.event(te::started(id)).await;
    s.event(AgentEvent::QuestionAsked {
        call_id: id.to_owned(),
        questions,
    })
    .await;
}

pub(super) fn done(id: &str) -> AgentEvent {
    te::finished(
        id,
        ToolOutcome::Completed {
            is_error: false,
            exit: None,
        },
        "",
        0,
        9_000,
    )
}

async fn card(s: Stage) {
    s.send("Scaffold the notes service").await;
    asked(&s, 1, "q", four(), 0.004).await;
    s.wait(700).await.shot("first");
    s.status("waiting");
    s.key(KeyCode::Down).key(KeyCode::Char('1'));
    s.wait(100).await.shot("multi");
    s.key(KeyCode::Char('1')).key(KeyCode::Char('3'));
    s.key(KeyCode::Char('4'));
    s.input(Input::Paste("Webhooks for new notes".to_owned()));
    s.wait(100).await.shot("own");
    s.key(KeyCode::Tab).key(KeyCode::Tab).key(KeyCode::Tab);
    s.wait(100).await.shot("review");
}

async fn single(s: Stage) {
    s.send("Scaffold the notes service").await;
    asked(&s, 1, "q", vec![database()], 0.004).await;
    s.wait(700).await.shot("one");
    s.key(KeyCode::Down).key(KeyCode::Down).key(KeyCode::Down);
    s.input(Input::Paste("MySQL, because the team\nknows it".to_owned()));
    s.wait(100).await.shot("typing");
}

async fn compact(s: Stage) {
    s.send("Scaffold the notes service").await;
    asked(&s, 1, "q", four(), 0.004).await;
    s.wait(700).await.shot("compact");
    s.key(KeyCode::Down);
    s.wait(100).await.shot("second option");
}

async fn rows(s: Stage) {
    s.send("Scaffold the notes service").await;
    asked_with(
        &s,
        1,
        "a",
        vec![database(), features(), infrastructure()],
        Some(te::requested(1, 1, "b", "cargo new notes-service")),
        0.004,
    )
    .await;
    s.wait(700).await;
    let answers = vec![
        Answer::choose(0),
        Answer {
            chosen: vec![0, 2],
            own: Some("Webhooks for new notes".to_owned()),
        },
        Answer::skipped(),
    ];
    s.event(closed("a", QuestionEnd::Answered { answers }))
        .await;
    s.event(done("a")).await;
    s.event(te::finished(
        "b",
        te::not_run(NotRunReason::AfterQuestion),
        "",
        0,
        0,
    ))
    .await;
    s.event(ended(
        completed(StopReason::EndTurn, true, Some(cost(0.004, 0.004, false))),
        9_000,
    ))
    .await;
    for (turn, (id, how)) in [
        ("c", QuestionEnd::Stopped),
        ("d", QuestionEnd::MessageInstead),
        ("e", QuestionEnd::Interrupted),
    ]
    .into_iter()
    .enumerate()
    {
        s.wait(1000)
            .await
            .send(&format!("Ask again {}", turn + 1))
            .await;
        asked(&s, 1, id, vec![naming()], 0.004 * (turn as f64 + 2.0)).await;
        s.wait(700).await;
        s.event(closed(id, how)).await;
        s.event(done(id)).await;
        s.event(ended(cancelled(), 2_000)).await;
    }
    s.wait(1000).await.send("Ask and stop").await;
    asked(&s, 1, "f", vec![naming()], 0.02).await;
    s.wait(700).await.shot("open");
    s.event(closed("f", QuestionEnd::Stopped)).await;
    s.event(ended(cancelled(), 2_000)).await;
    s.wait(500).await.shot("rows");
}

fn looks(rows: u16) -> Vec<Look> {
    let mut looks = vec![];
    for mode in MODES {
        for width in [40, 80, 120, 160, 200] {
            looks.push(Look {
                rows,
                ..truecolor(mode, GlyphSet::Unicode, width)
            });
        }
    }
    for width in [40, 80, 120, 160, 200] {
        looks.push(Look {
            rows,
            ..truecolor(Mode::Dark, GlyphSet::Ascii, width)
        });
        looks.push(Look {
            rows,
            motion: MotionPolicy::Reduced,
            ..truecolor(Mode::Dark, GlyphSet::Unicode, width)
        });
    }
    looks
}

#[tokio::test(start_paused = true)]
async fn the_question_card_from_40_to_200_columns_in_light_dark_ascii_and_reduced_motion() {
    for look in looks(CARD_ROWS) {
        snapshot("ask-card", look, &staged(look, card).await);
        snapshot("ask-single", look, &staged(look, single).await);
    }
}

#[tokio::test(start_paused = true)]
async fn the_compact_card_on_a_short_terminal() {
    for look in looks(COMPACT_ROWS) {
        snapshot("ask-compact", look, &staged(look, compact).await);
    }
}

#[tokio::test(start_paused = true)]
async fn question_rows_in_every_ending_from_40_to_200_columns() {
    for look in looks(70) {
        snapshot("ask-rows", look, &staged(look, rows).await);
    }
}
