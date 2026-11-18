use wisp_core::agents::endpoints::summary_inputs;
use wisp_core::endpoints::ProviderKind;
use wisp_protocol::settings::{CredentialKind, EndpointSummary};
use wisp_render::chrome::account::AccountKind;
use wisp_tui::app::endpoint_card::endpoint_facts_of;

use crate::endpoints::auth_credential;

#[test]
fn the_model_and_account_classify_every_summary_shape_alike() {
    let mut shapes = 0;
    for provider in ProviderKind::ALL {
        let mut credentials: Vec<Option<CredentialKind>> = provider
            .auths()
            .iter()
            .map(|auth| Some(auth_credential(*auth).wire()))
            .collect();
        credentials.extend([None, Some(CredentialKind::Other("unknown".into()))]);
        for credential in credentials {
            for official in [Some(true), Some(false), None] {
                let summary = EndpointSummary {
                    name: "work".into(),
                    provider: provider.as_str().into(),
                    credential: credential.clone(),
                    official,
                    ..EndpointSummary::default()
                };
                let model = summary_inputs(&summary).map(|(provider, billing, gateway)| {
                    let subscription = !billing.is_metered();
                    if gateway && !subscription {
                        AccountKind::Gateway
                    } else {
                        AccountKind::billed(provider.as_str(), subscription)
                    }
                });
                let facts = endpoint_facts_of(&summary);
                let screen = facts.kind().and(facts.account_kind());
                assert_eq!(model, screen, "{summary:?}");
                shapes += 1;
            }
        }
    }
    assert_eq!(shapes, 3 * (5 + 4 + 5));
}
