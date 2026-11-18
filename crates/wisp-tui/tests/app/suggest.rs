use std::time::Duration;

use tokio::time::sleep;
use wisp_protocol::context::ReportDetail;
use wisp_protocol::settings::SettingChange;
use wisp_protocol::{AgentEvent, ClientCommand, Effort, QuestionEnd};
use wisp_render::theme::glyphs::GlyphSet;
use wisp_render::theme::{Depth, Mode, Theme};
use wisp_render::transcript::copy;
use wisp_tui::app::SessionFacts;
use wisp_tui::app::highlight::Start;
use wisp_tui::term::{Input, Key, KeyCode, Mouse, MouseKind};

use super::help::{ask, snapshot, styled};
use super::steering::{Agent, cancelled, steering_facts, steering_on};
use super::{Screen, facts, options};
use crate::{Drive, shows, status_line};
use wisp_tui::app::commands::{self, CONTEXT_WORDS, CommandId};

const HINTS: &str = "esc close \u{b7} tab complete \u{b7} enter run";
const HELP_TITLE: &str = "Help: commands and keys";

fn summary(id: CommandId) -> &'static str {
    commands::get(id).unwrap().summary
}

fn keys(a: &Agent, text: &str) {
    for c in text.chars() {
        a.inputs
            .send(Input::Key(Key::plain(KeyCode::Char(c))))
            .unwrap();
    }
}

fn rule(lines: &[String]) -> usize {
    super::composer_top(lines)
}

fn listed(lines: &[String]) -> Vec<String> {
    let top = super::frame_top(lines);
    lines[..top]
        .iter()
        .rev()
        .take_while(|l| {
            l.starts_with("   /")
                || l.starts_with(" \u{203A} /")
                || l.contains("no command matches")
        })
        .map(|l| l.trim_end().to_owned())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn composer(lines: &[String]) -> String {
    super::unframed(&lines[rule(lines) + 1])
}

fn click(row: u16, column: u16) -> Input {
    Input::Mouse(Mouse {
        kind: MouseKind::Press,
        column,
        row,
        modified: false,
    })
}

fn release(row: u16, column: u16) -> Input {
    Input::Mouse(Mouse {
        kind: MouseKind::Release,
        column,
        row,
        modified: false,
    })
}

fn context_of(command: &ClientCommand) -> ReportDetail {
    match command {
        ClientCommand::ContextReport { detail, .. } => detail.clone(),
        other => panic!("not a context report: {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn slash_opens_every_command_and_typing_filters_by_prefix() {
    steering_on(
        Screen::new(24, 80),
        options(Start::Stalled),
        |a| async move {
            let before = a.settle().await;
            assert!(shows(&before, copy::PLACEHOLDER), "{before:#?}");
            assert!(listed(&before).is_empty(), "{before:#?}");
            keys(&a, "/");
            let lines = a.settle().await;
            let rows = listed(&lines);
            assert_eq!(rows.len(), 8, "{lines:#?}");
            assert!(rows[0].starts_with(" \u{203A} /model [id]"), "{rows:#?}");
            assert!(rows[0].ends_with(summary(CommandId::Model)), "{rows:#?}");
            assert!(rows[4].starts_with("   /compact [focus]"), "{rows:#?}");
            assert!(rows[5].starts_with("   /rewind, /undo"), "{rows:#?}");
            assert!(rows[6].starts_with("   /config [filter]"), "{rows:#?}");
            assert!(rows[7].starts_with("   /ps"), "{rows:#?}");
            assert_eq!(rule(&lines), rule(&before), "the transcript moved");
            assert!(status_line(&lines).contains(HINTS), "{lines:#?}");
            keys(&a, "e");
            let lines = a.settle().await;
            let rows = listed(&lines);
            assert_eq!(rows.len(), 2, "{lines:#?}");
            assert!(rows[0].ends_with(summary(CommandId::Effort)));
            assert!(rows[1].ends_with(summary(CommandId::Exit)));
            assert_eq!(composer(&lines), " \u{203a} /e");
            keys(&a, "x");
            let lines = a.settle().await;
            assert_eq!(listed(&lines).len(), 1, "{lines:#?}");
            assert!(listed(&lines)[0].ends_with(summary(CommandId::Exit)));
            a.ctrl('c');
            let lines = a.settle().await;
            assert!(listed(&lines).is_empty(), "{lines:#?}");
            assert!(!status_line(&lines).contains(HINTS), "{lines:#?}");
            a.ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn no_match_shows_one_row_and_enter_refuses_the_unknown_word() {
    steering_on(
        Screen::new(24, 80),
        options(Start::Stalled),
        |mut a| async move {
            keys(&a, "/hepl");
            let lines = a.settle().await;
            assert_eq!(
                listed(&lines),
                ["   no command matches /hepl"],
                "{lines:#?}"
            );
            a.key(KeyCode::Tab);
            let lines = a.settle().await;
            assert_eq!(composer(&lines), " \u{203a} /hepl");
            a.key(KeyCode::Enter);
            a.nothing_sent().await;
            let lines = a.settle().await;
            assert!(shows(&lines, "/hepl isn't a command"), "{lines:#?}");
            assert_eq!(composer(&lines), " \u{203a} /hepl");
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn enter_runs_the_highlighted_command_as_if_its_name_were_typed() {
    steering_on(
        Screen::new(24, 80),
        options(Start::Stalled),
        |a| async move {
            keys(&a, "/");
            a.key(KeyCode::Enter);
            let lines = a.settle().await;
            assert!(
                shows(&lines, "type to filter"),
                "the model picker: {lines:#?}"
            );
            assert!(listed(&lines).is_empty(), "{lines:#?}");
            a.key(KeyCode::Esc);
            keys(&a, "/hel");
            a.key(KeyCode::Enter);
            let lines = a.settle().await;
            assert!(shows(&lines, HELP_TITLE), "{lines:#?}");
            a.key(KeyCode::Esc);
            let lines = a.settle().await;
            assert_eq!(composer(&lines), format!(" \u{203a} {}", copy::PLACEHOLDER));
            keys(&a, "/a");
            a.key(KeyCode::Enter);
            let lines = a.settle().await;
            assert!(shows(&lines, "Sign in to another account"), "{lines:#?}");
            a.key(KeyCode::Esc).ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn choosing_from_the_list_and_typing_in_full_send_the_same_command_and_history() {
    steering_on(
        Screen::new(24, 80),
        options(Start::Stalled),
        |mut a| async move {
            keys(&a, "/co");
            a.key(KeyCode::Enter);
            let chosen = context_of(&a.next().await);
            keys(&a, "/co");
            a.key(KeyCode::Tab);
            let lines = a.settle().await;
            assert_eq!(composer(&lines), " \u{203a} /context");
            a.key(KeyCode::Enter);
            let tabbed = context_of(&a.next().await);
            a.paste("/context").key(KeyCode::Enter);
            let typed = context_of(&a.next().await);
            assert_eq!(chosen, ReportDetail::Summary);
            assert_eq!(tabbed, chosen);
            assert_eq!(typed, chosen);
            keys(&a, "/hel");
            a.key(KeyCode::Enter);
            a.nothing_sent().await;
            let from_list = a.settle().await;
            a.key(KeyCode::Esc);
            a.paste("/help").key(KeyCode::Enter);
            a.nothing_sent().await;
            let typed = a.settle().await;
            a.key(KeyCode::Esc);
            assert!(shows(&from_list, HELP_TITLE), "{from_list:#?}");
            assert!(shows(&typed, HELP_TITLE), "{typed:#?}");
            for recalled in ["/help", "/context", "/context", "/context"] {
                a.key(KeyCode::Up);
                let lines = a.settle().await;
                assert_eq!(composer(&lines), format!(" \u{203a} {recalled}"));
                assert!(listed(&lines).is_empty(), "{lines:#?}");
            }
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn tab_completes_and_a_click_on_a_row_completes_it_without_running_it() {
    let screen = Screen::new(24, 80);
    steering_on(
        screen.clone(),
        options(Start::Stalled),
        |mut a| async move {
            keys(&a, "/e");
            a.key(KeyCode::Tab);
            let lines = a.settle().await;
            assert_eq!(composer(&lines), " \u{203a} /effort");
            assert!(listed(&lines).is_empty(), "{lines:#?}");
            a.ctrl('c');
            keys(&a, "/he");
            a.key(KeyCode::Tab);
            let lines = a.settle().await;
            assert_eq!(composer(&lines), " \u{203a} /help");
            assert_eq!(listed(&lines).len(), 1, "{lines:#?}");
            a.ctrl('c');
            keys(&a, "/a");
            let lines = a.settle().await;
            let row = lines
                .iter()
                .position(|l| l.contains(summary(CommandId::Endpoint)))
                .unwrap();
            let row = u16::try_from(row).unwrap();
            a.inputs.send(click(row, 6)).unwrap();
            a.inputs.send(release(row, 6)).unwrap();
            a.nothing_sent().await;
            let lines = a.settle().await;
            assert_eq!(composer(&lines), " \u{203a} /account");
            assert!(listed(&lines).is_empty(), "{lines:#?}");
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_click_elsewhere_closes_the_list_and_still_does_what_it_would() {
    let screen = Screen::new(24, 80);
    steering_on(screen.clone(), options(Start::Stalled), |a| async move {
        keys(&a, "/e");
        let lines = a.settle().await;
        let input = u16::try_from(rule(&lines) + 1).unwrap();
        a.inputs.send(click(input, 5)).unwrap();
        a.inputs.send(release(input, 5)).unwrap();
        let lines = a.settle().await;
        assert!(listed(&lines).is_empty(), "{lines:#?}");
        assert_eq!(composer(&lines), " \u{203a} /e");
        keys(&a, "x");
        let lines = a.settle().await;
        assert_eq!(
            composer(&lines),
            " \u{203a} /xe",
            "the click moved the cursor"
        );
        a.ctrl('c');
        keys(&a, "/e");
        a.settle().await;
        a.inputs.send(click(1, 2)).unwrap();
        let lines = a.settle().await;
        assert!(listed(&lines).is_empty(), "{lines:#?}");
        a.inputs.send(release(1, 2)).unwrap();
        keys(&a, "f");
        let lines = a.settle().await;
        assert_eq!(listed(&lines).len(), 1, "a new word reopens it: {lines:#?}");
        a.ctrl('c').ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn esc_closes_the_list_until_the_word_changes() {
    steering_on(
        Screen::new(24, 80),
        options(Start::Stalled),
        |a| async move {
            keys(&a, "/e");
            a.key(KeyCode::Esc);
            let lines = a.settle().await;
            assert!(listed(&lines).is_empty(), "{lines:#?}");
            assert_eq!(composer(&lines), " \u{203a} /e");
            a.key(KeyCode::Down);
            keys(&a, "f");
            let lines = a.settle().await;
            assert_eq!(listed(&lines).len(), 1, "{lines:#?}");
            assert!(listed(&lines)[0].ends_with(summary(CommandId::Effort)));
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_recalled_command_opens_no_list_until_it_is_edited() {
    steering_on(
        Screen::new(24, 80),
        options(Start::Stalled),
        |mut a| async move {
            a.paste("/context").key(KeyCode::Enter);
            a.next().await;
            a.key(KeyCode::Up);
            let lines = a.settle().await;
            assert_eq!(composer(&lines), " \u{203a} /context");
            assert!(listed(&lines).is_empty(), "{lines:#?}");
            a.key(KeyCode::Backspace);
            let lines = a.settle().await;
            assert_eq!(listed(&lines).len(), 1, "{lines:#?}");
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn while_busy_tab_esc_and_up_belong_to_the_open_list() {
    steering_on(
        Screen::new(24, 80),
        options(Start::Stalled),
        |mut a| async move {
            a.start("first", 1).await;
            a.paste("later").key(KeyCode::Tab);
            let queued = a.next().await;
            a.accept(&queued).await;
            keys(&a, "/fo");
            a.key(KeyCode::Tab);
            a.nothing_sent().await;
            let lines = a.settle().await;
            assert_eq!(composer(&lines), " \u{203a} /fo");
            a.key(KeyCode::Backspace).key(KeyCode::Backspace);
            keys(&a, "e");
            let lines = a.settle().await;
            assert!(!shows(&lines, "later"), "waiting rows hide: {lines:#?}");
            assert!(status_line(&lines).contains("1 waiting"), "{lines:#?}");
            assert!(status_line(&lines).contains(BUSY_HINTS), "{lines:#?}");
            a.key(KeyCode::Up);
            let lines = a.settle().await;
            assert!(
                listed(&lines)[1].ends_with(summary(CommandId::Exit)),
                "{lines:#?}"
            );
            assert!(!shows(&lines, "later"), "{lines:#?}");
            a.key(KeyCode::Tab);
            a.nothing_sent().await;
            let lines = a.settle().await;
            assert_eq!(composer(&lines), " \u{203a} /exit");
            a.key(KeyCode::Esc);
            a.nothing_sent().await;
            let lines = a.settle().await;
            assert!(listed(&lines).is_empty(), "{lines:#?}");
            assert!(shows(&lines, "later"), "{lines:#?}");
            assert!(
                status_line(&lines).contains(&copy::tab_queues().text()),
                "{lines:#?}"
            );
            a.key(KeyCode::Esc);
            assert_eq!(a.next().await, ClientCommand::Cancel);
            a.emit(cancelled()).await;
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_question_hides_the_list_and_the_next_edit_after_it_reopens_it() {
    steering_on(
        Screen::new(30, 80),
        options(Start::Stalled),
        |mut a| async move {
            a.start("plan it", 1).await;
            keys(&a, "/e");
            let lines = a.settle().await;
            assert_eq!(listed(&lines).len(), 2, "{lines:#?}");
            sleep(Duration::from_millis(600)).await;
            ask(&a).await;
            let lines = a.settle().await;
            assert!(listed(&lines).is_empty(), "{lines:#?}");
            assert!(shows(&lines, "Which database"), "{lines:#?}");
            a.emit(AgentEvent::QuestionClosed {
                call_id: "q1".to_owned(),
                how: QuestionEnd::Interrupted,
            })
            .await;
            let lines = a.settle().await;
            assert!(!shows(&lines, "PostgreSQL"), "{lines:#?}");
            assert!(listed(&lines).is_empty(), "{lines:#?}");
            assert_eq!(composer(&lines), " \u{203a} /e");
            keys(&a, "f");
            let lines = a.settle().await;
            assert_eq!(listed(&lines).len(), 1, "{lines:#?}");
            a.nothing_sent().await;
            a.emit(cancelled()).await;
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn small_terminals_scroll_two_rows_and_below_that_the_line_still_runs() {
    steering_on(
        Screen::new(6, 80),
        options(Start::Stalled),
        |a| async move {
            keys(&a, "/");
            let lines = a.settle().await;
            let rows = listed(&lines);
            assert_eq!(rows.len(), 2, "{lines:#?}");
            assert!(rows[0].ends_with(summary(CommandId::Model)), "{rows:#?}");
            a.key(KeyCode::Down).key(KeyCode::Down);
            let lines = a.settle().await;
            let rows = listed(&lines);
            assert_eq!(rows.len(), 2, "{lines:#?}");
            assert!(rows[1].ends_with(summary(CommandId::Endpoint)), "{rows:#?}");
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
    steering_on(
        Screen::new(5, 80),
        options(Start::Stalled),
        |a| async move {
            keys(&a, "/hel");
            let lines = a.settle().await;
            assert!(!shows(&lines, "Show commands"), "{lines:#?}");
            assert!(!status_line(&lines).contains("tab complete"), "{lines:#?}");
            keys(&a, "p");
            a.key(KeyCode::Enter);
            let lines = a.settle().await;
            assert!(shows(&lines, HELP_TITLE), "{lines:#?}");
            a.key(KeyCode::Esc).ctrl('c').ctrl('c');
        },
    )
    .await;
}

fn reversed_across(style: &str, cols: usize) -> bool {
    let Some(spans) = style.trim().strip_prefix("~ ") else {
        return false;
    };
    let mut next = 0;
    for span in spans.split("; ") {
        let (range, looks) = span.split_once(' ').unwrap_or((span, ""));
        let (from, to) = range.split_once("..").unwrap();
        if from.parse::<usize>().unwrap() != next || !looks.contains("inverse") {
            return false;
        }
        next = to.parse().unwrap();
    }
    next == cols
}

#[tokio::test(start_paused = true)]
async fn without_colour_the_highlighted_row_is_reverse_video() {
    let mut opts = options(Start::Stalled);
    opts.theme = Theme::new(Mode::Dark, Depth::None);
    steering_on(Screen::new(24, 80), opts, |a| async move {
        keys(&a, "/e");
        a.settle().await;
        let shot = a.screen.styled();
        let style_of = |needle: &str| {
            let mut lines = shot.lines();
            lines.find(|l| l.contains(needle)).unwrap();
            lines.next().unwrap_or_default().to_owned()
        };
        assert!(
            reversed_across(&style_of(summary(CommandId::Effort)), 80),
            "{shot}"
        );
        assert!(
            !style_of(summary(CommandId::Exit)).contains("inverse"),
            "{shot}"
        );
        a.key(KeyCode::Down);
        a.settle().await;
        let shot = a.screen.styled();
        let mut lines = shot.lines();
        lines
            .find(|l| l.contains(summary(CommandId::Exit)))
            .unwrap();
        assert!(reversed_across(lines.next().unwrap(), 80), "{shot}");
        a.ctrl('c').ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn an_exact_name_alias_or_case_is_sent_and_remembered_as_typed() {
    steering_on(
        Screen::new(24, 80),
        options(Start::Stalled),
        |mut a| async move {
            keys(&a, "/Context");
            let lines = a.settle().await;
            assert_eq!(listed(&lines).len(), 1, "{lines:#?}");
            a.key(KeyCode::Enter);
            assert_eq!(context_of(&a.next().await), ReportDetail::Summary);
            keys(&a, "/HELP");
            a.key(KeyCode::Enter);
            let lines = a.settle().await;
            assert!(shows(&lines, HELP_TITLE), "{lines:#?}");
            a.key(KeyCode::Esc);
            for recalled in ["/HELP", "/Context"] {
                a.key(KeyCode::Up);
                let lines = a.settle().await;
                assert_eq!(composer(&lines), format!(" \u{203a} {recalled}"));
            }
            a.ctrl('c');
            keys(&a, "/QUIT");
            a.key(KeyCode::Enter);
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn clearing_forgets_the_highlight_and_the_dismissal() {
    steering_on(
        Screen::new(24, 80),
        options(Start::Stalled),
        |a| async move {
            keys(&a, "/e");
            a.key(KeyCode::Down).key(KeyCode::Esc);
            a.ctrl('c');
            a.paste("/e");
            let lines = a.settle().await;
            assert_eq!(listed(&lines).len(), 2, "{lines:#?}");
            a.key(KeyCode::Enter);
            let lines = a.settle().await;
            assert!(shows(&lines, "next turn"), "the effort picker: {lines:#?}");
            a.key(KeyCode::Esc).ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn suggest_snapshots_at_80_and_120_in_light_dark_and_ascii() {
    let looks = [
        (Mode::Dark, GlyphSet::Unicode, "dark-truecolor-unicode"),
        (Mode::Dark, GlyphSet::Ascii, "dark-truecolor-ascii"),
    ];
    for (mode, glyphs, look) in looks {
        for (cols, rows) in [(80, 24), (120, 40)] {
            let screen = Screen::new(rows, cols);
            steering_on(screen.clone(), styled(mode, glyphs), |a| async move {
                keys(&a, "/e");
                a.settle().await;
                snapshot(&format!("suggest-{look}-{cols}x{rows}"), a.screen.styled());
                a.ctrl('c').ctrl('c').ctrl('c');
            })
            .await;
        }
    }
}

#[tokio::test(start_paused = true)]
async fn suggest_snapshots_while_busy_with_no_match_and_at_30_columns() {
    steering_on(
        Screen::new(40, 120),
        styled(Mode::Dark, GlyphSet::Unicode),
        |mut a| async move {
            a.start("Make cache misses name their cause", 1).await;
            sleep(Duration::from_secs(2)).await;
            keys(&a, "/");
            a.settle().await;
            snapshot(
                "suggest-busy-dark-truecolor-unicode-120x40",
                a.screen.styled(),
            );
            a.key(KeyCode::Esc).key(KeyCode::Esc);
            a.next().await;
            a.emit(cancelled()).await;
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
    steering_on(
        Screen::new(24, 80),
        styled(Mode::Dark, GlyphSet::Unicode),
        |a| async move {
            keys(&a, "/fo");
            a.settle().await;
            snapshot(
                "suggest-none-dark-truecolor-unicode-80x24",
                a.screen.styled(),
            );
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
    steering_on(
        Screen::new(12, 30),
        styled(Mode::Dark, GlyphSet::Unicode),
        |a| async move {
            keys(&a, "/e");
            a.settle().await;
            snapshot("suggest-dark-truecolor-unicode-30x12", a.screen.styled());
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
}

const BUSY_HINTS: &str = "1 waiting \u{b7} esc close \u{b7} tab complete \u{b7} enter run";
const ARG_HINTS: &str = "esc close \u{b7} enter run";

fn on_opus() -> SessionFacts {
    SessionFacts {
        model: "claude-opus-5-5".to_owned(),
        effort: "high".to_owned(),
        endpoints: vec![crate::session_account(
            wisp_render::chrome::Endpoint::Official,
        )],
        ..facts()
    }
}

fn changes_of(command: &ClientCommand) -> Vec<SettingChange> {
    match command {
        ClientCommand::ChangeSettings { changes, .. } => changes.clone(),
        other => panic!("not a settings change: {other:?}"),
    }
}

fn model_change(id: &str) -> Vec<SettingChange> {
    ClientCommand::shortcut_changes(None, Some(id.to_owned()), None)
}

fn effort_change(level: Effort) -> Vec<SettingChange> {
    ClientCommand::shortcut_changes(None, None, Some(level))
}

fn over_rule(lines: &[String], count: usize) -> Vec<String> {
    let top = super::frame_top(lines);
    lines[top.saturating_sub(count)..top]
        .iter()
        .map(|l| l.trim_end().to_owned())
        .collect()
}

#[tokio::test(start_paused = true)]
async fn after_effort_and_a_space_the_rows_are_the_effort_pickers_levels() {
    steering_facts(
        Screen::new(24, 80),
        options(Start::Stalled),
        on_opus(),
        |mut a| async move {
            keys(&a, "/effort x");
            let lines = a.settle().await;
            let rows = over_rule(&lines, 6);
            assert!(rows[0].starts_with("   Effort opus-5-5"), "{rows:#?}");
            assert!(rows[0].ends_with("next turn"), "{rows:#?}");
            let levels: Vec<&str> = rows[1..]
                .iter()
                .map(|r| {
                    r.trim_start_matches([' ', '\u{203A}'])
                        .split_whitespace()
                        .next()
                        .unwrap_or_default()
                })
                .collect();
            assert_eq!(levels, ["low", "medium", "high", "xhigh", "max"]);
            assert!(rows[3].starts_with("   high \u{2713}"), "{rows:#?}");
            assert!(status_line(&lines).contains(ARG_HINTS), "{lines:#?}");
            assert!(!status_line(&lines).contains("tab complete"), "{lines:#?}");
            a.key(KeyCode::Enter);
            assert_eq!(changes_of(&a.next().await), effort_change(Effort::Xhigh));
            let lines = a.settle().await;
            assert_eq!(composer(&lines), format!(" \u{203a} {}", copy::PLACEHOLDER));
            assert!(!shows(&lines, "next turn"), "{lines:#?}");
            keys(&a, "/effort m");
            a.key(KeyCode::Enter);
            a.nothing_sent().await;
            let lines = a.settle().await;
            assert!(shows(&lines, "not `m`"), "{lines:#?}");
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn enter_never_guesses_a_model_from_the_open_list() {
    steering_facts(
        Screen::new(24, 80),
        options(Start::Stalled),
        on_opus(),
        |mut a| async move {
            keys(&a, "/model sonnet");
            let lines = a.settle().await;
            assert!(shows(&lines, "sonnet-4-6"), "{lines:#?}");
            assert!(shows(&lines, "sonnet-5-5"), "{lines:#?}");
            a.key(KeyCode::Enter);
            a.describe().await;
            let lines = a.settle().await;
            assert!(shows(&lines, "matching \"sonnet\""), "{lines:#?}");
            a.key(KeyCode::Esc);
            a.paste("/model sonnet").key(KeyCode::Enter);
            a.describe().await;
            a.key(KeyCode::Esc);
            keys(&a, "/model sonnet");
            a.key(KeyCode::Down).key(KeyCode::Down).key(KeyCode::Enter);
            assert_eq!(
                changes_of(&a.next().await),
                model_change("claude-sonnet-4-6")
            );
            keys(&a, "/model claude-sonnet-5-5");
            a.key(KeyCode::Enter);
            assert_eq!(
                changes_of(&a.next().await),
                model_change("claude-sonnet-5-5")
            );
            keys(&a, "/model sonnet-5-5");
            a.key(KeyCode::Enter);
            assert_eq!(
                changes_of(&a.next().await),
                model_change("claude-sonnet-5-5")
            );
            for recalled in [
                "/model claude-sonnet-5-5",
                "/model claude-sonnet-4-6",
                "/model sonnet",
                "/model sonnet",
            ] {
                a.key(KeyCode::Up);
                let lines = a.settle().await;
                assert_eq!(composer(&lines), format!(" \u{203a} {recalled}"));
            }
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn choosing_a_value_and_typing_it_in_full_send_the_same_command_and_history() {
    steering_facts(
        Screen::new(24, 80),
        options(Start::Stalled),
        on_opus(),
        |mut a| async move {
            keys(&a, "/effort ");
            a.key(KeyCode::Down).key(KeyCode::Down).key(KeyCode::Enter);
            let chosen = changes_of(&a.next().await);
            keys(&a, "/effort me");
            a.key(KeyCode::Tab);
            let lines = a.settle().await;
            assert_eq!(composer(&lines), " \u{203a} /effort medium");
            assert!(shows(&lines, "Effort opus-5-5"), "still listed: {lines:#?}");
            a.key(KeyCode::Enter);
            let tabbed = changes_of(&a.next().await);
            a.paste("/effort medium").key(KeyCode::Enter);
            let typed = changes_of(&a.next().await);
            assert_eq!(chosen, effort_change(Effort::Medium));
            assert_eq!(tabbed, chosen);
            assert_eq!(typed, chosen);
            for _ in 0..3 {
                a.key(KeyCode::Up);
                let lines = a.settle().await;
                assert_eq!(composer(&lines), " \u{203a} /effort medium");
            }
            a.ctrl('c');
            keys(&a, "/context ");
            let lines = a.settle().await;
            let words = over_rule(&lines, CONTEXT_WORDS.len());
            for ((word, summary), row) in CONTEXT_WORDS.iter().zip(&words) {
                assert!(row.starts_with(&format!("   {word} ")), "{words:#?}");
                assert!(row.ends_with(summary), "{words:#?}");
            }
            a.key(KeyCode::Enter);
            assert_eq!(context_of(&a.next().await), ReportDetail::Summary);
            keys(&a, "/context ");
            a.key(KeyCode::Down).key(KeyCode::Enter);
            let chosen = context_of(&a.next().await);
            a.paste("/context all").key(KeyCode::Enter);
            assert_eq!(context_of(&a.next().await), chosen);
            assert_eq!(chosen, ReportDetail::All);
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn an_endpoint_word_is_never_turned_into_a_name_it_only_resembles() {
    steering_facts(
        Screen::new(24, 80),
        options(Start::Stalled),
        on_opus(),
        |mut a| async move {
            keys(&a, "/endpoint man");
            a.key(KeyCode::Enter);
            a.nothing_sent().await;
            let from_list = status_line(&a.settle().await);
            a.paste("/endpoint man").key(KeyCode::Enter);
            a.nothing_sent().await;
            let typed = status_line(&a.settle().await);
            assert_eq!(from_list, typed);
            assert!(typed.contains("No account \"man\""), "{typed}");
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_click_on_a_value_completes_it_without_running_it() {
    steering_facts(
        Screen::new(24, 80),
        options(Start::Stalled),
        on_opus(),
        |mut a| async move {
            keys(&a, "/effort ");
            let lines = a.settle().await;
            let row = lines
                .iter()
                .position(|l| l.starts_with("   xhigh"))
                .unwrap();
            let row = u16::try_from(row).unwrap();
            a.inputs.send(click(row, 6)).unwrap();
            a.inputs.send(release(row, 6)).unwrap();
            a.nothing_sent().await;
            let lines = a.settle().await;
            assert_eq!(composer(&lines), " \u{203a} /effort xhigh");
            let header = lines
                .iter()
                .position(|l| l.contains("Effort opus-5-5"))
                .unwrap();
            let header = u16::try_from(header).unwrap();
            a.inputs.send(click(header, 6)).unwrap();
            a.inputs.send(release(header, 6)).unwrap();
            a.nothing_sent().await;
            let lines = a.settle().await;
            assert!(!shows(&lines, "Effort opus-5-5"), "{lines:#?}");
            assert_eq!(composer(&lines), " \u{203a} /effort xhigh");
            a.key(KeyCode::Enter);
            assert_eq!(changes_of(&a.next().await), effort_change(Effort::Xhigh));
            a.ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn typing_into_the_argument_list_reaches_the_composer() {
    steering_facts(
        Screen::new(24, 80),
        options(Start::Stalled),
        on_opus(),
        |mut a| async move {
            keys(&a, "/model op");
            a.key(KeyCode::Backspace);
            keys(&a, "pus-5");
            let lines = a.settle().await;
            assert_eq!(composer(&lines), " \u{203a} /model opus-5");
            assert!(shows(&lines, "opus-5-5"), "{lines:#?}");
            assert!(!shows(&lines, "opus-4-8"), "{lines:#?}");
            a.key(KeyCode::Esc);
            let lines = a.settle().await;
            assert!(!shows(&lines, "matching \""), "{lines:#?}");
            assert_eq!(composer(&lines), " \u{203a} /model opus-5");
            keys(&a, "-5");
            let lines = a.settle().await;
            assert!(
                shows(&lines, "matching \"opus-5-5\""),
                "a new word reopens it: {lines:#?}"
            );
            a.ctrl('c');
            keys(&a, "/model ");
            a.key(KeyCode::Enter);
            a.describe().await;
            let lines = a.settle().await;
            assert!(shows(&lines, "type to filter"), "the picker: {lines:#?}");
            a.key(KeyCode::Esc).ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn suggest_args_snapshots_at_80_and_120_in_light_dark_and_ascii() {
    let looks = [
        (Mode::Dark, GlyphSet::Unicode, "dark-truecolor-unicode"),
        (Mode::Dark, GlyphSet::Ascii, "dark-truecolor-ascii"),
    ];
    for (mode, glyphs, look) in looks {
        for (cols, rows) in [(80, 24), (120, 40)] {
            let screen = Screen::new(rows, cols);
            steering_facts(
                screen.clone(),
                styled(mode, glyphs),
                on_opus(),
                |a| async move {
                    keys(&a, "/effort x");
                    a.settle().await;
                    snapshot(
                        &format!("suggest-args-{look}-{cols}x{rows}"),
                        a.screen.styled(),
                    );
                    a.ctrl('c').ctrl('c').ctrl('c');
                },
            )
            .await;
        }
    }
}

#[tokio::test(start_paused = true)]
async fn suggest_args_snapshots_while_busy_with_no_match_words_and_at_30_columns() {
    steering_facts(
        Screen::new(40, 120),
        styled(Mode::Dark, GlyphSet::Unicode),
        on_opus(),
        |mut a| async move {
            a.start("Make cache misses name their cause", 1).await;
            sleep(Duration::from_secs(2)).await;
            keys(&a, "/model s");
            a.settle().await;
            snapshot(
                "suggest-args-busy-dark-truecolor-unicode-120x40",
                a.screen.styled(),
            );
            a.key(KeyCode::Esc).key(KeyCode::Esc);
            a.next().await;
            a.emit(cancelled()).await;
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
    steering_facts(
        Screen::new(24, 80),
        styled(Mode::Dark, GlyphSet::Unicode),
        on_opus(),
        |a| async move {
            keys(&a, "/endpoint zz");
            a.settle().await;
            snapshot(
                "suggest-args-none-dark-truecolor-unicode-80x24",
                a.screen.styled(),
            );
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
    steering_facts(
        Screen::new(24, 80),
        styled(Mode::Dark, GlyphSet::Unicode),
        on_opus(),
        |a| async move {
            keys(&a, "/context ");
            a.settle().await;
            snapshot(
                "suggest-args-words-dark-truecolor-unicode-80x24",
                a.screen.styled(),
            );
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
    steering_facts(
        Screen::new(12, 30),
        styled(Mode::Dark, GlyphSet::Unicode),
        on_opus(),
        |a| async move {
            keys(&a, "/effort x");
            a.settle().await;
            snapshot(
                "suggest-args-dark-truecolor-unicode-30x12",
                a.screen.styled(),
            );
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn without_colour_the_highlighted_value_is_reverse_video() {
    let mut opts = options(Start::Stalled);
    opts.theme = Theme::new(Mode::Dark, Depth::None);
    steering_facts(Screen::new(24, 80), opts, on_opus(), |a| async move {
        keys(&a, "/effort x");
        a.settle().await;
        let shot = a.screen.styled();
        let style_of = |needle: &str| {
            let mut lines = shot.lines();
            lines.find(|l| l.starts_with(needle)).unwrap();
            lines.next().unwrap_or_default().to_owned()
        };
        assert!(reversed_across(&style_of("| \u{203A} xhigh"), 80), "{shot}");
        assert!(!style_of("|   low").contains("inverse"), "{shot}");
        assert!(!style_of("|   max").contains("inverse"), "{shot}");
        a.ctrl('c');
        keys(&a, "/effort m");
        a.settle().await;
        let shot = a.screen.styled();
        let menu: Vec<&str> = shot
            .lines()
            .skip_while(|l| !l.contains("Effort opus-5-5"))
            .take_while(|l| !l.starts_with("|\u{256D}"))
            .collect();
        assert!(menu.iter().any(|l| l.starts_with("|   max")), "{shot}");
        assert!(
            !menu.concat().contains("inverse"),
            "nothing is highlighted: {shot}"
        );
        a.ctrl('c').ctrl('c').ctrl('c');
    })
    .await;
}
