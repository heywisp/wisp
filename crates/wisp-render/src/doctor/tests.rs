use super::*;

fn text(text: &str) -> DoctorCell {
    DoctorCell::Text { text: text.into() }
}

fn row(key: &str, cells: Vec<DoctorCell>) -> DoctorRow {
    DoctorRow {
        key: key.into(),
        cells,
    }
}

#[test]
fn columns_pad_to_their_widest_cell_and_sizes_align_right() {
    let rows = [
        row(
            "project",
            vec![
                text("AGENTS.md"),
                DoctorCell::Bytes { bytes: Some(2048) },
                text("loads"),
            ],
        ),
        row(
            "import",
            vec![
                text("docs/rules.md"),
                DoctorCell::Bytes {
                    bytes: Some(2_000_000),
                },
                text("from AGENTS.md"),
            ],
        ),
        row(
            "default",
            vec![text("~/.config/wisp/x.md"), text(""), text("missing")],
        ),
        row("total", vec![text("3.0 KiB, about 761 tokens")]),
        row("empty", vec![]),
    ];
    assert_eq!(
        table(&rows, 9),
        [
            "project   AGENTS.md                2.0 KiB   loads",
            "import    docs/rules.md         1953.1 KiB   from AGENTS.md",
            "default   ~/.config/wisp/x.md                missing",
            "total     3.0 KiB, about 761 tokens",
            "empty",
        ]
    );
}

#[test]
fn a_size_takes_at_least_nine_columns_and_a_last_cell_is_never_padded() {
    let rows = [
        row("a", vec![DoctorCell::Bytes { bytes: Some(0) }, text("x")]),
        row("b", vec![text("longer than the size")]),
    ];
    assert_eq!(
        table(&rows, 1),
        ["a   0.0 KiB   x", "b longer than the size"]
    );
    assert_eq!(kib(1536), "1.5 KiB");
}
