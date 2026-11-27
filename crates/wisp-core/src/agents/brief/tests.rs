use wisp_tool_text::agents::agent_lines_in;

use super::*;

fn environment_lines(
    path: &AgentPath,
    spawns: bool,
    at_limit: Option<u32>,
    presets: &[&Preset],
    instructions: Option<&str>,
    background: bool,
) -> Vec<String> {
    let mut lines = path_lines(path);
    lines.extend(other_lines(
        path,
        spawns,
        at_limit,
        presets,
        instructions,
        background,
    ));
    lines
}

fn preset(name: &str, description: &str) -> Preset {
    Preset {
        name: name.into(),
        description: description.into(),
        model: None,
        effort: None,
        preload: None,
        tools_off: Vec::new(),
        spawn: None,
        instructions: None,
        endpoint: None,
        fallback: Vec::new(),
        worker: None,
    }
}

#[test]
fn the_brief_holds_the_task_and_the_checklist() {
    assert_eq!(
        brief("  Review the change.\n", &[]),
        ("Review the change.".to_owned(), Vec::new())
    );
    let (text, spans) = brief(
        " Review the change. ",
        &[
            " check types ".into(),
            "read café\n- [ ] still one step".into(),
            " ".into(),
            "".into(),
        ],
    );
    assert_eq!(
        text,
        "Review the change.\n\nChecklist:\n- [ ] check types\n- [ ] read café\n- [ ] still one step\n- [ ] \n- [ ] "
    );
    assert_eq!(
        spans
            .iter()
            .map(|span| &text[span.start..span.end])
            .collect::<Vec<_>>(),
        ["check types", "read café\n- [ ] still one step", "", ""]
    );
    assert!(spans.windows(2).all(|pair| pair[0].end < pair[1].start));
}

#[test]
fn a_child_names_itself_and_its_parent_and_the_presets_it_may_use() {
    let reviewer = preset("reviewer", "Reviews a change");
    let coder = preset("coder", "Writes code");
    let presets = [&coder, &reviewer];
    assert_eq!(
        environment_lines(
            &AgentPath::new("/review"),
            true,
            None,
            &presets,
            None,
            false
        ),
        [
            "Agent: /review",
            "Parent: main",
            "Presets for spawn_agent: coder (Writes code); reviewer (Reviews a change)",
        ]
    );
    assert_eq!(
        environment_lines(
            &AgentPath::new("/review/planner"),
            true,
            Some(2),
            &presets,
            Some("~/roles/planner/AGENTS.md"),
            true
        ),
        [
            "Agent: /review/planner",
            "Parent: /review",
            "Preset instructions: ~/roles/planner/AGENTS.md",
            BACKGROUND_LINE,
            "Spawning: you are at the depth limit (2), so spawn_agent refuses",
        ]
    );
    assert_eq!(
        environment_lines(&AgentPath::new("/docs"), false, None, &presets, None, false),
        ["Agent: /docs", "Parent: main"]
    );
    assert_eq!(
        environment_lines(&AgentPath::new("/docs"), false, None, &presets, None, true),
        ["Agent: /docs", "Parent: main", BACKGROUND_LINE]
    );
    assert_eq!(
        environment_lines(&AgentPath::root(), true, None, &presets, None, false),
        ["Presets for spawn_agent: coder (Writes code); reviewer (Reviews a change)"]
    );
    assert!(environment_lines(&AgentPath::root(), true, None, &[], None, false).is_empty());
    assert_eq!(
        path_lines(&AgentPath::new("/review/planner")),
        ["Agent: /review/planner", "Parent: /review"]
    );
    assert!(path_lines(&AgentPath::root()).is_empty());
}

#[test]
fn the_presets_line_names_each_presets_account_and_model_and_never_a_kind_or_a_key() {
    let coder = Preset {
        fallback: vec![wisp_protocol::agents::Choice {
            endpoint: "other".into(),
            model: None,
            effort: None,
        }],
        endpoint: Some("claude-work".into()),
        model: Some("claude-opus-5-5".into()),
        ..preset("coder", "Writes the change it is given")
    };
    let reviewer = Preset {
        endpoint: Some("chatgpt".into()),
        ..preset("reviewer", "Reviews a change it did not write")
    };
    let helper = Preset {
        model: Some("claude-sonnet-5-5".into()),
        ..preset("helper", "Small chores")
    };
    let keyed = Preset {
        endpoint: Some("sk-ant-api03-CANARY0123456789".into()),
        ..preset("keyed", "Odd")
    };
    assert_eq!(
        presets_line(&[&coder, &reviewer, &helper, &keyed]).unwrap(),
        "Presets for spawn_agent: coder (Writes the change it is given; on claude-work, with claude-opus-5-5); \
         reviewer (Reviews a change it did not write; on chatgpt); \
         helper (Small chores; with claude-sonnet-5-5); \
         keyed (Odd; on (not shown))"
    );
}

#[test]
fn durations_and_facts_read_short() {
    let path = AgentPath::new("/review/planner");
    let facts = Facts {
        path: &path,
        duration_ms: 372_000,
        steps: 31,
        spent: Some(Spent {
            usd: 0.84,
            lower_bound: false,
            billing: wisp_protocol::Billing::Metered,
            plan_usd: None,
        }),
    };
    assert_eq!(
        reported_text(&facts, "Done.\nMore."),
        "/review/planner reported · 6m12s · 31 steps this turn · $0.84 API\nDone.\nMore."
    );
    let partial = Facts {
        spent: Some(Spent {
            usd: 0.84,
            lower_bound: true,
            billing: wisp_protocol::Billing::Metered,
            plan_usd: None,
        }),
        ..facts
    };
    assert!(
        reported_text(&partial, "Done.").contains(" · $0.84+ API\n"),
        "{}",
        reported_text(&partial, "Done.")
    );
    let unpriced = Facts {
        spent: None,
        steps: 1,
        ..facts
    };
    assert_eq!(
        reported_text(&unpriced, "Done."),
        "/review/planner reported · 6m12s · 1 step this turn\nDone."
    );
    assert!(no_report_text(&facts, " ").ends_with(&format!("\n{}", crate::provider::NO_TEXT)));
    assert_eq!(
        no_report_text(&facts, "Still going"),
        "/review/planner ended without a report (no_report; its last text follows) · 6m12s · 31 steps this turn · $0.84 API\nStill going"
    );
    assert_eq!(
        stopped_text(&path, StopCause::WithParent),
        "/review/planner was stopped with its parent."
    );
    assert_eq!(
        failed_text(&path, ErrorKind::Overloaded, None, 1_800_000_000),
        "/review/planner failed (overloaded)."
    );
    let now = 1_800_000_000;
    assert_eq!(
        failed_text(
            &path,
            ErrorKind::RateLimit,
            Some((
                "example",
                now + 508_200,
                wisp_tool_text::agents::Cause::Limit
            )),
            now
        ),
        "/review/planner failed (rate_limit): example reached its usage limit, resets in 5d21h."
    );
    for at in [now - 1, now] {
        assert_eq!(
            failed_text(
                &path,
                ErrorKind::RateLimit,
                Some(("example", at, wisp_tool_text::agents::Cause::Limit)),
                now
            ),
            "/review/planner failed (rate_limit)."
        );
    }
    assert_eq!(
        failed_text(&path, ErrorKind::RateLimit, None, now),
        "/review/planner failed (rate_limit)."
    );
}

#[test]
fn a_report_over_16_kib_is_cut_at_a_line_or_a_character_and_names_its_file() {
    assert_eq!(cut_report("short", None), "short");
    let line = format!("{}\n", "a".repeat(99));
    let lines = line.repeat(400);
    let cut = cut_report(&lines, Some("~/out/report-1.md"));
    let (kept, note) = cut.rsplit_once('\n').unwrap();
    assert_eq!(note, "[cut: the full report is in ~/out/report-1.md]");
    assert!(kept.len() <= MAX_REPORT_BYTES);
    assert!(lines.starts_with(kept));
    assert!(kept.ends_with(&"a".repeat(99)));
    assert_eq!(kept.len() % 100, 99);
    let single = "é".repeat(20 * 1024);
    let cut = cut_report(&single, None);
    let (kept, note) = cut.rsplit_once('\n').unwrap();
    assert_eq!(note, "[cut: the full report is in the agent's log]");
    assert!(kept.len() <= MAX_REPORT_BYTES && kept.len() > MAX_REPORT_BYTES - 2);
    assert!(single.starts_with(kept));
}

#[test]
fn the_wait_notice_names_each_process_and_ends_with_the_last_text() {
    let review = AgentPath::new("/review");
    let titled = ["3 (Placeholder title)".to_owned()];
    assert_eq!(
        waiting_text(&review, &titled, "Placeholder last text line"),
        "/review waits for its process 3 (Placeholder title) to end and will report then. If that process won't end by itself, message /review to report now. Its last text follows.\nPlaceholder last text line"
    );
    assert_eq!(
        waiting_text(&review, &["3".to_owned()], ""),
        "/review waits for its process 3 to end and will report then. If that process won't end by itself, message /review to report now."
    );
    let several = ["4 (Placeholder one)".to_owned(), "7".to_owned()];
    assert_eq!(
        waiting_text(&AgentPath::new("/docs"), &several, "Placeholder"),
        "/docs waits for its processes 4 (Placeholder one) and 7 to end and will report then. If one of them won't end by itself, message /docs to report now. Its last text follows.\nPlaceholder"
    );
    assert_eq!(
        unwoken_text(&AgentPath::new("/docs")),
        "/docs won't be woken by its processes from before Wisp restarted; message it to continue."
    );
    assert!(retrying_text(&review, 1).contains("has waited 1 minute to retry"));
    assert!(retrying_text(&review, 40).contains("has waited 40 minutes to retry"));
}

#[test]
fn every_environment_line_is_read_back_as_an_agent_line() {
    let reviewer = preset("reviewer", "Reviews a change");
    let presets = [&reviewer];
    for (path, spawns, limit) in [
        ("/review", true, None),
        ("/review/planner", true, Some(2)),
        ("/docs", false, None),
    ] {
        let lines = environment_lines(
            &AgentPath::new(path),
            spawns,
            limit,
            &presets,
            Some("~/roles/x.md"),
            true,
        );
        let block = format!(
            "<environment>\nWorking directory: ~/w\n{}\n</environment>",
            lines.join("\n")
        );
        assert_eq!(agent_lines_in(&block), lines, "{path}");
    }
}

#[test]
fn report_spend_separates_plan_coverage_and_real_charges() {
    let path = AgentPath::new("/child");
    for (billing, plan_usd, expected) in [
        (
            wisp_protocol::Billing::ChatGptPlan,
            Some(0.031),
            "~$0.031 subscription",
        ),
        (
            wisp_protocol::Billing::AnthropicPlan,
            Some(0.031),
            "~$0.031 subscription",
        ),
        (
            wisp_protocol::Billing::Mixed,
            Some(0.021),
            "$0.01 API · ~$0.021 subscription",
        ),
    ] {
        let facts = Facts {
            path: &path,
            duration_ms: 1,
            steps: 1,
            spent: Some(Spent {
                usd: 0.031,
                lower_bound: false,
                billing,
                plan_usd,
            }),
        };
        assert!(reported_text(&facts, "done").contains(expected));
        assert!(no_report_text(&facts, "done").contains(expected));
    }
}

#[test]
fn plan_lower_bounds_match_the_agent_list_formatter() {
    let path = AgentPath::new("/child");
    let facts = Facts {
        path: &path,
        duration_ms: 0,
        steps: 1,
        spent: Some(Spent {
            usd: 0.031,
            lower_bound: true,
            billing: wisp_protocol::Billing::ChatGptPlan,
            plan_usd: Some(0.031),
        }),
    };
    let expected = "~$0.031+ subscription";
    assert!(reported_text(&facts, "done").contains(expected));
    assert_eq!(
        wisp_tool_text::agents::spend(
            31_000,
            true,
            None,
            Some(wisp_tool_text::agents::SUBSCRIPTION_NOTE)
        ),
        expected
    );
}

#[test]
fn a_fallback_notice_names_only_the_landing_and_reason_with_api_first() {
    use wisp_tool_text::agents::Fallback;
    let reason = "gemini-sub reached its usage limit for gemini-4-carbon, resets in 3d19h";
    let moved = |rebuild| Fallback::Moved { rebuild };
    for (how, account, model, api, expected) in [
        (
            Fallback::Started,
            "chatgpt-sub",
            "gpt-6-astra",
            false,
            "started on fallback chatgpt-sub with gpt-6-astra high",
        ),
        (
            Fallback::Started,
            "openai-api",
            "gpt-6.1-sol",
            true,
            "API: started on fallback openai-api with gpt-6.1-sol high",
        ),
        (
            moved(Some("~$0.12")),
            "gemini-sub",
            "gemini-3.8-flash",
            false,
            "moved to gemini-sub with gemini-3.8-flash high (cache rebuild ~$0.12)",
        ),
        (
            moved(Some("~$0.38")),
            "openai-api",
            "gpt-6.1-sol",
            true,
            "API: moved to openai-api with gpt-6.1-sol high (cache rebuild ~$0.38)",
        ),
        (
            moved(None),
            "gemini-sub",
            "gemini-3.8-flash",
            false,
            "moved to gemini-sub with gemini-3.8-flash high",
        ),
    ] {
        assert_eq!(
            fallback_text(how, account, model, Effort::High, api, reason),
            format!("{expected}: {reason}.")
        );
    }
}
