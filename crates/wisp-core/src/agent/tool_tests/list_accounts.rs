use wisp_protocol::plan_usage::{
    Availability, Percent, PlanTier, PlanUsage, UsageScope, UsageSource, UsageWindow,
};
use wisp_protocol::settings::{CredentialKind, EndpointSummary, SettingValue, Target};

use super::list_settings::{KEY, KEY_SHAPED, connection, layers, results};
use super::*;
use crate::endpoints::ProviderKind;
use crate::plan_usage::{Display, PlanUsageCache};
use crate::settings::Given;
use crate::settings::host::StartLayers;
use wisp_plugin_menus::ACCOUNTS;

const NOW: i64 = 1_800_000_000;
const GATEWAY_HOST: &str = "gateway-canary.example.invalid";
const REASON: &str = "REASON-CANARY: ~/private/keys.toml line 3";
const KEY_MODEL: &str = "claude-pasted0123456789key";

pub(super) fn summary(
    name: &str,
    provider: &str,
    credential: &str,
    official: bool,
) -> EndpointSummary {
    EndpointSummary {
        name: name.into(),
        provider: provider.into(),
        credential: Some(match credential {
            "api_key" => CredentialKind::ApiKey,
            other => CredentialKind::Other(other.into()),
        }),
        official: Some(official),
        host: (!official).then(|| GATEWAY_HOST.into()),
        priced: true,
        ..EndpointSummary::default()
    }
}

fn summaries() -> Vec<EndpointSummary> {
    vec![
        summary("claude-work", "anthropic", "anthropic_subscription", true),
        EndpointSummary {
            default: true,
            ..summary("claude-api", "anthropic", "api_key", true)
        },
        summary(KEY_SHAPED, "openai", "chatgpt_subscription", true),
        EndpointSummary {
            unavailable: Some(REASON.into()),
            ..summary("corp", "anthropic", "api_key", false)
        },
    ]
}

fn five_hour() -> PlanUsage {
    let mut usage = PlanUsage::new(
        None,
        vec![UsageWindow {
            key: "five_hour".into(),
            label: "5-hour".into(),
            duration_seconds: Some(18_000),
            scope: UsageScope::All,
            used_percent: Some(Percent::try_from(43.0).unwrap()),
            resets_at: Some(NOW + 7_800),
            observed_at: NOW,
            source: UsageSource::UsageEndpoint,
        }],
    )
    .unwrap();
    usage.plan_tier = Some(PlanTier::Pro);
    usage
}

fn listing(provider: Arc<Fake>, env: &Env) -> (Agent, PlanUsageCache) {
    let cache = PlanUsageCache::default();
    let chatgpt = own_list();
    let session = SessionConfig {
        model: KEY_MODEL.into(),
        ..default_session()
    };
    let agent = Agent::new(provider.clone(), session)
        .with_clock(TickingClock::new())
        .with_tools(crate::tool::testkit::bundled(&[ACCOUNTS]), env.workspace())
        .with_platform("Linux")
        .with_resources(None)
        .with_cancel_grace(Duration::from_millis(300))
        .with_connections(
            vec![
                connection("claude-work", provider.clone()),
                connection(KEY_SHAPED, chatgpt.clone()),
            ],
            Vec::new(),
            "claude-work",
        )
        .with_start_layers(StartLayers {
            endpoints: summaries(),
            ..layers()
        })
        .with_plan_usage(cache.clone(), Display::Enabled);
    let work = cache.bind("claude-work", provider).unwrap();
    cache.observe("claude-work", work, Ok(five_hour()), NOW);
    let chatgpt = cache.bind(KEY_SHAPED, chatgpt).unwrap();
    let limited = PlanUsage::new(Some(Availability::Limited), Vec::new()).unwrap();
    cache.observe(KEY_SHAPED, chatgpt, Ok(limited), NOW);
    (agent, cache)
}

fn own_list() -> Arc<Fake> {
    let knobs = crate::agent::testing::Knobs {
        list: Some(Arc::new(crate::models::list::tests::sample())),
        ..Default::default()
    };
    Fake::with(knobs, [])
}

fn rows(result: &str) -> Vec<Vec<&str>> {
    let mut rows: Vec<Vec<&str>> = Vec::new();
    for line in result.lines() {
        match (
            line.strip_prefix("- "),
            line.strip_prefix("  "),
            rows.last_mut(),
        ) {
            (Some(row), _, _) => rows.push(vec![row]),
            (None, Some(more), Some(row)) => row.push(more),
            _ => {}
        }
    }
    rows
}

#[tokio::test]
async fn list_accounts_reads_the_summaries_models_and_cached_limits_and_never_a_secret() {
    let env = Env::new("list-accounts");
    let scripted = scripted(vec![
        uses(&[call("a1", ACCOUNTS, json!({}))]),
        done("listed"),
        uses(&[call("a2", ACCOUNTS, json!({}))]),
        done("listed again"),
    ]);
    let (mut agent, _cache) = listing(scripted.clone(), &env);
    let (outcome, events) = turn(&mut agent, "which accounts do I have").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{events:#?}"
    );
    let claude_usage = wisp_protocol::settings::find("claude_usage").unwrap();
    agent.settings.session = vec![Given {
        target: Target::Name(claude_usage),
        value: SettingValue::Bool(false),
        origin: "test".into(),
    }];
    let (_, later) = turn(&mut agent, "and now").await;
    let found: Vec<String> = results(&events)
        .into_iter()
        .chain(results(&later))
        .collect();
    assert_eq!(found.len(), 2, "{events:#?}");
    let pair = agent.next_pair();
    let model = wisp_protocol::shown_model(&pair.model);
    assert!(
        found[0].starts_with(&format!(
            "You run on claude-work, model {model}, effort {}. New chats start on claude-api.\n4 accounts:\n",
            pair.effort.as_str()
        )),
        "{}",
        found[0]
    );
    let entries = agent.model_entries();
    let offers = wisp_protocol::settings::offers(&entries);
    let chat = crate::agent::settings::chat(&pair.model);
    let kinds = [
        (
            ProviderKind::Anthropic,
            wisp_protocol::Billing::AnthropicPlan,
            false,
        ),
        (
            ProviderKind::Anthropic,
            wisp_protocol::Billing::Metered,
            false,
        ),
        (
            ProviderKind::OpenAi,
            wisp_protocol::Billing::ChatGptPlan,
            false,
        ),
        (
            ProviderKind::Anthropic,
            wisp_protocol::Billing::Metered,
            true,
        ),
    ];
    let listed = rows(&found[0]);
    assert_eq!(listed.len(), 4, "{}", found[0]);
    for ((row, summary), (provider, billing, gateway)) in listed.iter().zip(summaries()).zip(kinds)
    {
        let name = crate::agents::endpoints::plain(&summary.name);
        let kind = crate::agents::endpoints::account_kind(provider, billing, gateway);
        assert!(row[0].starts_with(&format!("{name} · {kind}")), "{row:?}");
        let own = (summary.name == KEY_SHAPED).then(|| own_list().model_list().unwrap().offers());
        let models: Vec<String> = wisp_protocol::settings::offered(
            &offers,
            wisp_protocol::settings::Landing::new(
                &summary.provider,
                own.as_deref().unwrap_or_default(),
                summary.complete,
                summary.default_model.as_deref(),
            ),
            chat,
        )
        .iter()
        .map(|id| wisp_protocol::shown_model(id).to_owned())
        .collect();
        assert_eq!(row[1], format!("models: {}", models.join(", ")), "{row:?}");
    }
    assert_eq!(
        listed[2][1], "models: gpt-6.1-sol, gpt-new",
        "{:?}",
        listed[2]
    );
    assert!(listed[0][2].contains("5-hour 43%"), "{:?}", listed[0]);
    assert_eq!(listed[1].len(), 2, "an API account has no limits line");
    assert_eq!(
        listed[2][2], "limits: limit reached",
        "another provider's subscription"
    );
    assert!(listed[3][0].ends_with("unavailable (/account says why)"));
    let disabled = rows(&found[1]);
    assert_eq!(
        disabled[0][2],
        "limits: Claude usage is off (claude_usage in /config)"
    );
    assert_eq!(disabled[2][2], "limits: limit reached");
    assert_eq!(model, wisp_protocol::NOT_SHOWN);
    assert!(listed[0][1].contains(model), "{:?}", listed[0]);
    for text in &found {
        for hidden in [
            KEY,
            KEY_SHAPED,
            KEY_MODEL,
            "CANARY",
            "Pro",
            GATEWAY_HOST,
            "private",
        ] {
            assert!(!text.contains(hidden), "{hidden} in {text}");
        }
    }
    assert_eq!(scripted.plan_usage_calls(), 0, "no lookup started");
    let requests = scripted.sent();
    assert_eq!(requests.len(), 4);
    let first = &requests[0];
    assert_eq!(
        first
            .tools
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        [ACCOUNTS]
    );
    for recorded in &requests {
        assert_eq!(
            serde_json::to_string(&recorded.tools).unwrap(),
            serde_json::to_string(&first.tools).unwrap()
        );
        assert_eq!(recorded.session.system, first.session.system);
    }
    for (later, earlier) in requests.iter().skip(1).zip(&requests) {
        assert_eq!(
            later.messages[..earlier.messages.len()],
            earlier.messages[..],
            "a call appends only its result"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn esc_stops_the_turn_while_a_sign_in_read_has_not_answered() {
    let env = Env::new("list-accounts-esc");
    let scripted = scripted(vec![uses(&[call("a1", ACCOUNTS, json!({}))])]);
    let (mut agent, _cache) = listing(scripted.clone(), &env);
    let host = crate::endpoints::testing::AuthHost::new();
    *host.stall.lock().unwrap() = Some(Duration::from_secs(5));
    agent.queue.endpoints = Some(host.clone());
    let started = std::time::Instant::now();
    let (outcome, events) = turn_with(
        &mut agent,
        "which accounts do I have",
        Some(|e| matches!(e, AgentEvent::ToolCallStarted { .. })),
    )
    .await;
    assert!(
        matches!(outcome, TurnOutcome::Cancelled { .. }),
        "{events:#?}"
    );
    assert!(
        started.elapsed() < crate::plugin::host::accounts::SIGN_IN_WAIT,
        "{:?}",
        started.elapsed()
    );
    let found = results(&events);
    assert!(
        found.len() == 1 && found[0].starts_with("You run on claude-work"),
        "the call answers its cancel with the list: {found:?}"
    );
}
