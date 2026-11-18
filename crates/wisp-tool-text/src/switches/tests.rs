use super::*;

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|n| (*n).to_owned()).collect()
}

#[test]
fn a_note_reads_back_as_the_switches_it_was_made_from() {
    for (off, on) in [
        (names(&["show_diff"]), Vec::new()),
        (Vec::new(), names(&["view_file"])),
        (
            names(&["bash", "process"]),
            names(&["show_diff", "view_file"]),
        ),
    ] {
        let switched = Switched { off, on };
        let text = note(&switched).unwrap();
        assert_eq!(parse(&text), Some(switched));
    }
}

#[test]
fn the_note_names_each_direction_in_one_line() {
    let switched = Switched {
        off: names(&["bash", "process"]),
        on: names(&["show_diff"]),
    };
    assert_eq!(
        note(&switched).as_deref(),
        Some("Tools turned off: bash, process.\nTools turned on: show_diff.\n")
    );
    assert_eq!(note(&Switched::default()), None);
}

#[test]
fn other_text_is_no_note() {
    for text in [
        "",
        "hello",
        "Tools turned off: .",
        "Tools turned off: a b.",
        "Tools turned off: show_diff",
        "Tools turned off: show_diff.\nand more",
    ] {
        assert_eq!(parse(text), None, "{text:?}");
    }
}

#[test]
fn a_base_block_holds_the_whole_base_after_its_framing() {
    let block = base_block("You are Wisp.");
    assert!(is_base_block(&block));
    assert!(block.ends_with("\n\nYou are Wisp."));
    assert!(!is_base_block("You are Wisp."));
    assert_eq!(parse(&block), None);
}
