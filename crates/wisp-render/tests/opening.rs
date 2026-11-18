use wisp_protocol::Opening;
use wisp_render::chrome::{Credential, Facts, endpoint_facts_of};

#[test]
fn every_opening_fact_reaches_the_drawing_facts() {
    let opening: Opening = serde_json::from_str(include_str!(
        "../../wisp-protocol/tests/fixtures/opening.json"
    ))
    .unwrap();
    let expected = opening.clone();
    let facts = Facts::from(opening);
    assert_eq!(facts.version, expected.version);
    assert_eq!(facts.provider, expected.provider);
    assert_eq!(facts.model, expected.model);
    assert_eq!(facts.effort, expected.effort);
    assert_eq!(facts.cache_ttl, expected.cache_ttl);
    assert_eq!(facts.credential, Some(Credential::ApiKey));
    assert_eq!(facts.host, expected.host);
    assert_eq!(facts.log, expected.log);
    assert_eq!(facts.home, expected.home);
    assert_eq!(facts.cwd, expected.cwd);
    assert_eq!(facts.endpoint, expected.endpoint);
    assert_eq!(facts.models, expected.models);
    assert_eq!(facts.on_accounts, expected.on_accounts);
    assert_eq!(facts.write_multipliers, expected.write_multipliers);
    assert_eq!(facts.next, expected.next);
    assert_eq!(facts.max_tokens, expected.max_tokens);
    assert_eq!(facts.endpoint_name, expected.endpoint_name);
    assert_eq!(
        facts.endpoints,
        expected
            .endpoints
            .iter()
            .map(endpoint_facts_of)
            .collect::<Vec<_>>()
    );
    assert_eq!(facts.endpoint_note, expected.endpoint_note);
    assert_eq!(facts.resume_refused, expected.resume_refused);
    assert_eq!(facts.instructions, expected.instructions);
    assert_eq!(facts.search, expected.search);
    assert_eq!(facts.resumed, expected.resumed);
    assert_eq!(facts.claude_usage, expected.claude_usage);
    assert_eq!(facts.start_pick, expected.start_pick);
}
