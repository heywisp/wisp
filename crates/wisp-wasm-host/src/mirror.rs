use wisp_plugin_api as api;

use crate::bindings::exports::wisp::plugin0_1_0 as x1;
use crate::bindings::exports::wisp::plugin0_2_0 as x2;
use crate::bindings::exports::wisp::plugin0_3_0 as x3;
use crate::bindings::wisp::plugin0_1_0 as w1;
use crate::bindings::wisp::plugin0_2_0 as w2;
use crate::bindings::wisp::plugin0_3_0 as w3;

pub(crate) trait Mirror<T> {
    fn mirror(self) -> T;
}

macro_rules! same {
    ($($t:ty),*) => {
        $(impl Mirror<$t> for $t {
            fn mirror(self) -> $t {
                self
            }
        })*
    };
}

same!(String, bool, char, u8, u16, u32, u64, i32, i64, f64, ());

impl<A: Mirror<B>, B> Mirror<Vec<B>> for Vec<A> {
    fn mirror(self) -> Vec<B> {
        self.into_iter().map(Mirror::mirror).collect()
    }
}

impl<A: Mirror<B>, B> Mirror<Option<B>> for Option<A> {
    fn mirror(self) -> Option<B> {
        self.map(Mirror::mirror)
    }
}

impl<A: Mirror<B>, B, E: Mirror<F>, F> Mirror<Result<B, F>> for Result<A, E> {
    fn mirror(self) -> Result<B, F> {
        self.map(Mirror::mirror).map_err(Mirror::mirror)
    }
}

impl<A: Mirror<C>, B: Mirror<D>, C, D> Mirror<(C, D)> for (A, B) {
    fn mirror(self) -> (C, D) {
        (self.0.mirror(), self.1.mirror())
    }
}

impl Mirror<Box<api::tree::AgentNode>> for w2::agent_types::AgentNode {
    fn mirror(self) -> Box<api::tree::AgentNode> {
        Box::new(self.mirror())
    }
}

impl Mirror<w2::agent_types::AgentNode> for Box<api::tree::AgentNode> {
    fn mirror(self) -> w2::agent_types::AgentNode {
        (*self).mirror()
    }
}

impl Mirror<api::types::Attachment> for w1::types::Attachment {
    fn mirror(self) -> api::types::Attachment {
        api::types::Attachment {
            media_type: self.media_type,
            bytes: self.bytes,
            slice: None,
        }
    }
}

impl Mirror<w3::screen_report::ScreenReport> for api::host::ScreenReport {
    fn mirror(self) -> w3::screen_report::ScreenReport {
        w3::screen_report::ScreenReport {
            report: self.report,
            images: self
                .images
                .into_iter()
                .filter(|a| a.slice.is_none())
                .map(|a| w1::types::Attachment {
                    media_type: a.media_type,
                    bytes: a.bytes,
                })
                .collect(),
        }
    }
}

macro_rules! mirror {
    ($($kind:ident $($g:ident)::+ = $($a:ident)::+ { $($body:tt)* };)*) => {
        $(mirror!(@$kind ($($g)::+) ($($a)::+) { $($body)* });)*
    };
    (@record ($($g:ident)::+) ($($a:ident)::+) { $($f:ident),* }) => {
        mirror!(@record_one ($($g)::+) ($($a)::+) { $($f),* });
        mirror!(@record_one ($($a)::+) ($($g)::+) { $($f),* });
    };
    (@record_one ($($from:ident)::+) ($($to:ident)::+) { $($f:ident),* }) => {
        impl Mirror<$($to)::+> for $($from)::+ {
            fn mirror(self) -> $($to)::+ {
                let $($from)::+ { $($f),* } = self;
                $($to)::+ { $($f: $f.mirror()),* }
            }
        }
    };
    (@enum ($($g:ident)::+) ($($a:ident)::+) { $($v:ident),* }) => {
        mirror!(@variant ($($g)::+) ($($a)::+) { $($v),* });
    };
    (@variant ($($g:ident)::+) ($($a:ident)::+) { $($v:ident $(($p:ident))?),* }) => {
        mirror!(@variant_one ($($g)::+) ($($a)::+) { $($v $(($p))?),* });
        mirror!(@variant_one ($($a)::+) ($($g)::+) { $($v $(($p))?),* });
    };
    (@variant_one ($($from:ident)::+) ($($to:ident)::+) { $($v:ident $(($p:ident))?),* }) => {
        impl Mirror<$($to)::+> for $($from)::+ {
            fn mirror(self) -> $($to)::+ {
                type To = $($to)::+;
                match self {
                    $(Self::$v $(($p))? => To::$v $(($p.mirror()))?,)*
                }
            }
        }
    };
    (@flags ($($g:ident)::+) ($($a:ident)::+) { $($f:ident: $c:ident),* }) => {
        impl Mirror<$($a)::+> for $($g)::+ {
            fn mirror(self) -> $($a)::+ {
                $($a)::+ { $($f: self.contains(Self::$c)),* }
            }
        }
        impl Mirror<$($g)::+> for $($a)::+ {
            fn mirror(self) -> $($g)::+ {
                type Flags = $($g)::+;
                let $($a)::+ { $($f),* } = self;
                let mut flags = Flags::empty();
                $(if $f {
                    flags |= Flags::$c;
                })*
                flags
            }
        }
    };
}

mirror! {
    variant_one x1::commands::CommandReply = api::commands::CommandReply { None, Note(a), Send(a) };
    record w1::hook_types::LeadItem = api::hooks::LeadItem { by, text };
    record w1::hook_types::SessionStart = api::hooks::SessionStart { resumed };
    record w1::hook_types::UserInput = api::hooks::UserInput { text, images, steer };
    record w1::hook_types::BeforeTurn = api::hooks::BeforeTurn { turn, lead, typed, images, retry };
    record w1::hook_types::AfterTurn = api::hooks::AfterTurn { turn, outcome };
    record w1::hook_types::StepInfo = api::hooks::StepInfo { turn, step, attempt };
    record w1::hook_types::ResponseInfo = api::hooks::ResponseInfo { turn, step, stop_reason };
    record w1::hook_types::BeforeEnd = api::hooks::BeforeEnd { turn, step, reply, continues };
    record w1::hook_types::ToolCall = api::hooks::ToolCall { call_id, tool, input };
    record w1::hook_types::ToolResult = api::hooks::ToolResult {
        call_id, tool, input, result, is_error
    };
    record w1::hook_types::FillInfo = api::hooks::FillInfo {
        turn, step, before_turn, estimate, measured, window, point, ceiling, auto_compact, epoch
    };
    record w1::hook_types::CompactInfo = api::hooks::CompactInfo {
        trigger, engine, turn, focus, estimate, point, epoch, requested_by, instruction
    };
    record w1::hook_types::HeadInfo = api::hooks::HeadInfo {
        epoch, trigger, engine, first_turn, last_turn
    };
    record w1::hook_types::CompactedInfo = api::hooks::CompactedInfo {
        epoch, trigger, engine, before, after, summary, cost_micros, partial, failed
    };
    record w1::hook_types::RewindInfo = api::hooks::RewindInfo { turn, epoch };
    record w1::hook_types::CommandInfo = api::hooks::CommandInfo { name, args };
    record w1::hook_types::ToolsChange = api::hooks::ToolsChange { added, removed, via };
    record w1::hook_types::ModelSwitch = api::hooks::ModelSwitch { endpoint, model };
    record w1::hook_types::Custom = api::hooks::Custom { point, version, payload };
    variant w1::hook_types::Event = api::hooks::Event {
        SessionStart(a), SessionEnd, UserInput(a), BeforeTurn(a), AfterTurn(a),
        BeforeRequest(a), AfterResponse(a), BeforeEnd(a), BeforeTool(a), AfterTool(a),
        ContextThreshold(a), BeforeCompact(a), CompactionHead(a), AfterCompact(a), Rewound(a),
        Command(a), ToolsChanged(a), SettingsChanged(a), ModelSwitched(a), Custom(a)
    };
    variant w1::hook_types::Reply = api::hooks::Reply {
        None, Append(a), Rewrite(a), Block(a), Continue(a), Compact(a)
    };
    record w1::points::Answer = api::hooks::Answer { hook, reply };
    record w1::session::Facts = api::host::Facts {
        model, endpoint_kind, effort, platform, capabilities
    };
    enum w1::workspace::FileKind = api::host::FileKind {
        Regular, Directory, Fifo, Socket, Device, Other
    };
    record w1::clock::Instant = api::host::Instant { seconds, nanos };
    enum w1::context::FillPart = api::host::FillPart { System, Tools, Chat, Skills };
    record w1::context::PartTokens = api::host::PartTokens { part, tokens };
    record w1::context::Fill = api::host::Fill {
        estimate, measured, window, point, ceiling, auto_compact, epoch, parts, cached
    };
    variant w1::media::TokenCount = api::host::TokenCount { Counted(a), Unsupported, Failed(a) };
    record w1::call::OutputSummary = api::host::OutputSummary { text, bytes, lines, file };
    enum w1::jobs::Mode = api::host::Mode { Pipe, Terminal };
    record w1::jobs::Size = api::host::Size { cols, rows };
    record w1::jobs::SpawnSpec = api::host::SpawnSpec {
        program, args, cwd, env, mode, size, stdin, background, limit_ms, label, keep
    };
    variant w1::jobs::StoppedBy = api::host::StoppedBy { Model, User, SessionEnded, TimeLimit(a) };
    enum w1::jobs::LostReason = api::host::LostReason { StoppedAtResume, Gone };
    variant w1::jobs::JobEnd = api::host::JobEnd { Exited(a), Stopped(a), Lost(a), Failed(a) };
    variant w1::jobs::JobError = api::host::JobError {
        Driven, Ended, NoInput, Closed, LineTooLong(a), Invalid(a), Io(a)
    };
    record w1::jobs::JobInfo = api::host::JobInfo {
        id, call_id, label, end, mode, lines, bytes, restored, echo_off, full_screen, unread,
        limit_ms, left_ms, size, keep, log
    };
    enum w1::jobs::Settled = api::host::Settled { Ended, Quiet, Cancelled };
    record w1::jobs::WaitFor = api::host::WaitFor { timeout_ms, until, quiet_ms };
    record w1::jobs::HandedBack = api::host::HandedBack { id, quiet_ms };
    record w1::jobs::HoldSpec = api::host::HoldSpec { quiet_ms, yield_ms };
    variant w1::jobs::Waited = api::host::Waited {
        Ended(a), Matched, Quiet, TimedOut, Cancelled, Steered, Moved(a), NeedsInput(a),
        StillRunning(a)
    };
    record w1::jobs::Line = api::host::Line { number, text };
    record w1::jobs::Span = api::host::Span { first, last };
    variant w1::jobs::Row = api::host::Row { Line(a), Gap(a) };
    record w1::jobs::WriteFailure = api::host::WriteFailure { error, after };
    record w1::jobs::Capped = api::host::Capped { cap, dropped };
    variant w1::jobs::FileProblem = api::host::FileProblem {
        NotCreated(a), WriteFailed(a), Capped(a)
    };
    record w1::jobs::OutputFile = api::host::OutputFile { path, problem };
    record w1::jobs::Section = api::host::Section { first, last, bytes, lines, head, tail };
    record w1::jobs::Read = api::host::Read { section, complete, held, file };
    record w1::jobs::JobOutput = api::host::JobOutput { head, tail, bytes, lines, file };
    record w1::jobs::Unreadable = api::host::Unreadable { path, error };
    variant w1::jobs::SavedFile = api::host::SavedFile {
        Readable(a), NoneKept, Missing(a), Unreadable(a)
    };
    record w1::jobs::Rows = api::host::Rows { last, complete, rows, cut, held };
    record w1::jobs::FoundLine = api::host::FoundLine { number, text, matched };
    variant w1::jobs::FoundRow = api::host::FoundRow { Line(a), Separator };
    record w1::jobs::Found = api::host::Found { last, rows, shown, remaining, last_shown, gaps };
    record w1::jobs::Point = api::host::Point { row, col };
    record w1::jobs::Screen = api::host::Screen { rows, cursor, size, full_screen, changed };
    record w1::jobs::Typing = api::host::Typing { text, keys, signal, size };
    record w1::jobs::Written = api::host::Written {
        total, left, typed, stopped, failed, after_kill
    };
    variant w1::jobs::Captured = api::host::Captured { NotFound, Failed(a), Output(a) };
    enum w1::todos::TodoStatus = api::host::TodoStatus {
        Pending, InProgress, Completed, Cancelled
    };
    record w1::agents::SpawnInput = api::host::SpawnInput {
        name, task, todos, cwd, preset, endpoint, model, effort, tools_off, spawn, keep_warm
    };
    variant w1::agents::WarmMark = api::host::WarmMark { Limit(a), Unsupported };
    record w1::agents::StartedAgent = api::host::StartedAgent {
        path, model, effort, endpoint, provider, cwd, keep_warm, fell_back
    };
    enum w1::agents::Relation = api::host::Relation { Caller, Parent, Sibling, Child, Descendant };
    record w1::agents::Progress = api::host::Progress { completed, total };
    record w1::agents::WarmRow = api::host::WarmRow { refreshes, limit, paused };
    record w1::agents::AgentRow = api::host::AgentRow {
        path, session, relation, preset, model, effort, endpoint, provider, state, detail,
        spent_micros, billing_note, plan_micros, lower_bound, priced, checklist, last_report,
        cwd, running_ms, context_used, context_window, context_limit, context_estimated,
        keep_warm
    };
    enum w1::agents::Delivery = api::host::Delivery { Running, Woken, Idle, Queued, Held };
    record w1::agents::Sent = api::host::Sent { to, delivery };
    record w1::agents::Stopped = api::host::Stopped { path, below, ended };
    enum w1::types::SignalKind = api::types::SignalKind { Interrupt, Kill };
    enum w1::types::BackgroundReason = api::types::BackgroundReason {
        Started, Quiet, MovedByUser, StillRunning
    };
    enum w1::types::Level = api::types::Level { Debug, Info, Warn, Error };
    variant w1::types::Action = api::types::Action {
        Run, Read, Write, Edit, Search, Fetch, Input, Wait, Attach, Show, Signal(a), Other(a)
    };
    record w1::types::Verbs = api::types::Verbs { doing, done, failed };
    variant w1::types::OutputKind = api::types::OutputKind { Text, Code(a), Diff, None };
    record w1::types::DiffFile = api::types::DiffFile {
        path, old_path, language, hunks, added, removed
    };
    record w1::types::DiffView = api::types::DiffView { title, files };
    enum w1::types::NamedKey = api::types::NamedKey {
        Enter, Tab, Space, Esc, Backspace, Delete, Up, Down, Left, Right, Home, End, PageUp,
        PageDown
    };
    variant w1::types::Key = api::types::Key { Named(a), Function(a), Ctrl(a), Eof };
    enum w1::types::ReadKind = api::types::ReadKind { New, Tail, Lines, Search, Screen };
    record w1::types::ProcessView = api::types::ProcessView {
        id, text, keys, signal, read, pattern
    };
    variant_one w1::types::View = api::types::View { Diff(a), Process(a) };
    record_one w1::types::Display = api::types::Display { action, verbs, target, output, view, title };
    record w1::types::Matches = api::types::Matches { n, files };
    record w1::types::Diffstat = api::types::Diffstat { added, removed };
    record w1::types::Part = api::types::Part { sent, total, more_than };
    variant_one w1::types::Fact = api::types::Fact {
        Exit(a), Lines(a), Bytes(a), Matches(a), Diffstat(a), Process(a), Media(a), Part(a),
        Tokens(a)
    };
    variant w1::types::ExitStatus = api::types::ExitStatus { Code(a), Signal(a) };
    record w1::types::Completed = api::types::Completed { is_error, exit };
    record w1::types::Failure = api::types::Failure { message, before_start };
    variant w1::types::Outcome = api::types::Outcome {
        Completed(a), TimedOut(a), Cancelled, InvalidInput(a), Failed(a), Backgrounded(a)
    };
    record w1::types::Typed = api::types::Typed { echoed, unsent, signalled };
    record_one w1::types::ToolRun = api::types::ToolRun {
        outcome, facts, cwd, trailer, result, attachments, typed
    };
    record w1::types::QuestionOption = api::types::QuestionOption { label, description };
    record w1::types::Question = api::types::Question { header, question, options, multi_select };
    record w1::types::Answer = api::types::Answer { chosen, own };
    variant w1::types::AskReply = api::types::AskReply {
        Answered(a), Attached(a), Declined, ProcessEnded(a), MessageInstead, Unavailable,
        Stopped
    };
    record w2::config::SettingRow = api::config::SettingRow { name, value, layer, help, rule };
    enum w2::accounts::SignIn = api::config::SignIn {
        Working, Expired, Refreshable, Unavailable, ExpiryUnknown
    };
    record w2::accounts::LimitWindow = api::config::LimitWindow {
        label, used_percent, resets_at, observed_at
    };
    record w2::accounts::Limits = api::config::Limits { windows, limited, problem };
    record w2::accounts::AccountRow = api::config::AccountRow {
        name, kind, unavailable, sign_in, yours, start, models, limits
    };
    record w2::accounts::AccountList = api::config::AccountList { model, effort, rows };
    record w2::http::Request = api::http::Request { method, url, headers, body, timeout_ms };
    variant w2::http::HttpError = api::http::HttpError {
        NotAllowed(a), TimedOut, Cancelled, Failed(a)
    };
    record w2::pricing::Usage = api::pricing::Usage {
        input, output, cache_read, cache_write_5m, cache_write_1h, web_searches
    };
    record w2::pricing::SpendTotals = api::pricing::SpendTotals {
        turn, turn_at_least, session, session_at_least
    };
    enum w2::prompt_types::At = api::prompt::At { Start, Turn, Head };
    variant w2::prompt_types::Prior = api::prompt::Prior { None, Snapshot(a), Legacy(a) };
    record w2::prompt_types::Rendered = api::prompt::Rendered { snapshot, text };
    record w2::workspace_facts::EnvironmentFacts = api::prompt::EnvironmentFacts {
        cwd, project_root, platform, resources, tool_lines, agent_lines
    };
    record w2::job_news::LostJob = api::prompt::LostJob { id, target, end };
    record w2::job_news::EndedJob = api::prompt::EndedJob { id, target, end, unread };
    record w2::job_news::RewoundJob = api::prompt::RewoundJob { id, target };
    record w2::job_news::KeptJob = api::prompt::KeptJob { id, log };
    variant w2::job_news::Item = api::prompt::Item {
        Restarted(a), Kept(a), Unchecked(a), Lost(a), Ended(a), Typed(a), Rewound(a)
    };
    record w2::job_news::News = api::prompt::News { wake, items };
    enum w2::instructions_report::Scope = api::instructions::Scope { Project, Default, Preset };
    enum w2::instructions_report::ProblemKind = api::instructions::ProblemKind {
        NotAFile, NotUtf8, Unreadable, ImportMissing, ImportTooDeep
    };
    record w2::instructions_report::Problem = api::instructions::Problem { source, kind, message };
    variant w2::instructions_report::Diagnostic = api::instructions::Diagnostic {
        Problem(a), Large(a)
    };
    record w2::instructions_report::SourceInfo = api::instructions::SourceInfo {
        scope, name, display_path, bytes, sha256, imported_by, external
    };
    record w2::instructions_report::CandidateInfo = api::instructions::CandidateInfo {
        scope, display_path, bytes, mtime, sha256
    };
    record w2::instructions_report::Summary = api::instructions::Summary {
        chosen, imports, candidates, diagnostics
    };
    record w2::instructions_report::LoadReport = api::instructions::LoadReport {
        chosen, summary, project_root
    };
    record w2::instructions_report::FileChange = api::instructions::FileChange {
        scope, name, display_path, old_sha256, new_sha256
    };
    record w2::instructions_report::Change = api::instructions::Change {
        from, to, changed, summary
    };
    enum w2::slot_types::Tone = api::slots::Tone { Text, Muted, Faint, Accent, Ok, Error };
    enum w2::slot_types::NodeState = api::slots::NodeState {
        Working, Waiting, Done, Failed, Stopped
    };
    record w2::slot_types::TextSpan = api::slots::TextSpan { text, tone, bold };
    record w2::slot_types::ElapsedSpan = api::slots::ElapsedSpan { since_ms, tone };
    record w2::slot_types::UsdSpan = api::slots::UsdSpan { micros, at_least, tone };
    variant w2::slot_types::Span = api::slots::Span { Text(a), Elapsed(a), Usd(a), Fill(a) };
    record w2::slot_types::Row = api::slots::Row { key, value };
    record w2::slot_types::Gauge = api::slots::Gauge { label, value, max, tone, note };
    record w2::slot_types::Spark = api::slots::Spark { label, values, max, tone, note };
    record w2::slot_types::Node = api::slots::Node { depth, state, role, label, facts };
    variant_one w2::slot_types::Block = api::slots::Block {
        Line(a), Rows(a), Gauge(a), Spark(a), List(a), Tree(a), NextColumn, Fill(a)
    };
    record_one w2::slot_types::Content = api::slots::Content { note, blocks };
    enum w2::slot_types::SlotView = api::slots::SlotView { Panel, Page };
    record w2::history::SaidEntry = api::strategy::SaidEntry { turn, text, seq };
    record w2::history::CallEntry = api::strategy::CallEntry {
        turn, tool, target, status, cwd, saved, attached, withdrawn
    };
    record x2::strategy::StrategyInfo = api::strategy::StrategyInfo {
        compact, epoch, first_turn, last_turn, native, cut_off, verbatim_tokens
    };
    enum w2::agent_types::QueueReason = api::tree::QueueReason {
        Concurrency, Stagger, Endpoint, Retry, Unknown
    };
    enum w2::agent_types::HoldReason = api::tree::HoldReason {
        Stopped, HeldMessages, RetryOffer, Concurrency, Unknown
    };
    enum w2::agent_types::IdleOutcome = api::tree::IdleOutcome {
        Reported, Asked, Waiting, NoReport, InvalidReport, Failed, Stopped, Unknown
    };
    variant w2::agent_types::AgentState = api::tree::AgentState {
        Queued(a), Running, Stopping, Idle(a), Held(a), Ended, Unknown
    };
    variant w2::agent_types::Activity = api::tree::Activity {
        Idle, Streaming, Tool(a), Queued(a), Held(a), Unknown
    };
    record w2::agent_types::Progress = api::tree::Progress { completed, total };
    record w2::agent_types::AgentFill = api::tree::AgentFill { used, window, limit, estimated };
    record w2::agent_types::Identity = api::tree::Identity {
        path, name, parent, preset, depth, session_id, root_session_id
    };
    record w2::agent_types::AgentNode = api::tree::AgentNode {
        path, name, parent, session_id, preset, model, effort, endpoint, provider, depth, state,
        activity, since_ms, spent_micros, lower_bound, priced, checklist, cwd, running_ms,
        context
    };
    enum w2::agent_types::MailKind = api::tree::MailKind { Task, Message, Report, Notice, Unknown };
    enum w2::agent_types::ReportOutcome = api::tree::ReportOutcome {
        Reported, NoReport, InvalidReport, Unknown
    };
    record w2::agent_types::StateChange = api::tree::StateChange {
        agent, state, activity, spent_micros, lower_bound
    };
    record w2::agent_types::MailInfo = api::tree::MailInfo { id, from, to, kind, wake, bytes };
    record w2::agent_types::ReportInfo = api::tree::ReportInfo {
        agent, turn, outcome, cost_micros, duration_ms
    };
    record w2::agent_types::StopInfo = api::tree::StopInfo { agent, target, by };
    variant w2::agent_types::TreeEvent = api::tree::TreeEvent {
        AgentSpawned(a), AgentState(a), AgentMail(a), AgentReported(a), AgentStopped(a),
        AgentEnded(a)
    };
}

macro_rules! tools_rows {
    ($($world:ident),*) => {
        $(mirror! {
    enum $world::tools::Effect = api::tools::Effect {
        ReadOnly, IdempotentKeyed, LocalTxn, NonIdempotent
    };
    enum $world::tools::Purpose = api::tools::Purpose { Run, Show };
    flags $world::tools::Tags = api::tools::Tags {
        reversible: REVERSIBLE, irreversible: IRREVERSIBLE, external_message: EXTERNAL_MESSAGE,
        reads_untrusted: READS_UNTRUSTED, network: NETWORK
    };
    record $world::tools::Timeouts = api::tools::Timeouts { default_ms, max_ms };
    record $world::tools::ToolMeta = api::tools::ToolMeta {
        effect, tags, parallel_safe, timeouts, saves_output, needs_workspace, ends_reply,
        asks_user, reports_jobs, writing, purpose
    };
    record $world::tools::ClientTool = api::tools::ClientTool { description, input_schema, meta };
    variant $world::tools::ToolKind = api::tools::ToolKind { Client(a), Native(a) };
    record $world::tools::ToolDef = api::tools::ToolDef { name, kind, system_line, context_line };
    record $world::tools::BuildContext = api::tools::BuildContext { platform, capabilities };
    variant $world::tools::DoctorCell = api::tools::DoctorCell { Text(a), Bytes(a) };
    record $world::tools::DoctorRow = api::tools::DoctorRow { key, cells };
        })*
    };
}

tools_rows!(x1, x3);

mirror! {
    variant x3::commands::CommandReply = api::commands::CommandReply { None, Note(a), Send(a), Lines(a) };
    record x3::commands::CommandRow = api::commands::CommandRow { left, right };
    variant x3::commands::CommandLine = api::commands::CommandLine { Row(a), Text(a) };
    enum w3::index::Kind = api::index::Kind { Tool, Skill, Document };
    record w3::index::Entry = api::index::Entry { kind, name, summary, text };
    record_one w3::types::Hit = api::types::Hit { kind, name };
    variant_one w3::types::Fact = api::types::Fact {
        Exit(a), Lines(a), Bytes(a), Matches(a), Diffstat(a), Process(a), Media(a), Part(a),
        Tokens(a), Hits(a)
    };
    record_one w3::types::ToolRun = api::types::ToolRun {
        outcome, facts, cwd, trailer, result, attachments, typed
    };
    record w3::agent_states::ShownState = api::tree::ShownState { path, state, keep_warm };
    record w3::secrets::SecretAsk = api::host::SecretAsk {
        process, name, reason, end_input, show_start, show_end
    };
    enum w3::secrets::SecretRefusal = api::host::SecretRefusal {
        NoProcess, Ended, ShowsInput, NoInput, Attached, TooLong
    };
    variant w3::secrets::SecretEnd = api::host::SecretEnd {
        Typed, ProcessEnded, Unavailable, Stopped, Refused(a), Failed
    };
    record w3::agent_rename::Renamed = api::host::Renamed { from, to };
    record w3::slot_types::TopologyNode = api::slots::TopologyNode { key, parent, state };
    record w3::slot_types::Flow = api::slots::Flow { from, to, at_ms };
    enum w3::slot_types::Noun = api::slots::Noun { Agents };
    record w3::slot_types::Topology = api::slots::Topology { root, nodes, flows, counted };
    record w3::types::CodeView = api::types::CodeView { language, code };
    variant_one w3::slot_types::Block = api::slots::Block {
        Line(a), Rows(a), Gauge(a), Spark(a), List(a), Tree(a), NextColumn, Fill(a), Topology(a)
    };
    record_one w3::slot_types::Content = api::slots::Content { note, blocks };
    variant_one w3::types::View = api::types::View { Diff(a), Process(a), Code(a) };
    record_one w3::types::Display = api::types::Display { action, verbs, target, output, view, title };
}

impl Mirror<api::host::Todo> for w1::todos::Todo {
    fn mirror(self) -> api::host::Todo {
        let w1::todos::Todo { text, status } = self;
        api::host::Todo {
            text,
            status: status.mirror(),
            section: None,
        }
    }
}
