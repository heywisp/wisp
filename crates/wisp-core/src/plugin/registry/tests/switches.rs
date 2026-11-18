use wisp_plugin_testkit::hooks::{self as kit, Calls};

use super::*;

fn removing(ids: &[&str]) -> PluginConfig {
    PluginConfig {
        switches: PluginSwitches {
            off: BTreeSet::new(),
            removed: on(ids),
        },
        ..PluginConfig::default()
    }
}

fn hooked(id: &str, body: &str, calls: &Calls) -> (String, Make) {
    let make: Make = kit::instantiate(kit::none(), calls.clone());
    (kit::manifest(id, body), make)
}

fn hook(name: &str, extra: &str) -> String {
    format!("\n[[hooks]]\nname = \"{name}\"\npoint = \"before-turn\"\n{extra}")
}

fn entry<'a>(catalog: &'a Catalog, id: &str) -> &'a wisp_protocol::PluginEntry {
    catalog.plugins.iter().find(|p| p.id == id).unwrap()
}

#[test]
fn a_removed_plugin_is_never_instantiated_and_is_listed_not_installed() {
    let calls = Calls::default();
    let (registry, problems) = Registry::from_sources(
        vec![
            hooked("aa", &hook("x", ""), &calls),
            fixed("other", &["own"], vec![def("own", OBJECT)]),
        ],
        &removing(&["aa"]),
    );
    assert!(problems.is_empty(), "{problems:?}");
    assert!(registry.is_removed("aa") && registry.knows("aa"));
    let started = start(&registry, &["own"]);
    assert_eq!(calls.instances(), 0);
    assert!(!started.hooks.plugins.contains_key("aa"));
    let aa = entry(&started.catalog, "aa");
    assert_eq!((aa.installed, aa.enabled), (false, false));
    let other = entry(&started.catalog, "other");
    assert_eq!((other.installed, other.enabled), (true, true));
}

#[test]
fn a_plugin_off_at_start_stays_installed_and_its_tools_stay_out_of_the_session() {
    let config = PluginConfig {
        switches: PluginSwitches {
            off: on(&["alpha"]),
            removed: BTreeSet::new(),
        },
        ..PluginConfig::default()
    };
    let (registry, _) = Registry::from_sources(
        vec![
            fixed("alpha", &["a"], vec![def("a", OBJECT)]),
            fixed("beta", &["b"], vec![def("b", OBJECT)]),
        ],
        &config,
    );
    assert_eq!(registry.on_tools(&on(&["a", "b"])), on(&["b"]));
    let started = start(&registry, &["a", "b"]);
    assert_eq!(started.toolbox.names(), ["b"]);
    let alpha = entry(&started.catalog, "alpha");
    assert!(alpha.installed);
}

#[test]
fn a_plugin_left_out_at_start_is_listed_installed_with_its_problem() {
    let (registry, _) = Registry::from_sources(
        vec![fixed(
            "liar",
            &["told"],
            vec![def("told", OBJECT), def("extra", OBJECT)],
        )],
        &PluginConfig::default(),
    );
    let started = start(&registry, &["told"]);
    let liar = entry(&started.catalog, "liar");
    assert!(liar.installed);
    assert!(!liar.problems.is_empty());
}

#[test]
fn removing_one_of_two_clashing_plugins_gives_the_other_its_tool_in_the_next_session() {
    let sources = || {
        vec![
            fixed("alpha", &["dup"], vec![def("dup", OBJECT)]),
            fixed("beta", &["dup"], vec![def("dup", OBJECT)]),
        ]
    };
    let (both, problems) = Registry::from_sources(sources(), &PluginConfig::default());
    assert_eq!(problems.len(), 2);
    assert!(start(&both, &["dup"]).toolbox.names().is_empty());
    let (one, problems) = Registry::from_sources(sources(), &removing(&["beta"]));
    assert!(problems.is_empty(), "{problems:?}");
    let started = start(&one, &["dup"]);
    assert_eq!(started.toolbox.names(), ["dup"]);
    assert_eq!(
        started
            .toolbox
            .plugins()
            .iter()
            .map(|r| r.id.as_str())
            .collect::<Vec<_>>(),
        ["alpha"]
    );
}

#[test]
fn adding_back_a_plugin_that_would_close_a_cycle_is_refused_with_the_cycle() {
    let calls = Calls::default();
    let (registry, problems) = Registry::from_sources(
        vec![
            hooked("aa", &hook("x", "before = [\"bb/y\"]\n"), &calls),
            hooked("bb", &hook("y", "before = [\"aa/x\"]\n"), &calls),
        ],
        &removing(&["bb"]),
    );
    assert!(problems.is_empty(), "{problems:?}");
    let refused = registry.add_back("bb").unwrap_err();
    assert!(
        refused.starts_with("adding bb back would close a cycle: "),
        "{refused}"
    );
    assert!(
        refused.contains("aa/x") && refused.contains("bb/y"),
        "{refused}"
    );
    assert_eq!(registry.add_back("aa"), Ok(Vec::new()));
}

#[test]
fn adding_back_a_plugin_with_a_clash_warns_from_the_next_session() {
    let (registry, _) = Registry::from_sources(
        vec![
            fixed("alpha", &["dup"], vec![def("dup", OBJECT)]),
            fixed("beta", &["dup"], vec![def("dup", OBJECT)]),
        ],
        &removing(&["beta"]),
    );
    let warnings = registry.add_back("beta").unwrap();
    assert!(!warnings.is_empty());
    assert!(
        warnings.iter().all(|w| w.ends_with("(from the next turn)")),
        "{warnings:?}"
    );
}
