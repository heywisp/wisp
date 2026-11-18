use super::*;

#[test]
fn sizes_parse_from_columns_by_rows_within_the_limits() {
    assert_eq!(
        parse_size("120x40"),
        Some(Size {
            cols: 120,
            rows: 40
        })
    );
    assert_eq!(parse_size(" 80X24 "), Some(Size { cols: 80, rows: 24 }));
    assert_eq!(parse_size("80"), None);
    assert_eq!(parse_size("ax24"), None);
    assert!(size_allowed(Size { cols: 20, rows: 5 }));
    assert!(size_allowed(Size {
        cols: 500,
        rows: 200
    }));
    assert!(!size_allowed(Size { cols: 19, rows: 5 }));
    assert!(!size_allowed(Size {
        cols: 20,
        rows: 201
    }));
    assert_eq!(size_text(DEFAULT_SIZE), "100x30");
}

#[test]
fn key_names_list_every_named_key_then_the_ranges() {
    assert_eq!(
        key_names(),
        "Enter, Tab, Space, Esc, Backspace, Delete, Up, Down, Left, Right, Home, End, PageUp, PageDown, F1 to F12, C-a to C-z, EOF"
    );
}
