use wisp_plugin_api::Bundled;

#[allow(
    clippy::vec_init_then_push,
    reason = "each push is behind its plugin's feature"
)]
pub fn bundled() -> Vec<Bundled> {
    let mut all = Vec::new();
    #[cfg(feature = "openai")]
    all.push(wisp_plugin_openai::bundled());
    #[cfg(feature = "gemini")]
    all.push(wisp_plugin_gemini::bundled());
    #[cfg(feature = "anthropic")]
    all.push(wisp_plugin_anthropic::bundled());
    #[cfg(feature = "agent-viz")]
    all.push(wisp_plugin_agent_viz::bundled());
    #[cfg(feature = "agents")]
    all.push(wisp_plugin_agents::bundled());
    #[cfg(feature = "ask-user")]
    all.push(wisp_plugin_ask_user::bundled());
    #[cfg(feature = "chat")]
    all.push(wisp_plugin_chat::bundled());
    #[cfg(feature = "compaction")]
    all.push(wisp_plugin_compaction::bundled());
    #[cfg(feature = "context")]
    all.push(wisp_plugin_context::bundled());
    #[cfg(feature = "idle")]
    all.push(wisp_plugin_idle::bundled());
    #[cfg(feature = "instructions")]
    all.push(wisp_plugin_instructions::bundled());
    #[cfg(feature = "mcp")]
    all.push(wisp_plugin_mcp::bundled());
    #[cfg(feature = "memory")]
    all.push(wisp_plugin_memory::bundled());
    #[cfg(feature = "menus")]
    all.push(wisp_plugin_menus::bundled());
    #[cfg(feature = "page")]
    all.push(wisp_plugin_page::bundled());
    #[cfg(feature = "search")]
    all.push(wisp_plugin_search::bundled());
    #[cfg(feature = "secret")]
    all.push(wisp_plugin_secret::bundled());
    #[cfg(feature = "self-check")]
    all.push(wisp_plugin_self_check::bundled());
    #[cfg(feature = "shell")]
    all.push(wisp_plugin_shell::bundled());
    #[cfg(feature = "show-diff")]
    all.push(wisp_plugin_show_diff::bundled());
    #[cfg(feature = "skills")]
    all.push(wisp_plugin_skills::bundled());
    #[cfg(feature = "time")]
    all.push(wisp_plugin_time::bundled());
    #[cfg(feature = "todos")]
    all.push(wisp_plugin_todos::bundled());
    #[cfg(feature = "view-file")]
    all.push(wisp_plugin_view_file::bundled());
    #[cfg(feature = "web-search")]
    all.push(wisp_plugin_web_search::bundled());
    #[cfg(feature = "workspace")]
    all.push(wisp_plugin_workspace::bundled());
    #[cfg(feature = "testkit")]
    all.extend(testkit(std::env::var("WISP_TESTKIT").ok().as_deref()));
    all
}

#[cfg(feature = "testkit")]
fn testkit(mode: Option<&str>) -> Option<Bundled> {
    match mode? {
        "inert" => Some(wisp_plugin_testkit::bundled()),
        "block-input" => Some(wisp_plugin_testkit::block_input()),
        "sidebar" => Some(wisp_plugin_testkit::sidebar()),
        "sections" => Some(wisp_plugin_testkit::sections()),
        "settings" => Some(wisp_plugin_testkit::settings()),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
