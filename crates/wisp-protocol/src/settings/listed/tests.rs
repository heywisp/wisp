use super::*;

const MODELS: [Offer<'static>; 7] = [
    Offer {
        id: "claude-haiku-5-5",
        provider: Some("anthropic"),
        listed: Some(2),
    },
    Offer {
        id: "claude-opus-4-8",
        provider: Some("anthropic"),
        listed: None,
    },
    Offer {
        id: "claude-opus-5-5",
        provider: Some("anthropic"),
        listed: Some(0),
    },
    Offer {
        id: "claude-sonnet-5-5",
        provider: Some("anthropic"),
        listed: Some(1),
    },
    Offer {
        id: "gpt-6-astra",
        provider: Some("openai"),
        listed: Some(4),
    },
    Offer {
        id: "gpt-6.1-sol",
        provider: Some("openai"),
        listed: Some(3),
    },
    Offer {
        id: "mystery",
        provider: None,
        listed: Some(5),
    },
];

fn chat(provider: Option<&'static str>, model: &'static str) -> Chat<'static> {
    Chat { provider, model }
}

fn landing<'a>(provider: &'a str, models: Option<&'a [AccountModel]>) -> Landing<'a> {
    Landing::new(provider, models.unwrap_or_default(), false, None)
}

#[test]
fn each_account_offers_its_providers_listed_models_in_table_order_then_the_chats_model_and_reaches_the_rest()
 {
    let cases: [(&str, Chat<'static>, &[&str], &[&str]); 6] = [
        (
            "anthropic",
            chat(Some("anthropic"), "claude-opus-5-5"),
            &["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"],
            &[
                "claude-opus-5-5",
                "claude-sonnet-5-5",
                "claude-haiku-5-5",
                "claude-opus-4-8",
            ],
        ),
        (
            "anthropic",
            chat(Some("anthropic"), "claude-opus-4-8"),
            &[
                "claude-opus-5-5",
                "claude-sonnet-5-5",
                "claude-haiku-5-5",
                "claude-opus-4-8",
            ],
            &[
                "claude-opus-5-5",
                "claude-sonnet-5-5",
                "claude-haiku-5-5",
                "claude-opus-4-8",
            ],
        ),
        (
            "openai",
            chat(Some("anthropic"), "claude-opus-4-8"),
            &["gpt-6.1-sol", "gpt-6-astra"],
            &["gpt-6.1-sol", "gpt-6-astra"],
        ),
        (
            "OpenAI",
            chat(Some("openai"), "gpt-6.1-sol"),
            &["gpt-6.1-sol", "gpt-6-astra"],
            &["gpt-6.1-sol", "gpt-6-astra"],
        ),
        (
            "anthropic",
            chat(None, "opus-55"),
            &["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"],
            &[
                "claude-opus-5-5",
                "claude-sonnet-5-5",
                "claude-haiku-5-5",
                "claude-opus-4-8",
            ],
        ),
        (
            "gateway",
            chat(Some("anthropic"), "claude-opus-5-5"),
            &[],
            &[],
        ),
    ];
    for (account, chat, offers, reaches) in cases {
        assert_eq!(
            offered(&MODELS, landing(account, None), chat),
            offers,
            "{account} {chat:?}"
        );
        for every in [every, reachable] {
            assert_eq!(
                every(&MODELS, landing(account, None), chat),
                reaches,
                "{account} {chat:?}"
            );
        }
    }
}

#[test]
fn an_account_offers_a_typed_name_only_when_its_provider_is_known_and_matches() {
    let asks = |account: &str, provider: Option<&'static str>, model: &'static str| {
        offered(&MODELS, landing(account, None), chat(provider, model))
            .iter()
            .any(|id| id == model)
    };
    assert!(asks("anthropic", Some("anthropic"), "claude-opus-4-8"));
    assert!(!asks("openai", Some("anthropic"), "claude-opus-4-8"));
    assert!(!asks("anthropic", None, "opus-55"));
}

#[test]
fn an_own_list_ranks_its_shown_entries_reaches_its_hidden_ones_and_keeps_the_chats_model() {
    let own = |id: &str, listed: Option<u16>| AccountModel {
        id: id.to_owned(),
        listed,
    };
    let list = [
        own("gpt-6-astra", Some(0)),
        own("gpt-new", Some(1)),
        own("gpt-hidden", None),
    ];
    let on = Some(&list[..]);
    let gpt = chat(Some("openai"), "gpt-6.1-sol");
    assert_eq!(
        offered(&MODELS, landing("openai", on), gpt),
        ["gpt-6-astra", "gpt-new", "gpt-6.1-sol"]
    );
    assert_eq!(
        reachable(&MODELS, landing("openai", on), gpt),
        ["gpt-6-astra", "gpt-new", "gpt-6.1-sol", "gpt-hidden"]
    );
    assert_eq!(
        every(&MODELS, landing("openai", on), gpt),
        reachable(&MODELS, landing("openai", on), gpt)
    );
    let other = chat(None, "mystery");
    assert_eq!(
        every(&MODELS, landing("openai", on), other),
        ["gpt-6-astra", "gpt-new", "gpt-hidden"]
    );
    assert_eq!(
        reachable(&MODELS, landing("openai", on), other),
        ["gpt-6-astra", "gpt-new", "gpt-hidden", "gpt-6.1-sol"]
    );
    assert_eq!(runs_on(&MODELS, landing("openai", on), gpt), "gpt-6.1-sol");
    let hidden = chat(None, "gpt-hidden");
    assert_eq!(
        offered(&MODELS, landing("openai", on), hidden),
        ["gpt-6-astra", "gpt-new", "gpt-hidden"]
    );
    assert_eq!(
        runs_on(&MODELS, landing("openai", on), chat(None, "mystery")),
        "gpt-6-astra"
    );
    assert_eq!(
        offered(&MODELS, landing("openai", None), hidden),
        ["gpt-6.1-sol", "gpt-6-astra"]
    );
    let complete = Landing {
        complete: true,
        default_model: Some("gpt-new"),
        ..landing("openai", on)
    };
    assert_eq!(offered(&MODELS, complete, gpt), ["gpt-6-astra", "gpt-new"]);
    for listing in [every, reachable] {
        assert_eq!(
            listing(&MODELS, complete, gpt),
            ["gpt-6-astra", "gpt-new", "gpt-hidden"]
        );
    }
    assert_eq!(runs_on(&MODELS, complete, gpt), "gpt-new");
    assert_eq!(
        runs_on(
            &MODELS,
            Landing {
                default_model: Some("gpt-missing"),
                ..complete
            },
            gpt
        ),
        "gpt-6-astra"
    );
    assert_eq!(
        runs_on(
            &MODELS,
            complete,
            chat(Some("anthropic"), "claude-opus-5-5")
        ),
        "gpt-new"
    );
    let empty = Landing::new("openai", &[], false, Some("gpt-default"));
    assert_eq!(runs_on(&MODELS, empty, other), "gpt-default");
    assert_eq!(
        runs_on(&MODELS, landing("openai", None), other),
        "gpt-6.1-sol"
    );
}

#[test]
fn variants_share_sorted_siblings_and_show_their_own_effort() {
    let variant = |family: &str, effort| EffortVariant {
        family: family.into(),
        effort,
    };
    let rows = [
        ("one-high", variant("one", Effort::High)),
        ("two-low", variant("two", Effort::Low)),
        ("one-low", variant("one", Effort::Low)),
        ("one-medium", variant("one", Effort::Medium)),
    ];
    let siblings = siblings(rows.iter().map(|(id, variant)| (*id, variant)), &rows[0].1);
    assert_eq!(
        siblings,
        [
            (Effort::Low, "one-low"),
            (Effort::Medium, "one-medium"),
            (Effort::High, "one-high")
        ]
    );
    for given in [None, Some(Effort::Max)] {
        assert_eq!(
            display_effort(Some(&rows[0].1), Some(&[]), given),
            Some(Effort::High)
        );
        assert_eq!(display_effort(None, None, given), given);
        assert_eq!(display_effort(None, Some(&[]), given), None);
        assert_eq!(display_effort(None, Some(&Effort::ALL), given), given);
    }
}

#[test]
fn a_move_lands_on_the_chats_own_pair_then_the_remembered_one_then_todays_rule() {
    let last = Remembered {
        model: "claude-sonnet-5-5".into(),
        effort: Effort::High,
    };
    for (same, remembered, model, effort, recalled) in [
        (true, Some(&last), "gpt-6.1-sol", Some(Effort::Low), false),
        (
            false,
            Some(&last),
            "claude-sonnet-5-5",
            Some(Effort::High),
            true,
        ),
        (false, None, "claude-opus-5-5", Some(Effort::Low), false),
    ] {
        assert_eq!(
            lands_on(
                &MODELS,
                landing("anthropic", None),
                same,
                (chat(Some("openai"), "gpt-6.1-sol"), Some(Effort::Low)),
                remembered
            ),
            Landed {
                model: model.into(),
                effort,
                remembered: recalled
            }
        );
    }
}
