use std::collections::BTreeSet;

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::model::Edit;
use crate::words::alone;

pub const READ: &str = "board_read";
pub const WRITE: &str = "board_write";

const MAX_COLUMNS: usize = 12;
const MAX_TITLE: usize = 200;
const MAX_LOG: usize = 500;
const MAX_NOTE: usize = 2_000;
const SESSION_ID: usize = 36;

const BOARD_RULE: &str =
    "a board's name is 1 to 40 of a-z, 0-9 and -, starting with a letter or digit";
const TREE_RULE: &str = "a tree's id is a full session id, 36 of 0-9, a-f and -";
const COLUMN_RULE: &str = "a column's name is 1 to 24 of a-z, 0-9 and -";
const COLUMNS_RULE: &str = "a board has 1 to 12 columns";
const ID_RULE: &str = "a card's id is 1 to 32 of A-Z, a-z, 0-9, ., _ and -";
const CARD_NEEDS_BOARD: &str = "a card is read on its board, so name the board too";
const ALONE: &str = "delete and owner each go alone in a write";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Name {
    pub tree: Option<String>,
    pub name: String,
}

impl Name {
    pub fn given(&self) -> String {
        match &self.tree {
            Some(tree) => format!("{tree}/{}", self.name),
            None => self.name.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CardInput {
    pub id: String,
    pub title: Option<String>,
    pub column: Option<String>,
    pub assignee: Option<String>,
    pub note: Option<String>,
    pub log: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Write {
    pub board: String,
    pub columns: Option<Vec<String>>,
    pub edit: Option<Edit>,
    #[serde(default)]
    pub cards: Vec<CardInput>,
    #[serde(default)]
    pub remove: Vec<String>,
    #[serde(default)]
    pub delete: bool,
    pub owner: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadInput {
    board: Option<String>,
    card: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Read {
        board: Option<Name>,
        card: Option<String>,
    },
    Write(Write),
}

pub(crate) fn object(input: &str) -> Result<Map<String, Value>, String> {
    match serde_json::from_str(input) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err("input must be an object".to_owned()),
        Err(e) => Err(format!("input is not JSON: {e}")),
    }
}

fn typed<T: for<'de> Deserialize<'de>>(map: Map<String, Value>) -> Result<T, String> {
    serde_json::from_value(Value::Object(map)).map_err(|e| e.to_string())
}

fn of(text: &str, min: usize, max: usize, allowed: impl Fn(char) -> bool) -> bool {
    (min..=max).contains(&text.chars().count()) && text.chars().all(allowed)
}

fn lower(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'
}

pub(crate) fn board_name(text: &str) -> bool {
    of(text, 1, 40, lower) && !text.starts_with('-')
}

fn column(text: &str) -> bool {
    of(text, 1, 24, lower)
}

fn card_id(text: &str) -> bool {
    of(text, 1, 32, |c| {
        c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')
    })
}

fn session_id(text: &str) -> bool {
    of(text, SESSION_ID, SESSION_ID, |c| {
        c.is_ascii_digit() || matches!(c, 'a'..='f' | '-')
    })
}

fn one_line(text: &str, max: usize) -> bool {
    !text.contains(['\n', '\r']) && text.chars().count() <= max
}

pub(crate) fn qualified(text: &str) -> Option<Name> {
    let (tree, name) = text.split_once('/')?;
    (session_id(tree) && board_name(name)).then(|| Name {
        tree: Some(tree.to_owned()),
        name: name.to_owned(),
    })
}

fn name(text: &str) -> Result<Name, String> {
    if board_name(text) {
        return Ok(Name {
            tree: None,
            name: text.to_owned(),
        });
    }
    match text.split_once('/') {
        Some((tree, _)) if !session_id(tree) => Err(TREE_RULE.to_owned()),
        Some(_) => qualified(text).ok_or_else(|| BOARD_RULE.to_owned()),
        None => Err(BOARD_RULE.to_owned()),
    }
}

fn read(map: Map<String, Value>) -> Result<Request, String> {
    let input: ReadInput = typed(map)?;
    let board = input.board.as_deref().map(name).transpose()?;
    if let Some(id) = &input.card {
        if board.is_none() {
            return Err(CARD_NEEDS_BOARD.to_owned());
        }
        if !card_id(id) {
            return Err(ID_RULE.to_owned());
        }
    }
    Ok(Request::Read {
        board,
        card: input.card,
    })
}

fn check_card(card: &CardInput) -> Result<(), String> {
    let id = &card.id;
    if !card_id(id) {
        return Err(ID_RULE.to_owned());
    }
    if card
        .title
        .as_deref()
        .is_some_and(|t| !one_line(t, MAX_TITLE))
    {
        return Err(format!(
            "card {id}'s title is one line of at most 200 characters"
        ));
    }
    if card.log.as_deref().is_some_and(|t| !one_line(t, MAX_LOG)) {
        return Err(format!(
            "card {id}'s log line is one line of at most 500 characters"
        ));
    }
    if card
        .note
        .as_deref()
        .is_some_and(|t| t.chars().count() > MAX_NOTE)
    {
        return Err(format!("card {id}'s note is at most 2,000 characters"));
    }
    if card.column.as_deref().is_some_and(|c| !column(c)) {
        return Err(format!("card {id}'s column is 1 to 24 of a-z, 0-9 and -"));
    }
    Ok(())
}

fn write(map: Map<String, Value>) -> Result<Request, String> {
    let crowded = ["delete", "owner"]
        .iter()
        .any(|key| map.contains_key(*key) && !alone(&map, key));
    let write: Write = typed(map)?;
    if let Some(name) = qualified(&write.board) {
        return Err(format!(
            "board {} belongs to another tree; you can read it with board_read",
            name.given()
        ));
    }
    if !board_name(&write.board) {
        return Err(BOARD_RULE.to_owned());
    }
    if crowded {
        return Err(ALONE.to_owned());
    }
    if let Some(columns) = &write.columns {
        if !(1..=MAX_COLUMNS).contains(&columns.len()) {
            return Err(COLUMNS_RULE.to_owned());
        }
        let mut seen = BTreeSet::new();
        for name in columns {
            if !column(name) {
                return Err(COLUMN_RULE.to_owned());
            }
            if !seen.insert(name) {
                return Err(format!("column {name} is named twice in this write"));
            }
        }
    }
    let mut seen = BTreeSet::new();
    for card in &write.cards {
        check_card(card)?;
        if !seen.insert(card.id.as_str()) {
            return Err(format!("card {} is named twice in this write", card.id));
        }
    }
    for id in &write.remove {
        if !card_id(id) {
            return Err(ID_RULE.to_owned());
        }
        if !seen.insert(id.as_str()) {
            return Err(format!("card {id} is named twice in this write"));
        }
    }
    Ok(Request::Write(write))
}

pub(crate) fn request(tool: &str, map: Map<String, Value>) -> Result<Request, String> {
    match tool {
        READ => read(map),
        WRITE => write(map),
        other => Err(format!("the board plugin has no tool `{other}`")),
    }
}
