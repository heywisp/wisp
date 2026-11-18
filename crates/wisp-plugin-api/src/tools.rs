use std::any::Any;

use crate::types::Display;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    ReadOnly,
    IdempotentKeyed,
    LocalTxn,
    NonIdempotent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Run,
    Show,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tags {
    pub reversible: bool,
    pub irreversible: bool,
    pub external_message: bool,
    pub reads_untrusted: bool,
    pub network: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    pub default_ms: u64,
    pub max_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolMeta {
    pub effect: Effect,
    pub tags: Tags,
    pub parallel_safe: bool,
    pub timeouts: Timeouts,
    pub saves_output: bool,
    pub needs_workspace: bool,
    pub ends_reply: bool,
    pub asks_user: bool,
    pub reports_jobs: bool,
    pub writing: Option<String>,
    pub purpose: Purpose,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientTool {
    pub description: String,
    pub input_schema: String,
    pub meta: ToolMeta,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolKind {
    Client(ClientTool),
    Native(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolDef {
    pub name: String,
    pub kind: ToolKind,
    pub system_line: Option<String>,
    pub context_line: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildContext {
    pub platform: String,
    pub capabilities: Vec<String>,
}

pub trait Prepared: Send {
    fn display(&self) -> Display;
    fn into_any(self: Box<Self>) -> Box<dyn Any + Send>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DoctorCell {
    Text(String),
    Bytes(Option<u64>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorRow {
    pub key: String,
    pub cells: Vec<DoctorCell>,
}
