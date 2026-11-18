use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader};
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};
use std::time::SystemTime;

use jiff::Timestamp;
use serde::Deserialize;
use wisp_protocol::ChatRow;
use wisp_protocol::log::{LogEvent, LogRecord};

use super::{lookup_records, turns_seen};
use crate::endpoints::goes::shown;
use crate::runtime::in_parallel;
use crate::session::{JsonlStore, Line, SessionStore, Stamp, newer, read_lines_where, short_id};

pub(super) const LISTED: [&str; 13] = [
    "session_started",
    "session_resumed",
    "settings_changed",
    "agent_spawned",
    "agent_ended",
    "agent_renamed",
    "turn_started",
    "input_accepted",
    "turn_ended",
    "rewound",
    "context_compacted",
    "resume_fell_back",
    "session_named",
];

type Seen = BTreeMap<PathBuf, (Stamp, Listed)>;
type Dated = (SystemTime, String, u64, Option<Stamp>);

static SEEN: Mutex<Seen> = Mutex::new(BTreeMap::new());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wanted<'a> {
    Latest { here: &'a str },
    Id(&'a str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub id: String,
    pub skipped: Vec<String>,
    pub unreadable: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub id: String,
    pub modified: SystemTime,
    pub name: Option<String>,
    pub topic: String,
    pub typed: String,
    pub account: Option<String>,
    pub model: Option<String>,
    pub agents: u32,
    pub open_elsewhere: bool,
    pub hosted: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    pub chats: Vec<Listed>,
    pub unreadable: Vec<(String, String)>,
}

impl Listing {
    pub fn rows(
        &self,
        now: Timestamp,
        known: impl Fn(&str) -> bool,
        mark: impl Fn(&str) -> Option<u8>,
    ) -> Vec<ChatRow> {
        self.chats
            .iter()
            .map(|chat| {
                let account = chat.account.as_deref().filter(|name| known(name));
                ChatRow {
                    id: chat.id.clone(),
                    name: chat.name.clone(),
                    topic: chat.topic.clone(),
                    typed: chat.typed.clone(),
                    active_at: Timestamp::try_from(chat.modified)
                        .map_or_else(|_| String::new(), |at| short_ago(at, now)),
                    account: account.map(|name| shown(name).to_owned()),
                    mark: account.and_then(&mark),
                    model: chat.model.clone(),
                    agents: chat.agents,
                    hosted: chat.hosted,
                    open_elsewhere: chat.open_elsewhere,
                }
            })
            .collect()
    }
}

const KEY_LENGTH: usize = 32;

fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

fn masked(word: &str) -> Option<String> {
    let mut prefixes = wisp_protocol::KEY_PREFIXES.iter().chain(&["sk-"]);
    if let Some(prefix) = prefixes.find(|p| word.starts_with(**p)) {
        return Some(format!("{prefix}\u{2026}"));
    }
    let long = word.chars().count() >= KEY_LENGTH
        && word.chars().any(char::is_alphabetic)
        && word.chars().any(|c| c.is_ascii_digit());
    long.then(|| "\u{2026}".to_owned())
}

pub fn topic_mask(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(word_char) {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let end = rest.find(|c| !word_char(c)).unwrap_or(rest.len());
        let word = &rest[..end];
        out.push_str(masked(word).as_deref().unwrap_or(word));
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

pub(crate) fn typed_turn(event: &LogEvent) -> Option<(u64, &str)> {
    match event {
        LogEvent::TurnStarted {
            turn,
            text,
            origin: None,
            ..
        } if !text.is_empty() => Some((*turn, text)),
        LogEvent::TurnStarted {
            turn,
            inputs,
            origin: None,
            ..
        } => match inputs.as_slice() {
            [one] if one.note.is_some() && !one.text.is_empty() => Some((*turn, &one.text)),
            _ => None,
        },
        _ => None,
    }
}

fn typed(event: &LogEvent) -> Option<&str> {
    match event {
        LogEvent::InputAccepted {
            text, from: None, ..
        } if !text.is_empty() => Some(text),
        _ => typed_turn(event).map(|(_, text)| text),
    }
}

pub(crate) fn first_line(text: &str) -> Option<String> {
    let line = text.lines().find(|line| !line.trim().is_empty())?;
    Some(line.split_whitespace().collect::<Vec<_>>().join(" "))
}

fn topic(records: &[LogRecord]) -> String {
    records
        .iter()
        .filter_map(|record| typed(&record.event))
        .find_map(first_line)
        .map(|line| topic_mask(&line))
        .unwrap_or_default()
}

pub(super) fn listed(
    id: String,
    modified: SystemTime,
    records: &[LogRecord],
    open_elsewhere: bool,
) -> Listed {
    let mut account = None;
    let mut model = None;
    let mut agents: Vec<wisp_protocol::AgentPath> = Vec::new();
    let mut texts: Vec<&str> = Vec::new();
    let mut name = None;
    for record in records {
        if let Some(text) = typed(&record.event)
            && texts.last() != Some(&text)
        {
            texts.push(text);
        }
        match &record.event {
            LogEvent::SessionStarted {
                endpoint_name,
                model: logged,
                ..
            } => {
                account = endpoint_name.clone().or(account);
                model = Some(logged.clone());
            }
            LogEvent::SessionResumed { endpoint_name, .. } => {
                account = Some(endpoint_name.clone());
            }
            LogEvent::SettingsChanged {
                endpoint_name,
                model: logged,
                ..
            } => {
                account = endpoint_name.clone().or(account);
                model = Some(logged.clone());
            }
            LogEvent::AgentSpawned { agent, .. } => agents.push(agent.clone()),
            LogEvent::AgentEnded { agent, .. } => agents.retain(|a| a != agent),
            LogEvent::AgentRenamed { agent, to, .. } => {
                for moved in &mut agents {
                    *moved = std::mem::take(moved).renamed(agent, to);
                }
            }
            LogEvent::SessionNamed {
                name: Some(named), ..
            } => name = Some(named.clone()),
            _ => {}
        }
    }
    Listed {
        name,
        topic: topic(records),
        typed: texts.join("\n"),
        agents: u32::try_from(agents.len()).unwrap_or(u32::MAX),
        id,
        modified,
        account,
        model,
        open_elsewhere,
        hosted: false,
    }
}

pub fn short_ago(at: Timestamp, now: Timestamp) -> String {
    let minutes = now.duration_since(at).as_secs() / 60;
    match minutes {
        i64::MIN..=0 => "just now".to_owned(),
        1..=59 => format!("{minutes}m ago"),
        60..=1439 => format!("{}h ago", minutes / 60),
        _ => format!("{}d ago", minutes / 1440),
    }
}

pub fn ago(at: &str, now: Timestamp) -> String {
    let Ok(then) = at.parse::<Timestamp>() else {
        return "at an unknown time".to_owned();
    };
    let minutes = now.duration_since(then).as_secs() / 60;
    match minutes {
        i64::MIN..=0 => "just now".to_owned(),
        1..=59 => format!("{minutes} min ago"),
        60..=2879 => format!("{} h ago", minutes / 60),
        _ => format!("{} days ago", minutes / 1440),
    }
}

fn by_id(store: &JsonlStore, prefix: &str) -> Result<Found, String> {
    let wanted = prefix.to_ascii_lowercase();
    if !wisp_protocol::valid_id(&wanted) {
        return Err(format!("{prefix} is not a session ID"));
    }
    let ids = store.list().map_err(|e| e.to_string())?;
    let matching: Vec<&String> = ids.iter().filter(|id| id.starts_with(&wanted)).collect();
    match matching.as_slice() {
        [] => Err(format!("no session's ID starts with {prefix}")),
        [one] => match root_of(store, one) {
            Some(root) => Err(format!(
                "session {} is an agent in session {}'s tree; resume the tree with wisp --resume {}",
                short_id(one),
                short_id(&root),
                short_id(&root)
            )),
            None => Ok(Found {
                id: (*one).clone(),
                skipped: Vec::new(),
                unreadable: Vec::new(),
            }),
        },
        many => {
            let shown: Vec<&str> = many.iter().map(|id| short_id(id)).collect();
            Err(format!(
                "{prefix} matches {} sessions ({}); give more of the ID",
                many.len(),
                shown.join(", ")
            ))
        }
    }
}

#[derive(Deserialize)]
pub(super) struct Head {
    schema_version: u64,
    seq: u64,
    #[serde(rename = "type")]
    kind: String,
    parent: Option<HeadParent>,
    #[serde(default)]
    headless: bool,
    cwd: Option<String>,
}

#[derive(Deserialize)]
struct HeadParent {
    session_id: String,
}

pub(super) fn head(store: &JsonlStore, id: &str) -> Option<Head> {
    let mut line = Vec::new();
    BufReader::new(File::open(store.path(id)).ok()?)
        .read_until(b'\n', &mut line)
        .ok()?;
    let head: Head = serde_json::from_slice(&line).ok()?;
    (head.seq == 0 && !newer(head.schema_version) && head.kind == "session_started").then_some(head)
}

fn parent_of(store: &JsonlStore, id: &str) -> Option<String> {
    head(store, id)?.parent.map(|parent| parent.session_id)
}

fn root_of(store: &JsonlStore, id: &str) -> Option<String> {
    let mut root = parent_of(store, id)?;
    let mut seen = vec![id.to_owned()];
    while let Some(up) = parent_of(store, &root) {
        if seen.contains(&up) {
            break;
        }
        seen.push(std::mem::replace(&mut root, up));
    }
    Some(root)
}

fn read_lines_of(store: &JsonlStore, id: &str) -> Option<Vec<Line>> {
    let bytes = fs::read(store.path(id)).ok()?;
    let read = read_lines_where(&bytes, |kind| LISTED.contains(&kind));
    (read.newer == 0).then_some(read.lines)
}

fn directory(store: &JsonlStore, id: &str, first: Option<&Head>) -> Option<String> {
    if let Some(first) = first {
        return first.cwd.clone();
    }
    let lines = read_lines_of(store, id)?;
    lines.iter().rev().find_map(|line| match line {
        Line::Record(record) | Line::Misplaced(record) => match &record.event {
            LogEvent::SessionResumed { cwd, .. } | LogEvent::SessionStarted { cwd, .. } => {
                cwd.clone()
            }
            _ => None,
        },
        _ => None,
    })
}

fn newest_first(store: &JsonlStore) -> io::Result<Vec<Dated>> {
    let mut dated: Vec<_> = store
        .list()?
        .into_iter()
        .map(|id| {
            let (modified, len, stamp) = fs::metadata(store.path(&id)).map_or(
                (SystemTime::UNIX_EPOCH, 0, None),
                |metadata| {
                    (
                        metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                        metadata.len(),
                        Stamp::of(&metadata),
                    )
                },
            );
            (modified, id, len, stamp)
        })
        .collect();
    dated.sort_by(|a, b| (&b.0, &b.1, b.2).cmp(&(&a.0, &a.1, a.2)));
    Ok(dated)
}

fn chat(
    store: &JsonlStore,
    here: Option<&str>,
    scripted: bool,
    seen: &Seen,
    (modified, id, _, stamp): &Dated,
    needs_turn: bool,
) -> Option<Result<Listed, (String, String)>> {
    let first = head(store, id);
    let skipped = first
        .as_ref()
        .is_some_and(|h| h.parent.is_some() || (h.headless && !scripted));
    if skipped
        || here.is_some_and(|here| directory(store, id, first.as_ref()).as_deref() != Some(here))
    {
        return None;
    }
    let open_elsewhere = match store.probe(id) {
        Ok(_) => false,
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => true,
        Err(e) => return Some(Err((short_id(id).to_owned(), e.to_string()))),
    };
    let hosted = open_elsewhere
        && store
            .dir()
            .parent()
            .is_some_and(|data| wisp_protocol::link::socket(data, id).exists());
    if let Some((_, known)) = seen
        .get(&store.path(id))
        .filter(|(known, _)| Some(*known) == *stamp)
    {
        return Some(Ok(Listed {
            hosted,
            open_elsewhere,
            ..known.clone()
        }));
    }
    let lines = read_lines_of(store, id)?;
    if needs_turn && turns_seen(&lines) == 0 {
        return None;
    }
    let records = match lookup_records(lines) {
        Ok(records) => records,
        Err(e) => return Some(Err((short_id(id).to_owned(), e))),
    };
    Some(Ok(Listed {
        hosted,
        ..listed(id.clone(), *modified, &records, open_elsewhere)
    }))
}

fn chats<'a>(
    store: &'a JsonlStore,
    here: &'a str,
    scripted: bool,
) -> io::Result<impl Iterator<Item = Result<Listed, (String, String)>> + 'a> {
    Ok(newest_first(store)?
        .into_iter()
        .filter_map(move |entry| chat(store, Some(here), scripted, &Seen::new(), &entry, true)))
}

pub fn newest(store: &JsonlStore) -> io::Result<Option<Listed>> {
    Ok(newest_first(store)?
        .iter()
        .find_map(|entry| chat(store, None, true, &Seen::new(), entry, true)?.ok()))
}

pub fn list(store: &JsonlStore, here: &str, leave_out: Option<&str>) -> io::Result<Listing> {
    let mut entries = newest_first(store)?;
    entries.retain(|(_, id, _, _)| Some(id.as_str()) != leave_out);
    let mut seen = SEEN.lock().unwrap_or_else(PoisonError::into_inner);
    let memo: &Seen = &seen;
    let found = in_parallel(&entries, |entry| {
        Some((entry.3, chat(store, Some(here), false, memo, entry, true)?))
    });
    let mut listing = Listing::default();
    for (stamp, chat) in found.into_iter().flatten() {
        match chat {
            Ok(chat) => {
                if let Some(stamp) = stamp {
                    seen.insert(store.path(&chat.id), (stamp, chat.clone()));
                }
                listing.chats.push(chat);
            }
            Err(unreadable) => listing.unreadable.push(unreadable),
        }
    }
    Ok(listing)
}

pub fn rows_of(store: &JsonlStore, ids: &[String]) -> Vec<ChatRow> {
    let chats = in_parallel(ids, |id| {
        if !wisp_protocol::valid_id(id) {
            return None;
        }
        let modified = fs::metadata(store.path(id)).ok()?.modified().ok()?;
        chat(
            store,
            None,
            true,
            &Seen::new(),
            &(modified, id.clone(), 0, None),
            false,
        )?
        .ok()
    })
    .into_iter()
    .flatten()
    .collect();
    Listing {
        chats,
        unreadable: Vec::new(),
    }
    .rows(Timestamp::now(), |_| false, |_| None)
}

pub fn chat_rows(
    dir: PathBuf,
    here: &str,
    leave_out: Option<&str>,
    now: Timestamp,
    known: impl Fn(&str) -> bool,
    mark: impl Fn(&str) -> Option<u8>,
) -> io::Result<Vec<ChatRow>> {
    let store = JsonlStore::open(dir)?;
    Ok(list(&store, here, leave_out)?.rows(now, known, mark))
}

fn latest(store: &JsonlStore, here: &str) -> Result<Found, String> {
    let mut skipped = Vec::new();
    let mut unreadable = Vec::new();
    for chat in chats(store, here, true).map_err(|e| e.to_string())? {
        match chat {
            Ok(chat) if chat.open_elsewhere => skipped.push(short_id(&chat.id).to_owned()),
            Ok(chat) => {
                return Ok(Found {
                    id: chat.id,
                    skipped,
                    unreadable,
                });
            }
            Err(e) => unreadable.push(e),
        }
    }
    let mut why = match skipped.as_slice() {
        [] => Vec::new(),
        [one] => vec![format!("session {one} is open in another Wisp")],
        many => vec![format!(
            "sessions {} are open in another Wisp",
            many.join(", ")
        )],
    };
    why.extend(
        unreadable
            .iter()
            .map(|(id, e)| format!("session {id} can't be opened ({e})")),
    );
    match why.as_slice() {
        [] => Err(format!("no session to resume in {here}")),
        reasons => Err(format!(
            "no session to resume in {here}: {}",
            reasons.join("; ")
        )),
    }
}

pub fn find(store: &JsonlStore, wanted: Wanted<'_>) -> Result<Found, String> {
    match wanted {
        Wanted::Id(prefix) => by_id(store, prefix),
        Wanted::Latest { here } => latest(store, here),
    }
}
