use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, VecDeque};
use std::fmt;

use wisp_plugin_api::manifest::{
    BEFORE_TURN, CORE_ENVIRONMENT, CORE_ID, CORE_NEWS, CORE_NOTES, CORE_PROCESSES, CORE_SETS,
    CORE_STATE, TYPED_NODE,
};
use wisp_protocol::log::LEAD_TOOLS;

pub const CORE_CHAIN: [&str; 5] = [CORE_STATE, LEAD_TOOLS, CORE_NOTES, CORE_NEWS, TYPED_NODE];

const CORE_MEMBERS: [(&str, &str); 2] =
    [(CORE_ENVIRONMENT, CORE_STATE), (CORE_PROCESSES, CORE_NEWS)];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Hook,
    Section,
}

impl Kind {
    const fn default_slot(self) -> &'static str {
        match self {
            Self::Hook => CORE_NOTES,
            Self::Section => CORE_STATE,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Name {
    pub plugin: String,
    pub hook: String,
}

impl Name {
    pub fn new(plugin: &str, hook: &str) -> Self {
        Self {
            plugin: plugin.to_owned(),
            hook: hook.to_owned(),
        }
    }

    pub fn parse(full: &str) -> Option<Self> {
        let (plugin, hook) = full.split_once('/')?;
        Some(Self::new(plugin, hook))
    }

    pub fn is_core(&self) -> bool {
        self.plugin == CORE_ID
    }
}

impl fmt::Display for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.plugin, self.hook)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub name: Name,
    pub kind: Kind,
    pub sets: Vec<String>,
    pub after: Vec<String>,
    pub before: Vec<String>,
    pub continues: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Order {
    pub order: Vec<Name>,
    pub cycles: BTreeMap<Name, Vec<String>>,
}

struct Graph {
    names: Vec<Name>,
    real: usize,
    edges: BTreeSet<(usize, usize)>,
    continuing: BTreeSet<String>,
}

impl Graph {
    fn build(point: &str, nodes: &[Node]) -> Self {
        let mut names: BTreeSet<Name> = nodes.iter().map(|n| n.name.clone()).collect();
        let before_turn = point == BEFORE_TURN;
        let mut aliases: BTreeMap<String, String> = BTreeMap::new();
        let mut members: Vec<(String, &str)> = Vec::new();
        if before_turn {
            for (old, set) in CORE_MEMBERS {
                let Some(holder) = nodes.iter().find(|n| n.continues.as_deref() == Some(old))
                else {
                    continue;
                };
                let holder = holder.name.to_string();
                aliases.insert(old.to_owned(), holder.clone());
                members.push((holder, set));
            }
            let core = CORE_CHAIN.iter().filter(|n| !CORE_SETS.contains(n));
            names.extend(core.filter_map(|n| Name::parse(n)));
        }
        let continuing: BTreeSet<String> = aliases.values().cloned().collect();
        let named = |name: &str| aliases.get(name).map_or(name, String::as_str).to_owned();
        let mut names: Vec<Name> = names.into_iter().collect();
        let real = names.len();
        let mut ends: BTreeMap<String, (usize, usize)> = names
            .iter()
            .enumerate()
            .map(|(i, n)| (n.to_string(), (i, i)))
            .collect();
        let mut edges = BTreeSet::new();
        if before_turn {
            for set in CORE_SETS {
                let Some(name) = Name::parse(set) else {
                    continue;
                };
                let start = names.len();
                names.push(name.clone());
                names.push(name);
                ends.insert(set.to_owned(), (start, start + 1));
                edges.insert((start, start + 1));
            }
        }
        let first = |name: &str| ends.get(&named(name)).map(|(first, _)| *first);
        let last = |name: &str| ends.get(&named(name)).map(|(_, last)| *last);
        let join = |edges: &mut BTreeSet<(usize, usize)>, member: &str, set: &str| {
            if let (Some(start), Some(m), Some(end)) = (first(set), first(member), last(set)) {
                edges.insert((start, m));
                edges.insert((m, end));
            }
        };
        if before_turn {
            for pair in CORE_CHAIN.windows(2) {
                if let (Some(a), Some(b)) = (last(pair[0]), first(pair[1])) {
                    edges.insert((a, b));
                }
            }
            for (member, set) in &members {
                let own_set = nodes
                    .iter()
                    .any(|n| n.name.to_string() == *member && !n.sets.is_empty());
                if !own_set {
                    join(&mut edges, member, set);
                }
            }
        }
        for node in nodes {
            let full = node.name.to_string();
            let Some(h) = first(&full) else {
                continue;
            };
            for a in node.after.iter().filter_map(|a| last(a)) {
                edges.insert((a, h));
            }
            for b in node.before.iter().filter_map(|b| first(b)) {
                edges.insert((h, b));
            }
            if before_turn && continuing.contains(&full) {
                if let Some(set) = node.sets.first() {
                    join(&mut edges, &full, set);
                }
                continue;
            }
            if before_turn && !node.name.is_core() {
                let names_core = node.after.iter().chain(&node.before).any(|n| {
                    continuing.contains(&named(n)) || Name::parse(n).is_some_and(|n| n.is_core())
                });
                let default = node.kind.default_slot();
                let unconstrained = node.after.is_empty() && node.before.is_empty();
                match node.sets.first() {
                    Some(set) => join(&mut edges, &full, set),
                    None if unconstrained => join(&mut edges, &full, default),
                    None if !names_core => {
                        if let Some(start) = first(default) {
                            edges.insert((start, h));
                        }
                    }
                    None => {}
                }
                if let Some(typed) = first(TYPED_NODE) {
                    edges.insert((h, typed));
                }
            }
        }
        Self {
            names,
            real,
            edges,
            continuing,
        }
    }

    fn is_virtual(&self, i: usize) -> bool {
        i >= self.real
    }

    fn order(&self, order: Vec<usize>, cycles: BTreeMap<Name, Vec<String>>) -> Order {
        Order {
            order: order
                .into_iter()
                .filter(|i| !self.is_virtual(*i))
                .map(|i| self.names[i].clone())
                .collect(),
            cycles,
        }
    }

    fn successors(&self, from: usize, within: &BTreeSet<usize>) -> Vec<usize> {
        self.edges
            .range((from, 0)..=(from, usize::MAX))
            .map(|(_, to)| *to)
            .filter(|to| within.contains(to))
            .collect()
    }

    fn kahn(&self, within: &BTreeSet<usize>) -> (Vec<usize>, BTreeSet<usize>) {
        let mut degree: BTreeMap<usize, usize> = within.iter().map(|i| (*i, 0)).collect();
        for (a, b) in &self.edges {
            if within.contains(a) && within.contains(b) {
                *degree.entry(*b).or_default() += 1;
            }
        }
        let key = |i: usize| Reverse((!self.is_virtual(i), &self.names[i], i));
        let mut ready: BinaryHeap<Reverse<(bool, &Name, usize)>> = degree
            .iter()
            .filter(|(_, d)| **d == 0)
            .map(|(i, _)| key(*i))
            .collect();
        let mut order = Vec::new();
        while let Some(Reverse((_, _, i))) = ready.pop() {
            order.push(i);
            for to in self.successors(i, within) {
                if let Some(d) = degree.get_mut(&to) {
                    *d -= 1;
                    if *d == 0 {
                        ready.push(key(to));
                    }
                }
            }
        }
        let placed: BTreeSet<usize> = order.iter().copied().collect();
        let left = within.difference(&placed).copied().collect();
        (order, left)
    }

    fn components(&self, within: &BTreeSet<usize>) -> Vec<BTreeSet<usize>> {
        let mut tarjan = Tarjan {
            graph: self,
            within,
            index: BTreeMap::new(),
            low: BTreeMap::new(),
            stack: Vec::new(),
            on_stack: BTreeSet::new(),
            next: 0,
            found: Vec::new(),
        };
        for v in within {
            if !tarjan.index.contains_key(v) {
                tarjan.visit(*v);
            }
        }
        tarjan
            .found
            .into_iter()
            .filter(|c| c.len() > 1 || c.iter().any(|v| self.edges.contains(&(*v, *v))))
            .collect()
    }

    fn shortest_cycle(&self, component: &BTreeSet<usize>) -> Vec<String> {
        let Some(start) = component.iter().min_by_key(|i| &self.names[**i]).copied() else {
            return Vec::new();
        };
        let mut parent: BTreeMap<usize, usize> = BTreeMap::new();
        let mut queue = VecDeque::from([start]);
        let mut seen = BTreeSet::from([start]);
        let mut last = start;
        'search: while let Some(u) = queue.pop_front() {
            let mut next = self.successors(u, component);
            next.sort_by_key(|v| &self.names[*v]);
            for v in next {
                if v == start {
                    last = u;
                    break 'search;
                }
                if seen.insert(v) {
                    parent.insert(v, u);
                    queue.push_back(v);
                }
            }
        }
        let mut path = vec![last];
        while let Some(p) = parent.get(path.last().unwrap_or(&start)).copied() {
            path.push(p);
        }
        path.reverse();
        path.push(start);
        path.dedup_by(|b, a| {
            self.is_virtual(*a) && self.is_virtual(*b) && self.names[*a] == self.names[*b]
        });
        path.into_iter()
            .map(|i| self.names[i].to_string())
            .collect()
    }
}

struct Tarjan<'a> {
    graph: &'a Graph,
    within: &'a BTreeSet<usize>,
    index: BTreeMap<usize, usize>,
    low: BTreeMap<usize, usize>,
    stack: Vec<usize>,
    on_stack: BTreeSet<usize>,
    next: usize,
    found: Vec<BTreeSet<usize>>,
}

impl Tarjan<'_> {
    fn visit(&mut self, v: usize) {
        self.index.insert(v, self.next);
        self.low.insert(v, self.next);
        self.next += 1;
        self.stack.push(v);
        self.on_stack.insert(v);
        for w in self.graph.successors(v, self.within) {
            if !self.index.contains_key(&w) {
                self.visit(w);
                let low = self.low[&v].min(self.low[&w]);
                self.low.insert(v, low);
            } else if self.on_stack.contains(&w) {
                let low = self.low[&v].min(self.index[&w]);
                self.low.insert(v, low);
            }
        }
        if self.low[&v] == self.index[&v] {
            let mut component = BTreeSet::new();
            while let Some(w) = self.stack.pop() {
                self.on_stack.remove(&w);
                component.insert(w);
                if w == v {
                    break;
                }
            }
            self.found.push(component);
        }
    }
}

pub fn resolve(point: &str, nodes: &[Node]) -> Order {
    let graph = Graph::build(point, nodes);
    let mut within: BTreeSet<usize> = (0..graph.names.len()).collect();
    let mut cycles = BTreeMap::new();
    loop {
        let (order, left) = graph.kahn(&within);
        if left.is_empty() {
            return graph.order(order, cycles);
        }
        let components = graph.components(&left);
        let count = within.len();
        for component in components {
            let cycle = graph.shortest_cycle(&component);
            for i in component {
                let name = &graph.names[i];
                if !name.is_core() && !graph.continuing.contains(&name.to_string()) {
                    cycles.insert(name.clone(), cycle.clone());
                    within.remove(&i);
                }
            }
        }
        if within.len() == count {
            return graph.order(order, cycles);
        }
    }
}

pub fn missing(hooks: &[(Name, Vec<String>)]) -> BTreeMap<Name, Vec<String>> {
    let mut out: BTreeMap<Name, Vec<String>> = BTreeMap::new();
    loop {
        let present = |full: &str| {
            hooks
                .iter()
                .any(|(n, _)| n.to_string() == full && !out.contains_key(n))
        };
        let found: Vec<(Name, Vec<String>)> = hooks
            .iter()
            .filter(|(n, _)| !out.contains_key(n))
            .filter_map(|(n, requires)| {
                let absent: Vec<String> =
                    requires.iter().filter(|r| !present(r)).cloned().collect();
                (!absent.is_empty()).then(|| (n.clone(), absent))
            })
            .collect();
        if found.is_empty() {
            return out;
        }
        out.extend(found);
    }
}

#[cfg(test)]
mod tests;
