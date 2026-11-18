use super::*;
use crate::agent::tool_tests::steering::transcript::shown;
use crate::provider::Delta;
use crate::resume::transcript;

fn streamed_citation() -> Step {
    let mut script = vec![Ok(ProviderEvent::Sending), Ok(start(600))];
    script.extend(events_of(&[B::Found("s1")]));
    script.extend([
        Ok(ProviderEvent::BlockStart {
            index: 1,
            block: ContentBlock::Text {
                text: String::new(),
            },
        }),
        Ok(ProviderEvent::BlockDelta {
            index: 1,
            delta: Delta::Text("ripgrep is fast".into()),
        }),
        Ok(ProviderEvent::BlockDelta {
            index: 1,
            delta: Delta::Unknown {
                provider: "anthropic".into(),
                raw: json!({"type": "citations_delta", "citation": {
                    "type": "web_search_result_location",
                    "url": "https://example.com/s1",
                    "title": "Cited page",
                    "encrypted_index": "ei-1",
                    "cited_text": "quoted"
                }}),
            },
        }),
        Ok(ProviderEvent::BlockStop { index: 1 }),
    ]);
    script.extend(testing::ended(
        StopReason::EndTurn,
        usage(600, 0, 10),
        Vec::new(),
    ));
    Step::from(script)
}

#[tokio::test]
async fn a_replayed_search_shows_the_cited_reply_that_live_showed() {
    let mut rig = rig(vec![
        paused(&[B::Think("plan"), B::Search("s1", "rust ripgrep")]),
        streamed_citation(),
    ]);
    let (_, live) = rig.turn("search").await;
    let committed = log_events(&rig.store).into_iter().any(|e| {
        matches!(&e, LogEvent::MessageCommitted { message, .. }
            if message.content.iter().any(|b| matches!(b, wisp_protocol::log::LoggedBlock::Inline(ContentBlock::Opaque { raw, .. })
                if raw["type"] == "text" && raw.get("citations").is_some())))
    });
    assert!(committed, "the cited text is logged as an opaque block");
    let replayed = transcript(&records(&rig.store), &crate::tool::Toolbox::default(), None);
    let lines = shown(&replayed);
    let live_lines: Vec<String> = shown(&live)
        .into_iter()
        .filter(|l| !l.starts_with("thinking"))
        .collect();
    let replayed_lines: Vec<String> = lines
        .iter()
        .filter(|l| !l.starts_with("thinking") && !l.starts_with("ended"))
        .cloned()
        .collect();
    assert_eq!(replayed_lines, live_lines, "{replayed:#?}");
    assert!(
        lines.contains(&"text: ripgrep is fast".to_owned()),
        "{lines:#?}"
    );
}
