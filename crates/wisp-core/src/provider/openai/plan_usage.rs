use super::super::limit::retry_at;
use serde_json::Value;
use wisp_protocol::plan_usage::{
    Availability, Percent, PlanUsage, PlanUsageError, UsageScope, UsageSource, UsageWindow,
    valid_timestamp,
};

use super::OpenAiProvider;

pub(super) fn identity(call: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    call.header("originator", "codex_cli_rs").header(
        "User-Agent",
        "codex_cli_rs/0.0.0 (Unknown 0; unknown) unknown",
    )
}

pub(super) async fn lookup(provider: OpenAiProvider) -> Result<Option<PlanUsage>, PlanUsageError> {
    if !provider.subscription {
        return Ok(None);
    }
    let token = provider
        .credential
        .access_token()
        .await
        .map_err(|error| PlanUsageError::from(&error))?;
    fetch(
        &provider.client,
        &format!("{}/wham/usage", super::CHATGPT_BASE_URL),
        &token,
        provider.clock.as_ref(),
    )
    .await
    .map(Some)
}

async fn fetch(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    clock: &dyn crate::clock::Clock,
) -> Result<PlanUsage, PlanUsageError> {
    let call = identity(
        client
            .get(url)
            .bearer_auth(token)
            .header("Accept", "application/json"),
    );
    let (raw, now) =
        super::super::body::fetch_json(call, PlanUsageError::AccessDenied, clock).await?;
    full(&raw, now)
}

pub(super) fn full(raw: &Value, now: i64) -> Result<PlanUsage, PlanUsageError> {
    let mut windows = Vec::new();
    let rate = &raw["rate_limit"];
    pair(
        rate,
        "codex",
        UsageScope::All,
        UsageSource::UsageEndpoint,
        now,
        &mut windows,
    )?;
    for limit in raw
        .get("additional_rate_limits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(30)
    {
        let Some(model) = limit
            .get("normal_model_slug")
            .and_then(Value::as_str)
            .filter(|model| safe_model(model))
        else {
            continue;
        };
        let mut additional = Vec::new();
        if pair(
            &limit["rate_limit"],
            model,
            UsageScope::Model(model.to_owned()),
            UsageSource::UsageEndpoint,
            now,
            &mut additional,
        )
        .is_ok()
            && !additional
                .iter()
                .any(|new| windows.iter().any(|old| old.key == new.key))
        {
            windows.extend(additional);
        }
    }
    let availability = match (
        rate.get("allowed").and_then(Value::as_bool),
        rate.get("limit_reached").and_then(Value::as_bool),
    ) {
        (_, Some(true)) | (Some(false), _) => Some(Availability::Limited),
        (Some(true), _) => Some(Availability::Allowed),
        _ => None,
    };
    if windows.is_empty() && availability.is_none() {
        return Err(PlanUsageError::InvalidResponse);
    }
    PlanUsage::new(availability, windows)
        .map(|mut snapshot| {
            snapshot.plan_tier = raw
                .get("plan_type")
                .filter(|value| value.is_string())
                .and_then(|value| serde_json::from_value(value.clone()).ok());
            snapshot
        })
        .map_err(|_| PlanUsageError::InvalidResponse)
}

fn safe_model(model: &str) -> bool {
    model.len() <= 64
        && wisp_protocol::check_model_id(model).is_ok()
        && model
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
}

fn present(value: Option<&Value>) -> Option<&Value> {
    value.filter(|value| {
        !value.is_null() && !value.as_str().is_some_and(|text| text.trim().is_empty())
    })
}

fn pair(
    raw: &Value,
    key: &str,
    scope: UsageScope,
    source: UsageSource,
    now: i64,
    windows: &mut Vec<UsageWindow>,
) -> Result<(), PlanUsageError> {
    for bucket in ["primary", "secondary"] {
        let field = if source == UsageSource::UsageEndpoint {
            format!("{bucket}_window")
        } else {
            bucket.to_owned()
        };
        if let Some(raw) = present(raw.get(field))
            && let Some(window) = window(raw, key, &scope, source, now, bucket)?
        {
            windows.push(window);
        }
    }
    Ok(())
}

fn window(
    raw: &Value,
    key: &str,
    scope: &UsageScope,
    source: UsageSource,
    now: i64,
    bucket: &str,
) -> Result<Option<UsageWindow>, PlanUsageError> {
    if !raw.is_object() {
        return Err(PlanUsageError::InvalidResponse);
    }
    let percent = present(raw.get("used_percent"))
        .map(|value| {
            value
                .as_f64()
                .and_then(|value| Percent::try_from(value).ok())
                .ok_or(PlanUsageError::InvalidResponse)
        })
        .transpose()?;
    let duration_field = if source == UsageSource::UsageEndpoint {
        "limit_window_seconds"
    } else {
        "window_minutes"
    };
    let duration = present(raw.get(duration_field))
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| {
                    value.checked_mul(if source == UsageSource::UsageEndpoint {
                        1
                    } else {
                        60
                    })
                })
                .filter(|value| *value <= 31_622_400)
                .ok_or(PlanUsageError::InvalidResponse)
        })
        .transpose()?;
    let reset = present(raw.get("reset_at"))
        .map(|value| {
            value
                .as_i64()
                .filter(|value| valid_timestamp(*value))
                .ok_or(PlanUsageError::InvalidResponse)
        })
        .transpose()?;
    let reset = reset.filter(|value| *value != 0);
    let relative = if source == UsageSource::UsageEndpoint && reset.is_none_or(|reset| reset == 0) {
        present(raw.get("reset_after_seconds"))
            .map(|value| {
                value
                    .as_i64()
                    .filter(|value| *value >= 0)
                    .ok_or(PlanUsageError::InvalidResponse)
            })
            .transpose()?
    } else {
        None
    };
    let no_reset = reset.is_none_or(|value| value == 0) && relative.is_none_or(|value| value == 0);
    if duration.is_none_or(|value| value == 0)
        && percent.is_none_or(|percent| percent.value() == 0.0)
        && no_reset
    {
        return Ok(None);
    }
    if duration == Some(0) {
        return Err(PlanUsageError::InvalidResponse);
    }
    if percent.is_none()
        && duration.is_none()
        && reset.is_none()
        && relative.is_none_or(|value| value == 0)
    {
        return Ok(None);
    }
    let reset = match (reset, relative) {
        (Some(reset), _) => Some(reset),
        (None, Some(seconds)) => Some(
            now.checked_add(seconds)
                .filter(|value| valid_timestamp(*value))
                .ok_or(PlanUsageError::InvalidResponse)?,
        ),
        (None, None) => None,
    };
    let label = wisp_protocol::plan_usage::WINDOWS
        .iter()
        .find(|(seconds, ..)| duration == Some(*seconds))
        .map_or(bucket, |(_, _, label)| *label);
    Ok(Some(UsageWindow {
        key: format!("{key}.{bucket}"),
        label: match scope {
            UsageScope::All => label.to_owned(),
            UsageScope::Model(model) | UsageScope::ModelFamily(model) => {
                super::super::limit::model_label(model, label)
            }
        },
        duration_seconds: duration,
        scope: scope.clone(),
        used_percent: percent,
        resets_at: reset,
        observed_at: now,
        source,
    }))
}

pub(super) fn account_event(raw: &Value, now: i64) -> Option<Result<PlanUsage, PlanUsageError>> {
    if ["metered_limit_name", "limit_name"].iter().any(|key| {
        raw.get(key)
            .is_some_and(|v| !v.is_null() && v.as_str() != Some("codex"))
    }) {
        return None;
    }
    match event(raw, now) {
        Ok(usage) if usage.windows.is_empty() => None,
        result => Some(result),
    }
}

pub(super) fn event(raw: &Value, now: i64) -> Result<PlanUsage, PlanUsageError> {
    let rate_limits = raw
        .get("rate_limits")
        .filter(|value| value.is_object())
        .ok_or(PlanUsageError::InvalidResponse)?;
    let mut windows = Vec::new();
    pair(
        rate_limits,
        "codex",
        UsageScope::All,
        UsageSource::ResponseEvent,
        now,
        &mut windows,
    )?;
    PlanUsage::new(None, windows).map_err(|_| PlanUsageError::InvalidResponse)
}

pub(super) fn headers(
    headers: &reqwest::header::HeaderMap,
    now: i64,
) -> Option<Result<PlanUsage, PlanUsageError>> {
    let mut windows = Vec::new();
    let mut malformed = false;
    for bucket in ["primary", "secondary"] {
        match header_window(headers, bucket, now) {
            Ok(Some(window)) => windows.push(window),
            Ok(None) => {}
            Err(_) => malformed = true,
        }
    }
    if windows.is_empty() {
        return malformed.then_some(Err(PlanUsageError::InvalidResponse));
    }
    Some(PlanUsage::new(None, windows).map_err(|_| PlanUsageError::InvalidResponse))
}

fn header_window(
    headers: &reqwest::header::HeaderMap,
    bucket: &str,
    now: i64,
) -> Result<Option<UsageWindow>, PlanUsageError> {
    let mut fields = serde_json::Map::new();
    for (header, field) in [
        ("used-percent", "used_percent"),
        ("window-minutes", "window_minutes"),
        ("reset-at", "reset_at"),
    ] {
        let Some(value) = headers.get(format!("x-codex-{bucket}-{header}")) else {
            continue;
        };
        let value = value
            .to_str()
            .map_err(|_| PlanUsageError::InvalidResponse)?
            .trim();
        if value.is_empty() {
            continue;
        }
        let value = serde_json::from_str::<Value>(value)
            .ok()
            .filter(Value::is_number)
            .ok_or(PlanUsageError::InvalidResponse)?;
        fields.insert(field.to_owned(), value);
    }
    window(
        &Value::Object(fields),
        "codex",
        &UsageScope::All,
        UsageSource::ResponseHeader,
        now,
        bucket,
    )
}

pub(super) fn refusal(
    raw: &Value,
    headers: &reqwest::header::HeaderMap,
    now: i64,
    detail: &mut wisp_protocol::ErrorDetail,
) -> Option<wisp_protocol::ErrorKind> {
    let error = raw
        .pointer("/response/error")
        .or_else(|| raw.get("error"))
        .unwrap_or(raw);
    let explicit = ["type", "code"]
        .iter()
        .any(|key| error.get(key).and_then(Value::as_str) == Some("usage_limit_reached"));
    let mut headers = headers.clone();
    for name in [
        "x-codex-rate-limit-reached-type",
        "x-codex-primary-used-percent",
        "x-codex-primary-reset-at",
        "x-codex-secondary-used-percent",
        "x-codex-secondary-reset-at",
        "retry-after",
        "retry-after-ms",
    ] {
        if !headers.contains_key(name)
            && let Some(value) = raw
                .get("headers")
                .or_else(|| raw.pointer("/response/headers"))
                .or_else(|| error.get("headers"))
                .and_then(|h| h.get(name))
                .and_then(Value::as_str)
            && let Ok(value) = value.parse()
        {
            headers.insert(name, value);
        }
    }
    let reason = headers
        .get("x-codex-rate-limit-reached-type")
        .and_then(|v| v.to_str().ok())
        .map(str::trim);
    if matches!(
        reason,
        Some("workspace_owner_credits_depleted" | "workspace_member_credits_depleted")
    ) {
        detail.retry = wisp_protocol::RetryClass::Never;
        detail.error_type = None;
        return Some(wisp_protocol::ErrorKind::Billing);
    }
    let header_limit = matches!(
        reason,
        Some(
            "rate_limit_reached"
                | "workspace_owner_usage_limit_reached"
                | "workspace_member_usage_limit_reached"
        )
    );
    if !explicit && !header_limit {
        return None;
    }
    let absolute = error
        .get("resets_at")
        .and_then(Value::as_i64)
        .filter(|at| *at > now && valid_timestamp(*at));
    let relative = error
        .get("resets_in_seconds")
        .and_then(Value::as_u64)
        .and_then(|seconds| i64::try_from(seconds).ok())
        .and_then(|seconds| now.checked_add(seconds))
        .filter(|at| *at > now && valid_timestamp(*at));
    let header_reset = ["primary", "secondary"]
        .into_iter()
        .filter_map(|bucket| {
            let percent = headers
                .get(format!("x-codex-{bucket}-used-percent"))?
                .to_str()
                .ok()?
                .parse::<f64>()
                .ok()?;
            if !percent.is_finite() || percent < 100.0 {
                return None;
            }
            headers
                .get(format!("x-codex-{bucket}-reset-at"))?
                .to_str()
                .ok()?
                .parse::<i64>()
                .ok()
                .filter(|at| *at > now && valid_timestamp(*at))
        })
        .max();
    let duration = error
        .get("limit_window_minutes")
        .and_then(Value::as_u64)
        .and_then(|v| v.checked_mul(60))
        .filter(|v| *v > 0 && *v <= 31_622_400);
    let (reset, source) = if let Some(reset) = absolute.or(relative) {
        (Some(reset), UsageSource::ErrorBody)
    } else if let Some(reset) = retry_at(&headers, now) {
        (Some(reset), UsageSource::RetryAfter)
    } else {
        (
            header_reset,
            if explicit && header_reset.is_none() {
                UsageSource::ErrorBody
            } else {
                UsageSource::ResponseHeader
            },
        )
    };
    super::super::limit::reached(detail, now, reset, source, duration);
    detail.error_type = None;
    Some(wisp_protocol::ErrorKind::RateLimit)
}

#[cfg(test)]
mod refusal_tests {
    use super::*;
    use serde_json::json;
    use wisp_protocol::{ErrorDetail, FailurePhase, FailureSource, RetryClass};

    #[test]
    fn only_successful_usage_projects_the_closed_tier_vocabulary() {
        use wisp_protocol::plan_usage::PlanTier;
        for (wire, tier) in [
            ("free", PlanTier::Free),
            ("go", PlanTier::Go),
            ("plus", PlanTier::Plus),
            ("pro", PlanTier::Pro),
            ("prolite", PlanTier::ProLite),
            ("promax", PlanTier::ProMax),
            ("team", PlanTier::Team),
            (
                "self_serve_business_prolite",
                PlanTier::SelfServeBusinessProLite,
            ),
            (
                "self_serve_business_usage_based",
                PlanTier::SelfServeBusinessUsageBased,
            ),
            ("business", PlanTier::Business),
            ("ent26", PlanTier::Ent26),
            (
                "enterprise_cbp_automation",
                PlanTier::EnterpriseCbpAutomation,
            ),
            (
                "enterprise_cbp_usage_based",
                PlanTier::EnterpriseCbpUsageBased,
            ),
            ("enterprise", PlanTier::Enterprise),
            ("edu", PlanTier::Edu),
            ("edu_plus", PlanTier::EduPlus),
            ("edu_pro", PlanTier::EduPro),
            ("sci", PlanTier::Sci),
            ("sci_plus", PlanTier::SciPlus),
        ] {
            let raw = json!({"rate_limit":{"allowed":true},"plan_type":wire});
            assert_eq!(full(&raw, 1800000000).unwrap().plan_tier, Some(tier));
            assert!(full(&json!({"plan_type":wire}), 1800000000).is_err());
        }
        for value in [
            json!(null),
            json!(17),
            json!({"private":"CANARY"}),
            json!({"pro":null}),
            json!({"pro":5}),
            json!(["pro"]),
            json!("unknown"),
            json!("pro\nCANARY"),
            json!("x".repeat(10000)),
        ] {
            assert_eq!(
                full(
                    &json!({"rate_limit":{"allowed":true},"plan_type":value}),
                    1800000000
                )
                .unwrap()
                .plan_tier,
                None
            );
        }
    }

    #[test]
    fn type_only_and_code_refusals_share_reset_normalization_across_wrappers() {
        let now = 1_800_000_000;
        for field in ["type", "code"] {
            let error = json!({field:"usage_limit_reached", "resets_at":now+270000, "plan_type":"PRIVATE_CANARY"});
            for raw in [
                error.clone(),
                json!({"error":error}),
                json!({"response":{"error":error}}),
            ] {
                let mut detail = ErrorDetail::new(
                    FailurePhase::Stream,
                    FailureSource::Endpoint,
                    RetryClass::Transient,
                );
                assert!(
                    refusal(&raw, &reqwest::header::HeaderMap::new(), now, &mut detail).is_some()
                );
                assert_eq!(detail.retry, RetryClass::After { ms: 270_000_000 });
                assert_eq!(
                    detail.limit_reached.as_ref().unwrap().resets_at,
                    Some(now + 270_000)
                );
                assert!(!serde_json::to_string(&detail).unwrap().contains("CANARY"));
            }
        }
        for raw in [
            json!({"type":"usage_limit_reached"}),
            json!({"type":"usage_limit_reached", "resets_at":now-1}),
        ] {
            let mut detail = ErrorDetail::new(
                FailurePhase::Response,
                FailureSource::Endpoint,
                RetryClass::Transient,
            );
            assert!(refusal(&raw, &reqwest::header::HeaderMap::new(), now, &mut detail).is_some());
            assert_eq!(detail.retry, RetryClass::Never);
            assert_eq!(detail.limit_reached.unwrap().resets_at, None);
        }
    }

    #[test]
    fn reached_headers_keep_billing_distinct_and_only_use_exhausted_windows() {
        let now = 1_800_000_000;
        for reason in [
            "rate_limit_reached",
            "workspace_owner_usage_limit_reached",
            "workspace_member_usage_limit_reached",
            "workspace_owner_credits_depleted",
            "workspace_member_credits_depleted",
            "PRIVATE_CANARY",
        ] {
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert("x-codex-rate-limit-reached-type", reason.parse().unwrap());
            headers.insert("x-codex-primary-used-percent", "100".parse().unwrap());
            headers.insert(
                "x-codex-primary-reset-at",
                (now + 3600).to_string().parse().unwrap(),
            );
            headers.insert("x-codex-secondary-used-percent", "90".parse().unwrap());
            headers.insert(
                "x-codex-secondary-reset-at",
                (now + 600000).to_string().parse().unwrap(),
            );
            let mut detail = ErrorDetail::new(
                FailurePhase::Response,
                FailureSource::Endpoint,
                RetryClass::Transient,
            );
            let kind = refusal(&Value::Null, &headers, now, &mut detail);
            if reason.contains("credits") {
                assert_eq!(kind, Some(wisp_protocol::ErrorKind::Billing));
                assert!(detail.limit_reached.is_none());
                assert_eq!(detail.retry, RetryClass::Never);
            } else if reason == "PRIVATE_CANARY" {
                assert_eq!(kind, None);
            } else {
                assert_eq!(kind, Some(wisp_protocol::ErrorKind::RateLimit));
                assert_eq!(detail.limit_reached.unwrap().resets_at, Some(now + 3600));
                headers.insert("x-codex-secondary-used-percent", "100".parse().unwrap());
                let mut detail = ErrorDetail::new(
                    FailurePhase::Response,
                    FailureSource::Endpoint,
                    RetryClass::Transient,
                );
                refusal(&Value::Null, &headers, now, &mut detail);
                assert_eq!(detail.limit_reached.unwrap().resets_at, Some(now + 600000));
            }
        }
    }

    #[test]
    fn zero_reset_uses_relative_and_empty_header_bucket_is_absent() {
        let now = 1_800_000_000;
        let usage = full(&json!({"rate_limit":{"primary_window":{"used_percent":10,"limit_window_seconds":18000,"reset_at":0,"reset_after_seconds":3600}}}), now).unwrap();
        assert_eq!(usage.windows[0].resets_at, Some(now + 3600));
        let mut raw = reqwest::header::HeaderMap::new();
        raw.insert("x-codex-primary-used-percent", "0".parse().unwrap());
        assert!(headers(&raw, now).is_none_or(|usage| usage.unwrap().windows.is_empty()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_guard::http;
    use serde_json::json;

    const NOW: i64 = 1_800_000_000;

    fn live_headers() -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        for (key, value) in [
            ("x-codex-primary-used-percent", "30"),
            ("x-codex-primary-window-minutes", "10080"),
            ("x-codex-primary-reset-after-seconds", "563765"),
            ("x-codex-primary-reset-at", "1800563765"),
            ("x-codex-secondary-used-percent", "0"),
            ("x-codex-secondary-window-minutes", "0"),
            ("x-codex-secondary-reset-after-seconds", "0"),
            ("x-codex-secondary-reset-at", ""),
            ("x-codex-active-limit", "ACTIVE_CANARY"),
            (
                "x-codex-primary-over-secondary-limit-percent",
                "RATIO_CANARY",
            ),
            ("x-codex-plan-type", "PLAN_CANARY"),
            ("x-codex-credits-balance", "CREDITS_CANARY"),
            ("x-codex-credits-has-credits", "HAS_CANARY"),
            ("x-codex-credits-unlimited", "UNLIMITED_CANARY"),
            ("x-codex-safety-buffering-enabled", "BUFFER_CANARY"),
            ("x-codex-safety-buffering-faster-model", "MODEL_CANARY"),
        ] {
            headers.insert(key, value.parse().unwrap());
        }
        headers.insert(
            "x-codex-turn-state",
            "STATE_CANARY".repeat(65).parse().unwrap(),
        );
        headers
    }

    #[test]
    fn weekly_only_live_headers_keep_the_valid_primary_and_drop_account_fields() {
        let usage = headers(&live_headers(), NOW).unwrap().unwrap();
        assert_eq!(usage.windows.len(), 1);
        let window = &usage.windows[0];
        assert_eq!(window.label, "Weekly");
        assert_eq!(window.duration_seconds, Some(604_800));
        assert_eq!(window.used_percent.unwrap().value(), 30.0);
        assert_eq!(window.resets_at, Some(1_800_563_765));
        assert_eq!(window.source, UsageSource::ResponseHeader);
        assert!(!serde_json::to_string(&usage).unwrap().contains("CANARY"));
    }

    #[test]
    fn a_bad_header_bucket_never_discards_its_valid_sibling() {
        for bad in ["BAD_CANARY", "-1", "1e300", "0"] {
            for (valid, invalid) in [("primary", "secondary"), ("secondary", "primary")] {
                let mut raw = reqwest::header::HeaderMap::new();
                for (field, value) in [
                    ("used-percent", "30"),
                    ("window-minutes", "10080"),
                    ("reset-at", "1800563765"),
                ] {
                    raw.insert(
                        format!("x-codex-{valid}-{field}")
                            .parse::<reqwest::header::HeaderName>()
                            .unwrap(),
                        value.parse().unwrap(),
                    );
                }
                raw.insert(
                    format!("x-codex-{invalid}-used-percent")
                        .parse::<reqwest::header::HeaderName>()
                        .unwrap(),
                    "90".parse().unwrap(),
                );
                raw.insert(
                    format!("x-codex-{invalid}-window-minutes")
                        .parse::<reqwest::header::HeaderName>()
                        .unwrap(),
                    bad.parse().unwrap(),
                );
                let usage = headers(&raw, NOW).unwrap().unwrap();
                assert_eq!(usage.windows.len(), 1, "{bad}");
                assert_eq!(usage.windows[0].key, format!("codex.{valid}"));
            }
        }
    }

    #[test]
    fn zeroed_and_empty_windows_are_absent_across_get_events_and_headers() {
        for empty in [
            json!(null),
            json!({}),
            json!(""),
            json!({"used_percent":"", "limit_window_seconds":"", "reset_at":""}),
            json!({"used_percent":0,"limit_window_seconds":0,"reset_at":"","reset_after_seconds":0}),
            json!({"used_percent":0,"limit_window_seconds":0,"reset_at":0,"reset_after_seconds":0}),
        ] {
            let raw = json!({"rate_limit":{"primary_window":{"used_percent":30,"limit_window_seconds":604800,"reset_at":1800563765},"secondary_window":empty}});
            let usage = full(&raw, NOW).unwrap();
            assert_eq!(usage.windows.len(), 1, "{empty}");
            let absent = full(&json!({"rate_limit":{"allowed":true,"primary_window":empty,"secondary_window":empty}}), NOW).unwrap();
            assert!(absent.windows.is_empty());
            let mut empty_event = empty;
            if let Some(fields) = empty_event.as_object_mut()
                && let Some(duration) = fields.remove("limit_window_seconds")
            {
                fields.insert("window_minutes".into(), duration);
            }
            let raw = json!({"type":"codex.rate_limits","rate_limits":{"primary":{"used_percent":30,"window_minutes":10080,"reset_at":1800563765},"secondary":empty_event}});
            assert_eq!(account_event(&raw, NOW).unwrap().unwrap().windows.len(), 1);
            let raw = json!({"type":"codex.rate_limits","rate_limits":{"primary":empty_event,"secondary":empty_event}});
            assert!(account_event(&raw, NOW).is_none());
        }
        let mut raw = live_headers();
        for field in [
            "used-percent",
            "window-minutes",
            "reset-at",
            "reset-after-seconds",
        ] {
            raw.remove(format!("x-codex-primary-{field}"));
        }
        assert!(headers(&raw, NOW).is_none());
        raw.insert("x-codex-secondary-used-percent", "".parse().unwrap());
        raw.insert("x-codex-secondary-window-minutes", "  ".parse().unwrap());
        assert!(headers(&raw, NOW).is_none());
        let mut no_absolute = live_headers();
        no_absolute.remove("x-codex-primary-reset-at");
        no_absolute.insert(
            "x-codex-primary-reset-after-seconds",
            "IGNORED_CANARY".parse().unwrap(),
        );
        let usage = headers(&no_absolute, NOW).unwrap().unwrap();
        assert_eq!(usage.windows[0].resets_at, None);
        assert!(!serde_json::to_string(&usage).unwrap().contains("CANARY"));
        let usage = full(&json!({"rate_limit":{"primary_window":{"used_percent":0,"limit_window_seconds":604800,"reset_at":1800563765}}}), NOW).unwrap();
        assert_eq!(usage.windows.len(), 1);
        assert_eq!(usage.windows[0].used_percent.unwrap().value(), 0.0);
    }

    fn fixture() -> Value {
        json!({"rate_limit":{"allowed":true,"primary_window":{"used_percent":0.75,"limit_window_seconds":18000,"reset_at":1800003600},"secondary_window":{"used_percent":90,"limit_window_seconds":604800,"reset_at":1800604800}},"account_id":"ACCOUNT_CANARY","user_id":"USER_CANARY","plan_type":"PLAN_CANARY","credits":{"balance":"CREDIT_CANARY"},"promo":"PROMO_CANARY","additional_rate_limits":[{"limit_name":"PRIVATE_NAME_CANARY","metered_feature":"FEATURE_CANARY","rate_limit":{"primary_window":{"used_percent":96,"limit_window_seconds":300,"reset_after_seconds":10}}}]})
    }

    #[test]
    fn optional_buckets_do_not_poison_supported_windows() {
        let mut raw = fixture();
        raw["additional_rate_limits"] = json!([
            {"limit_name":"PRIVATE_NAME_CANARY", "metered_feature":"codex_other", "rate_limit":{"primary_window":{"used_percent":35,"limit_window_seconds":18000}}},
            {"normal_model_slug":"gpt-5.5", "rate_limit":{"primary_window":{"used_percent":20},"secondary_window":"MALFORMED_CANARY"}}
        ]);
        let usage = full(&raw, NOW).unwrap();
        assert_eq!(usage.windows.len(), 2);
        assert!(!serde_json::to_string(&usage).unwrap().contains("CANARY"));
        raw.as_object_mut().unwrap().remove("rate_limit");
        assert_eq!(full(&raw, NOW), Err(PlanUsageError::InvalidResponse));
    }

    #[test]
    fn unrelated_metered_events_are_consumed_without_cache_errors() {
        let mut machine =
            super::super::Machine::new("model".into(), "fake".into(), Default::default(), true);
        let events = machine.apply(json!({"type":"codex.rate_limits","metered_limit_name":"codex_other", "rate_limits":{"primary":{"used_percent":96}},"credits":{"balance":"PRIVATE_CANARY"}})).unwrap();
        assert!(events.is_empty(), "{events:?}");
    }

    #[test]
    fn full_snapshot_keeps_percent_seconds_and_models_but_no_account_fields() {
        let mut raw = fixture();
        let ordinary = full(&raw, NOW).unwrap();
        assert_eq!(ordinary.windows.len(), 2);
        assert!(!serde_json::to_string(&ordinary).unwrap().contains("CANARY"));
        raw["additional_rate_limits"][0]["normal_model_slug"] = json!("gpt-5.5");
        let usage = full(&raw, NOW).unwrap();
        assert_eq!(usage.windows.len(), 3);
        assert_eq!(usage.windows[0].used_percent.unwrap().value(), 0.75);
        assert_eq!(usage.windows[0].duration_seconds, Some(18_000));
        assert_eq!(usage.windows[2].resets_at, Some(NOW + 10));
        assert_eq!(usage.windows[2].scope, UsageScope::Model("gpt-5.5".into()));
        let json = serde_json::to_string(&usage).unwrap();
        assert!(!json.contains("CANARY"), "{json}");
        assert_eq!(serde_json::from_str::<PlanUsage>(&json).unwrap(), usage);
        for (seconds, label) in [(18_000, "gpt-5.5 5-hour"), (604_800, "gpt-5.5 weekly")] {
            raw["additional_rate_limits"][0]["rate_limit"]["primary_window"]["limit_window_seconds"] =
                json!(seconds);
            let usage = full(&raw, NOW).unwrap();
            assert_eq!(usage.windows[2].label, label);
        }
    }

    #[test]
    fn partial_null_malformed_overflow_and_unsupported_shapes_are_safe() {
        for invalid in [json!(101), json!(-1), json!("NaN"), json!(1e300)] {
            let mut raw = fixture();
            raw["rate_limit"]["primary_window"]["used_percent"] = invalid;
            assert_eq!(full(&raw, NOW), Err(PlanUsageError::InvalidResponse));
        }
        for (field, invalid) in [
            ("limit_window_seconds", json!(u64::MAX)),
            ("reset_at", json!(i64::MAX)),
            ("reset_at", json!("2026-10-30T00:00:00Z")),
        ] {
            let mut raw = fixture();
            raw["rate_limit"]["primary_window"][field] = invalid;
            assert_eq!(full(&raw, NOW), Err(PlanUsageError::InvalidResponse));
        }
        assert!(full(&json!({"unknown":{"used_percent":0}}), NOW).is_err());
        let usage = full(&json!({"rate_limit":{"allowed":false,"primary_window":null,"secondary_window":{"used_percent":null,"reset_at":1800000100}}}), NOW).unwrap();
        assert_eq!(usage.availability, Some(Availability::Limited));
        assert_eq!(usage.windows[0].used_percent, None);
    }

    #[test]
    fn event_and_headers_use_minutes_and_discard_credit_and_identity_canaries() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-codex-primary-used-percent", "75".parse().unwrap());
        headers.insert("x-codex-primary-window-minutes", "300".parse().unwrap());
        headers.insert("x-codex-primary-reset-at", "1800001000".parse().unwrap());
        headers.insert("x-codex-credits-balance", "CREDIT_CANARY".parse().unwrap());
        let usage = super::headers(&headers, NOW).unwrap().unwrap();
        assert_eq!(usage.windows[0].duration_seconds, Some(18_000));
        assert!(!serde_json::to_string(&usage).unwrap().contains("CANARY"));
        let raw = json!({"type":"codex.rate_limits","rate_limits":{"primary":{"used_percent":75,"window_minutes":300,"reset_at":1800001000}},"credits":{"balance":"CREDIT_CANARY"},"plan_type":"PLAN_CANARY","promo":"PROMO_CANARY"});
        assert_eq!(
            event(&raw, NOW).unwrap().windows[0].duration_seconds,
            Some(18_000)
        );
        for raw in [
            json!({"type":"codex.rate_limits","rate_limits":null,"credits":{"balance":"CANARY"}}),
            json!({"type":"codex.rate_limits","rate_limits":{"primary":"CANARY"}}),
        ] {
            let mut machine =
                super::super::Machine::new("model".into(), "fake".into(), Default::default(), true);
            let events = machine.apply(raw).unwrap();
            assert!(matches!(
                events.as_slice(),
                [crate::provider::ProviderEvent::PlanUsage(Err(
                    PlanUsageError::InvalidResponse
                ))]
            ));
            assert!(!format!("{events:?}").contains("CANARY"));
        }
    }

    #[tokio::test]
    async fn lookup_uses_only_approved_identity_and_safe_error_outcomes() {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        for (status, extra, body, expected) in [
            ("200 OK", &[][..], fixture().to_string(), None),
            (
                "401 Unauthorized",
                &[][..],
                "PRIVATE_BODY_CANARY".into(),
                Some(PlanUsageError::AuthenticationRequired),
            ),
            (
                "403 Forbidden",
                &[][..],
                "PRIVATE_BODY_CANARY".into(),
                Some(PlanUsageError::AccessDenied),
            ),
            (
                "302 Found",
                &[("Location", "http://127.0.0.1:1/redirect")][..],
                "PRIVATE_BODY_CANARY".into(),
                Some(PlanUsageError::InvalidResponse),
            ),
            (
                "200 OK",
                &[][..],
                "x".repeat(crate::provider::body::LIMIT + 1),
                Some(PlanUsageError::InvalidResponse),
            ),
        ] {
            let server = http::serve(vec![http::Reply::Send(http::response(
                status, extra, &body,
            ))])
            .await;
            let result = fetch(
                &client,
                &server.url,
                "fake-token",
                &crate::clock::StillClock,
            )
            .await;
            if let Some(expected) = expected {
                assert_eq!(result, Err(expected));
            } else {
                let now = crate::clock::Clock::now(&crate::clock::StillClock).as_second();
                assert!(
                    result
                        .as_ref()
                        .unwrap()
                        .windows
                        .iter()
                        .all(|window| window.observed_at == now)
                );
            }
            let request = server.requests.lock().unwrap()[0].to_ascii_lowercase();
            assert!(request.contains("authorization: bearer fake-token"));
            assert!(request.contains("originator: codex_cli_rs"));
            assert!(
                request.contains("user-agent: codex_cli_rs/0.0.0 (unknown 0; unknown) unknown")
            );
            for absent in ["account-id", "fedramp", "cookie", "session-id", "stainless"] {
                assert!(!request.contains(absent));
            }
            assert!(!format!("{result:?}").contains("CANARY"));
        }
        let start = crate::clock::Clock::now(&crate::clock::StillClock);
        let clock = std::sync::Arc::new(crate::clock::ShiftedClock::at(start, None));
        let moved = clock.clone();
        let url = http::gateway(move |_| {
            moved.advance(jiff::SignedDuration::from_secs(60));
            async {
                http::Reply::Send(http::response(
                    "429 Too Many Requests",
                    &[("Retry-After", "120")],
                    "",
                ))
            }
        })
        .await;
        assert_eq!(
            fetch(&client, &url, "fake-token", clock.as_ref()).await,
            Err(PlanUsageError::Throttled {
                retry_at: Some(start.as_second() + 180)
            })
        );
    }
    #[tokio::test]
    async fn lookup_timeout_is_bounded_and_does_not_retry() {
        let mut server = http::serve(vec![http::Reply::Hang]).await;
        let url = server.url.clone();
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let task = tokio::spawn(async move {
            fetch(&client, &url, "fake-token", &crate::clock::StillClock).await
        });
        assert!(matches!(server.next().await, http::Heard::Request(_)));
        tokio::time::pause();
        tokio::time::advance(std::time::Duration::from_secs(11)).await;
        assert_eq!(task.await.unwrap(), Err(PlanUsageError::Network));
    }

    #[test]
    fn retry_after_accepts_seconds_milliseconds_and_http_dates_without_overflow() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("retry-after", "120".parse().unwrap());
        assert_eq!(retry_at(&headers, NOW), Some(NOW + 120));
        headers.insert("retry-after-ms", "1500".parse().unwrap());
        assert_eq!(retry_at(&headers, NOW), Some(NOW + 2));
        headers.remove("retry-after-ms");
        headers.insert(
            "retry-after",
            "Sun, 06 Nov 1994 08:49:37 GMT".parse().unwrap(),
        );
        assert_eq!(retry_at(&headers, NOW), Some(784_111_777));
        for value in ["-1", "NaN", "1e300", "not a date"] {
            headers.insert("retry-after", value.parse().unwrap());
            assert_eq!(retry_at(&headers, NOW), None, "{value}");
        }
    }
    #[test]
    fn token_refresh_transport_errors_remain_retryable_lookup_failures() {
        let map = |kind| {
            PlanUsageError::from(&crate::provider::ProviderError::new(kind, "PRIVATE_CANARY"))
        };
        for kind in [
            wisp_protocol::ErrorKind::Network,
            wisp_protocol::ErrorKind::Timeout,
            wisp_protocol::ErrorKind::Other,
        ] {
            assert_eq!(map(kind), PlanUsageError::Network);
        }
        assert_eq!(
            map(wisp_protocol::ErrorKind::Auth),
            PlanUsageError::AuthenticationRequired
        );
        assert_eq!(
            map(wisp_protocol::ErrorKind::RateLimit),
            PlanUsageError::Throttled { retry_at: None }
        );
    }
}
