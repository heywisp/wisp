use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use wisp_protocol::settings::names;

use jiff::Timestamp;
use serde_json::Value;
use wisp_protocol::context::format_tokens;
use wisp_protocol::log::{
    BlockPosition, Cut, FeatureState, LogEvent, LogRecord, LoggedBlock, compacted_view, rewind_mask,
};
use wisp_protocol::log::{LoggedHead, LoggedInput, Rebuilt, SectionSnapshot, ToolsCopy};
use wisp_protocol::subscription_auth::{Followed, Stayed};
use wisp_protocol::{
    AgentEvent, ContentBlock, ErrorKind, HoldReason, InputId, InstructionsSource, Message, Role,
    TurnOutcome,
};
use wisp_protocol::{InstructionsSummary, format_usd, marked_amount};

use super::compact::{Head, Ledger};
use super::inbox::TurnEnd;
use super::question::PostedState;
use super::repair::ReturnedInput;
use super::search::holds_thinking_from;
use super::settings::Pending;
use super::withdraw::end_of;
use super::{Agent, InstructionsFacts, LastRequest, sections};
use crate::agents::fold::exit_named;
use crate::context::ttl_span;
use crate::provider::forms::server_call;
use crate::provider::forms::thinking_shaped;
use crate::provider::{Feature, Provider, RequestOptions};
use crate::resume::{
    FeatureFact, FellBack, LoggedSession, Lost, Recovered, ago, feature_state, lost_notes, recover,
    replayed, with_notes,
};
use crate::secrets::import::and_words;
use crate::session::{Line, rebuild_checked, reread, sha256_hex};
use flags::FlagNotes;

mod flags;

#[derive(Debug, Clone, PartialEq)]
pub(super) struct ResumedFrom {
    gap_ms: Option<u64>,
    truncated_bytes: u64,
    notice: String,
    instructions: Option<InstructionsSummary>,
    block_written: bool,
    source_written: Option<InstructionsSource>,
    extra_written: Option<String>,
    pub(super) fell_back: Option<LogEvent>,
    pub(super) fresh: bool,
    replied: bool,
    process_wake: bool,
    pub(super) switched: bool,
    whole_copy: bool,
    logged_tools: Option<String>,
    sections: Option<Vec<SectionSnapshot>>,
    pub(super) agents: (Vec<Followed>, Vec<Stayed>),
}

type Rendered = Result<
    (
        Agent,
        Vec<crate::plugin::Problem>,
        Option<InstructionsFacts>,
    ),
    String,
>;

pub struct ResumeSplit {
    logged: Box<LoggedSession>,
    moved: Option<String>,
    rebuilt: Box<Rebuilt>,
    lost: Vec<String>,
}

struct Notes {
    changed: bool,
    headers: bool,
    whole_copy: bool,
    moved: Option<String>,
    lost: Vec<String>,
    images: LostImages,
    flags: FlagNotes,
}

struct RestoredBlock {
    loaded: bool,
    restorable: bool,
    flagged: Option<InstructionsSource>,
    extra_flagged: Option<String>,
}

#[derive(Default)]
struct LostImages {
    rebuilt: bool,
    unreadable: Option<(u64, String)>,
}

const RETURNED_DROPPED: usize = 3;
const RESUME_CHANGED: &str = "resume";
const LOST_IMAGE: &str = ", and an image this session sent is no longer available";
const REASONING_DROPPED: &str = " and the model's earlier reasoning is dropped";

fn dropped_turns_text(turns: &[u64]) -> String {
    let mut runs: Vec<(u64, u64)> = Vec::new();
    for turn in turns {
        match runs.last_mut() {
            Some((_, last)) if last.checked_add(1) == Some(*turn) => *last = *turn,
            _ => runs.push((*turn, *turn)),
        }
    }
    let parts: Vec<String> = runs
        .iter()
        .map(|(first, last)| {
            if first == last {
                first.to_string()
            } else {
                format!("{first}–{last}")
            }
        })
        .collect();
    let single = matches!(runs.as_slice(), [(first, last)] if first == last);
    let listed = and_words(&parts);
    if single {
        format!("turn {listed} wasn't")
    } else {
        format!("turns {listed} weren't")
    }
}

fn dropped_text(session: &str, fell_back: &FellBack) -> String {
    let mut text = format!(
        "# Messages not restored in session {}\n\nPart of this session's log couldn't be read, so Wisp resumed it from an earlier point. These messages were sent after that point.\n",
        crate::session::short_id(session)
    );
    for (n, input) in fell_back.returned.iter().enumerate() {
        text.push_str(&format!("\n## Message {}\n\n{}\n", n + 1, input.text));
        for image in &input.images {
            text.push_str(&format!(
                "\n- Image {} ({})\n",
                image.sha256,
                image.media_type.as_str()
            ));
        }
    }
    text
}

fn later(at: &str, now: Timestamp) -> Option<u64> {
    let then: Timestamp = at.parse().ok()?;
    let span = now.duration_since(then);
    u64::try_from(span.as_millis()).ok()
}

fn usd(
    amount: f64,
    lower_bound: bool,
    billing: wisp_protocol::Billing,
    plan: Option<f64>,
) -> String {
    if billing == wisp_protocol::Billing::Mixed {
        return match plan {
            Some(plan) => format!(
                "{} · {} {}",
                marked_amount(&format_usd((amount - plan).max(0.0)), false, lower_bound),
                marked_amount(&format_usd(plan), true, false),
                wisp_protocol::SUBSCRIPTIONS_WORD
            ),
            None => wisp_protocol::MIXED_BILLING_NOTE.to_owned(),
        };
    }
    marked_amount(&format_usd(amount), !billing.is_metered(), lower_bound).to_string()
}

fn rewrite_cost_text(amount: Option<f64>) -> String {
    amount
        .map(|amount| format!(" ({})", marked_amount(&format_usd(amount), true, false)))
        .unwrap_or_default()
}

fn turns_text(turns: u64) -> String {
    match turns {
        1 => "1 turn".to_owned(),
        n => format!("{n} turns"),
    }
}

fn opening_texts(event: &LogEvent, mail: &BTreeSet<InputId>) -> Vec<String> {
    match event {
        LogEvent::TurnStarted { text, inputs, .. } if inputs.is_empty() => vec![text.clone()],
        LogEvent::TurnStarted { inputs, .. } => said_texts(inputs, mail),
        _ => Vec::new(),
    }
}

fn said_texts(inputs: &[LoggedInput], mail: &BTreeSet<InputId>) -> Vec<String> {
    inputs
        .iter()
        .filter(|i| !mail.contains(&i.id))
        .map(|i| i.text.clone())
        .collect()
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct HeadTurns {
    turns: u64,
    pub(super) span: Option<(u64, u64)>,
}

impl HeadTurns {
    pub(super) fn see(&mut self, event: &LogEvent) {
        match event {
            LogEvent::TurnStarted { turn, .. } => self.turns = self.turns.max(*turn),
            LogEvent::ContextCompacted { covers, .. } => {
                self.span = Some(match (covers, self.span) {
                    (Some(c), _) => (c.first_turn, c.last_turn),
                    (None, Some(old)) => (old.0, self.turns.max(old.1)),
                    (None, None) => (1, self.turns),
                });
            }
            _ => {}
        }
    }
}

fn head_turns(records: &[LogRecord]) -> Option<(u64, u64)> {
    let mut head = HeadTurns::default();
    records.iter().for_each(|r| head.see(&r.event));
    head.span
}

pub(super) fn native_summary(block: &Value) -> String {
    block
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

impl Head {
    pub(super) fn from_logged(
        head: LoggedHead,
        event: &LogEvent,
        turns: Option<(u64, u64)>,
    ) -> Option<Self> {
        let LogEvent::ContextCompacted { summary, block, .. } = event else {
            return None;
        };
        Some(Self {
            epoch: head.epoch,
            from: head.from,
            message: head.message,
            summary: summary
                .clone()
                .or_else(|| block.as_ref().map(native_summary))
                .unwrap_or_default(),
            native: block.is_some(),
            turns: turns?,
            cut: head.cut,
            placement: head.placement,
            additions: head.additions,
            join: head.join,
        })
    }
}

fn lost_text(lost: &[String]) -> Option<String> {
    let (last, rest) = lost.split_last()?;
    Some(match rest {
        [] => format!("Your switch to {last} wasn't restored: it can't be used now."),
        _ => format!(
            "Your switches to {} weren't restored: they can't be used now.",
            and_words(lost)
        ),
    })
}

fn holds_note(block: &ContentBlock, note: &str) -> bool {
    match block {
        ContentBlock::Text { text } => text == note,
        ContentBlock::ToolResult { content, .. } => content.iter().any(|b| holds_note(b, note)),
        _ => false,
    }
}

fn resumed_end(logged: &LoggedSession, records: &[LogRecord]) -> TurnEnd {
    let held_stopped = records
        .iter()
        .rev()
        .take_while(|r| !matches!(r.event, LogEvent::TurnStarted { .. }))
        .find_map(|r| match &r.event {
            LogEvent::WakeHeld { reason, .. } => Some(*reason == HoldReason::Stopped),
            _ => None,
        })
        .unwrap_or(false);
    let last = logged.last_turn.as_ref();
    let named = last.is_some_and(|t| exit_named(records, t.turn));
    let end = match last.map(|t| &t.outcome) {
        None | Some(Some(TurnOutcome::Completed { .. })) => TurnEnd::Completed,
        Some(Some(TurnOutcome::Error {
            kind: ErrorKind::Interrupted,
            ..
        })) => TurnEnd::Stopped,
        Some(Some(TurnOutcome::Error { .. })) => TurnEnd::Failed,
        Some(Some(TurnOutcome::Cancelled { .. }) | None) => TurnEnd::Stopped,
    };
    if (held_stopped || named) && end == TurnEnd::Completed {
        TurnEnd::Stopped
    } else {
        end
    }
}

fn mail_since_stop(records: &[LogRecord]) -> Vec<InputId> {
    let started = records
        .iter()
        .rposition(|r| matches!(r.event, LogEvent::TurnStarted { .. }));
    let ended = records
        .iter()
        .rposition(|r| matches!(r.event, LogEvent::TurnEnded { .. }));
    if started > ended {
        return Vec::new();
    }
    let stop = records
        .iter()
        .rposition(|r| match &r.event {
            LogEvent::TurnEnded { .. } => true,
            LogEvent::WakeHeld { reason, .. } => *reason == HoldReason::Stopped,
            _ => false,
        })
        .map_or(0, |at| at + 1);
    records
        .get(stop..)
        .unwrap_or_default()
        .iter()
        .filter_map(|r| match &r.event {
            LogEvent::InputAccepted {
                id, from: Some(_), ..
            } => Some(id.clone()),
            _ => None,
        })
        .collect()
}

pub(super) fn run_start(endpoints: &[String], endpoint: &str) -> usize {
    let Some(last) = endpoints.iter().rposition(|e| e == endpoint) else {
        return 0;
    };
    endpoints
        .get(..last)
        .and_then(|before| before.iter().rposition(|e| e != endpoint))
        .map_or(0, |other| other + 1)
}

impl Agent {
    pub fn resume(self, records: &[LogRecord], truncated_bytes: u64) -> Result<Self, String> {
        let lines = reread(records).map_err(|e| e.to_string())?;
        self.resume_lines(lines, truncated_bytes)
    }

    pub fn resume_lines(self, lines: Vec<Line>, truncated_bytes: u64) -> Result<Self, String> {
        let mut recovered = {
            let Some((log, _)) = &self.log else {
                return Err("resuming a session needs its log".to_owned());
            };
            let providers = |name: Option<&str>| Some(self.provider_for(name));
            recover(lines, log.store(), &providers)?
        };
        if recovered.fresh() {
            return Ok(self.start_over(&recovered));
        }
        let (logged, rebuilt) = recovered.take_checked();
        let (agent, split) = self.resume_logged(&recovered, logged.as_ref().ok(), rebuilt)?;
        agent.resume_decided(&recovered, split, truncated_bytes)
    }

    pub fn provider_for(&self, endpoint: Option<&str>) -> Arc<dyn Provider> {
        endpoint
            .and_then(|name| self.connections.iter().find(|c| c.name == name))
            .map_or_else(|| self.provider().clone(), |c| c.provider.clone())
    }

    pub fn resume_rendered<'a>(
        self,
        recovered: &'a Recovered,
        truncated_bytes: u64,
        logged: Option<&'a LoggedSession>,
        rebuilt: Option<Box<Rebuilt>>,
    ) -> Pin<Box<impl Future<Output = Rendered> + 'a>> {
        Box::pin(async move {
            let (agent, split) = self.resume_logged(recovered, logged, rebuilt)?;
            let (mut agent, warnings) = agent.with_start_sections().await;
            agent.describe_copied(&split.logged).await;
            let facts = agent.instructions_facts();
            let agent = agent.resume_decided(recovered, split, truncated_bytes)?;
            Ok((agent, warnings, facts))
        })
    }

    #[cfg(test)]
    pub(crate) fn resume_records_rendered(
        self,
        records: &[LogRecord],
    ) -> Pin<Box<impl Future<Output = Result<Self, String>> + '_>> {
        Box::pin(async move {
            let lines = reread(records).map_err(|e| e.to_string())?;
            let mut recovered = {
                let Some((log, _)) = &self.log else {
                    return Err("resuming a session needs its log".to_owned());
                };
                let providers = |name: Option<&str>| Some(self.provider_for(name));
                recover(lines, log.store(), &providers)?
            };
            if recovered.fresh() {
                let (agent, _) = self.start_over(&recovered).with_start_sections().await;
                return Ok(agent);
            }
            let (logged, rebuilt) = recovered.take_checked();
            let (agent, _, _) = self
                .resume_rendered(&recovered, 0, logged.as_ref().ok(), rebuilt)
                .await?;
            Ok(agent)
        })
    }

    pub fn resume_logged(
        mut self,
        recovered: &Recovered,
        handed: Option<&LoggedSession>,
        rebuilt: Option<Box<Rebuilt>>,
    ) -> Result<(Self, ResumeSplit), String> {
        let records = recovered.records.as_slice();
        let fell_back = recovered.fell_back.as_ref();
        let mut logged = match handed {
            Some(handed) => {
                #[cfg(test)]
                assert_eq!(LoggedSession::read(records).as_ref(), Ok(handed));
                handed.clone()
            }
            None => LoggedSession::read(records)?,
        };
        if let Some(fell_back) = fell_back {
            logged.fell_back(&fell_back.event);
        }
        let moved = self.moved_origin(&logged);
        let Some((log, _)) = &self.log else {
            return Err("resuming a session needs its log".to_owned());
        };
        let rebuild = || {
            let notes = lost_notes(records, &recovered.lost);
            rebuild_checked(log.store(), &with_notes(records, &notes), recovered.start)
                .map_err(|e| e.error.to_string())?
                .ok_or_else(|| "the log's compaction can't be rebuilt".to_owned())
        };
        let rebuilt = match rebuilt {
            Some(rebuilt) => {
                #[cfg(test)]
                assert_eq!(rebuild().as_ref(), Ok(&*rebuilt));
                rebuilt
            }
            None => {
                #[cfg(test)]
                assert!(fell_back.is_some());
                Box::new(rebuild()?)
            }
        };
        let message_threads = self.request_options().message_threads;
        self = self.with_request_options(RequestOptions {
            debug_cache: logged.current.debug_cache,
            refusal_fallback: logged.current.refusal_fallback,
            message_threads,
        });
        self.restore_settings(&logged, moved.is_some());
        let lost = self.restore_pending(&logged);
        self.restore_kernel(&logged);
        Ok((
            self,
            ResumeSplit {
                logged: Box::new(logged),
                moved,
                rebuilt,
                lost,
            },
        ))
    }

    pub fn resume_decided(
        mut self,
        recovered: &Recovered,
        split: ResumeSplit,
        truncated_bytes: u64,
    ) -> Result<Self, String> {
        let records = recovered.records.as_slice();
        let fell_back = recovered.fell_back.as_ref();
        let ResumeSplit {
            logged,
            moved,
            rebuilt,
            lost,
        } = split;
        let logged = *logged;
        let rebuilt = *rebuilt;
        let block = self.restore_instructions(&logged);
        let start = if block.loaded {
            sections::StartAt {
                at: records.len(),
                snapshots: self.sections.start().clone(),
                legacy: BTreeMap::new(),
            }
        } else {
            sections::logged_start(records)
        };
        self.sections.set_start(&start);
        self.sections.resumed_start = Some(start.clone());
        self.search.refused = rebuilt.search_refused.clone();
        let (pair, model) = logged.tools_pair();
        let ran_on = pair.unwrap_or_else(|| self.endpoint_name()).to_owned();
        self.search
            .kept
            .insert((ran_on, model.to_owned()), logged.had_search());
        let started_with = self.toolbox.web_search();
        if logged.had_search() {
            self.toolbox.set_web_search(true);
        }
        let adopted = self.adopt_copy(&logged);
        if !adopted {
            self.toolbox.set_web_search(started_with);
        }
        let whole_copy = !adopted && self.prefix_digests_differ(&logged);
        let block_changed = block.loaded && self.instructions_differ(&logged);

        let keyed: Vec<(u64, &LogEvent)> = records.iter().map(|r| (r.seq, &r.event)).collect();
        let masked = rewind_mask(&keyed).map_err(|e| e.to_string())?;
        let kept: Cow<'_, [LogRecord]> = if masked.contains(&true) {
            Cow::Owned(
                records
                    .iter()
                    .zip(&masked)
                    .filter(|(_, m)| !**m)
                    .map(|(r, _)| r.clone())
                    .collect(),
            )
        } else {
            Cow::Borrowed(records)
        };
        let points = rebuilt.points.clone();
        let restored = rebuilt.restored.clone();
        let highest = records
            .iter()
            .filter_map(|r| match &r.event {
                LogEvent::ContextCompacted { epoch, .. } => Some(*epoch),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        self.compaction.resumed_highest(highest);
        let sources = rebuilt.sources.clone();
        let fallback = logged
            .started
            .endpoint_name
            .clone()
            .unwrap_or_else(|| self.endpoint_name().to_owned());
        self.search.endpoints = rebuilt
            .endpoints
            .iter()
            .map(|e| e.clone().unwrap_or_else(|| fallback.clone()))
            .collect();
        self.accepted = match rebuilt.provisional {
            Some(message) => BlockPosition { message, block: 0 },
            None => end_of(&rebuilt.messages),
        };
        let user_turns: BTreeSet<u64> = rebuilt
            .messages
            .iter()
            .zip(&rebuilt.turns)
            .filter(|(m, _)| m.role == Role::User)
            .map(|(_, turn)| *turn)
            .collect();
        let replies: BTreeMap<usize, u64> = logged
            .replies
            .iter()
            .filter_map(|(record, tokens)| {
                let index = sources.iter().rposition(|s| s == record)?;
                Some((index, *tokens))
            })
            .collect();
        let turns = rebuilt.turns.clone();
        self.fill.resumed(rebuilt.turns.clone(), replies);
        self.search.paused = rebuilt.paused;
        self.search.closed = rebuilt.closed;
        self.search.cuts = rebuilt.cuts;
        self.history = rebuilt.messages;
        self.restore_tool_notes(rebuilt.tool_notes);
        self.restore_kept_breaks(&logged, &sources);
        let compacted = logged.last_compaction.and_then(|index| records.get(index));
        if let (Some(head), Some(record)) = (rebuilt.head, compacted) {
            self.compaction.head = Head::from_logged(head, &record.event, head_turns(&kept));
            self.compaction.resumed_since(record.seq.saturating_add(1));
        }
        self.restore_found(
            records
                .iter()
                .enumerate()
                .map(|(index, record)| (index, &record.event, masked[index])),
            &sources,
        );
        self.settle_resumed_tools();
        if whole_copy {
            self.rebuilt_at(self.history.len());
        }
        let changed = whole_copy || block_changed;
        if changed {
            self.settings.binding_controls = true;
            self.keeps_thinking();
        }
        self.compaction.compactions = logged.compactions;
        self.compaction.cost_usd = logged.compaction_usd;
        self.compaction.plan_usd = logged.compaction_plan_usd;
        self.compaction.plan_unknown = logged.compaction_plan_unknown;
        let ledgers = self.replay_notes(&kept);
        records.iter().for_each(|r| self.naming.see(&r.event));

        self.turns = logged.turns_seen;
        self.queue.lane.seed(records);
        self.seed_question_letters(records);
        self.session_usd = logged.session_usd;
        self.session_billing = resumed_billing(records, fell_back);
        self.session_plan_usd = logged.plan_usd;
        self.session_plan_unknown = logged.plan_unknown;
        self.session_lower_bound = logged.lower_bound;
        self.accounts = logged.accounts.clone();
        self.completed_on = logged.completed_on.clone();
        self.instructions_announced = true;
        let moves = sections::moves(
            records
                .iter()
                .enumerate()
                .zip(&masked)
                .filter(|(_, m)| !**m)
                .map(|(r, _)| r),
        );
        let committed = self.committed_at(
            &moves.window(logged.last_compaction, records.len()),
            |turn| user_turns.contains(&turn),
            &start,
        );
        self.sections.restore(committed);
        let outputs = logged
            .started_tools
            .iter()
            .filter(|name| {
                self.toolbox
                    .get(name)
                    .is_some_and(|tool| tool.meta().saves_output)
            })
            .count();
        self.outputs = u32::try_from(outputs).unwrap_or(u32::MAX);
        if self.has_todos() {
            self.todos.clone_from(&logged.todos);
        }
        self.posted.clone_from(&logged.posted);
        if let (Some(env), Some(path)) = (&mut self.tools, &logged.tool_cwd) {
            env.cwd = Some(PathBuf::from(path));
        }
        self.restore_search(&logged, &sources);
        self.restore_windows(&logged);
        let images = self.restore_lost(&recovered.lost, &turns);
        self.last = self.resumed_last(&logged, &sources);
        self.search.drops_covered = false;
        let headers = self.resumed_betas_differ(&logged);
        self.note_committed(
            logged
                .last_step
                .as_ref()
                .and_then(|s| s.prefix.clone())
                .filter(|_| !headers),
        );
        self.restore_refreshed(&logged);
        let reply = logged.last_step.as_ref().and_then(|s| s.reply);
        self.epoch
            .logged_at(reply.and_then(|r| records.get(r)).map(|r| r.seq));
        self.epoch.take_logged_entry();
        let flags = self.flag_notes(&logged, &block, block_changed);
        if changed || headers || flags.may_rebuild() {
            self.epoch.expect(RESUME_CHANGED);
        }
        self.sync_history();
        self.restore_processes(
            (records, &masked),
            &recovered.dropped_processes,
            logged.processes_seen,
        );
        self.repair = Some(self.plan_repair(&logged, records, &turns, &recovered.dropped_inputs));
        self.restore_mail(&logged);
        if let Some(posted) = &mut self.posted
            && let PostedState::Ending(Some(id)) = &posted.state
            && !self.inbox.waiting_mail().any(|input| input.id == *id)
        {
            posted.state = PostedState::Ending(None);
        }
        let end = resumed_end(&logged, records);
        self.end_wakes(end);
        let completed = end == TurnEnd::Completed
            && logged
                .last_turn
                .as_ref()
                .is_some_and(|last| matches!(last.outcome, Some(TurnOutcome::Completed { .. })));
        if completed && let Some(last) = &logged.last_turn {
            self.keep_warm.resumed = Some(
                logged
                    .warm_spell
                    .filter(|spell| spell.turn == last.turn)
                    .unwrap_or(crate::resume::WarmSpell {
                        turn: last.turn,
                        ..Default::default()
                    }),
            );
        }
        let process_wake = completed
            && !records
                .iter()
                .rev()
                .take_while(|r| !matches!(r.event, LogEvent::TurnStarted { .. }))
                .any(|r| matches!(r.event, LogEvent::SessionResumed { .. }));
        if end != TurnEnd::Completed {
            self.inbox.return_mail();
            if self.inbox.wakes_from_above(&mail_since_stop(records)) {
                self.inbox.clear_returned();
            }
        }
        if fell_back.is_none_or(|f| f.dropped_turns.is_empty()) {
            self.restore_logged_offer(&logged);
        }
        self.resume_marks(records, &masked, (&points, restored), (&turns, ledgers));
        let saved = fell_back.and_then(|f| self.return_dropped(f));

        let now = self.clock.now();
        let notes = Notes {
            changed,
            headers,
            whole_copy,
            moved,
            lost,
            images,
            flags,
        };
        let notice = self.resume_notice(&logged, now, &notes, fell_back, saved.as_deref());
        let switched = self.flag_value(names::ENDPOINT).is_none()
            && logged
                .endpoint()
                .is_some_and(|ran_on| ran_on != self.endpoint_name());
        self.cache_rebuild = (notes.changed || notes.headers).then(|| super::CacheRebuild {
            usd: self.resume_quote(),
        });
        let instructions = if block.loaded {
            self.instructions.as_ref().map(|s| s.summary.clone())
        } else {
            logged.instructions_loaded.clone()
        };
        let source_written = self
            .instructions
            .as_ref()
            .map(|s| s.source.clone())
            .filter(|source| logged.instructions_source.as_ref() != Some(source));
        self.resumed = Some(Box::new(ResumedFrom {
            gap_ms: later(&logged.last_at, now),
            truncated_bytes,
            notice,
            instructions,
            block_written: block.loaded && (block_changed || !block.restorable),
            source_written,
            extra_written: block.extra_flagged.clone().filter(|_| block.loaded),
            fell_back: fell_back.map(|f| f.event.clone()),
            fresh: false,
            replied: logged.last_turn.as_ref().is_some_and(|t| t.replied),
            process_wake,
            switched,
            whole_copy,
            logged_tools: Some(logged.tools.digest()),
            sections: block.loaded.then(|| self.start_sections()),
            agents: (Vec::new(), Vec::new()),
        }));
        self.evict_media();
        Ok(self)
    }

    pub fn start_over(mut self, recovered: &Recovered) -> Self {
        let Some(fell_back) = recovered.fell_back.as_ref() else {
            return self;
        };
        let records = recovered.records.as_slice();
        self.turns = fell_back.turns_seen();
        self.session_usd = LoggedSession::cost(records) + fell_back.spent_usd();
        self.session_billing = resumed_billing(records, Some(fell_back));
        let (plan, unknown) =
            crate::resume::fell_back_plan(fell_back.spent_usd(), fell_back.plan_usd());
        self.session_plan_usd = crate::resume::plan_usd(records) + plan;
        self.session_plan_unknown = crate::resume::plan_split(records).is_none() || unknown;
        self.session_lower_bound = true;
        self.apply_carried(records);
        self.queue.lane.seed(records);
        self.seed_question_letters(records);
        let unmasked = vec![false; records.len()];
        self.restore_processes(
            (records, &unmasked),
            &recovered.dropped_processes,
            fell_back.processes_seen(),
        );
        let saved = self.return_dropped(fell_back);
        let short = crate::session::short_id(&self.session_id).to_owned();
        let mut notice = format!(
            "Session {short}'s log couldn't be read from its start, so it continues as a new conversation; nothing earlier was restored ({} was spent before).",
            usd(
                self.session_usd,
                true,
                self.session_billing.unwrap_or_default(),
                self.session_plan_split()
            )
        );
        if let Some(path) = saved {
            notice.push_str(&format!(" Your messages are in {path}."));
        }
        if self.returns_dropped(fell_back) {
            notice.push_str(" The last ones are back in the composer.");
        }
        self.resumed = Some(Box::new(ResumedFrom {
            gap_ms: None,
            truncated_bytes: 0,
            notice,
            instructions: None,
            block_written: false,
            source_written: None,
            extra_written: None,
            fell_back: Some(fell_back.event.clone()),
            fresh: true,
            replied: false,
            process_wake: false,
            switched: false,
            whole_copy: false,
            logged_tools: None,
            sections: None,
            agents: (Vec::new(), Vec::new()),
        }));
        self
    }

    fn restore_lost(&mut self, lost: &[Lost], turns: &[u64]) -> LostImages {
        let mut images = LostImages::default();
        let (endpoint, model) = (self.endpoint_name().to_owned(), self.session.model.clone());
        let usable = self.binding_usable(&endpoint, &model);
        for item in lost {
            let at = self.note_position(item, turns);
            let replaced = match (&self.compaction.head, at) {
                (Some(head), Some(at)) => at.message < head.from,
                (_, None) => true,
                (None, Some(_)) => false,
            };
            let mut records = vec![item.event()];
            if !replaced {
                images.rebuilt = true;
                self.settings.binding_controls = true;
                if usable {
                    self.keep_break_through(self.history.len());
                }
                let through = self.history.len().saturating_sub(1);
                if let Some(from) =
                    at.filter(|from| !usable && holds_thinking_from(&self.history, *from, through))
                {
                    records.push(LogEvent::ThinkingCut {
                        from,
                        through,
                        tools_sha256: None,
                    });
                    self.search.cuts.push(Cut {
                        from,
                        through,
                        tools: false,
                    });
                }
            }
            match &item.transient {
                Some(reason) => {
                    images.unreadable.get_or_insert((item.turn, reason.clone()));
                    self.deferred_lost.extend(records);
                }
                None => self.resumed_records.extend(records),
            }
        }
        images
    }

    fn note_position(&self, lost: &Lost, turns: &[u64]) -> Option<BlockPosition> {
        self.history.iter().enumerate().find_map(|(message, m)| {
            if m.role != Role::User || turns.get(message) != Some(&lost.turn) {
                return None;
            }
            m.content
                .iter()
                .position(|b| holds_note(b, &lost.note))
                .map(|block| BlockPosition { message, block })
        })
    }

    fn returns_dropped(&self, fell_back: &FellBack) -> bool {
        fell_back
            .returned
            .iter()
            .any(|input| self.returned.iter().any(|r| r.id == input.id))
    }

    fn return_dropped(&mut self, fell_back: &FellBack) -> Option<String> {
        let dropped: Vec<ReturnedInput> = fell_back
            .returned
            .iter()
            .filter(|input| !self.returned.iter().any(|r| r.id == input.id))
            .map(|input| self.returned_input(input))
            .collect();
        let skip = dropped.len().saturating_sub(RETURNED_DROPPED);
        self.returned.extend(dropped.into_iter().skip(skip));
        if fell_back.returned.is_empty() {
            return None;
        }
        let text = dropped_text(&self.session_id, fell_back);
        let (log, _) = self.log.as_mut()?;
        match log.write_dropped(fell_back.seq, &text) {
            Ok(Some(path)) => Some(path.display().to_string()),
            Ok(None) | Err(_) => None,
        }
    }

    pub fn carry_agents(&mut self, moved: Vec<Followed>, stay: Vec<Stayed>) {
        if let Some(resumed) = self.resumed.as_mut() {
            resumed.agents = (moved, stay);
        }
    }

    pub(super) fn resumed_event(&mut self) -> AgentEvent {
        let (moved, stay) = self
            .resumed
            .as_mut()
            .map(|r| std::mem::take(&mut r.agents))
            .unwrap_or_default();
        AgentEvent::SessionResumed {
            turns: self.turns,
            moved,
            stay,
        }
    }

    pub fn resume_notice_text(&self) -> Option<&str> {
        self.resumed.as_ref().map(|r| r.notice.as_str())
    }

    pub fn resumed_instructions(&self) -> Option<&InstructionsSummary> {
        self.resumed.as_ref().and_then(|r| r.instructions.as_ref())
    }

    pub(super) fn resumed_block_written(&self) -> bool {
        self.resumed.as_ref().is_some_and(|r| r.block_written)
    }

    fn moved_origin(&self, logged: &LoggedSession) -> Option<String> {
        let name = self.endpoint_name();
        if logged.endpoint().is_some_and(|ran_on| ran_on != name) {
            return None;
        }
        let now = self.connection().context.endpoint.as_str();
        match logged.origins.get(name) {
            Some(then) if then != now => Some(format!(
                "{} now points at {now}, not {then} as when this session ran; Wisp continues there.",
                wisp_tool_text::output::capitalised(crate::endpoints::goes::named(name))
            )),
            _ => None,
        }
    }

    fn resumed_betas_differ(&self, logged: &LoggedSession) -> bool {
        let Some(logged) = &logged.sent_betas else {
            return false;
        };
        let (session, features) = self.request_parts();
        let messages = self.view_messages();
        let (tools, server_tools) = self.wire(&self.sent, self.server_tools());
        let request = crate::provider::Request {
            session: &session,
            tools: &tools,
            deferred: super::tool_changes::deferred_for(&self.deferred, features),
            by_value: super::tool_changes::by_value_for(&self.deferred, features),
            server_tools,
            messages: &messages,
            previous_response_id: None,
            features,
            options: self.request_options(),
            rendering: &crate::provider::UNRENDERED,
            compaction: None,
        };
        let compared = |betas: &[String]| -> Vec<String> {
            betas
                .iter()
                .filter(|b| self.provider().keys_cache(b))
                .cloned()
                .collect()
        };
        compared(logged) != compared(&self.provider().betas(&request))
    }

    fn prefix_digests_differ(&self, logged: &LoggedSession) -> bool {
        let system = self.session.system.as_deref().map(sha256_hex);
        system != logged.system_prompt_sha256
            || self
                .logged_tools_now(logged)
                .is_some_and(|digest| digest != logged.tools.digest())
    }

    fn instructions_differ(&self, logged: &LoggedSession) -> bool {
        self.session.instructions.as_deref().map(sha256_hex) != logged.instructions_sha256
    }

    pub(crate) fn cache_rebuild(&self) -> Option<super::CacheRebuild> {
        self.cache_rebuild
    }

    pub fn resume_quote(&self) -> Option<f64> {
        self.last
            .is_some()
            .then(|| self.rewrite_usd(self.fill_estimate()))
            .flatten()
    }

    fn tools_difference(&self, logged: &LoggedSession) -> String {
        let parts = self.tools_changes(logged);
        if parts.is_empty() {
            return String::new();
        }
        format!(
            " This run's tools differ from the session's: {}.",
            parts.join("; ")
        )
    }

    fn tools_changes(&self, logged: &LoggedSession) -> Vec<String> {
        let (endpoint, model) = logged.tools_pair();
        let endpoint = endpoint.unwrap_or_else(|| self.endpoint_name());
        if !self.connections.iter().any(|c| c.name == endpoint) {
            return Vec::new();
        }
        let now = self.tool_set(endpoint, model).names;
        let then = &logged.tools.names;
        let added: Vec<&str> = now
            .iter()
            .filter(|name| !then.contains(name))
            .map(String::as_str)
            .collect();
        let removed: Vec<&str> = then
            .iter()
            .filter(|name| !now.contains(name))
            .map(String::as_str)
            .collect();
        [("added", added), ("removed", removed)]
            .into_iter()
            .filter(|(_, names)| !names.is_empty())
            .map(|(what, names)| format!("{what} {}", names.join(", ")))
            .collect()
    }

    fn logged_tools_now(&self, logged: &LoggedSession) -> Option<String> {
        let (endpoint, model) = logged.tools_pair();
        let endpoint = endpoint.unwrap_or_else(|| self.endpoint_name());
        self.connections
            .iter()
            .any(|c| c.name == endpoint)
            .then(|| self.tool_set(endpoint, model).digest)
    }

    fn restore_instructions(&mut self, logged: &LoggedSession) -> RestoredBlock {
        let flagged = self.flagged_instructions(logged);
        let extra_flagged = self.flagged_extra(logged);
        let block = match (&logged.instructions, &logged.instructions_sha256) {
            (None, None) => Some(None),
            (Some(text), Some(hash)) if sha256_hex(text) == *hash => Some(Some(text.clone())),
            _ => None,
        };
        let restorable = block.is_some();
        let restored = match block.filter(|_| flagged.is_none() && extra_flagged.is_none()) {
            Some(block) => {
                self.session.instructions = block;
                true
            }
            None => false,
        };
        if let Some(state) = &mut self.instructions {
            if let Some(summary) = logged.instructions_loaded.as_ref().filter(|_| restored) {
                state.summary = summary.clone();
            }
            if flagged.is_none()
                && let Some(source) = &logged.instructions_source
            {
                state.source = source.clone();
            }
        }
        RestoredBlock {
            loaded: !restored,
            restorable,
            flagged,
            extra_flagged,
        }
    }

    fn restore_pending(&mut self, logged: &LoggedSession) -> Vec<String> {
        let mut lost = Vec::new();
        let flagged_endpoint = self.flag_value(names::ENDPOINT).is_some();
        let logged_pending = logged.pending.clone();
        let endpoint = match logged_pending.as_ref().and_then(|p| p.endpoint.as_deref()) {
            Some(_) if flagged_endpoint => self.endpoint,
            Some(name) => match self.connections.iter().position(|c| c.name == name) {
                Some(index) => index,
                None => {
                    lost.push(crate::endpoints::goes::named(name));
                    self.endpoint
                }
            },
            None => self.endpoint,
        };
        let model = self
            .flag_value(names::MODEL)
            .map(|v| v.text())
            .or_else(|| logged_pending.as_ref().map(|p| p.model.clone()))
            .unwrap_or_else(|| self.session.model.clone());
        let effort = self
            .flag_value(names::EFFORT)
            .and_then(|v| v.text().parse().ok())
            .or_else(|| logged_pending.as_ref().map(|p| p.effort))
            .unwrap_or(self.session.effort);
        let effort = crate::models::ModelTable::shared().own_effort(&model, effort);
        let unchanged = endpoint == self.endpoint
            && model == self.session.model
            && effort == self.session.effort;
        self.settings.pending = (!unchanged).then_some(Pending {
            endpoint,
            model,
            effort,
        });
        if !unchanged && endpoint != self.endpoint {
            match self.reserve_binding(endpoint) {
                Ok(binding) => self.pending_endpoint_binding = binding,
                Err(reason) => {
                    lost.push(reason);
                    self.settings.pending = None;
                    self.pending_endpoint_binding = None;
                }
            }
        }
        lost
    }

    fn restore_settings(&mut self, logged: &LoggedSession, origin_moved: bool) {
        let current = &logged.current;
        self.session.model.clone_from(&current.model);
        self.session.effort =
            crate::models::ModelTable::shared().own_effort(&current.model, current.effort);
        self.session.cache_ttl = current.cache_ttl;
        self.session.message_time = current.message_time;
        self.session.context_note = current.context_note;
        if let Some(on) = current.auto_compact {
            self.set_auto_compact(on);
        }
        let fallback = logged.started.endpoint_name.as_deref();
        let mut states = BTreeMap::new();
        for logged_feature in &logged.features {
            let Some(name) = logged_feature.endpoint.as_deref().or(fallback) else {
                continue;
            };
            let now = self
                .connections
                .iter()
                .find(|c| c.name == name)
                .map(|c| &c.context.endpoint);
            if now.is_none() || logged.origins.get(name) != now {
                continue;
            }
            states.insert(
                (
                    name.to_owned(),
                    logged_feature.model.clone(),
                    logged_feature.feature,
                ),
                logged_feature.state.into(),
            );
        }
        self.settings.resumed(logged.started.effort, states);
        let moved = origin_moved
            || logged
                .endpoint()
                .is_some_and(|name| name != self.endpoint_name());
        let bound = logged
            .features
            .iter()
            .any(|f| f.feature == Feature::BindingControls);
        let several = self.connections.len() > 1;
        let guessed = several || logged.switched || moved || bound;
        self.settings.binding_controls = logged
            .current
            .binding_controls
            .map_or(guessed, |on| on || moved);
        self.settings.turn_binding = logged.turn_binding.map_or(guessed, |on| on || moved);
        let last_turn = logged.kept_breaks.iter().any(|mark| {
            mark.last_turn && self.mark_usable(logged, mark.endpoint.as_deref(), &mark.model)
        });
        self.settings.binding_controls |= last_turn;
    }

    fn mark_usable(&self, logged: &LoggedSession, endpoint: Option<&str>, model: &str) -> bool {
        let endpoint = endpoint
            .or(logged.started.endpoint_name.as_deref())
            .unwrap_or(self.endpoint_name());
        self.binding_usable(endpoint, model)
    }

    fn restore_kept_breaks(&mut self, logged: &LoggedSession, sources: &[usize]) {
        let before = |at: usize| sources.partition_point(|s| *s < at);
        for mark in &logged.kept_breaks {
            if self.mark_usable(logged, mark.endpoint.as_deref(), &mark.model) {
                self.keep_break_through(before(mark.at));
            }
        }
        for tail in &logged.closed_tails {
            let kept = self.mark_usable(logged, tail.endpoint.as_deref(), &tail.model)
                && first_call(&self.history, &tail.calls).is_some_and(|from| {
                    let through = tail.through.min(self.history.len().saturating_sub(1));
                    holds_thinking_from(&self.history, from, through)
                });
            if kept {
                self.keep_break_through(before(tail.at));
                self.settings.binding_controls |= tail.last_turn;
            }
        }
    }

    fn apply_carried(&mut self, records: &[LogRecord]) {
        let current = self.endpoint_name().to_owned();
        for record in records {
            if let LogEvent::WindowLowered {
                endpoint,
                model,
                tokens,
            } = &record.event
            {
                self.models.restore_overflow(endpoint, model, *tokens);
                continue;
            }
            match feature_state(&record.event) {
                Some(FeatureFact::Pair {
                    feature,
                    endpoint,
                    model,
                    state,
                }) => {
                    if feature == Feature::BindingControls {
                        self.settings.binding_controls = true;
                    }
                    if endpoint.is_none_or(|name| name == current) {
                        let key = (current.clone(), model.to_owned(), feature);
                        self.settings.carried(key, state.into());
                    }
                }
                Some(FeatureFact::Search {
                    endpoint,
                    state: FeatureState::FallenBack,
                }) => {
                    self.search.refused.insert(endpoint.to_owned());
                }
                Some(FeatureFact::Search {
                    state: FeatureState::Proven,
                    ..
                })
                | None => {}
            }
        }
    }

    fn restore_windows(&mut self, logged: &LoggedSession) {
        for (endpoint, model, tokens) in &logged.windows {
            self.models.restore_overflow(endpoint, model, *tokens);
        }
    }

    fn restore_search(&mut self, logged: &LoggedSession, sources: &[usize]) {
        self.search.built_with = Some(match &logged.thinking_tools {
            Some(hash) => hash.digest(),
            None => self.current_tools().digest,
        });
        self.search.form = logged.form.unwrap_or_default();
        self.search.marker_dropped = logged.marker_dropped.clone();
        self.search.notes = logged.notes.clone();
        for (endpoint, record) in &logged.rendered {
            let at_record = sources.partition_point(|s| s < record);
            let through = at_record.max(run_start(&self.search.endpoints, endpoint));
            self.search.rendered_on.insert(endpoint.clone());
            self.search
                .rendered_through
                .insert(endpoint.clone(), through);
        }
        for (endpoint, record) in &logged.unreadable {
            let boundary = sources.partition_point(|s| s < record);
            self.search.unreadable.insert(endpoint.clone(), boundary);
        }
    }

    fn resumed_last(&self, logged: &LoggedSession, sources: &[usize]) -> Option<LastRequest> {
        if logged.cold_since_step {
            return None;
        }
        let step = logged.last_step.as_ref()?;
        let reply = sources.iter().rposition(|s| Some(*s) == step.reply)?;
        let before = self.history.get(..reply)?;
        let messages_len = match &self.compaction.head {
            Some(head) if head.leading() && reply >= head.from => head.logged().view(before).len(),
            Some(head) if reply >= head.from => {
                compacted_view(&head.message, head.from, before).len()
            }
            Some(_) => return None,
            None => reply,
        };
        self.logged_last(step, Some(messages_len))
    }

    fn replay_notes(&mut self, records: &[LogRecord]) -> BTreeMap<u64, Ledger> {
        let mut ledgers = BTreeMap::new();
        let mut opening = false;
        let mut mail = BTreeSet::new();
        let mut attached: Vec<(&str, &str)> = Vec::new();
        for record in records {
            let seq = Some(record.seq);
            match &record.event {
                LogEvent::InputAccepted {
                    id, from: Some(_), ..
                } => {
                    mail.insert(id.clone());
                }
                LogEvent::TurnStarted { turn, .. } => {
                    ledgers.insert(record.seq, self.compaction.calls_now());
                    self.turns = *turn;
                    self.note_opening(*turn, opening_texts(&record.event, &mail), seq);
                    opening = true;
                }
                LogEvent::MessageCommitted {
                    turn,
                    message,
                    model,
                    ..
                } => {
                    if std::mem::take(&mut opening) {
                        self.note_inputs(*turn, Vec::new(), seq);
                    }
                    if let Some(model) = model
                        && message.role == Role::Assistant
                        && message
                            .content
                            .iter()
                            .any(|b| matches!(b, LoggedBlock::Inline(b) if thinking_shaped(b)))
                    {
                        self.settings.thought(model);
                    }
                }
                LogEvent::InputCommitted { turn, inputs, .. } => {
                    opening = false;
                    self.note_inputs(*turn, said_texts(inputs, &mail), seq);
                }
                LogEvent::AttachmentsWithdrawn { items, .. } => {
                    for item in items {
                        if let Some(at) = attached.iter().rposition(|a| a.0 == item.sha256) {
                            self.compaction.withdraw_call(attached.remove(at).1);
                        }
                    }
                }
                event => {
                    if let LogEvent::ToolCallFinished {
                        call_id,
                        attachments,
                        ..
                    } = event
                    {
                        attached.extend(
                            attachments
                                .iter()
                                .map(|a| (a.sha256.as_str(), call_id.as_str())),
                        );
                    }
                    if let Some((turn, event)) = replayed(event) {
                        self.turns = turn;
                        self.note_event(&event, seq);
                    }
                }
            }
        }
        ledgers
    }

    pub(crate) fn carry_on(&mut self, interrupted: bool) -> bool {
        let news = self.resumed.as_ref().is_some_and(|r| r.process_wake)
            && self.hooks_word_news()
            && self.restart_news();
        if (!interrupted && !news)
            || (interrupted
                && self.path().is_root()
                && !self.resumed.as_ref().is_some_and(|r| r.replied))
        {
            return false;
        }
        self.offered = None;
        self.returned.append(&mut self.withheld);
        self.end_wakes(TurnEnd::Completed);
        self.inbox.clear_returned();
        true
    }

    fn restore_refreshed(&mut self, logged: &LoggedSession) {
        let (Some(last), Some(at)) = (&self.last, refreshed_at(logged)) else {
            return;
        };
        let (turn, view_len) = (last.turn, last.messages_len);
        self.epoch.refreshed(turn, view_len, at);
    }

    fn warm(&self, logged: &LoggedSession, now: Timestamp, notes: &Notes) -> bool {
        if notes.changed || notes.images.rebuilt || notes.moved.is_some() {
            return false;
        }
        let Some(step) = &logged.last_step else {
            return false;
        };
        let same_pair = step.endpoint_name.as_deref() == Some(self.endpoint_name())
            && step.model == self.session.model;
        let Ok(sent) = step.started_at.parse::<Timestamp>() else {
            return false;
        };
        let sent = refreshed_at(logged).map_or(sent, |at| at.max(sent));
        same_pair
            && self.last.is_some()
            && now.duration_since(sent) < ttl_span(self.session.cache_ttl)
    }

    pub(super) fn rewrite_usd(&self, tokens: u64) -> Option<f64> {
        let pair = self.next_pair();
        let ttl = self.next_cache_ttl().unwrap_or(self.session.cache_ttl);
        let info = self.priced_rates(pair.endpoint, &pair.model)?;
        Some(info.rewrite_usd(tokens, ttl))
    }

    fn resume_notice(
        &self,
        logged: &LoggedSession,
        now: Timestamp,
        notes: &Notes,
        fell_back: Option<&FellBack>,
        saved: Option<&str>,
    ) -> String {
        let head = match fell_back {
            Some(fell_back) => self.fallback_head(logged, now, fell_back, saved),
            None => format!(
                "Resumed session {} ({}, {}, last active {}).",
                logged.short_id(),
                turns_text(logged.turns),
                usd(
                    logged.session_usd,
                    logged.lower_bound,
                    self.session_billing.unwrap_or_default(),
                    self.session_plan_split()
                ),
                ago(&logged.last_at, now)
            ),
        };
        let mut notice = self.cache_notice(head, logged, now, notes);
        let classed = notes.flags.causes().is_empty() && self.warm(logged, now, notes);
        if let Some(next) = notes.flags.next_turn(classed) {
            notice.push(' ');
            notice.push_str(&next);
        }
        let unreadable = notes.images.unreadable.as_ref().map(|(turn, reason)| {
            format!(
                "An image in turn {turn} can't be read right now ({reason}). To keep it, quit and fix the file; sending a message continues without it, for good."
            )
        });
        for extra in notes
            .moved
            .iter()
            .cloned()
            .chain(lost_text(&notes.lost))
            .chain(unreadable)
        {
            notice.push(' ');
            notice.push_str(&extra);
        }
        notice
    }

    fn rewrite_cost(&self) -> (u64, String) {
        let tokens = self.fill_estimate();
        (tokens, rewrite_cost_text(self.rewrite_usd(tokens)))
    }

    fn fallback_head(
        &self,
        logged: &LoggedSession,
        now: Timestamp,
        fell_back: &FellBack,
        saved: Option<&str>,
    ) -> String {
        let id = logged.short_id();
        let spent = usd(
            logged.session_usd,
            logged.lower_bound,
            self.session_billing.unwrap_or_default(),
            self.session_plan_split(),
        );
        let facts = format!(
            "{}, {spent}, last active {}",
            turns_text(logged.turns),
            ago(&logged.last_at, now)
        );
        if fell_back.dropped_turns.is_empty() {
            if !fell_back.cut {
                return format!(
                    "Resumed session {id} ({facts}). Part of its log before the last compaction couldn't be read; the conversation the model sees is complete."
                );
            }
            let after = fell_back
                .last_turn
                .map(|turn| format!(" after turn {turn}"))
                .unwrap_or_default();
            return format!(
                "Resumed session {id} ({facts}). Part of its log{after} couldn't be read and was skipped; the conversation is complete."
            );
        }
        let dropped = dropped_turns_text(&fell_back.dropped_turns);
        let mut head = match fell_back.last_turn {
            Some(kept) => format!(
                "Resumed session {id} from turn {kept} ({facts}): part of its log couldn't be read, so {dropped} restored."
            ),
            None => format!(
                "Resumed session {id} with none of its turns ({spent}, last active {}): part of its log couldn't be read, so {dropped} restored.",
                ago(&logged.last_at, now)
            ),
        };
        if fell_back.settings_dropped {
            match fell_back.last_turn {
                Some(kept) => head.push_str(&format!(
                    " Settings changed after turn {kept} weren't restored either."
                )),
                None => head.push_str(" Settings changed in them weren't restored either."),
            }
        }
        if let Some(path) = saved {
            head.push_str(&format!(" Your messages from them are in {path}."));
        }
        if self.returns_dropped(fell_back) {
            head.push_str(" The last ones are back in the composer.");
        }
        head
    }

    fn cache_notice(
        &self,
        head: String,
        logged: &LoggedSession,
        now: Timestamp,
        notes: &Notes,
    ) -> String {
        let causes = notes.flags.causes();
        let image = if notes.images.rebuilt { LOST_IMAGE } else { "" };
        if notes.changed || !causes.is_empty() {
            let cost = if self.last.is_some() {
                self.rewrite_cost().1
            } else {
                String::new()
            };
            if !causes.is_empty() {
                return self.flags_rebuild(head, logged, notes, &causes, &cost);
            }
            let tools = self.tools_difference(logged);
            return format!(
                "{head}{tools} Wisp's prompt or tools changed since this session{image}: the cache is rebuilt{cost}{REASONING_DROPPED}; the conversation itself is kept."
            );
        }
        if notes.headers {
            let cost = if self.last.is_some() {
                self.rewrite_cost().1
            } else {
                String::new()
            };
            let reasoning = if notes.images.rebuilt {
                REASONING_DROPPED
            } else {
                ""
            };
            return format!(
                "{head} Wisp's request headers changed since this session{image}: the cache is rebuilt{cost}{reasoning}; the conversation itself is kept."
            );
        }
        if notes.images.rebuilt {
            let cost = if self.last.is_some() {
                self.rewrite_cost().1
            } else {
                String::new()
            };
            return format!(
                "{head} An image this session sent is no longer available, so the cache is rebuilt{cost}{REASONING_DROPPED}."
            );
        }
        if self.warm(logged, now, notes) {
            return head;
        }
        if self.last.is_none() {
            return format!("{head} The cache has likely expired; your next message rewrites it.");
        }
        let (tokens, cost) = self.rewrite_cost();
        format!(
            "{head} The cache has likely expired; your next message rewrites about {} tokens{cost}.",
            format_tokens(tokens)
        )
    }

    fn flags_rebuild(
        &self,
        head: String,
        logged: &LoggedSession,
        notes: &Notes,
        causes: &[String],
        cost: &str,
    ) -> String {
        let mut because = format!("Your flags change {}", and_words(causes));
        let tools = self.tools_changes(logged);
        if !tools.is_empty() {
            because.push_str(&format!(
                ", and this run's tools differ from the session's ({})",
                tools.join("; ")
            ));
        } else if notes.whole_copy || (notes.changed && !notes.flags.instructions_rebuilt) {
            because.push_str(", and Wisp's prompt changed since this session");
        }
        if notes.images.rebuilt {
            because.push_str(LOST_IMAGE);
        }
        let reasoning = if notes.flags.reasoning || notes.changed || notes.images.rebuilt {
            REASONING_DROPPED
        } else {
            ""
        };
        format!("{head} {because}: the cache is rebuilt{cost}{reasoning}.")
    }

    pub(super) fn resumed_record(&mut self) -> Option<LogEvent> {
        let resumed = self.resumed.as_ref()?;
        let (gap_ms, truncated_bytes) = (resumed.gap_ms, resumed.truncated_bytes);
        let sections = resumed.sections.clone();
        let instructions = self
            .session
            .instructions
            .clone()
            .filter(|_| resumed.block_written);
        let instructions_source = resumed.source_written.clone();
        let instructions_extra = resumed.extra_written.clone();
        let (endpoint, model) = self.next_names();
        let tools = self.tool_set(&endpoint, &model);
        let whole_copy = resumed.whole_copy
            || resumed
                .logged_tools
                .as_ref()
                .is_some_and(|logged| *logged != tools.digest);
        self.search.logged_tools = Some(tools.digest.clone());
        self.log.as_ref()?;
        let context = &self.connection().context;
        let env = self.tools.as_ref();
        Some(LogEvent::SessionResumed {
            sections,
            wisp_version: env!("CARGO_PKG_VERSION").to_owned(),
            endpoint: context.endpoint.clone(),
            endpoint_name: self.endpoint_name().to_owned(),
            credential_source: Some(context.credential_source.clone()),
            system_prompt_sha256: self.session.system.as_deref().map(sha256_hex),
            tools: tools.names,
            tools_sha256: tools.sha256,
            system_prompt: self.session.system.clone().filter(|_| whole_copy),
            system_base_sha256: self
                .base_sha256()
                .filter(|_| whole_copy && self.session.system.is_some()),
            whole: if whole_copy {
                self.whole_tools()
            } else {
                ToolsCopy::default()
            },
            tools_on: self.seeded_tools().filter(|_| whole_copy),
            instructions_sha256: self.session.instructions.as_deref().map(sha256_hex),
            instructions_source,
            instructions,
            instructions_extra,
            cwd: env.map(|e| e.workspace.shorten(&e.workspace.root)),
            gap_ms,
            truncated_bytes,
            returned: self
                .returned
                .iter()
                .filter(|_| self.parent.is_none())
                .map(|r| r.id.clone())
                .collect(),
        })
    }
}

fn first_call(history: &[Message], calls: &[String]) -> Option<BlockPosition> {
    history.iter().enumerate().find_map(|(message, m)| {
        m.content
            .iter()
            .position(|b| server_call(b).is_some_and(|id| calls.iter().any(|c| c == id)))
            .map(|block| BlockPosition { message, block })
    })
}

fn refreshed_at(logged: &LoggedSession) -> Option<Timestamp> {
    let (turn, at) = logged.refreshed.as_ref()?;
    let step = logged.last_step.as_ref()?;
    (*turn == step.turn).then(|| at.parse().ok()).flatten()
}

fn resumed_billing(
    records: &[LogRecord],
    fell_back: Option<&FellBack>,
) -> Option<wisp_protocol::Billing> {
    [
        crate::resume::billing(records),
        fell_back.and_then(|f| crate::resume::fell_back_billing(&f.event)),
    ]
    .into_iter()
    .flatten()
    .reduce(wisp_protocol::Billing::combined)
}

#[cfg(test)]
mod tests;
