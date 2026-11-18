use std::collections::BTreeSet;
use std::sync::Arc;

use futures::FutureExt;
use wisp_plugin_testkit::hooks::{self as kit, Calls};
use wisp_protocol::plugin::{CommandArg, CommandBusy, CommandEntry};

use crate::plugin::{KernelHost, Make, PluginConfig, Problem, Registry};

fn source(id: &str, body: &str) -> (String, Make) {
    let make: Make = kit::instantiate(kit::none(), Calls::default());
    (kit::manifest(id, body), make)
}

fn registry(sources: Vec<(String, Make)>) -> (Registry, Vec<Problem>) {
    Registry::from_sources(sources, &PluginConfig::default())
}

fn commands(registry: &Registry, id: &str) -> Vec<CommandEntry> {
    registry
        .catalog(&BTreeSet::new(), &[])
        .plugins
        .into_iter()
        .find(|p| p.id == id)
        .map(|p| p.commands)
        .unwrap_or_default()
}

fn shorts(registry: &Registry, id: &str) -> Vec<(String, bool)> {
    commands(registry, id)
        .into_iter()
        .map(|c| (c.name, c.short))
        .collect()
}

#[test]
fn a_command_lists_its_fields_and_the_commands_come_in_name_order() {
    let body = kit::command("zap", "aliases = [\"z\"]\ntimeout_ms = 5000\n")
        + &kit::command(
            "add",
            "busy = { between_turns = \"Add runs between turns.\" }\n\n[[commands.args]]\nname = \"what\"\nvalues = [\"one\", \"two\"]\n\n[[commands.args]]\nname = \"note\"\nvalues = \"free\"\n",
        );
    let (registry, problems) = registry(vec![source("aa", &body)]);
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(
        commands(&registry, "aa"),
        [
            CommandEntry {
                name: "add".into(),
                aliases: Vec::new(),
                summary: "Run add".into(),
                args: vec![
                    CommandArg {
                        name: "what".into(),
                        values: vec!["one".into(), "two".into()],
                    },
                    CommandArg {
                        name: "note".into(),
                        values: Vec::new(),
                    },
                ],
                busy: CommandBusy::BetweenTurns {
                    hint: "Add runs between turns.".into(),
                },
                short: true,
            },
            CommandEntry {
                name: "zap".into(),
                aliases: vec!["z".into()],
                summary: "Run zap".into(),
                args: Vec::new(),
                busy: CommandBusy::Runs,
                short: true,
            },
        ]
    );
}

#[test]
fn a_clash_removes_both_short_forms_whatever_the_order_with_a_problem_for_each() {
    for flip in [false, true] {
        let mut sources = vec![
            source("aa", &kit::command("deploy", "")),
            source("bb", &kit::command("ship", "aliases = [\"deploy\"]\n")),
            source("cc", &kit::command("lone", "")),
        ];
        if flip {
            sources.reverse();
        }
        let (registry, problems) = registry(sources);
        let problems: Vec<(String, String)> = problems
            .into_iter()
            .map(|p| (p.plugin, p.message))
            .collect();
        assert_eq!(
            problems,
            [
                (
                    "aa".to_owned(),
                    "its command `deploy` shares the name /deploy with bb, so it is only /aa:deploy"
                        .to_owned()
                ),
                (
                    "bb".to_owned(),
                    "its command `ship` shares the name /deploy with aa, so it is only /bb:deploy"
                        .to_owned()
                ),
            ]
        );
        assert_eq!(shorts(&registry, "aa"), [("deploy".to_owned(), false)]);
        assert_eq!(shorts(&registry, "bb"), [("ship".to_owned(), false)]);
        assert_eq!(shorts(&registry, "cc"), [("lone".to_owned(), true)]);
    }
}

#[test]
fn a_name_or_alias_of_a_built_in_keeps_only_its_full_form_with_a_problem() {
    let body = kit::command("rewind", "") + &kit::command("back", "aliases = [\"undo\"]\n");
    let (registry, problems) = registry(vec![source("aa", &body)]);
    assert_eq!(
        shorts(&registry, "aa"),
        [("back".to_owned(), false), ("rewind".to_owned(), false)]
    );
    let messages: Vec<&str> = problems.iter().map(|p| p.message.as_str()).collect();
    assert_eq!(
        messages,
        [
            "its command `back` uses the built-in name /undo, so it is only /aa:undo",
            "its command `rewind` uses the built-in name /rewind, so it is only /aa:rewind",
        ]
    );
    let catalog = registry.catalog(&BTreeSet::new(), &[]);
    assert_eq!(catalog.plugins[0].problems.len(), 2);
}

#[test]
fn a_started_command_is_found_by_its_full_name_or_an_alias() {
    let body = kit::command("deploy", "aliases = [\"ship\"]\ntimeout_ms = 1500\n");
    let (registry, _) = registry(vec![source("aa", &body)]);
    let started = registry
        .start(&BTreeSet::new(), Arc::new(KernelHost::new()))
        .now_or_never()
        .unwrap();
    let commands = &started.hooks.commands;
    let found = commands.find("aa:ship").unwrap();
    assert_eq!(found.full_name(), "aa:deploy");
    assert_eq!(found.timeout.as_millis(), 1500);
    assert!(commands.find("aa:deploy").is_some());
    assert!(commands.find("deploy").is_none());
    assert!(commands.find("bb:deploy").is_none());
}
