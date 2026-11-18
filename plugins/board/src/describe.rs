use serde_json::{Value, json};

use crate::input::{READ, WRITE};

const READ_DESCRIPTION: &str = "Read your agent tree's boards. A board is a kanban that agents share: columns in order, and cards, each in one column, with a title, an assignee, a note and a log. With no board, list the tree's boards; with a board, show its columns and cards, each card's note by its first line and its newest log line; with a card too, show that card's whole note and log. Another tree's board is read by its qualified name, the tree's id and the board's name joined by a slash, as the first line of that tree's list gives it.";

const WRITE_DESCRIPTION: &str = "Change a board of your agent tree. A write to a name that has no board creates it, owned by you, with the given columns (todo, doing and done when left out) and edit (owner when left out); such a write titles each of its cards. Cards are matched by id: a new id is added in the first column unless column names one, and a given field replaces the old one. With edit owner, only the owner changes the board, except that a card's assignee may change that card's column, note and log; with edit tree, any agent of the tree may change it. Only the owner changes edit, deletes the board or hands it over; once the owner has ended, its parent, or the nearest running agent above it, may hand it over. A write is all or nothing. delete and owner each go alone in a write.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Def {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: String,
}

fn text(description: &str) -> Value {
    json!({"type": "string", "description": description})
}

fn read_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "board": text("The board's name, or another tree's board as its qualified name. Leave it out to list your tree's boards."),
            "card": text("A card's id, to read that card of the board.")
        },
        "additionalProperties": false
    })
}

fn write_schema() -> Value {
    let card = json!({
        "type": "object",
        "properties": {
            "id": text("The card's id: 1 to 32 of A-Z, a-z, 0-9, ., _ and -."),
            "title": text("One line of at most 200 characters."),
            "column": text("One of the board's columns."),
            "assignee": text("The agent the card is for: main, or an agent's path as list_agents shows it. An empty string clears it."),
            "note": text("Text of at most 2,000 characters, which may hold lines, such as the steps of the card's work."),
            "log": text("A line of at most 500 characters to add to the card's log, stamped with the time, the card's column after this write and you.")
        },
        "required": ["id"],
        "additionalProperties": false
    });
    json!({
        "type": "object",
        "properties": {
            "board": text("The board's name: 1 to 40 of a-z, 0-9 and -, starting with a letter or digit."),
            "columns": {
                "type": "array",
                "items": {"type": "string"},
                "description": "The board's columns in order, replacing the list: 1 to 12 names of 1 to 24 of a-z, 0-9 and -. A column that holds cards after the write can't be left out."
            },
            "edit": {
                "type": "string",
                "enum": ["owner", "tree"],
                "description": "Who may change the board: owner, or tree for every agent of the tree."
            },
            "cards": {
                "type": "array",
                "items": card,
                "description": "Cards to add or change."
            },
            "remove": {
                "type": "array",
                "items": {"type": "string"},
                "description": "Ids of cards to remove."
            },
            "delete": {
                "type": "boolean",
                "description": "true deletes the board."
            },
            "owner": text("The running agent of the tree to hand the board to: main, or an agent's path as list_agents shows it.")
        },
        "required": ["board"],
        "additionalProperties": false
    })
}

pub fn definitions() -> Vec<Def> {
    vec![
        Def {
            name: READ,
            description: READ_DESCRIPTION,
            schema: read_schema().to_string(),
        },
        Def {
            name: WRITE,
            description: WRITE_DESCRIPTION,
            schema: write_schema().to_string(),
        },
    ]
}
