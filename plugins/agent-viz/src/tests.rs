use futures::executor::block_on;
use wisp_plugin_api::manifest::{AGENT_MAIL, AGENT_POINTS, Manifest, Origin, Switch};
use wisp_plugin_api::tree::{AgentNode, MailInfo, MailKind, QueueReason, ShownState};
use wisp_plugin_testkit::host::{FakeHost, agent_node};

use super::*;

const SECTION: &str = "topology";

fn render(tree: Vec<AgentNode>, states: Vec<ShownState>, view: SlotView) -> Vec<Block> {
    let host = FakeHost {
        tree,
        states,
        ..FakeHost::default()
    };
    let plugin = instantiate(Arc::new(host));
    block_on(plugin.render_slot(SECTION, view)).unwrap().blocks
}

fn shown(path: &str, state: NodeState) -> ShownState {
    ShownState {
        path: path.to_owned(),
        state,
        keep_warm: None,
    }
}

fn topology(nodes: &[(&str, Option<&str>, NodeState)], flows: Vec<Flow>) -> Vec<Block> {
    vec![Block::Topology(Topology {
        root: Some(ROOT_NAME.to_owned()),
        nodes: (nodes.iter())
            .map(|(key, parent, state)| TopologyNode {
                key: (*key).to_owned(),
                parent: parent.map(str::to_owned),
                state: *state,
            })
            .collect(),
        flows,
        counted: Some(Noun::Agents),
    })]
}

fn clocked(tree: Vec<AgentNode>) -> (Arc<dyn Plugin>, Arc<Mutex<u64>>) {
    let clock = Arc::new(Mutex::new(100_000));
    let read = clock.clone();
    let host = FakeHost {
        tree,
        now: Some(Arc::new(move || {
            let ms = *read.lock().unwrap();
            Instant {
                seconds: i64::try_from(ms / 1000).unwrap(),
                nanos: u32::try_from(ms % 1000).unwrap() * 1_000_000,
            }
        })),
        ..FakeHost::default()
    };
    (instantiate(Arc::new(host)), clock)
}

fn send(plugin: &dyn Plugin, from: &str, to: &str) {
    let sent = MailInfo {
        id: "m1".to_owned(),
        from: from.to_owned(),
        to: to.to_owned(),
        kind: MailKind::Message,
        wake: true,
        bytes: 3,
    };
    block_on(plugin.on_agent_hook("mail", TreeEvent::AgentMail(sent)));
}

fn flow(from: Option<&str>, to: Option<&str>, at_ms: u64) -> Flow {
    Flow {
        from: from.map(str::to_owned),
        to: to.map(str::to_owned),
        at_ms,
    }
}

#[test]
fn the_manifest_declares_one_sidebar_section_on_by_default_redrawn_by_the_agent_points_and_an_awaited_mail_hook()
 {
    let manifest = Manifest::parse(MANIFEST, Origin::Bundled).unwrap();
    assert_eq!(manifest.id, "agent-viz");
    let [slot] = manifest.slots.as_slice() else {
        panic!("{:?}", manifest.slots)
    };
    assert_eq!(
        (slot.name.as_str(), slot.title.as_str()),
        (SECTION, "Agents")
    );
    assert_eq!(slot.default, Switch::On);
    assert_eq!(slot.marks, AGENT_POINTS);
    let [hook] = manifest.hooks.as_slice() else {
        panic!("{:?}", manifest.hooks)
    };
    assert_eq!(
        (hook.name.as_str(), hook.point.as_str()),
        ("mail", AGENT_MAIL)
    );
    assert!(!hook.background);
    assert!(manifest.system.is_none() && manifest.tools.is_empty());
}

#[test]
fn no_agents_draw_nothing_on_the_panel_and_an_empty_topology_on_the_page() {
    assert_eq!(render(Vec::new(), Vec::new(), SlotView::Panel), []);
    assert_eq!(
        render(Vec::new(), Vec::new(), SlotView::Page),
        topology(&[], Vec::new())
    );
}

#[test]
fn six_agents_hang_from_their_parents_in_path_order_with_the_kernels_states() {
    use NodeState::{Done, Failed, Stopped, Waiting, Working};
    let paths = [
        "/review/coder",
        "/docs",
        "/review",
        "/review/helper",
        "/scout",
        "/docs/a",
    ];
    let tree = paths
        .iter()
        .map(|p| agent_node(p, AgentState::Running))
        .collect();
    let states = vec![
        shown("/review", Working),
        shown("/review/coder", Working),
        shown("/review/helper", Done),
        shown("/docs", Waiting),
        shown("/docs/a", Failed),
        shown("/scout", Stopped),
    ];
    assert_eq!(
        render(tree, states, SlotView::Page),
        topology(
            &[
                ("/docs", None, Waiting),
                ("/docs/a", Some("/docs"), Failed),
                ("/review", None, Working),
                ("/review/coder", Some("/review"), Working),
                ("/review/helper", Some("/review"), Done),
                ("/scout", None, Stopped),
            ],
            Vec::new()
        )
    );
}

#[test]
fn the_panel_carries_the_root_and_leaves_out_ended_agents_and_those_below_them() {
    let tree = vec![
        agent_node("/a", AgentState::Queued(QueueReason::Endpoint)),
        agent_node("/b", AgentState::Ended),
        agent_node("/b/c", AgentState::Running),
        agent_node("/bc", AgentState::Running),
    ];
    let states = vec![
        shown("/a", NodeState::Working),
        shown("/b", NodeState::Stopped),
        shown("/b/c", NodeState::Working),
        shown("/bc", NodeState::Done),
    ];
    assert_eq!(
        render(tree, states, SlotView::Panel),
        topology(
            &[
                ("/a", None, NodeState::Working),
                ("/bc", None, NodeState::Done)
            ],
            Vec::new()
        )
    );
    let ended = vec![agent_node("/b", AgentState::Ended)];
    assert_eq!(render(ended, Vec::new(), SlotView::Panel), []);
}

#[test]
fn an_agent_only_the_snapshot_has_waits_and_a_state_without_an_agent_is_skipped() {
    let tree = vec![agent_node("/new", AgentState::Running)];
    let states = vec![shown("/gone", NodeState::Working)];
    assert_eq!(
        render(tree, states, SlotView::Panel),
        topology(&[("/new", None, NodeState::Waiting)], Vec::new())
    );
}

#[test]
fn a_mail_flows_from_the_first_render_after_it_and_one_heard_2_s_before_that_render_is_dropped() {
    let (plugin, clock) = clocked(vec![agent_node("/a", AgentState::Running)]);
    send(&*plugin, "/", "/a");
    *clock.lock().unwrap() = 102_000;
    send(&*plugin, "/a", "/");
    *clock.lock().unwrap() = 102_400;
    let nodes = [("/a", None, NodeState::Waiting)];
    let fresh = vec![flow(Some("/a"), None, 102_400)];
    let panel = block_on(plugin.render_slot(SECTION, SlotView::Panel));
    assert_eq!(panel.unwrap().blocks, topology(&nodes, fresh.clone()));
    *clock.lock().unwrap() = 102_800;
    let page = block_on(plugin.render_slot(SECTION, SlotView::Page));
    assert_eq!(page.unwrap().blocks, topology(&nodes, fresh));
}

#[test]
fn the_newest_16_mails_are_kept_and_flows_to_ended_agents_are_dropped() {
    let tree = vec![
        agent_node("/a", AgentState::Running),
        agent_node("/b", AgentState::Ended),
    ];
    let (plugin, _clock) = clocked(tree);
    send(&*plugin, "/a", "/");
    for _ in 0..15 {
        send(&*plugin, "/", "/a");
    }
    send(&*plugin, "/a", "/b");
    let blocks = block_on(plugin.render_slot(SECTION, SlotView::Page));
    let nodes = [("/a", None, NodeState::Waiting)];
    let flows = vec![flow(None, Some("/a"), 100_000); 15];
    assert_eq!(blocks.unwrap().blocks, topology(&nodes, flows));
}
