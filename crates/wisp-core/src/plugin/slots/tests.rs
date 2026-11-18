use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::time::Instant;
use wisp_plugin_api::slots::{Block, Content, Flow, Node, NodeState, Noun, Topology, TopologyNode};
use wisp_plugin_testkit::hooks::{self as kit, Calls, Render};
use wisp_protocol::slot::SlotView;

use super::*;
use crate::plugin::caller::Caller;
use crate::plugin::{KernelHost, Make, PluginConfig, Registry, convert};

enum Step {
    Says(&'static str),
    Gives(Content),
    Errs(String),
    Panics,
    Takes(u64),
}

fn script(steps: Vec<Step>) -> Render {
    let steps = Arc::new(Mutex::new(VecDeque::from(steps)));
    Arc::new(move |_, _, _| {
        let step = locked(&steps).pop_front().unwrap_or(Step::Says("steady"));
        Box::pin(async move {
            match step {
                Step::Says(text) => Ok(kit::line(text)),
                Step::Gives(content) => Ok(content),
                Step::Errs(message) => Err(message),
                Step::Panics => panic!("render broke"),
                Step::Takes(ms) => {
                    tokio::time::sleep(Duration::from_millis(ms)).await;
                    Ok(kit::line("slow"))
                }
            }
        })
    })
}

fn timed(at: Arc<Mutex<Vec<(SlotView, Instant)>>>, ms: u64) -> (Render, Arc<AtomicUsize>) {
    let flight = Arc::new(AtomicUsize::new(0));
    let overlaps = Arc::new(AtomicUsize::new(0));
    let counted = overlaps.clone();
    let render: Render = Arc::new(move |_, _, view| {
        let at = at.clone();
        let flight = flight.clone();
        let overlaps = counted.clone();
        Box::pin(async move {
            let wire = match view {
                wisp_plugin_api::slots::SlotView::Panel => SlotView::Panel,
                wisp_plugin_api::slots::SlotView::Page => SlotView::Page,
            };
            locked(&at).push((wire, Instant::now()));
            if flight.fetch_add(1, Ordering::SeqCst) > 0 {
                overlaps.fetch_add(1, Ordering::SeqCst);
            }
            tokio::time::sleep(Duration::from_millis(ms)).await;
            flight.fetch_sub(1, Ordering::SeqCst);
            Ok(kit::line(&format!("{:?}", Instant::now())))
        })
    });
    (render, overlaps)
}

fn no_failures(seen: &[AgentEvent]) {
    assert!(seen.iter().all(|e| failure_of(e).is_none()), "{seen:?}");
}

struct Bench {
    lane: SlotLane,
    events: mpsc::Receiver<AgentEvent>,
    tx: mpsc::Sender<AgentEvent>,
    calls: Calls,
}

const S: &str = "aa/sec";

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

async fn bench_of(sources: Vec<(String, Make)>, calls: Calls) -> Bench {
    let (registry, problems) = Registry::from_sources(sources, &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let started = registry
        .start(&BTreeSet::new(), Arc::new(KernelHost::new()))
        .await;
    let lane = SlotLane::start(
        &started.hooks.sections,
        &started.hooks.plugins,
        Default::default(),
    );
    let (tx, events) = mpsc::channel(256);
    Bench {
        lane,
        events,
        tx,
        calls,
    }
}

async fn bench(body: &str, render: Render) -> Bench {
    let calls = Calls::default();
    let make: Make = kit::sections(kit::none(), render, calls.clone());
    bench_of(vec![(kit::slotted("aa", body), make)], calls).await
}

impl Bench {
    fn show(&self, sections: &[&str], pages: &[&str]) {
        self.lane.show(names(sections), names(pages), &self.tx);
    }

    async fn next(&mut self) -> AgentEvent {
        tokio::time::timeout(Duration::from_secs(3_600), self.events.recv())
            .await
            .unwrap()
            .unwrap()
    }

    async fn during(&mut self, ms: u64) -> Vec<AgentEvent> {
        let until = Instant::now() + Duration::from_millis(ms);
        let mut out = Vec::new();
        while let Ok(Some(e)) = tokio::time::timeout_at(until, self.events.recv()).await {
            out.push(e);
        }
        out
    }

    fn renders(&self, view: SlotView) -> usize {
        let view = convert::slot_view(view);
        self.calls
            .renders()
            .iter()
            .filter(|(_, v)| *v == view)
            .count()
    }
}

fn content_of(event: &AgentEvent) -> Option<(SlotView, String)> {
    match event {
        AgentEvent::SlotContent { view, content, .. } => {
            Some((*view, serde_json::to_string(content).unwrap()))
        }
        _ => None,
    }
}

fn failure_of(event: &AgentEvent) -> Option<(SlotView, SlotFailure, bool, bool)> {
    match event {
        AgentEvent::SlotFailed {
            view,
            failure,
            parked,
            restarted,
            ..
        } => Some((*view, failure.clone(), *parked, *restarted)),
        _ => None,
    }
}

fn section_body(extra: &str) -> String {
    kit::section("sec", extra)
}

#[tokio::test(start_paused = true)]
async fn nothing_renders_before_a_client_shows_a_section_and_showing_it_renders_once() {
    let mut b = bench(&section_body("marks = [\"after-turn\"]\n"), script(vec![])).await;
    b.lane.signals().mark("after-turn");
    assert!(b.during(1_000).await.is_empty());
    assert_eq!(b.renders(SlotView::Panel), 0);
    b.show(&[S], &[]);
    let first = b.next().await;
    assert_eq!(
        first,
        AgentEvent::SlotContent {
            section: S.to_owned(),
            view: SlotView::Panel,
            content: convert::slot_content(kit::line("steady")),
        }
    );
    assert!(b.during(1_000).await.is_empty());
    assert_eq!(b.renders(SlotView::Panel), 1);
}

#[tokio::test(start_paused = true)]
async fn a_mark_renders_only_a_shown_section_and_a_hidden_one_renders_again_when_shown() {
    let steps = vec![Step::Says("one"), Step::Says("two"), Step::Says("three")];
    let mut b = bench(&section_body("marks = [\"after-turn\"]\n"), script(steps)).await;
    b.show(&[S], &[]);
    b.next().await;
    b.lane.signals().mark("after-turn");
    b.next().await;
    assert_eq!(b.renders(SlotView::Panel), 2);
    b.show(&[], &[]);
    b.during(100).await;
    b.lane.signals().mark("after-turn");
    b.lane.signals().mark("before-request");
    assert!(b.during(1_000).await.is_empty());
    assert_eq!(b.renders(SlotView::Panel), 2);
    b.show(&[S], &[]);
    let back = b.next().await;
    assert!(content_of(&back).unwrap().1.contains("three"), "{back:?}");
    assert_eq!(b.renders(SlotView::Panel), 3);
}

#[tokio::test(start_paused = true)]
async fn a_point_the_section_doesnt_name_marks_nothing() {
    let mut b = bench(&section_body("marks = [\"after-turn\"]\n"), script(vec![])).await;
    b.show(&[S], &[]);
    b.next().await;
    b.lane.signals().mark("before-request");
    b.during(1_000).await;
    assert_eq!(b.renders(SlotView::Panel), 1);
}

#[tokio::test(start_paused = true)]
async fn twenty_marks_in_a_second_give_at_most_one_call_per_250_ms() {
    let at = Arc::new(Mutex::new(Vec::new()));
    let (render, overlaps) = timed(at.clone(), 10);
    let mut b = bench(&section_body("marks = [\"after-turn\"]\n"), render).await;
    b.show(&[S], &[]);
    for _ in 0..20 {
        b.lane.signals().mark("after-turn");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    no_failures(&b.during(1_000).await);
    assert_eq!(overlaps.load(Ordering::SeqCst), 0);
    let at = locked(&at).clone();
    assert!(at.len() <= 5, "{}", at.len());
    assert!(at.len() >= 4, "{}", at.len());
    for pair in at.windows(2) {
        assert!(pair[1].1 - pair[0].1 >= MIN_GAP, "{at:?}");
    }
}

#[tokio::test(start_paused = true)]
async fn a_mark_made_during_a_call_waits_for_the_next_call() {
    let at = Arc::new(Mutex::new(Vec::new()));
    let (render, overlaps) = timed(at.clone(), 300);
    let mut b = bench(&section_body("marks = [\"after-turn\"]\n"), render).await;
    b.show(&[S], &[]);
    tokio::time::sleep(Duration::from_millis(100)).await;
    b.lane.signals().mark("after-turn");
    no_failures(&b.during(2_000).await);
    assert_eq!(overlaps.load(Ordering::SeqCst), 0);
    assert_eq!(locked(&at).len(), 2);
}

#[tokio::test(start_paused = true)]
async fn refresh_marks_every_period_only_while_shown() {
    let mut b = bench(&section_body("refresh_s = 10\n"), script(vec![])).await;
    b.during(60_000).await;
    assert_eq!(b.renders(SlotView::Panel), 0);
    b.show(&[S], &[]);
    b.during(35_000).await;
    assert_eq!(b.renders(SlotView::Panel), 4);
    b.show(&[], &[]);
    b.during(60_000).await;
    assert_eq!(b.renders(SlotView::Panel), 4);
}

#[tokio::test(start_paused = true)]
async fn equal_content_sends_nothing_but_the_first_good_render_after_a_failure_is_sent() {
    let steps = vec![
        Step::Says("same"),
        Step::Says("same"),
        Step::Errs("down".to_owned()),
        Step::Says("same"),
        Step::Says("same"),
    ];
    let mut b = bench(&section_body("marks = [\"after-turn\"]\n"), script(steps)).await;
    b.show(&[S], &[]);
    let mut seen = b.during(400).await;
    for _ in 0..4 {
        b.lane.signals().mark("after-turn");
        seen.extend(b.during(400).await);
    }
    assert_eq!(b.renders(SlotView::Panel), 5);
    let kinds: Vec<&str> = seen
        .iter()
        .map(|e| match e {
            AgentEvent::SlotContent { .. } => "content",
            AgentEvent::SlotFailed { .. } => "failed",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["content", "failed", "content"]);
}

fn sized(bytes: usize) -> Content {
    let base = serde_json::to_vec(&convert::slot_content(kit::line("")))
        .unwrap()
        .len();
    kit::line(&"x".repeat(bytes - base))
}

fn listed(items: usize) -> Content {
    Content {
        note: Vec::new(),
        blocks: vec![Block::List(vec![vec![kit::text("i")]; items])],
    }
}

fn deep() -> Content {
    Content {
        note: Vec::new(),
        blocks: vec![Block::Tree(vec![Node {
            depth: 1,
            state: NodeState::Working,
            role: None,
            label: "x".to_owned(),
            facts: Vec::new(),
        }])],
    }
}

fn tangled() -> Content {
    let node = |key: &str, parent: Option<&str>| TopologyNode {
        key: key.to_owned(),
        parent: parent.map(str::to_owned),
        state: NodeState::Done,
    };
    Content {
        note: Vec::new(),
        blocks: vec![Block::Topology(Topology {
            root: Some("this chat".to_owned()),
            nodes: vec![node("a/b", Some("a")), node("a", None)],
            flows: vec![Flow {
                from: Some("a/b".to_owned()),
                to: None,
                at_ms: 5,
            }],
            counted: Some(Noun::Agents),
        })],
    }
}

#[test]
fn a_topology_crosses_with_its_root_keys_parents_and_states() {
    let wire = convert::slot_content(tangled());
    assert_eq!(
        serde_json::to_string(&wire).unwrap(),
        r#"{"blocks":[{"type":"topology","root":"this chat","nodes":[{"key":"a/b","parent":"a","state":"done"},{"key":"a","state":"done"}],"flows":[{"from":"a/b","at_ms":5}],"counted":"agents"}]}"#
    );
}

#[tokio::test(start_paused = true)]
async fn every_kind_of_failure_emits_one_slot_failed_and_keeps_the_last_content() {
    let steps = vec![
        Step::Says("good"),
        Step::Errs("nope".to_owned()),
        Step::Says("good"),
        Step::Panics,
        Step::Says("good"),
        Step::Takes(600),
        Step::Says("good"),
        Step::Gives(sized(16_385)),
        Step::Says("good"),
        Step::Gives(listed(201)),
        Step::Says("good"),
        Step::Gives(deep()),
        Step::Says("good"),
        Step::Gives(tangled()),
    ];
    let mut b = bench(&section_body("marks = [\"after-turn\"]\n"), script(steps)).await;
    b.show(&[S], &[]);
    let mut seen = b.during(400).await;
    for _ in 0..13 {
        b.lane.signals().mark("after-turn");
        seen.extend(b.during(1_000).await);
    }
    let failures: Vec<SlotFailure> = seen
        .iter()
        .filter_map(failure_of)
        .map(|(_, f, parked, restarted)| {
            assert!(!parked && !restarted);
            f
        })
        .collect();
    assert_eq!(
        failures,
        [
            SlotFailure::Failed {
                message: "nope".to_owned()
            },
            SlotFailure::Failed {
                message: crate::plugin::panicked("aa")
            },
            SlotFailure::TimedOut,
            SlotFailure::TooLarge { bytes: 16_385 },
            SlotFailure::Invalid {
                rule: "too_many_items".to_owned()
            },
            SlotFailure::Invalid {
                rule: "tree_depth".to_owned()
            },
            SlotFailure::Invalid {
                rule: "topology_keys".to_owned()
            },
        ]
    );
    let current = b.lane.current();
    assert!(
        matches!(&current[0], AgentEvent::SlotContent { content, .. } if *content == convert::slot_content(kit::line("good"))),
        "{current:?}"
    );
    assert!(matches!(&current[1], AgentEvent::SlotFailed { .. }));
}

#[tokio::test(start_paused = true)]
async fn a_long_error_comes_out_on_one_line_of_at_most_200_characters() {
    let message = format!("first\n{}", "y".repeat(300));
    let mut b = bench(&section_body(""), script(vec![Step::Errs(message)])).await;
    b.show(&[S], &[]);
    let (_, failure, _, _) = failure_of(&b.next().await).unwrap();
    let SlotFailure::Failed { message } = failure else {
        panic!("{failure:?}");
    };
    assert_eq!(message.chars().count(), MESSAGE_CHARS);
    assert!(message.starts_with("first y"), "{message}");
}

fn erring() -> Render {
    Arc::new(|_, _, _| Box::pin(async { Err("down".to_owned()) }))
}

#[tokio::test(start_paused = true)]
async fn the_third_failure_parks_and_retries_come_after_30_then_60_seconds() {
    let mut b = bench(&section_body("marks = [\"after-turn\"]\n"), erring()).await;
    b.show(&[S], &[]);
    let mut parked = Vec::new();
    for _ in 0..3 {
        let event = b.next().await;
        parked.push(failure_of(&event).unwrap().2);
        b.lane.signals().mark("after-turn");
    }
    assert_eq!(parked, [false, false, true]);
    let parked_at = Instant::now();
    b.lane.signals().mark("after-turn");
    assert!(b.during(29_000).await.is_empty());
    let retry = b.next().await;
    assert!(failure_of(&retry).unwrap().2);
    let first = Instant::now() - parked_at;
    assert!(
        first >= FIRST_RETRY && first < FIRST_RETRY + MIN_GAP,
        "{first:?}"
    );
    let again_at = Instant::now();
    b.next().await;
    let second = Instant::now() - again_at;
    assert!(
        second >= FIRST_RETRY * 2 && second < FIRST_RETRY * 2 + MIN_GAP,
        "{second:?}"
    );
    assert_eq!(b.renders(SlotView::Panel), 5);
}

#[test]
fn retries_double_up_to_ten_minutes() {
    let waits: Vec<u64> = (1..=7).map(|n| retry_after(n).as_secs()).collect();
    assert_eq!(waits, [30, 60, 120, 240, 480, 600, 600]);
}

#[tokio::test(start_paused = true)]
async fn a_good_render_resets_the_count_and_hiding_and_showing_unparks_at_once() {
    let steps = vec![
        Step::Errs("a".to_owned()),
        Step::Errs("b".to_owned()),
        Step::Says("fine"),
        Step::Errs("c".to_owned()),
        Step::Errs("d".to_owned()),
        Step::Errs("e".to_owned()),
        Step::Says("back"),
    ];
    let mut b = bench(&section_body("marks = [\"after-turn\"]\n"), script(steps)).await;
    b.show(&[S], &[]);
    let mut seen = Vec::new();
    for _ in 0..6 {
        seen.push(b.next().await);
        b.lane.signals().mark("after-turn");
    }
    let parked: Vec<bool> = seen
        .iter()
        .filter_map(failure_of)
        .map(|(_, _, p, _)| p)
        .collect();
    assert_eq!(parked, [false, false, false, false, true]);
    b.show(&[], &[]);
    b.during(10).await;
    b.show(&[S], &[]);
    let back = b.next().await;
    assert!(content_of(&back).unwrap().1.contains("back"), "{back:?}");
}

#[tokio::test(start_paused = true)]
async fn a_page_renders_under_its_own_limit_and_never_beside_its_panel() {
    let at = Arc::new(Mutex::new(Vec::new()));
    let (render, overlaps) = timed(at.clone(), 100);
    let mut b = bench(&section_body("marks = [\"after-turn\"]\n"), render).await;
    b.show(&[S], &[S]);
    for _ in 0..8 {
        b.lane.signals().mark("after-turn");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    no_failures(&b.during(1_000).await);
    assert_eq!(
        overlaps.load(Ordering::SeqCst),
        0,
        "a panel and a page rendered at once"
    );
    let at = locked(&at).clone();
    for view in [SlotView::Panel, SlotView::Page] {
        let times: Vec<Instant> = at
            .iter()
            .filter(|(v, _)| *v == view)
            .map(|(_, t)| *t)
            .collect();
        assert!(times.len() >= 3, "{view:?} {at:?}");
        for pair in times.windows(2) {
            assert!(pair[1] - pair[0] >= MIN_GAP, "{view:?} {at:?}");
        }
    }
    let closest = at
        .windows(2)
        .map(|pair| pair[1].1 - pair[0].1)
        .min()
        .unwrap();
    assert!(
        closest < MIN_GAP,
        "a shared limit would space every start: {at:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_page_gets_its_own_content_and_a_page_no_client_shows_gets_no_call() {
    let mut b = bench(&section_body(""), kit::views()).await;
    b.show(&[S], &[]);
    b.during(1_000).await;
    assert_eq!(b.renders(SlotView::Page), 0);
    b.show(&[S], &[S]);
    let page = b.next().await;
    let (view, text) = content_of(&page).unwrap();
    assert_eq!(view, SlotView::Page);
    assert!(
        text.contains("page sec") && text.contains("next_column"),
        "{text}"
    );
    b.show(&[S], &[]);
    b.during(1_000).await;
    assert_eq!(b.renders(SlotView::Page), 1);
    assert!(b.lane.current().iter().all(|e| matches!(
        e,
        AgentEvent::SlotContent {
            view: SlotView::Panel,
            ..
        }
    )));
}

#[tokio::test(start_paused = true)]
async fn a_page_takes_65536_bytes_where_a_panel_refuses_16385() {
    let big: Render = Arc::new(|_, _, view| {
        Box::pin(async move {
            Ok(match view {
                wisp_plugin_api::slots::SlotView::Panel => sized(16_385),
                wisp_plugin_api::slots::SlotView::Page => sized(65_536),
            })
        })
    });
    let mut b = bench(&section_body(""), big).await;
    b.show(&[S], &[S]);
    let seen = b.during(2_000).await;
    assert_eq!(seen.len(), 2);
    let failures: Vec<_> = seen.iter().filter_map(failure_of).collect();
    assert_eq!(
        failures,
        [(
            SlotView::Panel,
            SlotFailure::TooLarge { bytes: 16_385 },
            false,
            false
        )]
    );
    let pages: Vec<SlotView> = seen.iter().filter_map(content_of).map(|(v, _)| v).collect();
    assert_eq!(pages, [SlotView::Page]);
}

#[tokio::test(start_paused = true)]
async fn a_parked_page_leaves_its_panel_rendering_and_reopening_it_unparks_it() {
    let half: Render = Arc::new(|_, _, view| {
        Box::pin(async move {
            match view {
                wisp_plugin_api::slots::SlotView::Panel => {
                    Ok(kit::line(&format!("{:?}", Instant::now())))
                }
                wisp_plugin_api::slots::SlotView::Page => Err("page down".to_owned()),
            }
        })
    });
    let mut b = bench(&section_body("marks = [\"after-turn\"]\n"), half).await;
    b.show(&[S], &[S]);
    let mut seen = Vec::new();
    for _ in 0..5 {
        seen.extend(b.during(400).await);
        b.lane.signals().mark("after-turn");
    }
    seen.extend(b.during(400).await);
    let page_parked: Vec<bool> = seen
        .iter()
        .filter_map(failure_of)
        .map(|(view, _, parked, _)| {
            assert_eq!(view, SlotView::Page);
            parked
        })
        .collect();
    assert_eq!(page_parked, [false, false, true]);
    let panels = seen.iter().filter_map(content_of).count();
    assert!(panels >= 5, "{seen:?}");
    let pages_before = b.renders(SlotView::Page);
    b.show(&[S], &[]);
    b.during(10).await;
    b.show(&[S], &[S]);
    b.during(100).await;
    assert_eq!(b.renders(SlotView::Page), pages_before + 1);
}

#[tokio::test(start_paused = true)]
async fn an_unknown_name_is_ignored_and_a_detached_client_shows_nothing() {
    let mut b = bench(&section_body("marks = [\"after-turn\"]\n"), script(vec![])).await;
    b.show(&["zz/nope", "plain", S], &["zz/nope"]);
    b.next().await;
    b.lane.detach();
    b.during(10).await;
    b.lane.signals().mark("after-turn");
    b.during(1_000).await;
    assert_eq!(b.renders(SlotView::Panel), 1);
    assert_eq!(b.calls.renders().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_panic_fails_only_its_call_and_the_lane_keeps_going() {
    let mut b = bench(
        &section_body("marks = [\"after-turn\"]\n"),
        script(vec![Step::Panics, Step::Says("alive")]),
    )
    .await;
    b.show(&[S], &[]);
    assert!(failure_of(&b.next().await).is_some());
    b.lane.signals().mark("after-turn");
    assert!(content_of(&b.next().await).unwrap().1.contains("alive"));
}

fn sources(list: &[(&str, &str)]) -> Vec<(String, Make)> {
    list.iter()
        .map(|(id, body)| {
            let make: Make = kit::sections(kit::none(), kit::views(), Calls::default());
            (kit::slotted(id, body), make)
        })
        .collect()
}

fn ordered(sections: &Sections) -> Vec<&str> {
    sections.list().iter().map(|s| s.name.as_str()).collect()
}

#[test]
fn sections_are_ordered_by_after_and_before_then_by_name_and_a_cycle_is_named() {
    let list = [
        ("cc", kit::section("zz", "after = [\"aa/bc\"]\n")),
        (
            "aa",
            kit::section("bc", "") + &kit::section("ab", "before = [\"aa/bc\"]\n"),
        ),
        ("bb", kit::section("mm", "")),
    ];
    let list: Vec<(&str, &str)> = list.iter().map(|(i, b)| (*i, b.as_str())).collect();
    let (registry, problems) = Registry::from_sources(sources(&list), &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let sections = registry.sections(&[]);
    assert_eq!(ordered(&sections), ["aa/ab", "aa/bc", "bb/mm", "cc/zz"]);
    let mut shuffled = list.clone();
    shuffled.reverse();
    let (again, _) = Registry::from_sources(sources(&shuffled), &PluginConfig::default());
    assert_eq!(again.sections(&[]), sections);
    let cyclic = [
        ("aa", kit::section("xx", "after = [\"bb/yy\"]\n")),
        ("bb", kit::section("yy", "after = [\"aa/xx\"]\n")),
        ("cc", kit::section("ok", "after = [\"nobody/here\"]\n")),
    ];
    let cyclic: Vec<(&str, &str)> = cyclic.iter().map(|(i, b)| (*i, b.as_str())).collect();
    let (registry, problems) = Registry::from_sources(sources(&cyclic), &PluginConfig::default());
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(problems[0].message.contains("aa/xx") && problems[0].message.contains("cycle"));
    let sections = registry.sections(&[]);
    assert_eq!(ordered(&sections), ["cc/ok", "aa/xx", "bb/yy"]);
    assert!(sections.list()[1].cycle.is_some());
    let notes: Vec<&HookProblem> = sections.problems().iter().filter(|p| !p.cycle).collect();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].message.contains("nobody/here"));
    let catalog = registry.catalog(&BTreeSet::new(), &[]);
    let entries: Vec<SlotEntry> = catalog
        .plugins
        .iter()
        .flat_map(|p| p.slots.clone())
        .collect();
    let mut by_order = entries.clone();
    by_order.sort_by_key(|e| e.order);
    let catalog_order: Vec<&str> = by_order.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(catalog_order, ordered(&sections));
    assert!(matches!(
        &by_order[1].disabled,
        Some(SlotDisabled::Cycle { cycle }) if cycle.len() == 3
    ));
}

#[tokio::test]
async fn the_lane_starts_sections_in_the_catalogs_order() {
    let list = [
        ("cc", kit::section("zz", "")),
        ("aa", kit::section("bc", "before = [\"cc/zz\"]\n")),
        ("bb", kit::section("mm", "after = [\"cc/zz\"]\n")),
    ];
    let calls = Calls::default();
    let sources: Vec<(String, Make)> = list
        .iter()
        .map(|(id, body)| {
            let make: Make = kit::sections(kit::none(), kit::views(), calls.clone());
            (kit::slotted(id, body), make)
        })
        .collect();
    let mut b = bench_of(sources, calls).await;
    b.show(&["aa/bc", "bb/mm", "cc/zz"], &[]);
    let mut order = Vec::new();
    for _ in 0..3 {
        if let AgentEvent::SlotContent { section, .. } = b.next().await {
            order.push(section);
        }
    }
    let started: Vec<String> = b.calls.renders().into_iter().map(|(n, _)| n).collect();
    assert_eq!(started, ["bc", "zz", "mm"]);
    order.sort();
    assert_eq!(order, ["aa/bc", "bb/mm", "cc/zz"]);
}

#[tokio::test(start_paused = true)]
async fn imports_called_before_a_render_returns_its_future_are_refused_at_once() {
    let seen = kit::Seen::default();
    let mut b = bench(&section_body(""), kit::eager(seen.clone())).await;
    let shown = Instant::now();
    b.show(&[S], &[]);
    let first = b.next().await;
    assert!(content_of(&first).unwrap().1.contains("eager"), "{first:?}");
    assert!(Instant::now() - shown < CALL_LIMIT);
    assert_eq!(*locked(&seen), ["count Unsupported", "fire 0"]);
}

#[tokio::test(start_paused = true)]
async fn showing_a_parked_section_again_restarts_its_back_off_at_30_seconds() {
    let mut b = bench(&section_body("marks = [\"after-turn\"]\n"), erring()).await;
    b.show(&[S], &[]);
    for _ in 0..3 {
        b.next().await;
        b.lane.signals().mark("after-turn");
    }
    let retried = b.next().await;
    assert!(failure_of(&retried).unwrap().2, "{retried:?}");
    b.show(&[], &[]);
    b.during(10).await;
    b.show(&[S], &[]);
    let mut parked = Vec::new();
    for _ in 0..3 {
        parked.push(failure_of(&b.next().await).unwrap().2);
        b.lane.signals().mark("after-turn");
    }
    assert_eq!(parked, [false, false, true]);
    let parked_at = Instant::now();
    b.next().await;
    let wait = Instant::now() - parked_at;
    assert!(
        wait >= FIRST_RETRY && wait < FIRST_RETRY + MIN_GAP,
        "{wait:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_panel_hidden_while_it_renders_sends_nothing_until_shown_again() {
    let mut b = bench(
        &section_body(""),
        script(vec![Step::Takes(300), Step::Takes(10)]),
    )
    .await;
    b.show(&[S], &[]);
    tokio::time::sleep(Duration::from_millis(100)).await;
    b.show(&[], &[]);
    assert!(b.during(1_000).await.is_empty());
    b.show(&[S], &[]);
    let back = b.next().await;
    assert!(content_of(&back).unwrap().1.contains("slow"), "{back:?}");
}

#[tokio::test(start_paused = true)]
async fn a_slow_section_marked_all_the_time_leaves_its_plugins_other_section_its_turn() {
    let body = kit::section("ab", "marks = [\"after-turn\"]\n")
        + &kit::section("bc", "marks = [\"after-turn\"]\n");
    let (render, _) = timed(Arc::new(Mutex::new(Vec::new())), 300);
    let mut b = bench(&body, render).await;
    b.show(&["aa/ab", "aa/bc"], &[]);
    for _ in 0..10 {
        b.lane.signals().mark("after-turn");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    b.during(1_000).await;
    let renders = b.calls.renders();
    let of = |name: &str| renders.iter().filter(|(n, _)| n == name).count();
    assert!(of("ab") >= 2 && of("bc") >= 2, "{renders:?}");
}

const WINDOW: &str = "context/window";

const WINDOW_MARKS: [&str; 5] = [
    "after-response",
    "after-compact",
    "rewound",
    "model-switched",
    "settings-changed",
];

async fn context_bench() -> (Bench, Arc<KernelHost>) {
    let host = Arc::new(KernelHost::new());
    let (registry, problems) = Registry::from_sources(
        crate::plugin::testing::turn_line_sources(),
        &PluginConfig::default(),
    );
    assert!(problems.is_empty(), "{problems:?}");
    let started = registry.start(&BTreeSet::new(), host.clone()).await;
    let lane = SlotLane::start(
        &started.hooks.sections,
        &started.hooks.plugins,
        Default::default(),
    );
    let (tx, events) = mpsc::channel(256);
    let bench = Bench {
        lane,
        events,
        tx,
        calls: Calls::default(),
    };
    (bench, host)
}

#[tokio::test(start_paused = true)]
async fn the_context_section_redraws_at_each_of_its_marks_only() {
    let (mut b, host) = context_bench().await;
    host.set_estimate(1_000);
    b.show(&[WINDOW], &[]);
    let first = content_of(&b.next().await).unwrap().1;
    assert!(first.contains("\"used\":1000,"), "{first}");
    for (n, point) in (2u64..).zip(WINDOW_MARKS) {
        host.set_estimate(n * 1_000);
        b.lane.signals().mark(point);
        let drawn = content_of(&b.next().await).unwrap().1;
        assert!(
            drawn.contains(&format!("\"used\":{n}000,")),
            "{point}: {drawn}"
        );
        assert!(b.during(1_000).await.is_empty(), "{point}");
    }
    for point in [
        "after-turn",
        "before-request",
        "session-start",
        "before-turn",
    ] {
        host.set_estimate(99_000);
        b.lane.signals().mark(point);
        assert!(b.during(1_000).await.is_empty(), "{point}");
    }
}

#[tokio::test]
async fn the_default_catalog_lists_the_context_section_off() {
    let (registry, problems) = Registry::from_sources(
        crate::plugin::testing::turn_line_sources(),
        &PluginConfig::default(),
    );
    assert!(problems.is_empty(), "{problems:?}");
    let started = registry
        .start(&BTreeSet::new(), Arc::new(KernelHost::new()))
        .await;
    let entries = started.hooks.sections.entries("context");
    let names: Vec<(&str, bool)> = entries
        .iter()
        .map(|e| (e.name.as_str(), e.default_on))
        .collect();
    let current = format!(
        "context/{}",
        wisp_protocol::settings::current_slot("context", "usage")
    );
    assert_eq!(names, [(current.as_str(), false)]);
    assert_eq!(entries[0].title, "Context");
}

const USAGE_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../wisp-tui/tests/fixtures/context-usage.json"
);

fn usage_fill() -> wisp_plugin_api::host::Fill {
    use wisp_plugin_api::host::{FillPart, PartTokens};
    let part = |part, tokens| PartTokens { part, tokens };
    wisp_plugin_api::host::Fill {
        estimate: 160_000,
        measured: Some(159_000),
        window: Some(1_000_000),
        point: Some(400_000),
        ceiling: Some(850_000),
        auto_compact: true,
        epoch: 1,
        parts: vec![
            part(FillPart::System, 12_000),
            part(FillPart::Tools, 18_000),
            part(FillPart::Chat, 129_000),
        ],
        cached: Some(96),
    }
}

#[test]
fn the_context_sections_content_is_the_tui_fixture_and_the_same_for_the_page() {
    let host = wisp_plugin_testkit::host::FakeHost {
        fill: Some(usage_fill()),
        ..wisp_plugin_testkit::host::FakeHost::default()
    };
    let plugin = (wisp_plugin_context::bundled().instantiate)(Arc::new(host));
    let rendered = |view| {
        let content = futures::FutureExt::now_or_never(plugin.render_slot("window", view))
            .unwrap()
            .unwrap();
        convert::slot_content(content)
    };
    let content = rendered(wisp_plugin_api::slots::SlotView::Panel);
    assert_eq!(content, rendered(wisp_plugin_api::slots::SlotView::Page));
    assert_eq!(
        wisp_protocol::slot::check(&content, SlotView::Panel),
        Ok(())
    );
    let now = serde_json::to_string_pretty(&content).unwrap() + "\n";
    if std::env::var_os("WISP_BLESS").is_some() {
        std::fs::write(USAGE_FIXTURE, &now).unwrap();
    }
    let saved = std::fs::read_to_string(USAGE_FIXTURE).unwrap();
    assert!(
        saved == now,
        "the context section changed; run with WISP_BLESS=1 if that is intended\n--- saved\n{saved}\n--- now\n{now}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_disabled_plugin_gets_no_calls() {
    let mut b = bench(&section_body("marks = [\"after-turn\"]\n"), script(vec![])).await;
    b.lane
        .signals()
        .set_off(names(&["aa"]).into_iter().collect());
    b.show(&[S], &[S]);
    b.lane.signals().mark("after-turn");
    assert!(b.during(60_000).await.is_empty());
    assert_eq!(b.calls.renders().len(), 0);
}

#[tokio::test(start_paused = true)]
async fn switching_its_plugin_off_and_on_unparks_it() {
    let steps = vec![
        Step::Errs("a".to_owned()),
        Step::Errs("b".to_owned()),
        Step::Errs("c".to_owned()),
        Step::Says("back"),
    ];
    let mut b = bench(&section_body("marks = [\"after-turn\"]\n"), script(steps)).await;
    b.show(&[S], &[]);
    let mut parked = Vec::new();
    for _ in 0..3 {
        parked.push(failure_of(&b.next().await).unwrap().2);
        b.lane.signals().mark("after-turn");
    }
    assert_eq!(parked, [false, false, true]);
    b.lane
        .signals()
        .set_off(names(&["aa"]).into_iter().collect());
    assert!(b.during(1_000).await.is_empty());
    assert_eq!(b.renders(SlotView::Panel), 3);
    b.lane.signals().set_off(BTreeSet::new());
    let on_at = Instant::now();
    let back = b.next().await;
    assert!(
        Instant::now() - on_at < MIN_GAP,
        "{:?}",
        Instant::now() - on_at
    );
    assert!(content_of(&back).unwrap().1.contains("back"), "{back:?}");
    assert_eq!(b.renders(SlotView::Panel), 4);
}

struct Locked {
    inner: Arc<dyn Plugin>,
    lock: Arc<tokio::sync::RwLock<()>>,
}

impl Plugin for Locked {
    fn render_slot<'a>(
        &'a self,
        name: &'a str,
        view: wisp_plugin_api::slots::SlotView,
    ) -> wisp_plugin_api::BoxFuture<'a, Result<Content, String>> {
        self.inner.render_slot(name, view)
    }

    fn lock_for_render(&self) -> Option<Box<dyn Send>> {
        let held = self.lock.clone().try_write_owned().ok()?;
        Some(Box::new(held))
    }

    fn restarts_when_cut(&self) -> bool {
        true
    }
}

async fn locked_bench(body: &str, render: Render, lock: Arc<tokio::sync::RwLock<()>>) -> Bench {
    let calls = Calls::default();
    let inner = kit::sections(kit::none(), render, calls.clone());
    let make: Make = Arc::new(move |host| {
        let lock = lock.clone();
        Arc::new(Locked {
            inner: inner(host),
            lock,
        })
    });
    bench_of(vec![(kit::slotted("aa", body), make)], calls).await
}

#[tokio::test(start_paused = true)]
async fn a_cut_that_restarts_its_plugin_says_so_and_parks_with_no_retry() {
    let steps = vec![
        Step::Takes(600),
        Step::Takes(600),
        Step::Takes(600),
        Step::Errs("down".to_owned()),
        Step::Panics,
    ];
    let body = section_body("marks = [\"after-turn\"]\n");
    let mut b = locked_bench(&body, script(steps), Arc::default()).await;
    b.show(&[S], &[]);
    let mut seen = Vec::new();
    for _ in 0..3 {
        seen.push(failure_of(&b.next().await).unwrap());
        b.lane.signals().mark("after-turn");
    }
    let cut = |parked| (SlotView::Panel, SlotFailure::TimedOut, parked, true);
    assert_eq!(seen, [cut(false), cut(false), cut(true)]);
    assert_eq!(
        b.lane
            .current()
            .iter()
            .filter_map(failure_of)
            .collect::<Vec<_>>(),
        [cut(true)]
    );
    assert!(b.during(600_000).await.is_empty());
    assert_eq!(b.renders(SlotView::Panel), 3);
    b.show(&[], &[]);
    b.during(10).await;
    b.show(&[S], &[]);
    let shown = Instant::now();
    let erred = failure_of(&b.next().await).unwrap();
    assert!(Instant::now() - shown < MIN_GAP);
    let message = |message: String| SlotFailure::Failed { message };
    assert_eq!(
        erred,
        (SlotView::Panel, message("down".to_owned()), false, false)
    );
    b.lane.signals().mark("after-turn");
    let panicked = failure_of(&b.next().await).unwrap();
    let panic = message(crate::plugin::panicked("aa"));
    assert_eq!(panicked, (SlotView::Panel, panic, false, true));
}

#[tokio::test(start_paused = true)]
async fn a_plugin_busy_with_a_call_keeps_its_section_marked_and_renders_once_free() {
    let lock = Arc::new(tokio::sync::RwLock::new(()));
    let call = lock.clone().read_owned().await;
    let mut b = locked_bench(&section_body(""), script(vec![]), lock).await;
    b.show(&[S], &[]);
    assert!(b.during(1_000).await.is_empty());
    assert_eq!(b.renders(SlotView::Panel), 0);
    drop(call);
    let freed = Instant::now();
    assert!(content_of(&b.next().await).is_some());
    assert!(Instant::now() - freed <= MIN_GAP);
    assert!(b.during(1_000).await.is_empty());
    assert_eq!(b.renders(SlotView::Panel), 1);
}

#[tokio::test(start_paused = true)]
async fn a_render_runs_under_a_deadline_at_its_limit() {
    let seen = Arc::new(Mutex::new(None));
    let saw = seen.clone();
    let lock = Arc::new(tokio::sync::RwLock::new(()));
    let held = lock.clone();
    let render: Render = Arc::new(move |_, _, _| {
        let (saw, held) = (saw.clone(), held.clone());
        Box::pin(async move {
            let locked_out = held.try_read().is_err();
            *locked(&saw) = Some((Caller::current(), Instant::now(), locked_out));
            Ok(kit::line("timed"))
        })
    });
    let mut b = locked_bench(&section_body(""), render, lock).await;
    b.show(&[S], &[]);
    b.next().await;
    let (caller, at, locked_out) = locked(&seen).clone().unwrap();
    assert_eq!(caller.render, Some(crate::plugin::caller::Render::Slot));
    assert_eq!(caller.deadline, Some(at + CALL_LIMIT));
    assert!(locked_out);
}
