use std::sync::Arc;

use futures::StreamExt;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use jiff::SignedDuration;
use wisp_protocol::plan_usage::{PlanUsage, PlanUsageError};
use wisp_protocol::{Billing, CacheTtl, ContentBlock, ErrorKind};

use super::Generation;
use crate::endpoints::ProviderKind;
use crate::provider::{
    ModelCapabilities, PrefixBytes, Provider, ProviderError, ProviderEvent, Request, SearchSignal,
    ServerCalls, TokenCount, WirePrint,
};

pub struct Guarded {
    inner: Arc<dyn Provider>,
    generation: Generation,
}

impl Guarded {
    pub(crate) fn replacement(
        inner: Arc<dyn Provider>,
        generation: Generation,
    ) -> Arc<dyn Provider> {
        Arc::new(Self { inner, generation })
    }

    pub fn wrap(inner: Arc<dyn Provider>) -> Arc<dyn Provider> {
        Arc::new(Self {
            inner,
            generation: Generation::default(),
        })
    }
}

impl Provider for Guarded {
    fn generation(&self) -> Option<Generation> {
        Some(self.generation.clone())
    }

    fn configure_plan_usage(&self, enabled: bool) -> bool {
        self.inner.configure_plan_usage(enabled)
    }

    fn plan_usage(&self) -> BoxFuture<'static, Result<Option<PlanUsage>, PlanUsageError>> {
        let admitted = self.generation.admit();
        let inner = self.inner.clone();
        Box::pin(async move {
            let _admission = admitted.map_err(|_| PlanUsageError::AuthenticationRequired)?;
            inner.plan_usage().await
        })
    }

    fn stream(
        &self,
        request: &Request<'_>,
    ) -> BoxStream<'static, Result<ProviderEvent, ProviderError>> {
        let admission = match self.generation.admit() {
            Ok(admission) => admission,
            Err(error) => {
                return Box::pin(futures::stream::once(async move {
                    Err(ProviderError::new(ErrorKind::Auth, error.to_string()))
                }));
            }
        };
        let stream = self.inner.stream(request);
        Box::pin(futures::stream::unfold(
            (stream, admission),
            |(mut stream, admission)| async move {
                stream.next().await.map(|item| (item, (stream, admission)))
            },
        ))
    }

    fn count_tokens(&self, request: &Request<'_>) -> BoxFuture<'static, TokenCount> {
        let admission = match self.generation.admit() {
            Ok(admission) => admission,
            Err(error) => {
                return Box::pin(async move {
                    TokenCount::Failed {
                        reason: error.to_string(),
                    }
                });
            }
        };
        let count = self.inner.count_tokens(request);
        Box::pin(async move {
            let _admission = admission;
            count.await
        })
    }

    fn echoed<'a>(
        &self,
        content: &'a [ContentBlock],
        earlier: &ServerCalls,
    ) -> Vec<&'a ContentBlock> {
        self.inner.echoed(content, earlier)
    }

    fn search_signal(&self, block: &ContentBlock) -> Option<SearchSignal> {
        self.inner.search_signal(block)
    }

    fn pause_limit(&self) -> u32 {
        self.inner.pause_limit()
    }

    fn capabilities(&self, model: &str) -> Option<ModelCapabilities> {
        self.inner.capabilities(model)
    }

    fn official(&self) -> bool {
        self.inner.official()
    }

    fn default_model(&self) -> String {
        self.inner.default_model()
    }

    fn model_list(&self) -> Option<std::sync::Arc<crate::models::list::ModelList>> {
        self.inner.model_list()
    }

    fn complete_model_list(&self) -> Option<std::sync::Arc<crate::models::list::ModelList>> {
        self.inner.complete_model_list()
    }

    fn list_key(&self) -> Option<&str> {
        self.inner.list_key()
    }

    fn refresh_list(&self, folder: std::path::PathBuf, always: bool) -> BoxFuture<'static, ()> {
        let admitted = self.generation.admit();
        let inner = self.inner.clone();
        Box::pin(async move {
            let Ok(_admission) = admitted else {
                return;
            };
            inner.refresh_list(folder, always).await;
        })
    }

    fn media_types(&self) -> &'static [&'static str] {
        self.inner.media_types()
    }
    fn kind(&self) -> ProviderKind {
        self.inner.kind()
    }
    fn features_documented(&self) -> bool {
        self.inner.features_documented()
    }
    fn billing(&self) -> Billing {
        self.inner.billing()
    }
    fn cache_life(&self, ttl: CacheTtl) -> SignedDuration {
        self.inner.cache_life(ttl)
    }
    fn keeps_warm(&self) -> bool {
        self.inner.keeps_warm()
    }
    fn busy_waits_5xx(&self) -> bool {
        self.inner.busy_waits_5xx()
    }
    fn account_bound_thinking(&self, model: &str) -> bool {
        self.inner.account_bound_thinking(model)
    }
    fn cache_prefix(&self, request: &Request<'_>) -> Option<String> {
        self.inner.cache_prefix(request)
    }
    fn betas(&self, request: &Request<'_>) -> Vec<String> {
        self.inner.betas(request)
    }
    fn keys_cache(&self, beta: &str) -> bool {
        self.inner.keys_cache(beta)
    }
    fn threads(&self, request: &Request<'_>) -> bool {
        self.inner.threads(request)
    }
    fn reply_print(&self, content: &[ContentBlock]) -> Option<String> {
        self.inner.reply_print(content)
    }
    fn tool_changes(&self, block: &ContentBlock) -> Vec<ContentBlock> {
        self.inner.tool_changes(block)
    }
    fn prefix_bytes(&self, request: &Request<'_>) -> Option<PrefixBytes> {
        self.inner.prefix_bytes(request)
    }
    fn wire_print(&self, request: &Request<'_>) -> Option<WirePrint> {
        self.inner.wire_print(request)
    }
    fn voice(&self) -> Option<Arc<dyn crate::provider::voice::VoiceRoute>> {
        self.inner.voice()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fake(Arc<AtomicUsize>);

    impl Provider for Fake {
        fn plan_usage(&self) -> BoxFuture<'static, Result<Option<PlanUsage>, PlanUsageError>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::pending())
        }
        fn stream(
            &self,
            _: &Request<'_>,
        ) -> BoxStream<'static, Result<ProviderEvent, ProviderError>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(futures::stream::pending())
        }
        fn count_tokens(&self, _: &Request<'_>) -> BoxFuture<'static, TokenCount> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::pending())
        }
        fn cache_prefix(&self, _: &Request<'_>) -> Option<String> {
            Some("unchanged-prefix".to_owned())
        }
        fn list_key(&self) -> Option<&str> {
            Some("listed")
        }
        fn refresh_list(&self, _: std::path::PathBuf, _: bool) -> BoxFuture<'static, ()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::ready(()))
        }
    }

    fn request(session: &wisp_protocol::SessionConfig) -> Request<'_> {
        Request {
            session,
            tools: &[],
            deferred: &[],
            by_value: &[],
            server_tools: &[],
            messages: &[],
            previous_response_id: None,
            features: crate::provider::Features::default(),
            options: crate::provider::RequestOptions::default(),
            rendering: &crate::provider::UNRENDERED,
            compaction: None,
        }
    }

    #[test]
    fn a_wrapped_anthropic_provider_keeps_what_its_adapter_overrides() {
        use crate::provider::anthropic::{
            AnthropicProvider, COMPACTION_BETA, FALLBACK_BETA, INLINE_TOOLS_BETA,
            MESSAGE_THREADS_BETA,
        };
        let credential = crate::provider::credential::Credential::ApiKey("k".into());
        let inner = AnthropicProvider::new(&credential, "http://127.0.0.1:9").unwrap();
        let provider = Guarded::wrap(Arc::new(inner.with_threads()));
        assert!(!provider.keys_cache(COMPACTION_BETA));
        assert!(!provider.keys_cache(INLINE_TOOLS_BETA));
        assert!(!provider.keys_cache(MESSAGE_THREADS_BETA));
        assert!(!provider.keys_cache(FALLBACK_BETA));
        assert!(provider.keys_cache("cache-diagnosis-2026-04-07"));
        assert!(!provider.busy_waits_5xx());
        assert!(provider.threads(&request(&crate::agent::default_session())));
        assert!(provider.reply_print(&[]).is_some());
    }

    #[test]
    fn a_wrapped_provider_reports_its_inner_providers_list() {
        use crate::agent::testing::{Fake, Knobs};
        let list = Arc::new(crate::models::list::tests::sample());
        let knobs = Knobs {
            list: Some(list.clone()),
            ..Knobs::default()
        };
        let provider = Guarded::wrap(Fake::with(knobs.clone(), []));
        assert_eq!(provider.model_list(), Some(list.clone()));
        assert_eq!(provider.complete_model_list(), None);
        let knobs = Knobs {
            kind: Some(ProviderKind::Gemini),
            ..knobs
        };
        assert_eq!(
            Guarded::wrap(Fake::with(knobs, [])).complete_model_list(),
            Some(list)
        );
        assert_eq!(Guarded::wrap(Fake::new([])).model_list(), None);
    }

    #[tokio::test]
    async fn retired_clones_refuse_every_network_entry_without_calling_adapter() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = Guarded::wrap(Arc::new(Fake(calls.clone())));
        let old = provider.clone();
        let generation = provider.generation().unwrap();
        let session = crate::agent::default_session();
        let request = request(&session);
        let streaming = provider.stream(&request);
        assert!(matches!(
            generation.change(),
            Err(super::super::Unavailable::Busy)
        ));
        drop(streaming);
        let counting = provider.count_tokens(&request);
        assert!(matches!(
            generation.change(),
            Err(super::super::Unavailable::Busy)
        ));
        drop(counting);
        let lookup = provider.plan_usage();
        assert!(matches!(
            generation.change(),
            Err(super::super::Unavailable::Busy)
        ));
        drop(lookup);
        assert_eq!(provider.list_key(), Some("listed"));
        let listing = provider.refresh_list(std::path::PathBuf::new(), false);
        assert!(matches!(
            generation.change(),
            Err(super::super::Unavailable::Busy)
        ));
        listing.await;
        generation.change().unwrap().retire();
        let before = calls.load(Ordering::SeqCst);
        assert!(old.stream(&request).next().await.unwrap().is_err());
        assert!(matches!(
            old.count_tokens(&request).await,
            TokenCount::Failed { .. }
        ));
        assert_eq!(
            old.plan_usage().await,
            Err(PlanUsageError::AuthenticationRequired)
        );
        old.refresh_list(std::path::PathBuf::new(), true).await;
        assert_eq!(calls.load(Ordering::SeqCst), before);
        assert_eq!(
            old.cache_prefix(&request).as_deref(),
            Some("unchanged-prefix")
        );
    }
}
