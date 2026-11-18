use std::collections::BTreeSet;

use wisp_protocol::log::PrefixCopy;

use super::{Decision, LoggedBase};
use crate::agent::Agent;
use crate::agent::epoch::MarkReason;
use crate::search::WEB_SEARCH;
use crate::session::sha256_hex;
use crate::tool::{ToolSpec, definitions_of};

impl Agent {
    pub(super) fn rebuild_tools(&mut self, decision: &Decision) -> Vec<String> {
        self.check_prefix();
        let before = self.pair_tools().names;
        if self.search.built_with.is_none() {
            self.search.built_with = Some(self.current_tools().digest);
        }
        if let Some(wanted) = decision
            .search
            .filter(|_| decision.rebuild.contains(WEB_SEARCH))
        {
            self.flip_search(wanted);
        }
        let offer = self.read_offer(self.inline_use().is_some());
        let set: BTreeSet<&str> = self.deferred.iter().map(|s| s.name.as_str()).collect();
        let names: BTreeSet<&String> = offer
            .declared
            .iter()
            .filter(|name| !set.contains(name.as_str()))
            .chain(decision.rebuild.iter().filter(|name| *name != WEB_SEARCH))
            .collect();
        let sent: Vec<ToolSpec> = names
            .into_iter()
            .filter_map(|name| {
                self.toolbox
                    .definition(name)
                    .or_else(|| offer.latest.get(name))
                    .cloned()
            })
            .collect();
        let mut lines: Vec<String> = Vec::new();
        for line in sent
            .iter()
            .filter_map(|spec| self.toolbox.system_line_of(&spec.name))
        {
            if !lines.contains(&line) {
                lines.push(line);
            }
        }
        self.prompt.tool_lines = lines;
        self.session.system = self.prompt.build();
        if let Some(own) = self.tool_switches.own_context.take() {
            self.tool_context = own;
        }
        self.tool_switches.described.clear();
        self.tool_switches.base = LoggedBase::Own;
        self.deferred
            .retain(|spec| !sent.iter().any(|kept| kept.name == spec.name));
        let mut copy = self.whole_tools();
        copy.tool_definitions = definitions_of(&sent);
        copy.deferred_definitions = definitions_of(&self.deferred);
        copy.tool_context = self.tool_context.clone().map(Box::new);
        copy.history_len = self.history.len();
        self.tool_switches.point = copy.history_len;
        self.sent = sent;
        self.tool_switches.copy = Some(copy);
        self.epoch.rebuilt(MarkReason::ToolsChange);
        before
    }

    pub(super) fn move_sent_tools(&mut self, before: &[String]) {
        let now = self.pair_tools().names;
        if let Some(sent) = self.search.sent_tools.as_mut() {
            sent.retain(|name| now.contains(name) || !before.contains(name));
            for name in now.iter().filter(|name| !before.contains(name)) {
                if !sent.contains(name) {
                    sent.push(name.clone());
                }
            }
        }
    }

    pub(super) fn rebuilt_copy(&mut self, on: &BTreeSet<String>) -> Box<PrefixCopy> {
        let endpoint = self.endpoint_name().to_owned();
        let model = self.session.model.clone();
        let (tools, tools_sha256) = self.tools_to_log(&endpoint, &model);
        Box::new(PrefixCopy {
            system_prompt: self.session.system.clone(),
            system_prompt_sha256: self.session.system.as_deref().map(sha256_hex),
            system_base_sha256: self.base_sha256().filter(|_| self.session.system.is_some()),
            tools,
            tools_sha256,
            whole: self.whole_tools(),
            tools_on: Some(on.iter().cloned().collect()),
        })
    }
}

#[cfg(test)]
impl Agent {
    pub(in crate::agent) fn copy_described(&self) -> usize {
        self.tool_switches.described.len()
    }

    pub(in crate::agent) fn tool_context_now(&self) -> Option<wisp_protocol::log::ToolContext> {
        self.tool_context.clone()
    }
}
