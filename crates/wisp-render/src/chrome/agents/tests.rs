use super::*;
use crate::testing::{child, line_text, spend};
use crate::text::WidthMode;
use crate::theme::glyphs::GlyphSet;

fn account(name: &str, billing: wisp_protocol::Billing) -> AccountBilling {
    AccountBilling {
        name: name.into(),
        state: crate::chrome::account::AccountState::of(billing),
        mark: crate::chrome::account::mockup_mark(name),
    }
}

fn accounts() -> Vec<AccountBilling> {
    use wisp_protocol::Billing::{AnthropicPlan, ChatGptPlan, Metered};
    vec![
        account("claude-subscription", AnthropicPlan),
        account("claude-work", AnthropicPlan),
        account("chatgpt", ChatGptPlan),
        account("claude-api", Metered),
    ]
}

fn main_row() -> MainRow {
    MainRow {
        running_since: None,
        spend: AccountSpend::default(),
        meter: None,
        checklist: None,
        model: "model".into(),
        effort: None,
        keep_warm: None,
        endpoint: None,
        priced: true,
        accounts: vec![],
    }
}

fn agent(path: &str, account: &str, model: &str, spent: f64) -> AgentRow {
    let depth = path.matches('/').count();
    let name = path.rsplit('/').next().unwrap();
    let billing = if account == "claude-api" {
        wisp_protocol::Billing::Metered
    } else {
        wisp_protocol::Billing::AnthropicPlan
    };
    let provider = if account == "chatgpt" {
        "openai"
    } else {
        "anthropic"
    };
    AgentRow {
        entry: serde_json::from_value(serde_json::json!({
            "path": path, "name": name, "model": model,
            "effort": "high", "depth": depth, "state": AgentState::Idle { outcome: None },
            "activity": wisp_protocol::Activity::Idle, "spent_usd": spent,
            "billing": billing, "endpoint": account, "provider": provider
        }))
        .unwrap(),
        effort: Some(wisp_protocol::Effort::High),
        last_outcome: None,
        last_mail: None,
    }
}

fn strip(rows: &[AgentRow], main: AccountSpend, cols: usize, glyphs: GlyphSet) -> Option<String> {
    strip_line(rows, main, ChatTurn::Idle, Layout::new(cols, glyphs)).map(|line| line_text(&line))
}

fn words(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn a_subscription_chat_and_an_api_child_sum_to_one_estimated_total() {
    use wisp_protocol::Billing::{AnthropicPlan, Metered};
    let rows = [child("scout", 0.06, Metered, true)];
    let main = spend(0.84, AnthropicPlan);
    let line = strip(&rows, main, 80, GlyphSet::Unicode).unwrap();
    assert_eq!(line, format!("{:<67}total ~$0.90 ", " ⧉ 1 agent"));
    let summary = crate::chrome::Summary {
        turns: 2,
        total: crate::chrome::shown_total(main, &rows, ChatTurn::Idle),
        ..crate::chrome::Summary::default()
    };
    let bye = line_text(&summary.goodbye(Layout::new(80, GlyphSet::Unicode))[1]);
    assert!(bye.ends_with(", total ~$0.90."), "{bye}");
}

#[test]
fn an_all_api_total_is_bare_and_shows_without_agents() {
    use wisp_protocol::Billing::Metered;
    let rows = [child("scout", 0.06, Metered, true)];
    let line = strip(&rows, spend(0.25, Metered), 80, GlyphSet::Ascii).unwrap();
    assert_eq!(words(&line), "= 1 agent total $0.31");
    let alone = strip(&[], spend(0.31, Metered), 80, GlyphSet::Unicode).unwrap();
    assert_eq!(alone, format!("{:>79} ", "total $0.31"));
}

#[test]
fn a_mixed_child_without_a_split_marks_the_total_as_estimated() {
    use wisp_protocol::Billing::{Metered, Mixed};
    let rows = [child("scout", 0.06, Mixed, true)];
    let line = strip(&rows, spend(0.25, Metered), 80, GlyphSet::Unicode).unwrap();
    assert_eq!(words(&line), "⧉ 1 agent total ~$0.31");
}

#[test]
fn a_running_agent_or_an_unpriced_step_makes_the_total_a_lower_bound() {
    use wisp_protocol::Billing::{AnthropicPlan, Metered};
    let main = spend(40.0, AnthropicPlan);
    let busy = [running(child("scout", 4.06, Metered, true))];
    let line = strip(&busy, main, 80, GlyphSet::Unicode).unwrap();
    assert_eq!(words(&line), "⧉ 1 agent · 1 working total ~$44.06+");
    let mut unpriced = child("scout", 4.06, Metered, true);
    unpriced.entry.lower_bound = true;
    let line = strip(&[unpriced], main, 80, GlyphSet::Ascii).unwrap();
    assert_eq!(words(&line), "= 1 agent total ~$44.06+");
}

#[test]
fn this_chats_running_reply_makes_the_total_a_lower_bound_until_it_ends() {
    use wisp_protocol::Billing::Metered;
    let main = spend(0.01, Metered);
    let total = |turn| {
        strip_line(&[], main, turn, Layout::new(80, GlyphSet::Unicode))
            .map(|line| words(&line_text(&line)))
    };
    assert_eq!(total(ChatTurn::Running).as_deref(), Some("total $0.01+"));
    assert_eq!(total(ChatTurn::Idle).as_deref(), Some("total $0.01"));
}

#[test]
fn a_held_agent_counts_among_the_agents_but_not_as_working_and_an_account_queued_one_as_working() {
    use wisp_protocol::Billing::Metered;
    let mut held = child("review", 0.0, Metered, true);
    held.entry.state = AgentState::Held {
        reason: HoldReason::Stopped,
    };
    let rows = [running(child("coder", 0.0, Metered, true)), held.clone()];
    let line = strip(&rows, AccountSpend::default(), 80, GlyphSet::Unicode).unwrap();
    assert_eq!(words(&line), "⧉ 2 agents · 1 working");
    let mut queued = running(child("docs", 0.0, Metered, true));
    queued.entry.state = AgentState::Queued {
        reason: wisp_protocol::QueueReason::Endpoint,
    };
    assert_eq!(timed(&queued, 74_000), "running 1m 14s");
    let rows = [queued, held];
    let line = strip(&rows, AccountSpend::default(), 80, GlyphSet::Unicode).unwrap();
    assert_eq!(words(&line), "⧉ 2 agents · 1 working");
}

#[test]
fn the_total_ends_at_the_screen_edge_on_a_wide_terminal() {
    use wisp_protocol::Billing::Metered;
    let line = strip(&[], spend(0.31, Metered), 200, GlyphSet::Unicode).unwrap();
    assert_eq!(line.trim_end().chars().count(), 199, "{line}");
}

#[test]
fn nothing_spent_shows_no_figure_and_no_row_without_agents() {
    use wisp_protocol::Billing::Metered;
    assert_eq!(
        strip(&[], AccountSpend::default(), 80, GlyphSet::Unicode),
        None
    );
    let rows = [child("scout", 0.0, Metered, true)];
    let line = strip(&rows, AccountSpend::default(), 80, GlyphSet::Unicode).unwrap();
    assert_eq!(words(&line), "⧉ 1 agent");
    let mut ended = child("scout", 0.0, Metered, true);
    ended.entry.state = AgentState::Ended;
    assert_eq!(
        strip(&[ended], AccountSpend::default(), 80, GlyphSet::Unicode),
        None
    );
}

fn five_agents() -> Vec<AgentRow> {
    use wisp_protocol::Billing::{AnthropicPlan, Metered};
    let mut failed = child("broken", 0.0, AnthropicPlan, true);
    failed.entry.state = AgentState::Idle {
        outcome: Some(IdleOutcome::Failed),
    };
    let mut gone = child("old", 0.0, AnthropicPlan, true);
    gone.entry.state = AgentState::Ended;
    vec![
        running(child("coder", 4.06, Metered, true)),
        running(child("reviewer", 0.0, AnthropicPlan, true)),
        failed,
        child("scout", 0.0, AnthropicPlan, true),
        child("docs", 0.0, AnthropicPlan, true),
        gone,
    ]
}

#[test]
fn the_strip_counts_live_agents_working_and_failed_with_failed_in_the_error_colour() {
    let main = spend(40.0, wisp_protocol::Billing::AnthropicPlan);
    let line = strip_line(
        &five_agents(),
        main,
        ChatTurn::Idle,
        Layout::new(80, GlyphSet::Unicode),
    )
    .unwrap();
    assert_eq!(
        words(&line_text(&line)),
        "⧉ 5 agents · 2 working · 1 failed total ~$44.06+"
    );
    let failed = line.spans.iter().find(|s| s.text == "1 failed").unwrap();
    assert_eq!(failed.look, Look::fg(Token::Error));
    let glyph = line.spans.iter().find(|s| s.text == "⧉").unwrap();
    assert_eq!(glyph.look, Look::fg(Token::Accent));
    let mut calm = five_agents();
    calm[2].entry.state = AgentState::Idle { outcome: None };
    calm[0].entry.state = AgentState::Idle { outcome: None };
    calm[1].entry.state = AgentState::Idle { outcome: None };
    let line = strip(&calm, main, 80, GlyphSet::Ascii).unwrap();
    assert_eq!(words(&line), "= 5 agents total ~$44.06");
}

#[test]
fn as_the_width_shrinks_the_counts_collapse_first_a_problem_outranks_working_then_total_shortens() {
    let main = spend(40.0, wisp_protocol::Billing::AnthropicPlan);
    let mut forms: Vec<String> = vec![];
    for cols in (16..=120).rev() {
        let line = strip(&five_agents(), main, cols, GlyphSet::Unicode).unwrap();
        assert!(width(&line, WidthMode::Normal) <= cols, "{cols}: {line}");
        let line = words(&line);
        if forms.last() != Some(&line) {
            forms.push(line);
        }
    }
    assert_eq!(
        forms,
        [
            "⧉ 5 agents · 2 working · 1 failed total ~$44.06+",
            "⧉ 5 · 2 working · 1 failed total ~$44.06+",
            "⧉ 5 · 1 failed total ~$44.06+",
            "⧉ 5 total ~$44.06+",
            "⧉ 5 ~$44.06+",
        ]
    );
    let at_40 = strip(&five_agents(), main, 40, GlyphSet::Unicode).unwrap();
    assert_eq!(words(&at_40), "⧉ 5 · 1 failed total ~$44.06+");
    let mut working = five_agents();
    working[2].entry.state = AgentState::Idle { outcome: None };
    let at_36 = strip(&working, main, 36, GlyphSet::Ascii).unwrap();
    assert_eq!(words(&at_36), "= 5 - 2 working total ~$44.06+");
}

#[test]
fn the_presets_line_counts_one_or_many() {
    assert_eq!(
        copy::presets_changed(1),
        "1 preset changed since agents started; see /agents"
    );
    assert_eq!(
        copy::presets_changed(2),
        "2 presets changed since agents started; see /agents"
    );
}

fn running(mut row: AgentRow) -> AgentRow {
    row.entry.state = AgentState::Running;
    row.entry.since_ms = Some(0);
    row
}

#[test]
fn a_clock_next_changes_when_its_own_second_turns() {
    let t = Duration::from_secs(9);
    let next = |row: &AgentRow, now_ms| clocks_next_change(std::slice::from_ref(row), t, now_ms);
    let mut row = running(agent("/a", "claude-work", "claude-opus-5-5", 0.1));
    row.entry.since_ms = Some(500);
    assert_eq!(next(&row, 74_499), Some(t + Duration::from_millis(1)));
    assert_eq!(next(&row, 74_500), Some(t + Duration::from_secs(1)));
    row.entry.state = AgentState::Idle { outcome: None };
    assert_eq!(next(&row, 74_500), None);
    let rows = [row.clone()];
    let due = |running_since, picked: &str| {
        let main = MainRow {
            running_since,
            ..main_row()
        };
        let view = AgentsView {
            main: &main,
            rows: &rows,
            t,
            now_ms: 74_500,
        };
        agents_next_change(&view, &AgentPath::new(picked))
    };
    assert_eq!(due(None, "/"), None, "an idle main");
    assert_eq!(due(None, "/a"), Some(t + Duration::from_secs(1)), "idle");
    let since = Some(Duration::from_millis(8_700));
    assert_eq!(due(since, "/"), Some(Duration::from_millis(9_700)));
    let untimed = AgentRow {
        entry: AgentEntry {
            since_ms: None,
            ..running(row).entry
        },
        ..rows[0].clone()
    };
    assert_eq!(next(&untimed, 74_500), None);
}

fn coder() -> AgentRow {
    let mut row = running(agent(
        "/task-a/coder",
        "claude-work",
        "claude-sonnet-5-5",
        0.08,
    ));
    row.entry.since_ms = Some(NOW - 134_000);
    row.entry.activity = wisp_protocol::Activity::Tool {
        name: "probe".into(),
    };
    row.entry.context = Some(wisp_protocol::ContextFill {
        used: 41_000,
        window: Some(850_000),
        limit: None,
        estimated: false,
        auto_compact: false,
    });
    row.entry.checklist = Some(wisp_protocol::TodoProgress {
        completed: 2,
        total: 5,
    });
    row.last_mail = Some(("task from task-a".into(), NOW - 120_000));
    row
}

const NOW: u64 = 4 * 3_600_000;

fn main_on_api() -> MainRow {
    MainRow {
        spend: spend(0.42, wisp_protocol::Billing::Metered),
        model: "test-model".into(),
        effort: Some("low".into()),
        endpoint: Some("claude-api".into()),
        accounts: accounts(),
        ..main_row()
    }
}

fn panel(rows: &[AgentRow], picked: &str, cols: usize, set: GlyphSet) -> [StyledLine; 2] {
    let main = main_on_api();
    let view = AgentsView {
        main: &main,
        rows,
        t: Duration::ZERO,
        now_ms: NOW,
    };
    panel_rows(&view, &AgentPath::new(picked), Layout::new(cols, set))
}

fn shown(rows: &[AgentRow], picked: &str, cols: usize, set: GlyphSet) -> [String; 2] {
    panel(rows, picked, cols, set).map(|line| line_text(&line))
}

#[test]
fn the_panel_is_two_rows_cut_from_the_right_at_120_80_and_40_columns() {
    let rows = [coder()];
    let at = |cols, set| shown(&rows, "/task-a/coder", cols, set);
    assert_eq!(
        at(120, GlyphSet::Unicode),
        [
            " ● coder   sonnet-5-5 high   running 2m 14s: probe   ~$0.08   41k/850k   todo 2/5",
            "   claude-work · subscription · task from task-a, 2 min ago",
        ]
    );
    assert_eq!(
        at(80, GlyphSet::Unicode),
        [
            " ● coder  sonnet-5-5 high  running 2m 14s  ~$0.08  41k/850k",
            "   claude-work · subscription · task from task-a, 2 min ago",
        ]
    );
    assert_eq!(
        at(40, GlyphSet::Unicode),
        [
            " ● coder  running 2m 14s  ~$0.08",
            "   sonnet-5-5 high · claude-work"
        ]
    );
    assert_eq!(
        at(80, GlyphSet::Ascii)[1],
        "   claude-work - subscription - task from task-a, 2 min ago"
    );
    assert_eq!(
        at(56, GlyphSet::Unicode)[0],
        " ● coder  running 2m 14s  ~$0.08",
        "under 60 the model moves down"
    );
    let mut long = coder();
    long.last_mail = Some((
        "report from /task-a/a-reviewer-with-a-long-name".into(),
        NOW,
    ));
    let cut = shown(&[long], "/task-a/coder", 80, GlyphSet::Unicode);
    assert_eq!(
        cut[1], "   claude-work · subscription",
        "the mail goes first"
    );
    let looks = panel(&rows, "/task-a/coder", 120, GlyphSet::Unicode);
    let look = |row: usize, text: &str| {
        let span = looks[row].spans.iter().find(|s| s.text == text).unwrap();
        span.look
    };
    assert_eq!(look(0, "●"), Look::fg(Token::Working));
    assert_eq!(look(0, "coder"), Look::fg(Token::Text).bold());
    assert_eq!(look(0, "41k/850k"), Look::fg(Token::TextMuted));
    assert_eq!(
        look(1, "task from task-a, 2 min ago"),
        Look::fg(Token::TextFaint)
    );
    let mut done = coder();
    done.entry.state = AgentState::Idle {
        outcome: Some(IdleOutcome::Reported),
    };
    done.entry.activity = wisp_protocol::Activity::Idle;
    done.last_mail = None;
    let done = shown(&[done], "/task-a/coder", 120, GlyphSet::Unicode);
    assert_eq!(
        done,
        [
            " ✓ coder   sonnet-5-5 high   idle 2m 14s: reported   ~$0.08   41k/850k   todo 2/5",
            "   claude-work · subscription",
        ]
    );
    let mut waiting = coder();
    waiting.entry.state = AgentState::Idle {
        outcome: Some(IdleOutcome::Waiting),
    };
    waiting.entry.since_ms = Some(NOW - 2_467_000);
    let waiting = shown(&[waiting], "/task-a/coder", 40, GlyphSet::Unicode);
    assert_eq!(waiting[0], " ○ coder  idle 41m 07s: waiting  ~$0.08");
    let mut edited = coder();
    edited.entry.preset_change = Some(wisp_protocol::PresetChange::Removed);
    assert_eq!(
        shown(&[edited.clone()], "/task-a/coder", 120, GlyphSet::Unicode)[1],
        "   claude-work · subscription · preset removed · task from task-a, 2 min ago"
    );
    assert_eq!(
        shown(&[edited], "/task-a/coder", 56, GlyphSet::Unicode)[1],
        "   sonnet-5-5 high · claude-work · preset removed"
    );
    let mut warm = coder();
    warm.entry.keep_warm = Some(wisp_protocol::KeepWarm {
        refreshes: 2,
        limit: 8,
        paused: false,
    });
    assert_eq!(
        shown(&[warm.clone()], "/task-a/coder", 120, GlyphSet::Unicode)[1],
        "   claude-work · subscription · keep-warm: 2 of 8 refreshes · task from task-a, 2 min ago"
    );
    assert_eq!(
        shown(&[warm], "/task-a/coder", 80, GlyphSet::Unicode)[1],
        "   claude-work · subscription · keep-warm: 2 of 8 refreshes",
        "the mark shows whole and the mail leaves"
    );
    let mut untimed = coder();
    untimed.entry.state = AgentState::Idle { outcome: None };
    untimed.entry.since_ms = None;
    let untimed = shown(&[untimed], "/task-a/coder", 40, GlyphSet::Unicode);
    assert_eq!(untimed[0], " ■ coder  idle  ~$0.08");
}

#[test]
fn main_s_panel_is_its_model_state_and_cost_over_its_account() {
    let rows = [coder()];
    assert_eq!(
        shown(&rows, "/", 120, GlyphSet::Unicode),
        [
            " ⠶ main   test-model low   idle   $0.42",
            "   claude-api · API"
        ]
    );
    assert_eq!(
        shown(&rows, "/", 40, GlyphSet::Ascii),
        [" * main  idle  $0.42", "   test-model low - claude-api"]
    );
    let main = MainRow {
        keep_warm: Some(wisp_protocol::KeepWarm {
            refreshes: 0,
            limit: 8,
            paused: true,
        }),
        ..main_on_api()
    };
    let view = AgentsView {
        main: &main,
        rows: &rows,
        t: Duration::ZERO,
        now_ms: NOW,
    };
    let layout = Layout::new(120, GlyphSet::Unicode);
    assert_eq!(
        line_text(&panel_rows(&view, &AgentPath::root(), layout)[1]),
        "   claude-api · API · keep-warm: paused until its next turn"
    );
    let unknown = shown(&rows, "/task-b", 120, GlyphSet::Unicode);
    assert_eq!(
        unknown,
        [String::new(), String::new()],
        "a row not here yet"
    );
}

#[test]
fn the_panel_hides_a_key_shaped_model_and_reds_a_context_past_its_limit_while_nothing_compacts() {
    for (auto_compact, look) in [
        (true, Look::fg(Token::Text).bold()),
        (false, Look::fg(Token::Error).bold()),
    ] {
        let mut row = coder();
        row.entry.model = "sk-ant-api03-CANARYMODEL0000".into();
        row.entry.context = Some(wisp_protocol::ContextFill {
            used: 450_000,
            window: Some(1_000_000),
            limit: Some(400_000),
            estimated: false,
            auto_compact,
        });
        let lines = panel(&[row], "/task-a/coder", 120, GlyphSet::Unicode);
        let text = line_text(&lines[0]);
        assert!(
            text.contains("(looks like a key, not shown) high"),
            "{text}"
        );
        assert!(!text.contains("CANARY"), "{text}");
        let figure = lines[0].spans.iter().find(|s| s.text == "450k/400k");
        assert_eq!(figure.unwrap().look, look, "{auto_compact}");
    }
}

#[test]
fn a_long_account_name_is_cut_and_its_billing_word_never_is() {
    let long = "claude-subscription-for-the-whole-team-and-then-some-more-of-it-x";
    let mut main = main_on_api();
    main.accounts
        .push(account(long, wisp_protocol::Billing::AnthropicPlan));
    let rows = [agent("/coder", long, "claude-opus-5-5", 0.4)];
    let view = AgentsView {
        main: &main,
        rows: &rows,
        t: Duration::ZERO,
        now_ms: NOW,
    };
    for cols in [60, 80] {
        let layout = Layout::new(cols, GlyphSet::Unicode);
        let text = line_text(&panel_rows(&view, &AgentPath::new("/coder"), layout)[1]);
        assert!(text.starts_with("   claude-subscription-"), "{text}");
        assert!(text.ends_with("\u{2026} \u{b7} subscription"), "{text}");
        assert!(text.chars().count() < cols, "{text}");
    }
}

#[test]
fn the_page_ends_with_its_rule_panel_and_whole_keys_at_its_room() {
    let rows = [coder()];
    for (cols, set, moves, arrows) in [
        (80, GlyphSet::Unicode, copy::Moves::Around, "←↑↓→ move"),
        (40, GlyphSet::Unicode, copy::Moves::Around, "←↑↓→ move"),
        (80, GlyphSet::Ascii, copy::Moves::Around, "arrows move"),
        (80, GlyphSet::Unicode, copy::Moves::UpDown, "↑↓ move"),
        (80, GlyphSet::Ascii, copy::Moves::UpDown, "arrows move"),
    ] {
        let layout = Layout::new(cols, set);
        let keys = AgentKeys {
            opens: true,
            release: true,
            moves,
        };
        let panel = panel(&rows, "/task-a/coder", cols, set);
        let body = agents_note(copy::NO_AGENTS);
        let short = agents_page(body.clone(), panel.clone(), keys, 5, layout);
        let short: Vec<String> = short.iter().map(line_text).collect();
        assert_eq!(short.len(), 5, "{short:#?}");
        assert_eq!(short[0].trim(), copy::AGENTS_TITLE);
        assert!(short[2].contains("coder"), "{short:#?}");
        let page = agents_page(body, panel, keys, 19, layout);
        let text: Vec<String> = page.iter().map(line_text).collect();
        assert_eq!(text.len(), 19, "{text:#?}");
        assert_eq!(text[0].trim(), copy::AGENTS_TITLE);
        assert_eq!(text[2].trim(), copy::NO_AGENTS);
        assert!(
            text[15].trim().chars().all(|c| c == '─' || c == '-'),
            "{text:#?}"
        );
        assert!(text[16].contains("coder"), "{text:#?}");
        let keys = &text[18];
        assert!(keys.starts_with(" enter open"), "{keys}");
        assert!(keys.contains(arrows) || cols == 40, "{keys}");
        assert!(keys.trim_end().ends_with("esc close"), "{keys}");
        assert!(
            !keys.contains('…') && keys.chars().count() <= cols,
            "{keys}"
        );
    }
}

#[test]
fn the_keep_warm_words_here_are_the_words_the_parent_agent_reads() {
    for paused in [false, true] {
        let (refreshes, limit) = (3, 8);
        assert_eq!(
            copy::keep_warm_fact(wisp_protocol::KeepWarm {
                refreshes,
                limit,
                paused
            }),
            wisp_tool_text::agents::keep_warm(&wisp_plugin_api::host::WarmRow {
                refreshes,
                limit,
                paused
            })
        );
    }
}

#[test]
fn the_list_names_every_live_agent_under_its_parent() {
    use wisp_protocol::IdleOutcome::{Failed, Reported, Waiting};

    let long = "abcdefghijklmnopqrstuvwxyz0123456789";
    let nested = format!("/branch/seed/{long}");
    let mut rows = ["/branch", "/branch/seed", &nested, "/stone"]
        .map(|path| agent(path, "claude-api", "model", 0.0));
    rows[0].entry.state = AgentState::Running;
    rows[1].entry.name = "sprout".into();
    for (row, outcome) in rows[1..].iter_mut().zip([Waiting, Reported, Failed]) {
        row.entry.state = AgentState::Idle {
            outcome: Some(outcome),
        };
    }
    let listed = || std::iter::once(None).chain(rows.iter().map(Some));
    let picked = AgentPath::new("/branch/seed");
    for (set, rows, more) in [
        (
            GlyphSet::Unicode,
            " ⠶ main\n ├─● branch\n │ └─○ sprout\n │   └─✓ {}\n └─⨯ stone",
            "…",
        ),
        (
            GlyphSet::Ascii,
            " * main\n |-@ branch\n | `-o sprout\n |   `-+ {}\n `-x stone",
            "...",
        ),
    ] {
        for (cols, room) in [(120, 29), (80, 15), (60, 9)] {
            let layout = Layout::new(cols, set);
            let view = agents_tree(listed(), &picked, 8, layout);
            let expected = rows.replace(
                "{}",
                &format!("{}{more}", &long[..room - more.chars().count()]),
            );
            let actual = view
                .lines
                .iter()
                .map(line_text)
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(actual, expected, "{layout:?}");
            assert_eq!(list_width(listed(), layout), cols / 3);
            assert_eq!(view.lines[2].band.unwrap().ink, Token::AccentTint.into());
            assert_eq!(view.shown[2], picked);
        }
        let narrow = Layout::new(59, set);
        assert_eq!(list_width(listed(), narrow), 0);
        assert!(agents_tree(listed(), &picked, 8, narrow).lines.is_empty());
    }

    let short = ["/a", "/a/b", "/a/db"].map(|path| {
        let mut row = agent(path, "claude-api", "model", 0.0);
        row.entry.state = AgentState::Running;
        row
    });
    for set in [GlyphSet::Ascii, GlyphSet::AmbiguousWide] {
        let listed = std::iter::once(None).chain(short.iter().map(Some));
        let layout = Layout::new(80, set);
        let view = agents_tree(listed, &AgentPath::root(), 8, layout);
        let rows = view.lines[2..].iter().map(line_text).collect::<Vec<_>>();
        assert_eq!(rows, ["   |-@ b", "   `-@ db"], "{layout:?}");
    }
}

#[test]
fn a_long_list_keeps_the_picked_row_in_view() {
    let rows: Vec<_> = (0..6)
        .map(|i| agent(&format!("/leaf-{i}"), "claude-api", "model", 0.0))
        .collect();
    let listed = || std::iter::once(None).chain(rows.iter().map(Some));
    let layout = Layout::new(80, GlyphSet::Unicode);
    for (picked, shown, markers) in [
        ("/", ["/", "/leaf-0", "/leaf-1"], [false, true]),
        ("/leaf-2", ["/leaf-1", "/leaf-2", "/leaf-3"], [true, true]),
        ("/leaf-5", ["/leaf-3", "/leaf-4", "/leaf-5"], [true, false]),
    ] {
        let picked = AgentPath::new(picked);
        let view = agents_tree(listed(), &picked, 3, layout);
        assert_eq!(view.shown, shown.map(AgentPath::new));
        assert_eq!(
            [
                line_text(&view.lines[0]).ends_with('▴'),
                line_text(&view.lines[2]).ends_with('▾')
            ],
            markers
        );
        let i = view.shown.iter().position(|p| p == &picked).unwrap();
        assert!(view.lines[i].band.is_some(), "{picked:?}");
    }
}
