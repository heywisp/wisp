use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ExitStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermSize {
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundReason {
    Started,
    Quiet,
    MovedByUser,
    StillRunning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StoppedBy {
    Model,
    User,
    SessionEnded,
    TimeLimit { after_ms: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LostReason {
    StoppedAtResume,
    Gone,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProcessEnd {
    Exited { exit: ExitStatus },
    Stopped { by: StoppedBy },
    Lost { reason: LostReason },
    Failed { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportedEnd {
    pub id: u32,
    pub end: ProcessEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOrigin {
    ProcessExit,
    Mail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalKind {
    Interrupt,
    Kill,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetachReason {
    User,
    Ended,
    Disconnected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NeedsInputKind {
    HiddenInput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachRequestEnd {
    Typed,
    AttachedOnly,
    Declined,
    ProcessEnded,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretRequestEnd {
    Entered,
    ProcessEnded,
    Stopped,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AttachKeyCode {
    Char { char: char },
    Enter,
    Tab,
    Backspace,
    Esc,
    Up,
    Down,
    Right,
    Left,
    Home,
    End,
    Insert,
    Delete,
    PageUp,
    PageDown,
    F { n: u8 },
}

impl fmt::Debug for AttachKeyCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AttachKeyCode(..)")
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "KeyFields", into = "KeyFields")]
pub struct AttachKey {
    code: AttachKeyCode,
    shift: bool,
    alt: bool,
    ctrl: bool,
}

pub const MAX_FUNCTION_KEY: u8 = 12;

impl AttachKey {
    pub fn new(code: AttachKeyCode, shift: bool, alt: bool, ctrl: bool) -> Option<Self> {
        encodable(code, shift, ctrl).then_some(Self {
            code,
            shift,
            alt,
            ctrl,
        })
    }

    pub fn code(self) -> AttachKeyCode {
        self.code
    }

    pub fn shift(self) -> bool {
        self.shift
    }

    pub fn alt(self) -> bool {
        self.alt
    }

    pub fn ctrl(self) -> bool {
        self.ctrl
    }
}

impl fmt::Debug for AttachKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AttachKey(..)")
    }
}

fn encodable(code: AttachKeyCode, shift: bool, ctrl: bool) -> bool {
    match code {
        AttachKeyCode::Char { char } if ctrl => control_byte(char).is_some(),
        AttachKeyCode::Char { char } => !char.is_control(),
        AttachKeyCode::Enter => true,
        AttachKeyCode::Tab => !ctrl,
        AttachKeyCode::Backspace => !shift,
        AttachKeyCode::Esc => !shift && !ctrl,
        AttachKeyCode::Up
        | AttachKeyCode::Down
        | AttachKeyCode::Right
        | AttachKeyCode::Left
        | AttachKeyCode::Home
        | AttachKeyCode::End
        | AttachKeyCode::Insert
        | AttachKeyCode::Delete
        | AttachKeyCode::PageUp
        | AttachKeyCode::PageDown => true,
        AttachKeyCode::F { n } => (1..=MAX_FUNCTION_KEY).contains(&n),
    }
}

pub fn control_byte(char: char) -> Option<u8> {
    match char {
        'a'..='z' | 'A'..='Z' => u8::try_from(char.to_ascii_lowercase())
            .ok()
            .map(|b| b - b'a' + 1),
        ' ' | '@' | '2' => Some(0x00),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        '^' | '6' => Some(0x1e),
        '_' | '7' => Some(0x1f),
        '8' | '?' => Some(0x7f),
        _ => None,
    }
}

#[derive(Serialize, Deserialize)]
struct KeyFields {
    code: AttachKeyCode,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    shift: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    alt: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    ctrl: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoTerminalCode;

impl fmt::Display for NoTerminalCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the key has no terminal code")
    }
}

impl std::error::Error for NoTerminalCode {}

impl TryFrom<KeyFields> for AttachKey {
    type Error = NoTerminalCode;

    fn try_from(fields: KeyFields) -> Result<Self, Self::Error> {
        Self::new(fields.code, fields.shift, fields.alt, fields.ctrl).ok_or(NoTerminalCode)
    }
}

impl From<AttachKey> for KeyFields {
    fn from(key: AttachKey) -> Self {
        Self {
            code: key.code,
            shift: key.shift,
            alt: key.alt,
            ctrl: key.ctrl,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AttachInput {
    Text { text: String },
    Key { key: AttachKey },
    Paste { text: String },
}

impl fmt::Debug for AttachInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AttachInput(..)")
    }
}

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AttachItems(pub Vec<AttachInput>);

impl fmt::Debug for AttachItems {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AttachItems({})", self.0.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: AttachKeyCode, shift: bool, alt: bool, ctrl: bool) -> Option<AttachKey> {
        AttachKey::new(code, shift, alt, ctrl)
    }

    fn ch(char: char) -> AttachKeyCode {
        AttachKeyCode::Char { char }
    }

    #[test]
    fn text_keys_take_any_printable_character_and_shift() {
        for c in ['a', 'Z', '7', ']', 'é', '中', ' '] {
            assert!(key(ch(c), false, false, false).is_some(), "{c:?}");
            assert!(key(ch(c), true, true, false).is_some(), "{c:?}");
        }
        for c in ['\u{1b}', '\n', '\r', '\t', '\u{7f}', '\u{0}'] {
            assert!(key(ch(c), false, false, false).is_none(), "{c:?}");
        }
    }

    #[test]
    fn ctrl_takes_letters_and_the_table_s_punctuation_only() {
        let sends: Vec<char> = ('a'..='z')
            .chain('A'..='Z')
            .chain([
                ' ', '@', '2', '[', '3', '\\', '4', '^', '6', '_', '7', '8', '?',
            ])
            .collect();
        for c in sends {
            assert!(key(ch(c), false, false, true).is_some(), "{c:?}");
            assert!(key(ch(c), true, true, true).is_some(), "{c:?}");
        }
        for c in [']', '5', '1', '9', '0', ',', '.', '/', ';', 'é'] {
            assert!(key(ch(c), false, false, true).is_none(), "{c:?}");
        }
        assert_eq!(control_byte('c'), Some(0x03));
        assert_eq!(control_byte('C'), Some(0x03));
        assert_eq!(control_byte('z'), Some(0x1a));
        assert_eq!(control_byte('a'), Some(0x01));
    }

    #[test]
    fn named_keys_follow_the_table() {
        assert!(key(AttachKeyCode::Enter, true, false, true).is_some());
        assert!(key(AttachKeyCode::Tab, true, false, false).is_some());
        assert!(key(AttachKeyCode::Tab, false, false, true).is_none());
        assert!(key(AttachKeyCode::Backspace, false, true, true).is_some());
        assert!(key(AttachKeyCode::Backspace, true, false, false).is_none());
        assert!(key(AttachKeyCode::Esc, false, true, false).is_some());
        assert!(key(AttachKeyCode::Esc, false, false, true).is_none());
        assert!(key(AttachKeyCode::Esc, true, false, false).is_none());
        for code in [
            AttachKeyCode::Up,
            AttachKeyCode::Down,
            AttachKeyCode::Right,
            AttachKeyCode::Left,
            AttachKeyCode::Home,
            AttachKeyCode::End,
            AttachKeyCode::Insert,
            AttachKeyCode::Delete,
            AttachKeyCode::PageUp,
            AttachKeyCode::PageDown,
        ] {
            assert!(key(code, true, true, true).is_some());
        }
        for n in 1..=12 {
            assert!(
                key(AttachKeyCode::F { n }, true, true, true).is_some(),
                "F{n}"
            );
        }
        for n in [0, 13, 24] {
            assert!(
                key(AttachKeyCode::F { n }, false, false, false).is_none(),
                "F{n}"
            );
        }
    }

    #[test]
    fn a_key_with_no_terminal_code_does_not_deserialise() {
        let ok: AttachKey =
            serde_json::from_str(r#"{"code":{"type":"char","char":"c"},"ctrl":true}"#).unwrap();
        assert_eq!(key(ch('c'), false, false, true), Some(ok));
        let refused =
            serde_json::from_str::<AttachKey>(r#"{"code":{"type":"char","char":"]"},"ctrl":true}"#)
                .unwrap_err()
                .to_string();
        assert_eq!(refused, "the key has no terminal code");
        assert!(serde_json::from_str::<AttachKey>(r#"{"code":{"type":"f","n":13}}"#).is_err());
    }

    #[test]
    fn debug_never_prints_what_was_typed() {
        let secret = AttachKey::new(ch('s'), false, false, false).unwrap();
        let items = AttachItems(vec![
            AttachInput::Text {
                text: "hunter2".into(),
            },
            AttachInput::Key { key: secret },
            AttachInput::Paste {
                text: "pasted secret".into(),
            },
        ]);
        assert_eq!(format!("{items:?}"), "AttachItems(3)");
        assert_eq!(format!("{:?}", items.0[0]), "AttachInput(..)");
        assert_eq!(format!("{secret:?}"), "AttachKey(..)");
        assert_eq!(format!("{:?}", secret.code()), "AttachKeyCode(..)");
        assert_eq!(format!("{:#?}", items), "AttachItems(3)");
    }
}
