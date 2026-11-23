use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio_util::sync::CancellationToken;
use wisp_plugin_shell::Timing;
use wisp_protocol::{ToolOutcome, WorkspaceChange};
use wisp_tool_text::output::Captured;

use super::host::{KernelCall, KernelHost};
use crate::agents::AgentsHandle;
use crate::call::user::UserChannel;
use crate::call::{FileTarget, OutputDir, OutputSink, result_text};
use crate::jobs::{CallEnd, Carrier, Processes};
use crate::tool::testkit::{BASH_VERSION, shell_on};
use crate::tool::{ToolEntry, ToolRun, Toolbox};
use crate::workspace::Workspace;

mod bash;
mod process;
mod secret;

#[derive(Clone, Default)]
pub(crate) struct Ctl {
    pub(crate) cancel: CancellationToken,
    pub(crate) moved: CancellationToken,
    pub(crate) steered: CancellationToken,
}

pub(crate) struct Called {
    pub(crate) run: ToolRun,
    pub(crate) captured: Captured,
    pub(crate) text: String,
    pub(crate) number: Option<u32>,
    pub(crate) ends: Vec<String>,
    pub(crate) kept_lines: bool,
}

pub(crate) struct Begun {
    pub(crate) entry: ToolEntry,
    pub(crate) plugin: Arc<dyn wisp_plugin_api::Plugin>,
    pub(crate) call: Arc<KernelCall>,
    pub(crate) input: Option<Box<dyn wisp_plugin_api::tools::Prepared>>,
}

pub(crate) struct Shell {
    pub(crate) host: Arc<KernelHost>,
    pub(crate) toolbox: Toolbox,
    pub(crate) processes: Processes,
    pub(crate) out: OutputDir,
    pub(crate) user: UserChannel,
    root: PathBuf,
    home: Option<PathBuf>,
    calls: u32,
}

pub(crate) fn host_with_capture(host: KernelHost) -> KernelHost {
    host.with_capture(wisp_plugin_api::host::Captured::Output(
        BASH_VERSION.as_bytes().to_vec(),
    ))
}

impl Shell {
    pub(crate) fn new(host: KernelHost, timing: Timing, out: OutputDir, root: &Path) -> Self {
        let host = Arc::new(host_with_capture(host));
        let toolbox = shell_on(host.clone(), timing);
        let shell = Self {
            host,
            toolbox,
            processes: Processes::default(),
            out,
            user: UserChannel::none(),
            root: root.to_path_buf(),
            home: None,
            calls: 0,
        };
        shell.bind();
        shell
    }

    fn bind(&self) {
        self.host.bind(
            &self.processes,
            &Workspace {
                root: self.root.clone(),
                home: self.home.clone(),
                temp: self.root.clone(),
            },
        );
    }

    pub(crate) fn set_home(&mut self, home: &Path) {
        self.home = Some(home.to_path_buf());
        self.bind();
    }

    pub(crate) fn rebind(&mut self, processes: Processes) {
        self.processes = processes;
        self.bind();
    }

    pub(crate) fn entry(&self, name: &str) -> ToolEntry {
        self.toolbox.get(name).cloned().unwrap()
    }

    pub(crate) fn calls(&self) -> u32 {
        self.calls
    }

    pub(crate) fn limit_of(&self, name: &str, input: &Value) -> Option<tokio::time::Instant> {
        futures::executor::block_on(self.entry(name).time_limit(input))
            .map(|limit| tokio::time::Instant::now() + limit + crate::agent::STOP_MARGIN)
    }

    pub(crate) fn begin(
        &mut self,
        name: &str,
        input: &Value,
        cwd: &Path,
        ctl: Ctl,
        deadline: Option<tokio::time::Instant>,
    ) -> Result<Begun, String> {
        let entry = self.entry(name);
        let prepared = futures::executor::block_on(entry.prepare(input)).map_err(|e| e.message)?;
        self.calls += 1;
        let call_id = format!("call-{}", self.calls);
        let sink = if entry.meta().saves_output {
            let target: FileTarget = self
                .out
                .create(self.calls, name)
                .map(|(p, f)| (p, Box::new(f) as Box<dyn std::io::Write + Send>))
                .map_err(|e| e.to_string());
            OutputSink::new(call_id.clone(), None, target)
        } else {
            OutputSink::detached(call_id.clone())
        };
        if futures::executor::block_on(entry.movable(input)) {
            self.processes.set_moving(&call_id, ctl.moved.clone());
        }
        let call = Arc::new(KernelCall {
            asker: None,
            model_asks: Mutex::new(Vec::new()),
            call_id: call_id.clone(),
            cancel: ctl.cancel,
            steered: ctl.steered,
            moved: ctl.moved,
            deadline,
            user: self.user.clone(),
            cwd: cwd.to_path_buf(),
            live_tail: false,
            sink: Mutex::new(sink),
            host: self.host.clone(),
            agents: AgentsHandle::none(),
            todos: Mutex::new(None),
            plugin: entry.tool().id().to_owned(),
            view: None,
        });
        self.host.open_call(call.clone());
        Ok(Begun {
            plugin: entry.tool().plugin().clone(),
            entry,
            call,
            input: Some(prepared.into_plugin()),
        })
    }

    pub(crate) async fn call(
        &mut self,
        name: &str,
        input: &Value,
        cwd: &Path,
        ctl: Ctl,
        deadline: Option<tokio::time::Instant>,
        beside: impl Future<Output = ()>,
    ) -> Result<Called, String> {
        let mut begun = self.begin(name, input, cwd, ctl, deadline)?;
        let prepared = begun.input.take().unwrap();
        let (run, ()) = tokio::join!(begun.plugin.run(prepared, begun.call.as_ref()), beside);
        Ok(self.finish(&begun, Some(run)).await)
    }

    pub(crate) async fn finish(
        &self,
        begun: &Begun,
        run: Option<wisp_plugin_api::types::ToolRun>,
    ) -> Called {
        let call_id = begun.call.call_id.clone();
        let abandoned = run.is_none();
        let run = match run {
            Some(run) => super::convert::run(run).unwrap_or_else(|message| {
                ToolRun::new(ToolOutcome::Failed {
                    message,
                    before_start: false,
                })
            }),
            None => ToolRun::new(ToolOutcome::Cancelled),
        };
        let ends = if !abandoned && begun.entry.meta().reports_jobs {
            self.processes.take_ends(&Carrier::Call(call_id.clone()))
        } else {
            Vec::new()
        };
        self.host.close_call(&call_id);
        let mut sink = begun.call.take_sink();
        self.processes.clear_moving(&call_id);
        let (captured, kept_lines) = capture(&self.processes, &call_id, abandoned, &mut sink).await;
        let notes: Vec<String> = run.trailer.clone().into_iter().collect();
        let text = match run.result.clone() {
            Some(own) => std::iter::once(own)
                .chain(notes.iter().cloned())
                .collect::<Vec<_>>()
                .join("\n"),
            None => result_text(&run.outcome, &captured, None, &notes),
        };
        Called {
            run,
            captured,
            text,
            number: self.processes.take_number(&call_id),
            ends,
            kept_lines,
        }
    }

    pub(crate) fn cwd_of(run: &ToolRun) -> Option<PathBuf> {
        match &run.state {
            Some(WorkspaceChange::Cwd { path }) => Some(PathBuf::from(path)),
            _ => None,
        }
    }
}

async fn finished(sink: &mut OutputSink, unless_empty: bool) -> (Captured, bool) {
    let kept = sink.keeps_lines();
    let captured = if unless_empty {
        sink.finish_unless_empty().await
    } else {
        sink.finish().await
    };
    (captured, kept)
}

async fn capture(
    processes: &Processes,
    call_id: &str,
    abandoned: bool,
    sink: &mut OutputSink,
) -> (Captured, bool) {
    match processes.end_call(call_id, abandoned) {
        CallEnd::NoJob => finished(sink, true).await,
        CallEnd::Ended(job) => {
            let taken = job.slot().lock().await.take();
            job.stop().await;
            match taken {
                Some(mut own) => finished(&mut own, true).await,
                None => finished(sink, false).await,
            }
        }
        CallEnd::Abandoned(job) => {
            let taken = job.slot().lock().await.take();
            job.stop().await;
            match taken {
                Some(mut own) => finished(&mut own, false).await,
                None => finished(sink, false).await,
            }
        }
        CallEnd::Outlives(slot, state) => {
            let mut held = slot.lock().await;
            match held.as_mut() {
                Some(own) => {
                    let was_on = own.stop_events().await;
                    state.events_stopped(was_on);
                    (own.snapshot(), own.keeps_lines())
                }
                None => finished(sink, false).await,
            }
        }
    }
}

pub(super) const LONG_SIZES: [usize; 2] = [200 * 1024, wisp_plugin_shell::MAX_COMMAND_BYTES];

pub(super) fn long_command(size: usize, body: &str) -> String {
    let head = "cat >/dev/null <<'WISP_PAD'\n";
    let tail = format!("\nWISP_PAD\n{body}");
    let pad = size
        .checked_sub(head.len() + tail.len())
        .expect("body exceeds target size");
    let command = format!("{head}{}{tail}", "a".repeat(pad));
    assert_eq!(command.len(), size);
    command
}
