use std::time::Duration;

use wisp_plugin_api::Plugin;
use wisp_plugin_api::config::{AccountRow, Limits, SignIn};
use wisp_plugin_api::slots::{Block, Content, SlotView, Span, Tone, UsdSpan};
use wisp_plugin_testkit::host::FakeCall;
use wisp_protocol::settings::SettingValue;

use super::*;
use crate::plugin::caller::{Caller, Render};

fn plugin_host(owned: &[&str], kernel: &Arc<KernelHost>) -> PluginHost {
    let owned: Vec<String> = owned.iter().map(|s| (*s).to_owned()).collect();
    PluginHost::new("probe", "0.2.0", &owned, kernel.clone())
}

fn spawn_spec() -> SpawnSpec {
    SpawnSpec {
        program: "true".to_owned(),
        args: Vec::new(),
        cwd: std::env::temp_dir().display().to_string(),
        env: Vec::new(),
        mode: wisp_plugin_api::host::Mode::Pipe,
        size: None,
        stdin: false,
        background: false,
        limit_ms: None,
        label: "true".to_owned(),
        keep: false,
    }
}

#[tokio::test]
async fn a_jobs_call_whose_context_names_no_running_call_is_refused() {
    let kernel = Arc::new(KernelHost::new());
    let root = std::env::temp_dir();
    kernel.bind(
        &Processes::default(),
        &KernelWorkspace {
            root: root.clone(),
            home: None,
            temp: root,
        },
    );
    let host = plugin_host(&[], &kernel);
    let stranger = FakeCall {
        id: "nobody".to_owned(),
        ..FakeCall::default()
    };
    let refused = host.spawn(spawn_spec(), &stranger).await.err();
    let expected = if cfg!(unix) { NO_RUNNING_CALL } else { NO_JOBS };
    assert_eq!(refused, Some(JobError::Invalid(expected.to_owned())));
    assert!(host.adopt(1, &stranger).is_none());
}

#[test]
fn settings_answer_only_the_keys_the_plugin_owns() {
    let kernel = Arc::new(KernelHost::new());
    let host = plugin_host(&["time"], &kernel);
    assert_eq!(kernel.owned_settings(), ["time"]);
    kernel.publish(Snapshot {
        settings: vec![
            ("time".to_owned(), "utc".to_owned()),
            ("effort".to_owned(), "high".to_owned()),
        ],
        ..Snapshot::default()
    });
    assert_eq!(host.get("time").as_deref(), Some("utc"));
    assert_eq!(host.get("effort"), None);
    kernel.publish(Snapshot {
        settings: vec![("time".to_owned(), "off".to_owned())],
        ..Snapshot::default()
    });
    assert_eq!(host.get("time").as_deref(), Some("off"));
}

#[tokio::test(start_paused = true)]
async fn sleep_and_elapsed_follow_the_runtime_clock() {
    let kernel = Arc::new(KernelHost::new());
    let host = plugin_host(&[], &kernel);
    let before = host.elapsed_ms();
    host.sleep(3_600_000).await;
    assert_eq!(host.elapsed_ms() - before, 3_600_000);
}

#[tokio::test(start_paused = true)]
async fn a_deadline_is_milliseconds_on_the_hosts_elapsed_timeline() {
    let kernel = Arc::new(KernelHost::new());
    tokio::time::advance(Duration::from_millis(500)).await;
    let at = tokio::time::Instant::now() + Duration::from_millis(1_500);
    assert_eq!(kernel.deadline_ms(Some(at)), Some(2_000));
    assert_eq!(kernel.deadline_ms(None), None);
}

#[cfg(unix)]
#[test]
fn kind_of_tells_a_fifo_from_a_file_without_opening_it() {
    let dir = tempfile::tempdir().unwrap();
    let fifo = dir.path().join("pipe");
    rustix::fs::mkfifoat(
        rustix::fs::CWD,
        &fifo,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
    )
    .unwrap();
    let file = dir.path().join("file");
    std::fs::write(&file, "x").unwrap();
    assert_eq!(kind_of(&fifo), Some(FileKind::Fifo));
    assert_eq!(kind_of(&file), Some(FileKind::Regular));
    assert_eq!(kind_of(dir.path()), Some(FileKind::Directory));
    assert_eq!(kind_of(&dir.path().join("missing")), None);
}

const SPENT: SpendTotals = SpendTotals {
    turn: Some(7),
    turn_at_least: false,
    session: Some(11),
    session_at_least: true,
};

fn priced_at(kernel: &KernelHost, model: &str) {
    kernel.publish(Snapshot {
        prices: Prices {
            model: pricing::model_info(model).copied(),
            spend: SPENT,
        },
        ..Snapshot::default()
    });
}

fn usages() -> [ApiUsage; 4] {
    [
        ApiUsage {
            input: 1_234,
            output: 567,
            cache_read: 8_901,
            cache_write_5m: 2_345,
            cache_write_1h: 678,
            web_searches: 0,
        },
        ApiUsage {
            input: 90_000,
            output: 4_321,
            cache_read: 20_000,
            cache_write_5m: 0,
            cache_write_1h: 3_000,
            web_searches: 2,
        },
        ApiUsage {
            input: 99_000,
            output: 0,
            cache_read: 0,
            cache_write_5m: 0,
            cache_write_1h: 2_000,
            web_searches: 0,
        },
        ApiUsage {
            input: 3,
            output: 1,
            cache_read: 0,
            cache_write_5m: 7,
            cache_write_1h: 0,
            web_searches: 0,
        },
    ]
}

#[test]
fn an_estimate_is_estimate_usd_in_micro_dollars_rounded_half_up_for_every_model_and_ttl() {
    let kernel = Arc::new(KernelHost::new());
    let host = plugin_host(&[], &kernel);
    let mut models = 0;
    for model in pricing::model_ids() {
        let info = pricing::model_info(model).unwrap();
        priced_at(&kernel, model);
        for ttl in [CacheTtl::FiveMinutes, CacheTtl::OneHour] {
            for usage in usages() {
                let protocol = wisp_protocol::Usage {
                    input_tokens: usage.input,
                    output_tokens: usage.output,
                    cache_read_input_tokens: usage.cache_read,
                    cache_creation_input_tokens: usage.cache_write_5m + usage.cache_write_1h,
                    cache_creation_5m_input_tokens: Some(usage.cache_write_5m),
                    cache_creation_1h_input_tokens: Some(usage.cache_write_1h),
                    web_search_requests: usage.web_searches,
                    ..wisp_protocol::Usage::default()
                };
                let usd = pricing::estimate_usd(info, &protocol, ttl);
                let half_up = (usd * 1_000_000.0 + 0.5).floor() as u64;
                assert_eq!(
                    host.estimate(usage),
                    Some(half_up),
                    "{model} {ttl:?} {usage:?}"
                );
            }
        }
        models += 1;
    }
    assert_eq!(models, pricing::model_ids().count());
    assert!(models > 10);
}

#[test]
fn both_write_ttls_are_priced_whatever_the_session_ttl() {
    let kernel = Arc::new(KernelHost::new());
    let host = plugin_host(&[], &kernel);
    let write = |five: u64, hour: u64| ApiUsage {
        cache_write_5m: five,
        cache_write_1h: hour,
        ..ApiUsage::default()
    };
    priced_at(&kernel, "claude-opus-5-5");
    assert_eq!(host.estimate(write(1_000_000, 0)), Some(5_000_000));
    assert_eq!(host.estimate(write(0, 1_000_000)), Some(8_000_000));
    assert_eq!(host.estimate(write(1_000_000, 1_000_000)), Some(13_000_000));
}

#[test]
fn half_a_micro_dollar_rounds_up_and_the_long_prompt_tier_applies() {
    let kernel = Arc::new(KernelHost::new());
    let host = plugin_host(&[], &kernel);
    priced_at(&kernel, "claude-haiku-5-5");
    let read = |tokens: u64| ApiUsage {
        cache_read: tokens,
        ..ApiUsage::default()
    };
    assert_eq!(host.estimate(read(49)), Some(0));
    assert_eq!(host.estimate(read(50)), Some(1));
    assert_eq!(host.estimate(read(100_000)), Some(1_000));
    assert_eq!(host.estimate(read(100_001)), Some(5_000));
    let crossing = |write_1h: u64| ApiUsage {
        input: 99_000,
        cache_write_1h: write_1h,
        ..ApiUsage::default()
    };
    assert_eq!(host.estimate(crossing(1_000)), Some(10_100));
    assert_eq!(host.estimate(crossing(2_000)), Some(51_500));
    assert_eq!(pricing::micros(2.5e-6), 3);
    assert_eq!(pricing::whole_micros(2.5), 3);
}

#[test]
fn an_unknown_model_or_an_unpriced_connection_estimates_nothing() {
    let kernel = Arc::new(KernelHost::new());
    let host = plugin_host(&[], &kernel);
    assert_eq!(host.estimate(usages()[0]), None);
    assert_eq!(host.spend(), SpendTotals::default());
    priced_at(&kernel, "some-gateway-model");
    assert_eq!(host.estimate(usages()[0]), None);
    assert_eq!(host.spend(), SPENT);
}

struct Meter(Arc<dyn Host>);

impl Plugin for Meter {
    fn render_slot<'a>(
        &'a self,
        _name: &'a str,
        _view: SlotView,
    ) -> BoxFuture<'a, Result<Content, String>> {
        let usd = |micros: Option<u64>, at_least: bool| {
            Span::Usd(UsdSpan {
                micros: micros.unwrap_or_default(),
                at_least,
                tone: Tone::Text,
            })
        };
        let spend = self.0.spend();
        let quote = self.0.estimate(ApiUsage {
            cache_read: 1_000_000,
            ..ApiUsage::default()
        });
        let content = Content {
            note: Vec::new(),
            blocks: vec![Block::Line(vec![
                usd(spend.turn, spend.turn_at_least),
                usd(spend.session, spend.session_at_least),
                usd(quote, false),
            ])],
        };
        futures::future::ready(Ok(content)).boxed()
    }
}

fn usd_micros(content: &wisp_protocol::slot::SlotContent) -> Vec<(u64, bool)> {
    use wisp_protocol::slot::{SlotBlock, SlotSpan};
    content
        .blocks
        .iter()
        .flat_map(|block| match block {
            SlotBlock::Line { spans } => spans.clone(),
            other => panic!("{other:?}"),
        })
        .map(|span| match span {
            SlotSpan::Usd {
                micros, at_least, ..
            } => (micros, at_least),
            other => panic!("{other:?}"),
        })
        .collect()
}

#[tokio::test]
async fn a_slot_render_may_price_a_usage_and_read_the_spend() {
    let kernel = Arc::new(KernelHost::new());
    priced_at(&kernel, "claude-opus-5-5");
    let host: Arc<dyn Host> = Arc::new(plugin_host(&[], &kernel));
    let meter = Meter(host);
    let content = crate::plugin::slots::lane::call(
        &meter,
        "cost",
        wisp_protocol::slot::SlotView::Panel,
        "probe",
    )
    .await
    .unwrap();
    assert_eq!(
        usd_micros(&content),
        [(7, false), (11, true), (200_000, false)]
    );
}

fn open_call(kernel: &Arc<KernelHost>, id: &str, plugin: &str) -> Arc<KernelCall> {
    open_viewed(kernel, id, plugin, None)
}

fn open_viewed(
    kernel: &Arc<KernelHost>,
    id: &str,
    plugin: &str,
    view: Option<Arc<CallView>>,
) -> Arc<KernelCall> {
    open_with(kernel, id, plugin, view, UserChannel::none())
}

fn open_with(
    kernel: &Arc<KernelHost>,
    id: &str,
    plugin: &str,
    view: Option<Arc<CallView>>,
    user: UserChannel,
) -> Arc<KernelCall> {
    let call = Arc::new(KernelCall {
        asker: None,
        model_asks: Mutex::new(Vec::new()),
        call_id: id.to_owned(),
        cancel: CancellationToken::new(),
        steered: CancellationToken::new(),
        moved: CancellationToken::new(),
        deadline: None,
        user,
        cwd: std::env::temp_dir(),
        live_tail: false,
        sink: Mutex::new(OutputSink::detached(id)),
        host: kernel.clone(),
        todos: Mutex::new(None),
        plugin: plugin.to_owned(),
        agents: AgentsHandle::none(),
        view,
    });
    kernel.open_call(call.clone());
    call
}

#[tokio::test]
async fn a_screen_report_reaches_only_the_plugin_that_owns_the_call() {
    let kernel = Arc::new(KernelHost::new());
    let (asks, mut requests) = tokio::sync::mpsc::channel(1);
    open_with(&kernel, "c", "page", None, UserChannel::new(asks));
    tokio::spawn(async move {
        while let Some(crate::call::user::AskRequest::Report { reply }) = requests.recv().await {
            let report = serde_json::json!({"width": 380});
            let images = Vec::new();
            let _ = reply.send(crate::call::user::ScreenReport { report, images });
        }
    });
    for (plugin, world) in [("other", "0.3.0"), ("page", "0.2.0")] {
        let host = PluginHost::new(plugin, world, &[], kernel.clone());
        assert_eq!(host.wait("c".into(), 1_000).await, None, "{plugin} {world}");
    }
    let page = PluginHost::new("page", "0.3.0", &[], kernel.clone());
    assert_eq!(page.wait("x".into(), 1_000).await, None);
    let report = page.wait("c".into(), 1_000).await.map(|r| r.report);
    assert_eq!(report.as_deref(), Some(r#"{"width":380}"#));
}

fn spawn_input() -> SpawnInput {
    SpawnInput {
        name: "a".to_owned(),
        task: "Work.".to_owned(),
        ..SpawnInput::default()
    }
}

fn every_agents_call(host: &PluginHost, ctx: &dyn CallContext) -> [Result<(), String>; 5] {
    let spawned = host
        .spawn_agent(spawn_input(), ctx)
        .now_or_never()
        .unwrap()
        .map(|_| ());
    let listed = host.list_agents(ctx).map(|_| ());
    let reported = host.report("{}".to_owned(), ctx);
    let sent = host
        .send_message("..".to_owned(), "hi".to_owned(), true, ctx)
        .map(|_| ());
    let stopped = host.stop_agent("a".to_owned(), false, ctx).map(|_| ());
    [spawned, listed, reported, sent, stopped]
}

#[test]
fn only_the_agents_plugins_own_running_tool_calls_reach_the_agents_interface() {
    let kernel = Arc::new(KernelHost::new());
    let probe = plugin_host(&[], &kernel);
    let agents = PluginHost::new(AGENTS_PLUGIN, "0.2.0", &[], kernel.clone());
    open_call(&kernel, "toolu_probe", "probe");
    open_call(&kernel, "toolu_agents", AGENTS_PLUGIN);
    let ctx = |id: &str| FakeCall {
        id: id.to_owned(),
        ..FakeCall::default()
    };
    for id in ["toolu_probe", "toolu_agents", "nobody"] {
        for result in every_agents_call(&probe, &ctx(id)) {
            assert_eq!(
                result,
                Err(
                    "the agents interface belongs to the bundled agents plugin, not probe"
                        .to_owned()
                ),
                "{id}"
            );
        }
    }
    for (id, why) in [
        ("nobody", "a hook's context names no running call"),
        ("toolu_probe", "another plugin's call"),
    ] {
        for result in every_agents_call(&agents, &ctx(id)) {
            assert_eq!(
                result,
                Err(format!(
                    "no agents tool call {id} is running, so nothing was done"
                )),
                "{why}"
            );
        }
    }
    let own = ctx("toolu_agents");
    assert_eq!(agents.list_agents(&own), Ok(Vec::new()));
    assert_eq!(
        agents.report("{}".to_owned(), &own),
        Err("only an agent with a parent reports".to_owned())
    );
    assert_eq!(
        agents
            .spawn_agent(spawn_input(), &own)
            .now_or_never()
            .unwrap()
            .err(),
        Some("this session has no agents, so it can't spawn one".to_owned())
    );
    assert_eq!(
        agents.send_message("..".to_owned(), "hi".to_owned(), true, &own),
        Err("this session has no other agents".to_owned())
    );
    assert_eq!(
        agents.stop_agent("a".to_owned(), true, &own),
        Err("this session has no agents to stop".to_owned())
    );
    kernel.close_call("toolu_agents");
    assert!(agents.list_agents(&own).is_err());
}

#[test]
fn a_spawn_input_becomes_a_request_and_a_bad_effort_names_the_rule() {
    let request = spawn_request(SpawnInput {
        effort: Some("low".to_owned()),
        todos: vec!["Read".to_owned()],
        spawn: Some(false),
        endpoint: Some("b".to_owned()),
        ..spawn_input()
    })
    .unwrap();
    assert_eq!(request.endpoint.as_deref(), Some("b"));
    assert_eq!(request.effort, Some(wisp_protocol::Effort::Low));
    assert_eq!(request.todos, ["Read"]);
    assert_eq!(request.spawn, Some(false));
    assert!(!request.keep_warm);
    for (asked, marked) in [(Some(true), true), (Some(false), false), (None, false)] {
        let request = spawn_request(SpawnInput {
            keep_warm: asked,
            ..spawn_input()
        })
        .unwrap();
        assert_eq!(request.keep_warm, marked, "{asked:?}");
    }
    assert_eq!(
        spawn_request(SpawnInput {
            effort: Some("turbo".to_owned()),
            ..spawn_input()
        })
        .err()
        .as_deref(),
        Some("effort must be one of low, medium, high, xhigh, max")
    );
}

#[test]
fn the_tree_imports_answer_nothing_without_a_tree_even_inside_a_call() {
    use wisp_plugin_api::tree::{Agent as _, AgentTree as _};
    let kernel = Arc::new(KernelHost::new());
    let host = plugin_host(&[], &kernel);
    let call = super::super::caller::Caller::call(CancellationToken::new(), None);
    let (current, nodes) = call.sync_scope(|| (host.current_agent(), host.snapshot()));
    assert_eq!(current, None);
    assert!(nodes.is_empty());
}

fn rendering<F: std::future::Future>(future: F) -> impl std::future::Future<Output = F::Output> {
    super::super::caller::Caller::rendering(super::super::caller::Render::Slot).scope(future)
}

fn counting_snapshot() -> Snapshot {
    Snapshot {
        media_types: vec!["image/png".into()],
        count: MediaCount::new(|_| {
            futures::future::ready(crate::provider::TokenCount::Counted { tokens: 9 }).boxed()
        }),
        ..Snapshot::default()
    }
}

#[tokio::test]
async fn count_tokens_gates_media_and_slices_locally() {
    use wisp_plugin_api::types::{PageSlice, SecondSlice};

    let kernel = Arc::new(KernelHost::new());
    let blocks = Arc::new(std::sync::Mutex::new(Vec::new()));
    let forwarded = blocks.clone();
    kernel.publish(Snapshot {
        media_types: vec!["application/pdf".into(), "audio/*".into()],
        media_slices: vec![SliceKind::Pages],
        count: MediaCount::new(move |block| {
            forwarded.lock().unwrap().push(block);
            ready(crate::provider::TokenCount::Counted { tokens: 9 })
        }),
        ..Snapshot::default()
    });
    let pdf = "application/pdf";
    let pages = Some(FileSlice::Pages(PageSlice { start: 1, end: 2 }));
    for world in ["0.1.0", "0.3.0"] {
        let host = PluginHost::new("probe", world, &[], kernel.clone());
        assert!(host.media_slices().is_empty(), "world {world}");
        assert_eq!(
            host.count_tokens(pdf.into(), vec![1], pages).await,
            TokenCount::Unsupported,
            "world {world}"
        );
        assert_eq!(
            host.count_tokens(pdf.into(), vec![1], None).await,
            TokenCount::Counted(9),
            "world {world}"
        );
    }
    let host = PluginHost::new("probe", "0.4.0", &[], kernel);
    assert_eq!(host.media_slices(), vec![SliceKind::Pages]);
    assert_eq!(
        host.count_tokens("audio/wav".into(), vec![1], None).await,
        TokenCount::Counted(9)
    );
    for (media, slice) in [
        ("image/png", None),
        ("audio/wav", pages),
        (
            "audio/wav",
            Some(FileSlice::Seconds(SecondSlice { start: 30, end: 90 })),
        ),
        (pdf, Some(FileSlice::Pages(PageSlice { start: 0, end: 1 }))),
    ] {
        assert_eq!(
            host.count_tokens(media.into(), vec![1], slice).await,
            TokenCount::Unsupported,
            "{media} {slice:?}"
        );
    }
    assert_eq!(
        host.count_tokens(pdf.into(), vec![1], pages).await,
        TokenCount::Counted(9)
    );
    let whole = ContentBlock::media(pdf.parse().unwrap(), "AQ==");
    let sliced = ContentBlock::media_sliced(
        pdf.parse().unwrap(),
        "AQ==",
        Some(wisp_protocol::FileSlice::pages(1, 2).unwrap()),
    );
    let audio = ContentBlock::media("audio/wav".parse().unwrap(), "AQ==");
    assert_eq!(
        *blocks.lock().unwrap(),
        vec![whole.clone(), whole, audio, sliced]
    );
}

#[tokio::test(start_paused = true)]
async fn inside_a_render_waiting_and_provider_imports_answer_at_once_and_outside_they_work() {
    let kernel = Arc::new(KernelHost::new());
    kernel.publish(counting_snapshot());
    let host = plugin_host(&[], &kernel);
    let (counted, slept, captured) = rendering(async {
        let counted = host
            .count_tokens("image/png".to_owned(), vec![1], None)
            .await;
        let before = host.elapsed_ms();
        host.sleep(10_000).await;
        let captured = host.capture("true".to_owned(), Vec::new(), 10_000).await;
        (counted, host.elapsed_ms() - before, captured)
    })
    .await;
    assert_eq!(counted, TokenCount::Unsupported);
    assert_eq!(slept, 0);
    assert_eq!(captured, Captured::Failed(IN_RENDER.to_owned()));
    assert_eq!(
        host.count_tokens("image/png".to_owned(), vec![1], None)
            .await,
        TokenCount::Counted(9)
    );
    let before = host.elapsed_ms();
    host.sleep(10_000).await;
    assert_eq!(host.elapsed_ms() - before, 10_000);
}

#[tokio::test]
async fn a_render_that_kept_a_running_calls_id_gets_nothing_that_needs_a_call() {
    let kernel = Arc::new(KernelHost::new());
    let root = std::env::temp_dir();
    kernel.bind(
        &Processes::default(),
        &KernelWorkspace {
            root: root.clone(),
            home: None,
            temp: root,
        },
    );
    let host = plugin_host(&[], &kernel);
    let agents = PluginHost::new(AGENTS_PLUGIN, "0.2.0", &[], kernel.clone());
    let call = open_call(&kernel, "toolu_probe", "probe");
    open_call(&kernel, "toolu_agents", AGENTS_PLUGIN);
    let ctx = |id: &str| FakeCall {
        id: id.to_owned(),
        ..FakeCall::default()
    };
    let todo = || {
        vec![Todo {
            text: "a".to_owned(),
            status: wisp_plugin_api::host::TodoStatus::Pending,
            section: None,
        }]
    };
    let probe = ctx("toolu_probe");
    let (spawned, adopted, replaced) = rendering(async {
        let spawned = host.spawn(spawn_spec(), &probe).await.err();
        let adopted = host.adopt(1, &probe).is_none();
        let replaced = host.replace(todo(), &probe);
        (spawned, adopted, replaced)
    })
    .await;
    assert_eq!(spawned, Some(JobError::Invalid(NO_RUNNING_CALL.to_owned())));
    assert!(adopted);
    assert_eq!(replaced, Err(IN_RENDER.to_owned()));
    assert!(call.take_todos().is_none());
    let refused = rendering(async { every_agents_call(&agents, &ctx("toolu_agents")) }).await;
    for result in refused {
        assert_eq!(result, Err(IN_RENDER.to_owned()));
    }
    assert_eq!(host.replace(todo(), &probe), Ok(()));
    assert_eq!(call.take_todos(), Some(todo()));
    let under = |section: &str, text: &str| Todo {
        section: Some(section.to_owned()),
        text: text.to_owned(),
        ..todo()[0].clone()
    };
    let items = vec![
        under(" Next ", " first "),
        under("Done", "second"),
        todo()[0].clone(),
        under("Next", "third"),
    ];
    assert_eq!(host.replace(items, &probe), Ok(()));
    assert_eq!(
        call.take_todos(),
        Some(vec![
            todo()[0].clone(),
            under("Next", "first"),
            under("Next", "third"),
            under("Done", "second")
        ])
    );
    if cfg!(unix) {
        let job = host.spawn(spawn_spec(), &probe).await;
        assert!(job.is_ok(), "{:?}", job.err());
    }
}

#[tokio::test]
async fn inside_a_render_points_fire_reaches_no_hook_and_outside_it_does() {
    use crate::plugin::{Make, PluginConfig, Registry};
    use wisp_plugin_testkit::hooks as kit;
    let calls = kit::Calls::default();
    let body = "\n[[points]]\nname = \"x\"\nkind = \"observe\"\nversion = \"1\"\n\n[[hooks]]\nname = \"seen\"\npoint = \"aa/x\"\n";
    let make: Make = kit::instantiate(kit::none(), calls.clone());
    let (registry, problems) = Registry::from_sources(
        vec![(kit::manifest("aa", body), make)],
        &PluginConfig::default(),
    );
    assert!(problems.is_empty(), "{problems:?}");
    let kernel = Arc::new(KernelHost::new());
    let started = registry.start(&BTreeSet::new(), kernel.clone()).await;
    let (_engine, _finished) = crate::plugin::hooks::Engine::new(
        started.hooks.table,
        started.hooks.plugins,
        Some(kernel.clone()),
    );
    let host = PluginHost::new("aa", "0.2.0", &[], kernel);
    let answers = rendering(async { host.fire("x".to_owned(), String::new()).await }).await;
    assert!(answers.is_empty());
    assert_eq!(calls.count("seen"), 0);
    host.fire("x".to_owned(), String::new()).await;
    assert_eq!(calls.count("seen"), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn inside_a_render_a_jobs_signal_does_nothing_and_outside_it_stops_the_job() {
    use wisp_plugin_api::types::SignalKind;
    let kernel = Arc::new(KernelHost::new());
    let root = std::env::temp_dir();
    kernel.bind(
        &Processes::default(),
        &KernelWorkspace {
            root: root.clone(),
            home: None,
            temp: root,
        },
    );
    let host = plugin_host(&[], &kernel);
    let _call = open_call(&kernel, "toolu_signal", "probe");
    let ctx = FakeCall {
        id: "toolu_signal".to_owned(),
        ..FakeCall::default()
    };
    let spec = SpawnSpec {
        program: "sleep".to_owned(),
        args: vec!["30".to_owned()],
        label: "sleep 30".to_owned(),
        ..spawn_spec()
    };
    let job = host.spawn(spec, &ctx).await.unwrap();
    let written = rendering(async { job.signal(SignalKind::Kill, 1_000).await }).await;
    assert!(
        written.after_kill.is_none() && !written.stopped,
        "{written:?}"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(job.info().await.end.is_none());
    job.signal(SignalKind::Kill, 1_000).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while job.info().await.end.is_none() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the job never ended"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_start_render_gets_only_the_start_column_with_a_job_alive() {
    use super::super::imports::{ALL, Import, Inside};
    use wisp_plugin_api::host::{HoldSpec, Size, Typing, WaitFor};
    use wisp_plugin_api::types::SignalKind;
    let kernel = Arc::new(KernelHost::new());
    let root = std::env::temp_dir();
    kernel.bind(
        &Processes::default(),
        &KernelWorkspace {
            root: root.clone(),
            home: Some(root.clone()),
            temp: root.clone(),
        },
    );
    kernel.publish(Snapshot {
        facts: Facts {
            model: "claude-opus-5-5".to_owned(),
            ..Snapshot::default().facts
        },
        settings: vec![("time".to_owned(), "utc".to_owned())],
        media_types: vec!["image/png".to_owned()],
        media_slices: vec![SliceKind::Pages],
        image_edge: Some(8),
        fill: Fill {
            estimate: 5,
            ..Snapshot::default().fill
        },
        prices: Prices {
            model: pricing::model_info("claude-opus-5-5").copied(),
            spend: SPENT,
        },
        terminal: Some(Size { cols: 80, rows: 24 }),
        ..counting_snapshot()
    });
    let host = plugin_host(&["time"], &kernel);
    assert_eq!(host.terminal(), Some(Size { cols: 80, rows: 24 }));
    let _call = open_call(&kernel, "toolu_start", "probe");
    let ctx = FakeCall {
        id: "toolu_start".to_owned(),
        ..FakeCall::default()
    };
    let spec = SpawnSpec {
        program: "sleep".to_owned(),
        args: vec!["30".to_owned()],
        label: "sleep 30".to_owned(),
        background: true,
        ..spawn_spec()
    };
    let job = host.spawn(spec.clone(), &ctx).await.unwrap();
    assert_eq!(host.all().await.len(), 1);
    let inside = root.join("x");
    let inside = inside.display().to_string();
    let typing = || Typing {
        text: Some("x".to_owned()),
        keys: Vec::new(),
        signal: None,
        size: None,
    };
    let usage = ApiUsage {
        cache_read: 1_000_000,
        ..ApiUsage::default()
    };
    assert!(host.estimate(usage).is_some());
    let render = async {
        for import in ALL {
            let wit = import.wit();
            let mcp = PluginHost::new("mcp", "0.4.0", &[], kernel.clone()).origin(Origin::Bundled);
            match import {
                Import::McpPrompts => assert!(mcp.mcp().prompts().is_empty()),
                Import::McpGetPrompt => assert_eq!(mcp.mcp().get_prompt("local").await, Err(IN_START_RENDER.into())),
                Import::McpResources => assert!(mcp.mcp().resources().is_empty()),
                Import::McpReadResource => assert_eq!(mcp.mcp().read_resource("local").await, Err(IN_START_RENDER.into())),
                Import::McpServers => assert!(mcp.mcp().servers().is_empty()),
                Import::McpTools => assert!(mcp.mcp().tools().is_empty()),
                Import::McpRevision => assert_eq!(mcp.mcp().revision(), 0),
                Import::McpConnect => assert_eq!(
                    mcp.mcp().connect("local").await,
                    Err(IN_START_RENDER.into())
                ),
                Import::McpCall => assert_eq!(
                    mcp.mcp().call(&ctx, "mcp__local__echo", "{}", 1).await,
                    Err(IN_START_RENDER.into())
                ),
                Import::McpAdd => assert_eq!(
                    mcp.mcp().add("local", "true", &[]).await,
                    Err(IN_START_RENDER.into())
                ),
                Import::McpSignInStart => assert_eq!(mcp.mcp().sign_in_start("local").await, Err(IN_START_RENDER.into())),
                Import::McpSignInFinish => assert_eq!(mcp.mcp().sign_in_finish("local", "http://127.0.0.1:8123/oauth-callback?code=CANARY-code&state=CANARY-state").await, Err(IN_START_RENDER.into())),
                Import::McpRemove => {
                    assert_eq!(mcp.mcp().remove("local").await, Err(IN_START_RENDER.into()))
                }
                Import::IndexEntries => assert!(index_host(&kernel).index().entries().is_empty()),
                Import::IndexOpen => {
                    assert!(index_host(&kernel).index().open("entry").await.is_err())
                }
                Import::IndexChanged => {
                    index_host(&kernel).index().changed();
                    assert!(!kernel.index_dirty());
                }
                Import::SessionCurrent => assert_eq!(host.current(), Snapshot::default().facts),
                Import::WorkspaceRoot => assert_eq!(host.root(), root.display().to_string()),
                Import::WorkspaceHome => {
                    assert_eq!(host.home(), Some(root.display().to_string()));
                }
                Import::WorkspaceShorten => assert_eq!(host.shorten(&inside), "~/x"),
                Import::WorkspaceKindOf => {
                    assert_eq!(
                        host.kind_of(&root.display().to_string()),
                        Some(FileKind::Directory)
                    );
                }
                Import::ClockNow => assert_eq!(
                    host.now(),
                    Instant {
                        seconds: 0,
                        nanos: 0
                    }
                ),
                Import::ClockLocalOffset => assert_eq!(host.local_offset(host.now()), None),
                Import::ClockElapsedMs => assert_eq!(host.elapsed_ms(), 0),
                Import::ClockSleep => {
                    let started = std::time::Instant::now();
                    host.sleep(60_000).await;
                    assert!(started.elapsed() < Duration::from_secs(5));
                }
                Import::SettingsGet => assert_eq!(host.get("time").as_deref(), Some("utc")),
                Import::ContextCurrentFill => {
                    assert_eq!(host.current_fill(), Snapshot::default().fill);
                }
                Import::ContextEstimateText => assert!(host.estimate_text("some words") > 0),
                Import::MediaTypes => assert!(host.media_types().is_empty()),
                Import::MediaSlices => assert!(mcp.media_slices().is_empty()),
                Import::MediaImageEdge => assert_eq!(host.image_edge(), None),
                Import::MediaCountTokens => assert_eq!(
                    host.count_tokens("image/png".to_owned(), vec![1], None).await,
                    TokenCount::Unsupported
                ),
                Import::CallId
                | Import::CallCwd
                | Import::CallDeadlineMs
                | Import::CallCancelled
                | Import::CallWaitCancelled
                | Import::CallMoved
                | Import::CallSteered
                | Import::CallLiveTail
                | Import::CallWriteOutput
                | Import::CallOutputSummary
                | Import::CallPrivateFile
                | Import::CallAsk => {
                    assert_eq!(import.inside(Render::Start), Inside::NeedsCall, "{wit}");
                }
                Import::JobInfo => assert_eq!(job.info().await, idle::info()),
                Import::JobWrite => assert_eq!(job.write(typing(), 100).await, idle::written()),
                Import::JobSignal => {
                    assert_eq!(job.signal(SignalKind::Kill, 100).await, idle::written());
                }
                Import::JobResize => assert_eq!(job.resize(Size { cols: 9, rows: 9 }), Ok(())),
                Import::JobSetLimit => job.set_limit(Some(1)),
                Import::JobWait => {
                    let how = WaitFor {
                        timeout_ms: 60_000,
                        until: None,
                        quiet_ms: None,
                    };
                    assert_eq!(job.wait(how).await, Ok(idle::CANCELLED));
                }
                Import::JobHold => {
                    let spec = HoldSpec {
                        quiet_ms: Some(60_000),
                        yield_ms: Some(60_000),
                    };
                    assert_eq!(job.hold(spec).await, idle::CANCELLED);
                }
                Import::JobSettle => assert_eq!(job.settle(60_000, 60_000).await, idle::SETTLED),
                Import::JobScreen => assert_eq!(job.screen().await, None),
                Import::JobNewOutput => assert_eq!(job.new_output().await, idle::read()),
                Import::JobTail => assert_eq!(job.tail(10).await, idle::read()),
                Import::JobOutput => assert_eq!(job.output().await, idle::output()),
                Import::JobSaved => assert_eq!(job.saved().await, None),
                Import::JobLines => assert_eq!(job.lines(1, 10).await, idle::rows()),
                Import::JobSearch => {
                    assert_eq!(
                        job.search("x".to_owned(), 1, 0, 10).await,
                        Ok(idle::found())
                    );
                }
                Import::JobBackground => assert_eq!(job.background().await, None),
                Import::JobAskAttach => {
                    assert_eq!(
                        job.ask_attach("why".to_owned()).await,
                        AskReply::Unavailable
                    );
                }
                Import::JobsSpawn => assert_eq!(
                    host.spawn(spec.clone(), &ctx).await.err(),
                    Some(JobError::Invalid(NO_RUNNING_CALL.to_owned()))
                ),
                Import::JobsAdopt => assert!(host.adopt(1, &ctx).is_none()),
                Import::JobsAll => assert!(host.all().await.is_empty()),
                Import::JobsMarkReported => host.mark_reported(1, &ctx),
                Import::JobsWhich => assert_eq!(host.which("sleep"), None),
                Import::JobsCapture => assert_eq!(
                    host.capture("true".to_owned(), Vec::new(), 1_000).await,
                    Captured::Failed(IN_START_RENDER.to_owned())
                ),
                Import::TodosReplace => {
                    assert_eq!(
                        host.replace(Vec::new(), &ctx),
                        Err(IN_START_RENDER.to_owned())
                    );
                }
                Import::AgentsSpawnAgent
                | Import::AgentsListAgents
                | Import::AgentsReport
                | Import::AgentsSendMessage
                | Import::AgentsStopAgent => {
                    for result in every_agents_call(&host, &ctx) {
                        assert_eq!(result, Err(IN_START_RENDER.to_owned()), "{wit}");
                    }
                }
                Import::AgentRenameRenameAgent => {
                    let renamed = host.rename_agent("a".into(), "b".into(), &ctx);
                    assert_eq!(renamed, Err(IN_START_RENDER.to_owned()));
                }
                Import::ConfigSettings => {
                    assert_eq!(host.settings(None, &ctx), Err(IN_START_RENDER.to_owned()));
                }
                Import::AccountsList => {
                    assert_eq!(host.list(&ctx).await, Err(IN_START_RENDER.to_owned()));
                }
                Import::LogWrite => host.write(Level::Info, "a line"),
                Import::PointsFire => {
                    assert!(host.fire("x".to_owned(), String::new()).await.is_empty());
                }
                Import::WorkspaceFactsEnvironment => assert_eq!(host.environment(), None),
                Import::WorkspaceFactsConfigDir => assert_eq!(host.config_dir(), None),
                Import::WorkspaceFactsDataDir => {
                    assert_eq!(host.data_dir(), Err(IN_START_RENDER.to_owned()));
                }
                Import::WorkspaceFactsTerminal => assert_eq!(host.terminal(), None),
                Import::JobNewsOpening => assert_eq!(host.opening(), None),
                Import::PricingEstimate => assert_eq!(host.estimate(usage), None),
                Import::PricingSpend => assert_eq!(host.spend(), SpendTotals::default()),
                Import::HttpFetch => {
                    let refused = host.fetch(get("http://127.0.0.1:9/")).await.err();
                    assert_eq!(
                        refused,
                        Some(HttpError::NotAllowed(
                            super::super::http::IN_START_RENDER.to_owned()
                        ))
                    );
                }
                Import::HttpBodyRead
                | Import::HistoryViewSaid
                | Import::HistoryViewCalls
                | Import::HistoryViewLogPointer
                | Import::HistoryViewOutputPointer => {
                    assert_eq!(import.inside(Render::Start), Inside::Unavailable);
                }
                Import::AgentCurrent => assert_eq!(host.current_agent(), None),
                Import::AgentTreeSnapshot => assert!(host.snapshot().is_empty()),
                Import::AgentStatesStates => assert!(host.states().is_empty()),
                Import::ModelAsk => {
                    let req = wisp_plugin_api::model::Prompt {
                        system: String::new(),
                        text: String::new(),
                        max_tokens: 1,
                        native: None,
                    };
                    assert!(matches!(
                        host.model().ask(req, &ctx).await,
                        Err(wisp_plugin_api::model::AskError::NotAllowed(_))
                    ));
                }
                Import::UsageAccounts => assert!(host.usage().accounts().is_empty()),
                Import::ChatRename => assert_eq!(mcp.chat().rename("Strada".into()), Err(text::OUTSIDE.into())),
                Import::ProjectKey => assert!(mcp.project().key().is_none()),
                Import::SecretCheckCheck => {
                    assert_eq!(mcp.secret_check().check("ordinary text"), Verdict::Refused);
                }
                Import::PrivateFilesList => {
                    assert_eq!(mcp.private_files().list("", 1), Err(wisp_plugin_api::private_files::PrivateFileError::Unavailable));
                }
                Import::PrivateFilesRead => {
                    assert_eq!(mcp.private_files().read("example", 1), Err(wisp_plugin_api::private_files::PrivateFileError::Unavailable));
                }
                Import::PrivateFilesChange => {
                    use wisp_plugin_api::private_files::{ChangeAction, Expected, PrivateFileError};
                    assert_eq!(mcp.private_files().change("example", Expected::Absent, ChangeAction::Write(vec![]), None), Err(PrivateFileError::Unavailable));
                }
                Import::ManageManage => {
                    let search = PluginHost::new(manage::SEARCH_PLUGIN, "0.4.0", &[], kernel.clone());
                    let change = change(
                        wisp_plugin_api::manage::Action::Off,
                        wisp_plugin_api::manage::Kind::Tool,
                        "bash",
                    );
                    assert_eq!(
                        wisp_plugin_api::host::Host::manage(&search).manage(change, &ctx).await,
                        Err(IN_START_RENDER.into())
                    );
                }
                Import::ScreenReportWait => assert_eq!(host.wait("c".into(), 1).await, None),
                Import::SecretsTypeInto => {
                    let ask = secret_ask();
                    assert_eq!(host.type_into(ask, &ctx).await, SecretEnd::Unavailable);
                }
                Import::InstructionsReportLoaded | Import::InstructionsReportWarning => {
                    host.warning(wisp_plugin_api::instructions::Diagnostic::Large(1));
                    assert_eq!(kernel.take_instructed(), Instructed::default(), "{wit}");
                }
                Import::InstructionsReportChanged => {
                    host.changed(wisp_plugin_api::instructions::Change::default(), "t".into());
                    assert_eq!(kernel.take_change("t"), None, "{wit}");
                }
                Import::QuestionsPost => {
                    assert_eq!(
                        host.questions().post(Vec::new(), &ctx).await,
                        wisp_plugin_api::questions::Posted::Unavailable
                    );
                }
                Import::QuestionsWithdraw => {
                    assert!(!host.questions().withdraw(&ctx).await);
                }
            }
        }
    };
    Caller::rendering(Render::Start).scope(render).await;
    assert_eq!(host.all().await.len(), 1);
    assert!(job.info().await.end.is_none());
    job.signal(SignalKind::Kill, 1_000).await;
}

fn get(url: &str) -> Request {
    Request {
        method: "GET".to_owned(),
        url: url.to_owned(),
        headers: Vec::new(),
        body: None,
        timeout_ms: None,
    }
}

#[tokio::test]
async fn a_plugin_of_world_0_1_0_gets_no_answer_from_a_0_2_0_import() {
    use crate::plugin::{Make, PluginConfig, Registry};
    let mut answers = Vec::new();
    for world in ["0.1.0", "0.2.0"] {
        let hosts: Arc<Mutex<Vec<Arc<dyn Host>>>> = Arc::default();
        let make: Make = {
            let hosts = hosts.clone();
            let bundled = wisp_plugin_testkit::bundled();
            Arc::new(move |host: Arc<dyn Host>| {
                locked(&hosts).push(host.clone());
                (bundled.instantiate)(host)
            })
        };
        let manifest = wisp_plugin_testkit::MANIFEST
            .replace("world = \"0.2.0\"", &format!("world = \"{world}\""));
        let (registry, problems) =
            Registry::from_sources(vec![(manifest, make)], &PluginConfig::default());
        assert!(problems.is_empty(), "{problems:?}");
        let kernel = Arc::new(KernelHost::new());
        priced_at(&kernel, "claude-opus-5-5");
        kernel.set_config_dir(Some("~/.config/wisp".into()));
        registry.start(&BTreeSet::new(), kernel.clone()).await;
        kernel.offer_news("testkit", news());
        let host = locked(&hosts).first().cloned().unwrap();
        let usage = ApiUsage {
            cache_read: 1_000_000,
            ..ApiUsage::default()
        };
        let fetched = host.fetch(get("ftp://127.0.0.1/")).await.err();
        answers.push((
            host.estimate(usage),
            host.spend(),
            fetched,
            host.environment(),
            host.config_dir(),
            host.opening(),
        ));
    }
    let not_allowed = |why: &str| Some(HttpError::NotAllowed(why.to_owned()));
    assert_eq!(
        answers,
        [
            (
                None,
                SpendTotals::default(),
                not_allowed(super::super::http::OLD_WORLD),
                None,
                None,
                None
            ),
            (
                Some(200_000),
                SPENT,
                not_allowed(super::super::http::ONLY_HTTP),
                None,
                Some("~/.config/wisp".into()),
                Some(news())
            ),
        ]
    );
}

#[test]
fn the_terminal_answers_the_published_size_to_world_0_2_0_only() {
    let kernel = Arc::new(KernelHost::new());
    let size = Size {
        cols: 121,
        rows: 40,
    };
    let new = PluginHost::new("workspace", "0.2.0", &[], kernel.clone());
    let old = PluginHost::new("workspace", "0.1.0", &[], kernel.clone());
    assert_eq!(new.terminal(), None);
    kernel.publish(Snapshot {
        terminal: Some(size),
        ..Snapshot::default()
    });
    assert_eq!(new.terminal(), Some(size));
    assert_eq!(old.terminal(), None);
}

#[tokio::test]
async fn data_dir_gives_each_plugin_its_own_private_folder_and_none_in_a_slot_render() {
    let scratch = crate::test_guard::ScratchDir::new("wisp-data-dir-", "plugins");
    let kernel = Arc::new(KernelHost::new());
    let board = PluginHost::new("board", "0.2.0", &[], kernel.clone());
    let other = PluginHost::new("other", "0.2.0", &[], kernel.clone());
    let old = PluginHost::new("old", "0.1.0", &[], kernel.clone());
    assert_eq!(board.data_dir(), Err(NO_DATA_FOLDER.to_owned()));
    kernel.set_data_dir(Some(scratch.root.join("plugin-data")));
    let dir = board.data_dir().unwrap();
    assert_eq!(
        dir,
        scratch.root.join("plugin-data/board").display().to_string()
    );
    assert_eq!(board.data_dir().unwrap(), dir);
    assert_eq!(
        other.data_dir().unwrap(),
        scratch.root.join("plugin-data/other").display().to_string()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }
    assert_eq!(old.data_dir(), Err(NO_DATA_FOLDER.to_owned()));
    let rendered = rendering(async { board.data_dir() }).await;
    assert_eq!(rendered, Err(IN_RENDER.to_owned()));
}

#[tokio::test]
async fn project_key_answers_a_0_4_0_plugin_outside_renders_only() {
    use std::collections::BTreeSet;
    use std::sync::Mutex;

    use futures::FutureExt;
    use wisp_plugin_api::manifest::CORE_ID;
    use wisp_plugin_api::tools::{BuildContext, ClientTool, Prepared, ToolDef, ToolKind};
    use wisp_plugin_api::types::ToolRun;

    use crate::plugin::{Make, PluginConfig, Registry, testing};

    struct Probe {
        host: Arc<dyn Host>,
        seen: Arc<Mutex<Vec<Option<String>>>>,
        prepared: Arc<Mutex<Option<String>>>,
    }

    impl Plugin for Probe {
        fn describe<'a>(&'a self, _ctx: &'a BuildContext) -> BoxFuture<'a, Vec<ToolDef>> {
            self.seen.lock().unwrap().push(self.host.project().key());
            let host = self.host.clone();
            let seen = self.seen.clone();
            Box::pin(async move {
                seen.lock().unwrap().push(host.project().key());
                vec![ToolDef {
                    name: "probe".to_owned(),
                    kind: ToolKind::Client(ClientTool {
                        description: "Probe tool.".to_owned(),
                        input_schema: r#"{"type":"object"}"#.to_owned(),
                        meta: testing::meta(1_000, Some(1_000)),
                    }),
                    system_line: None,
                    context_line: None,
                }]
            })
        }

        fn prepare<'a>(
            &'a self,
            _tool: &'a str,
            _input: &'a str,
        ) -> BoxFuture<'a, Result<Box<dyn Prepared>, String>> {
            *self.prepared.lock().unwrap() = self.host.project().key();
            Box::pin(std::future::ready(Err("probe".to_owned())))
        }

        fn run<'a>(
            &'a self,
            _call: Box<dyn Prepared>,
            _ctx: &'a dyn CallContext,
        ) -> BoxFuture<'a, ToolRun> {
            futures::future::ready(ToolRun::completed()).boxed()
        }
    }

    let scratch = crate::test_guard::ScratchDir::new("wisp-project-key-", "host");
    let data = scratch.root.join("plugin-data");
    let kernel = Arc::new(KernelHost::new());
    let host = PluginHost::new("probe", "0.4.0", &[], kernel.clone());
    kernel.set_data_dir(Some(data.clone()));
    assert!(host.project().key().is_none());

    let workspace = scratch.workspace();
    crate::instructions::make_repository(&workspace.root).unwrap();
    kernel.bind(&Processes::default(), &workspace);
    assert!(rendering(async { host.project().key().is_none() }).await);
    assert!(
        Caller::rendering(Render::Start)
            .scope(async { host.project().key().is_none() })
            .await
    );

    let seen = Arc::new(Mutex::new(Vec::new()));
    let prepared = Arc::new(Mutex::new(None));
    let make: Make = {
        let seen = seen.clone();
        let prepared = prepared.clone();
        Arc::new(move |plugin_host: Arc<dyn Host>| {
            seen.lock().unwrap().push(plugin_host.project().key());
            Arc::new(Probe {
                host: plugin_host,
                seen: seen.clone(),
                prepared: prepared.clone(),
            }) as Arc<dyn Plugin>
        })
    };
    let manifest = testing::manifest("probe", &["probe".to_owned()], &[])
        .replace(r#"world = "0.2.0""#, r#"world = "0.4.0""#);
    let (registry, problems) =
        Registry::from_sources(vec![(manifest, make)], &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let on: BTreeSet<String> = ["probe".to_owned()].into_iter().collect();
    let context = registry.build_context(&on);
    let started = registry.start(&on, kernel.clone()).await;
    assert!(started.problems.is_empty(), "{:?}", started.problems);
    let redescribed = registry
        .redescribe(&on, &context, &started.hooks.plugins, &kernel)
        .await;
    assert!(
        redescribed.problems.is_empty(),
        "{:?}",
        redescribed.problems
    );
    let copied = registry.describe_with(&on, &context, &kernel).await;
    assert!(copied.contains_key("probe"));
    {
        let during_describe = seen.lock().unwrap();
        assert_eq!(during_describe.len(), 8);
        assert!(during_describe.iter().all(Option::is_none));
    }
    assert!(!data.join(CORE_ID).exists());

    let _ = started.hooks.plugins["probe"].prepare("probe", "{}").await;
    let from_tool = prepared.lock().unwrap().clone();
    let key = host.project().key();
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    assert!(key.is_some());
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        assert!(key.is_none());
        assert!(!data.join(CORE_ID).exists());
    }
    assert!(from_tool == key);
    assert!(key == crate::workspace::project_key(&workspace.root, &data));
    let old = PluginHost::new("probe", "0.3.0", &[], kernel.clone());
    assert!(old.project().key().is_none());
    kernel.set_data_dir(None);
    assert!(host.project().key().is_none());
}

#[test]
fn the_tool_text_copy_of_the_roots_name_matches_the_protocols() {
    assert_eq!(
        wisp_tool_text::agents::ROOT_NAME,
        wisp_protocol::agents::ROOT_NAME
    );
}

fn news() -> wisp_plugin_api::prompt::News {
    wisp_plugin_api::prompt::News {
        wake: false,
        items: vec![wisp_plugin_api::prompt::Item::Typed(3)],
    }
}

#[test]
fn job_news_answers_only_its_plugin_while_offered_and_tells_whether_it_was_read() {
    let kernel = Arc::new(KernelHost::new());
    let shell = PluginHost::new("shell", "0.2.0", &[], kernel.clone());
    let other = PluginHost::new("other", "0.2.0", &[], kernel.clone());
    assert_eq!(shell.opening(), None);
    kernel.offer_news("shell", news());
    assert!(!kernel.withdraw_news());
    kernel.offer_news("shell", news());
    assert_eq!(other.opening(), None);
    assert_eq!(shell.opening(), Some(news()));
    assert!(kernel.withdraw_news());
    assert_eq!(shell.opening(), None);
    assert!(!kernel.withdraw_news());
}

#[test]
fn the_instructions_report_is_refused_in_a_slot_render_and_to_a_0_1_0_host() {
    use wisp_plugin_api::instructions::{Change, Diagnostic, LoadReport};
    let kernel = Arc::new(KernelHost::new());
    let report = |host: &PluginHost| {
        host.loaded(LoadReport::default());
        host.warning(Diagnostic::Large(1));
        host.changed(Change::default(), "t".into());
    };
    let current = PluginHost::new(INSTRUCTIONS_PLUGIN, "0.2.0", &[], kernel.clone());
    Caller::rendering(Render::Slot).sync_scope(|| report(&current));
    assert_eq!(kernel.take_instructed(), Instructed::default());
    assert_eq!(kernel.take_change("t"), None);
    let older = PluginHost::new(INSTRUCTIONS_PLUGIN, "0.1.0", &[], kernel.clone());
    report(&older);
    assert_eq!(kernel.take_instructed(), Instructed::default());
    assert_eq!(kernel.take_change("t"), None);
    report(&current);
    assert_eq!(kernel.take_change("t"), Some(Change::default()));
    assert_eq!(
        kernel.take_instructed(),
        Instructed {
            report: Some(LoadReport::default()),
            warnings: vec![Diagnostic::Large(1)],
        }
    );
}

#[test]
fn only_the_latest_instructions_changes_wait_for_a_take() {
    use wisp_plugin_api::instructions::Change;
    let kernel = Arc::new(KernelHost::new());
    let host = PluginHost::new(INSTRUCTIONS_PLUGIN, "0.2.0", &[], kernel.clone());
    for n in 0..=KEPT_CHANGES {
        host.changed(Change::default(), n.to_string());
    }
    assert_eq!(kernel.take_change("0"), None);
    for n in 1..=KEPT_CHANGES {
        host.changed(Change::default(), n.to_string());
    }
    assert_eq!(
        kernel.take_change(&KEPT_CHANGES.to_string()),
        Some(Change::default())
    );
    assert_eq!(kernel.take_change("1"), None, "a take clears the rest");
}

fn resolved(
    name: &str,
    value: Option<SettingValue>,
    layer: wisp_protocol::settings::Layer,
) -> ResolvedSetting {
    ResolvedSetting {
        name: wisp_protocol::settings::SettingName::parse(name).unwrap(),
        value,
        layer,
        origin: format!("config.toml line 3 ORIGIN-CANARY-{name}"),
        shadowed: Vec::new(),
        choices: Vec::new(),
        applies: true,
    }
}

const KEY_SHAPED: &str = "sk-ant-api03-canary012345CANARYcdef";

fn view() -> Arc<CallView> {
    use wisp_protocol::settings::Layer;
    Arc::new(CallView {
        settings: vec![
            resolved(
                "effort",
                Some(SettingValue::Text("high".into())),
                Layer::Session,
            ),
            resolved(
                "endpoint",
                Some(SettingValue::Text(KEY_SHAPED.into())),
                Layer::Flag,
            ),
            resolved("time", None, Layer::Default),
        ],
        accounts: accounts::AccountsView {
            model: "claude-opus-5-5".to_owned(),
            effort: "high".to_owned(),
            rows: vec![(
                "work".to_owned(),
                AccountRow {
                    name: "work".to_owned(),
                    yours: true,
                    ..AccountRow::default()
                },
            )],
            auth: None,
        },
    })
}

fn rows(found: &[SettingRow]) -> Vec<(&str, &str, &str)> {
    found
        .iter()
        .map(|r| (r.name.as_str(), r.value.as_str(), r.layer.as_str()))
        .collect()
}

#[test]
fn only_the_menus_plugins_own_running_tool_calls_read_the_settings_and_accounts() {
    let block_on = |listed: BoxFuture<'_, Result<AccountList, String>>| {
        listed
            .now_or_never()
            .expect("a list with no auth host answers at once")
    };
    assert_eq!(MENUS_PLUGIN, wisp_plugin_menus::ID);
    let kernel = Arc::new(KernelHost::new());
    let probe = plugin_host(&[], &kernel);
    let menus = PluginHost::new(MENUS_PLUGIN, "0.2.0", &[], kernel.clone());
    open_viewed(&kernel, "toolu_probe", "probe", Some(view()));
    open_viewed(&kernel, "toolu_menus", MENUS_PLUGIN, Some(view()));
    let ctx = |id: &str| FakeCall {
        id: id.to_owned(),
        ..FakeCall::default()
    };
    for id in ["toolu_probe", "toolu_menus", "nobody"] {
        assert_eq!(
            probe.settings(None, &ctx(id)),
            Err("the config interface belongs to the bundled menus plugin, not probe".to_owned()),
            "{id}"
        );
        assert_eq!(
            block_on(probe.list(&ctx(id))),
            Err("the accounts interface belongs to the bundled menus plugin, not probe".to_owned()),
            "{id}"
        );
    }
    for (id, why) in [
        ("nobody", "a hook's context names no running call"),
        ("toolu_probe", "another plugin's call"),
    ] {
        let refused = Some(format!(
            "no menus tool call {id} is running, so nothing was done"
        ));
        assert_eq!(menus.settings(None, &ctx(id)).err(), refused, "{why}");
        assert_eq!(block_on(menus.list(&ctx(id))).err(), refused, "{why}");
    }
    let own = ctx("toolu_menus");
    let listed = block_on(menus.list(&own)).unwrap();
    assert_eq!(
        (
            listed.model.as_str(),
            listed.effort.as_str(),
            listed.rows.len()
        ),
        ("claude-opus-5-5", "high", 1)
    );
    let all = menus.settings(None, &own).unwrap();
    assert_eq!(
        rows(&all),
        [
            ("effort", "high", "session"),
            ("endpoint", "(not shown)", "flag"),
            ("time", "local", "default"),
        ]
    );
    let text = format!("{all:?}");
    for hidden in [KEY_SHAPED, "ORIGIN-CANARY", "config.toml"] {
        assert!(!text.contains(hidden), "{hidden} in {text}");
    }
    let time = wisp_protocol::settings::find("time").unwrap();
    let stamp = menus.settings(Some("stamp".to_owned()), &own).unwrap();
    assert_eq!(
        stamp,
        [SettingRow {
            name: "time".to_owned(),
            value: "local".to_owned(),
            layer: "default".to_owned(),
            help: time.help.to_owned(),
            rule: time.describe(Form::Rule),
        }]
    );
    assert_eq!(menus.settings(Some("xyz".to_owned()), &own), Ok(Vec::new()));
    open_call(&kernel, "toolu_bare", MENUS_PLUGIN);
    let bare = Some("no menus tool call toolu_bare is running, so nothing was done".to_owned());
    assert_eq!(
        menus.settings(None, &ctx("toolu_bare")).err(),
        bare,
        "no view"
    );
    assert_eq!(
        block_on(menus.list(&ctx("toolu_bare"))).err(),
        bare,
        "no view"
    );
    kernel.close_call("toolu_menus");
    let stale = Some("no menus tool call toolu_menus is running, so nothing was done".to_owned());
    assert_eq!(menus.settings(None, &own).err(), stale, "a stale call id");
    assert_eq!(block_on(menus.list(&own)).err(), stale, "a stale call id");
}

#[test]
fn an_account_name_in_a_value_is_shown_only_when_it_could_not_be_a_key() {
    use wisp_protocol::settings::Layer;
    for (name, shown) in [("claude-work", "claude-work"), (KEY_SHAPED, "(not shown)")] {
        let row = setting_row(&resolved(
            "endpoint",
            Some(SettingValue::Text(name.into())),
            Layer::Default,
        ));
        assert_eq!(row.value, shown, "{name}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sign_in_is_the_status_answer_read_only_for_subscriptions_and_none_after_three_seconds() {
    use wisp_protocol::subscription_auth::{
        LocalState, SubscriptionEndpoint, SubscriptionProvider,
    };
    let states = [
        (LocalState::Working, SignIn::Working),
        (LocalState::Expired, SignIn::Expired),
        (LocalState::Refreshable, SignIn::Refreshable),
        (LocalState::Unavailable, SignIn::Unavailable),
        (LocalState::SetupTokenExpiryUnknown, SignIn::ExpiryUnknown),
    ];
    let host = crate::endpoints::testing::AuthHost::new();
    *host.status.lock().unwrap() = states
        .iter()
        .enumerate()
        .map(|(n, (state, _))| SubscriptionEndpoint {
            alias: format!("plan-{n}"),
            provider: SubscriptionProvider::Claude,
            storage: None,
            is_default: false,
            state: *state,
        })
        .collect();
    let row = |alias: String, limits: Option<Limits>| {
        (
            alias.clone(),
            AccountRow {
                name: alias,
                limits,
                ..AccountRow::default()
            },
        )
    };
    let mut rows: Vec<_> = (0..states.len())
        .map(|n| row(format!("plan-{n}"), Some(Limits::default())))
        .collect();
    rows.push(row("api".to_owned(), None));
    let view = |rows: Vec<(String, AccountRow)>| accounts::AccountsView {
        model: String::new(),
        effort: String::new(),
        rows,
        auth: Some(host.clone()),
    };
    let signed = |list: AccountList| -> Vec<Option<SignIn>> {
        list.rows.iter().map(|row| row.sign_in).collect()
    };
    let cancel = CancellationToken::new();
    let mut expected: Vec<Option<SignIn>> = states.iter().map(|(_, word)| Some(*word)).collect();
    expected.push(None);
    assert_eq!(signed(view(rows.clone()).list(&cancel).await), expected);
    assert_eq!(host.calls.lock().unwrap().len(), 1);
    let api_only = view(vec![row("api".to_owned(), None)]).list(&cancel).await;
    assert_eq!(signed(api_only), [None]);
    assert_eq!(
        host.calls.lock().unwrap().len(),
        1,
        "no subscription, no read"
    );
    *host.stall.lock().unwrap() = Some(Duration::from_secs(5));
    let started = std::time::Instant::now();
    let stalled = view(rows).list(&cancel).await;
    let waited = started.elapsed();
    assert!(
        waited >= accounts::SIGN_IN_WAIT && waited < Duration::from_millis(4_500),
        "{waited:?}"
    );
    assert_eq!(signed(stalled.clone()), vec![None; states.len() + 1]);
    assert_eq!(stalled.rows[5].name, "api");
}

fn index_entry(kind: wisp_plugin_api::index::Kind, name: &str) -> wisp_plugin_api::index::Entry {
    wisp_plugin_api::index::Entry {
        kind,
        name: name.to_owned(),
        summary: "Placeholder entry".to_owned(),
        text: format!("{name} placeholder entry"),
    }
}

fn index_host(kernel: &Arc<KernelHost>) -> PluginHost {
    PluginHost::new("search", "0.3.0", &[], kernel.clone()).origin(Origin::Bundled)
}

fn entry_source(
    id: &str,
    entries: Vec<wisp_plugin_api::index::Entry>,
    fails: bool,
) -> Arc<dyn Plugin> {
    let (_, make) = crate::plugin::testing::index_source(id, Arc::new(Mutex::new(entries)), fails);
    make(Arc::new(wisp_plugin_testkit::host::FakeHost::default()))
}

#[test]
fn secret_check_refuses_older_worlds_and_default_fallback_and_allows_ordinary_calls() {
    const SAFE: &str = "a portable note";
    let kernel = Arc::new(KernelHost::new());
    let older = PluginHost::new("probe", "0.3.0", &[], kernel.clone());
    assert_eq!(older.secret_check().check(SAFE), Verdict::Refused);
    assert_eq!(().check(SAFE), Verdict::Refused);

    for origin in [Origin::Bundled, Origin::External] {
        let host = PluginHost::new("probe", "0.4.0", &[], kernel.clone()).origin(origin);
        for (caller, expected) in [
            (Caller::describing(), Verdict::Refused),
            (Caller::rendering(Render::Start), Verdict::Refused),
            (Caller::rendering(Render::Slot), Verdict::Safe),
            (Caller::default(), Verdict::Safe),
            (Caller::call(CancellationToken::new(), None), Verdict::Safe),
        ] {
            assert_eq!(
                caller.sync_scope(|| host.secret_check().check(SAFE)),
                expected
            );
        }
    }
}

#[test]
fn secret_check_uses_live_held_credentials_and_bound_or_unbound_home() {
    const FALLBACK: &str = "/srv/wisp-check-fallback";
    const BOUND: &str = "/srv/wisp-check-bound";
    const SHORT: &str = "CANARYpin";
    const LONG: &str = "CANARY-long-live-check-credential";
    let kernel = Arc::new(KernelHost::new());
    let host = PluginHost::new("probe", "0.4.0", &[], kernel.clone());
    let check = |text: &str| host.secret_check().check(text);
    let bind = |home| {
        kernel.bind(
            &Processes::default(),
            &KernelWorkspace {
                root: "/workspace".into(),
                home,
                temp: "/tmp".into(),
            },
        );
    };
    let fallback_path = format!("{FALLBACK}/note");
    let bound_path = format!("{BOUND}/note");
    kernel.set_fallback_home(Some(FALLBACK.into()));
    assert_eq!(check(&fallback_path), Verdict::Refused);
    assert_eq!(check(&bound_path), Verdict::Safe);
    bind(Some(BOUND.into()));
    assert_eq!(check(&fallback_path), Verdict::Safe);
    assert_eq!(check(&bound_path), Verdict::Refused);
    bind(None);
    assert_eq!(check(&fallback_path), Verdict::Refused);
    kernel.set_fallback_home(None);
    assert_eq!(check(&fallback_path), Verdict::Safe);

    let short = format!("key={SHORT}");
    let long = format!("prefix{LONG}suffix");
    assert_eq!(check(&short), Verdict::Safe);
    assert_eq!(check(&long), Verdict::Safe);
    crate::session::register_secrets([SHORT.to_owned(), LONG.to_owned()]);
    assert_eq!(check(&short), Verdict::Refused);
    assert_eq!(check(&long), Verdict::Refused);
    assert_eq!(check(&format!("_{SHORT}_")), Verdict::Safe);
}

#[tokio::test]
async fn secret_checked_index_source_hides_stale_entries_and_dirties_on_late_credential_or_home_change()
 {
    use wisp_plugin_api::index::{Entry, Kind};
    use wisp_plugin_api::tools::Prepared;
    use wisp_plugin_api::types::ToolRun;
    use wisp_plugin_testkit::{Call, fallback_display};

    const BODY: &str = "CANARY-omitted-index-body-credential";
    struct Source {
        host: PluginHost,
        entry: Option<(Kind, &'static str)>,
        body: Option<&'static str>,
    }
    impl Plugin for Source {
        fn entries(&self) -> BoxFuture<'_, Vec<Entry>> {
            let check = || {
                self.body
                    .is_none_or(|body| self.host.secret_check().check(body) == Verdict::Safe)
            };
            let entry = self.entry.filter(|_| check());
            async move {
                if self.entry.is_none() {
                    assert!(!check());
                }
                entry
                    .into_iter()
                    .map(|(kind, name)| index_entry(kind, name))
                    .collect()
            }
            .boxed()
        }

        fn run<'a>(
            &'a self,
            _call: Box<dyn Prepared>,
            _ctx: &'a dyn CallContext,
        ) -> BoxFuture<'a, ToolRun> {
            assert!(Caller::current().gather_checked.is_none());
            assert_eq!(
                self.host.secret_check().check("ordinary text"),
                Verdict::Safe
            );
            futures::future::ready(ToolRun::completed()).boxed()
        }
    }

    let kernel = Arc::new(KernelHost::new());
    let index = index_host(&kernel);
    let make = |id, entry, body| {
        Arc::new(Source {
            host: PluginHost::new(id, "0.4.0", &[], kernel.clone()),
            entry,
            body,
        })
    };
    let all = crate::tool::test_toolbox();
    let toolbox = crate::tool::Toolbox::default().with_held([all.get("echo").unwrap().clone()]);
    let owner = toolbox.held_tool("echo").unwrap().tool().id();
    let allowed = BTreeSet::from(["echo".to_owned()]);
    let plain = make("plain", Some((Kind::Document, "plain")), None);
    let sources = BTreeMap::from([
        (
            owner.to_owned(),
            make(owner, Some((Kind::Tool, "echo")), Some(BODY)) as Arc<dyn Plugin>,
        ),
        ("plain".to_owned(), plain.clone() as Arc<dyn Plugin>),
    ]);
    assert!(
        kernel
            .gather_index(&toolbox, &allowed, sources)
            .await
            .is_empty()
    );
    assert!(!kernel.index_dirty());
    assert!(kernel.index_parameters("echo").is_some());
    assert_eq!(kernel.index_entries().len(), 2);
    plain
        .run(
            Call::boxed(fallback_display("probe", "{}"), ()),
            &FakeCall::default(),
        )
        .await;
    crate::session::register_secrets([BODY.to_owned()]);
    assert!(kernel.index_dirty());
    assert_eq!(
        kernel.index_entries(),
        [index_entry(Kind::Document, "plain")]
    );
    assert_eq!(kernel.index_kind("echo"), None);
    assert_eq!(kernel.index_parameters("echo"), None);
    assert_eq!(
        index.index().open("echo").await.unwrap_err(),
        wisp_tool_text::index::STALE_SECRET_CHECK
    );

    let empty = make("empty", None, Some(BODY));
    assert!(
        kernel
            .gather_index(
                &crate::tool::Toolbox::default(),
                &BTreeSet::new(),
                BTreeMap::from([("empty".to_owned(), empty as Arc<dyn Plugin>)])
            )
            .await
            .is_empty()
    );
    assert!(kernel.index_entries().is_empty());
    assert!(!kernel.index_dirty());
    kernel.bind(
        &Processes::default(),
        &KernelWorkspace {
            root: "/workspace".into(),
            home: Some("/srv/wisp-index-check".into()),
            temp: "/tmp".into(),
        },
    );
    assert!(kernel.index_dirty());
}

#[tokio::test]
async fn index_entries_sort_by_kind_and_name_and_leave_out_an_off_tool() {
    use wisp_plugin_api::index::Kind;
    let kernel = Arc::new(KernelHost::new());
    let all = crate::tool::test_toolbox();
    let mut toolbox = crate::tool::Toolbox::default().with_held([
        all.get("echo").unwrap().clone(),
        all.get("sleep").unwrap().clone(),
    ]);
    let owner = toolbox.held_tool("echo").unwrap().tool().id().to_owned();
    let echo = wisp_plugin_api::index::Entry {
        summary: "Echo from its owner".to_owned(),
        ..index_entry(Kind::Tool, "echo")
    };
    let sources = |ids: &[&str]| {
        ids.iter()
            .map(|id| {
                let mut entries = vec![
                    echo.clone(),
                    index_entry(Kind::Tool, "sleep"),
                    index_entry(Kind::Tool, "gone"),
                ];
                if *id == "aa" {
                    entries = vec![
                        index_entry(Kind::Document, "guide"),
                        index_entry(Kind::Skill, "how-to"),
                    ];
                }
                (id.to_string(), entry_source(id, entries, false))
            })
            .collect::<BTreeMap<_, _>>()
    };
    let allowed = ["echo".to_owned(), "gone".to_owned()].into_iter().collect();
    let problems = kernel
        .gather_index(&toolbox, &allowed, sources(&["aa", &owner]))
        .await;
    assert!(problems.is_empty(), "{problems:?}");
    assert!(plugin_host(&[], &kernel).index().entries().is_empty());
    let entries = index_host(&kernel).index().entries();
    assert_eq!(
        entries
            .iter()
            .map(|entry| (entry.kind, entry.name.as_str()))
            .collect::<Vec<_>>(),
        [
            (Kind::Tool, "echo"),
            (Kind::Skill, "how-to"),
            (Kind::Document, "guide")
        ]
    );
    assert_eq!(entries[0], echo);
    assert_eq!(
        index_host(&kernel).index().open("echo").await.unwrap(),
        toolbox.definition("echo").unwrap().input_schema.to_string()
    );
    assert!(index_host(&kernel).index().open("sleep").await.is_err());
    let problems = kernel
        .gather_index(&toolbox, &allowed, sources(&["zz"]))
        .await;
    assert_eq!(
        problems
            .iter()
            .map(|problem| (problem.plugin.as_str(), problem.message.as_str()))
            .collect::<Vec<_>>(),
        [(
            "zz",
            "its tool `echo` has the same name as an earlier entry; it is left out"
        )]
    );
    let spec = toolbox.definition("echo").unwrap();
    assert_eq!(
        kernel.index_entries()[0],
        wisp_index::tool_entry(&spec.name, &spec.description, &spec.input_schema)
    );
    toolbox.turn_on("echo");
    let problems = kernel
        .gather_index(&toolbox, &allowed, sources(&[&owner]))
        .await;
    assert!(problems.is_empty(), "{problems:?}");
    assert!(kernel.index_entries().is_empty());
}

#[tokio::test]
async fn a_server_published_during_gather_does_not_add_out_of_scope_skills_or_documents() {
    use tokio::sync::Notify;
    use wisp_plugin_api::index::{Entry, Kind};

    #[derive(Default)]
    struct GatedEntries {
        started: Notify,
        listed: Notify,
    }

    impl Plugin for GatedEntries {
        fn entries(&self) -> wisp_plugin_api::BoxFuture<'_, Vec<Entry>> {
            Box::pin(async move {
                self.started.notify_one();
                self.listed.notified().await;
                vec![
                    index_entry(Kind::Skill, "mcp__local__prompt:review.code"),
                    index_entry(Kind::Document, "mcp__local__resource:test:///notes"),
                ]
            })
        }
    }

    let folder = tempfile::tempdir().unwrap();
    let kernel = KernelHost::new();
    let service = crate::mcp::Service::new(BTreeMap::new(), folder.path().into(), None);
    kernel.set_mcp(service.clone());
    kernel.scope_mcp();
    let source = Arc::new(GatedEntries::default());
    let sources = BTreeMap::from([("mcp".into(), source.clone() as Arc<dyn Plugin>)]);
    let toolbox = crate::tool::Toolbox::default();
    let allowed = BTreeSet::new();
    let (problems, ()) = tokio::join!(kernel.gather_index(&toolbox, &allowed, sources), async {
        source.started.notified().await;
        service
            .add(
                "local",
                crate::config::McpServer {
                    command: crate::test_guard::mcp_server(),
                    args: vec!["text".into()],
                    keys: Vec::new(),
                },
            )
            .await
            .unwrap();
        source.listed.notify_one();
    });
    assert!(problems.is_empty(), "{problems:?}");
    assert!(!service.prompts().is_empty() && !service.resources().is_empty());
    assert!(kernel.index_entries().is_empty());
}

#[tokio::test]
async fn opening_an_index_entry_routes_to_its_owning_source() {
    use wisp_plugin_api::index::Kind;
    let kernel = Arc::new(KernelHost::new());
    let sources: BTreeMap<_, _> = [
        (
            "aa".to_owned(),
            entry_source("aa", vec![index_entry(Kind::Skill, "one")], false),
        ),
        (
            "bb".to_owned(),
            entry_source("bb", vec![index_entry(Kind::Document, "two")], false),
        ),
    ]
    .into_iter()
    .collect();
    assert!(
        kernel
            .gather_index(
                &crate::tool::Toolbox::default(),
                &BTreeSet::new(),
                sources.clone()
            )
            .await
            .is_empty()
    );
    let host = index_host(&kernel);
    assert_eq!(host.index().open("one").await.unwrap(), "aa: one");
    assert_eq!(host.index().open("two").await.unwrap(), "bb: two");
    assert_eq!(
        host.index().open("missing").await.unwrap_err(),
        "nothing named `missing` was found"
    );
    let entries = Arc::new(Mutex::new(vec![index_entry(Kind::Skill, "retained")]));
    let source = crate::plugin::testing::index_source("source", entries, false);
    let (registry, problems) = crate::plugin::Registry::from_sources(
        vec![source],
        &crate::plugin::PluginConfig::default(),
    );
    assert!(problems.is_empty(), "{problems:?}");
    let started = registry.start_exact(&BTreeSet::new(), kernel.clone()).await;
    let source = Arc::downgrade(started.hooks.plugins.get("source").unwrap());
    assert_eq!(
        host.index().open("retained").await.unwrap(),
        "source: retained"
    );
    drop(started);
    assert!(source.upgrade().is_none());
    assert!(host.index().open("retained").await.is_err());
    let weak = Arc::downgrade(&kernel);
    drop(host);
    drop(kernel);
    assert!(weak.upgrade().is_none());
}

#[tokio::test]
async fn a_panicking_index_source_is_left_out_with_a_problem() {
    use wisp_plugin_api::index::Kind;
    let kernel = Arc::new(KernelHost::new());
    let sources = [
        (
            "aa".to_owned(),
            entry_source("aa", vec![index_entry(Kind::Skill, "missing")], true),
        ),
        (
            "bb".to_owned(),
            entry_source("bb", vec![index_entry(Kind::Skill, "kept")], false),
        ),
    ]
    .into_iter()
    .collect();
    let problems = kernel
        .gather_index(&crate::tool::Toolbox::default(), &BTreeSet::new(), sources)
        .await;
    assert_eq!(problems.len(), 1);
    assert_eq!(problems[0].plugin, "aa");
    assert!(problems[0].message.contains("panicked"));
    assert_eq!(index_host(&kernel).index().entries()[0].name, "kept");
}

#[tokio::test]
async fn an_open_from_a_checked_source_rechecks_its_stamp_after_the_owner_replies() {
    use tokio::sync::Notify;
    use wisp_plugin_api::index::{Entry, Kind};

    const NAME: &str = "checked note";
    const HOME: &str = "/srv/wisp-new-home";
    struct Source {
        host: PluginHost,
        started: Notify,
        finish: Notify,
        answer: Result<&'static str, &'static str>,
    }
    impl Plugin for Source {
        fn entries(&self) -> BoxFuture<'_, Vec<Entry>> {
            let text = match self.answer {
                Ok(text) | Err(text) => text,
            };
            assert_eq!(self.host.secret_check().check(text), Verdict::Safe);
            futures::future::ready(vec![index_entry(Kind::Document, NAME)]).boxed()
        }

        fn open_entry<'a>(&'a self, _name: &'a str) -> BoxFuture<'a, Result<String, String>> {
            async move {
                self.started.notify_one();
                self.finish.notified().await;
                self.answer.map(str::to_owned).map_err(str::to_owned)
            }
            .boxed()
        }
    }

    let stale = Err(wisp_tool_text::index::STALE_SECRET_CHECK.to_owned());
    for (answer, change_home, register) in [
        (Ok("a checked note"), false, None),
        (Ok("/srv/wisp-new-home/note"), true, None),
        (Err("/srv/wisp-new-home/note"), true, None),
        (
            Ok("CANARYnewInFlightCredential"),
            false,
            Some("CANARYnewInFlightCredential"),
        ),
        (
            Err("CANARYnewInFlightErrorCredential"),
            false,
            Some("CANARYnewInFlightErrorCredential"),
        ),
    ] {
        let kernel = Arc::new(KernelHost::new());
        let source = Arc::new(Source {
            host: PluginHost::new("probe", "0.4.0", &[], kernel.clone()),
            started: Notify::new(),
            finish: Notify::new(),
            answer,
        });
        let sources = BTreeMap::from([("probe".to_owned(), source.clone() as Arc<dyn Plugin>)]);
        let toolbox = crate::tool::Toolbox::default();
        let allowed = BTreeSet::new();
        assert!(
            kernel
                .gather_index(&toolbox, &allowed, sources)
                .await
                .is_empty()
        );
        let host = index_host(&kernel);
        let mut open = host.index().open(NAME);
        tokio::select! {
            () = source.started.notified() => {}
            _ = &mut open => panic!("open did not reach its checked source"),
        }
        if change_home {
            kernel.bind(
                &Processes::default(),
                &KernelWorkspace {
                    root: "/workspace".into(),
                    home: Some(HOME.into()),
                    temp: "/tmp".into(),
                },
            );
        }
        if let Some(secret) = register {
            crate::session::register_secrets([secret.to_owned()]);
            assert!(
                kernel
                    .gather_index(&toolbox, &allowed, BTreeMap::new())
                    .await
                    .is_empty()
            );
            assert!(!kernel.index_dirty());
        }
        source.finish.notify_one();
        let expected = if change_home || register.is_some() {
            stale.clone()
        } else {
            answer.map(str::to_owned).map_err(str::to_owned)
        };
        assert_eq!(open.await, expected);
    }
}

#[tokio::test]
async fn duplicate_entry_names_keep_the_first_kind_then_the_first_source_with_a_problem() {
    use wisp_plugin_api::index::{Entry, Kind};

    struct Checked(PluginHost, Arc<dyn Plugin>);
    impl Plugin for Checked {
        fn entries(&self) -> BoxFuture<'_, Vec<Entry>> {
            assert_eq!(self.0.secret_check().check("a note"), Verdict::Safe);
            self.1.entries()
        }

        fn open_entry<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<String, String>> {
            self.1.open_entry(name)
        }
    }

    let kernel = Arc::new(KernelHost::new());
    let sources: BTreeMap<_, _> = [
        (
            "aa".to_owned(),
            Arc::new(Checked(
                PluginHost::new("aa", "0.4.0", &[], kernel.clone()),
                entry_source(
                    "aa",
                    vec![
                        index_entry(Kind::Document, "same"),
                        index_entry(Kind::Skill, "shared"),
                    ],
                    false,
                ),
            )) as Arc<dyn Plugin>,
        ),
        (
            "bb".to_owned(),
            entry_source(
                "bb",
                vec![
                    index_entry(Kind::Skill, "same"),
                    index_entry(Kind::Skill, "shared"),
                ],
                false,
            ),
        ),
    ]
    .into_iter()
    .collect();
    let problems = kernel
        .gather_index(
            &crate::tool::Toolbox::default(),
            &BTreeSet::new(),
            sources.clone(),
        )
        .await;
    assert_eq!(problems.len(), 2);
    assert!(
        problems
            .iter()
            .all(|problem| problem.message.contains("same name"))
    );
    let host = index_host(&kernel);
    assert_eq!(host.index().entries().len(), 2);
    assert_eq!(host.index().open("same").await.unwrap(), "bb: same");
    assert_eq!(host.index().open("shared").await.unwrap(), "aa: shared");
    kernel.set_fallback_home(Some("/srv/wisp-new-home".into()));
    assert!(kernel.index_dirty());
    assert_eq!(host.index().entries(), [index_entry(Kind::Skill, "same")]);
    assert_eq!(host.index().open("same").await.unwrap(), "bb: same");
    assert_eq!(
        host.index().open("shared").await.unwrap_err(),
        wisp_tool_text::index::STALE_SECRET_CHECK
    );
    kernel.index_turned_off(BTreeSet::from(["shared".to_owned()]));
    assert_eq!(
        host.index().open("shared").await.unwrap_err(),
        wisp_tool_text::manage::turned_off("shared")
    );
}

#[tokio::test]
async fn index_changed_leaves_the_current_snapshot_until_the_next_gathering() {
    use wisp_plugin_api::index::Kind;
    let kernel = Arc::new(KernelHost::new());
    let entries = Arc::new(Mutex::new(vec![index_entry(Kind::Skill, "before")]));
    let source = crate::plugin::testing::index_source("aa", entries.clone(), false);
    let off = ["aa".to_owned()].into_iter().collect();
    let config = crate::plugin::PluginConfig {
        switches: crate::plugin::PluginSwitches {
            off,
            ..Default::default()
        },
        ..Default::default()
    };
    let (registry, problems) = crate::plugin::Registry::from_sources(vec![source], &config);
    assert!(problems.is_empty(), "{problems:?}");
    let started = registry.start(&BTreeSet::new(), kernel.clone()).await;
    assert!(index_host(&kernel).index().entries().is_empty());
    let on = BTreeSet::new();
    let gather = |off| registry.index_sources(&started.hooks.plugins, off);
    kernel
        .gather_index(
            &crate::tool::Toolbox::default(),
            &BTreeSet::new(),
            gather(&on),
        )
        .await;
    *locked(&entries) = vec![index_entry(Kind::Skill, "after")];
    let host = index_host(&kernel);
    host.index().changed();
    assert!(kernel.index_dirty());
    assert_eq!(host.index().entries()[0].name, "before");
    kernel
        .gather_index(
            &crate::tool::Toolbox::default(),
            &BTreeSet::new(),
            gather(&on),
        )
        .await;
    assert!(!kernel.index_dirty());
    assert_eq!(host.index().entries()[0].name, "after");
    kernel
        .gather_index(
            &crate::tool::Toolbox::default(),
            &BTreeSet::new(),
            gather(&config.switches.off),
        )
        .await;
    assert!(host.index().entries().is_empty());
    assert!(host.index().open("after").await.is_err());
    kernel
        .gather_index(
            &crate::tool::Toolbox::default(),
            &BTreeSet::new(),
            gather(&on),
        )
        .await;
    assert_eq!(host.index().open("after").await.unwrap(), "aa: after");
}

fn secret_ask() -> SecretAsk {
    SecretAsk {
        process: 1,
        name: String::new(),
        reason: String::new(),
        end_input: false,
        show_start: 0,
        show_end: 0,
    }
}

#[cfg(unix)]
#[tokio::test]
async fn the_secret_box_and_index_open_are_refused_to_an_installed_plugin() {
    use crate::plugin::{Make, PluginConfig, Registry, Source};
    for bundled in [true, false] {
        let hosts: Arc<Mutex<Vec<Arc<dyn Host>>>> = Arc::default();
        let caught = hosts.clone();
        let make: Make = Arc::new(move |host| {
            locked(&caught).push(host.clone());
            (wisp_plugin_testkit::bundled().instantiate)(host)
        });
        let manifest = wisp_plugin_testkit::MANIFEST.replace("0.2.0", "0.3.0");
        let source = if bundled {
            Source::in_process(manifest, make, Origin::Bundled)
        } else {
            Source::wasm(manifest, make, String::new())
        };
        let (registry, problems) = Registry::from_origins(vec![source], &PluginConfig::default());
        assert!(problems.is_empty(), "{problems:?}");
        let kernel = Arc::new(KernelHost::new());
        let root = std::env::temp_dir();
        kernel.bind(
            &Processes::default(),
            &KernelWorkspace {
                root: root.clone(),
                home: None,
                temp: root,
            },
        );
        registry.start(&BTreeSet::new(), kernel.clone()).await;
        let host = locked(&hosts).first().cloned().unwrap();
        open_call(&kernel, "c", "testkit");
        let ctx = FakeCall {
            id: "c".to_owned(),
            ..FakeCall::default()
        };
        let secret = if bundled {
            SecretEnd::Refused(SecretRefusal::NoProcess)
        } else {
            SecretEnd::Unavailable
        };
        assert_eq!(host.type_into(secret_ask(), &ctx).await, secret);
        let error = if bundled {
            "nothing named `missing` was found"
        } else {
            "an installed plugin can't open index entries"
        };
        assert_eq!(host.index().open("missing").await, Err(error.to_owned()));
    }
}

#[test]
fn usage_import_requires_a_bundled_capability_and_an_interactive_caller() {
    use wisp_plugin_api::usage::Usage as _;
    let kernel = Arc::new(KernelHost::new());
    kernel.publish(Snapshot {
        usage_accounts: vec![UsageAccount {
            name: "account-name-canary".into(),
            yours: true,
            off: false,
            expired: false,
            unavailable: false,
        }],
        ..Snapshot::default()
    });
    for origin in [Origin::Bundled, Origin::External] {
        for requested in [vec![], vec![Capability::Usage]] {
            for world in ["0.3.0", "0.4.0"] {
                let host = PluginHost::new("context", world, &[], kernel.clone())
                    .capabilities(&requested)
                    .origin(origin);
                assert!(host.accounts().is_empty());
                assert!(
                    Caller::rendering(Render::Start)
                        .sync_scope(|| host.accounts())
                        .is_empty()
                );
                let permitted =
                    origin == Origin::Bundled && !requested.is_empty() && world == "0.4.0";
                for caller in [
                    Caller::rendering(Render::Slot),
                    Caller::call(CancellationToken::new(), None),
                ] {
                    let rows = caller.sync_scope(|| host.accounts());
                    assert_eq!(!rows.is_empty(), permitted);
                    if permitted {
                        assert_eq!(rows[0].name, "account-name-canary");
                    }
                }
            }
        }
    }
    let mut snapshot = kernel.snapshot();
    snapshot.usage_accounts[0].name = "pastedkey01234567890123456789".into();
    kernel.publish(snapshot);
    let host = PluginHost::new("context", "0.4.0", &[], kernel)
        .origin(Origin::Bundled)
        .capabilities(&[Capability::Usage]);
    assert_eq!(
        Caller::rendering(Render::Slot).sync_scope(|| host.accounts())[0].name,
        "(not shown)"
    );
}

#[test]
fn usage_state_rules_are_ordered_and_preserve_the_kernel_stale_flag() {
    use accounts::state_of;
    use wisp_plugin_api::usage::UsageState as S;
    use wisp_protocol::plan_usage::{PlanUsage, PlanUsageError as E, UsageState};
    let base = UsageAccount {
        name: "account".into(),
        yours: false,
        off: false,
        expired: false,
        unavailable: false,
    };
    let reading = UsageState {
        snapshot: Some(PlanUsage::new(None, vec![]).unwrap()),
        loading: false,
        stale: false,
        error: None,
    };
    for (off, expired, unavailable, state, awaited, expected) in [
        (true, true, true, None, false, S::Off),
        (false, true, true, None, false, S::SignedOut),
        (
            false,
            false,
            true,
            Some(reading.clone()),
            true,
            S::Unavailable,
        ),
        (false, false, false, None, true, S::Unavailable),
        (
            false,
            false,
            false,
            Some(UsageState {
                snapshot: None,
                ..reading.clone()
            }),
            false,
            S::Unavailable,
        ),
        (
            false,
            false,
            false,
            Some(UsageState {
                snapshot: None,
                ..reading.clone()
            }),
            true,
            S::Checking,
        ),
        (
            false,
            false,
            false,
            Some(UsageState {
                snapshot: None,
                loading: true,
                ..reading.clone()
            }),
            false,
            S::Checking,
        ),
        (
            false,
            false,
            false,
            Some(UsageState {
                stale: true,
                ..reading.clone()
            }),
            false,
            S::Stale,
        ),
        (false, false, false, Some(reading.clone()), false, S::Ok),
    ] {
        let account = UsageAccount {
            off,
            expired,
            unavailable,
            ..base.clone()
        };
        assert_eq!(state_of(&account, state.as_ref(), awaited), expected);
    }
    for (error, expected) in [
        (E::Disabled, S::Off),
        (E::AuthenticationRequired, S::SignedOut),
        (E::ScopeRequired, S::SignedOut),
        (E::AccessDenied, S::Unavailable),
        (E::Unsupported, S::Unavailable),
        (E::Network, S::Stale),
        (E::Throttled { retry_at: Some(10) }, S::Stale),
    ] {
        let state = UsageState {
            error: Some(error),
            stale: true,
            ..reading.clone()
        };
        assert_eq!(state_of(&base, Some(&state), true), expected);
        assert_eq!(
            state_of(
                &base,
                Some(&UsageState {
                    snapshot: None,
                    ..state
                }),
                true
            ),
            if expected == S::Stale {
                S::Unavailable
            } else {
                expected
            }
        );
    }
}

#[tokio::test]
async fn model_asks_require_the_bundled_capability_world_and_own_running_tool_call() {
    use wisp_plugin_api::model::{AskError, Model, Prompt};
    let kernel = Arc::new(KernelHost::new());
    let ctx = open_viewed(&kernel, "q", "probe", None);
    let req = Prompt {
        system: "Answer briefly".into(),
        text: "Question".into(),
        max_tokens: 64,
        native: None,
    };
    for origin in [Origin::Bundled, Origin::External] {
        for requested in [vec![], vec![Capability::Model]] {
            for world in ["0.3.0", "0.4.0"] {
                let host = PluginHost::new("probe", world, &[], kernel.clone())
                    .origin(origin)
                    .capabilities(&requested);
                for mut caller in [
                    Caller::default(),
                    Caller::rendering(Render::Slot),
                    Caller::rendering(Render::Start),
                    Caller::call(ctx.cancel.clone(), None),
                ] {
                    caller.at.call_id = Some("q".into());
                    let is_call = caller.cancel.is_some();
                    let reply = caller.scope(host.ask(req.clone(), ctx.as_ref())).await;
                    let Err(AskError::NotAllowed(reason)) = reply else {
                        panic!("unexpected reply");
                    };
                    let permitted = origin == Origin::Bundled
                        && world == "0.4.0"
                        && !requested.is_empty()
                        && is_call;
                    assert_eq!(reason == "model requests are unavailable", permitted);
                }
            }
        }
    }
    let host = PluginHost::new("probe", "0.4.0", &[], kernel.clone())
        .origin(Origin::Bundled)
        .capabilities(&[Capability::Model]);
    let mut caller = Caller::call(ctx.cancel.clone(), None);
    caller.at.call_id = Some("q".into());
    let mut native = req.clone();
    native.native = Some("web_search".into());
    assert_eq!(
        caller.clone().scope(host.ask(native, ctx.as_ref())).await,
        Err(AskError::NotAllowed(
            "this plugin does not request web_search".into()
        ))
    );
    let stranger = FakeCall {
        id: "other".into(),
        ..FakeCall::default()
    };
    assert!(matches!(
        caller.scope(host.ask(req, &stranger)).await,
        Err(AskError::NotAllowed(_))
    ));
    assert!(ctx.take_model_asks().is_empty());
}

#[tokio::test]
async fn an_installed_plugin_cannot_start_or_finish_mcp_sign_ins() {
    let kernel = Arc::new(KernelHost::new());
    let host = PluginHost::new("mcp", "0.4.0", &[], kernel);
    let refused = "an installed plugin can't sign in to MCP servers";
    assert_eq!(host.mcp().sign_in_start("local").await, Err(refused.into()));
    assert_eq!(
        host.mcp()
            .sign_in_finish(
                "local",
                "http://127.0.0.1:8123/oauth-callback?code=CANARY-code&state=CANARY-state"
            )
            .await,
        Err(refused.into())
    );
}

fn names(list: &[&str]) -> BTreeSet<String> {
    list.iter().map(|name| (*name).to_owned()).collect()
}

fn switching() -> manage::Switching {
    manage::Switching {
        runnable: names(&["bash", "manage", "search", "show_diff", "use"]),
        natives: names(&["web_search"]),
        leaders: [("use".to_owned(), "search".to_owned())].into(),
        owners: [
            ("bash", "core"),
            ("linear", "linear"),
            ("manage", "search"),
            ("search", "search"),
            ("show_diff", "show-diff"),
            ("use", "search"),
        ]
        .map(|(tool, plugin)| (tool.to_owned(), plugin.to_owned()))
        .into(),
        finder: Some("search".to_owned()),
        listed: names(&["linear", "ponytail", "rust-code"]),
        own: names(&["rust-code"]),
        tools_off: names(&["edit_file", "stuck"]),
        stuck: names(&["stuck"]),
    }
}

fn change(
    action: wisp_plugin_api::manage::Action,
    kind: wisp_plugin_api::manage::Kind,
    name: &str,
) -> wisp_plugin_api::manage::Change {
    wisp_plugin_api::manage::Change {
        action,
        kind,
        name: name.to_owned(),
        from: None,
        keys: Vec::new(),
    }
}

#[test]
fn a_switch_is_decided_from_the_chats_facts_alone() {
    use manage::{Decided, Switch};
    use wisp_plugin_api::manage::Action;
    use wisp_plugin_api::manage::Kind::{Server, Skill, Tool};
    use wisp_tool_text::manage as text;
    let skills = names(&["machine-load"]);
    let listed = |name: &str, off, by_agent| Switch::Listed {
        name: name.to_owned(),
        off,
        by_agent,
    };
    let ask = |question: String, then, done: String, alone: String| Decided::Ask {
        question,
        then,
        done,
        alone,
    };
    let key = "abcdefghij0123456789xyz";
    let cases = [
        (
            change(Action::Off, Tool, "web_search"),
            Decided::Refused(text::native("web_search")),
        ),
        (
            change(Action::On, Tool, "web_search"),
            Decided::Refused(text::native("web_search")),
        ),
        (
            change(Action::Off, Skill, key),
            Decided::Refused("turned_off must be none or a list of names".to_owned()),
        ),
        (
            change(Action::On, Skill, key),
            Decided::Refused("turned_off must be none or a list of names".to_owned()),
        ),
        (
            change(Action::Off, Tool, "linear"),
            Decided::Unchanged(text::already("linear", false)),
        ),
        (
            change(Action::Off, Tool, "edit_file"),
            Decided::Unchanged(text::already("edit_file", false)),
        ),
        (
            change(Action::Off, Skill, "edit_file"),
            Decided::Refused(text::nothing_named("skill", "edit_file")),
        ),
        (
            change(Action::Off, Server, "stuck"),
            Decided::Refused(text::nothing_named("server", "stuck")),
        ),
        (
            change(Action::Off, Server, "edit_file"),
            Decided::Done(listed("edit_file", true, true), text::now_off("edit_file")),
        ),
        (
            change(Action::Off, Skill, "machine-load"),
            Decided::Done(
                listed("machine-load", true, true),
                text::now_off("machine-load"),
            ),
        ),
        (
            change(Action::Off, Tool, "show_diff"),
            Decided::Done(listed("show_diff", true, true), text::now_off("show_diff")),
        ),
        (
            change(Action::Off, Tool, "use"),
            ask(
                text::ask_off("search", true),
                listed("search", true, false),
                text::now_off("search"),
                text::stays("search"),
            ),
        ),
        (
            change(Action::Off, Tool, "manage"),
            ask(
                text::ask_off("manage", false),
                listed("manage", true, false),
                text::now_off("manage"),
                text::stays("manage"),
            ),
        ),
        (
            change(Action::Off, Skill, "show_diff"),
            Decided::Refused(text::nothing_named("skill", "show_diff")),
        ),
        (
            change(Action::Off, Tool, "nothing"),
            Decided::Refused(text::nothing_named("tool", "nothing")),
        ),
        (
            change(Action::On, Tool, "stuck"),
            Decided::Refused(text::would_rebuild("stuck")),
        ),
        (
            change(Action::On, Skill, "rust-code"),
            Decided::Done(listed("rust-code", false, true), text::now_on("rust-code")),
        ),
        (
            change(Action::On, Skill, "ponytail"),
            ask(
                text::ask_on("ponytail"),
                listed("ponytail", false, false),
                text::now_on("ponytail"),
                text::off_in_list("ponytail"),
            ),
        ),
        (
            change(Action::On, Tool, "edit_file"),
            ask(
                text::ask_on("edit_file"),
                Switch::Tools {
                    name: "edit_file".to_owned(),
                },
                text::now_on("edit_file"),
                text::off_in_tools("edit_file"),
            ),
        ),
        (
            change(Action::On, Tool, "use"),
            Decided::Unchanged(text::already("search", true)),
        ),
        (
            change(Action::On, Skill, "machine-load"),
            Decided::Unchanged(text::already("machine-load", true)),
        ),
        (
            change(Action::On, Server, "local"),
            Decided::Unchanged(text::already("local", true)),
        ),
        (
            change(Action::On, Skill, "nothing"),
            Decided::Refused(text::nothing_named("skill", "nothing")),
        ),
    ];
    let facts = switching();
    let servers = names(&["local", "edit_file"]);
    for (asked, decided) in cases {
        assert_eq!(
            facts.decide(&skills, &servers, &asked),
            decided,
            "{asked:?}"
        );
    }
}

#[tokio::test]
async fn manage_asks_through_its_own_call_and_counts_only_an_approval_that_beats_the_limit() {
    use crate::call::user::{AskReply, AskRequest};
    use wisp_plugin_api::manage::Kind::Tool;
    use wisp_plugin_api::manage::{Action, Manage};
    use wisp_tool_text::manage as text;
    let answered = |chosen: Vec<u32>, own: Option<&str>| AskReply::Answered {
        answers: vec![wisp_protocol::Answer {
            chosen,
            own: own.map(str::to_owned),
        }],
    };
    let card = wisp_protocol::Question {
        header: text::HEADER.to_owned(),
        question: text::ask_off("search", true),
        options: vec![
            wisp_protocol::QuestionOption {
                label: text::APPROVE.to_owned(),
                description: text::FOR_THIS_CHAT.to_owned(),
            },
            wisp_protocol::QuestionOption {
                label: text::NOT_NOW.to_owned(),
                description: String::new(),
            },
        ],
        multi_select: false,
        source: None,
    };
    let cases = [
        (
            answered(vec![0], None),
            false,
            Ok(text::now_off("search")),
            1,
        ),
        (
            answered(vec![1], None),
            false,
            Ok(text::CHOSE_NOT_NOW.to_owned()),
            0,
        ),
        (
            answered(Vec::new(), Some(" keep\n it ")),
            false,
            Ok(text::own_words("keep it")),
            0,
        ),
        (
            AskReply::MessageInstead,
            false,
            Ok(text::NO_ANSWER.to_owned()),
            0,
        ),
        (AskReply::Unavailable, false, Err(text::stays("search")), 0),
        (
            answered(vec![0], None),
            true,
            Ok(text::NO_ANSWER.to_owned()),
            0,
        ),
    ];
    for (reply, late, result, switched) in cases {
        let kernel = Arc::new(KernelHost::new());
        kernel.set_switching(Some(switching()));
        let (asks, mut requests) = tokio::sync::mpsc::channel(1);
        open_with(
            &kernel,
            "toolu_manage",
            manage::SEARCH_PLUGIN,
            None,
            UserChannel::new(asks),
        );
        let ctx = FakeCall {
            id: "toolu_manage".to_owned(),
            ..FakeCall::default()
        };
        let limit = ctx.cancel.clone();
        let asked = tokio::spawn(async move {
            let Some(AskRequest::Questions {
                questions,
                reply: answer,
            }) = requests.recv().await
            else {
                return Vec::new();
            };
            if late {
                limit.set();
            }
            let _ = answer.send(reply);
            questions
        });
        let host = PluginHost::new(manage::SEARCH_PLUGIN, "0.4.0", &[], kernel.clone());
        let got = Manage::manage(&host, change(Action::Off, Tool, "use"), &ctx).await;
        assert_eq!((got, late), (result, late));
        assert_eq!(asked.await.unwrap(), vec![card.clone()]);
        assert_eq!(kernel.take_switches().len(), switched);
    }
    let kernel = Arc::new(KernelHost::new());
    open_call(&kernel, "toolu_manage", manage::SEARCH_PLUGIN);
    let ctx = FakeCall {
        id: "toolu_manage".to_owned(),
        ..FakeCall::default()
    };
    let off = || change(Action::Off, Tool, "show_diff");
    let search = PluginHost::new(manage::SEARCH_PLUGIN, "0.4.0", &[], kernel.clone());
    assert_eq!(
        Manage::manage(&search, off(), &ctx).await,
        Err(text::NOT_MAIN.to_owned())
    );
    kernel.set_switching(Some(switching()));
    assert_eq!(
        Manage::manage(&search, off(), &ctx).await,
        Ok(text::now_off("show_diff"))
    );
    assert_eq!(
        Manage::manage(&search, off(), &ctx).await,
        Ok(text::already("show_diff", false))
    );
    assert_eq!(
        kernel.take_switches(),
        vec![manage::Switch::Listed {
            name: "show_diff".to_owned(),
            off: true,
            by_agent: true,
        }]
    );
    let probe = PluginHost::new("probe", "0.4.0", &[], kernel.clone());
    assert_eq!(
        Manage::manage(&probe, off(), &ctx).await,
        Err("the manage interface belongs to the bundled search plugin, not probe".to_owned())
    );
    let older = PluginHost::new(manage::SEARCH_PLUGIN, "0.3.0", &[], kernel.clone());
    assert!(Manage::manage(&older, off(), &ctx).await.is_err());
    assert!(kernel.take_switches().is_empty());
}

#[tokio::test]
async fn repeated_switches_of_a_tool_or_skill_in_one_turn_build_on_the_previous_switch() {
    use wisp_plugin_api::index::{Entry, Kind as EntryKind};
    use wisp_plugin_api::manage::{Action, Kind, Manage};
    use wisp_tool_text::manage as text;
    for (kind, name) in [(Kind::Tool, "show_diff"), (Kind::Skill, "machine-load")] {
        let kernel = Arc::new(KernelHost::new());
        kernel.set_switching(Some(switching()));
        locked(&kernel.index).entries.push(Entry {
            kind: EntryKind::Skill,
            name: "machine-load".to_owned(),
            summary: String::new(),
            text: String::new(),
        });
        open_call(&kernel, "toolu_manage", manage::SEARCH_PLUGIN);
        let ctx = FakeCall {
            id: "toolu_manage".to_owned(),
            ..FakeCall::default()
        };
        let host = PluginHost::new(manage::SEARCH_PLUGIN, "0.4.0", &[], kernel.clone());
        for action in [Action::Off, Action::On, Action::Off] {
            let expected = match action {
                Action::On => text::now_on(name),
                Action::Off => text::now_off(name),
                Action::Add | Action::Remove => unreachable!("this table switches only"),
            };
            assert_eq!(
                Manage::manage(&host, change(action, kind, name), &ctx).await,
                Ok(expected),
                "{kind:?} {action:?}"
            );
        }
        assert_eq!(kernel.take_switches().len(), 3);
    }
}

#[test]
fn the_plugin_key_detector_tracks_protocol_prefixes_and_boundary_cases() {
    assert_eq!(
        *wisp_protocol::KEY_PREFIXES,
        *wisp_tool_text::manage::KEY_PREFIXES
    );
    let mut cases: Vec<String> = [
        "1ABC",
        "",
        "A-B",
        "abcdefghij012345678",
        "abcdefghij0123456789",
        "abcdefghij012345678_",
        "abcdefghijklmnopqrst",
        "ABCDEFGHIJ0123456789",
        "AIzaCANARY_KEY_WITH_UNDERSCORE",
    ]
    .map(str::to_owned)
    .into();
    for prefix in &*wisp_protocol::KEY_PREFIXES {
        for count in [0, 1, 19, 20, 64] {
            cases.push(format!("{prefix}CANARY{}", "_".repeat(count)));
        }
    }
    for prefix in ["ghp_", "github_pat_", "sk_live_", "lin_api_", "ntn_"] {
        cases.push(format!("{prefix}CANARYa1{}", "x".repeat(40)));
    }
    for name in cases {
        assert_eq!(
            wisp_protocol::plain_variable(&name),
            wisp_tool_text::manage::plain_variable(&name),
            "variable detector disagrees"
        );
        assert_eq!(
            wisp_protocol::looks_like_key(&name),
            wisp_tool_text::manage::looks_like_key(&name),
            "detector disagrees"
        );
    }
}

#[tokio::test]
async fn a_child_without_shell_cannot_add_or_remove_and_never_asks() {
    use wisp_plugin_api::manage::{Action, Kind, Manage};
    let kernel = Arc::new(KernelHost::default());
    let service = crate::mcp::Service::new(
        BTreeMap::from([(
            "local".into(),
            crate::config::McpServer {
                keys: Vec::new(),
                command: "placeholder".into(),
                args: vec![],
            },
        )]),
        std::env::temp_dir(),
        None,
    );
    kernel.set_mcp(service.clone());
    kernel.set_managing(manage::Managing {
        shell: false,
        child: true,
        path: "/review/coder".into(),
        window: false,
    });
    open_call(&kernel, "toolu_manage", manage::SEARCH_PLUGIN);
    let plugin = PluginHost::new(manage::SEARCH_PLUGIN, "0.4.0", &[], kernel.clone())
        .origin(Origin::Bundled);
    let ctx = FakeCall {
        id: "toolu_manage".into(),
        ..FakeCall::default()
    };
    for (action, verb) in [(Action::Add, "add"), (Action::Remove, "remove")] {
        let mut request = change(action, Kind::Server, "local");
        if action == Action::Add {
            request.name = "other".into();
            request.from = Some("placeholder".into());
        }
        let expected = wisp_tool_text::manage::no_shell(verb, &request.name);
        assert_eq!(Manage::manage(&plugin, request, &ctx).await, Err(expected));
    }
    let key = format!("sk-ant-api03-CANARY{}", "x".repeat(64));
    for action in [Action::Add, Action::Remove, Action::On, Action::Off] {
        for kind in [Kind::Tool, Kind::Skill, Kind::Server] {
            assert_eq!(
                Manage::manage(&plugin, change(action, kind, &key), &ctx).await,
                Err(wisp_tool_text::manage::KEY_NAME.into())
            );
        }
    }
    assert!(service.take_notes().is_empty());
    assert_eq!(service.rows().len(), 1);
}

#[tokio::test]
async fn a_busy_or_stopped_secret_channel_never_claims_there_is_no_client() {
    use crate::call::user::{AskRequest, SecretReply};
    use wisp_plugin_api::manage::{Action, Kind, Manage};
    for stopped in [false, true] {
        let kernel = Arc::new(KernelHost::new());
        kernel.set_mcp(crate::mcp::Service::new(
            BTreeMap::new(),
            std::env::temp_dir(),
            None,
        ));
        kernel.set_managing(manage::Managing {
            shell: true,
            ..manage::Managing::default()
        });
        let (asks, mut requests) = tokio::sync::mpsc::channel(1);
        open_with(
            &kernel,
            "key",
            manage::SEARCH_PLUGIN,
            None,
            UserChannel::new(asks),
        );
        let ctx = FakeCall {
            id: "key".into(),
            ..FakeCall::default()
        };
        let cancel = ctx.cancel.clone();
        let asked = tokio::spawn(async move {
            let Some(AskRequest::Secret { reply, .. }) = requests.recv().await else {
                return;
            };
            if stopped {
                cancel.set();
            }
            let _ = reply.send(if stopped {
                SecretReply::Unavailable
            } else {
                SecretReply::Busy
            });
        });
        let host =
            PluginHost::new(manage::SEARCH_PLUGIN, "0.4.0", &[], kernel).origin(Origin::Bundled);
        let mut request = change(Action::Add, Kind::Server, "local");
        request.from = Some("placeholder".into());
        request.keys = vec!["WISP_TEST_TOKEN".into()];
        assert_eq!(
            Manage::manage(&host, request, &ctx).await,
            Ok(wisp_tool_text::manage::NO_ANSWER.into())
        );
        asked.await.unwrap();
    }
}

async fn form_host(
    forms: bool,
    user: UserChannel,
    child: bool,
) -> (PluginHost, FakeCall, Arc<Mutex<Vec<serde_json::Value>>>) {
    use crate::test_guard::http;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    let address = http::gateway(move |request| {
        let seen = seen.clone();
        async move {
            let request: serde_json::Value = serde_json::from_str(http::body(&request)).unwrap();
            seen.lock().unwrap().push(request.clone());
            let result = match request["method"].as_str().unwrap_or_default() {
                "server/discover" => serde_json::json!({"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}},"ttlMs":0,"cacheScope":"private"}),
                "tools/list" => serde_json::json!({"resultType":"complete","tools":[{"name":"echo","inputSchema":{"type":"object"}}],"ttlMs":0,"cacheScope":"private"}),
                "tools/call" if request["params"]["inputResponses"].is_null() => serde_json::json!({"resultType":"input_required","inputRequests":{"form":{"method":"elicitation/create","params":{"mode":"form","message":"Placeholder question","requestedSchema":{"type":"object","properties":{"text":{"type":"string"}}}}}}}),
                _ => serde_json::json!({"content":[{"type":"text","text":"done"}]}),
            };
            http::Reply::Send(http::json("200 OK", &serde_json::json!({"jsonrpc":"2.0","id":request["id"],"result":result}).to_string()))
        }
    }).await;
    let kernel = Arc::new(KernelHost::new());
    kernel.set_mcp(
        crate::mcp::Service::new(
            BTreeMap::from([(
                "linear".into(),
                crate::config::McpServer {
                    command: address,
                    args: Vec::new(),
                    keys: Vec::new(),
                },
            )]),
            std::env::temp_dir(),
            None,
        )
        .with_forms(forms),
    );
    kernel.set_managing(manage::Managing {
        child,
        ..manage::Managing::default()
    });
    kernel.mcp().connect("linear").await.unwrap();
    open_with(&kernel, "mcp-call", "mcp", None, user);
    (
        PluginHost::new("mcp", "0.4.0", &[], kernel).origin(Origin::Bundled),
        FakeCall {
            id: "mcp-call".into(),
            ..FakeCall::default()
        },
        requests,
    )
}

#[tokio::test]
async fn child_form_is_refused() {
    use wisp_plugin_api::mcp::Mcp;
    let (tx, mut rx) = tokio::sync::mpsc::channel(2);
    let (host, ctx, _) = form_host(true, UserChannel::new(tx), true).await;
    assert_eq!(Mcp::call(&host, &ctx, "mcp__linear__echo", "{}", 120_000).await, Err("linear asked a question that only the main chat can show; ask your parent to call mcp__linear__echo with the same input".into()));
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn headless_forms_are_not_advertised() {
    use wisp_plugin_api::mcp::Mcp;
    let (host, ctx, requests) = form_host(false, UserChannel::none(), false).await;
    assert_eq!(
        Mcp::call(&host, &ctx, "mcp__linear__echo", "{}", 120_000).await,
        Err(wisp_tool_text::mcp::INPUT_REQUIRED.into())
    );
    let requests = requests.lock().unwrap();
    assert!(
        requests[0]["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"]
            .get("elicitation")
            .is_none()
    );
}

#[tokio::test]
async fn asking_does_not_spend_server_time() {
    use crate::call::user::{AskReply, AskRequest};
    use wisp_plugin_api::mcp::Mcp;
    let (tx, mut rx) = tokio::sync::mpsc::channel(2);
    let (host, ctx, _) = form_host(true, UserChannel::new(tx), false).await;
    let answer = tokio::spawn(async move {
        let AskRequest::Questions { reply, .. } = rx.recv().await.unwrap() else {
            panic!("expected card")
        };
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(121)).await;
        tokio::time::resume();
        reply
            .send(AskReply::Answered {
                answers: vec![wisp_protocol::Answer::own("text")],
            })
            .unwrap();
    });
    let result = Mcp::call(&host, &ctx, "mcp__linear__echo", "{}", 120_000).await;
    answer.await.unwrap();
    assert_eq!(result.unwrap().result, Some("done".into()));
}

#[tokio::test]
async fn an_unattended_server_card_reaches_the_kernel_deadline() {
    use crate::agent::testing::{B, Fake, done, step};
    use crate::agent::{Agent, default_session};
    use crate::plugin::{PluginConfig, Registry};
    use wisp_protocol::{AgentEvent, StopReason, ToolOutcome};
    for mode in ["ask", "ask-legacy"] {
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(folder.path().join("form.json"), serde_json::json!({"mode":"form","message":"Placeholder question","requestedSchema":{"type":"object","properties":{"text":{"type":"string"}}}}).to_string()).unwrap();
        let kernel = Arc::new(KernelHost::new());
        kernel.set_mcp(
            crate::mcp::Service::new(
                BTreeMap::from([(
                    "linear".into(),
                    crate::config::McpServer {
                        command: crate::test_guard::mcp_server(),
                        args: vec![mode.into()],
                        keys: Vec::new(),
                    },
                )]),
                folder.path().into(),
                None,
            )
            .with_forms(true),
        );
        kernel.mcp().connect("linear").await.unwrap();
        let (registry, problems) = Registry::new(
            vec![wisp_plugin_mcp::bundled(), wisp_plugin_search::bundled()],
            &PluginConfig::default(),
        );
        assert!(problems.is_empty());
        let started = registry
            .start(&BTreeSet::from(["search".into()]), kernel)
            .await;
        assert!(started.problems.is_empty());
        let name = "mcp__linear__z_echo";
        let entry = started
            .toolbox
            .get(name)
            .or_else(|| started.toolbox.held_tool(name))
            .unwrap()
            .clone();
        let provider = Fake::new([
            step(
                &[B::Call("server-call", name, serde_json::json!({}))],
                StopReason::ToolUse,
            ),
            done(&[B::Text("Done.")]),
        ]);
        let workspace = crate::workspace::Workspace {
            root: folder.path().into(),
            home: None,
            temp: folder.path().into(),
        };
        let mut agent = Agent::new(provider, default_session())
            .with_tools(crate::tool::Toolbox::new([entry]), workspace)
            .with_user_input(true);
        let (_commands, mut commands) = tokio::sync::mpsc::channel(2);
        let (events, mut received) = tokio::sync::mpsc::channel(256);
        let run =
            tokio::spawn(async move { agent.run_turn("Go.".into(), &mut commands, &events).await });
        while !matches!(
            received.recv().await.unwrap(),
            AgentEvent::QuestionAsked { .. }
        ) {}
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(601)).await;
        tokio::task::yield_now().await;
        assert!(
            !run.is_finished(),
            "the card ended before the call deadline"
        );
        tokio::time::advance(Duration::from_secs(190)).await;
        tokio::time::resume();
        run.await.unwrap();
        let mut outcome = None;
        while let Some(event) = received.recv().await {
            if let AgentEvent::ToolCallFinished {
                call_id,
                outcome: done,
                ..
            } = event
                && call_id == "server-call"
            {
                outcome = Some(done);
            }
        }
        assert_eq!(outcome, Some(ToolOutcome::TimedOut { after_ms: 780_000 }));
    }
}

#[test]
fn chat_names_are_cleaned_and_only_accepted_in_a_saved_main_agents_call_or_command() {
    use wisp_tool_text::chat::{EMPTY, NOT_MAIN, NOT_SAVED, OUTSIDE};
    let call = || Caller::call(CancellationToken::new(), None);
    let hooked = call().hook(1, super::super::caller::At::default());
    let slot = Caller::rendering(Render::Slot);
    let start = Caller::rendering(Render::Start);
    let masked = format!("sk-{}", "CANARY".repeat(12));
    let (long, kept) = ("a".repeat(300), "a".repeat(200));
    let unavailable = wisp_plugin_api::chat::UNAVAILABLE;
    let cases = [
        ("0.4.0", hooked, None, "Strada", Err(OUTSIDE)),
        ("0.4.0", slot, None, "Strada", Err(OUTSIDE)),
        ("0.4.0", start, None, "Strada", Err(OUTSIDE)),
        ("0.4.0", Caller::default(), None, "Strada", Err(OUTSIDE)),
        ("0.4.0", call(), Some(NOT_MAIN), "Strada", Err(NOT_MAIN)),
        ("0.4.0", call(), Some(NOT_SAVED), "Strada", Err(NOT_SAVED)),
        ("0.4.0", call(), None, " ", Err(EMPTY)),
        ("0.3.0", call(), None, "Strada", Err(unavailable)),
        ("0.4.0", call(), None, masked.as_str(), Ok("sk-…")),
        ("0.4.0", call(), None, long.as_str(), Ok(kept.as_str())),
    ];
    for (world, caller, no_rename, name, expected) in cases {
        let kernel = Arc::new(KernelHost::new());
        kernel.publish(Snapshot {
            no_rename,
            ..Snapshot::default()
        });
        let host = PluginHost::new("chat", world, &[], kernel.clone());
        let result = caller.sync_scope(|| host.chat().rename(name.to_owned()));
        assert_eq!(
            result.as_deref().map_err(String::as_str),
            expected,
            "{world}"
        );
        assert_eq!(
            kernel.take_renames(),
            expected.into_iter().map(str::to_owned).collect::<Vec<_>>()
        );
        assert!(kernel.take_renames().is_empty());
    }
}

#[tokio::test]
async fn questions_interface_needs_world_0_4_0_and_a_running_call() {
    use wisp_plugin_api::questions::Posted;
    use wisp_plugin_api::types::{Question, QuestionOption};
    let item = || {
        vec![Question {
            header: "H".into(),
            question: "Q?".into(),
            options: vec![
                QuestionOption {
                    label: "A".into(),
                    description: String::new(),
                },
                QuestionOption {
                    label: "B".into(),
                    description: String::new(),
                },
            ],
            multi_select: false,
        }]
    };
    let kernel = Arc::new(KernelHost::new());
    let old = PluginHost::new("ask-user", "0.3.0", &[], kernel.clone());
    let current = PluginHost::new("ask-user", "0.4.0", &[], kernel.clone());
    let ctx = wisp_plugin_testkit::host::FakeCall {
        id: "call-q".into(),
        ..Default::default()
    };
    assert_eq!(
        old.questions().post(item(), &ctx).await,
        Posted::Unavailable
    );
    assert!(!old.questions().withdraw(&ctx).await);
    assert_eq!(
        current.questions().post(item(), &ctx).await,
        Posted::Unavailable
    );
    assert!(!current.questions().withdraw(&ctx).await);
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let _call = open_with(&kernel, "call-q", "ask-user", None, UserChannel::new(tx));
    for render in [Render::Slot, Render::Start] {
        let got = Caller::rendering(render)
            .scope(async {
                (
                    current.questions().post(item(), &ctx).await,
                    current.questions().withdraw(&ctx).await,
                )
            })
            .await;
        assert_eq!(got, (Posted::Unavailable, false));
    }
    let server = tokio::spawn(async move {
        let Some(crate::call::user::AskRequest::Post { reply, .. }) = rx.recv().await else {
            panic!("expected Post");
        };
        let _ = reply.send(Posted::Open);
        let Some(crate::call::user::AskRequest::Withdraw { reply }) = rx.recv().await else {
            panic!("expected Withdraw");
        };
        let _ = reply.send(true);
    });
    assert_eq!(current.questions().post(item(), &ctx).await, Posted::Open);
    assert!(current.questions().withdraw(&ctx).await);
    server.await.unwrap();
    kernel.close_call("call-q");
    assert_eq!(
        current.questions().post(item(), &ctx).await,
        Posted::Unavailable
    );
    assert!(!current.questions().withdraw(&ctx).await);
}
