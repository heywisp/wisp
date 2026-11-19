use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidUsage(&'static str);

impl std::fmt::Display for InvalidUsage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for InvalidUsage {}

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct Percent(f64);

impl Eq for Percent {}

impl Percent {
    pub fn value(self) -> f64 {
        self.0
    }
}

impl TryFrom<f64> for Percent {
    type Error = InvalidUsage;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        if value.is_finite() && (0.0..=100.0).contains(&value) {
            Ok(Self(value))
        } else {
            Err(InvalidUsage(
                "usage percent must be finite and between zero and one hundred",
            ))
        }
    }
}

impl From<Percent> for f64 {
    fn from(value: Percent) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    Allowed,
    Limited,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageSource {
    UsageEndpoint,
    ResponseHeader,
    ResponseEvent,
    ErrorBody,
    RetryAfter,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageScope {
    All,
    Model(String),
    ModelFamily(String),
}

impl UsageScope {
    pub fn applies_to(&self, model: &str) -> bool {
        match self {
            Self::All => true,
            Self::Model(id) => id == model,
            Self::ModelFamily(prefix) => model
                .strip_prefix(prefix)
                .is_some_and(|suffix| suffix.starts_with('-')),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "WindowFields")]
pub struct UsageWindow {
    pub key: String,
    pub label: String,
    pub duration_seconds: Option<u64>,
    pub scope: UsageScope,
    pub used_percent: Option<Percent>,
    pub resets_at: Option<i64>,
    pub observed_at: i64,
    pub source: UsageSource,
}

#[derive(Deserialize)]
struct WindowFields {
    key: String,
    label: String,
    duration_seconds: Option<u64>,
    scope: UsageScope,
    used_percent: Option<Percent>,
    resets_at: Option<i64>,
    observed_at: i64,
    source: UsageSource,
}

impl TryFrom<WindowFields> for UsageWindow {
    type Error = InvalidUsage;

    fn try_from(fields: WindowFields) -> Result<Self, Self::Error> {
        let window = Self {
            key: fields.key,
            label: fields.label,
            duration_seconds: fields.duration_seconds,
            scope: fields.scope,
            used_percent: fields.used_percent,
            resets_at: fields.resets_at,
            observed_at: fields.observed_at,
            source: fields.source,
        };
        if window.valid() {
            Ok(window)
        } else {
            Err(InvalidUsage("invalid subscription usage window"))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanTier {
    #[serde(rename = "free")]
    Free,
    #[serde(rename = "go")]
    Go,
    #[serde(rename = "plus")]
    Plus,
    #[serde(rename = "pro")]
    Pro,
    #[serde(rename = "prolite")]
    ProLite,
    #[serde(rename = "promax")]
    ProMax,
    #[serde(rename = "team")]
    Team,
    #[serde(rename = "self_serve_business_prolite")]
    SelfServeBusinessProLite,
    #[serde(rename = "self_serve_business_usage_based")]
    SelfServeBusinessUsageBased,
    #[serde(rename = "business")]
    Business,
    #[serde(rename = "ent26")]
    Ent26,
    #[serde(rename = "enterprise_cbp_automation")]
    EnterpriseCbpAutomation,
    #[serde(rename = "enterprise_cbp_usage_based")]
    EnterpriseCbpUsageBased,
    #[serde(rename = "enterprise")]
    Enterprise,
    #[serde(rename = "edu")]
    Edu,
    #[serde(rename = "edu_plus")]
    EduPlus,
    #[serde(rename = "edu_pro")]
    EduPro,
    #[serde(rename = "sci")]
    Sci,
    #[serde(rename = "sci_plus")]
    SciPlus,
}

impl PlanTier {
    pub fn label(self) -> &'static str {
        match self {
            Self::Free => "Free",
            Self::Go => "Go",
            Self::Plus => "Plus",
            Self::Pro => "Pro",
            Self::ProLite => "Pro Lite",
            Self::ProMax => "Pro Max",
            Self::Team => "Team",
            Self::SelfServeBusinessProLite => "Business Pro Lite",
            Self::SelfServeBusinessUsageBased => "Business Usage Based",
            Self::Business => "Business",
            Self::Ent26 => "Ent26",
            Self::EnterpriseCbpAutomation => "Enterprise CBP Automation",
            Self::EnterpriseCbpUsageBased => "Enterprise CBP Usage Based",
            Self::Enterprise => "Enterprise",
            Self::Edu => "Edu",
            Self::EduPlus => "Edu Plus",
            Self::EduPro => "Edu Pro",
            Self::Sci => "Sci",
            Self::SciPlus => "Sci Plus",
        }
    }
}

fn optional_tier<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<PlanTier>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value
        .is_string()
        .then(|| serde_json::from_value(value).ok())
        .flatten())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "Snapshot")]
pub struct PlanUsage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_tier: Option<PlanTier>,
    pub availability: Option<Availability>,
    pub windows: Vec<UsageWindow>,
}

#[derive(Deserialize)]
struct Snapshot {
    #[serde(default, deserialize_with = "optional_tier")]
    plan_tier: Option<PlanTier>,
    availability: Option<Availability>,
    windows: Vec<UsageWindow>,
}

impl TryFrom<Snapshot> for PlanUsage {
    type Error = InvalidUsage;

    fn try_from(value: Snapshot) -> Result<Self, Self::Error> {
        Self::new(value.availability, value.windows).map(|mut snapshot| {
            snapshot.plan_tier = value.plan_tier;
            snapshot
        })
    }
}

pub const MAX_USAGE_WINDOWS: usize = 64;

const MAX_WINDOW_NAME: usize = 96;

pub fn valid_window_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_WINDOW_NAME
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_. ".contains(&c))
}

pub fn valid_timestamp(seconds: i64) -> bool {
    (0..=253_402_300_799).contains(&seconds)
}

impl PlanUsage {
    pub fn new(
        availability: Option<Availability>,
        mut windows: Vec<UsageWindow>,
    ) -> Result<Self, InvalidUsage> {
        if windows.len() > MAX_USAGE_WINDOWS || windows.iter().any(|window| !window.valid()) {
            return Err(InvalidUsage("invalid subscription usage windows"));
        }
        windows.sort_by(|a, b| a.key.cmp(&b.key));
        if windows.windows(2).any(|pair| pair[0].key == pair[1].key) {
            return Err(InvalidUsage("duplicate subscription usage window"));
        }
        Ok(Self {
            plan_tier: None,
            availability,
            windows,
        })
    }
}

pub const WINDOWS: [(u64, &str, &str); 2] = [(18_000, "5h", "5-hour"), (604_800, "week", "Weekly")];

pub const RETRY_AFTER_KEY: &str = "retry-after";

pub const USAGE_MAX_AGE: i64 = 900;

impl UsageWindow {
    pub fn reset_passed(&self, now: i64) -> bool {
        self.resets_at.is_some_and(|at| at <= now)
    }

    pub fn stale(&self, now: i64) -> bool {
        now.saturating_sub(self.observed_at) >= USAGE_MAX_AGE || self.reset_passed(now)
    }

    pub fn is_retry_guard(&self) -> bool {
        self.key == RETRY_AFTER_KEY
    }

    pub fn valid(&self) -> bool {
        valid_window_name(&self.key)
            && valid_window_name(&self.label)
            && valid_timestamp(self.observed_at)
            && self.resets_at.is_none_or(valid_timestamp)
            && self
                .duration_seconds
                .is_none_or(|seconds| seconds > 0 && seconds <= 31_622_400)
            && match &self.scope {
                UsageScope::All => true,
                UsageScope::Model(model) => crate::valid_model_id(model),
                UsageScope::ModelFamily(model) => {
                    model.len() <= MAX_WINDOW_NAME && crate::valid_model_id(model)
                }
            }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlanUsageError {
    Disabled,
    Unsupported,
    AuthenticationRequired,
    AccessDenied,
    ScopeRequired,
    Throttled { retry_at: Option<i64> },
    Network,
    InvalidResponse,
}

impl std::fmt::Display for PlanUsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Disabled => "Claude usage is off (claude_usage in /config)",
            Self::Unsupported => "subscription usage is unavailable",
            Self::AuthenticationRequired => "sign in again to read subscription usage",
            Self::AccessDenied => {
                "subscription usage access was refused; check access or sign in again"
            }
            Self::ScopeRequired => "subscription usage requires a new sign-in with profile scope",
            Self::Throttled { .. } => "subscription usage lookup is throttled",
            Self::Network => "subscription usage lookup could not connect",
            Self::InvalidResponse => "subscription usage response is unsupported",
        })
    }
}

impl PlanUsageError {
    pub fn needs_user(self) -> bool {
        matches!(
            self,
            Self::AuthenticationRequired
                | Self::AccessDenied
                | Self::ScopeRequired
                | Self::Unsupported
        )
    }
}

impl std::error::Error for PlanUsageError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageAction {
    Subscribe,
    Release,
    Refresh,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageState {
    pub snapshot: Option<PlanUsage>,
    pub loading: bool,
    pub stale: bool,
    pub error: Option<PlanUsageError>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentEvent, ClientCommand, RequestId};

    #[test]
    fn tier_is_optional_closed_and_tolerant_on_decode() {
        let usage = PlanUsage::new(None, vec![window("a")]).unwrap();
        let original = serde_json::to_value(&usage).unwrap();
        assert!(original.get("plan_tier").is_none());
        for raw in [
            serde_json::json!(null),
            serde_json::json!(42),
            serde_json::json!({"secret":"CANARY"}),
            serde_json::json!({"pro":null}),
            serde_json::json!({"pro":5}),
            serde_json::json!(["pro"]),
            serde_json::json!("unknown"),
            serde_json::json!("pro\nCANARY"),
            serde_json::json!("x".repeat(10000)),
        ] {
            let mut value = original.clone();
            value["plan_tier"] = raw;
            assert_eq!(serde_json::from_value::<PlanUsage>(value).unwrap(), usage);
        }
        let mut value = original;
        value["plan_tier"] = serde_json::json!("pro");
        let decoded: PlanUsage = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(decoded.plan_tier, Some(PlanTier::Pro));
        assert_eq!(serde_json::to_value(decoded).unwrap(), value);
    }

    #[test]
    fn durable_limit_metadata_reads_without_it_and_round_trips_with_it() {
        let old = r#"{"phase":"response","source":"endpoint","status":429,"retry":{"type":"never"},"possibly_billed":false}"#;
        let mut detail: crate::ErrorDetail = serde_json::from_str(old).unwrap();
        assert!(detail.limit_reached.is_none());
        assert!(detail.model_limit.is_none());
        detail.limit_reached = Some(Box::new(window("subscription-limit")));
        detail.model_limit = Some(Box::new(UsageWindow {
            scope: UsageScope::ModelFamily("Fable".into()),
            used_percent: Some(100.0.try_into().unwrap()),
            resets_at: Some(1_800_000_100),
            ..window("fable-weekly")
        }));
        let mut invalid = serde_json::to_value(&detail).unwrap();
        invalid["model_limit"]["label"] = serde_json::Value::String("x".repeat(97));
        assert!(serde_json::from_value::<crate::ErrorDetail>(invalid).is_err());
        let encoded = serde_json::to_string(&detail).unwrap();
        assert_eq!(
            serde_json::from_str::<crate::ErrorDetail>(&encoded).unwrap(),
            detail
        );
        for (scope, valid) in [
            (UsageScope::Model("m".repeat(96)), true),
            (UsageScope::Model("m".repeat(97)), true),
            (UsageScope::Model("m".repeat(128)), true),
            (UsageScope::Model("m".repeat(129)), false),
            (UsageScope::ModelFamily("m".repeat(96)), true),
            (UsageScope::ModelFamily("m".repeat(97)), false),
        ] {
            detail.limit_reached.as_mut().unwrap().scope = scope;
            let encoded = serde_json::to_string(&detail).unwrap();
            let decoded = serde_json::from_str::<crate::ErrorDetail>(&encoded);
            let scope = &detail.limit_reached.as_ref().unwrap().scope;
            if valid {
                let decoded = decoded.unwrap_or_else(|err| panic!("{scope:?}: {err}"));
                assert_eq!(decoded, detail);
            } else {
                assert!(decoded.is_err(), "{scope:?}");
            }
        }
        assert!(UsageScope::ModelFamily("claude-opus".into()).applies_to("claude-opus-5-5"));
        assert!(!UsageScope::ModelFamily("claude-opus".into()).applies_to("claude-sonnet-5-5"));
    }

    #[test]
    fn percent_is_not_a_fraction_and_rejects_nonfinite_values() {
        assert_eq!(Percent::try_from(0.75).unwrap().value(), 0.75);
        assert_eq!(Percent::try_from(75.0).unwrap().value(), 75.0);
        for value in [-1.0, 101.0, f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
            assert!(Percent::try_from(value).is_err(), "value {value}");
        }
        assert!(serde_json::from_str::<Percent>("101").is_err());
    }

    fn window(key: &str) -> UsageWindow {
        UsageWindow {
            key: key.into(),
            label: "Weekly".into(),
            duration_seconds: Some(604_800),
            scope: UsageScope::All,
            used_percent: None,
            resets_at: None,
            observed_at: 1_800_000_000,
            source: UsageSource::ResponseEvent,
        }
    }

    #[test]
    fn snapshots_are_bounded_sorted_unique_and_validate_wire_values() {
        let usage = PlanUsage::new(None, vec![window("b"), window("a")]).unwrap();
        assert_eq!(usage.windows[0].key, "a");
        let mut full: Vec<_> = (0..MAX_USAGE_WINDOWS)
            .map(|n| window(&n.to_string()))
            .collect();
        assert!(PlanUsage::new(None, full.clone()).is_ok());
        full.push(window(&MAX_USAGE_WINDOWS.to_string()));
        assert!(PlanUsage::new(None, full).is_err());
        for (name, expected) in [
            ("Opus 4 weekly".into(), true),
            ("x".repeat(96), true),
            ("x".repeat(97), false),
            (String::new(), false),
            ("CANARY\n".into(), false),
        ] {
            assert_eq!(valid_window_name(&name), expected);
            let candidate = UsageWindow {
                label: name,
                ..window("model")
            };
            assert_eq!(PlanUsage::new(None, vec![candidate]).is_ok(), expected);
        }
        assert!(PlanUsage::new(None, vec![window("a"); 2]).is_err());
        for (field, value) in [
            ("label", serde_json::json!("https://private.invalid")),
            ("observed_at", serde_json::json!(-1)),
            ("resets_at", serde_json::json!(i64::MAX)),
            ("duration_seconds", serde_json::json!(u64::MAX)),
            ("used_percent", serde_json::json!(101)),
        ] {
            let mut raw = serde_json::to_value(&usage).unwrap();
            raw["windows"][0][field] = value;
            assert!(serde_json::from_value::<PlanUsage>(raw).is_err(), "{field}");
        }
    }

    #[test]
    fn commands_snapshots_and_warnings_round_trip_without_log_schema_changes() {
        let command = ClientCommand::PlanUsage {
            request: RequestId(7),
            endpoint: "alias".into(),
            action: UsageAction::Subscribe,
        };
        let json = serde_json::to_string(&command).unwrap();
        assert_eq!(
            serde_json::from_str::<ClientCommand>(&json).unwrap(),
            command
        );
        let event = AgentEvent::PlanUsageChanged {
            request: Some(RequestId(7)),
            endpoint: "alias".into(),
            state: UsageState {
                snapshot: Some({
                    let mut snapshot = PlanUsage::new(None, vec![window("a")]).unwrap();
                    snapshot.plan_tier = Some(PlanTier::Pro);
                    snapshot
                }),
                loading: false,
                stale: false,
                error: None,
            },
        };
        let warning = AgentEvent::PlanUsageWarning {
            endpoint: "alias".into(),
            window: window("a"),
            threshold: 95,
        };
        for event in [event, warning] {
            assert!(event.is_account_telemetry());
            let json = serde_json::to_string(&event).unwrap();
            assert_eq!(serde_json::from_str::<AgentEvent>(&json).unwrap(), event);
            assert!(matches!(
                serde_json::from_str::<crate::log::LogEvent>(&json),
                Ok(crate::log::LogEvent::Unknown)
            ));
        }
    }
    #[test]
    fn tier_is_never_part_of_durable_limit_metadata() {
        let mut detail = crate::ErrorDetail::new(
            crate::FailurePhase::Response,
            crate::FailureSource::Endpoint,
            crate::RetryClass::Never,
        );
        detail.limit_reached = Some(Box::new(window("weekly")));
        let mut value = serde_json::to_value(&detail).unwrap();
        value["plan_tier"] = serde_json::json!("pro");
        let decoded: crate::ErrorDetail = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, detail);
        assert!(
            !serde_json::to_string(&decoded)
                .unwrap()
                .contains("plan_tier")
        );
    }

    #[test]
    fn standalone_warning_windows_are_validated_on_the_wire() {
        let warning = AgentEvent::PlanUsageWarning {
            endpoint: "alias".into(),
            window: window("a"),
            threshold: 95,
        };
        let mut raw = serde_json::to_value(warning).unwrap();
        raw["window"]["label"] = serde_json::json!("https://private.invalid");
        assert!(serde_json::from_value::<AgentEvent>(raw).is_err());
    }
}
