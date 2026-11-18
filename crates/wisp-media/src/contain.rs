use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::MediaError;

thread_local! {
    static CONTAINED: Cell<bool> = const { Cell::new(false) };
}

pub fn panic_is_contained() -> bool {
    CONTAINED.with(Cell::get)
}

pub(crate) fn contain<R>(work: impl FnOnce() -> Result<R, MediaError>) -> Result<R, MediaError> {
    let outer = CONTAINED.with(|c| c.replace(true));
    let out = catch_unwind(AssertUnwindSafe(work));
    CONTAINED.with(|c| c.set(outer));
    out.unwrap_or(Err(MediaError::Panicked))
}

#[cfg(test)]
mod tests;
