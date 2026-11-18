use super::*;

const FAST: Duration = Duration::from_millis(5);
const SLOW: Duration = Duration::from_millis(51);

#[test]
fn a_slow_write_halves_the_rate_until_writes_recover() {
    let mut pace = Pace::new();
    pace.record(FAST);
    pace.record(SLOW_WRITE);
    assert_eq!(pace.rate(), Rate::Full);
    pace.record(SLOW);
    assert_eq!(pace.rate(), Rate::Half);
    assert_eq!(
        pace.rate().interval(Duration::from_millis(33)),
        Duration::from_millis(66)
    );
    pace.record(SLOW);
    assert_eq!(pace.rate(), Rate::Half);
    for _ in 1..RECOVER_AFTER {
        pace.record(FAST);
        assert_eq!(pace.rate(), Rate::Half);
    }
    pace.record(FAST);
    assert_eq!(pace.rate(), Rate::Full);
    assert_eq!(pace.rate().divisor(), 1);
}

#[test]
fn a_slow_write_during_recovery_starts_it_again() {
    let mut pace = Pace::new();
    pace.record(SLOW);
    for _ in 1..RECOVER_AFTER {
        pace.record(FAST);
    }
    pace.record(SLOW);
    for _ in 1..RECOVER_AFTER {
        pace.record(FAST);
    }
    assert_eq!(pace.rate(), Rate::Half);
    pace.record(FAST);
    assert_eq!(pace.rate(), Rate::Full);
}
