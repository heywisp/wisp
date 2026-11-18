use super::*;

#[test]
fn an_automatic_gate_is_at_the_ceiling_from_the_ceiling_on() {
    let due = Due {
        window: 200_000,
        point: 2_000,
        ceiling: 150_000,
    };
    assert!(Gate::auto(150_000, due).ceiling);
    assert!(!Gate::auto(149_999, due).ceiling);
}
