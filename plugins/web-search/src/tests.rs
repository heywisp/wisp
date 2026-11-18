use futures::executor::block_on;
use wisp_plugin_api::manifest::{Capability, Manifest, Origin};
use wisp_plugin_api::model::{Reply, Source};
use wisp_plugin_testkit::host::{FakeCall, FakeHost};

use super::*;

#[test]
fn the_definition_and_manifest_are_fixed_and_add_no_instruction() {
    let manifest = Manifest::parse(MANIFEST, Origin::Bundled).unwrap();
    assert_eq!(manifest.world, "0.4.0");
    assert_eq!(manifest.capabilities.requested, [Capability::Model]);
    assert_eq!(manifest.tools[0].native.as_deref(), Some(FAMILY));
    let def = def();
    assert_eq!(def.name, NAME);
    assert!(def.system_line.is_none() && def.context_line.is_none());
    let ToolKind::Client(client) = def.kind else {
        panic!("not a client tool");
    };
    assert_eq!(
        client.description,
        "Search the web. Returns the facts found, each with its source, then a numbered list of the sources. Use it for anything recent or that you are not sure of."
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&client.input_schema).unwrap(),
        serde_json::json!({"type":"object","properties":{"query":{"type":"string","description":"What to search for, in plain words. Make it complete in itself: the search sees none of this conversation."}},"required":["query"]})
    );
}

#[test]
fn a_query_is_required_and_its_row_is_a_search() {
    let plugin = instantiate(Arc::new(FakeHost::default()));
    for input in ["{}", "null", "{\"query\":0}", "{\"query\":\"  \"}"] {
        assert!(block_on(plugin.prepare(NAME, input)).is_err());
    }
    let call = block_on(plugin.prepare(NAME, r#"{"query":"a complete query"}"#)).unwrap();
    assert_eq!(call.display().action, Action::Search);
    assert_eq!(call.display().target, "\"a complete query\"");
    let ctx = BuildContext {
        platform: "unix".into(),
        capabilities: Vec::new(),
    };
    assert_eq!(block_on(plugin.describe(&ctx)), [def()]);
}

#[test]
fn an_ask_sends_only_the_query_and_instruction_and_formats_sources_or_no_results() {
    for sources in [
        Vec::new(),
        vec![Source {
            title: "Example".into(),
            url: "https://example.com/facts".into(),
        }],
    ] {
        let host = Arc::new(FakeHost::default());
        host.model_replies.lock().unwrap().push_back(Ok(Reply {
            text: "Findings".into(),
            sources: sources.clone(),
        }));
        let plugin = instantiate(host.clone());
        let call = block_on(plugin.prepare(NAME, r#"{"query":"a complete query"}"#)).unwrap();
        let run = block_on(plugin.run(call, &FakeCall::default()));
        assert!(matches!(run.outcome, Outcome::Completed(_)));
        assert_eq!(
            run.result.as_deref(),
            Some(if sources.is_empty() {
                "no results"
            } else {
                "Findings\n\n1. Example — https://example.com/facts"
            })
        );
        assert_eq!(
            *host.model_prompts.lock().unwrap(),
            [Prompt {
                system: INSTRUCTION.into(),
                text: "a complete query".into(),
                max_tokens: 4096,
                native: Some(FAMILY.into())
            }]
        );
    }
}

#[test]
fn every_model_error_is_failed_or_cancelled_never_completed() {
    for error in [
        AskError::NotAllowed("off".into()),
        AskError::Refused("native_search".into()),
        AskError::Failed("network".into()),
        AskError::Cancelled,
    ] {
        let host = Arc::new(FakeHost::default());
        host.model_replies
            .lock()
            .unwrap()
            .push_back(Err(error.clone()));
        let plugin = instantiate(host);
        let call = block_on(plugin.prepare(NAME, r#"{"query":"a complete query"}"#)).unwrap();
        let run = block_on(plugin.run(call, &FakeCall::default()));
        match error {
            AskError::Cancelled => {
                assert!(matches!(run.outcome, Outcome::Cancelled));
                assert_eq!(run.result.as_deref(), Some("web search cancelled"));
            }
            AskError::NotAllowed(reason) | AskError::Refused(reason) | AskError::Failed(reason) => {
                assert!(matches!(run.outcome, Outcome::Failed(_)));
                assert_eq!(run.result, Some(format!("web search failed: {reason}")));
            }
        }
    }
}
