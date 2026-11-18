use std::fs::{File, TryLockError};

pub(crate) fn try_lock(file: &File) -> Result<(), TryLockError> {
    file.try_lock()?;
    #[cfg(feature = "test-support")]
    copies::keep(file);
    Ok(())
}

#[cfg(feature = "test-support")]
pub mod copies {
    use std::cell::RefCell;
    use std::fs::File;

    thread_local! {
        static KEPT: RefCell<Option<Vec<File>>> = const { RefCell::new(None) };
    }

    pub fn keep_from_now() {
        KEPT.with(|kept| *kept.borrow_mut() = Some(Vec::new()));
    }

    pub fn take() -> Vec<File> {
        KEPT.with(|kept| kept.borrow_mut().take().unwrap_or_default())
    }

    pub(super) fn keep(file: &File) {
        KEPT.with(|kept| {
            if let (Some(kept), Ok(copy)) = (kept.borrow_mut().as_mut(), file.try_clone()) {
                kept.push(copy);
            }
        });
    }
}
