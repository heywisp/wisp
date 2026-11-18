pub const REPORT: &str = "Returns the facts found, each with its source, then a numbered list of the sources. Use it for anything recent or that you are not sure of.";
pub const QUERY: &str = "What to search for, in plain words.";
pub const INSTRUCTION: &str = "Search the web for the user's query and report what you find: every figure, date, name and version that bears on it, each with the source it comes from. Report the findings in full rather than a one-line answer; your reader sees nothing but this report.";
pub const MAX_TOKENS: u32 = 4096;

pub fn call_query() -> String {
    format!("{QUERY} Make it complete in itself: the search sees none of this conversation.")
}

pub fn schema(query: &str) -> String {
    format!(
        r#"{{"type":"object","properties":{{"query":{{"type":"string","description":{query:?}}}}},"required":["query"]}}"#
    )
}

pub fn numbered<'a>(sources: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    sources
        .into_iter()
        .enumerate()
        .map(|(n, (title, url))| format!("{}. {title} — {url}", n + 1))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests;
