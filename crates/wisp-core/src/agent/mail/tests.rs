use super::*;

fn mail(n: usize, from: &str, kind: MailKind) -> Mail {
    Mail {
        id: MailId(format!("m{n}")),
        from: AgentPath::new(from),
        to: AgentPath::new("/review"),
        kind,
        wake: true,
        text: format!("note {n}"),
    }
}

#[test]
fn one_sender_may_leave_at_most_64_messages_waiting_and_kernel_mail_is_never_refused() {
    let mailbox = Mailbox::default();
    let sender = mailbox.sender();
    for n in 0..MAX_MAIL_PER_SENDER {
        sender.send(mail(n, "/docs", MailKind::Message)).unwrap();
    }
    assert_eq!(
        sender.send(mail(99, "/docs", MailKind::Message)),
        Err(MailRefused::TooMany)
    );
    assert_eq!(
        sender.send(mail(100, "/docs", MailKind::Task)),
        Err(MailRefused::TooMany)
    );
    sender.send(mail(101, "/docs", MailKind::Report)).unwrap();
    sender.send(mail(102, "/docs", MailKind::Notice)).unwrap();
    sender.send(mail(103, "/plans", MailKind::Message)).unwrap();
    let docs = AgentPath::new("/docs");
    assert_eq!(sender.waiting_from(&docs), 66);
    assert_eq!(sender.unread_report(|p| p == &docs), Some(docs.clone()));
    mailbox.committed(&docs, MailKind::Notice);
    assert_eq!(sender.unread_report(|p| p == &docs), Some(docs.clone()));
    mailbox.committed(&docs, MailKind::Report);
    assert_eq!(sender.unread_report(|p| p == &docs), None);
    mailbox.committed(&docs, MailKind::Message);
    sender.send(mail(104, "/docs", MailKind::Message)).unwrap();
    assert_eq!(mailbox.take().len(), MAX_MAIL_PER_SENDER + 4);
}

#[test]
fn a_closed_mailbox_refuses_and_hands_back_what_arrived() {
    let mailbox = Mailbox::default();
    let sender = mailbox.sender();
    sender.send(mail(1, "/docs", MailKind::Message)).unwrap();
    let left = mailbox.close();
    assert_eq!(left, [mail(1, "/docs", MailKind::Message)]);
    assert_eq!(
        sender.send(mail(2, "/docs", MailKind::Report)),
        Err(MailRefused::Closed)
    );
}

#[tokio::test]
async fn a_send_before_anyone_waits_is_not_lost() {
    let mailbox = Mailbox::default();
    mailbox
        .sender()
        .send(mail(1, "/docs", MailKind::Message))
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), mailbox.arrived())
        .await
        .unwrap();
    assert_eq!(mailbox.take().len(), 1);
}

#[test]
fn mail_renders_as_one_fixed_block() {
    assert_eq!(
        render_mail(
            &AgentPath::new("/review/planner"),
            MailKind::Report,
            "/review/planner reported · 4s · 1 step this turn\nPlanned."
        ),
        "<wisp:message from=\"/review/planner\" kind=\"report\">\n/review/planner reported · 4s · 1 step this turn\nPlanned.\n</wisp:message>"
    );
    assert_eq!(mail_input_id(&MailId("m7".into())).as_str(), "mail-m7");
}
