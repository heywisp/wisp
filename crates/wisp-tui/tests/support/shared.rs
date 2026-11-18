#![allow(
    dead_code,
    clippy::unwrap_used,
    reason = "the library's tests and the app test binary each use part of the shared helpers, and a failed parse fails the test"
)]

use wisp_protocol::plugin::{HookDisabled, HookEntry, PluginSource, ToolEntry};
use wisp_protocol::settings::{
    AppliedChange, Layer, PlacedValue, SettingName, SettingTarget, SettingValue, SitePath, When,
    find,
};
use wisp_protocol::slot::SlotEntry;
use wisp_protocol::{AgentEvent, Catalog, CompactionTrigger, PluginEntry, RetryFocus, RetryOffer};
use wisp_render::chrome::account::mockup_mark;
use wisp_render::chrome::{Credential, Endpoint, EndpointFacts};
use wisp_render::styled::{StyledLine, StyledSpan};

pub(crate) fn started() -> AgentEvent {
    AgentEvent::CompactionStarted {
        trigger: CompactionTrigger::Manual,
        turn: None,
        switch: None,
        requested_by: None,
    }
}

pub(crate) fn changed() -> AgentEvent {
    AgentEvent::ContextChanged {
        used: 14_000,
        used_estimated: true,
        next: 14_000,
        window: Some(1_000_000),
        compact_at: Some(850_000),
        meter_limit: Some(850_000),
        auto_compact: true,
        epoch: 1,
    }
}

pub(crate) fn offer(retry: bool, switch: bool, focus: RetryFocus) -> RetryOffer {
    RetryOffer {
        retry,
        switch,
        focus,
        suggest: None,
        retries_at: None,
    }
}

pub(crate) fn switch_placed(id: &str, key: &str, on: bool) -> PlacedValue {
    PlacedValue {
        path: SitePath::plugin_switch(id, key).unwrap(),
        value: Some(SettingValue::Bool(on)),
        layer: Layer::Session,
    }
}

pub(crate) fn catalog_tool(name: &str, follows: Option<&str>, default_on: bool) -> ToolEntry {
    ToolEntry {
        name: name.to_owned(),
        on: default_on,
        default_on,
        native: false,
        follows: follows.map(str::to_owned),
        unavailable: None,
        waits: false,
    }
}

pub(crate) fn plugin(id: &str, enabled: bool, slots: Vec<SlotEntry>) -> PluginEntry {
    PluginEntry {
        prompt_sections: Vec::new(),
        id: id.to_owned(),
        version: "0.1.0".to_owned(),
        summary: String::new(),
        source: PluginSource::Bundled,
        installed: true,
        enabled,
        tools: vec![],
        commands: vec![],
        hooks: vec![],
        slots,
        settings: vec![],
        problems: vec![],
    }
}

pub(crate) fn defined_plugin(id: &str) -> PluginEntry {
    let mut out = plugin(id, true, vec![]);
    let row = wisp_protocol::plugin::SettingRow {
        name: "minutes".to_owned(),
        label: "Idle after".to_owned(),
        help: "Minutes without a key press before Wisp treats you as away.".to_owned(),
        kind: wisp_protocol::plugin::RowKind::Int {
            min: 1,
            max: 60,
            default: 10,
        },
    };
    out.settings = vec![wisp_protocol::plugin::Setting::Row(row)];
    out
}

pub(crate) fn hook_entry(
    name: &str,
    point: &str,
    order: u32,
    switch: Option<&str>,
    disabled: Option<HookDisabled>,
) -> HookEntry {
    HookEntry {
        hook: name.to_owned(),
        point: point.to_owned(),
        on: disabled.is_none(),
        order,
        disabled,
        switch: switch.map(str::to_owned),
    }
}

pub(crate) fn catalog(plugins: Vec<PluginEntry>) -> AgentEvent {
    AgentEvent::CatalogChanged {
        catalog: Catalog { plugins },
    }
}

pub(crate) fn endpoint(
    name: &str,
    reach: Endpoint,
    credential: Credential,
    host: Option<&str>,
    unavailable: Option<&str>,
) -> EndpointFacts {
    EndpointFacts {
        models: Vec::new(),
        mark: mockup_mark(name),
        name: name.to_owned(),
        provider: "anthropic".to_owned(),
        credential: Some(credential),
        reach: Some(reach),
        host: host.map(str::to_owned),
        unavailable: unavailable.map(str::to_owned),
        priced: unavailable.is_none(),
        expired: false,
        key_var: None,
        complete: false,
        default_model: None,
    }
}

pub(crate) fn named(name: &str) -> SettingTarget {
    SettingTarget::Name(SettingName::of(find(name).unwrap()))
}

pub(crate) fn change(
    target: SettingTarget,
    value: Option<SettingValue>,
    layer: Layer,
    when: When,
) -> AppliedChange {
    AppliedChange {
        target,
        value,
        layer,
        when,
        cache: None,
        consequence: None,
        saved: None,
        overridden: None,
        warnings: Vec::new(),
    }
}

pub(crate) fn fixed_now() -> jiff::Timestamp {
    "2026-10-26T18:00:00Z".parse().unwrap()
}

pub(crate) fn line(text: &str) -> StyledLine {
    StyledLine::new(vec![StyledSpan::plain(text)])
}
