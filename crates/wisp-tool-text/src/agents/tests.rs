use wisp_plugin_api::host::{
    AgentRow, Delivery, Progress, Relation, Sent, StartedAgent, Stopped, WarmMark, WarmRow,
};

use super::*;

fn row(path: &str, relation: Relation) -> AgentRow {
    AgentRow {
        path: path.to_owned(),
        session: format!("s{}", path.replace('/', "-")),
        relation,
        preset: None,
        model: "claude-opus-5-5".to_owned(),
        effort: "high".to_owned(),
        endpoint: None,
        provider: None,
        state: None,
        detail: None,
        billing_note: None,
        plan_micros: None,
        spent_micros: None,
        lower_bound: false,
        priced: true,
        checklist: None,
        last_report: None,
        cwd: None,
        running_ms: None,
        context_used: None,
        context_window: None,
        context_limit: None,
        context_estimated: false,
        keep_warm: None,
    }
}

#[test]
fn the_started_line_names_the_agent_and_says_not_to_poll() {
    let line = started(
        &StartedAgent {
            path: "/review/planner".to_owned(),
            model: "claude-opus-5-5".to_owned(),
            effort: "high".to_owned(),
            endpoint: "gpt".to_owned(),
            provider: "ChatGPT subscription".to_owned(),
            cwd: "~/Projects/Wisp-worktrees/review".to_owned(),
            keep_warm: None,
            fell_back: None,
        },
        None,
    );
    assert_eq!(
        line,
        "Started /review/planner on gpt (ChatGPT subscription) with claude-opus-5-5, high, in ~/Projects/Wisp-worktrees/review. Its report will arrive as a message; don't poll it, and end your turn unless you have other independent work."
    );
}

#[test]
fn the_list_shows_each_fact_it_has_and_nothing_for_an_empty_tree() {
    assert_eq!(listed(&[]), NO_AGENTS);
    let parent = row("main", Relation::Parent);
    let sibling = AgentRow {
        preset: Some("reviewer".to_owned()),
        state: Some("idle".to_owned()),
        detail: Some("reported".to_owned()),
        billing_note: None,
        plan_micros: None,
        spent_micros: Some(840_000),
        running_ms: Some(372_000),
        checklist: Some(Progress {
            completed: 2,
            total: 5,
        }),
        last_report: Some("Found 2 issues".to_owned()),
        cwd: Some("~/w".to_owned()),
        ..row("/coder", Relation::Sibling)
    };
    let child = AgentRow {
        state: Some("running".to_owned()),
        billing_note: None,
        plan_micros: None,
        spent_micros: Some(1_200),
        running_ms: Some(4_500),
        lower_bound: true,
        ..row("/review/x", Relation::Child)
    };
    let below = row("/review/x/y", Relation::Descendant);
    let me = AgentRow {
        preset: Some("planner".to_owned()),
        state: Some("running".to_owned()),
        detail: Some("using list_agents".to_owned()),
        billing_note: None,
        plan_micros: None,
        spent_micros: Some(120_000),
        running_ms: Some(62_000),
        cwd: Some("~/w".to_owned()),
        ..row("/review", Relation::Caller)
    };
    assert_eq!(
        listed(&[me, parent, sibling, child, below]),
        "You: /review · session s-review · preset planner · claude-opus-5-5, high · ran 1m02s · $0.12 API · in ~/w\n\
         4 agents you can reach:\n\
         - main (your parent) · session smain · claude-opus-5-5, high\n\
         - /coder (sibling) · session s-coder · preset reviewer · claude-opus-5-5, high · idle: reported · ran 6m12s · $0.84 API · todos 2 of 5 completed · in ~/w\n  Last report: Found 2 issues\n\
         - /review/x (your child) · session s-review-x · claude-opus-5-5, high · running · ran 4s · $0.0012+ API\n\
         - /review/x/y (below your child) · session s-review-x-y · claude-opus-5-5, high"
    );
    assert!(listed(&[row("/a", Relation::Child)]).starts_with("1 agent you can reach:\n"));
    assert_eq!(
        listed(&[row("main", Relation::Caller)]),
        format!("You: main · session smain · claude-opus-5-5, high\n{NO_AGENTS}")
    );
}

#[test]
fn the_report_line_is_the_first_non_empty_line_cut_at_200_characters() {
    assert_eq!(report_line(""), None);
    assert_eq!(
        report_line("\n  \nDone: 3 files\nmore"),
        Some("Done: 3 files".to_owned())
    );
    let long = "é".repeat(250);
    assert_eq!(report_line(&long).map(|l| l.chars().count()), Some(200));
}

#[test]
fn spawn_todos_are_checked_like_a_checklist() {
    assert_eq!(check_todos(&[]), Ok(()));
    assert_eq!(check_todos(&["Read".to_owned()]), Ok(()));
    let many: Vec<String> = (0..51).map(|i| format!("step {i}")).collect();
    assert_eq!(
        check_todos(&many),
        Err("`todos` has 51 items; give at most 50".to_owned())
    );
    assert_eq!(
        check_todos(&["a".to_owned(), "  ".to_owned()]),
        Err("`todos` item 2 is blank".to_owned())
    );
    assert_eq!(
        check_todos(&["x".repeat(501)]),
        Err("`todos` item 1 is 501 characters; the limit is 500".to_owned())
    );
}

#[test]
fn the_sent_line_says_when_the_recipient_reads_it() {
    let cases = [
        (
            Delivery::Running,
            "Sent to /review/coder (running: it reads this at its next step, or when it next runs if this turn ends first).",
        ),
        (Delivery::Woken, "Sent to /review/coder (idle: woken)."),
        (
            Delivery::Idle,
            "Sent to /review/coder (idle: it reads this when it next runs).",
        ),
        (
            Delivery::Queued,
            "Sent to /review/coder (queued: it reads this when it starts).",
        ),
        (
            Delivery::Held,
            "Sent to /review/coder (held: it reads this when it next runs).",
        ),
    ];
    for (delivery, want) in cases {
        let line = sent(&Sent {
            to: "/review/coder".to_owned(),
            delivery,
        });
        assert_eq!(line, want, "{delivery:?}");
    }
}

#[test]
fn the_stopped_line_counts_the_agents_below_and_says_what_ending_means() {
    let line = |below, ended| {
        stopped(&Stopped {
            path: "/review".to_owned(),
            below,
            ended,
        })
    };
    assert_eq!(
        line(0, false),
        "Stopped /review. It stays idle and keeps its context; message it to go on."
    );
    assert_eq!(
        line(1, false),
        "Stopped /review and 1 agent below it. It stays idle and keeps its context; message it to go on."
    );
    assert_eq!(
        line(2, true),
        "Ended /review and 2 agents below it. Ended agents take no more messages and no longer count toward the agent limit."
    );
}

#[test]
fn end_refused_names_the_reason_and_how_to_proceed() {
    assert_eq!(
        end_refused("/w", None, EndRefusal::Running),
        "/w was still running and hasn't reported yet; its report arrives as a message. To end it anyway, stop it first (stop_agent with end: false) and end it in a later reply."
    );
    assert_eq!(
        end_refused("/lead", Some("/lead/w"), EndRefusal::Queued),
        "/lead/w below /lead was queued to run and hasn't reported yet. To end /lead anyway, stop it first (stop_agent with end: false) and end it in a later reply."
    );
    assert_eq!(
        end_refused("/w", None, EndRefusal::UnreadReport),
        "/w's report is waiting for you and arrives as a message. Read it before ending /w in a later reply."
    );
}

#[test]
fn the_running_time_reads_in_seconds_minutes_or_hours() {
    assert_eq!(ran(999), "0s");
    assert_eq!(ran(59_000), "59s");
    assert_eq!(ran(372_000), "6m12s");
    assert_eq!(ran(3_723_000), "1h02m");
}

#[test]
fn a_row_shows_its_context_fill_after_its_spend_in_whole_thousands() {
    let child = AgentRow {
        state: Some("stopping".to_owned()),
        detail: Some("bash".to_owned()),
        billing_note: None,
        plan_micros: None,
        spent_micros: Some(120_000),
        context_used: Some(312_400),
        context_window: Some(1_000_000),
        context_limit: Some(400_000),
        cwd: Some("~/w".to_owned()),
        ..row("/review", Relation::Child)
    };
    let resumed = AgentRow {
        context_used: Some(48_700),
        context_window: Some(200_000),
        context_estimated: true,
        ..row("/docs", Relation::Child)
    };
    let unknown = AgentRow {
        context_used: Some(312_000),
        ..row("/odd", Relation::Child)
    };
    let me = AgentRow {
        context_used: Some(900),
        context_window: Some(1_000_000),
        context_limit: Some(400_000),
        ..row("main", Relation::Caller)
    };
    assert_eq!(
        listed(&[me, resumed, child, unknown]),
        "You: main · session smain · claude-opus-5-5, high · context 900 of 1M, limit 400k\n\
         3 agents you can reach:\n\
         - /docs (your child) · session s-docs · claude-opus-5-5, high · context about 49k of 200k\n\
         - /review (your child) · session s-review · claude-opus-5-5, high · stopping: bash · $0.12 API · context 312k of 1M, limit 400k · in ~/w\n\
         - /odd (your child) · session s-odd · claude-opus-5-5, high · context 312k"
    );
    assert_eq!(tokens(999_499), "999k");
    assert_eq!(tokens(999_500), "1M");
    assert_eq!(tokens(1_450_000), "1.5M");
}

fn planner(keep_warm: Option<WarmMark>) -> StartedAgent {
    StartedAgent {
        path: "/planner".to_owned(),
        model: "claude-opus-5-5".to_owned(),
        effort: "high".to_owned(),
        endpoint: "direct".to_owned(),
        provider: "anthropic".to_owned(),
        cwd: "~/w".to_owned(),
        keep_warm,
        fell_back: None,
    }
}

#[test]
fn a_kept_warm_start_names_the_limit_or_says_it_is_off() {
    let opening = "Started /planner on direct (anthropic) with claude-opus-5-5, high, in ~/w. Its report will arrive as a message; don't poll it, and end your turn unless you have other independent work.";
    assert_eq!(
        started(&planner(Some(WarmMark::Limit(8))), None),
        format!(
            "{opening} While it idles, Wisp refreshes its prompt cache up to 8 times per idle spell."
        )
    );
    assert_eq!(
        started(&planner(Some(WarmMark::Limit(1))), None),
        format!(
            "{opening} While it idles, Wisp refreshes its prompt cache up to 1 time per idle spell."
        )
    );
    assert_eq!(
        started(&planner(Some(WarmMark::Limit(0))), None),
        format!("{opening} keep_warm is off in this session (agents.warm_refreshes is 0).")
    );
    assert_eq!(started(&planner(None), None), opening);
    let gpt = StartedAgent {
        endpoint: "gpt".to_owned(),
        provider: "openai".to_owned(),
        model: "gpt-6.1-sol".to_owned(),
        ..planner(Some(WarmMark::Unsupported))
    };
    assert_eq!(
        started(&gpt, None),
        "Started /planner on gpt (openai) with gpt-6.1-sol, high, in ~/w. Its report will arrive as a message; don't poll it, and end your turn unless you have other independent work. keep_warm does nothing on account gpt: Wisp doesn't refresh this provider's cache."
    );
}

#[test]
fn a_kept_warm_row_shows_its_refreshes_or_its_pause_after_the_spend() {
    let live = AgentRow {
        billing_note: None,
        plan_micros: None,
        spent_micros: Some(120_000),
        context_used: Some(48_700),
        keep_warm: Some(WarmRow {
            refreshes: 2,
            limit: 8,
            paused: false,
        }),
        ..row("/planner", Relation::Child)
    };
    let paused = AgentRow {
        keep_warm: Some(WarmRow {
            refreshes: 1,
            limit: 8,
            paused: true,
        }),
        ..row("/coder", Relation::Child)
    };
    let me = AgentRow {
        keep_warm: Some(WarmRow {
            refreshes: 0,
            limit: 2,
            paused: false,
        }),
        ..row("main", Relation::Caller)
    };
    assert_eq!(
        listed(&[me, live, paused]),
        "You: main · session smain · claude-opus-5-5, high · keep-warm: 0 of 2 refreshes\n\
         2 agents you can reach:\n\
         - /planner (your child) · session s-planner · claude-opus-5-5, high · $0.12 API · keep-warm: 2 of 8 refreshes · context 49k\n\
         - /coder (your child) · session s-coder · claude-opus-5-5, high · keep-warm: paused until its next turn"
    );
}

#[test]
fn list_agents_labels_plan_equivalents_instead_of_spend() {
    let mut agent = row("/worker", Relation::Child);
    agent.spent_micros = Some(420_000);
    agent.billing_note = Some(SUBSCRIPTION_NOTE.into());
    let text = listed(&[agent]);
    assert!(text.contains("~$0.42 subscription"), "{text}");
    assert!(!text.contains("API"), "{text}");
    let mut lower = row("/worker", Relation::Child);
    lower.spent_micros = Some(420_000);
    lower.lower_bound = true;
    lower.billing_note = Some(SUBSCRIPTION_NOTE.into());
    let shown = listed(&[lower]);
    assert!(shown.contains("~$0.42+ subscription"), "{shown}");
}

#[test]
fn each_row_names_its_endpoint_and_an_unpriced_one_without_priced_spend_reads_not_priced() {
    let on = |path: &str, relation, endpoint: &str, provider: Option<&str>| AgentRow {
        endpoint: Some(endpoint.to_owned()),
        provider: provider.map(str::to_owned),
        ..row(path, relation)
    };
    let me = on("main", Relation::Caller, "direct", Some("anthropic"));
    let child = AgentRow {
        spent_micros: Some(840_000),
        ..on("/b", Relation::Child, "b", Some("anthropic"))
    };
    let ended = AgentRow {
        state: Some("ended".to_owned()),
        ..on("/old", Relation::Child, "b", None)
    };
    let unpriced = AgentRow {
        priced: false,
        lower_bound: true,
        ..on("/sub", Relation::Child, "sub", Some("anthropic"))
    };
    let unpriced_spent = AgentRow {
        priced: false,
        lower_bound: true,
        spent_micros: Some(1_200),
        ..on("/mixed", Relation::Child, "sub", Some("anthropic"))
    };
    assert_eq!(
        listed(&[me, child, ended, unpriced, unpriced_spent]),
        "You: main · session smain · claude-opus-5-5, high, on direct (anthropic)\n\
         4 agents you can reach:\n\
         - /b (your child) · session s-b · claude-opus-5-5, high, on b (anthropic) · $0.84 API\n\
         - /old (your child) · session s-old · claude-opus-5-5, high, on b · ended\n\
         - /sub (your child) · session s-sub · claude-opus-5-5, high, on sub (anthropic) · not priced\n\
         - /mixed (your child) · session s-mixed · claude-opus-5-5, high, on sub (anthropic) · $0.0012+ API"
    );
}

#[test]
fn mixed_session_spend_is_split_in_list_agents() {
    let mut agent = row("/worker", Relation::Child);
    agent.spent_micros = Some(16_400_000);
    agent.plan_micros = Some(4_100_000);
    agent.billing_note = Some("mixed billing".into());
    agent.lower_bound = true;
    let text = listed(&[agent]);
    assert!(text.contains("$12.30+ API · ~$4.10 subscription"), "{text}");
    assert!(!text.contains("16.40"), "{text}");
}

#[test]
fn two_claude_subscriptions_name_each_agents_own_account_and_kind() {
    let on = |path: &str, relation, endpoint: &str, micros: u64| AgentRow {
        endpoint: Some(endpoint.to_owned()),
        provider: Some("Claude subscription".to_owned()),
        spent_micros: Some(micros),
        billing_note: Some(SUBSCRIPTION_NOTE.to_owned()),
        ..row(path, relation)
    };
    let api = AgentRow {
        endpoint: Some("claude-api".to_owned()),
        provider: Some("Claude API".to_owned()),
        spent_micros: Some(61_000),
        ..row("/scout", Relation::Child)
    };
    let text = listed(&[
        on("/review", Relation::Caller, "claude-subscription", 0),
        on("/review/coder", Relation::Child, "claude-work", 802_000),
        api,
    ]);
    assert!(
        text.contains("claude-opus-5-5, high, on claude-subscription (Claude subscription)"),
        "{text}"
    );
    assert!(
        text.contains(
            "claude-opus-5-5, high, on claude-work (Claude subscription) · ~$0.802 subscription"
        ),
        "{text}"
    );
    assert!(
        text.contains("on claude-api (Claude API) · $0.061 API"),
        "{text}"
    );
    let line = started(
        &StartedAgent {
            endpoint: "claude-work".to_owned(),
            provider: "Claude subscription".to_owned(),
            ..planner(None)
        },
        None,
    );
    assert!(
        line.starts_with("Started /planner on claude-work (Claude subscription) with claude-opus-5-5, high, in ~/w."),
        "{line}"
    );
}

#[test]
fn each_account_kind_names_its_provider_and_its_billing_once() {
    assert_eq!(
        AccountProvider::ALL,
        [
            AccountProvider::Anthropic,
            AccountProvider::OpenAi,
            AccountProvider::Gemini
        ]
    );
    assert_eq!(
        account_kind(AccountProvider::Anthropic, true, false),
        "Claude subscription"
    );
    assert_eq!(
        account_kind(AccountProvider::Anthropic, true, true),
        "Claude subscription"
    );
    assert_eq!(
        account_kind(AccountProvider::OpenAi, true, false),
        "ChatGPT subscription"
    );
    assert_eq!(
        account_kind(AccountProvider::Gemini, true, false),
        "Antigravity subscription"
    );
    assert_eq!(
        account_kind(AccountProvider::Gemini, true, true),
        "Antigravity subscription"
    );
    assert_eq!(
        account_kind(AccountProvider::Anthropic, false, false),
        "Claude API"
    );
    assert_eq!(
        account_kind(AccountProvider::OpenAi, false, false),
        "OpenAI API"
    );
    assert_eq!(
        account_kind(AccountProvider::Gemini, false, false),
        "Gemini API"
    );
    for provider in AccountProvider::ALL {
        assert_eq!(account_kind(provider, false, true), "Gateway, API");
    }
}

#[test]
fn a_used_up_account_names_only_its_scope_and_known_reset() {
    for (model, reset, reason) in [
        (
            Some("gemini-4-carbon"),
            Some("3d19h"),
            " for gemini-4-carbon, resets in 3d19h",
        ),
        (Some("gemini-4-carbon"), None, " for gemini-4-carbon"),
        (None, Some("3d19h"), ", resets in 3d19h"),
        (None, None, ""),
    ] {
        assert_eq!(
            used_up("gemini-sub", model, reset, &Cause::Limit),
            format!(
                "account gemini-sub reached its usage limit{reason}; spawn on another account or model"
            )
        );
    }
}

#[test]
fn limit_reasons_keep_the_evidence_and_its_time_together() {
    let named = Cause::Named("Fable weekly".into());
    let model = Some("claude-fable-5-2");
    let at = Some("5d21h");
    for (cause, reason, failure) in [
        (
            &Cause::Retry,
            "example refused a request for claude-fable-5-2, retry in 5d21h",
            "example refused the request, retry in 5d21h",
        ),
        (
            &named,
            "example reached its Fable weekly limit for claude-fable-5-2, resets in 5d21h",
            "example reached its Fable weekly limit, resets in 5d21h",
        ),
    ] {
        assert_eq!(limit_reason("example", model, at, cause), reason);
        assert_eq!(
            used_up("example", model, at, cause),
            format!("account {reason}; spawn on another account or model")
        );
        assert_eq!(
            failed("/x", "rate_limit", Some(("example", "5d21h", cause))),
            format!("/x failed (rate_limit): {failure}.")
        );
    }
}

#[test]
fn the_agents_doc_quotes_the_system_lines() {
    let docs = include_str!("../../../../docs/agents.md");
    for line in [MESSAGES_SENTENCE, REPORT_SENTENCE, SPAWN_LINE] {
        assert!(docs.contains(line), "{line}");
    }
}
