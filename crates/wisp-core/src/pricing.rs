use std::sync::LazyLock;

use wisp_protocol::settings::names;
use wisp_protocol::{CacheTtl, Usage};

use crate::endpoints::ProviderKind;
use crate::models::{ModelRow, ModelTable};
use crate::provider::voice::Spent;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rates {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub cache_read_per_mtok: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LongPromptTier {
    pub prompt_tokens_over: u64,
    pub rates: Rates,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelInfo {
    pub rates: Rates,
    pub audio_above: bool,
    pub long_prompt: Option<LongPromptTier>,
    pub write_5m: f64,
    pub write_1h: f64,
    pub write_30m: f64,
    pub write_12h: f64,
    pub web_search_usd: f64,
}

impl ModelInfo {
    pub fn write_multiplier(&self, ttl: CacheTtl) -> f64 {
        match ttl {
            CacheTtl::FiveMinutes => self.write_5m,
            CacheTtl::OneHour => self.write_1h,
        }
    }

    pub fn rewrite_usd(&self, tokens: u64, ttl: CacheTtl) -> f64 {
        estimate_usd(
            self,
            &Usage {
                cache_creation_input_tokens: tokens,
                ..Usage::default()
            },
            ttl,
        )
    }

    pub fn rates_for(&self, usage: &Usage) -> &Rates {
        match &self.long_prompt {
            Some(tier) if usage.prompt_tokens() > tier.prompt_tokens_over => &tier.rates,
            _ => &self.rates,
        }
    }
}

const fn rates(input: f64, output: f64, cache_read: f64) -> Rates {
    Rates {
        input_per_mtok: input,
        output_per_mtok: output,
        cache_read_per_mtok: cache_read,
    }
}

static PRICED: LazyLock<Vec<(&'static str, ModelInfo)>> = LazyLock::new(|| {
    let table = ModelTable::shared();
    let mut priced: Vec<(&'static str, ModelInfo)> = table
        .rows()
        .iter()
        .filter_map(|row| Some((row, info_of(table, row)?)))
        .flat_map(|(row, info)| {
            std::iter::once(&row.id)
                .chain(&row.aliases)
                .map(move |id| (id.as_str(), info))
        })
        .collect();
    priced.sort_unstable_by_key(|(id, _)| *id);
    priced
});

fn info_of(table: &ModelTable, row: &ModelRow) -> Option<ModelInfo> {
    let provider = table.provider(row.provider?)?;
    Some(ModelInfo {
        rates: rate_card(row)?,
        audio_above: row
            .price_input_audio
            .zip(row.price_input)
            .is_some_and(|(audio, text)| audio > text),
        long_prompt: row.long_prompt.map(|tier| LongPromptTier {
            prompt_tokens_over: tier.over,
            rates: rates(
                millionths(tier.price_input),
                millionths(tier.price_output),
                millionths(tier.price_cache_read),
            ),
        }),
        write_5m: or_zero(provider.write_5m),
        write_1h: or_zero(provider.write_1h),
        write_30m: or_zero(provider.write_30m),
        write_12h: or_zero(provider.write_12h),
        web_search_usd: or_zero(provider.web_search_price),
    })
}

fn rate_card(row: &ModelRow) -> Option<Rates> {
    Some(rates(
        millionths(row.price_input?),
        millionths(row.price_output?),
        millionths(row.price_cache_read?),
    ))
}

pub fn price_keys(row: &ModelRow) -> [(&'static str, Option<u64>); 3] {
    [
        (names::PRICE_INPUT, row.price_input),
        (names::PRICE_OUTPUT, row.price_output),
        (names::PRICE_CACHE_READ, row.price_cache_read),
    ]
}

fn sets_a_price(row: &ModelRow) -> bool {
    price_keys(row).iter().any(|(_, given)| given.is_some())
}

pub fn resolve<'a>(
    layers: impl IntoIterator<Item = &'a ModelRow>,
    shipped: Option<&ModelRow>,
    table: &ModelTable,
    account: ProviderKind,
) -> Option<ModelInfo> {
    let Some(set) = layers.into_iter().find(|row| sets_a_price(row)) else {
        return info_of(table, shipped?);
    };
    let kind = shipped.and_then(|row| row.provider).unwrap_or(account);
    let base = ModelRow {
        provider: Some(kind),
        long_prompt: None,
        ..set.clone()
    };
    info_of(table, &base)
}

fn millionths(given: u64) -> f64 {
    given as f64 / 1_000_000.0
}

fn or_zero(given: Option<u64>) -> f64 {
    given.map_or(0.0, millionths)
}

pub fn prefix_write() -> f64 {
    write_multiplier(CacheTtl::OneHour)
}

pub fn web_search_usd(kind: ProviderKind, requests: u64) -> f64 {
    let price = ModelTable::shared()
        .provider(kind)
        .and_then(|p| p.web_search_price);
    requests as f64 * or_zero(price)
}

enum VoiceRate {
    Minutes {
        per_minute_usd: f64,
        floor_seconds: u64,
    },
    Tokens {
        text: Rates,
        audio: Rates,
        transcriber: &'static str,
        transcribed_per_minute_usd: f64,
    },
}

const GPT_LIVE: VoiceRate = VoiceRate::Minutes {
    per_minute_usd: 0.05,
    floor_seconds: 15,
};

const VOICE: &[(&str, VoiceRate)] = &[
    ("gpt-live-1", GPT_LIVE),
    ("gpt-live-1-codex", GPT_LIVE),
    (
        "gpt-realtime-2.1-mini",
        VoiceRate::Tokens {
            text: rates(0.60, 2.40, 0.06),
            audio: rates(10.0, 20.0, 0.30),
            transcriber: "gpt-realtime-whisper",
            transcribed_per_minute_usd: 0.017,
        },
    ),
];

pub fn voice_usd(model: &str, seconds: u64, spent: &[Spent]) -> Option<f64> {
    let (_, rate) = VOICE.iter().find(|(id, _)| *id == model)?;
    Some(match rate {
        VoiceRate::Minutes {
            per_minute_usd,
            floor_seconds,
        } => seconds.max(*floor_seconds) as f64 * per_minute_usd / 60.0,
        VoiceRate::Tokens {
            text,
            audio,
            transcribed_per_minute_usd,
            ..
        } => spent
            .iter()
            .map(|s| {
                tokens_usd(text, s.text_in, s.text_cached, s.text_out)
                    + tokens_usd(audio, s.audio_in, s.audio_cached, s.audio_out)
                    + s.transcribed_seconds * transcribed_per_minute_usd / 60.0
            })
            .sum(),
    })
}

pub fn voice_transcriber(model: &str) -> Option<&'static str> {
    VOICE.iter().find_map(|(id, rate)| match rate {
        VoiceRate::Tokens { transcriber, .. } if *id == model => Some(*transcriber),
        _ => None,
    })
}

fn tokens_usd(rates: &Rates, input: u64, cached: u64, output: u64) -> f64 {
    (input as f64 * rates.input_per_mtok
        + cached as f64 * rates.cache_read_per_mtok
        + output as f64 * rates.output_per_mtok)
        / 1_000_000.0
}

pub fn unpriced(usage: bool, usd: Option<f64>) -> bool {
    usage && usd.is_none()
}

pub fn write_multiplier(ttl: CacheTtl) -> f64 {
    let provider = ModelTable::shared().provider(ProviderKind::IMPLICIT);
    let write = provider.and_then(|p| match ttl {
        CacheTtl::FiveMinutes => p.write_5m,
        CacheTtl::OneHour => p.write_1h,
    });
    or_zero(write)
}

pub fn model_ids() -> impl Iterator<Item = &'static str> {
    PRICED.iter().map(|(id, _)| *id)
}

pub fn model_info(model: &str) -> Option<&'static ModelInfo> {
    PRICED
        .iter()
        .find(|(id, _)| *id == model)
        .map(|(_, info)| info)
}

pub fn estimate_usd(info: &ModelInfo, usage: &Usage, ttl: CacheTtl) -> f64 {
    let (w5m, w1h) = if usage.cache_creation_30m_input_tokens.is_some()
        || usage.cache_creation_12h_input_tokens.is_some()
    {
        (0, 0)
    } else {
        match (
            usage.cache_creation_5m_input_tokens,
            usage.cache_creation_1h_input_tokens,
        ) {
            (None, None) => match ttl {
                CacheTtl::FiveMinutes => (usage.cache_creation_input_tokens, 0),
                CacheTtl::OneHour => (0, usage.cache_creation_input_tokens),
            },
            (a, b) => (a.unwrap_or(0), b.unwrap_or(0)),
        }
    };
    let rates = info.rates_for(usage);
    let input = rates.input_per_mtok;
    let writes_usd = w5m as f64 * input * info.write_5m
        + w1h as f64 * input * info.write_1h
        + usage.cache_creation_30m_input_tokens.unwrap_or(0) as f64 * input * info.write_30m
        + usage.cache_creation_12h_input_tokens.unwrap_or(0) as f64 * input * info.write_12h;
    let read = usage.cache_read_input_tokens;
    tokens_usd(rates, usage.input_tokens, read, usage.output_tokens)
        + writes_usd / 1_000_000.0
        + usage.web_search_requests as f64 * info.web_search_usd
}

pub fn micros(usd: f64) -> u64 {
    whole_micros(usd * 1_000_000.0)
}

pub fn whole_micros(micros: f64) -> u64 {
    micros.round() as u64
}

pub fn prefix_quote(info: &ModelInfo, prefix_tokens: u64, ttl: CacheTtl) -> (f64, f64) {
    let read = Usage {
        cache_read_input_tokens: prefix_tokens,
        ..Usage::default()
    };
    let rewrite = Usage {
        cache_creation_input_tokens: prefix_tokens,
        ..Usage::default()
    };
    (
        estimate_usd(info, &read, ttl),
        estimate_usd(info, &rewrite, ttl),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_rewrite_and_prefix_quotes_use_input_prices() {
        let info = model_info("gpt-6.1-sol").unwrap();
        for ttl in [CacheTtl::FiveMinutes, CacheTtl::OneHour] {
            assert_eq!(info.write_multiplier(ttl), 1.0);
            assert_eq!(info.rewrite_usd(1000, ttl), 0.002);
            assert_eq!(prefix_quote(info, 1000, ttl).1, 0.002);
        }
        assert_eq!(
            model_info("claude-opus-5-5")
                .unwrap()
                .write_multiplier(CacheTtl::OneHour),
            2.0
        );
    }

    #[test]
    fn prefix_quote_prices_a_read_and_a_rewrite_at_the_ttl() {
        let info = model_info("claude-opus-5-5").unwrap();
        let (read, rewrite) = prefix_quote(info, 1_000_000, CacheTtl::OneHour);
        close(read, info.rates.cache_read_per_mtok);
        close(rewrite, info.rates.input_per_mtok * 2.0);
        let (_, short) = prefix_quote(info, 1_000_000, CacheTtl::FiveMinutes);
        close(short, info.rates.input_per_mtok * 1.25);
    }

    #[test]
    fn a_set_price_replaces_the_shipped_rate_card_most_specific_first() {
        let table = ModelTable::shared();
        let priced = |id: &str, (input, output, read): (u64, u64, u64)| ModelRow {
            price_input: Some(input),
            price_output: Some(output),
            price_cache_read: Some(read),
            ..ModelRow::new(id)
        };
        let haiku = table.find("claude-haiku-5-5");
        let account = priced("claude-haiku-5-5", (3_000_000, 9_000_000, 300_000));
        let mine = priced("claude-haiku-5-5", (2_000_000, 6_000_000, 200_000));
        let anthropic = ProviderKind::Anthropic;
        let set = resolve([&account, &mine], haiku, table, anthropic).unwrap();
        assert_eq!(set.rates, rates(3.0, 9.0, 0.3));
        assert_eq!(set.long_prompt, None);
        assert_eq!(
            set.write_1h,
            model_info("claude-haiku-5-5").unwrap().write_1h
        );
        let set = resolve([&mine], haiku, table, anthropic).unwrap();
        assert_eq!(set.rates, rates(2.0, 6.0, 0.2));
        assert_eq!(
            resolve([], haiku, table, anthropic).as_ref(),
            model_info("claude-haiku-5-5")
        );
        let part = ModelRow {
            price_input: Some(1_000_000),
            ..ModelRow::new("claude-haiku-5-5")
        };
        assert_eq!(resolve([&part, &mine], haiku, table, anthropic), None);
        let gateway = priced("my-model", (1_200_000, 4_800_000, 120_000));
        let openai = resolve([&gateway], None, table, ProviderKind::OpenAi).unwrap();
        assert_eq!(openai.rates, rates(1.2, 4.8, 0.12));
        assert_eq!((openai.write_5m, openai.write_30m), (1.0, 1.25));
        let claude = resolve([&gateway], None, table, anthropic).unwrap();
        assert_eq!((claude.write_1h, claude.write_30m), (2.0, 0.0));
        assert_eq!(resolve([], None, table, anthropic), None);
    }

    fn close(actual: f64, expected: f64) {
        assert!((actual - expected).abs() < 1e-12, "{actual} != {expected}");
    }

    #[test]
    fn opus_5_5_cost() {
        let info = model_info("claude-opus-5-5").unwrap();
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            cache_read_input_tokens: 1_000_000,
            cache_creation_input_tokens: 2_000_000,
            cache_creation_5m_input_tokens: Some(1_000_000),
            cache_creation_1h_input_tokens: Some(1_000_000),
            web_search_requests: 0,
            ..Usage::default()
        };
        let usd = estimate_usd(info, &usage, CacheTtl::OneHour);
        assert!((usd - 37.2).abs() < 1e-9, "{usd}");
    }

    #[test]
    fn fallback_models_are_priced_and_unknown_ones_are_not() {
        let info = model_info("claude-opus-4-8").unwrap();
        assert_eq!(info.rates, rates(5.0, 25.0, 0.50));
        assert!(model_info("claude-fable-5-1").is_some());
        assert!(model_info("some-gateway-model").is_none());
        assert!(model_info("gemini-3.1-flash-lite").unwrap().audio_above);
        assert!(!model_info("gemini-4-neon").unwrap().audio_above);
        let table = ModelTable::shared();
        for (ids, expected) in [
            (
                &["gemini-4-hydrogen", "gemini-4-hydrogen-xhigh"][..],
                rates(1.5, 7.5, 0.075),
            ),
            (
                &[
                    "gemini-4-neon",
                    "gemini-4-neon-low",
                    "gemini-4-neon-medium",
                    "gemini-4-neon-high",
                    "gemini-4-neon-xhigh",
                ][..],
                rates(3.0, 15.0, 0.15),
            ),
        ] {
            for &id in ids {
                let row = table.find(id).unwrap();
                assert_eq!(row.min_cacheable, Some(4096), "{id}");
                let resolved = resolve([], Some(row), table, ProviderKind::Gemini).unwrap();
                assert_eq!(resolved.rates, expected, "{id}");
                assert_eq!(resolved.long_prompt, None, "{id}");
                assert_eq!(model_info(id), Some(&resolved), "{id}");
            }
        }
        let flash_image = table.find("gemini-3.1-flash-image").unwrap();
        assert_eq!(
            resolve([], Some(flash_image), table, ProviderKind::Gemini),
            None
        );
        assert_eq!(model_info("gemini-3.1-flash-image"), None);
    }

    #[test]
    fn unsplit_writes_use_session_ttl() {
        let info = model_info("claude-opus-5-5").unwrap();
        let usage = Usage {
            cache_creation_input_tokens: 1_000_000,
            ..Usage::default()
        };
        assert!((estimate_usd(info, &usage, CacheTtl::OneHour) - 8.0).abs() < 1e-9);
        assert!((estimate_usd(info, &usage, CacheTtl::FiveMinutes) - 5.0).abs() < 1e-9);
    }

    #[test]
    fn gpt_6_2_bills_the_whole_request_at_the_long_tier_only_above_272k() {
        for (id, input, read, output) in [
            ("gpt-6.2-sol", 2.0, 0.10, 10.0),
            ("gpt-6.2-astra", 8.0, 0.40, 40.0),
        ] {
            let info = model_info(id).unwrap();
            for total in [272_000, 272_001] {
                let usage = Usage {
                    input_tokens: total - 100,
                    cache_read_input_tokens: 100,
                    output_tokens: 1_000,
                    ..Usage::default()
                };
                let (input_factor, output_factor) = if total > 272_000 {
                    (2.0, 1.5)
                } else {
                    (1.0, 1.0)
                };
                close(
                    estimate_usd(info, &usage, CacheTtl::OneHour),
                    ((usage.input_tokens as f64 * input + 100.0 * read) * input_factor
                        + 1_000.0 * output * output_factor)
                        / 1_000_000.0,
                );
            }
        }
    }

    #[test]
    fn every_model_has_positive_prices_and_a_cache_minimum() {
        for id in model_ids() {
            let info = model_info(id).unwrap();
            let tiers =
                std::iter::once(&info.rates).chain(info.long_prompt.iter().map(|t| &t.rates));
            for r in tiers {
                assert!(r.input_per_mtok > 0.0, "{id}");
                assert!(r.output_per_mtok > 0.0, "{id}");
                assert!(r.cache_read_per_mtok > 0.0, "{id}");
            }
            let row = ModelTable::shared().find(id).unwrap();
            assert!(row.min_cacheable.is_some_and(|min| min > 0), "{id}");
            if let Some(tier) = &info.long_prompt {
                let (base, long) = (&info.rates, &tier.rates);
                assert!(long.input_per_mtok >= base.input_per_mtok, "{id}");
                assert!(long.output_per_mtok >= base.output_per_mtok, "{id}");
                assert!(long.cache_read_per_mtok >= base.cache_read_per_mtok, "{id}");
            }
        }
    }

    #[test]
    fn haiku_5_5_saturates_an_overflowing_prompt_into_the_long_tier() {
        let info = model_info("claude-haiku-5-5").unwrap();
        let usage = Usage {
            input_tokens: u64::MAX,
            cache_read_input_tokens: 1,
            ..Usage::default()
        };
        assert_eq!(usage.prompt_tokens(), u64::MAX);
        let usd = estimate_usd(info, &usage, CacheTtl::OneHour);
        let expected = (u64::MAX as f64 * 0.50 + 0.05) / 1_000_000.0;
        assert!(((usd - expected) / expected).abs() < 1e-12, "{usd}");
    }

    #[test]
    fn each_web_search_costs_its_providers_price() {
        let anthropic = ProviderKind::Anthropic;
        close(web_search_usd(anthropic, 0), 0.0);
        close(web_search_usd(anthropic, 1), 0.01);
        close(web_search_usd(anthropic, 1_000), 10.0);
        close(web_search_usd(ProviderKind::Gemini, 1), 0.014);
        close(web_search_usd(ProviderKind::Gemini, 1_000), 14.0);
    }

    #[test]
    fn a_voice_call_costs_five_cents_a_minute_by_the_second_with_a_15_second_floor() {
        for model in ["gpt-live-1", "gpt-live-1-codex"] {
            close(voice_usd(model, 0, &[]).unwrap(), 0.0125);
            close(voice_usd(model, 15, &[]).unwrap(), 0.0125);
            close(voice_usd(model, 16, &[]).unwrap(), 16.0 * 0.05 / 60.0);
            close(voice_usd(model, 120, &[]).unwrap(), 0.10);
        }
        assert_eq!(voice_usd("gpt-6-sol", 60, &[]), None);
        let spent = Spent {
            text_in: 1_000_000,
            text_cached: 2_000_000,
            text_out: 3_000_000,
            audio_in: 4_000_000,
            audio_cached: 5_000_000,
            audio_out: 6_000_000,
            transcribed_seconds: 120.0,
        };
        let one = 0.60 + 0.12 + 7.20 + 40.0 + 1.50 + 120.0 + 0.034;
        let mini = "gpt-realtime-2.1-mini";
        close(voice_usd(mini, 600, &[spent, spent]).unwrap(), 2.0 * one);
        close(voice_usd(mini, 600, &[]).unwrap(), 0.0);
        close(voice_usd("gpt-live-1", 60, &[spent]).unwrap(), 0.05);
        assert_eq!(voice_transcriber(mini), Some("gpt-realtime-whisper"));
        assert_eq!(voice_transcriber("gpt-live-1"), None);
    }

    #[test]
    fn searches_are_priced_on_top_of_their_tokens() {
        let info = model_info("claude-haiku-5-5").unwrap();
        let tokens = Usage {
            input_tokens: 50_000,
            cache_creation_input_tokens: 25_000,
            cache_creation_5m_input_tokens: Some(25_000),
            cache_creation_1h_input_tokens: Some(0),
            ..Usage::default()
        };
        let searched = Usage {
            web_search_requests: 2,
            ..tokens
        };
        let ttl = CacheTtl::OneHour;
        close(estimate_usd(info, &tokens, ttl), 0.005 + 0.003125);
        close(
            estimate_usd(info, &searched, ttl) - estimate_usd(info, &tokens, ttl),
            0.02,
        );
    }

    #[test]
    fn sonnet_5_5_cache_reads_are_five_percent_of_input() {
        let info = model_info("claude-sonnet-5-5").unwrap();
        assert_eq!(info.rates, rates(2.0, 10.0, 0.10));
    }

    fn haiku_usage(input: u64, read: u64, w5m: u64, w1h: u64) -> Usage {
        Usage {
            input_tokens: input,
            output_tokens: 1_000,
            cache_read_input_tokens: read,
            cache_creation_input_tokens: w5m + w1h,
            cache_creation_5m_input_tokens: Some(w5m),
            cache_creation_1h_input_tokens: Some(w1h),
            web_search_requests: 0,
            ..Usage::default()
        }
    }

    fn haiku_cost(rates: (f64, f64, f64), usage: &Usage) -> f64 {
        let (input, output, read) = rates;
        (usage.input_tokens as f64 * input
            + usage.output_tokens as f64 * output
            + usage.cache_read_input_tokens as f64 * read
            + usage.cache_creation_5m_input_tokens.unwrap_or(0) as f64 * input * 1.25
            + usage.cache_creation_1h_input_tokens.unwrap_or(0) as f64 * input * 2.0)
            / 1_000_000.0
    }

    const HAIKU_SHORT: (f64, f64, f64) = (0.10, 0.50, 0.01);
    const HAIKU_LONG: (f64, f64, f64) = (0.50, 2.50, 0.05);

    #[test]
    fn haiku_5_5_at_the_threshold_uses_short_prompt_prices() {
        let info = model_info("claude-haiku-5-5").unwrap();
        let usage = haiku_usage(10_000, 60_000, 20_000, 10_000);
        assert_eq!(usage.prompt_tokens(), 100_000);
        close(
            estimate_usd(info, &usage, CacheTtl::OneHour),
            haiku_cost(HAIKU_SHORT, &usage),
        );
    }

    #[test]
    fn haiku_5_5_below_the_threshold_uses_short_prompt_prices() {
        let info = model_info("claude-haiku-5-5").unwrap();
        let usage = haiku_usage(9_999, 60_000, 20_000, 10_000);
        close(
            estimate_usd(info, &usage, CacheTtl::OneHour),
            haiku_cost(HAIKU_SHORT, &usage),
        );
    }

    #[test]
    fn haiku_5_5_one_token_over_bills_the_whole_request_at_long_prompt_prices() {
        let info = model_info("claude-haiku-5-5").unwrap();
        let usage = haiku_usage(10_001, 60_000, 20_000, 10_000);
        assert_eq!(usage.prompt_tokens(), 100_001);
        close(
            estimate_usd(info, &usage, CacheTtl::OneHour),
            haiku_cost(HAIKU_LONG, &usage),
        );
    }

    #[test]
    fn haiku_5_5_cache_reads_alone_can_cross_the_threshold() {
        let info = model_info("claude-haiku-5-5").unwrap();
        let usage = haiku_usage(1, 100_000, 0, 0);
        close(
            estimate_usd(info, &usage, CacheTtl::OneHour),
            haiku_cost(HAIKU_LONG, &usage),
        );
    }

    #[test]
    fn haiku_5_5_cache_writes_count_toward_the_threshold_with_unsplit_ttl() {
        let info = model_info("claude-haiku-5-5").unwrap();
        let usage = Usage {
            input_tokens: 1,
            cache_creation_input_tokens: 100_000,
            ..Usage::default()
        };
        close(
            estimate_usd(info, &usage, CacheTtl::FiveMinutes),
            (0.50 + 100_000.0 * 0.50 * 1.25) / 1_000_000.0,
        );
        close(
            estimate_usd(info, &usage, CacheTtl::OneHour),
            (0.50 + 100_000.0 * 0.50 * 2.0) / 1_000_000.0,
        );
    }

    #[test]
    fn haiku_5_5_tier_prices_match_the_published_table() {
        let info = model_info("claude-haiku-5-5").unwrap();
        let mtok = |input, read, w5m, w1h| Usage {
            input_tokens: input,
            cache_read_input_tokens: read,
            cache_creation_input_tokens: w5m + w1h,
            cache_creation_5m_input_tokens: Some(w5m),
            cache_creation_1h_input_tokens: Some(w1h),
            ..Usage::default()
        };
        let ttl = CacheTtl::OneHour;
        close(estimate_usd(info, &mtok(100_000, 0, 0, 0), ttl), 0.01);
        close(estimate_usd(info, &mtok(0, 100_000, 0, 0), ttl), 0.001);
        close(estimate_usd(info, &mtok(0, 0, 100_000, 0), ttl), 0.0125);
        close(estimate_usd(info, &mtok(0, 0, 0, 100_000), ttl), 0.02);
        close(estimate_usd(info, &mtok(1_000_000, 0, 0, 0), ttl), 0.50);
        close(estimate_usd(info, &mtok(0, 1_000_000, 0, 0), ttl), 0.05);
        close(estimate_usd(info, &mtok(0, 0, 1_000_000, 0), ttl), 0.625);
        close(estimate_usd(info, &mtok(0, 0, 0, 1_000_000), ttl), 1.0);
        let output = |prompt| Usage {
            input_tokens: prompt,
            output_tokens: 1_000_000,
            ..Usage::default()
        };
        close(
            estimate_usd(info, &output(100_000), ttl)
                - estimate_usd(info, &mtok(100_000, 0, 0, 0), ttl),
            0.50,
        );
        close(
            estimate_usd(info, &output(1_000_000), ttl)
                - estimate_usd(info, &mtok(1_000_000, 0, 0, 0), ttl),
            2.50,
        );
    }
}
