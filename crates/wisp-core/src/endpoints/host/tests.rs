use wisp_protocol::endpoint::StoreChoice;

use super::*;

fn spec(name: &str, url: Option<&str>) -> EndpointSpec {
    EndpointSpec {
        name: name.to_owned(),
        provider: "anthropic".to_owned(),
        auth: "api_key".to_owned(),
        url: url.map(Secret::new),
        store: StoreChoice::Auto,
        default: false,
    }
}

#[test]
fn a_good_spec_passes_and_names_its_provider() {
    let key = Secret::new("sk-ant-CANARYKEY0001");
    assert_eq!(
        check_spec(&spec("work", Some("https://gw.example.com/v1")), &key),
        Ok(ProviderKind::Anthropic)
    );
}

#[test]
fn every_refusal_is_worded_without_echoing_what_was_sent() {
    let key = Secret::new("sk-ant-CANARYKEY0001");
    let cases = [
        (spec("CANARY-Name", None), key.clone()),
        (spec("env", None), key.clone()),
        (spec("work", Some("ftp://CANARYHOST/x")), key.clone()),
        (spec("work", None), Secret::new("sk-ant CANARYKEY")),
        (
            EndpointSpec {
                provider: "CANARYPROVIDER".to_owned(),
                ..spec("work", None)
            },
            key.clone(),
        ),
        (
            EndpointSpec {
                auth: "CANARYAUTH".to_owned(),
                ..spec("work", None)
            },
            key.clone(),
        ),
    ];
    for (spec, key) in cases {
        let Err(reason) = check_spec(&spec, &key) else {
            panic!("{spec:?} passed");
        };
        assert!(!reason.contains("CANARY"), "{reason}");
    }
    assert_eq!(
        check_spec(&spec("env", None), &key),
        Err("that name is reserved for the keys-file account; choose another".to_owned())
    );
}

#[test]
fn the_save_worst_case_is_probe_lock_and_keychain() {
    assert_eq!(SAVE_WORST, Duration::from_secs(22));
}

#[test]
fn auth_alias_of_names_the_alias_each_command_changes() {
    use wisp_protocol::RequestId;
    use wisp_protocol::subscription_auth::{AuthCommand, SubscriptionProvider};
    let request = RequestId(1);
    let start = |alias: Option<&str>, reauthorize| AuthCommand::Start {
        request,
        provider: SubscriptionProvider::ChatGpt,
        alias: alias.map(str::to_owned),
        reauthorize,
    };
    let plan = || "plan".to_owned();
    for (command, expected) in [
        (start(Some("plan"), true), Some((plan(), true))),
        (start(Some("plan"), false), Some((plan(), false))),
        (start(None, true), None),
        (
            crate::endpoints::testing::logout(request.0, "plan"),
            Some((plan(), true)),
        ),
        (
            AuthCommand::Reload {
                request,
                alias: plan(),
            },
            Some((plan(), false)),
        ),
        (
            AuthCommand::Status {
                request,
                alias: Some(plan()),
            },
            None,
        ),
        (
            AuthCommand::Finish {
                request,
                login: request,
                response: None,
                alias: Some(plan()),
            },
            None,
        ),
    ] {
        assert_eq!(auth_alias_of(&command), expected);
    }
}

#[test]
fn oauth_only_subscriptions_are_refused_in_check_spec_with_their_login_hint() {
    let key = Secret::new("AIzaCANARY_gemini_key_with_underscre_1");
    assert_eq!(
        check_spec(
            &EndpointSpec {
                provider: "gemini".to_owned(),
                auth: "api_key".to_owned(),
                ..spec("gemini-api", None)
            },
            &key,
        ),
        Ok(ProviderKind::Gemini)
    );
    assert_eq!(
        check_spec(
            &EndpointSpec {
                provider: "gemini".to_owned(),
                auth: "antigravity_subscription".to_owned(),
                ..spec("antigravity", None)
            },
            &key,
        ),
        Err("Antigravity subscription sign-in uses wisp login antigravity".to_owned())
    );
    assert_eq!(
        check_spec(
            &EndpointSpec {
                provider: "openai".to_owned(),
                auth: "chatgpt_subscription".to_owned(),
                ..spec("chatgpt", None)
            },
            &key,
        ),
        Err("ChatGPT subscription sign-in uses wisp login chatgpt".to_owned())
    );
}
