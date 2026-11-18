use futures::executor::block_on;
use wisp_plugin_api::manifest::{Manifest, Origin, Switch};
use wisp_plugin_testkit::host::{FakeCall, FakeHost};

use super::*;

fn plugin(host: &FakeHost) -> Arc<dyn Plugin> {
    instantiate(Arc::new(host.clone()))
}

#[test]
fn the_command_passes_the_typed_name_and_answers_with_the_kept_name_or_refusal() {
    let manifest = Manifest::parse(MANIFEST, Origin::Bundled).unwrap();
    assert_eq!(manifest.commands[0].name, COMMAND);
    assert_eq!(manifest.tools[0].name, NAME);
    assert_eq!(manifest.tools[0].default, Switch::On);
    assert!(manifest.tools[0].held);
    let host = FakeHost {
        rename_reply: Some(Ok("Strada".to_owned())),
        ..FakeHost::default()
    };
    let p = plugin(&host);
    assert_eq!(
        block_on(p.run_command(COMMAND, "\"Strada\"")),
        CommandReply::Note(renamed("Strada"))
    );
    assert_eq!(*host.chat_renamed.lock().unwrap(), ["\"Strada\""]);
    assert_eq!(
        block_on(p.run_command(COMMAND, " \n")),
        CommandReply::Note(refused(NO_NAME))
    );
    assert_eq!(host.chat_renamed.lock().unwrap().len(), 1);
    assert_eq!(
        block_on(p.run_command("other", "Strada")),
        CommandReply::None
    );
    let refusing = FakeHost {
        rename_reply: Some(Err("not here".to_owned())),
        ..FakeHost::default()
    };
    assert_eq!(
        block_on(plugin(&refusing).run_command(COMMAND, "Strada")),
        CommandReply::Note(refused("not here"))
    );
}

#[test]
fn prepare_checks_the_name_and_describes_the_row() {
    let p = plugin(&FakeHost::default());
    for (input, expected) in [
        (json!({}), "`name` must be a string"),
        (json!({"name": 1}), "`name` must be a string"),
        (json!({"name": " \n"}), "`name` is blank"),
        (
            json!({"name": "Strada", "other": 1}),
            "unknown field `other`",
        ),
        (json!([]), "input must be an object"),
    ] {
        assert_eq!(
            block_on(p.prepare(NAME, &input.to_string()))
                .err()
                .as_deref(),
            Some(expected)
        );
    }
    let prepared = block_on(p.prepare(NAME, r#"{"name":"Strada"}"#)).unwrap();
    let row = prepared.display();
    assert_eq!(row.target, "to Strada");
    assert_eq!(
        row.verbs,
        Some(Verbs {
            doing: "Renaming this chat".to_owned(),
            done: DONE.to_owned(),
            failed: "couldn't rename this chat".to_owned(),
        })
    );
}

#[test]
fn run_answers_with_the_kept_name_or_fails_with_the_reason() {
    for reply in [Ok("Strada".to_owned()), Err("not here".to_owned())] {
        let host = FakeHost {
            rename_reply: Some(reply.clone()),
            ..FakeHost::default()
        };
        let p = plugin(&host);
        let prepared = block_on(p.prepare(NAME, r#"{"name":"Strada"}"#)).unwrap();
        let run = block_on(p.run(prepared, &FakeCall::default()));
        let expected = match reply {
            Ok(kept) => {
                assert!(matches!(run.outcome, Outcome::Completed(_)));
                renamed(&kept)
            }
            Err(reason) => {
                assert_eq!(
                    run.outcome,
                    Outcome::Failed(Failure {
                        message: reason.clone(),
                        before_start: false
                    })
                );
                reason
            }
        };
        assert_eq!(run.result.as_deref(), Some(expected.as_str()));
        assert_eq!(*host.chat_renamed.lock().unwrap(), ["Strada"]);
    }
}
