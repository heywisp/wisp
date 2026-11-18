use std::sync::OnceLock;

use wisp_plugin_api::host::{CallContext, Host};
use wisp_plugin_api::tools::Prepared as PluginPrepared;

use super::*;
use crate::plugin::testing::{self, Call, TestTool};
use crate::settings::host::StartLayers;

#[derive(Default)]
struct Reader {
    host: OnceLock<Arc<dyn Host>>,
}

impl TestTool for Reader {
    fn name(&self) -> String {
        "reader".into()
    }

    fn settings(&self) -> Vec<String> {
        vec!["time".into()]
    }

    fn attach(&self, host: Arc<dyn Host>) {
        let _ = self.host.set(host);
    }

    fn prepare(&self, input: &str) -> Result<Box<dyn PluginPrepared>, String> {
        Ok(Call::boxed(testing::fallback("reader", input), ()))
    }

    fn run<'a>(
        &'a self,
        _call: Box<dyn PluginPrepared>,
        _ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, wisp_plugin_api::types::ToolRun> {
        async move {
            let host = self.host.get().unwrap();
            let text = format!(
                "time={:?} effort={:?} unowned={:?}",
                host.get("time"),
                host.get("effort"),
                host.get("context.context_note"),
            );
            let text = format!(
                "{text} minutes={:?}",
                host.get("plugins.test-reader.minutes")
            );
            wisp_plugin_api::types::ToolRun {
                result: Some(text),
                ..wisp_plugin_api::types::ToolRun::completed()
            }
        }
        .boxed()
    }
}

fn tool_results(request: &Sent) -> String {
    serde_json::to_string(&request.messages).unwrap()
}

#[tokio::test]
async fn a_plugin_reads_a_set_value_and_a_mid_session_change_of_the_settings_it_owns() {
    let provider = scripted(vec![
        uses(&[call("a", "reader", json!({}))]),
        done("one"),
        uses(&[call("b", "reader", json!({}))]),
        done("two"),
    ]);
    let scratch = tempfile::tempdir().unwrap();
    let mut live = Live::with(provider.clone(), None, |agent| {
        let root = scratch.path().to_path_buf();
        let workspace = Workspace {
            root: root.clone(),
            home: None,
            temp: root,
        };
        agent
            .with_tools(
                testing::toolbox(vec![Arc::new(Reader::default())]),
                workspace,
            )
            .with_start_layers(StartLayers {
                flags: vec![flag("time", "utc")],
                ..StartLayers::default()
            })
    });
    live.send(send("tui-1", "first")).await;
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let change = ClientCommand::ChangeSettings {
        request: RequestId(1),
        changes: wisp_protocol::settings::sorted_changes(vec![SettingChange {
            target: SettingTarget::Name(SettingName::of(
                wisp_protocol::settings::find("time").unwrap(),
            )),
            value: Some(SettingValue::Text("off".into())),
        }]),
        save: false,
    };
    live.send(change).await;
    live.until(|e| {
        answers(std::slice::from_ref(e))
            .iter()
            .any(|(r, _)| *r == 1)
    })
    .await;
    live.send(send("tui-2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let requests = provider.sent();
    assert_eq!(requests.len(), 4);
    let first = tool_results(&requests[1]);
    assert!(
        first.contains(r#"time=Some(\"local\") effort=None unowned=None"#),
        "{first}"
    );
    assert!(first.contains("UTC-04:00"), "{first}");
    let second = tool_results(&requests[3]);
    assert!(
        second.contains(r#"time=Some(\"off\") effort=None unowned=None"#),
        "{second}"
    );
    live.end().await;
}

#[tokio::test]
async fn a_plugin_reads_wisps_own_value_then_a_change_of_a_row_it_defines() {
    let provider = scripted(vec![
        uses(&[call("a", "reader", json!({}))]),
        done("one"),
        uses(&[call("b", "reader", json!({}))]),
        done("two"),
    ]);
    let scratch = tempfile::tempdir().unwrap();
    let mut live = Live::with(provider.clone(), None, |agent| {
        let (body, make) = testing::source(Arc::new(Reader::default()));
        let body = wisp_plugin_testkit::with_settings(&body.replace("settings = [\"time\"]\n", ""));
        let (registry, problems) =
            crate::plugin::Registry::from_sources(vec![(body, make)], &Default::default());
        assert!(problems.is_empty(), "{problems:?}");
        let started = registry
            .start(
                &["reader".to_owned()].into_iter().collect(),
                Arc::new(crate::plugin::KernelHost::new()),
            )
            .now_or_never()
            .unwrap();
        assert!(started.problems.is_empty(), "{:?}", started.problems);
        agent
            .with_tools(
                started.toolbox,
                Workspace {
                    root: scratch.path().to_owned(),
                    home: None,
                    temp: scratch.path().to_owned(),
                },
            )
            .with_hooks(started.hooks, started.host)
    });
    live.send(send("tui-1", "first")).await;
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let spec = wisp_protocol::settings::find("plugins.test-reader.minutes").unwrap();
    live.send(ClientCommand::ChangeSettings {
        request: RequestId(1),
        changes: wisp_protocol::settings::sorted_changes(vec![SettingChange {
            target: SettingTarget::Name(SettingName::of(spec)),
            value: Some(SettingValue::Int(25)),
        }]),
        save: false,
    })
    .await;
    let answer = live
        .until(|e| {
            answers(std::slice::from_ref(e))
                .iter()
                .any(|(r, _)| *r == 1)
        })
        .await;
    assert!(!format!("{answer:?}").contains("SettingsRejected"));
    live.send(send("tui-2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let requests = provider.sent();
    assert_eq!(requests.len(), 4);
    assert!(tool_results(&requests[1]).contains(r#"minutes=Some(\"10\")"#));
    assert!(tool_results(&requests[3]).contains(r#"minutes=Some(\"25\")"#));
    live.end().await;
}
