use nix::sys::signal::{SigSet, Signal};

use super::TtouHeld;

fn ttou_blocked() -> bool {
    SigSet::thread_get_mask().unwrap().contains(Signal::SIGTTOU)
}

#[test]
fn holding_sigttou_blocks_it_until_dropped_and_restores_the_previous_mask() {
    std::thread::spawn(|| {
        assert!(!ttou_blocked());
        let held = TtouHeld::hold();
        assert!(ttou_blocked());
        let nested = TtouHeld::hold();
        drop(nested);
        assert!(ttou_blocked());
        drop(held);
        assert!(!ttou_blocked());
    })
    .join()
    .unwrap();
}
