use wisp_plugin_api::prompt::News;
use wisp_tool_text::jobs::{WAKE_LINE, item_line};

pub const PROCESSES: &str = "processes";

pub fn notice(news: &News) -> Option<String> {
    let lines: Vec<String> = news.items.iter().filter_map(item_line).collect();
    if lines.is_empty() {
        return None;
    }
    let body = news
        .wake
        .then(|| WAKE_LINE.to_owned())
        .into_iter()
        .chain(lines)
        .collect::<Vec<_>>()
        .join("\n");
    Some(format!("<processes>\n{body}\n</processes>"))
}
