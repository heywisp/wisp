use super::*;
use crate::test_guard::{alive, gone, pid_in, until};

fn bash_agent(provider: Arc<Fake>, env: &Env, grace: Duration) -> Agent {
    let toolbox = crate::tool::Toolbox::new([crate::tool::testkit::bash_entry()]);
    agent_with(provider, env, toolbox, grace)
}

fn on_output(e: &AgentEvent) -> bool {
    matches!(e, AgentEvent::ToolOutput { .. })
}

fn kinds(events: &[AgentEvent]) -> Vec<String> {
    let mut kinds: Vec<String> = Vec::new();
    for event in events
        .iter()
        .filter(|e| !matches!(e, AgentEvent::HookRan { .. }))
    {
        let kind = serde_json::to_value(event).unwrap()["type"]
            .as_str()
            .unwrap()
            .to_owned();
        if kind == "tool_output" && kinds.last().is_some_and(|k| k == "tool_output*") {
            continue;
        }
        kinds.push(if kind == "tool_output" {
            "tool_output*".to_owned()
        } else {
            kind
        });
    }
    kinds
}

fn record(name: &str, agent: &Agent, env: &Env, events: &[AgentEvent]) -> String {
    let streamed = streamed(events);
    let finished = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolCallFinished { .. } => {
                let mut value = serde_json::to_value(e).unwrap();
                value["duration_ms"] = json!(0);
                Some(serde_json::to_string_pretty(&value).unwrap())
            }
            _ => None,
        })
        .unwrap();
    let history = agent.history();
    let (_, text, is_error) = results(&history[2])[0];
    let body = format!(
        "== {name}\nevents: {}\nstreamed: {}\nfinished: {finished}\nis_error: {is_error}\nresult:\n{text}\n",
        kinds(events).join(" "),
        serde_json::to_string(&streamed).unwrap(),
    );
    body.replace(&env.root.display().to_string(), "<root>")
        .replace(&agent.session_id, "<session>")
}

async fn scenario(name: &str, input: Value, cancel: Option<(Trigger, Duration)>) -> String {
    let env = Env::new(&format!("golden-{name}"));
    fs::create_dir_all(env.work.join("sub")).unwrap();
    let provider = scripted(vec![uses(&[call("a", "bash", input)]), done("ok")]);
    let grace = cancel.map_or(CANCEL_GRACE, |(_, grace)| grace);
    let mut agent = bash_agent(provider, &env, grace);
    let (_, events) = turn_with(&mut agent, "go", cancel.map(|(when, _)| when)).await;
    record(name, &agent, &env, &events)
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bash/results.txt")
}

#[tokio::test]
async fn bash_results_and_events_match_the_golden_capture() {
    if crate::jobs::exec::find_program("bash").is_none() {
        eprintln!("skipped: bash is not on PATH");
        return;
    }
    let cases = vec![
        ("echo", json!({"command": "echo hi"}), None),
        ("silent", json!({"command": "true"}), None),
        (
            "exit-code",
            json!({"command": "echo out; echo err >&2; exit 3"}),
            None,
        ),
        ("no-newline", json!({"command": "printf x"}), None),
        (
            "partial-utf8",
            json!({"command": r"printf 'a\342\202'"}),
            None,
        ),
        (
            "progress",
            json!({"command": r"printf '10%%\r50%%\r100%%\n\033[31mred\033[0m\n'"}),
            None,
        ),
        ("large", json!({"command": "seq 1 5000"}), None),
        (
            "long-line",
            json!({"command": "head -c 40000 /dev/zero | tr '\\0' 'y'"}),
            None,
        ),
        ("cd", json!({"command": "cd sub && echo moved"}), None),
        (
            "clamped",
            json!({"command": "echo hi", "timeout_ms": 700_000}),
            None,
        ),
        (
            "timed-out",
            json!({"command": "echo start; sleep 30", "timeout_ms": 3000}),
            None,
        ),
        ("signal", json!({"command": "echo bye; kill -9 $$"}), None),
        (
            "cancelled",
            json!({"command": "echo start; sleep 30"}),
            Some((on_output as Trigger, CANCEL_GRACE)),
        ),
        (
            "abandoned",
            json!({"command": "trap '' TERM; echo start; sleep 30"}),
            Some((on_output as Trigger, GRACE)),
        ),
    ];
    let mut all = String::new();
    for (name, input, trigger) in cases {
        all.push_str(&scenario(name, input, trigger).await);
    }
    let path = golden_path();
    if std::env::var_os("WISP_BLESS").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &all).unwrap();
    }
    assert_eq!(fs::read_to_string(&path).unwrap(), all);
}

#[tokio::test]
async fn an_abandoned_bash_call_still_stops_its_command_and_leaves_no_job() {
    let env = Env::new("abandon-kill");
    let provider = scripted(vec![uses(&[call(
        "a",
        "bash",
        json!({"command": "trap '' TERM; sleep 31 & echo $! > pid.txt; echo start; wait"}),
    )])]);
    let mut agent = bash_agent(provider, &env, GRACE);
    let (_, events) = turn_with(&mut agent, "go", Some(on_output)).await;
    let pid = pid_in(&env.work.join("pid.txt")).await;
    let history = agent.history();
    let (_, text, _) = results(&history[2])[0];
    assert!(text.ends_with(ABANDONED_NOTE), "{text}");
    assert!(gone(pid).await, "the abandoned call left {pid} running");
    assert!(matches!(
        agent.processes.end_call("a", false),
        CallEnd::NoJob
    ));
    let finished = events
        .iter()
        .position(|e| matches!(e, AgentEvent::ToolCallFinished { .. }))
        .unwrap();
    assert!(
        events[finished..]
            .iter()
            .all(|e| !matches!(e, AgentEvent::ToolOutput { .. }))
    );
}

#[tokio::test]
async fn a_job_that_outlives_its_call_stops_its_events_before_the_call_finishes() {
    let env = Env::new("outlives");
    let provider = scripted(vec![
        uses(&[call(
            "a",
            "starter",
            json!({"command": "echo $$ > pid.txt; echo early; sleep 2; echo late; exec sleep 30"}),
        )]),
        done("ok"),
    ]);
    let toolbox = crate::tool::testkit::toolbox(&["starter"]);
    let mut agent = agent_with(provider, &env, toolbox, GRACE);
    let (_, events) = turn(&mut agent, "go").await;
    let pid = pid_in(&env.work.join("pid.txt")).await;
    assert_eq!(streamed(&events), "early\n");
    let (position, output) = events
        .iter()
        .enumerate()
        .find_map(|(i, e)| match e {
            AgentEvent::ToolCallFinished { output, .. } => Some((i, output.clone())),
            _ => None,
        })
        .unwrap();
    assert!(
        events[position..]
            .iter()
            .all(|e| !matches!(e, AgentEvent::ToolOutput { .. }))
    );
    assert_eq!(output.bytes, 6);
    assert_eq!(output.tail, "early\n");
    let file = PathBuf::from(output.file.unwrap());
    assert!(
        until(|| fs::read_to_string(&file).unwrap_or_default() == "early\nlate\n").await,
        "late never arrived"
    );
    assert!(alive(pid));
    assert!(matches!(
        agent.processes.end_call("a", false),
        CallEnd::Outlives(..)
    ));
    drop(agent);
    assert!(gone(pid).await, "dropping the agent left {pid} running");
}
