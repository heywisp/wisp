use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use wisp_plugin_api::index::{Entry, Kind};

fn words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    for word in text.split(|c: char| !c.is_alphanumeric() && c != '_') {
        let word = word.trim_matches('_');
        if word.is_empty() {
            continue;
        }
        let mut parts = Vec::new();
        let mut start = 0;
        let mut chars = word.char_indices().peekable();
        let mut previous = '_';
        while let Some((i, c)) = chars.next() {
            let next_is_lower = chars.peek().is_some_and(|(_, c)| c.is_lowercase());
            if c == '_'
                || (i > start
                    && c.is_uppercase()
                    && (previous.is_lowercase() || previous.is_numeric() || next_is_lower))
            {
                parts.push(&word[start..i]);
                start = i + if c == '_' { 1 } else { 0 };
            }
            previous = c;
        }
        parts.push(&word[start..]);
        if parts.len() > 1 {
            parts.insert(0, word);
        }
        words.extend(
            parts
                .into_iter()
                .filter(|part| !part.is_empty())
                .map(|part| {
                    let mut word = part.to_lowercase();
                    if word.len() > 1 && word.ends_with('s') {
                        word.pop();
                    }
                    word
                }),
        );
    }
    words
}

#[derive(Debug, Clone)]
pub struct Index {
    entries: Vec<(Entry, BTreeMap<String, usize>)>,
    frequency: BTreeMap<String, usize>,
    average: f64,
}

impl Index {
    pub fn new(mut entries: Vec<Entry>) -> Self {
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        let mut frequency = BTreeMap::new();
        let mut length = 0;
        let entries: Vec<_> = entries
            .into_iter()
            .map(|entry| {
                let mut terms = BTreeMap::new();
                for word in words(&entry.text).into_iter().chain(words(&entry.name)) {
                    *terms.entry(word).or_default() += 1;
                    length += 1;
                }
                for word in terms.keys() {
                    *frequency.entry(word.clone()).or_default() += 1;
                }
                (entry, terms)
            })
            .collect();
        let average = length as f64 / entries.len().max(1) as f64;
        Self {
            entries,
            frequency,
            average,
        }
    }

    pub fn search(&self, query: &str, kind: Option<Kind>, limit: Option<usize>) -> Vec<Entry> {
        self.search_above(query, kind, limit, 0.0)
    }

    pub fn search_above(
        &self,
        query: &str,
        kind: Option<Kind>,
        limit: Option<usize>,
        minimum: f64,
    ) -> Vec<Entry> {
        let query: BTreeSet<_> = words(query).into_iter().collect();
        let mut hits: Vec<_> = self
            .entries
            .iter()
            .filter(|(entry, _)| kind.is_none_or(|kind| entry.kind == kind))
            .map(|(entry, terms)| (entry, self.score(&query, terms)))
            .filter(|(_, score)| *score > minimum.max(0.0))
            .collect();
        hits.sort_by(|(a, sa), (b, sb)| {
            sb.total_cmp(sa)
                .then(a.name.cmp(&b.name))
                .then(a.kind.cmp(&b.kind))
        });
        let best = hits.first().map_or(0.0, |(_, score)| *score);
        hits.into_iter()
            .take_while(|(_, score)| *score >= best * 0.3)
            .take(limit.unwrap_or(5))
            .map(|(entry, _)| entry.clone())
            .collect()
    }

    fn score(&self, query: &BTreeSet<String>, terms: &BTreeMap<String, usize>) -> f64 {
        let length = terms.values().sum::<usize>() as f64;
        query
            .iter()
            .filter_map(|word| {
                let tf = *terms.get(word)? as f64;
                let df = *self.frequency.get(word)? as f64;
                let idf = (1.0 + (self.entries.len() as f64 - df + 0.5) / (df + 0.5)).ln();
                Some(idf * tf * 2.2 / (tf + 1.2 * (0.25 + 0.75 * length / self.average)))
            })
            .sum()
    }
}

pub fn tool_entry(name: &str, description: &str, schema: &Value) -> Entry {
    let mut text = format!("{name}\n{description}");
    arguments(schema, &mut text);
    Entry {
        kind: Kind::Tool,
        name: name.to_owned(),
        summary: first_line(description).to_owned(),
        text,
    }
}

fn arguments(schema: &Value, text: &mut String) {
    match schema {
        Value::Object(object) => {
            if let Some(description) = object.get("description").and_then(Value::as_str) {
                text.push('\n');
                text.push_str(description);
            }
            if let Some(properties) = object.get("properties").and_then(Value::as_object) {
                for name in properties.keys() {
                    text.push('\n');
                    text.push_str(name);
                }
            }
            for value in object.values() {
                arguments(value, text);
            }
        }
        Value::Array(values) => {
            for value in values {
                arguments(value, text);
            }
        }
        _ => {}
    }
}

pub fn first_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
