use std::collections::BTreeSet;
use std::time::Duration;

use wisp_plugin_api::host::{
    Fill as ApiFill, FillPart as ApiFillPart, PartTokens as ApiPartTokens,
    TokenCount as ApiTokenCount,
};
use wisp_plugin_api::pricing::Usage as ApiUsage;
use wisp_plugin_api::tools::{Effect, Purpose, Tags, ToolMeta as ApiMeta};
use wisp_plugin_api::tree;
use wisp_plugin_api::types as api;
use wisp_protocol::{
    Answer, BackgroundReason, ExitStatus, Fact, Mime, OutputKind, Question, QuestionOption,
    SignalKind, TodoItem, TodoStatus, ToolAction, ToolDisplay, ToolOutcome, ToolPurpose, ToolView,
    Typed, Usage, Verbs, WorkspaceChange,
};

use crate::agents::news::{StopNews, TreeNews};
use crate::provider::TokenCount;
use crate::tool::{Attachment, EffectClass, Tag, Timeouts, ToolMeta, ToolRun};

pub fn usage(usage: ApiUsage) -> Usage {
    Usage {
        input_tokens: usage.input,
        output_tokens: usage.output,
        cache_read_input_tokens: usage.cache_read,
        cache_creation_input_tokens: usage.cache_write_5m.saturating_add(usage.cache_write_1h),
        cache_creation_5m_input_tokens: Some(usage.cache_write_5m),
        cache_creation_1h_input_tokens: Some(usage.cache_write_1h),
        web_search_requests: usage.web_searches,
        ..Usage::default()
    }
}

pub fn todo(item: wisp_plugin_api::host::Todo) -> TodoItem {
    use wisp_plugin_api::host::TodoStatus as Api;
    TodoItem {
        text: item.text,
        status: match item.status {
            Api::Pending => TodoStatus::Pending,
            Api::InProgress => TodoStatus::InProgress,
            Api::Completed => TodoStatus::Completed,
            Api::Cancelled => TodoStatus::Cancelled,
        },
        section: item.section,
    }
}

pub fn signal(kind: api::SignalKind) -> SignalKind {
    match kind {
        api::SignalKind::Interrupt => SignalKind::Interrupt,
        api::SignalKind::Kill => SignalKind::Kill,
    }
}

pub fn signal_to_api(kind: SignalKind) -> api::SignalKind {
    match kind {
        SignalKind::Interrupt => api::SignalKind::Interrupt,
        SignalKind::Kill => api::SignalKind::Kill,
    }
}

pub fn action(action: api::Action) -> ToolAction {
    match action {
        api::Action::Run => ToolAction::Run,
        api::Action::Read => ToolAction::Read,
        api::Action::Write => ToolAction::Write,
        api::Action::Edit => ToolAction::Edit,
        api::Action::Search => ToolAction::Search,
        api::Action::Fetch => ToolAction::Fetch,
        api::Action::Input => ToolAction::Input,
        api::Action::Wait => ToolAction::Wait,
        api::Action::Attach => ToolAction::Attach,
        api::Action::Show => ToolAction::Show,
        api::Action::Signal(kind) => ToolAction::Signal { kind: signal(kind) },
        api::Action::Other(name) => ToolAction::Other { name },
    }
}

pub fn action_to_api(action: &ToolAction) -> api::Action {
    match action {
        ToolAction::Run => api::Action::Run,
        ToolAction::Read => api::Action::Read,
        ToolAction::Write => api::Action::Write,
        ToolAction::Edit => api::Action::Edit,
        ToolAction::Search => api::Action::Search,
        ToolAction::Fetch => api::Action::Fetch,
        ToolAction::Input => api::Action::Input,
        ToolAction::Wait => api::Action::Wait,
        ToolAction::Attach => api::Action::Attach,
        ToolAction::Show => api::Action::Show,
        ToolAction::Signal { kind } => api::Action::Signal(signal_to_api(*kind)),
        ToolAction::Other { name } => api::Action::Other(name.clone()),
        ToolAction::Unknown => api::Action::Other(String::new()),
    }
}

fn verbs(verbs: api::Verbs) -> Verbs {
    Verbs {
        doing: verbs.doing,
        done: verbs.done,
        failed: verbs.failed,
    }
}

fn verbs_to_api(verbs: &Verbs) -> api::Verbs {
    api::Verbs {
        doing: verbs.doing.clone(),
        done: verbs.done.clone(),
        failed: verbs.failed.clone(),
    }
}

fn output(kind: api::OutputKind) -> OutputKind {
    match kind {
        api::OutputKind::Text => OutputKind::Text,
        api::OutputKind::Code(lang) => OutputKind::Code { lang },
        api::OutputKind::Diff => OutputKind::Diff,
        api::OutputKind::None => OutputKind::None,
    }
}

fn output_to_api(kind: &OutputKind) -> api::OutputKind {
    match kind {
        OutputKind::Text | OutputKind::Unknown => api::OutputKind::Text,
        OutputKind::Code { lang } => api::OutputKind::Code(lang.clone()),
        OutputKind::Diff => api::OutputKind::Diff,
        OutputKind::None => api::OutputKind::None,
    }
}

fn view(view: api::View) -> Result<ToolView, String> {
    Ok(match view {
        api::View::Diff(diff) => ToolView::Diff {
            title: diff.title,
            files: diff
                .files
                .into_iter()
                .map(|f| wisp_protocol::DiffView {
                    path: f.path,
                    old_path: f.old_path,
                    language: f.language,
                    hunks: f.hunks,
                    added: f.added,
                    removed: f.removed,
                })
                .collect(),
        },
        api::View::Process(p) => ToolView::Process {
            id: p.id,
            text: p.text,
            keys: p.keys.iter().map(api::Key::name).collect(),
            signal: p.signal.map(signal),
            read: p.read.map(|read| read.name().to_owned()),
            pattern: p.pattern,
        },
        api::View::Code(c) => {
            wisp_tool_text::code::check(&c.code)?;
            ToolView::Code {
                language: c.language,
                code: c.code,
            }
        }
    })
}

fn view_to_api(view: &ToolView) -> Option<api::View> {
    Some(match view {
        ToolView::Diff { title, files } => api::View::Diff(api::DiffView {
            title: title.clone(),
            files: files
                .iter()
                .map(|f| api::DiffFile {
                    path: f.path.clone(),
                    old_path: f.old_path.clone(),
                    language: f.language.clone(),
                    hunks: f.hunks.clone(),
                    added: f.added,
                    removed: f.removed,
                })
                .collect(),
        }),
        ToolView::Process {
            id,
            text,
            keys,
            signal,
            read,
            pattern,
        } => api::View::Process(api::ProcessView {
            id: *id,
            text: text.clone(),
            keys: keys.iter().filter_map(|key| api::Key::parse(key)).collect(),
            signal: signal.map(signal_to_api),
            read: read.as_deref().and_then(api::ReadKind::named),
            pattern: pattern.clone(),
        }),
        ToolView::Code { language, code } => api::View::Code(api::CodeView {
            language: language.clone(),
            code: code.clone(),
        }),
        ToolView::Unknown => return None,
    })
}

pub fn display(display: api::Display) -> Result<ToolDisplay, String> {
    Ok(ToolDisplay {
        action: action(display.action),
        verbs: display.verbs.map(verbs),
        target: display.target,
        output: output(display.output),
        view: display.view.map(view).transpose()?,
        title: display.title,
        purpose: ToolPurpose::Run,
    })
}

pub fn display_to_api(display: &ToolDisplay) -> api::Display {
    api::Display {
        action: action_to_api(&display.action),
        verbs: display.verbs.as_ref().map(verbs_to_api),
        target: display.target.clone(),
        output: output_to_api(&display.output),
        view: display.view.as_ref().and_then(view_to_api),
        title: display.title.clone(),
    }
}

fn exit(status: api::ExitStatus) -> ExitStatus {
    match status {
        api::ExitStatus::Code(code) => ExitStatus::Code { code },
        api::ExitStatus::Signal(signal) => ExitStatus::Signal { signal },
    }
}

fn background(reason: api::BackgroundReason) -> BackgroundReason {
    match reason {
        api::BackgroundReason::Started => BackgroundReason::Started,
        api::BackgroundReason::Quiet => BackgroundReason::Quiet,
        api::BackgroundReason::MovedByUser => BackgroundReason::MovedByUser,
        api::BackgroundReason::StillRunning => BackgroundReason::StillRunning,
    }
}

pub fn outcome(outcome: api::Outcome) -> ToolOutcome {
    match outcome {
        api::Outcome::Completed(done) => ToolOutcome::Completed {
            is_error: done.is_error,
            exit: done.exit.map(exit),
        },
        api::Outcome::TimedOut(after_ms) => ToolOutcome::TimedOut { after_ms },
        api::Outcome::Cancelled => ToolOutcome::Cancelled,
        api::Outcome::InvalidInput(message) => ToolOutcome::InvalidInput { message },
        api::Outcome::Failed(failure) => ToolOutcome::Failed {
            message: failure.message,
            before_start: failure.before_start,
        },
        api::Outcome::Backgrounded(reason) => ToolOutcome::Backgrounded {
            reason: background(reason),
        },
    }
}

fn mime(text: &str) -> Result<Mime, String> {
    text.parse::<Mime>()
        .map_err(|_| format!("`{text}` is not a media type"))
}

fn fact(fact: api::Fact) -> Result<Fact, String> {
    Ok(match fact {
        api::Fact::Exit(code) => Fact::Exit { code },
        api::Fact::Lines(n) => Fact::Lines { n },
        api::Fact::Bytes(n) => Fact::Bytes { n },
        api::Fact::Matches(m) => Fact::Matches {
            n: m.n,
            files: m.files,
        },
        api::Fact::Diffstat(d) => Fact::Diffstat {
            added: d.added,
            removed: d.removed,
        },
        api::Fact::Process(id) => Fact::Process { id },
        api::Fact::Media(media_type) => Fact::Media {
            media_type: mime(&media_type)?,
        },
        api::Fact::Part(p) => Fact::Part {
            sent: p.sent,
            total: p.total,
            more_than: p.more_than,
        },
        api::Fact::Tokens(n) => Fact::Tokens { n },
        api::Fact::Hits(hits) => {
            let (mut tools, mut skills, mut documents) = (0, 0, 0);
            for hit in hits {
                match hit.kind {
                    wisp_plugin_api::index::Kind::Tool => tools += 1,
                    wisp_plugin_api::index::Kind::Skill => skills += 1,
                    wisp_plugin_api::index::Kind::Document => documents += 1,
                }
            }
            Fact::Hits {
                tools,
                skills,
                documents,
            }
        }
    })
}

fn typed(typed: api::Typed) -> Typed {
    Typed {
        echoed: typed.echoed,
        unsent: typed.unsent,
        signalled: typed.signalled,
    }
}

pub fn run(run: api::ToolRun) -> Result<ToolRun, String> {
    let hits = run
        .facts
        .iter()
        .filter_map(|fact| match fact {
            api::Fact::Hits(hits) => Some(hits.as_slice()),
            _ => None,
        })
        .flatten()
        .cloned()
        .collect();
    let facts = run
        .facts
        .into_iter()
        .map(fact)
        .collect::<Result<Vec<_>, _>>()?;
    let attachments = run
        .attachments
        .into_iter()
        .map(|a| {
            Ok(Attachment {
                media_type: mime(&a.media_type)?,
                bytes: a.bytes,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(ToolRun {
        outcome: outcome(run.outcome),
        hits,
        facts,
        state: run.cwd.map(|path| WorkspaceChange::Cwd { path }),
        trailer: run.trailer,
        result: run.result,
        attachments,
        typed: run.typed.map(typed),
    })
}

fn effect(effect: Effect) -> EffectClass {
    match effect {
        Effect::ReadOnly => EffectClass::ReadOnly,
        Effect::IdempotentKeyed => EffectClass::IdempotentKeyed,
        Effect::LocalTxn => EffectClass::LocalTxn,
        Effect::NonIdempotent => EffectClass::NonIdempotent,
    }
}

fn tags(tags: Tags) -> BTreeSet<Tag> {
    [
        (tags.reversible, Tag::Reversible),
        (tags.irreversible, Tag::Irreversible),
        (tags.external_message, Tag::ExternalMessage),
        (tags.reads_untrusted, Tag::ReadsUntrusted),
        (tags.network, Tag::Network),
    ]
    .into_iter()
    .filter_map(|(on, tag)| on.then_some(tag))
    .collect()
}

pub fn purpose(purpose: Purpose) -> ToolPurpose {
    match purpose {
        Purpose::Run => ToolPurpose::Run,
        Purpose::Show => ToolPurpose::Show,
    }
}

pub fn meta(meta: ApiMeta) -> ToolMeta {
    ToolMeta {
        effect: effect(meta.effect),
        tags: tags(meta.tags),
        parallel_safe: meta.parallel_safe,
        timeout: Timeouts {
            default: Duration::from_millis(meta.timeouts.default_ms),
            max: meta.timeouts.max_ms.map(Duration::from_millis),
        },
        saves_output: meta.saves_output,
        needs_workspace: meta.needs_workspace,
        ends_reply: meta.ends_reply,
        asks_user: meta.asks_user,
        reports_jobs: meta.reports_jobs,
        writing: meta.writing,
        purpose: purpose(meta.purpose),
    }
}

pub fn question(question: api::Question) -> Question {
    Question {
        header: question.header,
        question: question.question,
        options: question
            .options
            .into_iter()
            .map(|o| QuestionOption {
                label: o.label,
                description: o.description,
            })
            .collect(),
        multi_select: question.multi_select,
        source: None,
    }
}

pub fn question_to_api(question: &Question) -> api::Question {
    api::Question {
        header: question.header.clone(),
        question: question.question.clone(),
        options: question
            .options
            .iter()
            .map(|o| api::QuestionOption {
                label: o.label.clone(),
                description: o.description.clone(),
            })
            .collect(),
        multi_select: question.multi_select,
    }
}

pub fn answer_to_api(answer: &Answer) -> api::Answer {
    api::Answer {
        chosen: answer.chosen.clone(),
        own: answer.own.clone(),
    }
}

pub fn token_count(count: TokenCount) -> ApiTokenCount {
    match count {
        TokenCount::Counted { tokens } => ApiTokenCount::Counted(tokens),
        TokenCount::Unsupported => ApiTokenCount::Unsupported,
        TokenCount::Failed { reason } => ApiTokenCount::Failed(reason),
    }
}

pub fn identity(entry: &wisp_protocol::AgentEntry, root_session_id: String) -> tree::Identity {
    tree::Identity {
        path: entry.path.to_string(),
        name: entry.name.clone(),
        parent: entry.parent.as_ref().map(ToString::to_string),
        preset: entry.preset.clone(),
        depth: entry.depth,
        session_id: entry.session_id.clone(),
        root_session_id,
    }
}

fn queue_reason(reason: wisp_protocol::QueueReason) -> tree::QueueReason {
    use wisp_protocol::QueueReason as Q;
    match reason {
        Q::Concurrency => tree::QueueReason::Concurrency,
        Q::Stagger => tree::QueueReason::Stagger,
        Q::Endpoint => tree::QueueReason::Endpoint,
        Q::Retry => tree::QueueReason::Retry,
        Q::Unknown => tree::QueueReason::Unknown,
    }
}

fn hold_reason(reason: wisp_protocol::HoldReason) -> tree::HoldReason {
    use wisp_protocol::HoldReason as H;
    match reason {
        H::Stopped => tree::HoldReason::Stopped,
        H::HeldMessages => tree::HoldReason::HeldMessages,
        H::RetryOffer => tree::HoldReason::RetryOffer,
        H::Concurrency => tree::HoldReason::Concurrency,
        H::Unknown => tree::HoldReason::Unknown,
    }
}

fn idle_outcome(outcome: wisp_protocol::IdleOutcome) -> tree::IdleOutcome {
    use wisp_protocol::IdleOutcome as I;
    match outcome {
        I::Reported => tree::IdleOutcome::Reported,
        I::Asked => tree::IdleOutcome::Asked,
        I::Waiting => tree::IdleOutcome::Waiting,
        I::NoReport => tree::IdleOutcome::NoReport,
        I::InvalidReport => tree::IdleOutcome::InvalidReport,
        I::Failed => tree::IdleOutcome::Failed,
        I::Stopped => tree::IdleOutcome::Stopped,
        I::Unknown => tree::IdleOutcome::Unknown,
    }
}

fn agent_state(state: wisp_protocol::AgentState) -> tree::AgentState {
    use wisp_protocol::AgentState as S;
    match state {
        S::Queued { reason } => tree::AgentState::Queued(queue_reason(reason)),
        S::Running => tree::AgentState::Running,
        S::Stopping => tree::AgentState::Stopping,
        S::Idle { outcome } => tree::AgentState::Idle(outcome.map(idle_outcome)),
        S::Held { reason } => tree::AgentState::Held(hold_reason(reason)),
        S::Ended => tree::AgentState::Ended,
        S::Unknown => tree::AgentState::Unknown,
    }
}

fn activity(activity: wisp_protocol::Activity) -> tree::Activity {
    use wisp_protocol::Activity as A;
    match activity {
        A::Idle | A::Waiting { .. } => tree::Activity::Idle,
        A::Streaming => tree::Activity::Streaming,
        A::Tool { name } => tree::Activity::Tool(name),
        A::Queued { reason } => tree::Activity::Queued(queue_reason(reason)),
        A::Held { reason } => tree::Activity::Held(hold_reason(reason)),
        A::Unknown => tree::Activity::Unknown,
    }
}

pub fn node(entry: wisp_protocol::AgentEntry) -> tree::AgentNode {
    tree::AgentNode {
        path: entry.path.to_string(),
        name: entry.name,
        parent: entry.parent.as_ref().map(ToString::to_string),
        session_id: entry.session_id,
        preset: entry.preset,
        model: wisp_protocol::shown_model(&entry.model).to_owned(),
        effort: entry.effort.as_str().to_owned(),
        endpoint: entry
            .endpoint
            .as_deref()
            .map(|e| crate::agents::endpoints::plain(e).to_owned()),
        provider: entry.provider,
        depth: entry.depth,
        state: agent_state(entry.state),
        activity: activity(entry.activity),
        since_ms: entry.since_ms.unwrap_or_default(),
        spent_micros: crate::pricing::micros(entry.spent_usd),
        lower_bound: entry.lower_bound,
        priced: entry.priced,
        checklist: entry.checklist.map(|p| tree::Progress {
            completed: u32::try_from(p.completed).unwrap_or(u32::MAX),
            total: u32::try_from(p.total).unwrap_or(u32::MAX),
        }),
        cwd: entry.cwd,
        running_ms: entry.running_ms,
        context: entry.context.map(|c| tree::AgentFill {
            used: c.used,
            window: c.window,
            limit: c.limit,
            estimated: c.estimated,
        }),
    }
}

pub fn slot_view(view: wisp_protocol::slot::SlotView) -> wisp_plugin_api::slots::SlotView {
    use wisp_protocol::slot::SlotView as Wire;
    match view {
        Wire::Page => wisp_plugin_api::slots::SlotView::Page,
        Wire::Panel | Wire::Unknown => wisp_plugin_api::slots::SlotView::Panel,
    }
}

pub fn slot_content(content: wisp_plugin_api::slots::Content) -> wisp_protocol::slot::SlotContent {
    wisp_protocol::slot::SlotContent {
        note: slot_spans(content.note),
        blocks: content.blocks.into_iter().map(slot_block).collect(),
    }
}

fn slot_block(block: wisp_plugin_api::slots::Block) -> wisp_protocol::slot::SlotBlock {
    use wisp_plugin_api::slots::Block as Api;
    use wisp_protocol::slot::{SlotBlock, SlotNode, SlotRow};
    match block {
        Api::Line(spans) => SlotBlock::Line {
            spans: slot_spans(spans),
        },
        Api::Rows(rows) => SlotBlock::Rows {
            rows: rows
                .into_iter()
                .map(|row| SlotRow {
                    key: row.key,
                    value: slot_spans(row.value),
                })
                .collect(),
        },
        Api::Gauge(gauge) => SlotBlock::Gauge {
            label: gauge.label,
            value: gauge.value,
            max: gauge.max,
            tone: slot_tone(gauge.tone),
            note: slot_spans(gauge.note),
        },
        Api::Spark(spark) => SlotBlock::Spark {
            label: spark.label,
            values: spark.values,
            max: spark.max,
            tone: slot_tone(spark.tone),
            note: slot_spans(spark.note),
        },
        Api::List(items) => SlotBlock::List {
            items: items.into_iter().map(slot_spans).collect(),
        },
        Api::Tree(nodes) => SlotBlock::Tree {
            nodes: nodes
                .into_iter()
                .map(|node| SlotNode {
                    depth: node.depth,
                    state: slot_state(node.state),
                    role: node.role,
                    label: node.label,
                    facts: slot_spans(node.facts),
                })
                .collect(),
        },
        Api::NextColumn => SlotBlock::NextColumn,
        Api::Fill(fill) => SlotBlock::Fill {
            fill: slot_fill(fill),
        },
        Api::Topology(topology) => SlotBlock::Topology {
            root: topology.root,
            nodes: topology
                .nodes
                .into_iter()
                .map(|node| wisp_protocol::slot::SlotTopologyNode {
                    key: node.key,
                    parent: node.parent,
                    state: slot_state(node.state),
                })
                .collect(),
            flows: topology
                .flows
                .into_iter()
                .map(|flow| wisp_protocol::slot::SlotFlow {
                    from: flow.from,
                    to: flow.to,
                    at_ms: flow.at_ms,
                })
                .collect(),
            counted: topology.counted.map(|noun| match noun {
                wisp_plugin_api::slots::Noun::Agents => wisp_protocol::slot::SlotNoun::Agents,
            }),
        },
    }
}

pub fn fill_parts(parts: Vec<wisp_protocol::context::PartTokens>) -> Vec<ApiPartTokens> {
    use wisp_protocol::context::FillPart as Wire;
    parts
        .into_iter()
        .filter_map(|tokens| {
            let part = match tokens.part {
                Wire::System => ApiFillPart::System,
                Wire::Tools => ApiFillPart::Tools,
                Wire::Chat => ApiFillPart::Chat,
                Wire::Skills => ApiFillPart::Skills,
                Wire::Unknown => return None,
            };
            Some(ApiPartTokens {
                part,
                tokens: tokens.tokens,
            })
        })
        .collect()
}

fn slot_fill(fill: ApiFill) -> wisp_protocol::slot::SlotFill {
    use wisp_protocol::context::{FillPart, PartTokens, limit_of};
    wisp_protocol::slot::SlotFill {
        used: fill.measured.unwrap_or(fill.estimate),
        estimated: fill.measured.is_none(),
        window: fill.window,
        limit: limit_of(fill.point, fill.ceiling, fill.auto_compact),
        auto_compact: fill.auto_compact,
        parts: fill
            .parts
            .into_iter()
            .map(|tokens| PartTokens {
                part: match tokens.part {
                    ApiFillPart::System => FillPart::System,
                    ApiFillPart::Tools => FillPart::Tools,
                    ApiFillPart::Chat => FillPart::Chat,
                    ApiFillPart::Skills => FillPart::Skills,
                },
                tokens: tokens.tokens,
            })
            .collect(),
        cached: fill.cached,
    }
}

pub fn command_lines(
    lines: Vec<wisp_plugin_api::commands::CommandLine>,
) -> Vec<wisp_protocol::CommandLine> {
    use wisp_plugin_api::commands::CommandLine as Api;
    use wisp_protocol::CommandLine;
    lines
        .into_iter()
        .map(|line| match line {
            Api::Row(row) => CommandLine::Row {
                left: row.left.into_iter().map(slot_text).collect(),
                right: row.right.into_iter().map(slot_text).collect(),
            },
            Api::Text(spans) => CommandLine::Text {
                spans: spans.into_iter().map(slot_text).collect(),
            },
        })
        .collect()
}

fn slot_text(text: wisp_plugin_api::slots::TextSpan) -> wisp_protocol::slot::SlotSpan {
    wisp_protocol::slot::SlotSpan::Text {
        text: text.text,
        tone: slot_tone(text.tone),
        bold: text.bold,
    }
}

fn slot_spans(spans: Vec<wisp_plugin_api::slots::Span>) -> Vec<wisp_protocol::slot::SlotSpan> {
    use wisp_plugin_api::slots::Span as Api;
    use wisp_protocol::slot::SlotSpan;
    spans
        .into_iter()
        .map(|span| match span {
            Api::Text(text) => slot_text(text),
            Api::Elapsed(elapsed) => SlotSpan::Elapsed {
                since_ms: elapsed.since_ms,
                tone: slot_tone(elapsed.tone),
            },
            Api::Usd(usd) => SlotSpan::Usd {
                micros: usd.micros,
                at_least: usd.at_least,
                tone: slot_tone(usd.tone),
            },
            Api::Fill(fill) => SlotSpan::Fill {
                fill: slot_fill(fill),
            },
        })
        .collect()
}

fn slot_tone(tone: wisp_plugin_api::slots::Tone) -> wisp_protocol::slot::SlotTone {
    use wisp_plugin_api::slots::Tone as Api;
    use wisp_protocol::slot::SlotTone;
    match tone {
        Api::Text => SlotTone::Text,
        Api::Muted => SlotTone::Muted,
        Api::Faint => SlotTone::Faint,
        Api::Accent => SlotTone::Accent,
        Api::Ok => SlotTone::Ok,
        Api::Error => SlotTone::Error,
    }
}

fn slot_state(state: wisp_plugin_api::slots::NodeState) -> wisp_protocol::slot::NodeState {
    use wisp_plugin_api::slots::NodeState as Api;
    use wisp_protocol::slot::NodeState;
    match state {
        Api::Working => NodeState::Working,
        Api::Waiting => NodeState::Waiting,
        Api::Done => NodeState::Done,
        Api::Failed => NodeState::Failed,
        Api::Stopped => NodeState::Stopped,
    }
}

fn api_state(state: wisp_protocol::slot::NodeState) -> wisp_plugin_api::slots::NodeState {
    use wisp_plugin_api::slots::NodeState as Api;
    use wisp_protocol::slot::NodeState;
    match state {
        NodeState::Working => Api::Working,
        NodeState::Waiting | NodeState::Unknown => Api::Waiting,
        NodeState::Done => Api::Done,
        NodeState::Failed => Api::Failed,
        NodeState::Stopped => Api::Stopped,
    }
}

pub fn shown_state(entry: &wisp_protocol::AgentEntry) -> tree::ShownState {
    tree::ShownState {
        path: entry.path.to_string(),
        state: api_state(entry.state.node_state()),
        keep_warm: entry.keep_warm.map(warm_row),
    }
}

pub fn warm_row(warm: wisp_protocol::KeepWarm) -> wisp_plugin_api::host::WarmRow {
    wisp_plugin_api::host::WarmRow {
        refreshes: warm.refreshes,
        limit: warm.limit,
        paused: warm.paused,
    }
}

fn mail_kind(kind: wisp_protocol::MailKind) -> tree::MailKind {
    use wisp_protocol::MailKind as K;
    match kind {
        K::Task => tree::MailKind::Task,
        K::Message => tree::MailKind::Message,
        K::Report => tree::MailKind::Report,
        K::Notice => tree::MailKind::Notice,
        K::Unknown => tree::MailKind::Unknown,
    }
}

fn report_outcome(outcome: wisp_protocol::ReportOutcome) -> tree::ReportOutcome {
    use wisp_protocol::ReportOutcome as R;
    match outcome {
        R::Reported => tree::ReportOutcome::Reported,
        R::NoReport => tree::ReportOutcome::NoReport,
        R::InvalidReport => tree::ReportOutcome::InvalidReport,
        R::Unknown => tree::ReportOutcome::Unknown,
    }
}

fn stop_info(news: StopNews) -> tree::StopInfo {
    tree::StopInfo {
        agent: news.agent.to_string(),
        target: news.target.to_string(),
        by: news.by.as_ref().map(ToString::to_string),
    }
}

pub fn tree_event(news: TreeNews) -> tree::TreeEvent {
    match news {
        TreeNews::Spawned(node) => tree::TreeEvent::AgentSpawned(node),
        TreeNews::State(news) => tree::TreeEvent::AgentState(tree::StateChange {
            agent: news.agent.to_string(),
            state: agent_state(news.state),
            activity: activity(news.activity),
            spent_micros: crate::pricing::micros(news.spent_usd),
            lower_bound: news.lower_bound,
        }),
        TreeNews::Mail(news) => tree::TreeEvent::AgentMail(tree::MailInfo {
            id: news.id.to_string(),
            from: news.from.to_string(),
            to: news.to.to_string(),
            kind: mail_kind(news.kind),
            wake: news.wake,
            bytes: news.bytes,
        }),
        TreeNews::Reported(news) => tree::TreeEvent::AgentReported(tree::ReportInfo {
            agent: news.agent.to_string(),
            turn: news.turn,
            outcome: report_outcome(news.outcome),
            cost_micros: news.cost_usd.map(crate::pricing::micros),
            duration_ms: news.duration_ms,
        }),
        TreeNews::Stopped(news) => tree::TreeEvent::AgentStopped(stop_info(news)),
        TreeNews::Ended(news) => tree::TreeEvent::AgentEnded(stop_info(news)),
    }
}

pub fn doctor_row(row: wisp_plugin_api::tools::DoctorRow) -> wisp_protocol::DoctorRow {
    use wisp_plugin_api::tools::DoctorCell as Api;
    use wisp_protocol::DoctorCell;
    wisp_protocol::DoctorRow {
        key: row.key,
        cells: row
            .cells
            .into_iter()
            .map(|cell| match cell {
                Api::Text(text) => DoctorCell::Text { text },
                Api::Bytes(bytes) => DoctorCell::Bytes { bytes },
            })
            .collect(),
    }
}

pub fn screen_report(
    screen: crate::call::user::ScreenReport,
) -> wisp_plugin_api::host::ScreenReport {
    let images = screen.images.iter().filter_map(|image| {
        let bytes = wisp_media::decode_base64(&image.data).ok()?;
        let media_type = match wisp_media::sniff(&bytes)? {
            wisp_media::SourceFormat::Png => wisp_media::MediaType::Png,
            wisp_media::SourceFormat::Jpeg => wisp_media::MediaType::Jpeg,
            _ => return None,
        };
        let media_type = media_type.as_str().to_owned();
        Some(api::Attachment { media_type, bytes })
    });
    wisp_plugin_api::host::ScreenReport {
        report: screen.report.to_string(),
        images: images.collect(),
    }
}

#[cfg(test)]
mod tests;

pub fn setting(entry: &wisp_plugin_api::manifest::Setting) -> wisp_protocol::plugin::Setting {
    use wisp_plugin_api::manifest::{RowDefault, RowKind, Setting, SettingRow, Switch};
    use wisp_protocol::plugin::{RowKind as Kind, Setting as Entry, SettingRow as Row};
    match entry {
        Setting::Name(name) => Entry::Name(name.clone()),
        Setting::Row(SettingRow {
            name,
            label,
            help,
            kind,
            min,
            max,
            default,
        }) => Entry::Row(Row {
            name: name.clone(),
            label: label.clone(),
            help: help.clone(),
            kind: match (kind, min, max, default) {
                (RowKind::Bool, None, None, RowDefault::Bool(default)) => Kind::Bool {
                    default: *default == Switch::On,
                },
                (RowKind::Int, Some(min), Some(max), RowDefault::Int(default)) => Kind::Int {
                    min: *min,
                    max: *max,
                    default: *default,
                },
                (RowKind::Bool | RowKind::Int, _, _, RowDefault::Bool(_) | RowDefault::Int(_)) => {
                    Kind::Unknown
                }
            },
        }),
    }
}
