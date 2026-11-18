#[path = "../../../tests/support/dependencies.rs"]
mod dependencies;

use dependencies::{built_without_defaults, depends_only_on};
use serde_json::json;

const ALLOWED: [&str; 3] = ["futures", "serde", "toml"];

#[test]
fn wisp_plugin_api_depends_only_on_futures_serde_and_toml() {
    depends_only_on(&ALLOWED, &[]);
}

#[test]
fn futures_brings_no_executor() {
    built_without_defaults("futures", &["std"]);
}

#[test]
fn serde_json_keeps_keys_sorted_so_preserve_order_is_off() {
    let value = json!({"b": 1, "a": 2});
    assert_eq!(serde_json::to_string(&value).unwrap(), r#"{"a":2,"b":1}"#);
}
