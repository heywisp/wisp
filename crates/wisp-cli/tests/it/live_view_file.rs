#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a live check fails the test on any setup or network error"
)]

use crate::live;

use std::io::Cursor;

use crate::scratch::ScratchDir;
use live::{affords, agent_with, live_session, next_event, next_id, worst_cached_step_usd};
use tokio::sync::mpsc;
use wisp_core::agent;
use wisp_protocol::{AgentEvent, ClientCommand, TurnOutcome, WithdrawnAttachment};

const MODEL: &str = "claude-sonnet-5-5";
const CAP_USD: f64 = 0.05;
const MAX_TOKENS: u32 = 1024;
const PREFIX_TOKENS: u64 = 2_000;
const VIEW_TOKENS: u64 = 5_000;
const WRAPPER_TOKENS: u64 = 200;
const SECOND_LINE: &str = "the quiet heron counts eleven boats";
const PDF_WORD: &str = "LANTERN";

fn step_tokens(calls: u64) -> u64 {
    calls * VIEW_TOKENS + u64::from(MAX_TOKENS) + WRAPPER_TOKENS
}

fn worst_next_usd(prompt_tokens: u64, calls: u64) -> f64 {
    worst_cached_step_usd(
        MODEL,
        prompt_tokens,
        PREFIX_TOKENS,
        step_tokens(calls),
        MAX_TOKENS,
    )
}

#[test]
fn the_worst_first_request_fits_under_the_cap() {
    let first = worst_next_usd(0, 0);
    assert!(first > 0.0 && first < CAP_USD, "{first}");
    assert!(worst_next_usd(20_000, 1) > worst_next_usd(1, 1));
    let threshold = CAP_USD - worst_next_usd(12_000, 1);
    assert!(threshold > 0.02, "{threshold}");
}

#[test]
fn the_last_view_still_fits_after_the_expected_spend() {
    assert!(affords(0.02, worst_next_usd(12_000, 1), CAP_USD));
    assert!(!affords(0.02, worst_next_usd(12_000, 4), CAP_USD));
}

#[test]
fn the_fixtures_are_what_the_turns_ask_about() {
    let scratch = fixtures();
    let png = std::fs::read(scratch.root.join("red.png")).unwrap();
    let decoded = image::load_from_memory(&png).unwrap().to_rgb8();
    assert_eq!(decoded.dimensions(), (64, 64));
    assert!(decoded.pixels().all(|p| p.0 == [255, 0, 0]));
    let damaged = std::fs::read(scratch.root.join("broken.png")).unwrap();
    assert_eq!(damaged[..33], png[..33]);
    assert!(image::load_from_memory(&damaged).is_err());
    let pdf = std::fs::read(scratch.root.join("note.pdf")).unwrap();
    assert!(pdf.starts_with(b"%PDF-1.4\n"));
    let text = String::from_utf8_lossy(&pdf);
    let xref = text.find("xref\n").unwrap();
    let start: usize = text
        .split("startxref\n")
        .nth(1)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(start, xref);
    for n in 1..=5 {
        let offset = text[xref..]
            .lines()
            .nth(2 + n)
            .unwrap()
            .split(' ')
            .next()
            .unwrap()
            .parse::<usize>()
            .unwrap();
        assert!(text[offset..].starts_with(&format!("{n} 0 obj")), "{n}");
    }
    let notes = std::fs::read_to_string(scratch.root.join("notes.txt")).unwrap();
    assert_eq!(notes.lines().nth(1), Some(SECOND_LINE));
}

fn red_png() -> Vec<u8> {
    let img = image::RgbImage::from_pixel(64, 64, image::Rgb([255, 0, 0]));
    let mut out = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

fn damaged_png(png: &[u8]) -> Vec<u8> {
    let mut out = png[..33].to_vec();
    out.extend((0..400u32).map(|i| (i.wrapping_mul(37) ^ 0xA5) as u8));
    out
}

fn one_page_pdf() -> Vec<u8> {
    let stream = format!("BT /F1 36 Tf 72 700 Td ({PDF_WORD}) Tj ET");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_owned(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
    ];
    let mut out = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.push_str(&format!("{} 0 obj\n{body}\nendobj\n", i + 1));
    }
    let xref = out.len();
    out.push_str(&format!(
        "xref\n0 {}\n0000000000 65535 f \n",
        objects.len() + 1
    ));
    for offset in offsets {
        out.push_str(&format!("{offset:010} 00000 n \n"));
    }
    out.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    ));
    out.into_bytes()
}

fn fixtures() -> ScratchDir {
    let scratch = ScratchDir::new("wisp-live-", "view-file");
    let root = &scratch.root;
    let png = red_png();
    std::fs::write(root.join("red.png"), &png).unwrap();
    std::fs::write(root.join("broken.png"), damaged_png(&png)).unwrap();
    std::fs::write(root.join("note.pdf"), one_page_pdf()).unwrap();
    std::fs::write(
        root.join("notes.txt"),
        format!("Notes for the harbour\n{SECOND_LINE}\nnothing else today\n"),
    )
    .unwrap();
    scratch
}

struct Turn {
    outcome: TurnOutcome,
    text: String,
    paths: Vec<String>,
    withdrawn: Vec<WithdrawnAttachment>,
}

struct Live {
    commands: mpsc::Sender<ClientCommand>,
    events: mpsc::Receiver<AgentEvent>,
    spent: f64,
    prompt_tokens: u64,
    calls: u64,
    guarded: bool,
}

impl Live {
    fn new(scratch: &ScratchDir) -> Live {
        let agent = agent_with(
            live_session(MODEL, MAX_TOKENS),
            &["view_file"],
            &scratch.root,
            &scratch.dir("tmp"),
        );
        let (commands, events, _task) = agent::spawn(agent);
        Live {
            commands,
            events,
            spent: 0.0,
            prompt_tokens: 0,
            calls: 0,
            guarded: false,
        }
    }

    async fn turn(&mut self, text: &str) -> Turn {
        let mut turn = Turn {
            outcome: TurnOutcome::Cancelled {
                usage: None,
                cost: None,
                committed_steps: 0,
            },
            text: String::new(),
            paths: vec![],
            withdrawn: vec![],
        };
        if !self.affords_next() {
            return turn;
        }
        self.commands
            .send(ClientCommand::send(next_id(), text))
            .await
            .unwrap();
        loop {
            let event = next_event(&mut self.events).await;
            match &event {
                AgentEvent::StepEnded {
                    cost_usd, usage, ..
                } => {
                    self.spent += cost_usd.unwrap_or(0.0);
                    if let Some(usage) = usage {
                        self.prompt_tokens = usage.prompt_tokens();
                    }
                    if !self.guarded && !self.affords_next() {
                        self.commands.send(ClientCommand::Cancel).await.unwrap();
                    }
                    self.calls = 0;
                }
                AgentEvent::ToolCallRequested { tool, input, .. } => {
                    self.calls += 1;
                    let path = input["path"].as_str().unwrap_or_default();
                    turn.paths.push(format!("{tool} {path}"));
                }
                AgentEvent::TextDelta { text, .. } => turn.text.push_str(text),
                AgentEvent::AttachmentsWithdrawn { items, .. } => {
                    turn.withdrawn.extend(items.iter().cloned());
                }
                _ => {}
            }
            if let AgentEvent::TurnEnded { outcome, .. } = event {
                turn.outcome = outcome;
                eprintln!(
                    "live: spent ${:.4}; calls {:?}; withdrawn {:?}\n{}",
                    self.spent, turn.paths, turn.withdrawn, turn.text
                );
                return turn;
            }
        }
    }

    fn affords_next(&mut self) -> bool {
        let fits = affords(
            self.spent,
            worst_next_usd(self.prompt_tokens, self.calls),
            CAP_USD,
        );
        self.guarded |= !fits;
        fits
    }

    fn completed(&self, turn: &Turn, n: u32) {
        assert!(
            !self.guarded,
            "the cost guard stopped turn {n} at ${:.4}",
            self.spent
        );
        assert!(
            matches!(turn.outcome, TurnOutcome::Completed { .. }),
            "turn {n}: {:?}",
            turn.outcome
        );
        assert!(
            turn.paths.iter().any(|p| p.starts_with("view_file ")),
            "turn {n} viewed nothing: {:?}",
            turn.paths
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: costs real money; run by hand with credentials in the environment"]
async fn live_view_file_reads_an_image_a_pdf_and_text_and_survives_a_refused_image() {
    let scratch = fixtures();
    let mut live = Live::new(&scratch);

    let turn = live
        .turn("Use view_file to look at red.png. What colour is it? Reply with one word.")
        .await;
    live.completed(&turn, 1);
    assert!(turn.text.to_lowercase().contains("red"), "{}", turn.text);

    let turn = live
        .turn(
            "Use view_file to look at note.pdf. What word does it show? Reply with only that word.",
        )
        .await;
    live.completed(&turn, 2);
    assert!(turn.text.to_uppercase().contains(PDF_WORD), "{}", turn.text);

    let turn = live
        .turn("Use view_file to read notes.txt. Reply with only its second line, exactly.")
        .await;
    live.completed(&turn, 3);
    assert!(turn.text.contains(SECOND_LINE), "{}", turn.text);

    let turn = live
        .turn("Use view_file to look at broken.png and tell me in one sentence what happened.")
        .await;
    live.completed(&turn, 4);
    assert!(!turn.withdrawn.is_empty(), "nothing was withdrawn");
    assert!(
        turn.withdrawn.iter().all(|w| w.call_id.is_some()),
        "{:?}",
        turn.withdrawn
    );

    assert!(!live.guarded);
    assert!(live.spent <= CAP_USD, "spent ${:.4}", live.spent);
    eprintln!("live: total spent ${:.4}", live.spent);
}
