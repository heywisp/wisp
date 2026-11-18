use wisp_protocol::settings::{AccountModel, KeyVariable};
use wisp_protocol::{
    CacheEffect, ErrorDetail, ErrorKind, FailurePhase, FailureSource, RebuildReason, Usage,
};
use wisp_render::chrome::Facts;

use super::*;
use crate::app::fixture::{ended, endpoint_facts, four_accounts, limit, s, unglued, with_levels};
use crate::events::{Changed, End, Ending, outcome, settings_changed};
use crate::shared::offer;

const UNI: GlyphSet = GlyphSet::Unicode;
const OPUS: &str = "claude-opus-5-5";

fn warmed(facts: &Facts) -> Tracker {
    let mut tracker = Tracker::new(facts);
    tracker.apply(&AgentEvent::turn_started(1), facts, s(0), UNI);
    tracker.apply(
        &AgentEvent::StepEnded {
            step: 1,
            usage: Some(Usage {
                input_tokens: 100,
                cache_read_input_tokens: 189_900,
                ..Usage::default()
            }),
            cost_usd: None,
            lower_bound: false,
        },
        facts,
        s(10),
        UNI,
    );
    tracker
}

fn error(offer: RetryOffer, committed: u32) -> TurnOutcome {
    outcome(End {
        ending: Ending::Error(ErrorKind::Server),
        message: "Upstream provider stream was interrupted.".into(),
        committed_steps: committed,
        attempts: 6,
        gave_up: Some(GaveUp::StepLimit),
        offer: Some(offer),
        ..End::default()
    })
}

fn of_kind(kind: ErrorKind, offer: RetryOffer) -> TurnOutcome {
    let mut outcome = error(offer, 0);
    if let TurnOutcome::Error { kind: k, .. } = &mut outcome {
        *k = kind;
    }
    outcome
}

fn limited(offer: RetryOffer, resets_at: i64) -> TurnOutcome {
    let mut outcome = of_kind(ErrorKind::Billing, offer);
    if let TurnOutcome::Error { detail: d, .. } = &mut outcome {
        *d = Some(limit(Some(resets_at)));
    }
    outcome
}

struct Rig {
    facts: Facts,
    tracker: Tracker,
    choice: Choice,
    at: Duration,
    wall: i64,
}

impl Rig {
    fn new(outcome: &TurnOutcome) -> Rig {
        Rig::on(endpoint_facts(), outcome)
    }

    fn on(facts: Facts, outcome: &TurnOutcome) -> Rig {
        let mut tracker = warmed(&facts);
        tracker.apply(&ended(outcome.clone()), &facts, s(11), UNI);
        let mut choice = Choice::default();
        choice.event(
            &AgentEvent::turn_started(1),
            &Ctx {
                facts: &facts,
                tracker: &tracker,
                at: s(11),
                wall: 0,
                set: UNI,
            },
        );
        let failed = Failed::of(choice.turn(), outcome, tracker.session());
        if let Some(failed) = failed {
            choice.offered(failed, 1, None);
        }
        Rig {
            facts,
            tracker,
            choice,
            at: s(12),
            wall: 0,
        }
    }

    fn cx(&self) -> Ctx<'_> {
        Ctx {
            facts: &self.facts,
            tracker: &self.tracker,
            at: self.at,
            wall: self.wall,
            set: UNI,
        }
    }

    fn apply(&mut self, event: &AgentEvent) -> Seen {
        self.tracker.apply(event, &self.facts, self.at, UNI);
        let cx = Ctx {
            facts: &self.facts,
            tracker: &self.tracker,
            at: self.at,
            wall: self.wall,
            set: UNI,
        };
        self.choice.event(event, &cx)
    }

    fn rows(&self) -> (Vec<(String, String)>, usize) {
        match self.choice.shown(&self.cx(), false) {
            Some(Shown::Open { rows, focus, .. }) => {
                (rows.into_iter().map(|r| (r.label, r.text)).collect(), focus)
            }
            other => panic!("{other:?}"),
        }
    }

    fn labels(&self) -> Vec<String> {
        self.rows().0.into_iter().map(|(label, _)| label).collect()
    }

    fn account(&mut self, credential: Credential, key_var: Option<KeyVariable>) {
        let failed = self.choice.failed().map(|f| f.endpoint.clone());
        for e in &mut self.facts.endpoints {
            if Some(&e.name) == failed.as_ref() {
                e.credential = Some(credential);
                e.key_var = key_var.clone();
            }
        }
    }

    fn direct_serves_only(&mut self, model: &str) {
        let endpoint = &mut self.facts.endpoints[0];
        endpoint.models = vec![AccountModel {
            id: model.into(),
            listed: Some(0),
        }];
        endpoint.complete = true;
        endpoint.default_model = Some(model.into());
    }

    fn texts(&self) -> Vec<String> {
        self.rows().0.into_iter().map(|(_, text)| text).collect()
    }

    fn key(&mut self, code: KeyCode) -> Option<Act> {
        self.press(Key::plain(code))
    }

    fn press(&mut self, key: Key) -> Option<Act> {
        let cx = Ctx {
            facts: &self.facts,
            tracker: &self.tracker,
            at: self.at,
            wall: self.wall,
            set: UNI,
        };
        self.choice.key(key, &cx)
    }

    fn menu(&self) -> Menu {
        self.choice.menu(&self.cx()).unwrap()
    }

    fn switch_pending(&mut self, endpoint: &str, effort: Effort) {
        self.apply(&settings_changed(Changed {
            endpoint: endpoint.to_owned(),
            model: OPUS.to_owned(),
            effort,
            cache: CacheEffect::Rebuilt {
                reason: RebuildReason::EndpointSwitch,
            },
            ..Changed::default()
        }));
    }
}

#[test]
fn both_rows_show_with_the_focus_the_offer_names() {
    let rig = Rig::new(&error(offer(true, true, RetryFocus::Retry), 18));
    let (rows, focus) = rig.rows();
    assert_eq!(
        rows,
        [
            (
                "Retry".to_owned(),
                "from step 19 · cache warm ~$0.076".to_owned()
            ),
            (
                "Switch account".to_owned(),
                "pick one, then retry · cache rebuild".to_owned()
            ),
        ]
    );
    assert_eq!(focus, 0);
    let rig = Rig::new(&error(offer(false, true, RetryFocus::Switch), 0));
    assert_eq!(rig.rows().0.len(), 1);
    assert_eq!(rig.rows().0[0].0, "Switch account");
    let rig = Rig::new(&error(offer(true, false, RetryFocus::Retry), 0));
    assert_eq!(
        rig.texts(),
        ["sends your message again · cache warm ~$0.076"]
    );
    let mut rig = Rig::new(&of_kind(
        ErrorKind::Auth,
        offer(true, true, RetryFocus::Retry),
    ));
    rig.account(Credential::AnthropicPlan, None);
    assert_eq!(
        rig.labels(),
        ["Sign in again, then retry", "Switch account"]
    );
    assert_eq!(rig.rows().1, 0);
    rig.account(Credential::ApiKey, None);
    assert_eq!(
        rig.labels(),
        ["Enter a new key, then retry", "Switch account"]
    );
    let var = KeyVariable {
        name: "WORK_KEY".into(),
        in_keys_file: false,
    };
    rig.account(Credential::ApiKey, Some(var));
    assert_eq!(rig.labels(), ["Retry", "Switch account"]);
    let rig = Rig::new(&limited(offer(true, true, RetryFocus::Switch), 100));
    assert_eq!(rig.labels(), ["Switch account"]);
    assert_eq!(rig.rows().1, 0);
    let mut stopped = limited(offer(true, true, RetryFocus::Retry), 100);
    if let TurnOutcome::Error {
        kind,
        attempts,
        gave_up,
        detail: Some(detail),
        ..
    } = &mut stopped
    {
        *kind = ErrorKind::RateLimit;
        *attempts = 1;
        *gave_up = None;
        detail.retry = RetryClass::After { ms: 100_000 };
    }
    let mut rig = Rig::new(&stopped);
    assert_eq!(rig.labels(), ["Retry", "Switch account"]);
    assert_eq!(rig.key(KeyCode::Enter), Some(Act::Retry { endpoint: None }));
    for others in [false, true] {
        let mut exhausted = limited(
            offer(
                true,
                others,
                if others {
                    RetryFocus::Switch
                } else {
                    RetryFocus::Retry
                },
            ),
            508_200,
        );
        if let TurnOutcome::Error {
            kind,
            gave_up,
            detail: Some(detail),
            ..
        } = &mut exhausted
        {
            *kind = ErrorKind::RateLimit;
            *gave_up = Some(GaveUp::RetryAfterTooLong);
            detail.retry = RetryClass::After { ms: 508_200_000 };
        }
        let mut rig = Rig::new(&exhausted);
        assert_eq!(
            rig.labels(),
            [
                "Retry",
                if others {
                    "Switch account"
                } else {
                    "+ Sign in to another account…"
                }
            ]
        );
        assert_eq!(rig.rows().1, usize::from(others));
        assert!(rig.texts()[0].contains("cache warm"));
        assert!(!rig.texts()[0].contains("asked to wait"));
        if others {
            rig.key(KeyCode::Up);
        }
        assert_eq!(rig.key(KeyCode::Enter), Some(Act::Retry { endpoint: None }));
    }
}

#[test]
fn no_offer_means_no_choice() {
    let mut outcome = error(offer(true, true, RetryFocus::Retry), 1);
    if let TurnOutcome::Error { offer, .. } = &mut outcome {
        *offer = None;
    }
    let rig = Rig::new(&outcome);
    assert!(rig.choice.failed().is_none());
    assert_eq!(rig.choice.shown(&rig.cx(), false), None);
}

#[test]
fn the_retry_price_reads_checking_until_the_snapshot_answers() {
    let mut rig = Rig::new(&error(offer(true, true, RetryFocus::Retry), 18));
    let failed = rig.choice.failed().cloned().unwrap();
    rig.choice.offered(failed, 1, Some(RequestId(4)));
    assert_eq!(rig.texts()[0], "from step 19 · checking…");
    rig.choice.answered(RequestId(3));
    assert_eq!(rig.texts()[0], "from step 19 · checking…");
    rig.choice.answered(RequestId(4));
    assert_eq!(rig.texts()[0], "from step 19 · cache warm ~$0.076");
}

#[test]
fn after_the_ttl_the_retry_row_prices_the_rewrite() {
    let mut rig = Rig::new(&error(offer(true, true, RetryFocus::Retry), 18));
    rig.at = s(10 + 3601);
    assert_eq!(
        rig.texts()[0],
        "from step 19 · cache may have expired ~$1.52"
    );
}

#[test]
fn a_pending_endpoint_is_named_with_its_rebuild() {
    let mut rig = Rig::new(&error(offer(true, true, RetryFocus::Retry), 18));
    rig.switch_pending("direct", Effort::Medium);
    assert_eq!(rig.texts()[0], "on direct · cache rebuild ~$1.52");
    rig.direct_serves_only("claude-sonnet-5-5");
    assert_eq!(rig.texts()[0], "on direct \u{b7} cache rebuild ~$0.76");
    let mut rig = Rig::new(&error(offer(true, false, RetryFocus::Retry), 18));
    rig.switch_pending("direct", Effort::Medium);
    assert!(rig.choice.shown(&rig.cx(), false).is_none());
}

#[test]
fn an_unpriced_endpoint_and_a_long_wait_say_so() {
    let mut rig = Rig::new(&error(offer(true, true, RetryFocus::Retry), 18));
    for e in &mut rig.facts.endpoints {
        e.priced = false;
    }
    assert_eq!(rig.texts()[0], "from step 19 · cache warm (no prices)");
    let outcome = outcome(End {
        ending: Ending::Error(ErrorKind::RateLimit),
        message: "slow down".into(),
        committed_steps: 2,
        detail: Some(ErrorDetail::new(
            FailurePhase::Response,
            FailureSource::Endpoint,
            RetryClass::After { ms: 300_000 },
        )),
        attempts: 1,
        gave_up: Some(GaveUp::RetryAfterTooLong),
        offer: Some(offer(true, true, RetryFocus::Retry)),
        ..End::default()
    });
    let rig = Rig::new(&outcome);
    assert_eq!(rig.texts()[0], "from step 3 · asked to wait 5m 00s");
}

#[test]
fn keys_move_act_and_dismiss() {
    let mut rig = Rig::new(&error(offer(true, true, RetryFocus::Retry), 18));
    assert_eq!(rig.key(KeyCode::Char('x')), None);
    assert_eq!(rig.key(KeyCode::Down), Some(Act::Stay));
    assert_eq!(rig.rows().1, 1);
    assert_eq!(rig.key(KeyCode::Tab), Some(Act::Stay));
    assert_eq!(rig.rows().1, 0);
    assert_eq!(rig.key(KeyCode::Up), Some(Act::Stay));
    assert_eq!(rig.rows().1, 1);
    assert_eq!(rig.key(KeyCode::Enter), Some(Act::Pick));
    rig.choice.picking(None);
    assert!(rig.choice.is_picking());
    assert_eq!(rig.key(KeyCode::Char('x')), Some(Act::Stay));
    assert_eq!(rig.key(KeyCode::Esc), Some(Act::Stay));
    assert!(!rig.choice.is_picking());
    assert!(rig.choice.is_open());
    assert_eq!(rig.key(KeyCode::Up), Some(Act::Stay));
    assert_eq!(rig.key(KeyCode::Enter), Some(Act::Retry { endpoint: None }));
    assert_eq!(rig.key(KeyCode::Esc), Some(Act::Dismissed));
    assert!(!rig.choice.is_open());
    assert_eq!(rig.key(KeyCode::Enter), None);
    let mut rig = Rig::new(&of_kind(
        ErrorKind::Auth,
        offer(true, true, RetryFocus::Retry),
    ));
    rig.account(Credential::AnthropicPlan, None);
    let manifest = Some("manifest".to_owned());
    assert_eq!(
        rig.key(KeyCode::Enter),
        Some(Act::SignIn { account: manifest })
    );
    rig.account(Credential::ApiKey, None);
    assert_eq!(
        rig.key(KeyCode::Enter),
        Some(Act::NewKey {
            account: "manifest".to_owned()
        })
    );
    let mut rig = Rig::new(&limited(offer(true, false, RetryFocus::Retry), 100));
    rig.wall = 99;
    assert_eq!(rig.labels(), ["+ Sign in to another account\u{2026}"]);
    assert_eq!(rig.key(KeyCode::Enter), Some(Act::SignIn { account: None }));
    rig.wall = 100;
    assert_eq!(
        rig.labels(),
        ["Retry", "+ Sign in to another account\u{2026}"]
    );
    let mut exhausted = limited(offer(true, false, RetryFocus::Retry), 100);
    if let TurnOutcome::Error {
        gave_up,
        detail: Some(detail),
        ..
    } = &mut exhausted
    {
        *gave_up = Some(GaveUp::RetryAfterTooLong);
        detail.retry = RetryClass::After { ms: 100_000 };
    }
    let mut ready = Rig::new(&exhausted);
    ready.wall = 99;
    assert_eq!(
        ready.key(KeyCode::Enter),
        Some(Act::Retry { endpoint: None })
    );
    let mut rig = Rig::new(&error(offer(true, true, RetryFocus::Switch), 0));
    rig.choice.picking(None);
    for e in &mut rig.facts.endpoints {
        e.credential = Some(Credential::ApiKey);
    }
    rig.account(Credential::AnthropicPlan, None);
    assert_eq!(rig.menu().selected, None);
    rig.key(KeyCode::Down);
    assert_eq!(rig.menu().selected, Some(0));
    rig.choice.picking(None);
    rig.key(KeyCode::Up);
    assert_eq!(rig.menu().selected, Some(1));
    rig.facts.endpoints.retain(|e| e.name == "manifest");
    rig.choice.picking(None);
    assert_eq!(rig.key(KeyCode::Down), Some(Act::Stay));
    assert_eq!(rig.menu().selected, None);
}

#[test]
fn the_retry_picker_prices_each_endpoint_and_preselects_the_first_other() {
    let mut rig = Rig::new(&error(offer(true, true, RetryFocus::Switch), 18));
    rig.choice.picking(Some(RequestId(9)));
    let menu = rig.menu();
    let effects: Vec<&str> = menu.choices.iter().map(|c| c.effect.as_str()).collect();
    assert_eq!(
        effects,
        [
            "opus-5-5 medium \u{b7} checking\u{2026}",
            "opus-5-5 medium \u{b7} checking\u{2026}",
            "opus-5-5 medium \u{b7} checking\u{2026}",
        ]
    );
    rig.choice.answered(RequestId(9));
    let menu = rig.menu();
    let rows: Vec<(String, &str, bool, bool)> = menu
        .choices
        .iter()
        .map(|c| {
            (
                unglued(c.label.trim_start()),
                c.effect.as_str(),
                c.enabled,
                c.current,
            )
        })
        .collect();
    let rows: Vec<(&str, &str, bool, bool)> = rows
        .iter()
        .map(|(l, e, a, b)| (l.as_str(), *e, *a, *b))
        .collect();
    assert_eq!(
        rows,
        [
            (
                "direct",
                "opus-5-5 medium \u{b7} cache rebuild ~$1.52",
                true,
                false
            ),
            (
                "gateway",
                "opus-5-5 medium \u{b7} cache rebuild ~$1.52",
                true,
                false
            ),
            (
                "work",
                "opus-5-5 medium \u{b7} cache rebuild, no prices",
                false,
                false
            ),
        ]
    );
    assert_eq!(menu.selected, Some(0));
    assert_eq!(rig.key(KeyCode::Down), Some(Act::Stay));
    assert_eq!(rig.menu().selected, Some(1));
    assert_eq!(
        rig.key(KeyCode::Enter),
        Some(Act::Retry {
            endpoint: Some("gateway".to_owned())
        })
    );
    rig.at = s(10 + 3601);
    assert_eq!(
        rig.menu().choices[0].effect,
        "opus-5-5 medium \u{b7} cache rebuild ~$1.52"
    );
    rig.direct_serves_only("claude-sonnet-5-5");
    assert_eq!(
        rig.menu().choices[0].effect,
        "sonnet-5-5 medium \u{b7} cache rebuild ~$0.76"
    );
    let keys = |rig: &Rig| {
        let dock = rig.choice.dock(&rig.cx());
        dock.keys.iter().map(KeyHint::text).collect::<Vec<_>>()
    };
    let mut suggest = offer(true, true, RetryFocus::Switch);
    suggest.suggest = Some("claude-api".to_owned());
    let mut rig = Rig::on(four_accounts(), &error(suggest, 0));
    rig.choice.picking(None);
    let names: Vec<String> = rig
        .menu()
        .choices
        .iter()
        .map(|c| unglued(c.label.trim_start()))
        .collect();
    assert_eq!(names, ["claude-work", "claude-api", "chatgpt"]);
    assert_eq!(rig.menu().selected, Some(0));
    assert_eq!(keys(&rig), ["enter retry", "esc back"]);
    rig.key(KeyCode::Down);
    assert_eq!(rig.menu().selected, Some(1));
    assert_eq!(
        keys(&rig),
        ["enter use the API", "\u{2191}\u{2193} move", "esc back"]
    );
    let retry = Some(Act::Retry {
        endpoint: Some("claude-api".to_owned()),
    });
    assert_eq!(rig.key(KeyCode::Char('y')), Some(Act::Stay));
    assert_eq!(rig.key(KeyCode::Enter), retry);
    rig.facts
        .endpoints
        .retain(|e| e.name == "claude-subscription" || e.name == "claude-api");
    rig.choice.picking(None);
    assert_eq!(rig.menu().selected, None);
    assert_eq!(keys(&rig), ["\u{2191}\u{2193} move", "esc back"]);
    assert_eq!(rig.key(KeyCode::Enter), Some(Act::Stay));
    assert_eq!(rig.key(KeyCode::Char('y')), Some(Act::Stay));
    let cx = Ctx {
        facts: &rig.facts,
        tracker: &rig.tracker,
        at: rig.at,
        wall: 0,
        set: UNI,
    };
    rig.choice.pick_on("claude-api", &cx);
    assert_eq!(rig.menu().selected, Some(0));
    assert_eq!(rig.key(KeyCode::Enter), retry);
}

#[test]
fn the_failed_account_is_left_out_and_the_suggestion_is_preselected() {
    let mut binding = offer(false, true, RetryFocus::Switch);
    binding.suggest = Some("gateway".to_owned());
    let mut rig = Rig::new(&error(binding, 0));
    rig.choice.picking(None);
    let menu = rig.menu();
    assert!(
        menu.choices.iter().all(|c| unglued(&c.label) != "manifest"),
        "{menu:?}"
    );
    assert_eq!(unglued(menu.choices[1].label.trim_start()), "gateway");
    assert_eq!(menu.selected, Some(1));
    rig.key(KeyCode::Down);
    assert_eq!(rig.menu().selected, Some(0));
}

#[test]
fn a_retry_that_starts_is_used_with_its_merged_note() {
    for (model, price, failed_effort, retried_effort) in [
        (OPUS, "1.52", Effort::Medium, Effort::Medium),
        ("claude-sonnet-5-5", "0.76", Effort::Medium, Effort::Medium),
        ("claude-sonnet-5-5", "0.76", Effort::Xhigh, Effort::High),
    ] {
        let mut facts = endpoint_facts();
        facts.effort = failed_effort.as_str().to_owned();
        with_levels(
            &mut facts,
            "claude-sonnet-5-5",
            &[Effort::Low, Effort::Medium, Effort::High],
            Effort::High,
        );
        let mut rig = Rig::on(facts, &error(offer(true, true, RetryFocus::Retry), 18));
        if model != OPUS {
            rig.direct_serves_only(model);
        }
        if failed_effort != retried_effort {
            rig.choice.picking(None);
            assert_eq!(
                rig.menu().choices[0].effect,
                "sonnet-5-5 high \u{b7} cache rebuild ~$0.76"
            );
        }
        rig.apply(&settings_changed(Changed {
            answers: Some(RequestId(1)),
            endpoint: "manifest".to_owned(),
            model: OPUS.to_owned(),
            effort: Effort::Low,
            ..Changed::default()
        }));
        let cx = rig.cx();
        let mut choice = rig.choice.clone();
        choice.sent(Some("direct"), RequestId(5), &cx);
        rig.choice = choice;
        assert!(rig.choice.expects_switch("direct"));
        rig.choice.switched(&["claude-sonnet-5-5".to_owned()]);
        assert_eq!(
            rig.choice.shown(&rig.cx(), false),
            Some(Shown::Used("Retried on direct".to_owned()))
        );
        rig.apply(&settings_changed(Changed {
            endpoint: "direct".into(),
            model: model.into(),
            effort: Effort::Low,
            ..Changed::default()
        }));
        let seen = rig.apply(&AgentEvent::retry_started(2, 1, Some(RequestId(5))));
        assert_eq!(
            seen,
            Seen::Retried {
                used: "Retried on direct".to_owned(),
                note: Some(format!(
                    "Retrying step 19 on direct \u{b7} {model} at {effort} \u{b7} cache rebuild ~${price} \u{b7} earlier Sonnet 5.5 reasoning may not carry over \u{b7} your switch to low applies from your next message",
                    effort = retried_effort.as_str()
                )),
            }
        );
        assert!(rig.choice.failed().is_none());
        assert_eq!(rig.tracker.session().model, model);
        assert_eq!(rig.tracker.session().effort, Some(retried_effort));
        assert_eq!(rig.tracker.next().effort, Some(Effort::Low));
    }
}

#[test]
fn a_retry_this_window_did_not_send_draws_its_line_and_closes_the_choice_even_after_esc() {
    for (esc, moved) in [(false, false), (true, false), (false, true)] {
        let mut rig = Rig::new(&error(offer(true, true, RetryFocus::Retry), 18));
        if esc {
            assert_eq!(rig.key(KeyCode::Esc), Some(Act::Dismissed));
            assert_eq!(rig.choice.shown(&rig.cx(), false), None);
        }
        if moved {
            rig.switch_pending("direct", Effort::Medium);
        }
        let on = if moved { "direct" } else { "manifest" };
        let seen = rig.apply(&AgentEvent::retry_started(2, 1, None));
        assert_eq!(
            seen,
            Seen::Retried {
                used: copy::retried(moved.then_some(on)),
                note: Some(format!(
                    "Retrying step 19 on {on} \u{b7} claude-opus-5-5 at medium"
                )),
            }
        );
        assert!(rig.choice.failed().is_none());
    }
}

#[test]
fn a_new_turn_lapses_the_offer_and_a_refusal_is_said() {
    let mut rig = Rig::new(&error(offer(true, true, RetryFocus::Retry), 18));
    assert_eq!(rig.apply(&AgentEvent::turn_started(2)), Seen::Lapsed);
    assert!(rig.choice.failed().is_none());
    assert_eq!(rig.apply(&AgentEvent::turn_started(3)), Seen::Nothing);

    let mut rig = Rig::new(&error(offer(true, true, RetryFocus::Retry), 18));
    let cx = rig.cx();
    let mut choice = rig.choice.clone();
    choice.sent(None, RequestId(7), &cx);
    rig.choice = choice;
    let other = AgentEvent::Rejected {
        reason: "nothing to retry".into(),
        id: None,
        kind: Some(RejectKind::NotRetryable),
        request: Some(RequestId(6)),
    };
    assert_eq!(rig.apply(&other), Seen::Nothing);
    let refused = AgentEvent::Rejected {
        reason: "nothing to retry".into(),
        id: None,
        kind: Some(RejectKind::NotRetryable),
        request: Some(RequestId(7)),
    };
    assert_eq!(
        rig.apply(&refused),
        Seen::Refused("Couldn't retry: nothing to retry.".to_owned())
    );
    assert!(rig.choice.failed().is_none());
}

#[test]
fn a_plain_retry_names_the_failed_endpoint_and_its_warm_price() {
    for takes_effort in [true, false] {
        let mut rig = Rig::new(&error(offer(true, true, RetryFocus::Retry), 0));
        if !takes_effort {
            crate::app::fixture::with_levels(&mut rig.facts, OPUS, &[], Effort::Medium);
        }
        let cx = rig.cx();
        let mut choice = rig.choice.clone();
        choice.sent(None, RequestId(2), &cx);
        rig.choice = choice;
        assert!(!rig.choice.expects_switch("manifest"));
        let seen = rig.apply(&AgentEvent::retry_started(2, 1, Some(RequestId(2))));
        let note = if takes_effort {
            "Retrying your message on manifest · claude-opus-5-5 at medium · cache warm ~$0.076"
        } else {
            "Retrying your message on manifest · claude-opus-5-5 · cache warm ~$0.076"
        };
        assert_eq!(
            seen,
            Seen::Retried {
                used: "Retried".to_owned(),
                note: Some(note.to_owned()),
            }
        );
    }
}

#[test]
fn the_tracker_takes_only_the_endpoint_on_a_retry_and_the_rest_on_the_next_turn() {
    for model in [OPUS, "claude-haiku-5-5"] {
        let mut rig = Rig::new(&error(offer(true, true, RetryFocus::Retry), 18));
        if model != OPUS {
            rig.direct_serves_only(model);
        }
        rig.switch_pending("direct", Effort::High);
        rig.apply(&settings_changed(Changed {
            endpoint: "direct".to_owned(),
            model: "claude-sonnet-5-5".to_owned(),
            effort: Effort::High,
            cache: CacheEffect::Rebuilt {
                reason: RebuildReason::ModelSwitch,
            },
            ..Changed::default()
        }));
        rig.apply(&AgentEvent::retry_started(2, 1, Some(RequestId(1))));
        let session = rig.tracker.session().clone();
        assert_eq!(
            (
                session.endpoint.as_str(),
                session.model.as_str(),
                session.effort
            ),
            ("direct", model, Some(Effort::Medium))
        );
        let next = rig.tracker.next_turn().unwrap();
        assert_eq!(next.endpoint, None);
        assert_eq!(next.model.as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(next.effort, Some(Effort::High));
        rig.apply(&ended(error(offer(true, true, RetryFocus::Retry), 0)));
        rig.apply(&AgentEvent::turn_started(3));
        assert_eq!(rig.tracker.session().model, "claude-sonnet-5-5");
        assert_eq!(rig.tracker.next_turn(), None);
    }
    let mut facts = endpoint_facts();
    facts.effort = "xhigh".into();
    let manifest = facts
        .endpoints
        .iter_mut()
        .find(|e| e.name == "manifest")
        .unwrap();
    manifest.models = vec![AccountModel {
        id: OPUS.into(),
        listed: Some(0),
    }];
    manifest.complete = true;
    manifest.default_model = Some(OPUS.into());
    with_levels(&mut facts, "claude-sonnet-5-5", &[], Effort::High);
    let mut rig = Rig::on(facts, &error(offer(true, true, RetryFocus::Retry), 1));
    rig.direct_serves_only("claude-sonnet-5-5");
    rig.apply(&settings_changed(Changed {
        endpoint: "direct".into(),
        model: "claude-sonnet-5-5".into(),
        effort: Effort::Xhigh,
        ..Changed::default()
    }));
    rig.apply(&AgentEvent::retry_started(2, 1, Some(RequestId(1))));
    assert_eq!(rig.tracker.session().effort, Some(Effort::Xhigh));
    assert_eq!(
        display_effort(
            &rig.facts,
            &rig.tracker.session().model,
            rig.tracker.session().effort
        ),
        None
    );
    with_levels(
        &mut rig.facts,
        OPUS,
        &[Effort::Low, Effort::Medium, Effort::High],
        Effort::High,
    );
    rig.apply(&ended(error(offer(true, true, RetryFocus::Retry), 1)));
    rig.apply(&settings_changed(Changed {
        endpoint: "manifest".into(),
        model: OPUS.into(),
        effort: Effort::High,
        ..Changed::default()
    }));
    rig.apply(&AgentEvent::retry_started(3, 2, Some(RequestId(2))));
    assert_eq!(rig.tracker.session().model, OPUS);
    assert_eq!(rig.tracker.session().effort, Some(Effort::High));
}

#[test]
fn the_retry_picker_prices_the_failed_turns_model_while_another_model_is_pending() {
    let mut rig = Rig::new(&error(offer(true, true, RetryFocus::Switch), 18));
    rig.apply(&settings_changed(Changed {
        endpoint: "manifest".to_owned(),
        model: "claude-haiku-5-5".to_owned(),
        cache: CacheEffect::Rebuilt {
            reason: RebuildReason::ModelSwitch,
        },
        ..Changed::default()
    }));
    assert_eq!(rig.tracker.wanted().model, "claude-haiku-5-5");
    assert_eq!(rig.tracker.session().model, OPUS);
    rig.choice.picking(None);
    let effects: Vec<(String, String)> = rig
        .menu()
        .choices
        .into_iter()
        .map(|c| (unglued(c.label.trim_start()), c.effect))
        .collect();
    assert_eq!(
        effects[..3],
        [
            (
                "direct".to_owned(),
                "opus-5-5 medium \u{b7} cache rebuild ~$1.52".to_owned()
            ),
            (
                "gateway".to_owned(),
                "opus-5-5 medium \u{b7} cache rebuild ~$1.52".to_owned()
            ),
            (
                "work".to_owned(),
                "opus-5-5 medium \u{b7} cache rebuild, no prices".to_owned()
            ),
        ]
    );
    rig.at = s(10 + 3601);
    assert_eq!(
        rig.menu().choices[0].effect,
        "opus-5-5 medium \u{b7} cache rebuild ~$1.52"
    );
    rig.direct_serves_only("claude-sonnet-5-5");
    rig.switch_pending("direct", Effort::High);
    assert_eq!(
        rig.menu().choices[0].effect,
        "sonnet-5-5 medium \u{b7} cache rebuild ~$0.76"
    );
    assert_eq!(rig.tracker.session().model, OPUS);
}

#[test]
fn modified_arrows_and_enter_are_left_to_the_composer_and_ignored_by_the_picker() {
    let mut rig = Rig::new(&error(offer(true, true, RetryFocus::Retry), 18));
    for code in [KeyCode::Up, KeyCode::Down, KeyCode::Enter, KeyCode::Tab] {
        for (ctrl, alt) in [(true, false), (false, true), (true, true)] {
            let key = Key {
                ctrl,
                alt,
                ..Key::plain(code)
            };
            assert_eq!(rig.press(key), None, "{key:?}");
        }
    }
    assert_eq!(rig.rows().1, 0);
    rig.choice.picking(None);
    let before = rig.menu().selected;
    for code in [KeyCode::Up, KeyCode::Down, KeyCode::Enter, KeyCode::Tab] {
        for (ctrl, alt) in [(true, false), (false, true)] {
            let key = Key {
                ctrl,
                alt,
                ..Key::plain(code)
            };
            assert_eq!(rig.press(key), Some(Act::Stay), "{key:?}");
        }
    }
    assert!(rig.choice.is_picking());
    assert_eq!(rig.menu().selected, before);
}

fn compacted() -> AgentEvent {
    AgentEvent::Compacted {
        trigger: wisp_protocol::CompactionTrigger::Manual,
        engine: wisp_protocol::CompactionEngine::Native,
        before: 190_000,
        after: 9_000,
        partial: false,
        converted: false,
        cache_effect: CacheEffect::Rebuilt {
            reason: RebuildReason::Compaction,
        },
        usage: None,
        cost: None,
        answers: Some(RequestId(4)),
        turn: None,
        summary: None,
        paused: false,
    }
}

#[test]
fn a_completed_compaction_lapses_an_open_choice_but_waits_for_a_sent_retrys_answer() {
    let mut rig = Rig::new(&error(offer(true, true, RetryFocus::Retry), 18));
    assert_eq!(rig.apply(&compacted()), Seen::Lapsed);
    assert!(rig.choice.failed().is_none());
    assert_eq!(rig.apply(&compacted()), Seen::Nothing);

    let mut rig = Rig::new(&error(offer(true, true, RetryFocus::Retry), 18));
    let cx = rig.cx();
    let mut choice = rig.choice.clone();
    choice.sent(None, RequestId(7), &cx);
    rig.choice = choice;
    assert_eq!(rig.apply(&compacted()), Seen::Nothing);
    let refused = AgentEvent::Rejected {
        reason: "the conversation was compacted after the failure".into(),
        id: None,
        kind: Some(RejectKind::NotRetryable),
        request: Some(RequestId(7)),
    };
    assert_eq!(
        rig.apply(&refused),
        Seen::Refused(
            "Couldn't retry: the conversation was compacted after the failure.".to_owned()
        )
    );
}

#[test]
fn a_key_as_the_failed_or_pending_model_is_never_named_in_the_note() {
    let key = "sk-ant-api03-CANARYMODEL0000";
    let mut rig = Rig::new(&error(offer(true, true, RetryFocus::Retry), 0));
    rig.apply(&settings_changed(Changed {
        answers: Some(RequestId(1)),
        endpoint: "manifest".to_owned(),
        model: key.to_owned(),
        effort: Effort::Medium,
        ..Changed::default()
    }));
    let cx = rig.cx();
    let mut choice = rig.choice.clone();
    choice.sent(None, RequestId(2), &cx);
    rig.choice = choice;
    let seen = rig.apply(&AgentEvent::retry_started(2, 1, Some(RequestId(2))));
    let Seen::Retried {
        note: Some(note), ..
    } = seen
    else {
        panic!("{seen:?}");
    };
    assert!(!note.contains("CANARY"), "{note}");
    assert!(
        note.ends_with(&format!(
            "your switch to {} applies from your next message",
            wisp_protocol::NOT_SHOWN
        )),
        "{note}"
    );
}

#[test]
fn retry_estimates_are_marked_and_respect_tokens_only() {
    let mut facts = crate::app::fixture::facts(
        wisp_render::chrome::Endpoint::Official,
        wisp_protocol::CacheTtl::OneHour,
    );
    facts.credential = Some(wisp_render::chrome::Credential::ChatGptPlan);
    let tracker = warmed(&facts);
    let cx = Ctx {
        facts: &facts,
        tracker: &tracker,
        at: s(11),
        wall: 0,
        set: UNI,
    };
    assert!(cx.retry_cost("gateway", OPUS).contains("~$"));
    assert!(cx.rewrite_amount("gateway", OPUS).contains("~$"));
    assert!(
        cx.rewrite_amount("gateway", OPUS)
            .ends_with(", subscription")
    );
    let mut endpoint = endpoint_facts()
        .endpoints
        .into_iter()
        .find(|e| e.name == "gateway")
        .unwrap();
    endpoint.priced = false;
    facts.endpoints = vec![endpoint];
    let cx = Ctx {
        facts: &facts,
        tracker: &tracker,
        at: s(11),
        wall: 0,
        set: UNI,
    };
    assert!(!cx.retry_cost("gateway", OPUS).contains('$'));
    assert!(!cx.rewrite_amount("gateway", OPUS).contains('$'));
}
