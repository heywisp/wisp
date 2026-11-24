use futures::executor::block_on;
use wisp_plugin_testkit::hooks::before_turn;
use wisp_plugin_testkit::host::FakeHost;

use super::*;

const SECTION: &str = "window";

fn fill(estimate: u64, window: Option<u64>, point: Option<u64>, auto_compact: bool) -> Fill {
    Fill {
        estimate,
        measured: None,
        window,
        point,
        ceiling: None,
        auto_compact,
        epoch: 0,
        parts: Vec::new(),
        cached: None,
    }
}

fn hooked(fill: Fill) -> Reply {
    hooked_on(Vec::new(), fill)
}

fn hooked_on(capabilities: Vec<String>, fill: Fill) -> Reply {
    let host = FakeHost {
        capabilities,
        fill: Some(fill),
        ..FakeHost::default()
    };
    let event = Event::BeforeTurn(before_turn(0, ""));
    block_on(instantiate(Arc::new(host)).on_hook(HOOK, event))
}

#[test]
fn the_context_line_reads_the_window_and_turns_off_automatic_compaction() {
    assert_eq!(
        context_line(62_000, Some(1_000_000), None),
        "Context: 62k of 1M tokens (6%). Automatic compaction is off.\n"
    );
    assert_eq!(
        context_line(62_000, Some(400_000), None),
        "Context: 62k of 400k tokens (15%). Automatic compaction is off.\n"
    );
    assert_eq!(
        context_line(61_734, None, None),
        "Context: 62k tokens; window unknown.\n"
    );
    assert_eq!(
        context_line(900, Some(0), None),
        "Context: 900 tokens; window unknown.\n"
    );
    assert_eq!(
        context_line(1_300_000, Some(1_000_000), None),
        "Context: 1.3M of 1M tokens (130%). Automatic compaction is off.\n"
    );
    for line in [
        context_line(5, Some(200_000), None),
        context_line(5, None, None),
    ] {
        assert!(!line.contains("compacts at"), "{line}");
    }
}

#[test]
fn with_automatic_compaction_on_the_context_line_states_the_point() {
    assert_eq!(
        context_line(62_000, Some(1_000_000), Some(850_000)),
        "Context: 62k of 1M tokens (6%). Wisp compacts at 850k.\n"
    );
    assert_eq!(
        context_line(62_000, Some(200_000), Some(136_000)),
        "Context: 62k of 200k tokens (31%). Wisp compacts at 136k.\n"
    );
    assert_eq!(
        context_line(61_734, None, Some(850_000)),
        "Context: 62k tokens; window unknown.\n"
    );
}

#[test]
fn the_context_share_rounds_down_until_the_window_is_full() {
    assert_eq!(
        context_line(999_600, Some(1_000_000), None),
        "Context: 1M of 1M tokens (99%). Automatic compaction is off.\n"
    );
    assert_eq!(
        context_line(999_999, Some(1_000_000), None),
        "Context: 1M of 1M tokens (99%). Automatic compaction is off.\n"
    );
    assert_eq!(
        context_line(1_000_000, Some(1_000_000), None),
        "Context: 1M of 1M tokens (100%). Automatic compaction is off.\n"
    );
    assert_eq!(
        context_line(9_999, Some(1_000_000), None),
        "Context: 10k of 1M tokens (0%). Automatic compaction is off.\n"
    );
}

#[test]
fn the_hook_states_the_point_only_while_automatic_compaction_is_on() {
    assert_eq!(
        hooked(fill(62_000, Some(1_000_000), Some(850_000), true)),
        Reply::Append(vec![
            "Context: 62k of 1M tokens (6%). Wisp compacts at 850k.\n".to_owned()
        ])
    );
    assert_eq!(
        hooked(fill(62_000, Some(1_000_000), Some(850_000), false)),
        Reply::Append(vec![
            "Context: 62k of 1M tokens (6%). Automatic compaction is off.\n".to_owned()
        ])
    );
}

#[test]
fn with_the_data_block_the_hook_says_context_window() {
    let data = || vec![DATA_BLOCK.to_owned()];
    let line = |text: &str| Reply::Append(vec![text.to_owned()]);
    assert_eq!(
        hooked_on(data(), fill(211_000, Some(1_048_576), Some(256_000), true)),
        line("Context window: 211k of 1M tokens (20%); Wisp compacts at 256k.\n")
    );
    assert_eq!(
        hooked_on(data(), fill(211_000, Some(1_048_576), Some(256_000), false)),
        line("Context window: 211k of 1M tokens (20%); automatic compaction is off.\n")
    );
    assert_eq!(
        hooked_on(data(), fill(211_000, None, Some(256_000), true)),
        line("Context window: 211k tokens; window unknown.\n")
    );
    assert_eq!(
        hooked_on(
            Vec::new(),
            fill(211_000, Some(1_048_576), Some(256_000), true)
        ),
        line("Context: 211k of 1M tokens (20%). Wisp compacts at 256k.\n")
    );
}

#[test]
fn it_answers_only_its_hook_at_before_turn() {
    let plugin = instantiate(Arc::new(FakeHost::default()));
    assert_eq!(
        block_on(plugin.on_hook(HOOK, Event::SessionEnd)),
        Reply::None
    );
    assert_eq!(
        block_on(plugin.on_hook("other", Event::SessionEnd)),
        Reply::None
    );
}

#[test]
fn the_panel_and_the_page_get_the_current_fill() {
    use wisp_plugin_api::host::{FillPart, PartTokens};
    let current = Fill {
        measured: Some(159_000),
        ceiling: Some(850_000),
        parts: vec![
            PartTokens {
                part: FillPart::System,
                tokens: 12_000,
            },
            PartTokens {
                part: FillPart::Chat,
                tokens: 147_000,
            },
        ],
        cached: Some(96),
        ..fill(160_000, Some(1_000_000), Some(400_000), true)
    };
    let host = FakeHost {
        fill: Some(current.clone()),
        ..FakeHost::default()
    };
    let plugin = instantiate(Arc::new(host));
    let want = Content {
        note: vec![Span::Fill(current.clone())],
        blocks: vec![Block::Fill(current)],
    };
    for view in [SlotView::Panel, SlotView::Page] {
        assert_eq!(
            block_on(plugin.render_slot(SECTION, view)),
            Ok(want.clone())
        );
    }
}

#[test]
fn the_manifest_declares_the_section_off_with_its_marks() {
    use wisp_plugin_api::manifest::{Manifest, Origin, Switch};
    let manifest = Manifest::parse(MANIFEST, Origin::Bundled).unwrap();
    assert_eq!(manifest.world, "0.2.0");
    let [slot] = manifest.slots.as_slice() else {
        panic!("{:?}", manifest.slots)
    };
    assert_eq!(
        (slot.name.as_str(), slot.title.as_str()),
        (SECTION, "Context")
    );
    assert_eq!(slot.default, Switch::Off);
    assert_eq!(slot.refresh_s, None);
    assert_eq!(
        slot.marks,
        [
            "after-response",
            "after-compact",
            "rewound",
            "model-switched",
            "settings-changed"
        ]
    );
}
