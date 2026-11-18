use std::sync::Mutex as StdMutex;

use futures::FutureExt;

use super::*;
use crate::kit::BuildTools;
use crate::plugin::{KernelHost, Make, PluginConfig, Registry, Tools};

fn kit_agent(provider: Arc<Fake>, env: &Env, on: &[&str]) -> Agent {
    Agent::new(provider, default_session())
        .with_clock(TickingClock::new())
        .with_tools(crate::tool::testkit::toolbox(on), env.workspace())
        .with_platform("Linux")
        .with_resources(None)
        .with_cancel_grace(Duration::from_millis(300))
}

fn failed_message(events: &[AgentEvent], id: &str) -> Vec<String> {
    finished(events)
        .into_iter()
        .filter(|(call, _)| call == id)
        .map(|(_, outcome)| match outcome {
            ToolOutcome::Failed { message, .. } => message,
            other => format!("{other:?}"),
        })
        .collect()
}

#[tokio::test]
async fn a_panicking_plugin_tool_fails_once_naming_its_plugin_and_the_session_goes_on() {
    let env = Env::new("plugin-panic");
    let provider = scripted(vec![
        uses(&[call("p", "panic", json!({}))]),
        done("one"),
        uses(&[call("e", "echo", json!({"text": "still here\n"}))]),
        done("two"),
    ]);
    let mut agent = kit_agent(provider.clone(), &env, &["echo", "panic"]);
    let (outcome, events) = turn(&mut agent, "first").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{events:#?}"
    );
    assert_eq!(
        failed_message(&events, "p"),
        ["the testkit plugin panicked"]
    );
    let (outcome, events) = turn(&mut agent, "second").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{events:#?}"
    );
    assert_eq!(
        finished(&events),
        [(
            "e".to_owned(),
            ToolOutcome::Completed {
                is_error: false,
                exit: None
            }
        )]
    );
    let history = agent.history();
    let (_, text, is_error) = results(&history[2])[0];
    assert!(is_error);
    assert!(text.contains("the testkit plugin panicked"), "{text}");
}

static SEEN: StdMutex<Vec<bool>> = StdMutex::new(Vec::new());

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_panic_after_an_await_on_another_worker_is_contained_and_the_flag_clears() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {
        SEEN.lock().unwrap().push(wisp_contain::is_contained());
    }));
    let env = Env::new("plugin-panic-mt");
    let provider = scripted(vec![
        uses(&[call("p", "panic", json!({"ms": 30}))]),
        done("one"),
    ]);
    let agent = kit_agent(provider, &env, &["panic"]);
    let (agent, events) = tokio::spawn(async move {
        let mut agent = agent;
        let (_, events) = turn(&mut agent, "go").await;
        (agent, events)
    })
    .await
    .unwrap();
    drop(agent);
    let flags: Vec<bool> = futures::future::join_all(
        (0..64).map(|_| tokio::spawn(async { wisp_contain::is_contained() })),
    )
    .await
    .into_iter()
    .map(Result::unwrap)
    .collect();
    std::panic::set_hook(previous);
    assert_eq!(
        failed_message(&events, "p"),
        ["the testkit plugin panicked"]
    );
    assert_eq!(*SEEN.lock().unwrap(), [true]);
    assert!(flags.iter().all(|f| !f), "{flags:?}");
    assert!(!wisp_contain::is_contained());
}

fn sources(reversed: bool) -> Vec<(String, Make)> {
    let mut all: Vec<(String, Make)> = wisp_plugins::bundled()
        .into_iter()
        .chain([wisp_plugin_testkit::bundled()])
        .map(|b| {
            let make: Make = Arc::new(move |host| (b.instantiate)(host));
            (b.manifest.to_owned(), make)
        })
        .collect();
    if reversed {
        all.reverse();
    } else {
        all.rotate_left(1);
    }
    all
}

fn shuffled_tools(reversed: bool) -> Tools {
    let (registry, problems) = Registry::from_sources(sources(reversed), &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let on = ["ask_user", "echo", "show_diff", "sleep", "view_file"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    Tools::new(Arc::new(registry), on)
}

#[tokio::test]
async fn a_shuffled_bundle_sends_the_same_tools_and_system_prompt_on_every_request() {
    let mut bodies = Vec::new();
    let mut catalogs = Vec::new();
    for reversed in [false, true] {
        let tools = shuffled_tools(reversed);
        let started = tools
            .start(crate::kit::ROOT, Arc::new(KernelHost::new()))
            .now_or_never()
            .unwrap();
        catalogs.push(tools.registry().catalog(&tools.on, &started.problems));
        let toolbox = started.toolbox;
        let env = Env::new("plugin-shuffle");
        let provider = scripted(vec![
            uses(&[call("a", "sleep", json!({"ms": 0, "text": "slept\n"}))]),
            done("one"),
        ]);
        let mut agent = Agent::new(provider.clone(), default_session())
            .with_clock(TickingClock::new())
            .with_tools(toolbox, env.workspace())
            .with_platform("Linux")
            .with_resources(None);
        turn(&mut agent, "go").await;
        bodies.push(provider.texts());
    }
    assert_eq!(catalogs[0], catalogs[1]);
    let parts = |body: &str| {
        (
            tools_part(body).to_owned(),
            head_and_messages(body).0.to_owned(),
        )
    };
    let all: Vec<(String, String)> = bodies.iter().flatten().map(|b| parts(b)).collect();
    assert_eq!(all.len(), 4);
    assert!(all.windows(2).all(|pair| pair[0] == pair[1]), "{all:#?}");
}

#[tokio::test]
async fn the_catalog_is_announced_once_when_the_session_starts_and_never_logged() {
    let env = Env::new("catalog-once");
    let store = MemoryStore::default();
    let mut agent = kit_agent(scripted(vec![done("one"), done("two")]), &env, &["bash"])
        .with_catalog(crate::tool::testkit::catalog())
        .with_log(
            SessionLog::new(Box::new(store.clone()), "s1"),
            log_context(),
        );
    let (_, first) = turn(&mut agent, "hello").await;
    let (_, second) = turn(&mut agent, "again").await;
    let announced = |events: &[AgentEvent]| pick!(&events, AgentEvent::CatalogChanged { catalog } => catalog.clone());
    assert_eq!(announced(&first), [agent.catalog().clone()]);
    assert!(announced(&second).is_empty(), "{second:?}");
    let records = records(&store);
    assert!(!format!("{records:?}").contains("catalog"), "{records:?}");

    let mut bare = kit_agent(scripted(vec![done("one")]), &env, &["bash"]);
    let (_, events) = turn(&mut bare, "hello").await;
    assert!(announced(&events).is_empty(), "{events:?}");
}
