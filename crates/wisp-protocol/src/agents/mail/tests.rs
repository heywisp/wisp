use super::*;

fn path(p: &str) -> AgentPath {
    AgentPath::new(p)
}

#[test]
fn ancestors_are_strict_prefixes_ending_at_a_slash() {
    assert!(AgentPath::root().is_ancestor_of(&path("/review")));
    assert!(AgentPath::root().is_ancestor_of(&path("/review/planner")));
    assert!(path("/review").is_ancestor_of(&path("/review/planner")));
    assert!(!path("/review").is_ancestor_of(&path("/review")));
    assert!(!path("/review").is_ancestor_of(&path("/reviewer/planner")));
    assert!(!path("/review/planner").is_ancestor_of(&path("/review")));
    assert!(!path("/docs").is_ancestor_of(&path("/review/planner")));
    assert!(!AgentPath::root().is_ancestor_of(&AgentPath::root()));
}

#[test]
fn paths_sort_parents_first_then_by_name() {
    let mut paths = ["/b", "/a-b", "/a/c", "/", "/a/b", "/a"].map(path);
    paths.sort_by(AgentPath::tree_cmp);
    let sorted = paths.map(|p| p.0);
    assert_eq!(sorted, ["/", "/a", "/a/b", "/a/c", "/a-b", "/b"]);
}

#[test]
fn moved_and_followed_rename_a_subtree_in_order() {
    let (coder, fixer) = (path("/task/coder"), path("/task/fixer"));
    assert_eq!(coder.moved(&coder, &fixer), Some(fixer.clone()));
    assert_eq!(
        path("/task/coder/scout").moved(&coder, &fixer),
        Some(path("/task/fixer/scout"))
    );
    assert_eq!(path("/task/coder2").moved(&coder, &fixer), None);
    assert_eq!(path("/task").moved(&coder, &fixer), None);
    let chain = [
        (coder.clone(), path("/task/fixre")),
        (path("/task/fixre"), fixer.clone()),
    ];
    assert_eq!(
        path("/task/coder/scout").followed(&chain),
        path("/task/fixer/scout")
    );
    assert_eq!(path("/task/fixre").followed(&chain), fixer);
    assert_eq!(path("/docs").followed(&chain), path("/docs"));
    let back = [
        (coder.clone(), fixer.clone()),
        (fixer.clone(), coder.clone()),
    ];
    assert_eq!(coder.followed(&back), coder);
    assert_eq!(
        path("/task/fixer/scout").followed(&back),
        path("/task/coder/scout")
    );
}

#[test]
fn new_kinds_and_reasons_from_a_later_writer_read_as_unknown() {
    let kind: MailKind = serde_json::from_str("\"later\"").unwrap();
    assert_eq!(kind, MailKind::Unknown);
    let reason: HoldReason = serde_json::from_str("\"later\"").unwrap();
    assert_eq!(reason, HoldReason::Unknown);
    let source: WakeSource = serde_json::from_str(r#"{"type":"later","x":1}"#).unwrap();
    assert_eq!(source, WakeSource::Unknown);
    let action: AgentAction = serde_json::from_str("\"later\"").unwrap();
    assert_eq!(action, AgentAction::Unknown);
    let rename = serde_json::to_string(&AgentAction::Rename).unwrap();
    assert_eq!(rename, "\"rename\"");
}

#[test]
fn a_waking_sender_omits_wake_and_a_quiet_one_keeps_it() {
    let from = MailFrom {
        agent: path("/review"),
        kind: MailKind::Message,
        mail: MailId("m1".into()),
        wake: true,
        plugin: None,
    };
    let text = serde_json::to_string(&from).unwrap();
    assert_eq!(text, r#"{"agent":"/review","kind":"message","mail":"m1"}"#);
    assert_eq!(serde_json::from_str::<MailFrom>(&text).unwrap(), from);
    let labelled = MailFrom {
        plugin: Some("mcp".into()),
        ..from.clone()
    };
    assert_eq!(
        serde_json::to_value(&labelled).unwrap(),
        serde_json::json!({"agent":"/review","kind":"message","mail":"m1","plugin":"mcp"})
    );
    let quiet = MailFrom {
        wake: false,
        ..from
    };
    let text = serde_json::to_string(&quiet).unwrap();
    assert_eq!(
        text,
        r#"{"agent":"/review","kind":"message","mail":"m1","wake":false}"#
    );
    assert_eq!(serde_json::from_str::<MailFrom>(&text).unwrap(), quiet);
}

#[test]
fn paths_know_their_parent_name_and_depth() {
    let planner = path("/review/planner");
    assert_eq!(planner.parent(), Some(path("/review")));
    assert_eq!(path("/review").parent(), Some(AgentPath::root()));
    assert_eq!(AgentPath::root().parent(), None);
    assert_eq!(planner.name(), "planner");
    assert_eq!(AgentPath::root().join("review"), path("/review"));
    assert_eq!(path("/review").join("planner"), planner);
    assert_eq!(AgentPath::root().depth(), 0);
    assert_eq!(path("/review").depth(), 1);
    assert_eq!(planner.depth(), 2);
}

#[test]
fn mail_text_reads_back_its_body_and_a_reports_parts() {
    let from = path("/review");
    let mail = render_mail(&from, MailKind::Message, "one\n\ntwo");
    assert_eq!(mail_body(&mail), "one\n\ntwo");
    assert_eq!(mail_body(&render_mail(&from, MailKind::Message, "")), "");
    assert_eq!(mail_body("no envelope\nat all"), "no envelope\nat all");
    let report = report_text("/review reported\n· 4s", "Blocked: x\nmore");
    assert_eq!(report, "/review reported · 4s\nBlocked: x\nmore");
    let mail = render_mail(&from, MailKind::Report, &report);
    assert_eq!(
        report_parts(mail_body(&mail)),
        ("/review reported · 4s", "Blocked: x\nmore")
    );
    assert_eq!(report_parts("only a head"), ("only a head", ""));
}
