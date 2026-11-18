use std::future::Future;
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use wisp_plugin_api::http::{Body, HttpError, Request, Response};
use wisp_plugin_api::manifest::Capability;

use super::caller::{Caller, Render};
use super::imports::{Import, Inside};
use crate::policy::{Gate, HostCall, Verdict};

pub const USER_AGENT: &str = concat!("Wisp/", env!("CARGO_PKG_VERSION"));

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

pub const ONLY_HTTP: &str = "only http and https URLs can be fetched";

pub const IN_SLOT_RENDER: &str = "http isn't available while a sidebar section renders";

pub const IN_START_RENDER: &str = "http isn't available while a section renders at start";

pub const OLD_WORLD: &str = "http needs a plugin of world 0.2.0";

pub const BODY_CANCELLED: &str = "the request was cancelled";

pub const BODY_TIMED_OUT: &str = "the request timed out";

pub fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| e.to_string())
}

#[derive(Clone)]
struct Bounds {
    cancel: Option<CancellationToken>,
    deadline: Instant,
}

impl Bounds {
    fn new(caller: &Caller, timeout_ms: Option<u64>) -> Self {
        let wanted = timeout_ms.map_or(DEFAULT_TIMEOUT, Duration::from_millis);
        let own = Instant::now()
            .checked_add(wanted)
            .unwrap_or_else(|| Instant::now() + DEFAULT_TIMEOUT);
        let deadline = caller.deadline.map_or(own, |at| at.min(own));
        Self {
            cancel: caller.cancel.clone(),
            deadline,
        }
    }

    async fn within<F: Future>(&self, future: F) -> Result<F::Output, HttpError> {
        let cancelled = async {
            match &self.cancel {
                Some(token) => token.cancelled().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            biased;
            () = cancelled => Err(HttpError::Cancelled),
            () = tokio::time::sleep_until(self.deadline) => Err(HttpError::TimedOut),
            out = future => Ok(out),
        }
    }
}

fn refuses(import: Import, render: Render) -> bool {
    import.inside(render) != Inside::Allowed
}

fn refusal(render: Render) -> &'static str {
    match render {
        Render::Slot => IN_SLOT_RENDER,
        Render::Start => IN_START_RENDER,
    }
}

fn headers(given: Vec<(String, String)>) -> Result<HeaderMap, HttpError> {
    let mut map = HeaderMap::new();
    for (name, value) in given {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| HttpError::Failed(format!("`{name}` is not a valid header name")))?;
        if name == reqwest::header::USER_AGENT {
            continue;
        }
        let value = HeaderValue::from_str(&value)
            .map_err(|_| HttpError::Failed(format!("the `{name}` header's value is not valid")))?;
        map.append(name, value);
    }
    map.insert(
        reqwest::header::USER_AGENT,
        HeaderValue::from_static(USER_AGENT),
    );
    Ok(map)
}

fn message(error: &reqwest::Error) -> String {
    let mut message = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

fn gate_target(url: &reqwest::Url) -> String {
    let mut target = url.clone();
    let _ = target.set_username("");
    let _ = target.set_password(None);
    target.into()
}

pub async fn fetch(
    client: impl FnOnce() -> Result<reqwest::Client, String>,
    gate: &dyn Gate,
    plugin: &str,
    req: Request,
) -> Result<Response, HttpError> {
    let caller = Caller::current();
    if let Some(render) = caller.render.filter(|r| refuses(Import::HttpFetch, *r)) {
        return Err(HttpError::NotAllowed(refusal(render).to_owned()));
    }
    let url = reqwest::Url::parse(&req.url)
        .map_err(|e| HttpError::Failed(format!("the URL is not valid: {e}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(HttpError::NotAllowed(ONLY_HTTP.to_owned()));
    }
    match gate.check_host(&HostCall {
        plugin,
        capability: Capability::Net,
        target: &gate_target(&url),
    }) {
        Verdict::Allow => {}
    }
    let method = reqwest::Method::from_bytes(req.method.as_bytes())
        .map_err(|_| HttpError::Failed(format!("`{}` is not a valid method", req.method)))?;
    let headers = headers(req.headers)?;
    let bounds = Bounds::new(&caller, req.timeout_ms);
    if bounds
        .cancel
        .as_ref()
        .is_some_and(CancellationToken::is_cancelled)
    {
        return Err(HttpError::Cancelled);
    }
    let client = client().map_err(HttpError::Failed)?;
    let mut request = client.request(method, url).headers(headers);
    if let Some(body) = req.body {
        request = request.body(body);
    }
    let response = bounds
        .within(request.send())
        .await?
        .map_err(|e| HttpError::Failed(message(&e.without_url())))?;
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect();
    Ok(Response {
        status: response.status().as_u16(),
        headers,
        body: Box::new(HostBody {
            state: tokio::sync::Mutex::new(Reading {
                response: Some(response),
                left: Vec::new(),
                at: 0,
                failed: None,
            }),
            bounds,
        }),
    })
}

struct Reading {
    response: Option<reqwest::Response>,
    left: Vec<u8>,
    at: usize,
    failed: Option<String>,
}

impl Reading {
    fn take(&mut self, max: u32) -> Vec<u8> {
        let max = usize::try_from(max).unwrap_or(usize::MAX);
        let end = self.at.saturating_add(max).min(self.left.len());
        let out = self.left[self.at..end].to_vec();
        self.at = end;
        if self.at == self.left.len() {
            self.left = Vec::new();
            self.at = 0;
        }
        out
    }

    fn fail(&mut self, message: String) -> Result<Option<Vec<u8>>, String> {
        self.response = None;
        self.left = Vec::new();
        self.at = 0;
        self.failed = Some(message.clone());
        Err(message)
    }
}

struct HostBody {
    state: tokio::sync::Mutex<Reading>,
    bounds: Bounds,
}

impl Body for HostBody {
    fn read(&self, max: u32) -> BoxFuture<'_, Result<Option<Vec<u8>>, String>> {
        async move {
            if let Some(render) = Caller::current()
                .render
                .filter(|r| refuses(Import::HttpBodyRead, *r))
            {
                return Err(refusal(render).to_owned());
            }
            let mut state = self.state.lock().await;
            if let Some(failed) = &state.failed {
                return Err(failed.clone());
            }
            if !state.left.is_empty() {
                return Ok(Some(state.take(max)));
            }
            let Some(response) = state.response.as_mut() else {
                return Ok(None);
            };
            let chunk = match self.bounds.within(response.chunk()).await {
                Ok(chunk) => chunk,
                Err(HttpError::Cancelled) => return state.fail(BODY_CANCELLED.to_owned()),
                Err(_) => return state.fail(BODY_TIMED_OUT.to_owned()),
            };
            match chunk {
                Ok(Some(bytes)) => {
                    state.left = bytes.to_vec();
                    state.at = 0;
                    Ok(Some(state.take(max)))
                }
                Ok(None) => {
                    state.response = None;
                    Ok(None)
                }
                Err(e) => state.fail(message(&e.without_url())),
            }
        }
        .boxed()
    }
}

#[cfg(test)]
pub(crate) mod tests;
