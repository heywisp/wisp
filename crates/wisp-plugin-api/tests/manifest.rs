#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests fail on an unexpected parse"
)]

use std::path::{Path, PathBuf};

use toml_edit::{Array, Decor, DocumentMut, InlineTable, Item, RawString, Table, Value};
use wisp_plugin_api::manifest::{
    AGENT_POINTS, ArgValues, BUNDLED_IDS, Busy, Capability, Manifest, OnFailure, Origin, Platform,
    PointKind, Problem, RowDefault, RowKind, SettingRow, Switch, Threshold,
};

mod support;

const ROW: &str = r#"[[settings]]
name = "minutes"
label = "Idle after"
help = "Minutes before away."
kind = "int"
min = 1
max = 60
default = 10
"#;

const CONTEXT: &str = r#"id = "context"
version = "0.1.0"
world = "0.1.0"
summary = "Tells the model how full its context is"
settings = ["context.context_note"]

[system]
text = "A 'Context:' line, when present, gives how much of your context window the conversation fills; at the stated point Wisp replaces older turns with a summary."
after = ["time"]
before = []

[[hooks]]
name = "fill"
point = "before-turn"
after = ["time/sent"]
before = ["core/processes"]
requires = []
timeout_ms = 2000
on_failure = "ignore"
background = false
switch = "context.context_note"
"#;

const FULL: &str = r#"id = "acme"
version = "1.2.3"
world = "0.1.0"
summary = "Everything a manifest can say"
settings = ["plugins.acme.mode"]

[system]
text = "Acme tools are available."

[[tools]]
name = "acme_run"
provides = ["shell"]
platforms = ["unix"]

[[tools]]
name = "acme-read"
default = "off"
follows = "acme_run"

[[tools]]
name = "acme_search"
native = "web_search"

[[commands]]
name = "deploy"
aliases = ["ship"]
summary = "Deploy the current branch"
busy = { between_turns = "Wait for the turn to end" }
timeout_ms = 5000

[[commands.args]]
name = "target"
values = ["staging", "production"]

[[commands.args]]
name = "note"
values = "free"

[[commands]]
name = "status"
summary = "Show the deploy status"

[[hooks]]
name = "watch"
point = "acme/job-ended"
background = true

[[hooks]]
name = "early"
point = "context-threshold"
from = "80%"
on_failure = "block"

[[hooks]]
name = "late"
point = "context-threshold"
from = 150000
switch = "plugins.acme.mode"

[[hooks]]
name = "head"
point = "compaction-head"
reserve = "Acme state: unknown"

[[points]]
name = "job-ended"
kind = "observe"
version = "1"

[capabilities]
requested = ["exec", "fs", "net", "ask"]
"#;

fn parse(text: &str) -> Result<Manifest, Problem> {
    Manifest::parse(text, Origin::External)
}

fn problem(text: &str) -> Problem {
    support::problem(text, Origin::External)
}

fn message(text: &str) -> String {
    problem(text).message
}

fn minimal(rest: &str) -> String {
    format!("id = \"acme\"\nversion = \"0.1.0\"\nworld = \"0.1.0\"\nsummary = \"Acme\"\n{rest}")
}

#[test]
fn the_context_manifest_from_the_plan_parses_as_a_bundled_plugin() {
    let manifest = Manifest::parse(CONTEXT, Origin::Bundled).unwrap();
    assert_eq!(manifest.id, "context");
    assert_eq!(
        manifest.names().collect::<Vec<_>>(),
        ["context.context_note"]
    );
    let system = manifest.system.unwrap();
    assert_eq!(system.after, ["time"]);
    assert!(system.before.is_empty());
    let hook = &manifest.hooks[0];
    assert_eq!(hook.name, "fill");
    assert_eq!(hook.point, "before-turn");
    assert_eq!(hook.after, ["time/sent"]);
    assert_eq!(hook.before, ["core/processes"]);
    assert_eq!(hook.timeout_ms, Some(2000));
    assert_eq!(hook.on_failure, OnFailure::Ignore);
    assert!(!hook.background);
    assert_eq!(hook.switch.as_deref(), Some("context.context_note"));
    assert!(manifest.tools.is_empty() && manifest.commands.is_empty());
}

#[test]
fn every_table_and_key_of_world_0_1_0_is_read() {
    let manifest = parse(FULL).unwrap();
    let [run, read, search] = &manifest.tools[..] else {
        panic!("{:?}", manifest.tools)
    };
    assert_eq!(search.native.as_deref(), Some("web_search"));
    assert_eq!(run.native, None);
    assert_eq!(run.default, Switch::On);
    assert_eq!(run.platforms, [Platform::Unix]);
    assert_eq!(run.provides, ["shell"]);
    assert_eq!(read.default, Switch::Off);
    assert_eq!(read.follows.as_deref(), Some("acme_run"));
    assert_eq!(read.platforms, [Platform::Unix, Platform::Windows]);

    let [deploy, status] = &manifest.commands[..] else {
        panic!("{:?}", manifest.commands)
    };
    assert_eq!(deploy.aliases, ["ship"]);
    assert_eq!(
        deploy.busy,
        Busy::BetweenTurns("Wait for the turn to end".into())
    );
    assert_eq!(deploy.timeout_ms, 5000);
    assert_eq!(
        deploy.args[0].values,
        ArgValues::Words(vec!["staging".into(), "production".into()])
    );
    assert_eq!(deploy.args[1].values, ArgValues::Free);
    assert_eq!(status.busy, Busy::Runs);
    assert_eq!(status.timeout_ms, 30_000);
    assert!(status.args.is_empty());

    let hooks = &manifest.hooks;
    assert!(hooks[0].background);
    assert_eq!(hooks[1].from, Some(Threshold::Percent(80)));
    assert_eq!(hooks[1].on_failure, OnFailure::Block);
    assert_eq!(hooks[2].from, Some(Threshold::Tokens(150_000)));
    assert_eq!(hooks[3].reserve.as_deref(), Some("Acme state: unknown"));
    assert_eq!(hooks[0].timeout_ms, None);

    assert_eq!(manifest.points[0].kind, PointKind::Observe);
    assert_eq!(manifest.points[0].version, "1");
    assert_eq!(
        manifest.capabilities.requested,
        [
            Capability::Exec,
            Capability::Fs,
            Capability::Net,
            Capability::Ask
        ]
    );
}

#[test]
fn a_manifest_with_only_its_identity_is_complete() {
    let manifest = parse(&minimal("")).unwrap();
    assert!(manifest.settings.is_empty());
    assert!(manifest.system.is_none());
    assert!(manifest.hooks.is_empty() && manifest.points.is_empty());
    assert!(manifest.capabilities.requested.is_empty());
}

#[test]
fn an_unknown_key_is_refused_at_the_top_level_and_in_every_table() {
    let cases = [
        ("colour = \"red\"\n", "colour"),
        ("[system]\ntext = \"Hi.\"\ncolour = 1\n", "colour"),
        ("[[tools]]\nname = \"t\"\ncolour = 1\n", "colour"),
        (
            "[[commands]]\nname = \"c\"\nsummary = \"S\"\ncolour = 1\n",
            "colour",
        ),
        (
            "[[commands]]\nname = \"c\"\nsummary = \"S\"\n[[commands.args]]\nname = \"a\"\nvalues = \"free\"\ncolour = 1\n",
            "colour",
        ),
        (
            "[[hooks]]\nname = \"h\"\npoint = \"after-turn\"\ncolour = 1\n",
            "colour",
        ),
        (
            "[[points]]\nname = \"p\"\nkind = \"observe\"\nversion = \"1\"\ncolour = 1\n",
            "colour",
        ),
        ("[capabilities]\nrequested = []\ncolour = 1\n", "colour"),
        ("[[slots]]\nname = \"sidebar\"\n", "slots"),
    ];
    for (rest, key) in cases {
        let problem = problem(&minimal(rest));
        assert!(
            problem
                .message
                .starts_with(&format!("unknown field `{key}`")),
            "{rest}: {problem}"
        );
        assert!(problem.line.is_some() && problem.column.is_some(), "{rest}");
    }
}

#[test]
fn an_unknown_key_in_busy_is_refused() {
    let text = minimal("[[commands]]\nname = \"c\"\nsummary = \"S\"\nbusy = { colour = \"x\" }\n");
    let problem = problem(&text);
    assert!(
        problem.message.starts_with("unknown variant `colour`"),
        "{problem}"
    );
    assert_eq!((problem.line, problem.column), (Some(8), Some(10)));
    let both = minimal(
        "[[commands]]\nname = \"c\"\nsummary = \"S\"\nbusy = { between_turns = \"x\", colour = \"y\" }\n",
    );
    assert!(
        problem_text(&both).contains("line 8"),
        "{}",
        problem_text(&both)
    );
}

fn problem_text(text: &str) -> String {
    problem(text).to_string()
}

#[test]
fn an_unknown_key_names_its_line_and_column() {
    let text = minimal("\n[[hooks]]\nname = \"h\"\npoint = \"after-turn\"\n  colour = 1\n");
    let problem = problem(&text);
    assert_eq!(problem.line, Some(9));
    assert_eq!(problem.column, Some(3));
    assert_eq!(
        problem.to_string(),
        format!("line 9, column 3: {}", problem.message)
    );
}

#[test]
fn a_world_this_wisp_doesnt_speak_is_named_before_any_unknown_key() {
    let text = "id = \"acme\"\nversion = \"2.0.0\"\nworld = \"0.5.0\"\nsummary = \"Acme\"\n\n[[slots]]\nname = \"sidebar\"\nplace = \"right\"\n";
    let problem = problem(text);
    assert_eq!(
        problem.message,
        "world `0.5.0` isn't spoken by this Wisp, which speaks `0.1.0`, `0.2.0`, `0.3.0`, `0.4.0`"
    );
    assert_eq!((problem.line, problem.column), (Some(3), Some(9)));
}

#[test]
fn a_missing_world_or_bad_toml_keeps_tomls_position() {
    let missing = problem("id = \"acme\"\n");
    assert!(
        missing.message.contains("missing field `world`"),
        "{missing}"
    );
    let broken = problem("id = \"acme\"\nworld = \n");
    assert_eq!(broken.line, Some(2));
    let wrong = problem(&minimal("[[tools]]\nname = 3\n"));
    assert_eq!(wrong.line, Some(6));
}

#[test]
fn ids_follow_the_grammar() {
    for good in ["ab", "a1", "my-plugin", "a-b-c-d", &"a".repeat(32)] {
        let text = minimal("").replace("\"acme\"", &format!("\"{good}\""));
        assert!(parse(&text).is_ok(), "{good}");
    }
    for bad in [
        "a",
        "Acme",
        "acMe",
        "1abc",
        "-abc",
        "ab_c",
        "ab c",
        "ab.c",
        "",
        &"a".repeat(33),
    ] {
        let text = minimal("").replace("\"acme\"", &format!("\"{bad}\""));
        assert_eq!(
            message(&text),
            format!(
                "id `{bad}` must be 2 to 32 characters of a-z, 0-9 and -, starting with a letter"
            ),
        );
    }
}

#[test]
fn reserved_ids_are_refused() {
    for id in ["core", "wisp"] {
        let text = minimal("").replace("\"acme\"", &format!("\"{id}\""));
        assert_eq!(message(&text), format!("id `{id}` is reserved"));
        assert_eq!(
            Manifest::parse(&text, Origin::Bundled).unwrap_err().message,
            format!("id `{id}` is reserved")
        );
    }
    for id in BUNDLED_IDS {
        let text = minimal("").replace("\"acme\"", &format!("\"{id}\""));
        assert_eq!(
            message(&text),
            format!("id `{id}` is reserved for a bundled plugin")
        );
        assert!(Manifest::parse(&text, Origin::Bundled).is_ok(), "{id}");
    }
}

#[test]
fn summaries_have_a_length_limit_in_characters() {
    let wide = minimal("").replace("\"Acme\"", &format!("\"{}\"", "a".repeat(61)));
    assert_eq!(
        message(&wide),
        "summary is 61 characters long; the limit is 60"
    );
    let fits = minimal("").replace("\"Acme\"", &format!("\"{}\"", "a".repeat(60)));
    assert!(parse(&fits).is_ok());
    let wide_chars = minimal("").replace("\"Acme\"", &format!("\"{}\"", "界".repeat(61)));
    assert_eq!(
        message(&wide_chars),
        "summary is 61 characters long; the limit is 60"
    );
    let cjk = minimal("").replace("\"Acme\"", &format!("\"{}\"", "界".repeat(60)));
    assert!(parse(&cjk).is_ok());
    let command = minimal(&format!(
        "[[commands]]\nname = \"c\"\nsummary = \"{}\"\n",
        "a".repeat(33)
    ));
    assert_eq!(
        message(&command),
        "command `c`'s summary is 33 characters long; the limit is 32"
    );
}

#[test]
fn the_system_sentence_holds_no_dates_paths_or_ids() {
    for bad in [
        "It is 2026.",
        "Files live in src/lib.",
        "A \\\\ path.",
        "Session 7f3a.",
    ] {
        let text = minimal(&format!("[system]\ntext = \"{bad}\"\n"));
        assert_eq!(
            message(&text),
            "the system sentence holds a digit, `/` or `\\`, but it may hold no dates, paths or ids",
            "{bad}"
        );
    }
}

#[test]
fn tool_names_follow_the_api_rule() {
    for bad in ["", "a b", "a.b", "naïve", &"a".repeat(65)] {
        let text = minimal(&format!("[[tools]]\nname = \"{bad}\"\n"));
        assert_eq!(
            message(&text),
            format!("tool name `{bad}` must be 1 to 64 characters of A-Z, a-z, 0-9, _ and -")
        );
    }
    let ok = minimal(&format!("[[tools]]\nname = \"A_b-9{}\"\n", "x".repeat(59)));
    assert!(parse(&ok).is_ok());
}

#[test]
fn duplicate_names_are_refused() {
    let tools = minimal("[[tools]]\nname = \"t\"\n[[tools]]\nname = \"t\"\n");
    assert_eq!(message(&tools), "tool `t` is declared twice");
    let commands = minimal(
        "[[commands]]\nname = \"c\"\nsummary = \"S\"\n[[commands]]\nname = \"c\"\nsummary = \"S\"\n",
    );
    assert_eq!(message(&commands), "command name `c` is declared twice");
    let alias = minimal(
        "[[commands]]\nname = \"c\"\nsummary = \"S\"\n[[commands]]\nname = \"d\"\naliases = [\"c\"]\nsummary = \"S\"\n",
    );
    assert_eq!(message(&alias), "command name `c` is declared twice");
    let hooks = minimal(
        "[[hooks]]\nname = \"h\"\npoint = \"after-turn\"\n[[hooks]]\nname = \"h\"\npoint = \"rewound\"\n",
    );
    assert_eq!(message(&hooks), "hook `h` is declared twice");
    let points = minimal(
        "[[points]]\nname = \"p\"\nkind = \"observe\"\nversion = \"1\"\n[[points]]\nname = \"p\"\nkind = \"transform\"\nversion = \"2\"\n",
    );
    assert_eq!(message(&points), "point `p` is declared twice");
}

#[test]
fn command_hook_and_point_names_follow_the_grammar() {
    let command = minimal("[[commands]]\nname = \"Deploy\"\nsummary = \"S\"\n");
    assert_eq!(
        message(&command),
        "command name `Deploy` must be 1 to 32 characters of a-z, 0-9 and -, starting with a letter"
    );
    let alias = minimal("[[commands]]\nname = \"d\"\naliases = [\"a:b\"]\nsummary = \"S\"\n");
    assert_eq!(
        message(&alias),
        "command name `a:b` must be 1 to 32 characters of a-z, 0-9 and -, starting with a letter"
    );
    let hook = minimal("[[hooks]]\nname = \"a/b\"\npoint = \"after-turn\"\n");
    assert_eq!(
        message(&hook),
        "hook name `a/b` must be 1 to 32 characters of a-z, 0-9 and -, starting with a letter"
    );
    let point = minimal("[[points]]\nname = \"9p\"\nkind = \"observe\"\nversion = \"1\"\n");
    assert_eq!(
        message(&point),
        "point name `9p` must be 1 to 32 characters of a-z, 0-9 and -, starting with a letter"
    );
}

#[test]
fn a_native_family_is_shaped_like_a_tool_name() {
    let bad = minimal("[[tools]]\nname = \"a\"\nnative = \"web search\"\n");
    assert_eq!(
        message(&bad),
        "tool `a` asks for the native tool `web search`, which must be 1 to 64 characters of A-Z, a-z, 0-9, _ and -"
    );
    let good = minimal("[[tools]]\nname = \"a\"\nnative = \"web_search\"\n");
    assert_eq!(
        parse(&good).unwrap().tools[0].native.as_deref(),
        Some("web_search")
    );
}

#[test]
fn follows_names_a_leader_of_the_same_plugin_and_never_chains() {
    let missing = minimal("[[tools]]\nname = \"b\"\nfollows = \"a\"\n");
    assert_eq!(
        message(&missing),
        "tool `b` follows `a`, which isn't a tool of this plugin"
    );
    let chain = minimal(
        "[[tools]]\nname = \"a\"\n[[tools]]\nname = \"b\"\nfollows = \"a\"\n[[tools]]\nname = \"c\"\nfollows = \"b\"\n",
    );
    assert_eq!(
        message(&chain),
        "tool `c` follows `b`, which itself follows `a`; follows can't chain"
    );
    let own = minimal("[[tools]]\nname = \"a\"\nfollows = \"a\"\n");
    assert_eq!(
        message(&own),
        "tool `a` follows `a`, which itself follows `a`; follows can't chain"
    );
}

#[test]
fn a_switch_must_be_a_setting_the_plugin_lists() {
    let text = minimal("[[hooks]]\nname = \"h\"\npoint = \"after-turn\"\nswitch = \"time\"\n");
    assert_eq!(
        message(&text),
        "hook `h` is switched by `time`, a setting this plugin doesn't list"
    );
    let listed = minimal(
        "settings = [\"time\"]\n[[hooks]]\nname = \"h\"\npoint = \"after-turn\"\nswitch = \"time\"\n",
    );
    assert!(parse(&listed).is_ok());
    let defined = rows(&format!(
        "{ROW}\n[[hooks]]\nname = \"h\"\npoint = \"after-turn\"\nswitch = \"plugins.acme.minutes\"\n"
    ));
    assert_eq!(
        message(&defined),
        "hook `h` is switched by `plugins.acme.minutes`, a setting this plugin doesn't list"
    );
}

#[test]
fn background_is_for_observe_points_only() {
    for point in [
        "before-tool",
        "user-input",
        "before-turn",
        "before-end",
        "after-tool",
    ] {
        let text = minimal(&format!(
            "[[hooks]]\nname = \"h\"\npoint = \"{point}\"\nbackground = true\n"
        ));
        assert_eq!(
            message(&text),
            format!("hook `h` runs in the background at `{point}`, which isn't an observe point")
        );
    }
    let own = minimal(
        "[[points]]\nname = \"p\"\nkind = \"transform\"\nversion = \"1\"\n[[hooks]]\nname = \"h\"\npoint = \"acme/p\"\nbackground = true\n",
    );
    assert_eq!(
        message(&own),
        "hook `h` runs in the background at `acme/p`, which isn't an observe point"
    );
    for point in ["session-end", "after-turn", "settings-changed", "other/p"] {
        let text = minimal(&format!(
            "[[hooks]]\nname = \"h\"\npoint = \"{point}\"\nbackground = true\n"
        ));
        assert!(parse(&text).is_ok(), "{point}");
    }
}

#[test]
fn reserve_is_for_before_turn_and_compaction_head_only() {
    for point in ["before-turn", "compaction-head"] {
        let text = minimal(&format!(
            "[[hooks]]\nname = \"h\"\npoint = \"{point}\"\nreserve = \"Sent: x\"\n"
        ));
        assert!(parse(&text).is_ok(), "{point}");
    }
    for point in ["after-turn", "before-compact", "other/p"] {
        let text = minimal(&format!(
            "[[hooks]]\nname = \"h\"\npoint = \"{point}\"\nreserve = \"Sent: x\"\n"
        ));
        assert_eq!(
            message(&text),
            format!(
                "hook `h` declares reserve at `{point}`; only before-turn and compaction-head take it"
            )
        );
    }
}

#[test]
fn a_before_turn_hook_never_comes_after_the_typed_text() {
    let text =
        minimal("[[hooks]]\nname = \"h\"\npoint = \"before-turn\"\nafter = [\"core/typed\"]\n");
    assert_eq!(
        message(&text),
        "hook `h` comes after `core/typed`; the typed text ends the turn message"
    );
    for (point, key) in [("after-turn", "after"), ("before-turn", "before")] {
        let text = minimal(&format!(
            "[[hooks]]\nname = \"h\"\npoint = \"{point}\"\n{key} = [\"core/typed\"]\n"
        ));
        assert!(parse(&text).is_ok(), "{point} {key}");
    }
}

#[test]
fn from_is_for_context_threshold_only() {
    for point in ["before-turn", "before-compact", "other/p"] {
        let text = minimal(&format!(
            "[[hooks]]\nname = \"h\"\npoint = \"{point}\"\nfrom = 1000\n"
        ));
        assert_eq!(
            message(&text),
            format!("hook `h` declares from at `{point}`; only context-threshold takes it")
        );
    }
    for bad in ["\"0%\"", "\"101%\"", "\"80\"", "\"%\"", "\"8 0%\"", "-1"] {
        let text = minimal(&format!(
            "[[hooks]]\nname = \"h\"\npoint = \"context-threshold\"\nfrom = {bad}\n"
        ));
        let problem = problem(&text);
        assert!(
            problem.message.starts_with("invalid value"),
            "{bad}: {problem}"
        );
        assert_eq!(problem.line, Some(8), "{bad}");
    }
}

#[test]
fn hooks_name_a_point_of_the_world_or_of_a_plugin() {
    let unknown = minimal("[[hooks]]\nname = \"h\"\npoint = \"before-everything\"\n");
    assert_eq!(
        message(&unknown),
        "`before-everything` isn't a point of world 0.1.0; a plugin point is written <plugin>/<point>"
    );
    let malformed = minimal("[[hooks]]\nname = \"h\"\npoint = \"Other/p\"\n");
    assert_eq!(
        message(&malformed),
        "`Other/p` isn't a point of world 0.1.0; a plugin point is written <plugin>/<point>"
    );
    let undeclared = minimal("[[hooks]]\nname = \"h\"\npoint = \"acme/p\"\n");
    assert_eq!(
        message(&undeclared),
        "`acme/p` names this plugin, which declares no point `p`"
    );
}

#[test]
fn an_unknown_point_kind_or_capability_is_refused_where_it_is_written() {
    let kind = minimal("[[points]]\nname = \"p\"\nkind = \"decide\"\nversion = \"1\"\n");
    let problem = problem(&kind);
    assert!(
        problem.message.starts_with("unknown variant `decide`"),
        "{problem}"
    );
    assert_eq!(problem.line, Some(7));
    let capability = minimal("[capabilities]\nrequested = [\"gpu\"]\n");
    assert!(message(&capability).starts_with("unknown variant `gpu`"));
}

fn raw_has_comment(raw: Option<&RawString>) -> bool {
    raw.and_then(RawString::as_str)
        .is_some_and(|s| s.contains('#'))
}

fn decor_has_comment(decor: &Decor) -> bool {
    raw_has_comment(decor.prefix()) || raw_has_comment(decor.suffix())
}

fn value_has_comment(value: &Value) -> bool {
    if decor_has_comment(value.decor()) {
        return true;
    }
    match value {
        Value::Array(array) => array_has_comment(array),
        Value::InlineTable(table) => inline_has_comment(table),
        _ => false,
    }
}

fn array_has_comment(array: &Array) -> bool {
    raw_has_comment(Some(array.trailing())) || array.iter().any(value_has_comment)
}

fn inline_has_comment(table: &InlineTable) -> bool {
    raw_has_comment(Some(table.trailing()))
        || table.iter().any(|(key, value)| {
            table.key(key).is_some_and(|k| {
                decor_has_comment(k.leaf_decor()) || decor_has_comment(k.dotted_decor())
            }) || value_has_comment(value)
        })
}

fn item_has_comment(item: &Item) -> bool {
    match item {
        Item::None => false,
        Item::Value(value) => value_has_comment(value),
        Item::Table(table) => table_has_comment(table),
        Item::ArrayOfTables(tables) => tables.iter().any(table_has_comment),
    }
}

fn table_has_comment(table: &Table) -> bool {
    decor_has_comment(table.decor())
        || table.iter().any(|(key, item)| {
            table.key(key).is_some_and(|k| {
                decor_has_comment(k.leaf_decor()) || decor_has_comment(k.dotted_decor())
            }) || item_has_comment(item)
        })
}

fn has_comment(text: &str) -> bool {
    let document: DocumentMut = text.parse().unwrap();
    raw_has_comment(Some(document.trailing())) || table_has_comment(document.as_table())
}

#[test]
fn the_comment_check_finds_a_comment_wherever_toml_allows_one() {
    assert!(!has_comment(CONTEXT));
    assert!(!has_comment(FULL));
    assert!(!has_comment("text = \"a # in a string\"\n"));
    for commented in [
        "# top\nid = \"a\"\n",
        "id = \"a\" # after\n",
        "id = \"a\"\n# trailing\n",
        "[system] # header\ntext = \"x\"\n",
        "# before a table\n[system]\ntext = \"x\"\n",
        "[[hooks]]\nname = \"h\"\n\n# between\n[[hooks]]\nname = \"g\"\n",
        "list = [\n  \"a\", # in an array\n  \"b\",\n]\n",
        "list = [\n  \"a\",\n  # at the end of an array\n]\n",
        "[system]\n# inside a table\ntext = \"x\"\n",
    ] {
        assert!(has_comment(commented), "{commented}");
    }
}

fn bundled_manifests() -> Vec<PathBuf> {
    let plugins = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
    let mut found = Vec::new();
    for entry in std::fs::read_dir(&plugins).unwrap() {
        let path = entry.unwrap().path().join("wisp-plugin.toml");
        if path.is_file() {
            found.push(path);
        }
    }
    found.sort();
    found
}

#[test]
fn every_bundled_manifest_parses_and_holds_no_comment() {
    for path in bundled_manifests() {
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!has_comment(&text), "{} holds a # comment", path.display());
        if let Err(problem) = Manifest::parse(&text, Origin::Bundled) {
            panic!("{}: {problem}", path.display());
        }
    }
}

#[test]
fn agent_points_are_observe_points_of_world_0_2_0_and_later() {
    let in_world = |world: &str, hook: &str| {
        format!(
            "id = \"acme\"\nversion = \"0.1.0\"\nworld = \"{world}\"\nsummary = \"Acme\"\n[[hooks]]\nname = \"h\"\n{hook}"
        )
    };
    for point in AGENT_POINTS {
        let old = in_world("0.1.0", &format!("point = \"{point}\"\n"));
        assert_eq!(
            message(&old),
            format!("`{point}` is an agent point, which needs world 0.2.0 or later")
        );
        for world in ["0.2.0", "0.3.0"] {
            let new = in_world(
                world,
                &format!("point = \"{point}\"\nbackground = true\ntimeout_ms = 10000\n"),
            );
            assert!(parse(&new).is_ok(), "{point} in {world}");
        }
        let reserve = in_world(
            "0.2.0",
            &format!("point = \"{point}\"\nreserve = \"Sent: x\"\n"),
        );
        assert_eq!(
            message(&reserve),
            format!(
                "hook `h` declares reserve at `{point}`; only before-turn and compaction-head take it"
            )
        );
        let from = in_world("0.2.0", &format!("point = \"{point}\"\nfrom = 1000\n"));
        assert_eq!(
            message(&from),
            format!("hook `h` declares from at `{point}`; only context-threshold takes it")
        );
        let sets = in_world(
            "0.2.0",
            &format!("point = \"{point}\"\nin = [\"core/state\"]\n"),
        );
        assert_eq!(
            message(&sets),
            format!("hook `h` declares in at `{point}`; only before-turn takes it")
        );
    }
}

#[test]
fn a_strategy_table_is_read_in_world_0_2_0_only_and_its_summary_is_checked() {
    let in_world = |world: &str, summary: &str| {
        format!(
            "id = \"acme\"\nversion = \"0.1.0\"\nworld = \"{world}\"\nsummary = \"Acme\"\n[strategy]\nsummary = \"{summary}\"\n"
        )
    };
    let manifest = parse(&in_world("0.2.0", "Short notes")).unwrap();
    assert_eq!(manifest.strategy.unwrap().summary, "Short notes");
    assert!(parse(&in_world("0.2.0", "x")).is_ok());
    assert!(parse(&minimal("")).unwrap().strategy.is_none());
    assert!(message(&in_world("0.1.0", "Short notes")).starts_with("unknown field `strategy`"));
    assert_eq!(
        message(&in_world("0.2.0", "  ")),
        "the strategy's summary is blank"
    );
    assert_eq!(
        message(&in_world("0.2.0", "tab\\there")),
        "the strategy's summary holds a control character; it must be one line of text"
    );
    assert_eq!(
        message(&in_world("0.2.0", &"w".repeat(61))),
        "the strategy's summary is 61 characters long; the limit is 60"
    );
    assert!(parse(&in_world("0.2.0", &"w".repeat(60))).is_ok());
}

fn rows(rest: &str) -> String {
    minimal(rest).replace("world = \"0.1.0\"", "world = \"0.3.0\"")
}

#[test]
fn a_plugin_declares_its_setting_rows_under_world_0_3_0() {
    let inline = ROW.lines().skip(1).collect::<Vec<_>>().join(", ");
    let text = rows(&format!(
        "settings = [\"context.context_note\", {{{inline}}}]"
    ));
    let manifest = parse(&text).unwrap();
    assert_eq!(
        manifest.names().collect::<Vec<_>>(),
        ["context.context_note"]
    );
    assert_eq!(
        manifest.rows().cloned().collect::<Vec<_>>(),
        [SettingRow {
            name: "minutes".into(),
            label: "Idle after".into(),
            help: "Minutes before away.".into(),
            kind: RowKind::Int,
            min: Some(1),
            max: Some(60),
            default: RowDefault::Int(10),
        }]
    );
    for key in ["name", "label", "help", "kind", "default"] {
        let missing = ROW
            .lines()
            .filter(|line| !line.starts_with(&format!("{key} = ")))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            message(&rows(&missing)).contains(&format!("missing field `{key}`")),
            "{key}"
        );
    }
}

#[test]
fn a_setting_row_is_refused_below_world_0_3_0_where_it_is_written() {
    for world in ["0.1.0", "0.2.0"] {
        let text = rows(ROW).replace("0.3.0", world);
        support::refused_at(
            &text,
            Origin::External,
            "[[settings]]",
            "invalid type: map, expected a string",
        );
    }
}

#[test]
fn a_row_is_declared_once_and_its_label_and_help_are_short_single_lines() {
    assert_eq!(
        message(&rows(&format!("{ROW}\n{ROW}"))),
        "setting `minutes` is declared twice"
    );
    for (key, original, limit) in [
        ("label", "Idle after", 24),
        ("help", "Minutes before away.", 160),
    ] {
        for blank in ["", " ", "\u{200b}"] {
            assert_eq!(
                message(&rows(&ROW.replace(original, blank))),
                format!("setting `minutes`'s {key} is blank")
            );
        }
        for control in [r"a\nb", r"a\tb", r"a\u007fb"] {
            assert_eq!(
                message(&rows(&ROW.replace(original, control))),
                format!(
                    "setting `minutes`'s {key} holds a control character; it must be one line of text"
                )
            );
        }
        let too_long = "界".repeat(limit + 1);
        assert_eq!(
            message(&rows(&ROW.replace(original, &too_long))),
            format!(
                "setting `minutes`'s {key} is {} characters long; the limit is {limit}",
                limit + 1
            )
        );
        assert!(
            parse(&rows(&ROW.replace(original, &"界".repeat(limit)))).is_ok(),
            "{key}"
        );
    }
}

#[test]
fn a_whole_number_row_needs_its_range_and_a_default_inside_it() {
    for field in ["min = 1\n", "max = 60\n"] {
        assert_eq!(
            message(&rows(&ROW.replace(field, ""))),
            "setting `minutes` needs min and max"
        );
    }
    assert_eq!(
        message(&rows(&ROW.replace("min = 1", "min = 61"))),
        "setting `minutes` has min above max"
    );
    for default in ["0", "61", "\"on\""] {
        let text = rows(&ROW.replace("default = 10", &format!("default = {default}")));
        assert_eq!(
            message(&text),
            "setting `minutes` needs a whole-number default from 1 to 60"
        );
    }
    for default in [1, 60] {
        let text = rows(&ROW.replace("default = 10", &format!("default = {default}")));
        assert!(parse(&text).is_ok(), "{default}");
    }
    let equal = ROW
        .replace("min = 1", "min = 60")
        .replace("default = 10", "default = 60");
    assert!(parse(&rows(&equal)).is_ok());
}

#[test]
fn an_on_off_row_takes_on_or_off_and_no_range() {
    let row = ROW
        .replace("kind = \"int\"", "kind = \"bool\"")
        .replace("min = 1\nmax = 60\n", "");
    assert_eq!(
        message(&rows(&row)),
        "setting `minutes` needs an on or off default"
    );
    let wrong_type = rows(&row.replace("default = 10", "default = true"));
    support::refused_at(
        &wrong_type,
        Origin::External,
        "true",
        "a whole number, or \"on\" or \"off\"",
    );
    for word in ["on", "off"] {
        let row = row.replace("default = 10", &format!("default = \"{word}\""));
        assert!(parse(&rows(&row)).is_ok(), "{word}");
        for range in ["min = 1", "max = 60"] {
            assert_eq!(
                message(&rows(&format!("{row}{range}\n"))),
                "setting `minutes` is bool and takes no min or max"
            );
        }
    }
}

#[test]
fn an_unknown_key_in_a_row_is_refused_where_it_is_written() {
    let text = rows(&format!("{ROW}colour = \"red\"\n"));
    let problem = problem(&text);
    assert!(
        problem.message.starts_with("unknown field `colour`"),
        "{problem}"
    );
    support::refused_at(&text, Origin::External, "colour", &problem.message);
}

#[test]
fn index_and_forwards_are_draft_only_even_when_they_are_false() {
    for world in ["0.1.0", "0.2.0", "0.3.0"] {
        let base = format!(
            "id = \"acme\"\nversion = \"0.1.0\"\nworld = \"{world}\"\nsummary = \"Acme\"\n"
        );
        for value in [true, false] {
            for addition in [
                format!("index = {value}\n"),
                format!("[[tools]]\nname = \"use\"\nforwards = {value}\n"),
            ] {
                let parsed = Manifest::parse(&format!("{base}{addition}"), Origin::External);
                if world == "0.3.0" {
                    let manifest = parsed.unwrap();
                    if manifest.tools.is_empty() {
                        assert_eq!(manifest.index, value);
                    } else {
                        assert_eq!(manifest.tools[0].forwards, value);
                    }
                } else {
                    assert!(parsed.unwrap_err().message.contains("unknown field"));
                }
            }
        }
    }
}

#[test]
fn a_native_tool_cannot_forward_calls() {
    let text = "id = \"acme\"\nversion = \"0.1.0\"\nworld = \"0.3.0\"\nsummary = \"Acme\"\n[[tools]]\nname = \"use\"\nforwards = true\nnative = \"web_search\"\n";
    assert_eq!(
        Manifest::parse(text, Origin::External).unwrap_err().message,
        "tool `use` is native and cannot forward a call"
    );
}

#[test]
fn usage_and_model_capabilities_need_world_0_4_0() {
    for (name, capability) in [("usage", Capability::Usage), ("model", Capability::Model)] {
        for world in ["0.1.0", "0.2.0", "0.3.0", "0.4.0"] {
            let text = minimal(&format!("[capabilities]\nrequested = [\"{name}\"]\n"))
                .replace("world = \"0.1.0\"", &format!("world = \"{world}\""));
            let parsed = Manifest::parse(&text, Origin::External);
            if world == "0.4.0" {
                assert_eq!(parsed.unwrap().capabilities.requested, [capability]);
            } else {
                assert!(parsed.unwrap_err().message.contains("needs world 0.4.0"));
            }
        }
    }
}

#[test]
fn a_held_tool_needs_world_0_4_0_and_keeps_forwards_in_0_3_0() {
    for world in ["0.1.0", "0.2.0", "0.3.0", "0.4.0"] {
        let text = minimal("[[tools]]\nname = \"manage\"\nheld = true\n")
            .replace("world = \"0.1.0\"", &format!("world = \"{world}\""));
        let parsed = Manifest::parse(&text, Origin::External);
        if world == "0.4.0" {
            assert!(parsed.unwrap().tools[0].held);
        } else {
            assert!(parsed.unwrap_err().message.contains("unknown field `held`"));
        }
    }
    let text = "id = \"acme\"\nversion = \"0.1.0\"\nworld = \"0.3.0\"\nsummary = \"Acme\"\n[[tools]]\nname = \"search\"\n[[tools]]\nname = \"use\"\nfollows = \"search\"\nforwards = true\n";
    let tools = Manifest::parse(text, Origin::External).unwrap().tools;
    assert!(tools[1].forwards && !tools[1].held && !tools[0].held);
}

#[test]
fn usage_changed_is_an_observe_point_from_world_0_4_0() {
    for world in ["0.1.0", "0.2.0", "0.3.0", "0.4.0"] {
        for body in [
            "[[hooks]]\nname = \"usage\"\npoint = \"usage-changed\"\nbackground = true\n",
            "[[slots]]\nname = \"usage\"\nslot = \"sidebar\"\ntitle = \"Usage\"\nmarks = [\"usage-changed\"]\n",
        ] {
            let text = minimal(body).replace("world = \"0.1.0\"", &format!("world = \"{world}\""));
            let parsed = Manifest::parse(&text, Origin::External);
            assert_eq!(parsed.is_ok(), world == "0.4.0", "{world}: {parsed:?}");
        }
    }
}

#[test]
fn dynamic_tools_has_a_valid_prefix_only_in_world_0_4_0() {
    let base = r#"id = "acme"
version = "1.0.0"
world = "0.4.0"
summary = "Server tools"
"#;
    let text = format!("{base}dynamic_tools = \"mcp__\"\n");
    let manifest = Manifest::parse(&text, Origin::External).unwrap();
    assert_eq!(manifest.dynamic_tools.as_deref(), Some("mcp__"));
    for prefix in ["", "bad prefix", "bad.prefix", "tool/", &"x".repeat(129)] {
        assert!(
            Manifest::parse(
                &format!("{base}dynamic_tools = {prefix:?}\n"),
                Origin::External
            )
            .is_err()
        );
    }
    for world in ["0.1.0", "0.2.0", "0.3.0"] {
        assert!(Manifest::parse(&text.replace("0.4.0", world), Origin::External).is_err());
    }
}

#[test]
fn a_dynamic_prefix_does_not_add_a_static_tool_or_change_an_older_manifest() {
    let text = r#"id = "acme"
version = "1.0.0"
world = "0.4.0"
summary = "Server tools"
dynamic_tools = "mcp__"
"#;
    let manifest = Manifest::parse(text, Origin::External).unwrap();
    assert!(manifest.tools.is_empty());
    assert!(
        Manifest::parse(CONTEXT, Origin::Bundled)
            .unwrap()
            .dynamic_tools
            .is_none()
    );
}
