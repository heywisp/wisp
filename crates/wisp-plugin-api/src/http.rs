use futures::future::BoxFuture;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    pub timeout_ms: Option<u64>,
}

pub trait Body: Send + Sync {
    fn read(&self, max: u32) -> BoxFuture<'_, Result<Option<Vec<u8>>, String>>;
}

pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Box<dyn Body>,
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpError {
    NotAllowed(String),
    TimedOut,
    Cancelled,
    Failed(String),
}

pub trait Http {
    fn fetch(&self, req: Request) -> BoxFuture<'_, Result<Response, HttpError>>;
}
