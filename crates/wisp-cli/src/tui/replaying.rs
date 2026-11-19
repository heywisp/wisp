use std::collections::{BTreeMap, BTreeSet};

use wisp_core::endpoints::{ConfigFile, ENV, StartReason};
use wisp_core::models::ModelTable;
use wisp_core::provider::shipped_capabilities;
use wisp_core::settings::host::SavedRead;
use wisp_core::settings::{
    Current, EndpointRow, Given, Layer, Layers, shipped_models, snapshot_rows,
};
use wisp_protocol::settings::{
    Dynamic, ModelEntry, SettingSpec, SettingValue, SitePath, Target, find, names,
};
use wisp_protocol::{CacheTtl, Effort};
use wisp_render::chrome::Endpoint;
use wisp_tui::app::replay::{Described, Settings};

use crate::endpoints;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    Flag,
    Variable,
    Saved,
    Default,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReplayEndpoint {
    pub(crate) name: String,
    pub(crate) reach: Endpoint,
    pub(crate) source: Source,
}

pub(crate) fn endpoint(flag: Option<&str>, config: Option<&ConfigFile>) -> ReplayEndpoint {
    let (choice, reason) = endpoints::choice(flag);
    let saved = config.and_then(|c| c.default.as_ref());
    let known = crate::configured(config);
    let (name, source) = match (choice, saved) {
        (Some(name), _) if reason == StartReason::Variable => {
            (crate::shown_choice(&name, &known), Source::Variable)
        }
        (Some(name), _) => (crate::shown_choice(&name, &known), Source::Flag),
        (None, Some(default)) => (default.to_string(), Source::Saved),
        (None, None) => (ENV.to_owned(), Source::Default),
    };
    ReplayEndpoint {
        reach: reach_of(&name, config),
        name,
        source,
    }
}

fn reach_of(name: &str, config: Option<&ConfigFile>) -> Endpoint {
    if name == ENV {
        return Endpoint::Official;
    }
    let official = config
        .and_then(|c| c.endpoints.iter().find(|(n, _)| n.as_str() == name))
        .and_then(|(_, e)| e.official());
    if official == Some(true) {
        Endpoint::Official
    } else {
        Endpoint::Custom
    }
}

pub(crate) struct Start<'a> {
    pub(crate) model: &'a str,
    pub(crate) effort: Effort,
    pub(crate) endpoint: &'a ReplayEndpoint,
    pub(crate) flags: &'a [Given],
    pub(crate) env: Vec<Given>,
    pub(crate) config: Option<&'a ConfigFile>,
    pub(crate) read: SavedRead,
    pub(crate) max_tokens: u32,
    pub(crate) cache_ttl: CacheTtl,
}

pub(crate) fn model_efforts(models: &[ModelEntry]) -> BTreeMap<String, Vec<Effort>> {
    models
        .iter()
        .filter_map(|m| {
            Some((
                m.id.clone(),
                shipped_capabilities(&m.id)?.1.efforts.to_vec(),
            ))
        })
        .collect()
}

pub(crate) fn settings(start: Start<'_>) -> Settings {
    let endpoint_spec = find(names::ENDPOINT);
    let mut env = start.env;
    if start.endpoint.source == Source::Variable
        && let Some(spec) = endpoint_spec
    {
        env.push(Given {
            target: Target::Name(spec),
            value: SettingValue::Text(start.endpoint.name.clone()),
            origin: endpoints::chosen_origin().to_owned(),
        });
    }
    let mut saved = start.config.map(|c| c.settings.clone());
    if let (Some(saved), Some(default), Some(spec)) = (
        saved.as_mut(),
        start.config.and_then(|c| c.default.as_ref()),
        endpoint_spec,
    ) {
        saved.insert(
            SitePath::global(spec),
            SettingValue::Text(default.to_string()),
            0,
        );
    }
    let shipped = ModelTable::shipped().ok();
    let layers = Layers {
        session: Vec::new(),
        flags: start.flags.to_vec(),
        env,
        saved: saved.as_ref(),
        shipped: shipped.as_ref(),
    };
    let current = Current {
        endpoint: Some(start.endpoint.name.clone()),
        model: start.model.to_owned(),
    };
    let fallback = EndpointRow {
        value: start.endpoint.name.clone(),
        layer: Layer::Default.wire(),
        origin: endpoint_spec
            .map(SettingSpec::default_words)
            .unwrap_or_default(),
        keys_file: None,
        effort: start.effort,
        credential: None,
        wire: None,
    };
    let models = shipped_models();
    let names: BTreeSet<String> = std::iter::once(ENV.to_owned())
        .chain(
            start
                .config
                .into_iter()
                .flat_map(|c| c.endpoints.keys().map(ToString::to_string)),
        )
        .collect();
    let catalog = crate::tools::catalog(&crate::tools::registry().0);
    let tools = catalog.leader_names();
    let choices = |spec: &'static SettingSpec| -> Vec<String> {
        let Some(dynamic) = spec.dynamic else {
            return spec
                .kind
                .choices()
                .iter()
                .map(|c| (*c).to_owned())
                .collect();
        };
        match dynamic {
            Dynamic::Tools => tools.clone(),
            Dynamic::Sections => catalog.section_names(),
            Dynamic::Endpoints => names.iter().cloned().collect(),
            Dynamic::Models => models.iter().map(|m| m.id.clone()).collect(),
            Dynamic::Efforts => shipped_capabilities(start.model)
                .map_or(&Effort::ALL[..], |(_, row)| row.efforts)
                .iter()
                .map(|e| e.as_str().to_owned())
                .collect(),
            Dynamic::VoiceEndpoints | Dynamic::Voices => Vec::new(),
        }
    };
    let rows = snapshot_rows(&layers, &current, true, &fallback, &choices);
    Settings {
        endpoint: start.endpoint.name.clone(),
        model: start.model.to_owned(),
        effort: start.effort,
        cache_ttl: start.cache_ttl,
        efforts: model_efforts(&models),
        described: Described {
            settings: rows,
            models,
            saved: start.read.saved,
            file: start.read.file,
            max_tokens: start.max_tokens,
            endpoint: Some(start.endpoint.name.clone()),
            model: Some(start.model.to_owned()),
            plugins: catalog.plugins,
        },
        ..Settings::default()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use wisp_core::provider::openai::model_capabilities;
    use wisp_protocol::settings::FileState;
    use wisp_protocol::{AgentEvent, ClientCommand, CompactionTrigger, TurnOutcome};
    use wisp_tui::app::replay::{Pending, Script, spawn};

    use super::*;

    fn replay_on(model: &str, effort: Effort) -> Settings {
        let endpoint = ReplayEndpoint {
            name: ENV.to_owned(),
            reach: Endpoint::Official,
            source: Source::Default,
        };
        settings(Start {
            model,
            effort,
            endpoint: &endpoint,
            flags: &[],
            env: Vec::new(),
            config: None,
            read: SavedRead {
                often: Vec::new(),
                saved: Vec::new(),
                file: FileState::Missing,
            },
            max_tokens: 64_000,
            cache_ttl: CacheTtl::OneHour,
        })
    }

    #[test]
    fn the_replay_efforts_map_holds_the_openai_models_efforts() {
        let efforts = model_efforts(&shipped_models());
        let gpt = model_capabilities("gpt-5.5").map(|row| row.efforts.to_vec());
        assert_eq!(efforts.get("gpt-5.5").cloned(), gpt);
        assert!(!efforts["gpt-5.5"].contains(&Effort::Max));
    }

    #[test]
    fn the_replay_effort_choices_for_an_openai_model_are_its_own() {
        let settings = replay_on("gpt-5.5", Effort::High);
        let choices = settings
            .described
            .settings
            .iter()
            .find(|row| row.name.to_string() == "effort")
            .map(|row| row.choices.clone());
        assert_eq!(
            choices,
            Some(vec![
                "low".to_owned(),
                "medium".to_owned(),
                "high".to_owned(),
                "xhigh".to_owned()
            ])
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_withdrawn_switch_keeps_an_openai_model_off_an_effort_it_cannot_take() {
        let settings = Settings {
            pending: Some(Pending {
                endpoint: ENV.to_owned(),
                model: "gpt-6.1-sol".to_owned(),
                effort: Effort::Max,
            }),
            ..replay_on("gpt-5.5", Effort::High)
        };
        let events_in = vec![
            AgentEvent::CompactionStarted {
                trigger: CompactionTrigger::Switch,
                turn: Some(1),
                switch: None,
                requested_by: None,
            },
            AgentEvent::turn_started(1),
        ];
        let script = Script::new(events_in).with_settings(settings);
        let (commands, mut events) = spawn(script, Duration::from_millis(12));
        commands
            .send(ClientCommand::send("t-1", "go"))
            .await
            .unwrap();
        let mut seen = Vec::new();
        while let Some(event) = events.recv().await {
            if matches!(event, AgentEvent::CompactionStarted { .. }) {
                commands.send(ClientCommand::Cancel).await.unwrap();
            }
            let withdrawn = matches!(
                event,
                AgentEvent::SettingsChanged {
                    withdrawn: Some(_),
                    ..
                }
            );
            seen.push(event);
            if withdrawn {
                break;
            }
        }
        let Some(AgentEvent::SettingsChanged { model, effort, .. }) = seen.last() else {
            panic!("{seen:?}");
        };
        assert_eq!((model.as_str(), *effort), ("gpt-5.5", Effort::High));
        assert!(
            seen.iter().any(|e| matches!(
                e,
                AgentEvent::TurnEnded {
                    outcome: TurnOutcome::Cancelled { .. },
                    ..
                }
            )),
            "{seen:?}"
        );
    }

    type Step = (Option<&'static str>, Option<Effort>);

    async fn refusal(
        commands: tokio::sync::mpsc::Sender<ClientCommand>,
        mut events: tokio::sync::mpsc::Receiver<AgentEvent>,
        steps: &[Step],
    ) -> String {
        for (n, (to, asked)) in steps.iter().enumerate() {
            let change = ClientCommand::ChangeSettings {
                request: wisp_protocol::RequestId(n as u64),
                changes: ClientCommand::shortcut_changes(None, to.map(str::to_owned), *asked),
                save: false,
            };
            commands.send(change).await.unwrap();
            let last = n + 1 == steps.len();
            loop {
                match events.recv().await.unwrap() {
                    AgentEvent::Rejected { reason, .. } if last => return reason,
                    AgentEvent::SettingsChanged { .. } if !last => break,
                    e @ (AgentEvent::Rejected { .. } | AgentEvent::SettingsChanged { .. }) => {
                        panic!("step {n}: {e:?}")
                    }
                    _ => {}
                }
            }
        }
        panic!("no step was refused")
    }

    #[tokio::test]
    async fn a_replay_refuses_an_effort_the_model_cannot_take_as_the_kernel_does() {
        use wisp_core::endpoints::{
            EndpointName, PricingMode, ProviderKind, ResolvedEndpoint, UrlSource,
        };
        use wisp_core::provider::credential::{Credential, Source as KeySource};
        let asked_max = "gpt-5.5 doesn't take effort max; it takes low, medium, high, xhigh";
        let cases: [(&str, Effort, &[Step], &str); 3] = [
            (
                "gpt-5.5",
                Effort::High,
                &[(None, Some(Effort::Max))],
                asked_max,
            ),
            (
                "gpt-6.1-sol",
                Effort::Max,
                &[(Some("gpt-5.5"), None)],
                "gpt-5.5 doesn't take the current effort max; it takes low, medium, high, xhigh",
            ),
            (
                "gpt-6.1-sol",
                Effort::High,
                &[(Some("gpt-5.5"), None), (None, Some(Effort::Max))],
                asked_max,
            ),
        ];
        for (model, effort, steps, want) in cases {
            let endpoint = ResolvedEndpoint {
                client_version: None,
                name: EndpointName::parse("openai-test").unwrap(),
                provider: ProviderKind::OpenAi,
                credential: Credential::ApiKey("synthetic-openai-key".into()),
                base_url: wisp_core::provider::openai::DEFAULT_BASE_URL.into(),
                source: KeySource::Environment,
                key_env: None,
                url_source: UrlSource::Official,
                pricing: PricingMode::List,
            };
            let provider =
                wisp_core::provider::openai::OpenAiProvider::from_endpoint(&endpoint).unwrap();
            let mut session = wisp_core::agent::default_session();
            session.model = model.to_owned();
            session.effort = effort;
            let agent = wisp_core::agent::Agent::new(std::sync::Arc::new(provider), session);
            let (commands, events, _task) = wisp_core::agent::spawn(agent);
            let kernel = refusal(commands, events, steps).await;
            let script = Script::new(Vec::new()).with_settings(replay_on(model, effort));
            let (commands, events) = spawn(script, Duration::from_millis(1));
            let replayed = refusal(commands, events, steps).await;
            assert_eq!((kernel.as_str(), replayed.as_str()), (want, want));
        }
    }

    #[test]
    fn a_replay_describes_the_catalog_so_an_unset_tools_list_shows_its_defaults() {
        let settings = replay_on("claude-sonnet-5-5", Effort::Medium);
        let default_on = |name: &str| {
            settings
                .described
                .plugins
                .iter()
                .flat_map(|p| &p.tools)
                .find(|t| t.name == name)
                .map(|t| t.default_on)
        };
        assert_eq!(default_on("bash"), Some(true));
        assert_eq!(default_on("write_todos"), Some(true));
        assert_eq!(default_on("spawn_agent"), Some(true));
        assert_eq!(default_on("report"), Some(false));
    }
}
