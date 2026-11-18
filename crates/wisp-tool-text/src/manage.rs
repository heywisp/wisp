use std::sync::LazyLock;

pub use crate::question::TIME_LIMIT_MS;

pub const TURN_ON: &str = "Turn on";
pub const TURN_OFF: &str = "Turn off";
pub const ADD: &str = "Add";
pub const REMOVE: &str = "Remove";

pub const HEADER: &str = "Needs you";
pub const APPROVE: &str = "Approve";
pub const NOT_NOW: &str = "Not now";
pub const FOR_THIS_CHAT: &str = "For this chat. Your settings stay as they are.";

pub const CHOSE_NOT_NOW: &str = "nothing changed: the user chose Not now";
pub const NO_ANSWER: &str = "nothing changed: the user didn't answer";

pub const NOT_MAIN: &str = "only the main chat turns tools and skills on or off; this agent's tools were set when it started";
pub const ADD_TOOL: &str = "a tool is added with its plugin: run wisp plugin add MANIFEST WASM";

pub const TURN_ON_WHERE: &str = "turn on: /config, Turned off";

pub fn ask_on(name: &str) -> String {
    format!("Turn on {name} in this chat? It is off in your settings.")
}

pub fn ask_off(name: &str, finds: bool) -> String {
    let then = if finds {
        "can't find tools, skills or servers"
    } else {
        "can't turn tools, skills or servers on or off"
    };
    format!("Turn off {name} in this chat? The model then {then}.")
}

pub fn now_off(name: &str) -> String {
    format!("{name} is off in this chat from the next message")
}

pub fn now_on(name: &str) -> String {
    format!("{name} is on in this chat from the next message")
}

pub fn own_words(words: &str) -> String {
    format!(
        "nothing changed: the user answered in their own words instead of choosing: {words}. You may ask again"
    )
}

pub fn stays(name: &str) -> String {
    format!("{name} stays on; turn it off in /config, Turned off")
}

pub fn native(name: &str) -> String {
    format!(
        "{name} is the provider's own tool, and switching it rebuilds the cache; switch it in /config, Tools"
    )
}

pub fn nothing_named(kind: &str, name: &str) -> String {
    format!("there is no {kind} named `{name}`")
}

pub fn off_in_list(name: &str) -> String {
    format!("{name} is off in your settings; turn it on in /config, Turned off")
}

pub fn off_in_tools(name: &str) -> String {
    format!("{name} is off in your settings; turn it on in /config, Tools")
}

pub fn already(name: &str, on: bool) -> String {
    format!("{name} is already {}", if on { "on" } else { "off" })
}

pub fn would_rebuild(name: &str) -> String {
    format!("{name} can't come on in this chat without rebuilding the cache; turn it on in /config")
}

pub fn turned_off(name: &str) -> String {
    format!("{name} is in Turned off; take it out in /config to use it")
}

pub fn kind_name(kind: wisp_plugin_api::manage::Kind) -> &'static str {
    match kind {
        wisp_plugin_api::manage::Kind::Tool => "tool",
        wisp_plugin_api::manage::Kind::Skill => "skill",
        wisp_plugin_api::manage::Kind::Server => "server",
    }
}

pub const CHILD_ADDED: &str = "agents started from now have it; this one doesn't";
pub const ADD_SERVER_APPROVAL: &str =
    "Starts it now and saves it in your settings, for every chat.";
pub const REMOVE_SERVER_APPROVAL: &str = "Removes it from your settings, for every chat.";
pub const KEY_NAME: &str = "a key can't be used as a name";
pub const REMOVE_TOOL: &str = "a tool is removed with its plugin: run wisp plugin remove ID";

pub fn no_shell(action: &str, name: &str) -> String {
    let from = if action == "add" { " and from" } else { "" };
    format!(
        "this agent has no shell, so it can't {action} a server; ask your parent to {action} {name} with manage (action {action}, kind server, the same name{from})"
    )
}

pub fn ask_add(name: &str, from: &str) -> String {
    format!(
        "Add the server {name}? This chat has no shell. It runs: {}",
        crate::flat(from)
    )
}

pub fn ask_remove(name: &str) -> String {
    format!("Remove the server {name}? This chat has no shell.")
}

pub fn child_changed(path: &str, name: &str, added: bool) -> String {
    format!(
        "{path} {} server {name}",
        if added { "added" } else { "removed" }
    )
}

pub const LINK_UNREADABLE: &str =
    "You can't read the sign-in link: it is in no log, file or tool result, so don't look for it.";

pub fn sign_in_note(name: &str, host: &str) -> String {
    format!("sign in to {name} at {host}")
}

pub fn added_waiting(name: &str) -> String {
    format!(
        "{name} added; it needs the user to sign in. Wisp shows the sign-in link in the chat and tries to open it in the user's browser. {LINK_UNREADABLE} To have Wisp try again, call manage with action add, kind server and name {name}; the user can also type /mcp sign-in {name}. Ask the user to press Allow on that page, then end your reply: Wisp sends you a notice once they have allowed or declined the sign-in. If the notice says {name} signed in, search for {name}'s tools; otherwise tell the user what it says and try again only if they ask."
    )
}

pub fn waiting_unshown(name: &str) -> String {
    format!(
        "{name} added; it needs the user to sign in, and this run has no window to show the sign-in link in. {LINK_UNREADABLE} Tell the user to type /mcp sign-in {name} in a full-screen Wisp chat."
    )
}

pub fn child_waiting(name: &str) -> String {
    format!(
        "{name} needs a sign-in, which the main chat shows at its next message; agents started after it have the server. {LINK_UNREADABLE}"
    )
}

pub fn not_waiting(name: &str) -> String {
    format!("{name} isn't waiting for a sign-in")
}

pub static KEY_PREFIXES: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    include_str!("../../wisp-protocol/data/key-prefixes.txt")
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .collect()
});

pub fn looks_like_key(name: &str) -> bool {
    name.chars().count() >= 20
        && (KEY_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
            || (name.chars().any(|c| c.is_ascii_lowercase())
                && name.chars().any(|c| c.is_ascii_digit())
                && !name.contains('_')))
}

pub fn plain_variable(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !looks_like_key(name)
        && !looks_like_key(&name.replace('_', ""))
}
