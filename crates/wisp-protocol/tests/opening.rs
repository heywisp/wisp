mod support;

use wisp_protocol::Opening;

#[test]
fn an_opening_round_trips_and_a_running_chat_omits_its_start_notes() {
    let mut opening: Opening =
        serde_json::from_str(&support::fixture_text("opening.json")).unwrap();
    support::golden("opening", &opening);
    opening.start = None;
    let value = serde_json::to_value(&opening).unwrap();
    assert!(value.get("start").is_none());
    assert_eq!(serde_json::from_value::<Opening>(value).unwrap(), opening);
}
