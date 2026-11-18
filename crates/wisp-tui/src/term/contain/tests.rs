use super::*;

#[test]
fn records_keep_only_the_newest_panics() {
    for i in 0..(KEPT_PANICS + 4) {
        record(format!("panic {i}"));
    }
    let kept = contained_panics();
    assert!(kept.len() <= KEPT_PANICS);
    assert!(
        kept.iter()
            .any(|m| m == &format!("panic {}", KEPT_PANICS + 3))
    );
}

#[test]
fn work_contained_by_wisp_media_counts_as_contained() {
    assert!(!contained());
    let inside = wisp_media::contain_for_testing(contained);
    assert_eq!(inside, Ok(true));
    assert!(!contained());
}
