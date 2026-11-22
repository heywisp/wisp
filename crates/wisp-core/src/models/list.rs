use std::cmp::Reverse;
use std::collections::BTreeSet;
use std::future::Future;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::time::{Instant, timeout_at};
use wisp_protocol::settings::AccountModel;
use wisp_protocol::{Effort, ErrorDetail, ErrorKind, FailurePhase, FailureSource, RetryClass};
use wisp_protocol::{check_model_id, valid_model_id};

use crate::agent::{COMPACTION_MAX_TOKENS, DEFAULT_MAX_TOKENS};
use crate::context::check_limit;
use crate::endpoints::ProviderKind;
use crate::models::{ModelRow, ModelTable};
use crate::plan_usage::record::{self, Claim};
use crate::provider::credential::Credential;
use crate::provider::{ModelCapabilities, ProviderError};
use crate::search::listed_version;

pub const REFRESH_AFTER: i64 = 60 * 60;
pub const ROUND_EVERY: Duration = Duration::from_secs(REFRESH_AFTER.unsigned_abs() / 4);
pub const TIMEOUT: Duration = Duration::from_secs(30);
pub const LIST_WAIT: Duration = Duration::from_secs(5);

tokio::task_local! {
    static LIST_WAIT_BUDGET: ListWait;
}

#[derive(Clone, Default)]
pub struct ListWait(Arc<OnceLock<Duration>>);

impl ListWait {
    pub async fn fetch<T>(&self, future: impl Future<Output = T>) -> T {
        LIST_WAIT_BUDGET.scope(self.clone(), future).await
    }

    pub async fn until(&self, deadline: Instant, future: impl Future) {
        tokio::pin!(future);
        if timeout_at(deadline, &mut future).await.is_err()
            && let Some(extra) = self.0.get()
        {
            let _ = timeout_at(deadline + *extra, future).await;
        }
    }

    pub(crate) fn reserve_for_release(limit: Duration) {
        let _ = LIST_WAIT_BUDGET.try_with(|wait| wait.0.set(limit));
    }
}

pub mod anthropic;

#[derive(Deserialize)]
struct Wire {
    models: Vec<WireEntry>,
}

#[derive(Deserialize)]
struct WireEntry {
    slug: String,
    visibility: String,
    #[serde(default)]
    priority: i64,
    context_window: Option<u64>,
    max_context_window: Option<u64>,
    supported_reasoning_levels: Vec<WireLevel>,
    default_reasoning_level: Option<String>,
    input_modalities: Vec<String>,
    supports_search_tool: bool,
}

#[derive(Deserialize)]
struct WireLevel {
    effort: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelList(Vec<ListedModel>);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListedModel {
    pub id: String,
    pub shown: bool,
    pub window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_window: Option<u64>,
    levels: Vec<Effort>,
    default_level: Option<Effort>,
    #[serde(default)]
    max_output: Option<u32>,
    #[serde(default)]
    adaptive: Option<bool>,
    pub images: bool,
    pub search: bool,
}

#[derive(Clone, Default)]
pub struct ListCell(Arc<Mutex<Held>>);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListMeta {
    pub default_model: Option<String>,
    pub web_search_models: Vec<String>,
}

#[derive(Default)]
struct Held {
    list: Option<Arc<ModelList>>,
    meta: ListMeta,
    sent: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedList {
    pub attempted: i64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub failed: bool,
    pub list: ModelList,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub web_search_models: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unusable {
    Unreachable,
    Unreadable,
    Invalid,
    Empty,
}

impl Unusable {
    pub fn text(self, kind: ProviderKind) -> String {
        let title = kind.title();
        match self {
            Self::Unreachable => format!("{title} model discovery failed"),
            Self::Unreadable => format!("{title} model discovery was unreadable or too large"),
            Self::Invalid => format!("{title} returned an invalid model list"),
            Self::Empty => format!("{title} model list has no entries"),
        }
    }

    pub fn error(self, kind: ProviderKind) -> ProviderError {
        match self {
            Self::Unreachable | Self::Unreadable => network(self.text(kind)),
            Self::Invalid | Self::Empty => ProviderError::malformed(self.text(kind)),
        }
    }
}

fn network(message: String) -> ProviderError {
    ProviderError::classified(
        ErrorKind::Network,
        message,
        ErrorDetail::new(
            FailurePhase::Stream,
            FailureSource::Local,
            RetryClass::Transient,
        ),
    )
    .possibly_billed()
}

pub fn key(url: &str, credential: &Credential, account: &str) -> Option<String> {
    let (address, sign_in, name) = match credential {
        Credential::Subscription {
            profile: Some(path),
            ..
        } => (
            url.to_owned(),
            "subscription",
            path.as_os_str().as_encoded_bytes(),
        ),
        Credential::ApiKey(_) | Credential::AuthToken(_) => {
            (crate::endpoints::origin(url), "account", account.as_bytes())
        }
        _ => return None,
    };
    let mut hash = Sha256::new();
    hash.update(address.as_bytes());
    hash.update([0]);
    hash.update(sign_in.as_bytes());
    hash.update([0]);
    hash.update(name);
    wisp_plugin_api::hex::encode(&hash.finalize())
        .get(..32)
        .map(str::to_owned)
}

pub fn ids_only(bytes: &[u8]) -> Result<Vec<String>, Unusable> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| Unusable::Invalid)?;
    let models = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or(Unusable::Empty)?;
    Ok(models
        .iter()
        .filter_map(|model| model.get("id").and_then(Value::as_str))
        .filter(|id| check_model_id(id).is_ok())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect())
}

impl ModelList {
    pub fn parse(bytes: &[u8]) -> Result<Self, Unusable> {
        let mut entries = serde_json::from_slice::<Wire>(bytes)
            .map_err(|_| Unusable::Invalid)?
            .models;
        entries.retain(|entry| valid_model_id(&entry.slug));
        entries.sort_by_key(|entry| entry.priority);
        Self::new(entries.into_iter().map(ListedModel::from).collect())
    }

    fn new(models: Vec<ListedModel>) -> Result<Self, Unusable> {
        if models.iter().any(|model| model.shown) {
            Ok(Self(models))
        } else {
            Err(Unusable::Empty)
        }
    }

    pub(crate) fn from_gemini_ids(
        ids: impl IntoIterator<Item = (String, Option<u64>, Option<u32>)>,
        default_model: Option<&str>,
    ) -> Result<Self, Unusable> {
        let mut seen = BTreeSet::new();
        let mut models = Vec::new();
        for (id, input_limit, output_limit) in ids {
            if !valid_model_id(&id) || !seen.insert(id.clone()) {
                continue;
            }
            models.push(ListedModel {
                shown: true,
                window: window(&id, input_limit),
                default_window: None,
                max_window: None,
                levels: Vec::new(),
                default_level: None,
                max_output: max_output(&id, output_limit),
                adaptive: None,
                images: false,
                search: false,
                id,
            });
        }
        sort_gemini(&mut models, default_model);
        Self::new(models)
    }

    pub(crate) fn sorted_gemini(&self, default_model: Option<&str>) -> Self {
        let mut models = self.0.clone();
        sort_gemini(&mut models, default_model);
        Self(models)
    }

    pub fn find(&self, id: &str) -> Option<&ListedModel> {
        let table = ModelTable::shared();
        let id = table.canonical(id);
        self.0.iter().find(|model| table.canonical(&model.id) == id)
    }

    pub fn capabilities(
        &self,
        kind: ProviderKind,
        id: &str,
        shipped: Option<ModelCapabilities>,
        base: ModelCapabilities,
    ) -> Option<ModelCapabilities> {
        Some(self.find(id)?.capabilities(kind, shipped, base))
    }

    pub fn with_new(&self, fresh: &Self, sent: &BTreeSet<String>) -> Self {
        let table = ModelTable::shared();
        let named = |id: &str| {
            sent.iter()
                .any(|name| table.canonical(name) == table.canonical(id))
        };
        let new = fresh
            .0
            .iter()
            .filter(|model| self.find(&model.id).is_none() && !named(&model.id));
        Self(self.0.iter().chain(new).cloned().collect())
    }

    pub fn ids(&self) -> Vec<String> {
        self.0.iter().map(|model| model.id.clone()).collect()
    }

    pub fn offers(&self) -> Vec<AccountModel> {
        let mut ranks = 0..;
        self.0
            .iter()
            .map(|model| AccountModel {
                id: model.id.clone(),
                listed: model.shown.then(|| ranks.next()).flatten(),
            })
            .collect()
    }
}

impl From<WireEntry> for ListedModel {
    fn from(entry: WireEntry) -> Self {
        let known = |level: &str| level.parse::<Effort>().ok();
        let window = window(
            &entry.slug,
            entry.context_window.or(entry.max_context_window),
        );
        Self {
            id: entry.slug,
            shown: entry.visibility == "list",
            window,
            default_window: entry.context_window,
            max_window: entry.max_context_window,
            levels: entry
                .supported_reasoning_levels
                .iter()
                .filter_map(|level| known(&level.effort))
                .collect(),
            default_level: entry.default_reasoning_level.as_deref().and_then(known),
            max_output: None,
            adaptive: None,
            images: entry.input_modalities.iter().any(|kind| kind == "image"),
            search: entry.supports_search_tool,
        }
    }
}

fn gemini_version(id: &str) -> Option<(u32, u32)> {
    let rest = id.strip_prefix("gemini-")?;
    let segment = rest.split('-').next()?;
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    match segment.split_once('.') {
        Some((major, minor)) if digits(major) && digits(minor) => {
            Some((major.parse().ok()?, minor.parse().ok()?))
        }
        None if digits(segment) => Some((segment.parse().ok()?, 0)),
        _ => None,
    }
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct GeminiSortKey<'a> {
    tier: u8,
    rank: u16,
    effort: Option<Effort>,
    version: Reverse<Option<(u32, u32)>>,
    id: &'a str,
}

fn gemini_sort_key<'a>(
    table: &ModelTable,
    id: &'a str,
    default_model: Option<&str>,
) -> GeminiSortKey<'a> {
    let row = table.find(id);
    let variant = row.and_then(|r| r.effort_variant.as_ref());
    let family = variant.map_or(id, |v| v.family.as_str());
    let has_thinking = row
        .and_then(|r| r.capabilities)
        .is_some_and(|c| c.adaptive_thinking || c.thinking_budgets.is_some());
    let (tier, rank, effort, version) = if let Some(rank) = table.listed_rank(family) {
        (0, rank, variant.map(|v| v.effort), None)
    } else if default_model.is_some_and(|d| table.canonical(d) == table.canonical(id)) {
        (1, 0, variant.map(|v| v.effort), None)
    } else if row.is_some() && has_thinking {
        (2, 0, None, gemini_version(id))
    } else if row.is_some() {
        (3, 0, None, gemini_version(id))
    } else {
        (4, 0, None, gemini_version(id))
    };
    GeminiSortKey {
        tier,
        rank,
        effort,
        version: Reverse(version),
        id,
    }
}

fn sort_gemini(models: &mut [ListedModel], default_model: Option<&str>) {
    let table = ModelTable::shared();
    models.sort_by(|a, b| {
        gemini_sort_key(table, &a.id, default_model).cmp(&gemini_sort_key(
            table,
            &b.id,
            default_model,
        ))
    });
}

fn window(id: &str, tokens: Option<u64>) -> Option<u64> {
    tokens.filter(|tokens| check_limit(id, *tokens, DEFAULT_MAX_TOKENS).is_ok())
}

fn max_output(id: &str, tokens: Option<u32>) -> Option<u32> {
    let shipped = ModelTable::shared().find(id).and_then(|row| row.max_output);
    tokens.filter(|tokens| *tokens >= COMPACTION_MAX_TOKENS || (shipped.is_none() && *tokens > 0))
}

fn limits_only(kind: ProviderKind) -> bool {
    kind == ProviderKind::Gemini
}

impl ListedModel {
    pub(crate) fn maximum(&self) -> Option<u64> {
        window(&self.id, self.max_window)
    }

    pub(crate) fn listed_default(&self) -> Option<u64> {
        let default = window(&self.id, self.default_window)?;
        Some(self.maximum().map_or(default, |max| default.min(max)))
    }

    pub(crate) fn ceiling(&self) -> Option<u64> {
        match (self.default_window, self.max_window) {
            (None, None) => window(&self.id, self.window),
            _ => self.maximum(),
        }
    }

    pub fn levels(&self) -> &'static [Effort] {
        crate::models::effort_set(&self.levels)
    }

    pub fn default_level(&self) -> Option<Effort> {
        self.default_level
            .filter(|level| self.levels().contains(level))
    }

    pub fn capabilities(
        &self,
        kind: ProviderKind,
        shipped: Option<ModelCapabilities>,
        base: ModelCapabilities,
    ) -> ModelCapabilities {
        let mut row = shipped.unwrap_or(base);
        row.max_output = self.max_output.or(row.max_output);
        if limits_only(kind) {
            return row;
        }
        let kept = row
            .default_effort
            .filter(|level| self.levels().contains(level));
        ModelCapabilities {
            efforts: self.levels(),
            default_effort: self.default_level().or(kept),
            adaptive_thinking: self.adaptive.unwrap_or(row.adaptive_thinking),
            images: row.images.filter(|_| self.images),
            ..row
        }
    }

    pub fn row(&self, kind: ProviderKind) -> ModelRow {
        let search = self.search.then(|| listed_version(kind).to_owned());
        ModelRow {
            context_window: self.window,
            web_search: (!limits_only(kind)).then(|| search.into_iter().collect()),
            ..ModelRow::new(self.id.clone())
        }
    }
}

impl ListCell {
    pub fn new(list: Option<Arc<ModelList>>) -> Self {
        Self::new_with_meta(list, ListMeta::default())
    }

    pub fn new_with_meta(list: Option<Arc<ModelList>>, meta: ListMeta) -> Self {
        Self(Arc::new(Mutex::new(Held {
            list,
            meta,
            sent: BTreeSet::new(),
        })))
    }

    fn held(&self) -> MutexGuard<'_, Held> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn get(&self) -> Option<Arc<ModelList>> {
        self.held().list.clone()
    }

    pub fn default_model(&self) -> Option<String> {
        self.held().meta.default_model.clone()
    }

    pub fn web_search_models(&self) -> Vec<String> {
        self.held().meta.web_search_models.clone()
    }

    pub fn sent(&self, model: &str) {
        let mut held = self.held();
        if !held.sent.contains(model) {
            held.sent.insert(model.to_owned());
        }
    }

    pub fn add(&self, fetched: &ModelList) {
        self.add_with_meta(fetched, ListMeta::default());
    }

    pub fn add_with_meta(&self, fetched: &ModelList, meta: ListMeta) {
        let mut held = self.held();
        if let Some(model) = meta.default_model {
            held.meta.default_model = Some(model);
        }
        held.meta.web_search_models = meta.web_search_models;
        let Some(list) = held.list.as_deref() else {
            return;
        };
        let grown = list.with_new(fetched, &held.sent);
        if grown != *list {
            held.list = Some(Arc::new(grown));
        }
    }

    pub(crate) fn begin(
        &self,
        folder: &Path,
        key: Option<&str>,
        always: bool,
        now: i64,
    ) -> Option<Claim> {
        let key = key?;
        let current = || SavedList::load(folder, key).filter(|saved| !saved.stale(now));
        let served = || {
            current()
                .map(|saved| {
                    let meta = saved.meta();
                    self.add_with_meta(&saved.list, meta);
                })
                .is_some()
        };
        let mut claim = Claim::Unshared;
        if !always {
            if served() {
                return None;
            }
            claim = record::claim(folder, key);
            if matches!(claim, Claim::Busy) || served() {
                return None;
            }
        }
        SavedList::record(folder, Some(key), None, now);
        Some(claim)
    }
}

impl SavedList {
    pub fn meta(&self) -> ListMeta {
        ListMeta {
            default_model: self.default_model.clone(),
            web_search_models: self.web_search_models.clone(),
        }
    }

    pub fn load(folder: &Path, key: &str) -> Option<Self> {
        let mut saved: Self = record::read(folder, key)?;
        saved.list.0.retain(|model| valid_model_id(&model.id));
        saved.default_model = saved.default_model.filter(|id| valid_model_id(id));
        saved.web_search_models.retain(|id| valid_model_id(id));
        Some(saved)
    }

    pub fn take(folder: Option<&Path>, key: Option<&str>) -> Option<Arc<ModelList>> {
        Self::take_with_meta(folder, key).0
    }

    pub fn take_with_meta(
        folder: Option<&Path>,
        key: Option<&str>,
    ) -> (Option<Arc<ModelList>>, ListMeta) {
        let Some(saved) = folder.zip(key).and_then(|(f, key)| Self::load(f, key)) else {
            return (None, ListMeta::default());
        };
        let meta = saved.meta();
        (saved.into_list().map(Arc::new), meta)
    }

    pub fn record(folder: &Path, key: Option<&str>, fetched: Option<ModelList>, now: i64) {
        Self::record_with_meta(folder, key, fetched, ListMeta::default(), now);
    }

    pub fn record_with_meta(
        folder: &Path,
        key: Option<&str>,
        fetched: Option<ModelList>,
        meta: ListMeta,
        now: i64,
    ) {
        let Some(key) = key else {
            return;
        };
        let previous = Self::load(folder, key);
        let meta = match &fetched {
            Some(_) => meta,
            None => previous.as_ref().map(Self::meta).unwrap_or_default(),
        };
        let failed = fetched.is_none();
        let list = fetched
            .or_else(|| previous.map(|saved| saved.list))
            .unwrap_or_default();
        let _ = Self {
            attempted: now,
            failed,
            list,
            default_model: meta.default_model,
            web_search_models: meta.web_search_models,
        }
        .save(folder, key);
    }

    pub fn save(&self, folder: &Path, key: &str) -> std::io::Result<()> {
        crate::secrets::create_folder(folder)?;
        record::write(folder, key, self)
    }

    pub fn stale(&self, now: i64) -> bool {
        let after = if self.failed {
            (ROUND_EVERY - TIMEOUT).as_secs().cast_signed()
        } else {
            REFRESH_AFTER
        };
        now.saturating_sub(self.attempted) >= after
    }

    pub fn into_list(self) -> Option<ModelList> {
        (!self.list.0.is_empty()).then_some(self.list)
    }
}

#[cfg(test)]
pub(crate) mod tests;
