use super::page::{Columns, account_rows};
use super::*;
use crate::chrome::account::AccountKind;
use crate::events::{End, Ending, turn_ended};
use crate::markdown::Layout;
use crate::styled::StyledLine;
use crate::testing::text;
use crate::text::width;
use crate::theme::glyphs::GlyphSet;
use crate::theme::{Depth, Mode, Theme};
use crate::transcript::copy;
use wisp_protocol::plan_usage::{Percent, PlanTier, PlanUsage, PlanUsageError, UsageScope};
use wisp_protocol::plan_usage::{UsageSource, UsageState};
use wisp_protocol::{AccountSpend, Billing};

const NOW: i64 = 1_893_508_000;

fn window(label: &str, value: Option<f64>, duration: u64) -> UsageWindow {
    UsageWindow {
        key: label.into(),
        label: label.into(),
        duration_seconds: Some(duration),
        scope: UsageScope::All,
        used_percent: value.map(|p| Percent::try_from(p).unwrap()),
        resets_at: Some(NOW + 8040),
        observed_at: NOW,
        source: UsageSource::UsageEndpoint,
    }
}

fn state(windows: Vec<UsageWindow>, tier: Option<PlanTier>) -> UsageState {
    let mut snapshot = PlanUsage::new(None, windows).unwrap();
    snapshot.plan_tier = tier;
    UsageState {
        snapshot: Some(snapshot),
        loading: false,
        stale: false,
        error: None,
    }
}

fn spend(plan: f64, paid: f64) -> Spend {
    let amount = AccountSpend::of(plan + paid, Some(plan), Billing::Mixed, false);
    Spend {
        amount,
        priced: true,
    }
}

fn all_spend() -> AllChats {
    AllChats {
        accounts: [
            (
                "claude-subscription".into(),
                [spend(120.0, 0.0), spend(2400.0, 0.0)],
            ),
            ("claude-api".into(), [spend(0.0, 9.0), spend(0.0, 150.0)]),
        ]
        .into(),
        today: spend(120.0, 9.0),
        window: spend(2400.0, 150.0),
        label: format!("{} days", wisp_protocol::SPEND_WINDOW_DAYS),
        days: [
            ("Today", 120.0, 9.0),
            ("Yesterday", 310.0, 12.0),
            ("Thu", 280.0, 5.0),
            ("Wed", 250.0, 0.0),
            ("Tue", 400.0, 20.0),
            ("Mon", 330.0, 8.0),
            ("Sun", 90.0, 6.0),
        ]
        .map(|(label, subscription, api)| (label.into(), spend(subscription, api)))
        .into(),
    }
}

fn unsplit(usd: f64, priced: bool) -> Spend {
    let amount = AccountSpend::of(usd, None, Billing::Mixed, true);
    Spend { amount, priced }
}

fn lower(mut spend: Spend) -> Spend {
    spend.amount.lower_bound = true;
    spend
}

fn time(at: i64) -> ResetTime {
    let (short, brief, long) = match at - NOW {
        266400 => ("in 3d 2h · Fri 19:00", "Fri", "Fri 19:00"),
        93600 => ("in 1d 2h · Wed 19:00", "Wed", "Wed 19:00"),
        _ => ("in 2h 14m · Tue 19:00", "19:00", "19:00, in 2 h 14 min"),
    };
    ResetTime {
        short: short.into(),
        brief: brief.into(),
        long: long.into(),
    }
}

fn account<'a>(alias: &'a str, kind: AccountKind, state: &'a UsageState) -> AccountView<'a> {
    AccountView {
        alias,
        mark: crate::chrome::account::mockup_mark(alias),
        kind,
        state,
        unavailable: None,
        spend: spend(0.0, 0.0),
        active: false,
        focused: false,
        refused: None,
        saved_key: false,
        off: false,
    }
}

fn rows(account: &AccountView<'_>, now: i64, layout: Layout) -> Vec<StyledLine> {
    let page = UsagePage {
        accounts: std::slice::from_ref(account),
        total: account.spend,
        all: None,
        now,
        scroll: 0,
    };
    account_rows(
        account,
        None,
        &Columns::of(&page, layout),
        now,
        layout,
        &time,
    )
}

fn capture(rows: &[StyledLine], layout: Layout, theme: &Theme) -> String {
    for line in rows {
        let cells: usize = line
            .spans
            .iter()
            .map(|s| width(&s.text, layout.width_mode))
            .sum();
        assert!(
            cells <= layout.width,
            "{cells} > {}: {}",
            layout.width,
            text(std::slice::from_ref(line))
        );
    }
    let mut output = text(rows)
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    output.push_str("\nStyles:\n");
    for line in rows {
        let styles = theme
            .line(line)
            .spans
            .into_iter()
            .filter(|s| !s.content.trim().is_empty())
            .map(|span| format!("{:?}:{:?}", span.content, span.style))
            .collect::<Vec<_>>();
        output.push_str(&styles.join(" | "));
        output.push('\n');
    }
    output
}

#[test]
fn page_detail_notices_and_mini_full_matrix() {
    let mut healthy = state(
        vec![
            window("5-hour", Some(42.0), 18000),
            window("Weekly", Some(18.0), 604800),
        ],
        None,
    );
    healthy.snapshot.as_mut().unwrap().windows[1].resets_at = Some(NOW + 266400);
    let mut warning = state(
        vec![
            window("5-hour", Some(87.0), 18000),
            window("Weekly", Some(93.0), 604800),
        ],
        None,
    );
    warning.snapshot.as_mut().unwrap().windows[1].resets_at = Some(NOW + 93600);
    let mut chatgpt = state(
        vec![
            window("5-hour", Some(12.0), 18000),
            window("Weekly", Some(31.0), 604800),
        ],
        Some(PlanTier::Pro),
    );
    chatgpt.snapshot.as_mut().unwrap().windows[1].resets_at = Some(NOW + 93600);
    let none = UsageState {
        snapshot: None,
        loading: false,
        stale: false,
        error: None,
    };
    let expired = UsageState {
        error: Some(PlanUsageError::AuthenticationRequired),
        ..none.clone()
    };
    let checking = UsageState {
        loading: true,
        ..healthy.clone()
    };
    let reached = state(vec![window("5-hour", Some(100.0), 18000)], None);
    let mut stale = state(vec![window("5-hour", Some(42.0), 18000)], None);
    stale.snapshot.as_mut().unwrap().windows[0].observed_at -= 7200;
    let unavailable = UsageState {
        error: Some(PlanUsageError::Network),
        ..none.clone()
    };
    let pages = [
        (
            "FULL",
            vec![
                (
                    "claude-subscription",
                    AccountKind::ClaudeSubscription,
                    &healthy,
                    spend(41.20, 0.0),
                ),
                (
                    "claude-work",
                    AccountKind::ClaudeSubscription,
                    &warning,
                    spend(3.10, 0.0),
                ),
                ("chatgpt", AccountKind::ChatGpt, &chatgpt, spend(12.40, 0.0)),
                (
                    "claude-api",
                    AccountKind::ClaudeApi,
                    &none,
                    spend(0.0, 2.86),
                ),
            ],
            spend(56.70, 2.86),
        ),
        (
            "STATES",
            vec![
                (
                    "claude-checking",
                    AccountKind::ClaudeSubscription,
                    &checking,
                    spend(0.0, 0.0),
                ),
                (
                    "claude-reached",
                    AccountKind::ClaudeSubscription,
                    &reached,
                    spend(1.25, 0.0),
                ),
                (
                    "claude-old",
                    AccountKind::ClaudeSubscription,
                    &stale,
                    spend(0.40, 0.0),
                ),
                (
                    "chatgpt-away",
                    AccountKind::ChatGpt,
                    &unavailable,
                    spend(0.18, 0.0),
                ),
                (
                    "claude-expired",
                    AccountKind::ClaudeSubscription,
                    &expired,
                    spend(0.0, 0.0),
                ),
            ],
            spend(1.83, 0.0),
        ),
    ];
    for columns in [40, 80, 120] {
        let mode = Mode::Dark;
        for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
            let layout = Layout::new(columns, glyphs);
            let theme = Theme::new(mode, Depth::TrueColor);
            let mut sections = vec![];
            for (title, fixtures, total) in &pages {
                let accounts: Vec<_> = fixtures
                    .iter()
                    .enumerate()
                    .map(|(index, (alias, kind, state, spend))| AccountView {
                        spend: *spend,
                        active: index == 0,
                        focused: index == 0,
                        ..account(alias, *kind, state)
                    })
                    .collect();
                let all = all_spend();
                let page = UsagePage {
                    accounts: &accounts,
                    total: *total,
                    all: (*title == "FULL").then_some(&all),
                    now: NOW,
                    scroll: 0,
                };
                sections.push(format!(
                    "{title}\n{}",
                    capture(&page_rows(&page, 100, layout, time).rows, layout, &theme)
                ));
                if *title != "FULL" {
                    continue;
                }
                for (height, scroll) in [(24, 0), (12, 999), (6, 999)] {
                    let view = UsagePage { scroll, ..page };
                    let rows = page_rows(&view, height, layout, time).rows;
                    assert!(rows.len() <= height);
                    sections.push(format!(
                        "VIEWPORT {height} SCROLL {scroll}\n{}",
                        capture(&rows, layout, &theme)
                    ));
                }
            }
            let fix = |file| {
                crate::chrome::account::variable_line(
                    "claude-api",
                    "ANTHROPIC_API_KEY",
                    file,
                    crate::chrome::account::Ending::Change,
                )
            };
            let refused = [
                (
                    "claude-api",
                    Some(fix(Some(wisp_protocol::DEFAULT_KEYS_FILE))),
                ),
                ("shell-api", Some(fix(None))),
                ("work-api", None),
            ]
            .map(|(alias, fix)| AccountView {
                spend: spend(0.0, 2.86),
                focused: fix.is_none(),
                saved_key: fix.is_none(),
                refused: Some(Refused { fix }),
                ..account(alias, AccountKind::ClaudeApi, &none)
            });
            let page = UsagePage {
                accounts: &refused,
                total: spend(0.0, 8.58),
                all: None,
                now: NOW,
                scroll: 0,
            };
            let rows = page_rows(&page, 100, layout, time).rows;
            let slips = crate::chrome::account::vocabulary_slips(&text(&rows));
            assert_eq!(slips, Vec::<&str>::new());
            sections.push(format!("REFUSED\n{}", capture(&rows, layout, &theme)));
            let windows = &warning.snapshot.as_ref().unwrap().windows;
            sections.push(format!(
                "NOTICE\n{}",
                capture(
                    &warning_rows("claude-work", &windows[0], layout, time),
                    layout,
                    &theme
                )
            ));
            for (name, state) in [
                ("two-window", &warning),
                ("stale", &stale),
                ("unavailable", &unavailable),
            ] {
                for hint in [UsageHint::Show, UsageHint::PageOpen] {
                    let candidates = mini_candidates(state, "model", None, NOW, hint, layout);
                    for room in [
                        columns.saturating_sub(20),
                        columns.saturating_sub(40),
                        20,
                        8,
                    ] {
                        let rows: Vec<_> =
                            fit_mini(&candidates, room, layout).into_iter().collect();
                        sections.push(format!(
                            "MINI {name} {hint:?} ROOM {room}\n{}",
                            capture(&rows, layout, &theme)
                        ));
                    }
                }
            }
            insta::assert_snapshot!(
                format!("usage_{columns}_{mode:?}_{glyphs:?}"),
                sections.join("\n\n")
            );
        }
    }
}

#[test]
fn a_fractional_reading_keeps_one_percent_column() {
    let meters = |values: [Option<f64>; 3], passed: bool, layout: Layout| -> Vec<String> {
        let mut windows = vec![
            window("5-hour", values[0], 18000),
            window("Weekly", values[1], 604800),
            window("Opus", values[2], 604800),
        ];
        if passed {
            windows.push(UsageWindow {
                resets_at: Some(NOW),
                ..window("Sonnet", Some(33.3333), 604800)
            });
        }
        let s = state(windows, None);
        text(&rows(
            &account("work", AccountKind::ChatGpt, &s),
            NOW,
            layout,
        ))
        .lines()
        .filter(|line| line.contains('%') || line.contains("unknown"))
        .map(str::to_owned)
        .collect()
    };
    let ends = |values: [Option<f64>; 3], layout: Layout| -> Vec<usize> {
        meters(values, false, layout)
            .iter()
            .map(|line| {
                let end = line.find('%').map_or_else(
                    || line.find("unknown").map_or(0, |at| at + "unknown".len()),
                    |at| at + 1,
                );
                width(&line[..end], layout.width_mode)
            })
            .collect()
    };
    for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
        for columns in [40, 80, 120] {
            let layout = Layout::new(columns, glyphs);
            let whole = [Some(87.0), Some(9.0), Some(42.0)];
            let today = ends(whole, layout);
            assert_eq!(today.len(), 3);
            assert!(today.iter().all(|end| *end == today[0]), "{today:?}");
            assert_eq!(meters(whole, true, layout), meters(whole, false, layout));
            for values in [
                [Some(74.99), None, Some(42.0)],
                [None, Some(42.0), Some(74.99)],
            ] {
                assert_eq!(ends(values, layout), today, "{columns} {values:?}");
            }
        }
    }
}

#[test]
fn boundaries_states_and_display_width_are_truthful() {
    for (mode, depth) in [(Mode::Dark, Depth::TrueColor), (Mode::Dark, Depth::None)] {
        for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii, GlyphSet::AmbiguousWide] {
            for columns in [40, 80, 120] {
                let layout = Layout::new(columns, glyphs);
                let theme = Theme::new(mode, depth);
                let family = UsageWindow {
                    scope: UsageScope::ModelFamily("Fable".into()),
                    ..window("Fable weekly", Some(100.0), 604800)
                };
                let reading = state(vec![window("Weekly", Some(66.0), 604800), family], None);
                let shown = text(&rows(
                    &account("sub-a", AccountKind::ClaudeSubscription, &reading),
                    NOW,
                    layout,
                ));
                assert!(shown.contains("Fable weekly"), "{shown}");
                assert!(shown.contains("100%"), "{shown}");
                assert!(
                    shown
                        .lines()
                        .all(|line| width(line, layout.width_mode) <= columns),
                    "{shown}"
                );
                let mut sections = vec![];
                for value in [0.0, 74.99, 75.0, 89.99, 90.0, 95.0, 99.99, 100.0] {
                    let s = state(vec![window("Weekly", Some(value), 604800)], None);
                    let account = AccountView {
                        spend: spend(0.0001, 0.0),
                        ..account(
                            "fictional-e\u{301}-界界-long-account",
                            AccountKind::ChatGpt,
                            &s,
                        )
                    };
                    let rows = rows(&account, NOW, layout);
                    let plain = text(&rows);
                    for gone in ["used", "! near", "!! high"] {
                        assert!(!plain.contains(gone), "{plain}");
                    }
                    assert_eq!(plain.contains("100%"), value == 100.0, "{plain}");
                    assert_eq!(
                        plain.contains("limit reached"),
                        value == 100.0 && columns >= 60,
                        "{plain}"
                    );
                    sections.push(capture(&rows, layout, &theme));
                }
                for error in [
                    None,
                    Some(PlanUsageError::Throttled {
                        retry_at: Some(NOW + 60),
                    }),
                    Some(PlanUsageError::AccessDenied),
                    Some(PlanUsageError::ScopeRequired),
                    Some(PlanUsageError::InvalidResponse),
                ] {
                    for loading in [false, true] {
                        let s = UsageState {
                            error,
                            loading,
                            ..state(vec![window("Unknown", None, 300)], None)
                        };
                        let account = AccountView {
                            spend: unsplit(0.25, false),
                            active: true,
                            ..account("unknown", AccountKind::ChatGpt, &s)
                        };
                        let rows = rows(&account, NOW, layout);
                        if !loading {
                            let plain = text(&rows);
                            assert!(!plain.contains("unavailable until"), "{plain}");
                            assert!(!plain.contains("tries again"), "{plain}");
                            let bare = UsageState {
                                snapshot: None,
                                ..s.clone()
                            };
                            let alone = text(&self::rows(
                                &AccountView {
                                    state: &bare,
                                    ..self::account("unknown", AccountKind::ChatGpt, &bare)
                                },
                                NOW,
                                layout,
                            ));
                            assert_eq!(
                                alone.contains("usage unavailable until 19:00"),
                                matches!(error, Some(PlanUsageError::Throttled { .. })),
                                "{alone}"
                            );
                            assert_eq!(
                                alone.contains("try again"),
                                matches!(error, None | Some(PlanUsageError::InvalidResponse)),
                                "{alone}"
                            );
                            assert_eq!(
                                plain.contains("enter sign in again"),
                                error == Some(PlanUsageError::ScopeRequired),
                                "{plain}"
                            );
                        }
                        sections.push(capture(&rows, layout, &theme));
                    }
                }
                let s = state(vec![window("Model", Some(46.0), 300)], None);
                let account = AccountView {
                    spend: lower(spend(1.0, 2.0)),
                    ..account("reset-due", AccountKind::ChatGpt, &s)
                };
                let rows = rows(&account, NOW + 8040, layout);
                assert!(text(&rows).contains("reset since"), "{}", text(&rows));
                assert!(!text(&rows).contains("ago"), "{}", text(&rows));
                let past = UsageState {
                    snapshot: None,
                    error: Some(PlanUsageError::Throttled {
                        retry_at: Some(NOW),
                    }),
                    ..s.clone()
                };
                let plain = text(&self::rows(
                    &self::account("past", AccountKind::ChatGpt, &past),
                    NOW,
                    layout,
                ));
                assert!(plain.contains("try again"), "{plain}");
                sections.push(capture(&rows, layout, &theme));
                let empty = UsagePage {
                    accounts: &[],
                    total: spend(0.0, 0.0),
                    all: None,
                    now: NOW,
                    scroll: 0,
                };
                sections.push(capture(
                    &page_rows(&empty, 24, layout, time).rows,
                    layout,
                    &theme,
                ));
                insta::assert_snapshot!(
                    format!("usage_states_{columns}_{mode:?}_{depth:?}_{glyphs:?}"),
                    sections.join("\n\n")
                );
            }
        }
    }
}

#[test]
fn off_api_and_unknown_tiers_have_no_redundant_copy() {
    for (kind, error) in [
        (
            AccountKind::ClaudeSubscription,
            Some(PlanUsageError::Disabled),
        ),
        (AccountKind::ClaudeApi, None),
    ] {
        let s = UsageState {
            error,
            ..state(vec![], Some(PlanTier::Pro))
        };
        let api = kind == AccountKind::ClaudeApi;
        let account = AccountView {
            spend: if api {
                spend(0.0, 1.0)
            } else {
                spend(1.0, 0.0)
            },
            ..account("claude-max", kind, &s)
        };
        let shown = text(&rows(&account, NOW, Layout::new(120, GlyphSet::Unicode)));
        assert_eq!(shown.lines().count(), 2, "{shown}");
        let amount = if api { " $1.00" } else { " ~$1.00" };
        assert!(shown.lines().nth(1).unwrap().ends_with(amount), "{shown}");
        for gone in ["Pro", "Max", "lookup off", "claude_usage", "/config"] {
            assert!(!shown.contains(gone), "{shown}");
        }
    }
}

#[test]
fn gauge_uses_brand_meter_scale_without_gold() {
    use crate::chrome::context::MeterLevel;
    use crate::styled::StyledSpan;
    use crate::theme::Token;
    for (value, level, token, bold, used) in [
        (0.0, MeterLevel::Calm, Token::TextMuted, false, 0),
        (0.004, MeterLevel::Calm, Token::TextMuted, false, 1),
        (74.99, MeterLevel::Calm, Token::TextMuted, false, 7),
        (75.0, MeterLevel::Warning, Token::Text, true, 7),
        (89.99, MeterLevel::Warning, Token::Text, true, 8),
        (90.0, MeterLevel::Over, Token::Error, false, 9),
        (99.999, MeterLevel::Over, Token::Error, false, 9),
        (100.0, MeterLevel::Over, Token::Error, false, 10),
    ] {
        let window = window("Weekly", Some(value), 604800);
        assert_eq!(severity(&window), level);
        let look = level.look();
        assert_eq!(look.fg, Some(token.into()));
        assert_eq!(look.bold, bold);
        for (glyphs, full, free) in [(GlyphSet::Unicode, "⣿", "⣀"), (GlyphSet::Ascii, "#", ".")]
        {
            let bar = gauge(&window, 10, look, Layout::new(80, glyphs)).unwrap();
            let runs = [
                StyledSpan::new(full.repeat(used), look),
                StyledSpan::new(free.repeat(10 - used), Token::TextFaint),
            ];
            let expected: Vec<_> = runs.into_iter().filter(|s| !s.text.is_empty()).collect();
            assert_eq!(bar, expected, "{value}");
        }
    }
}

#[test]
fn plan_usage_stays_out_of_the_transcript_as_root_and_child_events() {
    let event = wisp_protocol::AgentEvent::PlanUsageChanged {
        request: None,
        endpoint: "fictional".into(),
        state: state(
            vec![window("Weekly", Some(46.0), 604800)],
            Some(PlanTier::Pro),
        ),
    };
    for event in [
        event.clone(),
        wisp_protocol::AgentEvent::Agent {
            agent: wisp_protocol::AgentPath::new("/child"),
            event: Box::new(event),
        },
    ] {
        assert!(event.is_account_telemetry());
        let mut transcript = crate::transcript::Transcript::new(crate::transcript::Config {
            provider: "OpenAI".into(),
            cache_ttl: wisp_protocol::CacheTtl::OneHour,
            logging: true,
        });
        transcript.apply(&event, std::time::Duration::ZERO);
        let renderer = crate::markdown::Renderer::new(Layout::new(80, GlyphSet::Unicode));
        assert!(
            transcript
                .rows(&renderer, crate::transcript::Detail::Full)
                .is_empty()
        );
    }
}

#[test]
fn zero_spend_retains_billing_and_unknown_split_retains_amount() {
    use crate::chrome::account::AccountState::{Api, Subscription};
    let zero = spend(0.0, 0.0);
    assert_eq!(
        zero.parts(false, true),
        [(Some(Subscription), "~$0.00".into())]
    );
    assert_eq!(zero.parts(true, false), [(Some(Api), "$0.00".into())]);
    let unknown = unsplit(0.25, true);
    assert_eq!(unknown.parts(true, true), [(None, "~$0.25+".into())]);
    let s = state(vec![], None);
    let shown = text(&rows(
        &AccountView {
            spend: unknown,
            ..account("chatgpt", AccountKind::ChatGpt, &s)
        },
        NOW,
        Layout::new(80, GlyphSet::Ascii),
    ));
    let row = shown.lines().last().unwrap();
    assert!(row.starts_with("    Spent"), "{shown}");
    assert!(
        row.ends_with("~$0.25+ - mixed billing - split unavailable"),
        "{shown}"
    );
    assert!(!row.contains("subscription"), "{shown}");
}

#[test]
fn all_chat_kinds_include_listed_and_removed_accounts_and_keep_zero_and_lower_days() {
    let state = state(vec![], None);
    for (kind, window) in [
        (AccountKind::ClaudeApi, spend(2.0, 0.0)),
        (AccountKind::ChatGpt, spend(0.0, 2.0)),
    ] {
        let accounts = [account("listed", kind, &state)];
        let all = AllChats {
            window,
            days: vec![("Today".into(), lower(spend(0.0, 0.0)))],
            ..all_spend()
        };
        let page = UsagePage {
            accounts: &accounts,
            total: spend(0.0, 0.0),
            all: Some(&all),
            now: NOW,
            scroll: 0,
        };
        let shown = text(&page_rows(&page, 100, Layout::new(80, GlyphSet::Ascii), time).rows);
        assert!(shown.contains("All chats - Spent"), "{shown}");
        assert!(shown.contains("All chats - API equivalent"), "{shown}");
        for row in shown
            .lines()
            .filter(|row| row.trim_start().starts_with("Today "))
        {
            assert!(row.ends_with("$0.00+"), "{row}");
        }
        assert_eq!(
            shown
                .lines()
                .filter(|row| row.trim_start().starts_with("Today "))
                .count(),
            2
        );
        let empty = UsagePage { all: None, ..page };
        let shown = text(&page_rows(&empty, 100, Layout::new(80, GlyphSet::Ascii), time).rows);
        assert!(
            !shown.contains("All chats") && !shown.contains("all chats"),
            "{shown}"
        );
    }
}

#[test]
fn all_chat_unknown_splits_stay_one_amount_in_accounts_and_days() {
    let state = state(vec![], None);
    let accounts = [account("claude-api", AccountKind::ClaudeApi, &state)];
    let all = AllChats {
        accounts: [("claude-api".into(), [unsplit(0.25, true); 2])].into(),
        today: unsplit(0.25, true),
        window: unsplit(0.25, true),
        days: vec![("Today".into(), unsplit(0.25, true))],
        ..all_spend()
    };
    let page = UsagePage {
        accounts: &accounts,
        total: spend(0.0, 0.0),
        all: Some(&all),
        now: NOW,
        scroll: 0,
    };
    for columns in [40, 80, 120] {
        let layout = Layout::new(columns, GlyphSet::Ascii);
        let rows = page_rows(&page, 100, layout, time).rows;
        let shown = capture(&rows, layout, &Theme::new(Mode::Dark, Depth::TrueColor));
        assert_eq!(text(&rows).matches("~$0.25+").count(), 5, "{shown}");
        assert_eq!(
            text(&rows)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .matches("split unavailable")
                .count(),
            5,
            "{shown}"
        );
        assert!(!shown.contains("All chats - API equivalent"), "{shown}");
    }
}

#[test]
fn all_chat_totals_stay_pinned_and_aligned_for_ten_accounts_at_every_scroll() {
    let state = state(vec![], None);
    for two_kinds in [false, true] {
        let accounts: Vec<_> = (0..10)
            .map(|index| {
                account(
                    if index == 0 {
                        "claude-subscription"
                    } else {
                        "claude-api"
                    },
                    if two_kinds && index > 0 {
                        AccountKind::ClaudeApi
                    } else {
                        AccountKind::ChatGpt
                    },
                    &state,
                )
            })
            .collect();
        let mut all = all_spend();
        if !two_kinds {
            all.window = lower(spend(520.0, 0.0));
            all.today = spend(120.0, 0.0);
            all.label = "since 12 Nov".into();
            all.accounts.clear();
        }
        for columns in [40, 80, 120] {
            for glyphs in [GlyphSet::Ascii, GlyphSet::Unicode] {
                let layout = Layout::new(columns, glyphs);
                let page = UsagePage {
                    accounts: &accounts,
                    total: spend(40.0, if two_kinds { 3.0 } else { 0.0 }),
                    all: Some(&all),
                    now: NOW,
                    scroll: 0,
                };
                let max = page_rows(&page, 24, layout, time).max_scroll;
                for scroll in 0..=max {
                    let rows = page_rows(&UsagePage { scroll, ..page }, 24, layout, time).rows;
                    let shown = text(&rows);
                    capture(&rows, layout, &Theme::new(Mode::Dark, Depth::TrueColor));
                    let footer: Vec<_> = shown
                        .lines()
                        .skip_while(|line| !line.trim_start().starts_with("This chat"))
                        .collect();
                    assert_eq!(footer.len(), if columns == 40 { 7 } else { 4 }, "{shown}");
                    assert!(
                        footer.iter().any(|row| row.contains("All chats, today")),
                        "{shown}"
                    );
                    assert!(
                        footer
                            .iter()
                            .any(|row| row.contains(&copy::all_chats_label(&all.label))),
                        "{shown}"
                    );
                    let ends: Vec<_> = footer
                        .iter()
                        .filter_map(|row| row.find(" subscriptions"))
                        .collect();
                    assert_eq!(ends.len(), 3, "{shown}");
                    assert!(ends.iter().all(|end| *end == ends[0]), "{shown}");
                    if columns == 40 && !two_kinds {
                        assert_eq!(footer[0].trim(), "This chat");
                        assert_eq!(footer[2].trim(), "All chats, today");
                        assert_eq!(footer[4].trim(), "All chats, since 12 Nov");
                    }
                }
            }
        }
    }
}

#[test]
fn status_candidates_keep_only_relevant_windows_and_fit_atomically() {
    let mut model = window("Model", Some(99.99), 300);
    model.scope = UsageScope::Model("specific".into());
    let family = UsageWindow {
        scope: UsageScope::ModelFamily("Fable".into()),
        ..window("Fable weekly", Some(100.0), 604800)
    };
    let s = state(
        vec![window("Weekly", Some(46.0), 604800), model, family],
        None,
    );
    let layout = Layout::new(80, GlyphSet::Ascii);
    assert!(
        mini_candidates(&s, "claude-fable-5-2", None, NOW, UsageHint::Show, layout)
            .iter()
            .all(|candidate| !text(std::slice::from_ref(&candidate.line)).contains("Fable"))
    );
    let mut collision = window("Unlisted weekly", Some(95.0), 604800);
    collision.scope = UsageScope::ModelFamily("claude".into());
    let mut projected = window("Fable weekly", Some(100.0), 604800);
    projected.scope = UsageScope::Model("claude-fable-5-2".into());
    for (provider, model, windows, shown) in [
        (None, "claude-fable-5-2", vec![collision.clone()], false),
        (
            Some("anthropic"),
            "claude-fable-5-2",
            vec![collision.clone()],
            false,
        ),
        (
            Some("anthropic"),
            "claude-fable-5-2",
            vec![collision, projected.clone()],
            true,
        ),
        (None, "claude-opus-5-5", vec![projected], false),
    ] {
        let candidates = mini_candidates(
            &state(windows, None),
            model,
            provider,
            NOW,
            UsageHint::Show,
            layout,
        );
        let line = candidates
            .first()
            .map(|c| text(std::slice::from_ref(&c.line)))
            .unwrap_or_default();
        assert_eq!(line.contains("Fable weekly"), shown, "{line}");
        assert!(!line.contains("Unlisted weekly"), "{line}");
    }
    let gemini = UsageWindow {
        scope: UsageScope::ModelFamily("gemini".into()),
        ..window("Gemini weekly", Some(95.0), 604800)
    };
    let gemini = state(vec![gemini], None);
    for (provider, warns) in [(None, false), (Some("gemini"), true)] {
        assert_eq!(
            mini_candidates(
                &gemini,
                "gemini-4-carbon",
                provider,
                NOW,
                UsageHint::Show,
                layout
            )
            .iter()
            .any(|c| c.warns),
            warns
        );
    }
    let other = mini_candidates(&s, "other", None, NOW, UsageHint::Show, layout);
    assert!(
        !text(
            &other
                .iter()
                .map(|candidate| candidate.line.clone())
                .collect::<Vec<_>>()
        )
        .contains("99.99")
    );
    let specific = mini_candidates(&s, "specific", None, NOW, UsageHint::Show, layout);
    assert!(
        text(
            &specific
                .iter()
                .map(|candidate| candidate.line.clone())
                .collect::<Vec<_>>()
        )
        .contains("99.99% used")
    );
    for remaining in 0..80 {
        if let Some(line) = fit_mini(&specific, remaining, layout) {
            let shown = text(&[line]);
            assert!(width(&shown, layout.width_mode) <= remaining);
            assert!(shown.ends_with("/usage") || shown.ends_with("% used"));
            if shown.contains('%') {
                assert!(shown.contains("% used"));
            }
        }
    }
    let lines = |s: &UsageState, now: i64| {
        mini_candidates(s, "specific", None, now, UsageHint::Show, layout)
            .into_iter()
            .map(|candidate| text(&[candidate.line]))
            .collect::<Vec<_>>()
    };
    let mut reset = window("Model", Some(99.99), 300);
    reset.resets_at = Some(NOW + 60);
    let mixed = state(vec![window("Weekly", Some(46.0), 604800), reset], None);
    assert!(
        lines(&mixed, NOW + 60)
            .iter()
            .all(|line| !line.contains("99.99"))
    );
    assert!(lines(&mixed, NOW + 60)[0].starts_with("week "));
    let alone = state(vec![mixed.snapshot.unwrap().windows[0].clone()], None);
    assert_eq!(lines(&alone, NOW + 60), ["stale /usage"]);
}

#[test]
fn expanded_tier_spend_keeps_lower_bound_and_never_prices_unknown_work() {
    let s = state(vec![], Some(PlanTier::Pro));
    for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
        let layout = Layout::new(120, glyphs);
        let mut account = AccountView {
            spend: lower(spend(1.25, 0.0)),
            ..account("fictional", AccountKind::ChatGpt, &s)
        };
        let shown = text(&rows(&account, NOW, layout));
        let row = shown.lines().last().unwrap();
        assert!(row.starts_with("    API equivalent "), "{shown}");
        assert!(row.ends_with(" ~$1.25+"), "{shown}");
        for usd in [0.0, 1.25] {
            account.spend = Spend {
                priced: false,
                ..spend(usd, 0.0)
            };
            let shown = text(&rows(&account, NOW, layout));
            assert_eq!(shown.contains("not priced"), usd == 0.0, "{shown}");
        }
    }
}

#[test]
fn rich_mini_marks_and_gauges_only_the_highest_window() {
    let s = state(
        vec![
            window("5-hour", Some(82.0), 18000),
            window("Weekly", Some(93.0), 604800),
        ],
        None,
    );
    for (glyphs, rich) in [
        (GlyphSet::Unicode, "!! week ⣿⣿⣿⣀ 93% used · 5h 82% used"),
        (GlyphSet::Ascii, "!! week ###. 93% used - 5h 82% used"),
    ] {
        let layout = Layout::new(80, glyphs);
        for hint in [UsageHint::Show, UsageHint::PageOpen] {
            let candidates = mini_candidates(&s, "model", None, NOW, hint, layout);
            let expected = if hint == UsageHint::Show {
                format!(
                    "{rich} {} /usage",
                    crate::theme::glyphs::Glyphs::of(glyphs).separator
                )
            } else {
                rich.into()
            };
            assert_eq!(text(std::slice::from_ref(&candidates[0].line)), expected);
            let lines: Vec<String> = candidates
                .iter()
                .map(|candidate| text(std::slice::from_ref(&candidate.line)))
                .collect();
            let one = "!! week 93% used";
            let separator = crate::theme::glyphs::Glyphs::of(glyphs).separator;
            let hinted = format!("{one} {separator} /usage");
            assert_eq!(lines.iter().filter(|line| *line == one).count(), 1);
            if hint == UsageHint::Show {
                let at = lines.iter().position(|line| line == one);
                assert_eq!(
                    at.and_then(|at| at.checked_sub(1)).map(|at| &lines[at]),
                    Some(&hinted),
                    "{lines:?}"
                );
            }
        }
    }
}

#[test]
fn focused_accounts_use_shared_selection_on_every_row() {
    use crate::theme::{Ink, Token};
    let s = state(
        vec![window("Weekly", Some(46.0), 604800)],
        Some(PlanTier::Team),
    );
    for columns in [40, 80, 120] {
        for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
            let layout = Layout::new(columns, glyphs);
            let mut account = AccountView {
                spend: spend(2.80, 0.0),
                active: true,
                focused: true,
                ..account("chatgpt", AccountKind::ChatGpt, &s)
            };
            let shown = rows(&account, NOW, layout);
            let first = &shown[0];
            assert_eq!(
                first.spans[0].text,
                crate::chrome::selection_mark(layout).text
            );
            assert_eq!(first.spans[0].look.fg, Some(Ink::Token(Token::Accent)));
            assert!(text(&shown[..1]).contains("ChatGPT Team"));
            assert!(text(&shown).contains("this chat"));
            for row in &shown {
                assert_eq!(
                    width(&text(std::slice::from_ref(row)), layout.width_mode),
                    columns
                );
                for span in &row.spans {
                    assert_eq!(span.look.bg, Some(Ink::Token(Token::AccentTint)));
                    assert_ne!(span.look.fg, Some(Ink::Token(Token::TextFaint)));
                }
            }
            assert!(text(&shown).contains("~$2.80"));
            account.focused = false;
            let shown = rows(&account, NOW, layout);
            assert!(text(&shown).starts_with("  ChatGPT Team"));
            assert!(
                shown
                    .iter()
                    .flat_map(|row| &row.spans)
                    .all(|span| span.look.bg.is_none())
            );
        }
    }
}

#[test]
fn footer_keys_stay_atomic_lowercase_and_exit_last_at_every_height() {
    let expired = UsageState {
        snapshot: None,
        loading: false,
        stale: false,
        error: Some(PlanUsageError::AuthenticationRequired),
    };
    let accounts = [AccountView {
        focused: true,
        ..account("claude", AccountKind::ClaudeSubscription, &expired)
    }];
    for (accounts, keys) in [
        (&[][..], &["r refresh", "esc close"][..]),
        (
            &accounts[..],
            &["enter sign in again", "r refresh", "esc close"][..],
        ),
    ] {
        let page = UsagePage {
            accounts,
            total: spend(0.0, 0.0),
            all: None,
            now: NOW,
            scroll: 0,
        };
        for columns in [40, 80, 120] {
            for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
                let layout = Layout::new(columns, glyphs);
                let separator = format!(" {} ", crate::theme::glyphs::Glyphs::of(glyphs).separator);
                for height in [1, 4, 6, 24] {
                    let rows = page_rows(&page, height, layout, time).rows;
                    assert!(rows.len() <= height);
                    let last = text(&rows[rows.len() - 1..]).trim_end().to_owned();
                    assert!(width(&last, layout.width_mode) <= columns);
                    assert!(last.ends_with("esc close"), "{last}");
                    for key in last.trim().split(&separator) {
                        assert!(keys.contains(&key), "{key}");
                    }
                    if columns >= 80 {
                        assert_eq!(last.trim(), keys.join(&separator));
                    }
                    assert_eq!(text(&rows).matches("esc close").count(), 1);
                }
            }
        }
    }
}

#[test]
fn limit_refusal_retains_the_turn_endpoint_after_selection_changes() {
    use wisp_protocol::{
        AgentEvent, ErrorDetail, ErrorKind, FailurePhase, FailureSource, RetryClass, RetryFocus,
        RetryOffer,
    };
    const HOUR: i64 = 3600;
    let time = |at| {
        if at == NOW + HOUR {
            "Fri 30 Oct 09:00 +00:00".into()
        } else {
            "Sun 1 Nov 15:10 +00:00".into()
        }
    };
    let guard = UsageWindow {
        key: wisp_protocol::plan_usage::RETRY_AFTER_KEY.into(),
        label: "Retry after".into(),
        scope: UsageScope::Model("claude-fable-5-2".into()),
        resets_at: Some(NOW + HOUR),
        ..window("Retry after", None, 604800)
    };
    let reading = UsageWindow {
        scope: UsageScope::ModelFamily("Fable".into()),
        ..window("Fable weekly", Some(100.0), 604800)
    };
    for (account, guarded, armed, status) in [
        (Some("example"), true, true, 429),
        (None, true, false, 429),
        (Some("example"), false, true, 429),
        (Some("example"), true, true, 503),
    ] {
        let mut transcript = crate::transcript::Transcript::new(crate::transcript::Config {
            provider: "Anthropic".into(),
            cache_ttl: wisp_protocol::CacheTtl::OneHour,
            logging: true,
        })
        .with_reset_time(time);
        transcript.endpoint(account);
        transcript.apply(&AgentEvent::turn_started(1), std::time::Duration::ZERO);
        transcript.endpoint(Some("example2"));
        let mut detail = ErrorDetail::new(
            FailurePhase::Response,
            FailureSource::Endpoint,
            RetryClass::Never,
        );
        detail.status = Some(status);
        detail.limit_reached = Some(Box::new(if guarded {
            guard.clone()
        } else {
            window("Weekly", None, 604800)
        }));
        detail.model_limit = Some(Box::new(reading.clone()));
        transcript.apply(
            &turn_ended(End {
                ending: Ending::Error(ErrorKind::RateLimit),
                message: "example refusal".into(),
                detail: Some(detail),
                attempts: 1,
                offer: Some(RetryOffer {
                    retry: true,
                    switch: true,
                    focus: RetryFocus::Retry,
                    suggest: None,
                    retries_at: armed.then_some(NOW + HOUR + 60),
                }),
                ..End::default()
            }),
            std::time::Duration::ZERO,
        );
        for columns in [40, 80, 120] {
            for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
                let renderer = crate::markdown::Renderer::new(Layout::new(columns, glyphs));
                let draw = |transcript: &mut crate::transcript::Transcript| {
                    text(&transcript.rows(&renderer, crate::transcript::Detail::Full))
                };
                let shown = draw(&mut transcript);
                let is_guard = guarded && status == 429;
                let sep = if glyphs == GlyphSet::Unicode {
                    "·"
                } else {
                    "-"
                };
                let expected = if is_guard && account.is_none() {
                    "Request rate limited"
                } else if is_guard {
                    "example is rate limited"
                } else {
                    "example reached its usage limit"
                };
                assert!(shown.contains(expected), "{shown}");
                assert_eq!(
                    shown.contains("Wisp retries by itself then."),
                    is_guard && armed,
                    "{shown}"
                );
                assert!(!shown.contains("example2"), "{shown}");
                if is_guard && account.is_some() {
                    let (headline, reading) = if columns == 40 {
                        (
                            format!("{expected} {sep} retry\n   after Fri 30 Oct 09:00 +00:00"),
                            format!(
                                "Fable weekly 100% used {sep} resets Sun\n   1 Nov 15:10 +00:00"
                            ),
                        )
                    } else {
                        (
                            format!("{expected} {sep} retry after Fri 30 Oct 09:00 +00:00"),
                            format!("Fable weekly 100% used {sep} resets Sun 1 Nov 15:10 +00:00"),
                        )
                    };
                    assert!(
                        shown.contains(&format!(
                            "{headline}\n   {reading}\n   Nothing from this turn was kept."
                        )),
                        "{shown}"
                    );
                }
                transcript.apply(
                    &AgentEvent::PlanUsageChanged {
                        request: None,
                        endpoint: "example".into(),
                        state: state(vec![window("Fable weekly", Some(1.0), 604800)], None),
                    },
                    std::time::Duration::ZERO,
                );
                assert_eq!(shown, draw(&mut transcript));
            }
        }
    }
}

#[test]
fn expired_account_displays_the_clients_shared_login_hint() {
    let state = UsageState {
        snapshot: None,
        loading: false,
        stale: false,
        error: Some(PlanUsageError::AuthenticationRequired),
    };
    let account = account("claude", AccountKind::ClaudeSubscription, &state);
    let wide = text(&rows(&account, NOW, Layout::new(80, GlyphSet::Ascii)));
    let row = wide.lines().nth(1).unwrap();
    assert!(row.starts_with("    sign-in expired   "), "{wide}");
    assert!(row.ends_with("   enter sign in again"), "{wide}");
    let narrow = text(&rows(&account, NOW, Layout::new(40, GlyphSet::Ascii)));
    assert!(
        narrow.contains("    sign-in expired\n    enter sign in again"),
        "{narrow}"
    );
    assert!(!wide.contains("/login"), "{wide}");
}

#[test]
fn the_model_reads_the_percent_and_the_words_usage_and_account_show() {
    for (value, shown) in [
        (0.0, "0%"),
        (0.4, "0.4%"),
        (43.0, "43%"),
        (12.5, "12.5%"),
        (99.4, "99.4%"),
        (99.6, "99.6%"),
        (99.9, "99.9%"),
        (100.0, "100%"),
    ] {
        assert_eq!(percent(Percent::try_from(value).unwrap()), shown, "{value}");
        assert_eq!(wisp_tool_text::config::percent(value), shown, "{value}");
    }
    assert_eq!(LIMIT_REACHED, wisp_tool_text::config::LIMIT_REACHED);
    assert_eq!(crate::transcript::copy::SEP, wisp_tool_text::SEP);
    assert_eq!(
        crate::chrome::account::billing_word(crate::chrome::account::AccountState::Expired),
        wisp_tool_text::config::SIGN_IN_EXPIRED
    );
}

#[test]
fn an_off_providers_subscription_never_offers_a_sign_in() {
    let state = UsageState {
        snapshot: None,
        loading: false,
        stale: false,
        error: Some(PlanUsageError::AuthenticationRequired),
    };
    let mut account = account("antigravity", AccountKind::Antigravity, &state);
    account.off = true;
    account.unavailable = Some("gemini is off");
    assert!(!account.signs_in());
    let shown = text(&rows(&account, NOW, Layout::new(80, GlyphSet::Ascii)));
    assert!(shown.contains("unavailable: gemini is off"), "{shown}");
    assert!(!shown.contains("sign in"), "{shown}");
}
