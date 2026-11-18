use wisp_protocol::Billing;
use wisp_protocol::CacheEffect;
use wisp_protocol::subscription_auth::{Stayed, Unusable};

use super::resume::{child_send, entry, list, reported, resume};
use super::*;

const OWNERS: [&str; 3] = ["/codebase-recap", "/merge-agent5", "/helper5"];

pub(super) fn subscription(name: &'static str) -> FakeEndpoint {
    FakeEndpoint {
        billing: Some(Billing::AnthropicPlan),
        ..endpoint(name)
    }
}

pub(super) fn api(name: &'static str) -> FakeEndpoint {
    FakeEndpoint {
        billing: Some(Billing::Metered),
        ..endpoint(name)
    }
}

async fn ran(name: &str, endpoints: Vec<FakeEndpoint>, children: &[(&str, &str)]) -> Run {
    let root = on(endpoints[0].name, "/");
    let openai_names: Vec<&str> = endpoints
        .iter()
        .filter(|e| e.table == Table::OpenAi)
        .map(|e| e.name)
        .collect();
    let mut scripts: Vec<(String, Vec<Step>)> = children
        .iter()
        .map(|(on_endpoint, child)| {
            (
                on(on_endpoint, child),
                vec![uses(&[report_call("r1", "done")])],
            )
        })
        .collect();
    scripts.push((root, vec![done("noted"); children.len()]));
    let scripts: Vec<(&str, Vec<Step>)> = scripts
        .iter()
        .map(|(path, steps)| (path.as_str(), steps.clone()))
        .collect();
    let mut run = start(
        name,
        Setup {
            endpoints,
            ..Setup::default()
        },
        scripts,
    )
    .await;
    for (on_endpoint, child) in children {
        let name = child.trim_start_matches('/');
        run.spawn(SpawnRequest {
            endpoint: Some((*on_endpoint).into()),
            model: openai_names.contains(on_endpoint).then(|| GPT.into()),
            ..spawn_request(name, "Work.")
        })
        .await;
        reported(&mut run, child).await;
    }
    tokio::time::sleep(Duration::from_secs(30)).await;
    run.quit().await
}

fn after_resume(run: &Run, agent: &str) -> Vec<LogEvent> {
    let records = run.records(agent);
    let from = records
        .iter()
        .rposition(|e| matches!(e, LogEvent::SessionResumed { .. }))
        .unwrap_or_else(|| panic!("{agent} logged no resume: {records:#?}"));
    records.into_iter().skip(from).collect()
}

fn switch_marks(events: &[LogEvent], to: &str) -> (usize, usize) {
    let switches = events
        .iter()
        .filter(|e| {
            matches!(e, LogEvent::SettingsChanged {
                endpoint_name: Some(name),
                cache: CacheEffect::Rebuilt { reason: wisp_protocol::RebuildReason::EndpointSwitch },
                ..
            } if name == to)
        })
        .count();
    let marks = events
        .iter()
        .filter(
            |e| matches!(e, LogEvent::RebuildMarked { reason, .. } if reason == "endpoint_switch"),
        )
        .count();
    (switches, marks)
}

#[tokio::test(start_paused = true)]
async fn the_owners_idle_children_on_the_keys_file_key_follow_the_chat_onto_the_subscription() {
    let children: Vec<(&str, &str)> = OWNERS.iter().map(|c| (ENV, *c)).collect();
    let run = ran("follow-owner", vec![endpoint(ENV)], &children).await;
    let scripts: Vec<(String, Vec<Step>)> = OWNERS
        .iter()
        .map(|c| (on("claude-subscription", c), vec![done("again")]))
        .chain([(on("claude-subscription", "/"), vec![done("ok"); 3])])
        .collect();
    let back = resume(
        run,
        Setup {
            endpoints: vec![subscription("claude-subscription")],
            ..Setup::default()
        },
        scripts
            .iter()
            .map(|(path, steps)| (path.as_str(), steps.clone()))
            .collect(),
    )
    .await;
    let mut sorted = OWNERS.to_vec();
    sorted.sort_unstable();
    assert_eq!(
        back.warnings,
        sorted
            .iter()
            .map(|c| format!(
                "{c} ran on the keys-file account, which isn't configured now; it continues on claude-subscription with model {MODEL}"
            ))
            .collect::<Vec<_>>()
    );
    for warning in &back.warnings {
        assert!(!warning.contains(" env"), "{warning}");
    }
    assert_eq!(
        back.moved
            .iter()
            .map(|f| format!("/{}", f.agent))
            .collect::<Vec<_>>(),
        sorted
    );
    assert!(back.stay.is_empty());
    assert_eq!(back.live, sorted);
    let mut run = back.run;
    let agents = list(&mut run).await;
    for child in OWNERS {
        assert_eq!(
            entry(&agents, child).endpoint.as_deref(),
            Some("claude-subscription"),
            "{child}"
        );
        let history = &entry(&agents, child).accounts;
        assert_eq!(history.keys().collect::<Vec<_>>(), [ENV], "{child}");
        run.send(child_send(child, &format!("{child}-1"), "Once more."))
            .await;
        run.child_ended(child).await;
        let now = list(&mut run).await;
        let accounts = &entry(&now, child).accounts;
        assert_eq!(accounts[ENV], history[ENV], "{child}");
        assert!(accounts["claude-subscription"].usd > 0.0, "{child}");
    }
    let run = run.quit().await;
    for child in OWNERS {
        let before = run.requests(child);
        let after = run.requests(&on("claude-subscription", child));
        assert_eq!(after.len(), 1, "{child}");
        let last = before.last().unwrap();
        assert!(
            after[0].messages.starts_with(&last.messages),
            "{child}'s first request on the subscription doesn't extend its history"
        );
        assert!(after[0].messages.len() > last.messages.len(), "{child}");
        assert_eq!(after[0].prefix(), last.prefix(), "{child}");
        let events = after_resume(&run, child);
        assert_eq!(
            switch_marks(&events, "claude-subscription"),
            (1, 1),
            "{child}: {events:#?}"
        );
        let marked = events
            .iter()
            .position(|e| matches!(e, LogEvent::RebuildMarked { .. }))
            .unwrap();
        let turn = events
            .iter()
            .position(|e| matches!(e, LogEvent::TurnStarted { .. }))
            .unwrap();
        assert!(marked < turn, "{child}: the mark comes before its turn");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, LogEvent::CacheMiss { .. })),
            "{child}: {events:#?}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn an_agent_on_a_gone_api_account_follows_a_chat_that_didnt_move_and_a_working_one_stays() {
    let both = || vec![subscription("claude-subscription"), api("claude-api")];
    let run = ran("follow-scout-stays", both(), &[("claude-api", "/scout")]).await;
    let back = resume(
        run,
        Setup {
            endpoints: both(),
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    let mut run = back.run;
    let agents = list(&mut run).await;
    assert_eq!(
        entry(&agents, "/scout").endpoint.as_deref(),
        Some("claude-api")
    );
    run.quit().await;

    let run = ran("follow-scout", both(), &[("claude-api", "/scout")]).await;
    let back = resume(
        run,
        Setup {
            endpoints: vec![subscription("claude-subscription")],
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    assert_eq!(
        back.warnings,
        [format!(
            "/scout ran on account claude-api, which isn't configured now; it continues on claude-subscription with model {MODEL}"
        )]
    );
    assert_eq!(back.live, ["/scout"]);
    assert_eq!(
        back.moved
            .iter()
            .map(|f| (f.agent.as_str(), f.model.as_str()))
            .collect::<Vec<_>>(),
        [("scout", MODEL)]
    );
    back.run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn an_agent_of_another_provider_stays() {
    let run = ran(
        "follow-other-provider",
        vec![
            subscription("claude-subscription"),
            openai("chatgpt", Billing::ChatGptPlan),
        ],
        &[("chatgpt", "/gpt")],
    )
    .await;
    let back = resume(
        run,
        Setup {
            endpoints: vec![subscription("claude-subscription")],
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    assert_eq!(
        back.warnings,
        [
            "/gpt wasn't resumed: it ran on account chatgpt, which isn't configured now, and claude-subscription is another provider; it stays ended for this run"
        ]
    );
    assert_eq!(back.ended, ["/gpt"]);
    assert!(back.moved.is_empty());
    assert_eq!(
        back.stay,
        [Stayed {
            agent: "gpt".to_owned(),
            charges: false,
            account: "chatgpt".to_owned(),
            ..Default::default()
        }]
    );
    back.run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn an_agent_follows_from_a_subscription_to_the_api_only_with_the_chats_consent() {
    let plan = || {
        vec![
            subscription("claude-subscription"),
            subscription("claude-work"),
        ]
    };
    let run = ran("follow-consented", plan(), &[("claude-work", "/coder")]).await;
    let back = resume(
        run,
        Setup {
            endpoints: vec![api("claude-api")],
            follow: vec!["coder".to_owned()],
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    assert_eq!(
        back.warnings,
        [format!(
            "/coder ran on account claude-work, which isn't configured now; it continues on claude-api with model {MODEL}"
        )]
    );
    assert_eq!(back.live, ["/coder"]);
    assert_eq!(
        back.moved
            .iter()
            .map(|f| f.agent.as_str())
            .collect::<Vec<_>>(),
        ["coder"]
    );
    back.run.quit().await;

    let coder_stays = || {
        [Stayed {
            agent: "coder".to_owned(),
            charges: true,
            account: "claude-work".to_owned(),
            ..Default::default()
        }]
    };
    let run = ran("follow-unnamed", plan(), &[("claude-work", "/coder")]).await;
    let back = resume(
        run,
        Setup {
            endpoints: vec![api("claude-api")],
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    assert_eq!(
        back.warnings,
        [
            "/coder wasn't resumed: it ran on account claude-work, which isn't configured now, and claude-api bills differently; it stays ended for this run"
        ]
    );
    assert_eq!(back.ended, ["/coder"]);
    assert_eq!(back.stay, coder_stays());
    back.run.quit().await;

    let run = ran(
        "follow-not-consented",
        vec![api("claude-api"), subscription("claude-work")],
        &[("claude-work", "/coder")],
    )
    .await;
    let back = resume(
        run,
        Setup {
            endpoints: vec![api("claude-api")],
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    assert_eq!(
        back.warnings,
        [
            "/coder wasn't resumed: it ran on account claude-work, which isn't configured now, and claude-api bills differently; it stays ended for this run"
        ]
    );
    assert_eq!(back.ended, ["/coder"]);
    assert_eq!(back.stay, coder_stays());
    back.run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn an_agent_whose_account_cant_be_read_goes_to_the_start_account_only() {
    let team = |state: &str, sign_in: bool| Unusable {
        account: "claude-team".to_owned(),
        state: state.to_owned(),
        sign_in,
    };
    let three = || {
        vec![
            subscription("claude-work"),
            subscription("claude-subscription"),
            subscription("claude-team"),
        ]
    };
    let run = ran("unreadable-moves", three(), &[("claude-team", "/coder")]).await;
    let back = resume(
        run,
        Setup {
            endpoints: vec![
                subscription("claude-work"),
                subscription("claude-subscription"),
            ],
            unavailable: vec![("claude-team", "its keychain can't be read")],
            start: Some("claude-subscription"),
            ..Setup::default()
        },
        vec![("/", vec![done("ok")])],
    )
    .await;
    assert_eq!(back.live, ["/coder"]);
    let moved: Vec<_> = back
        .moved
        .iter()
        .map(|f| (f.agent.as_str(), f.from.clone(), f.to.as_deref()))
        .collect();
    assert_eq!(
        moved,
        [(
            "coder",
            Some(team("can't be read now", false)),
            Some("claude-subscription")
        )]
    );
    assert!(back.stay.is_empty());
    let mut run = back.run;
    let agents = list(&mut run).await;
    assert_eq!(
        entry(&agents, "/coder").endpoint.as_deref(),
        Some("claude-subscription")
    );
    run.quit().await;

    let stays = [Stayed {
        agent: "coder".to_owned(),
        account: "claude-team".to_owned(),
        from: Some(team("sign-in expired", true)),
        ..Stayed::default()
    }];
    let cases = [
        (
            vec![api("claude-api"), api("work-api")],
            Vec::new(),
            "work-api",
        ),
        (
            vec![
                subscription("claude-work"),
                openai("chatgpt", Billing::ChatGptPlan),
            ],
            Vec::new(),
            "chatgpt",
        ),
        (
            vec![subscription("claude-work")],
            vec![("claude-subscription", "its keychain can't be read")],
            "claude-subscription",
        ),
    ];
    for (n, (endpoints, more, start)) in cases.into_iter().enumerate() {
        let run = ran(
            &format!("unreadable-stays-{n}"),
            three(),
            &[("claude-team", "/coder")],
        )
        .await;
        let mut unavailable = vec![("claude-team", "its sign-in expired")];
        unavailable.extend(more);
        let back = resume(
            run,
            Setup {
                endpoints,
                unavailable,
                follow: vec!["coder".to_owned()],
                start: Some(start),
                unusable: vec![team("sign-in expired", true)],
                ..Setup::default()
            },
            Vec::new(),
        )
        .await;
        assert!(back.warnings.is_empty(), "{:?}", back.warnings);
        assert_eq!(back.ended, ["/coder"], "{start}");
        assert!(back.moved.is_empty(), "{start}");
        assert_eq!(back.stay, stays, "{start}");
        let run = back.run.quit().await;
        assert!(
            notices(&run, "/")
                .iter()
                .any(|(_, text)| text.contains("its account claude-team is unavailable")),
            "{start}"
        );
    }
}
