use clap::Parser;

use super::*;

#[derive(Parser)]
struct Flags {
    #[arg(long, value_enum)]
    instructions: Option<InstructionsArg>,
    #[arg(long, value_parser = parse_warn_tokens)]
    instructions_warn_tokens: Option<u64>,
}

#[test]
fn the_source_comes_from_the_flag_then_the_file_then_auto() {
    assert_eq!(
        settings(None, None, None),
        Settings {
            source: InstructionsSource::Auto,
            warn_tokens: 8_000
        }
    );
    let file = InstructionsConfig {
        source: Some(InstructionsSource::Default),
        warn_tokens: Some(100),
    };
    assert_eq!(
        settings(None, None, Some(&file)),
        Settings {
            source: InstructionsSource::Default,
            warn_tokens: 100
        }
    );
    assert_eq!(
        settings(Some(InstructionsArg::None), Some(9_000), Some(&file)),
        Settings {
            source: InstructionsSource::None,
            warn_tokens: 9_000
        }
    );
}

#[test]
fn the_flags_parse_their_values_and_refuse_others() {
    let flags = Flags::try_parse_from([
        "wisp",
        "--instructions",
        "default",
        "--instructions-warn-tokens",
        "8k",
    ])
    .unwrap();
    assert_eq!(flags.instructions, Some(InstructionsArg::Default));
    assert_eq!(flags.instructions_warn_tokens, Some(8_000));
    let bad = Flags::try_parse_from(["wisp", "--instructions", "all"])
        .err()
        .unwrap()
        .to_string();
    assert!(bad.contains("--instructions"), "{bad}");
    for value in ["-1", ""] {
        assert_eq!(
            parse_warn_tokens(value),
            Err("not a token count, such as 8000 or 8k".to_owned())
        );
        let flag = format!("--instructions-warn-tokens={value}");
        let bad = Flags::try_parse_from(["wisp", flag.as_str()])
            .err()
            .unwrap()
            .to_string();
        assert!(bad.contains("--instructions-warn-tokens"), "{bad}");
    }
}

#[test]
fn facts_carry_the_summary_for_the_welcome_line() {
    let summary = wisp_protocol::InstructionsSummary::default();
    let loaded = Loaded {
        chosen: Some(wisp_protocol::Scope::Project),
        summary: summary.clone(),
    };
    let facts = facts(&loaded);
    assert_eq!(facts.chosen, Some(wisp_protocol::Scope::Project));
    assert_eq!(facts.summary, summary);
    assert_eq!(facts.note, None);
}

#[test]
fn the_flag_values_are_the_settings_choices() {
    let extras: Vec<&str> = ExtraArg::value_variants()
        .iter()
        .map(|v| v.name())
        .collect();
    assert_eq!(extras, wisp_protocol::settings::INSTRUCTION_EXTRAS);
    let sources: Vec<&str> = InstructionsArg::value_variants()
        .iter()
        .map(|v| v.name())
        .collect();
    assert_eq!(sources, wisp_protocol::InstructionsSource::WORDS);
}
