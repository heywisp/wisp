pub const COMMAND: &str = "rename";
pub const DONE: &str = "Renamed this chat";
pub const NO_NAME: &str = "type a name after it";
pub const NOT_MAIN: &str = "only the main agent can rename this chat";
pub const NOT_SAVED: &str = "this chat isn't saved, so it has no name";
pub const EMPTY: &str = "the name is empty";
pub const OUTSIDE: &str = "a chat is renamed only from a tool call or a command";

pub fn renamed(name: &str) -> String {
    format!("{DONE} to {name}")
}

pub fn refused(reason: &str) -> String {
    format!("/{COMMAND}: {reason}")
}
