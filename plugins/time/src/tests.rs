use futures::executor::block_on;
use wisp_plugin_api::manifest::{Manifest, Origin};
use wisp_plugin_api::tools::BuildContext;
use wisp_plugin_testkit::hooks::before_turn;
use wisp_plugin_testkit::host::{FakeHost, OffsetFn};

use super::*;

const HOUR: i32 = 3_600;

fn instant(at: &str) -> Instant {
    let at: Timestamp = at.parse().unwrap();
    Instant {
        seconds: at.as_second(),
        nanos: u32::try_from(at.subsec_nanosecond()).unwrap(),
    }
}

fn sent_with(mode: &str, at: &str, offset: Arc<OffsetFn>) -> Reply {
    sent_on(Vec::new(), mode, at, offset)
}

fn sent_on(capabilities: Vec<String>, mode: &str, at: &str, offset: Arc<OffsetFn>) -> Reply {
    let at = instant(at);
    let host = FakeHost {
        capabilities,
        settings: vec![(SETTING.to_owned(), mode.to_owned())],
        now: Some(Arc::new(move || at)),
        offset: Some(offset),
        ..FakeHost::default()
    };
    block_on(instantiate(Arc::new(host)).on_hook(HOOK, Event::BeforeTurn(before_turn(0, "hi"))))
}

fn fixed(seconds: i32) -> Arc<OffsetFn> {
    Arc::new(move |_| Some(seconds))
}

fn local(at: &str, offset: i32) -> String {
    match sent_with("local", at, fixed(offset)) {
        Reply::Append(lines) => lines.concat(),
        other => panic!("{other:?}"),
    }
}

fn new_york() -> Arc<OffsetFn> {
    let switch = instant("2026-11-01T06:00:00Z");
    Arc::new(move |at| Some(if at < switch { -4 * HOUR } else { -5 * HOUR }))
}

#[test]
fn local_negative_offset() {
    assert_eq!(
        local("2026-10-22T03:58:59Z", -4 * HOUR),
        "Sent: Wednesday 2026-10-21 23:58:59 UTC-04:00\n"
    );
}

#[test]
fn local_positive_and_half_hour_offsets() {
    assert_eq!(
        local("2026-10-22T03:58:00Z", 2 * HOUR),
        "Sent: Thursday 2026-10-22 05:58:00 UTC+02:00\n"
    );
    assert_eq!(
        local("2026-10-22T03:58:00Z", 5 * HOUR + 30 * 60),
        "Sent: Thursday 2026-10-22 09:28:00 UTC+05:30\n"
    );
    assert_eq!(
        local("2026-10-22T03:58:00Z", -(3 * HOUR + 30 * 60)),
        "Sent: Thursday 2026-10-22 00:28:00 UTC-03:30\n"
    );
}

#[test]
fn the_offset_comes_from_the_instant_across_daylight_saving() {
    assert_eq!(
        sent_with("local", "2026-11-01T05:59:00Z", new_york()),
        Reply::Append(vec![
            "Sent: Sunday 2026-11-01 01:59:00 UTC-04:00\n".to_owned()
        ])
    );
    assert_eq!(
        sent_with("local", "2026-11-01T06:01:00Z", new_york()),
        Reply::Append(vec![
            "Sent: Sunday 2026-11-01 01:01:00 UTC-05:00\n".to_owned()
        ])
    );
}

#[test]
fn seconds_are_truncated_not_rounded() {
    assert_eq!(
        local("2026-10-22T03:58:59.999Z", -4 * HOUR),
        "Sent: Wednesday 2026-10-21 23:58:59 UTC-04:00\n"
    );
}

#[test]
fn the_manifest_reserve_is_the_epoch_stamp() {
    let manifest = Manifest::parse(MANIFEST, Origin::Bundled).unwrap();
    let [hook] = &manifest.hooks[..] else {
        panic!("{:?}", manifest.hooks)
    };
    let epoch = stamp(
        Instant {
            seconds: 0,
            nanos: 0,
        },
        0,
    );
    assert_eq!(hook.reserve, epoch);
    assert_eq!(
        hook.reserve.as_deref(),
        Some("Sent: Thursday 1970-01-01 00:00:00 UTC+00:00\n")
    );
}

#[test]
fn utc_ignores_the_local_offset_and_off_sends_nothing() {
    assert_eq!(
        sent_with("utc", "2026-10-22T03:58:00Z", new_york()),
        Reply::Append(vec![
            "Sent: Thursday 2026-10-22 03:58:00 UTC+00:00\n".to_owned()
        ])
    );
    assert_eq!(
        sent_with("off", "2026-10-22T03:58:00Z", new_york()),
        Reply::None
    );
}

#[test]
fn with_the_data_block_the_time_is_one_iso_line() {
    let data = || vec![DATA_BLOCK.to_owned()];
    let line = |text: &str| Reply::Append(vec![text.to_owned()]);
    assert_eq!(
        sent_on(data(), "local", "2026-11-23T23:53:41.9Z", fixed(8 * HOUR)),
        line("The current local time is: 2026-11-24T07:53:41+08:00.\n")
    );
    assert_eq!(
        sent_on(
            data(),
            "local",
            "2026-10-22T03:58:00Z",
            fixed(-(3 * HOUR + 30 * 60))
        ),
        line("The current local time is: 2026-10-22T00:28:00-03:30.\n")
    );
    assert_eq!(
        sent_on(data(), "utc", "2026-11-23T23:53:41Z", fixed(8 * HOUR)),
        line("The current local time is: 2026-11-23T23:53:41+00:00.\n")
    );
    assert_eq!(
        sent_on(data(), "off", "2026-11-23T23:53:41Z", fixed(8 * HOUR)),
        Reply::None
    );
    assert_eq!(
        sent_on(Vec::new(), "local", "2026-11-23T23:53:41Z", fixed(8 * HOUR)),
        line("Sent: Tuesday 2026-11-24 07:53:41 UTC+08:00\n")
    );
}

#[test]
fn an_unknown_local_offset_falls_back_to_utc() {
    assert_eq!(
        sent_with("local", "2026-10-22T03:58:00Z", Arc::new(|_| None)),
        Reply::Append(vec![
            "Sent: Thursday 2026-10-22 03:58:00 UTC+00:00\n".to_owned()
        ])
    );
}

#[test]
fn it_answers_only_its_hook_at_before_turn() {
    let plugin = instantiate(Arc::new(FakeHost::default()));
    assert_eq!(
        block_on(plugin.on_hook("other", Event::BeforeTurn(before_turn(0, "hi")))),
        Reply::None
    );
    assert_eq!(
        block_on(plugin.on_hook(HOOK, Event::SessionEnd)),
        Reply::None
    );
    let context = BuildContext {
        platform: "unix".to_owned(),
        capabilities: Vec::new(),
    };
    assert!(block_on(plugin.describe(&context)).is_empty());
}
