use std::collections::BTreeMap;

use futures::FutureExt;
use futures::future::BoxFuture;

use super::*;
use crate::plugin::testing::{self, Call, TestTool};
use crate::tool::{ToolEntry, ToolPurpose, Toolbox};

const NAME: &str = "show_diff";

struct Declared {
    name: String,
    purpose: ToolPurpose,
}

impl Declared {
    fn tool(name: &str, purpose: ToolPurpose) -> ToolEntry {
        testing::entry(Arc::new(Self {
            name: name.into(),
            purpose,
        }))
    }
}

impl TestTool for Declared {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn schema(&self) -> Value {
        json!({"type": "object", "properties": {"ok": {"type": "boolean"}}})
    }

    fn meta(&self) -> wisp_plugin_api::tools::ToolMeta {
        wisp_plugin_api::tools::ToolMeta {
            parallel_safe: true,
            saves_output: false,
            needs_workspace: false,
            purpose: match self.purpose {
                ToolPurpose::Show => wisp_plugin_api::tools::Purpose::Show,
                ToolPurpose::Run => wisp_plugin_api::tools::Purpose::Run,
            },
            ..testing::meta(5_000, Some(5_000))
        }
    }

    fn prepare(&self, input: &str) -> Result<Box<dyn wisp_plugin_api::tools::Prepared>, String> {
        let value: Value = serde_json::from_str(input).map_err(|e| e.to_string())?;
        if value.get("ok").is_none() {
            return Err("`ok` is required".to_owned());
        }
        Ok(Call::boxed(testing::fallback(&self.name, input), ()))
    }

    fn run<'a>(
        &'a self,
        _call: Box<dyn wisp_plugin_api::tools::Prepared>,
        _ctx: &'a dyn wisp_plugin_api::host::CallContext,
    ) -> BoxFuture<'a, wisp_plugin_api::types::ToolRun> {
        async { wisp_plugin_api::types::ToolRun::completed() }.boxed()
    }
}

fn declaring(provider: Arc<Fake>, env: &Env, store: &MemoryStore) -> Agent {
    let context = log_context();
    let tools = Toolbox::new([
        crate::tool::testkit::show_diff(),
        Declared::tool("shower", ToolPurpose::Show),
        Declared::tool("runner", ToolPurpose::Run),
    ]);
    Agent::new(provider, default_session())
        .with_clock(TickingClock::new())
        .with_tools(tools, env.workspace())
        .with_platform("Linux")
        .with_resources(None)
        .with_log(SessionLog::new(Box::new(store.clone()), "s1"), context)
}

#[tokio::test]
async fn every_call_carries_its_tools_declared_purpose() {
    let env = Env::new("purpose");
    let store = MemoryStore::default();
    let diff = json!({"files": [{"path": "a.rs", "before": "a\n", "after": "b\n"}]});
    let scripted = scripted(vec![
        uses(&[
            call("diff", NAME, diff),
            call("bad-diff", NAME, json!({"files": [{"path": "a.rs"}]})),
            call("show", "shower", json!({"ok": true})),
            call("bad-show", "shower", json!({})),
            call("run", "runner", json!({"ok": true})),
            call("bad-run", "runner", json!({})),
            call("nowhere", "nowhere", json!({"ok": true})),
        ]),
        done("shown"),
    ]);
    let mut agent = declaring(scripted, &env, &store);
    let (outcome, events) = turn(&mut agent, "show me").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{events:#?}"
    );

    let displays: BTreeMap<String, wisp_protocol::ToolDisplay> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCallRequested {
                call_id, display, ..
            } => Some((call_id.clone(), display.clone())),
            _ => None,
        })
        .collect();
    let purposes: BTreeMap<&str, ToolPurpose> = displays
        .iter()
        .map(|(id, display)| (id.as_str(), display.purpose))
        .collect();
    assert_eq!(
        purposes,
        BTreeMap::from([
            ("bad-diff", ToolPurpose::Show),
            ("bad-run", ToolPurpose::Run),
            ("bad-show", ToolPurpose::Show),
            ("diff", ToolPurpose::Show),
            ("nowhere", ToolPurpose::Run),
            ("run", ToolPurpose::Run),
            ("show", ToolPurpose::Show),
        ])
    );
    assert_eq!(
        displays["bad-show"].action,
        ToolAction::Other {
            name: "shower".into()
        }
    );

    let logged: BTreeMap<String, Option<Value>> = records(&store)
        .into_iter()
        .filter(|r| matches!(r.event, LogEvent::ToolCallRequested { .. }))
        .map(|r| {
            let value = serde_json::to_value(&r).unwrap();
            (
                value["call_id"].as_str().unwrap().to_owned(),
                value["display"].get("purpose").cloned(),
            )
        })
        .collect();
    let show = Some(json!("show"));
    assert_eq!(
        logged,
        BTreeMap::from([
            ("bad-diff".to_owned(), show.clone()),
            ("bad-run".to_owned(), None),
            ("bad-show".to_owned(), show.clone()),
            ("diff".to_owned(), show.clone()),
            ("nowhere".to_owned(), None),
            ("run".to_owned(), None),
            ("show".to_owned(), show),
        ])
    );
}
