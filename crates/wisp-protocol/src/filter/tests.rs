use super::Query;

#[test]
fn every_word_matches_anywhere_in_any_text_and_found_merges_its_hits() {
    let query = Query::new("  Usage NARROW ");
    assert!(!query.is_empty());
    assert!(query.matches(&["narrow", "page usage"]));
    assert!(query.matches(&["Narrow usage page"]));
    assert!(!query.matches(&["narrow", "page"]));
    assert!(!query.matches(&[]));
    let blank = Query::new("   ");
    assert!(blank.is_empty());
    assert!(blank.matches(&[]));
    assert!(blank.found("Narrow usage page").is_empty());
    let text = "Narrow usage page";
    let found = |query: &str| -> Vec<(usize, usize)> {
        let hits = Query::new(query).found(text);
        hits.into_iter().map(|hit| (hit.start, hit.end)).collect()
    };
    assert_eq!(found("page usage"), [(7, 12), (13, 17)]);
    assert_eq!(found("us usage"), [(7, 12)]);
    assert_eq!(found("usage page"), [(7, 12), (13, 17)]);
    assert_eq!(found("row usa"), [(3, 6), (7, 10)]);
    assert_eq!(found("narrow usage"), [(0, 6), (7, 12)]);
    assert_eq!(found("narro w"), [(0, 6)]);
    assert_eq!(found("ow usag e"), [(4, 6), (7, 12)]);
    assert_eq!(found("missing"), []);
    assert!(Query::new("i").found("İstanbul is").is_empty());
    assert!(Query::new("i").matches(&["İstanbul"]));
}
