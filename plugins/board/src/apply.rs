use crate::input::{CardInput, Write};
use crate::model::{Board, Card, DEFAULT_COLUMNS, Edit, LogLine, ROOT};
use crate::rights::{check, find};
use crate::words::{self, Counts, named};
use crate::{Agent, Me, Refusal};

pub(crate) struct Applied {
    pub(crate) board: Board,
    pub(crate) line: String,
    pub(crate) changed: bool,
}

fn rule(sentence: String) -> Refusal {
    Refusal::Rule(sentence)
}

fn root(given: &str) -> bool {
    given == ROOT || given == wisp_tool_text::agents::ROOT_NAME
}

fn member<'a>(agents: &'a [Agent], given: &str) -> Result<Option<&'a Agent>, Refusal> {
    if given.is_empty() {
        return Ok(None);
    }
    let path = if root(given) { ROOT } else { given };
    match find(agents, path, None) {
        Some(agent) => Ok(Some(agent)),
        None => Err(rule(words::not_in_tree(given))),
    }
}

fn receiver<'a>(agents: &'a [Agent], given: &str, name: &str) -> Result<&'a Agent, Refusal> {
    match member(agents, given)? {
        Some(agent) if agent.ended => Err(rule(format!(
            "{} has ended; hand board {name} to a running agent",
            named(&agent.path)
        ))),
        Some(agent) => Ok(agent),
        None => Err(rule(words::not_in_tree(given))),
    }
}

fn keyed(agents: &[Agent], path: &mut String, session: &mut Option<String>) {
    if let Some(agent) = find(agents, path, session.as_deref()) {
        path.clone_from(&agent.path);
        *session = Some(agent.session.clone());
    }
}

fn created(write: &Write, me: &Me, at: &str, next: u64) -> Result<Board, Refusal> {
    if write.delete || write.owner.is_some() || !write.remove.is_empty() {
        return Err(rule(words::no_board(&write.board)));
    }
    if write.cards.iter().any(|card| card.title.is_none()) {
        return Err(rule(format!(
            "there is no board {}, and a write that creates one titles each card",
            write.board
        )));
    }
    Ok(Board {
        owner: me.path.clone(),
        owner_session: Some(me.session.clone()),
        edit: write.edit.unwrap_or(Edit::Owner),
        columns: write
            .columns
            .clone()
            .unwrap_or_else(|| DEFAULT_COLUMNS.iter().map(|c| (*c).to_owned()).collect()),
        cards: Vec::new(),
        updated: at.to_owned(),
        created: next,
        deleted: false,
    })
}

fn change(
    board: &mut Board,
    input: &CardInput,
    agents: &[Agent],
    counts: &mut Counts,
) -> Result<(), Refusal> {
    let assignee = input
        .assignee
        .as_deref()
        .map(|given| member(agents, given))
        .transpose()?;
    let at = match board.cards.iter().position(|card| card.id == input.id) {
        Some(at) => {
            let fields = [&input.title, &input.column, &input.assignee, &input.note];
            if fields.iter().any(|field| field.is_some()) {
                counts.updated += 1;
            }
            at
        }
        None => {
            counts.added += 1;
            board.cards.push(Card {
                id: input.id.clone(),
                title: input.id.clone(),
                column: board.columns.first().cloned().unwrap_or_default(),
                assignee: None,
                assignee_session: None,
                note: None,
                log: Vec::new(),
            });
            board.cards.len() - 1
        }
    };
    let card = &mut board.cards[at];
    if let Some(title) = &input.title {
        card.title.clone_from(title);
    }
    if let Some(column) = &input.column {
        card.column.clone_from(column);
    }
    if let Some(assignee) = assignee {
        card.assignee = assignee.map(|agent| agent.path.clone());
        card.assignee_session = assignee.map(|agent| agent.session.clone());
    }
    if let Some(note) = &input.note {
        card.note = Some(note.clone());
    }
    Ok(())
}

fn log(board: &mut Board, write: &Write, me: &Me, at: &str, counts: &mut Counts) {
    for input in &write.cards {
        let Some(text) = &input.log else { continue };
        if let Some(card) = board.cards.iter_mut().find(|card| card.id == input.id) {
            card.log.push(LogLine {
                at: at.to_owned(),
                column: card.column.clone(),
                author: me.path.clone(),
                text: text.clone(),
            });
            counts.logs += 1;
        }
    }
}

fn placed(board: &Board, write: &Write) -> Result<(), Refusal> {
    for card in &board.cards {
        if board.columns.contains(&card.column) {
            continue;
        }
        let moved = write
            .cards
            .iter()
            .any(|input| input.id == card.id && input.column.is_some());
        let held = board
            .cards
            .iter()
            .filter(|other| other.column == card.column)
            .count();
        return Err(rule(if moved {
            format!(
                "board {} has no column {}; its columns are {}",
                write.board,
                card.column,
                board.columns.join(", ")
            )
        } else {
            format!(
                "column {} holds {}; move or remove its cards first",
                card.column,
                wisp_tool_text::output::count(held as u64, "card")
            )
        }));
    }
    Ok(())
}

pub(crate) fn apply(
    read: Option<&Board>,
    me: &Me,
    agents: &[Agent],
    write: &Write,
    at: &str,
    next: u64,
) -> Result<Applied, Refusal> {
    let name = &write.board;
    let mut counts = Counts {
        created: read.is_none(),
        ..Counts::default()
    };
    let mut board = match read {
        None => created(write, me, at, next)?,
        Some(board) => {
            check(board, name, me, agents, write).map_err(rule)?;
            let mut board = board.clone();
            keyed(agents, &mut board.owner, &mut board.owner_session);
            for card in &mut board.cards {
                if let Some(path) = &mut card.assignee {
                    keyed(agents, path, &mut card.assignee_session);
                }
            }
            board
        }
    };
    board.updated = at.to_owned();
    if write.delete {
        board.deleted = true;
        return Ok(Applied {
            board,
            line: format!("board {name} deleted"),
            changed: true,
        });
    }
    if let Some(given) = &write.owner {
        let agent = receiver(agents, given, name)?;
        board.owner.clone_from(&agent.path);
        board.owner_session = Some(agent.session.clone());
        let line = format!("board {name} now belongs to {}", named(&board.owner));
        return Ok(Applied {
            board,
            line,
            changed: true,
        });
    }
    if let Some(columns) = &write.columns {
        board.columns.clone_from(columns);
        counts.columns = true;
    }
    counts.edit = write.edit;
    if let Some(edit) = write.edit {
        board.edit = edit;
    }
    for input in &write.cards {
        change(&mut board, input, agents, &mut counts)?;
    }
    for id in &write.remove {
        let Some(at) = board.cards.iter().position(|card| &card.id == id) else {
            return Err(rule(words::no_card(name, id)));
        };
        board.cards.remove(at);
        counts.removed += 1;
    }
    placed(&board, write)?;
    log(&mut board, write, me, at, &mut counts);
    let line = counts.line(name);
    Ok(Applied {
        board,
        changed: counts != Counts::default(),
        line,
    })
}
