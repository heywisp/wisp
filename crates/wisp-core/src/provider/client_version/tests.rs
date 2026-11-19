use super::*;
use crate::test_guard::{http, refusing_addr, within};
use serde_json::json;

const KIND: ProviderKind = ProviderKind::OpenAi;
const TAG: &str = "rust-v0.170.0";

fn client(url: &str) -> Client {
    Client {
        pinned: None,
        version: ClientVersion::parse("0.169.0").unwrap(),
        latest: Some(Latest {
            url: Url::parse(url).unwrap(),
            tag_prefix: "rust-v".into(),
        }),
    }
}

fn now() -> i64 {
    jiff::Timestamp::now().as_second()
}

fn release(tag: &str) -> String {
    json!({"tag_name": tag, "draft": false, "prerelease": false}).to_string()
}

fn reply(status: &str, body: &str) -> http::Reply {
    http::Reply::Send(http::json(status, body))
}

#[tokio::test]
async fn a_latest_release_gives_its_canonical_version_and_is_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("models");
    let alias = record_alias(KIND);
    let server = http::serve(vec![
        reply("200 OK", &release(TAG)),
        reply("200 OK", &release("rust-v0.171.0")),
    ])
    .await;
    let client = client(&format!("{}/releases/latest", server.url));
    let version = ClientVersion::parse("0.170.0").unwrap();
    let before = now();
    assert_eq!(resolve(&client, Some(&folder), KIND).await, version);
    assert_eq!(current(&client, Some(&folder), KIND), version);
    let saved: Record = record::read(&folder, &alias).unwrap();
    assert_eq!(saved.version, Some(version));
    assert!((before..=now()).contains(&saved.checked));
    let raw = std::fs::read(folder.join("client-openai.json")).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&raw).unwrap(),
        json!({
            "version": "0.170.0", "checked": saved.checked,
        })
    );
    #[cfg(unix)]
    for (path, want) in [
        (&folder, 0o700),
        (&folder.join("client-openai.json"), 0o600),
        (&folder.join("client-openai.lock"), 0o600),
    ] {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            want
        );
    }
    {
        let requests = server.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(
            request.lines().next(),
            Some("GET /releases/latest HTTP/1.1")
        );
        assert!(http::body(request).is_empty());
        let headers: std::collections::BTreeMap<_, _> =
            http::header_lines(request).into_iter().collect();
        assert_eq!(
            headers,
            std::collections::BTreeMap::from([
                (
                    "host".into(),
                    server.url.strip_prefix("http://").unwrap().into()
                ),
                ("accept".into(), "application/vnd.github+json".into()),
                ("x-github-api-version".into(), "2022-11-28".into()),
                ("user-agent".into(), crate::plugin::http::USER_AGENT.into()),
            ])
        );
    }
    let stale = Record {
        checked: saved.checked - REFRESH_AFTER,
        ..saved
    };
    record::write(&folder, &alias, &stale).unwrap();
    let next = ClientVersion::parse("0.171.0").unwrap();
    assert_eq!(resolve(&client, Some(&folder), KIND).await, next);
    assert_eq!(server.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn a_malformed_release_answer_fails_and_keeps_the_recorded_version() {
    let dir = tempfile::tempdir().unwrap();
    let alias = record_alias(KIND);
    let redirect = http::serve(vec![reply("200 OK", &release(TAG))]).await;
    let mut cases = vec![
        ("403", reply("403 Forbidden", &release(TAG))),
        ("404", reply("404 Not Found", &release(TAG))),
        ("429", reply("429 Too Many Requests", &release(TAG))),
        ("500", reply("500 Error", "")),
        ("201", reply("201 Created", &release(TAG))),
        (
            "redirect",
            http::Reply::Send(http::response(
                "302 Found",
                &[("Location", &redirect.url)],
                &release(TAG),
            )),
        ),
        ("bad JSON", reply("200 OK", "not JSON")),
        (
            "not an object",
            reply("200 OK", &json!([TAG, false, false]).to_string()),
        ),
        (
            "missing tag",
            reply("200 OK", r#"{"draft":false,"prerelease":false}"#),
        ),
        (
            "long tag",
            reply("200 OK", &release(&format!("rust-v{}", "9".repeat(64)))),
        ),
        ("wrong prefix", reply("200 OK", &release("v0.170.0"))),
        ("suffix", reply("200 OK", &release("rust-v0.170.0-alpha.1"))),
        ("four parts", reply("200 OK", &release("rust-v0.170.0.1"))),
        ("leading zero", reply("200 OK", &release("rust-v00.170.0"))),
        (
            "large body",
            reply(
                "200 OK",
                &format!("{}{}", release(TAG), " ".repeat(RELEASE_LIMIT)),
            ),
        ),
    ];
    for key in ["draft", "prerelease"] {
        for flag in [
            None,
            Some(json!(true)),
            Some(json!(null)),
            Some(json!("false")),
        ] {
            let mut body: serde_json::Value = serde_json::from_str(&release(TAG)).unwrap();
            if let Some(flag) = flag {
                body[key] = flag;
            } else {
                body.as_object_mut().unwrap().remove(key);
            }
            cases.push((key, reply("200 OK", &body.to_string())));
        }
    }
    let mut fixtures = Vec::new();
    for (case, reply) in cases {
        fixtures.push((case, http::serve(vec![reply]).await));
    }
    let (_socket, address) = refusing_addr();
    let unreachable = format!("http://{address}");
    for (case, url) in fixtures
        .iter()
        .map(|(case, server)| (*case, server.url.as_str()))
        .chain([("unreachable", unreachable.as_str())])
    {
        let client = client(url);
        let version = ClientVersion::parse("0.168.0").unwrap();
        let saved = Record {
            version: Some(version),
            checked: now() - REFRESH_AFTER,
        };
        record::write(dir.path(), &alias, &saved).unwrap();
        assert_eq!(
            resolve(&client, Some(dir.path()), KIND).await,
            version,
            "{case}"
        );
        let after: Record = record::read(dir.path(), &alias).unwrap();
        assert_eq!(after.version, saved.version, "{case}");
        assert!(after.checked > saved.checked, "{case}");
    }
    assert!(redirect.requests.lock().unwrap().is_empty());
    assert!(
        fixtures
            .iter()
            .all(|(_, server)| server.requests.lock().unwrap().len() == 1)
    );
}

#[tokio::test]
async fn the_reported_version_is_the_last_found_then_the_shipped_one() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path();
    let alias = record_alias(KIND);
    let path = folder.join(format!("{alias}.json"));
    let server = http::serve(vec![reply("200 OK", &release("rust-v0.168.0"))]).await;
    let client = client(&server.url);
    let pin = ClientVersion::parse("0.169.1").unwrap();
    let pinned = Client {
        pinned: Some(pin),
        ..client.clone()
    };
    assert_eq!(current(&pinned, None, KIND), pin);
    assert_eq!(resolve(&pinned, None, KIND).await, pin);
    let shipped = client.version;
    assert_eq!(current(&client, Some(folder), KIND), shipped);
    assert_eq!(resolve(&client, None, KIND).await, shipped);
    for version in [
        None,
        ClientVersion::parse("0.170.0"),
        ClientVersion::parse("0.168.0"),
    ] {
        record::write(
            folder,
            &alias,
            &Record {
                version,
                checked: now(),
            },
        )
        .unwrap();
        let expected = version.unwrap_or(shipped);
        assert_eq!(current(&client, Some(folder), KIND), expected);
        assert_eq!(resolve(&client, Some(folder), KIND).await, expected);
        assert_eq!(current(&pinned, Some(folder), KIND), pin);
        assert_eq!(resolve(&pinned, Some(folder), KIND).await, pin);
    }
    let old = ClientVersion::parse("0.168.0").unwrap();
    let newer = ClientVersion::parse("0.170.0").unwrap();
    let stale = Record {
        version: Some(newer),
        checked: now() - REFRESH_AFTER,
    };
    record::write(folder, &alias, &stale).unwrap();
    assert_eq!(resolve(&pinned, Some(folder), KIND).await, pin);
    assert_eq!(
        resolve(
            &for_account(&client, true, false, None).unwrap(),
            Some(folder),
            KIND
        )
        .await,
        newer
    );
    std::fs::write(&path, r#"{"version":"00.1.2","checked":0}"#).unwrap();
    assert_eq!(current(&client, Some(folder), KIND), shipped);
    assert_eq!(resolve(&client, Some(&path), KIND).await, shipped);
    std::fs::remove_file(&path).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&path, &path).unwrap();
        assert_eq!(resolve(&client, Some(folder), KIND).await, shipped);
        assert!(std::fs::symlink_metadata(&path).unwrap().is_symlink());
        std::fs::remove_file(&path).unwrap();
    }
    std::fs::create_dir(&path).unwrap();
    assert_eq!(resolve(&client, Some(folder), KIND).await, shipped);
    assert!(server.requests.lock().unwrap().is_empty());
    std::fs::remove_dir(&path).unwrap();
    record::write(folder, &alias, &stale).unwrap();
    assert_eq!(resolve(&client, Some(folder), KIND).await, old);
    assert_eq!(current(&client, Some(folder), KIND), old);
    assert_eq!(server.requests.lock().unwrap().len(), 1);
}

const ORIGIN_PORT: &str = "WISP_TEST_CLIENT_VERSION_ORIGIN_PORT";
const PROXY_PORT: &str = "WISP_TEST_CLIENT_VERSION_PROXY_PORT";
const PROXY_ENV: [&str; 6] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
];

fn canary_proxy(port: u16) -> String {
    format!("http://CANARY_PROXY_USER:CANARY_PROXY_PASSWORD@127.0.0.1:{port}")
}

#[tokio::test]
#[ignore = "run by the_lookup_never_uses_a_proxy with a cleared environment and loopback fixtures"]
async fn lookup_proxy_test_child() {
    let port = |name| std::env::var(name).unwrap().parse::<u16>().unwrap();
    let proxy = canary_proxy(port(PROXY_PORT));
    assert!(
        PROXY_ENV
            .iter()
            .all(|name| std::env::var(name).is_ok_and(|value| value == proxy))
    );
    let url = format!("http://127.0.0.1:{}", port(ORIGIN_PORT));
    let proxied = reqwest::Client::new()
        .get(&url)
        .timeout(LOOKUP_TIMEOUT)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(proxied, "proxy fixture");
    let latest = client(&url).latest.unwrap();
    assert_eq!(lookup(&latest).await, ClientVersion::parse("0.170.0"));
}

#[tokio::test]
async fn the_lookup_never_uses_a_proxy() {
    const TEST: &str = concat!(module_path!(), "::lookup_proxy_test_child");
    let origin = http::serve(vec![reply("200 OK", &release(TAG))]).await;
    let proxy = http::serve(vec![reply("200 OK", "proxy fixture"), http::Reply::Hang]).await;
    let port = |url: &str| Url::parse(url).unwrap().port().unwrap();
    let proxy_port = port(&proxy.url);
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            TEST.strip_prefix("wisp_core::").unwrap(),
            "--ignored",
            "--nocapture",
        ])
        .kill_on_drop(true)
        .env_clear()
        .env(ORIGIN_PORT, port(&origin.url).to_string())
        .env(PROXY_PORT, proxy_port.to_string());
    for name in PROXY_ENV {
        child.env(name, canary_proxy(proxy_port));
    }
    let result = within("proxy test child", child.output()).await.unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let proxied = proxy.requests.lock().unwrap();
    assert_eq!(proxied.len(), 1);
    assert_eq!(
        proxied[0].lines().next(),
        Some(format!("GET {}/ HTTP/1.1", origin.url).as_str())
    );
    assert_eq!(origin.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn overlapping_resolves_in_one_folder_send_one_lookup_and_keep_its_version() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path();
    let alias = record_alias(KIND);
    let previous = ClientVersion::parse("0.168.0").unwrap();
    let saved = Record {
        version: Some(previous),
        checked: now() - REFRESH_AFTER,
    };
    record::write(folder, &alias, &saved).unwrap();
    let (arrived, mut requests) = tokio::sync::mpsc::unbounded_channel();
    let respond = std::sync::Arc::new(tokio::sync::Notify::new());
    let server = http::gateway({
        let respond = respond.clone();
        move |request| {
            let respond = respond.clone();
            arrived.send(request).unwrap();
            async move {
                respond.notified().await;
                reply("200 OK", &release(TAG))
            }
        }
    })
    .await;
    let client = client(&server);
    let claim = record::claim(folder, &alias);
    assert!(matches!(claim, Claim::Held(_)));
    assert_eq!(resolve(&client, Some(folder), KIND).await, previous);
    assert_eq!(record::read(folder, &alias), Some(saved));
    assert!(requests.try_recv().is_err());
    drop(claim);
    let first = tokio::spawn({
        let client = client.clone();
        let folder = folder.to_path_buf();
        async move { resolve(&client, Some(&folder), KIND).await }
    });
    within("first release request", requests.recv())
        .await
        .unwrap();
    assert_eq!(resolve(&client, Some(folder), KIND).await, previous);
    let during: Record = record::read(folder, &alias).unwrap();
    assert_eq!(during.version, saved.version);
    assert!(during.checked > saved.checked);
    respond.notify_one();
    let found = ClientVersion::parse("0.170.0").unwrap();
    assert_eq!(first.await.unwrap(), found);
    assert_eq!(
        record::read(folder, &alias),
        Some(Record {
            version: Some(found),
            ..during
        })
    );
    assert_eq!(resolve(&client, Some(folder), KIND).await, found);
    assert!(requests.try_recv().is_err());
}

#[tokio::test]
async fn a_forced_lookup_ignores_the_hour_and_records_like_any_lookup() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path();
    let alias = record_alias(KIND);
    let path = folder.join(format!("{alias}.json"));
    let found = ClientVersion::parse("0.170.0").unwrap();
    let next = ClientVersion::parse("0.171.0").unwrap();
    let pin = ClientVersion::parse("0.168.0").unwrap();
    let server = http::serve(vec![
        reply("200 OK", &release(TAG)),
        reply("500 Error", ""),
        reply("200 OK", &release("rust-v0.171.0")),
    ])
    .await;
    let account = client(&server.url);
    let mut saved = Record {
        version: Some(account.version),
        checked: now() - 1,
    };
    record::write(folder, &alias, &saved).unwrap();
    assert_eq!(
        check(&account, Some(folder), KIND).await,
        Lookup::Found(found)
    );
    let found_record: Record = record::read(folder, &alias).unwrap();
    assert_eq!(found_record.version, Some(found));
    assert!(found_record.checked > saved.checked);
    assert_eq!(resolve(&account, Some(folder), KIND).await, found);
    let claim = record::claim(folder, &alias);
    assert!(matches!(claim, Claim::Held(_)));
    assert_eq!(check(&account, Some(folder), KIND).await, Lookup::Busy);
    assert_eq!(record::read(folder, &alias), Some(found_record));
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    drop(claim);

    saved = Record {
        version: Some(found),
        checked: now() - 1,
    };
    record::write(folder, &alias, &saved).unwrap();
    assert_eq!(
        check(&account, Some(folder), KIND).await,
        Lookup::Kept(found)
    );
    let failed: Record = record::read(folder, &alias).unwrap();
    assert_eq!(failed.version, saved.version);
    assert!(failed.checked > saved.checked);
    let pinned = Client {
        pinned: Some(pin),
        ..account.clone()
    };
    assert_eq!(
        check(&pinned, Some(folder), KIND).await,
        Lookup::Found(next)
    );
    assert_eq!(current(&account, Some(folder), KIND), next);
    assert_eq!(current(&pinned, Some(folder), KIND), pin);
    assert_eq!(resolve(&pinned, Some(folder), KIND).await, pin);

    assert_eq!(
        check(
            &for_account(&account, true, false, None).unwrap(),
            Some(folder),
            KIND
        )
        .await,
        Lookup::NoRelease
    );
    for folder in [None, Some(path.as_path())] {
        assert_eq!(
            check(&account, folder, KIND).await,
            Lookup::Unrecorded(None)
        );
    }
    assert_eq!(server.requests.lock().unwrap().len(), 3);

    let blocked_write = http::gateway({
        let path = path.clone();
        move |_| {
            let path = path.clone();
            async move {
                std::fs::remove_file(&path).unwrap();
                std::fs::create_dir(&path).unwrap();
                reply("200 OK", &release(TAG))
            }
        }
    })
    .await;
    let account = client(&blocked_write);
    assert_eq!(
        check(&account, Some(folder), KIND).await,
        Lookup::Unrecorded(Some(found))
    );
    assert_eq!(
        check(&account, Some(folder), KIND).await,
        Lookup::Unrecorded(None)
    );
    std::fs::remove_dir(&path).unwrap();
    assert_eq!(resolve(&account, Some(folder), KIND).await, found);
}
