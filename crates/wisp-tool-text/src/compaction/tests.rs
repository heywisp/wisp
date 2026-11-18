use super::*;

const BASE: &str = "Write a summary.";

#[test]
fn the_instruction_keeps_its_last_line_and_its_limit() {
    let plain = instruction(BASE, None);
    assert_eq!(
        plain,
        format!("{BASE}\n{CREDENTIALS_LINE}\n{INSTRUCTIONS_LINE}")
    );
    assert!(!plain.contains(FOCUS));
    let focused = instruction(BASE, Some("  the parser  "));
    assert!(focused.ends_with(&format!("Focus: the parser\n{INSTRUCTIONS_LINE}")));
    let long = instruction(BASE, Some(&"é".repeat(40_000)));
    assert_eq!(long.chars().count(), MAX_INSTRUCTION_CHARS);
    assert!(long.ends_with(INSTRUCTIONS_LINE));
    assert_eq!(instruction(BASE, Some("   ")), plain);
}

#[test]
fn tokens_round_bytes_up() {
    assert_eq!(text_tokens(""), 0);
    assert_eq!(text_tokens("abc"), 1);
    assert_eq!(text_tokens("abcd"), 2);
    assert_eq!(text_tokens("é"), 1);
}
