use clap::{CommandFactory, Parser};
use wisp_protocol::{ErrorKind, TurnOutcome, TurnStats};

use super::*;
use crate::Args;
use hostile::{corpus, hostile_detail, hostile_events};

mod hostile;

#[test]
fn verbose_is_the_only_jsonl_warning() {
    let mut p = Presentation {
        presenter: Presenter::Jsonl,
        verbose: false,
        tui_verbose: true,
        display: wisp_tui::term::SavedDisplay::default(),
    };
    assert!(jsonl_warnings(&p).is_empty());
    p.verbose = true;
    assert_eq!(
        jsonl_warnings(&p),
        vec!["wisp: --verbose has no effect with --output jsonl".to_owned()]
    );
}

#[tokio::test]
async fn a_line_that_is_not_utf8_is_an_error() {
    let input = std::io::Cursor::new(b"hello\n\xff\xfe\nlater\n".to_vec());
    let mut lines = read_lines(input);
    assert_eq!(lines.recv().await, Some(Ok("hello".to_owned())));
    assert!(matches!(lines.recv().await, Some(Err(e)) if e.contains("stdin")));
    assert_eq!(lines.recv().await, None);
}

#[test]
fn output_flags_parse_and_jsonl_is_marked_unstable() {
    let args = Args::try_parse_from(["wisp"]).unwrap();
    assert_eq!(args.output, OutputMode::Auto);
    assert!(!args.verbose);
    let args = Args::try_parse_from(["wisp", "--output", "jsonl", "--verbose"]).unwrap();
    assert_eq!(args.output, OutputMode::Jsonl);
    assert!(args.verbose);
    assert!(Args::try_parse_from(["wisp", "--output", "fancy"]).is_err());
    let help = Args::command().term_width(0).render_long_help().to_string();
    assert!(help.contains("Unstable until M6"), "{help}");
    assert!(help.contains("[default: auto]"), "{help}");
    for mode in ["auto", "tui", "jsonl"] {
        assert_eq!(
            Args::try_parse_from(["wisp", "--output", mode])
                .unwrap()
                .output
                .requested(),
            match mode {
                "auto" => Requested::Auto,
                "tui" => Requested::Tui,
                _ => Requested::Jsonl,
            }
        );
    }
    assert!(help.contains("--verbose"), "{help}");
    assert!(!help.contains("plain"), "{help}");
    assert!(!help.contains("screen"), "{help}");
}

#[test]
fn plain_output_and_screen_reader_mode_are_gone() {
    for args in [
        &["wisp", "--output", "plain"][..],
        &["wisp", "--screen-reader"][..],
    ] {
        let err = Args::try_parse_from(args).err().unwrap();
        assert_eq!(err.exit_code(), 2, "{args:?}");
    }
}

#[test]
fn a_hostile_error_and_its_detail_show_inert_in_the_transcript() {
    use wisp_render::markdown::{Layout, Renderer};
    use wisp_render::theme::glyphs::GlyphSet;
    use wisp_render::transcript::{Config, Detail, Transcript};
    let renderer = Renderer::new(Layout::new(200, GlyphSet::Unicode));
    for h in corpus() {
        let mut transcript = Transcript::new(Config {
            provider: "Anthropic".into(),
            cache_ttl: wisp_protocol::CacheTtl::OneHour,
            logging: true,
        });
        let event = AgentEvent::TurnEnded {
            outcome: TurnOutcome::Error {
                kind: ErrorKind::InvalidRequest,
                message: h.clone(),
                usage: None,
                cost: None,
                committed_steps: 2,
                detail: Some(hostile_detail(&h)),
                attempts: 0,
                gave_up: None,
                offer: None,
            },
            stats: TurnStats::default(),
        };
        transcript.apply(&AgentEvent::turn_started(1), std::time::Duration::ZERO);
        transcript.apply(&event, std::time::Duration::ZERO);
        for detail in [Detail::Scrollback, Detail::Full] {
            let shown: String = transcript
                .rows(&renderer, detail)
                .iter()
                .flat_map(|row| row.spans.iter().map(|s| s.text.clone()))
                .collect();
            assert!(
                !shown
                    .chars()
                    .any(|c| c.is_control() || matches!(c, '\u{202A}'..='\u{202E}')),
                "{shown:?}"
            );
        }
        let full: String = transcript
            .rows(&renderer, Detail::Full)
            .iter()
            .flat_map(|row| row.spans.iter().map(|s| s.text.clone()))
            .collect();
        assert!(full.contains("502"), "{full:?}");
        let line = jsonl_line(&event).unwrap();
        let body = line.strip_suffix('\n').unwrap();
        assert!(!body.chars().any(char::is_control), "{line:?}");
    }
}

#[test]
fn jsonl_lines_carry_no_raw_controls_and_round_trip() {
    for h in corpus() {
        for event in hostile_events(&h) {
            let line = jsonl_line(&event).unwrap();
            let body = line.strip_suffix('\n').unwrap();
            assert!(!body.contains('\n'), "{line:?}");
            let raw: Vec<char> = body
                .chars()
                .filter(|c| c.is_control() || wisp_protocol::jsonl::hidden_format(*c))
                .collect();
            assert_eq!(raw, Vec::<char>::new(), "{line:?}");
            let back: AgentEvent = serde_json::from_str(body).unwrap();
            assert_eq!(back, event);
        }
    }
}

#[test]
fn jsonl_matches_the_session_log_escaping() {
    let event = AgentEvent::TextDelta {
        index: 0,
        text: "a\u{7f}b\u{9b}c\u{1b}d".into(),
    };
    let line = jsonl_line(&event).unwrap();
    assert_eq!(
        line,
        "{\"type\":\"text_delta\",\"index\":0,\"text\":\"a\\u007fb\\u009bc\\u001bd\"}\n"
    );
}

#[test]
fn help_fits_80_columns() {
    let built = crate::tools::command::<Args>;
    for help in [
        built().term_width(80).render_help().to_string(),
        built().term_width(80).render_long_help().to_string(),
    ] {
        for line in help.lines() {
            assert!(line.chars().count() <= 80, "{line:?}");
        }
    }
}

#[test]
fn account_telemetry_is_excluded_from_root_and_child_jsonl_output() {
    let event = AgentEvent::PlanUsageChanged {
        request: Some(wisp_protocol::RequestId(2)),
        endpoint: "alias".into(),
        state: wisp_protocol::plan_usage::UsageState {
            snapshot: Some({
                let mut snapshot = wisp_protocol::plan_usage::PlanUsage::new(None, vec![]).unwrap();
                snapshot.plan_tier = Some(wisp_protocol::plan_usage::PlanTier::Pro);
                snapshot
            }),
            loading: false,
            stale: true,
            error: Some(wisp_protocol::plan_usage::PlanUsageError::Unsupported),
        },
    };
    assert_eq!(jsonl_line(&event).unwrap(), "");
    let child = AgentEvent::Agent {
        agent: wisp_protocol::AgentPath::root(),
        event: Box::new(event),
    };
    assert_eq!(jsonl_line(&child).unwrap(), "");
}

#[test]
fn private_auth_challenges_are_excluded_from_jsonl_even_inside_child_events() {
    use wisp_protocol::subscription_auth::{
        AuthEvent, AuthOutcome, ChallengeKind, PrivateChallenge, SubscriptionProvider,
    };
    let event = AgentEvent::SubscriptionAuth {
        event: AuthEvent {
            request: wisp_protocol::RequestId(1),
            outcome: AuthOutcome::Challenge {
                challenge: PrivateChallenge {
                    login: wisp_protocol::RequestId(1),
                    provider: SubscriptionProvider::ChatGpt,
                    alias: "plan".to_owned(),
                    kind: ChallengeKind::BrowserLoopback,
                    authorization_url: wisp_protocol::Secret::new(
                        "https://example.invalid/?state=STATE_CANARY&code=CODE_CANARY&token=TOKEN_CANARY&account=ACCOUNT_CANARY",
                    ),
                    expires_in_secs: 600,
                    opened: false,
                },
            },
        },
    };
    assert!(super::jsonl_line(&event).unwrap().is_empty());
    let nested = AgentEvent::Agent {
        agent: wisp_protocol::AgentPath::root(),
        event: Box::new(event),
    };
    assert!(super::jsonl_line(&nested).unwrap().is_empty());
}

#[test]
fn highest_jsonl_ignores_other_clients_ids() {
    let highest_jsonl = |ids: &[&str]| InputId::highest_numbered(super::JSONL_PREFIX, ids.to_vec());
    let ids = ["tui-9", "jsonl-2", "a", "jsonl-x", "jsonl-007", "jsonl-"];
    assert_eq!(highest_jsonl(&ids), 7);
    assert_eq!(highest_jsonl(&[]), 0);
    assert_eq!(highest_jsonl(&["tui-4"]), 0);
}

#[test]
fn jsonl_hides_server_sign_in_links_but_keeps_its_notes() {
    let event: AgentEvent = serde_json::from_value(serde_json::json!({"type":"plugins_changed","notes":[{"plugin":"mcp","message":"child added server local"}],"links":[{"plugin":"mcp","message":"sign in","link":"https://example.invalid/authorize?CANARY-placeholder"}]})).unwrap();
    let nested = AgentEvent::Agent {
        agent: wisp_protocol::AgentPath::new("/child"),
        event: Box::new(event.clone()),
    };
    for event in [event, nested] {
        let line = super::jsonl_line(&event).unwrap();
        assert!(!line.contains("CANARY-placeholder"));
        assert!(line.contains("child added server local"));
        assert!(!line.contains("\"links\""));
    }
    let only: AgentEvent = serde_json::from_value(serde_json::json!({"type":"plugins_changed","links":[{"plugin":"mcp","message":"sign in","link":"https://example.invalid/authorize?CANARY-placeholder"}]})).unwrap();
    for event in [
        only.clone(),
        AgentEvent::Agent {
            agent: wisp_protocol::AgentPath::new("/child"),
            event: Box::new(only),
        },
    ] {
        assert!(super::jsonl_line(&event).unwrap().is_empty());
    }
}
