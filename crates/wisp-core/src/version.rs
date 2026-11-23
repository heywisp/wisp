use std::sync::OnceLock;

static COMMIT: OnceLock<String> = OnceLock::new();

pub fn set_commit(commit: &str) {
    let trimmed = commit.trim();
    if !trimmed.is_empty() {
        let _ = COMMIT.set(trimmed.to_owned());
    }
}

#[must_use]
pub fn wisp_version() -> String {
    let base = env!("CARGO_PKG_VERSION");
    match COMMIT.get() {
        Some(commit) => format!("{base}+{commit}"),
        None => base.to_owned(),
    }
}
