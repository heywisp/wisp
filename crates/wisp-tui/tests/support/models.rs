use wisp_protocol::Effort;
use wisp_render::chrome::{
    ImageTier, LongPromptRate, ModelCaps, ModelFacts, ModelRates, WriteMultipliers,
};

const ALL: [Effort; 5] = Effort::ALL;
const NO_XHIGH: [Effort; 4] = [Effort::Low, Effort::Medium, Effort::High, Effort::Max];
const UP_TO_HIGH: [Effort; 3] = [Effort::Low, Effort::Medium, Effort::High];

pub(crate) const LISTED: [&str; 5] = [
    "claude-opus-5-5",
    "claude-sonnet-5-5",
    "claude-haiku-5-5",
    "gpt-6.1-sol",
    "gpt-6-astra",
];

fn model(
    id: &str,
    prices: (f64, f64),
    long: Option<(u64, f64)>,
    efforts: &[Effort],
    default: Effort,
    per_message: bool,
    adaptive: bool,
) -> ModelFacts {
    ModelFacts {
        shipped: true,
        provider: Some(
            if id.starts_with("gpt-") {
                "openai"
            } else {
                "anthropic"
            }
            .to_owned(),
        ),
        id: id.to_owned(),
        listed: LISTED
            .iter()
            .position(|listed| *listed == id)
            .and_then(|rank| u16::try_from(rank).ok()),
        rates: Some(ModelRates {
            writes: None,
            input_per_mtok: prices.0,
            output_per_mtok: prices.1,
            cache_read_per_mtok: prices.0 / 10.0,
            long_prompt: long.map(|(over, input)| LongPromptRate {
                prompt_tokens_over: over,
                input_per_mtok: input,
                cache_read_per_mtok: input / 10.0,
            }),
        }),
        caps: Some(ModelCaps {
            supported: adaptive,
            efforts: efforts.to_vec(),
            default_effort: Some(default),
            per_message_effort: per_message,
            adaptive_thinking: adaptive,
            max_output: adaptive.then_some(128_000),
        }),
        variant: None,
        image_tier: Some(ImageTier {
            max_edge: 2576,
            max_tokens: 4784,
        }),
    }
}

pub(crate) fn models() -> Vec<ModelFacts> {
    vec![
        model(
            "claude-fable-5-1",
            (10.0, 50.0),
            None,
            &ALL,
            Effort::High,
            true,
            true,
        ),
        model(
            "claude-haiku-5-5",
            (0.10, 0.50),
            Some((100_000, 0.50)),
            &ALL,
            Effort::Medium,
            true,
            true,
        ),
        model(
            "claude-opus-4-5",
            (5.0, 25.0),
            None,
            &UP_TO_HIGH,
            Effort::High,
            false,
            false,
        ),
        model(
            "claude-opus-4-8",
            (5.0, 25.0),
            None,
            &ALL,
            Effort::High,
            false,
            true,
        ),
        model(
            "claude-opus-5-5",
            (4.0, 20.0),
            None,
            &ALL,
            Effort::Medium,
            true,
            true,
        ),
        model(
            "claude-sonnet-4-6",
            (3.0, 15.0),
            None,
            &NO_XHIGH,
            Effort::High,
            false,
            true,
        ),
        model(
            "claude-sonnet-5-5",
            (2.0, 10.0),
            None,
            &ALL,
            Effort::High,
            true,
            true,
        ),
    ]
}

pub(crate) fn multipliers(tail: f64) -> WriteMultipliers {
    WriteMultipliers {
        prefix: 2.0,
        tail,
        five_minutes: 1.25,
        one_hour: 2.0,
    }
}
