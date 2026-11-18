use wisp_tool_text::switches::{self, REFUSED, Switched};

use super::limits::{LOW_WISP, SUMMARY, priced};
use super::resume::{Back, child_send, reported, resume};
use super::*;
use crate::stack_alarm::{Timer, on_alarm_stack};

const SHOW_DIFF: &str = "show_diff";

fn with_show_diff(setup: Setup) -> Setup {
    Setup {
        plugins: vec![SHOW_DIFF],
        ..setup
    }
}

fn removals(request: &Sent) -> Vec<String> {
    request
        .messages
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| match b {
            ContentBlock::ToolRemoval { tool } => Some(tool.name().to_owned()),
            _ => None,
        })
        .collect()
}

fn result_text(request: &Sent, id: &str) -> String {
    request
        .messages
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| match b {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } if tool_use_id == id => Some(texts(content).join("\n")),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn all_texts(request: &Sent) -> Vec<String> {
    request
        .messages
        .iter()
        .flat_map(|m| texts(&m.content))
        .collect()
}

fn gone_warning(agent: &str) -> String {
    format!(
        "{agent}'s tool {SHOW_DIFF} isn't provided by any plugin now, so it's turned off from its next message"
    )
}

async fn reported_with_show_diff(name: &str, setup: Setup, child: &str, on: &str) -> Run {
    let mut run = start(
        name,
        with_show_diff(setup),
        vec![
            (on, vec![uses(&[report_call("r1", "done")])]),
            ("/", vec![done("noted")]),
        ],
    )
    .await;
    let endpoint = on.split_once(':').map(|(e, _)| e.to_owned());
    run.spawn(SpawnRequest {
        endpoint,
        ..spawn_request(child.trim_start_matches('/'), "Work.")
    })
    .await;
    reported(&mut run, child).await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    run.quit().await
}

async fn messaged(back: Back, child: &str) -> Run {
    let mut run = back.run;
    run.send(child_send(child, "you-1", "Once more.")).await;
    run.child_ended(child).await;
    run.quit().await
}

fn calling_show_diff() -> Vec<Step> {
    vec![
        uses(&[call("d1", SHOW_DIFF, json!({"files": []}))]),
        done("again"),
    ]
}

#[tokio::test(start_paused = true)]
async fn a_child_resumed_after_its_plugin_was_removed_drops_the_tool_refuses_its_calls_and_rebuilds_nothing()
 {
    let noted = || Setup {
        endpoints: vec![FakeEndpoint {
            table: Table::Only(&[]),
            ..endpoint(ENV)
        }],
        ..Setup::default()
    };
    for (n, setup) in [Setup::default, noted].into_iter().enumerate() {
        let blocks = n == 0;
        let run =
            reported_with_show_diff(&format!("resume-child-gone-{n}"), setup(), "/c", "/c").await;
        let before = run.requests("/c").last().cloned().unwrap();
        assert!(before.tool_names().contains(&SHOW_DIFF.to_owned()));
        let back = resume(
            run,
            Setup {
                removed: vec!["show-diff"],
                ..setup()
            },
            vec![("/c", calling_show_diff()), ("/", vec![done("ok")])],
        )
        .await;
        assert_eq!(back.warnings, [gone_warning("/c")], "case {n}");
        let run = messaged(back, "/c").await;
        let sent = run.requests("/c");
        let (first, second) = (&sent[sent.len() - 2], &sent[sent.len() - 1]);
        assert_eq!(first.prefix(), before.prefix(), "case {n}");
        assert_eq!(first.tools, before.tools, "case {n}");
        assert!(first.messages.starts_with(&before.messages), "case {n}");
        let note = switches::note(&Switched {
            off: vec![SHOW_DIFF.to_owned()],
            on: Vec::new(),
        })
        .unwrap();
        if blocks {
            assert_eq!(removals(first), [SHOW_DIFF], "case {n}");
            assert!(!all_texts(first).contains(&note), "case {n}");
        } else {
            assert!(removals(first).is_empty(), "case {n}");
            assert!(
                all_texts(first).contains(&note),
                "case {n}: {:?}",
                all_texts(first)
            );
        }
        assert!(result_text(second, "d1").contains(REFUSED), "case {n}");
        let changed: Vec<(Vec<String>, Vec<String>)> = run
            .seen
            .iter()
            .filter_map(|e| inner(e, "/c"))
            .filter_map(|e| match e {
                AgentEvent::ToolsChanged { added, removed, .. } => {
                    Some((added.clone(), removed.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            changed,
            [(Vec::new(), vec![SHOW_DIFF.to_owned()])],
            "case {n}"
        );
        let logged = tools_changed(&run).1;
        assert_eq!(logged, 1, "case {n}");
        let back = resume(
            run,
            Setup {
                removed: vec!["show-diff"],
                ..setup()
            },
            vec![("/c", vec![done("third")]), ("/", vec![done("ok")])],
        )
        .await;
        assert!(back.warnings.is_empty(), "case {n}: {:?}", back.warnings);
        let run = messaged(back, "/c").await;
        let last = run.requests("/c").last().cloned().unwrap();
        assert_eq!(last.prefix(), before.prefix(), "case {n}");
        assert!(last.messages.starts_with(&second.messages), "case {n}");
        let fresh = &last.messages[second.messages.len()..];
        assert!(
            fresh.iter().all(|m| m.role != Role::System),
            "case {n}: {fresh:?}"
        );
        assert!(
            !fresh
                .iter()
                .flat_map(|m| texts(&m.content))
                .any(|t| t == note),
            "case {n}"
        );
        assert_eq!(tools_changed(&run), (false, logged), "case {n}");
    }
}

fn tools_changed(run: &Run) -> (bool, usize) {
    let announced = run
        .seen
        .iter()
        .filter_map(|e| inner(e, "/c"))
        .any(|e| matches!(e, AgentEvent::ToolsChanged { .. }));
    let logged = run
        .records("/c")
        .iter()
        .filter(|e| matches!(e, LogEvent::ToolsChanged { .. }))
        .count();
    (announced, logged)
}

#[tokio::test(start_paused = true)]
async fn a_resumed_child_that_compacts_restates_its_removal_at_the_head() {
    let low = || Setup {
        models: Some(LOW_WISP),
        ..Setup::default()
    };
    let run = reported_with_show_diff("resume-child-compact", low(), "/c", "/c").await;
    let before = run.requests("/c").last().cloned().unwrap();
    let back = resume(
        run,
        Setup {
            removed: vec!["show-diff"],
            ..low()
        },
        vec![
            (
                "/c",
                vec![
                    priced(
                        uses(&[call("d1", SHOW_DIFF, json!({"files": []}))]),
                        5_000,
                        40,
                    ),
                    priced(done(SUMMARY), 6_000, 300),
                    done("again"),
                ],
            ),
            ("/", vec![done("ok"); 2]),
        ],
    )
    .await;
    assert_eq!(back.warnings, [gone_warning("/c")]);
    let run = messaged(back, "/c").await;
    let compacted = run
        .seen
        .iter()
        .filter_map(|e| inner(e, "/c"))
        .any(|e| matches!(e, AgentEvent::Compacted { .. }));
    assert!(compacted, "the resumed turn compacts");
    let last = run.requests("/c").last().cloned().unwrap();
    assert_eq!(last.tools, before.tools);
    assert_eq!(last.messages.len(), 2, "{:?}", last.messages);
    assert_eq!(last.messages[1].role, Role::System);
    assert_eq!(removals(&last), [SHOW_DIFF]);
}

fn on_b(setup: Setup) -> Setup {
    Setup {
        endpoints: vec![endpoint(ENV), endpoint("b")],
        ..setup
    }
}

fn fallback_warning() -> String {
    format!(
        "/c ran on account b, which isn't configured now; it continues on the keys-file account with model {MODEL}"
    )
}

#[tokio::test(start_paused = true)]
async fn a_child_on_another_endpoint_adopts_its_copy_there_or_on_a_fallback_with_the_same_server_tools()
 {
    let cases = [
        (on_b(Setup::default()), "b:/c", vec![gone_warning("/c")]),
        (
            Setup::default(),
            "/c",
            vec![fallback_warning(), gone_warning("/c")],
        ),
    ];
    for (n, (later, now_on, warnings)) in cases.into_iter().enumerate() {
        let run = reported_with_show_diff(
            &format!("resume-child-endpoint-{n}"),
            on_b(Setup::default()),
            "/c",
            "b:/c",
        )
        .await;
        let before = run.requests("b:/c").last().cloned().unwrap();
        let back = resume(
            run,
            Setup {
                removed: vec!["show-diff"],
                ..later
            },
            vec![(now_on, vec![done("again")]), ("/", vec![done("ok")])],
        )
        .await;
        assert_eq!(back.warnings, warnings, "case {n}");
        let run = messaged(back, "/c").await;
        let first = run.requests(now_on).last().cloned().unwrap();
        assert_eq!(first.prefix(), before.prefix(), "case {n}");
        assert_eq!(first.tools, before.tools, "case {n}");
        assert_eq!(first.server_names(), before.server_names(), "case {n}");
        assert!(first.messages.starts_with(&before.messages), "case {n}");
        assert_eq!(removals(&first), [SHOW_DIFF], "case {n}");
    }
}

#[tokio::test(start_paused = true)]
async fn a_child_on_a_fallback_whose_server_tools_differ_takes_todays_rebuild() {
    let searching = || {
        on_b(Setup {
            plugins: vec!["web_search"],
            ..Setup::default()
        })
    };
    let mut run = start(
        "resume-child-fallback-search",
        Setup {
            plugins: vec!["web_search", SHOW_DIFF],
            ..searching()
        },
        vec![
            ("b:/c", vec![uses(&[report_call("r1", "done")])]),
            ("/", vec![done("noted")]),
        ],
    )
    .await;
    run.spawn(SpawnRequest {
        endpoint: Some("b".into()),
        ..spawn_request("c", "Work.")
    })
    .await;
    reported(&mut run, "/c").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let run = run.quit().await;
    let before = run.requests("b:/c").last().cloned().unwrap();
    assert!(before.server_names().is_empty());
    assert!(before.tool_names().contains(&"web_search".to_owned()));
    let back = resume(
        run,
        Setup {
            plugins: vec!["web_search"],
            removed: vec!["show-diff"],
            models: Some("[endpoints.env]\nnative_web_search = false\n"),
            ..Setup::default()
        },
        vec![("/c", calling_show_diff()), ("/", vec![done("ok")])],
    )
    .await;
    assert_eq!(back.warnings.len(), 3, "{:?}", back.warnings);
    assert_eq!(back.warnings[0], fallback_warning());
    assert_eq!(
        back.warnings[1],
        format!("/c's tool {SHOW_DIFF} isn't provided by any plugin now, so it resumes without it")
    );
    assert!(
        back.warnings[2].contains("1 agent rebuilds its cache on its next request"),
        "{:?}",
        back.warnings
    );
    let run = messaged(back, "/c").await;
    let sent = run.requests("/c");
    let (first, second) = (&sent[sent.len() - 2], &sent[sent.len() - 1]);
    assert!(first.server_names().is_empty());
    assert!(!first.tool_names().contains(&"web_search".to_owned()));
    assert!(!first.tool_names().contains(&SHOW_DIFF.to_owned()));
    assert!(removals(first).is_empty());
    assert!(first.messages.iter().all(|m| m.role != Role::System));
    assert!(
        !all_texts(first)
            .iter()
            .any(|t| t.starts_with(switches::TURNED_OFF) || t.starts_with(switches::TURNED_ON)),
        "{:?}",
        all_texts(first)
    );
    assert_eq!(tools_changed(&run), (false, 0));
    assert!(
        result_text(second, "d1").contains("there is no tool named"),
        "{}",
        result_text(second, "d1")
    );
}

fn upgraded_child(run: &Run) {
    run.store.edit(&session_name("/c"), |records| {
        let mut digests = None;
        for record in records.iter_mut() {
            let mut value = serde_json::to_value(&*record).unwrap();
            if value["type"] == "session_started" {
                let digest = |value: &Value| {
                    let specs: Vec<crate::tool::ToolSpec> =
                        serde_json::from_value(value["tool_definitions"].clone()).unwrap();
                    crate::session::sha256_hex(&crate::tool::tools_json(&specs, &[]))
                };
                let old = digest(&value);
                assert_eq!(value["tools_sha256"], json!(old));
                for definition in value["tool_definitions"].as_array_mut().unwrap() {
                    if definition["name"] == SHOW_DIFF {
                        let older =
                            format!("{} Older.", definition["description"].as_str().unwrap());
                        definition["description"] = json!(older);
                    }
                }
                let new = digest(&value);
                value["tools_sha256"] = json!(new);
                digests = Some((json!(old), new));
            } else if let Some((old, new)) = &digests
                && value["tools_sha256"] == *old
            {
                value["tools_sha256"] = json!(new);
            }
            *record = serde_json::from_value(value).unwrap();
        }
    });
}

#[tokio::test(start_paused = true)]
async fn a_resumed_child_whose_first_opening_rebuilds_counts_in_the_rebuild_warning_and_defers_nothing()
 {
    let plain = || Setup {
        endpoints: vec![FakeEndpoint {
            table: Table::Only(&[]),
            ..endpoint(ENV)
        }],
        ..Setup::default()
    };
    let run = reported_with_show_diff("resume-child-upgraded", plain(), "/c", "/c").await;
    let before = run.requests("/c").last().cloned().unwrap();
    upgraded_child(&run);
    let back = resume(
        run,
        with_show_diff(plain()),
        vec![("/c", vec![done("again")]), ("/", vec![done("ok")])],
    )
    .await;
    assert_eq!(
        back.warnings,
        [
            "Wisp's prompt or tools changed since this tree ran: 1 agent rebuilds its cache on its next request, dropping earlier reasoning; the conversations are kept"
        ]
    );
    let run = messaged(back, "/c").await;
    let first = run.requests("/c").last().cloned().unwrap();
    assert!(first.messages.starts_with(&before.messages));
    assert_eq!(first.tool_names(), before.tool_names());
    let rebuilt: Vec<wisp_protocol::ToolChangeVia> = run
        .seen
        .iter()
        .filter_map(|e| inner(e, "/c"))
        .filter_map(|e| match e {
            AgentEvent::ToolsChanged { via, .. } => Some(*via),
            _ => None,
        })
        .collect();
    assert_eq!(rebuilt, [wisp_protocol::ToolChangeVia::Rebuild]);
    assert_eq!(tools_changed(&run), (true, 1));
    assert!(run.requests("/c").iter().all(|r| r.deferred.is_empty()));
    let copies: Vec<usize> = run
        .records("/c")
        .iter()
        .filter_map(|e| match e {
            LogEvent::ToolsChanged { copy, .. } if copy.system_prompt.is_some() => {
                Some(copy.whole.deferred_definitions.len())
            }
            _ => None,
        })
        .collect();
    assert_eq!(copies, [0], "a child that rebuilds defers nothing");
}

#[test]
fn a_resumed_child_holding_a_tool_on_a_pair_that_can_defer_defers_nothing() {
    on_alarm_stack(Timer::Paused, || async {
        let mut run = start(
            "resume-child-holding",
            with_show_diff(Setup::default()),
            vec![
                ("/c", vec![uses(&[report_call("r1", "done")])]),
                ("/", vec![done("noted")]),
            ],
        )
        .await;
        run.spawn(SpawnRequest {
            tools_off: vec![SHOW_DIFF.to_owned()],
            ..spawn_request("c", "Work.")
        })
        .await;
        reported(&mut run, "/c").await;
        tokio::time::sleep(Duration::from_secs(30)).await;
        let run = run.quit().await;
        let before = run.requests("/c").last().cloned().unwrap();
        assert!(!before.tool_names().contains(&SHOW_DIFF.to_owned()));
        assert!(before.deferred.is_empty());
        let back = resume(
            run,
            with_show_diff(Setup::default()),
            vec![("/c", vec![done("again")]), ("/", vec![done("ok")])],
        )
        .await;
        assert!(back.warnings.is_empty(), "{:?}", back.warnings);
        let run = messaged(back, "/c").await;
        let first = run.requests("/c").last().cloned().unwrap();
        assert!(first.messages.starts_with(&before.messages));
        assert_eq!(first.tool_names(), before.tool_names());
        assert!(run.requests("/c").iter().all(|r| r.deferred.is_empty()));
        assert_eq!(tools_changed(&run), (false, 0));
    });
}
