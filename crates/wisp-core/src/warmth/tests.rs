use std::time::Duration;

use super::*;

fn bytes() -> PrefixBytes {
    PrefixBytes {
        model: br#""claude-opus-4-1""#.to_vec(),
        effort: br#"{"effort":"high"}"#.to_vec(),
        thinking: br#"{"type":"adaptive","display":"summarized"}"#.to_vec(),
        tools: br#"[{"name":"bash"}]"#.to_vec(),
        system: br#"[{"type":"text","text":"You are Wisp."}]"#.to_vec(),
    }
}

fn print() -> Fingerprint {
    Fingerprint::new("env", &bytes())
}

fn with(component: Component) -> Fingerprint {
    let mut bytes = bytes();
    let mut endpoint = "env";
    match component {
        Component::Endpoint => endpoint = "manifest",
        Component::Model => bytes.model = br#""claude-sonnet-4-5""#.to_vec(),
        Component::Effort => bytes.effort = br#"{"effort":"low"}"#.to_vec(),
        Component::Thinking => {
            bytes.thinking = br#"{"type":"adaptive","display":"summarized","block_binding":{"prefix_mismatch_behavior":"drop_block"}}"#.to_vec();
        }
        Component::Tools => bytes.tools = br#"[{"name":"view_file"}]"#.to_vec(),
        Component::System => {
            bytes.system = br#"[{"type":"text","text":"You are Jarvis."}]"#.to_vec();
        }
    }
    Fingerprint::new(endpoint, &bytes)
}

#[test]
fn equal_prefixes_give_one_key() {
    assert_eq!(print(), print());
    assert_eq!(print().key(), print().key());
    assert_eq!(print().key().as_str().len(), 64);
    assert!(print().changed(&print()).is_empty());
}

#[test]
fn each_component_changes_the_key_and_is_named() {
    let base = print();
    let mut keys = vec![base.key().clone()];
    for component in Component::ALL {
        let changed = with(component);
        assert_ne!(changed.key(), base.key(), "{}", component.as_str());
        assert_eq!(changed.changed(&base), vec![component]);
        assert_ne!(changed.part(component), base.part(component));
        keys.push(changed.key().clone());
    }
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), Component::ALL.len() + 1);
}

#[test]
fn bytes_moved_between_components_change_the_key() {
    let mut moved = bytes();
    moved.tools = br#"[{"name":"bash"}][{"type":"text","text":"You are Wisp."}]"#.to_vec();
    moved.system = Vec::new();
    assert_ne!(Fingerprint::new("env", &moved).key(), print().key());
}

#[test]
fn the_key_is_pinned() {
    assert_eq!(
        print().key().as_str(),
        sha256_hex(
            &Component::ALL
                .iter()
                .map(|c| format!("{}={}\n", c.as_str(), print().part(*c)))
                .collect::<String>()
        )
    );
    assert_eq!(print().part(Component::Endpoint), sha256_hex("env"));
    assert_eq!(
        print().part(Component::Tools),
        sha256_hex(r#"[{"name":"bash"}]"#)
    );
}

#[tokio::test(start_paused = true)]
async fn a_key_warms_at_its_first_stream_event() {
    let warmth = WarmthTable::default();
    let key = print().key().clone();
    assert_eq!(warmth.heat(&key), Heat::Cold);
    let mut ticket = warmth.sent(print());
    assert_eq!(ticket.fingerprint(), &print());
    assert_eq!(warmth.heat(&key), Heat::Warming);
    tokio::time::advance(Duration::from_secs(3)).await;
    assert_eq!(warmth.heat(&key), Heat::Warming);
    ticket.streamed();
    assert_eq!(warmth.heat(&key), Heat::Warm);
    drop(ticket);
    assert_eq!(warmth.heat(&key), Heat::Warm);
    assert_eq!(warmth.heat(with(Component::Tools).key()), Heat::Cold);
}

#[tokio::test(start_paused = true)]
async fn a_failed_first_request_leaves_the_key_cold() {
    let warmth = WarmthTable::default();
    let key = print().key().clone();
    let ticket = warmth.sent(print());
    assert_eq!(warmth.heat(&key), Heat::Warming);
    drop(ticket);
    assert_eq!(warmth.heat(&key), Heat::Cold);
    let mut again = warmth.sent(print());
    assert_eq!(warmth.heat(&key), Heat::Warming);
    again.streamed();
    assert_eq!(warmth.heat(&key), Heat::Warm);
}

#[tokio::test(start_paused = true)]
async fn a_failed_later_request_keeps_a_warm_key_warm() {
    let warmth = WarmthTable::default();
    let key = print().key().clone();
    warmth.sent(print()).streamed();
    drop(warmth.sent(print()));
    assert_eq!(warmth.heat(&key), Heat::Warm);
}

#[tokio::test(start_paused = true)]
async fn only_the_warming_request_failing_cools_the_key() {
    let warmth = WarmthTable::default();
    let key = print().key().clone();
    let first = warmth.sent(print());
    let mut second = warmth.sent(print());
    assert_eq!(warmth.heat(&key), Heat::Warming);
    drop(first);
    assert_eq!(warmth.heat(&key), Heat::Cold);
    let third = warmth.sent(print());
    second.streamed();
    assert_eq!(warmth.heat(&key), Heat::Warm);
    drop(third);
    assert_eq!(warmth.heat(&key), Heat::Warm);
}

#[tokio::test(start_paused = true)]
async fn a_second_request_streaming_first_warms_the_key() {
    let warmth = WarmthTable::default();
    let key = print().key().clone();
    let first = warmth.sent(print());
    let mut second = warmth.sent(print());
    second.streamed();
    assert_eq!(warmth.heat(&key), Heat::Warm);
    drop(first);
    assert_eq!(warmth.heat(&key), Heat::Warm);
}

#[tokio::test(start_paused = true)]
async fn a_key_stays_warm_for_an_hour_after_its_latest_request() {
    let warmth = WarmthTable::default();
    let key = print().key().clone();
    warmth.sent(print()).streamed();
    tokio::time::advance(WARM_FOR - Duration::from_secs(60)).await;
    assert_eq!(warmth.heat(&key), Heat::Warm);
    let mut later = warmth.sent(print());
    assert_eq!(warmth.heat(&key), Heat::Warm);
    tokio::time::advance(Duration::from_secs(120)).await;
    later.streamed();
    assert_eq!(warmth.heat(&key), Heat::Warm);
    tokio::time::advance(WARM_FOR - Duration::from_secs(121)).await;
    assert_eq!(warmth.heat(&key), Heat::Warm);
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(warmth.heat(&key), Heat::Cold);
    let ticket = warmth.sent(print());
    assert_eq!(warmth.heat(&key), Heat::Warming);
    drop(ticket);
    assert_eq!(warmth.heat(&key), Heat::Cold);
}

#[tokio::test(start_paused = true)]
async fn warmth_counts_from_the_send_not_the_stream() {
    let warmth = WarmthTable::default();
    let key = print().key().clone();
    let mut ticket = warmth.sent(print());
    tokio::time::advance(Duration::from_secs(5)).await;
    ticket.streamed();
    tokio::time::advance(WARM_FOR - Duration::from_secs(5)).await;
    assert_eq!(warmth.heat(&key), Heat::Cold);
}

#[tokio::test(start_paused = true)]
async fn a_warming_key_never_cools_on_its_own() {
    let warmth = WarmthTable::default();
    let key = print().key().clone();
    let _ticket = warmth.sent(print());
    tokio::time::advance(WARM_FOR * 2).await;
    assert_eq!(warmth.heat(&key), Heat::Warming);
}

#[tokio::test(start_paused = true)]
async fn clones_share_one_table() {
    let warmth = WarmthTable::default();
    let other = warmth.clone();
    warmth.sent(print()).streamed();
    assert_eq!(other.heat(print().key()), Heat::Warm);
    assert_eq!(WarmthTable::default().heat(print().key()), Heat::Cold);
}

#[tokio::test(start_paused = true)]
async fn cold_keys_are_dropped_from_the_table() {
    let warmth = WarmthTable::default();
    warmth.sent(print()).streamed();
    drop(warmth.sent(with(Component::Model)));
    tokio::time::advance(WARM_FOR).await;
    warmth.sent(with(Component::Tools)).streamed();
    assert_eq!(warmth.heats(), vec![Heat::Warm]);
}

#[tokio::test(start_paused = true)]
async fn a_failed_request_does_not_extend_warmth() {
    let warmth = WarmthTable::default();
    let key = print().key().clone();
    warmth.sent(print()).streamed();
    tokio::time::advance(Duration::from_secs(50 * 60)).await;
    drop(warmth.sent(print()));
    assert_eq!(warmth.heat(&key), Heat::Warm);
    tokio::time::advance(Duration::from_secs(11 * 60)).await;
    assert_eq!(warmth.heat(&key), Heat::Cold);
}

#[tokio::test(start_paused = true)]
async fn a_wait_on_a_warming_key_ends_at_its_first_stream_event() {
    let table = WarmthTable::default();
    let mut first = table.sent(print());
    let wait = tokio::spawn(table.clone().cooled(print().key().clone(), STAGGER_FOR));
    let started = Instant::now();
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(!wait.is_finished());
    first.streamed();
    wait.await.unwrap();
    assert_eq!(started.elapsed(), Duration::from_secs(1));
}

#[tokio::test(start_paused = true)]
async fn a_wait_ends_when_the_warming_request_fails() {
    let table = WarmthTable::default();
    let first = table.sent(print());
    let wait = tokio::spawn(table.clone().cooled(print().key().clone(), STAGGER_FOR));
    let started = Instant::now();
    tokio::time::sleep(Duration::from_millis(300)).await;
    drop(first);
    wait.await.unwrap();
    assert_eq!(started.elapsed(), Duration::from_millis(300));
    assert_eq!(table.heat(print().key()), Heat::Cold);
}

#[tokio::test(start_paused = true)]
async fn a_wait_gives_up_after_five_seconds_and_never_waits_on_a_cold_or_warm_key() {
    let table = WarmthTable::default();
    let started = Instant::now();
    table
        .clone()
        .cooled(print().key().clone(), STAGGER_FOR)
        .await;
    assert_eq!(started.elapsed(), Duration::ZERO);
    let first = table.sent(print());
    table
        .clone()
        .cooled(print().key().clone(), STAGGER_FOR)
        .await;
    assert_eq!(started.elapsed(), STAGGER_FOR);
    let mut first = first;
    first.streamed();
    let again = Instant::now();
    table
        .clone()
        .cooled(print().key().clone(), STAGGER_FOR)
        .await;
    assert_eq!(again.elapsed(), Duration::ZERO);
}

#[tokio::test(start_paused = true)]
async fn a_claim_refuses_a_warming_key_and_a_released_waiter_claims_it_again() {
    let table = WarmthTable::default();
    let first = table.claim(print()).unwrap();
    assert!(table.claim(print()).is_err());
    let waiters = [
        tokio::spawn(table.clone().claim_within(print(), STAGGER_FOR)),
        tokio::spawn(table.clone().claim_within(print(), STAGGER_FOR)),
    ];
    let started = Instant::now();
    tokio::time::sleep(Duration::from_secs(1)).await;
    drop(first);
    tokio::time::sleep(Duration::from_millis(10)).await;
    let done: Vec<bool> = waiters.iter().map(|w| w.is_finished()).collect();
    assert_eq!(done.iter().filter(|d| **d).count(), 1, "{done:?}");
    assert_eq!(table.heat(print().key()), Heat::Warming);
    let [one, two] = waiters;
    let (mut warmer, other) = if one.is_finished() {
        (one.await.unwrap(), two)
    } else {
        (two.await.unwrap(), one)
    };
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(!other.is_finished());
    warmer.streamed();
    let later = other.await.unwrap();
    assert!(started.elapsed() < STAGGER_FOR);
    drop(later);
    let stuck = table.claim(with(Component::Tools)).unwrap();
    let given_up = Instant::now();
    let late = table
        .clone()
        .claim_within(with(Component::Tools), STAGGER_FOR)
        .await;
    assert_eq!(given_up.elapsed(), STAGGER_FOR);
    drop((stuck, late));
}
