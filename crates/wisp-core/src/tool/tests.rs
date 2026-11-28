use std::path::PathBuf;

use futures::FutureExt;
use serde_json::json;

use super::*;
use crate::plugin::testing::{self, TestTool};

struct Dummy {
    name: String,
    schema: Value,
}

impl Dummy {
    fn tool(name: &str, schema: Value) -> ToolEntry {
        testing::entry(Arc::new(Self {
            name: name.into(),
            schema,
        }))
    }
}

impl TestTool for Dummy {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn schema(&self) -> Value {
        self.schema.clone()
    }

    fn meta(&self) -> wisp_plugin_api::tools::ToolMeta {
        wisp_plugin_api::tools::ToolMeta {
            parallel_safe: true,
            ..testing::meta(1_000, Some(2_000))
        }
    }

    fn prepare(&self, _input: &str) -> Result<Box<dyn PluginPrepared>, String> {
        Err("dummy".to_owned())
    }

    fn run<'a>(
        &'a self,
        _call: Box<dyn PluginPrepared>,
        _ctx: &'a dyn wisp_plugin_api::host::CallContext,
    ) -> BoxFuture<'a, wisp_plugin_api::types::ToolRun> {
        async { wisp_plugin_api::types::ToolRun::completed() }.boxed()
    }
}

#[test]
fn tools_registered_out_of_order_serialise_sorted_and_canonical() {
    let zeta = Dummy::tool(
        "zeta",
        json!({"type": "object", "required": ["b"], "properties": {"b": {"type": "string"}, "a": {"type": "integer"}}}),
    );
    let alpha = Dummy::tool("alpha", json!({"type": "object", "properties": {}}));
    let toolbox = Toolbox::new([zeta.clone(), alpha.clone(), zeta]);
    assert_eq!(toolbox.names(), ["alpha", "zeta"]);
    assert_eq!(
        toolbox.canonical_json(),
        r#"[{"description":"The alpha tool.","input_schema":{"properties":{},"type":"object"},"name":"alpha"},{"description":"The zeta tool.","input_schema":{"properties":{"a":{"type":"integer"},"b":{"type":"string"}},"required":["b"],"type":"object"},"name":"zeta"}]"#
    );
    let again = Toolbox::new([
        alpha,
        Dummy::tool(
            "zeta",
            json!({"type": "object", "properties": {"a": {"type": "integer"}, "b": {"type": "string"}}, "required": ["b"]}),
        ),
    ]);
    assert_eq!(again.canonical_json(), toolbox.canonical_json());
    assert_eq!(again.sha256(), toolbox.sha256());
    assert_eq!(Toolbox::default().sha256(), None);
}

#[test]
fn streaming_tool_arrays_match_canonical_values() {
    let toolbox = testkit::bundled(&["bash", "view_file"]);
    let view = toolbox.get("view_file").unwrap().spec().clone();
    let tools = [
        ToolSpec {
            name: "zeta".into(),
            description: "Café \"quoted\" \\path\nnext".into(),
            input_schema: json!({"z": [{"b": "é", "a": "\\"}], "a": {"y": 1, "b": null}}),
        },
        view.clone(),
    ];
    let server = [ServerToolSpec {
        name: "middle".into(),
        kind: "server_type".into(),
    }];
    for (specs, deferred, server) in [
        (&[][..], &[][..], &[][..]),
        (tools.as_slice(), &[], &[]),
        (&[], &tools, &[]),
        (&[], &[], &server),
        (&tools, std::slice::from_ref(&view), &server),
    ] {
        let values: Vec<Value> = ordered(specs, deferred, server)
            .into_iter()
            .map(|entry| match entry {
                Entry::Client { spec, deferred } => {
                    let mut value = serde_json::to_value(spec).unwrap();
                    if deferred {
                        value["defer_loading"] = Value::Bool(true);
                    }
                    value
                }
                Entry::Server(spec) => serde_json::to_value(spec).unwrap(),
            })
            .collect();
        let canonical = serde_json::to_string(&values).unwrap();
        let digest = sha256_hex(&canonical);
        assert_eq!(array_json(specs, deferred, server), canonical);
        assert_eq!(array_json_len(specs, deferred, server), canonical.len());
        assert_eq!(array_json_sha256(specs, deferred, server), digest);
        assert_eq!(
            tools_sha256(specs, deferred, server),
            (!specs.is_empty() || !server.is_empty()).then_some(digest)
        );
    }
}

#[test]
fn prepare_answers_unknown_and_turned_off_tools() {
    let mut toolbox = Toolbox::new([Dummy::tool("alpha", json!({}))]);
    assert_eq!(
        futures::executor::block_on(toolbox.prepare("nope", &json!({})))
            .unwrap_err()
            .message,
        "there is no tool named `nope`"
    );
    assert_eq!(
        futures::executor::block_on(toolbox.prepare("alpha", &json!({})))
            .unwrap_err()
            .message,
        "dummy"
    );
    let search = crate::agent::testing::search_tool("web_search_20250305");
    assert_eq!(
        futures::executor::block_on(toolbox.prepare_after("web_search", &json!({}), search))
            .unwrap_err()
            .message,
        "web search didn't run for this request; answer without it"
    );
    assert_eq!(
        futures::executor::block_on(toolbox.prepare_after("web_search", &json!({}), search))
            .unwrap_err()
            .cause,
        InvalidCause::SearchNotRun
    );
    let named = Toolbox::new([Dummy::tool("web_search", json!({}))]);
    assert_eq!(
        futures::executor::block_on(named.prepare_after("web_search", &json!({}), search))
            .unwrap_err(),
        InvalidInput::new("dummy"),
        "a client tool of that name is prepared as itself"
    );
    assert_eq!(
        futures::executor::block_on(toolbox.prepare_after("nope", &json!({}), search))
            .unwrap_err()
            .message,
        "there is no tool named `nope`"
    );
    assert_eq!(
        futures::executor::block_on(toolbox.prepare("web_search", &json!({})))
            .unwrap_err()
            .message,
        "there is no tool named `web_search`"
    );
    toolbox.set_enabled("alpha", false);
    assert_eq!(
        futures::executor::block_on(toolbox.prepare("alpha", &json!({})))
            .unwrap_err()
            .message,
        "this tool is turned off"
    );
    assert_eq!(
        toolbox.names(),
        ["alpha"],
        "a turned-off tool stays declared"
    );
    toolbox.set_enabled("alpha", true);
    assert!(toolbox.is_enabled("alpha"));
}

#[test]
fn the_fallback_display_lists_the_inputs_keys_and_never_a_value() {
    let display = fallback_display("x", &json!({"b": "two\nlines", "a": 1}));
    assert_eq!(display.target, "{a, b}");
    assert_eq!(display.action, ToolAction::Other { name: "x".into() });
    assert_eq!(display.view, None);
    assert_eq!(display.title, None);
    let marker = "marker-5f1c2e";
    let secret = fallback_display("x", &json!({"id": 3, "input": marker, "keys": [marker]}));
    assert_eq!(secret.target, "{id, input, keys}");
    assert!(!serde_json::to_string(&secret).unwrap().contains(marker));
    assert_eq!(fallback_display("x", &json!({})).target, "{}");
    assert_eq!(fallback_display("x", &json!(marker)).target, "{}");
    assert_eq!(fallback_display("x", &json!([marker])).target, "{}");
    let long: serde_json::Map<String, Value> = (0..40)
        .map(|n| (format!("key{n:02}"), json!(marker)))
        .collect();
    let long = fallback_display("x", &Value::Object(long));
    assert_eq!(long.target.chars().count(), FALLBACK_TARGET_CHARS);
    assert!(long.target.ends_with('…'));
    assert!(!long.target.contains(marker));
    crate::session::register_secrets(["prefix CANARY-newline-held".to_owned()]);
    let is_safe = |text: &str| crate::plugin::host::lookup_is_safe(text, None, None);
    assert!(fallback_keys_safe(
        &json!({"b": "two\nlines", "a": 1}),
        is_safe
    ));
    assert!(fallback_keys_safe(&json!({}), is_safe));
    assert!(fallback_keys_safe(&json!(marker), is_safe));
    let span_key = format!("{}sk-ant-CANARY0123456789012345", "k.".repeat(33));
    let span = json!({span_key: 1});
    assert!(is_safe(&raw_fallback_display("x", &span).target));
    assert!(!fallback_keys_safe(&span, is_safe));
    assert_eq!(fallback_display("x", &span).target, "{}");
    let bad_name = fallback_display("sk-ant-CANARY0123456789012345", &json!({"a": 1}));
    assert_eq!(
        bad_name.action,
        ToolAction::Other {
            name: "tool".into()
        }
    );
    assert_eq!(bad_name.target, "{}");
    let mut past: serde_json::Map<String, Value> =
        (0..20).map(|n| (format!("k{n:02}"), json!(1))).collect();
    past.insert("z_user@example.invalid".to_owned(), json!(1));
    let past = Value::Object(past);
    assert!(!fallback_keys_safe(&past, is_safe));
    assert_eq!(fallback_display("x", &past).target, "{}");
    assert!(!fallback_keys_safe(
        &json!({"prefix\nCANARY-newline-held": 1}),
        is_safe
    ));
    let zw_name = fallback_display("user\u{200B}@example.invalid", &Value::Null);
    assert_eq!(
        zw_name.action,
        ToolAction::Other {
            name: "tool".into()
        }
    );
    assert_eq!(zw_name.target, "{}");
    let bom_name = fallback_display("sk-ant-\u{FEFF}CANARY0123456789012345", &Value::Null);
    assert_eq!(
        bom_name.action,
        ToolAction::Other {
            name: "tool".into()
        }
    );
    assert_eq!(bom_name.target, "{}");
    assert_eq!(
        fallback_display("x", &json!({"sk-ant-\u{200B}CANARY0123456789012345": 1})).target,
        "{}"
    );
    assert_eq!(
        fallback_display("x", &json!({"user\u{FEFF}@example.invalid": 1})).target,
        "{}"
    );
    crate::session::register_secrets(["prefix CANARY-tab-held".to_owned()]);
    assert_eq!(
        fallback_display("x", &json!({"prefix\tCANARY-tab-held": 1})).target,
        "{}"
    );
}

#[test]
fn canonical_fallback_and_clipping_cover_other_name_code_lang_and_view_fields() {
    use wisp_protocol::SignalKind;

    let canonical = |name: &str| ToolDisplay {
        action: ToolAction::Other {
            name: name.to_owned(),
        },
        verbs: None,
        target: "{}".to_owned(),
        output: OutputKind::Text,
        view: None,
        title: None,
        purpose: ToolPurpose::Run,
    };
    assert!(is_canonical_data_free_fallback("plug", &canonical("tool")));
    assert!(is_canonical_data_free_fallback(
        "plug",
        &canonical(crate::mcp::PLUGIN)
    ));
    assert!(is_canonical_data_free_fallback("plug", &canonical("plug")));
    assert!(!is_canonical_data_free_fallback(
        "plug",
        &canonical("other_plug")
    ));
    assert!(!is_canonical_data_free_fallback(
        "plug",
        &ToolDisplay {
            output: OutputKind::Code {
                lang: "rust".to_owned(),
            },
            ..canonical("plug")
        }
    ));
    assert!(!is_canonical_data_free_fallback(
        "plug",
        &ToolDisplay {
            target: "{a}".to_owned(),
            ..canonical("plug")
        }
    ));
    assert!(!is_canonical_data_free_fallback(
        "plug",
        &ToolDisplay {
            title: Some("t".to_owned()),
            ..canonical("plug")
        }
    ));
    assert!(!is_canonical_data_free_fallback(
        "plug",
        &ToolDisplay {
            verbs: Some(Verbs {
                doing: "a".to_owned(),
                done: "b".to_owned(),
                failed: "c".to_owned(),
            }),
            ..canonical("plug")
        }
    ));
    assert!(!is_canonical_data_free_fallback(
        "plug",
        &ToolDisplay {
            view: Some(ToolView::Code {
                language: None,
                code: "x".to_owned(),
            }),
            ..canonical("plug")
        }
    ));

    let clean_diff = DiffView {
        path: Some("a.rs".to_owned()),
        old_path: Some("old.rs".to_owned()),
        language: Some("rust".to_owned()),
        hunks: "@@ -1 +1 @@".to_owned(),
        added: 1,
        removed: 1,
    };
    let make_process =
        |text: &str, keys: Vec<String>, read: &str, pattern: &str| ToolView::Process {
            id: Some(1),
            text: Some(text.to_owned()),
            keys,
            signal: Some(SignalKind::Interrupt),
            read: Some(read.to_owned()),
            pattern: Some(pattern.to_owned()),
        };
    let clean_process = make_process("hi", vec!["Enter".to_owned()], "tail", "ok");
    assert!(!display_is_clipped(&ToolDisplay {
        action: ToolAction::Other {
            name: "plug".to_owned(),
        },
        verbs: Some(Verbs {
            doing: "d".to_owned(),
            done: "n".to_owned(),
            failed: "f".to_owned(),
        }),
        target: "ok".to_owned(),
        output: OutputKind::Code {
            lang: "rust".to_owned(),
        },
        view: Some(ToolView::Diff {
            title: Some("diff".to_owned()),
            files: vec![clean_diff.clone()],
        }),
        title: Some("title".to_owned()),
        purpose: ToolPurpose::Run,
    }));
    assert!(!display_is_clipped(&ToolDisplay {
        view: Some(clean_process.clone()),
        ..canonical("plug")
    }));
    assert!(!display_is_clipped(&ToolDisplay {
        view: Some(ToolView::Code {
            language: Some("rust".to_owned()),
            code: "let x = 1;".to_owned(),
        }),
        ..canonical("plug")
    }));

    for clipped_display in [
        canonical("plug\u{2026}"),
        ToolDisplay {
            output: OutputKind::Code {
                lang: "rust\u{2026}".to_owned(),
            },
            ..canonical("plug")
        },
        ToolDisplay {
            view: Some(ToolView::Diff {
                title: Some("diff\u{2026}".to_owned()),
                files: vec![clean_diff.clone()],
            }),
            ..canonical("plug")
        },
        ToolDisplay {
            view: Some(ToolView::Diff {
                title: None,
                files: vec![DiffView {
                    path: Some("a\u{2026}".to_owned()),
                    ..clean_diff.clone()
                }],
            }),
            ..canonical("plug")
        },
        ToolDisplay {
            view: Some(ToolView::Diff {
                title: None,
                files: vec![DiffView {
                    old_path: Some("b\u{2026}".to_owned()),
                    ..clean_diff.clone()
                }],
            }),
            ..canonical("plug")
        },
        ToolDisplay {
            view: Some(ToolView::Diff {
                title: None,
                files: vec![DiffView {
                    language: Some("rs\u{2026}".to_owned()),
                    ..clean_diff.clone()
                }],
            }),
            ..canonical("plug")
        },
        ToolDisplay {
            view: Some(ToolView::Diff {
                title: None,
                files: vec![DiffView {
                    hunks: "h\u{2026}".to_owned(),
                    ..clean_diff
                }],
            }),
            ..canonical("plug")
        },
        ToolDisplay {
            view: Some(make_process(
                "t\u{2026}",
                vec!["Enter".to_owned()],
                "tail",
                "ok",
            )),
            ..canonical("plug")
        },
        ToolDisplay {
            view: Some(make_process(
                "hi",
                vec!["k\u{2026}".to_owned()],
                "tail",
                "ok",
            )),
            ..canonical("plug")
        },
        ToolDisplay {
            view: Some(make_process(
                "hi",
                vec!["Enter".to_owned()],
                "r\u{2026}",
                "ok",
            )),
            ..canonical("plug")
        },
        ToolDisplay {
            view: Some(make_process(
                "hi",
                vec!["Enter".to_owned()],
                "tail",
                "p\u{2026}",
            )),
            ..canonical("plug")
        },
        ToolDisplay {
            view: Some(ToolView::Code {
                language: Some("py\u{2026}".to_owned()),
                code: "x".to_owned(),
            }),
            ..canonical("plug")
        },
        ToolDisplay {
            view: Some(ToolView::Code {
                language: None,
                code: "x\u{2026}".to_owned(),
            }),
            ..canonical("plug")
        },
    ] {
        assert!(display_is_clipped(&clipped_display), "{clipped_display:?}");
    }

    let empty_held = crate::session::Redactions::default();
    let unsafe_key = "sk-ant-CANARY0123456789012345";

    let mut d1 = ToolDisplay {
        purpose: ToolPurpose::Show,
        ..canonical("sec.\u{2026}")
    };
    scrub_tool_call_display(
        &format!("sec.{unsafe_key}"),
        &json!({"a": 1}),
        &mut d1,
        None,
        &empty_held,
        true,
    );
    assert_eq!(
        d1.action,
        ToolAction::Other {
            name: "tool".into()
        }
    );
    assert_eq!(d1.target, "{}");
    assert_eq!(d1.purpose, ToolPurpose::Show);

    let mut d2 = canonical("plug_\u{2026}");
    scrub_tool_call_display(
        "plug_long",
        &json!({"k": unsafe_key}),
        &mut d2,
        None,
        &empty_held,
        true,
    );
    assert_eq!(
        d2.action,
        ToolAction::Other {
            name: "tool".into()
        }
    );
    assert_eq!(d2.target, "{}");

    let mut d3 = ToolDisplay {
        target: "{a, sec\u{2026}}".to_owned(),
        ..canonical("plug")
    };
    scrub_tool_call_display(
        unsafe_key,
        &json!({"a": 1}),
        &mut d3,
        None,
        &empty_held,
        true,
    );
    assert_eq!(
        d3.action,
        ToolAction::Other {
            name: "tool".into()
        }
    );
    assert_eq!(d3.target, "{}");

    let mut d4 = ToolDisplay {
        target: "{a, val\u{2026}}".to_owned(),
        ..canonical("plug")
    };
    scrub_tool_call_display(
        "plug",
        &json!({"a": unsafe_key}),
        &mut d4,
        None,
        &empty_held,
        true,
    );
    assert_eq!(
        d4.action,
        ToolAction::Other {
            name: "plug".into()
        }
    );
    assert_eq!(d4.target, "{}");

    let combined_held = crate::session::Redactions::new(["prefix CANARY-crlf-zw".to_owned()]);
    assert!(!display_text_is_safe_with_held(
        "prefix\r\n\u{200B}CANARY-crlf-zw",
        None,
        &combined_held
    ));
    assert!(text_has_held_match(
        "prefix\r\n\u{200B}CANARY-crlf-zw",
        &combined_held
    ));

    let mut collapsed = ToolDisplay {
        target: "{safe_key}".to_owned(),
        ..canonical("plug")
    };
    scrub_tool_call_display(
        "plug",
        &json!("[redacted]"),
        &mut collapsed,
        None,
        &empty_held,
        true,
    );
    assert_eq!(collapsed.target, "{}");

    let mut redacted_key = ToolDisplay {
        target: "{[redacted]}".to_owned(),
        ..canonical("plug")
    };
    scrub_tool_call_display(
        "plug",
        &json!({"[redacted]": "v"}),
        &mut redacted_key,
        None,
        &empty_held,
        true,
    );
    assert_eq!(redacted_key.target, "{}");

    let mut clipped_redacted_val = ToolDisplay {
        target: "{a, val_prefix_\u{2026}}".to_owned(),
        ..canonical("plug")
    };
    scrub_tool_call_display(
        "plug",
        &json!({"a": "[redacted]"}),
        &mut clipped_redacted_val,
        None,
        &empty_held,
        true,
    );
    assert_eq!(clipped_redacted_val.target, "{}");

    let mut unclipped_redacted_val = ToolDisplay {
        target: "{a}".to_owned(),
        ..canonical("plug")
    };
    scrub_tool_call_display(
        "plug",
        &json!({"a": "[redacted]"}),
        &mut unclipped_redacted_val,
        None,
        &empty_held,
        true,
    );
    assert_eq!(unclipped_redacted_val.target, "{a}");

    let write_clipped = ToolDisplay {
        action: ToolAction::Write,
        verbs: None,
        target: "prefix\u{2026}".to_owned(),
        output: OutputKind::None,
        view: None,
        title: None,
        purpose: ToolPurpose::Run,
    };
    let mut refused_redacted_write = write_clipped.clone();
    scrub_tool_call_display(
        "board_write",
        &json!({"board": "[redacted]"}),
        &mut refused_redacted_write,
        None,
        &empty_held,
        true,
    );
    assert_eq!(
        refused_redacted_write.action,
        ToolAction::Other {
            name: "tool".to_owned()
        }
    );
    assert_eq!(refused_redacted_write.target, "{}");

    let mut valid_redacted_write = write_clipped;
    scrub_tool_call_display(
        "board_write",
        &json!({"board": "[redacted]"}),
        &mut valid_redacted_write,
        None,
        &empty_held,
        false,
    );
    assert_eq!(valid_redacted_write.action, ToolAction::Write);
    assert_eq!(valid_redacted_write.target, "prefix\u{2026}");

    let mut braced_ascii_dots = ToolDisplay {
        target: "{a: dir/alice@exam...}".to_owned(),
        ..canonical("plug")
    };
    scrub_tool_call_display(
        "plug",
        &json!({"a": "[redacted]"}),
        &mut braced_ascii_dots,
        None,
        &empty_held,
        true,
    );
    assert_eq!(braced_ascii_dots.target, "{}");

    let mut braced_first_line = ToolDisplay {
        target: "{a: line_one_secret}".to_owned(),
        ..canonical("plug")
    };
    scrub_tool_call_display(
        "plug",
        &json!({"a": "[redacted]"}),
        &mut braced_first_line,
        None,
        &empty_held,
        true,
    );
    assert_eq!(braced_first_line.target, "{}");

    let mut refused_redacted_tool_name = ToolDisplay {
        target: "{a}".to_owned(),
        ..canonical("line_one_tool")
    };
    scrub_tool_call_display(
        "[redacted]",
        &json!({"a": 1}),
        &mut refused_redacted_tool_name,
        None,
        &empty_held,
        true,
    );
    assert_eq!(
        refused_redacted_tool_name.action,
        ToolAction::Other {
            name: "tool".to_owned()
        }
    );
    assert_eq!(refused_redacted_tool_name.target, "{}");

    for bad_display in [
        ToolDisplay {
            action: ToolAction::Show,
            verbs: None,
            target: "dir/alice@exam...".to_owned(),
            output: OutputKind::None,
            view: None,
            title: None,
            purpose: ToolPurpose::Show,
        },
        ToolDisplay {
            action: ToolAction::Show,
            verbs: None,
            target: "first_line_secret".to_owned(),
            output: OutputKind::None,
            view: None,
            title: Some("first_line_secret".to_owned()),
            purpose: ToolPurpose::Show,
        },
    ] {
        let mut scrubbed = bad_display;
        scrub_tool_call_display(
            "show_diff",
            &json!("[redacted]"),
            &mut scrubbed,
            None,
            &empty_held,
            true,
        );
        assert_eq!(
            scrubbed.action,
            ToolAction::Other {
                name: "tool".to_owned()
            }
        );
        assert_eq!(scrubbed.target, "{}");
        assert_eq!(scrubbed.title, None);
        assert_eq!(scrubbed.purpose, ToolPurpose::Show);
    }

    let show_page = ToolDisplay {
        action: ToolAction::Show,
        verbs: None,
        target: "proj_01234567890123456789".to_owned(),
        output: OutputKind::None,
        view: None,
        title: None,
        purpose: ToolPurpose::Show,
    };
    let mut kept_show = show_page.clone();
    scrub_tool_call_display(
        "show_page",
        &json!({"id": "proj_01234567890123456789"}),
        &mut kept_show,
        None,
        &empty_held,
        false,
    );
    assert_eq!(kept_show, show_page);

    let mut unredacted_email_dots = ToolDisplay {
        target: "{a: dir/alice@exam...}".to_owned(),
        ..canonical("plug")
    };
    scrub_tool_call_display(
        "plug",
        &json!({"a": "dir/alice@example.invalid"}),
        &mut unredacted_email_dots,
        None,
        &empty_held,
        true,
    );
    assert_eq!(unredacted_email_dots.action, canonical("plug").action);
    assert_eq!(unredacted_email_dots.target, "{}");

    let mut unredacted_key_dots = ToolDisplay {
        target: "{a: sk-ant-CANARY012345...}".to_owned(),
        ..canonical("plug")
    };
    scrub_tool_call_display(
        "plug",
        &json!({"a": unsafe_key}),
        &mut unredacted_key_dots,
        None,
        &empty_held,
        true,
    );
    assert_eq!(unredacted_key_dots.action, canonical("plug").action);
    assert_eq!(unredacted_key_dots.target, "{}");

    let mut unredacted_key_name = ToolDisplay {
        target: "{a}".to_owned(),
        ..canonical("sk-ant-CANARY012345...")
    };
    scrub_tool_call_display(
        unsafe_key,
        &json!({"a": 1}),
        &mut unredacted_key_name,
        None,
        &empty_held,
        true,
    );
    assert_eq!(unredacted_key_name.action, canonical("tool").action);
    assert_eq!(unredacted_key_name.target, "{}");

    let late_token = "CANARY_LATE_TOKEN_0123456789";
    let late_held = crate::session::Redactions::new([late_token.to_owned()]);
    let mut unredacted_held_dots = ToolDisplay {
        target: "{a: CANARY_LATE_TOKEN...}".to_owned(),
        ..canonical("plug")
    };
    scrub_tool_call_display(
        "plug",
        &json!({"a": late_token}),
        &mut unredacted_held_dots,
        None,
        &late_held,
        true,
    );
    assert_eq!(unredacted_held_dots.action, canonical("plug").action);
    assert_eq!(unredacted_held_dots.target, "{}");

    let mut unredacted_held_name = ToolDisplay {
        target: "{a}".to_owned(),
        ..canonical("CANARY_LATE_TOKEN...")
    };
    scrub_tool_call_display(
        late_token,
        &json!({"a": 1}),
        &mut unredacted_held_name,
        None,
        &late_held,
        true,
    );
    assert_eq!(unredacted_held_name.action, canonical("tool").action);
    assert_eq!(unredacted_held_name.target, "{}");

    let mut preserved_safe_canonical = ToolDisplay {
        target: "{a}".to_owned(),
        ..canonical("plug")
    };
    scrub_tool_call_display(
        "plug",
        &json!({"a": unsafe_key}),
        &mut preserved_safe_canonical,
        None,
        &empty_held,
        true,
    );
    assert_eq!(preserved_safe_canonical.action, canonical("plug").action);
    assert_eq!(preserved_safe_canonical.target, "{a}");

    let benign_non_canonical = ToolDisplay {
        target: "{custom benign...}".to_owned(),
        ..canonical("plug_alt")
    };
    let mut kept_benign = benign_non_canonical.clone();
    scrub_tool_call_display(
        "plug",
        &json!({"a": "safe"}),
        &mut kept_benign,
        None,
        &empty_held,
        true,
    );
    assert_eq!(kept_benign, benign_non_canonical);

    let gen_before = crate::session::held_generation();
    let held_late = "CANARYGenAdvancedLateSecret";
    crate::session::register_secrets([held_late.to_owned()]);
    let gen_now = crate::session::held_generation();
    let held_now = crate::session::read_held();
    let mut canonical_other = canonical("plug");
    scrub_refused_tool_call(
        "plug",
        &mut json!({"k": held_late}),
        &mut canonical_other,
        None,
        &held_now,
        false,
        gen_before,
        gen_now,
    );
    assert_eq!(canonical_other, canonical("plug"));

    let mut invented_other = canonical("other_plug");
    scrub_refused_tool_call(
        "plug",
        &mut json!({"k": held_late}),
        &mut invented_other,
        None,
        &held_now,
        false,
        gen_before,
        gen_now,
    );
    assert_eq!(
        invented_other.action,
        ToolAction::Other {
            name: "tool".to_owned()
        }
    );
    assert_eq!(invented_other.target, "{}");
}

#[test]
fn an_unregistered_tools_refused_call_shows_only_its_keys() {
    let toolbox = Toolbox::new([Dummy::tool("known", json!({"type": "object"}))]);
    let marker = "marker-77ab09";
    let display = futures::executor::block_on(
        toolbox.rejected_display("nope", &json!({"input": marker, "id": 1})),
    );
    assert_eq!(display.target, "{id, input}");
    assert_eq!(
        display.action,
        ToolAction::Other {
            name: "nope".into()
        }
    );
    assert!(!serde_json::to_string(&display).unwrap().contains(marker));
    let show_box = testkit::bundled_exact(&["show_diff"]);
    let long_path = format!("{}sk-ant-CANARY0123456789012345", "dir/".repeat(18));
    let shown = futures::executor::block_on(
        show_box.rejected_display("show_diff", &json!({"files": [{"path": long_path}]})),
    );
    assert_eq!(shown.target, "diff");
    assert!(shown.title.is_none());
    let long_title = format!("{}user@example.invalid", "Title ".repeat(36));
    let shown = futures::executor::block_on(
        show_box.rejected_display("show_diff", &json!({"files": [], "title": long_title})),
    );
    assert_eq!(shown.target, "diff");
    assert!(shown.title.is_none());
}

fn scrubbed_call(
    tool: &str,
    mut input: Value,
    held: &crate::session::Redactions,
    refused: bool,
) -> Value {
    let home = Some(std::path::Path::new("/home/alice"));
    let mut display = fallback_display_with_held(tool, &input, home, held);
    scrub_tool_call(tool, &mut input, &mut display, home, held, refused);
    let once = (input.clone(), display.clone());
    scrub_tool_call(tool, &mut input, &mut display, home, held, refused);
    assert_eq!((input.clone(), display), once);
    input
}

#[test]
fn a_refused_calls_unsafe_input_keeps_its_keys_and_never_a_value() {
    let canary = "CANARYRefusedInputValue";
    let held = crate::session::Redactions::new([canary.to_owned()]);
    let none = crate::session::Redactions::default();
    let email = ["alice", "example.invalid"].join("@");
    let refuse =
        |input: Value, held: &crate::session::Redactions| scrubbed_call("plug", input, held, true);
    assert_eq!(
        refuse(json!({"path": email, "n": 3}), &none),
        json!({"path": null, "n": null})
    );
    assert_eq!(
        refuse(json!({"outer": {"inner": [email]}, "n": 3}), &none),
        json!({"outer": null, "n": null})
    );
    assert_eq!(
        refuse(json!({"home": "/home/alice/notes"}), &none),
        json!({"home": null})
    );
    let registered = refuse(json!({"a": format!("x {canary}")}), &held);
    assert_eq!(registered, json!({"a": null}));
    assert!(!registered.to_string().contains(canary));
    assert_eq!(refuse(json!({email.clone(): 1}), &none), Value::Null);
    assert_eq!(refuse(json!(email), &none), Value::Null);
    assert_eq!(refuse(json!([email]), &none), Value::Null);
    let safe = json!({"path": "src/lib.rs", "n": [1, {"k": "v"}]});
    assert_eq!(refuse(safe.clone(), &none), safe);
    assert_eq!(refuse(safe.clone(), &held), safe);
}

#[test]
fn an_accepted_calls_input_hides_checklist_fields_and_registered_secrets_only() {
    let canary = "CANARYAcceptedInputValue";
    let held = crate::session::Redactions::new([canary.to_owned()]);
    let none = crate::session::Redactions::default();
    let email = ["alice", "example.invalid"].join("@");
    let accept = |tool: &str, input: Value, held: &crate::session::Redactions| {
        scrubbed_call(tool, input, held, false)
    };
    let step = |text: &str| json!({"text": text, "status": "pending"});
    assert_eq!(
        accept(
            "write_todos",
            json!({"todos": [step(&format!("mail {email}")), step("Read the file")]}),
            &none
        ),
        json!({"todos": [step("[redacted]"), step("Read the file")]})
    );
    assert_eq!(
        accept(
            "spawn_agent",
            json!({"name": "w", "task": format!("mail {email}"), "todos": ["see /home/alice/x", "Read the file"]}),
            &none
        ),
        json!({"name": "w", "task": format!("mail {email}"), "todos": ["[redacted]", "Read the file"]})
    );
    assert_eq!(
        accept(
            "plug",
            json!({"name": "write_todos", "input": {"todos": [step(&format!("mail {email}"))]}}),
            &none
        ),
        json!({"name": "write_todos", "input": {"todos": [step("[redacted]")]}})
    );
    assert_eq!(
        accept("plug", json!({"todos": format!("mail {email}")}), &none),
        json!({"todos": "[redacted]"})
    );
    assert_eq!(
        accept(
            "plug",
            json!({"todos": [step(&format!("key {canary}"))]}),
            &held
        ),
        json!({"todos": [step("[redacted]")]})
    );
    let command = json!({
        "command": format!("echo {email} /home/alice/x sk-ant-CANARY0123456789012345"),
        "description": "Print"
    });
    assert_eq!(accept("bash", command.clone(), &none), command);
    assert_eq!(accept("bash", command.clone(), &held), command);
    assert_eq!(
        accept(
            "bash",
            json!({"command": format!("echo hi\n{canary}"), "description": "Print"}),
            &held
        ),
        json!({"command": "echo hi\n[redacted]", "description": "Print"})
    );
    let split = format!("echo {}\u{200B}{}", &canary[..6], &canary[6..]);
    assert_eq!(
        accept(
            "bash",
            json!({"command": split, "description": "Print"}),
            &held
        ),
        json!({"command": "[redacted]", "description": "Print"})
    );
    assert_eq!(
        accept("bash", json!({"b": 2, format!("key {canary}"): 1}), &held),
        json!({"b": 2, "key [redacted]": 1})
    );
}

#[test]
fn canonical_bytes_of_a_value_input_are_pinned() {
    let input: Value =
        serde_json::from_str(r#"{"z": 1, "a": {"y": [3, {"c": true, "b": null}], "x": "é"}}"#)
            .unwrap();
    assert_eq!(
        serde_json::to_string(&input).unwrap(),
        r#"{"a":{"x":"é","y":[3,{"b":null,"c":true}]},"z":1}"#
    );
}

#[test]
fn the_shipped_toolbox_is_empty_in_every_mode() {
    assert!(testkit::bundled(&[]).is_empty());
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tools")
}

#[test]
fn view_file_alone_sends_its_golden_no_bash_spec() {
    let bless = std::env::var_os("WISP_BLESS").is_some();
    let toolbox = testkit::bundled(&["view_file"]);
    assert_eq!(toolbox.names(), ["view_file"]);
    let path = fixture_dir().join("view_file_without_bash.json");
    let bytes = format!("{}\n", serde_json::to_string(&toolbox.specs()[0]).unwrap());
    if bless {
        std::fs::write(&path, &bytes).unwrap();
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), bytes);
    assert!(!bytes.contains("bash"));
    let again = testkit::bundled(&["view_file"]);
    assert_eq!(
        format!("{}\n", serde_json::to_string(&again.specs()[0]).unwrap()),
        bytes
    );
}

#[test]
fn bash_and_process_without_secret_match_todays_specs() {
    for toolbox in [
        testkit::bundled(&["bash"]),
        testkit::start(
            vec![testkit::shell_source(wisp_plugin_shell::Timing::default())],
            &["bash"],
        ),
    ] {
        assert_eq!(toolbox.names(), ["bash", "process"]);
        for spec in toolbox.specs() {
            let path = fixture_dir().join(format!("{}_without_secret.json", spec.name));
            let bytes = format!("{}\n", serde_json::to_string(spec).unwrap());
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                bytes,
                "{}",
                path.display()
            );
        }
    }
}

#[test]
fn every_builtin_tool_matches_its_golden_spec() {
    let bless = std::env::var_os("WISP_BLESS").is_some();
    let toolbox = testkit::bundled(
        &[
            testkit::ALL,
            &[
                "show_page",
                "show_code",
                "manage",
                "remember",
                "rename_chat",
            ],
        ]
        .concat(),
    );
    assert!(toolbox.names().iter().any(|n| n == "view_file"));
    let held = toolbox.held_specs();
    let held = held.iter().filter(|spec| toolbox.is_enabled(&spec.name));
    assert_eq!(
        held.clone()
            .map(|spec| spec.name.as_str())
            .collect::<Vec<_>>(),
        ["manage", "rename_chat"]
    );
    for spec in toolbox.specs().iter().chain(held) {
        let path = fixture_dir().join(format!("{}.json", spec.name));
        let bytes = format!("{}\n", serde_json::to_string(spec).unwrap());
        if bless {
            std::fs::create_dir_all(fixture_dir()).unwrap();
            std::fs::write(&path, &bytes).unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            bytes,
            "{}",
            path.display()
        );
    }
}

#[test]
fn write_todos_stands_alone_adds_no_sentence_and_matches_its_golden_spec() {
    let bless = std::env::var_os("WISP_BLESS").is_some();
    let toolbox = testkit::bundled(&["write_todos"]);
    assert_eq!(toolbox.names(), ["write_todos"]);
    assert!(toolbox.system_lines().is_empty());
    let path = fixture_dir().join("write_todos.json");
    let bytes = format!("{}\n", serde_json::to_string(&toolbox.specs()[0]).unwrap());
    if bless {
        std::fs::write(&path, &bytes).unwrap();
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), bytes);
    let again = testkit::bundled(&["write_todos", "show_diff"]);
    let spec = again
        .specs()
        .iter()
        .find(|s| s.name == "write_todos")
        .map(|s| format!("{}\n", serde_json::to_string(s).unwrap()));
    assert_eq!(spec.as_deref(), Some(bytes.as_str()));
}

#[test]
fn spawn_agent_brings_its_followers_but_never_report_and_each_matches_its_golden_spec() {
    let bless = std::env::var_os("WISP_BLESS").is_some();
    let names = [
        "list_agents",
        "rename_agent",
        "report",
        "send_message",
        "spawn_agent",
        "stop_agent",
    ];
    let (registry, _) = crate::plugin::Registry::new(
        wisp_plugins::bundled(),
        &crate::plugin::PluginConfig::default(),
    );
    let defaults = registry.on_tools(&registry.default_on());
    for name in [
        "list_agents",
        "rename_agent",
        "send_message",
        "spawn_agent",
        "stop_agent",
        "write_todos",
    ] {
        assert!(defaults.contains(name), "{name}");
    }
    assert!(!defaults.contains("report"));
    assert_eq!(
        testkit::bundled(&["spawn_agent"]).names(),
        [
            "list_agents",
            "rename_agent",
            "send_message",
            "spawn_agent",
            "stop_agent"
        ]
    );
    assert!(testkit::bundled(&["report", "list_agents"]).is_empty());
    let toolbox = testkit::bundled_exact(&names);
    assert_eq!(toolbox.names(), names);
    assert_eq!(
        toolbox.system_lines(),
        [
            wisp_tool_text::agents::MESSAGES_SENTENCE,
            wisp_tool_text::agents::REPORT_SENTENCE,
            wisp_tool_text::agents::SPAWN_LINE,
        ]
    );
    for spec in toolbox.specs() {
        let path = fixture_dir().join(format!("{}.json", spec.name));
        let bytes = format!("{}\n", serde_json::to_string(spec).unwrap());
        if bless {
            std::fs::write(&path, &bytes).unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            bytes,
            "{}",
            spec.name
        );
        let alone = testkit::bundled_exact(&[spec.name.as_str(), "show_diff", "write_todos"]);
        let again = alone.specs().iter().find(|s| s.name == spec.name).unwrap();
        assert_eq!(serde_json::to_string(again).unwrap(), bytes.trim_end());
    }
    assert!(
        testkit::bundled(&["spawn_agent"])
            .get("spawn_agent")
            .is_some_and(|tool| !tool.meta().needs_workspace && !tool.meta().asks_user)
    );
}

#[test]
fn every_test_tool_has_a_sample_its_prepare_accepts() {
    let toolbox = test_toolbox();
    assert!(!toolbox.is_empty());
    for sample in samples() {
        let name = &sample.tool.spec().name;
        assert!(toolbox.get(name).is_some(), "{name}");
        assert!(
            futures::executor::block_on(toolbox.prepare(name, &sample.input)).is_ok(),
            "{name}"
        );
        let spec = serde_json::to_string(sample.tool.spec()).unwrap();
        assert!(!spec.chars().any(|c| c.is_ascii_digit()), "{spec}");
        assert!(!spec.contains('/'), "{spec}");
    }
}

#[test]
fn show_diff_is_the_only_built_in_that_shows_content() {
    let toolbox = testkit::bundled(testkit::ALL);
    let declared: Vec<(String, ToolPurpose)> = toolbox
        .names()
        .into_iter()
        .map(|name| {
            let purpose = toolbox.get(&name).unwrap().meta().purpose;
            (name, purpose)
        })
        .collect();
    let mut expected = vec![
        ("ask_user".to_owned(), ToolPurpose::Run),
        ("list_accounts".to_owned(), ToolPurpose::Run),
        ("list_settings".to_owned(), ToolPurpose::Run),
        ("show_diff".to_owned(), ToolPurpose::Show),
    ];
    if cfg!(unix) {
        expected.extend([
            ("ask_secret".to_owned(), ToolPurpose::Run),
            ("bash".to_owned(), ToolPurpose::Run),
            ("process".to_owned(), ToolPurpose::Run),
            ("view_file".to_owned(), ToolPurpose::Run),
        ]);
    }
    expected.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(declared, expected);
}

#[test]
fn a_tool_that_is_not_registered_runs() {
    let toolbox = testkit::bundled(&["show_diff"]);
    assert_eq!(toolbox.purpose("show_diff"), ToolPurpose::Show);
    assert_eq!(toolbox.purpose("nowhere"), ToolPurpose::Run);
    assert_eq!(toolbox.purpose("web_search"), ToolPurpose::Run);
    assert_eq!(Toolbox::default().purpose("show_diff"), ToolPurpose::Run);
}

#[test]
fn a_refused_show_diff_call_keeps_its_own_display_and_an_unknown_tool_falls_back() {
    let toolbox = testkit::bundled(&["show_diff"]);
    let input = json!({"files": [{"path": "a.rs"}]});
    assert!(futures::executor::block_on(toolbox.prepare("show_diff", &input)).is_err());
    let own = futures::executor::block_on(toolbox.rejected_display("show_diff", &input));
    assert_ne!(own, fallback_display("show_diff", &input));
    assert_eq!(
        futures::executor::block_on(toolbox.rejected_display("nowhere", &input)),
        fallback_display("nowhere", &input)
    );
}

#[cfg(unix)]
#[test]
fn view_file_names_bash_only_while_the_shell_is_on() {
    let alone = testkit::bundled(&["view_file"]);
    let with_shell = testkit::bundled(&["bash", "view_file"]);
    assert_eq!(alone.provider_of(SHELL), None);
    assert_eq!(with_shell.provider_of(SHELL), Some("bash"));
    assert_eq!(alone.capabilities(), Vec::<String>::new());
    assert_eq!(with_shell.capabilities(), ["shell"]);
    let described =
        |toolbox: &Toolbox| toolbox.get("view_file").unwrap().spec().description.clone();
    assert!(!described(&alone).contains("bash"));
    assert!(described(&with_shell).contains("bash"));
}

#[test]
fn json_keys_keep_sorted_order_in_the_kernels_feature_graph() {
    let value = json!({"b": 1, "a": 2});
    assert_eq!(value.to_string(), r#"{"a":2,"b":1}"#);
}

fn source_files(dir: &std::path::Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            source_files(&path, files);
        } else if path.extension().is_some_and(|e| e == "rs") {
            files.push(path);
        }
    }
}

fn test_only(relative: &str) -> bool {
    let name = relative.rsplit('/').next().unwrap_or_default();
    relative.split('/').any(|part| {
        part == "tests" || part == "tool_tests" || part == "shell_tests" || part.ends_with("_tests")
    }) || matches!(
        name,
        "tests.rs"
            | "testing.rs"
            | "testkit.rs"
            | "snap.rs"
            | "shell_tests.rs"
            | "tool_tests.rs"
            | "test_guard.rs"
    )
}

#[test]
fn the_kernel_names_no_bundled_tool_outside_its_allow_list() {
    let mut names: Vec<String> = wisp_plugins::bundled()
        .into_iter()
        .chain([wisp_plugin_testkit::bundled()])
        .flat_map(|bundled| {
            wisp_plugin_api::manifest::Manifest::parse(
                bundled.manifest,
                wisp_plugin_api::manifest::Origin::Bundled,
            )
            .unwrap()
            .tools
            .into_iter()
            .map(|tool| tool.name)
        })
        .collect();
    names.push("web_search".to_owned());
    let allowed = [
        ("web_search", "search.rs"),
        ("web_search", "settings.rs"),
        ("web_search", "config.rs"),
        ("web_search", "models.rs"),
        ("confirm", "tool.rs"),
        ("echo", "provider/openai/realtime.rs"),
        ("search", "provider/foreign.rs"),
        ("search", "provider/gemini/request.rs"),
    ];
    let allowed_lines = [
        (
            "agents.rs",
            "\"report\" => return Err(PresetProblem::ReportRemoved)".to_owned(),
        ),
        (
            "plugin/host/manage.rs",
            "pub(crate) const SEARCH_PLUGIN: &str = \"search\";".to_owned(),
        ),
    ];
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    source_files(&root, &mut files);
    let mut found = Vec::new();
    for file in files {
        let relative = file
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if test_only(&relative) {
            continue;
        }
        let text = allowed_lines
            .iter()
            .filter(|(path, _)| *path == relative)
            .fold(std::fs::read_to_string(&file).unwrap(), |text, (_, key)| {
                assert_eq!(text.matches(key.as_str()).count(), 1, "{relative}: {key}");
                text.replace(key.as_str(), "")
            });
        for name in &names {
            let literal = format!("\"{name}\"");
            if text.contains(&literal) && !allowed.contains(&(name.as_str(), relative.as_str())) {
                found.push(format!("{relative}: {literal}"));
            }
        }
    }
    assert!(found.is_empty(), "{found:#?}");
    assert!(
        !cfg!(unix) || names.iter().any(|n| n == "bash") && names.iter().any(|n| n == "process")
    );
}
