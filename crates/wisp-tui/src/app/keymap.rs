use crate::term::{Key, KeyCode};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Send,
    Steer,
    Queue,
    Release,
    Newline,
    Stop,
    Clear,
    ArmQuit,
    Quit,
    HistoryPrevious,
    HistoryNext,
    EnterRows,
    SaveEdit,
    CancelEdit,
    Pager,
    Help,
    Repaint,
    Suspend,
    Scroll(ViewScroll),
    CycleEffort,
    AttachImage,
    Background,
    TakeOver,
    Copy,
    Deselect,
    PrimeRewind,
    Rewind,
    Agents,
    Edit(Key),
    Nothing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewScroll {
    HalfUp,
    HalfDown,
    Top,
    Bottom,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Context {
    pub running: bool,
    pub commanding: bool,
    pub empty: bool,
    pub blank: bool,
    pub armed: bool,
    pub first_row: bool,
    pub last_row: bool,
    pub scrolled: bool,
    pub selection: bool,
    pub tab_queues: bool,
    pub held: bool,
    pub editing: bool,
    pub rows: bool,
    pub browsing: bool,
    pub primed: bool,
    pub movable: bool,
    pub agents: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowAction {
    Up,
    Down,
    Edit,
    Remove,
    Switch,
    Leave,
    Stay,
    Pass,
}

pub fn row_action(key: Key) -> RowAction {
    let Key {
        code,
        ctrl,
        alt,
        shift,
    } = key;
    let plain = !ctrl && !alt;
    match code {
        KeyCode::Up if plain => RowAction::Up,
        KeyCode::Down if plain => RowAction::Down,
        KeyCode::Enter if plain && !shift => RowAction::Edit,
        KeyCode::Char('e') if plain => RowAction::Edit,
        KeyCode::Delete | KeyCode::Backspace if plain => RowAction::Remove,
        KeyCode::Char('x') if plain => RowAction::Remove,
        KeyCode::Tab if plain && !shift => RowAction::Switch,
        KeyCode::Esc if plain => RowAction::Leave,
        KeyCode::Char('c' | 'C') if ctrl && !alt => RowAction::Stay,
        _ => RowAction::Pass,
    }
}

pub(crate) fn take_over_key(key: Key) -> bool {
    match key.code {
        KeyCode::F(12) => !key.ctrl && !key.alt && !key.shift,
        KeyCode::Char(']' | '5') => key.ctrl && !key.alt,
        _ => false,
    }
}

pub fn action(key: Key, cx: Context) -> Action {
    let Key {
        code,
        ctrl,
        alt,
        shift,
    } = key;
    match code {
        KeyCode::Enter if shift || alt || ctrl => Action::Newline,
        KeyCode::Enter if cx.editing && cx.blank => Action::Nothing,
        KeyCode::Enter if cx.editing => Action::SaveEdit,
        KeyCode::Esc if cx.editing => Action::CancelEdit,
        KeyCode::Tab | KeyCode::Up | KeyCode::Down if cx.editing && !ctrl && !alt => {
            Action::Edit(key)
        }
        KeyCode::Enter if cx.blank && cx.held && !cx.running => Action::Release,
        KeyCode::Enter if cx.blank => Action::Nothing,
        KeyCode::Enter if cx.running => Action::Steer,
        KeyCode::Enter => Action::Send,
        KeyCode::Tab if cx.running && cx.tab_queues && !ctrl && !alt && !shift => Action::Queue,
        KeyCode::Char('j' | 'J') if ctrl && !alt => Action::Newline,
        KeyCode::Char('c' | 'C') if ctrl && cx.selection => Action::Copy,
        KeyCode::Esc if cx.running || cx.commanding => Action::Stop,
        KeyCode::Esc if cx.selection => Action::Deselect,
        KeyCode::Esc if cx.scrolled => Action::Scroll(ViewScroll::Bottom),
        KeyCode::Esc if cx.empty && !ctrl && !alt && cx.primed => Action::Rewind,
        KeyCode::Esc if cx.empty && !ctrl && !alt => Action::PrimeRewind,
        KeyCode::Esc => Action::Nothing,
        KeyCode::PageUp if !alt => Action::Scroll(ViewScroll::HalfUp),
        KeyCode::PageDown if !alt => Action::Scroll(ViewScroll::HalfDown),
        KeyCode::Home if ctrl || (cx.empty && !alt) => Action::Scroll(ViewScroll::Top),
        KeyCode::End if ctrl || (cx.empty && !alt) => Action::Scroll(ViewScroll::Bottom),
        KeyCode::Char('c' | 'C') if ctrl => {
            if cx.running || cx.commanding {
                Action::Stop
            } else if !cx.empty {
                Action::Clear
            } else if cx.armed {
                Action::Quit
            } else {
                Action::ArmQuit
            }
        }
        KeyCode::Char('d' | 'D') if ctrl && cx.empty && !cx.running && cx.agents && !cx.armed => {
            Action::ArmQuit
        }
        KeyCode::Char('d' | 'D') if ctrl && cx.empty && !cx.running => Action::Quit,
        KeyCode::Char('d' | 'D') if ctrl && cx.empty => Action::Nothing,
        KeyCode::Char('b' | 'B') if ctrl && !alt && cx.movable => Action::Background,
        _ if take_over_key(key) => Action::TakeOver,
        KeyCode::Char('o' | 'O') if ctrl => Action::Pager,
        KeyCode::Char('t' | 'T') if ctrl && !alt => Action::Agents,
        KeyCode::Char('l' | 'L') if ctrl => Action::Repaint,
        KeyCode::Char('z' | 'Z') if ctrl => Action::Suspend,
        KeyCode::BackTab => Action::CycleEffort,
        KeyCode::Char('?') if !ctrl && !alt && cx.empty && !cx.editing => Action::Help,
        KeyCode::Char('v' | 'V') if alt != ctrl => Action::AttachImage,
        KeyCode::Up if cx.first_row && !ctrl && !alt && cx.rows && !cx.browsing => {
            Action::EnterRows
        }
        KeyCode::Up if cx.first_row && !ctrl && !alt => Action::HistoryPrevious,
        KeyCode::Down if cx.last_row && !ctrl && !alt => Action::HistoryNext,
        _ => Action::Edit(key),
    }
}

#[cfg(test)]
mod tests;
