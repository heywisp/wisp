#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a failed step fails the test"
)]

#[path = "../../../crates/wisp-tui/tests/support/scratch.rs"]
mod scratch;
mod store;

use std::cell::RefCell;
use std::path::PathBuf;

use jiff::Timestamp;
use serde_json::{Value, json};

use super::*;
use scratch::ScratchDir;

const ROOT_ID: &str = "01a1aaaa-0000-7000-8000-000000000001";
const OTHER_ID: &str = "01a1eeee-0000-7000-8000-000000000005";
const OWNER: &str = "/coord";
const CODER: &str = "/coord/coder";
const PEER: &str = "/coord/peer";
const AT: i64 = 1_793_703_900;
const EIGHT_HOURS: i32 = 8 * 3600;

struct Fake {
    me: Option<Me>,
    agents: Vec<Agent>,
    data: Result<String, String>,
    clock: RefCell<Vec<i64>>,
    scratch: std::rc::Rc<ScratchDir>,
}

impl Host for Fake {
    fn me(&self) -> Option<Me> {
        self.me.clone()
    }

    fn agents(&self) -> Vec<Agent> {
        self.agents.clone()
    }

    fn now(&self) -> Timestamp {
        let mut ticks = self.clock.borrow_mut();
        let at = if ticks.len() > 1 {
            ticks.remove(0)
        } else {
            ticks[0]
        };
        Timestamp::from_second(at).unwrap()
    }

    fn offset(&self, _at: Timestamp) -> i32 {
        EIGHT_HOURS
    }

    fn data_dir(&self) -> Result<String, String> {
        self.data.clone()
    }
}

fn agents(ended: &[&str]) -> Vec<Agent> {
    [OWNER, CODER, PEER, "/next"]
        .into_iter()
        .map(|path| Agent {
            path: path.to_owned(),
            session: session(path),
            ended: ended.contains(&path),
        })
        .collect()
}

fn session(path: &str) -> String {
    format!("session{}", path.replace('/', "-"))
}

fn me(path: &str, root: &str) -> Me {
    Me {
        path: path.to_owned(),
        session: if path == "/" {
            root.to_owned()
        } else {
            session(path)
        },
        root: root.to_owned(),
    }
}

impl Fake {
    fn new(name: &str) -> Fake {
        let scratch = ScratchDir::new("wisp-board-", name);
        Fake {
            me: Some(me(OWNER, ROOT_ID)),
            agents: agents(&[]),
            data: Ok(scratch.root.join("data").display().to_string()),
            clock: RefCell::new(vec![AT]),
            scratch: std::rc::Rc::new(scratch),
        }
    }

    fn as_agent(&self, path: &str) -> Fake {
        self.in_tree(path, ROOT_ID)
    }

    fn in_tree(&self, path: &str, root: &str) -> Fake {
        Fake {
            me: Some(me(path, root)),
            agents: self.agents.clone(),
            data: self.data.clone(),
            clock: RefCell::new(self.clock.borrow().clone()),
            scratch: self.scratch.clone(),
        }
    }

    fn ended(&self, paths: &[&str]) -> Fake {
        let mut host = self.in_tree(OWNER, ROOT_ID);
        host.me.clone_from(&self.me);
        host.agents = agents(paths);
        host
    }

    fn renamed(&self, from: &str, to: &str) -> Fake {
        let moved = |path: &str| match path.strip_prefix(from) {
            Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("{to}{rest}"),
            _ => path.to_owned(),
        };
        let mut host = self.in_tree(OWNER, ROOT_ID);
        host.me = self.me.clone().map(|me| Me {
            path: moved(&me.path),
            ..me
        });
        for agent in &mut host.agents {
            agent.path = moved(&agent.path);
        }
        host
    }

    fn folder(&self) -> PathBuf {
        self.scratch.root.join("data/boards").join(ROOT_ID)
    }

    fn call(&self, tool: &str, input: &Value) -> Result<Reply, String> {
        let call = prepare(tool, &input.to_string())?;
        Ok(run(self, &call.request))
    }

    fn write(&self, input: &Value) -> Reply {
        self.call(WRITE, input).unwrap()
    }

    fn read(&self, input: &Value) -> Reply {
        self.call(READ, input).unwrap()
    }

    fn board(&self, name: &str) -> Option<Board> {
        crate::store::newest(&self.folder(), name, name)
            .unwrap()
            .and_then(crate::store::Version::live)
    }
}

fn line(text: &str) -> Reply {
    Reply::Line(text.to_owned())
}

fn refused(text: &str) -> Reply {
    Reply::Refused(text.to_owned())
}

fn view(reply: Reply) -> String {
    match reply {
        Reply::View(text) => text,
        other => panic!("not a view: {other:?}"),
    }
}

fn phase2(host: &Fake) {
    let created = host.write(&json!({
        "board": "phase2",
        "columns": ["todo", "building", "review", "done"],
        "cards": [
            {"id": "14a", "title": "Settle the contract", "assignee": CODER,
             "note": "Placeholder note, line one\n- placeholder step one\n- placeholder step two"},
            {"id": "14", "title": "Build the host", "assignee": PEER},
            {"id": "15", "title": "Install plugins"}
        ]
    }));
    assert_eq!(
        created,
        line("board phase2: created, 3 cards added, columns set")
    );
}

#[test]
fn a_board_alone_creates_an_empty_board_with_the_default_columns_owned_by_its_writer() {
    let host = Fake::new("alone");
    assert_eq!(
        host.write(&json!({"board": "plain"})),
        line("board plain: created")
    );
    let board = host.board("plain").unwrap();
    assert_eq!(board.columns, ["todo", "doing", "done"]);
    assert_eq!((board.owner.as_str(), board.edit), (OWNER, Edit::Owner));
    assert!(board.cards.is_empty());
    assert_eq!(board.created, 1);
    assert_eq!(
        host.write(&json!({"board": "plain"})),
        line("board plain: no change")
    );
    assert_eq!(
        crate::store::versions(&host.folder(), "plain").unwrap(),
        [1]
    );
}

#[test]
fn a_creating_write_titles_each_card_and_needs_no_remove_delete_or_owner() {
    let host = Fake::new("creating");
    let untitled = "there is no board fresh, and a write that creates one titles each card";
    for input in [
        json!({"board": "fresh", "cards": [{"id": "a", "title": "A"}, {"id": "b"}]}),
        json!({"board": "fresh", "cards": [{"id": "b", "log": "started"}]}),
    ] {
        assert_eq!(host.write(&input), refused(untitled), "{input}");
    }
    for input in [
        json!({"board": "fresh", "remove": ["a"]}),
        json!({"board": "fresh", "delete": true}),
        json!({"board": "fresh", "owner": CODER}),
    ] {
        assert_eq!(
            host.write(&input),
            refused("there is no board fresh"),
            "{input}"
        );
    }
    assert!(!host.folder().exists() || std::fs::read_dir(host.folder()).unwrap().count() == 0);
    let created = host.write(&json!({
        "board": "fresh", "columns": ["a", "b"], "edit": "tree",
        "cards": [{"id": "x", "title": "X", "column": "b"}]
    }));
    assert_eq!(
        created,
        line("board fresh: created, 1 card added, columns set, edit tree")
    );
    let board = host.board("fresh").unwrap();
    assert_eq!(board.cards[0].column, "b");
    assert_eq!(board.edit, Edit::Tree);
}

#[test]
fn each_write_reads_as_its_parts_in_order_and_in_the_singular_for_one() {
    let host = Fake::new("lines");
    phase2(&host);
    let cases = [
        (
            json!({"board": "phase2", "cards": [{"id": "16", "title": "Add the board"}, {"id": "17"}]}),
            "board phase2: 2 cards added",
        ),
        (
            json!({"board": "phase2", "cards": [{"id": "16", "column": "building"}]}),
            "board phase2: 1 card updated",
        ),
        (
            json!({"board": "phase2", "cards": [{"id": "16", "log": "started"}]}),
            "board phase2: 1 log line",
        ),
        (
            json!({"board": "phase2", "cards": [{"id": "16", "note": "n", "log": "a"}, {"id": "15", "log": "b"}]}),
            "board phase2: 1 card updated, 2 log lines",
        ),
        (
            json!({"board": "phase2", "remove": ["17"], "columns": ["todo", "building", "review", "merging", "done"], "edit": "owner",
                   "cards": [{"id": "18", "title": "T", "log": "x"}, {"id": "15", "title": "U"}]}),
            "board phase2: 1 card added, 1 card updated, 1 card removed, 1 log line, columns set, edit owner",
        ),
        (
            json!({"board": "phase2", "remove": ["18", "16"], "edit": "tree"}),
            "board phase2: 2 cards removed, edit tree",
        ),
        (json!({"board": "phase2"}), "board phase2: no change"),
        (
            json!({"board": "phase2", "owner": "main"}),
            "board phase2 now belongs to main",
        ),
    ];
    for (input, expected) in cases {
        assert_eq!(host.write(&input), line(expected), "{input}");
    }
    let root = host.as_agent("/");
    assert_eq!(
        root.write(&json!({"board": "phase2", "delete": true})),
        line("board phase2 deleted")
    );
    assert_eq!(host.board("phase2"), None);
}

#[test]
fn cards_keep_their_order_and_a_new_id_goes_last_in_the_first_column_titled_with_its_id() {
    let host = Fake::new("cards");
    phase2(&host);
    host.write(
        &json!({"board": "phase2", "cards": [{"id": "zz"}, {"id": "14", "column": "review"}]}),
    );
    let board = host.board("phase2").unwrap();
    let ids: Vec<&str> = board.cards.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["14a", "14", "15", "zz"]);
    let zz = board.card("zz").unwrap();
    assert_eq!((zz.title.as_str(), zz.column.as_str()), ("zz", "todo"));
    host.write(&json!({"board": "phase2", "remove": ["14"]}));
    let ids: Vec<String> = host
        .board("phase2")
        .unwrap()
        .cards
        .into_iter()
        .map(|c| c.id)
        .collect();
    assert_eq!(ids, ["14a", "15", "zz"]);
    assert_eq!(
        host.write(&json!({"board": "phase2", "remove": ["14z"]})),
        refused("board phase2 has no card 14z")
    );
}

#[test]
fn columns_are_checked_on_the_board_the_whole_write_leaves() {
    let host = Fake::new("columns");
    phase2(&host);
    assert_eq!(
        host.write(&json!({"board": "phase2", "columns": ["building", "review", "done"]})),
        refused("column todo holds 3 cards; move or remove its cards first")
    );
    host.write(&json!({"board": "phase2", "cards": [{"id": "14a", "column": "building"}]}));
    let before = host.board("phase2").unwrap();
    assert_eq!(
        host.write(&json!({"board": "phase2", "columns": ["todo", "review", "done"]})),
        refused("column building holds 1 card; move or remove its cards first")
    );
    assert_eq!(
        host.write(&json!({"board": "phase2", "cards": [{"id": "14", "column": "testing"}]})),
        refused("board phase2 has no column testing; its columns are todo, building, review, done")
    );
    assert_eq!(host.board("phase2").unwrap(), before);
    assert_eq!(
        host.write(
            &json!({"board": "phase2", "columns": ["todo", "doing", "review", "done"],
                           "cards": [{"id": "14a", "column": "doing"}]})
        ),
        line("board phase2: 1 card updated, columns set")
    );
    host.write(&json!({"board": "phase2", "cards": [{"id": "15", "column": "review"}]}));
    assert_eq!(
        host.write(
            &json!({"board": "phase2", "columns": ["todo", "doing", "done"], "remove": ["15"]})
        ),
        line("board phase2: 1 card removed, columns set")
    );
    assert_eq!(
        host.board("phase2").unwrap().columns,
        ["todo", "doing", "done"]
    );
}

const BELONGS: &str =
    "board phase2 belongs to /coord; only it and each card's assignee may change it";
const YOURS: &str =
    "you may change card 14a's column, note and log; only /coord may change the rest";
const ONLY: &str =
    "only board phase2's owner, /coord, may change its edit setting, delete it or hand it over";

fn card(fields: Value) -> Value {
    json!({"board": "phase2", "cards": [fields]})
}

#[test]
fn who_may_change_what_follows_the_rights_table_and_a_refused_write_changes_nothing() {
    let changes = [
        ("own column", card(json!({"id": "14a", "column": "review"}))),
        ("own note", card(json!({"id": "14a", "note": "new"}))),
        ("own log", card(json!({"id": "14a", "log": "checks pass"}))),
        ("own title", card(json!({"id": "14a", "title": "Retitled"}))),
        ("own assignee", card(json!({"id": "14a", "assignee": ""}))),
        (
            "other's column",
            card(json!({"id": "14", "column": "review"})),
        ),
        ("unassigned log", card(json!({"id": "15", "log": "x"}))),
        ("add", card(json!({"id": "new", "title": "New"}))),
        ("remove", json!({"board": "phase2", "remove": ["15"]})),
        (
            "columns",
            json!({"board": "phase2", "columns": ["todo", "building", "review", "done", "extra"]}),
        ),
        (
            "mixed",
            json!({"board": "phase2", "cards": [{"id": "14a", "column": "review"}, {"id": "14a2", "title": "X"}]}),
        ),
        (
            "own and title",
            card(json!({"id": "14a", "column": "review", "title": "Retitled"})),
        ),
        ("edit", json!({"board": "phase2", "edit": "owner"})),
        ("delete", json!({"board": "phase2", "delete": true})),
        ("owner", json!({"board": "phase2", "owner": PEER})),
    ];
    let as_coder = |what: &str| match what {
        "own column" | "own note" | "own log" => None,
        "own title" | "own assignee" | "mixed" | "own and title" => Some(YOURS),
        "edit" | "delete" | "owner" => Some(ONLY),
        _ => Some(BELONGS),
    };
    let in_tree = |what: &str| match what {
        "edit" | "delete" | "owner" => Some(ONLY),
        _ => None,
    };
    for edit in ["owner", "tree"] {
        for (actor, expect) in [
            (
                OWNER,
                &(|_: &str| None) as &dyn Fn(&str) -> Option<&'static str>,
            ),
            (CODER, &as_coder),
            (PEER, &|what: &str| match what {
                "edit" | "delete" | "owner" => Some(ONLY),
                "other's column" => None,
                _ => Some(BELONGS),
            }),
        ] {
            for (what, input) in &changes {
                let host = Fake::new("rights");
                phase2(&host);
                host.write(&json!({"board": "phase2", "edit": edit}));
                let before = host.board("phase2").unwrap();
                let expected = if edit == "tree" && actor != OWNER {
                    in_tree(what)
                } else {
                    expect(what)
                };
                let reply = host.as_agent(actor).write(input);
                let case = format!("{edit} {actor} {what}");
                match expected {
                    Some(sentence) => {
                        assert_eq!(reply, refused(sentence), "{case}");
                        assert_eq!(host.board("phase2").unwrap(), before, "{case}");
                    }
                    None => assert!(matches!(reply, Reply::Line(_)), "{case}: {reply:?}"),
                }
            }
        }
    }
}

#[test]
fn a_board_is_handed_over_by_its_owner_or_once_it_has_ended_by_the_nearest_running_agent_above() {
    let fresh = |ended: &[&str]| {
        let host = Fake::new("handover");
        host.as_agent(CODER).write(&json!({"board": "phase2"}));
        host.ended(ended)
    };
    let hand = |host: &Fake, by: &str, to: &str| {
        host.as_agent(by)
            .write(&json!({"board": "phase2", "owner": to}))
    };
    let only = "only board phase2's owner, /coord/coder, may change its edit setting, delete it or hand it over";
    let host = fresh(&[]);
    assert_eq!(hand(&host, OWNER, PEER), refused(only));
    assert_eq!(
        hand(&host, CODER, "/next"),
        line("board phase2 now belongs to /next")
    );
    let host = fresh(&[CODER]);
    assert_eq!(
        hand(&host, OWNER, PEER),
        line("board phase2 now belongs to /coord/peer")
    );
    let host = fresh(&[CODER, OWNER]);
    assert_eq!(
        hand(&host, PEER, PEER),
        refused(&format!(
            "{only}; /coord/coder has ended, and main can hand the board over"
        ))
    );
    assert_eq!(
        hand(&host, "/", CODER),
        refused("/coord/coder has ended; hand board phase2 to a running agent")
    );
    assert_eq!(
        hand(&host, "/", "/plugins-phase2-9"),
        refused("/plugins-phase2-9 isn't an agent of this tree")
    );
    assert_eq!(
        hand(&host, "/", "/"),
        line("board phase2 now belongs to main")
    );
    assert_eq!(host.board("phase2").unwrap().owner, "/");
    assert_eq!(
        hand(&host, "/", "main"),
        line("board phase2 now belongs to main")
    );
}

#[test]
fn an_ended_owner_names_who_can_hand_the_board_over_in_each_refusal() {
    let host = Fake::new("ended");
    phase2(&host);
    let host = host.ended(&[OWNER]);
    let suffix = "; /coord has ended, and main can hand the board over";
    let coder = host.as_agent(CODER);
    for (input, sentence) in [
        (card(json!({"id": "14", "column": "review"})), BELONGS),
        (card(json!({"id": "14a", "title": "T"})), YOURS),
        (json!({"board": "phase2", "delete": true}), ONLY),
    ] {
        assert_eq!(coder.write(&input), refused(&format!("{sentence}{suffix}")));
    }
}

#[test]
fn a_renamed_owner_and_assignee_keep_their_board_and_show_their_new_paths() {
    let host = Fake::new("renamed");
    phase2(&host);
    let lead = host.renamed(OWNER, "/lead");
    let shown = view(lead.read(&json!({"board": "phase2"})));
    assert!(
        shown.starts_with("board phase2 · owner /lead · edit owner\n"),
        "{shown}"
    );
    assert!(shown.contains("  14a · Settle the contract · /lead/coder · "));
    let coder = host.as_agent(CODER).renamed(OWNER, "/lead");
    assert_eq!(
        coder.write(&card(json!({"id": "14a", "column": "review"}))),
        line("board phase2: 1 card updated")
    );
    assert_eq!(
        lead.write(&json!({"board": "phase2", "edit": "tree"})),
        line("board phase2: edit tree")
    );
    let board = host.board("phase2").unwrap();
    assert_eq!(board.owner, "/lead");
    assert_eq!(board.cards[0].assignee.as_deref(), Some("/lead/coder"));
    host.as_agent(CODER).write(&json!({"board": "mine"}));
    let mut gone = host.as_agent(PEER).renamed(OWNER, "/lead");
    for agent in &mut gone.agents {
        agent.ended |= agent.path == "/lead/coder";
    }
    assert_eq!(
        gone.write(&json!({"board": "mine", "owner": PEER})),
        refused(
            "only board mine's owner, /lead/coder, may change its edit setting, delete it or hand it over; /lead/coder has ended, and /lead can hand the board over"
        )
    );
    gone.me = lead.me.clone();
    assert_eq!(
        gone.write(&json!({"board": "mine", "owner": "/lead/peer"})),
        line("board mine now belongs to /lead/peer")
    );
    assert_eq!(
        host.board("mine").unwrap().owner_session,
        Some(session(PEER))
    );
}

#[test]
fn a_stored_path_keyed_board_loads_is_honoured_and_is_keyed_by_session_on_its_next_write() {
    let host = Fake::new("path-keyed");
    let stored = json!({
        "cards": [{"assignee": CODER, "column": "todo", "id": "14a", "title": "T"}],
        "columns": ["todo", "done"], "created": 1, "deleted": false, "edit": "owner",
        "owner": OWNER, "updated": "2026-11-03 19:05:00+08:00"
    });
    let file = host.folder().join("phase2.1.json");
    std::fs::create_dir_all(host.folder()).unwrap();
    std::fs::write(&file, stored.to_string()).unwrap();
    assert_eq!(
        view(host.read(&json!({"board": "phase2"}))),
        "board phase2 · owner /coord · edit owner\ntodo (1)\n  14a · T · /coord/coder\ndone (0)\n"
    );
    let moved = card(json!({"id": "14a", "column": "done"}));
    assert_eq!(host.as_agent(PEER).write(&moved), refused(BELONGS));
    assert_eq!(
        host.write(&json!({"board": "phase2"})),
        line("board phase2: no change")
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), stored.to_string());
    assert_eq!(
        host.as_agent(CODER).write(&moved),
        line("board phase2: 1 card updated")
    );
    let board = host.board("phase2").unwrap();
    assert_eq!(board.owner_session, Some(session(OWNER)));
    assert_eq!(board.cards[0].assignee_session, Some(session(CODER)));
}

#[test]
fn the_root_is_main_in_every_line_and_main_or_a_slash_names_it_and_is_stored_as_a_slash() {
    let host = Fake::new("root");
    let root = host.as_agent("/");
    root.write(&json!({"board": "phase2", "cards": [
        {"id": "a", "title": "A", "assignee": "main", "log": "by main"},
        {"id": "b", "title": "B", "assignee": "/"}
    ]}));
    let board = host.board("phase2").unwrap();
    assert_eq!(board.owner, "/");
    assert_eq!(board.cards[0].assignee.as_deref(), Some("/"));
    assert_eq!(board.cards[1].assignee.as_deref(), Some("/"));
    assert_eq!(board.cards[0].log[0].author, "/");
    let shown = view(host.read(&json!({"board": "phase2"})));
    assert_eq!(
        shown,
        "board phase2 · owner main · edit owner\ntodo (2)\n  a · A · main\n    11-03 19:05 · todo · main · by main\n  b · B · main\ndoing (0)\ndone (0)\n"
    );
    assert_eq!(
        host.as_agent(CODER)
            .write(&card(json!({"id": "a", "column": "done"}))),
        refused("board phase2 belongs to main; only it and each card's assignee may change it")
    );
    assert_eq!(
        root.write(&card(json!({"id": "a", "assignee": "/nobody"}))),
        refused("/nobody isn't an agent of this tree")
    );
}

#[test]
fn a_log_line_takes_the_cards_column_after_the_write_and_its_author_and_shows_its_time() {
    let host = Fake::new("log");
    phase2(&host);
    host.as_agent(CODER).write(&card(
        json!({"id": "14a", "column": "building", "log": "started"}),
    ));
    let board = host.board("phase2").unwrap();
    assert_eq!(
        &board.card("14a").unwrap().log[0],
        &LogLine {
            at: "2026-11-03 19:05:00+08:00".to_owned(),
            column: "building".to_owned(),
            author: CODER.to_owned(),
            text: "started".to_owned(),
        }
    );
    let shown = view(host.read(&json!({"board": "phase2", "card": "14a"})));
    assert!(
        shown.ends_with("log:\n  11-03 19:05 · building · /coord/coder · started\n"),
        "{shown}"
    );
}

#[test]
fn the_board_and_card_views_are_pinned_and_mark_ended_agents_only_in_this_tree() {
    let host = Fake::new("views");
    phase2(&host);
    host.as_agent(CODER).write(&card(
        json!({"id": "14a", "column": "building", "log": "started"}),
    ));
    host.as_agent(CODER)
        .write(&card(json!({"id": "14a", "log": "checks pass"})));
    let long = format!("\n  {}\n", "w".repeat(100));
    host.write(&card(json!({"id": "15", "note": long})));
    assert_eq!(
        view(host.read(&json!({"board": "phase2"}))),
        format!(
            "board phase2 · owner /coord · edit owner\ntodo (2)\n  14 · Build the host · /coord/peer\n  15 · Install plugins · {}…\nbuilding (1)\n  14a · Settle the contract · /coord/coder · Placeholder note, line one (+2 lines)\n    11-03 19:05 · building · /coord/coder · checks pass\nreview (0)\ndone (0)\n",
            "w".repeat(79)
        )
    );
    let card_view = "title: Settle the contract\ncolumn: building\nassignee: /coord/coder (ended)\nnote:\n  Placeholder note, line one\n  - placeholder step one\n  - placeholder step two\nlog:\n  11-03 19:05 · building · /coord/coder · started\n  11-03 19:05 · building · /coord/coder · checks pass\n";
    let ended = host.ended(&[CODER, OWNER]);
    assert_eq!(
        view(ended.read(&json!({"board": "phase2", "card": "14a"}))),
        format!("board phase2 · card 14a\n{card_view}")
    );
    let shown = view(ended.read(&json!({"board": "phase2"})));
    assert!(
        shown.starts_with("board phase2 · owner /coord (ended) · edit owner\n"),
        "{shown}"
    );
    assert!(shown.contains("  14a · Settle the contract · /coord/coder (ended) · "));
    assert_eq!(
        view(ended.read(&json!({}))),
        "boards of tree 01a1aaaa-0000-7000-8000-000000000001\nphase2 · owner /coord (ended) · edit owner · 3 cards · updated 11-03 19:05\n"
    );
    let other = ended.in_tree("/", OTHER_ID);
    let qualified = format!("{ROOT_ID}/phase2");
    assert_eq!(
        view(other.read(&json!({"board": qualified, "card": "14a"}))),
        format!(
            "board phase2 of tree {ROOT_ID} · card 14a\n{}",
            card_view.replace(" (ended)", "")
        )
    );
    let shown = view(other.read(&json!({"board": qualified})));
    assert!(
        shown.starts_with(&format!(
            "board phase2 of tree {ROOT_ID} · owner /coord · edit owner\n"
        )),
        "{shown}"
    );
    assert!(!shown.contains("(ended)"));
    assert_eq!(
        view(other.read(&json!({}))),
        format!("no boards in tree {OTHER_ID} yet\n")
    );
    assert_eq!(
        other.read(&json!({"board": "phase2"})),
        refused("there is no board phase2")
    );
    assert_eq!(
        host.read(&json!({"board": "phase2", "card": "14z"})),
        refused("board phase2 has no card 14z")
    );
    let unassigned = view(host.read(&json!({"board": "phase2", "card": "14"})));
    assert_eq!(
        unassigned,
        "board phase2 · card 14\ntitle: Build the host\ncolumn: todo\nassignee: /coord/peer\n"
    );
}

#[test]
fn input_outside_the_grammar_and_limits_is_refused_before_it_runs() {
    let id = "a".repeat(33);
    let cases = [
        (
            READ,
            json!({"board": "Phase2"}),
            "a board's name is 1 to 40 of a-z, 0-9 and -, starting with a letter or digit",
        ),
        (
            READ,
            json!({"board": "-x"}),
            "a board's name is 1 to 40 of a-z, 0-9 and -, starting with a letter or digit",
        ),
        (
            READ,
            json!({"board": "x".repeat(41)}),
            "a board's name is 1 to 40 of a-z, 0-9 and -, starting with a letter or digit",
        ),
        (
            READ,
            json!({"board": "01a1/phase2"}),
            "a tree's id is a full session id, 36 of 0-9, a-f and -",
        ),
        (
            READ,
            json!({"board": format!("{ROOT_ID}/Phase2")}),
            "a board's name is 1 to 40 of a-z, 0-9 and -, starting with a letter or digit",
        ),
        (
            READ,
            json!({"card": "14a"}),
            "a card is read on its board, so name the board too",
        ),
        (
            READ,
            json!({"board": "phase2", "card": id}),
            "a card's id is 1 to 32 of A-Z, a-z, 0-9, ., _ and -",
        ),
        (
            WRITE,
            json!({"board": format!("{ROOT_ID}/phase2"), "cards": [{"id": "14a", "log": "x"}]}),
            "board 01a1aaaa-0000-7000-8000-000000000001/phase2 belongs to another tree; you can read it with board_read",
        ),
        (
            WRITE,
            json!({"board": "phase2", "delete": true, "owner": "main"}),
            "delete and owner each go alone in a write",
        ),
        (
            WRITE,
            json!({"board": "phase2", "owner": "main", "edit": "tree"}),
            "delete and owner each go alone in a write",
        ),
        (
            WRITE,
            json!({"board": "phase2", "delete": true, "remove": ["a"]}),
            "delete and owner each go alone in a write",
        ),
        (
            WRITE,
            json!({"board": "phase2", "cards": [{"id": "14a"}], "remove": ["14a"]}),
            "card 14a is named twice in this write",
        ),
        (
            WRITE,
            json!({"board": "phase2", "cards": [{"id": "14a"}, {"id": "14a"}]}),
            "card 14a is named twice in this write",
        ),
        (
            WRITE,
            json!({"board": "phase2", "cards": [{"id": "14a", "title": "two\nlines"}]}),
            "card 14a's title is one line of at most 200 characters",
        ),
        (
            WRITE,
            json!({"board": "phase2", "cards": [{"id": "14a", "title": "t".repeat(201)}]}),
            "card 14a's title is one line of at most 200 characters",
        ),
        (
            WRITE,
            json!({"board": "phase2", "cards": [{"id": "14a", "log": "a\rb"}]}),
            "card 14a's log line is one line of at most 500 characters",
        ),
        (
            WRITE,
            json!({"board": "phase2", "cards": [{"id": "14a", "log": "l".repeat(501)}]}),
            "card 14a's log line is one line of at most 500 characters",
        ),
        (
            WRITE,
            json!({"board": "phase2", "cards": [{"id": "14a", "note": "n".repeat(2_001)}]}),
            "card 14a's note is at most 2,000 characters",
        ),
        (
            WRITE,
            json!({"board": "phase2", "cards": [{"id": "14a", "column": "Review"}]}),
            "card 14a's column is 1 to 24 of a-z, 0-9 and -",
        ),
        (
            WRITE,
            json!({"board": "phase2", "cards": [{"id": "a b"}]}),
            "a card's id is 1 to 32 of A-Z, a-z, 0-9, ., _ and -",
        ),
        (
            WRITE,
            json!({"board": "phase2", "columns": ["c".repeat(25)]}),
            "a column's name is 1 to 24 of a-z, 0-9 and -",
        ),
        (
            WRITE,
            json!({"board": "phase2", "columns": []}),
            "a board has 1 to 12 columns",
        ),
        (
            WRITE,
            json!({"board": "phase2", "columns": (0..13).map(|n| format!("c{n}")).collect::<Vec<_>>()}),
            "a board has 1 to 12 columns",
        ),
        (
            WRITE,
            json!({"board": "phase2", "columns": ["a", "a"]}),
            "column a is named twice in this write",
        ),
    ];
    for (tool, input, sentence) in cases {
        assert_eq!(
            prepare(tool, &input.to_string()).err().as_deref(),
            Some(sentence),
            "{input}"
        );
    }
    for (tool, input) in [
        (WRITE, json!({"board": "phase2", "colour": "red"})),
        (WRITE, json!({"board": "phase2", "edit": "everyone"})),
        (READ, json!({"board": 7})),
        (WRITE, json!({})),
    ] {
        assert!(prepare(tool, &input.to_string()).is_err(), "{input}");
    }
    let fits = json!({"board": "x".repeat(40), "columns": (0..12).map(|n| format!("c{n}")).collect::<Vec<_>>(),
        "cards": [{"id": "a".repeat(32), "title": "t".repeat(200), "log": "l".repeat(500), "note": "n\n".repeat(1_000), "column": "c".repeat(24)}]});
    assert!(prepare(WRITE, &fits.to_string()).is_ok());
}

fn shown_of(tool: &str, input: &Value) -> Option<Shown> {
    rejected(tool, &input.to_string())
}

fn target(write: bool, verbs: Option<[&'static str; 3]>, target: &str) -> Option<Shown> {
    Some(Shown {
        write,
        verbs,
        target: target.to_owned(),
    })
}

#[test]
fn each_call_shows_what_it_did_and_a_refused_input_keeps_its_row() {
    const DELETE: Option<[&str; 3]> = Some(["Deleting", "Deleted", "couldn't delete"]);
    const HAND: Option<[&str; 3]> = Some(["Handing", "Handed", "couldn't hand over"]);
    let qualified = format!("{ROOT_ID}/phase2");
    let long = "x".repeat(50);
    let cases = [
        (READ, json!({}), target(false, None, "boards")),
        (READ, json!({"card": "14a"}), target(false, None, "boards")),
        (
            READ,
            json!({"board": "phase2"}),
            target(false, None, "board phase2"),
        ),
        (
            READ,
            json!({"board": "phase2", "card": "14a"}),
            target(false, None, "card 14a of board phase2"),
        ),
        (
            READ,
            json!({"board": qualified}),
            target(false, None, &format!("board phase2 of tree {ROOT_ID}")),
        ),
        (
            READ,
            json!({"board": qualified, "card": "14a"}),
            target(
                false,
                None,
                &format!("card 14a of board phase2 of tree {ROOT_ID}"),
            ),
        ),
        (READ, json!({"board": 7}), None),
        (
            WRITE,
            json!({"board": "phase2", "columns": ["a"]}),
            target(true, None, "board phase2"),
        ),
        (
            WRITE,
            json!({"board": "phase2", "cards": [{"id": "14a", "log": "x"}]}),
            target(true, None, "card 14a of board phase2"),
        ),
        (
            WRITE,
            json!({"board": "phase2", "cards": [{"id": "14a"}], "remove": ["14a"]}),
            target(true, None, "board phase2"),
        ),
        (
            WRITE,
            json!({"board": "phase2", "cards": [{"id": "a"}, {"id": "b"}]}),
            target(true, None, "board phase2"),
        ),
        (
            WRITE,
            json!({"board": "phase2", "delete": true}),
            target(true, DELETE, "board phase2"),
        ),
        (
            WRITE,
            json!({"board": "phase2", "owner": "/"}),
            target(true, HAND, "board phase2 to main"),
        ),
        (
            WRITE,
            json!({"board": "phase2", "owner": PEER}),
            target(true, HAND, "board phase2 to /coord/peer"),
        ),
        (
            WRITE,
            json!({"board": "phase2", "delete": true, "owner": "main"}),
            target(true, None, "board phase2"),
        ),
        (
            WRITE,
            json!({"board": "phase2", "delete": false}),
            target(true, None, "board phase2"),
        ),
        (
            WRITE,
            json!({"board": qualified, "cards": [{"id": "14a", "log": "x"}]}),
            target(
                true,
                None,
                &format!("card 14a of board phase2 of tree {ROOT_ID}"),
            ),
        ),
        (
            WRITE,
            json!({"board": "phase2", "cards": [{"id": "14a", "title": "a\nb"}]}),
            target(true, None, "card 14a of board phase2"),
        ),
        (
            WRITE,
            json!({"board": long, "cards": [{"id": long}]}),
            target(
                true,
                None,
                &format!("card {}… of board {}…", "x".repeat(39), "x".repeat(39)),
            ),
        ),
        (WRITE, json!({"cards": [{"id": "14a"}]}), None),
        (WRITE, json!({"board": ["phase2"]}), None),
    ];
    for (tool, input, expected) in cases {
        assert_eq!(shown_of(tool, &input), expected, "{tool} {input}");
        if let Ok(call) = prepare(tool, &input.to_string()) {
            assert_eq!(Some(call.shown), expected, "{tool} {input}");
        }
    }
}

#[test]
fn every_sentence_has_its_outcome_a_tool_error_for_a_rule_and_failed_for_the_store_or_session() {
    let host = Fake::new("outcomes");
    let none = Fake {
        me: None,
        ..host.as_agent(OWNER)
    };
    assert_eq!(
        none.read(&json!({})),
        Reply::Failed("the board needs this session's agent tree".to_owned())
    );
    let homeless = Fake {
        data: Err("this session has no data folder".to_owned()),
        ..host.as_agent(OWNER)
    };
    assert_eq!(
        homeless.write(&json!({"board": "phase2"})),
        Reply::Failed("this session has no data folder".to_owned())
    );
    std::fs::create_dir_all(host.folder().parent().unwrap()).unwrap();
    std::fs::write(host.folder(), "not a folder").unwrap();
    assert_eq!(
        host.read(&json!({})),
        Reply::Failed("the boards of this tree couldn't be read: not a directory".to_owned())
    );
    assert_eq!(
        host.read(&json!({"board": "phase2"})),
        Reply::Failed("board phase2 couldn't be read: not a directory".to_owned())
    );
    std::fs::remove_file(host.folder()).unwrap();
    std::fs::create_dir_all(
        host.folder()
            .join(format!("phase2.{}.tmp", me(OWNER, ROOT_ID).session)),
    )
    .unwrap();
    assert_eq!(
        host.write(&json!({"board": "phase2"})),
        Reply::Failed("board phase2 couldn't be written: entity already exists".to_owned())
    );
    assert_eq!(
        host.read(&json!({"board": "phase2"})),
        refused("there is no board phase2")
    );
}

#[test]
fn the_board_file_is_canonical_json_with_sorted_keys_and_a_two_space_indent() {
    let host = Fake::new("canonical");
    host.write(&json!({"board": "b", "columns": ["x"], "cards": [{"id": "1", "title": "T", "assignee": PEER, "log": "l"}]}));
    let bytes = std::fs::read_to_string(host.folder().join("b.1.json")).unwrap();
    let expected = "{\n  \"cards\": [\n    {\n      \"assignee\": \"/coord/peer\",\n      \"assignee_session\": \"session-coord-peer\",\n      \"column\": \"x\",\n      \"id\": \"1\",\n      \"log\": [\n        {\n          \"at\": \"2026-11-03 19:05:00+08:00\",\n          \"author\": \"/coord\",\n          \"column\": \"x\",\n          \"text\": \"l\"\n        }\n      ],\n      \"title\": \"T\"\n    }\n  ],\n  \"columns\": [\n    \"x\"\n  ],\n  \"created\": 1,\n  \"deleted\": false,\n  \"edit\": \"owner\",\n  \"owner\": \"/coord\",\n  \"owner_session\": \"session-coord\",\n  \"updated\": \"2026-11-03 19:05:00+08:00\"\n}\n";
    assert_eq!(bytes, expected);
    assert_eq!(host.board("b").unwrap().bytes(), expected.as_bytes());
}

#[test]
fn both_tools_have_fixed_definitions_and_a_ten_second_limit() {
    assert_eq!(LIMIT_MS, 10_000);
    let defs = definitions();
    let names: Vec<&str> = defs.iter().map(|d| d.name).collect();
    assert_eq!(names, [READ, WRITE]);
    for def in &defs {
        let schema: Value = serde_json::from_str(&def.schema).unwrap();
        assert_eq!(schema["additionalProperties"], false, "{}", def.name);
        for text in [def.description, &def.schema] {
            assert!(
                !text.contains("/coord") && !text.contains(ROOT_ID),
                "{text}"
            );
        }
    }
    assert!(
        defs[1].description.contains("creates it") && defs[1].description.contains("titles each")
    );
}
