use std::collections::BTreeMap;
use std::fmt::Write;

use serde_json::Value;

pub(crate) const NO_OWNER: &str = "<no wisp frame>";
const TOP: usize = 25;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Bytes {
    pub(crate) end: u64,
    pub(crate) peak: u64,
    pub(crate) total: u64,
}

impl std::ops::AddAssign for Bytes {
    fn add_assign(&mut self, other: Bytes) {
        self.end += other.end;
        self.peak += other.peak;
        self.total += other.total;
    }
}

#[derive(Debug, Default)]
pub(crate) struct Owners {
    pub(crate) rows: BTreeMap<String, Bytes>,
}

fn name(frame: &str) -> Option<&str> {
    let (_, rest) = frame.split_once(": ")?;
    Some(rest.rfind(" (").map_or(rest, |at| &rest[..at]))
}

fn async_body(frame: &str) -> bool {
    name(frame).is_some_and(|n| n.starts_with("{async_fn#") || n.starts_with("{async_block#"))
}

fn future_owner(frame: &str) -> Option<String> {
    let inner = name(frame)?.strip_prefix("poll<")?;
    let rest = &inner[inner.find("wisp_")?..];
    let end = ["::{async_fn_env", "::{async_block_env"]
        .iter()
        .filter_map(|e| rest.find(e))
        .min()?;
    let mut path = rest[..end].to_owned();
    while let Some(at) = path.find("{impl#") {
        let end = path[at..].find("}::").map_or(path.len(), |n| at + n + 3);
        path.replace_range(at..end, "");
    }
    (path.starts_with("wisp_") && !path.contains(['/', '<', '{'])).then_some(path)
}

pub(crate) fn label(frame: &str) -> Option<String> {
    let name = name(frame)?;
    let name = match name.strip_prefix('<') {
        Some(inner) => {
            let close = closing(inner)?;
            let (own, after) = (&inner[..close], &inner[close + 1..]);
            if top_level(own).contains(" as ") {
                return None;
            }
            format!("{own}{after}")
        }
        None => name.to_owned(),
    };
    let mut path = top_level(&name).replace("::{{closure}}", "");
    while let Some(at) = path.find("::{closure#") {
        let end = path[at..].find('}').map_or(path.len(), |n| at + n + 1);
        path.replace_range(at..end, "");
    }
    (path.starts_with("wisp_") && !path.contains('/')).then_some(path)
}

fn closing(text: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut previous = ' ';
    for (at, c) in text.char_indices() {
        match c {
            '<' => depth += 1,
            '>' if previous != '-' => {
                if depth == 0 {
                    return Some(at);
                }
                depth -= 1;
            }
            _ => {}
        }
        previous = c;
    }
    None
}

fn top_level(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('<') {
        out.push_str(rest[..at].trim_end_matches("::"));
        let Some(close) = closing(&rest[at + 1..]) else {
            return out;
        };
        rest = &rest[at + 1 + close + 1..];
    }
    out.push_str(rest);
    out
}

pub(crate) fn group(json: &str) -> Owners {
    let profile: Value = serde_json::from_str(json).unwrap();
    let table: Vec<&str> = profile["ftbl"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap())
        .collect();
    let labels: Vec<Option<String>> = table.iter().map(|f| label(f)).collect();
    let mut owners = Owners::default();
    owners.rows.insert(NO_OWNER.to_owned(), Bytes::default());
    for point in profile["pps"].as_array().unwrap() {
        let stack: Vec<usize> = point["fs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| usize::try_from(i.as_u64().unwrap()).unwrap())
            .collect();
        let owner = stack
            .iter()
            .enumerate()
            .find_map(|(at, &frame)| {
                labels[frame].clone().or_else(|| {
                    let next = *stack.get(at + 1)?;
                    async_body(table[frame])
                        .then(|| future_owner(table[next]))
                        .flatten()
                })
            })
            .unwrap_or_else(|| NO_OWNER.to_owned());
        let field = |name: &str| point[name].as_u64().unwrap_or(0);
        *owners.rows.entry(owner).or_default() += Bytes {
            end: field("eb"),
            peak: field("gb"),
            total: field("tb"),
        };
    }
    owners
}

impl Owners {
    fn sum(&self) -> Bytes {
        let mut sum = Bytes::default();
        for bytes in self.rows.values() {
            sum += *bytes;
        }
        sum
    }

    pub(crate) fn crates(&self) -> BTreeMap<String, Bytes> {
        let mut crates: BTreeMap<String, Bytes> = BTreeMap::new();
        for (label, bytes) in &self.rows {
            let name = label.split("::").next().unwrap_or(label).to_owned();
            *crates.entry(name).or_default() += *bytes;
        }
        crates
    }

    pub(crate) fn table(&self, title: &str, pick: impl Fn(&Bytes) -> u64) -> String {
        let all = pick(&self.sum()).max(1);
        let mut rows: Vec<(&String, u64)> = self.rows.iter().map(|(l, b)| (l, pick(b))).collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        let mut out = format!("| {title} | bytes | share |\n|---|---:|---:|\n");
        let mut shown_none = false;
        for (label, bytes) in rows.iter().take(TOP) {
            shown_none |= label.as_str() == NO_OWNER;
            row(&mut out, label, *bytes, all);
        }
        if !shown_none {
            let none = pick(&self.rows[NO_OWNER]);
            row(&mut out, NO_OWNER, none, all);
        }
        row(&mut out, "all", all, all);
        out
    }

    pub(crate) fn report(&self, what: &str) -> String {
        let mut out = format!("OWNERS {what}\n\n");
        out.push_str(&self.table("live at the end", |b| b.end));
        out.push('\n');
        out.push_str(&self.table("live at the peak", |b| b.peak));
        out.push('\n');
        out.push_str(&self.table("allocated in total", |b| b.total));
        out.push_str("\n| crate | at the end | at the peak | in total |\n|---|---:|---:|---:|\n");
        for (name, bytes) in self.crates() {
            let _ = writeln!(
                out,
                "| {name} | {} | {} | {} |",
                bytes.end, bytes.peak, bytes.total
            );
        }
        out
    }
}

fn row(out: &mut String, label: &str, bytes: u64, all: u64) {
    let share = bytes as f64 * 100.0 / all as f64;
    let _ = writeln!(out, "| `{label}` | {bytes} | {share:.1}% |");
}
