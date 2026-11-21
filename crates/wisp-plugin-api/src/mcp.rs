use crate::BoxFuture;
use crate::host::CallContext;
use crate::types::ToolRun;

pub const START_LIMIT_MS: u64 = 60_000;

const UNAVAILABLE: &str = "MCP is not available";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Stopped,
    Starting,
    Ready,
    NeedsSignIn,
    NeedsKey(Vec<String>),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    pub name: String,
    pub state: State,
    pub tools: u32,
    pub listed: bool,
    pub fresh: bool,
    pub off: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tool {
    pub name: String,
    pub server: String,
    pub original: String,
    pub description: String,
    pub input_schema: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PromptArgument {
    pub name: String,
    pub required: bool,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub name: String,
    pub server: String,
    pub original: String,
    pub description: String,
    pub arguments: Vec<PromptArgument>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resource {
    pub name: String,
    pub server: String,
    pub uri: String,
    pub title: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptMessage {
    pub role: String,
    pub text: Option<String>,
}

pub trait Mcp: Send + Sync {
    fn prompts(&self) -> Vec<Prompt>;
    fn get_prompt<'a>(&'a self, name: &'a str)
    -> BoxFuture<'a, Result<Vec<PromptMessage>, String>>;
    fn resources(&self) -> Vec<Resource>;
    fn read_resource<'a>(
        &'a self,
        name: &'a str,
    ) -> BoxFuture<'a, Result<Vec<Option<String>>, String>>;
    fn servers(&self) -> Vec<Server>;
    fn tools(&self) -> Vec<Tool>;
    fn revision(&self) -> u64;
    fn connect<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<(), String>>;
    fn call<'a>(
        &'a self,
        ctx: &'a dyn CallContext,
        name: &'a str,
        input: &'a str,
        limit_ms: u64,
    ) -> BoxFuture<'a, Result<ToolRun, String>>;
    fn add<'a>(
        &'a self,
        name: &'a str,
        command: &'a str,
        args: &'a [String],
    ) -> BoxFuture<'a, Result<u32, String>>;
    fn remove<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<(), String>>;
    fn sign_in_start<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<Option<String>, String>>;
    fn sign_in_finish<'a>(
        &'a self,
        name: &'a str,
        address: &'a str,
    ) -> BoxFuture<'a, Result<u32, String>>;
}

impl Mcp for () {
    fn prompts(&self) -> Vec<Prompt> {
        Vec::new()
    }
    fn get_prompt<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<Vec<PromptMessage>, String>> {
        Box::pin(async { Err(UNAVAILABLE.into()) })
    }
    fn resources(&self) -> Vec<Resource> {
        Vec::new()
    }
    fn read_resource<'a>(
        &'a self,
        _: &'a str,
    ) -> BoxFuture<'a, Result<Vec<Option<String>>, String>> {
        Box::pin(async { Err(UNAVAILABLE.into()) })
    }
    fn servers(&self) -> Vec<Server> {
        Vec::new()
    }
    fn tools(&self) -> Vec<Tool> {
        Vec::new()
    }
    fn revision(&self) -> u64 {
        0
    }
    fn connect<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Err(UNAVAILABLE.into()) })
    }
    fn call<'a>(
        &'a self,
        _: &'a dyn CallContext,
        _: &'a str,
        _: &'a str,
        _: u64,
    ) -> BoxFuture<'a, Result<ToolRun, String>> {
        Box::pin(async { Err(UNAVAILABLE.into()) })
    }
    fn add<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: &'a [String],
    ) -> BoxFuture<'a, Result<u32, String>> {
        Box::pin(async { Err(UNAVAILABLE.into()) })
    }
    fn remove<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Err(UNAVAILABLE.into()) })
    }
    fn sign_in_start<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Result<Option<String>, String>> {
        Box::pin(async { Err(UNAVAILABLE.into()) })
    }
    fn sign_in_finish<'a>(&'a self, _: &'a str, _: &'a str) -> BoxFuture<'a, Result<u32, String>> {
        Box::pin(async { Err(UNAVAILABLE.into()) })
    }
}
