use super::*;

fn text(text: &str) -> SlotSpan {
    SlotSpan::Text {
        text: text.into(),
        tone: SlotTone::Text,
        bold: false,
    }
}

fn line(text_: &str) -> SlotBlock {
    SlotBlock::Line {
        spans: vec![text(text_)],
    }
}

fn content(blocks: Vec<SlotBlock>) -> SlotContent {
    SlotContent {
        note: Vec::new(),
        blocks,
    }
}

fn size(content: &SlotContent) -> u64 {
    serde_json::to_vec(content).unwrap().len() as u64
}

fn of_size(bytes: u64) -> SlotContent {
    let base = size(&content(vec![line("")]));
    let pad = usize::try_from(bytes - base).unwrap();
    let padded = content(vec![line(&"x".repeat(pad))]);
    assert_eq!(size(&padded), bytes);
    padded
}

fn lines(count: usize) -> SlotContent {
    content(vec![SlotBlock::Line { spans: Vec::new() }; count])
}

fn node(depth: u8) -> SlotNode {
    SlotNode {
        depth,
        state: NodeState::Done,
        role: None,
        label: String::new(),
        facts: Vec::new(),
    }
}

fn tree(depths: &[u8]) -> SlotContent {
    content(vec![SlotBlock::Tree {
        nodes: depths.iter().copied().map(node).collect(),
    }])
}

fn gauge(value: u64, max: u64) -> SlotContent {
    content(vec![SlotBlock::Gauge {
        label: String::new(),
        value,
        max,
        tone: SlotTone::Text,
        note: Vec::new(),
    }])
}

fn spark(values: Vec<u64>, max: u64) -> SlotContent {
    content(vec![SlotBlock::Spark {
        label: String::new(),
        values,
        max,
        tone: SlotTone::Text,
        note: Vec::new(),
    }])
}

fn fill(parts: Vec<PartTokens>, cached: Option<u8>) -> SlotContent {
    content(vec![SlotBlock::Fill {
        fill: SlotFill {
            used: 1,
            estimated: false,
            window: Some(10),
            limit: Some(8),
            auto_compact: true,
            parts,
            cached,
        },
    }])
}

fn tokens(part: FillPart) -> PartTokens {
    PartTokens { part, tokens: 1 }
}

#[test]
fn the_panel_takes_exactly_16384_bytes_and_200_items() {
    assert_eq!(check(&of_size(16_384), SlotView::Panel), Ok(()));
    assert_eq!(
        check(&of_size(16_385), SlotView::Panel),
        Err(SlotInvalid::TooLarge { bytes: 16_385 })
    );
    assert_eq!(check(&lines(200), SlotView::Panel), Ok(()));
    assert_eq!(
        check(&lines(201), SlotView::Panel),
        Err(SlotInvalid::Rule(TOO_MANY_ITEMS))
    );
}

#[test]
fn a_page_takes_exactly_65536_bytes_and_1000_items() {
    assert_eq!(check(&of_size(16_385), SlotView::Page), Ok(()));
    assert_eq!(check(&of_size(65_536), SlotView::Page), Ok(()));
    assert_eq!(
        check(&of_size(65_537), SlotView::Page),
        Err(SlotInvalid::TooLarge { bytes: 65_537 })
    );
    assert_eq!(check(&lines(201), SlotView::Page), Ok(()));
    assert_eq!(check(&lines(1_000), SlotView::Page), Ok(()));
    assert_eq!(
        check(&lines(1_001), SlotView::Page),
        Err(SlotInvalid::Rule(TOO_MANY_ITEMS))
    );
}

#[test]
fn an_unknown_view_gets_the_panel_limits() {
    assert_eq!(
        check(&lines(201), SlotView::Unknown),
        Err(SlotInvalid::Rule(TOO_MANY_ITEMS))
    );
}

#[test]
fn each_line_row_list_item_node_gauge_spark_fill_and_column_counts_as_one_item() {
    let mixed = |rows: usize| {
        let mut blocks = gauge(1, 1).blocks;
        blocks.extend(spark(vec![1], 1).blocks);
        blocks.push(SlotBlock::Line { spans: Vec::new() });
        blocks.push(SlotBlock::NextColumn);
        blocks.extend(fill(Vec::new(), None).blocks);
        blocks.push(SlotBlock::Rows {
            rows: vec![
                SlotRow {
                    key: String::new(),
                    value: Vec::new(),
                };
                rows
            ],
        });
        blocks.push(SlotBlock::List {
            items: vec![Vec::new(); 65],
        });
        blocks.extend(tree(&[0; 65]).blocks);
        content(blocks)
    };
    assert_eq!(check(&mixed(65), SlotView::Panel), Ok(()));
    assert_eq!(
        check(&mixed(66), SlotView::Panel),
        Err(SlotInvalid::Rule(TOO_MANY_ITEMS))
    );
}

#[test]
fn a_tree_starts_at_depth_0_steps_down_one_level_at_most_and_stops_at_8() {
    assert_eq!(check(&tree(&[]), SlotView::Panel), Ok(()));
    assert_eq!(
        check(&tree(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 1, 0]), SlotView::Panel),
        Ok(())
    );
    for depths in [
        &[1][..],
        &[0, 2],
        &[0, 1, 3],
        &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
    ] {
        assert_eq!(
            check(&tree(depths), SlotView::Panel),
            Err(SlotInvalid::Rule(TREE_DEPTH)),
            "{depths:?}"
        );
    }
}

fn topology(nodes: &[(&str, Option<&str>)]) -> SlotContent {
    content(vec![SlotBlock::Topology {
        root: Some("this chat".into()),
        nodes: nodes
            .iter()
            .map(|(key, parent)| SlotTopologyNode {
                key: (*key).into(),
                parent: parent.map(str::to_owned),
                state: NodeState::Working,
            })
            .collect(),
        flows: Vec::new(),
        counted: None,
    }])
}

#[test]
fn a_topology_names_each_key_once_and_each_parent_before_its_children() {
    let ok = [
        ("a", None),
        ("a/b", Some("a")),
        ("c", None),
        ("a/d", Some("a")),
    ];
    assert_eq!(check(&topology(&ok), SlotView::Panel), Ok(()));
    assert_eq!(check(&topology(&[]), SlotView::Panel), Ok(()));
    for broken in [
        &[("a", None), ("a", None)][..],
        &[("a", None), ("b", Some("x"))],
        &[("a/b", Some("a")), ("a", None)],
        &[("a", Some("a"))],
    ] {
        assert_eq!(
            check(&topology(broken), SlotView::Panel),
            Err(SlotInvalid::Rule(TOPOLOGY_KEYS)),
            "{broken:?}"
        );
    }
}

fn flowing(ends: &[(Option<&str>, Option<&str>)]) -> SlotContent {
    let mut flowing = topology(&[("a", None), ("a/b", Some("a"))]);
    if let SlotBlock::Topology { flows, .. } = &mut flowing.blocks[0] {
        *flows = (ends.iter())
            .map(|(from, to)| SlotFlow {
                from: from.map(str::to_owned),
                to: to.map(str::to_owned),
                at_ms: 1,
            })
            .collect();
    }
    flowing
}

#[test]
fn a_flow_runs_between_keys_the_topology_names_or_the_root() {
    for ok in [
        &[][..],
        &[(Some("a/b"), None)],
        &[(None, Some("a")), (Some("a"), Some("a/b"))],
    ] {
        assert_eq!(check(&flowing(ok), SlotView::Panel), Ok(()), "{ok:?}");
    }
    for broken in [&[(Some("x"), None)][..], &[(Some("a"), Some("gone"))]] {
        assert_eq!(
            check(&flowing(broken), SlotView::Panel),
            Err(SlotInvalid::Rule(TOPOLOGY_KEYS)),
            "{broken:?}"
        );
    }
}

#[test]
fn a_topology_takes_the_page_limits_in_the_panel_and_counts_its_flows_as_items() {
    let keys: Vec<String> = (0..1000)
        .map(|i| format!("/agent-viz/coordinator/worker-{i:03}"))
        .collect();
    let nodes = |count: usize| -> Vec<(&str, Option<&str>)> {
        keys[..count].iter().map(|k| (k.as_str(), None)).collect()
    };
    let busy = topology(&nodes(300));
    assert!(size(&busy) > PANEL_BYTES, "{}", size(&busy));
    assert_eq!(check(&busy, SlotView::Panel), Ok(()));
    let list = content(vec![SlotBlock::List {
        items: vec![Vec::new(); 300],
    }]);
    assert_eq!(
        check(&list, SlotView::Panel),
        Err(SlotInvalid::Rule(TOO_MANY_ITEMS))
    );
    let mut full = topology(&nodes(1000));
    assert_eq!(check(&full, SlotView::Panel), Ok(()));
    if let SlotBlock::Topology { flows, .. } = &mut full.blocks[0] {
        flows.push(SlotFlow {
            from: None,
            to: None,
            at_ms: 1,
        });
    }
    assert_eq!(
        check(&full, SlotView::Panel),
        Err(SlotInvalid::Rule(TOO_MANY_ITEMS))
    );
}

#[test]
fn a_gauge_needs_a_max_above_0_and_a_value_within_it() {
    assert_eq!(check(&gauge(5, 5), SlotView::Panel), Ok(()));
    assert_eq!(check(&gauge(0, 1), SlotView::Panel), Ok(()));
    for (value, max) in [(0, 0), (6, 5)] {
        assert_eq!(
            check(&gauge(value, max), SlotView::Panel),
            Err(SlotInvalid::Rule(GAUGE_RANGE))
        );
    }
}

#[test]
fn a_spark_needs_a_max_above_0_and_at_most_120_values_within_it() {
    assert_eq!(check(&spark(vec![5; 120], 5), SlotView::Panel), Ok(()));
    assert_eq!(check(&spark(Vec::new(), 1), SlotView::Panel), Ok(()));
    for (values, max) in [(vec![0], 0), (vec![5; 121], 5), (vec![1, 6, 2], 5)] {
        assert_eq!(
            check(&spark(values, max), SlotView::Panel),
            Err(SlotInvalid::Rule(SPARK_RANGE))
        );
    }
}

#[test]
fn a_fill_caches_at_most_100_percent_and_names_each_part_once() {
    let parts = vec![
        tokens(FillPart::System),
        tokens(FillPart::Chat),
        tokens(FillPart::Unknown),
        tokens(FillPart::Unknown),
    ];
    assert_eq!(check(&fill(parts, Some(100)), SlotView::Panel), Ok(()));
    for (parts, cached) in [
        (Vec::new(), Some(101)),
        (vec![tokens(FillPart::Chat), tokens(FillPart::Chat)], None),
    ] {
        assert_eq!(
            check(&fill(parts, cached), SlotView::Panel),
            Err(SlotInvalid::Rule(FILL_RANGE))
        );
    }
}

#[test]
fn the_size_limit_is_checked_before_the_rules() {
    let mut big = of_size(16_385);
    big.blocks.extend(gauge(0, 0).blocks);
    assert!(matches!(
        check(&big, SlotView::Panel),
        Err(SlotInvalid::TooLarge { .. })
    ));
}

#[test]
fn every_open_enum_reads_a_newer_value_as_unknown() {
    let block: SlotBlock = serde_json::from_str(r#"{"type":"table","cells":[]}"#).unwrap();
    assert_eq!(block, SlotBlock::Unknown);
    let span: SlotSpan = serde_json::from_str(r#"{"type":"link","url":"x"}"#).unwrap();
    assert_eq!(span, SlotSpan::Unknown);
    let tone: SlotTone = serde_json::from_str(r#""warn""#).unwrap();
    assert_eq!(tone, SlotTone::Unknown);
    let state: NodeState = serde_json::from_str(r#""paused""#).unwrap();
    assert_eq!(state, NodeState::Unknown);
    let place: SlotPlace = serde_json::from_str(r#""footer""#).unwrap();
    assert_eq!(place, SlotPlace::Unknown);
    let view: SlotView = serde_json::from_str(r#""zoom""#).unwrap();
    assert_eq!(view, SlotView::Unknown);
    let failure: SlotFailure = serde_json::from_str(r#"{"type":"crashed","code":3}"#).unwrap();
    assert_eq!(failure, SlotFailure::Unknown);
    let disabled: SlotDisabled = serde_json::from_str(r#"{"type":"replaced"}"#).unwrap();
    assert_eq!(disabled, SlotDisabled::Unknown);
}

#[test]
fn an_unknown_view_in_an_event_reads_as_unknown_and_a_panel_is_left_out() {
    let event: crate::AgentEvent = serde_json::from_str(
        r#"{"type":"slot_content","section":"a/b","view":"zoom","content":{"blocks":[]}}"#,
    )
    .unwrap();
    assert!(matches!(
        event,
        crate::AgentEvent::SlotContent {
            view: SlotView::Unknown,
            ..
        }
    ));
    let panel = crate::AgentEvent::SlotContent {
        section: "a/b".into(),
        view: SlotView::Panel,
        content: SlotContent::default(),
    };
    assert_eq!(
        serde_json::to_string(&panel).unwrap(),
        r#"{"type":"slot_content","section":"a/b","content":{"blocks":[]}}"#
    );
}

#[test]
fn equal_content_is_equal_bytes_and_defaults_are_left_out() {
    let build = || {
        content(vec![
            line("a"),
            SlotBlock::Tree {
                nodes: vec![node(0)],
            },
            SlotBlock::NextColumn,
        ])
    };
    let first = serde_json::to_vec(&build()).unwrap();
    assert_eq!(first, serde_json::to_vec(&build()).unwrap());
    let back: SlotContent = serde_json::from_slice(&first).unwrap();
    assert_eq!(serde_json::to_vec(&back).unwrap(), first);
    assert_eq!(
        String::from_utf8(first).unwrap(),
        r#"{"blocks":[{"type":"line","spans":[{"type":"text","text":"a"}]},{"type":"tree","nodes":[{"depth":0,"state":"done","label":""}]},{"type":"next_column"}]}"#
    );
}
