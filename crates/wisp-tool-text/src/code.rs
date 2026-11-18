use crate::output::group;

pub const MAX_SIDE_LINES: usize = 10_000;
pub const MAX_SIDE_BYTES: usize = 512 * 1024;

pub fn line_count(text: &str) -> usize {
    text.split_inclusive('\n').count()
}

pub fn size(bytes: usize) -> String {
    if bytes >= 1024 * 1024 && bytes.is_multiple_of(1024 * 1024) {
        format!("{} MiB", bytes / (1024 * 1024))
    } else if bytes >= 1024 {
        format!("{} KiB", group(bytes.div_ceil(1024) as u64))
    } else {
        format!("{} bytes", group(bytes as u64))
    }
}

pub fn check_side(lines: usize, bytes: usize, what: &str) -> Result<(), String> {
    if lines > MAX_SIDE_LINES {
        return Err(format!(
            "{what} has {} lines; the limit is {} lines",
            group(lines as u64),
            group(MAX_SIDE_LINES as u64)
        ));
    }
    if bytes > MAX_SIDE_BYTES {
        return Err(format!(
            "{what} is {}; the limit is {}",
            size(bytes),
            size(MAX_SIDE_BYTES)
        ));
    }
    Ok(())
}

pub fn check_controls(code: &str) -> Result<(), String> {
    let mut chars = code.chars().peekable();
    let mut line = 1;
    while let Some(c) = chars.next() {
        match c {
            '\n' => line += 1,
            '\t' => {}
            '\r' if chars.peek() == Some(&'\n') => {}
            c if c.is_control() => {
                return Err(format!(
                    "code holds a control character (U+{:04X}) on line {line}",
                    u32::from(c)
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

pub fn check(code: &str) -> Result<(), String> {
    check_side(line_count(code), code.len(), "code")?;
    check_controls(code)
}
