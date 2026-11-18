use super::*;
use crate::term::clipboard::Destination;
use crate::testing::{line_text, texts, trimmed};
use wisp_render::styled::Look;
use wisp_render::theme::glyphs::GlyphSet;

const CLAUDE_URL: &str = "https://claude.ai/oauth/authorize?code=true&state=EXAMPLE";
const CHATGPT_URL: &str = "https://auth.openai.com/oauth/authorize?state=EXAMPLE";

fn endpoints() -> Vec<Endpoint> {
    vec![
        Endpoint {
            alias: "chatgpt".into(),
            provider: Some(SubscriptionProvider::ChatGpt),
            working: true,
            mark: Some(3),
        },
        Endpoint {
            alias: "chatgpt-2".into(),
            provider: Some(SubscriptionProvider::ChatGpt),
            working: true,
            mark: Some(6),
        },
        Endpoint {
            alias: "work".into(),
            provider: Some(SubscriptionProvider::Claude),
            working: true,
            mark: Some(2),
        },
        Endpoint {
            alias: "api".into(),
            provider: None,
            working: true,
            mark: Some(4),
        },
    ]
}
fn wizard(words: &str) -> (Wizard, Option<Intent>) {
    let known = endpoints();
    Wizard::new(route(words, &known).unwrap(), known, RequestId(1)).unwrap()
}
fn enter(wizard: &mut Wizard) -> Option<Intent> {
    wizard.key(Key::plain(KeyCode::Enter), Duration::ZERO)
}
fn screen(wizard: &Wizard, width: usize, glyphs: GlyphSet) -> String {
    let known = endpoints().into_iter().find(|e| e.alias == wizard.alias());
    let mark = known.map_or(Some(5), |e| e.mark);
    shown(&wizard.render(24, Layout::new(width, glyphs), mark))
}
fn shown(rows: &CardRows) -> String {
    trimmed(&rows.lines).join("\n")
}

fn typed(wizard: &mut Wizard, text: &str) {
    for c in text.chars() {
        wizard.key(Key::plain(KeyCode::Char(c)), Duration::ZERO);
    }
}

#[test]
fn shortcut_grammar_defaults_and_explicit_names_ask_only_missing_questions() {
    let (mut empty, action) = wizard("");
    assert!(action.is_none());
    assert_eq!(empty.phase, Phase::Provider);
    enter(&mut empty);
    assert_eq!(empty.phase, Phase::Account);
    assert_eq!(empty.provider, Some(SubscriptionProvider::Claude));
    let (mut chatgpt, action) = wizard("chatgpt");
    assert!(action.is_none());
    let shown = screen(&chatgpt, 80, GlyphSet::Unicode);
    assert!(shown.contains("1 Sign in again to ● chatgpt\n"), "{shown}");
    assert!(shown.contains("2 Sign in again to ● chatgpt-2"), "{shown}");
    assert!(shown.contains("3 Add another ChatGPT account"), "{shown}");
    chatgpt.key(Key::plain(KeyCode::Char('3')), Duration::ZERO);
    assert_eq!(chatgpt.phase, Phase::Name);
    assert_eq!(chatgpt.alias, "");
    assert!(screen(&chatgpt, 80, GlyphSet::Unicode).contains("chatgpt is taken."));
    assert!(!screen(&chatgpt, 80, GlyphSet::Unicode).contains("chatgpt-3"));
    typed(&mut chatgpt, "Home");
    assert_eq!(chatgpt.alias, "home");
    assert!(
        matches!(enter(&mut chatgpt), Some(Intent::Start { selection, .. }) if selection.alias == "home" && !selection.reauthorize)
    );
    let (mut again, _) = wizard("chatgpt");
    assert!(
        matches!(again.key(Key::plain(KeyCode::Char('2')), Duration::ZERO), Some(Intent::Start { selection, .. }) if selection.alias == "chatgpt-2" && selection.reauthorize)
    );
    let (mut named, action) = wizard("new-name");
    assert!(action.is_none());
    assert_eq!(named.phase, Phase::Provider);
    assert!(
        matches!(named.key(Key::plain(KeyCode::Char('1')), Duration::ZERO), Some(Intent::Start { selection, .. }) if selection.provider == SubscriptionProvider::Claude && selection.alias == "new-name")
    );
    let (_, action) = wizard("work");
    assert!(
        matches!(action, Some(Intent::Start { selection, .. }) if selection.provider == SubscriptionProvider::Claude && selection.reauthorize)
    );
    let (_, action) = wizard("claude work");
    assert!(
        matches!(action, Some(Intent::Start { selection, .. }) if selection.alias == "work" && selection.reauthorize)
    );
    assert!(
        Wizard::new(
            route("chatgpt work", &endpoints()).unwrap(),
            endpoints(),
            RequestId(1)
        )
        .is_err()
    );
    assert!(route("api", &endpoints()).is_err());
    assert!(route("claude env", &endpoints()).is_err());
    assert_eq!(
        route("ENV", &endpoints()).unwrap_err(),
        "That name is reserved for the keys-file account; choose another."
    );
    assert_eq!(login_hint("chatgpt"), "/login --name chatgpt");
    assert_eq!(login_hint("status"), "/login --name status");
    assert_eq!(login_hint("work"), "/login work");
    assert_eq!(login_hint("key"), "/login key");
}

#[test]
fn secret_code_never_enters_rendered_text_debug_or_later_state() {
    let (mut wizard, _) = wizard("claude new");
    assert!(
        wizard
            .challenged(
                RequestId(2),
                RequestId(1),
                ChallengeKind::ManualCode,
                Secret::new("WRONG_URL_CANARY"),
                false,
                Duration::from_secs(600),
                Duration::ZERO
            )
            .is_none()
    );
    assert!(wizard.url.is_none());
    wizard.challenged(
        RequestId(1),
        RequestId(1),
        ChallengeKind::ManualCode,
        Secret::new("https://example.invalid/oauth?state=URL_CANARY"),
        false,
        Duration::from_secs(600),
        Duration::ZERO,
    );
    wizard.paste("CODE_CANARY#STATE_CANARY\n");
    let unseen = |wizard: &Wizard| {
        assert!(!format!("{wizard:?}").contains("CANARY"));
        for glyphs in [GlyphSet::Ascii, GlyphSet::Unicode] {
            for width in [40, 80, 120] {
                let shown = screen(wizard, width, glyphs);
                assert!(!shown.contains("CODE_CANARY") && !shown.contains("STATE_CANARY"));
                assert!(!shown.contains("URL_CANARY"), "{shown}");
                assert!(shown.contains("Code  "), "{shown}");
            }
        }
    };
    unseen(&wizard);
    let Some(Intent::Finish {
        response: Some(response),
        login,
    }) = enter(&mut wizard)
    else {
        panic!("finish missing")
    };
    assert_eq!(login, RequestId(1));
    assert_eq!(response.expose(), "CODE_CANARY#STATE_CANARY");
    assert!(!format!("{response:?}").contains("CANARY"));
    assert_eq!(wizard.code.value().expose(), "CODE_CANARY#STATE_CANARY");
    unseen(&wizard);
    wizard.waits_on(RequestId(3));
    assert!(!wizard.ready(RequestId(2), false));
    assert!(wizard.ready(RequestId(3), false));
    assert!(wizard.url.is_none() && !wizard.code.filled());
    assert!(!screen(&wizard, 80, GlyphSet::Unicode).contains("CANARY"));
}

#[test]
fn cancellation_and_expiry_drop_private_state_and_reject_late_success() {
    for expired in [false, true] {
        let (mut wizard, _) = wizard("claude new");
        wizard.challenged(
            RequestId(1),
            RequestId(1),
            ChallengeKind::ManualCode,
            Secret::new("https://claude.ai/oauth/authorize?state=PRIVATE_URL_CANARY"),
            false,
            Duration::from_secs(10),
            Duration::ZERO,
        );
        wizard.paste("CODE_CANARY#STATE_CANARY");
        let before = screen(&wizard, 80, GlyphSet::Unicode);
        assert!(!before.contains("CANARY"), "{before}");
        let intent = if expired {
            wizard.tick(Duration::from_secs(10)).unwrap()
        } else {
            wizard.cancel()
        };
        assert!(matches!(
            intent,
            Intent::Cancel {
                login: RequestId(1)
            }
        ));
        assert!(wizard.url.is_none());
        assert!(!wizard.ready(RequestId(1), false));
        assert!(!screen(&wizard, 80, GlyphSet::Unicode).contains("CANARY"));
        assert!(wizard.tick(Duration::from_secs(20)).is_none());
    }
}

#[test]
fn loopback_finish_and_saved_not_ready_retry_do_not_start_another_oauth_flow() {
    let (mut wizard, _) = wizard("chatgpt new");
    assert!(matches!(
        wizard.challenged(
            RequestId(1),
            RequestId(1),
            ChallengeKind::BrowserLoopback,
            Secret::new("URL_CANARY"),
            false,
            Duration::from_secs(600),
            Duration::ZERO
        ),
        Some(Intent::Finish {
            response: None,
            login: RequestId(1)
        })
    ));
    wizard.waits_on(RequestId(2));
    assert!(wizard.saved_not_ready(RequestId(2), Some(StoreKind::Keyring), true, false));
    assert!(wizard.url.is_none());
    assert!(matches!(enter(&mut wizard), Some(Intent::Reload { alias }) if alias == "new"));
    wizard.waits_on(RequestId(3));
    assert!(wizard.failure(RequestId(3), AuthFailureKind::Storage, None, "x", None));
    assert_eq!(wizard.phase, Phase::SavedNotReady);
    assert!(matches!(enter(&mut wizard), Some(Intent::Reload { .. })));
    wizard.waits_on(RequestId(4));
    assert!(wizard.ready(RequestId(4), true));
    let shown = screen(&wizard, 120, GlyphSet::Unicode);
    assert!(
        shown.contains("Signed in · ChatGPT subscription · ● new"),
        "{shown}"
    );
    assert!(!shown.contains(CLEANUP_WARNING), "{shown}");
    let lines = wizard.signed_in_lines(Some(5), &Glyphs::of(GlyphSet::Unicode));
    assert_eq!(
        texts(&lines),
        [
            "✓ Signed in · ChatGPT subscription · ● new",
            CLEANUP_WARNING
        ]
    );
    assert_eq!(lines[0].spans[1].look, Look::fg(Token::Mark5).glued());
}

#[test]
fn cards_reuse_question_layout_across_widths_and_glyphs() {
    let mut shots = Vec::new();
    for glyphs in [GlyphSet::Ascii, GlyphSet::Unicode] {
        for width in [40, 80, 120] {
            let (mut provider, _) =
                Wizard::new(route("", &[]).unwrap(), vec![], RequestId(1)).unwrap();
            shots.push(format!(
                "{glyphs:?} {width} provider\n{}",
                screen(&provider, width, glyphs)
            ));
            let (mut gone, _) = Wizard::new(route("", &[]).unwrap(), vec![], RequestId(1)).unwrap();
            gone.note = wisp_render::chrome::account::message_waits(
                &["claude".to_owned()],
                &Glyphs::of(glyphs),
            );
            shots.push(format!("gone\n{}", screen(&gone, width, glyphs)));
            provider.key(Key::plain(KeyCode::Char('1')), Duration::ZERO);
            shots.push(format!("name\n{}", screen(&provider, width, glyphs)));
            enter(&mut provider);
            provider.challenged(
                RequestId(1),
                RequestId(1),
                ChallengeKind::ManualCode,
                Secret::new("https://example.invalid/sign-in"),
                false,
                Duration::from_secs(600),
                Duration::ZERO,
            );
            provider.endpoints = vec![Endpoint {
                alias: "claude-work".into(),
                provider: Some(SubscriptionProvider::Claude),
                working: true,
                mark: crate::app::fixture::mockup_mark("claude-work"),
            }];
            assert!(provider.failure(
                RequestId(1),
                AuthFailureKind::Declined,
                None,
                "this is the same Claude account as claude-work",
                Some("claude-work"),
            ));
            shots.push(format!(
                "same account\n{}",
                screen(&provider, width, glyphs)
            ));
            let known = ["claude", "claude-work"].map(|alias| Endpoint {
                alias: alias.into(),
                provider: Some(SubscriptionProvider::Claude),
                working: alias == "claude",
                mark: crate::app::fixture::mockup_mark(alias),
            });
            let (mut again, _) =
                Wizard::new(route("", &known).unwrap(), known.to_vec(), RequestId(1)).unwrap();
            again.key(Key::plain(KeyCode::Char('1')), Duration::ZERO);
            let shown = screen(&again, width, glyphs);
            let rows: Vec<&str> = shown.lines().filter(|l| l.contains("again to")).collect();
            assert!(
                rows.len() == 2 && rows[0].contains("claude-work"),
                "{shown}"
            );
            shots.push(format!("not working first\n{shown}"));
            shots.extend(sign_out_cards(width, glyphs));
            shots.extend(where_cards(width, glyphs));
        }
    }
    insta::assert_snapshot!(shots.join("\n\n"));
}

#[test]
fn mouse_selection_and_a_restarted_login_keep_identity_and_cancel_visible() {
    let (mut selected, _) = wizard("");
    for (key, row) in [
        (KeyCode::Down, 1),
        (KeyCode::Down, 2),
        (KeyCode::Down, 3),
        (KeyCode::Down, 3),
        (KeyCode::Up, 2),
    ] {
        selected.key(Key::plain(key), Duration::ZERO);
        assert_eq!(selected.pick.focus, Some(row));
    }
    assert!(selected.click(CardHit::Option(0), Duration::ZERO).is_none());
    assert_eq!(selected.provider, Some(SubscriptionProvider::Claude));
    assert!(selected.click(CardHit::Option(1), Duration::ZERO).is_none());
    assert_eq!(selected.alias, "claude");
    assert!(screen(&selected, 40, GlyphSet::Ascii).contains("esc cancel"));
    let (mut key, _) = wizard("");
    assert!(matches!(
        key.click(CardHit::Option(3), Duration::ZERO),
        Some(Intent::AddKey)
    ));
    let (mut raced, started) = wizard("claude new");
    assert!(matches!(started, Some(Intent::Start { .. })));
    raced.start_sent(RequestId(4));
    raced.challenged(
        RequestId(1),
        RequestId(1),
        ChallengeKind::ManualCode,
        Secret::new("STALE_URL_CANARY"),
        false,
        Duration::from_secs(1),
        Duration::ZERO,
    );
    assert!(raced.url.is_none());
    raced.challenged(
        RequestId(4),
        RequestId(4),
        ChallengeKind::ManualCode,
        Secret::new("https://example.invalid"),
        false,
        Duration::from_secs(5),
        Duration::ZERO,
    );
    assert_eq!(raced.phase, Phase::ManualCode);
}

#[test]
fn reserved_alias_hints_round_trip_to_the_named_provider_and_account() {
    for provider in SubscriptionProvider::ALL {
        for alias in [
            SubscriptionProvider::ChatGpt.default_name(),
            SubscriptionProvider::Claude.default_name(),
            SubscriptionProvider::Antigravity.default_name(),
            STATUS,
            "key",
            "work",
        ] {
            let known = vec![Endpoint {
                alias: alias.into(),
                provider: Some(provider),
                working: true,
                mark: crate::app::fixture::mockup_mark(alias),
            }];
            let hint = login_again(Surface::App, alias, reaches(alias));
            let parsed = route(hint.strip_prefix("/login ").unwrap(), &known).unwrap();
            assert_eq!(
                parsed,
                Route::Login {
                    provider: Some(provider),
                    alias: Some(alias.into()),
                    existing_alias: true
                }
            );
            let (_, intent) = Wizard::new(parsed, known, RequestId(1)).unwrap();
            assert!(
                matches!(intent, Some(Intent::Start { selection, .. }) if selection.alias == alias && selection.provider == provider && selection.reauthorize)
            );
        }
    }
    assert_eq!(route(STATUS, &[]).unwrap(), Route::Status);
    assert_eq!(route("status work", &[]).unwrap(), Route::Status);
}

#[test]
fn saved_not_ready_keeps_cleanup_and_saved_details_across_reload_failures() {
    let (mut wizard, _) = wizard("chatgpt new");
    wizard.challenged(
        RequestId(1),
        RequestId(1),
        ChallengeKind::BrowserLoopback,
        Secret::new("URL_CANARY"),
        false,
        Duration::from_secs(600),
        Duration::ZERO,
    );
    wizard.waits_on(RequestId(2));
    assert!(wizard.saved_not_ready(RequestId(2), Some(StoreKind::Keyring), true, true));
    let saved = screen(&wizard, 120, GlyphSet::Unicode);
    assert!(
        saved.contains("Saved: new")
            && saved.contains("keychain storage")
            && saved.contains("new chats start here")
            && saved.contains(CLEANUP_WARNING)
    );
    assert!(!saved.contains("URL_CANARY"));
    for request in [3, 4] {
        assert!(matches!(enter(&mut wizard), Some(Intent::Reload { alias }) if alias == "new"));
        wizard.waits_on(RequestId(request));
        assert!(wizard.failure(
            RequestId(request),
            AuthFailureKind::Storage,
            None,
            "x",
            None
        ));
        assert_eq!(screen(&wizard, 120, GlyphSet::Unicode), saved);
    }
    enter(&mut wizard);
    wizard.waits_on(RequestId(5));
    assert!(wizard.saved_not_ready(RequestId(5), Some(StoreKind::Keyring), true, false));
    assert!(screen(&wizard, 120, GlyphSet::Unicode).contains(CLEANUP_WARNING));
    enter(&mut wizard);
    wizard.waits_on(RequestId(6));
    assert!(wizard.ready(RequestId(6), false));
    assert!(!screen(&wizard, 120, GlyphSet::Unicode).contains(CLEANUP_WARNING));
}

#[test]
fn a_local_or_host_time_out_shows_the_time_out_line_and_drops_the_paste_mistake() {
    let timed_out = mistake_line(Mistake::TimedOut, "Claude");
    let spaced = mistake_line(Mistake::Pasted(CodeProblem::Spaced), "Claude");
    for host in [false, true] {
        let (mut wizard, _) = wizard("claude new");
        wizard.challenged(
            RequestId(1),
            RequestId(1),
            ChallengeKind::ManualCode,
            Secret::new(CLAUDE_URL),
            false,
            Duration::from_secs(10),
            Duration::ZERO,
        );
        wizard.paste("invalid code with spaces");
        assert!(screen(&wizard, 120, GlyphSet::Unicode).contains(&spaced));
        if host {
            assert!(wizard.failure(RequestId(1), AuthFailureKind::Expired, None, "x", None));
            assert!(matches!(wizard.release(), Some(Intent::Cancel { .. })));
        } else {
            assert!(matches!(
                wizard.tick(Duration::from_secs(10)),
                Some(Intent::Cancel { .. })
            ));
        }
        let expired = screen(&wizard, 120, GlyphSet::Unicode);
        assert!(expired.contains(&timed_out), "{expired}");
        assert!(
            expired.contains("Code") && !expired.contains(&spaced),
            "{expired}"
        );
        assert!(wizard.url.is_none());
    }

    for expires in [Duration::ZERO, Duration::MAX] {
        let (mut wizard, _) = self::wizard("claude new");
        wizard.message = "stale validation prompt".into();
        assert!(matches!(
            wizard.challenged(
                RequestId(1),
                RequestId(1),
                ChallengeKind::ManualCode,
                Secret::new(CLAUDE_URL),
                false,
                expires,
                Duration::from_secs(1),
            ),
            Some(Intent::Cancel { .. })
        ));
        let expired = screen(&wizard, 120, GlyphSet::Unicode);
        assert!(expired.contains(&timed_out));
        assert!(!expired.contains("stale validation prompt"));
    }
}

#[test]
fn the_name_step_lowercases_says_why_and_never_numbers() {
    let (mut wizard, _) = wizard("claude");
    wizard.key(Key::plain(KeyCode::Char('2')), Duration::ZERO);
    assert_eq!(wizard.alias, "claude");
    wizard.key(Key::ctrl('u'), Duration::ZERO);
    typed(&mut wizard, "Work");
    assert_eq!(wizard.alias, "work");
    assert!(enter(&mut wizard).is_none());
    assert_eq!(wizard.phase, Phase::Name);
    assert!(screen(&wizard, 80, GlyphSet::Unicode).contains("work is taken."));
    let rows = wizard.render(24, Layout::new(80, GlyphSet::Unicode), None);
    assert!(rows.lines.iter().any(|line| {
        line.spans.iter().any(|span| {
            span.text.contains("work is taken.")
                && span.look == wisp_render::styled::Look::fg(wisp_render::theme::Token::Error)
        })
    }));
    wizard.paste("  Claude1 ");
    assert_eq!(wizard.alias, "claude1");
    wizard.paste("not a name");
    for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
        for width in [40, 80, 120] {
            let shown = screen(&wizard, width, glyphs)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            assert!(shown.contains(ACCOUNT_NAME_RULE.as_str()), "{shown}");
        }
    }
    wizard.paste("env");
    assert!(
        screen(&wizard, 80, GlyphSet::Unicode)
            .contains("That name is reserved for the keys-file account; choose another.")
    );
    wizard.key(Key::ctrl('u'), Duration::ZERO);
    typed(&mut wizard, "env");
    assert!(enter(&mut wizard).is_none());
    assert!(
        screen(&wizard, 80, GlyphSet::Unicode)
            .contains("That name is reserved for the keys-file account; choose another.")
    );
    wizard.key(Key::ctrl('u'), Duration::ZERO);
    typed(&mut wizard, "claude1");
    assert!(
        matches!(enter(&mut wizard), Some(Intent::Start { selection, .. }) if selection.alias == "claude1" && !selection.reauthorize)
    );
    assert_eq!(wizard.alias, "claude1");
}

#[test]
fn the_name_step_at_every_width() {
    let taken = vec![Endpoint {
        alias: "claude".into(),
        provider: Some(SubscriptionProvider::Claude),
        working: true,
        mark: crate::app::fixture::mockup_mark("claude"),
    }];
    for (known, label) in [(vec![], "free"), (taken, "taken")] {
        let (mut wizard, _) =
            Wizard::new(route("claude", &known).unwrap(), known, RequestId(1)).unwrap();
        if label == "taken" {
            wizard.key(Key::plain(KeyCode::Char('2')), Duration::ZERO);
        }
        for (glyphs, set) in [(GlyphSet::Unicode, "unicode"), (GlyphSet::Ascii, "ascii")] {
            for width in [40, 80, 120] {
                insta::assert_snapshot!(
                    format!("name-step-{label}-dark-{set}-{width}"),
                    screen(&wizard, width, glyphs)
                );
            }
        }
    }
}

fn card(words: &str, known: Vec<Endpoint>, opened: bool) -> Wizard {
    let (mut wizard, _) = Wizard::new(route(words, &known).unwrap(), known, RequestId(1)).unwrap();
    let chatgpt = words.contains("chatgpt");
    wizard.challenged(
        RequestId(1),
        RequestId(1),
        if chatgpt {
            ChallengeKind::BrowserLoopback
        } else {
            ChallengeKind::ManualCode
        },
        Secret::new(if chatgpt { CHATGPT_URL } else { CLAUDE_URL }),
        opened,
        Duration::from_secs(600),
        Duration::ZERO,
    );
    wizard
}

fn words_line(wizard: &Wizard, glyphs: GlyphSet) -> String {
    screen(wizard, 80, glyphs)
        .lines()
        .nth(1)
        .and_then(|line| line.trim_start().split_once("   "))
        .map(|(_, words)| words.trim().to_owned())
        .unwrap_or_default()
}

fn native() -> Outcome {
    Outcome::Copied(vec![Destination::Native(
        crate::term::clipboard::Native::Windows,
    )])
}

fn failed() -> Outcome {
    Outcome::Failed("no clipboard was found for this session".into())
}

fn finished(wizard: &mut Wizard, code: &str) {
    wizard.paste(code);
    assert!(matches!(enter(wizard), Some(Intent::Finish { .. })));
    wizard.waits_on(RequestId(2));
}

const CODE: &str = "CODE_CANARY#STATE_CANARY";

#[test]
fn the_words_say_whether_the_browser_opened_and_where_the_link_went() {
    let cases = [
        (
            Outcome::Copied(vec![
                Destination::Tmux { forwards: true },
                Destination::Native(crate::term::clipboard::Native::Wayland),
            ]),
            "link copied",
        ),
        (Outcome::Sent, "link sent to your terminal"),
        (
            Outcome::Copied(vec![Destination::Tmux { forwards: true }]),
            "link sent to your terminal",
        ),
        (
            Outcome::Copied(vec![Destination::Tmux { forwards: false }]),
            "link copied to tmux",
        ),
        (
            Outcome::Failed("wl-copy failed (exit 3)".into()),
            "couldn't copy the link",
        ),
        (failed(), "couldn't copy the link"),
        (Outcome::TooLarge, "couldn't copy the link"),
    ];
    for (opened, first) in [(true, TRIED_BROWSER), (false, NO_BROWSER)] {
        for words in ["claude claude", "chatgpt chatgpt"] {
            let mut wizard = card(words, vec![], opened);
            assert_eq!(words_line(&wizard, GlyphSet::Unicode), first);
            for (outcome, second) in &cases {
                wizard.copied(outcome);
                assert_eq!(
                    words_line(&wizard, GlyphSet::Unicode),
                    format!("{first} \u{b7} {second}")
                );
                assert_eq!(
                    words_line(&wizard, GlyphSet::Ascii),
                    format!("{first} - {second}")
                );
            }
        }
    }
}

#[test]
fn the_link_is_copied_once_per_challenge_and_a_click_opens_or_copies_again() {
    let mut opened = card("claude claude", vec![], true);
    assert_eq!(opened.take_copy().as_deref(), Some(CLAUDE_URL));
    assert_eq!(opened.take_copy(), None);
    assert!(matches!(
        opened.link(),
        Some(Intent::Open {
            login: RequestId(1)
        })
    ));
    assert_eq!(opened.take_copy(), None);
    opened.reopened(RequestId(1), false);
    assert_eq!(words_line(&opened, GlyphSet::Unicode), "no browser here");
    assert_eq!(opened.phase, Phase::ManualCode);
    assert!(opened.url.is_some() && opened.deadline().is_some());
    assert!(opened.link().is_none());
    assert!(opened.take_copy().is_some());
    opened.reopened(RequestId(9), true);
    assert_eq!(words_line(&opened, GlyphSet::Unicode), "no browser here");
    opened.reopened(RequestId(1), true);
    opened.open_failed(RequestId(9));
    assert_eq!(words_line(&opened, GlyphSet::Unicode), TRIED_BROWSER);

    let mut closed = card("claude claude", vec![], false);
    assert!(closed.take_copy().is_some());
    closed.copied(&failed());
    assert_eq!(
        words_line(&closed, GlyphSet::Unicode),
        "no browser here \u{b7} couldn't copy the link"
    );
    assert!(closed.link().is_none());
    assert!(closed.take_copy().is_some());
    assert_eq!(closed.take_copy(), None);
    closed.copied(&failed());
    let way = "no browser here \u{b7} quit, then run wisp login claude";
    assert_eq!(words_line(&closed, GlyphSet::Unicode), way);
    closed.key(Key::ctrl('l'), Duration::ZERO);
    assert!(closed.take_copy().is_some());
    closed.copied(&native());
    assert_eq!(
        words_line(&closed, GlyphSet::Unicode),
        "no browser here \u{b7} link copied"
    );
    closed.link();
    closed.copied(&failed());
    assert_eq!(words_line(&closed, GlyphSet::Unicode), way);
    assert!(closed.tick(Duration::from_secs(600)).is_some());
    assert_eq!(words_line(&closed, GlyphSet::Unicode), "no browser here");
    assert!(matches!(enter(&mut closed), Some(Intent::Start { .. })));
    closed.challenged(
        RequestId(1),
        RequestId(1),
        ChallengeKind::ManualCode,
        Secret::new(CLAUDE_URL),
        false,
        Duration::from_secs(600),
        Duration::ZERO,
    );
    assert_eq!(words_line(&closed, GlyphSet::Unicode), "no browser here");
    assert!(closed.take_copy().is_some());
    closed.cancel();
    assert!(closed.link().is_none());
    assert_eq!(closed.take_copy(), None);

    let restarts: [fn(&mut Wizard) -> Option<Intent>; 3] = [
        Wizard::link,
        |wizard| wizard.key(Key::ctrl('o'), Duration::ZERO),
        enter,
    ];
    for restart in restarts {
        let mut refused = card("claude claude", vec![], true);
        refused.take_copy();
        refused.copied(&native());
        finished(&mut refused, CODE);
        assert!(refused.failure(RequestId(2), AuthFailureKind::Unavailable, None, "x", None));
        assert!(matches!(refused.release(), Some(Intent::Cancel { .. })));
        let ended = screen(&refused, 80, GlyphSet::Unicode);
        assert!(
            ended.contains(&mistake_line(Mistake::Refused, "Claude")),
            "{ended}"
        );
        assert!(ended.contains("claude.ai/oauth/"), "{ended}");
        assert_eq!(words_line(&refused, GlyphSet::Unicode), TRIED_BROWSER);
        refused.paste(CODE);
        typed(&mut refused, "abc");
        assert_eq!(screen(&refused, 80, GlyphSet::Unicode), ended);
        let Some(Intent::Start { selection, .. }) = restart(&mut refused) else {
            panic!("restart missing")
        };
        assert_eq!(selection.alias, "claude");
        assert!(!selection.reauthorize);
        assert_eq!(screen(&refused, 80, GlyphSet::Unicode), ended);
        refused.start_sent(RequestId(5));
        refused.challenged(
            RequestId(5),
            RequestId(5),
            ChallengeKind::ManualCode,
            Secret::new(CLAUDE_URL),
            true,
            Duration::from_secs(600),
            Duration::ZERO,
        );
        assert!(!screen(&refused, 80, GlyphSet::Unicode).contains("accept that code"));
        assert!(refused.take_copy().is_some());
        assert_eq!(refused.take_copy(), None);
    }

    let mut offline = card("claude claude", vec![], true);
    finished(&mut offline, CODE);
    assert!(offline.failure(RequestId(2), AuthFailureKind::Unreachable, None, "x", None));
    assert!(offline.release().is_none());
    assert!(
        screen(&offline, 80, GlyphSet::Unicode)
            .contains(&mistake_line(Mistake::Unreachable, "Claude"))
    );
    let Some(Intent::Finish {
        login: RequestId(1),
        response: Some(response),
    }) = enter(&mut offline)
    else {
        panic!("finish missing")
    };
    assert_eq!(response.expose(), CODE);
}

#[test]
fn the_key_line_lists_only_the_keys_that_work() {
    for words in ["claude claude", "chatgpt chatgpt"] {
        for opened in [false, true] {
            let mut wizard = card(words, vec![], opened);
            for ended in [false, true] {
                let keys = screen(&wizard, 120, GlyphSet::Unicode)
                    .lines()
                    .last()
                    .unwrap_or_default()
                    .to_owned();
                assert_eq!(keys.contains("open"), opened, "{keys}");
                assert_eq!(keys.contains("copy"), !opened || !ended, "{keys}");
                assert!(keys.ends_with("esc cancel"), "{keys}");
                wizard.tick(Duration::from_secs(600));
            }
        }
    }
}

fn labelled(states: &[(&str, &Wizard)], width: usize, glyphs: GlyphSet) -> String {
    states
        .iter()
        .map(|(label, wizard)| format!("{label}\n\n{}", screen(wizard, width, glyphs)))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn states(words: &str) -> Vec<(&'static str, Wizard)> {
    let claude = !words.contains("chatgpt");
    let mut copied = card(words, vec![], true);
    copied.copied(&native());
    let before = card(words, vec![], true);
    let mut closed = card(words, vec![], false);
    closed.copied(&native());
    let mut failing = card(words, vec![], false);
    failing.copied(&failed());
    let mut way = card(words, vec![], false);
    way.link();
    way.copied(&failed());
    let mut refused_open = card(words, vec![], true);
    refused_open.copied(&native());
    refused_open.open_failed(RequestId(1));
    let mut timed_out = card(words, vec![], true);
    timed_out.copied(&native());
    timed_out.tick(Duration::from_secs(600));
    let mut out = vec![
        ("opened, link copied", copied),
        ("before the copy reports", before),
        ("no browser, link copied", closed),
        ("no browser, copy failed", failing),
        ("a press whose copy fails", way),
        ("a refused reopen", refused_open),
        ("timed out", timed_out),
    ];
    if claude {
        let mut mistake = card(words, vec![], true);
        mistake.copied(&native());
        finished(&mut mistake, &format!("ye5b{}k9Qd", "x".repeat(44)));
        mistake.failure(
            RequestId(2),
            AuthFailureKind::CodeRejected,
            Some(CodeProblem::NoState),
            "x",
            None,
        );
        let mut refused = card(words, vec![], true);
        refused.copied(&native());
        finished(&mut refused, CODE);
        refused.failure(RequestId(2), AuthFailureKind::Unavailable, None, "x", None);
        out.push(("a paste mistake", mistake));
        out.push(("a refused code", refused));
    }
    out
}

#[test]
fn the_sign_in_card_with_its_words_and_masked_code_at_every_width() {
    let other = vec![Endpoint {
        alias: "claude".into(),
        provider: Some(SubscriptionProvider::Claude),
        working: true,
        mark: crate::app::fixture::mockup_mark("claude"),
    }];
    for (glyphs, set) in [(GlyphSet::Unicode, "unicode"), (GlyphSet::Ascii, "ascii")] {
        for width in [40, 80, 120] {
            let claude = states("claude claude");
            let chatgpt = states("chatgpt chatgpt");
            let mut another = card("claude claude-work", other.clone(), true);
            another.copied(&native());
            let mut done = card("claude claude", vec![], true);
            finished(&mut done, CODE);
            done.ready(RequestId(2), false);
            done.offer_use("direct");
            let mut shown: Vec<(&str, &Wizard)> = vec![];
            shown.extend(claude.iter().map(|(label, wizard)| (*label, wizard)));
            shown.push(("adding another Claude account", &another));
            shown.push(("done, chat elsewhere", &done));
            shown.extend(chatgpt.iter().map(|(label, wizard)| (*label, wizard)));
            let text = labelled(&shown, width, glyphs);
            assert!(!text.contains("CANARY") && !text.contains("EXAMPLE"));
            insta::assert_snapshot!(format!("link-words-dark-{set}-{width}"), text);
        }
    }
}

fn error_rows(rows: &CardRows) -> usize {
    rows.lines
        .iter()
        .filter(|line| {
            line.spans.iter().any(|span| {
                span.look == wisp_render::styled::Look::fg(wisp_render::theme::Token::Error)
            })
        })
        .count()
}

#[test]
fn no_row_of_the_browser_step_moves_and_a_mistake_only_lifts_the_rows_above_it() {
    for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
        for width in 40..=120 {
            let layout = Layout::new(width, glyphs);
            for words in ["claude claude", "chatgpt chatgpt"] {
                let shown = states(words);
                let (_, first) = &shown[0];
                let base = first.render(24, layout, None);
                for (label, wizard) in &shown {
                    let rows = wizard.render(24, layout, None);
                    let lifted = error_rows(&rows);
                    let keep = base.lines.len() - 1;
                    assert_eq!(
                        rows.lines.len(),
                        base.lines.len() + lifted,
                        "{label} {width}"
                    );
                    assert_eq!(rows.hits[..keep], base.hits[..keep], "{label} {width}");
                    assert_eq!(rows.lines[0], base.lines[0], "{label} {width}");
                }
                let mut ended = card(words, vec![], true);
                ended.tick(Duration::from_secs(600));
                let before = ended.render(24, layout, None);
                ended.link();
                assert_eq!(
                    ended.render(24, layout, None),
                    before,
                    "restart {words} {width}"
                );
            }
        }
    }
    let mistakes = [
        Mistake::Pasted(CodeProblem::Empty),
        Mistake::Pasted(CodeProblem::NoState),
        Mistake::Pasted(CodeProblem::Repeated),
        Mistake::Pasted(CodeProblem::Spaced),
        Mistake::Pasted(CodeProblem::TooLong),
        Mistake::Earlier,
        Mistake::Refused,
        Mistake::Unreachable,
        Mistake::TimedOut,
    ];
    for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
        for (words, provider, lines) in [
            ("claude claude", "Claude", &mistakes[..]),
            ("chatgpt chatgpt", "ChatGPT", &[Mistake::TimedOut][..]),
        ] {
            for mistake in lines {
                let mut wizard = card(words, vec![], true);
                wizard.message = mistake_line(*mistake, provider);
                let rows = wizard.render(24, Layout::new(80, glyphs), None);
                let texts: Vec<String> = rows
                    .lines
                    .iter()
                    .filter(|line| {
                        line.spans.iter().any(|span| {
                            span.look
                                == wisp_render::styled::Look::fg(wisp_render::theme::Token::Error)
                        })
                    })
                    .map(line_text)
                    .collect();
                assert!(
                    texts.len() == 1
                        || texts
                            .iter()
                            .all(|row: &String| row.split_whitespace().count() > 1),
                    "{mistake:?}: {texts:?}"
                );
            }
        }
    }
}

fn where_cards(width: usize, glyphs: GlyphSet) -> Vec<String> {
    use crate::app::fixture::mockup_mark;
    use crate::app::logout::{SignOut, WayRow};
    use wisp_render::chrome::account::{RAN_ON_IT, Said, WHERE_CONTINUE, WHERE_START, Who};
    let layout = Layout::new(width, glyphs);
    let set = Glyphs::of(glyphs);
    let live = |name: &str| Who::Account(name.to_owned(), mockup_mark(name));
    let gone = |name: &str| Who::Account(name.to_owned(), None);
    let line = |text: &str| text.to_owned();
    let behind = "Earlier reasoning won't carry over, and reviewer stays behind.";
    let to_api = "This chat moves from your subscription to the API.";
    let way = |chat: &str, kind: &str, billing: &str, charges: bool| WayRow {
        mark: mockup_mark(chat),
        chat: chat.to_owned(),
        kind: kind.to_owned(),
        billing: billing.to_owned(),
        model: "opus-5-5 high".to_owned(),
        price: "cache rebuild ~$0.41".to_owned(),
        consequence: if charges {
            vec![line(to_api), line(behind)]
        } else {
            vec![line(behind)]
        },
        charges,
        ..WayRow::default()
    };
    let real = vec![
        way(
            "claude-subscription",
            "Claude subscription",
            "subscription",
            false,
        ),
        way("claude-work", "Claude subscription", "subscription", false),
        way("claude-api", "Claude API", "API", true),
    ];
    let continuing = |name: &str| {
        let lead = Said::RanOn(gone(name), None);
        (WHERE_CONTINUE.to_owned(), lead.marked(&set), lead.mark())
    };
    let mut shots = vec![(
        "T§3b",
        SignOut::gone(
            Vec::new(),
            continuing("chatgpt"),
            real,
            wisp_render::transcript::copy::esc(wisp_render::transcript::copy::QUIT_WORD),
        )
        .render(24, layout),
    )];
    let (title, prompt) = wisp_render::chrome::account::off_title("gemini", false);
    let off_ways = ["claude-subscription", "claude-work"].map(|name| WayRow {
        consequence: vec![line(
            "Earlier reasoning won't carry over, and worker stays behind.",
        )],
        ..way(name, "Claude subscription", "subscription", false)
    });
    shots.push((
        "provider going off",
        SignOut::gone(Vec::new(), (title, prompt, None), off_ways.into(), cancel())
            .render(24, layout),
    ));
    let api_only = vec![
        WayRow {
            mark: mockup_mark("chatgpt"),
            chat: "chatgpt".to_owned(),
            kind: "ChatGPT subscription".to_owned(),
            billing: "subscription".to_owned(),
            model: "gpt-6.1-sol high".to_owned(),
            price: "cache rebuild ~$0.38".to_owned(),
            consequence: vec![line(
                "Earlier reasoning won't carry over, and coder stays behind.",
            )],
            ..WayRow::default()
        },
        WayRow {
            mark: mockup_mark("claude-api"),
            chat: "claude-api".to_owned(),
            follow: vec![line("coder")],
            kind: "Claude API".to_owned(),
            billing: "API".to_owned(),
            model: "opus-5-5 high".to_owned(),
            price: "cache rebuild ~$0.41 for this chat".to_owned(),
            consequence: vec![line(
                "This chat and coder move from your subscription to the API.",
            )],
            charges: true,
            ..WayRow::default()
        },
    ];
    let mut card = SignOut::gone(
        Vec::new(),
        continuing("claude-subscription"),
        api_only,
        wisp_render::transcript::copy::esc(wisp_render::transcript::copy::BACK_WORD),
    );
    shots.push(("T§3c, from the picker", card.render(24, layout)));
    card.drawn = true;
    card.key(Key::plain(KeyCode::Down));
    shots.push(("T§3c, on the API", card.render(24, layout)));
    let start = |chat: &str, kind: &str, billing: &str, model: &str, charges: bool| WayRow {
        mark: mockup_mark(chat),
        chat: chat.to_owned(),
        kind: kind.to_owned(),
        billing: billing.to_owned(),
        model: model.to_owned(),
        consequence: if charges {
            vec![line(to_api)]
        } else {
            Vec::new()
        },
        charges,
        ..WayRow::default()
    };
    let starting = |lead: Said| (lead.marked(&set), WHERE_START.to_owned(), lead.mark());
    let expired = vec![
        WayRow {
            mark: mockup_mark("claude-subscription"),
            chat: "claude-subscription".to_owned(),
            again: true,
            ..WayRow::default()
        },
        start(
            "claude-work",
            "Claude subscription",
            "subscription",
            "opus-5-5 high",
            false,
        ),
        start(
            "chatgpt",
            "ChatGPT subscription",
            "subscription",
            "gpt-6.1-sol high",
            false,
        ),
        start("claude-api", "Claude API", "API", "opus-5-5 high", true),
    ];
    let mut card = SignOut::gone(
        Vec::new(),
        starting(Said::SignInExpired(live("claude-subscription"))),
        expired.clone(),
        wisp_render::transcript::copy::esc(wisp_render::transcript::copy::QUIT_WORD),
    );
    shots.push(("T§2a", card.render(24, layout)));
    card.drawn = true;
    card.key(Key::plain(KeyCode::Char('4')));
    shots.push(("T§2a, on the API", card.render(24, layout)));
    let api_only = vec![
        start("claude-api", "Claude API", "API", "opus-5-5 high", true),
        start("work-api", "Claude API", "API", "opus-5-5 high", true),
    ];
    let card = SignOut::gone(
        Vec::new(),
        starting(Said::KeyMissing(live("claude-token"), None)),
        api_only,
        wisp_render::transcript::copy::esc(wisp_render::transcript::copy::QUIT_WORD),
    );
    shots.push(("T§2b, asks first", card.render(24, layout)));
    let priced = |row: WayRow, price: &str, consequence: &[&str]| WayRow {
        price: price.to_owned(),
        consequence: consequence.iter().map(|text| line(text)).collect(),
        ..row
    };
    let rows = vec![
        expired[0].clone(),
        priced(expired[1].clone(), "cache rebuild ~$0.41", &[]),
        priced(
            expired[2].clone(),
            "cache rebuild ~$0.38",
            &["Earlier reasoning won't carry over."],
        ),
        priced(expired[3].clone(), "cache rebuild ~$0.41", &[to_api]),
    ];
    let expired = Said::SignInExpired(live("claude-subscription"));
    let heading = (expired.marked(&set), RAN_ON_IT.to_owned(), expired.mark());
    let card = SignOut::gone(
        Vec::new(),
        heading,
        rows,
        wisp_render::transcript::copy::esc(wisp_render::transcript::copy::QUIT_WORD),
    );
    shots.push(("T§3e", card.render(24, layout)));
    shots
        .into_iter()
        .map(|(name, rows)| format!("where {name}\n{}", shown(&rows)))
        .collect()
}

fn sign_out_cards(width: usize, glyphs: GlyphSet) -> Vec<String> {
    use crate::app::fixture::mockup_mark;
    use crate::app::logout::{self, Choice, SignOut, WayRow};
    use wisp_protocol::subscription_auth::PresetGone;
    use wisp_render::chrome::account::{AccountState, Asked, Moving, cant_move};
    let set = Glyphs::of(glyphs);
    let sep = format!(" {} ", set.separator);
    let layout = Layout::new(width, glyphs);
    let names = |list: &[&str]| list.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
    let named = |list: &[&str]| {
        let named = list.iter().map(|n| ((*n).to_owned(), mockup_mark(n)));
        named.collect::<Vec<_>>()
    };
    let asked = |list: &[&str]| Asked {
        names: named(list),
        ..Asked::default()
    };
    let choice = |name: &str, kind: &str, notes: &[&str]| Choice {
        name: name.to_owned(),
        mark: mockup_mark(name),
        details: vec![
            std::iter::once(kind)
                .chain(notes.iter().copied())
                .collect::<Vec<_>>()
                .join(&sep),
            std::iter::once(kind)
                .chain(notes.iter().copied().take(1))
                .collect::<Vec<_>>()
                .join(&sep),
        ],
    };
    let ways = vec![
        WayRow {
            chat: "chatgpt".to_owned(),
            mark: mockup_mark("chatgpt"),
            follow: Vec::new(),
            kind: "ChatGPT subscription".to_owned(),
            billing: "subscription".to_owned(),
            model: "gpt-6.1-sol high".to_owned(),
            price: "cache rebuild ~$0.38".to_owned(),
            rebuild: None,
            movers: Vec::new(),
            consequence: vec![
                "Earlier reasoning won't carry over, and coder stays behind.".to_owned(),
            ],
            charges: false,
            again: false,
        },
        WayRow {
            chat: "claude-api".to_owned(),
            mark: mockup_mark("claude-api"),
            follow: names(&["coder"]),
            kind: "Claude API".to_owned(),
            billing: "API".to_owned(),
            model: "opus-5-5 high".to_owned(),
            price: "cache rebuild ~$0.41 and ~$0.08".to_owned(),
            rebuild: None,
            movers: Vec::new(),
            consequence: vec![
                "This chat and coder move from your subscription to the API.".to_owned(),
            ],
            charges: true,
            again: false,
        },
    ];
    let mut list = SignOut::which(vec![
        choice(
            "claude-subscription",
            "Claude subscription",
            &["this chat", "new chats start here"],
        ),
        choice("claude-work", "Claude subscription", &["coder uses it"]),
        choice("chatgpt", "ChatGPT subscription", &["reviewer uses it"]),
        choice(
            "claude-api",
            "Claude API",
            &["from ~/.config/wisp/.env", "scout uses it"],
        ),
    ]);
    let mut shots = vec![("which", list.render(24, layout))];
    list.drawn = true;
    list.key(Key::plain(KeyCode::Down));
    shots.push(("which, after a down", list.render(24, layout)));
    let yes = |asked: Asked| SignOut::yes(asked, Vec::new(), None).render(24, layout);
    shots.push((
        "a stored key",
        yes(Asked {
            keys: named(&["work-api"]),
            ..asked(&["work-api"])
        }),
    ));
    let preset = |account: &str, runs: bool| PresetGone {
        preset: "coder".to_owned(),
        account: account.to_owned(),
        runs,
    };
    shots.push((
        "a stored key, with a preset",
        yes(Asked {
            keys: named(&["work-api"]),
            presets: vec![preset("work-api", true)],
            ..asked(&["work-api"])
        }),
    ));
    for list in [&["claude"][..], &["claude", "chatgpt"]] {
        shots.push((
            "nothing left",
            yes(Asked {
                nothing_left: true,
                ..asked(list)
            }),
        ));
    }
    let rows = |rows: &[(&str, Option<&str>)]| {
        let rows = rows
            .iter()
            .map(|(n, a)| ((*n).to_owned(), a.map(str::to_owned)));
        rows.collect::<Vec<_>>()
    };
    let to_api = |movers: &[(&str, Option<&str>)]| Moving {
        to: ("claude-api".to_owned(), mockup_mark("claude-api")),
        state: AccountState::Api,
        rows: rows(movers),
    };
    let behind = |agent: &str| vec![(agent.to_owned(), cant_move("Claude"))];
    let four = ["claude-subscription", "claude-work", "chatgpt", "work-api"];
    let both = [("this chat", Some("~$0.41")), ("coder", Some("~$0.08"))];
    let all = Asked {
        keys: named(&["work-api"]),
        moving: Some(to_api(&both)),
        behind: behind("reviewer"),
        ..asked(&four)
    };
    let structured = [
        (
            "all, nothing left",
            Asked {
                keys: named(&["work-api"]),
                nothing_left: true,
                ..asked(&four)
            },
        ),
        ("all", all.clone()),
        (
            "all, with a preset",
            Asked {
                presets: vec![preset("claude-work", false)],
                ..all.clone()
            },
        ),
        (
            "all, scout unpriced",
            Asked {
                moving: Some(to_api(&[both[0], both[1], ("scout", None)])),
                ..all.clone()
            },
        ),
        (
            "all, a long name",
            Asked {
                behind: behind("code-reviewer"),
                ..all.clone()
            },
        ),
        (
            "a stored key, coder moving",
            Asked {
                keys: named(&["work-api"]),
                moving: Some(Moving {
                    to: (
                        "claude-subscription".to_owned(),
                        mockup_mark("claude-subscription"),
                    ),
                    state: AccountState::Subscription,
                    rows: rows(&[both[1]]),
                }),
                ..asked(&["work-api"])
            },
        ),
    ];
    for (name, asked) in structured {
        let way = (name == "all").then_some(1);
        let ways = if way.is_some() {
            ways.clone()
        } else {
            Vec::new()
        };
        shots.push((name, SignOut::yes(asked, ways, way).render(24, layout)));
    }
    let ways_for_gone = ways.clone();
    let typed = SignOut::place(named(&["claude-subscription"]), ways.clone(), true);
    shots.push(("where, typed", typed.render(24, layout)));
    let mut after = SignOut::yes(asked(&["claude-subscription"]), ways, None);
    after.drawn = true;
    after.key(Key::plain(KeyCode::Char('1')));
    after.key(Key::plain(KeyCode::Enter));
    shots.push(("where, after sign out", after.render(24, layout)));
    after.drawn = true;
    after.key(Key::plain(KeyCode::Down));
    after.key(Key::plain(KeyCode::Down));
    shots.push(("where, chosen", after.render(24, layout)));
    let gone_ways = vec![
        WayRow {
            follow: Vec::new(),
            consequence: vec!["Earlier reasoning won't carry over.".to_owned()],
            ..ways_for_gone[0].clone()
        },
        WayRow {
            follow: Vec::new(),
            price: "cache rebuild ~$0.41".to_owned(),
            consequence: vec!["This chat moves from your subscription to the API.".to_owned()],
            ..ways_for_gone[1].clone()
        },
    ];
    let gone_names = names(&["claude-subscription", "claude-work"]);
    let (title, prompt) =
        wisp_render::chrome::account::gone_title(&gone_names, &Glyphs::of(layout.glyphs));
    let heading = (title, prompt, None);
    let mut gone = SignOut::gone(gone_names, heading, gone_ways, super::cancel());
    shots.push(("gone elsewhere", gone.render(24, layout)));
    gone.drawn = true;
    assert_eq!(gone.key(Key::plain(KeyCode::Char('2'))), None);
    assert_eq!(gone.click(CardHit::Option(1)), None);
    shots.push(("gone elsewhere, on the API", gone.render(24, layout)));
    assert!(matches!(
        gone.key(Key::plain(KeyCode::Enter)),
        Some(logout::Intent::Send { gone: true, .. })
    ));
    shots
        .into_iter()
        .map(|(name, rows)| format!("sign-out {name}\n{}", shown(&rows)))
        .collect()
}

#[test]
fn antigravity_browser_loopback_handles_copy_reopen_and_completion() {
    let (mut wizard, intent) = wizard("antigravity new");
    assert!(matches!(
        intent,
        Some(Intent::Start { ref selection, .. })
            if selection.provider == SubscriptionProvider::Antigravity
                && selection.alias == "new"
                && !selection.reauthorize
    ));
    let finish = wizard.challenged(
        RequestId(1),
        RequestId(1),
        ChallengeKind::BrowserLoopback,
        Secret::new("https://accounts.google.com/o/oauth2/v2/auth?state=EXAMPLE"),
        true,
        Duration::from_secs(600),
        Duration::ZERO,
    );
    assert!(matches!(
        finish,
        Some(Intent::Finish {
            login: RequestId(1),
            response: None,
        })
    ));
    assert!(wizard.take_copy().is_some());
    assert_eq!(wizard.phase, Phase::Finishing);
    let rendered = screen(&wizard, 80, GlyphSet::Unicode);
    assert!(rendered.contains("Sign in to Antigravity"), "{rendered}");
    assert!(!rendered.contains("Callback URL"), "{rendered}");
    assert!(rendered.contains("c copy"), "{rendered}");
    wizard.key(Key::plain(KeyCode::Char('c')), Duration::ZERO);
    assert!(wizard.take_copy().is_some());
    assert!(matches!(
        enter(&mut wizard),
        Some(Intent::Open {
            login: RequestId(1)
        })
    ));
    wizard.waits_on(RequestId(2));
    assert!(wizard.ready(RequestId(2), false));
    assert_eq!(wizard.phase, Phase::Done);
}

#[test]
fn antigravity_headless_manual_code_labels_callback_url_and_retries_paste() {
    let (mut wizard, _) = wizard("antigravity headless");
    assert!(
        wizard
            .challenged(
                RequestId(1),
                RequestId(1),
                ChallengeKind::ManualCode,
                Secret::new("https://accounts.google.com/o/oauth2/v2/auth?state=EXAMPLE"),
                false,
                Duration::from_secs(600),
                Duration::ZERO,
            )
            .is_none()
    );
    assert_eq!(wizard.phase, Phase::ManualCode);
    let rendered = screen(&wizard, 80, GlyphSet::Unicode);
    assert!(rendered.contains("Callback URL"), "{rendered}");
    assert!(!rendered.contains("Code  "), "{rendered}");
    assert!(rendered.contains("ctrl-l copy"), "{rendered}");

    wizard.paste("http://127.0.0.1:51121/oauth-callback?code=bad&state=EXAMPLE");
    let finish = enter(&mut wizard);
    assert!(matches!(
        finish,
        Some(Intent::Finish {
            login: RequestId(1),
            response: Some(_),
        })
    ));
    wizard.waits_on(RequestId(2));
    assert!(wizard.failure(
        RequestId(2),
        AuthFailureKind::CodeRejected,
        Some(CodeProblem::NoState),
        "bad url",
        None,
    ));
    assert_eq!(wizard.phase, Phase::ManualCode);
    assert!(screen(&wizard, 80, GlyphSet::Unicode).contains("Callback URL"));
}
