use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use wisp_client::{Channels, QuitBound};
use wisp_core::agent::DEFAULT_MAX_TOKENS;
use wisp_protocol::{CacheTtl, Effort, Opening as ChatOpening, SessionView};
use wisp_tui::app::endpoint_card::CardSetup;
use wisp_tui::app::replay::{self, Script, Settings};
use wisp_tui::app::resumed::{Continuing, Resumed};
use wisp_tui::app::setup::{Opening, SAVE_ABANDONED, SETUP_CANCELLED, SessionStart, setup_saved};
use wisp_tui::app::start::{LaunchError, Launched, launch};
use wisp_tui::app::{Exit, Session, SessionFacts};
use wisp_tui::term::Flags;
use wisp_tui::term::prompt::CANCEL_EXIT;

use crate::output::{RunEnd, SAVE_CUT};
pub(crate) use wisp_start::models::{Chosen, models};
#[cfg(test)]
pub(crate) use wisp_start::models::{caps_of, image_tier_of, rates_of, write_multipliers};

pub(crate) fn notes(warnings: Vec<String>) -> Vec<String> {
    warnings
        .into_iter()
        .map(|w| match w.strip_prefix("wisp: ") {
            Some(rest) => rest.to_owned(),
            None => w,
        })
        .collect()
}

pub(crate) fn facts(
    chosen: Chosen<'_>,
    credential: Option<wisp_render::chrome::Credential>,
    host: Option<String>,
    log: Option<String>,
) -> SessionFacts {
    wisp_start::models::facts(chosen, credential.map(|kind| kind.wire()), host, log).into()
}

pub(crate) fn session(
    mut opening: ChatOpening,
    view: Option<SessionView>,
    channels: Channels,
) -> (SessionStart, CardSetup) {
    let card = std::mem::take(&mut opening.card);
    let start = opening.start.take().unwrap_or_default();
    let facts = SessionFacts::from(opening);
    let billing = facts
        .credential
        .map_or(wisp_protocol::Billing::Metered, |kind| kind.billing());
    let continuing = start.continued.map(|value| Continuing::of(value, billing));
    let Channels {
        commands,
        events,
        quit,
    } = channels;
    let events = crate::browser::opening(
        events,
        crate::auth::system_opener(wisp_core::mcp::is_sign_in_link),
    );
    (
        SessionStart {
            session: Session {
                leave_note: None,
                attached: None,
                alerts: Vec::new(),
                facts,
                events,
                commands,
                resumed: view.map(|view| Resumed { view, continuing }),
            },
            started: start.note,
            warnings: notes(start.warnings),
            quit,
        },
        card,
    )
}

pub(crate) async fn run(
    opening: ChatOpening,
    view: Option<SessionView>,
    channels: Channels,
    display: &wisp_tui::term::SavedDisplay,
    verbose: bool,
) -> RunEnd {
    let (start, card) = session(opening, view, channels);
    let opening = Opening::Session {
        session: Box::new(start.session),
        card,
        notes: start.warnings,
        started: start.started,
    };
    open(
        opening,
        start.quit,
        display,
        verbose,
        &mut crate::keep::Back::default(),
    )
    .await
}

pub(crate) async fn pick(
    chats: Vec<wisp_protocol::ChatRow>,
    focus: Option<&str>,
    display: &wisp_tui::term::SavedDisplay,
    verbose: bool,
) -> Option<String> {
    let opening = Opening::Resume {
        chats,
        focus: focus.map(str::to_owned),
    };
    let launched = launch(opening, Flags::default(), display, verbose, None, None).await;
    match launched.exit {
        Ok(Exit::Resume(id)) => Some(id),
        Ok(_) => None,
        Err(e) => {
            drop(writeln!(std::io::stderr(), "wisp: {e}"));
            None
        }
    }
}

pub(crate) async fn open(
    opening: Opening,
    quit: Option<QuitBound>,
    display: &wisp_tui::term::SavedDisplay,
    verbose: bool,
    back: &mut crate::keep::Back,
) -> RunEnd {
    let Launched {
        unsent,
        exit,
        goodbye,
        warnings,
        unshown,
    } = launch(
        opening,
        Flags::default(),
        display,
        verbose,
        quit,
        back.unsent.take(),
    )
    .await;
    let cut = matches!(exit, Ok(Exit::Cut | Exit::Restarting));
    back.restarting = matches!(exit, Ok(Exit::Restarting));
    let (mut end, note) = report(exit);
    if cut {
        back.unsent = unsent;
        end.cut = Some(goodbye);
    } else {
        drop(std::io::stdout().write_all(&goodbye));
        crate::keep::hand_back(unsent.as_ref());
    }
    let mut err = std::io::stderr();
    for line in stderr_lines(&warnings, note.as_deref(), &unshown) {
        drop(writeln!(err, "{line}"));
    }
    end
}

fn stderr_lines(warnings: &[String], note: Option<&str>, unshown: &[String]) -> Vec<String> {
    use wisp_render::text::sanitize;
    warnings
        .iter()
        .map(|w| sanitize(w))
        .chain(note.map(str::to_owned))
        .chain(unshown.iter().map(|u| format!("wisp: {}", sanitize(u))))
        .collect()
}

fn report(result: Result<Exit, LaunchError>) -> (RunEnd, Option<String>) {
    let failed = |note: Option<String>| (RunEnd::code(1), note);
    match result {
        Ok(Exit::Quit | Exit::Taken) => (RunEnd::code(0), None),
        Ok(Exit::Leave) => (
            RunEnd {
                leave: true,
                ..RunEnd::code(0)
            },
            None,
        ),
        Ok(Exit::Lost | Exit::Cut) => failed(None),
        Ok(Exit::Restarting) => (RunEnd::code(0), None),
        Ok(Exit::Resume(id)) => (
            RunEnd {
                resume: Some(id),
                ..RunEnd::code(0)
            },
            None,
        ),
        Ok(Exit::Ended) => failed(Some("wisp: the agent stopped unexpectedly".to_owned())),
        Ok(Exit::Dying) => failed(None),
        Ok(Exit::InputStopped(reason)) => failed(Some(format!("wisp: input stopped: {reason}"))),
        Ok(Exit::Forced { save_waiting }) => (
            RunEnd::forced(),
            save_waiting.then(|| format!("wisp: {SAVE_CUT}")),
        ),
        Ok(Exit::SetupCancelled) => (RunEnd::code(CANCEL_EXIT), Some(SETUP_CANCELLED.to_owned())),
        Ok(Exit::SetupSaved { name, forced }) => (
            RunEnd {
                forced,
                ..RunEnd::code(0)
            },
            Some(setup_saved(&wisp_render::text::sanitize(&name))),
        ),
        Ok(Exit::SaveAbandoned) => (
            RunEnd {
                forced: true,
                ..RunEnd::code(CANCEL_EXIT)
            },
            Some(SAVE_ABANDONED.to_owned()),
        ),
        Ok(Exit::StartFailed(text)) => failed(Some(text)),
        Err(e) => failed(Some(format!("wisp: {e}"))),
    }
}

pub(crate) mod replaying;

pub(crate) fn replay_facts(
    endpoint: &replaying::ReplayEndpoint,
    model: &str,
    effort: Effort,
    cache_ttl: CacheTtl,
) -> SessionFacts {
    let chosen = Chosen {
        model,
        effort: effort.as_str(),
        cache_ttl,
        endpoint: endpoint.reach,
        max_tokens: DEFAULT_MAX_TOKENS,
        claude_usage: false,
    };
    let mut facts = facts(chosen, None, None, None);
    facts.endpoint_name = Some(endpoint.name.clone());
    facts
}

pub(crate) async fn replay(path: &Path, settings: Settings, facts: SessionFacts) -> ExitCode {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) => {
            eprintln!(
                "wisp: could not read {}: {}",
                wisp_core::workspace::home_shown(path),
                e.kind()
            );
            return ExitCode::FAILURE;
        }
    };
    let events = match replay::parse(&text) {
        Ok(events) => events,
        Err(e) => {
            eprintln!("wisp: {}: {e}", wisp_core::workspace::home_shown(path));
            return ExitCode::FAILURE;
        }
    };
    let script = Script::new(events).with_settings(settings);
    let (commands, events) = replay::spawn(script, replay::PACE);
    let channels = Channels {
        commands,
        events,
        quit: None,
    };
    let opening = Opening::Session {
        session: Box::new(Session {
            leave_note: None,
            attached: None,
            alerts: Vec::new(),
            facts,
            events: channels.events,
            commands: channels.commands,
            resumed: None,
        }),
        card: CardSetup::default(),
        notes: Vec::new(),
        started: None,
    };
    open(
        opening,
        channels.quit,
        &wisp_tui::term::SavedDisplay::default(),
        false,
        &mut crate::keep::Back::default(),
    )
    .await
    .exit_code()
}

#[cfg(test)]
mod account_agreement;

#[cfg(test)]
mod agreement;

#[cfg(test)]
mod screen_agreement;

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};
    use wisp_core::agent::Connection;
    use wisp_core::endpoints::ProviderKind;
    use wisp_core::pricing;
    use wisp_core::provider::shipped_capabilities;
    use wisp_render::chrome::Endpoint;

    use super::*;
    use crate::Args;
    use crate::output::OutputMode;

    #[test]
    fn notes_drop_one_leading_wisp_mark_and_nothing_else() {
        assert_eq!(
            notes(vec![
                "wisp: a start warning".into(),
                "wisp: wisp: twice".into(),
                "no mark".into(),
                "a wisp: inside".into(),
            ]),
            [
                "a start warning",
                "wisp: twice",
                "no mark",
                "a wisp: inside"
            ]
        );
    }

    #[test]
    fn a_screen_that_never_opened_prints_its_notes_after_the_error() {
        assert_eq!(
            stderr_lines(
                &["note: a save note".to_owned()],
                Some("wisp: no terminal"),
                &["a start warning".to_owned()]
            ),
            [
                "note: a save note",
                "wisp: no terminal",
                "wisp: a start warning"
            ]
        );
        assert_eq!(
            stderr_lines(&[], None, &["a \x1b[2Jwarning".to_owned()]),
            ["wisp: a ^[[2Jwarning"]
        );
    }

    #[test]
    fn openai_ui_write_rates_come_from_the_central_price_table() {
        let rates = rates_of(pricing::model_info("gpt-6.1-sol").unwrap());
        let writes = rates.writes.unwrap();
        assert_eq!(writes.prefix, 1.0);
        assert_eq!(writes.at(CacheTtl::FiveMinutes).tail, 1.0);
        assert_eq!(writes.at(CacheTtl::OneHour).tail, 1.0);
    }

    #[test]
    fn model_facts_equal_the_capability_rows_and_list_every_priced_model() {
        let models = models(&[]);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        let mut described: Vec<&str> = wisp_core::models::ModelTable::shared()
            .described_ids()
            .collect();
        described.sort_unstable();
        assert_eq!(ids, described);
        for id in pricing::model_ids() {
            assert!(ids.contains(&id), "{id}");
        }
        for facts in &models {
            let id = &facts.id;
            let (kind, row) = shipped_capabilities(id).unwrap();
            assert!(facts.shipped, "{id}");
            let caps = facts.caps.as_ref().unwrap();
            assert_eq!(caps.efforts, row.efforts, "{id}");
            assert_eq!(caps.default_effort, row.default_effort, "{id}");
            assert_eq!(caps.per_message_effort, row.per_message_effort, "{id}");
            assert_eq!(caps.adaptive_thinking, row.adaptive_thinking, "{id}");
            assert_eq!(facts.selectable(), row.selectable(kind), "{id}");
            assert_eq!(
                facts.rates.is_some(),
                pricing::model_info(id).is_some(),
                "{id}"
            );
        }
        use crate::resume::tests::{connection, listed};
        let first = listed(&[
            ("gpt-6.1-sol", "list", "medium"),
            ("gpt-new", "hide", "low"),
        ]);
        let other = listed(&[("gpt-5.5", "list", "low"), ("gpt-new", "list", "medium")]);
        let ready = [
            connection("a", Some(first.clone())),
            connection("b", Some(other)),
        ];
        let lists = super::models(&ready);
        let facts = |id: &str| lists.iter().find(|m| m.id == id).unwrap();
        let sol = wisp_core::provider::openai::listed_capabilities(Some(&first), "gpt-6.1-sol");
        let sol = sol.unwrap();
        assert_eq!(
            facts("gpt-6.1-sol").caps,
            Some(caps_of(&sol, ProviderKind::OpenAi))
        );
        let new = facts("gpt-new");
        assert_eq!(
            (new.provider.as_deref(), new.listed, new.shipped),
            (None, None, false)
        );
        assert!(new.rates.is_none() && new.selectable());
        assert_eq!(new.caps.as_ref().unwrap().default_effort, Some(Effort::Low));
        let ranked = super::models(&[connection("b", Some(listed(&[("gpt-new", "list", "low")])))]);
        assert_eq!(
            ranked.iter().find(|m| m.id == "gpt-new").unwrap().listed,
            Some(0)
        );
        let entries = [
            ("claude-opus-5-5", true),
            ("claude-opus-6", true),
            ("claude-opus-7", false),
        ];
        let claude = claude_connection(&entries);
        let lists = super::models(std::slice::from_ref(&claude));
        let facts = |id: &str| lists.iter().find(|m| m.id == id).unwrap();
        let opus = claude.provider.capabilities("claude-opus-5-5").unwrap();
        assert_eq!(
            opus.efforts,
            [Effort::Low, Effort::Medium, Effort::High, Effort::Max]
        );
        let opus_facts = Some(caps_of(&opus, ProviderKind::Anthropic));
        assert_eq!(facts("claude-opus-5-5").caps, opus_facts);
        let new = facts("claude-opus-6");
        assert_eq!(
            (new.provider.as_deref(), new.listed, new.shipped),
            (Some("anthropic"), Some(1), false)
        );
        use wisp_protocol::settings::{Landing, offered, runs_on};
        let offers = wisp_render::chrome::offers(&lists);
        let chat = wisp_render::chrome::chat_of(&lists, &new.id);
        let to = Landing::new("anthropic", &[], false, None);
        assert!(offered(&offers, to, chat).contains(&new.id));
        assert_eq!(runs_on(&offers, to, chat), new.id);
        assert!(new.rates.is_none() && new.selectable());
        assert!(!facts("claude-opus-7").selectable());
    }

    fn claude_connection(entries: &[(&str, bool)]) -> Connection {
        use wisp_core::endpoints::{EndpointName, PricingMode, ResolvedEndpoint, UrlSource};
        use wisp_core::provider::credential::{Credential, Source};
        let on = |supported: bool| serde_json::json!({ "supported": supported });
        let entries: Vec<_> = entries
            .iter()
            .map(|(id, adaptive)| {
                serde_json::json!({
                    "id": id, "lifecycle": "active", "line": "opus", "max_input_tokens": 1_000_000,
                    "max_tokens": 128_000,
                    "capabilities": {
                        "effort": { "supported": true, "low": on(true), "medium": on(true),
                            "high": on(true), "xhigh": null, "max": on(true) },
                        "thinking": { "types": { "adaptive": on(*adaptive) } },
                        "image_input": on(true), "server_tools": { "web_search": on(true) },
                    },
                })
            })
            .collect();
        let body = serde_json::json!({ "data": entries }).to_string();
        let list = wisp_core::models::list::anthropic::parse(body.as_bytes()).unwrap();
        let endpoint = ResolvedEndpoint {
            client_version: None,
            name: EndpointName::parse("claude-api").unwrap(),
            provider: ProviderKind::Anthropic,
            credential: Credential::ApiKey("fixture-key".into()),
            base_url: wisp_core::provider::anthropic::DEFAULT_BASE_URL.into(),
            source: Source::Keyring,
            key_env: None,
            url_source: UrlSource::Official,
            pricing: PricingMode::List,
        };
        let dir = crate::scratch_dir("claude-facts");
        let key = ProviderKind::Anthropic.connect(&endpoint, None).unwrap();
        let key = key.list_key().unwrap().to_owned();
        let saved = wisp_core::models::list::SavedList {
            attempted: 1,
            failed: false,
            list,
            default_model: None,
            web_search_models: Vec::new(),
        };
        saved.save(&dir.root, &key).unwrap();
        let provider = ProviderKind::Anthropic.connect(&endpoint, Some(&dir.root));
        Connection {
            provider: provider.unwrap(),
            ..crate::resume::tests::connection("claude-api", None)
        }
    }

    #[test]
    fn the_tuis_input_rate_equals_rates_for_on_each_side_of_every_threshold() {
        for model in models(&[]) {
            let Some(info) = pricing::model_info(&model.id) else {
                assert_eq!(model.rates, None, "{}", model.id);
                continue;
            };
            let rates = model.rates.unwrap();
            assert_eq!(rates.output_per_mtok, info.rates.output_per_mtok);
            let mut sizes = vec![0, 1, 1_000, 200_000, u64::MAX];
            if let Some(tier) = info.long_prompt {
                let at = tier.prompt_tokens_over;
                sizes.extend([at - 1, at, at + 1]);
            }
            for prompt in sizes {
                let usage = wisp_protocol::Usage {
                    input_tokens: prompt,
                    ..wisp_protocol::Usage::default()
                };
                assert_eq!(
                    rates.input_for(prompt),
                    info.rates_for(&usage).input_per_mtok,
                    "{} at {prompt}",
                    model.id
                );
            }
        }
    }

    #[test]
    fn the_write_multipliers_come_from_pricing() {
        let one_hour = write_multipliers(CacheTtl::OneHour);
        assert_eq!((one_hour.prefix, one_hour.tail), (2.0, 2.0));
        let five = write_multipliers(CacheTtl::FiveMinutes);
        assert_eq!((five.prefix, five.tail), (2.0, 1.25));
    }

    #[test]
    fn replays_name_no_credential() {
        let chosen = Chosen {
            model: "replay",
            effort: "replay",
            cache_ttl: CacheTtl::OneHour,
            endpoint: Endpoint::Official,
            max_tokens: DEFAULT_MAX_TOKENS,
            claude_usage: false,
        };
        let on = Chosen {
            claude_usage: true,
            ..chosen
        };
        assert!(facts(on, None, None, None).claude_usage);
        let facts = facts(chosen, None, None, None);
        assert!(!facts.claude_usage);
        assert_eq!(facts.credential, None);
        assert_eq!(facts.host, None);
        assert_eq!(facts.credential(), None);
        assert_eq!(
            facts.credential_label(
                usize::MAX,
                &wisp_render::theme::glyphs::Glyphs::of(
                    wisp_render::theme::glyphs::GlyphSet::Unicode
                ),
                wisp_render::text::WidthMode::Normal
            ),
            "replay, no API calls"
        );
    }

    #[test]
    fn a_stopped_input_reader_fails_with_a_note() {
        assert_eq!(report(Ok(Exit::Quit)), (RunEnd::code(0), None));
        assert_eq!(
            report(Ok(Exit::InputStopped("tty gone".to_owned()))),
            (
                RunEnd::code(1),
                Some("wisp: input stopped: tty gone".to_owned())
            )
        );
        assert_eq!(report(Ok(Exit::Dying)), (RunEnd::code(1), None));
        assert_eq!(
            report(Ok(Exit::Ended)),
            (
                RunEnd::code(1),
                Some("wisp: the agent stopped unexpectedly".to_owned())
            )
        );
    }

    #[test]
    fn a_forced_quit_exits_130_and_names_a_waiting_save_only() {
        let forced = RunEnd::forced();
        assert_eq!(
            report(Ok(Exit::Forced {
                save_waiting: false
            })),
            (forced.clone(), None)
        );
        assert_eq!(
            report(Ok(Exit::Forced { save_waiting: true })),
            (
                forced,
                Some(
                    "wisp: a settings save was still waiting for config.toml; it may not be saved."
                        .to_owned()
                )
            )
        );
    }

    #[test]
    fn each_setup_end_has_its_code_and_line() {
        assert_eq!(
            report(Ok(Exit::SetupCancelled)),
            (RunEnd::code(130), Some("Nothing was set up.".to_owned()))
        );
        assert_eq!(
            report(Ok(Exit::SetupSaved {
                name: "manifest".to_owned(),
                forced: true
            })),
            (
                RunEnd {
                    forced: true,
                    ..RunEnd::code(0)
                },
                Some("Saved \"manifest\". wisp starts on it next time.".to_owned())
            )
        );
        assert_eq!(
            report(Ok(Exit::SaveAbandoned)),
            (
                RunEnd::forced(),
                Some("wisp: an account save was still running; it may not be saved.".to_owned())
            )
        );
        assert_eq!(
            report(Ok(Exit::StartFailed("wisp: no luck".to_owned()))),
            (RunEnd::code(1), Some("wisp: no luck".to_owned()))
        );
    }

    #[test]
    fn auto_is_the_default_and_tui_can_be_asked_for() {
        assert_eq!(
            Args::try_parse_from(["wisp"]).unwrap().output,
            OutputMode::Auto
        );
        let args = Args::try_parse_from(["wisp", "--output", "tui"]).unwrap();
        assert_eq!(args.output, OutputMode::Tui);
        assert!(Args::try_parse_from(["wisp", "--output", "fancy"]).is_err());
    }

    #[test]
    fn replay_takes_a_file_and_stays_out_of_the_help() {
        let args = Args::try_parse_from(["wisp", "--replay", "turns.jsonl"]).unwrap();
        assert_eq!(args.replay.as_deref(), Some(Path::new("turns.jsonl")));
        let help = Args::command().term_width(0).render_help().to_string();
        assert!(!help.contains("--replay"), "{help}");
        assert!(help.contains("--output"), "{help}");
    }
}
