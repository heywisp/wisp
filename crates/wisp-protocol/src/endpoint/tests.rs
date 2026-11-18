use serde_json::json;

use super::*;
use crate::{AgentEvent, ClientCommand, RejectKind, RequestId};

const KEY: &str = "sk-ant-CANARYKEY0001";
const URL: &str = "https://user:CANARYPASS@gw.example.com/CANARYPATH?t=CANARYQUERY";

fn spec() -> EndpointSpec {
    EndpointSpec {
        name: "manifest".to_owned(),
        provider: "anthropic".to_owned(),
        auth: "bearer".to_owned(),
        url: Some(Secret::new(URL)),
        store: StoreChoice::Auto,
        default: true,
    }
}

#[test]
fn a_save_command_round_trips_and_never_shows_its_secrets_in_debug() {
    let command = ClientCommand::SaveEndpoint {
        request: RequestId(7),
        endpoint: spec(),
        key: Secret::new(KEY),
        check: true,
    };
    let text = serde_json::to_string(&command).unwrap();
    let back: ClientCommand = serde_json::from_str(&text).unwrap();
    assert_eq!(back, command);
    for shown in [format!("{command:?}"), format!("{command:#?}")] {
        for canary in ["CANARYKEY", "CANARYPASS", "CANARYPATH", "CANARYQUERY"] {
            assert!(!shown.contains(canary), "{shown}");
        }
    }
}

#[test]
fn unknown_store_and_check_values_keep_their_text() {
    let store: StoreKind = serde_json::from_value(json!("vault")).unwrap();
    assert_eq!(store, StoreKind::Other("vault".to_owned()));
    assert_eq!(serde_json::to_value(&store).unwrap(), json!("vault"));
    let check: CheckResult =
        serde_json::from_value(json!({"outcome": "rate_limited", "reason": "slow down"})).unwrap();
    assert_eq!(check.outcome, CheckKind::Other("rate_limited".to_owned()));
    assert_eq!(check.reason.as_deref(), Some("slow down"));
    let bare: CheckResult = serde_json::from_value(json!({"outcome": "later"})).unwrap();
    assert_eq!(bare.outcome, CheckKind::Other("later".to_owned()));
    assert_eq!(bare.reason, None);
}

#[test]
fn a_later_reason_arrives_whole_as_other() {
    let why: NotSavedReason = serde_json::from_value(json!({"quota": {"left": 0}})).unwrap();
    assert_eq!(why, NotSavedReason::Other(json!({"quota": {"left": 0}})));
    let known: NotSavedReason =
        serde_json::from_value(json!({"keychain_failed": "The keychain didn't answer."})).unwrap();
    assert_eq!(
        known,
        NotSavedReason::KeychainFailed("The keychain didn't answer.".to_owned())
    );
    let check = NotSavedReason::Check(CheckResult::of(CheckKind::KeyRefused));
    assert_eq!(
        serde_json::to_value(&check).unwrap(),
        json!({"check": {"outcome": "key_refused"}})
    );
}

#[test]
fn an_endpoint_refusal_and_its_catch_all_parse() {
    let refusal = AgentEvent::endpoint_refused(Some(RequestId(3)), SET_UP_FIRST);
    let text = serde_json::to_value(&refusal).unwrap();
    assert_eq!(text["kind"], json!("endpoint"));
    assert_eq!(text["request"], json!(3));
    let later: RejectKind = serde_json::from_value(json!("rate_limited")).unwrap();
    assert_eq!(later, RejectKind::Other("rate_limited".to_owned()));
}

#[test]
fn an_edit_round_trips_and_never_shows_its_secrets_in_debug() {
    let command = ClientCommand::EditEndpoint {
        request: RequestId(8),
        name: "manifest".to_owned(),
        edit: EndpointEdit {
            key: Some(Secret::new(KEY)),
            url: Some(UrlEdit::Set(Secret::new(URL))),
            key_env: VariableName::parse("WORK_API_KEY").map(VariableEdit::Set),
            ..EndpointEdit::default()
        },
        store: Some(StoreChoice::File),
        check: false,
    };
    let text = serde_json::to_string(&command).unwrap();
    let back: ClientCommand = serde_json::from_str(&text).unwrap();
    assert_eq!(back, command);
    for shown in [format!("{command:?}"), format!("{command:#?}")] {
        for canary in ["CANARYKEY", "CANARYPASS", "CANARYPATH", "CANARYQUERY"] {
            assert!(!shown.contains(canary), "{shown}");
        }
    }
}

#[test]
fn an_edit_names_the_fields_it_sets_and_an_empty_one_sets_none() {
    assert!(EndpointEdit::default().is_empty());
    assert!(EndpointEdit::default().fields().is_empty());
    let edit = EndpointEdit {
        provider: Some("anthropic".to_owned()),
        url_env: Some(VariableEdit::Unset),
        ..EndpointEdit::default()
    };
    assert!(!edit.is_empty());
    assert_eq!(
        edit.fields(),
        [ConnectionField::Provider, ConnectionField::UrlEnv]
    );
}

#[test]
fn a_variable_that_is_not_a_name_is_refused_without_echoing_it() {
    let refused = serde_json::from_value::<VariableEdit>(json!({"set": "sk ant CANARY"}))
        .unwrap_err()
        .to_string();
    assert!(!refused.contains("CANARY"), "{refused}");
    let field: ConnectionField = serde_json::from_value(json!("region")).unwrap();
    assert_eq!(field, ConnectionField::Other("region".to_owned()));
    let remove: ClientCommand =
        serde_json::from_value(json!({"type": "remove_endpoint", "request": 3, "name": "gw"}))
            .unwrap();
    assert_eq!(
        remove,
        ClientCommand::RemoveEndpoint {
            request: RequestId(3),
            name: "gw".to_owned(),
            keep_key: false,
        }
    );
}

#[test]
fn loopback_hosts_are_localhost_in_any_case_and_loopback_addresses() {
    for host in ["localhost", "LocalHost", "127.0.0.1", "[::1]"] {
        assert!(loopback_host(host), "{host}");
    }
    for host in ["gw.example.com", "192.0.2.1"] {
        assert!(!loopback_host(host), "{host}");
    }
}
