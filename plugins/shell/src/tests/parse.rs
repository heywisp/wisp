use serde_json::{Value, json};
use wisp_plugin_api::types::{Key, NamedKey, ReadKind, SignalKind};
use wisp_tool_text::jobs::no_process;

use crate::process::input::{
    ATTACH_ALONE, Call, INPUT_QUIET_MS, INPUT_WAIT_MS, INTERRUPT_QUIET_MS, INTERRUPT_WAIT_MS,
    NO_ID, UNTIL_IGNORED, parse,
};

fn ok(input: Value) -> Call {
    parse(&input).unwrap()
}

fn refused(input: Value) -> String {
    parse(&input).unwrap_err()
}

#[test]
fn without_an_id_it_lists_and_takes_nothing_but_read() {
    assert_eq!(ok(json!({})).id, None);
    assert_eq!(ok(json!({"read": "tail"})).id, None);
    assert_eq!(refused(json!({"wait_ms": 1000})), NO_ID);
    assert_eq!(refused(json!({"read": "tail", "lines": 3})), NO_ID);
    assert_eq!(refused(json!({"id": null, "input": "x"})), NO_ID);
    assert_eq!(ok(json!({"id": null})).id, None);
    assert_eq!(ok(json!({"read": "screen"})).id, None);
    assert_eq!(refused(json!({"size": "80x24"})), NO_ID);
    assert_eq!(
        refused(json!({"read": "page"})),
        "`read` must be new, tail, lines, search or screen, not `page`"
    );
}

#[test]
fn size_and_read_screen_parse_and_are_bounded() {
    let call = ok(json!({"id": 1, "size": "120x40", "read": "screen"}));
    assert_eq!(
        call.size,
        Some(wisp_plugin_api::host::Size {
            cols: 120,
            rows: 40
        })
    );
    assert_eq!(call.read, Some(ReadKind::Screen));
    assert!(call.changes());
    assert!(!call.writes());
    assert_eq!(
        refused(json!({"id": 1, "size": "big"})),
        "`size` must be COLUMNSxROWS, such as 120x40, not `big`"
    );
    assert_eq!(
        refused(json!({"id": 1, "size": "501x40"})),
        "`size` must be from 20x5 to 500x200, not 501x40"
    );
    assert_eq!(
        refused(json!({"id": 1, "size": "80x4"})),
        "`size` must be from 20x5 to 500x200, not 80x4"
    );
    assert_eq!(
        refused(json!({"id": 1, "size": 80})),
        "`size` must be a string"
    );
}

#[test]
fn unknown_fields_and_bad_values_are_invalid() {
    assert_eq!(
        refused(json!({"id": 1, "attach": "why"})),
        "unknown field `attach`"
    );
    assert_eq!(
        ok(json!({"id": 1, "ask_user_to_attach": "why"}))
            .attach
            .as_deref(),
        Some("why")
    );
    assert_eq!(refused(json!({"id": 0})), no_process(0));
    assert_eq!(refused(json!({"id": -2})), no_process(-2));
    assert_eq!(refused(json!({"id": "1"})), "`id` must be a whole number");
    assert_eq!(
        refused(json!({"id": 1, "signal": "term"})),
        "`signal` must be interrupt or kill, not `term`"
    );
    assert_eq!(
        refused(json!({"id": 1, "timeout_ms": -1})),
        "`timeout_ms` must be 0 or more milliseconds"
    );
    assert_eq!(
        refused(json!({"id": 1, "input": "x".repeat(64 * 1024 + 1)})),
        "`input` is 65,537 bytes; the limit is 65,536 bytes"
    );
    assert!(
        refused(json!({"id": 1, "read": "search", "pattern": "("}))
            .starts_with("`pattern` is not a valid regular expression: ")
    );
    assert!(
        refused(json!({"id": 1, "wait_ms": 10, "until": "[z-a]"}))
            .starts_with("`until` is not a valid regular expression: ")
    );
}

#[test]
fn keys_are_named_and_eof_comes_last() {
    let call = ok(json!({"id": 1, "keys": ["Enter", "C-c", "F12", "Up", "EOF"]}));
    assert_eq!(
        call.keys,
        [
            Key::Named(NamedKey::Enter),
            Key::Ctrl('c'),
            Key::Function(12),
            Key::Named(NamedKey::Up),
            Key::Eof
        ]
    );
    assert_eq!(
        refused(json!({"id": 1, "keys": ["EOF", "Enter"]})),
        "EOF closes stdin, so it must be the last key"
    );
    let unknown = refused(json!({"id": 1, "keys": ["Ctrl-C"]}));
    assert!(unknown.starts_with("unknown key `Ctrl-C`; the keys are Enter, Tab, Space, Esc"));
    assert!(unknown.ends_with("F1 to F12, C-a to C-z, EOF"));
    assert!(refused(json!({"id": 1, "keys": ["F13"]})).starts_with("unknown key `F13`"));
    assert!(refused(json!({"id": 1, "keys": ["F01"]})).starts_with("unknown key `F01`"));
    assert_eq!(
        refused(json!({"id": 1, "keys": "Enter"})),
        "`keys` must be a list of key names"
    );
}

#[test]
fn read_fields_belong_to_their_views() {
    assert_eq!(
        refused(json!({"id": 1, "from": 3})),
        "`from` only applies to read lines or read search"
    );
    assert_eq!(
        refused(json!({"id": 1, "read": "tail", "from": 3})),
        "`from` only applies to read lines or read search"
    );
    assert_eq!(
        refused(json!({"id": 1, "read": "search"})),
        "read search needs a `pattern`"
    );
    assert_eq!(
        refused(json!({"id": 1, "pattern": "x"})),
        "`pattern` only applies to read search"
    );
    assert_eq!(
        refused(json!({"id": 1, "read": "lines", "context": 2})),
        "`context` only applies to read search"
    );
    assert_eq!(
        refused(json!({"id": 1, "lines": 5})),
        "`lines` only applies to read tail, lines or search"
    );
    assert_eq!(
        refused(json!({"id": 1, "read": "lines", "lines": 0})),
        "`lines` must be 1 or more"
    );
    assert_eq!(
        refused(json!({"id": 1, "read": "lines", "from": 0})),
        "`from` must be 1 or more"
    );
    let call = ok(
        json!({"id": 1, "read": "search", "pattern": "(?i)err", "context": 30, "lines": 900, "from": 7}),
    );
    assert_eq!(call.read_kind(), ReadKind::Search);
    assert_eq!(call.context, 10);
    assert_eq!(call.lines, Some(500));
    assert_eq!(call.from, Some(7));
    assert_eq!(
        call.notes,
        [
            "context 30 is outside 0 to 10, so 10 was used.",
            "lines 900 is outside 1 to 500, so 500 was used."
        ]
    );
}

#[test]
fn waits_default_after_writes_and_interrupts_and_clamp_with_a_note() {
    let plain = ok(json!({"id": 1}));
    assert_eq!((plain.wait_ms, plain.quiet_ms), (0, None));
    let written = ok(json!({"id": 1, "input": "x"}));
    assert_eq!(
        (written.wait_ms, written.quiet_ms),
        (INPUT_WAIT_MS, Some(INPUT_QUIET_MS))
    );
    let interrupted = ok(json!({"id": 1, "signal": "interrupt"}));
    assert_eq!(interrupted.signal, Some(SignalKind::Interrupt));
    assert_eq!(
        (interrupted.wait_ms, interrupted.quiet_ms),
        (INTERRUPT_WAIT_MS, Some(INTERRUPT_QUIET_MS))
    );
    let killed = ok(json!({"id": 1, "signal": "kill"}));
    assert_eq!((killed.wait_ms, killed.quiet_ms), (0, None));
    let long = ok(json!({"id": 1, "wait_ms": 700_000, "quiet_ms": -5}));
    assert_eq!((long.wait_ms, long.quiet_ms), (600_000, Some(0)));
    assert_eq!(
        long.notes,
        [
            "wait_ms 700000 is outside 0 to 600,000, so 600,000 was used.",
            "quiet_ms -5 is outside 0 to 600,000, so 0 was used."
        ]
    );
    let ignored = ok(json!({"id": 1, "until": "x", "quiet_ms": 10}));
    assert!(ignored.until.is_none());
    assert_eq!(ignored.quiet_ms, None);
    assert_eq!(ignored.notes, [UNTIL_IGNORED]);
    let explicit = ok(json!({"id": 1, "input": "x", "wait_ms": 0, "until": "y"}));
    assert_eq!(explicit.wait_ms, 0);
    assert_eq!(explicit.notes, [UNTIL_IGNORED]);
    let limit = ok(json!({"id": 1, "timeout_ms": 900_000_000}));
    assert_eq!(limit.timeout_ms, Some(900_000_000));
    assert!(limit.notes.is_empty());
}

#[test]
fn a_description_goes_with_every_call_trimmed_and_blank_means_none() {
    let described = |input: Value| ok(input).description;
    let words = Some("Check the dev server".to_owned());
    assert_eq!(
        described(json!({"description": " Check the dev server "})),
        words
    );
    assert_eq!(
        described(json!({"read": "tail", "description": "Check the dev server"})),
        words
    );
    for action in [
        json!({"id": 1, "input": "yes", "keys": ["Enter"]}),
        json!({"id": 1, "keys": ["Up"]}),
        json!({"id": 1, "signal": "interrupt"}),
        json!({"id": 1, "wait_ms": 1000}),
        json!({"id": 1, "read": "search", "pattern": "ERROR"}),
        json!({"id": 1, "read": "screen"}),
        json!({"id": 1, "size": "120x40"}),
        json!({"id": 1, "timeout_ms": 0}),
        json!({"id": 1, "ask_user_to_attach": "log in"}),
        json!({"id": 1, "read": "screen", "ask_user_to_attach": "log in"}),
    ] {
        let mut input = action;
        input["description"] = json!("Check the dev server");
        assert_eq!(described(input.clone()), words, "{input}");
    }
    assert_eq!(described(json!({"id": 1, "description": "  "})), None);
    assert_eq!(described(json!({"id": 1, "description": null})), None);
    assert_eq!(described(json!({"id": 1})), None);
    assert_eq!(
        refused(json!({"id": 1, "description": 3})),
        "`description` must be a string"
    );
    assert_eq!(
        refused(json!({"description": ["x"]})),
        "`description` must be a string"
    );
}

#[test]
fn the_list_and_attach_refusals_still_fire_and_name_description() {
    assert!(NO_ID.ends_with("takes nothing but read and description"));
    assert_eq!(
        ATTACH_ALONE,
        "only `read` and `description` may accompany `ask_user_to_attach`"
    );
    assert_eq!(
        refused(json!({"description": "List them", "wait_ms": 1000})),
        NO_ID
    );
    assert_eq!(
        refused(
            json!({"id": 1, "ask_user_to_attach": "log in", "description": "Log in", "input": "x"})
        ),
        ATTACH_ALONE
    );
    assert_eq!(
        refused(json!({"id": 1, "ask_user_to_attach": "log in", "keys": ["Enter"]})),
        ATTACH_ALONE
    );
}

#[test]
fn the_schema_offers_every_read_kind() {
    let schema = crate::process::schema(false);
    let names: Vec<&str> = ReadKind::ALL.iter().map(|kind| kind.name()).collect();
    assert_eq!(schema["properties"]["read"]["enum"], json!(names));
}
