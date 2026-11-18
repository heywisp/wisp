use super::*;

fn todo(text: &str, status: TodoStatus) -> Todo {
    Todo {
        text: text.to_owned(),
        status,
        section: None,
    }
}

fn under(section: &str, text: &str) -> Todo {
    Todo {
        section: Some(section.to_owned()),
        ..todo(text, TodoStatus::Pending)
    }
}

#[test]
fn the_result_line_counts_completed_items_and_names_the_one_in_progress() {
    let items = [
        todo("Read the plan", TodoStatus::Completed),
        todo("Write the plan", TodoStatus::InProgress),
        todo("Review it", TodoStatus::Pending),
        todo("Drop the old one", TodoStatus::Cancelled),
        todo("Merge", TodoStatus::Completed),
    ];
    assert_eq!(
        updated(&items),
        "Todos updated: 2 of 5 completed. In progress: \"Write the plan\"."
    );
    assert_eq!(target(&items), "2 of 5 completed");
    assert_eq!(updated(&items[2..3]), "Todos updated: 0 of 1 completed.");
}

#[test]
fn the_rules_refuse_each_bad_list() {
    let pending = |text: &str| todo(text, TodoStatus::Pending);
    let many: Vec<Todo> = (0..=MAX_ITEMS)
        .map(|i| pending(&format!("step {i}")))
        .collect();
    let long = "x".repeat(MAX_TEXT_CHARS + 1);
    let cases: Vec<(Vec<Todo>, Result<(), String>)> =
        vec![
        (vec![pending("one")], Ok(())),
        (vec![under("   ", "one")], Ok(())),
        (vec![under(&"é".repeat(MAX_SECTION_CHARS), "one")], Ok(())),
        (vec![pending(&format!("  {}  ", "é".repeat(MAX_TEXT_CHARS)))], Ok(())),
        (
            vec![pending("one"), under(&"é".repeat(MAX_SECTION_CHARS + 1), "two")],
            Err("item 2's `section` is 61 characters; the limit is 60".to_owned()),
        ),
        (
            vec![under("Next\nLater", "one")],
            Err("item 1's `section` must be one line with no control characters".to_owned()),
        ),
        (
            vec![under("Next\tLater", "one")],
            Err("item 1's `section` must be one line with no control characters".to_owned()),
        ),
        (vec![under("Next\n", "one")], Ok(())),
        (many[..MAX_ITEMS].to_vec(), Ok(())),
        (vec![pending(&"é".repeat(MAX_TEXT_CHARS))], Ok(())),
        (Vec::new(), Err("`todos` has 0 items; give 1 to 50".to_owned())),
        (many, Err("`todos` has 51 items; give 1 to 50".to_owned())),
        (
            vec![pending("one"), pending("  ")],
            Err("item 2's `text` is blank".to_owned()),
        ),
        (
            vec![pending(&long)],
            Err("item 1's `text` is 501 characters; the limit is 500".to_owned()),
        ),
        (
            vec![
                todo("a", TodoStatus::Completed),
                todo("b", TodoStatus::InProgress),
                todo("c", TodoStatus::Pending),
                todo("d", TodoStatus::InProgress),
            ],
            Err(
                "items 2 and 4 are both in_progress; at most one item may be in_progress at a time"
                    .to_owned(),
            ),
        ),
    ];
    for (items, expected) in cases {
        assert_eq!(check(&items), expected, "{items:?}");
    }
}

#[test]
fn every_status_has_its_wire_name() {
    for (name, status) in STATUSES {
        assert_eq!(status_named(name), Some(status));
    }
    assert_eq!(status_named("blocked"), None);
    assert_eq!(bad_status(), "pending, in_progress, completed or cancelled");
}

#[test]
fn the_normal_form_trims_and_gathers_under_the_first_heading() {
    let plain = vec![
        todo("first", TodoStatus::Completed),
        todo("second", TodoStatus::InProgress),
    ];
    assert_eq!(normal(plain.clone()), plain);
    let items = vec![
        under(" Next ", " first "),
        under("Done", "second"),
        todo(" plain ", TodoStatus::Cancelled),
        under("Next", "third"),
        under("  ", " blank "),
        under("next", "fourth"),
        under("Done", "fifth"),
    ];
    let expected = vec![
        todo("plain", TodoStatus::Cancelled),
        todo("blank", TodoStatus::Pending),
        under("Next", "first"),
        under("Next", "third"),
        under("Done", "second"),
        under("Done", "fifth"),
        under("next", "fourth"),
    ];
    assert_eq!(normal(items), expected);
    assert_eq!(normal(expected.clone()), expected);
}
