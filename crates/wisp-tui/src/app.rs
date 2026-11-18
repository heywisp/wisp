pub mod agents;
pub mod chats;
pub mod commands;
pub mod config_screen;
pub mod display;
pub mod endpoint_card;
pub mod find;
#[cfg(test)]
mod fixture;
pub mod frames;
pub mod help;
pub mod highlight;
pub mod inputs;
pub mod keymap;
pub mod list;
pub mod logout;
mod mail;
pub mod opener;
pub mod pager;
pub mod picker;
mod presenter;
pub mod processes;
pub mod question;
pub mod replay;
mod reset_time;
pub mod resumed;
pub mod retry;
pub mod rewind;
pub mod selection;
pub mod settings;
pub mod setup;
pub mod sidebar;
pub mod start;
pub mod subscription_login;
pub mod suggest;
pub mod tool_files;
mod usage_spend;
pub mod view;
pub mod welcome;

use tokio::sync::mpsc;
use wisp_protocol::{AgentEvent, ClientCommand};

pub use presenter::{
    AGAIN_TO_QUIT, AGENT_STOPPED, AppError, DRAG_CAPTURED, DRAG_NATIVE, Exit, Finished,
    GNU_SCREEN_KEEPS, MAX_RESTORES, NEW_KEY, NEWLINE_KEYS, NEWLINE_KEYS_KITTY, NOTE_HANG,
    NOTHING_TO_COPY, Options, Rows, SELECT_KEY, SELECT_KEY_REMOTE, SLICE, TERMINAL_PASTE_WINDOW,
    TMUX_MOUSE_OFF, TOAST, Unsent, VS_CODE_WHEEL, WHEEL_ROWS, copies_hint, handed_off_line,
    host_errors, new_hint, note_rows, recovered, rows_for, run, run_to_end,
};
pub use wisp_render::chrome::{Credential, Facts as SessionFacts};

pub struct Attached {
    pub first_request: wisp_protocol::RequestId,
    pub now: Option<wisp_protocol::Now>,
    pub end: mpsc::Sender<bool>,
    pub ended: tokio::sync::watch::Receiver<Option<wisp_client::link::LinkEnd>>,
    pub error_file: std::path::PathBuf,
}

pub struct Session {
    pub leave_note: Option<String>,
    pub attached: Option<Attached>,
    pub alerts: Vec<String>,
    pub facts: SessionFacts,
    pub events: mpsc::Receiver<AgentEvent>,
    pub commands: mpsc::Sender<ClientCommand>,
    pub resumed: Option<resumed::Resumed>,
}
