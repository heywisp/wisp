use std::any::Any;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures::future::BoxFuture;
use tokio::sync::{RwLock, RwLockReadGuard, oneshot};
use wasmtime::component::{Accessor, Resource, ResourceAny};
use wisp_plugin_api::commands::CommandReply;
use wisp_plugin_api::hooks::{Event, Reply};
use wisp_plugin_api::host::{CallContext, Host};
use wisp_plugin_api::index::Entry;
use wisp_plugin_api::prompt::{At, Prior, Rendered};
use wisp_plugin_api::slots::{Content, SlotView};
use wisp_plugin_api::strategy::{StrategyInfo, View};
use wisp_plugin_api::tools::{BuildContext, DoctorRow, Prepared, ToolDef};
use wisp_plugin_api::tree::TreeEvent;
use wisp_plugin_api::types::{Display, Failure, Outcome, ToolRun};
use wisp_plugin_api::{Plugin, Scope};

use crate::context::CallLink;
use crate::exports::{Exports, Pre, with_commands, with_slots, with_tools};
use crate::imports::Viewed;
use crate::instance::{CUT, Live, Mark, Marked, SETTING_UP, Start, Work, call, start};
use crate::mirror::Mirror;
use crate::scope::{Unscoped, caller_of};
use crate::state::State;

pub(crate) const UNBOUNDED: Duration = Duration::from_secs(5);

enum Stop {
    Ended(String),
    Restarted,
}

#[derive(Default)]
struct Slot {
    live: Option<Live>,
    generation: u64,
    build: Option<BuildContext>,
}

struct Shared {
    id: String,
    pre: Pre,
    host: Arc<dyn Host>,
    env: Arc<[(String, String)]>,
    slot: Mutex<Slot>,
    render: Arc<RwLock<()>>,
}

pub(crate) struct WasmPlugin(Arc<Shared>);

struct WasmPrepared {
    handle: Option<ResourceAny>,
    display: Display,
    generation: u64,
    shared: Arc<Shared>,
}

impl Prepared for WasmPrepared {
    fn display(&self) -> Display {
        self.display.clone()
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> {
        self
    }
}

impl Drop for WasmPrepared {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            self.shared.release(self.generation, handle);
        }
    }
}

struct Guard<'a> {
    shared: &'a Shared,
    generation: u64,
    armed: bool,
}

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.shared.cut(self.generation);
        }
    }
}

fn describe_work(build: BuildContext) -> Work {
    Box::new(move |accessor, exports| {
        Box::pin(async move {
            with_tools!(&exports.tools, |tools| {
                let describe = tools.func_describe();
                call(accessor, Arc::new(Unscoped), describe, (build.mirror(),)).await?;
                Ok(())
            })
        })
    })
}

impl Shared {
    fn slot(&self) -> MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn current(&self, expected: Option<u64>) -> Result<(Live, bool), Stop> {
        let mut slot = self.slot();
        if let Some(live) = slot.live.as_ref().filter(|live| live.running()) {
            return match expected {
                Some(generation) if generation != live.generation => Err(Stop::Restarted),
                _ => Ok((live.clone(), false)),
            };
        }
        if expected.is_some() {
            return Err(Stop::Restarted);
        }
        slot.generation += 1;
        let restart = slot.generation > 1;
        let live = start(Start {
            pre: self.pre.clone(),
            host: self.host.clone(),
            env: self.env.clone(),
            generation: slot.generation,
            first: slot.build.clone().filter(|_| restart).map(describe_work),
            ready: !restart,
        });
        slot.live = Some(live.clone());
        Ok((live, restart))
    }

    fn key(&self) -> usize {
        std::ptr::from_ref(self).addr()
    }

    fn mark(&self) -> Option<Mark> {
        SETTING_UP
            .try_with(|mark| *mark)
            .ok()
            .filter(|mark| mark.plugin == self.key())
    }

    async fn hold(&self) -> Option<RwLockReadGuard<'_, ()>> {
        match self.mark() {
            Some(_) => None,
            None => Some(self.render.read().await),
        }
    }

    async fn live(&self, expected: Option<u64>) -> Result<Live, Stop> {
        if let Some(mark) = self.mark() {
            return self.current(Some(mark.generation)).map(|(live, _)| live);
        }
        let (live, restart) = self.current(expected)?;
        if restart {
            self.set_up(&live).await;
        }
        if live.settled().await {
            Ok(live)
        } else {
            Err(Stop::Ended(live.ended.reason().await))
        }
    }

    async fn set_up(&self, live: &Live) {
        let mut guard = Guard {
            shared: self,
            generation: live.generation,
            armed: true,
        };
        if let Some(kernel) = self.host.kernel() {
            let mark = Mark {
                plugin: self.key(),
                generation: live.generation,
            };
            SETTING_UP.scope(mark, kernel.restarted()).await;
        }
        guard.armed = false;
        live.set_up();
    }

    fn cut(&self, generation: u64) {
        let mut slot = self.slot();
        if let Some(live) = slot.live.take_if(|live| live.generation == generation) {
            live.cut();
        }
    }

    fn release(&self, generation: u64, handle: ResourceAny) {
        let slot = self.slot();
        let Some(live) = slot
            .live
            .as_ref()
            .filter(|live| live.generation == generation)
        else {
            return;
        };
        let work: Work = Box::new(move |accessor, _| {
            Box::pin(async move { handle.resource_drop_concurrent(accessor).await })
        });
        let _ = live.work.send(work);
    }

    async fn ask<R, J>(
        &self,
        expected: Option<u64>,
        limit: Option<Duration>,
        hold: bool,
        job: J,
    ) -> Result<R, Stop>
    where
        R: Send + 'static,
        J: for<'a> FnOnce(
                &'a Accessor<State>,
                &'a Exports,
                Arc<dyn Scope>,
                u64,
            ) -> BoxFuture<'a, wasmtime::Result<R>>
            + Send
            + 'static,
    {
        let caller = caller_of(&*self.host);
        let scope: Arc<dyn Scope> = match self.mark() {
            Some(mark) => Arc::new(Marked {
                scope: caller,
                mark,
            }),
            None => caller,
        };
        let deadline = limit.map(|limit| tokio::time::Instant::now() + limit);
        let ready = async {
            let held = if hold { self.hold().await } else { None };
            self.live(expected).await.map(|live| (live, held))
        };
        let (live, _held) = match deadline {
            Some(at) => tokio::time::timeout_at(at, ready)
                .await
                .unwrap_or_else(|_| Err(Stop::Ended(CUT.to_owned())))?,
            None => ready.await?,
        };
        let generation = live.generation;
        let (reply, answer) = oneshot::channel();
        let work: Work = Box::new(move |accessor, exports| {
            Box::pin(async move {
                let value = job(accessor, exports, scope, generation).await?;
                let _ = reply.send(value);
                Ok(())
            })
        });
        if live.work.send(work).is_err() {
            return Err(Stop::Ended(live.ended.reason().await));
        }
        let mut guard = Guard {
            shared: self,
            generation,
            armed: true,
        };
        let answered = match deadline {
            Some(at) => match tokio::time::timeout_at(at, answer).await {
                Ok(answered) => answered,
                Err(_) => {
                    drop(guard);
                    return Err(Stop::Ended(live.ended.reason().await));
                }
            },
            None => answer.await,
        };
        guard.armed = false;
        match answered {
            Ok(value) => Ok(value),
            Err(_) => Err(Stop::Ended(live.ended.reason().await)),
        }
    }

    fn stopped(&self, stop: Stop) -> String {
        match stop {
            Stop::Ended(reason) => format!("the {} plugin stopped: {reason}", self.id),
            Stop::Restarted => format!("the {} plugin restarted before this call ran", self.id),
        }
    }
}

impl WasmPlugin {
    pub(crate) fn new(
        id: String,
        pre: Pre,
        host: Arc<dyn Host>,
        env: Arc<[(String, String)]>,
    ) -> Self {
        Self(Arc::new(Shared {
            id,
            pre,
            host,
            env,
            slot: Mutex::new(Slot::default()),
            render: Arc::default(),
        }))
    }

    async fn per_call<R, J>(&self, default: R, job: J) -> R
    where
        R: Send + 'static,
        J: for<'a> FnOnce(
                &'a Accessor<State>,
                &'a Exports,
                Arc<dyn Scope>,
                u64,
            ) -> BoxFuture<'a, wasmtime::Result<R>>
            + Send
            + 'static,
    {
        self.0
            .ask(None, Some(UNBOUNDED), true, job)
            .await
            .unwrap_or(default)
    }

    async fn bounded<R, J>(&self, job: J) -> R
    where
        R: Send + 'static,
        J: for<'a> FnOnce(
                &'a Accessor<State>,
                &'a Exports,
                Arc<dyn Scope>,
                u64,
            ) -> BoxFuture<'a, wasmtime::Result<R>>
            + Send
            + 'static,
    {
        self.unwound(self.0.ask(None, None, true, job).await)
    }

    fn unwound<R>(&self, asked: Result<R, Stop>) -> R {
        asked.unwrap_or_else(|stop| std::panic::resume_unwind(Box::new(self.0.stopped(stop))))
    }
}

impl Plugin for WasmPlugin {
    fn entries(&self) -> BoxFuture<'_, Vec<Entry>> {
        Box::pin(async move {
            let asked = self.0.ask(
                None,
                Some(UNBOUNDED),
                true,
                move |accessor, exports, scope, _| {
                    Box::pin(async move {
                        let Some(source) = &exports.index_source else {
                            return Ok(Vec::new());
                        };
                        let (entries,) = call(accessor, scope, source.func_entries(), ()).await?;
                        Ok(entries.mirror())
                    })
                },
            );
            self.unwound(asked.await)
        })
    }

    fn open_entry<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<String, String>> {
        let name = name.to_owned();
        Box::pin(self.bounded(move |accessor, exports, scope, _| {
            Box::pin(async move {
                let Some(source) = &exports.index_source else {
                    return Ok(wisp_plugin_api::index::Index::open(&(), &name).await);
                };
                let (text,) = call(accessor, scope, source.func_open_entry(), (name,)).await?;
                Ok(text)
            })
        }))
    }

    fn describe<'a>(&'a self, ctx: &'a BuildContext) -> BoxFuture<'a, Vec<ToolDef>> {
        self.0.slot().build.get_or_insert_with(|| ctx.clone());
        let build = ctx.clone();
        Box::pin(
            self.per_call(Vec::new(), move |accessor, exports, scope, _| {
                Box::pin(async move {
                    with_tools!(&exports.tools, |tools| {
                        let describe = tools.func_describe();
                        let (defs,) = call(accessor, scope, describe, (build.mirror(),)).await?;
                        Ok(defs.mirror())
                    })
                })
            }),
        )
    }

    fn doctor<'a>(&'a self, ctx: &'a BuildContext) -> BoxFuture<'a, Vec<DoctorRow>> {
        let build = ctx.clone();
        Box::pin(
            self.per_call(Vec::new(), move |accessor, exports, scope, _| {
                Box::pin(async move {
                    with_tools!(&exports.tools, |tools| {
                        let doctor = tools.func_doctor();
                        let (rows,) = call(accessor, scope, doctor, (build.mirror(),)).await?;
                        Ok(rows.mirror())
                    })
                })
            }),
        )
    }

    fn prepare<'a>(
        &'a self,
        tool: &'a str,
        input: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Prepared>, String>> {
        let args = (tool.to_owned(), input.to_owned());
        Box::pin(async move {
            let asked = self
                .0
                .ask(
                    None,
                    Some(UNBOUNDED),
                    true,
                    move |accessor, exports, scope, generation| {
                        Box::pin(async move {
                            with_tools!(&exports.tools, |tools| {
                                let (prepared,) =
                                    call(accessor, scope.clone(), tools.func_prepare(), args)
                                        .await?;
                                let handle = match prepared {
                                    Ok(handle) => handle,
                                    Err(message) => return Ok(Err(message)),
                                };
                                let display = tools.prepared().func_display();
                                let (shown,) = call(accessor, scope, display, (handle,)).await?;
                                Ok(Ok((handle, shown.mirror(), generation)))
                            })
                        })
                    },
                )
                .await;
            match asked {
                Ok(Ok((handle, display, generation))) => Ok(Box::new(WasmPrepared {
                    handle: Some(handle),
                    display,
                    generation,
                    shared: self.0.clone(),
                })
                    as Box<dyn Prepared>),
                Ok(Err(message)) => Err(message),
                Err(stop) => Err(self.0.stopped(stop)),
            }
        })
    }

    fn rejected_display<'a>(
        &'a self,
        tool: &'a str,
        input: &'a str,
    ) -> BoxFuture<'a, Option<Display>> {
        let args = (tool.to_owned(), input.to_owned());
        Box::pin(self.per_call(None, move |accessor, exports, scope, _| {
            Box::pin(async move {
                with_tools!(&exports.tools, |tools| {
                    let func = tools.func_rejected_display();
                    let (shown,) = call(accessor, scope, func, args).await?;
                    Ok(shown.mirror())
                })
            })
        }))
    }

    fn time_limit_ms<'a>(&'a self, tool: &'a str, input: &'a str) -> BoxFuture<'a, Option<u64>> {
        let args = (tool.to_owned(), input.to_owned());
        Box::pin(self.per_call(None, move |accessor, exports, scope, _| {
            Box::pin(async move {
                with_tools!(&exports.tools, |tools| {
                    let func = tools.func_time_limit_ms();
                    let (limit,) = call(accessor, scope, func, args).await?;
                    Ok(limit)
                })
            })
        }))
    }

    fn movable<'a>(&'a self, tool: &'a str, input: &'a str) -> BoxFuture<'a, bool> {
        let args = (tool.to_owned(), input.to_owned());
        Box::pin(self.per_call(false, move |accessor, exports, scope, _| {
            Box::pin(async move {
                with_tools!(&exports.tools, |tools| {
                    let func = tools.func_movable();
                    let (movable,) = call(accessor, scope, func, args).await?;
                    Ok(movable)
                })
            })
        }))
    }

    fn interrupted_result<'a>(
        &'a self,
        tool: &'a str,
        input: &'a str,
    ) -> BoxFuture<'a, Option<String>> {
        let args = (tool.to_owned(), input.to_owned());
        Box::pin(self.per_call(None, move |accessor, exports, scope, _| {
            Box::pin(async move {
                with_tools!(&exports.tools, |tools| {
                    let func = tools.func_interrupted_result();
                    let (result,) = call(accessor, scope, func, args).await?;
                    Ok(result)
                })
            })
        }))
    }

    fn run<'a>(
        &'a self,
        call_: Box<dyn Prepared>,
        ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, ToolRun> {
        Box::pin(self.run_unboxed(call_, ctx))
    }

    fn run_command<'a>(&'a self, name: &'a str, args: &'a str) -> BoxFuture<'a, CommandReply> {
        let args = (name.to_owned(), args.to_owned());
        Box::pin(self.bounded(move |accessor, exports, scope, _| {
            Box::pin(async move {
                with_commands!(&exports.commands, |commands| {
                    let func = commands.func_run();
                    let (reply,) = call(accessor, scope, func, args).await?;
                    Ok(reply.mirror())
                })
            })
        }))
    }

    fn on_hook<'a>(&'a self, hook: &'a str, event: Event) -> BoxFuture<'a, Reply> {
        let args = (hook.to_owned(), event.mirror());
        Box::pin(self.bounded(move |accessor, exports, scope, _| {
            Box::pin(async move {
                let func = exports.hooks.func_on_hook();
                let (reply,) = call(accessor, scope, func, args).await?;
                Ok(reply.mirror())
            })
        }))
    }

    fn render_slot<'a>(
        &'a self,
        name: &'a str,
        view: SlotView,
    ) -> BoxFuture<'a, Result<Content, String>> {
        let args = (name.to_owned(), view.mirror());
        Box::pin(async move {
            let asked = self
                .0
                .ask(None, None, false, move |accessor, exports, scope, _| {
                    Box::pin(async move {
                        with_slots!(&exports.slots, |slots| {
                            let func = slots.func_render_slot();
                            let (rendered,) = call(accessor, scope, func, args).await?;
                            Ok(rendered.mirror())
                        })
                    })
                });
            self.unwound(asked.await)
        })
    }

    fn lock_for_render(&self) -> Option<Box<dyn Send>> {
        let held = self.0.render.clone().try_write_owned().ok()?;
        Some(Box::new(held))
    }

    fn restarts_when_cut(&self) -> bool {
        true
    }

    fn render_section<'a>(
        &'a self,
        name: &'a str,
        at: At,
        previous: Prior,
        target: Option<&'a str>,
    ) -> BoxFuture<'a, Rendered> {
        let args = (
            name.to_owned(),
            at.mirror(),
            previous.mirror(),
            target.map(str::to_owned),
        );
        Box::pin(self.bounded(move |accessor, exports, scope, _| {
            Box::pin(async move {
                let func = exports.prompt_sections.func_render_section();
                let (rendered,) = call(accessor, scope, func, args).await?;
                Ok(rendered.mirror())
            })
        }))
    }

    fn on_agent_hook<'a>(&'a self, hook: &'a str, event: TreeEvent) -> BoxFuture<'a, ()> {
        let args = (hook.to_owned(), event.mirror());
        Box::pin(self.bounded(move |accessor, exports, scope, _| {
            Box::pin(async move {
                let func = exports.agent_hooks.func_on_agent_hook();
                call(accessor, scope, func, args).await?;
                Ok(())
            })
        }))
    }

    fn instruction<'a>(&'a self, info: StrategyInfo) -> BoxFuture<'a, Option<String>> {
        let args = (info.mirror(),);
        Box::pin(self.bounded(move |accessor, exports, scope, _| {
            Box::pin(async move {
                let func = exports.strategy.func_instruction();
                let (text,) = call(accessor, scope, func, args).await?;
                Ok(text)
            })
        }))
    }

    fn accept<'a>(
        &'a self,
        info: StrategyInfo,
        summary: String,
    ) -> BoxFuture<'a, Result<(), String>> {
        let args = (info.mirror(), summary);
        Box::pin(self.bounded(move |accessor, exports, scope, _| {
            Box::pin(async move {
                let func = exports.strategy.func_accept();
                let (verdict,) = call(accessor, scope, func, args).await?;
                Ok(verdict)
            })
        }))
    }

    fn head<'a>(
        &'a self,
        info: StrategyInfo,
        view: &'a dyn View,
        summary: Option<String>,
    ) -> BoxFuture<'a, Vec<String>> {
        let info = info.mirror();
        let viewed = Viewed::of(view);
        Box::pin(self.bounded(move |accessor, exports, scope, _| {
            Box::pin(async move {
                let view = accessor.with(|mut access| access.get().table.push(viewed))?;
                let rep = view.rep();
                let func = exports.strategy.func_head();
                let (texts,) = call(accessor, scope, func, (info, view, summary)).await?;
                accessor.with(|mut access| {
                    access.get().table.delete(Resource::<Viewed>::new_own(rep))
                })?;
                Ok(texts)
            })
        }))
    }
}

impl WasmPlugin {
    async fn run_unboxed(&self, prepared: Box<dyn Prepared>, ctx: &dyn CallContext) -> ToolRun {
        let Ok(mut prepared) = prepared.into_any().downcast::<WasmPrepared>() else {
            return ToolRun::failed_to_start(format!(
                "the {} plugin was given another plugin's call",
                self.0.id
            ));
        };
        let Some(handle) = prepared.handle.take() else {
            return ToolRun::failed_to_start(self.0.stopped(Stop::Restarted));
        };
        let (link, mut requests) = CallLink::new();
        let asked = self.0.ask(
            Some(prepared.generation),
            None,
            true,
            move |accessor, exports, scope, _| {
                Box::pin(async move {
                    let ctx = accessor.with(|mut access| access.get().table.push(link))?;
                    let rep = ctx.rep();
                    let ran = with_tools!(&exports.tools, |tools| {
                        let run = tools.func_run();
                        let (ran,) = call(accessor, scope, run, (handle, ctx)).await?;
                        ran.mirror()
                    });
                    accessor.with(|mut access| {
                        access
                            .get()
                            .table
                            .delete(Resource::<CallLink>::new_own(rep))
                    })?;
                    Ok(ran)
                })
            },
        );
        let mut asked = std::pin::pin!(asked);
        let mut serving = futures::stream::FuturesUnordered::new();
        let answered = std::future::poll_fn(|cx| {
            while let std::task::Poll::Ready(Some(request)) = requests.poll_recv(cx) {
                serving.push(request(ctx));
            }
            while let std::task::Poll::Ready(Some(())) =
                futures::StreamExt::poll_next_unpin(&mut serving, cx)
            {}
            asked.as_mut().poll(cx)
        })
        .await;
        answered.unwrap_or_else(|stop| {
            ToolRun::new(Outcome::Failed(Failure {
                message: self.0.stopped(stop),
                before_start: false,
            }))
        })
    }
}
