use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::RequestId;
use crate::names::CONFIG_FILE;
use crate::settings::{self, TokensOrPercent};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextReport {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_plan_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "crate::Billing::is_metered")]
    pub billing: crate::Billing,
    #[serde(default, skip_serializing_if = "crate::Billing::is_metered")]
    pub spent_billing: crate::Billing,
    pub answers: RequestId,
    #[serde(default = "summary")]
    pub detail: ReportDetail,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub endpoint: String,
    pub fill: Fill,
    pub next: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<Window>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact_at: Option<CompactPoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reserve: Option<u64>,
    pub auto_compact: bool,
    #[serde(default)]
    pub categories: Vec<CategoryEstimate>,
    #[serde(default)]
    pub basis: ReportBasis,
    pub epoch: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub compactions: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reread_cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact_now_cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact_now_head_write_usd: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fill {
    Measured {
        used: u64,
        turn: u64,
        step: u64,
    },
    EstimatedAfterCompaction {
        tokens: u64,
        epoch: u64,
    },
    None,
    #[serde(untagged)]
    Other(Value),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Window {
    pub tokens: u64,
    pub source: WindowSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documented: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowSource {
    Session,
    Flag,
    Endpoint,
    Config,
    Shipped,
    Overflow,
    Subscription,
    Api,
    #[serde(untagged)]
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompactPoint {
    pub tokens: u64,
    pub set_by: PointSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capped_from: Option<u64>,
}

pub fn window_for(
    window: u64,
    ceiling: Option<u64>,
    max_tokens: u32,
    configured: Option<TokensOrPercent>,
) -> u64 {
    match (ceiling, configured) {
        (Some(ceiling), Some(TokensOrPercent::Tokens(point))) => {
            settings::smallest_window(point, max_tokens)
                .min(ceiling)
                .max(window)
        }
        _ => window,
    }
}

pub fn compact_at(
    window: u64,
    max_tokens: u32,
    configured: Option<(TokensOrPercent, PointSource)>,
) -> CompactPoint {
    let default = settings::default_point(window, max_tokens);
    match configured {
        None => CompactPoint {
            tokens: default,
            set_by: PointSource::Default,
            capped_from: None,
        },
        Some((value, set_by)) => {
            let tokens = value.tokens_on(window);
            CompactPoint {
                tokens: tokens.min(default),
                set_by,
                capped_from: (tokens > default).then_some(tokens),
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PointSource {
    Session,
    Flag,
    Endpoint,
    Config,
    Default,
    #[serde(untagged)]
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CategoryEstimate {
    pub category: Category,
    pub tokens: u64,
    #[serde(default)]
    pub source: FigureSource,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<ItemEstimate>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FigureSource {
    Counted,
    Measured,
    Split,
    #[default]
    Estimated,
    #[serde(untagged)]
    Other(String),
}

impl FigureSource {
    pub fn approximate(&self) -> bool {
        !matches!(self, FigureSource::Counted | FigureSource::Measured)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    SystemPrompt,
    Tools,
    Instructions,
    Environment,
    Summary,
    UserMessages,
    Replies,
    ToolResults,
    #[serde(alias = "images")]
    Attachments,
    #[serde(untagged)]
    Other(String),
}

impl Category {
    pub const ORDER: [Category; 9] = [
        Category::SystemPrompt,
        Category::Tools,
        Category::Instructions,
        Category::Environment,
        Category::Summary,
        Category::UserMessages,
        Category::Replies,
        Category::ToolResults,
        Category::Attachments,
    ];

    pub fn part(&self) -> FillPart {
        match self {
            Category::SystemPrompt | Category::Instructions => FillPart::System,
            Category::Tools => FillPart::Tools,
            Category::Environment
            | Category::Summary
            | Category::UserMessages
            | Category::Replies
            | Category::ToolResults
            | Category::Attachments
            | Category::Other(_) => FillPart::Chat,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FillPart {
    System,
    Tools,
    Chat,
    Skills,
    #[serde(other)]
    Unknown,
}

impl FillPart {
    pub const ORDER: [FillPart; 4] = [
        FillPart::System,
        FillPart::Tools,
        FillPart::Chat,
        FillPart::Skills,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartTokens {
    pub part: FillPart,
    pub tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemEstimate {
    pub name: String,
    pub tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unestimated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportBasis {
    Counted,
    Counting,
    PartlyEstimated {
        reason: String,
    },
    Estimated {
        reason: EstimateReason,
    },
    #[serde(untagged)]
    Other(Value),
}

impl Default for ReportBasis {
    fn default() -> Self {
        Self::Estimated {
            reason: EstimateReason::NoCounter,
        }
    }
}

impl ReportBasis {
    pub fn settled(&self) -> bool {
        !matches!(self, Self::Counting)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EstimateReason {
    NoCounter,
    Off,
    Failed(String),
    NothingYet,
    #[serde(untagged)]
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportDetail {
    All,
    Summary,
    #[serde(untagged)]
    Other(String),
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

fn summary() -> ReportDetail {
    ReportDetail::Summary
}

pub fn format_usd(usd: f64) -> String {
    let usd = if usd.is_finite() { usd.max(0.0) } else { 0.0 };
    if usd == 0.0 {
        return "$0.00".to_owned();
    }
    let magnitude = usd.log10().floor() as i32;
    let decimals = (2 - magnitude).clamp(2, 9) as usize;
    let text = format!("{usd:.decimals$}");
    let (whole, frac) = text.split_once('.').unwrap_or((&text, ""));
    let mut frac = frac.to_owned();
    while frac.len() > 2 && frac.ends_with('0') {
        frac.pop();
    }
    format!("${whole}.{frac}")
}

pub const SUBSCRIPTION_WORD: &str = "subscription";
pub const SUBSCRIPTIONS_WORD: &str = "subscriptions";

const ESTIMATE_MARK: &str = "~";
const LOWER_BOUND_MARK: &str = "+";

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Marked {
    pub lead: &'static str,
    pub amount: String,
    pub tail: &'static str,
}

impl std::fmt::Display for Marked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}{}{}", self.lead, self.amount, self.tail)
    }
}

pub fn marked_amount(amount: &str, estimate: bool, lower: bool) -> Marked {
    Marked {
        lead: if estimate { ESTIMATE_MARK } else { "" },
        amount: amount.to_owned(),
        tail: if lower { LOWER_BOUND_MARK } else { "" },
    }
}

pub fn format_tokens(tokens: u64) -> String {
    match tokens {
        0..1_000 => tokens.to_string(),
        1_000..999_500 => format!("{}k", (tokens + 500) / 1_000),
        _ => {
            let tenths = (u128::from(tokens.max(1_000_000)) + 50_000) / 100_000;
            match (tenths / 10, tenths % 10) {
                (whole, 0) => format!("{whole}M"),
                (whole, tenth) => format!("{whole}.{tenth}M"),
            }
        }
    }
}

pub fn limit_of(point: Option<u64>, ceiling: Option<u64>, auto_compact: bool) -> Option<u64> {
    if auto_compact {
        point.or(ceiling)
    } else {
        ceiling
    }
}

pub fn fill_percent(part: u64, whole: u64) -> u64 {
    if whole == 0 {
        return 0;
    }
    u64::try_from(u128::from(part) * 100 / u128::from(whole)).unwrap_or(u64::MAX)
}

pub fn cache_percent(read: u64, prompt: u64) -> Option<u8> {
    (prompt > 0).then(|| u8::try_from(fill_percent(read, prompt).min(100)).unwrap_or(100))
}

pub fn unknown_window(model: &str) -> String {
    let shown = crate::shown_model(model);
    if shown != model {
        return format!("Window unknown for the model {shown}, so automatic compaction is off.");
    }
    format!(
        "Window unknown for {model}: set context_window under [models.\"{model}\"] in {CONFIG_FILE} to enable automatic compaction."
    )
}

#[cfg(test)]
mod tests;
