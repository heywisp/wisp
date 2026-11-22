use super::api_key;
use super::subscription::credential;
use crate::clock::{Clock, StillClock};
use crate::endpoints::{EndpointName, PricingMode, ProviderKind, ResolvedEndpoint, UrlSource};
use crate::models::list::tests::{claude_entry, claude_wire};
use crate::models::list::{REFRESH_AFTER, SavedList, anthropic};
use crate::provider::anthropic::{AnthropicProvider, Credential, DEFAULT_BASE_URL, Source};
use crate::provider::credential::SubscriptionKind;
use crate::provider::{Provider, ProviderError};
use crate::test_guard::http;
use wisp_protocol::settings::{AccountModel, ClientVersion};

pub(super) fn account(credential: Credential, base_url: &str) -> ResolvedEndpoint {
    ResolvedEndpoint {
        client_version: None,
        name: EndpointName::parse("claude-test").unwrap(),
        provider: ProviderKind::Anthropic,
        credential,
        base_url: base_url.into(),
        source: Source::Keyring,
        key_env: None,
        url_source: UrlSource::Official,
        pricing: PricingMode::List,
    }
}

fn profiled() -> Credential {
    Credential::Subscription {
        kind: SubscriptionKind::Anthropic,
        secret: "synthetic-oauth".into(),
        profile: Some("synthetic-profile".into()),
    }
}

fn connect(credential: Credential, base_url: &str) -> AnthropicProvider {
    AnthropicProvider::from_endpoint(&account(credential, base_url)).unwrap()
}

async fn built(provider: &AnthropicProvider, version: Option<ClientVersion>) -> reqwest::Request {
    let subscription = match version {
        Some(version) => Some((
            version,
            super::super::bearer(&provider.credential).await.unwrap(),
        )),
        None => None,
    };
    let request = provider.list_request(subscription).build().unwrap();
    assert_eq!(request.method(), "GET");
    request
}

fn named<'a>(request: &'a reqwest::Request, name: &str) -> Option<&'a str> {
    let value = request.headers().get(name)?;
    Some(value.to_str().unwrap())
}

#[tokio::test]
async fn the_list_request_sends_each_accounts_own_header_set_to_its_own_origin() {
    let connected = connect(credential("CANARY_CLAUDE_LIST"), DEFAULT_BASE_URL);
    assert!(connected.client_version.as_ref().unwrap().latest.is_some());
    assert!(connected.list_key().is_none());
    let version = ClientVersion::parse("2.1.999").unwrap();
    let signed_in = built(&connected, Some(version)).await;
    assert!(signed_in.headers()["authorization"].is_sensitive());
    let official = "https://api.anthropic.com/v1/models?limit=1000";
    assert_eq!(signed_in.url().as_str(), official);
    let (identity, _) = crate::provider::anthropic::subscription::data().unwrap();
    let betas = identity.betas.join(",");
    for (name, value) in [
        ("authorization", "Bearer CANARY_CLAUDE_LIST"),
        ("anthropic-beta", betas.as_str()),
        ("anthropic-version", "2023-06-01"),
        ("accept", "application/json"),
    ] {
        assert_eq!(named(&signed_in, name), Some(value), "{name}");
    }
    for (name, value) in &identity.rendered(version).unwrap().headers {
        assert_eq!(
            named(&signed_in, name.as_str()),
            Some(value.to_str().unwrap()),
            "{name}"
        );
    }
    assert_eq!(named(&signed_in, "x-api-key"), None);
    let key = api_key("synthetic-key");
    let api_provider = connect(key, DEFAULT_BASE_URL);
    assert!(api_provider.client_version.is_none());
    let api = built(&api_provider, None).await;
    assert_eq!(api.url().as_str(), official);
    assert_eq!(named(&api, "x-api-key"), Some("synthetic-key"));
    assert_eq!(named(&api, "anthropic-version"), Some("2023-06-01"));
    for absent in ["anthropic-beta", "authorization", "user-agent", "x-app"] {
        assert_eq!(named(&api, absent), None, "{absent}");
    }
    let bearer = Credential::AuthToken("synthetic-token".into());
    let gateway_provider = connect(bearer, "https://gateway.example/anthropic/");
    assert!(gateway_provider.client_version.is_none());
    let gateway = built(&gateway_provider, None).await;
    let url = "https://gateway.example/anthropic/v1/models?limit=1000";
    assert_eq!(gateway.url().as_str(), url);
    let token = named(&gateway, "authorization");
    assert_eq!(token, Some("Bearer synthetic-token"));
    assert_eq!(named(&gateway, "x-api-key"), None);
}

fn sample() -> Vec<u8> {
    let mut opus_4_5 = claude_entry("claude-opus-4-5-20251101", "opus", 200_000);
    opus_4_5["max_tokens"] = serde_json::json!(64_000);
    claude_wire(&[claude_entry("claude-opus-5-5", "opus", 1_000_000), opus_4_5])
}

async fn answer(
    provider: &AnthropicProvider,
    reply: &str,
) -> Result<reqwest::Response, ProviderError> {
    let server = http::serve(vec![http::Reply::Send(reply.to_owned())]).await;
    let response = provider.client.get(server.url).send().await;
    response.map_err(|_| ProviderError::malformed("unreachable"))
}

fn max_output(provider: &dyn Provider) -> Option<u32> {
    provider.capabilities("claude-opus-4-5")?.max_output
}

#[tokio::test]
async fn a_fetched_list_is_saved_under_its_sign_in_and_read_once_by_each_connection() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("models");
    let clock = crate::clock::TickingClock::new();
    let signed_in = connect(profiled(), DEFAULT_BASE_URL);
    let signed_in = signed_in.with_clock(clock);
    let key = signed_in.list_key().unwrap().to_owned();
    let body = String::from_utf8(sample()).unwrap();
    let ok = http::json("200 OK", &body);
    signed_in
        .save_fetched(answer(&signed_in, &ok).await, &folder)
        .await;
    let first = SavedList::load(&folder, &key).unwrap();
    assert_eq!(first.list, anthropic::parse(&sample()).unwrap());
    assert_eq!(signed_in.model_list(), None);
    let refused = http::json("500 Internal Server Error", &body);
    signed_in
        .save_fetched(answer(&signed_in, &refused).await, &folder)
        .await;
    let kept = SavedList::load(&folder, &key).unwrap();
    assert_eq!(kept.list, first.list);
    assert!(kept.attempted > first.attempted);
    let endpoint = account(profiled(), DEFAULT_BASE_URL);
    let connected = ProviderKind::Anthropic
        .connect(&endpoint, Some(&folder))
        .unwrap();
    assert_eq!(connected.model_list().as_deref(), Some(&first.list));
    assert_eq!(max_output(connected.as_ref()), Some(64_000));
    let newer = claude_wire(&[claude_entry("claude-opus-5-5", "opus", 500_000)]);
    let newer = anthropic::parse(&newer).unwrap();
    SavedList {
        attempted: kept.attempted + 1,
        failed: false,
        list: newer.clone(),
        default_model: None,
        web_search_models: Vec::new(),
    }
    .save(&folder, &key)
    .unwrap();
    assert_eq!(connected.model_list().as_deref(), Some(&first.list));
    assert_eq!(max_output(connected.as_ref()), Some(64_000));
    let fresh = ProviderKind::Anthropic
        .connect(&endpoint, Some(&folder))
        .unwrap();
    assert_eq!(fresh.model_list().as_deref(), Some(&newer));
    let setup = connect(credential("synthetic-oauth"), DEFAULT_BASE_URL);
    assert_eq!(setup.list_key(), None);
    let empty = dir.path().join("empty");
    setup.save_fetched(answer(&setup, &ok).await, &empty).await;
    assert!(!empty.exists());
    let official = connect(api_key("synthetic-key"), DEFAULT_BASE_URL);
    let official = official.list_key().unwrap().to_owned();
    assert_ne!(official, key);
    let fable = claude_wire(&[claude_entry("claude-fable-5-2", "fable", 1_000_000)]);
    let fable = http::json("200 OK", &String::from_utf8(fable).unwrap());
    let error = r#"{"error":{"message":"PLACEHOLDER","type":"invalid_request_error"}}"#;
    let not_a_list = http::json("200 OK", error);
    let gateway = "https://gateway.example";
    let bearer = Credential::AuthToken("CANARY_GATEWAY_TOKEN".into());
    let failed = dir.path().join("failed");
    let unlisted = connect(bearer.clone(), gateway);
    unlisted
        .save_fetched(answer(&unlisted, &not_a_list).await, &failed)
        .await;
    let endpoint = account(bearer.clone(), gateway);
    let shipped = ProviderKind::Anthropic.connect(&endpoint, Some(&failed));
    assert_eq!(shipped.unwrap().model_list(), None);
    for credential in [api_key("CANARY_GATEWAY_KEY"), bearer] {
        let provider = connect(credential.clone(), gateway);
        let own = provider.list_key().unwrap().to_owned();
        assert_ne!(own, official);
        provider
            .save_fetched(answer(&provider, &fable).await, &folder)
            .await;
        let saved = SavedList::load(&folder, &own).unwrap().list;
        for reply in [&refused, &not_a_list] {
            provider
                .save_fetched(answer(&provider, reply).await, &folder)
                .await;
            assert_eq!(SavedList::load(&folder, &own).unwrap().list, saved);
        }
        let endpoint = account(credential, gateway);
        let shown = ProviderKind::Anthropic.connect(&endpoint, Some(&folder));
        let offers = shown.unwrap().model_list().unwrap().offers();
        let fable = AccountModel {
            id: "claude-fable-5-2".into(),
            listed: Some(0),
        };
        assert_eq!(offers, [fable]);
    }
}

#[tokio::test]
async fn a_gateways_model_ids_are_the_ids_it_lists_sorted() {
    let entries = serde_json::json!({ "data": [
        { "id": "claude-b", "display_name": "PLACEHOLDER" },
        { "id": "claude-a", "display_name": "PLACEHOLDER" },
    ] });
    let reply = http::json("200 OK", &entries.to_string());
    let server = http::serve(vec![http::Reply::Send(reply)]).await;
    let key = api_key("synthetic-key");
    let gateway = connect(key, &server.url);
    assert_eq!(gateway.model_ids().await.unwrap(), ["claude-a", "claude-b"]);
    let asked = server.requests.lock().unwrap()[0].clone();
    assert!(
        asked.starts_with("GET /v1/models?limit=1000 HTTP/1.1"),
        "{asked}"
    );
    let headers = http::header_lines(&asked);
    assert!(headers.contains(&("x-api-key".into(), "synthetic-key".into())));
}

fn listing(folder: &std::path::Path, url: &str, saved: bool) -> AnthropicProvider {
    let mut provider = connect(api_key("synthetic-key"), url);
    provider.list_key = crate::models::list::key(&provider.url, &provider.credential, "list-test");
    let provider = provider.with_clock(std::sync::Arc::new(StillClock));
    provider.with_saved_list(saved.then_some(folder))
}

fn requests(server: &http::Server) -> usize {
    server.requests.lock().unwrap().len()
}

fn opus_6() -> serde_json::Value {
    claude_entry("claude-opus-6", "opus", 1_000_000)
}

#[tokio::test]
async fn connections_that_find_one_list_stale_send_one_request_and_a_failed_check_waits_the_hour() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("models");
    let now = StillClock.now().as_second();
    let old = anthropic::parse(&sample()).unwrap();
    let newer = claude_wire(&[claude_entry("claude-opus-5-5", "opus", 1_000_000), opus_6()]);
    let ok = http::json("200 OK", &String::from_utf8(newer).unwrap());
    let refused = http::json("401 Unauthorized", "{}");
    let server = http::serve(vec![
        http::Reply::Send(ok.clone()),
        http::Reply::Send(refused),
        http::Reply::Send(ok),
    ])
    .await;
    let key = listing(&folder, &server.url, false)
        .list_key()
        .unwrap()
        .to_owned();
    let stale = |attempted| SavedList {
        attempted,
        failed: false,
        list: old.clone(),
        default_model: None,
        web_search_models: Vec::new(),
    };
    stale(now - REFRESH_AFTER).save(&folder, &key).unwrap();
    let (first, second) = (
        listing(&folder, &server.url, true),
        listing(&folder, &server.url, true),
    );
    assert_eq!(first.model_list().as_deref(), Some(&old));
    first.refresh_list(folder.clone(), false).await;
    second.refresh_list(folder.clone(), false).await;
    assert_eq!(requests(&server), 1);
    for provider in [&first, &second] {
        assert!(
            provider
                .model_list()
                .unwrap()
                .find("claude-opus-6")
                .is_some()
        );
    }
    let saved = SavedList::load(&folder, &key).unwrap();
    assert_eq!(saved.list.ids(), ["claude-opus-5-5", "claude-opus-6"]);
    stale(now - REFRESH_AFTER).save(&folder, &key).unwrap();
    let (third, fourth) = (
        listing(&folder, &server.url, true),
        listing(&folder, &server.url, true),
    );
    third.refresh_list(folder.clone(), false).await;
    assert_eq!(requests(&server), 2);
    let kept = SavedList::load(&folder, &key).unwrap();
    assert_eq!((kept.attempted, &kept.list), (now, &old));
    fourth.refresh_list(folder.clone(), false).await;
    third.refresh_list(folder.clone(), false).await;
    assert_eq!(requests(&server), 2);
    assert_eq!(third.model_list().as_deref(), Some(&old));
    fourth.refresh_list(folder.clone(), true).await;
    assert_eq!(requests(&server), 3);
    assert!(fourth.model_list().unwrap().find("claude-opus-6").is_some());
}

#[tokio::test]
async fn the_attempt_is_saved_before_the_request_leaves() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("models");
    let now = StillClock.now().as_second();
    let mut hanging = http::serve(vec![http::Reply::Hang]).await;
    let provider = listing(&folder, &hanging.url, false);
    let key = provider.list_key().unwrap().to_owned();
    let fetch = tokio::spawn(provider.refresh_list(folder.clone(), false));
    assert!(matches!(hanging.next().await, http::Heard::Request(_)));
    let saved = SavedList::load(&folder, &key).unwrap();
    assert_eq!(saved.attempted, now);
    assert!(saved.failed);
    assert_eq!(provider.model_list(), None);
    fetch.abort();
}

#[tokio::test]
async fn a_name_a_connection_has_sent_keeps_its_facts_when_a_fetched_list_first_names_it() {
    use wisp_protocol::{CacheTtl, Message};
    let dir = tempfile::tempdir().unwrap();
    let (folder, url) = (dir.path().join("models"), "http://127.0.0.1:9");
    let key = listing(&folder, url, false).list_key().unwrap().to_owned();
    let old = anthropic::parse(&sample()).unwrap();
    SavedList {
        attempted: 1,
        failed: false,
        list: old,
        default_model: None,
        web_search_models: Vec::new(),
    }
    .save(&folder, &key)
    .unwrap();
    let provider = listing(&folder, url, true);
    let mut session = super::session(None, CacheTtl::OneHour);
    session.model = "claude-opus-6".into();
    let messages = [Message::user_text("question")];
    let request = super::plain_request(Default::default(), &session, &messages, None);
    let before = provider.capabilities("claude-opus-6");
    drop(provider.stream(&request));
    let newer = [claude_entry("claude-opus-5-5", "opus", 1_000_000), opus_6()];
    let ok = http::json("200 OK", &String::from_utf8(claude_wire(&newer)).unwrap());
    provider
        .save_fetched(answer(&provider, &ok).await, &folder)
        .await;
    assert_eq!(provider.capabilities("claude-opus-6"), before);
    assert!(
        provider
            .model_list()
            .unwrap()
            .find("claude-opus-6")
            .is_none()
    );
}
