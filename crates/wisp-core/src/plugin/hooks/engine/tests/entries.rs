use std::collections::BTreeSet;

use wisp_protocol::plugin::{HookDisabled, HookEntry};

use super::{hooks, parsed};
use crate::plugin::hooks::points::BEFORE_TURN;
use crate::plugin::hooks::{HookState, Name, Table};
use crate::plugin::testing::hook_body;

fn at(table: &Table, state: &HookState, point: &str) -> Vec<HookEntry> {
    let mut entries: Vec<HookEntry> = table
        .hook_entries(state)
        .into_values()
        .flatten()
        .filter(|e| e.point == point)
        .collect();
    entries.sort_by_key(|e| e.order);
    entries
}

fn names(entries: &[HookEntry]) -> Vec<String> {
    entries.iter().map(|e| e.hook.clone()).collect()
}

fn engine_hooks(table: &Table, point: &str) -> Vec<String> {
    table
        .order(point)
        .iter()
        .filter(|name| table.hook(name).is_some())
        .map(ToString::to_string)
        .collect()
}

fn orders(entries: &[HookEntry]) -> Vec<u32> {
    entries.iter().map(|e| e.order).collect()
}

#[test]
fn the_bundled_entries_follow_the_engines_order_counting_only_hooks() {
    let manifests = [
        parsed(wisp_plugin_context::MANIFEST),
        parsed(wisp_plugin_self_check::MANIFEST),
        parsed(wisp_plugin_shell::MANIFEST),
        parsed(wisp_plugin_time::MANIFEST),
        parsed(wisp_plugin_workspace::MANIFEST),
    ];
    let table = Table::build(&manifests);
    let state = HookState::default();
    let turn = at(&table, &state, BEFORE_TURN);
    assert_eq!(
        names(&turn),
        ["time/sent", "context/fill", "shell/processes"]
    );
    assert_eq!(names(&turn), engine_hooks(&table, BEFORE_TURN));
    assert_eq!(orders(&turn), [0, 1, 2]);
    assert!(turn.iter().all(|e| e.on && e.disabled.is_none()));
    let switches: Vec<Option<&str>> = turn.iter().map(|e| e.switch.as_deref()).collect();
    assert_eq!(switches, [Some("time"), Some("context.context_note"), None]);
    for point in ["after-response", "before-end"] {
        let entries = at(&table, &state, point);
        assert_eq!(orders(&entries), [0], "{point}");
        assert_eq!(entries[0].switch.as_deref(), Some("self_check.before_end"));
    }
    let by_plugin = table.hook_entries(&state);
    assert!(!by_plugin.contains_key("workspace"), "{by_plugin:?}");
}

#[test]
fn a_set_a_cycle_a_missing_requires_and_a_left_out_hook_each_take_their_place() {
    let aa = parsed(&hooks::manifest_in(
        "0.2.0",
        "aa",
        &[
            hook_body("first", BEFORE_TURN, "in = [\"core/state\"]\n"),
            hook_body("loop", BEFORE_TURN, "after = [\"bb/loop\"]\n"),
            hook_body("needs", BEFORE_TURN, "requires = [\"zz/gone\"]\n"),
            hook_body("plain", BEFORE_TURN, ""),
        ]
        .concat(),
    ));
    let bb = parsed(&hooks::manifest(
        "bb",
        &hook_body("loop", BEFORE_TURN, "after = [\"aa/loop\"]\n"),
    ));
    let gone = parsed(&hooks::manifest("cc", &hook_body("late", BEFORE_TURN, "")));
    let table = Table::build([&aa, &bb]);
    let state = HookState {
        missing: table.missing_without(&BTreeSet::new()),
        left_out: gone
            .hooks
            .iter()
            .map(|h| (Name::new("cc", &h.name), h.clone()))
            .collect(),
        ..HookState::default()
    };
    let entries = at(&table, &state, BEFORE_TURN);
    let placed = engine_hooks(&table, BEFORE_TURN);
    assert_eq!(placed, ["aa/first", "aa/needs", "aa/plain"]);
    assert_eq!(
        names(&entries),
        [
            "aa/first", "aa/needs", "aa/plain", "aa/loop", "bb/loop", "cc/late"
        ]
    );
    assert_eq!(orders(&entries), [0, 1, 2, 3, 4, 5]);
    let reasons: Vec<Option<HookDisabled>> = entries.iter().map(|e| e.disabled.clone()).collect();
    let cycle = vec![
        "aa/loop".to_owned(),
        "bb/loop".to_owned(),
        "aa/loop".to_owned(),
    ];
    assert_eq!(
        reasons,
        [
            None,
            Some(HookDisabled::Missing {
                requires: vec!["zz/gone".into()]
            }),
            None,
            Some(HookDisabled::Cycle {
                cycle: cycle.clone()
            }),
            Some(HookDisabled::Cycle { cycle }),
            Some(HookDisabled::LeftOut),
        ]
    );
    assert_eq!(
        entries.iter().map(|e| e.on).collect::<Vec<_>>(),
        [true, false, true, false, false, false]
    );
}

#[test]
fn a_plugin_off_its_dependents_missing_and_a_switch_off_read_in_that_order() {
    let aa = parsed(&hooks::manifest(
        "aa",
        &[
            hook_body("base", BEFORE_TURN, ""),
            hook_body("own", BEFORE_TURN, "requires = [\"zz/gone\"]\n"),
        ]
        .concat(),
    ));
    let bb = parsed(&hooks::manifest(
        "bb",
        &[
            hook_body("needs", BEFORE_TURN, "requires = [\"aa/base\"]\n"),
            hook_body("quiet", BEFORE_TURN, ""),
        ]
        .concat(),
    ));
    let table = Table::build([&aa, &bb]);
    let off: BTreeSet<String> = ["aa".to_owned()].into();
    let state = HookState {
        plugins_off: off.clone(),
        hooks_off: [Name::new("bb", "quiet"), Name::new("aa", "base")].into(),
        missing: table.missing_without(&off),
        left_out: Vec::new(),
    };
    let reason = |name: &str| {
        at(&table, &state, BEFORE_TURN)
            .into_iter()
            .find(|e| e.hook == name)
            .and_then(|e| e.disabled)
    };
    assert_eq!(reason("aa/base"), Some(HookDisabled::PluginOff));
    assert_eq!(
        reason("aa/own"),
        Some(HookDisabled::Missing {
            requires: vec!["zz/gone".into()]
        })
    );
    assert_eq!(
        reason("bb/needs"),
        Some(HookDisabled::Missing {
            requires: vec!["aa/base".into()]
        })
    );
    assert_eq!(reason("bb/quiet"), Some(HookDisabled::SwitchedOff));
}

#[test]
fn the_compaction_points_list_their_hooks_in_the_engines_order() {
    use crate::plugin::hooks::points::{BEFORE_COMPACT, COMPACTION_HEAD};
    let aa = parsed(&hooks::manifest(
        "aa",
        &[
            hook_body("zeta", BEFORE_COMPACT, ""),
            hook_body("alpha", BEFORE_COMPACT, "after = [\"aa/zeta\"]\n"),
            hook_body("head", COMPACTION_HEAD, ""),
        ]
        .concat(),
    ));
    let table = Table::build([&aa]);
    let state = HookState::default();
    let compact = at(&table, &state, BEFORE_COMPACT);
    assert_eq!(names(&compact), ["aa/zeta", "aa/alpha"]);
    assert_eq!(names(&compact), engine_hooks(&table, BEFORE_COMPACT));
    assert_eq!(orders(&compact), [0, 1]);
    let head = at(&table, &state, COMPACTION_HEAD);
    assert_eq!(names(&head), ["aa/head"]);
    assert!(head[0].on, "{head:?}");
}
