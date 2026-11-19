use std::sync::{Arc, Mutex, PoisonError};

use futures::stream::BoxStream;
use reqwest::header::{HeaderName, HeaderValue};
use rmcp::model::{ClientJsonRpcMessage, ClientRequest};
use rmcp::transport::auth::{AuthClient, AuthorizationManager};
use rmcp::transport::streamable_http_client::{
    StreamableHttpClient, StreamableHttpError, StreamableHttpPostResponse,
};
use sse_stream::{Error as SseError, Sse};

#[derive(Clone)]
pub(super) struct Http {
    address: Arc<str>,
    client: reqwest::Client,
    authorized: Option<Arc<AuthClient<reqwest::Client>>>,
    challenge: Arc<Mutex<Option<String>>>,
}

#[allow(
    clippy::disallowed_types,
    reason = "rmcp::StreamableHttpClient method signatures require this map; Wisp only forwards it"
)]
type Headers = std::collections::HashMap<HeaderName, HeaderValue>;

type Error = StreamableHttpError<reqwest::Error>;

impl Http {
    pub(super) fn new(
        address: &str,
        manager: Option<AuthorizationManager>,
        challenge: Arc<Mutex<Option<String>>>,
    ) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "couldn't open the server's address")?;
        Ok(Self {
            address: address.into(),
            authorized: manager.map(|manager| Arc::new(AuthClient::new(client.clone(), manager))),
            client,
            challenge,
        })
    }

    fn check(&self, address: &str, supplied: Option<String>) -> Result<(), Error> {
        if address != self.address.as_ref()
            || supplied.is_some()
            || self.authorized.is_some() && !super::sign_in::permitted(address)
        {
            Err(Error::UnexpectedServerResponse(
                "the server's address changed".into(),
            ))
        } else {
            Ok(())
        }
    }

    fn received<T>(&self, result: Result<T, Error>, track_challenge: bool) -> Result<T, Error> {
        if track_challenge && let Err(Error::AuthRequired(challenge)) = &result {
            *self
                .challenge
                .lock()
                .unwrap_or_else(PoisonError::into_inner) =
                Some(challenge.www_authenticate_header.clone());
        }
        result
    }
}

fn is_listen(message: &ClientJsonRpcMessage) -> bool {
    matches!(message, ClientJsonRpcMessage::Request(request)
        if matches!(request.request, ClientRequest::SubscriptionsListenRequest(_)))
}

macro_rules! send {
    ($this:ident, $uri:ident, $supplied:ident, $track:expr, $method:ident($($arg:expr),*)) => {{
        $this.check(&$uri, $supplied)?;
        let track_challenge = $track;
        let result = async {
            match &$this.authorized {
                Some(client) => client.$method($uri, $($arg),*).await,
                None => $this.client.$method($uri, $($arg),*).await,
            }
        }.await;
        $this.received(result, track_challenge)
    }};
}

impl StreamableHttpClient for Http {
    type Error = reqwest::Error;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: Headers,
    ) -> Result<StreamableHttpPostResponse, Error> {
        send!(
            self,
            uri,
            auth_header,
            !is_listen(&message),
            post_message(message, session_id, None, custom_headers)
        )
    }

    async fn post_message_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: Headers,
        max_sse_event_size: usize,
    ) -> Result<StreamableHttpPostResponse, Error> {
        send!(
            self,
            uri,
            auth_header,
            !is_listen(&message),
            post_message_with_max_sse_event_size(
                message,
                session_id,
                None,
                custom_headers,
                max_sse_event_size
            )
        )
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_header: Option<String>,
        custom_headers: Headers,
    ) -> Result<(), Error> {
        send!(
            self,
            uri,
            auth_header,
            true,
            delete_session(session_id, None, custom_headers)
        )
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: Headers,
    ) -> Result<BoxStream<'static, Result<Sse, SseError>>, Error> {
        send!(
            self,
            uri,
            auth_header,
            true,
            get_stream(session_id, last_event_id, None, custom_headers)
        )
    }

    async fn get_stream_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: Headers,
        max_sse_event_size: usize,
    ) -> Result<BoxStream<'static, Result<Sse, SseError>>, Error> {
        send!(
            self,
            uri,
            auth_header,
            true,
            get_stream_with_max_sse_event_size(
                session_id,
                last_event_id,
                None,
                custom_headers,
                max_sse_event_size
            )
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn tokens_are_bound_to_one_address_and_https_or_loopback() {
        for (address, allowed) in [
            ("https://server.example.invalid/mcp", true),
            ("http://127.0.0.1:8123/mcp", true),
            ("http://localhost:8123/mcp", true),
            ("http://[::1]:8123/mcp", true),
            ("http://[2001:db8::1]:8123/mcp", false),
            ("http://server.example.invalid/mcp", false),
        ] {
            let manager = AuthorizationManager::new(address).await.unwrap();
            let client = Http::new(address, Some(manager), Arc::default()).unwrap();
            assert_eq!(client.check(address, None).is_ok(), allowed);
            assert!(client.check(&format!("{address}/other"), None).is_err());
            assert!(
                client
                    .check(address, Some("CANARY-supplied-token".into()))
                    .is_err()
            );
        }
    }
}
