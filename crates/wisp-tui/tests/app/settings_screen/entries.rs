use wisp_protocol::endpoint::{ConnectionField, StoreKind};
use wisp_protocol::settings::{
    AUTH_ANTHROPIC_SUBSCRIPTION, CredentialKind, EndpointSummary, ModelEntry, SavedEntry,
};
use wisp_tui::app::config_screen::{SAVING, SWITCH_FIRST};

use super::*;
use crate::events::{Changed, settings_changed};
use crate::shared::{change, named};

const WIDE: Look = Look::dark(120, 40);
const TALL: Look = Look::dark(120, 65);

fn endpoint(name: &str, host: &str, official: bool, default: bool) -> EndpointSummary {
    EndpointSummary {
        models: Vec::new(),
        mark: crate::mockup_mark(name),
        name: name.to_owned(),
        provider: "anthropic".to_owned(),
        credential: None,
        official: Some(official),
        host: Some(host.to_owned()),
        unavailable: None,
        priced: true,
        default,
        keys_file: false,
        expired: false,
        key_var: None,
        voice_model: None,
        complete: false,
        default_model: None,
    }
}

fn at_endpoint(name: &str, field: &str, text: &str, line: u64) -> SavedValue {
    SavedValue {
        path: SitePath::new(Place::Endpoint(name.to_owned()), find(field).unwrap()),
        entry: SavedEntry::Value {
            value: value_of(field, text),
        },
        line: Some(line),
    }
}

fn at_model(id: &str, field: &str, text: &str, line: u64) -> SavedValue {
    SavedValue {
        path: SitePath::new(Place::Model(id.to_owned()), find(field).unwrap()),
        entry: SavedEntry::Value {
            value: value_of(field, text),
        },
        line: Some(line),
    }
}

pub(super) fn with_entries(request: RequestId) -> SettingsSnapshot {
    let mut out = snapshot_of(request);
    out.endpoints = vec![
        endpoint("direct", "api.anthropic.com", true, false),
        endpoint("manifest", "gw.example.com", false, true),
    ];
    out.saved
        .push(at_endpoint("manifest", "key_store", "keyring", 7));
    out.saved
        .push(at_endpoint("direct", "key_store", "file", 11));
    out.saved.push(at_model(MODEL, "compact_at", "80%", 14));
    let model = ModelEntry {
        id: MODEL.to_owned(),
        shipped: true,
        configured: true,
        context_window: Some(1_000_000),
        listed: None,
        provider: None,
    };
    let gpt = ModelEntry {
        id: "gpt-5.5".to_owned(),
        context_window: None,
        ..model.clone()
    };
    let added = ModelEntry {
        id: "claude-zzz-9-9".to_owned(),
        shipped: false,
        ..gpt.clone()
    };
    out.models = vec![model, gpt, added];
    out
}

fn moved_default(request: RequestId) -> SettingsSnapshot {
    let mut out = with_entries(request);
    out.context.endpoint = "direct".to_owned();
    for e in &mut out.endpoints {
        e.default = e.name == "direct";
    }
    out
}

fn on_subscription(request: RequestId) -> SettingsSnapshot {
    let mut out = with_entries(request);
    for e in &mut out.endpoints {
        e.credential = Some(CredentialKind::Other(
            AUTH_ANTHROPIC_SUBSCRIPTION.to_owned(),
        ));
    }
    out.saved
        .push(at_endpoint("manifest", "max_in_flight", "4", 15));
    out
}

fn line_of<'a>(rows: &'a [String], name: &str) -> &'a str {
    rows.iter()
        .find(|r| {
            r.trim_start_matches(" \u{203a} ")
                .trim_start()
                .starts_with(&format!("{name}  "))
        })
        .map_or("", String::as_str)
}

async fn on_entry(
    stage: &mut Stage,
    snapshot: fn(RequestId) -> SettingsSnapshot,
    filter: &str,
) -> Vec<String> {
    let account = snapshot(RequestId(0))
        .endpoints
        .iter()
        .any(|e| e.name == filter);
    stage
        .open(
            if account {
                "/config endpoint"
            } else {
                "/config"
            },
            snapshot,
        )
        .await;
    if account {
        stage.key(KeyCode::Enter);
    }
    stage.char('/').typed(filter).key(KeyCode::Enter);
    stage
        .until(|r| shows(r, &format!(" Filter  {filter}")) && selected(r).contains(filter))
        .await
}

#[tokio::test]
async fn endpoints_and_models_are_rows_with_their_marks() {
    staged(TALL, |mut stage| async move {
        stage.open("/config endpoint", with_entries).await;
        stage.key(KeyCode::Enter);
        let rows = stage.until(|r| shows(r, "Add an API key")).await;
        assert!(
            line_of(&rows, "manifest ✓").contains("anthropic"),
            "{rows:#?}"
        );
        assert!(!line_of(&rows, "direct").contains("new chats"), "{rows:#?}");
        assert!(!shows(&rows, "+ Add"), "{rows:#?}");
        stage
            .key(KeyCode::Left)
            .key(KeyCode::End)
            .key(KeyCode::Right)
            .key(KeyCode::End)
            .key(KeyCode::Enter);
        let rows = stage
            .until(|r| {
                r.get(2).is_some_and(|row| row.trim() == "Models") && shows(r, "Add a model")
            })
            .await;
        assert!(shows(&rows, "1,000,000 window"), "{rows:#?}");
        assert!(
            !line_of(&rows, "gpt-5.5").contains("web search"),
            "{rows:#?}"
        );
        assert!(shows(&rows, "claude-zzz-9-9"), "{rows:#?}");
        assert!(!shows(&rows, " zzz-9-9"), "{rows:#?}");
    })
    .await;
}

#[tokio::test]
async fn right_on_an_account_lists_its_fields_and_esc_goes_back() {
    staged(WIDE, |mut stage| async move {
        let rows = on_entry(&mut stage, with_entries, "direct").await;
        assert!(selected(&rows).contains("direct"), "{rows:#?}");
        stage.char('?');
        stage.until(|r| shows(r, "Requests in flight")).await;
        stage.char('?').key(KeyCode::Right);
        let rows = stage.until(|r| shows(r, "Account direct")).await;
        assert!(shows(&rows, "set \u{b7} credentials.json"), "{rows:#?}");
        assert!(shows(&rows, "the provider's URL"), "{rows:#?}");
        stage.key(KeyCode::Esc);
        let rows = stage.until(|r| !shows(r, "Account direct")).await;
        assert!(selected(&rows).starts_with("direct"), "{rows:#?}");
        assert!(shows(&rows, " Filter  direct"), "the filter comes back");
        stage.nothing_sent().await;
    })
    .await;
}

#[tokio::test]
async fn a_subscription_account_shows_its_own_requests_in_flight() {
    staged(WIDE, |mut stage| async move {
        let rows = on_entry(&mut stage, on_subscription, "direct").await;
        assert!(selected(&rows).contains("direct"), "{rows:#?}");
        stage.char('?');
        let rows = stage.until(|r| shows(r, "Requests in flight")).await;
        assert!(
            rows.iter()
                .any(|r| r.contains("Requests in flight") && r.contains("12")),
            "{rows:#?}"
        );
        stage.char('?');
        stage
            .char('/')
            .ctrl('u')
            .typed("manifest")
            .key(KeyCode::Enter);
        stage.until(|r| selected(r).contains("manifest")).await;
        stage.key(KeyCode::Right);
        stage.until(|r| shows(r, "Account manifest")).await;
        stage.char('/').typed("max_in_flight").key(KeyCode::Enter);
        let rows = stage
            .until(|r| shows(r, "Wisp's own: 12") && shows(r, "d back to 12"))
            .await;
        assert!(
            selected(&rows).contains("Requests in flight  4"),
            "{rows:#?}"
        );
    })
    .await;
}

#[tokio::test]
async fn d_on_the_current_endpoint_is_refused_and_on_another_asks_then_removes() {
    staged(TALL, |mut stage| async move {
        let rows = on_entry(&mut stage, with_entries, "manifest").await;
        assert!(selected(&rows).contains("manifest"), "{rows:#?}");
        stage.char('d');
        stage
            .until(|r| shows(r, "Switch to another account first."))
            .await;
        stage.nothing_sent().await;
        stage
            .char('/')
            .ctrl('u')
            .typed("direct")
            .key(KeyCode::Enter);
        stage.until(|r| selected(r).contains("direct")).await;
        stage.char('d');
        stage
            .until(|r| {
                shows(
                    r,
                    "Remove \"direct\" and delete its key from credentials.json?",
                )
            })
            .await;
        stage.key(KeyCode::Enter);
        stage.nothing_sent().await;
        stage.char('d').key(KeyCode::Up).key(KeyCode::Enter);
        let ClientCommand::RemoveEndpoint {
            request,
            name,
            keep_key,
        } = stage.next().await
        else {
            panic!("expected RemoveEndpoint");
        };
        assert_eq!((name.as_str(), keep_key), ("direct", false));
        stage
            .emit(AgentEvent::EndpointRemoved {
                answers: request,
                name: "direct".to_owned(),
                kept_item: false,
                default_cleared: false,
                notes: vec!["\u{1b}[31mred\u{1b}[0m note".to_owned()],
            })
            .await;
        let again = stage.asked().await;
        let mut gone = with_entries(again);
        gone.endpoints.retain(|e| e.name != "direct");
        stage.emit(AgentEvent::SettingsSnapshot(gone)).await;
        stage.key(KeyCode::Esc);
        let rows = stage
            .until(|r| line_of(r, "direct").is_empty() && !line_of(r, "manifest ✓").is_empty())
            .await;
        assert!(line_of(&rows, "direct").is_empty(), "{rows:#?}");
        assert!(!line_of(&rows, "manifest ✓").is_empty(), "{rows:#?}");
        stage.close_settings().await;
        let rows = stage.until(|r| shows(r, "^[[31mred^[[0m note")).await;
        assert!(!rows.iter().any(|r| r.contains('\u{1b}')), "{rows:#?}");
    })
    .await;
}

#[tokio::test]
async fn a_connection_field_is_sent_as_an_endpoint_edit() {
    staged(WIDE, |mut stage| async move {
        on_entry(&mut stage, with_entries, "direct").await;
        stage.key(KeyCode::Right);
        stage.until(|r| shows(r, "Account direct")).await;
        stage.char('/').typed("key_store").key(KeyCode::Enter);
        let rows = stage.until(|r| selected(r).contains("file")).await;
        assert!(selected(&rows).contains("file"), "{rows:#?}");
        stage.key(KeyCode::Enter).key(KeyCode::Enter);
        stage.nothing_sent().await;
        stage.key(KeyCode::Enter);
        stage.until(|r| footer(r).contains("esc cancel")).await;
        stage.key(KeyCode::Right).key(KeyCode::Enter);
        let command = stage.next().await;
        let ClientCommand::EditEndpoint {
            request,
            name,
            store,
            ..
        } = command
        else {
            panic!("expected EditEndpoint, got {command:?}");
        };
        assert_eq!(name, "direct");
        assert_eq!(store, Some(wisp_protocol::endpoint::StoreChoice::Keyring));
        stage
            .emit(AgentEvent::EndpointEdited {
                answers: request,
                name: "direct".to_owned(),
                changed: vec![
                    ConnectionField::Store,
                    ConnectionField::Other("\u{1b}[2Jfield\u{202e}".to_owned()),
                ],
                store: Some(StoreKind::Keyring),
                check: wisp_protocol::endpoint::CheckResult::of(
                    wisp_protocol::endpoint::CheckKind::Skipped,
                ),
                notes: vec!["\u{1b}[31mred\u{1b}[0m note\u{202e}x".to_owned()],
            })
            .await;
        let again = stage.asked().await;
        stage
            .emit(AgentEvent::SettingsSnapshot(with_entries(again)))
            .await;
        stage.close_settings().await;
        let rows = stage
            .until(|r| shows(r, "^[[31mred^[[0m note<U+202E>x"))
            .await;
        assert!(shows(&rows, "^[[2Jfield<U+202E>"), "{rows:#?}");
        assert!(
            !rows
                .iter()
                .any(|r| r.contains('\u{1b}') || r.contains('\u{202e}')),
            "{rows:#?}"
        );
    })
    .await;
}

#[tokio::test]
async fn enter_on_an_account_saves_it_as_the_default_and_the_mark_moves() {
    staged(WIDE, |mut stage| async move {
        on_entry(&mut stage, with_entries, "direct").await;
        stage.key(KeyCode::Enter);
        let command = stage.next().await;
        let ClientCommand::ChangeSettings {
            request,
            changes,
            save,
        } = command
        else {
            panic!("expected ChangeSettings, got {command:?}");
        };
        assert!(save);
        assert_eq!(
            changes,
            [SettingChange {
                target: named("endpoint"),
                value: Some(SettingValue::Text("direct".to_owned())),
            }]
        );
        stage
            .emit(settings_changed(Changed {
                answers: Some(request),
                endpoint: "direct".into(),
                model: MODEL.into(),
                effort: wisp_protocol::Effort::High,
                changes: vec![AppliedChange {
                    saved: Some(Saved {
                        path_display: "~/.config/wisp/config.toml".to_owned(),
                        line: Some(1),
                        notes: Vec::new(),
                    }),
                    ..change(
                        named("endpoint"),
                        Some(SettingValue::Text("direct".to_owned())),
                        Layer::Session,
                        When::NextTurn,
                    )
                }],
                ..Changed::default()
            }))
            .await;
        let again = stage.asked().await;
        stage
            .emit(AgentEvent::SettingsSnapshot(moved_default(again)))
            .await;
        let rows = stage
            .until(|r| line_of(r, "direct ✓").contains("anthropic"))
            .await;
        assert!(
            line_of(&rows, "direct ✓").contains("anthropic"),
            "{rows:#?}"
        );
        assert!(!line_of(&rows, "manifest").contains("✓"), "{rows:#?}");
    })
    .await;
}

#[tokio::test]
async fn a_model_s_page_names_its_saved_place_by_the_file_key_not_by_this_model() {
    staged(WIDE, |mut stage| async move {
        on_entry(&mut stage, with_entries, "1,000,000").await;
        stage.key(KeyCode::Enter);
        stage.until(|r| shows(r, "Compact at")).await;
        stage
            .char('/')
            .typed("compact")
            .key(KeyCode::Enter)
            .char('?');
        let rows = stage
            .until(|r| shows(r, "config.toml line 14") && shows(r, "Name"))
            .await;
        let key = format!("models.\"{MODEL}\".compact_at");
        assert!(shows(&rows, &key), "{rows:#?}");
        assert!(
            !shows(&rows, "this model · config.toml line 14"),
            "{rows:#?}"
        );
    })
    .await;
}

#[tokio::test]
async fn d_on_a_model_unsets_all_its_saved_settings_in_one_change() {
    staged(WIDE, |mut stage| async move {
        on_entry(&mut stage, with_entries, "1,000,000").await;
        stage.char('d');
        stage.until(|r| shows(r, "Remove your settings for")).await;
        stage.key(KeyCode::Up).key(KeyCode::Enter);
        let command = stage.next().await;
        let ClientCommand::ChangeSettings { changes, save, .. } = command else {
            panic!("expected ChangeSettings, got {command:?}");
        };
        assert!(save);
        assert_eq!(
            changes,
            [SettingChange {
                target: SettingTarget::Path(SitePath::new(
                    Place::Model(MODEL.to_owned()),
                    find("compact_at").unwrap()
                )),
                value: None,
            }]
        );
    })
    .await;
}

#[tokio::test]
async fn the_current_default_account_needs_no_enter_and_an_unavailable_one_says_why() {
    fn broken(request: RequestId) -> SettingsSnapshot {
        let mut out = with_entries(request);
        out.endpoints.push(EndpointSummary {
            unavailable: Some("its key is \u{1b}[2Jmissing".to_owned()),
            ..endpoint("old", "old.example.com", false, false)
        });
        out
    }
    staged(WIDE, |mut stage| async move {
        stage.open("/config", broken).await;
        stage.char('/').typed("manifest").key(KeyCode::Enter);
        let rows = stage
            .until(|r| selected(r).contains("manifest ✓") && !footer(r).contains("enter done"))
            .await;
        assert!(!shows(&rows, "enter use it"), "{rows:#?}");
        stage.key(KeyCode::Enter);
        stage.nothing_sent().await;
        stage.char('/').ctrl('u').typed("old").key(KeyCode::Enter);
        let rows = stage.until(|r| shows(r, "its key is")).await;
        assert!(shows(&rows, "missing"), "{rows:#?}");
        assert!(!rows.iter().any(|r| r.contains('\u{1b}')), "{rows:#?}");
    })
    .await;
}

#[tokio::test]
async fn a_keychain_failure_on_a_store_only_move_offers_no_file_instead() {
    staged(WIDE, |mut stage| async move {
        on_entry(&mut stage, with_entries, "direct").await;
        stage.key(KeyCode::Right);
        stage.until(|r| shows(r, "Account direct")).await;
        stage.char('/').typed("key_store").key(KeyCode::Enter);
        stage.until(|r| selected(r).contains("file")).await;
        stage.key(KeyCode::Enter);
        stage.until(|r| footer(r).contains("esc cancel")).await;
        stage.key(KeyCode::Right).key(KeyCode::Enter);
        let command = stage.next().await;
        let ClientCommand::EditEndpoint {
            request,
            edit,
            store,
            ..
        } = command
        else {
            panic!("expected EditEndpoint, got {command:?}");
        };
        assert!(edit.is_empty(), "{edit:?}");
        assert_eq!(store, Some(wisp_protocol::endpoint::StoreChoice::Keyring));
        stage
            .emit(AgentEvent::EndpointNotSaved {
                answers: request,
                name: "direct".to_owned(),
                why: wisp_protocol::endpoint::NotSavedReason::KeychainFailed(
                    "isn't running".to_owned(),
                ),
            })
            .await;
        let rows = stage.until(|r| shows(r, "isn't running")).await;
        assert!(!shows(&rows, "credentials.json instead"), "{rows:#?}");
        stage.char('1');
        stage.nothing_sent().await;
    })
    .await;
}

#[tokio::test]
async fn enter_pressed_twice_sends_one_default_change() {
    staged(WIDE, |mut stage| async move {
        on_entry(&mut stage, with_entries, "direct").await;
        stage.key(KeyCode::Enter);
        let command = stage.next().await;
        assert!(
            matches!(command, ClientCommand::ChangeSettings { save: true, .. }),
            "{command:?}"
        );
        stage.key(KeyCode::Enter);
        stage.nothing_sent().await;
    })
    .await;
}

#[tokio::test]
async fn enter_on_an_account_after_the_agent_stopped_leaves_it_unsaved() {
    staged(WIDE, |mut stage| async move {
        on_entry(&mut stage, with_entries, "direct").await;
        stage.commands.close();
        tokio::task::yield_now().await;
        stage.key(KeyCode::Enter);
        let rows = stage.until(|r| shows(r, "The agent stopped")).await;
        assert!(!line_of(&rows, "direct").contains(SAVING), "{rows:#?}");
    })
    .await;
}

#[tokio::test]
async fn the_keys_file_account_is_never_removed() {
    fn with_keys_file(request: RequestId) -> SettingsSnapshot {
        let mut out = with_entries(request);
        out.endpoints.push(EndpointSummary {
            keys_file: true,
            ..endpoint("claude-api", "api.anthropic.com", true, false)
        });
        out
    }
    let card = Some(CardSetup {
        keys_file: "/srv/keys/.env".to_owned(),
        ..super::super::endpoints::card_setup()
    });
    staged_options(WIDE, display(), card, |mut stage| async move {
        stage.open("/config", with_keys_file).await;
        stage.char('/').typed("claude-api").key(KeyCode::Enter);
        let from = "Its key comes from /srv/keys/.env or your environment, so";
        let rows = stage.until(|r| selected(r).contains("claude-api")).await;
        assert!(shows(&rows, from), "{rows:#?}");
        stage.char('d');
        stage
            .until(|r| shows(r, &format!("Settings \u{b7} {from} it can't be removed.")))
            .await;
        stage.nothing_sent().await;
    })
    .await;
}

#[tokio::test]
async fn a_keychain_failure_on_remove_resends_keeping_the_key_and_d_waits_for_the_answer() {
    staged(WIDE, |mut stage| async move {
        on_entry(&mut stage, with_entries, "direct").await;
        stage.char('d').key(KeyCode::Up).key(KeyCode::Enter);
        let ClientCommand::RemoveEndpoint {
            request, keep_key, ..
        } = stage.next().await
        else {
            panic!("expected RemoveEndpoint");
        };
        assert!(!keep_key);
        stage.char('d');
        let rows = stage.unchanged_for_a_while().await;
        assert!(!shows(&rows, "Remove \"direct\""), "{rows:#?}");
        stage.nothing_sent().await;
        stage
            .emit(AgentEvent::EndpointNotSaved {
                answers: request,
                name: "direct".to_owned(),
                why: wisp_protocol::endpoint::NotSavedReason::KeychainFailed(
                    "isn't running".to_owned(),
                ),
            })
            .await;
        stage.until(|r| shows(r, "Remove only the table")).await;
        stage.key(KeyCode::Up).key(KeyCode::Enter);
        let ClientCommand::RemoveEndpoint { name, keep_key, .. } = stage.next().await else {
            panic!("expected RemoveEndpoint");
        };
        assert_eq!((name.as_str(), keep_key), ("direct", true));
    })
    .await;
}

#[tokio::test]
async fn a_failed_check_offers_to_save_without_it_and_resends_unchecked() {
    staged(WIDE, |mut stage| async move {
        on_entry(&mut stage, with_entries, "direct").await;
        stage.key(KeyCode::Right);
        stage.until(|r| shows(r, "Account direct")).await;
        stage.char('/').typed("key_store").key(KeyCode::Enter);
        stage.until(|r| selected(r).contains("file")).await;
        stage.key(KeyCode::Enter);
        stage.until(|r| footer(r).contains("esc cancel")).await;
        stage.key(KeyCode::Right).key(KeyCode::Enter);
        let ClientCommand::EditEndpoint {
            request,
            check,
            store,
            ..
        } = stage.next().await
        else {
            panic!("expected EditEndpoint");
        };
        assert!(check);
        stage
            .emit(AgentEvent::EndpointNotSaved {
                answers: request,
                name: "direct".to_owned(),
                why: wisp_protocol::endpoint::NotSavedReason::Check(
                    wisp_protocol::endpoint::CheckResult::of(
                        wisp_protocol::endpoint::CheckKind::Unreachable,
                    ),
                ),
            })
            .await;
        stage
            .until(|r| said(r, "Save it anyway, without the"))
            .await;
        stage.char('1');
        let ClientCommand::EditEndpoint {
            check: again,
            store: same,
            ..
        } = stage.next().await
        else {
            panic!("expected EditEndpoint");
        };
        assert!(!again);
        assert_eq!(same, store);
    })
    .await;
}

fn switching_to_direct(request: RequestId) -> SettingsSnapshot {
    let mut out = with_entries(request);
    out.context.endpoint = "direct".to_owned();
    out
}

#[tokio::test]
async fn neither_the_live_endpoint_nor_a_pending_one_is_removed() {
    for name in ["manifest", "direct"] {
        staged_on(
            WIDE,
            display(),
            None,
            two_endpoints(),
            |mut stage| async move {
                stage.open("/config", switching_to_direct).await;
                stage.char('/').typed(name).key(KeyCode::Enter);
                let rows = stage.until(|r| selected(r).contains(name)).await;
                assert!(!shows(&rows, SWITCH_FIRST), "{name}: {rows:#?}");
                stage.char('d');
                stage.until(|r| shows(r, SWITCH_FIRST)).await;
                stage.nothing_sent().await;
            },
        )
        .await;
    }
}

#[tokio::test]
async fn a_refresh_during_the_question_is_checked_before_remove_sends() {
    staged(WIDE, |mut stage| async move {
        on_entry(&mut stage, with_entries, "direct").await;
        stage.char('d');
        stage.until(|r| shows(r, "Remove \"direct\"")).await;
        stage
            .emit(AgentEvent::EndpointsChanged {
                endpoints: Vec::new(),
            })
            .await;
        let again = stage.asked().await;
        let mut refreshed = switching_to_direct(again);
        for endpoint in &mut refreshed.endpoints {
            endpoint.default = endpoint.name == "direct";
        }
        stage.emit(AgentEvent::SettingsSnapshot(refreshed)).await;
        stage
            .until(|r| line_of(r, "direct ✓").contains("anthropic"))
            .await;
        stage.key(KeyCode::Up).key(KeyCode::Enter);
        stage.until(|r| shows(r, SWITCH_FIRST)).await;
        stage.nothing_sent().await;
    })
    .await;
}

async fn resized(
    stage: &mut Stage,
    rows: u16,
    cols: u16,
    done: impl Fn(&[String]) -> bool,
) -> Vec<String> {
    let size = stage.screen.resize(rows, cols);
    stage.input(Input::Resize(size));
    stage
        .until(|r| r.len() == usize::from(rows) && done(r))
        .await
}

#[tokio::test]
async fn the_remove_question_keeps_esc_cancel_in_the_footer_at_every_width() {
    staged(Look::dark(40, 40), |mut stage| async move {
        on_entry(&mut stage, with_entries, "direct").await;
        stage.char('d');
        let rows = stage.until(|r| shows(r, "Remove \"direct\"")).await;
        assert!(footer(&rows).contains("esc cancel"), "{rows:#?}");
        assert!(shows(&rows, "esc cancel \u{b7} enter choose"), "{rows:#?}");
        for cols in 41..=200 {
            let size = stage.screen.resize(40, cols);
            stage.input(Input::Resize(size));
            let rows = stage
                .until(|r| {
                    r.get(1)
                        .is_some_and(|rule| rule.chars().count() == usize::from(cols))
                })
                .await;
            assert!(footer(&rows).contains("esc cancel"), "{cols}: {rows:#?}");
            assert!(said(&rows, "Remove"), "{cols}: {rows:#?}");
        }
        assert!(
            footer(&stage.screen.visible()).contains("esc cancel \u{b7} enter choose"),
            "{:#?}",
            stage.screen.visible()
        );
        for closing in [KeyCode::Esc, KeyCode::Char('q')] {
            stage.key(closing);
            stage.until(|r| !shows(r, "Remove \"direct\"")).await;
            stage.nothing_sent().await;
            stage.char('d');
            stage.until(|r| shows(r, "Remove \"direct\"")).await;
        }
        let answers = |r: &[String]| shows(r, "1 Remove") && shows(r, "\u{203a} 2 Keep");
        resized(&mut stage, 14, 180, |r| {
            let keys = r
                .iter()
                .filter(|l| l.contains("esc cancel \u{b7} enter choose"));
            shows(r, "? Remove \"direct\"") && answers(r) && keys.count() == 2
        })
        .await;
        resized(&mut stage, 16, 40, |r| {
            shows(r, "from credentials.json?") && answers(r) && shows(r, "enter choose")
        })
        .await;
        let rows = resized(&mut stage, 12, 40, |r| {
            shows(r, "delete its key\u{2026}") && answers(r) && shows(r, "enter choose")
        })
        .await;
        assert!(!shows(&rows, "from credentials.json?"), "{rows:#?}");
        stage.key(KeyCode::Enter);
        stage.until(|r| !shows(r, "Remove \"direct\"")).await;
        stage.nothing_sent().await;
        stage.char('d').char('1');
        stage.until(|r| shows(r, "\u{203a} 1 Remove")).await;
        stage.nothing_sent().await;
        stage.key(KeyCode::Enter);
        let ClientCommand::RemoveEndpoint { name, .. } = stage.next().await else {
            panic!("expected RemoveEndpoint");
        };
        assert_eq!(name, "direct");
    })
    .await;
}

#[tokio::test]
async fn a_connection_field_editor_keeps_esc_cancel_first_at_every_width() {
    staged(Look::dark(40, 40), |mut stage| async move {
        on_entry(&mut stage, with_entries, "direct").await;
        stage.key(KeyCode::Right);
        stage.until(|r| shows(r, "Account direct")).await;
        stage.char('/').typed("key_store").key(KeyCode::Enter);
        stage.until(|r| selected(r).contains("file")).await;
        stage.key(KeyCode::Enter);
        stage.until(|r| footer(r).contains("esc cancel")).await;
        for cols in 41..=200 {
            let size = stage.screen.resize(40, cols);
            stage.input(Input::Resize(size));
            let rows = stage
                .until(|r| {
                    r.get(1)
                        .is_some_and(|rule| rule.chars().count() == usize::from(cols))
                })
                .await;
            let hints = footer(&rows).split("Settings").nth(1).unwrap_or_default();
            assert!(
                hints.trim_start().starts_with("esc cancel"),
                "{cols}: {rows:#?}"
            );
        }
    })
    .await;
}
