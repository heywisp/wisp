use clap::{CommandFactory, Parser};
use wisp_protocol::settings::{PLUGIN_ENABLED, PLUGIN_HOOK, PLUGIN_INSTALLED, SETTINGS};

use super::*;
use crate::Args;

fn row(id: &str, open_elsewhere: bool) -> ChatRow {
    let topic = format!("topic {id}");
    let chat =
        wisp_render::chrome::mockup_chat(id, &topic, "claude-work", "claude-opus-5-5", "1h ago");
    ChatRow {
        name: None,
        open_elsewhere,
        ..chat
    }
}

const HERE: &str = "~/Projects/Wisp";

const KEPT_FLAGS: [&str; 7] = [
    "config", "env_file", "keychain", "verbose", "output", "set", "resume",
];

const DROPPED_FLAGS: [&str; 21] = [
    "host",
    "restarted",
    "endpoint",
    "model",
    "effort",
    "cache_ttl",
    "time",
    "debug_cache",
    "context_note",
    "no_auto_compact",
    "compact_at",
    "retry_budget",
    "no_fallback",
    "self_check",
    "instructions",
    "instructions_extra",
    "instructions_warn_tokens",
    "tools",
    "resume_unlocked",
    "replay",
    "no_log",
];

const DROPPED_SETTINGS: [&str; 53] = [
    "agent",
    "agents.max_agents",
    "agents.max_depth",
    "agents.max_running",
    "agents.max_tree_running",
    "agents.wake_when_stuck",
    "agents.warm_refreshes",
    "aliases",
    "cache_ttl",
    "claude_usage",
    "compact_at",
    "context.auto_compact",
    "context.background_compaction",
    "context.background_tokens",
    "context.compact_effort",
    "context.context_note",
    "context.count_tokens",
    "context.strategy",
    "context.verbatim_user_tokens",
    "context_window",
    "debug_cache",
    "effort",
    "endpoint",
    "instructions.extra",
    "instructions.source",
    "instructions.warn_tokens",
    "keep_warm",
    "log",
    "max_output",
    "message_threads",
    "model",
    "native_compaction",
    "native_web_search",
    PLUGIN_ENABLED,
    PLUGIN_HOOK,
    PLUGIN_INSTALLED,
    "preload",
    "preset",
    "price_cache_read",
    "price_input",
    "price_output",
    "refusal_fallback",
    "retry.budget_usd",
    "self_check.before_end",
    "time",
    "tool_output.max_age_days",
    "tools",
    "turned_off",
    "voice.endpoint",
    "voice.instructions",
    "voice.language",
    "voice.voice",
    "web_search",
];

#[test]
fn every_flag_is_kept_or_dropped_exactly_once() {
    let ids: Vec<String> = Args::command()
        .get_arguments()
        .map(|arg| arg.get_id().to_string())
        .filter(|id| id != "help" && id != "version")
        .collect();
    for id in &ids {
        let places = KEPT_FLAGS
            .iter()
            .chain(&DROPPED_FLAGS)
            .filter(|f| *f == id)
            .count();
        assert_eq!(
            places, 1,
            "--{id} must be in exactly one of KEPT_FLAGS and DROPPED_FLAGS"
        );
    }
    for listed in KEPT_FLAGS.iter().chain(&DROPPED_FLAGS) {
        assert!(ids.iter().any(|id| id == listed), "{listed} is no flag");
    }
}

#[test]
fn every_setting_is_kept_or_dropped_exactly_once() {
    for spec in SETTINGS {
        let places = KEPT_SETTINGS
            .iter()
            .chain(&DROPPED_SETTINGS)
            .filter(|name| **name == spec.name)
            .count();
        assert_eq!(
            places, 1,
            "{} must be in exactly one settings list",
            spec.name
        );
    }
    for listed in KEPT_SETTINGS.iter().chain(&DROPPED_SETTINGS) {
        assert!(
            SETTINGS.iter().any(|spec| spec.name == *listed),
            "{listed} is no setting"
        );
    }
}

#[test]
fn only_display_and_account_sets_are_kept() {
    let sets = [
        "display.theme=light",
        "endpoints.work.client_version=1.2.3",
        "effort=low",
        "model=claude-sonnet-5-5",
        "log=false",
        "keychain=off",
        "time=off",
        "not a setting",
    ]
    .map(str::to_owned);
    assert_eq!(
        kept_sets(&sets),
        [
            "display.theme=light",
            "endpoints.work.client_version=1.2.3",
            "keychain=off"
        ]
    );
}

#[test]
fn the_relaunch_keeps_only_the_kept_flags() {
    let given = Args::try_parse_from([
        "wisp",
        "--endpoint",
        "claude-api",
        "--no-log",
        "--time",
        "utc",
        "--set",
        "effort=low",
        "--set",
        "display.theme=light",
        "--self-check",
        "--config",
        "c.toml",
        "--env-file",
        "e.env",
        "--verbose",
        "--keychain",
        "off",
        "--output",
        "tui",
        "--model",
        "claude-sonnet-5-5",
    ])
    .unwrap();
    let again = crate::relaunch(&given, "01a2".to_owned());
    let kept = [
        "--config",
        "c.toml",
        "--env-file",
        "e.env",
        "--keychain=off",
        "--verbose",
        "--output=tui",
        "--set=display.theme=light",
        "--resume=01a2",
    ];
    let mut fresh = Args::try_parse_from(std::iter::once("wisp").chain(kept)).unwrap();
    fresh.host_args = kept.map(Into::into).to_vec();
    assert_eq!(format!("{again:?}"), format!("{fresh:?}"));
    let mut parsed = Args::try_parse_from(
        std::iter::once(std::ffi::OsString::from("wisp")).chain(again.host_args.clone()),
    )
    .unwrap();
    parsed.host_args.clone_from(&again.host_args);
    assert_eq!(format!("{parsed:?}"), format!("{again:?}"));
    let command = Args::command();
    for flag in KEPT_FLAGS {
        let long = command
            .get_arguments()
            .find(|arg| arg.get_id() == flag)
            .and_then(|arg| arg.get_long())
            .unwrap();
        let name = format!("--{long}");
        let with_value = format!("{name}=");
        assert!(
            again.host_args.iter().any(|arg| arg
                .to_str()
                .is_some_and(|arg| arg == name || arg.starts_with(&with_value))),
            "the relaunch did not carry {name}"
        );
    }
}

#[test]
fn one_openable_chat_opens_directly() {
    assert_eq!(
        choose(vec![row("a", false)], HERE),
        Choice::Open {
            id: "a".to_owned(),
            note: None
        }
    );
    assert_eq!(
        choose(vec![row("a", false), row("b", true)], HERE),
        Choice::Open {
            id: "a".to_owned(),
            note: None
        }
    );
}

#[test]
fn a_newer_chat_open_elsewhere_is_named_when_the_older_one_opens() {
    assert_eq!(
        choose(vec![row("a", true), row("b", false)], HERE),
        Choice::Open {
            id: "b".to_owned(),
            note: Some("wisp: The newest chat here is open in another window.".to_owned())
        }
    );
}

#[test]
fn two_openable_chats_open_the_picker_with_every_row() {
    let chats = vec![row("a", true), row("b", false), row("c", false)];
    assert_eq!(choose(chats.clone(), HERE), Choice::Pick { chats });
}

#[test]
fn no_chat_and_every_chat_open_elsewhere_refuse_with_their_lines() {
    assert_eq!(
        choose(Vec::new(), HERE),
        Choice::Refuse(
            "wisp: nothing to resume in ~/Projects/Wisp; run wisp to start a new chat".to_owned()
        )
    );
    assert_eq!(
        choose(vec![row("a", true), row("b", true)], HERE),
        Choice::Refuse(
            "wisp: every chat in ~/Projects/Wisp is open in another window; continue there or run wisp to start a new chat"
                .to_owned()
        )
    );
}

#[test]
fn a_kept_chat_always_opens_the_picker_even_beside_one_closed_chat() {
    let mut running = row("a", true);
    running.hosted = true;
    for chats in [vec![running.clone()], vec![running, row("b", false)]] {
        assert_eq!(choose(chats.clone(), HERE), Choice::Pick { chats });
    }
}
