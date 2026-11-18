use super::*;

#[test]
fn dirty_signals_merge_into_one_draw_per_interval() {
    let start = Duration::from_secs(5);
    let mut f = Frames::default();
    assert_eq!(f.due(start, Rate::Full), None);
    f.mark();
    assert!(f.ready(start, Rate::Full));
    f.drew(start);
    assert!(!f.is_dirty());
    f.mark();
    f.mark();
    assert_eq!(f.due(start, Rate::Full), Some(start + FRAME));
    assert!(!f.ready(start + FRAME / 2, Rate::Full));
    assert!(f.ready(start + FRAME, Rate::Full));
    assert_eq!(f.due(start, Rate::Half), Some(start + 2 * FRAME));
    assert_eq!(
        f.due(start + 3 * FRAME, Rate::Half),
        Some(start + 3 * FRAME)
    );
    f.drew(start + FRAME);
    assert_eq!(f.drawn(), 2);
}

#[test]
fn a_frame_far_in_the_future_never_overflows() {
    let mut f = Frames::default();
    f.drew(Duration::MAX);
    f.mark();
    assert_eq!(f.due(Duration::ZERO, Rate::Half), Some(Duration::MAX));
}
