use wisp_protocol::{AgentEvent, Scope, SourceChange, SourceInfo};
use wisp_render::theme::Mode;
use wisp_render::theme::glyphs::GlyphSet;

use super::{Stage, WIDTHS, finished_turn, snapshot, staged, truecolor};

fn info(scope: Scope, path: &str) -> Box<SourceInfo> {
    Box::new(SourceInfo {
        scope,
        name: "AGENTS.md".into(),
        display_path: path.into(),
        bytes: 10,
        sha256: "ab".repeat(32),
        imported_by: None,
        external: false,
    })
}

fn change(path: &str) -> SourceChange {
    SourceChange {
        display_path: path.into(),
        old_sha256: Some("ab".repeat(32)),
        new_sha256: Some("cd".repeat(32)),
    }
}

struct Record {
    turn: u64,
    epoch: Option<u64>,
    retry_of: Option<u64>,
    ahead: bool,
    sent: bool,
    to: Scope,
}

fn changed(record: Record, paths: &[&str]) -> AgentEvent {
    let to = match record.to {
        Scope::Default => info(Scope::Default, "~/.config/wisp/AGENTS.md"),
        _ => info(Scope::Project, "~/Projects/Wisp/AGENTS.md"),
    };
    AgentEvent::InstructionsChanged {
        turn: Some(record.turn),
        epoch: record.epoch,
        retry_of: record.retry_of,
        ahead: record.ahead,
        from: Some(info(Scope::Project, "~/Projects/Wisp/AGENTS.md")),
        to: Some(to),
        changed: paths.iter().map(|p| change(p)).collect(),
        chosen: None,
        imports: Vec::new(),
        candidates: Vec::new(),
        sent: record.sent,
        diagnostics: Vec::new(),
    }
}

fn record(turn: u64) -> Record {
    Record {
        turn,
        epoch: None,
        retry_of: None,
        ahead: false,
        sent: true,
        to: Scope::Project,
    }
}

async fn notes(s: Stage) {
    finished_turn(&s, "Make cache misses name their cause").await;
    let wisp = "~/Projects/Wisp/AGENTS.md";
    let docs = "~/Projects/Wisp/docs/caching.md";
    s.event(changed(record(2), &[wisp, docs])).await;
    s.event(changed(
        Record {
            epoch: Some(1),
            sent: false,
            ..record(2)
        },
        &[wisp],
    ))
    .await;
    s.event(changed(
        Record {
            epoch: Some(1),
            ..record(2)
        },
        &[docs],
    ))
    .await;
    s.event(changed(
        Record {
            retry_of: Some(2),
            ..record(3)
        },
        &[wisp],
    ))
    .await;
    s.event(changed(
        Record {
            epoch: Some(2),
            ahead: true,
            ..record(3)
        },
        &[wisp],
    ))
    .await;
    s.event(changed(
        Record {
            to: Scope::Default,
            ..record(4)
        },
        &[wisp],
    ))
    .await;
    s.wait(4000).await.shot("notes");
}

#[tokio::test(start_paused = true)]
async fn the_instructions_notes_show_for_a_sent_delta_and_a_switch_only() {
    for width in WIDTHS {
        let look = truecolor(Mode::Dark, GlyphSet::Unicode, width);
        let shots = staged(look, |s| Box::pin(notes(s))).await;
        let shot = &shots[0].1;
        assert_eq!(shot.matches("instructions updated").count(), 2, "{shot}");
        assert_eq!(shot.matches("instructions switched").count(), 1, "{shot}");
        snapshot("instructions", look, &shots);
    }
}
