use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::ops::Range;
use std::sync::LazyLock;

use toml::Spanned;
use toml::de::{DeString, DeTable, DeValue};

use crate::agent::DEFAULT_MAX_TOKENS;
use crate::config::{from_toml, usd_micros};
use crate::context::{CompactAt, LimitTooSmall, check_limit};
use crate::endpoints::ProviderKind;
use crate::provider::anthropic::subscription::Identity;
use crate::provider::gemini::antigravity::Identity as AntigravityIdentity;
use crate::provider::{ImageCaps, ModelCapabilities, ThinkingBudgets};
use wisp_media::Tier;
use wisp_protocol::settings::{
    A_STRING, EffortVariant, SettingValue, TOKEN_COUNT, TOKENS_OR_SHARE, TRUE_OR_FALSE, USD_WORDS,
    find, parse_tokens, quoted_or, siblings,
};
use wisp_protocol::{CONFIG_FILE, Effort, MODEL_IDS_RULE, model_or_placeholder, model_words};

pub mod list;

pub static WEB_SEARCH_ALLOWED: LazyLock<String> = LazyLock::new(|| {
    let versions = wisp_protocol::settings::WEB_SEARCH_VERSIONS.join(", ");
    format!("a list of known web search tool versions ({versions})")
});

pub const SHIPPED: &str = include_str!("../data/models.toml");

static PROVIDER_ALLOWED: LazyLock<String> = LazyLock::new(ProviderKind::accepted);

const MULTIPLIER_WORDS: &str = "a number, such as 1.25";

const POSITIVE_TOKENS: &str = "a positive number of tokens";

const TOP_FIELDS: [&str; 2] = ["providers", "models"];

const LONG_PROMPT_FIELDS: [&str; 4] = ["over", "price_input", "price_output", "price_cache_read"];

static LONG_PROMPT_ALLOWED: LazyLock<String> =
    LazyLock::new(|| format!("a table of {}", LONG_PROMPT_FIELDS.join(", ")));

static EFFORT_ALLOWED: LazyLock<String> = LazyLock::new(|| quoted_or(&Effort::WORDS));

static EFFORTS_ALLOWED: LazyLock<String> =
    LazyLock::new(|| format!("a list of {}", *EFFORT_ALLOWED));

const IMAGE_FIELDS: [&str; 2] = ["tier", "per_request"];

static IMAGES_ALLOWED: LazyLock<String> = LazyLock::new(|| {
    let tiers = quoted_or(&Tier::NAMED.map(|(name, _)| name));
    let [tier, count] = IMAGE_FIELDS;
    format!("a table of {tier} ({tiers}) and {count}")
});

static LEVELS: [Effort; 5] = Effort::ALL;
static EFFORT_SETS: LazyLock<Vec<Vec<Effort>>> = LazyLock::new(|| {
    (0..1_usize << LEVELS.len())
        .map(|mask| {
            let named = |(bit, _): &(usize, &Effort)| mask & (1 << bit) != 0;
            LEVELS
                .iter()
                .enumerate()
                .filter(named)
                .map(|(_, level)| *level)
                .collect()
        })
        .collect()
});

pub(crate) fn effort_set(levels: &[Effort]) -> &'static [Effort] {
    let named = |(_, level): &(usize, &Effort)| levels.contains(level);
    let mask = LEVELS.iter().enumerate().filter(named);
    let mask = mask.fold(0, |mask, (bit, _)| mask | 1 << bit);
    EFFORT_SETS.get(mask).map_or(&[], Vec::as_slice)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Quote {
    Own(&'static str),
    Shared(&'static str),
    Provenance,
}

const SHIPPED_FIELDS: [(&str, Quote); 25] = [
    ("context_window", Quote::Provenance),
    ("native_compaction", Quote::Own("native_compaction")),
    ("web_search", Quote::Own("web_search")),
    ("aliases", Quote::Provenance),
    ("listed", Quote::Provenance),
    ("provider", Quote::Provenance),
    ("price_input", Quote::Shared("price")),
    ("price_output", Quote::Shared("price")),
    ("price_cache_read", Quote::Shared("price")),
    ("long_prompt", Quote::Shared("price")),
    ("bill_tool_use_prompt", Quote::Shared("price")),
    ("min_cacheable", Quote::Shared("min_cacheable")),
    ("max_output", Quote::Own("efforts")),
    ("efforts", Quote::Own("efforts")),
    ("effort_variant", Quote::Shared("antigravity")),
    ("default_effort", Quote::Own("efforts")),
    ("per_message_effort", Quote::Own("efforts")),
    ("inline_tools", Quote::Shared("inline_tools")),
    ("tools_after_results", Quote::Shared("tools_after_results")),
    ("deferred_tools", Quote::Shared("deferred_tools")),
    ("adaptive_thinking", Quote::Own("efforts")),
    ("thinking_budgets", Quote::Own("efforts")),
    ("images", Quote::Own("images")),
    ("source", Quote::Provenance),
    ("sources", Quote::Provenance),
];

const PROVIDER_FIELDS: [(&str, Quote); 19] = [
    ("claude_code", Quote::Own("claude_code")),
    ("codex", Quote::Own("codex")),
    ("write_5m", Quote::Own("cache_writes")),
    ("write_1h", Quote::Own("cache_writes")),
    ("write_30m", Quote::Own("cache_writes")),
    ("write_12h", Quote::Own("cache_writes")),
    ("web_search_price", Quote::Own("web_search_price")),
    ("message_threads", Quote::Own("message_threads")),
    ("weekly_limits", Quote::Own("weekly_limits")),
    (
        "account_bound_thinking",
        Quote::Own("account_bound_thinking"),
    ),
    ("default_model", Quote::Own("default_model")),
    ("web_search_model", Quote::Own("web_search_model")),
    ("web_search_by_call", Quote::Own("web_search_by_call")),
    ("antigravity", Quote::Own("antigravity")),
    (
        "subscription_additional_tools",
        Quote::Own("subscription_additional_tools"),
    ),
    ("api_additional_tools", Quote::Own("api_additional_tools")),
    (
        "subscription_tools_after_outputs",
        Quote::Own("subscription_tools_after_outputs"),
    ),
    (
        "api_tools_after_outputs",
        Quote::Own("api_tools_after_outputs"),
    ),
    ("sources", Quote::Provenance),
];

fn field_names(fields: &[(&str, Quote)]) -> String {
    let names: Vec<&str> = fields.iter().map(|(name, _)| *name).collect();
    names.join(", ")
}

static SHIPPED_ALLOWED: LazyLock<String> = LazyLock::new(|| field_names(&SHIPPED_FIELDS));

static PROVIDER_FIELDS_ALLOWED: LazyLock<String> = LazyLock::new(|| field_names(&PROVIDER_FIELDS));

pub const MODEL_FIELDS: &str = "context_window, compact_at, native_compaction, verbatim_user_tokens, \
background_compaction, background_tokens, web_search, aliases, max_output, price_input, \
price_output, price_cache_read";
pub const ENDPOINT_MODEL_FIELDS: &str = "context_window, compact_at, native_compaction, \
verbatim_user_tokens, background_compaction, background_tokens, web_search, max_output, \
price_input, price_output, price_cache_read";

fn is_field(fields: &str, name: &str) -> bool {
    fields.split(", ").any(|field| field == name)
}

fn shipped_field(name: &str) -> bool {
    SHIPPED_FIELDS.iter().any(|(field, _)| *field == name)
}

fn shipped_only(name: &str) -> bool {
    shipped_field(name) && !is_field(MODEL_FIELDS, name)
}

fn config_only(name: &str) -> bool {
    is_field(MODEL_FIELDS, name) && !shipped_field(name)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Site {
    Shipped,
    Models,
    EndpointModels,
}

impl Site {
    pub fn allowed(self) -> &'static str {
        match self {
            Site::Shipped => &SHIPPED_ALLOWED,
            Site::Models => MODEL_FIELDS,
            Site::EndpointModels => ENDPOINT_MODEL_FIELDS,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRow {
    pub id: String,
    pub aliases: Vec<String>,
    pub context_window: Option<u64>,
    pub compact_at: Option<CompactAt>,
    pub native_compaction: Option<bool>,
    pub verbatim_user_tokens: Option<u64>,
    pub background_compaction: Option<bool>,
    pub background_tokens: Option<u64>,
    pub web_search: Option<Vec<String>>,
    pub listed: bool,
    pub provider: Option<ProviderKind>,
    pub price_input: Option<u64>,
    pub price_output: Option<u64>,
    pub price_cache_read: Option<u64>,
    pub long_prompt: Option<LongPromptRow>,
    pub bill_tool_use_prompt: bool,
    pub min_cacheable: Option<u64>,
    pub max_output: Option<u32>,
    pub capabilities: Option<ModelCapabilities>,
    pub effort_variant: Option<EffortVariant>,
    pub source: Option<String>,
    pub sources: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LongPromptRow {
    pub over: u64,
    pub price_input: u64,
    pub price_output: u64,
    pub price_cache_read: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderRow {
    pub(crate) claude_code: Option<Identity>,
    pub(crate) codex_version: Option<String>,
    pub(crate) codex_window: Option<u64>,
    pub write_5m: Option<u64>,
    pub write_1h: Option<u64>,
    pub write_30m: Option<u64>,
    pub write_12h: Option<u64>,
    pub web_search_price: Option<u64>,
    pub message_threads: Vec<String>,
    pub weekly_limits: BTreeMap<String, Vec<String>>,
    pub account_bound_thinking: Vec<String>,
    pub default_model: Option<String>,
    pub web_search_model: Option<String>,
    pub web_search_by_call: bool,
    pub(crate) antigravity: Option<AntigravityIdentity>,
    pub subscription_additional_tools: Vec<String>,
    pub api_additional_tools: Vec<String>,
    pub subscription_tools_after_outputs: Vec<String>,
    pub api_tools_after_outputs: Vec<String>,
    pub sources: BTreeMap<String, String>,
}

impl ModelRow {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            aliases: Vec::new(),
            context_window: None,
            compact_at: None,
            native_compaction: None,
            verbatim_user_tokens: None,
            background_compaction: None,
            background_tokens: None,
            web_search: None,
            listed: false,
            provider: None,
            price_input: None,
            price_output: None,
            price_cache_read: None,
            long_prompt: None,
            bill_tool_use_prompt: false,
            min_cacheable: None,
            max_output: None,
            capabilities: None,
            effort_variant: None,
            source: None,
            sources: BTreeMap::new(),
        }
    }

    pub fn answers_to(&self, model: &str) -> bool {
        self.id == model || self.aliases.iter().any(|a| a == model)
    }

    pub fn source_of(&self, field: &str) -> Option<&str> {
        self.sources
            .get(field)
            .or(self.source.as_ref())
            .map(String::as_str)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowProblem {
    Syntax,
    UnknownTop,
    NotTable {
        what: &'static str,
    },
    UnknownField {
        site: Site,
    },
    UnknownProviderField,
    ConfigOnly {
        field: String,
    },
    ShippedOnly {
        field: String,
    },
    NotAtEndpoint {
        field: &'static str,
    },
    WrongType {
        field: &'static str,
        expected: &'static str,
    },
    BadValue {
        field: &'static str,
        allowed: &'static str,
    },
    MissingSource {
        model: String,
    },
    MissingFieldSource {
        table: &'static str,
        name: String,
        field: &'static str,
        key: &'static str,
    },
    Duplicate {
        model: String,
    },
    LimitTooSmall(LimitTooSmall),
}

impl fmt::Display for RowProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RowProblem::Syntax => f.write_str("not valid TOML"),
            RowProblem::UnknownTop => {
                write!(f, "unknown field; allowed: {}", TOP_FIELDS.join(", "))
            }
            RowProblem::NotTable { what } => write!(f, "{what} must be a table"),
            RowProblem::UnknownField { site } => {
                write!(
                    f,
                    "unknown field in a model table; allowed: {}",
                    site.allowed()
                )
            }
            RowProblem::UnknownProviderField => write!(
                f,
                "unknown field in a provider table; allowed: {}",
                *PROVIDER_FIELDS_ALLOWED
            ),
            RowProblem::ConfigOnly { field } => write!(
                f,
                "{field} is a {CONFIG_FILE} setting; the shipped model file can't set it"
            ),
            RowProblem::ShippedOnly { field } => write!(
                f,
                "{field} belongs to the shipped model file, not {CONFIG_FILE}"
            ),
            RowProblem::NotAtEndpoint { field } => write!(
                f,
                "{field} goes under [models.\"ID\"], not under an account's models"
            ),
            RowProblem::WrongType { field, expected } => write!(f, "{field} must be {expected}"),
            RowProblem::BadValue { field, allowed } => write!(f, "{field} must be {allowed}"),
            RowProblem::MissingSource { model } => {
                write!(
                    f,
                    "[models.\"{}\"] needs source, the page its values come from",
                    model_or_placeholder(model)
                )
            }
            RowProblem::MissingFieldSource {
                table,
                name,
                field,
                key,
            } => write!(
                f,
                "[{table}.\"{}\"] sets {field}, so sources needs {key}",
                model_or_placeholder(name)
            ),
            RowProblem::Duplicate { model } => {
                write!(f, "{} names more than one model row", model_words(model))
            }
            RowProblem::LimitTooSmall(limit) => write!(f, "{limit}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowError {
    pub span: Option<Range<usize>>,
    pub problem: RowProblem,
}

impl RowError {
    fn at(span: Range<usize>, problem: RowProblem) -> Self {
        Self {
            span: Some(span),
            problem,
        }
    }

    pub fn line(&self, text: &str) -> Option<usize> {
        let span = self.span.as_ref()?;
        let end = span.start.min(text.len());
        Some(
            text.as_bytes()[..end]
                .iter()
                .filter(|b| **b == b'\n')
                .count()
                + 1,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShippedError {
    pub line: Option<usize>,
    pub problem: RowProblem,
}

impl fmt::Display for ShippedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(line) => write!(f, "the shipped model file, line {line}: {}", self.problem),
            None => write!(f, "the shipped model file: {}", self.problem),
        }
    }
}

impl std::error::Error for ShippedError {}

type Entry<'a, 'i> = (&'a Spanned<DeString<'i>>, &'a Spanned<DeValue<'i>>);

pub(crate) fn in_order<'a, 'i>(table: &'a DeTable<'i>) -> Vec<Entry<'a, 'i>> {
    let mut entries: Vec<Entry<'a, 'i>> = table.iter().collect();
    entries.sort_by_key(|(key, _)| key.span().start);
    entries
}

fn string(field: &'static str, at: &Range<usize>, value: &DeValue<'_>) -> Result<String, RowError> {
    value.as_str().map(str::to_owned).ok_or_else(|| {
        RowError::at(
            at.clone(),
            RowProblem::WrongType {
                field,
                expected: A_STRING,
            },
        )
    })
}

pub(crate) fn integer(value: &DeValue<'_>) -> Option<u64> {
    let int = value.as_integer()?;
    u64::from_str_radix(int.as_str(), int.radix()).ok()
}

fn token_count(
    field: &'static str,
    at: &Range<usize>,
    value: &DeValue<'_>,
) -> Result<u64, RowError> {
    let wrong = || {
        RowError::at(
            at.clone(),
            RowProblem::WrongType {
                field,
                expected: TOKEN_COUNT,
            },
        )
    };
    match value {
        DeValue::String(text) => parse_tokens(text).ok_or_else(wrong),
        DeValue::Integer(_) => integer(value).ok_or_else(|| {
            RowError::at(
                at.clone(),
                RowProblem::BadValue {
                    field,
                    allowed: TOKEN_COUNT,
                },
            )
        }),
        _ => Err(wrong()),
    }
}

fn positive_tokens(
    field: &'static str,
    at: &Range<usize>,
    value: &DeValue<'_>,
) -> Result<u64, RowError> {
    match token_count(field, at, value) {
        Ok(0)
        | Err(RowError {
            problem: RowProblem::BadValue { .. },
            ..
        }) => Err(RowError::at(
            at.clone(),
            RowProblem::BadValue {
                field,
                allowed: POSITIVE_TOKENS,
            },
        )),
        counted => counted,
    }
}

fn compact_at(at: &Range<usize>, value: &DeValue<'_>) -> Result<CompactAt, RowError> {
    let bad = || {
        RowError::at(
            at.clone(),
            RowProblem::BadValue {
                field: "compact_at",
                allowed: TOKENS_OR_SHARE,
            },
        )
    };
    if value.is_integer() {
        return integer(value).map(CompactAt::Tokens).ok_or_else(bad);
    }
    let Some(text) = value.as_str() else {
        return Err(RowError::at(
            at.clone(),
            RowProblem::WrongType {
                field: "compact_at",
                expected: TOKENS_OR_SHARE,
            },
        ));
    };
    CompactAt::parse(text).ok_or_else(bad)
}

fn listed(
    (field, spec): (&'static str, &str),
    allowed: &'static str,
    at: &Range<usize>,
    value: &DeValue<'_>,
) -> Result<Vec<String>, RowError> {
    match find(spec).and_then(|spec| from_toml(spec, value)) {
        Some(SettingValue::List(items)) => Ok(items),
        _ => Err(bad_value(field, allowed, at)),
    }
}

fn weekly_limits(
    at: &Range<usize>,
    value: &DeValue<'_>,
) -> Result<BTreeMap<String, Vec<String>>, RowError> {
    let bad = || {
        bad_value(
            "weekly_limits",
            "a table of family names and model ID lists",
            at,
        )
    };
    in_order(value.as_table().ok_or_else(bad)?)
        .into_iter()
        .map(|(key, ids)| {
            let name: &str = key.get_ref();
            if !wisp_protocol::valid_model_id(name) {
                return Err(bad());
            }
            let models = listed(
                ("weekly_limits", "aliases"),
                MODEL_IDS_RULE,
                at,
                ids.get_ref(),
            )?;
            Ok((name.to_owned(), models))
        })
        .collect()
}

fn provider(at: &Range<usize>, name: Option<&str>) -> Result<ProviderKind, RowError> {
    name.and_then(ProviderKind::parse).ok_or_else(|| {
        RowError::at(
            at.clone(),
            RowProblem::BadValue {
                field: "provider",
                allowed: &PROVIDER_ALLOWED,
            },
        )
    })
}

fn decimal(
    field: &'static str,
    allowed: &'static str,
    at: &Range<usize>,
    value: &DeValue<'_>,
) -> Result<u64, RowError> {
    usd_micros(value).ok_or_else(|| bad_value(field, allowed, at))
}

fn boolean(field: &'static str, at: &Range<usize>, value: &DeValue<'_>) -> Result<bool, RowError> {
    value.as_bool().ok_or_else(|| {
        RowError::at(
            at.clone(),
            RowProblem::WrongType {
                field,
                expected: TRUE_OR_FALSE,
            },
        )
    })
}

pub(crate) fn bad_value(field: &'static str, allowed: &'static str, at: &Range<usize>) -> RowError {
    RowError::at(at.clone(), RowProblem::BadValue { field, allowed })
}

pub(crate) fn table_of<'a, 'i, const N: usize>(
    names: [&str; N],
    value: &'a DeValue<'i>,
    bad: &impl Fn() -> RowError,
) -> Result<[Option<&'a DeValue<'i>>; N], RowError> {
    let mut given = [None; N];
    for (key, item) in in_order(value.as_table().ok_or_else(bad)?) {
        let name: &str = key.get_ref();
        let slot = names.iter().position(|field| *field == name);
        *slot.and_then(|slot| given.get_mut(slot)).ok_or_else(bad)? = Some(item.get_ref());
    }
    Ok(given)
}

fn effort(value: &DeValue<'_>) -> Option<Effort> {
    value.as_str()?.parse().ok()
}

fn efforts(at: &Range<usize>, value: &DeValue<'_>) -> Result<&'static [Effort], RowError> {
    let bad = || bad_value("efforts", &EFFORTS_ALLOWED, at);
    let items = value.as_array().ok_or_else(bad)?;
    let levels: Option<Vec<Effort>> = items.iter().map(|item| effort(item.get_ref())).collect();
    Ok(effort_set(&levels.ok_or_else(bad)?))
}

const EFFORT_VARIANT_ALLOWED: &str =
    "a table of family (a model ID) and effort (an effort level), without efforts";

fn effort_variant(at: &Range<usize>, value: &DeValue<'_>) -> Result<EffortVariant, RowError> {
    let bad = || bad_value("effort_variant", EFFORT_VARIANT_ALLOWED, at);
    let [Some(family), Some(level)] = table_of(["family", "effort"], value, &bad)? else {
        return Err(bad());
    };
    let family = family
        .as_str()
        .filter(|id| wisp_protocol::valid_model_id(id));
    Ok(EffortVariant {
        family: family.ok_or_else(bad)?.to_owned(),
        effort: effort(level).ok_or_else(bad)?,
    })
}

const THINKING_BUDGETS_ALLOWED: &str =
    "a non-empty table mapping effort levels to positive token counts";
const THINKING_BUDGET_FIELDS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

fn thinking_budgets(at: &Range<usize>, value: &DeValue<'_>) -> Result<ThinkingBudgets, RowError> {
    let bad = || bad_value("thinking_budgets", THINKING_BUDGETS_ALLOWED, at);
    let slots = table_of(THINKING_BUDGET_FIELDS, value, &bad)?;
    if slots.iter().all(Option::is_none) {
        return Err(bad());
    }
    let mut parsed = [None; 5];
    for (slot, raw) in parsed.iter_mut().zip(slots) {
        if let Some(raw) = raw {
            let count = integer(raw)
                .filter(|n| *n > 0)
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(bad)?;
            *slot = Some(count);
        }
    }
    let [low, medium, high, xhigh, max] = parsed;
    Ok(ThinkingBudgets {
        low,
        medium,
        high,
        xhigh,
        max,
    })
}

fn images(at: &Range<usize>, value: &DeValue<'_>) -> Result<ImageCaps, RowError> {
    let bad = || bad_value("images", &IMAGES_ALLOWED, at);
    let [Some(tier), Some(per_request)] = table_of(IMAGE_FIELDS, value, &bad)? else {
        return Err(bad());
    };
    let named = Tier::NAMED
        .iter()
        .find(|(name, _)| Some(*name) == tier.as_str());
    let per_request = integer(per_request).filter(|count| *count > 0);
    let per_request = per_request.and_then(|count| usize::try_from(count).ok());
    Ok(ImageCaps::new(
        named.ok_or_else(bad)?.1,
        per_request.ok_or_else(bad)?,
    ))
}

fn long_prompt(at: &Range<usize>, value: &DeValue<'_>) -> Result<LongPromptRow, RowError> {
    let bad = || bad_value("long_prompt", &LONG_PROMPT_ALLOWED, at);
    let [Some(over), Some(input), Some(output), Some(read)] =
        table_of(LONG_PROMPT_FIELDS, value, &bad)?
    else {
        return Err(bad());
    };
    Ok(LongPromptRow {
        over: integer(over).filter(|over| *over > 0).ok_or_else(bad)?,
        price_input: usd_micros(input).ok_or_else(bad)?,
        price_output: usd_micros(output).ok_or_else(bad)?,
        price_cache_read: usd_micros(read).ok_or_else(bad)?,
    })
}

fn check_sources(
    (table, id): (&'static str, &Spanned<DeString<'_>>),
    keys: &[Entry<'_, '_>],
    rules: &[(&'static str, Quote)],
    own: &BTreeMap<String, String>,
    shared: Option<&BTreeMap<String, String>>,
    unknown: RowProblem,
) -> Result<(), RowError> {
    for (key, _) in keys {
        let name: &str = key.get_ref();
        let Some((field, quote)) = rules.iter().find(|(field, _)| *field == name) else {
            return Err(RowError::at(key.span(), unknown));
        };
        let (source, tables) = match quote {
            Quote::Own(source) => (source, [Some(own), None]),
            Quote::Shared(source) => (source, [Some(own), shared]),
            Quote::Provenance => continue,
        };
        if !tables
            .into_iter()
            .flatten()
            .any(|t| t.contains_key(*source))
        {
            return Err(RowError::at(
                id.span(),
                RowProblem::MissingFieldSource {
                    table,
                    name: id.get_ref().to_string(),
                    field,
                    key: source,
                },
            ));
        }
    }
    Ok(())
}

fn sources(at: &Range<usize>, value: &DeValue<'_>) -> Result<BTreeMap<String, String>, RowError> {
    let wrong = || {
        RowError::at(
            at.clone(),
            RowProblem::WrongType {
                field: "sources",
                expected: "a table of page URLs by field",
            },
        )
    };
    let table = value.as_table().ok_or_else(wrong)?;
    let mut out = BTreeMap::new();
    for (key, page) in in_order(table) {
        let page = page.get_ref().as_str().ok_or_else(wrong)?;
        out.insert(key.get_ref().to_string(), page.to_owned());
    }
    Ok(out)
}

pub fn walk_row(
    site: Site,
    id: &Spanned<DeString<'_>>,
    value: &Spanned<DeValue<'_>>,
) -> Result<ModelRow, RowError> {
    let Some(table) = value.get_ref().as_table() else {
        return Err(RowError::at(
            id.span(),
            RowProblem::NotTable {
                what: "a model entry",
            },
        ));
    };
    let mut row = ModelRow::new(id.get_ref().to_string());
    let mut described = None;
    let mut caps = ModelCapabilities::default();
    for (field, value) in in_order(table) {
        let at = field.span();
        let value = value.get_ref();
        let name: &str = field.get_ref();
        if site == Site::Shipped && config_only(name) {
            return Err(RowError::at(
                at,
                RowProblem::ConfigOnly {
                    field: name.to_owned(),
                },
            ));
        }
        if site != Site::Shipped && shipped_only(name) {
            return Err(RowError::at(
                at,
                RowProblem::ShippedOnly {
                    field: name.to_owned(),
                },
            ));
        }
        match name {
            "context_window" => {
                row.context_window = Some(positive_tokens("context_window", &at, value)?);
            }
            "native_compaction" => {
                row.native_compaction = Some(boolean("native_compaction", &at, value)?);
            }
            "compact_at" => row.compact_at = Some(compact_at(&at, value)?),
            "verbatim_user_tokens" => {
                row.verbatim_user_tokens = Some(token_count("verbatim_user_tokens", &at, value)?);
            }
            "background_compaction" => {
                row.background_compaction = Some(boolean("background_compaction", &at, value)?);
            }
            "background_tokens" => {
                row.background_tokens = Some(positive_tokens("background_tokens", &at, value)?);
            }
            "web_search" => {
                let spec = ("web_search", "web_search");
                row.web_search = Some(listed(spec, &WEB_SEARCH_ALLOWED, &at, value)?);
            }
            "aliases" if site == Site::EndpointModels => {
                return Err(RowError::at(
                    at,
                    RowProblem::NotAtEndpoint { field: "aliases" },
                ));
            }
            "aliases" => row.aliases = listed(("aliases", "aliases"), MODEL_IDS_RULE, &at, value)?,
            "listed" => row.listed = boolean("listed", &at, value)?,
            "provider" => row.provider = Some(provider(&at, value.as_str())?),
            "price_input" => row.price_input = Some(decimal("price_input", USD_WORDS, &at, value)?),
            "price_output" => {
                row.price_output = Some(decimal("price_output", USD_WORDS, &at, value)?);
            }
            "price_cache_read" => {
                row.price_cache_read = Some(decimal("price_cache_read", USD_WORDS, &at, value)?);
            }
            "long_prompt" => row.long_prompt = Some(long_prompt(&at, value)?),
            "bill_tool_use_prompt" => {
                row.bill_tool_use_prompt = boolean("bill_tool_use_prompt", &at, value)?;
            }
            "min_cacheable" => {
                row.min_cacheable = Some(positive_tokens("min_cacheable", &at, value)?);
            }
            "max_output" => {
                let tokens = u32::try_from(positive_tokens("max_output", &at, value)?);
                let bad = || bad_value("max_output", POSITIVE_TOKENS, &at);
                row.max_output = Some(tokens.map_err(|_| bad())?);
            }
            "efforts" => described = Some(efforts(&at, value)?),
            "effort_variant" => row.effort_variant = Some(effort_variant(&at, value)?),
            "default_effort" => {
                let bad = || bad_value("default_effort", &EFFORT_ALLOWED, &at);
                caps.default_effort = Some(effort(value).ok_or_else(bad)?);
            }
            "per_message_effort" => {
                caps.per_message_effort = boolean("per_message_effort", &at, value)?;
            }
            "inline_tools" => caps.inline_tools = boolean("inline_tools", &at, value)?,
            "tools_after_results" => {
                caps.tools_after_results = boolean("tools_after_results", &at, value)?
            }
            "deferred_tools" => caps.deferred_tools = boolean("deferred_tools", &at, value)?,
            "adaptive_thinking" => {
                caps.adaptive_thinking = boolean("adaptive_thinking", &at, value)?;
            }
            "thinking_budgets" => {
                caps.thinking_budgets = Some(thinking_budgets(&at, value)?);
            }
            "images" => caps.images = Some(images(&at, value)?),
            "source" => row.source = Some(string("source", &at, value)?),
            "sources" => row.sources = sources(&at, value)?,
            _ => return Err(RowError::at(at, RowProblem::UnknownField { site })),
        }
    }
    if row.effort_variant.is_some() {
        if described.is_some() {
            return Err(bad_value(
                "effort_variant",
                EFFORT_VARIANT_ALLOWED,
                &id.span(),
            ));
        }
        described = Some(&[]);
        caps.effort_in_id = true;
    }
    row.capabilities = described.map(|efforts| ModelCapabilities {
        efforts,
        max_output: row.max_output,
        ..caps
    });
    if site == Site::Shipped && row.source.is_none() {
        return Err(RowError::at(
            id.span(),
            RowProblem::MissingSource {
                model: row.id.clone(),
            },
        ));
    }
    Ok(row)
}

pub fn walk_models(
    site: Site,
    at: Range<usize>,
    value: &DeValue<'_>,
) -> Result<Vec<ModelRow>, RowError> {
    let Some(table) = value.as_table() else {
        return Err(RowError::at(at, RowProblem::NotTable { what: "models" }));
    };
    let mut rows: Vec<ModelRow> = Vec::new();
    let mut seen = BTreeSet::new();
    for (id, entry) in in_order(table) {
        let row = walk_row(site, id, entry)?;
        for name in std::iter::once(&row.id).chain(&row.aliases) {
            if !seen.insert(name.clone()) {
                return Err(RowError::at(
                    id.span(),
                    RowProblem::Duplicate {
                        model: name.clone(),
                    },
                ));
            }
        }
        rows.push(row);
    }
    Ok(rows)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigModels {
    pub compact_at: Option<CompactAt>,
    pub models: Vec<ModelRow>,
    pub endpoints: BTreeMap<String, Vec<ModelRow>>,
    pub native_web_search: BTreeMap<String, bool>,
    pub tables: std::collections::BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigModelsError {
    pub line: Option<usize>,
    pub problem: RowProblem,
}

impl fmt::Display for ConfigModelsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(line) => write!(f, "line {line}: {}", self.problem),
            None => write!(f, "{}", self.problem),
        }
    }
}

pub fn floor_checked(
    site: Site,
    at: Range<usize>,
    value: &DeValue<'_>,
) -> Result<Vec<ModelRow>, RowError> {
    let rows = walk_models(site, at.clone(), value)?;
    let spans: Vec<Range<usize>> = value
        .as_table()
        .map(|table| in_order(table).iter().map(|(id, _)| id.span()).collect())
        .unwrap_or_default();
    for (index, row) in rows.iter().enumerate() {
        if let Some(window) = row.context_window {
            let span = spans.get(index).cloned().unwrap_or_else(|| at.clone());
            check_limit(&row.id, window, DEFAULT_MAX_TOKENS)
                .map_err(|e| RowError::at(span, RowProblem::LimitTooSmall(e)))?;
        }
    }
    Ok(rows)
}

impl ConfigModels {
    pub fn table<'a>(&self, endpoint: &'a str) -> &'a str {
        crate::endpoints::table_name(endpoint, |name| self.tables.contains(name))
    }

    pub fn parse(text: &str) -> Result<Self, ConfigModelsError> {
        Self::walk(text).map_err(|e| ConfigModelsError {
            line: e.line(text),
            problem: e.problem,
        })
    }

    fn walk(text: &str) -> Result<Self, RowError> {
        let root = DeTable::parse(text).map_err(|e| RowError {
            span: e.span(),
            problem: RowProblem::Syntax,
        })?;
        let mut out = Self::default();
        for (key, value) in in_order(root.get_ref()) {
            match &**key.get_ref() {
                "compact_at" => out.compact_at = compact_at(&key.span(), value.get_ref()).ok(),
                "models" => out.models = floor_checked(Site::Models, key.span(), value.get_ref())?,
                "endpoints" => {
                    let Some(endpoints) = value.get_ref().as_table() else {
                        continue;
                    };
                    for (name, entry) in in_order(endpoints) {
                        out.tables.insert(name.get_ref().to_string());
                        let Some(entry) = entry.get_ref().as_table() else {
                            continue;
                        };
                        for (field, models) in in_order(entry) {
                            if &**field.get_ref() == "native_web_search" {
                                let (at, on) = (field.span(), models.get_ref());
                                let on = boolean("native_web_search", &at, on)?;
                                out.native_web_search.insert(name.get_ref().to_string(), on);
                            }
                            if &**field.get_ref() == "models" {
                                let rows = floor_checked(
                                    Site::EndpointModels,
                                    field.span(),
                                    models.get_ref(),
                                )?;
                                out.endpoints.insert(name.get_ref().to_string(), rows);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(out)
    }
}

fn codex_version_parts(version: &str) -> Option<[u64; 3]> {
    let mut parts = version.split('.');
    let mut next_number = || {
        let part = parts.next()?;
        if part.starts_with('+') {
            return None;
        }
        part.parse().ok()
    };
    let numbers = [next_number()?, next_number()?, next_number()?];
    parts.next().is_none().then_some(numbers)
}

fn walk_providers(
    at: Range<usize>,
    value: &DeValue<'_>,
) -> Result<BTreeMap<ProviderKind, ProviderRow>, RowError> {
    let Some(table) = value.as_table() else {
        return Err(RowError::at(at, RowProblem::NotTable { what: "providers" }));
    };
    let mut out = BTreeMap::new();
    for (name, entry) in in_order(table) {
        let kind = provider(&name.span(), Some(name.get_ref()))?;
        let Some(fields) = entry.get_ref().as_table() else {
            return Err(RowError::at(
                name.span(),
                RowProblem::NotTable {
                    what: "a provider entry",
                },
            ));
        };
        let keys = in_order(fields);
        let mut row = ProviderRow::default();
        for (field, value) in &keys {
            let (at, value) = (field.span(), value.get_ref());
            let write =
                |field: &'static str| decimal(field, MULTIPLIER_WORDS, &at, value).map(Some);
            match &**field.get_ref() {
                "claude_code" => row.claude_code = Some(Identity::parse(&at, value)?),
                "codex" => {
                    let bad = || {
                        bad_value(
                            "codex",
                            "a table of version (three dot-separated numbers) and window (tokens)",
                            &at,
                        )
                    };
                    let [Some(version), Some(window)] =
                        table_of(["version", "window"], value, &bad)?
                    else {
                        return Err(bad());
                    };
                    let version = version.as_str().ok_or_else(bad)?;
                    if codex_version_parts(version).is_none() {
                        return Err(bad());
                    }
                    row.codex_version = Some(version.to_owned());
                    let window = token_count("window", &at, window)?;
                    check_limit("codex", window, DEFAULT_MAX_TOKENS)
                        .map_err(|e| RowError::at(at, RowProblem::LimitTooSmall(e)))?;
                    row.codex_window = Some(window);
                }
                "write_5m" => row.write_5m = write("write_5m")?,
                "write_1h" => row.write_1h = write("write_1h")?,
                "write_30m" => row.write_30m = write("write_30m")?,
                "write_12h" => row.write_12h = write("write_12h")?,
                "web_search_price" => {
                    row.web_search_price =
                        Some(decimal("web_search_price", USD_WORDS, &at, value)?);
                }
                "web_search_by_call" => {
                    row.web_search_by_call = boolean("web_search_by_call", &at, value)?
                }
                "weekly_limits" => row.weekly_limits = weekly_limits(&at, value)?,
                "message_threads" => {
                    let spec = ("message_threads", "aliases");
                    row.message_threads = listed(spec, MODEL_IDS_RULE, &at, value)?;
                }
                "account_bound_thinking" => {
                    let spec = ("account_bound_thinking", "aliases");
                    row.account_bound_thinking = listed(spec, MODEL_IDS_RULE, &at, value)?;
                }
                key @ ("default_model" | "web_search_model") => {
                    let (key, slot) = match key {
                        "default_model" => ("default_model", &mut row.default_model),
                        _ => ("web_search_model", &mut row.web_search_model),
                    };
                    let id = string(key, &at, value)?;
                    if !wisp_protocol::valid_model_id(&id) {
                        return Err(bad_value(key, MODEL_IDS_RULE, &at));
                    }
                    *slot = Some(id);
                }
                "antigravity" => {
                    row.antigravity = Some(AntigravityIdentity::from_table(&at, value)?)
                }
                key @ ("subscription_additional_tools"
                | "api_additional_tools"
                | "subscription_tools_after_outputs"
                | "api_tools_after_outputs") => {
                    let (key, slot) = match key {
                        "subscription_additional_tools" => (
                            "subscription_additional_tools",
                            &mut row.subscription_additional_tools,
                        ),
                        "api_additional_tools" => {
                            ("api_additional_tools", &mut row.api_additional_tools)
                        }
                        "subscription_tools_after_outputs" => (
                            "subscription_tools_after_outputs",
                            &mut row.subscription_tools_after_outputs,
                        ),
                        _ => ("api_tools_after_outputs", &mut row.api_tools_after_outputs),
                    };
                    *slot = listed((key, "aliases"), MODEL_IDS_RULE, &at, value)?;
                }
                "sources" => row.sources = sources(&at, value)?,
                _ => return Err(RowError::at(at, RowProblem::UnknownProviderField)),
            }
        }
        check_sources(
            ("providers", name),
            &keys,
            &PROVIDER_FIELDS,
            &row.sources,
            None,
            RowProblem::UnknownProviderField,
        )?;
        out.insert(kind, row);
    }
    Ok(out)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelTable {
    rows: Vec<ModelRow>,
    providers: BTreeMap<ProviderKind, ProviderRow>,
}

impl ModelTable {
    pub fn shipped() -> Result<Self, ShippedError> {
        Self::parse_shipped(SHIPPED)
    }

    pub fn parse_shipped(text: &str) -> Result<Self, ShippedError> {
        Self::walk(text).map_err(|e| ShippedError {
            line: e.line(text),
            problem: e.problem,
        })
    }

    fn walk(text: &str) -> Result<Self, RowError> {
        let root = DeTable::parse(text).map_err(|e| RowError {
            span: e.span(),
            problem: RowProblem::Syntax,
        })?;
        let entries = in_order(root.get_ref());
        let mut table = Self::default();
        if let Some((key, _)) = entries
            .iter()
            .find(|(key, _)| !TOP_FIELDS.contains(&&**key.get_ref()))
        {
            return Err(RowError::at(key.span(), RowProblem::UnknownTop));
        }
        let top = |name: &str| entries.iter().find(|(key, _)| &**key.get_ref() == name);
        if let Some((key, providers)) = top("providers") {
            table.providers = walk_providers(key.span(), providers.get_ref())?;
        }
        let Some((key, models)) = top("models") else {
            return Ok(table);
        };
        table.rows = walk_models(Site::Shipped, key.span(), models.get_ref())?;
        let levels: Vec<_> = table
            .rows
            .iter()
            .map(|row| {
                let own = row.effort_variant.as_ref()?;
                let variants = table
                    .rows
                    .iter()
                    .filter_map(|row| Some((row.id.as_str(), row.effort_variant.as_ref()?)));
                Some(effort_set(
                    &siblings(variants, own)
                        .into_iter()
                        .map(|(level, _)| level)
                        .collect::<Vec<_>>(),
                ))
            })
            .collect();
        for (row, levels) in table.rows.iter_mut().zip(levels) {
            if let Some(caps) = row.capabilities.as_mut()
                && let Some(levels) = levels
            {
                caps.efforts = levels;
            }
        }
        let ids = models
            .get_ref()
            .as_table()
            .map(in_order)
            .unwrap_or_default();
        for ((id, entry), row) in ids.iter().zip(&table.rows) {
            let keys = entry.get_ref().as_table().map(in_order).unwrap_or_default();
            let provider = row.provider.and_then(|kind| table.providers.get(&kind));
            check_sources(
                ("models", id),
                &keys,
                &SHIPPED_FIELDS,
                &row.sources,
                provider.map(|provider| &provider.sources),
                RowProblem::UnknownField {
                    site: Site::Shipped,
                },
            )?;
        }
        Ok(table)
    }

    pub fn rows(&self) -> &[ModelRow] {
        &self.rows
    }

    pub fn provider(&self, kind: ProviderKind) -> Option<&ProviderRow> {
        self.providers.get(&kind)
    }

    pub fn find(&self, model: &str) -> Option<&ModelRow> {
        self.rows.iter().find(|r| r.answers_to(model))
    }

    pub fn sibling(&self, model: &str, level: Effort) -> Option<&str> {
        let own = self.find(model)?.effort_variant.as_ref()?;
        let variants = self
            .rows
            .iter()
            .filter_map(|row| Some((row.id.as_str(), row.effort_variant.as_ref()?)));
        siblings(variants, own)
            .into_iter()
            .find(|(effort, _)| *effort == level)
            .map(|(_, id)| id)
    }

    pub fn own_effort(&self, model: &str, given: Effort) -> Effort {
        self.find(model)
            .and_then(|row| row.effort_variant.as_ref())
            .map_or(given, |variant| variant.effort)
    }

    pub fn capabilities(&self, kind: ProviderKind, model: &str) -> Option<&ModelCapabilities> {
        let row = self.find(model).filter(|row| row.provider == Some(kind));
        row.and_then(|row| row.capabilities.as_ref())
    }

    pub fn listed_rank(&self, model: &str) -> Option<u16> {
        let row = self.find(model).filter(|row| row.listed)?;
        let rank = self
            .rows
            .iter()
            .filter(|r| r.listed)
            .position(|r| r.id == row.id)?;
        u16::try_from(rank).ok()
    }

    pub fn canonical<'a>(&'a self, model: &'a str) -> &'a str {
        self.find(model).map_or(model, |row| row.id.as_str())
    }

    pub fn names_model(&self, id: &str, aliases: &[String], model: &str) -> bool {
        id == model
            || aliases.iter().any(|alias| alias == model)
            || self.canonical(id) == self.canonical(model)
    }

    pub fn shared() -> &'static Self {
        &SHARED
    }
}

static SHARED: std::sync::LazyLock<ModelTable> =
    std::sync::LazyLock::new(|| ModelTable::shipped().unwrap_or_default());

#[cfg(test)]
mod tests;
