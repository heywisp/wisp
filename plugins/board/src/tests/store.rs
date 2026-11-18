use std::cell::Cell;
use std::fs::{self, File, FileTimes};
use std::path::Path;
use std::time::{Duration, SystemTime};

use serde_json::json;

use super::*;
use crate::store::{MAX_BYTES, versions};

fn numbers(host: &Fake, name: &str) -> Vec<u64> {
    versions(&host.folder(), name).unwrap()
}

fn write_during(host: &Fake, input: &Value, meanwhile: impl Fn(u32)) -> Reply {
    let call = prepare(WRITE, &input.to_string()).unwrap();
    let Request::Write(write) = call.request else {
        panic!("not a write")
    };
    let me = host.me.clone().unwrap();
    let agents = crate::members(host, &me);
    let tries = Cell::new(0);
    let done = crate::store::write(
        &host.folder(),
        &write.board,
        &me.session,
        &|| host.now(),
        |read, next| {
            tries.set(tries.get() + 1);
            meanwhile(tries.get());
            crate::apply::apply(
                read,
                &me,
                &agents,
                &write,
                "2026-11-03 19:05:00+08:00",
                next,
            )
        },
    );
    reply(done.map(Reply::Line))
}

fn log(id: &str, text: &str) -> Value {
    json!({"board": "phase2", "cards": [{"id": id, "log": text}]})
}

fn age(path: &Path, by: Duration) {
    let at = SystemTime::now() - by;
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(at).set_accessed(at))
        .unwrap();
}

#[test]
fn two_writes_from_one_version_both_land_the_later_by_reading_again() {
    let host = Fake::new("two-writers");
    phase2(&host);
    let coder = host.as_agent(CODER);
    let peer = host.as_agent(PEER);
    let landed = write_during(&coder, &log("14a", "mine"), |try_| {
        if try_ == 1 {
            assert_eq!(
                peer.write(&log("14", "theirs")),
                line("board phase2: 1 log line")
            );
        }
    });
    assert_eq!(landed, line("board phase2: 1 log line"));
    assert_eq!(numbers(&host, "phase2"), [1, 2, 3]);
    let board = host.board("phase2").unwrap();
    assert_eq!(board.card("14a").unwrap().log[0].text, "mine");
    assert_eq!(board.card("14").unwrap().log[0].text, "theirs");
}

#[test]
fn a_stale_writer_fails_to_link_and_lands_above_the_versions_linked_under_it() {
    let host = Fake::new("stale");
    phase2(&host);
    for n in 0..4 {
        host.write(&log("15", &format!("w{n}")));
    }
    assert_eq!(numbers(&host, "phase2"), [1, 2, 3, 4, 5]);
    let peer = host.as_agent(PEER);
    let landed = write_during(&host.as_agent(CODER), &log("14a", "late"), |try_| {
        if try_ == 1 {
            peer.write(&log("14", "six"));
            peer.write(&log("14", "seven"));
        }
    });
    assert_eq!(landed, line("board phase2: 1 log line"));
    assert_eq!(numbers(&host, "phase2"), [1, 2, 3, 4, 5, 6, 7, 8]);
    let board = host.board("phase2").unwrap();
    let texts: Vec<&str> = board
        .card("14")
        .unwrap()
        .log
        .iter()
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(texts, ["six", "seven"]);
    assert_eq!(board.card("14a").unwrap().log[0].text, "late");
}

#[test]
fn clean_up_deletes_only_versions_more_than_two_minutes_older_than_the_one_just_linked() {
    let host = Fake::new("clean");
    phase2(&host);
    host.write(&log("15", "two"));
    host.write(&log("15", "three"));
    age(
        &host.folder().join("phase2.1.json"),
        Duration::from_secs(121),
    );
    age(
        &host.folder().join("phase2.2.json"),
        Duration::from_secs(100),
    );
    host.write(&log("15", "four"));
    assert_eq!(numbers(&host, "phase2"), [2, 3, 4]);
}

#[test]
fn a_writer_lists_again_rather_than_link_more_than_thirty_seconds_after_its_listing() {
    let host = Fake::new("relist");
    phase2(&host);
    *host.clock.borrow_mut() = vec![AT, AT + 31, AT + 40];
    let tries = Cell::new(0);
    let landed = write_during(&host, &log("15", "slow"), |n| tries.set(n));
    assert_eq!(landed, line("board phase2: 1 log line"));
    assert_eq!(tries.get(), 2);
    assert_eq!(numbers(&host, "phase2"), [1, 2]);
    *host.clock.borrow_mut() = vec![AT, AT + 30, AT + 30];
    let tries = Cell::new(0);
    write_during(&host, &log("15", "in time"), |n| tries.set(n));
    assert_eq!(tries.get(), 1);
}

#[test]
fn a_deleted_board_is_created_again_at_the_next_number_and_stray_writes_are_refused() {
    let host = Fake::new("recreate");
    phase2(&host);
    assert_eq!(
        host.write(&json!({"board": "phase2", "delete": true})),
        line("board phase2 deleted")
    );
    let coder = host.as_agent(CODER);
    assert_eq!(
        coder.write(&log("14a", "after")),
        refused("there is no board phase2, and a write that creates one titles each card")
    );
    assert_eq!(
        coder.write(&json!({"board": "phse2", "cards": [{"id": "14a", "column": "review"}]})),
        refused("there is no board phse2, and a write that creates one titles each card")
    );
    assert_eq!(numbers(&host, "phase2"), [1, 2]);
    assert_eq!(numbers(&host, "phse2"), Vec::<u64>::new());
    assert_eq!(
        coder.write(&json!({"board": "phase2", "cards": [{"id": "a", "title": "A"}]})),
        line("board phase2: created, 1 card added")
    );
    assert_eq!(numbers(&host, "phase2"), [1, 2, 3]);
    let board = host.board("phase2").unwrap();
    assert_eq!((board.owner.as_str(), board.created), (CODER, 3));
}

#[test]
fn a_write_whose_board_was_deleted_or_created_again_meanwhile_writes_nothing() {
    for again in [false, true] {
        let host = Fake::new("meanwhile");
        phase2(&host);
        let coder = host.as_agent(CODER);
        let refused_write = write_during(&coder, &log("14a", "late"), |try_| {
            if try_ == 1 {
                host.write(&json!({"board": "phase2", "delete": true}));
                if again {
                    host.write(&json!({"board": "phase2", "cards": [{"id": "14a", "title": "New", "assignee": CODER}]}));
                }
            }
        });
        assert_eq!(
            refused_write,
            refused("board phase2 was deleted while this write ran; nothing was written")
        );
        let expected: &[u64] = if again { &[1, 2, 3] } else { &[1, 2] };
        assert_eq!(numbers(&host, "phase2"), expected);
    }
}

#[test]
fn two_creators_racing_on_one_name_leave_one_owner() {
    let host = Fake::new("race");
    let peer = host.as_agent(PEER);
    let lost = write_during(
        &host.as_agent(CODER),
        &json!({"board": "phase2", "cards": [{"id": "a", "title": "A"}]}),
        |try_| {
            if try_ == 1 {
                peer.write(&json!({"board": "phase2"}));
            }
        },
    );
    assert_eq!(
        lost,
        refused(
            "board phase2 belongs to /coord/peer; only it and each card's assignee may change it"
        )
    );
    assert_eq!(numbers(&host, "phase2"), [1]);
    assert_eq!(host.board("phase2").unwrap().owner, PEER);
}

#[test]
fn each_agent_writes_its_own_temporary_file_and_leftovers_never_stop_a_read() {
    let host = Fake::new("temps");
    phase2(&host);
    let folder = host.folder();
    let coder = host.as_agent(CODER);
    let mine = folder.join(format!("phase2.{}.tmp", coder.me.clone().unwrap().session));
    let theirs = folder.join(format!("phase2.{}.tmp", host.me.clone().unwrap().session));
    assert_ne!(mine, theirs);
    fs::write(&mine, "leftover").unwrap();
    fs::write(&theirs, "busy writing").unwrap();
    fs::write(folder.join("phase2.json"), "stray").unwrap();
    assert_eq!(
        coder.write(&log("14a", "x")),
        line("board phase2: 1 log line")
    );
    assert!(!mine.exists());
    assert_eq!(fs::read_to_string(&theirs).unwrap(), "busy writing");
    assert_eq!(numbers(&host, "phase2"), [1, 2]);
    assert!(matches!(
        host.read(&json!({"board": "phase2"})),
        Reply::View(_)
    ));
}

#[test]
fn a_version_that_vanishes_under_a_reader_is_listed_again() {
    let host = Fake::new("vanish");
    phase2(&host);
    let folder = host.folder();
    std::os::unix::fs::symlink(folder.join("gone"), folder.join("phase2.9.json")).unwrap();
    assert_eq!(
        host.read(&json!({"board": "phase2"})),
        Reply::Failed("board phase2 is busy; try again".to_owned())
    );
    fs::remove_file(folder.join("phase2.9.json")).unwrap();
    assert!(matches!(
        host.read(&json!({"board": "phase2"})),
        Reply::View(_)
    ));
}

#[test]
fn a_write_may_not_grow_a_board_past_256_kib_but_may_always_shrink_it() {
    let host = Fake::new("size");
    let note = "n".repeat(2_000);
    let cards: Vec<Value> = (0..130)
        .map(|n| json!({"id": format!("c{n}"), "title": "T", "note": note}))
        .collect();
    assert_eq!(
        host.write(&json!({"board": "big", "cards": cards})),
        refused("board big would pass 256 KiB; removing cards makes room")
    );
    host.write(&json!({"board": "big", "cards": cards[..120]}));
    let size = |host: &Fake| {
        fs::metadata(
            host.folder()
                .join(format!("big.{}.json", numbers(host, "big").last().unwrap())),
        )
        .unwrap()
        .len()
    };
    assert!(size(&host) < MAX_BYTES as u64);
    let more: Vec<Value> = (120..130)
        .map(|n| json!({"id": format!("c{n}"), "title": "T", "note": note}))
        .collect();
    assert_eq!(
        host.write(&json!({"board": "big", "cards": more})),
        refused("board big would pass 256 KiB; removing cards makes room")
    );
    let file = host.folder().join("big.1.json");
    let mut board: Board = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    board.cards.extend((120..135).map(|n| Card {
        id: format!("c{n}"),
        title: "T".to_owned(),
        column: "todo".to_owned(),
        assignee: None,
        assignee_session: None,
        note: Some(note.clone()),
        log: Vec::new(),
    }));
    fs::write(host.folder().join("big.2.json"), board.bytes()).unwrap();
    assert!(size(&host) > MAX_BYTES as u64);
    assert_eq!(
        host.write(&json!({"board": "big", "remove": ["c134"]})),
        line("board big: 1 card removed")
    );
}
