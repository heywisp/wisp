use std::fmt;

use jiff::{SignedDuration, Timestamp};
use wisp_protocol::context::{PointSource, Window, WindowSource, format_tokens, window_for};
use wisp_protocol::settings::{self, CompactionCost, POINT_FLOOR, names};
use wisp_protocol::{
    Billing, CacheTtl, ContentBlock, EffortMarker, ImageSource, Message, Usage, format_usd,
};

use std::collections::{BTreeMap, BTreeSet};

use crate::config::{SavedSettings, set_row_value};
use crate::endpoints::{ProviderKind, goes};
use crate::models::list::{ListedModel, ModelList};
use crate::models::{ConfigModels, ModelRow, ModelTable};
use crate::pricing::{self, ModelInfo};
use crate::settings::{Current, Found, Given, Layer, Layers, resolve_one};
use wisp_protocol::settings::{Place, SettingValue, Target, WindowRoom};

pub use wisp_protocol::context::compact_at;
pub use wisp_protocol::settings::TokensOrPercent as CompactAt;
pub use wisp_tool_text::compaction::{BYTES_PER_TOKEN, text_tokens};
pub use wisp_tool_text::output::group;

pub const SUMMARY_TOKENS_ESTIMATE: u64 = 6_000;

pub const TYPICAL_HEAD_TOKENS: u64 = 15_000;

pub const BACKGROUND_TOKENS: u64 = 40_000;

pub const BACKGROUND_MARGIN: u64 = 5_000;

pub const UNREADABLE_IMAGE_TOKENS: u64 = wisp_media::Tier::HIGH.max_tokens as u64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimitTooSmall {
    pub model: String,
    pub window: u64,
    pub point: u64,
    pub reserve: u64,
    pub smallest: u64,
}

impl fmt::Display for LimitTooSmall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "context_window {} for {} puts the compaction point at {} (window \u{2212} reserve {}); it must be at least {}. Set at least {}.",
            self.window,
            wisp_protocol::model_words(&self.model),
            group(self.point),
            group(self.reserve),
            group(POINT_FLOOR),
            self.smallest
        )
    }
}

impl std::error::Error for LimitTooSmall {}

pub fn check_limit(model: &str, window: u64, max_tokens: u32) -> Result<(), LimitTooSmall> {
    let point = settings::default_point(window, max_tokens);
    if point >= POINT_FLOOR {
        return Ok(());
    }
    Err(LimitTooSmall {
        model: model.to_owned(),
        window,
        point,
        reserve: settings::reserve(window, max_tokens),
        smallest: settings::smallest_window(POINT_FLOOR, max_tokens),
    })
}

pub fn above_documented(model: &str, limit: u64, documented: u64, source: &str) -> Option<String> {
    (limit > documented).then(|| {
        format!(
            "context_window {limit} for {} is above the documented window of {} ({source}); requests may fail with a context overflow",
            wisp_protocol::model_words(model),
            format_tokens(documented)
        )
    })
}

pub fn image_tokens(source: &ImageSource) -> u64 {
    let ImageSource::Base64 { data, .. } = source;
    wisp_media::decode_base64(data)
        .ok()
        .and_then(|bytes| wisp_media::dimensions(&bytes).ok())
        .map_or(UNREADABLE_IMAGE_TOKENS, |(width, height)| {
            wisp_media::tokens(width, height)
        })
}

fn json_tokens(value: &serde_json::Value) -> u64 {
    text_tokens(&value.to_string())
}

pub fn block_tokens(block: &ContentBlock) -> u64 {
    match block {
        ContentBlock::Text { text } => text_tokens(text),
        ContentBlock::Thinking { thinking, .. } => text_tokens(thinking),
        ContentBlock::RedactedThinking { data } => text_tokens(data),
        ContentBlock::ToolUse { name, input, .. } => {
            text_tokens(name).saturating_add(json_tokens(input))
        }
        ContentBlock::ToolResult { content, .. } => blocks_tokens(content),
        ContentBlock::Opaque { raw, .. } => json_tokens(raw),
        ContentBlock::Effort { .. } => 0,
        ContentBlock::Image { source } => image_tokens(source),
        ContentBlock::Media { .. } => 0,
        ContentBlock::ToolAddition { tool } | ContentBlock::ToolRemoval { tool } => {
            serde_json::to_value(tool).map_or(0, |value| json_tokens(&value))
        }
    }
}

pub fn blocks_tokens(blocks: &[ContentBlock]) -> u64 {
    blocks
        .iter()
        .fold(0, |sum, block| sum.saturating_add(block_tokens(block)))
}

#[derive(Debug, Clone, PartialEq)]
pub struct Appended {
    pub user: Vec<ContentBlock>,
    pub marker: Option<EffortMarker>,
}

pub fn fork_request(view: &[Message], appended: Appended) -> Vec<Message> {
    let mut messages = view.to_vec();
    crate::agent::append_user(&mut messages, appended.user);
    messages.extend(appended.marker.map(Message::effort));
    messages
}

pub fn next_estimate(used: u64, output_tokens: u64, new_content: &[ContentBlock]) -> u64 {
    used.saturating_add(output_tokens)
        .saturating_add(blocks_tokens(new_content))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionCostInput {
    pub prefix_tokens: u64,
    pub warm_tokens: u64,
    pub after_anchor_tokens: u64,
    pub summary_tokens: u64,
    pub head_tokens: u64,
    pub head_ttl: CacheTtl,
    pub prefix_warm: bool,
    pub tail_warm: bool,
}

fn prompt_of(tokens: u64) -> Usage {
    Usage {
        input_tokens: tokens,
        ..Usage::default()
    }
}

pub fn compaction_cost(input: &CompactionCostInput, prices: &ModelInfo) -> CompactionCost {
    let request = input
        .prefix_tokens
        .saturating_add(input.warm_tokens)
        .saturating_add(input.after_anchor_tokens);
    let rates = prices.rates_for(&prompt_of(request));
    let prefix_rate = if input.prefix_warm {
        rates.cache_read_per_mtok
    } else {
        rates.input_per_mtok * prices.write_multiplier(CacheTtl::OneHour)
    };
    let warm_rate = if input.tail_warm {
        rates.cache_read_per_mtok
    } else {
        rates.input_per_mtok
    };
    let estimate = input.prefix_tokens as f64 * prefix_rate
        + input.warm_tokens as f64 * warm_rate
        + input.after_anchor_tokens as f64 * rates.input_per_mtok
        + input.summary_tokens as f64 * rates.output_per_mtok;
    let head_request = input.prefix_tokens.saturating_add(input.head_tokens);
    let head_rates = prices.rates_for(&prompt_of(head_request));
    let head_write = input.head_tokens as f64
        * head_rates.input_per_mtok
        * prices.write_multiplier(input.head_ttl);
    CompactionCost {
        estimate_micros: pricing::whole_micros(estimate),
        head_write_micros: pricing::whole_micros(head_write),
    }
}

pub fn low_point_warning(
    configured: CompactAt,
    point: u64,
    (model, prices): (&str, Option<&ModelInfo>),
    head_ttl: CacheTtl,
) -> Option<String> {
    if point >= POINT_FLOOR {
        return None;
    }
    let lead = format!(
        "compact_at {configured} is below {}: once the head after a compaction is above it, every turn starts with a compaction",
        group(POINT_FLOOR)
    );
    let Some(prices) = prices else {
        return Some(format!("{lead}."));
    };
    let cost = compaction_cost(
        &CompactionCostInput {
            prefix_tokens: 0,
            warm_tokens: TYPICAL_HEAD_TOKENS,
            after_anchor_tokens: 0,
            summary_tokens: SUMMARY_TOKENS_ESTIMATE,
            head_tokens: TYPICAL_HEAD_TOKENS,
            head_ttl,
            prefix_warm: true,
            tail_warm: true,
        },
        prices,
    );
    Some(format!(
        "{lead}, {} a turn to compact and {} to write the head again, at {model}'s prices.",
        wisp_protocol::marked_amount(&format_usd(usd(cost.estimate_micros)), true, false),
        wisp_protocol::marked_amount(&format_usd(usd(cost.head_write_micros)), true, false)
    ))
}

pub fn background_threshold(background_tokens: u64) -> u64 {
    TYPICAL_HEAD_TOKENS
        .saturating_add(background_tokens)
        .saturating_add(BACKGROUND_MARGIN)
}

pub fn background_point_warning(
    model: &str,
    point: u64,
    background_tokens: Option<u64>,
) -> Option<String> {
    let threshold = background_threshold(background_tokens?);
    (point < threshold).then(|| {
        format!(
            "background compaction rarely runs below about {}, and {model} compacts at {}; compaction will usually wait at the point.",
            group(threshold),
            group(point)
        )
    })
}

pub fn reread_micros(used: u64, prices: &ModelInfo) -> u64 {
    let rates = prices.rates_for(&prompt_of(used));
    pricing::whole_micros(used as f64 * rates.cache_read_per_mtok)
}

pub fn usd(micros: u64) -> f64 {
    micros as f64 / 1_000_000.0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    pub used: u64,
    pub output_tokens: u64,
    pub started_at: Option<Timestamp>,
    pub ttl: Option<CacheTtl>,
    pub same_pair: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactNow {
    pub prefix_tokens: u64,
    pub content_since: u64,
    pub next_estimate: u64,
    pub head_tokens: u64,
    pub instruction_tokens: u64,
    pub head_ttl: CacheTtl,
    pub now: Timestamp,
}

pub fn ttl_span(ttl: CacheTtl) -> SignedDuration {
    match ttl {
        CacheTtl::FiveMinutes => SignedDuration::from_mins(5),
        CacheTtl::OneHour => SignedDuration::from_hours(1),
    }
}

impl Anchor {
    fn within(&self, now: Timestamp, span: impl FnOnce(CacheTtl) -> SignedDuration) -> bool {
        match (self.same_pair, self.started_at, self.ttl) {
            (true, Some(started), Some(ttl)) => now.duration_since(started) < span(ttl),
            _ => false,
        }
    }
}

pub fn compact_now_input(anchor: Option<&Anchor>, now: &CompactNow) -> CompactionCostInput {
    let base = CompactionCostInput {
        prefix_tokens: now.prefix_tokens,
        warm_tokens: now.next_estimate.saturating_sub(now.prefix_tokens),
        after_anchor_tokens: now.instruction_tokens,
        summary_tokens: SUMMARY_TOKENS_ESTIMATE,
        head_tokens: now.head_tokens,
        head_ttl: now.head_ttl,
        prefix_warm: false,
        tail_warm: false,
    };
    let Some(anchor) = anchor else {
        return base;
    };
    CompactionCostInput {
        warm_tokens: anchor.used.saturating_sub(now.prefix_tokens),
        after_anchor_tokens: anchor
            .output_tokens
            .saturating_add(now.content_since)
            .saturating_add(now.instruction_tokens),
        prefix_warm: anchor.within(now.now, |_| ttl_span(CacheTtl::OneHour)),
        tail_warm: anchor.within(now.now, ttl_span),
        ..base
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelSettings {
    shipped: ModelTable,
    config: ConfigModels,
    saved: SavedSettings,
    session: Vec<Given>,
    overflows: Vec<(String, String, u64)>,
    flags: FlagRows,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct FlagRows {
    models: Vec<ModelRow>,
    endpoints: BTreeMap<String, Vec<ModelRow>>,
    native_web_search: BTreeMap<String, bool>,
    given: Vec<Given>,
}

fn set_flag(rows: &mut Vec<ModelRow>, model: &str, leaf: &str, value: &SettingValue) {
    if !rows.iter().any(|row| row.id == model) {
        rows.push(ModelRow::new(model));
    }
    if let Some(row) = rows.iter_mut().find(|row| row.id == model) {
        set_row_value(row, leaf, value);
    }
}

impl FlagRows {
    fn of(flags: &[Given]) -> Self {
        let mut out = Self {
            given: flags.to_vec(),
            ..Self::default()
        };
        for given in flags {
            let Target::Path(path) = &given.target else {
                continue;
            };
            let leaf = path.spec().leaf();
            match &path.place {
                Place::Model(model) => set_flag(&mut out.models, model, leaf, &given.value),
                Place::EndpointModel { endpoint, model } => set_flag(
                    out.endpoints.entry(endpoint.clone()).or_default(),
                    model,
                    leaf,
                    &given.value,
                ),
                Place::Endpoint(endpoint) => {
                    if let (names::NATIVE_WEB_SEARCH, SettingValue::Bool(on)) =
                        (path.spec().name, &given.value)
                    {
                        out.native_web_search.insert(endpoint.clone(), *on);
                    }
                }
                Place::Global
                | Place::Agent(_)
                | Place::PluginHook { .. }
                | Place::PluginSlot { .. }
                | Place::Plugin(_) => {}
            }
        }
        out
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layered<'a> {
    pub flag_endpoint: Option<&'a ModelRow>,
    pub flag: Option<&'a ModelRow>,
    pub endpoint: Option<&'a ModelRow>,
    pub config: Option<&'a ModelRow>,
    pub shipped: Option<&'a ModelRow>,
    pub native_web_search: Option<bool>,
    pub native_web_search_from: Layer,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ListedRow {
    pub row: ModelRow,
    pub source: WindowSource,
    pub max_window: Option<u64>,
}

impl ListedRow {
    pub(crate) fn of(
        list: Option<&ModelList>,
        kind: ProviderKind,
        billing: Billing,
        model: &str,
    ) -> Option<Self> {
        let entry = list.and_then(|list| list.find(model));
        if billing != Billing::ChatGptPlan {
            let source = match billing {
                Billing::Metered => WindowSource::Api,
                _ => WindowSource::Subscription,
            };
            return Some(Self {
                row: entry?.row(kind),
                source,
                max_window: None,
            });
        }
        let default = entry.and_then(ListedModel::listed_default);
        let window = default
            .map(|tokens| (tokens, WindowSource::Subscription))
            .or_else(|| bounded(model, entry.and_then(ListedModel::ceiling)));
        let mut row = match entry {
            Some(entry) => entry.row(kind),
            None if window.is_some() => ModelRow::new(model),
            None => return None,
        };
        let (tokens, source) = window.unzip();
        row.context_window = tokens;
        Some(Self {
            row,
            source: source.unwrap_or(WindowSource::Subscription),
            max_window: default.and(entry.and_then(ListedModel::maximum)),
        })
    }
}

fn bounded(model: &str, ceiling: Option<u64>) -> Option<(u64, WindowSource)> {
    let table = ModelTable::shared();
    let shipped = table
        .find(model)
        .filter(|row| row.provider == Some(ProviderKind::OpenAi))
        .and_then(|row| row.context_window);
    let cap = table
        .provider(ProviderKind::OpenAi)
        .and_then(|row| row.codex_window);
    let tokens = shipped.into_iter().chain(ceiling).min()?;
    let tokens = cap.map_or(tokens, |cap| tokens.min(cap));
    let source = if ceiling == Some(tokens) {
        WindowSource::Subscription
    } else {
        WindowSource::Shipped
    };
    Some((tokens, source))
}

#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub window: Option<Window>,
    pub compact_at: Option<(CompactAt, PointSource)>,
}

impl ModelSettings {
    pub fn new(shipped: ModelTable, config: ConfigModels) -> Self {
        Self {
            shipped,
            saved: SavedSettings::from_models(&config),
            config,
            session: Vec::new(),
            overflows: Vec::new(),
            flags: FlagRows::default(),
        }
    }

    pub fn note_overflow(
        &mut self,
        endpoint: &str,
        model: &str,
        tokens: u64,
        listed: Option<&ListedRow>,
    ) -> bool {
        let (window, ceiling) = self.ungrown(endpoint, model, listed);
        let lowers = ceiling
            .or_else(|| window.map(|window| window.tokens))
            .is_none_or(|top| tokens < top);
        if lowers {
            self.restore_overflow(endpoint, model, tokens);
        }
        lowers
    }

    pub fn restore_overflow(&mut self, endpoint: &str, model: &str, tokens: u64) {
        self.overflows
            .retain(|(at, id, _)| !(at == endpoint && id == model));
        self.overflows
            .push((endpoint.to_owned(), model.to_owned(), tokens));
        self.overflows.sort();
    }

    fn overflow_of(&self, endpoint: &str, model: &str) -> Option<u64> {
        self.overflows
            .iter()
            .find(|(at, id, _)| {
                at == endpoint && (id == model || self.canonical(id) == self.canonical(model))
            })
            .map(|(_, _, tokens)| *tokens)
    }

    #[must_use]
    pub fn with_flags(mut self, flags: &[Given]) -> Self {
        self.set_flags(flags);
        self
    }

    pub fn set_flags(&mut self, flags: &[Given]) {
        self.flags = FlagRows::of(flags);
    }

    fn flag_point(&self) -> Option<CompactAt> {
        let named = |g: &&Given| matches!(g.target, Target::Name(s) if s.name == names::COMPACT_AT);
        let given = self.flags.given.iter().find(named);
        given.and_then(|g| CompactAt::of(&g.value))
    }

    pub fn set_session(&mut self, session: Vec<Given>) {
        self.session = session;
    }

    pub fn set_saved(&mut self, saved: SavedSettings) {
        self.saved = saved;
    }

    pub fn shipped_only() -> Self {
        Self::new(
            ModelTable::shipped().unwrap_or_default(),
            ConfigModels::default(),
        )
    }

    fn canonical<'a>(&'a self, model: &'a str) -> &'a str {
        self.shipped.canonical(model)
    }

    fn matches(&self, row: &ModelRow, model: &str) -> bool {
        let family: Vec<String> = self
            .flags
            .models
            .iter()
            .chain(&self.config.models)
            .chain(std::iter::once(row))
            .filter(|other| other.id == row.id || other.aliases.contains(&row.id))
            .flat_map(|other| std::iter::once(&other.id).chain(&other.aliases))
            .cloned()
            .collect();
        self.shipped.names_model(&row.id, &family, model)
    }

    fn flag_rows(&self, endpoint: &str, model: &str) -> (Option<&ModelRow>, Option<&ModelRow>) {
        let at_endpoint = self
            .flags
            .endpoints
            .get(endpoint)
            .or_else(|| self.flags.endpoints.get(self.config.table(endpoint)))
            .and_then(|rows| self.configured(rows, model));
        (at_endpoint, self.configured(&self.flags.models, model))
    }

    fn configured<'a>(&'a self, rows: &'a [ModelRow], model: &str) -> Option<&'a ModelRow> {
        rows.iter().find(|row| self.matches(row, model))
    }

    fn set(&self, name: &str, endpoint: &str, model: &str) -> Option<Found> {
        let layers = Layers {
            session: self.session.clone(),
            flags: self.flags.given.clone(),
            env: Vec::new(),
            saved: Some(&self.saved),
            shipped: None,
        };
        let current = Current {
            endpoint: Some(endpoint.to_owned()),
            model: model.to_owned(),
        };
        let spec = settings::find(name)?;
        resolve_one(spec, &layers, &current)
            .found
            .into_iter()
            .next()
    }

    fn ungrown(
        &self,
        endpoint: &str,
        model: &str,
        listed: Option<&ListedRow>,
    ) -> (Option<Window>, Option<u64>) {
        let shipped = self.shipped.find(model).and_then(|row| row.context_window);
        let window = |listed: &ListedRow| Some((listed.row.context_window?, listed.source.clone()));
        let (documented, documented_source) = match listed.and_then(window) {
            Some((tokens, source)) => (Some(tokens), source),
            None => (shipped, WindowSource::Shipped),
        };
        let limit = self
            .set(names::CONTEXT_WINDOW, endpoint, model)
            .and_then(|found| match found.value {
                SettingValue::Tokens(tokens) => Some((tokens, sources(found.layer).0)),
                _ => None,
            });
        let overflow = self.overflow_of(endpoint, model);
        let ceiling = listed
            .filter(|_| limit.is_none())
            .and_then(|row| row.max_window)
            .map(|max| overflow.map_or(max, |tokens| tokens.min(max)));
        let window = match limit {
            Some((tokens, source)) => Some(Window {
                tokens,
                source,
                documented: documented.filter(|d| *d != tokens),
            }),
            None => documented.map(|tokens| Window {
                tokens,
                source: documented_source,
                documented: None,
            }),
        };
        let window = match overflow {
            Some(tokens) if window.as_ref().is_none_or(|w| tokens < w.tokens) => Some(Window {
                tokens,
                source: WindowSource::Overflow,
                documented: documented.filter(|d| *d != tokens),
            }),
            _ => window,
        };
        (window, ceiling)
    }

    pub fn window_room(
        &self,
        endpoint: &str,
        model: &str,
        listed: Option<&ListedRow>,
    ) -> Option<WindowRoom> {
        let (window, ceiling) = self.ungrown(endpoint, model, listed);
        let (base, max) = (window?.tokens, ceiling?);
        (max > base).then_some(WindowRoom { base, max })
    }

    pub fn resolve(
        &self,
        endpoint: &str,
        model: &str,
        listed: Option<&ListedRow>,
        max_tokens: u32,
    ) -> Resolved {
        self.resolve_with_point_origin(endpoint, model, listed, max_tokens)
            .0
    }

    pub fn resolve_with_point_origin(
        &self,
        endpoint: &str,
        model: &str,
        listed: Option<&ListedRow>,
        max_tokens: u32,
    ) -> (Resolved, Option<String>) {
        let (window, ceiling) = self.ungrown(endpoint, model, listed);
        let (compact_at, origin) = self
            .set(names::COMPACT_AT, endpoint, model)
            .and_then(|found| {
                let at = CompactAt::of(&found.value)?;
                Some(((at, sources(found.layer).1), found.origin))
            })
            .unzip();
        let window = window.map(|window| {
            let tokens = window_for(
                window.tokens,
                ceiling,
                max_tokens,
                compact_at.as_ref().map(|(at, _)| *at),
            );
            if tokens == window.tokens {
                return window;
            }
            Window {
                tokens,
                source: if self.overflow_of(endpoint, model) == Some(tokens) {
                    WindowSource::Overflow
                } else {
                    window.source
                },
                documented: Some(window.tokens),
            }
        });
        (Resolved { window, compact_at }, origin)
    }

    pub fn layered(&self, endpoint: &str, model: &str) -> Layered<'_> {
        let (flag_endpoint, flag) = self.flag_rows(endpoint, model);
        let flagged = self
            .flags
            .native_web_search
            .get(endpoint)
            .or_else(|| {
                self.flags
                    .native_web_search
                    .get(self.config.table(endpoint))
            })
            .copied();
        let saved = self
            .config
            .native_web_search
            .get(self.config.table(endpoint))
            .copied();
        Layered {
            flag_endpoint,
            flag,
            endpoint: self
                .config
                .endpoints
                .get(self.config.table(endpoint))
                .and_then(|rows| self.configured(rows, model)),
            config: self.configured(&self.config.models, model),
            shipped: self.shipped.find(model),
            native_web_search: flagged.or(saved),
            native_web_search_from: match (flagged, saved) {
                (Some(_), _) => Layer::Flag,
                (None, Some(_)) => Layer::Endpoint,
                (None, None) => Layer::Default,
            },
        }
    }

    pub fn native_compaction(&self, endpoint: &str, model: &str) -> bool {
        let rows = self.layered(endpoint, model);
        let all = Self::own_rows(&rows).into_iter().chain([rows.shipped]);
        all.flatten()
            .find_map(|row| row.native_compaction)
            .unwrap_or(false)
    }

    pub(crate) fn own_rows<'a>(rows: &Layered<'a>) -> [Option<&'a ModelRow>; 4] {
        [rows.flag_endpoint, rows.flag, rows.endpoint, rows.config]
    }

    pub fn price(&self, endpoint: &str, model: &str, kind: ProviderKind) -> Option<ModelInfo> {
        let rows = self.layered(endpoint, model);
        let own = Self::own_rows(&rows).into_iter().flatten();
        pricing::resolve(own, rows.shipped, &self.shipped, kind)
    }

    pub fn max_output(&self, endpoint: &str, model: &str, documented: Option<u32>) -> Option<u32> {
        let rows = self.layered(endpoint, model);
        let mut own = Self::own_rows(&rows).into_iter().flatten();
        own.find_map(|row| row.max_output).or(documented)
    }

    pub fn reply_limit(
        &self,
        endpoint: &str,
        model: &str,
        documented: Option<u32>,
        max_tokens: u32,
    ) -> u32 {
        self.max_output(endpoint, model, documented)
            .map_or(max_tokens, |cap| cap.min(max_tokens))
    }

    pub fn named_models(&self) -> BTreeSet<String> {
        let flagged = self
            .flags
            .models
            .iter()
            .chain(self.flags.endpoints.values().flatten());
        self.shipped
            .rows()
            .iter()
            .chain(&self.config.models)
            .chain(self.config.endpoints.values().flatten())
            .chain(flagged)
            .flat_map(|row| std::iter::once(&row.id).chain(&row.aliases))
            .cloned()
            .collect()
    }

    pub fn verbatim_user_tokens(&self, endpoint: &str, model: &str) -> Option<u64> {
        let rows = self.layered(endpoint, model);
        [rows.endpoint, rows.config]
            .into_iter()
            .flatten()
            .find_map(|row| row.verbatim_user_tokens)
    }

    pub fn low_points(
        &self,
        endpoint: &str,
        model: &str,
        reply: &dyn Fn(&str, &str) -> u32,
        head_ttl: CacheTtl,
        background: &dyn Fn(&str, &str) -> Option<u64>,
        listed: &dyn Fn(&str, &str) -> Option<ListedRow>,
    ) -> Vec<String> {
        let start = endpoint;
        let band = |row_endpoint: &str, row_model: &str| {
            let at = own(row_endpoint, endpoint);
            self.native_compaction(at, row_model)
                .then(|| background(at, row_model))
                .flatten()
        };
        let reply = |at: &str, model: &str| reply(own(at, start), model);
        let resolve = |row_endpoint: &str, row_model: &str| {
            let row = listed(own(row_endpoint, endpoint), row_model);
            self.resolve(
                row_endpoint,
                row_model,
                row.as_ref(),
                reply(row_endpoint, row_model),
            )
        };
        let flag = self
            .flag_point()
            .map(|at| (endpoint, model, at, PointSource::Flag));
        let everywhere = self
            .config
            .compact_at
            .map(|at| (endpoint, model, at, PointSource::Config));
        let models = self
            .config
            .models
            .iter()
            .map(|row| ("", row, PointSource::Config));
        let endpoints = self.config.endpoints.iter().flat_map(|(name, rows)| {
            rows.iter()
                .map(move |row| (name.as_str(), row, PointSource::Endpoint))
        });
        let flag_models = self
            .flags
            .models
            .iter()
            .map(|row| ("", row, PointSource::Flag));
        let flag_endpoints = self.flags.endpoints.iter().flat_map(|(name, rows)| {
            rows.iter()
                .map(move |row| (name.as_str(), row, PointSource::Flag))
        });
        let saved = flag_models
            .chain(flag_endpoints)
            .chain(models)
            .chain(endpoints)
            .filter_map(|(endpoint, row, source)| {
                row.compact_at
                    .map(|at| (endpoint, row.id.as_str(), at, source))
            });
        let mut out: Vec<String> = flag
            .into_iter()
            .chain(saved)
            .chain(everywhere)
            .flat_map(|(endpoint, model, at, source)| {
                let point = match resolve(endpoint, model).window {
                    Some(window) => {
                        compact_at(window.tokens, reply(endpoint, model), Some((at, source))).tokens
                    }
                    None => match at {
                        CompactAt::Tokens(tokens) => tokens,
                        CompactAt::Share(_) => return Vec::new(),
                    },
                };
                let prices = self.price(own(endpoint, start), model, ProviderKind::IMPLICIT);
                low_point_warning(at, point, (model, prices.as_ref()), head_ttl)
                    .into_iter()
                    .chain(background_point_warning(
                        model,
                        point,
                        band(endpoint, model),
                    ))
                    .collect()
            })
            .collect();
        let model_rows = self
            .flags
            .models
            .iter()
            .chain(&self.config.models)
            .map(|row| ("", row));
        let endpoint_rows = self
            .flags
            .endpoints
            .iter()
            .chain(&self.config.endpoints)
            .flat_map(|(name, rows)| rows.iter().map(move |row| (name.as_str(), row)));
        let limits = model_rows
            .chain(endpoint_rows)
            .filter(|(_, row)| row.context_window.is_some())
            .filter_map(|(at, row)| {
                let resolved = resolve(own(at, endpoint), &row.id);
                if resolved.compact_at.is_some() {
                    return None;
                }
                let point = compact_at(resolved.window?.tokens, reply(at, &row.id), None).tokens;
                background_point_warning(&row.id, point, band(at, &row.id))
            });
        for line in limits {
            if !out.contains(&line) {
                out.push(line);
            }
        }
        out
    }

    pub fn warnings(&self) -> Vec<String> {
        self.limit_warnings(
            self.config
                .models
                .iter()
                .chain(self.config.endpoints.values().flatten()),
        )
    }

    pub fn flag_warnings(&self) -> Vec<String> {
        let mut out = self.limit_warnings(
            self.flags
                .models
                .iter()
                .chain(self.flags.endpoints.values().flatten()),
        );
        out.extend(self.price_notes(&self.config.models, &self.config.endpoints));
        out.extend(self.price_notes(&self.flags.models, &self.flags.endpoints));
        out
    }

    fn price_notes(
        &self,
        models: &[ModelRow],
        endpoints: &BTreeMap<String, Vec<ModelRow>>,
    ) -> Vec<String> {
        let every = models.iter().map(|row| (None, row));
        let one = endpoints
            .iter()
            .flat_map(|(name, rows)| rows.iter().map(move |row| (Some(name.as_str()), row)));
        let accounts: BTreeSet<&String> = self
            .config
            .endpoints
            .keys()
            .chain(self.flags.endpoints.keys())
            .collect();
        every
            .chain(one)
            .filter_map(|(at, row)| {
                let (set, missing) = part_set(row)?;
                let unpriced =
                    |name: &str| self.price(name, &row.id, ProviderKind::IMPLICIT).is_none();
                if !unpriced(at.unwrap_or_default()) {
                    return None;
                }
                let model = wisp_protocol::shown_model(&row.id);
                let lead = format!("has {set} but not {missing}, so Wisp shows no cost for it");
                Some(match at {
                    Some(name) => format!("{model} on {} {lead} there", goes::shown(name)),
                    None => {
                        let priced: Vec<&str> = accounts
                            .iter()
                            .filter(|name| !unpriced(name))
                            .map(|name| goes::shown(name))
                            .collect();
                        if priced.is_empty() {
                            format!("{model} {lead}")
                        } else {
                            format!("{model} {lead}, except on {}", priced.join(", "))
                        }
                    }
                })
            })
            .collect()
    }

    fn limit_warnings<'a>(&self, rows: impl Iterator<Item = &'a ModelRow>) -> Vec<String> {
        rows.filter_map(|row| {
            let limit = row.context_window?;
            let shipped = self.shipped.find(&row.id)?;
            let documented = shipped.context_window?;
            let source = shipped.source_of("context_window").unwrap_or_default();
            above_documented(&row.id, limit, documented, source)
        })
        .collect()
    }
}

fn part_set(row: &ModelRow) -> Option<(String, String)> {
    let keys = pricing::price_keys(row);
    let (set, missing): (Vec<_>, Vec<_>) = keys.iter().partition(|(_, given)| given.is_some());
    let words = |keys: Vec<&(&str, Option<u64>)>, joint: &str| {
        keys.iter()
            .map(|(key, _)| *key)
            .collect::<Vec<_>>()
            .join(joint)
    };
    (!set.is_empty() && !missing.is_empty()).then(|| (words(set, " and "), words(missing, " or ")))
}

fn sources(layer: Layer) -> (WindowSource, PointSource) {
    match layer {
        Layer::Session => (WindowSource::Session, PointSource::Session),
        Layer::Flag => (WindowSource::Flag, PointSource::Flag),
        Layer::EndpointModel => (WindowSource::Endpoint, PointSource::Endpoint),
        _ => (WindowSource::Config, PointSource::Config),
    }
}

fn own<'a>(row_endpoint: &'a str, start: &'a str) -> &'a str {
    if row_endpoint.is_empty() {
        start
    } else {
        row_endpoint
    }
}

pub fn listed_only_warning(
    provider: &dyn crate::provider::Provider,
    model: &str,
    configured: bool,
) -> Option<String> {
    if provider.kind() != ProviderKind::Gemini || ModelTable::shared().find(model).is_some() {
        return None;
    }
    let caps = provider.capabilities(model)?;
    (caps.efforts.is_empty()
        && !caps.adaptive_thinking
        && caps.thinking_budgets.is_none()
        && caps.images.is_none())
    .then(|| {
        let shown = wisp_protocol::shown_model(model);
        if configured {
            format!("No capability data for {shown} yet: thinking, effort and images are off.")
        } else {
            format!(
                "No data for {shown} yet: thinking, effort and images are off; add [models.\"{}\"] in {} to set price or context window.",
                wisp_protocol::model_or_placeholder(model),
                wisp_protocol::CONFIG_FILE
            )
        }
    })
}

#[cfg(test)]
mod tests;
