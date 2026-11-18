use super::*;

#[test]
fn a_lock_error_other_than_would_block_means_locks_are_unsupported() {
    let id = "01a167d6-5e2a-7b31-9c4d-2f6e8a1b3c5d";
    assert_eq!(lock_state(Ok(()), id).unwrap(), LogLock::Held);
    let busy = lock_state(Err(fs::TryLockError::WouldBlock), id).unwrap_err();
    assert_eq!(busy.kind(), io::ErrorKind::WouldBlock);
    assert_eq!(
        busy.to_string(),
        "session 01a167d6-5e2a is open in another Wisp"
    );
    for error in [
        io::Error::from_raw_os_error(37),
        io::Error::from_raw_os_error(95),
        io::Error::from(io::ErrorKind::Unsupported),
    ] {
        let reason = error.to_string();
        assert_eq!(
            lock_state(Err(fs::TryLockError::Error(error)), id).unwrap(),
            LogLock::Unsupported(reason)
        );
    }
}
