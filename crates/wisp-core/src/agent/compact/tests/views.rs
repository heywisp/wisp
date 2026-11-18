use wisp_protocol::log::kept_join;
use wisp_protocol::{Effort, EffortMarker};

use super::*;
use crate::agent::Pending;

fn whole_request_view(agent: &Agent, pending: &Pending) -> Vec<Message> {
    let mut messages = agent.history.clone();
    if pending.user.is_some() && agent.at_head() {
        messages.push(Message {
            role: Role::User,
            content: Vec::new(),
        });
    }
    pending.place(&mut messages);
    match &agent.compaction.head {
        Some(head) => head.logged().view(&messages),
        None => messages,
    }
}

pub(in crate::agent::compact) fn matches_the_whole_view(
    agent: &Agent,
    pending: &Pending,
    view: &[Message],
) {
    assert_eq!(view, whole_request_view(agent, pending));
}

fn whole_view_messages(agent: &Agent) -> Vec<Message> {
    match &agent.compaction.head {
        Some(head) => head.logged().view(&agent.history.clone()),
        None => agent.history.clone(),
    }
}

fn text(text: String) -> ContentBlock {
    ContentBlock::Text { text }
}

fn histories(longest: usize) -> Vec<Vec<Message>> {
    let roles = [Role::User, Role::Assistant, Role::System];
    let mut all = vec![Vec::new()];
    let mut last = vec![Vec::<Role>::new()];
    for _ in 0..longest {
        let next: Vec<Vec<Role>> = last
            .iter()
            .flat_map(|shape| {
                roles.iter().map(|role| {
                    let mut longer = shape.clone();
                    longer.push(*role);
                    longer
                })
            })
            .collect();
        all.extend(next.iter().map(|shape| {
            shape
                .iter()
                .enumerate()
                .map(|(index, role)| Message {
                    role: *role,
                    content: vec![text(format!("m{index}a")), text(format!("m{index}b"))],
                })
                .collect()
        }));
        last = next;
    }
    all
}

fn heads(history: &[Message]) -> Vec<Head> {
    let lead = Message {
        role: Role::Assistant,
        content: vec![text("lead".into())],
    };
    let head = |from: usize, cut: usize, placement: Placement, additions: Vec<ContentBlock>| Head {
        epoch: 1,
        from,
        message: lead.clone(),
        summary: String::new(),
        native: placement == Placement::Leading,
        turns: (1, 1),
        cut,
        placement,
        join: match placement {
            Placement::Leading => kept_join(history, cut, from),
            Placement::Head => None,
        },
        additions,
    };
    let len = history.len();
    let mut all: Vec<Head> = (0..=len)
        .map(|from| head(from, from, Placement::Head, Vec::new()))
        .collect();
    for cut in 0..=len {
        for from in 0..=len {
            all.push(head(from, cut, Placement::Leading, Vec::new()));
            all.push(head(
                from,
                cut,
                Placement::Leading,
                vec![text("added".into())],
            ));
        }
    }
    all
}

fn pendings() -> Vec<Pending> {
    let user = || Some(vec![text("new".into()), text("words".into())]);
    let marker = Some(EffortMarker {
        level: Effort::Low,
        rendered: true,
    });
    vec![
        Pending::default(),
        Pending {
            user: user(),
            ..Pending::default()
        },
        Pending {
            user: user(),
            marker,
            ..Pending::default()
        },
        Pending {
            user: user(),
            tools: vec![text("tools".into())],
            after: vec![text("after".into())],
            ..Pending::default()
        },
        Pending {
            user: user(),
            marker,
            after: vec![text("after".into())],
            ..Pending::default()
        },
    ]
}

#[test]
fn the_request_view_matches_the_whole_history_view_for_every_head_and_pending() {
    let mut agent = Agent::new(documented(vec![]), default_session());
    let pendings = pendings();
    let mut checked = 0;
    for history in histories(5) {
        agent.history = history.clone();
        agent.compaction.head = None;
        for pending in &pendings {
            assert_eq!(
                agent.request_view(pending),
                whole_request_view(&agent, pending)
            );
        }
        assert_eq!(agent.view_messages(), whole_view_messages(&agent));
        for head in heads(&history) {
            agent.compaction.head = Some(head);
            for pending in &pendings {
                assert_eq!(
                    agent.request_view(pending),
                    whole_request_view(&agent, pending),
                    "{:?} {pending:?}",
                    agent.compaction.head
                );
                checked += 1;
            }
            assert_eq!(agent.view_messages(), whole_view_messages(&agent));
        }
    }
    assert!(checked > 10_000, "{checked}");
}

#[test]
fn a_request_view_shares_the_image_bytes_of_the_history() {
    let mut agent = Agent::new(documented(vec![]), default_session());
    let image = ContentBlock::image(wisp_protocol::ImageType::Png, "aGVsbG8=");
    agent.history = vec![
        Message::user_text("first"),
        Message {
            role: Role::Assistant,
            content: vec![text("seen".into())],
        },
        Message {
            role: Role::User,
            content: vec![text("look".into()), image],
        },
    ];
    agent.compaction.head = heads(&agent.history)
        .into_iter()
        .find(|head| head.leading() && head.cut == 1 && head.from == 2);
    let data = |block: &ContentBlock| match block {
        ContentBlock::Image {
            source: wisp_protocol::ImageSource::Base64 { data, .. },
        } => Some(Arc::clone(data)),
        _ => None,
    };
    let held = agent.history[2].content.iter().find_map(data);
    let sent = agent
        .request_view(&Pending::default())
        .iter()
        .flat_map(|m| m.content.iter().filter_map(data).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    let (Some(held), [sent]) = (held, sent.as_slice()) else {
        panic!("one image in the history and one in the view");
    };
    assert!(Arc::ptr_eq(&held, sent));
}
