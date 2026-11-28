use serde_json::{Value, json};
use wisp_protocol::{
    AgentEvent, BackgroundReason, BlockKind, CacheEffect, Dropped, Fact, OutputKind, ProcessEnd,
    RewindPoint, RewindReason, SignalKind, StopReason, ToolAction, ToolDisplay, ToolOutcome,
    ToolPurpose, ToolView, Typed,
};
use wisp_tui::app::replay;
use wisp_tui::app::resumed::Resumed;
use wisp_tui::term::{Input, Key, KeyCode, MouseKind};

use super::super::select::Fake;
use super::{
    Look, Screen, Stage, cancelled, completed, cost, ended, facts, snapshot, staged,
    staged_session, te, tool_looks,
};
use crate::Drive;
use crate::agents::main_spent;
use crate::events::typed;

const PSQL: u32 = 31;
const DEV: u32 = 27;
const START_DEV: &str = "Start the dev server";
const TODO: &str = include_str!("../fixtures/todo-list.jsonl");

fn tool() -> String {
    "terminal".to_owned()
}

fn process_view(id: Option<u32>) -> ToolView {
    ToolView::Process {
        id,
        text: None,
        keys: Vec::new(),
        signal: None,
        read: None,
        pattern: None,
    }
}

fn display(action: ToolAction, target: &str, view: Option<ToolView>) -> ToolDisplay {
    ToolDisplay {
        action,
        verbs: None,
        target: target.to_owned(),
        output: OutputKind::Text,
        view,
        title: None,
        purpose: ToolPurpose::Run,
    }
}

struct Call {
    id: &'static str,
    input: Value,
    display: ToolDisplay,
    outcome: ToolOutcome,
    typed: Option<Typed>,
}

fn ok() -> ToolOutcome {
    ToolOutcome::Completed {
        is_error: false,
        exit: None,
    }
}

fn refused(message: &str) -> ToolOutcome {
    ToolOutcome::InvalidInput {
        message: message.to_owned(),
    }
}

fn write(
    id: &'static str,
    process: u32,
    text: Option<&str>,
    keys: &[&str],
    signal: Option<SignalKind>,
    verdict: Option<Typed>,
) -> Call {
    let mut input = json!({"id": process});
    if let Some(text) = text {
        input["input"] = json!(text);
    }
    if !keys.is_empty() {
        input["keys"] = json!(keys);
    }
    let view = ToolView::Process {
        id: Some(process),
        text: text.map(str::to_owned),
        keys: keys.iter().map(|k| (*k).to_owned()).collect(),
        signal,
        read: None,
        pattern: None,
    };
    let action = match signal {
        Some(kind) => ToolAction::Signal { kind },
        None => ToolAction::Input,
    };
    Call {
        id,
        input,
        display: display(action, &process.to_string(), Some(view)),
        outcome: ok(),
        typed: verdict,
    }
}

fn plain(id: &'static str, action: ToolAction, view: ToolView, input: Value) -> Call {
    let target = match &view {
        ToolView::Process { id: Some(n), .. } => n.to_string(),
        _ => String::new(),
    };
    Call {
        id,
        input,
        display: display(action, &target, Some(view)),
        outcome: ok(),
        typed: None,
    }
}

fn old_format(id: &'static str, marker: &str) -> Call {
    let input = json!({"input": marker, "action": "write"});
    let raw = input.to_string();
    let cut: String = raw.chars().take(79).collect();
    Call {
        id,
        input,
        display: display(
            ToolAction::Other { name: tool() },
            &format!("{cut}\u{2026}"),
            None,
        ),
        outcome: refused("`id` is required to write to a process"),
        typed: None,
    }
}

fn told(mut call: Call, description: &str) -> Call {
    call.input["description"] = json!(description);
    call.display.title = Some(description.to_owned());
    call
}

fn answered(call: &Call, done: &mut AgentEvent) {
    let process = call
        .input
        .get("id")
        .and_then(Value::as_u64)
        .and_then(|id| u32::try_from(id).ok())
        .filter(|_| call.outcome == ok());
    if let AgentEvent::ToolCallFinished {
        typed,
        output,
        facts,
        ..
    } = done
    {
        *typed = call.typed;
        output.file = None;
        facts.extend(process.map(|id| Fact::Process { id }));
    }
}

fn refused_write(id: &'static str, marker: &str) -> Call {
    Call {
        id,
        input: json!({"id": PSQL, "input": marker, "keys": ["Bogus"]}),
        display: display(
            ToolAction::Input,
            &PSQL.to_string(),
            Some(process_view(Some(PSQL))),
        ),
        outcome: refused("unknown key: Bogus"),
        typed: None,
    }
}

async fn background(
    s: &Stage,
    block: usize,
    (call, id, command): (&str, u32, &str),
    tty: bool,
    title: Option<&str>,
) {
    s.event(te::block(block)).await;
    let mut asked = te::requested(1, block, call, command);
    if let AgentEvent::ToolCallRequested { display, .. } = &mut asked {
        display.title = title.map(str::to_owned);
    }
    s.event(asked).await;
    s.event(te::started(call)).await;
    s.event(AgentEvent::ProcessStarted {
        id,
        call_id: call.to_owned(),
        target: command.to_owned(),
        tty,
        stdin: true,
        size: None,
        file: None,
        reason: BackgroundReason::Started,
        log: None,
        keeps_running: false,
        pid: None,
    })
    .await;
    let mut done = te::finished(
        call,
        ToolOutcome::Backgrounded {
            reason: BackgroundReason::Started,
        },
        "",
        0,
        1_500,
    );
    if let AgentEvent::ToolCallFinished { facts, process, .. } = &mut done {
        *facts = vec![Fact::Process { id }];
        *process = Some(id);
    }
    s.event(done).await;
}

async fn request(s: &Stage, step: u32, block: usize, call: &Call) {
    s.event(AgentEvent::BlockStarted {
        index: block,
        kind: BlockKind::ToolUse {
            writing: Some("input for a process".to_owned()),
            name: tool(),
        },
    })
    .await;
    s.event(AgentEvent::ToolCallRequested {
        step,
        block,
        call_id: call.id.to_owned(),
        tool: tool(),
        input: call.input.clone(),
        display: call.display.clone(),
        refused: false,
    })
    .await;
}

async fn finish(s: &Stage, call: &Call) {
    s.event(te::started(call.id)).await;
    let mut done = te::finished(call.id, call.outcome.clone(), "", 0, 1_000);
    answered(call, &mut done);
    s.event(done).await;
}

async fn programs(s: &Stage) {
    s.send("Open the database and the dev server").await;
    s.event(te::step(1)).await;
    background(s, 0, ("dev", DEV, "npm run dev"), false, Some(START_DEV)).await;
    background(s, 1, ("psql", PSQL, "psql -U app notes"), true, None).await;
    s.event(te::step_ended(1, 0.004)).await;
}

fn typed_calls() -> Vec<Call> {
    let long = "insert into notes (title, body) values ('a long title', 'and a longer body');";
    vec![
        write(
            "echoed",
            PSQL,
            Some("select count(*) from notes;"),
            &["Enter"],
            None,
            typed(true, false, false),
        ),
        write(
            "hidden",
            PSQL,
            Some("hunter2"),
            &["Enter"],
            None,
            typed(false, false, false),
        ),
        write(
            "keys",
            PSQL,
            None,
            &["Up", "Enter"],
            None,
            typed(false, false, false),
        ),
        write(
            "multi",
            PSQL,
            Some("begin;\nupdate notes set seen = true;\ncommit;"),
            &[],
            None,
            typed(true, false, false),
        ),
        write(
            "long",
            PSQL,
            Some(long),
            &[],
            None,
            typed(true, false, false),
        ),
        write(
            "interrupted",
            PSQL,
            Some("select pg_sleep(60);"),
            &["Enter"],
            Some(SignalKind::Interrupt),
            typed(true, false, true),
        ),
        write(
            "unsignalled",
            PSQL,
            Some("\\dt"),
            &[],
            Some(SignalKind::Interrupt),
            typed(true, false, false),
        ),
        write(
            "partly",
            PSQL,
            Some("select 1;"),
            &["Enter"],
            None,
            typed(true, true, false),
        ),
        write("unjudged", PSQL, Some("select 2;"), &[], None, None),
        refused_write("refused", "hunter3"),
        old_format("old", "old secret"),
        plain(
            "search",
            ToolAction::Search,
            ToolView::Process {
                id: Some(DEV),
                text: None,
                keys: Vec::new(),
                signal: None,
                read: Some("output".to_owned()),
                pattern: Some("ERROR".to_owned()),
            },
            json!({"id": DEV, "read": "output", "pattern": "ERROR"}),
        ),
        plain(
            "screen",
            ToolAction::Read,
            ToolView::Process {
                id: Some(PSQL),
                text: None,
                keys: Vec::new(),
                signal: None,
                read: Some("screen".to_owned()),
                pattern: None,
            },
            json!({"id": PSQL, "read": "screen"}),
        ),
        plain(
            "wait",
            ToolAction::Wait,
            process_view(Some(DEV)),
            json!({"id": DEV, "wait_ms": 5000}),
        ),
        plain(
            "stop",
            ToolAction::Signal {
                kind: SignalKind::Kill,
            },
            ToolView::Process {
                id: Some(DEV),
                text: None,
                keys: Vec::new(),
                signal: Some(SignalKind::Kill),
                read: None,
                pattern: None,
            },
            json!({"id": DEV, "signal": "kill"}),
        ),
        plain(
            "list",
            ToolAction::Other { name: tool() },
            process_view(None),
            json!({}),
        ),
        told(
            plain(
                "told",
                ToolAction::Read,
                process_view(Some(DEV)),
                json!({"id": DEV}),
            ),
            "Check the dev server came up",
        ),
    ]
}

async fn typed_rows(s: Stage) {
    programs(&s).await;
    s.event(te::step(2)).await;
    let calls = typed_calls();
    for (block, call) in calls.iter().enumerate() {
        request(&s, 2, block, call).await;
    }
    s.event(main_spent((0.008, Some(0.0), false), &[])).await;
    s.event(te::step_ended(2, 0.004)).await;
    s.wait(100).await.shot("queued");
    for call in &calls {
        finish(&s, call).await;
    }
    s.event(ended(
        completed(StopReason::EndTurn, true, Some(cost(0.01, 0.02, false))),
        9_000,
    ))
    .await;
    s.wait(3_000).await.shot("done");
}

#[tokio::test(start_paused = true)]
async fn typed_rows_at_80_and_120_in_light_dark_ascii_and_reduced_motion() {
    for look in tool_looks() {
        snapshot("typed", look, &staged(look, typed_rows).await);
    }
}

fn rows_of(shots: &[(String, String)], name: &str) -> Vec<String> {
    let shot = &shots.iter().find(|(n, _)| n == name).unwrap().1;
    shot.lines()
        .filter_map(|l| l.strip_prefix('|'))
        .map(str::to_owned)
        .collect()
}

#[tokio::test(start_paused = true)]
async fn typed_rows_show_text_only_when_echoed_and_name_their_process_once() {
    let look = Look {
        rows: 70,
        ..super::truecolor(
            wisp_render::theme::Mode::Dark,
            wisp_render::theme::glyphs::GlyphSet::Unicode,
            120,
        )
    };
    let shots = staged(look, typed_rows).await;
    let rows = rows_of(&shots, "done");
    let row = |needle: &str| {
        rows.iter()
            .find(|r| r.contains(needle))
            .cloned()
            .unwrap_or_else(|| panic!("{needle}: {rows:#?}"))
    };
    let echoed = row("Typed into process 31 and pressed Enter: \"select count(*) from notes;\"");
    assert!(echoed.trim_end().ends_with("  1s"), "{echoed}");
    row("Typed into process 31 and pressed Enter  ");
    row("Pressed Up + Enter in process 31");
    row("Typed into process 31: \"begin;\u{23CE}\" (+2 lines)");
    row("Typed into process 31 and pressed Enter, then interrupted it: \"select pg_sleep(60);\"");
    assert!(row("Typed into process 31: \"\\dt\"").contains("interrupt not sent"));
    assert!(row("Typed into process 31 and pressed Enter: \"select 1;\"").contains("partly sent"));
    row("Typed into process 31  ");
    let refused = row("Used terminal 31");
    assert!(refused.trim_end().ends_with("couldn't start"), "{refused}");
    assert!(
        !rows
            .iter()
            .any(|r| r.contains("Typed") && r.contains("couldn't start")),
        "{rows:#?}"
    );
    row(&format!(
        "Searched process 27's output for \"ERROR\"  {START_DEV}"
    ));
    row("Read process 31's screen  ");
    row(&format!("Waited for process 27  {START_DEV}"));
    row(&format!("Stopped process 27  {START_DEV}"));
    row("Listed background processes");
    let told = row("Check the dev server came up  read the output");
    assert!(told.trim_end().ends_with("process 27 \u{b7} 1s"), "{told}");
    for process in ["process 27", "process 31"] {
        for r in rows.iter().filter(|r| r.contains(process)) {
            assert_eq!(r.matches(process).count(), 1, "{r}");
        }
    }
    for command in ["npm run dev", "psql -U app notes"] {
        assert_eq!(
            rows.iter().filter(|r| r.contains(command)).count(),
            1,
            "{command}: {rows:#?}"
        );
    }
    for hidden in ["hunter2", "hunter3", "old secret", "select 2;"] {
        assert!(
            !rows.iter().any(|r| r.contains(hidden)),
            "{hidden}: {rows:#?}"
        );
    }
    let queued = rows_of(&shots, "queued");
    for hidden in ["select count", "hunter2", "begin;"] {
        assert!(
            !queued.iter().any(|r| r.contains(hidden)),
            "{hidden}: {queued:#?}"
        );
    }
}

async fn writing(s: Stage) {
    s.send("Lint it").await;
    s.event(te::step(1)).await;
    s.event(te::block(0)).await;
    s.wait(14_000).await.shot("writing");
    s.event(te::requested(1, 0, "a", "cargo clippy --workspace"))
        .await;
    s.event(AgentEvent::BlockStarted {
        index: 1,
        kind: BlockKind::ToolUse {
            writing: None,
            name: "lint".to_owned(),
        },
    })
    .await;
    s.wait(2_000).await.shot("unnamed");
    s.event(ended(cancelled(), 17_000)).await;
    s.wait(3_000).await.shot("not sent");
}

#[tokio::test(start_paused = true)]
async fn writing_rows_at_80_and_120_in_light_dark_ascii_and_reduced_motion() {
    for look in tool_looks() {
        let shots = staged(look, writing).await;
        let rows = rows_of(&shots, "writing");
        let status = rows
            .iter()
            .rev()
            .filter(|r| !r.trim().is_empty())
            .nth(1)
            .unwrap();
        assert!(status.contains("Writing a command"), "{status}");
        snapshot("writing", look, &shots);
    }
}

fn todo_events() -> Vec<AgentEvent> {
    replay::parse(TODO).unwrap()
}

async fn todo_live(s: Stage) {
    s.send("Work through the todo list").await;
    let events = todo_events();
    let live = |e: &AgentEvent| {
        !matches!(
            e,
            AgentEvent::TurnStarted { .. }
                | AgentEvent::InputAccepted { .. }
                | AgentEvent::InputDelivered { .. }
                | AgentEvent::ProcessRestored { .. }
        )
    };
    let mut steps_ended = 0;
    for event in events.into_iter().filter(live) {
        let ends_step = matches!(event, AgentEvent::StepEnded { .. });
        s.event(event).await;
        if ends_step {
            steps_ended += 1;
            if steps_ended == 2 {
                s.wait(100).await.shot("queued");
            }
        }
    }
    s.wait(3_000).await.shot("live");
}

fn resumed_todo() -> Resumed {
    Resumed {
        view: wisp_protocol::SessionView {
            events: todo_events(),
            ..Default::default()
        },
        ..Default::default()
    }
}

async fn todo_resumed(look: Look) -> Vec<(String, String)> {
    staged_session(
        Screen::new(look.rows, look.width),
        look,
        facts(),
        Some(resumed_todo()),
        |_| {},
        |s| async move {
            s.wait(500).await.shot("resumed");
        },
    )
    .await
}

#[tokio::test(start_paused = true)]
async fn todo_list_rows_live_and_resumed_at_80_and_120_in_light_dark_ascii_and_reduced_motion() {
    for look in tool_looks() {
        let mut shots = staged(look, todo_live).await;
        shots.extend(todo_resumed(look).await);
        snapshot("todo", look, &shots);
    }
}

#[tokio::test(start_paused = true)]
async fn a_resumed_session_names_its_programs_and_shows_restored_ends() {
    let look = Look {
        rows: 70,
        ..super::truecolor(
            wisp_render::theme::Mode::Dark,
            wisp_render::theme::glyphs::GlyphSet::Unicode,
            120,
        )
    };
    let shots = todo_resumed(look).await;
    let rows = rows_of(&shots, "resumed");
    let row = |needle: &str| {
        rows.iter()
            .find(|r| r.contains(needle))
            .cloned()
            .unwrap_or_else(|| panic!("{needle}: {rows:#?}"))
    };
    let dev = row("Start the dev server");
    assert!(dev.contains("process 27"), "{dev}");
    assert!(dev.contains("before the restart"), "{dev}");
    assert!(!dev.contains("\u{21D7}"), "{dev}");
    let psql = row("Ran psql -U app notes");
    assert!(psql.contains("process 31"), "{psql}");
    assert!(psql.contains("exit 0"), "{psql}");
    assert!(psql.contains("before the restart"), "{psql}");
    row("Typed into process 31 and pressed Enter: \"select count(*) from notes;\"");
    row("Typed into process 31 and pressed Enter  ");
    assert!(row("Build the docs").contains("not run"));
    for hidden in ["NOT-ECHOED-SECRET", "REFUSED-SECRET", "OLD-FORMAT-SECRET"] {
        assert!(
            !rows.iter().any(|r| r.contains(hidden)),
            "{hidden}: {rows:#?}"
        );
    }
}

fn marker() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(7);
    format!("MK{:x}{:x}", std::process::id(), nanos)
}

fn secret_calls(marker: &str) -> Vec<Call> {
    let long = format!("{marker}{}", "z".repeat(90));
    vec![
        write(
            "hidden",
            PSQL,
            Some(marker),
            &["Enter"],
            None,
            typed(false, false, false),
        ),
        write(
            "pipe",
            DEV,
            Some(marker),
            &["Enter"],
            None,
            typed(false, false, false),
        ),
        refused_write("refused", marker),
        write("unjudged", PSQL, Some(marker), &[], None, None),
        old_format("old", &long),
        Call {
            id: "viewless",
            input: json!({"id": PSQL, "input": format!("{PSQL}\n{marker}\n")}),
            display: display(ToolAction::Input, &PSQL.to_string(), None),
            outcome: ok(),
            typed: None,
        },
        told(
            write(
                "told",
                PSQL,
                Some(marker),
                &["Enter"],
                None,
                typed(false, false, false),
            ),
            "Answer the prompt",
        ),
        guarded(marker),
    ]
}

fn guarded(marker: &str) -> Call {
    let mut call = write(
        "guarded",
        PSQL,
        Some(marker),
        &["Enter"],
        None,
        typed(false, false, false),
    );
    call.input["description"] = json!(format!("Log in with {marker}"));
    call
}

fn plain_text(raw: &str) -> String {
    let mut out = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        if chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        }
    }
    out
}

fn assert_hidden(marker: &str, what: &str, text: &str) {
    assert!(!text.contains(marker), "{what} shows the marker: {text}");
    let short: String = marker.chars().take(6).collect();
    assert!(
        !text.contains(&short),
        "{what} shows part of the marker: {text}"
    );
}

#[tokio::test(start_paused = true)]
async fn typed_secrets_never_reach_rows_full_detail_the_reader_the_picker_a_copy_or_the_hand_off() {
    let marker = marker();
    let fake = Fake::new("typed-secret");
    let clipboard = fake.clipboard();
    let look = Look {
        rows: 40,
        ..super::truecolor(
            wisp_render::theme::Mode::Dark,
            wisp_render::theme::glyphs::GlyphSet::Unicode,
            100,
        )
    };
    let screen = Screen::new(look.rows, look.width);
    let fake = &fake;
    let secret = marker.clone();
    super::staged_tuned(
        screen.clone(),
        look,
        facts(),
        |options| {
            options.clipboard = clipboard;
            options.copy_on_release = false;
        },
        |s| async move {
            let marker = secret;
            programs(&s).await;
            s.event(te::step(2)).await;
            let calls = secret_calls(&marker);
            for (block, call) in calls.iter().enumerate() {
                request(&s, 2, block, call).await;
            }
            s.event(te::step_ended(2, 0.004)).await;
            for call in &calls {
                s.event(te::started(call.id)).await;
            }
            s.wait(500).await;
            assert_hidden(&marker, "running rows", &s.screen.visible().join("\n"));
            for call in &calls {
                let mut done = te::finished(call.id, call.outcome.clone(), "", 0, 1_000);
                answered(call, &mut done);
                s.event(done).await;
            }
            s.event(ended(
                completed(StopReason::EndTurn, true, Some(cost(0.01, 0.02, false))),
                9_000,
            ))
            .await;
            s.wait(3_000).await;
            let main = s.screen.visible();
            assert_hidden(&marker, "main rows", &main.join("\n"));
            let rows: Vec<usize> = (0..main.len())
                .filter(|&y| {
                    let r = &main[y];
                    r.contains("into process")
                        || r.contains("Answer the prompt")
                        || r.contains("Used terminal")
                })
                .collect();
            assert_eq!(rows.len(), calls.len(), "{main:#?}");
            for y in rows {
                let col = main[y].chars().position(|c| c.is_alphabetic()).unwrap();
                let spot = (u16::try_from(col + 1).unwrap(), u16::try_from(y).unwrap());
                s.input(Input::Mouse(wisp_tui::term::Mouse {
                    kind: MouseKind::Press,
                    column: spot.0,
                    row: spot.1,
                    modified: false,
                }));
                s.input(Input::Mouse(wisp_tui::term::Mouse {
                    kind: MouseKind::Release,
                    column: spot.0,
                    row: spot.1,
                    modified: false,
                }));
                s.wait(600).await;
                let page = s.screen.visible();
                assert!(
                    page.last()
                        .is_some_and(|l| l.contains(&wisp_tui::app::pager::back_keys().text())),
                    "{page:#?}"
                );
                assert_hidden(&marker, "the reader", &page.join("\n"));
                s.key(KeyCode::Char('q'));
                s.wait(200).await;
            }
            let last = u16::try_from(main.len() - 1).unwrap();
            for (kind, column, row) in [
                (MouseKind::Press, 0, 0),
                (MouseKind::Drag, look.width - 1, last),
                (MouseKind::Release, look.width - 1, last),
            ] {
                s.input(Input::Mouse(wisp_tui::term::Mouse {
                    kind,
                    column,
                    row,
                    modified: false,
                }));
            }
            s.wait(100).await;
            s.input(Input::Key(Key::ctrl('c')));
            let copies = crate::composer_box::copied(fake, 1).await;
            assert_eq!(copies.len(), 1, "{copies:#?}");
            assert!(copies[0].contains("Typed into process 31"), "{copies:#?}");
            assert_hidden(&marker, "the copy", &copies[0]);
            s.input(Input::Key(Key::ctrl('o')));
            s.wait(300).await;
            let picker = s.screen.visible();
            assert!(
                picker.iter().any(|l| l.contains("Full transcript")),
                "{picker:#?}"
            );
            assert_hidden(&marker, "the picker", &picker.join("\n"));
            s.paste(&marker);
            s.wait(200).await;
            let filtered = s.screen.visible();
            assert!(
                filtered.iter().any(|l| l.contains("nothing matches")),
                "{filtered:#?}"
            );
            s.key(KeyCode::Esc);
            s.wait(200).await;
            s.input(Input::Key(Key::ctrl('o')));
            s.wait(200).await;
            s.input(Input::Key(Key::ctrl('o')));
            s.wait(300).await;
            let mut full = vec![];
            for _ in 0..10 {
                full.extend(s.screen.visible());
                s.key(KeyCode::PageUp);
                s.wait(50).await;
            }
            assert!(
                full.iter().any(|l| l.contains("Typed into process 31")),
                "{full:#?}"
            );
            assert_hidden(&marker, "full detail", &full.join("\n"));
            s.key(KeyCode::Char('['));
            s.wait(100).await;
            let handed: Vec<u8> = s.screen.handed().concat();
            let handed = plain_text(&String::from_utf8_lossy(&handed));
            assert!(handed.contains("Typed into process 31"), "{handed}");
            assert_hidden(&marker, "the hand-off", &handed);
            s.key(KeyCode::Esc);
            s.wait(100).await;
            s.key(KeyCode::Char('q'));
            s.wait(100).await;
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_rewind_before_a_background_call_keeps_its_program_for_later_rows() {
    let look = Look {
        rows: 50,
        ..super::truecolor(
            wisp_render::theme::Mode::Dark,
            wisp_render::theme::glyphs::GlyphSet::Unicode,
            120,
        )
    };
    let shots = staged(look, |s| async move {
        programs(&s).await;
        s.event(ended(
            completed(StopReason::EndTurn, true, Some(cost(0.01, 0.02, false))),
            2_000,
        ))
        .await;
        s.wait(1_000).await;
        s.send("Check the count").await;
        s.event(te::step(1)).await;
        let call = write(
            "count",
            PSQL,
            Some("select 3;"),
            &["Enter"],
            None,
            typed(true, false, false),
        );
        request(&s, 1, 0, &call).await;
        finish(&s, &call).await;
        s.event(ended(
            completed(StopReason::EndTurn, true, Some(cost(0.01, 0.03, false))),
            2_000,
        ))
        .await;
        s.wait(1_000).await;
        s.event(AgentEvent::Rewound {
            answers: None,
            point: RewindPoint::Turn { turn: 2 },
            reason: RewindReason::User,
            dropped: Dropped {
                turns: vec![2],
                messages: 2,
            },
            returned: Vec::new(),
            cache: CacheEffect::Kept,
        })
        .await;
        s.wait(500).await;
        s.send("Check it again").await;
        s.event(te::step(1)).await;
        let again = write(
            "again",
            PSQL,
            Some("select 4;"),
            &["Enter"],
            None,
            typed(true, false, false),
        );
        request(&s, 1, 0, &again).await;
        finish(&s, &again).await;
        s.event(AgentEvent::ProcessEnded {
            id: PSQL,
            end: ProcessEnd::Exited {
                exit: wisp_protocol::ExitStatus::Code { code: 0 },
            },
            bytes: 0,
            lines: 0,
            duration_ms: 9_000,
        })
        .await;
        s.event(ended(
            completed(StopReason::EndTurn, true, Some(cost(0.01, 0.04, false))),
            2_000,
        ))
        .await;
        s.wait(2_000).await.shot("after");
    })
    .await;
    let rows = rows_of(&shots, "after");
    let again = rows
        .iter()
        .find(|r| r.contains("Typed into process 31 and pressed Enter: \"select 4;\""))
        .unwrap_or_else(|| panic!("{rows:#?}"));
    assert_eq!(again.matches("process 31").count(), 1, "{again}");
    assert!(!again.contains("psql"), "{again}");
    assert!(!rows.iter().any(|r| r.contains("select 3;")), "{rows:#?}");
    let psql = rows
        .iter()
        .find(|r| r.contains("psql -U app notes") && r.contains("Ran"))
        .unwrap_or_else(|| panic!("{rows:#?}"));
    assert!(psql.contains("exit 0"), "{psql}");
}

fn picker_entries(rows: &[String]) -> Vec<String> {
    let from = rows
        .iter()
        .position(|r| r.contains("Full transcript"))
        .unwrap_or_else(|| panic!("no picker: {rows:#?}"));
    rows[from..]
        .iter()
        .take_while(|r| {
            (r.starts_with("   ") || r.starts_with(" \u{203A} ")) && !r.contains("enter open")
        })
        .cloned()
        .collect()
}

fn entry_with<'a>(entries: &'a [String], needle: &str) -> &'a String {
    entries
        .iter()
        .find(|r| r.contains(needle))
        .unwrap_or_else(|| panic!("{needle}: {entries:#?}"))
}

#[tokio::test(start_paused = true)]
async fn the_picker_lists_every_call_and_job_as_its_row_and_filters_by_title_command_and_program() {
    let look = Look {
        rows: 44,
        ..super::truecolor(
            wisp_render::theme::Mode::Dark,
            wisp_render::theme::glyphs::GlyphSet::Unicode,
            100,
        )
    };
    staged(look, |s| async move {
        programs(&s).await;
        s.event(AgentEvent::ProcessRestored {
            id: PSQL,
            call_id: Some("psql".to_owned()),
            target: "psql -U app notes".to_owned(),
            tty: true,
            end: Some(ProcessEnd::Exited {
                exit: wisp_protocol::ExitStatus::Code { code: 0 },
            }),
            duration_ms: 1_000,
            keeps_running: false,
            unchecked: false,
            pid: None,
            log: None,
        })
        .await;
        s.event(te::step(2)).await;
        let echoed = write(
            "echoed",
            DEV,
            Some("rs"),
            &["Enter"],
            None,
            typed(true, false, false),
        );
        request(&s, 2, 0, &echoed).await;
        finish(&s, &echoed).await;
        s.event(te::block(1)).await;
        s.event(te::requested(2, 1, "fail", "cargo build --release"))
            .await;
        s.event(te::started("fail")).await;
        s.event(te::finished("fail", te::ok(101), "", 0, 3_000))
            .await;
        s.event(te::block(2)).await;
        s.event(te::requested(2, 2, "run", "cargo test --workspace"))
            .await;
        s.event(te::started("run")).await;
        s.wait(2_000).await;
        s.input(Input::Key(Key::ctrl('o')));
        s.wait(300).await;
        let entries = picker_entries(&s.screen.visible());
        assert_eq!(entries.len(), 6, "{entries:#?}");
        let running = entry_with(&entries, "Running cargo test --workspace").clone();
        assert!(running.ends_with(" 2s"), "{running}");
        assert!(running.starts_with(" \u{203A} "), "{entries:#?}");
        assert!(
            entry_with(&entries, "Ran cargo build --release").ends_with("exit 101 \u{b7} 3s"),
            "{entries:#?}"
        );
        assert!(
            entry_with(
                &entries,
                &format!("Typed into process 27 and pressed Enter: \"rs\"  {START_DEV}")
            )
            .contains("1s"),
            "{entries:#?}"
        );
        assert!(
            entry_with(&entries, &format!("{START_DEV}  npm run dev"))
                .ends_with("process 27 \u{b7} running"),
            "{entries:#?}"
        );
        assert!(
            entry_with(&entries, "Ran psql -U app notes")
                .ends_with("process 31 \u{b7} exit 0 \u{b7} before the restart"),
            "{entries:#?}"
        );
        for row in &entries[1..] {
            assert_eq!(row.chars().count(), usize::from(look.width), "{row:?}");
        }
        s.wait(2_000).await;
        let ticked = picker_entries(&s.screen.visible());
        assert!(
            entry_with(&ticked, "Running cargo test --workspace").ends_with(" 4s"),
            "{ticked:#?}"
        );
        for (filter, shown) in [
            (
                "npm run dev",
                vec!["Start the dev server  npm run dev", "into process 27"],
            ),
            ("--release", vec!["Ran cargo build --release"]),
            ("before the restart", vec!["Ran psql -U app notes"]),
            ("failed", vec!["Ran cargo build --release"]),
        ] {
            s.paste(filter);
            s.wait(200).await;
            let rows = s.screen.visible();
            let found: Vec<&String> = rows
                .iter()
                .filter(|r| {
                    r.contains(" \u{2713} ")
                        || r.contains(" \u{21D7} ")
                        || r.contains(" \u{2A2F} ")
                        || r.contains(" \u{25E6} ")
                })
                .filter(|r| r.starts_with("   ") || r.starts_with(" \u{203A} "))
                .collect();
            assert_eq!(found.len(), shown.len(), "{filter}: {rows:#?}");
            for needle in shown {
                assert!(
                    found.iter().any(|r| r.contains(needle)),
                    "{filter}: {rows:#?}"
                );
            }
            for _ in 0..filter.chars().count() {
                s.key(KeyCode::Backspace);
            }
            s.wait(200).await;
        }
        s.paste("--release");
        s.wait(200).await;
        s.key(KeyCode::Enter);
        s.wait(300).await;
        let page = s.screen.visible();
        assert!(page[0].contains("Ran cargo build --release"), "{page:#?}");
        assert!(
            page.last()
                .is_some_and(|l| l.contains(&wisp_tui::app::pager::back_keys().text())),
            "{page:#?}"
        );
        s.key(KeyCode::Char('q'));
        s.wait(200).await;
        s.input(Input::Key(Key::ctrl('o')));
        s.wait(300).await;
        let rows = s.screen.visible();
        let y = rows
            .iter()
            .position(|r| r.contains("Ran psql -U app notes") && r.starts_with("   "))
            .unwrap_or_else(|| panic!("{rows:#?}"));
        let row = u16::try_from(y).unwrap();
        for kind in [MouseKind::Press, MouseKind::Release] {
            s.input(Input::Mouse(wisp_tui::term::Mouse {
                kind,
                column: 20,
                row,
                modified: false,
            }));
        }
        s.wait(200).await;
        let page = s.screen.visible();
        assert!(page[0].contains("Ran psql -U app notes"), "{page:#?}");
        assert!(
            page.iter().any(|l| l.contains("process 31 \u{b7} exit 0")),
            "{page:#?}"
        );
        s.key(KeyCode::Char('q'));
        s.wait(100).await;
    })
    .await;
}
