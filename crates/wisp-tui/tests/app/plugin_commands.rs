use std::time::Duration;

use tokio::time::sleep;
use wisp_protocol::plugin::{
    CommandArg, CommandBusy, CommandEntry, CommandOutcome, CommandRefusal, CommandWait,
};
use wisp_protocol::{AgentEvent, ClientCommand, DeliveryMode, InputId, PluginEntry, RequestId};
use wisp_render::theme::Mode;
use wisp_render::transcript::copy;
use wisp_tui::term::KeyCode;

use super::help::snapshot;
use super::sidebar::{NOTES, ROWS, TREE, base, drain, finish, options_in, quit, shot, slot};
use super::steering::{Agent, steering_on};
use super::{Screen, key};
use crate::events::{Changed, settings_changed};
use crate::shared::{catalog, change, plugin};
use crate::{Drive, shows};

const HOSTILE: &str = "x\u{202e}y\u{1b}[2J";

pub(super) fn command(
    name: &str,
    aliases: &[&str],
    values: &[&str],
    busy: Option<&str>,
) -> CommandEntry {
    CommandEntry {
        name: name.to_owned(),
        aliases: aliases.iter().map(|a| (*a).to_owned()).collect(),
        summary: format!("Run the {name} step"),
        args: if values.is_empty() {
            vec![]
        } else {
            vec![CommandArg {
                name: "speed".to_owned(),
                values: values.iter().map(|v| (*v).to_owned()).collect(),
            }]
        },
        busy: busy.map_or(CommandBusy::Runs, |hint| CommandBusy::BetweenTurns {
            hint: hint.to_owned(),
        }),
        short: true,
    }
}

pub(super) fn testkit(commands: Vec<CommandEntry>) -> PluginEntry {
    PluginEntry {
        commands,
        ..plugin("testkit", true, vec![])
    }
}

fn commands() -> Vec<CommandEntry> {
    vec![
        command("go", &["g"], &["fast", "slow"], None),
        command("note", &[], &[], None),
        command("fail", &[], &[], None),
        command("wait", &[], &[], Some("/wait runs between turns")),
    ]
}

fn hostile_commands() -> Vec<CommandEntry> {
    vec![CommandEntry {
        name: "evil".to_owned(),
        aliases: vec![HOSTILE.to_owned()],
        summary: format!("Sum {HOSTILE}"),
        args: vec![CommandArg {
            name: "mode".to_owned(),
            values: vec![HOSTILE.to_owned()],
        }],
        busy: CommandBusy::BetweenTurns {
            hint: format!("Busy {HOSTILE}"),
        },
        short: true,
    }]
}

async fn run(a: &mut Agent, line: &str) -> (String, String, RequestId) {
    drain(a).await;
    a.paste(line).key(KeyCode::Enter);
    sleep(Duration::from_millis(30)).await;
    let mut found = None;
    while let Ok(command) = a.commands.try_recv() {
        if let ClientCommand::RunPluginCommand {
            name,
            args,
            request,
        } = command
        {
            found = Some((name, args.into_inner(), request));
        }
    }
    found.unwrap_or_else(|| panic!("{line} sent no plugin command"))
}

async fn done(a: &Agent, request: RequestId, outcome: CommandOutcome) {
    answered(a, request, outcome, false).await;
}

async fn answered(a: &Agent, request: RequestId, outcome: CommandOutcome, safe: bool) {
    a.emit(AgentEvent::CommandDone {
        args_safe_to_echo: safe,
        answers: request,
        outcome,
    })
    .await;
}

async fn deferred(a: &Agent, request: RequestId, until: CommandWait) {
    a.emit(AgentEvent::CommandDeferred { request, until }).await;
}

fn composer(lines: &[String]) -> String {
    super::unframed(&lines[super::composer_top(lines) + 1])
}

#[tokio::test(start_paused = true)]
async fn a_plugin_command_is_sent_by_its_full_name_and_an_unknown_one_stays_typed() {
    steering_on(
        Screen::new(ROWS, 100),
        options_in(Mode::Dark),
        |mut a| async move {
            a.emit(catalog(vec![
                testkit(commands()),
                PluginEntry {
                    enabled: false,
                    ..testkit(vec![command("off", &[], &[], None)])
                },
            ]))
            .await;
            let (name, args, first) = run(&mut a, "/go  fast ").await;
            assert_eq!((name.as_str(), args.as_str()), ("testkit:go", "fast"));
            let (name, _, second) = run(&mut a, "/TESTKIT:G").await;
            assert_eq!(name, "testkit:g");
            done(&a, first, CommandOutcome::None).await;
            done(&a, second, CommandOutcome::None).await;
            a.emit(catalog(vec![PluginEntry {
                enabled: false,
                ..testkit(commands())
            }]))
            .await;
            unknown(&mut a, "/go").await;
            a.emit(catalog(vec![testkit(commands())])).await;
            a.emit(snapshot_of(vec![PluginEntry {
                enabled: false,
                ..testkit(commands())
            }]))
            .await;
            unknown(&mut a, "/go").await;
            a.emit(snapshot_of(vec![testkit(commands())])).await;
            let (name, _, third) = run(&mut a, "/go").await;
            assert_eq!(name, "testkit:go");
            done(&a, third, CommandOutcome::None).await;
            for typed in ["/nosuch:cmd", "/off"] {
                drain(&mut a).await;
                a.paste(typed).key(KeyCode::Enter);
                let lines = a.settle().await;
                assert_eq!(composer(&lines), format!(" \u{203a} {typed}"));
                let word = typed.trim_start_matches('/');
                assert!(shows(&lines, &copy::unknown_command(word)), "{lines:#?}");
                assert!(drain(&mut a).await.is_empty());
                a.ctrl('c');
            }
            quit(&a).await;
        },
    )
    .await;
}

fn snapshot_of(plugins: Vec<PluginEntry>) -> AgentEvent {
    let AgentEvent::SettingsSnapshot(mut snapshot) = super::described(0) else {
        panic!("described is a settings snapshot");
    };
    snapshot.plugins = plugins;
    AgentEvent::SettingsSnapshot(snapshot)
}

async fn unknown(a: &mut Agent, typed: &str) {
    drain(a).await;
    a.paste(typed).key(KeyCode::Enter);
    let lines = a.settle().await;
    let word = typed.trim_start_matches('/');
    assert!(shows(&lines, &copy::unknown_command(word)), "{lines:#?}");
    assert!(drain(a).await.is_empty());
    a.ctrl('c');
}

#[tokio::test(start_paused = true)]
async fn a_sent_reply_waits_between_the_steers_typed_before_and_after_its_command() {
    steering_on(
        Screen::new(ROWS, 100),
        options_in(Mode::Dark),
        |mut a| async move {
            a.emit(catalog(vec![testkit(commands())])).await;
            a.start("Show me the plan", 1).await;
            drain(&mut a).await;
            a.paste("alpha steer").key(KeyCode::Enter);
            let alpha = a.next().await;
            let alpha = a.accept(&alpha).await;
            let (_, _, request) = run(&mut a, "/note").await;
            a.paste("bravo steer").key(KeyCode::Enter);
            let bravo = a.next().await;
            let bravo = a.accept(&bravo).await;
            let reply = InputId("wisp-command-1".to_owned());
            a.emit(AgentEvent::InputAccepted {
                opening: false,
                front: false,
                id: reply.clone(),
                mode: DeliveryMode::Steer,
                text: "charlie reply".to_owned(),
                images: Vec::new(),
                replaces: None,
                held: false,
                from: None,
                checklist_spans: Vec::new(),
            })
            .await;
            done(
                &a,
                request,
                CommandOutcome::Sent {
                    input: reply.clone(),
                },
            )
            .await;
            let lines = a.settle().await;
            let at = |text: &str| {
                lines
                    .iter()
                    .position(|l| l.contains(text))
                    .unwrap_or_else(|| panic!("{text} not drawn: {lines:#?}"))
            };
            assert!(at("alpha steer") < at("charlie reply"), "{lines:#?}");
            assert!(at("charlie reply") < at("bravo steer"), "{lines:#?}");
            a.emit(AgentEvent::InputDelivered {
                ids: vec![alpha, InputId("wisp-command-1".to_owned()), bravo],
                step: 2,
            })
            .await;
            a.emit(super::completed()).await;
            drain(&mut a).await;
            quit(&a).await;
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn esc_stops_a_running_command_until_its_answer_and_a_refusal_answers_it() {
    steering_on(
        Screen::new(ROWS, 100),
        options_in(Mode::Dark),
        |mut a| async move {
            a.emit(catalog(vec![testkit(commands())])).await;
            let (_, _, request) = run(&mut a, "/note").await;
            a.key(KeyCode::Esc);
            assert_eq!(a.next().await, ClientCommand::Cancel);
            a.ctrl('c');
            assert_eq!(a.next().await, ClientCommand::Cancel);
            done(&a, request, CommandOutcome::Cancelled).await;
            a.key(KeyCode::Esc);
            a.nothing_sent().await;
            let (_, _, request) = run(&mut a, "/fail").await;
            a.emit(AgentEvent::refused(request, "not here")).await;
            let lines = a.settle().await;
            assert!(shows(&lines, "/testkit:fail: not here"), "{lines:#?}");
            a.key(KeyCode::Esc);
            a.nothing_sent().await;
            quit(&a).await;
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_between_turns_command_is_refused_with_its_hint_while_busy() {
    steering_on(
        Screen::new(ROWS, 100),
        options_in(Mode::Dark),
        |mut a| async move {
            a.emit(catalog(vec![testkit(commands())])).await;
            a.start("Show me the plan", 1).await;
            drain(&mut a).await;
            a.paste("/wait").key(KeyCode::Enter);
            let lines = a.settle().await;
            assert!(shows(&lines, "/wait runs between turns"), "{lines:#?}");
            assert_eq!(composer(&lines), " \u{203a} /wait");
            assert!(drain(&mut a).await.is_empty());
            a.emit(super::completed()).await;
            a.ctrl('c');
            quit(&a).await;
        },
    )
    .await;
}

async fn lines_scene(a: &mut Agent, plugins: Vec<PluginEntry>) {
    a.emit(catalog(plugins)).await;
    let (_, _, go) = run(a, "/go fast").await;
    deferred(a, go, CommandWait::Call).await;
    deferred(a, go, CommandWait::Command).await;
    let (_, _, waits) = run(a, "/testkit:g").await;
    deferred(a, waits, CommandWait::Hook).await;
    let (_, _, note) = run(a, "/note").await;
    done(
        a,
        note,
        CommandOutcome::Note {
            text: "3 files changed".to_owned(),
        },
    )
    .await;
    let outcomes = [
        CommandOutcome::Failed {
            message: "the server said no".to_owned(),
        },
        CommandOutcome::TimedOut,
        CommandOutcome::Cancelled,
        CommandOutcome::Refused {
            kind: CommandRefusal::Busy,
            hint: "/fail runs between turns".to_owned(),
        },
    ];
    for outcome in outcomes {
        let (_, _, request) = run(a, "/fail").await;
        done(a, request, outcome).await;
    }
}

async fn answer_all(a: &Agent) {
    for request in 1..=20 {
        done(a, RequestId(request), CommandOutcome::Cancelled).await;
    }
}

#[tokio::test(start_paused = true)]
async fn deferred_and_done_lines_in_dark_and_light_and_beside_the_docked_sidebar() {
    let (mode, look) = (Mode::Dark, "dark");
    let styled = shot(mode, 100, |mut a| async move {
        lines_scene(&mut a, vec![testkit(commands())]).await;
        a.settle().await;
        let styled = a.screen.styled();
        answer_all(&a).await;
        drain(&mut a).await;
        quit(&a).await;
        styled
    })
    .await;
    snapshot(&format!("plugin-command-lines-{look}-100"), styled);
    let styled = shot(Mode::Dark, 160, |mut a| async move {
        base(&mut a).await;
        let mut plugins = vec![
            plugin("agent-viz", true, vec![slot(TREE, "Agents")]),
            plugin("demo", true, vec![slot(NOTES, "Notes")]),
        ];
        plugins.push(testkit(commands()));
        lines_scene(&mut a, plugins).await;
        a.settle().await;
        let styled = a.screen.styled();
        answer_all(&a).await;
        finish(a).await;
        styled
    })
    .await;
    snapshot("plugin-command-lines-docked-dark-160", styled);
}

#[tokio::test(start_paused = true)]
async fn plugin_suggestions_and_help_in_dark_and_light() {
    let (mode, look) = (Mode::Dark, "dark");
    let suggested = shot(mode, 100, |a| async move {
        a.emit(catalog(vec![testkit(commands())])).await;
        a.inputs.send(key(KeyCode::Char('/'))).unwrap();
        a.inputs.send(key(KeyCode::Char('t'))).unwrap();
        a.settle().await;
        let styled = a.screen.styled();
        a.ctrl('c');
        quit(&a).await;
        styled
    })
    .await;
    snapshot(&format!("plugin-suggestions-{look}-100"), suggested);
    let help = shot(mode, 100, |a| async move {
        a.emit(catalog(vec![testkit(commands())])).await;
        a.paste("/help").key(KeyCode::Enter);
        a.settle().await;
        let styled = a.screen.styled();
        a.key(KeyCode::Esc);
        quit(&a).await;
        styled
    })
    .await;
    snapshot(&format!("plugin-help-{look}-100"), help);
}

#[tokio::test(start_paused = true)]
async fn a_hostile_plugin_is_drawn_inert_everywhere_its_text_shows() {
    let mut shots = Vec::new();
    let suggested = shot(Mode::Dark, 100, |a| async move {
        a.emit(catalog(vec![testkit(hostile_commands())])).await;
        a.inputs.send(key(KeyCode::Char('/'))).unwrap();
        a.inputs.send(key(KeyCode::Char('e'))).unwrap();
        a.settle().await;
        let styled = a.screen.styled();
        a.ctrl('c');
        quit(&a).await;
        styled
    })
    .await;
    shots.push(suggested);
    let values = shot(Mode::Dark, 100, |a| async move {
        a.emit(catalog(vec![testkit(hostile_commands())])).await;
        for c in "/evil ".chars() {
            a.inputs.send(key(KeyCode::Char(c))).unwrap();
        }
        a.settle().await;
        let styled = a.screen.styled();
        a.ctrl('c');
        quit(&a).await;
        styled
    })
    .await;
    shots.push(values);
    let help = shot(Mode::Dark, 100, |a| async move {
        a.emit(catalog(vec![testkit(hostile_commands())])).await;
        a.paste("/help").key(KeyCode::Enter);
        a.settle().await;
        let styled = a.screen.styled();
        a.key(KeyCode::Esc);
        quit(&a).await;
        styled
    })
    .await;
    shots.push(help);
    let busy = shot(Mode::Dark, 100, |mut a| async move {
        a.emit(catalog(vec![testkit(hostile_commands())])).await;
        a.start("Show me the plan", 1).await;
        a.paste("/evil").key(KeyCode::Enter);
        a.settle().await;
        let styled = a.screen.styled();
        a.emit(super::completed()).await;
        a.ctrl('c');
        drain(&mut a).await;
        quit(&a).await;
        styled
    })
    .await;
    shots.push(busy);
    let lines = shot(Mode::Dark, 100, |mut a| async move {
        a.emit(catalog(vec![testkit(hostile_commands())])).await;
        let (_, _, first) = run(&mut a, "/evil").await;
        deferred(&a, first, CommandWait::Call).await;
        done(
            &a,
            first,
            CommandOutcome::Failed {
                message: HOSTILE.to_owned(),
            },
        )
        .await;
        let (_, _, second) = run(&mut a, "/testkit:evil").await;
        deferred(&a, second, CommandWait::Call).await;
        let (_, _, third) = run(&mut a, "/evil").await;
        done(
            &a,
            third,
            CommandOutcome::Note {
                text: HOSTILE.to_owned(),
            },
        )
        .await;
        let (_, _, fourth) = run(&mut a, "/evil").await;
        done(
            &a,
            fourth,
            CommandOutcome::Refused {
                kind: CommandRefusal::Other,
                hint: HOSTILE.to_owned(),
            },
        )
        .await;
        a.settle().await;
        let styled = a.screen.styled();
        done(&a, second, CommandOutcome::Cancelled).await;
        drain(&mut a).await;
        quit(&a).await;
        styled
    })
    .await;
    shots.push(lines);
    for shot in &shots {
        assert!(!shot.contains('\u{202e}'), "{shot}");
        assert!(!shot.contains("\u{1b}[2J"), "{shot}");
    }
    snapshot("plugin-hostile-dark-100", shots.join("\n----\n"));
}

fn switched_off() -> AgentEvent {
    use wisp_protocol::settings::{
        AppliedChange, ENABLED_KEY, Layer, SettingTarget, SettingValue, SitePath, When,
    };
    settings_changed(Changed {
        answers: Some(RequestId(90)),
        endpoint: "claude-api".into(),
        model: "test-model".into(),
        changes: vec![AppliedChange {
            cache: Some(wisp_protocol::CacheEffect::Kept),
            ..change(
                SettingTarget::Path(SitePath::plugin_switch("testkit", ENABLED_KEY).unwrap()),
                Some(SettingValue::Bool(false)),
                Layer::Session,
                When::NextTurn,
            )
        }],
        ..Changed::default()
    })
}

#[tokio::test(start_paused = true)]
async fn a_plugin_switched_off_leaves_the_slash_list_at_the_applying_catalog_not_at_the_change() {
    steering_on(
        Screen::new(ROWS, 100),
        options_in(Mode::Dark),
        |mut a| async move {
            a.emit(catalog(vec![testkit(commands())])).await;
            a.emit(switched_off()).await;
            let (name, _, first) = run(&mut a, "/go").await;
            assert_eq!(name, "testkit:go");
            done(&a, first, CommandOutcome::None).await;
            a.emit(catalog(vec![PluginEntry {
                enabled: false,
                ..testkit(commands())
            }]))
            .await;
            unknown(&mut a, "/go").await;
            quit(&a).await;
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_key_in_a_plugin_command_is_refused_before_send_echo_or_recall() {
    steering_on(
        Screen::new(ROWS, 100),
        options_in(Mode::Dark),
        |mut a| async move {
            a.emit(catalog(vec![testkit(commands())])).await;
            a.settle().await;
            for value in [
                "sk-ant-api03-CANARYMODEL0000",
                "TOKEN=sk-ant-api03-CANARYMODEL0000",
                "'sk-ant-api03-CANARYMODEL0000'",
            ] {
                a.paste(&format!("/go {value}")).key(KeyCode::Enter);
                a.nothing_sent().await;
                let lines = a.settle().await;
                assert!(
                    lines
                        .iter()
                        .any(|line| line.contains(copy::KEY_SHAPED_COMMAND)),
                    "{lines:?}"
                );
                assert!(
                    !lines.iter().any(|line| line.contains("CANARY")),
                    "{lines:?}"
                );
                a.key(KeyCode::Up);
                let lines = a.settle().await;
                assert!(
                    !lines.iter().any(|line| line.contains("CANARY")),
                    "{lines:?}"
                );
            }
            finish(a).await;
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn typed_plugin_commands_echo_with_opaque_secrets_masked_and_skills_keeps_its_typed_line() {
    steering_on(
        Screen::new(ROWS, 100),
        options_in(Mode::Dark),
        |mut a| async move {
            a.emit(catalog(vec![testkit(vec![
                command("mcp", &[], &[], None),
                command("skills", &[], &[], None),
            ])]))
            .await;
            let secret = "CANARY_OPAQUE_VALUE_0011";
            let (_, args, request) =
                run(&mut a, &format!("/mcp add notes server --token {secret}")).await;
            assert!(args.ends_with(secret));
            answered(&a, request, CommandOutcome::None, true).await;
            let lines = a.settle().await;
            assert!(!lines.iter().any(|line| line.contains(secret)), "{lines:?}");
            assert!(
                lines
                    .iter()
                    .any(|line| line.contains("/mcp add notes server --token")
                        && line.contains("0011")),
                "{lines:?}"
            );
            let (_, args, request) = run(&mut a, &format!("/mcp\u{00a0}--token {secret}")).await;
            assert_eq!(args, format!("--token {secret}"));
            answered(&a, request, CommandOutcome::None, true).await;
            let masked = wisp_protocol::masked_command(&args).unwrap();
            let lines = a.settle().await;
            assert!(shows(&lines, &format!("/mcp {masked}")));
            assert!(!shows(&lines, secret));
            a.key(KeyCode::Up);
            assert!(composer(&a.settle().await).contains(&format!("/mcp {masked}")));
            a.key(KeyCode::Down);
            let code = "CANARY-code-0011";
            let address =
                format!("http://127.0.0.1:8123/oauth-callback?%63ode={code}&state=CANARY-state");
            let redirect = format!("/mcp sign-in linear '{address}' # {address}");
            let (_, args, request) = run(&mut a, &redirect).await;
            assert!(args.contains(code));
            answered(&a, request, CommandOutcome::None, true).await;
            let lines = a.settle().await;
            assert!(!lines.iter().any(|line| line.contains(code)));
            assert!(lines.iter().any(|line| line.contains("state=CANARY-state")));
            a.key(KeyCode::Up);
            let lines = a.settle().await;
            assert!(!composer(&lines).contains(code));
            assert!(!lines.iter().any(|line| line.contains(code)));
            a.key(KeyCode::Down);
            let (_, _, request) = run(&mut a, "/skills").await;
            done(&a, request, CommandOutcome::None).await;
            let lines = a.settle().await;
            assert!(
                lines.iter().any(|line| line.contains("› /skills")),
                "{lines:?}"
            );
            finish(a).await;
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn command_args_enter_recall_only_with_an_explicit_safe_answer() {
    steering_on(
        Screen::new(ROWS, 100),
        options_in(Mode::Dark),
        |mut a| async move {
            a.emit(catalog(vec![testkit(commands())])).await;
            for (word, approval) in [
                ("CANARY-held-word", "false"),
                ("invented-legacy", "omitted"),
                ("invented-rejected", "rejected"),
                ("invented-safe", "true"),
            ] {
                let (_, _, request) = run(&mut a, &format!("/go {word}")).await;
                assert!(!shows(&a.settle().await, word));
                match approval {
                    "omitted" => {
                        let event = format!(
                            r#"{{"type":"command_done","answers":{},"outcome":{{"type":"none"}}}}"#,
                            request.0
                        );
                        a.emit(serde_json::from_str(&event).unwrap()).await;
                    }
                    "rejected" => {
                        a.emit(AgentEvent::Rejected {
                            request: Some(request),
                            id: None,
                            kind: None,
                            reason: "Unavailable.".into(),
                        })
                        .await
                    }
                    _ => answered(&a, request, CommandOutcome::None, approval == "true").await,
                }
                let lines = a.settle().await;
                assert!(shows(&lines, "/go"));
                assert_eq!(shows(&lines, word), approval == "true");
                a.key(KeyCode::Up);
                assert_eq!(
                    composer(&a.settle().await).contains(word),
                    approval == "true"
                );
                a.key(KeyCode::Down);
            }
            finish(a).await;
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn disconnect_drops_unchecked_plugin_args_without_echo_or_recall() {
    const WORD: &str = "invented-pending-argument";
    let mut opts = options_in(Mode::Dark);
    let ended = super::steering::attached(&mut opts, wisp_protocol::link::REQUEST_RANGE);
    let screen = Screen::new(ROWS, 100);
    let finished =
        super::steering::steering_finished(screen, opts, super::facts(), |mut a| async move {
            a.emit(catalog(vec![testkit(commands())])).await;
            run(&mut a, &format!("/go {WORD}")).await;
            a.key(KeyCode::Up);
            assert!(!shows(&a.settle().await, WORD));
            a.cut(ended).await;
        })
        .await;
    assert_eq!(finished.exit.unwrap(), wisp_tui::app::Exit::Cut);
    assert!(finished.unshown.iter().all(|line| !line.contains(WORD)));
}
