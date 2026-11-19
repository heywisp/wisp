use wisp_core::endpoints::ProviderKind;
use wisp_tui::app::endpoint_card::{AuthEntry, CardSetup, ProviderEntry};

use crate::endpoints::Sources;
use crate::manage::{Context, credentials_shown};
use crate::presence::Presence;

pub(crate) fn catalogue() -> Vec<ProviderEntry> {
    ProviderKind::ALL
        .iter()
        .map(|provider| ProviderEntry {
            name: provider.as_str().to_owned(),
            title: provider.title().to_owned(),
            default_url: provider.default_url().to_owned(),
            auths: provider
                .auths()
                .iter()
                .filter(|auth| !auth.requires_oauth())
                .map(|auth| AuthEntry {
                    name: auth.as_str().to_owned(),
                    words: provider.auth_words(*auth).to_owned(),
                })
                .collect(),
            official_auth: provider.official_auth().as_str().to_owned(),
        })
        .collect()
}

pub(crate) fn setup(sources: &Sources, ctx: &Context<'_>) -> CardSetup {
    let config = sources.config.as_ref().ok().and_then(Option::as_ref);
    let presence = if sources.inputs.available() {
        Presence::scan(&sources.inputs, config).found(config)
    } else {
        Vec::new()
    };
    CardSetup {
        catalogue: catalogue(),
        presence,
        credentials_shown: credentials_shown(ctx),
        keys_file: sources.file_words(),
        lead: None,
    }
}

#[cfg(test)]
pub(crate) fn test_context<'a>(
    root: &std::path::Path,
    config: &'a std::path::Path,
    keychain: &'a crate::keychain::Keychain,
) -> Context<'a> {
    Context {
        set: &[],
        env_file: None,
        config: Some(config),
        model: "claude-sonnet-5-5",
        caps: crate::prompt_caps(&wisp_tui::term::SavedDisplay::default()),
        claude_usage: true,
        xdg_config_home: None,
        home: Some(root.to_owned()),
        keychain,
        models: None,
    }
}

#[cfg(test)]
mod tests;
