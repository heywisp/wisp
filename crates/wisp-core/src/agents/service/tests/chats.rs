use super::*;
use crate::resume::tests::session_here;
use crate::session::JsonlStore;
use crate::test_guard::shared::{lines, started_in};

const OLDER: &str = "0001aaaa-0000-7000-8000-000000000000";
const NEWER: &str = "0002bbbb-0000-7000-8000-000000000000";
const AWAY: &str = "0003cccc-0000-7000-8000-000000000000";

#[tokio::test(start_paused = true)]
async fn list_chats_answers_with_this_directorys_other_chats_newest_first() {
    let mut run = start(
        "chats",
        Setup {
            sessions: true,
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    let store = JsonlStore::open(run.env.dir("sessions")).unwrap();
    session_here(&store, OLDER, "~/work", None, 1, 30);
    session_here(&store, NEWER, "~/work", None, 2, 5);
    session_here(&store, AWAY, "~/elsewhere", None, 1, 1);
    session_here(&store, LISTED_ROOT, "~/work", None, 1, 1);
    let request = run.request();
    run.send(ClientCommand::ListChats { request }).await;
    let found = run
        .until(
            "the chats",
            |e| matches!(e, AgentEvent::ChatsListed { request: r, .. } if *r == request),
        )
        .await;
    let AgentEvent::ChatsListed { chats, .. } = found else {
        unreachable!()
    };
    let ids: Vec<&str> = chats.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, [NEWER, OLDER]);
    assert!(chats.iter().all(|c| !c.open_elsewhere));
    let again = run.request();
    run.send(ClientCommand::ListChats { request: again }).await;
    let repeated = run
        .until(
            "the chats again",
            |e| matches!(e, AgentEvent::ChatsListed { request: r, .. } if *r == again),
        )
        .await;
    assert!(matches!(repeated, AgentEvent::ChatsListed { chats: rows, .. } if rows == chats));
}

#[tokio::test(start_paused = true)]
async fn list_spend_answers_with_this_folders_days_accounts_and_window() {
    use crate::spend::tests::{append, cost};
    use wisp_protocol::Billing;

    let clock = Arc::new(ShiftedClock::at(
        "2026-10-28T06:00:00Z".parse().unwrap(),
        Some(jiff::tz::TimeZone::get("Pacific/Kiritimati").unwrap()),
    ));
    let today = crate::spend::day(Timestamp::now(), clock.as_ref());
    let yesterday = today.yesterday().unwrap();
    let mut run = start(
        "spend",
        Setup {
            sessions: true,
            clock: Some(clock),
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    let mut store = JsonlStore::open(run.env.dir("sessions")).unwrap();
    append(&mut store, today, "api", cost(1.0, Billing::Metered));
    append(
        &mut store,
        today,
        "subscription",
        cost(2.0, Billing::ChatGptPlan),
    );
    append(&mut store, yesterday, "api", cost(4.0, Billing::Metered));
    for expected in [7.0, 15.0] {
        let request = run.request();
        run.send(ClientCommand::ListSpend { request }).await;
        let found = run
            .until(
                "the spend",
                |e| matches!(e, AgentEvent::SpendListed { request: r, .. } if *r == request),
            )
            .await;
        let AgentEvent::SpendListed { days, window, .. } = found else {
            unreachable!()
        };
        assert_eq!(days.len(), 2);
        assert_eq!(days[0].day, today.to_string());
        assert_eq!(days[1].day, yesterday.to_string());
        assert_eq!(days[0].total.usd, expected - 4.0);
        assert_eq!(days[1].total.usd, 4.0);
        assert_eq!(days[0].accounts["api"].usd, expected - 6.0);
        assert_eq!(days[0].accounts["subscription"].plan_usd, Some(2.0));
        assert_eq!(window.since, Some(yesterday.to_string()));
        assert_eq!(window.total.usd, expected);
        assert_eq!(window.total.plan_usd, Some(2.0));
        assert_eq!(window.accounts["api"].usd, expected - 2.0);
        assert!(!window.total.lower_bound);
        if expected == 7.0 {
            append(&mut store, today, "api", cost(8.0, Billing::Metered));
        }
    }
}

#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn the_root_answers_other_commands_while_it_lists_chats() {
    let mut run = start(
        "chats-waiting",
        Setup {
            sessions: true,
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    let store = JsonlStore::open(run.env.dir("sessions")).unwrap();
    session_here(&store, OLDER, "~/work", None, 1, 30);
    let fifo = store.path(AWAY);
    rustix::fs::mkfifoat(
        rustix::fs::CWD,
        &fifo,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
    )
    .unwrap();
    let head = lines(&[started_in(AWAY, "~/elsewhere", &[], None)]);
    let (release, released) = std::sync::mpsc::channel::<()>();
    let writer = std::thread::spawn({
        let fifo = fifo.clone();
        move || {
            released
                .recv_timeout(std::time::Duration::from_secs(60))
                .ok();
            fs::write(&fifo, head).unwrap();
        }
    });
    let listing = run.request();
    run.send(ClientCommand::ListChats { request: listing })
        .await;
    let agents = run.request();
    run.send(ClientCommand::ListAgents { request: agents })
        .await;
    let first = run
        .until("an answer", |e| {
            matches!(e, AgentEvent::ChatsListed { request, .. } if *request == listing)
                || matches!(e, AgentEvent::AgentsSnapshot { request, .. } if *request == Some(agents))
        })
        .await;
    assert!(
        matches!(first, AgentEvent::AgentsSnapshot { .. }),
        "{first:?}"
    );
    release.send(()).unwrap();
    let found = run
        .until(
            "the chats",
            |e| matches!(e, AgentEvent::ChatsListed { request, .. } if *request == listing),
        )
        .await;
    let AgentEvent::ChatsListed { chats, .. } = found else {
        unreachable!()
    };
    let ids: Vec<&str> = chats.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, [OLDER]);
    writer.join().unwrap();
    fs::remove_file(&fifo).unwrap();
}
