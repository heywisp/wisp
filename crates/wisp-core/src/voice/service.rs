use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::time::Instant;
use wisp_protocol::settings::{SettingValue, SettingsSnapshot, VoiceUnset, names};
use wisp_protocol::voice::{AudioFrame, CallEnd, CallId, CallOutcome, VoiceCommand, VoiceEvent};
use wisp_protocol::{
    AccountSpend, AgentEvent, BlockKind, ClientCommand, ErrorKind, InputId, RequestId, StopReason,
    ToolOutcome, TurnOutcome, Usage,
};
use wisp_tool_text::output::cut_chars;
use wisp_tool_text::voice::{self as words, line};

use super::earlier::{Earlier, HANDED, TAP};
use super::{Control, Leg, LegAudio, Legs};
use crate::agents::PresetPlace;
use crate::kit::{Connect, ROOT};
use crate::provider::ProviderError;
use crate::provider::voice::{LiveEvent, LiveSay, OpenCall, Opened, Spent, VoiceRoute};

pub const QUIET: Duration = Duration::from_secs(120);
pub(super) const HOLD: Duration = Duration::from_millis(500);
pub(super) const REPLY_WAIT: Duration = Duration::from_secs(3);
pub(super) const STEADY: Duration = Duration::from_secs(60);
pub(super) const RENEW_EARLY: Duration = Duration::from_secs(60);
pub(super) const RENEW_LAST: Duration = Duration::from_secs(10);
const VOICE_ASK: RequestId = RequestId(u64::MAX);

pub struct Voice {
    pub accounts: Arc<dyn Connect>,
    pub place: PresetPlace,
    pub legs: Option<Legs>,
    pub records: mpsc::UnboundedSender<CallRecord>,
    pub earlier: Earlier,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallRecord {
    pub call: CallId,
    pub outcome: CallOutcome,
    pub seconds: u64,
    pub lines: Vec<String>,
    pub unknown: BTreeMap<String, u32>,
    pub endpoint_name: Option<String>,
    pub model: Option<String>,
    pub usage: Option<Usage>,
    pub cost_usd: Option<f64>,
    pub list_usd: Option<f64>,
}

impl CallRecord {
    fn unopened(call: CallId, outcome: CallOutcome) -> Self {
        Self {
            call,
            outcome,
            seconds: 0,
            lines: Vec::new(),
            unknown: BTreeMap::new(),
            endpoint_name: None,
            model: None,
            usage: None,
            cost_usd: None,
            list_usd: None,
        }
    }

    pub fn unpriced(&self) -> bool {
        crate::pricing::unpriced(self.usage.is_some(), self.cost_usd)
    }
}

pub(super) enum Unopened {
    Stopped,
    Failed(ErrorKind, String),
}

pub(super) type Started = Result<Link, Unopened>;

pub(super) struct Opening {
    call: CallId,
    stopped: Arc<AtomicBool>,
    pub(super) future: BoxFuture<'static, Started>,
}

#[derive(Clone, Copy)]
enum Hold {
    Delegation,
    Reply,
}

struct Spoken {
    tap: bool,
    text: String,
}

struct HandOff {
    id: InputId,
    lines: Vec<Spoken>,
    delegations: Vec<(String, Option<String>)>,
    answering: bool,
    replied: bool,
    text: Option<(Option<usize>, String)>,
    tools: BTreeMap<String, String>,
    next: NextSaid,
}

#[derive(Default)]
pub(super) struct NextSaid {
    after_tool: bool,
}

impl NextSaid {
    pub(super) fn heard(&mut self, event: &AgentEvent) {
        if let AgentEvent::ToolCallFinished { outcome, .. } = event {
            self.after_tool = matches!(
                outcome,
                ToolOutcome::Completed {
                    is_error: false,
                    ..
                }
            );
        }
    }

    pub(super) fn opens(&mut self, said_yet: bool) -> bool {
        let after_tool = std::mem::take(&mut self.after_tool);
        !said_yet || after_tool
    }
}

pub(super) struct Link {
    account: String,
    priced: bool,
    route: Arc<dyn VoiceRoute>,
    asked: OpenCall,
    leg: Box<dyn Leg>,
    opened: Opened,
    out: mpsc::UnboundedSender<String>,
    pub(super) frames: mpsc::Receiver<String>,
    since: Instant,
    failed: Option<CallOutcome>,
    unknown: BTreeMap<String, u32>,
    spent: Vec<Spent>,
}

pub(super) struct Call {
    id: CallId,
    alone: bool,
    pub(super) link: Option<Link>,
    parked: Vec<LiveSay>,
    cost: AccountSpend,
    started: Instant,
    quiet_from: Instant,
    lines: Vec<Spoken>,
    delegations: Vec<(String, Option<String>)>,
    hold: Option<(Hold, Instant)>,
    open: Option<HandOff>,
    pending: bool,
    record: Vec<String>,
}

pub(super) struct Service {
    voice: Arc<Voice>,
    agent: Option<mpsc::Sender<ClientCommand>>,
    client: mpsc::Sender<AgentEvent>,
    up: Arc<Mutex<mpsc::Receiver<AudioFrame>>>,
    down: mpsc::Sender<AudioFrame>,
    pub(super) opening: Option<Opening>,
    pub(super) call: Option<Call>,
    brain: bool,
    handoffs: u64,
    last_tap: Option<Instant>,
    earlier: Earlier,
    carried: Vec<Spoken>,
    asked: Option<oneshot::Sender<SettingsSnapshot>>,
}

impl Service {
    pub(super) fn new(
        mut voice: Voice,
        agent: mpsc::Sender<ClientCommand>,
        client: mpsc::Sender<AgentEvent>,
        up: mpsc::Receiver<AudioFrame>,
        down: mpsc::Sender<AudioFrame>,
    ) -> Self {
        let earlier = std::mem::take(&mut voice.earlier);
        Self {
            voice: Arc::new(voice),
            agent: Some(agent),
            client,
            up: Arc::new(Mutex::new(up)),
            down,
            opening: None,
            call: None,
            brain: false,
            handoffs: earlier.taps,
            last_tap: None,
            earlier,
            carried: Vec::new(),
            asked: None,
        }
    }

    pub(super) fn quiet_at(&self) -> Option<Instant> {
        let call = self.call.as_ref().filter(|_| !self.brain)?;
        Some(call.quiet_from + QUIET)
    }

    pub(super) fn renew_at(&self) -> Option<Instant> {
        let link = self.call.as_ref()?.link.as_ref()?;
        let limit = link.route.limit()?;
        let early = if link.route.busy() {
            RENEW_LAST
        } else {
            RENEW_EARLY
        };
        Some(link.since + limit.saturating_sub(early))
    }

    pub(super) async fn renew(&mut self) {
        if self.renew_at().is_some_and(|at| at <= Instant::now()) {
            self.redial(CallEnd::Hangup);
        }
    }

    pub(super) fn hold_at(&self) -> Option<Instant> {
        self.call.as_ref()?.hold.map(|(_, at)| at)
    }

    pub(super) async fn held(&mut self) {
        if let Some(call) = self.call.as_mut() {
            call.hold = None;
        }
        self.hand_off().await;
    }

    pub(super) async fn pass(&mut self, command: ClientCommand) {
        if let Some(agent) = &self.agent {
            let _ = agent.send(command).await;
        }
    }

    pub(super) async fn command(&mut self, command: VoiceCommand) {
        match command {
            VoiceCommand::Start { call, .. } => {
                self.end(CallEnd::Hangup).await;
                let (asked, settings) = oneshot::channel();
                let ask = ClientCommand::DescribeSettings { request: VOICE_ASK };
                if let Some(agent) = &self.agent
                    && agent.send(ask).await.is_ok()
                {
                    self.asked = Some(asked);
                }
                let voice = self.voice.clone();
                self.dialing(call, |audio, stopped| {
                    open(voice, settings, audio, stopped).boxed()
                });
            }
            VoiceCommand::Stop { call } if self.holds(&call) => {
                let answering = self.call.as_ref().and_then(|c| c.open.as_ref());
                if answering.is_some_and(|open| open.answering) {
                    self.pass(ClientCommand::Cancel).await;
                }
                self.end(CallEnd::Hangup).await;
            }
            VoiceCommand::Note { text, .. } => self.tap(&text).await,
            VoiceCommand::Stop { .. } => {}
        }
    }

    fn dialing(
        &mut self,
        call: CallId,
        dial: impl FnOnce(LegAudio, Arc<AtomicBool>) -> BoxFuture<'static, Started>,
    ) {
        let audio = LegAudio {
            call: call.clone(),
            up: self.up.clone(),
            down: self.down.clone(),
        };
        let stopped = Arc::new(AtomicBool::new(false));
        let future = dial(audio, stopped.clone());
        self.opening = Some(Opening {
            call,
            stopped,
            future,
        });
    }

    fn redial(&mut self, end: CallEnd) {
        let Some(call) = self.call.as_mut() else {
            return;
        };
        let Some(mut link) = call.link.take() else {
            return;
        };
        let record = link.closed(call.id.clone(), end, std::mem::take(&mut call.record));
        call.cost.add(book(&self.voice.records, record));
        let (account, asked) = (link.account.clone(), link.asked.clone());
        tokio::spawn(link.hang_up());
        let voice = self.voice.clone();
        let id = call.id.clone();
        self.dialing(id, |audio, stopped| {
            redial(voice, account, asked, audio, stopped).boxed()
        });
    }

    fn holds(&self, call: &CallId) -> bool {
        self.call.as_ref().is_some_and(|c| c.id == *call)
            || self.opening.as_ref().is_some_and(|o| o.call == *call)
    }

    pub(super) async fn opened(&mut self, started: Started) {
        let Some(opening) = self.opening.take() else {
            return;
        };
        match started {
            Ok(link) => {
                if let Some(text) = self.earlier.note() {
                    link.say(&LiveSay::Note {
                        delegation: None,
                        text,
                    });
                }
                if let Some(call) = self.call.as_mut() {
                    for say in std::mem::take(&mut call.parked) {
                        link.say(&say);
                    }
                    call.link = Some(link);
                    return;
                }
                self.call = Some(Call::new(opening.call.clone(), link));
                self.emit(VoiceEvent::Started { call: opening.call }).await;
            }
            Err(Unopened::Stopped) => {}
            Err(Unopened::Failed(kind, message)) => {
                let outcome = CallOutcome::Failed { kind, message };
                if self.call.is_some() {
                    return self.finish(outcome).await;
                }
                self.unopened(opening.call, outcome).await;
            }
        }
    }

    pub(super) async fn frame(&mut self, frame: Option<String>) {
        let Some(link) = self.call.as_mut().and_then(|call| call.link.as_mut()) else {
            return;
        };
        let Some(frame) = frame else {
            if link.since.elapsed() < STEADY {
                return self.end(CallEnd::Remote).await;
            }
            return self.redial(CallEnd::Remote);
        };
        for event in link.route.read(&frame) {
            self.live(event).await;
        }
    }

    async fn live(&mut self, event: LiveEvent) {
        let Some(call) = self.call.as_mut() else {
            return;
        };
        match event {
            LiveEvent::Heard(text) => {
                let now = Instant::now();
                call.quiet_from = now;
                call.push(false, line(words::HEARD, &text));
                let asking = call
                    .open
                    .as_ref()
                    .is_some_and(|open| !open.replied && !open.delegations.is_empty());
                match call.hold {
                    Some((Hold::Delegation, _)) => self.held().await,
                    None if !asking => {
                        call.pending = true;
                        if !call.alone {
                            return self.hand_off().await;
                        }
                        if call.delegations.is_empty() {
                            call.hold = Some((Hold::Reply, now + REPLY_WAIT));
                        }
                    }
                    _ => {}
                }
            }
            LiveEvent::Said(text) => {
                let now = Instant::now();
                call.quiet_from = now;
                call.push(false, line(words::SAID, &text));
                call.hurry(now);
            }
            LiveEvent::Delegated { id, request } => {
                call.delegations.push((id, request));
                if let Some((Hold::Reply, _)) = call.hold {
                    return self.held().await;
                }
                call.hold
                    .get_or_insert((Hold::Delegation, Instant::now() + HOLD));
            }
            LiveEvent::TurnDone => call.quiet_from = Instant::now(),
            LiveEvent::Interrupted { cut } => {
                call.quiet_from = Instant::now();
                let open = call.open.as_mut().filter(|open| open.replied);
                let unsent = open.and_then(|open| open.text.take()).map(|(_, rest)| rest);
                if let Some(part) = cut.or(unsent).filter(|part| !part.trim().is_empty()) {
                    call.push(false, line(words::CUT_OFF, part.trim()));
                }
            }
            other => {
                if let Some(link) = call.link.as_mut() {
                    link.took(other);
                }
            }
        }
    }

    pub(super) async fn quiet(&mut self) {
        self.end(CallEnd::Idle).await;
    }

    pub(super) async fn client_gone(&mut self) {
        self.end(CallEnd::ClientGone).await;
        self.agent = None;
    }

    pub(super) async fn session_gone(&mut self) {
        self.end(CallEnd::Hangup).await;
    }

    async fn tap(&mut self, text: &str) {
        let now = Instant::now();
        if self
            .last_tap
            .is_some_and(|at| now.duration_since(at) < words::TAP_GAP)
        {
            return;
        }
        self.last_tap = Some(now);
        let tapped = line(words::TAPPED, &cut_chars(text, words::TAP_CHARS));
        let Some(call) = self.call.as_mut() else {
            self.handoffs += 1;
            let id = format!("{TAP}{}", self.handoffs);
            let carried = std::mem::take(&mut self.carried).into_iter();
            let text: Vec<String> = carried.map(|l| l.text).chain([tapped]).collect();
            return self.pass(ClientCommand::queue(id, text.join("\n"))).await;
        };
        call.quiet_from = now;
        call.pending = true;
        call.hurry(now);
        call.push(true, tapped.clone());
        call.say(&LiveSay::Note {
            delegation: None,
            text: tapped,
        });
        self.hand_off().await;
    }

    async fn hand_off(&mut self) {
        let Some(call) = self.call.as_mut() else {
            return;
        };
        if call.open.is_some()
            || call.hold.is_some()
            || (call.delegations.is_empty() && !call.pending)
        {
            return;
        }
        call.pending = false;
        let lines = self.carried.drain(..).chain(call.lines.drain(..)).collect();
        let (lines, dropped) = capped(lines);
        call.record.extend(dropped);
        let delegations = std::mem::take(&mut call.delegations);
        let text: Vec<String> = lines
            .iter()
            .map(|l| l.text.clone())
            .chain(asked(delegations.clone()))
            .collect();
        self.handoffs += 1;
        let id = InputId(format!("{HANDED}{}-{}", call.id.0, self.handoffs));
        call.open = Some(HandOff {
            id: id.clone(),
            lines,
            delegations,
            answering: false,
            replied: false,
            text: None,
            tools: BTreeMap::new(),
            next: NextSaid::default(),
        });
        self.pass(ClientCommand::queue(id.0, text.join("\n"))).await;
    }

    pub(super) async fn event(&mut self, event: AgentEvent) {
        if let AgentEvent::SettingsSnapshot(snapshot) = event {
            if snapshot.answers != VOICE_ASK {
                let _ = self
                    .client
                    .send(AgentEvent::SettingsSnapshot(snapshot))
                    .await;
            } else if let Some(asked) = self.asked.take() {
                let _ = asked.send(snapshot);
            }
            return;
        }
        self.read(&event).await;
        crate::agent::send_scrubbed(&self.client, event, None).await;
    }

    async fn read(&mut self, event: &AgentEvent) {
        self.earlier.heard(event);
        match event {
            AgentEvent::TurnStarted { .. } => self.brain = true,
            AgentEvent::TurnEnded { outcome, .. } => {
                self.brain = false;
                self.turn_ended(outcome).await;
            }
            AgentEvent::InputHeld { ids, .. } => self.withdrawn(ids, true).await,
            AgentEvent::InputWithdrawn { ids, .. } => self.withdrawn(ids, false).await,
            AgentEvent::Rejected { id: Some(id), .. } => {
                self.withdrawn(std::slice::from_ref(id), false).await;
            }
            _ => {}
        }
        let Some(call) = self.call.as_mut() else {
            return;
        };
        let Some(open) = call.open.as_mut() else {
            return;
        };
        open.next.heard(event);
        match event {
            AgentEvent::InputDelivered { ids, .. } if ids.contains(&open.id) => {
                open.answering = true;
            }
            _ if !open.answering => {}
            AgentEvent::BlockStarted {
                index,
                kind: BlockKind::Text,
            } if open.next.opens(open.replied) || open.text.is_some() => {
                open.text = Some((Some(*index), String::new()));
            }
            AgentEvent::BlockStarted { index, .. } => {
                if let Some(text) = open.text.as_mut().filter(|(at, _)| *at == Some(*index)) {
                    *text = (None, String::new());
                }
            }
            AgentEvent::TextDelta { index, text } => {
                for part in open.sentences(*index, text) {
                    call.answer(part);
                }
            }
            AgentEvent::BlockStopped { index } => {
                if let Some((_, rest)) = open.text.take_if(|(at, _)| *at == Some(*index))
                    && (!open.replied || !rest.trim().is_empty())
                {
                    call.answer(rest.trim().to_owned());
                }
            }
            AgentEvent::ToolCallRequested { call_id, tool, .. } => {
                open.tools.insert(call_id.clone(), tool.clone());
            }
            AgentEvent::ToolCallFinished {
                call_id,
                outcome:
                    ToolOutcome::Completed {
                        is_error: false, ..
                    },
                ..
            } => {
                if let Some(tool) = open.tools.remove(call_id) {
                    call.note(words::finished(&tool));
                }
            }
            _ => {}
        }
    }

    async fn turn_ended(&mut self, outcome: &TurnOutcome) {
        let Some(call) = self.call.as_mut() else {
            return;
        };
        call.quiet_from = Instant::now();
        let Some(open) = call.open.as_ref().filter(|open| open.answering) else {
            return;
        };
        let answered = matches!(
            outcome,
            TurnOutcome::Completed {
                stop_reason: StopReason::EndTurn,
                ..
            }
        );
        if open.replied && (open.text.is_some() || !answered) {
            call.answer(words::NO_ANSWER.to_owned());
        } else if !open.replied && (!open.delegations.is_empty() || !call.alone) {
            if answered {
                for (id, _) in open.delegations.clone() {
                    call.say(&LiveSay::Note {
                        delegation: Some(id),
                        text: words::NOTHING_TO_SAY.to_owned(),
                    });
                }
            } else {
                call.answer(words::NO_ANSWER.to_owned());
            }
        }
        call.open = None;
        self.hand_off().await;
    }

    async fn withdrawn(&mut self, ids: &[InputId], held: bool) {
        let Some(call) = self.call.as_mut() else {
            return;
        };
        let Some(open) = call.open.as_ref().filter(|open| ids.contains(&open.id)) else {
            return;
        };
        if !open.replied && (!open.delegations.is_empty() || !call.alone) {
            call.answer(words::NO_ANSWER.to_owned());
        }
        let Some(open) = call.open.take() else {
            return;
        };
        call.lines.splice(0..0, open.lines);
        call.record.extend(asked(open.delegations));
        if held {
            self.pass(ClientCommand::Withdraw { ids: vec![open.id] })
                .await;
        }
        self.hand_off().await;
    }

    async fn end(&mut self, end: CallEnd) {
        self.finish(CallOutcome::Completed { end }).await;
    }

    async fn finish(&mut self, outcome: CallOutcome) {
        if let Some(opening) = self.opening.take() {
            opening.stopped.store(true, Ordering::SeqCst);
            tokio::spawn(async move {
                if let Ok(link) = opening.future.await {
                    link.hang_up().await;
                }
            });
            if self.call.is_none() {
                return self.unopened(opening.call, outcome).await;
            }
        }
        let Some(mut call) = self.call.take() else {
            return;
        };
        let failed = call.link.as_mut().and_then(|link| link.failed.take());
        let outcome = match outcome {
            CallOutcome::Completed {
                end: CallEnd::Hangup | CallEnd::ClientGone,
            } => outcome,
            _ => failed.unwrap_or(outcome),
        };
        if let Some(open) = call.open.take().filter(|open| !open.answering) {
            self.pass(ClientCommand::Withdraw { ids: vec![open.id] })
                .await;
            call.lines.splice(0..0, open.lines);
            call.delegations.splice(0..0, open.delegations);
        }
        let asked =
            asked(std::mem::take(&mut call.delegations)).map(|text| Spoken { tap: false, text });
        let carried: Vec<Spoken> = call.lines.drain(..).chain(asked).collect();
        let mut lines = std::mem::take(&mut call.record);
        lines.extend(carried.iter().map(|l| l.text.clone()));
        let carried = std::mem::take(&mut self.carried).into_iter().chain(carried);
        self.carried = capped(carried.collect()).0;
        let record = match call.link.as_mut() {
            Some(link) => link.record(call.id.clone(), outcome.clone(), lines),
            None => CallRecord {
                lines,
                ..CallRecord::unopened(call.id.clone(), outcome.clone())
            },
        };
        let mut cost = call.cost;
        cost.add(book(&self.voice.records, record));
        if let Some(link) = call.link {
            tokio::spawn(link.hang_up());
        }
        let seconds = call.started.elapsed().as_secs();
        let event = VoiceEvent::Ended {
            call: call.id,
            outcome,
            seconds,
            cost,
        };
        self.emit(event).await;
    }

    async fn unopened(&mut self, call: CallId, outcome: CallOutcome) {
        let record = CallRecord::unopened(call.clone(), outcome.clone());
        let cost = book(&self.voice.records, record);
        let event = VoiceEvent::Ended {
            call,
            outcome,
            seconds: 0,
            cost,
        };
        self.emit(event).await;
    }

    async fn emit(&mut self, event: VoiceEvent) {
        let _ = self.client.send(AgentEvent::Voice { event }).await;
    }
}

fn book(records: &mpsc::UnboundedSender<CallRecord>, record: CallRecord) -> AccountSpend {
    let cost = AccountSpend::charged(record.usage.as_ref(), record.cost_usd, record.unpriced());
    let _ = records.send(record);
    cost
}

impl Link {
    fn new(
        account: String,
        priced: bool,
        route: Arc<dyn VoiceRoute>,
        asked: OpenCall,
        leg: Box<dyn Leg>,
        opened: Opened,
        control: Control,
    ) -> Self {
        Self {
            account,
            priced,
            route,
            asked,
            leg,
            opened,
            out: control.out,
            frames: control.frames,
            since: Instant::now(),
            failed: None,
            unknown: BTreeMap::new(),
            spent: Vec::new(),
        }
    }

    fn took(&mut self, event: LiveEvent) {
        match event {
            LiveEvent::Failed { kind, message } => {
                self.failed = Some(CallOutcome::Failed { kind, message });
            }
            LiveEvent::Spent(spent) => self.spent.push(spent),
            LiveEvent::Send(frame) => {
                let _ = self.out.send(frame);
            }
            LiveEvent::Unknown(kind) => *self.unknown.entry(kind).or_default() += 1,
            _ => {}
        }
    }

    fn closed(&mut self, call: CallId, end: CallEnd, lines: Vec<String>) -> CallRecord {
        let outcome = match end {
            CallEnd::Hangup => None,
            _ => self.failed.take(),
        };
        let outcome = outcome.unwrap_or(CallOutcome::Completed { end });
        self.record(call, outcome, lines)
    }

    fn record(&mut self, call: CallId, outcome: CallOutcome, lines: Vec<String>) -> CallRecord {
        let seconds = self.since.elapsed().as_secs();
        let model = self.route.model().to_owned();
        let usd = crate::pricing::voice_usd(&model, seconds, &self.spent);
        let (cost_usd, list_usd) = if self.priced {
            (usd, None)
        } else {
            (None, usd)
        };
        CallRecord {
            call,
            outcome,
            seconds,
            lines,
            unknown: std::mem::take(&mut self.unknown),
            endpoint_name: Some(self.account.clone()),
            cost_usd,
            list_usd,
            model: Some(model),
            usage: Some(Usage {
                billing: self.route.billing(),
                ..Usage::default()
            }),
        }
    }

    fn say(&self, say: &LiveSay) {
        for frame in self.route.write(say) {
            let _ = self.out.send(frame);
        }
    }

    async fn hang_up(mut self) {
        self.say(&LiveSay::HangUp);
        drop(self.out);
        self.leg.close().await;
        let _ = self.route.close(&self.opened).await;
    }
}

impl Call {
    fn new(id: CallId, link: Link) -> Self {
        let now = Instant::now();
        Self {
            id,
            alone: link.route.answers_alone(),
            link: Some(link),
            parked: Vec::new(),
            cost: AccountSpend::default(),
            started: now,
            quiet_from: now,
            lines: Vec::new(),
            delegations: Vec::new(),
            hold: None,
            open: None,
            pending: false,
            record: Vec::new(),
        }
    }

    fn hurry(&mut self, now: Instant) {
        if let Some((Hold::Reply, at)) = self.hold.as_mut() {
            *at = (*at).min(now + HOLD);
        }
    }

    fn push(&mut self, tap: bool, text: String) {
        self.lines.push(Spoken { tap, text });
    }

    fn say(&mut self, say: &LiveSay) {
        match &self.link {
            Some(link) => link.say(say),
            None => self.parked.push(say.clone()),
        }
    }

    fn answer(&mut self, text: String) {
        let Some(open) = self.open.as_mut() else {
            return;
        };
        let first = !std::mem::replace(&mut open.replied, true);
        let mut ids: Vec<String> = open.delegations.iter().map(|(id, _)| id.clone()).collect();
        let newest = ids.pop();
        for delegation in ids.into_iter().filter(|_| first) {
            let text = words::COVERED.to_owned();
            self.say(&LiveSay::Note {
                delegation: Some(delegation),
                text,
            });
        }
        self.say(&LiveSay::Reply {
            delegation: newest,
            text,
        });
    }

    fn note(&mut self, text: String) {
        let delegation = self
            .open
            .as_ref()
            .and_then(|open| open.delegations.last())
            .map(|(id, _)| id.clone());
        self.say(&LiveSay::Note { delegation, text });
    }
}

impl HandOff {
    fn sentences(&mut self, index: usize, delta: &str) -> Vec<String> {
        let mut parts = Vec::new();
        let Some((_, rest)) = self.text.as_mut().filter(|(at, _)| *at == Some(index)) else {
            return parts;
        };
        rest.push_str(delta);
        while let Some(end) = sentence_end(rest) {
            let part: String = rest.drain(..end).collect();
            parts.extend(Some(part.trim().to_owned()).filter(|p| !p.is_empty()));
            *rest = rest.trim_start().to_owned();
        }
        parts
    }
}

pub(super) fn sentence_end(text: &str) -> Option<usize> {
    let mut closed = false;
    for (at, c) in text.char_indices() {
        if c == '\n' || (closed && c.is_whitespace()) {
            return Some(at);
        }
        closed = match c {
            '.' | '!' | '?' | '\u{2026}' => true,
            '"' | '\'' | ')' | ']' | '\u{201d}' | '\u{2019}' => closed,
            _ => false,
        };
    }
    None
}

fn capped(lines: Vec<Spoken>) -> (Vec<Spoken>, Vec<String>) {
    let (mut talk, mut taps) = (0, 0);
    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    for spoken in lines.into_iter().rev() {
        let (count, cap) = if spoken.tap {
            (&mut taps, words::HANDOFF_TAPS)
        } else {
            (&mut talk, words::HANDOFF_LINES)
        };
        if *count < cap {
            *count += 1;
            kept.push(spoken);
        } else {
            dropped.push(spoken.text);
        }
    }
    kept.reverse();
    dropped.reverse();
    (kept, dropped)
}

fn asked(delegations: Vec<(String, Option<String>)>) -> impl Iterator<Item = String> {
    delegations
        .into_iter()
        .filter_map(|(_, request)| request.map(|r| line(words::ASKED, &r)))
}

fn route_of(accounts: &dyn Connect, name: &str) -> Option<(Arc<dyn VoiceRoute>, bool)> {
    let connected = accounts.connect(ROOT).ok()?;
    let connection = connected.ready.iter().find(|c| c.name == name)?;
    Some((connection.provider.voice()?, connection.priced))
}

async fn instructions(written: String, place: PresetPlace) -> Option<String> {
    let read = move || {
        let config_dir = place.config_dir.as_deref();
        let path = crate::instructions::resolve_preset(&written, config_dir, place.home.as_deref());
        std::fs::read_to_string(path.ok()?).ok()
    };
    tokio::task::spawn_blocking(read).await.ok().flatten()
}

async fn open(
    voice: Arc<Voice>,
    settings: oneshot::Receiver<SettingsSnapshot>,
    audio: LegAudio,
    stopped: Arc<AtomicBool>,
) -> Started {
    let own = |sentence: &str| Unopened::Failed(ErrorKind::Other, sentence.to_owned());
    let unready = || own(words::ACCOUNT_UNREADY);
    let snapshot = settings.await.map_err(|_| unready())?;
    let text = |name: &str| {
        let setting = snapshot.settings.iter().find(|s| s.name.as_str() == name)?;
        setting.value.as_ref().map(SettingValue::text)
    };
    let name = snapshot.voice_account().map_err(|unset| match unset {
        VoiceUnset::NoneOffers => own(words::NO_VOICE_ACCOUNT),
        VoiceUnset::NotChosen => own(words::NO_ACCOUNT),
    })?;
    let (route, priced) = route_of(&*voice.accounts, &name).ok_or_else(unready)?;
    let legs = voice.legs.clone().ok_or_else(unready)?;
    let instructions = match text(names::VOICE_INSTRUCTIONS).filter(|_| route.answers_alone()) {
        Some(written) => instructions(written, voice.place.clone())
            .await
            .ok_or_else(|| own(words::NO_INSTRUCTIONS))?,
        None => String::new(),
    };
    let voice = text(names::VOICE_VOICE)
        .filter(|voice| route.voices().contains(&voice.as_str()))
        .or_else(|| route.voices().first().map(|v| (*v).to_owned()))
        .unwrap_or_default();
    let call = OpenCall {
        offer: String::new(),
        instructions,
        voice,
        language: text(names::VOICE_LANGUAGE),
    };
    dial(legs, name, route, priced, call, audio, stopped).await
}

async fn redial(
    voice: Arc<Voice>,
    account: String,
    asked: OpenCall,
    audio: LegAudio,
    stopped: Arc<AtomicBool>,
) -> Started {
    let unready = || Unopened::Failed(ErrorKind::Other, words::ACCOUNT_UNREADY.to_owned());
    let (route, priced) = route_of(&*voice.accounts, &account).ok_or_else(unready)?;
    let legs = voice.legs.clone().ok_or_else(unready)?;
    dial(legs, account, route, priced, asked, audio, stopped).await
}

async fn dial(
    legs: Legs,
    account: String,
    route: Arc<dyn VoiceRoute>,
    priced: bool,
    mut call: OpenCall,
    audio: LegAudio,
    stopped: Arc<AtomicBool>,
) -> Started {
    let provider = route.provider().to_owned();
    let failure = |e: ProviderError| Unopened::Failed(e.kind, e.kind.sentence(&provider));
    let mut leg = legs();
    call.offer = match leg.offer().await {
        Ok(offer) => offer,
        Err(e) => {
            leg.close().await;
            return Err(failure(e));
        }
    };
    if stopped.load(Ordering::SeqCst) {
        leg.close().await;
        return Err(Unopened::Stopped);
    }
    let since = Instant::now();
    let opened = match route.open(call.clone()).await {
        Ok(opened) => opened,
        Err(e) => {
            leg.close().await;
            return Err(Unopened::Failed(e.kind, e.message));
        }
    };
    if stopped.load(Ordering::SeqCst) {
        let _ = route.close(&opened).await;
        leg.close().await;
        return Err(Unopened::Stopped);
    }
    match leg.answer(opened.answer.clone(), audio).await {
        Ok(control) => Ok(Link {
            since,
            ..Link::new(account, priced, route, call, leg, opened, control)
        }),
        Err(e) => {
            let _ = route.close(&opened).await;
            leg.close().await;
            Err(failure(e))
        }
    }
}
