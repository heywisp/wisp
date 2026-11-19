use crate::test_guard::log_context;
#[test]
fn openai_rewrite_uses_input_prices_without_anthropic_write_premiums() {
    use crate::endpoints::{EndpointName, PricingMode, ProviderKind, ResolvedEndpoint, UrlSource};
    use crate::provider::credential::{Credential, Source};
    let endpoint = ResolvedEndpoint {
        client_version: None,
        name: EndpointName::parse("openai").unwrap(),
        provider: ProviderKind::OpenAi,
        credential: Credential::ApiKey("synthetic".into()),
        base_url: "http://127.0.0.1:1".into(),
        source: Source::Environment,
        key_env: None,
        url_source: UrlSource::Official,
        pricing: PricingMode::List,
    };
    let provider = crate::provider::openai::OpenAiProvider::from_endpoint(&endpoint).unwrap();
    let mut session = crate::agent::default_session();
    session.model = "gpt-6.1-sol".into();
    let info = crate::pricing::model_info(&session.model).unwrap();
    let expected = crate::pricing::estimate_usd(
        info,
        &wisp_protocol::Usage {
            input_tokens: 1000,
            ..Default::default()
        },
        session.cache_ttl,
    );
    let agent = crate::agent::Agent::new(std::sync::Arc::new(provider), session);
    assert_eq!(agent.rewrite_usd(1000), Some(expected));
}

#[test]
fn mixed_resume_notice_never_shows_a_summed_charge() {
    use serde_json::json;
    use wisp_protocol::log::LogRecord;
    let events = [
        json!({"type":"session_started","session_id":"mixed-session","wisp_version":"0.1.0","model":"claude-opus-5-5","effort":"medium","cache_ttl":"one_hour","endpoint":"https://api.anthropic.com","credential_source":"environment"}),
        json!({"type":"turn_started","turn":1,"text":"hello"}),
        json!({"type":"step_ended","turn":1,"step":1,"usage":wisp_protocol::Usage { billing: wisp_protocol::Billing::Metered, ..Default::default() },"cost_usd":12.3,"lower_bound":false}),
        json!({"type":"step_ended","turn":1,"step":2,"usage":wisp_protocol::Usage { billing: wisp_protocol::Billing::AnthropicPlan, ..Default::default() },"cost_usd":4.1,"lower_bound":false}),
    ];
    let records: Vec<LogRecord> = events
        .into_iter()
        .enumerate()
        .map(|(seq, mut event)| {
            event["seq"] = json!(seq);
            event["schema_version"] = json!(36);
            event["at"] = json!("2026-10-28T12:00:00Z");
            serde_json::from_value(event).unwrap()
        })
        .collect();
    let provider = crate::provider::anthropic::AnthropicProvider::new(
        &crate::provider::credential::Credential::ApiKey("synthetic".into()),
        "https://api.anthropic.com",
    )
    .unwrap();
    let agent = crate::agent::Agent::new(
        std::sync::Arc::new(provider),
        crate::agent::default_session(),
    )
    .with_log(
        crate::session::SessionLog::continuing(
            Box::new(crate::session::MemoryStore::default()),
            "mixed-session",
            records.len() as u64,
        ),
        log_context(),
    )
    .resume(&records, 0)
    .unwrap();
    let notice = agent.resume_notice_text().unwrap();
    assert!(notice.contains("Resumed session"), "{notice}");
    assert!(notice.contains("$12.30 · ~$4.10 subscriptions"), "{notice}");
    assert!(!notice.contains("$16.40"), "{notice}");
    assert_eq!(
        super::usd(16.4, true, wisp_protocol::Billing::Mixed, Some(4.1)),
        "$12.30+ · ~$4.10 subscriptions"
    );
    assert!(!super::usd(16.4, true, wisp_protocol::Billing::Mixed, None).contains('$'));
}

#[test]
fn resume_amounts_mark_estimates_with_a_tilde_and_lower_bounds_with_a_plus() {
    use wisp_protocol::Billing;
    assert_eq!(super::rewrite_cost_text(Some(0.41)), " (~$0.41)");
    assert_eq!(super::rewrite_cost_text(None), "");
    assert_eq!(
        super::usd(4.2, false, Billing::AnthropicPlan, None),
        "~$4.20"
    );
    assert_eq!(super::usd(4.2, true, Billing::ChatGptPlan, None), "~$4.20+");
    assert_eq!(super::usd(4.2, true, Billing::Metered, None), "$4.20+");
    assert_eq!(
        super::usd(4.2, true, Billing::Mixed, Some(3.2)),
        "$1.00+ · ~$3.20 subscriptions"
    );
}
