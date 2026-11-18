use super::*;

fn scan(source: &str) -> Scan {
    let mut s = Scan::default();
    s.advance(source, source.len());
    s
}

#[test]
fn a_fence_in_a_list_item_cuts_with_its_containers() {
    let source = "1. Create:\n\n   ```rust\n   let a = 1;\n";
    let s = scan(source);
    let cut = s.gate_cut(source.len()).map(|c| c.head);
    assert_eq!(
        cut,
        Some(format!("1. {ANCHOR}\n\n   ```rust\n   {ANCHOR}\n"))
    );
    let kinds: Vec<Kind> = s.lines.iter().map(|l| l.kind).collect();
    assert_eq!(
        kinds,
        [Kind::Para, Kind::Blank, Kind::FenceOpen, Kind::Code]
    );
}

#[test]
fn quotes_lists_and_tables_keep_their_context() {
    let s = scan("> - a\n>   b\n");
    assert_eq!(
        s.gate_cut(0).map(|c| c.head),
        Some(format!("> - {ANCHOR}\n>\n"))
    );
    let s = scan("| a | b |\n|---|---|\n| 1 | 2 |\n");
    assert_eq!(
        s.gate_cut(0).map(|c| c.head),
        Some("| a | b |\n|---|---|\n".to_owned())
    );
    let s = scan("- a\nlazy\n");
    assert_eq!(s.lines.get(1).map(|l| l.kind), Some(Kind::Lazy));
}

#[test]
fn an_html_block_cut_replays_its_opener() {
    let s = scan("- item\n\n  <pre>\n  one\n");
    assert_eq!(
        s.gate_cut(0).map(|c| c.head),
        Some(format!("- {ANCHOR}\n\n  <pre>\n  {ANCHOR}\n"))
    );
    let kinds: Vec<Kind> = s.lines.iter().map(|l| l.kind).collect();
    assert_eq!(kinds, [Kind::Para, Kind::Blank, Kind::Other, Kind::Html]);
    let s = scan("<div>\n- not an item\n\n- item\n");
    assert_eq!(
        s.gate_cut(0).map(|c| c.head),
        Some(format!("- {ANCHOR}\n\n"))
    );
}

#[test]
fn a_capped_line_stops_all_cuts() {
    let deep = format!("{}deep\n", "> ".repeat(66));
    let source = format!("- a\n  b\n{deep}\n- c\n");
    let s = scan(&source);
    assert_eq!(s.gate_cut(source.len()), None);
    let cuts = s.cuts(&source, &[], &[""], true, source.len());
    assert!(cuts.iter().all(|c| c.at < 8), "{cuts:?}");
}

#[test]
fn a_tab_indented_closer_ends_the_fence() {
    let source = "> ```rust\n> /* x\n>\t```\n";
    let s = scan(source);
    let kinds: Vec<Kind> = s.lines.iter().map(|l| l.kind).collect();
    assert_eq!(kinds, [Kind::FenceOpen, Kind::Code, Kind::FenceClose]);
    let cut = s.gate_cut(source.len());
    assert_eq!(cut.as_ref().and_then(|c| c.fence), None);
    assert_eq!(cut.map(|c| c.head), Some(format!("> {ANCHOR}\n>\n")));
    let next = format!("{source}> after\n");
    let s = scan(&next);
    let cuts = s.cuts(&next, &[], &[""], true, next.len());
    let after: Vec<&Cut> = cuts.iter().filter(|c| c.at >= source.len()).collect();
    assert!(!after.is_empty());
    assert!(after.iter().all(|c| !c.head.contains("```")), "{after:?}");
}

#[test]
fn tabs_count_to_the_next_stop_wherever_indentation_is_measured() {
    let s = scan("-\tone\n\t```\n");
    let kinds: Vec<Kind> = s.lines.iter().map(|l| l.kind).collect();
    assert_eq!(kinds, [Kind::Para, Kind::FenceOpen]);
    let s = scan(">\t ```\n> a\n>\t ```\n");
    let kinds: Vec<Kind> = s.lines.iter().map(|l| l.kind).collect();
    assert_eq!(kinds, [Kind::FenceOpen, Kind::Code, Kind::FenceClose]);
    let s = scan("\t```\n");
    assert_eq!(s.lines.first().map(|l| l.kind), Some(Kind::Other));
}

#[test]
fn a_code_cut_claims_the_lines_of_its_fence_before_it() {
    let source = "```rust\na\nb\n```\n\n```rust\nc\nd\n";
    let s = scan(source);
    let claims: Vec<Option<usize>> = s
        .cuts(source, &[], &[""], true, source.len())
        .into_iter()
        .filter(|c| c.head.contains("```"))
        .map(|c| c.fence)
        .collect();
    assert_eq!(claims, [Some(0), Some(1), Some(0), Some(1)]);
    assert_eq!(s.gate_cut(source.len()).and_then(|c| c.fence), Some(2));
}
