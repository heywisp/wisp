use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Value, json};
use wisp_protocol::plan_usage::{
    MAX_USAGE_WINDOWS, Percent, PlanUsage, PlanUsageError, UsageScope, UsageSource, UsageWindow,
    WINDOWS, valid_timestamp, valid_window_name,
};

use super::GeminiProvider;
use super::antigravity::{AvailableModelsResponse, antigravity_headers};

const ALL_MODELS: &str = "All models";

#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct QuotaInfo {
    #[serde(default, rename = "remainingFraction")]
    pub(super) remaining_fraction: Option<f64>,
    #[serde(default, rename = "resetTime")]
    pub(super) reset_time: Option<String>,
}

pub(super) async fn lookup(provider: GeminiProvider) -> Result<Option<PlanUsage>, PlanUsageError> {
    let Some(user_agent) = provider.user_agent.as_deref() else {
        return Ok(None);
    };
    let project = provider
        .project_id(user_agent)
        .await
        .map_err(|error| PlanUsageError::from(&error))?;
    let token = provider
        .credential
        .access_token()
        .await
        .map_err(|error| PlanUsageError::from(&error))?;
    fetch(
        &provider.client,
        &provider.base_url,
        &token,
        provider.clock.as_ref(),
        user_agent,
        is_official_summary_base(&provider.base_url).then_some(project.as_str()),
    )
    .await
    .map(Some)
}

fn is_official_summary_base(base_url: &str) -> bool {
    super::is_antigravity_origin(&crate::endpoints::origin(base_url))
}

async fn fetch(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    clock: &dyn crate::clock::Clock,
    user_agent: &str,
    project: Option<&str>,
) -> Result<PlanUsage, PlanUsageError> {
    let post = |method, body| post_json(client, base_url, token, clock, user_agent, method, body);
    if let Some(project) = project {
        let summary = post("retrieveUserQuotaSummary", json!({"project": project}))
            .await
            .and_then(|(raw, now)| parse_summary_quota(&raw, now));
        match summary {
            Err(PlanUsageError::InvalidResponse | PlanUsageError::AccessDenied) => {}
            other => return other,
        }
    }
    let (raw, now) = post("fetchAvailableModels", json!({})).await?;
    parse_models_quota(&raw, now)
}

async fn post_json(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    clock: &dyn crate::clock::Clock,
    user_agent: &str,
    method: &str,
    body: Value,
) -> Result<(Value, i64), PlanUsageError> {
    let url = format!("{}/v1internal:{method}", base_url.trim_end_matches('/'));
    let call = antigravity_headers(
        client.post(url).header("Accept", "application/json"),
        token,
        user_agent,
    )
    .json(&body);
    super::super::body::fetch_json(call, PlanUsageError::AccessDenied, clock).await
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct QuotaSummary {
    groups: Vec<QuotaGroup>,
    buckets: Vec<Value>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct QuotaGroup {
    buckets: Vec<Value>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct QuotaBucket {
    bucket_id: String,
    window: Option<String>,
    #[serde(flatten)]
    quota: QuotaInfo,
    disabled: bool,
}

fn parse_summary_quota(raw: &Value, now: i64) -> Result<PlanUsage, PlanUsageError> {
    let summary: QuotaSummary =
        serde_json::from_value(raw.clone()).map_err(|_| PlanUsageError::InvalidResponse)?;
    let mut windows = BTreeMap::new();
    for raw in summary
        .groups
        .into_iter()
        .flat_map(|group| group.buckets)
        .chain(summary.buckets)
    {
        let Ok(bucket) = serde_json::from_value::<QuotaBucket>(raw) else {
            continue;
        };
        let Some(suffix) = bucket.bucket_id.strip_prefix(super::ID_PREFIX) else {
            continue;
        };
        if bucket.disabled
            || !valid_window_name(&bucket.bucket_id)
            || windows.contains_key(&bucket.bucket_id)
        {
            continue;
        }
        let standard = match (bucket.bucket_id.as_str(), bucket.window.as_deref()) {
            ("gemini-5h", Some("5h")) => Some(WINDOWS[0]),
            ("gemini-weekly", Some("weekly")) => Some(WINDOWS[1]),
            _ => None,
        };
        let (key, label, duration) = match standard {
            Some((duration, _, label)) => (bucket.bucket_id.clone(), label, Some(duration)),
            None => (format!("quota {suffix}"), ALL_MODELS, None),
        };
        if let Some((_, window)) =
            bucket
                .quota
                .window(&key, label.to_owned(), duration, UsageScope::All, now)
        {
            windows.insert(bucket.bucket_id, window);
        }
    }
    let mut windows: Vec<_> = windows.into_values().collect();
    windows.sort_by_key(|window| window.duration_seconds.is_none());
    windows.truncate(MAX_USAGE_WINDOWS);
    snapshot(windows)
}

const QUOTA_PERCENT_PRECISION: f64 = 1_000_000_000.0;

impl QuotaInfo {
    fn window(
        &self,
        key: &str,
        label: String,
        duration_seconds: Option<u64>,
        scope: UsageScope,
        now: i64,
    ) -> Option<(Option<f64>, UsageWindow)> {
        let remaining_percent =
            self.remaining_fraction
                .filter(|f| f.is_finite())
                .map(|remaining| {
                    let percent = remaining.clamp(0.0, 1.0) * 100.0;
                    let snapped =
                        (percent * QUOTA_PERCENT_PRECISION).round() / QUOTA_PERCENT_PRECISION;
                    if remaining > 0.0 {
                        snapped.max(f64::MIN_POSITIVE)
                    } else {
                        snapped
                    }
                });
        let used_percent = remaining_percent
            .and_then(|remaining| Percent::try_from(100.0 - remaining.ceil()).ok());
        let resets_at = self
            .reset_time
            .as_deref()
            .and_then(|s| s.parse::<jiff::Timestamp>().ok())
            .map(|ts| ts.as_second())
            .filter(|ts| valid_timestamp(*ts) && *ts != 0);
        (used_percent.is_some() || resets_at.is_some()).then(|| {
            (
                remaining_percent,
                UsageWindow {
                    key: key.to_owned(),
                    label,
                    duration_seconds,
                    scope,
                    used_percent,
                    resets_at,
                    observed_at: now,
                    source: UsageSource::UsageEndpoint,
                },
            )
        })
    }
}

pub(super) fn parse_models_quota(raw: &Value, now: i64) -> Result<PlanUsage, PlanUsageError> {
    let parsed: AvailableModelsResponse =
        serde_json::from_value(raw.clone()).map_err(|_| PlanUsageError::InvalidResponse)?;
    let mut quotas = Vec::new();
    for (raw_id, entry) in parsed.models {
        let id = raw_id.strip_prefix("models/").unwrap_or(&raw_id);
        if !super::is_gemini_model(id, &entry) || !valid_window_name(id) {
            continue;
        }
        let Some(quota) = &entry.quota_info else {
            continue;
        };
        quotas.extend(quota.window(
            id,
            clean_label(id),
            None,
            UsageScope::Model(id.to_owned()),
            now,
        ));
    }
    quotas.sort_by(|a, b| a.1.key.cmp(&b.1.key));
    quotas.dedup_by(|a, b| a.1.key == b.1.key);
    if quotas.len() > 1
        && quotas
            .windows(2)
            .all(|pair| pair[0].0 == pair[1].0 && pair[0].1.resets_at == pair[1].1.resets_at)
    {
        quotas.truncate(1);
        if let Some((_, window)) = quotas.first_mut() {
            window.key = "all-models".into();
            window.label = ALL_MODELS.into();
            window.scope = UsageScope::All;
        }
    }
    quotas.truncate(MAX_USAGE_WINDOWS);
    let mut windows: Vec<_> = quotas.into_iter().map(|(_, window)| window).collect();
    unique_labels(&mut windows);
    snapshot(windows)
}

fn snapshot(windows: Vec<UsageWindow>) -> Result<PlanUsage, PlanUsageError> {
    if windows.is_empty() {
        return Err(PlanUsageError::InvalidResponse);
    }
    PlanUsage::new(None, windows).map_err(|_| PlanUsageError::InvalidResponse)
}

fn unique_labels(windows: &mut [UsageWindow]) {
    let mut counts = BTreeMap::new();
    for window in windows.iter() {
        *counts.entry(window.label.clone()).or_insert(0) += 1;
    }
    for window in windows {
        if counts.get(&window.label).is_some_and(|count| *count > 1) {
            window.label.clone_from(&window.key);
        }
    }
}

fn clean_label(id: &str) -> String {
    let stem = id.strip_suffix("-preview").unwrap_or(id);
    stem.split('-')
        .filter(|part| !part.is_empty())
        .map(|part| match part {
            "gemini" => "Gemini".to_owned(),
            "pro" => "Pro".to_owned(),
            "flash" => "Flash".to_owned(),
            "lite" => "Lite".to_owned(),
            other => {
                let mut chars = other.chars();
                match chars.next() {
                    Some(first) if first.is_ascii_alphabetic() => {
                        let mut word = String::new();
                        word.push(first.to_ascii_uppercase());
                        word.push_str(chars.as_str());
                        word
                    }
                    _ => other.to_owned(),
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::SystemClock;
    use crate::test_guard::http;

    #[test]
    fn summary_quota_prefers_grouped_gemini_windows_and_reads_partial_buckets() {
        let reset = "2026-12-01T04:04:17Z";
        let now = 1_760_000_000;
        let payload = json!({
            "groups": [{"buckets": [
                {"bucketId": "gemini-5h", "window": "5h", "remainingFraction": 1},
                {"bucketId": "3p-weekly", "window": "weekly", "remainingFraction": 0.8},
                {"bucketId": "3p-5h", "window": "5h", "remainingFraction": "not a number"},
                {"bucketId": "gemini-disabled", "remainingFraction": 0.2, "disabled": true},
                {"bucketId": "gemini-zero", "resetTime": "1970-01-01T00:00:00Z"},
                {"bucketId": "gemini-missing", "resetTime": "not a timestamp"}
            ]}],
            "buckets": [
                {"bucketId": "gemini-5h", "remainingFraction": 0, "resetTime": reset},
                {"bucketId": "gemini-weekly", "window": "weekly", "resetTime": reset},
                {"bucketId": "gemini-new", "window": "monthly", "remainingFraction": 0.61181027, "resetTime": reset}
            ]
        });
        let usage = parse_summary_quota(&payload, now).unwrap();
        let [short, weekly, unknown] = usage.windows.as_slice() else {
            panic!("expected three Gemini windows");
        };
        let reset = reset.parse::<jiff::Timestamp>().unwrap().as_second();
        assert_eq!(short.key, "gemini-5h");
        assert_eq!(
            (short.label.as_str(), short.duration_seconds),
            ("5-hour", Some(18_000))
        );
        assert_eq!(
            (short.used_percent.map(Percent::value), short.resets_at),
            (Some(0.0), None)
        );
        assert_eq!(weekly.key, "gemini-weekly");
        assert_eq!(
            (weekly.label.as_str(), weekly.duration_seconds),
            ("Weekly", Some(604_800))
        );
        assert_eq!((weekly.used_percent, weekly.resets_at), (None, Some(reset)));
        assert_eq!(
            (unknown.label.as_str(), unknown.duration_seconds),
            ("All models", None)
        );
        assert_eq!(
            (unknown.used_percent.map(Percent::value), unknown.resets_at),
            (Some(38.0), Some(reset))
        );
        assert!(
            usage
                .windows
                .iter()
                .all(|window| window.scope == UsageScope::All && window.observed_at == now)
        );
        for raw in [
            json!({}),
            json!({"buckets": "not a list"}),
            json!({"groups": [{"buckets": "not a list"}]}),
            json!({"buckets": [{"bucketId": "3p-5h", "remainingFraction": 1}]}),
        ] {
            assert_eq!(
                parse_summary_quota(&raw, now),
                Err(PlanUsageError::InvalidResponse)
            );
        }
    }

    #[test]
    fn summary_cap_keeps_known_windows_before_unknown_buckets() {
        let mut buckets: Vec<_> = (0..MAX_USAGE_WINDOWS + 6)
            .map(|i| json!({"bucketId": format!("gemini-{i:03}"), "remainingFraction": 0.8}))
            .collect();
        buckets.extend([
            json!({"bucketId": "gemini-5h", "window": "5h", "remainingFraction": 0.8}),
            json!({"bucketId": "gemini-weekly", "window": "weekly", "remainingFraction": 0.8}),
        ]);
        let usage = parse_summary_quota(&json!({"buckets": buckets}), 1_760_000_000).unwrap();
        assert_eq!(usage.windows.len(), MAX_USAGE_WINDOWS);
        assert!(usage.windows.iter().any(|window| window.key == "gemini-5h"));
        assert!(
            usage
                .windows
                .iter()
                .any(|window| window.key == "gemini-weekly")
        );
    }

    #[test]
    fn parse_models_quota_filters_partner_models_and_orders_deterministically() {
        let payload = json!({
            "models": {
                "gemini-3.1-pro-preview": {
                    "displayName": "Gemini 3.1 Pro Preview",
                    "apiProvider": "API_PROVIDER_GOOGLE_GEMINI",
                    "quotaInfo": {
                        "remainingFraction": 0.75,
                        "resetTime": "2026-11-11T19:00:00Z"
                    }
                },
                "claude-sonnet-4-5": {
                    "displayName": "Claude Sonnet 4.5",
                    "apiProvider": "API_PROVIDER_ANTHROPIC",
                    "quotaInfo": {
                        "remainingFraction": 0.50,
                        "resetTime": "2026-11-11T19:00:00Z"
                    }
                },
                "partner-custom-v1": {
                    "displayName": "Partner Model",
                    "quotaInfo": {
                        "remainingFraction": 0.20
                    }
                },
                "gemini-3-flash-preview": {
                    "displayName": "Gemini 3 Flash (Preview)",
                    "quotaInfo": {
                        "remainingFraction": 0.90,
                        "resetTime": "2026-11-11T19:00:00Z"
                    }
                }
            }
        });
        let usage = parse_models_quota(&payload, 1_760_000_000).unwrap();
        assert_eq!(usage.windows.len(), 2);
        assert_eq!(usage.windows[0].key, "gemini-3-flash-preview");
        assert_eq!(usage.windows[0].label, "Gemini 3 Flash");
        assert_eq!(
            usage.windows[0].scope,
            UsageScope::Model("gemini-3-flash-preview".into())
        );
        assert_eq!(usage.windows[0].duration_seconds, None);
        assert!((usage.windows[0].used_percent.unwrap().value() - 10.0).abs() < 1e-6);

        assert_eq!(usage.windows[1].key, "gemini-3.1-pro-preview");
        assert_eq!(usage.windows[1].label, "Gemini 3.1 Pro");
        assert!((usage.windows[1].used_percent.unwrap().value() - 25.0).abs() < 1e-6);
    }

    #[test]
    fn parse_models_quota_clamps_out_of_range_fractions() {
        for (remaining, expected) in [
            (1.35, 0.0),
            (-0.25, 100.0),
            (0.90, 10.0),
            (0.07, 93.0),
            (0.2501, 74.0),
            (0.25, 75.0),
            (0.1001, 89.0),
            (0.10, 90.0),
            (0.0501, 94.0),
            (0.05, 95.0),
            (0.0001, 99.0),
            (f64::MIN_POSITIVE, 99.0),
            (0.0, 100.0),
        ] {
            let payload = json!({"models": {
                "gemini-2.5-flash": {"quotaInfo": {"remainingFraction": remaining}}
            }});
            let usage = parse_models_quota(&payload, 1_760_000_000).unwrap();
            assert_eq!(usage.windows.len(), 1);
            let window = &usage.windows[0];
            assert_eq!(
                window.used_percent.unwrap().value(),
                expected,
                "{remaining}"
            );
            assert_eq!(window.scope, UsageScope::Model("gemini-2.5-flash".into()));
        }
    }

    #[test]
    fn parse_models_quota_collapses_only_a_shared_limit() {
        let mut models = serde_json::Map::new();
        for (id, remaining) in [
            ("gemini-2.5-flash", 0.37),
            ("gemini-2.5-pro", 0.37 + f64::EPSILON),
            ("gemini-3.5-flash-lite", 0.37),
        ] {
            models.insert(
                id.into(),
                json!({"quotaInfo": {
                    "remainingFraction": remaining, "resetTime": "2026-11-12T19:00:00Z"
                }}),
            );
        }
        let now = 1_760_000_000;
        let usage = parse_models_quota(&json!({"models": models}), now).unwrap();
        assert_eq!(usage.windows.len(), 1);
        let window = &usage.windows[0];
        assert_eq!(window.label, "All models");
        assert_eq!(window.scope, UsageScope::All);
        assert_eq!(window.used_percent.unwrap().value(), 63.0);
        assert_eq!(window.resets_at, Some(1_794_510_000));
        assert_eq!(window.duration_seconds, None);
        assert_eq!(window.observed_at, now);
        assert_eq!(window.source, UsageSource::UsageEndpoint);
        assert!(window.scope.applies_to("gemini-2.5-flash"));
        assert!(window.scope.applies_to("gemini-4-carbon"));
        models.remove("gemini-2.5-flash");
        let fewer = parse_models_quota(&json!({"models": models}), now).unwrap();
        assert_eq!(fewer.windows[0].key, window.key);
    }

    #[test]
    fn parse_models_quota_keeps_different_limits_model_scoped() {
        for quota in [
            json!({"remainingFraction": 0.60, "resetTime": "2026-11-12T19:00:00Z"}),
            json!({"remainingFraction": 0.4599, "resetTime": "2026-11-12T19:00:00Z"}),
            json!({"remainingFraction": 0.4534, "resetTime": "2026-11-13T19:00:00Z"}),
            json!({"resetTime": "2026-11-12T19:00:00Z"}),
            json!({"remainingFraction": 0.4534}),
        ] {
            let shared = json!({"quotaInfo": {
                "remainingFraction": 0.4534, "resetTime": "2026-11-12T19:00:00Z"
            }});
            let payload = json!({"models": {
                "gemini-2.5-flash": shared,
                "gemini-2.5-pro": shared,
                "gemini-3.5-flash-lite": {"quotaInfo": quota}
            }});
            let usage = parse_models_quota(&payload, 1_760_000_000).unwrap();
            assert_eq!(usage.windows.len(), 3, "{quota}");
            assert_eq!(usage.windows[0].used_percent.unwrap().value(), 54.0);
            for window in usage.windows {
                assert_eq!(window.scope, UsageScope::Model(window.key.clone()));
                assert!(window.scope.applies_to(&window.key));
                assert!(!window.scope.applies_to("gemini-4-carbon"));
            }
        }
    }

    #[test]
    fn parse_models_quota_disambiguates_colliding_labels() {
        let base = "gemini-4-carbon";
        let preview = format!("{base}-preview");
        let mut models = serde_json::Map::new();
        for (index, id) in [
            "gemini-2.5-flash",
            "gemini-2.5-flash-lite",
            "gemini-3.5-flash-lite",
            base,
            &preview,
        ]
        .into_iter()
        .enumerate()
        {
            models.insert(
                id.into(),
                json!({
                    "displayName": "Wrong service label",
                    "quotaInfo": {"remainingFraction": (index as f64 + 1.0) / 10.0}
                }),
            );
        }
        let usage = parse_models_quota(&json!({"models": models}), 1_760_000_000).unwrap();
        let labels: Vec<_> = usage
            .windows
            .iter()
            .map(|window| window.label.as_str())
            .collect();
        assert_eq!(
            labels,
            [
                "Gemini 2.5 Flash",
                "Gemini 2.5 Flash Lite",
                "Gemini 3.5 Flash Lite",
                base,
                &preview,
            ]
        );
    }

    #[test]
    fn clean_label_uses_id_and_strips_preview_suffix() {
        for (id, label) in [
            ("gemini-3.1-pro-preview", "Gemini 3.1 Pro"),
            ("gemini-3-flash-preview", "Gemini 3 Flash"),
            ("gemini-2.5-pro", "Gemini 2.5 Pro"),
            ("gemini-4-carbon", "Gemini 4 Carbon"),
        ] {
            assert_eq!(clean_label(id), label, "{id}");
        }
    }

    #[test]
    fn parse_models_quota_uses_id_labels_and_skips_bad_keys() {
        let long_id = format!("gemini-{}", "x".repeat(95));
        let payload = json!({
            "models": {
                "gemini-4-carbon-medium": {
                    "displayName": "Gemini 4 Carbon (Medium)",
                    "apiProvider": "API_PROVIDER_GOOGLE_GEMINI",
                    "quotaInfo": {
                        "remainingFraction": 0.70,
                        "resetTime": "2026-11-11T19:00:00Z"
                    }
                },
                "gemini-4-argon-high": {
                    "displayName": "Gemini 4 Argon (High) (Preview) (Experimental)",
                    "apiProvider": "API_PROVIDER_GOOGLE_GEMINI",
                    "quotaInfo": {
                        "remainingFraction": 0.55,
                        "resetTime": "2026-11-11T19:00:00Z"
                    }
                },
                "gemini-3.8-flash-low": {
                    "displayName": "Gemini 3.8 Flash (Low)",
                    "quotaInfo": {
                        "remainingFraction": 0.85,
                        "resetTime": "2026-11-11T19:00:00Z"
                    }
                },
                "gemini-4-carbon": {
                    "quotaInfo": {
                        "remainingFraction": 0.95
                    }
                },
                "gemini-4-carbon:colon": {
                    "displayName": "Malformed Key Colon",
                    "quotaInfo": {
                        "remainingFraction": 0.50
                    }
                },
                long_id: {
                    "displayName": "Malformed Key Too Long",
                    "quotaInfo": {
                        "remainingFraction": 0.50
                    }
                }
            }
        });
        let usage = parse_models_quota(&payload, 1_760_000_000).unwrap();
        let pairs: Vec<(&str, &str)> = usage
            .windows
            .iter()
            .map(|w| (w.key.as_str(), w.label.as_str()))
            .collect();
        assert_eq!(pairs.len(), 4);
        assert_eq!(pairs[0], ("gemini-3.8-flash-low", "Gemini 3.8 Flash Low"));
        assert_eq!(pairs[1], ("gemini-4-argon-high", "Gemini 4 Argon High"));
        assert_eq!(pairs[2], ("gemini-4-carbon", "Gemini 4 Carbon"));
        assert_eq!(
            pairs[3],
            ("gemini-4-carbon-medium", "Gemini 4 Carbon Medium")
        );

        let mut crowded = serde_json::Map::new();
        crowded.insert(
            "models/gemini-4-carbon".into(),
            json!({"quotaInfo": {"remainingFraction": 0.80}}),
        );
        crowded.insert(
            "gemini-4-carbon".into(),
            json!({"quotaInfo": {"remainingFraction": 0.80}}),
        );
        for idx in 0..MAX_USAGE_WINDOWS + 6 {
            crowded.insert(
                format!("gemini-4-extra-{idx:03}"),
                json!({"quotaInfo": {"remainingFraction": 0.80}}),
            );
        }
        crowded.insert(
            "gemini-4-extra-999".into(),
            json!({"quotaInfo": {"remainingFraction": 0.60}}),
        );
        let capped = parse_models_quota(&json!({"models": crowded}), 1_760_000_000).unwrap();
        assert_eq!(capped.windows.len(), MAX_USAGE_WINDOWS);
        assert_eq!(
            capped
                .windows
                .iter()
                .filter(|w| w.key == "gemini-4-carbon")
                .count(),
            1
        );
        for pair in capped.windows.windows(2) {
            assert!(pair[0].key < pair[1].key);
        }
    }

    const TOKEN: &str = "quota-CANARY";
    const PROJECT: &str = "fixture-project";
    const MODEL_QUOTA: &str =
        r#"{"models":{"gemini-3.1-pro-preview":{"quotaInfo":{"remainingFraction":0.6}}}}"#;

    #[test]
    fn quota_summary_requires_an_official_antigravity_origin() {
        for base in [
            super::super::antigravity_base_url(),
            super::super::PRODUCTION_ANTIGRAVITY_ORIGIN,
        ] {
            let base = base.trim_end_matches('/');
            for suffix in ["", "/", "/path/to/api/"] {
                let url = format!("{base}{suffix}");
                assert!(is_official_summary_base(&url), "{url}");
            }
            let mut nondefault_port = reqwest::Url::parse(base).unwrap();
            nondefault_port.set_port(Some(444)).unwrap();
            assert!(!is_official_summary_base(nondefault_port.as_str()));
        }
        for url in [
            "https://other.googleapis.com",
            "http://127.0.0.1:18080",
            "http://localhost:8080",
            "http://[::1]:8080",
            "not a URL",
        ] {
            assert!(!is_official_summary_base(url), "{url}");
        }
    }

    #[tokio::test]
    async fn quota_lookup_does_not_send_the_cached_project_to_an_override() {
        use crate::endpoints::{
            EndpointName, PathShown, PricingMode, ProviderKind, ResolvedEndpoint, UrlSource,
        };
        use crate::provider::credential::{Credential, Source, SubscriptionKind};

        let server = http::serve(vec![
            http::Reply::Send(http::json("200 OK", MODEL_QUOTA)),
            http::Reply::Send(http::json("200 OK", MODEL_QUOTA)),
        ])
        .await;
        let provider = GeminiProvider::from_endpoint(&ResolvedEndpoint {
            name: EndpointName::parse("gemini-fixture").unwrap(),
            provider: ProviderKind::Gemini,
            credential: Credential::Subscription {
                kind: SubscriptionKind::Antigravity,
                secret: TOKEN.into(),
                profile: None,
            },
            base_url: server.url.clone(),
            source: Source::Environment,
            key_env: None,
            url_source: UrlSource::Config {
                path: "fixture.toml".into(),
                shown: PathShown::Given,
            },
            pricing: PricingMode::List,
            client_version: None,
        })
        .unwrap();
        *provider.project_cache.lock().unwrap() = Some(PROJECT.to_owned());
        let usage = lookup(provider).await.unwrap().unwrap();
        let requests = server.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("POST /v1internal:fetchAvailableModels "));
        assert_eq!(http::body(&requests[0]), "{}");
        assert!(!requests[0].contains(PROJECT));
        assert_eq!(usage.windows.len(), 1);
        assert_eq!(usage.windows[0].key, "gemini-3.1-pro-preview");
        assert_eq!(
            usage.windows[0].used_percent.map(Percent::value),
            Some(40.0)
        );
    }

    #[tokio::test]
    async fn fetch_posts_project_with_antigravity_headers_and_parses_summary() {
        let mut server = http::serve(vec![
            http::Reply::Send(http::json(
                "200 OK",
                r#"{"buckets":[{"bucketId":"gemini-5h","window":"5h","remainingFraction":0.6}]}"#,
            )),
            http::Reply::Send(http::json("200 OK", MODEL_QUOTA)),
        ])
        .await;
        let client = reqwest::Client::new();
        let user_agent = super::super::antigravity::shipped_user_agent();
        let usage = fetch(
            &client,
            &server.url,
            TOKEN,
            &SystemClock,
            &user_agent,
            Some(PROJECT),
        )
        .await
        .unwrap();
        assert_eq!(usage.windows.len(), 1);
        assert_eq!(usage.windows[0].key, "gemini-5h");
        assert_eq!(
            usage.windows[0].used_percent.map(Percent::value),
            Some(40.0)
        );
        let http::Heard::Request(raw) = server.next().await else {
            panic!("expected request");
        };
        assert!(raw.starts_with("POST /v1internal:retrieveUserQuotaSummary "));
        assert_eq!(
            serde_json::from_str::<Value>(http::body(&raw)).unwrap(),
            json!({"project": PROJECT})
        );
        let headers = http::header_lines(&raw);
        for (name, value) in [
            ("authorization", "Bearer quota-CANARY"),
            ("accept", "application/json"),
            ("content-type", "application/json"),
            ("user-agent", user_agent.as_str()),
        ] {
            let mut values = headers
                .iter()
                .filter_map(|(key, val)| (key == name).then_some(val.as_str()));
            assert!(
                values.next() == Some(value) && values.next().is_none(),
                "header {name}"
            );
        }
        assert_eq!(server.requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn fetch_uses_fallback_only_for_an_invalid_or_refused_summary() {
        use PlanUsageError::{
            AccessDenied, AuthenticationRequired, InvalidResponse, Network, Throttled,
        };
        let client = reqwest::Client::new();
        let user_agent = super::super::antigravity::shipped_user_agent();
        for (status, body, fallback, expected, tried_fallback) in [
            ("400 Bad Request", "{}", "200 OK", Ok(()), true),
            ("404 Not Found", "{}", "200 OK", Ok(()), true),
            ("503 Service Unavailable", "{}", "200 OK", Ok(()), true),
            ("200 OK", "{", "200 OK", Ok(()), true),
            ("200 OK", "{}", "200 OK", Ok(()), true),
            ("403 Forbidden", "{}", "200 OK", Ok(()), true),
            (
                "404 Not Found",
                "{}",
                "403 Forbidden",
                Err(AccessDenied),
                true,
            ),
            (
                "403 Forbidden",
                "{}",
                "404 Not Found",
                Err(InvalidResponse),
                true,
            ),
            (
                "401 Unauthorized",
                "{}",
                "200 OK",
                Err(AuthenticationRequired),
                false,
            ),
            (
                "429 Too Many Requests",
                "{}",
                "200 OK",
                Err(Throttled { retry_at: None }),
                false,
            ),
            ("", "", "200 OK", Err(Network), false),
            ("truncated", "{", "200 OK", Err(Network), false),
        ] {
            let first = match status {
                "" => String::new(),
                "truncated" => format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 64\r\nConnection: close\r\n\r\n{body}"
                ),
                _ => http::json(status, body),
            };
            let server = http::serve(vec![
                http::Reply::Send(first),
                http::Reply::Send(http::json(fallback, MODEL_QUOTA)),
            ])
            .await;
            let found = fetch(
                &client,
                &server.url,
                TOKEN,
                &SystemClock,
                &user_agent,
                Some(PROJECT),
            )
            .await;
            if let Ok(usage) = &found {
                let window = &usage.windows[0];
                assert_eq!(window.key, "gemini-3.1-pro-preview");
                assert_eq!(window.duration_seconds, None);
                assert_eq!(window.used_percent.map(Percent::value), Some(40.0));
            }
            assert_eq!(found.map(|_| ()), expected, "{status} {body}");
            let requests = server.requests.lock().unwrap();
            assert_eq!(
                requests.len(),
                if tried_fallback { 2 } else { 1 },
                "{status} {body}"
            );
            assert!(requests[0].starts_with("POST /v1internal:retrieveUserQuotaSummary "));
            if tried_fallback {
                assert!(requests[1].starts_with("POST /v1internal:fetchAvailableModels "));
                assert_eq!(http::body(&requests[1]), "{}");
                let headers = |raw| {
                    http::header_lines(raw)
                        .into_iter()
                        .filter(|(k, _)| k != "content-length")
                        .collect::<BTreeMap<_, _>>()
                };
                assert_eq!(headers(&requests[0]), headers(&requests[1]));
            }
        }
    }
}
