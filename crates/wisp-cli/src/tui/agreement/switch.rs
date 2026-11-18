use std::time::Duration;

use tokio::sync::mpsc;
use wisp_core::context::ModelSettings;
use wisp_core::models::{ConfigModels, ModelTable};
use wisp_protocol::settings::{Consequence, SettingChange, SettingValue, SwitchTarget, Withdrawal};
use wisp_protocol::{
    AgentEvent, ClientCommand, CompactionTrigger, Effort, RequestId, SwitchCompactionCause,
    TurnOutcome,
};
use wisp_render::chrome::{
    Alive, CacheSize, Effect, Endpoint, Rebuild, Scope, SwitchCause, SwitchWork,
};
use wisp_tui::app::picker::{Picked, Picker};
use wisp_tui::app::replay::{self, Script};
use wisp_tui::app::settings::Answer;

use super::{Harness, OPUS, Reply, UNI, WAIT, answer, compaction, filled_answer};

const SONNET_4_6: &str = "claude-sonnet-4-6";
const NO_NATIVE: &str = "[endpoints.env.models.\"claude-opus-5-5\"]\nnative_compaction = false\n";

fn harness(config: &str, switch_reply: Reply) -> Harness {
    let seen = std::sync::atomic::AtomicUsize::new(0);
    let models = ModelSettings::new(
        ModelTable::shipped().unwrap(),
        ConfigModels::parse(config).unwrap(),
    );
    let (provider, _) = super::provider(
        Endpoint::Official,
        std::sync::Arc::new(move |request: &str, n| {
            if request.contains("\"compaction\":") {
                let first = seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
                let reply = if first { Reply::Compacts } else { switch_reply };
                compaction(SONNET_4_6, reply)
            } else {
                answer(&super::model_of(request), n)
            }
        }),
    );
    Harness::build(
        provider,
        Endpoint::Official,
        SONNET_4_6,
        Effort::High,
        |a| a.with_model_settings(models),
    )
}

enum Do {
    Send(&'static str),
    Compact(u64),
    Change(u64, Option<&'static str>, Option<Effort>),
    Describe(u64),
    ChangeOnStep(&'static str, u64, &'static str, Option<Effort>),
    ChangesOnStep(&'static str, u64, Vec<SettingChange>),
    CancelSwitch(&'static str),
    Wait(std::sync::Arc<Shifted>, Duration),
}

#[derive(Default)]
struct Shifted {
    by: std::sync::Mutex<Duration>,
}

impl Shifted {
    fn advance(&self, by: Duration) {
        let mut shift = self.by.lock().unwrap();
        *shift += by;
    }
}

impl wisp_core::clock::Clock for Shifted {
    fn now(&self) -> jiff::Timestamp {
        let by = *self.by.lock().unwrap();
        jiff::Timestamp::now()
            .checked_add(jiff::SignedDuration::try_from(by).unwrap())
            .unwrap()
    }

    fn local_zone(&self) -> Option<jiff::tz::TimeZone> {
        Some(jiff::tz::TimeZone::UTC)
    }
}

async fn until(
    events: &mut mpsc::Receiver<AgentEvent>,
    seen: &mut Vec<AgentEvent>,
    done: impl Fn(&AgentEvent) -> bool,
) {
    loop {
        let event = tokio::time::timeout(WAIT, events.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out; seen {seen:#?}"))
            .unwrap();
        let stop = done(&event);
        seen.push(event);
        if stop {
            return;
        }
    }
}

fn change(request: u64, model: Option<&str>, effort: Option<Effort>) -> ClientCommand {
    ClientCommand::ChangeSettings {
        request: RequestId(request),
        changes: ClientCommand::shortcut_changes(None, model.map(str::to_owned), effort),
        save: false,
    }
}

fn answers(event: &AgentEvent, request: u64) -> bool {
    match event {
        AgentEvent::SettingsChanged {
            answers: Some(r), ..
        } => *r == RequestId(request),
        AgentEvent::SettingsSnapshot(s) => s.answers == RequestId(request),
        _ => false,
    }
}

fn ended(event: &AgentEvent) -> bool {
    matches!(event, AgentEvent::TurnEnded { .. })
}

fn withdrawal(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::SettingsChanged {
            withdrawn: Some(_),
            ..
        }
    )
}

async fn run_steps(
    commands: mpsc::Sender<ClientCommand>,
    mut events: mpsc::Receiver<AgentEvent>,
    steps: Vec<Do>,
) -> Vec<AgentEvent> {
    let mut seen = vec![];
    for (n, step) in steps.into_iter().enumerate() {
        let id = format!("t-{n}");
        match step {
            Do::Send(text) => {
                commands.send(ClientCommand::send(&id, text)).await.unwrap();
                until(&mut events, &mut seen, ended).await;
            }
            Do::Compact(request) => {
                commands
                    .send(ClientCommand::Compact {
                        focus: None,
                        request: RequestId(request),
                    })
                    .await
                    .unwrap();
                until(&mut events, &mut seen, |e| {
                    matches!(e, AgentEvent::ContextChanged { .. })
                })
                .await;
            }
            Do::Change(request, model, effort) => {
                commands.send(change(request, model, effort)).await.unwrap();
                until(&mut events, &mut seen, |e| answers(e, request)).await;
            }
            Do::Describe(request) => {
                commands
                    .send(ClientCommand::DescribeSettings {
                        request: RequestId(request),
                    })
                    .await
                    .unwrap();
                until(&mut events, &mut seen, |e| answers(e, request)).await;
            }
            Do::ChangeOnStep(text, request, model, effort) => {
                commands.send(ClientCommand::send(&id, text)).await.unwrap();
                until(&mut events, &mut seen, |e| {
                    matches!(e, AgentEvent::StepStarted { .. })
                })
                .await;
                commands
                    .send(change(request, Some(model), effort))
                    .await
                    .unwrap();
                until(&mut events, &mut seen, ended).await;
            }
            Do::ChangesOnStep(text, request, changes) => {
                commands.send(ClientCommand::send(&id, text)).await.unwrap();
                until(&mut events, &mut seen, |e| {
                    matches!(e, AgentEvent::StepStarted { .. })
                })
                .await;
                let command = ClientCommand::ChangeSettings {
                    request: RequestId(request),
                    changes,
                    save: false,
                };
                commands.send(command).await.unwrap();
                until(&mut events, &mut seen, ended).await;
            }
            Do::Wait(clock, by) => clock.advance(by),
            Do::CancelSwitch(text) => {
                commands.send(ClientCommand::send(&id, text)).await.unwrap();
                until(&mut events, &mut seen, |e| {
                    matches!(
                        e,
                        AgentEvent::CompactionStarted {
                            trigger: CompactionTrigger::Switch,
                            ..
                        }
                    )
                })
                .await;
                commands.send(ClientCommand::Cancel).await.unwrap();
                until(&mut events, &mut seen, withdrawal).await;
            }
        }
    }
    drop(commands);
    while let Ok(Some(event)) =
        tokio::time::timeout(Duration::from_millis(200), events.recv()).await
    {
        seen.push(event);
    }
    seen
}

async fn live(h: &mut Harness, steps: Vec<Do>) -> Vec<AgentEvent> {
    let (commands, mut inbox) = mpsc::channel(8);
    let (sink, events) = mpsc::channel(4096);
    let driver = tokio::spawn(run_steps(commands, events, steps));
    h.agent.serve(&mut inbox, &sink).await;
    drop(sink);
    driver.await.unwrap()
}

async fn replayed(events: Vec<AgentEvent>, steps: Vec<Do>) -> Vec<AgentEvent> {
    let (commands, out) = replay::spawn(Script::new(events), Duration::from_millis(1));
    run_steps(commands, out, steps).await
}

struct Seen {
    notes: Vec<String>,
}

fn feed(h: &mut Harness, events: &[AgentEvent]) -> Seen {
    let mut notes = vec![];
    for e in events {
        match h.tracker.apply(e, &h.facts, Duration::ZERO, UNI) {
            Answer::Note(note) => notes.push(note),
            Answer::Notes(more) => notes.extend(more),
            Answer::Refused(note) => notes.push(note),
            Answer::Moved(moved, after) => {
                notes.push(
                    wisp_render::chrome::account::moved_body(
                        &moved,
                        &wisp_render::theme::glyphs::Glyphs::of(UNI),
                        400,
                    )
                    .join(" "),
                );
                notes.extend(after);
            }
            Answer::Hint(_) | Answer::Other => {}
        }
    }
    h.tracker.show(&mut h.facts);
    Seen { notes }
}

fn quote(events: &[AgentEvent]) -> Option<(u64, u64, SwitchCompactionCause)> {
    events.iter().find_map(|e| match e {
        AgentEvent::SettingsSnapshot(s) => s
            .switches
            .iter()
            .find(|q| q.target == SwitchTarget::Model(OPUS.to_owned()))
            .and_then(|q| match &q.consequence {
                Some(Consequence::CompactsBeforeSwitch {
                    estimate_micros,
                    head_tokens,
                    cause,
                }) => Some((*estimate_micros, *head_tokens, cause.clone())),
                _ => None,
            }),
        _ => None,
    })
}

#[allow(
    clippy::cast_precision_loss,
    reason = "an estimate in millionths of a dollar compared to the cent"
)]
fn usd(micros: u64) -> f64 {
    micros as f64 / 1_000_000.0
}

#[tokio::test]
async fn the_switchs_quote_prices_the_compaction_beside_a_rebuild_of_the_head() {
    let mut h = harness(NO_NATIVE, Reply::Compacts);
    let events = live(
        &mut h,
        vec![
            Do::Send("read the parser"),
            Do::Compact(1),
            Do::Send("now the lexer"),
            Do::Describe(2),
        ],
    )
    .await;
    let Some((micros, head_tokens, cause)) = quote(&events) else {
        panic!("no quote: {events:#?}");
    };
    assert_eq!(cause, SwitchCompactionCause::Convert);
    assert!(micros > 0, "{micros}");
    feed(&mut h, &events);
    let effect = h.tracker.model_effect(&h.facts, OPUS, Duration::ZERO);
    let Effect::Switch(rebuild, work) = effect else {
        panic!("{effect:?}");
    };
    assert_eq!(
        work,
        SwitchWork {
            usd: Some(usd(micros)),
            cause: SwitchCause::Convert,
            alone: false,
        }
    );
    let prefix = h.tracker.cache_size().prefix;
    let rates = h.facts.model_facts(OPUS).and_then(|m| m.rates);
    let head = Rebuild::estimate(
        CacheSize {
            prefix,
            total: prefix + head_tokens,
        },
        Scope::Exact,
        Alive {
            prefix: true,
            tail: true,
        },
        rates.as_ref(),
        h.facts.write_multipliers.at(h.tracker.cache_ttl()),
    );
    assert_eq!(rebuild, head);
    let text = effect.text(UNI);
    assert!(
        text.ends_with(&format!(
            "compacts before your next message, then switches (~{} \u{b7} converts the summary)",
            wisp_render::transcript::copy::usd(usd(micros))
        )),
        "{text}"
    );
    assert!(!text.contains("$0.00"), "{text}");
}

#[tokio::test]
async fn a_pick_under_another_account_shows_the_compaction_that_pair_needs() {
    let mut h = Harness::endpoints(
        &[("direct", Endpoint::Official), ("gw", Endpoint::Official)],
        "direct",
        SONNET_4_6,
        Effort::High,
        |request: &str, n| {
            if request.contains("\"compaction\":") {
                compaction(SONNET_4_6, Reply::Compacts)
            } else {
                answer(&super::model_of(request), n)
            }
        },
    );
    let models = ModelSettings::new(
        ModelTable::shipped().unwrap(),
        ConfigModels::parse("[models.\"claude-opus-5-5\"]\nnative_compaction = false\n").unwrap(),
    );
    h.agent = h.agent.with_model_settings(models);
    let events = live(
        &mut h,
        vec![
            Do::Send("read the parser"),
            Do::Compact(1),
            Do::Send("now the lexer"),
            Do::Describe(2),
        ],
    )
    .await;
    feed(&mut h, &events);
    let effect = h.tracker.pair_effect(&h.facts, "gw", OPUS, Duration::ZERO);
    assert!(
        matches!(
            effect,
            Effect::Switch(
                _,
                SwitchWork {
                    cause: SwitchCause::Convert,
                    ..
                }
            )
        ),
        "{effect:?}"
    );
}

#[tokio::test]
async fn a_quoted_switch_names_its_estimate_in_the_note_and_again_when_it_runs() {
    let mut h = harness(NO_NATIVE, Reply::Compacts);
    let events = live(
        &mut h,
        vec![
            Do::Send("read the parser"),
            Do::Compact(1),
            Do::Send("now the lexer"),
            Do::Change(2, Some(OPUS), None),
            Do::Send("and the tests"),
        ],
    )
    .await;
    let seen = feed(&mut h, &events);
    let quoted = events.iter().find_map(|e| match e {
        AgentEvent::SettingsChanged {
            answers: Some(_),
            changes,
            ..
        } => changes.iter().find_map(|c| c.consequence.clone()),
        _ => None,
    });
    let Some(Consequence::CompactsBeforeSwitch {
        estimate_micros, ..
    }) = quoted
    else {
        panic!("{events:#?}");
    };
    let run = events.iter().find_map(|e| match e {
        AgentEvent::CompactionStarted {
            trigger: CompactionTrigger::Switch,
            switch: Some(s),
            ..
        } => Some(s.estimate_micros),
        _ => None,
    });
    let Some(run) = run else {
        panic!("{events:#?}");
    };
    let amount = |m| {
        wisp_protocol::marked_amount(&wisp_render::transcript::copy::usd(usd(m)), true, false)
            .to_string()
    };
    assert!(
        seen.notes.iter().any(|n| n.starts_with("Switching to ")),
        "{:#?}",
        seen.notes
    );
    let switched = seen
        .notes
        .iter()
        .find(|n| n.starts_with("compacts before"))
        .unwrap_or_else(|| panic!("{:#?}", seen.notes));
    assert!(
        switched.ends_with(&format!(
            "compacts before your next message, then switches ({} \u{b7} converts the summary)",
            amount(estimate_micros)
        )),
        "{switched}"
    );
    let running = format!(
        "Compacting before switching to {OPUS} \u{b7} {}",
        amount(run)
    );
    assert!(seen.notes.contains(&running), "{:#?}", seen.notes);
    assert_eq!(h.tracker.session().model, OPUS);
}

fn tracked(h: &Harness) -> (String, String, Option<Effort>, String, Option<Effort>) {
    (
        h.tracker.session().model.clone(),
        h.tracker.next().model.clone(),
        h.tracker.next().effort,
        h.facts.model.clone(),
        h.tracker.session().effort,
    )
}

#[tokio::test]
async fn a_cancelled_switch_compaction_withdraws_the_switch_and_the_replay_agrees() {
    let mut h = harness(NO_NATIVE, Reply::Stalls);
    let events = live(
        &mut h,
        vec![
            Do::Change(1, None, Some(Effort::Medium)),
            Do::Send("read the parser"),
            Do::Compact(2),
            Do::ChangeOnStep("now the lexer", 3, OPUS, Some(Effort::Xhigh)),
            Do::CancelSwitch("and the tests"),
        ],
    )
    .await;
    let ends: Vec<&AgentEvent> = events
        .iter()
        .filter(|e| ended(e) || withdrawal(e))
        .collect();
    assert!(
        matches!(
            ends.as_slice(),
            [
                _,
                _,
                AgentEvent::TurnEnded {
                    outcome: TurnOutcome::Cancelled { .. },
                    ..
                },
                AgentEvent::SettingsChanged {
                    withdrawn: Some(Withdrawal::SwitchCompactionCancelled),
                    ..
                }
            ]
        ),
        "{ends:#?}"
    );
    let seen = feed(&mut h, &events);
    assert_eq!(h.tracker.session().model, SONNET_4_6);
    assert_eq!(h.tracker.next().model, SONNET_4_6);
    assert!(
        seen.notes.iter().any(|n| n.starts_with(&format!(
            "Switch to {OPUS} withdrawn: the compaction it needed was cancelled"
        ))),
        "{:#?}",
        seen.notes
    );
    assert!(
        seen.notes.iter().any(|n| n.contains(&format!(
            "effort back to medium: {} doesn't take xhigh",
            wisp_render::chrome::short_model(SONNET_4_6)
        ))),
        "{:#?}",
        seen.notes
    );
    assert_eq!(h.tracker.next().effort, Some(Effort::Medium));

    let mut r = harness(NO_NATIVE, Reply::Stalls);
    let replay_steps = vec![
        Do::Send("read the parser"),
        Do::Compact(2),
        Do::Send("now the lexer"),
        Do::Send("and the tests"),
    ];
    let again = replayed(events, replay_steps).await;
    let replayed_seen = feed(&mut r, &again);
    assert_eq!(tracked(&r), tracked(&h));
    assert_eq!(r.facts.next, h.facts.next);
    assert_eq!(r.tracker.cache_ttl(), h.tracker.cache_ttl());
    assert_eq!(replayed_seen.notes, seen.notes);
}

async fn a_failed_switch_compaction_converts(reply: Reply) {
    let mut h = harness(NO_NATIVE, reply);
    let events = live(
        &mut h,
        vec![
            Do::Send("read the parser"),
            Do::Compact(1),
            Do::ChangeOnStep("now the lexer", 2, OPUS, None),
            Do::Send("and the tests"),
        ],
    )
    .await;
    let seen = feed(&mut h, &events);
    let expected = format!(
        "Couldn't compact before switching to {OPUS}; converted the summary instead \u{b7} turn 2 kept only as your messages and the ledger"
    );
    assert!(
        seen.notes.contains(&expected),
        "{:#?}\n{events:#?}",
        seen.notes
    );
    assert_eq!(h.tracker.session().model, OPUS);

    let mut r = harness(NO_NATIVE, reply);
    let again = replayed(
        events,
        vec![
            Do::Send("read the parser"),
            Do::Compact(1),
            Do::Send("now the lexer"),
            Do::Send("and the tests"),
        ],
    )
    .await;
    let replayed_seen = feed(&mut r, &again);
    assert_eq!(tracked(&r), tracked(&h));
    assert_eq!(replayed_seen.notes, seen.notes);
}

#[tokio::test]
async fn a_refused_switch_compaction_converts_and_says_which_turns_it_lost() {
    a_failed_switch_compaction_converts(Reply::Refuses).await;
}

#[tokio::test]
async fn a_switch_compaction_the_api_errors_on_converts_the_same_way() {
    a_failed_switch_compaction_converts(Reply::Errors).await;
}

#[tokio::test]
async fn a_switch_compaction_that_ends_with_no_text_converts_the_same_way() {
    a_failed_switch_compaction_converts(Reply::Empty).await;
}

fn kinds_from_switch(events: &[AgentEvent]) -> Vec<String> {
    let from = events
        .iter()
        .position(|e| {
            matches!(
                e,
                AgentEvent::CompactionStarted {
                    trigger: CompactionTrigger::Switch,
                    ..
                }
            )
        })
        .unwrap_or(0);
    events[from..]
        .iter()
        .filter(|e| !matches!(e, AgentEvent::HookRan { .. }))
        .map(|e| {
            let text = format!("{e:?}");
            text.split([' ', '(', '{']).next().unwrap_or("").to_owned()
        })
        .collect()
}

#[tokio::test]
async fn esc_in_a_replay_restores_the_effort_set_between_turns_like_a_live_cancel() {
    let mut a = harness(NO_NATIVE, Reply::Compacts);
    let recorded = live(
        &mut a,
        vec![
            Do::Change(7, None, Some(Effort::Medium)),
            Do::Send("read the parser"),
            Do::Compact(1),
            Do::ChangeOnStep("now the lexer", 2, OPUS, None),
            Do::Send("and the tests"),
        ],
    )
    .await;
    let mut b = harness(NO_NATIVE, Reply::Stalls);
    let cancelled = live(
        &mut b,
        vec![
            Do::Change(7, None, Some(Effort::Medium)),
            Do::Send("read the parser"),
            Do::Compact(1),
            Do::ChangeOnStep("now the lexer", 2, OPUS, None),
            Do::CancelSwitch("and the tests"),
        ],
    )
    .await;
    let settings = replay::Settings {
        endpoint: a.tracker.session().endpoint.clone(),
        model: SONNET_4_6.to_owned(),
        effort: Effort::High,
        efforts: crate::tui::replaying::model_efforts(&wisp_core::settings::shipped_models()),
        ..replay::Settings::default()
    };
    let script = Script::new(recorded).with_settings(settings);
    let (rc, ro) = replay::spawn(script, Duration::from_millis(1));
    let again = run_steps(
        rc,
        ro,
        vec![
            Do::Send("read the parser"),
            Do::Compact(1),
            Do::Send("now the lexer"),
            Do::CancelSwitch("and the tests"),
        ],
    )
    .await;
    let live_seen = feed(&mut b, &cancelled);
    let mut r = harness(NO_NATIVE, Reply::Stalls);
    let replay_seen = feed(&mut r, &again);
    assert_eq!(tracked(&r), tracked(&b));
    assert_eq!(replay_seen.notes, live_seen.notes);
    assert_eq!(kinds_from_switch(&again), kinds_from_switch(&cancelled));
}

#[tokio::test]
async fn esc_in_a_replay_sends_an_xhigh_effort_back_like_a_live_cancel() {
    let steps = || {
        vec![
            Do::Change(7, None, Some(Effort::Medium)),
            Do::Send("read the parser"),
            Do::Compact(1),
            Do::ChangeOnStep("now the lexer", 2, OPUS, Some(Effort::Xhigh)),
        ]
    };
    let mut a = harness(NO_NATIVE, Reply::Compacts);
    let mut recording = steps();
    recording.push(Do::Send("and the tests"));
    let recorded = live(&mut a, recording).await;
    let mut b = harness(NO_NATIVE, Reply::Stalls);
    let mut cancelling = steps();
    cancelling.push(Do::CancelSwitch("and the tests"));
    let cancelled = live(&mut b, cancelling).await;
    let settings = replay::Settings {
        endpoint: a.tracker.session().endpoint.clone(),
        model: SONNET_4_6.to_owned(),
        effort: Effort::High,
        efforts: crate::tui::replaying::model_efforts(&wisp_core::settings::shipped_models()),
        ..replay::Settings::default()
    };
    let script = Script::new(recorded).with_settings(settings);
    let (rc, ro) = replay::spawn(script, Duration::from_millis(1));
    let again = run_steps(
        rc,
        ro,
        vec![
            Do::Send("read the parser"),
            Do::Compact(1),
            Do::Send("now the lexer"),
            Do::CancelSwitch("and the tests"),
        ],
    )
    .await;
    let live_seen = feed(&mut b, &cancelled);
    let mut r = harness(NO_NATIVE, Reply::Stalls);
    let replay_seen = feed(&mut r, &again);
    assert!(
        live_seen
            .notes
            .iter()
            .any(|n| n.contains("effort back to medium")),
        "{:#?}",
        live_seen.notes
    );
    assert_eq!(tracked(&r), tracked(&b));
    assert_eq!(replay_seen.notes, live_seen.notes);
}

fn opus_and_five_minutes() -> Vec<SettingChange> {
    let mut changes = ClientCommand::shortcut_changes(None, Some(OPUS.to_owned()), None);
    changes.extend(SettingChange::named(
        "cache_ttl",
        SettingValue::Text("5m".to_owned()),
    ));
    wisp_protocol::settings::sorted_changes(changes)
}

#[tokio::test]
async fn a_cancel_keeps_a_pending_cache_lifetime_and_the_replay_agrees() {
    let mut h = harness(NO_NATIVE, Reply::Stalls);
    let events = live(
        &mut h,
        vec![
            Do::Send("read the parser"),
            Do::Compact(1),
            Do::ChangesOnStep("now the lexer", 2, opus_and_five_minutes()),
            Do::CancelSwitch("and the tests"),
        ],
    )
    .await;
    let withdrawn = events.iter().find_map(|e| match e {
        AgentEvent::SettingsChanged {
            withdrawn: Some(_),
            cache,
            changes,
            ..
        } => Some((cache.clone(), changes.clone())),
        _ => None,
    });
    let Some((cache, changes)) = withdrawn else {
        panic!("{events:#?}");
    };
    assert_eq!(
        cache,
        wisp_protocol::CacheEffect::Rebuilt {
            reason: wisp_protocol::RebuildReason::CacheTtlChange
        }
    );
    assert!(
        changes.iter().any(|c| c.target.to_string() == "cache_ttl"
            && c.value == Some(SettingValue::Text("5m".to_owned()))),
        "{changes:#?}"
    );
    let seen = feed(&mut h, &events);
    assert_eq!(h.tracker.session().model, SONNET_4_6);
    assert_eq!(h.tracker.cache_ttl(), wisp_protocol::CacheTtl::OneHour);
    assert!(
        !seen.notes.iter().any(|n| n.starts_with("Cache lifetime")
            && n.contains("5m")
            && seen.notes.iter().position(|m| m == n)
                > seen.notes.iter().position(|m| m.starts_with("Switch to"))),
        "{:#?}",
        seen.notes
    );

    let mut r = harness(NO_NATIVE, Reply::Stalls);
    let again = replayed(
        events,
        vec![
            Do::Send("read the parser"),
            Do::Compact(1),
            Do::Send("now the lexer"),
            Do::Send("and the tests"),
        ],
    )
    .await;
    let replayed_seen = feed(&mut r, &again);
    assert_eq!(tracked(&r), tracked(&h));
    assert_eq!(r.tracker.cache_ttl(), h.tracker.cache_ttl());
    assert_eq!(r.facts.next, h.facts.next);
    assert_eq!(replayed_seen.notes, seen.notes);
}

const FILL: u64 = 600_000;

async fn quoted_at_fill(wait: Option<Duration>) -> (u64, u64, Harness) {
    let clock = std::sync::Arc::new(Shifted::default());
    let models = ModelSettings::new(
        ModelTable::shipped().unwrap(),
        ConfigModels::parse(NO_NATIVE).unwrap(),
    );
    let (provider, _) = super::provider(
        Endpoint::Official,
        std::sync::Arc::new(|request: &str, n| {
            if request.contains("\"compaction\":") {
                compaction(SONNET_4_6, Reply::Compacts)
            } else {
                filled_answer(&super::model_of(request), n, FILL)
            }
        }),
    );
    let shifted = std::sync::Arc::clone(&clock);
    let mut h = Harness::build(
        provider,
        Endpoint::Official,
        SONNET_4_6,
        Effort::High,
        |a| a.with_model_settings(models).with_clock(shifted),
    );
    let mut steps = vec![
        Do::Send("read the parser"),
        Do::Compact(1),
        Do::Send("now the lexer"),
    ];
    steps.extend(wait.map(|by| Do::Wait(clock, by)));
    steps.push(Do::Describe(2));
    let events = live(&mut h, steps).await;
    let Some((micros, head_tokens, cause)) = quote(&events) else {
        panic!("no quote: {events:#?}");
    };
    assert_eq!(cause, SwitchCompactionCause::Convert);
    feed(&mut h, &events);
    (micros, head_tokens, h)
}

fn badge_rebuild(h: &Harness, head_tokens: u64, at: Duration) -> (Rebuild, Rebuild) {
    let Effect::Switch(rebuild, _) = h.tracker.model_effect(&h.facts, OPUS, at) else {
        panic!("not a quoted switch");
    };
    let prefix = h.tracker.cache_size().prefix;
    let head = Rebuild::estimate(
        CacheSize {
            prefix,
            total: prefix + head_tokens,
        },
        Scope::Exact,
        Alive {
            prefix: true,
            tail: true,
        },
        h.facts.model_facts(OPUS).and_then(|m| m.rates).as_ref(),
        h.facts.write_multipliers.at(h.tracker.cache_ttl()),
    );
    (rebuild, head)
}

#[tokio::test]
async fn at_600k_a_switch_prices_the_read_warm_and_cold_and_rebuilds_only_the_head() {
    let (warm, warm_head, h) = quoted_at_fill(None).await;
    assert!(
        h.tracker.cache_size().total >= FILL,
        "{:?}",
        h.tracker.cache_size()
    );
    assert!(warm_head < 50_000, "{warm_head}");
    let (rebuild, head) = badge_rebuild(&h, warm_head, Duration::ZERO);
    assert_eq!(rebuild, head);

    let two_hours = Duration::from_secs(7_200);
    let (cold, cold_head, h) = quoted_at_fill(Some(two_hours)).await;
    assert!(cold > warm, "cold {cold} warm {warm}");
    let (rebuild, head) = badge_rebuild(&h, cold_head, two_hours);
    assert_eq!(rebuild, head);
    assert!(
        matches!(rebuild, Rebuild::Usd(usd) if usd > 0.0),
        "{rebuild:?}"
    );
}

#[tokio::test]
async fn the_picker_and_the_kernel_land_on_the_accounts_remembered_pair() {
    use wisp_core::settings::host::StartLayers;
    use wisp_core::settings::remembered::RememberedStore;
    let dir = tempfile::tempdir().unwrap();
    let mut h = Harness::endpoints(
        &[("api", Endpoint::Official), ("gw", Endpoint::Custom)],
        "gw",
        OPUS,
        Effort::Medium,
        |_, n| answer(OPUS, n),
    );
    h.agent = h.agent.with_start_layers(StartLayers {
        remembered: RememberedStore::shared(dir.path().to_path_buf()),
        ..StartLayers::default()
    });
    let events = live(
        &mut h,
        vec![
            Do::ChangesOnStep(
                "seed",
                1,
                ClientCommand::shortcut_changes(
                    Some("api".into()),
                    Some(super::SONNET.into()),
                    Some(Effort::High),
                ),
            ),
            Do::Describe(2),
        ],
    )
    .await;
    feed(&mut h, &events);
    for (i, account, model, effort) in [
        (3, "gw", OPUS, Effort::Medium),
        (5, "api", super::SONNET, Effort::High),
    ] {
        let held = h.tracker.remembered(account).unwrap().clone();
        assert_eq!((held.model.as_str(), held.effort), (model, effort));
        let wisp_tui::app::picker::Step::Done(Picked::Settings {
            endpoint,
            model: picked_model,
            effort: picked_effort,
        }) = Picker::account_step(account, &h.facts, &h.tracker)
        else {
            panic!("expected the account pick")
        };
        assert_eq!((picked_model, picked_effort), (None, None));
        let events = h
            .turn(vec![ClientCommand::shortcut(
                RequestId(i),
                endpoint,
                None,
                None,
            )])
            .await;
        assert!(events.iter().any(|e| matches!(e, AgentEvent::SettingsChanged { endpoint, model: landed, effort: level, answers: Some(r), .. } if endpoint == account && landed == model && *level == effort && r.0 == i)));
        let quoted = live(&mut h, vec![Do::Describe(i + 1)]).await;
        feed(&mut h, &quoted);
        assert!(h.tracker.remembered(account).is_none());
    }
}
