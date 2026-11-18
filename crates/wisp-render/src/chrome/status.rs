use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::mem::discriminant;
use std::time::Duration;

use wisp_protocol::plugin::PluginEntry;
use wisp_protocol::{
    AgentEvent, BlockKind, BusyWait, CompactionTrigger, RebuildReason, Resend, RestartState,
    StopReason, TurnOutcome, TurnStats, Unready, Usage,
};

use super::facts::{Facts, RowFact, Tally};
use super::one_line;
use super::row::{
    GROUP_GAP, KeyHint, ORB_GAP, STOP_ONLY_BELOW, clip, cut_at_word, edge, fit_hints, hints_width,
    pending_text, status_row_keeping,
};
use super::settings::approx;
use super::summary::down_arrow;
use crate::markdown::Layout;
use crate::motion::{
    FRAME_MIN, MotionCtx, MotionPolicy, ORB_CELLS, Orb, OrbFrame, Presence, ms, settles,
};
use crate::styled::{Look, StyledLine, StyledSpan};
use crate::text::{WidthMode, spans_text, spans_width, width};
use crate::theme::glyphs::{GlyphSet, Glyphs};
use crate::theme::{Ink, Token};
use crate::transcript::copy::{self, Fact};
use crate::transcript::{Rebuilds, Said, clean_outcome};

pub(super) const PROCESS_PLUGIN: &str = "shell";
pub(super) const PROCESS_NEWS: &str = "processes";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum State {
    Ready,
    Listening,
    Thinking,
    Answering,
    Working,
    Asking,
    Compacting,
    Done,
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Phase {
    Ready {
        note: Vec<String>,
    },
    Listening,
    Thinking {
        since: Duration,
        retry_until: Option<Duration>,
    },
    Answering,
    Working {
        work: Work,
    },
    Asking {
        since: Duration,
    },
    Compacting {
        since: Duration,
        retry_until: Option<Duration>,
    },
    Done {
        facts: Vec<Fact>,
        output_tokens: Option<u64>,
    },
    Stopped {
        kept: u32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Work {
    Writing { phrase: String, since: Duration },
    Running { doing: String, since: Duration },
}

impl Phase {
    const fn presence(&self) -> Presence {
        match self {
            Phase::Ready { .. } => Presence::Ready,
            Phase::Listening => Presence::Listening,
            Phase::Thinking { .. } => Presence::Thinking,
            Phase::Answering => Presence::Answering { last_chunk: None },
            Phase::Working { .. } => Presence::Working { progress: None },
            Phase::Asking { .. } => Presence::Listening,
            Phase::Compacting { .. } => Presence::Thinking,
            Phase::Done { .. } => Presence::Done,
            Phase::Stopped { .. } => Presence::Stopped,
        }
    }

    const fn running(&self) -> bool {
        matches!(
            self,
            Phase::Thinking { .. }
                | Phase::Answering
                | Phase::Working { .. }
                | Phase::Asking { .. }
                | Phase::Compacting { .. }
        )
    }

    const fn settling(&self) -> bool {
        matches!(self, Phase::Done { .. } | Phase::Stopped { .. })
    }

    fn ready() -> Phase {
        Phase::Ready { note: vec![] }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Settle {
    orb_at: Option<Duration>,
    words_at: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Hint {
    text: String,
    until: Option<Duration>,
    stops: bool,
}

impl Hint {
    fn shows(&self, t: Duration) -> bool {
        self.until.is_none_or(|u| t < u)
    }
}

const RESEND_HINT: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Waiting {
    Retry {
        attempt: u32,
        max: u32,
        cause: &'static str,
        busy: Option<BusyWait>,
    },
    Resend,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct OutputRate {
    from: Option<Duration>,
    last: Option<Duration>,
    tokens: u64,
    elapsed: Duration,
    unavailable: bool,
}

impl OutputRate {
    fn apply(&mut self, event: &AgentEvent, t: Duration) {
        match event {
            AgentEvent::StepStarted { .. } => {
                self.unavailable |= self.from.is_some();
                self.from = Some(t);
                self.last = None;
            }
            AgentEvent::TextDelta { text, .. } | AgentEvent::ThinkingDelta { text, .. }
                if text.is_empty() => {}
            AgentEvent::TextDelta { .. }
            | AgentEvent::ThinkingDelta { .. }
            | AgentEvent::BlockStopped { .. } => {
                if self.from.is_some() {
                    self.last = Some(t);
                }
            }
            AgentEvent::StepEnded { usage, .. } => {
                match (usage, self.from.take(), self.last.take()) {
                    (Some(u), _, _) if u.output_tokens == 0 => {}
                    (Some(u), Some(from), Some(last)) if last > from => {
                        self.tokens = self.tokens.saturating_add(u.output_tokens);
                        self.elapsed = self.elapsed.saturating_add(last.saturating_sub(from));
                    }
                    _ => self.unavailable = true,
                }
            }
            AgentEvent::Retrying { .. } => {
                self.unavailable = true;
                self.from = None;
                self.last = None;
            }
            AgentEvent::TurnEnded { outcome, .. } => {
                self.unavailable |=
                    self.from.take().is_some() || !matches!(outcome, TurnOutcome::Completed { .. });
                self.last = None;
            }
            AgentEvent::SettingsChanged { .. } => {
                *self = Self {
                    unavailable: true,
                    ..Self::default()
                };
            }
            _ => {}
        }
    }

    fn per_second(&self) -> Option<f64> {
        (!self.unavailable && self.tokens > 0 && !self.elapsed.is_zero())
            .then(|| self.tokens as f64 / self.elapsed.as_secs_f64())
    }
}

const SPEED_WINDOW: Duration = Duration::from_secs(3);
const SPEED_KEPT: Duration = Duration::from_secs(4);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Reply {
    began: Option<Duration>,
    recent: VecDeque<(Duration, u64)>,
}

impl Reply {
    fn streamed(&mut self, chars: u64, t: Duration) {
        self.began.get_or_insert(t);
        while self
            .recent
            .front()
            .is_some_and(|(at, _)| t.saturating_sub(*at) >= SPEED_KEPT)
        {
            self.recent.pop_front();
        }
        self.recent.push_back((t, chars));
    }

    fn per_second(&self, tick: Duration) -> Option<f64> {
        let span = tick.checked_sub(self.began?).filter(|s| !s.is_zero())?;
        let from = tick.saturating_sub(SPEED_WINDOW);
        let chars: u64 = self
            .recent
            .iter()
            .filter(|(at, _)| (from..tick).contains(at))
            .map(|(_, n)| n)
            .sum();
        let secs = span
            .clamp(Duration::from_secs(1), SPEED_WINDOW)
            .as_secs_f64();
        Some((chars as f64 / CHARS_PER_TOKEN as f64 / secs).round())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    plan_usage: Vec<crate::plan_usage::UsageCandidate>,
    retry: Option<Waiting>,
    phase: Phase,
    entered: Duration,
    orb: Orb,
    settle: Option<Settle>,
    hint: Option<Hint>,
    composer_empty: bool,
    awaiting: bool,
    turn: bool,
    compacting: bool,
    cache_miss: bool,
    rebuilds: Rebuilds,
    said: Said,
    tally: Tally,
    output_rate: OutputRate,
    targets: BTreeMap<String, String>,
    started: BTreeSet<String>,
    seed: u64,
    turns: u64,
    verb: &'static str,
    since_enter: Option<Duration>,
    streamed_chars: u64,
    reply: Reply,
    effort: Option<String>,
    keys: Vec<KeyHint>,
    background_processes: Vec<u32>,
    process_wakes: bool,
    tail: Option<Duration>,
    step_ended: Option<Duration>,
    restart: Option<(Duration, Vec<Unready>)>,
}

#[derive(Clone, Debug, PartialEq)]
struct Line {
    lead: Vec<StyledSpan>,
    detail: Vec<StyledSpan>,
    tail: Vec<StyledSpan>,
    facts: Vec<String>,
    last_to_go: Option<String>,
    bracketed: bool,
    ends_in_esc: bool,
    yields_to_esc: bool,
    notice: bool,
}

impl Line {
    fn plain(lead: Vec<StyledSpan>) -> Line {
        Line {
            lead,
            detail: vec![],
            tail: vec![],
            facts: vec![],
            last_to_go: None,
            bracketed: false,
            ends_in_esc: false,
            yields_to_esc: false,
            notice: false,
        }
    }

    fn told(whole: String, head: &str, token: Token) -> Line {
        match whole.strip_prefix(head) {
            Some(detail) => Line {
                detail: vec![StyledSpan::new(detail, token)],
                ..Line::plain(vec![StyledSpan::new(head, token)])
            },
            None => Line::plain(vec![StyledSpan::new(whole, token)]),
        }
    }

    fn with_tail(mut self, tail: StyledSpan) -> Line {
        self.tail.push(tail);
        self
    }

    fn cut(self, room: usize, layout: Layout) -> Vec<StyledSpan> {
        let glyphs = &Glyphs::of(layout.glyphs);
        let mode = layout.width_mode;
        let mut line = self.brief(room, glyphs, mode);
        while line.tail.len() > 1 && line.width(glyphs, mode) > room {
            line.tail.pop();
        }
        let tail_w: usize = spans_width(&line.tail, mode);
        let lead_room = room.saturating_sub(tail_w);
        if line.width(glyphs, mode) <= room || lead_room <= width(glyphs.more, mode) {
            return clip(line.spans(glyphs), room, glyphs.more, mode);
        }
        let tail = line.tail.clone();
        let mut out = cut_at_word(
            Line {
                tail: vec![],
                ..line
            }
            .spans(glyphs),
            lead_room,
            layout,
        );
        out.extend(tail);
        out
    }

    fn without_esc(mut self) -> Line {
        if self.ends_in_esc
            && self
                .facts
                .last()
                .is_some_and(|f| *f == copy::esc_to_stop().text())
        {
            self.facts.pop();
            self.ends_in_esc = false;
        }
        self
    }

    fn yielding(mut self) -> Line {
        self.yields_to_esc = true;
        self
    }

    fn facts_text(&self, glyphs: &Glyphs) -> String {
        let sep = format!(" {} ", glyphs.separator);
        if self.facts.is_empty() {
            String::new()
        } else if self.bracketed {
            format!(" {}", self.facts.join(&sep))
        } else {
            format!("{sep}{}", self.facts.join(&sep))
        }
    }

    fn spans(self, glyphs: &Glyphs) -> Vec<StyledSpan> {
        let facts = self.facts_text(glyphs);
        let mut out = self.lead;
        out.extend(self.detail);
        if !facts.is_empty() {
            out.push(muted(facts));
        }
        out.extend(self.tail);
        out
    }

    fn width(&self, glyphs: &Glyphs, mode: WidthMode) -> usize {
        let lead: usize = spans_width(&self.lead, mode) + spans_width(&self.detail, mode);
        let tail: usize = spans_width(&self.tail, mode);
        lead + width(&self.facts_text(glyphs), mode) + tail
    }

    fn brief(mut self, room: usize, glyphs: &Glyphs, mode: WidthMode) -> Line {
        if self.width(glyphs, mode) > room {
            self.detail.clear();
        }
        self
    }

    fn fit(self, room: usize, glyphs: &Glyphs, mode: WidthMode) -> Line {
        let mut line = self.clone();
        while !line.facts.is_empty() && line.width(glyphs, mode) > room {
            let at = line
                .facts
                .iter()
                .rposition(|f| line.last_to_go.as_ref() != Some(f))
                .unwrap_or(line.facts.len().saturating_sub(1));
            line.facts.remove(at);
            line.ends_in_esc = false;
        }
        match self.last_to_go {
            Some(last) if line.facts.is_empty() => {
                let mut rest = Line {
                    last_to_go: None,
                    ..self
                };
                rest.facts.retain(|f| *f != last);
                rest.fit(room, glyphs, mode)
            }
            _ => line,
        }
    }
}

pub const VERB_FROM: Duration = Duration::from_millis(300);
pub const FACTS_FROM: Duration = Duration::from_secs(1);
pub const SHIMMER_STEP: Duration = Duration::from_millis(100);
pub const SHIMMER_PERIOD: Duration = Duration::from_secs(2);
pub const SHIMMER_SWEEP: Duration = Duration::from_millis(1500);
const SHIMMER_HALF_WIDTH: f64 = 2.0;
const SHIMMER_DEPTH: f64 = 0.4;
const CHARS_PER_TOKEN: u64 = 4;

pub const fn shimmers(cx: MotionCtx) -> bool {
    matches!(cx.policy, MotionPolicy::Full) && cx.blend && !cx.ascii
}

pub fn shimmer(text: &str, base: Token, local: Duration, cx: MotionCtx) -> Vec<StyledSpan> {
    if !shimmers(cx) {
        return vec![StyledSpan::new(text, base)];
    }
    let graphemes: Vec<&str> =
        unicode_segmentation::UnicodeSegmentation::graphemes(text, true).collect();
    let step = SHIMMER_STEP.as_millis();
    let period = SHIMMER_PERIOD.as_millis();
    let at = (local.as_millis() / step * step) % period;
    let sweep = SHIMMER_SWEEP.as_millis();
    let travel = graphemes.len() as f64 + 2.0 * SHIMMER_HALF_WIDTH;
    let centre = -SHIMMER_HALF_WIDTH + travel * at.min(sweep) as f64 / sweep as f64;
    let mut out: Vec<StyledSpan> = vec![];
    for (i, g) in graphemes.into_iter().enumerate() {
        let near = 1.0 - (i as f64 - centre).abs() / SHIMMER_HALF_WIDTH;
        let look = if near > 0.0 {
            Look::fg(Ink::mix(Token::Bg, base, near * SHIMMER_DEPTH, base))
        } else {
            Look::fg(Ink::Token(base))
        };
        match out.last_mut() {
            Some(last) if last.look == look => last.text.push_str(g),
            _ => out.push(StyledSpan::new(g, look)),
        }
    }
    out
}

const SCAN_LIMIT: usize = 4096;
pub const SCHEDULE_HORIZON: Duration = Duration::from_secs(3600);
const ONE_MS: Duration = Duration::from_millis(1);

fn done_facts(
    outcome: &TurnOutcome,
    stats: &TurnStats,
    cache_miss: bool,
    rebuilt: Option<RebuildReason>,
) -> Vec<Fact> {
    let mut facts = vec![Fact::plain(copy::duration(stats.duration_ms))];
    let usage = outcome.usage();
    if let Some(reason) = copy::shown_rebuild(rebuilt, cache_miss, usage) {
        facts.push(Fact::plain(copy::cache_rebuilt(&reason)));
    } else if cache_miss {
        facts.push(Fact::plain(copy::CACHE_MISS));
    } else if let Some(pct) = usage.and_then(Usage::cache_percent) {
        facts.push(Fact::plain(copy::cache_rate(pct)));
    }
    facts
}

fn ended_note(stop_reason: &StopReason, committed: bool) -> Vec<String> {
    let mut parts: Vec<String> = copy::stop_words(stop_reason).into_iter().collect();
    if !committed {
        parts.push(copy::NOT_KEPT.to_owned());
    }
    if let Some(first) = parts.first_mut() {
        *first = copy::sentence_case(first);
    }
    parts
}

fn muted(text: impl Into<String>) -> StyledSpan {
    StyledSpan::new(text, Token::TextMuted)
}

impl Default for Status {
    fn default() -> Status {
        Status::new()
    }
}

impl Status {
    pub fn new() -> Status {
        Status {
            retry: None,
            phase: Phase::ready(),
            entered: Duration::ZERO,
            orb: Orb::new(Presence::Ready),
            settle: None,
            hint: None,
            composer_empty: true,
            awaiting: false,
            turn: false,
            compacting: false,
            cache_miss: false,
            rebuilds: Rebuilds::default(),
            said: Said::default(),
            tally: Tally::default(),
            output_rate: OutputRate::default(),
            targets: BTreeMap::new(),
            started: BTreeSet::new(),
            seed: 0,
            turns: 0,
            verb: copy::verb(0, 0),
            since_enter: None,
            streamed_chars: 0,
            reply: Reply::default(),
            effort: None,
            keys: Vec::new(),
            background_processes: Vec::new(),
            process_wakes: true,
            tail: None,
            step_ended: None,
            plan_usage: Vec::new(),
            restart: None,
        }
    }

    pub fn with_tail(mut self, tail: Duration) -> Status {
        self.tail = Some(tail);
        self
    }

    pub fn with_seed(mut self, seed: u64) -> Status {
        self.seed = seed;
        self
    }

    pub fn turn_effort(&mut self, effort: Option<&str>) {
        self.effort = effort.map(one_line).filter(|e| !e.is_empty());
    }

    pub fn keys(&mut self, keys: &[KeyHint]) {
        self.keys = keys
            .iter()
            .map(|k| k.map(one_line))
            .filter(|k| !k.text().is_empty())
            .collect();
    }

    pub fn background_processes(&mut self, ids: Vec<u32>) {
        self.background_processes = ids;
    }

    fn process_catalog(&mut self, plugins: &[PluginEntry]) {
        self.process_wakes = plugins
            .iter()
            .find(|p| p.id == PROCESS_PLUGIN)
            .is_none_or(|p| {
                p.installed
                    && p.enabled
                    && p.hooks
                        .iter()
                        .filter(|h| h.hook == PROCESS_NEWS)
                        .all(|h| h.on)
            });
    }

    pub fn verb(&self) -> &'static str {
        self.verb
    }

    pub fn estimated_tokens(&self) -> u64 {
        self.streamed_chars.div_ceil(CHARS_PER_TOKEN)
    }

    fn begin_turn(&mut self, t: Duration) {
        self.verb = copy::verb(self.seed, self.turns);
        self.turns = self.turns.saturating_add(1);
        self.since_enter = Some(t);
        self.streamed_chars = 0;
        self.output_rate = OutputRate::default();
    }

    fn end_turn(&mut self) {
        self.since_enter = None;
        self.streamed_chars = 0;
    }

    fn streamed(&mut self, text: &str, t: Duration) {
        let n = u64::try_from(text.chars().count()).unwrap_or(u64::MAX);
        if self.is_running() && n > 0 {
            self.streamed_chars = self.streamed_chars.saturating_add(n);
            self.reply.streamed(n, t);
        }
    }

    pub fn tally(&self) -> &Tally {
        &self.tally
    }

    pub fn name_call(&mut self, call_id: &str, doing: &str) {
        self.targets.insert(call_id.to_owned(), one_line(doing));
    }

    pub fn replayed(&mut self, event: &AgentEvent) {
        self.rebuilds.apply(event);
        match event {
            AgentEvent::TurnEnded { outcome, .. } => {
                self.tally.add(&clean_outcome(outcome, Said::default()))
            }
            AgentEvent::ContextChanged { .. } => self.tally.changed(event),
            AgentEvent::TodosUpdated { items, .. } => self.tally.todos(items),
            _ => {}
        }
    }

    pub fn is_restarting(&self) -> bool {
        self.restart.is_some()
    }

    pub fn is_running(&self) -> bool {
        self.awaiting || self.turn || self.compacting
    }

    pub fn is_compacting(&self) -> bool {
        self.compacting
    }

    pub fn is_awaiting(&self) -> bool {
        self.awaiting && !self.turn
    }

    pub fn stop_awaiting(&mut self, t: Duration, cx: MotionCtx) {
        if self.is_awaiting() {
            self.expire(t, cx);
            self.awaiting = false;
            self.end_turn();
            let idle = self.idle();
            self.go(idle, t, cx);
        }
    }

    fn idle_presence(&self) -> Presence {
        if self.composer_empty {
            Presence::Ready
        } else {
            Presence::Listening
        }
    }

    fn orb_settled(&self, t: Duration) -> bool {
        self.settle
            .is_some_and(|s| s.orb_at.is_none_or(|at| t >= at))
    }

    fn fix_settled_orb(&mut self, t: Duration, cx: MotionCtx) {
        if self.settled(t).is_some() {
            let (orb, since) = self.live_orb(t, cx);
            self.orb = orb;
            self.entered = since;
            if let Some(s) = &mut self.settle {
                s.orb_at = None;
            }
        }
    }

    fn settled(&self, t: Duration) -> Option<Duration> {
        self.settle.and_then(|s| s.orb_at).filter(|at| t >= *at)
    }

    fn clamp(&self, local: Duration, cx: MotionCtx) -> Duration {
        match settles(self.orb.state(), cx) {
            Some(own) if self.phase.settling() => local.min(own.saturating_sub(ONE_MS)),
            _ => local,
        }
    }

    fn live_orb(&self, t: Duration, cx: MotionCtx) -> (Orb, Duration) {
        match self.settled(t) {
            Some(at) => {
                let last = self.clamp(at.saturating_sub(self.entered).saturating_sub(ONE_MS), cx);
                (self.orb.change(self.idle_presence(), last, cx), at)
            }
            None => (self.orb, self.entered),
        }
    }

    fn go(&mut self, phase: Phase, t: Duration, cx: MotionCtx) {
        let to = phase.presence();
        let (orb, since) = self.live_orb(t, cx);
        if discriminant(&to) != discriminant(&orb.state()) {
            let local = if self.settled(t).is_some() {
                t.saturating_sub(since)
            } else {
                self.clamp(t.saturating_sub(since), cx)
            };
            self.orb = orb.change(to, local, cx);
            self.entered = t;
        } else {
            self.orb = orb;
            self.entered = since;
        }
        if !matches!(phase, Phase::Answering) {
            self.reply = Reply::default();
        }
        self.settle = phase.settling().then(|| {
            let latch = |d: Option<Duration>| t.saturating_add(d.unwrap_or_default());
            Settle {
                orb_at: Some(latch(settles(to, cx))),
                words_at: latch(settles(Presence::Done, cx)),
            }
        });
        self.phase = phase;
    }

    fn idle(&self) -> Phase {
        if self.composer_empty {
            Phase::ready()
        } else {
            Phase::Listening
        }
    }

    fn expire(&mut self, t: Duration, cx: MotionCtx) {
        if self.settle.is_some_and(|s| t >= s.words_at) {
            let idle = self.idle();
            self.go(idle, t, cx);
        }
        if self
            .hint
            .as_ref()
            .is_some_and(|h| h.until.is_some_and(|u| t >= u))
        {
            self.hint = None;
        }
    }

    fn think(&mut self, t: Duration, cx: MotionCtx, retry_until: Option<Duration>) {
        let since = match self.phase {
            Phase::Thinking { since, .. } => since,
            _ => t,
        };
        self.go(Phase::Thinking { since, retry_until }, t, cx);
    }

    fn compaction_started(
        &mut self,
        trigger: CompactionTrigger,
        turn: bool,
        t: Duration,
        cx: MotionCtx,
    ) {
        if trigger == CompactionTrigger::Background {
            return;
        }
        if !turn && !self.turn {
            self.compacting = true;
            self.awaiting = false;
            self.since_enter = Some(t);
            self.streamed_chars = 0;
        }
        self.go(
            Phase::Compacting {
                since: t,
                retry_until: None,
            },
            t,
            cx,
        );
    }

    fn compaction_ended(&mut self, usage: Option<&Usage>, t: Duration, cx: MotionCtx) {
        if !matches!(self.phase, Phase::Compacting { .. }) && !self.compacting {
            return;
        }
        if self.compacting && !self.turn {
            self.compacting = false;
            self.tally.settle(usage);
            self.end_turn();
            let idle = self.idle();
            self.go(idle, t, cx);
        } else if self.turn {
            self.think(t, cx, None);
        }
    }

    pub fn hint(&mut self, text: &str, t: Duration, lasts: Option<Duration>) {
        self.hint = Some(Hint {
            text: one_line(text),
            until: lasts.map(|l| t.saturating_add(l.min(SCHEDULE_HORIZON))),
            stops: false,
        });
    }

    pub fn hint_naming_stop(&mut self, text: &str, t: Duration, lasts: Option<Duration>) {
        self.hint(text, t, lasts);
        if let Some(hint) = &mut self.hint {
            hint.stops = true;
        }
    }

    pub fn clear_hint(&mut self) {
        self.hint = None;
    }

    pub fn shown_hint(&self, t: Duration) -> Option<&str> {
        self.hint
            .as_ref()
            .filter(|h| h.shows(t))
            .map(|h| h.text.as_str())
    }

    pub fn submitted(&mut self, t: Duration, cx: MotionCtx) {
        self.expire(t, cx);
        self.awaiting = true;
        self.composer_empty = true;
        self.cache_miss = false;
        self.begin_turn(t);
        self.go(
            Phase::Thinking {
                since: t,
                retry_until: None,
            },
            t,
            cx,
        );
    }

    pub fn composer(&mut self, empty: bool, t: Duration, cx: MotionCtx) {
        self.expire(t, cx);
        self.fix_settled_orb(t, cx);
        self.composer_empty = empty;
        if self.is_running() {
            return;
        }
        match (&self.phase, empty) {
            (Phase::Listening, true) => self.go(Phase::ready(), t, cx),
            (Phase::Ready { .. } | Phase::Done { .. } | Phase::Stopped { .. }, false) => {
                self.go(Phase::Listening, t, cx)
            }
            _ => {}
        }
    }

    pub fn apply(&mut self, event: &AgentEvent, t: Duration, cx: MotionCtx) {
        self.expire(t, cx);
        self.output_rate.apply(event, t);
        self.rebuilds.apply(event);
        self.said.apply(event);
        match event {
            AgentEvent::CatalogChanged { catalog } => self.process_catalog(&catalog.plugins),
            AgentEvent::SettingsSnapshot(snapshot) => self.process_catalog(&snapshot.plugins),
            AgentEvent::Restart {
                state,
                open: unfinished,
            } => {
                self.restart = (*state != RestartState::Cancelled).then(|| (t, unfinished.clone()));
            }
            AgentEvent::TurnStarted { .. } => {
                if self.since_enter.is_none() {
                    self.begin_turn(t);
                }
                self.awaiting = false;
                self.turn = true;
                self.compacting = false;
                self.cache_miss = false;
                self.targets.clear();
                self.started.clear();
                if !self.phase.running() {
                    self.think(t, cx, None);
                }
            }
            AgentEvent::Retrying {
                delay_ms,
                attempt,
                max_attempts,
                kind,
                detail,
                resend,
                busy,
                ..
            } => {
                self.retry = Some(match resend {
                    Some(Resend::BreakpointLimit) => Waiting::Resend,
                    None => Waiting::Retry {
                        attempt: *attempt,
                        max: *max_attempts,
                        cause: copy::retry_cause(*kind, detail.as_ref().map(|d| d.phase)),
                        busy: *busy,
                    },
                });
                if resend.is_some() {
                    self.hint(&copy::resend_status(), t, Some(RESEND_HINT));
                }
                let until = t.saturating_add(Duration::from_millis(*delay_ms));
                match self.phase {
                    Phase::Compacting { since, .. } => self.go(
                        Phase::Compacting {
                            since,
                            retry_until: Some(until),
                        },
                        t,
                        cx,
                    ),
                    _ => self.think(t, cx, Some(until)),
                }
            }
            AgentEvent::CompactionStarted { trigger, turn, .. } => {
                self.compaction_started(*trigger, turn.is_some(), t, cx);
            }
            AgentEvent::Compacted { trigger, usage, .. }
            | AgentEvent::CompactionFailed { trigger, usage, .. }
                if *trigger != CompactionTrigger::Background =>
            {
                self.compaction_ended(usage.as_ref(), t, cx);
            }
            AgentEvent::BlockStarted {
                kind: BlockKind::Thinking | BlockKind::RedactedThinking,
                ..
            } => self.think(t, cx, None),
            AgentEvent::ThinkingDelta { .. } => self.think(t, cx, None),
            AgentEvent::BlockStarted {
                kind: BlockKind::Text,
                ..
            } => self.go(Phase::Answering, t, cx),
            AgentEvent::TextDelta { text, .. } => {
                self.streamed(text, t);
                self.go(Phase::Answering, t, cx);
                self.orb = self.orb.chunk(t.saturating_sub(self.entered));
            }
            AgentEvent::BlockStarted {
                kind: BlockKind::ToolUse { name, writing },
                ..
            } => {
                let name = one_line(name);
                let phrase = writing
                    .as_deref()
                    .map(one_line)
                    .filter(|w| !w.trim().is_empty())
                    .unwrap_or_else(|| copy::writing_call(&name));
                self.go(
                    Phase::Working {
                        work: Work::Writing { phrase, since: t },
                    },
                    t,
                    cx,
                );
            }
            AgentEvent::StepStarted { step } if *step > 1 && self.turn => self.think(t, cx, None),
            AgentEvent::ContextChanged { .. } => self.tally.changed(event),
            AgentEvent::StepEnded { .. } => {
                self.step_ended = Some(t);
                self.tally.step();
                if self.turn
                    && matches!(
                        self.phase,
                        Phase::Working {
                            work: Work::Writing { .. }
                        }
                    )
                {
                    self.think(t, cx, None);
                }
            }
            AgentEvent::ToolCallStarted { call_id, .. } if self.turn => {
                let doing = self.targets.get(call_id).cloned().unwrap_or_default();
                self.started.insert(call_id.clone());
                self.go(
                    Phase::Working {
                        work: Work::Running { doing, since: t },
                    },
                    t,
                    cx,
                );
            }
            AgentEvent::QuestionAsked { .. } if self.turn => {
                self.go(Phase::Asking { since: t }, t, cx);
            }
            AgentEvent::QuestionClosed { .. } => {
                if matches!(self.phase, Phase::Asking { .. }) {
                    self.think(t, cx, None);
                }
            }
            AgentEvent::ToolCallFinished { call_id, .. } => {
                self.targets.remove(call_id);
                if self.started.remove(call_id) && self.started.is_empty() && self.turn {
                    self.think(t, cx, None);
                }
            }
            AgentEvent::CacheMiss { .. } => self.cache_miss = true,
            AgentEvent::TurnEnded { outcome, stats } => {
                let outcome = clean_outcome(outcome, self.said);
                self.awaiting = false;
                self.turn = false;
                self.tally.add(&outcome);
                let phase = match &outcome {
                    TurnOutcome::Completed {
                        stop_reason: StopReason::EndTurn,
                        committed: true,
                        ..
                    } => Phase::Done {
                        facts: done_facts(&outcome, stats, self.cache_miss, self.rebuilds.turn()),
                        output_tokens: outcome.usage().map(|u| u.output_tokens),
                    },
                    TurnOutcome::Completed {
                        stop_reason,
                        committed,
                        ..
                    } if self.composer_empty => Phase::Ready {
                        note: ended_note(stop_reason, *committed),
                    },
                    TurnOutcome::Completed { .. } => Phase::Listening,
                    TurnOutcome::Cancelled {
                        committed_steps, ..
                    } => Phase::Stopped {
                        kept: *committed_steps,
                    },
                    TurnOutcome::Error { .. } => self.idle(),
                };
                self.cache_miss = false;
                self.targets.clear();
                self.started.clear();
                self.end_turn();
                self.go(phase, t, cx);
            }
            AgentEvent::TodosUpdated { items, .. } => self.tally.todos(items),
            _ => {}
        }
    }

    pub fn state(&self, t: Duration, _cx: MotionCtx) -> State {
        let settled = self.orb_settled(t);
        match &self.phase {
            Phase::Ready { .. } => State::Ready,
            Phase::Listening => State::Listening,
            Phase::Thinking { .. } => State::Thinking,
            Phase::Answering => State::Answering,
            Phase::Working { .. } => State::Working,
            Phase::Asking { .. } => State::Asking,
            Phase::Compacting { .. } => State::Compacting,
            Phase::Done { .. } | Phase::Stopped { .. } if settled && !self.composer_empty => {
                State::Listening
            }
            Phase::Done { .. } | Phase::Stopped { .. } if settled => State::Ready,
            Phase::Done { .. } => State::Done,
            Phase::Stopped { .. } => State::Stopped,
        }
    }

    pub fn orb(&self, t: Duration, cx: MotionCtx) -> OrbFrame {
        let (orb, since) = self.live_orb(t, cx);
        let local = t.saturating_sub(since);
        let local = if self.settled(t).is_some() {
            local
        } else {
            self.clamp(local, cx)
        };
        orb.frame(local, cx)
    }

    pub fn resting(&self, t: Duration, cx: MotionCtx) -> Option<Duration> {
        let (orb, since) = self.live_orb(t, cx);
        let ready = orb.state() == Presence::Ready && !self.is_running();
        let hinted = self.shown_hint(t).is_some();
        (ready && !hinted).then(|| t.saturating_sub(since))
    }

    fn turn_since(&self) -> Option<Duration> {
        let phase = match &self.phase {
            Phase::Thinking { since, .. } | Phase::Compacting { since, .. } => *since,
            Phase::Answering => self.entered,
            _ => return None,
        };
        Some(self.since_enter.unwrap_or(phase))
    }

    fn turn_time(&self, t: Duration) -> Duration {
        self.turn_since()
            .map_or(Duration::ZERO, |since| t.saturating_sub(since))
    }

    fn working(&self, t: Duration, cx: MotionCtx, glyphs: &Glyphs, answering: bool) -> Line {
        let elapsed = self.turn_time(t);
        if elapsed < VERB_FROM {
            return Line::plain(vec![]);
        }
        let base = if answering {
            Token::Text
        } else {
            Token::Thinking
        };
        let word = format!("{}{}", self.verb, glyphs.more);
        let lead = shimmer(&word, base, t.saturating_sub(self.entered), cx);
        if elapsed < FACTS_FROM {
            return Line::plain(lead).yielding();
        }
        let mut facts = vec![copy::clock(elapsed.as_secs())];
        let tokens = self.estimated_tokens();
        if tokens > 0 {
            facts.push(copy::tokens_down(
                down_arrow(glyphs),
                approx(glyphs.set),
                tokens,
            ));
        }
        let tick = t.saturating_sub(Duration::from_nanos(u64::from(elapsed.subsec_nanos())));
        let speed = self
            .reply
            .per_second(tick)
            .map(|rate| copy::tokens_per_second(approx(glyphs.set), rate));
        facts.extend(speed.clone());
        facts.push(match (answering, &self.effort) {
            (true, _) => copy::esc_to_stop().text(),
            (false, Some(effort)) => copy::thinking_with(effort),
            (false, None) => copy::THINKING_PHASE.to_owned(),
        });
        Line {
            notice: false,
            lead,
            detail: vec![],
            tail: vec![],
            facts,
            last_to_go: speed,
            bracketed: true,
            ends_in_esc: answering,
            yields_to_esc: true,
        }
    }

    fn compacting_line(&self, t: Duration, cx: MotionCtx, glyphs: &Glyphs) -> Line {
        let elapsed = self.turn_time(t);
        if elapsed < VERB_FROM {
            return Line::plain(vec![]);
        }
        let word = format!("{}{}", copy::COMPACTING, glyphs.more);
        let lead = shimmer(&word, Token::Thinking, t.saturating_sub(self.entered), cx);
        if elapsed < FACTS_FROM {
            return Line::plain(lead).yielding();
        }
        Line {
            notice: false,
            lead,
            detail: vec![],
            tail: vec![],
            facts: vec![copy::clock(elapsed.as_secs()), copy::esc_to_stop().text()],
            last_to_go: None,
            bracketed: true,
            ends_in_esc: true,
            yields_to_esc: true,
        }
    }

    fn retry_line(&self, t: Duration, until: Duration, sep: &str) -> Line {
        let secs = until.saturating_sub(t).as_millis().div_ceil(1000);
        let secs = u64::try_from(secs).unwrap_or(u64::MAX);
        let text = match self.retry {
            Some(Waiting::Resend) => return Line::plain(vec![muted(copy::resend_status())]),
            Some(Waiting::Retry {
                attempt,
                max,
                cause,
                busy: None,
            }) => copy::retrying_in(secs, attempt, max, cause),
            Some(Waiting::Retry {
                attempt,
                max,
                cause,
                busy: Some(busy),
            }) => format!(
                "{}{sep}{}",
                copy::retrying_in(secs, attempt, max, cause),
                copy::busy_waited(busy)
            ),
            None => copy::retrying_in(secs, 0, 0, ""),
        };
        Line::told(text, &copy::retrying_head(secs), Token::TextMuted)
    }

    fn line(&self, t: Duration, cx: MotionCtx, glyphs: &Glyphs) -> Line {
        let sep = format!(" {} ", glyphs.separator);
        if let Some(h) = self.hint.as_ref().filter(|h| h.shows(t)) {
            return Line {
                ends_in_esc: h.stops,
                notice: true,
                ..Line::plain(vec![muted(h.text.clone())])
            };
        }
        let ready = || {
            let ids = if self.process_wakes {
                self.background_processes.as_slice()
            } else {
                &[]
            };
            Line::plain(vec![muted(copy::ready_with_processes(
                ids,
                glyphs.separator,
            ))])
        };
        let held = self.settle.is_some_and(|s| t < s.words_at);
        match &self.phase {
            Phase::Ready { note } if note.is_empty() => ready(),
            Phase::Ready { note } => Line::plain(vec![muted(note.join(&sep))]),
            Phase::Listening => Line::plain(vec![]),
            Phase::Thinking { retry_until, .. } => match retry_until {
                Some(until) if t < *until => self.retry_line(t, *until, &sep),
                _ => self.working(t, cx, glyphs, false),
            },
            Phase::Compacting { retry_until, .. } => match retry_until {
                Some(until) if t < *until => self.retry_line(t, *until, &sep),
                _ => self.compacting_line(t, cx, glyphs),
            },
            Phase::Answering => self.working(t, cx, glyphs, true),
            Phase::Working {
                work: Work::Writing { phrase, since },
            } => {
                let secs = t.saturating_sub(*since).as_secs();
                Line::told(copy::writing(phrase), copy::WRITING, Token::TextMuted)
                    .with_tail(muted(format!("{sep}{}", copy::clock(secs))))
            }
            Phase::Working {
                work: Work::Running { doing, since },
            } => {
                let secs = t.saturating_sub(*since).as_secs();
                let text = if doing.is_empty() {
                    copy::WORKING.to_owned()
                } else {
                    doing.clone()
                };
                Line::plain(vec![muted(text)])
                    .with_tail(muted(format!("{sep}{}", copy::clock(secs))))
            }
            Phase::Asking { since, .. } => {
                let secs = t.saturating_sub(*since).as_secs();
                let line =
                    Line::told(copy::waiting_for_answer(), copy::WAITING, Token::Text).with_tail(
                        StyledSpan::new(format!("{sep}{}", copy::clock(secs)), Token::Text),
                    );
                match self.cache_note(t) {
                    Some(note) => {
                        line.with_tail(StyledSpan::new(format!("{sep}{note}"), Token::Text))
                    }
                    None => line,
                }
            }
            Phase::Done {
                facts,
                output_tokens,
            } if held => {
                let mut said = vec![];
                for (i, f) in facts.iter().enumerate() {
                    said.push(f.text());
                    if let Some(n) = output_tokens.filter(|_| i == 0) {
                        said.push(copy::tokens_down(down_arrow(glyphs), "", n));
                    }
                }
                Line {
                    notice: false,
                    lead: vec![StyledSpan::new(copy::DONE, Token::Ok)],
                    detail: vec![],
                    tail: vec![],
                    facts: said,
                    last_to_go: None,
                    bracketed: false,
                    ends_in_esc: false,
                    yields_to_esc: false,
                }
            }
            Phase::Stopped { kept } if held => {
                Line::told(copy::stopped(*kept), copy::STOPPED, Token::TextMuted)
            }
            Phase::Done { .. } | Phase::Stopped { .. } if self.composer_empty => ready(),
            Phase::Done { .. } | Phase::Stopped { .. } => Line::plain(vec![]),
        }
    }

    fn expiry(&self) -> Option<(Duration, Duration)> {
        let tail = self.tail?;
        Some((self.step_ended?.saturating_add(tail), tail))
    }

    pub fn cache_note(&self, t: Duration) -> Option<String> {
        let (expires, tail) = self.expiry()?;
        let left = expires.saturating_sub(t);
        if left >= tail / 6 {
            return None;
        }
        if left.is_zero() {
            return Some(copy::CACHE_EXPIRED_NOW.to_owned());
        }
        let minutes = left.as_millis().div_ceil(60_000);
        Some(copy::cache_expires_in(
            u64::try_from(minutes).unwrap_or(u64::MAX),
        ))
    }

    fn cache_changes(&self, t: Duration) -> Vec<Duration> {
        let Some((expires, tail)) = self.expiry() else {
            return vec![];
        };
        let mut out = vec![
            expires.saturating_sub(tail / 6).saturating_add(ONE_MS),
            expires,
        ];
        let left = expires.saturating_sub(t);
        if !left.is_zero() {
            let minute = Duration::from_secs(60);
            let whole =
                u32::try_from(left.saturating_sub(ONE_MS).as_millis() / 60_000).unwrap_or(u32::MAX);
            out.push(expires.saturating_sub(minute.saturating_mul(whole)));
        }
        out
    }

    pub fn sentence(&self, t: Duration, cx: MotionCtx, glyphs: &Glyphs) -> Vec<StyledSpan> {
        self.line(t, cx, glyphs).spans(glyphs)
    }

    pub fn sentence_stopless(
        &self,
        t: Duration,
        cx: MotionCtx,
        glyphs: &Glyphs,
    ) -> Vec<StyledSpan> {
        let mut line = self.line(t, cx, glyphs);
        if line.ends_in_esc && !line.facts.is_empty() {
            line.facts.pop();
        }
        line.spans(glyphs)
    }

    pub fn core(&self, t: Duration, cx: MotionCtx) -> Ink {
        self.orb(t, cx).core()
    }

    pub fn usage_candidates(&mut self, candidates: Vec<crate::plan_usage::UsageCandidate>) -> bool {
        if self.plan_usage == candidates {
            return false;
        }
        self.plan_usage = candidates;
        true
    }

    pub fn row(&self, t: Duration, cx: MotionCtx, facts: &Facts, layout: Layout) -> StyledLine {
        let glyphs = Glyphs::of(layout.glyphs);
        let mode = layout.width_mode;
        let room = edge(layout).saturating_sub(1 + ORB_CELLS + ORB_GAP);
        if self.shown_hint(t).is_none()
            && let Some((since, unfinished)) = &self.restart
        {
            let mut unfinished = unfinished.clone();
            wisp_protocol::age_unready(&mut unfinished, t.saturating_sub(*since));
            let right = self.tally.facts(facts, &glyphs, &self.plan_usage, None);
            let sentence = super::restart::window_phrase(&unfinished, room, layout);
            return super::row::status_row(&self.orb(t, cx), sentence, &[], &right, layout);
        }
        let running = self.is_running();
        let mut hints = self.keys.clone();
        let binds_exit = |h: &KeyHint| h.key() == copy::ESC_KEY || h.is_exit();
        let esc_bound = hints.iter().any(binds_exit);
        let initial = self.line(t, cx, &glyphs);
        let full = match initial {
            line if running && esc_bound && !line.notice => line.without_esc(),
            line => line,
        }
        .brief(room, &glyphs, mode);
        let mut right = self.tally.facts(
            facts,
            &glyphs,
            &self.plan_usage,
            self.output_rate.per_second().filter(|_| !running),
        );
        let ready = !running
            && full.facts.is_empty()
            && full.tail.is_empty()
            && spans_text(&full.lead) == copy::READY;
        let continuation = format!(
            "{} {} {}",
            copy::READY,
            glyphs.separator,
            copy::AGENT_CONTINUES_WHEN
        );
        let mut keep = None;
        let sentence = if !running
            && !full.notice
            && spans_text(&full.lead).starts_with(&continuation)
        {
            let process_hints = match hints.as_slice() {
                [.., running, ps] if running.does().is_empty() && ps.key() == copy::PS_HINT => {
                    hints.split_off(hints.len() - 2)
                }
                _ => Vec::new(),
            };
            keep = hints.iter().find(|hint| binds_exit(hint)).cloned();
            fit_hints(&mut hints, keep.as_ref(), |keys| {
                hints_width(keys, &glyphs, mode) <= room.saturating_sub(width(copy::READY, mode))
            });
            let room = room.saturating_sub(hints_width(&hints, &glyphs, mode));
            hints.extend(process_hints);
            if room >= width(&continuation, mode) + width(glyphs.more, mode) {
                clip(full.spans(&glyphs), room, glyphs.more, mode)
            } else {
                vec![muted(copy::READY)]
            }
        } else {
            let reserve = if layout.width < STOP_ONLY_BELOW && running {
                0
            } else {
                let named: Vec<usize> = right
                    .iter()
                    .filter_map(|fact| match fact {
                        RowFact::Chip(chip) => Some(chip.width(usize::MAX, &glyphs, mode)),
                        RowFact::Pending(chip) => {
                            Some(width(&pending_text(chip, false, &glyphs), mode))
                        }
                        RowFact::Credential(label) => {
                            Some(width(&label.without_host(&glyphs), mode))
                        }
                        _ => None,
                    })
                    .map(|w| w + GROUP_GAP)
                    .collect();
                let all: usize = named.iter().sum();
                let stop_w = if running {
                    let stop = hints
                        .iter()
                        .find(|h| binds_exit(h))
                        .map_or_else(|| copy::esc_to_stop().text(), KeyHint::text);
                    width(&stop, mode) + GROUP_GAP
                } else {
                    0
                };
                if all + stop_w + spans_width(&full.lead, mode) + spans_width(&full.detail, mode)
                    <= room
                {
                    all
                } else {
                    named.last().copied().unwrap_or(0)
                }
            };
            let line = full
                .clone()
                .fit(room.saturating_sub(reserve), &glyphs, mode);
            let stop = match hints.iter().position(binds_exit) {
                Some(at) if !running && hints[at].is_exit() => Some(hints.remove(at)),
                _ if !running || line.ends_in_esc => None,
                Some(at) => Some(hints.remove(at)),
                None => Some(copy::esc_to_stop()),
            };
            if running && layout.width < STOP_ONLY_BELOW {
                hints.clear();
                right.clear();
            }
            let after_note = usize::from(hints.first().is_some_and(|h| h.does().is_empty()));
            match stop {
                None => line.spans(&glyphs),
                Some(stop) => {
                    let stop_w = width(&stop.text(), mode);
                    let short = room.saturating_sub(GROUP_GAP + stop_w + reserve);
                    if line.notice {
                        hints.insert(after_note.min(hints.len()), stop.clone());
                        keep = Some(stop);
                        line.spans(&glyphs)
                    } else if short > width(glyphs.more, mode) {
                        hints.insert(after_note.min(hints.len()), stop.clone());
                        keep = Some(stop);
                        if line.yields_to_esc {
                            let spans = full.fit(short, &glyphs, mode).spans(&glyphs);
                            cut_at_word(spans, short, layout)
                        } else {
                            full.cut(short, layout)
                        }
                    } else {
                        hints.clear();
                        right.clear();
                        if room >= stop_w { stop.spans() } else { vec![] }
                    }
                }
            }
        };
        status_row_keeping(
            &self.orb(t, cx),
            sentence,
            &hints,
            keep.as_ref(),
            &right,
            ready,
            layout,
        )
    }

    fn candidates(&self, t: Duration, cx: MotionCtx) -> Option<Duration> {
        let mut times: Vec<Duration> = vec![];
        let (orb, since) = self.live_orb(t, cx);
        times.extend(
            orb.next_change(t.saturating_sub(since), cx)
                .map(|n| n.saturating_add(since)),
        );
        if let Some(s) = self.settle {
            times.extend(s.orb_at);
            times.push(s.words_at);
        }
        if let Some(h) = &self.hint {
            times.extend(h.until);
        }
        if let Phase::Working {
            work: Work::Running { since, .. } | Work::Writing { since, .. },
        }
        | Phase::Asking { since, .. } = &self.phase
        {
            let secs = t.saturating_sub(*since).as_secs().saturating_add(1);
            times.push(since.saturating_add(Duration::from_secs(secs)));
        }
        if matches!(self.phase, Phase::Asking { .. }) {
            times.extend(self.cache_changes(t));
        }
        let working_since = match &self.phase {
            Phase::Thinking {
                retry_until: Some(until),
                ..
            }
            | Phase::Compacting {
                retry_until: Some(until),
                ..
            } if t < *until => {
                let left = until.saturating_sub(t).as_millis();
                let whole = left.div_ceil(1000).saturating_sub(1);
                let step = u64::try_from(whole * 1000).unwrap_or(u64::MAX);
                times.push(until.saturating_sub(Duration::from_millis(step)));
                None
            }
            _ => self.turn_since(),
        };
        if let Some(since) = working_since {
            let elapsed = t.saturating_sub(since);
            let next = if elapsed < VERB_FROM {
                VERB_FROM
            } else if elapsed < FACTS_FROM {
                FACTS_FROM
            } else {
                Duration::from_secs(elapsed.as_secs().saturating_add(1))
            };
            times.push(since.saturating_add(next));
            if shimmers(cx) && elapsed >= VERB_FROM {
                let step = ms(SHIMMER_STEP);
                let local = ms(t.saturating_sub(self.entered));
                let grid = (local / step).saturating_add(1).saturating_mul(step);
                times.push(self.entered.saturating_add(Duration::from_millis(grid)));
            }
        }
        times.into_iter().filter(|n| *n > t).min()
    }

    fn view(&self, t: Duration, cx: MotionCtx) -> (OrbFrame, Vec<StyledSpan>) {
        (
            self.orb(t, cx),
            self.sentence(t, cx, &Glyphs::of(GlyphSet::Unicode)),
        )
    }

    pub fn next_change(&self, t: Duration, cx: MotionCtx) -> Option<Duration> {
        if let Some((since, _)) = self.restart {
            let second = since.saturating_add(Duration::from_secs(
                t.saturating_sub(since).as_secs().saturating_add(1),
            ));
            return self
                .candidates(t, cx)
                .into_iter()
                .chain([second])
                .filter(|next| *next > t)
                .min();
        }
        let now = self.view(t, cx);
        let mut at = t;
        for _ in 0..SCAN_LIMIT {
            let next = self.candidates(at, cx)?;
            if next > t.saturating_add(SCHEDULE_HORIZON) {
                return None;
            }
            if self.view(next, cx) != now {
                return Some(next.max(t.saturating_add(FRAME_MIN)));
            }
            at = next;
        }
        None
    }
}
