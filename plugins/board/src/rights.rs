use crate::input::Write;
use crate::model::{Board, Edit, ROOT};
use crate::words::named;
use crate::{Agent, Me};

pub(crate) fn find<'a>(
    agents: &'a [Agent],
    path: &str,
    session: Option<&str>,
) -> Option<&'a Agent> {
    agents.iter().find(|agent| match session {
        Some(session) => agent.session == session,
        None => agent.path == path,
    })
}

fn is_me(me: &Me, path: &str, session: Option<&str>) -> bool {
    match session {
        Some(session) => me.session == session,
        None => me.path == path,
    }
}

fn owner<'a>(board: &Board, agents: &'a [Agent]) -> Option<&'a Agent> {
    find(agents, &board.owner, board.owner_session.as_deref())
}

fn parent(path: &str) -> Option<&str> {
    match path.rsplit_once('/') {
        Some(("", "")) | None => None,
        Some(("", _)) => Some(ROOT),
        Some((parent, _)) => Some(parent),
    }
}

fn handler<'a>(agents: &'a [Agent], owner: &str) -> Option<&'a Agent> {
    let mut at = parent(owner);
    while let Some(path) = at {
        let found = find(agents, path, None);
        if found.is_some_and(|agent| !agent.ended) {
            return found;
        }
        at = parent(path);
    }
    find(agents, ROOT, None)
}

fn mine(board: &Board, me: &Me) -> bool {
    is_me(me, &board.owner, board.owner_session.as_deref())
}

fn may_hand_over(board: &Board, me: &Me, agents: &[Agent]) -> bool {
    mine(board, me)
        || owner(board, agents)
            .filter(|owner| owner.ended)
            .and_then(|owner| handler(agents, &owner.path))
            .is_some_and(|handler| handler.session == me.session)
}

fn suffix(board: &Board, agents: &[Agent]) -> String {
    match owner(board, agents) {
        Some(owner) if owner.ended => format!(
            "; {} has ended, and {} can hand the board over",
            named(&owner.path),
            named(handler(agents, &owner.path).map_or(ROOT, |h| h.path.as_str()))
        ),
        _ => String::new(),
    }
}

pub(crate) fn check(
    board: &Board,
    name: &str,
    me: &Me,
    agents: &[Agent],
    write: &Write,
) -> Result<(), String> {
    if mine(board, me) {
        return Ok(());
    }
    let owner = named(owner(board, agents).map_or(&board.owner, |owner| &owner.path));
    let suffix = suffix(board, agents);
    if write.edit.is_some() || write.delete || write.owner.is_some() {
        if write.owner.is_some() && may_hand_over(board, me, agents) {
            return Ok(());
        }
        return Err(format!(
            "only board {name}'s owner, {owner}, may change its edit setting, delete it or hand it over{suffix}"
        ));
    }
    if board.edit == Edit::Tree {
        return Ok(());
    }
    let mine = |id: &str| {
        board.card(id).is_some_and(|card| {
            card.assignee
                .as_deref()
                .is_some_and(|path| is_me(me, path, card.assignee_session.as_deref()))
        })
    };
    let allowed = write.columns.is_none()
        && write.remove.is_empty()
        && write
            .cards
            .iter()
            .all(|card| mine(&card.id) && card.title.is_none() && card.assignee.is_none());
    if allowed {
        return Ok(());
    }
    Err(match write.cards.iter().find(|card| mine(&card.id)) {
        Some(card) => format!(
            "you may change card {}'s column, note and log; only {owner} may change the rest{suffix}",
            card.id
        ),
        None => format!(
            "board {name} belongs to {owner}; only it and each card's assignee may change it{suffix}"
        ),
    })
}
