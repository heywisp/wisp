use serde_json::json;
use wisp_protocol::log::LOG_SCHEMA_VERSION;

use super::*;
use crate::test_guard::protocol_fixture;

fn record(schema_version: u16, event: serde_json::Value) -> LogRecord {
    let mut line = event;
    line["schema_version"] = json!(schema_version);
    line["seq"] = json!(0);
    line["at"] = json!("2026-10-28T12:00:00Z");
    serde_json::from_value(line).unwrap()
}

fn compacted(schema_version: u16, head: &[&str], sections: serde_json::Value) -> LogRecord {
    let content: Vec<serde_json::Value> = head
        .iter()
        .map(|t| json!({"type": "text", "text": t}))
        .collect();
    record(
        schema_version,
        json!({
            "type": "context_compacted", "epoch": 1, "trigger": "manual", "engine": "wisp",
            "before": 10, "after": 5, "from": 2, "duration_ms": 1,
            "head": {"role": "user", "content": content},
            "sections": sections,
        }),
    )
}

fn turn(n: u64, lead: serde_json::Value) -> LogRecord {
    record(
        LOG_SCHEMA_VERSION,
        json!({"type": "turn_started", "turn": n, "text": "hi", "lead": lead}),
    )
}

fn agent() -> Agent {
    Agent::new(
        crate::agent::testing::Fake::new([]),
        crate::agent::default_session(),
    )
}

const ENV: &str = "<environment>\nWorking directory: ~/w\n</environment>\n";

#[test]
fn an_old_heads_environment_reads_as_legacy_text_and_its_absence_as_legacy_none() {
    let with = compacted(32, &[ENV, "<compacted_context/>"], json!([]));
    let without = compacted(32, &["<compacted_context/>"], json!([]));
    let new = compacted(
        LOG_SCHEMA_VERSION,
        &[ENV],
        json!([{"name": LEAD_ENVIRONMENT, "snapshot": ENV}]),
    );
    let read = |r: &LogRecord| moves([(0, r)]).window(Some(0), 1).head.unwrap().1.clone();
    assert_eq!(
        read(&with),
        BTreeMap::from([(LEAD_ENVIRONMENT.to_owned(), Prior::Legacy(Some(ENV.into())))])
    );
    assert_eq!(
        read(&without),
        BTreeMap::from([(LEAD_ENVIRONMENT.to_owned(), Prior::Legacy(None))])
    );
    assert_eq!(
        read(&new),
        BTreeMap::from([(LEAD_ENVIRONMENT.to_owned(), Prior::Snapshot(ENV.into()))])
    );
}

#[test]
fn only_kept_turns_after_the_head_move_a_section() {
    let records = [
        turn(
            1,
            json!([{"by": "bb/notes", "text": "N1", "snapshot": "v1"}]),
        ),
        compacted(
            LOG_SCHEMA_VERSION,
            &[],
            json!([{"name": "bb/notes", "snapshot": "v2"}]),
        ),
        turn(
            2,
            json!([{"by": "bb/notes", "text": "N3", "snapshot": "v3"}]),
        ),
        turn(3, json!([{"by": "core/environment", "text": ENV}])),
        turn(
            4,
            json!([{"by": "bb/notes", "text": "N4", "snapshot": "v4"}]),
        ),
    ];
    let all = moves(records.iter().enumerate());
    let agent = agent();
    let start = StartAt::default();
    let window = all.window(Some(1), records.len());
    let at = |kept: &[u64]| agent.committed_at(&window, |t| kept.contains(&t), &start);
    assert_eq!(
        at(&[2, 3]),
        BTreeMap::from([
            ("bb/notes".to_owned(), Prior::Snapshot("v3".into())),
            (LEAD_ENVIRONMENT.to_owned(), Prior::Legacy(Some(ENV.into()))),
        ])
    );
    assert_eq!(
        at(&[]),
        BTreeMap::from([("bb/notes".to_owned(), Prior::Snapshot("v2".into()))])
    );
    assert_eq!(
        at(&[2, 4]),
        BTreeMap::from([("bb/notes".to_owned(), Prior::Snapshot("v4".into()))])
    );
}

#[test]
fn the_last_sections_record_is_the_start() {
    let started = record(
        LOG_SCHEMA_VERSION,
        json!({
            "type": "session_started", "session_id": "s1", "wisp_version": "0.1.0",
            "model": "m", "effort": "low", "cache_ttl": "one_hour", "system_prompt": null,
            "system_prompt_sha256": null, "endpoint": "https://api.anthropic.com",
            "credential_source": "env_file",
            "sections": [{"name": "bb/rules", "snapshot": "r1"}],
        }),
    );
    let mut resumed: serde_json::Value =
        serde_json::to_value(protocol_fixture("log_session_resumed_sections")).unwrap();
    resumed["seq"] = json!(3);
    let resumed: LogRecord = serde_json::from_value(resumed).unwrap();
    let plain = protocol_fixture("log_session_resumed");
    let records = [started.clone(), plain.clone(), resumed, plain];
    let start = logged_start(&records);
    assert_eq!(start.at, 2);
    assert_eq!(
        start.snapshots.get("acme/rules").map(String::as_str),
        Some("rules v2")
    );
    let only = logged_start(&[started]);
    assert_eq!(
        (only.at, only.snapshots.get("bb/rules").map(String::as_str)),
        (0, Some("r1"))
    );
}

mod start {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use wisp_plugin_api::manifest::Origin;
    use wisp_plugin_testkit::hooks::{self as kit, Calls, Section};

    use super::*;
    use crate::plugin::host::IN_START_RENDER;
    use crate::plugin::{KernelHost, Make, PluginConfig, Registry, Source};

    fn rules(section: Section, origin: Origin) -> Source {
        let make: Make = kit::prompted(kit::none(), section, Calls::default());
        let body = kit::prompt_section("rules", "system", "");
        Source::in_process(kit::manifest_in("0.2.0", "bb", &body), make, origin)
    }

    fn after(wait: Duration, text: String) -> Section {
        Arc::new(move |_, _| {
            let text = text.clone();
            Box::pin(async move {
                tokio::time::sleep(wait).await;
                Rendered {
                    snapshot: "r1".into(),
                    text: Some(text),
                }
            })
        })
    }

    async fn sectioned(source: Source) -> Agent {
        let (registry, problems) = Registry::from_origins(vec![source], &PluginConfig::default());
        assert!(problems.is_empty(), "{problems:?}");
        let started = registry
            .start(&BTreeSet::new(), Arc::new(KernelHost::new()))
            .await;
        let dir = std::env::temp_dir();
        let workspace = crate::workspace::Workspace {
            root: dir.clone(),
            home: None,
            temp: dir,
        };
        agent()
            .with_hooks(started.hooks, started.host)
            .with_tools(crate::tool::testkit::toolbox(&["echo"]), workspace)
    }

    #[tokio::test]
    async fn a_start_render_runs_as_a_start_render_before_its_first_await() {
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        let into = seen.clone();
        let section: Section = Arc::new(move |host, call| {
            let refused = host
                .replace(Vec::new(), &wisp_plugin_testkit::host::FakeCall::default())
                .err()
                .unwrap_or_default();
            into.lock()
                .unwrap()
                .push(format!("{:?} {refused}", call.at));
            let root = host.root();
            Box::pin(async move {
                Rendered {
                    snapshot: root.clone(),
                    text: Some(format!("<rules>\n{root}\n</rules>\n")),
                }
            })
        });
        let (agent, warnings) = sectioned(rules(section, Origin::Bundled))
            .await
            .with_start_sections()
            .await;
        assert!(warnings.is_empty(), "{warnings:?}");
        let root = std::env::temp_dir().display().to_string();
        assert_eq!(*seen.lock().unwrap(), [format!("Start {IN_START_RENDER}")]);
        assert_eq!(
            agent.session.instructions.as_deref(),
            Some(format!("<rules>\n{root}\n</rules>\n").as_str())
        );
        assert_eq!(
            agent.start_sections(),
            [SectionSnapshot {
                name: "bb/rules".into(),
                snapshot: root.clone()
            }]
        );
        assert_eq!(
            agent.sections.committed().get("bb/rules"),
            Some(&Prior::Snapshot(root))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_non_bundled_start_render_past_ten_seconds_warns_and_leaves_no_base() {
        let slow = after(Duration::from_secs(11), "<rules>\nr1\n</rules>\n".into());
        let (agent, warnings) = sectioned(rules(slow, Origin::External))
            .await
            .with_start_sections()
            .await;
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(warnings[0].plugin, "bb");
        assert_eq!(agent.session.instructions, None);
        assert!(agent.start_sections().is_empty());
        assert_eq!(agent.section_base("bb/rules"), Prior::None);
    }

    #[tokio::test(start_paused = true)]
    async fn a_bundled_start_render_has_no_time_limit() {
        let slow = after(Duration::from_secs(600), "<rules>\nr1\n</rules>\n".into());
        let (agent, warnings) = sectioned(rules(slow, Origin::Bundled))
            .await
            .with_start_sections()
            .await;
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(agent.start_sections().len(), 1);
    }

    #[tokio::test]
    async fn an_oversized_start_text_fails_from_a_non_bundled_plugin_and_is_whole_from_a_bundled_one()
     {
        let big = format!("<rules>\n{}\n</rules>\n", "x".repeat(10_001));
        let now = Duration::ZERO;
        let (external, warnings) = sectioned(rules(after(now, big.clone()), Origin::External))
            .await
            .with_start_sections()
            .await;
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(external.session.instructions, None);
        let (bundled, warnings) = sectioned(rules(after(now, big.clone()), Origin::Bundled))
            .await
            .with_start_sections()
            .await;
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(bundled.session.instructions.as_deref(), Some(big.as_str()));
    }

    #[tokio::test]
    async fn a_system_section_moves_only_by_kept_turns_after_the_last_sections_record() {
        let agent = sectioned(rules(after(Duration::ZERO, String::new()), Origin::Bundled)).await;
        let entry =
            |n: u64, v: &str| turn(n, json!([{"by": "bb/rules", "text": v, "snapshot": v}]));
        let records = [
            entry(1, "v1"),
            entry(2, "v2"),
            turn(3, json!([])),
            entry(4, "v4"),
            entry(5, "v5"),
        ];
        let all = moves(records.iter().enumerate());
        let window = all.window(None, records.len());
        let start = StartAt {
            at: 2,
            snapshots: BTreeMap::from([("bb/rules".to_owned(), "fresh".to_owned())]),
            legacy: BTreeMap::new(),
        };
        let at = |kept: &[u64]| agent.committed_at(&window, |t| kept.contains(&t), &start);
        let rules = |p: &str| BTreeMap::from([("bb/rules".to_owned(), Prior::Snapshot(p.into()))]);
        assert_eq!(at(&[1, 2, 3]), rules("fresh"));
        assert_eq!(at(&[1, 2, 3, 4]), rules("v4"));
        assert_eq!(at(&[1, 2, 3, 5]), rules("v5"));
        assert_eq!(at(&[1, 2]), rules("fresh"));
    }
}

fn started_with(instructions: Option<&str>, sections: serde_json::Value) -> LogRecord {
    record(
        LOG_SCHEMA_VERSION,
        json!({
            "type": "session_started", "session_id": "s1", "wisp_version": "0.1.0",
            "model": "m", "effort": "low", "cache_ttl": "one_hour", "system_prompt": null,
            "system_prompt_sha256": null, "endpoint": "https://api.anthropic.com",
            "credential_source": "env_file",
            "instructions": instructions,
            "instructions_sha256": instructions.map(wisp_instructions::sha256_hex),
            "sections": sections,
        }),
    )
}

fn resumed_with(
    instructions: Option<&str>,
    sha: Option<&str>,
    sections: serde_json::Value,
) -> LogRecord {
    record(
        LOG_SCHEMA_VERSION,
        json!({
            "type": "session_resumed", "wisp_version": "0.1.0",
            "endpoint": "https://api.anthropic.com", "endpoint_name": "env",
            "instructions": instructions,
            "instructions_sha256": sha,
            "sections": sections,
        }),
    )
}

const PROJECT_BLOCK: &str = "<instructions>\nopening\n<file scope=\"project\" name=\"AGENTS.md\">\nOne.\n</file>\n</instructions>";
const DEFAULT_BLOCK: &str = "<instructions>\nopening\n<file scope=\"default\" name=\"AGENTS.md\">\nOne.\n</file>\n</instructions>";

#[test]
fn a_legacy_base_needs_a_project_or_default_file_in_the_logged_block() {
    let legacy = |records: &[LogRecord]| logged_start(records).legacy;
    let preset = "<instructions>\npreset\n<file scope=\"preset\" name=\"AGENTS.md\">\nP.\n</file>\n</instructions>";
    assert!(legacy(&[started_with(Some(preset), json!([]))]).is_empty());
    assert!(legacy(&[started_with(None, json!([]))]).is_empty());
    assert_eq!(
        legacy(&[started_with(Some(PROJECT_BLOCK), json!([]))]),
        BTreeMap::from([(
            INSTRUCTIONS_SECTION.to_owned(),
            Some(PROJECT_BLOCK.to_owned())
        )])
    );
    let snapshot = json!([{"name": INSTRUCTIONS_SECTION, "snapshot": "aa project AGENTS.md\n"}]);
    assert!(legacy(&[started_with(Some(PROJECT_BLOCK), snapshot)]).is_empty());
}

#[test]
fn the_block_folds_from_the_latest_record_that_carries_it_apart_from_the_sections() {
    let sha = wisp_instructions::sha256_hex(PROJECT_BLOCK);
    let records = [
        started_with(Some(PROJECT_BLOCK), json!([])),
        resumed_with(None, Some(&sha), json!([])),
    ];
    let start = logged_start(&records);
    assert_eq!(start.at, 1);
    assert_eq!(
        start.legacy.get(INSTRUCTIONS_SECTION),
        Some(&Some(PROJECT_BLOCK.to_owned())),
        "an identical fresh block logs no text and keeps the earlier one"
    );
    let gone = [
        started_with(Some(PROJECT_BLOCK), json!([])),
        resumed_with(None, None, json!([])),
    ];
    assert!(logged_start(&gone).legacy.is_empty());
}

#[test]
fn a_two_field_snapshot_takes_its_scope_from_the_logged_block() {
    let old = json!([{"name": INSTRUCTIONS_SECTION, "snapshot": "aa AGENTS.md\nbb a.md\n"}]);
    let default = logged_start(&[started_with(Some(DEFAULT_BLOCK), old.clone())]);
    assert_eq!(
        default
            .snapshots
            .get(INSTRUCTIONS_SECTION)
            .map(String::as_str),
        Some("aa default AGENTS.md\nbb default a.md\n")
    );
    let unsaid = logged_start(&[started_with(None, old)]);
    assert_eq!(
        unsaid
            .snapshots
            .get(INSTRUCTIONS_SECTION)
            .map(String::as_str),
        Some("aa project AGENTS.md\nbb project a.md\n")
    );
    let scoped = json!([{"name": INSTRUCTIONS_SECTION, "snapshot": "aa default AGENTS.md\n"}]);
    let kept = logged_start(&[started_with(Some(PROJECT_BLOCK), scoped)]);
    assert_eq!(
        kept.snapshots.get(INSTRUCTIONS_SECTION).map(String::as_str),
        Some("aa default AGENTS.md\n")
    );
}
