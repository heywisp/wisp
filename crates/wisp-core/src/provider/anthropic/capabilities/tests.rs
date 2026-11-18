use wisp_protocol::Effort;

use super::*;
use crate::provider::openai::GPT;

fn described() -> Vec<(&'static str, &'static ModelCapabilities)> {
    let rows = ModelTable::shared().rows().iter();
    rows.flat_map(|r| std::iter::once(&r.id).chain(&r.aliases))
        .filter_map(|id| Some((id.as_str(), row(id)?)))
        .collect()
}

fn quote(key: &str) -> &'static str {
    anthropic_lists().unwrap().sources[key].as_str()
}

#[test]
fn a_default_is_always_one_of_the_levels() {
    for row in ModelTable::shared().rows() {
        let Some(caps) = row.capabilities else {
            continue;
        };
        if let Some(d) = caps.default_effort {
            assert!(caps.takes_effort(d), "{}", row.id);
        }
        assert!(
            !caps.per_message_effort || caps.adaptive_thinking,
            "{}",
            row.id
        );
    }
}

#[test]
fn unknown_ids_have_no_row() {
    assert!(row("claude-opus-9").is_none());
    assert!(row("").is_none());
    assert!(row("CLAUDE-OPUS-5-5").is_none());
    assert!(row("gpt-5.5").is_none());
}

#[test]
fn the_many_image_rule_is_the_same_on_every_row() {
    let rows = ModelTable::shared().rows().iter();
    let described = rows.filter_map(|row| Some((row.id.as_str(), row.capabilities?)));
    let defaults = [("GPT", GPT), ("LISTED_ONLY", LISTED_ONLY)];
    for (id, row) in described.chain(defaults) {
        let images = row
            .images
            .unwrap_or_else(|| panic!("{id} has no image columns"));
        assert_eq!(images.many_images, 20, "{id}");
        assert_eq!(images.many_image_edge, 2000, "{id}");
        assert_eq!(images.many_image_edge, wisp_media::MAX_LONG_EDGE, "{id}");
    }
}

#[test]
fn account_bound_thinking_matches_its_quoted_source() {
    let source = quote("account_bound_thinking");
    assert!(source.contains(
        "Claude Sonnet 5.5, Claude Haiku 5.5, Claude Fable 5.2, and Claude Mythos 5.2 work only in the account"
    ));
    let models = &anthropic_lists().unwrap().account_bound_thinking;
    assert_eq!(
        models,
        &["claude-fable-5-2", "claude-haiku-5-5", "claude-sonnet-5-5"]
    );
    for id in models {
        assert!(row(id).is_some(), "{id}");
        assert!(account_bound_thinking(id));
    }
    for id in ["claude-opus-5-5", "claude-sonnet-5", "claude-fable-5-1", ""] {
        assert!(!account_bound_thinking(id), "{id}");
    }
}

#[test]
fn message_threads_match_their_quoted_source() {
    let source = quote("message_threads");
    assert!(
        source
            .starts_with("https://platform.claude.com/docs/en/build-with-claude/message-threads:")
    );
    assert!(source.contains("supportedModels"));
    let models = &anthropic_lists().unwrap().message_threads;
    assert!(!models.is_empty());
    for id in models {
        assert!(message_threads(id), "{id}");
    }
    for id in [
        "claude-opus-4-5",
        "claude-sonnet-4-5",
        "claude-haiku-4-5",
        "gpt-5.5",
        "",
    ] {
        assert!(!message_threads(id), "{id}");
    }
}

#[test]
fn inline_tools_match_their_quoted_source() {
    let listed = [
        ("claude-fable-5-2", "Claude Fable 5.2"),
        ("claude-fable-5-1", "Claude Fable 5.1"),
        ("claude-mythos-5-1", "Claude Mythos 5.1"),
        ("claude-fable-5", "Claude Fable 5,"),
        ("claude-mythos-5", "Claude Mythos 5,"),
        ("claude-opus-5-5", "Claude Opus 5.5"),
        ("claude-opus-5", "Claude Opus 5,"),
        ("claude-opus-4-8", "Claude Opus 4.8"),
        ("claude-sonnet-5-5", "Claude Sonnet 5.5"),
        ("claude-haiku-5-5", "Claude Haiku 5.5"),
    ];
    let source = quote("inline_tools");
    assert!(source.contains("tool changes are in beta on the same models"));
    for (id, row) in described() {
        let named = listed.iter().find(|(model, _)| *model == id);
        assert_eq!(row.inline_tools, named.is_some(), "{id}");
        if let Some((_, words)) = named {
            assert!(source.contains(words), "{id}");
            assert!(source.contains("inline-tools-2026-09-15"), "{id}");
        }
    }
    assert!(!row("claude-sonnet-5").unwrap().inline_tools);
    assert!(!row("claude-opus-4-7").unwrap().inline_tools);
}

#[test]
fn deferred_tools_match_their_quoted_source() {
    let page = "https://platform.claude.com/docs/en/agents-and-tools/tool-use/tool-search-tool";
    let mut pages = quote("pages").split(" · ");
    assert!(pages.any(|s| s.starts_with(page) && s.contains("2026-11-10")));
    let listed = [
        ("claude-fable-5-2", "Claude Fable 5.2"),
        ("claude-fable-5-1", "Claude Fable 5.1"),
        ("claude-mythos-5-1", "Claude Mythos 5.1"),
        ("claude-fable-5", "Claude Fable 5,"),
        ("claude-mythos-5", "Claude Mythos 5,"),
        ("claude-opus-5-5", "Claude Opus 5.5"),
        ("claude-opus-5", "Claude Opus 5,"),
        ("claude-opus-4-8", "Claude Opus 4.8"),
        ("claude-opus-4-7", "Claude Opus 4.7"),
        ("claude-opus-4-6", "Claude Opus 4.6"),
        ("claude-opus-4-5-20251101", "Claude Opus 4.5"),
        ("claude-opus-4-5", "Claude Opus 4.5"),
        ("claude-sonnet-5-5", "Claude Sonnet 5.5"),
        ("claude-sonnet-5", "Claude Sonnet 5,"),
        ("claude-sonnet-4-6", "Claude Sonnet 4.6"),
        ("claude-sonnet-4-5-20250929", "Claude Sonnet 4.5"),
        ("claude-sonnet-4-5", "Claude Sonnet 4.5"),
        ("claude-haiku-5-5", "Claude Haiku 5.5"),
        ("claude-haiku-4-5-20251001", "Claude Haiku 4.5"),
        ("claude-haiku-4-5", "Claude Haiku 4.5"),
    ];
    let source = quote("deferred_tools");
    assert!(source.starts_with("Tool search tool (read 2026-11-10)"));
    assert!(
        source.contains("Claude Opus 4.1 and earlier models don't support the tool search tool")
    );
    for (id, row) in described() {
        let named = listed.iter().find(|(model, _)| *model == id);
        assert_eq!(row.deferred_tools, named.is_some(), "{id}");
        if let Some((_, words)) = named {
            assert!(source.contains(words), "{id}");
        }
    }
    assert!(row("claude-opus-4-1").is_none());
    assert!(!row("claude-opus-4-7").unwrap().inline_tools);
    assert!(!row("claude-sonnet-5").unwrap().inline_tools);
}

#[test]
fn a_listed_model_takes_the_lists_facts_and_the_tables_the_rest() {
    use crate::models::list::tests::{claude_entry, claude_wire};
    use serde_json::json;
    let mut opus = claude_entry("claude-opus-5-5", "opus", 1_000_000);
    opus["max_tokens"] = json!(200_000);
    opus["capabilities"]["thinking"]["types"]["adaptive"] = json!({ "supported": false });
    let mut opus_4_5 = claude_entry("claude-opus-4-5-20251101", "opus", 200_000);
    opus_4_5["max_tokens"] = json!(64_000);
    opus_4_5["capabilities"]["effort"]["high"] = json!({ "supported": false });
    opus_4_5["capabilities"]["image_input"] = json!({ "supported": false });
    let entries = [
        claude_entry("claude-opus-6", "opus", 1_000_000),
        opus,
        opus_4_5,
    ];
    let list = crate::models::list::anthropic::parse(&claude_wire(&entries)).unwrap();
    let listed = |id| listed(Some(&list), id);
    let opus = ModelCapabilities {
        adaptive_thinking: false,
        max_output: Some(200_000),
        ..*row("claude-opus-5-5").unwrap()
    };
    assert_eq!(listed("claude-opus-5-5"), Some(opus));
    let opus_4_5 = ModelCapabilities {
        efforts: &[Effort::Low, Effort::Medium, Effort::Xhigh, Effort::Max],
        default_effort: None,
        adaptive_thinking: true,
        max_output: Some(64_000),
        images: None,
        ..*row("claude-opus-4-5").unwrap()
    };
    assert_eq!(listed("claude-opus-4-5"), Some(opus_4_5));
    let only_listed = ModelCapabilities {
        efforts: &Effort::ALL,
        adaptive_thinking: true,
        max_output: Some(128_000),
        ..LISTED_ONLY
    };
    assert_eq!(listed("claude-opus-6"), Some(only_listed));
    assert_eq!(LISTED_ONLY.images, row("claude-opus-4-5").unwrap().images);
    let shipped = |id| row(id).copied();
    assert_eq!(listed("claude-sonnet-5-5"), shipped("claude-sonnet-5-5"));
    assert_eq!(
        super::listed(None, "claude-opus-5-5"),
        shipped("claude-opus-5-5")
    );
    assert_eq!(super::listed(None, "claude-opus-6"), None);
    assert_eq!(listed("claude-opus-9"), None);
}
