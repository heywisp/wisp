use super::*;
use crate::agents::testing::connection;

fn view_with(env_account: Option<&str>) -> EndpointView {
    EndpointView::new(
        &[
            connection("claude-api", None),
            connection("direct", Some(&["claude-haiku-4-5", "claude-ghost-1"])),
        ],
        &[("gpt".into(), "no key in /opt/keys/gpt.env".into())],
        env_account,
    )
}

fn view() -> EndpointView {
    view_with(Some("claude-api"))
}

#[test]
fn the_view_lists_ready_endpoints_by_name_with_their_provider_and_never_a_gone_one() {
    let view = view();
    assert_eq!(
        view.listed(&Gone::new()),
        "claude-api (Claude API), direct (Claude API)"
    );
    assert_eq!(
        view.listed(&Gone::from([("direct".to_owned(), None)])),
        "claude-api (Claude API)"
    );
    assert_eq!(view.unavailable("gpt"), Some("no key in /opt/keys/gpt.env"));
    assert!(view.ready("gpt").is_none());
    assert_eq!(
        view.serving("claude-opus-5-5").map(|r| r.name.as_str()),
        Some("claude-api")
    );
    for (account, shown) in [
        (Some("claude-api"), Some("claude-api")),
        (Some("direct"), Some("direct")),
        (None, None),
    ] {
        assert_eq!(
            view_with(account).ready("env").map(|r| r.name.as_str()),
            shown,
            "{account:?}"
        );
    }
    let gone = EndpointView::new(
        &[connection("direct", None)],
        &[("gateway".into(), "ANTHROPIC_AUTH_TOKEN is not set".into())],
        Some("gateway"),
    );
    assert_eq!(
        gone.unavailable("env"),
        Some("ANTHROPIC_AUTH_TOKEN is not set")
    );
    assert!(gone.ready("env").is_none());
}

#[test]
fn the_served_list_names_table_rows_in_order_and_never_a_model_the_table_lacks() {
    let view = view();
    let direct = view.ready("direct").unwrap();
    assert_eq!(served(direct), ["claude-haiku-4-5"]);
    assert!(!direct.serves("claude-ghost-1"));
    let env = view.ready("claude-api").unwrap();
    let all = served(env);
    assert!(all.contains(&"claude-opus-5-5"), "{all:?}");
}

#[test]
fn a_logged_kind_is_known_unrecorded_or_unknown_and_never_guessed() {
    assert_eq!(
        LoggedKind::read(Some("anthropic")),
        LoggedKind::Known(ProviderKind::Anthropic)
    );
    assert_eq!(LoggedKind::read(None), LoggedKind::Unrecorded);
    assert_eq!(LoggedKind::read(Some("future")), LoggedKind::Unknown);
    assert_eq!(LoggedKind::read(None).shown(), None);
    assert_eq!(LoggedKind::read(Some("future")).shown(), None);
}

#[test]
fn refusals_never_echo_a_key_shaped_or_ill_formed_name() {
    let listed = view().listed(&Gone::new());
    for name in [
        "sk-ant-api03-CANARY0123456789",
        "abcdefghij0123456789xyz",
        "a b",
    ] {
        let text = unknown_text(name, &listed);
        assert!(!text.contains(name), "{text}");
        assert!(
            text.starts_with("no account of that name in this chat"),
            "{text}"
        );
        let text = crate::agents::PresetProblem::ModelNotServed {
            model: "claude-ghost-1".into(),
            endpoint: name.to_owned(),
        }
        .to_string();
        assert!(!text.contains(name), "{text}");
    }
    assert_eq!(
        unknown_text("gtp", &listed),
        "no account \"gtp\" in this chat; accounts: claude-api (Claude API), direct (Claude API)"
    );
}

#[test]
fn an_account_is_a_gateway_exactly_when_the_endpoint_rule_calls_its_url_unofficial() {
    for (url, kind) in [
        ("https://api.anthropic.com", "Claude API"),
        ("https://api.anthropic.com/", "Claude API"),
        ("https://gw.example.com", "Gateway, API"),
        ("not a url", "Gateway, API"),
    ] {
        let mut gw = connection("gw", None);
        gw.context.endpoint = url.into();
        assert_eq!(connection_kind(&gw), kind, "{url}");
        assert_eq!(
            connection_kind(&gw) == "Gateway, API",
            !crate::endpoints::is_official(ProviderKind::Anthropic, url),
            "{url}"
        );
        let view = EndpointView::new(&[gw], &[], None);
        assert_eq!(view.listed(&Gone::new()), format!("gw ({kind})"));
    }
}

#[test]
fn account_kind_names_gemini_api_antigravity_subscription_and_gateways() {
    assert_eq!(
        account_kind(ProviderKind::Gemini, wisp_protocol::Billing::Metered, false),
        "Gemini API"
    );
    assert_eq!(
        account_kind(ProviderKind::Gemini, wisp_protocol::Billing::Metered, true),
        "Gateway, API"
    );
    assert_eq!(
        account_kind(
            ProviderKind::Gemini,
            wisp_protocol::Billing::AntigravityPlan,
            false
        ),
        "Antigravity subscription"
    );
    assert_eq!(
        LoggedKind::read(Some("gemini")),
        LoggedKind::Known(ProviderKind::Gemini)
    );
}
