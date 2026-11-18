use super::resume::{child_send, environment_of, list, reported, resume, without_clocks};
use super::*;

const OPENING: &str = "Instructions for your preset follow, from the file named by Preset instructions in <environment>. File names are relative to that file's directory. Your parent's messages take precedence over them.";

const PLANNER: &str =
    "[agents.presets.planner]\ndescription = \"Plans\"\ninstructions = \"~/roles/planner\"\n";

fn section(text: &str) -> String {
    format!(
        "<instructions>\n{OPENING}\n<file scope=\"preset\" name=\"AGENTS.md\">\n{text}</file>\n</instructions>"
    )
}

fn block(run: &Run, agent: &str) -> Option<String> {
    run.requests(agent)[0].session.instructions.clone()
}

fn preset_spawn(name: &str, preset: &str, cwd: &str) -> SpawnRequest {
    SpawnRequest {
        preset: Some(preset.into()),
        cwd: Some(cwd.into()),
        ..spawn_request(name, "Work.")
    }
}

fn plain_spawn(name: &str, cwd: &str) -> SpawnRequest {
    SpawnRequest {
        cwd: Some(cwd.into()),
        ..spawn_request(name, "Work.")
    }
}

fn reporting(text: &str) -> Vec<Step> {
    vec![uses(&[report_call("r1", text)]), done("x")]
}

#[tokio::test(start_paused = true)]
async fn preset_children_share_one_prefix_whose_block_is_the_projects_then_the_presets_section() {
    let mut run = start(
        "instructions-prefix",
        Setup {
            project_instructions: true,
            home_files: vec![
                ("wt1/.git", "gitdir: elsewhere\n"),
                ("wt1/AGENTS.md", "Project rules.\n"),
                ("wt2/.git", "gitdir: elsewhere\n"),
                ("wt2/AGENTS.md", "Project rules.\n"),
                ("roles/planner/AGENTS.md", "You plan.\n"),
                ("roles/review.md", "You review.\n"),
            ],
            preset_config: Some(
                "[agents.presets.planner]\ndescription = \"Plans\"\ninstructions = \"~/roles/planner\"\n\n[agents.presets.twin]\ndescription = \"Also plans\"\ninstructions = \"../../roles/planner/AGENTS.md\"\n\n[agents.presets.reviewer]\ndescription = \"Reviews\"\ninstructions = \"~/roles/review.md\"\n\n[agents.presets.coder]\ndescription = \"Codes\"\n",
            ),
            ..Setup::default()
        },
        vec![
            ("/", vec![done("ok"); 8]),
            (
                "/p1",
                vec![
                    uses(&[spawn_call("s1", "leaf", "Go deeper.")]),
                    done("waiting"),
                    uses(&[report_call("r1", "p1 done")]),
                ],
            ),
            ("/p1/leaf", reporting("leaf done")),
            ("/p2", reporting("p2 done")),
            ("/t", reporting("t done")),
            ("/r", reporting("r done")),
            ("/c", reporting("c done")),
            ("/n", reporting("n done")),
        ],
    )
    .await;
    run.spawn(preset_spawn("p1", "planner", "~/wt1")).await;
    run.spawn(preset_spawn("p2", "planner", "~/wt2")).await;
    run.spawn(preset_spawn("t", "twin", "~/wt1")).await;
    run.spawn(preset_spawn("r", "reviewer", "~/wt1")).await;
    run.spawn(preset_spawn("c", "coder", "~/wt1")).await;
    run.spawn(plain_spawn("n", "~/wt1")).await;
    for child in ["/p1/leaf", "/p1", "/p2", "/t", "/r", "/c", "/n"] {
        reported(&mut run, child).await;
    }
    let project = block(&run, "/n").unwrap();
    assert!(project.contains("Project rules.\n"), "{project}");
    let planner = format!("{project}\n{}", section("You plan.\n"));
    assert_eq!(block(&run, "/p1").as_deref(), Some(planner.as_str()));
    let p1 = run.requests("/p1")[0].clone();
    let p2 = run.requests("/p2")[0].clone();
    assert_eq!(p2.prefix(), p1.prefix());
    assert_eq!(p2.tools, p1.tools);
    assert_eq!(block(&run, "/t"), block(&run, "/p1"));
    let r = run.requests("/r")[0].clone();
    assert_eq!(r.tools, p1.tools);
    assert_eq!(r.session.system, p1.session.system);
    let review = section("You review.\n").replace("name=\"AGENTS.md\"", "name=\"review.md\"");
    assert_eq!(r.session.instructions, Some(format!("{project}\n{review}")));
    assert_eq!(block(&run, "/c").as_deref(), Some(project.as_str()));
    assert_eq!(run.requests("/c")[0].tools, run.requests("/n")[0].tools);
    assert_eq!(block(&run, "/p1/leaf").as_deref(), Some(project.as_str()));
    let env = p1.first_user_texts()[0].clone();
    assert!(
        env.contains("\nParent: main\nPreset instructions: ~/roles/planner/AGENTS.md\n"),
        "{env}"
    );
    let twin_env = run.requests("/t")[0].first_user_texts()[0].clone();
    assert!(
        twin_env.contains("\nPreset instructions: ~/roles/planner/AGENTS.md\n"),
        "{twin_env}"
    );
    for agent in ["/n", "/c", "/p1/leaf"] {
        let env = run.requests(agent)[0].first_user_texts()[0].clone();
        assert!(!env.contains("Preset instructions"), "{agent}: {env}");
    }
    let run = run.quit().await;
    let loaded = run
        .records("/p1")
        .into_iter()
        .find_map(|e| match e {
            LogEvent::InstructionsLoaded {
                chosen: Some(chosen),
                imports,
                ..
            } => Some((chosen, imports)),
            _ => None,
        })
        .unwrap();
    let scopes: Vec<&str> = std::iter::once(&loaded.0)
        .chain(&loaded.1)
        .map(|f| f.scope.as_str())
        .collect();
    assert_eq!(scopes, ["project", "preset"], "{loaded:?}");
}

#[tokio::test(start_paused = true)]
async fn without_project_instructions_a_preset_child_still_gets_its_section() {
    let mut run = start(
        "instructions-none",
        Setup {
            home_files: vec![
                ("work/.git", "gitdir: elsewhere\n"),
                ("work/AGENTS.md", "Project rules.\n"),
                ("roles/planner/AGENTS.md", "You plan.\n"),
            ],
            preset_config: Some(PLANNER),
            ..Setup::default()
        },
        vec![
            ("/", vec![done("ok"); 3]),
            ("/p", reporting("p done")),
            ("/n", reporting("n done")),
        ],
    )
    .await;
    run.spawn(preset_spawn("p", "planner", "~/work")).await;
    run.spawn(plain_spawn("n", "~/work")).await;
    reported(&mut run, "/p").await;
    reported(&mut run, "/n").await;
    assert_eq!(block(&run, "/p"), Some(section("You plan.\n")));
    assert_eq!(block(&run, "/n"), None);
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn an_edit_reaches_the_next_spawn_a_running_child_keeps_its_block_and_a_broken_file_refuses()
{
    let mut run = start(
        "instructions-edits",
        Setup {
            home_files: vec![("roles/planner/AGENTS.md", "You plan.\n")],
            preset_config: Some(PLANNER),
            ..Setup::default()
        },
        vec![
            ("/", vec![done("ok"); 6]),
            (
                "/a",
                vec![uses(&[report_call("r1", "a done")]), done("again")],
            ),
            ("/b", reporting("b done")),
        ],
    )
    .await;
    run.spawn(preset_spawn("a", "planner", "~/work")).await;
    reported(&mut run, "/a").await;
    run.child_ended("/a").await;
    let file = run.env.root.join("roles/planner/AGENTS.md");
    fs::write(&file, "You plan carefully.\n").unwrap();
    run.send(child_send("/a", "you-1", "Once more.")).await;
    run.child_ended("/a").await;
    let a = run.requests("/a");
    assert_eq!(a.len(), 2);
    assert_eq!(a[1].prefix(), a[0].prefix());
    assert_eq!(a[1].session.instructions, Some(section("You plan.\n")));
    run.spawn(preset_spawn("b", "planner", "~/work")).await;
    reported(&mut run, "/b").await;
    assert_eq!(block(&run, "/b"), Some(section("You plan carefully.\n")));
    fs::remove_file(&file).unwrap();
    let request = run.spawn(preset_spawn("c", "planner", "~/work")).await;
    let rejected = run
        .until(
            "the refusal",
            |e| matches!(e, AgentEvent::Rejected { request: Some(r), .. } if *r == request),
        )
        .await;
    let AgentEvent::Rejected { reason, .. } = rejected else {
        unreachable!()
    };
    assert!(
        reason.contains("preset planner's instructions no longer load: ")
            && reason.contains("~/roles/planner"),
        "{reason}"
    );
    assert!(
        !reason.contains(&run.env.root.to_string_lossy().into_owned()),
        "{reason}"
    );
    let run = run.quit().await;
    assert!(run.requests("/c").is_empty());
    assert!(
        run.store
            .read(&session_name("/c"))
            .map(|r| r.is_empty())
            .unwrap_or(true)
    );
}

#[tokio::test(start_paused = true)]
async fn a_resumed_preset_child_keeps_its_logged_block_and_environment_after_an_edit_and_a_removal()
{
    let setup = |config: &'static str| Setup {
        preset_config: Some(config),
        ..Setup::default()
    };
    let mut run = start(
        "instructions-resume",
        Setup {
            home_files: vec![("roles/planner/AGENTS.md", "You plan.\n")],
            ..setup(PLANNER)
        },
        vec![("/", vec![done("ok"); 3]), ("/p", reporting("p done"))],
    )
    .await;
    run.spawn(preset_spawn("p", "planner", "~/work")).await;
    reported(&mut run, "/p").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let run = run.quit().await;
    let before = run.requests("/p")[0].clone();
    let env_before = before.first_user_texts()[0].clone();
    fs::write(
        run.env.root.join("roles/planner/AGENTS.md"),
        "You plan differently.\n",
    )
    .unwrap();
    let back = resume(run, setup(PLANNER), vec![("/p", vec![done("again")])]).await;
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    let mut run = back.run;
    run.send(child_send("/p", "you-1", "Once more.")).await;
    run.child_ended("/p").await;
    let after = run.requests("/p").last().cloned().unwrap();
    assert_eq!(after.prefix(), before.prefix());
    assert_eq!(after.session.instructions, Some(section("You plan.\n")));
    assert!(after.messages.starts_with(&before.messages));
    assert_eq!(after.first_user_texts()[0], env_before);
    assert_eq!(environment_of(&after), None);
    assert!(!format!("{:?}", after.messages).contains("instructions-update"));
    let run = run.quit().await;
    let back = resume(
        run,
        setup("[agents.presets.other]\ndescription = \"Other\"\n"),
        vec![("/p", vec![done("still")])],
    )
    .await;
    assert_eq!(back.warnings.len(), 1, "{:?}", back.warnings);
    assert!(
        back.warnings[0].starts_with("preset planner was removed; /p keeps the configuration"),
        "{:?}",
        back.warnings
    );
    let mut run = back.run;
    run.send(child_send("/p", "you-2", "And again.")).await;
    run.child_ended("/p").await;
    let last = run.requests("/p").last().cloned().unwrap();
    assert_eq!(last.prefix(), before.prefix());
    assert_eq!(last.first_user_texts()[0], env_before);
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn paths_with_a_redacted_endpoint_path_survive_the_scrub_so_a_resume_resends_nothing() {
    let config = "[agents.presets.planner]\ndescription = \"Plans\"\ninstructions = \"~/api-roles/planner\"\n";
    let setup = || Setup {
        redactions: vec!["/api"],
        preset_config: Some(config),
        ..Setup::default()
    };
    let mut run = start(
        "instructions-canary",
        Setup {
            home_files: vec![
                ("api-roles/planner/AGENTS.md", "You plan.\n"),
                ("api-work/notes.md", "x\n"),
            ],
            ..setup()
        },
        vec![
            ("/", vec![done("ok"); 3]),
            ("/api-review", reporting("reviewed")),
        ],
    )
    .await;
    run.spawn(preset_spawn("api-review", "planner", "~/api-work"))
        .await;
    reported(&mut run, "/api-review").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let listed = list(&mut run).await;
    let run = run.quit().await;
    let before = run.requests("/api-review")[0].clone();
    let back = resume(
        run,
        setup(),
        vec![
            (
                "/",
                vec![
                    uses(&[send_call("m1", "api-review", "Check the paths.")]),
                    done("ok"),
                    done("ok"),
                ],
            ),
            ("/api-review", reporting("checked")),
        ],
    )
    .await;
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    let mut run = back.run;
    let relisted = list(&mut run).await;
    assert_eq!(without_clocks(&relisted), without_clocks(&listed));
    run.say("tell it").await;
    reported(&mut run, "/api-review").await;
    let after = run.requests("/api-review").last().cloned().unwrap();
    assert_eq!(after.prefix(), before.prefix());
    assert!(after.messages.starts_with(&before.messages));
    assert_eq!(environment_of(&after), None);
    assert!(
        after
            .last_user_texts()
            .join("\n")
            .contains("Check the paths."),
        "{:?}",
        after.last_user_texts()
    );
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_version_29_tree_with_typed_reports_resumes_with_their_summaries_and_one_rebuild_warning()
{
    let old_setup = || Setup {
        presets: presets("[agents.presets.rev]\ndescription = \"Reviews\"\n"),
        ..Setup::default()
    };
    let mut run = start(
        "instructions-old-tree",
        old_setup(),
        vec![
            ("/", vec![done("ok"); 4]),
            ("/old", vec![uses(&[report_call("r1", "typed")]), done("x")]),
        ],
    )
    .await;
    run.spawn(SpawnRequest {
        preset: Some("rev".into()),
        ..spawn_request("old", "Work.")
    })
    .await;
    reported(&mut run, "/old").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let run = run.quit().await;
    let before = run.requests("/old")[0].clone();
    let typed = json!({
        "kind": "plan",
        "status": "done",
        "summary": "Planned it.\nIn detail.",
        "plan_path": "docs/plans/x.md",
        "decisions": []
    });
    for agent in ["/", "/old"] {
        run.store.edit(&session_name(agent), |records| {
            for record in records.iter_mut() {
                record.schema_version = 29;
                with_report_keys(record);
                match &mut record.event {
                    LogEvent::AgentReported { report, .. } => *report = Some(typed.clone()),
                    LogEvent::SessionStarted { tools_sha256, .. } if agent == "/old" => {
                        *tools_sha256 = Some("0".repeat(64));
                    }
                    _ => {}
                }
            }
        });
    }
    let back = resume(
        run,
        old_setup(),
        vec![
            (
                "/",
                vec![
                    uses(&[send_call("m1", "old", "Once more.")]),
                    done("ok"),
                    done("ok"),
                ],
            ),
            ("/old", reporting("Now in text.\nWith a body.")),
        ],
    )
    .await;
    assert_eq!(back.warnings.len(), 1, "{:?}", back.warnings);
    assert!(
        back.warnings[0].starts_with("Wisp's prompt or tools changed since this tree ran: 1 agent"),
        "{:?}",
        back.warnings
    );
    let mut run = back.run;
    let listed = list(&mut run).await;
    assert_eq!(listed[0].last_report.as_deref(), Some("Planned it."));
    run.say("again").await;
    reported(&mut run, "/old").await;
    let after = run.requests("/old").last().cloned().unwrap();
    assert!(after.messages.starts_with(&before.messages));
    let listed = list(&mut run).await;
    assert_eq!(listed[0].last_report.as_deref(), Some("Now in text."));
    let run = run.quit().await;
    let mails = run.mail_to("/");
    let (_, _, last) = mails
        .iter()
        .rev()
        .find(|(kind, _, _)| *kind == MailKind::Report)
        .unwrap();
    assert!(
        unwrapped(last).ends_with("\nNow in text.\nWith a body."),
        "{last}"
    );
}

fn with_report_keys(record: &mut wisp_protocol::log::LogRecord) {
    let mut value = serde_json::to_value(&*record).unwrap();
    let mut found = false;
    for pointer in ["/config", "/parent/config"] {
        if let Some(Value::Object(config)) = value.pointer_mut(pointer) {
            config.insert("report".into(), json!("plan"));
            let values = config
                .get_mut("preset_values")
                .and_then(Value::as_object_mut);
            values.unwrap().insert("report".into(), json!("plan"));
            found = true;
        }
    }
    if found {
        *record = serde_json::from_value(value).unwrap();
    }
}

fn big(line: &str) -> String {
    let mut text = String::new();
    let mut n = 0;
    while text.len() < 40 * 1024 {
        text.push_str(&format!("{line} {n:05}\n"));
        n += 1;
    }
    text.trim_end().to_owned()
}

fn unwrapped(mail: &str) -> &str {
    let inner = mail.strip_suffix("\n</wisp:message>").unwrap();
    inner.split_once('\n').map(|(_, rest)| rest).unwrap()
}

fn cut_file(run: &Run, mail: &str) -> String {
    let note = unwrapped(mail)
        .rsplit_once("\n[cut: the full report is in ")
        .map(|(_, note)| note.trim_end_matches(']'))
        .unwrap_or_else(|| panic!("no cut note: {}", &mail[mail.len().saturating_sub(200)..]));
    let path = note
        .strip_prefix("~/")
        .map(|rest| run.env.root.join(rest))
        .unwrap();
    fs::read_to_string(path).unwrap()
}

#[tokio::test(start_paused = true)]
async fn long_reports_are_cut_with_a_note_naming_the_file_that_holds_them_whole() {
    let lines = big("a line of the report");
    let single = "x".repeat(40 * 1024);
    let last: &'static str = Box::leak(big("a line of its last text").into_boxed_str());
    let mut run = start(
        "instructions-cuts",
        Setup::default(),
        vec![
            ("/", vec![done("ok"); 8]),
            ("/lines", reporting(&lines)),
            ("/single", reporting(&single)),
            ("/silent", vec![done(last), done(last), done(last)]),
        ],
    )
    .await;
    for name in ["lines", "single", "silent"] {
        run.spawn(spawn_request(name, "Work.")).await;
        run.child_ended(&format!("/{name}")).await;
    }
    for name in ["lines", "single", "silent"] {
        let path = format!("/{name}");
        run.until(
            &format!("{path}'s report"),
            move |e| matches!(e, AgentEvent::AgentReported { agent, .. } if agent.as_str() == path),
        )
        .await;
    }
    let run = run.quit().await;
    let reports: Vec<String> = run
        .mail_to("/")
        .into_iter()
        .filter(|(kind, _, _)| *kind == MailKind::Report)
        .map(|(_, _, text)| text)
        .collect();
    assert_eq!(reports.len(), 3, "{}", reports.len());
    for (mail, whole) in reports.iter().zip([lines.as_str(), single.as_str(), last]) {
        let (body, _) = unwrapped(mail).rsplit_once("\n[cut: ").unwrap();
        let kept = body.split_once('\n').map(|(_, kept)| kept).unwrap();
        assert!(kept.len() <= brief::MAX_REPORT_BYTES, "{}", kept.len());
        assert!(whole.starts_with(kept));
        assert_eq!(cut_file(&run, mail), whole);
        if whole.contains('\n') {
            assert!(whole[kept.len()..].starts_with('\n'), "cut at a line");
        } else {
            assert_eq!(kept.len(), brief::MAX_REPORT_BYTES);
        }
    }
    let live = run.seen.iter().any(|e| {
        matches!(e, AgentEvent::AgentReported { report: Some(Value::String(text)), .. } if *text == lines)
    });
    assert!(live, "the event holds the whole text");
    let logged: Vec<Value> = pick!(
        run.records("/lines"),
        LogEvent::AgentReported {
            report: Some(report),
            ..
        } => report
    );
    assert!(
        logged.contains(&Value::String(lines.clone())),
        "the record holds the whole text"
    );
}

#[tokio::test(start_paused = true)]
async fn list_agents_opens_with_the_callers_own_row_and_every_session_id() {
    let mut run = start(
        "instructions-list",
        Setup::default(),
        vec![
            (
                "/",
                vec![
                    uses(&[call("l0", wisp_protocol::agents::LIST_AGENTS, json!({}))]),
                    done("ok"),
                    done("ok"),
                    done("ok"),
                ],
            ),
            (
                "/a",
                vec![
                    uses(&[call("l1", wisp_protocol::agents::LIST_AGENTS, json!({}))]),
                    uses(&[report_call("r1", "a done")]),
                ],
            ),
        ],
    )
    .await;
    run.say("list").await;
    run.root_ended().await;
    run.spawn(spawn_request("a", "Work.")).await;
    reported(&mut run, "/a").await;
    let (_, root) = result_of(&run.requests("/")[1], "l0");
    assert_eq!(
        root,
        format!(
            "You: main · session s-root · {MODEL}, high, on env (Claude API) · context 100 of 1M, limit 850k · in ~/work\nNo other agents yet."
        )
    );
    let (_, a) = result_of(&run.requests("/a")[1], "l1");
    let rows: Vec<&str> = a.lines().collect();
    assert!(
        rows[0].starts_with("You: /a · session s-a · ")
            && rows[0].contains(" API · context 100 of 1M, limit 850k · in ~/work")
            && rows[0].contains(" · ran 0s · "),
        "{a}"
    );
    assert!(!rows[0].contains("running"), "{a}");
    assert_eq!(rows[1], "1 agent you can reach:");
    assert!(
        rows[2].starts_with("- main (your parent) · session s-root · ")
            && rows[2].contains(" · context 100 of 1M, limit 850k · "),
        "{a}"
    );
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_resumed_child_keeps_its_block_when_its_presets_file_moves_or_is_deleted() {
    let setup = |config: &'static str| Setup {
        preset_config: Some(config),
        ..Setup::default()
    };
    let mut run = start(
        "instructions-moved",
        Setup {
            home_files: vec![
                ("roles/planner/AGENTS.md", "You plan.\n"),
                ("roles/other.md", "You plan otherwise.\n"),
            ],
            ..setup(PLANNER)
        },
        vec![("/", vec![done("ok"); 3]), ("/p", reporting("p done"))],
    )
    .await;
    run.spawn(preset_spawn("p", "planner", "~/work")).await;
    reported(&mut run, "/p").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let run = run.quit().await;
    let before = run.requests("/p")[0].clone();
    let env_before = before.first_user_texts()[0].clone();
    let moved =
        "[agents.presets.planner]\ndescription = \"Plans\"\ninstructions = \"~/roles/other.md\"\n";
    let back = resume(run, setup(moved), vec![("/p", vec![done("again")])]).await;
    assert_eq!(back.live, ["/p"]);
    assert_eq!(back.warnings.len(), 1, "{:?}", back.warnings);
    assert!(
        back.warnings[0].starts_with("preset planner has changed; /p keeps the configuration"),
        "{:?}",
        back.warnings
    );
    let mut run = back.run;
    run.send(child_send("/p", "you-1", "Once more.")).await;
    run.child_ended("/p").await;
    let after = run.requests("/p").last().cloned().unwrap();
    assert_eq!(after.prefix(), before.prefix());
    assert_eq!(after.first_user_texts()[0], env_before);
    let run = run.quit().await;
    fs::remove_file(run.env.root.join("roles/planner/AGENTS.md")).unwrap();
    let back = resume(run, setup(PLANNER), vec![("/p", vec![done("still")])]).await;
    assert_eq!(back.live, ["/p"]);
    assert_eq!(back.warnings.len(), 1, "{:?}", back.warnings);
    assert!(
        back.warnings[0].starts_with("preset planner is disabled; /p keeps the configuration"),
        "{:?}",
        back.warnings
    );
    let mut run = back.run;
    run.send(child_send("/p", "you-2", "And again.")).await;
    run.child_ended("/p").await;
    let last = run.requests("/p").last().cloned().unwrap();
    assert_eq!(last.prefix(), before.prefix());
    assert_eq!(last.session.instructions, Some(section("You plan.\n")));
    assert!(last.messages.starts_with(&before.messages));
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_log_without_preset_values_counts_instructions_added_since_as_a_change() {
    let plain = "[agents.presets.planner]\ndescription = \"Plans\"\n";
    let mut run = start(
        "instructions-no-values",
        Setup {
            home_files: vec![("roles/planner/AGENTS.md", "You plan.\n")],
            preset_config: Some(plain),
            ..Setup::default()
        },
        vec![("/", vec![done("ok"); 3]), ("/p", reporting("p done"))],
    )
    .await;
    run.spawn(preset_spawn("p", "planner", "~/work")).await;
    reported(&mut run, "/p").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let run = run.quit().await;
    for agent in ["/", "/p"] {
        run.store.edit(&session_name(agent), |records| {
            for record in records.iter_mut() {
                match &mut record.event {
                    LogEvent::AgentSpawned { config, .. } => config.preset_values = None,
                    LogEvent::SessionStarted {
                        parent: Some(parent),
                        ..
                    } => parent.config.preset_values = None,
                    _ => {}
                }
            }
        });
    }
    let unchanged = resume(
        run,
        Setup {
            preset_config: Some(plain),
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    assert!(unchanged.warnings.is_empty(), "{:?}", unchanged.warnings);
    let run = unchanged.run.quit().await;
    let back = resume(
        run,
        Setup {
            preset_config: Some(PLANNER),
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    assert_eq!(back.warnings.len(), 1, "{:?}", back.warnings);
    assert!(
        back.warnings[0].starts_with("preset planner has changed; /p keeps the configuration"),
        "{:?}",
        back.warnings
    );
    let _ = back.run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_compacted_preset_child_keeps_its_system_and_its_heads_environment_names_the_file() {
    use super::limits::{LOW_WISP, SUMMARY, priced};
    let mut run = start(
        "instructions-compact",
        Setup {
            models: Some(LOW_WISP),
            home_files: vec![("roles/planner/AGENTS.md", "You plan.\n")],
            preset_config: Some(PLANNER),
            ..Setup::default()
        },
        vec![
            (
                "/p",
                vec![
                    priced(uses(&[report_call("r1", "first")]), 5_000, 40),
                    priced(done(SUMMARY), 3_000, 300),
                    uses(&[report_call("r2", "second")]),
                ],
            ),
            ("/", vec![done("noted"); 3]),
        ],
    )
    .await;
    run.spawn(preset_spawn("p", "planner", "~/work")).await;
    reported(&mut run, "/p").await;
    run.child_ended("/p").await;
    run.send(child_send("/p", "you-1", "Again, please.")).await;
    run.child_ended("/p").await;
    let run = run.quit().await;
    let compacted = run
        .seen
        .iter()
        .filter_map(|e| inner(e, "/p"))
        .any(|e| matches!(e, AgentEvent::Compacted { .. }));
    assert!(compacted, "the second turn compacts");
    let requests = run.requests("/p");
    let first = &requests[0];
    let last = requests.last().unwrap();
    assert_eq!(last.session.system, first.session.system);
    assert_eq!(last.session.instructions, first.session.instructions);
    assert_eq!(last.tools, first.tools);
    let head = last.first_user_texts().join("\n");
    assert!(head.contains("<compacted_context"), "{head}");
    assert!(
        head.contains("<environment>")
            && head.contains("\nPreset instructions: ~/roles/planner/AGENTS.md\n"),
        "{head}"
    );
}

#[tokio::test(start_paused = true)]
async fn the_roots_extra_instructions_never_reach_a_child() {
    let mut run = start(
        "instructions-extra-child",
        Setup {
            project_instructions: true,
            extra: Some(crate::instructions::Extra::Autonomous),
            home_files: vec![
                ("wt1/.git", "gitdir: elsewhere\n"),
                ("wt1/AGENTS.md", "Project rules.\n"),
            ],
            ..Setup::default()
        },
        vec![("/", vec![done("ok"); 4]), ("/n", reporting("n done"))],
    )
    .await;
    run.say("hi").await;
    run.root_ended().await;
    run.spawn(plain_spawn("n", "~/wt1")).await;
    reported(&mut run, "/n").await;
    let child = block(&run, "/n").unwrap();
    assert!(child.contains("Project rules.\n"), "{child}");
    assert!(!child.contains("extra_instructions"), "{child}");
    let root = block(&run, "/").unwrap();
    assert!(
        root.starts_with(&crate::instructions::Extra::Autonomous.block()),
        "{root}"
    );
    run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_child_on_a_fallback_sends_the_bytes_of_an_explicit_spawn_there() {
    let rooted = rooted("fallback-prefix", Setup {
        project_instructions: true,
        home_files: vec![("wt/.git", "gitdir: elsewhere\n"), ("wt/AGENTS.md", "Project rules.\n"), ("roles/planner/AGENTS.md", "You plan.\n")],
        endpoints: vec![endpoint("first"), openai("spare", wisp_protocol::Billing::ChatGptPlan)],
        preset_config: Some("[agents.presets.planner]\ndescription = 'Plans'\nendpoint = 'first'\nmodel = 'claude-opus-5-5'\ninstructions = '~/roles/planner'\nfallback = [{endpoint = 'spare', model = 'gpt-6-astra', effort = 'high'}]\n"),
        ..Setup::default()
    }, vec![("spare:/fallback", vec![done("working").hanging()]), ("spare:/explicit", vec![done("working").hanging()])]).await;
    let now = rooted.kit.clock.now().as_second();
    super::endpoints::hold_account(&rooted, "first", now, now + 3600);
    let mut run = serve(rooted);
    for name in ["fallback", "explicit"] {
        let mut request = preset_spawn(name, "planner", "~/wt");
        if name == "explicit" {
            request.endpoint = Some("spare".into());
            request.model = Some("gpt-6-astra".into());
            request.effort = Some(Effort::High);
        }
        run.spawn(request).await;
        run.until("the child's step", |event| matches!(event, AgentEvent::Agent { agent, event } if agent.as_str() == format!("/{name}") && matches!(event.as_ref(), AgentEvent::TextDelta { text, .. } if text == "working"))).await;
    }
    let fallback = &run.requests("spare:/fallback")[0];
    let explicit = &run.requests("spare:/explicit")[0];
    assert_eq!(fallback.prefix(), explicit.prefix());
    assert_eq!(fallback.tools, explicit.tools);
    assert_eq!(fallback.session, explicit.session);
    assert!(
        fallback
            .session
            .instructions
            .as_deref()
            .unwrap()
            .contains("Project rules.")
    );
    run.quit().await;
}
