use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub(crate) const CLIENT: &str = "CANARY-client-0011";
pub(crate) const CLIENT_SECRET: &str = "CANARY-client-secret-0011";
pub(crate) const ACCESS: &str = "CANARY-access-0011";
pub(crate) const REFRESH: &str = "CANARY-refresh-0011";
pub(crate) const CODE: &str = "CANARY-code-0011";

pub(crate) struct Request {
    pub(crate) path: String,
    pub(crate) headers: BTreeMap<String, String>,
    pub(crate) body: Value,
}

struct State {
    base: String,
    issuer: String,
    protected: AtomicBool,
    revoked: AtomicBool,
    down: AtomicBool,
    plain_endpoints: AtomicBool,
    before_registration: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    pub(crate) version: AtomicUsize,
    pub(crate) requests: Mutex<Vec<Request>>,
}

pub(crate) struct Server {
    state: Arc<State>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Server {
    pub(crate) async fn start(protected: bool, issuer: Option<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(State {
            issuer: issuer.unwrap_or_else(|| base.clone()),
            base,
            protected: AtomicBool::new(protected),
            revoked: AtomicBool::new(false),
            down: AtomicBool::new(false),
            plain_endpoints: AtomicBool::new(false),
            before_registration: Mutex::default(),
            version: AtomicUsize::new(1),
            requests: Mutex::default(),
        });
        let shared = state.clone();
        let task = tokio::spawn(async move {
            let mut tasks = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (socket, _) = accepted.unwrap();
                        let state = shared.clone();
                        tasks.spawn(async move { handle(socket, state).await });
                    }
                    Some(result) = tasks.join_next(), if !tasks.is_empty() => result.unwrap(),
                }
            }
        });
        Self { state, task }
    }

    pub(crate) fn address(&self) -> String {
        format!("{}/mcp", self.state.base)
    }
    pub(crate) fn issuer(&self) -> String {
        self.state.issuer.clone()
    }
    pub(crate) fn revoke(&self) {
        self.state.revoked.store(true, Ordering::Release);
    }
    #[allow(dead_code, reason = "only the integration-test copy uses this control")]
    pub(crate) fn down(&self) {
        self.state.down.store(true, Ordering::Release);
    }
    pub(crate) fn plain_endpoints(&self) {
        self.state.plain_endpoints.store(true, Ordering::Release);
    }
    #[allow(dead_code, reason = "only the unit-test copy uses this control")]
    pub(crate) fn before_registration(&self, callback: impl FnOnce() + Send + 'static) {
        *self.state.before_registration.lock().unwrap() = Some(Box::new(callback));
    }
    pub(crate) fn version(&self, version: usize) {
        self.state.version.store(version, Ordering::Release);
    }
    pub(crate) fn requests<R>(&self, read: impl FnOnce(&[Request]) -> R) -> R {
        read(&self.state.requests.lock().unwrap())
    }
}

pub(crate) fn redirected(link: &str, state: Option<&str>) -> String {
    let link = reqwest::Url::parse(link).unwrap();
    let query: BTreeMap<_, _> = link
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    let mut address = reqwest::Url::parse(&query["redirect_uri"]).unwrap();
    address
        .query_pairs_mut()
        .append_pair("code", CODE)
        .append_pair("state", state.unwrap_or(&query["state"]));
    address.into()
}

async fn handle(mut socket: tokio::net::TcpStream, state: Arc<State>) {
    let mut bytes = Vec::new();
    let (head, body) = loop {
        let mut chunk = [0; 4096];
        let count = socket.read(&mut chunk).await.unwrap();
        if count == 0 {
            return;
        }
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(split) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let head = String::from_utf8(bytes[..split].to_vec()).unwrap();
            let length = head
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .map_or(0, |(_, value)| value.trim().parse::<usize>().unwrap());
            if bytes.len() >= split + 4 + length {
                break (head, bytes[split + 4..split + 4 + length].to_vec());
            }
        }
        assert!(bytes.len() < 1024 * 1024);
    };
    let mut first = head.lines().next().unwrap().split_whitespace();
    let method = first.next().unwrap();
    let path = first.next().unwrap().to_owned();
    let headers: BTreeMap<_, _> = head
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let body: Value = if headers
        .get("content-type")
        .is_some_and(|value| value.starts_with("application/x-www-form-urlencoded"))
    {
        reqwest::Url::parse(&format!(
            "http://localhost/?{}",
            String::from_utf8(body).unwrap()
        ))
        .unwrap()
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), Value::String(value.into_owned())))
        .collect::<serde_json::Map<_, _>>()
        .into()
    } else {
        serde_json::from_slice(&body).unwrap_or(Value::Null)
    };
    let endpoint = |suffix: &str| {
        if state.plain_endpoints.load(Ordering::Acquire) {
            format!("http://server.example.invalid/{suffix}")
        } else {
            format!("{}/{suffix}", state.issuer)
        }
    };
    let (status, extra, result) = match path.as_str() {
        "/.well-known/oauth-protected-resource" | "/.well-known/oauth-protected-resource/mcp" => (
            200,
            String::new(),
            json!({"resource": format!("{}/mcp",state.base), "authorization_servers": [state.issuer], "scopes_supported": ["read"]}),
        ),
        "/.well-known/oauth-authorization-server" | "/.well-known/openid-configuration" => (
            200,
            String::new(),
            json!({"issuer": state.issuer, "authorization_endpoint": endpoint("authorize"), "token_endpoint":endpoint("token"), "registration_endpoint":endpoint("register"), "response_types_supported":["code"], "grant_types_supported":["authorization_code","refresh_token"], "code_challenge_methods_supported":["S256"], "token_endpoint_auth_methods_supported":["none","client_secret_basic"], "scopes_supported":["read"]}),
        ),
        "/register" => {
            if let Some(callback) = state.before_registration.lock().unwrap().take() {
                callback();
            }
            (
                201,
                String::new(),
                json!({"client_id":CLIENT,"client_secret":CLIENT_SECRET,"redirect_uris":body["redirect_uris"],"token_endpoint_auth_method":"client_secret_basic"}),
            )
        }
        "/token" => {
            assert!(body["code"] == CODE || body["refresh_token"] == REFRESH);
            if body["refresh_token"] == REFRESH && state.revoked.load(Ordering::Acquire) {
                (400, String::new(), json!({"error":"invalid_grant"}))
            } else {
                state.revoked.store(false, Ordering::Release);
                (
                    200,
                    String::new(),
                    json!({"access_token":ACCESS,"refresh_token":REFRESH,"expires_in":3600,"token_type":"Bearer","scope":"read"}),
                )
            }
        }

        "/mcp" if state.down.load(Ordering::Acquire) => (503, String::new(), json!({})),
        "/mcp"
            if state.protected.load(Ordering::Acquire)
                && (state.revoked.load(Ordering::Acquire)
                    || headers.get("authorization") != Some(&format!("Bearer {ACCESS}"))) =>
        {
            (
                401,
                format!(
                    "WWW-Authenticate: Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource\"\r\n",
                    state.base
                ),
                json!({}),
            )
        }
        "/mcp" if method != "POST" => (405, String::new(), json!({})),
        "/mcp" => {
            let version = state.version.load(Ordering::Acquire);
            let result = match body["method"].as_str().unwrap_or_default() {
                "server/discover" => {
                    json!({"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}} ,"ttlMs":0,"cacheScope":"private"})
                }
                "initialize" => {
                    json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}})
                }
                "tools/list" => {
                    json!({"resultType":"complete","tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object","properties":{"version":{"const":version}}}}],"ttlMs":1000,"cacheScope":"private"})
                }
                "tools/call" if body["params"]["arguments"]["invalid"] == true => Value::Null,
                "tools/call" if body["params"]["arguments"]["ask"] == true => {
                    json!({"resultType":"input_required","requestState":"CANARY-request-state","inputRequests":{"choice":{"method":"elicitation/create","params":{"mode":"form","message":"Choose","requestedSchema":{"type":"object","properties":{"choice":{"type":"string","enum":["one","two"]}},"required":["choice"]}}}}})
                }
                _ => json!({"content":[{"type":"text","text":"fixture answer"}],"isError":false}),
            };
            if body.get("id").is_none() {
                (202, String::new(), Value::Null)
            } else if result.is_null() {
                (
                    200,
                    String::new(),
                    json!({"jsonrpc":"2.0","id":body["id"],"error":{"code":-32602,"message":"invalid arguments"}}),
                )
            } else {
                (
                    200,
                    String::new(),
                    json!({"jsonrpc":"2.0","id":body["id"],"result":result}),
                )
            }
        }
        _ => (404, String::new(), json!({})),
    };
    state.requests.lock().unwrap().push(Request {
        path,
        headers,
        body,
    });
    let body = if status == 202 {
        String::new()
    } else {
        result.to_string()
    };
    let response = format!(
        "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = socket.write_all(response.as_bytes()).await;
}
