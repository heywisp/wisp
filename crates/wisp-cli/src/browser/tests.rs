use super::*;

const PAGE: &str = "https://claude.ai/oauth/authorize?code=true&state=URL_CANARY";

const SOCKET: &str = "/run/user/1000/vscode-ipc-0.sock";

fn live(path: &str) -> bool {
    path == SOCKET
}

fn all((env, macos): &(Env, bool)) -> Vec<Vec<String>> {
    candidates(env, &live, *macos, PAGE)
}

fn first(env: &(Env, bool)) -> Option<Vec<String>> {
    all(env).into_iter().next()
}

fn words(list: &[&str]) -> Vec<String> {
    list.iter().map(|word| (*word).to_owned()).collect()
}

#[test]
fn the_opener_tries_browser_then_the_platform_in_order() {
    let wsl = (
        Env::from_pairs([("WSL_DISTRO_NAME", "Ubuntu"), ("DISPLAY", ":0")]),
        false,
    );
    assert_eq!(
        all(&wsl),
        vec![
            words(&["wslview", PAGE]),
            words(&["rundll32.exe", "url.dll,FileProtocolHandler", PAGE]),
        ]
    );
    let browser = (
        Env::from_pairs([
            ("BROWSER", "firefox --new-tab %s:chromium"),
            ("WSL_DISTRO_NAME", "Ubuntu"),
        ]),
        false,
    );
    assert_eq!(
        first(&browser),
        Some(words(&["firefox", "--new-tab", PAGE]))
    );
    assert_eq!(all(&browser).len(), 3);
    let plain = (
        Env::from_pairs([("BROWSER", "/usr/bin/firefox"), ("DISPLAY", ":0")]),
        false,
    );
    assert_eq!(
        all(&plain),
        vec![
            words(&["/usr/bin/firefox", PAGE]),
            words(&["xdg-open", PAGE])
        ]
    );
    let lynx = (
        Env::from_pairs([("BROWSER", "/usr/bin/www-browser:w3m"), ("DISPLAY", ":0")]),
        false,
    );
    assert_eq!(all(&lynx), vec![words(&["xdg-open", PAGE])]);
    let mac = (Env::from_pairs([]), true);
    assert_eq!(first(&mac), Some(words(&["open", PAGE])));
    let wayland = (Env::from_pairs([("WAYLAND_DISPLAY", "wayland-0")]), false);
    assert_eq!(first(&wayland), Some(words(&["xdg-open", PAGE])));
    let vscode = (
        Env::from_pairs([
            ("SSH_CONNECTION", "10.0.0.1 22 10.0.0.2 22"),
            ("BROWSER", "/opt/vscode-server/bin/helpers/browser.sh"),
            ("VSCODE_IPC_HOOK_CLI", SOCKET),
        ]),
        false,
    );
    assert_eq!(
        all(&vscode),
        vec![words(&["/opt/vscode-server/bin/helpers/browser.sh", PAGE])]
    );
    let remote_mac = (vscode.0, true);
    assert_eq!(
        all(&remote_mac),
        vec![words(&["/opt/vscode-server/bin/helpers/browser.sh", PAGE])]
    );
}

#[test]
fn the_opener_opens_nothing_under_ci_over_ssh_without_a_display_or_with_no_desktop() {
    for env in [
        (Env::from_pairs([]), false),
        (
            Env::from_pairs([
                ("CI", "true"),
                ("WSL_DISTRO_NAME", "Ubuntu"),
                ("BROWSER", "firefox"),
            ]),
            false,
        ),
        (
            Env::from_pairs([
                ("SSH_CONNECTION", "10.0.0.1 22 10.0.0.2 22"),
                ("BROWSER", "firefox"),
            ]),
            false,
        ),
        (Env::from_pairs([("BROWSER", "")]), false),
        (
            Env::from_pairs([
                ("SSH_CONNECTION", "10.0.0.1 22 10.0.0.2 22"),
                ("BROWSER", "/opt/vscode-server/bin/helpers/browser.sh"),
                ("VSCODE_IPC_HOOK_CLI", "/run/user/1000/vscode-ipc-gone.sock"),
            ]),
            false,
        ),
        (
            Env::from_pairs([("SSH_CLIENT", "10.0.0.1 22 22"), ("BROWSER", "firefox")]),
            false,
        ),
        (
            Env::from_pairs([("CI", "true"), ("BROWSER", "firefox"), ("DISPLAY", ":0")]),
            false,
        ),
        (
            Env::from_pairs([
                ("SSH_CONNECTION", "10.0.0.1 22 10.0.0.2 22"),
                ("BROWSER", "/usr/bin/www-browser"),
                ("VSCODE_IPC_HOOK_CLI", SOCKET),
            ]),
            false,
        ),
    ] {
        assert!(all(&env).is_empty(), "{env:?}");
    }
    let forwarded = (
        Env::from_pairs([
            ("SSH_CONNECTION", "10.0.0.1 22 10.0.0.2 22"),
            ("DISPLAY", ":0"),
        ]),
        false,
    );
    assert_eq!(first(&forwarded), Some(words(&["xdg-open", PAGE])));
}

#[test]
fn the_opener_takes_the_link_as_one_argument_and_only_the_sign_in_pages() {
    let env = (
        Env::from_pairs([("BROWSER", "sh -c %s"), ("DISPLAY", ":0")]),
        false,
    );
    let mut seen = Vec::new();
    let mut spawn = |command: &[String]| {
        seen.push(command.to_vec());
        false
    };
    assert!(!open_with(
        &env.0,
        &live,
        env.1,
        PAGE,
        wisp_core::oauth::is_sign_in_page,
        &mut spawn
    ));
    assert_eq!(
        seen,
        vec![words(&["sh", "-c", PAGE]), words(&["xdg-open", PAGE])]
    );
    for url in [
        "https://claude.ai.evil.example/oauth/authorize",
        "http://claude.ai/oauth/authorize",
        "https://example.com/;rm -rf",
        "$(touch x)",
    ] {
        let mut called = false;
        assert!(!open_with(
            &env.0,
            &live,
            env.1,
            url,
            wisp_core::oauth::is_sign_in_page,
            &mut |_: &[String]| {
                called = true;
                true
            }
        ));
        assert!(!called, "{url}");
    }
    let mut tries = 0;
    assert!(open_with(
        &env.0,
        &live,
        env.1,
        PAGE,
        wisp_core::oauth::is_sign_in_page,
        &mut |_: &[String]| {
            tries += 1;
            tries == 2
        }
    ));
    assert_eq!(tries, 2);
    let link = "https://example.invalid/authorize?state=CANARY-state&scope=read+write";
    assert!(open_with(
        &env.0,
        &live,
        env.1,
        link,
        wisp_core::mcp::is_sign_in_link,
        &mut |command| {
            assert_eq!(command, words(&["sh", "-c", link]));
            true
        }
    ));
    for link in [
        "https://example.invalid/$(CANARY)",
        "https://user@example.invalid/authorize",
        "http://example.invalid/authorize",
    ] {
        assert!(!open_with(
            &env.0,
            &live,
            env.1,
            link,
            wisp_core::mcp::is_sign_in_link,
            &mut |_| panic!("refused link was spawned")
        ));
    }
}

#[test]
fn a_failed_spawn_names_nothing_of_the_link() {
    let missing = words(&["wisp-no-such-browser-program", PAGE]);
    assert!(!spawn(&missing));
}

#[test]
fn can_open_in_agrees_with_candidate_discovery_across_graphical_and_headless_hosts() {
    assert!(can_open_in(
        &Env::from_pairs([("DISPLAY", ":0")]),
        &live,
        false
    ));
    assert!(can_open_in(
        &Env::from_pairs([("WAYLAND_DISPLAY", "wayland-0")]),
        &live,
        false
    ));
    assert!(can_open_in(&Env::from_pairs([]), &live, true));
    assert!(can_open_in(
        &Env::from_pairs([
            ("SSH_CONNECTION", "10.0.0.1 22 10.0.0.2 22"),
            ("BROWSER", "/opt/vscode-server/bin/helpers/browser.sh"),
            ("VSCODE_IPC_HOOK_CLI", SOCKET),
        ]),
        &live,
        false,
    ));
    assert!(!can_open_in(&Env::from_pairs([]), &live, false));
    assert!(!can_open_in(
        &Env::from_pairs([("CI", "true"), ("DISPLAY", ":0")]),
        &live,
        false
    ));
    assert!(!can_open_in(
        &Env::from_pairs([
            ("SSH_CONNECTION", "10.0.0.1 22 10.0.0.2 22"),
            ("BROWSER", "firefox"),
        ]),
        &live,
        false,
    ));
}

#[tokio::test]
async fn the_relay_opens_each_live_link_and_passes_other_events_unchanged() {
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    let link = "https://example.invalid/authorize?state=CANARY-state";
    let note = json!({"type":"plugins_changed","links":[{"plugin":"mcp","message":"sign in to local at example.invalid","link":link}]});
    let events: Vec<AgentEvent> = [
        note.clone(),
        json!({"type":"agent","agent":"child","event":note}),
        json!({"type":"turn_started","turn":1}),
    ]
    .into_iter()
    .map(|value| serde_json::from_value(value).unwrap())
    .collect();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorded = seen.clone();
    let (send, receive) = mpsc::channel(1);
    let mut opened = opening(
        receive,
        Arc::new(move |link| {
            let mut seen = recorded.lock().unwrap();
            seen.push(link.to_owned());
            seen.len() == 1
        }),
        None,
    );
    for (index, mut event) in events.into_iter().enumerate() {
        send.send(event.clone()).await.unwrap();
        for note in event.links_mut() {
            note.opened = Some(index == 0);
        }
        let received = opened.recv().await.unwrap();
        assert_eq!(received, event);
        assert!(
            !serde_json::to_value(&received)
                .unwrap()
                .to_string()
                .contains("opened")
        );
    }
    assert_eq!(*seen.lock().unwrap(), [link, link]);
    drop(send);
    assert!(opened.recv().await.is_none());
    let (send, receive) = mpsc::channel(1);
    drop(opening(receive, Arc::new(|_| false), None));
    send.closed().await;
}

#[tokio::test]
async fn the_relay_scrubs_events_queued_before_a_secret_registered() {
    use std::sync::Arc;
    use wisp_core::session::{REDACTED, register_secrets};
    use wisp_start::scrub::testing::{CANARY, queued, scrub};

    let events = queued();
    let (send, receive) = mpsc::channel(4);
    let mut opened = opening(receive, Arc::new(|_| false), Some(scrub()));
    for event in &events {
        send.send(event.clone()).await.unwrap();
    }
    register_secrets([CANARY.to_owned()]);
    for _ in &events {
        let shown = serde_json::to_string(&opened.recv().await.unwrap()).unwrap();
        assert!(!shown.contains(CANARY), "{shown}");
        assert_eq!(shown.matches(REDACTED).count(), 1, "{shown}");
    }
}
