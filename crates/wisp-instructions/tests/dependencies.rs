#[path = "../../../tests/support/dependencies.rs"]
mod dependencies;

use dependencies::{built_without_defaults, depends_only_on};

const ALLOWED: [&str; 5] = [
    "hmac-sha256",
    "pulldown-cmark",
    "serde_json",
    "wisp-plugin-api",
    "wisp-tool-text",
];

const WORKSPACE: [&str; 2] = ["wisp-plugin-api", "wisp-tool-text"];

#[test]
fn wisp_instructions_depends_only_on_its_allowed_crates() {
    depends_only_on(&ALLOWED, &WORKSPACE);
}

#[test]
fn the_markdown_and_hash_crates_are_built_without_default_features() {
    for (name, req) in [("pulldown-cmark", "^0.13.5"), ("hmac-sha256", "^1.1.15")] {
        assert_eq!(built_without_defaults(name, &[])["req"], req, "{name}");
    }
}
