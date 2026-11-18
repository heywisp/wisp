use std::io::Cursor;

use image::{DynamicImage, ImageFormat};
use wisp_protocol::ImageType;
use wisp_protocol::settings::Percent;

use super::*;
use crate::pricing::model_info;

fn opus() -> &'static ModelInfo {
    model_info("claude-opus-5-5").unwrap()
}

fn haiku() -> &'static ModelInfo {
    model_info("claude-haiku-5-5").unwrap()
}

fn fixed(prefix_warm: bool, tail_warm: bool) -> CompactionCostInput {
    CompactionCostInput {
        prefix_tokens: 5_000,
        warm_tokens: 845_000,
        after_anchor_tokens: 0,
        summary_tokens: 6_000,
        head_tokens: 15_000,
        head_ttl: CacheTtl::OneHour,
        prefix_warm,
        tail_warm,
    }
}

fn micros(tokens: u64, per_mtok: f64) -> f64 {
    tokens as f64 * per_mtok
}

#[test]
fn a_warm_compaction_reads_the_conversation() {
    let cost = compaction_cost(&fixed(true, true), opus());
    assert_eq!(cost.estimate_micros, 290_000);
    assert_eq!(cost.head_write_micros, 120_000);
}

#[test]
fn a_cold_tail_pays_plain_input_for_the_conversation() {
    let cost = compaction_cost(&fixed(true, false), opus());
    assert_eq!(cost.estimate_micros, 3_501_000);
    assert_eq!(cost.head_write_micros, 120_000);
}

#[test]
fn an_expired_hour_writes_the_system_and_tools_again() {
    let cost = compaction_cost(&fixed(false, false), opus());
    assert_eq!(cost.estimate_micros, 3_540_000);
    assert_eq!(cost.head_write_micros, 120_000);
}

#[test]
fn the_head_ttl_prices_only_the_head_write() {
    for (prefix, tail) in [(true, true), (true, false), (false, false)] {
        let hour = compaction_cost(&fixed(prefix, tail), opus());
        let five = compaction_cost(
            &CompactionCostInput {
                head_ttl: CacheTtl::FiveMinutes,
                ..fixed(prefix, tail)
            },
            opus(),
        );
        assert_eq!(five.head_write_micros, 75_000);
        assert_eq!(hour.head_write_micros, 120_000);
        assert_eq!(five.estimate_micros, hour.estimate_micros);
    }
}

#[test]
fn content_after_the_anchor_is_plain_input() {
    let rates = opus().rates;
    let base = compaction_cost(&fixed(true, true), opus());
    let more = compaction_cost(
        &CompactionCostInput {
            after_anchor_tokens: 1_200,
            ..fixed(true, true)
        },
        opus(),
    );
    assert_eq!(
        (more.estimate_micros - base.estimate_micros) as f64,
        micros(1_200, rates.input_per_mtok)
    );
}

#[test]
fn every_figure_comes_from_pricing() {
    for info in [opus(), model_info("claude-sonnet-5-5").unwrap()] {
        let rates = info.rates;
        let cost = compaction_cost(&fixed(true, false), info);
        let expected = micros(5_000, rates.cache_read_per_mtok)
            + micros(845_000, rates.input_per_mtok)
            + micros(6_000, rates.output_per_mtok);
        assert_eq!(cost.estimate_micros, expected.round() as u64);
        let head = micros(15_000, rates.input_per_mtok) * info.write_multiplier(CacheTtl::OneHour);
        assert_eq!(cost.head_write_micros, head.round() as u64);
    }
}

#[test]
fn haiku_reads_a_long_prompt_at_the_long_rates_and_writes_the_head_at_the_short_ones() {
    let info = haiku();
    let tier = info.long_prompt.unwrap();
    let cost = compaction_cost(&fixed(true, true), info);
    let long = tier.rates;
    let expected = micros(5_000, long.cache_read_per_mtok)
        + micros(845_000, long.cache_read_per_mtok)
        + micros(6_000, long.output_per_mtok);
    assert_eq!(cost.estimate_micros, expected.round() as u64);
    let short = info.rates;
    let head = micros(15_000, short.input_per_mtok) * info.write_multiplier(CacheTtl::OneHour);
    assert_eq!(cost.head_write_micros, head.round() as u64);
    assert_ne!(long.input_per_mtok, short.input_per_mtok);
}

fn start() -> Timestamp {
    Timestamp::from_second(1_800_000_000).unwrap()
}

fn anchor(ttl: CacheTtl) -> Anchor {
    Anchor {
        used: 67_400,
        output_tokens: 700,
        started_at: Some(start()),
        ttl: Some(ttl),
        same_pair: true,
    }
}

const HEAD_TOKENS: u64 = 15_000;
const INSTRUCTION_TOKENS: u64 = 500;

fn now_after(minutes: i64, head_ttl: CacheTtl) -> CompactNow {
    CompactNow {
        prefix_tokens: 5_000,
        content_since: 0,
        next_estimate: 68_100,
        head_tokens: HEAD_TOKENS,
        instruction_tokens: INSTRUCTION_TOKENS,
        head_ttl,
        now: start() + SignedDuration::from_mins(minutes),
    }
}

#[test]
fn the_anchor_prices_warm_within_its_ttl() {
    let input = compact_now_input(
        Some(&anchor(CacheTtl::OneHour)),
        &now_after(10, CacheTtl::OneHour),
    );
    assert!(input.prefix_warm && input.tail_warm);
    let input = compact_now_input(
        Some(&anchor(CacheTtl::FiveMinutes)),
        &now_after(4, CacheTtl::OneHour),
    );
    assert!(input.prefix_warm && input.tail_warm);
}

#[test]
fn past_its_ttl_but_within_the_hour_the_tail_is_cold() {
    let input = compact_now_input(
        Some(&anchor(CacheTtl::FiveMinutes)),
        &now_after(10, CacheTtl::OneHour),
    );
    assert!(input.prefix_warm);
    assert!(!input.tail_warm);
}

#[test]
fn past_the_hour_both_are_cold() {
    let input = compact_now_input(
        Some(&anchor(CacheTtl::OneHour)),
        &now_after(61, CacheTtl::OneHour),
    );
    assert!(!input.prefix_warm && !input.tail_warm);
}

#[test]
fn a_restored_anchor_without_its_time_or_ttl_is_cold() {
    let mut restored = anchor(CacheTtl::OneHour);
    restored.started_at = None;
    let input = compact_now_input(Some(&restored), &now_after(1, CacheTtl::OneHour));
    assert!(!input.prefix_warm && !input.tail_warm);
    let mut restored = anchor(CacheTtl::OneHour);
    restored.ttl = None;
    let input = compact_now_input(Some(&restored), &now_after(1, CacheTtl::OneHour));
    assert!(!input.prefix_warm && !input.tail_warm);
}

#[test]
fn with_no_request_yet_the_conversation_is_the_next_estimate_less_the_prefix() {
    let input = compact_now_input(None, &now_after(0, CacheTtl::OneHour));
    assert_eq!(input.warm_tokens, 68_100 - 5_000);
    assert_eq!(input.after_anchor_tokens, INSTRUCTION_TOKENS);
    assert!(!input.prefix_warm && !input.tail_warm);
}

#[test]
fn after_a_switch_the_old_sizes_are_priced_cold() {
    let mut other = anchor(CacheTtl::OneHour);
    other.same_pair = false;
    let input = compact_now_input(Some(&other), &now_after(1, CacheTtl::OneHour));
    assert!(!input.prefix_warm && !input.tail_warm);
    assert_eq!(input.warm_tokens, 62_400);
    assert_eq!(input.after_anchor_tokens, 700 + INSTRUCTION_TOKENS);
}

#[test]
fn the_conversation_is_never_negative() {
    let mut early = anchor(CacheTtl::OneHour);
    early.used = 3_000;
    let input = compact_now_input(Some(&early), &now_after(1, CacheTtl::OneHour));
    assert_eq!(input.warm_tokens, 0);
}

#[test]
fn context_figures_use_the_stated_estimates() {
    let input = compact_now_input(
        Some(&anchor(CacheTtl::OneHour)),
        &now_after(1, CacheTtl::OneHour),
    );
    assert_eq!(
        input,
        CompactionCostInput {
            prefix_tokens: 5_000,
            warm_tokens: 62_400,
            after_anchor_tokens: 700 + INSTRUCTION_TOKENS,
            summary_tokens: SUMMARY_TOKENS_ESTIMATE,
            head_tokens: HEAD_TOKENS,
            head_ttl: CacheTtl::OneHour,
            prefix_warm: true,
            tail_warm: true,
        }
    );
    let cost = compaction_cost(&input, opus());
    assert_eq!(cost.estimate_micros, 1_000 + 12_480 + 4_800 + 120_000);
    assert_eq!(usd(cost.head_write_micros), 0.12);
    let five = compact_now_input(
        Some(&anchor(CacheTtl::OneHour)),
        &now_after(1, CacheTtl::FiveMinutes),
    );
    assert_eq!(usd(compaction_cost(&five, opus()).head_write_micros), 0.075);
}

#[test]
fn rereading_the_fill_is_priced_at_the_cache_read_rate() {
    assert_eq!(reread_micros(62_400, opus()), 12_480);
    assert_eq!(usd(reread_micros(62_400, opus())), 0.01248);
}

#[test]
fn compact_at_by_the_default_rule() {
    let point = compact_at(1_000_000, 64_000, None);
    assert_eq!(point.tokens, 850_000);
    assert_eq!(point.set_by, PointSource::Default);
    assert_eq!(point.capped_from, None);
    assert_eq!(compact_at(200_000, 64_000, None).tokens, 136_000);
}

#[test]
fn a_configured_point_as_a_share_or_tokens() {
    let share = CompactAt::parse("60%").unwrap();
    let point = compact_at(1_000_000, 64_000, Some((share, PointSource::Endpoint)));
    assert_eq!(point.tokens, 600_000);
    assert_eq!(point.set_by, PointSource::Endpoint);
    let tokens = CompactAt::parse("400000").unwrap();
    assert_eq!(
        compact_at(1_000_000, 64_000, Some((tokens, PointSource::Config))).tokens,
        400_000
    );
    assert_eq!(
        compact_at(200_000, 64_000, Some((share, PointSource::Config))).tokens,
        120_000
    );
}

#[test]
fn a_point_past_the_reserve_is_clamped() {
    let share = CompactAt::parse("95%").unwrap();
    let point = compact_at(1_000_000, 64_000, Some((share, PointSource::Config)));
    assert_eq!(point.tokens, 850_000);
    assert_eq!(point.capped_from, Some(950_000));
    let tokens = CompactAt::Tokens(190_000);
    let point = compact_at(200_000, 64_000, Some((tokens, PointSource::Config)));
    assert_eq!(point.tokens, 136_000);
    assert_eq!(point.capped_from, Some(190_000));
}

#[test]
fn the_rounding_never_puts_the_point_late() {
    assert_eq!(compact_at(333_333, 32_000, None).tokens, 283_333);
    let share = CompactAt::parse("85%").unwrap();
    assert_eq!(
        compact_at(333_333, 32_000, Some((share, PointSource::Config))).tokens,
        283_333
    );
}

#[test]
fn compact_at_parses_tokens_and_shares_only() {
    assert_eq!(
        CompactAt::parse("85%"),
        Some(CompactAt::Share(Percent::new(85).unwrap()))
    );
    assert_eq!(
        CompactAt::parse("0%"),
        Some(CompactAt::Share(Percent::new(0).unwrap()))
    );
    assert_eq!(CompactAt::parse("10000"), Some(CompactAt::Tokens(10_000)));
    assert_eq!(CompactAt::parse("400k"), Some(CompactAt::Tokens(400_000)));
    for bad in ["101%", "-5", "85.5%", "%", "", "1e6", "+85%"] {
        assert_eq!(CompactAt::parse(bad), None, "{bad}");
    }
    assert_eq!(CompactAt::parse("85%").unwrap().to_string(), "85%");
    assert_eq!(CompactAt::Tokens(400_000).to_string(), "400000");
}

#[test]
fn a_limit_below_the_floor_is_refused_the_same_for_every_model() {
    let error = check_limit("my-model", 100_000, 64_000).unwrap_err();
    assert_eq!(
        error.to_string(),
        "context_window 100000 for my-model puts the compaction point at 36,000 (window \u{2212} reserve 64,000); it must be at least 40,000. Set at least 104000."
    );
    assert_eq!(check_limit("claude-opus-5-5", 104_000, 64_000), Ok(()));
    assert_eq!(compact_at(104_000, 64_000, None).tokens, 40_000);
    assert!(check_limit("claude-opus-5-5", 103_999, 64_000).is_err());
    assert_eq!(settings::smallest_window(POINT_FLOOR, 64_000), 104_000);
    assert_eq!(settings::smallest_window(POINT_FLOOR, 0), 80_000);
    assert_eq!(settings::smallest_window(POINT_FLOOR, 32_000), 80_000);
}

#[test]
fn a_limit_above_the_documented_window_is_warned_about() {
    let page = "https://platform.claude.com/docs/en/build-with-claude/context-windows";
    assert_eq!(
        above_documented("claude-opus-5-5", 2_000_000, 1_000_000, page).unwrap(),
        format!(
            "context_window 2000000 for claude-opus-5-5 is above the documented window of 1M ({page}); requests may fail with a context overflow"
        )
    );
    assert_eq!(
        above_documented("claude-opus-5-5", 400_000, 1_000_000, page),
        None
    );
    assert_eq!(
        above_documented("claude-opus-5-5", 1_000_000, 1_000_000, page),
        None
    );
}

fn image(width: u32, height: u32) -> ContentBlock {
    let mut bytes = Vec::new();
    DynamicImage::new_rgb8(width, height)
        .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
        .unwrap();
    ContentBlock::Image {
        source: ImageSource::Base64 {
            media_type: ImageType::Png,
            data: wisp_media::encode_base64(&bytes).into(),
        },
    }
}

#[test]
fn text_is_three_bytes_a_token_rounded_up() {
    assert_eq!(text_tokens(""), 0);
    assert_eq!(text_tokens("ab"), 1);
    assert_eq!(text_tokens("abc"), 1);
    assert_eq!(text_tokens("abcd"), 2);
}

#[test]
fn the_estimate_with_text_tool_results_and_images() {
    let text = ContentBlock::Text {
        text: "x".repeat(300),
    };
    let result = ContentBlock::ToolResult {
        tool_use_id: "toolu_1".into(),
        content: vec![
            ContentBlock::Text {
                text: "y".repeat(90),
            },
            image(280, 56),
        ],
        is_error: false,
    };
    assert_eq!(block_tokens(&text), 100);
    assert_eq!(block_tokens(&image(280, 56)), 10 * 2);
    assert_eq!(block_tokens(&result), 30 + 20);
    assert_eq!(
        next_estimate(60_000, 700, &[text, result, image(28, 28)]),
        60_000 + 700 + 100 + 50 + 1
    );
}

#[test]
fn an_unreadable_image_is_estimated_high() {
    let broken = ContentBlock::Image {
        source: ImageSource::Base64 {
            media_type: ImageType::Png,
            data: "not base64!".into(),
        },
    };
    assert_eq!(block_tokens(&broken), UNREADABLE_IMAGE_TOKENS);
}

#[test]
fn effort_markers_add_nothing() {
    let marker = ContentBlock::Effort {
        level: wisp_protocol::Effort::High,
        rendered: true,
    };
    assert_eq!(block_tokens(&marker), 0);
}

#[test]
fn the_flag_point_beats_the_tables_and_the_session_beats_the_flag() {
    let text = "compact_at = \"30%\"\n\n[models.\"claude-opus-5-5\"]\ncompact_at = 600000\n\n[endpoints.gw.models.\"claude-opus-5-5\"]\ncompact_at = \"60%\"\n";
    let config = ConfigModels::parse(text).unwrap();
    let shipped = crate::models::ModelTable::shipped().unwrap();
    let models = ModelSettings::new(shipped, config);
    let opus = "claude-opus-5-5";
    assert_eq!(
        models.resolve("gw", opus, None, 64_000).compact_at,
        Some((CompactAt::parse("60%").unwrap(), PointSource::Endpoint))
    );
    assert_eq!(
        models.resolve("env", opus, None, 64_000).compact_at,
        Some((CompactAt::Tokens(600_000), PointSource::Config))
    );
    assert_eq!(
        models.resolve("env", "my-model", None, 64_000).compact_at,
        Some((CompactAt::parse("30%").unwrap(), PointSource::Config))
    );
    let mut flagged = models.with_flags(&[place_flag("compact_at", "40%")]);
    for endpoint in ["gw", "env"] {
        assert_eq!(
            flagged.resolve(endpoint, opus, None, 64_000).compact_at,
            Some((CompactAt::parse("40%").unwrap(), PointSource::Flag))
        );
    }
    assert_eq!(
        flagged.resolve("env", "my-model", None, 64_000).compact_at,
        Some((CompactAt::parse("40%").unwrap(), PointSource::Flag))
    );
    let own = place_flag("models.\"claude-opus-5-5\".compact_at", "300k");
    flagged.set_session(vec![own.clone()]);
    assert_eq!(
        flagged.resolve("gw", opus, None, 64_000).compact_at,
        Some((CompactAt::Tokens(300_000), PointSource::Session))
    );
    let point = compact_at(
        1_000_000,
        64_000,
        flagged.resolve("env", "my-model", None, 64_000).compact_at,
    );
    assert_eq!((point.tokens, point.set_by), (400_000, PointSource::Flag));
    let past = compact_at(
        1_000_000,
        64_000,
        Some((CompactAt::Tokens(990_000), PointSource::Flag)),
    );
    assert_eq!((past.tokens, past.capped_from), (850_000, Some(990_000)));
    flagged.set_session(vec![place_flag("compact_at", "250k"), own]);
    assert_eq!(
        flagged.resolve("gw", opus, None, 64_000).compact_at,
        Some((CompactAt::Tokens(300_000), PointSource::Session))
    );
    assert_eq!(
        flagged.resolve("gw", "my-model", None, 64_000).compact_at,
        Some((CompactAt::Tokens(250_000), PointSource::Session))
    );
}

#[test]
fn a_fork_keeps_the_view_and_appends_its_suffix_as_the_next_user_content() {
    let reply = Message {
        role: wisp_protocol::Role::Assistant,
        content: vec![ContentBlock::Text {
            text: "Done.".into(),
        }],
    };
    let view = vec![Message::user_text("go"), reply];
    let ask = |marker| Appended {
        user: vec![ContentBlock::Text {
            text: "Summarize.".into(),
        }],
        marker,
    };
    let forked = fork_request(&view, ask(None));
    assert_eq!(forked[..2], view[..]);
    assert_eq!(forked[2], Message::user_text("Summarize."));
    assert_eq!(forked.len(), 3);
    let marker = EffortMarker {
        level: wisp_protocol::Effort::High,
        rendered: true,
    };
    let marked = fork_request(&view, ask(Some(marker)));
    assert_eq!(marked[..3], forked[..]);
    assert_eq!(marked[3], Message::effort(marker));
    let results = [
        Message::user_text("go"),
        Message {
            role: wisp_protocol::Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "t1".into(),
                content: vec![],
                is_error: false,
            }],
        },
    ];
    let joined = fork_request(&results[1..], ask(None));
    assert_eq!(joined.len(), 1);
    assert_eq!(joined[0].content.len(), 2);
    assert_eq!(joined[0].content[0], results[1].content[0]);
}

#[test]
fn a_key_as_the_model_is_never_named_in_a_window_message() {
    let key = "sk-ant-api03-CANARYMODEL0000";
    let small = check_limit(key, 100_000, 64_000).unwrap_err().to_string();
    assert!(
        small.starts_with("context_window 100000 for this model puts"),
        "{small}"
    );
    let above = above_documented(key, 2_000_000, 1_000_000, "page").unwrap();
    assert!(
        above.starts_with("context_window 2000000 for this model is above"),
        "{above}"
    );
    let images = crate::agent::no_image_sentence(key);
    assert!(
        images.starts_with(&format!("{} can't take images", wisp_protocol::NOT_SHOWN)),
        "{images}"
    );
    assert!(!format!("{small}{above}{images}").contains("CANARY"));
}

#[test]
fn a_point_below_the_floor_warns_with_the_price_of_a_compaction_each_turn() {
    let opus = (
        "claude-opus-5-5",
        crate::pricing::model_info("claude-opus-5-5"),
    );
    let tokens = CompactAt::Tokens(10_000);
    assert_eq!(
        low_point_warning(tokens, 10_000, opus, CacheTtl::OneHour).as_deref(),
        Some(
            "compact_at 10000 is below 40,000: once the head after a compaction is above it, every turn starts with a compaction, ~$0.123 a turn to compact and ~$0.12 to write the head again, at claude-opus-5-5's prices."
        )
    );
    let five = low_point_warning(tokens, 10_000, opus, CacheTtl::FiveMinutes).unwrap();
    assert!(
        five.contains("~$0.123 a turn to compact and ~$0.075 to write"),
        "{five}"
    );
    assert_eq!(
        low_point_warning(CompactAt::Tokens(40_000), 40_000, opus, CacheTtl::OneHour),
        None
    );
    let share = CompactAt::parse("5%").unwrap();
    let unpriced = low_point_warning(share, 10_000, ("my-model", None), CacheTtl::OneHour).unwrap();
    assert!(
        unpriced.starts_with("compact_at 5% is below 40,000:"),
        "{unpriced}"
    );
    assert!(
        unpriced.ends_with("every turn starts with a compaction."),
        "{unpriced}"
    );
}

#[test]
fn a_low_point_from_the_flag_or_a_table_is_named_with_its_price() {
    let text = "[models.\"claude-opus-5-5\"]\ncompact_at = 30000\n\n[models.\"my-model\"]\ncompact_at = \"2%\"\n\n[endpoints.gw.models.\"claude-sonnet-5-5\"]\ncompact_at = 20000\n";
    let config = ConfigModels::parse(text).unwrap();
    let shipped = crate::models::ModelTable::shipped().unwrap();
    let models = ModelSettings::new(shipped, config);
    let ttl = CacheTtl::OneHour;
    let warnings = models.low_points("env", "claude-opus-5-5", &|_, _| 64_000, ttl, &none, &none);
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(
        warnings[0].starts_with("compact_at 30000 is below 40,000:")
            && warnings[0].ends_with("at claude-opus-5-5's prices."),
        "{warnings:?}"
    );
    assert!(
        warnings[1].starts_with("compact_at 20000 is below 40,000:"),
        "{warnings:?}"
    );
    assert!(
        warnings[1].ends_with("at claude-sonnet-5-5's prices."),
        "{warnings:?}"
    );
    let flagged = models.with_flags(&[place_flag("compact_at", "10000")]);
    let warnings = flagged.low_points("env", "claude-opus-5-5", &|_, _| 64_000, ttl, &none, &none);
    assert_eq!(warnings.len(), 3, "{warnings:?}");
    assert_eq!(
        warnings[0],
        "compact_at 10000 is below 40,000: once the head after a compaction is above it, every turn starts with a compaction, ~$0.123 a turn to compact and ~$0.12 to write the head again, at claude-opus-5-5's prices."
    );
    let everywhere = ModelSettings::new(
        crate::models::ModelTable::shipped().unwrap(),
        ConfigModels::parse("compact_at = 10000\n").unwrap(),
    );
    let warnings =
        everywhere.low_points("env", "claude-opus-5-5", &|_, _| 64_000, ttl, &none, &none);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].starts_with("compact_at 10000 is below 40,000:"),
        "{warnings:?}"
    );
    let quiet = ModelSettings::shipped_only().with_flags(&[place_flag("compact_at", "40000")]);
    assert!(
        quiet
            .low_points("env", "claude-opus-5-5", &|_, _| 64_000, ttl, &none, &none)
            .is_empty()
    );
}

fn none<T>(_: &str, _: &str) -> Option<T> {
    None
}

fn place_flag(target: &str, value: &str) -> Given {
    let target = wisp_protocol::settings::parse_target(target).unwrap();
    let value = target.spec().parse(value).unwrap();
    Given {
        target,
        value,
        origin: "--set".to_owned(),
    }
}

#[test]
fn a_subscriptions_list_row_gives_the_window_search_and_points_of_its_own_account() {
    use crate::models::list::tests;
    use WindowSource::{Shipped, Subscription};
    use serde_json::json;

    let wire = ProviderKind::OpenAi;
    let sample = tests::sample();
    let row = |model: &str| ListedRow::of(Some(&sample), wire, Billing::ChatGptPlan, model);
    let (sol_id, new_id) = ("gpt-6.1-sol", "gpt-new");
    let (sol, new) = (row(sol_id).unwrap(), row(new_id).unwrap());
    let models = ModelSettings::shipped_only();
    let window = models
        .resolve("chatgpt", sol_id, Some(&sol), 64_000)
        .window
        .unwrap();
    let listed = (window.tokens, window.source, window.documented);
    assert_eq!(listed, (500_000, Subscription, None));
    let api = ListedRow::of(Some(&sample), wire, Billing::Metered, sol_id).unwrap();
    let window = models
        .resolve("claude-api", sol_id, Some(&api), 64_000)
        .window;
    let window = window.unwrap();
    assert_eq!((window.tokens, window.source), (500_000, WindowSource::Api));
    let metered = ListedRow::of(None, wire, Billing::Metered, sol_id);
    let shipped = models
        .resolve("api", sol_id, metered.as_ref(), 64_000)
        .window
        .unwrap();
    assert_eq!((shipped.tokens, shipped.source), (1_050_000, Shipped));
    let points = crate::test_guard::models("compact_at = 400000\n");
    let astra = "gpt-6.2-astra";
    let claude = "claude-opus-5-5";
    let sub = |window, point| Some((window, Subscription, point));
    let ship = |window, point| Some((window, Shipped, point));
    let listed = || sub(272_000, 208_000);
    let fallback = || ship(272_000, 208_000);
    for (id, default, max, old, expected) in [
        (
            astra,
            Some(272_000),
            Some(872_000),
            None,
            sub(470_589, 400_000),
        ),
        (
            astra,
            Some(500_000),
            Some(500_000),
            None,
            sub(500_000, 400_000),
        ),
        (
            astra,
            Some(128_000),
            Some(128_000),
            None,
            sub(128_000, 64_000),
        ),
        (astra, None, Some(128_000), None, sub(128_000, 64_000)),
        (astra, None, Some(160_000), None, sub(160_000, 96_000)),
        (
            astra,
            Some(200_000),
            Some(160_000),
            None,
            sub(160_000, 96_000),
        ),
        (astra, None, Some(872_000), None, fallback()),
        (astra, Some(272_000), Some(0), None, listed()),
        (astra, Some(272_000), Some(32_000), None, listed()),
        (astra, Some(32_000), Some(872_000), None, fallback()),
        (astra, None, Some(0), None, fallback()),
        (astra, None, None, Some(872_000), fallback()),
        (astra, None, None, Some(272_000), listed()),
        (astra, None, None, None, fallback()),
        (new_id, None, Some(872_000), None, fallback()),
        (new_id, Some(32_000), Some(872_000), None, fallback()),
        (new_id, None, None, None, None),
        (claude, None, None, None, ship(1_000_000, 400_000)),
    ] {
        let fixture = if let Some(old) = old {
            Some(
                serde_json::from_value(json!([{
                    "id": id, "shown": true, "window": old,
                    "levels": [], "images": false, "search": false
                }]))
                .unwrap(),
            )
        } else if default.is_some() || max.is_some() {
            let mut entry = tests::entry(id, 1, 0);
            entry["context_window"] = json!(default);
            entry["max_context_window"] = json!(max);
            Some(ModelList::parse(&tests::wire(&[entry])).unwrap())
        } else {
            None
        };
        let list = fixture.as_ref().or((id == claude).then_some(&sample));
        let pick = |billing| ListedRow::of(list, wire, billing, id);
        let chatgpt = pick(Billing::ChatGptPlan);
        if id == claude {
            assert!(chatgpt.is_none());
        }
        let resolved = points.resolve("chatgpt", id, chatgpt.as_ref(), 64_000);
        let actual = resolved.window.map(|w| {
            let point = compact_at(w.tokens, 64_000, resolved.compact_at);
            (w.tokens, w.source, point.tokens)
        });
        let label = format!("{id} {default:?}/{max:?}, old {old:?}");
        assert_eq!(actual, expected, "{label}");
        let entry = list.and_then(|list| list.find(id));
        for (billing, source) in [
            (Billing::Metered, WindowSource::Api),
            (Billing::AnthropicPlan, Subscription),
            (Billing::AntigravityPlan, Subscription),
        ] {
            let legacy = entry.map(|entry| ListedRow {
                row: entry.row(wire),
                source,
                max_window: None,
            });
            assert_eq!(pick(billing), legacy, "{billing:?}: {label}");
        }
    }
    let list = ModelList::parse(&tests::wire(&[tests::entry(astra, 1, 272_000)])).unwrap();
    let sub = ListedRow::of(Some(&list), wire, Billing::ChatGptPlan, astra);
    let api = ListedRow::of(Some(&list), wire, Billing::Metered, astra);
    let show = |settings: &ModelSettings, row: Option<&ListedRow>, max| {
        let resolved = settings.resolve("chatgpt", astra, row, max);
        let window = resolved.window.unwrap();
        let point = compact_at(window.tokens, max, resolved.compact_at);
        (
            window.tokens,
            window.source,
            window.documented,
            point.tokens,
            point.capped_from,
        )
    };
    let config = |at: &str| crate::test_guard::models(&format!("compact_at = {at}\n"));
    let subscription = |settings: &ModelSettings, max| show(settings, sub.as_ref(), max);
    let mut small = config("150000");
    assert_eq!(
        subscription(&small, 64_000),
        (272_000, Subscription, None, 150_000, None)
    );
    assert_eq!(
        subscription(&config("900000"), 64_000),
        (872_000, Subscription, Some(272_000), 741_200, Some(900_000))
    );
    assert_eq!(
        subscription(&points, 128_000),
        (528_000, Subscription, Some(272_000), 400_000, None)
    );
    assert_eq!(
        subscription(&config("\"60%\""), 64_000),
        (272_000, Subscription, None, 163_200, None)
    );
    let fixed = crate::test_guard::models(
        "compact_at = 400000\n[models.\"gpt-6.2-astra\"]\ncontext_window = 300000\n",
    );
    assert_eq!(
        subscription(&fixed, 64_000),
        (
            300_000,
            WindowSource::Config,
            Some(272_000),
            236_000,
            Some(400_000)
        )
    );
    assert_eq!(
        show(&points, api.as_ref(), 64_000),
        (272_000, WindowSource::Api, None, 208_000, Some(400_000))
    );
    let astra_row = sub.as_ref();
    assert!(small.note_overflow("chatgpt", astra, 450_000, astra_row));
    assert_eq!(
        small.window_room("chatgpt", astra, astra_row),
        Some(WindowRoom {
            base: 272_000,
            max: 450_000
        })
    );
    small.set_session(vec![place_flag("compact_at", "600000")]);
    assert_eq!(
        subscription(&small, 64_000),
        (
            450_000,
            WindowSource::Overflow,
            Some(272_000),
            382_500,
            Some(600_000)
        )
    );
    assert!(!small.note_overflow("chatgpt", astra, 500_000, astra_row));
    assert!(small.note_overflow("chatgpt", astra, 200_000, astra_row));
    assert_eq!(small.window_room("chatgpt", astra, astra_row), None);

    let new_window = |row| models.resolve("chatgpt", new_id, row, 64_000).window;
    assert_eq!(new_window(Some(&new)).map(|w| w.tokens), Some(272_000));
    assert_eq!(new_window(None), None);
    let text = "[models.\"gpt-6.1-sol\"]\ncontext_window = 600000\n\n[endpoints.chatgpt.models.\"gpt-new\"]\ncompact_at = \"10%\"\n";
    let config = crate::test_guard::models(text);
    let mine = config
        .resolve("chatgpt", sol_id, Some(&sol), 64_000)
        .window
        .unwrap();
    let shown = (mine.tokens, mine.source, mine.documented);
    assert_eq!(shown, (600_000, WindowSource::Config, Some(500_000)));
    let mut lowered = models.clone();
    assert!(!lowered.note_overflow("chatgpt", sol_id, 900_000, Some(&sol)));
    assert!(lowered.note_overflow("chatgpt", sol_id, 450_000, Some(&sol)));
    let search = |row| crate::search::SearchFacts::resolve(&models, "chatgpt", sol_id, wire, row);
    assert_eq!(search(Some(&sol)).versions, [crate::search::WEB_SEARCH]);
    let mut silent = tests::entry(sol_id, 1, 500_000);
    silent["supports_search_tool"] = serde_json::json!(false);
    let silent = ModelList::parse(&tests::wire(&[silent])).unwrap();
    let off = ListedRow::of(Some(&silent), wire, Billing::ChatGptPlan, "gpt-6.1-sol").unwrap();
    assert!(search(Some(&off)).versions.is_empty());
    let lookup = |account: &str, model: &str| (account == "chatgpt").then(|| row(model)).flatten();
    let ttl = CacheTtl::OneHour;
    assert!(
        config
            .low_points("env", sol_id, &|_, _| 64_000, ttl, &none, &none)
            .is_empty()
    );
    let warned = config.low_points("env", sol_id, &|_, _| 64_000, ttl, &none, &lookup);
    assert!(
        warned[0].starts_with("compact_at 10% is below 40,000"),
        "{warned:?}"
    );
}

#[test]
fn per_place_flags_beat_the_tables_for_one_run_and_the_session_beats_them() {
    let text = "[models.\"claude-opus-5-5\"]\ncontext_window = 500000\ncompact_at = 400000\nnative_compaction = false\n";
    let models = ModelSettings::new(
        crate::models::ModelTable::shipped().unwrap(),
        ConfigModels::parse(text).unwrap(),
    );
    let opus = "claude-opus-5-5";
    let mut flagged = models.clone().with_flags(&[
        place_flag("models.\"claude-opus-5-5\".context_window", "300000"),
        place_flag("endpoints.gw.models.\"claude-opus-5-5\".compact_at", "50%"),
        place_flag("models.\"claude-opus-5-5\".compact_at", "250000"),
        place_flag("models.\"claude-opus-5-5\".native_compaction", "on"),
    ]);
    let window = flagged.resolve("env", opus, None, 64_000).window.unwrap();
    assert_eq!(
        (window.tokens, window.source),
        (300_000, WindowSource::Flag)
    );
    assert_eq!(
        flagged.resolve("gw", opus, None, 64_000).compact_at,
        Some((CompactAt::parse("50%").unwrap(), PointSource::Flag))
    );
    assert_eq!(
        flagged.resolve("env", opus, None, 64_000).compact_at,
        Some((CompactAt::Tokens(250_000), PointSource::Flag))
    );
    assert!(flagged.native_compaction("env", opus));
    assert!(!models.native_compaction("env", opus));
    assert_eq!(
        flagged
            .resolve("env", "claude-haiku-5-5", None, 64_000)
            .window
            .unwrap()
            .source,
        WindowSource::Shipped
    );
    flagged.set_session(vec![place_flag(
        "models.\"claude-opus-5-5\".context_window",
        "400000",
    )]);
    assert_eq!(
        flagged
            .resolve("env", opus, None, 64_000)
            .window
            .unwrap()
            .source,
        WindowSource::Session
    );
}

#[test]
fn a_per_place_flag_point_below_the_floor_is_warned_about_like_a_saved_one() {
    let flagged = ModelSettings::shipped_only()
        .with_flags(&[place_flag("models.\"claude-opus-5-5\".compact_at", "10000")]);
    let warnings = flagged.low_points(
        "env",
        "claude-opus-5-5",
        &|_, _| 64_000,
        CacheTtl::OneHour,
        &none,
        &none,
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.starts_with("compact_at 10000 is below")),
        "{warnings:?}"
    );
    let above = ModelSettings::shipped_only().with_flags(&[place_flag(
        "models.\"claude-opus-5-5\".context_window",
        "5000000",
    )]);
    assert!(above.warnings().is_empty());
    assert_eq!(
        above.flag_warnings().len(),
        1,
        "{:?}",
        above.flag_warnings()
    );
}

#[test]
fn a_flag_meets_a_config_row_through_its_saved_alias() {
    let text = "[models.\"my-model\"]\naliases = [\"my-alias\"]\ncontext_window = 500000\n";
    for given in ["my-model", "my-alias"] {
        let models = ModelSettings::new(
            crate::models::ModelTable::shipped().unwrap(),
            ConfigModels::parse(text).unwrap(),
        )
        .with_flags(&[place_flag(
            &format!("models.\"{given}\".context_window"),
            "300000",
        )]);
        for model in ["my-model", "my-alias"] {
            let window = models.resolve("gw", model, None, 64_000).window.unwrap();
            assert_eq!(
                (window.tokens, window.source),
                (300_000, WindowSource::Flag),
                "{given} at {model}"
            );
        }
    }
}

#[test]
fn an_endpoint_and_model_flag_beats_a_model_flag_for_the_window() {
    let models = ModelSettings::shipped_only().with_flags(&[
        place_flag("models.\"claude-opus-5-5\".context_window", "300000"),
        place_flag(
            "endpoints.gw.models.\"claude-opus-5-5\".context_window",
            "200000",
        ),
    ]);
    assert_eq!(
        models
            .resolve("gw", "claude-opus-5-5", None, 64_000)
            .window
            .unwrap()
            .tokens,
        200_000
    );
    assert_eq!(
        models
            .resolve("env", "claude-opus-5-5", None, 64_000)
            .window
            .unwrap()
            .tokens,
        300_000
    );
}

#[test]
fn the_background_warning_uses_a_flagged_window_over_the_saved_one() {
    let text = "[models.\"claude-opus-5-5\"]\ncontext_window = 1000000\n";
    let models = ModelSettings::new(
        crate::models::ModelTable::shipped().unwrap(),
        ConfigModels::parse(text).unwrap(),
    );
    let band = |_: &str, _: &str| Some(BACKGROUND_TOKENS);
    let ttl = CacheTtl::OneHour;
    let saved = models.low_points("env", "claude-opus-5-5", &|_, _| 64_000, ttl, &band, &none);
    let flagged = models
        .with_flags(&[place_flag(
            "models.\"claude-opus-5-5\".context_window",
            "120000",
        )])
        .low_points("env", "claude-opus-5-5", &|_, _| 64_000, ttl, &band, &none);
    assert!(saved.is_empty(), "{saved:?}");
    let narrow = ModelSettings::new(
        crate::models::ModelTable::shipped().unwrap(),
        ConfigModels::parse("[models.\"claude-opus-5-5\"]\ncontext_window = 110000\n").unwrap(),
    );
    assert!(
        !narrow
            .low_points("env", "claude-opus-5-5", &|_, _| 64_000, ttl, &band, &none)
            .is_empty()
    );
    let widened = narrow
        .with_flags(&[place_flag(
            "models.\"claude-opus-5-5\".context_window",
            "300000",
        )])
        .low_points("env", "claude-opus-5-5", &|_, _| 64_000, ttl, &band, &none);
    assert!(widened.is_empty(), "{widened:?}");
    assert!(
        flagged
            .iter()
            .any(|w| w.starts_with("background compaction rarely runs below")),
        "{flagged:?}"
    );
}

#[test]
fn openai_compaction_estimates_do_not_add_anthropic_write_premiums() {
    let input = CompactionCostInput {
        prefix_tokens: 1000,
        warm_tokens: 0,
        after_anchor_tokens: 0,
        summary_tokens: 0,
        head_tokens: 1000,
        head_ttl: CacheTtl::OneHour,
        prefix_warm: false,
        tail_warm: false,
    };
    let cost = compaction_cost(&input, crate::pricing::model_info("gpt-6.1-sol").unwrap());
    assert_eq!(cost.estimate_micros, 2000);
    assert_eq!(cost.head_write_micros, 2000);
}

#[test]
fn the_low_point_warning_marks_its_estimates_with_a_tilde() {
    let warning = low_point_warning(
        CompactAt::Tokens(10_000),
        10_000,
        (
            "claude-opus-5-5",
            crate::pricing::model_info("claude-opus-5-5"),
        ),
        wisp_protocol::CacheTtl::OneHour,
    )
    .unwrap_or_default();
    assert!(
        warning.contains(", ~$0.123 a turn to compact and ~$0.12 to write the head again"),
        "{warning}"
    );
}
