use jiff::Timestamp;

use crate::bindings::exports::wisp::plugin0_1_0::tools::{
    self, BuildContext, ClientTool, DoctorRow, Effect, Purpose, Tags, Timeouts, ToolDef, ToolKind,
    ToolMeta,
};
use crate::bindings::exports::wisp::plugin0_1_0::{commands, hooks};
use crate::bindings::exports::wisp::plugin0_2_0::{
    agent_hooks, prompt_sections, slot_render, strategy,
};
use crate::bindings::wisp::plugin0_1_0::call::CallContext;
use crate::bindings::wisp::plugin0_1_0::clock::{self, Instant};
use crate::bindings::wisp::plugin0_1_0::types::{
    Action, Completed, Display, Failure, Outcome, OutputKind, ToolRun, Verbs,
};
use crate::bindings::wisp::plugin0_2_0::agent_types::AgentState;
use crate::bindings::wisp::plugin0_2_0::{agent, agent_tree, workspace_facts};
use crate::{Agent, LIMIT_MS, Me, READ, Reply, Shown};

pub(crate) struct Plugin;

struct Wasm;

impl crate::Host for Wasm {
    fn me(&self) -> Option<Me> {
        agent::current().map(|me| Me {
            path: me.path,
            session: me.session_id,
            root: me.root_session_id,
        })
    }

    fn agents(&self) -> Vec<Agent> {
        agent_tree::snapshot()
            .into_iter()
            .map(|node| Agent {
                path: node.path,
                session: node.session_id,
                ended: matches!(node.state, AgentState::Ended),
            })
            .collect()
    }

    fn now(&self) -> Timestamp {
        let at = clock::now();
        let nanos = i32::try_from(at.nanos).unwrap_or_default();
        Timestamp::new(at.seconds, nanos).unwrap_or(Timestamp::UNIX_EPOCH)
    }

    fn offset(&self, at: Timestamp) -> i32 {
        let nanos = u32::try_from(at.subsec_nanosecond()).unwrap_or_default();
        clock::local_offset(Instant {
            seconds: at.as_second(),
            nanos,
        })
        .unwrap_or_default()
    }

    fn data_dir(&self) -> Result<String, String> {
        workspace_facts::data_dir()
    }
}

fn meta(tool: &str) -> ToolMeta {
    let read = tool == READ;
    ToolMeta {
        effect: if read {
            Effect::ReadOnly
        } else {
            Effect::LocalTxn
        },
        tags: Tags::empty(),
        parallel_safe: read,
        timeouts: Timeouts {
            default_ms: LIMIT_MS,
            max_ms: Some(LIMIT_MS),
        },
        saves_output: read,
        needs_workspace: false,
        ends_reply: false,
        asks_user: false,
        reports_jobs: false,
        writing: None,
        purpose: Purpose::Run,
    }
}

fn display(shown: Shown) -> Display {
    Display {
        action: if shown.write {
            Action::Write
        } else {
            Action::Read
        },
        verbs: shown.verbs.map(|[doing, done, failed]| Verbs {
            doing: doing.to_owned(),
            done: done.to_owned(),
            failed: failed.to_owned(),
        }),
        target: shown.target,
        output: OutputKind::Text,
        view: None,
        title: None,
    }
}

fn ran(outcome: Outcome, result: Option<String>) -> ToolRun {
    ToolRun {
        outcome,
        facts: Vec::new(),
        cwd: None,
        trailer: None,
        result,
        attachments: Vec::new(),
        typed: None,
    }
}

fn completed(is_error: bool, result: Option<String>) -> ToolRun {
    ran(
        Outcome::Completed(Completed {
            is_error,
            exit: None,
        }),
        result,
    )
}

pub(crate) struct Call(crate::Call);

impl tools::GuestPrepared for Call {
    fn display(&self) -> Display {
        display(self.0.shown.clone())
    }
}

impl tools::Guest for Plugin {
    type Prepared = Call;

    async fn describe(_ctx: BuildContext) -> Vec<ToolDef> {
        crate::definitions()
            .into_iter()
            .map(|def| ToolDef {
                name: def.name.to_owned(),
                kind: ToolKind::Client(ClientTool {
                    description: def.description.to_owned(),
                    input_schema: def.schema,
                    meta: meta(def.name),
                }),
                system_line: None,
                context_line: None,
            })
            .collect()
    }

    async fn doctor(_ctx: BuildContext) -> Vec<DoctorRow> {
        Vec::new()
    }

    async fn prepare(tool: String, input: String) -> Result<tools::Prepared, String> {
        crate::prepare(&tool, &input).map(|call| tools::Prepared::new(Call(call)))
    }

    async fn rejected_display(tool: String, input: String) -> Option<Display> {
        crate::rejected(&tool, &input).map(display)
    }

    async fn time_limit_ms(_tool: String, _input: String) -> Option<u64> {
        Some(LIMIT_MS)
    }

    async fn movable(_tool: String, _input: String) -> bool {
        false
    }

    async fn interrupted_result(_tool: String, _input: String) -> Option<String> {
        None
    }

    async fn run(call: tools::Prepared, ctx: &CallContext) -> ToolRun {
        let Call(call) = call.into_inner();
        match crate::run(&Wasm, &call.request) {
            Reply::View(text) => {
                ctx.write_output(text.as_bytes());
                completed(false, None)
            }
            Reply::Line(line) => completed(false, Some(line)),
            Reply::Refused(sentence) => completed(true, Some(sentence)),
            Reply::Failed(message) => ran(
                Outcome::Failed(Failure {
                    message,
                    before_start: false,
                }),
                None,
            ),
        }
    }
}

impl commands::Guest for Plugin {
    async fn run(_name: String, _args: String) -> commands::CommandReply {
        commands::CommandReply::None
    }
}

impl hooks::Guest for Plugin {
    async fn on_hook(_hook: String, _ev: hooks::Event) -> hooks::Reply {
        hooks::Reply::None
    }
}

impl slot_render::Guest for Plugin {
    async fn render_slot(
        _name: String,
        _view: slot_render::SlotView,
    ) -> Result<slot_render::Content, String> {
        Ok(slot_render::Content {
            note: Vec::new(),
            blocks: Vec::new(),
        })
    }
}

impl prompt_sections::Guest for Plugin {
    async fn render_section(
        _name: String,
        _at: prompt_sections::At,
        _previous: prompt_sections::Prior,
        _target: Option<String>,
    ) -> prompt_sections::Rendered {
        prompt_sections::Rendered {
            snapshot: String::new(),
            text: None,
        }
    }
}

impl agent_hooks::Guest for Plugin {
    async fn on_agent_hook(_hook: String, _ev: agent_hooks::TreeEvent) {}
}

impl strategy::Guest for Plugin {
    async fn instruction(_info: strategy::StrategyInfo) -> Option<String> {
        None
    }

    async fn accept(_info: strategy::StrategyInfo, _summary: String) -> Result<(), String> {
        Ok(())
    }

    async fn head(
        _info: strategy::StrategyInfo,
        _view: &strategy::View,
        _summary: Option<String>,
    ) -> Vec<String> {
        Vec::new()
    }
}
