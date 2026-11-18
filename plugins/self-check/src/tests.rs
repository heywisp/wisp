use futures::executor::block_on;
use wisp_plugin_api::hooks::{BeforeEnd, ResponseInfo};
use wisp_plugin_api::manifest::{Manifest, Origin};
use wisp_plugin_api::tree::Identity;
use wisp_plugin_testkit::host::{FakeHost, identity};

use super::*;

fn plugin(identity: Option<Identity>) -> Arc<dyn Plugin> {
    instantiate(Arc::new(FakeHost {
        identity,
        ..FakeHost::default()
    }))
}

fn respond(plugin: &Arc<dyn Plugin>, turn: u64, stop_reason: &str) -> Reply {
    block_on(plugin.on_hook(
        WORKED,
        Event::AfterResponse(ResponseInfo {
            turn,
            step: 0,
            stop_reason: stop_reason.to_owned(),
        }),
    ))
}

fn end(plugin: &Arc<dyn Plugin>, turn: u64) -> Reply {
    block_on(plugin.on_hook(
        REVIEW,
        Event::BeforeEnd(BeforeEnd {
            turn,
            step: 1,
            reply: "Done.".to_owned(),
            continues: 0,
        }),
    ))
}

#[test]
fn the_root_after_a_tool_use_is_asked_once_per_turn() {
    let root = plugin(Some(identity("/")));
    assert_eq!(respond(&root, 3, "tool_use"), Reply::None);
    assert_eq!(respond(&root, 3, "end_turn"), Reply::None);
    let Reply::Continue(text) = end(&root, 3) else {
        panic!("no continue")
    };
    assert_eq!(text.as_bytes(), b"Before you end: re-read the task. List each requirement it states and, for each, the evidence that it is met: a command you ran and what it showed, or a file you checked. Fix any gap you find, then end your turn.");
    assert_eq!(end(&root, 3), Reply::None);
}

#[test]
fn a_turn_without_a_tool_use_is_never_asked() {
    let root = plugin(Some(identity("/")));
    assert_eq!(end(&root, 1), Reply::None);
    respond(&root, 2, "end_turn");
    respond(&root, 2, "pause_turn");
    assert_eq!(end(&root, 2), Reply::None);
    respond(&root, 2, "tool_use");
    respond(&root, 3, "end_turn");
    assert_eq!(end(&root, 3), Reply::None);
}

#[test]
fn a_child_or_an_agent_outside_a_tree_is_never_asked() {
    for identity in [Some(identity("/c")), None] {
        let other = plugin(identity.clone());
        respond(&other, 1, "tool_use");
        assert_eq!(end(&other, 1), Reply::None, "{identity:?}");
    }
}

#[test]
fn the_manifest_is_bundled_world_two_with_no_prompt_or_tools_and_both_hooks_switched() {
    let manifest = Manifest::parse(MANIFEST, Origin::Bundled).unwrap();
    assert_eq!(manifest.id, "self-check");
    assert_eq!(manifest.world, "0.2.0");
    assert!(manifest.system.is_none());
    assert!(manifest.tools.is_empty());
    assert_eq!(manifest.names().collect::<Vec<_>>(), [SETTING]);
    let hooks: Vec<(&str, &str, Option<&str>)> = manifest
        .hooks
        .iter()
        .map(|h| (h.name.as_str(), h.point.as_str(), h.switch.as_deref()))
        .collect();
    assert_eq!(
        hooks,
        [
            (WORKED, "after-response", Some(SETTING)),
            (REVIEW, "before-end", Some(SETTING)),
        ]
    );
    assert_eq!(manifest.hooks[1].timeout_ms, Some(2000));
}
