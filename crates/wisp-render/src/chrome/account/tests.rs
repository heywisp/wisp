use super::*;
use crate::chrome::settings::Scope;
use crate::styled::Look;
use crate::testing::{line_text, texts};
use crate::text::spans_text;
use crate::theme::glyphs::GlyphSet;

const PROBLEMS: [CodeProblem; 5] = [
    CodeProblem::Empty,
    CodeProblem::NoState,
    CodeProblem::Repeated,
    CodeProblem::Spaced,
    CodeProblem::TooLong,
];

fn mistakes() -> Vec<Mistake> {
    PROBLEMS
        .iter()
        .map(|problem| Mistake::Pasted(*problem))
        .chain([
            Mistake::Earlier,
            Mistake::Refused,
            Mistake::Unreachable,
            Mistake::TimedOut,
        ])
        .collect()
}

fn refusals() -> Vec<SameAccount> {
    let mut out = Vec::new();
    for surface in [Surface::Terminal, Surface::App] {
        for (provider, name) in [
            (SubscriptionProvider::Claude, "claude"),
            (SubscriptionProvider::ChatGpt, "chatgpt"),
            (SubscriptionProvider::Claude, "claude-work"),
            (
                SubscriptionProvider::Claude,
                "claude-work-account-for-the-team",
            ),
        ] {
            out.push(SameAccount {
                provider,
                name: name.to_owned(),
                mark: mockup_mark(name),
                surface,
                short: !matches!(name, "claude" | "chatgpt"),
            });
        }
    }
    out
}

fn bare_rows(refusal: &SameAccount, columns: usize, glyphs: &Glyphs) -> Vec<String> {
    let room = match refusal.surface {
        Surface::Terminal => columns - 2,
        Surface::App => columns - 4,
    };
    let rows = texts(&same_account_rows(refusal, room, glyphs));
    for row in &rows {
        assert!(width_of(row, WidthMode::from(glyphs.set)) <= room, "{row}");
    }
    let lead = "To sign it in again:";
    let at = rows.iter().position(|row| row.starts_with(lead)).unwrap();
    let command: Vec<&str> = match rows[at].strip_prefix(lead).unwrap().trim() {
        "" => rows[at + 1..].iter().map(String::as_str).collect(),
        joined => vec![joined],
    };
    assert_eq!(
        command.join(" "),
        login_again(refusal.surface, &refusal.name, refusal.short)
    );
    if command.len() > 1 {
        assert_eq!(command.last(), Some(&refusal.name.as_str()));
    }
    rows
}

fn kinds() -> [AccountKind; 7] {
    AccountKind::ALL
}

#[test]
fn the_kind_words_here_are_the_words_the_parent_agent_reads() {
    use wisp_tool_text::agents::AccountProvider;
    for kind in AccountKind::ALL {
        let (provider, word, subscription, gateway) = match kind {
            AccountKind::ClaudeSubscription => {
                (AccountProvider::Anthropic, "anthropic", true, false)
            }
            AccountKind::ChatGpt => (AccountProvider::OpenAi, "openai", true, false),
            AccountKind::Antigravity => (AccountProvider::Gemini, "gemini", true, false),
            AccountKind::ClaudeApi => (AccountProvider::Anthropic, "anthropic", false, false),
            AccountKind::OpenAiApi => (AccountProvider::OpenAi, "openai", false, false),
            AccountKind::GeminiApi => (AccountProvider::Gemini, "gemini", false, false),
            AccountKind::Gateway => (AccountProvider::Anthropic, "anthropic", false, true),
        };
        let parent = wisp_tool_text::agents::account_kind(provider, subscription, gateway);
        let here = if kind.names_billing() {
            kind.label().to_owned()
        } else {
            format!("{}, {}", kind.label(), billing_word(kind.billing()))
        };
        assert_eq!(parent, here, "{kind:?}");
        if kind != AccountKind::Gateway {
            assert_eq!(AccountKind::billed(word, subscription), kind);
        }
    }
}

#[test]
fn each_credential_and_provider_has_one_kind() {
    use Credential::{AnthropicPlan, AntigravityPlan, ApiKey, AuthToken, ChatGptPlan};
    use Endpoint::{Custom, Official};
    for (credential, reach, provider, kind) in [
        (
            AnthropicPlan,
            Official,
            "anthropic",
            AccountKind::ClaudeSubscription,
        ),
        (
            AnthropicPlan,
            Custom,
            "anthropic",
            AccountKind::ClaudeSubscription,
        ),
        (ChatGptPlan, Official, "openai", AccountKind::ChatGpt),
        (
            AntigravityPlan,
            Official,
            "gemini",
            AccountKind::Antigravity,
        ),
        (AntigravityPlan, Custom, "gemini", AccountKind::Antigravity),
        (ApiKey, Official, "anthropic", AccountKind::ClaudeApi),
        (AuthToken, Official, "anthropic", AccountKind::ClaudeApi),
        (ApiKey, Official, "openai", AccountKind::OpenAiApi),
        (ApiKey, Official, "OpenAI", AccountKind::OpenAiApi),
        (AuthToken, Official, "openai", AccountKind::OpenAiApi),
        (ApiKey, Official, "gemini", AccountKind::GeminiApi),
        (ApiKey, Official, "Gemini", AccountKind::GeminiApi),
        (AuthToken, Official, "gemini", AccountKind::GeminiApi),
        (ApiKey, Custom, "anthropic", AccountKind::Gateway),
        (AuthToken, Custom, "anthropic", AccountKind::Gateway),
        (ApiKey, Custom, "openai", AccountKind::Gateway),
        (ApiKey, Custom, "gemini", AccountKind::Gateway),
    ] {
        assert_eq!(
            AccountKind::of(credential, reach, provider),
            kind,
            "{credential:?} {reach:?} {provider}"
        );
    }
    let labels: Vec<_> = kinds().iter().map(|kind| kind.label()).collect();
    assert_eq!(
        labels,
        [
            "Claude subscription",
            "ChatGPT subscription",
            "Antigravity subscription",
            "Claude API",
            "OpenAI API",
            "Gemini API",
            "Gateway"
        ]
    );
    for kind in kinds()
        .into_iter()
        .filter(|kind| *kind != AccountKind::Gateway)
    {
        assert_eq!(
            kind.label(),
            format!("{} {}", kind.provider_word(), billing_word(kind.billing()))
        );
    }
    for kind in kinds() {
        let expected = if matches!(
            kind,
            AccountKind::ClaudeSubscription | AccountKind::ChatGpt | AccountKind::Antigravity
        ) {
            AccountState::Subscription
        } else {
            AccountState::Api
        };
        assert_eq!(kind.billing(), expected, "{kind:?}");
    }
}

#[test]
fn antigravity_and_gemini_api_render_on_usage_page() {
    use crate::markdown::Layout;
    use crate::plan_usage::{AccountView, ResetTime, Spend, UsagePage, page_rows};
    use crate::testing::text;
    use crate::theme::glyphs::GlyphSet;
    use wisp_protocol::plan_usage::{
        Percent, PlanUsage, UsageScope, UsageSource, UsageState, UsageWindow,
    };
    use wisp_protocol::{AccountSpend, Billing};

    assert_eq!(
        Credential::of(&Credential::AntigravityPlan.wire()),
        Some(Credential::AntigravityPlan)
    );
    assert_eq!(
        Credential::AntigravityPlan.billing(),
        Billing::AntigravityPlan
    );

    let now = 1_893_508_000_i64;
    let model_window = |id: &str, label: &str, pct: f64| UsageWindow {
        key: id.into(),
        label: label.into(),
        duration_seconds: None,
        scope: UsageScope::Model(id.into()),
        used_percent: Some(Percent::try_from(pct).unwrap()),
        resets_at: Some(now + 8040),
        observed_at: now,
        source: UsageSource::UsageEndpoint,
    };
    let antigravity_state = UsageState {
        snapshot: Some(
            PlanUsage::new(
                None,
                vec![
                    model_window("gemini-3-flash-preview", "Gemini 3 Flash", 10.0),
                    model_window("gemini-3.1-pro-preview", "Gemini 3.1 Pro", 25.0),
                ],
            )
            .unwrap(),
        ),
        loading: false,
        stale: false,
        error: None,
    };
    let empty_state = UsageState {
        snapshot: None,
        loading: false,
        stale: false,
        error: None,
    };
    let sub_spend = Spend {
        amount: AccountSpend::of(0.42, Some(0.42), Billing::AntigravityPlan, false),
        priced: true,
    };
    let api_spend = Spend {
        amount: AccountSpend::of(0.18, Some(0.0), Billing::Metered, false),
        priced: true,
    };
    let total = Spend {
        amount: AccountSpend::of(0.60, Some(0.42), Billing::Mixed, false),
        priced: true,
    };
    let accounts = [
        AccountView {
            alias: "antigravity",
            mark: mockup_mark("antigravity"),
            kind: AccountKind::Antigravity,
            state: &antigravity_state,
            unavailable: None,
            spend: sub_spend,
            active: true,
            focused: true,
            refused: None,
            saved_key: false,
            off: false,
        },
        AccountView {
            alias: "gemini-api",
            mark: mockup_mark("gemini-api"),
            kind: AccountKind::GeminiApi,
            state: &empty_state,
            unavailable: None,
            spend: api_spend,
            active: false,
            focused: false,
            refused: None,
            saved_key: false,
            off: false,
        },
    ];
    let page = UsagePage {
        accounts: &accounts,
        total,
        all: None,
        now,
        scroll: 0,
    };
    let render = |cols: usize, set: GlyphSet| {
        let layout = Layout::new(cols, set);
        let rows = page_rows(&page, 100, layout, |_| ResetTime {
            short: "in 2h 14m · Tue 19:00".into(),
            brief: "19:00".into(),
            long: "19:00, in 2 h 14 min".into(),
        })
        .rows;
        text(&rows)
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(
        render(40, GlyphSet::Unicode),
        [
            " Usage",
            "────────────────────────────────────────",
            "› Antigravity subscription",
            "  ● antigravity               this chat",
            "    Gemini 3 Flash ⣿⣀⣀⣀⣀⣀⣀⣀  10%",
            "    resets 19:00",
            "    Gemini 3.1 Pro ⣿⣿⣀⣀⣀⣀⣀⣀  25%",
            "    resets 19:00",
            "    API equivalent        ~$0.42",
            "",
            "  Gemini API · ● gemini-api",
            "    Spent                  $0.18",
            "────────────────────────────────────────",
            "  This chat                $0.18 API",
            "                    ~$0.42 subscriptions",
            " r refresh · esc close",
        ]
        .join("\n")
    );
    assert_eq!(
        render(80, GlyphSet::Ascii),
        [
            " Usage",
            "--------------------------------------------------------------------------------",
            "> Antigravity subscription - * antigravity   this chat",
            "    Gemini 3 Flash   #.................    10%   resets 19:00",
            "    Gemini 3.1 Pro   ####..............    25%   resets 19:00",
            "    API equivalent                      ~$0.42",
            "",
            "  Gemini API - * gemini-api",
            "    Spent                                $0.18",
            "--------------------------------------------------------------------------------",
            "  This chat                              $0.18 API - ~$0.42 subscriptions",
            " r refresh - esc close",
        ]
        .join("\n")
    );
    assert_eq!(
        render(120, GlyphSet::Unicode),
        [
            " Usage",
            "────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────",
            "› Antigravity subscription · ● antigravity   this chat",
            "    Gemini 3 Flash   ⣿⣿⣿⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀    10%   resets 19:00, in 2 h 14 min",
            "    Gemini 3.1 Pro   ⣿⣿⣿⣿⣿⣿⣿⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀    25%   resets 19:00, in 2 h 14 min",
            "    API equivalent                                  ~$0.42",
            "",
            "  Gemini API · ● gemini-api",
            "    Spent                                            $0.18",
            "────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────",
            "  This chat                                          $0.18 API · ~$0.42 subscriptions",
            " r refresh · esc close",
        ]
        .join("\n")
    );
}

#[test]
fn the_billing_words() {
    assert_eq!(billing_word(AccountState::Subscription), "subscription");
    assert_eq!(billing_word(AccountState::Api), "API");
    assert_eq!(billing_word(AccountState::Gone), "gone");
}

#[test]
fn an_expired_sign_in_is_billed_as_its_subscription() {
    let (expired, subscription, api) = (
        AccountState::Expired,
        AccountState::Subscription,
        AccountState::Api,
    );
    for (from, to, signed_in) in [
        (expired, api, (subscription, api)),
        (api, expired, (api, subscription)),
    ] {
        assert!(billing_move(from, to).is_some(), "{from:?} {to:?}");
        assert_eq!(
            billing_move(from, to),
            billing_move(signed_in.0, signed_in.1)
        );
        assert_eq!(
            moves_to("claude-work", from, to),
            moves_to("claude-work", signed_in.0, signed_in.1)
        );
        assert_eq!(moved_from(from, to), moved_from(signed_in.0, signed_in.1));
    }
    for follow in [Vec::new(), vec!["coder".to_owned()]] {
        assert_eq!(
            consequence(&follow, &[], true, api, expired),
            consequence(&follow, &[], true, api, subscription)
        );
    }
    assert_eq!(quoted(0.41, expired), quoted(0.41, subscription));
    assert_eq!(
        starts_on("claude-work", expired),
        starts_on("claude-work", subscription)
    );
    for state in [subscription, expired, api] {
        assert_eq!(billing_move(AccountState::Gone, state), None);
        assert_eq!(billing_move(state, AccountState::Gone), None);
    }
}

#[test]
fn every_paste_mistake_has_its_own_line() {
    let lines: Vec<_> = mistakes()
        .into_iter()
        .map(|mistake| mistake_line(mistake, "Claude"))
        .collect();
    let unique: std::collections::BTreeSet<_> = lines.iter().collect();
    assert_eq!(unique.len(), 9);
    assert_eq!(lines[0], "Paste the code from Claude's page first.");
    assert_eq!(
        lines[1],
        "That's only part of the code: use the Copy button on Claude's page."
    );
    assert_eq!(lines[7], "Couldn't reach Claude. enter tries again.");

    let ag_lines: Vec<_> = mistakes()
        .into_iter()
        .map(|mistake| mistake_line(mistake, "Antigravity"))
        .collect();
    let ag_unique: std::collections::BTreeSet<_> = ag_lines.iter().collect();
    assert_eq!(ag_unique.len(), 9);
    assert!(
        ag_lines
            .iter()
            .all(|line| !line.contains("Copy button") && !line.contains("the code")),
        "{ag_lines:?}"
    );
    assert!(ag_lines[0].contains("callback URL"), "{}", ag_lines[0]);
    assert!(ag_lines[1].contains("address bar"), "{}", ag_lines[1]);
}

#[test]
fn a_moved_notice_draws_a_hostile_account_name_cleaned() {
    let moved = gone_move("w\u{1b}[31m\u{202e}x", AccountState::Api, 0.41);
    let text = moved_lines(&moved, &Glyphs::of(GlyphSet::Unicode), 80).concat();
    assert!(text.contains("w^[[31m<U+202E>x"), "{text:?}");
}

fn rebuilds() -> Vec<Rebuild> {
    vec![
        Rebuild::Usd(0.41),
        Rebuild::PlanUsd(0.41, Scope::Exact),
        Rebuild::PlanUsd(0.41, Scope::UpTo),
        Rebuild::UpToUsd(0.41),
        Rebuild::Expired,
        Rebuild::Empty,
        Rebuild::Unpriced(Scope::Exact),
        Rebuild::Unpriced(Scope::UpTo),
        Rebuild::Unknown,
    ]
}

fn moves() -> Vec<Moved> {
    vec![
        Moved {
            mark: mockup_mark("claude-work"),
            how: Move::Switched,
            name: "claude-work".to_owned(),
            state: AccountState::Subscription,
            from: None,
            model: "opus-5-5".to_owned(),
            effort: Some("high".to_owned()),
            rebuild: Some(Rebuild::PlanUsd(0.41, Scope::Exact)),
        },
        Moved {
            mark: mockup_mark("claude-api"),
            how: Move::Started,
            name: "claude-api".to_owned(),
            state: AccountState::Api,
            from: Some(AccountState::Subscription),
            model: "opus-5-5".to_owned(),
            effort: Some("high".to_owned()),
            rebuild: Some(Rebuild::Usd(0.41)),
        },
        Moved {
            mark: mockup_mark("chatgpt"),
            how: Move::Continuing,
            name: "chatgpt".to_owned(),
            state: AccountState::Subscription,
            from: None,
            model: "gpt-6.1-sol".to_owned(),
            effort: None,
            rebuild: None,
        },
        Moved {
            mark: mockup_mark("claude-work"),
            how: Move::Switching,
            name: "claude-work".to_owned(),
            state: AccountState::Subscription,
            from: None,
            model: "opus-5-5".to_owned(),
            effort: Some("high".to_owned()),
            rebuild: Some(Rebuild::Empty),
        },
        Moved {
            mark: mockup_mark("a-very-long-account-name-for-forty"),
            how: Move::Switched,
            name: "a-very-long-account-name-for-forty".to_owned(),
            state: AccountState::Api,
            from: None,
            model: "sonnet-5-5".to_owned(),
            effort: Some("high".to_owned()),
            rebuild: Some(Rebuild::Usd(0.21)),
        },
    ]
}

fn sign_outs() -> Vec<SignedOut> {
    let moved = |how, name: &str, state| Moved {
        mark: mockup_mark(name),
        how,
        name: name.to_owned(),
        state,
        from: Some(AccountState::Subscription),
        model: "opus-5-5".to_owned(),
        effort: Some("high".to_owned()),
        rebuild: Some(Rebuild::Usd(0.41)),
    };
    let kept = |agent: &str, to: &str, account: &str, charges| Kept {
        agent: agent.to_owned(),
        to: to.to_owned(),
        account: account.to_owned(),
        charges,
        busy: None,
    };
    vec![
        SignedOut {
            gone: vec![(AccountKind::ClaudeSubscription, "claude-work".to_owned())],
            kept: vec![kept("coder", "Claude", "claude-work", true)],
            ..SignedOut::default()
        },
        SignedOut {
            start_mark: mockup_mark("claude-api"),
            gone: vec![
                (
                    AccountKind::ClaudeSubscription,
                    "claude-subscription".to_owned(),
                ),
                (AccountKind::ChatGpt, "chatgpt".to_owned()),
                (AccountKind::ClaudeApi, "work-api".to_owned()),
            ],
            left_sign_ins: 1,
            left_keys: vec!["work-api".to_owned()],
            chat: Some(moved(Move::Continuing, "claude-api", AccountState::Api)),
            start: Some(("claude-api".to_owned(), AccountState::Api, Start::Too)),
            agents: vec![(
                "coder".to_owned(),
                moved(Move::Switched, "claude-api", AccountState::Api),
                None,
            )],
            kept: vec![
                kept("reviewer", "Claude", "chatgpt", false),
                kept("scout", "Claude", "claude-subscription", true),
            ],
            again: false,
            elsewhere: false,
            again_with: None,
            reason: None,
            presets: Vec::new(),
            not_resumed: Vec::new(),
        },
        SignedOut {
            chat: Some(Moved {
                from: None,
                ..moved(
                    Move::Continuing,
                    "claude-subscription",
                    AccountState::Subscription,
                )
            }),
            kept: vec![kept("reviewer", "Claude", "chatgpt", false)],
            ..SignedOut::default()
        },
        SignedOut {
            chat: Some(moved(Move::Continuing, "claude-api", AccountState::Api)),
            agents: vec![(
                "coder".to_owned(),
                Moved {
                    from: None,
                    rebuild: Some(Rebuild::Usd(0.08)),
                    ..moved(Move::Switched, "claude-api", AccountState::Api)
                },
                None,
            )],
            ..SignedOut::default()
        },
        SignedOut {
            gone: vec![(
                AccountKind::ClaudeSubscription,
                "claude-subscription".to_owned(),
            )],
            start: Some(("claude-api".to_owned(), AccountState::Api, Start::Now)),
            start_mark: mockup_mark("claude-api"),
            ..SignedOut::default()
        },
        SignedOut {
            gone: vec![
                (AccountKind::ClaudeSubscription, "claude".to_owned()),
                (AccountKind::ChatGpt, "chatgpt".to_owned()),
            ],
            again: true,
            ..SignedOut::default()
        },
        SignedOut {
            gone: vec![(
                AccountKind::ClaudeSubscription,
                "claude-subscription".to_owned(),
            )],
            chat: Some(gone_move("claude-work", AccountState::Subscription, 0.41)),
            agents: vec![(
                "coder".to_owned(),
                gone_move("claude-work", AccountState::Subscription, 0.08),
                None,
            )],
            elsewhere: true,
            ..SignedOut::default()
        },
        SignedOut {
            gone: vec![(AccountKind::ClaudeSubscription, "claude-work".to_owned())],
            kept: vec![kept("coder", "the API", "claude-work", true)],
            elsewhere: true,
            ..SignedOut::default()
        },
        SignedOut {
            gone: vec![(
                AccountKind::ClaudeSubscription,
                "claude-subscription".to_owned(),
            )],
            chat: Some(gone_move("claude-work", AccountState::Subscription, 0.41)),
            kept: vec![Kept {
                busy: Some(("claude-work".to_owned(), AccountState::Subscription)),
                ..kept("coder", "Claude", "claude-subscription", false)
            }],
            elsewhere: true,
            ..SignedOut::default()
        },
        SignedOut {
            gone: vec![
                (
                    AccountKind::ClaudeSubscription,
                    "claude-subscription".to_owned(),
                ),
                (AccountKind::ClaudeSubscription, "claude-work".to_owned()),
            ],
            chat: Some(Moved {
                from: Some(AccountState::Subscription),
                model: "opus-5-5".to_owned(),
                ..gone_move("claude-api", AccountState::Api, 0.41)
            }),
            elsewhere: true,
            ..SignedOut::default()
        },
        SignedOut {
            chat: Some(Moved {
                from: None,
                rebuild: None,
                ..moved(Move::Started, "claude-api", AccountState::Api)
            }),
            again_with: Some("/login claude-subscription".to_owned()),
            ..SignedOut::default()
        },
        SignedOut {
            chat: Some(Moved {
                from: None,
                rebuild: None,
                ..moved(
                    Move::Started,
                    "claude-subscription",
                    AccountState::Subscription,
                )
            }),
            reason: Some(Said::State(
                Who::Account("work-api".to_owned(), mockup_mark("work-api")),
                "WORK_KEY not set".to_owned(),
            )),
            ..SignedOut::default()
        },
        SignedOut {
            chat: Some(moved(Move::Continuing, "claude-api", AccountState::Api)),
            not_resumed: vec![
                unloaded("coder", "claude-team", "sign-in expired", true),
                unloaded("scout", "claude-work", "can't be read now", false),
            ],
            ..SignedOut::default()
        },
        SignedOut {
            not_resumed: vec![unloaded("coder", "claude-team", "sign-in expired", true)],
            ..SignedOut::default()
        },
        SignedOut {
            not_resumed: vec![
                unloaded("coder", "claude-subscription", "sign-in expired", true),
                unloaded(
                    "tester",
                    "work-api",
                    "ANTHROPIC_TEAM_KEY_FOR_REVIEWS not set",
                    false,
                ),
            ],
            ..SignedOut::default()
        },
        SignedOut {
            gone: vec![(AccountKind::ClaudeSubscription, "claude-work".to_owned())],
            agents: vec![(
                "coder".to_owned(),
                moved(
                    Move::Switched,
                    "claude-subscription",
                    AccountState::Subscription,
                ),
                None,
            )],
            presets: vec![(preset_gone("claude-work", true), String::new())],
            ..SignedOut::default()
        },
        SignedOut {
            gone: vec![(AccountKind::ClaudeSubscription, "claude-work".to_owned())],
            kept: vec![kept("coder", "Claude", "claude-work", true)],
            presets: vec![(
                preset_gone("claude-work", false),
                "/login claude-work".to_owned(),
            )],
            ..SignedOut::default()
        },
    ]
}

fn preset_gone(account: &str, runs: bool) -> PresetGone {
    PresetGone {
        preset: "coder".to_owned(),
        account: account.to_owned(),
        runs,
    }
}

fn gone_move(name: &str, state: AccountState, usd: f64) -> Moved {
    Moved {
        mark: mockup_mark(name),
        how: Move::Continuing,
        name: name.to_owned(),
        state,
        from: None,
        model: "opus-5-5".to_owned(),
        effort: Some("high".to_owned()),
        rebuild: Some(match state {
            AccountState::Subscription => Rebuild::PlanUsd(usd, Scope::Exact),
            _ => Rebuild::Usd(usd),
        }),
    }
}

fn unloaded(
    agent: &str,
    account: &str,
    state: &str,
    sign_in: bool,
) -> (String, Unusable, Option<u8>, Option<String>) {
    let from = Unusable {
        account: account.to_owned(),
        state: state.to_owned(),
        sign_in,
    };
    let fix = sign_in.then(|| login_again(Surface::App, account, true));
    (agent.to_owned(), from, mockup_mark(account), fix)
}

fn drawn(text: &str, mark: Option<u8>, glyphs: &Glyphs) -> String {
    let line = mark_line(text, Token::TextMuted, mark, glyphs);
    line_text(&line)
}

fn sign_out_texts(glyphs: &Glyphs, columns: usize) -> Vec<String> {
    let names = |list: &[&str]| list.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
    let mut texts: Vec<String> = sign_outs()
        .iter()
        .map(|out| {
            signed_out_body(out, glyphs, columns.saturating_sub(3))
                .into_iter()
                .map(|(lead, line)| {
                    let lead = match lead {
                        Lead::Done => glyphs.done,
                        Lead::Bullet => glyphs.bullet,
                        Lead::Hang => " ",
                    };
                    let text = format!(" {lead} {}", crate::text::spans_text(&line.spans));
                    text.trim_end().to_owned()
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect();
    for ending in [Ending::Change, Ending::SignOut] {
        for file in [Some("<home>/.config/wisp/.env"), None] {
            texts.push(variable_line(
                "claude-api",
                "ANTHROPIC_API_KEY",
                file,
                ending,
            ));
        }
    }
    for file in [Some("~/.config/wisp/.env"), None] {
        let sep = format!(" {} ", glyphs.separator);
        texts.push(row_notes(true, true, file, &names(&["coder"])).join(&sep));
    }
    for state in [AccountState::Subscription, AccountState::Api] {
        for start in [Start::Now, Start::Too] {
            let line = start_line("claude-api", Some(4), state, start, glyphs);
            texts.push(wrap_plain(&line, columns, "", "", glyphs).join("\n"));
        }
    }
    for charges in [false, true] {
        texts.extend(stays_on("coder", "Claude", "claude-work", charges, glyphs));
    }
    for (agent, from, _, _) in [
        unloaded("coder", "claude-team", "sign-in expired", true),
        unloaded("scout", "claude-work", "can't be read now", false),
    ] {
        let command = login_again(Surface::Terminal, &from.account, true);
        let stayed = Stayed {
            agent,
            from: Some(from),
            ..Stayed::default()
        };
        texts.extend(not_resumed_line(&stayed, &command));
    }
    let named = |list: &[&str]| {
        let named = list.iter().map(|n| ((*n).to_owned(), mockup_mark(n)));
        named.collect::<Vec<_>>()
    };
    let four = named(&["claude-subscription", "claude-work", "chatgpt", "work-api"]);
    let priced = |rows: &[(&str, Option<&str>)]| {
        let rows = rows
            .iter()
            .map(|(n, a)| ((*n).to_owned(), a.map(str::to_owned)));
        rows.collect::<Vec<_>>()
    };
    let to_api = |rows: &[(&str, Option<&str>)]| Moving {
        to: ("claude-api".to_owned(), mockup_mark("claude-api")),
        state: AccountState::Api,
        rows: priced(rows),
    };
    let behind = |agent: &str| vec![(agent.to_owned(), cant_move("Claude"))];
    let both = [("this chat", Some("~$0.41")), ("coder", Some("~$0.08"))];
    let questions = [
        Asked {
            names: four.clone(),
            keys: named(&["work-api"]),
            moving: Some(to_api(&both)),
            behind: behind("reviewer"),
            presets: vec![preset_gone("claude-work", false)],
            ..Asked::default()
        },
        Asked {
            names: four.clone(),
            keys: named(&["work-api"]),
            ..Asked::default()
        },
        Asked {
            names: four.clone(),
            keys: named(&["work-api"]),
            moving: Some(to_api(&[both[0], both[1], ("scout", None)])),
            behind: behind("reviewer"),
            ..Asked::default()
        },
        Asked {
            names: four.clone(),
            keys: named(&["work-api"]),
            moving: Some(to_api(&both)),
            behind: behind("code-reviewer"),
            ..Asked::default()
        },
        Asked {
            names: named(&["work-api"]),
            keys: named(&["work-api"]),
            moving: Some(Moving {
                to: (
                    "claude-subscription".to_owned(),
                    mockup_mark("claude-subscription"),
                ),
                state: AccountState::Subscription,
                rows: priced(&[both[1]]),
            }),
            ..Asked::default()
        },
        Asked {
            names: named(&["work-api"]),
            keys: named(&["work-api"]),
            presets: vec![preset_gone("work-api", true)],
            ..Asked::default()
        },
        Asked {
            names: named(&["claude", "chatgpt"]),
            nothing_left: true,
            ..Asked::default()
        },
    ];
    let room = columns.saturating_sub(4);
    for asked in questions {
        let (title, asked) = asked.titled(room, glyphs);
        let mut rows = vec![spans_text(&title)];
        let lines = logout_question(&asked, room, glyphs);
        let marks = lines
            .iter()
            .flat_map(|line| &line.spans)
            .filter(|s| is_mark(s));
        let marks: Vec<Look> = marks.map(|span| span.look).collect();
        let gone = marks
            .iter()
            .filter(|look| **look == mark(None, glyphs).look);
        assert!(
            !asked.counted || gone.count() == asked.names.len(),
            "{marks:?}"
        );
        let to = asked.moving.as_ref().map(|m| mark(m.to.1, glyphs).look);
        assert!(to.is_none_or(|to| marks.contains(&to)), "{marks:?}");
        for line in lines {
            let wrapped = wrap(&line.spans, room, WidthMode::from(glyphs.set));
            let text = spans_text(&line.spans);
            assert!(wrapped.len() == 1 || !text.starts_with(' '), "{text}");
            rows.extend(wrapped.iter().map(|row| spans_text(&row.spans)));
        }
        texts.push(rows.join("\n"));
    }
    texts.push(stays_on("reviewer", "Claude", "chatgpt", false, glyphs)[0].clone());
    texts.extend(consequence(
        &[],
        &names(&["coder", "scout"]),
        false,
        AccountState::Subscription,
        AccountState::Subscription,
    ));
    texts.extend(consequence(
        &names(&["coder"]),
        &[],
        true,
        AccountState::Subscription,
        AccountState::Api,
    ));
    texts.extend(consequence(
        &[],
        &names(&["coder"]),
        false,
        AccountState::Subscription,
        AccountState::Subscription,
    ));
    texts.extend(consequence(
        &[],
        &[],
        false,
        AccountState::Subscription,
        AccountState::Api,
    ));
    texts.push(no_account("work", &names(&["claude-work", "chatgpt"])));
    texts.push(no_account("work", &[]));
    texts.push(key_left("work-api"));
    texts.extend(
        [
            KEY_DELETED,
            WHERE_SHOULD_IT_GO,
            WHERE_TITLE,
            WHICH_TITLE,
            NOTHING_SIGNED_OUT,
        ]
        .map(str::to_owned),
    );
    texts
}

#[test]
fn the_forms_at_three_widths_in_unicode_and_ascii() {
    for columns in [40, 80, 120] {
        for set in [GlyphSet::Unicode, GlyphSet::Ascii] {
            let glyphs = Glyphs::of(set);
            let mut sections = Vec::new();
            for kind in kinds() {
                sections.push(format!(
                    "{} | {}",
                    title(kind, "claude-work", &glyphs),
                    chip("claude-work", kind.billing(), &glyphs)
                ));
            }
            sections.push(chip("claude-work", AccountState::Gone, &glyphs));
            for moved in moves() {
                let lines: Vec<String> = moved_lines(&moved, &glyphs, columns)
                    .iter()
                    .map(|line| drawn(line, moved.mark, &glyphs))
                    .collect();
                for line in &lines {
                    assert!(line.chars().count() <= columns, "{line}");
                    assert!(!line.trim_end().ends_with(glyphs.separator), "{line}");
                }
                sections.push(lines.join("\n"));
            }
            for mistake in mistakes() {
                sections.push(mistake_line(mistake, "Claude"));
            }
            for refusal in refusals() {
                sections.push(bare_rows(&refusal, columns, &glyphs).join("\n"));
            }
            sections.extend(sign_out_texts(&glyphs, columns));
            for (name, state) in [
                ("claude-subscription", AccountState::Subscription),
                ("claude-api", AccountState::Api),
            ] {
                let parts = starts_on(name, state);
                sections.push(fit_parts(&[], &parts, "", &glyphs, columns - 3).join("\n"));
            }
            for (_, line) in signed_out_body(&sign_outs()[13], &glyphs, columns - 3) {
                let text = crate::text::spans_text(&line.spans);
                assert!(
                    width_of(&text, WidthMode::from(set)) <= columns - 3,
                    "{text}"
                );
            }
            let table = signed_out_body(&sign_outs()[12], &glyphs, columns - 3);
            let looks: Vec<(String, Look)> = table
                .iter()
                .flat_map(|(_, line)| &line.spans)
                .filter(|span| !span.text.trim().is_empty())
                .map(|span| (span.text.trim().to_owned(), span.look))
                .collect();
            let fix = glyphs.separator.to_owned();
            for (text, token) in [
                (NOT_RESUMED, Token::Text),
                ("coder", Token::Text),
                ("claude-team:", Token::Text),
                ("sign-in\u{a0}expired", Token::Error),
                (fix.as_str(), Token::TextFaint),
                ("/login\u{a0}claude-team", Token::Text),
            ] {
                let tight = columns == 40 && text == fix;
                assert_eq!(
                    looks.contains(&(text.to_owned(), Look::fg(token))),
                    !tight,
                    "{text} {looks:?}"
                );
            }
            let lone = format!("{} ", glyphs.separator);
            for line in sections.iter().flat_map(|s| s.lines()) {
                let glyphs_only = line.replace('\u{a0}', " ");
                assert!(set == GlyphSet::Unicode || glyphs_only.is_ascii(), "{line}");
                assert!(
                    set == GlyphSet::Ascii || !line.trim_start().starts_with(&lone),
                    "{line}"
                );
            }
            insta::assert_snapshot!(
                format!("account_{columns}_Dark_{set:?}"),
                sections.join("\n")
            );
        }
    }
    let wide = sign_out_texts(&Glyphs::of(GlyphSet::AmbiguousWide), 80);
    let rows = wide.iter().flat_map(|t| t.lines());
    let scout = rows.map(|l| l.split_whitespace().collect::<Vec<_>>());
    assert!(
        scout.clone().any(|words| words == ["scout", "-"]),
        "{wide:#?}"
    );
    assert!(!wide.iter().any(|t| t.contains('\u{2014}')), "{wide:#?}");
}

#[test]
fn the_module_says_none_of_the_banned_words() {
    let mut texts: Vec<String> = Vec::new();
    for set in [GlyphSet::Unicode, GlyphSet::Ascii] {
        let glyphs = Glyphs::of(set);
        for kind in kinds() {
            texts.push(title(kind, "claude-work", &glyphs));
            texts.push(chip("claude-work", kind.billing(), &glyphs));
            texts.push(kind_words(kind, &glyphs));
        }
        for rebuild in rebuilds() {
            let (full, tight) = rebuild.parts(set, false);
            texts.extend([
                rebuild.line(set),
                rebuild.short(set),
                rebuild.cell(),
                full,
                tight,
            ]);
        }
        texts.push(sign_in_row(&glyphs));
        for columns in [40, 80, 120] {
            for moved in moves() {
                texts.extend(moved_lines(&moved, &glyphs, columns));
            }
            texts.extend(sign_out_texts(&glyphs, columns));
        }
    }
    for mistake in mistakes() {
        texts.push(mistake_line(mistake, "Claude"));
        texts.push(mistake_line(mistake, "ChatGPT"));
    }
    for provider in ["Claude", "ChatGPT"] {
        texts.push(switch_first(provider));
    }
    for kind in kinds() {
        for again in [false, true] {
            texts.push(signed_in(
                again,
                kind,
                "work",
                Some(2),
                &Glyphs::of(GlyphSet::Unicode),
            ));
        }
    }
    texts.push(CLEANUP_WARNING.to_owned());
    for refusal in refusals() {
        texts.extend(bare_rows(&refusal, 40, &Glyphs::of(GlyphSet::Unicode)));
    }
    texts.push(billing_word(AccountState::Gone).to_owned());
    for (from, to) in [
        (AccountState::Subscription, AccountState::Api),
        (AccountState::Api, AccountState::Subscription),
        (AccountState::Api, AccountState::Api),
    ] {
        texts.push(moves_to("claude-work", from, to));
        texts.extend(
            starts_on("claude-work", to)
                .into_iter()
                .flat_map(|(a, b)| [a, b]),
        );
    }
    texts.extend([
        stay_on("chatgpt"),
        doesnt_offer("opus-5-5"),
        none_offers("opus-55"),
        CHOOSE_A_MODEL.to_owned(),
    ]);
    texts.extend([
        no_carry("chatgpt"),
        sign_in_expired("claude-work"),
        THIS_CHAT_NOW.to_owned(),
        ONLY_THE_MODEL.to_owned(),
        ADDS_AN_ACCOUNT.to_owned(),
        SIGNS_IN_THEN_SWITCHES.to_owned(),
        SWITCH_WORD.to_owned(),
        crate::transcript::copy::CLOSE_WORD.to_owned(),
        ACCOUNTS_WORD.to_owned(),
        MODELS_WORD.to_owned(),
        REBUILD_TITLE.to_owned(),
        crate::transcript::copy::RETRY_ON_TITLE.to_owned(),
        crate::transcript::copy::SWITCH_ACCOUNT.to_owned(),
        crate::transcript::copy::SIGN_IN_THEN_RETRY.to_owned(),
        crate::transcript::copy::NEW_KEY_THEN_RETRY.to_owned(),
        crate::transcript::copy::use_the_api().text(),
        key_refused("Claude", "claude-work · API"),
        crate::transcript::copy::SEARCH_NOT_RUN.to_owned(),
        crate::transcript::copy::INJECTED_THINKING.to_owned(),
        crate::transcript::copy::LAYER_ENDPOINT.to_owned(),
        crate::transcript::copy::LAYER_ENDPOINT_MODEL.to_owned(),
        crate::transcript::copy::SUMMARY_BLOCK_REFUSED.to_owned(),
    ]);
    let limit = wisp_protocol::plan_usage::UsageWindow {
        key: "five_hour".into(),
        label: "5-hour".into(),
        duration_seconds: Some(18000),
        scope: wisp_protocol::plan_usage::UsageScope::All,
        used_percent: None,
        resets_at: Some(18000),
        observed_at: 0,
        source: wisp_protocol::plan_usage::UsageSource::ErrorBody,
    };
    for endpoint in [Some("claude-work"), None] {
        texts.push(crate::transcript::copy::limit_reached_at(
            &limit,
            endpoint,
            false,
            |_| "14:05".into(),
        ));
    }
    for text in &texts {
        assert_eq!(vocabulary_slips(text), Vec::<&str>::new(), "{text}");
    }
    for slip in [
        "covered by plan",
        "claude-work · paid",
        "the env account",
        "this endpoint",
        "cause: endpoint_changed",
        "charged per use",
        "\u{2248} $0.41",
        "\u{2265} 2",
        "Signed in: claude plan-covered",
        "an env-account",
    ] {
        assert!(!vocabulary_slips(slip).is_empty(), "{slip}");
    }
    assert!(vocabulary_slips("explained, unpaid planner").is_empty());
    assert!(vocabulary_slips("--env-file PATH").is_empty());
    assert!(vocabulary_slips("~/.config/wisp/.env").is_empty());
    let env = &wisp_protocol::KEYS_FILE_ALIAS;
    for shown in [
        "\u{25cf} env",
        "env  Claude API",
        "Moves this chat to env",
        "\u{2219} Switched to env \u{b7} API",
        "\u{2502} env \u{2502}",
        "env",
    ] {
        assert!(vocabulary_slips(shown).contains(env), "{shown}");
    }
    for fine in [
        "claude-api is a saved account, so the key in Wisp's keys file (~/.config/wisp/.env) isn't used. Rename one to use both.",
        "Environment  ~120",
        "set in your environment",
    ] {
        assert!(!vocabulary_slips(fine).contains(env), "{fine}");
    }
}
