use jiff::Timestamp;
use jiff::tz::{Offset, TimeZone};
use serde_json::{Map, Value};
use wisp_tool_text::agents::ROOT_NAME;
use wisp_tool_text::output::{count, cut_chars, more_lines};

use crate::Agent;
use crate::input::{READ, WRITE};
use crate::model::{Board, Card, Edit, LogLine, ROOT};
use crate::rights::find;

pub const NO_TREE: &str = "the board needs this session's agent tree";

const NOTE_CHARS: usize = 80;
const NAME_CHARS: usize = 40;

pub(crate) fn named(path: &str) -> &str {
    if path == ROOT { ROOT_NAME } else { path }
}

pub(crate) fn no_board(given: &str) -> String {
    format!("there is no board {given}")
}

pub(crate) fn no_card(given: &str, id: &str) -> String {
    format!("board {given} has no card {id}")
}

pub(crate) fn not_in_tree(given: &str) -> String {
    format!("{} isn't an agent of this tree", named(given))
}

pub(crate) fn busy(name: &str) -> String {
    format!("board {name} is busy; try again")
}

pub(crate) fn unreadable(name: &str, kind: std::io::ErrorKind) -> String {
    format!("board {name} couldn't be read: {kind}")
}

pub(crate) fn unwritable(name: &str, kind: std::io::ErrorKind) -> String {
    format!("board {name} couldn't be written: {kind}")
}

pub(crate) fn unlisted(kind: std::io::ErrorKind) -> String {
    format!("the boards of this tree couldn't be read: {kind}")
}

pub(crate) fn too_big(name: &str) -> String {
    format!("board {name} would pass 256 KiB; removing cards makes room")
}

pub(crate) fn deleted_meanwhile(name: &str) -> String {
    format!("board {name} was deleted while this write ran; nothing was written")
}

pub(crate) fn stamp(now: Timestamp, offset: impl Fn(Timestamp) -> i32) -> String {
    let offset = Offset::from_seconds(offset(now)).unwrap_or(Offset::UTC);
    now.to_zoned(TimeZone::fixed(offset))
        .strftime("%Y-%m-%d %H:%M:%S%:z")
        .to_string()
}

fn short(at: &str) -> &str {
    at.get(5..16).unwrap_or(at)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Counts {
    pub(crate) created: bool,
    pub(crate) added: u64,
    pub(crate) updated: u64,
    pub(crate) removed: u64,
    pub(crate) logs: u64,
    pub(crate) columns: bool,
    pub(crate) edit: Option<Edit>,
}

impl Counts {
    pub(crate) fn line(&self, name: &str) -> String {
        let mut parts = Vec::new();
        if self.created {
            parts.push("created".to_owned());
        }
        for (n, unit) in [
            (self.added, "card added"),
            (self.updated, "card updated"),
            (self.removed, "card removed"),
        ] {
            if n > 0 {
                let (noun, verb) = unit.split_once(' ').unwrap_or((unit, ""));
                parts.push(format!("{} {verb}", count(n, noun)));
            }
        }
        if self.logs > 0 {
            parts.push(count(self.logs, "log line"));
        }
        if self.columns {
            parts.push("columns set".to_owned());
        }
        if let Some(edit) = self.edit {
            parts.push(format!("edit {}", edit.word()));
        }
        if parts.is_empty() {
            parts.push("no change".to_owned());
        }
        format!("board {name}: {}", parts.join(", "))
    }
}

fn who(path: &str, session: Option<&str>, marks: Option<&[Agent]>) -> String {
    let found = marks.and_then(|agents| find(agents, path, session));
    let shown = found.map_or(path, |agent| agent.path.as_str());
    let mark = found.is_some_and(|agent| agent.ended);
    format!("{}{}", named(shown), if mark { " (ended)" } else { "" })
}

fn log_line(line: &LogLine) -> String {
    format!(
        "{} · {} · {} · {}",
        short(&line.at),
        line.column,
        named(&line.author),
        line.text
    )
}

fn note_line(note: &str) -> Option<String> {
    let lines: Vec<&str> = note.trim_end().lines().collect();
    let at = lines.iter().position(|line| !line.trim().is_empty())?;
    let first = cut_chars(lines[at].trim(), NOTE_CHARS);
    Some(match lines.len() - at - 1 {
        0 => first,
        more => format!("{first} {}", more_lines(more)),
    })
}

pub(crate) fn head(name: &str, tree: Option<&str>) -> String {
    match tree {
        Some(tree) => format!("board {name} of tree {tree}"),
        None => format!("board {name}"),
    }
}

fn owned(head: &str, board: &Board, marks: Option<&[Agent]>) -> String {
    format!(
        "{head} · owner {} · edit {}",
        who(&board.owner, board.owner_session.as_deref(), marks),
        board.edit.word()
    )
}

pub(crate) fn board_view(head: &str, board: &Board, marks: Option<&[Agent]>) -> String {
    let mut lines = vec![owned(head, board, marks)];
    for column in &board.columns {
        let cards: Vec<&Card> = board.cards.iter().filter(|c| &c.column == column).collect();
        lines.push(format!("{column} ({})", cards.len()));
        for card in cards {
            let fields = [
                Some(card.id.clone()),
                Some(card.title.clone()),
                card.assignee
                    .as_deref()
                    .map(|a| who(a, card.assignee_session.as_deref(), marks)),
                card.note.as_deref().and_then(note_line),
            ];
            let fields: Vec<String> = fields.into_iter().flatten().collect();
            lines.push(format!("  {}", fields.join(" · ")));
            if let Some(newest) = card.log.last() {
                lines.push(format!("    {}", log_line(newest)));
            }
        }
    }
    lines.join("\n") + "\n"
}

pub(crate) fn card_view(head: &str, card: &Card, marks: Option<&[Agent]>) -> String {
    let mut lines = vec![
        format!("{head} · card {}", card.id),
        format!("title: {}", card.title),
        format!("column: {}", card.column),
    ];
    if let Some(assignee) = &card.assignee {
        lines.push(format!(
            "assignee: {}",
            who(assignee, card.assignee_session.as_deref(), marks)
        ));
    }
    if let Some(note) = card.note.as_deref().filter(|n| !n.trim().is_empty()) {
        lines.push("note:".to_owned());
        lines.extend(
            note.trim_end()
                .lines()
                .map(|l| format!("  {l}").trim_end().to_owned()),
        );
    }
    if !card.log.is_empty() {
        lines.push("log:".to_owned());
        lines.extend(card.log.iter().map(|l| format!("  {}", log_line(l))));
    }
    lines.join("\n") + "\n"
}

pub(crate) fn list(root: &str, boards: &[(String, Board)], agents: &[Agent]) -> String {
    if boards.is_empty() {
        return format!("no boards in tree {root} yet\n");
    }
    let mut lines = vec![format!("boards of tree {root}")];
    for (name, board) in boards {
        lines.push(format!(
            "{} · {} · updated {}",
            owned(name, board, Some(agents)),
            count(board.cards.len() as u64, "card"),
            short(&board.updated)
        ));
    }
    lines.join("\n") + "\n"
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    pub write: bool,
    pub verbs: Option<[&'static str; 3]>,
    pub target: String,
}

fn cut(text: &str) -> String {
    cut_chars(text, NAME_CHARS)
}

fn board_target(given: &str) -> String {
    match given.split_once('/') {
        Some((tree, name)) => format!("board {} of tree {}", cut(name), cut(tree)),
        None => format!("board {}", cut(given)),
    }
}

pub(crate) fn alone(map: &Map<String, Value>, key: &str) -> bool {
    map.contains_key(key) && map.len() == 2
}

pub fn shown(tool: &str, map: &Map<String, Value>) -> Option<Shown> {
    let board = match map.get("board") {
        Some(Value::String(given)) => board_target(given),
        None if tool == READ => "boards".to_owned(),
        _ => return None,
    };
    let text = |key: &str| map.get(key).and_then(Value::as_str);
    let of = |card: &str| format!("card {} of {board}", cut(card));
    let plain = |target: String, write: bool| Shown {
        write,
        verbs: None,
        target,
    };
    Some(match tool {
        READ => plain(
            text("card")
                .filter(|_| map.contains_key("board"))
                .map_or_else(|| board.clone(), of),
            false,
        ),
        WRITE if alone(map, "delete") && map.get("delete") == Some(&Value::Bool(true)) => Shown {
            write: true,
            verbs: Some(["Deleting", "Deleted", "couldn't delete"]),
            target: board,
        },
        WRITE if alone(map, "owner") && text("owner").is_some() => Shown {
            write: true,
            verbs: Some(["Handing", "Handed", "couldn't hand over"]),
            target: format!(
                "{board} to {}",
                cut(named(text("owner").unwrap_or_default()))
            ),
        },
        WRITE => {
            let one = match map.get("cards") {
                Some(Value::Array(cards)) if cards.len() == 1 && alone(map, "cards") => {
                    cards[0].get("id").and_then(Value::as_str)
                }
                _ => None,
            };
            plain(one.map_or_else(|| board.clone(), of), true)
        }
        _ => return None,
    })
}
