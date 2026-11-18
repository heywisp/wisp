use super::*;
use crate::chrome::AgentRow;
use crate::chrome::ProcessRow;
use crate::styled::StyledLine;
use crate::testing::{child, line_text, spend};
use crate::theme::Ink;
use crate::theme::glyphs::GlyphSet;
use wisp_protocol::{AgentState, Billing};

const SESSION: &str = "01a18e69-13bd-7c2e-9d4f-3a5b6c7d8e9f";
const TWO_H_14_MIN: u64 = 8_040_000;

fn summary(main: AccountSpend, rows: &[AgentRow]) -> Summary {
    Summary {
        turns: 3,
        stopped: 0,
        duration_ms: TWO_H_14_MIN,
        total: shown_total(main, rows, ChatTurn::Idle),
        session: Some(SESSION.to_owned()),
    }
}

fn rows(summary: &Summary, width: usize) -> Vec<String> {
    summary
        .goodbye(Layout::new(width, GlyphSet::Unicode))
        .iter()
        .map(line_text)
        .collect()
}

fn inks(line: &StyledLine) -> String {
    let mut at = 0;
    let mut out = vec![];
    for span in &line.spans {
        let end = at + span.text.chars().count();
        if let Some(Ink::Token(token)) = span.look.fg {
            let bold = if span.look.bold { " bold" } else { "" };
            out.push(format!("{at}..{end} {token:?}{bold}"));
        }
        at = end;
    }
    out.join("; ")
}

fn kept(id: u32, command: &str, kept: Keeps, pid: u32) -> ProcessRow {
    ProcessRow {
        id,
        command: command.to_owned(),
        terminal: None,
        end: None,
        clock_ms: 0,
        caption: Default::default(),
        waiting: false,
        restored: kept == Keeps::Unchecked,
        kept,
        pid: Some(pid),
        log: Some(format!("/tmp/wisp-{SESSION}/000{id}-job.log")),
        agent: wisp_protocol::AgentPath::root(),
        agent_ended: false,
        opens: true,
        stoppable: true,
        attachable: false,
    }
}

fn process_rows() -> [ProcessRow; 4] {
    [
        kept(1, "npm run dev", Keeps::Running, 4321),
        ProcessRow {
            end: Some(wisp_protocol::ProcessEnd::Lost {
                reason: wisp_protocol::LostReason::Gone,
            }),
            ..kept(2, "ended", Keeps::Running, 4500)
        },
        kept(3, "python3 -m http.server 8000", Keeps::Unchecked, 4400),
        kept(4, "not kept", Keeps::No, 4600),
    ]
}

fn api_child(name: &str, spent: f64) -> AgentRow {
    child(name, spent, Billing::Metered, true)
}

#[test]
fn the_goodbye_totals_main_and_every_child_as_the_strip_does() {
    let rows_api = [api_child("review", 0.96), api_child("coder", 0.46)];
    let all_api = summary(spend(0.42, Billing::Metered), &rows_api);
    assert_eq!(rows(&all_api, 40)[1], "That was 2 h 14 min, total $1.84.");
    let unpriced = summary(
        spend(0.42, Billing::Metered),
        &[child("local", 0.0, Billing::Metered, false)],
    );
    assert_eq!(rows(&unpriced, 40)[1], "That was 2 h 14 min, total $0.42+.");
    let covered = summary(
        spend(1.00, Billing::AnthropicPlan),
        &[child("review", 2.10, Billing::AnthropicPlan, true)],
    );
    assert_eq!(rows(&covered, 40)[1], "That was 2 h 14 min, total ~$3.10.");
    let mut running = rows_api.clone();
    running[1].entry.state = AgentState::Running;
    let busy = summary(spend(0.42, Billing::Metered), &running);
    assert_eq!(rows(&busy, 40)[1], "That was 2 h 14 min, total $1.84+.");
    let nothing = summary(AccountSpend::default(), &[]);
    assert_eq!(rows(&nothing, 40)[1], "That was 2 h 14 min.");
}

#[test]
fn the_goodbye_shows_only_after_a_turn_or_a_stopped_agent() {
    let quiet = Summary {
        turns: 0,
        ..summary(AccountSpend::default(), &[])
    };
    assert!(quiet.goodbye(Layout::new(80, GlyphSet::Unicode)).is_empty());
    let stopped = Summary {
        stopped: 1,
        ..quiet.clone()
    };
    let shown = rows(&stopped, 80);
    assert_eq!(
        shown.last().map(String::as_str),
        Some("wisp --resume 01a18e69-13bd")
    );
    assert!(
        shown[2].ends_with("Continue your agent with:"),
        "{shown:#?}"
    );
}

#[test]
fn without_a_log_there_is_no_command() {
    let unlogged = Summary {
        session: None,
        ..summary(AccountSpend::default(), &[])
    };
    let shown = rows(&unlogged, 80);
    assert_eq!(shown.len(), 3, "{shown:#?}");
    assert!(!shown.iter().any(|r| r.contains("--resume")), "{shown:#?}");
    assert!(!shown[2].contains("    w"), "{shown:#?}");
    let two = Summary {
        stopped: 2,
        ..unlogged
    };
    assert!(rows(&two, 80)[2].ends_with("Your 2 agents aren't saved."));
}

#[test]
fn goodbye_snapshots_at_40_80_and_120() {
    let tree = [
        child("review", 30.0, Billing::AnthropicPlan, true),
        api_child("coder", 4.06),
    ];
    let processes = process_rows();
    let cases: [(&str, Summary, &[ProcessRow]); 4] = [
        (
            "one stopped agent",
            Summary {
                stopped: 1,
                ..summary(spend(10.0, Billing::AnthropicPlan), &tree)
            },
            &processes,
        ),
        (
            "two stopped agents",
            Summary {
                stopped: 2,
                ..summary(spend(10.0, Billing::AnthropicPlan), &tree)
            },
            &[],
        ),
        (
            "all API",
            Summary {
                duration_ms: 720_000,
                ..summary(spend(0.31, Billing::Metered), &[])
            },
            &[],
        ),
        (
            "no log",
            Summary {
                stopped: 1,
                session: None,
                ..summary(spend(0.31, Billing::Metered), &[])
            },
            &[],
        ),
    ];
    for (set, glyphs) in [("unicode", GlyphSet::Unicode), ("ascii", GlyphSet::Ascii)] {
        for width in [40, 80, 120] {
            let mut out = String::new();
            for (name, summary, processes) in &cases {
                out.push_str(&format!("== {name}\n"));
                let layout = Layout::new(width, glyphs);
                let mut lines = summary.goodbye(layout);
                add_kept(&mut lines, processes, layout);
                for line in lines {
                    out.push_str(&format!("|{}\n  ~ {}\n", line_text(&line), inks(&line)));
                }
            }
            insta::assert_snapshot!(format!("goodbye-dark-{set}-{width}"), out);
        }
    }
}

#[test]
fn leaving_uses_the_goodbye_resume_line_without_time_spend_or_stopped_agents() {
    let mut summary = summary(spend(10.0, Billing::Metered), &[]);
    summary.turns = 0;
    summary.stopped = 2;
    for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
        for width in [40, 80, 120] {
            let shown = summary
                .leave(Layout::new(width, glyphs))
                .iter()
                .map(line_text)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(shown.contains(copy::CHAT_STAYS_OPEN));
            assert!(shown.contains(&copy::goodbye_agents(0, true).unwrap()));
            assert!(shown.ends_with("wisp --resume 01a18e69-13bd"));
            assert!(!shown.contains("That was"));
            assert!(!shown.contains("stopped"));
            assert!(!shown.contains('$'));
            let lost = summary
                .away(Away::Lost, Layout::new(width, glyphs))
                .iter()
                .map(line_text)
                .collect::<Vec<_>>()
                .join(" ");
            assert!(lost.contains(copy::CHAT_LOST));
            assert!(lost.contains(&copy::goodbye_agents(summary.stopped, true).unwrap()));
            assert!(!lost.contains("stopped"));
            assert!(lost.ends_with("wisp --resume 01a18e69-13bd"));
        }
    }
}
