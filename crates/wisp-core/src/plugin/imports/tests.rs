use std::collections::BTreeSet;
use std::path::Path;

use wit_parser::{Resolve, WorldItem, WorldKey};

use super::*;

const WIT_0_1_0: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../wit/v0.1.0");

const WIT_0_2_0: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../wit/v0.2.0");

const WIT_0_3_0: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../wit/v0.3.0");

const WIT_0_4_0: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../wit/v0.4.0");

fn world_imports() -> BTreeSet<String> {
    imports_of(&[WIT_0_1_0, WIT_0_2_0, WIT_0_3_0, WIT_0_4_0])
}

fn imports_of(dirs: &[&str]) -> BTreeSet<String> {
    let mut resolve = Resolve::new();
    let mut package = None;
    for dir in dirs {
        package = Some(resolve.push_dir(Path::new(dir)).unwrap().0);
    }
    let package = package.unwrap();
    let world = &resolve.worlds[resolve.packages[package].worlds["plugin"]];
    let mut out = BTreeSet::new();
    for (key, item) in &world.imports {
        let WorldItem::Interface { id, .. } = item else {
            panic!(
                "the world imports a bare item: {}",
                resolve.name_world_key(key)
            );
        };
        let interface = &resolve.interfaces[*id];
        let name = match key {
            WorldKey::Interface(_) => interface.name.clone().unwrap(),
            WorldKey::Name(name) => name.clone(),
        };
        for function in interface.functions.values() {
            let owner = function
                .kind
                .resource()
                .map(|r| resolve.types[r].name.clone().unwrap());
            let full = match owner {
                Some(owner) => format!("{name}.{owner}.{}", function.item_name()),
                None => format!("{name}.{}", function.item_name()),
            };
            out.insert(full);
        }
    }
    out
}

#[test]
fn the_table_has_a_row_for_every_import_of_the_world_resource_methods_included() {
    let table: BTreeSet<String> = ALL.iter().map(|i| i.wit().to_owned()).collect();
    assert_eq!(table.len(), ALL.len(), "two rows name one import");
    let world = world_imports();
    let missing: Vec<&String> = world.difference(&table).collect();
    let extra: Vec<&String> = table.difference(&world).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "imports without a row: {missing:?}; rows without an import: {extra:?}"
    );
}

#[test]
fn a_slot_render_may_call_only_the_reads_that_answer_at_once() {
    let allowed: BTreeSet<&str> = ALL
        .iter()
        .filter(|i| i.inside(Render::Slot) == Inside::Allowed)
        .map(|i| i.wit())
        .collect();
    let expected: BTreeSet<&str> = [
        "session.current",
        "workspace.root",
        "workspace.home",
        "workspace.shorten",
        "workspace.kind-of",
        "clock.now",
        "clock.local-offset",
        "clock.elapsed-ms",
        "settings.get",
        "context.current-fill",
        "context.estimate-text",
        "media.media-types",
        "media.media-slices",
        "media.image-edge",
        "jobs.which",
        "pricing.estimate",
        "pricing.spend",
        "agent.current",
        "agent-tree.snapshot",
        "agent-states.states",
        "usage.accounts",
    ]
    .into_iter()
    .collect();
    assert_eq!(allowed, expected);
    let quiet: Vec<&str> = ALL
        .iter()
        .filter(|i| i.inside(Render::Slot) == Inside::Quiet)
        .map(|i| i.wit())
        .collect();
    assert_eq!(quiet, ["jobs.job.info", "jobs.all"]);
    for import in ALL {
        let needs_call = import.wit().starts_with("call.")
            || import.wit().starts_with("agents.")
            || import.wit().starts_with("agent-rename.")
            || import.wit().starts_with("secrets.")
            || import.wit().starts_with("config.")
            || import.wit().starts_with("accounts.")
            || import.wit().starts_with("questions.")
            || matches!(
                import,
                Import::JobsSpawn
                    | Import::JobsAdopt
                    | Import::JobsMarkReported
                    | Import::TodosReplace
                    | Import::ChatRename
                    | Import::ManageManage
            );
        assert_eq!(
            import.inside(Render::Slot) == Inside::NeedsCall,
            needs_call,
            "{}",
            import.wit()
        );
    }
}

#[test]
fn a_start_render_may_call_only_what_the_instructions_plugin_reads_at_start() {
    let allowed: BTreeSet<&str> = ALL
        .iter()
        .filter(|i| i.inside(Render::Start) == Inside::Allowed)
        .map(|i| i.wit())
        .collect();
    let expected: BTreeSet<&str> = [
        "workspace.root",
        "workspace.home",
        "workspace.shorten",
        "workspace.kind-of",
        "settings.get",
        "context.estimate-text",
        "log.write",
        "workspace-facts.config-dir",
        "instructions-report.loaded",
        "instructions-report.warning",
        "instructions-report.changed",
    ]
    .into_iter()
    .collect();
    assert_eq!(allowed, expected);
    for import in ALL {
        let slot = import.inside(Render::Slot);
        let start = import.inside(Render::Start);
        let wanted = match slot {
            Inside::NeedsCall => Inside::NeedsCall,
            _ if allowed.contains(import.wit()) => Inside::Allowed,
            _ => Inside::Unavailable,
        };
        assert_eq!(start, wanted, "{}", import.wit());
    }
}

#[test]
fn the_world_column_is_the_first_world_whose_wit_declares_the_import() {
    let worlds = [
        ("0.1.0", imports_of(&[WIT_0_1_0])),
        ("0.2.0", imports_of(&[WIT_0_1_0, WIT_0_2_0])),
        ("0.3.0", imports_of(&[WIT_0_1_0, WIT_0_2_0, WIT_0_3_0])),
        ("0.4.0", world_imports()),
    ];
    for import in ALL {
        let first = worlds.iter().position(|(_, w)| w.contains(import.wit()));
        let first = first.unwrap();
        assert_eq!(import.world(), worlds[first].0, "{}", import.wit());
        for (at, (world, _)) in worlds.iter().enumerate() {
            assert_eq!(
                import.spoken_in(world),
                at >= first,
                "{} in {world}",
                import.wit()
            );
        }
        assert!(!import.spoken_in("9.9.9"), "{}", import.wit());
    }
}

#[tokio::test]
async fn refused_reads_the_render_flag_of_the_running_task() {
    assert!(!Import::ClockSleep.refused());
    let inside = Caller::rendering(Render::Slot)
        .scope(async { (Import::ClockSleep.refused(), Import::ClockNow.refused()) })
        .await;
    assert_eq!(inside, (true, false));
}

#[test]
fn the_agent_rows_agree_with_sees_agents_in_every_render() {
    for render in [Render::Slot, Render::Start] {
        for cancel in [None, Some(tokio_util::sync::CancellationToken::new())] {
            let caller = Caller {
                cancel,
                ..Caller::rendering(render)
            };
            for import in [
                Import::AgentCurrent,
                Import::AgentTreeSnapshot,
                Import::AgentStatesStates,
                Import::UsageAccounts,
            ] {
                assert_eq!(
                    import.inside(render) == Inside::Allowed,
                    caller.sees_agents(),
                    "{} in {render:?}, token {}",
                    import.wit(),
                    caller.cancel.is_some()
                );
            }
        }
    }
}

#[test]
fn a_quiet_jobs_call_lists_jobs_quietly_and_leaves_every_other_import_allowed() {
    let quiet = Caller::call(tokio_util::sync::CancellationToken::new(), None).quiet_jobs();
    let rows = quiet.sync_scope(|| {
        ALL.iter()
            .map(|import| (*import, import.current()))
            .collect::<Vec<_>>()
    });
    for (import, inside) in rows {
        let expected = if matches!(import, Import::JobsAll | Import::JobInfo) {
            Inside::Quiet
        } else {
            Inside::Allowed
        };
        assert_eq!(inside, expected, "{}", import.wit());
    }
    assert_eq!(
        Caller::call(tokio_util::sync::CancellationToken::new(), None)
            .sync_scope(|| Import::JobsAll.current()),
        Inside::Allowed
    );
    let describing = Caller::describing().sync_scope(|| {
        ALL.iter()
            .map(|import| (*import, import.current()))
            .collect::<Vec<_>>()
    });
    for (import, inside) in describing {
        let expected = if import == Import::ProjectKey {
            Inside::Unavailable
        } else {
            Inside::Allowed
        };
        assert_eq!(inside, expected, "{}", import.wit());
    }
}
