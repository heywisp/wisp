use wisp_protocol::Placement;
use wisp_protocol::log::Cut;

use super::*;
use crate::agent::search::WHOLE;

fn turn(n: usize) -> [Message; 2] {
    [
        Message::user_text(format!("u{n}")),
        Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Thinking {
                    thinking: format!("Thought {n}."),
                    signature: format!("sig-{n}"),
                },
                ContentBlock::Text {
                    text: format!("Done {n}."),
                },
            ],
        },
    ]
}

fn unbound(turns: usize) -> Agent {
    let mut session = default_session();
    session.model = "claude-unlisted-test".into();
    let mut agent = Agent::new(scripted(vec![]), session);
    agent.history = (0..turns).flat_map(turn).collect();
    agent
}

fn head(agent: &mut Agent, placement: Placement, cut: usize, from: usize) {
    agent.compaction.head = Some(Head {
        epoch: 1,
        from,
        message: Message::user_text("head"),
        summary: String::new(),
        native: false,
        turns: (1, 1),
        cut,
        placement,
        additions: Vec::new(),
        join: None,
    });
}

fn carries(agent: &mut Agent, until: Option<usize>) -> bool {
    agent.search.kept_break = until;
    agent.carries_kept_break()
}

#[test]
fn without_a_head_the_cut_covers_the_whole_history_while_it_holds_thinking() {
    let mut agent = unbound(2);
    assert!(!carries(&mut agent, None));
    assert!(!carries(&mut agent, Some(0)));
    assert!(carries(&mut agent, Some(2)));
    assert_eq!(
        agent.whole_cut().map(|(cut, _)| cut),
        Some(Cut {
            from: WHOLE,
            through: 3,
            tools: false,
        })
    );
}

#[test]
fn a_break_past_the_history_after_a_rewind_cuts_only_while_thinking_remains() {
    let mut agent = unbound(1);
    assert!(carries(&mut agent, Some(40)));
    assert!(agent.whole_cut().is_some());
    agent.history.clear();
    assert!(!carries(&mut agent, Some(40)));
    agent.history.push(Message::user_text("plain"));
    assert!(!carries(&mut agent, Some(usize::MAX)));
    assert!(agent.whole_cut().is_none());
}

#[test]
fn a_plain_head_drops_the_broken_thinking_and_keeps_what_came_after() {
    let mut agent = unbound(4);
    head(&mut agent, Placement::Head, 4, 4);
    assert!(!carries(&mut agent, Some(2)));
    assert!(!carries(&mut agent, Some(4)));
    assert!(agent.whole_cut().is_none());
    assert!(carries(&mut agent, Some(6)));
    assert!(agent.whole_cut().is_some());
}

#[test]
fn a_leading_head_keeps_its_kept_thinking_after_an_earlier_break() {
    let mut agent = unbound(4);
    head(&mut agent, Placement::Leading, 2, 6);
    assert!(!carries(&mut agent, None));
    assert!(!carries(&mut agent, Some(1)));
    assert!(carries(&mut agent, Some(4)));
    assert!(agent.whole_cut().is_some());
    head(&mut agent, Placement::Leading, 2, 3);
    assert!(!carries(&mut agent, Some(1)));
    assert!(agent.whole_cut().is_none());
    assert!(carries(&mut agent, Some(4)));
}
