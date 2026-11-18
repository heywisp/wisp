#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "offline auth fixtures fail on unexpected I/O"
)]

use super::*;
use crate::secrets::keyring::{
    BackendError, KeyringBackend, KeyringStore, MemoryKeyring, MemoryMode, TERMINAL_DEADLINE,
};
use crate::test_guard::{refusing_addr, within};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn private_dir() -> tempfile::TempDir {
    let folder = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(folder.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    folder
}

fn tokens(generation: u64, expiry: u64) -> Tokens {
    Tokens {
        version: PROFILE_VERSION,
        kind: SubscriptionKind::ChatGpt,
        access_token: Secret::new(format!("synthetic-access-{generation}")),
        refresh_token: Secret::new(format!("synthetic-refresh-{generation}")),
        expires_at: expiry,
        generation,
        project_id: None,
    }
}

pub(crate) fn valid_chatgpt_profile() -> (tempfile::TempDir, PathBuf) {
    let folder = private_dir();
    let path = profile(folder.path());
    save(&path, &tokens(0, now() + 3600)).unwrap();
    (folder, path)
}

pub(crate) async fn refresh_refused_at(
    path: &Path,
    url: &str,
    refused: Secret,
) -> Result<String, ProviderError> {
    access_clock(path, SubscriptionKind::ChatGpt, url, Some(refused), now)
        .await
        .map_err(provider_error)
}

fn held(digit: char) -> AccountHash {
    AccountHash(digit.to_string().repeat(MAX_ACCOUNT))
}

fn approved(account: Option<AccountHash>) -> Exchanged {
    Exchanged {
        kind: SubscriptionKind::ChatGpt,
        tokens: tokens(0, now() + 3600),
        account,
    }
}

fn jwt(claims: &Value) -> String {
    format!("e30.{}.sig", URL_SAFE_NO_PAD.encode(claims.to_string()))
}

fn openai(user: &str, user_id: Value, workspace: Value) -> Value {
    json!({"email": "synthetic-claim-email@example.invalid", "https://api.openai.com/auth": {user: user_id, "chatgpt_account_id": workspace}})
}

fn profile(folder: &Path) -> PathBuf {
    folder.join(format!("oauth-{}.json", uuid::Uuid::new_v4().simple()))
}

fn account(path: &Path) -> String {
    format!(
        "oauth/{}",
        path.file_stem()
            .unwrap()
            .to_str()
            .unwrap()
            .strip_prefix("oauth-")
            .unwrap()
    )
}

pub(crate) fn memory_store(path: &Path, memory: &MemoryKeyring) {
    test_keyrings()
        .lock()
        .unwrap()
        .insert(account(path), Arc::new(memory.store(TERMINAL_DEADLINE)));
}

async fn server(
    status: u16,
    body: Value,
) -> (
    String,
    Arc<StdMutex<Vec<String>>>,
    tokio::task::JoinHandle<()>,
) {
    server_with(status, body, async {}).await
}

async fn server_with(
    status: u16,
    body: Value,
    received: impl Future<Output = ()> + Send + 'static,
) -> (
    String,
    Arc<StdMutex<Vec<String>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let seen = Arc::new(StdMutex::new(Vec::new()));
    let capture = seen.clone();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut data = Vec::new();
        loop {
            let mut chunk = [0u8; 1024];
            let count = stream.read(&mut chunk).await.unwrap();
            data.extend_from_slice(&chunk[..count]);
            if let Some(split) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = std::str::from_utf8(&data[..split]).unwrap();
                let len = head
                    .lines()
                    .find_map(|s| {
                        s.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|n| n.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if data.len() >= split + 4 + len {
                    break;
                }
            }
            assert!(count > 0);
        }
        capture
            .lock()
            .unwrap()
            .push(String::from_utf8(data).unwrap());
        received.await;
        let body = body.to_string();
        stream.write_all(format!("HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
    });
    (format!("http://{address}/token"), seen, task)
}

#[tokio::test]
async fn pkce_verifier_stays_local_and_state_is_independent() {
    let pending = PendingLogin::start(SubscriptionKind::Anthropic)
        .await
        .unwrap();
    let query: BTreeMap<_, _> = pending.url.query_pairs().collect();
    assert_ne!(query["state"], pending.verifier.expose());
    assert_eq!(
        query["code_challenge"],
        URL_SAFE_NO_PAD.encode(Sha256::digest(pending.verifier.expose().as_bytes()))
    );
    assert!(
        !pending
            .authorization_url()
            .contains(pending.verifier.expose())
    );
    assert!(!query.contains_key("login_hint"));
    assert!(!format!("{pending:?}").contains(pending.return_to.state.expose()));
}

#[tokio::test]
async fn wrong_state_and_expired_manual_login_are_rejected_before_network() {
    let folder = private_dir();
    let pending = PendingLogin::start(SubscriptionKind::Anthropic)
        .await
        .unwrap();
    assert!(matches!(
        pending
            .finish(Some("synthetic#wrong"), &folder.path().join("config.toml"))
            .await,
        Err(AuthError::State)
    ));
    let mut pending = PendingLogin::start(SubscriptionKind::Anthropic)
        .await
        .unwrap();
    pending.return_to.started = tokio::time::Instant::now() - Duration::from_secs(601);
    assert!(matches!(
        pending
            .finish(Some("synthetic#wrong"), &folder.path().join("config.toml"))
            .await,
        Err(AuthError::Expired)
    ));
    let mut left = std::fs::read_dir(folder.path()).unwrap().flatten();
    assert!(left.all(|e| e.path().extension().is_none_or(|x| x != "refused")));
}

#[tokio::test]
async fn concurrent_requests_refresh_once_and_persist_replacement_together() {
    let folder = private_dir();
    let path = profile(folder.path());
    save(&path, &tokens(0, now() - 1)).unwrap();
    let (url,seen,task)=server(200,json!({"access_token":"synthetic-new-access","refresh_token":"synthetic-new-refresh","expires_in":3600})).await;
    let (first, second) = tokio::join!(
        access_at(&path, SubscriptionKind::ChatGpt, &url, now()),
        access_at(&path, SubscriptionKind::ChatGpt, &url, now())
    );
    assert_eq!(first.unwrap(), "synthetic-new-access");
    assert_eq!(second.unwrap(), "synthetic-new-access");
    task.await.unwrap();
    assert_eq!(seen.lock().unwrap().len(), 1);
    let stored = read(&path, SubscriptionKind::ChatGpt).unwrap();
    assert_eq!(stored.generation, 1);
    assert_eq!(stored.refresh_token.expose(), "synthetic-new-refresh");
    let request = seen.lock().unwrap()[0].clone();
    assert!(request.contains("refresh_token=synthetic-refresh-0"));
    assert!(!request.to_ascii_lowercase().contains("chatgpt-account-id"));
    let queued = profile(folder.path());
    save(&path, &tokens(1, now() - 1)).unwrap();
    save(&queued, &tokens(0, now() - 1)).unwrap();
    let grant = Arc::new(tokio::sync::Notify::new());
    let sent = grant.clone();
    let answer = json!({"access_token":"synthetic-sent-access","refresh_token":"synthetic-sent-refresh","expires_in":3600});
    let hold = Arc::new(tokio::sync::Notify::new());
    let held = hold.clone();
    let (url, _, task) = server_with(200, answer, async move {
        sent.notify_one();
        held.notified().await;
    })
    .await;
    let (unsent_url, unsent, unsent_task) = server(200, json!({})).await;
    let first = {
        let path = path.clone();
        tokio::spawn(async move { access_at(&path, SubscriptionKind::ChatGpt, &url, now()).await })
    };
    within("the refresh request", grant.notified()).await;
    let second = {
        let queued = queued.clone();
        tokio::spawn(async move {
            access_at(&queued, SubscriptionKind::ChatGpt, &unsent_url, now()).await
        })
    };
    tokio::task::yield_now().await;
    tokio::join!(settle(), async { hold.notify_one() });
    assert!(matches!(second.await.unwrap(), Err(AuthError::Network)));
    assert_eq!(first.await.unwrap().unwrap(), "synthetic-sent-access");
    task.await.unwrap();
    assert!(unsent.lock().unwrap().is_empty());
    unsent_task.abort();
    assert_eq!(
        read(&queued, SubscriptionKind::ChatGpt).unwrap().generation,
        0
    );
}

#[tokio::test]
async fn concurrent_refusals_refresh_a_valid_token_once_and_persist_replacement_together() {
    let (_folder, path) = valid_chatgpt_profile();
    let (url, seen, task) = server(
        200,
        json!({"access_token":"synthetic-new-access","refresh_token":"synthetic-new-refresh","expires_in":3600}),
    )
    .await;
    let (first, second) = tokio::join!(
        refresh_refused_at(&path, &url, Secret::new("synthetic-access-0")),
        refresh_refused_at(&path, &url, Secret::new("synthetic-access-0"))
    );
    assert_eq!(first.unwrap(), "synthetic-new-access");
    assert_eq!(second.unwrap(), "synthetic-new-access");
    task.await.unwrap();
    assert_eq!(seen.lock().unwrap().len(), 1);
    let stored = read(&path, SubscriptionKind::ChatGpt).unwrap();
    assert_eq!(stored.generation, 1);
    assert_eq!(stored.access_token.expose(), "synthetic-new-access");
    assert_eq!(stored.refresh_token.expose(), "synthetic-new-refresh");
    assert!(stored.valid());
    assert!(seen.lock().unwrap()[0].contains("refresh_token=synthetic-refresh-0"));
}

#[tokio::test]
async fn a_later_refusal_uses_the_newer_token_even_after_a_new_sign_in_resets_generation() {
    let (_folder, path) = valid_chatgpt_profile();
    for generation in [2, 0] {
        let mut current = tokens(generation, now() + 3600);
        current.access_token = Secret::new("synthetic-later-access");
        save(&path, &current).unwrap();
        let before = std::fs::read(&path).unwrap();
        let (_socket, addr) = refusing_addr();
        let access = refresh_refused_at(
            &path,
            &format!("http://{addr}/token"),
            Secret::new("synthetic-access-0"),
        )
        .await
        .unwrap();
        assert_eq!(access, "synthetic-later-access");
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[tokio::test]
async fn a_forced_refresh_outage_never_returns_the_already_refused_access_token() {
    let (_folder, path) = valid_chatgpt_profile();
    let before = std::fs::read(&path).unwrap();
    let (url, _, task) = server(503, json!({"error":"synthetic-private-refresh-error"})).await;
    let error = refresh_refused_at(&path, &url, Secret::new("synthetic-access-0"))
        .await
        .unwrap_err();
    task.await.unwrap();
    assert_eq!(error.kind, ErrorKind::Server);
    assert_eq!(error.detail.retry, wisp_protocol::RetryClass::Transient);
    assert!(!format!("{error:?}").contains("synthetic"));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(!refused(&path));
}

#[tokio::test]
async fn superseded_pending_rotation_never_overwrites_newer_generation() {
    let folder = private_dir();
    let path = profile(folder.path());
    save(&path, &tokens(2, now() + 3600)).unwrap();
    pending().lock().await.insert(
        path.clone(),
        PendingRotation {
            predecessor_sha256: refresh_digest(&tokens(0, now() - 1)),
            rotated: tokens(1, now() + 3600),
        },
    );
    let access = access_at(
        &path,
        SubscriptionKind::ChatGpt,
        "http://127.0.0.1:1/token",
        now(),
    )
    .await
    .unwrap();
    assert_eq!(access, "synthetic-access-2");
    assert_eq!(
        read(&path, SubscriptionKind::ChatGpt).unwrap().generation,
        2
    );
}

#[tokio::test]
async fn expired_pending_rotation_refreshes_using_its_replacement_token() {
    let folder = private_dir();
    let path = profile(folder.path());
    let stamp = now();
    save(&path, &tokens(0, stamp - 10)).unwrap();
    pending().lock().await.insert(
        path.clone(),
        PendingRotation {
            predecessor_sha256: refresh_digest(&tokens(0, stamp - 10)),
            rotated: tokens(1, stamp - 1),
        },
    );
    let (url,seen,task)=server(200,json!({"access_token":"synthetic-latest","refresh_token":"synthetic-latest-refresh","expires_in":3600})).await;
    assert_eq!(
        access_at(&path, SubscriptionKind::ChatGpt, &url, stamp)
            .await
            .unwrap(),
        "synthetic-latest"
    );
    task.await.unwrap();
    let request = seen.lock().unwrap()[0].clone();
    assert!(request.contains("refresh_token=synthetic-refresh-1"));
    assert!(!request.contains("refresh_token=synthetic-refresh-0"));
    assert_eq!(
        read(&path, SubscriptionKind::ChatGpt).unwrap().generation,
        2
    );
}

#[tokio::test]
async fn proactive_refresh_outage_preserves_access_until_actual_expiry() {
    let folder = private_dir();
    let path = profile(folder.path());
    let stamp = now();
    save(&path, &tokens(0, stamp + 30)).unwrap();
    let (url, _, task) = server(503, json!({"error":"unavailable"})).await;
    assert_eq!(
        access_at(&path, SubscriptionKind::ChatGpt, &url, stamp)
            .await
            .unwrap(),
        "synthetic-access-0"
    );
    task.await.unwrap();
    assert_eq!(
        read(&path, SubscriptionKind::ChatGpt).unwrap().generation,
        0
    );
}

#[tokio::test]
async fn access_expiring_during_a_failed_refresh_is_never_returned() {
    let folder = private_dir();
    let path = profile(folder.path());
    let stamp = now();
    save(&path, &tokens(0, stamp + 30)).unwrap();
    let clock = Arc::new(AtomicU64::new(stamp));
    let advance = clock.clone();
    let (url, _, task) = server_with(503, json!({"error": "synthetic-outage"}), async move {
        advance.store(stamp + 45, Ordering::SeqCst)
    })
    .await;
    let result = access_clock(&path, SubscriptionKind::ChatGpt, &url, None, move || {
        clock.load(Ordering::SeqCst)
    })
    .await;
    task.await.unwrap();
    assert!(matches!(result, Err(AuthError::Refused(503))));
    assert_eq!(
        read(&path, SubscriptionKind::ChatGpt).unwrap().generation,
        0
    );
    assert!(!pending().lock().await.contains_key(&path));
}

#[test]
fn keychain_profile_keeps_token_material_out_of_local_files() {
    let folder = private_dir();
    let path = profile(folder.path());
    let memory = MemoryKeyring::new();
    memory_store(&path, &memory);
    persist_profile(
        &path,
        SubscriptionKind::ChatGpt,
        &tokens(0, now() + 3600),
        StoreKind::Keyring,
        None,
    )
    .unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("synthetic-access"));
    assert!(!text.contains("synthetic-refresh"));
    assert_eq!(
        status(&path, SubscriptionKind::ChatGpt).unwrap().store,
        StoreKind::Keyring
    );
    assert_eq!(
        read(&path, SubscriptionKind::ChatGpt)
            .unwrap()
            .access_token
            .expose(),
        "synthetic-access-0"
    );
    assert!(
        memory
            .items()
            .values()
            .any(|s| s.contains("synthetic-refresh-0"))
    );
}

#[test]
fn failed_initial_locator_write_removes_the_new_keychain_profile() {
    let folder = private_dir();
    let path = profile(folder.path());
    std::fs::create_dir(&path).unwrap();
    let memory = MemoryKeyring::new();
    memory_store(&path, &memory);
    assert!(
        persist_profile(
            &path,
            SubscriptionKind::ChatGpt,
            &tokens(0, now() + 3600),
            StoreKind::Keyring,
            None
        )
        .is_err()
    );
    assert!(memory.items().is_empty());
}

#[cfg(unix)]
#[test]
fn profile_files_and_parent_folders_reject_other_user_access_or_symlinks() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let folder = private_dir();
    let path = profile(folder.path());
    save(&path, &tokens(0, now() + 3600)).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o602)).unwrap();
    assert!(read(&path, SubscriptionKind::ChatGpt).is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::set_permissions(folder.path(), std::fs::Permissions::from_mode(0o702)).unwrap();
    assert!(read(&path, SubscriptionKind::ChatGpt).is_err());
    std::fs::set_permissions(folder.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let link = folder.path().join("link");
    symlink(&path, &link).unwrap();
    assert!(read(&link, SubscriptionKind::ChatGpt).is_err());
    let config = folder.path().join("config.toml");
    prepare(&config).unwrap();
    let other = profile_path(&config, "oauth:00000000000000000000000000000001").unwrap();
    let file = account_path(&other);
    let signed = approved(Some(held('a')));
    write_account(&other, Some(&held('a')));
    assert_eq!(signed.held_by(&config, None), vec![other.clone()]);
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(read_account(&other), None);
    assert!(signed.held_by(&config, None).is_empty());
    std::fs::remove_file(&file).unwrap();
    secrets::write_atomic(&file, "a".repeat(MAX_ACCOUNT + 1).as_bytes(), 0o600).unwrap();
    assert_eq!(read_account(&other), None);
    assert!(signed.held_by(&config, None).is_empty());
    std::fs::remove_file(&file).unwrap();
    let decoy = folder.path().join("decoy");
    secrets::write_atomic(&decoy, held('a').0.as_bytes(), 0o600).unwrap();
    symlink(&decoy, &file).unwrap();
    assert_eq!(read_account(&other), None);
    assert!(signed.held_by(&config, None).is_empty());
}

#[tokio::test]
async fn manual_callback_requires_complete_state_and_success_keeps_verifier_out_of_storage() {
    let folder = private_dir();
    let config = folder.path().join("config.toml");
    let login = PendingLogin::start(SubscriptionKind::Anthropic)
        .await
        .unwrap();
    let state = login.return_to.state.expose().to_owned();
    let verifier = login.verifier.expose().to_owned();
    let (url, seen, task) = server(
        200,
        json!({
            "access_token": "synthetic-login-access",
            "refresh_token": "synthetic-login-refresh",
            "expires_in": 3600,
            "account": {"uuid": "synthetic-account-uuid", "email_address": "synthetic@example.invalid"},
            "organization": {"uuid": "synthetic-organization-uuid"},
        }),
    )
    .await;
    let handle = login
        .finish_at(Some(&format!("synthetic-code#{state}")), &config, &url)
        .await
        .unwrap()
        .persist(&config, StoreKind::File)
        .unwrap();
    task.await.unwrap();
    let request = seen.lock().unwrap()[0].clone();
    let body: Value = serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(body["code"], "synthetic-code");
    assert_eq!(body["state"], state);
    assert_eq!(body["code_verifier"], verifier);
    assert!(
        request
            .to_ascii_lowercase()
            .contains("content-type: application/json")
    );
    assert!(
        request
            .to_ascii_lowercase()
            .contains("user-agent: anthropic")
    );
    let path = profile_path(&config, &handle).unwrap();
    let stored = std::fs::read_to_string(&path).unwrap();
    assert!(!stored.contains(&state));
    assert!(!stored.contains(&verifier));
    assert!(!stored.contains("synthetic-code"));
    let held = account_path(&path);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&held).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let held = std::fs::read_to_string(&held).unwrap();
    assert_eq!(held.len(), 64);
    assert!(held.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(read_account(&path), Some(held));
    assert_eq!(
        read(&path, SubscriptionKind::Anthropic)
            .unwrap()
            .refresh_token
            .expose(),
        "synthetic-login-refresh"
    );

    let login = PendingLogin::start(SubscriptionKind::Anthropic)
        .await
        .unwrap();
    assert!(matches!(
        login.finish_at(Some("bare-code"), &config, &url).await,
        Err(AuthError::Pasted(CodeProblem::NoState))
    ));
}

#[tokio::test]
async fn a_doubled_paste_is_reported_as_doubled() {
    let folder = private_dir();
    let config = folder.path().join("config.toml");
    let login = PendingLogin::start(SubscriptionKind::Anthropic)
        .await
        .unwrap();
    let state = login.return_to.state.expose().to_owned();
    let doubled = format!("PASTE_CANARY#{state}PASTE_CANARY#{state}");
    let result = login
        .finish_at(Some(&doubled), &config, "http://127.0.0.1:1/token")
        .await;
    assert!(matches!(
        result,
        Err(AuthError::Pasted(CodeProblem::Repeated))
    ));
    let error = result.unwrap_err();
    assert!(!error.to_string().contains("PASTE_CANARY"));
    assert!(!format!("{error:?}").contains("PASTE_CANARY"));
    assert!(!error.to_string().contains(&state));
}

#[tokio::test]
async fn an_empty_paste_is_reported_as_empty() {
    let folder = private_dir();
    let config = folder.path().join("config.toml");
    for pasted in [None, Some(""), Some("\r\n")] {
        let login = PendingLogin::start(SubscriptionKind::Anthropic)
            .await
            .unwrap();
        assert!(matches!(
            login
                .finish_at(pasted, &config, "http://127.0.0.1:1/token")
                .await,
            Err(AuthError::Pasted(CodeProblem::Empty))
        ));
    }
}

#[tokio::test]
async fn a_rejected_paste_keeps_the_login() {
    let folder = private_dir();
    let config = folder.path().join("config.toml");
    let login = PendingLogin::start(SubscriptionKind::Anthropic)
        .await
        .unwrap();
    let state = login.return_to.state.expose().to_owned();
    assert!(matches!(
        login.check(Some("synthetic-code")),
        Err(AuthError::Pasted(CodeProblem::NoState))
    ));
    assert!(matches!(
        login.check(Some("synthetic-code#another-sign-in")),
        Err(AuthError::State)
    ));
    assert!(matches!(
        login.check(Some(&format!("synthetic-code#{state}x"))),
        Err(AuthError::State)
    ));
    assert!(
        login
            .check(Some(&format!("  synthetic-code#{state}\r\n")))
            .is_ok()
    );
    let (url, seen, task) = server(
        200,
        json!({
            "access_token": "synthetic-login-access",
            "refresh_token": "synthetic-login-refresh",
            "expires_in": 3600,
        }),
    )
    .await;
    let handle = login
        .finish_at(Some(&format!("synthetic-code#{state}")), &config, &url)
        .await
        .unwrap()
        .persist(&config, StoreKind::File)
        .unwrap();
    task.await.unwrap();
    assert!(profile_path(&config, &handle).is_some());
    let request = seen.lock().unwrap()[0].clone();
    assert!(request.contains("\"code\":\"synthetic-code\""));
}

#[tokio::test]
async fn a_copy_for_one_retry_keeps_the_same_state_and_verifier() {
    let folder = private_dir();
    let config = folder.path().join("config.toml");
    let login = PendingLogin::start(SubscriptionKind::Anthropic)
        .await
        .unwrap();
    let state = login.return_to.state.expose().to_owned();
    let spare = login.again();
    assert_eq!(spare.authorization_url(), login.authorization_url());
    let (_socket, addr) = refusing_addr();
    assert!(matches!(
        login
            .finish_at(
                Some(&format!("synthetic-code#{state}")),
                &config,
                &format!("http://{addr}/token"),
            )
            .await,
        Err(AuthError::Network)
    ));
    let (url, seen, task) = server(
        200,
        json!({
            "access_token": "synthetic-retry-access",
            "refresh_token": "synthetic-retry-refresh",
            "expires_in": 3600,
        }),
    )
    .await;
    spare
        .finish_at(Some(&format!("synthetic-code#{state}")), &config, &url)
        .await
        .unwrap();
    task.await.unwrap();
    assert!(seen.lock().unwrap()[0].contains(&state));
}

#[tokio::test(start_paused = true)]
async fn expired_listener_login_does_not_restart_its_deadline() {
    let folder = private_dir();
    let mut login = PendingLogin::start(SubscriptionKind::Anthropic)
        .await
        .unwrap();
    login.kind = SubscriptionKind::ChatGpt;
    login.return_to.listener = Some(Arc::new(TcpListener::bind("127.0.0.1:0").await.unwrap()));
    login.return_to.started = tokio::time::Instant::now() - LOGIN_TIMEOUT - Duration::from_secs(1);
    let before = tokio::time::Instant::now();
    assert!(matches!(
        login
            .finish_at(
                None,
                &folder.path().join("config.toml"),
                "http://127.0.0.1:1/token",
            )
            .await,
        Err(AuthError::Expired)
    ));
    assert_eq!(tokio::time::Instant::now(), before);
}

#[tokio::test]
async fn denied_callback_reports_a_safe_page_and_no_credential() {
    let login = PendingLogin::start(SubscriptionKind::Anthropic)
        .await
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let callback = RedirectReturn::new(
        format!("http://{address}/auth/callback"),
        login.return_to.state.clone(),
        None,
    );
    let target = format!(
        "/auth/callback?state={}&error=access_denied&error_description=synthetic-private-error",
        login.return_to.state.expose()
    );
    let send = async {
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream
            .write_all(format!("GET {target} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    };
    let (result, page) = tokio::join!(
        async {
            let returned = callback.callback(&listener).await?;
            let code = returned.code.expose().to_owned();
            returned.respond("Sign-in received. Return to Wisp.").await;
            Ok::<_, AuthError>(code)
        },
        send
    );
    assert!(matches!(result, Err(AuthError::Declined)));
    assert!(page.contains("Sign-in declined."));
    assert!(page.contains("Cache-Control: no-store"));
    assert!(!page.contains(login.return_to.state.expose()));
    assert!(!page.contains("synthetic-private-error"));
}

#[tokio::test]
async fn callback_accepts_a_valid_request_line_split_across_tcp_reads() {
    let login = PendingLogin::start(SubscriptionKind::Anthropic)
        .await
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let callback = RedirectReturn::new(
        format!("http://{address}/auth/callback"),
        login.return_to.state.clone(),
        None,
    );
    let remainder = format!(
        "back?state={}&code=synthetic-fragmented-code HTTP/1.1\r\nHost: localhost\r\n\r\n",
        login.return_to.state.expose()
    );
    let send = async {
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream.set_nodelay(true).unwrap();
        stream.write_all(b"GET /auth/call").await.unwrap();
        tokio::time::sleep(Duration::from_millis(25)).await;
        stream.write_all(remainder.as_bytes()).await.unwrap();
        let mut page = String::new();
        stream.read_to_string(&mut page).await.unwrap();
        page
    };
    let (result, page) = within("the callback", async {
        tokio::join!(
            async {
                let returned = callback.callback(&listener).await?;
                let code = returned.code.expose().to_owned();
                returned.respond("Sign-in received. Return to Wisp.").await;
                Ok::<_, AuthError>(code)
            },
            send
        )
    })
    .await;
    assert_eq!(result.unwrap(), "synthetic-fragmented-code");
    assert!(page.contains("Sign-in received. Return to Wisp."));
    assert!(!page.contains("synthetic-fragmented-code"));
    assert!(!page.contains(login.return_to.state.expose()));
}

async fn callback_page(address: std::net::SocketAddr, request: &[u8]) -> String {
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream.write_all(request).await.unwrap();
    let mut page = Vec::new();
    match stream.read_to_end(&mut page).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
        Err(error) => panic!("could not read callback response: {error}"),
    }
    String::from_utf8(page).unwrap()
}

#[tokio::test]
async fn callback_skips_idle_and_malformed_sockets_before_a_valid_request() {
    let login = PendingLogin::start(SubscriptionKind::Anthropic)
        .await
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let callback = RedirectReturn::new(
        format!("http://{address}/auth/callback"),
        login.return_to.state.clone(),
        None,
    );
    let valid = format!(
        "GET /auth/callback?state={}&code=synthetic-later-code HTTP/1.1\r\nHost: localhost\r\n\r\n",
        login.return_to.state.expose()
    );
    let send = async {
        let idle = callback_page(address, b"").await;
        assert!(idle.is_empty());
        let malformed = callback_page(address, b"GET /auth/callback?\xff HTTP/1.1\r\n\r\n").await;
        assert!(malformed.is_empty());
        callback_page(address, valid.as_bytes()).await
    };
    let (result, page) = within("the callback", async {
        tokio::join!(
            async {
                let returned = callback.callback(&listener).await?;
                let code = returned.code.expose().to_owned();
                returned.respond("Sign-in received. Return to Wisp.").await;
                Ok::<_, AuthError>(code)
            },
            send
        )
    })
    .await;
    assert_eq!(result.unwrap(), "synthetic-later-code");
    assert!(page.contains("Sign-in received. Return to Wisp."));
    assert!(!page.contains("synthetic-later-code"));
    assert!(!page.contains(login.return_to.state.expose()));
}

#[tokio::test]
async fn expired_refresh_errors_never_return_stale_access_or_raw_provider_errors() {
    for kind in [SubscriptionKind::ChatGpt, SubscriptionKind::Anthropic] {
        for code in [400, 401, 403, 429, 503, 200, 0] {
            let folder = private_dir();
            let path = profile(folder.path());
            let stamp = now();
            let mut original = tokens(3, stamp - 1);
            original.kind = kind;
            save(&path, &original).unwrap();
            let before = std::fs::read(&path).unwrap();
            let error = if code == 0 {
                let (_socket, addr) = refusing_addr();
                access_at(&path, kind, &format!("http://{addr}/"), stamp)
                    .await
                    .unwrap_err()
            } else {
                let body = if code == 200 {
                    json!({"error": "synthetic-bad-answer"})
                } else {
                    json!({"error": "synthetic-refresh-3", "access_token": "synthetic-provider-error"})
                };
                let (url, _, task) = server(code, body).await;
                let error = access_at(&path, kind, &url, stamp).await.unwrap_err();
                task.await.unwrap();
                error
            };
            match code {
                0 => assert!(matches!(error, AuthError::Network)),
                200 => assert!(matches!(error, AuthError::InvalidResponse)),
                _ => assert!(matches!(error, AuthError::Refused(refused) if refused == code)),
            }
            assert!(!format!("{error:?} {error}").contains("synthetic"));
            assert_eq!(std::fs::read(&path).unwrap(), before);
            assert!(!pending().lock().await.contains_key(&path));
            let marked = matches!(code, 400 | 401);
            assert_eq!(refused(&path), marked, "{code}");
            assert_eq!(status(&path, kind).unwrap().refused, marked, "{code}");
            if marked {
                let mark = refused_path(&path);
                assert_eq!(std::fs::read(&mark).unwrap(), b"");
                assert!(!secrets::readable_by_others(&mark));
                let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
                assert_eq!(
                    mark.file_name().unwrap().to_string_lossy(),
                    format!("{stem}.refused")
                );
                for secret in [&original.access_token, &original.refresh_token] {
                    assert!(!stem.contains(secret.expose()));
                }
            }
        }
        let canary = jwt(&openai(
            "chatgpt_user_id",
            json!("IDENTITY_CANARY"),
            json!("IDENTITY_CANARY"),
        ));
        let (url, _, task) = server(
            200,
            json!({"access_token": canary, "id_token": canary, "refresh_token": "synthetic-refresh", "expires_in": 0,
                "account": {"uuid": "IDENTITY_CANARY", "email_address": "IDENTITY_CANARY@example.invalid"},
                "organization": {"uuid": "IDENTITY_CANARY"}}),
        )
        .await;
        let error = exchange(&client().unwrap(), &url, kind, &json!({}), None)
            .await
            .unwrap_err();
        task.await.unwrap();
        assert!(matches!(error, AuthError::InvalidResponse));
        assert!(!format!("{error:?} {error}").contains("CANARY"));
    }
}

#[tokio::test]
async fn omitted_refresh_token_preserves_the_previous_secret_for_both_providers() {
    for kind in SubscriptionKind::ALL {
        let folder = private_dir();
        let path = profile(folder.path());
        let stamp = now();
        let mut original = tokens(7, stamp - 1);
        original.kind = kind;
        save(&path, &original).unwrap();
        let (url, seen, task) = server(
            200,
            json!({"access_token": "synthetic-omitted-refresh-access", "expires_in": 3600}),
        )
        .await;
        assert_eq!(
            access_at(&path, kind, &url, stamp).await.unwrap(),
            "synthetic-omitted-refresh-access"
        );
        task.await.unwrap();
        let saved = read(&path, kind).unwrap();
        assert_eq!(saved.refresh_token, original.refresh_token);
        assert_eq!(saved.generation, 8);
        let raw = seen.lock().unwrap()[0].clone();
        assert!(!raw.to_ascii_lowercase().contains("chatgpt-account-id"));
        let body = raw.split_once("\r\n\r\n").unwrap().1;
        match kind {
            SubscriptionKind::ChatGpt | SubscriptionKind::Antigravity => {
                let decoded = Url::parse(&format!("http://localhost/?{body}")).unwrap();
                let pairs: BTreeMap<_, _> = decoded.query_pairs().collect();
                assert_eq!(pairs["refresh_token"], "synthetic-refresh-7");
                assert_eq!(pairs["grant_type"], "refresh_token");
            }
            SubscriptionKind::Anthropic => {
                let decoded: Value = serde_json::from_str(body).unwrap();
                assert_eq!(decoded["refresh_token"], "synthetic-refresh-7");
                assert_eq!(decoded["grant_type"], "refresh_token");
            }
        }
        assert_eq!(read_account(&path), None);
        let both = |who: &str| match kind {
            SubscriptionKind::Anthropic => {
                json!({"access_token": "synthetic-filled-access", "expires_in": 3600,
                "account": {"uuid": who}, "organization": {"uuid": "synthetic-organization"}})
            }
            SubscriptionKind::ChatGpt => {
                json!({"access_token": jwt(&openai("chatgpt_user_id", json!(who), json!("synthetic-workspace"))), "expires_in": 3600})
            }
            SubscriptionKind::Antigravity => {
                json!({"access_token": "synthetic-filled-access", "id_token": jwt(&json!({"sub": who})), "expires_in": 3600})
            }
        };
        let mut filled = None;
        for who in ["synthetic-first-user", "synthetic-second-user"] {
            let (url, seen, task) = server(200, both(who)).await;
            access_at(&path, kind, &url, stamp + 7200).await.unwrap();
            task.await.unwrap();
            assert!(
                !seen.lock().unwrap()[0]
                    .to_ascii_lowercase()
                    .contains("chatgpt-account-id")
            );
            let found = read_account(&path);
            assert!(found.is_some());
            assert_eq!(*filled.get_or_insert(found.clone()), found);
        }
    }
    #[cfg(unix)]
    {
        let folder = private_dir();
        let path = profile(folder.path());
        let memory = MemoryKeyring::new();
        memory_store(&path, &memory);
        let stamp = now();
        persist_profile(
            &path,
            SubscriptionKind::ChatGpt,
            &tokens(0, stamp - 1),
            StoreKind::Keyring,
            None,
        )
        .unwrap();
        let blocked = account_path(&path).join("blocked");
        let (url, _, task) = server_with(
        200,
        json!({"access_token": jwt(&openai("chatgpt_user_id", json!("synthetic-user"), json!("synthetic-workspace"))), "expires_in": 3600}),
        async move { std::fs::create_dir_all(&blocked).unwrap() },
    )
    .await;
        let access = access_at(&path, SubscriptionKind::ChatGpt, &url, stamp).await;
        task.await.unwrap();
        assert!(access.unwrap().starts_with("e30."));
        assert_eq!(read_account(&path), None);
        assert!(account_path(&path).join("blocked").is_dir());
    }
}

#[tokio::test]
async fn rejected_rotated_token_response_never_replaces_the_saved_bundle() {
    for body in [
        json!({"access_token": "invalid\nheader", "refresh_token": "synthetic-invalid-refresh", "expires_in": 3600}),
        json!({"access_token": "synthetic-invalid-access", "refresh_token": "synthetic-invalid-refresh", "expires_in": 0}),
        json!({"refresh_token": "synthetic-invalid-refresh", "expires_in": 3600}),
    ] {
        let folder = private_dir();
        let path = profile(folder.path());
        let stamp = now();
        save(&path, &tokens(0, stamp - 1)).unwrap();
        let before = std::fs::read(&path).unwrap();
        let (url, _, task) = server(200, body).await;
        assert!(matches!(
            access_at(&path, SubscriptionKind::ChatGpt, &url, stamp).await,
            Err(AuthError::InvalidResponse)
        ));
        task.await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!pending().lock().await.contains_key(&path));
    }
}

#[tokio::test]
async fn failed_keychain_rotation_is_recovered_without_reusing_the_old_refresh_token() {
    let folder = private_dir();
    let path = profile(folder.path());
    let memory = MemoryKeyring::new();
    memory_store(&path, &memory);
    let stamp = now();
    persist_profile(
        &path,
        SubscriptionKind::ChatGpt,
        &tokens(0, stamp - 1),
        StoreKind::Keyring,
        None,
    )
    .unwrap();
    let locator = std::fs::read(&path).unwrap();
    let failing = memory.clone();
    let (url, seen, task) = server_with(
        200,
        json!({"access_token":"synthetic-recovered-access","refresh_token":"synthetic-recovered-refresh","expires_in":3600}),
        async move { failing.set_mode(MemoryMode::Failing(BackendError::Other)) },
    )
    .await;
    assert!(matches!(
        access_at(&path, SubscriptionKind::ChatGpt, &url, stamp).await,
        Err(AuthError::Store)
    ));
    task.await.unwrap();
    assert!(pending().lock().await.contains_key(&path));
    assert!(memory.items()[&account(&path)].contains("synthetic-refresh-0"));
    assert!(!rotation_path(&path).exists());
    memory.set_mode(MemoryMode::Working);
    assert_eq!(
        access_at(&path, SubscriptionKind::ChatGpt, &url, stamp)
            .await
            .unwrap(),
        "synthetic-recovered-access"
    );
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert!(!pending().lock().await.contains_key(&path));
    assert_eq!(std::fs::read(&path).unwrap(), locator);
    let stored = read(&path, SubscriptionKind::ChatGpt).unwrap();
    assert_eq!(stored.refresh_token.expose(), "synthetic-recovered-refresh");
    assert_eq!(stored.generation, 1);
}

#[tokio::test]
async fn pending_rotation_cannot_replace_a_new_login_with_a_reset_generation() {
    let folder = private_dir();
    let path = profile(folder.path());
    let stamp = now();
    let mut current = tokens(0, stamp + 3600);
    current.access_token = Secret::new("synthetic-new-login-access");
    current.refresh_token = Secret::new("synthetic-new-login-refresh");
    save(&path, &current).unwrap();
    pending().lock().await.insert(
        path.clone(),
        PendingRotation {
            predecessor_sha256: refresh_digest(&tokens(0, stamp - 1)),
            rotated: tokens(1, stamp + 3600),
        },
    );
    assert_eq!(
        access_at(
            &path,
            SubscriptionKind::ChatGpt,
            "http://127.0.0.1:1/token",
            stamp,
        )
        .await
        .unwrap(),
        "synthetic-new-login-access"
    );
    assert_eq!(
        read(&path, SubscriptionKind::ChatGpt)
            .unwrap()
            .refresh_token,
        current.refresh_token
    );
    assert!(!pending().lock().await.contains_key(&path));
}

struct GuardedBackend {
    inner: Box<dyn KeyringBackend>,
    folder: PathBuf,
    held: Arc<AtomicBool>,
    reject_delete: bool,
}

impl KeyringBackend for GuardedBackend {
    fn get(&mut self, account: &str) -> Result<String, BackendError> {
        self.inner.get(account)
    }

    fn set(&mut self, account: &str, secret: &str) -> Result<(), BackendError> {
        self.held.store(
            matches!(
                secrets::lock(
                    &[(&self.folder, LockKind::Credentials)],
                    PathShown::Given,
                    Duration::ZERO,
                ),
                Err(secrets::StoreError::Busy)
            ),
            Ordering::SeqCst,
        );
        self.inner.set(account, secret)
    }

    fn delete(&mut self, account: &str) -> Result<(), BackendError> {
        if self.reject_delete {
            Err(BackendError::Other)
        } else {
            self.inner.delete(account)
        }
    }
}

fn guarded_keyring(path: &Path, memory: &MemoryKeyring, reject_delete: bool) -> Arc<AtomicBool> {
    let open = memory.opener();
    let folder = path.parent().unwrap().to_owned();
    let held = Arc::new(AtomicBool::new(false));
    let captured = held.clone();
    let store = KeyringStore::new(
        Box::new(move || {
            Ok(Box::new(GuardedBackend {
                inner: open()?,
                folder,
                held: captured,
                reject_delete,
            }) as Box<dyn KeyringBackend>)
        }),
        TERMINAL_DEADLINE,
    );
    test_keyrings()
        .lock()
        .unwrap()
        .insert(account(path), Arc::new(store));
    held
}

#[test]
fn initial_keychain_save_holds_the_shared_credentials_lock() {
    let folder = private_dir();
    let path = profile(folder.path());
    let memory = MemoryKeyring::new();
    let held = guarded_keyring(&path, &memory, false);
    persist_profile(
        &path,
        SubscriptionKind::ChatGpt,
        &tokens(0, now() + 3600),
        StoreKind::Keyring,
        None,
    )
    .unwrap();
    assert!(held.load(Ordering::SeqCst));
    assert!(memory.items().contains_key(&account(&path)));
}

#[test]
fn failed_sidecar_rollback_reports_uncertainty_without_exposing_tokens() {
    let folder = private_dir();
    let path = profile(folder.path());
    std::fs::create_dir(&path).unwrap();
    let memory = MemoryKeyring::new();
    guarded_keyring(&path, &memory, true);
    let error = persist_profile(
        &path,
        SubscriptionKind::ChatGpt,
        &tokens(0, now() + 3600),
        StoreKind::Keyring,
        None,
    )
    .unwrap_err();
    assert!(matches!(error, AuthError::StoreUncertain));
    assert!(!format!("{error:?} {error}").contains("synthetic"));
    assert!(memory.items().contains_key(&account(&path)));
}

#[cfg(unix)]
#[test]
fn symlinked_parent_and_private_file_modes_are_checked_before_credentials_are_read() {
    use std::os::unix::fs::{MetadataExt, symlink};

    let folder = private_dir();
    let private = folder.path().join("private");
    secrets::create_folder(&private).unwrap();
    let path = profile(&private);
    persist_profile(
        &path,
        SubscriptionKind::ChatGpt,
        &tokens(0, now() + 3600),
        StoreKind::File,
        None,
    )
    .unwrap();
    let metadata = std::fs::metadata(&path).unwrap();
    assert_eq!(metadata.mode() & 0o777, 0o600);
    assert_eq!(metadata.uid(), rustix::process::getuid().as_raw());
    assert_eq!(std::fs::metadata(&private).unwrap().mode() & 0o777, 0o700);
    let alias = folder.path().join("alias");
    symlink(&private, &alias).unwrap();
    let through_link = alias.join(path.file_name().unwrap());
    assert!(matches!(
        read(&through_link, SubscriptionKind::ChatGpt),
        Err(AuthError::Store)
    ));
    assert!(read(&path, SubscriptionKind::Anthropic).is_err());
}

#[tokio::test]
async fn copied_config_cannot_read_delete_or_replace_the_original_browser_profile() {
    let folder = private_dir();
    let original_config = folder.path().join("original.toml");
    let copied_config = folder.path().join("copy.toml");
    let handle = format!("oauth:{}", uuid::Uuid::new_v4().simple());
    std::fs::write(&original_config, "copied config fixture").unwrap();
    std::fs::copy(&original_config, &copied_config).unwrap();
    prepare(&original_config).unwrap();
    let original_path = profile_path(&original_config, &handle).unwrap();
    let copied_path = profile_path(&copied_config, &handle).unwrap();
    assert_ne!(original_path, copied_path);
    let mut original = tokens(4, now() + 3600);
    original.kind = SubscriptionKind::Anthropic;
    persist_profile(
        &original_path,
        SubscriptionKind::Anthropic,
        &original,
        StoreKind::File,
        None,
    )
    .unwrap();
    let before = std::fs::read(&original_path).unwrap();
    assert!(matches!(
        credential(&copied_config, &handle, SubscriptionKind::Anthropic),
        Err(AuthError::LoggedOut | AuthError::Store)
    ));
    assert!(
        logout(&copied_path, SubscriptionKind::Anthropic)
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(&original_path).unwrap(), before);
    prepare(&copied_config).unwrap();
    assert!(matches!(
        credential(&copied_config, &handle, SubscriptionKind::Anthropic),
        Err(AuthError::LoggedOut)
    ));
    let old_copied_credential =
        credential(&copied_config, &handle, SubscriptionKind::Anthropic).ok();
    assert!(old_copied_credential.is_none());
    let new_handle = format!("oauth:{}", uuid::Uuid::new_v4().simple());
    let new_path = profile_path(&copied_config, &new_handle).unwrap();
    let mut copied = tokens(0, now() + 3600);
    copied.kind = SubscriptionKind::Anthropic;
    persist_profile(
        &new_path,
        SubscriptionKind::Anthropic,
        &copied,
        StoreKind::File,
        None,
    )
    .unwrap();
    assert_eq!(
        credential(&copied_config, &new_handle, SubscriptionKind::Anthropic)
            .unwrap()
            .secret(),
        "synthetic-access-0"
    );
    forget(&new_path).await.unwrap();
    assert_eq!(std::fs::read(&original_path).unwrap(), before);
    assert_eq!(
        credential(&original_config, &handle, SubscriptionKind::Anthropic)
            .unwrap()
            .secret(),
        "synthetic-access-4"
    );
}

#[test]
fn config_namespace_is_stable_before_creation_and_across_canonical_path_spellings() {
    #[cfg(unix)]
    assert_eq!(
        profile_path(
            Path::new("/wisp-hex-fixture.toml"),
            "oauth:00112233445566778899aabbccddeeff"
        ),
        Some(PathBuf::from(
            "/oauth/f968eeaa949071c820faf48420bc0ebfa056d89d17f08cb8248055f2fb9fdfd3/oauth-00112233445566778899aabbccddeeff.json"
        ))
    );
    let folder = private_dir();
    let config = folder.path().join("config.toml");
    let handle = format!("oauth:{}", uuid::Uuid::new_v4().simple());
    let before_creation = profile_path(&config, &handle).unwrap();
    prepare(&config).unwrap();
    std::fs::write(&config, "fixture").unwrap();
    assert_eq!(profile_path(&config, &handle).unwrap(), before_creation);
    let child = folder.path().join("child");
    std::fs::create_dir(&child).unwrap();
    let equivalent = child.join("..").join("config.toml");
    assert_eq!(profile_path(&equivalent, &handle).unwrap(), before_creation);
    assert!(before_creation.starts_with(std::fs::canonicalize(folder.path()).unwrap()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_during_refresh_keeps_the_rotated_tokens_and_redacts_logs() {
    use crate::session::{MemoryStore, SessionLog, SessionStore};
    use wisp_protocol::log::LogEvent;
    let folder = private_dir();
    let path = profile(folder.path());
    save(&path, &tokens(0, now() - 1)).unwrap();
    let store = MemoryStore::default();
    let mut log = SessionLog::new(Box::new(store.clone()), "rotation");
    let abort: Arc<StdMutex<Option<tokio::task::AbortHandle>>> = Arc::new(StdMutex::new(None));
    let cancel = abort.clone();
    let (release, held) = std::sync::mpsc::channel::<()>();
    let (url, seen, server) = server_with(200, json!({"access_token":"synthetic-detached-access","refresh_token":"synthetic-detached-refresh","expires_in":3600}), async move {
        cancel.lock().unwrap().as_ref().unwrap().abort();
        tokio::task::block_in_place(|| held.recv().unwrap());
    }).await;
    let caller_path = path.clone();
    let caller_url = url.clone();
    let caller = tokio::spawn(async move {
        access_at(&caller_path, SubscriptionKind::ChatGpt, &caller_url, now()).await
    });
    *abort.lock().unwrap() = Some(caller.abort_handle());
    assert!(caller.await.unwrap_err().is_cancelled());
    let waiting = tokio::time::timeout(Duration::from_millis(300), settle()).await;
    assert!(waiting.is_err());
    release.send(()).unwrap();
    settle().await;
    let saved = read(&path, SubscriptionKind::ChatGpt).unwrap();
    assert_eq!(saved.generation, 1);
    assert_eq!(saved.refresh_token.expose(), "synthetic-detached-refresh");
    assert_eq!(
        access_at(&path, SubscriptionKind::ChatGpt, &url, now())
            .await
            .unwrap(),
        "synthetic-detached-access"
    );
    server.await.unwrap();
    assert_eq!(seen.lock().unwrap().len(), 1);
    log.record(LogEvent::Rejected {
        turn: Some(1),
        reason: "synthetic-detached-access synthetic-detached-refresh".into(),
        id: None,
        kind: None,
    });
    let records = store.read("rotation").unwrap();
    let encoded = serde_json::to_string(&records).unwrap();
    assert!(!encoded.contains("synthetic-detached"));
    assert!(encoded.contains("[redacted] [redacted]"));
}

#[tokio::test]
async fn chunked_oauth_response_is_bounded_without_content_length() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/token", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 4096];
        assert!(stream.read(&mut request).await.unwrap() > 0);
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let chunk = vec![b'x'; MAX_RESPONSE + 1];
        stream
            .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
            .await
            .unwrap();
        let _ = stream.write_all(&chunk).await;
        let _ = stream.write_all(b"\r\n0\r\n\r\n").await;
    });
    let result = exchange(
        &client().unwrap(),
        &url,
        SubscriptionKind::ChatGpt,
        &json!({"grant_type":"refresh_token"}),
        None,
    )
    .await;
    assert!(matches!(result, Err(AuthError::InvalidResponse)));
    server.await.unwrap();
}

#[tokio::test]
async fn failed_file_rotation_recovers_from_sidecar_without_another_exchange() {
    let folder = private_dir();
    let path = profile(folder.path());
    let original = tokens(0, now() - 1);
    save(&path, &original).unwrap();
    let blocked = path.clone();
    let (url, seen, server) = server_with(200, json!({"access_token":"synthetic-file-access","refresh_token":"synthetic-file-refresh","expires_in":3600}), async move {
        std::fs::remove_file(&blocked).unwrap();
        std::fs::create_dir(&blocked).unwrap();
    }).await;
    assert!(matches!(
        access_at(&path, SubscriptionKind::ChatGpt, &url, now()).await,
        Err(AuthError::Store)
    ));
    server.await.unwrap();
    let sidecar = std::fs::read_to_string(rotation_path(&path)).unwrap();
    assert!(!sidecar.contains(original.refresh_token.expose()));
    assert!(!secrets::readable_by_others(&rotation_path(&path)));
    pending().lock().await.remove(&path);
    std::fs::remove_dir(&path).unwrap();
    save(&path, &original).unwrap();
    assert_eq!(
        access_at(&path, SubscriptionKind::ChatGpt, &url, now())
            .await
            .unwrap(),
        "synthetic-file-access"
    );
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert_eq!(
        read(&path, SubscriptionKind::ChatGpt).unwrap().generation,
        1
    );
    assert!(!rotation_path(&path).exists());
    let mark = refused_path(&path);
    std::fs::write(&mark, "").unwrap();
    save(&path, &tokens(1, now() - 1)).unwrap();
    assert!(!mark.exists());
    std::fs::create_dir(&mark).unwrap();
    let (url, _, server) = server_with(401, json!({}), async {}).await;
    assert!(matches!(
        access_at(&path, SubscriptionKind::ChatGpt, &url, now()).await,
        Err(AuthError::Refused(401))
    ));
    server.await.unwrap();
    assert!(!refused(&path));
    let (url, _, server) = server_with(
        200,
        json!({"access_token":"synthetic-later-access","refresh_token":"synthetic-later-refresh","expires_in":3600}),
        async {},
    )
    .await;
    assert_eq!(
        access_at(&path, SubscriptionKind::ChatGpt, &url, now())
            .await
            .unwrap(),
        "synthetic-later-access"
    );
    server.await.unwrap();
    assert!(mark.is_dir());
    assert!(!rotation_path(&path).exists());
}

#[test]
fn oauth_store_moves_transfer_all_tokens_in_both_directions() {
    let folder = private_dir();
    let config = folder.path().join("config.toml");
    let handle = format!("oauth:{}", uuid::Uuid::new_v4().simple());
    let mut path = profile_path(&config, &handle).unwrap();
    let memory = MemoryKeyring::new();
    memory_store(&path, &memory);
    let original = tokens(7, now() + 3600);
    persist_profile(
        &path,
        SubscriptionKind::ChatGpt,
        &original,
        StoreKind::File,
        Some(&held('b')),
    )
    .unwrap();
    let keys = memory.store(TERMINAL_DEADLINE);
    let name = crate::endpoints::EndpointName::parse("plan").unwrap();
    secrets::apply(
        &config,
        PathShown::Given,
        secrets::Change::Add {
            name: name.clone(),
            endpoint: crate::config_file::NewEndpoint {
                provider: crate::endpoints::ProviderKind::OpenAi,
                auth: crate::endpoints::AuthKind::ChatGptSubscription,
                store: StoreKind::File,
                pricing: crate::endpoints::PricingMode::List,
            },
            record: secrets::StoredRecord::Key {
                key: Secret::new(&handle),
                url: None,
            },
            make_default: false,
        },
        &keys,
        secrets::LOCK_WAIT,
    )
    .unwrap();
    for target in [StoreKind::Keyring, StoreKind::File] {
        let items = memory.items();
        secrets::write_atomic(&refused_path(&path), &[], 0o600).unwrap();
        assert!(status(&path, SubscriptionKind::ChatGpt).unwrap().refused);
        assert_eq!(memory.items(), items);
        let marked = refused_path(&path);
        if target == StoreKind::File {
            let decoy = folder.path().join("decoy");
            secrets::write_atomic(&decoy, held('b').0.as_bytes(), 0o600).unwrap();
            std::fs::remove_file(account_path(&path)).unwrap();
            #[cfg(unix)]
            std::os::unix::fs::symlink(&decoy, account_path(&path)).unwrap();
        }
        secrets::apply(
            &config,
            PathShown::Given,
            secrets::Change::Edit {
                name: name.clone(),
                edit: secrets::Edit {
                    store: Some(target),
                    ..Default::default()
                },
            },
            &keys,
            secrets::LOCK_WAIT,
        )
        .unwrap();
        assert!(!path.exists());
        assert!(!marked.exists());
        let id = crate::config_file::ConfigText::read(&config, PathShown::Given)
            .unwrap()
            .current()
            .unwrap()
            .store_id
            .unwrap();
        let stored = match target {
            StoreKind::File => secrets::FileStore::beside(&config, PathShown::Given)
                .read_all(&id)
                .unwrap()
                .remove(&name)
                .unwrap(),
            StoreKind::Keyring => crate::secrets::keyring::KeyringAccess::read(&keys, &id, &name)
                .unwrap()
                .unwrap(),
        };
        let secrets::StoredRecord::Key { key, .. } = stored.record;
        path = profile_path(&config, key.expose()).unwrap();
        let found = read(&path, SubscriptionKind::ChatGpt).unwrap();
        assert!(found.access_token.expose() == original.access_token.expose());
        assert!(found.refresh_token.expose() == original.refresh_token.expose());
        assert_eq!(
            (found.expires_at, found.generation),
            (original.expires_at, original.generation)
        );
        assert_eq!(
            status(&path, SubscriptionKind::ChatGpt).unwrap().store,
            target
        );
        let local = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            local.contains("synthetic-access"),
            target == StoreKind::File
        );
        assert_eq!(memory.items().is_empty(), target == StoreKind::File);
        assert_eq!(
            read_account(&path),
            (target == StoreKind::Keyring).then(|| held('b').0)
        );
    }
}

#[test]
fn failed_oauth_store_move_preserves_the_source_tokens() {
    let folder = private_dir();
    let config = folder.path().join("config.toml");
    let handle = format!("oauth:{}", uuid::Uuid::new_v4().simple());
    let path = profile_path(&config, &handle).unwrap();
    let memory = MemoryKeyring::new();
    memory_store(&path, &memory);
    persist_profile(
        &path,
        SubscriptionKind::ChatGpt,
        &tokens(3, now() + 3600),
        StoreKind::File,
        Some(&held('c')),
    )
    .unwrap();
    let before = std::fs::read(&path).unwrap();
    memory.set_mode(MemoryMode::Failing(BackendError::Other));
    assert!(
        stage_move(
            &config,
            &handle,
            SubscriptionKind::ChatGpt,
            StoreKind::Keyring
        )
        .is_err()
    );
    assert!(std::fs::read(&path).unwrap() == before);
    assert_eq!(
        read(&path, SubscriptionKind::ChatGpt).unwrap().generation,
        3
    );
    assert_eq!(read_account(&path), Some(held('c').0));
}

#[test]
fn config_write_failure_during_relogin_keeps_old_oauth_credentials_resolvable() {
    use crate::config_file::{ConfigText, ConfigWriteError, NewEndpoint, WriteProblem};
    use crate::endpoints::{AuthKind, EndpointName, PricingMode, ProviderKind};
    use crate::secrets::keyring::KeyringAccess;
    use crate::secrets::{Change, Edit, FileStore, StoredRecord};
    for store in [StoreKind::File, StoreKind::Keyring] {
        let folder = private_dir();
        let config = folder.path().join("config.toml");
        let memory = MemoryKeyring::new();
        let keys = memory.store(TERMINAL_DEADLINE);
        let old = format!("oauth:{}", uuid::Uuid::new_v4().simple());
        let new = format!("oauth:{}", uuid::Uuid::new_v4().simple());
        for (handle, generation) in [(&old, 0), (&new, 1)] {
            let path = profile_path(&config, handle).unwrap();
            memory_store(&path, &memory);
            persist_profile(
                &path,
                SubscriptionKind::ChatGpt,
                &tokens(generation, now() + 3600),
                store,
                None,
            )
            .unwrap();
        }
        let name = EndpointName::parse("plan").unwrap();
        let record = |handle: &str| StoredRecord::Key {
            key: Secret::new(handle),
            url: None,
        };
        secrets::apply(
            &config,
            PathShown::Given,
            Change::Add {
                name: name.clone(),
                endpoint: NewEndpoint {
                    provider: ProviderKind::OpenAi,
                    auth: AuthKind::ChatGptSubscription,
                    store,
                    pricing: PricingMode::List,
                },
                record: record(&old),
                make_default: true,
            },
            &keys,
            secrets::LOCK_WAIT,
        )
        .unwrap();
        let before = std::fs::read(&config).unwrap();
        let result = crate::secrets::apply_with(
            &config,
            PathShown::Given,
            Change::Edit {
                name: name.clone(),
                edit: Edit {
                    record: Some(record(&new)),
                    ..Edit::default()
                },
            },
            &keys,
            secrets::LOCK_WAIT,
            &|| {
                Err(crate::secrets::ChangeError::Config(ConfigWriteError {
                    path: config.clone(),
                    shown: PathShown::Given,
                    problem: WriteProblem::Write(std::io::ErrorKind::PermissionDenied),
                }))
            },
        );
        assert!(result.is_err());
        assert!(std::fs::read(&config).unwrap() == before);
        let id = ConfigText::read(&config, PathShown::Given)
            .unwrap()
            .current()
            .unwrap()
            .store_id
            .unwrap();
        let stored = match store {
            StoreKind::File => FileStore::beside(&config, PathShown::Given)
                .read_all(&id)
                .unwrap()
                .remove(&name)
                .unwrap(),
            StoreKind::Keyring => KeyringAccess::read(&keys, &id, &name).unwrap().unwrap(),
        };
        let StoredRecord::Key { key, .. } = stored.record;
        let resolved = credential(&config, key.expose(), SubscriptionKind::ChatGpt).unwrap();
        assert!(resolved.secret() == "synthetic-access-0");
        let target = if store == StoreKind::File {
            StoreKind::Keyring
        } else {
            StoreKind::File
        };
        let failed_move = crate::secrets::apply_with(
            &config,
            PathShown::Given,
            Change::Edit {
                name: name.clone(),
                edit: Edit {
                    store: Some(target),
                    ..Edit::default()
                },
            },
            &keys,
            secrets::LOCK_WAIT,
            &|| {
                Err(crate::secrets::ChangeError::Config(ConfigWriteError {
                    path: config.clone(),
                    shown: PathShown::Given,
                    problem: WriteProblem::Write(std::io::ErrorKind::PermissionDenied),
                }))
            },
        );
        assert!(failed_move.is_err());
        assert!(std::fs::read(&config).unwrap() == before);
        assert!(
            credential(&config, &old, SubscriptionKind::ChatGpt)
                .unwrap()
                .secret()
                == "synthetic-access-0"
        );
        assert_eq!(
            status(
                &profile_path(&config, &old).unwrap(),
                SubscriptionKind::ChatGpt
            )
            .unwrap()
            .store,
            store
        );
    }
}

#[test]
fn oauth_recovery_files_are_ignored_alongside_profiles() {
    let patterns = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.gitignore"));
    assert!(patterns.lines().any(|line| line == "oauth-*.json"));
    assert!(patterns.lines().any(|line| line == "oauth-*.pending"));
    assert!(patterns.lines().any(|line| line == "oauth-*.account"));
}

#[test]
fn staged_profile_moves_hold_refresh_locks_until_commit_or_abort() {
    let folder = private_dir();
    let config = folder.path().join("config.toml");
    let handle = format!("oauth:{}", uuid::Uuid::new_v4().simple());
    let path = profile_path(&config, &handle).unwrap();
    persist_profile(
        &path,
        SubscriptionKind::ChatGpt,
        &tokens(0, now() + 3600),
        StoreKind::File,
        None,
    )
    .unwrap();
    let staged = stage_move(&config, &handle, SubscriptionKind::ChatGpt, StoreKind::File).unwrap();
    let target = profile_path(&config, staged.handle()).unwrap();
    assert!(pending().try_lock().is_err());
    assert!(path.exists() && target.exists());
    drop(staged);
    assert!(pending().try_lock().is_ok());
    assert!(path.exists() && !target.exists());
    let staged = stage_move(&config, &handle, SubscriptionKind::ChatGpt, StoreKind::File).unwrap();
    let target = profile_path(&config, staged.handle()).unwrap();
    staged.commit();
    assert!(pending().try_lock().is_ok());
    assert!(!path.exists() && target.exists());
    assert_eq!(
        read(&target, SubscriptionKind::ChatGpt).unwrap().generation,
        0
    );
}

#[tokio::test]
async fn pending_login_cancel_releases_its_ephemeral_listener() {
    use crate::subscription_login::PendingSubscriptionLogin;
    let first = std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range")
        .ok()
        .and_then(|range| range.split_whitespace().next()?.parse::<u16>().ok())
        .unwrap_or(32768);
    let mut port = first.saturating_sub(1 + (std::process::id() % 1000) as u16);
    let pending = loop {
        match PendingLogin::start_on(SubscriptionKind::ChatGpt, port).await {
            Ok(pending) => break pending,
            Err(_) => port -= 1,
        }
    };
    let address = pending
        .return_to
        .listener
        .as_ref()
        .unwrap()
        .local_addr()
        .unwrap();
    assert!(TcpListener::bind(address).await.is_err());
    let pending: Box<dyn PendingSubscriptionLogin> = Box::new(pending);
    pending.cancel();
    let rebound = TcpListener::bind(address).await.unwrap();
    assert_eq!(rebound.local_addr().unwrap(), address);
}

#[tokio::test]
async fn occupied_callback_port_refuses_login_without_stealing_listener() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let result = PendingLogin::start_on(SubscriptionKind::ChatGpt, address.port()).await;
    assert!(matches!(result, Err(AuthError::Callback)));
    assert_eq!(listener.local_addr().unwrap(), address);
}

#[tokio::test]
async fn forgetting_an_absent_profile_is_idempotent() {
    let dir = private_dir();
    let path = dir.path().join("absent").join("profile.json");
    forget(&path).await.unwrap();
    std::fs::create_dir(path.parent().unwrap()).unwrap();
    forget(&path).await.unwrap();
    forget(&path).await.unwrap();
    std::fs::write(&path, "synthetic-profile").unwrap();
    write_account(&path, Some(&held('d')));
    std::fs::write(refused_path(&path), "").unwrap();
    assert!(account_path(&path).exists());
    forget(&path).await.unwrap();
    let left = std::fs::read_dir(path.parent().unwrap()).unwrap();
    assert!(
        left.flatten()
            .all(|e| !e.file_name().to_string_lossy().starts_with("profile"))
    );
}

#[tokio::test]
async fn account_hash_takes_both_halves_and_never_fails_a_sign_in() {
    use SubscriptionKind::{Anthropic, ChatGpt};
    let claude = |account: Value, organization: Value| {
        account_hash(
            Anthropic,
            &json!({"account": account, "organization": organization}),
        )
        .map(|hash| hash.0)
    };
    let pair = claude(
        json!({"uuid": "synthetic-account", "email_address": "synthetic@example.invalid"}),
        json!({"uuid": "synthetic-organization"}),
    )
    .unwrap();
    assert_eq!(pair.len(), MAX_ACCOUNT);
    assert_ne!(
        claude(
            json!({"uuid": "synthetic-account"}),
            json!({"uuid": "synthetic-other-organization"})
        ),
        Some(pair.clone())
    );
    let good = openai(
        "chatgpt_user_id",
        json!("synthetic-account"),
        json!("synthetic-organization"),
    );
    let chatgpt = |answer: Value| account_hash(ChatGpt, &answer).map(|hash| hash.0);
    let workspace =
        chatgpt(json!({"id_token": jwt(&good), "access_token": "synthetic-access"})).unwrap();
    assert_ne!(workspace, pair);
    assert_eq!(
        chatgpt(json!({"access_token": jwt(&good)})),
        Some(workspace.clone())
    );
    assert_eq!(
        chatgpt(json!({"id_token": "e30.%%%.sig", "access_token": jwt(&good)})),
        Some(workspace.clone())
    );
    assert_eq!(
        chatgpt(
            json!({"access_token": jwt(&openai("user_id", json!("synthetic-account"), json!("synthetic-organization")))})
        ),
        Some(workspace)
    );
    let unknown = [
        (
            Anthropic,
            json!({"account": null, "organization": {"uuid": "synthetic-organization"}}),
        ),
        (Anthropic, json!({"account": {"uuid": "synthetic-account"}})),
        (
            Anthropic,
            json!({"account": {"uuid": ""}, "organization": {"uuid": "synthetic-organization"}}),
        ),
        (
            Anthropic,
            json!({"account": {"uuid": 7}, "organization": {"uuid": "synthetic-organization"}}),
        ),
        (ChatGpt, json!({})),
        (ChatGpt, json!({"id_token": "e30.%%%.sig"})),
        (
            ChatGpt,
            json!({"id_token": jwt(&openai("chatgpt_user_id", json!(7), json!("synthetic-organization")))}),
        ),
        (
            ChatGpt,
            json!({"id_token": jwt(&openai("chatgpt_user_id", json!(""), json!("")))}),
        ),
        (
            ChatGpt,
            json!({"id_token": jwt(&json!({"https://api.openai.com/auth": {"chatgpt_user_id": "synthetic-account"}}))}),
        ),
    ];
    for (kind, mut answer) in unknown {
        assert!(account_hash(kind, &answer).is_none(), "{answer}");
        let folder = private_dir();
        let config = folder.path().join("config.toml");
        answer["access_token"] = answer
            .get("access_token")
            .cloned()
            .unwrap_or(json!("synthetic-access"));
        answer["refresh_token"] = json!("synthetic-refresh");
        answer["expires_in"] = json!(3600);
        let (url, _, task) = server(200, answer).await;
        let (tokens, account) = exchange(&client().unwrap(), &url, kind, &json!({}), None)
            .await
            .unwrap();
        task.await.unwrap();
        assert!(account.is_none());
        let handle = Exchanged {
            kind,
            tokens,
            account,
        }
        .persist(&config, StoreKind::File)
        .unwrap();
        let path = profile_path(&config, &handle).unwrap();
        assert!(read(&path, kind).is_ok());
        assert!(!account_path(&path).exists());
    }
    let folder = private_dir();
    let path = profile(folder.path());
    std::fs::create_dir(account_path(&path)).unwrap();
    persist_profile(
        &path,
        ChatGpt,
        &tokens(0, now() + 3600),
        StoreKind::File,
        Some(&held('e')),
    )
    .unwrap();
    assert!(read(&path, ChatGpt).is_ok());
    assert_eq!(read_account(&path), None);
}

#[test]
fn held_by_reads_only_account_files_and_trusts_the_own_profile() {
    let folder = private_dir();
    let config = folder.path().join("config.toml");
    prepare(&config).unwrap();
    let at = |n: u8| profile_path(&config, &format!("oauth:{n:032x}")).unwrap();
    let signed = approved(Some(held('a')));
    assert!(signed.held_by(&config, None).is_empty());
    assert!(approved(None).held_by(&config, None).is_empty());
    let (claude, work, other) = (at(1), at(2), at(3));
    write_account(&claude, Some(&held('a')));
    write_account(&other, Some(&held('f')));
    let lock = claude.parent().unwrap().join(".lock");
    std::fs::write(&lock, held('a').0).unwrap();
    std::fs::write(claude.with_extension("stray"), held('a').0).unwrap();
    assert_eq!(signed.held_by(&config, None), vec![claude.clone()]);
    assert!(signed.held_by(&config, Some(&work)).is_empty());
    write_account(&work, Some(&held('a')));
    assert!(signed.held_by(&config, Some(&work)).is_empty());
    assert_eq!(
        signed.held_by(&config, Some(&other)),
        vec![claude.clone(), work.clone()]
    );
    std::fs::remove_file(account_path(&work)).unwrap();
    let handle = signed.persist(&config, StoreKind::File).unwrap();
    assert_eq!(
        read_account(&profile_path(&config, &handle).unwrap()),
        Some(held('a').0)
    );
}

#[test]
fn logout_cleanup_retains_remote_confirmation_without_changing_claude_errors() {
    for remote_revoked in [false, true] {
        let error = logout_cleanup_result(
            SubscriptionKind::ChatGpt,
            remote_revoked,
            Err(AuthError::Store),
        )
        .unwrap_err();
        assert!(
            matches!(error, AuthError::LogoutCleanup { remote_revoked: seen } if seen == remote_revoked)
        );
    }
    assert!(matches!(
        logout_cleanup_result(SubscriptionKind::Anthropic, false, Err(AuthError::Store)),
        Err(AuthError::Store)
    ));
}

#[tokio::test]
async fn only_the_providers_sign_in_pages_may_be_opened() {
    for kind in SubscriptionKind::ALL {
        let login = PendingLogin::start_on(kind, 0).await.unwrap();
        assert!(is_sign_in_page(login.authorization_url()));
    }
    for url in [
        "http://claude.ai/oauth/authorize?state=x",
        "https://claude.ai.example.com/oauth/authorize",
        "https://claude.ai/other",
        "https://user@claude.ai/oauth/authorize",
        "https://claude.ai:8443/oauth/authorize",
        "file:///etc/passwd",
        "javascript:alert(1)",
        "--help",
        "",
    ] {
        assert!(!is_sign_in_page(url), "{url}");
    }
    assert!(is_sign_in_page(
        "https://auth.openai.com/oauth/authorize?x=1"
    ));
}

#[tokio::test]
async fn account_ids_and_their_hash_are_never_shown_kept_or_sent() {
    const CANARY: &str = "IDENTITY_CANARY";
    let claims = json!({"email": "EMAIL_CANARY@example.invalid", "https://api.openai.com/auth": {"chatgpt_user_id": CANARY, "chatgpt_account_id": CANARY}});
    for (kind, answer) in [
        (
            SubscriptionKind::Anthropic,
            json!({"access_token": "synthetic-access", "refresh_token": "synthetic-refresh", "expires_in": 3600,
                "account": {"uuid": CANARY, "email_address": "EMAIL_CANARY@example.invalid"}, "organization": {"uuid": CANARY}}),
        ),
        (
            SubscriptionKind::ChatGpt,
            json!({"access_token": "synthetic-access", "id_token": jwt(&claims), "refresh_token": "synthetic-refresh", "expires_in": 3600}),
        ),
    ] {
        let folder = private_dir();
        let config = folder.path().join("config.toml");
        let mut login = PendingLogin::start(SubscriptionKind::Anthropic)
            .await
            .unwrap();
        login.kind = kind;
        let pasted = format!("synthetic-code#{}", login.return_to.state.expose());
        let pending = format!("{login:?}");
        let (url, _, task) = server(200, answer).await;
        let exchanged = login.finish_at(Some(&pasted), &config, &url).await.unwrap();
        task.await.unwrap();
        let hash = exchanged.account.as_ref().unwrap().0.clone();
        let shown = format!(
            "{pending} {exchanged:?} {:?} {:?}",
            exchanged.tokens, exchanged.account
        );
        let handle = exchanged.persist(&config, StoreKind::File).unwrap();
        let path = profile_path(&config, &handle).unwrap();
        let mut files = vec![];
        let mut stack = vec![folder.path().to_owned()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap().path();
                if entry.is_dir() {
                    stack.push(entry);
                } else {
                    files.push((entry.clone(), std::fs::read_to_string(&entry).unwrap()));
                }
            }
        }
        for (file, text) in &files {
            assert!(
                !text.contains(CANARY) && !text.contains("EMAIL_CANARY"),
                "{file:?}"
            );
            assert_eq!(
                text.contains(&hash),
                *file == account_path(&path),
                "{file:?}"
            );
        }
        let (url, seen, task) = server(
            200,
            json!({"access_token": "synthetic-later", "expires_in": 3600}),
        )
        .await;
        access_at(&path, kind, &url, now() + 7200).await.unwrap();
        task.await.unwrap();
        let later = seen.lock().unwrap().join("\n");
        for text in [&shown, &later] {
            assert!(
                !text.contains(CANARY) && !text.contains("EMAIL_CANARY"),
                "{text}"
            );
            assert!(!text.contains(&hash), "{text}");
        }
    }
}

#[test]
fn parse_redirect_url_percent_decodes_validates_state_and_allows_retry_on_error() {
    let login = PendingLogin::start_manual_antigravity().unwrap();
    assert!(!login.uses_loopback());
    let query: BTreeMap<_, _> = login.url.query_pairs().collect();
    assert_eq!(query["redirect_uri"], ANTIGRAVITY_MANUAL_REDIRECT);
    assert_eq!(query["access_type"], "offline");
    assert_eq!(query["prompt"], "consent");
    assert!(!login.authorization_url().contains(ANTIGRAVITY_SECRET));
    let state = login.return_to.state.expose();
    assert!(matches!(
        login.check(Some("")),
        Err(AuthError::Pasted(CodeProblem::Empty))
    ));
    assert!(matches!(
        login.check(Some("https://evil.example/oauth-callback?code=c&state=s")),
        Err(AuthError::Pasted(CodeProblem::NoState))
    ));
    assert!(matches!(
        login.check(Some(
            "http://127.0.0.1:51121/oauth-callback?code=4%2F0AeanS_code&state=wrong"
        )),
        Err(AuthError::State)
    ));
    let valid = format!(
        "http://127.0.0.1:51121/oauth-callback?code=4%2F0AeanS_code%2B1&state={state}&scope=email"
    );
    login.check(Some(&valid)).unwrap();
    let decoded = parse_redirect_url(&valid, state, "/oauth-callback").unwrap();
    assert_eq!(decoded, "4/0AeanS_code+1");
}

#[tokio::test]
async fn start_with_mode_keeps_chatgpt_on_1455_and_switches_only_antigravity_headless() {
    let antigravity_loopback = PendingLogin::start_with_mode(SubscriptionKind::Antigravity, true)
        .await
        .unwrap();
    assert!(antigravity_loopback.uses_loopback());
    assert!(
        antigravity_loopback
            .return_to
            .url
            .starts_with("http://127.0.0.1:")
    );
    assert!(
        antigravity_loopback
            .return_to
            .url
            .ends_with("/oauth-callback")
    );
    let antigravity_manual = PendingLogin::start_with_mode(SubscriptionKind::Antigravity, false)
        .await
        .unwrap();
    assert!(!antigravity_manual.uses_loopback());
    assert_eq!(
        antigravity_manual.return_to.url,
        ANTIGRAVITY_MANUAL_REDIRECT
    );
}

#[test]
fn antigravity_account_hash_hashes_jwt_sub_and_never_keeps_raw_claims() {
    let id_token = jwt(&json!({
        "sub": "109876543210987654321",
        "email": "CANARY_USER@example.invalid"
    }));
    let answer = json!({
        "access_token": "ya29.synthetic-access",
        "refresh_token": "1//synthetic-refresh",
        "id_token": id_token,
        "expires_in": 3600
    });
    let hash = account_hash(SubscriptionKind::Antigravity, &answer).unwrap();
    let expected = crate::session::sha256_json_hex(&(
        "wisp-account-1",
        SubscriptionKind::Antigravity,
        "109876543210987654321",
        "google",
    ))
    .unwrap();
    assert_eq!(hash.0, expected);
    assert!(!hash.0.contains("10987654321"));
}

#[tokio::test]
async fn antigravity_exchange_and_refresh_send_form_encoded_client_secret_and_preserve_project_id()
{
    let folder = private_dir();
    let config = folder.path().join("config.toml");
    let login = PendingLogin::start_manual_antigravity().unwrap();
    let state = login.return_to.state.expose().to_owned();
    let id_token = jwt(&json!({"sub": "google-sub-canary-1"}));
    let (url, seen, task) = server(
        200,
        json!({
            "access_token": "ya29.initial-access",
            "refresh_token": "1//initial-refresh",
            "id_token": id_token,
            "expires_in": 3600
        }),
    )
    .await;
    let pasted =
        format!("http://127.0.0.1:51121/oauth-callback?code=4%2Fauth_code_1&state={state}");
    let exchanged = login.finish_at(Some(&pasted), &config, &url).await.unwrap();
    task.await.unwrap();
    let first_req = seen.lock().unwrap()[0].clone();
    assert!(
        first_req
            .to_ascii_lowercase()
            .contains("content-type: application/x-www-form-urlencoded"),
        "{first_req}"
    );
    let first_body = first_req.split("\r\n\r\n").nth(1).unwrap();
    let parsed_first: BTreeMap<String, String> =
        Url::parse(&format!("https://localhost/?{first_body}"))
            .unwrap()
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
    assert_eq!(parsed_first["grant_type"], "authorization_code");
    assert_eq!(parsed_first["code"], "4/auth_code_1");
    assert_eq!(parsed_first["client_id"], ANTIGRAVITY_CLIENT);
    assert_eq!(parsed_first["client_secret"], ANTIGRAVITY_SECRET);

    let handle = exchanged.persist(&config, StoreKind::File).unwrap();
    let path = profile_path(&config, &handle).unwrap();
    save_project_id(&path, "cloudaicompanion-proj-redacted-99").unwrap();
    assert_eq!(
        read_project_id(&path).as_deref(),
        Some("cloudaicompanion-proj-redacted-99")
    );
    assert!(
        !crate::session::without_registered_secrets("project cloudaicompanion-proj-redacted-99")
            .contains("cloudaicompanion-proj-redacted-99")
    );

    let (refresh_url, refresh_seen, refresh_task) = server(
        200,
        json!({
            "access_token": "ya29.refreshed-access",
            "expires_in": 3600
        }),
    )
    .await;
    let refreshed = access_at(
        &path,
        SubscriptionKind::Antigravity,
        &refresh_url,
        now() + 7200,
    )
    .await
    .unwrap();
    refresh_task.await.unwrap();
    assert_eq!(refreshed, "ya29.refreshed-access");
    assert_eq!(
        read_project_id(&path).as_deref(),
        Some("cloudaicompanion-proj-redacted-99")
    );
    let refresh_req = refresh_seen.lock().unwrap()[0].clone();
    let refresh_body = refresh_req.split("\r\n\r\n").nth(1).unwrap();
    let parsed_refresh: BTreeMap<String, String> =
        Url::parse(&format!("https://localhost/?{refresh_body}"))
            .unwrap()
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
    assert_eq!(parsed_refresh["grant_type"], "refresh_token");
    assert_eq!(parsed_refresh["client_id"], ANTIGRAVITY_CLIENT);
    assert_eq!(parsed_refresh["client_secret"], ANTIGRAVITY_SECRET);
}

#[tokio::test]
async fn antigravity_loopback_callback_accepts_oauth_callback_path() {
    let folder = private_dir();
    let config = folder.path().join("config.toml");
    let login = PendingLogin::start_on(SubscriptionKind::Antigravity, 0)
        .await
        .unwrap();
    let state = login.return_to.state.expose().to_owned();
    let callback_url = Url::parse(&login.return_to.url).unwrap();
    let port = callback_url.port().unwrap();
    let (token_endpoint, _, token_task) = server(
        200,
        json!({
            "access_token": "ya29.loopback-access",
            "refresh_token": "1//loopback-refresh",
            "expires_in": 3600
        }),
    )
    .await;
    let browser = tokio::spawn(async move {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let req = format!(
            "GET /oauth-callback?code=loopback-code-7&state={state} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"
        );
        stream.write_all(req.as_bytes()).await.unwrap();
        let mut resp = Vec::new();
        stream.read_to_end(&mut resp).await.unwrap();
        String::from_utf8(resp).unwrap()
    });
    let exchanged = within(
        "antigravity loopback exchange",
        login.finish_at(None, &config, &token_endpoint),
    )
    .await
    .unwrap();
    token_task.await.unwrap();
    let page = browser.await.unwrap();
    assert!(page.contains("Sign-in received. Return to Wisp."), "{page}");
    assert_eq!(
        exchanged.tokens.access_token.expose(),
        "ya29.loopback-access"
    );
}

use crate::mcp_test as mcp_http;

#[tokio::test]
async fn mcp_credentials_use_the_account_profile_store_and_honor_its_location() {
    let folder = private_dir();
    let config = folder.path().join("config.toml");
    let path = mcp_sign_in_profile(&config, "local").unwrap();
    let canonical = crate::config_file::canonical_config(&config).unwrap();
    let hash = crate::session::sha256_bytes_hex(canonical.as_os_str().as_encoded_bytes());
    let identity = crate::session::sha256_hex(
        &serde_json::to_string(&(hash.clone(), "sign-in/local")).unwrap(),
    );
    assert_eq!(
        path,
        canonical
            .parent()
            .unwrap()
            .join(crate::secrets::OAUTH)
            .join(hash)
            .join(format!("oauth-mcp-{identity}.json"))
    );
    assert_ne!(path, mcp_profile(&config, "keys/local").unwrap());
    assert_ne!(
        path,
        mcp_profile(&config, "https://server.example.invalid/mcp").unwrap()
    );
    let memory = MemoryKeyring::new();
    memory_store(&path, &memory);
    let record = json!({"client_id":"CANARY-client", "client_secret":"CANARY-client-secret", "access_token":"CANARY-access", "refresh_token":"CANARY-refresh"});
    let persist = |kind| {
        let locks = lock_profile(&path).unwrap();
        persist_value_locked(&path, &record, kind, &locks).unwrap();
    };
    let clear = |path: &Path| {
        let locks = lock_profile(path)?;
        clear_value_locked(path, &locks)
    };
    persist(StoreKind::Keyring);
    let location = std::fs::read_to_string(&path).unwrap();
    assert!(!location.contains("CANARY"));
    assert!(!location.contains("\"kind\""));
    assert_eq!(read_value::<Value>(&path).unwrap(), Some(record.clone()));
    let copied = mcp_sign_in_profile(&folder.path().join("copy.toml"), "local").unwrap();
    std::fs::copy(&path, &copied).unwrap();
    assert!(read_value::<Value>(&copied).is_err());
    assert!(clear(&copied).is_err());
    assert!(memory.items().contains_key(&account(&path)));
    std::fs::remove_file(copied).unwrap();

    persist(StoreKind::File);
    assert!(keyring_location(&path).is_some());
    clear(&path).unwrap();
    assert!(!path.exists());
    assert!(memory.items().is_empty());
    persist(StoreKind::File);
    persist(StoreKind::Keyring);
    assert!(keyring_location(&path).is_none());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    clear(&path).unwrap();
    let server = mcp_http::Server::start(true, None).await;
    let service = crate::mcp::Service::new(BTreeMap::new(), folder.path().to_owned(), None)
        .with_config(config.clone(), PathShown::Given)
        .with_store(StoreKind::Keyring);
    service
        .add(
            "local",
            crate::config::McpServer {
                keys: Vec::new(),
                command: server.address(),
                args: Vec::new(),
            },
        )
        .await
        .unwrap();
    let path = mcp_sign_in_profile(&config, "local").unwrap();
    memory_store(&path, &memory);
    let unused = MemoryKeyring::new();
    memory_store(&mcp_profile(&config, &server.address()).unwrap(), &unused);
    let link = service.sign_in_start("local").await.unwrap().unwrap();
    assert!(link.starts_with(&server.issuer()));
    service
        .sign_in_finish("local", &mcp_http::redirected(&link, None))
        .await
        .unwrap();
    assert!(!memory.items().is_empty());
    assert!(!std::fs::read_to_string(&path).unwrap().contains("CANARY"));
    server.version(2);
    service.connect("local").await.unwrap();
    assert_eq!(
        service.saved("local").unwrap().tools[0].input_schema["properties"]["version"]["const"],
        2
    );
    server.requests(|requests| {
        assert!(requests.iter().any(|request| request.path == "/token"
            && request.headers.contains_key("authorization")
            && request.body["code"] == mcp_http::CODE))
    });
    assert_eq!(unused.state().opened, 0);
    server.revoke();
    assert!(
        service
            .call(
                "mcp__local__echo",
                json!({}),
                Duration::from_secs(2),
                tokio_util::sync::CancellationToken::new()
            )
            .await
            .is_err()
    );
    assert_eq!(service.rows()[0].state, crate::mcp::State::NeedsSignIn);
    memory.set_mode(MemoryMode::Failing(BackendError::NoStorageAccess));
    assert_eq!(
        service.remove("local").await.unwrap_err(),
        "couldn't use the sign-in store"
    );
    assert!(path.exists());
    assert!(!memory.items().is_empty());
    assert!(
        crate::config_file::ConfigText::read(&config, PathShown::Given)
            .unwrap()
            .current()
            .unwrap()
            .mcp_servers
            .contains_key("local")
    );
    memory.set_mode(MemoryMode::Working);
    memory_store(&path, &memory);
    assert!(read_value::<Value>(&path).unwrap().is_some());
    let moved = mcp_http::Server::start(true, Some(server.issuer())).await;
    let edit = |address: &str| {
        let current = std::fs::read_to_string(&config).unwrap();
        let current = current
            .replace(&server.address(), address)
            .replace(&moved.address(), address);
        crate::secrets::write_atomic(&config, current.as_bytes(), 0o600).unwrap();
        let servers = crate::config_file::ConfigText::read(&config, PathShown::Given)
            .unwrap()
            .current()
            .unwrap()
            .mcp_servers;
        crate::mcp::Service::new(servers, folder.path().to_owned(), None)
            .with_config(config.clone(), PathShown::Given)
            .with_store(StoreKind::Keyring)
    };
    let changed = edit(&moved.address());
    assert!(changed.connect("local").await.is_err());
    assert!(!path.exists());
    assert!(memory.items().is_empty());
    moved.requests(|requests| {
        assert!(requests.iter().all(|request| {
            !request.headers.contains_key("authorization") && request.path != "/token"
        }))
    });
    let link = changed.sign_in_start("local").await.unwrap().unwrap();
    changed
        .sign_in_finish("local", &mcp_http::redirected(&link, None))
        .await
        .unwrap();
    assert!(path.exists());
    assert!(!memory.items().is_empty());
    let changed = edit("CANARY-stdio-command");
    changed.remove("local").await.unwrap();
    assert!(!path.exists());
    assert!(memory.items().is_empty());
    assert!(
        !crate::config_file::ConfigText::read(&config, PathShown::Given)
            .unwrap()
            .current()
            .unwrap()
            .mcp_servers
            .contains_key("local")
    );
}

#[tokio::test]
async fn a_shared_redirect_checks_its_path_duplicates_state_and_expiry() {
    let mut returned = RedirectReturn::new(
        "http://127.0.0.1:8123/expected".into(),
        Secret::new("CANARY-state"),
        None,
    );
    let address = "http://127.0.0.1:8123/expected?code=CANARY-code&state=CANARY-state";
    assert_eq!(
        returned.pasted(address).unwrap().code.expose(),
        "CANARY-code"
    );
    assert!(
        returned
            .pasted(&address.replace("/expected", "/other"))
            .is_err()
    );
    assert!(
        returned
            .pasted(&format!("{address}&state=CANARY-state"))
            .is_err()
    );
    returned.started = tokio::time::Instant::now() - LOGIN_TIMEOUT;
    assert!(matches!(returned.pasted(address), Err(AuthError::Expired)));
}
