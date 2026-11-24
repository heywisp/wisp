pub const RESUME_TITLE: &str = "Resume";
pub const RESUME_SUMMARY: &str = "Pick a chat here to continue";
pub const RESUME_BETWEEN_TURNS: &str = "/resume runs between turns.";
macro_rules! needs_log {
    ($flag:literal) => {
        concat!($flag, " needs the session log, and logging is off")
    };
}
pub const RESUME_NEEDS_LOG: &str = needs_log!("--resume");
pub const SESSION_ID_NEEDS_LOG: &str = needs_log!("--session-id");
pub const NOT_SAVED: &str = "This chat isn't saved.";
pub const KEPT_OPEN: &str = "kept open";
pub const OPEN_ELSEWHERE: &str = "open elsewhere";
pub const CONTINUE_THERE: &str = "Continue it in the other window.";
pub const RESUME_WORD: &str = "resume";
pub const QUIT_WORD: &str = "quit";
pub const CLOSE_WORD: &str = "close";
pub const CLEAR_WORD: &str = "clear";

pub fn no_other_chat(here: &str) -> String {
    format!("No other chat to resume in {here}.")
}

pub const CHAT_TAKEN: &str = "Opened in another window.";
pub const BRING_CHAT_BACK: &str = "Bring it back here with:";
pub const WINDOW_RECONNECTING: &str =
    "Reconnecting to the chat. Press ctrl-c to close this window.";
pub const WINDOW_REOPENED: &str = "Reconnected to the chat.";
pub const CHAT_LOST: &str = "The chat ended unexpectedly.";
pub const OPEN_CHAT_AGAIN: &str = "Open it again with:";

pub const IDLE_WOKE: &str = "no agent was working";
pub const WISP_RESTARTED: &str = "Wisp restarted";
pub const ABOUT_TO_RESTART: &str = "About to restart";
pub const AFTER_RESTART: &str = "after restart";
pub const NOT_SENT: &str = "Not sent:";

pub fn to_agent(agent: &str) -> String {
    format!("To {agent}:")
}

pub const WINDOW_RESTARTING: &str = "Restarting the chat. Press ctrl-c to stop waiting.";
pub const WINDOW_RESTARTED: &str = "Restarted on the newest version.";
pub const RESTART_QUESTION: &str =
    "The open question was stopped; if its agent doesn't ask again, message it your answer.";
pub const RESTART_CANCELLED: &str = "The restart was cancelled; this chat carries on.";
pub const WINDOW_UNSENT: &str = "This window still runs the older one, to keep what you typed. To update it once that is sent: /leave, then wisp\u{a0}--resume";
pub const CHAT_OLDER: &str = "This chat runs an older version.";
pub const RESTART_KEPT: &str = "Restart the chats kept open on the newest version";

pub fn restart_hint() -> String {
    format!("{RESTART_KEPT}: wisp\u{a0}restart")
}

pub fn window_restarted(unsent: bool, question: bool) -> String {
    let mut note = WINDOW_RESTARTED.to_owned();
    if unsent {
        note.push(' ');
        note.push_str(WINDOW_UNSENT);
    }
    if question {
        note.push(' ');
        note.push_str(RESTART_QUESTION);
    }
    note
}
