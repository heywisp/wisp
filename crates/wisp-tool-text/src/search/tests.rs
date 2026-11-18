use super::*;

#[test]
fn shipped_descriptions_round_trip_as_json_without_debug_escape_changes() {
    for query in [QUERY.to_owned(), call_query()] {
        let schema = schema(&query);
        let parsed: serde_json::Value = serde_json::from_str(&schema).unwrap();
        let encoded = schema
            .split_once("\"description\":")
            .unwrap()
            .1
            .strip_suffix("}},\"required\":[\"query\"]}")
            .unwrap();
        let decoded: String = serde_json::from_str(encoded).unwrap();
        assert_eq!(decoded, query);
        assert_eq!(serde_json::to_string(&decoded).unwrap(), encoded);
        assert_eq!(parsed["properties"]["query"]["description"], decoded);
    }
}
