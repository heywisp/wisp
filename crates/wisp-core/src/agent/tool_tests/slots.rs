use std::sync::{Mutex as StdMutex, OnceLock};

use futures::FutureExt;
use futures::future::BoxFuture;
use tokio::sync::Notify;
use wisp_plugin_api::host::{CallContext, Host, TokenCount as Counted};
use wisp_plugin_api::tools::{Prepared, ToolMeta};
use wisp_plugin_api::types::ToolRun;
use wisp_plugin_testkit::hooks::{self as kit, Calls, Render};
use wisp_protocol::RequestId;
use wisp_protocol::settings::{SettingChange, SettingValue};

use super::*;
use crate::agent::tests::answered;
use crate::plugin::testing::{self, TestTool};
use crate::plugin::{KernelHost, Make, PluginConfig, Registry};
use crate::test_guard::until_event;

type Log = Arc<StdMutex<Vec<&'static str>>>;

struct Gated {
    host: OnceLock<Arc<dyn Host>>,
    started: Notify,
    gate: Notify,
    counts: StdMutex<Vec<Counted>>,
    log: Log,
}

impl TestTool for Gated {
    fn name(&self) -> String {
        "gated".to_owned()
    }

    fn meta(&self) -> ToolMeta {
        testing::meta(60_000, Some(60_000))
    }

    fn attach(&self, host: Arc<dyn Host>) {
        let _ = self.host.set(host);
    }

    fn prepare(&self, input: &str) -> Result<Box<dyn Prepared>, String> {
        Ok(testing::Call::boxed(testing::fallback("gated", input), ()))
    }

    fn run<'a>(
        &'a self,
        _call: Box<dyn Prepared>,
        _ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, ToolRun> {
        async move {
            let host = self.host.get().unwrap().clone();
            let first = host
                .count_tokens("image/png".into(), vec![1, 2, 3], None)
                .await;
            self.log.lock().unwrap().push("start");
            self.started.notify_one();
            self.gate.notified().await;
            let second = host
                .count_tokens("image/png".into(), vec![1, 2, 3], None)
                .await;
            self.log.lock().unwrap().push("end");
            self.counts.lock().unwrap().extend([first, second]);
            ToolRun::completed()
        }
        .boxed()
    }
}

fn logging(log: Log) -> Render {
    Arc::new(move |_, _, _| {
        let mut log = log.lock().unwrap();
        log.push("render");
        let words = format!("drawn {}", log.len());
        Box::pin(async move { Ok(kit::line(&words)) })
    })
}

async fn gated_agent(env: &Env, log: &Log) -> (Arc<Gated>, Agent) {
    let tool = Arc::new(Gated {
        host: OnceLock::new(),
        started: Notify::new(),
        gate: Notify::new(),
        counts: StdMutex::new(Vec::new()),
        log: log.clone(),
    });
    let section: Make = kit::sections(kit::none(), logging(log.clone()), Calls::default());
    let body = kit::section("sec", "marks = [\"settings-changed\"]\n");
    let (registry, problems) = Registry::from_sources(
        vec![
            testing::source(tool.clone()),
            (kit::slotted("aa", &body), section),
        ],
        &PluginConfig::default(),
    );
    assert!(problems.is_empty(), "{problems:?}");
    let on = ["gated".to_owned()].into_iter().collect();
    let started = registry.start(&on, Arc::new(KernelHost::new())).await;
    assert!(started.problems.is_empty(), "{:?}", started.problems);
    let provider = scripted(vec![uses(&[call("g", "gated", json!({}))]), done("ok")]);
    let agent = Agent::new(provider.view(|k| k.count = Some(9)), default_session())
        .with_clock(TickingClock::new())
        .with_tools(started.toolbox, env.workspace())
        .with_platform("Linux")
        .with_resources(None)
        .with_hooks(started.hooks, started.host);
    (tool, agent)
}

#[tokio::test]
async fn a_count_tokens_change_during_a_call_waits_for_its_end_to_reach_the_snapshot_and_the_sidebar()
 {
    let env = Env::new("slots-count-tokens");
    let log: Log = Arc::default();
    let (tool, agent) = gated_agent(&env, &log).await;
    let (commands, mut events, _task) = crate::agent::spawn(agent);
    commands
        .send(ClientCommand::SlotsShown {
            sections: vec!["aa/sec".to_owned()],
            pages: Vec::new(),
        })
        .await
        .unwrap();
    until_event(&mut events, &mut Vec::new(), "the event", |e| {
        matches!(e, AgentEvent::SlotContent { .. })
    })
    .await;
    commands
        .send(ClientCommand::send("go", "go"))
        .await
        .unwrap();
    within("the tool's start", tool.started.notified()).await;
    let change = SettingChange::named("context.count_tokens", SettingValue::Bool(false)).unwrap();
    commands
        .send(ClientCommand::ChangeSettings {
            request: RequestId(1),
            changes: vec![change],
            save: false,
        })
        .await
        .unwrap();
    until_event(&mut events, &mut Vec::new(), "the event", |e| {
        matches!(e, AgentEvent::SettingsChanged { .. })
    })
    .await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    tool.gate.notify_one();
    until_turn_end(&mut events, &mut Vec::new()).await;
    until_event(&mut events, &mut Vec::new(), "the event", |e| {
        matches!(e, AgentEvent::SlotContent { .. })
    })
    .await;
    assert_eq!(
        *tool.counts.lock().unwrap(),
        [Counted::Counted(9), Counted::Counted(9)]
    );
    assert_eq!(*log.lock().unwrap(), ["render", "start", "end", "render"]);
    drop(commands);
}

#[tokio::test]
async fn slots_shown_during_a_tool_call_renders_while_the_call_still_runs() {
    let env = Env::new("slots-shown-mid-call");
    let log: Log = Arc::default();
    let (tool, agent) = gated_agent(&env, &log).await;
    let (commands, mut events, _task) = crate::agent::spawn(agent);
    commands
        .send(ClientCommand::send("go", "go"))
        .await
        .unwrap();
    within("the tool's start", tool.started.notified()).await;
    commands
        .send(ClientCommand::SlotsShown {
            sections: vec!["aa/sec".to_owned()],
            pages: Vec::new(),
        })
        .await
        .unwrap();
    until_event(&mut events, &mut Vec::new(), "the slot content", |e| {
        assert!(!matches!(e, AgentEvent::TurnEnded { .. }), "{e:?}");
        matches!(e, AgentEvent::SlotContent { .. })
    })
    .await;
    assert_eq!(*log.lock().unwrap(), ["start", "render"]);
    tool.gate.notify_one();
    until_turn_end(&mut events, &mut Vec::new()).await;
    assert_eq!(*log.lock().unwrap(), ["start", "render", "end"]);
}

fn one_section_catalog() -> wisp_protocol::Catalog {
    wisp_protocol::Catalog {
        plugins: vec![wisp_protocol::PluginEntry {
            prompt_sections: Vec::new(),
            id: "aa".to_owned(),
            version: "0.1.0".to_owned(),
            summary: String::new(),
            source: wisp_protocol::plugin::PluginSource::Bundled,
            installed: true,
            enabled: true,
            tools: vec![],
            commands: vec![],
            hooks: vec![],
            slots: vec![wisp_protocol::slot::SlotEntry {
                name: "aa/sec".to_owned(),
                slot: wisp_protocol::slot::SlotPlace::Sidebar,
                title: "sec".to_owned(),
                default_on: true,
                order: 0,
                disabled: None,
            }],
            settings: vec![],
            problems: vec![],
        }],
    }
}

#[tokio::test]
async fn a_section_switch_names_a_section_of_the_catalog_and_anything_else_is_refused() {
    let env = Env::new("slots-section-switch");
    let log: Log = Arc::default();
    let (_tool, agent) = gated_agent(&env, &log).await;
    let catalog = one_section_catalog();
    let (commands, mut events, _task) = crate::agent::spawn(agent.with_catalog(catalog));
    let spec = wisp_protocol::settings::find(wisp_protocol::settings::PLUGIN_SLOT).unwrap();
    let switch = |request: u64, plugin: &str, section: &str| ClientCommand::ChangeSettings {
        request: RequestId(request),
        changes: vec![SettingChange {
            target: wisp_protocol::settings::SettingTarget::Path(
                wisp_protocol::settings::SitePath::new(
                    wisp_protocol::settings::Place::PluginSlot {
                        plugin: plugin.to_owned(),
                        section: section.to_owned(),
                    },
                    spec,
                ),
            ),
            value: Some(SettingValue::Bool(false)),
        }],
        save: false,
    };
    commands.send(switch(1, "aa", "sec")).await.unwrap();
    let applied = answered(&mut events, 1).await;
    assert!(
        matches!(applied, AgentEvent::SettingsChanged { .. }),
        "{applied:?}"
    );
    for (request, plugin, section) in [(2, "aa", "nope"), (3, "zz", "sec")] {
        commands
            .send(switch(request, plugin, section))
            .await
            .unwrap();
        let AgentEvent::Rejected { reason, .. } = answered(&mut events, request).await else {
            panic!("{plugin}/{section} was accepted");
        };
        assert_eq!(
            reason,
            format!(
                "plugins.{plugin}.slots.{section}: {plugin}/{section} is no sidebar section; sections: aa/sec"
            )
        );
    }
    drop(commands);
}

#[tokio::test]
async fn a_fold_list_keeps_names_that_are_no_section_with_a_warning_and_refuses_bad_grammar() {
    let env = Env::new("slots-fold-list");
    let log: Log = Arc::default();
    let (_tool, agent) = gated_agent(&env, &log).await;
    let (commands, mut events, _task) =
        crate::agent::spawn(agent.with_catalog(one_section_catalog()));
    let fold = |request: u64, names: &[&str]| ClientCommand::ChangeSettings {
        request: RequestId(request),
        changes: vec![
            SettingChange::named(
                "display.sidebar_folded",
                SettingValue::List(names.iter().map(|n| (*n).to_owned()).collect()),
            )
            .unwrap(),
        ],
        save: false,
    };
    commands
        .send(fold(1, &["aa/sec", "zz/gone"]))
        .await
        .unwrap();
    let AgentEvent::SettingsChanged { changes, .. } = answered(&mut events, 1).await else {
        panic!("a list naming a removed section was refused");
    };
    assert_eq!(
        changes[0].value,
        Some(SettingValue::List(vec![
            "aa/sec".to_owned(),
            "zz/gone".to_owned()
        ]))
    );
    assert_eq!(
        changes[0].warnings,
        ["no sidebar section named zz/gone; kept"]
    );
    commands.send(fold(2, &["aa/sec"])).await.unwrap();
    let AgentEvent::SettingsChanged { changes, .. } = answered(&mut events, 2).await else {
        panic!("a list of known sections was refused");
    };
    assert!(changes[0].warnings.is_empty(), "{changes:?}");
    for (request, name) in [(7, "display.sidebar_folded"), (8, "display.sidebar_pinned")] {
        commands
            .send(ClientCommand::ChangeSettings {
                request: RequestId(request),
                changes: vec![SettingChange::named(name, SettingValue::List(vec![])).unwrap()],
                save: false,
            })
            .await
            .unwrap();
        let AgentEvent::SettingsChanged { changes, .. } = answered(&mut events, request).await
        else {
            panic!("an empty {name} was refused");
        };
        assert_eq!(changes[0].value, Some(SettingValue::List(vec![])));
        assert!(changes[0].warnings.is_empty(), "{changes:?}");
    }
    commands
        .send(ClientCommand::ChangeSettings {
            request: RequestId(9),
            changes: vec![SettingChange::named("tools", SettingValue::List(vec![])).unwrap()],
            save: true,
        })
        .await
        .unwrap();
    let refused = answered(&mut events, 9).await;
    assert!(
        matches!(&refused, AgentEvent::Rejected { reason, .. } if reason.contains("tools must be none or a list of tool names")),
        "an empty tools list was not refused: {refused:?}"
    );
    for (request, bad) in [(3, "aa"), (4, "Aa/sec"), (5, "aa/s"), (6, "aa/sec/x")] {
        commands.send(fold(request, &[bad])).await.unwrap();
        let rejected = answered(&mut events, request).await;
        assert!(
            matches!(rejected, AgentEvent::Rejected { .. }),
            "{bad}: {rejected:?}"
        );
    }
    drop(commands);
}
