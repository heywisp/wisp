#[path = "../../../tests/support/dependencies.rs"]
mod dependencies;

#[test]
fn wisp_index_depends_only_on_json_and_the_plugin_api() {
    dependencies::depends_only_on(&["serde_json", "wisp-plugin-api"], &["wisp-plugin-api"]);
}
