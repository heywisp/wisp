#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test on any socket or process error"
)]

use crate::support;

use std::process::Output;

use support::server::{Server, json};
use support::{
    WISP,
    home::{CONFIG, Home, KEYS_FILE, both, command_in, err, out, run},
};

const CANARY_KEY: &str = "sk-ant-CANARYSETTINGKEY0001";
const CANARY_NAME: &str = "canary0000000000000key1x";
const SONNET: &str = "claude-sonnet-5-5";
const OPUS: &str = "claude-opus-5-5";

fn serve() -> Server {
    Server::start(|_| json("200 OK", "{\"input_tokens\":5}\n"))
}

impl Server {
    fn models(&self) -> Vec<String> {
        self.bodies()
            .iter()
            .filter_map(|b| serde_json::from_str::<serde_json::Value>(b).ok())
            .filter_map(|v| v["model"].as_str().map(str::to_owned))
            .collect()
    }
}

impl Home {
    fn run_with(&self, args: &[&str], env: &[(&str, &str)], stdin: &str) -> Output {
        run(
            command_in(&self.0, WISP)
                .envs(env.iter().copied())
                .args(args),
            stdin,
        )
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_with(args, &[], "")
    }
}

fn code(output: &Output) -> i32 {
    output.status.code().unwrap_or(-1)
}

fn clean(output: &Output, home: &Home) {
    let text = both(output);
    assert!(!text.contains("CANARY"), "{text}");
    assert!(!text.contains("canary0"), "{text}");
    assert!(
        !text.contains(&home.0.display().to_string()),
        "a path in {text}"
    );
}

#[test]
fn latest_on_an_account_without_a_release_says_so_and_sends_nothing() {
    let server = serve();
    let home = Home::configured("latest-no-release");
    home.write(
        CONFIG,
        &format!(
            "[endpoints.work]\nauth = 'api_key'\nkey_env = 'LOCAL_KEY'\nurl = {:?}\n",
            server.url()
        ),
    );
    let output = home.run_with(
        &["account", "models", "work", "--latest"],
        &[("LOCAL_KEY", CANARY_KEY)],
        "",
    );
    assert_eq!(code(&output), 1);
    assert_eq!(
        err(&output),
        "wisp: work has no client release to look up\n"
    );
    assert!(out(&output).is_empty());
    assert!(server.heads().is_empty());
}

const COMMENTED: &str =
    "# my settings\neffort = \"high\" # thinking\n\n# display\n[display]\ntheme = \"dark\"\n";

#[test]
fn set_get_and_unset_keep_comments_and_say_what_changed() {
    let home = Home::configured("round-trip");
    home.write(CONFIG, COMMENTED);
    let set = home.run(&["config", "set", "effort", "low"]);
    assert!(set.status.success(), "{}", both(&set));
    assert_eq!(
        out(&set),
        "Saved effort = low (was high).\napplies to new sessions; a running Wisp keeps its settings (use /config there)\n"
    );
    assert_eq!(
        home.read(CONFIG),
        "# my settings\neffort = \"low\" # thinking\n\n# display\n[display]\ntheme = \"dark\"\n"
    );
    let get = home.run(&["config", "get", "effort"]);
    assert_eq!(out(&get), "low\n");
    let explain = home.run(&["config", "get", "effort", "--explain", "--effort", "max"]);
    assert_eq!(
        out(&explain),
        "effort = max\n  wins      --effort max\n  shadowed  effort = low\n            config.toml line 2\n  default   medium\n"
    );
    let before = home.run(&["--effort", "max", "config", "get", "effort", "--explain"]);
    assert_eq!(out(&before), out(&explain));
    let set = home.run(&["config", "set", "display.mouse", "off"]);
    assert!(set.status.success(), "{}", both(&set));
    assert!(
        home.read(CONFIG)
            .ends_with("[display]\ntheme = \"dark\"\nmouse = false\n")
    );
    let unset = home.run(&["config", "unset", "display.mouse"]);
    assert!(unset.status.success(), "{}", both(&unset));
    assert!(
        out(&unset).starts_with("Removed display.mouse (was off).\n"),
        "{}",
        out(&unset)
    );
    let unset = home.run(&["config", "unset", "effort"]);
    assert!(unset.status.success(), "{}", both(&unset));
    let left = home.read(CONFIG);
    assert!(left.contains("# my settings"), "{left}");
    assert!(
        left.contains("# display\n[display]\ntheme = \"dark\"\n"),
        "{left}"
    );
    assert!(!left.contains("effort"), "{left}");
    let again = home.run(&["config", "unset", "effort"]);
    assert_eq!(out(&again), "effort isn't saved; nothing to remove.\n");
}

#[test]
fn list_and_json_are_stable_and_show_per_model_values() {
    let home = Home::configured("list");
    home.write(
        CONFIG,
        "effort = \"high\"\n[models.\"claude-opus-5-5\"]\ncompact_at = \"60%\"\n",
    );
    let list = home.run(&["config", "list"]);
    assert!(list.status.success(), "{}", both(&list));
    let text = out(&list);
    assert!(
        text.contains("  effort                     high  (config.toml line 1)\n"),
        "{text}"
    );
    assert!(
        text.contains("    models.\"claude-opus-5-5\".compact_at = 60%  (config.toml line 3)\n"),
        "{text}"
    );
    assert!(!text.contains("store_id"), "{text}");
    assert!(!text.contains("  agent "), "{text}");
    assert_eq!(out(&home.run(&["config", "list"])), text);
    let json = home.run(&["config", "list", "effort", "--json"]);
    let value: serde_json::Value = serde_json::from_str(&out(&json)).unwrap();
    let names: Vec<&str> = value
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v["name"].as_str())
        .collect();
    assert_eq!(names, ["context.compact_effort", "effort"]);
    assert_eq!(value[1]["value"], "high");
    let get = home.run(&["config", "get", "effort", "--json"]);
    let value: serde_json::Value = serde_json::from_str(&out(&get)).unwrap();
    assert_eq!(value["layers"][0]["line"], 1);
}

#[test]
fn requests_in_flight_default_by_account_kind() {
    let home = Home::configured("in-flight");
    home.write(
        CONFIG,
        "store_id = \"0123456789abcdef\"\ndefault = \"claude-work\"\n\n[endpoints.claude-work]\nauth = \"anthropic_subscription\"\nkey_store = \"file\"\n\n[endpoints.direct]\nauth = \"api_key\"\nkey_env = \"DIRECT_KEY\"\n",
    );
    let path = "endpoints.claude-work.max_in_flight";
    assert_eq!(out(&home.run(&["config", "get", path])), "12\n");
    let explain = out(&home.run(&["config", "get", path, "--explain"]));
    assert!(explain.ends_with("  default   12\n"), "{explain}");
    let json = out(&home.run(&["config", "get", path, "--json"]));
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["value"], "12");
    assert_eq!(value["layers"], serde_json::json!([]));
    let direct = home.run(&["config", "get", "endpoints.direct.max_in_flight"]);
    assert_eq!(out(&direct), "8\n");
}

#[test]
fn secrets_are_refused_unread_and_never_echoed() {
    let home = Home::configured("secrets");
    let text = "[endpoints.manifest]\nauth = \"bearer\"\nkey_env = \"M\"\n";
    home.write(CONFIG, text);
    for args in [
        vec!["config", "set", "endpoints.manifest.key", CANARY_KEY],
        vec!["config", "set", "endpoints.manifest.key"],
        vec!["config", "unset", "endpoints.manifest.key"],
    ] {
        let output = home.run(&args);
        assert_eq!(code(&output), 2, "{}", both(&output));
        assert!(
            err(&output).starts_with("wisp: endpoints.manifest.key is a secret, so wisp config doesn't change it; use wisp account edit manifest.\n"),
            "{}",
            err(&output)
        );
        clean(&output, &home);
    }
    let with_value = home.run(&["config", "set", "endpoints.manifest.url", CANARY_KEY]);
    assert!(err(&with_value).contains("A key on the command line is saved in your shell history"));
    let as_var = home.run(&["config", "set", "endpoints.manifest.key_env", CANARY_NAME]);
    assert_eq!(code(&as_var), 2, "{}", both(&as_var));
    clean(&as_var, &home);
    assert_eq!(home.read(CONFIG), text);
}

#[test]
fn a_canary_name_is_never_quoted() {
    let home = Home::configured("names");
    for args in [
        vec!["config", "set", CANARY_KEY, "on"],
        vec!["config", "unset", CANARY_NAME],
        vec!["config", "get", CANARY_NAME],
        vec!["--set", &format!("{CANARY_NAME}=on"), "config", "path"],
    ] {
        let output = home.run(&args);
        assert_eq!(code(&output), 2, "{}", both(&output));
        assert_eq!(
            err(&output),
            "wisp: that isn't a setting name; see wisp config list\n"
        );
        clean(&output, &home);
    }
    let keyed = format!("models.\"{CANARY_KEY}\"");
    for (leaf, value, start) in [
        ("compact_at", "95x", "wisp: compact_at needs"),
        ("context_window", "10", "wisp: context_window needs"),
        (
            "context_window",
            "50000",
            "wisp: context_window 50000 for this model",
        ),
    ] {
        let output = home.run(&["config", "set", &format!("{keyed}.{leaf}"), value]);
        assert_eq!(code(&output), 2, "{}", both(&output));
        assert!(err(&output).starts_with(start), "{}", err(&output));
        clean(&output, &home);
    }
    assert_eq!(home.read(CONFIG), "");
    let near = home.run(&["config", "get", "effrot"]);
    assert_eq!(code(&near), 2);
    assert_eq!(
        err(&near),
        "wisp: unknown setting effrot; did you mean effort?\n"
    );
    let reserved = home.run(&["config", "set", "agents.reviewer.time", "off"]);
    assert_eq!(
        err(&reserved),
        "wisp: agents.reviewer.time is planned with agent types and can't be set yet; a preset goes in [agents.presets.reviewer] in config.toml\n"
    );
}

#[test]
fn a_main_preset_that_is_unknown_or_off_stops_the_start_with_its_error() {
    let home = Home::configured("main-preset");
    home.write(
        CONFIG,
        "[agents.presets.reviewer]\ndescription = \"Reviews\"\n\n[agents.presets.broken]\ndescription = \"B\"\ntools_off = [\"shwo_diff\"]\n",
    );
    let env = [
        ("ANTHROPIC_API_KEY", CANARY_KEY),
        ("ANTHROPIC_BASE_URL", "http://127.0.0.1:9"),
    ];
    for (preset, said) in [
        (
            "broken",
            "wisp: preset broken (line 4) is off: tools_off names shwo_diff, which is no tool; did you mean show_diff?\n",
        ),
        (
            "ghost",
            "wisp: there is no preset \"ghost\"; presets: reviewer\n",
        ),
    ] {
        let set = format!("preset={preset}");
        let start = home.run_with(&["--output", "jsonl", "--set", &set], &env, "hello\n");
        assert_eq!(code(&start), 1, "{}", both(&start));
        assert_eq!(err(&start), said);
    }
}

#[test]
fn agent_limits_are_set_by_command_and_presets_only_in_the_file() {
    let home = Home::configured("agents");
    let set = home.run(&["config", "set", "agents.max_running", "2"]);
    assert!(set.status.success(), "{}", both(&set));
    assert_eq!(home.read(CONFIG), "[agents]\nmax_running = 2\n");
    let get = home.run(&["config", "get", "agents.max_running"]);
    assert_eq!(out(&get), "2\n");
    let depth = home.run(&["config", "get", "agents.max_depth"]);
    assert_eq!(out(&depth), "2\n");
    let bad = home.run(&["config", "set", "agents.max_depth", "9"]);
    assert_eq!(code(&bad), 2, "{}", both(&bad));
    assert!(
        err(&bad).contains("must be a whole number from 1 to 5, or none"),
        "{}",
        err(&bad)
    );
    let preset = home.run(&["config", "set", "agents.presets.reviewer.effort", "high"]);
    assert_eq!(code(&preset), 2, "{}", both(&preset));
    assert_eq!(
        err(&preset),
        "wisp: presets live only in config.toml; edit [agents.presets.reviewer] there\n"
    );
    assert_eq!(home.read(CONFIG), "[agents]\nmax_running = 2\n");
    home.write(
        CONFIG,
        "[agents]\nmax_running = 2\n\n[agents.presets.reviewer]\ndescription = \"Reviews\"\neffort = \"high\"\n\n[agents.presets.broken]\ndescription = \"B\"\ntools_off = [\"shwo_diff\"]\n",
    );
    let doctor = home.run(&["--set", "agents.max_depth=3", "doctor"]);
    let shown = out(&doctor);
    let section = shown
        .split("\nagents\n")
        .nth(1)
        .and_then(|rest| rest.split("\n\n").next())
        .unwrap_or_default();
    assert_eq!(
        section,
        "  max_running        2 (config.toml line 2)\n  max_tree_running   none (default)\n  max_agents         50 (default)\n  max_depth          3 (flag)\n  preset reviewer    Reviews \u{b7} effort high\n  preset broken      off (config.toml line 8): tools_off names shwo_diff, which is no tool; did you mean show_diff?",
        "{shown}"
    );
    let none = home.run(&["config", "set", "agents.max_agents", "none"]);
    assert!(none.status.success(), "{}", both(&none));
    assert!(
        home.read(CONFIG).contains("max_agents = \"none\"\n"),
        "{}",
        home.read(CONFIG)
    );
    assert_eq!(
        out(&home.run(&["config", "get", "agents.max_agents"])),
        "none\n"
    );
    let doctor = home.run(&["--set", "agents.max_depth=none", "doctor"]);
    let shown = out(&doctor);
    assert!(
        shown.contains("  max_agents         none (config.toml line 3)\n"),
        "{shown}"
    );
    assert!(
        shown.contains("  max_depth          none (flag)\n"),
        "{shown}"
    );
    let word = home.run(&["config", "set", "agents.max_depth", "lots"]);
    assert_eq!(code(&word), 2, "{}", both(&word));
    assert!(
        err(&word).contains("must be a whole number from 1 to 5, or none"),
        "{}",
        err(&word)
    );
    for name in ["agents.max_running", "agents.max_agents"] {
        let unset = home.run(&["config", "unset", name]);
        assert!(unset.status.success(), "{}", both(&unset));
    }
    let left = home.read(CONFIG);
    assert!(!left.contains("max_running"), "{left}");
    assert!(!left.contains("[agents]\n"), "{left}");
    assert!(left.contains("[agents.presets.reviewer]"), "{left}");
}

#[test]
fn flags_and_set_give_one_value_each() {
    let home = Home::configured("flags");
    let both_given = home.run(&["--set", "effort=low", "--effort", "high", "config", "path"]);
    assert_eq!(code(&both_given), 2, "{}", both(&both_given));
    assert_eq!(
        err(&both_given),
        "wisp: --effort and --set effort both set effort; give one\n"
    );
    let twice = home.run(&[
        "--set",
        "effort=low",
        "--set",
        "effort=max",
        "config",
        "path",
    ]);
    assert_eq!(code(&twice), 2);
    assert_eq!(
        err(&twice),
        "wisp: --set effort was given more than once for effort\n"
    );
    let bad = home.run(&["--set", &format!("effort={CANARY_KEY}"), "config", "path"]);
    assert_eq!(code(&bad), 2);
    clean(&bad, &home);
    for set in [
        "models.\"m\".compact_at=50%",
        "endpoints.gw.pricing=none",
        "endpoints.gw.models.\"m\".web_search=none",
        "compact_at=50%",
    ] {
        let site = home.run(&["--set", set, "config", "path"]);
        assert!(site.status.success(), "{set}: {}", both(&site));
    }
    let connection = home.run(&["--set", "endpoints.gw.auth=bearer", "config", "path"]);
    assert_eq!(code(&connection), 2);
    assert_eq!(
        err(&connection),
        "wisp: endpoints.gw.auth is how the account connects, so --set doesn't change it for one run; use wisp account edit gw\n"
    );
    let bare = home.run(&["--set", "native_compaction=on", "config", "path"]);
    assert_eq!(code(&bare), 2);
    assert!(
        err(&bare).starts_with(
            "wisp: native_compaction is set per account or model; give its path, such as models.\""
        ),
        "{}",
        err(&bare)
    );
    let both_places = home.run(&[
        "--set",
        "models.\"m\".context_window=300000",
        "--set",
        "models.\"m\".context_window=400000",
        "config",
        "path",
    ]);
    assert_eq!(code(&both_places), 2);
    assert_eq!(
        err(&both_places),
        "wisp: --set models.\"m\".context_window was given more than once for models.\"m\".context_window\n"
    );
}

#[test]
fn a_point_is_saved_for_every_model_or_one_and_one_past_the_reserve_is_noted() {
    let home = Home::configured("compact");
    let bare = home.run(&["config", "set", "compact_at", "95%"]);
    assert!(bare.status.success(), "{}", both(&bare));
    assert!(out(&bare).starts_with("Saved compact_at = 95% (was not set).\n"));
    assert_eq!(
        err(&bare),
        "wisp: compact_at 95% is 950,000 tokens on claude-opus-5-5's window 1M (shipped); with that window Wisp compacts at 850,000 (window \u{2212} reserve).\n"
    );
    let low = home.run(&["config", "set", "compact_at", "10000"]);
    assert!(
        err(&low).starts_with("wisp: compact_at 10000 is below 40,000:"),
        "{}",
        both(&low)
    );
    let explain = home.run(&["config", "get", "compact_at", "--explain"]);
    assert!(
        out(&explain).contains("  wins      compact_at = 10,000\n"),
        "{}",
        both(&explain)
    );
    let unset = home.run(&["config", "unset", "compact_at"]);
    assert!(unset.status.success(), "{}", both(&unset));
    assert_eq!(home.read(CONFIG), "");
    let share = home.run(&[
        "config",
        "set",
        "models.\"claude-opus-5-5\".compact_at",
        "95%",
    ]);
    assert!(share.status.success(), "{}", both(&share));
    assert!(
        out(&share)
            .starts_with("Saved models.\"claude-opus-5-5\".compact_at = 95% (was not set).\n")
    );
    assert_eq!(
        err(&share),
        "wisp: compact_at 95% is 950,000 tokens on claude-opus-5-5's window 1M (shipped); with that window Wisp compacts at 850,000 (window \u{2212} reserve).\n"
    );
    let tokens = home.run(&[
        "config",
        "set",
        "models.\"claude-opus-5-5\".compact_at",
        "900000",
    ]);
    assert_eq!(
        err(&tokens),
        "wisp: compact_at 900,000 is past the reserve of claude-opus-5-5's window 1M (shipped); with that window Wisp compacts at 850,000 (window \u{2212} reserve).\n"
    );
    let fine = home.run(&[
        "config",
        "set",
        "models.\"claude-opus-5-5\".compact_at",
        "80%",
    ]);
    assert_eq!(err(&fine), "");
    let low = home.run(&[
        "config",
        "set",
        "models.\"claude-opus-5-5\".compact_at",
        "10000",
    ]);
    assert!(low.status.success(), "{}", both(&low));
    assert_eq!(
        err(&low),
        "wisp: compact_at 10000 is below 40,000: once the head after a compaction is above it, every turn starts with a compaction, ~$0.123 a turn to compact and ~$0.12 to write the head again, at claude-opus-5-5's prices.\nwisp: background compaction rarely runs below about 60,000, and claude-opus-5-5 compacts at 10,000; compaction will usually wait at the point.\n"
    );
    let off = home.run(&["config", "set", "context.auto_compact", "off"]);
    assert!(off.status.success(), "{}", both(&off));
    let quoted = home.run(&[
        "config",
        "set",
        "models.\"claude-opus-5-5\".compact_at",
        "95%",
    ]);
    assert!(
        err(&quoted).contains("with that window Wisp would compact at 850,000"),
        "{}",
        both(&quoted)
    );
    let unknown = home.run(&["config", "set", "models.\"made-up\".compact_at", "95%"]);
    assert!(unknown.status.success(), "{}", both(&unknown));
    assert_eq!(err(&unknown), "");
    let small = home.run(&["config", "set", "models.\"made-up\".context_window", "1000"]);
    assert_eq!(code(&small), 2);
    let on = home.run(&["config", "set", "context.auto_compact", "on"]);
    assert!(on.status.success(), "{}", both(&on));
    let window = home.run(&[
        "config",
        "set",
        "models.\"claude-opus-5-5\".context_window",
        "500000",
    ]);
    assert!(window.status.success(), "{}", both(&window));
    let capped = home.run(&[
        "config",
        "set",
        "models.\"claude-opus-5-5\".compact_at",
        "450000",
    ]);
    assert!(capped.status.success(), "{}", both(&capped));
    assert_eq!(
        err(&capped),
        "wisp: compact_at 450,000 is past the reserve of claude-opus-5-5's window 500k (config; documented 1M); with that window Wisp compacts at 425,000 (window \u{2212} reserve).\n"
    );
    let layered = Home::configured("shadowed-compact");
    layered.write(
        CONFIG,
        "[models.\"claude-opus-5-5\"]\ncompact_at = 200000\n",
    );
    let shadowed = layered.run(&["config", "set", "compact_at", "95%"]);
    assert!(shadowed.status.success(), "{}", both(&shadowed));
    assert_eq!(err(&shadowed), "");

    layered.write(
        CONFIG,
        "[models.\"claude-opus-5-5\"]\ncontext_window = 200000\nmax_output = 40000\n",
    );
    let default_limit = layered.run(&[
        "config",
        "set",
        "models.\"claude-opus-5-5\".compact_at",
        "160000",
    ]);
    assert!(default_limit.status.success(), "{}", both(&default_limit));
    assert_eq!(
        err(&default_limit),
        "wisp: compact_at 160,000 is past the reserve of claude-opus-5-5's window 200k (config; documented 1M); with that window Wisp compacts at 136,000 (window \u{2212} reserve).\n"
    );
}

#[test]
fn setting_the_band_rechecks_the_point() {
    let home = Home::configured("band");
    let window = home.run(&[
        "config",
        "set",
        "models.\"claude-opus-5-5\".context_window",
        "130000",
    ]);
    assert!(window.status.success(), "{}", both(&window));
    assert_eq!(err(&window), "");
    let wide = home.run(&["config", "set", "context.background_tokens", "60000"]);
    assert!(wide.status.success(), "{}", both(&wide));
    assert_eq!(
        err(&wide),
        "wisp: background compaction rarely runs below about 80,000, and claude-opus-5-5 compacts at 66,000; compaction will usually wait at the point.\n"
    );
    let narrow = home.run(&["config", "set", "context.background_tokens", "20000"]);
    assert_eq!(err(&narrow), "");
    let limit = home.run(&[
        "config",
        "set",
        "models.\"claude-opus-5-5\".context_window",
        "110000",
    ]);
    assert_eq!(err(&limit), "");
    let back = home.run(&["config", "set", "context.background_tokens", "40000"]);
    assert_eq!(
        err(&back),
        "wisp: background compaction rarely runs below about 60,000, and claude-opus-5-5 compacts at 46,000; compaction will usually wait at the point.\n"
    );
    let off = home.run(&[
        "config",
        "set",
        "models.\"claude-opus-5-5\".background_compaction",
        "false",
    ]);
    assert!(off.status.success(), "{}", both(&off));
    assert_eq!(err(&off), "");
    let again = home.run(&["config", "set", "context.background_tokens", "50000"]);
    assert_eq!(err(&again), "");
}

#[test]
fn a_given_config_path_is_never_echoed() {
    let home = Home::configured("given");
    let missing = home.file("nope/config.toml");
    let missing = missing.to_str().unwrap();
    for sub in [
        vec!["get", "effort"],
        vec!["list"],
        vec!["path"],
        vec!["unset", "effort"],
    ] {
        let mut args = vec!["--config", missing, "config"];
        args.extend(sub);
        let output = home.run(&args);
        assert!(!output.status.success());
        clean(&output, &home);
    }
    let output = home.run(&["--config", missing, "config", "set", "effort", "low"]);
    assert_eq!(
        err(&output),
        "wisp: the folder of the file given with --config doesn't exist\n"
    );
    assert!(!home.file("nope").exists());
    let here = home.file("given.toml");
    let here = here.to_str().unwrap();
    let get = home.run(&["--config", here, "config", "get", "effort"]);
    assert_eq!(
        err(&get),
        "wisp: the file given with --config doesn't exist\n"
    );
    let set = home.run(&["--config", here, "config", "set", "effort", "low"]);
    assert!(set.status.success(), "{}", both(&set));
    assert!(
        out(&set).contains("created the file given with --config\n"),
        "{}",
        out(&set)
    );
    clean(&set, &home);
    let path = home.run(&["--config", here, "config", "path"]);
    assert_eq!(out(&path), "the file given with --config\n");
    let default = home.run(&["config", "path"]);
    assert_eq!(out(&default), "~/.config/wisp/config.toml\n");
}

#[test]
fn refusals_name_the_default_file_from_home_and_never_a_given_one() {
    let home = Home::configured("refusals");
    let shown = "~/.config/wisp/config.toml";
    let cases: [(&[u8], &[&str], String); 4] = [
        (
            b"effort = \n",
            &["get", "effort"],
            format!("wisp: {shown} line 1 is not valid TOML; nothing was loaded\n"),
        ),
        (
            b"effort = \n",
            &["set", "effort", "low"],
            format!("wisp: {shown} line 1 is not valid TOML; nothing was loaded\n"),
        ),
        (
            b"[endpoints]\nx = 1\n",
            &["list"],
            format!("wisp: {shown} line 2: [endpoints] entries must be tables\n"),
        ),
        (
            b"\xff",
            &["get", "effort"],
            format!("wisp: {shown} is not valid UTF-8\n"),
        ),
    ];
    for (text, sub, said) in cases {
        std::fs::write(home.file(CONFIG), text).unwrap();
        let mut args = vec!["config"];
        args.extend(sub);
        let output = home.run(&args);
        assert!(!output.status.success(), "{}", both(&output));
        assert_eq!(err(&output), said, "{sub:?}");
        clean(&output, &home);
    }
    std::fs::remove_file(home.file(CONFIG)).unwrap();
    std::os::unix::fs::symlink(home.file("nowhere.toml"), home.file(CONFIG)).unwrap();
    let output = home.run(&["config", "set", "effort", "low"]);
    assert_eq!(
        err(&output),
        format!(
            "wisp: could not write {shown}: it is a link to a file that doesn't exist; create that file or replace the link. Nothing was changed\n"
        )
    );
    clean(&output, &home);
    let elsewhere = Home::new("config-elsewhere");
    let given = elsewhere.file("x.toml");
    std::fs::write(&given, "effort = \n").unwrap();
    let output = home.run(&[
        "--config",
        given.to_str().unwrap(),
        "config",
        "get",
        "effort",
    ]);
    assert_eq!(
        err(&output),
        "wisp: the file given with --config line 1 is not valid TOML; nothing was loaded\n"
    );
    assert!(
        !both(&output).contains(&elsewhere.0.display().to_string()),
        "{}",
        both(&output)
    );
}

#[test]
fn env_file_notes_name_the_default_file_from_home() {
    let home = Home::configured("env-notes");
    let env_file = KEYS_FILE;
    home.write(env_file, "X=1\n");
    let session = home.run_with(&["--output", "jsonl", "--time", "off"], &[], "hi\n");
    assert!(!session.status.success(), "{}", both(&session));
    assert!(
        err(&session).starts_with(
            "wisp: no Anthropic credentials: set ANTHROPIC_API_KEY (an API key) or ANTHROPIC_AUTH_TOKEN (a bearer token for a gateway) in the environment or ~/.config/wisp/.env\n"
        ),
        "{}",
        both(&session)
    );
    clean(&session, &home);
    std::fs::write(home.file(env_file), b"\xff").unwrap();
    let list = home.run(&["endpoint", "list"]);
    assert_eq!(err(&list), "wisp: ~/.config/wisp/.env is not valid UTF-8\n");
    clean(&list, &home);
    home.write(
        CONFIG,
        "default = \"main\"\n[endpoints.main]\nauth = \"bearer\"\nkey_env = \"MAIN_KEY\"\n[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW_KEY\"\nurl_env = \"GW_URL\"\n",
    );
    home.write(env_file, "GW_KEY=gw-token\n");
    let list = home.run(&["doctor"]);
    for line in [
        "unavailable: GW_URL is not set in ~/.config/wisp/.env, where GW_KEY was found",
        "unavailable: MAIN_KEY is not set in the environment or ~/.config/wisp/.env",
    ] {
        assert!(
            out(&list).lines().any(|l| l.ends_with(line)),
            "{line:?} in {}",
            both(&list)
        );
    }
    clean(&list, &home);
    home.write("gw.env", "GW_KEY=gw-token\n");
    let gw = home.file("gw.env");
    let import = home.run(&[
        "--env-file",
        gw.to_str().unwrap(),
        "endpoint",
        "import",
        "--no-check",
    ]);
    assert!(
        out(&import).contains("Skipped gw: GW_URL is not set in ~/gw.env, where GW_KEY was found"),
        "{}",
        both(&import)
    );
    clean(&import, &home);
    home.write("keys.env", "ANTHROPIC_API_KEY=file-token\n");
    let given = home.file("keys.env");
    let doctor = home.run(&["--env-file", given.to_str().unwrap(), "doctor"]);
    let line = "  warning            Using ANTHROPIC_API_KEY from ~/keys.env, not the default account \"main\" (--account main to use it)";
    assert!(out(&doctor).lines().any(|l| l == line), "{}", both(&doctor));
    clean(&doctor, &home);
}

#[test]
fn doctor_and_replay_name_paths_under_home_with_a_tilde() {
    let home = Home::configured("doctor-tools");
    std::fs::create_dir_all(home.file("bin")).unwrap();
    for tool in ["bash", "setsid"] {
        let Some(real) = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|dir| dir.join(tool))
            .find(|p| p.is_file())
        else {
            return;
        };
        std::os::unix::fs::symlink(real, home.file("bin").join(tool)).unwrap();
    }
    let path = std::env::join_paths(std::iter::once(home.file("bin")).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .unwrap();
    std::fs::create_dir_all(home.file("tmp")).unwrap();
    let tmp = home.file("tmp");
    let doctor = home.run_with(
        &["doctor"],
        &[
            ("PATH", path.to_str().unwrap()),
            ("TMPDIR", tmp.to_str().unwrap()),
        ],
        "",
    );
    let shown = out(&doctor);
    let rows: Vec<&str> = shown
        .lines()
        .filter(|l| {
            l.starts_with("  bash ")
                || (l.starts_with("  setsid ") && !l.starts_with("  setsid --ctty"))
        })
        .collect();
    assert_eq!(rows.len(), 2, "{}", both(&doctor));
    assert!(rows[0].contains(" ~/bin/bash ("), "{rows:?}");
    assert!(
        rows[1].contains(" ~/bin/setsid (each command runs in its own session)"),
        "{rows:?}"
    );
    assert!(
        out(&doctor)
            .lines()
            .any(|l| l == "  tool output        ~/tmp/wisp-<session id>"),
        "{}",
        both(&doctor)
    );
    clean(&doctor, &home);
    let replay = home.run(&["--replay", home.file("missing.jsonl").to_str().unwrap()]);
    assert_eq!(
        err(&replay),
        "wisp: could not read ~/missing.jsonl: entity not found\n"
    );
    home.write("broken.jsonl", "not json\n");
    let replay = home.run(&["--replay", home.file("broken.jsonl").to_str().unwrap()]);
    assert!(
        err(&replay).starts_with("wisp: ~/broken.jsonl: "),
        "{}",
        both(&replay)
    );
    clean(&replay, &home);
}

#[test]
fn a_data_folder_that_cannot_open_is_named_from_home() {
    let home = Home::bare("data-folder");
    home.dir(support::home::CONFIG_DIR);
    home.write(".local/share/wisp", "");
    let env = [
        ("ANTHROPIC_API_KEY", "test-key-not-used"),
        ("ANTHROPIC_BASE_URL", "http://127.0.0.1:9"),
    ];
    let session = home.run_with(&["--output", "jsonl", "--time", "off"], &env, "");
    assert!(
        err(&session).starts_with(
            "wisp: not logging this session: could not open ~/.local/share/wisp/sessions: "
        ),
        "{}",
        both(&session)
    );
    clean(&session, &home);
    let resume = home.run_with(&["--resume", "--output", "jsonl"], &env, "");
    assert_eq!(code(&resume), 1, "{}", both(&resume));
    assert!(
        err(&resume).starts_with("wisp: could not open ~/.local/share/wisp/sessions: "),
        "{}",
        both(&resume)
    );
    clean(&resume, &home);
}

#[test]
fn a_first_save_without_config_names_the_default_file_it_created() {
    let home = Home::configured("created-default");
    let set = home.run(&["config", "set", "effort", "low"]);
    assert!(set.status.success(), "{}", both(&set));
    let said = out(&set);
    assert!(
        said.contains("created ~/.config/wisp/config.toml\n"),
        "{said}"
    );
    assert!(!said.contains("--config"), "{said}");
    assert!(
        !both(&set).contains(home.0.to_str().unwrap()),
        "{}",
        both(&set)
    );
    assert!(home.file(CONFIG).exists());
    let again = home.run(&["config", "set", "effort", "high"]);
    assert!(again.status.success(), "{}", both(&again));
    assert!(!out(&again).contains("created"), "{}", out(&again));
}

#[test]
fn the_first_save_in_a_git_work_tree_notes_the_lock_once() {
    let home = Home::configured("lock");
    wisp_core::instructions::make_repository(&home.0).unwrap();
    let first = home.run(&["config", "set", "effort", "low"]);
    assert!(first.status.success(), "{}", both(&first));
    assert_eq!(err(&first).matches("note: ").count(), 1, "{}", err(&first));
    let printed = out(&first);
    let lines: Vec<&str> = printed.lines().collect();
    assert!(lines[0].starts_with("Saved effort"), "{lines:?}");
    let second = home.run(&["config", "set", "effort", "high"]);
    assert_eq!(err(&second), "");
}

#[test]
fn every_probe_uses_the_resolved_model() {
    let server = serve();
    let home = Home::configured("probe");
    home.write(
        CONFIG,
        &format!(
            "model = \"{SONNET}\"\n[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW_TOKEN\"\nurl = \"http://127.0.0.1:{}\"\n",
            server.port
        ),
    );
    let env = [("GW_TOKEN", "gw-token")];
    let saved = home.run_with(&["doctor", "--check", "gw"], &env, "");
    assert!(
        out(&saved).contains(&format!("count_tokens with {SONNET}")),
        "{}",
        both(&saved)
    );
    let flag = home.run_with(&["--model", OPUS, "doctor", "--check", "gw"], &env, "");
    assert!(
        out(&flag).contains(&format!("count_tokens with {OPUS}")),
        "{}",
        both(&flag)
    );
    assert_eq!(server.models(), [SONNET, OPUS]);
    let url = format!("http://127.0.0.1:{}", server.port);
    let add = home.run_with(
        &[
            "endpoint",
            "add",
            "second",
            "--key-stdin",
            "--url",
            &url,
            "--auth",
            "api_key",
        ],
        &[],
        "second-token\n",
    );
    assert!(add.status.success(), "{}", both(&add));
    let set = home.run_with(
        &[
            "--set",
            &format!("model={OPUS}"),
            "endpoint",
            "add",
            "third",
            "--key-stdin",
            "--url",
            &url,
            "--auth",
            "api_key",
        ],
        &[],
        "third-token\n",
    );
    assert!(set.status.success(), "{}", both(&set));
    assert_eq!(server.models(), [SONNET, OPUS, SONNET, OPUS]);
    let both_flags = home.run(&["--model", OPUS, "--set", "model=claude-x", "config", "path"]);
    assert_eq!(code(&both_flags), 2);
}

const TWO_ENDPOINTS: &str = "default = \"main\"\n[endpoints.main]\nauth = \"bearer\"\nkey_env = \"MAIN_KEY\"\n[endpoints.other]\nauth = \"bearer\"\nkey_env = \"OTHER_KEY\"\n";

#[test]
fn the_environment_takes_part_in_get_and_list() {
    let home = Home::configured("environment");
    home.write(
        CONFIG,
        &format!("{TWO_ENDPOINTS}[display]\ntheme = \"light\"\n"),
    );
    let keys = [("MAIN_KEY", "main-token"), ("OTHER_KEY", "other-token")];
    let mut env = keys.to_vec();
    env.push(("WISP_ENDPOINT", "other"));
    let chosen = home.run_with(&["config", "get", "endpoint", "--explain"], &env, "");
    assert!(out(&chosen).starts_with(
        "endpoint = other\n  wins      WISP_ENDPOINT=other\n  shadowed  default = main\n"
    ));
    let exported = home.run_with(
        &["config", "get", "endpoint", "--explain"],
        &[("ANTHROPIC_API_KEY", "exported-0000")],
        "",
    );
    assert!(
        out(&exported).starts_with(
            "endpoint = claude-api\n  wins      claude-api: ANTHROPIC_API_KEY is set in "
        ),
        "{}",
        both(&exported)
    );
    assert!(!both(&exported).contains("exported-0000"));
    let theme = home.run_with(
        &["config", "get", "display.theme", "--explain"],
        &[("WISP_THEME", "dark")],
        "",
    );
    assert!(
        out(&theme).starts_with(
            "display.theme = dark\n  wins      WISP_THEME=dark\n  shadowed  display.theme = light\n"
        ),
        "{}",
        both(&theme)
    );
}

#[test]
fn a_key_only_in_the_default_env_file_explains_as_the_start_rule() {
    let home = Home::configured("env-file");
    home.write(KEYS_FILE, "ANTHROPIC_API_KEY=sk-ant-api03-file-0000\n");
    let keys = [("MAIN_KEY", "main-token"), ("OTHER_KEY", "other-token")];
    home.write(
        CONFIG,
        "[endpoints.main]\nauth = \"bearer\"\nkey_env = \"MAIN_KEY\"\n[endpoints.other]\nauth = \"bearer\"\nkey_env = \"OTHER_KEY\"\n",
    );
    let unset = home.run_with(&["config", "get", "endpoint", "--explain"], &keys, "");
    assert_eq!(
        out(&unset),
        "endpoint = claude-api\n  default   claude-api (ANTHROPIC_API_KEY from ~/.config/wisp/.env)\n",
        "{}",
        both(&unset)
    );
    let plain = home.run_with(&["config", "get", "endpoint"], &keys, "");
    assert_eq!(out(&plain), "claude-api\n");
    let list = home.run_with(&["config", "list", "endpoint"], &keys, "");
    assert!(
        out(&list).contains("  endpoint                   claude-api  (default)\n"),
        "{}",
        both(&list)
    );
    home.write(
        CONFIG,
        "default = \"env\"\n[endpoints.main]\nauth = \"bearer\"\nkey_env = \"MAIN_KEY\"\n",
    );
    let chosen = [
        home.run_with(&["config", "get", "endpoint"], &keys, ""),
        home.run_with(&["config", "get", "endpoint", "--explain"], &keys, ""),
        home.run_with(&["config", "get", "endpoint", "--json"], &keys, ""),
        home.run_with(&["config", "list", "endpoint"], &keys, ""),
        home.run_with(&["doctor"], &keys, ""),
    ];
    for (run, shown) in chosen.iter().zip([
        "claude-api\n",
        "endpoint = claude-api\n  wins      default = claude-api\n",
        "\"value\":\"claude-api\"",
        "  endpoint                   claude-api  (config.toml line 1)\n",
        "  settings           endpoint = claude-api\n",
    ]) {
        assert!(out(run).contains(shown), "{shown}: {}", both(run));
        assert!(!out(run).contains("= env"), "{}", both(run));
    }
    home.write(CONFIG, TWO_ENDPOINTS);
    let saved = home.run_with(&["config", "get", "endpoint", "--explain"], &keys, "");
    assert!(
        out(&saved).starts_with("endpoint = main\n  wins      default = main\n"),
        "{}",
        both(&saved)
    );
    assert!(!both(&saved).contains("environment"), "{}", both(&saved));
    assert!(!both(&saved).contains("file-0000"));
}

const WRONG_PLACE: &str = "sk-ant-api03-CANARYWRONGPLACE0000";

const KEY_TO_LOWERCASE: &str = "sk-ant-api03-CANARYlower01";

#[test]
fn a_key_in_the_endpoint_variable_is_never_printed() {
    let home = Home::configured("wrong-place");
    home.write(CONFIG, TWO_ENDPOINTS);
    let env = [
        ("MAIN_KEY", "main-token"),
        ("OTHER_KEY", "other-token"),
        ("WISP_ENDPOINT", WRONG_PLACE),
    ];
    let explain = home.run_with(
        &[
            "--endpoint",
            "other",
            "config",
            "get",
            "endpoint",
            "--explain",
        ],
        &env,
        "",
    );
    assert_eq!(
        out(&explain),
        "endpoint = other\n  wins      --account other\n  shadowed  WISP_ENDPOINT=(looks like a key, not shown)\n  shadowed  default = main\n            config.toml line 1\n  default   the start rules\n",
        "{}",
        both(&explain)
    );
    for args in [
        &["--endpoint", "other", "config", "get", "endpoint", "--json"][..],
        &["--endpoint", "other", "config", "list"][..],
        &["--endpoint", "other", "config", "list", "--json"][..],
    ] {
        let run = home.run_with(args, &env, "");
        assert!(run.status.success(), "{}", both(&run));
        assert!(!both(&run).contains("CANARYWRONGPLACE"), "{}", both(&run));
    }
    let json = home.run_with(
        &["--endpoint", "other", "config", "get", "endpoint", "--json"],
        &env,
        "",
    );
    assert!(
        out(&json).contains("\"value\":\"(looks like a key, not shown)\""),
        "{}",
        both(&json)
    );
    assert!(
        out(&json).contains("\"layer\":\"variable\""),
        "{}",
        both(&json)
    );
    assert!(!both(&explain).contains("CANARYWRONGPLACE"));
    let keys = &env[..2];
    let lowercase_key = KEY_TO_LOWERCASE.to_ascii_lowercase();
    for flag in [WRONG_PLACE, &lowercase_key] {
        let explain = home.run_with(
            &["--endpoint", flag, "config", "get", "endpoint", "--explain"],
            keys,
            "",
        );
        assert!(
            out(&explain).starts_with(
                "endpoint = (looks like a key, not shown)\n  wins      --account (looks like a key, not shown)\n"
            ),
            "{}",
            both(&explain)
        );
        for args in [
            &["--endpoint", flag, "config", "get", "endpoint", "--json"][..],
            &["--endpoint", flag, "config", "list"][..],
            &["--endpoint", flag, "config", "list", "--json"][..],
            &["--endpoint", flag, "config", "get", "key", "--explain"][..],
        ] {
            let run = home.run_with(args, keys, "");
            assert!(run.status.success(), "{}", both(&run));
            assert!(
                !both(&run).to_lowercase().contains("canary"),
                "{}",
                both(&run)
            );
        }
    }
}

const KEY_AS_MODEL: &str = "sk-ant-api03-CANARYMODEL0000";
const HIDDEN: &str = "(looks like a key, not shown)";

fn never_names_the_key(run: &Output) {
    let text = both(run);
    assert!(!text.to_lowercase().contains("canary"), "{text}");
}

#[test]
fn a_key_given_as_the_model_is_never_printed() {
    let server = serve();
    let home = Home::configured("key-as-model");
    home.write(
        CONFIG,
        &format!(
            "[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW_TOKEN\"\nurl = \"http://127.0.0.1:{}\"\n",
            server.port
        ),
    );
    let env = [("GW_TOKEN", "gw-token")];
    let set = format!("model={KEY_AS_MODEL}");
    let lowercase_key = KEY_TO_LOWERCASE.to_ascii_lowercase();
    for (given, origin) in [
        (&["--model", KEY_AS_MODEL][..], "--model"),
        (&["--set", set.as_str()][..], "--set model"),
        (&["--model", &lowercase_key][..], "--model"),
    ] {
        for args in [
            &["config", "get", "model"][..],
            &["doctor", "--check", "gw"],
            &["--output", "jsonl"],
        ] {
            let all: Vec<&str> = given.iter().chain(args).copied().collect();
            let run = home.run_with(&all, &env, "hi\n");
            never_names_the_key(&run);
            assert_eq!(code(&run), 2, "{}", both(&run));
            assert_eq!(
                err(&run),
                format!(
                    "wisp: {origin}: this looks like a key, not a model ID, so it can't be used\n"
                )
            );
        }
    }
    assert!(server.models().is_empty());
    assert_eq!(home.read(CONFIG).matches("model").count(), 0);
}

#[test]
fn a_saved_key_as_the_model_is_never_printed_and_real_ids_still_are() {
    let server = serve();
    let home = Home::configured("saved-key-as-model");
    home.write(
        CONFIG,
        &format!(
            "model = \"{KEY_AS_MODEL}\"\n[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW_TOKEN\"\nurl = \"http://127.0.0.1:{}\"\n",
            server.port
        ),
    );
    let env = [("GW_TOKEN", "gw-token")];
    let explain = home.run(&["config", "get", "model", "--explain"]);
    assert_eq!(
        out(&explain),
        format!(
            "model = {HIDDEN}\n  wins      model = {HIDDEN}\n            config.toml line 1\n  default   claude-opus-5-5\n"
        ),
        "{}",
        both(&explain)
    );
    let list = home.run(&["config", "list"]);
    let listed = home.run(&["config", "list", "--json"]);
    let shadowed = home.run(&[
        "--model",
        "claude-sonnet-4-5-20250929",
        "config",
        "get",
        "model",
        "--explain",
    ]);
    assert!(
        out(&shadowed).starts_with(&format!(
            "model = claude-sonnet-4-5-20250929\n  wins      --model claude-sonnet-4-5-20250929\n  shadowed  model = {HIDDEN}\n"
        )),
        "{}",
        both(&shadowed)
    );
    let window = home.run_with(&["config", "get", "context_window", "--explain"], &env, "");
    assert!(
        out(&window).starts_with(&format!("context_window for {HIDDEN} on gw = ")),
        "{}",
        both(&window)
    );
    let doctor = home.run_with(&["doctor"], &env, "");
    assert!(
        out(&doctor).contains(&format!("  model              {HIDDEN}\n")),
        "{}",
        both(&doctor)
    );
    let check = home.run_with(&["doctor", "--check", "gw"], &env, "");
    assert!(
        out(&check).contains(&format!("count_tokens with {HIDDEN}")),
        "{}",
        both(&check)
    );
    for run in [&window, &doctor, &check] {
        never_names_the_key(run);
    }
    let replaced = home.run(&["config", "set", "model", "claude-sonnet-4-5-20250929"]);
    assert!(
        out(&replaced).starts_with(&format!(
            "Saved model = claude-sonnet-4-5-20250929 (was {HIDDEN}).\n"
        )),
        "{}",
        both(&replaced)
    );
    for run in [&explain, &list, &listed, &shadowed, &replaced] {
        assert!(run.status.success(), "{}", both(run));
        never_names_the_key(run);
    }
    for id in [
        "claude-sonnet-4-5-20250929",
        "us.anthropic.claude-3-7-sonnet-20250219-v1:0",
        "gpt-4o-mini-2024-07-18",
    ] {
        let get = home.run(&["--model", id, "config", "get", "model"]);
        assert_eq!(out(&get), format!("{id}\n"), "{}", both(&get));
    }
}

const KEY_SHAPED_ENDPOINT: &str = "prod-gateway-eu-west-2";

#[test]
fn a_configured_endpoint_whose_name_looks_like_a_key_takes_paths() {
    let home = Home::configured("key-shaped-path");
    let config = format!(
        "default = \"main\"\n[endpoints.main]\nauth = \"bearer\"\nkey_env = \"MAIN_KEY\"\n[endpoints.{KEY_SHAPED_ENDPOINT}]\nauth = \"bearer\"\nkey_env = \"MAIN_KEY\"\n"
    );
    home.write(CONFIG, &config);
    let env = [("MAIN_KEY", "main-token")];
    for (path, value) in [
        (format!("endpoints.{KEY_SHAPED_ENDPOINT}.pricing"), "none"),
        (
            format!("endpoints.{KEY_SHAPED_ENDPOINT}.models.\"claude-opus-5-5\".compact_at"),
            "60%",
        ),
    ] {
        let set = home.run_with(&["config", "set", &path, value], &env, "");
        assert!(set.status.success(), "{}", both(&set));
        assert!(
            out(&set).starts_with(&format!("Saved {path} = {value} (was not set).\n")),
            "{}",
            both(&set)
        );
        let get = home.run_with(&["config", "get", &path], &env, "");
        assert_eq!(out(&get), format!("{value}\n"), "{}", both(&get));
        let leaf = path.rsplit('.').next().unwrap();
        let list = home.run_with(&["config", "list", leaf], &env, "");
        assert!(
            out(&list).contains(&format!("    {path} = {value}")),
            "{}",
            both(&list)
        );
        let unset = home.run_with(&["config", "unset", &path], &env, "");
        assert!(unset.status.success(), "{}", both(&unset));
    }
    assert_eq!(home.read(CONFIG), config);
    let unknown = "prod-gateway-eu-west-9";
    let lowercase_key_path = format!(
        "endpoints.{}.pricing",
        KEY_TO_LOWERCASE.to_ascii_lowercase()
    );
    for args in [
        vec![
            "config",
            "set",
            "endpoints.prod-gateway-eu-west-9.pricing",
            "none",
        ],
        vec!["config", "get", "endpoints.prod-gateway-eu-west-9.pricing"],
        vec![
            "config",
            "unset",
            "endpoints.prod-gateway-eu-west-9.pricing",
        ],
        vec!["config", "set", &lowercase_key_path, "none"],
    ] {
        let refused = home.run_with(&args, &env, "");
        assert_eq!(code(&refused), 2, "{}", both(&refused));
        assert_eq!(
            err(&refused),
            "wisp: that isn't a setting name; see wisp config list\n"
        );
        assert!(!both(&refused).contains(unknown), "{}", both(&refused));
        never_names_the_key(&refused);
    }
    assert_eq!(home.read(CONFIG), config);
}

#[test]
fn values_saved_under_a_key_as_the_model_never_print_it() {
    let home = Home::configured("saved-under-key");
    home.write(
        CONFIG,
        &format!(
            "model = \"{KEY_AS_MODEL}\"\n[models.\"{KEY_AS_MODEL}\"]\ncontext_window = 300000\ncompact_at = \"60%\"\n[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW_TOKEN\"\n[endpoints.gw.models.\"{KEY_AS_MODEL}\"]\ncompact_at = \"50%\"\n"
        ),
    );
    let env = [("GW_TOKEN", "gw-token")];
    let run = |args: &[&str]| {
        let output = home.run_with(args, &env, "");
        never_names_the_key(&output);
        output
    };
    let list = run(&["config", "list"]);
    for line in [
        "    models.\"MODEL\".context_window = 300,000  (config.toml line 3)\n",
        "    models.\"MODEL\".compact_at = 60%  (config.toml line 4)\n",
        "    endpoints.gw.models.\"MODEL\".compact_at = 50%  (config.toml line 9)\n",
    ] {
        assert!(out(&list).contains(line), "{line}{}", both(&list));
    }
    let listed = run(&["config", "list", "--json"]);
    assert!(
        out(&listed).contains("\"path\":\"models.\\\"MODEL\\\".context_window\""),
        "{}",
        both(&listed)
    );
    let explain = run(&["config", "get", "compact_at", "--explain"]);
    assert_eq!(
        out(&explain),
        format!(
            "compact_at for {HIDDEN} on gw = 50%\n  wins      endpoints.gw.models.\"MODEL\".compact_at = 50%\n            config.toml line 9\n  shadowed  models.\"MODEL\".compact_at = 60%\n            config.toml line 4\n  default   window \u{2212} reserve\n"
        ),
        "{}",
        both(&explain)
    );
    let json = run(&["config", "get", "compact_at", "--json"]);
    assert!(
        out(&json).contains("\"origin\":\"endpoints.gw.models.\\\"MODEL\\\".compact_at\""),
        "{}",
        both(&json)
    );
    let doctor = run(&["doctor"]);
    assert!(
        out(&doctor).contains("models.\"MODEL\".context_window = 300,000"),
        "{}",
        both(&doctor)
    );
    let path = format!("models.\"{KEY_AS_MODEL}\".compact_at");
    let set = run(&["config", "set", &path, "99%"]);
    assert!(set.status.success(), "{}", both(&set));
    assert!(
        out(&set).starts_with("Saved models.\"MODEL\".compact_at = 99% (was 60%).\n"),
        "{}",
        both(&set)
    );
    assert!(
        err(&set).contains("compact_at 99% is 297,000 tokens on this model's window 300k (config)"),
        "{}",
        both(&set)
    );
    let got = run(&["config", "get", &path, "--explain"]);
    assert!(
        out(&got).contains("  wins      models.\"MODEL\".compact_at = 99%\n"),
        "{}",
        both(&got)
    );
    let unset = run(&["config", "unset", &path]);
    assert!(
        out(&unset).starts_with("Removed models.\"MODEL\".compact_at (was 99%).\n"),
        "{}",
        both(&unset)
    );
    let again = run(&["config", "unset", &path]);
    assert!(
        both(&again).contains("models.\"MODEL\".compact_at isn't saved; nothing to remove."),
        "{}",
        both(&again)
    );
    let small = Home::configured("saved-under-key-small");
    small.write(
        CONFIG,
        &format!("[models.\"{KEY_AS_MODEL}\"]\ncontext_window = 100000\n"),
    );
    let refused = small.run(&["config", "list"]);
    never_names_the_key(&refused);
    assert!(
        err(&refused).contains("context_window 100000 for this model puts the compaction point"),
        "{}",
        both(&refused)
    );
}

#[test]
fn set_takes_a_per_endpoint_path_never_quotes_a_key_shaped_one_and_names_a_typo_unknown() {
    let home = Home::configured("key-shaped-typo");
    let config = format!(
        "default = \"main\"\n[endpoints.main]\nauth = \"bearer\"\nkey_env = \"MAIN_KEY\"\n[endpoints.{KEY_SHAPED_ENDPOINT}]\nauth = \"bearer\"\nkey_env = \"MAIN_KEY\"\n"
    );
    home.write(CONFIG, &config);
    let env = [("MAIN_KEY", "main-token")];
    let set = home.run_with(
        &[
            "--set",
            "endpoints.main.pricing=none",
            "config",
            "get",
            "endpoints.main.pricing",
            "--explain",
        ],
        &env,
        "",
    );
    assert!(set.status.success(), "{}", both(&set));
    assert_eq!(
        out(&set),
        "pricing on main = none\n  wins      --set endpoints.main.pricing=none\n  default   list\n"
    );
    for endpoint in [KEY_SHAPED_ENDPOINT, "prod-gateway-eu-west-9"] {
        let set = format!("endpoints.{endpoint}.pricing=none");
        let refused = home.run_with(&["--set", &set, "config", "get", "pricing"], &env, "");
        assert_eq!(code(&refused), 2, "{}", both(&refused));
        assert!(!both(&refused).contains(endpoint), "{}", both(&refused));
    }
    for endpoint in ["main", KEY_SHAPED_ENDPOINT] {
        let path = format!("endpoints.{endpoint}.prcing");
        let typo = home.run_with(&["config", "set", &path, "none"], &env, "");
        assert_eq!(code(&typo), 2, "{}", both(&typo));
        assert_eq!(
            err(&typo),
            format!("wisp: unknown setting {path}; see wisp config list\n")
        );
    }
    assert_eq!(home.read(CONFIG), config);
}

#[test]
fn an_unknown_endpoint_variable_is_listed_as_the_layer_a_start_would_use() {
    let home = Home::configured("unknown-variable");
    home.write(CONFIG, TWO_ENDPOINTS);
    let keys = [("MAIN_KEY", "main-token"), ("OTHER_KEY", "other-token")];
    let mut env = keys.to_vec();
    env.push(("WISP_ENDPOINT", "work"));
    let explain = home.run_with(&["config", "get", "endpoint", "--explain"], &env, "");
    assert_eq!(
        out(&explain),
        "endpoint = work\n  wins      WISP_ENDPOINT=work\n  shadowed  default = main\n            config.toml line 1\n  default   the start rules\n",
        "{}",
        both(&explain)
    );
    let under = home.run_with(
        &[
            "--endpoint",
            "nowhere",
            "config",
            "get",
            "endpoint",
            "--explain",
        ],
        &env,
        "",
    );
    assert!(
        out(&under).starts_with(
            "endpoint = nowhere\n  wins      --account nowhere\n  shadowed  WISP_ENDPOINT=work\n"
        ),
        "{}",
        both(&under)
    );
    env.pop();
    env.push(("WISP_ENDPOINT", WRONG_PLACE));
    for args in [
        &["config", "get", "endpoint", "--explain"][..],
        &["config", "get", "endpoint", "--json"][..],
        &["config", "list"][..],
    ] {
        let run = home.run_with(args, &env, "");
        assert!(run.status.success(), "{}", both(&run));
        assert!(!both(&run).contains("CANARYWRONGPLACE"), "{}", both(&run));
        assert!(
            out(&run).contains("(looks like a key, not shown)"),
            "{}",
            both(&run)
        );
    }
}

#[test]
fn a_configured_name_that_looks_like_a_key_is_printed_in_full() {
    let name = "gateway-eu-central-1x";
    let home = Home::configured("key-shaped-name");
    home.write(
        CONFIG,
        &format!(
            "default = \"main\"\n[endpoints.main]\nauth = \"bearer\"\nkey_env = \"MAIN_KEY\"\n[endpoints.{name}]\nauth = \"bearer\"\nkey_env = \"OTHER_KEY\"\n"
        ),
    );
    let keys = [("MAIN_KEY", "main-token"), ("OTHER_KEY", "other-token")];
    let flagged = home.run_with(
        &["--endpoint", name, "config", "get", "endpoint", "--explain"],
        &keys,
        "",
    );
    assert!(
        out(&flagged).starts_with(&format!(
            "endpoint = {name}\n  wins      --account {name}\n"
        )),
        "{}",
        both(&flagged)
    );
    let mut env = keys.to_vec();
    env.push(("WISP_ENDPOINT", name));
    let under = home.run_with(
        &[
            "--endpoint",
            "main",
            "config",
            "get",
            "endpoint",
            "--explain",
        ],
        &env,
        "",
    );
    assert!(
        out(&under).starts_with(&format!(
            "endpoint = main\n  wins      --account main\n  shadowed  WISP_ENDPOINT={name}\n"
        )),
        "{}",
        both(&under)
    );
    for (args, env) in [
        (
            &["--endpoint", name, "config", "get", "endpoint", "--json"][..],
            &keys[..],
        ),
        (&["config", "get", "endpoint", "--json"][..], &env[..]),
        (&["--endpoint", name, "config", "list"][..], &keys[..]),
    ] {
        let run = home.run_with(args, env, "");
        assert!(run.status.success(), "{}", both(&run));
        assert!(out(&run).contains(name), "{}", both(&run));
        assert!(!both(&run).contains("not shown"), "{}", both(&run));
    }
}

#[test]
fn secret_rows_show_only_whether_they_are_set() {
    let home = Home::configured("secret-rows");
    home.write(
        CONFIG,
        &format!("{TWO_ENDPOINTS}[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW_KEY\"\nurl = \"https://gw.example.com/CANARYPATH\"\n"),
    );
    let env = [("MAIN_KEY", "sk-ant-CANARYMAINKEY0001")];
    let list = home.run_with(&["config", "list", "key"], &env, "");
    let text = out(&list);
    assert!(text.contains("    endpoints.main.key = set\n"), "{text}");
    assert!(
        text.contains("    endpoints.other.key = not set\n"),
        "{text}"
    );
    let url = home.run_with(&["config", "list", "url"], &env, "");
    assert!(
        out(&url).contains("    endpoints.gw.url = set\n"),
        "{}",
        out(&url)
    );
    assert!(
        out(&url).contains("    endpoints.main.url = not set\n"),
        "{}",
        out(&url)
    );
    assert_eq!(
        out(&home.run_with(&["config", "get", "endpoints.main.key"], &env, "")),
        "set\n"
    );
    assert_eq!(
        out(&home.run_with(&["config", "get", "endpoints.gw.url"], &env, "")),
        "set\n"
    );
    let json = home.run_with(&["config", "list", "--json"], &env, "");
    for output in [&list, &url, &json] {
        assert!(!both(output).contains("CANARY"), "{}", both(output));
    }
}

#[test]
fn real_model_ids_round_trip_through_their_paths() {
    let home = Home::configured("model-ids");
    home.write(
        CONFIG,
        "[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW_KEY\"\n",
    );
    for path in [
        "models.\"claude-sonnet-4-5-20250929\".compact_at",
        "endpoints.gw.models.\"anthropic/claude-sonnet-4.5\".compact_at",
    ] {
        let set = home.run(&["config", "set", path, "60%"]);
        assert!(set.status.success(), "{}", both(&set));
        assert!(
            out(&set).starts_with(&format!("Saved {path} = 60% (was not set).\n")),
            "{}",
            out(&set)
        );
        assert_eq!(out(&home.run(&["config", "get", path])), "60%\n");
        let list = out(&home.run(&["config", "list", "compact_at"]));
        assert!(list.contains(&format!("    {path} = 60%")), "{list}");
        let unset = home.run(&["config", "unset", path]);
        assert!(unset.status.success(), "{}", both(&unset));
        assert_ne!(out(&home.run(&["config", "get", path])), "60%\n");
    }
    assert_eq!(
        home.read(CONFIG),
        "[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW_KEY\"\n"
    );
}

#[test]
fn the_env_endpoint_key_shows_set_when_exported() {
    let home = Home::configured("env-key");
    let env = [("ANTHROPIC_API_KEY", "sk-ant-CANARYEXPORTED0001")];
    let explain = home.run_with(&["config", "get", "key", "--explain"], &env, "");
    assert!(
        out(&explain).starts_with("endpoints.claude-api.key = set\n"),
        "{}",
        both(&explain)
    );
    let list = home.run_with(&["config", "list", "key"], &env, "");
    assert!(
        out(&list).contains("    endpoints.claude-api.key = set\n"),
        "{}",
        out(&list)
    );
    let url = home.run_with(&["config", "get", "endpoints.claude-api.url"], &env, "");
    assert_eq!(out(&url), "not set\n", "{}", both(&url));
    let typed = home.run_with(&["config", "get", "endpoints.env.key"], &env, "");
    assert_eq!(out(&typed), "set\n", "{}", both(&typed));
    let flagged = home.run_with(
        &[
            "--set",
            "endpoints.env.native_web_search=false",
            "config",
            "get",
            "endpoints.claude-api.native_web_search",
            "--explain",
        ],
        &env,
        "",
    );
    assert!(
        out(&flagged).contains("  wins      --set endpoints.env.native_web_search=off\n"),
        "{}",
        both(&flagged)
    );
    let doctor = home.run_with(
        &["--set", "endpoints.env.native_web_search=false", "doctor"],
        &env,
        "",
    );
    assert!(
        out(&doctor).contains("  claude-api         off (off for this account) (web_search: shipped; native_web_search: flag)\n"),
        "{}",
        both(&doctor)
    );
    let none = home.run(&["config", "get", "endpoints.claude-api.key"]);
    assert_eq!(out(&none), "not set\n", "{}", both(&none));
    for output in [&explain, &list, &url, &typed, &flagged, &doctor] {
        clean(output, &home);
    }
}

#[test]
fn a_flag_under_a_model_or_its_saved_alias_wins_in_explain_and_doctor() {
    let home = Home::configured("saved-alias");
    home.write(
        CONFIG,
        "[models.\"my-model\"]\naliases = [\"my-alias\"]\ncontext_window = 500000\n\n[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW_KEY\"\nurl = \"https://gw.example.com\"\n",
    );
    let env = [("GW_KEY", "gw-token")];
    for given in ["my-model", "my-alias"] {
        let set = format!("models.\"{given}\".context_window=300000");
        let explain = home.run_with(
            &[
                "--model",
                "my-alias",
                "--set",
                &set,
                "config",
                "get",
                "context_window",
                "--explain",
            ],
            &env,
            "",
        );
        assert_eq!(
            out(&explain),
            format!(
                "context_window for my-alias on gw = 300,000\n  wins      --set models.\"{given}\".context_window=300,000\n  shadowed  models.\"my-model\".context_window = 500,000\n            config.toml line 3\n  default   the documented window\n"
            ),
            "{}",
            both(&explain)
        );
        let doctor = home.run_with(&["--model", "my-alias", "--set", &set, "doctor"], &env, "");
        assert!(
            out(&doctor).contains("  window             300k (flag)\n"),
            "{}",
            both(&doctor)
        );
    }
    let search = home.run_with(
        &[
            "--model",
            "claude-haiku-5-5",
            "--set",
            "endpoints.gw.native_web_search=on",
            "doctor",
        ],
        &env,
        "",
    );
    assert!(
        out(&search).contains("on (web_search: shipped; native_web_search: flag)"),
        "{}",
        both(&search)
    );
    let saved = |list: &str| {
        format!(
            "[models.\"claude-haiku-5-5\"]\nweb_search = {list}\n\n[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW_KEY\"\nurl = \"https://gw.example.com\"\n"
        )
    };
    let on_haiku = |config: &str, args: &[&str]| {
        home.write(CONFIG, config);
        home.run_with(&[&["--model", "claude-haiku-5-5"], args].concat(), &env, "")
    };
    let stray = saved("[\"web_search\"]");
    let explain = on_haiku(&stray, &["config", "get", "web_search", "--explain"]);
    assert_eq!(
        out(&explain),
        "web_search for claude-haiku-5-5 on gw = web_search_20250305\n  wins      web_search_20250305 (shipped, models.toml claude-haiku-5-5)\n  shadowed  models.\"claude-haiku-5-5\".web_search = web_search\n            config.toml line 2\n  default   the shipped fact, else none\n",
        "{}",
        both(&explain)
    );
    let list = on_haiku(&stray, &["config", "list", "web_search"]);
    assert!(
        out(&list).contains("  web_search                 web_search_20250305  (shipped)\n    models.\"claude-haiku-5-5\".web_search = web_search  (config.toml line 2)\n"),
        "{}",
        both(&list)
    );
    let doctor = on_haiku(&stray, &["doctor"]);
    let shipped = "  gw                 on (web_search: shipped; native_web_search: default)\n";
    assert!(out(&doctor).contains(shipped), "{}", both(&doctor));
    let owner = saved("[\"web_search\", \"web_search_20250305\"]");
    let explain = on_haiku(&owner, &["config", "get", "web_search", "--explain"]);
    assert_eq!(
        out(&explain),
        "web_search for claude-haiku-5-5 on gw = web_search, web_search_20250305\n  wins      models.\"claude-haiku-5-5\".web_search = web_search, web_search_20250305\n            config.toml line 2\n  shadowed  web_search_20250305 (shipped, models.toml claude-haiku-5-5)\n  default   the shipped fact, else none\n",
        "{}",
        both(&explain)
    );
    let doctor = on_haiku(&owner, &["doctor"]);
    let model = "  gw                 on (web_search: model; native_web_search: default)\n";
    assert!(out(&doctor).contains(model), "{}", both(&doctor));
}

#[test]
fn a_set_alias_that_another_row_already_uses_is_refused() {
    let home = Home::configured("alias-clash");
    home.write(
        CONFIG,
        "[models.\"model-a\"]\naliases = [\"shared-x\"]\ncontext_window = 500000\n",
    );
    let clash = home.run(&[
        "--set",
        "models.\"model-b\".aliases=shared-x",
        "config",
        "list",
    ]);
    assert_eq!(code(&clash), 2, "{}", both(&clash));
    assert_eq!(
        err(&clash),
        "wisp: shared-x names more than one model row; nothing was changed\n"
    );
    let flags_only = home.run(&[
        "--set",
        "models.\"model-b\".aliases=other",
        "--set",
        "models.\"model-c\".aliases=other",
        "config",
        "list",
    ]);
    assert_eq!(code(&flags_only), 2, "{}", both(&flags_only));
    let replaced = home.run(&[
        "--set",
        "models.\"model-a\".aliases=new-name",
        "--set",
        "models.\"model-b\".aliases=shared-x",
        "config",
        "list",
    ]);
    assert!(replaced.status.success(), "{}", both(&replaced));
}

const CATALOG_TOOLS: &str = "ask_secret, ask_user, bash, list_accounts, list_settings, manage, rename_chat, search, show_code, show_diff, spawn_agent, view_file, web_search, write_todos, or none";

#[test]
fn tool_names_are_checked_against_the_catalog_where_they_are_written() {
    let home = Home::configured("tool-names");
    let nope = home.run(&["config", "set", "tools", "nope"]);
    assert_eq!(code(&nope), 2, "{}", both(&nope));
    assert!(
        err(&nope).contains(&format!("nope is no tool; tools: {CATALOG_TOOLS}")),
        "{}",
        err(&nope)
    );
    let follower = home.run(&["config", "set", "tools", "process"]);
    assert_eq!(code(&follower), 2, "{}", both(&follower));
    assert!(
        err(&follower).contains("process comes with bash; name bash instead"),
        "{}",
        err(&follower)
    );
    let flag = home.run(&["--set", "tools=nope", "config", "get", "tools"]);
    assert_eq!(code(&flag), 2, "{}", both(&flag));
    assert!(
        err(&flag).contains(&format!(
            "--set tools: nope is no tool; tools: {CATALOG_TOOLS}"
        )),
        "{}",
        err(&flag)
    );
    for pasted in [
        vec!["config", "set", "tools", "ghp_CANARY0123456789abcdefABCDEF"],
        vec![
            "--set",
            "tools=ghp_CANARY0123456789abcdefABCDEF",
            "config",
            "get",
            "tools",
        ],
    ] {
        let output = home.run(&pasted);
        assert_eq!(code(&output), 2, "{}", both(&output));
        assert!(!both(&output).contains("CANARY"), "{}", both(&output));
        assert!(
            err(&output).contains("tools must be none or a list of tool names"),
            "{}",
            err(&output)
        );
    }
    let typed = home.run(&["--tools", "nope", "config", "get", "tools"]);
    assert_eq!(code(&typed), 2, "{}", both(&typed));
    for name in [
        "ask_secret",
        "ask_user",
        "bash",
        "write_todos",
        "web_search",
        "none",
    ] {
        assert!(err(&typed).contains(name), "{name}: {}", err(&typed));
    }
    assert!(!err(&typed).contains("process"), "{}", err(&typed));
    let mixed = home.run(&["--tools", "bash,none", "config", "get", "tools"]);
    assert_eq!(code(&mixed), 2, "{}", both(&mixed));
    assert!(
        err(&mixed).contains("--tools takes none on its own"),
        "{}",
        err(&mixed)
    );
    for (alone, reason) in [
        (
            "list_agents",
            "list_agents comes with spawn_agent; name spawn_agent instead",
        ),
        (
            "report",
            "report is for child agents only; spawn_agent gives it to every child",
        ),
    ] {
        for args in [
            vec!["--tools", alone, "config", "get", "tools"],
            vec!["config", "set", "tools", alone],
        ] {
            let output = home.run(&args);
            assert_eq!(code(&output), 2, "{}", both(&output));
            assert!(err(&output).contains(reason), "{}", err(&output));
        }
    }
    for given in ["spawn_agent,list_agents", "spawn_agent,report"] {
        let beside = home.run(&["--tools", given, "config", "get", "tools"]);
        assert!(beside.status.success(), "{}", both(&beside));
        assert!(!both(&beside).contains("list_agents"), "{}", both(&beside));
        assert!(!both(&beside).contains("report"), "{}", both(&beside));
        assert!(err(&beside).is_empty(), "{}", err(&beside));
    }
    assert!(home.read(CONFIG).is_empty());
    let todos = home.run(&["config", "set", "tools", "write_todos"]);
    assert!(todos.status.success(), "{}", both(&todos));
    assert_eq!(home.read(CONFIG), "tools = [\"write_todos\"]\n");
    let defaults = "ask_secret,ask_user,bash,list_accounts,list_settings,manage,rename_chat,search,show_code,show_diff,spawn_agent,view_file,web_search,write_todos";
    let applies = "applies to new sessions; a running Wisp keeps its settings (use /config there)";
    for (given, was) in [
        (
            format!("{defaults},list_agents,process,send_message,stop_agent"),
            "write_todos",
        ),
        (defaults.to_owned(), "not set"),
    ] {
        let reset = home.run(&["config", "set", "tools", &given]);
        assert!(reset.status.success(), "{}", both(&reset));
        assert_eq!(
            out(&reset),
            format!(
                "Saved tools as the default (was {was}), so new tools come on too.\n{applies}\n"
            )
        );
        assert!(err(&reset).is_empty(), "{}", err(&reset));
        assert!(
            !home.read(CONFIG).contains("tools"),
            "{}",
            home.read(CONFIG)
        );
    }
    let fewer = defaults.trim_end_matches(",write_todos");
    let more = home.run(&["config", "set", "tools", fewer]);
    assert!(more.status.success(), "{}", both(&more));
    assert!(
        out(&more).starts_with("Saved tools = ask_secret, ask_user,"),
        "{}",
        out(&more)
    );
    let saved = home.read(CONFIG);
    assert!(
        saved.contains("\"ask_secret\"")
            && saved.contains("\"ask_user\"")
            && !saved.contains("write_todos"),
        "{saved}"
    );
    let fresh = Home::configured("tool-names-fresh");
    let first = fresh.run(&["config", "set", "tools", defaults]);
    assert!(first.status.success(), "{}", both(&first));
    assert!(!fresh.exists(CONFIG), "{}", fresh.read(CONFIG));
    let off = fresh.run(&["config", "set", "plugins.todos.enabled", "off"]);
    assert!(off.status.success(), "{}", both(&off));
    let rest = defaults.trim_end_matches(",write_todos");
    let without = fresh.run(&["config", "set", "tools", rest]);
    assert!(
        out(&without).starts_with("Saved tools as the default (was not set)"),
        "{}",
        both(&without)
    );
    assert!(
        !fresh.read(CONFIG).contains("tools"),
        "{}",
        fresh.read(CONFIG)
    );
}

#[test]
fn a_saved_tool_the_catalog_lacks_still_loads_and_stays_in_the_file() {
    let home = Home::configured("tool-gone");
    home.write(CONFIG, "tools = [\"bash\", \"telepathy\"]\n");
    let get = home.run(&["config", "get", "tools"]);
    assert!(get.status.success(), "{}", both(&get));
    assert_eq!(out(&get), "bash, telepathy\n");
    let effort = home.run(&["config", "set", "effort", "low"]);
    assert!(effort.status.success(), "{}", both(&effort));
    assert!(
        home.read(CONFIG)
            .contains("tools = [\"bash\", \"telepathy\"]"),
        "{}",
        home.read(CONFIG)
    );
    home.write(
        CONFIG,
        "tools = [\"bash\", \"opaque_CANARY0123456789abcdefABCDEF\"]\n",
    );
    let get = home.run(&["config", "get", "tools"]);
    assert!(get.status.success(), "{}", both(&get));
    assert!(!both(&get).contains("CANARY"), "{}", both(&get));
    assert_eq!(out(&get), "bash, (looks like a key, not shown)\n");
}

#[test]
fn a_hook_switch_names_a_known_hook_and_a_saved_one_reads_back() {
    let home = Home::configured("hook-switch");
    let set = home.run(&["config", "set", "plugins.aa.hooks.x", "off"]);
    assert_eq!(code(&set), 2, "{}", both(&set));
    assert_eq!(
        err(&set),
        "wisp: plugins.aa.hooks.x: aa/x is no hook; hooks: agent-viz/mail, idle/done, idle/note, idle/seen, mcp/compacted, mcp/list, mcp/refresh, mcp/rewound, shell/processes, skills/compacted, skills/nudge, skills/rewound, skills/scan\n"
    );
    let flagged = home.run(&["--set", "plugins.aa.hooks.x=off", "config", "get", "time"]);
    assert_eq!(code(&flagged), 2, "{}", both(&flagged));
    home.write(CONFIG, "[plugins.aa.hooks]\nx = false\n");
    let list = home.run(&["config", "list"]);
    assert!(list.status.success(), "{}", both(&list));
    assert!(!out(&list).contains("plugins."), "{}", out(&list));
    let notice = home.run(&["config", "set", "plugins.shell.hooks.processes", "off"]);
    assert!(notice.status.success(), "{}", both(&notice));
    assert!(
        home.read(CONFIG).contains("processes = false"),
        "{}",
        home.read(CONFIG)
    );
}

#[test]
fn plugin_switches_write_the_config_and_refuse_an_unknown_id_or_a_path() {
    let home = Home::configured("plugin-switches");
    for args in [
        ["plugin", "disable", "time"],
        ["plugin", "remove", "context"],
    ] {
        let run = home.run(&args);
        assert!(run.status.success(), "{}", both(&run));
        clean(&run, &home);
    }
    assert_eq!(
        home.read(CONFIG),
        "[plugins.time]\nenabled = false\n\n[plugins.context]\ninstalled = false\n"
    );
    let listed = out(&home.run(&["plugin", "list"]));
    assert!(listed.contains("time 0.1.0 off\n"), "{listed}");
    assert!(listed.contains("context 0.1.0 removed\n"), "{listed}");
    for args in [["plugin", "enable", "time"], ["plugin", "add", "context"]] {
        let run = home.run(&args);
        assert!(run.status.success(), "{}", both(&run));
    }
    assert_eq!(
        home.read(CONFIG),
        "[plugins.time]\nenabled = true\n\n[plugins.context]\ninstalled = true\n"
    );
    let before = home.read(CONFIG);
    for (args, said) in [
        (
            ["plugin", "disable", "nope"],
            "wisp: no plugin is named nope; plugins: ",
        ),
        (
            ["plugin", "add", "./mine.wasm"],
            "wisp: ./mine.wasm isn't a plugin id\n",
        ),
    ] {
        let run = home.run(&args);
        assert_eq!(code(&run), 2, "{}", both(&run));
        assert!(err(&run).starts_with(said), "{}", err(&run));
        clean(&run, &home);
    }
    assert_eq!(home.read(CONFIG), before);
}

#[test]
fn enabling_self_check_with_its_setting_off_says_so() {
    let home = Home::configured("plugin-self-check");
    let run = home.run(&["plugin", "enable", "self-check"]);
    assert!(run.status.success(), "{}", both(&run));
    assert!(
        out(&run)
            .ends_with("self-check is on; its hooks follow self_check.before_end, which is off\n"),
        "{}",
        out(&run)
    );
    home.run(&["config", "set", "self_check.before_end", "on"]);
    let run = home.run(&["plugin", "enable", "self-check"]);
    assert!(!out(&run).contains("which is off"), "{}", out(&run));
}

const PLUGIN_LIST: &[&str] = &[
    "agent-viz 0.1.0 on\n",
    "  Draws your agents as a topology\n",
    "agents 0.1.0 on\n",
    "  Named child agents that report back and message each other\n",
    "  tools: list_agents, rename_agent, report, send_message, spawn_agent, stop_agent\n",
    "anthropic 0.1.0 on\n",
    "  Your Claude subscription and Claude API accounts\n",
    "ask-user 0.1.0 on\n",
    "  Asks the user multiple-choice questions\n",
    "  tools: ask_user\n",
    "chat 0.1.0 on\n",
    "  Renames this chat\n",
    "  tools: rename_chat\n",
    "compaction 0.1.0 on\n",
    "  Wisp's six-section summary and compaction head layout\n",
    "  settings: context.verbatim_user_tokens\n",
    "context 0.1.0 off\n",
    "  Tells the model how full its context is\n",
    "  settings: context.context_note\n",
    "gemini 0.1.0 on\n",
    "  Your Antigravity subscription and Gemini API accounts\n",
    "idle 0.1.0 on\n",
    "  Ends turns without a reply while the user is idle\n",
    "instructions 0.1.0 on\n",
    "  Puts the AGENTS.md files in the system prompt\n",
    "  settings: instructions.source, instructions.warn_tokens\n",
    "mcp 0.1.0 on\n",
    "  Tools, prompts and resources from your MCP servers\n",
    "menus 0.1.0 on\n",
    "  Lets the model read what /config, /account and /usage show\n",
    "  tools: list_accounts, list_settings\n",
    "openai 0.1.0 on\n",
    "  Your ChatGPT subscription and OpenAI API accounts\n",
    "search 0.1.0 on\n",
    "  Find tools, skills and documents when the model needs them\n",
    "  tools: manage, search, use\n",
    "secret 0.1.0 on\n",
    "  Types a secret from the user into a running process\n",
    "  tools: ask_secret\n",
    "self-check 0.1.0 on\n",
    "  Asks the model to check its work before a turn ends\n",
    "  settings: self_check.before_end\n",
    "shell 0.1.0 on\n",
    "  Runs shell commands without asking, and their processes\n",
    "  tools: bash, process\n",
    "show-diff 0.1.0 on\n",
    "  Shows the user code or a change, changing nothing\n",
    "  tools: show_code, show_diff\n",
    "skills 0.1.0 on\n",
    "  Finds and reads how-to skills when they are needed\n",
    "  settings: turned_off\n",
    "time 0.1.0 removed\n",
    "  Stamps each message with the time it was sent\n",
    "  settings: time\n",
    "todos 0.1.0 on\n",
    "  Keeps the agent's checklist for multi-step work\n",
    "  tools: write_todos\n",
    "view-file 0.1.0 on\n",
    "  Attaches a file from the user's computer\n",
    "  tools: view_file\n",
    "web-search 0.1.0 on\n",
    "  Provider web search, where the account and model have it\n",
    "  tools: web_search\n",
    "workspace 0.1.0 on\n",
    "  Tells the model where it works\n",
];

#[test]
fn every_provider_has_a_bundled_plugin_that_declares_only_its_identity() {
    use wisp_plugin_api::manifest::{Manifest, Origin};

    let bundled = wisp_plugins::bundled();
    for id in wisp_protocol::settings::PROVIDERS {
        let manifest = bundled
            .iter()
            .find_map(|plugin| {
                let manifest = Manifest::parse(plugin.manifest, Origin::Bundled).unwrap();
                (manifest.id == *id).then_some(manifest)
            })
            .unwrap();
        assert!(manifest.settings.is_empty());
        assert!(manifest.system.is_none());
        assert!(manifest.tools.is_empty());
        assert!(manifest.commands.is_empty());
        assert!(manifest.hooks.is_empty());
        assert!(manifest.points.is_empty());
        assert_eq!(manifest.capabilities, Default::default());
        assert!(manifest.slots.is_empty());
        assert!(manifest.prompt_sections.is_empty());
        assert!(manifest.strategy.is_none());
        assert!(!manifest.index);
        assert!(manifest.dynamic_tools.is_none());
    }
}

#[test]
fn the_plugin_list_shows_every_bundled_plugin_with_its_saved_state() {
    let home = Home::configured("plugin-list");
    for args in [
        ["plugin", "remove", "time"],
        ["plugin", "disable", "context"],
    ] {
        let run = home.run(&args);
        assert!(run.status.success(), "{}", both(&run));
    }
    let run = home.run(&["plugin", "list"]);
    assert!(run.status.success(), "{}", both(&run));
    assert_eq!(out(&run), PLUGIN_LIST.concat());
    clean(&run, &home);
}

#[test]
fn setting_a_removed_plugins_setting_notes_that_it_waits_for_the_add_back() {
    let home = Home::configured("plugin-removed-setting");
    let run = home.run(&["plugin", "remove", "time"]);
    assert!(run.status.success(), "{}", both(&run));
    let run = home.run(&["config", "set", "time", "off"]);
    assert!(run.status.success(), "{}", both(&run));
    assert!(
        err(&run).ends_with(
            "wisp: time belongs to the plugin time, which is removed; it applies once wisp plugin add time brings it back\n"
        ),
        "{}",
        both(&run)
    );
    let run = home.run(&["plugin", "add", "time"]);
    assert!(run.status.success(), "{}", both(&run));
    let run = home.run(&["config", "set", "time", "on"]);
    assert!(!err(&run).contains("is removed"), "{}", both(&run));
}

#[test]
fn the_compaction_and_instructions_settings_belong_to_their_plugins() {
    let home = Home::configured("plugin-removed-owned");
    for (id, setting, value) in [
        ("compaction", "context.verbatim_user_tokens", "4000"),
        ("instructions", "instructions.warn_tokens", "4000"),
    ] {
        let run = home.run(&["plugin", "remove", id]);
        assert!(run.status.success(), "{}", both(&run));
        let run = home.run(&["config", "set", setting, value]);
        assert!(run.status.success(), "{}", both(&run));
        assert!(
            err(&run).ends_with(&format!(
                "wisp: {setting} belongs to the plugin {id}, which is removed; it applies once wisp plugin add {id} brings it back\n"
            )),
            "{}",
            both(&run)
        );
    }
}

const PLUGIN_HOOKS: &[&str] = &[
    "after-compact\n",
    "  mcp/compacted \u{b7} on\n",
    "  skills/compacted \u{b7} on\n",
    "after-response\n",
    "  self-check/worked \u{b7} follows self_check.before_end \u{b7} off\n",
    "after-turn\n",
    "  idle/done \u{b7} on\n",
    "agent-mail\n",
    "  agent-viz/mail \u{b7} on\n",
    "before-end\n",
    "  self-check/review \u{b7} follows self_check.before_end \u{b7} off\n",
    "before-turn\n",
    "  mcp/refresh \u{b7} in core/state \u{b7} on\n",
    "  skills/scan \u{b7} in core/state \u{b7} on\n",
    "  workspace/environment \u{b7} prompt section\n",
    "  instructions/instructions \u{b7} prompt section\n",
    "  workspace/terminal \u{b7} prompt section\n",
    "  core/tools\n",
    "  skills/nudge \u{b7} in core/notes \u{b7} on\n",
    "  time/sent \u{b7} in core/notes \u{b7} follows time \u{b7} off\n",
    "  idle/note \u{b7} in core/notes \u{b7} on\n",
    "  agents/kept-warm \u{b7} prompt section\n",
    "  shell/processes \u{b7} in core/news \u{b7} continues core/processes \u{b7} off\n",
    "  core/typed\n",
    "rewound\n",
    "  mcp/rewound \u{b7} on\n",
    "  skills/rewound \u{b7} on\n",
    "session-start\n",
    "  mcp/list \u{b7} on\n",
    "user-input\n",
    "  idle/seen \u{b7} on\n",
];

#[test]
fn the_hook_list_shows_each_points_order_and_each_hooks_saved_state() {
    let home = Home::configured("plugin-hooks");
    for args in [
        &["config", "set", "plugins.shell.hooks.processes", "off"][..],
        &["config", "set", "time", "off"],
        &["plugin", "remove", "context"],
    ] {
        let run = home.run(args);
        assert!(run.status.success(), "{}", both(&run));
    }
    let run = home.run(&["plugin", "hooks"]);
    assert!(run.status.success(), "{}", both(&run));
    assert_eq!(out(&run), PLUGIN_HOOKS.concat());
    clean(&run, &home);
}

#[test]
fn a_declared_row_is_set_saved_listed_and_refused_out_of_range() {
    let home = Home::configured("declared-row");
    home.write(CONFIG, "default = \"local\"\n[endpoints.local]\nauth = \"api_key\"\nkey_env = \"LOCAL_KEY\"\nurl = \"http://127.0.0.1:9\"\n");
    let env = [
        ("WISP_TESTKIT", "settings"),
        ("LOCAL_KEY", "CANARY_LOCAL_SETTING_TEST"),
    ];
    let doctor = home.run_with(&["doctor"], &env, "");
    assert!(doctor.status.success(), "{}", both(&doctor));
    let set = home.run_with(
        &["config", "set", "plugins.testkit.minutes", "25"],
        &env,
        "",
    );
    assert!(set.status.success(), "{}", both(&set));
    assert!(
        home.read(CONFIG)
            .contains("[plugins.testkit]\nminutes = 25")
    );
    let get = home.run_with(
        &[
            "--set",
            "plugins.testkit.minutes=35",
            "config",
            "get",
            "plugins.testkit.minutes",
        ],
        &env,
        "",
    );
    assert!(get.status.success(), "{}", both(&get));
    assert!(out(&get).contains("35"), "{}", both(&get));
    let list = home.run_with(&["config", "list", "--json"], &env, "");
    assert!(list.status.success(), "{}", both(&list));
    let rows: serde_json::Value = serde_json::from_str(&out(&list)).unwrap();
    assert!(
        rows.as_array()
            .unwrap()
            .iter()
            .any(|row| row["name"] == "plugins.testkit.minutes"
                && row["value"]
                    .as_str()
                    .is_some_and(|value| value.contains("25"))),
        "{}",
        out(&list)
    );
    let bad = home.run_with(
        &["config", "set", "plugins.testkit.minutes", "61"],
        &env,
        "",
    );
    assert_eq!(code(&bad), 2, "{}", both(&bad));
    assert!(err(&bad).contains("60"), "{}", both(&bad));
    assert!(home.read(CONFIG).contains("minutes = 25"));
}

#[test]
fn doctor_shows_a_configured_mcp_server_without_starting_it() {
    let home = Home::configured("doctor-mcp");
    let config = "default = 'local'\n[endpoints.local]\nauth = 'api_key'\nkey_env = 'LOCAL_KEY'\nurl = 'http://127.0.0.1:9'\n[mcp.servers.local]\ncommand = 'wisp-mcp-must-not-start'\nargs = []\n";
    home.write(CONFIG, config);
    home.write(
        "servers.toml",
        "[mcp.servers.run]\ncommand = 'wisp-mcp-must-not-start'\n",
    );
    let env = &[("LOCAL_KEY", "CANARY_DOCTOR_MCP_VALUE"), ("WISP_JOB", "1")];
    let output = home.run_with(
        &["--mcp-config", "servers.toml", "--tools", "none", "doctor"],
        env,
        "",
    );
    assert!(output.status.success(), "{}", both(&output));
    let shown = out(&output);
    let rows: Vec<_> = shown
        .lines()
        .filter(|line| line.trim_start().starts_with("mcp "))
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
        .collect();
    assert_eq!(
        rows,
        [
            ["mcp", "local", "starts", "on", "use", "—"],
            ["mcp", "run", "starts", "on", "use", "—"],
        ]
    );
    assert!(!shown.contains("wisp-mcp-must-not-start"), "{shown}");
    clean(&output, &home);
    home.write("bad.toml", "[unrelated]\nvalue = 'private-value'");
    let bad = home.run_with(&["--mcp-config", "bad.toml", "doctor"], env, "");
    assert_eq!(bad.status.code(), Some(1));
    assert!(bad.stdout.is_empty());
    assert_eq!(
        err(&bad),
        "wisp: the --mcp-config file may hold only [mcp.servers.<name>] tables\n"
    );
    assert!(
        home.read(CONFIG) == config,
        "doctor changed the saved config"
    );
}
