use super::*;
use crate::testing::trimmed;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use wisp_protocol::plan_usage::{PlanUsage, UsageScope, UsageSource, UsageWindow};
use wisp_render::chrome::context::MeterLevel::{Calm, Over, Warning};
use wisp_render::styled::Look;
use wisp_render::theme::Token;
use wisp_render::theme::glyphs::GlyphSet;

const NOW: i64 = 1_893_508_000;

fn account(name: &str, kind: AccountKind, held: Held, health: Health) -> Account {
    Account {
        mark: wisp_render::chrome::account::mockup_mark(name),
        name: name.to_owned(),
        kind,
        held,
        health,
        host: None,
        off: None,
    }
}

fn chatgpt(name: &str) -> Account {
    account(
        name,
        AccountKind::ChatGpt,
        Held::Subscription(SubscriptionProvider::ChatGpt),
        Health::SignedIn,
    )
}

fn usage(five: Option<f64>, week: Option<f64>) -> Option<UsageState> {
    let window = |key: &str, duration: u64, used: f64| UsageWindow {
        key: key.to_owned(),
        label: key.to_owned(),
        duration_seconds: Some(duration),
        scope: UsageScope::All,
        used_percent: Some(used.try_into().unwrap()),
        resets_at: None,
        observed_at: 0,
        source: UsageSource::UsageEndpoint,
    };
    let windows = [("five", 18_000, five), ("week", 604_800, week)]
        .into_iter()
        .filter_map(|(key, duration, used)| Some(window(key, duration, used?)))
        .collect();
    Some(UsageState {
        snapshot: Some(PlanUsage::new(None, windows).unwrap()),
        loading: false,
        stale: false,
        error: None,
    })
}

fn failed(error: PlanUsageError) -> Option<UsageState> {
    Some(UsageState {
        snapshot: None,
        loading: false,
        stale: true,
        error: Some(error),
    })
}

fn looked(pairs: &[(&str, Option<UsageState>)]) -> Looked {
    pairs
        .iter()
        .map(|(name, state)| ((*name).to_owned(), state.clone()))
        .collect()
}

fn listed(list: Vec<Account>, default: Option<&str>) -> Accounts {
    Accounts {
        list,
        default: default.map(str::to_owned),
        ..Accounts::default()
    }
}

fn mockups() -> Vec<(Accounts, Looked)> {
    let mut gateway = account(
        "team-gateway",
        AccountKind::Gateway,
        Held::Stored,
        Health::SignedIn,
    );
    gateway.host = Some("gateway.example.com".to_owned());
    let mut missing = account(
        "work-api",
        AccountKind::ClaudeApi,
        Held::Stored,
        Health::KeyMissing,
    );
    missing.host = Some("api.anthropic.com".to_owned());
    let openai = account(
        "openai-api",
        AccountKind::OpenAiApi,
        Held::Stored,
        Health::SignedIn,
    );
    vec![
        (
            listed(
                vec![
                    claude("claude1", Health::SignedIn),
                    claude("claude2", Health::SignedIn),
                    missing,
                    gateway,
                ],
                Some("claude1"),
            ),
            looked(&[
                ("claude1", usage(Some(42.0), Some(18.0))),
                ("claude2", usage(Some(87.0), Some(64.0))),
            ]),
        ),
        (
            listed(
                vec![
                    chatgpt("chatgpt"),
                    claude("claude1", Health::SignedIn),
                    claude("claude2", Health::Expired),
                    openai,
                ],
                Some("openai-api"),
            ),
            looked(&[
                ("chatgpt", usage(Some(12.0), Some(31.0))),
                ("claude1", usage(Some(42.0), Some(18.0))),
                ("claude2", failed(PlanUsageError::AuthenticationRequired)),
            ]),
        ),
        (
            listed(
                vec![
                    chatgpt("chatgpt"),
                    claude("claude1", Health::SignedIn),
                    claude("claude2", Health::SignedIn),
                ],
                Some("claude1"),
            ),
            looked(&[
                ("chatgpt", usage(None, Some(31.0))),
                ("claude1", None),
                ("claude2", usage(Some(100.0), Some(64.0))),
            ]),
        ),
        (
            listed(
                vec![
                    claude("claude-personal", Health::SignedIn),
                    claude("claude1", Health::SignedIn),
                ],
                Some("claude1"),
            ),
            looked(&[
                ("claude-personal", usage(Some(87.5), Some(64.0))),
                ("claude1", usage(Some(42.0), Some(18.0))),
            ]),
        ),
    ]
}

fn text(lines: &[StyledLine]) -> String {
    trimmed(lines)
        .iter()
        .map(|row| format!("{row}\n"))
        .collect()
}

fn shown(accounts: &Accounts, only: Option<&str>, found: &Looked, room: usize) -> String {
    text(&panel(
        accounts,
        only,
        found,
        room,
        &Glyphs::of(GlyphSet::Unicode),
        true,
        NOW,
    ))
}

fn claude(name: &str, health: Health) -> Account {
    account(
        name,
        AccountKind::ClaudeSubscription,
        Held::Subscription(SubscriptionProvider::Claude),
        health,
    )
}

fn example() -> Accounts {
    Accounts {
        list: vec![
            claude("claude-subscription", Health::SignedIn),
            claude("claude-work", Health::Expired),
            chatgpt("chatgpt"),
            account(
                "claude-api",
                AccountKind::ClaudeApi,
                Held::Variable {
                    var: "ANTHROPIC_API_KEY".to_owned(),
                    keys_file: true,
                },
                Health::SignedIn,
            ),
            account(
                "claude-api-2",
                AccountKind::ClaudeApi,
                Held::Stored,
                Health::KeyMissing,
            ),
            account(
                "claude-token",
                AccountKind::ClaudeSubscription,
                Held::SetupToken,
                Health::SignedIn,
            ),
        ],
        default: Some("claude-subscription".to_owned()),
        keys_file: "~/.config/wisp/.env".to_owned(),
        shadowed: None,
    }
}

#[test]
fn status_lists_every_account_where_new_chats_start_and_each_next_step() {
    for (set, set_name) in [(GlyphSet::Unicode, "Unicode"), (GlyphSet::Ascii, "Ascii")] {
        let glyphs = Glyphs::of(set);
        for width in [40, 76, 77, 80, 120] {
            let mut all = String::new();
            for (accounts, found) in mockups() {
                all.push_str(&text(&panel(
                    &accounts, None, &found, width, &glyphs, true, NOW,
                )));
            }
            let (accounts, found) = &mockups()[1];
            all.push_str(&text(&panel(
                accounts,
                Some("claude2"),
                found,
                width,
                &glyphs,
                true,
                NOW,
            )));
            all.push_str(&text(&panel(
                &Accounts::default(),
                None,
                &Looked::new(),
                width,
                &glyphs,
                true,
                NOW,
            )));
            for line in all.lines() {
                assert!(line.chars().count() <= width, "{line}");
                assert!(set == GlyphSet::Unicode || line.is_ascii(), "{line}");
            }
            assert_eq!(
                wisp_render::chrome::account::vocabulary_slips(&all),
                Vec::<&str>::new()
            );
            insta::assert_snapshot!(format!("status_{width}_Dark_{set_name}"), all);
        }
    }
    let (accounts, _) = &mockups()[0];
    for width in [40, 80] {
        let read = shown(accounts, None, &mockups()[0].1, width);
        for found in [
            looked(&[("claude1", None), ("claude2", None)]),
            looked(&[
                (
                    "claude1",
                    usage(Some(42.0), Some(18.0)).map(|state| UsageState {
                        stale: true,
                        ..state
                    }),
                ),
                ("claude2", usage(Some(10.0), Some(5.0))),
            ]),
            looked(&[
                ("claude1", failed(PlanUsageError::Network)),
                (
                    "claude2",
                    failed(PlanUsageError::Throttled { retry_at: None }),
                ),
            ]),
        ] {
            let other = shown(accounts, None, &found, width);
            assert!(other.contains(wisp_render::chrome::accounts::USAGE_UNAVAILABLE));
            assert_eq!(read.lines().count(), other.lines().count(), "{other}");
            for (a, b) in read.lines().zip(other.lines()) {
                assert_eq!(a.chars().count(), b.chars().count(), "{b}");
                let x = |line: &str| line.find("starts").map(|at| line[..at].chars().count());
                assert_eq!(x(a), x(b), "{b}");
            }
        }
    }
    let rounded = listed(vec![claude("claude1", Health::SignedIn)], None);
    for (value, shown, level, model_only) in [
        (37.5, "37%", Calm, false),
        (99.6, "99%", Over, false),
        (99.4, "99%", Over, false),
        (0.4, "0%", Calm, false),
        (100.0, "100%", Over, false),
        (74.6, "74%", Calm, false),
        (89.6, "89%", Warning, false),
        (99.6, "99%", Over, true),
    ] {
        let mut state = usage(Some(value), Some(89.6)).unwrap();
        if model_only {
            let windows = &mut state.snapshot.as_mut().unwrap().windows;
            windows.truncate(1);
            windows[0].duration_seconds = None;
            windows[0].scope = UsageScope::Model("claude-model".into());
        }
        let lines = panel(
            &rounded,
            None,
            &looked(&[("claude1", Some(state))]),
            80,
            &Glyphs::of(GlyphSet::Unicode),
            true,
            NOW,
        );
        let spans: Vec<_> = lines.iter().flat_map(|line| &line.spans).collect();
        let span = |text: &str| {
            spans
                .iter()
                .find(|span| span.text.trim() == text)
                .unwrap()
                .look
        };
        assert_eq!(span(shown), level.look(), "{value}");
        if !model_only {
            assert_eq!(span("89%"), Warning.look());
            assert_eq!(span(&"\u{28ff}".repeat(8)), Warning.look());
            assert_eq!(span(&"\u{28c0}".repeat(2)), Look::fg(Token::TextFaint));
        }
    }
    let (_, found) = &mockups()[1];
    let unexpired = listed(vec![claude("claude2", Health::SignedIn)], None);
    let unavailable = shown(&unexpired, None, found, 80);
    assert!(unavailable.contains("usage unavailable"), "{unavailable}");
    assert!(!unavailable.contains("sign-in expired"), "{unavailable}");
    let mut state = usage(Some(42.0), Some(18.0)).unwrap();
    for (window, left) in state
        .snapshot
        .as_mut()
        .unwrap()
        .windows
        .iter_mut()
        .zip([7800, 367200])
    {
        window.resets_at = Some(NOW + left);
    }
    let found = looked(&[("claude1", Some(state))]);
    for columns in [40, 80, 120] {
        for (now, reset, weekly) in [(NOW, "2h10m", "4d06h"), (NOW + 60, "2h09m", "4d05h")] {
            let shown = text(&panel(
                &rounded,
                None,
                &found,
                columns,
                &Glyphs::of(GlyphSet::Unicode),
                true,
                now,
            ));
            assert!(shown.contains(reset), "{shown}");
            assert!(shown.contains(weekly), "{shown}");
            assert!(
                shown.lines().all(|line| line.chars().count() <= columns),
                "{shown}"
            );
        }
    }
}

#[test]
fn a_name_that_is_no_account_lists_the_accounts() {
    assert_eq!(
        no_such_account("work", &example()),
        "there's no account called work; yours are claude-subscription, claude-work, chatgpt, claude-api, claude-api-2 and claude-token"
    );
    assert_eq!(
        no_such_account("work", &Accounts::default()),
        "there's no account called work; run wisp login to sign in"
    );
}

#[test]
fn a_long_name_takes_its_own_line_with_its_cells_under_it() {
    let long = "claude-a-very-long-account-name-";
    assert_eq!(long.len(), 32);
    let (short, found) = &mockups()[3];
    let mut accounts = short.clone();
    accounts.list[0].name = long.to_owned();
    let mut found = found.clone();
    found.insert(long.to_owned(), found["claude-personal"].clone());
    let without = listed(vec![claude("claude1", Health::SignedIn)], Some("claude1"));
    for width in [40, 80] {
        let text = shown(&accounts, None, &found, width);
        let lines: Vec<&str> = text.lines().collect();
        let at = lines.iter().position(|line| line.contains(long)).unwrap();
        assert_eq!(
            lines[at].trim_matches(['\u{2502}', ' ']),
            format!("\u{25cf} {long}"),
            "{text}"
        );
        let under = lines[at + 1];
        assert!(
            under.starts_with(&format!("\u{2502}{}", " ".repeat(15))),
            "{text}"
        );
        assert!(under.contains("87%"), "{text}");
        let alone = shown(&without, None, &found, width);
        let mine = |text: &str| -> Vec<String> {
            text.lines()
                .skip_while(|line| !line.contains("claude1"))
                .take_while(|line| !line.starts_with('\u{2570}'))
                .map(str::to_owned)
                .collect()
        };
        assert_eq!(mine(&text), mine(&alone), "{text}");
        for line in &lines {
            assert!(line.chars().count() <= width, "{line}");
        }
    }
}

#[test]
fn a_locked_key_an_unset_variable_and_a_gateway_each_read_true() {
    let mut accounts = Accounts {
        list: vec![
            account(
                "locked",
                AccountKind::ClaudeApi,
                Held::Stored,
                Health::Unreadable,
            ),
            account(
                "shell",
                AccountKind::ClaudeApi,
                Held::Variable {
                    var: "WORK_KEY".to_owned(),
                    keys_file: false,
                },
                Health::Unset,
            ),
            account("gw", AccountKind::Gateway, Held::Stored, Health::SignedIn),
        ],
        ..Accounts::default()
    };
    accounts.list[2].host = Some("gw.example.com:8443".to_owned());
    let text = shown(&accounts, None, &Looked::new(), 80);
    assert!(text.contains("can't be read now"), "{text}");
    assert!(text.contains("WORK_KEY not set"), "{text}");
    assert!(text.contains("gw.example.com:8443"), "{text}");
    assert!(!text.contains("new key"), "{text}");
    assert!(!text.contains("wisp login locked"), "{text}");
    assert!(!text.contains("wisp login shell"), "{text}");
}

struct Never(AtomicUsize);

impl Provider for Never {
    fn stream(
        &self,
        _: &wisp_core::provider::Request<'_>,
    ) -> futures::stream::BoxStream<
        'static,
        Result<wisp_core::provider::ProviderEvent, wisp_core::provider::ProviderError>,
    > {
        Box::pin(futures::stream::empty())
    }
    fn cache_prefix(&self, _: &wisp_core::provider::Request<'_>) -> Option<String> {
        None
    }
    fn plan_usage(
        &self,
    ) -> futures::future::BoxFuture<'static, Result<Option<PlanUsage>, PlanUsageError>> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(futures::future::pending())
    }
}

#[tokio::test(start_paused = true)]
async fn claude_is_asked_only_with_its_usage_on_and_a_stuck_lookup_ends_at_the_deadline() {
    let accounts = example();
    for (claude_usage, want) in [
        (false, vec!["chatgpt"]),
        (true, vec!["claude-subscription", "chatgpt"]),
    ] {
        let asked = Mutex::new(Vec::new());
        let never = Arc::new(Never(AtomicUsize::new(0)));
        let connect = |name: &str| {
            asked.lock().unwrap().push(name.to_owned());
            Some(never.clone() as Arc<dyn Provider>)
        };
        let deadline = tokio::time::Instant::now() + LOOKUP_LIMIT;
        let found = limits(
            &accounts,
            None,
            claude_usage,
            PlanUsageCache::default(),
            connect,
            deadline,
        )
        .await;
        assert_eq!(tokio::time::Instant::now(), deadline);
        assert_eq!(*asked.lock().unwrap(), want);
        assert_eq!(
            never.0.load(std::sync::atomic::Ordering::SeqCst),
            want.len()
        );
        assert!(found.values().all(Option::is_none), "{found:?}");
        let text = shown(&accounts, None, &found, 80);
        assert!(
            text.lines()
                .any(|line| line.contains("chatgpt") && line.contains("usage unavailable")),
            "{text}"
        );
        assert_eq!(
            text.matches("usage unavailable").count(),
            want.len(),
            "{text}"
        );
    }
    let one = limits(
        &accounts,
        Some("claude-api"),
        true,
        PlanUsageCache::default(),
        |_| None,
        tokio::time::Instant::now(),
    )
    .await;
    assert!(one.is_empty());
    let unready = limits(
        &accounts,
        Some("chatgpt"),
        true,
        PlanUsageCache::default(),
        |_| None,
        tokio::time::Instant::now(),
    )
    .await;
    assert_eq!(unready, looked(&[("chatgpt", None)]));
}

#[tokio::test]
async fn off_provider_accounts_keep_their_rows_and_fix_without_usage_or_sign_in_advice() {
    for (removed, health) in [
        (false, Health::SignedIn),
        (false, Health::Expired),
        (true, Health::SignedIn),
        (true, Health::Expired),
    ] {
        let reason = wisp_core::plugin::registry::off_words("gemini", removed);
        let fix = wisp_core::plugin::registry::provider_fix("gemini", removed);
        let mut subscription = account(
            "antigravity",
            AccountKind::Antigravity,
            Held::Subscription(SubscriptionProvider::Antigravity),
            health,
        );
        subscription.off = Some((reason.clone(), fix.clone()));
        let mut api = account("g1", AccountKind::GeminiApi, Held::Stored, Health::SignedIn);
        api.off = subscription.off.clone();
        let accounts = listed(vec![subscription, api], None);
        let found = limits(
            &accounts,
            None,
            true,
            PlanUsageCache::default(),
            |_| panic!("off accounts have no lookup"),
            tokio::time::Instant::now() + LOOKUP_LIMIT,
        )
        .await;
        assert!(found.is_empty());
        for width in [40, 80, 120] {
            let shown = shown(&accounts, None, &found, width);
            for words in [&reason, &fix, "antigravity", "g1"] {
                assert!(shown.contains(words), "{shown}");
            }
            for gone in ["sign-in expired", "sign in", "model used"] {
                assert!(!shown.contains(gone), "{shown}");
            }
        }
    }
    let mut implicit = account(
        wisp_protocol::CLAUDE_API,
        AccountKind::ClaudeApi,
        Held::Variable {
            var: "ANTHROPIC_API_KEY".into(),
            keys_file: true,
        },
        Health::SignedIn,
    );
    let provider = wisp_core::endpoints::ProviderKind::IMPLICIT.as_str();
    implicit.off = Some((
        wisp_core::plugin::registry::off_words(provider, false),
        wisp_core::plugin::registry::provider_fix(provider, false),
    ));
    let accounts = listed(vec![implicit], None);
    let shown = shown(&accounts, None, &Looked::new(), 120);
    assert!(shown.contains(wisp_protocol::CLAUDE_API), "{shown}");
    assert!(shown.contains(&format!("{provider} is off")), "{shown}");
}
