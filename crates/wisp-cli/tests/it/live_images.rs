#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a live check fails the test on any setup or network error"
)]

use crate::live;

use std::sync::Arc;

use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder, Rgb, RgbImage};
use live::turn_lines::with_turn_lines;
use live::{affords, live_session, next_event, next_id, provider, scrubbed, secrets, worst_usd};
use tokio::sync::mpsc;
use wisp_core::agent::{self, Agent};
use wisp_protocol::{AgentEvent, ClientCommand, ImageData, TurnOutcome, Usage};

const MODEL: &str = "claude-sonnet-5-5";
const CAP_USD: f64 = 0.04;
const MAX_TOKENS: u32 = 200;
const SIDE: u32 = 800;
const FACTS: u32 = 40;
const SYSTEM_TOKENS_MAX: u64 = 1_300;
const TEXT_TURN_TOKENS_MAX: u64 = 300;
const IMAGE_TURN_TOKENS_MAX: u64 = 1_400;

fn system_prompt() -> String {
    let facts: Vec<String> = (1..=FACTS)
        .map(|i| {
            format!(
                "Fact {i}: the lighthouse at harbour {i} burns lamp type {} and was last painted in {}.",
                (i * 7) % 13,
                1900 + i
            )
        })
        .collect();
    format!(
        "You are Wisp, a helpful assistant running in a terminal. Answer in one short sentence. \
The reference notes below are for a later task; don't mention them unless asked.\n{}",
        facts.join("\n")
    )
}

fn quadrants() -> Vec<u8> {
    let half = SIDE / 2;
    let image = RgbImage::from_fn(SIDE, SIDE, |x, y| match (x < half, y < half) {
        (true, true) => Rgb([220, 30, 30]),
        (false, true) => Rgb([30, 160, 60]),
        (true, false) => Rgb([30, 60, 220]),
        (false, false) => Rgb([240, 200, 20]),
    });
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(image.as_raw(), SIDE, SIDE, ExtendedColorType::Rgb8)
        .unwrap();
    out
}

fn worst_turn_usd(messages_tokens: u64) -> f64 {
    worst_usd(MODEL, 0, SYSTEM_TOKENS_MAX, messages_tokens, MAX_TOKENS)
}

fn worst_messages() -> [u64; 3] {
    let first = TEXT_TURN_TOKENS_MAX;
    let second = first + u64::from(MAX_TOKENS) + IMAGE_TURN_TOKENS_MAX;
    let third = second + u64::from(MAX_TOKENS) + TEXT_TURN_TOKENS_MAX;
    [first, second, third]
}

#[test]
fn all_three_live_turns_fit_under_the_cap_at_their_worst() {
    let worst: f64 = worst_messages().iter().map(|p| worst_turn_usd(*p)).sum();
    assert!(worst > 0.0 && worst <= CAP_USD, "{worst}");
    let tokens = wisp_media::tokens(SIDE, SIDE);
    assert_eq!(tokens, 841);
    assert!(tokens + TEXT_TURN_TOKENS_MAX <= IMAGE_TURN_TOKENS_MAX);
    let normalised = wisp_media::normalise(&quadrants(), wisp_media::Tier::HIGH).unwrap();
    assert_eq!((normalised.width, normalised.height), (SIDE, SIDE));
    assert!(system_prompt().len() / 3 < usize::try_from(SYSTEM_TOKENS_MAX).unwrap());
}

struct Measured {
    usage: Usage,
    cost: f64,
    estimate: u64,
}

struct Live {
    commands: mpsc::Sender<ClientCommand>,
    events: mpsc::Receiver<AgentEvent>,
    spent: f64,
}

impl Live {
    fn new() -> Live {
        let mut session = live_session(MODEL, MAX_TOKENS);
        session.system = Some(system_prompt());
        let (commands, events, _task) =
            agent::spawn(with_turn_lines(Agent::new(Arc::new(provider()), session)));
        Live {
            commands,
            events,
            spent: 0.0,
        }
    }

    async fn turn(&mut self, worst_prompt: u64, text: &str, images: Vec<ImageData>) -> Measured {
        assert!(
            affords(self.spent, worst_turn_usd(worst_prompt), CAP_USD),
            "the cost guard stopped the turn"
        );
        let estimate = if images.is_empty() {
            0
        } else {
            wisp_media::tokens(SIDE, SIDE)
        };
        self.commands
            .send(ClientCommand::Send {
                id: wisp_protocol::InputId(next_id()),
                text: text.to_owned(),
                images,
                mode: wisp_protocol::DeliveryMode::Steer,
                turn: None,
                replaces: None,
            })
            .await
            .unwrap();
        loop {
            match next_event(&mut self.events).await {
                AgentEvent::StepEnded { cost_usd, .. } => {
                    self.spent += cost_usd.unwrap_or(0.0);
                    if self.spent > CAP_USD {
                        self.commands.send(ClientCommand::Cancel).await.unwrap();
                    }
                }
                AgentEvent::Rejected { reason, .. } => {
                    panic!("rejected: {}", secrets().scrub(&reason))
                }
                AgentEvent::TurnEnded { outcome, .. } => {
                    return match outcome {
                        TurnOutcome::Completed { usage, cost, .. } => Measured {
                            usage,
                            cost: cost.map_or(0.0, |c| c.turn_usd),
                            estimate,
                        },
                        other => panic!("the turn didn't complete: {}", scrubbed(&other)),
                    };
                }
                _ => {}
            }
        }
    }
}

fn report(name: &str, m: &Measured) {
    let u = &m.usage;
    eprintln!(
        "live {name}: input {} · cache read {} · cache write {} (5m {:?}, 1h {:?}) · output {} · prompt {} · image estimate {} · ${:.5}",
        u.input_tokens,
        u.cache_read_input_tokens,
        u.cache_creation_input_tokens,
        u.cache_creation_5m_input_tokens,
        u.cache_creation_1h_input_tokens,
        u.output_tokens,
        u.prompt_tokens(),
        m.estimate,
        m.cost
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: costs real money; run by hand with credentials in the environment"]
async fn live_an_image_turn_and_the_prompt_cache() {
    let mut live = Live::new();
    let [p1, p2, p3] = worst_messages();

    let first = live
        .turn(p1, "Reply with the single word ready.", Vec::new())
        .await;
    report("turn 1 (text)", &first);

    let image = ImageData {
        data: wisp_media::encode_base64(&quadrants()).into(),
    };
    let second = live
        .turn(
            p2,
            "Name the four colours in this image, one word each.",
            vec![image],
        )
        .await;
    report("turn 2 (800x800 PNG)", &second);

    let third = live
        .turn(
            p3,
            "Which of those colours is warmest? One word.",
            Vec::new(),
        )
        .await;
    report("turn 3 (text)", &third);

    let before_image = first.usage.prompt_tokens();
    let image_turn = second.usage.prompt_tokens();
    let image_tokens = image_turn.saturating_sub(before_image);
    eprintln!(
        "live: the image turn added {image_tokens} prompt tokens (image estimate {}, plus the stamp, text and turn 1's reply)",
        second.estimate
    );
    let kept_before = second.usage.cache_read_input_tokens >= before_image / 2;
    let kept_after = third.usage.cache_read_input_tokens >= image_turn / 2;
    eprintln!(
        "live verdict: adding the image {} the cached prefix before it; the turn after it {} the image turn's prefix",
        if kept_before { "kept" } else { "rebuilt" },
        if kept_after { "read" } else { "missed" }
    );
    eprintln!("live: total spent ${:.4} of ${CAP_USD}", live.spent);
    assert!(
        first.usage.cache_creation_input_tokens > 0,
        "turn 1 wrote no cache"
    );
    assert!(image_tokens >= second.estimate / 2, "{image_tokens}");
    assert!(live.spent <= CAP_USD, "spent ${:.4}", live.spent);
}
