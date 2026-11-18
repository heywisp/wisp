use std::io::Write;

use super::*;
use crate::resume::lookup::{LISTED, head, listed};
use crate::session::{ResumeLock, jsonl_line, known_type, read_lines_where};
use crate::test_guard::{fixture, protocol_fixtures};

const C: &str = "0003cccc-0000-7000-8000-000000000000";
const D: &str = "0004dddd-0000-7000-8000-000000000000";
const E: &str = "0005eeee-0000-7000-8000-000000000000";
const F: &str = "0006ffff-0000-7000-8000-000000000000";

fn typed(turn: u64, id: &str, text: &str) -> Value {
    json!({"type": "input_accepted", "turn": turn, "id": id, "mode": "queue", "text": text})
}

fn mailed(turn: u64, text: &str) -> Value {
    json!({
        "type": "input_accepted", "turn": turn, "id": "m1", "mode": "queue", "text": text,
        "from": {"agent": "/coder", "kind": "report", "mail": "1"},
    })
}

fn spawned(agent: &str) -> Value {
    json!({
        "type": "agent_spawned", "turn": 1, "agent": agent, "session_id": "s", "depth": 1,
        "model": "m", "effort": "high", "config": {},
    })
}

fn one_turn(n: u64, extra: Vec<Value>) -> Vec<Value> {
    let mut events =
        vec![json!({"type": "turn_started", "turn": n, "origin": "mail", "text": "mail"})];
    events.extend(extra);
    events.extend(full_turn(n, 0.1).into_iter().skip(1));
    events
}

fn worked() -> Vec<Value> {
    let mut events = vec![started_in(A, "~/wisp", &[], None)];
    events.extend(one_turn(
        1,
        vec![
            mailed(1, "a report from an agent"),
            typed(1, "u1", "\n  Fix the   cache\tmiss  \nand more below"),
            spawned("/coder"),
            spawned("/reviewer"),
            json!({"type": "agent_renamed", "turn": 1, "agent": "/reviewer", "to": "/critic"}),
            json!({"type": "agent_ended", "turn": 1, "agent": "/critic", "spent_usd": 0.5}),
        ],
    ));
    events.push(change(
        "claude-sonnet-5-5",
        "high",
        "claude-api",
        Vec::new(),
    ));
    let later = typed(2, "u2", "a later message");
    events.extend(one_turn(2, vec![later.clone(), later, typed(2, "u3", "")]));
    events.push(json!({"type": "session_named", "name": "Fix sk-ant-api03-\u{2026}"}));
    events.push(json!({"type": "session_named", "detail": "overloaded"}));
    events
}

#[test]
fn the_list_holds_this_directorys_root_chats_with_a_turn_newest_first() {
    let dir = temp_dir("list");
    let store = JsonlStore::open(&dir).unwrap();
    write_session(&store, A, &worked(), 90);
    session_here(&store, B, "~/wisp", None, 1, 5);
    session_here(&store, C, "~/elsewhere", None, 1, 1);
    session_here(&store, D, "~/wisp", None, 1, 2);
    make_child(&store, D, A, "/coder");
    session_here(&store, E, "~/wisp", None, 0, 3);
    let target = dir.join("elsewhere.jsonl");
    fs::write(&target, lines(&[started_in(F, "~/wisp", &[], None)])).unwrap();
    std::os::unix::fs::symlink(&target, store.path(F)).unwrap();
    let mut other = JsonlStore::open(&dir).unwrap();
    other.reopen(B, ResumeLock::Required).unwrap();

    let listing = list(&store, "~/wisp", None).unwrap();
    let ids: Vec<&str> = listing.chats.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, [B, A]);
    assert_eq!(listing.unreadable.len(), 1);
    assert_eq!(listing.unreadable[0].0, "0006ffff-0000");
    assert!(listing.chats[0].open_elsewhere);
    assert_eq!(listing.chats[0].topic, "message 1");
    let chat = &listing.chats[1];
    assert!(!chat.open_elsewhere);
    assert_eq!(chat.topic, "Fix the cache miss");
    assert_eq!(
        chat.typed,
        "\n  Fix the   cache\tmiss  \nand more below\na later message"
    );
    assert_eq!(chat.account.as_deref(), Some("claude-api"));
    assert_eq!(chat.model.as_deref(), Some("claude-sonnet-5-5"));
    assert_eq!(chat.agents, 1);

    let found = find(&store, Wanted::Latest { here: "~/wisp" }).unwrap();
    assert_eq!(found.id, A);
    assert_eq!(found.skipped, ["0002bbbb-0000"]);
    assert_eq!(found.unreadable, listing.unreadable);

    let now = jiff::Timestamp::now();
    let rows = listing.rows(
        now,
        |name| name == "claude-api",
        |name| (name == "claude-api").then_some(4),
    );
    assert_eq!(rows[1].active_at, "1h ago");
    assert_eq!(rows[1].name.as_deref(), Some("Fix sk-ant-api03-\u{2026}"));
    assert_eq!(rows[1].topic, "Fix the cache miss");
    assert_eq!(rows[0].name, None);
    assert_eq!(rows[1].mark, Some(4));
    assert_eq!(rows[0].account, None);
    assert!(rows[0].open_elsewhere);
    assert_eq!(rows[0].mark, None);
    let left = list(&store, "~/wisp", Some(B)).unwrap();
    let ids: Vec<&str> = left.chats.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, [A]);
    drop(other);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn an_unchanged_log_is_read_once_and_a_changed_one_again() {
    let dir = temp_dir("list-seen");
    let store = JsonlStore::open(&dir).unwrap();
    session_here(&store, A, "~/wisp", None, 1, 30);
    let listed = |store: &JsonlStore| list(store, "~/wisp", None).unwrap().chats;
    assert_eq!(listed(&store)[0].topic, "message 1");

    let path = store.path(A);
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    rewrite_lines(&store, A, |_, line| {
        line.replacen("message 1", "changed 1", 1)
    });
    set_modified(&path, modified);
    let kept = listed(&store);
    assert_eq!(kept[0].topic, "message 1");
    assert!(!kept[0].open_elsewhere);

    let mut other = JsonlStore::open(&dir).unwrap();
    other.reopen(A, ResumeLock::Required).unwrap();
    let locked = listed(&store);
    assert_eq!(locked[0].topic, "message 1");
    assert!(locked[0].open_elsewhere);
    let later = modified + Duration::from_secs(60);
    set_modified(&path, later);
    assert_eq!(listed(&store)[0].topic, "changed 1");

    let mut events = vec![started_in(A, "~/wisp", &[], None)];
    events.extend(full_turn(1, 0.1));
    events.push(turn(2));
    let grown = lines(&events);
    let last = grown.lines().last().unwrap();
    File::options()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(format!("{last}\n").as_bytes())
        .unwrap();
    set_modified(&path, later);
    let changed = listed(&store);
    assert_eq!(changed[0].typed, "changed 1\nmessage 2");
    assert!(changed[0].open_elsewhere);
    drop(other);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn pre_epoch_chats_keep_their_modified_order_without_being_remembered() {
    let dir = temp_dir("list-pre-epoch");
    let store = JsonlStore::open(&dir).unwrap();
    let newer = SystemTime::UNIX_EPOCH - Duration::from_secs(1);
    for (id, modified) in [(A, newer), (B, newer - Duration::from_secs(1))] {
        session_here(&store, id, "~/wisp", None, 1, 30);
        set_modified(&store.path(id), modified);
        assert_eq!(store.stamp(id), None);
    }
    let listed = list(&store, "~/wisp", None).unwrap().chats;
    assert_eq!(
        listed.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
        [A, B]
    );
    assert_eq!(listed[0].modified, newer);
    assert_eq!(newest(&store).unwrap().unwrap().id, A);
    rewrite_lines(&store, A, |_, line| {
        line.replacen("message 1", "changed 1", 1)
    });
    set_modified(&store.path(A), newer);
    assert_eq!(
        list(&store, "~/wisp", None).unwrap().chats[0].topic,
        "changed 1"
    );
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_scripted_chat_is_left_out_of_the_list_but_still_resumes() {
    let dir = temp_dir("list-headless");
    let store = JsonlStore::open(&dir).unwrap();
    session_here(&store, A, "~/wisp", None, 1, 30);
    let mut scripted = started_in(B, "~/wisp", &[], None);
    scripted["headless"] = json!(true);
    let mut events = vec![scripted];
    events.extend(full_turn(1, 0.1));
    write_session(&store, B, &events, 1);

    let listing = list(&store, "~/wisp", None).unwrap();
    let ids: Vec<&str> = listing.chats.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, [A]);
    assert_eq!(
        find(&store, Wanted::Latest { here: "~/wisp" }).unwrap().id,
        B,
        "a script's resume with no ID keeps today's rule"
    );
    assert_eq!(find(&store, Wanted::Id("0002")).unwrap().id, B);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_newest_lesson_is_the_newest_chat_with_a_turn_from_any_directory() {
    let dir = temp_dir("newest");
    let store = JsonlStore::open(&dir).unwrap();
    assert!(newest(&store).unwrap().is_none());
    session_here(&store, A, "~/wisp", None, 1, 30);
    let mut headless = started_in(B, "~/elsewhere", &[], None);
    headless["headless"] = json!(true);
    let mut events = vec![headless];
    events.extend(full_turn(1, 0.1));
    write_session(&store, B, &events, 5);
    session_here(&store, C, "~/wisp", None, 0, 1);

    let found = newest(&store).unwrap().unwrap();
    assert_eq!(found.id, B);
    assert!(!found.open_elsewhere);
    let mut other = JsonlStore::open(&dir).unwrap();
    other.reopen(B, ResumeLock::Required).unwrap();
    let found = newest(&store).unwrap().unwrap();
    assert_eq!(found.id, B);
    assert!(found.open_elsewhere);
    drop(other);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn many_chats_list_newest_first() {
    let dir = temp_dir("list-many");
    let store = JsonlStore::open(&dir).unwrap();
    let id = |n: u64| format!("{n:08x}-0000-7000-8000-000000000000");
    for n in 1..=40 {
        session_here(&store, &id(n), "~/wisp", None, 1, n * 2);
        session_here(&store, &id(100 + n), "~/wisp", None, 1, n * 2 + 1);
        make_child(&store, &id(100 + n), &id(n), "/coder");
    }

    let listing = list(&store, "~/wisp", None).unwrap();
    let ids: Vec<String> = listing.chats.iter().map(|c| c.id.clone()).collect();
    assert_eq!(ids, (1..=40).map(id).collect::<Vec<_>>());
    assert!(listing.unreadable.is_empty());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn short_ages_read_in_minutes_hours_and_days() {
    let ago = |minutes: i64| {
        short_ago(
            now()
                .checked_sub(jiff::SignedDuration::from_mins(minutes))
                .unwrap(),
            now(),
        )
    };
    assert_eq!(ago(0), "just now");
    assert_eq!(ago(12), "12m ago");
    assert_eq!(ago(125), "2h ago");
    assert_eq!(ago(16 * 60), "16h ago");
    assert_eq!(ago(1440 + 5), "1d ago");
}

#[test]
fn key_shaped_words_in_a_topic_are_masked() {
    let key = format!("sk-ant-api03-{}_Ab-9{}", "x1".repeat(20), "Q".repeat(10));
    assert_eq!(
        topic_mask(&format!("why does {key} fail")),
        "why does sk-ant-api03-\u{2026} fail"
    );
    assert_eq!(
        topic_mask(&format!("ANTHROPIC_API_KEY={key} wisp")),
        "ANTHROPIC_API_KEY=sk-ant-api03-\u{2026} wisp"
    );
    assert_eq!(
        topic_mask(&format!("try `{key}`")),
        "try `sk-ant-api03-\u{2026}`"
    );
    assert_eq!(
        topic_mask("sk-proj-abc and sk-other and sk-learn"),
        "sk-proj-\u{2026} and sk-\u{2026} and sk-\u{2026}"
    );
    let token = "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0";
    assert_eq!(topic_mask(&format!("commit {token}.")), "commit \u{2026}.");
    let short = "abcdefghijklmnopqrstuvwxyzabcdefghij";
    assert_eq!(topic_mask(short), short);
    assert_eq!(
        topic_mask("caf\u{e9} \u{1b}[31m ok"),
        "caf\u{e9} \u{1b}[31m ok"
    );
}

fn written(events: &[Value]) -> String {
    log(events).iter().map(|r| jsonl_line(r).unwrap()).collect()
}

fn read_as_listed(text: &str, keep: impl Fn(&str) -> bool) -> ((Listed, u64, u64), Vec<Line>) {
    let read = read_lines_where(text.as_bytes(), keep);
    let records = lookup_records(read.lines.clone()).unwrap();
    let chat = listed(A.to_owned(), SystemTime::UNIX_EPOCH, &records, false);
    ((chat, turns_seen(&read.lines), read.newer), read.lines)
}

#[test]
fn the_list_parses_only_its_own_kinds_and_lists_every_log_as_a_full_read_would() {
    let mut events = worked();
    let turn_one_end = events
        .iter()
        .position(|e| e["type"] == "settings_changed")
        .unwrap()
        - 1;
    events.push(json!({
        "type": "session_resumed", "wisp_version": "0.1.0",
        "endpoint": "https://api.anthropic.com", "endpoint_name": "claude-work",
    }));
    events.extend(one_turn(3, vec![typed(3, "u3", "undo this")]));
    events.push(json!({
        "type": "rewound", "to": turn_one_end, "point": {"type": "turn", "turn": 2},
        "reason": "user", "messages": 2, "blocks": 1,
        "dropped": {"turns": [2, 3], "messages": 4}, "cache": {"type": "kept"},
    }));
    events.extend(one_turn(4, vec![typed(4, "u4", "after the rewind")]));
    events.push(json!({
        "type": "resume_fell_back", "kept_through": 3, "dropped_through": 5, "last_turn": 1,
        "turns_seen": 4, "processes_seen": 0, "spent_usd": 0.5, "damage": [],
    }));
    let damaged = |text: &str, turn: u64| {
        let start = format!(r#""type":"turn_started","turn":{turn}"#);
        let damaged = text.replacen(&start, &format!("{start}x"), 1);
        assert_ne!(damaged, text);
        damaged
    };
    let rewound_over = damaged(&written(&events), 3);
    events.push(compacted(Some(0.1)));
    let whole = written(&events);
    let version = format!(r#"{{"schema_version":{LOG_SCHEMA_VERSION},"#);
    let newer = format!(r#"{{"schema_version":{},"#, LOG_SCHEMA_VERSION + 1);
    let reply = whole.find(r#""type":"message_committed""#).unwrap();
    let start = whole[..reply].rfind('\n').map_or(0, |end| end + 1);
    let newer_reply = format!(
        "{}{newer}{}",
        &whole[..start],
        &whole[start + version.len()..]
    );
    let mut logs = vec![rewound_over, damaged(&whole, 4), newer_reply, whole];
    for entry in fs::read_dir(protocol_fixtures())
        .unwrap()
        .map(Result::unwrap)
    {
        let name = entry.file_name().into_string().unwrap();
        if !(name.starts_with("log_") && name.ends_with(".json")) {
            continue;
        }
        let text = fixture::stored(&fs::read_to_string(entry.path()).unwrap());
        let mut event: Value = serde_json::from_str(&text).unwrap();
        for key in ["schema_version", "seq", "at"] {
            event.as_object_mut().unwrap().remove(key);
        }
        if event["turn"].is_u64() {
            event["turn"] = json!(1);
        }
        let mut events = vec![started_in(A, "~/wisp", &[], None)];
        events.extend(full_turn(1, 0.25));
        events.push(event);
        logs.push(written(&events));
    }
    let keep = |kind: &str| LISTED.contains(&kind);
    for text in &logs {
        let (full, full_lines) = read_as_listed(text, |_| true);
        let (narrow, narrow_lines) = read_as_listed(text, keep);
        assert!(narrow_lines.contains(&Line::Skipped), "{text}");
        let unreadable: String = text
            .lines()
            .zip(narrow_lines.iter().zip(&full_lines))
            .map(|(line, reads)| match (reads, line.strip_suffix('}')) {
                ((Line::Skipped, read), Some(body)) if *read != Line::Skipped => {
                    format!("{body},\n")
                }
                _ => format!("{line}\n"),
            })
            .collect();
        assert_eq!(narrow, full, "{text}");
        assert_eq!(read_as_listed(&unreadable, keep).0, full, "{text}");
    }
    assert!(LISTED.iter().all(|kind| known_type(kind)));
}

#[test]
fn the_head_is_read_only_from_a_session_started_this_wisp_reads() {
    let dir = temp_dir("list-head");
    let store = JsonlStore::open(&dir).unwrap();
    let first = written(&[started_in(A, "~/wisp", &[], None)]);
    let version = format!(r#""schema_version":{LOG_SCHEMA_VERSION}"#);
    let newer = format!(r#""schema_version":{}"#, LOG_SCHEMA_VERSION + 1);
    for (line, read) in [
        (first.clone(), true),
        (first.replacen(r#""seq":0"#, r#""seq":1"#, 1), false),
        (first.replacen(&version, &newer, 1), false),
        (written(&[turn(1)]), false),
    ] {
        fs::write(store.path(A), &line).unwrap();
        assert_eq!(head(&store, A).is_some(), read, "{line}");
    }
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn rows_of_kept_open_ids_use_the_lists_names_even_before_a_turn() {
    let dir = temp_dir("rows-of");
    let store = JsonlStore::open(dir.join("sessions")).unwrap();
    write_session(&store, A, &worked(), 90);
    session_here(&store, B, "~/elsewhere", None, 1, 5);
    session_here(&store, C, "~/wisp", None, 0, 1);
    let mut other = JsonlStore::open(dir.join("sessions")).unwrap();
    other.reopen(A, ResumeLock::Required).unwrap();
    fs::create_dir_all(dir.join("run")).unwrap();
    fs::write(wisp_protocol::link::socket(&dir, A), []).unwrap();
    let rows = rows_of(
        &store,
        &[B.into(), C.into(), A.into(), "../missing".into(), D.into()],
    );
    assert_eq!(
        rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        [B, C, A]
    );
    assert_eq!(rows[0].topic, "message 1");
    assert!(!rows[0].open_elsewhere);
    assert!(rows[2].hosted);
    assert!(rows[2].open_elsewhere);
    assert_eq!(rows[1].topic, "");
    assert_eq!(rows[1].name, None);
    let listed =
        list(&store, "~/wisp", None)
            .unwrap()
            .rows(jiff::Timestamp::now(), |_| false, |_| None);
    assert_eq!(rows[2].name, listed[0].name);
    assert_eq!(rows[2].topic, listed[0].topic);
    drop(other);
    fs::remove_dir_all(dir).unwrap();
}
