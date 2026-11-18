use wisp_protocol::{AttachKey, AttachKeyCode, control_byte};

const ESC: u8 = 0x1b;
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modes {
    pub application_cursor: bool,
    pub bracketed_paste: bool,
}

fn modifier(key: AttachKey) -> u8 {
    1 + u8::from(key.shift()) + 2 * u8::from(key.alt()) + 4 * u8::from(key.ctrl())
}

fn cursor(letter: u8, key: AttachKey, modes: Modes) -> Vec<u8> {
    match modifier(key) {
        1 if modes.application_cursor => vec![ESC, b'O', letter],
        1 => vec![ESC, b'[', letter],
        m => format!("\x1b[1;{m}{}", char::from(letter)).into_bytes(),
    }
}

fn tilde(number: u8, key: AttachKey) -> Vec<u8> {
    match modifier(key) {
        1 => format!("\x1b[{number}~").into_bytes(),
        m => format!("\x1b[{number};{m}~").into_bytes(),
    }
}

fn function(n: u8, key: AttachKey) -> Vec<u8> {
    match n {
        1..=4 => {
            let letter = b'P' + (n - 1);
            match modifier(key) {
                1 => vec![ESC, b'O', letter],
                m => format!("\x1b[1;{m}{}", char::from(letter)).into_bytes(),
            }
        }
        5 => tilde(15, key),
        6..=10 => tilde(n + 11, key),
        _ => tilde(n + 12, key),
    }
}

fn with_alt(key: AttachKey, mut bytes: Vec<u8>) -> Vec<u8> {
    if key.alt() {
        bytes.insert(0, ESC);
    }
    bytes
}

pub fn encode(key: AttachKey, modes: Modes) -> Vec<u8> {
    match key.code() {
        AttachKeyCode::Char { char } => {
            let bytes = match (key.ctrl(), control_byte(char)) {
                (true, Some(byte)) => vec![byte],
                _ => {
                    let mut buf = [0u8; 4];
                    char.encode_utf8(&mut buf).as_bytes().to_vec()
                }
            };
            with_alt(key, bytes)
        }
        AttachKeyCode::Enter => with_alt(key, b"\r".to_vec()),
        AttachKeyCode::Tab if key.shift() => with_alt(key, b"\x1b[Z".to_vec()),
        AttachKeyCode::Tab => with_alt(key, b"\t".to_vec()),
        AttachKeyCode::Backspace if key.ctrl() => with_alt(key, vec![0x08]),
        AttachKeyCode::Backspace => with_alt(key, vec![0x7f]),
        AttachKeyCode::Esc => with_alt(key, vec![ESC]),
        AttachKeyCode::Up => cursor(b'A', key, modes),
        AttachKeyCode::Down => cursor(b'B', key, modes),
        AttachKeyCode::Right => cursor(b'C', key, modes),
        AttachKeyCode::Left => cursor(b'D', key, modes),
        AttachKeyCode::Home => cursor(b'H', key, modes),
        AttachKeyCode::End => cursor(b'F', key, modes),
        AttachKeyCode::Insert => tilde(2, key),
        AttachKeyCode::Delete => tilde(3, key),
        AttachKeyCode::PageUp => tilde(5, key),
        AttachKeyCode::PageDown => tilde(6, key),
        AttachKeyCode::F { n } => function(n, key),
    }
}

pub fn paste(text: &str, modes: Modes) -> Vec<u8> {
    if modes.bracketed_paste && text.contains('\n') {
        let mut bytes = PASTE_START.to_vec();
        bytes.extend_from_slice(text.as_bytes());
        bytes.extend_from_slice(PASTE_END);
        bytes
    } else {
        text.as_bytes().to_vec()
    }
}

pub fn user_paste(text: &str, modes: Modes) -> Vec<u8> {
    let text = text.replace("\x1b[201~", "");
    if modes.bracketed_paste {
        let mut bytes = PASTE_START.to_vec();
        bytes.extend_from_slice(text.as_bytes());
        bytes.extend_from_slice(PASTE_END);
        bytes
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

pub use wisp_plugin_api::types::{Key, NamedKey};
pub use wisp_tool_text::terminal::key_names;

pub trait KeyBytes {
    fn attach(&self) -> Option<AttachKey>;
    fn terminal_bytes(&self, modes: Modes) -> Vec<u8>;
    fn pipe_bytes(&self) -> Option<Vec<u8>>;
}

impl KeyBytes for Key {
    fn attach(&self) -> Option<AttachKey> {
        let code = match self {
            Self::Named(NamedKey::Enter) => AttachKeyCode::Enter,
            Self::Named(NamedKey::Tab) => AttachKeyCode::Tab,
            Self::Named(NamedKey::Space) => AttachKeyCode::Char { char: ' ' },
            Self::Named(NamedKey::Esc) => AttachKeyCode::Esc,
            Self::Named(NamedKey::Backspace) => AttachKeyCode::Backspace,
            Self::Named(NamedKey::Delete) => AttachKeyCode::Delete,
            Self::Named(NamedKey::Up) => AttachKeyCode::Up,
            Self::Named(NamedKey::Down) => AttachKeyCode::Down,
            Self::Named(NamedKey::Left) => AttachKeyCode::Left,
            Self::Named(NamedKey::Right) => AttachKeyCode::Right,
            Self::Named(NamedKey::Home) => AttachKeyCode::Home,
            Self::Named(NamedKey::End) => AttachKeyCode::End,
            Self::Named(NamedKey::PageUp) => AttachKeyCode::PageUp,
            Self::Named(NamedKey::PageDown) => AttachKeyCode::PageDown,
            Self::Function(n) => AttachKeyCode::F { n: *n },
            Self::Ctrl(letter) => {
                return AttachKey::new(AttachKeyCode::Char { char: *letter }, false, false, true);
            }
            Self::Eof => return None,
        };
        AttachKey::new(code, false, false, false)
    }

    fn terminal_bytes(&self, modes: Modes) -> Vec<u8> {
        match (self, self.attach()) {
            (Self::Eof, _) => vec![0x04],
            (_, Some(key)) => encode(key, modes),
            (_, None) => Vec::new(),
        }
    }

    fn pipe_bytes(&self) -> Option<Vec<u8>> {
        match self {
            Self::Named(NamedKey::Enter) => Some(b"\n".to_vec()),
            Self::Named(NamedKey::Tab) => Some(b"\t".to_vec()),
            Self::Named(NamedKey::Space) => Some(b" ".to_vec()),
            Self::Named(NamedKey::Esc) => Some(b"\x1b".to_vec()),
            Self::Named(NamedKey::Backspace) => Some(b"\x7f".to_vec()),
            Self::Ctrl(letter) => control_byte(*letter).map(|byte| vec![byte]),
            Self::Named(_) | Self::Function(_) | Self::Eof => None,
        }
    }
}

#[cfg(test)]
mod tests;
