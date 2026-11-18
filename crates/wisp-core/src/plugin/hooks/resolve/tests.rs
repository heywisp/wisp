use super::*;

fn node(full: &str, after: &[&str], before: &[&str]) -> Node {
    Node {
        name: Name::parse(full).unwrap(),
        kind: Kind::Hook,
        sets: Vec::new(),
        after: after.iter().map(|s| (*s).to_owned()).collect(),
        before: before.iter().map(|s| (*s).to_owned()).collect(),
        continues: None,
    }
}

fn joined(full: &str, set: &str, after: &[&str], before: &[&str]) -> Node {
    Node {
        sets: vec![set.to_owned()],
        ..node(full, after, before)
    }
}

const BARE_CHAIN: [&str; 3] = ["core/tools", "shell/processes", "core/typed"];

fn environment() -> Node {
    Node {
        kind: Kind::Section,
        continues: Some("core/environment".into()),
        ..joined("workspace/environment", "core/state", &[], &[])
    }
}

fn processes() -> Node {
    Node {
        continues: Some("core/processes".into()),
        ..joined("shell/processes", "core/news", &[], &[])
    }
}

fn resolve(point: &str, nodes: &[Node]) -> Order {
    let mut nodes = nodes.to_vec();
    if point == BEFORE_TURN
        && nodes
            .iter()
            .all(|n| n.name.to_string() != "shell/processes")
    {
        nodes.push(processes());
    }
    super::resolve(point, &nodes)
}

fn names(order: &Order) -> Vec<String> {
    order.order.iter().map(ToString::to_string).collect()
}

#[test]
fn the_order_is_the_smallest_by_plugin_then_hook() {
    let order = resolve(
        "after-turn",
        &[
            node("a-b/x", &[], &[]),
            node("a/x", &[], &[]),
            node("a/w", &[], &[]),
        ],
    );
    assert_eq!(names(&order), ["a/w", "a/x", "a-b/x"]);
    assert!("a-b/x" < "a/x", "as one string the order would differ");
}

#[test]
fn before_and_after_are_the_same_edge() {
    let one = resolve(
        "after-turn",
        &[node("b/h", &[], &[]), node("z/h", &[], &["b/h"])],
    );
    let two = resolve(
        "after-turn",
        &[node("b/h", &["z/h"], &[]), node("z/h", &[], &[])],
    );
    assert_eq!(names(&one), ["z/h", "b/h"]);
    assert_eq!(one, two);
}

#[test]
fn an_unconstrained_before_turn_hook_lands_in_the_notes_whatever_its_plugin_id() {
    for id in ["aa/x", "acme/x", "zz/x"] {
        let order = resolve("before-turn", &[node(id, &[], &[])]);
        assert_eq!(
            names(&order),
            ["core/tools", id, "shell/processes", "core/typed"]
        );
    }
    let early = resolve("before-turn", &[node("acme/x", &[], &["core/tools"])]);
    assert_eq!(names(&early)[0], "acme/x");
}

#[test]
fn an_unplaced_section_lands_in_the_state_and_a_hook_in_the_notes() {
    let section = Node {
        kind: Kind::Section,
        ..node("zz/s", &[], &[])
    };
    let order = resolve("before-turn", &[section, node("aa/h", &[], &[])]);
    assert_eq!(
        names(&order),
        [
            "zz/s",
            "core/tools",
            "aa/h",
            "shell/processes",
            "core/typed"
        ]
    );
}

#[test]
fn a_hook_joins_the_set_it_names_and_a_set_in_after_or_before_means_the_whole_set() {
    let order = resolve(
        "before-turn",
        &[
            joined("aa/state", "core/state", &[], &[]),
            joined("aa/news", "core/news", &[], &[]),
            node("zz/before-state", &[], &["core/state"]),
            node("aa/after-news", &["core/news"], &[]),
        ],
    );
    assert_eq!(
        names(&order),
        [
            "zz/before-state",
            "aa/state",
            "core/tools",
            "aa/news",
            "shell/processes",
            "aa/after-news",
            "core/typed"
        ]
    );
}

#[test]
fn a_hook_after_only_a_plugin_hook_keeps_its_lower_bound_and_no_upper_one() {
    let order = resolve(
        "before-turn",
        &[
            node("xx/first", &["core/processes"], &[]),
            node("xx/second", &["xx/first"], &[]),
        ],
    );
    assert!(order.cycles.is_empty(), "{:?}", order.cycles);
    assert_eq!(
        names(&order),
        [
            "core/tools",
            "shell/processes",
            "xx/first",
            "xx/second",
            "core/typed"
        ]
    );
    let alone = resolve("before-turn", &[node("aa/x", &["zz/gone"], &[])]);
    assert_eq!(names(&alone)[1], "aa/x");
}

#[test]
fn a_set_node_never_delays_a_hook_that_sorts_after_it() {
    let order = resolve(
        "before-turn",
        &[
            joined("aa/n", "core/news", &[], &[]),
            node("ab/x", &["core/tools"], &[]),
        ],
    );
    assert_eq!(
        names(&order),
        [
            "core/tools",
            "aa/n",
            "ab/x",
            "shell/processes",
            "core/typed"
        ]
    );
}

#[test]
fn an_empty_set_still_orders() {
    let order = resolve(
        "before-turn",
        &[
            node("aa/late", &["core/notes"], &[]),
            node("zz/early", &[], &["core/notes"]),
        ],
    );
    let names = names(&order);
    let at = |n: &str| names.iter().position(|o| o == n).unwrap();
    assert!(at("zz/early") < at("aa/late"), "{names:?}");
    assert!(at("core/tools") < at("aa/late"), "{names:?}");
    assert!(at("zz/early") < at("shell/processes"), "{names:?}");
}

#[test]
fn the_bundled_order_reproduces_todays_lead() {
    let today = [
        "workspace/environment",
        "core/tools",
        "time/sent",
        "context/fill",
        "shell/processes",
        "core/typed",
    ];
    let bundled = resolve(
        "before-turn",
        &[
            environment(),
            joined("context/fill", "core/notes", &["time/sent"], &[]),
            joined("time/sent", "core/notes", &[], &[]),
        ],
    );
    assert_eq!(names(&bundled), today);
    let named_neighbours = resolve(
        "before-turn",
        &[
            environment(),
            node(
                "context/fill",
                &["core/tools", "time/sent"],
                &["core/processes"],
            ),
            node("time/sent", &["core/tools"], &["core/processes"]),
        ],
    );
    assert_eq!(names(&named_neighbours), today);
}

#[test]
fn a_hook_after_the_processes_still_comes_before_the_typed_text() {
    let order = resolve("before-turn", &[node("zz/h", &["core/processes"], &[])]);
    assert_eq!(names(&order).last().unwrap(), "core/typed");
    assert_eq!(names(&order)[2], "zz/h");
}

#[test]
fn names_that_are_not_nodes_here_are_ignored() {
    let order = resolve(
        "after-turn",
        &[node("a/h", &["gone/x", "core/typed"], &["nobody/y"])],
    );
    assert_eq!(names(&order), ["a/h"]);
    assert!(order.cycles.is_empty());
}

#[test]
fn requires_disables_to_a_fixed_point() {
    let hooks = vec![
        (Name::new("a", "h"), vec!["b/h".to_owned()]),
        (Name::new("b", "h"), vec!["c/h".to_owned()]),
        (Name::new("c", "h"), vec!["gone/h".to_owned()]),
        (Name::new("d", "h"), vec!["e/h".to_owned()]),
        (Name::new("e", "h"), Vec::new()),
    ];
    let missing = missing(&hooks);
    assert_eq!(
        missing.keys().map(ToString::to_string).collect::<Vec<_>>(),
        ["a/h", "b/h", "c/h"]
    );
    assert_eq!(missing[&Name::new("c", "h")], ["gone/h"]);
    assert_eq!(missing[&Name::new("a", "h")], ["b/h"]);
}

#[test]
fn a_cycle_disables_its_hooks_and_names_the_shortest_cycle_through_its_smallest_node() {
    let order = resolve(
        "after-turn",
        &[
            node("context/fill", &["acme/x"], &[]),
            node("acme/x", &["context/fill"], &[]),
            node("zed/z", &["acme/x"], &[]),
        ],
    );
    assert_eq!(names(&order), ["zed/z"]);
    let cycle = vec!["acme/x", "context/fill", "acme/x"];
    assert_eq!(order.cycles[&Name::new("acme", "x")], cycle);
    assert_eq!(order.cycles[&Name::new("context", "fill")], cycle);
}

#[test]
fn the_cycle_walk_ends_on_several_cycles_and_takes_the_shortest() {
    let order = resolve(
        "after-turn",
        &[
            node("a/a", &[], &["b/b", "c/c"]),
            node("b/b", &[], &["d/d"]),
            node("c/c", &[], &["a/a"]),
            node("d/d", &[], &["e/e"]),
            node("e/e", &[], &["a/a", "b/b"]),
            node("f/f", &[], &["f/f"]),
        ],
    );
    assert_eq!(order.cycles[&Name::new("a", "a")], ["a/a", "c/c", "a/a"]);
    assert_eq!(order.cycles[&Name::new("f", "f")], ["f/f", "f/f"]);
    assert!(order.order.is_empty());
    let order = resolve(
        "after-turn",
        &[
            node("a/a", &[], &["b/b", "c/c"]),
            node("b/b", &[], &["a/a"]),
            node("c/c", &[], &["d/d"]),
            node("d/d", &[], &["a/a"]),
        ],
    );
    assert_eq!(order.cycles[&Name::new("a", "a")], ["a/a", "b/b", "a/a"]);
}

#[test]
fn a_cycle_through_a_core_node_disables_only_the_hooks() {
    let order = resolve(
        "before-turn",
        &[node("acme/x", &["core/processes"], &["core/tools"])],
    );
    assert!(order.cycles.contains_key(&Name::new("acme", "x")));
    assert_eq!(names(&order), BARE_CHAIN);
    let order = resolve(
        "before-turn",
        &[
            environment(),
            node("acme/y", &["core/environment"], &["core/state"]),
        ],
    );
    assert_eq!(
        order.cycles.keys().collect::<Vec<_>>(),
        [&Name::new("acme", "y")]
    );
    assert_eq!(names(&order)[0], "workspace/environment");
}

#[test]
fn a_cycle_through_a_set_is_named_by_the_set() {
    let order = resolve(
        "before-turn",
        &[joined("acme/x", "core/news", &[], &["core/tools"])],
    );
    assert_eq!(
        order.cycles[&Name::new("acme", "x")],
        ["acme/x", "core/tools", "core/notes", "core/news", "acme/x"]
    );
    assert_eq!(names(&order), BARE_CHAIN);
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(n.max(1)).unwrap_or(1)).unwrap_or(0)
    }

    fn chance(&mut self, pct: usize) -> bool {
        self.below(100) < pct
    }

    fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.below(i + 1);
            items.swap(i, j);
        }
    }
}

const SEEDS: u64 = 400;

fn seeds() -> impl Iterator<Item = Rng> {
    (1..=SEEDS).map(|seed| Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1))
}

const PLUGINS: [&str; 5] = ["a", "a-b", "b", "core-x", "zz"];

fn random_names(rng: &mut Rng) -> Vec<String> {
    let count = 2 + rng.below(7);
    let mut out: Vec<String> = Vec::new();
    while out.len() < count {
        let name = format!("{}/h{}", PLUGINS[rng.below(PLUGINS.len())], rng.below(4));
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

const PLACES: [(&str, &str); 8] = [
    ("in", "core/state"),
    ("after", "core/tools"),
    ("in", "core/notes"),
    ("", ""),
    ("before", "core/news"),
    ("in", "core/news"),
    ("after", "core/processes"),
    ("after", "core/news"),
];

fn random_graph(rng: &mut Rng, acyclic: bool) -> Vec<Node> {
    let names = random_names(rng);
    let mut places: Vec<usize> = names.iter().map(|_| rng.below(PLACES.len())).collect();
    if acyclic {
        places.sort_unstable();
    }
    names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let mut after = Vec::new();
            let mut before = Vec::new();
            for (j, other) in names.iter().enumerate() {
                if i == j || !rng.chance(30) {
                    continue;
                }
                let earlier = if acyclic { j < i } else { rng.chance(50) };
                if earlier {
                    after.push(other.clone());
                } else {
                    before.push(other.clone());
                }
            }
            let mut sets = Vec::new();
            match PLACES[places[i]] {
                ("in", set) => sets.push(set.to_owned()),
                ("after", core) => after.push(core.to_owned()),
                ("before", core) => before.push(core.to_owned()),
                _ => {}
            }
            Node {
                name: Name::parse(name).unwrap(),
                kind: Kind::Hook,
                sets,
                after,
                before,
                continues: None,
            }
        })
        .collect()
}

fn rank(name: &str) -> Option<usize> {
    let set = if name == "shell/processes" {
        "core/news"
    } else {
        name
    };
    CORE_CHAIN.iter().position(|n| *n == set)
}

fn assert_placed(node: &Node, order: &[String]) {
    let position = |n: &str| order.iter().position(|o| o == n).unwrap();
    let me = position(&node.name.to_string());
    let is_set = |n: &str| CORE_SETS.contains(&n);
    let names_core = node
        .after
        .iter()
        .chain(&node.before)
        .any(|n| n.starts_with("core/"));
    let mut joined: Vec<&str> = node.sets.iter().map(String::as_str).collect();
    let mut lower: Vec<&str> = Vec::new();
    if joined.is_empty() && !names_core {
        if node.after.is_empty() && node.before.is_empty() {
            joined.push("core/notes");
        } else {
            lower.push("core/notes");
        }
    }
    for core in order.iter().filter(|o| o.starts_with("core/")) {
        let (Some(theirs), at) = (rank(core), position(core)) else {
            continue;
        };
        for set in &joined {
            let mine = rank(set).unwrap();
            assert!(
                theirs >= mine || at < me,
                "{core} before {node:?}: {order:?}"
            );
            assert!(
                theirs <= mine || me < at,
                "{core} after {node:?}: {order:?}"
            );
        }
        for set in &lower {
            assert!(
                theirs >= rank(set).unwrap() || at < me,
                "{core} before {node:?}: {order:?}"
            );
        }
        for a in node.after.iter().filter(|a| is_set(a)) {
            assert!(theirs > rank(a).unwrap() || at < me, "{node:?}: {order:?}");
        }
        for b in node.before.iter().filter(|b| is_set(b)) {
            assert!(theirs < rank(b).unwrap() || me < at, "{node:?}: {order:?}");
        }
    }
}

#[test]
fn random_dags_give_one_order_whatever_the_input_order() {
    for mut rng in seeds() {
        let mut nodes = random_graph(&mut rng, true);
        let first = resolve("before-turn", &nodes);
        let order = names(&first);
        assert!(first.cycles.is_empty(), "{nodes:?}");
        assert_eq!(order.last().unwrap(), "core/typed", "{nodes:?}");
        assert_eq!(first.order.len(), nodes.len() + BARE_CHAIN.len());
        for _ in 0..4 {
            rng.shuffle(&mut nodes);
            assert_eq!(resolve("before-turn", &nodes), first, "{nodes:?}");
        }
        let position = |n: &str| order.iter().position(|o| o == n).unwrap();
        for n in &nodes {
            let me = position(&n.name.to_string());
            for a in n.after.iter().filter(|a| order.contains(a)) {
                assert!(position(a) < me, "{nodes:?}");
            }
            for b in n.before.iter().filter(|b| order.contains(b)) {
                assert!(me < position(b), "{nodes:?}");
            }
            assert_placed(n, &order);
        }
    }
}

#[test]
fn random_graphs_with_cycles_give_one_disabled_set_and_one_message() {
    let mut cyclic = 0;
    for mut rng in seeds() {
        let mut nodes = random_graph(&mut rng, false);
        let first = resolve("after-turn", &nodes);
        for _ in 0..4 {
            rng.shuffle(&mut nodes);
            assert_eq!(resolve("after-turn", &nodes), first, "{nodes:?}");
        }
        for (name, cycle) in &first.cycles {
            assert_eq!(cycle.first(), cycle.last(), "{name}: {cycle:?}");
            assert!(
                cycle.contains(&name.to_string()) || cycle.len() >= 2,
                "{cycle:?}"
            );
        }
        cyclic += usize::from(!first.cycles.is_empty());
        assert_eq!(first.order.len() + first.cycles.len(), nodes.len());
    }
    assert!(cyclic > 20, "{cyclic}");
}

#[test]
fn the_old_core_names_are_aliases_of_the_nodes_that_continue_them() {
    let old = resolve(
        "before-turn",
        &[
            environment(),
            node("aa/early", &[], &["core/processes"]),
            node("aa/late", &["core/environment"], &[]),
        ],
    );
    let new = resolve(
        "before-turn",
        &[
            environment(),
            node("aa/early", &[], &["shell/processes"]),
            node("aa/late", &["workspace/environment"], &[]),
        ],
    );
    assert!(old.cycles.is_empty(), "{:?}", old.cycles);
    assert_eq!(names(&old), names(&new));
    let at = |n: &str| names(&old).iter().position(|o| o == n).unwrap();
    assert!(at("aa/early") < at("shell/processes"));
    assert!(at("workspace/environment") < at("aa/late"));
    for after in ["shell/processes", "core/processes"] {
        let order = resolve("before-turn", &[node("zz/after", &[after], &[])]);
        assert!(order.cycles.is_empty(), "{after}: {:?}", order.cycles);
        let names = names(&order);
        let at = |n: &str| names.iter().position(|o| o == n).unwrap();
        assert!(at("shell/processes") < at("zz/after"), "{after}: {names:?}");
    }
}

#[test]
fn a_manifest_node_continuing_a_core_name_takes_its_place_and_counts_as_core() {
    let continuing = Node {
        sets: Vec::new(),
        ..environment()
    };
    let hook = || node("aa/x", &["core/environment"], &[]);
    let order = resolve("before-turn", &[continuing, hook()]);
    assert!(order.cycles.is_empty(), "{:?}", order.cycles);
    assert_eq!(
        names(&order),
        names(&resolve("before-turn", &[environment(), hook()]))
    );
    assert_eq!(names(&order)[0], "workspace/environment");
    let at = |n: &str| names(&order).iter().position(|o| o == n).unwrap();
    assert!(at("workspace/environment") < at("aa/x"));
}

#[test]
fn without_a_node_continuing_it_the_environment_is_no_node_and_its_old_name_is_ignored() {
    assert!(!Name::new("workspace", "environment").is_core());
    let order = resolve("before-turn", &[node("aa/x", &["core/environment"], &[])]);
    assert!(order.cycles.is_empty(), "{:?}", order.cycles);
    assert!(
        !names(&order).contains(&"workspace/environment".to_owned()),
        "{:?}",
        names(&order)
    );
}

#[test]
fn without_a_node_continuing_it_the_notice_is_no_node_and_its_old_name_is_ignored() {
    let order = super::resolve("before-turn", &[node("aa/x", &["core/processes"], &[])]);
    assert!(order.cycles.is_empty(), "{:?}", order.cycles);
    assert!(
        !names(&order).contains(&"shell/processes".to_owned()),
        "{:?}",
        names(&order)
    );
}
