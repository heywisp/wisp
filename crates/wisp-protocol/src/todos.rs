use serde::{Deserialize, Serialize};

use crate::log::{LogEvent, LogRecord, rewind_mask};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

impl TodoStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    pub text: String,
    pub status: TodoStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoProgress {
    pub completed: usize,
    pub total: usize,
}

pub fn progress(items: &[TodoItem]) -> TodoProgress {
    TodoProgress {
        completed: items
            .iter()
            .filter(|i| i.status == TodoStatus::Completed)
            .count(),
        total: items.len(),
    }
}

pub fn in_progress(items: &[TodoItem]) -> Option<&TodoItem> {
    items.iter().find(|i| i.status == TodoStatus::InProgress)
}

pub fn open(items: &[TodoItem]) -> bool {
    items
        .iter()
        .any(|i| matches!(i.status, TodoStatus::Pending | TodoStatus::InProgress))
}

pub fn checklist<'a>(records: impl IntoIterator<Item = &'a LogRecord>) -> Vec<TodoItem> {
    let keyed: Vec<(u64, &LogEvent)> = records.into_iter().map(|r| (r.seq, &r.event)).collect();
    let masked = rewind_mask(&keyed).unwrap_or_else(|_| vec![false; keyed.len()]);
    keyed
        .iter()
        .zip(masked)
        .rev()
        .filter(|(_, masked)| !masked)
        .find_map(|((_, event), _)| listed(event))
        .map(<[TodoItem]>::to_vec)
        .unwrap_or_default()
}

pub fn listed(event: &LogEvent) -> Option<&[TodoItem]> {
    match event {
        LogEvent::TodosUpdated { items, .. } => Some(items),
        LogEvent::SessionStarted {
            parent: Some(parent),
            ..
        } => Some(&parent.todos),
        _ => None,
    }
}
