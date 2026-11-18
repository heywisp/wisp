use std::path::Path;

use serde_json::Value;
use wisp_plugin_api::manifest::Capability;

#[derive(Debug, Clone, Copy)]
pub struct GateCall<'a> {
    pub tool: &'a str,
    pub input: &'a Value,
    pub cwd: &'a Path,
}

#[derive(Debug, Clone, Copy)]
pub struct HostCall<'a> {
    pub plugin: &'a str,
    pub capability: Capability,
    pub target: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allow,
}

pub trait Gate: Send + Sync {
    fn check(&self, call: &GateCall<'_>) -> Verdict;
    fn check_host(&self, call: &HostCall<'_>) -> Verdict;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AllowAll;

impl Gate for AllowAll {
    fn check(&self, _call: &GateCall<'_>) -> Verdict {
        Verdict::Allow
    }

    fn check_host(&self, _call: &HostCall<'_>) -> Verdict {
        Verdict::Allow
    }
}
