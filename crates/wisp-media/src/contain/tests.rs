use std::cell::Cell;
use std::sync::Once;

use super::*;

thread_local! {
    static SEEN: Cell<Option<bool>> = const { Cell::new(None) };
}

fn watch_panics() {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            SEEN.with(|seen| seen.set(Some(panic_is_contained())));
            previous(info);
        }));
    });
}

#[test]
fn the_flag_is_set_only_inside_contain_and_nests() {
    assert!(!panic_is_contained());
    let outer = contain(|| {
        assert!(panic_is_contained());
        let inner = contain(|| Ok(panic_is_contained()));
        assert_eq!(inner, Ok(true));
        Ok(panic_is_contained())
    });
    assert_eq!(outer, Ok(true));
    assert!(!panic_is_contained());
}

#[test]
fn a_decoder_panic_is_contained_seen_by_the_hook_and_becomes_an_error() {
    watch_panics();
    SEEN.with(|seen| seen.set(None));
    let caught: Result<u8, MediaError> = contain(|| panic!("decoder blew up"));
    assert_eq!(caught, Err(MediaError::Panicked));
    assert_eq!(SEEN.with(Cell::get), Some(true));
    assert!(!panic_is_contained());
}

#[test]
fn errors_pass_through_unchanged() {
    let failed: Result<u8, MediaError> = contain(|| Err(MediaError::Malformed));
    assert_eq!(failed, Err(MediaError::Malformed));
    assert!(!panic_is_contained());
}

#[test]
fn the_flag_is_per_thread() {
    let seen_elsewhere = contain(|| {
        std::thread::spawn(panic_is_contained)
            .join()
            .map_err(|_| MediaError::Panicked)
    });
    assert_eq!(seen_elsewhere, Ok(false));
}
