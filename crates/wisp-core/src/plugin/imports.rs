use wisp_plugin_api::manifest::at_least;

use super::caller::{Caller, Render};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Import {
    SessionCurrent,
    WorkspaceRoot,
    WorkspaceHome,
    WorkspaceShorten,
    WorkspaceKindOf,
    ClockNow,
    ClockLocalOffset,
    ClockElapsedMs,
    ClockSleep,
    SettingsGet,
    ContextCurrentFill,
    ContextEstimateText,
    MediaTypes,
    MediaSlices,
    MediaImageEdge,
    MediaCountTokens,
    CallId,
    CallCwd,
    CallDeadlineMs,
    CallCancelled,
    CallWaitCancelled,
    CallMoved,
    CallSteered,
    CallLiveTail,
    CallWriteOutput,
    CallOutputSummary,
    CallPrivateFile,
    CallAsk,
    JobInfo,
    JobWrite,
    JobSignal,
    JobResize,
    JobSetLimit,
    JobWait,
    JobHold,
    JobSettle,
    JobScreen,
    JobNewOutput,
    JobTail,
    JobOutput,
    JobSaved,
    JobLines,
    JobSearch,
    JobBackground,
    JobAskAttach,
    JobsSpawn,
    JobsAdopt,
    JobsAll,
    JobsMarkReported,
    JobsWhich,
    JobsCapture,
    TodosReplace,
    AgentsSpawnAgent,
    AgentsListAgents,
    AgentsReport,
    AgentsSendMessage,
    AgentsStopAgent,
    LogWrite,
    PointsFire,
    WorkspaceFactsEnvironment,
    WorkspaceFactsConfigDir,
    WorkspaceFactsDataDir,
    WorkspaceFactsTerminal,
    InstructionsReportLoaded,
    InstructionsReportWarning,
    InstructionsReportChanged,
    JobNewsOpening,
    PricingEstimate,
    PricingSpend,
    HttpFetch,
    HttpBodyRead,
    AgentCurrent,
    AgentTreeSnapshot,
    HistoryViewSaid,
    HistoryViewCalls,
    HistoryViewLogPointer,
    HistoryViewOutputPointer,
    ConfigSettings,
    AccountsList,
    AgentStatesStates,
    ScreenReportWait,
    SecretsTypeInto,
    AgentRenameRenameAgent,
    IndexEntries,
    IndexOpen,
    IndexChanged,
    ChatRename,
    ManageManage,
    ModelAsk,
    UsageAccounts,
    ProjectKey,
    SecretCheckCheck,
    PrivateFilesList,
    PrivateFilesRead,
    PrivateFilesChange,
    McpPrompts,
    McpGetPrompt,
    McpResources,
    McpReadResource,
    McpServers,
    McpTools,
    McpRevision,
    McpConnect,
    McpCall,
    McpAdd,
    McpRemove,
    McpSignInStart,
    McpSignInFinish,
    QuestionsPost,
    QuestionsWithdraw,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inside {
    Allowed,
    Quiet,
    Unavailable,
    NeedsCall,
}

pub const ALL: [Import; 110] = [
    Import::SessionCurrent,
    Import::WorkspaceRoot,
    Import::WorkspaceHome,
    Import::WorkspaceShorten,
    Import::WorkspaceKindOf,
    Import::ClockNow,
    Import::ClockLocalOffset,
    Import::ClockElapsedMs,
    Import::ClockSleep,
    Import::SettingsGet,
    Import::ContextCurrentFill,
    Import::ContextEstimateText,
    Import::MediaTypes,
    Import::MediaSlices,
    Import::MediaImageEdge,
    Import::MediaCountTokens,
    Import::CallId,
    Import::CallCwd,
    Import::CallDeadlineMs,
    Import::CallCancelled,
    Import::CallWaitCancelled,
    Import::CallMoved,
    Import::CallSteered,
    Import::CallLiveTail,
    Import::CallWriteOutput,
    Import::CallOutputSummary,
    Import::CallPrivateFile,
    Import::CallAsk,
    Import::JobInfo,
    Import::JobWrite,
    Import::JobSignal,
    Import::JobResize,
    Import::JobSetLimit,
    Import::JobWait,
    Import::JobHold,
    Import::JobSettle,
    Import::JobScreen,
    Import::JobNewOutput,
    Import::JobTail,
    Import::JobOutput,
    Import::JobSaved,
    Import::JobLines,
    Import::JobSearch,
    Import::JobBackground,
    Import::JobAskAttach,
    Import::JobsSpawn,
    Import::JobsAdopt,
    Import::JobsAll,
    Import::JobsMarkReported,
    Import::JobsWhich,
    Import::JobsCapture,
    Import::TodosReplace,
    Import::AgentsSpawnAgent,
    Import::AgentsListAgents,
    Import::AgentsReport,
    Import::AgentsSendMessage,
    Import::AgentsStopAgent,
    Import::LogWrite,
    Import::PointsFire,
    Import::WorkspaceFactsEnvironment,
    Import::WorkspaceFactsConfigDir,
    Import::WorkspaceFactsDataDir,
    Import::WorkspaceFactsTerminal,
    Import::InstructionsReportLoaded,
    Import::InstructionsReportWarning,
    Import::InstructionsReportChanged,
    Import::JobNewsOpening,
    Import::PricingEstimate,
    Import::PricingSpend,
    Import::HttpFetch,
    Import::HttpBodyRead,
    Import::AgentCurrent,
    Import::AgentTreeSnapshot,
    Import::HistoryViewSaid,
    Import::HistoryViewCalls,
    Import::HistoryViewLogPointer,
    Import::HistoryViewOutputPointer,
    Import::ConfigSettings,
    Import::AccountsList,
    Import::AgentStatesStates,
    Import::ScreenReportWait,
    Import::SecretsTypeInto,
    Import::AgentRenameRenameAgent,
    Import::IndexEntries,
    Import::IndexOpen,
    Import::IndexChanged,
    Import::ChatRename,
    Import::ManageManage,
    Import::ModelAsk,
    Import::UsageAccounts,
    Import::ProjectKey,
    Import::SecretCheckCheck,
    Import::PrivateFilesList,
    Import::PrivateFilesRead,
    Import::PrivateFilesChange,
    Import::McpPrompts,
    Import::McpGetPrompt,
    Import::McpResources,
    Import::McpReadResource,
    Import::McpServers,
    Import::McpTools,
    Import::McpRevision,
    Import::McpConnect,
    Import::McpCall,
    Import::McpAdd,
    Import::McpRemove,
    Import::McpSignInStart,
    Import::McpSignInFinish,
    Import::QuestionsPost,
    Import::QuestionsWithdraw,
];

impl Import {
    pub fn wit(self) -> &'static str {
        match self {
            Self::SessionCurrent => "session.current",
            Self::WorkspaceRoot => "workspace.root",
            Self::WorkspaceHome => "workspace.home",
            Self::WorkspaceShorten => "workspace.shorten",
            Self::WorkspaceKindOf => "workspace.kind-of",
            Self::ClockNow => "clock.now",
            Self::ClockLocalOffset => "clock.local-offset",
            Self::ClockElapsedMs => "clock.elapsed-ms",
            Self::ClockSleep => "clock.sleep",
            Self::SettingsGet => "settings.get",
            Self::ContextCurrentFill => "context.current-fill",
            Self::ContextEstimateText => "context.estimate-text",
            Self::MediaTypes => "media.media-types",
            Self::MediaSlices => "media.media-slices",
            Self::MediaImageEdge => "media.image-edge",
            Self::MediaCountTokens => "media.count-tokens",
            Self::CallId => "call.call-context.id",
            Self::CallCwd => "call.call-context.cwd",
            Self::CallDeadlineMs => "call.call-context.deadline-ms",
            Self::CallCancelled => "call.call-context.cancelled",
            Self::CallWaitCancelled => "call.call-context.wait-cancelled",
            Self::CallMoved => "call.call-context.moved",
            Self::CallSteered => "call.call-context.steered",
            Self::CallLiveTail => "call.call-context.live-tail",
            Self::CallWriteOutput => "call.call-context.write-output",
            Self::CallOutputSummary => "call.call-context.output-summary",
            Self::CallPrivateFile => "call.call-context.private-file",
            Self::CallAsk => "call.call-context.ask",
            Self::JobInfo => "jobs.job.info",
            Self::JobWrite => "jobs.job.write",
            Self::JobSignal => "jobs.job.signal",
            Self::JobResize => "jobs.job.resize",
            Self::JobSetLimit => "jobs.job.set-limit",
            Self::JobWait => "jobs.job.wait",
            Self::JobHold => "jobs.job.hold",
            Self::JobSettle => "jobs.job.settle",
            Self::JobScreen => "jobs.job.screen",
            Self::JobNewOutput => "jobs.job.new-output",
            Self::JobTail => "jobs.job.tail",
            Self::JobOutput => "jobs.job.output",
            Self::JobSaved => "jobs.job.saved",
            Self::JobLines => "jobs.job.lines",
            Self::JobSearch => "jobs.job.search",
            Self::JobBackground => "jobs.job.background",
            Self::JobAskAttach => "jobs.job.ask-attach",
            Self::JobsSpawn => "jobs.spawn",
            Self::JobsAdopt => "jobs.adopt",
            Self::JobsAll => "jobs.all",
            Self::JobsMarkReported => "jobs.mark-reported",
            Self::JobsWhich => "jobs.which",
            Self::JobsCapture => "jobs.capture",
            Self::TodosReplace => "todos.replace",
            Self::AgentsSpawnAgent => "agents.spawn-agent",
            Self::AgentsListAgents => "agents.list-agents",
            Self::AgentsReport => "agents.report",
            Self::AgentsSendMessage => "agents.send-message",
            Self::AgentsStopAgent => "agents.stop-agent",
            Self::LogWrite => "log.write",
            Self::PointsFire => "points.fire",
            Self::WorkspaceFactsEnvironment => "workspace-facts.environment",
            Self::WorkspaceFactsConfigDir => "workspace-facts.config-dir",
            Self::WorkspaceFactsDataDir => "workspace-facts.data-dir",
            Self::WorkspaceFactsTerminal => "workspace-facts.terminal",
            Self::InstructionsReportLoaded => "instructions-report.loaded",
            Self::InstructionsReportWarning => "instructions-report.warning",
            Self::InstructionsReportChanged => "instructions-report.changed",
            Self::JobNewsOpening => "job-news.opening",
            Self::PricingEstimate => "pricing.estimate",
            Self::PricingSpend => "pricing.spend",
            Self::HttpFetch => "http.fetch",
            Self::HttpBodyRead => "http.body.read",
            Self::AgentCurrent => "agent.current",
            Self::AgentTreeSnapshot => "agent-tree.snapshot",
            Self::HistoryViewSaid => "history.view.said",
            Self::HistoryViewCalls => "history.view.calls",
            Self::HistoryViewLogPointer => "history.view.log-pointer",
            Self::HistoryViewOutputPointer => "history.view.output-pointer",
            Self::ConfigSettings => "config.settings",
            Self::AccountsList => "accounts.list",
            Self::AgentStatesStates => "agent-states.states",
            Self::ScreenReportWait => "screen-report.wait",
            Self::SecretsTypeInto => "secrets.type-into",
            Self::AgentRenameRenameAgent => "agent-rename.rename-agent",
            Self::IndexEntries => "index.entries",
            Self::IndexOpen => "index.open",
            Self::IndexChanged => "index.changed",
            Self::ChatRename => "chat.rename",
            Self::ManageManage => "manage.manage",
            Self::ModelAsk => "model.ask",
            Self::UsageAccounts => "usage.accounts",
            Self::ProjectKey => "project.key",
            Self::SecretCheckCheck => "secret-check.check",
            Self::PrivateFilesList => "private-files.list",
            Self::PrivateFilesRead => "private-files.read",
            Self::PrivateFilesChange => "private-files.change",
            Self::McpPrompts => "mcp.prompts",
            Self::McpGetPrompt => "mcp.get-prompt",
            Self::McpResources => "mcp.resources",
            Self::McpReadResource => "mcp.read-resource",
            Self::McpServers => "mcp.servers",
            Self::McpTools => "mcp.tools",
            Self::McpRevision => "mcp.revision",
            Self::McpConnect => "mcp.connect",
            Self::McpCall => "mcp.call",
            Self::McpAdd => "mcp.add",
            Self::McpRemove => "mcp.remove",
            Self::McpSignInStart => "mcp.sign-in-start",
            Self::McpSignInFinish => "mcp.sign-in-finish",
            Self::QuestionsPost => "questions.post",
            Self::QuestionsWithdraw => "questions.withdraw",
        }
    }

    pub fn inside(self, render: Render) -> Inside {
        use Inside::{Allowed, NeedsCall, Quiet, Unavailable};
        let (slot, start) = match self {
            Self::McpPrompts
            | Self::McpGetPrompt
            | Self::McpResources
            | Self::McpReadResource
            | Self::McpServers
            | Self::McpTools
            | Self::McpRevision
            | Self::McpConnect
            | Self::McpCall
            | Self::McpAdd
            | Self::McpSignInStart
            | Self::McpSignInFinish
            | Self::McpRemove
            | Self::IndexEntries
            | Self::IndexOpen
            | Self::IndexChanged => (Unavailable, Unavailable),
            Self::WorkspaceRoot
            | Self::WorkspaceHome
            | Self::WorkspaceShorten
            | Self::WorkspaceKindOf
            | Self::SettingsGet
            | Self::ContextEstimateText => (Allowed, Allowed),
            Self::SessionCurrent
            | Self::ClockNow
            | Self::ClockLocalOffset
            | Self::ClockElapsedMs
            | Self::ContextCurrentFill
            | Self::MediaTypes
            | Self::MediaSlices
            | Self::MediaImageEdge
            | Self::JobsWhich
            | Self::PricingEstimate
            | Self::PricingSpend
            | Self::AgentCurrent
            | Self::AgentTreeSnapshot
            | Self::AgentStatesStates
            | Self::UsageAccounts
            | Self::SecretCheckCheck => (Allowed, Unavailable),
            Self::JobInfo | Self::JobsAll => (Quiet, Unavailable),
            Self::LogWrite
            | Self::WorkspaceFactsConfigDir
            | Self::InstructionsReportLoaded
            | Self::InstructionsReportWarning
            | Self::InstructionsReportChanged => (Unavailable, Allowed),
            Self::ModelAsk
            | Self::ClockSleep
            | Self::MediaCountTokens
            | Self::JobWrite
            | Self::JobSignal
            | Self::JobResize
            | Self::JobSetLimit
            | Self::JobWait
            | Self::JobHold
            | Self::JobSettle
            | Self::JobScreen
            | Self::JobNewOutput
            | Self::JobTail
            | Self::JobOutput
            | Self::JobSaved
            | Self::JobLines
            | Self::JobSearch
            | Self::JobBackground
            | Self::JobAskAttach
            | Self::JobsCapture
            | Self::WorkspaceFactsEnvironment
            | Self::WorkspaceFactsDataDir
            | Self::PrivateFilesList
            | Self::PrivateFilesRead
            | Self::PrivateFilesChange
            | Self::ProjectKey
            | Self::WorkspaceFactsTerminal
            | Self::JobNewsOpening
            | Self::PointsFire
            | Self::HttpFetch
            | Self::HttpBodyRead
            | Self::HistoryViewSaid
            | Self::HistoryViewCalls
            | Self::HistoryViewLogPointer
            | Self::HistoryViewOutputPointer
            | Self::ScreenReportWait => (Unavailable, Unavailable),
            Self::CallId
            | Self::CallCwd
            | Self::CallDeadlineMs
            | Self::CallCancelled
            | Self::CallWaitCancelled
            | Self::CallMoved
            | Self::CallSteered
            | Self::CallLiveTail
            | Self::CallWriteOutput
            | Self::CallOutputSummary
            | Self::CallPrivateFile
            | Self::CallAsk
            | Self::JobsSpawn
            | Self::JobsAdopt
            | Self::JobsMarkReported
            | Self::TodosReplace
            | Self::AgentsSpawnAgent
            | Self::AgentsListAgents
            | Self::AgentsReport
            | Self::AgentsSendMessage
            | Self::AgentsStopAgent
            | Self::AgentRenameRenameAgent
            | Self::SecretsTypeInto
            | Self::ConfigSettings
            | Self::ChatRename
            | Self::ManageManage
            | Self::QuestionsPost
            | Self::QuestionsWithdraw
            | Self::AccountsList => (NeedsCall, NeedsCall),
        };
        match render {
            Render::Slot => slot,
            Render::Start => start,
        }
    }

    pub fn world(self) -> &'static str {
        match self {
            Self::ChatRename
            | Self::ManageManage
            | Self::ModelAsk
            | Self::UsageAccounts
            | Self::ProjectKey
            | Self::SecretCheckCheck
            | Self::PrivateFilesList
            | Self::PrivateFilesRead
            | Self::PrivateFilesChange
            | Self::McpPrompts
            | Self::McpGetPrompt
            | Self::McpResources
            | Self::McpReadResource
            | Self::McpServers
            | Self::McpTools
            | Self::McpRevision
            | Self::McpConnect
            | Self::McpCall
            | Self::McpAdd
            | Self::McpSignInStart
            | Self::McpSignInFinish
            | Self::McpRemove
            | Self::QuestionsPost
            | Self::QuestionsWithdraw
            | Self::MediaSlices => "0.4.0",
            Self::IndexEntries
            | Self::IndexOpen
            | Self::IndexChanged
            | Self::AgentStatesStates
            | Self::ScreenReportWait
            | Self::SecretsTypeInto
            | Self::AgentRenameRenameAgent => "0.3.0",
            Self::WorkspaceFactsEnvironment
            | Self::WorkspaceFactsConfigDir
            | Self::WorkspaceFactsDataDir
            | Self::WorkspaceFactsTerminal
            | Self::InstructionsReportLoaded
            | Self::InstructionsReportWarning
            | Self::InstructionsReportChanged
            | Self::JobNewsOpening
            | Self::PricingEstimate
            | Self::PricingSpend
            | Self::HttpFetch
            | Self::HttpBodyRead
            | Self::AgentCurrent
            | Self::AgentTreeSnapshot
            | Self::HistoryViewSaid
            | Self::HistoryViewCalls
            | Self::HistoryViewLogPointer
            | Self::HistoryViewOutputPointer
            | Self::ConfigSettings
            | Self::AccountsList => "0.2.0",
            _ => "0.1.0",
        }
    }

    pub fn spoken_in(self, world: &str) -> bool {
        at_least(world, self.world())
    }

    pub fn current(self) -> Inside {
        let caller = Caller::current();
        match caller.render {
            Some(render) => self.inside(render),
            None if caller.quiet_jobs && matches!(self, Self::JobsAll | Self::JobInfo) => {
                Inside::Quiet
            }
            None if caller.describing
                && matches!(self, Self::ProjectKey | Self::SecretCheckCheck) =>
            {
                Inside::Unavailable
            }
            None => Inside::Allowed,
        }
    }

    pub fn refused(self) -> bool {
        self.current() != Inside::Allowed
    }
}

#[cfg(test)]
mod tests;
