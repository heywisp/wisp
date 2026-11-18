pub mod account;
pub mod accounts;
mod agents;
pub(crate) mod band;
mod banner;
mod caret;
mod chats;
mod composer;
pub mod config_screen;
pub mod context;
pub mod endpoint_card;
mod facts;
mod help;
mod menu;
mod processes;
mod question;
pub mod restart;
mod rewind;
mod row;
mod search;
mod settings;
pub mod sidebar;
mod status;
mod summary;
pub mod topology;
mod waiting;
mod welcome;

pub(crate) use agents::THIS_CHAT;
pub use agents::{
    AGENTS_LIST_GAP, AGENTS_MIN_ROWS, AccountBilling, AgentRow, AgentsTree, AgentsView, MainRow,
    PAGE_CHROME, agent_label, agent_row, agents_next_change, agents_note, agents_page, agents_tree,
    beside, chat_header, clocks_next_change, count_ladder, list_width, panel_rows, releasable,
    strip_line, timed,
};
pub use band::{WIDE_FROM, band_cols, selected_line, selected_look, selected_row, selection_mark};
pub use banner::{WORDMARK, banner, banner_next_change, banner_with, logo};
pub use caret::place_caret;
pub use chats::{ChatsPlace, ChatsView, Joins, chats_fit, chats_lines, chats_whole, title_of};
#[cfg(any(test, feature = "testing"))]
pub use chats::{mockup_chat, mockup_chats, mockup_older_chats};
pub use composer::{
    BOX_FROM_COLUMNS, BOX_FROM_ROWS, COMPOSER_MAX_ROWS, ComposerLook, ComposerShape, Edge,
    FRAMED_STATUS_FROM_ROWS, STATUS_FROM_ROWS, TOP_ROW_FROM_ROWS, border, composer_edge,
    composer_lead_width, composer_placeholder, composer_spans, fits_box, frame_inner, frame_row,
    placeholder_for,
};
pub use facts::{
    AccountFacts, Credential, CredentialLabel, Endpoint, EndpointFacts, Facts, HOST_MIN, ImageTier,
    InstructionsFacts, LongPromptRate, ModelCaps, ModelFacts, ModelRates, NextTurn, RowFact,
    StartPick, Tally, WriteMultipliers, chat_of, endpoint_facts_of, named_model, offers,
    providers_off, short_model,
};
pub use help::{
    HELP_GAP, HELP_INDENT, HELP_KEY_MAX, HELP_STACKED_BELOW, HELP_STACKED_HANG, HelpPage, HelpRow,
    HelpSection, help_page,
};
pub use menu::{
    Choice, DOCK_KEEPS, DOCK_MIN_ROWS, Dock, Head, MENU_MAX_ROWS, Menu, SUGGEST_ARGS_ABOVE,
    SUGGEST_MAX_ROWS, SUGGEST_SUMMARIES_FROM, Shape, SuggestRows, Suggestion, Suggestions, Tone,
    Under, dock_room, docked_lines, docked_rows, key_row, menu_lead, menu_lines, menu_rows,
    suggestion_rows,
};
pub use processes::{
    PS_MIN_ROWS, ProcessRow, PsView, SCREEN_CHROME_ROWS, ScreenMode, ScreenView, StepInView,
    ps_next_change, ps_rows, screen_inner, screen_rows, step_in_rows,
};
pub use question::{
    CardHit, CardMode, CardPage, CardRows, CardView, Draft, FormAccount, FormField, FormValue,
    FormView, HEADER_MAX, LinkCells, LinkRow, OWN_MAX_ROWS, QUESTION_MIN_ROWS, SECRET_PLACEHOLDER,
    SecretGoes, SecretView, answer_options, answer_words, cut_header, form_rows, question_rows,
    secret_rows,
};
pub use rewind::{
    REWIND_LEAD, RewindView, rewind_dock, rewind_loading, rewind_menu, rewind_notes, rewind_panel,
};
pub use row::{
    GROUP_GAP, KEY_ROW, KeyHint, KeyLook, ORB_GAP, STOP_ONLY_BELOW, clip, edge, status_row,
};
pub use search::{
    SearchForms, SearchPair, changes_search, search_banner, search_change, search_row,
};
pub use settings::{
    Alive, CacheSize, Change, Effect, Rebuild, Scope, SwitchCause, SwitchWork, Told, Wins, approx,
    change_note, model_name, saved_parts, setting_label, setting_note, when_words,
};
pub use status::{
    FACTS_FROM, SCHEDULE_HORIZON, SHIMMER_PERIOD, SHIMMER_STEP, SHIMMER_SWEEP, State, Status,
    VERB_FROM, shimmer, shimmers,
};
pub use summary::{
    Away, ChatTurn, Summary, add_kept, down_arrow, home_short, session_total, shown_total,
    up_arrow, up_down,
};
pub use waiting::{WAITING_MAX_ROWS, WaitLabel, WaitingRow, waiting_rows};
pub use welcome::{
    ArtKind, BOX_FROM, Frame, OPENING, Opening, PLAIN, ROUNDED, SPLASH_ASCII, SPLASH_FROM,
    SplashCells, WELCOME_MAX, WELCOME_SETTLED, WelcomeArt, detail_lines, frame_for, key_line,
    opening, opening_art, opening_lap, opening_next_change, splash, splash_idle, welcome,
    welcome_art, welcome_in_detail, welcome_next_change, words_fade,
};

use crate::styled::StyledSpan;
use crate::text::sanitize;
use crate::theme::Token;
use wisp_protocol::{AgentEvent, Ends, TurnOutcome};

pub fn one_line(text: &str) -> String {
    sanitize(text).replace(['\n', '\t', '\r'], " ")
}

pub fn masked_secret(value: &str, room: usize, ellipsis: &str) -> StyledSpan {
    StyledSpan::new(wisp_protocol::masked(value, room, ellipsis), Token::Secret)
}

pub fn masked_secret_ends(
    value: &str,
    ends: Ends,
    counted: bool,
    room: usize,
    ellipsis: &str,
) -> StyledSpan {
    StyledSpan::new(
        wisp_protocol::masked_ends(value, ends, counted, room, ellipsis),
        Token::Secret,
    )
}

pub fn model_line(model: &str) -> String {
    one_line(wisp_protocol::shown_text(model))
}

pub fn without_models(event: &AgentEvent, models: &[&str]) -> Option<AgentEvent> {
    let text = match event {
        AgentEvent::TurnEnded {
            outcome: TurnOutcome::Error { message, .. },
            ..
        }
        | AgentEvent::CompactionFailed { message, .. } => message,
        AgentEvent::Retrying { reason, .. }
        | AgentEvent::Rejected { reason, .. }
        | AgentEvent::LogDisabled { reason } => reason,
        _ => return None,
    };
    let hidden = wisp_protocol::hide_models(text, models.iter().copied());
    if hidden == *text {
        return None;
    }
    let mut event = event.clone();
    match &mut event {
        AgentEvent::TurnEnded {
            outcome: TurnOutcome::Error { message, .. },
            ..
        }
        | AgentEvent::CompactionFailed { message, .. } => *message = hidden,
        AgentEvent::Retrying { reason, .. }
        | AgentEvent::Rejected { reason, .. }
        | AgentEvent::LogDisabled { reason } => *reason = hidden,
        _ => {}
    }
    Some(event)
}

#[cfg(test)]
mod tests;
