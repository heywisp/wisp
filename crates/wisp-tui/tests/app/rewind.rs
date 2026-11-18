use std::time::Duration;

use tokio::time::sleep;
use wisp_protocol::{
    AgentEvent, CacheEffect, ClientCommand, Dropped, ImageData, InputId, MissingImage,
    NOTHING_TO_REWIND, PointInfo, RejectKind, RequestId, ReturnedInput, RewindPoint, RewindQuote,
    RewindReason, Warmth, WithdrawnBy,
};
use wisp_render::transcript::copy;
use wisp_tui::term::KeyCode;

use super::completed;
use super::steering::{Agent, cancelled, row_with, steering};
use crate::{Drive, status_line};

const PIXEL: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

pub(super) fn point(turn: u64, label: &str) -> PointInfo {
    PointInfo {
        point: RewindPoint::Turn { turn },
        label: label.to_owned(),
        at: "2026-10-26T14:02:00Z".to_owned(),
        inputs_preview: Vec::new(),
        quote: RewindQuote {
            prefix_tokens: 190_000,
            warm: Warmth::Warm,
            read_micros: Some(40_000),
            rewrite_micros: Some(700_000),
        },
        compaction: None,
        processes: Vec::new(),
    }
}

pub(super) fn rewound(request: RequestId, turn: u64, returned: Vec<ReturnedInput>) -> AgentEvent {
    AgentEvent::Rewound {
        answers: Some(request),
        point: RewindPoint::Turn { turn },
        reason: RewindReason::User,
        dropped: Dropped {
            turns: vec![turn],
            messages: 2,
        },
        returned,
        cache: CacheEffect::Kept,
    }
}

pub(super) fn returned(id: &InputId, text: &str) -> ReturnedInput {
    ReturnedInput {
        id: id.clone(),
        text: text.to_owned(),
        images: Vec::new(),
        missing: Vec::new(),
    }
}

async fn two_turns(a: &mut Agent) -> InputId {
    a.start("first", 1).await;
    a.emit(completed()).await;
    let second = a.start("second", 2).await;
    a.emit(completed()).await;
    second
}

pub(super) async fn asked(a: &mut Agent) -> RequestId {
    match a.next().await {
        ClientCommand::RewindPoints { request } => request,
        other => panic!("expected RewindPoints, got {other:?}"),
    }
}

async fn picked(a: &mut Agent) -> (RewindPoint, RequestId) {
    match a.next().await {
        ClientCommand::Rewind { to, request } => (to, request),
        other => panic!("expected Rewind, got {other:?}"),
    }
}

fn docked(lines: &[String]) -> bool {
    lines.iter().any(|l| l.trim() == copy::REWIND_NAME)
}

#[tokio::test(start_paused = true)]
async fn esc_esc_opens_the_picker_and_a_confirmed_rewind_cuts_the_transcript_and_restores_the_message()
 {
    steering(|mut a| async move {
        let second = two_turns(&mut a).await;
        a.key(KeyCode::Esc);
        let lines = a.settle().await;
        assert!(
            status_line(&lines).contains(&copy::again_to_rewind().text()),
            "{lines:#?}"
        );
        a.nothing_sent().await;
        a.key(KeyCode::Esc);
        let request = asked(&mut a).await;
        let loading = a.settle().await;
        assert!(docked(&loading), "{loading:#?}");
        assert!(row_with(&loading, "loading").is_some(), "{loading:#?}");
        assert!(row_with(&loading, "esc close").is_some(), "{loading:#?}");
        assert!(
            row_with(&loading, &copy::enter_choose().text()).is_none(),
            "{loading:#?}"
        );
        a.key(KeyCode::Enter);
        a.nothing_sent().await;
        a.emit(AgentEvent::RewindPoints {
            answers: request,
            points: vec![point(1, "first"), point(2, "second")],
        })
        .await;
        let lines = a.settle().await;
        assert!(docked(&lines), "{lines:#?}");
        assert!(
            row_with(&lines, &copy::enter_choose().text()).is_some(),
            "{lines:#?}"
        );
        let newest = row_with(&lines, "turn 2").unwrap_or_else(|| panic!("{lines:#?}"));
        assert!(
            newest.contains("14:02") && newest.contains("second"),
            "{lines:#?}"
        );
        assert!(newest.contains("cache warm"), "{lines:#?}");
        a.key(KeyCode::Enter);
        let lines = a.settle().await;
        let panel =
            row_with(&lines, "Rewind to before turn 2").unwrap_or_else(|| panic!("{lines:#?}"));
        assert!(panel.contains(copy::FILES_STAY), "{lines:#?}");
        a.key(KeyCode::Enter);
        let (to, request) = picked(&mut a).await;
        assert_eq!(to, RewindPoint::Turn { turn: 2 });
        a.emit(rewound(request, 2, vec![returned(&second, "second")]))
            .await;
        let lines = a.settle().await;
        let divider =
            row_with(&lines, "rewound to before turn 2").unwrap_or_else(|| panic!("{lines:#?}"));
        assert!(divider.contains("turn 2 set aside"), "{lines:#?}");
        assert!(divider.contains(copy::FILES_UNCHANGED), "{lines:#?}");
        assert!(row_with(&lines, "first").is_some(), "{lines:#?}");
        assert_eq!(
            lines.iter().filter(|l| l.contains("second")).count(),
            1,
            "{lines:#?}"
        );
        assert!(a.screen.caret_line().contains("second"), "{lines:#?}");
        a.key(KeyCode::Enter);
        let id = match a.next().await {
            ClientCommand::Send { id, text, .. } => {
                assert_eq!(text, "second");
                id
            }
            other => panic!("expected Send, got {other:?}"),
        };
        a.emit(AgentEvent::turn_started(3)).await;
        a.emit(AgentEvent::InputDelivered {
            ids: vec![id],
            step: 1,
        })
        .await;
        a.emit(AgentEvent::BlockStarted {
            index: 0,
            kind: wisp_protocol::BlockKind::Text,
        })
        .await;
        a.emit(AgentEvent::TextDelta {
            index: 0,
            text: "a fresh answer one\n\na fresh answer two\n\na fresh answer three\n".to_owned(),
        })
        .await;
        a.emit(AgentEvent::BlockStopped { index: 0 }).await;
        a.emit(completed()).await;
        let lines = a.settle().await;
        let divider = super::steering::position(&lines, "rewound to before turn 2");
        let resent = super::steering::position(&lines, "\u{203A} second");
        let answer = super::steering::position(&lines, "a fresh answer");
        assert!(divider < resent && resent < answer, "{lines:#?}");
        for part in ["one", "two", "three"] {
            let line = format!("a fresh answer {part}");
            assert_eq!(
                lines.iter().filter(|l| l.contains(&line)).count(),
                1,
                "{lines:#?}"
            );
        }
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_rewind_that_cuts_nothing_still_scrolls_main_chat_to_the_bottom() {
    steering(|mut a| async move {
        a.start("first", 1).await;
        let text: String = (1..=40).map(|n| format!("line {n} of forty\n\n")).collect();
        a.emit(AgentEvent::TextDelta { index: 0, text }).await;
        a.emit(completed()).await;
        for _ in 0..20 {
            a.key(KeyCode::PageUp);
        }
        let lines = a.settle().await;
        assert!(row_with(&lines, "line 1 of forty").is_some(), "{lines:#?}");
        a.paste("/rewind").key(KeyCode::Enter);
        let request = asked(&mut a).await;
        a.emit(AgentEvent::RewindPoints {
            answers: request,
            points: vec![point(2, "second")],
        })
        .await;
        a.key(KeyCode::Enter).key(KeyCode::Enter);
        let (_, request) = picked(&mut a).await;
        a.emit(rewound(request, 2, Vec::new())).await;
        let lines = a.settle().await;
        assert!(
            row_with(&lines, "rewound to before turn 2").is_some(),
            "{lines:#?}"
        );
        assert!(row_with(&lines, "line 1 of forty").is_none(), "{lines:#?}");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_held_message_comes_back_after_the_rewound_one() {
    steering(|mut a| async move {
        a.start("first", 1).await;
        a.emit(completed()).await;
        let second = a.start("second", 2).await;
        a.paste("for later").key(KeyCode::Tab);
        let queued = a.next().await;
        let queued = a.accept(&queued).await;
        a.key(KeyCode::Esc);
        assert_eq!(a.next().await, ClientCommand::Cancel);
        a.emit(AgentEvent::InputHeld {
            ids: vec![queued.clone()],
            by: WithdrawnBy::TurnStopped,
        })
        .await;
        a.emit(cancelled()).await;
        sleep(Duration::from_millis(1500)).await;
        a.paste("/rewind").key(KeyCode::Enter);
        let request = asked(&mut a).await;
        a.emit(AgentEvent::RewindPoints {
            answers: request,
            points: vec![point(1, "first"), point(2, "second")],
        })
        .await;
        a.key(KeyCode::Enter).key(KeyCode::Enter);
        let (_, request) = picked(&mut a).await;
        a.emit(AgentEvent::InputWithdrawn {
            ids: vec![queued.clone()],
            by: WithdrawnBy::Rewound,
        })
        .await;
        a.emit(rewound(
            request,
            2,
            vec![returned(&second, "second"), returned(&queued, "for later")],
        ))
        .await;
        let lines = a.settle().await;
        let restored = super::steering::position(&lines, "\u{203A} second");
        let later = super::steering::position(&lines, "for later");
        assert!(later > restored, "{lines:#?}");
        assert!(!lines.iter().any(|l| l.contains(copy::HELD)), "{lines:#?}");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_prime_lapses_after_a_second_and_any_other_key_cancels_it() {
    steering(|mut a| async move {
        two_turns(&mut a).await;
        a.key(KeyCode::Esc);
        sleep(Duration::from_millis(1100)).await;
        a.key(KeyCode::Esc);
        a.nothing_sent().await;
        sleep(Duration::from_millis(1100)).await;
        a.key(KeyCode::Esc).key(KeyCode::Left).key(KeyCode::Esc);
        a.nothing_sent().await;
        a.key(KeyCode::Esc);
        assert!(matches!(a.next().await, ClientCommand::RewindPoints { .. }));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_prime_hint_leaves_on_its_own_after_a_second_without_input() {
    steering(|mut a| async move {
        two_turns(&mut a).await;
        a.settle().await;
        a.key(KeyCode::Esc);
        let lines = a.settle().await;
        assert!(
            status_line(&lines).contains(&copy::again_to_rewind().text()),
            "{lines:#?}"
        );
        sleep(Duration::from_millis(1100)).await;
        let lines = a.screen.steady();
        assert!(
            !status_line(&lines).contains(&copy::again_to_rewind().text()),
            "{lines:#?}"
        );
        a.nothing_sent().await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn esc_while_running_stops_the_turn_and_does_not_prime() {
    steering(|mut a| async move {
        a.start("first", 1).await;
        a.key(KeyCode::Esc);
        assert_eq!(a.next().await, ClientCommand::Cancel);
        a.emit(cancelled()).await;
        a.key(KeyCode::Esc);
        a.nothing_sent().await;
        let lines = a.settle().await;
        assert!(!docked(&lines), "{lines:#?}");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn rewind_works_between_turns_and_undo_is_its_alias() {
    steering(|mut a| async move {
        a.start("first", 1).await;
        a.paste("/rewind").key(KeyCode::Enter);
        a.nothing_sent().await;
        let lines = a.settle().await;
        assert!(
            row_with(&lines, copy::REWIND_BETWEEN_TURNS).is_some(),
            "{lines:#?}"
        );
        assert!(a.screen.caret_line().contains("/rewind"), "{lines:#?}");
        a.emit(completed()).await;
        a.key(KeyCode::Enter);
        assert!(matches!(a.next().await, ClientCommand::RewindPoints { .. }));
        a.key(KeyCode::Esc);
        a.paste("/undo").key(KeyCode::Enter);
        assert!(matches!(a.next().await, ClientCommand::RewindPoints { .. }));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn no_points_closes_the_picker_with_a_hint() {
    steering(|mut a| async move {
        two_turns(&mut a).await;
        a.key(KeyCode::Esc).key(KeyCode::Esc);
        let request = asked(&mut a).await;
        a.emit(AgentEvent::RewindPoints {
            answers: request,
            points: Vec::new(),
        })
        .await;
        let lines = a.settle().await;
        assert!(row_with(&lines, NOTHING_TO_REWIND).is_some(), "{lines:#?}");
        assert!(!docked(&lines), "{lines:#?}");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_refused_rewind_clears_the_rewinding_hint_and_keeps_the_transcript() {
    steering(|mut a| async move {
        two_turns(&mut a).await;
        a.key(KeyCode::Esc).key(KeyCode::Esc);
        let request = asked(&mut a).await;
        a.emit(AgentEvent::RewindPoints {
            answers: request,
            points: vec![point(1, "first"), point(2, "second")],
        })
        .await;
        a.key(KeyCode::Enter).key(KeyCode::Enter);
        let (_, request) = picked(&mut a).await;
        let lines = a.settle().await;
        assert!(
            status_line(&lines).contains(copy::REWIND_SENT),
            "{lines:#?}"
        );
        a.emit(AgentEvent::Rejected {
            reason: wisp_protocol::SENT_BEFORE_REWIND.to_owned(),
            id: None,
            kind: Some(RejectKind::NotRewindable),
            request: Some(request),
        })
        .await;
        let lines = a.settle().await;
        assert!(
            !status_line(&lines).contains(copy::REWIND_SENT),
            "{lines:#?}"
        );
        assert!(
            row_with(&lines, "Not rewound: a message was sent before the rewind.").is_some(),
            "{lines:#?}"
        );
        assert!(
            row_with(&lines, "This message wasn't sent").is_none(),
            "{lines:#?}"
        );
        assert!(row_with(&lines, "second").is_some(), "{lines:#?}");
        assert!(
            row_with(&lines, "rewound to before").is_none(),
            "{lines:#?}"
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn an_input_this_client_never_held_comes_back_with_its_images_as_chips() {
    steering(|mut a| async move {
        two_turns(&mut a).await;
        a.key(KeyCode::Esc).key(KeyCode::Esc);
        let request = asked(&mut a).await;
        a.emit(AgentEvent::RewindPoints {
            answers: request,
            points: vec![point(1, "first"), point(2, "[Image #1] what is this")],
        })
        .await;
        a.key(KeyCode::Enter).key(KeyCode::Enter);
        let (_, request) = picked(&mut a).await;
        let elsewhere = ReturnedInput {
            id: InputId("other-1".to_owned()),
            text: "[Image #1] what is this".to_owned(),
            images: vec![ImageData { data: PIXEL.into() }],
            missing: vec![MissingImage {
                ordinal: 2,
                sha256: "b".repeat(64),
            }],
        };
        a.emit(rewound(request, 2, vec![elsewhere])).await;
        let lines = a.settle().await;
        assert!(
            lines
                .iter()
                .any(|l| l.contains("[Image #1") && l.contains("what is this")),
            "{lines:#?}"
        );
        assert!(
            row_with(&lines, &copy::image_not_restored(&[2])).is_some(),
            "{lines:#?}"
        );
    })
    .await;
}

fn marked_points() -> Vec<PointInfo> {
    let quote = |warm: Warmth, read: Option<u64>, rewrite: Option<u64>| RewindQuote {
        prefix_tokens: 190_000,
        warm,
        read_micros: read,
        rewrite_micros: rewrite,
    };
    let mut points = vec![
        PointInfo {
            quote: quote(Warmth::MayHaveExpired, Some(30_000), Some(710_000)),
            ..point(9, "add the retry tests to agent/tests.rs")
        },
        PointInfo {
            quote: quote(
                Warmth::OtherModel {
                    model: "claude-opus-5-5".to_owned(),
                },
                None,
                Some(1_200_000),
            ),
            ..point(10, "run them")
        },
        PointInfo {
            quote: quote(
                Warmth::OtherEndpoint {
                    name: "manifest".to_owned(),
                },
                None,
                None,
            ),
            ..point(11, "now fix the flaky one")
        },
        PointInfo {
            quote: quote(
                Warmth::OtherEffort {
                    effort: wisp_protocol::Effort::High,
                },
                None,
                Some(650_000),
            ),
            ..point(12, "\u{21BB} process 3 exited")
        },
        PointInfo {
            quote: quote(Warmth::PrefixChanged, None, Some(690_000)),
            ..point(13, "tighten the timeout")
        },
        point(14, "[Image #1] what is this error"),
    ];
    if let Some(last) = points.last_mut() {
        last.compaction = Some(wisp_protocol::CompactionNote {
            turn: 30,
            compacts_first: false,
        });
        last.processes = vec![3, 4];
    }
    points
}

async fn rewind_scene(mode: wisp_render::theme::Mode, width: u16) -> String {
    let screen = super::Screen::new(30, width);
    let mut options = wisp_tui::app::Options::new(
        wisp_render::theme::Theme::new(mode, wisp_render::theme::Depth::TrueColor),
        wisp_render::theme::glyphs::GlyphSet::Unicode,
    );
    options.code = wisp_tui::app::highlight::Start::Stalled;
    options.zone = jiff::tz::TimeZone::UTC;
    options.now = super::fixed_now;
    let shots = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
    let taken = std::rc::Rc::clone(&shots);
    let seen = screen.clone();
    super::steering::steering_on(screen, options, |mut a| async move {
        let shot = |name: &str| {
            taken
                .borrow_mut()
                .push_str(&format!("== {name}\n{}\n", seen.styled()));
        };
        let second = two_turns(&mut a).await;
        a.key(KeyCode::Esc);
        a.settle().await;
        shot("primed");
        a.key(KeyCode::Esc);
        let request = asked(&mut a).await;
        a.settle().await;
        shot("loading");
        a.emit(AgentEvent::RewindPoints {
            answers: request,
            points: marked_points(),
        })
        .await;
        a.settle().await;
        shot("picker");
        a.key(KeyCode::Enter);
        a.settle().await;
        shot("panel");
        a.key(KeyCode::Enter);
        let (_, request) = picked(&mut a).await;
        a.emit(rewound(request, 2, vec![returned(&second, "second")]))
            .await;
        a.settle().await;
        shot("after");
    })
    .await;
    shots.take()
}

async fn docked_at(width: u16) {
    let screen = super::Screen::new(30, width);
    let mut options = super::options(wisp_tui::app::highlight::Start::Stalled);
    options.zone = jiff::tz::TimeZone::UTC;
    options.now = super::fixed_now;
    let seen = screen.clone();
    super::steering::steering_on(screen, options, |mut a| async move {
        two_turns(&mut a).await;
        a.paste("/rewind").key(KeyCode::Enter);
        let request = asked(&mut a).await;
        a.emit(AgentEvent::RewindPoints {
            answers: request,
            points: vec![point(1, "first"), point(2, "second"), point(3, "third")],
        })
        .await;
        let lines = a.settle().await;
        let rule = lines
            .iter()
            .rposition(|l| !l.is_empty() && l.chars().all(|c| c == '\u{2500}'))
            .unwrap_or_else(|| panic!("{width}: no dock rule: {lines:#?}"));
        let title = super::steering::position(&lines, copy::REWIND_NAME);
        let rows: Vec<usize> = ["turn 1", "turn 2", "turn 3"]
            .iter()
            .map(|t| super::steering::position(&lines, t))
            .collect();
        let composer = lines
            .iter()
            .position(|l| l.starts_with('\u{256d}'))
            .unwrap_or_else(|| panic!("{width}: no composer: {lines:#?}"));
        assert_eq!(title, rule + 1, "{width}: {lines:#?}");
        assert!(
            rule < rows[0] && rows[0] < rows[1] && rows[1] < rows[2] && rows[2] < composer,
            "{width}: {lines:#?}"
        );
        assert!(
            lines[rows[2]].starts_with(" \u{203a} "),
            "{width}: {lines:#?}"
        );
        assert!(
            lines[..rule].iter().any(|l| l.contains("second")),
            "{width}: {lines:#?}"
        );
        if width >= 120 {
            assert!(lines[rows[0]].contains("first"), "{width}: {lines:#?}");
        }
        a.key(KeyCode::Up);
        let lines = a.settle().await;
        let two = super::steering::position(&lines, "turn 2");
        assert!(lines[two].starts_with(" \u{203a} "), "{width}: {lines:#?}");
        let (column, row) = super::select::locate(&seen, "turn 1");
        let click = super::select::at(wisp_tui::term::MouseKind::Press, column, row);
        a.inputs.send(click.clone()).unwrap();
        let lines = a.settle().await;
        let one = super::steering::position(&lines, "turn 1");
        assert!(lines[one].starts_with(" \u{203a} "), "{width}: {lines:#?}");
        a.nothing_sent().await;
        a.inputs.send(click).unwrap();
        let lines = a.settle().await;
        assert!(
            row_with(&lines, "Rewind to before turn 1").is_some(),
            "{width}: {lines:#?}"
        );
        a.key(KeyCode::Enter);
        let (to, _) = picked(&mut a).await;
        assert_eq!(to, RewindPoint::Turn { turn: 1 });
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_docked_picker_opens_lists_the_turns_and_selects_at_40_and_120_columns() {
    for width in [40, 120] {
        docked_at(width).await;
    }
}

#[tokio::test(start_paused = true)]
async fn the_picker_and_panel_at_80_and_120_in_light_and_dark() {
    use wisp_render::theme::Mode;
    let (mode, name) = (Mode::Dark, "dark");
    for width in [80, 120] {
        let body = rewind_scene(mode, width).await;
        let mut settings = insta::Settings::clone_current();
        settings.set_prepend_module_to_snapshot(false);
        settings.set_snapshot_path("../snapshots/chrome");
        settings.bind(|| {
            insta::assert_snapshot!(format!("rewind-{name}-truecolor-unicode-{width}"), body);
        });
    }
}

#[tokio::test(start_paused = true)]
async fn the_rewind_replay_fixture_keeps_the_first_turn_and_draws_the_divider() {
    let events = wisp_tui::app::replay::parse(include_str!("../fixtures/rewind.jsonl")).unwrap();
    let screen = super::Screen::new(30, 80);
    super::session(
        screen.clone(),
        vec![events],
        wisp_tui::app::highlight::Start::Stalled,
        |run| async move {
            sleep(Duration::from_millis(100)).await;
            run.say("hello");
            sleep(Duration::from_secs(2)).await;
            run.say("again");
            sleep(Duration::from_secs(2)).await;
            run.send(super::key(KeyCode::Esc));
            run.send(super::key(KeyCode::Esc));
            sleep(Duration::from_millis(200)).await;
            let lines = run.screen.visible();
            assert!(docked(&lines), "{lines:#?}");
            run.send(super::key(KeyCode::Enter));
            run.send(super::key(KeyCode::Enter));
            sleep(Duration::from_millis(500)).await;
            let lines = run.screen.visible();
            assert!(row_with(&lines, "Hello back.").is_some(), "{lines:#?}");
            assert!(row_with(&lines, "Again, hello.").is_none(), "{lines:#?}");
            assert!(
                row_with(&lines, "rewound to before turn 2").is_some(),
                "{lines:#?}"
            );
            assert!(run.screen.caret_line().contains("again"), "{lines:#?}");
            run.send(super::ctrl('d'));
        },
    )
    .await
    .unwrap();
}

fn long(word: &str) -> String {
    (1..=10)
        .map(|i| format!("{word} {i}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test(start_paused = true)]
async fn pasted_text_comes_back_as_its_chip_for_the_rewound_and_the_held_message() {
    steering(|mut a| async move {
        let first = a.start(&long("alpha"), 1).await;
        a.emit(completed()).await;
        let second = a.start(&long("beta"), 2).await;
        a.paste(&long("gamma")).paste(" later").key(KeyCode::Tab);
        let queued = a.next().await;
        let queued = a.accept(&queued).await;
        a.key(KeyCode::Esc);
        assert_eq!(a.next().await, ClientCommand::Cancel);
        a.emit(AgentEvent::InputHeld {
            ids: vec![queued.clone()],
            by: WithdrawnBy::TurnStopped,
        })
        .await;
        a.emit(cancelled()).await;
        sleep(Duration::from_millis(1500)).await;
        a.paste("/rewind").key(KeyCode::Enter);
        let request = asked(&mut a).await;
        a.emit(AgentEvent::RewindPoints {
            answers: request,
            points: vec![point(1, "alpha 1"), point(2, "beta 1")],
        })
        .await;
        a.key(KeyCode::Up).key(KeyCode::Enter).key(KeyCode::Enter);
        let (to, request) = picked(&mut a).await;
        assert_eq!(to, RewindPoint::Turn { turn: 1 });
        a.emit(AgentEvent::InputWithdrawn {
            ids: vec![queued.clone()],
            by: WithdrawnBy::Rewound,
        })
        .await;
        a.emit(AgentEvent::Rewound {
            answers: Some(request),
            point: RewindPoint::Turn { turn: 1 },
            reason: RewindReason::User,
            dropped: Dropped {
                turns: vec![1, 2],
                messages: 4,
            },
            returned: vec![
                returned(&first, &long("alpha")),
                returned(&queued, &format!("{} later", long("gamma"))),
            ],
            cache: CacheEffect::Kept,
        })
        .await;
        let lines = a.settle().await;
        assert!(
            row_with(&lines, "[Pasted text #1 +10 lines]").is_some(),
            "{lines:#?}"
        );
        assert!(
            row_with(&lines, "[Pasted text #2 +10 lines]").is_some(),
            "{lines:#?}"
        );
        for raw in ["alpha 3", "gamma 3", "beta"] {
            assert!(row_with(&lines, raw).is_none(), "{raw}: {lines:#?}");
        }
        drop(second);
        a.key(KeyCode::Enter);
        match a.next().await {
            ClientCommand::Send { text, .. } => {
                assert_eq!(
                    text,
                    format!("{}\n\n{} later", long("alpha"), long("gamma"))
                );
            }
            other => panic!("expected Send, got {other:?}"),
        }
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_mouse_press_clears_the_prime_so_the_next_esc_only_primes_again() {
    steering(|mut a| async move {
        two_turns(&mut a).await;
        a.key(KeyCode::Esc);
        let lines = a.settle().await;
        assert!(
            status_line(&lines).contains(&copy::again_to_rewind().text()),
            "{lines:#?}"
        );
        a.inputs
            .send(super::select::at(wisp_tui::term::MouseKind::Press, 2, 2))
            .unwrap();
        let lines = a.settle().await;
        assert!(
            !status_line(&lines).contains(&copy::again_to_rewind().text()),
            "{lines:#?}"
        );
        a.key(KeyCode::Esc);
        a.nothing_sent().await;
        let lines = a.settle().await;
        assert!(
            status_line(&lines).contains(&copy::again_to_rewind().text()),
            "{lines:#?}"
        );
        a.key(KeyCode::Esc);
        let request = asked(&mut a).await;
        a.emit(AgentEvent::RewindPoints {
            answers: request,
            points: vec![point(1, "first"), point(2, "second")],
        })
        .await;
        assert!(docked(&a.settle().await));
        a.inputs
            .send(super::select::at(wisp_tui::term::MouseKind::Press, 2, 2))
            .unwrap();
        assert!(!docked(&a.settle().await));
        a.nothing_sent().await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_point_from_an_earlier_day_shows_its_date() {
    steering(|mut a| async move {
        two_turns(&mut a).await;
        a.key(KeyCode::Esc).key(KeyCode::Esc);
        let request = asked(&mut a).await;
        a.emit(AgentEvent::RewindPoints {
            answers: request,
            points: vec![
                PointInfo {
                    at: "2026-10-25T14:02:00Z".to_owned(),
                    ..point(1, "first")
                },
                point(2, "second"),
            ],
        })
        .await;
        let lines = a.settle().await;
        let older = row_with(&lines, "turn 1").unwrap_or_else(|| panic!("{lines:#?}"));
        assert!(older.contains("Oct 25 14:02"), "{lines:#?}");
        let newer = row_with(&lines, "turn 2").unwrap_or_else(|| panic!("{lines:#?}"));
        assert!(
            newer.contains("14:02") && !newer.contains("Oct"),
            "{lines:#?}"
        );
    })
    .await;
}

async fn rewound_with_a_missing_image(a: &mut Agent, text: &str) {
    two_turns(a).await;
    a.key(KeyCode::Esc).key(KeyCode::Esc);
    let request = asked(a).await;
    a.emit(AgentEvent::RewindPoints {
        answers: request,
        points: vec![point(1, "first"), point(2, "second")],
    })
    .await;
    a.key(KeyCode::Enter).key(KeyCode::Enter);
    let (_, request) = picked(a).await;
    let input = ReturnedInput {
        missing: vec![MissingImage {
            ordinal: 1,
            sha256: "a".repeat(64),
        }],
        ..returned(&InputId("resumed-2".to_owned()), text)
    };
    a.emit(rewound(request, 2, vec![input])).await;
}

#[tokio::test(start_paused = true)]
async fn an_image_only_message_comes_back_empty_with_the_line_until_sent() {
    steering(|mut a| async move {
        rewound_with_a_missing_image(&mut a, "").await;
        let note = copy::image_not_restored(&[1]);
        let lines = a.settle().await;
        assert!(row_with(&lines, &note).is_some(), "{lines:#?}");
        assert!(
            a.screen.caret_line().contains(copy::PLACEHOLDER_SHORT),
            "{lines:#?}"
        );
        sleep(Duration::from_secs(30)).await;
        let lines = a.settle().await;
        assert!(row_with(&lines, &note).is_some(), "{lines:#?}");
        a.key(KeyCode::Esc).key(KeyCode::Esc);
        asked(&mut a).await;
        let lines = a.settle().await;
        assert!(docked(&lines), "{lines:#?}");
        assert!(row_with(&lines, &note).is_none(), "{lines:#?}");
        a.key(KeyCode::Esc);
        let lines = a.settle().await;
        assert!(!docked(&lines), "{lines:#?}");
        assert!(row_with(&lines, &note).is_some(), "{lines:#?}");
        a.paste("try again").key(KeyCode::Enter);
        assert!(matches!(a.next().await, ClientCommand::Send { .. }));
        let lines = a.settle().await;
        assert!(row_with(&lines, &note).is_none(), "{lines:#?}");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn ctrl_c_dismisses_the_line_and_typing_never_brings_it_back() {
    steering(|mut a| async move {
        rewound_with_a_missing_image(&mut a, "look at this").await;
        let note = copy::image_not_restored(&[1]);
        let lines = a.settle().await;
        assert!(row_with(&lines, &note).is_some(), "{lines:#?}");
        a.ctrl('c');
        let lines = a.settle().await;
        assert!(row_with(&lines, &note).is_none(), "{lines:#?}");
        a.paste("something new");
        let lines = a.settle().await;
        assert!(row_with(&lines, &note).is_none(), "{lines:#?}");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_rewind_refused_for_a_running_turn_shows_the_tui_key_hint() {
    steering(|mut a| async move {
        two_turns(&mut a).await;
        a.key(KeyCode::Esc).key(KeyCode::Esc);
        let request = asked(&mut a).await;
        a.emit(AgentEvent::RewindPoints {
            answers: request,
            points: vec![point(1, "first"), point(2, "second")],
        })
        .await;
        a.key(KeyCode::Enter).key(KeyCode::Enter);
        let (_, request) = picked(&mut a).await;
        a.emit(AgentEvent::Rejected {
            reason: wisp_protocol::REWIND_WHILE_RUNNING.to_owned(),
            id: None,
            kind: Some(RejectKind::TurnRunning),
            request: Some(request),
        })
        .await;
        let lines = a.settle().await;
        assert!(
            row_with(&lines, &copy::not_rewound(copy::REWIND_BETWEEN_TURNS)).is_some(),
            "{lines:#?}"
        );
        assert!(
            row_with(&lines, wisp_protocol::REWIND_WHILE_RUNNING).is_none(),
            "{lines:#?}"
        );
    })
    .await;
}
