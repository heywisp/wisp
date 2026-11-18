use wisp_plugin_api::hooks::{Event, ToolsChange};
use wisp_plugin_testkit::hooks as testkit;

use super::native::{HAIKU, native_rig};
use super::*;
use crate::plugin::{KernelHost, Make, PluginConfig, Registry};

async fn watched(rig: &mut Rig) -> testkit::Calls {
    let calls = testkit::Calls::default();
    let make: Make = testkit::instantiate(testkit::none(), calls.clone());
    let sources = vec![(
        testkit::manifest(
            "watcher",
            "\n[[hooks]]\nname = \"seen\"\npoint = \"tools-changed\"\n",
        ),
        make,
    )];
    let (registry, problems) = Registry::from_sources(sources, &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let started = registry
        .start(&BTreeSet::new(), Arc::new(KernelHost::new()))
        .await;
    let placeholder = Agent::new(rig.provider.clone(), default_session());
    let agent = std::mem::replace(&mut rig.agent, placeholder);
    rig.agent = agent.with_hooks(started.hooks, started.host);
    calls
}

fn seen(calls: &testkit::Calls) -> Vec<Event> {
    calls.of("seen").into_iter().map(|c| c.event).collect()
}

fn rebuild(added: &[&str], removed: &[&str]) -> Event {
    let names = |n: &[&str]| n.iter().map(|s| (*s).to_owned()).collect();
    Event::ToolsChanged(ToolsChange {
        added: names(added),
        removed: names(removed),
        via: "rebuild".into(),
    })
}

#[tokio::test]
async fn a_search_refusal_fires_tools_changed_once_with_the_removal() {
    let mut rig = native_rig(
        vec![
            search_off(),
            done(&[B::Text("ok")]),
            done(&[B::Text("again")]),
        ],
        &[DEFAULT_MODEL],
    );
    let calls = watched(&mut rig).await;
    rig.turn("go").await;
    rig.turn("next").await;
    assert_eq!(seen(&calls), [rebuild(&[], &["web_search"])]);
}

#[tokio::test]
async fn a_form_changing_switch_fires_tools_changed_each_way() {
    let mut rig = native_rig(
        vec![
            done(&[B::Text("a")]),
            done(&[B::Text("b")]),
            done(&[B::Text("c")]),
            done(&[B::Text("d")]),
        ],
        &[DEFAULT_MODEL],
    );
    let calls = watched(&mut rig).await;
    let (ev_tx, _ev_rx) = mpsc::channel(4096);
    rig.turn("one").await;
    rig.agent
        .change_settings(None, Some(HAIKU.into()), None, &ev_tx)
        .await;
    rig.turn("two").await;
    rig.agent
        .change_settings(None, Some(DEFAULT_MODEL.into()), None, &ev_tx)
        .await;
    rig.turn("three").await;
    rig.turn("four").await;
    assert_eq!(
        seen(&calls),
        [rebuild(&[], &["web_search"]), rebuild(&["web_search"], &[])]
    );
}
