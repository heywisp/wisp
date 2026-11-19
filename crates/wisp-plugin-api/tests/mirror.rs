#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "the drift test fails on any mismatch"
)]

use std::any::Any;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll, Waker};

use futures::FutureExt;
use wisp_plugin_api::chat::Chat;
use wisp_plugin_api::commands::CommandReply;
use wisp_plugin_api::config::{self, AccountList, Accounts, Config, SettingRow};
use wisp_plugin_api::hooks::{self, Event, Points, Reply};
use wisp_plugin_api::host::{
    self, AgentRename, AgentRow, Agents, CallContext, Captured, Clock, Context, Facts, FileKind,
    Fill, Found, HoldSpec, Host, Instant, Job, JobError, JobInfo, JobOutput, Jobs, Log, Media,
    Mode, OutputFile, OutputSummary, Read, Rows, SavedFile, Screen, ScreenReport, ScreenReports,
    SecretAsk, SecretEnd, Secrets, Sent, Session, Settings, Settled, Size, SpawnInput, SpawnSpec,
    StartedAgent, Stopped, Todo, Todos, TokenCount, Typing, WaitFor, Waited, Workspace, Written,
};
use wisp_plugin_api::http::{self, Body, Http, HttpError, Response};
use wisp_plugin_api::index;
use wisp_plugin_api::instructions::{self, InstructionsReport};
use wisp_plugin_api::manage::{self, Manage};
use wisp_plugin_api::manifest::CORE_POINTS;
use wisp_plugin_api::mcp::{self, Mcp};
use wisp_plugin_api::model::{self, Model};
use wisp_plugin_api::pricing::{self, Pricing, SpendTotals};
use wisp_plugin_api::project::Project;
use wisp_plugin_api::prompt::{
    self, At, EnvironmentFacts, JobNews, News, Prior, Rendered, WorkspaceFacts,
};
use wisp_plugin_api::slots::{self, Content, SlotView};
use wisp_plugin_api::strategy::{self, StrategyInfo, View};
use wisp_plugin_api::tools::{self, BuildContext, DoctorRow, Prepared, ToolDef};
use wisp_plugin_api::tree::{
    self, AgentNode, AgentStates, AgentTree, Identity, ShownState, TreeEvent,
};
use wisp_plugin_api::types::{
    self, Action, AskReply, Display, Level, Outcome, OutputKind, Question, SignalKind, ToolRun,
    Typed,
};
use wisp_plugin_api::usage::{self, Usage};
use wisp_plugin_api::{BoxFuture, Bundled, Plugin, commands};
use wit_parser::{FunctionKind, Resolve, TypeDefKind, WorldItem, WorldKey};

const WIT_0_1_0: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../wit/v0.1.0");

const WIT_0_2_0: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../wit/v0.2.0");

const WIT_0_3_0: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../wit/v0.3.0");

const WIT_0_4_0: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../wit/v0.4.0");

const ADDED_IN_0_4_0: [&str; 6] = ["chat", "manage", "mcp", "model", "project", "usage"];

const ADDED_IN_0_2_0: [&str; 17] = [
    "slot-types",
    "slot-render",
    "prompt-types",
    "prompt-sections",
    "workspace-facts",
    "instructions-report",
    "job-news",
    "pricing",
    "http",
    "agent-types",
    "agent",
    "agent-tree",
    "agent-hooks",
    "history",
    "strategy",
    "config",
    "accounts",
];

const ADDED_IN_0_3_0: [&str; 6] = [
    "agent-rename",
    "agent-states",
    "screen-report",
    "secrets",
    "index",
    "index-source",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Record,
    Variant,
    Enum,
    Flags,
    Resource,
}

struct Mirror {
    interface: &'static str,
    wit: &'static str,
    kind: Kind,
    rust: &'static str,
    names: Vec<&'static str>,
}

macro_rules! fields {
    ($kind:ident, $interface:literal, $wit:literal, $ty:ty { $($field:ident),* $(,)? }) => {{
        #[allow(dead_code, reason = "compiled only, so a field the WIT lacks fails to build")]
        fn exhaustive(value: $ty) {
            type T = $ty;
            let T { $($field: _),* } = value;
        }
        Mirror {
            interface: $interface,
            wit: $wit,
            kind: Kind::$kind,
            rust: stringify!($ty),
            names: vec![$(stringify!($field)),*],
        }
    }};
}

macro_rules! cases {
    ($kind:ident, $interface:literal, $wit:literal, $ty:ty { $($case:ident),* $(,)? }) => {{
        #[allow(dead_code, reason = "compiled only, so a case the WIT lacks fails to build")]
        fn exhaustive(value: $ty) {
            type T = $ty;
            match value {
                $(T::$case { .. } => {}),*
            }
        }
        Mirror {
            interface: $interface,
            wit: $wit,
            kind: Kind::$kind,
            rust: stringify!($ty),
            names: vec![$(stringify!($case)),*],
        }
    }};
}

fn mirrors() -> Vec<Mirror> {
    vec![
        cases!(
            Enum,
            "manage",
            "action",
            manage::Action {
                On,
                Off,
                Add,
                Remove
            }
        ),
        cases!(
            Enum,
            "manage",
            "kind",
            manage::Kind {
                Tool,
                Skill,
                Server
            }
        ),
        fields!(
            Record,
            "manage",
            "change",
            manage::Change {
                action,
                kind,
                name,
                from,
                keys
            }
        ),
        fields!(
            Record,
            "model",
            "prompt",
            model::Prompt {
                system,
                text,
                max_tokens,
                native
            }
        ),
        fields!(Record, "model", "source", model::Source { title, url }),
        fields!(Record, "model", "reply", model::Reply { text, sources }),
        cases!(
            Variant,
            "model",
            "ask-error",
            model::AskError {
                NotAllowed,
                Cancelled,
                Refused,
                Failed
            }
        ),
        cases!(
            Variant,
            "mcp",
            "state",
            mcp::State {
                Stopped,
                Starting,
                Ready,
                NeedsSignIn,
                NeedsKey,
                Failed
            }
        ),
        fields!(
            Record,
            "mcp",
            "server",
            mcp::Server {
                name,
                state,
                tools,
                listed,
                off
            }
        ),
        fields!(
            Record,
            "mcp",
            "tool",
            mcp::Tool {
                name,
                server,
                original,
                description,
                input_schema
            }
        ),
        fields!(
            Record,
            "mcp",
            "prompt-argument",
            mcp::PromptArgument {
                name,
                required,
                description
            }
        ),
        fields!(
            Record,
            "mcp",
            "prompt",
            mcp::Prompt {
                name,
                server,
                original,
                description,
                arguments
            }
        ),
        fields!(
            Record,
            "mcp",
            "resource",
            mcp::Resource {
                name,
                server,
                uri,
                title,
                description
            }
        ),
        fields!(
            Record,
            "mcp",
            "prompt-message",
            mcp::PromptMessage { role, text }
        ),
        cases!(
            Enum,
            "usage",
            "usage-state",
            usage::UsageState {
                Ok,
                Stale,
                Checking,
                SignedOut,
                Off,
                Unavailable,
                Unknown
            }
        ),
        fields!(
            Record,
            "usage",
            "account-usage",
            usage::AccountUsage {
                name,
                yours,
                state,
                limited,
                retry_at,
                windows
            }
        ),
        fields!(Record, "types", "hit", types::Hit { kind, name }),
        cases!(
            Enum,
            "index",
            "kind",
            index::Kind {
                Tool,
                Skill,
                Document
            }
        ),
        fields!(
            Record,
            "index",
            "entry",
            index::Entry {
                kind,
                name,
                summary,
                text
            }
        ),
        cases!(
            Enum,
            "types",
            "signal-kind",
            types::SignalKind { Interrupt, Kill }
        ),
        cases!(
            Enum,
            "types",
            "background-reason",
            types::BackgroundReason {
                Started,
                Quiet,
                MovedByUser,
                StillRunning
            }
        ),
        cases!(
            Enum,
            "types",
            "level",
            types::Level {
                Debug,
                Info,
                Warn,
                Error
            }
        ),
        cases!(
            Variant,
            "types",
            "action",
            types::Action {
                Run,
                Read,
                Write,
                Edit,
                Search,
                Fetch,
                Input,
                Wait,
                Attach,
                Show,
                Signal,
                Other,
            }
        ),
        fields!(
            Record,
            "types",
            "verbs",
            types::Verbs {
                doing,
                done,
                failed
            }
        ),
        cases!(
            Variant,
            "types",
            "output-kind",
            types::OutputKind {
                Text,
                Code,
                Diff,
                None
            }
        ),
        fields!(
            Record,
            "types",
            "diff-file",
            types::DiffFile {
                path,
                old_path,
                language,
                hunks,
                added,
                removed,
            }
        ),
        fields!(
            Record,
            "types",
            "diff-view",
            types::DiffView { title, files }
        ),
        cases!(
            Enum,
            "types",
            "named-key",
            types::NamedKey {
                Enter,
                Tab,
                Space,
                Esc,
                Backspace,
                Delete,
                Up,
                Down,
                Left,
                Right,
                Home,
                End,
                PageUp,
                PageDown,
            }
        ),
        cases!(
            Variant,
            "types",
            "key",
            types::Key {
                Named,
                Function,
                Ctrl,
                Eof
            }
        ),
        cases!(
            Enum,
            "types",
            "read-kind",
            types::ReadKind {
                New,
                Tail,
                Lines,
                Search,
                Screen
            }
        ),
        fields!(
            Record,
            "types",
            "process-view",
            types::ProcessView {
                id,
                text,
                keys,
                signal,
                read,
                pattern,
            }
        ),
        fields!(
            Record,
            "types",
            "code-view",
            types::CodeView { language, code }
        ),
        cases!(
            Variant,
            "types",
            "view",
            types::View {
                Diff,
                Process,
                Code
            }
        ),
        fields!(
            Record,
            "types",
            "display",
            types::Display {
                action,
                verbs,
                target,
                output,
                view,
                title,
            }
        ),
        fields!(Record, "types", "matches", types::Matches { n, files }),
        fields!(
            Record,
            "types",
            "diffstat",
            types::Diffstat { added, removed }
        ),
        fields!(
            Record,
            "types",
            "part",
            types::Part {
                sent,
                total,
                more_than
            }
        ),
        cases!(
            Variant,
            "types",
            "fact",
            types::Fact {
                Exit,
                Lines,
                Bytes,
                Matches,
                Diffstat,
                Process,
                Media,
                Part,
                Tokens,
                Hits,
            }
        ),
        cases!(
            Variant,
            "types",
            "exit-status",
            types::ExitStatus { Code, Signal }
        ),
        fields!(
            Record,
            "types",
            "completed",
            types::Completed { is_error, exit }
        ),
        fields!(
            Record,
            "types",
            "failure",
            types::Failure {
                message,
                before_start
            }
        ),
        cases!(
            Variant,
            "types",
            "outcome",
            types::Outcome {
                Completed,
                TimedOut,
                Cancelled,
                InvalidInput,
                Failed,
                Backgrounded,
            }
        ),
        fields!(
            Record,
            "types",
            "attachment",
            types::Attachment { media_type, bytes }
        ),
        fields!(
            Record,
            "types",
            "typed",
            types::Typed {
                echoed,
                unsent,
                signalled
            }
        ),
        fields!(
            Record,
            "types",
            "tool-run",
            types::ToolRun {
                outcome,
                facts,
                cwd,
                trailer,
                result,
                attachments,
                typed,
            }
        ),
        fields!(
            Record,
            "types",
            "question-option",
            types::QuestionOption { label, description }
        ),
        fields!(
            Record,
            "types",
            "question",
            types::Question {
                header,
                question,
                options,
                multi_select,
            }
        ),
        fields!(Record, "types", "answer", types::Answer { chosen, own }),
        cases!(
            Variant,
            "types",
            "ask-reply",
            types::AskReply {
                Answered,
                Attached,
                Declined,
                ProcessEnded,
                MessageInstead,
                Unavailable,
                Stopped,
            }
        ),
        fields!(
            Record,
            "session",
            "facts",
            host::Facts {
                model,
                endpoint_kind,
                effort,
                platform,
                capabilities,
            }
        ),
        cases!(
            Enum,
            "workspace",
            "file-kind",
            host::FileKind {
                Regular,
                Directory,
                Fifo,
                Socket,
                Device,
                Other,
            }
        ),
        fields!(Record, "clock", "instant", host::Instant { seconds, nanos }),
        fields!(
            Record,
            "context",
            "fill",
            host::Fill {
                estimate,
                measured,
                window,
                point,
                ceiling,
                auto_compact,
                epoch,
                parts,
                cached,
            }
        ),
        cases!(
            Enum,
            "context",
            "fill-part",
            host::FillPart {
                System,
                Tools,
                Chat,
                Skills
            }
        ),
        fields!(
            Record,
            "context",
            "part-tokens",
            host::PartTokens { part, tokens }
        ),
        cases!(
            Variant,
            "media",
            "token-count",
            host::TokenCount {
                Counted,
                Unsupported,
                Failed
            }
        ),
        fields!(
            Record,
            "call",
            "output-summary",
            host::OutputSummary {
                text,
                bytes,
                lines,
                file
            }
        ),
        cases!(
            Enum,
            "todos",
            "todo-status",
            host::TodoStatus {
                Pending,
                InProgress,
                Completed,
                Cancelled
            }
        ),
        fields!(
            Record,
            "todos",
            "todo",
            host::Todo {
                text,
                status,
                section
            }
        ),
        fields!(
            Record,
            "agents",
            "spawn-input",
            host::SpawnInput {
                name,
                task,
                todos,
                cwd,
                preset,
                endpoint,
                model,
                effort,
                tools_off,
                spawn,
                keep_warm
            }
        ),
        cases!(
            Variant,
            "agents",
            "warm-mark",
            host::WarmMark { Limit, Unsupported }
        ),
        fields!(
            Record,
            "agents",
            "started-agent",
            host::StartedAgent {
                path,
                model,
                effort,
                endpoint,
                provider,
                cwd,
                keep_warm,
                fell_back
            }
        ),
        cases!(
            Enum,
            "agents",
            "relation",
            host::Relation {
                Caller,
                Parent,
                Sibling,
                Child,
                Descendant
            }
        ),
        fields!(
            Record,
            "agents",
            "progress",
            host::Progress { completed, total }
        ),
        fields!(
            Record,
            "agents",
            "warm-row",
            host::WarmRow {
                refreshes,
                limit,
                paused
            }
        ),
        fields!(
            Record,
            "agents",
            "agent-row",
            host::AgentRow {
                path,
                session,
                relation,
                preset,
                model,
                effort,
                endpoint,
                provider,
                state,
                detail,
                spent_micros,
                billing_note,
                plan_micros,
                lower_bound,
                priced,
                checklist,
                last_report,
                cwd,
                running_ms,
                context_used,
                context_window,
                context_limit,
                context_estimated,
                keep_warm
            }
        ),
        cases!(
            Enum,
            "agents",
            "delivery",
            host::Delivery {
                Running,
                Woken,
                Idle,
                Queued,
                Held
            }
        ),
        fields!(Record, "agents", "sent", host::Sent { to, delivery }),
        fields!(
            Record,
            "agents",
            "stopped",
            host::Stopped { path, below, ended }
        ),
        cases!(Enum, "jobs", "mode", host::Mode { Pipe, Terminal }),
        fields!(Record, "jobs", "size", host::Size { cols, rows }),
        fields!(
            Record,
            "jobs",
            "spawn-spec",
            host::SpawnSpec {
                program,
                args,
                cwd,
                env,
                mode,
                size,
                stdin,
                background,
                limit_ms,
                label,
                keep,
            }
        ),
        cases!(
            Variant,
            "jobs",
            "stopped-by",
            host::StoppedBy {
                Model,
                User,
                SessionEnded,
                TimeLimit
            }
        ),
        cases!(
            Enum,
            "jobs",
            "lost-reason",
            host::LostReason {
                StoppedAtResume,
                Gone
            }
        ),
        cases!(
            Variant,
            "jobs",
            "job-end",
            host::JobEnd {
                Exited,
                Stopped,
                Lost,
                Failed
            }
        ),
        cases!(
            Variant,
            "jobs",
            "job-error",
            host::JobError {
                Driven,
                Ended,
                NoInput,
                Closed,
                LineTooLong,
                Invalid,
                Io
            }
        ),
        fields!(
            Record,
            "jobs",
            "job-info",
            host::JobInfo {
                id,
                call_id,
                label,
                end,
                mode,
                lines,
                bytes,
                restored,
                echo_off,
                full_screen,
                unread,
                limit_ms,
                left_ms,
                size,
                keep,
                log,
            }
        ),
        cases!(
            Enum,
            "jobs",
            "settled",
            host::Settled {
                Ended,
                Quiet,
                Cancelled
            }
        ),
        fields!(
            Record,
            "jobs",
            "wait-for",
            host::WaitFor {
                timeout_ms,
                until,
                quiet_ms
            }
        ),
        fields!(
            Record,
            "jobs",
            "handed-back",
            host::HandedBack { id, quiet_ms }
        ),
        fields!(
            Record,
            "jobs",
            "hold-spec",
            host::HoldSpec { quiet_ms, yield_ms }
        ),
        cases!(
            Variant,
            "jobs",
            "waited",
            host::Waited {
                Ended,
                Matched,
                Quiet,
                TimedOut,
                Cancelled,
                Steered,
                Moved,
                NeedsInput,
                StillRunning,
            }
        ),
        fields!(Record, "jobs", "line", host::Line { number, text }),
        fields!(Record, "jobs", "span", host::Span { first, last }),
        cases!(Variant, "jobs", "row", host::Row { Line, Gap }),
        fields!(
            Record,
            "jobs",
            "write-failure",
            host::WriteFailure { error, after }
        ),
        fields!(Record, "jobs", "capped", host::Capped { cap, dropped }),
        cases!(
            Variant,
            "jobs",
            "file-problem",
            host::FileProblem {
                NotCreated,
                WriteFailed,
                Capped
            }
        ),
        fields!(
            Record,
            "jobs",
            "output-file",
            host::OutputFile { path, problem }
        ),
        fields!(
            Record,
            "jobs",
            "section",
            host::Section {
                first,
                last,
                bytes,
                lines,
                head,
                tail
            }
        ),
        fields!(
            Record,
            "jobs",
            "read",
            host::Read {
                section,
                complete,
                held,
                file
            }
        ),
        fields!(
            Record,
            "jobs",
            "job-output",
            host::JobOutput {
                head,
                tail,
                bytes,
                lines,
                file
            }
        ),
        fields!(
            Record,
            "jobs",
            "unreadable",
            host::Unreadable { path, error }
        ),
        cases!(
            Variant,
            "jobs",
            "saved-file",
            host::SavedFile {
                Readable,
                NoneKept,
                Missing,
                Unreadable
            }
        ),
        fields!(
            Record,
            "jobs",
            "rows",
            host::Rows {
                last,
                complete,
                rows,
                cut,
                held
            }
        ),
        fields!(
            Record,
            "jobs",
            "found-line",
            host::FoundLine {
                number,
                text,
                matched
            }
        ),
        cases!(
            Variant,
            "jobs",
            "found-row",
            host::FoundRow { Line, Separator }
        ),
        fields!(
            Record,
            "jobs",
            "found",
            host::Found {
                last,
                rows,
                shown,
                remaining,
                last_shown,
                gaps
            }
        ),
        fields!(Record, "jobs", "point", host::Point { row, col }),
        fields!(
            Record,
            "jobs",
            "screen",
            host::Screen {
                rows,
                cursor,
                size,
                full_screen,
                changed
            }
        ),
        fields!(
            Record,
            "jobs",
            "typing",
            host::Typing {
                text,
                keys,
                signal,
                size
            }
        ),
        fields!(
            Record,
            "jobs",
            "written",
            host::Written {
                total,
                left,
                typed,
                stopped,
                failed,
                after_kill
            }
        ),
        cases!(
            Variant,
            "jobs",
            "captured",
            host::Captured {
                NotFound,
                Failed,
                Output
            }
        ),
        cases!(
            Enum,
            "tools",
            "effect",
            tools::Effect {
                ReadOnly,
                IdempotentKeyed,
                LocalTxn,
                NonIdempotent,
            }
        ),
        cases!(Enum, "tools", "purpose", tools::Purpose { Run, Show }),
        fields!(
            Flags,
            "tools",
            "tags",
            tools::Tags {
                reversible,
                irreversible,
                external_message,
                reads_untrusted,
                network,
            }
        ),
        fields!(
            Record,
            "tools",
            "timeouts",
            tools::Timeouts { default_ms, max_ms }
        ),
        fields!(
            Record,
            "tools",
            "tool-meta",
            tools::ToolMeta {
                effect,
                tags,
                parallel_safe,
                timeouts,
                saves_output,
                needs_workspace,
                ends_reply,
                asks_user,
                reports_jobs,
                writing,
                purpose,
            }
        ),
        fields!(
            Record,
            "tools",
            "client-tool",
            tools::ClientTool {
                description,
                input_schema,
                meta
            }
        ),
        cases!(
            Variant,
            "tools",
            "tool-kind",
            tools::ToolKind { Client, Native }
        ),
        fields!(
            Record,
            "tools",
            "tool-def",
            tools::ToolDef {
                name,
                kind,
                system_line,
                context_line
            }
        ),
        fields!(
            Record,
            "tools",
            "build-context",
            tools::BuildContext {
                platform,
                capabilities
            }
        ),
        fields!(
            Record,
            "tools",
            "doctor-row",
            tools::DoctorRow { key, cells }
        ),
        cases!(
            Variant,
            "tools",
            "doctor-cell",
            tools::DoctorCell { Text, Bytes }
        ),
        fields!(
            Record,
            "commands",
            "command-row",
            commands::CommandRow { left, right }
        ),
        cases!(
            Variant,
            "commands",
            "command-line",
            commands::CommandLine { Row, Text }
        ),
        cases!(
            Variant,
            "commands",
            "command-reply",
            commands::CommandReply {
                None,
                Note,
                Send,
                Lines
            }
        ),
        fields!(
            Record,
            "hook-types",
            "lead-item",
            hooks::LeadItem { by, text }
        ),
        fields!(
            Record,
            "hook-types",
            "session-start",
            hooks::SessionStart { resumed }
        ),
        fields!(
            Record,
            "hook-types",
            "user-input",
            hooks::UserInput {
                text,
                images,
                steer
            }
        ),
        fields!(
            Record,
            "hook-types",
            "before-turn",
            hooks::BeforeTurn {
                turn,
                lead,
                typed,
                images,
                retry,
            }
        ),
        fields!(
            Record,
            "hook-types",
            "after-turn",
            hooks::AfterTurn { turn, outcome }
        ),
        fields!(
            Record,
            "hook-types",
            "step-info",
            hooks::StepInfo {
                turn,
                step,
                attempt
            }
        ),
        fields!(
            Record,
            "hook-types",
            "response-info",
            hooks::ResponseInfo {
                turn,
                step,
                stop_reason
            }
        ),
        fields!(
            Record,
            "hook-types",
            "before-end",
            hooks::BeforeEnd {
                turn,
                step,
                reply,
                continues
            }
        ),
        fields!(
            Record,
            "hook-types",
            "tool-call",
            hooks::ToolCall {
                call_id,
                tool,
                input
            }
        ),
        fields!(
            Record,
            "hook-types",
            "tool-result",
            hooks::ToolResult {
                call_id,
                tool,
                input,
                result,
                is_error,
            }
        ),
        fields!(
            Record,
            "hook-types",
            "fill-info",
            hooks::FillInfo {
                turn,
                step,
                before_turn,
                estimate,
                measured,
                window,
                point,
                ceiling,
                auto_compact,
                epoch,
            }
        ),
        fields!(
            Record,
            "hook-types",
            "compact-info",
            hooks::CompactInfo {
                trigger,
                engine,
                turn,
                focus,
                estimate,
                point,
                epoch,
                requested_by,
                instruction,
            }
        ),
        fields!(
            Record,
            "hook-types",
            "head-info",
            hooks::HeadInfo {
                epoch,
                trigger,
                engine,
                first_turn,
                last_turn,
            }
        ),
        fields!(
            Record,
            "hook-types",
            "compacted-info",
            hooks::CompactedInfo {
                epoch,
                trigger,
                engine,
                before,
                after,
                summary,
                cost_micros,
                partial,
                failed,
            }
        ),
        fields!(
            Record,
            "hook-types",
            "rewind-info",
            hooks::RewindInfo { turn, epoch }
        ),
        fields!(
            Record,
            "hook-types",
            "command-info",
            hooks::CommandInfo { name, args }
        ),
        fields!(
            Record,
            "hook-types",
            "tools-change",
            hooks::ToolsChange {
                added,
                removed,
                via
            }
        ),
        fields!(
            Record,
            "hook-types",
            "model-switch",
            hooks::ModelSwitch { endpoint, model }
        ),
        fields!(
            Record,
            "hook-types",
            "custom",
            hooks::Custom {
                point,
                version,
                payload
            }
        ),
        cases!(
            Variant,
            "hook-types",
            "event",
            hooks::Event {
                SessionStart,
                SessionEnd,
                UserInput,
                BeforeTurn,
                AfterTurn,
                BeforeRequest,
                AfterResponse,
                BeforeEnd,
                BeforeTool,
                AfterTool,
                ContextThreshold,
                BeforeCompact,
                CompactionHead,
                AfterCompact,
                Rewound,
                Command,
                ToolsChanged,
                SettingsChanged,
                ModelSwitched,
                Custom,
            }
        ),
        cases!(
            Variant,
            "hook-types",
            "reply",
            hooks::Reply {
                None,
                Append,
                Rewrite,
                Block,
                Continue,
                Compact,
            }
        ),
        fields!(Record, "points", "answer", hooks::Answer { hook, reply }),
        cases!(
            Enum,
            "slot-types",
            "tone",
            slots::Tone {
                Text,
                Muted,
                Faint,
                Accent,
                Ok,
                Error
            }
        ),
        cases!(
            Enum,
            "slot-types",
            "node-state",
            slots::NodeState {
                Working,
                Waiting,
                Done,
                Failed,
                Stopped
            }
        ),
        fields!(
            Record,
            "slot-types",
            "text-span",
            slots::TextSpan { text, tone, bold }
        ),
        fields!(
            Record,
            "slot-types",
            "elapsed-span",
            slots::ElapsedSpan { since_ms, tone }
        ),
        fields!(
            Record,
            "slot-types",
            "usd-span",
            slots::UsdSpan {
                micros,
                at_least,
                tone
            }
        ),
        cases!(
            Variant,
            "slot-types",
            "span",
            slots::Span {
                Text,
                Elapsed,
                Usd,
                Fill
            }
        ),
        fields!(Record, "slot-types", "row", slots::Row { key, value }),
        fields!(
            Record,
            "slot-types",
            "gauge",
            slots::Gauge {
                label,
                value,
                max,
                tone,
                note
            }
        ),
        fields!(
            Record,
            "slot-types",
            "spark",
            slots::Spark {
                label,
                values,
                max,
                tone,
                note
            }
        ),
        fields!(
            Record,
            "slot-types",
            "node",
            slots::Node {
                depth,
                state,
                role,
                label,
                facts
            }
        ),
        cases!(
            Variant,
            "slot-types",
            "block",
            slots::Block {
                Line,
                Rows,
                Gauge,
                Spark,
                List,
                Tree,
                NextColumn,
                Fill,
                Topology
            }
        ),
        fields!(
            Record,
            "slot-types",
            "topology-node",
            slots::TopologyNode { key, parent, state }
        ),
        fields!(
            Record,
            "slot-types",
            "flow",
            slots::Flow { from, to, at_ms }
        ),
        cases!(Enum, "slot-types", "noun", slots::Noun { Agents }),
        fields!(
            Record,
            "slot-types",
            "topology",
            slots::Topology {
                root,
                nodes,
                flows,
                counted
            }
        ),
        fields!(
            Record,
            "slot-types",
            "content",
            slots::Content { note, blocks }
        ),
        cases!(
            Enum,
            "slot-types",
            "slot-view",
            slots::SlotView { Panel, Page }
        ),
        cases!(Enum, "prompt-types", "at", prompt::At { Start, Turn, Head }),
        cases!(
            Variant,
            "prompt-types",
            "prior",
            prompt::Prior {
                None,
                Snapshot,
                Legacy
            }
        ),
        fields!(
            Record,
            "prompt-types",
            "rendered",
            prompt::Rendered { snapshot, text }
        ),
        fields!(
            Record,
            "workspace-facts",
            "environment-facts",
            prompt::EnvironmentFacts {
                cwd,
                project_root,
                platform,
                resources,
                tool_lines,
                agent_lines
            }
        ),
        cases!(
            Enum,
            "instructions-report",
            "scope",
            instructions::Scope {
                Project,
                Default,
                Preset
            }
        ),
        cases!(
            Enum,
            "instructions-report",
            "problem-kind",
            instructions::ProblemKind {
                NotAFile,
                NotUtf8,
                Unreadable,
                ImportMissing,
                ImportTooDeep
            }
        ),
        fields!(
            Record,
            "instructions-report",
            "problem",
            instructions::Problem {
                source,
                kind,
                message
            }
        ),
        cases!(
            Variant,
            "instructions-report",
            "diagnostic",
            instructions::Diagnostic { Problem, Large }
        ),
        fields!(
            Record,
            "instructions-report",
            "source-info",
            instructions::SourceInfo {
                scope,
                name,
                display_path,
                bytes,
                sha256,
                imported_by,
                external
            }
        ),
        fields!(
            Record,
            "instructions-report",
            "candidate-info",
            instructions::CandidateInfo {
                scope,
                display_path,
                bytes,
                mtime,
                sha256
            }
        ),
        fields!(
            Record,
            "instructions-report",
            "summary",
            instructions::Summary {
                chosen,
                imports,
                candidates,
                diagnostics
            }
        ),
        fields!(
            Record,
            "instructions-report",
            "file-change",
            instructions::FileChange {
                scope,
                name,
                display_path,
                old_sha256,
                new_sha256
            }
        ),
        fields!(
            Record,
            "instructions-report",
            "change",
            instructions::Change {
                from,
                to,
                changed,
                summary
            }
        ),
        fields!(
            Record,
            "instructions-report",
            "load-report",
            instructions::LoadReport {
                chosen,
                summary,
                project_root
            }
        ),
        fields!(
            Record,
            "job-news",
            "lost-job",
            prompt::LostJob { id, target, end }
        ),
        fields!(
            Record,
            "job-news",
            "ended-job",
            prompt::EndedJob {
                id,
                target,
                end,
                unread
            }
        ),
        fields!(
            Record,
            "job-news",
            "rewound-job",
            prompt::RewoundJob { id, target }
        ),
        fields!(Record, "job-news", "kept-job", prompt::KeptJob { id, log }),
        cases!(
            Variant,
            "job-news",
            "item",
            prompt::Item {
                Restarted,
                Kept,
                Unchecked,
                Lost,
                Ended,
                Typed,
                Rewound
            }
        ),
        fields!(Record, "job-news", "news", prompt::News { wake, items }),
        fields!(
            Record,
            "config",
            "setting-row",
            config::SettingRow {
                name,
                value,
                layer,
                help,
                rule
            }
        ),
        cases!(
            Enum,
            "accounts",
            "sign-in",
            config::SignIn {
                Working,
                Expired,
                Refreshable,
                Unavailable,
                ExpiryUnknown
            }
        ),
        fields!(
            Record,
            "accounts",
            "limit-window",
            config::LimitWindow {
                label,
                used_percent,
                resets_at,
                observed_at
            }
        ),
        fields!(
            Record,
            "accounts",
            "limits",
            config::Limits {
                windows,
                limited,
                problem
            }
        ),
        fields!(
            Record,
            "accounts",
            "account-row",
            config::AccountRow {
                name,
                kind,
                unavailable,
                sign_in,
                yours,
                start,
                models,
                limits
            }
        ),
        fields!(
            Record,
            "accounts",
            "account-list",
            config::AccountList {
                model,
                effort,
                rows
            }
        ),
        fields!(
            Record,
            "pricing",
            "usage",
            pricing::Usage {
                input,
                output,
                cache_read,
                cache_write_5m,
                cache_write_1h,
                web_searches
            }
        ),
        fields!(
            Record,
            "pricing",
            "spend-totals",
            pricing::SpendTotals {
                turn,
                turn_at_least,
                session,
                session_at_least
            }
        ),
        fields!(
            Record,
            "http",
            "request",
            http::Request {
                method,
                url,
                headers,
                body,
                timeout_ms
            }
        ),
        fields!(
            Record,
            "http",
            "response",
            http::Response {
                status,
                headers,
                body
            }
        ),
        cases!(
            Variant,
            "http",
            "http-error",
            http::HttpError {
                NotAllowed,
                TimedOut,
                Cancelled,
                Failed
            }
        ),
        cases!(
            Enum,
            "agent-types",
            "queue-reason",
            tree::QueueReason {
                Concurrency,
                Stagger,
                Endpoint,
                Retry,
                Unknown
            }
        ),
        cases!(
            Enum,
            "agent-types",
            "hold-reason",
            tree::HoldReason {
                Stopped,
                HeldMessages,
                RetryOffer,
                Concurrency,
                Unknown
            }
        ),
        cases!(
            Enum,
            "agent-types",
            "idle-outcome",
            tree::IdleOutcome {
                Reported,
                Asked,
                Waiting,
                NoReport,
                InvalidReport,
                Failed,
                Stopped,
                Unknown
            }
        ),
        cases!(
            Variant,
            "agent-types",
            "agent-state",
            tree::AgentState {
                Queued,
                Running,
                Stopping,
                Idle,
                Held,
                Ended,
                Unknown
            }
        ),
        cases!(
            Variant,
            "agent-types",
            "activity",
            tree::Activity {
                Idle,
                Streaming,
                Tool,
                Queued,
                Held,
                Unknown
            }
        ),
        fields!(
            Record,
            "agent-types",
            "progress",
            tree::Progress { completed, total }
        ),
        fields!(
            Record,
            "agent-types",
            "agent-fill",
            tree::AgentFill {
                used,
                window,
                limit,
                estimated
            }
        ),
        fields!(
            Record,
            "agent-states",
            "shown-state",
            tree::ShownState {
                path,
                state,
                keep_warm
            }
        ),
        fields!(
            Record,
            "screen-report",
            "screen-report",
            host::ScreenReport { report, images }
        ),
        fields!(
            Record,
            "secrets",
            "secret-ask",
            host::SecretAsk {
                process,
                name,
                reason,
                end_input,
                show_start,
                show_end
            }
        ),
        cases!(
            Enum,
            "secrets",
            "secret-refusal",
            host::SecretRefusal {
                NoProcess,
                Ended,
                ShowsInput,
                NoInput,
                Attached,
                TooLong
            }
        ),
        cases!(
            Variant,
            "secrets",
            "secret-end",
            host::SecretEnd {
                Typed,
                ProcessEnded,
                Unavailable,
                Stopped,
                Refused,
                Failed
            }
        ),
        fields!(
            Record,
            "agent-rename",
            "renamed",
            host::Renamed { from, to }
        ),
        fields!(
            Record,
            "agent-types",
            "identity",
            tree::Identity {
                path,
                name,
                parent,
                preset,
                depth,
                session_id,
                root_session_id
            }
        ),
        fields!(
            Record,
            "agent-types",
            "agent-node",
            tree::AgentNode {
                path,
                name,
                parent,
                session_id,
                preset,
                model,
                effort,
                endpoint,
                provider,
                depth,
                state,
                activity,
                since_ms,
                spent_micros,
                lower_bound,
                priced,
                checklist,
                cwd,
                running_ms,
                context
            }
        ),
        cases!(
            Enum,
            "agent-types",
            "mail-kind",
            tree::MailKind {
                Task,
                Message,
                Report,
                Notice,
                Unknown
            }
        ),
        cases!(
            Enum,
            "agent-types",
            "report-outcome",
            tree::ReportOutcome {
                Reported,
                NoReport,
                InvalidReport,
                Unknown
            }
        ),
        fields!(
            Record,
            "agent-types",
            "state-change",
            tree::StateChange {
                agent,
                state,
                activity,
                spent_micros,
                lower_bound
            }
        ),
        fields!(
            Record,
            "agent-types",
            "mail-info",
            tree::MailInfo {
                id,
                from,
                to,
                kind,
                wake,
                bytes
            }
        ),
        fields!(
            Record,
            "agent-types",
            "report-info",
            tree::ReportInfo {
                agent,
                turn,
                outcome,
                cost_micros,
                duration_ms
            }
        ),
        fields!(
            Record,
            "agent-types",
            "stop-info",
            tree::StopInfo { agent, target, by }
        ),
        fields!(
            Record,
            "history",
            "said-entry",
            strategy::SaidEntry { turn, text, seq }
        ),
        fields!(
            Record,
            "history",
            "call-entry",
            strategy::CallEntry {
                turn,
                tool,
                target,
                status,
                cwd,
                saved,
                attached,
                withdrawn
            }
        ),
        fields!(
            Record,
            "strategy",
            "strategy-info",
            strategy::StrategyInfo {
                compact,
                epoch,
                first_turn,
                last_turn,
                native,
                cut_off,
                verbatim_tokens
            }
        ),
        cases!(
            Variant,
            "agent-types",
            "tree-event",
            tree::TreeEvent {
                AgentSpawned,
                AgentState,
                AgentMail,
                AgentReported,
                AgentStopped,
                AgentEnded
            }
        ),
    ]
}

struct Probe;

struct Method {
    name: &'static str,
    returns: &'static str,
}

macro_rules! probe {
    ($list:ident, impl $trait:path {
        $(fn $name:ident $(<$($lt:lifetime),*>)? ($($args:tt)*) $(-> $ret:ty)? $body:block)*
    }) => {
        impl $trait for Probe {
            $(fn $name $(<$($lt),*>)? ($($args)*) $(-> $ret)? $body)*
        }

        fn $list() -> Vec<Method> {
            vec![$(Method { name: stringify!($name), returns: stringify!($($ret)?) }),*]
        }
    };
}

fn ready<'a, T: Send + 'a>(value: T) -> BoxFuture<'a, T> {
    Box::pin(std::future::ready(value))
}

fn display() -> Display {
    Display {
        action: Action::Run,
        verbs: None,
        target: String::new(),
        output: OutputKind::None,
        view: None,
        title: None,
    }
}

fn job_info() -> JobInfo {
    JobInfo {
        id: None,
        call_id: None,
        label: String::new(),
        end: None,
        mode: Mode::Pipe,
        lines: 0,
        bytes: 0,
        restored: false,
        echo_off: false,
        full_screen: false,
        unread: 0,
        limit_ms: None,
        left_ms: None,
        size: None,
        keep: false,
        log: None,
    }
}

fn read() -> Read {
    Read {
        section: None,
        complete: 0,
        held: None,
        file: OutputFile {
            path: None,
            problem: None,
        },
    }
}

fn written() -> Written {
    Written {
        total: 0,
        left: 0,
        typed: Typed {
            echoed: false,
            unsent: false,
            signalled: false,
        },
        stopped: false,
        failed: None,
        after_kill: None,
    }
}

probe!(session_methods, impl Session {
    fn current(&self) -> Facts {
        Facts {
            model: String::new(),
            endpoint_kind: String::new(),
            effort: String::new(),
            platform: String::new(),
            capabilities: Vec::new(),
        }
    }
});

probe!(workspace_methods, impl Workspace {
    fn root(&self) -> String { String::new() }
    fn home(&self) -> Option<String> { None }
    fn shorten(&self, path: &str) -> String { path.to_owned() }
    fn kind_of(&self, _path: &str) -> Option<FileKind> { None }
});

probe!(clock_methods, impl Clock {
    fn now(&self) -> Instant { Instant { seconds: 0, nanos: 0 } }
    fn local_offset(&self, _at: Instant) -> Option<i32> { None }
    fn elapsed_ms(&self) -> u64 { 0 }
    fn sleep(&self, _ms: u64) -> BoxFuture<'_, ()> { ready(()) }
});

probe!(settings_methods, impl Settings {
    fn get(&self, _key: &str) -> Option<String> { None }
});

probe!(context_methods, impl Context {
    fn current_fill(&self) -> Fill {
        Fill {
            estimate: 0,
            measured: None,
            window: None,
            point: None,
            ceiling: None,
            auto_compact: false,
            epoch: 0,
            parts: Vec::new(),
            cached: None,
        }
    }
    fn estimate_text(&self, _text: &str) -> u64 { 0 }
});

probe!(media_methods, impl Media {
    fn media_types(&self) -> Vec<String> { Vec::new() }
    fn image_edge(&self) -> Option<u32> { None }
    fn count_tokens(&self, _media_type: String, _bytes: Vec<u8>) -> BoxFuture<'_, TokenCount> {
        ready(TokenCount::Unsupported)
    }
});

probe!(call_context_methods, impl CallContext {
    fn id(&self) -> String { String::new() }
    fn cwd(&self) -> String { String::new() }
    fn deadline_ms(&self) -> Option<u64> { None }
    fn cancelled(&self) -> bool { false }
    fn wait_cancelled(&self) -> BoxFuture<'_, ()> { ready(()) }
    fn moved(&self) -> bool { false }
    fn steered(&self) -> bool { false }
    fn live_tail(&self) -> bool { false }
    fn write_output(&self, _bytes: &[u8]) {}
    fn output_summary(&self) -> OutputSummary {
        OutputSummary { text: String::new(), bytes: 0, lines: 0, file: None }
    }
    fn private_file(&self, _suffix: &str) -> Result<String, String> { Err(String::new()) }
    fn ask(&self, _items: Vec<Question>) -> BoxFuture<'_, AskReply> { ready(AskReply::Unavailable) }
});

probe!(job_methods, impl Job {
    fn info(&self) -> BoxFuture<'_, JobInfo> { ready(job_info()) }
    fn write(&self, _input: Typing, _within_ms: u64) -> BoxFuture<'_, Written> { ready(written()) }
    fn signal(&self, _kind: SignalKind, _within_ms: u64) -> BoxFuture<'_, Written> { ready(written()) }
    fn resize(&self, _to: Size) -> Result<(), JobError> { Ok(()) }
    fn set_limit(&self, _ms: Option<u64>) {}
    fn wait(&self, _how: WaitFor) -> BoxFuture<'_, Result<Waited, String>> { ready(Ok(Waited::Quiet)) }
    fn hold(&self, _spec: HoldSpec) -> BoxFuture<'_, Waited> { ready(Waited::Quiet) }
    fn settle(&self, _quiet_ms: u64, _max_ms: u64) -> BoxFuture<'_, Settled> { ready(Settled::Quiet) }
    fn screen(&self) -> BoxFuture<'_, Option<Screen>> { ready(None) }
    fn new_output(&self) -> BoxFuture<'_, Read> { ready(read()) }
    fn tail(&self, _lines: u32) -> BoxFuture<'_, Read> { ready(read()) }
    fn output(&self) -> BoxFuture<'_, JobOutput> {
        ready(JobOutput { head: String::new(), tail: String::new(), bytes: 0, lines: 0, file: read().file })
    }
    fn saved(&self) -> BoxFuture<'_, Option<SavedFile>> { ready(None) }
    fn lines(&self, _first: u64, _count: u32) -> BoxFuture<'_, Rows> {
        ready(Rows { last: 0, complete: 0, rows: Vec::new(), cut: false, held: None })
    }
    fn search(&self, _pattern: String, _from: u64, _context: u32, _max: u32) -> BoxFuture<'_, Result<Found, String>> {
        ready(Err(String::new()))
    }
    fn background(&self) -> BoxFuture<'_, Option<u32>> { ready(None) }
    fn ask_attach(&self, _reason: String) -> BoxFuture<'_, AskReply> { ready(AskReply::Declined) }
});

probe!(jobs_methods, impl Jobs {
    fn spawn<'a>(&'a self, _spec: SpawnSpec, _ctx: &'a dyn CallContext) -> BoxFuture<'a, Result<Box<dyn Job>, JobError>> {
        ready(Ok(Box::new(Probe) as Box<dyn Job>))
    }
    fn adopt(&self, _id: u32, _ctx: &dyn CallContext) -> Option<Box<dyn Job>> { None }
    fn all(&self) -> BoxFuture<'_, Vec<JobInfo>> { ready(vec![job_info()]) }
    fn mark_reported(&self, _id: u32, _ctx: &dyn CallContext) {}
    fn which(&self, _program: &str) -> Option<String> { None }
    fn capture(&self, _program: String, _args: Vec<String>, _timeout_ms: u64) -> BoxFuture<'_, Captured> {
        ready(Captured::NotFound)
    }
});

probe!(todos_methods, impl Todos {
    fn replace(&self, _items: Vec<Todo>, _ctx: &dyn CallContext) -> Result<(), String> { Ok(()) }
});

probe!(agents_methods, impl Agents {
    fn spawn_agent<'a>(&'a self, _input: SpawnInput, _ctx: &'a dyn CallContext) -> BoxFuture<'a, Result<StartedAgent, String>> {
        ready(Err(String::new()))
    }
    fn list_agents(&self, _ctx: &dyn CallContext) -> Result<Vec<AgentRow>, String> { Ok(Vec::new()) }
    fn report(&self, _input: String, _ctx: &dyn CallContext) -> Result<(), String> { Ok(()) }
    fn send_message(&self, _to: String, _text: String, _wake: bool, _ctx: &dyn CallContext) -> Result<Sent, String> { Err(String::new()) }
    fn stop_agent(&self, _name: String, _end: bool, _ctx: &dyn CallContext) -> Result<Stopped, String> { Err(String::new()) }
});

probe!(log_methods, impl Log {
    fn write(&self, _level: Level, _line: &str) {}
});

probe!(points_methods, impl Points {
    fn fire(&self, _point: String, _payload: String) -> BoxFuture<'_, Vec<hooks::Answer>> {
        ready(Vec::new())
    }
});

probe!(workspace_facts_methods, impl WorkspaceFacts {
    fn environment(&self) -> Option<EnvironmentFacts> { None }
    fn config_dir(&self) -> Option<String> { None }
    fn data_dir(&self) -> Result<String, String> { Err(String::new()) }
    fn terminal(&self) -> Option<Size> { None }
});

probe!(instructions_report_methods, impl InstructionsReport {
    fn loaded(&self, _report: instructions::LoadReport) {}
    fn warning(&self, _diagnostic: instructions::Diagnostic) {}
    fn changed(&self, _change: instructions::Change, _text: String) {}
});

probe!(job_news_methods, impl JobNews {
    fn opening(&self) -> Option<News> { None }
});

probe!(pricing_methods, impl Pricing {
    fn estimate(&self, _usage: pricing::Usage) -> Option<u64> { None }
    fn spend(&self) -> SpendTotals { SpendTotals::default() }
});

probe!(http_methods, impl Http {
    fn fetch(&self, _req: http::Request) -> BoxFuture<'_, Result<Response, HttpError>> {
        ready(Err(HttpError::Cancelled))
    }
});

probe!(body_methods, impl Body {
    fn read(&self, _max: u32) -> BoxFuture<'_, Result<Option<Vec<u8>>, String>> { ready(Ok(None)) }
});

probe!(agent_methods, impl tree::Agent {
    fn current_agent(&self) -> Option<Identity> { None }
});

probe!(agent_tree_methods, impl AgentTree {
    fn snapshot(&self) -> Vec<AgentNode> { Vec::new() }
});

probe!(mcp_methods, impl Mcp {
    fn prompts(&self) -> Vec<mcp::Prompt> { Vec::new() }
    fn get_prompt<'a>(&'a self, _name: &'a str) -> BoxFuture<'a, Result<Vec<mcp::PromptMessage>, String>> { ready(Ok(Vec::new())) }
    fn resources(&self) -> Vec<mcp::Resource> { Vec::new() }
    fn read_resource<'a>(&'a self, _name: &'a str) -> BoxFuture<'a, Result<Vec<Option<String>>, String>> { ready(Ok(Vec::new())) }
    fn sign_in_start<'a>(&'a self, _name: &'a str) -> BoxFuture<'a, Result<Option<String>, String>> { ready(Ok(None)) }
    fn sign_in_finish<'a>(&'a self, _name: &'a str, _address: &'a str) -> BoxFuture<'a, Result<u32, String>> { ready(Ok(0)) }
    fn servers(&self) -> Vec<mcp::Server> { Vec::new() }
    fn tools(&self) -> Vec<mcp::Tool> { Vec::new() }
    fn revision(&self) -> u64 { 0 }
    fn connect<'a>(&'a self, _name: &'a str) -> BoxFuture<'a, Result<(), String>> { ready(Ok(())) }
    fn call<'a>(&'a self, _ctx: &'a dyn CallContext, _name: &'a str, _input: &'a str, _limit_ms: u64) -> BoxFuture<'a, Result<ToolRun, String>> { ready(Ok(ToolRun::completed())) }
    fn add<'a>(&'a self, _name: &'a str, _command: &'a str, _args: &'a [String]) -> BoxFuture<'a, Result<u32, String>> { ready(Ok(0)) }
    fn remove<'a>(&'a self, _name: &'a str) -> BoxFuture<'a, Result<(), String>> { ready(Ok(())) }
});

probe!(model_methods, impl Model {
    fn ask<'a>(&'a self, _req: model::Prompt, _ctx: &'a dyn CallContext) -> BoxFuture<'a, Result<model::Reply, model::AskError>> { ready(Err(model::AskError::Cancelled)) }
});

probe!(chat_methods, impl Chat {
    fn rename(&self, _name: String) -> Result<String, String> { Ok(String::new()) }
});

probe!(project_methods, impl Project {
    fn key(&self) -> Option<String> { None }
});

probe!(manage_methods, impl Manage {
    fn manage<'a>(&'a self, _change: manage::Change, _ctx: &'a dyn CallContext) -> BoxFuture<'a, Result<String, String>> { ready(Ok(String::new())) }
});

probe!(usage_methods, impl Usage {
    fn accounts(&self) -> Vec<usage::AccountUsage> { Vec::new() }
});

probe!(agent_states_methods, impl AgentStates {
    fn states(&self) -> Vec<ShownState> { Vec::new() }
});

probe!(screen_report_methods, impl ScreenReports {
    fn wait(&self, _call: String, _within_ms: u64) -> BoxFuture<'_, Option<ScreenReport>> { ready(None) }
});

probe!(secrets_methods, impl Secrets {
    fn type_into<'a>(&'a self, _ask: SecretAsk, _ctx: &'a dyn CallContext) -> BoxFuture<'a, SecretEnd> { ready(SecretEnd::Failed) }
});

probe!(agent_rename_methods, impl AgentRename {
    fn rename_agent(&self, _name: String, _new_name: String, _ctx: &dyn CallContext) -> Result<host::Renamed, String> { Err(String::new()) }
});

probe!(config_methods, impl Config {
    fn settings(&self, _filter: Option<String>, _ctx: &dyn CallContext) -> Result<Vec<SettingRow>, String> { Ok(Vec::new()) }
});

probe!(accounts_methods, impl Accounts {
    fn list<'a>(&'a self, _ctx: &'a dyn CallContext) -> BoxFuture<'a, Result<AccountList, String>> { ready(Ok(AccountList::default())) }
});

probe!(view_methods, impl View {
    fn said(&self) -> Vec<strategy::SaidEntry> { Vec::new() }
    fn calls(&self) -> Vec<strategy::CallEntry> { Vec::new() }
    fn log_pointer(&self) -> Option<String> { None }
    fn output_pointer(&self) -> Option<String> { None }
});

probe!(index_methods, impl index::Index {
    fn entries(&self) -> Vec<index::Entry> { Vec::new() }
    fn open<'a>(&'a self, _name: &'a str) -> BoxFuture<'a, Result<String, String>> { ready(Ok(String::new())) }
    fn changed(&self) {}
});

impl Host for Probe {}

probe!(prepared_methods, impl Prepared {
    fn display(&self) -> Display { display() }
    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> { self }
});

probe!(plugin_methods, impl Plugin {
    fn entries(&self) -> BoxFuture<'_, Vec<index::Entry>> { ready(Vec::new()) }
    fn open_entry<'a>(&'a self, _name: &'a str) -> BoxFuture<'a, Result<String, String>> { ready(Ok(String::new())) }
    fn describe<'a>(&'a self, _ctx: &'a BuildContext) -> BoxFuture<'a, Vec<ToolDef>> {
        ready(Vec::new())
    }
    fn doctor<'a>(&'a self, _ctx: &'a BuildContext) -> BoxFuture<'a, Vec<DoctorRow>> {
        ready(Vec::new())
    }
    fn prepare<'a>(&'a self, _tool: &'a str, _input: &'a str) -> BoxFuture<'a, Result<Box<dyn Prepared>, String>> {
        ready(Ok(Box::new(Probe)))
    }
    fn rejected_display<'a>(&'a self, _tool: &'a str, _input: &'a str) -> BoxFuture<'a, Option<Display>> { ready(None) }
    fn time_limit_ms<'a>(&'a self, _tool: &'a str, _input: &'a str) -> BoxFuture<'a, Option<u64>> { ready(None) }
    fn movable<'a>(&'a self, _tool: &'a str, _input: &'a str) -> BoxFuture<'a, bool> { ready(false) }
    fn interrupted_result<'a>(&'a self, _tool: &'a str, _input: &'a str) -> BoxFuture<'a, Option<String>> { ready(None) }
    fn run<'a>(&'a self, _call: Box<dyn Prepared>, _ctx: &'a dyn CallContext) -> BoxFuture<'a, ToolRun> {
        ready(ToolRun::new(Outcome::Cancelled))
    }
    fn run_command<'a>(&'a self, _name: &'a str, _args: &'a str) -> BoxFuture<'a, CommandReply> {
        ready(CommandReply::None)
    }
    fn on_hook<'a>(&'a self, _hook: &'a str, _event: Event) -> BoxFuture<'a, Reply> {
        ready(Reply::None)
    }
    fn render_slot<'a>(&'a self, _name: &'a str, _view: SlotView) -> BoxFuture<'a, Result<Content, String>> {
        ready(Ok(Content::default()))
    }
    fn lock_for_render(&self) -> Option<Box<dyn Send>> { None }
    fn restarts_when_cut(&self) -> bool { false }
    fn render_section<'a>(&'a self, _name: &'a str, _at: At, _previous: Prior, _target: Option<&'a str>) -> BoxFuture<'a, Rendered> {
        ready(Rendered::default())
    }
    fn on_agent_hook<'a>(&'a self, _hook: &'a str, _event: TreeEvent) -> BoxFuture<'a, ()> {
        ready(())
    }
    fn instruction<'a>(&'a self, _info: StrategyInfo) -> BoxFuture<'a, Option<String>> {
        ready(None)
    }
    fn accept<'a>(&'a self, _info: StrategyInfo, _summary: String) -> BoxFuture<'a, Result<(), String>> {
        ready(Ok(()))
    }
    fn head<'a>(&'a self, _info: StrategyInfo, _view: &'a dyn View, _summary: Option<String>) -> BoxFuture<'a, Vec<String>> {
        ready(Vec::new())
    }
});

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Import,
    Export,
}

struct Trait {
    rust: &'static str,
    side: Side,
    sources: &'static [(&'static str, Option<&'static str>)],
    methods: fn() -> Vec<Method>,
    rust_only: &'static [&'static str],
}

fn traits() -> Vec<Trait> {
    let import = |rust, sources, methods| Trait {
        rust,
        side: Side::Import,
        sources,
        methods,
        rust_only: &[],
    };
    vec![
        import("Chat", &[("chat", None)], chat_methods),
        import("Project", &[("project", None)], project_methods),
        import("Mcp", &[("mcp", None)], mcp_methods),
        import("Manage", &[("manage", None)], manage_methods),
        import("Model", &[("model", None)], model_methods),
        import("Usage", &[("usage", None)], usage_methods),
        import("Index", &[("index", None)], index_methods),
        import("Session", &[("session", None)], session_methods),
        import("Workspace", &[("workspace", None)], workspace_methods),
        import("Clock", &[("clock", None)], clock_methods),
        import("Settings", &[("settings", None)], settings_methods),
        import("Context", &[("context", None)], context_methods),
        import("Media", &[("media", None)], media_methods),
        import(
            "CallContext",
            &[("call", Some("call-context"))],
            call_context_methods,
        ),
        import("Job", &[("jobs", Some("job"))], job_methods),
        import("Jobs", &[("jobs", None)], jobs_methods),
        import("Todos", &[("todos", None)], todos_methods),
        import("Agents", &[("agents", None)], agents_methods),
        import("Log", &[("log", None)], log_methods),
        import("Points", &[("points", None)], points_methods),
        import(
            "WorkspaceFacts",
            &[("workspace-facts", None)],
            workspace_facts_methods,
        ),
        import(
            "InstructionsReport",
            &[("instructions-report", None)],
            instructions_report_methods,
        ),
        import("JobNews", &[("job-news", None)], job_news_methods),
        import("Pricing", &[("pricing", None)], pricing_methods),
        import("Http", &[("http", None)], http_methods),
        import("Body", &[("http", Some("body"))], body_methods),
        import("Agent", &[("agent", None)], agent_methods),
        import("AgentTree", &[("agent-tree", None)], agent_tree_methods),
        import(
            "AgentStates",
            &[("agent-states", None)],
            agent_states_methods,
        ),
        import(
            "ScreenReports",
            &[("screen-report", None)],
            screen_report_methods,
        ),
        import("Secrets", &[("secrets", None)], secrets_methods),
        import(
            "AgentRename",
            &[("agent-rename", None)],
            agent_rename_methods,
        ),
        import("View", &[("history", Some("view"))], view_methods),
        import("Config", &[("config", None)], config_methods),
        import("Accounts", &[("accounts", None)], accounts_methods),
        Trait {
            rust: "Prepared",
            side: Side::Export,
            sources: &[("tools", Some("prepared"))],
            methods: prepared_methods,
            rust_only: &["into_any"],
        },
        Trait {
            rust: "Plugin",
            side: Side::Export,
            sources: &[
                ("tools", None),
                ("commands", None),
                ("hooks", None),
                ("slot-render", None),
                ("prompt-sections", None),
                ("agent-hooks", None),
                ("strategy", None),
                ("index-source", None),
            ],
            methods: plugin_methods,
            rust_only: &["lock_for_render", "restarts_when_cut"],
        },
    ]
}

const RENAMED: [(&str, &str, &str); 2] = [
    ("commands", "run", "run_command"),
    ("agent", "current", "current_agent"),
];

const TYPE_ONLY: [&str; 5] = [
    "types",
    "hook-types",
    "slot-types",
    "prompt-types",
    "agent-types",
];

fn snake(kebab: &str) -> String {
    kebab.replace('-', "_")
}

fn camel(kebab: &str) -> String {
    kebab
        .split('-')
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_ascii_uppercase().to_string() + chars.as_str()
            })
        })
        .collect()
}

fn rust_method(interface: &str, function: &str) -> String {
    RENAMED
        .iter()
        .find(|(i, f, _)| *i == interface && *f == function)
        .map_or_else(|| snake(function), |(_, _, rust)| (*rust).to_owned())
}

fn last_segment(path: &str) -> &str {
    path.rsplit("::").next().unwrap().trim()
}

struct World {
    resolve: Resolve,
    package: wit_parser::PackageId,
}

fn loaded(folders: &[&str]) -> World {
    let mut resolve = Resolve::new();
    let mut package = None;
    for folder in folders {
        package = Some(resolve.push_dir(Path::new(folder)).unwrap().0);
    }
    World {
        resolve,
        package: package.unwrap(),
    }
}

fn world_0_1_0() -> World {
    loaded(&[WIT_0_1_0])
}

fn world_0_2_0() -> World {
    loaded(&[WIT_0_1_0, WIT_0_2_0])
}

fn world_0_3_0() -> World {
    loaded(&[WIT_0_1_0, WIT_0_2_0, WIT_0_3_0])
}

fn world() -> World {
    loaded(&[WIT_0_1_0, WIT_0_2_0, WIT_0_3_0, WIT_0_4_0])
}

type Shapes = BTreeMap<(String, String), (Kind, Vec<String>)>;

struct WitFunction {
    name: String,
    is_async: bool,
}

type Functions = BTreeMap<(String, Option<String>), Vec<WitFunction>>;

impl World {
    fn interfaces(&self) -> Vec<(String, &wit_parser::Interface)> {
        self.resolve
            .packages
            .iter()
            .flat_map(|(_, package)| package.interfaces.iter())
            .map(|(name, id)| (name.clone(), &self.resolve.interfaces[*id]))
            .collect()
    }

    fn shapes(&self) -> Shapes {
        let mut out = Shapes::new();
        for (interface, item) in self.interfaces() {
            for (name, id) in &item.types {
                let shape = match &self.resolve.types[*id].kind {
                    TypeDefKind::Record(r) => (
                        Kind::Record,
                        r.fields.iter().map(|f| snake(&f.name)).collect(),
                    ),
                    TypeDefKind::Variant(v) => (
                        Kind::Variant,
                        v.cases.iter().map(|c| camel(&c.name)).collect(),
                    ),
                    TypeDefKind::Enum(e) => {
                        (Kind::Enum, e.cases.iter().map(|c| camel(&c.name)).collect())
                    }
                    TypeDefKind::Flags(f) => (
                        Kind::Flags,
                        f.flags.iter().map(|f| snake(&f.name)).collect(),
                    ),
                    TypeDefKind::Resource => (Kind::Resource, Vec::new()),
                    TypeDefKind::Type(_) => continue,
                    other => panic!("{interface}.{name}: unexpected named type {other:?}"),
                };
                out.insert((interface.clone(), name.clone()), shape);
            }
        }
        out
    }

    fn functions(&self) -> Functions {
        let mut out = Functions::new();
        for (interface, item) in self.interfaces() {
            for function in item.functions.values() {
                let owner = function.kind.resource().map(|id| {
                    self.resolve.types[id]
                        .name
                        .clone()
                        .unwrap_or_else(|| panic!("an unnamed resource in {interface}"))
                });
                assert!(
                    !matches!(function.kind, FunctionKind::Constructor(_)),
                    "{interface}: constructors have no mirror yet"
                );
                out.entry((interface.clone(), owner))
                    .or_default()
                    .push(WitFunction {
                        name: function.item_name().to_owned(),
                        is_async: function.kind.is_async(),
                    });
            }
        }
        out
    }

    fn side(&self, side: Side) -> BTreeSet<String> {
        let world = self.resolve.packages[self.package].worlds["plugin"];
        let world = &self.resolve.worlds[world];
        let items = match side {
            Side::Import => &world.imports,
            Side::Export => &world.exports,
        };
        items
            .iter()
            .map(|(key, item)| {
                assert!(
                    matches!(item, WorldItem::Interface { .. }),
                    "the world names a bare item: {}",
                    self.resolve.name_world_key(key)
                );
                match key {
                    WorldKey::Interface(id) => self.resolve.interfaces[*id].name.clone().unwrap(),
                    WorldKey::Name(name) => name.clone(),
                }
            })
            .collect()
    }
}

#[test]
fn every_wit_record_variant_enum_and_flags_has_a_rust_twin_with_the_same_names() {
    let world = world();
    let mut shapes = world.shapes();
    let points: Vec<String> = CORE_POINTS.iter().map(|p| camel(p)).collect();
    let event = &shapes[&("hook-types".to_owned(), "event".to_owned())].1;
    assert_eq!(
        event[..event.len() - 1],
        points,
        "the WIT event cases less custom"
    );
    let mut failures = Vec::new();
    for mirror in mirrors() {
        let key = (mirror.interface.to_owned(), mirror.wit.to_owned());
        let Some((kind, names)) = shapes.remove(&key) else {
            failures.push(format!(
                "{}.{} isn't in the WIT",
                mirror.interface, mirror.wit
            ));
            continue;
        };
        if kind != mirror.kind {
            failures.push(format!(
                "{}.{}: {kind:?} in the WIT, {:?} in Rust",
                key.0, key.1, mirror.kind
            ));
        }
        if last_segment(mirror.rust) != camel(mirror.wit) {
            failures.push(format!(
                "{}.{} is mirrored by {}",
                key.0, key.1, mirror.rust
            ));
        }
        let rust: Vec<String> = mirror.names.iter().map(|n| (*n).to_owned()).collect();
        if rust != names {
            failures.push(format!("{}.{}: WIT {names:?}, Rust {rust:?}", key.0, key.1));
        }
    }
    let resources: BTreeSet<(String, String)> = traits()
        .iter()
        .flat_map(|t| t.sources.iter())
        .filter_map(|(i, r)| r.map(|r| ((*i).to_owned(), r.to_owned())))
        .collect();
    for (key, (kind, _)) in shapes {
        if kind != Kind::Resource || !resources.contains(&key) {
            failures.push(format!("{}.{} ({kind:?}) has no Rust twin", key.0, key.1));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn every_wit_function_is_a_trait_method_and_async_ones_return_futures() {
    let world = world();
    let mut functions = world.functions();
    let mut failures = Vec::new();
    for t in traits() {
        let methods = (t.methods)();
        let mut expected: Vec<(String, bool)> = Vec::new();
        for (interface, resource) in t.sources {
            let key = ((*interface).to_owned(), resource.map(str::to_owned));
            let Some(found) = functions.remove(&key) else {
                failures.push(format!("{}: {key:?} has no functions in the WIT", t.rust));
                continue;
            };
            if let Some(resource) = resource
                && last_segment(t.rust) != camel(resource)
            {
                failures.push(format!("resource {resource} is mirrored by {}", t.rust));
            }
            expected.extend(
                found
                    .iter()
                    .map(|f| (rust_method(interface, &f.name), f.is_async)),
            );
        }
        let rust: BTreeSet<String> = methods
            .iter()
            .map(|m| m.name.to_owned())
            .filter(|m| !t.rust_only.contains(&m.as_str()))
            .collect();
        let wit: BTreeSet<String> = expected.iter().map(|(n, _)| n.clone()).collect();
        if rust != wit {
            failures.push(format!("{}: WIT {wit:?}, Rust {rust:?}", t.rust));
        }
        for (name, is_async) in &expected {
            if let Some(method) = methods.iter().find(|m| m.name == name) {
                let future = method.returns.starts_with("BoxFuture");
                if future != *is_async {
                    failures.push(format!(
                        "{}::{name}: async in the WIT is {is_async}, but it returns `{}`",
                        t.rust, method.returns
                    ));
                }
            }
        }
    }
    for (key, rest) in functions {
        let names: Vec<&str> = rest.iter().map(|f| f.name.as_str()).collect();
        failures.push(format!("{key:?} has no Rust trait: {names:?}"));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn mirrored(side: Side) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = traits()
        .iter()
        .filter(|t| t.side == side)
        .flat_map(|t| t.sources.iter().map(|(i, _)| (*i).to_owned()))
        .collect();
    if side == Side::Import {
        names.extend(TYPE_ONLY.iter().map(|i| (*i).to_owned()));
    }
    names
}

fn in_0_1_0(names: BTreeSet<String>) -> BTreeSet<String> {
    names
        .into_iter()
        .filter(|n| {
            !ADDED_IN_0_2_0.contains(&n.as_str())
                && !ADDED_IN_0_3_0.contains(&n.as_str())
                && !ADDED_IN_0_4_0.contains(&n.as_str())
        })
        .collect()
}

#[test]
fn the_world_imports_and_exports_what_the_traits_mirror() {
    let world = world();
    let imports = mirrored(Side::Import);
    let exports = mirrored(Side::Export);
    assert_eq!(world.side(Side::Import), imports);
    assert_eq!(world.side(Side::Export), exports);
    let declared: BTreeSet<String> = world.interfaces().into_iter().map(|(n, _)| n).collect();
    let all: BTreeSet<String> = imports.union(&exports).cloned().collect();
    assert_eq!(declared, all);
    let old = world_0_1_0();
    assert_eq!(old.side(Side::Import), in_0_1_0(imports));
    assert_eq!(old.side(Side::Export), in_0_1_0(exports));
    let declared: BTreeSet<String> = old.interfaces().into_iter().map(|(n, _)| n).collect();
    assert_eq!(declared, in_0_1_0(all));
}

#[test]
fn world_0_2_0_includes_every_item_of_0_1_0_from_its_folder_and_adds_only_its_own() {
    let new = world_0_2_0();
    let packages: Vec<String> = new
        .resolve
        .packages
        .iter()
        .map(|(_, p)| p.name.to_string())
        .collect();
    assert_eq!(packages, ["wisp:plugin@0.1.0", "wisp:plugin@0.2.0"]);
    let plugin = |w: &World, package: wit_parser::PackageId| {
        let id = w.resolve.packages[package].worlds["plugin"];
        w.resolve.worlds[id].clone()
    };
    let older = new
        .resolve
        .packages
        .iter()
        .find(|(_, p)| {
            p.name
                .version
                .as_ref()
                .is_some_and(|v| v.to_string() == "0.1.0")
        })
        .map(|(id, _)| id)
        .unwrap();
    let base = plugin(&new, older);
    let delta = plugin(&new, new.package);
    for (key, item) in &base.imports {
        assert_eq!(
            delta.imports.get(key),
            Some(item),
            "{}",
            new.resolve.name_world_key(key)
        );
    }
    for (key, item) in &base.exports {
        assert_eq!(
            delta.exports.get(key),
            Some(item),
            "{}",
            new.resolve.name_world_key(key)
        );
    }
    let names = |items: &wit_parser::IndexMap<WorldKey, WorldItem>, from: &wit_parser::World| {
        items
            .keys()
            .filter(|k| !from.imports.contains_key(*k) && !from.exports.contains_key(*k))
            .map(|k| new.resolve.name_world_key(k))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        names(&delta.imports, &base),
        [
            "wisp:plugin/workspace-facts@0.2.0",
            "wisp:plugin/instructions-report@0.2.0",
            "wisp:plugin/job-news@0.2.0",
            "wisp:plugin/pricing@0.2.0",
            "wisp:plugin/http@0.2.0",
            "wisp:plugin/agent-types@0.2.0",
            "wisp:plugin/agent@0.2.0",
            "wisp:plugin/agent-tree@0.2.0",
            "wisp:plugin/history@0.2.0",
            "wisp:plugin/config@0.2.0",
            "wisp:plugin/accounts@0.2.0",
            "wisp:plugin/slot-types@0.2.0",
            "wisp:plugin/prompt-types@0.2.0",
        ]
    );
    assert_eq!(
        names(&delta.exports, &base),
        [
            "wisp:plugin/slot-render@0.2.0",
            "wisp:plugin/prompt-sections@0.2.0",
            "wisp:plugin/agent-hooks@0.2.0",
            "wisp:plugin/strategy@0.2.0",
        ]
    );
    let mut alone = Resolve::new();
    assert!(alone.push_dir(Path::new(WIT_0_2_0)).is_err());
}

#[test]
fn world_0_3_0_replaces_slot_render_tools_and_commands_and_extends_their_types() {
    let all = world_0_3_0();
    let plugin = |w: &World| {
        let id = w.resolve.packages[w.package].worlds["plugin"];
        w.resolve.worlds[id].clone()
    };
    let named = |w: &World, items: &wit_parser::IndexMap<WorldKey, WorldItem>| {
        items
            .keys()
            .map(|k| w.resolve.name_world_key(k))
            .collect::<BTreeSet<_>>()
    };
    let old = world_0_2_0();
    let (base, delta) = (plugin(&old), plugin(&all));
    let swap = |names: BTreeSet<String>| -> BTreeSet<String> {
        names
            .into_iter()
            .map(|n| {
                n.replace("slot-render@0.2.0", "slot-render@0.3.0")
                    .replace("tools@0.1.0", "tools@0.3.0")
                    .replace("commands@0.1.0", "commands@0.3.0")
            })
            .collect()
    };
    let mut imports = named(&old, &base.imports);
    imports.insert("wisp:plugin/slot-types@0.3.0".to_owned());
    imports.insert("wisp:plugin/types@0.3.0".to_owned());
    imports.extend(
        ADDED_IN_0_3_0
            .into_iter()
            .filter(|name| *name != "index-source")
            .map(|name| format!("wisp:plugin/{name}@0.3.0")),
    );
    assert_eq!(named(&all, &delta.imports), imports);
    let mut exports = swap(named(&old, &base.exports));
    exports.insert("wisp:plugin/index-source@0.3.0".to_owned());
    assert_eq!(named(&all, &delta.exports), exports);
    let older = old.shapes();
    let newer = all.shapes();
    let key = |name: &str| ("slot-types".to_owned(), name.to_owned());
    let (blocks, more) = (&older[&key("block")].1, &newer[&key("block")].1);
    assert_eq!(more[..blocks.len()], blocks[..]);
    assert_eq!(more[blocks.len()..], ["Topology"]);
    assert_eq!(older[&key("content")], newer[&key("content")]);
    let type_key = |name: &str| ("types".to_owned(), name.to_owned());
    let (views, more) = (&older[&type_key("view")].1, &newer[&type_key("view")].1);
    assert_eq!(more[..views.len()], views[..]);
    assert_eq!(more[views.len()..], ["Code"]);
    assert_eq!(older[&type_key("display")], newer[&type_key("display")]);
    let declared = |w: &World, interface: &str| -> Vec<String> {
        let own = w.resolve.packages[w.package].interfaces[interface];
        w.resolve.interfaces[own]
            .types
            .iter()
            .filter(|(_, id)| !matches!(w.resolve.types[**id].kind, TypeDefKind::Type(_)))
            .map(|(name, _)| name.clone())
            .collect()
    };
    let tools = declared(&world_0_1_0(), "tools");
    assert_eq!(declared(&all, "tools"), tools);
    for name in &tools {
        let key = ("tools".to_owned(), name.clone());
        assert_eq!(older[&key], newer[&key], "{name}");
    }
    assert_eq!(
        declared(&all, "slot-types"),
        [
            "topology-node",
            "flow",
            "noun",
            "topology",
            "block",
            "content"
        ]
    );
    assert_eq!(
        declared(&all, "types"),
        ["hit", "fact", "tool-run", "code-view", "view", "display"]
    );
    assert_eq!(
        declared(&all, "commands"),
        ["command-row", "command-line", "command-reply"]
    );
    let reply = ("commands".to_owned(), "command-reply".to_owned());
    assert_eq!(newer[&reply].1[..older[&reply].1.len()], older[&reply].1);
    assert_eq!(newer[&reply].1.last().unwrap(), "Lines");
    let own = all.resolve.packages[all.package].interfaces["commands"];
    let run = &all.resolve.interfaces[own].functions["run"];
    assert_eq!(
        run.result,
        Some(wit_parser::Type::Id(
            all.resolve.interfaces[own].types["command-reply"]
        ))
    );
    let fact = type_key("fact");
    assert_eq!(newer[&fact].1[..older[&fact].1.len()], older[&fact].1);
    assert_eq!(newer[&fact].1.last().unwrap(), "Hits");
    assert_eq!(older[&type_key("tool-run")], newer[&type_key("tool-run")]);
    let mut alone = Resolve::new();
    assert!(alone.push_dir(Path::new(WIT_0_3_0)).is_err());
}

fn wit_files(folder: &str) -> Vec<String> {
    let mut files: Vec<String> = std::fs::read_dir(folder)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    files.sort();
    files
}

#[test]
fn the_parser_reads_every_async_func() {
    let world = world();
    let parsed = world
        .functions()
        .values()
        .flatten()
        .filter(|f| f.is_async)
        .count();
    let mut written = 0;
    for folder in [WIT_0_1_0, WIT_0_2_0, WIT_0_3_0, WIT_0_4_0] {
        for file in wit_files(folder) {
            let text = std::fs::read_to_string(Path::new(folder).join(file)).unwrap();
            written += text.matches("async func").count();
        }
    }
    assert!(written >= 19, "{written}");
    assert_eq!(parsed, written);
}

#[test]
fn each_folder_holds_one_package_and_each_later_one_only_its_delta() {
    assert_eq!(
        wit_files(WIT_0_1_0),
        [
            "commands.wit",
            "hooks.wit",
            "host.wit",
            "tools.wit",
            "types.wit",
            "world.wit"
        ]
    );
    assert_eq!(
        wit_files(WIT_0_2_0),
        [
            "config.wit",
            "http.wit",
            "pricing.wit",
            "prompt.wit",
            "slots.wit",
            "strategy.wit",
            "tree.wit",
            "world.wit"
        ]
    );
    let old = world_0_1_0();
    assert_eq!(
        old.resolve.packages[old.package].name.to_string(),
        "wisp:plugin@0.1.0"
    );
    assert_eq!(old.resolve.packages.len(), 1);
    assert_eq!(
        wit_files(WIT_0_3_0),
        [
            "commands.wit",
            "index.wit",
            "rename.wit",
            "screen-report.wit",
            "secrets.wit",
            "slots.wit",
            "states.wit",
            "tools.wit",
            "world.wit"
        ]
    );
    assert_eq!(
        wit_files(WIT_0_4_0),
        [
            "chat.wit",
            "manage.wit",
            "mcp.wit",
            "model.wit",
            "project.wit",
            "todos.wit",
            "usage.wit",
            "world.wit"
        ]
    );
    for (new, name, count) in [
        (world_0_2_0(), "wisp:plugin@0.2.0", 2),
        (world_0_3_0(), "wisp:plugin@0.3.0", 3),
        (world(), "wisp:plugin@0.4.0", 4),
    ] {
        assert_eq!(new.resolve.packages[new.package].name.to_string(), name);
        assert_eq!(new.resolve.packages.len(), count);
    }
}

#[test]
fn the_tree_imports_hold_one_plain_reader_each_and_use_only_their_own_types() {
    let world = world_0_2_0();
    let functions = world.functions();
    let plain = |interface: &str| -> Vec<(String, bool)> {
        functions[&(interface.to_owned(), None)]
            .iter()
            .map(|f| (f.name.clone(), f.is_async))
            .collect()
    };
    assert_eq!(plain("agent"), [("current".to_owned(), false)]);
    assert_eq!(plain("agent-tree"), [("snapshot".to_owned(), false)]);
    assert_eq!(plain("agent-hooks"), [("on-agent-hook".to_owned(), true)]);
    assert!(!functions.contains_key(&("agent-types".to_owned(), None)));
    let interfaces: BTreeMap<String, &wit_parser::Interface> =
        world.interfaces().into_iter().collect();
    let types_id = world.resolve.packages[world.package].interfaces["agent-types"];
    for name in ["agent-types", "agent", "agent-tree", "agent-hooks"] {
        for (item, id) in &interfaces[name].types {
            if let TypeDefKind::Type(wit_parser::Type::Id(used)) = world.resolve.types[*id].kind {
                assert_eq!(
                    world.resolve.types[used].owner,
                    wit_parser::TypeOwner::Interface(types_id),
                    "{name}.{item} uses a type from outside agent-types"
                );
            }
        }
    }
    let text = std::fs::read_to_string(Path::new(WIT_0_2_0).join("tree.wit")).unwrap();
    assert!(!text.contains("agents"), "{text}");
}

fn instantiate(host: Arc<dyn Host>) -> Arc<dyn Plugin> {
    let _ = host.current();
    Arc::new(Probe)
}

#[test]
fn a_plugin_is_instantiated_with_its_host_and_takes_back_its_prepared_value() {
    let bundled = Bundled {
        manifest: "",
        instantiate,
    };
    let plugin = (bundled.instantiate)(Arc::new(Probe));
    let prepared = now(plugin.prepare("probe", "{}")).unwrap();
    assert_eq!(prepared.display(), display());
    assert!(prepared.into_any().downcast::<Probe>().is_ok());
    let _: Box<dyn Job> = Probe
        .spawn(spawn_spec(), &Probe)
        .now_or_never()
        .unwrap()
        .unwrap();
}

fn spawn_spec() -> SpawnSpec {
    SpawnSpec {
        program: String::new(),
        args: Vec::new(),
        cwd: String::new(),
        env: Vec::new(),
        mode: Mode::Terminal,
        size: None,
        stdin: false,
        background: false,
        limit_ms: None,
        label: String::new(),
        keep: false,
    }
}

struct Older;

impl Plugin for Older {
    fn describe<'a>(&'a self, _ctx: &'a BuildContext) -> BoxFuture<'a, Vec<ToolDef>> {
        ready(Vec::new())
    }
    fn doctor<'a>(&'a self, _ctx: &'a BuildContext) -> BoxFuture<'a, Vec<DoctorRow>> {
        ready(Vec::new())
    }
    fn prepare<'a>(
        &'a self,
        tool: &'a str,
        _input: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Prepared>, String>> {
        Box::pin(std::future::ready(Err(format!("no tool {tool}"))))
    }
    fn rejected_display<'a>(
        &'a self,
        _tool: &'a str,
        _input: &'a str,
    ) -> BoxFuture<'a, Option<Display>> {
        Box::pin(std::future::ready(None))
    }
    fn time_limit_ms<'a>(&'a self, _tool: &'a str, _input: &'a str) -> BoxFuture<'a, Option<u64>> {
        Box::pin(std::future::ready(None))
    }
    fn movable<'a>(&'a self, _tool: &'a str, _input: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(std::future::ready(false))
    }
    fn interrupted_result<'a>(
        &'a self,
        _tool: &'a str,
        _input: &'a str,
    ) -> BoxFuture<'a, Option<String>> {
        Box::pin(std::future::ready(None))
    }
    fn run<'a>(
        &'a self,
        call: Box<dyn Prepared>,
        ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, ToolRun> {
        Probe.run(call, ctx)
    }
    fn run_command<'a>(&'a self, _name: &'a str, args: &'a str) -> BoxFuture<'a, CommandReply> {
        ready(CommandReply::Note(args.to_owned()))
    }
    fn on_hook<'a>(&'a self, _hook: &'a str, _event: Event) -> BoxFuture<'a, Reply> {
        ready(Reply::None)
    }
}

fn now<T>(future: BoxFuture<'_, T>) -> T {
    let mut future = future;
    let mut cx = TaskContext::from_waker(Waker::noop());
    match future.as_mut().poll(&mut cx) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("a ready future was pending"),
    }
}

#[test]
fn a_plugin_written_for_0_1_0_builds_and_runs_and_renders_empty_content_and_no_section() {
    let plugin: Arc<dyn Plugin> = Arc::new(Older);
    assert_eq!(
        now(plugin.run_command("c", "hi")),
        CommandReply::Note("hi".to_owned())
    );
    assert!(now(plugin.prepare("t", "{}")).is_err());
    for view in [SlotView::Panel, SlotView::Page] {
        assert_eq!(
            now(plugin.render_slot("usage", view)),
            Ok(Content::default())
        );
    }
    assert!(Content::default().note.is_empty() && Content::default().blocks.is_empty());
    for (at, previous, target) in [
        (At::Start, Prior::None, None),
        (At::Turn, Prior::Snapshot("s".to_owned()), None),
        (At::Head, Prior::Legacy(Some("t".to_owned())), Some("s")),
    ] {
        assert_eq!(
            now(plugin.render_section("environment", at, previous, target)),
            Rendered {
                snapshot: String::new(),
                text: None
            }
        );
    }
    let info = strategy_info();
    assert_eq!(now(plugin.instruction(info.clone())), None);
    assert_eq!(now(plugin.accept(info.clone(), "s".to_owned())), Ok(()));
    assert!(now(plugin.head(info, &Probe, Some("s".to_owned()))).is_empty());
}

fn strategy_info() -> StrategyInfo {
    StrategyInfo {
        compact: hooks::CompactInfo {
            trigger: "manual".to_owned(),
            engine: "wisp".to_owned(),
            turn: None,
            focus: None,
            estimate: 0,
            point: None,
            epoch: 0,
            requested_by: None,
            instruction: String::new(),
        },
        epoch: 1,
        first_turn: 1,
        last_turn: 1,
        native: false,
        cut_off: None,
        verbatim_tokens: 8_000,
    }
}

struct Bare;

impl Plugin for Bare {}

#[test]
fn a_plugin_that_writes_nothing_gets_the_documented_defaults() {
    const NO_TOOLS: &str = "this plugin has no tools";
    let plugin: Arc<dyn Plugin> = Arc::new(Bare);
    let ctx = BuildContext {
        platform: String::new(),
        capabilities: Vec::new(),
    };
    assert!(now(plugin.describe(&ctx)).is_empty());
    assert!(now(plugin.doctor(&ctx)).is_empty());
    assert_eq!(
        now(plugin.prepare("t", "{}")).err().as_deref(),
        Some(NO_TOOLS)
    );
    assert_eq!(now(plugin.rejected_display("t", "{}")), None);
    assert_eq!(now(plugin.time_limit_ms("t", "{}")), None);
    assert!(!now(plugin.movable("t", "{}")));
    assert_eq!(now(plugin.interrupted_result("t", "{}")), None);
    assert_eq!(
        now(plugin.run(Box::new(Probe), &Probe)),
        ToolRun::failed_to_start(NO_TOOLS).with_result(NO_TOOLS)
    );
    assert_eq!(now(plugin.run_command("c", "hi")), CommandReply::None);
    assert_eq!(now(plugin.on_hook("h", Event::SessionEnd)), Reply::None);
}

fn wit_cases(interface: &str, name: &str) -> Vec<String> {
    let (_, names) = world()
        .shapes()
        .remove(&(interface.to_owned(), name.to_owned()))
        .unwrap();
    names
}

#[test]
fn every_wit_named_key_round_trips_through_its_name() {
    let cases = wit_cases("types", "named-key");
    assert_eq!(cases.len(), types::NamedKey::ALL.len());
    for case in cases {
        let key = types::Key::parse(&case).unwrap_or_else(|| panic!("{case} doesn't parse"));
        assert!(matches!(key, types::Key::Named(_)), "{case}");
        assert_eq!(key.name(), case);
    }
}

#[test]
fn every_wit_read_kind_round_trips_through_its_name() {
    let cases = wit_cases("types", "read-kind");
    let names: Vec<String> = types::ReadKind::ALL
        .iter()
        .map(|kind| camel(kind.name()))
        .collect();
    assert_eq!(names, cases);
    for kind in types::ReadKind::ALL {
        assert_eq!(types::ReadKind::named(kind.name()), Some(kind));
    }
}

#[test]
fn world_0_4_0_includes_every_item_of_0_3_0_from_its_folder() {
    let old = world_0_3_0();
    let new = world();
    for side in [Side::Import, Side::Export] {
        let names = |w: &World| {
            let id = w.resolve.packages[w.package].worlds["plugin"];
            let world = &w.resolve.worlds[id];
            let items = match side {
                Side::Import => &world.imports,
                Side::Export => &world.exports,
            };
            items
                .keys()
                .map(|k| w.resolve.name_world_key(k))
                .collect::<BTreeSet<_>>()
        };
        let prior = names(&old);
        let current = names(&new);
        assert!(prior.is_subset(&current));
        if side == Side::Import {
            assert_eq!(
                current.difference(&prior).cloned().collect::<Vec<_>>(),
                [
                    "wisp:plugin/chat@0.4.0",
                    "wisp:plugin/manage@0.4.0",
                    "wisp:plugin/mcp@0.4.0",
                    "wisp:plugin/model@0.4.0",
                    "wisp:plugin/project@0.4.0",
                    "wisp:plugin/todos@0.4.0",
                    "wisp:plugin/usage@0.4.0"
                ]
            );
        } else {
            assert_eq!(current, prior);
        }
    }
    let key = ("todos".to_owned(), "todo".to_owned());
    let older = world_0_1_0().shapes();
    let newer = new.shapes();
    let (fields, more) = (&older[&key].1, &newer[&key].1);
    assert_eq!(more[..fields.len()], fields[..]);
    assert_eq!(more[fields.len()..], ["section"]);
    assert!(Resolve::new().push_dir(Path::new(WIT_0_4_0)).is_err());
}
