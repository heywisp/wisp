use wisp_render::text::{WidthMode, width};

use super::*;

const ALL: [CommandId; 17] = [
    CommandId::Model,
    CommandId::Effort,
    CommandId::Endpoint,
    CommandId::Context,
    CommandId::Compact,
    CommandId::Rewind,
    CommandId::Config,
    CommandId::Processes,
    CommandId::Agents,
    CommandId::Sidebar,
    CommandId::Help,
    CommandId::Exit,
    CommandId::Leave,
    CommandId::Resume,
    CommandId::Usage,
    CommandId::Login,
    CommandId::Logout,
];

const DOCS: &str = include_str!("../../../../../docs/commands.md");

fn all_names() -> Vec<&'static str> {
    COMMANDS.iter().flat_map(accepted).collect()
}

#[test]
fn every_command_id_has_exactly_one_entry() {
    for id in ALL {
        let entries = COMMANDS.iter().filter(|c| c.id == id).count();
        assert_eq!(entries, 1, "{id:?}");
        assert_eq!(get(id).map(|c| c.id), Some(id));
    }
}

#[test]
fn names_and_aliases_are_unique_lowercase_and_follow_the_grammar() {
    let names = all_names();
    for (i, name) in names.iter().enumerate() {
        assert!(is_name(name), "{name}");
        assert_eq!(name.to_ascii_lowercase(), *name);
        assert!(!names[i + 1..].contains(name), "{name} twice");
    }
}

#[test]
fn summaries_are_short_sentences_without_a_final_period() {
    for command in COMMANDS {
        let summary = command.summary;
        assert!(width(summary, WidthMode::AmbiguousWide) <= 32, "{summary}");
        assert!(
            summary.chars().next().is_some_and(char::is_uppercase),
            "{summary}"
        );
        assert!(!summary.ends_with('.'), "{summary}");
        let rest: String = summary.chars().skip(1).collect();
        assert!(
            rest.split_whitespace()
                .all(|w| w == "Wisp" || w.chars().all(|c| !c.is_uppercase())),
            "{summary}"
        );
    }
}

#[test]
fn word_values_are_unique_within_their_argument() {
    for command in COMMANDS {
        for arg in command.args {
            if let Source::Words(words) = arg.source {
                for (i, (word, about)) in words.iter().enumerate() {
                    assert!(!about.is_empty());
                    assert!(
                        !words[i + 1..].iter().any(|(w, _)| w == word),
                        "{word} twice in /{}",
                        command.name
                    );
                }
            }
        }
    }
}

#[test]
fn the_commands_doc_mentions_every_name_and_alias() {
    for name in all_names() {
        assert!(DOCS.contains(&format!("`/{name}")), "/{name}");
    }
}

#[test]
fn the_commands_doc_names_the_sidebar_and_each_of_its_words() {
    assert!(DOCS.contains("| `/sidebar` |"), "no /sidebar row");
    for (word, _) in SIDEBAR_WORDS {
        assert!(
            DOCS.contains(&format!("`/sidebar {word}`")),
            "/sidebar {word}"
        );
    }
}

#[test]
fn between_turns_hints_name_their_command() {
    for command in COMMANDS {
        if let Busy::BetweenTurns(hint) = command.busy {
            assert!(hint.starts_with(&format!("/{} ", command.name)), "{hint}");
        }
    }
    for command in COMMANDS {
        let want = match command.busy {
            Busy::Runs => None,
            Busy::BetweenTurns(hint) => Some(hint),
        };
        assert_eq!(between_turns(command.id), want);
    }
}

fn run(id: CommandId, args: &str) -> Parsed<'_> {
    Parsed::Command { id, args }
}

#[test]
fn parse_runs_every_name_and_alias_in_any_case() {
    for command in COMMANDS {
        for name in names(command) {
            for typed in [
                format!("/{name}"),
                format!("/{}", name.to_ascii_uppercase()),
                format!("/{name}   "),
            ] {
                assert_eq!(parse(&[], &typed, false), run(command.id, ""), "{typed}");
            }
        }
    }
    assert_eq!(parse(&[], "/quit", false), run(CommandId::Exit, ""));
    assert_eq!(parse(&[], "/Model", false), run(CommandId::Model, ""));
}

#[test]
fn parse_table() {
    let cases: &[(&str, bool, Parsed<'_>)] = &[
        (
            "/model claude-sonnet-5-5",
            false,
            run(CommandId::Model, "claude-sonnet-5-5"),
        ),
        ("/effort   high  ", false, run(CommandId::Effort, "high")),
        ("/effort\thigh", false, run(CommandId::Effort, "high")),
        (
            "/context all count",
            false,
            run(CommandId::Context, "all count"),
        ),
        ("/endpoint work", false, run(CommandId::Endpoint, "work")),
        ("/help me", false, run(CommandId::Help, "me")),
        (" /help", false, Parsed::Message),
        ("  /model x", false, Parsed::Message),
        ("/model x\nmore", false, Parsed::Message),
        ("/help\n", false, Parsed::Message),
        ("/help\r", false, Parsed::Message),
        ("/model", true, Parsed::Message),
        ("/hepl", true, Parsed::Message),
        ("/etc/hosts is wrong", false, Parsed::Message),
        ("/tmp/x is full", false, Parsed::Message),
        ("/", false, Parsed::Message),
        ("/ model", false, Parsed::Message),
        ("/--", false, Parsed::Message),
        ("/--help", false, Parsed::Message),
        ("/2x", false, Parsed::Message),
        ("/model!", false, Parsed::Message),
        ("see /model", false, Parsed::Message),
        ("", false, Parsed::Message),
        ("hello", false, Parsed::Message),
        ("/hepl", false, Parsed::Unknown("hepl")),
        ("/hepl me", false, Parsed::Unknown("hepl")),
        ("/Hepl", false, Parsed::Unknown("Hepl")),
        ("/tmp is full", false, Parsed::Unknown("tmp")),
        (
            "/compact",
            false,
            Parsed::Command {
                id: CommandId::Compact,
                args: "",
            },
        ),
        (
            "/Compact  keep the API notes ",
            false,
            Parsed::Command {
                id: CommandId::Compact,
                args: "keep the API notes",
            },
        ),
        ("/compact\nmore", false, Parsed::Message),
        ("/a-2", false, Parsed::Unknown("a-2")),
        ("/models", false, Parsed::Unknown("models")),
    ];
    for (text, attachments, want) in cases {
        assert_eq!(parse(&[], text, *attachments), *want, "{text:?}");
    }
}

#[test]
fn synopsis_shows_each_argument_shape() {
    let shapes = [
        (CommandId::Model, "/model [id]"),
        (CommandId::Effort, "/effort [level]"),
        (CommandId::Endpoint, "/account [name]"),
        (CommandId::Context, "/context [all]"),
        (CommandId::Compact, "/compact [focus]"),
        (CommandId::Processes, "/ps"),
        (CommandId::Help, "/help"),
        (CommandId::Exit, "/exit"),
    ];
    for (id, want) in shapes {
        let command = get(id).copied();
        assert_eq!(command.map(|c| synopsis(&c)).as_deref(), Some(want));
    }
    let free = Command {
        id: CommandId::Help,
        name: "compact",
        aliases: &[],
        args: &[Arg {
            name: "focus",
            source: Source::Free,
        }],
        summary: "Summarize to free up context",
        busy: Busy::BetweenTurns("/compact runs between turns."),
        chat: InChat::Main,
    };
    assert_eq!(synopsis(&free), "/compact [focus]");
    let settings = Command {
        name: "config",
        args: &[Arg {
            name: "filter",
            source: Source::Settings,
        }],
        ..free
    };
    assert_eq!(synopsis(&settings), "/config [filter]");
}

fn ids(found: &[&Command]) -> Vec<&'static str> {
    found.iter().map(|c| c.name).collect()
}

#[test]
fn matching_is_a_prefix_on_names_and_aliases_in_registry_order() {
    assert_eq!(
        ids(&matching("")),
        COMMANDS
            .iter()
            .map(|command| command.name)
            .collect::<Vec<_>>()
    );
    assert_eq!(ids(&matching("co")), ["context", "compact", "config"]);
    assert_eq!(ids(&matching("e")), ["effort", "exit"]);
    assert!(ids(&matching("en")).is_empty());
    assert_eq!(ids(&matching("ac")), ["account"]);
    assert_eq!(ids(&matching("AC")), ["account"]);
    assert_eq!(ids(&matching("q")), ["exit"]);
    assert_eq!(ids(&matching("quit")), ["exit"]);
    assert_eq!(ids(&matching("un")), ["rewind"]);
    assert_eq!(ids(&matching("odel")), Vec::<&str>::new());
    assert_eq!(ids(&matching("models")), Vec::<&str>::new());
    assert_eq!(ids(&matching("/model")), Vec::<&str>::new());
}

const NESTED: &[Command] = &[
    Command {
        id: CommandId::Context,
        name: "contexts",
        aliases: &[],
        args: &[],
        summary: "Longer",
        busy: Busy::Runs,
        chat: InChat::Main,
    },
    Command {
        id: CommandId::Model,
        name: "cont",
        aliases: &[],
        args: &[],
        summary: "Shorter",
        busy: Busy::Runs,
        chat: InChat::Main,
    },
    Command {
        id: CommandId::Exit,
        name: "exit",
        aliases: &["con"],
        args: &[],
        summary: "Alias",
        busy: Busy::Runs,
        chat: InChat::Main,
    },
];

#[test]
fn matching_puts_an_exact_match_first() {
    assert_eq!(
        ids(&matching_in(NESTED, "co")),
        ["contexts", "cont", "exit"]
    );
    assert_eq!(ids(&matching_in(NESTED, "cont")), ["cont", "contexts"]);
    assert_eq!(
        ids(&matching_in(NESTED, "CON")),
        ["exit", "contexts", "cont"]
    );
}

#[test]
fn parse_in_another_registry_uses_its_names() {
    assert_eq!(
        parse_in(NESTED, &[], "/con", false),
        run(CommandId::Exit, "")
    );
    assert_eq!(
        parse_in(NESTED, &[], "/model", false),
        Parsed::Unknown("model")
    );
}

#[test]
fn the_protocols_built_in_names_are_the_commands_names_and_aliases() {
    let mut names = all_names();
    names.sort_unstable();
    let mut listed = wisp_protocol::BUILT_IN_COMMANDS.to_vec();
    listed.sort_unstable();
    assert_eq!(listed, names);
}

fn plugin(plugin: &str, name: &str, aliases: &[&str], short: bool) -> PluginCommand {
    PluginCommand {
        plugin: plugin.to_owned(),
        name: name.to_owned(),
        aliases: aliases.iter().map(|a| (*a).to_owned()).collect(),
        args: vec![],
        summary: "Do the thing".to_owned(),
        between_turns: None,
        short,
    }
}

fn sent(parsed: Parsed<'_>) -> Option<(String, &str)> {
    match parsed {
        Parsed::Plugin {
            command,
            base,
            args,
        } => Some((command.full(base), args)),
        Parsed::Command { .. } | Parsed::Unknown(_) | Parsed::Message => None,
    }
}

#[test]
fn a_plugin_command_parses_by_full_name_and_by_its_short_form_only_when_short() {
    let plugins = [
        plugin("testkit", "echo", &["e2"], true),
        plugin("other", "clash", &[], false),
    ];
    let cases: &[(&str, Option<(&str, &str)>)] = &[
        ("/testkit:echo hi there", Some(("testkit:echo", "hi there"))),
        ("/TestKit:ECHO", Some(("testkit:echo", ""))),
        ("/testkit:e2 x", Some(("testkit:e2", "x"))),
        ("/echo  x ", Some(("testkit:echo", "x"))),
        ("/e2", Some(("testkit:e2", ""))),
        ("/other:clash", Some(("other:clash", ""))),
        ("/clash", None),
        ("/other:echo", None),
    ];
    for (text, want) in cases {
        let got = sent(parse(&plugins, text, false));
        assert_eq!(
            got.as_ref().map(|(name, args)| (name.as_str(), *args)),
            *want,
            "{text}"
        );
    }
    assert_eq!(parse(&plugins, "/clash", false), Parsed::Unknown("clash"));
    assert_eq!(parse(&plugins, "/help", false), run(CommandId::Help, ""));
    let shadow = [plugin("testkit", "help", &[], true)];
    assert_eq!(parse(&shadow, "/help", false), run(CommandId::Help, ""));
    assert!(sent(parse(&shadow, "/testkit:help", false)).is_some());
    assert_eq!(parse(&plugins, "/testkit:echo", true), Parsed::Message);
}

#[test]
fn a_plugin_word_is_a_command_word_and_an_unknown_one_stays_unknown() {
    let cases: &[(&str, Parsed<'_>)] = &[
        ("/nosuch:cmd", Parsed::Unknown("nosuch:cmd")),
        ("/a:b and more", Parsed::Unknown("a:b")),
        ("/a:", Parsed::Message),
        ("/todo: buy milk", Parsed::Message),
        ("/:b", Parsed::Message),
        ("/a:b:c", Parsed::Message),
        ("/a:2b", Parsed::Message),
        ("/2a:b", Parsed::Message),
    ];
    for (text, want) in cases {
        assert_eq!(parse(&[], text, false), *want, "{text}");
    }
}

#[test]
fn the_runtime_list_holds_only_installed_and_enabled_plugins_commands() {
    use wisp_protocol::plugin::{CommandEntry, PluginSource};
    let entry = |id: &str, installed: bool, enabled: bool| PluginEntry {
        id: id.to_owned(),
        version: "0.1.0".to_owned(),
        summary: String::new(),
        source: PluginSource::Bundled,
        installed,
        enabled,
        tools: vec![],
        commands: vec![CommandEntry {
            name: "go".to_owned(),
            aliases: vec!["g".to_owned()],
            summary: "Go".to_owned(),
            args: vec![CommandArg {
                name: "speed".to_owned(),
                values: vec!["fast".to_owned()],
            }],
            busy: CommandBusy::BetweenTurns {
                hint: "/go waits".to_owned(),
            },
            short: false,
        }],
        hooks: vec![],
        slots: vec![],
        prompt_sections: Vec::new(),
        settings: vec![],
        problems: vec![],
    };
    let listed = plugin_commands(&[
        entry("on", true, true),
        entry("off", true, false),
        entry("gone", false, true),
    ]);
    assert_eq!(
        listed,
        [PluginCommand {
            plugin: "on".to_owned(),
            name: "go".to_owned(),
            aliases: vec!["g".to_owned()],
            args: vec![CommandArg {
                name: "speed".to_owned(),
                values: vec!["fast".to_owned()],
            }],
            summary: "Go".to_owned(),
            between_turns: Some("/go waits".to_owned()),
            short: false,
        }]
    );
    assert_eq!(listed[0].names(), ["on:go", "on:g"]);
}

#[test]
fn plugin_entries_list_after_the_built_ins_and_match_short_and_full_forms() {
    let plugins = [
        plugin("testkit", "echo", &["say"], true),
        plugin("other", "exits", &[], false),
    ];
    let keys = |word: &str| -> Vec<String> {
        matching_entries(COMMANDS, &plugins, word)
            .iter()
            .map(Entry::key)
            .collect()
    };
    assert_eq!(keys("e"), ["effort", "exit", "testkit:echo"]);
    assert_eq!(keys("ex"), ["exit"]);
    assert_eq!(keys("ot"), ["other:exits"]);
    assert_eq!(keys("testkit:"), ["testkit:echo"]);
    assert_eq!(keys("SAY"), ["testkit:echo"]);
    assert_eq!(keys("").len(), COMMANDS.len() + 2);
    let echo = Entry::Plugin(plugins[0].clone());
    assert_eq!(echo.word_for("ec"), "echo");
    assert_eq!(echo.word_for("testkit:e"), "testkit:echo");
    assert_eq!(echo.word_for("t"), "testkit:echo");
    assert_eq!(echo.word_for(""), "echo");
    assert_eq!(
        Entry::Plugin(plugins[1].clone()).word_for("ex"),
        "other:exits"
    );
    assert_eq!(matched(&echo, "s").as_deref(), Some("say"));
}

#[test]
fn endpoint_is_a_hidden_alias_that_still_runs_account() {
    assert_eq!(
        parse(&[], "/endpoint work", false),
        run(CommandId::Endpoint, "work")
    );
    assert_eq!(
        parse(&[], "/account work", false),
        run(CommandId::Endpoint, "work")
    );
    assert!(matching("endpoint").is_empty());
    let account = get(CommandId::Endpoint).copied();
    let row = account.map(|c| crate::app::help::command_row(&c));
    assert_eq!(row.map(|r| r.keys).as_deref(), Some("/account"));
    let help: Vec<String> = COMMANDS
        .iter()
        .map(|c| crate::app::help::command_row(c).keys)
        .collect();
    assert!(
        help.iter().all(|keys| !keys.contains("/endpoint")),
        "{help:?}"
    );
}
