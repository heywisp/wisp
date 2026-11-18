use super::*;
use serde_json::json;
use std::time::Duration;

const REFRESH: &str = "CANARY-refresh-token-credential";

#[test]
fn sign_in_links_only_open_with_safe_literal_addresses() {
    for (link, opens) in [
        (
            "https://example.invalid/authorize?client_id=CANARY-client&redirect_uri=http%3A%2F%2F127.0.0.1&scope=read+write",
            true,
        ),
        ("http://127.0.0.1:8123/authorize?state=CANARY-state", true),
        ("http://localhost/authorize", true),
        ("http://[::1]/authorize", true),
        ("http://example.invalid/authorize", false),
        ("https://user@example.invalid/authorize", false),
        ("https://:CANARY-password@example.invalid/authorize", false),
        ("https:example.invalid", false),
        ("HTTPS://example.invalid", false),
        ("https://example.invalid/%", false),
        ("https://example.invalid/%a", false),
        ("https://example.invalid/%zz", false),
        ("https://example.invalid/$", false),
        ("https://example.invalid/(", false),
        ("https://example.invalid/)", false),
        ("https://example.invalid/'", false),
        ("https://example.invalid/`", false),
        ("https://example.invalid/a b", false),
        ("https://example.invalid/é", false),
        ("not an address", false),
    ] {
        assert_eq!(is_sign_in_link(link), opens, "{link}");
    }
}

fn credentials(access: &str) -> StoredCredentials {
    StoredCredentials::new("CANARY-client".into(), Some(serde_json::from_value(json!({"access_token":access,"refresh_token":REFRESH,"token_type":"Bearer","expires_in":3600})).unwrap()), Vec::new(), Some(0)).with_issuer(Some("https://issuer.example.invalid".into()))
}

fn store(config: PathBuf, address: &str) -> Store {
    let path = oauth::mcp_sign_in_profile(&config, "local").unwrap();
    if !config.exists() {
        crate::secrets::write_atomic(
            &config,
            format!("[mcp.servers.local]\ncommand = \"{address}\"\n").as_bytes(),
            0o600,
        )
        .unwrap();
    }
    let store = Store {
        path,
        legacy: oauth::mcp_profile(&config, address).unwrap(),
        kind: StoreKind::File,
        record: Arc::default(),
        issuer: "https://issuer.example.invalid".into(),
        config,
        name: "local".into(),
        address: address.into(),
        registration: None,
        guard: Arc::default(),
    };
    if !store.path.exists() && store.configured().is_ok() {
        let record = Record {
            address: address.into(),
            credentials: Some(credentials("CANARY-initial-access")),
            ..Record::default()
        };
        let locks = oauth::lock_profile(&store.path).unwrap();
        oauth::persist_value_locked(&store.path, &record, store.kind, &locks).unwrap();
    }
    store
}

#[tokio::test]
async fn a_stored_sign_in_is_held_by_the_chat_start_pass() {
    let folder = tempfile::tempdir().unwrap();
    let config = folder.path().join("config.toml");
    let store = store(config.clone(), "https://server.example.invalid/mcp");
    const ACCESS: &str = "CANARY-stored-sign-in-held-from-chat-start";
    let record = Record {
        address: store.address.clone(),
        credentials: Some(credentials(ACCESS)),
        ..Record::default()
    };
    let locks = oauth::lock_profile(&store.path).unwrap();
    oauth::persist_value_locked(&store.path, &record, StoreKind::File, &locks).unwrap();
    drop(locks);
    assert_eq!(crate::session::held().mask(ACCESS), ACCESS);
    let service = crate::mcp::Service::new(
        [(
            "local".into(),
            crate::config::McpServer {
                command: store.address,
                args: Vec::new(),
                keys: Vec::new(),
            },
        )]
        .into(),
        folder.path().to_owned(),
        None,
    )
    .with_config(config, crate::endpoints::PathShown::Given)
    .with_store(StoreKind::File);
    service.refresh_keys().await;
    assert_eq!(
        crate::session::held().mask(ACCESS),
        crate::session::REDACTED
    );
}

#[tokio::test]
async fn the_credentials_lock_serializes_refreshes_and_each_load_is_fresh() {
    let folder = tempfile::tempdir().unwrap();
    let config = folder.path().join("config.toml");
    let mut first = store(config.clone(), "https://server.example.invalid/mcp");
    let mut second = store(config.clone(), "https://server.example.invalid/mcp");
    let mut third = store(config, "https://server.example.invalid/mcp");
    const CLIENT: &str = "CANARY-client-secret-credential";
    let registration = Registration {
        client_secret: Some(Secret::new(CLIENT)),
        redirect: "http://127.0.0.1/callback".into(),
        scopes: Vec::new(),
    };
    for store in [&mut first, &mut second, &mut third] {
        store.registration = Some(registration.clone());
    }
    let mut record = first.read().await.unwrap();
    for value in ["CANARY-initial-access", REFRESH] {
        assert_eq!(crate::session::held().mask(value), crate::session::REDACTED);
    }
    record.registration = Some(registration);
    first
        .persist(&record, &first.locks().await.unwrap())
        .unwrap();
    assert!(first.load().await.unwrap().is_some());
    first
        .save(credentials("CANARY-first-access"))
        .await
        .unwrap();
    assert_eq!(
        crate::session::held().scrub("CANARY-first-access"),
        "CANARY-first-access"
    );
    let held = first.acquire_refresh_guard().await.unwrap().unwrap();
    let loading = tokio::spawn(async move { third.load().await });
    let mut waiting = tokio::spawn(async move {
        let guard = second.acquire_refresh_guard().await.unwrap().unwrap();
        let loaded = second.load().await.unwrap().unwrap();
        let access = serde_json::to_value(loaded.token_response.unwrap()).unwrap()["access_token"]
            .as_str()
            .unwrap()
            .to_owned();
        drop(guard);
        access
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut waiting)
            .await
            .is_err()
    );
    first
        .save(credentials("CANARY-next-access-credential"))
        .await
        .unwrap();
    assert_eq!(
        crate::session::held().mask("CANARY-next-access-credential"),
        crate::session::REDACTED
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(1), loading)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .is_some()
    );
    drop(held);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), waiting)
            .await
            .unwrap()
            .unwrap(),
        "CANARY-next-access-credential"
    );
    let held = oauth::lock_profile(&first.path).unwrap();
    let release = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        drop(held);
    });
    first.clear().await.unwrap();
    release.await.unwrap();
    assert_eq!(first.record.lock().await.address, first.address);
    assert!(first.load().await.unwrap().is_none());
    const OLD_ADDRESS: &str = "CANARY-discarded-address-token";
    record.address = "https://other.example.invalid/mcp".into();
    record.credentials = Some(credentials(OLD_ADDRESS));
    oauth::persist_value_locked(
        &first.path,
        &record,
        first.kind,
        &first.locks().await.unwrap(),
    )
    .unwrap();
    assert!(first.load().await.unwrap().is_none());
    assert_eq!(
        crate::session::held().mask(OLD_ADDRESS),
        crate::session::REDACTED
    );

    const REPLACED: &str = "CANARY-replaced-registration-token";
    const REPLACED_CLIENT: &str = "CANARY-replaced-registration-secret";
    const INCOMING: &str = "CANARY-rejected-incoming-token";
    record.address = first.address.clone();
    record.credentials = Some(credentials(REPLACED));
    record.registration.as_mut().unwrap().client_secret = Some(Secret::new(REPLACED_CLIENT));
    oauth::persist_value_locked(
        &first.path,
        &record,
        first.kind,
        &first.locks().await.unwrap(),
    )
    .unwrap();
    assert!(first.save(credentials(INCOMING)).await.is_err());
    for value in [REPLACED, REPLACED_CLIENT] {
        assert_eq!(crate::session::held().mask(value), crate::session::REDACTED);
    }

    const CLEAR_TOKEN: &str = "CANARY-clear-read-registration-token";
    const CLEAR_CLIENT: &str = "CANARY-clear-read-registration-secret";
    record.credentials = Some(credentials(CLEAR_TOKEN));
    record.registration.as_mut().unwrap().client_secret = Some(Secret::new(CLEAR_CLIENT));
    oauth::persist_value_locked(
        &first.path,
        &record,
        first.kind,
        &first.locks().await.unwrap(),
    )
    .unwrap();
    assert!(first.clear().await.is_err());
    for value in [CLEAR_TOKEN, CLEAR_CLIENT] {
        assert_eq!(crate::session::held().mask(value), crate::session::REDACTED);
    }

    crate::secrets::write_atomic(&first.config, b"", 0o600).unwrap();
    let value = "CANARY-wrong-issuer-credential";
    assert!(
        first
            .save(credentials(value).with_issuer(None))
            .await
            .is_err()
    );
    assert_eq!(crate::session::held().mask(value), crate::session::REDACTED);
    const CANCELLED_CLIENT: &str = "CANARY-cancelled-registration-secret";
    record.registration.as_mut().unwrap().client_secret = Some(Secret::new(CANCELLED_CLIENT));
    assert!(
        first
            .persist(&record, &first.locks().await.unwrap())
            .is_err()
    );
    assert_eq!(
        crate::session::held().mask(CANCELLED_CLIENT),
        crate::session::REDACTED
    );
}

#[tokio::test]
async fn changing_the_issuer_or_server_address_never_reuses_a_token() {
    let folder = tempfile::tempdir().unwrap();
    let config = folder.path().join("config.toml");
    let first = store(config.clone(), "https://server.example.invalid/mcp");
    first.save(credentials("CANARY-access")).await.unwrap();
    let mut changed = store(config.clone(), "https://server.example.invalid/mcp");
    changed.issuer = "https://other.example.invalid".into();
    assert!(changed.load().await.unwrap().is_none());
    let other = store(config.clone(), "https://other.example.invalid/mcp");
    assert!(other.load().await.unwrap().is_none());
    let isolated = oauth::mcp_sign_in_profile(&folder.path().join("other.toml"), "local").unwrap();
    assert_ne!(&first.path, &isolated);
    let mut newer = first.clone();
    newer.record = Arc::default();
    newer.registration = Some(Registration {
        client_secret: None,
        redirect: "http://127.0.0.1:8123/other".into(),
        scopes: Vec::new(),
    });
    let mut record = Record {
        address: newer.address.clone(),
        credentials: Some(credentials("CANARY-new-access")),
        registration: newer.registration.clone(),
    };
    let locks = newer.locks().await.unwrap();
    newer.persist(&record, &locks).unwrap();
    drop(locks);
    assert!(first.load().await.unwrap().is_none());
    assert!(
        first
            .save(credentials("CANARY-stale-access"))
            .await
            .is_err()
    );
    assert!(first.clear().await.is_err());
    assert!(newer.load().await.unwrap().is_some());
    let address = "https://other.example.invalid/mcp";
    crate::secrets::write_atomic(
        &config,
        format!("[mcp.servers.local]\ncommand = \"{address}\"\n").as_bytes(),
        0o600,
    )
    .unwrap();
    let mut moved = newer.clone();
    moved.address = address.into();
    moved.legacy = oauth::mcp_profile(&config, address).unwrap();
    moved.record = Arc::default();
    assert!(moved.load().await.unwrap().is_none());
    assert!(!first.path.exists());
    record.address = address.into();
    moved
        .persist(&record, &moved.locks().await.unwrap())
        .unwrap();
    assert!(newer.load().await.unwrap().is_none());
    assert!(
        newer
            .save(credentials("CANARY-stale-access"))
            .await
            .is_err()
    );
    assert!(newer.clear().await.is_err());
    assert!(moved.load().await.unwrap().is_some());
}

#[tokio::test]
async fn sign_in_refuses_insecure_resources_metadata_and_unusable_store() {
    use std::os::unix::fs::PermissionsExt;

    let folder = tempfile::tempdir().unwrap();
    let config = folder.path().join("config.toml");
    let refused = start(
        "local",
        "http://server.example.invalid/mcp",
        Some(config.clone()),
        StoreKind::File,
        None,
    )
    .await;
    assert!(matches!(refused, Err(error) if error == INSECURE));
    let server = crate::mcp_test::Server::start(true, None).await;
    let _store = store(config.clone(), &server.address());
    server.plain_endpoints();
    let refused = start(
        "local",
        &server.address(),
        Some(config),
        StoreKind::File,
        None,
    )
    .await;
    assert!(matches!(refused, Err(error) if error == INSECURE));
    server.requests(|requests| {
        assert!(
            requests
                .iter()
                .all(|request| request.path != "/register" && request.path != "/token")
        )
    });
    let server = crate::mcp_test::Server::start(true, None).await;
    let config = folder.path().join("unusable.toml");
    let store = store(config.clone(), &server.address());
    server.before_registration(move || {
        std::fs::set_permissions(
            store.path.parent().unwrap(),
            std::fs::Permissions::from_mode(0o740),
        )
        .unwrap();
    });
    let secret = crate::mcp_test::CLIENT_SECRET;
    assert_eq!(crate::session::held().mask(secret), secret);
    let refused = start(
        "local",
        &server.address(),
        Some(config),
        StoreKind::File,
        None,
    )
    .await;
    assert!(matches!(refused, Err(error) if error == STORE_ERROR));
    assert_eq!(
        crate::session::held().mask(secret),
        crate::session::REDACTED
    );
}

#[tokio::test]
async fn another_name_at_the_old_address_has_no_sign_in() {
    let folder = tempfile::tempdir().unwrap();
    let config = folder.path().join("config.toml");
    let server = crate::mcp_test::Server::start(true, None).await;
    let first = store(config.clone(), &server.address());
    let record = Record {
        address: server.address(),
        credentials: Some(credentials("CANARY-access").with_issuer(Some(server.issuer()))),
        registration: Some(Registration {
            client_secret: Some(Secret::new("CANARY-client-secret")),
            redirect: "http://127.0.0.1:8123/return".into(),
            scopes: Vec::new(),
        }),
    };
    first
        .persist(&record, &first.locks().await.unwrap())
        .unwrap();
    crate::secrets::write_atomic(
        &config,
        format!(
            "[mcp.servers.local]\ncommand = \"https://other.example.invalid/mcp\"\n[mcp.servers.other]\ncommand = \"{}\"\n",
            server.address()
        ).as_bytes(),
        0o600,
    ).unwrap();
    let (_, other) = manager(
        "other",
        &server.address(),
        Some(config),
        StoreKind::File,
        None,
    )
    .await
    .unwrap();
    assert!(other.load().await.unwrap().is_none());
    assert!(other.registration.is_none());
    assert!(first.path.exists());
    server.requests(|requests| {
        assert!(requests.iter().all(|request| {
            !request.headers.contains_key("authorization") && request.path != "/token"
        }))
    });
}

#[tokio::test]
async fn legacy_address_credentials_are_cleared_without_being_reused() {
    for changed in [true, false] {
        let folder = tempfile::tempdir().unwrap();
        let config = folder.path().join("config.toml");
        let server = crate::mcp_test::Server::start(true, None).await;
        let other = crate::mcp_test::Server::start(true, Some(server.issuer())).await;
        let address = if changed {
            other.address()
        } else {
            server.address()
        };
        let _store = store(config.clone(), &address);
        let legacy = oauth::mcp_profile(&config, &server.address()).unwrap();
        let record = json!({
            "credentials": credentials("CANARY-legacy-access").with_issuer(Some(server.issuer())),
            "registration": {
                "client_secret": "CANARY-legacy-client-secret",
                "redirect": "http://127.0.0.1:8123/return",
                "scopes": []
            }
        });
        assert!(
            serde_json::from_value::<Record>(record.clone())
                .unwrap()
                .address
                .is_empty()
        );
        oauth::persist_value_locked(
            &legacy,
            &record,
            StoreKind::File,
            &oauth::lock_profile(&legacy).unwrap(),
        )
        .unwrap();
        assert!(
            restored("local", &address, Some(config), StoreKind::File, None)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(legacy.exists(), changed);
        for endpoint in [&server, &other] {
            endpoint.requests(|requests| {
                assert!(requests.iter().all(|request| {
                    !request.headers.contains_key("authorization") && request.path != "/token"
                }))
            });
        }
    }
}
