use wisp_plugin_api::host::{Todo, TodoStatus};

pub const NAME: &str = "write_todos";
pub const MIN_ITEMS: usize = 1;
pub const MAX_ITEMS: usize = 50;
pub const MAX_TEXT_CHARS: usize = 500;
pub const MAX_SECTION_CHARS: usize = 60;

pub const STATUSES: [(&str, TodoStatus); 4] = [
    ("pending", TodoStatus::Pending),
    ("in_progress", TodoStatus::InProgress),
    ("completed", TodoStatus::Completed),
    ("cancelled", TodoStatus::Cancelled),
];

pub fn bad_status() -> String {
    let [(a, _), (b, _), (c, _), (d, _)] = STATUSES;
    format!("{a}, {b}, {c} or {d}")
}

pub fn status_named(name: &str) -> Option<TodoStatus> {
    STATUSES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, status)| *status)
}

pub fn check_count(count: usize, least: usize) -> Result<(), String> {
    if !(least..=MAX_ITEMS).contains(&count) {
        let range = if least == 0 {
            format!("at most {MAX_ITEMS}")
        } else {
            format!("{least} to {MAX_ITEMS}")
        };
        return Err(format!("`todos` has {count} items; give {range}"));
    }
    Ok(())
}

fn check_length(subject: &str, value: &str, limit: usize) -> Result<(), String> {
    let chars = value.chars().count();
    if chars > limit {
        return Err(format!(
            "{subject} is {chars} characters; the limit is {limit}"
        ));
    }
    Ok(())
}

pub fn check_step(subject: &str, text: &str) -> Result<(), String> {
    let text = text.trim();
    if text.is_empty() {
        return Err(format!("{subject} is blank"));
    }
    check_length(subject, text, MAX_TEXT_CHARS)
}

pub fn check_text(text: &str, n: usize) -> Result<(), String> {
    check_step(&format!("item {n}'s `text`"), text)
}

fn section_of(item: &Todo) -> Option<&str> {
    item.section
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
}

fn check_section(section: &str, n: usize) -> Result<(), String> {
    check_length(&format!("item {n}'s `section`"), section, MAX_SECTION_CHARS)?;
    if section.chars().any(char::is_control) {
        return Err(format!(
            "item {n}'s `section` must be one line with no control characters"
        ));
    }
    Ok(())
}

pub fn check(items: &[Todo]) -> Result<(), String> {
    check_count(items.len(), MIN_ITEMS)?;
    for (i, item) in items.iter().enumerate() {
        check_text(&item.text, i + 1)?;
        if let Some(section) = section_of(item) {
            check_section(section, i + 1)?;
        }
    }
    let mut working = items
        .iter()
        .enumerate()
        .filter(|(_, item)| item.status == TodoStatus::InProgress)
        .map(|(i, _)| i + 1);
    if let (Some(first), Some(second)) = (working.next(), working.next()) {
        return Err(format!(
            "items {first} and {second} are both in_progress; at most one item may be in_progress at a time"
        ));
    }
    Ok(())
}

pub fn normal(mut items: Vec<Todo>) -> Vec<Todo> {
    let mut names = Vec::new();
    for item in &mut items {
        item.text = item.text.trim().to_owned();
        item.section = section_of(item).map(str::to_owned);
        if let Some(name) = &item.section
            && !names.contains(name)
        {
            names.push(name.clone());
        }
    }
    items.sort_by_key(|item| {
        item.section
            .as_ref()
            .and_then(|name| names.iter().position(|seen| seen == name))
            .map_or(0, |rank| rank + 1)
    });
    items
}

pub fn completed(items: &[Todo]) -> usize {
    items
        .iter()
        .filter(|item| item.status == TodoStatus::Completed)
        .count()
}

pub fn updated(items: &[Todo]) -> String {
    let done = completed(items);
    let total = items.len();
    match items
        .iter()
        .find(|item| item.status == TodoStatus::InProgress)
    {
        Some(item) => format!(
            "Todos updated: {done} of {total} completed. In progress: \"{}\".",
            item.text
        ),
        None => format!("Todos updated: {done} of {total} completed."),
    }
}

pub fn target(items: &[Todo]) -> String {
    format!("{} of {} completed", completed(items), items.len())
}

#[cfg(test)]
mod tests;
