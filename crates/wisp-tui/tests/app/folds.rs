use std::time::Duration;

use tokio::time::sleep;
use wisp_protocol::settings::Layer;
use wisp_protocol::settings::names::{DISPLAY_SIDEBAR_FOLDED, DISPLAY_SIDEBAR_PINNED};
use wisp_protocol::settings::{SettingName, SettingTarget, SettingValue, find};
use wisp_protocol::{AgentEvent, AgentState, ClientCommand, Question, QuestionOption, RequestId};
use wisp_render::theme::Mode;
use wisp_tui::app::Options;
use wisp_tui::term::{Input, KeyCode, MouseKind, SavedDisplay};

use super::Screen;
use super::agents::{entry, spawned};
use super::select::{locate, tinted};
use super::sidebar::{
    NOTES, ROWS, TREE, applied, base, both, drain, finish, kept, kept_options, mouse, names, notes,
    quit, snapshot, stage, talk,
};
use super::steering::{Agent, steering_on};
use crate::pages::docked;
use crate::shared::{catalog, plugin};
use crate::{Drive, shows};

const MARK: &str = "\u{25b8}";

fn folds_sent(commands: &[ClientCommand]) -> Vec<(RequestId, Vec<String>)> {
    assert!(
        commands
            .iter()
            .all(|c| !matches!(c, ClientCommand::SettingsUsed { .. }))
    );
    commands
        .iter()
        .filter_map(|command| match command {
            ClientCommand::ChangeSettings {
                request,
                changes,
                save,
            } if changes
                .iter()
                .any(|c| c.target.spec().name == DISPLAY_SIDEBAR_FOLDED) =>
            {
                assert!(save, "a fold is saved");
                assert_eq!(changes.len(), 1, "{changes:?}");
                match &changes[0].value {
                    Some(SettingValue::List(list)) => Some((*request, list.clone())),
                    other => panic!("not a list: {other:?}"),
                }
            }
            _ => None,
        })
        .collect()
}

pub(super) fn list_set(name: &str, list: &[&str]) -> AgentEvent {
    let spec = find(name).unwrap();
    let mut event = applied(
        SettingTarget::Name(SettingName::of(spec)),
        SettingValue::List(names(list)),
    );
    if let AgentEvent::SettingsChanged { changes, .. } = &mut event {
        changes[0].layer = Layer::Session;
    }
    event
}

fn answer(request: RequestId, list: &[&str], warnings: &[&str]) -> AgentEvent {
    let mut event = list_set(DISPLAY_SIDEBAR_FOLDED, list);
    if let AgentEvent::SettingsChanged {
        answers, changes, ..
    } = &mut event
    {
        *answers = Some(request);
        changes[0].warnings = warnings.iter().map(|w| (*w).to_owned()).collect();
    }
    event
}

fn refused(request: RequestId, reason: &str) -> AgentEvent {
    AgentEvent::Rejected {
        reason: reason.to_owned(),
        id: None,
        kind: None,
        request: Some(request),
    }
}

fn title_row(lines: &[String], title: &str) -> Option<usize> {
    lines.iter().position(|l| {
        l.chars()
            .skip(82)
            .collect::<String>()
            .trim_start()
            .starts_with(title)
    })
}

fn folded_title(lines: &[String], title: &str) -> bool {
    title_row(lines, title).is_some_and(|row| lines[row].contains(&format!("{title} {MARK}")))
}

pub(super) fn click(a: &Agent, column: u16, row: u16) {
    mouse(a, MouseKind::Press, column, row);
    mouse(a, MouseKind::Release, column, row);
}

async fn click_title(a: &Agent, title: &str) {
    sleep(Duration::from_millis(450)).await;
    let (column, row) = locate(&a.screen, &format!(" {title}"));
    click(a, column + 1, row);
}

fn transcript_lines(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .take(lines.len().saturating_sub(4))
        .map(|l| l.chars().take(80).collect::<String>())
        .collect()
}

pub(super) fn with_saved(saved: SavedDisplay) -> Options {
    let mut kept = kept();
    kept.saved = saved;
    let mut options = kept_options();
    options.kept = Some(kept);
    options
}

#[tokio::test(start_paused = true)]
async fn a_title_click_folds_quietly_and_a_second_click_opens_it_at_rest_and_mid_turn() {
    steering_on(Screen::new(ROWS, 120), kept_options(), |mut a| async move {
        talk(&mut a, 6).await;
        docked(&mut a).await;
        let start = a.settle().await;
        let frames = a.screen.full_frames();
        click_title(&a, "Notes").await;
        let lines = a.settle().await;
        assert!(folded_title(&lines, "Notes"), "{lines:#?}");
        assert!(!shows(&lines, "note 1"), "{lines:#?}");
        let sent = folds_sent(&drain(&mut a).await);
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].1, names(&[NOTES]));
        a.emit(answer(sent[0].0, &[NOTES], &[])).await;
        let lines = a.settle().await;
        assert!(folded_title(&lines, "Notes"), "{lines:#?}");
        assert_eq!(transcript_lines(&lines), transcript_lines(&start));
        a.start("Keep going", 2).await;
        drain(&mut a).await;
        let streaming = a.settle().await;
        click_title(&a, "Agents").await;
        let lines = a.settle().await;
        assert!(folded_title(&lines, "Agents"), "{lines:#?}");
        let sent = folds_sent(&drain(&mut a).await);
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].1, names(&[TREE, NOTES]));
        a.emit(answer(sent[0].0, &[TREE, NOTES], &[])).await;
        click_title(&a, "Notes").await;
        let lines = a.settle().await;
        assert!(!folded_title(&lines, "Notes"), "{lines:#?}");
        assert!(shows(&lines, "note 1"), "{lines:#?}");
        let sent = folds_sent(&drain(&mut a).await);
        assert_eq!(sent[0].1, names(&[TREE]));
        a.emit(answer(sent[0].0, &[TREE], &[])).await;
        let lines = a.settle().await;
        assert_eq!(transcript_lines(&lines), transcript_lines(&streaming));
        assert_eq!(a.screen.full_frames(), frames, "a fold re-wrapped");
        assert!(
            !lines.iter().any(|l| l.contains("sidebar_folded")
                || l.contains("Folded sections")
                || l.contains("Saved")),
            "{lines:#?}"
        );
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_refused_fold_puts_back_the_effective_list_unless_a_later_fold_waits() {
    steering_on(Screen::new(ROWS, 120), kept_options(), |mut a| async move {
        docked(&mut a).await;
        click_title(&a, "Notes").await;
        let lines = a.settle().await;
        assert!(folded_title(&lines, "Notes"));
        let sent = folds_sent(&drain(&mut a).await);
        a.emit(refused(sent[0].0, "the config file is read-only"))
            .await;
        let lines = a.settle().await;
        assert!(!folded_title(&lines, "Notes"), "{lines:#?}");
        assert!(shows(&lines, "note 1"), "{lines:#?}");
        assert!(shows(&lines, "the config file is read-only"), "{lines:#?}");
        click_title(&a, "Notes").await;
        a.settle().await;
        click_title(&a, "Agents").await;
        let lines = a.settle().await;
        let sent = folds_sent(&drain(&mut a).await);
        assert_eq!(sent.len(), 2, "{sent:?}");
        assert_eq!(sent[1].1, names(&[TREE, NOTES]));
        a.emit(refused(sent[0].0, "busy")).await;
        let after = a.settle().await;
        assert!(folded_title(&after, "Notes"), "{after:#?}");
        assert!(folded_title(&after, "Agents"), "{after:#?}");
        assert_eq!(
            after
                .iter()
                .map(|l| l.chars().skip(82).collect::<String>())
                .collect::<Vec<_>>(),
            lines
                .iter()
                .map(|l| l.chars().skip(82).collect::<String>())
                .collect::<Vec<_>>()
        );
        a.emit(answer(sent[1].0, &[TREE, NOTES], &[])).await;
        let lines = a.settle().await;
        assert!(folded_title(&lines, "Notes") && folded_title(&lines, "Agents"));
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn two_clicks_before_either_answer_keep_both_folds_with_no_flicker() {
    steering_on(Screen::new(ROWS, 120), kept_options(), |mut a| async move {
        docked(&mut a).await;
        click_title(&a, "Notes").await;
        a.settle().await;
        click_title(&a, "Agents").await;
        let lines = a.settle().await;
        let sent = folds_sent(&drain(&mut a).await);
        assert_eq!(
            sent.iter().map(|(_, l)| l.clone()).collect::<Vec<_>>(),
            [names(&[NOTES]), names(&[TREE, NOTES])]
        );
        a.emit(answer(sent[0].0, &[NOTES], &[])).await;
        let first = a.settle().await;
        assert_eq!(first, lines, "the first answer moved the panel");
        assert!(folded_title(&first, "Agents") && folded_title(&first, "Notes"));
        a.emit(answer(sent[1].0, &[TREE, NOTES], &[])).await;
        assert_eq!(a.settle().await, lines);
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_list_set_with_set_seeds_the_run_and_a_click_still_folds_and_saves() {
    let options =
        with_saved(SavedDisplay::from_pairs([]).with_flags([(DISPLAY_SIDEBAR_FOLDED, TREE)]));
    steering_on(Screen::new(ROWS, 120), options, |mut a| async move {
        docked(&mut a).await;
        let lines = a.settle().await;
        assert!(folded_title(&lines, "Agents"), "{lines:#?}");
        click_title(&a, "Notes").await;
        let lines = a.settle().await;
        assert!(folded_title(&lines, "Agents") && folded_title(&lines, "Notes"));
        let sent = folds_sent(&drain(&mut a).await);
        assert_eq!(sent[0].1, names(&[TREE, NOTES]));
        a.emit(answer(sent[0].0, &[TREE, NOTES], &[])).await;
        let lines = a.settle().await;
        assert!(folded_title(&lines, "Agents") && folded_title(&lines, "Notes"));
        click_title(&a, "Agents").await;
        let lines = a.settle().await;
        assert!(!folded_title(&lines, "Agents"), "{lines:#?}");
        let sent = folds_sent(&drain(&mut a).await);
        assert_eq!(sent[0].1, names(&[NOTES]));
        a.emit(answer(sent[0].0, &[NOTES], &[])).await;
        assert!(!folded_title(&a.settle().await, "Agents"));
        folds_sent(&drain(&mut a).await);
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_saved_name_that_is_no_section_is_kept_by_a_click_and_its_warning_never_drawn() {
    let gone = "gone/section";
    let options = with_saved(SavedDisplay::from_pairs([(DISPLAY_SIDEBAR_FOLDED, gone)]));
    steering_on(Screen::new(ROWS, 120), options, |mut a| async move {
        docked(&mut a).await;
        let lines = a.settle().await;
        assert!(!folded_title(&lines, "Agents") && !folded_title(&lines, "Notes"));
        click_title(&a, "Agents").await;
        a.settle().await;
        let sent = folds_sent(&drain(&mut a).await);
        assert_eq!(sent[0].1, names(&[TREE, gone]));
        let warning = "no sidebar section named gone/section; kept";
        a.emit(answer(sent[0].0, &[TREE, gone], &[warning])).await;
        let lines = a.settle().await;
        assert!(!shows(&lines, "no sidebar section named"), "{lines:#?}");
        assert!(folded_title(&lines, "Agents"));
        click_title(&a, "Agents").await;
        a.settle().await;
        let sent = folds_sent(&drain(&mut a).await);
        assert_eq!(sent[0].1, names(&[gone]));
        quit(&a).await;
    })
    .await;
}

async fn apart() {
    sleep(Duration::from_millis(450)).await;
}

fn question() -> AgentEvent {
    AgentEvent::QuestionAsked {
        call_id: "q1".to_owned(),
        questions: vec![Question {
            header: "Database".to_owned(),
            question: "Which database?".to_owned(),
            options: vec![
                QuestionOption {
                    label: "SQLite".to_owned(),
                    description: String::new(),
                },
                QuestionOption {
                    label: "PostgreSQL".to_owned(),
                    description: String::new(),
                },
            ],
            multi_select: false,
            source: None,
        }],
    }
}

#[tokio::test(start_paused = true)]
async fn a_refocus_click_a_click_over_a_selection_a_drag_and_a_press_under_a_picker_fold_nothing() {
    steering_on(Screen::new(ROWS, 120), kept_options(), |mut a| async move {
        talk(&mut a, 4).await;
        docked(&mut a).await;
        let start = a.settle().await;
        let (column, row) = locate(&a.screen, " Agents");
        let column = column + 1;
        a.inputs.send(Input::Focus(true)).unwrap();
        click(&a, column, row);
        assert_eq!(a.settle().await, start, "a refocus click folded");
        apart().await;
        let (col, line) = locate(&a.screen, "line 1 of");
        mouse(&a, MouseKind::Press, col, line);
        mouse(&a, MouseKind::Drag, col + 6, line);
        mouse(&a, MouseKind::Release, col + 6, line);
        a.settle().await;
        assert!(!tinted(&a.screen).is_empty(), "nothing selected");
        click(&a, column, row);
        let lines = a.settle().await;
        assert!(tinted(&a.screen).is_empty(), "the selection stayed");
        assert!(
            !folded_title(&lines, "Agents"),
            "a click over a selection folded"
        );
        apart().await;
        mouse(&a, MouseKind::Press, column, row);
        mouse(&a, MouseKind::Drag, column + 2, row);
        mouse(&a, MouseKind::Release, column + 2, row);
        let lines = a.settle().await;
        assert!(!folded_title(&lines, "Agents"), "a drag folded");
        apart().await;
        a.paste("/effort").key(KeyCode::Enter);
        a.settle().await;
        click(&a, column, row);
        let lines = a.settle().await;
        assert!(
            !folded_title(&lines, "Agents"),
            "a press under a picker folded"
        );
        apart().await;
        a.paste("/");
        let listed = a.settle().await;
        assert!(shows(&listed, "/ps  "), "{listed:#?}");
        click(&a, column, row);
        let lines = a.settle().await;
        assert!(
            !folded_title(&lines, "Agents"),
            "a press under suggestions folded"
        );
        a.key(KeyCode::Backspace);
        a.settle().await;
        apart().await;
        a.ctrl('t');
        let agents = a.settle().await;
        assert!(shows(&agents, "⠶ main"), "{agents:#?}");
        click(&a, column, row);
        let lines = a.settle().await;
        assert!(
            !folded_title(&lines, "Agents"),
            "a press with /agents open folded"
        );
        a.key(KeyCode::Esc);
        a.settle().await;
        apart().await;
        a.emit(question()).await;
        let carded = a.settle().await;
        assert!(shows(&carded, "Which database?"), "{carded:#?}");
        click(&a, column, row);
        let lines = a.settle().await;
        assert!(
            !folded_title(&lines, "Agents"),
            "a press with a question card open folded"
        );
        assert!(folds_sent(&drain(&mut a).await).is_empty());
        a.key(KeyCode::Esc);
        a.settle().await;
        drain(&mut a).await;
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn with_nothing_held_the_same_click_folds_once_and_a_double_click_saves_once() {
    steering_on(Screen::new(ROWS, 120), kept_options(), |mut a| async move {
        docked(&mut a).await;
        let (column, row) = locate(&a.screen, " Agents");
        click(&a, column + 1, row);
        click(&a, column + 1, row);
        let lines = a.settle().await;
        assert!(folded_title(&lines, "Agents"), "{lines:#?}");
        let sent = folds_sent(&drain(&mut a).await);
        assert_eq!(sent.len(), 1, "{sent:?}");
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_title_click_that_wobbles_one_column_still_folds() {
    steering_on(Screen::new(ROWS, 120), kept_options(), |mut a| async move {
        docked(&mut a).await;
        let (column, row) = locate(&a.screen, " Agents");
        mouse(&a, MouseKind::Press, column + 1, row);
        mouse(&a, MouseKind::Drag, column + 2, row);
        mouse(&a, MouseKind::Release, column + 2, row);
        let lines = a.settle().await;
        assert!(folded_title(&lines, "Agents"), "{lines:#?}");
        assert_eq!(folds_sent(&drain(&mut a).await).len(), 1);
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_title_press_whose_release_was_lost_never_folds_on_a_later_release() {
    steering_on(Screen::new(ROWS, 120), kept_options(), |mut a| async move {
        docked(&mut a).await;
        let (column, row) = locate(&a.screen, " Agents");
        mouse(&a, MouseKind::Press, column + 1, row);
        a.settle().await;
        apart().await;
        a.paste("/effort").key(KeyCode::Enter);
        a.settle().await;
        click(&a, column + 1, row);
        let lines = a.settle().await;
        assert!(!folded_title(&lines, "Agents"), "{lines:#?}");
        assert!(folds_sent(&drain(&mut a).await).is_empty());
        quit(&a).await;
    })
    .await;
}

fn overlay_folded(lines: &[String], title: &str) -> bool {
    lines
        .iter()
        .any(|l| l.contains(&format!("\u{2502} {title} {MARK}")))
}

#[tokio::test(start_paused = true)]
async fn a_title_click_in_the_overlay_folds_and_keeps_the_refocus_and_selection_guards() {
    steering_on(Screen::new(ROWS, 80), kept_options(), |mut a| async move {
        talk(&mut a, 4).await;
        docked(&mut a).await;
        a.paste("/sidebar").key(KeyCode::Enter);
        let lines = a.settle().await;
        assert!(shows(&lines, "scroll \u{b7} esc close"), "{lines:#?}");
        let (column, row) = locate(&a.screen, "\u{2502} Agents");
        let column = column + 2;
        a.inputs.send(Input::Focus(true)).unwrap();
        click(&a, column, row);
        let lines = a.settle().await;
        assert!(!overlay_folded(&lines, "Agents"), "a refocus click folded");
        apart().await;
        let (col, line) = locate(&a.screen, "line 1 of");
        mouse(&a, MouseKind::Press, col, line);
        mouse(&a, MouseKind::Drag, col + 6, line);
        mouse(&a, MouseKind::Release, col + 6, line);
        a.settle().await;
        assert!(!tinted(&a.screen).is_empty(), "nothing selected");
        click(&a, column, row);
        let lines = a.settle().await;
        assert!(tinted(&a.screen).is_empty(), "the selection stayed");
        assert!(
            !overlay_folded(&lines, "Agents"),
            "a click over a selection folded"
        );
        assert!(folds_sent(&drain(&mut a).await).is_empty());
        apart().await;
        click(&a, column, row);
        let lines = a.settle().await;
        assert!(overlay_folded(&lines, "Agents"), "{lines:#?}");
        let sent = folds_sent(&drain(&mut a).await);
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].1, names(&[TREE]));
        a.key(KeyCode::Esc);
        a.settle().await;
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_title_under_a_card_is_never_hit() {
    steering_on(Screen::new(ROWS, 120), kept_options(), |mut a| async move {
        a.emit(both()).await;
        a.emit(notes(2)).await;
        a.emit(super::sidebar::content(
            TREE,
            (0..16)
                .map(|i| wisp_protocol::slot::SlotBlock::Line {
                    spans: vec![super::sidebar::text(&format!("row {i}"))],
                })
                .collect(),
        ))
        .await;
        a.settle().await;
        drain(&mut a).await;
        let (column, row) = locate(&a.screen, " Notes");
        a.emit(spawned(entry("/review", AgentState::Running, 0.4)))
            .await;
        a.ctrl('t');
        let covered = a.settle().await;
        assert!(!covered[usize::from(row)].contains("Notes"), "{covered:#?}");
        click(&a, column + 1, row);
        a.settle().await;
        assert!(folds_sent(&drain(&mut a).await).is_empty());
        a.key(KeyCode::Esc);
        let lines = a.settle().await;
        assert!(!folded_title(&lines, "Notes"), "{lines:#?}");
        click(&a, column + 1, row);
        let lines = a.settle().await;
        assert!(folded_title(&lines, "Notes"), "{lines:#?}");
        assert_eq!(folds_sent(&drain(&mut a).await).len(), 1);
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn pinning_from_config_moves_the_section_to_the_top_with_its_note_and_rewraps_nothing() {
    steering_on(Screen::new(ROWS, 120), kept_options(), |mut a| async move {
        talk(&mut a, 4).await;
        docked(&mut a).await;
        let start = a.settle().await;
        assert!(title_row(&start, "Agents") < title_row(&start, "Notes"));
        let frames = a.screen.full_frames();
        a.emit(list_set(DISPLAY_SIDEBAR_PINNED, &[NOTES])).await;
        let lines = a.settle().await;
        assert_eq!(title_row(&lines, "Notes"), Some(1), "{lines:#?}");
        assert!(title_row(&lines, "Agents") > Some(1), "{lines:#?}");
        assert!(shows(&lines, "Pinned sections"), "{lines:#?}");
        assert_eq!(a.screen.full_frames(), frames, "pinning re-wrapped");
        quit(&a).await;
    })
    .await;
}

fn scene_options(mode: Mode) -> Options {
    let word = match mode {
        Mode::Dark => "dark",
        Mode::Light => "light",
    };
    with_saved(SavedDisplay::from_pairs([("display.theme", word)]))
}

async fn fold_scene(mode: Mode) -> String {
    let out = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
    let kept = out.clone();
    steering_on(
        Screen::new(ROWS, 160),
        scene_options(mode),
        |mut a| async move {
            base(&mut a).await;
            a.emit(list_set(DISPLAY_SIDEBAR_PINNED, &[TREE])).await;
            a.emit(catalog(vec![
                plugin(
                    "agent-viz",
                    true,
                    vec![super::sidebar::slot(TREE, "Agents")],
                ),
                plugin("demo", true, vec![super::sidebar::slot(NOTES, "Notes")]),
                plugin("aaa", true, vec![super::sidebar::slot("aaa/log", "Log")]),
            ]))
            .await;
            a.settle().await;
            click_title(&a, "Notes").await;
            a.settle().await;
            a.emit(super::sidebar::content(
                "aaa/log",
                vec![wisp_protocol::slot::SlotBlock::List {
                    items: (1..=22)
                        .map(|i| vec![super::sidebar::text(&format!("entry {i}"))])
                        .collect(),
                }],
            ))
            .await;
            a.settle().await;
            for _ in 0..2 {
                mouse(&a, MouseKind::WheelDown, 140, 20);
            }
            *kept.borrow_mut() = finish(a).await;
        },
    )
    .await;
    out.borrow().clone()
}

#[tokio::test(start_paused = true)]
async fn sidebar_fold_and_pin_scene_in_dark() {
    let shot = fold_scene(Mode::Dark).await;
    snapshot(
        "sidebar-folds-dark-truecolor-unicode",
        &[("160 columns, pinned, folded and scrolled".to_owned(), shot)],
    );
}

#[tokio::test(start_paused = true)]
async fn a_fold_click_in_a_session_without_a_kept_display_still_folds() {
    stage(120, |mut a| async move {
        docked(&mut a).await;
        click_title(&a, "Notes").await;
        let lines = a.settle().await;
        assert!(folded_title(&lines, "Notes"), "{lines:#?}");
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn another_display_answer_while_a_sidebar_save_waits_leaves_the_dock_alone() {
    steering_on(Screen::new(ROWS, 160), kept_options(), |mut a| async move {
        docked(&mut a).await;
        assert!(super::sidebar::rule_at(&a.screen, 119));
        let frames = a.screen.full_frames();
        a.paste("/sidebar").key(KeyCode::Enter);
        a.settle().await;
        assert!(super::sidebar::no_rule(&a.screen));
        let theme = find("display.theme").unwrap();
        a.emit(applied(
            SettingTarget::Name(SettingName::of(theme)),
            theme.parse("dark").unwrap(),
        ))
        .await;
        a.settle().await;
        assert!(
            super::sidebar::no_rule(&a.screen),
            "the theme answer docked it"
        );
        a.emit(super::sidebar::sidebar_set("hidden")).await;
        a.settle().await;
        assert!(super::sidebar::no_rule(&a.screen));
        assert_eq!(
            a.screen.full_frames() - frames,
            1,
            "the transcript moved twice"
        );
        quit(&a).await;
    })
    .await;
}
