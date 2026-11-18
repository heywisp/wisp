use wisp_protocol::Billing::{AnthropicPlan, ChatGptPlan, Metered};

use super::*;
use crate::endpoints::ProviderKind::{Anthropic, OpenAi};

const CLAUDE_SUBSCRIPTION: Account<'static> = Account {
    name: "claude-subscription",
    provider: Anthropic,
    billing: AnthropicPlan,
};
const CLAUDE_WORK: Account<'static> = Account {
    name: "claude-work",
    provider: Anthropic,
    billing: AnthropicPlan,
};
const CHATGPT: Account<'static> = Account {
    name: "chatgpt",
    provider: OpenAi,
    billing: ChatGptPlan,
};
const CLAUDE_API: Account<'static> = Account {
    name: "claude-api",
    provider: Anthropic,
    billing: Metered,
};
const CLAUDE_API_2: Account<'static> = Account {
    name: "claude-api-2",
    provider: Anthropic,
    billing: Metered,
};

fn left(account: Account<'static>) -> Leaving<'static> {
    Leaving {
        name: account.name,
        provider: Some(account.provider),
        billing: Some(account.billing),
    }
}

const ENV_KEY: Leaving<'static> = Leaving {
    name: "env",
    provider: Some(Anthropic),
    billing: Some(Metered),
};

const UNKNOWN: Leaving<'static> = Leaving {
    name: "claude-old",
    provider: None,
    billing: None,
};

const GATEWAY: Leaving<'static> = Leaving {
    name: "gateway",
    provider: None,
    billing: Some(Metered),
};

pub(crate) struct Row {
    pub(crate) case: &'static str,
    pub(crate) accounts: Vec<Account<'static>>,
    pub(crate) from: Leaving<'static>,
    pub(crate) start: Option<&'static str>,
    pub(crate) goes: Goes<'static>,
}

pub(crate) fn table() -> Vec<Row> {
    vec![
        Row {
            case: "the owner's case: an API key from the keys file, only a Claude subscription left",
            accounts: vec![CLAUDE_SUBSCRIPTION],
            from: ENV_KEY,
            start: Some("claude-subscription"),
            goes: Goes::Continue("claude-subscription"),
        },
        Row {
            case: "T§3a: the start account is the same provider and doesn't start charging",
            accounts: vec![CHATGPT, CLAUDE_WORK],
            from: left(CLAUDE_SUBSCRIPTION),
            start: Some("claude-work"),
            goes: Goes::Continue("claude-work"),
        },
        Row {
            case: "T§3b: the chat ran on chatgpt, now gone, and new chats start on a Claude account",
            accounts: vec![CLAUDE_API, CLAUDE_SUBSCRIPTION, CLAUDE_WORK],
            from: left(CHATGPT),
            start: Some("claude-subscription"),
            goes: Goes::Ask(vec!["claude-subscription", "claude-work", "claude-api"]),
        },
        Row {
            case: "T§3c: from a subscription, another provider or the API is left",
            accounts: vec![CHATGPT, CLAUDE_API],
            from: left(CLAUDE_SUBSCRIPTION),
            start: Some("chatgpt"),
            goes: Goes::Ask(vec!["chatgpt", "claude-api"]),
        },
        Row {
            case: "the start account is another provider: the only same-provider one that doesn't start charging",
            accounts: vec![CHATGPT, CLAUDE_SUBSCRIPTION],
            from: left(CLAUDE_API),
            start: Some("chatgpt"),
            goes: Goes::Continue("claude-subscription"),
        },
        Row {
            case: "several fit and none is the start account",
            accounts: vec![CHATGPT, CLAUDE_SUBSCRIPTION, CLAUDE_WORK],
            from: left(CLAUDE_API),
            start: Some("chatgpt"),
            goes: Goes::Ask(vec!["chatgpt", "claude-subscription", "claude-work"]),
        },
        Row {
            case: "several fit and one is the start account",
            accounts: vec![CLAUDE_SUBSCRIPTION, CLAUDE_WORK],
            from: left(CLAUDE_API),
            start: Some("claude-work"),
            goes: Goes::Continue("claude-work"),
        },
        Row {
            case: "API to API adds no new kind of charge",
            accounts: vec![CLAUDE_API_2],
            from: left(CLAUDE_API),
            start: Some("claude-api-2"),
            goes: Goes::Continue("claude-api-2"),
        },
        Row {
            case: "subscription to the API always asks, even with one choice",
            accounts: vec![CLAUDE_API],
            from: left(CLAUDE_SUBSCRIPTION),
            start: Some("claude-api"),
            goes: Goes::Ask(vec!["claude-api"]),
        },
        Row {
            case: "the account being left never counts as the start account",
            accounts: vec![CLAUDE_SUBSCRIPTION, CLAUDE_WORK, CLAUDE_API],
            from: left(CLAUDE_API),
            start: Some("claude-api"),
            goes: Goes::Ask(vec!["claude-subscription", "claude-work"]),
        },
        Row {
            case: "no same-provider account is left",
            accounts: vec![CHATGPT],
            from: left(CLAUDE_SUBSCRIPTION),
            start: Some("chatgpt"),
            goes: Goes::Ask(vec!["chatgpt"]),
        },
        Row {
            case: "an account nothing is known about asks even before a subscription",
            accounts: vec![CLAUDE_SUBSCRIPTION],
            from: UNKNOWN,
            start: None,
            goes: Goes::Ask(vec!["claude-subscription"]),
        },
        Row {
            case: "a gateway whose provider can't be told never crosses to the start account silently",
            accounts: vec![CLAUDE_SUBSCRIPTION],
            from: GATEWAY,
            start: Some("claude-subscription"),
            goes: Goes::Ask(vec!["claude-subscription"]),
        },
        Row {
            case: "a gateway told by its model's provider continues on that provider's start account",
            accounts: vec![CLAUDE_SUBSCRIPTION],
            from: Leaving {
                provider: Some(Anthropic),
                ..GATEWAY
            },
            start: Some("claude-subscription"),
            goes: Goes::Continue("claude-subscription"),
        },
        Row {
            case: "an account nothing is known about asks before the API",
            accounts: vec![CLAUDE_API],
            from: UNKNOWN,
            start: None,
            goes: Goes::Ask(vec!["claude-api"]),
        },
    ]
}

#[test]
fn chat_goes_follows_the_one_rule() {
    for row in table() {
        assert_eq!(
            chat_goes(&row.accounts, &row.from, row.start),
            row.goes,
            "{}",
            row.case
        );
    }
    let claude = |a: &Account<'_>| a.provider == Anthropic;
    for (case, accounts, start, goes) in [
        (
            "the start account offers it",
            vec![CLAUDE_SUBSCRIPTION, CLAUDE_WORK],
            "claude-subscription",
            ModelGoes::Here,
        ),
        (
            "one other account offers it, from a subscription to a subscription",
            vec![CHATGPT, CLAUDE_SUBSCRIPTION],
            "chatgpt",
            ModelGoes::Continue("claude-subscription"),
        ),
        (
            "several offer it: subscriptions before the API, then by name",
            vec![CLAUDE_API, CHATGPT, CLAUDE_WORK, CLAUDE_SUBSCRIPTION],
            "chatgpt",
            ModelGoes::Ask(vec!["claude-subscription", "claude-work", "claude-api"]),
        ),
        (
            "only an API account offers it, from a subscription",
            vec![CHATGPT, CLAUDE_API],
            "chatgpt",
            ModelGoes::Ask(vec!["claude-api"]),
        ),
        (
            "no account offers it",
            vec![CHATGPT],
            "chatgpt",
            ModelGoes::NoneOffers,
        ),
    ] {
        assert_eq!(model_goes(&accounts, start, claude), goes, "{case}");
    }
}

#[test]
fn only_a_move_from_a_subscription_to_the_api_starts_charging() {
    for (from, to, charges) in [
        (Some(Metered), AnthropicPlan, false),
        (Some(Metered), Metered, false),
        (Some(AnthropicPlan), AnthropicPlan, false),
        (Some(AnthropicPlan), ChatGptPlan, false),
        (Some(AnthropicPlan), Metered, true),
        (Some(ChatGptPlan), Metered, true),
        (Some(wisp_protocol::Billing::Mixed), Metered, true),
        (None, Metered, true),
        (None, AnthropicPlan, false),
    ] {
        assert_eq!(starts_charging(from, to), charges, "{from:?} to {to:?}");
    }
}

#[test]
fn the_keys_file_account_is_never_shown_by_its_name() {
    assert_eq!(shown("env"), wisp_protocol::KEYS_FILE_ACCOUNT);
    assert_eq!(shown("claude-work"), "claude-work");
    assert_eq!(named("env"), wisp_protocol::KEYS_FILE_ACCOUNT);
    assert_eq!(named("claude-work"), "account claude-work");
}

#[test]
fn a_logged_model_or_subscription_names_its_provider() {
    assert_eq!(model_provider("claude-opus-5-5"), Some(Anthropic));
    assert_eq!(model_provider("gpt-6.1-sol"), Some(OpenAi));
    assert_eq!(
        model_provider("gemini-3.1-pro-preview"),
        Some(ProviderKind::Gemini)
    );
    assert_eq!(
        model_provider("gemini-future-experimental"),
        Some(ProviderKind::Gemini)
    );
    assert_eq!(model_provider("claude-mythos-preview"), Some(Anthropic));
    assert_eq!(model_provider("someday-1"), None);
    assert_eq!(billing_provider(AnthropicPlan), Some(Anthropic));
    assert_eq!(billing_provider(ChatGptPlan), Some(OpenAi));
    assert_eq!(
        billing_provider(wisp_protocol::Billing::AntigravityPlan),
        Some(ProviderKind::Gemini)
    );
    assert_eq!(billing_provider(Metered), None);
}

#[test]
fn antigravity_subscription_to_gemini_api_always_asks_before_charging() {
    let antigravity = Account {
        name: "antigravity",
        provider: ProviderKind::Gemini,
        billing: wisp_protocol::Billing::AntigravityPlan,
    };
    let gemini_api = Account {
        name: "gemini-api",
        provider: ProviderKind::Gemini,
        billing: Metered,
    };
    assert_eq!(
        chat_goes(&[gemini_api], &left(antigravity), Some("gemini-api")),
        Goes::Ask(vec!["gemini-api"])
    );
    assert_eq!(
        chat_goes(&[antigravity], &left(gemini_api), Some("antigravity")),
        Goes::Continue("antigravity")
    );
}

#[test]
fn provider_switch_refusals_name_everyone_and_the_next_action() {
    assert_eq!(
        answering_on("gemini", "Try again once it's done."),
        "This chat is answering on gemini. Try again once it's done."
    );
    assert_eq!(
        working_on(&["/worker".into()], "gemini"),
        "worker is working on gemini. Try again once it's done."
    );
    assert_eq!(
        working_on(&["/worker".into(), "/coder".into()], "gemini"),
        "worker and coder are working on gemini. Try again once they're done."
    );
    assert_eq!(
        provider_in_use("gemini"),
        "This chat is on gemini. Switch account first (/account)."
    );
    assert_eq!(
        no_other_account(),
        "No other account for this chat to go to. Add one first (/login)."
    );
}
