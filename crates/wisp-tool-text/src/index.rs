pub const INVALID_INPUT: &str = "input must be an object with the tool's fields";

use wisp_plugin_api::index::Entry;

pub const SEARCH_OFF: &str =
    "search is off, so the model can't find these: turn it on in /config, Tools";

pub const SAVED_LIST: &str = "(saved list; may be out of date)";

pub fn from_saved_list(summary: &str) -> String {
    if summary.is_empty() {
        SAVED_LIST.to_owned()
    } else {
        format!("{summary} {SAVED_LIST}")
    }
}

pub fn hit_line(entry: &Entry, next: &str) -> String {
    format!(
        "{} {} — {} — {next}",
        entry.kind.name(),
        entry.name,
        entry.summary
    )
}

pub fn run_with(parameters: &str) -> String {
    format!("run it with use; parameters: {parameters}")
}

pub fn not_loaded(name: &str) -> String {
    format!("this tool isn't loaded; run it with use: {{\"name\":\"{name}\",\"input\":{{}}}}")
}

pub fn parameters(message: &str, parameters: &str) -> String {
    format!("{message}\nParameters: {parameters}")
}

pub fn already_in_view(name: &str) -> String {
    format!("Skill `{name}` is already in view.")
}
