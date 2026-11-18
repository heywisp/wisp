use super::*;
use crate::mcp_test as http;
use wisp_protocol::{MailFrom, TurnOrigin, render_mail};

fn setup(service: crate::mcp::Service) -> Setup {
    Setup {
        plugins: vec!["manage", "search"],
        full_screen: true,
        mcp: service,
        ..Setup::default()
    }
}

async fn waiting(
    name: &str,
    after: Vec<Step>,
) -> (ScratchDir, http::Server, crate::mcp::Service, Run, String) {
    let env = ScratchDir::new("wisp-agents-test-", name);
    let server = http::Server::start(true, None).await;
    let service = crate::mcp::Service::new(
        BTreeMap::new(),
        env.root.clone(),
        Some(env.root.join("mcp")),
    )
    .with_config(
        env.root.join("config.toml"),
        crate::endpoints::PathShown::Given,
    );
    let mut steps = vec![
        uses(&[call(
            "add",
            "use",
            json!({"name":"manage","input":{"action":"add","kind":"server","name":"linear","from":server.address()}}),
        )]),
        done("Waiting."),
    ];
    steps.extend(after);
    let mut run = start(name, setup(service.clone()), vec![("/", steps)]).await;
    run.say("Add linear.").await;
    run.root_ended().await;
    assert_eq!(
        result_of(&run.requests("/")[1], "add").1,
        wisp_tool_text::manage::added_waiting("linear")
    );
    let link = run
        .seen
        .iter()
        .find_map(|event| match event {
            AgentEvent::PluginsChanged {
                links: Some(links), ..
            } => links.first().map(|note| note.link.expose().to_owned()),
            _ => None,
        })
        .unwrap();
    (env, server, service, run, link)
}

async fn notice(run: &mut Run, message: &str) -> MailFrom {
    let AgentEvent::InputAccepted { from: Some(from), text, .. } = run.until("the sign-in notice", |event| matches!(event, AgentEvent::InputAccepted { from: Some(from), .. } if from.plugin.as_deref() == Some("mcp"))).await else { panic!("not a notice") };
    assert_eq!(from.agent, AgentPath::root());
    assert_eq!(from.kind, MailKind::Notice);
    assert!(from.wake);
    assert_eq!(
        text,
        render_mail(&AgentPath::root(), MailKind::Notice, message)
    );
    run.describe().await;
    tokio::time::pause();
    tokio::time::advance(crate::agent::WAKE_SETTLE + Duration::from_millis(1)).await;
    tokio::time::resume();
    run.root_ended().await;
    from
}

#[tokio::test(flavor = "current_thread")]
async fn allow_wakes_the_waiting_main_agent_once_and_its_notice_keeps_the_prefix_through_a_resume()
{
    let (_env, _server, service, mut run, link) = waiting(
        "sign-in-allow",
        vec![done("Signed in."), done("Still here.")],
    )
    .await;
    let before = run.requests("/")[1].clone();
    let page = reqwest::get(http::redirected(&link, None))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(page, "Signed in to linear. You can close this page.");
    let message = "linear signed in · 1 tool";
    let from = notice(&mut run, message).await;
    let woke = run.requests("/").last().cloned().unwrap();
    grows_from(&woke, &before, "the sign-in wake");
    let rendered = render_mail(&from.agent, from.kind, message);
    assert_eq!(woke.all_text().matches(&rendered).count(), 1);
    assert!(!run.seen.iter().any(|event| matches!(event, AgentEvent::PluginsChanged { notes, .. } if notes.iter().any(|note| note.message == message))));
    assert_eq!(
        run.records("/")
            .iter()
            .filter(|event| matches!(
                event,
                LogEvent::TurnStarted {
                    origin: Some(TurnOrigin::Mail),
                    ..
                }
            ))
            .count(),
        1
    );
    run.say("Anything else?").await;
    run.root_ended().await;
    let later = run.requests("/").last().cloned().unwrap();
    grows_from(&later, &woke, "a later message");
    let run = run.quit().await;
    assert_eq!(run.records("/").iter().filter(|event| matches!(event, LogEvent::InputAccepted { from: Some(from), .. } if from.plugin.as_deref() == Some("mcp"))).count(), 1);
    let mut run = resume::resume(run, setup(service), vec![("/", vec![done("Resumed.")])])
        .await
        .run;
    run.say("After resume.").await;
    run.root_ended().await;
    let resumed = run.requests("/").last().cloned().unwrap();
    grows_from(&resumed, &later, "the resumed request");
    assert_eq!(resumed.all_text().matches(&rendered).count(), 1);
    let run = run.quit().await;
    assert_eq!(
        run.records("/")
            .iter()
            .filter(|event| matches!(
                event,
                LogEvent::TurnStarted {
                    origin: Some(TurnOrigin::Mail),
                    ..
                }
            ))
            .count(),
        1
    );
}

#[tokio::test(flavor = "current_thread")]
async fn deny_tells_the_waiting_agent_and_a_server_signed_in_elsewhere_is_added_without_a_wait() {
    let (env, server, service, mut run, link) = waiting(
        "sign-in-deny",
        vec![
            done("Not signed in."),
            uses(&[call(
                "again",
                "use",
                json!({"name":"manage","input":{"action":"add","kind":"server","name":"linear"}}),
            )]),
            done("Added."),
        ],
    )
    .await;
    let mut address = reqwest::Url::parse(&http::redirected(&link, None)).unwrap();
    address
        .query_pairs_mut()
        .append_pair("error", "access_denied");
    assert_eq!(
        reqwest::get(address).await.unwrap().text().await.unwrap(),
        "Sign-in declined. Return to Wisp."
    );
    notice(&mut run, "linear not signed in: sign-in was declined").await;
    let other = crate::mcp::Service::new(
        BTreeMap::from([(
            "linear".into(),
            crate::config::McpServer {
                keys: Vec::new(),
                command: server.address(),
                args: Vec::new(),
            },
        )]),
        env.root.clone(),
        Some(env.root.join("mcp")),
    )
    .with_config(
        env.root.join("config.toml"),
        crate::endpoints::PathShown::Given,
    );
    let link = other.sign_in_start("linear").await.unwrap().unwrap();
    other
        .sign_in_finish("linear", &http::redirected(&link, None))
        .await
        .unwrap();
    assert_eq!(service.rows()[0].state, crate::mcp::State::NeedsSignIn);
    run.say("Add linear again.").await;
    run.root_ended().await;
    assert_eq!(
        result_of(run.requests("/").last().unwrap(), "again"),
        (false, "linear added".into())
    );
    run.describe().await;
    tokio::time::pause();
    tokio::time::advance(crate::agent::WAKE_SETTLE + Duration::from_millis(1)).await;
    tokio::time::resume();
    run.describe().await;
    let run = run.quit().await;
    assert_eq!(
        run.records("/")
            .iter()
            .filter(|event| matches!(event, LogEvent::InputAccepted { .. }))
            .count(),
        1
    );
    assert_eq!(
        run.records("/")
            .iter()
            .filter(|event| matches!(event, LogEvent::TurnStarted { .. }))
            .count(),
        3
    );
}
