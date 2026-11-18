use wisp_core::agent::{self, Agent, LogContext, default_session};
use wisp_core::provider::anthropic::{AnthropicProvider, Credential as ApiKey};
use wisp_core::session::{MemoryStore, Redactions, SessionLog, SessionStore};
use wisp_protocol::endpoint::EndpointEdit;
use wisp_protocol::log::CredentialSource;
use wisp_protocol::{AgentEvent, ClientCommand, RequestId};

use super::*;
use crate::turn_lines::with_turn_lines;

const NEW_KEY: &str = "sk-ant-CANARYEDITKEYe5f6";

fn failing_reads_on_probe(memory: &MemoryKeyring) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let memory = memory.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut buf = [0u8; 8192];
            let _ = stream.read(&mut buf);
            memory.set_mode(MemoryMode::ReadsFail(BackendError::PlatformFailure));
            let body = r#"{"input_tokens":3}"#;
            let reply = format!(
                "HTTP/1.1 200 X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    format!("http://127.0.0.1:{port}")
}

fn store_id(place: &Place) -> String {
    place
        .read("config.toml")
        .lines()
        .find_map(|l| l.strip_prefix("store_id = \""))
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn an_unconfirmed_key_edit_names_no_store_id_in_any_event_or_the_log() {
    let place = Place::new();
    let memory = MemoryKeyring::new();
    let url = failing_reads_on_probe(&memory);
    let host = place.host(memory_keychain(&memory));
    let saved = host
        .save(
            spec("gw", Some(url), StoreChoice::Keyring),
            Secret::new(KEY),
            false,
            idle(),
        )
        .await
        .unwrap();
    assert!(matches!(saved, SaveOutcome::Saved(_)), "{saved:?}");
    let id = store_id(&place);
    let provider =
        AnthropicProvider::new(&ApiKey::ApiKey("sk-test".into()), "http://127.0.0.1:9").unwrap();
    let store = MemoryStore::default();
    let context = LogContext {
        endpoint: "http://127.0.0.1:9".into(),
        endpoint_name: "env".into(),
        credential_source: CredentialSource::Environment,
        redactions: Redactions::new([KEY.to_owned(), NEW_KEY.to_owned()]),
    };
    let agent = with_turn_lines(Agent::new(Arc::new(provider), default_session()))
        .with_endpoint_host(Arc::new(host))
        .with_log(SessionLog::new(Box::new(store.clone()), "s1"), context);
    let (commands, mut events, task) = agent::spawn(agent);
    commands
        .send(ClientCommand::EditEndpoint {
            request: RequestId(1),
            name: "gw".into(),
            edit: EndpointEdit {
                key: Some(Secret::new(NEW_KEY)),
                ..EndpointEdit::default()
            },
            store: None,
            check: true,
        })
        .await
        .unwrap();
    let mut seen = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .unwrap()
            .unwrap();
        let answered = matches!(
            &event,
            AgentEvent::Rejected { request: Some(r), .. }
                | AgentEvent::EndpointEdited { answers: r, .. }
                | AgentEvent::EndpointNotSaved { answers: r, .. }
                if *r == RequestId(1)
        );
        seen.push(event);
        if answered {
            break;
        }
    }
    drop(commands);
    while let Some(event) = events.recv().await {
        seen.push(event);
    }
    assert!(task.join().await);
    let shown = format!("{seen:?}");
    let logged = serde_json::to_string(&store.read("s1").unwrap()).unwrap();
    for text in [&shown, &logged] {
        assert!(text.contains("may have been saved there"), "{text}");
        assert!(!text.contains(&id), "{text}");
        assert!(!text.contains("account "), "{text}");
        no_canary(text);
        assert!(!text.contains(NEW_KEY), "{text}");
    }
}
