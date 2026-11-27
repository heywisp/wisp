use std::future::Future;
use std::time::Duration;

use tokio::time::sleep;
use wisp_protocol::endpoint::{CheckKind, CheckResult, ConnectionField, StoreKind};
use wisp_protocol::plan_usage::{UsageScope, UsageSource, UsageWindow};
use wisp_protocol::settings::{CredentialKind, KeyVariable};
use wisp_protocol::subscription_auth::{
    AuthCommand, AuthEvent, AuthOutcome, ChallengeKind, Followed, LocalState, PrivateChallenge,
    Stayed, SubscriptionEndpoint, SubscriptionProvider, Way,
};
use wisp_protocol::{
    AgentEvent, ClientCommand, CompactionFailure, CompactionTrigger, DeliveryMode, Effort,
    ErrorKind, InputId, NOT_SHOWN, RejectKind, RequestId, RetryFocus, TurnOutcome, WithdrawnBy,
};
use wisp_protocol::{ErrorDetail, FailurePhase, FailureSource, RetryClass, Secret};
use wisp_render::chrome::account::AccountKind;
use wisp_render::chrome::endpoint_card as words;
use wisp_render::chrome::{Endpoint, EndpointFacts};
use wisp_tui::app::highlight::Start;
use wisp_tui::app::{Credential, Options, SessionFacts};
use wisp_tui::term::{Input, KeyCode, Mouse, MouseKind};

use super::scenes::on_accounts;
use super::steering::{Agent, position, row_with, sent_id, steering_facts};
use super::{Screen, eventually, facts, options, real_wait};
use crate::Drive;
use crate::events::{Changed, End, Ending, settings_changed, turn_ended};
use crate::shared::offer;

pub(crate) fn endpoint(name: &str, reach: Endpoint) -> EndpointFacts {
    EndpointFacts {
        models: Vec::new(),
        mark: crate::mockup_mark(name),
        name: name.to_owned(),
        provider: "anthropic".to_owned(),
        credential: Some(Credential::ApiKey),
        reach: Some(reach),
        host: (reach == Endpoint::Custom).then(|| "gw.example.com".to_owned()),
        unavailable: None,
        priced: true,
        expired: false,
        key_var: None,
        complete: false,
        default_model: None,
    }
}

pub(super) fn two_endpoints() -> SessionFacts {
    SessionFacts {
        endpoint_name: Some("direct".to_owned()),
        endpoints: vec![
            endpoint("direct", Endpoint::Official),
            endpoint("gw", Endpoint::Custom),
        ],
        ..facts()
    }
}

async fn retrying<D, F>(drive: D)
where
    D: FnOnce(Agent) -> F,
    F: Future<Output = ()>,
{
    retrying_with(options(Start::Stalled), drive).await;
}

async fn retrying_with<D, F>(options: Options, drive: D)
where
    D: FnOnce(Agent) -> F,
    F: Future<Output = ()>,
{
    retrying_on(two_endpoints(), options, drive).await;
}

async fn retrying_on<D, F>(facts: SessionFacts, options: Options, drive: D)
where
    D: FnOnce(Agent) -> F,
    F: Future<Output = ()>,
{
    steering_facts(Screen::new(30, 80), options, facts, drive).await;
}

fn offered(kind: ErrorKind, retry: bool, switch: bool, focus: RetryFocus) -> AgentEvent {
    turn_ended(End {
        ending: Ending::Error(kind),
        message: "Upstream provider stream was interrupted.".to_owned(),
        committed_steps: 2,
        offer: Some(offer(retry, switch, focus)),
        ..End::default()
    })
}

pub(super) async fn failed_turn(
    a: &mut Agent,
    turn: u64,
    retry: bool,
    switch: bool,
    focus: RetryFocus,
) {
    a.start("go", turn).await;
    a.emit(offered(ErrorKind::Server, retry, switch, focus))
        .await;
    a.describe().await;
}

async fn failing<D, F>(
    facts: SessionFacts,
    kind: ErrorKind,
    switch: bool,
    detail: Option<ErrorDetail>,
    drive: D,
) where
    D: FnOnce(Agent) -> F,
    F: Future<Output = ()>,
{
    let mut options = options(Start::Stalled);
    options.card_setup = Some(super::endpoints::card_setup());
    retrying_on(facts, options, |mut a| async move {
        a.start("go", 1).await;
        let mut ended = offered(kind, true, switch, RetryFocus::Retry);
        if let AgentEvent::TurnEnded {
            outcome: TurnOutcome::Error { detail: d, .. },
            ..
        } = &mut ended
        {
            *d = detail;
        }
        a.emit(ended).await;
        a.describe().await;
        drive(a).await;
    })
    .await;
}

async fn signs_in(a: &mut Agent, alias: &str) {
    let ClientCommand::SubscriptionAuth {
        command: AuthCommand::Start { request: login, .. },
    } = a.next().await
    else {
        panic!("a sign-in starts");
    };
    let challenge = PrivateChallenge {
        login,
        provider: SubscriptionProvider::Claude,
        alias: alias.into(),
        kind: ChallengeKind::ManualCode,
        authorization_url: Secret::new("https://example.invalid/authorize?state=CANARY"),
        expires_in_secs: 600,
        opened: false,
    };
    let auth = |request, outcome| AgentEvent::SubscriptionAuth {
        event: AuthEvent { request, outcome },
    };
    a.emit(auth(login, AuthOutcome::Challenge { challenge }))
        .await;
    a.paste("CODE_CANARY#STATE_CANARY").key(KeyCode::Enter);
    let ClientCommand::SubscriptionAuth {
        command: AuthCommand::Finish { request, .. },
    } = a.next().await
    else {
        panic!("the pasted code finishes the sign-in");
    };
    let endpoint = SubscriptionEndpoint {
        alias: alias.into(),
        provider: SubscriptionProvider::Claude,
        storage: Some(StoreKind::File),
        is_default: false,
        state: LocalState::Working,
    };
    let previous_cleanup_pending = false;
    a.emit(auth(
        request,
        AuthOutcome::Ready {
            endpoint,
            previous_cleanup_pending,
        },
    ))
    .await;
}

async fn key_saved(a: &mut Agent) {
    let ClientCommand::EditEndpoint { request, name, .. } = a.next().await else {
        panic!("the form sends the key");
    };
    a.emit(AgentEvent::EndpointEdited {
        answers: request,
        name,
        changed: vec![ConnectionField::Key],
        store: Some(StoreKind::Keyring),
        check: CheckResult::of(CheckKind::Skipped),
        notes: Vec::new(),
    })
    .await;
}

async fn no_retry_sent(a: &mut Agent) {
    a.settle().await;
    while let Ok(command) = a.commands.try_recv() {
        assert!(
            !matches!(command, ClientCommand::Retry { .. }),
            "{command:?}"
        );
    }
}

async fn next_command(a: &mut Agent) -> ClientCommand {
    loop {
        match a.next().await {
            ClientCommand::PlanUsage { .. } | ClientCommand::DescribeSettings { .. } => {}
            other => return other,
        }
    }
}

async fn retry_sent(a: &mut Agent) -> Option<String> {
    match next_command(a).await {
        ClientCommand::Retry { endpoint, .. } => endpoint,
        other => panic!("not a retry: {other:?}"),
    }
}

fn retry_of(command: &ClientCommand) -> (u64, Option<String>, RequestId) {
    match command {
        ClientCommand::Retry {
            turn,
            endpoint,
            request,
        } => (*turn, endpoint.clone(), *request),
        other => panic!("not a retry: {other:?}"),
    }
}

const ACCOUNTS: [(&str, Credential); 4] = [
    ("claude-subscription", Credential::AnthropicPlan),
    ("claude-work", Credential::AnthropicPlan),
    ("chatgpt", Credential::ChatGptPlan),
    ("claude-api", Credential::ApiKey),
];

fn listed(names: &[&str]) -> AgentEvent {
    let endpoints = ACCOUNTS
        .iter()
        .filter(|(name, _)| names.contains(name))
        .map(|&(name, credential)| {
            let kind = match credential {
                Credential::ApiKey => "key",
                Credential::ChatGptPlan => "chatgpt_subscription",
                _ => "anthropic_subscription",
            };
            let mut summary = super::subscription_login::account(name, kind, false, false);
            super::scenes::provider_of(credential).clone_into(&mut summary.provider);
            summary
        })
        .collect();
    AgentEvent::EndpointsChanged { endpoints }
}

fn gone(names: &[&str], moved: &[&str], stay: &[&str], ways: &[(&str, bool)]) -> AgentEvent {
    let names_of = |list: &[&str]| list.iter().map(|n| (*n).to_owned()).collect();
    AgentEvent::AccountsGone {
        names: names_of(names),
        moved: moved
            .iter()
            .map(|agent| Followed {
                agent: (*agent).to_owned(),
                model: "test-model".to_owned(),
                effort: Effort::Low,
                rebuild_usd: None,
                from: None,
                to: None,
            })
            .collect(),
        stay: stay
            .iter()
            .map(|agent| Stayed {
                agent: (*agent).to_owned(),
                busy: true,
                ..Stayed::default()
            })
            .collect(),
        ways: ways
            .iter()
            .map(|&(chat, charges)| Way {
                chat: Some(chat.to_owned()),
                charges,
                ..Way::default()
            })
            .collect(),
    }
}

fn gone_refused(id: Option<InputId>) -> AgentEvent {
    AgentEvent::Rejected {
        reason: "claude-subscription was signed out in another window".to_owned(),
        id,
        kind: Some(RejectKind::AccountGone),
        request: None,
    }
}

async fn refused_as_gone(a: &mut Agent, ways: &[(&str, bool)]) {
    a.paste("hello").key(KeyCode::Enter);
    let id = sent_id(&a.next().await);
    a.emit(listed(&["chatgpt", "claude-api"])).await;
    a.emit(gone(
        &["claude-subscription", "claude-work"],
        &[],
        &[],
        ways,
    ))
    .await;
    a.emit(gone_refused(Some(id))).await;
}

async fn logged_out_elsewhere(a: &mut Agent) -> Option<String> {
    let ClientCommand::SubscriptionAuth {
        command:
            AuthCommand::Logout {
                request,
                alias,
                chat,
                gone: true,
                confirmed: true,
                ..
            },
    } = a.next().await
    else {
        panic!("the card answers the removal");
    };
    a.emit(AgentEvent::SubscriptionAuth {
        event: AuthEvent {
            request,
            outcome: super::subscription_login::logged_out(&alias, &[]),
        },
    })
    .await;
    chat
}

fn moved_to(endpoint: &str) -> AgentEvent {
    settings_changed(Changed {
        endpoint: endpoint.to_owned(),
        model: "test-model".to_owned(),
        ..Changed::default()
    })
}

#[tokio::test(start_paused = true)]
async fn enter_on_retry_sends_it_for_the_failed_turn_and_the_choice_shrinks() {
    retrying(|mut a| async move {
        failed_turn(&mut a, 1, true, true, RetryFocus::Retry).await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "› Retry").is_some(), "{lines:#?}");
        a.key(KeyCode::Enter);
        let (turn, endpoint, request) = retry_of(&a.next().await);
        assert_eq!((turn, endpoint), (1, None));
        let lines = a.settle().await;
        assert!(row_with(&lines, "Retried").is_some(), "{lines:#?}");
        assert!(row_with(&lines, "Switch account").is_none(), "{lines:#?}");
        a.emit(AgentEvent::retry_started(2, 1, Some(request))).await;
        let lines = a.settle().await;
        assert!(
            row_with(&lines, "↻ Retrying step 3 on direct").is_some(),
            "{lines:#?}"
        );
    })
    .await;
    let direct = [
        ("direct", Credential::AnthropicPlan),
        ("gw", Credential::ApiKey),
    ];
    failing(
        on_accounts(facts(), &direct),
        ErrorKind::Auth,
        true,
        None,
        |mut a| async move {
            let lines = a.settle().await;
            shows_scrubbed(&lines, "⨯ direct's sign-in expired");
            shows_scrubbed(&lines, "› Sign in again, then retry");
            a.key(KeyCode::Enter);
            signs_in(&mut a, "direct").await;
            assert_eq!(retry_sent(&mut a).await, None);
        },
    )
    .await;
    let keys = [("direct", Credential::ApiKey), ("gw", Credential::ApiKey)];
    failing(
        on_accounts(facts(), &keys),
        ErrorKind::Auth,
        true,
        None,
        |mut a| async move {
            let lines = a.settle().await;
            shows_scrubbed(&lines, "⨯ Claude refused the key for direct · API");
            a.key(KeyCode::Enter);
            let lines = a.settle().await;
            shows_scrubbed(&lines, "Change the key for \"direct\"");
            a.key(KeyCode::Esc).paste("/config").key(KeyCode::Enter);
            let request = loop {
                if let ClientCommand::DescribeSettings { request } = a.next().await {
                    break request;
                }
            };
            let AgentEvent::SettingsSnapshot(mut snapshot) = super::described(request.0) else {
                panic!("a settings snapshot");
            };
            snapshot.endpoints = vec![super::endpoints::summary("direct", true)];
            a.emit(AgentEvent::SettingsSnapshot(snapshot)).await;
            a.settle().await;
            a.key(KeyCode::Char('/'))
                .paste("direct")
                .key(KeyCode::Enter);
            a.settle().await;
            a.key(KeyCode::Right);
            a.settle().await;
            for _ in 0..4 {
                a.key(KeyCode::Down);
            }
            a.key(KeyCode::Enter)
                .paste("CANARY-other-key")
                .key(KeyCode::Enter);
            key_saved(&mut a).await;
            no_retry_sent(&mut a).await;
            a.key(KeyCode::Esc).key(KeyCode::Esc).key(KeyCode::Esc);
            a.settle().await;
            a.key(KeyCode::Enter);
            let lines = a.settle().await;
            shows_scrubbed(&lines, "Change the key for \"direct\"");
            a.paste("CANARY-new-key").key(KeyCode::Enter);
            key_saved(&mut a).await;
            assert_eq!(retry_sent(&mut a).await, None);
        },
    )
    .await;
    for (in_keys_file, line) in [
        (false, "Change WORK_KEY in your shell."),
        (true, "Change WORK_KEY in ~/.config/wisp/.env."),
    ] {
        let mut facts = on_accounts(facts(), &keys);
        facts.endpoints[0].key_var = Some(KeyVariable {
            name: "WORK_KEY".into(),
            in_keys_file,
        });
        failing(facts, ErrorKind::Auth, false, None, |mut a| async move {
            let lines = a.settle().await;
            shows_scrubbed(&lines, line);
            shows_scrubbed(&lines, "› Retry");
            a.key(KeyCode::Enter);
            assert_eq!(retry_sent(&mut a).await, None);
            a.paste("/usage").key(KeyCode::Enter);
            let lines = a.settle().await;
            let fix = line.strip_suffix('.').unwrap_or(line).replacen('C', "c", 1);
            shows_scrubbed(&lines, &format!("Key refused: {fix}"));
            assert!(row_with(&lines, "enter change key").is_none(), "{lines:#?}");
            a.key(KeyCode::Esc);
            let good = Some(wisp_protocol::Usage::default());
            for (on, usage, kept) in [
                ("gw", good, true),
                ("direct", None, true),
                ("direct", good, false),
            ] {
                a.emit(moved_to(on)).await;
                a.emit(AgentEvent::turn_started(2)).await;
                a.emit(AgentEvent::StepEnded {
                    step: 1,
                    usage,
                    cost_usd: None,
                    lower_bound: false,
                })
                .await;
                a.paste("/usage").key(KeyCode::Enter);
                let lines = a.settle().await;
                assert_eq!(
                    row_with(&lines, "Key refused").is_some(),
                    kept,
                    "{lines:#?}"
                );
                a.key(KeyCode::Esc);
            }
        })
        .await;
    }
    let mut saved = on_accounts(facts(), &keys);
    saved.endpoints[1].unavailable = Some("ANTHROPIC_API_KEY isn't set".into());
    failing(saved, ErrorKind::Auth, false, None, |mut a| async move {
        a.settle().await;
        a.paste("/usage").key(KeyCode::Enter);
        super::plan_usage::spend_request(&mut a).await;
        assert!(matches!(a.next().await, ClientCommand::ListAgents { .. }));
        let lines = a.settle().await;
        let refused = row_with(&lines, "Key refused").map(|row| row.trim());
        assert_eq!(refused, Some("Key refused"), "{lines:#?}");
        shows_scrubbed(&lines, "enter change key");
        a.key(KeyCode::Enter);
        let lines = a.settle().await;
        shows_scrubbed(&lines, "Change the key for \"direct\"");
        let account = AccountKind::ClaudeApi.label();
        assert!(row_with(&lines, account).is_none(), "{lines:#?}");
        let lines = a.key(KeyCode::Esc).settle().await;
        assert_eq!(
            row_with(&lines, "Key refused").map(|row| row.trim()),
            refused
        );
        a.key(KeyCode::Enter).settle().await;
        a.paste("CANARY-new-key").key(KeyCode::Enter);
        key_saved(&mut a).await;
        let lines = a.settle().await;
        shows_scrubbed(&lines, account);
        shows_scrubbed(&lines, "enter change key");
        assert!(row_with(&lines, "Key refused").is_none(), "{lines:#?}");
        shows_no_key(&a);
        let lines = a.key(KeyCode::Down).settle().await;
        assert!(row_with(&lines, "enter change key").is_none(), "{lines:#?}");
        a.key(KeyCode::Enter);
        a.nothing_sent().await;
    })
    .await;
    let mut limit = ErrorDetail::new(
        FailurePhase::Response,
        FailureSource::Endpoint,
        RetryClass::Never,
    );
    limit.limit_reached = Some(Box::new(UsageWindow {
        key: "five_hour".into(),
        label: "5-hour".into(),
        duration_seconds: Some(18000),
        scope: UsageScope::All,
        used_percent: None,
        resets_at: Some(4_000_000_000),
        observed_at: 0,
        source: UsageSource::ErrorBody,
    }));
    let claude = on_accounts(facts(), &[("claude", Credential::AnthropicPlan)]);
    failing(
        claude,
        ErrorKind::Billing,
        false,
        Some(limit),
        |mut a| async move {
            let lines = a.settle().await;
            let title = row_with(&lines, "claude reached its usage limit");
            assert!(title.is_some_and(|l| !l.contains("steps")), "{lines:#?}");
            shows_scrubbed(&lines, "› + Sign in to another account…");
            for (login, alias, picks) in [("", "claude", "11"), ("/login work", "work", "1")] {
                a.key(KeyCode::Enter);
                if !login.is_empty() {
                    a.key(KeyCode::Esc).paste(login).key(KeyCode::Enter);
                }
                for pick in picks.chars() {
                    a.settle().await;
                    a.key(KeyCode::Char(pick));
                }
                signs_in(&mut a, alias).await;
                no_retry_sent(&mut a).await;
            }
            let lines = a.key(KeyCode::Esc).settle().await;
            assert!(!lines.iter().any(|l| l.contains("Retry ")
                || l.contains("before the reply")
                || l.contains("interrupted")));
            a.key(KeyCode::Enter);
            a.settle().await;
            a.key(KeyCode::Char('4'));
            a.settle().await;
            a.key(KeyCode::Enter)
                .paste(super::endpoints::KEY)
                .key(KeyCode::Enter)
                .key(KeyCode::Enter);
            let ClientCommand::SaveEndpoint { request, .. } = a.next().await else {
                panic!("the key form saves the new account");
            };
            let mut claude = super::endpoints::summary("claude", true);
            claude.credential = Some(CredentialKind::Other("anthropic_subscription".into()));
            let endpoints = vec![claude, super::endpoints::summary("anthropic", false)];
            a.emit(AgentEvent::EndpointsChanged { endpoints }).await;
            a.emit(super::endpoints::saved(
                request,
                "anthropic",
                StoreKind::Keyring,
            ))
            .await;
            let lines = a.settle().await;
            shows_scrubbed(&lines, "› \u{25cf} anthropic");
            shows_scrubbed(&lines, "enter use the API");
            a.key(KeyCode::Enter);
            assert_eq!(retry_sent(&mut a).await.as_deref(), Some("anthropic"));
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn switch_endpoint_opens_the_retry_picker_and_enter_retries_there() {
    let mut f = two_endpoints();
    f.endpoints[1].provider = "openai".into();
    f.endpoints[1].models = vec![wisp_protocol::settings::AccountModel {
        id: "gpt-6.1-sol".into(),
        listed: Some(0),
    }];
    retrying_on(f, options(Start::Stalled), |mut a| async move {
        failed_turn(&mut a, 1, true, true, RetryFocus::Retry).await;
        a.key(KeyCode::Down).key(KeyCode::Enter);
        a.describe().await;
        let lines = a.settle().await;
        let title = position(&lines, "Retry on");
        assert!(
            lines[title + 1].contains("gw  Gateway \u{b7} API"),
            "{lines:#?}"
        );
        assert!(lines[title + 1].contains("gpt-6.1-sol low"), "{lines:#?}");
        assert!(!lines[title + 1].contains("test-model"), "{lines:#?}");
        assert!(!lines[title + 2].contains("direct"), "{lines:#?}");
        a.key(KeyCode::Esc);
        let lines = a.settle().await;
        assert!(row_with(&lines, "Retry on").is_none(), "{lines:#?}");
        assert!(row_with(&lines, "› Switch account").is_some(), "{lines:#?}");
        a.key(KeyCode::Enter);
        a.describe().await;
        a.key(KeyCode::Enter);
        let (turn, endpoint, _) = retry_of(&a.next().await);
        assert_eq!((turn, endpoint.as_deref()), (1, Some("gw")));
        let lines = a.settle().await;
        assert!(row_with(&lines, "Retried on gw").is_some(), "{lines:#?}");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn typing_gives_the_composer_the_keys_and_erasing_gives_them_back() {
    retrying(|mut a| async move {
        failed_turn(&mut a, 1, true, true, RetryFocus::Retry).await;
        a.paste("x").key(KeyCode::Down);
        a.nothing_sent().await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "› Retry").is_some(), "{lines:#?}");
        assert!(row_with(&lines, "esc dismiss").is_none(), "{lines:#?}");
        a.key(KeyCode::Backspace)
            .key(KeyCode::Down)
            .key(KeyCode::Up);
        a.key(KeyCode::Enter);
        let (turn, endpoint, _) = retry_of(&a.next().await);
        assert_eq!((turn, endpoint), (1, None));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn enter_on_typed_text_sends_a_message_and_the_offer_lapses() {
    retrying(|mut a| async move {
        failed_turn(&mut a, 1, true, true, RetryFocus::Retry).await;
        a.paste("continue").key(KeyCode::Enter);
        let command = a.next().await;
        let ClientCommand::Send { text, mode, .. } = &command else {
            panic!("{command:?}");
        };
        assert_eq!((text.as_str(), *mode), ("continue", DeliveryMode::Steer));
        a.emit(AgentEvent::turn_started(2)).await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "Retry").is_none(), "{lines:#?}");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn with_held_messages_enter_acts_on_the_choice_and_after_esc_releases_them() {
    retrying(|mut a| async move {
        failed_turn(&mut a, 1, true, true, RetryFocus::Retry).await;
        a.emit(AgentEvent::InputAccepted {
            opening: false,
            front: false,
            id: InputId("tui-9".to_owned()),
            mode: DeliveryMode::Queue,
            text: "and then this".to_owned(),
            images: Vec::new(),
            replaces: None,
            held: true,
            from: None,
            checklist_spans: Vec::new(),
        })
        .await;
        a.key(KeyCode::Esc);
        a.settle().await;
        a.key(KeyCode::Enter);
        assert_eq!(a.next().await, ClientCommand::Release);
    })
    .await;
    retrying(|mut a| async move {
        failed_turn(&mut a, 1, true, true, RetryFocus::Retry).await;
        a.emit(AgentEvent::InputAccepted {
            opening: false,
            front: false,
            id: InputId("tui-9".to_owned()),
            mode: DeliveryMode::Queue,
            text: "and then this".to_owned(),
            images: Vec::new(),
            replaces: None,
            held: true,
            from: None,
            checklist_spans: Vec::new(),
        })
        .await;
        a.key(KeyCode::Enter);
        retry_of(&a.next().await);
    })
    .await;
    let accounts = || on_accounts(facts(), &ACCOUNTS);
    retrying_on(accounts(), options(Start::Stalled), |mut a| async move {
        a.paste("hello").key(KeyCode::Enter);
        let id = sent_id(&a.next().await);
        a.emit(listed(&["claude-work", "chatgpt", "claude-api"]))
            .await;
        let ways = [("chatgpt", false), ("claude-api", true)];
        a.emit(gone(&["claude-subscription"], &[], &[], &ways))
            .await;
        a.emit(listed(&["chatgpt", "claude-api"])).await;
        a.emit(gone(&["claude-work"], &[], &[], &[])).await;
        a.emit(gone_refused(Some(id))).await;
        let lines = a.settle().await;
        let title = position(
            &lines,
            "claude-subscription was signed out in another window",
        );
        let chatgpt = position(&lines, "1 \u{25cf} chatgpt");
        let api = position(&lines, "2 \u{25cf} claude-api");
        assert!(title < chatgpt && chatgpt < api, "{lines:#?}");
        assert!(
            lines[chatgpt..api]
                .iter()
                .any(|l| l.contains("Earlier reasoning won't carry over.")),
            "{lines:#?}"
        );
        assert!(row_with(&lines, "Your message wasn't sent.").is_some());
        assert!(row_with(&lines, "This message wasn't sent").is_none());
        a.key(KeyCode::Char('2'));
        a.nothing_sent().await;
        a.key(KeyCode::Up).key(KeyCode::Enter);
        assert_eq!(
            logged_out_elsewhere(&mut a).await.as_deref(),
            Some("chatgpt")
        );
        let ClientCommand::Send { text, .. } = next_command(&mut a).await else {
            panic!("the message goes once the chat moves");
        };
        assert_eq!(text, "hello");
        a.emit(moved_to("chatgpt")).await;
        a.emit(gone(&["claude-subscription"], &["late"], &[], &[]))
            .await;
        let lines = a.settle().await;
        let reasons = lines
            .iter()
            .filter(|l| l.contains("subscription was signed out"));
        assert_eq!(reasons.count(), 1, "{lines:#?}");
    })
    .await;
    retrying_on(accounts(), options(Start::Stalled), |mut a| async move {
        a.paste("hello").key(KeyCode::Enter);
        let id = sent_id(&a.next().await);
        a.emit(listed(&["chatgpt", "claude-api"])).await;
        let late = super::agents::on_account("/late", "claude-subscription", 40_000);
        a.emit(super::agents::spawned(late)).await;
        a.emit(AgentEvent::StepEnded {
            step: 1,
            usage: Some(wisp_protocol::Usage {
                cache_read_input_tokens: 20_000,
                ..wisp_protocol::Usage::default()
            }),
            cost_usd: None,
            lower_bound: false,
        })
        .await;
        let mut card = gone(
            &["claude-subscription"],
            &[],
            &[],
            &[("chatgpt", false), ("claude-api", true)],
        );
        if let AgentEvent::AccountsGone { ways, .. } = &mut card {
            ways[1].follow = vec!["late".to_owned()];
        }
        a.emit(card).await;
        a.emit(gone_refused(Some(id))).await;
        let lines = a.settle().await;
        let api = position(&lines, "2 \u{25cf} claude-api");
        let priced = lines[api..].iter().find(|l| l.contains("cache rebuild ~$"));
        let figure = priced.and_then(|l| l.split_whitespace().last());
        let figure = figure.unwrap_or_default().to_owned();
        assert!(priced.is_some_and(|l| l.contains(" and ~$")), "{lines:#?}");
        a.key(KeyCode::Down).key(KeyCode::Char('2'));
        a.nothing_sent().await;
        a.key(KeyCode::Enter);
        assert_eq!(
            logged_out_elsewhere(&mut a).await.as_deref(),
            Some("claude-api")
        );
        a.emit(moved_to("claude-api")).await;
        a.emit(gone(&["claude-subscription"], &["late"], &[], &[]))
            .await;
        let lines = a.settle().await;
        let late = row_with(&lines, "late switched to");
        assert!(
            late.is_some_and(|l| l.trim_end().ends_with(&figure)),
            "{figure}\n{lines:#?}"
        );
    })
    .await;
    retrying_on(accounts(), options(Start::Stalled), |mut a| async move {
        refused_as_gone(&mut a, &[("chatgpt", false), ("claude-api", true)]).await;
        a.settle().await;
        a.key(KeyCode::Esc);
        a.nothing_sent().await;
        a.key(KeyCode::Enter);
        let id = sent_id(&a.next().await);
        a.emit(AgentEvent::Rejected {
            reason: "the gateway is down".to_owned(),
            id: Some(id),
            kind: None,
            request: None,
        })
        .await;
        let lines = a.settle().await;
        assert!(
            row_with(&lines, "This message wasn't sent: the gateway is down.").is_some(),
            "{lines:#?}"
        );
    })
    .await;
    for (queued, key) in [
        (false, Some(KeyCode::Enter)),
        (true, Some(KeyCode::Char('1'))),
        (true, Some(KeyCode::Esc)),
        (true, None),
    ] {
        retrying_on(
            accounts(),
            options(Start::Stalled),
            move |mut a| async move {
                let id = if queued {
                    None
                } else {
                    a.paste("hello").key(KeyCode::Enter);
                    Some(sent_id(&a.next().await))
                };
                a.emit(listed(&[])).await;
                a.emit(gone(&["claude-subscription"], &[], &[], &[])).await;
                a.emit(gone_refused(id)).await;
                let lines = a.settle().await;
                assert!(
                    row_with(
                        &lines,
                        "claude-subscription was signed out in another window."
                    )
                    .is_some(),
                    "{lines:#?}"
                );
                a.key(KeyCode::Char('1'));
                a.settle().await;
                a.key(KeyCode::Enter);
                signs_in(&mut a, "claude").await;
                let resent = match next_command(&mut a).await {
                    ClientCommand::Send { id, text, .. } if !queued && text == "hello" => Some(id),
                    ClientCommand::Release if queued => None,
                    other => panic!("{other:?}"),
                };
                let claude = super::subscription_login::account(
                    "claude",
                    "anthropic_subscription",
                    false,
                    false,
                );
                a.emit(AgentEvent::EndpointsChanged {
                    endpoints: vec![claude],
                })
                .await;
                let Some(key) = key else {
                    a.emit(moved_to("claude")).await;
                    a.emit(gone(&["claude-subscription"], &[], &[], &[])).await;
                    let lines = a.settle().await;
                    assert!(row_with(&lines, "for this chat").is_none(), "{lines:#?}");
                    a.nothing_sent().await;
                    return;
                };
                a.emit(gone(
                    &["claude-subscription"],
                    &[],
                    &[],
                    &[("claude", false)],
                ))
                .await;
                a.emit(gone_refused(resent)).await;
                let asks = "Where should this chat go?";
                let lines = a.settle().await;
                assert!(
                    row_with(&lines, "1 Use \u{25cf} claude for this chat").is_some()
                        && row_with(&lines, asks).is_none(),
                    "{lines:#?}"
                );
                a.key(key);
                if key == KeyCode::Esc {
                    a.nothing_sent().await;
                    let lines = a.settle().await;
                    assert!(row_with(&lines, asks).is_some(), "{lines:#?}");
                    return;
                }
                let ClientCommand::ChangeSettings { changes, .. } = next_command(&mut a).await
                else {
                    panic!("Use moves the chat");
                };
                assert!(format!("{changes:?}").contains("\"claude\""), "{changes:?}");
                if !queued {
                    let ClientCommand::Send { text, .. } = next_command(&mut a).await else {
                        panic!("the refused message goes after the move");
                    };
                    assert_eq!(text, "hello");
                }
                a.nothing_sent().await;
                let lines = a.settle().await;
                assert!(row_with(&lines, asks).is_none(), "{lines:#?}");
            },
        )
        .await;
    }
    let mut keyed = options(Start::Stalled);
    keyed.card_setup = Some(super::endpoints::card_setup());
    retrying_on(accounts(), keyed, |mut a| async move {
        let ids = ["first", "second"].map(|id| InputId(id.to_owned()));
        for id in &ids {
            a.emit(AgentEvent::InputAccepted {
                opening: false,
                front: false,
                id: id.clone(),
                mode: DeliveryMode::Queue,
                text: id.0.clone(),
                images: Vec::new(),
                replaces: None,
                held: true,
                from: None,
                checklist_spans: Vec::new(),
            })
            .await;
        }
        a.emit(listed(&[])).await;
        a.emit(gone(&["claude-subscription"], &[], &[], &[])).await;
        a.emit(gone_refused(None)).await;
        a.settle().await;
        a.key(KeyCode::Char('4'));
        a.settle().await;
        a.key(KeyCode::Enter)
            .paste(super::endpoints::KEY)
            .key(KeyCode::Enter)
            .key(KeyCode::Enter);
        let ClientCommand::SaveEndpoint { request, .. } = a.next().await else {
            panic!("the key form saves the new account");
        };
        let endpoints = vec![super::endpoints::summary("anthropic", false)];
        a.emit(AgentEvent::EndpointsChanged { endpoints }).await;
        a.emit(super::endpoints::saved(
            request,
            "anthropic",
            StoreKind::Keyring,
        ))
        .await;
        assert_eq!(next_command(&mut a).await, ClientCommand::Release);
        a.emit(gone(
            &["claude-subscription"],
            &[],
            &[],
            &[("anthropic", true)],
        ))
        .await;
        a.emit(gone_refused(None)).await;
        let lines = a.settle().await;
        let row = row_with(&lines, "1 \u{25cf} anthropic");
        assert!(row.is_some_and(|l| !l.contains('›')), "{lines:#?}");
        for key in [KeyCode::Enter, KeyCode::Char('1')] {
            a.key(key);
            a.nothing_sent().await;
        }
        shows_scrubbed(&a.settle().await, "enter use the API");
        a.key(KeyCode::Enter);
        assert_eq!(
            logged_out_elsewhere(&mut a).await.as_deref(),
            Some("anthropic")
        );
        a.emit(AgentEvent::InputUnheld {
            ids: ids.to_vec(),
            front: false,
        })
        .await;
        a.nothing_sent().await;
        let lines = a.settle().await;
        assert!(
            position(&lines, "first") < position(&lines, "second"),
            "{lines:#?}"
        );
    })
    .await;
    retrying_on(accounts(), options(Start::Stalled), |a| async move {
        a.emit(listed(&["claude-work", "chatgpt", "claude-api"]))
            .await;
        a.emit(moved_to("claude-work")).await;
        a.emit(gone(&["claude-subscription"], &["coder"], &["late"], &[]))
            .await;
        a.emit(listed(&["claude-work", "claude-api"])).await;
        a.emit(gone(&["chatgpt"], &[], &[], &[])).await;
        a.emit(gone(&["claude-subscription"], &["late"], &[], &[]))
            .await;
        let lines = a.settle().await;
        let continuing = position(&lines, "Continuing on");
        let reason = position(
            &lines,
            "claude-subscription was signed out in another window.",
        );
        let coder = position(&lines, "coder switched to");
        let working = position(&lines, "late is working:");
        let other = position(&lines, "chatgpt was signed out in another window.");
        let late = position(&lines, "late switched to");
        assert!(
            continuing < reason && reason < coder && coder < working,
            "{lines:#?}"
        );
        assert!(working < other && other < late, "{lines:#?}");
        let reasons = lines
            .iter()
            .filter(|l| l.contains("claude-subscription was signed out"))
            .count();
        assert_eq!(reasons, 1, "{lines:#?}");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_refused_retry_is_said_and_the_choice_goes() {
    retrying(|mut a| async move {
        failed_turn(&mut a, 1, true, true, RetryFocus::Retry).await;
        a.key(KeyCode::Enter);
        let (_, _, request) = retry_of(&a.next().await);
        a.emit(AgentEvent::Rejected {
            reason: "turn 1 is no longer the last turn".to_owned(),
            id: None,
            kind: Some(RejectKind::NotRetryable),
            request: Some(request),
        })
        .await;
        let lines = a.settle().await;
        assert!(
            row_with(&lines, "Couldn't retry: turn 1 is no longer the last turn.").is_some(),
            "{lines:#?}"
        );
        assert!(row_with(&lines, "Retried").is_none(), "{lines:#?}");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_resent_message_is_tagged_retried() {
    retrying(|mut a| async move {
        a.start("what is this", 1).await;
        a.emit(turn_ended(End {
            ending: Ending::Error(ErrorKind::Overloaded),
            message: "Overloaded".to_owned(),
            offer: Some(offer(true, true, RetryFocus::Retry)),
            ..End::default()
        }))
        .await;
        a.describe().await;
        a.key(KeyCode::Enter);
        let (_, _, request) = retry_of(&a.next().await);
        let resent = InputId("wisp-retry-2-1".to_owned());
        a.emit(AgentEvent::InputAccepted {
            opening: false,
            front: false,
            id: resent.clone(),
            mode: DeliveryMode::Steer,
            text: "what is this".to_owned(),
            images: Vec::new(),
            replaces: None,
            held: false,
            from: None,
            checklist_spans: Vec::new(),
        })
        .await;
        a.emit(AgentEvent::retry_started(2, 1, Some(request))).await;
        a.emit(AgentEvent::InputDelivered {
            ids: vec![resent],
            step: 1,
        })
        .await;
        let lines = a.settle().await;
        assert!(
            lines
                .iter()
                .any(|l| l.contains("› what is this") && l.ends_with("retried")),
            "{lines:#?}"
        );
        assert!(
            row_with(&lines, "↻ Retrying your message on direct").is_some(),
            "{lines:#?}"
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn no_choice_after_a_stop_or_on_an_earlier_failure_and_no_slash_retry() {
    retrying(|mut a| async move {
        a.start("go", 1).await;
        a.emit(super::steering::cancelled()).await;
        a.nothing_sent().await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "Retry").is_none(), "{lines:#?}");
        failed_turn(&mut a, 2, true, true, RetryFocus::Retry).await;
        a.start("next", 3).await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "› Retry").is_none(), "{lines:#?}");
        a.emit(super::completed()).await;
        a.paste("/retry").key(KeyCode::Enter);
        a.nothing_sent().await;
        let lines = a.settle().await;
        assert!(
            row_with(&lines, "/retry isn't a command").is_some(),
            "{lines:#?}"
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_limit_cell_says_wisp_retries_at_the_reset_and_keeps_it_after_esc() {
    let mut ended = offered(ErrorKind::RateLimit, true, true, RetryFocus::Switch);
    if let AgentEvent::TurnEnded {
        outcome:
            TurnOutcome::Error {
                detail,
                offer: Some(offer),
                ..
            },
        ..
    } = &mut ended
    {
        let mut limit = ErrorDetail::new(
            FailurePhase::Response,
            FailureSource::Endpoint,
            RetryClass::Never,
        );
        limit.limit_reached = Some(Box::new(UsageWindow {
            key: "subscription-limit".into(),
            label: "Subscription limit".into(),
            duration_seconds: None,
            scope: UsageScope::All,
            used_percent: None,
            resets_at: Some(4_000_000_000),
            observed_at: 0,
            source: UsageSource::RetryAfter,
        }));
        *detail = Some(limit);
        offer.retries_at = Some(4_000_000_060);
    }
    retrying(|mut a| async move {
        a.start("go", 1).await;
        a.emit(ended).await;
        a.describe().await;
        let lines = a.settle().await;
        shows_scrubbed(&lines, "Wisp retries by itself when it resets.");
        assert!(row_with(&lines, "Switch account").is_some(), "{lines:#?}");
        let lines = a.key(KeyCode::Esc).settle().await;
        assert!(row_with(&lines, "Switch account").is_none(), "{lines:#?}");
        shows_scrubbed(&lines, "Wisp retries by itself when it resets.");
        a.emit(moved_to("gw")).await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "Wisp retries").is_none(), "{lines:#?}");
        a.emit(moved_to("direct")).await;
        shows_scrubbed(&a.settle().await, "Wisp retries by itself when it resets.");
        a.emit(AgentEvent::retry_started(2, 1, None)).await;
        let lines = a.settle().await;
        assert!(
            row_with(&lines, "\u{21bb} Retrying step 3 on direct").is_some(),
            "{lines:#?}"
        );
        assert!(row_with(&lines, "Wisp retries").is_none(), "{lines:#?}");
        shows_scrubbed(&lines, "Retried");
        a.nothing_sent().await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn an_open_endpoint_card_takes_the_keys_before_the_choice() {
    let mut with_card = options(Start::Stalled);
    with_card.card_setup = Some(super::endpoints::card_setup());
    retrying_with(with_card, |mut a| async move {
        failed_turn(&mut a, 1, true, true, RetryFocus::Retry).await;
        a.paste("/endpoint add").key(KeyCode::Enter);
        let lines = a.settle().await;
        assert!(row_with(&lines, words::ADD_TITLE).is_some(), "{lines:#?}");
        assert!(row_with(&lines, "esc dismiss").is_none(), "{lines:#?}");
        a.key(KeyCode::Down).key(KeyCode::Enter);
        a.nothing_sent().await;
        a.key(KeyCode::Esc);
        let lines = a.settle().await;
        assert!(row_with(&lines, words::ADD_TITLE).is_none(), "{lines:#?}");
        assert!(row_with(&lines, "› Retry").is_some(), "{lines:#?}");
        a.key(KeyCode::Enter);
        let (turn, endpoint, _) = retry_of(&a.next().await);
        assert_eq!((turn, endpoint), (1, None));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn enter_after_sending_a_message_and_before_its_turn_starts_leaves_the_choice_alone() {
    retrying(|mut a| async move {
        failed_turn(&mut a, 1, true, true, RetryFocus::Retry).await;
        a.paste("continue").key(KeyCode::Enter);
        let command = a.next().await;
        assert!(
            matches!(&command, ClientCommand::Send { text, .. } if text == "continue"),
            "{command:?}"
        );
        a.key(KeyCode::Enter).key(KeyCode::Down).key(KeyCode::Enter);
        a.nothing_sent().await;
        a.emit(AgentEvent::turn_started(2)).await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "› Retry").is_none(), "{lines:#?}");
    })
    .await;
}

const KEY_MODEL: &str = "sk-ant-api03-CANARYSESSION0001";

fn echoing(kind: ErrorKind, message: String) -> AgentEvent {
    turn_ended(End {
        ending: Ending::Error(kind),
        message,
        attempts: 1,
        ..End::default()
    })
}

fn shows_scrubbed(lines: &[String], phrase: &str) {
    let screen = lines.join(" ");
    let squeezed = screen.split_whitespace().collect::<Vec<_>>().join(" ");
    let wanted = phrase.replace("{}", NOT_SHOWN);
    assert!(squeezed.contains(&wanted), "{wanted:?} in {lines:#?}");
}

fn shows_no_key(a: &Agent) {
    let seen = a.screen.seen().join("\n");
    assert!(!seen.to_ascii_lowercase().contains("canary"), "{seen}");
    let main = a.screen.main_before().join("\n");
    assert!(!main.to_ascii_lowercase().contains("canary"), "{main}");
}

#[tokio::test(start_paused = true)]
async fn provider_text_that_echoes_a_key_shaped_model_shows_the_placeholder() {
    let facts = SessionFacts {
        model: KEY_MODEL.to_owned(),
        ..facts()
    };
    retrying_on(facts, options(Start::Stalled), |mut a| async move {
        a.start("go", 1).await;
        a.emit(AgentEvent::Retrying {
            attempt: 1,
            delay_ms: 2_000,
            reason: format!("model {KEY_MODEL} is overloaded"),
            step: 1,
            kind: ErrorKind::Overloaded,
            detail: None,
            max_attempts: 5,
            discarded_blocks: 0,
            discarded_chars: 0,
            usage: None,
            model: None,
            cost_usd: None,
            lower_bound: false,
            resend: None,
            busy: None,
        })
        .await;
        shows_scrubbed(&a.settle().await, "model {} is overloaded");
        a.emit(echoing(
            ErrorKind::InvalidRequest,
            format!("no such model: {}", KEY_MODEL.to_ascii_lowercase()),
        ))
        .await;
        shows_scrubbed(&a.settle().await, "no such model: {}");
        a.emit(AgentEvent::CompactionFailed {
            trigger: CompactionTrigger::Manual,
            engine: None,
            kind: CompactionFailure::Other("model_missing".to_owned()),
            message: format!("compaction model {KEY_MODEL} not found"),
            usage: None,
            cost: None,
            answers: None,
            turn: None,
            paused: false,
            requested_by: None,
        })
        .await;
        shows_scrubbed(&a.settle().await, "compaction model {} not found");
        shows_no_key(&a);
    })
    .await;
}

async fn closed_at_resume(a: &mut Agent) {
    a.emit(AgentEvent::SessionResumed {
        turns: 4,
        moved: Vec::new(),
        stay: Vec::new(),
    })
    .await;
    a.emit(offered(
        ErrorKind::Interrupted,
        true,
        false,
        RetryFocus::Retry,
    ))
    .await;
    a.describe().await;
    let lines = a.settle().await;
    assert!(
        row_with(&lines, "stream was interrupted").is_some(),
        "{lines:#?}"
    );
    assert!(row_with(&lines, "› Retry").is_some(), "{lines:#?}");
}

#[tokio::test(start_paused = true)]
async fn retry_on_a_turn_closed_at_resume_names_that_turn() {
    retrying(|mut a| async move {
        closed_at_resume(&mut a).await;
        a.key(KeyCode::Enter);
        let (turn, endpoint, _) = retry_of(&a.next().await);
        assert_eq!((turn, endpoint), (4, None));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_turn_closed_at_resume_shows_its_failure_and_the_next_message_runs() {
    retrying(|mut a| async move {
        closed_at_resume(&mut a).await;
        a.start("go on", 5).await;
        a.emit(turn_ended(End {
            ending: Ending::Cancelled,
            ..End::default()
        }))
        .await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "working on it").is_some(), "{lines:#?}");
        assert!(row_with(&lines, "› Retry").is_none(), "{lines:#?}");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_rewind_takes_the_retry_choice_away_so_enter_sends_the_message() {
    retrying(|mut a| async move {
        let first = a.start("first", 1).await;
        a.emit(super::completed()).await;
        failed_turn(&mut a, 2, true, true, RetryFocus::Retry).await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "› Retry").is_some(), "{lines:#?}");
        a.paste("/rewind").key(KeyCode::Enter);
        let request = match a.next().await {
            ClientCommand::RewindPoints { request } => request,
            other => panic!("expected RewindPoints, got {other:?}"),
        };
        a.emit(AgentEvent::RewindPoints {
            answers: request,
            points: vec![
                super::rewind::point(1, "first"),
                super::rewind::point(2, "go"),
            ],
        })
        .await;
        a.key(KeyCode::Enter).key(KeyCode::Enter);
        let request = match a.next().await {
            ClientCommand::Rewind { request, .. } => request,
            other => panic!("expected Rewind, got {other:?}"),
        };
        a.emit(super::rewind::rewound(
            request,
            1,
            vec![super::rewind::returned(&first, "first")],
        ))
        .await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "› Retry").is_none(), "{lines:#?}");
        assert!(a.screen.caret_line().contains("first"), "{lines:#?}");
        a.key(KeyCode::Enter);
        match a.next().await {
            ClientCommand::Send { text, .. } => assert_eq!(text, "first"),
            other => panic!("expected Send, got {other:?}"),
        }
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn right_click_pastes_into_the_composer_while_the_retry_choice_is_open() {
    let dir = super::images::Dir::new("right-retry");
    dir.put("text", b"pasted words");
    let options = super::images::right_pasting(&dir, true);
    retrying_with(options, |mut a| async move {
        failed_turn(&mut a, 1, true, true, RetryFocus::Retry).await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "› Retry").is_some(), "{lines:#?}");
        a.inputs.send(super::images::right_click()).unwrap();
        eventually(|| a.screen.caret_line().contains("pasted words")).await;
        assert!(
            a.screen.caret_line().contains("pasted words"),
            "{:#?}",
            a.screen.visible()
        );
        a.nothing_sent().await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn right_click_does_nothing_while_the_retry_endpoint_picker_is_open() {
    let dir = super::images::Dir::new("right-retry-pick");
    dir.put("text", b"pasted words");
    let options = super::images::right_pasting(&dir, true);
    retrying_with(options, |mut a| async move {
        failed_turn(&mut a, 1, true, true, RetryFocus::Retry).await;
        a.key(KeyCode::Down).key(KeyCode::Enter);
        a.describe().await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "Retry on").is_some(), "{lines:#?}");
        a.inputs.send(super::images::right_click()).unwrap();
        real_wait(300).await;
        assert!(super::images::calls(&dir).is_empty());
        a.key(KeyCode::Esc);
        let lines = a.settle().await;
        assert!(row_with(&lines, "Retry on").is_none(), "{lines:#?}");
        dir.put("held", b"");
        a.inputs.send(super::images::right_click()).unwrap();
        a.key(KeyCode::Enter);
        a.describe().await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "Retry on").is_some(), "{lines:#?}");
        super::images::land_held_read(&dir, &a.screen).await;
        assert_eq!(
            super::images::calls(&dir),
            vec!["--no-newline --type text".to_owned()]
        );
        let lines = a.settle().await;
        assert!(row_with(&lines, "Retry on").is_some(), "{lines:#?}");
        a.key(KeyCode::Esc);
        let lines = a.settle().await;
        assert!(
            !lines.iter().any(|l| l.contains("pasted words")),
            "{lines:#?}"
        );
        a.nothing_sent().await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_paste_under_the_retry_endpoint_picker_never_reaches_the_composer() {
    let (dir, options) = super::images::pasting("under-retry-pick");
    let facts = super::images::seeing(two_endpoints());
    retrying_on(facts, options, |mut a| async move {
        failed_turn(&mut a, 1, true, true, RetryFocus::Retry).await;
        a.key(KeyCode::Down).key(KeyCode::Enter);
        a.describe().await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "Retry on").is_some(), "{lines:#?}");
        super::images::paste_every_way(&a.inputs).await;
        a.key(KeyCode::Esc);
        let lines = a.settle().await;
        assert!(row_with(&lines, "Retry on").is_none(), "{lines:#?}");
        assert_eq!(
            super::images::leaks(&a.screen, &dir),
            super::images::NO_LEAKS
        );
        a.nothing_sent().await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_paste_into_the_model_or_account_picker_lands_in_its_filter_only() {
    for command in ["/model", "/account"] {
        retrying(|mut a| async move {
            a.paste(command).key(KeyCode::Enter);
            a.describe().await;
            a.paste("gw");
            let lines = a.settle().await;
            assert!(
                lines.iter().any(|l| l.contains("matching \"gw\"")),
                "{command}: {lines:#?}"
            );
            a.key(KeyCode::Esc);
            a.settle().await;
            a.key(KeyCode::Enter);
            a.nothing_sent().await;
        })
        .await;
    }
}

fn resized(a: &Agent, rows: u16) {
    let size = a.screen.resize(rows, 80);
    a.inputs.send(Input::Resize(size)).unwrap();
}

#[tokio::test(start_paused = true)]
async fn below_three_rows_the_retry_on_picker_closes_and_will_not_open() {
    retrying(|mut a| async move {
        failed_turn(&mut a, 1, true, true, RetryFocus::Retry).await;
        a.key(KeyCode::Down).key(KeyCode::Enter);
        a.describe().await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "Retry on").is_some(), "{lines:#?}");
        resized(&a, 3);
        sleep(Duration::from_secs(1)).await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "Retry on").is_none(), "{lines:#?}");
        a.key(KeyCode::Enter);
        sleep(Duration::from_millis(100)).await;
        a.nothing_sent().await;
        let lines = a.settle().await;
        assert!(
            super::status_line(&lines).contains(wisp_render::transcript::copy::PICKER_NO_ROOM),
            "{lines:#?}"
        );
        resized(&a, 30);
        sleep(Duration::from_secs(1)).await;
        a.key(KeyCode::Enter);
        a.describe().await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "Retry on").is_some(), "{lines:#?}");
        a.key(KeyCode::Esc).key(KeyCode::Esc);
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_retry_on_dock_hides_held_messages_behind_a_count_and_sits_on_the_composer() {
    retrying(|mut a| async move {
        a.start("go", 1).await;
        a.paste("next one").key(KeyCode::Tab);
        let queued = a.next().await;
        let queued = a.accept(&queued).await;
        a.emit(AgentEvent::InputHeld {
            ids: vec![queued],
            by: WithdrawnBy::TurnFailed,
        })
        .await;
        a.emit(offered(ErrorKind::Server, true, true, RetryFocus::Switch))
            .await;
        a.describe().await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "next one").is_some(), "{lines:#?}");
        a.key(KeyCode::Enter);
        a.describe().await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "Retry on").is_some(), "{lines:#?}");
        assert!(row_with(&lines, "next one").is_none(), "{lines:#?}");
        let top = lines
            .iter()
            .rposition(|l| l.starts_with('\u{256D}'))
            .unwrap();
        assert!(lines[top - 1].contains("esc back"), "{lines:#?}");
        let status = super::status_line(&lines);
        assert!(status.contains("1 waiting"), "{status}");
        assert!(!status.contains("enter send held"), "{status}");
        a.key(KeyCode::Esc);
        let lines = a.settle().await;
        assert!(row_with(&lines, "next one").is_some(), "{lines:#?}");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_click_in_the_box_while_the_retry_on_dock_is_open_leaves_the_input_to_esc() {
    retrying(|mut a| async move {
        failed_turn(&mut a, 1, true, true, RetryFocus::Retry).await;
        a.key(KeyCode::Down).key(KeyCode::Enter);
        a.describe().await;
        a.paste("hello world");
        let lines = a.settle().await;
        assert!(row_with(&lines, "Retry on").is_some(), "{lines:#?}");
        let top = lines
            .iter()
            .rposition(|l| l.starts_with('\u{256D}'))
            .unwrap();
        assert!(!lines[top + 1].contains("hello world"), "{lines:#?}");
        let row = u16::try_from(top + 1).unwrap();
        for kind in [MouseKind::Press, MouseKind::Release] {
            a.inputs
                .send(Input::Mouse(Mouse {
                    kind,
                    column: 4 + 5,
                    row,
                    modified: false,
                }))
                .unwrap();
        }
        let lines = a.settle().await;
        assert!(row_with(&lines, "Retry on").is_some(), "{lines:#?}");
        a.key(KeyCode::Esc);
        a.paste("^");
        let lines = a.settle().await;
        assert!(row_with(&lines, "Retry on").is_none(), "{lines:#?}");
        assert!(a.screen.caret_line().contains('^'), "{lines:#?}");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn every_height_from_5_to_20_cues_hidden_endpoints_and_shows_the_selection() {
    retrying(|mut a| async move {
        failed_turn(&mut a, 1, true, true, RetryFocus::Retry).await;
        a.key(KeyCode::Down).key(KeyCode::Enter);
        a.describe().await;
        a.settle().await;
        for height in (5..=20u16).rev() {
            resized(&a, height);
            sleep(Duration::from_secs(1)).await;
            for _ in 0..2 {
                let lines = a.settle().await;
                let title = position(&lines, "   Retry on which account?");
                let top = super::frame_top(&lines);
                let dock = &lines[title..top];
                let shown = ["gw"]
                    .iter()
                    .filter(|name| {
                        dock.iter()
                            .skip(1)
                            .any(|r| r.contains(&format!(" {name} ")))
                    })
                    .count();
                assert!(
                    super::docked::cued(dock, shown, 1),
                    "{height} rows: {lines:#?}"
                );
                assert!(
                    dock.iter().any(|r| r.starts_with(" \u{203A} ")),
                    "{height} rows: {lines:#?}"
                );
                a.key(KeyCode::Down);
            }
        }
        a.key(KeyCode::Esc).key(KeyCode::Esc);
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn with_the_retry_choice_open_ctrl_o_and_enter_open_the_selection_and_send_no_retry() {
    retrying(|mut a| async move {
        a.start("go", 1).await;
        a.emit(AgentEvent::BlockStarted {
            index: 1,
            kind: wisp_protocol::BlockKind::Thinking,
        })
        .await;
        a.emit(AgentEvent::ThinkingDelta {
            index: 1,
            text: "Placeholder thought one.\nPlaceholder two.\n".to_owned(),
        })
        .await;
        a.emit(AgentEvent::BlockStopped { index: 1 }).await;
        a.emit(offered(ErrorKind::Server, true, true, RetryFocus::Retry))
            .await;
        a.describe().await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "› Retry").is_some(), "{lines:#?}");
        a.ctrl('o');
        let lines = a.settle().await;
        assert!(
            lines
                .iter()
                .any(|l| l.contains(wisp_render::transcript::copy::EVERY_CELL)),
            "{lines:#?}"
        );
        a.key(KeyCode::Down).key(KeyCode::Enter);
        a.nothing_sent().await;
        let lines = a.settle().await;
        assert!(
            lines
                .last()
                .unwrap()
                .contains(&wisp_tui::app::pager::back_keys().text()),
            "{lines:#?}"
        );
        assert!(lines[0].contains("Thought"), "{lines:#?}");
        a.key(KeyCode::Char('q'));
        let lines = a.settle().await;
        assert!(row_with(&lines, "› Retry").is_some(), "{lines:#?}");
        a.nothing_sent().await;
    })
    .await;
}
