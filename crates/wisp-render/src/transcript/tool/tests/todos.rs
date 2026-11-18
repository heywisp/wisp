use wisp_protocol::{TodoItem, TodoStatus};

use super::*;
use crate::events::{Requested, tool_requested};
use crate::styled::Look;
use crate::theme::Token;

fn item(text: &str, status: TodoStatus) -> TodoItem {
    TodoItem {
        text: text.to_owned(),
        status,
        section: None,
    }
}

fn list() -> Vec<TodoItem> {
    vec![
        item("Read the plan", TodoStatus::Completed),
        item(
            "Write the parser and its validation table",
            TodoStatus::InProgress,
        ),
        item("Draw the list in the tool row", TodoStatus::Pending),
        item("Keep the old renderer", TodoStatus::Cancelled),
        item("Run the checks", TodoStatus::Completed),
    ]
}

fn under(section: &str, mut item: TodoItem) -> TodoItem {
    item.section = Some(section.to_owned());
    item
}

fn sections() -> Vec<TodoItem> {
    let items = list();
    let next = "Things to check before the merge request opens";
    vec![
        items[2].clone(),
        under("Done", items[0].clone()),
        under("Done", items[3].clone()),
        under(next, items[1].clone()),
        under(next, items[4].clone()),
    ]
}

fn asked(id: &str, items: &[TodoItem]) -> AgentEvent {
    let todos: Vec<serde_json::Value> = items
        .iter()
        .map(|i| json!({"text": i.text, "status": i.status.as_str()}))
        .collect();
    let completed = items
        .iter()
        .filter(|i| i.status == TodoStatus::Completed)
        .count();
    tool_requested(Requested {
        call_id: id.to_owned(),
        tool: "write_todos".to_owned(),
        input: json!({ "todos": todos }),
        action: ToolAction::Other {
            name: "write_todos".to_owned(),
        },
        target: format!("{completed} of {} completed", items.len()),
        output: OutputKind::None,
        ..Requested::default()
    })
}

fn done(id: &str) -> AgentEvent {
    let mut event = finished(
        id,
        ToolOutcome::Completed {
            is_error: false,
            exit: None,
        },
        ToolOutputView {
            inlined: true,
            ..ToolOutputView::default()
        },
        3,
    );
    if let AgentEvent::ToolCallFinished { result, .. } = &mut event {
        *result = Some("Todos updated: 2 of 5 completed.".to_owned());
    }
    event
}

fn updated(id: Option<&str>, items: Vec<TodoItem>) -> AgentEvent {
    AgentEvent::TodosUpdated {
        call_id: id.map(str::to_owned),
        items,
    }
}

fn checklist(items: Vec<TodoItem>) -> Transcript {
    let mut t = transcript();
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            asked("t1", &items),
            started("t1"),
            done("t1"),
            updated(Some("t1"), items),
            ended(completed(StopReason::EndTurn, 2, 1)),
        ],
    );
    t
}

#[test]
fn the_tool_row_draws_the_list_under_its_title() {
    let mut t = checklist(list());
    let unicode = text(&t.rows(&r(80), Detail::Scrollback));
    insta::assert_snapshot!("todo_rows_80", unicode);
    let narrow = text(&t.rows(&r(40), Detail::Scrollback));
    insta::assert_snapshot!("todo_rows_40", narrow);
    let ascii = Renderer::new(Layout::new(80, GlyphSet::Ascii));
    let plain = text(&t.rows(&ascii, Detail::Scrollback));
    assert!(plain.is_ascii(), "{plain}");
    insta::assert_snapshot!("todo_rows_ascii", plain);
    let mut t = checklist(sections());
    insta::assert_snapshot!(
        "todo_sections_80",
        text(&t.rows(&r(80), Detail::Scrollback))
    );
    insta::assert_snapshot!(
        "todo_sections_40",
        text(&t.rows(&r(40), Detail::Scrollback))
    );
    let plain = text(&t.rows(&ascii, Detail::Scrollback));
    assert!(plain.is_ascii(), "{plain}");
    insta::assert_snapshot!("todo_sections_ascii", plain);
}

#[test]
fn every_full_detail_view_draws_the_list() {
    let mut t = checklist(list());
    let full = text(&t.rows(&r(80), Detail::Full));
    insta::assert_snapshot!("todo_rows_full", full);
    for item in list() {
        assert!(full.contains(&item.text), "{full}");
    }
    let cell = t
        .cells()
        .iter()
        .position(|c| c.tool_state().is_some())
        .unwrap();
    let page = text(&t.cell_page(cell, &r(80)).unwrap().body);
    for item in list() {
        assert!(page.contains(&item.text), "{page}");
    }
    assert!(page.contains("\u{25CC} Write the parser"), "{page}");
    let mut t = checklist(sections());
    insta::assert_snapshot!("todo_sections_full", text(&t.rows(&r(80), Detail::Full)));
}

#[test]
fn each_status_has_its_own_look() {
    let mut t = checklist(sections());
    let rows = t.rows(&r(80), Detail::Scrollback);
    let look_of = |needle: &str| {
        rows.iter()
            .flat_map(|l| l.spans.iter())
            .find(|s| s.text.contains(needle))
            .map(|s| s.look)
            .unwrap_or_else(|| panic!("{needle}: {}", text(&rows)))
    };
    let working = look_of("Write the parser");
    assert!(working.bold, "{working:?}");
    let dropped = look_of("Keep the old renderer");
    assert!(dropped.strike, "{dropped:?}");
    assert_eq!(dropped.fg, look_of("Read the plan").fg);
    let pending = look_of("Draw the list");
    assert!(!pending.bold && !pending.strike, "{pending:?}");
    assert_eq!(look_of("Done"), Look::fg(Token::TextMuted).bold());
    let drawn = text(&rows);
    assert_eq!(drawn.matches("Done").count(), 1, "{drawn}");
    assert!(
        drawn.find("Draw the list").unwrap() < drawn.find("Done").unwrap(),
        "{drawn}"
    );
}

#[test]
fn an_update_without_a_call_and_one_for_an_unknown_call_draw_nothing() {
    let mut t = checklist(list());
    let before = text(&t.rows(&r(80), Detail::Scrollback));
    t.apply(&updated(None, Vec::new()), ms(100));
    t.apply(&updated(Some("elsewhere"), list()), ms(110));
    assert_eq!(text(&t.rows(&r(80), Detail::Scrollback)), before);
}

#[test]
fn a_later_call_draws_its_own_list_and_the_earlier_row_keeps_its_own() {
    let mut t = checklist(list());
    let before = t.cells().len();
    t.apply(&AgentEvent::turn_started(2), ms(200));
    t.apply(&asked("t2", &list()[..1]), ms(210));
    t.apply(&done("t2"), ms(220));
    t.apply(&updated(Some("t2"), list()[..1].to_vec()), ms(230));
    let rows = text(&t.rows(&r(80), Detail::Scrollback));
    assert!(t.cells().len() > before, "{rows}");
    assert_eq!(rows.matches("Read the plan").count(), 2, "{rows}");
    assert_eq!(rows.matches("Run the checks").count(), 1, "{rows}");
}
