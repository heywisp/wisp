use super::*;

const LOW_NATIVE: &str = "[models.\"claude-opus-5-5\"]\ncompact_at = 2000\n";
const LOW_WISP: &str =
    "[models.\"claude-opus-5-5\"]\ncompact_at = 2000\nnative_compaction = false\n";
const AT_TRIGGER: &str =
    "[models.\"claude-opus-5-5\"]\ncompact_at = 437\nnative_compaction = false\n";
const UNDER_TRIGGER: &str =
    "[models.\"claude-opus-5-5\"]\ncompact_at = 438\nnative_compaction = false\n";

fn on(config: &'static str) -> Setup {
    Setup {
        models: Some(config),
        ..Setup::default()
    }
}

fn compactions(seen: &[AgentEvent]) -> usize {
    seen.iter()
        .filter(|e| matches!(e, AgentEvent::Compacted { .. }))
        .count()
}

async fn two_turns(rig: &Rig) -> Vec<AgentEvent> {
    let mut live = rig.start();
    live.completed("u1", "hello").await;
    live.completed("u2", "again").await;
    let (_, seen) = live.close().await;
    rig.check();
    seen
}

#[tokio::test]
async fn an_automatic_compaction_with_wisps_engine() {
    let script = vec![
        says("msg_01", 5_000, "Hello."),
        greeted("msg_02", 5_000),
        says("msg_03", 600, "Again."),
    ];
    let rig = Rig::new("compact_auto_wisp", on(LOW_WISP), script).await;
    let seen = two_turns(&rig).await;
    assert_eq!(compactions(&seen), 1, "{seen:#?}");
}

#[tokio::test]
async fn an_automatic_compaction_with_the_native_engine() {
    let script = vec![
        says("msg_01", 5_000, "Hello."),
        reply(
            "msg_02",
            5_000,
            vec![Part::Compaction("So far: a greeting.")],
            "compaction",
        ),
        says("msg_03", 600, "Again."),
    ];
    let rig = Rig::new("compact_auto_native", on(LOW_NATIVE), script).await;
    let seen = two_turns(&rig).await;
    assert_eq!(compactions(&seen), 1, "{seen:#?}");
}

#[tokio::test]
async fn an_estimate_exactly_at_the_trigger_compacts() {
    let script = vec![
        says("msg_01", 400, "Hello."),
        greeted("msg_02", 400),
        says("msg_03", 300, "Again."),
    ];
    let rig = Rig::new("compact_at_trigger", on(AT_TRIGGER), script).await;
    let seen = two_turns(&rig).await;
    assert_eq!(compactions(&seen), 1, "{seen:#?}");
}

#[tokio::test]
async fn an_estimate_one_token_under_the_trigger_does_not_compact() {
    let script = vec![says("msg_01", 400, "Hello."), says("msg_02", 520, "Again.")];
    let rig = Rig::new("compact_under_trigger", on(UNDER_TRIGGER), script).await;
    let seen = two_turns(&rig).await;
    assert_eq!(compactions(&seen), 0, "{seen:#?}");
}

fn change(change: wisp_protocol::settings::SettingChange) -> ClientCommand {
    ClientCommand::ChangeSettings {
        request: RequestId(90),
        changes: vec![change],
        save: false,
    }
}

#[tokio::test]
async fn a_time_change_queued_for_the_turn_leaves_the_head_of_its_decision() {
    let off = change(
        wisp_protocol::settings::SettingChange::named(
            "time",
            wisp_protocol::settings::SettingValue::Text("off".into()),
        )
        .unwrap(),
    );
    let script = vec![
        says("msg_01", 400, "Hello.").then(off),
        greeted("msg_02", 400),
        says("msg_03", 300, "Again."),
    ];
    let rig = Rig::new("compact_at_trigger", on(AT_TRIGGER), script).await;
    let mut live = rig.start();
    live.completed("u1", "hello").await;
    live.completed("u2", "again").await;
    let (_, seen) = live.close().await;
    assert_eq!(compactions(&seen), 1, "{seen:#?}");
}

fn reserving() -> (String, crate::plugin::Make) {
    let body = "\n[[hooks]]\nname = \"x\"\npoint = \"before-turn\"\nreserve = \"RESERVED\\n\"\n";
    let make: crate::plugin::Make = wisp_plugin_testkit::hooks::instantiate(
        wisp_plugin_testkit::hooks::none(),
        wisp_plugin_testkit::hooks::Calls::default(),
    );
    (wisp_plugin_testkit::hooks::manifest("aa", body), make)
}

#[tokio::test]
async fn a_hook_switched_off_between_turns_drops_its_reserve_from_the_next_decision() {
    use wisp_protocol::settings::{
        PLUGIN_HOOK, Place, SettingChange, SettingTarget, SettingValue, SitePath, find,
    };
    let setup = Setup {
        hooks: Some(reserving),
        ..on(UNDER_TRIGGER)
    };
    let script = vec![says("msg_01", 400, "Hello."), says("msg_02", 520, "Again.")];
    let rig = Rig::new("compact_under_trigger", setup, script).await;
    let mut live = rig.start();
    live.completed("u1", "hello").await;
    let path = SitePath::new(
        Place::PluginHook {
            plugin: "aa".into(),
            hook: "x".into(),
        },
        find(PLUGIN_HOOK).unwrap(),
    );
    live.send(change(SettingChange {
        target: SettingTarget::Path(path),
        value: Some(SettingValue::Bool(false)),
    }))
    .await;
    live.until(|e| matches!(e, AgentEvent::SettingsChanged { .. }))
        .await;
    live.completed("u2", "again").await;
    let (_, seen) = live.close().await;
    assert_eq!(compactions(&seen), 0, "{seen:#?}");
}

static IDLE_KNOB: std::sync::LazyLock<wisp_plugin_testkit::hooks::Knob> =
    std::sync::LazyLock::new(|| wisp_plugin_testkit::hooks::knob("v"));

fn idle_notes() -> (String, crate::plugin::Make) {
    use wisp_plugin_testkit::hooks as kit;
    let body = kit::prompt_section("notes", "lead", "");
    let make: crate::plugin::Make = kit::prompted(
        kit::none(),
        kit::changing(IDLE_KNOB.clone()),
        kit::Calls::default(),
    );
    (kit::manifest_in("0.2.0", "bb", &body), make)
}

#[tokio::test]
async fn an_unchanged_section_leaves_the_under_trigger_decision() {
    let setup = Setup {
        hooks: Some(idle_notes),
        ..on(UNDER_TRIGGER)
    };
    let script = vec![says("msg_01", 400, "Hello."), says("msg_02", 520, "Again.")];
    let rig = Rig::new("compact_under_trigger", setup, script).await;
    let mut live = rig.start();
    live.completed("u1", "hello").await;
    live.completed("u2", "again").await;
    let (_, seen) = live.close().await;
    assert_eq!(compactions(&seen), 0, "{seen:#?}");
}

#[tokio::test]
async fn a_section_edited_while_idle_counts_in_the_pre_turn_decision() {
    let setup = Setup {
        hooks: Some(idle_notes),
        ..on(UNDER_TRIGGER)
    };
    let script = vec![
        says("msg_01", 400, "Hello."),
        greeted("msg_02", 400),
        says("msg_03", 300, "Again."),
    ];
    let rig = Rig::new("compact_under_trigger", setup, script).await;
    let mut live = rig.start();
    live.completed("u1", "hello").await;
    wisp_plugin_testkit::hooks::turn_to(&IDLE_KNOB, "w");
    live.completed("u2", "again").await;
    let (_, seen) = live.close().await;
    assert_eq!(compactions(&seen), 1, "{seen:#?}");
    let bodies = rig.server.seen().bodies;
    assert!(
        bodies[2].contains("<notes>\\nw\\n</notes>"),
        "{}",
        bodies[2]
    );
}
