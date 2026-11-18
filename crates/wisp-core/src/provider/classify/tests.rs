use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::json;

use super::*;

fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
    let mut h = HeaderMap::new();
    for (name, value) in pairs {
        h.insert(*name, HeaderValue::from_str(value).unwrap());
    }
    h
}

fn anthropic(error_type: &str) -> Value {
    json!({"type": "error", "error": {"type": error_type, "message": "m"}, "request_id": "req_body"})
}

fn classify_http(status: u16, h: &HeaderMap, body: &Value) -> Classified {
    http_error(status, h, error_object(body).as_ref())
}

struct Row {
    status: u16,
    headers: &'static [(&'static str, &'static str)],
    body: Value,
    kind: ErrorKind,
    retry: RetryClass,
    billed: bool,
    error_type: Option<&'static str>,
}

fn row(
    status: u16,
    body: Value,
    kind: ErrorKind,
    retry: RetryClass,
    billed: bool,
    error_type: Option<&'static str>,
) -> Row {
    Row {
        status,
        headers: &[],
        body,
        kind,
        retry,
        billed,
        error_type,
    }
}

#[test]
fn every_http_status_row_has_its_kind_retry_and_billing() {
    use ErrorKind as K;
    use RetryClass::{Never, Transient};
    let none = Value::Null;
    let rows = [
        row(
            400,
            anthropic("invalid_request_error"),
            K::InvalidRequest,
            Never,
            false,
            Some("invalid_request_error"),
        ),
        row(
            401,
            anthropic("authentication_error"),
            K::Auth,
            Never,
            false,
            Some("authentication_error"),
        ),
        row(
            403,
            anthropic("permission_error"),
            K::Auth,
            Never,
            false,
            Some("permission_error"),
        ),
        row(
            402,
            anthropic("billing_error"),
            K::Billing,
            Never,
            false,
            Some("billing_error"),
        ),
        row(402, none.clone(), K::Billing, Never, false, None),
        row(
            404,
            anthropic("not_found_error"),
            K::InvalidRequest,
            Never,
            false,
            Some("not_found_error"),
        ),
        row(408, none.clone(), K::Timeout, Transient, true, None),
        row(
            409,
            anthropic("conflict_error"),
            K::Server,
            Transient,
            false,
            Some("conflict_error"),
        ),
        row(
            413,
            anthropic("request_too_large"),
            K::InvalidRequest,
            Never,
            false,
            Some("request_too_large"),
        ),
        row(
            429,
            anthropic("rate_limit_error"),
            K::RateLimit,
            Transient,
            false,
            Some("rate_limit_error"),
        ),
        row(
            500,
            anthropic("api_error"),
            K::Server,
            Transient,
            true,
            Some("api_error"),
        ),
        row(500, none.clone(), K::Server, Transient, true, None),
        row(
            502,
            anthropic("api_error"),
            K::Server,
            Transient,
            true,
            Some("api_error"),
        ),
        row(502, none.clone(), K::Server, Transient, true, None),
        row(503, none.clone(), K::Server, Transient, true, None),
        row(
            503,
            anthropic("overloaded_error"),
            K::Overloaded,
            Transient,
            false,
            Some("overloaded_error"),
        ),
        row(
            503,
            json!({"error": {"code": "server_is_overloaded"}}),
            K::Overloaded,
            Transient,
            false,
            Some("server_is_overloaded"),
        ),
        row(
            429,
            json!({"error": {"code": "usage_limit_reached"}}),
            K::Billing,
            Never,
            false,
            Some("usage_limit_reached"),
        ),
        row(504, none.clone(), K::Timeout, Transient, true, None),
        row(
            504,
            anthropic("timeout_error"),
            K::Timeout,
            Transient,
            true,
            Some("timeout_error"),
        ),
        row(
            529,
            anthropic("overloaded_error"),
            K::Overloaded,
            Transient,
            false,
            Some("overloaded_error"),
        ),
        row(529, none.clone(), K::Overloaded, Transient, false, None),
        row(520, none.clone(), K::Server, Transient, true, None),
        row(599, none.clone(), K::Server, Transient, true, None),
        row(
            301,
            anthropic("api_error"),
            K::InvalidResponse,
            Never,
            false,
            Some("api_error"),
        ),
        row(307, none.clone(), K::InvalidResponse, Never, false, None),
        row(418, none.clone(), K::InvalidRequest, Never, false, None),
        row(422, none.clone(), K::InvalidRequest, Never, false, None),
        row(
            400,
            json!({"error": {"message": "too long", "type": "invalid_request_error", "code": "context_length_exceeded"}}),
            K::ContextOverflow,
            Never,
            false,
            Some("context_length_exceeded"),
        ),
        row(
            429,
            json!({"error": {"message": "slow", "type": "requests", "code": "rate_limit_exceeded"}}),
            K::RateLimit,
            Transient,
            false,
            Some("rate_limit_exceeded"),
        ),
        row(
            429,
            json!({"error": {"message": "quota", "type": "insufficient_quota", "code": "insufficient_quota"}}),
            K::Billing,
            Never,
            false,
            Some("insufficient_quota"),
        ),
        row(
            401,
            json!({"error": {"message": "bad key", "type": "invalid_request_error", "code": "invalid_api_key"}}),
            K::Auth,
            Never,
            false,
            Some("invalid_api_key"),
        ),
        row(
            502,
            json!({"error": {"message": "upstream", "type": "server_error", "code": null, "status": 502}}),
            K::Server,
            Transient,
            true,
            Some("server_error"),
        ),
        row(
            500,
            json!({"error": "plain text"}),
            K::Server,
            Transient,
            true,
            None,
        ),
    ];
    for r in rows {
        let c = classify_http(r.status, &headers(r.headers), &r.body);
        let label = format!("{} {}", r.status, r.body);
        assert_eq!(c.kind, r.kind, "{label}");
        assert_eq!(c.detail.retry, r.retry, "{label}");
        assert_eq!(c.detail.possibly_billed, r.billed, "{label}");
        assert_eq!(c.detail.error_type.as_deref(), r.error_type, "{label}");
        assert_eq!(c.detail.status, Some(r.status), "{label}");
        assert_eq!(c.detail.phase, FailurePhase::Response, "{label}");
        assert_eq!(c.detail.source, FailureSource::Endpoint, "{label}");
    }
}

#[test]
fn a_429_with_a_retry_after_header_asks_to_wait() {
    let body = anthropic("rate_limit_error");
    let seconds = classify_http(429, &headers(&[("retry-after", "2")]), &body);
    assert_eq!(seconds.detail.retry, RetryClass::After { ms: 2000 });
    let millis = classify_http(429, &headers(&[("retry-after-ms", "1500")]), &body);
    assert_eq!(millis.detail.retry, RetryClass::After { ms: 1500 });
    let server = classify_http(
        500,
        &headers(&[("retry-after", "2")]),
        &anthropic("api_error"),
    );
    assert_eq!(server.detail.retry, RetryClass::Transient);
}

#[test]
fn a_gateways_concurrency_429_is_a_rate_limit_and_a_spend_cap_is_billing() {
    let m203 = json!({"type": "error", "error": {"type": "rate_limit_error",
        "message": "[Manifest M203] Too many concurrent requests"}});
    let busy = classify_http(429, &HeaderMap::new(), &m203);
    assert_eq!(busy.kind, ErrorKind::RateLimit);
    assert_eq!(busy.detail.retry, RetryClass::Transient);
    assert!(!busy.detail.possibly_billed);
    let cap = json!({"type": "error", "error": {"type": "rate_limit_error",
        "message": "You have reached your specified API usage limits.",
        "details": {"error_code": "enforced_spend_limit_reached"}}});
    let cap = classify_http(429, &HeaderMap::new(), &cap);
    assert_eq!(cap.kind, ErrorKind::Billing);
    assert_eq!(cap.detail.retry, RetryClass::Never);
    assert_eq!(cap.detail.error_type.as_deref(), Some("rate_limit_error"));
}

#[test]
fn a_529_with_a_retry_after_header_asks_to_wait() {
    let overloaded = classify_http(
        529,
        &headers(&[("retry-after", "3")]),
        &anthropic("overloaded_error"),
    );
    assert_eq!(overloaded.kind, ErrorKind::Overloaded);
    assert_eq!(overloaded.detail.retry, RetryClass::After { ms: 3000 });
}

#[test]
fn the_request_id_comes_from_the_header_then_the_body() {
    let body = anthropic("api_error");
    let from_header = classify_http(500, &headers(&[("request-id", "req_header")]), &body);
    assert_eq!(from_header.detail.request_id.as_deref(), Some("req_header"));
    let from_body = classify_http(500, &HeaderMap::new(), &body);
    assert_eq!(from_body.detail.request_id.as_deref(), Some("req_body"));
    let none = classify_http(500, &HeaderMap::new(), &Value::Null);
    assert_eq!(none.detail.request_id, None);

    let registered = "CANARY/registered for request ID";
    crate::session::register_secrets([registered.to_owned()]);
    let with_registered = format!("req: {registered} end");
    let prefixed_key = "sk-proj-CANARY_ab1_cd2_ef3";
    let long_id = "x".repeat(80);
    let real_shape = "req_011CSCANARY000000000000";
    let uuid = "00000000-0000-0000-0000-000000000000";
    for (raw, expected) in [
        (with_registered.as_str(), "req: [redacted] end"),
        (prefixed_key, "[redacted]"),
        (long_id.as_str(), &long_id[..64]),
        (real_shape, real_shape),
        (uuid, uuid),
    ] {
        let h = headers(&[("request-id", raw)]);
        let object =
            error_object(&json!({"error": {"type": "api_error", "request_id": raw}})).unwrap();
        let after_200 = super::super::sse::after_200(
            ProviderError::new(ErrorKind::Server, "failed"),
            Some(raw.to_owned()),
        );
        for detail in [
            http_error(500, &h, error_object(&body).as_ref()).detail,
            http_error(500, &HeaderMap::new(), Some(&object)).detail,
            body_error(&object, FailurePhase::Stream, None).detail,
            body_error(
                &error_object(&body).unwrap(),
                FailurePhase::Stream,
                Some(raw),
            )
            .detail,
            text_reply(FailurePhase::Response, Some(raw)),
            *after_200.detail,
        ] {
            assert_eq!(detail.request_id.as_deref(), Some(expected), "{raw:?}");
        }
    }
    let flat = text_reply(FailurePhase::Response, Some("a\rb\nc\td\u{2028}e"));
    assert_eq!(flat.request_id.as_deref(), Some("a b c d e"));
}

#[test]
fn retry_after_reads_seconds_and_milliseconds_and_ignores_dates_and_garbage() {
    let read = |pairs: &[(&'static str, &'static str)]| retry_after_ms(&headers(pairs));
    assert_eq!(read(&[("retry-after", "3")]), Some(3000));
    assert_eq!(read(&[("retry-after", "0.5")]), Some(500));
    assert_eq!(read(&[("retry-after", "120")]), Some(120_000));
    assert_eq!(read(&[("retry-after-ms", "250")]), Some(250));
    assert_eq!(
        read(&[("retry-after-ms", "250"), ("retry-after", "9")]),
        Some(250)
    );
    assert_eq!(
        read(&[("retry-after-ms", "soon"), ("retry-after", "9")]),
        Some(9000)
    );
    assert_eq!(
        read(&[("retry-after", "Wed, 21 Oct 2026 07:28:00 GMT")]),
        None
    );
    for garbage in ["soon", "-1", "NaN", "inf", ""] {
        assert_eq!(read(&[("retry-after", garbage)]), None, "{garbage}");
    }
    assert_eq!(read(&[]), None);
}

#[test]
fn error_objects_are_read_in_every_shape() {
    let anthropic_body = json!({"type": "error", "error": {"type": "api_error", "message": "boom"}, "request_id": "req_1"});
    assert_eq!(
        error_object(&anthropic_body),
        Some(ErrorObject {
            error_type: Some("api_error".into()),
            code: None,
            message: Some("boom".into()),
            status: None,
            request_id: Some("req_1".into()),
            error_code: None,
        })
    );
    let openai =
        json!({"error": {"message": "bad", "type": "server_error", "code": 17, "status": 502}});
    assert_eq!(
        error_object(&openai),
        Some(ErrorObject {
            error_type: Some("server_error".into()),
            code: Some("17".into()),
            message: Some("bad".into()),
            status: Some(502),
            request_id: None,
            error_code: None,
        })
    );
    let without_status = json!({"error": {"message": "bad", "type": "server_error", "code": "x"}});
    assert_eq!(error_object(&without_status).and_then(|o| o.status), None);
    let bare = json!({"error": "upstream exploded"});
    assert_eq!(
        error_object(&bare),
        Some(ErrorObject {
            message: Some("upstream exploded".into()),
            ..ErrorObject::default()
        })
    );
    let typed =
        json!({"type": "error", "code": "rate_limit_exceeded", "message": "slow", "status": "429"});
    assert_eq!(
        error_object(&typed),
        Some(ErrorObject {
            error_type: None,
            code: Some("rate_limit_exceeded".into()),
            message: Some("slow".into()),
            status: Some(429),
            request_id: None,
            error_code: None,
        })
    );
    let refused_block = json!({"type": "error", "error": {"type": "invalid_request_error", "message": "The compaction block's signature is invalid.", "details": {"error_code": "compaction_signature_invalid"}}});
    assert_eq!(
        error_object(&refused_block).and_then(|o| o.error_code),
        Some("compaction_signature_invalid".to_owned())
    );
    for not_an_error in [
        json!({"choices": [{"delta": {"content": "hi"}}]}),
        json!({"error": null}),
        json!({"type": "message_start"}),
        json!("error"),
    ] {
        assert_eq!(error_object(&not_an_error), None, "{not_an_error}");
    }
    let out_of_range = json!({"error": {"type": "x", "status": 200}});
    assert_eq!(error_object(&out_of_range).and_then(|o| o.status), None);
}

#[test]
fn an_error_object_in_a_200_is_classified_by_type_then_status() {
    use ErrorKind as K;
    let cases = [
        (
            json!({"error": {"status": 502, "message": "bad gateway"}}),
            K::Server,
            RetryClass::Transient,
            true,
        ),
        (
            json!({"error": {"status": 503, "type": "overloaded_error"}}),
            K::Overloaded,
            RetryClass::Transient,
            false,
        ),
        (
            json!({"error": {"status": 429}}),
            K::RateLimit,
            RetryClass::Transient,
            false,
        ),
        (
            json!({"error": {"status": 529}}),
            K::Overloaded,
            RetryClass::Transient,
            false,
        ),
        (
            json!({"type": "error", "error": {"type": "api_error", "message": "x"}}),
            K::Server,
            RetryClass::Transient,
            true,
        ),
        (
            json!({"type": "error", "error": {"type": "overloaded_error"}}),
            K::Overloaded,
            RetryClass::Transient,
            false,
        ),
        (
            json!({"type": "error", "code": "rate_limit_exceeded", "message": "x"}),
            K::RateLimit,
            RetryClass::Transient,
            false,
        ),
        (
            json!({"error": {"type": "insufficient_quota"}}),
            K::Billing,
            RetryClass::Never,
            false,
        ),
        (
            json!({"error": {"code": "invalid_api_key"}}),
            K::Auth,
            RetryClass::Never,
            false,
        ),
        (
            json!({"error": {"type": "invalid_request_error", "code": "context_length_exceeded"}}),
            K::ContextOverflow,
            RetryClass::Never,
            false,
        ),
        (
            json!({"error": {"type": "timeout_error"}}),
            K::Timeout,
            RetryClass::Transient,
            true,
        ),
        (
            json!({"error": "text only"}),
            K::InvalidResponse,
            RetryClass::Never,
            true,
        ),
        (
            json!({"error": {"message": "text only"}}),
            K::InvalidResponse,
            RetryClass::Never,
            true,
        ),
        (
            json!({"type": "error", "code": "invalid_request", "message": "prompt is too long"}),
            K::InvalidRequest,
            RetryClass::Never,
            false,
        ),
        (
            json!({"error": {"type": "invalid_request"}}),
            K::InvalidRequest,
            RetryClass::Never,
            false,
        ),
        (
            json!({"error": {"type": "provider_error", "code": "invalid_request"}}),
            K::InvalidRequest,
            RetryClass::Never,
            false,
        ),
        (
            json!({"error": {"type": "invalid_request", "code": "provider_error"}}),
            K::InvalidRequest,
            RetryClass::Never,
            false,
        ),
        (
            json!({"error": {"type": "invalid_request", "code": "rate_limit_exceeded"}}),
            K::RateLimit,
            RetryClass::Transient,
            false,
        ),
        (
            json!({"error": {"type": "invalid_request", "status": 503}}),
            K::Server,
            RetryClass::Transient,
            true,
        ),
        (
            json!({"error": {"type": "invalid_request", "status": 429}}),
            K::RateLimit,
            RetryClass::Transient,
            false,
        ),
        (
            json!({"error": {"type": "server_error", "code": "invalid_request"}}),
            K::Server,
            RetryClass::Transient,
            true,
        ),
        (
            json!({"error": {"type": "brand_new_error", "message": "?"}}),
            K::Other,
            RetryClass::Never,
            true,
        ),
    ];
    for (body, kind, retry, billed) in cases {
        let object = error_object(&body).unwrap();
        let whole = body_error(&object, FailurePhase::Response, Some("req_h"));
        assert_eq!(whole.kind, kind, "{body}");
        assert_eq!(whole.detail.retry, retry, "{body}");
        assert_eq!(whole.detail.possibly_billed, billed, "{body}");
        assert_eq!(whole.detail.phase, FailurePhase::Response);
        assert_eq!(whole.detail.source, FailureSource::Endpoint);
        assert_eq!(whole.detail.request_id.as_deref(), Some("req_h"));
        let after_events = body_error(&object, FailurePhase::Stream, None);
        assert_eq!(after_events.kind, kind, "{body}");
        assert_eq!(after_events.detail.retry, retry, "{body}");
        if kind == K::InvalidRequest {
            assert_eq!(
                after_events.detail.error_type.as_deref(),
                Some("invalid_request")
            );
        }
        assert!(after_events.detail.possibly_billed, "{body}");
        assert_eq!(after_events.detail.phase, FailurePhase::Stream);
    }
}

#[test]
fn a_generic_invalid_request_type_never_overrides_a_more_specific_status() {
    let missing_key = json!({"error": {"message": "You didn't provide an API key.", "type": "invalid_request_error", "code": null, "param": null}});
    let c = classify_http(401, &HeaderMap::new(), &missing_key);
    assert_eq!(c.kind, ErrorKind::Auth);
    assert_eq!(
        c.detail.error_type.as_deref(),
        Some("invalid_request_error")
    );
    for (status, kind) in [
        (403, ErrorKind::Auth),
        (402, ErrorKind::Billing),
        (429, ErrorKind::RateLimit),
        (500, ErrorKind::Server),
        (400, ErrorKind::InvalidRequest),
        (404, ErrorKind::InvalidRequest),
    ] {
        let body = json!({"error": {"type": "invalid_request_error", "message": "m"}});
        assert_eq!(
            classify_http(status, &HeaderMap::new(), &body).kind,
            kind,
            "{status}"
        );
        let in_body = json!({"error": {"type": "invalid_request_error", "status": status}});
        let object = error_object(&in_body).unwrap();
        assert_eq!(
            body_error(&object, FailurePhase::Response, None).kind,
            kind,
            "{status}"
        );
    }
    let statusless = error_object(&json!({"error": {"type": "invalid_request_error"}})).unwrap();
    assert_eq!(
        body_error(&statusless, FailurePhase::Response, None).kind,
        ErrorKind::InvalidRequest
    );
}

#[test]
fn a_prompt_that_is_too_long_is_an_overflow_in_every_transport() {
    let too_long = json!({"type": "error", "error": {"type": "invalid_request_error", "message": "prompt is too long: 210000 tokens > 200000 maximum"}});
    let other = json!({"type": "error", "error": {"type": "invalid_request_error", "message": "messages: roles must alternate"}});
    let cases = [
        (&too_long, ErrorKind::ContextOverflow),
        (&other, ErrorKind::InvalidRequest),
    ];
    for (body, kind) in cases {
        let http = classify_http(400, &HeaderMap::new(), body);
        assert_eq!(http.kind, kind, "{body}");
        assert_eq!(http.detail.retry, RetryClass::Never, "{body}");
        let object = error_object(body).unwrap();
        for phase in [FailurePhase::Response, FailurePhase::Stream] {
            let classified = body_error(&object, phase, None);
            assert_eq!(classified.kind, kind, "{body} {phase:?}");
            assert_eq!(classified.detail.retry, RetryClass::Never, "{body}");
        }
    }
}

#[test]
fn only_an_invalid_request_reads_the_message_for_an_overflow() {
    let cases = [
        (
            json!({"error": {"type": "api_error", "message": "prompt is too long"}}),
            ErrorKind::Server,
        ),
        (
            json!({"error": {"type": "invalid_request_error", "status": 429, "message": "prompt is too long"}}),
            ErrorKind::RateLimit,
        ),
        (
            json!({"error": {"message": "Prompt Is Too Long: 5 tokens > 4 maximum", "type": "invalid_request_error"}}),
            ErrorKind::ContextOverflow,
        ),
    ];
    for (body, kind) in cases {
        let object = error_object(&body).unwrap();
        assert_eq!(
            body_error(&object, FailurePhase::Response, None).kind,
            kind,
            "{body}"
        );
    }
}

#[test]
fn a_typeless_400_saying_the_prompt_is_too_long_is_an_overflow() {
    let gateway = json!({"error": {"message": "gateway.BadRequestError: UpstreamException - prompt is too long: 210000 tokens > 200000 maximum", "type": null, "param": null, "code": "400"}});
    let untyped = json!({"error": {"message": "prompt is too long: 5 tokens > 4 maximum"}});
    let http = classify_http(400, &HeaderMap::new(), &gateway);
    assert_eq!(http.kind, ErrorKind::ContextOverflow);
    assert_eq!(http.detail.retry, RetryClass::Never);
    assert_eq!(
        classify_http(400, &HeaderMap::new(), &untyped).kind,
        ErrorKind::ContextOverflow
    );
    let object = error_object(&gateway).unwrap();
    for phase in [FailurePhase::Response, FailurePhase::Stream] {
        assert_eq!(
            body_error(&object, phase, None).kind,
            ErrorKind::ContextOverflow,
            "{phase:?}"
        );
    }
    let cases = [
        (
            classify_http(
                400,
                &HeaderMap::new(),
                &json!({"error": {"message": "messages: roles must alternate", "type": null, "code": "400"}}),
            ),
            ErrorKind::InvalidRequest,
        ),
        (
            classify_http(500, &HeaderMap::new(), &untyped),
            ErrorKind::Server,
        ),
        (
            classify_http(413, &HeaderMap::new(), &untyped),
            ErrorKind::InvalidRequest,
        ),
    ];
    for (classified, kind) in cases {
        assert_eq!(classified.kind, kind);
    }
    let statusless = error_object(&untyped).unwrap();
    assert_eq!(
        body_error(&statusless, FailurePhase::Response, None).kind,
        ErrorKind::InvalidResponse
    );
}

#[test]
fn a_conflict_in_a_200_body_is_not_billed_like_the_409_row() {
    let object =
        error_object(&json!({"error": {"type": "conflict_error", "message": "again"}})).unwrap();
    let c = body_error(&object, FailurePhase::Response, None);
    assert_eq!(c.kind, ErrorKind::Server);
    assert_eq!(c.detail.retry, RetryClass::Transient);
    assert!(!c.detail.possibly_billed);
    assert!(
        !classify_http(409, &HeaderMap::new(), &anthropic("conflict_error"))
            .detail
            .possibly_billed
    );
    let api = error_object(&json!({"error": {"type": "api_error"}})).unwrap();
    assert!(
        body_error(&api, FailurePhase::Response, None)
            .detail
            .possibly_billed
    );
}

#[test]
fn an_unknown_type_is_other_and_keeps_its_type_and_status() {
    let object = error_object(&json!({"error": {"type": "brand_new_error"}})).unwrap();
    let c = body_error(&object, FailurePhase::Stream, None);
    assert_eq!(c.kind, ErrorKind::Other);
    assert_eq!(c.detail.error_type.as_deref(), Some("brand_new_error"));
    assert!(c.detail.status.is_some());
    let http = classify_http(400, &HeaderMap::new(), &anthropic("brand_new_error"));
    assert_eq!(http.kind, ErrorKind::InvalidRequest);
    assert_eq!(http.detail.error_type.as_deref(), Some("brand_new_error"));
    let letters_only = "CANARYmQzNJtwvfSXYhRkxZWb";
    assert!(wisp_protocol::model_looks_like_key(letters_only));
    for (name, recorded) in [
        ("ServiceUnavailableException", "ServiceUnavailableException"),
        ("AIzaCANARYabcdefghijklmnopqrstuvwxyz", "[redacted]"),
        (letters_only, "[redacted]"),
    ] {
        let classified = classify_http(422, &HeaderMap::new(), &anthropic(name));
        assert_eq!(classified.detail.error_type.as_deref(), Some(recorded));
        let error = ProviderError::classified(classified.kind, name, classified.detail);
        assert_eq!(error.message, "[redacted]");
        assert_eq!(error.detail.error_type.as_deref(), Some(recorded));
    }
    let raw = format!("sk-proj-CANARY_ab1_cd2_ef3 {}", "界".repeat(80));
    let scrubbed = format!("[redacted] {}", "界".repeat(53));
    let object = error_object(&anthropic(&raw)).unwrap();
    assert_eq!(object.raw_name(), Some(raw.as_str()));
    assert_eq!(object.recorded_name().as_deref(), Some(scrubbed.as_str()));
    let c = body_error(&object, FailurePhase::Stream, None);
    assert_eq!(c.kind, ErrorKind::Other);
    assert_eq!(c.detail.error_type.as_deref(), Some(scrubbed.as_str()));
    let http = classify_http(400, &HeaderMap::new(), &anthropic(&raw));
    assert_eq!(http.kind, ErrorKind::InvalidRequest);
    assert_eq!(http.detail.error_type.as_deref(), Some(scrubbed.as_str()));

    let code_only = error_object(&json!({"error": {"code": "brand_new_code"}})).unwrap();
    let c = body_error(&code_only, FailurePhase::Stream, None);
    assert_eq!(c.detail.error_type, None);
}

const DOCUMENTED_TYPES: &[&str] = &[
    "invalid_request_error",
    "authentication_error",
    "permission_error",
    "billing_error",
    "not_found_error",
    "request_too_large",
    "rate_limit_error",
    "api_error",
    "overloaded_error",
    "timeout_error",
    "conflict_error",
    "server_error",
    "rate_limit_exceeded",
    "insufficient_quota",
    "invalid_api_key",
    "context_length_exceeded",
    "model_not_found",
    "unsupported_parameter",
    "insufficient_scope",
    "account_not_eligible",
    "usage_limit_reached",
    "usage_limit_exceeded",
    "server_is_overloaded",
    "slow_down",
];

#[test]
fn no_documented_input_comes_out_as_other() {
    for name in DOCUMENTED_TYPES {
        assert!(
            kind_of(name).is_some_and(|k| k != ErrorKind::Other),
            "{name}"
        );
        for body in [
            anthropic(name),
            json!({"error": {"type": name, "message": "m"}}),
            json!({"error": {"code": name, "message": "m"}}),
            json!({"type": "error", "code": name, "message": "m"}),
        ] {
            let object = error_object(&body).unwrap();
            for phase in [FailurePhase::Response, FailurePhase::Stream] {
                assert_ne!(
                    body_error(&object, phase, None).kind,
                    ErrorKind::Other,
                    "{body}"
                );
            }
            for status in [400, 429, 500, 502, 529] {
                assert_ne!(
                    http_error(status, &HeaderMap::new(), Some(&object)).kind,
                    ErrorKind::Other,
                    "{status} {body}"
                );
            }
        }
    }
    for status in 300..=599 {
        assert_ne!(status_kind(status), ErrorKind::Other, "{status}");
        let c = http_error(status, &HeaderMap::new(), None);
        assert_ne!(c.kind, ErrorKind::Other, "{status}");
    }
    for body in [
        json!({"error": "bare"}),
        json!({"error": {"message": "m", "status": 502}}),
        json!({"error": {"message": "m"}}),
    ] {
        let object = error_object(&body).unwrap();
        assert_ne!(
            body_error(&object, FailurePhase::Response, None).kind,
            ErrorKind::Other,
            "{body}"
        );
    }
    for (kind, retry) in [
        (ErrorKind::Network, RetryClass::Transient),
        (ErrorKind::Timeout, RetryClass::Transient),
    ] {
        assert_eq!(retry_for(kind, None), retry);
    }
}

#[test]
fn an_overflow_states_its_limit_where_the_message_gives_one() {
    for (message, limit) in [
        (
            "prompt is too long: 210000 tokens > 200000 maximum",
            Some(200_000),
        ),
        (
            "Prompt is too long: 1,000,123 tokens > 1,000,000 maximum",
            Some(1_000_000),
        ),
        (
            "gateway.BadRequestError: UpstreamException - prompt is too long: 210000 tokens > 200000 maximum",
            Some(200_000),
        ),
        ("prompt is too long", None),
        ("prompt is too long: 210000 tokens > maximum", None),
        ("prompt is too long: 0 tokens > 0 maximum", None),
        ("200000 maximum", None),
    ] {
        assert_eq!(stated_limit(message), limit, "{message}");
    }
}
