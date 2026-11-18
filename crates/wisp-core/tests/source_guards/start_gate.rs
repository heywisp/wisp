use crate::purity::{non_test_code, sources};
use crate::tokens::repo;

#[test]
fn only_the_start_tools_module_turns_bash_on() {
    let crates = repo().join("crates");
    let gate = crates.join("wisp-start").join("src").join("tools.rs");
    let gate_text = std::fs::read_to_string(&gate).unwrap();
    assert!(gate_text.contains("let sources = wisp_plugins::bundled()"));
    assert!(gate_text.contains("Registry::from_origins(sources, &config)"));
    assert!(gate_text.contains("names(requested, &registry)"));
    assert!(gate_text.contains("None => registry.default_on()"));
    let mut files = Vec::new();
    for client in ["wisp-cli", "wisp-start", "wispd", "wisp-tui", "wisp-render"] {
        files.extend(sources(&crates.join(client).join("src"), "rs", &[], 1));
    }
    assert!(files.contains(&gate));
    files.retain(|f| *f != gate);
    assert!(!gate_text.contains("BashTool"));
    assert!(!gate_text.contains("AskUserTool"));
    assert!(!gate_text.contains("ask_user: true"));
    for file in files {
        let text = non_test_code(&file);
        for needle in [
            "Registry::new(",
            "Registry::from_sources(",
            "wisp_plugins::bundled(",
            "bash: true",
            "ask_user: true",
            "BashTool",
            "AskUserTool",
        ] {
            assert!(!text.contains(needle), "{} uses {needle}", file.display());
        }
    }
}
