use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use wisp_core::session::{JsonlStore, ResumeLock};
use wisp_render::transcript::copy;

use super::{
    API_KEY, CONFIG, Home, PATIENCE, Pty, READY, Server, assert_hosted, assert_prefix_stable,
    check, counted_tokens, full_screen_with, is_count, normalise_body, normalise_head, picking,
    reply, serve, sse_reply, usable, wait_for_exit, wait_for_requests, wait_until,
};
use crate::support::server::Seen;

fn sessions(home: &Home) -> PathBuf {
    home.0.join(".local/share/wisp/sessions")
}

fn ids(home: &Home) -> BTreeSet<String> {
    std::fs::read_dir(sessions(home)).map_or_else(
        |_| BTreeSet::new(),
        |entries| {
            entries
                .filter_map(|e| e.unwrap().file_name().into_string().ok())
                .filter_map(|name| name.strip_suffix(".jsonl").map(str::to_owned))
                .collect()
        },
    )
}

fn seed(home: &Home, server: &Server, text: &str, args: &[&str], envs: &[(&str, &str)]) -> String {
    let before = ids(home);
    let mut pty = full_screen_with(home, server, &os(args), envs);
    pty.wait_for(READY);
    std::thread::sleep(Duration::from_millis(200));
    pty.master
        .write_all(format!("{text}\r").as_bytes())
        .unwrap();
    pty.wait_shown("GOLDEN-DONE");
    pty.wait_idle();
    pty.master.write_all(b"\x04").unwrap();
    wait_for_exit(&mut pty.child);
    let mut new: Vec<String> = ids(home).difference(&before).cloned().collect();
    assert_eq!(new.len(), 1, "{}", pty.shown());
    new.remove(0)
}

fn messages(server: &Server) -> Vec<(String, String)> {
    server
        .requests()
        .into_iter()
        .filter(|(head, _)| !is_count(head))
        .collect()
}

fn os<'a>(args: &'a [&'a str]) -> Vec<&'a OsStr> {
    args.iter().map(OsStr::new).collect()
}

fn again(pty: &mut Pty, server: &Server) -> (String, String) {
    let sent = messages(server).len();
    std::thread::sleep(Duration::from_millis(200));
    pty.master.write_all(b"again\r").unwrap();
    wait_for_requests(server, server.requests().len() + 1);
    pty.wait_shown("GOLDEN-DONE");
    pty.wait_idle();
    let all = messages(server);
    assert_eq!(
        all.len(),
        sent + 1,
        "one message request, and no self-check"
    );
    let (head, body) = all[sent].clone();
    (normalise_head(&head), normalise_body(&body))
}

#[test]
fn a_legacy_non_adaptive_chat_can_resume_and_switch_to_an_adaptive_model() {
    if !usable() {
        return;
    }
    for model in ["claude-opus-4-5", "claude-sonnet-4-5"] {
        let server = serve();
        let home = Home::new("resume-non-adaptive");
        let url = server.url();
        home.write(
            CONFIG,
            &format!("[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"ANTHROPIC_AUTH_TOKEN\"\nurl = \"{url}\"\n"),
        );
        let envs = [
            ("ANTHROPIC_AUTH_TOKEN", super::GW_TOKEN),
            ("ANTHROPIC_BASE_URL", url.as_str()),
            ("WISP_JOB", "1"),
        ];
        let mut doctor = home.golden_command(super::WISP);
        doctor
            .envs(envs)
            .args(["--model", "claude-opus-4-6", "doctor"]);
        let output = doctor.output().unwrap();
        assert!(output.status.success(), "{}", super::both(&output));
        assert!(
            super::both(&output).contains("→ 127.0.0.1:"),
            "{}",
            super::both(&output)
        );
        let id = seed(
            &home,
            &server,
            "legacy history",
            &["--model", "claude-opus-4-6"],
            &envs,
        );
        let log = sessions(&home).join(format!("{id}.jsonl"));
        let before = std::fs::read_to_string(&log).unwrap();
        let legacy = before.replace("\"claude-opus-4-6\"", &format!("\"{model}\""));
        assert_ne!(legacy, before);
        std::fs::write(&log, &legacy).unwrap();
        let mut pty = full_screen_with(&home, &server, &os(&["--resume", &id]), &envs);
        pty.wait_shown("legacy history");
        pty.wait_shown(model.strip_prefix("claude-").unwrap());
        assert_eq!(messages(&server).len(), 1);
        assert!(std::fs::read_to_string(&log).unwrap().starts_with(&legacy));
        let (head, body) = again(&mut pty, &server);
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["model"], model);
        assert!(body["thinking"]["block_binding"].is_null());
        assert!(!head.contains(wisp_core::provider::anthropic::BINDING_CONTROLS_BETA));
        pty.master.write_all(b"/model claude-opus-5-5\r").unwrap();
        pty.wait_shown("opus-5-5 medium");
        let (_, body) = again(&mut pty, &server);
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["model"], "claude-opus-5-5");
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert!(body["messages"].to_string().contains("legacy history"));
        assert_eq!(messages(&server).len(), 3);
        pty.master.write_all(b"\x04").unwrap();
        assert!(wait_for_exit(&mut pty.child).success());
        assert!(std::fs::read_to_string(&log).unwrap().starts_with(&legacy));
    }
}

#[test]
fn a_chat_picked_with_resume_sends_what_a_fresh_resume_of_it_sends() {
    if !usable() {
        return;
    }
    let server = serve();
    let home = Home::new("resume-relaunch");
    let elsewhere = serve();
    let config = format!(
        "[endpoints.work]\nprovider = \"anthropic\"\nauth = \"api_key\"\nkey_env = \"ANTHROPIC_API_KEY\"\nurl = \"{}\"\npricing = \"none\"\n[endpoints.elsewhere]\nprovider = \"anthropic\"\nauth = \"api_key\"\nkey_env = \"ANTHROPIC_API_KEY\"\nurl = \"{}\"\npricing = \"none\"\n",
        server.url(),
        elsewhere.url()
    );
    home.write(CONFIG, &config);
    let url = server.url();
    let initial = [
        ("ANTHROPIC_API_KEY", API_KEY),
        ("ANTHROPIC_BASE_URL", url.as_str()),
    ];
    let flags = [
        "--model",
        "claude-sonnet-5-5",
        "--effort",
        "high",
        "--account",
        "work",
    ];
    let picked = seed(&home, &server, "the picked chat", &flags, &initial);
    let spend = home.file(".local/share/wisp/spend");
    std::fs::remove_dir_all(&spend).unwrap();
    home.write(
        CONFIG,
        &format!(
            "default = \"elsewhere\"\nmodel = \"claude-haiku-5-5\"\neffort = \"low\"\n{config}"
        ),
    );
    let elsewhere_url = elsewhere.url();
    let changed = [
        ("ANTHROPIC_API_KEY", API_KEY),
        ("ANTHROPIC_BASE_URL", elsewhere_url.as_str()),
        ("WISP_ENDPOINT", "elsewhere"),
    ];
    let log = sessions(&home).join(format!("{picked}.jsonl"));
    let kept = std::fs::read(&log).unwrap();
    let args = os(&[
        "--endpoint",
        "env",
        "--no-log",
        "--time",
        "utc",
        "--set",
        "effort=low",
        "--self-check",
    ]);
    let mut pty = full_screen_with(&home, &server, &args, &changed);
    pty.wait_for(READY);
    std::thread::sleep(Duration::from_millis(200));
    assert!(!spend.exists(), "--no-log saved spend");
    pty.master.write_all(b"/resume\r").unwrap();
    pty.wait_shown("the picked chat");
    pty.wait_shown(copy::NOT_SAVED);
    pty.master.write_all(b"\r").unwrap();
    pty.wait_shown("Resumed session");
    assert_hosted(&home, &pty);
    let relaunched = again(&mut pty, &server);
    pty.master.write_all(b"\x04").unwrap();
    wait_for_exit(&mut pty.child);
    drop(pty);
    assert_eq!(
        ids(&home),
        BTreeSet::from([picked.clone()]),
        "--no-log logged"
    );

    std::fs::write(&log, kept).unwrap();
    let mut pty = full_screen_with(&home, &server, &os(&["--resume", &picked]), &changed);
    pty.wait_shown("Resumed session");
    let fresh = again(&mut pty, &server);
    pty.master.write_all(b"\x04").unwrap();
    wait_for_exit(&mut pty.child);
    assert_eq!(relaunched, fresh);
    let bodies: Vec<serde_json::Value> = messages(&server)
        .iter()
        .map(|(_, body)| serde_json::from_str(body).unwrap())
        .collect();
    for body in &bodies {
        assert_eq!(body["model"], "claude-sonnet-5-5");
        assert_eq!(body["output_config"]["effort"], "high");
    }
    assert_prefix_stable(&bodies[..2], None);
    assert_prefix_stable(&[bodies[0].clone(), bodies[2].clone()], None);
    assert!(messages(&elsewhere).is_empty());
}

#[test]
fn explicit_resume_choices_win_over_the_logged_choices() {
    if !usable() {
        return;
    }
    let server = serve();
    let work = serve();
    let home = Home::new("resume-explicit-choices");
    home.write(CONFIG, &format!("[endpoints.work]\nprovider = \"anthropic\"\nauth = \"api_key\"\nkey_env = \"ANTHROPIC_API_KEY\"\nurl = \"{}\"\npricing = \"none\"\n", work.url()));
    let url = server.url();
    let env = [
        ("ANTHROPIC_API_KEY", API_KEY),
        ("ANTHROPIC_BASE_URL", url.as_str()),
    ];
    let initial = ["--model", "claude-sonnet-5-5", "--effort", "high"];
    let id = seed(&home, &server, "the chosen chat", &initial, &env);
    let log = sessions(&home).join(format!("{id}.jsonl"));
    let kept = std::fs::read(&log).unwrap();
    let before: serde_json::Value = serde_json::from_str(&messages(&server)[0].1).unwrap();
    for (flags, model, effort, account) in [
        (
            vec!["--effort", "low"],
            "claude-sonnet-5-5",
            "low",
            "gateway",
        ),
        (
            vec!["--model", "claude-opus-5-5"],
            "claude-opus-5-5",
            "high",
            "gateway",
        ),
        (
            vec!["--account", "work"],
            "claude-sonnet-5-5",
            "high",
            "work",
        ),
        (
            vec![
                "--model",
                "claude-opus-5-5",
                "--effort",
                "low",
                "--account",
                "work",
            ],
            "claude-opus-5-5",
            "low",
            "work",
        ),
    ] {
        std::fs::write(&log, &kept).unwrap();
        let mut args = vec!["--resume", id.as_str()];
        args.extend(flags);
        let selected = if account == "work" { &work } else { &server };
        let mut pty = full_screen_with(&home, &server, &os(&args), &env);
        pty.wait_shown("Resumed session");
        again(&mut pty, selected);
        pty.master.write_all(b"\x04").unwrap();
        wait_for_exit(&mut pty.child);
        let body: serde_json::Value =
            serde_json::from_str(&messages(selected).last().unwrap().1).unwrap();
        assert_prefix_stable(&[before.clone(), body.clone()], None);
        assert_eq!(body["model"], model, "{args:?}");
        let effective = body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find_map(|message| message["output_config"]["effort"].as_str())
            .unwrap_or_else(|| body["output_config"]["effort"].as_str().unwrap());
        assert_eq!(effective, effort, "{args:?}");
        let last = home
            .records()
            .into_iter()
            .rev()
            .find(|r| r["type"] == "turn_started")
            .unwrap();
        assert_eq!(last["endpoint_name"], account);
        assert_eq!(last["effort"], effort);
    }
}

#[test]
fn a_removed_model_on_resume_fails_without_substituting_the_default() {
    if !usable() {
        return;
    }
    let missing = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let gone = missing.clone();
    let server = super::serve_by(std::sync::Arc::new(move |_| {
        if gone.load(std::sync::atomic::Ordering::Relaxed) {
            super::json(
                "404 Not Found",
                r#"{"type":"error","error":{"type":"not_found_error","message":"no such model"}}"#,
            )
        } else {
            reply()
        }
    }));
    let home = Home::new("resume-removed-model");
    let url = server.url();
    let env = [
        ("ANTHROPIC_API_KEY", API_KEY),
        ("ANTHROPIC_BASE_URL", url.as_str()),
    ];
    let initial = ["--model", "claude-sonnet-5-5"];
    let id = seed(&home, &server, "before removal", &initial, &env);
    missing.store(true, std::sync::atomic::Ordering::Relaxed);
    let output = crate::support::home::run(
        home.golden_command(super::WISP)
            .args(["--resume", &id, "--output", "jsonl"])
            .envs(env),
        "again\n",
    );
    assert!(output.status.success(), "{}", super::both(&output));
    let ended: Vec<serde_json::Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|event| event["type"] == "turn_ended")
        .collect();
    assert_eq!(ended.len(), 1);
    let ended = &ended[0];
    assert_eq!(ended["outcome"]["status"], "error");
    assert_eq!(ended["outcome"]["detail"]["status"], 404);
    assert_eq!(ended["outcome"]["message"], "no such model");
    let bodies = messages(&server);
    assert_eq!(bodies.len(), 2);
    for (_, body) in bodies {
        let body: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["model"], "claude-sonnet-5-5");
    }
}

#[test]
fn a_chat_is_named_from_its_first_three_turns_and_the_picker_shows_the_name() {
    if !usable() {
        return;
    }
    let answer = |request: &Seen| {
        if is_count(&request.head) {
            counted_tokens(&request.body)
        } else {
            reply()
        }
    };
    let named = sse_reply(
        &serde_json::json!({"type": "text", "text": ""}),
        "Cache work",
    );
    let server = Server::naming(answer, named);
    let home = Home::new("resume-naming");
    let mut pty = full_screen_with(&home, &server, &[], &[]);
    pty.wait_for(READY);
    for (said, n) in ["the named chat", "a second message", "a third one"]
        .iter()
        .zip(1..)
    {
        std::thread::sleep(Duration::from_millis(200));
        pty.master
            .write_all(format!("{said}\r").as_bytes())
            .unwrap();
        pty.wait_screen("GOLDEN-DONE", |s| s.matches("GOLDEN-DONE").count() >= n);
        pty.wait_idle();
    }
    let log = || {
        std::fs::read_to_string(
            sessions(&home).join(format!("{}.jsonl", ids(&home).first().unwrap())),
        )
    };
    assert!(wait_until(PATIENCE, || log()
        .unwrap()
        .contains("\"session_named\"")));
    pty.master.write_all(b"\x04").unwrap();
    wait_for_exit(&mut pty.child);
    drop(pty);
    check("tui_naming", &server.named());
    let mut pty = full_screen_with(&home, &server, &os(&["--no-log"]), &[]);
    pty.wait_for(READY);
    std::thread::sleep(Duration::from_millis(200));
    pty.master.write_all(b"/resume\r").unwrap();
    pty.wait_shown("Cache work");
    pty.master.write_all(b"\x1b").unwrap();
    pty.wait_for(READY);
    std::thread::sleep(Duration::from_millis(200));
    pty.master.write_all(b"\x04").unwrap();
    wait_for_exit(&mut pty.child);
}

#[test]
fn with_logging_off_in_config_a_pick_shows_the_refusal_and_this_chat_stays() {
    if !usable() {
        return;
    }
    let server = serve();
    let home = Home::new("resume-log-off");
    seed(&home, &server, "an older chat", &[], &[]);
    let before = ids(&home);
    home.write(CONFIG, "log = false\n");
    let mut pty = picking(&home, &server, &[], &[]);
    pty.wait_for(READY);
    pty.wait_for("This chat can't stay open without its window: the session log is off.");
    std::thread::sleep(Duration::from_millis(200));
    pty.master.write_all(b"/resume\r").unwrap();
    pty.wait_shown("an older chat");
    pty.master.write_all(b"\r").unwrap();
    pty.wait_shown(copy::RESUME_NEEDS_LOG);
    pty.master.write_all(b"\x1b").unwrap();
    pty.wait_for(READY);
    std::thread::sleep(Duration::from_millis(200));
    pty.master.write_all(b"still here\r").unwrap();
    pty.wait_shown("GOLDEN-DONE");
    pty.wait_idle();
    pty.master.write_all(b"\x04").unwrap();
    wait_for_exit(&mut pty.child);
    assert_eq!(ids(&home), before, "nothing was logged");
}

#[test]
fn a_chat_taken_between_the_pick_and_the_reopen_reopens_the_start_picker_on_it() {
    if !usable() {
        return;
    }
    let server = serve();
    let home = Home::new("resume-race");
    seed(&home, &server, "the older chat", &[], &[]);
    let newer = seed(&home, &server, "the newer chat", &[], &[]);
    let mut pty = picking(&home, &server, &os(&["--resume"]), &[]);
    pty.wait_shown("the newer chat");
    let mut other = JsonlStore::open(sessions(&home)).unwrap();
    other.reopen(&newer, ResumeLock::Required).unwrap();
    pty.master.write_all(b"\r").unwrap();
    pty.wait_shown(copy::CONTINUE_THERE);
    pty.master.write_all(b"\x1b").unwrap();
    wait_for_exit(&mut pty.child);
    assert!(pty.child.wait().unwrap().success());
}

#[test]
fn a_chat_whose_account_is_gone_gets_the_resume_line_for_a_gone_account() {
    if !usable() {
        return;
    }
    let server = serve();
    let home = Home::new("resume-gone");
    home.write(
        CONFIG,
        &format!(
            "[endpoints.claude-team]\nprovider = \"anthropic\"\nauth = \"api_key\"\nkey_env = \"TEAM_KEY\"\nurl = \"{}\"\npricing = \"none\"\n",
            server.url()
        ),
    );
    let gone = seed(
        &home,
        &server,
        "on the team account",
        &["--endpoint", "claude-team"],
        &[
            ("TEAM_KEY", API_KEY),
            ("ANTHROPIC_API_KEY", API_KEY),
            ("ANTHROPIC_BASE_URL", &server.url()),
        ],
    );
    seed(
        &home,
        &server,
        "on the env account",
        &["--endpoint", "env"],
        &[],
    );
    home.write(CONFIG, "");
    let mut pty = picking(&home, &server, &os(&["--resume"]), &[]);
    pty.wait_shown("on the team account");
    pty.master.write_all(b"\x1b[B\r").unwrap();
    pty.wait_shown("Continuing on ● gateway");
    pty.wait_shown("cache rebuild ~$0.00012");
    assert_hosted(&home, &pty);
    let shown = pty.wait_shown(wisp_core::session::short_id(&gone));
    assert!(!shown.contains("is gone"), "{shown}");
    pty.master.write_all(b"\x04").unwrap();
    wait_for_exit(&mut pty.child);
}
