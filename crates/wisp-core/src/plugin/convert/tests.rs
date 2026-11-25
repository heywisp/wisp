use super::*;

#[test]
fn every_action_round_trips() {
    let actions = [
        ToolAction::Run,
        ToolAction::Read,
        ToolAction::Write,
        ToolAction::Edit,
        ToolAction::Search,
        ToolAction::Fetch,
        ToolAction::Input,
        ToolAction::Wait,
        ToolAction::Attach,
        ToolAction::Show,
        ToolAction::Signal {
            kind: SignalKind::Interrupt,
        },
        ToolAction::Signal {
            kind: SignalKind::Kill,
        },
        ToolAction::Other {
            name: "probe".into(),
        },
    ];
    for original in actions {
        assert_eq!(action(action_to_api(&original)), original);
    }
}

#[test]
fn a_display_round_trips_and_takes_the_run_purpose() {
    let original = ToolDisplay {
        action: ToolAction::Show,
        verbs: None,
        target: "src/x.rs".into(),
        output: OutputKind::Code {
            lang: "rust".into(),
        },
        view: Some(ToolView::Diff {
            title: Some("rename".into()),
            files: vec![wisp_protocol::DiffView {
                path: Some("src/x.rs".into()),
                old_path: None,
                language: Some("rust".into()),
                hunks: "@@ -1 +1 @@\n-a\n+b\n".into(),
                added: 1,
                removed: 1,
            }],
        }),
        title: Some("rename".into()),
        purpose: ToolPurpose::Show,
    };
    let code = ToolDisplay {
        view: Some(ToolView::Code {
            language: Some("rust".into()),
            code: "fn main() {}\n".into(),
        }),
        ..original.clone()
    };
    for original in [original, code] {
        let back = display(display_to_api(&original)).unwrap();
        assert_eq!(
            back,
            ToolDisplay {
                purpose: ToolPurpose::Run,
                ..original
            }
        );
    }
}

#[test]
fn a_logged_process_display_keeps_its_key_and_read_names() {
    let from_plugin = api::Display {
        action: api::Action::Input,
        verbs: None,
        target: "7".into(),
        output: api::OutputKind::Text,
        view: Some(api::View::Process(api::ProcessView {
            id: Some(7),
            text: Some("yes".into()),
            keys: vec![
                api::Key::Named(api::NamedKey::PageDown),
                api::Key::Function(5),
                api::Key::Ctrl('c'),
                api::Key::Eof,
            ],
            signal: None,
            read: Some(api::ReadKind::Screen),
            pattern: None,
        })),
        title: None,
    };
    let shown = display(from_plugin.clone()).unwrap();
    assert_eq!(
        serde_json::to_string(&shown.view).unwrap(),
        r#"{"type":"process","id":7,"text":"yes","keys":["PageDown","F5","C-c","EOF"],"read":"screen"}"#
    );
    assert_eq!(display_to_api(&shown), from_plugin);
    let mut stale = shown;
    if let Some(ToolView::Process { keys, read, .. }) = &mut stale.view {
        keys.insert(0, "Hyper".into());
        *read = Some("sideways".into());
    }
    let Some(api::View::Process(back)) = display_to_api(&stale).view else {
        panic!("not a process view");
    };
    assert_eq!(back.keys.len(), 4);
    assert_eq!(back.read, None);
}

#[test]
fn a_plugins_verbs_reach_the_display_and_go_back_unchanged() {
    let mut from_plugin = wisp_plugin_testkit::fallback_display("brew", "{}");
    from_plugin.verbs = Some(wisp_plugin_testkit::verbs());
    let shown = display(from_plugin.clone()).unwrap();
    assert_eq!(
        shown.verbs,
        Some(wisp_protocol::Verbs {
            doing: "Brewing".into(),
            done: "Brewed".into(),
            failed: "couldn't brew".into(),
        })
    );
    assert_eq!(display_to_api(&shown), from_plugin);
}

#[test]
fn an_unknown_action_and_output_go_to_a_plugin_as_other_and_text() {
    assert_eq!(
        action_to_api(&ToolAction::Unknown),
        api::Action::Other(String::new())
    );
    assert_eq!(output_to_api(&OutputKind::Unknown), api::OutputKind::Text);
}

#[test]
fn a_run_maps_its_facts_cwd_and_attachments() {
    let run = api::ToolRun {
        outcome: api::Outcome::Completed(api::Completed {
            is_error: true,
            exit: Some(api::ExitStatus::Code(3)),
        }),
        facts: vec![api::Fact::Exit(3), api::Fact::Media("image/png".into())],
        cwd: Some("/work/sub".into()),
        trailer: Some("trailer".into()),
        result: None,
        attachments: vec![api::Attachment {
            media_type: "image/png".into(),
            bytes: vec![1, 2],
            slice: None,
        }],
        typed: None,
    };
    let converted = super::run(run).unwrap();
    assert_eq!(
        converted.outcome,
        ToolOutcome::Completed {
            is_error: true,
            exit: Some(ExitStatus::Code { code: 3 })
        }
    );
    assert_eq!(
        converted.facts,
        [
            Fact::Exit { code: 3 },
            Fact::Media {
                media_type: "image/png".parse().unwrap()
            }
        ]
    );
    assert_eq!(
        converted.state,
        Some(WorkspaceChange::Cwd {
            path: "/work/sub".into()
        })
    );
    assert_eq!(converted.attachments[0].bytes, [1, 2]);
}

#[test]
fn file_slice_conversion_matches_parser_bounds_and_display_and_kind_names() {
    use wisp_tool_text::files::{parse_pages, parse_seconds, parse_sheet, read_only};

    for (parsed, expected, line) in [
        (parse_pages("5"), FileSlice::pages(5, 5), "read only page 5"),
        (
            parse_pages("14-16"),
            FileSlice::pages(14, 16),
            "read only pages 14-16",
        ),
        (
            parse_seconds("30-90"),
            FileSlice::seconds(30, 90),
            "read only seconds 30-90",
        ),
        (parse_sheet("2"), FileSlice::sheet(2), "read only sheet 2"),
        (
            parse_pages("4294967295"),
            FileSlice::pages(u32::MAX, u32::MAX),
            "read only page 4294967295",
        ),
        (
            parse_seconds("0-4294967295"),
            FileSlice::seconds(0, u32::MAX),
            "read only seconds 0-4294967295",
        ),
        (
            parse_sheet("4294967295"),
            FileSlice::sheet(u32::MAX),
            "read only sheet 4294967295",
        ),
    ] {
        let parsed = parsed.unwrap();
        let proto = file_slice(parsed).unwrap();
        assert_eq!(proto, expected.unwrap());
        assert_eq!(read_only(&parsed), proto.read_only("-"));
        assert_eq!(proto.read_only("-"), line);
        assert_eq!(proto.kind_name(), parsed.kind().name());
        assert_eq!(slice_kind(&proto), parsed.kind());
    }
    for (parsed, invalid) in [
        (
            parse_pages("0"),
            api::FileSlice::Pages(api::PageSlice { start: 0, end: 0 }),
        ),
        (
            parse_pages("1-0"),
            api::FileSlice::Pages(api::PageSlice { start: 1, end: 0 }),
        ),
        (
            parse_seconds("1-1"),
            api::FileSlice::Seconds(api::SecondSlice { start: 1, end: 1 }),
        ),
        (
            parse_seconds("1-0"),
            api::FileSlice::Seconds(api::SecondSlice { start: 1, end: 0 }),
        ),
        (parse_sheet("0"), api::FileSlice::Sheet(0)),
    ] {
        assert!(parsed.is_err(), "{invalid:?}");
        let error = file_slice(invalid).unwrap_err();
        let mut run = api::ToolRun::completed();
        run.attachments.push(api::Attachment {
            media_type: "application/pdf".into(),
            bytes: vec![1],
            slice: Some(invalid),
        });
        assert_eq!(super::run(run).unwrap_err(), error);
    }
    for parsed in [
        parse_pages("4294967296"),
        parse_seconds("0-4294967296"),
        parse_sheet("4294967296"),
        parse_pages("14-"),
        parse_seconds("0-"),
    ] {
        assert!(parsed.is_err());
    }
    assert_eq!(
        api::SliceKind::ALL
            .iter()
            .map(api::SliceKind::name)
            .collect::<Vec<_>>(),
        ["pages", "seconds", "sheet"]
    );
}

#[test]
fn a_run_with_a_bad_media_type_is_refused() {
    let mut run = super::run(api::ToolRun {
        facts: vec![api::Fact::Media("not a type".into())],
        ..api::ToolRun::new(api::Outcome::Cancelled)
    });
    assert_eq!(
        run.as_ref().err().map(String::as_str),
        Some("`not a type` is not a media type")
    );
    run = super::run(api::ToolRun::new(api::Outcome::Cancelled));
    assert_eq!(run.unwrap().outcome, ToolOutcome::Cancelled);
}

#[test]
fn meta_keeps_purpose_writing_and_limits() {
    let meta = super::meta(ApiMeta {
        effect: Effect::NonIdempotent,
        tags: Tags {
            reversible: true,
            network: true,
            ..Tags::default()
        },
        parallel_safe: true,
        timeouts: wisp_plugin_api::tools::Timeouts {
            default_ms: 1_500,
            max_ms: Some(9_000),
        },
        saves_output: true,
        needs_workspace: false,
        ends_reply: true,
        asks_user: true,
        reports_jobs: false,
        writing: Some("a nap".into()),
        purpose: Purpose::Show,
    });
    assert_eq!(meta.effect, EffectClass::NonIdempotent);
    assert_eq!(meta.tags, BTreeSet::from([Tag::Reversible, Tag::Network]));
    assert_eq!(
        meta.timeout,
        Timeouts {
            default: Duration::from_millis(1_500),
            max: Some(Duration::from_millis(9_000))
        }
    );
    assert_eq!(meta.writing.as_deref(), Some("a nap"));
    assert_eq!(meta.purpose, ToolPurpose::Show);
    assert!(meta.ends_reply && meta.asks_user && meta.saves_output && meta.parallel_safe);
}

#[test]
fn every_node_state_round_trips_and_an_account_queued_agent_answers_working() {
    use wisp_plugin_api::slots::NodeState as Api;
    use wisp_protocol::slot::NodeState;
    for api in [
        Api::Working,
        Api::Waiting,
        Api::Done,
        Api::Failed,
        Api::Stopped,
    ] {
        assert_eq!(api_state(slot_state(api)), api);
    }
    assert_eq!(api_state(NodeState::Unknown), Api::Waiting);
    let entry = |state: wisp_protocol::AgentState| -> wisp_protocol::AgentEntry {
        let mut json = serde_json::json!({
            "path": "/a/b", "name": "b", "model": "m", "effort": "low", "depth": 2,
            "activity": {"type": "idle"}, "spent_usd": 0.0,
        });
        json["state"] = serde_json::to_value(state).unwrap();
        serde_json::from_value(json).unwrap()
    };
    let queued = |reason| wisp_protocol::AgentState::Queued { reason };
    for (state, want) in [
        (queued(wisp_protocol::QueueReason::Endpoint), Api::Working),
        (
            queued(wisp_protocol::QueueReason::Concurrency),
            Api::Waiting,
        ),
        (wisp_protocol::AgentState::Ended, Api::Stopped),
    ] {
        let shown = shown_state(&entry(state));
        assert_eq!(
            (shown.path.as_str(), shown.state),
            ("/a/b", want),
            "{state:?}"
        );
    }
}
