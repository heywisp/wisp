use std::collections::VecDeque;
use std::sync::Mutex;

use wisp_protocol::CodeProblem;
use wisp_protocol::subscription_auth::{ChallengeKind, LocalState, SubscriptionEndpoint};
use wisp_render::chrome::account::{self, mockup_mark, vocabulary_slips};
use wisp_render::theme::glyphs::GlyphSet;

use wisp_tui::term::print::Stream;
use wisp_tui::term::prompt::{ListRow, Shown, lead_lines, list_lines};

use super::*;
use crate::testing::texts;

const CODE: &str = "CODE_CANARY#STATE_CANARY";
const URL: &str = "https://claude.ai/oauth/authorize?state=STATE_CANARY";

fn account(name: &str, held: Held, health: Health) -> Account {
    let kind = match &held {
        Held::Subscription(provider) => AccountKind::from(*provider),
        Held::SetupToken => AccountKind::ClaudeSubscription,
        Held::Stored | Held::Variable { .. } => AccountKind::ClaudeApi,
    };
    Account {
        mark: mockup_mark(name),
        name: name.to_owned(),
        kind,
        held,
        health,
        host: None,
        off: None,
    }
}

fn claude(name: &str) -> Account {
    account(
        name,
        Held::Subscription(SubscriptionProvider::Claude),
        Health::SignedIn,
    )
}

fn accounts(list: Vec<Account>) -> Accounts {
    Accounts {
        list,
        default: None,
        keys_file: "~/.config/wisp/.env".to_owned(),
        shadowed: None,
    }
}

#[derive(Default)]
struct Script {
    interrupt: bool,
    cancel_copy: bool,
    answers: VecDeque<String>,
    said: Vec<String>,
    asked: Vec<String>,
}

impl Script {
    fn new(answers: &[&str]) -> Self {
        Self {
            answers: answers.iter().map(|a| (*a).to_owned()).collect(),
            ..Self::default()
        }
    }

    fn next(&mut self, question: &str) -> Result<String, Stop> {
        self.asked.push(question.to_owned());
        match self.answers.pop_front() {
            Some(answer) if answer == "^C" => Err(Stop::Cancelled),
            Some(answer) => Ok(answer),
            None => Err(Stop::Cancelled),
        }
    }
}

impl Front for Script {
    fn say(&mut self, text: &str) {
        self.said.push(text.to_owned());
    }
    fn choose(&mut self, rows: &[Row]) -> Result<usize, Stop> {
        let glyphs = Glyphs::of(GlyphSet::Unicode);
        let shown = numbered_text(rows, 80, &glyphs);
        let answer = self.next(&shown)?;
        pick_answer(&answer, rows).ok_or(Stop::Failed(answer))
    }
    fn line(&mut self, question: &str, start: &str) -> Result<String, Stop> {
        let answer = self.next(&format!("{question}{start}"))?;
        Ok(if answer.is_empty() {
            start.to_owned()
        } else {
            answer
        })
    }
    fn code(&mut self, question: &str) -> Result<String, Stop> {
        self.next(question)
    }
    fn enter(&mut self) -> Result<(), Stop> {
        self.next("enter").map(drop)
    }
    fn wait(&mut self, done: &dyn Fn() -> bool) -> Result<(), Stop> {
        while !done() {
            std::thread::sleep(std::time::Duration::from_millis(5));
            if self.interrupt {
                return Err(Stop::Cancelled);
            }
        }
        Ok(())
    }
    fn link(&mut self, url: &Secret, browser: &str) -> Result<(), Stop> {
        if self.cancel_copy {
            return Err(Stop::Cancelled);
        }
        self.say(url.expose());
        self.say(browser);
        Ok(())
    }
    fn glyphs(&self) -> Glyphs {
        Glyphs::of(GlyphSet::Unicode)
    }
}

#[derive(Clone, Default)]
struct Recorder {
    sent: Arc<Mutex<Vec<AuthCommand>>>,
    replies: Arc<Mutex<VecDeque<AuthOutcome>>>,
}

impl Recorder {
    fn new(replies: Vec<AuthOutcome>) -> Self {
        Self {
            sent: Arc::default(),
            replies: Arc::new(Mutex::new(replies.into())),
        }
    }
    fn sent(&self) -> Vec<AuthCommand> {
        self.sent.lock().unwrap().clone()
    }
}

impl Auth for Recorder {
    fn send(&self, command: AuthCommand) -> BoxFuture<'static, AuthEvent> {
        let request = command.request();
        self.sent.lock().unwrap().push(command);
        let outcome = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(AuthOutcome::Cancelled { login: request });
        Box::pin(async move { AuthEvent { request, outcome } })
    }
    fn prefer(&self, _: RequestId, _: Option<StoreKind>, _: bool) {}
}

fn challenge(provider: SubscriptionProvider, alias: &str, opened: bool) -> AuthOutcome {
    AuthOutcome::Challenge {
        challenge: PrivateChallenge {
            login: RequestId(1),
            provider,
            alias: alias.to_owned(),
            kind: match provider {
                SubscriptionProvider::Claude => ChallengeKind::ManualCode,
                SubscriptionProvider::ChatGpt => ChallengeKind::BrowserLoopback,
                SubscriptionProvider::Antigravity if opened => ChallengeKind::BrowserLoopback,
                SubscriptionProvider::Antigravity => ChallengeKind::ManualCode,
            },
            authorization_url: Secret::new(URL),
            expires_in_secs: 600,
            opened,
        },
    }
}

fn ready(alias: &str, provider: SubscriptionProvider) -> AuthOutcome {
    AuthOutcome::Ready {
        endpoint: SubscriptionEndpoint {
            alias: alias.to_owned(),
            provider,
            storage: None,
            is_default: false,
            state: LocalState::Working,
        },
        previous_cleanup_pending: false,
    }
}

fn failed(kind: AuthFailureKind, problem: Option<CodeProblem>) -> AuthOutcome {
    AuthOutcome::Failed {
        kind,
        message: "refused".to_owned(),
        problem,
        same_account: None,
    }
}

fn want(name: &str) -> Want {
    Want {
        provider: SubscriptionProvider::Claude,
        name: name.to_owned(),
        again: false,
        another: false,
    }
}

fn run<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

#[test]
fn the_keyword_table() {
    let none = accounts(vec![]);
    let one = accounts(vec![claude("claude-work")]);
    let two = accounts(vec![claude("claude-work"), claude("claude-subscription")]);
    let mixed = accounts(vec![
        claude("claude-work"),
        account("saved-key", Held::Stored, Health::SignedIn),
        account(
            "work-env",
            Held::Variable {
                var: "WORK_KEY".to_owned(),
                keys_file: false,
            },
            Health::SignedIn,
        ),
        account(
            "env",
            Held::Variable {
                var: "ANTHROPIC_API_KEY".to_owned(),
                keys_file: true,
            },
            Health::SignedIn,
        ),
    ]);
    let subscription = |name: Option<&str>| Goal::Subscription {
        provider: SubscriptionProvider::Claude,
        name: name.map(str::to_owned),
    };
    for set in [&none, &one, &two] {
        assert_eq!(
            route(None, None, None, None, set),
            Goal::Pick { name: None }
        );
        assert_eq!(
            route(Some("claude"), None, None, None, set),
            subscription(None)
        );
        assert_eq!(
            route(None, None, None, Some(ProviderKind::Anthropic), set),
            subscription(None)
        );
        assert_eq!(
            route(Some("chatgpt"), None, None, None, set),
            Goal::Subscription {
                provider: SubscriptionProvider::ChatGpt,
                name: None
            }
        );
        assert_eq!(
            route(Some("antigravity"), None, None, None, set),
            Goal::Subscription {
                provider: SubscriptionProvider::Antigravity,
                name: None
            }
        );
        assert_eq!(
            route(None, None, None, Some(ProviderKind::Gemini), set),
            Goal::Subscription {
                provider: SubscriptionProvider::Antigravity,
                name: None
            }
        );
        assert_eq!(
            route(Some("key"), None, None, None, set),
            Goal::Key {
                name: None,
                provider: None
            }
        );
        assert_eq!(
            route(Some("key"), None, None, Some(ProviderKind::OpenAi), set),
            Goal::Key {
                name: None,
                provider: Some(SubscriptionProvider::ChatGpt)
            }
        );
        assert_eq!(
            route(Some("key"), None, None, Some(ProviderKind::Gemini), set),
            Goal::Key {
                name: None,
                provider: Some(SubscriptionProvider::Antigravity)
            }
        );
        assert_eq!(
            route(Some("status"), None, None, None, set),
            Goal::Status(None)
        );
        assert_eq!(
            route(Some("claude-home"), None, None, None, set),
            Goal::Pick {
                name: Some("claude-home".to_owned())
            }
        );
    }
    assert_eq!(
        route(Some("claude"), None, Some("claude-work"), None, &one),
        subscription(Some("claude-work"))
    );
    assert_eq!(
        route(Some("key"), None, Some("claude-work"), None, &one),
        Goal::Refuse(
            "claude-work is a Claude subscription account, not a key; sign in to it again with wisp login claude-work, or give another --name"
                .to_owned()
        )
    );
    assert_eq!(
        route(Some("key"), None, Some("saved-key"), None, &mixed),
        Goal::Again(mixed.get("saved-key").unwrap().clone())
    );
    assert_eq!(
        route(Some("key"), None, Some(" Work "), None, &one),
        Goal::Key {
            name: Some("work".to_owned()),
            provider: None
        }
    );
    assert_eq!(
        route(Some("key"), None, Some("Claude-Work"), None, &one),
        route(Some("key"), None, Some("claude-work"), None, &one)
    );
    assert_eq!(
        route(Some("key"), None, Some("env"), None, &mixed),
        Goal::Again(mixed.get("env").unwrap().clone())
    );
    assert_eq!(
        route(Some("key"), None, Some("Bad!"), None, &one),
        Goal::Refuse(wisp_protocol::ACCOUNT_NAME_RULE.to_owned())
    );
    assert_eq!(
        route(None, None, Some(" Work "), None, &one),
        Goal::Pick {
            name: Some("work".to_owned())
        }
    );
    assert_eq!(
        route(None, None, Some("Bad!"), None, &one),
        Goal::Refuse(wisp_protocol::ACCOUNT_NAME_RULE.to_owned())
    );
    assert_eq!(
        route(None, None, Some("ENV"), None, &one),
        Goal::Refuse("That name is reserved for the keys-file account; choose another.".to_owned())
    );
    assert_eq!(
        route(None, None, Some("env"), None, &mixed),
        Goal::Again(mixed.get("env").unwrap().clone())
    );
    assert_eq!(
        route(Some("status"), Some("claude-work"), None, None, &one),
        Goal::Status(Some("claude-work".to_owned()))
    );
    for name in ["claude-work", "saved-key", "work-env", "env"] {
        assert_eq!(
            route(Some(name), None, None, None, &mixed),
            Goal::Again(mixed.get(name).unwrap().clone()),
            "{name}"
        );
    }
    assert!(matches!(
        route(Some("claude"), Some("extra"), None, None, &one),
        Goal::Refuse(_)
    ));
    assert_eq!(
        mixed.variable_line("env", Ending::Change).unwrap(),
        "Change ANTHROPIC_API_KEY in ~/.config/wisp/.env."
    );
    assert_eq!(
        mixed.variable_line("work-env", Ending::Change).unwrap(),
        "Change WORK_KEY in your shell."
    );
    let reserved = accounts(
        [
            "claude",
            "chatgpt",
            "antigravity",
            "key",
            "status",
            "claude-work",
        ]
        .into_iter()
        .map(claude)
        .collect(),
    );
    for name in [
        "claude",
        "chatgpt",
        "antigravity",
        "key",
        "status",
        "claude-work",
    ] {
        let command = login_again(Surface::Terminal, name, reaches(name, &reserved));
        let words: Vec<&str> = command.split_whitespace().skip(2).collect();
        let goal = match words.as_slice() {
            ["--name", given] => route(None, None, Some(given), None, &reserved),
            [given] => route(Some(given), None, None, None, &reserved),
            _ => panic!("{command}"),
        };
        assert_eq!(goal, Goal::Again(claude(name)), "{command}");
        assert_eq!(
            command.contains("--name"),
            name != "claude-work",
            "{command}"
        );
    }
}

#[test]
fn a_provider_word_with_accounts_of_it_asks_and_never_replaces_silently() {
    let two = accounts(vec![
        claude("claude-subscription"),
        account(
            "claude-work",
            Held::Subscription(SubscriptionProvider::Claude),
            Health::Expired,
        ),
    ]);
    let mut front = Script::new(&["1"]);
    let picked =
        plan_subscription(&mut front, &two, SubscriptionProvider::Claude, None, true).unwrap();
    assert_eq!(
        picked,
        Want {
            provider: SubscriptionProvider::Claude,
            name: "claude-work".to_owned(),
            again: true,
            another: true,
        }
    );
    assert_eq!(
        front.asked[0],
        "  1  Sign in again to claude-work           sign-in expired\n  2  Sign in again to claude-subscription\n  3  Add another Claude account\n"
    );
    let mut front = Script::new(&["3", ""]);
    assert_eq!(
        plan_subscription(&mut front, &two, SubscriptionProvider::Claude, None, true).unwrap(),
        Want {
            provider: SubscriptionProvider::Claude,
            name: "claude".to_owned(),
            again: false,
            another: true,
        }
    );
    let mut front = Script::new(&["another", ""]);
    assert!(
        !plan_subscription(&mut front, &two, SubscriptionProvider::Claude, None, true)
            .unwrap()
            .again
    );
    let mut front = Script::new(&["^C"]);
    assert_eq!(
        plan_subscription(&mut front, &two, SubscriptionProvider::Claude, None, true),
        Err(Stop::Cancelled)
    );
}

#[test]
fn a_provider_word_with_accounts_of_it_and_no_terminal_refuses_naming_the_name_flag() {
    let one = accounts(vec![claude("claude-subscription")]);
    let mut front = Script::new(&[]);
    let Err(Stop::Failed(line)) =
        plan_subscription(&mut front, &one, SubscriptionProvider::Claude, None, false)
    else {
        panic!("expected a refusal")
    };
    assert!(line.contains("--name NAME"), "{line}");
    assert!(line.contains("claude-subscription"), "{line}");
    assert!(front.asked.is_empty());
}

#[test]
fn naming_an_existing_account_signs_in_to_it_again() {
    let one = accounts(vec![
        claude("claude-subscription"),
        account("saved-key", Held::Stored, Health::SignedIn),
    ]);
    let mut front = Script::new(&[]);
    assert_eq!(
        plan_subscription(
            &mut front,
            &one,
            SubscriptionProvider::Claude,
            Some("claude-subscription".to_owned()),
            false
        )
        .unwrap(),
        Want {
            provider: SubscriptionProvider::Claude,
            name: "claude-subscription".to_owned(),
            again: true,
            another: false,
        }
    );
    let two = accounts(vec![claude("claude-subscription"), claude("claude-work")]);
    assert!(
        plan_subscription(
            &mut front,
            &two,
            SubscriptionProvider::Claude,
            Some("claude-subscription".to_owned()),
            false
        )
        .unwrap()
        .another
    );
    assert!(matches!(
        plan_subscription(
            &mut front,
            &one,
            SubscriptionProvider::ChatGpt,
            Some("claude-subscription".to_owned()),
            false
        ),
        Err(Stop::Failed(_))
    ));
    assert!(matches!(
        plan_subscription(
            &mut front,
            &one,
            SubscriptionProvider::Claude,
            Some("saved-key".to_owned()),
            true
        ),
        Err(Stop::Failed(_))
    ));
    for (given, said) in [
        (
            "env",
            "That name is reserved for the keys-file account; choose another.",
        ),
        (&"a".repeat(33), wisp_protocol::ACCOUNT_NAME_RULE.as_str()),
    ] {
        assert_eq!(
            plan_subscription(
                &mut front,
                &one,
                SubscriptionProvider::Claude,
                Some(given.to_owned()),
                false
            ),
            Err(Stop::Failed(said.to_owned()))
        );
    }
}

#[test]
fn a_taken_suggested_name_asks_for_one_and_is_never_numbered() {
    let taken = accounts(vec![account("claude", Held::Stored, Health::SignedIn)]);
    let mut front = Script::new(&["claude", "Bad Name", "ENV", "claude-home"]);
    let picked =
        plan_subscription(&mut front, &taken, SubscriptionProvider::Claude, None, true).unwrap();
    assert_eq!(picked.name, "claude-home");
    assert_eq!(front.asked, ["Name for this account: "; 4]);
    assert_eq!(
        front.said,
        [
            "claude is taken.",
            "claude is taken.",
            wisp_protocol::ACCOUNT_NAME_RULE.as_str(),
            "That name is reserved for the keys-file account; choose another."
        ]
    );
    let mut front = Script::new(&[]);
    assert!(matches!(
        plan_subscription(&mut front, &taken, SubscriptionProvider::Claude, None, false),
        Err(Stop::Failed(line)) if line.contains("--name")
    ));
}

#[test]
fn a_bad_paste_asks_again_on_the_same_login_and_an_empty_enter_says_to_paste_first() {
    let auth = Recorder::new(vec![
        challenge(SubscriptionProvider::Claude, "claude", true),
        failed(AuthFailureKind::CodeRejected, Some(CodeProblem::Repeated)),
        failed(AuthFailureKind::CodeRejected, None),
        ready("claude", SubscriptionProvider::Claude),
    ]);
    let mut front = Script::new(&[
        "",
        "CODE_CANARY#STATE_CANARYCODE_CANARY#STATE_CANARY",
        "CODE_CANARY#OLD",
        CODE,
    ]);
    let done = run(subscribe(
        &auth,
        &mut front,
        &accounts(vec![]),
        want("claude"),
        Prefs::default(),
    ))
    .unwrap();
    assert_eq!(done.name, "claude");
    let sent = auth.sent();
    assert_eq!(sent.len(), 4);
    let logins: Vec<RequestId> = sent[1..]
        .iter()
        .map(|command| match command {
            AuthCommand::Finish { login, .. } => *login,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(logins, vec![RequestId(1); 3]);
    assert_eq!(
        front.said,
        [
            "Sign in on Claude's page for claude, then paste the code it shows.",
            URL,
            "tried your browser",
            "Paste the code from Claude's page first.",
            "That's the code twice. Paste again; the new paste replaces this one.",
            "That code is from an earlier sign-in: paste the one from the newest page.",
        ]
    );
    for line in &front.said {
        assert!(!line.contains("CODE_CANARY"));
    }
}

#[test]
fn a_transport_failure_retries_once_on_the_kept_login_with_the_same_code() {
    let auth = Recorder::new(vec![
        challenge(SubscriptionProvider::Claude, "claude", false),
        failed(AuthFailureKind::Unreachable, None),
        ready("claude", SubscriptionProvider::Claude),
    ]);
    let mut front = Script::new(&[CODE, ""]);
    run(subscribe(
        &auth,
        &mut front,
        &accounts(vec![]),
        want("claude"),
        Prefs::default(),
    ))
    .unwrap();
    let sent = auth.sent();
    let codes: Vec<Option<String>> = sent
        .iter()
        .filter_map(|command| match command {
            AuthCommand::Finish {
                response, login, ..
            } => {
                assert_eq!(*login, RequestId(1));
                Some(response.as_ref().map(|r| r.expose().to_owned()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(codes, vec![Some(CODE.to_owned()), Some(CODE.to_owned())]);
    assert!(
        front
            .said
            .contains(&"Couldn't reach Claude. enter tries again.".to_owned())
    );
    assert_eq!(
        front.said[..3],
        [
            "Sign in on Claude's page for claude, then paste the code it shows.",
            URL,
            "no browser here"
        ]
    );
}

#[test]
fn a_refused_code_ends_the_login_and_enter_opens_a_new_page() {
    let auth = Recorder::new(vec![
        challenge(SubscriptionProvider::Claude, "claude", true),
        failed(AuthFailureKind::Unavailable, None),
        challenge(SubscriptionProvider::Claude, "claude", true),
        ready("claude", SubscriptionProvider::Claude),
    ]);
    let mut front = Script::new(&[CODE, "", CODE]);
    run(subscribe(
        &auth,
        &mut front,
        &accounts(vec![]),
        want("claude"),
        Prefs::default(),
    ))
    .unwrap();
    let starts = auth
        .sent()
        .iter()
        .filter(|command| matches!(command, AuthCommand::Start { .. }))
        .count();
    assert_eq!(starts, 2);
    assert!(front.said.contains(
        &"Claude didn't accept that code; each works once. enter opens a new page.".to_owned()
    ));
    let auth = Recorder::new(vec![
        challenge(SubscriptionProvider::Claude, "claude-two", true),
        AuthOutcome::Failed {
            kind: AuthFailureKind::Declined,
            message: "refused".to_owned(),
            problem: None,
            same_account: Some("claude".to_owned()),
        },
    ]);
    let stop = run(subscribe(
        &auth,
        &mut Script::new(&[CODE]),
        &accounts(vec![claude("claude")]),
        want("claude-two"),
        Prefs::default(),
    ))
    .unwrap_err();
    assert_eq!(
        stop,
        Stop::SameAccount(SameAccount {
            provider: SubscriptionProvider::Claude,
            name: "claude".to_owned(),
            mark: Some(5),
            surface: Surface::Terminal,
            short: false,
        })
    );
    let starts = auth
        .sent()
        .iter()
        .filter(|command| matches!(command, AuthCommand::Start { .. }))
        .count();
    assert_eq!(starts, 1);
    assert_eq!(
        finish(
            stop,
            &crate::prompt_caps(&wisp_tui::term::SavedDisplay::default())
        ),
        ExitCode::FAILURE
    );
}

#[test]
fn a_name_taken_by_the_time_the_sign_in_finishes_asks_again_with_the_login_kept() {
    let auth = Recorder::new(vec![
        challenge(SubscriptionProvider::Claude, "claude", true),
        failed(AuthFailureKind::AliasConflict, None),
        ready("claude-home", SubscriptionProvider::Claude),
    ]);
    let mut front = Script::new(&[CODE, "claude-home"]);
    let done = run(subscribe(
        &auth,
        &mut front,
        &accounts(vec![]),
        want("claude"),
        Prefs::default(),
    ))
    .unwrap();
    assert_eq!(done.name, "claude-home");
    let sent = auth.sent();
    let AuthCommand::Finish {
        login,
        alias,
        response,
        ..
    } = &sent[2]
    else {
        panic!("{sent:?}")
    };
    assert_eq!(*login, RequestId(1));
    assert_eq!(alias.as_deref(), Some("claude-home"));
    assert_eq!(response.as_ref().map(Secret::expose), Some(CODE));
    assert_eq!(front.asked[1], "Name for this account: ");
    assert!(front.said.contains(&"claude is taken.".to_owned()));
}

#[test]
fn ctrl_c_at_the_code_cancels_the_pending_login() {
    let copying = Script {
        cancel_copy: true,
        ..Script::new(&[CODE])
    };
    for mut front in [Script::new(&["^C"]), copying] {
        let auth = Recorder::new(vec![challenge(
            SubscriptionProvider::Claude,
            "claude",
            true,
        )]);
        assert_eq!(
            run(subscribe(
                &auth,
                &mut front,
                &accounts(vec![]),
                want("claude"),
                Prefs::default()
            )),
            Err(Stop::Cancelled)
        );
        assert_eq!(front.asked.len(), usize::from(!front.cancel_copy));
        assert!(matches!(
            auth.sent()[..],
            [
                AuthCommand::Start { .. },
                AuthCommand::Cancel {
                    login: RequestId(1),
                    ..
                }
            ]
        ));
    }
}

#[derive(Clone, Default)]
struct Saving {
    saved: Arc<tokio::sync::Notify>,
}

impl Auth for Saving {
    fn send(&self, command: AuthCommand) -> BoxFuture<'static, AuthEvent> {
        let request = command.request();
        let saved = self.saved.clone();
        Box::pin(async move {
            let outcome = match command {
                AuthCommand::Start { .. } => {
                    challenge(SubscriptionProvider::ChatGpt, "chatgpt", true)
                }
                AuthCommand::Finish { .. } => {
                    saved.notified().await;
                    ready("chatgpt", SubscriptionProvider::ChatGpt)
                }
                _ => {
                    saved.notify_one();
                    failed(AuthFailureKind::Busy, None)
                }
            };
            AuthEvent { request, outcome }
        })
    }
    fn prefer(&self, _: RequestId, _: Option<StoreKind>, _: bool) {}
}

#[test]
fn ctrl_c_while_the_account_is_saved_reports_the_saved_account() {
    let mut front = Script {
        interrupt: true,
        ..Script::default()
    };
    let done = run(subscribe(
        &Saving::default(),
        &mut front,
        &accounts(vec![]),
        Want {
            provider: SubscriptionProvider::ChatGpt,
            ..want("chatgpt")
        },
        Prefs::default(),
    ))
    .unwrap();
    assert_eq!(done.name, "chatgpt");
    assert!(done.ready);
}

#[test]
fn chatgpt_waits_for_the_browser_and_retries_once_after_a_transport_failure() {
    let auth = Recorder::new(vec![
        challenge(SubscriptionProvider::ChatGpt, "chatgpt", true),
        failed(AuthFailureKind::Unreachable, None),
        challenge(SubscriptionProvider::ChatGpt, "chatgpt", true),
        ready("chatgpt", SubscriptionProvider::ChatGpt),
    ]);
    let mut front = Script::new(&[""]);
    let wanted = Want {
        provider: SubscriptionProvider::ChatGpt,
        ..want("chatgpt")
    };
    run(subscribe(
        &auth,
        &mut front,
        &accounts(vec![]),
        wanted,
        Prefs::default(),
    ))
    .unwrap();
    let sent = auth.sent();
    assert!(matches!(
        sent[1],
        AuthCommand::Finish { response: None, .. }
    ));
    assert!(matches!(
        sent[2],
        AuthCommand::Open {
            login: RequestId(1),
            ..
        }
    ));
    assert!(matches!(
        sent[3],
        AuthCommand::Finish {
            login: RequestId(1),
            ..
        }
    ));
    assert_eq!(
        front.said[0],
        "Sign in on ChatGPT's page for chatgpt; wisp carries on by itself."
    );
}

fn signed_out_of(names: &[&str]) -> AuthOutcome {
    AuthOutcome::LoggedOut {
        presets: Vec::new(),
        alias: names[0].to_owned(),
        remote_revoked: false,
        cleanup_pending: false,
        also: names[1..].iter().map(|n| (*n).to_owned()).collect(),
        left_sign_ins: Vec::new(),
        left_keys: Vec::new(),
        new_start: None,
        moved: Vec::new(),
        stay: Vec::new(),
        stopped: None,
    }
}

fn four_accounts() -> Accounts {
    let mut set = accounts(vec![
        claude("claude-subscription"),
        claude("claude-work"),
        account(
            "chatgpt",
            Held::Subscription(SubscriptionProvider::ChatGpt),
            Health::SignedIn,
        ),
        account(
            "claude-api",
            Held::Variable {
                var: "ANTHROPIC_API_KEY".to_owned(),
                keys_file: true,
            },
            Health::SignedIn,
        ),
    ]);
    set.default = Some("claude-subscription".to_owned());
    set
}

fn signing_out(
    auth: &Recorder,
    answers: &[&str],
    name: Option<&str>,
) -> (Result<AuthOutcome, Stop>, Script) {
    let mut front = Script::new(answers);
    let answer = super::super::logout::Answer {
        yes: false,
        terminal: true,
    };
    let glyphs = Glyphs::of(GlyphSet::Unicode);
    let out = run(super::super::logout::sign_out(
        auth,
        &mut front,
        &four_accounts(),
        name,
        &answer,
        &glyphs,
        80,
    ));
    (out, front)
}

#[test]
fn the_terminal_sends_the_commands_the_card_sends() {
    let auth = Recorder::new(vec![
        challenge(SubscriptionProvider::Claude, "claude", true),
        ready("claude", SubscriptionProvider::Claude),
    ]);
    let none = accounts(vec![]);
    let mut front = Script::new(&["1", "", CODE]);
    let Picked::Subscription(provider) = first_question(&mut front).unwrap() else {
        panic!("expected Claude")
    };
    let wanted = plan_subscription(&mut front, &none, provider, None, true).unwrap();
    run(subscribe(
        &auth,
        &mut front,
        &none,
        wanted,
        Prefs::default(),
    ))
    .unwrap();
    assert_eq!(
        auth.sent(),
        vec![
            AuthCommand::Start {
                request: RequestId(1),
                provider: SubscriptionProvider::Claude,
                alias: Some("claude".to_owned()),
                reauthorize: false,
            },
            AuthCommand::Finish {
                request: RequestId(2),
                login: RequestId(1),
                response: Some(Secret::new(CODE)),
                alias: None,
            },
        ]
    );
    assert_eq!(
        wisp_protocol::subscription_auth::SubscriptionProvider::Claude.default_name(),
        "claude"
    );
    let typed = Recorder::new(vec![signed_out_of(&["claude-work"])]);
    assert!(signing_out(&typed, &[], Some("claude-work")).0.is_ok());
    assert_eq!(
        typed.sent(),
        [AuthCommand::Logout {
            request: RequestId(1),
            alias: "claude-work".to_owned(),
            also: Vec::new(),
            chat: None,
            confirmed: false,
            follow: Vec::new(),
            gone: false,
            accept_api: false,
        }]
    );
    let quiet = Recorder::new(Vec::new());
    assert!(signing_out(&quiet, &[""], None).0.is_err());
    assert_eq!(quiet.sent(), []);
    let picked = Recorder::new(vec![signed_out_of(&["claude-work"])]);
    let (out, front) = signing_out(&picked, &["2"], None);
    assert!(out.is_ok());
    assert_eq!(picked.sent(), typed.sent());
    assert!(
        !front.said.iter().any(|q| q.contains("Sign out of")),
        "{:?}",
        front.said
    );
    let needs = AuthOutcome::LogoutNeeds {
        fixed: false,
        presets: Vec::new(),
        deletes_keys_of: Vec::new(),
        new_start: Some("claude-api".to_owned()),
        ways: Vec::new(),
    };
    let all = Recorder::new(vec![needs.clone()]);
    assert!(signing_out(&all, &[""], Some("all")).0.is_err());
    assert!(matches!(
        all.sent().as_slice(),
        [AuthCommand::Logout { confirmed: false, also, .. }] if also.len() == 2
    ));
    let refused = Recorder::new(vec![failed(AuthFailureKind::InvalidAlias, None)]);
    let (out, _) = signing_out(&refused, &[], Some("claude-api"));
    assert!(matches!(out, Ok(AuthOutcome::Failed { .. })));
    assert!(matches!(
        refused.sent().as_slice(),
        [AuthCommand::Logout { confirmed: false, also, .. }] if also.is_empty()
    ));
}

fn caps(colour: bool) -> wisp_tui::term::Caps {
    use wisp_tui::term::caps::{Env, Flags, Streams};
    let streams = Streams {
        stdin_tty: true,
        stdout_tty: colour,
        stderr_tty: colour,
    };
    let env = Env::from_pairs([("TERM", "xterm-256color"), ("COLORTERM", "truecolor")]);
    wisp_tui::term::Caps::resolve(&env, streams, Flags::default(), None)
}

fn glyph_sets() -> [(GlyphSet, &'static str); 2] {
    [(GlyphSet::Unicode, "Unicode"), (GlyphSet::Ascii, "Ascii")]
}

#[test]
fn result_lines_and_help_at_three_widths() {
    for width in [40, 80, 120] {
        for (set, set_name) in glyph_sets() {
            let glyphs = Glyphs::of(set);
            let done = |name: &str| Done {
                mark: mockup_mark(name),
                name: name.to_owned(),
                ready: true,
                cleanup_pending: false,
            };
            let mut sections = Vec::new();
            for (want, done, first) in [
                (want("claude"), done("claude"), true),
                (want("claude"), done("claude"), false),
                (
                    Want {
                        again: true,
                        ..want("claude-work")
                    },
                    done("claude-work"),
                    false,
                ),
                (
                    Want {
                        provider: SubscriptionProvider::ChatGpt,
                        ..want("chatgpt")
                    },
                    Done {
                        ready: false,
                        ..done("chatgpt")
                    },
                    false,
                ),
            ] {
                let lines = result_lines(
                    AccountKind::from(want.provider),
                    &done,
                    want.again,
                    first,
                    &glyphs,
                    width,
                );
                let plain = wisp_tui::term::print::styled(&lines, &caps(false), Stream::Stdout, 0);
                let coloured =
                    wisp_tui::term::print::styled(&lines, &caps(true), Stream::Stdout, 0);
                let mark = account::mark(done.mark, &glyphs).text;
                let shown = texts(&lines).join("\n");
                assert_eq!(
                    String::from_utf8_lossy(&plain),
                    format!("{}\n", shown.replacen(&mark, "", 1))
                );
                assert!(
                    String::from_utf8_lossy(&coloured).contains(&mark),
                    "{shown}"
                );
                sections.push(shown);
            }
            for (provider, name, again) in [
                (SubscriptionProvider::Claude, "claude", false),
                (SubscriptionProvider::Claude, "claude-work", true),
                (SubscriptionProvider::ChatGpt, "chatgpt", false),
            ] {
                let mark = mockup_mark(name).filter(|_| again);
                let AuthOutcome::Challenge { challenge } = challenge(provider, name, true) else {
                    unreachable!()
                };
                let mut front = Script::default();
                let wanted = Want {
                    provider,
                    again,
                    another: provider == SubscriptionProvider::Claude,
                    ..want(name)
                };
                intro(&mut front, &wanted, &challenge, mark).unwrap();
                let said = front.said.iter().take_while(|line| *line != URL);
                let shown = said.flat_map(|line| {
                    texts(&marked_lines(
                        &line.replace('●', glyphs.mark),
                        mark,
                        width,
                        &glyphs,
                    ))
                });
                sections.push(shown.collect::<Vec<_>>().join("\n"));
            }
            let statuses = [
                (account::TRIED_BROWSER, account::LINK_COPIED),
                (account::TRIED_BROWSER, account::LINK_NOT_COPIED),
                (account::NO_BROWSER, account::LINK_SENT),
                (account::NO_BROWSER, account::LINK_NOT_COPIED),
            ]
            .map(|(browser, words)| {
                let tail = status_tail(browser, words, width, &glyphs).join("\n");
                format!("{browser}{tail}")
            });
            sections.push(statuses.join("\n"));
            let again = mockup_mark("claude-subscription");
            let rows = [
                Row {
                    word: "claude".into(),
                    mark: None,
                    label: "Claude".into(),
                    detail: "Pro or Max subscription".into(),
                    keep_detail: false,
                },
                Row {
                    word: "claude-subscription".into(),
                    mark: again,
                    label: "Sign in again to claude-subscription".into(),
                    detail: "sign-in expired".into(),
                    keep_detail: false,
                },
                Row {
                    word: "another".into(),
                    mark: None,
                    label: "Add another Claude account".into(),
                    detail: String::new(),
                    keep_detail: false,
                },
            ];
            let shown = rows_text(&rows, width, &glyphs);
            sections.push(numbered_text(&rows, width, &glyphs));
            for state in [
                Shown::Picking(Some(0)),
                Shown::Picking(Some(1)),
                Shown::Picking(None),
            ] {
                let lines = texts(&list_lines(&shown, state, &glyphs));
                sections.push(lines.join("\n"));
            }
            sections.push(help(width, &glyphs));
            sections.push(status_help(width, &glyphs));
            let set = four_accounts();
            let set_rows = super::super::logout::rows(&set, &glyphs);
            let listed = rows_text(&set_rows, width, &glyphs);
            sections.push(numbered_text(&set_rows, width, &glyphs));
            let lines = texts(&list_lines(&listed, Shown::Picking(None), &glyphs));
            sections.push(lines.join("\n"));
            for (names, start) in [
                (
                    &["claude-subscription", "claude-work", "chatgpt"][..],
                    Some("claude-api"),
                ),
                (&["claude-subscription", "claude-work", "chatgpt"], None),
                (&["work-api"], None),
            ] {
                let keys = if names == ["work-api"] {
                    vec!["work-api".to_owned()]
                } else {
                    Vec::new()
                };
                let auth = Recorder::new(vec![
                    AuthOutcome::LogoutNeeds {
                        fixed: false,
                        presets: Vec::new(),
                        deletes_keys_of: keys,
                        new_start: start.map(str::to_owned),
                        ways: Vec::new(),
                    },
                    signed_out_of(names),
                ]);
                let word = if names.len() > 1 { "all" } else { names[0] };
                let mut front = Script::new(&["1"]);
                let answer = super::super::logout::Answer {
                    yes: false,
                    terminal: true,
                };
                let mut set = four_accounts();
                if start.is_none() && names.len() > 1 {
                    set.list.pop();
                }
                if names == ["work-api"] {
                    set.list
                        .push(account("work-api", Held::Stored, Health::SignedIn));
                }
                let outcome = run(super::super::logout::sign_out(
                    &auth,
                    &mut front,
                    &set,
                    Some(word),
                    &answer,
                    &glyphs,
                    width,
                ))
                .unwrap();
                let mut shown = Vec::new();
                assert_eq!(front.asked.len(), 1, "{:?}", front.asked);
                shown.extend(texts(&lead_lines(&front.said.join("\n"), width, &glyphs)));
                let options = rows_text(&label_rows(&SIGN_OUT_OPTIONS), width, &glyphs);
                shown.extend(texts(&list_lines(
                    &options,
                    Shown::Picking(Some(1)),
                    &glyphs,
                )));
                let mut outcome = outcome;
                if let (AuthOutcome::LoggedOut { new_start, .. }, Some(start)) =
                    (&mut outcome, start)
                {
                    *new_start = Some(start.to_owned());
                }
                let (lines, _) =
                    super::super::logout::result_lines(&outcome, &set, &glyphs, width).unwrap();
                shown.extend(texts(&lines));
                sections.push(format!("$ wisp logout {word}\n{}", shown.join("\n")));
            }
            let text = sections.join("\n--\n");
            for line in text.lines() {
                assert!(line.chars().count() <= width, "{line}");
            }
            assert_eq!(vocabulary_slips(&text), Vec::<&str>::new(), "{text}");
            insta::assert_snapshot!(format!("login_{width}_Dark_{set_name}"), text);
        }
    }
}

#[test]
fn a_kept_detail_sits_under_its_label_when_it_does_not_fit_beside_it() {
    let detail = "Claude API \u{b7} moves this chat from your subscription to the API";
    let rows = |keep_detail: bool| {
        [
            ("claude-api", detail),
            ("claude-subscription", "Claude subscription"),
        ]
        .map(|(label, detail)| Row {
            word: String::new(),
            mark: None,
            label: label.into(),
            detail: detail.into(),
            keep_detail,
        })
    };
    for (set, set_name) in glyph_sets() {
        let glyphs = Glyphs::of(set);
        let lines =
            |rows: Vec<ListRow>| rows.into_iter().map(|(lines, _)| lines).collect::<Vec<_>>();
        assert_eq!(
            lines(rows_text(&rows(true), 120, &glyphs)),
            [
                vec![format!("claude-api            {detail}")],
                vec!["claude-subscription   Claude subscription".to_owned()],
            ],
            "{set_name}"
        );
        for width in [40, 80] {
            let kept = lines(rows_text(&rows(true), width, &glyphs));
            assert_eq!(kept[0][0], "claude-api", "{set_name} {width}");
            assert_eq!(kept[0][1..].join(" "), detail, "{set_name} {width}");
            assert_eq!(kept[1], ["claude-subscription", "Claude subscription"]);
            assert_eq!(
                lines(rows_text(&rows(false), width, &glyphs)),
                [vec!["claude-api"], vec!["claude-subscription"]],
                "{set_name} {width}"
            );
        }
    }
}

#[test]
fn the_cli_help_says_none_of_the_banned_words() {
    for width in [40, 80, 120] {
        let glyphs = Glyphs::of(GlyphSet::Unicode);
        for text in [help(width, &glyphs), status_help(width, &glyphs)] {
            assert_eq!(vocabulary_slips(&text), Vec::<&str>::new(), "{text}");
        }
    }
    let missing_word = super::no_word();
    assert_eq!(
        missing_word,
        "say what to sign in to: wisp login claude, chatgpt, antigravity or key."
    );
    for line in [missing_word.as_str(), KEY_SCRIPT, FIRST, CANCELLED] {
        assert_eq!(vocabulary_slips(line), Vec::<&str>::new(), "{line}");
    }
}

#[test]
fn every_sign_in_line_says_none_of_the_banned_words() {
    let mixed = accounts(vec![
        claude("claude"),
        account("saved-key", Held::Stored, Health::SignedIn),
        account(
            "claude-api",
            Held::Variable {
                var: "ANTHROPIC_API_KEY".to_owned(),
                keys_file: true,
            },
            Health::SignedIn,
        ),
        account(
            "work-shell",
            Held::Variable {
                var: "WORK_KEY".to_owned(),
                keys_file: false,
            },
            Health::SignedIn,
        ),
    ]);
    let mut lines = vec![
        super::super::accounts::no_such_account("work", &mixed),
        super::super::accounts::no_such_account("work", &accounts(vec![])),
        mixed.variable_line("claude-api", Ending::Change).unwrap(),
        mixed.variable_line("work-shell", Ending::Change).unwrap(),
        setup_token_line("claude-token"),
        wisp_protocol::marked_amount("$0.41", true, false).to_string(),
    ];
    for provider in SubscriptionProvider::ALL {
        for (name, terminal) in [
            (Some("saved-key"), true),
            (Some("NOT A NAME"), true),
            (None, false),
        ] {
            let crowded = accounts(vec![
                claude("claude-work"),
                account(provider.default_name(), Held::Stored, Health::SignedIn),
            ]);
            let set = if name.is_some() { &mixed } else { &crowded };
            if let Err(Stop::Failed(line)) = plan_subscription(
                &mut Script::default(),
                set,
                provider,
                name.map(str::to_owned),
                terminal,
            ) {
                lines.push(line);
            }
        }
        for (opened, another, mark) in [
            (true, false, None),
            (false, true, None),
            (true, true, Some(2)),
        ] {
            let AuthOutcome::Challenge { challenge } = challenge(provider, "work", opened) else {
                unreachable!()
            };
            let mut front = Script::default();
            intro(
                &mut front,
                &Want {
                    provider,
                    another,
                    again: mark.is_some(),
                    ..want("work")
                },
                &challenge,
                mark,
            )
            .unwrap();
            let switch = wisp_render::chrome::account::switch_first(provider.title());
            assert_eq!(front.said.contains(&switch), another, "{:?}", front.said);
            lines.extend(front.said);
        }
    }
    let mut front = Script::new(&["NOT A NAME", "saved-key", "fresh"]);
    name_step(&mut front, &|name| mixed.taken(name), "claude").unwrap();
    lines.extend(front.asked);
    lines.extend(front.said);
    let mut front = Script::new(&["3"]);
    first_question(&mut front).unwrap();
    lines.extend(front.asked);
    let held = accounts(vec![
        claude("claude-work"),
        account(
            "chatgpt",
            Held::Subscription(SubscriptionProvider::ChatGpt),
            Health::SignedIn,
        ),
    ]);
    for (provider, name, wide, narrow) in [
        (
            SubscriptionProvider::Claude,
            "claude-work",
            "⨯ Not signed in: this is the same Claude account as ● claude-work.\n  \
             To sign it in again: wisp login claude-work",
            "⨯ Not signed in: this is the same Claude\n  account as ● claude-work.\n  \
             To sign it in again:\n  wisp login claude-work",
        ),
        (
            SubscriptionProvider::ChatGpt,
            "chatgpt",
            "⨯ Not signed in: this is the same ChatGPT account as ● chatgpt.\n  \
             To sign it in again: wisp login --name chatgpt",
            "⨯ Not signed in: this is the same\n  ChatGPT account as ● chatgpt.\n  \
             To sign it in again:\n  wisp login --name chatgpt",
        ),
    ] {
        let Stop::SameAccount(refusal) = refused(provider, name.to_owned(), &held) else {
            unreachable!()
        };
        for set in [GlyphSet::Unicode, GlyphSet::Ascii] {
            let glyphs = Glyphs::of(set);
            for (width, mockup) in [(120, wide), (80, wide), (40, narrow)] {
                let shown = texts(&same_account_lines(&refusal, width, &glyphs));
                assert_eq!(
                    shown.join("\n"),
                    mockup.replace('⨯', glyphs.failed).replace('●', glyphs.mark)
                );
                lines.extend(shown);
            }
        }
    }
    use wisp_core::endpoints::goes::{answering_on, in_use, working_on};
    lines.extend([
        mixed.variable_line("claude-api", Ending::SignOut).unwrap(),
        mixed.variable_line("work-shell", Ending::SignOut).unwrap(),
        answering_on(
            "claude-work",
            "Sign out once it's done, or press esc to stop it.",
        ),
        working_on(&["/coder".to_owned()], "claude-work"),
        in_use("claude-work"),
        wisp_core::agents::endpoints::gone_text("claude-work"),
    ]);
    assert!(lines.len() > 20, "{lines:?}");
    for line in &lines {
        assert_eq!(vocabulary_slips(line), Vec::<&str>::new(), "{line}");
    }
}

#[test]
fn the_name_step_starts_with_the_suggestion_and_saves_what_is_typed() {
    let none = accounts(vec![]);
    for (answers, saved) in [
        (vec![""], "claude"),
        (vec!["claude1"], "claude1"),
        (vec!["  Claude1 "], "claude1"),
    ] {
        let mut front = Script::new(&answers);
        let want =
            plan_subscription(&mut front, &none, SubscriptionProvider::Claude, None, true).unwrap();
        assert_eq!(want.name, saved);
        assert_eq!(front.asked, ["Name for this account: claude"]);
        assert!(front.said.is_empty(), "{:?}", front.said);
    }
    let mut front = Script::new(&[]);
    let named = plan_subscription(
        &mut front,
        &none,
        SubscriptionProvider::Claude,
        Some("work".to_owned()),
        true,
    )
    .unwrap();
    assert_eq!(named.name, "work");
    assert!(front.asked.is_empty());
}

#[test]
fn the_key_path_gets_the_same_name_step() {
    let mut front = Script::new(&[""]);
    let taken = std::collections::BTreeSet::new();
    let suggested = wisp_core::endpoints::EndpointName::parse("claude-api").unwrap();
    let name = crate::manage::signing_in_name(&mut front, &taken, &suggested).unwrap();
    assert_eq!(name.as_str(), "claude-api");
    assert_eq!(front.asked, ["Name for this account: claude-api"]);
    let taken = std::collections::BTreeSet::from([suggested.clone()]);
    let mut front = Script::new(&["Work-Api"]);
    let name = crate::manage::signing_in_name(&mut front, &taken, &suggested).unwrap();
    assert_eq!(name.as_str(), "work-api");
    assert_eq!(front.asked, ["Name for this account: "]);
    assert_eq!(front.said, ["claude-api is taken."]);
}

#[test]
fn antigravity_loopback_waits_for_the_browser_and_lists_option_in_first_question() {
    let mut picker = Script::new(&["antigravity"]);
    let picked = first_question(&mut picker).unwrap();
    assert!(matches!(
        picked,
        Picked::Subscription(SubscriptionProvider::Antigravity)
    ));
    assert!(
        picker.asked[0].contains("3  Antigravity   Gemini subscription")
            && picker.asked[0].contains("4  API key       Claude, OpenAI, Gemini or a gateway"),
        "{:?}",
        picker.asked
    );
    let auth = Recorder::new(vec![
        challenge(SubscriptionProvider::Antigravity, "antigravity", true),
        ready("antigravity", SubscriptionProvider::Antigravity),
    ]);
    let mut front = Script::new(&[]);
    let done = run(subscribe(
        &auth,
        &mut front,
        &accounts(vec![]),
        Want {
            provider: SubscriptionProvider::Antigravity,
            ..want("antigravity")
        },
        Prefs::default(),
    ))
    .unwrap();
    assert_eq!(done.name, "antigravity");
    assert_eq!(
        front.said[0],
        "Sign in on Antigravity's page for antigravity; wisp carries on by itself."
    );
    assert!(matches!(
        auth.sent()[1],
        AuthCommand::Finish { response: None, .. }
    ));
}

#[test]
fn antigravity_headless_manual_mode_asks_for_callback_url_and_sends_it() {
    let callback = "http://127.0.0.1:51121/oauth-callback?code=CODE_CANARY&state=STATE_CANARY";
    let auth = Recorder::new(vec![
        challenge(SubscriptionProvider::Antigravity, "antigravity", false),
        failed(AuthFailureKind::CodeRejected, Some(CodeProblem::NoState)),
        ready("antigravity", SubscriptionProvider::Antigravity),
    ]);
    let mut front = Script::new(&["not-a-callback", callback]);
    let done = run(subscribe(
        &auth,
        &mut front,
        &accounts(vec![]),
        Want {
            provider: SubscriptionProvider::Antigravity,
            ..want("antigravity")
        },
        Prefs::default(),
    ))
    .unwrap();
    assert_eq!(done.name, "antigravity");
    assert_eq!(front.asked, ["Callback URL: ", "Callback URL: "]);
    assert_eq!(
        front.said[0],
        "Sign in on Antigravity's page for antigravity, then paste the callback URL."
    );
    assert_eq!(front.said[2], "no browser here");
    assert!(matches!(
        &auth.sent()[2],
        AuthCommand::Finish {
            response: Some(secret),
            ..
        } if secret.expose() == callback
    ));
}
