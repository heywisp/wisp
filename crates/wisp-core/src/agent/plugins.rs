use wisp_plugin_api::host::{Facts, Fill};
use wisp_plugin_api::pricing::SpendTotals;
use wisp_protocol::AgentEvent;
use wisp_tool_text::DATA_BLOCK;

use super::Agent;
use crate::plugin::{KernelHost, PLATFORM, Prices, Snapshot, convert};
use crate::pricing;

impl Agent {
    pub(super) fn plugin_snapshot(&self, host: &KernelHost) -> Snapshot {
        self.snapshot_with(host, self.context_changed())
    }

    pub(super) fn quiet_snapshot(&self, host: &KernelHost) -> Snapshot {
        self.snapshot_with(host, self.context_figures())
    }

    fn snapshot_with(&self, host: &KernelHost, context: AgentEvent) -> Snapshot {
        let applied = self.hooks.applied.as_ref();
        let settings = host
            .owned_settings()
            .into_iter()
            .filter_map(|key| {
                let value = match applied.and_then(|a| a.settings.get(&key)) {
                    Some(value) => value.clone(),
                    None => self.owned_value(&key),
                }?;
                Some((key, value))
            })
            .collect();
        Snapshot {
            facts: Facts {
                model: self.session.model.clone(),
                endpoint_kind: self.provider().kind().as_str().to_owned(),
                effort: self.session.effort.as_str().to_owned(),
                platform: PLATFORM.to_owned(),
                capabilities: self.plugin_capabilities(),
            },
            media_types: self
                .carried_media()
                .iter()
                .map(|t| (*t).to_owned())
                .collect(),
            media_slices: Vec::new(),
            image_edge: self
                .provider()
                .capabilities(&self.session.model)
                .and_then(|caps| caps.images)
                .map(|images| images.many_image_edge),
            count: self.media_count(),
            clock: self.clock.clone(),
            settings,
            fill: self.plugin_fill(context),
            prices: self.plugin_prices(),
            gate: self.gate.clone(),
            environment: self.environment_facts(),
            terminal: self.terminal.map(crate::jobs::screen::api_size),
            usage_accounts: self
                .account_basics()
                .into_iter()
                .filter_map(|(_, usage)| usage)
                .collect(),
            usage_cache: self.plan_usage.clone(),
            no_rename: self.no_rename(),
        }
    }

    fn plugin_capabilities(&self) -> Vec<String> {
        let mut capabilities = self.toolbox.capabilities().to_vec();
        if self.models.turn_form(&self.session.model).is_some() {
            capabilities.push(DATA_BLOCK.to_owned());
        }
        capabilities
    }

    pub(super) fn terminal_shown(&mut self, size: wisp_protocol::TermSize) {
        if !self.is_child() && size.cols > 0 && size.rows > 0 {
            self.terminal = Some(size);
        }
    }

    fn plugin_prices(&self) -> Prices {
        let priced = self.priced();
        let turn = self
            .open_track()
            .and_then(|track| Some((track.usd?, track.lower_bound)));
        let any_priced = self.session_usd > 0.0 || !self.session_lower_bound;
        let session = any_priced.then_some((self.session_usd, self.session_lower_bound));
        let (turn, session) = if priced {
            (turn, session)
        } else {
            (None, None)
        };
        Prices {
            model: self
                .rates(self.endpoint, &self.session.model)
                .filter(|_| priced),
            spend: SpendTotals {
                turn: turn.map(|(usd, _)| pricing::micros(usd)),
                turn_at_least: turn.is_some_and(|(_, at_least)| at_least),
                session: session.map(|(usd, _)| pricing::micros(usd)),
                session_at_least: session.is_some_and(|(_, at_least)| at_least),
            },
        }
    }

    fn plugin_fill(&self, context: AgentEvent) -> Fill {
        let AgentEvent::ContextChanged {
            used,
            used_estimated,
            next,
            window,
            compact_at,
            epoch,
            ..
        } = context
        else {
            return Snapshot::default().fill;
        };
        let figures = self.meter_figures();
        let measured = (!used_estimated).then_some(used);
        Fill {
            estimate: next,
            measured,
            window,
            point: compact_at,
            ceiling: figures.ceiling,
            auto_compact: figures.auto_compact,
            epoch,
            parts: convert::fill_parts(self.parts(measured.unwrap_or(next))),
            cached: self.cached(),
        }
    }
}
