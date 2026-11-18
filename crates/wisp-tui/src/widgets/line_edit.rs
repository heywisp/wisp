use crate::term::{Key, KeyCode};
use wisp_protocol::{MAX_ENDPOINT_NAME, endpoint_char};

#[derive(Clone, Copy)]
pub struct LineEdit {
    pub max: usize,
    pub filter: fn(char) -> Option<char>,
}

impl LineEdit {
    pub const ANY: Self = Self {
        max: usize::MAX,
        filter: Some,
    };
    pub const TEXT: Self = Self {
        max: usize::MAX,
        filter: printable,
    };
    pub const ENDPOINT_NAME: Self = Self {
        max: MAX_ENDPOINT_NAME,
        filter: |c| {
            let c = c.to_ascii_lowercase();
            endpoint_char(c).then_some(c)
        },
    };

    pub fn push(self, text: &mut String, c: char) -> bool {
        match (self.filter)(c) {
            Some(c) if text.chars().count() < self.max => {
                text.push(c);
                true
            }
            _ => false,
        }
    }

    pub fn paste(self, text: &mut String, pasted: &str) -> bool {
        let room = self.max.saturating_sub(text.chars().count());
        let before = text.len();
        text.extend(pasted.chars().filter_map(self.filter).take(room));
        text.len() > before
    }

    pub fn erase(text: &mut String, all: bool) {
        if all {
            text.clear();
        } else {
            text.pop();
        }
    }

    pub fn key(self, text: &mut String, key: Key) -> bool {
        match key.code {
            KeyCode::Char('u' | 'U') if key.ctrl && !key.alt => Self::erase(text, true),
            KeyCode::Backspace if !key.alt => Self::erase(text, false),
            KeyCode::Char(c) if !key.ctrl && !key.alt => return self.push(text, c),
            _ => return false,
        }
        true
    }
}

pub fn printable(c: char) -> Option<char> {
    (!c.is_control()).then_some(c)
}

pub fn cleaned(c: char) -> Option<char> {
    printable(if c.is_whitespace() { ' ' } else { c })
}
