use serde_json::Value;
use wisp_protocol::plan_usage::{
    MAX_USAGE_WINDOWS, Percent, PlanUsage, PlanUsageError, UsageScope, UsageSource, UsageWindow,
    WINDOWS, valid_timestamp, valid_window_name,
};

use super::super::limit::model_label;
use super::AnthropicProvider;

pub(super) async fn lookup(
    provider: AnthropicProvider,
) -> Result<Option<PlanUsage>, PlanUsageError> {
    let Some(identity) = provider.rendered.as_ref() else {
        return Ok(None);
    };
    if !provider
        .usage_enabled
        .load(std::sync::atomic::Ordering::Acquire)
    {
        return Err(PlanUsageError::Disabled);
    }
    let token = provider
        .credential
        .access_token()
        .await
        .map_err(|error| PlanUsageError::from(&error))?;
    fetch(
        &provider.client,
        "https://api.anthropic.com/api/oauth/usage",
        &token,
        provider.clock.as_ref(),
        identity,
    )
    .await
    .map(Some)
}

async fn fetch(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    clock: &dyn crate::clock::Clock,
    identity: &super::subscription::Rendered,
) -> Result<PlanUsage, PlanUsageError> {
    let mut call = client
        .get(url)
        .bearer_auth(token)
        .header("Accept", "application/json");
    for (name, value) in &identity.usage_headers {
        call = call.header(name, value);
    }
    let (raw, now) =
        super::super::body::fetch_json(call, PlanUsageError::ScopeRequired, clock).await?;
    decode(&raw, now)
}

fn decode(raw: &Value, now: i64) -> Result<PlanUsage, PlanUsageError> {
    let object = raw.as_object().ok_or(PlanUsageError::InvalidResponse)?;
    let mut windows = Vec::new();
    for (key, raw) in object {
        if raw.is_null() {
            continue;
        }
        let (label, duration_seconds, scope) = match key.as_str() {
            "five_hour" => (WINDOWS[0].2.into(), Some(WINDOWS[0].0), UsageScope::All),
            "seven_day" => (WINDOWS[1].2.into(), Some(WINDOWS[1].0), UsageScope::All),
            "seven_day_oauth_apps" => (
                format!("{} OAuth apps", WINDOWS[1].2),
                Some(WINDOWS[1].0),
                UsageScope::All,
            ),
            _ => {
                let Some(model) = key
                    .strip_prefix("seven_day_")
                    .filter(|name| safe_name(name) && wisp_protocol::valid_model_id(name))
                else {
                    continue;
                };
                if raw.get("utilization").is_none() && raw.get("resets_at").is_none() {
                    continue;
                }
                (
                    model_label(
                        &wisp_tool_text::output::capitalised(model.replace('_', " ")),
                        WINDOWS[1].2,
                    ),
                    Some(WINDOWS[1].0),
                    UsageScope::ModelFamily(model.into()),
                )
            }
        };
        let (used_percent, resets_at) = match reading(raw, "utilization") {
            Ok(reading) => reading,
            Err(error) if scope == UsageScope::All => return Err(error),
            Err(_) => continue,
        };
        windows.push(UsageWindow {
            key: key.into(),
            label,
            duration_seconds,
            scope,
            used_percent,
            resets_at,
            observed_at: now,
            source: UsageSource::UsageEndpoint,
        });
        if windows.len() == MAX_USAGE_WINDOWS {
            break;
        }
    }
    for window in raw
        .get("limits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(MAX_USAGE_WINDOWS)
        .filter_map(|raw| scoped_window(raw, now))
    {
        if let Some(old) = windows.iter_mut().find(|old| old.key == window.key) {
            *old = window;
        } else if windows.len() < MAX_USAGE_WINDOWS {
            windows.push(window);
        }
    }
    if windows.is_empty() {
        return Err(PlanUsageError::InvalidResponse);
    }
    PlanUsage::new(None, windows).map_err(|_| PlanUsageError::InvalidResponse)
}

fn safe_name(name: &str) -> bool {
    valid_window_name(name)
        && name.trim() == name
        && name.len() <= 40
        && super::super::without_secret_text(name) == name
}

fn reading(raw: &Value, field: &str) -> Result<(Option<Percent>, Option<i64>), PlanUsageError> {
    if !raw.is_object() {
        return Err(PlanUsageError::InvalidResponse);
    }
    let used = raw
        .get(field)
        .filter(|v| !v.is_null())
        .map(|v| {
            v.as_f64()
                .and_then(|v| Percent::try_from(v).ok())
                .ok_or(PlanUsageError::InvalidResponse)
        })
        .transpose()?;
    let reset = raw
        .get("resets_at")
        .filter(|v| !v.is_null())
        .map(|v| {
            v.as_str()
                .and_then(|v| v.parse::<jiff::Timestamp>().ok())
                .map(|v| v.as_second())
                .filter(|at| valid_timestamp(*at))
                .ok_or(PlanUsageError::InvalidResponse)
        })
        .transpose()?;
    Ok((used, reset))
}

fn scoped_window(raw: &Value, now: i64) -> Option<UsageWindow> {
    let kind = raw.get("kind")?.as_str().filter(|name| safe_name(name))?;
    let group = raw.get("group")?.as_str().filter(|name| safe_name(name))?;
    let duration_seconds = match group {
        "session" => Some(WINDOWS[0].0),
        "weekly" => Some(WINDOWS[1].0),
        _ => None,
    };
    let (key, label, scope) = match (kind, group) {
        ("session", "session") if raw.get("scope").is_none_or(Value::is_null) => {
            ("five_hour".into(), WINDOWS[0].2.into(), UsageScope::All)
        }
        ("weekly_all", "weekly") if raw.get("scope").is_none_or(Value::is_null) => {
            ("seven_day".into(), WINDOWS[1].2.into(), UsageScope::All)
        }
        _ => {
            let model = raw.pointer("/scope/model")?;
            let id = match model.get("id").filter(|id| !id.is_null()) {
                Some(id) => Some(
                    id.as_str()
                        .filter(|id| safe_name(id) && wisp_protocol::valid_model_id(id))?,
                ),
                None => None,
            };
            let name = model
                .get("display_name")
                .and_then(Value::as_str)
                .filter(|name| safe_name(name))
                .or(id)?;
            let model_key = id.unwrap_or(name).replace(' ', "-");
            let key = format!("{kind}-{model_key}");
            let scope = if id.is_some() {
                UsageScope::Model(model_key)
            } else {
                UsageScope::ModelFamily(model_key)
            };
            let period = WINDOWS
                .iter()
                .find(|(seconds, ..)| duration_seconds == Some(*seconds))
                .map_or(group, |(_, _, label)| *label);
            (key, model_label(name, period), scope)
        }
    };
    let (used_percent, resets_at) = reading(raw, "percent").ok()?;
    Some(UsageWindow {
        key,
        label,
        duration_seconds,
        scope,
        used_percent,
        resets_at,
        observed_at: now,
        source: UsageSource::UsageEndpoint,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_guard::http;
    use serde_json::json;

    #[tokio::test]
    async fn usage_request_has_only_the_selected_identity_headers() {
        let body = r#"{"five_hour":{"utilization":25,"resets_at":null}}"#;
        let server =
            http::serve(vec![http::Reply::Send(http::response("200 OK", &[], body))]).await;
        let url = format!("{}/api/oauth/usage", server.url);
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let (identity, client_version) = super::super::subscription::data().unwrap();
        let rendered = identity.rendered(client_version.version).unwrap();
        assert_eq!(
            fetch(&client, &url, "fake", &crate::clock::StillClock, &rendered)
                .await
                .unwrap()
                .windows
                .len(),
            1
        );
        let headers = http::header_lines(&server.requests.lock().unwrap()[0]);
        let names: std::collections::BTreeSet<_> =
            headers.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            [
                "accept",
                "authorization",
                "host",
                "user-agent",
                "x-app",
                "anthropic-beta"
            ]
            .into_iter()
            .collect()
        );
        for (name, value) in &rendered.usage_headers {
            assert!(headers.contains(&(name.as_str().into(), value.to_str().unwrap().into())));
        }
    }

    #[test]
    fn maps_only_supported_windows_and_preserves_unknown_values() {
        let usage = decode(&json!({"five_hour":{"utilization":75.0,"resets_at":"2027-01-15T09:00:00Z"}, "seven_day":{"utilization":null,"resets_at":null}, "plan_type":"PRIVATE_CANARY", "extra_usage":{"used_credits":999}}), 1_800_000_000).unwrap();
        assert_eq!(usage.windows.len(), 2);
        assert_eq!(usage.windows[0].used_percent.unwrap().value(), 75.0);
        assert!(usage.windows[1].used_percent.is_none());
        assert!(!format!("{usage:?}").contains("CANARY"));
        assert!(decode(&json!({"five_hour":{"utilization":101}}), 1_800_000_000).is_err());
        assert!(decode(&json!({"limits":[]}), 1_800_000_000).is_err());
        for (all, model) in [(66, 100), (16, 22)] {
            let raw = json!({
                "five_hour": {"utilization":1,"resets_at":null},
                "seven_day": {"utilization":all,"resets_at":null},
                "seven_day_opus": {"utilization":12},
                "seven_day_new_model": {"utilization":34},
                "seven_day_breakdown": {"rows":[{"percent":50}]},
                "spend": {"percent":99},
                "limits": [
                    {"kind":"session","group":"session","percent":1,"scope":null},
                    {"kind":"weekly_all","group":"weekly","percent":all,"scope":null},
                    {"kind":"weekly_scoped","group":"weekly","percent":model,"scope":{"model":{"display_name":"Fable","id":null}},"resets_at":"2027-01-15T09:00:00Z"}
                ]
            });
            let usage = decode(&raw, 1_800_000_000).unwrap();
            assert_eq!(usage.windows.len(), 5);
            let weekly = usage.windows.iter().find(|w| w.key == "seven_day").unwrap();
            assert_eq!(weekly.used_percent.unwrap().value(), f64::from(all));
            assert!(weekly.resets_at.is_none());
            let family = usage
                .windows
                .iter()
                .find(|w| w.label == "Fable weekly")
                .unwrap();
            assert_eq!(family.used_percent.unwrap().value(), f64::from(model));
            assert_eq!(family.scope, UsageScope::ModelFamily("Fable".into()));
            assert!(!family.scope.applies_to("claude-fable-5-2"));
            assert!(family.resets_at.is_some());
            assert_eq!(family.source, UsageSource::UsageEndpoint);
            assert_eq!(family.duration_seconds, Some(WINDOWS[1].0));
            assert!(usage.windows.iter().any(|w| w.label == "Opus weekly"));
            assert!(usage.windows.iter().any(|w| w.label == "New model weekly"));
            for malformed in [
                json!(null),
                json!(false),
                json!({"percent":101}),
                json!({"percent":"CANARY"}),
                json!({"resets_at":"CANARY"}),
                json!({"scope":{"model":{"display_name":"PRIVATE_CANARY\n"}}}),
            ] {
                let mut changed = raw.clone();
                let entry = &mut changed["limits"][2];
                if let Some(fields) = malformed.as_object() {
                    entry.as_object_mut().unwrap().extend(fields.clone());
                } else {
                    *entry = malformed;
                }
                let usage = decode(&changed, 1_800_000_000).unwrap();
                assert_eq!(usage.windows.len(), 4);
                assert!(!format!("{usage:?}").contains("CANARY"));
            }
            for name in [
                uuid::Uuid::nil().to_string(),
                format!("{}@{}", "PRIVATE_CANARY", "invalid"),
                "sk-CANARY0123456789abcdefghijklmnop".into(),
            ] {
                let mut changed = raw.clone();
                changed["limits"][2]["scope"]["model"]["display_name"] = json!(name);
                assert_eq!(decode(&changed, 1_800_000_000).unwrap().windows.len(), 4);
            }
            let mut spaced = raw.clone();
            spaced["limits"][2]["scope"]["model"]["display_name"] = json!("Opus 4");
            let usage = decode(&spaced, 1_800_000_000).unwrap();
            let family = usage
                .windows
                .iter()
                .find(|w| w.label == "Opus 4 weekly")
                .unwrap();
            assert_eq!(family.key, "weekly_scoped-Opus-4");
            assert_eq!(family.scope, UsageScope::ModelFamily("Opus-4".into()));
            assert!(!family.scope.applies_to("claude-opus-4"));
            let mut session = spaced.clone();
            session["limits"][2]["group"] = json!("session");
            assert!(
                decode(&session, 1_800_000_000)
                    .unwrap()
                    .windows
                    .iter()
                    .any(|w| w.label == "Opus 4 5-hour")
            );
            let mut identified = raw.clone();
            identified["limits"][2]["scope"]["model"]["id"] = json!("claude-fable-5-2");
            identified["limits"][2]["resets_at"] = Value::Null;
            let usage = decode(&identified, 1_800_000_000).unwrap();
            let family = usage
                .windows
                .iter()
                .find(|w| w.label == "Fable weekly")
                .unwrap();
            assert_eq!(family.scope, UsageScope::Model("claude-fable-5-2".into()));
            assert!(family.resets_at.is_none());
        }
    }
}
