use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;
use wisp_protocol::plan_usage::{
    Percent, PlanUsage, PlanUsageError, UsageScope, UsageSource, UsageWindow, valid_timestamp,
};

use super::GeminiProvider;
use super::antigravity::{AvailableModelsResponse, Identity};

#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct QuotaInfo {
    #[serde(default, rename = "remainingFraction")]
    pub(super) remaining_fraction: Option<f64>,
    #[serde(default, rename = "resetTime")]
    pub(super) reset_time: Option<String>,
}

pub(super) async fn lookup(provider: GeminiProvider) -> Result<Option<PlanUsage>, PlanUsageError> {
    if !provider.subscription {
        return Ok(None);
    }
    let _ = provider
        .project_id()
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
    )
    .await
    .map(Some)
}

pub(super) async fn fetch(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    clock: &dyn crate::clock::Clock,
) -> Result<PlanUsage, PlanUsageError> {
    fetch_with_identity(client, base_url, token, clock, super::antigravity::data()).await
}

async fn fetch_with_identity(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    clock: &dyn crate::clock::Clock,
    identity: Option<&Identity>,
) -> Result<PlanUsage, PlanUsageError> {
    let identity = identity.ok_or(PlanUsageError::Unsupported)?;
    let url = format!(
        "{}/v1internal:fetchAvailableModels",
        base_url.trim_end_matches('/')
    );
    let call = client
        .post(url)
        .bearer_auth(token)
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .header("User-Agent", &identity.user_agent)
        .body("{}");
    let (raw, now) =
        super::super::body::fetch_json(call, PlanUsageError::AccessDenied, clock).await?;
    parse_models_quota(&raw, now)
}

const MAX_LABEL_BYTES: usize = 96;
const QUOTA_PERCENT_PRECISION: f64 = 1_000_000_000.0;

fn valid_window_key(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_LABEL_BYTES
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_. ".contains(&c))
}

pub(super) fn parse_models_quota(raw: &Value, now: i64) -> Result<PlanUsage, PlanUsageError> {
    let parsed: AvailableModelsResponse =
        serde_json::from_value(raw.clone()).map_err(|_| PlanUsageError::InvalidResponse)?;
    let mut quotas = Vec::new();
    for (raw_id, entry) in parsed.models {
        let id = raw_id.strip_prefix("models/").unwrap_or(&raw_id);
        if !super::is_gemini_model(id, &entry) || !valid_window_key(id) {
            continue;
        }
        let Some(quota) = &entry.quota_info else {
            continue;
        };
        let remaining_percent =
            quota
                .remaining_fraction
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
        let resets_at = quota
            .reset_time
            .as_deref()
            .and_then(|s| s.parse::<jiff::Timestamp>().ok())
            .map(|ts| ts.as_second())
            .filter(|ts| valid_timestamp(*ts) && *ts != 0);
        if used_percent.is_none() && resets_at.is_none() {
            continue;
        }
        quotas.push((
            remaining_percent,
            UsageWindow {
                key: id.to_owned(),
                label: clean_label(id),
                duration_seconds: None,
                scope: UsageScope::Model(id.to_owned()),
                used_percent,
                resets_at,
                observed_at: now,
                source: UsageSource::UsageEndpoint,
            },
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
            window.label = "All models".into();
            window.scope = UsageScope::All;
        }
    }
    quotas.truncate(64);
    let mut windows: Vec<_> = quotas.into_iter().map(|(_, window)| window).collect();
    unique_labels(&mut windows);
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
    use serde_json::json;

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
        for idx in 0..70 {
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
        assert_eq!(capped.windows.len(), 64);
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

    #[tokio::test]
    async fn fetch_posts_empty_json_with_antigravity_headers_and_parses_windows() {
        let mut server = http::serve(vec![http::Reply::Send(http::json(
            "200 OK",
            &json!({
                "models": {
                    "gemini-3.1-pro-preview": {
                        "displayName": "Gemini 3.1 Pro",
                        "quotaInfo": {
                            "remainingFraction": 0.6,
                            "resetTime": "2026-11-11T19:00:00Z"
                        }
                    }
                }
            })
            .to_string(),
        ))])
        .await;
        let client = reqwest::Client::new();
        let usage = fetch(&client, &server.url, "tok_quota", &SystemClock)
            .await
            .unwrap();
        assert_eq!(usage.windows.len(), 1);
        assert!((usage.windows[0].used_percent.unwrap().value() - 40.0).abs() < 1e-6);
        let http::Heard::Request(raw) = server.next().await else {
            panic!("expected request");
        };
        assert!(raw.starts_with("POST /v1internal:fetchAvailableModels "));
        assert_eq!(http::body(&raw), "{}");
        let headers = http::header_lines(&raw);
        assert!(
            headers
                .iter()
                .any(|(k, v)| k == "user-agent" && v == super::super::antigravity_user_agent())
        );
    }

    #[tokio::test]
    async fn fetch_maps_401_and_403_to_typed_plan_usage_errors() {
        let server = http::serve(vec![
            http::Reply::Send(http::json("401 Unauthorized", "{}")),
            http::Reply::Send(http::json("403 Forbidden", "{}")),
        ])
        .await;
        let client = reqwest::Client::new();
        let first = fetch(&client, &server.url, "bad_tok", &SystemClock)
            .await
            .unwrap_err();
        assert_eq!(first, PlanUsageError::AuthenticationRequired);
        let second = fetch(&client, &server.url, "forbidden_tok", &SystemClock)
            .await
            .unwrap_err();
        assert_eq!(second, PlanUsageError::AccessDenied);
        let missing = fetch_with_identity(&client, &server.url, "tok", &SystemClock, None)
            .await
            .unwrap_err();
        assert_eq!(missing, PlanUsageError::Unsupported);
    }
}
