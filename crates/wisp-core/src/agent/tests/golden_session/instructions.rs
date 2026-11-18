use super::*;

const DEFAULT: &str = "Prefer British spelling.\n";

fn in_sub() -> Setup {
    Setup {
        tools: Tools::Default,
        start: Some("sub"),
        instructions: true,
        ..Setup::default()
    }
}

#[tokio::test]
async fn a_project_file_with_an_import_from_a_subdirectory() {
    let script = vec![
        calls(
            "msg_01",
            400,
            vec![call("toolu_01", "bash", json!({"command": "echo hi"}))],
        ),
        says("msg_02", 520, "Done."),
    ];
    let rig = Rig::new("instructions_project", in_sub(), script).await;
    rig.project();
    let mut live = rig.start();
    live.completed("u1", "Look around.").await;
    live.close().await;
    rig.check();
}

#[tokio::test]
async fn the_users_default_file_only() {
    let script = vec![says("msg_01", 400, "Hello.")];
    let rig = Rig::new("instructions_default", in_sub(), script).await;
    rig.repository();
    fs::create_dir_all(rig.env.root.join("config")).unwrap();
    fs::write(rig.env.root.join("config").join("AGENTS.md"), DEFAULT).unwrap();
    let mut live = rig.start();
    live.completed("u1", "hello").await;
    live.close().await;
    rig.check();
}

#[tokio::test]
async fn an_empty_project_file() {
    let script = vec![says("msg_01", 400, "Hello.")];
    let rig = Rig::new("instructions_empty", in_sub(), script).await;
    rig.repository();
    fs::write(rig.env.work().join("AGENTS.md"), "").unwrap();
    fs::create_dir_all(rig.env.root.join("config")).unwrap();
    fs::write(rig.env.root.join("config").join("AGENTS.md"), DEFAULT).unwrap();
    let mut live = rig.start();
    live.completed("u1", "hello").await;
    live.close().await;
    rig.check();
}

#[tokio::test]
async fn a_project_file_at_the_repository_root() {
    let setup = Setup {
        start: None,
        ..in_sub()
    };
    let script = vec![says("msg_01", 400, "Hello.")];
    let rig = Rig::new("instructions_root", setup, script).await;
    rig.project();
    let mut live = rig.start();
    live.completed("u1", "hello").await;
    live.close().await;
    rig.check();
}

#[tokio::test]
async fn a_project_file_resumed_then_compacted() {
    let setup = Setup {
        clock: Clocks::Stepped,
        models: Some("[models.\"claude-opus-5-5\"]\nnative_compaction = false\n"),
        ..in_sub()
    };
    let script = vec![
        says("msg_01", 40_000, "Hello."),
        greeted("msg_02", 40_100),
        says("msg_03", 520, "Again."),
    ];
    let rig = Rig::new("instructions_resume", setup, script).await;
    rig.project();
    let mut live = rig.start();
    live.completed("u1", "hello").await;
    let (before, _) = live.close().await;
    rig.assert_rebuilds(&before);
    let back = rig.resume();
    let (resumed, _) = back.close().await;
    assert_eq!(
        serde_json::to_string(&*resumed.history()).unwrap(),
        serde_json::to_string(&*before.history()).unwrap(),
        "the resumed history differs from the live one"
    );
    let mut back = rig.resume();
    back.compacted(1).await;
    back.completed("u2", "again").await;
    let (after, _) = back.close().await;
    rig.assert_rebuilds(&after);
    rig.check();
}

fn today_base() -> Value {
    let today = fs::read_to_string(fixture_dir("instructions_project").join("01.json")).unwrap();
    serde_json::from_str::<Value>(&today).unwrap()["system"][0].clone()
}

fn with_extra() -> Setup {
    Setup {
        extra: Some(crate::instructions::Extra::Autonomous),
        ..in_sub()
    }
}

#[tokio::test]
async fn the_autonomous_text_alone_is_the_second_block() {
    let script = vec![says("msg_01", 400, "Hello.")];
    let rig = Rig::new("instructions_extra_alone", with_extra(), script).await;
    rig.repository();
    let mut live = rig.start();
    live.completed("u1", "hello").await;
    live.close().await;
    rig.check();
    let body: Value = serde_json::from_str(&rig.server.seen().bodies[0]).unwrap();
    let system = body["system"].as_array().unwrap();
    assert_eq!(system.len(), 2);
    assert_eq!(system[0], today_base());
    assert_eq!(
        system[1]["text"],
        json!(crate::instructions::Extra::Autonomous.block())
    );
}

#[tokio::test]
async fn the_autonomous_text_goes_before_the_project_file() {
    let script = vec![says("msg_01", 400, "Hello.")];
    let rig = Rig::new("instructions_extra_project", with_extra(), script).await;
    rig.project();
    let mut live = rig.start();
    live.completed("u1", "hello").await;
    live.close().await;
    rig.check();
    let body: Value = serde_json::from_str(&rig.server.seen().bodies[0]).unwrap();
    assert_eq!(body["system"][0], today_base());
    let second = body["system"][1]["text"].as_str().unwrap().to_owned();
    let extra = crate::instructions::Extra::Autonomous.block();
    let project = second.strip_prefix(&format!("{extra}\n")).unwrap();
    assert!(project.starts_with("<instructions>\n"), "{project}");
    assert!(project.contains("Answer briefly."), "{project}");
}

#[tokio::test]
async fn without_the_plugin_only_the_autonomous_text_comes() {
    let setup = Setup {
        removed: true,
        ..with_extra()
    };
    let script = vec![says("msg_01", 400, "Hello.")];
    let rig = Rig::new("instructions_removed", setup, script).await;
    rig.project();
    let mut live = rig.start();
    live.completed("u1", "hello").await;
    live.close().await;
    rig.check();
    let raw = &rig.server.seen().bodies[0];
    assert!(!raw.contains("<instructions>"), "{raw}");
    assert!(!raw.contains("Project root:"), "{raw}");
    assert!(!raw.contains("Answer briefly."), "{raw}");
    let body: Value = serde_json::from_str(raw).unwrap();
    let system = body["system"].as_array().unwrap();
    assert_eq!(system.len(), 2);
    assert_eq!(system[0], today_base());
    assert_eq!(
        system[1]["text"],
        json!(crate::instructions::Extra::Autonomous.block())
    );
}

mod deltas;
