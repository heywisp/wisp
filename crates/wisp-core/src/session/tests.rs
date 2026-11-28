use serde_json::json;
use wisp_protocol::endpoint::{CheckResult, ConnectionField, NotSavedReason, StoreKind};
use wisp_protocol::log::{CredentialSource, TurnLead};
use wisp_protocol::{
    CacheTtl, ContentBlock, Effort, Message, MessageTime, Mime, Role, SearchAction, Usage,
};

use super::redaction::holds;
use super::*;
use crate::test_guard::kind_of;
use crate::test_guard::shared::{record, temp_dir};
use crate::tool::snap::mime;

fn started() -> LogEvent {
    LogEvent::SessionStarted {
        system_base_sha256: None,
        sections: Vec::new(),
        plugins: Vec::new(),
        session_id: "0199-abc".into(),
        wisp_version: "0.1.0".into(),
        model: "claude-opus-5-5".into(),
        effort: Effort::Medium,
        cache_ttl: CacheTtl::OneHour,
        system_prompt: Some("sys".into()),
        system_prompt_sha256: Some(sha256_hex("sys")),
        endpoint: "https://api.anthropic.com".into(),
        credential_source: CredentialSource::Environment,
        endpoint_name: Some("env".into()),
        message_time: Some(MessageTime::Local),
        tools: Vec::new(),
        tools_sha256: None,
        whole: Default::default(),
        cwd: None,
        output_dir: None,
        instructions: None,
        instructions_sha256: None,
        instructions_source: None,
        instructions_extra: None,
        preset: None,
        context_note_enabled: None,
        debug_cache: None,
        refusal_fallback: None,
        parent: None,
        headless: false,
    }
}

fn tricky_message() -> Message {
    Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::Thinking {
                thinking: "line one\nline \"two\" \u{1F600} \\ é".into(),
                signature: "EqQBCkgIARABGAIiQL+/=\u{2028}xyz==".into(),
            },
            ContentBlock::RedactedThinking {
                data: "opaque/+==".into(),
            },
            ContentBlock::opaque(
                "anthropic",
                json!({"type": "server_tool_use", "id": "srv_1", "input": {"z": 1, "a": [1.5, null]}}),
            ),
            ContentBlock::Text {
                text: "  spaced  \t".into(),
            },
        ],
    }
}

fn sample_records() -> Vec<LogRecord> {
    vec![
        record(0, started()),
        record(
            1,
            LogEvent::TurnStarted {
                ends: Vec::new(),
                endpoint_name: None,
                turn: 1,
                origin: None,
                lead: TurnLead::from_fields(
                    None,
                    Some("Sent: Wednesday 2026-10-21 23:58 UTC-04:00\n".into()),
                    None,
                    None,
                ),
                text: "hi".into(),
                model: None,
                effort: None,
                effort_marker: None,
                images: Vec::new(),
                input_id: None,
                inputs: Vec::new(),
                cache_ttl: None,
                debug_cache: None,
                refusal_fallback: None,
                binding_controls: None,
                retry_of: None,
                kept_tools_sha256: None,
                tool_message: Vec::new(),
            },
        ),
        record(
            2,
            LogEvent::MessageCommitted {
                model: None,
                stop_reason: None,
                turn: 1,
                message: tricky_message().into(),
            },
        ),
        record(
            3,
            LogEvent::SessionEnded {
                turns: 1,
                interrupted: None,
            },
        ),
    ]
}

#[test]
fn session_uuid_normalizes_only_uuids() {
    let canonical = "550e8400-e29b-41d4-a716-446655440000";
    for id in [
        canonical,
        "550e8400e29b41d4a716446655440000",
        "550E8400-E29B-41D4-A716-446655440000",
    ] {
        assert_eq!(session_uuid(id).as_deref(), Some(canonical));
    }
    for id in [
        "",
        "550e8400",
        "550e8400-e29b-41d4-a716-44665544zzzz",
        "{550e8400-e29b-41d4-a716-446655440000}",
        "urn:uuid:550e8400-e29b-41d4-a716-446655440000",
    ] {
        assert_eq!(session_uuid(id), None);
    }
}

#[test]
fn create_session_does_not_open_an_existing_log() {
    let dir = temp_dir("exclusive-session");
    let mut store = JsonlStore::open(&dir).unwrap();
    let id = new_session_id();
    let path = store.path(&id);
    store.create_session(&id).unwrap();
    fs::write(&path, "existing log").unwrap();
    assert_eq!(
        store.create_session(&id).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(fs::read_to_string(path).unwrap(), "existing log");
    drop(store);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn jsonl_store_round_trips() {
    let dir = temp_dir("rt");
    let mut store = JsonlStore::open(&dir).unwrap();
    let id = new_session_id();
    for r in sample_records() {
        store.append(&id, &r).unwrap();
    }
    store.sync(&id).unwrap();
    assert_eq!(store.read(&id).unwrap(), sample_records());
    assert_eq!(store.list().unwrap(), vec![id.clone()]);
    let text = fs::read_to_string(store.path(&id)).unwrap();
    assert_eq!(text.lines().count(), 4);
    assert!(text.ends_with('\n'));
    assert_eq!(
        JsonlStore::open(&dir).unwrap().read(&id).unwrap(),
        sample_records()
    );
    let spaced = format!("\n  \t\n{}\n \t\n", text.replace('\n', "\n\n \t\n"));
    fs::write(store.path(&id), spaced).unwrap();
    assert_eq!(store.read(&id).unwrap(), sample_records());
    assert_eq!(store.reader(&id, 4).unwrap()().unwrap(), sample_records());
    assert!(store.reader(&id, 0).unwrap()().unwrap().is_empty());
    let reader = store.reader(&id, 3).unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(store.path(&id))
        .unwrap()
        .write_all(b"{")
        .unwrap();
    assert_eq!(reader().unwrap(), sample_records()[..3]);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn memory_store_round_trips() {
    let mut store = MemoryStore::default();
    let handle = store.clone();
    for r in sample_records() {
        store.append("a", &r).unwrap();
    }
    assert_eq!(handle.read("a").unwrap(), sample_records());
    assert_eq!(handle.list().unwrap(), vec!["a".to_owned()]);
    assert!(handle.read("b").is_err());
}

#[test]
fn sessions_list_oldest_first() {
    let dir = temp_dir("list");
    let mut store = JsonlStore::open(&dir).unwrap();
    let ids: Vec<String> = (0..3).map(|_| new_session_id()).collect();
    for id in ids.iter().rev() {
        store.append(id, &record(0, started())).unwrap();
    }
    fs::write(dir.join("notes.txt"), "x").unwrap();
    assert_eq!(store.list().unwrap(), ids);
    fs::remove_dir_all(&dir).unwrap();
}

#[cfg(unix)]
#[test]
fn directory_and_files_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp_dir("perm");
    fs::create_dir_all(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    let nested = dir.join("wisp").join("sessions");
    let mut store = JsonlStore::open(&dir).unwrap();
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&dir), 0o700);
    let id = new_session_id();
    store.append(&id, &record(0, started())).unwrap();
    assert_eq!(mode(&store.path(&id)), 0o600);

    let mut store = JsonlStore::open(&nested).unwrap();
    assert_eq!(mode(store.dir()), 0o700);
    assert_eq!(mode(&dir.join("wisp")), 0o700);
    let day = "2026-11-14".parse().unwrap();
    store.append_spend(day, "{}\n").unwrap();
    assert_eq!(mode(&crate::spend::folder(&nested)), 0o700);
    assert_eq!(mode(&crate::spend::file(&nested, day)), 0o600);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn message_committed_round_trips_byte_identically() {
    let r = record(
        2,
        LogEvent::MessageCommitted {
            model: None,
            stop_reason: None,
            turn: 1,
            message: tricky_message().into(),
        },
    );
    let line = serde_json::to_string(&r).unwrap();
    let back: LogRecord = serde_json::from_str(&line).unwrap();
    assert_eq!(back, r);
    assert_eq!(serde_json::to_string(&back).unwrap(), line);
    let LogEvent::MessageCommitted { message, .. } = back.event else {
        panic!()
    };
    assert_eq!(
        serde_json::to_string(&message).unwrap(),
        serde_json::to_string(&tricky_message()).unwrap()
    );
    assert!(line.starts_with(&format!(
        r#"{{"schema_version":{LOG_SCHEMA_VERSION},"seq":2,"at":"#
    )));
    assert!(line.contains(r#""type":"message_committed""#));
}

#[test]
fn truncated_final_line_is_tolerated() {
    let full = lines();
    let last = full.last().unwrap();
    for cut in 1..last.len() {
        let text = format!("{}\n{}", full[..3].join("\n"), &last[..cut]);
        assert_eq!(
            parse_jsonl(&text).unwrap(),
            sample_records()[..3],
            "cut {cut}"
        );
    }
    let text = format!("{}\n{{garbage\n{}\n", full[0], full[1]);
    assert!(parse_jsonl(&text).is_err());
}

fn lines() -> Vec<String> {
    sample_records()
        .iter()
        .map(|r| serde_json::to_string(r).unwrap())
        .collect()
}

fn renumbered(lines: &[String]) -> String {
    lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let mut v: serde_json::Value = serde_json::from_str(l).unwrap();
            v["seq"] = i.into();
            serde_json::to_string(&v).unwrap() + "\n"
        })
        .collect()
}

#[test]
fn v42_log_opens_appends_a_current_slice_and_rebuilds() {
    use wisp_protocol::FileSlice;
    use wisp_protocol::log::{LoggedBlock, LoggedMessage};

    let dir = tempfile::tempdir().unwrap();
    let mut store = JsonlStore::open(dir.path()).unwrap();
    let id = new_session_id();
    let path = store.path(&id);
    let pdf = mime("application/pdf");
    let sha = sha256_bytes_hex(b"a");
    store.put_blob(&sha, &pdf, b"a").unwrap();
    let commit = |slice| LogEvent::MessageCommitted {
        model: None,
        stop_reason: None,
        turn: 1,
        message: LoggedMessage::stored(
            &Message {
                role: Role::User,
                content: vec![ContentBlock::media_sliced(pdf.clone(), "YQ==", slice)],
            },
            blob_ref,
        ),
    };
    let mut old = sample_records();
    old.truncate(2);
    old.push(record(2, commit(None)));
    for line in &mut old {
        line.schema_version = 42;
        store.append(&id, line).unwrap();
    }
    let prefix = fs::read(&path).unwrap();
    drop(store);
    let mut store = JsonlStore::open(dir.path()).unwrap();
    let reopened = store.reopen(&id, ResumeLock::Optional).unwrap();
    assert_eq!(reopened.next_seq, 3);
    let slice = FileSlice::pages(14, 16).unwrap();
    let mut log = SessionLog::continuing(Box::new(store), &id, reopened.next_seq);
    assert_eq!(log.record(commit(Some(slice))), None);
    assert_eq!(log.sync(), None);
    let text = fs::read_to_string(path).unwrap();
    assert!(text.as_bytes().starts_with(&prefix));
    let parsed = parse_jsonl(&text).unwrap();
    assert_eq!(parsed[..3], old);
    assert_eq!(parsed[3].schema_version, LOG_SCHEMA_VERSION);
    assert!(
        matches!(&parsed[2].event, LogEvent::MessageCommitted {message, ..} if matches!(message.content.as_slice(), [LoggedBlock::Media {slice: None, ..}]))
    );
    let rebuilt = rebuild_checked(log.store(), &parsed, None)
        .unwrap()
        .unwrap();
    assert_eq!(
        rebuilt.messages[1].content[0].inline_media().unwrap().slice,
        None
    );
    assert_eq!(
        rebuilt.messages[2].content[0],
        ContentBlock::media_sliced(pdf, "YQ==", Some(slice))
    );
}

#[test]
fn unknown_record_types_and_newer_versions_are_skipped() {
    let full = lines();
    let future = r#"{"schema_version":1,"seq":0,"at":"x","type":"hologram_recorded","x":1}"#;
    let newer = format!(
        r#"{{"schema_version":{},"seq":0,"at":"x","type":"turn_started","renamed":1}}"#,
        LOG_SCHEMA_VERSION + 1
    );
    let text = renumbered(&[full[0].clone(), future.into(), newer, full[1].clone()]);
    let records = parse_jsonl(&text).unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[1].event, sample_records()[1].event);
    let extra = full[1].replacen('{', "{\"future_field\":42,", 1);
    assert_eq!(
        parse_jsonl(renumbered(&[full[0].clone(), extra]))
            .unwrap()
            .len(),
        2
    );
    let version = format!(r#"{{"schema_version":{LOG_SCHEMA_VERSION},"#);
    let valid_newer = full[1].replacen(
        &version,
        &format!(r#"{{"schema_version":{},"#, LOG_SCHEMA_VERSION + 1),
        1,
    );
    let twice_at = full[2].replacen('{', r#"{"at":"y","#, 1);
    assert!(valid_newer != full[1] && twice_at != full[2]);
    let text = format!("{}\n{valid_newer}\n{twice_at}\n", full[0]);
    let parsed = parse_counted(text.as_bytes()).unwrap();
    assert_eq!((parsed.records.len(), parsed.newer), (2, 1));
    assert_eq!(parsed.records[1].event, sample_records()[2].event);
    let read = read_lines(text.as_bytes());
    assert_eq!((read.lines[1].clone(), read.newer), (Line::Skipped, 1));
    assert!(matches!(&read.lines[2], Line::Record(r) if r.event == sample_records()[2].event));
    let misplaced = format!("{}\n{}\n{valid_newer}\n", full[0], full[1]);
    assert_eq!(read_lines(misplaced.as_bytes()).lines[2], Line::Skipped);
    let mixed = format!("{misplaced}{{broken\n{}\n{future}\n", full[1]);
    for chunk in [1, 4] {
        assert_eq!(
            read_lines_in(mixed.as_bytes(), chunk),
            read_lines(mixed.as_bytes())
        );
    }
    let numbered_from_0 = parse_jsonl(renumbered(&full[..2])).unwrap();
    assert_eq!(reread(&numbered_from_0).unwrap().len(), 2);
    assert!(reread(&numbered_from_0[1..]).is_err());
}

#[test]
fn only_a_type_serde_has_no_variant_for_is_unknown() {
    assert!(known_type("instructions_loaded"), "all fields optional");
    assert!(serde_json::from_value::<LogEvent>(json!({"type": "instructions_loaded"})).is_ok());
    assert!(known_type("compaction_prepared"), "required fields");
    assert!(serde_json::from_value::<LogEvent>(json!({"type": "compaction_prepared"})).is_err());
    assert!(known_type("endpoint_removed"));
    assert!(!known_type("hologram_recorded"));
    assert!(!known_type("unknown"));
    assert!(serde_json::to_string(&LogEvent::Unknown).is_err());
    let full = lines();
    let unknown = |seq: usize, envelope: &str| {
        format!(r#"{{{envelope}"seq":{seq},"type":"hologram_recorded","x":1}}"#)
    };
    let at = r#""schema_version":1,"at":"x","#;
    let text = format!(
        "{}\n{}\n{}\n{}\n",
        full[0],
        unknown(1, at),
        unknown(2, ""),
        unknown(9, at)
    );
    let parsed = parse_counted(text.as_bytes()).unwrap_err();
    assert!(parsed.to_string().contains("line 4"), "{parsed}");
    let text = format!("{}\n{}\n{}\n", full[0], unknown(1, at), unknown(2, ""));
    let parsed = parse_counted(text.as_bytes()).unwrap();
    assert_eq!(parsed.records.len(), 1);
    assert_eq!((parsed.lines, parsed.newer), (3, 0));
    let text = format!(
        "{}\n{}\n{}\n{}\n",
        full[0],
        unknown(1, at),
        unknown(2, ""),
        unknown(9, at)
    );
    let read = read_lines(text.as_bytes());
    assert!(matches!(read.lines[0], Line::Record(_)));
    assert_eq!(
        read.lines[1..],
        [
            Line::Skipped,
            Line::Skipped,
            Line::Damaged(DamageKind::OutOfOrder)
        ]
    );
    let numbered = r#"{"schema_version":1,"seq":1,"at":"x","type":999}"#;
    let err = parse_jsonl(format!("{}\n{numbered}\n", full[0])).unwrap_err();
    assert!(err.to_string().contains("line 2"), "{err}");
    let read = read_lines(format!("{}\n{numbered}\n", full[0]).as_bytes());
    assert_eq!(read.lines[1], Line::Damaged(DamageKind::InvalidRecord));
    let removed = r#"{"schema_version":1,"seq":1,"at":"x","type":"endpoint_removed"}"#;
    let read = read_lines(format!("{}\n{removed}\n", full[0]).as_bytes());
    assert_eq!(read.lines[1], Line::Damaged(DamageKind::InvalidRecord));
    assert!(parse_jsonl(format!("{}\n{removed}\n", full[0])).is_err());
}

#[test]
fn a_corrupted_known_record_is_an_error() {
    let full = lines();
    let bad = full[2].replace("\"content\"", "\"contemt\"");
    assert_ne!(bad, full[2]);
    let text = format!("{}\n{}\n{bad}\n{}\n", full[0], full[1], full[3]);
    let err = parse_jsonl(&text).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("line 3"), "{err}");
    let text = format!("{}\n{}\n{bad}\n", full[0], full[1]);
    assert!(parse_jsonl(&text).is_err());
}

#[test]
fn seq_gaps_are_corruption() {
    let full = lines();
    let text = format!("{}\n{}\n{}\n", full[0], full[1], full[3]);
    let err = parse_jsonl(&text).unwrap_err();
    assert!(err.to_string().contains("expected seq 2"), "{err}");
    assert!(parse_jsonl(format!("{}\n", full[1])).is_err());
}

fn mr4_jsonl_line(record: &LogRecord) -> String {
    let json = serde_json::to_string(record).unwrap();
    let mut line = String::with_capacity(json.len() + 1);
    for c in json.chars() {
        if c == '\u{7f}' || ('\u{80}'..='\u{9f}').contains(&c) {
            line.push_str(&format!("\\u{:04x}", u32::from(c)));
        } else if wisp_protocol::jsonl::hidden_format(c) {
            for unit in c.encode_utf16(&mut [0; 2]) {
                line.push_str(&format!("\\u{unit:04x}"));
            }
        } else {
            line.push(c);
        }
    }
    line.push('\n');
    line
}

#[test]
fn log_bytes_match_the_escaping_before_it_moved_to_the_protocol() {
    let hostile =
        "a\u{1b}]52;c;x\u{7}b\u{7f}c\u{9b}31md\u{85}\u{80}\u{9f}\u{a0}\r\n\t\u{0}é✓\u{202e}";
    let records = vec![
        record(0, started()),
        record(
            1,
            LogEvent::Rejected {
                turn: Some(3),
                reason: hostile.into(),
                id: None,
                kind: None,
            },
        ),
        record(
            2,
            LogEvent::Rejected {
                turn: None,
                reason: String::new(),
                id: None,
                kind: None,
            },
        ),
        record(
            3,
            LogEvent::TurnStarted {
                ends: Vec::new(),
                endpoint_name: None,
                turn: 1,
                origin: None,
                lead: TurnLead::from_fields(
                    None,
                    Some("2026-10-21 19:15 +01:00".into()),
                    None,
                    None,
                ),
                text: format!("{hostile}{hostile}"),
                model: None,
                effort: None,
                effort_marker: None,
                images: Vec::new(),
                input_id: None,
                inputs: Vec::new(),
                cache_ttl: None,
                debug_cache: None,
                refusal_fallback: None,
                binding_controls: None,
                retry_of: None,
                kept_tools_sha256: None,
                tool_message: Vec::new(),
            },
        ),
    ];
    for r in &records {
        assert_eq!(
            jsonl_line(r).unwrap().into_bytes(),
            mr4_jsonl_line(r).into_bytes()
        );
    }
}

#[test]
fn log_lines_escape_del_and_c1_controls() {
    let r = record(
        0,
        LogEvent::Rejected {
            turn: None,
            reason: "a\u{1b}]52;c;x\u{7}b\u{7f}c\u{9b}31md\u{85}".into(),
            id: None,
            kind: None,
        },
    );
    let line = jsonl_line(&r).unwrap();
    assert!(line.ends_with('\n'));
    let raw_controls = |s: &str| s.chars().any(|c| c.is_control() && c != '\n');
    assert!(!raw_controls(&line), "{line:?}");
    assert!(line.contains("\\u007f") && line.contains("\\u009b") && line.contains("\\u0085"));
    assert_eq!(parse_jsonl(&line).unwrap(), vec![r.clone()]);

    let dir = temp_dir("ctl");
    let mut store = JsonlStore::open(&dir).unwrap();
    let id = new_session_id();
    store.append(&id, &r).unwrap();
    let text = fs::read_to_string(store.path(&id)).unwrap();
    assert!(!raw_controls(&text), "{text:?}");
    assert_eq!(store.read(&id).unwrap(), vec![r]);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn both_logged_checklists_scrub_unregistered_values_without_a_registry() {
    use wisp_protocol::{AgentPath, DeliveryMode, InputId, ParentLink, TodoItem, TodoStatus};

    let safe = TodoItem {
        text: "Verify the build".into(),
        status: TodoStatus::Completed,
        section: None,
    };
    let sensitive = TodoItem {
        text: "sk-ant-api03-CANARY-direct-log-checklist-fixture".into(),
        status: TodoStatus::Pending,
        section: Some(["CANARY-direct-log-checklist", "example.invalid"].join("@")),
    };
    let attached = TodoItem {
        text: "-iframework/Users/CANARY/Frameworks".into(),
        status: TodoStatus::InProgress,
        section: Some(r"line\n-I/Users/CANARY/inc".into()),
    };
    let steps = [
        sensitive.text.clone(),
        sensitive.section.clone().unwrap(),
        attached
            .section
            .as_deref()
            .unwrap()
            .strip_prefix(r"line\n")
            .unwrap()
            .to_owned(),
    ];
    let items = vec![safe.clone(), sensitive, attached];
    let update = LogEvent::TodosUpdated {
        turn: 1,
        call_id: "call".into(),
        items: items.clone(),
    };
    let mut child_start = started();
    if let LogEvent::SessionStarted { parent, .. } = &mut child_start {
        *parent = Some(Box::new(ParentLink {
            session_id: "parent".into(),
            agent: AgentPath::new("/child"),
            depth: 1,
            config: Default::default(),
            todos: items,
        }));
    }
    let redactions = Redactions::default();
    let store = MemoryStore::default();
    let mut log = SessionLog::new(Box::new(store.clone()), "typed-checklists");
    let assert_scrubbed = |event: &LogEvent| {
        let items = wisp_protocol::todos::listed(event).unwrap();
        assert_eq!(items[0], safe);
        assert_eq!(items[1].text, REDACTED);
        assert_eq!(items[1].section.as_deref(), Some(REDACTED));
        assert_eq!(items[1].status, TodoStatus::Pending);
        assert_eq!(items[2].text, REDACTED);
        assert_eq!(items[2].section.as_deref(), Some(REDACTED));
        assert_eq!(items[2].status, TodoStatus::InProgress);
    };
    for event in [update, child_start] {
        assert_scrubbed(&redactions.scrub_event(event.clone()));
        assert_eq!(log.record(event), None);
    }
    for record in store.read("typed-checklists").unwrap() {
        assert_scrubbed(&record.event);
    }
    let task = "Keep the literal task prose CANARY-in-task";
    let mut text = task.to_owned();
    let mut spans = Vec::new();
    for step in &steps {
        text.push_str("\n- [ ] ");
        let start = text.len();
        text.push_str(step);
        spans.push(wisp_protocol::TextSpan {
            start,
            end: text.len(),
        });
    }
    let accepted = |checklist_spans| LogEvent::InputAccepted {
        turn: None,
        id: InputId("mail-spans".into()),
        mode: DeliveryMode::Steer,
        text: text.clone(),
        images: Vec::new(),
        replaces: None,
        held: false,
        from: None,
        checklist_spans,
    };
    let dir = tempfile::tempdir().unwrap();
    let mut disk = JsonlStore::open(dir.path()).unwrap();
    let id = new_session_id();
    let mut legacy = record(0, started());
    legacy.schema_version = 43;
    disk.append(&id, &legacy).unwrap();
    let path = disk.path(&id);
    let prefix = fs::read(&path).unwrap();
    drop(disk);
    let mut disk = JsonlStore::open(dir.path()).unwrap();
    let reopened = disk.reopen(&id, ResumeLock::Optional).unwrap();
    let mut log = SessionLog::continuing(Box::new(disk), &id, reopened.next_seq);
    assert_eq!(log.record(accepted(spans)), None);
    assert_eq!(
        log.record(accepted(vec![wisp_protocol::TextSpan {
            start: 1,
            end: usize::MAX
        }])),
        None
    );
    assert_eq!(log.sync(), None);
    let raw = fs::read_to_string(path).unwrap();
    assert!(raw.as_bytes().starts_with(&prefix));
    let parsed = parse_jsonl(&raw).unwrap();
    assert_eq!(parsed[0], legacy);
    assert!(
        parsed[1..]
            .iter()
            .all(|r| r.schema_version == LOG_SCHEMA_VERSION)
    );
    let [
        LogEvent::InputAccepted {
            text: safe,
            checklist_spans: spans,
            ..
        },
        LogEvent::InputAccepted {
            text: suppressed, ..
        },
    ] = [&parsed[1].event, &parsed[2].event]
    else {
        panic!("unexpected rows");
    };
    assert_eq!(
        safe,
        &format!("{task}\n- [ ] [redacted]\n- [ ] [redacted]\n- [ ] [redacted]")
    );
    assert!(
        spans
            .iter()
            .all(|span| &safe[span.start..span.end] == REDACTED)
    );
    assert_eq!(suppressed, REDACTED);
    assert!(steps.iter().all(|step| !raw.contains(step.as_str())));
    assert!(
        rebuild_checked(log.store(), &parsed, None)
            .unwrap()
            .is_none_or(|rebuilt| rebuilt.messages.is_empty())
    );
}

#[test]
fn redactions_scrub_every_string_but_the_conversation() {
    register_secrets([
        "/p/secret".to_owned(),
        "fake-CANARY-key-0123456789abcdef".to_owned(),
    ]);
    let redactions = Redactions::new([
        "fake-CANARY-key-0123456789abcdef".to_owned(),
        "/p/secret".to_owned(),
    ]);
    assert_eq!(format!("{redactions:?}"), "Redactions(5 items)");
    let store = MemoryStore::default();
    let mut log = SessionLog::new(Box::new(store.clone()), "s");
    register(&redactions);
    log.record(LogEvent::Rejected {
        turn: Some(1),
        reason: "sent fake-CANARY-key-0123456789abcdef to http://h/p/secret/v1/messages".into(),
        id: None,
        kind: None,
    });
    log.record(LogEvent::TurnStarted {
        ends: Vec::new(),
        endpoint_name: None,
        turn: 2,
        origin: None,
        lead: TurnLead::default(),
        text: "my notes mention /p/secret".into(),
        model: None,
        effort: None,
        effort_marker: None,
        images: Vec::new(),
        input_id: None,
        inputs: Vec::new(),
        cache_ttl: None,
        debug_cache: None,
        refusal_fallback: None,
        binding_controls: None,
        retry_of: None,
        kept_tools_sha256: None,
        tool_message: Vec::new(),
    });
    let committed = LogEvent::MessageCommitted {
        turn: 2,
        message: wisp_protocol::Message::user_text("fake-CANARY-key-0123456789abcdef /p/secret")
            .into(),
        model: None,
        stop_reason: None,
    };
    log.record(committed.clone());
    let mut accepted = one_of_every_event()
        .into_iter()
        .find(|event| matches!(event, LogEvent::InputAccepted { .. }))
        .unwrap();
    if let LogEvent::InputAccepted { text, .. } = &mut accepted {
        *text = "fake-CANARY-key-0123456789abcdef".into();
    }
    log.record(accepted.clone());
    let records = store.read("s").unwrap();
    assert_eq!(
        records[0].event,
        LogEvent::Rejected {
            turn: Some(1),
            reason: "sent [redacted] to http://h[redacted]/v1/messages".into(),
            id: None,
            kind: None,
        }
    );
    assert_eq!(records[2].event, committed);
    assert_eq!(records[3].event, accepted);
    assert!(
        matches!(&records[1].event, LogEvent::TurnStarted { text, .. } if text.ends_with("/p/secret"))
    );
    assert!(log.spend(
        "2026-11-14".parse().unwrap(),
        crate::spend::Record {
            at: now_rfc3339(),
            session: "s".into(),
            account: "fake-CANARY-key-0123456789abcdef".into(),
            model: "/p/secret".into(),
            spend: Default::default(),
        }
    ));
    let record: crate::spend::Record = serde_json::from_str(&store.spend_lines()[0]).unwrap();
    assert_eq!(record.account, REDACTED);
    assert_eq!(record.model, REDACTED);
    let secret = "fake-CANARY-key-0123456789abcdef";
    let source = json!({
        "scope": "project", "name": secret, "display_path": secret, "bytes": 1,
        "sha256": "abc", "imported_by": secret
    });
    let diagnostic = json!({"source": secret, "kind": "large", "message": secret});
    let preset = json!({
        "name": secret, "description": secret, "model": secret, "tools_off": [secret],
        "preload": [secret], "instructions": "instructions", "endpoint": secret
    });
    let config = json!({"preset": secret, "tools_off": [secret], "preset_values": preset});
    let prefix = json!({
        "tool_definitions": [{(secret): secret}], "deferred_definitions": [secret],
        "tool_context": {"platform": secret, "capabilities": [secret]}
    });
    let detail = json!({
        "phase": "stream", "source": "endpoint", "retry": {"type": "never"},
        "error_type": secret, "request_id": secret, "model": secret, "possibly_billed": false,
        "stream": {"bytes": 1, "events": 1, "text_parts": 1, "thinking_parts": 0,
            "tool_use_parts": 0, "last_event_keys": [secret], "partial_event": false,
            "stop_reason_seen": true},
        "malformed_call": {"form": "call", "function": secret, "chars": 12, "balanced": true},
        "limit_reached": {"key": "window", "label": secret, "duration_seconds": 1,
            "scope": "all", "used_percent": 1, "resets_at": null, "observed_at": 0,
            "source": "error_body"}
    });
    let samples: BTreeMap<_, _> = [
        ("session_started", json!({
            "model": secret, "system_prompt": secret, "endpoint": secret, "endpoint_name": secret,
            "tools": [secret], "output_dir": secret,
            "parent": {"session_id": "child", "agent": "/child", "depth": 1,
                "config": config, "todos": [{"text": secret, "status": "pending", "section": secret}]}
        })),
        ("input_held", json!({"by": {"hook": {"hook": "plugin", "reason": secret}}})),
        ("input_withdrawn", json!({"by": {"hook": {"hook": "plugin", "reason": secret}}})),
        ("retrying", json!({"reason": secret, "partial_output": secret, "model": secret, "detail": detail})),
        ("step_ended", json!({"model": secret, "betas": [secret], "thread_error": secret})),
        ("rebuild_marked", json!({"reason": secret})),
        ("attachments_withdrawn", json!({"items": [{"ordinal": 0, "sha256": "abc", "note": secret}]})),
        ("fallback", json!({"from": secret, "to": secret})),
        ("cache_miss", json!({"diagnostics": {(secret): secret}, "cause": {"type": "expected", "reason": secret}})),
        ("cache_miss_repeated", json!({"cause": {"type": "injected_thinking", "first_path": secret, "count": 1}})),
        ("cache_refresh", json!({"detail": secret, "model": secret})),
        ("model_asked", json!({"detail": secret, "model": secret})),
        ("session_named", json!({"name": secret, "detail": secret, "model": secret})),
        ("voice_call", json!({"lines": [secret], "endpoint_name": secret, "model": secret,
            "outcome": {"type": "failed", "kind": "other", "message": secret}})),
        ("unknown_provider_event", json!({"event": {(secret): secret}})),
        ("input_transformations", json!({"transformations": [{"type": "changed", "path": secret,
            "reason": secret, "extra": secret}]})),
        ("rejected", json!({"reason": secret})),
        ("agent_refused", json!({"reason": secret})),
        ("agent_spawned", json!({"model": secret, "endpoint": secret, "provider": secret, "config": config})),
        ("agent_reported", json!({"report": {(secret): secret}})),
        ("turn_ended", json!({"partial_output": secret, "outcome": {
            "status": "error", "kind": "other", "message": secret, "usage": null, "cost": null,
            "detail": detail, "offer": {"retry": true, "switch": false, "focus": "retry", "suggest": secret}
        }})),
        ("tool_call_requested", json!({"input": {(secret): secret}, "display": {
            "action": {"type": "other", "name": secret},
            "verbs": {"doing": secret, "done": secret, "failed": secret},
            "target": secret, "title": secret, "output": {"type": "code", "lang": secret},
            "view": {"type": "diff", "title": secret, "files": [{"path": secret,
                "old_path": secret, "language": secret, "hunks": secret, "added": 1, "removed": 1}]}
        }})),
        ("tool_call_started", json!({"cwd": secret})),
        ("tool_call_finished", json!({
            "outcome": {"status": "failed", "message": secret},
            "output": {"tail": secret, "bytes": 1, "lines": 1, "file": "output",
                "file_problem": secret, "inlined": false},
            "notes": [secret], "attachments": [{"media_type": "text/plain", "sha256": "abc",
                "bytes": 1, "saved": secret}]
        })),
        ("image_lost", json!({"note": secret})),
        ("session_resumed", json!({"endpoint": secret, "endpoint_name": secret, "tools": [secret],
            "system_prompt": secret, "tools_on": [secret]})),
        ("settings_changed", json!({"model": secret, "endpoint_name": secret, "endpoint": secret,
            "account_bound": [secret], "tools": [secret], "changes": [{"target": {"name": "model"},
                "value": {"kind": "text", "text": secret}, "layer": "session", "when": "now",
                "saved": {"path_display": secret, "notes": [secret]}}]})),
        ("per_message_effort", json!({"model": secret, "endpoint_name": secret})),
        ("binding_controls", json!({"model": secret, "endpoint_name": secret})),
        ("native_web_search", json!({"endpoint_name": secret})),
        ("native_compaction", json!({"model": secret, "endpoint_name": secret})),
        ("inline_tools", json!({"model": secret, "endpoint_name": secret})),
        ("deferred_tools", json!({"model": secret, "endpoint_name": secret})),
        ("plugins_changed", json!({"notes": [{"plugin": "plugin", "message": secret}]})),
        ("tools_changed", json!({"added": [secret], "removed": [secret], "system_prompt": secret,
            "tools": [secret], "tools_on": [secret]})),
        ("context_compacted", json!({"focus": secret, "summary": secret, "model": secret,
            "instructions_by": [secret], "strategy": secret, "instruction": secret, "requested_by": secret})),
        ("compaction_prepared", json!({"model": secret})),
        ("compaction_failed", json!({"message": secret, "model": secret, "strategy": secret,
            "instruction": secret, "requested_by": secret, "kind": {"discarded": {"reason": secret}}})),
        ("search_history_rendered", json!({"endpoint_name": secret})),
        ("search_history_unreadable", json!({"endpoint_name": secret})),
        ("system_marker_dropped", json!({"endpoint_name": secret})),
        ("attachments_as_notes", json!({"endpoint_name": secret, "model": secret})),
        ("web_search_not_run", json!({"endpoint_name": secret, "model": secret})),
        ("search_started", json!({"query": secret})),
        ("search_ended", json!({"outcome": {"status": "done", "type": "results", "hits": [{"title": secret,
            "url": secret, "snippet": secret, "page_age": secret}]}})),
        ("citations", json!({"sources": [{"url": secret, "title": secret}]})),
        ("instructions_loaded", json!({"diagnostics": [{"source": "instructions", "kind": "large", "message": secret}]})),
        ("instructions_changed", json!({"from": source, "to": source, "chosen": source, "imports": [source],
            "changed": [{"display_path": secret}], "candidates": [{"scope": "project", "display_path": secret}],
            "diagnostics": [diagnostic], "text": secret})),
        ("process_started", json!({"log": secret})),
        ("process_ended", json!({"end": {"type": "failed", "message": secret}})),
        ("endpoint_saved", json!({"name": secret, "check": {"outcome": "ok", "reason": secret}})),
        ("endpoint_not_saved", json!({"name": secret, "why": {"keychain_failed": secret}})),
        ("endpoint_edited", json!({"name": secret, "check": {"outcome": "ok", "reason": secret}})),
        ("endpoint_removed", json!({"name": secret})),
        ("endpoints_changed", json!({"endpoints": [{"name": secret, "provider": secret, "host": secret,
            "unavailable": secret, "priced": false, "default": false, "key_var": {"name": secret},
            "models": [{"id": secret}], "default_model": secret, "voice_model": secret}]})),
        ("window_lowered", json!({"endpoint": secret, "model": secret})),
        ("todos_updated", json!({"items": [{"text": secret, "status": "pending", "section": secret}]})),
        ("hook_ran", json!({"outcome": {"type": "failed", "message": secret}})),
        ("hook_applied", json!({"change": {"type": "input", "before": {(secret): secret}, "after": secret}})),
    ].into_iter().collect();
    let registered_store = MemoryStore::default();
    let mut registered_log = SessionLog::new(Box::new(registered_store.clone()), "registered");
    for event in one_of_every_event() {
        let kind = kind_of(&event);
        let mut value = serde_json::to_value(event).unwrap();
        if let Some(fields) = samples.get(kind.as_str()) {
            value
                .as_object_mut()
                .unwrap()
                .extend(fields.as_object().unwrap().clone());
        }
        if matches!(
            kind.as_str(),
            "session_started" | "session_resumed" | "tools_changed"
        ) {
            value
                .as_object_mut()
                .unwrap()
                .extend(prefix.as_object().unwrap().clone());
        }
        let variants = match kind.as_str() {
            "voice_call" => vec![("/outcome", json!({"type": "completed", "end": "hangup"}))],
            "tool_call_requested" => vec![
                (
                    "/display/view",
                    json!({"type": "process", "text": secret, "keys": [secret],
                    "read": secret, "pattern": secret}),
                ),
                (
                    "/display/view",
                    json!({"type": "code", "language": secret,
                    "code": format!("code {secret}")}),
                ),
            ],
            "tool_call_finished" => vec![(
                "/outcome",
                json!({"status": "invalid_input", "message": secret}),
            )],
            "turn_ended" => vec![(
                "/outcome",
                json!({"status": "completed", "stop_reason": "end_turn",
                "model": secret, "usage": Usage::default(), "cost": null, "committed": true}),
            )],
            "hook_ran" => vec![
                ("/outcome", json!({"type": "blocked", "reason": secret})),
                ("/outcome", json!({"type": "rejected", "rule": secret})),
            ],
            "hook_applied" => vec![
                (
                    "/change",
                    json!({"type": "result", "before": secret, "after": secret}),
                ),
                (
                    "/change",
                    json!({"type": "text", "before": secret, "after": secret}),
                ),
                ("/change", json!({"type": "continue", "text": secret})),
                ("/change", json!({"type": "blocked", "reason": secret})),
            ],
            "search_ended" => vec![(
                "/outcome",
                json!({"status": "done", "type": "failed",
                "failure": "unavailable", "code": secret}),
            )],
            "endpoint_not_saved" => vec![
                (
                    "/why",
                    json!({"check": {"outcome": "ok", "reason": secret}}),
                ),
                ("/why", json!({"future": secret})),
            ],
            "settings_changed" => vec![
                ("/changes/0/value", json!({"kind": "list", "text": secret})),
                (
                    "/changes/0/value",
                    json!({"kind": "future", "text": secret}),
                ),
            ],
            "cache_miss" => vec![
                (
                    "/cause",
                    json!({"type": "provider_dropped_thinking", "first_path": secret, "count": 1}),
                ),
                ("/cause", json!({"type": "future", "content": secret})),
            ],
            _ => Vec::new(),
        };
        let mut examples = vec![value.clone()];
        for (pointer, sample) in variants {
            let mut example = value.clone();
            *example.pointer_mut(pointer).unwrap() = sample;
            examples.push(example);
        }
        for example in examples {
            let event: LogEvent = serde_json::from_value(example).unwrap();
            let mut expected = serde_json::to_value(&event).unwrap();
            redactions.scrub_value(&mut expected);
            let detail = match kind.as_str() {
                "retrying" => expected.pointer_mut("/detail"),
                "turn_ended" => expected.pointer_mut("/outcome/detail"),
                _ => None,
            };
            if let Some(serde_json::Value::Object(detail)) = detail {
                assert_eq!(detail.remove("limit_reached").unwrap()["label"], REDACTED);
            }
            assert!(
                !serde_json::to_string(&expected).unwrap().contains(secret),
                "{kind}"
            );
            assert_eq!(registered_log.record(event.clone()), None, "{kind}");
            let logged = registered_store.read("registered").unwrap().pop().unwrap();
            assert_eq!(
                serde_json::to_value(logged.event).unwrap(),
                expected,
                "registered {kind}"
            );
            let event = redactions.scrub_event(event);
            assert_eq!(serde_json::to_value(event).unwrap(), expected, "{kind}");
        }
    }
    for host in ["server", "localhost"] {
        let url = format!("http://{host}:11434/v1");
        let redactions = crate::secrets::variable_redactions(
            None,
            [],
            &|name| (name == "OPENAI_BASE_URL").then(|| url.clone()),
            None,
        );
        let store = MemoryStore::default();
        let mut log = SessionLog::new(Box::new(store.clone()), "s");
        register(&redactions);
        let mut start = started();
        if let LogEvent::SessionStarted { endpoint, .. } = &mut start {
            *endpoint = format!("http://{host}:11434");
        }
        let note = LogEvent::Rejected {
            turn: None,
            reason: "curl http://localhost:3000/health failed; see localhost docs".to_owned(),
            id: None,
            kind: None,
        };
        let failed = crate::test_guard::protocol_fixture("log_turn_ended_error").event;
        let events = vec![start, note.clone(), failed, note];
        for event in &events {
            assert_eq!(log.record(event.clone()), None, "{host}");
        }
        assert!(log.is_on(), "{host}");
        assert_eq!(
            store
                .read("s")
                .unwrap()
                .into_iter()
                .map(|record| record.event)
                .collect::<Vec<_>>(),
            events,
            "{host}"
        );
    }
}

#[cfg(unix)]
#[test]
fn session_files_never_follow_symlinks() {
    let dir = temp_dir("symlink");
    let mut store = JsonlStore::open(&dir).unwrap();
    let id = new_session_id();
    let target = dir.join("elsewhere.txt");
    fs::write(&target, "").unwrap();
    std::os::unix::fs::symlink(&target, store.path(&id)).unwrap();
    assert!(store.append(&id, &record(0, started())).is_err());
    assert_eq!(fs::read_to_string(&target).unwrap(), "");

    let id = new_session_id();
    let missing = dir.join("missing.txt");
    std::os::unix::fs::symlink(&missing, store.path(&id)).unwrap();
    assert!(store.append(&id, &record(0, started())).is_err());
    assert!(!missing.exists());

    let id = new_session_id();
    std::os::unix::fs::symlink("/dev/null", store.path(&id)).unwrap();
    let mut log = SessionLog::new(Box::new(JsonlStore::open(&dir).unwrap()), id);
    assert!(log.record(started()).is_some());
    let sessions = dir.join("sessions");
    let mut store = JsonlStore::open(&sessions).unwrap();
    let day = "2026-11-14".parse().unwrap();
    let folder = crate::spend::folder(&sessions);
    let path = crate::spend::file(&sessions, day);
    std::os::unix::fs::symlink(&dir, &folder).unwrap();
    assert!(store.append_spend(day, "{}\n").is_err());
    fs::remove_file(&folder).unwrap();
    create_private_dir(&folder).unwrap();
    for target in [&target, &missing, Path::new("/dev/null")] {
        std::os::unix::fs::symlink(target, &path).unwrap();
        assert!(store.append_spend(day, "{}\n").is_err());
        fs::remove_file(&path).unwrap();
    }
    assert_eq!(fs::read_to_string(&target).unwrap(), "");
    assert!(!missing.exists());
    fs::remove_dir_all(&dir).unwrap();
}

#[cfg(unix)]
#[test]
fn an_existing_session_file_is_tightened_to_600() {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp_dir("mode644");
    let mut store = JsonlStore::open(&dir).unwrap();
    let id = new_session_id();
    fs::write(store.path(&id), "").unwrap();
    fs::set_permissions(store.path(&id), fs::Permissions::from_mode(0o644)).unwrap();
    store.append(&id, &record(0, started())).unwrap();
    let mode = fs::metadata(store.path(&id)).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert_eq!(store.read(&id).unwrap().len(), 1);
    let id = new_session_id();
    fs::create_dir(store.path(&id)).unwrap();
    assert!(store.append(&id, &record(0, started())).is_err());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn session_ids_cannot_escape_the_directory() {
    let dir = temp_dir("esc");
    let mut store = JsonlStore::open(&dir).unwrap();
    for id in ["../x", "a/b", "", "x.jsonl"] {
        assert!(store.append(id, &record(0, started())).is_err(), "{id}");
        assert!(store.read(id).is_err(), "{id}");
    }
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn default_dir_follows_xdg() {
    assert_eq!(
        JsonlStore::default_dir(None, Some("/home/u".into())),
        Some(PathBuf::from("/home/u/.local/share/wisp/sessions"))
    );
    assert_eq!(
        JsonlStore::default_dir(Some("/data".into()), Some("/home/u".into())),
        Some(PathBuf::from("/data/wisp/sessions"))
    );
    assert_eq!(
        JsonlStore::default_dir(Some("rel".into()), Some("/home/u".into())),
        Some(PathBuf::from("/home/u/.local/share/wisp/sessions"))
    );
    assert_eq!(JsonlStore::default_dir(None, None), None);
}

#[test]
fn ids_timestamps_and_hashes() {
    let at = "2026-11-14T01:00:00Z".parse().unwrap();
    for (zone, expected) in [
        (None, "2026-11-14"),
        (Some("America/New_York"), "2026-11-13"),
    ] {
        let clock = crate::clock::ShiftedClock::at(
            at,
            zone.map(|name| jiff::tz::TimeZone::get(name).unwrap()),
        );
        assert_eq!(crate::spend::day(at, &clock).to_string(), expected);
    }
    let a = new_session_id();
    let b = new_session_id();
    assert!(a < b, "{a} {b}");
    assert_eq!(&a[14..15], "7", "UUIDv7: {a}");
    let at = now_rfc3339();
    assert_eq!(at.len(), 24, "{at}");
    assert!(
        at.ends_with('Z') && &at[10..11] == "T" && &at[19..20] == ".",
        "{at}"
    );
    assert_eq!(
        sha256_hex("abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(wisp_plugin_api::hex::encode(&[]), "");
    assert_eq!(
        wisp_plugin_api::hex::encode(&[0, 1, 15, 16, 127, 128, 255]),
        "00010f107f80ff"
    );
    assert_eq!(
        sha256_bytes_hex(&[]),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_bytes_hex(&[0, 128, 255]),
        "5240672d7b51756b829ad0ef8d9468b7a078afa2f410484fd3892dab47becb72"
    );
}

#[test]
fn session_log_numbers_records_and_stops_after_a_failure() {
    let store = MemoryStore::default();
    let mut log = SessionLog::new(Box::new(store.clone()), "s");
    assert_eq!(log.record(started()), None);
    assert_eq!(
        log.record(LogEvent::SessionEnded {
            turns: 0,
            interrupted: None
        }),
        None
    );
    let seqs: Vec<u64> = store.read("s").unwrap().iter().map(|r| r.seq).collect();
    assert_eq!(seqs, [0, 1]);

    let failing = MemoryStore::failing();
    let mut log = SessionLog::new(Box::new(failing), "s");
    let reason = log.record(started()).unwrap();
    assert!(reason.contains("disk full"), "{reason}");
    assert_eq!(log.record(started()), None);
    assert_eq!(log.sync(), None);
}

#[test]
fn a_tail_cut_inside_a_multibyte_character_is_skipped() {
    let records: Vec<LogRecord> = (0..3)
        .map(|i| {
            record(
                i,
                LogEvent::TurnStarted {
                    ends: Vec::new(),
                    endpoint_name: None,
                    turn: i,
                    origin: None,
                    lead: TurnLead::default(),
                    text: format!("中文 {i} 😀 é 粗体"),
                    model: None,
                    effort: None,
                    effort_marker: None,
                    images: Vec::new(),
                    input_id: None,
                    inputs: Vec::new(),
                    cache_ttl: None,
                    debug_cache: None,
                    refusal_fallback: None,
                    binding_controls: None,
                    retry_of: None,
                    kept_tools_sha256: None,
                    tool_message: Vec::new(),
                },
            )
        })
        .collect();
    let full: Vec<u8> = records
        .iter()
        .flat_map(|r| jsonl_line(r).unwrap().into_bytes())
        .collect();
    let last_start = full.len() - jsonl_line(&records[2]).unwrap().len();
    let mut split_chars = 0;
    for cut in last_start..full.len() {
        let bytes = &full[..cut];
        split_chars += usize::from(std::str::from_utf8(bytes).is_err());
        assert_eq!(parse_jsonl(bytes).unwrap(), records[..2], "cut at {cut}");
    }
    assert!(split_chars > 10, "the cuts must split characters");
    assert_eq!(parse_jsonl(&full).unwrap(), records);

    let dir = temp_dir("utf8");
    let store = JsonlStore::open(&dir).unwrap();
    let id = new_session_id();
    let cut = (last_start..full.len())
        .find(|c| std::str::from_utf8(&full[..*c]).is_err())
        .unwrap();
    fs::write(store.path(&id), &full[..cut]).unwrap();
    assert_eq!(store.read(&id).unwrap(), records[..2]);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn invalid_utf8_in_a_complete_line_is_corruption() {
    let full = lines();
    let mut bytes = format!("{}\n", full[0]).into_bytes();
    let mut bad = full[1].clone().into_bytes();
    let at = bad.len() / 2;
    bad[at] = 0xff;
    bytes.extend(bad);
    bytes.push(b'\n');
    let err = parse_jsonl(&bytes).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("line 2"), "{err}");
}

#[cfg(unix)]
#[test]
fn a_fifo_at_the_session_path_fails_instead_of_blocking() {
    let dir = temp_dir("fifo");
    let mut store = JsonlStore::open(&dir).unwrap();
    let id = new_session_id();
    let made = std::process::Command::new("mkfifo")
        .arg(store.path(&id))
        .status()
        .is_ok_and(|s| s.success());
    if !made {
        eprintln!("mkfifo unavailable; skipping");
        return;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let path_id = id.clone();
    std::thread::spawn(move || {
        let result = store.append(&path_id, &record(0, started()));
        let _ = tx.send(result.is_err());
    });
    let failed = rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("append blocked on a FIFO");
    assert!(failed);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn records_from_before_message_stamps_still_parse() {
    let old = concat!(
        r#"{"schema_version":1,"seq":0,"at":"2026-10-21T19:15:02.481Z","type":"session_started","session_id":"0199-abc","wisp_version":"0.1.0","model":"m","effort":"medium","cache_ttl":"one_hour","system_prompt":null,"system_prompt_sha256":null,"endpoint":"https://api.anthropic.com","credential_source":"environment"}"#,
        "\n",
        r#"{"schema_version":1,"seq":1,"at":"2026-10-21T19:15:03.481Z","type":"turn_started","turn":1,"text":"hi"}"#,
        "\n",
    );
    let records = parse_jsonl(old).unwrap();
    assert!(matches!(
        records[0].event,
        LogEvent::SessionStarted {
            message_time: None,
            endpoint_name: None,
            ..
        }
    ));
    assert_eq!(
        records[1].event,
        LogEvent::TurnStarted {
            ends: Vec::new(),
            endpoint_name: None,
            turn: 1,
            origin: None,
            lead: TurnLead::default(),
            text: "hi".into(),
            model: None,
            effort: None,
            effort_marker: None,
            images: Vec::new(),
            input_id: None,
            inputs: Vec::new(),
            cache_ttl: None,
            debug_cache: None,
            refusal_fallback: None,
            binding_controls: None,
            retry_of: None,
            kept_tools_sha256: None,
            tool_message: Vec::new(),
        }
    );
    let line = jsonl_line(&records[1]).unwrap();
    assert!(!line.contains("stamp"), "{line}");
}

#[test]
fn redacting_colliding_json_keys_keeps_nested_values_and_is_idempotent() {
    let redactions = Redactions::new([
        "CANARYJsonKeyOne".into(),
        "CANARYJsonKeyTwo".into(),
        " (2)".into(),
    ]);
    let mut value = json!({"outer": [{
        "CANARYJsonKeyOne": 1,
        "CANARYJsonKeyTwo": 2,
        (REDACTED): "literal",
        "[redacted] (3)": "already present"
    }]});
    let expected = json!({"outer": [{
        (REDACTED): "literal",
        "[redacted] (3)": "already present",
        "[redacted] (4)": 1,
        "[redacted] (5)": 2
    }]});
    for _ in 0..2 {
        redactions.scrub_value(&mut value);
        assert_eq!(value, expected);
    }
    let mut nested = json!({"outer": ["prefix CANARYJsonKeyOne", 42]});
    let overlapping = Redactions::new(["CANARYJsonKeyOne".into(), "redact".into()]);
    for _ in 0..2 {
        overlapping.scrub_value(&mut nested);
        assert_eq!(nested, json!({"outer": [REDACTED, 42]}));
    }
    let mut no_safe_key = json!({"outer": {"CANARYJsonKeyOne": 1, (REDACTED): 2}});
    Redactions::new(["CANARYJsonKeyOne".into(), "redacted".into()]).scrub_value(&mut no_safe_key);
    assert_eq!(no_safe_key, json!({"outer": REDACTED}));
}

#[test]
fn overlapping_redactions_scrub_fully() {
    let entries = Redactions::typed(["AAA", "act", "][r"].map(str::to_owned));
    assert_eq!(entries.mask("AAAAAA"), REDACTED);
    assert_eq!(entries.scrub("AAAAAA"), REDACTED);
    let entries = Redactions::typed(["abcXYZ", "zabc"].map(str::to_owned));
    assert_eq!(entries.mask("zabcXYZ"), REDACTED);
    assert_eq!(entries.scrub("zabcXYZ"), REDACTED);
    let key = "sk-CANARY-private-key-credential";
    let entries = Redactions::new([key.to_owned()]).union([&Redactions::typed(["=sk".to_owned()])]);
    assert_eq!(
        entries.mask(&format!("key={key}")),
        format!("key{REDACTED}")
    );
    assert_eq!(
        entries.scrub(&format!("key={key}")),
        format!("key{REDACTED}")
    );
    for items in [["abc", "abcXYZ"], ["abcXYZ", "abc"]] {
        let redactions = Redactions::new(items.map(str::to_owned));
        assert_eq!(
            redactions.scrub("token abcXYZ"),
            format!("token {REDACTED}")
        );
        assert_eq!(
            redactions.scrub("abc abcXYZ"),
            format!("{REDACTED} {REDACTED}")
        );
    }
}

#[test]
fn redactions_require_three_non_whitespace_characters_before_debug_escaping() {
    assert_eq!(MIN_REDACTION_CHARS, 3);
    let padded = format!(" a b{}", " ".repeat(wisp_protocol::KEY_SHAPED_MIN));
    for short in [
        "",
        "e",
        "ab",
        "  ",
        "\n\t",
        " a b ",
        "é🙂",
        "\"\\",
        padded.as_str(),
    ] {
        for redactions in [
            Redactions::new([short.to_owned()]),
            Redactions::log_only([short.to_owned()]),
        ] {
            assert!(redactions.is_empty(), "{short:?}");
            assert_eq!(redactions.scrub(short), short);
            assert_eq!(redactions.mask(short), short);
        }
        register_secrets([short.to_owned()]);
        assert_eq!(without_registered_secrets(short), short);
    }
    for value in ["abc", "é🙂x", " a b c "] {
        for redactions in [
            Redactions::new([value.to_owned()]),
            Redactions::log_only([value.to_owned()]),
        ] {
            assert_eq!(redactions.scrub(value), REDACTED);
            assert_eq!(redactions.mask(value), value);
        }
        register_secrets([value.to_owned()]);
        assert_eq!(without_registered_secrets(value), REDACTED);
    }
}

#[test]
fn short_per_call_redactions_leave_every_event_unchanged() {
    let redactions = Redactions::new(["e".to_owned()]);
    let events = one_of_every_event();
    for event in &events {
        assert_eq!(redactions.scrub_event(event.clone()), *event);
    }
    let redactions = Redactions::new(
        events.iter().map(kind_of).chain(
            [
                "proven",
                "fallen_back",
                "environment",
                "cancel",
                "text",
                "run",
            ]
            .map(str::to_owned),
        ),
    );
    let store = MemoryStore::default();
    let mut log = SessionLog::new(Box::new(store.clone()), "s");
    register(&redactions);
    for event in &events {
        assert_eq!(log.record(event.clone()), None);
    }
    assert!(log.is_on());
    let records = store.read("s").unwrap();
    let text: String = records
        .iter()
        .map(|record| jsonl_line(record).unwrap())
        .collect();
    assert_eq!(parse_jsonl(text).unwrap(), records);
    for (record, event) in records.iter().zip(events) {
        assert_eq!(kind_of(&record.event), kind_of(&event));
    }
}

#[test]
fn short_registered_values_keep_record_types_and_logging_enabled() {
    register_secrets(["e".to_owned()]);
    let store = MemoryStore::default();
    let mut log = SessionLog::new(Box::new(store.clone()), "s");
    let rejected = LogEvent::Rejected {
        turn: Some(1),
        reason: "the request was rejected".into(),
        id: None,
        kind: None,
    };
    let mut events = vec![rejected.clone(), rejected];
    events.extend(one_of_every_event());
    for event in &events {
        assert_eq!(log.record(event.clone()), None);
    }
    assert!(!log.failed, "scrubbing must not disable logging");
    let records = store.read("s").unwrap();
    assert_eq!(records.len(), events.len());
    for (record, event) in records.iter().zip(events) {
        assert_eq!(record.event, event);
    }
}

#[test]
fn redactions_preserve_nested_enum_values_and_keep_logging_enabled() {
    let event = one_of_every_event()
        .into_iter()
        .find(|event| matches!(event, LogEvent::ToolCallFinished { .. }))
        .unwrap();
    let redactions = Redactions::new(["cancel".to_owned()]);
    assert_eq!(redactions.scrub("cancelled"), "[redacted]led");
    let scrubbed = redactions.scrub_event(event.clone());
    assert_eq!(scrubbed, event);
    let store = MemoryStore::default();
    let mut log = SessionLog::new(Box::new(store.clone()), "s");
    register(&redactions);
    assert_eq!(log.record(event.clone()), None);
    assert_eq!(log.record(started()), None);
    assert!(!log.failed, "a nested enum must not disable logging");
    assert_eq!(store.read("s").unwrap()[0].event, event);
}

#[test]
fn late_redactions_omit_only_affected_usage_windows_and_keep_retry_records_readable() {
    use wisp_protocol::plan_usage::{
        RETRY_AFTER_KEY, UsageScope, UsageSource, UsageWindow, WINDOWS,
    };
    use wisp_protocol::{
        ErrorDetail, ErrorKind, FailurePhase, FailureSource, RetryClass, RetryFocus, RetryOffer,
        TurnOutcome, TurnStats,
    };

    let dir = tempfile::tempdir().unwrap();
    let mut store = JsonlStore::open(dir.path()).unwrap();
    let id = new_session_id();
    let path = store.path(&id);
    let mut log = SessionLog::new(Box::new(JsonlStore::open(dir.path()).unwrap()), &id);
    let now = 1_800_000_000;
    let retry_at = now + 508_200;
    let guard = UsageWindow {
        key: RETRY_AFTER_KEY.into(),
        label: "Retry after".into(),
        duration_seconds: None,
        scope: UsageScope::Model("example-model".into()),
        used_percent: None,
        resets_at: Some(retry_at),
        observed_at: now,
        source: UsageSource::RetryAfter,
    };
    let reading = UsageWindow {
        key: "example-weekly".into(),
        label: "Example weekly".into(),
        duration_seconds: Some(WINDOWS[1].0),
        scope: UsageScope::ModelFamily("Example".into()),
        used_percent: Some(100.0.try_into().unwrap()),
        resets_at: Some(now + 3600),
        source: UsageSource::UsageEndpoint,
        ..guard.clone()
    };
    let mut detail = ErrorDetail::new(
        FailurePhase::Response,
        FailureSource::Endpoint,
        RetryClass::After { ms: 508_200_000 },
    );
    detail.status = Some(429);
    detail.limit_reached = Some(Box::new(guard));
    detail.model_limit = Some(Box::new(reading));
    let retrying = one_of_every_event()
        .into_iter()
        .find(|event| matches!(event, LogEvent::Retrying { .. }))
        .unwrap();
    let events = |detail: ErrorDetail| {
        let mut retrying = retrying.clone();
        if let LogEvent::Retrying {
            detail: saved,
            delay_ms,
            kind,
            ..
        } = &mut retrying
        {
            *saved = Some(detail.clone());
            *delay_ms = 508_200_000;
            *kind = Some(ErrorKind::RateLimit);
        }
        [
            retrying,
            LogEvent::TurnEnded {
                turn: 1,
                stats: TurnStats::default(),
                partial_output: None,
                outcome: TurnOutcome::Error {
                    kind: ErrorKind::RateLimit,
                    message: "example refusal".into(),
                    usage: None,
                    cost: None,
                    committed_steps: 0,
                    detail: Some(detail),
                    attempts: 2,
                    gave_up: None,
                    offer: Some(RetryOffer {
                        retry: true,
                        switch: true,
                        focus: RetryFocus::Switch,
                        suggest: None,
                        retries_at: Some(retry_at + 60),
                    }),
                },
            },
        ]
    };
    let mut expected = Vec::from(events(detail.clone()));
    for len in [96, 97, 128] {
        let mut scoped = detail.clone();
        scoped.limit_reached.as_mut().unwrap().scope = UsageScope::Model("m".repeat(len));
        expected.extend(events(scoped));
    }
    for event in &expected {
        assert_eq!(log.record(event.clone()), None);
    }
    let mut hidden = vec![detail.clone(); 4];
    hidden[0].model_limit.as_mut().unwrap().key = "example-CANARY-734-log-key".into();
    hidden[1].model_limit.as_mut().unwrap().label = "Example CANARY-734-log-label weekly".into();
    hidden[2].model_limit.as_mut().unwrap().scope =
        UsageScope::ModelFamily("CANARY-734-log-family".into());
    hidden[3].limit_reached.as_mut().unwrap().scope =
        UsageScope::Model("example-CANARY-734-log-model".into());
    assert!(
        hidden
            .iter()
            .all(|d| d.limit_reached.as_ref().unwrap().valid()
                && d.model_limit.as_ref().unwrap().valid())
    );
    register(&Redactions::new(["CANARY-734-log".to_owned()]));
    for (i, given) in hidden.into_iter().enumerate() {
        for event in events(given) {
            assert_eq!(log.record(event), None);
        }
        let mut wanted = detail.clone();
        if i == 3 {
            wanted.limit_reached = None;
        } else {
            wanted.model_limit = None;
        }
        expected.extend(events(wanted));
    }
    assert_eq!(log.sync(), None);
    drop(log);
    let text = fs::read_to_string(path).unwrap();
    assert!(!text.contains("CANARY-734-log"));
    assert!(!text.contains(REDACTED));
    let reopened = store.reopen(&id, ResumeLock::Optional).unwrap();
    assert_eq!(reopened.truncated_bytes, 0);
    let reopened: Vec<_> = reopened
        .lines
        .into_iter()
        .map(|line| match line {
            Line::Record(record) => record.event,
            _ => panic!("expected a valid retry or terminal record"),
        })
        .collect();
    assert_eq!(reopened, expected);
}

#[test]
fn tool_result_masks_only_held_credentials_at_the_length_floor() {
    let key = format!(
        "CANARY{}\\\"",
        "x".repeat(wisp_protocol::KEY_SHAPED_MIN - 8)
    );
    let short = format!("CANARY{}", "\\".repeat(wisp_protocol::KEY_SHAPED_MIN - 7));
    let unicode = format!("CANARY{}", "é".repeat(wisp_protocol::KEY_SHAPED_MIN - 7));
    let path = "/CANARY-long-account-path";
    let project = "CANARY-long-project-identifier";
    let redactions = Redactions::new([key.clone(), short.clone(), unicode.clone()]).union([
        &Redactions::log_only([
            path.to_owned(),
            project.to_owned(),
            "/v1".to_owned(),
            key.clone(),
        ]),
    ]);
    let escaped = |value: &str| {
        let printed = format!("{value:?}");
        printed[1..printed.len() - 1].to_owned()
    };
    let text = format!(
        "{key} {} {short} {} {unicode} {path} {project} /v1",
        escaped(&key),
        escaped(&short)
    );
    let expected = format!(
        "{REDACTED} {REDACTED} {short} {} {unicode} {path} {project} /v1",
        escaped(&short)
    );
    assert_eq!(redactions.mask(&text), expected);
    assert_eq!(redactions.mask(&expected), expected);
    assert_eq!(redactions.mask("untouched"), "untouched");
    assert_eq!(redactions.scrub(path), REDACTED);
    assert_eq!(redactions.scrub(project), REDACTED);
    assert_eq!(redactions.scrub(&short), REDACTED);
    let duplicate = Redactions::log_only([key.clone()]).union([&redactions]);
    assert_eq!(duplicate, redactions);
    assert_eq!(duplicate.mask(&key), REDACTED);
    for length in [21, 22, 23] {
        let fake = format!("CANARY{}", "x".repeat(length - 6));
        let redactions = Redactions::new([fake.clone()]);
        for prefix in ["", ":", "u:"] {
            let encoded =
                base64::engine::general_purpose::STANDARD.encode(format!("{prefix}{fake}:suffix"));
            let masked = redactions.mask(&encoded);
            assert!(
                masked.contains(REDACTED),
                "length {length}, prefix {prefix:?}"
            );
            assert_eq!(redactions.scrub(&encoded), masked);
            for form in base64_forms(&fake) {
                assert!(
                    !masked.contains(&form),
                    "length {length}, prefix {prefix:?}"
                );
            }
        }
    }
    for value in [short.as_str(), path] {
        let encoded = base64::engine::general_purpose::STANDARD.encode(value);
        assert_eq!(redactions.mask(&encoded), encoded);
        assert_eq!(redactions.scrub(&encoded), encoded);
    }
}

#[test]
fn secret_checks_respect_redaction_kind_and_boundaries() {
    let short = "CANARYpin";
    let long = "CANARYlongheldcredential";
    let log = "/CANARY-logged";
    let held = Redactions::new([short.to_owned(), "tag:CANARYpin".to_owned()]);
    let project = Redactions::held_only([long.to_owned()]);
    let log_only = Redactions::log_only([log.to_owned(), short.to_owned(), long.to_owned()]);
    let all = log_only.union([&held, &project]);
    for text in [
        "CANARYpin",
        r"key\u003dCANARYpin",
        "notag:CANARYpin",
        "preCANARYlongheldcredentialpost",
    ] {
        assert!(all.has_secret_check_match(text), "{text}");
    }
    for text in [log, "preCANARYpin", "CANARYpin_old"] {
        assert!(!all.has_secret_check_match(text), "{text}");
        assert!(all.has_match(text, RedactionKind::LogOnly), "{text}");
    }
    assert!(!log_only.has_secret_check_match(short));
    assert!(!all.has_match(long, RedactionKind::Masked));
    assert!(!all.has_match(log, RedactionKind::Held));
    assert!(all.has_match(short, RedactionKind::Held));
    assert_eq!(all.mask(long), long);
    assert_eq!(all.scrub(long), REDACTED);

    let typed = Redactions::typed([short.to_owned()]);
    let strongest = all.union([&typed, &held, &log_only]);
    assert!(strongest.has_secret_check_match("preCANARYpin"));
    assert!(strongest.has_match(short, RedactionKind::Masked));
    assert_eq!(strongest.mask(short), REDACTED);
    let marker = Redactions::new([REDACTED.to_owned()]);
    assert!(marker.has_match(REDACTED, RedactionKind::Held));
    assert!(marker.has_secret_check_match(REDACTED));
}

#[test]
fn registered_redaction_generation_changes_only_with_the_set() {
    let value = "CANARYgenerationupdate";
    let before = held_generation();
    let log_only = Redactions::log_only([value.to_owned()]);
    register(&log_only);
    let after = held_generation();
    assert!(after > before);
    register(&log_only);
    assert_eq!(held_generation(), after);
    let upgraded = Redactions::held_only([value.to_owned()]);
    register(&upgraded);
    assert!(held_generation() > after);
    assert!(held().has_secret_check_match(value));
}

#[test]
fn byte_redactions_keep_non_utf8_and_typed_values_stay_local() {
    let fake = "CANARY-byte-mask-credential";
    let redactions = Redactions::new([fake.to_owned()]);
    let bytes = [b"\xff".as_slice(), fake.as_bytes(), b"\xfe"].concat();
    assert_eq!(
        redactions.mask_bytes(&bytes),
        Some([b"\xff".as_slice(), REDACTED.as_bytes(), b"\xfe"].concat())
    );
    assert_eq!(redactions.mask_bytes(b"\xffuntouched\xfe"), None);
    let mut pending = MaskTail::default();
    assert_eq!(
        redactions.mask_chunk(b"prefix CANARY-byte", &mut pending),
        (b"prefix ".to_vec(), false)
    );
    assert_eq!(
        redactions.release(&mut pending),
        (b"CANARY-byte".to_vec(), false)
    );
    assert!(pending.bytes.is_empty());
    assert_eq!(
        redactions.mask_chunk(b"nothing", &mut pending),
        (b"nothing".to_vec(), false)
    );
    let typed = Redactions::typed(["CANARYabc".to_owned(), "xy".to_owned()]);
    assert_eq!(typed.mask("CANARYabc xy"), format!("{REDACTED} xy"));
    assert_eq!(
        typed.mask_chunk(b"CANARY", &mut pending),
        (Vec::new(), false)
    );
    assert_eq!(typed.release(&mut pending), (b"CANARY".to_vec(), false));
    assert_eq!(redactions.mask("CANARYabc"), "CANARYabc");
}

#[test]
fn redactions_also_catch_a_secret_escaped_by_debug_output() {
    let secret = r#"sk-"quoted"\back"#;
    let redactions = Redactions::new([secret.to_owned()]);
    let printed = format!("{:?}", format!("error: {secret}"));
    assert_eq!(redactions.scrub(&printed), r#""error: [redacted]""#);
    assert_eq!(redactions.scrub(secret), "[redacted]");
    assert_eq!(redactions.union([&redactions, &redactions]), redactions);
    let registered = || format!("{:?}", *process_secrets().read().unwrap());
    register_secrets([secret.to_owned()]);
    let once = registered();
    for _ in 0..3 {
        register_secrets([secret.to_owned()]);
    }
    assert_eq!(registered(), once);
}

#[test]
fn scrub_catches_a_secret_spelled_with_unicode_escapes() {
    let secret = "CANARY-a\"b\\c-\u{1F642}-unicode";
    let redactions = Redactions::new([secret.to_owned()]);
    let every = |upper: bool| -> String {
        secret
            .encode_utf16()
            .map(|unit| {
                if upper {
                    format!("\\u{unit:04X}")
                } else {
                    format!("\\u{unit:04x}")
                }
            })
            .collect()
    };
    let single = r#"\u0043ANARY-a\"b\\c-🙂-unicode"#;
    for spelled in [
        single.to_owned(),
        r#"CANARY-a\u0022b\\c-🙂-unicode"#.to_owned(),
        r#"CANARY-a\"b\\c-\uD83D\ude42-unicode"#.to_owned(),
        r#"\u{43}ANARY-a\"b\\c-\u{1f642}-unicode"#.to_owned(),
        every(true),
        every(false),
    ] {
        let text = format!("key={spelled}.");
        assert_eq!(redactions.scrub(&text), format!("key={REDACTED}."));
        assert_eq!(redactions.mask(&text), format!("key={REDACTED}."));
        assert_eq!(Redactions::log_only([secret.to_owned()]).mask(&text), text);
        assert_ne!(Redactions::log_only([secret.to_owned()]).scrub(&text), text);
    }
    let mut invalid = b"\xff key=".to_vec();
    invalid.extend_from_slice(single.as_bytes());
    invalid.extend_from_slice(b" \xc3\\");
    let mut masked = b"\xff key=".to_vec();
    masked.extend_from_slice(REDACTED.as_bytes());
    masked.extend_from_slice(b" \xc3\\");
    assert_eq!(redactions.mask_bytes(&invalid), Some(masked));
    assert_eq!(redactions.mask_bytes(b"\xff\\u0043\xc3\\"), None);
    assert_eq!(redactions.scrub(&format!("{secret}{single}")), REDACTED);
    assert_eq!(
        redactions.scrub(&format!("{single} {secret}")),
        format!("{REDACTED} {REDACTED}")
    );
    let wrapped = |text: String, times: usize| {
        (0..times).fold(text, |text, _| serde_json::to_string(&text).unwrap())
    };
    for times in 0..ESCAPE_LEVELS {
        let text = wrapped(format!("key={single}."), times);
        assert_eq!(
            redactions.scrub(&text),
            wrapped(format!("key={REDACTED}."), times)
        );
        assert_eq!(redactions.mask(&text), redactions.scrub(&text));
    }
    let beyond = wrapped(format!("key={single}."), ESCAPE_LEVELS);
    assert_eq!(redactions.scrub(&beyond), beyond);
    assert_eq!(redactions.mask(&beyond), beyond);
    let rest = r#"ANARY-a\"b\\c-🙂-unicode"#;
    let mut unchanged = [
        r"\uD800",
        r"\uDC43",
        r"\u00G3",
        r"\u{110043}",
        r"\u{}",
        r"\u{0000043}",
        r"\u{D800}",
        r"\u{4G}",
        r"\u{+43}",
        r"\x43",
        r"\u43",
        r"\u+043",
    ]
    .map(|first| format!("key={first}{rest}."))
    .to_vec();
    unchanged.extend([
        single.replacen("unicode", r"unicod\", 1),
        r#"line\n\u0041 \u{1F600} \"quoted\" \\ \/"#.to_owned(),
        r"\u0043ANARY-a".to_owned(),
        r"\u{".repeat(100_000),
        r"\u{1234567".repeat(100_000),
    ]);
    for text in unchanged {
        assert_eq!(redactions.scrub(&text), text);
        assert_eq!(redactions.mask(&text), text);
    }
}

fn png_like(tag: &str) -> (String, Vec<u8>) {
    let bytes = format!("\u{89}PNG fake image {tag}").into_bytes();
    (sha256_bytes_hex(&bytes), bytes)
}

fn png() -> Mime {
    mime("image/png")
}

fn image_store(name: &str) -> (PathBuf, JsonlStore) {
    let root = temp_dir(name);
    let store = JsonlStore::open(root.join("wisp").join("sessions")).unwrap();
    (root, store)
}

fn leftovers(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn attachments_live_beside_sessions_named_by_hash_and_subtype() {
    let (root, mut store) = image_store("blob-names");
    assert_eq!(
        store.attachments_dir(),
        root.join("wisp").join("attachments")
    );
    assert_eq!(store.images_dir(), root.join("wisp").join("images"));
    assert!(
        !store.attachments_dir().exists(),
        "made only when an attachment is saved"
    );
    let (png_sha, png_bytes) = png_like("a");
    let (jpg_sha, jpg_bytes) = png_like("b");
    let (pdf_sha, pdf_bytes) = png_like("c");
    let (odd_sha, odd_bytes) = png_like("d");
    store.put_blob(&png_sha, &png(), &png_bytes).unwrap();
    store
        .put_blob(&jpg_sha, &mime("image/jpeg"), &jpg_bytes)
        .unwrap();
    store
        .put_blob(&pdf_sha, &mime("application/pdf"), &pdf_bytes)
        .unwrap();
    store
        .put_blob(&odd_sha, &mime("image/svg+xml"), &odd_bytes)
        .unwrap();
    let mut want = vec![
        format!("{png_sha}.png"),
        format!("{jpg_sha}.jpeg"),
        format!("{pdf_sha}.pdf"),
        odd_sha.clone(),
    ];
    want.sort();
    assert_eq!(leftovers(store.attachments_dir()), want);
    assert!(!store.images_dir().exists(), "nothing new goes to images/");
    assert_eq!(
        store.get_blob(&png_sha, &png()).unwrap(),
        Blob {
            media_type: png(),
            bytes: png_bytes
        }
    );
    assert_eq!(
        store
            .get_blob(&pdf_sha, &mime("application/pdf"))
            .unwrap()
            .bytes,
        pdf_bytes
    );
    assert_eq!(
        store
            .get_blob(&odd_sha, &mime("image/svg+xml"))
            .unwrap()
            .bytes,
        odd_bytes
    );
    let (absent, _) = png_like("e");
    assert_eq!(
        store.get_blob(&absent, &png()).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(
        store.blob_path(&pdf_sha, &mime("application/pdf")),
        Some(store.attachments_dir().join(format!("{pdf_sha}.pdf")))
    );
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn an_extension_is_a_short_alphanumeric_subtype_or_nothing() {
    let name = |text: &str| blob_name("abc", &mime(text));
    assert_eq!(name("image/png"), "abc.png");
    assert_eq!(name("image/webp"), "abc.webp");
    assert_eq!(name("application/pdf"), "abc.pdf");
    assert_eq!(name("application/octet-stream"), "abc");
    assert_eq!(name("image/svg+xml"), "abc");
    assert_eq!(name("application/x-abcdefghi"), "abc");
    assert_eq!(name("audio/mpeg4abc"), "abc.mpeg4abc");
}

#[test]
fn old_pasted_images_are_still_read_from_the_images_folder() {
    let (root, store) = image_store("blob-legacy");
    let (png_sha, png_bytes) = png_like("a");
    let (jpg_sha, jpg_bytes) = png_like("b");
    fs::create_dir_all(store.images_dir()).unwrap();
    fs::write(
        store.images_dir().join(format!("{png_sha}.png")),
        &png_bytes,
    )
    .unwrap();
    fs::write(
        store.images_dir().join(format!("{jpg_sha}.jpg")),
        &jpg_bytes,
    )
    .unwrap();
    assert_eq!(store.get_blob(&png_sha, &png()).unwrap().bytes, png_bytes);
    assert_eq!(
        store.get_blob(&jpg_sha, &mime("image/jpeg")).unwrap().bytes,
        jpg_bytes
    );
    assert!(
        store.get_blob(&png_sha, &mime("image/webp")).is_err(),
        "only PNG and JPEG were ever saved there"
    );
    let missing = store
        .get_blob(&png_like("c").0, &png())
        .unwrap_err()
        .to_string();
    assert!(missing.contains("attachments"), "{missing}");
    assert!(missing.contains("images"), "{missing}");
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn a_blob_must_match_its_hash_and_the_hash_must_be_a_name() {
    let (root, mut store) = image_store("blob-hash");
    let (sha, bytes) = png_like("a");
    assert!(store.put_blob(&sha, &png(), b"other").is_err());
    for bad in [
        "",
        "../x",
        &sha.to_uppercase(),
        &sha[..63],
        &format!("{sha}0"),
    ] {
        assert!(store.put_blob(bad, &png(), &bytes).is_err(), "{bad}");
        assert!(store.get_blob(bad, &png()).is_err(), "{bad}");
    }
    assert!(!store.attachments_dir().exists());
    fs::remove_dir_all(&root).unwrap();
}

#[cfg(unix)]
#[test]
fn the_attachments_folder_is_700_and_each_file_600() {
    use std::os::unix::fs::PermissionsExt;
    let (root, mut store) = image_store("blob-modes");
    fs::create_dir_all(store.attachments_dir()).unwrap();
    fs::set_permissions(store.attachments_dir(), fs::Permissions::from_mode(0o755)).unwrap();
    let (sha, bytes) = png_like("a");
    store.put_blob(&sha, &png(), &bytes).unwrap();
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(store.attachments_dir()), 0o700);
    let path = store.attachment_path(&sha, &png());
    assert_eq!(mode(&path), 0o600);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    store.put_blob(&sha, &png(), &bytes).unwrap();
    assert_eq!(mode(&path), 0o600, "a reused file is tightened");
    fs::remove_dir_all(&root).unwrap();
}

#[cfg(unix)]
#[test]
fn a_verified_file_is_reused_and_a_bad_one_rewritten_atomically() {
    use std::os::unix::fs::MetadataExt;
    let (root, mut store) = image_store("blob-reuse");
    let (sha, bytes) = png_like("a");
    store.put_blob(&sha, &png(), &bytes).unwrap();
    let path = store.attachment_path(&sha, &png());
    let first = fs::metadata(&path).unwrap().ino();
    store.put_blob(&sha, &png(), &bytes).unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().ino(),
        first,
        "reused, not rewritten"
    );

    fs::write(&path, b"damaged").unwrap();
    assert!(store.get_blob(&sha, &png()).is_err());
    store.put_blob(&sha, &png(), &bytes).unwrap();
    assert_ne!(
        fs::metadata(&path).unwrap().ino(),
        first,
        "replaced by a rename"
    );
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(store.get_blob(&sha, &png()).unwrap().bytes, bytes);

    fs::write(
        &path,
        vec![0u8; usize::try_from(MAX_BLOB_BYTES).unwrap() + 1],
    )
    .unwrap();
    let too_big = store.get_blob(&sha, &png()).unwrap_err().to_string();
    assert_eq!(too_big, "the file is larger than any attachment Wisp sends");
    store.put_blob(&sha, &png(), &bytes).unwrap();
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(
        leftovers(store.attachments_dir()),
        [format!("{sha}.png")],
        "no temporary files are left behind"
    );
    fs::remove_dir_all(&root).unwrap();
}

#[cfg(unix)]
#[test]
fn attachment_files_and_the_folder_never_follow_symlinks() {
    let (root, mut store) = image_store("blob-symlink");
    let (sha, bytes) = png_like("a");
    fs::create_dir_all(store.attachments_dir()).unwrap();
    let target = root.join("elsewhere.png");
    fs::write(&target, b"keep").unwrap();
    let path = store.attachment_path(&sha, &png());
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert!(store.put_blob(&sha, &png(), &bytes).is_err());
    assert!(store.get_blob(&sha, &png()).is_err());
    assert_eq!(fs::read(&target).unwrap(), b"keep");
    assert!(
        fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    fs::remove_file(&path).unwrap();

    let elsewhere = root.join("other-folder");
    fs::create_dir_all(&elsewhere).unwrap();
    let linked = root.join("linked-attachments");
    std::os::unix::fs::symlink(&elsewhere, &linked).unwrap();
    let mut linked_store = JsonlStore::open(root.join("s2"))
        .unwrap()
        .with_attachments_dir(&linked);
    assert!(linked_store.put_blob(&sha, &png(), &bytes).is_err());
    assert!(leftovers(&elsewhere).is_empty());
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn a_session_log_stops_after_an_attachment_write_fails() {
    let mut log = SessionLog::new(Box::new(MemoryStore::failing()), "s1");
    let (sha, bytes) = png_like("a");
    let reason = log.put_blob(&sha, &png(), &bytes).unwrap();
    assert!(
        reason.starts_with("attachment could not be saved"),
        "{reason}"
    );
    assert!(!log.is_on());
    assert!(log.put_blob(&sha, &png(), &bytes).is_none());
    assert!(log.saved_path(&sha, &png()).is_none());
    assert!(log.record(started()).is_none());
}

#[test]
fn spend_writers_share_a_day_and_leave_earlier_days_unchanged() {
    let root = temp_dir("spend-writers");
    let sessions = root.join("sessions");
    let day: jiff::civil::Date = "2026-11-14".parse().unwrap();
    let mut first = JsonlStore::open(&sessions).unwrap();
    first.append_spend(day, "{\"seed\":true}\n").unwrap();
    let mut second = JsonlStore::open(&sessions).unwrap();
    std::thread::scope(|scope| {
        for (writer, store) in [(1, &mut first), (2, &mut second)] {
            scope.spawn(move || {
                for n in 0..100 {
                    store
                        .append_spend(day, &format!("{{\"writer\":{writer},\"n\":{n}}}\n"))
                        .unwrap();
                }
            });
        }
    });
    let path = crate::spend::file(&sessions, day);
    let old = fs::read(&path).unwrap();
    let lines: std::collections::BTreeSet<String> = std::str::from_utf8(&old)
        .unwrap()
        .lines()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap();
            line.to_owned()
        })
        .collect();
    assert_eq!(lines.len(), 201);
    first.append_spend(day.tomorrow().unwrap(), "{}\n").unwrap();
    assert_eq!(fs::read(&path).unwrap(), old);
    assert_eq!(
        fs::read_to_string(crate::spend::file(&sessions, day.tomorrow().unwrap())).unwrap(),
        "{}\n"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_spend_stays_on_its_day_without_disabling_the_log_and_a_deleted_folder_reopens() {
    let root = temp_dir("spend-failure");
    let sessions = root.join("sessions");
    let folder = crate::spend::folder(&sessions);
    let store = JsonlStore::open(&sessions).unwrap();
    fs::write(&folder, "blocked").unwrap();
    let id = new_session_id();
    let mut log = SessionLog::new(Box::new(store), &id);
    register(&Redactions::new(["hidden".to_owned()]));
    let sample = |cost| crate::spend::Record {
        at: "2026-11-14T00:00:00.000Z".into(),
        session: id.clone(),
        account: "hidden-account".into(),
        model: "hidden-model".into(),
        spend: crate::agent::Spend {
            usd: Some(cost),
            ..Default::default()
        },
    };
    let day: jiff::civil::Date = "2026-11-14".parse().unwrap();
    let next = day.tomorrow().unwrap();
    let memory = MemoryStore::failing();
    let mut memory_log = SessionLog::new(Box::new(memory.clone()), &id);
    assert!(!memory_log.spend(day, sample(0.0)));
    assert!(memory_log.is_on());
    assert!(memory.spend_lines().is_empty());
    assert!(!log.spend(day, sample(1.0)));
    assert!(log.is_on());
    assert!(log.record(started()).is_none());
    assert_eq!(log.next_seq(), 1);
    fs::remove_file(&folder).unwrap();
    assert!(log.spend(next, sample(2.0)));
    let read = |day| fs::read_to_string(crate::spend::file(&sessions, day)).unwrap();
    let old: crate::spend::Record = serde_json::from_str(&read(day)).unwrap();
    assert_eq!(old.spend.usd, Some(1.0));
    assert_eq!(old.account, "[redacted]-account");
    assert_eq!(old.model, "[redacted]-model");
    let value: serde_json::Value = serde_json::from_str(&read(day)).unwrap();
    assert_eq!(
        value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "account",
            "at",
            "cost_usd",
            "list_usd",
            "lower_bound",
            "model",
            "session",
            "usage"
        ]
    );
    let missing: crate::spend::Record = serde_json::from_value(json!({"at": old.at, "session": old.session, "account": old.account, "model": old.model, "future": true})).unwrap();
    assert!(missing.spend.usd.is_none());
    assert!(!missing.spend.lower_bound);
    let guard = open_private(&crate::spend::file(&sessions, next)).unwrap();
    guard.try_lock().unwrap();
    assert!(!log.spend(next, sample(3.0)));
    guard.unlock().unwrap();
    assert!(log.sync().is_none());
    assert_eq!(read(next).lines().count(), 2);
    assert!(log.spend(next, sample(4.0)));
    let costs: Vec<f64> = read(next)
        .lines()
        .map(|line| {
            serde_json::from_str::<crate::spend::Record>(line)
                .unwrap()
                .spend
                .usd
                .unwrap()
        })
        .collect();
    assert_eq!(costs, [2.0, 3.0, 4.0]);
    let past = day.yesterday().unwrap();
    let stuck = crate::spend::file(&sessions, past);
    fs::create_dir(&stuck).unwrap();
    assert!(!log.spend(past, sample(5.0)));
    assert!(!log.spend(next, sample(6.0)));
    assert_eq!(
        read(next).lines().count(),
        4,
        "a stuck past day does not block this one"
    );
    fs::remove_dir(stuck).unwrap();
    assert!(log.sync().is_none());
    assert_eq!(read(past).lines().count(), 1);
    fs::remove_dir_all(&folder).unwrap();
    assert!(log.spend(next, sample(7.0)));
    assert_eq!(read(next).lines().count(), 1);
    assert!(log.is_on());
    assert!(log.sync().is_none());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_spend_fragment_is_ended_and_short_writes_retain_only_unfinished_records() {
    const CHILD_PATH: &str = "WISP_TEST_SPEND_CRASH_PATH";
    if let Some(path) = std::env::var_os(CHILD_PATH) {
        let path = PathBuf::from(path);
        let mut file = open_private(&path).unwrap();
        crate::file_lock::try_lock(&file).unwrap();
        file.write_all(b"{\"fragment\":").unwrap();
        fs::write(path.with_extension("ready"), "").unwrap();
        io::stdin().read_exact(&mut [0]).unwrap();
        return;
    }
    let root = temp_dir("spend-fragment");
    let sessions = root.join("sessions");
    let mut store = JsonlStore::open(&sessions).unwrap();
    let day = "2026-11-14".parse().unwrap();
    create_private_dir(&crate::spend::folder(&sessions)).unwrap();
    let path = crate::spend::file(&sessions, day);
    let mut holder = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "session::tests::a_spend_fragment_is_ended_and_short_writes_retain_only_unfinished_records", "--nocapture"])
        .env(CHILD_PATH, &path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn().unwrap();
    let ready = path.with_extension("ready");
    let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !ready.exists() && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let refused = store.append_spend(day, "{\"kept\":true}\n");
    let killed = holder.kill();
    holder.wait().unwrap();
    assert!(ready.exists(), "holder never took its lock");
    killed.unwrap();
    assert_eq!(refused.unwrap_err().kind(), io::ErrorKind::WouldBlock);
    store.append_spend(day, "{\"good\":true}\n").unwrap();
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "{\"fragment\":\n{\"kept\":true}\n{\"good\":true}\n"
    );
    let batch = "{\"n\":1}\n{\"n\":2}\n";
    for cut in 0..batch.len() {
        let mut bytes = vec![0; cut];
        let mut written = 0;
        assert!(write_counted(&mut bytes.as_mut_slice(), batch.as_bytes(), &mut written).is_err());
        assert_eq!(written, cut);
        let mut kept = batch.to_owned();
        retain_unwritten(&mut kept, written);
        let mut recovered = String::from_utf8(bytes).unwrap();
        if !recovered.ends_with('\n') {
            recovered.push('\n');
        }
        recovered.push_str(&kept);
        let values: Vec<serde_json::Value> = recovered
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        assert_eq!(values, [json!({"n":1}), json!({"n":2})], "cut {cut}");
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn unwritten_spend_is_bounded_and_retried_before_new_lines() {
    let root = temp_dir("spend-bounded");
    let sessions = root.join("sessions");
    let mut store = JsonlStore::open(&sessions).unwrap();
    let folder = crate::spend::folder(&sessions);
    fs::write(&folder, "blocked").unwrap();
    let day = "2026-11-14".parse().unwrap();
    let line = format!("{{\"data\":\"{}\"}}\n", "x".repeat(1000));
    for _ in 0..100 {
        assert!(store.append_spend(day, &line).is_err());
    }
    assert_eq!(
        store.pending_spend[&day].len(),
        MAX_PENDING_SPEND / line.len() * line.len()
    );
    fs::remove_file(folder).unwrap();
    assert!(
        store
            .append_spend(day, &"x".repeat(MAX_PENDING_SPEND + 1))
            .is_err()
    );
    store.append_spend(day, "{}\n").unwrap();
    let text = fs::read_to_string(crate::spend::file(&sessions, day)).unwrap();
    assert_eq!(text.lines().count(), MAX_PENDING_SPEND / line.len() + 1);
    assert!(store.pending_spend.is_empty());
    fs::remove_dir_all(root).unwrap();
}

fn masked_rule(event: &LogEvent) -> &'static str {
    match event {
        LogEvent::SessionStarted { .. } => "dropped",
        LogEvent::TurnStarted { .. } => "settings",
        LogEvent::CompactionPrepared { .. } => "dropped",
        LogEvent::Retrying { .. } => "dropped",
        LogEvent::StepEnded { .. } => "baseline",
        LogEvent::RebuildMarked { .. } => "dropped",
        LogEvent::Fallback { .. } => "dropped",
        LogEvent::CacheMiss { .. } => "dropped",
        LogEvent::CacheMissRepeated { .. } => "dropped",
        LogEvent::CacheRefresh { .. } => "dropped",
        LogEvent::ModelAsked { .. } | LogEvent::SessionNamed { .. } => "dropped",
        LogEvent::VoiceCall { .. } => "dropped",
        LogEvent::UnknownProviderEvent { .. } => "dropped",
        LogEvent::Rejected { .. } => "dropped",
        LogEvent::WakeHeld { .. } => "dropped",
        LogEvent::AgentRefused { .. } => "dropped",
        LogEvent::AgentSpawned { .. } => "kept",
        LogEvent::AgentEnded { .. } => "kept",
        LogEvent::AgentRenamed { .. } => "kept",
        LogEvent::AgentReported { .. } => "kept",
        LogEvent::Rewound { .. } => "dropped",
        LogEvent::WindowLowered { .. } => "kept",
        LogEvent::TodosUpdated { .. } => "dropped",
        LogEvent::HookRan { .. } => "dropped",
        LogEvent::HookApplied { .. } => "dropped",
        LogEvent::ResumeFellBack { .. } => "kept",
        LogEvent::ImageLost { .. } => "kept",
        LogEvent::MessageCommitted { .. } => "dropped",
        LogEvent::TurnEnded { .. } => "kept",
        LogEvent::ToolCallRequested { .. } => "kept",
        LogEvent::ToolCallStarted { .. } => "kept",
        LogEvent::ToolCallFinished { .. } => "dropped",
        LogEvent::SessionEnded { .. } => "dropped",
        LogEvent::SessionResumed { .. } => "kept",
        LogEvent::SettingsChanged { .. } => "kept",
        LogEvent::PluginsChanged { .. } => "kept",
        LogEvent::PerMessageEffort { .. } => "kept",
        LogEvent::BindingControls { .. } => "kept",
        LogEvent::InputAccepted { .. } => "kept",
        LogEvent::InputHeld { .. } => "dropped",
        LogEvent::InputUnheld { .. } => "kept",
        LogEvent::ReturnedTaken { .. } => "kept",
        LogEvent::InputCommitted { .. } => "kept",
        LogEvent::InputRevised { .. } => "kept",
        LogEvent::InputWithdrawn { .. } => "kept",
        LogEvent::InputDelivered { .. } => "kept",
        LogEvent::QuestionAsked { .. } => "dropped",
        LogEvent::QuestionClosed { .. } => "dropped",
        LogEvent::TailClosed { .. } => "dropped",
        LogEvent::RenderingForm { .. } => "kept",
        LogEvent::ThinkingCut { .. } => "tools_cut",
        LogEvent::SearchHistoryRendered { .. } => "kept",
        LogEvent::AttachmentsAsNotes { .. } => "kept",
        LogEvent::SearchHistoryUnreadable { .. } => "kept",
        LogEvent::SystemMarkerDropped { .. } => "kept",
        LogEvent::NativeWebSearch { .. } => "search_refusal",
        LogEvent::SearchStarted { .. } => "dropped",
        LogEvent::SearchEnded { .. } => "dropped",
        LogEvent::Citations { .. } => "dropped",
        LogEvent::InstructionsLoaded { .. } => "kept",
        LogEvent::InstructionsChanged { .. } => "dropped",
        LogEvent::AttachmentsWithdrawn { .. } => "kept_items",
        LogEvent::ProcessStarted { .. } => "numbering",
        LogEvent::ProcessEnded { .. } => "dropped",
        LogEvent::ProcessResized { .. } => "dropped",
        LogEvent::ProcessAttached { .. } => "dropped",
        LogEvent::ProcessDetached { .. } => "dropped",
        LogEvent::ProcessHeard { .. } => "dropped",
        LogEvent::ProcessNeedsInput { .. } => "dropped",
        LogEvent::ProcessNeedsInputCleared { .. } => "dropped",
        LogEvent::EndpointSaved { .. } => "dropped",
        LogEvent::EndpointNotSaved { .. } => "dropped",
        LogEvent::EndpointEdited { .. } => "dropped",
        LogEvent::EndpointRemoved { .. } => "dropped",
        LogEvent::EndpointsChanged { .. } => "dropped",
        LogEvent::InputTransformations { .. } => "dropped",
        LogEvent::WebSearchNotRun { .. } => "dropped",
        LogEvent::NativeCompaction { .. } => "kept",
        LogEvent::InlineTools { .. } => "kept",
        LogEvent::DeferredTools { .. } => "kept",
        LogEvent::ToolsChanged { .. } => "kept",
        LogEvent::ContextCompacted { .. } => "epoch",
        LogEvent::CompactionFailed { .. } => "dropped",
        LogEvent::Unknown => "dropped",
    }
}

#[test]
fn every_record_has_a_rule_for_when_a_rewind_masks_it() {
    let read: Vec<(String, &str)> = one_of_every_event()
        .iter()
        .map(|e| (kind_of(e), masked_rule(e)))
        .filter(|(_, rule)| *rule != "dropped" && *rule != "kept")
        .collect();
    assert_eq!(
        read,
        [
            ("turn_started", "settings"),
            ("step_ended", "baseline"),
            ("attachments_withdrawn", "kept_items"),
            ("context_compacted", "epoch"),
            ("thinking_cut", "tools_cut"),
            ("native_web_search", "search_refusal"),
            ("process_started", "numbering"),
        ]
        .map(|(name, rule)| (name.to_owned(), rule))
    );
}

fn masked_fold(extra: Option<&LogEvent>) -> String {
    let started = one_of_every_event()
        .into_iter()
        .find(|e| matches!(e, LogEvent::SessionStarted { .. }))
        .unwrap();
    let turn = |turn: u64, text: &str| -> LogEvent {
        serde_json::from_value(json!({"type": "turn_started", "turn": turn, "text": text})).unwrap()
    };
    let reply = |turn: u64, text: &str| -> LogEvent {
        serde_json::from_value(json!({"type": "message_committed", "turn": turn, "message": {"role": "assistant", "content": [{"type": "text", "text": text}]}}))
            .unwrap()
    };
    let mut events = vec![started, turn(1, "one"), reply(1, "one"), turn(2, "two")];
    events.extend(extra.cloned());
    events.push(reply(2, "two"));
    events.push(LogEvent::Rewound {
        to: 2,
        point: wisp_protocol::RewindPoint::Turn { turn: 2 },
        reason: wisp_protocol::RewindReason::User,
        messages: 2,
        blocks: 1,
        dropped: wisp_protocol::Dropped {
            turns: vec![2],
            messages: 2,
        },
        cache: wisp_protocol::CacheEffect::Kept,
    });
    let records: Vec<LogRecord> = events
        .into_iter()
        .zip(0..)
        .map(|(event, seq)| LogRecord {
            schema_version: LOG_SCHEMA_VERSION,
            seq,
            at: "2026-10-25T12:00:00Z".into(),
            event,
        })
        .collect();
    let rebuilt = rebuild_checked(&MemoryStore::default(), &records, None)
        .map(Option::unwrap)
        .map_err(|e| e.error);
    let mut logged = crate::resume::LoggedSession::read(&records);
    if let Ok(logged) = &mut logged {
        logged.session_usd = 0.0;
        logged.plan_usd = 0.0;
        logged.lower_bound = false;
        logged.compaction_usd = None;
        logged.accounts.clear();
    }
    format!("{rebuilt:?}\n{logged:?}")
}

fn nested(event: LogEvent) -> LogEvent {
    match event {
        LogEvent::Rewound {
            point,
            reason,
            dropped,
            cache,
            ..
        } => LogEvent::Rewound {
            to: 3,
            point,
            reason,
            messages: 3,
            blocks: 1,
            dropped,
            cache,
        },
        other => other,
    }
}

#[test]
fn a_masked_record_changes_the_fold_only_where_its_rule_says() {
    let plain = masked_fold(None);
    assert!(!plain.contains("Err("), "{plain}");
    let mut changed = Vec::new();
    for event in one_of_every_event().into_iter().map(nested) {
        if matches!(event, LogEvent::SessionStarted { .. }) {
            continue;
        }
        let folded = masked_fold(Some(&event));
        assert!(!folded.contains("Err("), "{}: {folded}", kind_of(&event));
        let rule = masked_rule(&event);
        if rule == "dropped" {
            assert_eq!(folded, plain, "{}", kind_of(&event));
        } else if folded != plain {
            changed.push((kind_of(&event), rule));
        }
    }
    assert_eq!(
        changed,
        [
            ("turn_started", "settings"),
            ("settings_changed", "kept"),
            ("per_message_effort", "kept"),
            ("binding_controls", "kept"),
            ("native_compaction", "kept"),
            ("inline_tools", "kept"),
            ("deferred_tools", "kept"),
            ("tools_changed", "kept"),
            ("input_accepted", "kept"),
            ("rendering_form", "kept"),
            ("thinking_cut", "tools_cut"),
            ("search_history_rendered", "kept"),
            ("search_history_unreadable", "kept"),
            ("system_marker_dropped", "kept"),
            ("attachments_as_notes", "kept"),
            ("native_web_search", "search_refusal"),
            ("instructions_loaded", "kept"),
            ("process_started", "numbering"),
            ("session_resumed", "kept"),
            ("window_lowered", "kept"),
            ("resume_fell_back", "kept"),
            ("image_lost", "kept"),
        ]
        .map(|(name, rule)| (name.to_owned(), rule))
    );
}

fn one_of_every_event() -> Vec<LogEvent> {
    use wisp_protocol::log::{FeatureState, LoggedInput};
    use wisp_protocol::{
        CacheEffect, DeliveryMode, InputId, OutputKind, ToolAction, ToolDisplay, ToolOutcome,
        ToolOutputView, ToolPurpose, TurnOutcome, TurnStats, WithdrawnBy,
    };
    let display = ToolDisplay {
        action: ToolAction::Run,
        verbs: None,
        target: "ls".into(),
        output: OutputKind::Text,
        view: None,
        title: None,
        purpose: ToolPurpose::Run,
    };
    vec![
        started(),
        LogEvent::TurnStarted {
            ends: Vec::new(),
            turn: 1,
            origin: None,
            lead: TurnLead::default(),
            text: "look".into(),
            model: None,
            effort: None,
            effort_marker: None,
            images: vec![ImageRef {
                sha256: "ab".repeat(32),
                media_type: ImageType::Jpeg,
                bytes: 3,
                width: 3,
                height: 1,
            }],
            endpoint_name: Some("direct".into()),
            input_id: None,
            inputs: Vec::new(),
            cache_ttl: None,
            debug_cache: None,
            refusal_fallback: None,
            binding_controls: None,
            retry_of: None,
            kept_tools_sha256: None,
            tool_message: Vec::new(),
        },
        LogEvent::Retrying {
            list_usd: None,
            turn: Some(1),
            attempt: 1,
            delay_ms: 10,
            reason: "overloaded".into(),
            step: Some(1),
            kind: Some(wisp_protocol::ErrorKind::Overloaded),
            detail: None,
            usage: None,
            partial_output: Some("half an answer".into()),
            model: None,
            cost_usd: None,
            lower_bound: false,
            resend: None,
            busy: None,
        },
        LogEvent::StepEnded {
            list_usd: None,
            model: None,
            turn: 1,
            step: 1,
            usage: None,
            cost_usd: Some(0.01),
            lower_bound: false,
            prompt_tokens: Some(9000),
            response_id: Some("msg_1".into()),
            window: None,
            compact_at: None,
            used: None,
            first_pass: None,
            accepted: false,
            view_len: None,
            started_at: None,
            ttl: None,
            prefix: None,
            queued_ms: None,
            thread: None,
            thread_error: None,
            betas: Vec::new(),
        },
        LogEvent::AttachmentsWithdrawn {
            turn: 1,
            step: 1,
            items: vec![wisp_protocol::log::WithdrawnBlock {
                ordinal: 0,
                sha256: "ab".repeat(32),
                note: "[image #1 couldn't be processed: Could not process image]".into(),
            }],
        },
        LogEvent::RebuildMarked {
            turn: 1,
            reason: "model_switch".into(),
        },
        LogEvent::Fallback {
            turn: 1,
            from: "a".into(),
            to: "b".into(),
        },
        LogEvent::CacheMiss {
            turn: Some(1),
            previous_prompt_tokens: 9000,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 9000,
            diagnostics: None,
            cause: Some(wisp_protocol::MissCause::ToolsChanged),
        },
        LogEvent::CacheMissRepeated {
            billing: Default::default(),
            turn: Some(1),
            cause: wisp_protocol::MissCause::ToolsChanged,
            count: 2,
            usd: Some(0.5),
        },
        LogEvent::CacheRefresh {
            list_usd: None,
            model: None,
            turn: 1,
            outcome: wisp_protocol::RefreshOutcome::Read,
            detail: None,
            started_at: Some("2026-10-25T12:55:00Z".into()),
            count: 1,
            usage: None,
            cost_usd: Some(0.04),
            lower_bound: false,
        },
        LogEvent::ModelAsked {
            call_id: "q".into(),
            plugin: "web-search".into(),
            turn: Some(1),
            detail: None,
            usage: None,
            cost_usd: Some(0.002),
            list_usd: None,
            plan_usd: Some(0.0),
            model: None,
            lower_bound: false,
        },
        LogEvent::SessionNamed {
            name: Some("Cache fix".into()),
            detail: None,
            usage: None,
            cost_usd: Some(0.001),
            list_usd: None,
            model: None,
            lower_bound: false,
        },
        LogEvent::VoiceCall {
            call: wisp_protocol::voice::CallId("call-1".into()),
            outcome: wisp_protocol::voice::CallOutcome::Completed {
                end: wisp_protocol::voice::CallEnd::Hangup,
            },
            seconds: 30,
            lines: vec!["Heard: hello".into()],
            unknown: BTreeMap::from([("mystery".into(), 2)]),
            endpoint_name: Some("talk".into()),
            usage: Some(Usage {
                billing: wisp_protocol::Billing::ChatGptPlan,
                ..Usage::default()
            }),
            cost_usd: Some(0.025),
            list_usd: None,
            model: Some("gpt-live-1-codex".into()),
            lower_bound: false,
        },
        LogEvent::UnknownProviderEvent {
            turn: Some(1),
            provider: "anthropic".into(),
            event: json!({"type": "new"}),
        },
        LogEvent::InputTransformations {
            turn: Some(1),
            provider: "anthropic".into(),
            transformations: serde_json::from_value(json!([{
                "type": "thinking_dropped",
                "path": "messages.1.content.0",
                "reason": "model_binding_mismatch"
            }]))
            .unwrap(),
        },
        LogEvent::Rejected {
            turn: None,
            reason: "empty".into(),
            id: None,
            kind: None,
        },
        LogEvent::WakeHeld {
            turn: 1,
            sources: vec![wisp_protocol::WakeSource::Process { id: 1 }],
            reason: wisp_protocol::HoldReason::Stopped,
        },
        LogEvent::AgentRefused {
            turn: None,
            by: wisp_protocol::AgentPath::root(),
            action: wisp_protocol::AgentAction::Send,
            reason: "no such agent".into(),
        },
        LogEvent::AgentSpawned {
            turn: Some(1),
            agent: wisp_protocol::AgentPath::new("/review"),
            session_id: "s-child".into(),
            depth: 1,
            model: "claude-opus-5-5".into(),
            effort: wisp_protocol::Effort::High,
            cwd: Some("~/p".into()),
            config: wisp_protocol::SpawnChoices::default(),
            endpoint: None,
            provider: None,
        },
        LogEvent::AgentEnded {
            billing: wisp_protocol::Billing::Metered,
            plan_usd: None,
            turn: Some(2),
            agent: wisp_protocol::AgentPath::new("/review"),
            spent_usd: 0.5,
        },
        LogEvent::AgentRenamed {
            turn: Some(2),
            agent: wisp_protocol::AgentPath::new("/review"),
            to: wisp_protocol::AgentPath::new("/fixer"),
        },
        LogEvent::AgentReported {
            turn: 1,
            agent: wisp_protocol::AgentPath::new("/review"),
            outcome: wisp_protocol::ReportOutcome::Reported,
            report: Some(json!("Done.")),
            duration_ms: 10,
        },
        LogEvent::MessageCommitted {
            turn: 1,
            message: tricky_message().into(),
            model: Some("claude-opus-5-5".into()),
            stop_reason: None,
        },
        LogEvent::ToolCallRequested {
            turn: 1,
            step: 1,
            block: 0,
            call_id: "t".into(),
            tool: "bash".into(),
            input: json!({"command": "ls"}),
            display,
            refused: false,
        },
        LogEvent::ToolCallStarted {
            turn: 1,
            call_id: "t".into(),
            cwd: "~".into(),
            movable: false,
            file: None,
        },
        LogEvent::ToolCallFinished {
            turn: 1,
            call_id: "t".into(),
            outcome: ToolOutcome::Cancelled,
            output: ToolOutputView::default(),
            facts: Vec::new(),
            state: Some(wisp_protocol::WorkspaceChange::Cwd {
                path: "/work/sub".into(),
            }),
            duration_ms: 5,
            result: Some("The user answered:\nKeep it".into()),
            process: None,
            attachments: Vec::new(),
            notes: Some(Vec::new()),
            typed: None,
        },
        LogEvent::QuestionAsked {
            turn: 1,
            call_id: "t".into(),
            questions: vec![wisp_protocol::Question {
                header: "Plan".into(),
                question: "Keep it?".into(),
                options: vec![
                    wisp_protocol::QuestionOption {
                        label: "Keep it".into(),
                        description: String::new(),
                    },
                    wisp_protocol::QuestionOption {
                        label: "Drop it".into(),
                        description: "Start over.".into(),
                    },
                ],
                multi_select: false,
                source: None,
            }],
        },
        LogEvent::QuestionClosed {
            turn: 1,
            call_id: "t".into(),
            how: wisp_protocol::QuestionEnd::Answered {
                answers: vec![wisp_protocol::Answer::choose(0)],
            },
        },
        LogEvent::TurnEnded {
            turn: 1,
            outcome: TurnOutcome::Cancelled {
                usage: None,
                cost: None,
                committed_steps: 0,
            },
            stats: TurnStats::default(),
            partial_output: None,
        },
        LogEvent::SettingsChanged {
            model: "claude-opus-5-5".into(),
            effort: Effort::Low,
            cache: CacheEffect::KeptIfPassedOn,
            endpoint_name: Some("direct".into()),
            provider: None,
            endpoint: Some("https://api.anthropic.com".into()),
            credential_source: Some(CredentialSource::Environment),
            account_bound: vec!["claude-sonnet-5-5".into()],
            tools: Vec::new(),
            tools_sha256: None,
            changes: Vec::new(),
            withdrawn: None,
        },
        LogEvent::PluginsChanged {
            turn: 2,
            added: vec![wisp_protocol::PluginRecord {
                id: "todos".into(),
                version: "0.1.0".into(),
                source: wisp_protocol::PluginSource::Bundled,
            }],
            updated: Vec::new(),
            removed: Vec::new(),
            kept: Vec::new(),
            notes: Vec::new(),
        },
        LogEvent::PerMessageEffort {
            model: "claude-opus-5-5".into(),
            state: FeatureState::FallenBack,
            endpoint_name: Some("direct".into()),
        },
        LogEvent::BindingControls {
            model: "claude-opus-5-5".into(),
            state: FeatureState::Proven,
            endpoint_name: Some("direct".into()),
        },
        LogEvent::NativeCompaction {
            model: "claude-opus-5-5".into(),
            state: FeatureState::Proven,
            endpoint_name: Some("direct".into()),
        },
        LogEvent::InlineTools {
            model: "claude-opus-5-5".into(),
            state: FeatureState::Proven,
            endpoint_name: Some("direct".into()),
        },
        LogEvent::DeferredTools {
            model: "claude-opus-5-5".into(),
            state: FeatureState::FallenBack,
            endpoint_name: Some("direct".into()),
        },
        LogEvent::ToolsChanged {
            turn: 2,
            via: wisp_protocol::ToolChangeVia::Inline,
            added: vec!["bash".into()],
            removed: vec!["view_file".into()],
            copy: Default::default(),
        },
        LogEvent::ContextCompacted {
            list_usd: None,
            model: None,
            lower_bound: false,
            plan_usd: None,
            sections: Vec::new(),
            turn: None,
            epoch: 1,
            trigger: wisp_protocol::CompactionTrigger::Manual,
            engine: wisp_protocol::CompactionEngine::Native,
            focus: Some("the parser".into()),
            before: 90_000,
            after: 7_000,
            partial: false,
            covers: Some(wisp_protocol::Covers {
                first_seq: 0,
                last_seq: 12,
                first_turn: 1,
                last_turn: 1,
            }),
            from: 4,
            head: wisp_protocol::Message {
                role: wisp_protocol::Role::User,
                content: vec![wisp_protocol::ContentBlock::Text {
                    text: "<compacted_context epoch=\"1\" turns=\"1–1\">\n</compacted_context>\n"
                        .into(),
                }],
            },
            summary: None,
            block: Some(json!({"type": "compaction", "content": "So far: a parser."})),
            usage: None,
            cost_usd: Some(0.02),
            duration_ms: 900,
            switch: None,
            placement: wisp_protocol::Placement::Head,
            cut: None,
            additions: Vec::new(),
            instructions_by: Vec::new(),
            strategy: None,
            instruction: None,
            requested_by: None,
        },
        LogEvent::CompactionFailed {
            list_usd: None,
            model: None,
            plan_usd: None,
            turn: None,
            trigger: wisp_protocol::CompactionTrigger::Manual,
            engine: Some(wisp_protocol::CompactionEngine::Native),
            kind: wisp_protocol::CompactionFailure::Cancelled,
            message: "The compaction was cancelled.".into(),
            usage: None,
            cost_usd: None,
            lower_bound: false,
            switch: None,
            strategy: None,
            instruction: None,
            requested_by: None,
        },
        LogEvent::InputAccepted {
            turn: Some(1),
            id: InputId("tui-2".into()),
            mode: DeliveryMode::Queue,
            text: "and then this".into(),
            images: Vec::new(),
            replaces: Some(InputId("tui-1".into())),
            held: true,
            from: None,
            checklist_spans: Vec::new(),
        },
        LogEvent::InputHeld {
            turn: Some(1),
            ids: vec![InputId("tui-2".into())],
            by: WithdrawnBy::TurnStopped,
        },
        LogEvent::InputUnheld {
            turn: None,
            ids: vec![InputId("tui-2".into())],
        },
        LogEvent::ReturnedTaken {
            ids: vec![InputId("tui-2".into())],
        },
        LogEvent::InputCommitted {
            turn: 1,
            step: 2,
            inputs: vec![LoggedInput {
                id: InputId("tui-3".into()),
                text: "also this".into(),
                images: Vec::new(),
                note: None,
                checklist_spans: Vec::new(),
            }],
        },
        LogEvent::InputRevised {
            turn: Some(1),
            inputs: vec![LoggedInput {
                id: InputId("mail-CANARY".into()),
                text: "[redacted]".into(),
                images: Vec::new(),
                note: None,
                checklist_spans: vec![wisp_protocol::TextSpan { start: 0, end: 10 }],
            }],
        },
        LogEvent::InputWithdrawn {
            turn: None,
            ids: vec![InputId("tui-2".into())],
            by: WithdrawnBy::SessionEnded,
        },
        LogEvent::InputDelivered {
            turn: 1,
            step: 2,
            ids: vec![InputId("tui-3".into())],
        },
        LogEvent::TailClosed {
            through: 3,
            calls: vec!["srvtoolu_1".into()],
            reason: wisp_protocol::UnfinishedReason::Stopped,
        },
        LogEvent::RenderingForm {
            form: wisp_protocol::RenderingForm::WithoutThinking,
        },
        LogEvent::ThinkingCut {
            from: wisp_protocol::log::BlockPosition {
                message: 1,
                block: 2,
            },
            through: 3,
            tools_sha256: Some("cd".repeat(32)),
        },
        LogEvent::SearchHistoryRendered {
            endpoint_name: "direct".into(),
        },
        LogEvent::SearchHistoryUnreadable {
            endpoint_name: "direct".into(),
        },
        LogEvent::SystemMarkerDropped {
            endpoint_name: "direct".into(),
        },
        LogEvent::AttachmentsAsNotes {
            endpoint_name: "direct".into(),
            model: "text-model".into(),
        },
        LogEvent::NativeWebSearch {
            endpoint_name: "direct".into(),
            state: FeatureState::FallenBack,
        },
        LogEvent::WebSearchNotRun {
            turn: 1,
            endpoint_name: "direct".into(),
            model: "claude-haiku-5-5".into(),
        },
        LogEvent::SearchStarted {
            turn: 1,
            step: 1,
            attempt: 0,
            call_id: "srvtoolu_1".into(),
            query: "rust ripgrep".into(),
            action: SearchAction::Search,
        },
        LogEvent::SearchEnded {
            turn: None,
            step: 0,
            attempt: 0,
            call_id: "srvtoolu_1".into(),
            outcome: wisp_protocol::SearchEnd::Unfinished {
                reason: wisp_protocol::UnfinishedReason::Failed,
                possibly_billed: true,
            },
        },
        LogEvent::Citations {
            turn: 1,
            step: 2,
            attempt: 0,
            index: 1,
            sources: vec![wisp_protocol::CitedSource {
                url: "https://example.com/rg".into(),
                title: Some("ripgrep".into()),
            }],
        },
        LogEvent::InstructionsLoaded {
            turn: None,
            chosen: Some(wisp_protocol::SourceInfo {
                scope: wisp_protocol::Scope::Project,
                name: "AGENTS.md".into(),
                display_path: "~/p/AGENTS.md".into(),
                bytes: 6,
                sha256: "ab".repeat(32),
                imported_by: None,
                external: false,
            }),
            imports: Vec::new(),
            candidates: Vec::new(),
            diagnostics: Vec::new(),
        },
        LogEvent::InstructionsChanged {
            turn: Some(2),
            epoch: None,
            retry_of: None,
            ahead: false,
            from: None,
            to: None,
            changed: vec![wisp_protocol::SourceChange {
                display_path: "~/p/AGENTS.md".into(),
                old_sha256: Some("ab".repeat(32)),
                new_sha256: Some("cd".repeat(32)),
            }],
            chosen: None,
            imports: Vec::new(),
            candidates: Vec::new(),
            sent: true,
            diagnostics: Vec::new(),
            text: "<instructions-update>\n</instructions-update>".into(),
        },
        LogEvent::ProcessStarted {
            turn: 1,
            id: 3,
            call_id: "t".into(),
            reason: wisp_protocol::BackgroundReason::Started,
            tty: true,
            stdin: true,
            size: Some(wisp_protocol::TermSize {
                cols: 100,
                rows: 30,
            }),
            file: Some("~/out/0001-bash.out".into()),
            pid: Some(4242),
            sid: Some(4242),
            start_ticks: Some(9001),
            boot_id: Some("8f1e5c7a-2d4b-4e1f-9a3c-6b7d8e9f0a1b".into()),
            keeps_running: false,
            log: None,
        },
        LogEvent::ProcessResized {
            id: 3,
            size: wisp_protocol::TermSize { cols: 80, rows: 24 },
        },
        LogEvent::ProcessNeedsInput {
            id: 3,
            kind: wisp_protocol::NeedsInputKind::HiddenInput,
        },
        LogEvent::ProcessAttached {
            id: 3,
            size: wisp_protocol::TermSize { cols: 80, rows: 24 },
        },
        LogEvent::ProcessNeedsInputCleared { id: 3 },
        LogEvent::ProcessDetached {
            id: 3,
            size: wisp_protocol::TermSize {
                cols: 100,
                rows: 30,
            },
            reason: wisp_protocol::DetachReason::User,
            typed: true,
            answered: false,
        },
        LogEvent::ProcessEnded {
            id: 3,
            end: wisp_protocol::ProcessEnd::Stopped {
                by: wisp_protocol::StoppedBy::TimeLimit { after_ms: 600_000 },
            },
            bytes: 12,
            lines: 1,
            duration_ms: 600_004,
        },
        LogEvent::ProcessHeard {
            turn: 1,
            ids: vec![3],
            typed: vec![3],
        },
        LogEvent::SessionEnded {
            turns: 1,
            interrupted: None,
        },
        LogEvent::SessionResumed {
            credential_source: None,
            system_base_sha256: None,
            sections: None,
            wisp_version: "0.1.0".into(),
            endpoint: "https://api.anthropic.com".into(),
            endpoint_name: "env".into(),
            system_prompt_sha256: Some(sha256_hex("sys")),
            tools: vec!["bash".into()],
            tools_sha256: Some("abc".into()),
            system_prompt: None,
            whole: Default::default(),
            tools_on: None,
            instructions_sha256: None,
            instructions_source: None,
            instructions: None,
            instructions_extra: None,
            cwd: Some("~/wisp".into()),
            gap_ms: Some(3_600_000),
            truncated_bytes: 17,
            returned: Vec::new(),
        },
        LogEvent::Rewound {
            to: 1,
            point: wisp_protocol::RewindPoint::Turn { turn: 2 },
            reason: wisp_protocol::RewindReason::User,
            messages: 2,
            blocks: 3,
            dropped: wisp_protocol::Dropped {
                turns: vec![2],
                messages: 2,
            },
            cache: wisp_protocol::CacheEffect::Kept,
        },
        LogEvent::WindowLowered {
            endpoint: "manifest".into(),
            model: "claude-opus-5-5".into(),
            tokens: 200_000,
        },
        LogEvent::TodosUpdated {
            turn: 1,
            call_id: "toolu_1".into(),
            items: vec![wisp_protocol::TodoItem {
                text: "Write the plan".into(),
                status: wisp_protocol::TodoStatus::InProgress,
                section: None,
            }],
        },
        LogEvent::HookRan {
            hook: "acme/check".into(),
            point: "before-tool".into(),
            turn: Some(1),
            step: Some(1),
            call_id: Some("toolu_1".into()),
            outcome: wisp_protocol::HookOutcome::Applied,
            ms: 4,
        },
        LogEvent::HookApplied {
            hook: "acme/check".into(),
            point: "before-tool".into(),
            turn: Some(1),
            step: Some(1),
            call_id: Some("toolu_1".into()),
            input_id: None,
            change: wisp_protocol::HookChange::Input {
                before: json!({"command": "ls"}),
                after: json!({"command": "ls -a"}),
            },
        },
        LogEvent::ResumeFellBack {
            billing: wisp_protocol::Billing::Metered,
            plan_usd: None,
            kept_through: Some(12),
            dropped_through: Some(20),
            covered_through: None,
            last_turn: Some(2),
            turns_seen: 4,
            processes_seen: 1,
            spent_usd: 0.25,
            damage: vec![wisp_protocol::log::Damage {
                kind: wisp_protocol::log::DamageKind::Image,
                seq: 14,
                turn: Some(3),
                sha256: Some(sha256_hex("img")),
            }],
        },
        LogEvent::ImageLost {
            turn: 1,
            sha256: sha256_hex("img"),
            note: "[image no longer available: image/png, 1.2 MB]".into(),
        },
        LogEvent::CompactionPrepared {
            list_usd: None,
            model: None,
            lower_bound: false,
            plan_usd: None,
            turn: None,
            epoch: 1,
            covers: None,
            block: json!({"type": "compaction", "content": "So far."}),
            usage: Some(wisp_protocol::Usage::default()),
            cost_usd: Some(0.25),
            duration_ms: 900,
        },
        LogEvent::EndpointSaved {
            turn: Some(1),
            name: "manifest".into(),
            store: StoreKind::Keyring,
            check: CheckResult::skipped(),
        },
        LogEvent::EndpointNotSaved {
            turn: None,
            name: "manifest".into(),
            why: NotSavedReason::KeychainFailed("didn't answer".into()),
        },
        LogEvent::EndpointEdited {
            turn: Some(1),
            name: "manifest".into(),
            changed: vec![ConnectionField::Key],
            store: None,
            check: CheckResult::skipped(),
        },
        LogEvent::EndpointRemoved {
            turn: None,
            name: "direct".into(),
            kept_item: false,
            default_cleared: true,
        },
        LogEvent::EndpointsChanged {
            turn: None,
            endpoints: Vec::new(),
        },
    ]
}

#[test]
fn each_mirrored_record_is_what_its_event_logs() {
    let mut mirrored = 0;
    for record in one_of_every_event() {
        let Some(event) = mirror::live_event(&record) else {
            continue;
        };
        let turn = serde_json::to_value(&record).unwrap()["turn"].as_u64();
        assert_eq!(mirror::log_record(&event, turn), Some(record));
        mirrored += 1;
    }
    assert_eq!(mirrored, mirror::MIRRORED);
}

#[test]
fn every_record_type_written_is_read_back() {
    let events = one_of_every_event();
    let mut kinds: Vec<String> = events.iter().map(kind_of).collect();
    kinds.sort_unstable();
    kinds.dedup();
    assert_eq!(kinds.len(), events.len(), "one record of each type");
    assert!(kinds.iter().all(|k| known_type(k)), "{kinds:?}");

    let text: String = events
        .iter()
        .enumerate()
        .map(|(seq, event)| jsonl_line(&record(seq as u64, event.clone())).unwrap())
        .collect();
    let back: Vec<LogEvent> = parse_jsonl(text)
        .unwrap()
        .into_iter()
        .map(|r| r.event)
        .collect();
    assert_eq!(back, events);
}

#[test]
fn the_record_types_table_has_one_row_for_each_record_type_written() {
    let docs = include_str!("../../../../docs/session-log.md");
    let mut rows: Vec<&str> = crate::test_guard::table_rows(docs, "| `type` | When | Fields |")
        .filter_map(|line| line.strip_prefix("| `")?.split_once('`'))
        .map(|(kind, _)| kind)
        .collect();
    let mut written: Vec<String> = one_of_every_event().iter().map(kind_of).collect();
    rows.sort_unstable();
    written.sort_unstable();
    assert_eq!(rows, written, "docs/session-log.md's record types");
}

#[test]
fn the_images_usage_is_read_only_and_counts_files() {
    let root = temp_dir("usage");
    let images = root.join("images");
    assert_eq!(images_usage(&images).unwrap(), None);
    assert!(!images.exists());
    fs::create_dir_all(images.join("sub")).unwrap();
    fs::write(images.join("a.png"), [0u8; 10]).unwrap();
    fs::write(images.join("b.jpg"), [0u8; 5]).unwrap();
    assert_eq!(
        images_usage(&images).unwrap(),
        Some(FolderUsage {
            files: 2,
            bytes: 15
        })
    );
    fs::write(root.join("file"), "").unwrap();
    assert_eq!(
        images_usage(&root.join("file")).unwrap_err().to_string(),
        "not a folder"
    );
    std::os::unix::fs::symlink(&images, root.join("link")).unwrap();
    assert_eq!(
        images_usage(&root.join("link")).unwrap_err().to_string(),
        "is a symlink"
    );
    assert_eq!(
        JsonlStore::default_images_dir(None, Some("/h".into())),
        Some(PathBuf::from("/h/.local/share/wisp/images"))
    );
    fs::remove_dir_all(&root).unwrap();
}

fn after_question_line(version: u16, reason: &str) -> String {
    format!(
        r#"{{"schema_version":{version},"seq":0,"at":"x","type":"tool_call_finished","turn":1,"call_id":"t","outcome":{{"status":"not_run","reason":{{"type":"{reason}"}}}},"output":{{"tail":"","bytes":0,"lines":0,"file":null,"file_problem":null,"inlined":true}},"facts":[],"state":null,"duration_ms":0}}"#
    )
}

#[test]
fn a_reason_from_a_newer_version_is_skipped_but_one_claimed_by_this_version_must_parse() {
    let newer = after_question_line(LOG_SCHEMA_VERSION + 1, "something_newer");
    assert!(parse_jsonl(format!("{newer}\n")).unwrap().is_empty());
    let claimed = after_question_line(LOG_SCHEMA_VERSION, "something_newer");
    assert!(parse_jsonl(format!("{claimed}\n")).is_err());
    for (wire, want) in [
        ("after_question", wisp_protocol::NotRunReason::AfterQuestion),
        ("after_refusal", wisp_protocol::NotRunReason::AfterRefusal),
    ] {
        let known = after_question_line(LOG_SCHEMA_VERSION, wire);
        let records = parse_jsonl(format!("{known}\n")).unwrap();
        assert!(matches!(
            &records[0].event,
            LogEvent::ToolCallFinished {
                outcome: wisp_protocol::ToolOutcome::NotRun { reason },
                result: None,
                ..
            } if *reason == want
        ));
    }
}

#[test]
fn a_version_eight_log_still_loads() {
    let line = after_question_line(8, "turn_stopped");
    let records = parse_jsonl(format!("{line}\n")).unwrap();
    assert_eq!(records[0].schema_version, 8);
    assert!(matches!(
        &records[0].event,
        LogEvent::ToolCallFinished { result: None, .. }
    ));
}

#[test]
fn question_text_answers_tool_results_and_a_code_views_code_are_exempt_from_redaction() {
    use wisp_protocol::{
        Answer, OutputKind, Question, QuestionEnd, QuestionOption, ToolAction, ToolDisplay,
        ToolOutcome, ToolOutputView, ToolPurpose, ToolView,
    };
    let store = MemoryStore::default();
    let mut log = SessionLog::new(Box::new(store.clone()), "s");
    register(&Redactions::new(["/v1".to_owned()]));
    let questions = vec![Question {
        header: "API /v1".into(),
        question: "Use the /v1 API?".into(),
        options: vec![
            QuestionOption {
                label: "Keep /v1".into(),
                description: "Stay on /v1.".into(),
            },
            QuestionOption {
                label: "Move".into(),
                description: String::new(),
            },
        ],
        multi_select: false,
        source: None,
    }];
    let how = QuestionEnd::Answered {
        answers: vec![Answer::own("use the /v1 API")],
    };
    let result = "The user answered:\nIn their own words: use the /v1 API".to_owned();
    log.record(LogEvent::QuestionAsked {
        turn: 1,
        call_id: "t/v1".into(),
        questions: questions.clone(),
    });
    log.record(LogEvent::QuestionClosed {
        turn: 1,
        call_id: "t/v1".into(),
        how: how.clone(),
    });
    log.record(LogEvent::ToolCallFinished {
        turn: 1,
        call_id: "t/v1".into(),
        outcome: ToolOutcome::Failed {
            message: "reached /v1".into(),
            before_start: false,
        },
        output: ToolOutputView {
            tail: "GET /v1".into(),
            ..ToolOutputView::default()
        },
        facts: Vec::new(),
        state: None,
        duration_ms: 1,
        result: Some(result.clone()),
        process: None,
        attachments: Vec::new(),
        notes: None,
        typed: None,
    });
    let code = "curl https://api.example.com/v1/models".to_owned();
    assert!(holds(
        &json!({"nested": [null, false, 1, {"text": format!("{code}\r\n") }]}),
        &code
    ));
    assert!(!holds(&json!({"code": [null, false, 1]}), "code"));
    assert!(holds(&json!({"text": ["anything"]}), ""));
    assert!(!holds(&json!([null, false, 1]), ""));
    let shown = |at: &str, language: &str, input: &str, code: &str| LogEvent::ToolCallRequested {
        turn: 1,
        step: 1,
        block: 0,
        call_id: "c/v1".into(),
        tool: "show_code".into(),
        input: json!({"code": input, "title": format!("List {at}")}),
        display: ToolDisplay {
            action: ToolAction::Show,
            verbs: None,
            target: format!("List {at}"),
            output: OutputKind::None,
            view: Some(ToolView::Code {
                language: Some(language.to_owned()),
                code: code.to_owned(),
            }),
            title: Some(format!("List {at}")),
            purpose: ToolPurpose::Show,
        },
        refused: false,
    };
    log.record(shown("/v1", "bash /v1", &format!("{code}\r\n"), &code));
    log.record(shown("/v1", "bash /v1", "echo ready", &code));
    let events: Vec<LogEvent> = store
        .read("s")
        .unwrap()
        .into_iter()
        .map(|r| r.event)
        .collect();
    assert_eq!(events.len(), 5, "{events:?}");
    assert_eq!(
        events[3],
        shown(
            "[redacted]",
            "bash [redacted]",
            "curl https://api.example.com[redacted]/models\r\n",
            &code
        )
    );
    assert_eq!(
        events[4],
        shown(
            "[redacted]",
            "bash [redacted]",
            "echo ready",
            "curl https://api.example.com[redacted]/models"
        )
    );
    assert_eq!(
        events[..3],
        [
            LogEvent::QuestionAsked {
                turn: 1,
                call_id: "t/v1".into(),
                questions,
            },
            LogEvent::QuestionClosed {
                turn: 1,
                call_id: "t/v1".into(),
                how,
            },
            LogEvent::ToolCallFinished {
                turn: 1,
                call_id: "t/v1".into(),
                outcome: ToolOutcome::Failed {
                    message: "reached [redacted]".into(),
                    before_start: false,
                },
                output: ToolOutputView {
                    tail: "GET [redacted]".into(),
                    ..ToolOutputView::default()
                },
                facts: Vec::new(),
                state: None,
                duration_ms: 1,
                result: Some(result),
                process: None,
                attachments: Vec::new(),
                notes: None,
                typed: None,
            },
        ]
    );
}

#[test]
fn search_records_are_read_strictly_and_older_versions_still_load() {
    let line = |seq: u64, version: u16, body: serde_json::Value| {
        let mut value =
            json!({"schema_version": version, "seq": seq, "at": "2026-10-23T12:00:00Z"});
        if let (Some(map), serde_json::Value::Object(body)) = (value.as_object_mut(), body) {
            map.extend(body);
        }
        format!("{value}\n")
    };
    let log = [
        line(0, 9, json!({"type": "turn_started", "turn": 1, "text": "hi"})),
        line(1, 9, json!({"type": "message_committed", "turn": 1, "message": {"role": "assistant", "content": [{"type": "text", "text": "a"}]}})),
        line(2, 10, json!({"type": "tail_closed", "through": 1, "calls": ["srvtoolu_1"], "reason": "stopped"})),
        line(3, 10, json!({"type": "rendering_form", "form": "flattened"})),
        line(4, 10, json!({"type": "search_history_rendered", "endpoint_name": "direct"})),
        line(5, LOG_SCHEMA_VERSION + 1, json!({"type": "tail_closed", "through": 1, "calls": [], "reason": "vanished"})),
    ]
    .concat();
    let records = parse_jsonl(&log).unwrap();
    assert_eq!(records.len(), 5);
    let store = MemoryStore::default();
    let rebuilt = rebuild(&store, records.iter().map(|r| &r.event)).unwrap();
    assert_eq!(rebuilt.messages.len(), 2);
    assert!(rebuilt.closed.calls.contains("srvtoolu_1"));
    assert!(rebuilt.closed.tails.contains(&1));

    let bad = [line(
        0,
        10,
        json!({"type": "tail_closed", "through": 1, "calls": [], "reason": "vanished"}),
    )]
    .concat();
    assert!(parse_jsonl(&bad).is_err());
    let later_reason = [
        line(0, 10, json!({"type": "settings_changed", "model": "m", "effort": "low", "cache": {"type": "rebuilt", "reason": "something_new"}})),
    ]
    .concat();
    let later = parse_jsonl(&later_reason).unwrap();
    assert!(
        matches!(
            &later[0].event,
            LogEvent::SettingsChanged {
                cache: wisp_protocol::CacheEffect::Rebuilt {
                    reason: wisp_protocol::RebuildReason::Other(text)
                },
                ..
            } if text == "something_new"
        ),
        "{later:?}"
    );
    let refused = [
        line(0, 10, json!({"type": "settings_changed", "model": "m", "effort": "low", "cache": {"type": "rebuilt", "reason": "native_search_refused"}})),
    ]
    .concat();
    assert_eq!(parse_jsonl(&refused).unwrap().len(), 1);
}

#[test]
fn what_resume_reads_back_is_exempt_from_redaction() {
    use wisp_protocol::{
        CandidateInfo, Diagnostic, DiagnosticKind, InstructionsSource, Scope, SourceInfo,
        ToolOutcome, ToolOutputView, WorkspaceChange,
    };
    let store = MemoryStore::default();
    let mut log = SessionLog::new(Box::new(store.clone()), "s");
    let base_digest = sha256_hex("base /v1");
    register(&Redactions::new([
        "/v1".to_owned(),
        "CANARYKEY0001".to_owned(),
        base_digest[8..24].to_owned(),
    ]));
    let block = "<instructions>\nUse the /v1 API.\n</instructions>".to_owned();
    let sections = |text: &str| {
        vec![wisp_protocol::log::SectionSnapshot {
            name: "acme/rules".into(),
            snapshot: text.into(),
        }]
    };
    let started = |system: &str| LogEvent::SessionStarted {
        system_base_sha256: Some(base_digest.clone()),
        sections: sections("Read /tmp/x/v1/proj/RULES.md"),
        plugins: Vec::new(),
        session_id: "0199-abc".into(),
        wisp_version: "0.1.0".into(),
        model: "claude-opus-5-5".into(),
        effort: Effort::Medium,
        cache_ttl: CacheTtl::OneHour,
        system_prompt: Some(system.into()),
        system_prompt_sha256: Some(sha256_hex("sys /v1")),
        endpoint: "https://api.anthropic.com".into(),
        credential_source: CredentialSource::Environment,
        endpoint_name: Some("env".into()),
        message_time: Some(MessageTime::Local),
        tools: vec!["bash".into()],
        tools_sha256: Some(sha256_hex("bash /v1")),
        whole: Default::default(),
        cwd: Some("/tmp/x/v1/proj".into()),
        output_dir: None,
        instructions: Some(block.clone()),
        instructions_sha256: Some(sha256_hex(&block)),
        instructions_source: Some(InstructionsSource::Auto),
        instructions_extra: None,
        preset: None,
        context_note_enabled: None,
        debug_cache: None,
        refusal_fallback: None,
        parent: None,
        headless: false,
    };
    let source = SourceInfo {
        scope: Scope::Project,
        name: "docs/v1/x.md".into(),
        display_path: "/tmp/x/v1/proj/docs/v1/x.md".into(),
        bytes: 3,
        sha256: "ab".repeat(32),
        imported_by: Some("AGENTS.md".into()),
        external: false,
    };
    let loaded = |message: &str| LogEvent::InstructionsLoaded {
        turn: None,
        chosen: Some(SourceInfo {
            name: "AGENTS.md".into(),
            display_path: "/tmp/x/v1/proj/AGENTS.md".into(),
            imported_by: None,
            ..source.clone()
        }),
        imports: vec![source.clone()],
        candidates: vec![CandidateInfo {
            scope: Scope::Project,
            display_path: "/tmp/x/v1/proj/AGENTS.md".into(),
            bytes: Some(3),
            mtime: Some(1),
            sha256: Some("ab".repeat(32)),
        }],
        diagnostics: vec![Diagnostic {
            source: Some("/tmp/x/v1/proj/gone.md".into()),
            kind: DiagnosticKind::ImportMissing,
            message: message.into(),
        }],
    };
    let finished = |path: &str, tail: &str| LogEvent::ToolCallFinished {
        turn: 1,
        call_id: "t".into(),
        outcome: ToolOutcome::Failed {
            message: "x".into(),
            before_start: false,
        },
        output: ToolOutputView {
            tail: tail.into(),
            file: Some("/tmp/x/v1/out/3-bash.log".into()),
            ..ToolOutputView::default()
        },
        facts: Vec::new(),
        state: Some(WorkspaceChange::Cwd { path: path.into() }),
        duration_ms: 1,
        result: None,
        process: None,
        attachments: Vec::new(),
        notes: None,
        typed: None,
    };
    log.record(started("sys /v1"));
    log.record(loaded("key CANARYKEY0001 at /tmp/x/v1/proj/gone.md"));
    log.record(finished("/tmp/x/v1/proj/sub", "GET /v1/models"));
    let process = |call_id: &str| LogEvent::ProcessStarted {
        turn: 1,
        id: 1,
        call_id: call_id.into(),
        reason: wisp_protocol::BackgroundReason::Started,
        tty: false,
        stdin: false,
        size: None,
        file: Some("/tmp/x/v1/out/1-bash.log".into()),
        pid: None,
        sid: None,
        start_ticks: None,
        boot_id: None,
        keeps_running: false,
        log: None,
    };
    log.record(process("c/v1"));
    let call = |cwd: &str| LogEvent::ToolCallStarted {
        turn: 1,
        call_id: "t".into(),
        cwd: cwd.into(),
        movable: false,
        file: Some("/tmp/x/v1/out/3-bash.log".into()),
    };
    log.record(call("~/v1"));
    let events: Vec<LogEvent> = store
        .read("s")
        .unwrap()
        .into_iter()
        .map(|r| r.event)
        .collect();
    assert_eq!(events[0], started("sys [redacted]"));
    assert_eq!(
        events[1],
        loaded("key [redacted] at /tmp/x[redacted]/proj/gone.md")
    );
    assert_eq!(
        events[2],
        finished("/tmp/x/v1/proj/sub", "GET [redacted]/models")
    );
    assert_eq!(events[3], process("c/v1"));
    assert_eq!(events[4], call("~[redacted]"));
    let every = one_of_every_event();
    let with_sections = |mut event: LogEvent, text: &str| {
        match &mut event {
            LogEvent::SessionResumed { sections: s, .. } => *s = Some(sections(text)),
            LogEvent::ContextCompacted { sections: s, .. } => *s = sections(text),
            _ => {}
        }
        event
    };
    let resumed = with_sections(
        every
            .iter()
            .find(|e| matches!(e, LogEvent::SessionResumed { .. }))
            .unwrap()
            .clone(),
        "<environment>\nWorking directory: ~/v1/proj\n</environment>\n",
    );
    let compacted = with_sections(
        every
            .iter()
            .find(|e| matches!(e, LogEvent::ContextCompacted { .. }))
            .unwrap()
            .clone(),
        "<environment>\nAgents: /v1-review\n</environment>\n",
    );
    log.record(resumed.clone());
    log.record(compacted.clone());
    let records = store.read("s").unwrap();
    assert_eq!(records[5].event, resumed);
    assert_eq!(records[6].event, compacted);
    let rebuilt = |system: &str| LogEvent::ToolsChanged {
        turn: 2,
        via: wisp_protocol::ToolChangeVia::Rebuild,
        added: Vec::new(),
        removed: Vec::new(),
        copy: Box::new(wisp_protocol::log::PrefixCopy {
            system_prompt: Some(system.into()),
            system_prompt_sha256: Some(base_digest.clone()),
            system_base_sha256: Some(base_digest.clone()),
            tools: vec!["bash".into()],
            tools_sha256: Some(base_digest.clone()),
            ..Default::default()
        }),
    };
    log.record(rebuilt("sys /v1"));
    assert_eq!(store.read("s").unwrap()[7].event, rebuilt("sys [redacted]"));
}

#[test]
fn the_instructions_a_resume_logged_round_trip_under_redaction() {
    use wisp_protocol::InstructionsSource;
    let store = MemoryStore::default();
    let mut log = SessionLog::new(Box::new(store.clone()), "s");
    register(&Redactions::new([
        "/v1".to_owned(),
        "autonomous".to_owned(),
    ]));
    let first = "<instructions>\nUse the /v1 API.\n</instructions>".to_owned();
    let second = "<instructions>\nRead /tmp/x/v1/notes.md first.\n</instructions>".to_owned();
    let mut started = one_of_every_event()
        .into_iter()
        .find(|e| matches!(e, LogEvent::SessionStarted { .. }))
        .unwrap();
    if let LogEvent::SessionStarted {
        instructions,
        instructions_sha256,
        instructions_source,
        instructions_extra,
        parent,
        ..
    } = &mut started
    {
        *instructions_extra = Some("autonomous".into());
        *instructions = Some(first.clone());
        *instructions_sha256 = Some(sha256_hex(&first));
        *instructions_source = Some(InstructionsSource::Auto);
        *parent = None;
    }
    let resumed = |system: &str| LogEvent::SessionResumed {
        credential_source: None,
        system_base_sha256: None,
        sections: None,
        wisp_version: "0.1.0".into(),
        endpoint: "https://api.anthropic.com".into(),
        endpoint_name: "env".into(),
        system_prompt_sha256: None,
        tools: Vec::new(),
        tools_sha256: None,
        system_prompt: Some(system.into()),
        whole: Default::default(),
        tools_on: None,
        instructions_sha256: Some(sha256_hex(&second)),
        instructions_source: Some(InstructionsSource::Default),
        instructions: Some(second.clone()),
        instructions_extra: Some("none".into()),
        cwd: Some("/tmp/x/v1/proj".into()),
        gap_ms: None,
        truncated_bytes: 0,
        returned: Vec::new(),
    };
    log.record(started);
    log.record(resumed("sys /v1"));
    let records = store.read("s").unwrap();
    assert_eq!(records[1].event, resumed("sys [redacted]"));
    let logged = crate::resume::LoggedSession::read(&records).unwrap();
    assert_eq!(logged.instructions.as_deref(), Some(second.as_str()));
    assert_eq!(
        logged.instructions_source,
        Some(InstructionsSource::Default)
    );
    assert_eq!(logged.instructions_sha256, Some(sha256_hex(&second)));
    assert_eq!(logged.started.instructions.as_deref(), Some(first.as_str()));
    assert_eq!(logged.instructions_extra.as_deref(), Some("none"));
    assert_eq!(
        logged.started.instructions_extra.as_deref(),
        Some("autonomous")
    );
}

#[test]
fn agent_paths_names_and_preset_files_resume_reads_back_are_exempt_from_redaction() {
    use wisp_protocol::agents::Preset;
    use wisp_protocol::{AgentPath, MailFrom, MailId, MailKind, ParentLink, SpawnChoices};
    let store = MemoryStore::default();
    let mut log = SessionLog::new(Box::new(store.clone()), "s");
    register(&Redactions::new(["/api".to_owned()]));
    let choices = SpawnChoices {
        preset: Some("reviewer".into()),
        instructions: Some("~/api-roles/reviewer/AGENTS.md".into()),
        preset_values: Some(Box::new(Preset {
            name: "reviewer".into(),
            description: "Reviews /api changes".into(),
            model: None,
            effort: None,
            preload: None,
            tools_off: Vec::new(),
            spawn: None,
            instructions: Some("~/api-roles/reviewer".into()),
            endpoint: None,
            fallback: vec![wisp_protocol::agents::Choice {
                endpoint: "/api".into(),
                model: Some("/api".into()),
                effort: None,
            }],
            worker: None,
        })),
        choices: vec![wisp_protocol::agents::Choice {
            endpoint: "/api".into(),
            model: Some("/api".into()),
            effort: None,
        }],
        ..SpawnChoices::default()
    };
    let spawned = LogEvent::AgentSpawned {
        turn: Some(1),
        agent: AgentPath::new("/api-review"),
        session_id: "s-child".into(),
        depth: 1,
        model: "claude-opus-5-5".into(),
        effort: Effort::High,
        cwd: Some("~/api-work".into()),
        config: choices.clone(),
        endpoint: None,
        provider: None,
    };
    let ended = LogEvent::AgentEnded {
        billing: wisp_protocol::Billing::Metered,
        plan_usd: None,
        turn: Some(2),
        agent: AgentPath::new("/api-review"),
        spent_usd: 0.5,
    };
    let reported = LogEvent::AgentReported {
        turn: 1,
        agent: AgentPath::new("/api-review"),
        outcome: wisp_protocol::ReportOutcome::Reported,
        report: Some(json!("Done.")),
        duration_ms: 10,
    };
    let every = one_of_every_event();
    let mut accepted = every
        .iter()
        .find(|e| matches!(e, LogEvent::InputAccepted { .. }))
        .unwrap()
        .clone();
    if let LogEvent::InputAccepted { from, .. } = &mut accepted {
        *from = Some(MailFrom {
            agent: AgentPath::new("/api-review"),
            kind: MailKind::Report,
            mail: MailId("m1".into()),
            wake: true,
            plugin: None,
        });
    }
    let call = every
        .iter()
        .find(|e| matches!(e, LogEvent::ToolCallRequested { .. }))
        .unwrap()
        .clone();
    let requested = |name: &str, to: &str| {
        let mut event = call.clone();
        if let LogEvent::ToolCallRequested { tool, input, .. } = &mut event {
            *tool = name.into();
            *input = json!({"to": to, "text": "hi"});
        }
        event
    };
    let mut with_code = requested("send_message", "/api-review");
    if let LogEvent::ToolCallRequested { input, display, .. } = &mut with_code {
        input["code"] = json!("echo /api");
        display.view = Some(wisp_protocol::ToolView::Code {
            language: None,
            code: "echo /api".into(),
        });
    }
    let mut started = every
        .iter()
        .find(|e| matches!(e, LogEvent::SessionStarted { .. }))
        .unwrap()
        .clone();
    if let LogEvent::SessionStarted { parent, .. } = &mut started {
        *parent = Some(Box::new(ParentLink {
            session_id: "s-root".into(),
            agent: AgentPath::new("/api-review"),
            depth: 1,
            config: choices.clone(),
            todos: Vec::new(),
        }));
    }
    for event in [
        started.clone(),
        spawned.clone(),
        ended.clone(),
        reported.clone(),
        accepted.clone(),
        requested("send_message", "/api-review"),
        requested("other", "/api-review"),
        with_code,
    ] {
        log.record(event);
    }
    let events: Vec<LogEvent> = store
        .read("s")
        .unwrap()
        .into_iter()
        .map(|r| r.event)
        .collect();
    let LogEvent::SessionStarted { parent, .. } = &events[0] else {
        panic!("{:?}", events[0]);
    };
    let parent = parent.as_deref().unwrap();
    assert_eq!(parent.agent.as_str(), "/api-review");
    assert_eq!(parent.config.instructions, choices.instructions);
    assert_eq!(
        parent.config.preset_values.as_ref().unwrap().instructions,
        choices.preset_values.as_ref().unwrap().instructions
    );
    assert_eq!(
        parent.config.preset_values.as_ref().unwrap().description,
        "Reviews [redacted] changes"
    );
    for fallback in [
        &parent.config.preset_values.as_ref().unwrap().fallback[0],
        &parent.config.choices[0],
    ] {
        assert_eq!(fallback.endpoint, "[redacted]");
        assert_eq!(fallback.model.as_deref(), Some("[redacted]"));
    }
    let LogEvent::AgentSpawned {
        agent, cwd, config, ..
    } = &events[1]
    else {
        panic!("{:?}", events[1]);
    };
    assert_eq!(agent.as_str(), "/api-review");
    assert_eq!(cwd.as_deref(), Some("~/api-work"));
    assert_eq!(config.instructions, choices.instructions);
    assert_eq!(
        config.preset_values.as_ref().unwrap().instructions,
        choices.preset_values.as_ref().unwrap().instructions
    );
    assert_eq!(events[2], ended);
    assert_eq!(events[3], reported);
    assert_eq!(events[4], accepted);
    assert_eq!(events[5], requested("send_message", "/api-review"));
    assert_eq!(events[6], requested("other", "[redacted]-review"));
    assert_eq!(
        serde_json::to_value(&events[7]).unwrap()["display"]["view"]["code"],
        "echo [redacted]"
    );
}

mod lock;

fn folded_after_masking(
    unmasked: Vec<serde_json::Value>,
    masked: Vec<serde_json::Value>,
) -> crate::resume::LoggedSession {
    let started = one_of_every_event()
        .into_iter()
        .find(|e| matches!(e, LogEvent::SessionStarted { .. }))
        .unwrap();
    let mut events = vec![started];
    events.extend(
        unmasked
            .into_iter()
            .map(|v| serde_json::from_value::<LogEvent>(v).unwrap()),
    );
    let to = u64::try_from(events.len()).unwrap() - 1;
    events.push(
        serde_json::from_value(json!({"type": "turn_started", "turn": 2, "text": "two"})).unwrap(),
    );
    events.extend(
        masked
            .into_iter()
            .map(|v| serde_json::from_value::<LogEvent>(v).unwrap()),
    );
    events.push(LogEvent::Rewound {
        to,
        point: wisp_protocol::RewindPoint::Turn { turn: 2 },
        reason: wisp_protocol::RewindReason::User,
        messages: 2,
        blocks: 1,
        dropped: wisp_protocol::Dropped {
            turns: vec![2],
            messages: 0,
        },
        cache: wisp_protocol::CacheEffect::Kept,
    });
    let records: Vec<LogRecord> = events
        .into_iter()
        .zip(0..)
        .map(|(event, seq)| LogRecord {
            schema_version: LOG_SCHEMA_VERSION,
            seq,
            at: "2026-10-25T12:00:00Z".into(),
            event,
        })
        .collect();
    crate::resume::LoggedSession::read(&records).unwrap()
}

fn turn_one() -> Vec<serde_json::Value> {
    vec![
        json!({"type": "turn_started", "turn": 1, "text": "one", "context": "Context: here"}),
        json!({"type": "message_committed", "turn": 1, "message": {"role": "assistant", "content": [{"type": "text", "text": "one"}]}}),
    ]
}

fn accepted_step(turn: u64) -> serde_json::Value {
    json!({"type": "step_ended", "turn": turn, "step": 1, "usage": null, "cost_usd": null, "lower_bound": false, "prompt_tokens": 5000, "response_id": null, "accepted": true, "started_at": "2026-10-25T12:00:00Z"})
}

#[test]
fn a_masked_step_never_becomes_the_last_step_even_after_an_unanswered_reply() {
    let logged = folded_after_masking(turn_one(), vec![accepted_step(2)]);
    assert!(logged.last_step.is_none(), "{:?}", logged.last_step);
    let mut answered = turn_one();
    answered.push(accepted_step(1));
    let logged = folded_after_masking(answered, Vec::new());
    assert_eq!(logged.last_step.map(|s| s.turn), Some(1));
}

#[cfg(unix)]
#[test]
fn store_errors_name_their_paths_under_home_with_a_tilde() {
    let root = temp_dir("blob-home");
    let open = || {
        JsonlStore::open(root.join("wisp").join("sessions"))
            .unwrap()
            .with_home(Some(root.clone()))
    };
    let mut store = open();
    let (sha, bytes) = png_like("a");
    let err = |e: io::Error| e.to_string();
    assert_eq!(
        err(store.get_blob(&sha, &png()).unwrap_err()),
        format!("no file named {sha}.png in ~/wisp/attachments or ~/wisp/images/{sha}.png")
    );
    assert_eq!(
        err(store.get_blob(&sha, &mime("application/pdf")).unwrap_err()),
        format!("no file named {sha}.pdf in ~/wisp/attachments")
    );
    fs::create_dir_all(store.attachment_path(&sha, &png())).unwrap();
    assert_eq!(
        err(store.get_blob(&sha, &png()).unwrap_err()),
        format!("~/wisp/attachments/{sha}.png is not a regular file")
    );
    fs::remove_dir_all(store.attachments_dir()).unwrap();
    fs::create_dir_all(store.attachments_dir()).unwrap();
    std::os::unix::fs::symlink(
        root.join("elsewhere.png"),
        store.attachment_path(&sha, &png()),
    )
    .unwrap();
    assert_eq!(
        err(store.put_blob(&sha, &png(), &bytes).unwrap_err()),
        format!("~/wisp/attachments/{sha}.png is a symlink; Wisp won't write through it")
    );
    fs::remove_dir_all(store.attachments_dir()).unwrap();
    fs::create_dir_all(root.join("other")).unwrap();
    std::os::unix::fs::symlink(root.join("other"), store.attachments_dir()).unwrap();
    assert_eq!(
        err(store.put_blob(&sha, &png(), &bytes).unwrap_err()),
        "~/wisp/attachments is a symlink; Wisp won't write through it"
    );
    let mut elsewhere = open().with_home(Some(root.join("someone-else")));
    assert_eq!(
        err(elsewhere.put_blob(&sha, &png(), &bytes).unwrap_err()),
        format!(
            "{} is a symlink; Wisp won't write through it",
            store.attachments_dir().display()
        )
    );
    fs::remove_dir_all(&root).unwrap();
}

pub(super) fn matches_the_buffered_digest(value: &impl serde::Serialize, digest: &str) {
    let buffered = serde_json::to_vec(value).map(|bytes| sha256_bytes_hex(&bytes));
    assert_eq!(buffered.ok().as_deref(), Some(digest));
}

#[test]
fn hashing_while_serialising_equals_hashing_the_serialised_bytes() {
    let values = [
        json!({}),
        json!({"model": "m", "messages": [{"role": "user", "content": "héllo \u{1}"}]}),
        json!([1, 2.5, null, true, "x".repeat(70_000)]),
    ];
    for value in &values {
        let streamed = sha256_json_hex(value);
        let buffered = serde_json::to_vec(value).map(|bytes| sha256_bytes_hex(&bytes));
        assert_eq!(streamed, buffered.ok());
    }
    assert_eq!(
        sha256_json_hex(&"").as_deref(),
        Some("12ae32cb1ec02d01eda3581b127c1fee3b0dc53572ed6baf239721a03d82e126")
    );
}

#[test]
fn a_stale_journal_generation_neither_appends_nor_allocates_a_sequence() {
    use wisp_protocol::{DeliveryMode, HookChange, InputId, TextSpan};

    let store = MemoryStore::default();
    let mut log = SessionLog::new(Box::new(store.clone()), "checked-record");
    let (_, stale) = held_with_generation();
    register_secrets(["CANARYOpaqueForStaleJournalSequence".into()]);
    assert_eq!(
        log.record_if_generation_current(stale, started(), None),
        Err(())
    );
    assert_eq!(log.next_seq(), 0);
    assert!(store.list().unwrap().is_empty());

    let (_, current) = held_with_generation();
    assert_eq!(
        log.record_if_generation_current(current, started(), None),
        Ok(None)
    );
    assert_eq!(log.next_seq(), 1);

    let home = Path::new("/opt/wisp-test-home");
    let home_field = "/opt/wisp-test-home/notes";
    let hook_event = LogEvent::HookApplied {
        hook: "aa/h".into(),
        point: "before-tool".into(),
        turn: None,
        step: None,
        call_id: None,
        input_id: None,
        change: HookChange::Input {
            before: json!({"path": home_field}),
            after: json!({"path": "ok"}),
        },
    };
    let prose = "Task prose\n- [ ] ";
    let brief_text = format!("{prose}{home_field}");
    let brief_event = LogEvent::InputAccepted {
        turn: None,
        id: InputId("mail-home".into()),
        mode: DeliveryMode::Steer,
        text: brief_text.clone(),
        images: Vec::new(),
        replaces: None,
        held: false,
        from: None,
        checklist_spans: vec![TextSpan {
            start: prose.len(),
            end: brief_text.len(),
        }],
    };
    assert_eq!(
        log.record_if_generation_current(stale, hook_event.clone(), Some(home)),
        Err(())
    );
    assert_eq!(log.next_seq(), 1);
    assert_eq!(
        log.record_if_generation_current(current, hook_event, Some(home)),
        Ok(None)
    );
    assert_eq!(
        log.record_if_generation_current(current, brief_event, Some(home)),
        Ok(None)
    );
    assert_eq!(log.next_seq(), 3);
    let rows = store.read("checked-record").unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].seq, 0);
    assert_eq!(rows[1].seq, 1);
    assert_eq!(rows[2].seq, 2);
    assert!(matches!(
        &rows[1].event,
        LogEvent::HookApplied {
            change: HookChange::Input { before, .. },
            ..
        } if before == &json!({"path": REDACTED})
    ));
    assert!(matches!(
        &rows[2].event,
        LogEvent::InputAccepted { text, .. } if text == &format!("{prose}{REDACTED}")
    ));
}

#[test]
fn record_if_generation_current_excludes_secret_registration_across_append_and_drops_guard_before_sync()
 {
    use std::sync::{Arc, Mutex};
    use wisp_protocol::{InputId, TextSpan};

    struct ProbeStore {
        memory: MemoryStore,
        at_append: Mutex<Option<std::sync::mpsc::Sender<()>>>,
        probed: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
        append_blocked: Arc<std::sync::atomic::AtomicBool>,
        sync_blocked: Arc<std::sync::atomic::AtomicBool>,
    }

    impl SessionStore for ProbeStore {
        fn append(&mut self, session: &str, row: &LogRecord) -> std::io::Result<()> {
            self.append_blocked.store(
                secrets_write_blocked(),
                std::sync::atomic::Ordering::Release,
            );
            if let Some(tx) = self.at_append.lock().unwrap().take() {
                tx.send(()).unwrap();
            }
            if let Some(rx) = self.probed.lock().unwrap().take() {
                rx.recv().unwrap();
            }
            self.memory.append(session, row)
        }

        fn sync(&mut self, session: &str) -> std::io::Result<()> {
            self.sync_blocked.store(
                secrets_write_blocked(),
                std::sync::atomic::Ordering::Release,
            );
            self.memory.sync(session)
        }

        fn read(&self, session: &str) -> std::io::Result<Vec<LogRecord>> {
            self.memory.read(session)
        }

        fn list(&self) -> std::io::Result<Vec<String>> {
            self.memory.list()
        }

        fn put_blob(
            &mut self,
            sha256: &str,
            media_type: &Mime,
            bytes: &[u8],
        ) -> std::io::Result<()> {
            self.memory.put_blob(sha256, media_type, bytes)
        }

        fn get_blob(&self, sha256: &str, media_type: &Mime) -> std::io::Result<Blob> {
            self.memory.get_blob(sha256, media_type)
        }
    }

    reset_held_for_test();
    let canary = "CANARYOpaqueOrderedAcrossCheckedAppend";
    let prose = "Task prose\n- [ ] ";
    let text = format!("{prose}{canary}");
    let event = LogEvent::InputRevised {
        turn: None,
        inputs: vec![wisp_protocol::log::LoggedInput {
            id: InputId("mail-ordered".into()),
            text: text.clone(),
            images: Vec::new(),
            note: None,
            checklist_spans: vec![TextSpan {
                start: prose.len(),
                end: text.len(),
            }],
        }],
    };
    let memory = MemoryStore::default();
    let append_blocked = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sync_blocked = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (at_append_tx, at_append_rx) = std::sync::mpsc::channel();
    let (probed_tx, probed_rx) = std::sync::mpsc::channel();
    let store = ProbeStore {
        memory: memory.clone(),
        at_append: Mutex::new(Some(at_append_tx)),
        probed: Mutex::new(Some(probed_rx)),
        append_blocked: append_blocked.clone(),
        sync_blocked: sync_blocked.clone(),
    };
    let mut log = SessionLog::new(Box::new(store), "probe");
    let (_, current) = held_with_generation();
    let worker = std::thread::spawn(move || {
        at_append_rx.recv().unwrap();
        let blocked = secrets_write_blocked();
        probed_tx.send(()).unwrap();
        register_secrets([canary.to_owned()]);
        blocked
    });
    assert_eq!(
        log.record_if_generation_current(current, event, None),
        Ok(None)
    );
    let worker_blocked = worker.join().unwrap();
    assert_eq!(log.sync(), None);
    assert!(worker_blocked);
    assert!(append_blocked.load(std::sync::atomic::Ordering::Acquire));
    assert!(!sync_blocked.load(std::sync::atomic::Ordering::Acquire));
    let saved = memory.read("probe").unwrap();
    assert_eq!(saved.len(), 1);
    assert!(matches!(
        &saved[0].event,
        LogEvent::InputRevised { inputs, .. } if inputs[0].text.contains(canary)
    ));
}

#[tokio::test]
async fn send_scrubbed_handles_hook_audit_and_spanned_brief_together_without_deadlock_or_double_mangling()
 {
    use wisp_protocol::{
        AgentEvent, ErrorKind, HookChange, RequestId, SessionView, TurnOutcome, TurnStats,
    };

    let canary = "CANARYOpaqueCombinedAuditBrief";
    let store = MemoryStore::default();
    let mut agent = crate::agent::Agent::new(
        crate::agent::testing::Fake::new(Vec::new()),
        crate::agent::default_session(),
    )
    .with_log(
        SessionLog::new(Box::new(store.clone()), "combined"),
        crate::test_guard::log_context(),
    );
    let primed = crate::agent::testing::prime_brief(
        &mut agent,
        crate::agent::testing::checklist_mail("combined", "Keep task", &[canary]),
    )
    .await
    .into_iter()
    .find(|event| matches!(event, AgentEvent::InputAccepted { .. }))
    .unwrap();
    let owner = agent.brief_owner();
    register_secrets([canary.into()]);
    let AgentEvent::SessionSnapshot { now, .. } = serde_json::from_str(include_str!(
        "../../../wisp-protocol/tests/fixtures/event_session_snapshot.json"
    ))
    .unwrap() else {
        panic!("expected a session snapshot fixture");
    };
    let hooked = |change| AgentEvent::HookApplied {
        hook: "aa/h".into(),
        point: "before-tool".into(),
        turn: None,
        step: None,
        call_id: None,
        input_id: None,
        change,
    };
    let hook_event = hooked(HookChange::Input {
        before: json!({"k": canary}),
        after: json!({"k": canary, (REDACTED): "kept"}),
    });
    let terminal_event = AgentEvent::TurnEnded {
        outcome: TurnOutcome::Error {
            kind: ErrorKind::InvalidResponse,
            message: compaction_terminal_message(canary.to_owned()),
            usage: None,
            cost: None,
            committed_steps: 0,
            detail: None,
            attempts: 1,
            gave_up: None,
            offer: None,
        },
        stats: TurnStats::default(),
    };
    let snapshot = AgentEvent::SessionSnapshot {
        answers: RequestId(9),
        view: SessionView {
            events: vec![
                hook_event,
                terminal_event,
                primed,
                hooked(HookChange::Result {
                    before: canary.into(),
                    after: format!("{canary} tail"),
                }),
                hooked(HookChange::Text {
                    before: format!("typed {canary}"),
                    after: canary.into(),
                }),
                hooked(HookChange::Continue {
                    text: format!("{canary}!"),
                }),
            ],
            ..SessionView::default()
        },
        now,
        state: Vec::new(),
    };
    let (relay, mut received) = tokio::sync::mpsc::channel(1);
    crate::test_guard::within(
        "combined hook audit and spanned brief send_scrubbed",
        crate::agent::send_scrubbed(&relay, snapshot, None, |_| Some(owner.clone())),
    )
    .await;
    let AgentEvent::SessionSnapshot { view, .. } = received.try_recv().unwrap() else {
        panic!("expected snapshot");
    };
    assert!(received.try_recv().is_err());
    let [
        AgentEvent::HookApplied {
            change: HookChange::Input { before, after },
            ..
        },
        AgentEvent::TurnEnded {
            outcome: TurnOutcome::Error { message, .. },
            ..
        },
        AgentEvent::InputAccepted {
            text,
            checklist_spans,
            ..
        },
        held_changes @ ..,
    ] = view.events.as_slice()
    else {
        panic!("{:#?}", view.events);
    };
    assert_eq!(before, &json!({"k": REDACTED}));
    assert_eq!(after, &json!({"k": REDACTED, (REDACTED): "kept"}));
    assert_eq!(message, "Compaction stopped: [redacted]");
    assert_eq!(
        held_changes,
        [
            hooked(HookChange::Result {
                before: REDACTED.into(),
                after: format!("{REDACTED} tail"),
            }),
            hooked(HookChange::Text {
                before: format!("typed {REDACTED}"),
                after: REDACTED.into(),
            }),
            hooked(HookChange::Continue {
                text: format!("{REDACTED}!"),
            }),
        ]
    );
    assert!(
        text.contains("Keep task") && !text.contains(canary),
        "{text}"
    );
    assert_eq!(checklist_spans.len(), 1);
    assert_eq!(
        &text[checklist_spans[0].start..checklist_spans[0].end],
        REDACTED
    );
    assert!(
        store
            .read("combined")
            .unwrap()
            .iter()
            .any(|row| matches!(&row.event, LogEvent::InputRevised { turn: None, inputs } if inputs[0].text == *text))
    );
}

#[test]
fn session_log_scrubs_viewless_show_cutoff_identity_with_empty_held_while_preserving_valid_report()
{
    use wisp_protocol::{OutputKind, ToolAction, ToolDisplay, ToolPurpose, ToolView, Verbs};

    assert!(held().is_empty());
    let store = MemoryStore::default();
    let mut log = SessionLog::new(Box::new(store.clone()), "s");
    let home = std::path::Path::new("/home/alice");
    let email = "alice@example.invalid";

    log.record_with_home(
        LogEvent::ToolCallRequested {
            turn: 1,
            step: 1,
            block: 0,
            call_id: "c_show_cut".into(),
            tool: "show_diff".into(),
            input: json!({"files": [{"path": format!("{}{email}", "dir/".repeat(19))}]}),
            display: ToolDisplay {
                action: ToolAction::Show,
                verbs: None,
                target: format!("{}alice@\u{2026}", "dir/".repeat(18)),
                output: OutputKind::None,
                view: None,
                title: None,
                purpose: ToolPurpose::Show,
            },
            refused: true,
        },
        Some(home),
    );
    let report_display = ToolDisplay {
        action: ToolAction::Other {
            name: "report".into(),
        },
        verbs: Some(Verbs {
            doing: "Reporting".into(),
            done: "Reported".into(),
            failed: "couldn't report".into(),
        }),
        target: format!("/home/alice/src/main.rs {email}"),
        output: OutputKind::Text,
        view: None,
        title: None,
        purpose: ToolPurpose::Run,
    };
    log.record_with_home(
        LogEvent::ToolCallRequested {
            turn: 1,
            step: 1,
            block: 1,
            call_id: "c_valid_report".into(),
            tool: "report".into(),
            input: json!({"text": format!("/home/alice/src/main.rs {email}")}),
            display: report_display.clone(),
            refused: false,
        },
        Some(home),
    );
    log.record_with_home(
        LogEvent::ToolCallRequested {
            turn: 1,
            step: 1,
            block: 2,
            call_id: "c_write_cut".into(),
            tool: "board_write".into(),
            input: json!({"board": format!("board {email} sk-ant-CANARYLogWrite01234567890123")}),
            display: ToolDisplay {
                action: ToolAction::Write,
                verbs: None,
                target: "board-alice@exam\u{2026}".into(),
                output: OutputKind::None,
                view: None,
                title: None,
                purpose: ToolPurpose::Run,
            },
            refused: true,
        },
        Some(home),
    );
    log.record_with_home(
        LogEvent::ToolCallRequested {
            turn: 1,
            step: 1,
            block: 3,
            call_id: "c_process_cut".into(),
            tool: "process".into(),
            input: json!({"pattern": format!("{}{email}", "pat-".repeat(20))}),
            display: ToolDisplay {
                action: ToolAction::Search,
                verbs: None,
                target: "proc 1".into(),
                output: OutputKind::Text,
                view: Some(ToolView::Process {
                    id: Some(1),
                    text: None,
                    keys: Vec::new(),
                    signal: None,
                    read: None,
                    pattern: Some(format!("{}alice@\u{2026}", "pat-".repeat(15))),
                }),
                title: Some(format!("{}alice@\u{2026}", "pat-".repeat(15))),
                purpose: ToolPurpose::Run,
            },
            refused: true,
        },
        Some(home),
    );
    let show_page_display = ToolDisplay {
        action: ToolAction::Show,
        verbs: None,
        target: "proj_01234567890123456789".into(),
        output: OutputKind::None,
        view: None,
        title: None,
        purpose: ToolPurpose::Show,
    };
    log.record_with_home(
        LogEvent::ToolCallRequested {
            turn: 1,
            step: 1,
            block: 4,
            call_id: "c_show_page".into(),
            tool: "show_page".into(),
            input: json!({"id": "proj_01234567890123456789"}),
            display: show_page_display.clone(),
            refused: false,
        },
        Some(home),
    );

    let events: Vec<LogEvent> = store
        .read("s")
        .unwrap()
        .into_iter()
        .map(|r| r.event)
        .collect();
    assert_eq!(events.len(), 5);
    for idx in [0usize, 2, 3] {
        let LogEvent::ToolCallRequested { display, .. } = &events[idx] else {
            panic!("{:?}", events[idx]);
        };
        assert_eq!(
            display.action,
            ToolAction::Other {
                name: "tool".into()
            },
            "{idx}: {display:?}"
        );
        assert_eq!(display.target, "{}", "{idx}: {display:?}");
        assert_eq!(display.view, None, "{idx}: {display:?}");
        assert_eq!(display.title, None, "{idx}: {display:?}");
    }
    let LogEvent::ToolCallRequested {
        display: kept_report,
        ..
    } = &events[1]
    else {
        panic!("{:?}", events[1]);
    };
    assert_eq!(kept_report, &report_display);
    let LogEvent::ToolCallRequested {
        display: kept_show_page,
        ..
    } = &events[4]
    else {
        panic!("{:?}", events[4]);
    };
    assert_eq!(kept_show_page, &show_page_display);

    let log_only = Redactions::log_only(["CANARYlogonly-secret".to_owned()]);
    let code = "echo CANARYlogonly-secret".to_owned();
    let refused_code = redaction::scrub(
        &log_only,
        LogEvent::ToolCallRequested {
            turn: 1,
            step: 1,
            block: 5,
            call_id: "c_refused_code".into(),
            tool: "show_code".into(),
            input: json!({"code": code}),
            display: ToolDisplay {
                action: ToolAction::Show,
                verbs: None,
                target: "Code".into(),
                output: OutputKind::None,
                view: Some(ToolView::Code {
                    language: Some("bash".into()),
                    code: code.clone(),
                }),
                title: None,
                purpose: ToolPurpose::Show,
            },
            refused: true,
        },
        Some(home),
    );
    let LogEvent::ToolCallRequested { display, .. } = refused_code else {
        panic!("{refused_code:?}");
    };
    assert_eq!(
        display.view,
        Some(ToolView::Code {
            language: Some("bash".into()),
            code: "echo [redacted]".into(),
        })
    );
}
