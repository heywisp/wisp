use std::sync::{Mutex, PoisonError};

pub use wisp_contain::contain;

pub const KEPT_PANICS: usize = 16;

static RECORDED: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub fn contained() -> bool {
    wisp_contain::is_contained() || wisp_media::panic_is_contained()
}

pub(super) fn record(message: String) {
    let mut kept = RECORDED.lock().unwrap_or_else(PoisonError::into_inner);
    if kept.len() >= KEPT_PANICS {
        kept.remove(0);
    }
    kept.push(message);
}

pub fn contained_panics() -> Vec<String> {
    RECORDED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

#[cfg(test)]
mod tests;
