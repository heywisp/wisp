use wisp_protocol::log::LEAD_DATA;

use super::*;

fn on(model: &str) -> SessionConfig {
    let mut session = default_session();
    session.model = model.into();
    session
}

fn report(id: &str) -> Mail {
    Mail {
        kind: MailKind::Report,
        ..letter(id, CHILD, true)
    }
}

fn framed(mail: &Mail) -> String {
    let form = neon_form();
    format!("{}{}{}", form.mail_before, shown(mail), form.mail_after)
}

fn data_entries(records: &[LogRecord]) -> Vec<(u64, LeadEntry)> {
    records
        .iter()
        .filter_map(|r| match &r.event {
            LogEvent::TurnStarted { turn, lead, .. } => Some((*turn, lead.entries.clone())),
            _ => None,
        })
        .flat_map(|(turn, entries)| entries.into_iter().map(move |e| (turn, e)))
        .filter(|(_, e)| e.by == LEAD_DATA)
        .collect()
}

async fn woken(model: &str) -> (Harness, Mail) {
    let (mut h, mail) = mailed_with_on(on(model), "form-woken", vec![done("woke")], None);
    let child = report("m1");
    mail.send(child.clone()).unwrap();
    let events = h.drive(Vec::new(), |_| Vec::new(), turns(1), PATIENT).await;
    assert_eq!(origins(&events), [Some(TurnOrigin::Mail)]);
    (h, child)
}

async fn folded(model: &str) -> (Harness, Mail, Mail) {
    let child = report("m1");
    let brief = task("m2");
    let plan = vec![
        two_calls().mail(child.clone()).mail(brief.clone()),
        done("ok"),
    ];
    let (mut h, _mail) = mailed_with_on(on(model), "form-folded", plan, None);
    let events = h.run(vec![steer("go")], 1).await;
    assert_eq!(delivered(&events)[1], (vec![mid("m2"), mid("m1")], 2));
    (h, child, brief)
}

#[tokio::test(start_paused = true)]
async fn a_report_that_wakes_a_neon_agent_comes_framed_and_the_data_part_ends_the_turn() {
    let (h, child) = woken(NEON).await;
    let texts = last_texts(&h);
    let n = texts.len();
    assert_eq!(texts[n - 2], framed(&child), "{texts:#?}");
    assert!(is_data(&texts[n - 1]), "{texts:#?}");
    assert!(!texts.iter().any(|t| bare_lines(t)), "{texts:#?}");
    let data = data_entries(&records(&h.store));
    assert_eq!(data.len(), 1, "{data:#?}");
    assert!(data[0].1.tail);
    assert_eq!(data[0].1.text, texts[n - 1]);
    h.assert_rebuilds();
}

#[tokio::test(start_paused = true)]
async fn mail_folded_into_a_neon_turn_is_framed_but_a_task_stays_bare() {
    let (h, child, brief) = folded(NEON).await;
    let requests = h.requests();
    let opening = texts(&requests[0][0]);
    assert_eq!(opening[opening.len() - 2], "go", "{opening:#?}");
    assert!(is_data(opening.last().unwrap()), "{opening:#?}");
    let tail = texts(requests[1].last().unwrap());
    assert!(
        tail.ends_with(&[shown(&brief).as_str(), framed(&child).as_str()]),
        "{tail:#?}"
    );
    assert!(!tail.iter().any(|t| is_data(t)), "{tail:#?}");
    assert_each_request_extends_the_last(&requests);
    h.assert_rebuilds();
}

#[tokio::test(start_paused = true)]
async fn other_models_send_the_bare_lines_and_bare_mail_as_before() {
    let (woke, child) = woken(DEFAULT_MODEL).await;
    let texts = last_texts(&woke);
    let n = texts.len();
    assert!(texts[n - 3].starts_with("Sent: "), "{texts:#?}");
    assert!(texts[n - 2].starts_with("Context: "), "{texts:#?}");
    assert_eq!(texts[n - 1], shown(&child));
    let (fold, child, brief) = folded(DEFAULT_MODEL).await;
    let tail = texts_of_last(&fold);
    assert!(tail.ends_with(&[shown(&brief), shown(&child)]), "{tail:#?}");
    for h in [&woke, &fold] {
        for body in h.provider.texts() {
            assert!(!body.contains("SYSTEM_MESSAGE"), "{body}");
            assert!(!body.contains("ADDITIONAL_METADATA"), "{body}");
        }
        let records = records(&h.store);
        for record in &records {
            if matches!(record.event, LogEvent::TurnStarted { .. }) {
                let line = crate::session::jsonl_line(record).unwrap();
                assert!(!line.contains("\"tail\""), "{line}");
            }
        }
        assert!(data_entries(&records).is_empty());
    }
}

fn texts_of_last(h: &Harness) -> Vec<String> {
    let requests = h.requests();
    texts(requests[1].last().unwrap())
        .into_iter()
        .map(str::to_owned)
        .collect()
}
