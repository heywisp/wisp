use wisp_render::chrome::{HelpRow, HelpSection};
use wisp_render::theme::glyphs::GlyphSet;
use wisp_render::transcript::copy;

use super::commands::{COMMANDS, Command, PluginCommand, synopsis};
use super::keymap::{Action, RowAction, ViewScroll};
use super::pager::{PagerAction, Scroll};
use super::presenter::{NEW_KEY, drag_key};
use crate::term::{Key, KeyCode};
use Doc::{Main, Pager, Reader, Rows};

pub const NEWLINE_ROW: &str = "alt-enter, ctrl-j";
pub const NEWLINE_ROW_KITTY: &str = "shift-enter, alt-enter, ctrl-j";
pub const FIRST_KEYS: usize = 1;
pub const RIGHT_CLICK: &str = "right-click";
pub const RIGHT_CLICK_PASTES: &str = "Copy the selection; paste when nothing is selected";
pub const RIGHT_CLICK_COPIES: &str = "Copy the selection";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Doc {
    Main(Action),
    Rows(RowAction),
    Pager(PagerAction),
    Reader(PagerAction),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum When {
    Always,
    Mouse,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyRow {
    pub keys: &'static str,
    pub text: &'static str,
    pub when: When,
    pub docs: &'static [Doc],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Section {
    pub title: &'static str,
    pub rows: &'static [KeyRow],
}

const fn row(keys: &'static str, text: &'static str, docs: &'static [Doc]) -> KeyRow {
    KeyRow {
        keys,
        text,
        when: When::Always,
        docs,
    }
}

const fn mouse_row(keys: &'static str, text: &'static str, docs: &'static [Doc]) -> KeyRow {
    KeyRow {
        when: When::Mouse,
        ..row(keys, text, docs)
    }
}

const LINES_UP: PagerAction = PagerAction::Scroll(Scroll::Lines(-1));
const LINES_DOWN: PagerAction = PagerAction::Scroll(Scroll::Lines(1));
const PAGE_UP: PagerAction = PagerAction::Scroll(Scroll::Pages(-1));
const PAGE_DOWN: PagerAction = PagerAction::Scroll(Scroll::Pages(1));
const HALF_UP: PagerAction = PagerAction::Scroll(Scroll::Halves(-1));
const HALF_DOWN: PagerAction = PagerAction::Scroll(Scroll::Halves(1));
const PAGER_TOP: PagerAction = PagerAction::Scroll(Scroll::Top);
const PAGER_BOTTOM: PagerAction = PagerAction::Scroll(Scroll::Bottom);
const SLASH: Key = Key::plain(KeyCode::Char('/'));

pub const KEYS: &[Section] = &[
    Section {
        title: copy::KEYS_WRITING,
        rows: &[
            row(copy::ENTER_KEY, "Send", &[Main(Action::Send)]),
            row("{newline}", "Add a line", &[Main(Action::Newline)]),
            row("/", "Suggest commands", &[Main(Action::Edit(SLASH))]),
            row("?", "This help, on an empty message", &[Main(Action::Help)]),
            row(
                "{esc} {esc}",
                "Rewind, on an empty message",
                &[Main(Action::PrimeRewind), Main(Action::Rewind)],
            ),
            row(copy::TAB_KEY, "Expand a paste", &[]),
            row(
                "ctrl-v, alt-v",
                "Attach the clipboard's image",
                &[Main(Action::AttachImage)],
            ),
            row(
                "{up} {down}",
                "Recall this session's messages",
                &[Main(Action::HistoryPrevious), Main(Action::HistoryNext)],
            ),
            row("ctrl-u, ctrl-r", "Undo, redo", &[]),
            row(
                copy::CTRL_C_KEY,
                "Clear the message",
                &[Main(Action::Clear)],
            ),
            row(
                "shift-tab",
                "Cycle effort where it is free",
                &[Main(Action::CycleEffort)],
            ),
        ],
    },
    Section {
        title: copy::KEYS_WORKING,
        rows: &[
            row(copy::ENTER_KEY, "Add to this turn", &[Main(Action::Steer)]),
            row(
                copy::TAB_KEY,
                "Save for the next turn",
                &[Main(Action::Queue)],
            ),
            row("{esc}, {ctrl_c}", "Stop", &[Main(Action::Stop)]),
            row("{up}", "Go to waiting messages", &[Main(Action::EnterRows)]),
            row(
                "ctrl-b",
                "Background the running command",
                &[Main(Action::Background)],
            ),
            row(
                "{take_over}",
                "Take over a waiting terminal, and go back",
                &[Main(Action::TakeOver)],
            ),
        ],
    },
    Section {
        title: copy::KEYS_WAITING,
        rows: &[
            row(
                "{up} {down}",
                "Move between them",
                &[Rows(RowAction::Up), Rows(RowAction::Down)],
            ),
            row("{enter}, e", "Edit", &[Rows(RowAction::Edit)]),
            row("del, x", "Remove", &[Rows(RowAction::Remove)]),
            row(
                copy::TAB_KEY,
                "Switch next step and next turn",
                &[Rows(RowAction::Switch)],
            ),
            row(
                copy::ESC_KEY,
                "Back to your message",
                &[Rows(RowAction::Leave)],
            ),
            row(
                copy::CTRL_C_KEY,
                "Stop, staying here",
                &[Rows(RowAction::Stay)],
            ),
            row(
                "{enter}, while editing",
                "Save the edit",
                &[Main(Action::SaveEdit)],
            ),
            row(
                "{esc}, while editing",
                "Cancel the edit",
                &[Main(Action::CancelEdit)],
            ),
            row(
                "{enter}, empty message",
                "Send held messages",
                &[Main(Action::Release)],
            ),
        ],
    },
    Section {
        title: copy::KEYS_READING,
        rows: &[
            row(
                "wheel, pgup, pgdn",
                "Scroll",
                &[
                    Main(Action::Scroll(ViewScroll::HalfUp)),
                    Main(Action::Scroll(ViewScroll::HalfDown)),
                ],
            ),
            row(
                "ctrl-home, {ctrl_end}",
                "Jump to the top or the bottom",
                &[
                    Main(Action::Scroll(ViewScroll::Top)),
                    Main(Action::Scroll(ViewScroll::Bottom)),
                ],
            ),
            row(
                copy::ESC_KEY,
                "Back to the bottom",
                &[Main(Action::Scroll(ViewScroll::Bottom))],
            ),
            mouse_row("drag, {ctrl_c}", "Select, then copy", &[Main(Action::Copy)]),
            mouse_row(RIGHT_CLICK, "{right-click}", &[]),
            mouse_row(
                copy::ESC_KEY,
                "Clear the selection",
                &[Main(Action::Deselect)],
            ),
            row("{select}", "Select in the terminal", &[]),
            row(
                copy::KEY_CTRL_O,
                "Pick what to read in full",
                &[Main(Action::Pager)],
            ),
        ],
    },
    Section {
        title: copy::KEYS_TRANSCRIPT,
        rows: &[
            row(
                "{up} {down}, j k",
                "Scroll a line",
                &[Pager(LINES_UP), Pager(LINES_DOWN)],
            ),
            row(
                "pgup, pgdn, {space}, b f",
                "Scroll a page",
                &[Pager(PAGE_UP), Pager(PAGE_DOWN)],
            ),
            row(
                "u d",
                "Scroll half a page",
                &[Pager(HALF_UP), Pager(HALF_DOWN)],
            ),
            row(
                "g G, home end",
                "Jump to the top or the bottom",
                &[Pager(PAGER_TOP), Pager(PAGER_BOTTOM)],
            ),
            row(
                "[",
                "Hand off to the scrollback",
                &[Pager(PagerAction::HandOff)],
            ),
            row(
                "/ ?",
                "Search forward or back",
                &[
                    Pager(PagerAction::Search { back: false }),
                    Pager(PagerAction::Search { back: true }),
                ],
            ),
            row(
                "n N",
                "Next or previous match",
                &[Pager(PagerAction::Next), Pager(PagerAction::Previous)],
            ),
            row("q, {esc}, {ctrl_o}", "Back", &[Pager(PagerAction::Close)]),
            row(
                copy::KEY_CTRL_O,
                "From a reader, every detail at that cell",
                &[Reader(PagerAction::Detail)],
            ),
            row(
                "s",
                "From a reader, a running terminal's screen",
                &[Reader(PagerAction::Screen)],
            ),
            row(
                "c",
                "From a reader, copy its code",
                &[Reader(PagerAction::Copy)],
            ),
            row(
                copy::CTRL_C_KEY,
                "Stop, or back when idle",
                &[Pager(PagerAction::Stop), Pager(PagerAction::Close)],
            ),
        ],
    },
    Section {
        title: copy::KEYS_ANYWHERE,
        rows: &[
            row(
                "ctrl-l",
                "Repaint",
                &[Main(Action::Repaint), Pager(PagerAction::Repaint)],
            ),
            row(
                "ctrl-z",
                "Suspend",
                &[Main(Action::Suspend), Pager(PagerAction::Suspend)],
            ),
            row(
                "ctrl-t",
                "List the agents and their spend",
                &[Main(Action::Agents)],
            ),
            row(
                "{ctrl_c} twice, ctrl-d",
                "Quit",
                &[Main(Action::ArmQuit), Main(Action::Quit)],
            ),
        ],
    },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Terminal {
    pub glyphs: GlyphSet,
    pub shift_enter: bool,
    pub mouse: bool,
    pub select_key: &'static str,
    pub right_paste: bool,
}

impl Terminal {
    fn shows(self, when: When) -> bool {
        match when {
            When::Always => true,
            When::Mouse => self.mouse,
        }
    }

    pub fn keys(self, keys: &str) -> String {
        let [up, down] = copy::arrow_keys(self.glyphs);
        let newline = if self.shift_enter {
            NEWLINE_ROW_KITTY
        } else {
            NEWLINE_ROW
        };
        let take_over = format!("{}, {}", copy::F12_KEY, copy::BRACKET_KEY);
        keys.replace("{newline}", newline)
            .replace("{esc}", copy::ESC_KEY)
            .replace("{enter}", copy::ENTER_KEY)
            .replace("{ctrl_c}", copy::CTRL_C_KEY)
            .replace("{space}", copy::SPACE_KEY)
            .replace("{ctrl_o}", copy::KEY_CTRL_O)
            .replace("{ctrl_end}", NEW_KEY)
            .replace("{take_over}", &take_over)
            .replace("{select}", drag_key(self.mouse, self.select_key))
            .replace("{up}", up)
            .replace("{down}", down)
    }

    pub fn text(self, text: &str) -> String {
        let right = if self.right_paste {
            RIGHT_CLICK_PASTES
        } else {
            RIGHT_CLICK_COPIES
        };
        text.replace("{right-click}", right)
    }
}

pub fn command_row(command: &Command) -> HelpRow {
    let names: Vec<String> = std::iter::once(command.name)
        .chain(command.aliases.iter().copied())
        .map(|name| format!("/{name}"))
        .collect();
    let full = synopsis(command);
    let args = full
        .strip_prefix(&format!("/{}", command.name))
        .unwrap_or_default()
        .to_owned();
    HelpRow {
        keys: names.join(", "),
        args,
        text: command.summary.to_owned(),
    }
}

pub fn plugin_row(command: &PluginCommand) -> HelpRow {
    let names: Vec<String> = command
        .shown(false)
        .iter()
        .map(|name| format!("/{name}"))
        .collect();
    HelpRow {
        keys: names.join(", "),
        args: command.synopsis_args(),
        text: command.summary.clone(),
    }
}

pub fn first_keys(plugins: &[PluginCommand]) -> usize {
    FIRST_KEYS + usize::from(!plugins.is_empty())
}

pub fn sections(terminal: Terminal, plugins: &[PluginCommand]) -> Vec<HelpSection> {
    let commands = HelpSection {
        lead: None,
        title: copy::HELP_COMMANDS.to_owned(),
        rows: COMMANDS.iter().map(command_row).collect(),
    };
    let plugins = (!plugins.is_empty()).then(|| HelpSection {
        lead: None,
        title: copy::HELP_PLUGINS.to_owned(),
        rows: plugins.iter().map(plugin_row).collect(),
    });
    let keys = KEYS.iter().map(|section| HelpSection {
        lead: Some(copy::KEYS_LEAD.to_owned()),
        title: section.title.to_owned(),
        rows: section
            .rows
            .iter()
            .filter(|row| terminal.shows(row.when))
            .map(|row| HelpRow {
                keys: terminal.keys(row.keys),
                args: String::new(),
                text: terminal.text(row.text),
            })
            .collect(),
    });
    std::iter::once(commands)
        .chain(plugins)
        .chain(keys)
        .collect()
}

#[cfg(test)]
mod tests;
