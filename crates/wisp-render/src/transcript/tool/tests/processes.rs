use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};

use serde_json::Value;
use wisp_protocol::{BackgroundReason, LostReason, ProcessEnd, SignalKind, ToolView, Typed};
use wisp_tool_text::jobs::no_process;

use super::*;
use crate::events::{Requested, tool_requested, typed};
use crate::transcript::{TYPED_COLUMNS, echoed};

const PROGRAM: &str = "python3 manage.py shell";
const ABOUT: &str = "Start the Django shell";
const NO_ID: &str = "give `id` to act on a process; without it the call lists this session's processes and takes nothing but read and description";

struct Call<'a> {
    id: Option<u32>,
    text: Option<&'a str>,
    keys: &'a [&'a str],
    signal: Option<SignalKind>,
    read: Option<&'a str>,
    pattern: Option<&'a str>,
    waits: bool,
    attach: bool,
}

const BARE: Call<'static> = Call {
    id: Some(27),
    text: None,
    keys: &[],
    signal: None,
    read: Some("new"),
    pattern: None,
    waits: false,
    attach: false,
};

fn action_of(call: &Call<'_>) -> ToolAction {
    match (call.id, call.signal) {
        (None, _) => ToolAction::Other {
            name: "jobs".to_owned(),
        },
        _ if call.attach => ToolAction::Attach,
        (Some(_), Some(kind)) => ToolAction::Signal { kind },
        _ if call.text.is_some() || !call.keys.is_empty() => ToolAction::Input,
        _ if call.waits => ToolAction::Wait,
        _ if call.read == Some("search") => ToolAction::Search,
        _ => ToolAction::Read,
    }
}

fn process_call(block: usize, call_id: &str, call: &Call<'_>) -> AgentEvent {
    let mut input = serde_json::Map::new();
    if let Some(id) = call.id {
        input.insert("id".to_owned(), json!(id));
    }
    if let Some(text) = call.text {
        input.insert("input".to_owned(), json!(text));
    }
    tool_requested(Requested {
        block,
        call_id: call_id.to_owned(),
        tool: "jobs".to_owned(),
        input: Value::Object(input),
        action: action_of(call),
        target: call.id.map(|id| id.to_string()).unwrap_or_default(),
        view: Some(ToolView::Process {
            id: call.id,
            text: call.text.map(str::to_owned),
            keys: call.keys.iter().map(|k| (*k).to_owned()).collect(),
            signal: call.signal,
            read: call.read.map(str::to_owned),
            pattern: call.pattern.map(str::to_owned),
        }),
        ..Requested::default()
    })
}

fn described(mut event: AgentEvent, description: &str) -> AgentEvent {
    if let AgentEvent::ToolCallRequested { input, display, .. } = &mut event {
        input["description"] = json!(description);
        display.title = Some(description.to_owned());
    }
    event
}

fn done(call_id: &str, verdict: Option<Typed>) -> AgentEvent {
    done_as(call_id, verdict, Some(27))
}

fn done_as(call_id: &str, verdict: Option<Typed>, id: Option<u32>) -> AgentEvent {
    ended_as(
        call_id,
        ToolOutcome::Completed {
            is_error: false,
            exit: None,
        },
        verdict,
        id,
    )
}

fn ended_as(
    call_id: &str,
    outcome: ToolOutcome,
    verdict: Option<Typed>,
    id: Option<u32>,
) -> AgentEvent {
    let mut event = finished(call_id, outcome, view("", 0), 1000);
    if let AgentEvent::ToolCallFinished { typed, facts, .. } = &mut event {
        *typed = verdict;
        facts.extend(id.map(|id| Fact::Process { id }));
    }
    event
}

fn refused(call_id: &str, message: &str) -> AgentEvent {
    let mut event = finished(
        call_id,
        ToolOutcome::Completed {
            is_error: true,
            exit: None,
        },
        view("", 0),
        0,
    );
    if let AgentEvent::ToolCallFinished { result, .. } = &mut event {
        *result = Some(message.to_owned());
    }
    event
}

fn process_started(id: u32, call_id: &str, target: &str) -> AgentEvent {
    AgentEvent::ProcessStarted {
        id,
        call_id: call_id.to_owned(),
        target: target.to_owned(),
        tty: true,
        stdin: true,
        size: None,
        file: None,
        reason: BackgroundReason::Started,
        log: None,
        keeps_running: false,
        pid: None,
    }
}

fn process_restored(id: u32, call_id: &str, target: &str) -> AgentEvent {
    AgentEvent::ProcessRestored {
        id,
        call_id: Some(call_id.to_owned()),
        target: target.to_owned(),
        tty: true,
        end: Some(ProcessEnd::Lost {
            reason: LostReason::StoppedAtResume,
        }),
        duration_ms: 0,
        keeps_running: false,
        unchecked: false,
        pid: None,
        log: None,
    }
}

fn backgrounded(call_id: &str, id: u32) -> AgentEvent {
    let mut event = finished(
        call_id,
        ToolOutcome::Backgrounded {
            reason: BackgroundReason::Started,
        },
        view("", 0),
        1000,
    );
    if let AgentEvent::ToolCallFinished { facts, process, .. } = &mut event {
        facts.push(Fact::Process { id });
        *process = Some(id);
    }
    event
}

fn started_program(t: &mut Transcript, target: &str) {
    apply(
        t,
        &[
            AgentEvent::turn_started(1),
            requested(1, 0, "bg", target),
            started("bg"),
            process_started(27, "bg", target),
            backgrounded("bg", 27),
        ],
    );
}

fn started_described(t: &mut Transcript, target: &str, about: &str) {
    apply(
        t,
        &[
            AgentEvent::turn_started(1),
            described(requested(1, 0, "bg", target), about),
            started("bg"),
            process_started(27, "bg", target),
            backgrounded("bg", 27),
        ],
    );
}

fn row_of(t: &mut Transcript, call_id: &str) -> String {
    let index = t
        .cells()
        .iter()
        .rposition(|c| c.tool_call() == Some(call_id))
        .unwrap_or_else(|| panic!("{call_id}"));
    let rows = t
        .cell_rows(index, &r(140), Detail::Scrollback)
        .unwrap()
        .rows;
    let heads: Vec<&StyledLine> = rows.iter().filter(|l| !l.spans.is_empty()).collect();
    assert_eq!(heads.len(), 1, "{}", text(&rows));
    text(&[heads[0].clone()])
}

fn input_row(call: &Call<'_>, verdict: Option<Typed>) -> String {
    let mut t = transcript();
    started_program(&mut t, PROGRAM);
    t.apply(&process_call(1, "p", call), ms(10));
    t.apply(&started("p"), ms(10));
    t.apply(&done_as("p", verdict, call.id), ms(20));
    row_of(&mut t, "p")
}

#[derive(Clone, Copy)]
enum Start {
    Described,
    Bare,
}

fn rows_of(
    call: &Call<'_>,
    verdict: Option<Typed>,
    start: Start,
    description: Option<&str>,
) -> (String, String) {
    let mut t = transcript();
    match start {
        Start::Described => started_described(&mut t, PROGRAM, ABOUT),
        Start::Bare => started_program(&mut t, PROGRAM),
    }
    let request = process_call(1, "p", call);
    let request = match description {
        Some(d) => described(request, d),
        None => request,
    };
    t.apply(&request, ms(10));
    t.apply(&started("p"), ms(10));
    let running = row_of(&mut t, "p");
    t.apply(&done_as("p", verdict, call.id), ms(20));
    (running, row_of(&mut t, "p"))
}

struct Words<'a> {
    call: Call<'a>,
    verdict: Option<Typed>,
    done: &'a str,
    running: &'a str,
    act: Option<(&'a str, &'a str)>,
}

fn every_kind_of_call() -> Vec<Words<'static>> {
    let enter: &'static [&'static str] = &["Enter"];
    let echo = typed(true, false, false);
    let hidden = typed(false, false, false);
    vec![
        Words {
            call: Call {
                text: Some("yes"),
                keys: enter,
                ..BARE
            },
            verdict: echo,
            done: "Typed into process 27 and pressed Enter: \"yes\"",
            running: "Typing into process 27 and pressing Enter",
            act: Some((
                "typed \"yes\" and pressed Enter",
                "typing and pressing Enter",
            )),
        },
        Words {
            call: Call {
                text: Some("hunter2"),
                keys: enter,
                ..BARE
            },
            verdict: hidden,
            done: "Typed into process 27 and pressed Enter",
            running: "Typing into process 27 and pressing Enter",
            act: Some(("typed and pressed Enter", "typing and pressing Enter")),
        },
        Words {
            call: Call {
                text: Some("yes\n"),
                ..BARE
            },
            verdict: echo,
            done: "Typed into process 27: \"yes\u{23CE}\"",
            running: "Typing into process 27",
            act: Some(("typed \"yes\u{23CE}\"", "typing")),
        },
        Words {
            call: Call {
                text: Some("one\ntwo\nthree\n"),
                ..BARE
            },
            verdict: echo,
            done: "Typed into process 27: \"one\u{23CE}\" (+2 lines)",
            running: "Typing into process 27",
            act: Some(("typed \"one\u{23CE}\" (+2 lines)", "typing")),
        },
        Words {
            call: Call {
                text: Some("hunter2\n"),
                ..BARE
            },
            verdict: hidden,
            done: "Typed into process 27",
            running: "Typing into process 27",
            act: Some(("typed", "typing")),
        },
        Words {
            call: Call {
                text: Some("hunter2\n"),
                ..BARE
            },
            verdict: None,
            done: "Typed into process 27",
            running: "Typing into process 27",
            act: Some(("typed", "typing")),
        },
        Words {
            call: Call {
                keys: &["Up", "Enter"],
                ..BARE
            },
            verdict: echo,
            done: "Pressed Up + Enter in process 27",
            running: "Pressing Up + Enter in process 27",
            act: Some(("pressed Up + Enter", "pressing Up + Enter")),
        },
        Words {
            call: Call {
                text: Some("x\n"),
                signal: Some(SignalKind::Interrupt),
                ..BARE
            },
            verdict: typed(true, false, true),
            done: "Typed into process 27, then interrupted it: \"x\u{23CE}\"",
            running: "Typing into process 27",
            act: Some(("typed \"x\u{23CE}\", then interrupted", "typing")),
        },
        Words {
            call: Call {
                text: Some("x\n"),
                keys: enter,
                signal: Some(SignalKind::Kill),
                ..BARE
            },
            verdict: typed(true, false, true),
            done: "Typed into process 27 and pressed Enter, then stopped it: \"x\u{23CE}\"",
            running: "Typing into process 27 and pressing Enter",
            act: Some((
                "typed \"x\u{23CE}\" and pressed Enter, then stopped",
                "typing and pressing Enter",
            )),
        },
        Words {
            call: Call {
                signal: Some(SignalKind::Interrupt),
                ..BARE
            },
            verdict: None,
            done: "Interrupted process 27",
            running: "Interrupting process 27",
            act: Some(("interrupted", "interrupting")),
        },
        Words {
            call: Call {
                signal: Some(SignalKind::Kill),
                ..BARE
            },
            verdict: None,
            done: "Stopped process 27",
            running: "Stopping process 27",
            act: Some(("stopped", "stopping")),
        },
        Words {
            call: Call {
                waits: true,
                ..BARE
            },
            verdict: None,
            done: "Waited for process 27",
            running: "Waiting for process 27",
            act: Some(("waited", "waiting")),
        },
        Words {
            call: Call {
                read: Some("search"),
                pattern: Some("ERROR"),
                ..BARE
            },
            verdict: None,
            done: "Searched process 27's output for \"ERROR\"",
            running: "Searching process 27's output for \"ERROR\"",
            act: Some((
                "searched the output for \"ERROR\"",
                "searching the output for \"ERROR\"",
            )),
        },
        Words {
            call: Call {
                read: Some("screen"),
                ..BARE
            },
            verdict: None,
            done: "Read process 27's screen",
            running: "Reading process 27's screen",
            act: Some(("read the screen", "reading the screen")),
        },
        Words {
            call: BARE,
            verdict: None,
            done: "Read process 27's output",
            running: "Reading process 27's output",
            act: Some(("read the output", "reading the output")),
        },
        Words {
            call: Call {
                attach: true,
                ..BARE
            },
            verdict: None,
            done: "Asked you to take over process 27",
            running: "Asking you to take over process 27",
            act: Some(("asked you to take over", "asking you to take over")),
        },
        Words {
            call: Call {
                id: None,
                read: None,
                ..BARE
            },
            verdict: None,
            done: "Listed background processes",
            running: "Listing background processes",
            act: None,
        },
    ]
}

fn named_once(row: &str, call: &Call<'_>) {
    let want = usize::from(call.id.is_some());
    assert_eq!(row.matches("process 27").count(), want, "{row}");
    assert!(!row.contains(PROGRAM), "{row}");
    assert!(!row.contains("hunter2"), "{row}");
}

#[test]
fn every_process_row_reads_in_plain_words_with_or_without_a_description() {
    const ASK: &str = "Ask the shell for the users";
    for words in every_kind_of_call() {
        let call = &words.call;
        let (running, done) = rows_of(call, words.verdict, Start::Described, None);
        let about = if call.id.is_some() {
            format!("  {ABOUT}")
        } else {
            String::new()
        };
        assert!(
            done.contains(&format!(" {}{about}", words.done)),
            "{}: {done}",
            words.done
        );
        assert!(
            running.contains(&format!(" {}{about}", words.running)),
            "{}: {running}",
            words.running
        );
        let (running_bare, done_bare) = rows_of(call, words.verdict, Start::Bare, None);
        assert!(done_bare.contains(words.done), "{done_bare}");
        assert!(running_bare.contains(words.running), "{running_bare}");
        let (running_told, done_told) = rows_of(call, words.verdict, Start::Described, Some(ASK));
        match words.act {
            Some((act, act_running)) => {
                assert!(
                    done_told.contains(&format!(" {ASK}  {act} ")),
                    "{act}: {done_told}"
                );
                assert!(
                    running_told.contains(&format!(" {ASK}  {act_running} ")),
                    "{act_running}: {running_told}"
                );
                assert!(done_told.contains("process 27 \u{b7} 1s"), "{done_told}");
            }
            None => {
                assert!(
                    done_told.starts_with(&format!(" \u{2713} {ASK}  ")),
                    "{done_told}"
                );
                assert!(done_told.trim_end().ends_with("  1s"), "{done_told}");
            }
        }
        for row in [
            &running,
            &done,
            &running_bare,
            &done_bare,
            &running_told,
            &done_told,
        ] {
            named_once(row, call);
            assert_eq!(row.lines().count(), 1, "{row}");
        }
        for row in [&running_bare, &done_bare, &running_told, &done_told] {
            assert!(!row.contains(ABOUT), "{row}");
        }
    }
}

#[test]
fn every_state_of_a_process_row_names_its_process_once() {
    const STOP: &str = "Stop the shell";
    let write = Call {
        text: Some("x\n"),
        signal: Some(SignalKind::Interrupt),
        ..BARE
    };
    let mut t = transcript();
    started_described(&mut t, PROGRAM, ABOUT);
    let mut block = 1;
    let mut call = |t: &mut Transcript, call_id: &str, call: &Call<'_>, told: bool| {
        let request = process_call(block, call_id, call);
        block += 1;
        let request = if told {
            described(request, STOP)
        } else {
            request
        };
        t.apply(&request, ms(10));
    };
    let mut rows = vec![];
    for told in [false, true] {
        let tag = |name: &str| format!("{name}-{told}");
        call(&mut t, &tag("queued"), &BARE, told);
        call(&mut t, &tag("stopped"), &BARE, told);
        t.apply(&started(&tag("stopped")), ms(11));
        t.apply(
            &ended_as(&tag("stopped"), ToolOutcome::Cancelled, None, Some(27)),
            ms(12),
        );
        call(&mut t, &tag("slow"), &BARE, told);
        t.apply(&started(&tag("slow")), ms(11));
        t.apply(
            &ended_as(
                &tag("slow"),
                ToolOutcome::TimedOut { after_ms: 2000 },
                None,
                Some(27),
            ),
            ms(12),
        );
        call(&mut t, &tag("partly"), &write, told);
        t.apply(&started(&tag("partly")), ms(11));
        t.apply(&done(&tag("partly"), typed(true, true, false)), ms(12));
        call(&mut t, &tag("held"), &write, told);
        t.apply(&started(&tag("held")), ms(11));
        t.apply(&done(&tag("held"), typed(false, false, false)), ms(12));
        call(&mut t, &tag("refused"), &write, told);
        t.apply(
            &refused(&tag("refused"), "`wait_ms` must be a whole number"),
            ms(12),
        );
        let state =
            |name: &str, made: &str, fact: &str| (tag(name), made.to_owned(), fact.to_owned());
        rows.push((
            told,
            state("queued", "Reading process 27's output", "queued"),
        ));
        rows.push((
            told,
            state("stopped", "Read process 27's output", "stopped"),
        ));
        rows.push((told, state("slow", "Read process 27's output", "timed out")));
        rows.push((
            told,
            state(
                "partly",
                "Typed into process 27: \"x\u{23CE}\"",
                "partly sent",
            ),
        ));
        rows.push((
            told,
            state("held", "Typed into process 27", "interrupt not sent"),
        ));
        rows.push((told, state("refused", "Used jobs 27", "couldn't interrupt")));
    }
    for (told, (call_id, made, fact)) in rows {
        let row = row_of(&mut t, &call_id);
        assert!(row.contains(&fact), "{fact}: {row}");
        let refused_bare = !told && made.starts_with("Used jobs 27");
        assert_eq!(
            row.matches("process 27").count(),
            usize::from(!refused_bare),
            "{row}"
        );
        assert!(!row.contains(PROGRAM), "{row}");
        if told {
            assert!(row.contains(STOP), "{row}");
            assert!(
                row.contains(&format!("process 27 \u{b7} {fact}")),
                "{fact}: {row}"
            );
            assert!(!row.contains(&made), "{row}");
            assert!(!row.contains(ABOUT), "{row}");
        } else {
            assert!(row.contains(&format!("{made}  {ABOUT}")), "{row}");
        }
    }
    let refused_told = row_of(&mut t, "refused-true");
    assert!(
        refused_told.contains(&format!("{STOP}  ")) && !refused_told.contains("typed"),
        "{refused_told}"
    );
    assert!(
        refused_told
            .trim_end()
            .ends_with("process 27 \u{b7} couldn't interrupt"),
        "{refused_told}"
    );
}

#[test]
fn long_typed_text_is_cut_to_forty_columns() {
    let long = "a".repeat(60);
    let row = input_row(
        &Call {
            text: Some(&long),
            ..BARE
        },
        typed(true, false, false),
    );
    let quoted = row.split('"').nth(1).unwrap();
    assert_eq!(quoted.chars().count(), TYPED_COLUMNS, "{row}");
    assert!(quoted.ends_with('\u{2026}'), "{row}");
}

#[test]
fn an_unsent_part_and_a_held_back_interrupt_are_facts() {
    let row = input_row(
        &Call {
            text: Some("x\n"),
            ..BARE
        },
        typed(true, true, false),
    );
    assert!(row.contains("partly sent"), "{row}");
    let row = input_row(
        &Call {
            text: Some("x\n"),
            signal: Some(SignalKind::Interrupt),
            ..BARE
        },
        typed(true, false, false),
    );
    assert!(row.contains("interrupt not sent"), "{row}");
    assert!(!row.contains("then interrupted"), "{row}");
}

#[test]
fn a_running_input_row_uses_the_present_tense_and_never_shows_text() {
    let mut t = transcript();
    started_program(&mut t, PROGRAM);
    t.apply(
        &process_call(
            1,
            "p",
            &Call {
                text: Some("yes\n"),
                ..BARE
            },
        ),
        ms(10),
    );
    t.apply(&started("p"), ms(10));
    let row = row_of(&mut t, "p");
    assert!(row.contains("Typing into process 27"), "{row}");
    assert_eq!(
        t.call_doing("p", Layout::new(80, GlyphSet::Unicode))
            .as_deref(),
        Some("Typing into process 27")
    );
    let mut told = transcript();
    started_described(&mut told, PROGRAM, ABOUT);
    told.apply(
        &described(process_call(1, "p", &BARE), "Check the shell"),
        ms(10),
    );
    told.apply(&started("p"), ms(10));
    assert_eq!(
        told.call_doing("p", Layout::new(80, GlyphSet::Unicode))
            .as_deref(),
        Some("Check the shell")
    );
}

#[test]
fn a_process_takes_its_starting_calls_description_live_by_link_and_after_a_restore() {
    let typed_call = Call {
        text: Some("yes\n"),
        ..BARE
    };
    let mut live = transcript();
    started_described(&mut live, "npm init", ABOUT);
    live.apply(&process_call(1, "p", &typed_call), ms(10));
    let row = row_of(&mut live, "p");
    assert!(
        row.contains(&format!("Typing into process 27  {ABOUT}")),
        "{row}"
    );
    assert!(!row.contains("npm init"), "{row}");
    let mut linked = transcript();
    apply(
        &mut linked,
        &[
            AgentEvent::turn_started(1),
            described(requested(1, 0, "bg", "npm init"), ABOUT),
            started("bg"),
            backgrounded("bg", 27),
            process_call(1, "p", &typed_call),
        ],
    );
    let row = row_of(&mut linked, "p");
    assert!(
        row.contains(&format!("Typing into process 27  {ABOUT}")),
        "{row}"
    );
    for target in ["", "   ", "npm init"] {
        let mut bare = transcript();
        started_program(&mut bare, target);
        bare.apply(&process_call(1, "p", &typed_call), ms(10));
        let row = row_of(&mut bare, "p");
        assert!(row.contains("Typing into process 27"), "{row}");
        assert_eq!(row.matches("process 27").count(), 1, "{row}");
        assert!(!row.contains("npm init"), "{row}");
        assert!(!row.contains("Runs the checks"), "{row}");
    }
    let mut restored = transcript();
    for event in [
        AgentEvent::turn_started(1),
        described(requested(1, 0, "bg", "psql mydb"), ABOUT),
        started("bg"),
        finished("bg", ok(0), view("", 0), 1000),
        process_call(1, "p", &typed_call),
        done("p", typed(false, false, false)),
    ] {
        restored.replay(&event, ms(1));
    }
    let row = row_of(&mut restored, "p");
    assert!(row.contains("Typed into process 27"), "{row}");
    assert!(!row.contains(ABOUT), "{row}");
    let at = restored
        .cells()
        .iter()
        .rposition(|c| c.tool_call() == Some("p"))
        .unwrap();
    let before = restored.cells()[at].version();
    restored.replay(&process_restored(27, "bg", "psql mydb"), ms(2));
    restored.resume_done();
    assert!(restored.cells()[at].version() > before);
    let row = row_of(&mut restored, "p");
    assert!(
        row.contains(&format!("Typed into process 27  {ABOUT}")),
        "{row}"
    );
    assert_eq!(row.matches("process 27").count(), 1, "{row}");
    assert!(!row.contains("psql"), "{row}");
    let page = page_lines(&mut restored, "p");
    assert_eq!(
        page.first().map(String::as_str),
        Some(format!("process 27 \u{b7} {ABOUT} \u{b7} psql mydb").as_str()),
        "{page:#?}"
    );
    let mut unknown = transcript();
    unknown.apply(&AgentEvent::turn_started(1), ms(0));
    unknown.apply(&process_call(0, "p", &typed_call), ms(10));
    unknown.apply(&done("p", typed(false, false, false)), ms(20));
    let before = unknown.cells()[0].version();
    unknown.apply(&process_restored(27, "gone", "psql mydb"), ms(30));
    assert!(unknown.cells()[0].version() > before);
    let row = row_of(&mut unknown, "p");
    assert!(row.contains("Typed into process 27"), "{row}");
    assert_eq!(row.matches("process 27").count(), 1, "{row}");
    assert!(!row.contains("psql"), "{row}");
}

#[test]
fn an_old_log_without_a_view_reads_typed_into_process_n() {
    let mut t = transcript();
    t.apply(&AgentEvent::turn_started(1), ms(0));
    for (block, (id, action, want)) in [
        ("i", ToolAction::Input, "Typed into process 27"),
        ("w", ToolAction::Wait, "Waited for process 27"),
        (
            "s",
            ToolAction::Signal {
                kind: SignalKind::Interrupt,
            },
            "Interrupted process 27",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        t.apply(
            &tool_requested(Requested {
                block,
                call_id: id.to_owned(),
                tool: "jobs".to_owned(),
                input: json!({"id": 27}),
                action,
                target: "27".to_owned(),
                ..Requested::default()
            }),
            ms(1),
        );
        t.apply(&done(id, None), ms(2));
        let row = row_of(&mut t, id);
        assert!(row.contains(want), "{want}: {row}");
        assert!(!row.contains("Typed 27"), "{row}");
    }
}

#[test]
fn a_process_ending_in_a_later_turn_updates_its_starting_row() {
    let mut t = transcript();
    started_program(&mut t, "npm run dev");
    t.apply(&ended(completed(StopReason::EndTurn, 1, 1)), ms(100));
    t.apply(&AgentEvent::turn_started(2), ms(200));
    assert!(row_of(&mut t, "bg").ends_with("process 27 \u{b7} running"));
    t.apply(&process_ended(27, 0), ms(300));
    let row = row_of(&mut t, "bg");
    assert!(row.starts_with(" \u{2713} Ran npm run dev"), "{row}");
    assert!(row.ends_with("process 27 \u{b7} exit 0"), "{row}");
}

#[test]
fn a_command_handed_back_still_running_says_ps_stops_it_until_it_ends() {
    let mut t = transcript();
    let mut back = backgrounded("bg", 27);
    if let AgentEvent::ToolCallFinished { outcome, .. } = &mut back {
        *outcome = ToolOutcome::Backgrounded {
            reason: BackgroundReason::StillRunning,
        };
    }
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            requested(1, 0, "bg", "cargo build --release"),
            started("bg"),
            process_started(27, "bg", "cargo build --release"),
            back,
        ],
    );
    let row = row_of(&mut t, "bg");
    assert!(
        row.ends_with("process 27 \u{b7} running \u{b7} /ps stops it"),
        "{row:?}"
    );
    t.apply(&process_ended(27, 0), ms(300));
    let row = row_of(&mut t, "bg");
    assert!(row.ends_with("process 27 \u{b7} exit 0"), "{row:?}");
}

#[test]
fn a_replayed_background_call_takes_its_end_from_a_restore_or_reads_before_the_restart() {
    let replay = |t: &mut Transcript| {
        for event in [
            AgentEvent::turn_started(1),
            requested(1, 0, "bg", "npm run dev"),
            started("bg"),
            backgrounded("bg", 27),
            ended(completed(StopReason::EndTurn, 1, 1)),
            AgentEvent::turn_started(2),
            process_call(
                0,
                "p",
                &Call {
                    text: Some("q\n"),
                    ..BARE
                },
            ),
            done("p", typed(true, false, false)),
            ended(completed(StopReason::EndTurn, 1, 1)),
        ] {
            t.replay(&event, ms(1));
        }
    };
    let mut restored = transcript();
    replay(&mut restored);
    assert!(row_of(&mut restored, "p").contains("into process 27"));
    restored.replay(&process_restored(27, "bg", "npm run dev"), ms(2));
    restored.resume_done();
    let row = row_of(&mut restored, "bg");
    assert!(
        row.ends_with("process 27 \u{b7} lost \u{b7} before the restart"),
        "{row}"
    );
    let row = row_of(&mut restored, "p");
    assert!(
        row.contains("Typed into process 27: \"q\u{23CE}\""),
        "{row}"
    );
    assert!(!row.contains("npm run dev"), "{row}");
    let page = page_lines(&mut restored, "p");
    assert_eq!(
        page.first().map(String::as_str),
        Some("process 27 \u{b7} npm run dev"),
        "{page:#?}"
    );
    let mut silent = transcript();
    replay(&mut silent);
    silent.resume_done();
    let row = row_of(&mut silent, "bg");
    assert!(row.starts_with(" \u{25E6} Ran npm run dev"), "{row}");
    assert!(
        row.ends_with("process 27 \u{b7} before the restart"),
        "{row}"
    );
}

#[test]
fn a_cut_drops_cell_links_at_or_past_it_and_keeps_names_and_descriptions() {
    let mut after = transcript();
    started_program(&mut after, "npm run dev");
    after.apply(&ended(completed(StopReason::EndTurn, 1, 1)), ms(10));
    after.apply(&AgentEvent::turn_started(2), ms(20));
    after.apply(&process_call(0, "p", &BARE), ms(21));
    after.apply(&ended(completed(StopReason::EndTurn, 1, 1)), ms(22));
    after.apply(&rewound(2), ms(30));
    after.apply(&process_ended(27, 0), ms(40));
    assert!(row_of(&mut after, "bg").ends_with("process 27 \u{b7} exit 0"));
    let mut before = transcript();
    started_described(&mut before, "npm run dev", ABOUT);
    before.apply(&ended(completed(StopReason::EndTurn, 1, 1)), ms(10));
    before.apply(&rewound(1), ms(30));
    before.apply(&AgentEvent::turn_started(2), ms(31));
    before.apply(&requested(1, 0, "x", "ls"), ms(32));
    let cells = before.cells().len();
    before.apply(&process_ended(27, 0), ms(40));
    assert_eq!(before.cells().len(), cells);
    before.apply(&process_call(1, "p", &BARE), ms(41));
    let row = row_of(&mut before, "p");
    assert!(
        row.contains(&format!("Reading process 27's output  {ABOUT}")),
        "{row}"
    );
    let page = page_lines(&mut before, "p");
    assert_eq!(
        page.first().map(String::as_str),
        Some(format!("process 27 \u{b7} {ABOUT} \u{b7} npm run dev").as_str()),
        "{page:#?}"
    );
    assert!(!row_of(&mut before, "x").contains("exit 0"));
    let mut restored = transcript();
    for event in [
        AgentEvent::turn_started(1),
        process_call(0, "p", &BARE),
        AgentEvent::turn_started(2),
        process_call(0, "q", &BARE),
        ended(completed(StopReason::EndTurn, 1, 1)),
        rewound(2),
        requested(1, 0, "x", "ls"),
    ] {
        restored.replay(&event, ms(1));
    }
    let versions: Vec<u64> = restored.cells().iter().map(|c| c.version()).collect();
    restored.replay(&process_restored(27, "bg", "npm run dev"), ms(2));
    let renamed: Vec<Option<&str>> = restored
        .cells()
        .iter()
        .zip(&versions)
        .filter(|(c, v)| c.version() != **v)
        .map(|(c, _)| c.tool_call())
        .collect();
    assert_eq!(renamed, [Some("p")]);
}

#[test]
fn the_rule_shows_typed_text_only_for_echoed_input() {
    assert!(echoed(typed(true, false, false).as_ref()));
    assert!(echoed(typed(true, true, true).as_ref()));
    assert!(!echoed(typed(false, false, false).as_ref()));
    assert!(!echoed(None));
}

fn marker() -> String {
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u64(7);
    format!("MK{:016x}", hasher.finish())
}

fn secret_turn(marker: &str) -> Transcript {
    let long = format!("{marker}{}", "z".repeat(90));
    let old_target = format!("{{\"input\":\"{long}\"}}");
    let old_target: String = old_target.chars().take(79).chain(['\u{2026}']).collect();
    let mut t = transcript();
    started_program(&mut t, PROGRAM);
    let text = format!("{marker}\n");
    let hidden = Call {
        text: Some(&text),
        ..BARE
    };
    let mut events = vec![
        process_call(1, "not-echoed", &hidden),
        started("not-echoed"),
        done("not-echoed", typed(false, false, false)),
        process_call(2, "pipe", &hidden),
        started("pipe"),
        done("pipe", typed(false, false, true)),
        process_call(3, "no-verdict", &hidden),
        started("no-verdict"),
        done("no-verdict", None),
        process_call(4, "running", &hidden),
        started("running"),
    ];
    let mut refused_call = process_call(5, "refused", &Call { text: None, ..BARE });
    if let AgentEvent::ToolCallRequested { input, .. } = &mut refused_call {
        *input = json!({"id": 27, "input": text, "wait_ms": "soon"});
    }
    events.push(refused_call);
    events.push(refused("refused", "`wait_ms` must be a whole number"));
    events.push(tool_requested(Requested {
        block: 6,
        call_id: "old".to_owned(),
        tool: "jobs".to_owned(),
        input: json!({"input": long}),
        action: ToolAction::Other {
            name: "jobs".to_owned(),
        },
        target: old_target,
        ..Requested::default()
    }));
    events.push(refused("old", NO_ID));
    events.push(tool_requested(Requested {
        block: 7,
        call_id: "viewless".to_owned(),
        tool: "jobs".to_owned(),
        input: json!({"id": 27, "input": format!("27\n{marker}\n")}),
        action: ToolAction::Input,
        target: "27".to_owned(),
        ..Requested::default()
    }));
    events.push(started("viewless"));
    events.push(done("viewless", None));
    events.push(described(
        process_call(8, "told-hidden", &hidden),
        "Answer the prompt",
    ));
    events.push(started("told-hidden"));
    events.push(done("told-hidden", typed(false, false, false)));
    let mut guarded = process_call(9, "guarded", &hidden);
    if let AgentEvent::ToolCallRequested { input, .. } = &mut guarded {
        input["description"] = json!(format!("Type {marker} into it"));
    }
    events.push(guarded);
    events.push(started("guarded"));
    events.push(done("guarded", typed(false, false, false)));
    for event in &events {
        t.apply(event, ms(50));
    }
    t
}

#[test]
fn a_secret_typed_into_a_program_never_reaches_a_row_a_page_a_picker_entry_or_the_status() {
    let marker = marker();
    let mut t = secret_turn(&marker);
    let check = |t: &mut Transcript, when: &str| {
        for width in [40, 80, 200] {
            for set in [GlyphSet::Unicode, GlyphSet::Ascii] {
                let r = Renderer::new(Layout::new(width, set));
                for detail in [Detail::Scrollback, Detail::Full] {
                    let all = text(&t.rows(&r, detail));
                    assert!(!all.contains(&marker), "{when} {detail:?} {width}: {all}");
                    assert!(!all.contains("MK"), "{when} {detail:?} {width}: {all}");
                }
                let openables = t.openables(Layout::new(width, set));
                assert_eq!(
                    openables
                        .iter()
                        .filter(|o| matches!(o.target, crate::transcript::OpenTarget::Cell(_)))
                        .count(),
                    tool_cells(t).len(),
                    "{when} {width}: {openables:?}"
                );
                for o in &openables {
                    let line = text(std::slice::from_ref(&o.line));
                    assert!(!line.contains("MK"), "{when} openable {width}: {line}");
                    assert!(
                        !o.filter.contains("MK"),
                        "{when} filter {width}: {}",
                        o.filter
                    );
                }
                for index in 0..t.cells().len() {
                    if let Some(page) = t.cell_page(index, &r) {
                        let mut all = text(std::slice::from_ref(&page.title));
                        all.push_str(&text(&page.body));
                        assert!(!all.contains("MK"), "{when} page {index}: {all}");
                    }
                }
            }
        }
        for call in [
            "not-echoed",
            "pipe",
            "no-verdict",
            "running",
            "refused",
            "old",
            "viewless",
            "told-hidden",
            "guarded",
        ] {
            if let Some(caption) = t.call_caption(call, Layout::new(80, GlyphSet::Unicode)) {
                for part in [caption.title, caption.command, caption.extra]
                    .into_iter()
                    .flatten()
                {
                    assert!(!part.contains(&marker) && !part.contains("MK"));
                }
            }
            let doing = t
                .call_doing(call, Layout::new(80, GlyphSet::Unicode))
                .unwrap_or_default();
            assert!(!doing.contains("MK"), "{when} {call}: {doing}");
        }
    };
    check(&mut t, "running");
    let old = row_of(&mut t, "old");
    assert!(old.contains("Used jobs {}"), "{old}");
    let frame = t.frame(&r(80));
    let framed = format!("{}\n{}", text(&frame.commit), text(&frame.live));
    assert!(!framed.contains("MK"), "{framed}");
    t.apply(&done("running", typed(false, false, false)), ms(60));
    t.apply(&ended(completed(StopReason::EndTurn, 1, 6)), ms(70));
    check(&mut t, "after");
}

#[test]
fn a_refused_process_call_reads_as_a_plain_use_and_never_as_done() {
    let mut t = transcript();
    started_program(&mut t, PROGRAM);
    let listless = Call {
        id: None,
        text: Some("y\n"),
        read: None,
        ..BARE
    };
    let write = Call {
        text: Some("y\n"),
        keys: &["Enter"],
        ..BARE
    };
    for (block, (call_id, call)) in [("listless", &listless), ("write", &write)]
        .into_iter()
        .enumerate()
    {
        t.apply(&process_call(block + 1, call_id, call), ms(10));
        t.apply(&started(call_id), ms(11));
        t.apply(
            &finished(
                call_id,
                ToolOutcome::InvalidInput {
                    message: "`id` is required".to_owned(),
                },
                view("", 0),
                0,
            ),
            ms(12),
        );
    }
    let listless = row_of(&mut t, "listless");
    assert!(listless.contains("Used jobs"), "{listless}");
    assert!(!listless.contains("Listed"), "{listless}");
    let write = row_of(&mut t, "write");
    assert!(write.contains("Used jobs 27"), "{write}");
    assert!(!write.contains(PROGRAM), "{write}");
    assert!(!write.contains("Typed"), "{write}");
    for row in [listless, write] {
        assert!(row.trim_end().ends_with("couldn't start"), "{row}");
    }
}

#[test]
fn a_refused_call_to_an_unknown_process_names_its_number_once() {
    let mut t = transcript();
    t.apply(&AgentEvent::turn_started(1), ms(0));
    let call = Call {
        id: Some(99),
        text: Some("y\n"),
        ..BARE
    };
    t.apply(&process_call(1, "unknown", &call), ms(10));
    t.apply(&refused("unknown", "no process 99"), ms(12));
    let row = row_of(&mut t, "unknown");
    assert!(row.contains("Used jobs 99"), "{row}");
    assert!(!row.contains(&copy::process_name(99)), "{row}");
}

fn page_lines(t: &mut Transcript, call_id: &str) -> Vec<String> {
    let index = t
        .cells()
        .iter()
        .rposition(|c| c.tool_call() == Some(call_id))
        .unwrap_or_else(|| panic!("{call_id}"));
    let page = t.cell_page(index, &r(80)).unwrap();
    page.body
        .iter()
        .map(|l| text(std::slice::from_ref(l)).trim().to_owned())
        .collect()
}

#[test]
fn a_process_page_names_the_process_then_shows_typed_text_only_when_echoed_and_keys_and_patterns_always()
 {
    let mut t = transcript();
    started_described(&mut t, PROGRAM, ABOUT);
    let write = Call {
        text: Some("yes\n"),
        keys: &["Enter"],
        ..BARE
    };
    let search = Call {
        read: Some("search"),
        pattern: Some("Traceback (most recent call last)"),
        ..BARE
    };
    let calls = [
        ("echoed", &write, typed(true, false, false)),
        ("hidden", &write, typed(false, false, false)),
        ("unjudged", &write, None),
        ("search", &search, None),
    ];
    for (block, (call_id, call, verdict)) in calls.into_iter().enumerate() {
        t.apply(&process_call(block + 1, call_id, call), ms(10));
        t.apply(&started(call_id), ms(11));
        t.apply(&done(call_id, verdict), ms(12));
    }
    t.apply(
        &described(process_call(6, "told", &write), "Answer the prompt"),
        ms(13),
    );
    t.apply(&done("told", typed(true, false, false)), ms(14));
    let list = Call {
        id: None,
        read: None,
        ..BARE
    };
    t.apply(
        &described(process_call(7, "list", &list), "See what runs"),
        ms(15),
    );
    t.apply(&done_as("list", None, None), ms(16));
    let name = format!("process 27 \u{b7} {ABOUT} \u{b7} {PROGRAM}");
    for call_id in ["echoed", "hidden", "unjudged", "search", "told"] {
        let lines = page_lines(&mut t, call_id);
        assert_eq!(lines.first(), Some(&name), "{lines:#?}");
        assert!(
            !lines.join("\n").contains("Answer the prompt"),
            "{lines:#?}"
        );
    }
    let listed = page_lines(&mut t, "list");
    assert!(!listed.join("\n").contains("27"), "{listed:#?}");
    assert!(!listed.join("\n").contains("See what runs"), "{listed:#?}");
    let page_of = |t: &mut Transcript, call_id: &str| page_lines(t, call_id).split_off(1);
    let newline = crate::theme::glyphs::Glyphs::of(GlyphSet::Unicode).newline;
    let told = page_of(&mut t, "told");
    assert_eq!(
        told.first().map(String::as_str),
        Some(format!("\"yes{newline}\" + Enter").as_str()),
        "{told:#?}"
    );
    let echoed = page_of(&mut t, "echoed");
    assert_eq!(
        echoed.first().map(String::as_str),
        Some(format!("\"yes{newline}\" + Enter").as_str()),
        "{echoed:#?}"
    );
    for call_id in ["hidden", "unjudged"] {
        let lines = page_of(&mut t, call_id);
        assert_eq!(
            lines.first().map(String::as_str),
            Some("+ Enter"),
            "{lines:#?}"
        );
        assert!(!lines.join("\n").contains("yes"), "{lines:#?}");
    }
    let search = page_of(&mut t, "search");
    assert_eq!(
        search.first().map(String::as_str),
        Some("\"Traceback (most recent call last)\""),
        "{search:#?}"
    );
}

fn cell_entries(t: &Transcript) -> Vec<(usize, String, String, bool)> {
    t.openables(Layout::new(118, GlyphSet::Unicode))
        .into_iter()
        .filter_map(|o| match o.target {
            crate::transcript::OpenTarget::Cell(index) => {
                Some((index, text(std::slice::from_ref(&o.line)), o.filter, o.live))
            }
            crate::transcript::OpenTarget::Transcript => None,
        })
        .collect()
}

#[test]
fn an_input_entry_filters_by_the_whole_program_it_never_shows_and_its_shown_words() {
    let long = format!("node {}", "x".repeat(80));
    let mut t = transcript();
    started_program(&mut t, &long);
    let echoed_call = Call {
        text: Some("yes\n"),
        ..BARE
    };
    t.apply(&process_call(1, "p", &echoed_call), ms(10));
    t.apply(&done("p", typed(true, false, false)), ms(20));
    let entries = cell_entries(&t);
    assert_eq!(entries.len(), 2, "{entries:#?}");
    let (_, line, filter, live) = &entries[0];
    assert!(
        line.contains("Typed into process 27: \"yes\u{23CE}\""),
        "{line}"
    );
    assert!(!line.contains("node"), "{line}");
    assert!(filter.contains(&long), "{filter}");
    assert!(filter.contains("Typed into process 27: \"yes"), "{filter}");
    assert!(filter.contains("process 27"), "{filter}");
    assert!(!live);
}

#[test]
fn a_background_job_is_one_entry_at_its_calls_place_before_and_after_its_end_and_restored() {
    let mut t = transcript();
    started_program(&mut t, "npm run dev");
    t.apply(&ended(completed(StopReason::EndTurn, 1, 1)), ms(100));
    let running = cell_entries(&t);
    assert_eq!(running.len(), 1, "{running:#?}");
    let (index, line, filter, live) = &running[0];
    assert_eq!(t.cells()[*index].tool_call(), Some("bg"));
    assert!(line.starts_with("\u{21D7} Running npm run dev"), "{line}");
    assert!(line.ends_with("process 27 \u{b7} running"), "{line}");
    assert!(filter.contains("running"), "{filter}");
    assert!(!live);
    t.apply(&process_ended(27, 1), ms(300));
    let ended_entries = cell_entries(&t);
    assert_eq!(ended_entries.len(), 1, "{ended_entries:#?}");
    let (_, line, filter, _) = &ended_entries[0];
    assert!(line.starts_with("\u{2A2F} Ran npm run dev"), "{line}");
    assert!(line.ends_with("process 27 \u{b7} exit 1"), "{line}");
    assert!(filter.contains("exit 1"), "{filter}");
    assert!(filter.contains(copy::FAILED_FACT), "{filter}");
    let mut restored = transcript();
    for event in [
        AgentEvent::turn_started(1),
        requested(1, 0, "bg", "npm run dev"),
        started("bg"),
        backgrounded("bg", 27),
        ended(completed(StopReason::EndTurn, 1, 1)),
    ] {
        restored.replay(&event, ms(1));
    }
    restored.replay(&process_restored(27, "bg", "npm run dev"), ms(2));
    restored.resume_done();
    let entries = cell_entries(&restored);
    assert_eq!(entries.len(), 1, "{entries:#?}");
    assert!(
        entries[0]
            .1
            .ends_with("process 27 \u{b7} lost \u{b7} before the restart"),
        "{entries:#?}"
    );
}

fn every_width_turn() -> Transcript {
    let mut t = transcript();
    started_described(&mut t, PROGRAM, ABOUT);
    let long = format!("{}\n", "a".repeat(60));
    let echoed_long = Call {
        text: Some(&long),
        ..BARE
    };
    let calls = [
        ("made", &BARE, None),
        ("told", &BARE, Some("Ask the planner to revise the plan")),
        ("long", &echoed_long, None),
        ("long-told", &echoed_long, Some("Answer the long prompt")),
    ];
    for (block, (call_id, call, description)) in calls.into_iter().enumerate() {
        let request = process_call(block + 1, call_id, call);
        let request = match description {
            Some(d) => described(request, d),
            None => request,
        };
        t.apply(&request, ms(10));
        t.apply(&started(call_id), ms(11));
        t.apply(&done(call_id, typed(true, false, false)), ms(12));
    }
    t
}

#[test]
fn a_process_row_is_one_line_at_every_width_and_keeps_its_process_at_eighty_and_wider() {
    let mut t = every_width_turn();
    for set in [GlyphSet::Unicode, GlyphSet::Ascii] {
        for width in [40, 60, 80, 120, 200] {
            let layout = Layout::new(width, set);
            let rows: Vec<String> = t
                .rows(&Renderer::new(layout), Detail::Scrollback)
                .iter()
                .map(|l| text(std::slice::from_ref(l)))
                .filter(|l| !l.trim().is_empty())
                .collect();
            assert_eq!(rows.len(), 5, "{set:?} {width}: {rows:#?}");
            for row in &rows[1..] {
                assert!(
                    crate::text::width(row, layout.width_mode) <= width,
                    "{width}: {row}"
                );
                assert!(!row.contains(PROGRAM), "{row}");
                if width >= 80 {
                    assert_eq!(row.matches("process 27").count(), 1, "{width}: {row}");
                }
                if set == GlyphSet::Ascii {
                    assert!(row.is_ascii(), "{row}");
                }
            }
            let told = &rows[2];
            assert_eq!(
                told.contains("Ask the planner to revise the plan"),
                width >= 60,
                "{width}: {told}"
            );
            assert!(told.contains("process 27"), "{width}: {told}");
            if width == 60 {
                assert!(!told.contains("read the output"), "{told}");
            }
            if width >= 120 {
                assert!(
                    told.contains("Ask the planner to revise the plan  read the output"),
                    "{told}"
                );
            }
            let long = &rows[3];
            if width >= 80 {
                assert!(long.contains("Typed into process 27: \"aaa"), "{long}");
                assert!(!long.contains(&"a".repeat(TYPED_COLUMNS)), "{long}");
            }
            if width == 80 {
                assert!(!long.contains(ABOUT), "{long}");
            }
            for entry in t.openables(layout) {
                let crate::transcript::OpenTarget::Cell(index) = entry.target else {
                    continue;
                };
                if t.cells()[index].tool_call() == Some("bg") {
                    continue;
                }
                let line = text(std::slice::from_ref(&entry.line));
                assert!(!line.contains(PROGRAM), "{line}");
                assert!(entry.filter.contains(PROGRAM), "{}", entry.filter);
            }
        }
    }
    for call_id in ["made", "told", "long", "long-told"] {
        let doing = t
            .call_doing(call_id, Layout::new(80, GlyphSet::Unicode))
            .unwrap_or_default();
        assert!(!doing.contains(PROGRAM), "{doing}");
    }
    assert_eq!(
        t.call_doing("told", Layout::new(80, GlyphSet::Unicode))
            .as_deref(),
        Some("Ask the planner to revise the plan")
    );
}

fn ends_of(told: bool) -> Vec<(String, String)> {
    let request = |block: usize, call_id: &str| {
        let event = process_call(block, call_id, &BARE);
        if told {
            described(event, "Check the shell")
        } else {
            event
        }
    };
    let start = |t: &mut Transcript, replay: bool| {
        let mut bg = requested(1, 0, "bg", PROGRAM);
        if let AgentEvent::ToolCallRequested { display, .. } = &mut bg {
            display.title = Some(ABOUT.to_owned());
        }
        for event in [
            AgentEvent::turn_started(1),
            bg,
            started("bg"),
            process_started(27, "bg", PROGRAM),
            backgrounded("bg", 27),
        ] {
            if replay {
                t.replay(&event, ms(1));
            } else {
                t.apply(&event, ms(1));
            }
        }
    };
    let mut live = transcript();
    start(&mut live, false);
    live.apply(&request(1, "abandoned"), ms(10));
    live.apply(&request(2, "not-run"), ms(10));
    live.apply(
        &ended_as(
            "not-run",
            ToolOutcome::NotRun {
                reason: wisp_protocol::NotRunReason::TurnStopped,
            },
            None,
            None,
        ),
        ms(11),
    );
    live.apply(&request(3, "invalid"), ms(10));
    live.apply(
        &ended_as(
            "invalid",
            ToolOutcome::InvalidInput {
                message: "`lines` must be 1 or more".to_owned(),
            },
            None,
            None,
        ),
        ms(11),
    );
    live.apply(&ended(completed(StopReason::EndTurn, 1, 3)), ms(20));
    let mut replayed = transcript();
    start(&mut replayed, true);
    replayed.replay(&request(1, "interrupted"), ms(10));
    replayed.replay(&started("interrupted"), ms(10));
    replayed.replay(&ended(completed(StopReason::EndTurn, 1, 1)), ms(20));
    replayed.resume_done();
    let mut rows: Vec<(String, String)> = ["abandoned", "not-run", "invalid"]
        .into_iter()
        .map(|id| (id.to_owned(), row_of(&mut live, id)))
        .collect();
    rows.push((
        "interrupted".to_owned(),
        row_of(&mut replayed, "interrupted"),
    ));
    rows
}

#[test]
fn a_process_row_that_never_finishes_normally_names_its_process_once() {
    for told in [false, true] {
        for (call_id, row) in ends_of(told) {
            let fact = match call_id.as_str() {
                "abandoned" => "not run \u{b7} the turn was stopped",
                "not-run" => "not run",
                "invalid" => "couldn't start",
                _ => "interrupted \u{b7} Wisp stopped while it ran",
            };
            assert!(row.contains(fact), "{call_id}: {row}");
            assert!(!row.contains(PROGRAM), "{row}");
            if told {
                assert!(row.contains("Check the shell"), "{row}");
                assert!(
                    row.contains(&format!("process 27 \u{b7} {fact}")),
                    "{call_id}: {row}"
                );
                assert_eq!(row.matches("process 27").count(), 1, "{call_id}: {row}");
            } else if call_id == "invalid" {
                assert!(row.contains(&format!("Used jobs 27  {ABOUT}")), "{row}");
                assert!(!row.contains("process 27"), "{row}");
            } else {
                assert!(row.contains("process 27's output"), "{call_id}: {row}");
                assert_eq!(row.matches("process 27").count(), 1, "{call_id}: {row}");
            }
        }
    }
}

#[test]
fn a_described_refused_call_to_an_unknown_process_names_it_once() {
    let mut t = transcript();
    t.apply(&AgentEvent::turn_started(1), ms(0));
    let call = Call {
        id: Some(99),
        text: Some("y\n"),
        ..BARE
    };
    t.apply(
        &described(process_call(1, "unknown", &call), "Answer the prompt"),
        ms(10),
    );
    t.apply(&refused("unknown", &no_process(99)), ms(12));
    let row = row_of(&mut t, "unknown");
    assert!(row.contains("Answer the prompt"), "{row}");
    assert!(!row.contains("typed"), "{row}");
    assert!(
        row.trim_end().ends_with("process 99 \u{b7} couldn't type"),
        "{row}"
    );
    assert_eq!(row.matches("process 99").count(), 1, "{row}");
}

fn hook_rewrote(call_id: &str, marker: &str) -> [AgentEvent; 2] {
    [
        AgentEvent::HookRan {
            hook: "aa/guard".to_owned(),
            point: "before-tool".to_owned(),
            turn: Some(1),
            step: Some(1),
            call_id: Some(call_id.to_owned()),
            outcome: wisp_protocol::HookOutcome::Applied,
            ms: 3,
        },
        AgentEvent::HookApplied {
            hook: "aa/guard".to_owned(),
            point: "before-tool".to_owned(),
            turn: Some(1),
            step: Some(1),
            call_id: Some(call_id.to_owned()),
            input_id: None,
            change: wisp_protocol::HookChange::Input {
                before: json!({"id": 27, "input": "y\n"}),
                after: json!({"id": 27, "input": format!("{marker}\n")}),
            },
        },
    ]
}

fn every_view(t: &mut Transcript) -> String {
    let r = r(80);
    let mut all = text(&t.rows(&r, Detail::Scrollback));
    all.push_str(&text(&t.rows(&r, Detail::Full)));
    for index in 0..t.cells().len() {
        if let Some(page) = t.cell_page(index, &r) {
            all.push_str(&text(std::slice::from_ref(&page.title)));
            all.push_str(&text(&page.body));
        }
    }
    all
}

#[test]
fn a_hooks_rewrite_of_typed_input_shows_only_field_names_until_an_echoed_verdict() {
    let marker = marker();
    let typed_in = Call {
        text: Some("y\n"),
        ..BARE
    };
    for (verdict, shown) in [
        (None, false),
        (typed(false, false, false), false),
        (typed(true, false, false), true),
    ] {
        let mut t = transcript();
        started_program(&mut t, PROGRAM);
        t.apply(&process_call(1, "c", &typed_in), ms(10));
        for event in hook_rewrote("c", &marker) {
            t.apply(&event, ms(20));
        }
        t.apply(&started("c"), ms(30));
        let before = every_view(&mut t);
        assert!(!before.contains("MK"), "before the verdict: {before}");
        assert!(
            before
                .contains("hook aa/guard at before-tool applied \u{b7} 3 ms \u{b7} changed input"),
            "{before}"
        );
        assert!(before.contains("rewritten by aa/guard"), "{before}");
        t.apply(&done("c", verdict), ms(40));
        t.apply(&ended(completed(StopReason::EndTurn, 1, 6)), ms(50));
        let after = every_view(&mut t);
        assert_eq!(after.contains(&marker), shown, "{verdict:?}: {after}");
        let full = text(&t.rows(&r(80), Detail::Full));
        assert_eq!(
            full.matches(&marker).count(),
            if shown { 2 } else { 0 },
            "{full}"
        );
        if shown {
            assert!(
                full.contains(&format!("changed input = \"{marker}\u{23ce}\"")),
                "{full}"
            );
        }
        let scrollback = text(&t.rows(&r(80), Detail::Scrollback));
        assert!(!scrollback.contains("hook aa/guard"), "{scrollback}");
    }
}
