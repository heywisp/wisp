use wisp_protocol::endpoint::{CheckKind, CheckResult, NotSavedReason, StoreChoice, StoreKind};
use wisp_protocol::settings::{CredentialKind, EndpointSummary};
use wisp_protocol::{AgentEvent, RequestId, VariableName};
use wisp_render::chrome::endpoint_card::{
    self as words, FieldValue, StatusTone, endpoint_card_rows,
};
use wisp_render::chrome::{Credential, Endpoint};
use wisp_render::markdown::Layout;
use wisp_render::theme::glyphs::GlyphSet;

use super::*;
use crate::term::{Key, KeyCode};

const CANARY: &str = "sk-ant-CANARYCARDKEY0042";

fn anthropic() -> ProviderEntry {
    ProviderEntry {
        name: "anthropic".into(),
        title: "Anthropic".into(),
        default_url: "https://api.anthropic.com".into(),
        auths: vec![
            AuthEntry {
                name: "api_key".into(),
                words: "x-api-key header (an API key)".into(),
            },
            AuthEntry {
                name: "bearer".into(),
                words: "Authorization: Bearer header (a token)".into(),
            },
        ],
        official_auth: "api_key".into(),
    }
}

fn setup() -> CardSetup {
    CardSetup {
        catalogue: vec![anthropic()],
        presence: Vec::new(),
        credentials_shown: "~/.config/wisp/credentials.json".into(),
        keys_file: "~/.config/wisp/.env".into(),
        lead: None,
    }
}

fn card(mode: CardMode) -> EndpointCard {
    EndpointCard::new(mode, setup(), Vec::new())
}

fn press(card: &mut EndpointCard, code: KeyCode) -> Step {
    card.key(Key::plain(code))
}

fn typed(card: &mut EndpointCard, text: &str) {
    for c in text.chars() {
        press(card, KeyCode::Char(c));
    }
}

fn sent(step: Step) -> SaveRequest {
    match step {
        Step::Send(request) => request,
        other => panic!("{other:?}"),
    }
}

fn official_request(card: &mut EndpointCard) -> SaveRequest {
    assert_eq!(press(card, KeyCode::Enter), Step::Stay);
    assert_eq!(card.focus(), Field::Key);
    typed(card, CANARY);
    assert_eq!(press(card, KeyCode::Enter), Step::Stay);
    assert_eq!(card.focus(), Field::Name);
    let request = sent(press(card, KeyCode::Enter));
    card.sent(RequestId(7));
    request
}

fn shown(card: &EndpointCard) -> String {
    format!("{:?}", card.view(GlyphSet::Unicode))
}

fn not_saved(why: NotSavedReason) -> AgentEvent {
    AgentEvent::EndpointNotSaved {
        answers: RequestId(7),
        name: "anthropic".into(),
        why,
    }
}

fn check(outcome: CheckKind, reason: Option<&str>) -> NotSavedReason {
    NotSavedReason::Check(CheckResult {
        outcome,
        reason: reason.map(str::to_owned),
    })
}

fn menu(card: &EndpointCard) -> (String, Vec<String>, usize) {
    let menu = card.view(GlyphSet::Unicode).menu.expect("a menu");
    (menu.question, menu.options, menu.selected)
}

#[test]
fn the_official_flow_sends_one_checked_save_with_the_suggested_name() {
    let mut card = card(CardMode::FirstRun);
    assert_eq!(card.focus(), Field::Connect);
    let request = official_request(&mut card);
    assert_eq!(request.endpoint.name, "anthropic");
    assert_eq!(request.endpoint.provider, "anthropic");
    assert_eq!(request.endpoint.auth, "api_key");
    assert_eq!(request.endpoint.url, None);
    assert_eq!(request.endpoint.store, StoreChoice::Auto);
    assert!(request.endpoint.default);
    assert!(request.check);
    assert_eq!(request.key.expose(), CANARY);
    assert!(card.saving());
    assert_eq!(card.pending(), Some(RequestId(7)));
    let added = sent({
        let mut add = EndpointCard::new(CardMode::Add, setup(), vec!["anthropic".into()]);
        press(&mut add, KeyCode::Enter);
        typed(&mut add, CANARY);
        press(&mut add, KeyCode::Enter);
        press(&mut add, KeyCode::Enter)
    });
    assert_eq!(added.endpoint.name, "anthropic-2");
    assert!(!added.endpoint.default);
}

#[test]
fn the_key_never_enters_rendered_text_or_debug() {
    let mut card = card(CardMode::FirstRun);
    press(&mut card, KeyCode::Tab);
    let holds = |view: &words::EndpointCardView, key: &str| {
        view.fields
            .iter()
            .any(|f| matches!(&f.value, FieldValue::Secret(value, _) if value.expose() == key))
    };
    assert!(holds(&card.view(GlyphSet::Unicode), ""));
    typed(&mut card, "a");
    let one = shown(&card);
    card.paste(&format!("  {CANARY}\n"));
    let long = shown(&card);
    assert!(!one.contains("CANARY"));
    assert!(!long.contains("CANARY"), "{long}");
    assert!(!format!("{card:?}").contains("CANARY"));
    for glyphs in [GlyphSet::Ascii, GlyphSet::Unicode] {
        let view = card.view(glyphs);
        assert!(holds(&view, CANARY));
        for width in [40, 80, 120] {
            let rows: String = endpoint_card_rows(&view, Layout::new(width, glyphs))
                .lines
                .iter()
                .flat_map(|l| l.spans.iter().map(|s| s.text.as_str()))
                .collect();
            assert!(rows.contains("0042") && !rows.contains("CANARY"), "{rows}");
        }
    }
    let request = sent({
        press(&mut card, KeyCode::Enter);
        press(&mut card, KeyCode::Enter)
    });
    assert_eq!(request.key.expose(), CANARY);
    assert!(!format!("{request:?}").contains("CANARY"));
}

#[test]
fn a_refused_paste_in_the_account_key_field_says_so_at_once() {
    let mut card = card(CardMode::FirstRun);
    press(&mut card, KeyCode::Tab);
    for c in CANARY.chars() {
        press(&mut card, KeyCode::Char(c));
        let rows = endpoint_card_rows(
            &card.view(GlyphSet::Unicode),
            Layout::new(80, GlyphSet::Unicode),
        );
        let text = rows
            .lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.as_str()))
            .collect::<String>();
        assert!(!text.contains("0042"));
    }
    card.paste("two\nlines");
    assert!(
        card.view(GlyphSet::Unicode)
            .status
            .iter()
            .any(|(s, _)| s.contains("That key"))
    );
    assert_eq!(card.key.value().expose(), CANARY);
    assert_eq!(card.key.ends(), Some(wisp_protocol::Ends::default()));
    press(&mut card, KeyCode::Tab);
    assert_eq!(card.focus(), Field::Name);
}

#[test]
fn a_gateway_asks_for_its_url_and_auth_and_names_itself_after_the_host() {
    let mut card = card(CardMode::FirstRun);
    press(&mut card, KeyCode::Down);
    press(&mut card, KeyCode::Enter);
    assert_eq!(card.focus(), Field::Url);
    card.paste("https://gw.example.com/anthropic\n");
    press(&mut card, KeyCode::Enter);
    assert_eq!(card.focus(), Field::Auth);
    press(&mut card, KeyCode::Char('2'));
    press(&mut card, KeyCode::Enter);
    card.paste(CANARY);
    press(&mut card, KeyCode::Enter);
    let request = sent(press(&mut card, KeyCode::Enter));
    assert_eq!(request.endpoint.name, "gw");
    assert_eq!(request.endpoint.auth, "bearer");
    assert_eq!(
        request.endpoint.url.as_ref().map(|u| u.expose().to_owned()),
        Some("https://gw.example.com/anthropic".to_owned())
    );
}

#[test]
fn plain_http_to_another_host_asks_first_and_no_goes_back_to_the_url() {
    let mut card = card(CardMode::FirstRun);
    press(&mut card, KeyCode::Char('2'));
    press(&mut card, KeyCode::Enter);
    typed(&mut card, "http://gw.example.com");
    press(&mut card, KeyCode::Enter);
    let (question, options, selected) = menu(&card);
    assert!(
        question.contains("plain http to gw.example.com"),
        "{question}"
    );
    assert_eq!(options, ["Yes", "No"]);
    assert_eq!(selected, 1);
    press(&mut card, KeyCode::Enter);
    assert_eq!(card.focus(), Field::Url);
    assert!(card.view(GlyphSet::Unicode).status.is_empty());
    press(&mut card, KeyCode::Enter);
    press(&mut card, KeyCode::Char('1'));
    assert_eq!(card.focus(), Field::Auth);
    let mut local = EndpointCard::new(CardMode::FirstRun, setup(), Vec::new());
    press(&mut local, KeyCode::Char('2'));
    press(&mut local, KeyCode::Enter);
    typed(&mut local, "http://127.0.0.1:8080");
    press(&mut local, KeyCode::Enter);
    assert!(local.view(GlyphSet::Unicode).menu.is_none());
    assert_eq!(local.focus(), Field::Auth);
}

#[test]
fn local_checks_stop_on_the_field_with_a_reason_that_never_echoes() {
    let mut card = card(CardMode::Add);
    press(&mut card, KeyCode::Char('2'));
    press(&mut card, KeyCode::Enter);
    typed(&mut card, "ftp://CANARYHOST");
    press(&mut card, KeyCode::Enter);
    let view = card.view(GlyphSet::Unicode);
    assert_eq!(
        view.status,
        [(words::NOT_HTTP.to_owned(), StatusTone::Error)]
    );
    assert_eq!(card.focus(), Field::Url);
    press(&mut card, KeyCode::BackTab);
    press(&mut card, KeyCode::Char('1'));
    press(&mut card, KeyCode::Enter);
    typed(&mut card, "sk ant CANARY");
    press(&mut card, KeyCode::Enter);
    let status = &card.view(GlyphSet::Unicode).status;
    assert!(
        status[0].0.starts_with("That key has a space"),
        "{status:?}"
    );
    assert!(status[0].0.ends_with("Paste it again."));
    assert!(!format!("{status:?}").contains("CANARY"));
    assert_eq!(card.focus(), Field::Key);
    let before = card.view(GlyphSet::Unicode).status;
    for key in [
        Key::plain(KeyCode::Char('x')),
        Key::plain(KeyCode::Backspace),
        Key::ctrl('u'),
    ] {
        card.key(key);
        assert_eq!(card.view(GlyphSet::Unicode).status, before);
    }
    let mut taken = EndpointCard::new(CardMode::Add, setup(), vec!["work".into()]);
    press(&mut taken, KeyCode::Enter);
    typed(&mut taken, CANARY);
    press(&mut taken, KeyCode::Enter);
    taken.key(Key::ctrl('u'));
    typed(&mut taken, "Work!");
    assert_eq!(press(&mut taken, KeyCode::Enter), Step::Stay);
    assert_eq!(
        taken.view(GlyphSet::Unicode).status[0].0,
        words::taken_name("work")
    );
    taken.key(Key::ctrl('u'));
    typed(&mut taken, "env");
    press(&mut taken, KeyCode::Enter);
    assert_eq!(
        taken.view(GlyphSet::Unicode).status[0].0,
        wisp_protocol::account_name_reserved()
    );
}

#[test]
fn a_refused_key_offers_enter_again_save_anyway_and_cancel() {
    let mut file_asked = false;
    let mut card = card(CardMode::FirstRun);
    let first = official_request(&mut card);
    let answer = card.answer(
        &not_saved(check(CheckKind::KeyRefused, None)),
        &mut file_asked,
        GlyphSet::Unicode,
    );
    assert_eq!(answer, Answered::Shown);
    let (question, options, _) = menu(&card);
    assert_eq!(question, words::KEY_REFUSED);
    assert_eq!(options, ["Enter it again", "Save anyway", "Cancel"]);
    press(&mut card, KeyCode::Enter);
    assert_eq!(card.focus(), Field::Key);
    assert!(card.key.value().expose().is_empty());
    typed(&mut card, "wrong");
    card.key(Key::ctrl('u'));
    assert!(card.key.value().expose().is_empty());
    typed(&mut card, "sk-ant-NEWKEYxy");
    press(&mut card, KeyCode::Backspace);
    card.key(Key::alt(KeyCode::Backspace));
    press(&mut card, KeyCode::Enter);
    let again = sent(press(&mut card, KeyCode::Enter));
    assert!(again.check);
    assert_eq!(again.key.expose(), "sk-ant-NEWKEY");
    card.sent(RequestId(7));
    card.answer(
        &not_saved(check(CheckKind::KeyRefused, None)),
        &mut file_asked,
        GlyphSet::Unicode,
    );
    let anyway = sent(press(&mut card, KeyCode::Char('2')));
    assert!(!anyway.check);
    assert_eq!(anyway.endpoint, first.endpoint);
    card.sent(RequestId(8));
    card.answer(
        &AgentEvent::EndpointNotSaved {
            answers: RequestId(8),
            name: "anthropic".into(),
            why: check(CheckKind::KeyRefused, None),
        },
        &mut file_asked,
        GlyphSet::Unicode,
    );
    assert_eq!(press(&mut card, KeyCode::Char('3')), Step::Stay);
    assert!(!card.saving());
    assert_eq!(
        card.view(GlyphSet::Unicode).status,
        [(words::NOT_SAVED.to_owned(), StatusTone::Muted)]
    );
    sent(press(&mut card, KeyCode::Enter));
    card.sent(RequestId(7));
    card.answer(
        &not_saved(check(CheckKind::KeyRefused, None)),
        &mut file_asked,
        GlyphSet::Unicode,
    );
    assert_eq!(press(&mut card, KeyCode::Esc), Step::Stay);
    assert!(!card.saving());
    assert_eq!(card.view(GlyphSet::Unicode).status[0].0, words::NOT_SAVED);
}

#[test]
fn forbidden_and_not_offered_ask_and_yes_saves_without_the_check() {
    for (outcome, question) in [
        (CheckKind::Forbidden, words::forbidden_ask()),
        (CheckKind::NotOffered, words::not_offered_ask()),
    ] {
        let mut file_asked = false;
        let mut card = card(CardMode::FirstRun);
        official_request(&mut card);
        card.answer(
            &not_saved(check(outcome, None)),
            &mut file_asked,
            GlyphSet::Unicode,
        );
        let (shown, options, selected) = menu(&card);
        assert_eq!(shown, question);
        assert_eq!(options, ["Yes", "No"]);
        assert_eq!(selected, 0);
        let mut escaped = card.clone();
        assert_eq!(press(&mut escaped, KeyCode::Esc), Step::Stay);
        let yes = sent(press(&mut card, KeyCode::Enter));
        assert!(!yes.check);
    }
}

#[test]
fn a_failed_check_offers_the_url_for_a_gateway_and_a_retry_for_the_official_url() {
    let mut file_asked = false;
    let mut card = card(CardMode::FirstRun);
    official_request(&mut card);
    card.answer(
        &not_saved(check(
            CheckKind::Unreachable,
            Some("unreachable: timed out"),
        )),
        &mut file_asked,
        GlyphSet::Unicode,
    );
    let (question, options, _) = menu(&card);
    assert_eq!(question, "The check failed: unreachable: timed out.");
    assert_eq!(options, ["Try again", "Save anyway", "Cancel"]);
    let retry = sent(press(&mut card, KeyCode::Enter));
    assert!(retry.check);
    let mut gateway = EndpointCard::new(CardMode::FirstRun, setup(), Vec::new());
    press(&mut gateway, KeyCode::Char('2'));
    press(&mut gateway, KeyCode::Enter);
    typed(&mut gateway, "https://gw.example.com");
    press(&mut gateway, KeyCode::Enter);
    press(&mut gateway, KeyCode::Enter);
    typed(&mut gateway, CANARY);
    press(&mut gateway, KeyCode::Enter);
    sent(press(&mut gateway, KeyCode::Enter));
    gateway.sent(RequestId(7));
    gateway.answer(
        &not_saved(check(
            CheckKind::Unexpected,
            Some("unexpected answer (500)"),
        )),
        &mut file_asked,
        GlyphSet::Unicode,
    );
    assert_eq!(menu(&gateway).1[0], "Enter the URL again");
    press(&mut gateway, KeyCode::Enter);
    assert_eq!(gateway.focus(), Field::Url);
    assert!(!gateway.saving());
}

#[test]
fn a_failed_keychain_write_asks_about_the_file_once_per_run() {
    let mut file_asked = false;
    let mut card = card(CardMode::FirstRun);
    official_request(&mut card);
    card.answer(
        &not_saved(NotSavedReason::KeychainFailed("didn't answer".into())),
        &mut file_asked,
        GlyphSet::Unicode,
    );
    assert!(file_asked);
    let (question, options, selected) = menu(&card);
    assert_eq!(
        question,
        "The keychain didn't answer. Keep this key in ~/.config/wisp/credentials.json instead?"
    );
    assert_eq!(options, ["Yes", "No"]);
    assert_eq!(selected, 1);
    let file = sent(press(&mut card, KeyCode::Char('1')));
    assert_eq!(file.endpoint.store, StoreChoice::File);
    assert!(!file.check);
    let mut again = EndpointCard::new(CardMode::Add, setup(), Vec::new());
    official_request(&mut again);
    again.answer(
        &not_saved(NotSavedReason::KeychainFailed("isn't running".into())),
        &mut file_asked,
        GlyphSet::Unicode,
    );
    let view = again.view(GlyphSet::Unicode);
    assert!(view.menu.is_none());
    assert_eq!(
        view.status,
        [(words::keychain_stopped("isn't running"), StatusTone::Error)]
    );
    assert!(!format!("{view:?}").contains("credentials.json"));
}

#[test]
fn a_refusal_shows_its_reason_and_never_offers_the_file() {
    let mut file_asked = false;
    let mut card = card(CardMode::FirstRun);
    official_request(&mut card);
    let refusal = AgentEvent::endpoint_refused(
        Some(RequestId(7)),
        "the keychain may have been saved there\u{9b}2J",
    );
    assert_eq!(
        card.answer(&refusal, &mut file_asked, GlyphSet::Unicode),
        Answered::Shown
    );
    assert!(!file_asked);
    let view = card.view(GlyphSet::Unicode);
    assert!(view.menu.is_none());
    assert!(!format!("{view:?}").contains('\u{9b}'));
    assert_eq!(view.status[0].1, StatusTone::Error);
    assert!(view.active);
}

#[test]
fn only_the_pending_request_is_answered() {
    let mut file_asked = false;
    let mut card = card(CardMode::FirstRun);
    official_request(&mut card);
    let other = AgentEvent::EndpointSaved {
        answers: RequestId(3),
        name: "x".into(),
        store: StoreKind::File,
        check: CheckResult::of(CheckKind::Ok),
        notes: Vec::new(),
    };
    assert_eq!(
        card.answer(&other, &mut file_asked, GlyphSet::Unicode),
        Answered::Not
    );
    assert!(card.saving());
}

#[test]
fn a_save_shows_where_the_key_went_and_its_notes() {
    let mut file_asked = false;
    let mut card = card(CardMode::FirstRun);
    official_request(&mut card);
    let saved = AgentEvent::EndpointSaved {
        answers: RequestId(7),
        name: "anthropic".into(),
        store: StoreKind::Keyring,
        check: CheckResult::of(CheckKind::Ok),
        notes: vec!["note: a\u{9b}31mx\u{7}b".into()],
    };
    let answer = card.answer(&saved, &mut file_asked, GlyphSet::Unicode);
    assert!(matches!(answer, Answered::Saved { ref name, .. } if name == "anthropic"));
    assert!(card.is_saved());
    let status = card.view(GlyphSet::Unicode).status;
    assert_eq!(
        status[0],
        (
            "Checking\u{2026} ok \u{00B7} saved in the keychain".to_owned(),
            StatusTone::Ok
        )
    );
    assert!(!status[1].0.contains('\u{9b}'));
    assert_eq!(press(&mut card, KeyCode::Enter), Step::Stay);
}

#[test]
fn esc_quits_the_first_run_and_closes_the_add_card() {
    let mut first = card(CardMode::FirstRun);
    assert_eq!(press(&mut first, KeyCode::Esc), Step::Quit);
    assert_eq!(first.key(Key::ctrl('c')), Step::Quit);
    let mut add = card(CardMode::Add);
    assert_eq!(press(&mut add, KeyCode::Esc), Step::Close);
    assert_eq!(add.key(Key::ctrl('c')), Step::Close);
    let mut saving = card(CardMode::FirstRun);
    official_request(&mut saving);
    assert_eq!(saving.key(Key::ctrl('c')), Step::Quit);
    let status = saving.view(GlyphSet::Unicode).status;
    assert_eq!(
        status,
        [
            (words::saving_words(GlyphSet::Unicode), StatusTone::Muted),
            (words::QUIT_AFTER_SAVE.to_owned(), StatusTone::Muted)
        ]
    );
}

#[test]
fn keys_found_elsewhere_are_named_with_the_import_command() {
    let mut with = setup();
    with.presence = vec![VariableName::from_static("ANTHROPIC_API_KEY")];
    let card = EndpointCard::new(CardMode::Add, with, Vec::new());
    assert_eq!(
        card.view(GlyphSet::Unicode).note.as_deref(),
        Some(
            "Keys were found in ANTHROPIC_API_KEY. To store them, quit and run wisp account import (or wisp setup)."
        )
    );
}

#[test]
fn names_are_suggested_from_the_provider_or_the_gateway_host() {
    assert_eq!(suggested_name("anthropic", None, &[]), "anthropic");
    assert_eq!(suggested_name("gemini", None, &[]), "gemini-api");
    assert_eq!(
        suggested_name("anthropic", Some("https://gw.example.com/x"), &[]),
        "gw"
    );
    assert_eq!(
        suggested_name("anthropic", Some("http://user:pw@Proxy_1.local:8080"), &[]),
        "proxy_1"
    );
    assert_eq!(
        suggested_name("anthropic", Some("https://[::1]:9000"), &[]),
        "gateway"
    );
    assert_eq!(
        suggested_name("anthropic", Some("https://env.example.com"), &[]),
        "gateway"
    );
    assert_eq!(
        suggested_name(
            "anthropic",
            None,
            &["anthropic".into(), "anthropic-2".into()]
        ),
        "anthropic-3"
    );
}

#[test]
fn a_summary_becomes_the_facts_the_picker_shows() {
    let summary = EndpointSummary {
        models: Vec::new(),
        mark: None,
        name: "gw".into(),
        provider: "anthropic".into(),
        credential: Some(CredentialKind::AuthToken),
        official: Some(false),
        host: Some("gw.example.com".into()),
        unavailable: None,
        priced: true,
        default: false,
        keys_file: false,
        expired: false,
        key_var: None,
        voice_model: None,
        complete: false,
        default_model: None,
    };
    let facts = endpoint_facts_of(&summary);
    assert_eq!(facts.credential, Some(Credential::AuthToken));
    assert_eq!(facts.reach, Some(Endpoint::Custom));
    assert_eq!(facts.host.as_deref(), Some("gw.example.com"));
    let unknown = EndpointSummary {
        credential: Some(CredentialKind::Other("oauth".into())),
        official: None,
        ..summary
    };
    let facts = endpoint_facts_of(&unknown);
    assert_eq!(facts.credential, None);
    assert_eq!(facts.reach, None);
}

#[test]
fn the_cards_debug_never_shows_the_url_it_holds() {
    let mut card = card(CardMode::Add);
    press(&mut card, KeyCode::Char('2'));
    press(&mut card, KeyCode::Enter);
    typed(&mut card, "https://gw.example.com/v1?token=CANARYQUERY");
    press(&mut card, KeyCode::Enter);
    let debug = format!("{card:?}");
    assert!(!debug.contains("CANARYQUERY"), "{debug}");
    assert!(debug.contains("url: \"<redacted>\""), "{debug}");
    assert!(format!("{:?}", self::card(CardMode::Add)).contains("url: \"\""));
}
