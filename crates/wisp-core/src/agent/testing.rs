use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use futures::stream::{self, BoxStream};
use futures::{FutureExt, StreamExt};
use jiff::SignedDuration;
use serde_json::{Value, json};
use tokio::sync::{Notify, Semaphore, mpsc};
use tokio::time::Instant;
use wisp_protocol::plan_usage::{PlanUsage, PlanUsageError};
use wisp_protocol::{
    AgentEvent, Billing, CacheTtl, ClientCommand, ContentBlock, ErrorDetail, ErrorKind,
    FailurePhase, FailureSource, HistoryChange, ImageData, InputTransformation, Message,
    RetryClass, SessionConfig, StopReason, TurnOutcome, Usage, WithdrawnAttachment,
};

use crate::agent::tests::{start, usage};
use crate::agent::{Agent, Mail, MailSender};
use crate::endpoints::ProviderKind;
use crate::provider::anthropic::{self, capabilities, media, search};
use crate::provider::{
    CompactionAsk, Delta, Features, Iteration, ModelCapabilities, PrefixBytes, Provider,
    ProviderError, ProviderEvent, Rendering, Request, RequestOptions, SearchSignal, ServerCalls,
    ServerToolSpec, TokenCount, WirePrint,
};
use crate::session::{MemoryStore, REDACTED, SessionStore};
use crate::test_guard::{self, pick, search_use, within};
use crate::tool::ToolSpec;

pub(crate) type Script = Vec<Result<ProviderEvent, ProviderError>>;

#[derive(Clone)]
pub(crate) enum Wait {
    Gate(Arc<Semaphore>),
    Notified(Arc<Notify>),
    Sleep(Duration),
    Take(Arc<Semaphore>),
}

#[derive(Clone)]
pub(crate) enum Then {
    Command(ClientCommand),
    Mail(Mail),
    Run(Arc<dyn Fn() + Send + Sync>),
}

#[derive(Clone, Default)]
pub(crate) struct Step {
    sending: bool,
    wait: Option<Wait>,
    pub(crate) script: Script,
    then: Vec<Then>,
    hang: bool,
}

impl From<Script> for Step {
    fn from(script: Script) -> Self {
        Self {
            script,
            ..Self::default()
        }
    }
}

impl Step {
    pub(crate) fn hang() -> Self {
        Self {
            sending: true,
            hang: true,
            ..Self::default()
        }
    }

    pub(crate) fn gated(self, gate: &Arc<Semaphore>) -> Self {
        Self {
            sending: true,
            ..self.held(Wait::Gate(gate.clone()))
        }
    }

    pub(crate) fn held(self, wait: Wait) -> Self {
        Self {
            wait: Some(wait),
            ..self
        }
    }

    pub(crate) fn hanging(self) -> Self {
        Self { hang: true, ..self }
    }

    pub(crate) fn then(mut self, command: ClientCommand) -> Self {
        self.then.push(Then::Command(command));
        self
    }

    pub(crate) fn mail(mut self, mail: Mail) -> Self {
        self.then.push(Then::Mail(mail));
        self
    }

    pub(crate) fn run(mut self, f: impl Fn() + Send + Sync + 'static) -> Self {
        self.then.push(Then::Run(Arc::new(f)));
        self
    }

    fn play(
        self,
        commands: Option<mpsc::Sender<ClientCommand>>,
        letters: Option<MailSender>,
    ) -> BoxStream<'static, Result<ProviderEvent, ProviderError>> {
        let Self {
            sending,
            wait,
            mut script,
            then,
            hang,
        } = self;
        let error = if matches!(script.last(), Some(Err(_))) {
            script.pop()
        } else {
            None
        };
        let events = stream::once(async move {
            match wait {
                Some(Wait::Gate(gate)) => drop(gate.acquire_owned().await),
                Some(Wait::Notified(notify)) => notify.notified().await,
                Some(Wait::Sleep(wait)) => tokio::time::sleep(wait).await,
                Some(Wait::Take(gate)) => {
                    let _ = gate.acquire().await.map(|p| p.forget());
                }
                None => {}
            }
            stream::iter(script)
        })
        .flatten();
        let after = stream::once(async move {
            for action in then {
                match action {
                    Then::Command(command) => match &commands {
                        Some(commands) => commands.try_send(command).unwrap(),
                        None => broken("a step's commands need Fake::send_to"),
                    },
                    Then::Mail(mail) => match &letters {
                        Some(letters) => letters.send(mail).unwrap(),
                        None => broken("a step's mail needs Fake::mail_to"),
                    },
                    Then::Run(f) => f(),
                }
            }
            error
        })
        .filter_map(futures::future::ready);
        let played = stream::iter(sending.then_some(Ok(ProviderEvent::Sending)))
            .chain(events)
            .chain(after);
        if hang {
            played.chain(stream::pending()).boxed()
        } else {
            played.boxed()
        }
    }
}

#[derive(Clone, Default)]
pub(crate) enum Prefix {
    #[default]
    None,
    Sent,
    Fixed(String),
}

#[derive(Clone, Copy)]
pub(crate) enum Caps {
    Fixed(Option<&'static ModelCapabilities>),
    Fn(fn(&str) -> Option<&'static ModelCapabilities>),
    Only(&'static [&'static str]),
}

#[derive(Clone, Copy)]
pub(crate) enum Prints {
    Real,
    Shared,
}

#[derive(Clone, Default)]
pub(crate) struct Knobs {
    pub(crate) prefix: Prefix,
    pub(crate) caps: Option<Caps>,
    pub(crate) documented: Option<bool>,
    pub(crate) bound: Option<bool>,
    pub(crate) keeps_warm: Option<bool>,
    pub(crate) cache_life: Option<SignedDuration>,
    pub(crate) opt_in: Option<bool>,
    pub(crate) media: bool,
    pub(crate) search: bool,
    pub(crate) pause_limit: Option<u32>,
    pub(crate) wire_print: bool,
    pub(crate) threads: bool,
    pub(crate) prefix_bytes: Option<Prints>,
    pub(crate) count: Option<u64>,
    pub(crate) hang_when_out: bool,
    pub(crate) kind: Option<ProviderKind>,
    pub(crate) billing: Option<Billing>,
    pub(crate) wire: Option<(Arc<Wire>, String)>,
    pub(crate) list: Option<Arc<crate::models::list::ModelList>>,
    pub(crate) web_search_models: Vec<String>,
    pub(crate) fresh: Option<Arc<crate::models::list::ModelList>>,
    pub(crate) stall_list: bool,
    pub(crate) plan_usage: Option<PlanUsage>,
    pub(crate) voice: Option<Arc<dyn crate::provider::voice::VoiceRoute>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Moment {
    Sent(String, Instant),
    Streamed(String),
}

#[derive(Default)]
pub(crate) struct Wire {
    pub(crate) timeline: Mutex<Vec<Moment>>,
    cached: Mutex<BTreeSet<String>>,
}

pub(crate) const PREFIX_TOKENS: u64 = 10_000;

fn cached_usage(script: Script, hit: bool) -> Script {
    restamp(script, |usage, _| {
        if hit {
            usage.cache_read_input_tokens = PREFIX_TOKENS;
        } else {
            usage.cache_creation_input_tokens = PREFIX_TOKENS;
        }
    })
}

pub(crate) fn restamp(mut script: Script, stamp: impl Fn(&mut Usage, &mut [Iteration])) -> Script {
    for event in &mut script {
        if let Ok(
            ProviderEvent::MessageStart {
                usage, iterations, ..
            }
            | ProviderEvent::MessageDelta {
                usage, iterations, ..
            },
        ) = event
        {
            stamp(usage, iterations);
        }
    }
    script
}

static ORDER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub(crate) struct Sent {
    pub(crate) session: SessionConfig,
    pub(crate) tools: Vec<ToolSpec>,
    pub(crate) deferred: Vec<ToolSpec>,
    pub(crate) by_value: Vec<ToolSpec>,
    pub(crate) server_tools: Vec<ServerToolSpec>,
    pub(crate) messages: Vec<Message>,
    pub(crate) previous_response_id: Option<String>,
    pub(crate) features: Features,
    pub(crate) options: RequestOptions,
    pub(crate) rendering: Rendering,
    pub(crate) compaction: Option<CompactionAsk>,
    pub(crate) at: Instant,
    pub(crate) order: u64,
}

impl Sent {
    fn of(request: &Request<'_>) -> Self {
        let Request {
            session,
            tools,
            deferred,
            by_value,
            server_tools,
            messages,
            previous_response_id,
            features,
            options,
            rendering,
            compaction,
        } = *request;
        Self {
            session: session.clone(),
            tools: tools.to_vec(),
            deferred: deferred.to_vec(),
            by_value: by_value.to_vec(),
            server_tools: server_tools.to_vec(),
            messages: messages.to_vec(),
            previous_response_id: previous_response_id.map(str::to_owned),
            features,
            options,
            rendering: rendering.clone(),
            compaction: compaction.cloned(),
            at: Instant::now(),
            order: ORDER.fetch_add(1, Ordering::SeqCst),
        }
    }

    pub(crate) fn request(&self) -> Request<'_> {
        Request {
            session: &self.session,
            tools: &self.tools,
            deferred: &self.deferred,
            by_value: &self.by_value,
            server_tools: &self.server_tools,
            messages: &self.messages,
            previous_response_id: self.previous_response_id.as_deref(),
            features: self.features,
            options: self.options,
            rendering: &self.rendering,
            compaction: self.compaction.as_ref(),
        }
    }

    pub(crate) fn body(&self) -> Value {
        body_of(&self.request())
    }

    pub(crate) fn text(&self) -> String {
        text_of(&self.request())
    }

    pub(crate) fn beta(&self) -> String {
        anthropic::beta_header(&self.request())
    }

    pub(crate) fn count_body(&self) -> Vec<u8> {
        anthropic::count_body(&self.request()).unwrap()
    }
}

#[derive(Default)]
struct Shared {
    steps: Mutex<VecDeque<Step>>,
    sent: Mutex<Vec<Sent>>,
    counted: Mutex<Vec<Sent>>,
    plan_usage_calls: AtomicUsize,
    refreshes: AtomicUsize,
    commands: Mutex<Option<mpsc::Sender<ClientCommand>>>,
    mail: Mutex<Option<MailSender>>,
}

pub(crate) const NEW_GEMINI: &str = "gemini-nova-new";

pub(crate) fn gemini_list(ids: &[&str]) -> Arc<crate::models::list::ModelList> {
    Arc::new(
        crate::models::list::ModelList::from_gemini_ids(
            ids.iter().map(|&id| (id.to_owned(), None, None)),
            None,
        )
        .unwrap(),
    )
}

pub(crate) struct Fake {
    shared: Arc<Shared>,
    knobs: Mutex<Knobs>,
}

impl Fake {
    pub(crate) fn new(steps: impl IntoIterator<Item = Step>) -> Arc<Self> {
        Self::with(Knobs::default(), steps)
    }

    pub(crate) fn with(knobs: Knobs, steps: impl IntoIterator<Item = Step>) -> Arc<Self> {
        let fake = Self {
            shared: Arc::default(),
            knobs: Mutex::new(knobs),
        };
        fake.push(steps);
        Arc::new(fake)
    }

    pub(crate) fn view(&self, change: impl FnOnce(&mut Knobs)) -> Arc<Self> {
        let mut knobs = self.knobs(|k| k.clone());
        change(&mut knobs);
        Arc::new(Self {
            shared: self.shared.clone(),
            knobs: Mutex::new(knobs),
        })
    }

    pub(crate) fn knobs<R>(&self, f: impl FnOnce(&mut Knobs) -> R) -> R {
        f(&mut self.knobs.lock().unwrap())
    }

    pub(crate) fn send_to(&self, commands: mpsc::Sender<ClientCommand>) {
        *self.shared.commands.lock().unwrap() = Some(commands);
    }

    pub(crate) fn commands(&self) -> mpsc::Sender<ClientCommand> {
        self.shared.commands.lock().unwrap().clone().unwrap()
    }

    pub(crate) fn mail_to(&self, mail: MailSender) {
        *self.shared.mail.lock().unwrap() = Some(mail);
    }

    pub(crate) fn push(&self, steps: impl IntoIterator<Item = Step>) {
        self.shared.steps.lock().unwrap().extend(steps);
    }

    pub(crate) fn clear(&self) {
        self.shared.steps.lock().unwrap().clear();
    }

    pub(crate) fn sent(&self) -> Vec<Sent> {
        self.shared.sent.lock().unwrap().clone()
    }

    pub(crate) fn body(&self, n: usize) -> Value {
        self.sent()[n].body()
    }

    pub(crate) fn beta(&self, n: usize) -> String {
        self.sent()[n].beta()
    }

    pub(crate) fn bodies(&self) -> Vec<Value> {
        self.sent().iter().map(Sent::body).collect()
    }

    pub(crate) fn texts(&self) -> Vec<String> {
        self.sent().iter().map(Sent::text).collect()
    }

    pub(crate) fn counted(&self) -> Vec<Sent> {
        self.shared.counted.lock().unwrap().clone()
    }

    pub(crate) fn refreshes(&self) -> usize {
        self.shared.refreshes.load(Ordering::SeqCst)
    }

    pub(crate) fn plan_usage_calls(&self) -> usize {
        self.shared.plan_usage_calls.load(Ordering::SeqCst)
    }
}

struct Bare;

impl Provider for Bare {
    fn stream(&self, _: &Request<'_>) -> BoxStream<'static, Result<ProviderEvent, ProviderError>> {
        stream::empty().boxed()
    }

    fn cache_prefix(&self, _: &Request<'_>) -> Option<String> {
        None
    }
}

impl Provider for Fake {
    fn configure_plan_usage(&self, enabled: bool) -> bool {
        self.knobs(|k| match k.opt_in.as_mut() {
            Some(on) => std::mem::replace(on, enabled) != enabled,
            None => Bare.configure_plan_usage(enabled),
        })
    }

    fn plan_usage(&self) -> BoxFuture<'static, Result<Option<PlanUsage>, PlanUsageError>> {
        self.shared.plan_usage_calls.fetch_add(1, Ordering::SeqCst);
        match self.knobs(|k| k.plan_usage.clone()) {
            Some(usage) => futures::future::ready(Ok(Some(usage))).boxed(),
            None => Bare.plan_usage(),
        }
    }

    fn stream(
        &self,
        request: &Request<'_>,
    ) -> BoxStream<'static, Result<ProviderEvent, ProviderError>> {
        self.shared.sent.lock().unwrap().push(Sent::of(request));
        let valid_json = match crate::endpoints::goes::model_provider(&request.session.model) {
            Some(ProviderKind::OpenAi) => crate::provider::openai::build_body(request)
                .is_ok_and(|body| serde_json::to_vec(&body).is_ok()),
            Some(ProviderKind::Gemini) => crate::provider::gemini::build_body(request)
                .is_ok_and(|body| serde_json::to_vec(&body).is_ok()),
            None | Some(ProviderKind::Anthropic) => anthropic::build_body(request)
                .ok()
                .and_then(|body| serde_json::from_slice::<Value>(&body).ok())
                .is_some_and(|body| {
                    if misplaced_system(&body).is_some() {
                        broken("a system message with content comes before a user message");
                    }
                    true
                }),
        };
        if !valid_json {
            broken("the request body is not JSON");
        }
        let next = self.shared.steps.lock().unwrap().pop_front();
        let Some(step) = next else {
            if !self.knobs(|k| k.hang_when_out) {
                broken("the test sent more requests than it scripted");
            }
            return stream::pending().boxed();
        };
        let commands = self.shared.commands.lock().unwrap().clone();
        let mail = self.shared.mail.lock().unwrap().clone();
        if let Some((wire, path)) = self.knobs(|k| k.wire.clone()) {
            let key = self.prefix_bytes(request).map(|bytes| {
                crate::warmth::Fingerprint::new("env", &bytes)
                    .key()
                    .as_str()
                    .to_owned()
            });
            return stream::once(async move {
                let hit = key
                    .as_ref()
                    .is_some_and(|k| wire.cached.lock().unwrap().contains(k));
                let mut step = step;
                wire.timeline
                    .lock()
                    .unwrap()
                    .push(Moment::Sent(path.clone(), Instant::now()));
                if let Some(wait) = step.wait.take() {
                    Step::default().held(wait).play(None, None).count().await;
                }
                wire.timeline.lock().unwrap().push(Moment::Streamed(path));
                if let Some(key) = key {
                    wire.cached.lock().unwrap().insert(key);
                    step.script = cached_usage(step.script, hit);
                }
                step.play(commands, mail)
            })
            .flatten()
            .boxed();
        }
        step.play(commands, mail)
    }

    fn echoed<'a>(
        &self,
        content: &'a [ContentBlock],
        earlier: &ServerCalls,
    ) -> Vec<&'a ContentBlock> {
        anthropic::echoed_blocks(content, earlier)
    }

    fn kind(&self) -> ProviderKind {
        self.knobs(|k| k.kind).unwrap_or_else(|| Bare.kind())
    }

    fn billing(&self) -> Billing {
        self.knobs(|k| k.billing).unwrap_or_else(|| Bare.billing())
    }

    fn pause_limit(&self) -> u32 {
        self.knobs(|k| k.pause_limit)
            .unwrap_or(super::MAX_CONTINUATIONS)
    }

    fn search_signal(&self, block: &ContentBlock) -> Option<SearchSignal> {
        if self.knobs(|k| k.search) {
            search::signal(block)
        } else {
            Bare.search_signal(block)
        }
    }

    fn capabilities(&self, model: &str) -> Option<ModelCapabilities> {
        let row = match self.knobs(|k| k.caps) {
            Some(Caps::Fixed(row)) => row.copied(),
            Some(Caps::Fn(row)) => row(model).copied(),
            Some(Caps::Only(models)) => capabilities::row(model)
                .filter(|_| models.contains(&model))
                .copied(),
            None => Bare.capabilities(model),
        };
        if self.kind() == ProviderKind::Gemini
            && let Some(list) = self.complete_model_list()
        {
            return list.capabilities(
                self.kind(),
                model,
                row,
                crate::provider::gemini::LISTED_ONLY,
            );
        }
        let row = row?;
        let list = self.model_list();
        list.as_deref()
            .and_then(|list| list.capabilities(self.kind(), model, Some(row), row))
            .or_else(|| self.complete_model_list().is_none().then_some(row))
    }

    fn web_search_models(&self) -> Vec<String> {
        self.knobs(|k| k.web_search_models.clone())
    }

    fn model_list(&self) -> Option<Arc<crate::models::list::ModelList>> {
        self.knobs(|k| k.list.clone())
    }

    fn complete_model_list(&self) -> Option<Arc<crate::models::list::ModelList>> {
        (self.kind() == ProviderKind::Gemini)
            .then(|| self.model_list())
            .flatten()
    }

    fn list_key(&self) -> Option<&str> {
        self.knobs(|k| k.fresh.is_some()).then_some("fake-list")
    }

    fn refresh_list(&self, _: std::path::PathBuf, _: bool) -> BoxFuture<'static, ()> {
        self.shared.refreshes.fetch_add(1, Ordering::SeqCst);
        if self.knobs(|k| k.stall_list) {
            return futures::future::pending().boxed();
        }
        let cell = crate::models::list::ListCell::new(self.model_list());
        let sent = self.shared.sent.lock().unwrap();
        sent.iter().for_each(|sent| cell.sent(&sent.session.model));
        self.knobs(|k| {
            k.fresh.iter().for_each(|fresh| cell.add(fresh));
            k.list = cell.get();
        });
        futures::future::ready(()).boxed()
    }

    fn media_types(&self) -> &'static [&'static str] {
        if self.knobs(|k| k.media) {
            &media::MEDIA_TYPES
        } else {
            Bare.media_types()
        }
    }

    fn features_documented(&self) -> bool {
        self.knobs(|k| k.documented)
            .unwrap_or_else(|| Bare.features_documented())
    }

    fn cache_life(&self, ttl: CacheTtl) -> SignedDuration {
        self.knobs(|k| k.cache_life)
            .unwrap_or_else(|| Bare.cache_life(ttl))
    }

    fn keeps_warm(&self) -> bool {
        self.knobs(|k| k.keeps_warm)
            .unwrap_or_else(|| Bare.keeps_warm())
    }

    fn voice(&self) -> Option<Arc<dyn crate::provider::voice::VoiceRoute>> {
        self.knobs(|k| k.voice.clone())
    }

    fn account_bound_thinking(&self, model: &str) -> bool {
        self.knobs(|k| k.bound)
            .unwrap_or_else(|| Bare.account_bound_thinking(model))
    }

    fn count_tokens(&self, request: &Request<'_>) -> BoxFuture<'static, TokenCount> {
        let Some(tokens) = self.knobs(|k| k.count) else {
            return Bare.count_tokens(request);
        };
        self.shared.counted.lock().unwrap().push(Sent::of(request));
        futures::future::ready(TokenCount::Counted { tokens }).boxed()
    }

    fn cache_prefix(&self, request: &Request<'_>) -> Option<String> {
        match self.knobs(|k| k.prefix.clone()) {
            Prefix::None => None,
            Prefix::Sent => anthropic::sent_prefix(request),
            Prefix::Fixed(digest) => Some(digest),
        }
    }

    fn prefix_bytes(&self, request: &Request<'_>) -> Option<PrefixBytes> {
        match self.knobs(|k| k.prefix_bytes) {
            Some(Prints::Real) => anthropic::prefix_bytes(request),
            Some(Prints::Shared) => Some(PrefixBytes {
                model: b"m".to_vec(),
                effort: b"e".to_vec(),
                thinking: b"t".to_vec(),
                tools: b"tools".to_vec(),
                system: b"system".to_vec(),
            }),
            None => Bare.prefix_bytes(request),
        }
    }

    fn wire_print(&self, request: &Request<'_>) -> Option<WirePrint> {
        if self.knobs(|k| k.wire_print) {
            anthropic::wire_print(request)
        } else {
            Bare.wire_print(request)
        }
    }

    fn threads(&self, request: &Request<'_>) -> bool {
        self.knobs(|k| k.threads) && anthropic::threads(request)
    }

    fn reply_print(&self, content: &[ContentBlock]) -> Option<String> {
        if self.knobs(|k| k.wire_print) {
            anthropic::reply_print(content)
        } else {
            Bare.reply_print(content)
        }
    }
}

#[derive(Clone)]
pub(crate) enum B {
    Think(&'static str),
    Text(&'static str),
    Call(&'static str, &'static str, Value),
    Search(&'static str, &'static str),
    Found(&'static str),
    Cited(&'static str, &'static str),
}

pub(crate) fn found(id: &str) -> Value {
    json!({"type": "web_search_tool_result", "tool_use_id": id, "content": [{
        "type": "web_search_result",
        "url": format!("https://example.com/{id}"),
        "title": format!("Page {id}"),
        "encrypted_content": format!("enc-{id}"),
        "page_age": "2 days ago"
    }]})
}

pub(crate) fn block_of(b: &B) -> ContentBlock {
    match b {
        B::Think(text) => ContentBlock::Thinking {
            thinking: (*text).into(),
            signature: format!("sig-{text}"),
        },
        B::Text(text) => ContentBlock::Text {
            text: (*text).into(),
        },
        B::Call(id, name, input) => ContentBlock::ToolUse {
            id: (*id).into(),
            name: (*name).into(),
            input: input.clone(),
        },
        B::Search(id, query) => test_guard::opaque(search_use(id, query)),
        B::Found(id) => test_guard::opaque(found(id)),
        B::Cited(text, url) => {
            test_guard::opaque(json!({"type": "text", "text": text, "citations": [{
                "type": "web_search_result_location",
                "url": url,
                "title": "Cited page",
                "encrypted_index": "ei-1",
                "cited_text": "quoted"
            }]}))
        }
    }
}

pub(crate) fn events_of(content: &[B]) -> Script {
    let mut script = Vec::new();
    for (index, b) in content.iter().enumerate() {
        script.push(Ok(ProviderEvent::BlockStart {
            index,
            block: block_of(b),
        }));
        script.push(Ok(ProviderEvent::BlockStop { index }));
    }
    script
}

pub(crate) fn script(content: &[B], stop: StopReason) -> Script {
    let mut s = vec![Ok(ProviderEvent::Sending), Ok(start(600, 0))];
    s.extend(events_of(content));
    s.extend(ended(stop, usage(600, 0, 10), Vec::new()));
    s
}

pub(crate) fn step(content: &[B], stop: StopReason) -> Step {
    Step::from(script(content, stop))
}

pub(crate) fn done(content: &[B]) -> Step {
    step(content, StopReason::EndTurn)
}

pub(crate) fn failing(content: &[B], error: ProviderError) -> Step {
    failing_after(events_of(content), error)
}

pub(crate) fn failing_after(events: Script, error: ProviderError) -> Step {
    let mut script = vec![Ok(ProviderEvent::Sending), Ok(start(600, 0))];
    script.extend(events);
    script.push(Err(error));
    Step::from(script)
}

pub(crate) fn adaptations(events: &[AgentEvent]) -> Vec<(Option<String>, HistoryChange)> {
    pick!(
        events,
        AgentEvent::HistoryAdapted {
            endpoint_name,
            change,
        } => (endpoint_name.clone(), change.clone())
    )
}

pub(crate) fn dropping(content: &[B], drops: &[(&str, &str)]) -> Step {
    let mut step = done(content);
    let transformations = drops
        .iter()
        .map(|(path, reason)| InputTransformation {
            kind: "thinking_dropped".into(),
            path: Some(Some((*path).into())),
            reason: Some(Some((*reason).into())),
            extra: BTreeMap::new(),
        })
        .collect();
    step.script.insert(
        2,
        Ok(ProviderEvent::InputTransformations {
            provider: "anthropic".into(),
            transformations,
        }),
    );
    step
}

pub(crate) fn with_iterations(mut step: Step, iterations: &[Iteration]) -> Step {
    for event in &mut step.script {
        if let Ok(ProviderEvent::MessageDelta { iterations: at, .. }) = event {
            *at = iterations.to_vec();
        }
    }
    step
}

pub(crate) fn failure(kind: ErrorKind, retry: RetryClass, billed: bool) -> ProviderError {
    let phase = if billed {
        FailurePhase::Stream
    } else {
        FailurePhase::Response
    };
    let mut detail = ErrorDetail::new(phase, FailureSource::Endpoint, retry);
    detail.possibly_billed = billed;
    ProviderError::classified(kind, format!("{} happened", kind.as_str()), detail)
}

pub(crate) fn classified(
    kind: ErrorKind,
    message: impl Into<String>,
    phase: FailurePhase,
    retry: RetryClass,
) -> ProviderError {
    let detail = ErrorDetail::new(phase, FailureSource::Endpoint, retry);
    ProviderError::classified(kind, message, detail)
}

pub(crate) fn content_refusal(message: &str) -> ProviderError {
    let mut error = classified(
        ErrorKind::InvalidRequest,
        message,
        FailurePhase::Response,
        RetryClass::Never,
    );
    error.content_refused = true;
    error
}

pub(crate) fn mid_stream(retry: RetryClass) -> ProviderError {
    let mut detail = ErrorDetail::new(FailurePhase::Stream, FailureSource::Endpoint, retry);
    detail.possibly_billed = true;
    ProviderError::classified(ErrorKind::Server, "the stream broke", detail)
}

pub(crate) fn started(id: Option<&str>, model: &str, usage: Usage) -> ProviderEvent {
    ProviderEvent::MessageStart {
        id: id.map(str::to_owned),
        model: model.into(),
        usage,
        iterations: Vec::new(),
        diagnostics: None,
    }
}

pub(crate) fn ended(stop: StopReason, usage: Usage, iterations: Vec<Iteration>) -> Script {
    vec![
        Ok(ProviderEvent::MessageDelta {
            stop_reason: Some(stop),
            stop_details: None,
            usage,
            iterations,
            diagnostics: None,
        }),
        Ok(ProviderEvent::MessageStop),
    ]
}

pub(crate) fn text(index: usize, text: &str) -> Script {
    vec![
        Ok(ProviderEvent::BlockStart {
            index,
            block: ContentBlock::Text {
                text: String::new(),
            },
        }),
        Ok(ProviderEvent::BlockDelta {
            index,
            delta: Delta::Text(text.into()),
        }),
        Ok(ProviderEvent::BlockStop { index }),
    ]
}

pub(crate) fn thinking(index: usize, thought: &str, signature: &str) -> Script {
    vec![
        Ok(ProviderEvent::BlockStart {
            index,
            block: ContentBlock::Thinking {
                thinking: String::new(),
                signature: String::new(),
            },
        }),
        Ok(ProviderEvent::BlockDelta {
            index,
            delta: Delta::Thinking(thought.into()),
        }),
        Ok(ProviderEvent::BlockDelta {
            index,
            delta: Delta::Signature(signature.into()),
        }),
        Ok(ProviderEvent::BlockStop { index }),
    ]
}

pub(crate) fn tool_use(index: usize, id: &str, name: &str, parts: &[&str]) -> Script {
    let mut script = vec![Ok(ProviderEvent::BlockStart {
        index,
        block: ContentBlock::ToolUse {
            id: id.into(),
            name: name.into(),
            input: json!({}),
        },
    })];
    script.extend(parts.iter().map(|part| {
        Ok(ProviderEvent::BlockDelta {
            index,
            delta: Delta::InputJson((*part).into()),
        })
    }));
    script.push(Ok(ProviderEvent::BlockStop { index }));
    script
}

pub(crate) fn opaque(index: usize, raw: Value) -> Script {
    vec![
        Ok(ProviderEvent::BlockStart {
            index,
            block: ContentBlock::opaque("anthropic", raw),
        }),
        Ok(ProviderEvent::BlockStop { index }),
    ]
}

pub(crate) fn fallback_marker(from: &str, to: &str) -> Value {
    json!({"type": "fallback", "from": {"model": from}, "to": {"model": to}})
}

pub(crate) fn body_of(request: &Request<'_>) -> Value {
    serde_json::from_slice(&anthropic::build_body(request).unwrap()).unwrap()
}

pub(crate) fn text_of(request: &Request<'_>) -> String {
    String::from_utf8(anthropic::build_body(request).unwrap()).unwrap()
}

pub(crate) fn misplaced_system(body: &Value) -> Option<usize> {
    let shapes = body["messages"].as_array()?.iter().map(|m| {
        let content = m["content"].as_array().is_some_and(|c| !c.is_empty());
        (m["role"].as_str().unwrap_or_default(), content)
    });
    anthropic::misplaced_system(shapes).map(|(at, _)| at)
}

fn broken(why: &str) -> ! {
    eprintln!("the fake kit stops the test: {why}");
    std::process::abort()
}

pub(crate) async fn turn(
    agent: &mut Agent,
    text: &str,
    images: &[&str],
) -> (TurnOutcome, Vec<AgentEvent>) {
    let (_tx, mut rx) = mpsc::channel::<ClientCommand>(4);
    let (ev_tx, mut ev_rx) = mpsc::channel::<AgentEvent>(1024);
    let images = images
        .iter()
        .map(|d| ImageData { data: (*d).into() })
        .collect();
    let outcome = within(
        "the turn's end",
        agent.run_user_turn(text.into(), images, &mut rx, &ev_tx),
    )
    .await
    .unwrap();
    drop(ev_tx);
    let mut events = Vec::new();
    while let Some(e) = ev_rx.recv().await {
        events.push(e);
    }
    (outcome, events)
}

pub(crate) async fn stream_error(provider: &dyn Provider, request: &Request<'_>) -> ProviderError {
    let items: Vec<_> = provider.stream(request).collect().await;
    match items.last() {
        Some(Err(e)) => e.clone(),
        other => panic!("{other:?}"),
    }
}

fn without(body: &str, start: &str) -> String {
    let mut out = String::new();
    let mut rest = body;
    while let Some(at) = rest.find(start) {
        out.push_str(&rest[..at]);
        let after = &rest[at..];
        let end = after.find("\\n").map_or(after.len(), |e| e + 2);
        out.push_str("<line>");
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

pub(crate) fn without_stamps(body: &str) -> String {
    without(&without(body, "Sent: "), "Context: ")
}

pub(crate) fn seeing(inner: &Fake, caps: Option<&'static ModelCapabilities>) -> Arc<Fake> {
    inner.view(|k| {
        k.caps = Some(Caps::Fixed(caps));
    })
}

pub(crate) fn custom_caps(
    images: Option<crate::provider::ImageCaps>,
) -> Option<&'static ModelCapabilities> {
    let row = ModelCapabilities {
        images,
        ..*capabilities::row(crate::agent::DEFAULT_MODEL).unwrap()
    };
    Some(Box::leak(Box::new(row)))
}

pub(crate) fn seeing_connection(
    name: &str,
    scripted: &Arc<Fake>,
    caps: Option<&'static ModelCapabilities>,
) -> crate::agent::Connection {
    crate::agent::Connection {
        name: name.to_owned(),
        provider: seeing(scripted, caps),
        context: crate::agent::LogContext {
            endpoint_name: name.to_owned(),
            ..test_guard::log_context()
        },
        priced: true,
    }
}

pub(crate) fn noted(events: &[AgentEvent]) -> Vec<(String, usize, usize)> {
    crate::test_guard::pick!(events, AgentEvent::HistoryAdapted {
        change: wisp_protocol::HistoryChange::AttachmentsAsNotes { model, images, files },
        ..
    } => (model.clone(), *images, *files))
}

pub(crate) fn withdrawn(events: &[AgentEvent]) -> Vec<WithdrawnAttachment> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::AttachmentsWithdrawn { items, .. } => Some(items.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

pub(crate) fn completed(outcome: &TurnOutcome) {
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
}

pub(crate) async fn assert_tool_output_redacted(
    agent: &mut Agent,
    events: &mpsc::Sender<AgentEvent>,
    store: &MemoryStore,
    secrets: &[impl AsRef<str>],
) {
    let secrets: Vec<&str> = secrets.iter().map(AsRef::as_ref).collect();
    let mut finished = AgentEvent::call_finished(
        "retired-key",
        wisp_protocol::ToolOutcome::Completed {
            is_error: false,
            exit: None,
        },
    );
    if let AgentEvent::ToolCallFinished { output, .. } = &mut finished {
        output.tail = secrets.join(" ");
    }
    agent.log_event(events, &finished, Some(1)).await;
    let id = &agent.session_id;
    let stored = serde_json::to_string(&store.read(id).unwrap()).unwrap();
    for secret in &secrets {
        assert!(!stored.contains(secret), "{id} leaked a credential");
    }
    assert!(stored.contains(&vec![REDACTED; secrets.len()].join(" ")));
}

pub(crate) fn search_tool(version: &str) -> &'static [ServerToolSpec] {
    std::slice::from_ref(crate::search::tool(version).unwrap())
}

#[tokio::test]
async fn a_sent_request_rebuilds_the_body_beta_and_count_body_it_was_sent_with() {
    let mut session = crate::agent::default_session();
    session.system = Some("be brief".into());
    let spec = |name: &str| ToolSpec {
        description: format!("{name} tool"),
        input_schema: serde_json::json!({"type": "object"}),
        name: name.into(),
    };
    let messages = [
        Message::user_text("hi"),
        Message {
            role: wisp_protocol::Role::Assistant,
            content: vec![
                ContentBlock::Thinking {
                    thinking: "thought 0".into(),
                    signature: "sig0+/==".into(),
                },
                ContentBlock::Text {
                    text: "hello".into(),
                },
            ],
        },
        Message::user_text("again"),
    ];
    let rendering = Rendering {
        form: wisp_protocol::RenderingForm::WithoutThinking,
        provisional: true,
        ..crate::provider::UNRENDERED.clone()
    };
    let compaction = CompactionAsk {
        native_instructions: Some("summarise".into()),
        tail: None,
    };
    let request = Request {
        session: &session,
        tools: &[spec("echo")],
        deferred: &[spec("later")],
        by_value: &[spec("added")],
        server_tools: search_tool(wisp_protocol::settings::CLAUDE_WEB_SEARCH),
        messages: &messages,
        previous_response_id: Some("resp_1"),
        features: Features {
            per_message_effort: Some(crate::provider::FeatureUse::Established),
            deferred_tools: Some(crate::provider::FeatureUse::Trial),
            documented: true,
            ..Features::default()
        },
        options: RequestOptions {
            debug_cache: true,
            refusal_fallback: false,
            message_threads: true,
        },
        rendering: &rendering,
        compaction: Some(&compaction),
    };
    let fake = Fake::with(
        Knobs {
            count: Some(1),
            ..Knobs::default()
        },
        [Step::default()],
    );
    drop(fake.stream(&request));
    fake.count_tokens(&request).await;
    let built = String::from_utf8(anthropic::build_body(&request).unwrap()).unwrap();
    assert_eq!(fake.texts(), [built.as_str()]);
    assert_eq!(fake.body(0), serde_json::from_str::<Value>(&built).unwrap());
    assert_eq!(fake.beta(0), anthropic::beta_header(&request));
    assert_eq!(
        fake.counted()[0].count_body(),
        anthropic::count_body(&request).unwrap()
    );
}

#[tokio::test]
async fn a_step_plays_sending_the_wait_its_events_its_actions_its_error_then_hangs() {
    let gate = Arc::new(Semaphore::new(0));
    let ran = Arc::new(AtomicUsize::new(0));
    let counter = ran.clone();
    let from = wisp_protocol::AgentPath::new("/parent");
    let letter = Mail {
        id: wisp_protocol::MailId("m1".into()),
        from: from.clone(),
        to: wisp_protocol::AgentPath::new("/"),
        kind: wisp_protocol::MailKind::Message,
        wake: false,
        text: "note".into(),
    };
    let error = failure(ErrorKind::Server, RetryClass::Never, false);
    let step = Step::from(vec![Ok(ProviderEvent::MessageStop), Err(error.clone())])
        .gated(&gate)
        .then(ClientCommand::Cancel)
        .mail(letter)
        .run(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        })
        .hanging();
    let fake = Fake::new([step]);
    let (tx, mut rx) = mpsc::channel(4);
    let mailbox = crate::agent::mail::Mailbox::default();
    fake.send_to(tx);
    fake.mail_to(mailbox.sender());
    let session = crate::agent::default_session();
    let request = Request {
        session: &session,
        tools: &[],
        deferred: &[],
        by_value: &[],
        server_tools: &[],
        messages: &[],
        previous_response_id: None,
        features: Features::default(),
        options: RequestOptions::default(),
        rendering: &crate::provider::UNRENDERED,
        compaction: None,
    };
    let mut played = fake.stream(&request);
    assert!(matches!(
        played.next().await,
        Some(Ok(ProviderEvent::Sending))
    ));
    assert!(played.next().now_or_never().is_none());
    assert_eq!(ran.load(Ordering::SeqCst), 0);
    gate.add_permits(1);
    assert!(matches!(
        played.next().await,
        Some(Ok(ProviderEvent::MessageStop))
    ));
    assert_eq!(played.next().await, Some(Err(error)));
    assert_eq!(rx.try_recv().unwrap(), ClientCommand::Cancel);
    assert_eq!(mailbox.sender().waiting_from(&from), 1);
    assert_eq!(ran.load(Ordering::SeqCst), 1);
    assert_eq!(gate.available_permits(), 1);
    assert!(played.next().now_or_never().is_none());
    let taken = Arc::new(Semaphore::new(1));
    let held = Fake::new([Step::default().held(Wait::Take(taken.clone()))]);
    assert!(held.stream(&request).next().await.is_none());
    assert_eq!(taken.available_permits(), 0);
    drop(fake.plan_usage());
    assert_eq!(fake.plan_usage_calls(), 1);
}
