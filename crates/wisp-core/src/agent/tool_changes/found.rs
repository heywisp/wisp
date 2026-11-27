use wisp_plugin_api::index::Kind;
use wisp_protocol::log::LogEvent;
use wisp_protocol::{AgentEvent, CacheEffect, ToolChangeVia};

use super::{Agent, FeatureUse, ToolPart, ToolSpec};
use crate::agent::Finished;

const DEFINITION_BYTES: usize = 16 * 1024;
const CUT: &str = "\n[description shortened to fit the 16 KiB definition limit]";

pub(super) fn definition(spec: &ToolSpec) -> Option<ToolSpec> {
    let size = |spec: &ToolSpec| serde_json::to_vec(spec).ok().map(|v| v.len());
    if size(spec)? <= DEFINITION_BYTES {
        return Some(spec.clone());
    }
    let mut limited = spec.clone();
    limited.description = CUT.to_owned();
    if size(&limited)? > DEFINITION_BYTES {
        return None;
    }
    let mut low = 0;
    let mut high = spec.description.len();
    while low < high {
        let mut end = low + (high - low).div_ceil(2);
        while !spec.description.is_char_boundary(end) {
            end -= 1;
        }
        limited.description = format!("{}{CUT}", &spec.description[..end]);
        if size(&limited)? <= DEFINITION_BYTES {
            low = if end == low { break } else { end };
        } else {
            high = end.saturating_sub(1);
        }
    }
    while !spec.description.is_char_boundary(low) {
        low -= 1;
    }
    limited.description = format!("{}{CUT}", &spec.description[..low]);
    Some(limited)
}

impl Agent {
    pub(in crate::agent) fn found_use(&self) -> Option<FeatureUse> {
        if self.is_child() || !self.settings.layers.full_screen || !self.switchable() {
            return None;
        }
        self.provider()
            .capabilities(&self.session.model)?
            .tools_after_results
            .then_some(())?;
        self.inline_use()
    }

    pub(in crate::agent) fn found_hits(&mut self, run: &mut Finished) {
        if run.outcome.is_error()
            || !run.hits.iter().any(|hit| hit.kind == Kind::Tool)
            || self.found_use().is_none()
        {
            return;
        }
        let Some(host) = &self.hooks.host else { return };
        let entries = host.index_tool_entries();
        let allowed = self.permitted_tools().unwrap_or_default();
        let mut changes = std::collections::BTreeMap::new();
        for hit in &run.hits {
            if hit.kind != Kind::Tool || !allowed.contains(&hit.name) {
                continue;
            }
            let Some(spec) = self.toolbox.definition(&hit.name) else {
                continue;
            };
            if definition(spec).is_none() {
                continue;
            }
            let Some(entry) = entries.iter().find(|entry| entry.name == hit.name) else {
                continue;
            };
            let parameters = spec.input_schema.to_string();
            changes.insert(
                wisp_tool_text::index::hit_line(
                    entry,
                    &wisp_tool_text::index::run_with(&parameters),
                ),
                wisp_tool_text::index::hit_line(entry, "call it"),
            );
            if !self.sent.iter().any(|spec| spec.name == hit.name) {
                self.tool_switches.discovered.insert(hit.name.clone());
            }
        }
        for result in std::iter::once(&mut run.text).chain(run.result.iter_mut()) {
            *result = result
                .lines()
                .map(|line| changes.get(line).map_or(line, String::as_str))
                .collect::<Vec<_>>()
                .join("\n");
        }
    }

    pub(in crate::agent) async fn found_moment(
        &mut self,
        events: &tokio::sync::mpsc::Sender<AgentEvent>,
    ) -> ToolPart {
        let discovered = std::mem::take(&mut self.tool_switches.discovered);
        if discovered.is_empty() || self.found_use().is_none() {
            return ToolPart::default();
        }
        let allowed = self.permitted_tools().unwrap_or_default();
        let point = self.history.len();
        for name in discovered.into_iter().filter(|name| allowed.contains(name)) {
            self.tool_switches.found.entry(name).or_insert(point);
        }
        let mut desired = self.applied_on();
        desired.extend(
            self.tool_switches
                .found
                .keys()
                .filter(|name| allowed.contains(*name))
                .cloned(),
        );
        let decision = self.decide_for(&desired);
        if !decision.rebuild.is_empty() {
            return ToolPart::default();
        }
        let before = self.offered_now();
        self.apply_tools(&decision);
        self.mark_tools_in_catalog(&decision, events).await;
        let added: Vec<_> = self.offered_now().difference(&before).cloned().collect();
        if !added.is_empty() {
            let cache = match self.found_use() {
                Some(FeatureUse::Trial) => CacheEffect::KeptIfPassedOn,
                _ => CacheEffect::Kept,
            };
            self.tools_changed(
                added,
                Vec::new(),
                ToolChangeVia::Inline,
                Some(cache),
                Box::default(),
                events,
            )
            .await;
        }
        ToolPart {
            blocks: decision.blocks(),
            note: None,
        }
    }

    pub(in crate::agent) fn restore_found<'a>(
        &mut self,
        events: impl Iterator<Item = (usize, &'a LogEvent, bool)>,
        sources: &[usize],
    ) {
        self.tool_switches.found.clear();
        let mut forgotten = std::collections::BTreeSet::new();
        let mut current = None;
        for (source, event, masked) in events {
            match event {
                LogEvent::TurnStarted { turn, .. } => current = Some(*turn),
                LogEvent::ToolsChanged {
                    turn,
                    via: ToolChangeVia::Inline,
                    added,
                    ..
                } if current == Some(*turn) => {
                    let at = sources.partition_point(|index| *index <= source);
                    for name in added
                        .iter()
                        .filter(|name| !self.sent.iter().any(|spec| spec.name == **name))
                    {
                        if masked {
                            forgotten.insert(name.clone());
                        } else {
                            self.tool_switches.found.entry(name.clone()).or_insert(at);
                            forgotten.remove(name);
                        }
                    }
                }
                LogEvent::ToolsChanged { added, removed, .. } => {
                    for name in added.iter().chain(removed) {
                        forgotten.remove(name);
                    }
                }
                _ => {}
            }
        }
        if let Some(on) = self.tool_switches.logged_on.as_mut() {
            for name in forgotten {
                if !self.tool_switches.found.contains_key(&name) {
                    on.remove(&name);
                }
            }
        }
    }
}
