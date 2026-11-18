mod count;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::task::AbortHandle;
use wisp_protocol::context::{
    Category, CategoryEstimate, CompactPoint, ContextReport, EstimateReason, FigureSource, Fill,
    FillPart, ItemEstimate, PartTokens, ReportBasis, ReportDetail, cache_percent, limit_of,
};
use wisp_protocol::settings::{self, CompactionCost};
use wisp_protocol::{AgentEvent, ContentBlock, Message, RequestId, Role, SessionConfig, Usage};
use wisp_tool_text::ENVIRONMENT_TAG;

use super::{Agent, LastRequest, emit};
use crate::context::{
    self, CompactNow, Resolved, block_tokens, blocks_tokens, compact_now_input, compaction_cost,
    image_tokens, next_estimate, reread_micros, text_tokens, usd,
};
use crate::provider::{Features, Iteration, Request, TokenCount, UNRENDERED};
use crate::tool::MediaCount;
use count::{CountKey, Fixed, Layer, Mode, ReportInputs, Results, assemble};

pub const TOOL_RESULT_ITEMS: usize = 5;
const COUNT_PLACEHOLDER: &str = ".";
const LABEL_MAX: usize = 80;

type Counts = Arc<Mutex<BTreeMap<CountKey, u64>>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct MeterFigures {
    pub(super) window: Option<u64>,
    pub(super) point: Option<u64>,
    pub(super) ceiling: Option<u64>,
    pub(super) auto_compact: bool,
    pub(super) limit: Option<u64>,
}

#[derive(Debug, Default)]
pub(super) struct FillState {
    turns: Vec<u64>,
    replies: BTreeMap<usize, u64>,
    uncountable: Arc<Mutex<BTreeSet<String>>>,
    counts: Counts,
    pub(super) count_off: bool,
    counting: Vec<AbortHandle>,
}

impl FillState {
    pub(super) fn resumed(&mut self, turns: Vec<u64>, replies: BTreeMap<usize, u64>) {
        self.turns = turns;
        self.replies = replies;
    }
}

pub(super) fn used_of(usage: &Usage, iterations: &[Iteration]) -> u64 {
    iterations
        .last()
        .map_or_else(|| usage.prompt_tokens(), |i| i.usage.prompt_tokens())
}

fn call_label(name: &str, input: &Value) -> String {
    let first = match input {
        Value::Object(map) => map.values().find_map(Value::as_str),
        _ => None,
    };
    let line = first
        .and_then(|text| text.lines().map(str::trim).find(|l| !l.is_empty()))
        .unwrap_or(name);
    line.chars().take(LABEL_MAX).collect()
}

#[derive(Default)]
struct Tally {
    environment: u64,
    summary: u64,
    user: u64,
    replies: u64,
    replies_estimated: bool,
    results: Vec<ItemEstimate>,
    attachments: Vec<ItemEstimate>,
    images: usize,
}

impl Tally {
    fn image(&mut self, tokens: u64, turn: Option<u64>) {
        self.images += 1;
        let name = format!("image {}", self.images);
        self.attachments.push(ItemEstimate {
            name,
            tokens,
            turn,
            unestimated: false,
        });
    }

    fn file(&mut self, name: String, turn: Option<u64>) {
        self.attachments.push(ItemEstimate {
            name,
            tokens: 0,
            turn,
            unestimated: true,
        });
    }
}

fn row(category: Category, tokens: u64, items: Vec<ItemEstimate>) -> Option<CategoryEstimate> {
    (tokens > 0 || !items.is_empty()).then_some(CategoryEstimate {
        category,
        tokens,
        source: FigureSource::Estimated,
        items,
    })
}

fn sum(items: &[ItemEstimate]) -> u64 {
    items.iter().fold(0, |s, i| s.saturating_add(i.tokens))
}

fn is_fixed(category: &Category) -> bool {
    matches!(
        category.part(),
        FillPart::System | FillPart::Tools | FillPart::Skills
    )
}

fn ratio(value: u64, numerator: u64, denominator: u64) -> u64 {
    if denominator == 0 {
        return 0;
    }
    let scaled = (u128::from(value) * u128::from(numerator) + u128::from(denominator) / 2)
        / u128::from(denominator);
    u64::try_from(scaled).unwrap_or(u64::MAX)
}

pub(super) fn apportion(target: u64, weights: &[u64]) -> Vec<u64> {
    let whole: u128 = weights.iter().map(|w| u128::from(*w)).sum();
    if whole == 0 {
        return vec![0; weights.len()];
    }
    let target = u128::from(target);
    let mut shares: Vec<u128> = weights
        .iter()
        .map(|w| target * u128::from(*w) / whole)
        .collect();
    let given: u128 = shares.iter().sum();
    let mut order: Vec<usize> = (0..weights.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(target * u128::from(weights[i]) % whole));
    let short = usize::try_from(target - given).unwrap_or(usize::MAX);
    for &i in order.iter().take(short) {
        shares[i] += 1;
    }
    shares
        .into_iter()
        .map(|s| u64::try_from(s).unwrap_or(u64::MAX))
        .collect()
}

fn scale(rows: &mut Vec<CategoryEstimate>, pick: impl Fn(&CategoryEstimate) -> bool, target: u64) {
    let picked: Vec<usize> = (0..rows.len()).filter(|&i| pick(&rows[i])).collect();
    let weights: Vec<u64> = picked.iter().map(|&i| rows[i].tokens).collect();
    if weights.iter().all(|w| *w == 0) {
        if target > 0 {
            home(rows).tokens = target;
        }
        return;
    }
    for (&i, share) in picked.iter().zip(apportion(target, &weights)) {
        let row = &mut rows[i];
        let before = row.tokens;
        for item in &mut row.items {
            item.tokens = ratio(item.tokens, share, before);
        }
        row.tokens = share;
        row.source = FigureSource::Split;
    }
}

fn slot(rows: &mut Vec<CategoryEstimate>, category: Category) -> &mut CategoryEstimate {
    let rank = |c: &Category| Category::ORDER.iter().position(|o| o == c);
    let at = match rows.iter().position(|r| r.category == category) {
        Some(at) => at,
        None => {
            let mine = rank(&category);
            let at = rows
                .iter()
                .position(|r| rank(&r.category).is_none_or(|r| Some(r) > mine))
                .unwrap_or(rows.len());
            rows.insert(
                at,
                CategoryEstimate {
                    category,
                    tokens: 0,
                    source: FigureSource::Estimated,
                    items: vec![],
                },
            );
            at
        }
    };
    &mut rows[at]
}

fn home(rows: &mut Vec<CategoryEstimate>) -> &mut CategoryEstimate {
    let row = slot(rows, Category::UserMessages);
    row.source = FigureSource::Split;
    row
}

fn sum_rows(rows: &[CategoryEstimate]) -> u64 {
    rows.iter().fold(0, |s, r| s.saturating_add(r.tokens))
}

pub(super) fn split(mut rows: Vec<CategoryEstimate>, next: u64) -> Vec<CategoryEstimate> {
    let fixed = rows
        .iter()
        .filter(|r| is_fixed(&r.category))
        .fold(0u64, |s, r| s.saturating_add(r.tokens));
    let kept = rows
        .iter()
        .filter(|r| !is_fixed(&r.category) && r.source == FigureSource::Measured)
        .fold(0u64, |s, r| s.saturating_add(r.tokens));
    let pooled = rows
        .iter()
        .any(|r| !is_fixed(&r.category) && r.source != FigureSource::Measured && r.tokens > 0);
    if next < fixed {
        scale(&mut rows, |_| true, next);
    } else if pooled && next - fixed >= kept {
        scale(
            &mut rows,
            |r| !is_fixed(&r.category) && r.source != FigureSource::Measured,
            next - fixed - kept,
        );
    } else {
        scale(&mut rows, |r| !is_fixed(&r.category), next - fixed);
    }
    rows
}

impl Agent {
    pub(super) fn sync_fill(&mut self) {
        let len = self.history.len();
        let turn = self.running_turn().unwrap_or(self.turns);
        self.fill.turns.resize(len, turn);
        self.fill.replies.retain(|index, _| *index < len);
    }

    pub(super) fn note_reply(&mut self, index: usize, output_tokens: u64) {
        self.fill.replies.insert(index, output_tokens);
    }

    pub(super) fn resolved(&self) -> Resolved {
        let (name, model) = (self.endpoint_name(), &self.session.model);
        let listed = self.listed_row(name, model);
        self.models
            .resolve(name, model, listed.as_ref(), self.session.max_tokens)
    }

    fn point(&self, resolved: &Resolved) -> Option<CompactPoint> {
        let window = resolved.window.as_ref()?;
        Some(context::compact_at(
            window.tokens,
            self.session.max_tokens,
            resolved.compact_at.clone(),
        ))
    }

    pub(super) fn window_and_point(&self) -> (Option<u64>, Option<u64>) {
        let resolved = self.resolved();
        let point = self.point(&resolved).map(|p| p.tokens);
        (resolved.window.map(|w| w.tokens), point)
    }

    fn labels(&self) -> BTreeMap<String, String> {
        self.history
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(|block| match block {
                ContentBlock::ToolUse {
                    id, name, input, ..
                } => Some((id.clone(), call_label(name, input))),
                _ => None,
            })
            .collect()
    }

    fn tally(&self, upto: usize, pending_environment: bool) -> Tally {
        let labels = self.labels();
        let mut tally = Tally::default();
        for block in self.head_blocks() {
            match block {
                ContentBlock::Text { text } if text.starts_with(ENVIRONMENT_TAG) => {
                    tally.environment = tally.environment.saturating_add(text_tokens(text));
                }
                other => tally.summary = tally.summary.saturating_add(block_tokens(other)),
            }
        }
        let from = self.view_from();
        for (index, message) in self.history.iter().enumerate().take(upto).skip(from) {
            let turn = self.fill.turns.get(index).copied();
            match message.role {
                Role::System => {}
                Role::Assistant => {
                    let measured = self.fill.replies.get(&index).copied();
                    tally.replies_estimated |= measured.is_none();
                    let tokens = measured.unwrap_or_else(|| blocks_tokens(&message.content));
                    tally.replies = tally.replies.saturating_add(tokens);
                }
                Role::User => self.tally_user(message, turn, &labels, &mut tally),
            }
        }
        if pending_environment {
            for block in self.turn_section_texts() {
                tally.environment = tally.environment.saturating_add(text_tokens(&block));
            }
        }
        tally
    }

    fn tally_user(
        &self,
        message: &Message,
        turn: Option<u64>,
        labels: &BTreeMap<String, String>,
        tally: &mut Tally,
    ) {
        for block in &message.content {
            match block {
                ContentBlock::Text { text } if text.starts_with(ENVIRONMENT_TAG) => {
                    tally.environment = tally.environment.saturating_add(text_tokens(text));
                }
                ContentBlock::Image { source } => tally.image(image_tokens(source), turn),
                ContentBlock::Media { .. } => {
                    let name = format!("file {}", tally.attachments.len() + 1);
                    tally.file(name, turn);
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => {
                    let mut tokens = 0u64;
                    let name = labels
                        .get(tool_use_id)
                        .cloned()
                        .unwrap_or_else(|| tool_use_id.clone());
                    for inner in content {
                        match inner {
                            ContentBlock::Image { source } => {
                                tally.image(image_tokens(source), turn);
                            }
                            ContentBlock::Media { .. } => {
                                let target = self.call_target(tool_use_id);
                                tally.file(target.unwrap_or_else(|| name.clone()), turn);
                            }
                            other => tokens = tokens.saturating_add(block_tokens(other)),
                        }
                    }
                    tally.results.push(ItemEstimate {
                        name,
                        tokens,
                        turn,
                        unestimated: false,
                    });
                }
                other => tally.user = tally.user.saturating_add(block_tokens(other)),
            }
        }
    }

    pub(super) fn parts(&self, total: u64) -> Vec<PartTokens> {
        let rows = split(self.fixed_rows(), total);
        FillPart::ORDER
            .into_iter()
            .map(|part| PartTokens {
                part,
                tokens: rows
                    .iter()
                    .filter(|r| r.category.part() == part)
                    .fold(0, |s, r| s.saturating_add(r.tokens)),
            })
            .filter(|p| p.tokens > 0)
            .collect()
    }

    pub(super) fn cached(&self) -> Option<u8> {
        let last = self.last.as_ref()?;
        cache_percent(last.cache_read?, last.prompt_tokens)
    }

    fn fixed_rows(&self) -> Vec<CategoryEstimate> {
        let system = self.session.system.as_deref().map_or(0, text_tokens);
        let tools: Vec<ItemEstimate> = self
            .counted_tools()
            .iter()
            .map(|spec| ItemEstimate {
                name: spec.name.clone(),
                tokens: serde_json::to_string(spec).map_or(0, |text| text_tokens(&text)),
                turn: None,
                unestimated: false,
            })
            .collect();
        let instructions = self.session.instructions.as_deref().map_or(0, text_tokens);
        let files: Vec<ItemEstimate> = self
            .instructions
            .as_ref()
            .filter(|_| instructions > 0)
            .map(|state| {
                state
                    .summary
                    .chosen
                    .iter()
                    .chain(&state.summary.imports)
                    .map(|source| ItemEstimate {
                        name: source.display_path.clone(),
                        tokens: source.bytes.div_ceil(context::BYTES_PER_TOKEN),
                        turn: None,
                        unestimated: false,
                    })
                    .collect()
            })
            .unwrap_or_default();
        [
            row(Category::SystemPrompt, system, vec![]),
            row(Category::Tools, sum(&tools), tools),
            row(Category::Instructions, instructions, files),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    pub(super) fn categories(
        &self,
        upto: usize,
        pending_environment: bool,
    ) -> Vec<CategoryEstimate> {
        let mut rows = self.fixed_rows();
        let mut tally = self.tally(upto, pending_environment);
        let attachments = sum(&tally.attachments);
        let results = sum(&tally.results);
        tally
            .results
            .sort_by_key(|item| std::cmp::Reverse(item.tokens));
        tally.results.truncate(TOOL_RESULT_ITEMS);
        let replies = row(Category::Replies, tally.replies, vec![]).map(|mut r| {
            if !tally.replies_estimated {
                r.source = FigureSource::Measured;
            }
            r
        });
        rows.extend(
            [
                row(Category::Environment, tally.environment, vec![]),
                row(Category::Summary, tally.summary, vec![]),
                row(Category::UserMessages, tally.user, vec![]),
                replies,
                row(Category::ToolResults, results, tally.results),
                row(Category::Attachments, attachments, tally.attachments),
            ]
            .into_iter()
            .flatten(),
        );
        rows
    }

    fn upto(&self) -> usize {
        self.last.as_ref().map_or(self.history.len(), |l| {
            self.history_len(l.messages_len).min(self.history.len())
        })
    }

    fn since_reply(&self) -> Vec<ContentBlock> {
        let from = self.upto().saturating_add(1);
        self.history
            .iter()
            .skip(from)
            .flat_map(|m| m.content.iter().cloned())
            .collect()
    }

    pub(super) fn next_tokens(&self, categories: &[CategoryEstimate]) -> u64 {
        match &self.last {
            Some(last) => next_estimate(last.used, last.output_tokens, &self.since_reply()),
            None => categories.iter().fold(0, |s, c| s.saturating_add(c.tokens)),
        }
    }

    pub(super) fn message_estimate(&self, head: &[ContentBlock], typed: &[ContentBlock]) -> u64 {
        let sent = blocks_tokens(head).saturating_add(blocks_tokens(typed));
        let before = match &self.last {
            Some(last) => next_estimate(last.used, last.output_tokens, &self.since_reply()),
            None => self
                .categories(self.history.len(), false)
                .iter()
                .fold(0u64, |s, c| s.saturating_add(c.tokens)),
        };
        before.saturating_add(sent)
    }

    pub(super) fn context_changed(&self) -> AgentEvent {
        let changed = self.context_figures();
        self.agents.set_context(&changed);
        changed
    }

    pub(super) fn meter_figures(&self) -> MeterFigures {
        let (window, point) = self.window_and_point();
        let ceiling = window.map(|window| settings::default_point(window, self.session.max_tokens));
        let auto_compact = self.auto_compact();
        MeterFigures {
            window,
            point,
            ceiling,
            auto_compact,
            limit: limit_of(point, ceiling, auto_compact),
        }
    }

    pub(super) fn context_figures(&self) -> AgentEvent {
        let figures = self.meter_figures();
        let next = match &self.last {
            Some(_) => self.next_tokens(&[]),
            None => self.next_tokens(&self.categories(self.history.len(), true)),
        };
        AgentEvent::ContextChanged {
            used: self.last.as_ref().map_or(next, |l| l.used),
            used_estimated: self.last.is_none(),
            next,
            window: figures.window,
            compact_at: figures.point,
            meter_limit: figures.limit,
            auto_compact: figures.auto_compact,
            epoch: self.epoch_number(),
        }
    }

    pub fn context_now(&self) -> AgentEvent {
        self.context_changed()
    }

    pub fn with_count_tokens(mut self, on: bool) -> Self {
        self.fill.count_off = !on;
        self
    }

    pub(super) fn set_count_tokens(&mut self, on: bool) {
        self.fill.count_off = !on;
    }

    pub(super) fn media_count(&self) -> MediaCount {
        let endpoint = self.endpoint_name().to_owned();
        if self.fill.count_off || self.uncountable(&endpoint) {
            return MediaCount::default();
        }
        let provider = self.provider().clone();
        let (session, features) = self.bare_request_parts();
        let options = self.request_options();
        let uncountable = self.fill.uncountable.clone();
        MediaCount::new(move |block| {
            let messages = [Message {
                role: Role::User,
                content: vec![block],
            }];
            let request = Request {
                session: &session,
                tools: &[],
                deferred: &[],
                by_value: &[],
                server_tools: &[],
                messages: &messages,
                previous_response_id: None,
                features,
                options,
                rendering: &UNRENDERED,
                compaction: None,
            };
            let asked = provider.count_tokens(&request);
            let uncountable = uncountable.clone();
            let endpoint = endpoint.clone();
            async move {
                let answer = asked.await;
                if answer == TokenCount::Unsupported
                    && let Ok(mut set) = uncountable.lock()
                {
                    set.insert(endpoint);
                }
                answer
            }
            .boxed()
        })
    }

    fn measured_fill(&self) -> Option<&LastRequest> {
        self.last.as_ref().filter(|last| {
            last.endpoint == self.endpoint
                && last.asked == self.session.model
                && last.model == last.asked
        })
    }

    fn count_mode(&self) -> Mode {
        if self.fill.count_off {
            Mode::Estimate(EstimateReason::Off)
        } else if self.uncountable(self.endpoint_name()) {
            Mode::Estimate(EstimateReason::NoCounter)
        } else if !self.history.iter().any(|m| m.role == Role::User) {
            Mode::Estimate(EstimateReason::NothingYet)
        } else {
            Mode::Count
        }
    }

    fn fixed(&self, session: &SessionConfig) -> Fixed {
        let (specs, server) = self.wire(&self.counted_tools(), self.server_tools());
        let tools = if specs.is_empty() && server.is_empty() {
            String::new()
        } else {
            crate::tool::tools_json(&specs, server)
        };
        Fixed {
            system: session.system.clone().unwrap_or_default(),
            instructions: session.instructions.clone().unwrap_or_default(),
            tools,
        }
    }

    fn count_layers(&self, session: &SessionConfig, features: Features) -> Vec<(Layer, CountKey)> {
        let fixed = self.fixed(session);
        fixed
            .layers()
            .into_iter()
            .map(|layer| {
                let key = CountKey {
                    endpoint: self.endpoint_name().to_owned(),
                    model: session.model.clone(),
                    effort: session.effort.as_str().to_owned(),
                    thinking: format!("{features:?}"),
                    part: fixed.part(layer),
                };
                (layer, key)
            })
            .collect()
    }

    fn last_user_end(&self) -> usize {
        self.history
            .iter()
            .rposition(|m| m.role == Role::User)
            .map_or(0, |i| i + 1)
    }

    fn whole_extra(&self) -> u64 {
        let dropped = (self.last_user_end().max(self.view_from())..self.history.len())
            .map(|i| {
                self.fill.replies.get(&i).copied().unwrap_or_else(|| {
                    self.history
                        .get(i)
                        .filter(|m| m.role == Role::Assistant)
                        .map_or(0, |m| blocks_tokens(&m.content))
                })
            })
            .fold(0, u64::saturating_add);
        let pending = if self.last.is_none() {
            self.turn_section_texts()
                .iter()
                .map(|block| text_tokens(block))
                .fold(0, u64::saturating_add)
        } else {
            0
        };
        dropped.saturating_add(pending)
    }

    pub(super) fn compact_now_cost(&self) -> Option<CompactionCost> {
        let estimates = self.categories(self.history.len(), self.last.is_none());
        let next = self.next_tokens(&estimates);
        self.compaction_cost_with(&estimates, next)
    }

    fn compaction_cost_with(
        &self,
        estimates: &[CategoryEstimate],
        next: u64,
    ) -> Option<CompactionCost> {
        let info = self.priced_rates(self.endpoint, &self.session.model)?;
        let prefix_tokens = estimates
            .iter()
            .filter(|c| is_fixed(&c.category))
            .fold(0u64, |s, c| s.saturating_add(c.tokens));
        let (anchor, content_since) = self.compaction_anchor();
        let now = CompactNow {
            prefix_tokens,
            content_since,
            next_estimate: next,
            head_tokens: self.compaction_head_tokens(),
            instruction_tokens: self.instruction_estimate(),
            head_ttl: self.session.cache_ttl,
            now: self.clock.now(),
        };
        Some(compaction_cost(
            &compact_now_input(anchor.as_ref(), &now),
            &info,
        ))
    }

    fn report_inputs(&self, detail: ReportDetail, request: RequestId) -> ReportInputs {
        let detail = match detail {
            ReportDetail::All => ReportDetail::All,
            _ => ReportDetail::Summary,
        };
        let resolved = self.resolved();
        let point = self.point(&resolved);
        let max_tokens = self.session.max_tokens;
        let estimates = self.categories(self.history.len(), self.last.is_none());
        let next = self.next_tokens(&estimates);
        let fill = match &self.last {
            Some(last) => Fill::Measured {
                used: last.used,
                turn: last.turn,
                step: u64::from(last.step),
            },
            None if self.compaction.head.is_some() => Fill::EstimatedAfterCompaction {
                tokens: next,
                epoch: self.epoch_number(),
            },
            None => Fill::None,
        };
        let prices = self.priced_rates(self.endpoint, &self.session.model);
        let filled = self.last.as_ref().map_or(next, |l| l.used);
        let cost = self.compaction_cost_with(&estimates, next);
        let report = ContextReport {
            billing: self.provider().billing(),
            compaction_plan_usd: self
                .session_billing
                .filter(|b| !b.is_metered())
                .filter(|_| !self.compaction.plan_unknown)
                .map(|_| self.compaction.plan_usd),
            spent_billing: self.session_billing.unwrap_or_default(),
            answers: request,
            detail,
            model: self.session.model.clone(),
            endpoint: self.endpoint_name().to_owned(),
            fill,
            next,
            window: resolved.window.clone(),
            compact_at: point,
            reserve: resolved
                .window
                .as_ref()
                .map(|w| settings::reserve(w.tokens, max_tokens)),
            auto_compact: self.auto_compact(),
            categories: vec![],
            basis: ReportBasis::default(),
            epoch: self.epoch_number(),
            compactions: self.compaction.compactions,
            compaction_cost_usd: self.compaction.cost_usd.filter(|_| self.priced()),
            reread_cost_usd: prices.map(|info| usd(reread_micros(filled, &info))),
            compact_now_cost_usd: cost.map(|c| usd(c.estimate_micros)),
            compact_now_head_write_usd: cost.map(|c| usd(c.head_write_micros)),
        };
        let mode = self.count_mode();
        let measured = self.measured_fill().map(|_| next);
        let (layers, whole_extra) = if mode == Mode::Count {
            let (session, features) = self.request_parts();
            let whole = measured.is_none().then(|| self.whole_extra());
            (self.count_layers(&session, features), whole)
        } else {
            (vec![], None)
        };
        ReportInputs {
            report,
            estimates,
            layers,
            measured,
            estimated_next: next,
            split_estimate: self.last.is_some(),
            whole_extra,
            mode,
        }
    }

    fn cached_counts(&self) -> BTreeMap<CountKey, u64> {
        self.fill
            .counts
            .lock()
            .map(|counts| counts.clone())
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub(super) fn report(&self, detail: ReportDetail, request: RequestId) -> ContextReport {
        let inputs = self.report_inputs(detail, request);
        let counts = self.cached_counts();
        let results = Results {
            settled: inputs.missing(&counts).is_empty() && !inputs.counts_whole(),
            ..Results::default()
        };
        assemble(&inputs, &counts, &results)
    }

    fn uncountable(&self, endpoint: &str) -> bool {
        self.fill
            .uncountable
            .lock()
            .is_ok_and(|set| set.contains(endpoint))
    }

    fn count_layer(
        &self,
        layer: Layer,
        session: &SessionConfig,
        features: Features,
    ) -> BoxFuture<'static, TokenCount> {
        let mut session = session.clone();
        match layer {
            Layer::Base => {
                session.system = None;
                session.instructions = None;
            }
            Layer::System => session.instructions = None,
            Layer::Instructions | Layer::Tools => {}
        }
        let (counted, server_tools) = self.wire(&self.counted_tools(), self.server_tools());
        let (tools, server_tools) = match layer {
            Layer::Tools => (counted.as_slice(), server_tools),
            Layer::Base | Layer::System | Layer::Instructions => (&[][..], &[][..]),
        };
        let messages = [Message::user_text(COUNT_PLACEHOLDER)];
        let request = Request {
            session: &session,
            tools,
            deferred: &[],
            by_value: &[],
            server_tools,
            messages: &messages,
            previous_response_id: None,
            features,
            options: self.request_options(),
            rendering: &UNRENDERED,
            compaction: None,
        };
        self.provider().count_tokens(&request)
    }

    fn count_whole(
        &self,
        session: &SessionConfig,
        features: Features,
    ) -> BoxFuture<'static, TokenCount> {
        let messages = self.compacted(&self.history[..self.last_user_end()]);
        let rendering = self.rendering(messages.len());
        let (counted, server_tools) = self.wire(&self.counted_tools(), self.server_tools());
        let request = Request {
            session,
            tools: &counted,
            deferred: &[],
            by_value: &[],
            server_tools,
            messages: &messages,
            previous_response_id: None,
            features,
            options: self.request_options(),
            rendering: &rendering,
            compaction: None,
        };
        self.provider().count_tokens(&request)
    }

    pub(super) async fn context_command(
        &mut self,
        detail: ReportDetail,
        request: RequestId,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let typed = match &detail {
            ReportDetail::All => "all".to_owned(),
            ReportDetail::Summary => String::new(),
            ReportDetail::Other(word) => word.clone(),
        };
        self.defer_command("context".to_owned(), typed);
        let inputs = self.report_inputs(detail, request);
        let cached = self.cached_counts();
        let missing = inputs.missing(&cached);
        let whole = inputs.counts_whole();
        if missing.is_empty() && !whole {
            let report = assemble(&inputs, &cached, &Results::settled());
            emit(events, AgentEvent::ContextReport(report)).await;
            return;
        }
        let first = assemble(&inputs, &cached, &Results::default());
        emit(events, AgentEvent::ContextReport(first)).await;
        let (session, features) = self.request_parts();
        let mut asked: Vec<(Option<CountKey>, BoxFuture<'static, TokenCount>)> = missing
            .into_iter()
            .filter_map(|layer| {
                let key = inputs.key(layer)?.clone();
                Some((Some(key), self.count_layer(layer, &session, features)))
            })
            .collect();
        if whole {
            asked.push((None, self.count_whole(&session, features)));
        }
        self.fill.counting.retain(|handle| !handle.is_finished());
        let handle = spawn_counts(
            inputs,
            cached,
            asked,
            self.fill.counts.clone(),
            self.fill.uncountable.clone(),
            events.clone(),
        );
        self.fill.counting.push(handle);
    }

    pub(super) fn stop_counts(&mut self) {
        for handle in self.fill.counting.drain(..) {
            handle.abort();
        }
    }
}

fn spawn_counts(
    inputs: ReportInputs,
    mut known: BTreeMap<CountKey, u64>,
    asked: Vec<(Option<CountKey>, BoxFuture<'static, TokenCount>)>,
    counts: Counts,
    uncountable: Arc<Mutex<BTreeSet<String>>>,
    events: mpsc::Sender<AgentEvent>,
) -> AbortHandle {
    let task = tokio::spawn(async move {
        let (keys, futures): (Vec<_>, Vec<_>) = asked.into_iter().unzip();
        let answers = futures::future::join_all(futures).await;
        let mut results = Results::settled();
        for (key, answer) in keys.into_iter().zip(answers) {
            match (key, answer) {
                (Some(key), TokenCount::Counted { tokens }) => {
                    if let Ok(mut shared) = counts.lock() {
                        shared.insert(key.clone(), tokens);
                    }
                    known.insert(key, tokens);
                }
                (None, TokenCount::Counted { tokens }) => results.whole = Some(tokens),
                (_, TokenCount::Unsupported) => results.unsupported = true,
                (_, TokenCount::Failed { reason }) => {
                    results.failure.get_or_insert(reason);
                }
            }
        }
        if results.unsupported
            && let Ok(mut set) = uncountable.lock()
        {
            set.insert(inputs.report.endpoint.clone());
        }
        let report = assemble(&inputs, &known, &results);
        let _ = events.send(AgentEvent::ContextReport(report)).await;
    });
    task.abort_handle()
}

#[cfg(test)]
mod tests;
