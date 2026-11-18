use std::io;
use std::path::PathBuf;

use wisp_core::session::JsonlStore;
use wisp_protocol::ChatRow;
use wisp_protocol::settings::{PLUGIN_SLOT, names};

pub(crate) const NEWER_OPEN: &str = "The newest chat here is open in another window.";

pub(crate) const KEPT_SETTINGS: [&str; 25] = [
    names::DISPLAY_AMBIGUOUS,
    names::DISPLAY_COLOR,
    names::DISPLAY_COPY_ON_SELECT,
    names::DISPLAY_FULL_REPAINT,
    names::DISPLAY_MOTION,
    names::DISPLAY_MOUSE,
    names::DISPLAY_PICTURES,
    names::DISPLAY_SCROLL_SPEED,
    names::DISPLAY_SIDEBAR,
    names::DISPLAY_SIDEBAR_FOLDED,
    names::DISPLAY_SIDEBAR_PINNED,
    names::DISPLAY_THEME,
    names::DISPLAY_VERBOSE,
    PLUGIN_SLOT,
    names::KEYCHAIN,
    names::AUTH,
    names::KEY,
    names::KEY_ENV,
    names::KEY_STORE,
    names::PROVIDER,
    names::STORE_ID,
    names::URL,
    names::URL_ENV,
    names::MAX_IN_FLIGHT,
    names::PRICING,
];

pub(crate) fn kept_sets(sets: &[String]) -> Vec<String> {
    sets.iter()
        .filter(|set| {
            set.split_once('=')
                .and_then(|(name, _)| crate::settings::target(name).ok())
                .is_some_and(|target| KEPT_SETTINGS.contains(&target.spec().name))
        })
        .cloned()
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Choice {
    Open { id: String, note: Option<String> },
    Pick { chats: Vec<ChatRow> },
    Refuse(String),
}

pub(crate) fn choose(chats: Vec<ChatRow>, here: &str) -> Choice {
    if chats.iter().any(|chat| chat.hosted) {
        return Choice::Pick { chats };
    }
    let mut openable = chats.iter().filter(|chat| chat.can_open());
    match (openable.next(), openable.next()) {
        (Some(only), None) => Choice::Open {
            id: only.id.clone(),
            note: chats
                .first()
                .filter(|newest| newest.open_elsewhere)
                .map(|_| format!("wisp: {NEWER_OPEN}")),
        },
        (Some(_), Some(_)) => Choice::Pick { chats },
        (None, _) if chats.is_empty() => Choice::Refuse(format!(
            "wisp: nothing to resume in {here}; run wisp to start a new chat"
        )),
        (None, _) => Choice::Refuse(format!(
            "wisp: every chat in {here} is open in another window; continue there or run wisp to start a new chat"
        )),
    }
}

pub(crate) fn taken(dir: Option<PathBuf>, id: &str) -> bool {
    dir.and_then(|dir| JsonlStore::open(dir).ok())
        .is_some_and(|store| {
            store
                .probe(id)
                .is_err_and(|e| e.kind() == io::ErrorKind::WouldBlock)
        })
}

#[cfg(test)]
mod tests;
