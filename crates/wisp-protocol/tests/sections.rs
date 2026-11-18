#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "fixture helpers fail the test on any read, write or parse error"
)]

mod support;

use std::fmt::Debug;

use serde::Deserialize;
use serde_json::json;
use support::{fixture_text, golden, lead_of, read};
use wisp_protocol::log::{
    CONTINUED, LEAD_ENVIRONMENT, LEAD_PROCESSES, LOG_SCHEMA_VERSION, LeadEntry, LogEvent,
    SECTIONS_SINCE, SectionSnapshot, TurnLead, continued_name,
};
use wisp_protocol::plugin::{PromptSectionEntry, SectionPlace};
use wisp_protocol::{AgentEvent, PluginEntry};

const READER_34: u16 = 34;

const OLD_READERS: [u16; 2] = [32, READER_34];

fn snapshots() -> Vec<SectionSnapshot> {
    vec![
        SectionSnapshot {
            name: "acme/rules".into(),
            snapshot: "rules v2".into(),
        },
        SectionSnapshot {
            name: LEAD_ENVIRONMENT.into(),
            snapshot: "<environment>\nWorking directory: ~/p\n</environment>\n".into(),
        },
    ]
}

#[test]
fn prompt_sections_bump_the_log_to_version_35() {
    assert_eq!(SECTIONS_SINCE, READER_34 + 1);
    const { assert!(LOG_SCHEMA_VERSION >= SECTIONS_SINCE) };
}

#[derive(Deserialize)]
struct EntryAt34 {
    by: String,
}

#[derive(Deserialize)]
struct TurnAt34 {
    schema_version: u16,
    lead: Vec<EntryAt34>,
}

#[test]
fn a_version_34_reader_sees_every_record_a_sections_log_writes_as_newer() {
    for name in [
        "log_turn_started",
        "log_turn_started_sections",
        "log_session_started_sections",
        "log_session_resumed_sections",
        "log_context_compacted_sections",
    ] {
        let line: serde_json::Value =
            serde_json::from_str(&fixture_text(&format!("{name}.json"))).unwrap();
        assert_eq!(line["schema_version"], json!(LOG_SCHEMA_VERSION), "{name}");
    }
    let stored = fixture_text("log_turn_started.json");
    let seen: TurnAt34 = serde_json::from_str(&stored).unwrap();
    assert!(seen.schema_version > READER_34);
    assert!(
        seen.lead.iter().all(|e| e.by != "core/environment"),
        "a version 34 reader would miss the renamed entry"
    );
    assert!(seen.lead.iter().any(|e| e.by == LEAD_ENVIRONMENT));
}

#[test]
fn the_old_core_names_read_as_the_plugin_names_that_continue_them() {
    assert_eq!(
        CONTINUED,
        [
            ("core/environment", LEAD_ENVIRONMENT),
            ("core/processes", LEAD_PROCESSES)
        ]
    );
    assert_eq!(continued_name("core/environment"), LEAD_ENVIRONMENT);
    assert_eq!(continued_name("core/processes"), LEAD_PROCESSES);
    assert_eq!(continued_name("acme/rules"), "acme/rules");
    let event: LogEvent = serde_json::from_value(json!({
        "type": "turn_started",
        "turn": 2,
        "lead": [
            {"by": "core/environment", "text": "E"},
            {"by": "core/processes", "text": "P"},
            {"by": "acme/rules", "text": "R"},
        ],
    }))
    .unwrap();
    assert_eq!(
        lead_of(&event).entries,
        [
            LeadEntry::new(LEAD_ENVIRONMENT, "E"),
            LeadEntry::new(LEAD_PROCESSES, "P").replayed(),
            LeadEntry::new("acme/rules", "R"),
        ]
    );
    assert!(lead_of(&event).replays());
}

#[test]
fn every_version_32_and_34_turn_fixture_reads_as_the_lead_the_current_one_writes() {
    for name in [
        "log_turn_started",
        "log_turn_started_context_note",
        "log_turn_started_wake",
        "log_turn_started_wake_ends",
    ] {
        let new = read(name);
        assert_eq!(new.schema_version, LOG_SCHEMA_VERSION, "{name}");
        for reader in OLD_READERS {
            let old = read(&format!("{name}_v{reader}"));
            assert_eq!(old.schema_version, reader, "{name}");
            assert_eq!(old.event, new.event, "{name} v{reader}");
        }
    }
}

#[test]
fn a_lead_entry_without_a_snapshot_or_replay_writes_neither() {
    let line = serde_json::to_value(LeadEntry::new("acme/rules", "R")).unwrap();
    assert_eq!(line, json!({"by": "acme/rules", "text": "R"}));
}

#[test]
fn the_section_shapes_match_their_fixtures() {
    let mut turn = read("log_turn_started");
    if let LogEvent::TurnStarted { lead, .. } = &mut turn.event {
        *lead = TurnLead::new(vec![
            LeadEntry::new("acme/rules", "<rules>\nv2\n</rules>\n").with_snapshot("rules v2"),
            LeadEntry::new("acme/news", "<news>\nbuild failed\n</news>\n").replayed(),
        ]);
    }
    golden("log_turn_started_sections", &turn);

    let mut started = read("log_session_started");
    if let LogEvent::SessionStarted { sections, .. } = &mut started.event {
        *sections = snapshots();
    }
    golden("log_session_started_sections", &started);

    let mut resumed = read("log_session_resumed");
    if let LogEvent::SessionResumed { sections, .. } = &mut resumed.event {
        *sections = Some(snapshots());
    }
    golden("log_session_resumed_sections", &resumed);

    let mut compacted = read("log_context_compacted_convert");
    if let LogEvent::ContextCompacted { sections, .. } = &mut compacted.event {
        *sections = snapshots();
    }
    golden("log_context_compacted_sections", &compacted);
}

#[test]
fn a_catalog_entry_names_its_prompt_sections_and_an_unknown_place_reads_as_unknown() {
    let AgentEvent::CatalogChanged { mut catalog } =
        serde_json::from_str::<AgentEvent>(&fixture_text("event_catalog_changed.json")).unwrap()
    else {
        panic!("event_catalog_changed is no catalog");
    };
    catalog.plugins.truncate(1);
    let entry: &mut PluginEntry = &mut catalog.plugins[0];
    entry.prompt_sections = vec![
        PromptSectionEntry {
            name: "acme/rules".into(),
            place: SectionPlace::System,
        },
        PromptSectionEntry {
            name: "acme/news".into(),
            place: SectionPlace::Lead,
        },
    ];
    golden(
        "event_catalog_prompt_sections",
        &AgentEvent::CatalogChanged { catalog },
    );
    let place: SectionPlace = serde_json::from_value(json!("sidebar")).unwrap();
    assert_eq!(place, SectionPlace::Unknown);
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OldHookDisabled {
    Cycle {
        cycle: Vec<String>,
    },
    Missing {
        requires: Vec<String>,
    },
    PluginOff,
    SwitchedOff,
    #[serde(other)]
    Unknown,
}

#[test]
fn a_catalogs_hooks_carry_their_switch_and_left_out_and_an_older_client_reads_unknown() {
    use wisp_protocol::plugin::{HookDisabled, HookEntry};
    let AgentEvent::CatalogChanged { mut catalog } =
        serde_json::from_str::<AgentEvent>(&fixture_text("event_catalog_changed.json")).unwrap()
    else {
        panic!("event_catalog_changed is no catalog");
    };
    catalog.plugins.truncate(1);
    catalog.plugins[0].hooks = vec![
        HookEntry {
            hook: "context/fill".into(),
            point: "before-turn".into(),
            on: true,
            order: 1,
            disabled: None,
            switch: Some("context.context_note".into()),
        },
        HookEntry {
            hook: "context/late".into(),
            point: "before-turn".into(),
            on: false,
            order: 3,
            disabled: Some(HookDisabled::LeftOut),
            switch: None,
        },
    ];
    let event = AgentEvent::CatalogChanged { catalog };
    golden("event_catalog_hooks", &event);
    let text = serde_json::to_value(&event).unwrap();
    let left_out = &text["catalog"]["plugins"][0]["hooks"][1]["disabled"];
    assert_eq!(left_out, &json!({"type": "left_out"}));
    let old: OldHookDisabled = serde_json::from_value(left_out.clone()).unwrap();
    assert_eq!(old, OldHookDisabled::Unknown);
    let unknown: HookDisabled = serde_json::from_value(json!({"type": "later"})).unwrap();
    assert_eq!(unknown, HookDisabled::Unknown);
    let plain = &text["catalog"]["plugins"][0]["hooks"][1];
    assert!(plain.get("switch").is_none(), "{plain}");
}
