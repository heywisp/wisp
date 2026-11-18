use std::time::Duration;

use wisp_protocol::settings::AccountModel;
use wisp_protocol::{AgentEvent, CacheTtl, Effort, InputId, NoSearch, SearchForm, TurnOutcome};
use wisp_render::chrome::{
    Credential, Endpoint, Facts, ImageTier, LongPromptRate, ModelCaps, ModelFacts, ModelRates,
    SearchForms, SearchPair, WriteMultipliers,
};
use wisp_render::markdown::{Layout, Renderer};
use wisp_render::theme::glyphs::GlyphSet;
use wisp_render::transcript::{Config, Detail, Transcript};

use crate::shared::endpoint;

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
    efforts: &[Effort],
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
            long_prompt: None,
        }),
        caps: Some(ModelCaps {
            supported: adaptive,
            efforts: efforts.to_vec(),
            default_effort: Some(Effort::High),
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
    let mut haiku = model("claude-haiku-5-5", (0.10, 0.50), &Effort::ALL, true, true);
    if let Some(rates) = &mut haiku.rates {
        rates.long_prompt = Some(LongPromptRate {
            prompt_tokens_over: 100_000,
            input_per_mtok: 0.50,
            cache_read_per_mtok: 0.05,
        });
    }
    if let Some(caps) = &mut haiku.caps {
        caps.default_effort = Some(Effort::Medium);
    }
    let mut opus = model("claude-opus-5-5", (4.0, 20.0), &Effort::ALL, true, true);
    if let Some(caps) = &mut opus.caps {
        caps.default_effort = Some(Effort::Medium);
    }
    vec![
        haiku,
        model(
            "claude-opus-4-5",
            (5.0, 25.0),
            &[Effort::Low, Effort::Medium, Effort::High],
            false,
            false,
        ),
        model("claude-opus-4-8", (5.0, 25.0), &Effort::ALL, false, true),
        opus,
        model(
            "claude-sonnet-4-6",
            (3.0, 15.0),
            &[Effort::Low, Effort::Medium, Effort::High, Effort::Max],
            false,
            true,
        ),
        model("claude-sonnet-5-5", (2.0, 10.0), &Effort::ALL, true, true),
    ]
}

pub(crate) fn facts(endpoint: Endpoint, ttl: CacheTtl) -> Facts {
    let tail = match ttl {
        CacheTtl::FiveMinutes => 1.25,
        CacheTtl::OneHour => 2.0,
    };
    Facts {
        version: "0.0.0".to_owned(),
        provider: "anthropic".to_owned(),
        model: "claude-opus-5-5".to_owned(),
        effort: "medium".to_owned(),
        cache_ttl: ttl,
        credential: None,
        host: None,
        log: None,
        home: None,
        cwd: "~".to_owned(),
        endpoint,
        models: models(),
        on_accounts: Vec::new(),
        write_multipliers: WriteMultipliers {
            prefix: 2.0,
            tail,
            five_minutes: 1.25,
            one_hour: 2.0,
        },
        next: None,
        max_tokens: 64_000,
        endpoint_name: None,
        endpoints: Vec::new(),
        endpoint_note: None,
        resume_refused: false,
        instructions: Default::default(),
        search: Default::default(),
        providers_off: Default::default(),
        resumed: None,
        claude_usage: false,
        start_pick: None,
    }
}

pub(crate) fn limit(resets_at: Option<i64>) -> wisp_protocol::ErrorDetail {
    use wisp_protocol::plan_usage::{UsageScope, UsageSource, UsageWindow};
    use wisp_protocol::{ErrorDetail, FailurePhase, FailureSource, RetryClass};
    let mut detail = ErrorDetail::new(
        FailurePhase::Response,
        FailureSource::Endpoint,
        RetryClass::Never,
    );
    detail.limit_reached = Some(Box::new(UsageWindow {
        key: "weekly".into(),
        label: "Weekly".into(),
        duration_seconds: Some(604800),
        scope: UsageScope::All,
        used_percent: None,
        resets_at,
        observed_at: 1800000000,
        source: UsageSource::ErrorBody,
    }));
    detail
}

pub(crate) fn unglued(text: &str) -> String {
    ["\u{25cf}", "\u{25cb}", "*", "o"]
        .iter()
        .fold(text.to_owned(), |text, glyph| {
            text.replace(&format!("{glyph}\u{a0}"), "")
        })
}

pub(crate) use wisp_render::chrome::account::mockup_mark;

pub(crate) fn endpoint_facts() -> Facts {
    Facts {
        credential: Some(Credential::AuthToken),
        host: Some("gw.example.com".to_owned()),
        endpoint_name: Some("manifest".to_owned()),
        endpoints: vec![
            endpoint("direct", Endpoint::Official, Credential::ApiKey, None, None),
            endpoint(
                "gateway",
                Endpoint::Custom,
                Credential::AuthToken,
                Some("gw2.example.com"),
                None,
            ),
            endpoint(
                "manifest",
                Endpoint::Custom,
                Credential::AuthToken,
                Some("gw.example.com"),
                None,
            ),
            endpoint(
                "work",
                Endpoint::Official,
                Credential::ApiKey,
                None,
                Some("WORK_KEY is not set"),
            ),
        ],
        ..facts(Endpoint::Custom, CacheTtl::OneHour)
    }
}

pub(crate) const CHATGPT_LIST: [(&str, f64); 9] = [
    ("gpt-6.1-sol", 2.5),
    ("gpt-6-astra", 1.0),
    ("gpt-6.1-terra", 1.25),
    ("gpt-6-sol", 2.0),
    ("gpt-reserve", 0.0),
    ("gpt-6-luna", 0.1),
    ("gpt-5.6-sol", 4.0),
    ("gpt-5.6-terra", 0.5),
    ("gpt-5.6-luna", 0.2),
];

pub(crate) fn four_accounts() -> Facts {
    let mut models = models();
    let mut own = Vec::new();
    let mut shown = 0..;
    for (id, input) in CHATGPT_LIST {
        let mut facts = model(id, (input, input * 6.0), &Effort::ALL, true, true);
        let hidden = id == "gpt-reserve";
        if hidden {
            facts.provider = None;
            facts.shipped = false;
            facts.rates = None;
        }
        if let Some(caps) = &mut facts.caps {
            let low = id.ends_with("-sol");
            caps.default_effort = Some(if low { Effort::Low } else { Effort::Medium });
        }
        let listed = (!hidden).then(|| shown.next()).flatten();
        own.push(AccountModel {
            id: id.to_owned(),
            listed,
        });
        models.push(facts);
    }
    let subscription = |name: &str| {
        endpoint(
            name,
            Endpoint::Official,
            Credential::AnthropicPlan,
            None,
            None,
        )
    };
    let mut chatgpt = endpoint(
        "chatgpt",
        Endpoint::Official,
        Credential::ChatGptPlan,
        None,
        None,
    );
    chatgpt.provider = "openai".to_owned();
    chatgpt.models = own;
    Facts {
        credential: Some(Credential::AnthropicPlan),
        endpoint_name: Some("claude-subscription".to_owned()),
        endpoints: vec![
            subscription("claude-subscription"),
            subscription("claude-work"),
            endpoint(
                "claude-api",
                Endpoint::Official,
                Credential::ApiKey,
                None,
                None,
            ),
            chatgpt,
        ],
        models,
        ..facts(Endpoint::Official, CacheTtl::OneHour)
    }
}

pub(crate) fn with_levels(f: &mut Facts, id: &str, efforts: &[Effort], default: Effort) {
    let Some(caps) = f
        .models
        .iter_mut()
        .find(|m| m.id == id)
        .and_then(|m| m.caps.as_mut())
    else {
        panic!("the fixture has no model `{id}` with capabilities");
    };
    caps.efforts = efforts.to_vec();
    caps.default_effort = Some(default);
}

pub(crate) const NO_SEARCH_DATA: &str = "claude-sonnet-4-6";

pub(crate) fn search_forms(models: &[ModelFacts]) -> SearchForms {
    let mut pairs = vec![];
    for endpoint in ["direct", "gateway", "manifest"] {
        for model in models.iter().filter(|m| m.id != NO_SEARCH_DATA) {
            let form = if endpoint == "direct" {
                SearchForm::Native
            } else {
                SearchForm::None(NoSearch::TurnedOff)
            };
            pairs.push(SearchPair {
                endpoint: endpoint.to_owned(),
                model: model.id.clone(),
                form,
            });
        }
    }
    SearchForms {
        requested: true,
        chat_only: false,
        pairs,
        refused: Vec::new(),
        kept: None,
    }
}

pub(crate) fn search_facts(start: &str) -> Facts {
    let mut f = endpoint_facts();
    f.search = search_forms(&f.models);
    if let Some(endpoint) = f.endpoint_facts(start).cloned() {
        f.endpoint_name = Some(endpoint.name);
        f.endpoint = endpoint.reach.unwrap_or(Endpoint::Official);
        f.credential = endpoint.credential;
        f.host = endpoint.host;
    }
    f
}

pub(crate) fn transcript() -> Transcript {
    Transcript::new(Config {
        provider: "anthropic".to_owned(),
        cache_ttl: wisp_protocol::CacheTtl::OneHour,
        logging: false,
    })
}

pub(crate) fn rendered(t: &mut Transcript) -> String {
    let r = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    crate::testing::text(&t.rows(&r, Detail::Scrollback))
}

pub(crate) fn ended(outcome: TurnOutcome) -> AgentEvent {
    crate::events::ended(outcome, 0)
}

pub(crate) fn id(n: &str) -> InputId {
    InputId(n.to_owned())
}

pub(crate) fn s(secs: u64) -> Duration {
    Duration::from_secs(secs)
}
