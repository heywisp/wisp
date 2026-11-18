use wisp_protocol::{AttachmentKind, WithdrawnAttachment};

use super::*;
use crate::tool::{Toolbox, snap};

#[tokio::test]
async fn a_steered_image_refused_with_a_file_counts_in_the_files_note() {
    let Harness {
        agent,
        provider,
        tx,
        rx,
        store,
        _env,
    } = harness(vec![
        uses(&[call("a", "snap", json!({}))]).then(red("late", DeliveryMode::Steer, None)),
        attachment_refused(),
        done("ok"),
    ]);
    let agent = agent.with_tools(
        Toolbox::new([snap::tool(), super::super::attachments::with_bash()]),
        _env.workspace(),
    );
    let mut h = Harness {
        agent,
        provider,
        tx,
        rx,
        store,
        _env,
    };
    let events = h.run(vec![steer("go")], 1).await;
    check_events(&events);
    let items: Vec<WithdrawnAttachment> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::AttachmentsWithdrawn { items, .. } => Some(items.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    let seen: Vec<(Option<&str>, usize, AttachmentKind, &str, &str)> = items
        .iter()
        .map(|i| {
            (
                i.call_id.as_deref(),
                i.ordinal,
                i.kind,
                i.label.as_str(),
                i.note.as_str(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            (
                Some("a"),
                0,
                AttachmentKind::File,
                "snap.png",
                "[snap.png (image/png) was withdrawn with 1 other attachment when the API \
refused this step: Could not process image. It may be fine: view it again on its own, and \
convert it with bash only if that is refused too.]"
            ),
            (
                None,
                1,
                AttachmentKind::Image,
                "#1",
                "[image #1 couldn't be processed: Could not process image]"
            ),
        ]
    );
    let requests = h.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(image_count(&requests[1]), 1);
    assert_eq!(image_count(&requests[2]), 0);
    h.assert_rebuilds();
}
