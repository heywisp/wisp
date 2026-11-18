use wisp_protocol::MissCause;
use wisp_protocol::settings::CACHE_KEPT;

use super::*;
use crate::clock::Clock;
use crate::pricing;

const TOP_LEVEL: &str = "claude-opus-4-8";
const KEY_MODEL: &str = "sk-ant-api03-AbCdEfGh123CANARY0XyZ";
const PROJECT_RULES: &str = "Answer briefly.\n";
const DEFAULT_RULES: &str = "Prefer short replies.\n";

fn causes(bench: &Bench) -> Vec<MissCause> {
    log_events(&bench.store)
        .into_iter()
        .filter_map(|e| match e {
            LogEvent::CacheMiss { cause, .. } => cause,
            _ => None,
        })
        .collect()
}

fn repeated(bench: &Bench) -> usize {
    records(&bench.store)
        .iter()
        .filter(|r| matches!(r.event, LogEvent::CacheMissRepeated { .. }))
        .count()
}

fn expected() -> MissCause {
    MissCause::Expected {
        reason: "resume".into(),
    }
}

fn refused(bench: &Bench) -> usize {
    bench.served().iter().filter(|s| s.status != 200).count()
}

async fn long_run(bench: &Bench, first: Build) {
    let long = "Read this carefully. ".repeat(5_000);
    let mut live = bench.start(first);
    live.completed("u1", &long).await;
    live.completed("u2", &long).await;
    finish(bench, live).await;
}

fn three_hours_on() -> ShiftedClock {
    let clock = fixed_clock();
    clock.advance(jiff::SignedDuration::from_hours(3));
    clock
}

fn resume_at(bench: &Bench, later: Build, clock: Arc<dyn Clock>) -> Agent {
    let records = records(&bench.store);
    let seq = u64::try_from(records.len()).unwrap();
    let log = SessionLog::continuing(Box::new(bench.store.clone()), "s1", seq);
    bench
        .agent(log, later, bench.connections(later))
        .with_clock(clock)
        .resume(&records, 0)
        .unwrap()
}

async fn run_from(bench: &Bench, first: Build) {
    let mut live = bench.start(first);
    two_turns(&mut live, 1).await;
    finish(bench, live).await;
}

async fn resumed(bench: &Bench, later: Build, from: usize) -> Agent {
    let mut live = bench.resume(later);
    two_turns(&mut live, from).await;
    live.completed(&format!("u{}", from + 2), "Once more.")
        .await;
    finish(bench, live).await
}

async fn flagged(case: &str, stage: bool, first: Build, later: Build) -> (Bench, Agent, usize) {
    let bench = Bench::new(case, stage).await;
    run_from(&bench, first).await;
    let change = bench.settled();
    let agent = resumed(&bench, later, 3).await;
    (bench, agent, change)
}

fn once(bench: &Bench, change: usize, cause: Option<MissCause>) {
    assert_settles(bench, &[change]);
    assert_eq!(causes(bench), Vec::from_iter(cause), "{}", bench.case);
    assert_eq!(repeated(bench), 0, "{}", bench.case);
    assert_eq!(refused(bench), 0, "{}", bench.case);
    assert!(
        bench.cuts().is_empty(),
        "{}: {:?}",
        bench.case,
        bench.cuts()
    );
}

fn never(bench: &Bench) {
    assert_settles(bench, &[]);
    assert_eq!(causes(bench), Vec::new(), "{}", bench.case);
    assert_eq!(repeated(bench), 0, "{}", bench.case);
    assert_eq!(refused(bench), 0, "{}", bench.case);
}

#[tokio::test]
async fn an_effort_flag_on_a_per_message_pair_and_a_fallback_flag_keep_the_cache() {
    let cases = [
        (&[("--effort", "effort", "low")], "effort low"),
        (
            &[("--no-fallback", "refusal_fallback", "false")],
            "refusal fallback off",
        ),
    ];
    for ((flags, words), stage) in cases.into_iter().flat_map(|c| [(c, false), (c, true)]) {
        let later = Build {
            flags,
            ..Build::default()
        };
        let (bench, agent, _) =
            flagged("flags-effort-marker", stage, Build::default(), later).await;
        never(&bench);
        assert!(
            notice(&agent).ends_with(&format!("From your next message: {words} ({CACHE_KEPT}).")),
            "{}",
            notice(&agent)
        );
    }
}

#[tokio::test]
async fn an_effort_flag_without_per_message_markers_misses_once() {
    for stage in [false, true] {
        let first = Build {
            model: TOP_LEVEL,
            ..Build::default()
        };
        let later = Build {
            flags: &[("--effort", "effort", "low")],
            ..first
        };
        let bench = Bench::new("flags-effort-top", stage).await;
        long_run(&bench, first).await;
        let change = bench.settled();
        let agent = resumed(&bench, later, 3).await;
        once(&bench, change, Some(expected()));
        let ok = settled(&bench.served());
        assert!(ok[change].read < ok[change - 1].total, "{}", bench.case);
        assert!(
            notice(&agent).contains(" Your flags change effort to low: the cache is rebuilt"),
            "{}",
            notice(&agent)
        );
        assert!(!notice(&agent).contains("reasoning"), "{}", notice(&agent));
    }
}

#[tokio::test]
async fn a_model_flag_misses_once_there_and_once_back() {
    for stage in [false, true] {
        let bench = Bench::new("flags-model", stage).await;
        run_from(&bench, Build::default()).await;
        let there = bench.settled();
        let agent = resumed(
            &bench,
            Build {
                flags: &[("--model", "model", SONNET)],
                ..Build::default()
            },
            3,
        )
        .await;
        assert!(
            notice(&agent).contains(&format!(
                " Your flags change the model to {SONNET}: the cache is rebuilt (~$"
            )),
            "{}",
            notice(&agent)
        );
        assert!(
            notice(&agent).ends_with(" and the model's earlier reasoning is dropped."),
            "{}",
            notice(&agent)
        );
        let plain = bench.settled();
        let agent = resumed(&bench, Build::default(), 6).await;
        assert!(!notice(&agent).contains("flags"), "{}", notice(&agent));
        let back = bench.settled();
        resumed(
            &bench,
            Build {
                flags: &[("--model", "model", OPUS)],
                ..Build::default()
            },
            9,
        )
        .await;
        assert_settles(&bench, &[there, back]);
        assert!(plain < back);
        let resumed = MissCause::Expected {
            reason: "resume".into(),
        };
        assert_eq!(
            causes(&bench),
            vec![resumed.clone(), resumed],
            "{}",
            bench.case
        );
        assert_eq!(repeated(&bench), 0);
        assert!(bench.cuts().is_empty(), "{:?}", bench.cuts());
    }
}

#[tokio::test]
async fn an_endpoint_flag_starts_on_its_endpoint_and_misses_once() {
    for stage in [false, true] {
        let bench = Bench::new("flags-endpoint", stage).await;
        let mut live = bench.start_with(Build::default(), bench.both());
        two_turns(&mut live, 1).await;
        finish(&bench, live).await;
        let change = bench.settled();
        let later = Build {
            flags: &[("--endpoint", "endpoint", "gw")],
            ..Build::default()
        };
        let flagged_first = vec![bench.connection("gw", true), bench.connection("env", false)];
        let mut live = bench.resume_with(later, flagged_first);
        two_turns(&mut live, 3).await;
        live.completed("u5", "Once more.").await;
        let agent = finish(&bench, live).await;
        assert_eq!(agent.endpoint_name(), "gw");
        assert!(
            notice(&agent).contains(" Your flags change the account to gw: the cache is rebuilt"),
            "{}",
            notice(&agent)
        );
        once(&bench, change, None);
    }
}

#[tokio::test]
async fn a_cache_ttl_flag_and_back_sends_no_misordered_markers() {
    for stage in [false, true] {
        let bench = Bench::new("flags-ttl", stage).await;
        long_run(&bench, Build::default()).await;
        let agent = resumed(
            &bench,
            Build {
                flags: &[("--cache-ttl", "cache_ttl", "5m")],
                ..Build::default()
            },
            3,
        )
        .await;
        assert!(
            notice(&agent)
                .ends_with("From your next message: cache TTL 5m (may rebuild the cache)."),
            "{}",
            notice(&agent)
        );
        assert_eq!(agent.session().cache_ttl, CacheTtl::FiveMinutes);
        let change = bench.settled();
        let agent = resumed(
            &bench,
            Build {
                flags: &[("--set cache_ttl", "cache_ttl", "1h")],
                ..Build::default()
            },
            6,
        )
        .await;
        assert_eq!(agent.session().cache_ttl, CacheTtl::OneHour);
        once(&bench, change, Some(expected()));
        let five = settled(&bench.served())
            .iter()
            .filter(|s| s.body.contains("{\"type\":\"ephemeral\"}"))
            .count();
        assert!(five > 0, "{}: no request used the 5m tail", bench.case);
    }
}

#[tokio::test]
async fn a_debug_cache_flag_misses_at_most_once() {
    for stage in [false, true] {
        let later = Build {
            flags: &[("--debug-cache", "debug_cache", "true")],
            ..Build::default()
        };
        let (bench, agent, change) = flagged("flags-debug", stage, Build::default(), later).await;
        assert!(
            notice(&agent)
                .ends_with("From your next message: cache debugging on (may rebuild the cache)."),
            "{}",
            notice(&agent)
        );
        once(&bench, change, Some(expected()));
    }
}

fn instruction_files(bench: &Bench) {
    fs::write(bench.env.work().join("AGENTS.md"), PROJECT_RULES).unwrap();
    fs::create_dir_all(bench.env.root.join("config")).unwrap();
    fs::write(bench.env.root.join("config/AGENTS.md"), DEFAULT_RULES).unwrap();
}

fn last_system(bench: &Bench) -> Value {
    let served = settled(&bench.served());
    let body: Value = serde_json::from_str(&served.last().unwrap().body).unwrap();
    body["system"].clone()
}

#[tokio::test]
async fn an_instructions_flag_rewrites_once_and_a_plain_resume_sends_the_same_bytes() {
    for (source, rules) in [("none", None), ("default", Some(DEFAULT_RULES))] {
        for stage in [false, true] {
            let bench = Bench::new(&format!("flags-instructions-{source}"), stage).await;
            instruction_files(&bench);
            let first = Build {
                instructions: Some("auto"),
                ..Build::default()
            };
            run_from(&bench, first).await;
            let change = bench.settled();
            let flags: &'static [(&str, &str, &str)] = match source {
                "none" => &[("--instructions", "instructions.source", "none")],
                _ => &[(
                    "--set instructions.source",
                    "instructions.source",
                    "default",
                )],
            };
            let agent = resumed(
                &bench,
                Build {
                    instructions: Some(source),
                    flags,
                    ..Build::default()
                },
                3,
            )
            .await;
            assert!(
                notice(&agent)
                    .contains(" Your flags change the project instructions: the cache is rebuilt"),
                "{}",
                notice(&agent)
            );
            let system = last_system(&bench).to_string();
            assert!(!system.contains(PROJECT_RULES.trim()), "{system}");
            assert_eq!(
                rules.is_some_and(|r| system.contains(r.trim())),
                rules.is_some(),
                "{system}"
            );
            once(&bench, change, Some(expected()));
            let ok = settled(&bench.served());
            assert!(!ok[change].drops.is_empty(), "{}", bench.case);
            let resumed_records: Vec<LogEvent> = log_events(&bench.store)
                .into_iter()
                .filter(|e| matches!(e, LogEvent::SessionResumed { .. }))
                .collect();
            let LogEvent::SessionResumed {
                instructions_source,
                instructions,
                ..
            } = &resumed_records[0]
            else {
                unreachable!()
            };
            assert_eq!(
                instructions_source.as_ref().map(|s| s.as_str()),
                Some(source)
            );
            assert_eq!(instructions.is_some(), rules.is_some());
            let before = settled(&bench.served()).last().unwrap().body.clone();
            let agent = resumed(
                &bench,
                Build {
                    instructions: Some(source),
                    ..Build::default()
                },
                6,
            )
            .await;
            assert!(!notice(&agent).contains("flags"), "{}", notice(&agent));
            assert!(!notice(&agent).contains("changed"), "{}", notice(&agent));
            assert_eq!(causes(&bench), vec![expected()], "{}", bench.case);
            let ok = settled(&bench.served());
            let after = ok.iter().rposition(|s| s.body == before).unwrap();
            assert!(extends(&before, &ok[after + 1].body), "{}", bench.case);
            assert!(ok[after + 1].read >= ok[after].total, "{}", bench.case);
            assert_eq!(ok[after + 1].drops, ok[change].drops, "{}", bench.case);
        }
    }
}

struct ExtraCase {
    case: &'static str,
    first: Option<&'static str>,
    flags: &'static [(&'static str, &'static str, &'static str)],
    later: Build,
    sentence: &'static str,
    logged: &'static str,
}

fn autonomous_line() -> String {
    let line = crate::instructions::Extra::Autonomous
        .text()
        .lines()
        .next()
        .unwrap();
    serde_json::to_string(line)
        .unwrap()
        .trim_matches('"')
        .to_owned()
}

#[tokio::test]
async fn an_extra_instructions_flag_rewrites_once_and_a_plain_resume_sends_the_same_bytes() {
    let cases = [
        ExtraCase {
            case: "flags-extra-on",
            first: None,
            flags: &[("--instructions-extra", "instructions.extra", "autonomous")],
            later: Build {
                instructions: Some("auto"),
                extra: Some("autonomous"),
                ..Build::default()
            },
            sentence: " Your flags change the extra instructions (autonomous): the cache is rebuilt",
            logged: "autonomous",
        },
        ExtraCase {
            case: "flags-extra-off",
            first: Some("autonomous"),
            flags: &[("--set instructions.extra", "instructions.extra", "none")],
            later: Build {
                instructions: Some("auto"),
                ..Build::default()
            },
            sentence: " Your flags change the extra instructions (none): the cache is rebuilt",
            logged: "none",
        },
        ExtraCase {
            case: "flags-extra-and-source",
            first: None,
            flags: &[
                ("--instructions", "instructions.source", "none"),
                ("--instructions-extra", "instructions.extra", "autonomous"),
            ],
            later: Build {
                instructions: Some("none"),
                extra: Some("autonomous"),
                ..Build::default()
            },
            sentence: " Your flags change the project instructions and the extra instructions (autonomous): the cache is rebuilt",
            logged: "autonomous",
        },
    ];
    for c in cases {
        for stage in [false, true] {
            let bench = Bench::new(c.case, stage).await;
            instruction_files(&bench);
            let first = Build {
                instructions: Some("auto"),
                extra: c.first,
                ..Build::default()
            };
            run_from(&bench, first).await;
            let change = bench.settled();
            let flagged = Build {
                flags: c.flags,
                ..c.later
            };
            let agent = resumed(&bench, flagged, 3).await;
            assert_eq!(
                notice(&agent).matches("the cache is rebuilt").count(),
                1,
                "{}",
                notice(&agent)
            );
            assert!(notice(&agent).contains(c.sentence), "{}", notice(&agent));
            let system = last_system(&bench).to_string();
            assert_eq!(
                system.contains(&autonomous_line()),
                c.later.extra.is_some(),
                "{system}"
            );
            once(&bench, change, Some(expected()));
            let logged: Vec<Option<String>> = pick!(
                log_events(&bench.store),
                LogEvent::SessionResumed {
                    instructions_extra, ..
                } => instructions_extra
            );
            assert_eq!(logged, [Some(c.logged.to_owned())], "{}", bench.case);
            let before = settled(&bench.served()).last().unwrap().body.clone();
            let agent = resumed(&bench, c.later, 6).await;
            assert!(!notice(&agent).contains("flags"), "{}", notice(&agent));
            assert!(!notice(&agent).contains("changed"), "{}", notice(&agent));
            assert_eq!(causes(&bench), vec![expected()], "{}", bench.case);
            let ok = settled(&bench.served());
            let after = ok.iter().rposition(|s| s.body == before).unwrap();
            assert!(extends(&before, &ok[after + 1].body), "{}", bench.case);
            assert!(ok[after + 1].read >= ok[after].total, "{}", bench.case);
        }
    }
}

#[tokio::test]
async fn without_a_flag_the_logged_extra_instructions_are_kept() {
    for stage in [false, true] {
        let bench = Bench::new("flags-extra-kept", stage).await;
        instruction_files(&bench);
        let first = Build {
            instructions: Some("auto"),
            extra: Some("autonomous"),
            ..Build::default()
        };
        run_from(&bench, first).await;
        let configured = Build {
            instructions: Some("auto"),
            ..Build::default()
        };
        let agent = resumed(&bench, configured, 3).await;
        never(&bench);
        assert!(!notice(&agent).contains("flags"), "{}", notice(&agent));
        assert!(last_system(&bench).to_string().contains(&autonomous_line()));
    }
}

#[tokio::test]
async fn a_model_and_an_effort_flag_together_miss_once() {
    for stage in [false, true] {
        let later = Build {
            flags: &[("--model", "model", SONNET), ("--effort", "effort", "low")],
            ..Build::default()
        };
        let (bench, agent, change) =
            flagged("flags-model-effort", stage, Build::default(), later).await;
        assert!(
            notice(&agent).contains(&format!(
                " Your flags change the model to {SONNET} and effort to low: the cache is rebuilt"
            )),
            "{}",
            notice(&agent)
        );
        let resumed = MissCause::Expected {
            reason: "resume".into(),
        };
        once(&bench, change, Some(resumed));
    }
}

async fn plain_and_flagged(
    case: &str,
    first: Build,
    flags: &'static [(&'static str, &'static str, &'static str)],
) {
    for stage in [false, true] {
        plain_and_flagged_on(case, stage, first, flags).await;
    }
}

async fn plain_and_flagged_on(
    case: &str,
    stage: bool,
    first: Build,
    flags: &'static [(&'static str, &'static str, &'static str)],
) {
    let first = Build {
        instructions: Some("auto"),
        ..first
    };
    let mut sent = Vec::new();
    let mut notices = Vec::new();
    for later in [first, Build { flags, ..first }] {
        let bench = Bench::new(case, stage).await;
        instruction_files(&bench);
        run_from(&bench, first).await;
        let from = bench.served().len();
        let agent = resumed(&bench, later, 3).await;
        notices.push(notice(&agent).to_owned());
        sent.push(
            bench.served()[from..]
                .iter()
                .map(|s| s.body.clone())
                .collect::<Vec<_>>(),
        );
        never(&bench);
    }
    assert_eq!(notices[0], notices[1], "{case}");
    assert!(!notices[1].contains("next message:"), "{}", notices[1]);
    assert_eq!(sent[0], sent[1], "{case}: a no-op flag changed the bytes");
}

fn options_on() -> Build {
    Build {
        options: RequestOptions {
            refusal_fallback: false,
            debug_cache: true,
            message_threads: true,
        },
        ..Build::default()
    }
}

#[tokio::test]
async fn flags_equal_to_the_next_turns_values_change_nothing() {
    plain_and_flagged(
        "flags-noop",
        options_on(),
        &[
            ("--model", "model", OPUS),
            ("--effort", "effort", "medium"),
            ("--endpoint", "endpoint", "env"),
            ("--cache-ttl", "cache_ttl", "1h"),
            ("--debug-cache", "debug_cache", "true"),
            ("--no-fallback", "refusal_fallback", "false"),
            ("--time", "time", "local"),
            ("--context-note", "context.context_note", "true"),
            ("--instructions", "instructions.source", "auto"),
            ("--instructions-extra", "instructions.extra", "none"),
        ],
    )
    .await;
}

#[tokio::test]
async fn set_forms_equal_to_the_next_turns_values_change_nothing() {
    plain_and_flagged(
        "flags-noop-set",
        options_on(),
        &[
            ("--set model", "model", OPUS),
            ("--set effort", "effort", "medium"),
            ("--set endpoint", "endpoint", "env"),
            ("--set cache_ttl", "cache_ttl", "1h"),
            ("--set debug_cache", "debug_cache", "true"),
            ("--set refusal_fallback", "refusal_fallback", "false"),
            ("--set time", "time", "local"),
            ("--set context.context_note", "context.context_note", "true"),
            ("--set instructions.source", "instructions.source", "auto"),
            ("--set instructions.extra", "instructions.extra", "none"),
        ],
    )
    .await;
}

#[tokio::test]
async fn a_flag_equal_to_the_session_replaces_a_logged_pending_change_and_names_it() {
    let bench = Bench::new("flags-pending", false).await;
    let mut live = bench.start(Build::default());
    two_turns(&mut live, 1).await;
    switch_model(&mut live, 1, SONNET).await;
    finish(&bench, live).await;
    let plain = bench.resume(Build::default());
    let (agent, _) = plain.close().await;
    assert!(
        agent
            .resume_notice_text()
            .is_some_and(|n| !n.contains("next message")),
        "{:?}",
        agent.resume_notice_text()
    );
    let agent = resumed(
        &bench,
        Build {
            flags: &[("--model", "model", OPUS)],
            ..Build::default()
        },
        3,
    )
    .await;
    assert!(
        notice(&agent).ends_with(&format!(
            "From your next message: model {OPUS} ({CACHE_KEPT})."
        )),
        "{}",
        notice(&agent)
    );
    assert_eq!(agent.session().model, OPUS);
    never(&bench);
    assert!(
        settled(&bench.served())
            .iter()
            .all(|s| s.body.contains(&format!("\"model\":\"{OPUS}\"")))
    );
}

#[tokio::test]
async fn a_key_shaped_model_flag_never_shows_in_the_notice() {
    let bench = Bench::new("flags-key-model", false).await;
    run_from(&bench, Build::default()).await;
    let live = bench.resume(Build {
        flags: &[("--model", "model", KEY_MODEL)],
        ..Build::default()
    });
    let (agent, _) = live.close().await;
    let text = notice(&agent);
    assert!(!text.contains(KEY_MODEL), "{text}");
    assert!(
        text.contains(wisp_protocol::shown_model(KEY_MODEL)),
        "{text}"
    );
}

#[test]
fn the_fake_refuses_a_1h_marker_after_a_5m_one() {
    let body = |system: Value, message: Value| {
        json!({
            "model": OPUS,
            "thinking": {"type": "adaptive"},
            "tools": [{"name": "bash", "cache_control": {"type": "ephemeral", "ttl": "1h"}}],
            "system": [{"type": "text", "text": "a", "cache_control": system}],
            "messages": [{"role": "user", "content": [{"type": "text", "text": "b", "cache_control": message}]}]
        })
    };
    let one_hour = json!({"type": "ephemeral", "ttl": "1h"});
    let five = json!({"type": "ephemeral"});
    assert!(!longer_after_shorter(&body(one_hour.clone(), five.clone())));
    assert!(!longer_after_shorter(&body(
        one_hour.clone(),
        one_hour.clone()
    )));
    let wrong = body(five, one_hour);
    assert!(longer_after_shorter(&wrong));
    let answer = Api::default().answer("", &wrong.to_string());
    assert!(answer.starts_with("HTTP/1.1 400"), "{answer}");
    assert!(answer.contains(TTL_ORDER), "{answer}");
}

#[tokio::test]
async fn a_flag_rebuild_is_priced_on_the_next_model() {
    let bench = Bench::new("flags-priced", false).await;
    long_run(&bench, Build::default()).await;
    let later = Build {
        flags: &[("--model", "model", SONNET)],
        ..Build::default()
    };
    let agent = resume_at(&bench, later, Arc::new(fixed_clock()));
    let tokens = agent.fill_estimate();
    let price = |model: &str| {
        let usage = Usage {
            cache_creation_input_tokens: tokens,
            cache_creation_1h_input_tokens: Some(tokens),
            ..Usage::default()
        };
        let info = pricing::model_info(model).unwrap();
        format!(
            "(~{})",
            wisp_protocol::format_usd(pricing::estimate_usd(info, &usage, CacheTtl::OneHour))
        )
    };
    assert_ne!(price(SONNET), price(OPUS));
    assert!(
        notice(&agent).contains(&format!("the cache is rebuilt {}", price(SONNET))),
        "{}",
        notice(&agent)
    );
}

#[tokio::test]
async fn a_flag_rebuild_is_priced_at_the_next_cache_ttl() {
    let bench = Bench::new("flags-priced-ttl", false).await;
    long_run(&bench, Build::default()).await;
    let later = Build {
        flags: &[
            ("--model", "model", SONNET),
            ("--cache-ttl", "cache_ttl", "5m"),
        ],
        ..Build::default()
    };
    let agent = resume_at(&bench, later, Arc::new(fixed_clock()));
    let tokens = agent.fill_estimate();
    let price = |ttl: CacheTtl| {
        let (five, hour) = match ttl {
            CacheTtl::FiveMinutes => (Some(tokens), Some(0)),
            CacheTtl::OneHour => (Some(0), Some(tokens)),
        };
        let usage = Usage {
            cache_creation_input_tokens: tokens,
            cache_creation_5m_input_tokens: five,
            cache_creation_1h_input_tokens: hour,
            ..Usage::default()
        };
        let info = pricing::model_info(SONNET).unwrap();
        format!(
            "(~{})",
            wisp_protocol::format_usd(pricing::estimate_usd(info, &usage, ttl))
        )
    };
    assert_ne!(price(CacheTtl::FiveMinutes), price(CacheTtl::OneHour));
    assert!(
        notice(&agent).contains(&format!(
            "the cache is rebuilt {}",
            price(CacheTtl::FiveMinutes)
        )),
        "{}",
        notice(&agent)
    );
}

#[tokio::test]
async fn after_the_cache_expired_the_flags_are_listed_without_a_class() {
    let bench = Bench::new("flags-expired", false).await;
    run_from(&bench, Build::default()).await;
    let later = Build {
        flags: &[("--effort", "effort", "low")],
        ..Build::default()
    };
    let agent = resume_at(&bench, later, Arc::new(three_hours_on()));
    let text = notice(&agent);
    assert!(text.contains("The cache has likely expired"), "{text}");
    assert!(
        text.ends_with("From your next message: effort low."),
        "{text}"
    );
    let warm = resume_at(&bench, later, Arc::new(fixed_clock()));
    assert!(
        notice(&warm)
            .ends_with(format!("From your next message: effort low ({CACHE_KEPT}).").as_str()),
        "{}",
        notice(&warm)
    );
}

#[tokio::test]
async fn an_effort_flag_beside_a_pending_model_switch_says_the_reasoning_is_dropped() {
    let bench = Bench::new("flags-pending-switch", false).await;
    let mut live = bench.start(Build::default());
    two_turns(&mut live, 1).await;
    switch_model(&mut live, 1, SONNET).await;
    finish(&bench, live).await;
    let later = Build {
        flags: &[("--effort", "effort", "low")],
        ..Build::default()
    };
    let agent = resume_at(&bench, later, Arc::new(fixed_clock()));
    let text = notice(&agent);
    assert!(
        text.contains(" Your flags change effort to low: the cache is rebuilt"),
        "{text}"
    );
    assert!(
        text.ends_with(" and the model's earlier reasoning is dropped."),
        "{text}"
    );
}

fn after_resume(bench: &Bench) -> Vec<LogEvent> {
    let events = log_events(&bench.store);
    let from = events
        .iter()
        .rposition(|e| matches!(*e, LogEvent::SessionResumed { .. }))
        .unwrap();
    events.into_iter().skip(from).collect()
}

fn switched_to(events: &[LogEvent], to: &str) -> Vec<Vec<String>> {
    events
        .iter()
        .filter_map(|e| match e {
            LogEvent::SettingsChanged {
                endpoint_name: Some(name),
                cache:
                    wisp_protocol::CacheEffect::Rebuilt {
                        reason: wisp_protocol::RebuildReason::EndpointSwitch,
                    },
                account_bound,
                ..
            } if name == to => Some(account_bound.clone()),
            _ => None,
        })
        .collect()
}

fn switch_marks(events: &[LogEvent]) -> usize {
    events
        .iter()
        .filter(
            |e| matches!(e, LogEvent::RebuildMarked { reason, .. } if reason == "endpoint_switch"),
        )
        .count()
}

async fn moved_off(case: &str, stage: bool, model: &'static str) -> (Bench, usize, Agent) {
    let bench = Bench::new(case, stage).await;
    let build = Build {
        model,
        ..Build::default()
    };
    let mut live = bench.start(build);
    two_turns(&mut live, 1).await;
    finish(&bench, live).await;
    let change = bench.settled();
    let mut live = bench.resume_with(build, vec![bench.connection("claude-subscription", false)]);
    two_turns(&mut live, 3).await;
    live.completed("u5", "Once more.").await;
    let agent = finish(&bench, live).await;
    (bench, change, agent)
}

#[tokio::test]
async fn a_resume_whose_account_is_gone_rebuilds_once_with_a_mark_and_no_miss() {
    for stage in [false, true] {
        let (bench, change, agent) = moved_off("moved-account", stage, OPUS).await;
        assert_eq!(agent.endpoint_name(), "claude-subscription");
        once(&bench, change, None);
        let events = after_resume(&bench);
        assert_eq!(
            switched_to(&events, "claude-subscription"),
            [Vec::<String>::new()],
            "{}",
            bench.case
        );
        assert_eq!(switch_marks(&events), 1, "{}", bench.case);
        let marked = events
            .iter()
            .position(|e| matches!(e, LogEvent::RebuildMarked { .. }))
            .unwrap();
        let turn = events
            .iter()
            .position(|e| matches!(e, LogEvent::TurnStarted { .. }))
            .unwrap();
        assert!(marked < turn, "{}", bench.case);
        let ok = settled(&bench.served());
        let (_, before) = prefix_parts(&ok[change - 1].body);
        let (_, after) = prefix_parts(&ok[change].body);
        assert!(
            after.len() > before.len() && after.starts_with(&before),
            "{}: the first request on the new account doesn't extend the history",
            bench.case
        );
    }
}

#[tokio::test]
async fn a_resumed_sonnet_log_with_thinking_gets_the_switchs_handling() {
    for stage in [false, true] {
        let (bench, change, agent) = moved_off("moved-sonnet", stage, SONNET).await;
        assert_eq!(agent.endpoint_name(), "claude-subscription");
        let events = after_resume(&bench);
        assert_eq!(
            switched_to(&events, "claude-subscription"),
            [vec![SONNET.to_owned()]],
            "{}",
            bench.case
        );
        assert_eq!(switch_marks(&events), 1, "{}", bench.case);
        let ok = settled(&bench.served());
        assert!(binds(&ok[change].body), "{}", bench.case);
        assert_eq!(refused(&bench), 0, "{}", bench.case);
    }
}

#[tokio::test]
async fn a_sign_out_of_the_chats_account_rebuilds_once_with_a_mark_and_no_miss() {
    use wisp_protocol::subscription_auth::{AuthEvent, AuthOutcome};
    for (stage, elsewhere) in [(false, false), (true, false), (false, true), (true, true)] {
        let case = if elsewhere {
            "signed-out-elsewhere"
        } else {
            "signed-out"
        };
        let bench = Bench::new(case, stage).await;
        let host = crate::endpoints::testing::AuthHost::new();
        *host.next_provider.lock().unwrap() = Some(bench.server.provider());
        host.load("env");
        host.load("claude-subscription");
        let log = SessionLog::new(Box::new(bench.store.clone()), "s1");
        let build = Build {
            model: OPUS,
            ..Build::default()
        };
        let mut ready = host.catalog.view().ready;
        ready.sort_by_key(|c| c.name != "env");
        let agent = bench
            .agent(log, build, ready)
            .with_endpoint_host(host.clone());
        let mut live = Live::on(agent, &bench.server, bench.steps.clone());
        two_turns(&mut live, 1).await;
        let change = bench.settled();
        if elsewhere {
            host.elsewhere(&["env"], &[]);
        } else {
            live.send(ClientCommand::SubscriptionAuth {
                command: crate::endpoints::testing::logout(90, "env"),
            })
            .await;
            let answer = live
                .until(|e| matches!(e, AgentEvent::SubscriptionAuth { .. }))
                .await;
            assert!(
                matches!(
                    answer,
                    AgentEvent::SubscriptionAuth {
                        event: AuthEvent {
                            outcome: AuthOutcome::LoggedOut { .. },
                            ..
                        }
                    }
                ),
                "{answer:?}"
            );
        }
        two_turns(&mut live, 3).await;
        live.completed("u5", "Once more.").await;
        let (agent, seen) = finish_seen(&bench, live).await;
        assert_eq!(agent.endpoint_name(), "claude-subscription");
        assert!(
            !seen
                .iter()
                .any(|e| matches!(e, AgentEvent::Rejected { .. })),
            "{}",
            bench.case
        );
        assert_eq!(
            seen.iter()
                .filter(|e| matches!(e, AgentEvent::AccountsGone { .. }))
                .count(),
            usize::from(elsewhere),
            "{}",
            bench.case
        );
        once(&bench, change, None);
        let events: Vec<LogEvent> = log_events(&bench.store);
        assert_eq!(
            switched_to(&events, "claude-subscription"),
            [Vec::<String>::new()],
            "{}",
            bench.case
        );
        assert_eq!(switch_marks(&events), 1, "{}", bench.case);
        let marked = events
            .iter()
            .position(|e| matches!(e, LogEvent::RebuildMarked { .. }))
            .unwrap();
        let turn = events
            .iter()
            .enumerate()
            .filter(|(_, e)| matches!(e, LogEvent::TurnStarted { .. }))
            .nth(2)
            .unwrap()
            .0;
        assert!(marked < turn, "{}", bench.case);
        let ok = settled(&bench.served());
        let (_, before) = prefix_parts(&ok[change - 1].body);
        let (_, after) = prefix_parts(&ok[change].body);
        assert!(
            after.len() > before.len() && after.starts_with(&before),
            "{}: the first request on the new account doesn't extend the history",
            bench.case
        );
    }
}
