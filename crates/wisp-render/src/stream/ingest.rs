use crate::text::sanitize;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ingest {
    carry: Vec<u8>,
    cr: bool,
    flag: String,
}

const FLAG: char = '\u{1F3F4}';
const MAX_TAGS: usize = 6;

fn open_flag(text: &str) -> usize {
    let mut tags = 0;
    for (at, c) in text.char_indices().rev() {
        match c {
            FLAG => return text.len() - at,
            '\u{E0020}'..='\u{E007E}' if tags < MAX_TAGS => tags += 1,
            _ => return 0,
        }
    }
    0
}

fn incomplete_tail(bytes: &[u8]) -> usize {
    for back in 1..=bytes.len().min(3) {
        let at = bytes.len() - back;
        let Some(&b) = bytes.get(at) else {
            return 0;
        };
        if b & 0xC0 == 0x80 {
            continue;
        }
        let need = match b {
            0xC2..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF4 => 4,
            _ => 0,
        };
        let tail = bytes.get(at..).unwrap_or_default();
        let incomplete = std::str::from_utf8(tail).is_err_and(|e| e.error_len().is_none());
        return if need > back && incomplete { back } else { 0 };
    }
    0
}

impl Ingest {
    pub fn new() -> Ingest {
        Ingest::default()
    }

    pub fn push(&mut self, bytes: &[u8]) -> String {
        let mut buf = std::mem::take(&mut self.carry);
        buf.extend_from_slice(bytes);
        let keep = incomplete_tail(&buf);
        self.carry = buf.split_off(buf.len() - keep);
        let text = String::from_utf8_lossy(&buf);
        self.lines(&text)
    }

    pub fn push_str(&mut self, text: &str) -> String {
        self.push(text.as_bytes())
    }

    pub fn finish(&mut self) -> String {
        let mut out = std::mem::take(&mut self.flag);
        if std::mem::take(&mut self.cr) {
            out.push('\r');
        }
        let carry = std::mem::take(&mut self.carry);
        out.push_str(&String::from_utf8_lossy(&carry));
        sanitize(&out)
    }

    fn lines(&mut self, text: &str) -> String {
        let mut out = std::mem::take(&mut self.flag);
        out.reserve(text.len());
        for c in text.chars() {
            if std::mem::take(&mut self.cr) && c != '\n' {
                out.push('\r');
            }
            if c == '\r' {
                self.cr = true;
            } else {
                out.push(c);
            }
        }
        if !self.cr {
            let held = open_flag(&out);
            self.flag = out.split_off(out.len() - held);
        }
        sanitize(&out)
    }
}
