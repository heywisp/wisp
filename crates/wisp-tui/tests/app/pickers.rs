use std::collections::VecDeque;
use std::future::Future;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::sleep;
use wisp_protocol::ErrorKind;
use wisp_protocol::log::FeatureState;
use wisp_protocol::settings::{CACHE_KEPT, CACHE_KEPT_EFFORT};
use wisp_protocol::{
    AgentEvent, BlockKind, CacheEffect, CitedSource, Effort, Feature, RebuildReason, SearchAction,
    SearchEnd, SearchHit, SearchOutcome, TurnOutcome, TurnStats, Usage,
};
use wisp_render::chrome::Endpoint;
use wisp_render::chrome::account::AccountKind;
use wisp_tui::app::Credential;
use wisp_tui::app::replay::{self, Script, Settings};
use wisp_tui::app::{Finished, Session, SessionFacts, run_to_end};
use wisp_tui::term::{Input, Key, KeyCode};

use super::{NEW_KEY, Run, Screen, ask_all, ctrl, facts, key, numbered, options, session};
use crate::Drive;
use crate::events::{Changed, End, Ending, settings_changed, turn_ended};
use crate::select::status_row;
use crate::shared::endpoint;
use wisp_tui::app::highlight::Start;

fn chosen(model: &str, effort: &str, endpoint: Endpoint) -> SessionFacts {
    SessionFacts {
        model: model.to_owned(),
        effort: effort.to_owned(),
        endpoint,
        endpoints: vec![crate::session_account(endpoint)],
        ..facts()
    }
}

fn usage() -> Usage {
    Usage {
        input_tokens: 100,
        output_tokens: 50,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 10_000,
        cache_creation_5m_input_tokens: Some(0),
        cache_creation_1h_input_tokens: Some(10_000),
        web_search_requests: 0,
        ..Usage::default()
    }
}

fn reply(text: &str, extra: Vec<AgentEvent>) -> Vec<AgentEvent> {
    let mut events = vec![
        AgentEvent::turn_started(1),
        AgentEvent::StepStarted { step: 1 },
        AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Text,
        },
        AgentEvent::TextDelta {
            index: 0,
            text: text.to_owned(),
        },
    ];
    events.extend(extra);
    events.extend([
        AgentEvent::BlockStopped { index: 0 },
        AgentEvent::StepEnded {
            step: 1,
            usage: Some(usage()),
            cost_usd: Some(0.05),
            lower_bound: false,
        },
        turn_ended(End {
            model: "claude-opus-5-5".to_owned(),
            usage: Some(usage()),
            ..End::default()
        }),
    ]);
    events
}

async fn picking<F, D>(
    screen: Screen,
    facts: SessionFacts,
    turns: Vec<Vec<AgentEvent>>,
    effects: Vec<CacheEffect>,
    drive: D,
) -> Finished
where
    D: FnOnce(Run) -> F,
    F: Future<Output = ()>,
{
    picking_bound(screen, facts, turns, effects, vec![], drive).await
}

async fn picking_bound<F, D>(
    screen: Screen,
    facts: SessionFacts,
    turns: Vec<Vec<AgentEvent>>,
    effects: Vec<CacheEffect>,
    account_bound: Vec<String>,
    drive: D,
) -> Finished
where
    D: FnOnce(Run) -> F,
    F: Future<Output = ()>,
{
    let events: Vec<AgentEvent> = turns.into_iter().flatten().collect();
    let effort = facts.effort.parse().unwrap_or(Effort::Medium);
    let settings = Settings {
        endpoint: facts
            .endpoint_name
            .clone()
            .unwrap_or_else(|| "claude-api".to_owned()),
        model: facts.model.clone(),
        effort,
        effects: VecDeque::from(effects),
        account_bound,
        pending: None,
        described: replay::Described::default(),
        session: Default::default(),
        cache_ttl: facts.cache_ttl,
        efforts: Default::default(),
    };
    let script = Script::new(events).with_settings(settings);
    let (commands, events) = replay::spawn(script, replay::PACE);
    let (inputs, received) = mpsc::unbounded_channel();
    let mut console = screen.clone();
    let session = Session {
        leave_note: None,
        attached: None,
        alerts: Vec::new(),
        facts,
        events,
        commands,
        resumed: None,
    };
    let app = run_to_end(&mut console, received, session, options(Start::Stalled));
    let driver = drive(Run {
        screen: screen.clone(),
        inputs,
    });
    let (finished, ()) = tokio::join!(app, driver);
    finished
}

fn feature_state(model: &str, state: FeatureState) -> AgentEvent {
    AgentEvent::FeatureStateChanged {
        endpoint: "claude-api".into(),
        model: model.to_owned(),
        feature: Feature::PerMessageEffort,
        state,
    }
}

fn back_tab() -> Input {
    Input::Key(Key::plain(KeyCode::BackTab))
}

async fn settle() {
    sleep(Duration::from_millis(200)).await;
}

fn shows(screen: &Screen, text: &str) -> bool {
    screen.steady().iter().any(|l| l.contains(text))
}

fn notes(screen: &Screen) -> Vec<String> {
    screen
        .visible()
        .into_iter()
        .filter_map(|l| l.trim_start().strip_prefix("\u{2219} ").map(str::to_owned))
        .collect()
}

#[tokio::test(start_paused = true)]
async fn the_model_picker_docks_and_the_transcript_ends_at_its_rule() {
    let screen = Screen::new(24, 80);
    let facts = chosen("claude-opus-5-5", "medium", Endpoint::Official);
    let long: String = (0..30)
        .map(|i| format!("line {i} of the answer\n\n"))
        .collect();
    picking(
        screen.clone(),
        facts,
        vec![reply(&long, vec![])],
        vec![],
        |run| async move {
            settle().await;
            run.say("question");
            sleep(Duration::from_secs(3)).await;
            let before = run.screen.steady();
            run.say("/model");
            settle().await;
            let open = run.screen.steady();
            let rule = open.iter().position(|l| l.trim() == "Model").unwrap() - 1;
            let end = super::frame_top(&before);
            assert!(rule > 0);
            assert_eq!(
                open[..rule],
                before[end - rule..end],
                "the transcript does not end at the rule: {open:#?}"
            );
            let mut folded = before[end..].to_vec();
            folded.drain(3..5);
            assert_eq!(open[super::frame_top(&open)..], folded, "{open:#?}");
            run.send(key(KeyCode::Esc));
            settle().await;
            assert_eq!(run.screen.steady(), before);
            assert!(notes(&run.screen).is_empty());
            run.send(ctrl('d'));
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn choosing_a_model_notes_the_rebuild_and_applies_from_the_next_turn() {
    let screen = Screen::new(30, 100);
    let facts = chosen("claude-opus-5-5", "medium", Endpoint::Official);
    picking(
        screen.clone(),
        facts,
        vec![reply("first", vec![]), reply("second", vec![])],
        vec![],
        |run| async move {
            settle().await;
            run.say("one");
            sleep(Duration::from_secs(2)).await;
            run.say("/model");
            settle().await;
            assert!(shows(&run.screen, "sonnet-5-5                   ~$0.0404"));
            run.send(Input::Paste("sonnet-5".to_owned()));
            settle().await;
            run.send(key(KeyCode::Enter));
            settle().await;
            assert_eq!(
                notes(&run.screen),
                ["Switched to \u{25cf} claude-api \u{b7} API \u{b7} sonnet-5-5 medium \u{b7} cache rebuild ~$0.0404"]
            );
            let expected = format!("{}   sonnet-5-5 medium", AccountKind::ClaudeApi.label());
            let row = status_row(&run.screen);
            assert!(row.contains(&expected), "{row}");
            assert!(!row.contains("next turn"), "{row}");
            run.say("two");
            sleep(Duration::from_secs(2)).await;
            let row = status_row(&run.screen);
            assert!(row.contains(&expected), "{row}");
            assert!(!row.contains("next turn"), "{row}");
            assert!(shows(&run.screen, "sonnet-5-5 medium"));
            run.send(ctrl('d'));
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_model_that_lacks_the_effort_chains_the_effort_picker() {
    let screen = Screen::new(30, 100);
    let facts = chosen("claude-opus-5-5", "xhigh", Endpoint::Official);
    picking(
        screen.clone(),
        facts,
        vec![],
        vec![],
        |run| async move {
            settle().await;
            run.say("/model");
            run.send(Input::Paste("sonnet-4".to_owned()));
            run.send(key(KeyCode::Enter));
            settle().await;
            assert!(shows(&run.screen, "with the model switch"));
            run.send(key(KeyCode::Esc));
            settle().await;
            assert!(!shows(&run.screen, "with the model switch"));
            assert!(notes(&run.screen).is_empty());
            run.say("/model claude-sonnet-4-6");
            settle().await;
            assert!(shows(&run.screen, "with the model switch"));
            run.send(key(KeyCode::Enter));
            settle().await;
            assert_eq!(
                notes(&run.screen),
                ["Switched to \u{25cf} claude-api \u{b7} API \u{b7} sonnet-4-6 high \u{b7} nothing cached yet, so nothing to rebuild"]
            );
            run.send(ctrl('d'));
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn typed_commands_apply_directly_and_bad_levels_are_explained() {
    let screen = Screen::new(30, 100);
    let facts = chosen("claude-opus-5-5", "medium", Endpoint::Official);
    picking(screen.clone(), facts, vec![], vec![], |run| async move {
        settle().await;
        run.say("/effort low");
        settle().await;
        run.say("/model claude-haiku-5-5");
        settle().await;
        assert_eq!(
            notes(&run.screen),
            [
                format!("Effort \u{2192} low \u{b7} {CACHE_KEPT}").as_str(),
                "Switched to \u{25cf} claude-api \u{b7} API \u{b7} haiku-5-5 low \u{b7} nothing cached yet, so nothing to rebuild",
            ]
        );
        run.say("/effort extreme");
        settle().await;
        let row = status_row(&run.screen);
        assert!(row.contains("not `extreme`"), "{row}");
        assert_eq!(notes(&run.screen).len(), 2);
        run.send(ctrl('d'));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_key_typed_as_the_model_command_is_refused_and_never_shown() {
    let screen = Screen::new(30, 100);
    let facts = chosen("claude-opus-5-5", "medium", Endpoint::Official);
    picking(screen.clone(), facts, vec![], vec![], |run| async move {
        settle().await;
        run.say("/model sk-ant-api03-CANARYMODEL0000");
        settle().await;
        let row = status_row(&run.screen);
        assert!(
            row.contains(wisp_render::transcript::copy::KEY_SHAPED_MODEL),
            "{row}"
        );
        assert!(notes(&run.screen).is_empty());
        assert!(!row.contains("next turn"), "{row}");
        run.send(Input::Paste(
            "/model sk-ant-api03-CANARYMODEL0000".to_owned(),
        ));
        run.send(key(KeyCode::Enter));
        settle().await;
        assert!(notes(&run.screen).is_empty());
        run.send(key(KeyCode::Up));
        settle().await;
        assert!(!shows(&run.screen, "CANARY"));
        assert!(status_row(&run.screen).contains("opus-5-5"));
        run.send(ctrl('d'));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn shift_tab_cycles_and_wraps_where_the_cache_is_kept() {
    let screen = Screen::new(30, 100);
    let facts = chosen("claude-opus-5-5", "medium", Endpoint::Official);
    picking(screen.clone(), facts, vec![], vec![], |run| async move {
        settle().await;
        run.send(back_tab());
        settle().await;
        let row = status_row(&run.screen);
        assert!(
            row.contains(
                format!("effort medium \u{2192} high \u{b7} next turn \u{b7} {CACHE_KEPT}")
                    .as_str()
            ),
            "{row}"
        );
        for _ in 0..3 {
            run.send(back_tab());
        }
        settle().await;
        assert_eq!(
            notes(&run.screen),
            [
                format!("Effort \u{2192} high \u{b7} {CACHE_KEPT}").as_str(),
                format!("Effort \u{2192} xhigh \u{b7} {CACHE_KEPT}").as_str(),
                format!("Effort \u{2192} max \u{b7} {CACHE_KEPT}").as_str(),
                format!("Effort \u{2192} low \u{b7} {CACHE_KEPT}").as_str(),
            ]
        );
        sleep(Duration::from_secs(3)).await;
        assert!(
            !status_row(&run.screen).contains(format!("next turn \u{b7} {CACHE_KEPT}").as_str())
        );
        run.send(ctrl('d'));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn shift_tab_changes_nothing_on_a_model_without_per_message_effort() {
    let screen = Screen::new(30, 120);
    let facts = chosen("claude-opus-4-8", "medium", Endpoint::Official);
    picking(
        screen.clone(),
        facts,
        vec![],
        vec![],
        |run| async move {
            settle().await;
            run.send(back_tab());
            settle().await;
            let row = status_row(&run.screen);
            assert!(
                row.contains("Changing effort on opus-4-8 rebuilds the cache (nothing cached yet). Use /effort to choose."),
                "{row}"
            );
            assert!(notes(&run.screen).is_empty());
            run.send(ctrl('d'));
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn on_a_gateway_shift_tab_waits_until_a_change_is_proven() {
    let screen = Screen::new(30, 160);
    let facts = chosen("claude-opus-5-5", "medium", Endpoint::Custom);
    picking(
        screen.clone(),
        facts,
        vec![reply(
            "proved",
            vec![feature_state("claude-opus-5-5", FeatureState::Proven)],
        )],
        vec![CacheEffect::KeptIfPassedOn, CacheEffect::Kept],
        |run| async move {
            settle().await;
            run.send(back_tab());
            settle().await;
            let row = status_row(&run.screen);
            assert!(
                row.contains("This gateway hasn't shown it passes per-message effort on"),
                "{row}"
            );
            assert!(notes(&run.screen).is_empty());
            run.say("/effort");
            settle().await;
            assert!(shows(&run.screen, CACHE_KEPT_EFFORT));
            run.send(key(KeyCode::Down));
            run.send(key(KeyCode::Enter));
            settle().await;
            run.say("go");
            sleep(Duration::from_secs(2)).await;
            run.send(back_tab());
            settle().await;
            let row = status_row(&run.screen);
            assert!(
                row.contains(
                    format!("effort high \u{2192} xhigh \u{b7} next turn \u{b7} {CACHE_KEPT}")
                        .as_str()
                ),
                "{row}"
            );
            assert_eq!(
                notes(&run.screen),
                [
                    format!("Effort \u{2192} high \u{b7} {CACHE_KEPT_EFFORT}").as_str(),
                    format!("Effort \u{2192} xhigh \u{b7} {CACHE_KEPT}").as_str(),
                ]
            );
            run.send(ctrl('d'));
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_gateway_rejection_adds_a_correcting_note_and_stops_shift_tab() {
    let screen = Screen::new(30, 160);
    let facts = chosen("claude-sonnet-5-5", "medium", Endpoint::Custom);
    let correction = settings_changed(Changed {
        endpoint: "claude-api".into(),
        model: "claude-sonnet-5-5".to_owned(),
        cache: CacheEffect::Rebuilt {
            reason: RebuildReason::PerMessageEffortRejected,
        },
        ..Changed::default()
    });
    let miss = AgentEvent::CacheMiss {
        previous_prompt_tokens: 10_000,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 10_000,
        diagnostics: None,
        cause: None,
    };
    picking(
        screen.clone(),
        facts,
        vec![reply(
            "answered",
            vec![
                feature_state("claude-sonnet-5-5", FeatureState::FallenBack),
                correction,
                miss,
            ],
        )],
        vec![CacheEffect::KeptIfPassedOn],
        |run| async move {
            settle().await;
            run.say("/effort low");
            settle().await;
            run.say("go");
            sleep(Duration::from_secs(2)).await;
            assert_eq!(
                notes(&run.screen),
                [
                    format!("Effort \u{2192} low \u{b7} {CACHE_KEPT_EFFORT}").as_str(),
                    "Effort \u{2192} low \u{b7} cache rebuilt: this gateway doesn't pass per-message effort on for sonnet-5-5",
                ]
            );
            assert!(shows(&run.screen, "cache rebuilt: per-message effort rejected"));
            run.send(back_tab());
            settle().await;
            let row = status_row(&run.screen);
            assert!(
                row.contains("This gateway doesn't pass per-message effort on for sonnet-5-5"),
                "{row}"
            );
            run.send(ctrl('d'));
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_search_turn_shows_its_row_and_numbered_sources() {
    let screen = Screen::new(24, 100);
    let facts = chosen("claude-opus-5-5", "medium", Endpoint::Official);
    let text = |index: usize, words: &str| {
        vec![
            AgentEvent::BlockStarted {
                index,
                kind: BlockKind::Text,
            },
            AgentEvent::TextDelta {
                index,
                text: words.to_owned(),
            },
            AgentEvent::BlockStopped { index },
        ]
    };
    let other = |index: usize, name: &str| {
        vec![
            AgentEvent::BlockStarted {
                index,
                kind: BlockKind::Other {
                    name: name.to_owned(),
                },
            },
            AgentEvent::BlockStopped { index },
        ]
    };
    let mut events = vec![
        AgentEvent::turn_started(1),
        AgentEvent::StepStarted { step: 1 },
    ];
    events.extend(text(0, "Let me look that up."));
    events.extend(other(1, "server_tool_use"));
    events.push(AgentEvent::SearchStarted {
        step: 1,
        attempt: 0,
        call_id: "srvtoolu_1".into(),
        query: "rust ripgrep crates".into(),
        action: SearchAction::Search,
    });
    events.extend(other(2, "web_search_tool_result"));
    events.push(AgentEvent::SearchEnded {
        step: 1,
        attempt: 0,
        call_id: "srvtoolu_1".into(),
        outcome: SearchEnd::Done(SearchOutcome::Results {
            hits: vec![SearchHit {
                title: "ripgrep on crates.io".into(),
                url: "https://crates.io/crates/ripgrep".into(),
                snippet: None,
                page_age: Some("2 days ago".into()),
            }],
        }),
    });
    events.extend(text(3, "ripgrep is published as a crate"));
    events.push(AgentEvent::Citations {
        step: 1,
        attempt: 0,
        index: 3,
        sources: vec![CitedSource {
            url: "https://crates.io/crates/ripgrep".into(),
            title: Some("ripgrep on crates.io".into()),
        }],
    });
    events.extend(text(4, "."));
    events.extend([
        AgentEvent::StepEnded {
            step: 1,
            usage: Some(usage()),
            cost_usd: Some(0.05),
            lower_bound: false,
        },
        turn_ended(End {
            model: "claude-opus-5-5".to_owned(),
            usage: Some(usage()),
            stats: TurnStats {
                started_at: "2026-10-24T09:00:00.000Z".into(),
                first_byte_ms: Some(400),
                duration_ms: 2_000,
                retries: 0,
                previous_prompt_tokens: None,
            },
            ..End::default()
        }),
    ]);
    picking(
        screen.clone(),
        facts,
        vec![events],
        vec![],
        |run| async move {
            settle().await;
            run.say("find ripgrep");
            sleep(Duration::from_secs(3)).await;
            let shot: String = run
                .screen
                .visible()
                .iter()
                .map(|l| format!("{}\n", l.trim_end()))
                .collect();
            assert!(
                shot.contains("Searched \"rust ripgrep crates\" \u{b7} 1 result"),
                "{shot}"
            );
            assert!(
                shot.contains("ripgrep is published as a crate[1]."),
                "{shot}"
            );
            assert!(!shot.contains("server_tool_use"), "{shot}");
            let mut settings = insta::Settings::clone_current();
            settings.set_prepend_module_to_snapshot(false);
            settings.set_snapshot_path("../snapshots/chrome");
            settings.bind(|| insta::assert_snapshot!("web-search-turn", shot));
            run.send(ctrl('d'));
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_mid_turn_search_refusal_notes_the_refused_endpoint_and_the_turns_rebuild() {
    let screen = Screen::new(30, 160);
    let facts = chosen("claude-opus-5-5", "medium", Endpoint::Official);
    let refused = AgentEvent::FeatureStateChanged {
        endpoint: "claude-api".into(),
        model: "claude-opus-5-5".into(),
        feature: Feature::NativeWebSearch,
        state: FeatureState::FallenBack,
    };
    let correction = settings_changed(Changed {
        endpoint: "claude-api".into(),
        model: "claude-opus-5-5".to_owned(),
        effort: Effort::Medium,
        cache: CacheEffect::Rebuilt {
            reason: RebuildReason::NativeSearchRefused,
        },
        ..Changed::default()
    });
    let miss = AgentEvent::CacheMiss {
        previous_prompt_tokens: 10_000,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 10_000,
        diagnostics: None,
        cause: None,
    };
    picking(
        screen.clone(),
        facts,
        vec![
            reply("answered", vec![refused, correction, miss]),
            reply("again", vec![]),
        ],
        vec![],
        |run| async move {
            settle().await;
            run.say("go");
            sleep(Duration::from_secs(2)).await;
            let shot: String = run
                .screen
                .visible()
                .iter()
                .map(|l| format!("{}\n", l.trim_end()))
                .collect();
            let mut settings = insta::Settings::clone_current();
            settings.set_prepend_module_to_snapshot(false);
            settings.set_snapshot_path("../snapshots/chrome");
            settings.bind(|| insta::assert_snapshot!("native-search-refused-mid-turn", shot));
            let shown = notes(&run.screen);
            assert_eq!(shown.len(), 1, "{shown:?}");
            assert!(
                shown[0].starts_with(
                    "web search isn't enabled for this account on claude-api; continuing without web search \u{b7} "
                ),
                "{shown:?}"
            );
            assert!(shows(
                &run.screen,
                "cache rebuilt: web search isn't enabled for this account"
            ));
            run.say("more");
            sleep(Duration::from_secs(2)).await;
            let footers = run
                .screen
                .visible()
                .into_iter()
                .filter(|l| l.contains("cache rebuilt: web search isn't enabled"))
                .count();
            assert_eq!(footers, 1);
            run.send(ctrl('d'));
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_change_during_a_turn_shows_as_pending_and_its_note_follows_the_turn() {
    let screen = Screen::new(30, 100);
    let facts = chosen("claude-opus-5-5", "medium", Endpoint::Official);
    let words: String = (0..300).map(|i| format!("w{i} ")).collect();
    let slow: Vec<AgentEvent> = (0..40)
        .map(|_| AgentEvent::TextDelta {
            index: 0,
            text: words.clone(),
        })
        .collect();
    picking(
        screen.clone(),
        facts,
        vec![reply("start ", slow)],
        vec![],
        |run| async move {
            settle().await;
            run.say("go");
            sleep(Duration::from_millis(100)).await;
            run.say("/model claude-sonnet-5-5");
            sleep(Duration::from_millis(100)).await;
            let row = status_row(&run.screen);
            assert!(row.contains("\u{2192} sonnet-5-5 next turn"), "{row}");
            assert!(notes(&run.screen).is_empty());
            sleep(Duration::from_secs(3)).await;
            let lines = run.screen.visible();
            let footer = lines.iter().position(|l| l.contains("cached ")).unwrap();
            let note = lines
                .iter()
                .position(|l| {
                    l.contains(
                        "Switching to \u{25cf} claude-api \u{b7} API \u{b7} sonnet-5-5 medium \u{b7} nothing cached yet, so nothing to rebuild",
                    )
                })
                .unwrap_or_else(|| panic!("{lines:#?}"));
            assert!(note > footer, "{lines:#?}");
            assert!(lines[note + 1].contains("From your next message."), "{lines:#?}");
            run.send(ctrl('d'));
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn hostile_model_ids_stay_inert_in_the_picker_notes_and_status() {
    let screen = Screen::new(30, 120);
    let facts = chosen("claude-opus-5-5", "medium", Endpoint::Official);
    let hostile = "\u{1b}]0;pwned\u{7}\u{1b}]52;c;Zm9v\u{7}\u{1b}[2J\u{1b}[31mx\u{202e}y\u{9b}31m";
    picking(screen.clone(), facts, vec![], vec![], |run| async move {
        settle().await;
        run.say("/model");
        run.send(Input::Paste(hostile.to_owned()));
        settle().await;
        assert!(shows(&run.screen, wisp_protocol::NOT_SHOWN));
        assert!(!shows(&run.screen, "pwned"));
        run.send(key(KeyCode::Esc));
        run.send(Input::Paste(format!("/model {hostile}")));
        run.send(key(KeyCode::Enter));
        settle().await;
        let lines = run.screen.visible();
        assert!(notes(&run.screen).is_empty(), "{lines:#?}");
        assert!(shows(&run.screen, "matching \""), "{lines:#?}");
        assert!(lines.iter().all(|l| !l.contains('\u{1b}')), "{lines:#?}");
        assert!(!status_row(&run.screen).contains("next turn"));
        run.send(key(KeyCode::Esc));
        run.send(ctrl('d'));
    })
    .await;
    assert_eq!(screen.spied(), Vec::<String>::new());
    assert!(!screen.visible().iter().any(|l| l.contains("pwned\u{7}")));
}

#[tokio::test(start_paused = true)]
async fn help_names_the_pickers_and_shift_tab() {
    let screen = Screen::new(35, 120);
    picking(screen.clone(), facts(), vec![], vec![], |run| async move {
        settle().await;
        run.say("/help");
        settle().await;
        let lines = run.screen.visible();
        let row = |needle: &str| {
            lines
                .iter()
                .find(|l| l.contains(needle))
                .cloned()
                .unwrap_or_else(|| panic!("{needle}: {lines:#?}"))
        };
        assert!(row("/model [id]").contains("Switch the model"));
        assert!(row("/effort [level]").contains("Set how hard the model thinks"));
        assert!(row("/account [name]").contains("Switch the chat's account"));
        assert!(row("shift-tab").contains("Cycle effort where it is free"));
        run.send(key(KeyCode::Esc));
        run.send(ctrl('d'));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn an_open_picker_hides_the_new_output_hint_until_it_closes() {
    let screen = Screen::new(20, 80);
    session(
        screen.clone(),
        numbered(8),
        Start::Stalled,
        |run| async move {
            sleep(Duration::from_millis(100)).await;
            ask_all(&run, 6).await;
            run.say("question 6");
            run.send(key(KeyCode::PageUp));
            sleep(Duration::from_secs(3)).await;
            assert!(shows(&run.screen, NEW_KEY), "{:#?}", run.screen.visible());
            run.say("/model");
            settle().await;
            assert!(shows(&run.screen, "type to filter"));
            let rows = run.screen.visible();
            assert!(!rows.iter().any(|l| l.contains(NEW_KEY)), "{rows:#?}");
            run.send(key(KeyCode::Esc));
            settle().await;
            assert!(shows(&run.screen, NEW_KEY), "{:#?}", run.screen.visible());
            run.send(ctrl('d'));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_picker_never_opens_or_stays_open_without_room_to_draw_it() {
    let screen = Screen::new(3, 80);
    picking(screen.clone(), facts(), vec![], vec![], |run| async move {
        settle().await;
        run.say("/model");
        settle().await;
        assert!(shows(&run.screen, "the picker doesn't fit"));
        run.send(Input::Paste("abc".to_owned()));
        settle().await;
        assert!(shows(&run.screen, "abc"), "{:#?}", run.screen.visible());
        run.send(ctrl('u'));
        let size = run.screen.resize(24, 80);
        run.send(Input::Resize(size));
        settle().await;
        run.say("/model");
        settle().await;
        assert!(shows(&run.screen, "type to filter"));
        let size = run.screen.resize(3, 80);
        run.send(Input::Resize(size));
        settle().await;
        run.send(Input::Paste("xyz".to_owned()));
        settle().await;
        assert!(shows(&run.screen, "xyz"), "{:#?}", run.screen.visible());
        let size = run.screen.resize(24, 80);
        run.send(Input::Resize(size));
        settle().await;
        assert!(!shows(&run.screen, "type to filter"));
        run.send(ctrl('d'));
        run.send(ctrl('d'));
    })
    .await;
}

fn panel_top(screen: &Screen) -> Option<usize> {
    screen
        .visible()
        .into_iter()
        .find(|l| l.starts_with('\u{256D}'))
        .map(|l| l.chars().count())
}

#[tokio::test(start_paused = true)]
async fn the_welcome_panel_shows_an_effort_change_and_rewraps_on_resize() {
    let screen = Screen::new(30, 100);
    let facts = chosen("claude-opus-5-5", "medium", Endpoint::Official);
    picking(
        screen.clone(),
        facts,
        vec![reply("first", vec![])],
        vec![],
        |run| async move {
            settle().await;
            let before = "opus-5-5 medium";
            assert!(shows(&run.screen, before), "{:#?}", run.screen.visible());
            assert_eq!(panel_top(&run.screen), Some(99));
            run.say("/effort low");
            settle().await;
            let after = "opus-5-5 low";
            assert!(shows(&run.screen, after), "{:#?}", run.screen.visible());
            assert!(!shows(&run.screen, before));
            let size = run.screen.resize(30, 60);
            run.send(Input::Resize(size));
            sleep(Duration::from_secs(1)).await;
            assert_eq!(panel_top(&run.screen), Some(59));
            assert!(
                shows(&run.screen, "opus-5-5 low"),
                "{:#?}",
                run.screen.visible()
            );
            let size = run.screen.resize(30, 140);
            run.send(Input::Resize(size));
            sleep(Duration::from_secs(1)).await;
            assert_eq!(panel_top(&run.screen), Some(100));
            let size = run.screen.resize(30, 50);
            run.send(Input::Resize(size));
            sleep(Duration::from_secs(1)).await;
            assert_eq!(panel_top(&run.screen), Some(49));
            assert!(
                shows(&run.screen, "\u{288E}\u{2836}\u{2870} wisp "),
                "{:#?}",
                run.screen.visible()
            );
            run.send(ctrl('d'));
        },
    )
    .await;
}

fn on_endpoint(start: &str) -> SessionFacts {
    let endpoints = vec![
        endpoint("direct", Endpoint::Official, Credential::ApiKey, None, None),
        endpoint(
            "gateway",
            Endpoint::Custom,
            Credential::AuthToken,
            Some("gw2.example.com"),
            None,
        ),
        endpoint(
            "manifest",
            Endpoint::Custom,
            Credential::AuthToken,
            Some("gw.example.com"),
            None,
        ),
        endpoint(
            "work",
            Endpoint::Official,
            Credential::ApiKey,
            None,
            Some("WORK_KEY is not set"),
        ),
    ];
    let active = endpoints.iter().find(|e| e.name == start).unwrap().clone();
    SessionFacts {
        credential: active.credential,
        host: active.host,
        endpoint: active.reach.unwrap(),
        endpoint_name: Some(start.to_owned()),
        endpoints,
        ..chosen("claude-opus-5-5", "medium", Endpoint::Official)
    }
}

fn priced_reply(text: &str) -> Vec<AgentEvent> {
    let mut events = reply(text, vec![]);
    if let Some(AgentEvent::TurnEnded {
        outcome: TurnOutcome::Completed { cost, .. },
        ..
    }) = events.last_mut()
    {
        *cost = Some(crate::events::cost(0.42, 0.42, false));
    }
    events
}

#[tokio::test(start_paused = true)]
async fn the_endpoint_picker_shows_the_rebuild_before_enter_and_the_label_follows_the_switch() {
    let screen = Screen::new(30, 120);
    picking(
        screen.clone(),
        on_endpoint("manifest"),
        vec![reply("first", vec![]), priced_reply("second")],
        vec![],
        |run| async move {
            settle().await;
            run.say("one");
            sleep(Duration::from_secs(2)).await;
            let row = status_row(&run.screen);
            assert!(row.contains("manifest \u{b7} API"), "{row}");
            run.say("/account");
            settle().await;
            let open = run.screen.visible();
            let row_of = |lines: &[String], name: &str| {
                lines
                    .iter()
                    .find(|l| {
                        l.starts_with(' ')
                            && !l.contains("Ready")
                            && l.contains(name)
                            && l.contains("opus-5-5")
                    })
                    .cloned()
                    .unwrap_or_else(|| panic!("{lines:#?}"))
            };
            let account = AccountKind::ClaudeApi.label();
            let direct = format!("direct      {account}");
            let work = format!("{account} \u{b7} WORK_KEY is not set");
            assert!(row_of(&open, "direct").contains(&direct));
            assert!(row_of(&open, "manifest").contains("\u{25cf} manifest \u{2713}  Gateway \u{b7} API"));
            assert!(row_of(&open, "work").contains(&work));
            assert!(shows(&run.screen, "This chat uses it now"));
            run.send(key(KeyCode::Up));
            run.send(key(KeyCode::Up));
            settle().await;
            assert!(shows(
                &run.screen,
                "Moves this chat to \u{25cf} direct \u{b7} cache rebuild ~$0.0808"
            ));
            run.send(key(KeyCode::Enter));
            settle().await;
            assert!(!shows(&run.screen, "type to filter"));
            assert_eq!(
                notes(&run.screen),
                ["Switched to \u{25cf} direct \u{b7} API \u{b7} opus-5-5 medium \u{b7} cache rebuild ~$0.0808"]
            );
            let row = status_row(&run.screen);
            assert!(row.contains("direct \u{b7} API   opus-5-5 medium"), "{row}");
            assert!(!row.contains("next message"), "{row}");
            run.say("two");
            sleep(Duration::from_secs(3)).await;
            let row = status_row(&run.screen);
            assert!(row.contains("direct \u{b7} API   opus-5-5 medium"), "{row}");
            assert!(!row.contains("next message"), "{row}");
            assert!(shows(&run.screen, "cache rebuilt: account switched"));
            run.send(ctrl('d'));
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_endpoint_picker_filters_and_closes_without_a_change() {
    let screen = Screen::new(30, 120);
    picking(
        screen.clone(),
        on_endpoint("manifest"),
        vec![],
        vec![],
        |run| async move {
            settle().await;
            run.say("/endpoint");
            settle().await;
            run.send(key(KeyCode::Esc));
            settle().await;
            assert!(!shows(&run.screen, "type to filter"));
            run.say("/endpoint");
            settle().await;
            run.send(ctrl('c'));
            settle().await;
            assert!(!shows(&run.screen, "type to filter"));
            assert!(notes(&run.screen).is_empty());
            run.say("/endpoint");
            run.send(Input::Paste("GATEWAY".to_owned()));
            settle().await;
            assert!(shows(&run.screen, "matching \"gateway\""));
            assert!(!shows(&run.screen, "direct"));
            run.send(key(KeyCode::Enter));
            settle().await;
            assert_eq!(
                notes(&run.screen),
                ["Switched to \u{25cf} gateway \u{b7} API \u{b7} opus-5-5 medium \u{b7} nothing cached yet, so nothing to rebuild"]
            );
            let row = status_row(&run.screen);
            assert!(row.contains("gateway \u{b7} API   opus-5-5 medium"), "{row}");
            assert!(!row.contains("next message"), "{row}");
            run.send(ctrl('d'));
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn typed_endpoint_commands_apply_or_say_why_not() {
    let screen = Screen::new(30, 200);
    picking_bound(
        screen.clone(),
        on_endpoint("manifest"),
        vec![],
        vec![],
        vec!["claude-sonnet-5-5".to_owned(), "claude-haiku-5-5".to_owned()],
        |run| async move {
            settle().await;
            run.say("/endpoint manifest");
            settle().await;
            let row = status_row(&run.screen);
            assert!(row.contains("This chat uses it now"), "{row}");
            run.say("/endpoint nope");
            settle().await;
            let row = status_row(&run.screen);
            assert!(
                row.contains("No account \"nope\". Available: direct, gateway, manifest"),
                "{row}"
            );
            run.say("/endpoint work");
            settle().await;
            let row = status_row(&run.screen);
            assert!(
                row.contains("Account \"work\" is unavailable: WORK_KEY is not set"),
                "{row}"
            );
            run.send(Input::Paste("/endpoint Bad\u{1b}[2J".to_owned()));
            run.send(key(KeyCode::Enter));
            settle().await;
            let row = status_row(&run.screen);
            assert!(
                row.contains("No account by that name. Available: direct, gateway, manifest"),
                "{row}"
            );
            assert!(!row.contains("Bad"), "{row}");
            assert!(notes(&run.screen).is_empty());
            run.say("/endpoint direct");
            settle().await;
            let bound = "earlier Sonnet 5.5 and Haiku 5.5 reasoning may not carry over";
            let to_direct = |model: &str| {
                format!("Switched to \u{25cf} direct \u{b7} API \u{b7} {model} medium \u{b7} nothing cached yet, so nothing to rebuild")
            };
            assert_eq!(notes(&run.screen), [to_direct("opus-5-5"), bound.to_owned()]);
            let row = status_row(&run.screen);
            assert!(row.contains("direct \u{b7} API   opus-5-5 medium"), "{row}");
            run.say("/model claude-sonnet-5-5");
            settle().await;
            assert_eq!(
                notes(&run.screen)[2..],
                [to_direct("sonnet-5-5"), bound.to_owned()]
            );
            run.say("/endpoint manifest");
            settle().await;
            assert_eq!(
                notes(&run.screen)[4..],
                ["Switched to \u{25cf} manifest \u{b7} API \u{b7} sonnet-5-5 medium \u{b7} nothing cached yet, so nothing to rebuild"]
            );
            let row = status_row(&run.screen);
            assert!(row.contains("manifest \u{b7} API   sonnet-5-5 medium"), "{row}");
            assert!(!row.contains("\u{2192}"), "{row}");
            run.send(ctrl('d'));
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn with_one_account_the_picker_offers_to_sign_in_to_another() {
    let screen = Screen::new(30, 120);
    let mut lone = on_endpoint("manifest");
    lone.endpoints
        .retain(|e| e.name == "manifest" || e.name == "work");
    picking(screen.clone(), lone, vec![], vec![], |run| async move {
        settle().await;
        run.say("/account");
        settle().await;
        assert!(shows(
            &run.screen,
            "\u{25cf} manifest \u{2713}  Gateway \u{b7} API"
        ));
        assert!(shows(&run.screen, "+ Sign in to another account\u{2026}"));
        run.send(ctrl('d'));
    })
    .await;
    let screen = Screen::new(30, 120);
    picking(screen.clone(), facts(), vec![], vec![], |run| async move {
        settle().await;
        run.say("/account");
        settle().await;
        assert!(shows(&run.screen, "+ Sign in to another account\u{2026}"));
        run.send(key(KeyCode::Esc));
        run.say("/account other");
        settle().await;
        assert!(
            status_row(&run.screen).contains("No account \"other\". Available: claude-api"),
            "{}",
            status_row(&run.screen)
        );
        run.send(ctrl('d'));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_thinking_binding_error_shows_the_kernels_advice() {
    let screen = Screen::new(30, 120);
    let message = "manifest rejected earlier thinking blocks as bound to a different conversation.\nThis conversation has moved between accounts. Switch back with /account direct, or start a new session to keep using manifest.\nInvalid `signature` in `thinking` block.";
    let failed = vec![
        AgentEvent::turn_started(1),
        turn_ended(End {
            ending: Ending::Error(ErrorKind::ThinkingBinding),
            message: message.to_owned(),
            ..End::default()
        }),
    ];
    picking(
        screen.clone(),
        on_endpoint("manifest"),
        vec![failed],
        vec![],
        |run| async move {
            settle().await;
            run.say("one");
            sleep(Duration::from_secs(2)).await;
            let lines = run.screen.visible();
            let first = lines
                .iter()
                .position(|l| l.contains("Anthropic stopped this turn."))
                .unwrap_or_else(|| panic!("{lines:#?}"));
            assert!(
                lines[first + 1].contains("manifest rejected earlier thinking blocks"),
                "{lines:#?}"
            );
            assert!(shows(
                &run.screen,
                "This conversation has moved between accounts."
            ));
            assert!(shows(&run.screen, "Switch back with /account direct"));
            run.send(ctrl('d'));
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn hostile_hosts_and_reasons_stay_inert_in_the_endpoint_picker() {
    let screen = Screen::new(30, 140);
    let hostile = "\u{1b}]0;pwned\u{7}\u{1b}]52;c;Zm9v\u{7}\u{1b}[2J\u{202e}x\u{9b}31m";
    let mut facts = on_endpoint("manifest");
    facts.endpoints[1].host = Some(format!("gw{hostile}.example.com"));
    facts.endpoints[3].unavailable = Some(format!("KEY{hostile} is not set"));
    picking(screen.clone(), facts, vec![], vec![], |run| async move {
        settle().await;
        run.say("/endpoint");
        settle().await;
        let lines = run.screen.visible();
        assert!(
            shows(
                &run.screen,
                &format!("{} \u{b7} KEY", AccountKind::ClaudeApi.label())
            ),
            "{lines:#?}"
        );
        assert!(lines.iter().all(|l| !l.contains('\u{1b}')), "{lines:#?}");
        run.send(key(KeyCode::Esc));
        run.say("/endpoint work");
        settle().await;
        assert!(!status_row(&run.screen).contains('\u{1b}'));
        run.say("/endpoint env");
        run.say("go");
        settle().await;
        let lines = run.screen.visible();
        assert!(lines.iter().all(|l| !l.contains('\u{1b}')), "{lines:#?}");
        run.send(ctrl('d'));
    })
    .await;
    assert_eq!(screen.spied(), Vec::<String>::new());
}

#[tokio::test(start_paused = true)]
async fn context_shows_a_cell_idle_and_while_busy_and_explains_bad_words() {
    let screen = Screen::new(40, 100);
    let facts = chosen("claude-opus-5-5", "medium", Endpoint::Official);
    picking(
        screen.clone(),
        facts,
        vec![reply("answered", vec![])],
        vec![],
        |run| async move {
            settle().await;
            run.say("/context");
            settle().await;
            assert!(shows(&run.screen, "Window unknown for claude-opus-5-5"));
            assert!(shows(&run.screen, "Usage by category"));
            run.say("/context everything");
            settle().await;
            assert!(status_row(&run.screen).contains("/context takes all"));
            run.say("/context all");
            settle().await;
            assert!(shows(&run.screen, "~ replay"));
            run.say("/context count");
            settle().await;
            assert!(
                status_row(&run.screen)
                    .contains("/context counts automatically now (context.count_tokens)")
            );
            run.send(ctrl('d'));
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_visible_picker_or_agents_page_keeps_keys_out_of_the_hidden_process_list() {
    for pane in ["/model", "/account", "opener", "/agents", "usage chip"] {
        let mut opts = options(Start::Stalled);
        opts.open_processes = true;
        let mut facts = facts();
        facts.endpoint_name = Some("work".to_owned());
        facts.endpoints = vec![wisp_render::chrome::EndpointFacts {
            name: "work".to_owned(),
            ..super::session_account(Endpoint::Official)
        }];
        super::steering::steering_facts(Screen::new(30, 100), opts, facts, |mut a| async move {
            a.start("check it", 1).await;
            if pane == "opener" {
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
                a.settle().await;
                super::sidebar::drain(&mut a).await;
                super::select::account_chip(&a, "work");
                a.settle().await;
                a.nothing_sent().await;
                assert!(!shows(&a.screen, "Account"));
                a.ctrl('o');
            } else if pane == "usage chip" {
                a.settle().await;
                let (column, row) = super::select::account_chip_spot(&a.screen, "work");
                for help in [true, false] {
                    if help {
                        a.paste("/help").key(KeyCode::Enter);
                    } else {
                        a.ctrl('o');
                    }
                    a.settle().await;
                    super::sidebar::mouse(&a, wisp_tui::term::MouseKind::Press, column, row);
                    super::sidebar::mouse(&a, wisp_tui::term::MouseKind::Release, column, row);
                    a.nothing_sent().await;
                    a.key(KeyCode::Esc);
                    a.settle().await;
                }

                a.paste("/usage").key(KeyCode::Enter);
                a.settle().await;
                super::sidebar::drain(&mut a).await;
                super::select::account_chip(&a, "work");
                a.describe().await;
            } else {
                a.paste(pane).key(KeyCode::Enter);
                if pane != "/agents" {
                    a.describe().await;
                }
            }
            let before = a.settle().await;
            assert!(
                !before.iter().any(|line| line.trim() == "Processes"),
                "{pane}: {before:?}"
            );
            a.emit(super::processes::waiting_terminal(5)).await;
            let covered = a.settle().await;
            assert!(
                !covered.iter().any(|line| line.trim() == "Processes"),
                "{pane}: {covered:?}"
            );
            a.key(KeyCode::Char('x')).key(KeyCode::Char('x'));
            let sent = super::sidebar::drain(&mut a).await;
            assert!(sent.is_empty(), "{pane}: {sent:?}");
            a.paste("x");
            let filtered = a.settle().await;
            if pane != "/agents" {
                assert!(
                    filtered
                        .iter()
                        .any(|line| line.contains("matching \"xxx\"")),
                    "{pane}: {filtered:?}"
                );
            }
            a.key(KeyCode::Esc);
            let lines = a.settle().await;
            assert!(
                lines.iter().any(|line| line.trim() == "Processes"),
                "{pane}: {lines:?}"
            );
            a.key(KeyCode::Char('x')).key(KeyCode::Char('x'));
            assert_eq!(
                a.next().await,
                wisp_protocol::ClientCommand::StopProcess { id: 5 }
            );
            super::processes::finish(&a).await;
        })
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_typed_account_asks_for_its_remembered_pair_before_switching() {
    use wisp_protocol::ClientCommand;
    use wisp_protocol::settings::{Remembered, SwitchQuote, SwitchTarget};
    for focus_child in [false, true] {
        super::steering::steering_facts(
            Screen::new(30, 100),
            options(Start::Stalled),
            on_endpoint("manifest"),
            |mut a| async move {
                a.paste("/account direct").key(KeyCode::Enter);
                let ClientCommand::DescribeSettings { request } = a.next().await else {
                    panic!("expected settings before switching")
                };
                a.nothing_sent().await;
                let mut snapshot = super::settings_screen::snapshot_of(request);
                snapshot.switches = vec![SwitchQuote {
                    target: SwitchTarget::Endpoint("direct".into()),
                    remembered: Some(Remembered {
                        model: "claude-sonnet-5-5".into(),
                        effort: Effort::High,
                    }),
                    consequence: None,
                    retry_compaction: None,
                }];
                if focus_child {
                    super::agent_keys::chat_on_work(&mut a).await;
                }
                a.emit(AgentEvent::SettingsSnapshot(snapshot)).await;
                if focus_child {
                    a.nothing_sent().await;
                } else {
                    let ClientCommand::ChangeSettings {
                        changes,
                        save: false,
                        ..
                    } = a.next().await
                    else {
                        panic!("expected the account pick")
                    };
                    assert_eq!(
                        changes,
                        ClientCommand::shortcut_changes(Some("direct".into()), None, None)
                    );
                }
                a.ctrl('c').ctrl('c');
            },
        )
        .await;
    }
}
