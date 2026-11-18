#[path = "../../../tests/support/dependencies.rs"]
mod dependencies;

#[test]
fn wisp_tool_text_depends_only_on_the_plugin_api() {
    dependencies::depends_only_on(&["wisp-plugin-api"], &["wisp-plugin-api"]);
}
