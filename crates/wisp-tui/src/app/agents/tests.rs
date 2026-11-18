use std::time::Duration;

use serde_json::json;
use wisp_protocol::{Activity, AgentEntry, AgentEvent, AgentPath, AgentState, Effort};

use super::{Agents, report_line};
use crate::events::{End, Ending, turn_ended};

#[test]
fn a_report_line_is_the_first_non_empty_line_of_the_text_or_its_summary() {
    assert_eq!(
        report_line(&json!("\n  Planned it  \nmore")),
        Some("Planned it".to_owned())
    );
    assert_eq!(
        report_line(&json!({ "summary": "Done\nrest" })),
        Some("Done".to_owned())
    );
    assert_eq!(report_line(&json!({ "status": "done" })), None);
    assert_eq!(report_line(&json!("  \n ")), None);
    assert_eq!(report_line(&json!(3)), None);
}

fn entry(path: &str, endpoint: &str) -> AgentEntry {
    let path = AgentPath::new(path);
    AgentEntry {
        billing: Default::default(),
        plan_usd: None,
        name: path.name().to_owned(),
        session_id: String::new(),
        parent: path.parent(),
        preset: None,
        model: "gpt-6-sol".to_owned(),
        effort: Effort::High,
        depth: path.depth(),
        state: AgentState::Idle { outcome: None },
        activity: Activity::Idle,
        spent_usd: 0.0,
        lower_bound: false,
        accounts: Default::default(),
        checklist: None,
        last_report: None,
        cwd: None,
        running_ms: None,
        since_ms: None,
        context: None,
        keep_warm: None,
        endpoint: Some(endpoint.to_owned()),
        provider: Some("openai".to_owned()),
        priced: false,
        path,
        preset_change: None,
    }
}

#[test]
fn the_table_keeps_endpoint_provider_and_priced_from_the_snapshot_and_a_spawn() {
    let facts = crate::app::fixture::endpoint_facts();
    let mut agents = Agents::default();
    let listed = entry("/a", "gpt");
    agents.apply(
        &AgentEvent::AgentsSnapshot {
            request: None,
            agents: vec![listed.clone()],
        },
        0,
        &facts,
    );
    let started = entry("/b", "sub");
    agents.apply(
        &AgentEvent::AgentSpawned {
            request: None,
            session_id: String::new(),
            agent: started.clone(),
            config: wisp_protocol::SpawnChoices::default(),
        },
        0,
        &facts,
    );
    let kept: Vec<AgentEntry> = agents.rows().iter().map(|r| r.entry.clone()).collect();
    assert_eq!(kept, vec![listed.clone(), started]);
    for path in ["/a-b", "/a/c", "/a/b"] {
        agents.apply(
            &AgentEvent::AgentSpawned {
                request: None,
                session_id: String::new(),
                agent: entry(path, "sub"),
                config: wisp_protocol::SpawnChoices::default(),
            },
            0,
            &facts,
        );
    }
    let paths: Vec<&str> = agents
        .rows()
        .iter()
        .map(|r| r.entry.path.as_str())
        .collect();
    assert_eq!(paths, ["/a", "/a/b", "/a/c", "/a-b", "/b"]);
    let spent = wisp_protocol::AccountSpend {
        usd: 0.5,
        plan_usd: Some(0.0),
        lower_bound: true,
    };
    let accounts = std::collections::BTreeMap::from([("gpt".to_owned(), spent)]);
    let change = |agent: &str| AgentEvent::AgentStateChanged {
        plan_usd: None,
        billing: Default::default(),
        agent: AgentPath::new(agent),
        state: listed.state,
        activity: listed.activity.clone(),
        spent_usd: 0.5,
        lower_bound: true,
        accounts: accounts.clone(),
        since_ms: None,
    };
    let first = |agents: &Agents| agents.rows()[0].entry.clone();
    agents.apply(&change("/a/c"), 0, &facts);
    let changed: Vec<&str> = (agents.rows().iter())
        .filter(|r| r.entry.accounts == accounts)
        .map(|r| r.entry.path.as_str())
        .collect();
    assert_eq!(changed, ["/a/c"]);
    agents.apply(&change("/a"), 0, &facts);
    assert_eq!(first(&agents).accounts, accounts);
    let mut later = listed;
    for (usd, want, kept) in [
        (0.25, 0.5, accounts.clone()),
        (0.75, 0.75, Default::default()),
    ] {
        later.spent_usd = usd;
        let snapshot = vec![later.clone()];
        agents.apply(
            &AgentEvent::AgentsSnapshot {
                request: None,
                agents: snapshot,
            },
            0,
            &facts,
        );
        let row = first(&agents);
        assert_eq!(
            (row.spent_usd, row.lower_bound, row.accounts),
            (want, true, kept)
        );
    }
}

#[test]
fn the_clock_counts_from_the_kernels_since_ms_and_reads_an_account_queue_as_running() {
    const NOW: u64 = 1_800_000_074_000;
    let facts = crate::app::fixture::endpoint_facts();
    let mut agents = Agents::default();
    let timed = AgentEntry {
        state: AgentState::Running,
        since_ms: Some(NOW - 74_000),
        ..entry("/a", "gpt")
    };
    let untimed = AgentEntry {
        state: AgentState::Running,
        ..entry("/b", "gpt")
    };
    agents.apply(
        &AgentEvent::AgentsSnapshot {
            request: None,
            agents: vec![timed, untimed],
        },
        0,
        &facts,
    );
    let states = |agents: &Agents| -> Vec<String> {
        let links = agents.links(NOW, " · ").into_iter();
        links.map(|(_, link)| link.state).collect()
    };
    assert_eq!(states(&agents), ["running 1m 14s", "running"]);
    let change = |state, at| AgentEvent::AgentStateChanged {
        plan_usd: None,
        billing: Default::default(),
        agent: AgentPath::new("/a"),
        state,
        activity: Activity::Idle,
        spent_usd: 0.0,
        lower_bound: false,
        accounts: Default::default(),
        since_ms: Some(at),
    };
    let lane = AgentState::Queued {
        reason: wisp_protocol::QueueReason::Endpoint,
    };
    for state in [lane, AgentState::Running] {
        agents.apply(&change(state, NOW - 74_000), 0, &facts);
        assert_eq!(states(&agents), ["running 1m 14s", "running"]);
    }
    agents.apply(&change(AgentState::Stopping, NOW), 0, &facts);
    assert_eq!(states(&agents), ["stopping 0s", "running"]);
}

fn models() -> (super::Models, wisp_render::chrome::Facts) {
    use wisp_protocol::CacheTtl;
    let config = wisp_render::transcript::Config {
        provider: "OpenAI".into(),
        cache_ttl: CacheTtl::OneHour,
        logging: true,
    };
    let facts =
        crate::app::fixture::facts(wisp_render::chrome::Endpoint::Official, CacheTtl::OneHour);
    (super::Models::new(config, None), facts)
}

#[test]
fn child_refusal_uses_its_turn_endpoint_not_a_later_catalogue_selection() {
    use wisp_protocol::ErrorKind;
    use wisp_render::markdown::{Layout, Renderer};
    use wisp_render::theme::glyphs::GlyphSet;
    use wisp_render::transcript::Detail;
    let (mut models, facts) = models();
    let first = entry("/child", "original");
    models.open(&first, &facts);
    let send = |event| AgentEvent::Agent {
        agent: first.path.clone(),
        event: Box::new(event),
    };
    models.apply(&send(AgentEvent::turn_started(1)), Duration::ZERO, &facts);
    models.open(&entry("/child", "next"), &facts);
    models.apply(
        &send(turn_ended(End {
            ending: Ending::Error(ErrorKind::RateLimit),
            message: "limit".into(),
            detail: Some(crate::app::fixture::limit(None)),
            attempts: 1,
            ..End::default()
        })),
        Duration::ZERO,
        &facts,
    );
    let renderer = Renderer::new(Layout::new(120, GlyphSet::Ascii));
    let rows = models
        .get_mut(&first.path)
        .unwrap()
        .transcript
        .rows(&renderer, Detail::Full);
    let text = rows
        .iter()
        .flat_map(|row| &row.spans)
        .map(|span| span.text.as_str())
        .collect::<String>();
    assert!(text.contains("original reached its usage limit"), "{text}");
    assert!(!text.contains("next reached"), "{text}");
}

#[test]
fn a_list_that_shows_two_agents_ended_keeps_both_boxes() {
    let (mut models, facts) = models();
    let ended = ["/a", "/b"].map(|path| AgentEntry {
        state: AgentState::Ended,
        ..entry(path, "gpt")
    });
    for agent in &ended {
        models.open(&entry(agent.path.as_str(), "gpt"), &facts);
        let box_text = format!("for {}", agent.path.as_str());
        models
            .get_mut(&agent.path)
            .unwrap()
            .composer
            .insert(&box_text);
    }
    let snapshot = AgentEvent::AgentsSnapshot {
        request: None,
        agents: ended.to_vec(),
    };
    let Some(super::Change::Lost(lost)) = models.apply(&snapshot, Duration::ZERO, &facts) else {
        panic!("expected the ended agents' text");
    };
    let lost: Vec<(&str, Vec<&str>)> = (lost.iter())
        .map(|(path, kept)| (path.as_str(), kept.iter().map(|s| s.text()).collect()))
        .collect();
    assert_eq!(lost, [("/a", vec!["for /a"]), ("/b", vec!["for /b"])]);
}

#[test]
fn a_childs_text_has_one_holder_until_its_box_or_main_history() {
    use crate::widgets::composer::Saved;
    use wisp_protocol::{DeliveryMode, InputId};
    let (mut live, facts) = models();
    for path in ["/a", "/b"] {
        let agent = entry(path, "gpt");
        live.open(&agent, &facts);
        let model = live.get_mut(&agent.path).unwrap();
        if path == "/a" {
            let saved = Saved::from_text("unconfirmed for a");
            model.inputs.sent(
                InputId("child-1".into()),
                DeliveryMode::Steer,
                saved.message().parts,
                saved,
            );
        }
        model.composer.insert(&format!("draft for {}", agent.name));
    }
    let children = live.unsent();
    assert_eq!(children.len(), 2);
    assert_eq!(children[0].unconfirmed[0].1.text(), "unconfirmed for a");
    assert!(live.unsent().is_empty());
    let (mut reopened, _) = models();
    reopened.hold(children.into_iter().map(super::Child::kept));
    let children = reopened.unsent();
    assert_eq!(
        children[0]
            .texts
            .iter()
            .map(Saved::text)
            .collect::<Vec<_>>(),
        ["unconfirmed for a", "draft for a"]
    );
    assert_eq!(children[1].texts[0].text(), "draft for b");
    assert!(reopened.unsent().is_empty());
    reopened.hold(children.into_iter().map(super::Child::kept));
    let snapshot = AgentEvent::AgentsSnapshot {
        request: None,
        agents: vec![
            entry("/a", "gpt"),
            AgentEntry {
                state: AgentState::Ended,
                ..entry("/b", "gpt")
            },
        ],
    };
    let Some(super::Change::Lost(lost)) = reopened.apply(&snapshot, Duration::ZERO, &facts) else {
        panic!("expected /b's text in main's history")
    };
    assert_eq!(lost.len(), 1);
    assert_eq!(lost[0].0.as_str(), "/b");
    assert_eq!(lost[0].1[0].text(), "draft for b");
    let children = reopened.unsent();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].agent.as_str(), "/a");
    assert_eq!(
        children[0].texts[0].text(),
        "unconfirmed for a\n\ndraft for a"
    );
    assert!(reopened.unsent().is_empty());
}

#[test]
fn model_written_agent_text_is_stored_inert() {
    let hostile = "x\u{9b}2J\u{7}\r\u{7f}y\u{202e}z";
    let bad = |text: &str| {
        text.chars()
            .any(|c| c.is_control() || matches!(c, '\u{202A}'..='\u{202E}'))
    };
    let facts = crate::app::fixture::endpoint_facts();
    let mut agents = Agents::default();
    let spawned = AgentEntry {
        name: hostile.to_owned(),
        session_id: hostile.to_owned(),
        preset: Some(hostile.to_owned()),
        provider: Some(hostile.to_owned()),
        cwd: Some(hostile.to_owned()),
        last_report: Some(hostile.to_owned()),
        state: AgentState::Running,
        activity: Activity::Tool {
            name: hostile.to_owned(),
        },
        ..entry("/a", "gpt")
    };
    agents.apply(
        &AgentEvent::AgentSpawned {
            request: None,
            session_id: String::new(),
            agent: spawned,
            config: wisp_protocol::SpawnChoices::default(),
        },
        0,
        &facts,
    );
    let row = agents.rows()[0].entry.clone();
    let Activity::Tool { name: tool } = &row.activity else {
        panic!("{:?}", row.activity);
    };
    for text in [
        row.name.as_str(),
        row.session_id.as_str(),
        row.preset.as_deref().unwrap_or_default(),
        row.provider.as_deref().unwrap_or_default(),
        row.cwd.as_deref().unwrap_or_default(),
        row.last_report.as_deref().unwrap_or_default(),
        tool.as_str(),
    ] {
        assert!(!bad(text), "{text:?}");
        assert!(
            text.contains("M-^[2J^G") && text.contains("<U+202E>"),
            "{text:?}"
        );
    }
    agents.apply(
        &AgentEvent::AgentStateChanged {
            plan_usd: None,
            billing: Default::default(),
            agent: AgentPath::new("/a"),
            state: AgentState::Running,
            activity: Activity::Tool {
                name: hostile.to_owned(),
            },
            spent_usd: 0.0,
            lower_bound: false,
            accounts: Default::default(),
            since_ms: None,
        },
        0,
        &facts,
    );
    agents.apply(
        &AgentEvent::AgentReported {
            agent: AgentPath::new("/a"),
            turn: 1,
            outcome: wisp_protocol::ReportOutcome::Reported,
            report: Some(json!(format!("{hostile}\nrest"))),
            cost: None,
            duration_ms: 1,
        },
        0,
        &facts,
    );
    let row = agents.rows()[0].entry.clone();
    let Activity::Tool { name: tool } = &row.activity else {
        panic!("{:?}", row.activity);
    };
    for text in [
        tool.as_str(),
        row.last_report.as_deref().unwrap_or_default(),
    ] {
        assert!(!bad(text), "{text:?}");
        assert!(text.contains("<U+202E>"), "{text:?}");
    }
}
