pub const BYTES_PER_TOKEN: u64 = 3;

pub const MAX_INSTRUCTION_CHARS: usize = 16_384;

pub const INSTRUCTIONS_LINE: &str = "The instructions are in the system prompt, and any \
update follows this summary; don't restate them.";

pub const CREDENTIALS_LINE: &str = "Never copy the value of a key, access token, password or other credential into the summary; name the credential instead.";

const FOCUS: &str = "Focus: ";

pub fn text_tokens(text: &str) -> u64 {
    (text.len() as u64).div_ceil(BYTES_PER_TOKEN)
}

pub fn instruction(base: &str, focus: Option<&str>) -> String {
    let mut text = format!("{base}\n{CREDENTIALS_LINE}\n");
    if let Some(focus) = focus.map(str::trim).filter(|f| !f.is_empty()) {
        let used = text.chars().count() + FOCUS.len() + 1 + INSTRUCTIONS_LINE.chars().count();
        let room = MAX_INSTRUCTION_CHARS.saturating_sub(used);
        let kept: String = focus.chars().take(room).collect();
        text.push_str(FOCUS);
        text.push_str(&kept);
        text.push('\n');
    }
    text.push_str(INSTRUCTIONS_LINE);
    text
}

#[cfg(test)]
mod tests;
