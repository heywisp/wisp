use std::ops::Range;

pub struct Query {
    words: Vec<String>,
}

impl Query {
    pub fn new(text: &str) -> Query {
        Query {
            words: text.split_whitespace().map(str::to_lowercase).collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    pub fn matches(&self, texts: &[&str]) -> bool {
        if self.is_empty() {
            return true;
        }
        let lower: Vec<String> = texts.iter().map(|text| text.to_lowercase()).collect();
        self.words
            .iter()
            .all(|word| lower.iter().any(|text| text.contains(word.as_str())))
    }

    pub fn found(&self, text: &str) -> Vec<Range<usize>> {
        let lower = text.to_lowercase();
        if lower.len() != text.len() {
            return Vec::new();
        }
        let mut hits: Vec<Range<usize>> = self
            .words
            .iter()
            .filter_map(|word| lower.find(word.as_str()).map(|at| at..at + word.len()))
            .collect();
        hits.sort_by_key(|hit| hit.start);
        let mut merged: Vec<Range<usize>> = Vec::new();
        for hit in hits {
            match merged.last_mut() {
                Some(last) if hit.start <= last.end => last.end = last.end.max(hit.end),
                _ => merged.push(hit),
            }
        }
        merged
    }
}

#[cfg(test)]
mod tests;
