use std::sync::Arc;

use wisp_plugin_api::{Bundled, Plugin};

pub const MANIFEST: &str = include_str!("../wisp-plugin.toml");

pub fn bundled() -> Bundled {
    Bundled {
        manifest: MANIFEST,
        instantiate: |_| Arc::new(Anthropic),
    }
}

struct Anthropic;

impl Plugin for Anthropic {}
