use std::collections::BTreeSet;
use std::sync::Arc;

use wisp_core::agent::Connection;
use wisp_core::endpoints::goes::model_provider;
use wisp_core::endpoints::{EndpointBook, ProviderKind};
use wisp_core::models::list::ModelList;
use wisp_core::pricing::{self, ModelInfo};
use wisp_core::provider::{ModelCapabilities, shipped_capabilities};
use wisp_protocol::opening::{
    Endpoint, ImageTier, LongPromptRate, ModelCaps, ModelFacts, ModelRates, WriteMultipliers,
};
use wisp_protocol::settings::{AccountModel, CredentialKind, EndpointSummary};
use wisp_protocol::{CacheTtl, Opening, StartSaid};

use crate::endpoints;

pub fn rates_of(info: &ModelInfo) -> ModelRates {
    ModelRates {
        writes: Some(WriteMultipliers {
            prefix: info.write_1h,
            tail: info.write_1h,
            five_minutes: info.write_5m,
            one_hour: info.write_1h,
        }),
        input_per_mtok: info.rates.input_per_mtok,
        output_per_mtok: info.rates.output_per_mtok,
        cache_read_per_mtok: info.rates.cache_read_per_mtok,
        long_prompt: info.long_prompt.map(|tier| LongPromptRate {
            prompt_tokens_over: tier.prompt_tokens_over,
            input_per_mtok: tier.rates.input_per_mtok,
            cache_read_per_mtok: tier.rates.cache_read_per_mtok,
        }),
    }
}

pub fn caps_of(row: &ModelCapabilities, kind: ProviderKind) -> ModelCaps {
    ModelCaps {
        supported: row.selectable(kind),
        efforts: row.efforts.to_vec(),
        default_effort: row.default_effort,
        per_message_effort: row.per_message_effort,
        adaptive_thinking: row.adaptive_thinking,
        max_output: row.max_output,
    }
}

pub fn image_tier_of(row: &ModelCapabilities) -> Option<ImageTier> {
    row.images.map(|images| ImageTier {
        max_edge: images.tier.max_edge,
        max_tokens: images.tier.max_tokens,
    })
}

pub fn models(ready: &[Connection]) -> Vec<ModelFacts> {
    let table = wisp_core::models::ModelTable::shared();
    let lists: Vec<(&Connection, Arc<ModelList>, Vec<AccountModel>)> = ready
        .iter()
        .filter_map(|c| c.provider.model_list().map(|list| (c, list)))
        .map(|(c, list)| {
            let offers = list.offers();
            (c, list, offers)
        })
        .collect();
    let mut ids: BTreeSet<String> = pricing::model_ids().map(str::to_owned).collect();
    ids.extend(lists.iter().flat_map(|(_, list, _)| list.ids()));
    ids.into_iter()
        .map(|id| {
            let kind = shipped_capabilities(&id).map(|(kind, _)| kind);
            let list = lists.iter().find(|(_, list, _)| list.find(&id).is_some());
            let row = match list {
                Some((c, ..)) => c
                    .provider
                    .capabilities(&id)
                    .map(|row| (c.provider.kind(), row)),
                None => shipped_capabilities(&id),
            };
            let shown = list.and_then(|(_, _, offers)| offers.iter().find(|m| m.id == id));
            ModelFacts {
                provider: model_provider(&id).map(|kind| kind.as_str().to_owned()),
                shipped: kind.is_some(),
                listed: kind.map_or_else(|| shown?.listed, |_| table.listed_rank(&id)),
                rates: pricing::model_info(&id).map(rates_of),
                caps: row.map(|(wire, row)| caps_of(&row, wire)),
                image_tier: row.as_ref().and_then(|(_, row)| image_tier_of(row)),
                variant: table.find(&id).and_then(|row| row.effort_variant.clone()),
                id,
            }
        })
        .collect()
}

fn cwd() -> String {
    std::env::current_dir()
        .map(|dir| wisp_core::workspace::home_shown(&dir))
        .unwrap_or_default()
}

pub fn write_multipliers(ttl: CacheTtl) -> WriteMultipliers {
    WriteMultipliers {
        prefix: pricing::prefix_write(),
        tail: pricing::write_multiplier(ttl),
        five_minutes: pricing::write_multiplier(CacheTtl::FiveMinutes),
        one_hour: pricing::write_multiplier(CacheTtl::OneHour),
    }
}

pub struct Chosen<'a> {
    pub model: &'a str,
    pub effort: &'a str,
    pub cache_ttl: CacheTtl,
    pub endpoint: Endpoint,
    pub max_tokens: u32,
    pub claude_usage: bool,
}

pub fn facts(
    chosen: Chosen<'_>,
    credential: Option<CredentialKind>,
    host: Option<String>,
    log: Option<String>,
) -> Opening {
    Opening {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        provider: model_provider(chosen.model)
            .unwrap_or(ProviderKind::IMPLICIT)
            .title()
            .to_owned(),
        model: chosen.model.to_owned(),
        effort: chosen.effort.to_owned(),
        cache_ttl: chosen.cache_ttl,
        credential,
        host,
        log,
        home: std::env::var("HOME").ok(),
        cwd: cwd(),
        endpoint: chosen.endpoint,
        models: models(&[]),
        on_accounts: Vec::new(),
        write_multipliers: write_multipliers(chosen.cache_ttl),
        next: None,
        max_tokens: chosen.max_tokens,
        endpoint_name: None,
        endpoints: Vec::new(),
        endpoint_note: None,
        resume_refused: false,
        instructions: Default::default(),
        search: Default::default(),
        resumed: None,
        claude_usage: chosen.claude_usage,
        start_pick: None,
        card: Default::default(),
        start: Some(StartSaid::default()),
    }
}

pub fn session_facts(
    book: &EndpointBook,
    chosen: Chosen<'_>,
    endpoints: Vec<EndpointSummary>,
    ready: &[Connection],
) -> Opening {
    Opening {
        provider: book.active.provider.title().to_owned(),
        endpoint_name: Some(book.start.to_string()),
        endpoints,
        models: models(ready),
        endpoint_note: book.overridden.as_ref().map(|o| o.note()),
        ..facts(
            chosen,
            Some(endpoints::credential_kind(&book.active.credential)),
            book.active.gateway_host(),
            None,
        )
    }
}
