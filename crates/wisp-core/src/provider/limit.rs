use reqwest::header::HeaderMap;
use wisp_protocol::plan_usage::{
    RETRY_AFTER_KEY, UsageScope, UsageSource, UsageWindow, valid_timestamp,
};
use wisp_protocol::{ErrorDetail, RetryClass};

pub(super) fn model_label(model: &str, period: &str) -> String {
    format!("{model} {}", period.to_ascii_lowercase())
}

pub(super) fn retry_at(headers: &HeaderMap, now: i64) -> Option<i64> {
    super::classify::retry_after_ms(headers)
        .and_then(|ms| i64::try_from(ms.div_ceil(1000)).ok())
        .and_then(|seconds| now.checked_add(seconds))
        .or_else(|| {
            jiff::civil::DateTime::strptime(
                "%a, %d %b %Y %H:%M:%S GMT",
                headers.get("retry-after")?.to_str().ok()?,
            )
            .ok()?
            .to_zoned(jiff::tz::TimeZone::UTC)
            .ok()
            .map(|time| time.timestamp().as_second())
        })
        .filter(|at| valid_timestamp(*at))
}

pub(crate) fn reached(
    detail: &mut ErrorDetail,
    now: i64,
    reset: Option<i64>,
    source: UsageSource,
    duration_seconds: Option<u64>,
) {
    let reset = reset.filter(|at| *at > now && valid_timestamp(*at));
    detail.retry = reset
        .and_then(|at| u64::try_from(at.checked_sub(now)?).ok()?.checked_mul(1000))
        .map_or(RetryClass::Never, |ms| RetryClass::After { ms });
    detail.limit_reached = Some(Box::new(UsageWindow {
        key: "subscription-limit".into(),
        label: "Subscription limit".into(),
        duration_seconds,
        scope: UsageScope::All,
        used_percent: None,
        resets_at: reset,
        observed_at: now,
        source,
    }));
}

pub(super) fn long_retry(
    detail: &mut ErrorDetail,
    headers: &HeaderMap,
    body_ms: Option<u64>,
    now: i64,
) {
    let reset = retry_at(headers, now);
    let ms = super::classify::retry_after_ms(headers).or_else(|| {
        reset?
            .checked_sub(now)
            .and_then(|v| u64::try_from(v).ok())
            .and_then(|v| v.checked_mul(1000))
    });
    let source = if ms.is_some() {
        UsageSource::RetryAfter
    } else {
        UsageSource::ErrorBody
    };
    let reset = reset.or_else(|| {
        now.checked_add(i64::try_from(body_ms?.div_ceil(1000)).ok()?)
            .filter(|at| valid_timestamp(*at))
    });
    if let Some(ms) = ms.or(body_ms) {
        if ms > 60_000 && reset.is_some() {
            reached(detail, now, reset, source, None);
        } else {
            detail.retry = RetryClass::After { ms };
        }
    }
}

pub(super) fn retry_guard(detail: &mut ErrorDetail, model: &str) {
    if let Some(window) = detail.limit_reached.as_mut() {
        window.key = RETRY_AFTER_KEY.into();
        window.label = "Retry after".into();
        window.scope = UsageScope::Model(model.to_owned());
        if !window.valid() {
            window.scope = UsageScope::All;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wisp_protocol::{FailurePhase, FailureSource};

    #[test]
    fn a_retry_guard_scopes_only_a_model_it_can_name() {
        let mut detail = ErrorDetail::new(
            FailurePhase::Response,
            FailureSource::Endpoint,
            RetryClass::Transient,
        );
        retry_guard(&mut detail, "example");
        assert!(detail.limit_reached.is_none());
        for len in [96, 97] {
            let model = "m".repeat(len);
            reached(
                &mut detail,
                1_800_000_000,
                Some(1_800_000_100),
                UsageSource::RetryAfter,
                None,
            );
            retry_guard(&mut detail, &model);
            let guard = detail.limit_reached.as_deref().unwrap();
            assert!(guard.is_retry_guard());
            assert_eq!(
                guard.scope,
                if len == 96 {
                    UsageScope::Model(model)
                } else {
                    UsageScope::All
                }
            );
            let encoded = serde_json::to_string(guard).unwrap();
            assert_eq!(
                *guard,
                serde_json::from_str::<UsageWindow>(&encoded).unwrap()
            );
        }
    }

    #[test]
    fn long_retry_after_is_anchored_and_malformed_values_leave_transient_retry() {
        let now = 1_800_000_000;
        for (name, value, body_ms, expected) in [
            ("retry-after", "270000", None, Some(270_000_000)),
            ("retry-after-ms", "270000000", None, Some(270_000_000)),
            ("retry-after", "12", None, Some(12_000)),
            ("retry-after-ms", "1500", None, Some(1500)),
            ("retry-after", "bad", None, None),
            ("", "", Some(270_000_000), Some(270_000_000)),
            ("", "", Some(42_000), Some(42_000)),
            ("", "", Some(60_000), Some(60_000)),
            ("", "", Some(60_001), Some(60_001)),
            ("retry-after", "bad", Some(270_000_000), Some(270_000_000)),
            ("retry-after", "12", Some(270_000_000), Some(12_000)),
            ("retry-after", "270000", Some(42_000), Some(270_000_000)),
        ] {
            let mut headers = HeaderMap::new();
            if !name.is_empty() {
                headers.insert(name, value.parse().unwrap());
            }
            let mut detail = ErrorDetail::new(
                FailurePhase::Response,
                FailureSource::Endpoint,
                RetryClass::Transient,
            );
            long_retry(&mut detail, &headers, body_ms, now);
            assert_eq!(
                detail.retry,
                expected.map_or(RetryClass::Transient, |ms: u64| RetryClass::After {
                    ms: if ms > 60_000 {
                        ms.div_ceil(1000) * 1000
                    } else {
                        ms
                    }
                })
            );
            if let Some(ms) = expected.filter(|ms| *ms > 60_000) {
                let window = detail.limit_reached.unwrap();
                assert_eq!(
                    window.resets_at,
                    Some(now + i64::try_from(ms.div_ceil(1000)).unwrap())
                );
                assert_eq!(
                    window.source,
                    if name.is_empty() || value == "bad" {
                        UsageSource::ErrorBody
                    } else {
                        UsageSource::RetryAfter
                    }
                );
            } else {
                assert!(detail.limit_reached.is_none());
            }
        }
    }
}
