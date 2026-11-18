use std::path::Path;

use futures::FutureExt;
use wisp_protocol::settings::{
    AUTH_ANTHROPIC_SUBSCRIPTION, SettingValue, Target, find, parse_target,
};

use super::*;
use crate::endpoints::{ConfigFile, PathShown};
use crate::settings::Given;

const ENV: &str = "env";

fn limited(limit: u32) -> Lanes {
    Lanes::new(BTreeMap::from([(ENV.to_owned(), Some(limit))]), None)
}

fn admitted(lanes: &Lanes, depth: u32) -> Pass {
    match lanes.enter(ENV, depth) {
        Ok(pass) => pass,
        Err(waiter) => panic!("{} ahead", waiter.ahead()),
    }
}

fn waiting(lanes: &Lanes, depth: u32) -> Waiter {
    match lanes.enter(ENV, depth) {
        Ok(_) => panic!("admitted at once"),
        Err(waiter) => waiter,
    }
}

fn now(waiter: &mut Waiter) -> Option<Pass> {
    let shown = waiter.ahead();
    match waiter.admitted(shown).now_or_never() {
        Some(Ok(pass)) => Some(pass),
        Some(Err(_)) | None => None,
    }
}

#[test]
fn with_a_limit_of_two_the_third_waits_until_one_ends() {
    let lanes = limited(2);
    let first = admitted(&lanes, 0);
    let _second = admitted(&lanes, 0);
    let mut third = waiting(&lanes, 0);
    assert_eq!(third.ahead(), 0);
    assert!(now(&mut third).is_none());
    assert_eq!(lanes.in_flight(ENV), 2);
    drop(first);
    let pass = now(&mut third);
    assert!(pass.is_some());
    assert_eq!(lanes.in_flight(ENV), 2);
    assert_eq!(lanes.waiting(ENV), 0);
    drop(pass);
    assert_eq!(lanes.in_flight(ENV), 1);
}

#[test]
fn the_main_agent_goes_first_and_each_depth_keeps_its_order() {
    let lanes = limited(1);
    let held = admitted(&lanes, 1);
    let mut deep_first = waiting(&lanes, 2);
    let mut deep_second = waiting(&lanes, 2);
    let mut child = waiting(&lanes, 1);
    let mut main = waiting(&lanes, 0);
    assert_eq!(
        [&deep_first, &deep_second, &child, &main].map(Waiter::ahead),
        [2, 3, 1, 0]
    );
    drop(held);
    assert!(now(&mut deep_first).is_none());
    assert!(now(&mut child).is_none());
    let main_pass = now(&mut main).unwrap();
    drop(main_pass);
    assert!(now(&mut deep_first).is_none());
    let child_pass = now(&mut child).unwrap();
    drop(child_pass);
    assert!(now(&mut deep_second).is_none());
    assert!(now(&mut deep_first).is_some());
}

#[tokio::test(start_paused = true)]
async fn a_background_request_ranks_at_its_agents_depth() {
    let lanes = limited(1);
    let held = admitted(&lanes, 2);
    let deep = waiting(&lanes, 2);
    let background = tokio::spawn({
        let lanes = lanes.clone();
        async move {
            let _pass = lanes.pass(ENV, 0).await;
        }
    });
    tokio::task::yield_now().await;
    assert_eq!(deep.ahead(), 1);
    assert_eq!(lanes.waiting(ENV), 2);
    drop(held);
    background.await.unwrap();
    assert_eq!(deep.ahead(), 0);
    let mut deep = deep;
    assert!(now(&mut deep).is_some());
}

#[tokio::test(start_paused = true)]
async fn a_dropped_waiter_leaves_and_those_behind_move_up() {
    let lanes = limited(1);
    let held = admitted(&lanes, 0);
    let gone = waiting(&lanes, 0);
    let mut last = waiting(&lanes, 0);
    assert_eq!(last.ahead(), 1);
    let moved = tokio::spawn(async move {
        let ahead = last.admitted(1).await.err();
        (ahead, last)
    });
    tokio::task::yield_now().await;
    drop(gone);
    let (ahead, mut last) = moved.await.unwrap();
    assert_eq!(ahead, Some(0));
    assert_eq!(lanes.waiting(ENV), 1);
    drop(held);
    assert!(now(&mut last).is_some());
}

#[test]
fn none_never_waits() {
    let lanes = Lanes::new(BTreeMap::from([(ENV.to_owned(), None)]), Some(1));
    let passes: Vec<Pass> = (0..100).map(|_| admitted(&lanes, 2)).collect();
    assert_eq!(passes.len(), 100);
    assert_eq!(lanes.in_flight(ENV), 0);
    let default = Lanes::default();
    let _many: Vec<Pass> = (0..100).map(|_| admitted(&default, 0)).collect();
    assert_eq!(default.limit(ENV), None);
}

#[test]
fn an_unknown_name_gets_the_default() {
    let subscription = CredentialKind::Other(AUTH_ANTHROPIC_SUBSCRIPTION.to_owned());
    let lanes = Lanes::resolve(
        &Layers::default(),
        [(ENV, None), ("claude", Some(&subscription))],
        "claude-opus-5-5",
    );
    assert_eq!(lanes.limit(ENV), Some(8));
    assert_eq!(lanes.limit("claude"), Some(12));
    assert_eq!(lanes.limit("added-later"), Some(8));
    let _held: Vec<Pass> = (0..8)
        .map(|_| match lanes.enter("added-later", 0) {
            Ok(pass) => pass,
            Err(_) => panic!("waited below the default"),
        })
        .collect();
    assert!(lanes.enter("added-later", 0).is_err());
}

fn parse(text: &str) -> ConfigFile {
    ConfigFile::parse(Path::new("/h/config.toml"), PathShown::Default, text).unwrap()
}

fn flag(path: &str, value: &str) -> Given {
    let spec = find(names::MAX_IN_FLIGHT).unwrap();
    Given {
        target: parse_target(path).unwrap(),
        value: spec.parse(value).unwrap(),
        origin: format!("--set {path}={value}"),
    }
}

#[test]
fn each_endpoint_reads_its_own_limit_from_the_file_and_the_flags() {
    let file = parse(
        "[endpoints.env]\nmax_in_flight = 4\n\n[endpoints.gw]\nurl = \"https://gw.example.com\"\nauth = \"bearer\"\nkey_env = \"GW_KEY\"\nmax_in_flight = \"none\"\n\n[endpoints.other]\nurl = \"https://o.example.com\"\nauth = \"bearer\"\nkey_env = \"O_KEY\"\nmax_in_flight = 3\n",
    );
    let saved = Layers {
        saved: Some(&file.settings),
        ..Layers::default()
    };
    let subscription = CredentialKind::Other(AUTH_ANTHROPIC_SUBSCRIPTION.to_owned());
    let kinds = [
        (ENV, Some(&subscription)),
        ("gw", Some(&subscription)),
        ("other", None),
        ("plain", Some(&subscription)),
    ];
    let lanes = Lanes::resolve(&saved, kinds, "claude-opus-5-5");
    assert_eq!(
        kinds.map(|(e, _)| lanes.limit(e)),
        [Some(4), None, Some(3), Some(12)]
    );
    let flagged = Layers {
        flags: vec![flag("endpoints.env.max_in_flight", "2")],
        saved: Some(&file.settings),
        ..Layers::default()
    };
    let lanes = Lanes::resolve(&flagged, [(ENV, None), ("other", None)], "claude-opus-5-5");
    assert_eq!(lanes.limit(ENV), Some(2));
    assert_eq!(lanes.limit("other"), Some(3));
}

#[test]
fn a_flag_out_of_range_is_refused() {
    let spec = find(names::MAX_IN_FLIGHT).unwrap();
    assert_eq!(spec.parse("1"), Ok(SettingValue::Int(1)));
    assert_eq!(spec.parse("64"), Ok(SettingValue::Int(64)));
    assert_eq!(spec.parse("none"), Ok(SettingValue::Text("none".into())));
    for refused in ["0", "65", "-1", "many"] {
        assert!(spec.parse(refused).is_err(), "{refused}");
    }
    assert!(matches!(
        parse_target("endpoints.env.max_in_flight"),
        Ok(Target::Path(_))
    ));
}
