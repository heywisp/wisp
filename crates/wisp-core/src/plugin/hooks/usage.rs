use std::sync::Arc;

use tokio::task::JoinHandle;
use wisp_plugin_api::hooks::Event;
use wisp_plugin_api::usage::AccountUsage;

use super::Engine;
use super::engine::Payload;
use super::points::USAGE_CHANGED;
use crate::plan_usage::PlanUsageCache;
use crate::plugin::KernelHost;

pub struct UsageLane(JoinHandle<()>);

fn comparable(mut rows: Vec<AccountUsage>) -> Vec<AccountUsage> {
    for row in &mut rows {
        for window in &mut row.windows {
            window.observed_at = 0;
        }
    }
    rows
}

impl UsageLane {
    pub fn start(engine: Arc<Engine>, host: Arc<KernelHost>, cache: &PlanUsageCache) -> Self {
        let mut changes = cache.changes();
        let mut previous = comparable(host.usage_accounts());
        Self(tokio::spawn(async move {
            while changes.changed().await.is_ok() {
                let rows = comparable(host.usage_accounts());
                if rows == previous {
                    continue;
                }
                previous = rows;
                engine.mark(USAGE_CHANGED);
                if engine.attached(USAGE_CHANGED) {
                    engine
                        .clone()
                        .fire_detached(
                            USAGE_CHANGED,
                            Payload::Hook(Event::Custom(wisp_plugin_api::hooks::Custom {
                                point: USAGE_CHANGED.to_owned(),
                                version: "1".to_owned(),
                                payload: String::new(),
                            })),
                        )
                        .await;
                    engine.mark(USAGE_CHANGED);
                }
            }
        }))
    }
}

impl Drop for UsageLane {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
mod tests;
