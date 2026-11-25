use futures::executor::block_on;
use wisp_plugin_api::types::{Attachment, Completed, Outcome};
use wisp_plugin_testkit::host::{FakeCall, FakeHost};

use super::*;

fn board(report: Option<Value>) -> ShowPage {
    let screen_report = report.map(|report| ScreenReport {
        report: report.to_string(),
        images: vec![picture()],
    });
    let host = FakeHost {
        screen_report,
        ..FakeHost::default()
    };
    ShowPage {
        host: Arc::new(host),
    }
}

fn picture() -> Attachment {
    let (media_type, bytes) = ("image/jpeg".to_owned(), vec![0xFF, 0xD8, 0xFF]);
    Attachment {
        media_type,
        bytes,
        slice: None,
    }
}

fn shown_with(report: Option<Value>, input: Value) -> (Display, ToolRun) {
    let page = board(report);
    let prepared = block_on(page.prepare(NAME, &input.to_string())).unwrap();
    let display = prepared.display();
    (display, block_on(page.run(prepared, &FakeCall::default())))
}

fn shown(input: Value) -> (Display, ToolRun) {
    shown_with(None, input)
}

#[test]
fn a_whole_page_is_shown_and_the_description_names_its_limits_and_tell() {
    let (display, run) = shown(json!({"html": "<h1>Fractions</h1><script>tell('hi')</script>"}));
    assert_eq!(display.action, Action::Show);
    assert_eq!(display.target, "page");
    assert_eq!(display.output, OutputKind::None);
    assert_eq!(
        run.outcome,
        Outcome::Completed(Completed {
            is_error: false,
            exit: None
        })
    );
    assert_eq!(
        run.result.as_deref(),
        Some("Shown on the board; no screen answered.")
    );
    assert!(description().contains(&format!("{TELL}(text)")));
    assert!(description().contains(&format!("{} KiB", MAX_BYTES / 1024)));
    assert!(description().contains(&format!("1 to {MAX_PART_CHARS} letters")));
    assert!(description().contains("has no network"));
    let def = def();
    assert_eq!(def.system_line, None);
    let ToolKind::Client(client) = def.kind else {
        panic!("not a client tool");
    };
    assert_eq!(client.input_schema, schema().to_string());
    assert_eq!(client.meta.purpose, Purpose::Show);
    assert!(!client.meta.parallel_safe);
}

#[test]
fn a_part_is_shown_up_to_the_limits() {
    let part = format!("a{}", "-_9Z".repeat(MAX_PART_CHARS / 4))
        .chars()
        .take(MAX_PART_CHARS)
        .collect::<String>();
    let html = "x".repeat(MAX_BYTES);
    let (display, run) = shown(json!({"html": html, "part": part}));
    assert_eq!(display.target, part);
    assert_eq!(run.result.as_deref(), Some(answer(None).as_str()));
    let (display, _) = shown(json!({"html": "<p>3/4</p>", "part": "step-2"}));
    assert_eq!(display.target, "step-2");
}

#[test]
fn a_bad_call_is_refused_with_one_sentence() {
    let bad_part = format!(
        "`part` must be an id of 1 to {MAX_PART_CHARS} letters, digits, - and _, starting with a letter"
    );
    let cases = [
        (json!({"html": ""}), "`html` is empty".to_owned()),
        (json!({"html": " \n"}), "`html` is empty".to_owned()),
        (json!({"part": "a"}), "`html` must be a string".to_owned()),
        (json!({"html": 1}), "`html` must be a string".to_owned()),
        (
            json!({"html": "x".repeat(MAX_BYTES + 1)}),
            "`html` is 16,385 bytes; the limit is 16,384 bytes (16 KiB), so show the page in parts"
                .to_owned(),
        ),
        (json!({"html": "x", "part": ""}), bad_part.to_owned()),
        (json!({"html": "x", "part": "2nd"}), bad_part.to_owned()),
        (json!({"html": "x", "part": "a b"}), bad_part.to_owned()),
        (json!({"html": "x", "part": "é"}), bad_part.to_owned()),
        (
            json!({"html": "x", "part": "a".repeat(MAX_PART_CHARS + 1)}),
            bad_part.to_owned(),
        ),
        (json!({"html": "x", "part": 7}), bad_part.to_owned()),
        (
            json!({"html": "x", "mode": "add"}),
            "unknown field `mode`".to_owned(),
        ),
        (json!(["x"]), "input must be an object".to_owned()),
    ];
    for (input, error) in cases {
        let refused = block_on(board(None).prepare(NAME, &input.to_string()));
        assert_eq!(refused.err(), Some(error), "{input}");
    }
}

#[test]
fn the_answer_gives_the_boards_size_scale_and_capped_errors() {
    let page = json!({"html": "<p>3/4</p>"});
    let fits = json!({"width": 380.4, "height": 560, "zoom": 1, "errors": [], "touched": true});
    let (_, run) = shown_with(Some(fits), page.clone());
    assert_eq!(
        run.result.as_deref(),
        Some("Shown on the board, 380 × 560 px.")
    );
    assert_eq!(run.attachments, [picture()]);
    assert!(shown(page.clone()).1.attachments.is_empty());
    let nearly = json!({"width": 380, "height": 560, "zoom": 0.996});
    let (_, run) = shown_with(Some(nearly), page.clone());
    let nearly = "Shown on the board, 380 × 560 px. Scaled to 99 % to fit.";
    assert_eq!(run.result.as_deref(), Some(nearly));
    let long = "e".repeat(MAX_ERROR_CHARS + 1);
    let mut errors = vec!["TypeError: rocket is undefined (line 12)".to_owned(), long];
    errors.extend((2..=MAX_ERRORS).map(|n| format!("error {n}")));
    let scaled = json!({"width": 380, "height": 560, "zoom": 0.72, "errors": errors});
    let (_, run) = shown_with(Some(scaled), page);
    let cut = format!("{}…", "e".repeat(MAX_ERROR_CHARS - 1));
    let shown = [errors[0].as_str(), &cut, "error 2", "error 3", "error 4"].join("; ");
    let expected =
        format!("Shown on the board, 380 × 560 px. Scaled to 72 % to fit. Script errors: {shown}.");
    assert_eq!(run.result.as_deref(), Some(expected.as_str()));
}
