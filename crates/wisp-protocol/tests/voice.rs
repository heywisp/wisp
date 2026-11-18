#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "fixture helpers fail the test on any read, write or parse error"
)]

mod support;

use support::golden;
use wisp_protocol::voice::{
    AudioFormat, CallEnd, CallId, CallOutcome, FRAME_MS, VoiceCommand, VoiceEvent,
};
use wisp_protocol::{AccountSpend, AgentEvent, Billing, ClientCommand, ErrorKind};

fn call() -> CallId {
    CallId("web-1".into())
}

#[test]
fn voice_commands_and_events_match_their_fixtures() {
    let commands = [
        (
            "command_voice_start",
            VoiceCommand::Start {
                call: call(),
                format: AudioFormat::Pcm16Mono48k,
            },
        ),
        ("command_voice_stop", VoiceCommand::Stop { call: call() }),
        (
            "command_voice_note",
            VoiceCommand::Note {
                call: call(),
                text: "Tapped: the red circle".into(),
            },
        ),
    ];
    for (name, command) in commands {
        golden(name, &ClientCommand::Voice { command });
    }
    let events = [
        ("event_voice_started", VoiceEvent::Started { call: call() }),
        (
            "event_voice_ended_hangup",
            VoiceEvent::Ended {
                call: call(),
                outcome: CallOutcome::Completed {
                    end: CallEnd::Hangup,
                },
                seconds: 95,
                cost: AccountSpend::of(0.25, None, Billing::ChatGptPlan, false),
            },
        ),
        (
            "event_voice_ended_failed",
            VoiceEvent::failed(call(), ErrorKind::RateLimit, "The voice limit was reached."),
        ),
    ];
    for (name, event) in events {
        golden(name, &AgentEvent::Voice { event });
    }
    let newer: CallEnd = serde_json::from_str("\"limit\"").unwrap();
    assert_eq!(newer, CallEnd::Unknown);
}

#[test]
fn the_one_format_is_pcm16_mono_at_48k_in_20ms_frames() {
    let format = AudioFormat::Pcm16Mono48k;
    assert_eq!(FRAME_MS, 20);
    assert_eq!(format.sample_rate(), 48_000);
    assert_eq!(format.frame_samples(), 960);
}
