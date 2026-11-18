use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HistoryChange {
    SearchesAsText {
        thinking_cut: bool,
    },
    ThinkingRefused,
    ThinkingDropped {
        count: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    AttachmentsAsNotes {
        model: String,
        images: usize,
        files: usize,
    },
}
