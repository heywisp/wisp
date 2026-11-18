use wisp_protocol::log::{FeatureState, LEAD_SENT, LogRecord, LoggedBlock, StoredSource, TurnLead};
use wisp_protocol::settings::{Applies, CACHE_KEPT, Kind, Percent, SettingSpec, SettingsSnapshot};
use wisp_protocol::{BlobRef, CacheTtl, Effort, ImageData, MessageTime};

use super::attachments::named_call;
use super::log::scratch;
use super::marker::{change, image_refused};
use super::*;
use crate::agent::settings::KERNEL_NAMES;
use crate::agent::testing::Prints;
use crate::agent::tool_tests::flag;
use crate::clock::StillClock;
use crate::session::{Line, reread};
use crate::test_guard::{self, RED_PIXEL as PIXEL, ScratchDir, records};
use crate::tool::{Toolbox, snap, testkit};

const SONNET: &str = "claude-sonnet-5-5";

fn documented_as(inner: &Fake, bound: bool) -> Arc<Fake> {
    inner.view(|k| {
        k.documented = Some(!bound);
        k.bound = Some(bound);
        k.keeps_warm = None;
        k.media = false;
    })
}

#[derive(Clone, Copy)]
enum Endpoints {
    One,
    Two,
    TwoOnOther,
    DocumentedOne,
    DocumentedTwo,
    DocumentedOther,
}

fn connected(agent: Agent, provider: Arc<Fake>, endpoints: Endpoints) -> Agent {
    let documented: Arc<dyn Provider> = documented_as(&provider, false);
    let scripted: Arc<dyn Provider> = provider;
    let connection = |name: &str, provider: &Arc<dyn Provider>| Connection {
        name: name.into(),
        provider: provider.clone(),
        context: LogContext {
            endpoint_name: name.into(),
            ..log_context()
        },
        priced: true,
    };
    let (list, active) = match endpoints {
        Endpoints::One => return agent,
        Endpoints::Two => (
            vec![connection("env", &scripted), connection("other", &scripted)],
            "env",
        ),
        Endpoints::TwoOnOther => (
            vec![connection("env", &scripted), connection("other", &scripted)],
            "other",
        ),
        Endpoints::DocumentedOne => (vec![connection("env", &documented)], "env"),
        Endpoints::DocumentedTwo => (
            vec![
                connection("env", &documented),
                connection("other", &documented),
            ],
            "env",
        ),
        Endpoints::DocumentedOther => (vec![connection("other", &documented)], "other"),
    };
    agent.with_connections(list, vec![], active)
}

fn copy_blob(from: &MemoryStore, writer: &mut MemoryStore, blob: &BlobRef) {
    let found = from.get_blob(&blob.sha256, &blob.media_type).unwrap();
    writer
        .put_blob(&blob.sha256, &found.media_type, &found.bytes)
        .unwrap();
}

fn copy_media(from: &MemoryStore, writer: &mut MemoryStore, blocks: &[LoggedBlock]) {
    for block in blocks {
        match block {
            LoggedBlock::Media {
                source: StoredSource::Blob(blob),
            } => copy_blob(from, writer, blob),
            LoggedBlock::ToolResult { content, .. } => copy_media(from, writer, content),
            LoggedBlock::Inline(_) => {}
        }
    }
}

fn copy_all(from: &MemoryStore, records: &[LogRecord]) -> MemoryStore {
    let store = MemoryStore::default();
    let mut writer = store.clone();
    for record in records {
        writer.append("s1", record).unwrap();
        match &record.event {
            LogEvent::TurnStarted { images, .. } => {
                for image in images {
                    let blob = BlobRef {
                        media_type: image.media_type.into(),
                        sha256: image.sha256.clone(),
                        bytes: image.bytes,
                    };
                    copy_blob(from, &mut writer, &blob);
                }
            }
            LogEvent::MessageCommitted { message, .. } => {
                copy_media(from, &mut writer, &message.content);
            }
            _ => {}
        }
    }
    store
}

fn live_agent(provider: Arc<Fake>, store: &MemoryStore, endpoints: Endpoints) -> Agent {
    let agent = Agent::new(provider.clone(), default_session())
        .with_clock(Arc::new(StillClock))
        .with_log(
            SessionLog::new(Box::new(store.clone()), "s1"),
            log_context(),
        );
    connected(agent, provider, endpoints)
}

fn resumed_agent(
    provider: Arc<Fake>,
    from: &MemoryStore,
    records: &[LogRecord],
    endpoints: Endpoints,
) -> Result<Agent, String> {
    resumed_with(provider, from, records, endpoints, |agent| agent)
}

fn resumed_with(
    provider: Arc<Fake>,
    from: &MemoryStore,
    records: &[LogRecord],
    endpoints: Endpoints,
    build: impl FnOnce(Agent) -> Agent,
) -> Result<Agent, String> {
    let store = copy_all(from, records);
    let seq = u64::try_from(records.len()).unwrap();
    let agent = Agent::new(provider.clone(), default_session()).with_clock(Arc::new(StillClock));
    build(connected(agent, provider, endpoints))
        .with_log(
            SessionLog::continuing(Box::new(store), "s1", seq),
            log_context(),
        )
        .resume(records, 0)
}

#[tokio::test]
async fn a_summary_immediately_after_resume_marks_only_the_unchanged_logged_anchor() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 1_000),
        answer("Two.", 1_100),
        compaction(SUMMARY),
    ]);
    provider.knobs(|k| k.prefix_bytes = Some(Prints::Real));
    let mut live = Live::start(live_agent(provider.clone(), &store, Endpoints::One));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    let records = records(&store);
    live.compact(2, None).await;
    live.close().await;
    assert!(
        provider.body(2)["messages"]
            .to_string()
            .contains("\"cache_control\"")
    );
    for changed in [false, true] {
        let next = scripted(vec![compaction(SUMMARY)]);
        next.knobs(|k| k.prefix_bytes = Some(Prints::Real));
        let agent = resumed_with(
            next.clone(),
            &store,
            &records,
            Endpoints::One,
            |mut agent| {
                if changed {
                    agent.session.system = Some("Changed system.".into());
                }
                agent
            },
        )
        .unwrap();
        assert!(agent.epoch.sent().is_none());
        let mut again = Live::start(agent);
        again.compact(2, None).await;
        again.close().await;
        assert_eq!(
            next.body(0)["messages"]
                .to_string()
                .contains("\"cache_control\""),
            !changed
        );
        if !changed {
            assert_eq!(next.body(0), provider.body(2));
            assert_eq!(next.beta(0), provider.beta(2));
        }
    }
}

async fn third_turn_after_resume(
    live: Live,
    sent_live: &Arc<Fake>,
    store: &MemoryStore,
    records: &[LogRecord],
    endpoints: Endpoints,
) {
    let mut live = live;
    assert!(completed(&live.turn("u3", "third").await));
    live.close().await;
    let sent_resumed = scripted(vec![answer("Three.", 1_200)]);
    let agent = resumed_agent(sent_resumed.clone(), store, records, endpoints).unwrap();
    let mut again = Live::start(agent);
    assert!(completed(&again.turn("u3", "third").await));
    again.close().await;
    assert_sends_live_last(&sent_resumed, sent_live, "the third turn");
}

fn assert_sends_live_last(resumed: &Fake, live: &Fake, at: &str) {
    let last = live.sent().len() - 1;
    assert_eq!(resumed.body(0), live.body(last), "{at}");
    assert_eq!(resumed.beta(0), live.beta(last), "{at}");
}

fn three_answers() -> Arc<Fake> {
    scripted(vec![
        answer("One.", 1_000),
        answer("Two.", 1_100),
        answer("Three.", 1_200),
    ])
}

async fn endpoints_changed_between_runs(before: Endpoints, after: Endpoints) {
    let store = MemoryStore::default();
    let provider = three_answers();
    let mut live = Live::start(live_agent(provider.clone(), &store, before));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    let records = records(&store);
    third_turn_after_resume(live, &provider, &store, &records, after).await;
}

#[tokio::test]
async fn binding_controls_follow_the_log_whatever_endpoints_are_configured_now() {
    endpoints_changed_between_runs(Endpoints::Two, Endpoints::Two).await;
    endpoints_changed_between_runs(Endpoints::Two, Endpoints::One).await;
    endpoints_changed_between_runs(Endpoints::One, Endpoints::Two).await;
    endpoints_changed_between_runs(Endpoints::DocumentedTwo, Endpoints::DocumentedOne).await;
    endpoints_changed_between_runs(Endpoints::DocumentedOne, Endpoints::DocumentedTwo).await;
}

#[tokio::test]
async fn an_older_log_that_switched_endpoint_keeps_binding_controls() {
    let store = MemoryStore::default();
    let provider = three_answers();
    let mut live = Live::start(live_agent(
        provider.clone(),
        &store,
        Endpoints::DocumentedTwo,
    ));
    assert!(completed(&live.turn("u1", "first").await));
    change(&mut live, 1, Some("other"), None).await;
    assert!(completed(&live.turn("u2", "second").await));
    let mut records = records(&store);
    for record in &mut records {
        if let LogEvent::TurnStarted {
            binding_controls, ..
        } = &mut record.event
        {
            assert_eq!(*binding_controls, Some(true));
            *binding_controls = None;
        }
    }
    third_turn_after_resume(
        live,
        &provider,
        &store,
        &records,
        Endpoints::DocumentedOther,
    )
    .await;
}

const NEW_STAMP: &str = "Sent: Sunday 2026-10-25 09:00:00 UTC+00:00\n";
const OLD_STAMP: &str = "Sent: Sunday 2026-10-25 09:00 UTC+00:00\n";

fn stamp_blocks(body: &mut Value) -> Vec<&mut Value> {
    body["messages"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .filter(|message| message["role"] == "user")
        .map(|message| {
            message["content"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|block| {
                    block["text"]
                        .as_str()
                        .is_some_and(|t| t.starts_with("Sent: "))
                })
                .unwrap()
        })
        .collect()
}

#[tokio::test]
async fn older_minute_stamps_replay_beside_a_new_seconds_stamp() {
    let store = MemoryStore::default();
    let provider = three_answers();
    let mut live = Live::start(live_agent(provider.clone(), &store, Endpoints::One));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    let mut records = records(&store);
    for record in &mut records {
        if let LogEvent::TurnStarted { lead, .. } = &mut record.event {
            let stamp = lead.entries.iter_mut().find(|e| e.by == LEAD_SENT).unwrap();
            assert_eq!(stamp.text, NEW_STAMP);
            OLD_STAMP.clone_into(&mut stamp.text);
        }
    }
    assert!(completed(&live.turn("u3", "third").await));
    live.close().await;
    let sent_resumed = scripted(vec![answer("Three.", 1_200)]);
    let agent = resumed_agent(sent_resumed.clone(), &store, &records, Endpoints::One).unwrap();
    let mut again = Live::start(agent);
    assert!(completed(&again.turn("u3", "third").await));
    again.close().await;

    let mut expected = provider.body(provider.sent().len() - 1);
    let mut blocks = stamp_blocks(&mut expected);
    assert_eq!(blocks.len(), 3);
    for block in blocks.iter_mut().take(2) {
        block["text"] = json!(OLD_STAMP);
    }
    let mut resumed = sent_resumed.body(0);
    assert_eq!(resumed, expected);
    let blocks = stamp_blocks(&mut resumed);
    assert_eq!(blocks[2]["text"], NEW_STAMP);
}

#[tokio::test]
async fn an_applied_effort_change_resumes_with_the_same_bytes() {
    let store = MemoryStore::default();
    let provider = three_answers();
    let mut live = Live::start(live_agent(provider.clone(), &store, Endpoints::One));
    assert!(completed(&live.turn("u1", "first").await));
    change(&mut live, 1, None, Some(Effort::Low)).await;
    assert!(completed(&live.turn("u2", "second").await));
    let records = records(&store);
    third_turn_after_resume(live, &provider, &store, &records, Endpoints::One).await;
}

#[tokio::test]
async fn a_switched_endpoint_resumes_with_the_same_bytes() {
    let store = MemoryStore::default();
    let provider = three_answers();
    let mut live = Live::start(live_agent(provider.clone(), &store, Endpoints::Two));
    assert!(completed(&live.turn("u1", "first").await));
    change(&mut live, 1, Some("other"), None).await;
    assert!(completed(&live.turn("u2", "second").await));
    let records = records(&store);
    third_turn_after_resume(live, &provider, &store, &records, Endpoints::Two).await;
}

fn option(name: &str, value: SettingValue) -> ClientCommand {
    ClientCommand::ChangeSettings {
        request: RequestId(9),
        changes: vec![SettingChange::named(name, value).unwrap()],
        save: false,
    }
}

async fn set_option(live: &mut Live, name: &str, value: SettingValue) {
    live.send(option(name, value)).await;
    live.until(|e| {
        matches!(
            e,
            AgentEvent::SettingsChanged {
                answers: Some(RequestId(9)),
                ..
            }
        )
    })
    .await;
}

#[tokio::test]
async fn a_change_after_the_last_turn_is_restored_and_sent_as_the_live_session_would() {
    let store = MemoryStore::default();
    let provider = three_answers();
    let mut live = Live::start(live_agent(provider.clone(), &store, Endpoints::One));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    change(&mut live, 1, None, Some(Effort::Low)).await;
    set_option(&mut live, "model", SettingValue::Text(SONNET.into())).await;
    set_option(&mut live, "cache_ttl", SettingValue::Text("5m".into())).await;
    let records = records(&store);
    let agent = resumed_agent(scripted(Vec::new()), &store, &records, Endpoints::One).unwrap();
    assert_eq!(
        agent.session.effort,
        Effort::Medium,
        "the last turn's effort"
    );
    assert_eq!(agent.session.cache_ttl, CacheTtl::OneHour);
    assert_eq!(agent.pending_settings(), Some(("env", SONNET, Effort::Low)));
    assert_eq!(agent.next_cache_ttl(), Some(CacheTtl::FiveMinutes));
    let notice = agent.resume_notice_text().unwrap();
    assert!(!notice.contains("restored"), "{notice}");
    third_turn_after_resume(live, &provider, &store, &records, Endpoints::One).await;
}

const NOT_SENT: [&str; 3] = [
    "context.strategy",
    "instructions.source",
    "self_check.before_end",
];

const SWITCHED_ONLY_FULL_SCREEN: [&str; 2] = ["tools", "turned_off"];

#[derive(Clone, Copy, Debug)]
enum Moment {
    BeforeTurnTwo,
    AfterTurnTwo,
}

fn next_turn(applies: Applies) -> bool {
    match applies {
        Applies::NextTurnKept
        | Applies::NextTurnRebuild
        | Applies::NextTurnMayRebuild
        | Applies::NextTurnEffort => true,
        Applies::Now
        | Applies::NextSession
        | Applies::NextCall
        | Applies::Connection
        | Applies::Reserved => false,
    }
}

fn next_turn_settings() -> Vec<&'static SettingSpec> {
    wisp_protocol::settings::SETTINGS
        .iter()
        .filter(|spec| KERNEL_NAMES.contains(&spec.name))
        .filter(|spec| next_turn(spec.applies) && !spec.per_plugin())
        .collect()
}

fn shown<'a>(snapshot: &'a SettingsSnapshot, name: &str) -> Option<&'a SettingValue> {
    snapshot
        .settings
        .iter()
        .find(|s| s.name.as_str() == name)
        .and_then(|s| s.value.as_ref())
}

fn changed(spec: &SettingSpec, run: &Run) -> SettingValue {
    let now = shown(&run.snapshot, spec.name);
    match spec.kind {
        Kind::Bool => SettingValue::Bool(now != Some(&SettingValue::Bool(true))),
        Kind::Choice(words) => {
            let other = words
                .iter()
                .rfind(|w| now != Some(&SettingValue::Text((**w).to_owned())))
                .unwrap();
            SettingValue::Text((*other).to_owned())
        }
        Kind::Tokens { .. } => match now {
            Some(SettingValue::Tokens(window)) => SettingValue::Tokens(window / 2),
            other => panic!("{}: {other:?}", spec.name),
        },
        Kind::TokensOrPercent => SettingValue::Percent(Percent::new(50).unwrap()),
        Kind::ModelId => SettingValue::Text(SONNET.into()),
        Kind::EndpointName => SettingValue::Text("other".into()),
        Kind::PluginId => SettingValue::Text("not-installed".into()),
        Kind::Preload => SettingValue::List(vec!["none".to_owned()]),
        Kind::ToolNames => SettingValue::List(
            run.agent
                .current_tools()
                .names
                .into_iter()
                .skip(1)
                .collect(),
        ),
        Kind::Names => SettingValue::List(
            run.agent
                .current_tools()
                .names
                .into_iter()
                .take(1)
                .collect(),
        ),
        Kind::Int { .. }
        | Kind::Usd { .. }
        | Kind::ModelIds
        | Kind::AgentName
        | Kind::VarName
        | Kind::VoiceName
        | Kind::Language
        | Kind::Path
        | Kind::List(_)
        | Kind::SectionNames
        | Kind::Secret => panic!("{} takes no next-turn value", spec.name),
    }
}

fn tooled(agent: Agent, scratch: &ScratchDir) -> Agent {
    agent
        .with_tools(testkit::bundled(testkit::ALL), scratch.workspace())
        .with_catalog(testkit::catalog())
        .with_platform("Linux")
        .with_resources(None)
        .with_model_settings(models(super::smaller::WINDOWS))
}

struct Run {
    snapshot: SettingsSnapshot,
    provider: Arc<Fake>,
    agent: Agent,
}

impl Run {
    fn state(&self) -> String {
        format!(
            "{:?} {:?} {} {}",
            self.agent.session,
            self.agent.settings.kernel,
            self.agent.auto_compact(),
            self.agent.endpoint_name()
        )
    }
}

async fn third_turn(mut live: Live, provider: Arc<Fake>) -> Run {
    let snapshot = super::smaller::snapshot(&mut live, 30).await;
    assert!(completed(&live.turn("u3", "third").await));
    let (agent, _) = live.close().await;
    Run {
        snapshot,
        provider,
        agent,
    }
}

async fn live_run(
    scratch: &ScratchDir,
    change: Option<(&str, &SettingValue, Moment)>,
) -> (Run, MemoryStore, Vec<LogRecord>) {
    let store = MemoryStore::default();
    let provider = three_answers();
    let agent = tooled(
        live_agent(provider.clone(), &store, Endpoints::Two),
        scratch,
    );
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    if let Some((name, value, Moment::BeforeTurnTwo)) = change {
        set_option(&mut live, name, value.clone()).await;
    }
    assert!(completed(&live.turn("u2", "second").await));
    if let Some((name, value, Moment::AfterTurnTwo)) = change {
        set_option(&mut live, name, value.clone()).await;
    }
    let records = records(&store);
    (third_turn(live, provider).await, store, records)
}

fn resumed_tooled(
    provider: Arc<Fake>,
    scratch: &ScratchDir,
    store: &MemoryStore,
    records: &[LogRecord],
) -> Agent {
    let logged = crate::resume::LoggedSession::read(records).unwrap();
    let endpoints = match logged.tools_pair().0 {
        Some("other") => Endpoints::TwoOnOther,
        _ => Endpoints::Two,
    };
    resumed_with(provider, store, records, endpoints, |agent| {
        tooled(agent, scratch)
    })
    .unwrap()
}

async fn resumed_run(scratch: &ScratchDir, store: &MemoryStore, records: &[LogRecord]) -> Run {
    let provider = scripted(vec![answer("Three.", 1_200)]);
    let agent = resumed_tooled(provider.clone(), scratch, store, records);
    third_turn(Live::start(agent), provider).await
}

#[tokio::test]
async fn every_next_turn_setting_resumes_as_live() {
    let scratch = scratch();
    let (unchanged, _, _) = live_run(&scratch, None).await;
    let unchanged_sent = unchanged.provider.body(2);
    let rows = next_turn_settings();
    for spec in &rows {
        let value = changed(spec, &unchanged);
        for moment in [Moment::BeforeTurnTwo, Moment::AfterTurnTwo] {
            let at = format!("{} {moment:?}", spec.name);
            let (live, store, records) =
                live_run(&scratch, Some((spec.name, &value, moment))).await;
            let back = resumed_run(&scratch, &store, &records).await;
            assert_eq!(back.snapshot.settings, live.snapshot.settings, "{at}");
            assert_eq!(back.snapshot.context, live.snapshot.context, "{at}");
            assert_sends_live_last(&back.provider, &live.provider, &at);
            assert_eq!(back.state(), live.state(), "{at}");
            assert_ne!(
                shown(&live.snapshot, spec.name),
                shown(&unchanged.snapshot, spec.name),
                "{at}"
            );
            let sent = live.provider.body(2);
            assert_eq!(
                sent == unchanged_sent,
                NOT_SENT.contains(&spec.name) || SWITCHED_ONLY_FULL_SCREEN.contains(&spec.name),
                "{at}"
            );
        }
    }
    let names: Vec<&str> = rows.iter().map(|s| s.name).collect();
    let mut unsent = NOT_SENT.iter().chain(&SWITCHED_ONLY_FULL_SCREEN);
    assert!(unsent.all(|n| names.contains(n)), "{names:?}");
}

fn dated_with_reads(words: &str, input: u64, read: u64) -> Step {
    let mut script = answer(words, input).script;
    script[0] = start(input, read);
    if let Ok(ProviderEvent::MessageStart { model, .. }) = &mut script[0] {
        model.push_str("-20261001");
    }
    let end = script.len() - 2;
    script.splice(end.., finish(StopReason::EndTurn, input, read, 40));
    Step::from(script)
}

#[tokio::test]
async fn each_point_resumes_from_its_cut_as_live_marked_it() {
    let scratch = scratch();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 1_000),
        answer("Two.", 1_100),
        dated_with_reads("Three.", 1_200, 900),
        named_call("t1", "show_diff", json!({}), 1_300),
        answer("Four.", 1_400),
        gave_up(),
    ]);
    let agent = live_agent(provider.clone(), &store, Endpoints::Two)
        .with_clock(crate::clock::TickingClock::new());
    let mut live = Live::start(tooled(agent, &scratch));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    change(&mut live, 1, Some("other"), None).await;
    assert!(completed(&live.turn("u3", "third").await));
    set_option(&mut live, "cache_ttl", SettingValue::Text("5m".into())).await;
    assert!(completed(&live.turn("u4", "fourth").await));
    live.send(ClientCommand::shortcut(
        RequestId(2),
        Some("env".into()),
        Some(SONNET.into()),
        Some(Effort::Low),
    ))
    .await;
    live.until(|e| {
        matches!(
            e,
            AgentEvent::SettingsChanged {
                answers: Some(RequestId(2)),
                ..
            }
        )
    })
    .await;
    set_option(&mut live, "cache_ttl", SettingValue::Text("1h".into())).await;
    let failed = live.turn("u5", "fifth").await;
    assert!(matches!(failed, TurnOutcome::Error { .. }), "{failed:?}");
    let (agent, _) = live.close().await;
    let records = records(&store);
    let resume =
        |records: &[LogRecord]| resumed_tooled(scripted(Vec::new()), &scratch, &store, records);
    let points = [2, 3, 4];
    super::rewind::assert_points_resume_as_live(&agent, &records, &points, resume);

    let stripped: Vec<LogRecord> = records
        .iter()
        .cloned()
        .map(|mut record| {
            if let LogEvent::StepEnded { used, .. } = &mut record.event {
                *used = None;
            }
            record
        })
        .collect();
    super::rewind::assert_cuts_resume_as_marked(&resume(&stripped), &stripped, &points, resume);
}

#[tokio::test]
async fn a_change_before_any_turn_is_restored() {
    let store = MemoryStore::default();
    let provider = three_answers();
    let mut live = Live::start(live_agent(provider.clone(), &store, Endpoints::One));
    change(&mut live, 1, None, Some(Effort::Low)).await;
    let records = records(&store);
    assert!(completed(&live.turn("u1", "first").await));
    live.close().await;
    let sent = scripted(vec![answer("One.", 1_000)]);
    let agent = resumed_agent(sent.clone(), &store, &records, Endpoints::One).unwrap();
    assert_eq!(agent.pending_settings().map(|p| p.2), Some(Effort::Low));
    let mut again = Live::start(agent);
    assert!(completed(&again.turn("u1", "first").await));
    again.close().await;
    assert_eq!(sent.body(0), provider.body(0));
}

#[tokio::test]
async fn a_pending_endpoint_is_restored_while_it_is_configured() {
    let store = MemoryStore::default();
    let provider = three_answers();
    let mut live = Live::start(live_agent(provider.clone(), &store, Endpoints::Two));
    assert!(completed(&live.turn("u1", "first").await));
    change(&mut live, 1, Some("other"), Some(Effort::Low)).await;
    let records = records(&store);
    let resumed =
        |endpoints| resumed_agent(scripted(Vec::new()), &store, &records, endpoints).unwrap();
    let stayed = resumed(Endpoints::Two);
    assert_eq!(
        stayed.pending_settings().map(|p| (p.0, p.2)),
        Some(("other", Effort::Low))
    );
    let moved = resumed(Endpoints::TwoOnOther);
    assert_eq!(moved.endpoint_name(), "other");
    assert_eq!(
        moved.pending_settings().map(|p| (p.0, p.2)),
        Some(("other", Effort::Low))
    );
    let gone = resumed(Endpoints::One);
    assert_eq!(
        gone.pending_settings().map(|p| (p.0, p.2)),
        Some(("env", Effort::Low))
    );
    let notice = gone.resume_notice_text().unwrap();
    assert!(
        notice.ends_with(" Your switch to account other wasn't restored: it can't be used now."),
        "{notice}"
    );
    assert!(!stayed.resume_notice_text().unwrap().contains("restored"));
    third_turn_after_resume(live, &provider, &store, &records, Endpoints::Two).await;
}

#[tokio::test]
async fn changes_undone_before_a_crash_resume_with_the_same_bytes_and_no_notice() {
    let store = MemoryStore::default();
    let provider = three_answers();
    let mut live = Live::start(live_agent(provider.clone(), &store, Endpoints::One));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    change(&mut live, 1, None, Some(Effort::Low)).await;
    change(&mut live, 2, None, Some(Effort::Medium)).await;
    for (name, there, back) in [
        (
            "cache_ttl",
            SettingValue::Text("5m".into()),
            SettingValue::Text("1h".into()),
        ),
        (
            "debug_cache",
            SettingValue::Bool(true),
            SettingValue::Bool(false),
        ),
        (
            "refusal_fallback",
            SettingValue::Bool(false),
            SettingValue::Bool(true),
        ),
    ] {
        set_option(&mut live, name, there).await;
        set_option(&mut live, name, back).await;
    }
    let records = records(&store);
    let agent = resumed_agent(scripted(Vec::new()), &store, &records, Endpoints::One).unwrap();
    let notice = agent.resume_notice_text().unwrap();
    assert!(!notice.contains("restored"), "{notice}");
    third_turn_after_resume(live, &provider, &store, &records, Endpoints::One).await;
}

#[tokio::test]
async fn a_log_with_the_flag_resumed_on_another_endpoint_binds_controls() {
    let store = MemoryStore::default();
    let provider = three_answers();
    let mut live = Live::start(
        live_agent(provider.clone(), &store, Endpoints::DocumentedOne).with_binding_controls(false),
    );
    assert!(completed(&live.turn("u1", "first").await));
    live.close().await;
    let records = records(&store);
    let written: Vec<Option<bool>> = records
        .iter()
        .filter_map(|r| match &r.event {
            LogEvent::TurnStarted {
                binding_controls, ..
            } => Some(*binding_controls),
            _ => None,
        })
        .collect();
    assert_eq!(written, [Some(false)]);
    let resumed = |endpoints| {
        resumed_agent(scripted(Vec::new()), &store, &records, endpoints)
            .unwrap()
            .settings
            .binding_controls
    };
    assert!(!resumed(Endpoints::DocumentedOne));
    assert!(resumed(Endpoints::DocumentedOther));
}

#[tokio::test]
async fn a_withdrawn_attachment_resumes_with_the_same_bytes() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        image_refused(),
        answer("Text only.", 1_000),
        answer("Two.", 1_100),
    ]);
    let mut live = Live::start(live_agent(provider.clone(), &store, Endpoints::One));
    live.send(ClientCommand::Send {
        id: InputId("u1".into()),
        text: "look".into(),
        images: vec![ImageData { data: PIXEL.into() }],
        mode: DeliveryMode::Steer,
        turn: None,
        replaces: None,
    })
    .await;
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let records = records(&store);
    assert!(
        records
            .iter()
            .any(|r| matches!(r.event, LogEvent::AttachmentsWithdrawn { .. }))
    );
    assert!(completed(&live.turn("u2", "second").await));
    live.close().await;

    let sent_resumed = scripted(vec![answer("Two.", 1_100)]);
    let agent = resumed_agent(sent_resumed.clone(), &store, &records, Endpoints::One).unwrap();
    let mut again = Live::start(agent);
    assert!(completed(&again.turn("u2", "second").await));
    again.close().await;
    assert_eq!(sent_resumed.body(0), provider.body(2));
}

fn screenshot_turn(records: &mut Vec<LogRecord>) {
    let turn = 3;
    let step_ended = |step: u32, accepted: bool| LogEvent::StepEnded {
        list_usd: None,
        model: None,
        turn,
        step,
        usage: None,
        cost_usd: None,
        lower_bound: false,
        prompt_tokens: None,
        response_id: None,
        window: None,
        compact_at: None,
        used: None,
        first_pass: None,
        accepted,
        view_len: None,
        started_at: None,
        ttl: None,
        prefix: None,
        queued_ms: None,
        thread: None,
        thread_error: None,
        betas: Vec::new(),
    };
    let committed = |role: Role, content: Vec<ContentBlock>| LogEvent::MessageCommitted {
        turn,
        message: Message { role, content }.into(),
        model: None,
        stop_reason: None,
    };
    let events = vec![
        LogEvent::TurnStarted {
            ends: Vec::new(),
            turn,
            origin: None,
            lead: TurnLead::default(),
            text: "take a screenshot".into(),
            model: None,
            effort: None,
            effort_marker: None,
            images: Vec::new(),
            endpoint_name: None,
            input_id: None,
            inputs: Vec::new(),
            cache_ttl: None,
            debug_cache: None,
            refusal_fallback: None,
            binding_controls: None,
            retry_of: None,
            kept_tools_sha256: None,
            tool_message: Vec::new(),
        },
        committed(
            Role::Assistant,
            vec![ContentBlock::ToolUse {
                id: "toolu_1".into(),
                name: "snap".into(),
                input: json!({}),
            }],
        ),
        step_ended(1, true),
        committed(
            Role::User,
            vec![ContentBlock::ToolResult {
                tool_use_id: "toolu_1".into(),
                content: vec![
                    ContentBlock::Text {
                        text: "a screenshot".into(),
                    },
                    ContentBlock::image(wisp_protocol::ImageType::Png, PIXEL),
                ],
                is_error: false,
            }],
        ),
        step_ended(2, false),
        LogEvent::TurnEnded {
            turn,
            outcome: TurnOutcome::Error {
                kind: ErrorKind::InvalidRequest,
                message: "bad request".into(),
                usage: None,
                cost: None,
                committed_steps: 1,
                detail: None,
                attempts: 0,
                gave_up: None,
                offer: None,
            },
            stats: wisp_protocol::TurnStats::default(),
            partial_output: None,
        },
    ];
    for event in events {
        let at = records.last().unwrap().at.clone();
        records.push(LogRecord {
            schema_version: wisp_protocol::log::LOG_SCHEMA_VERSION,
            seq: u64::try_from(records.len()).unwrap(),
            at,
            event,
        });
    }
}

#[tokio::test]
async fn an_image_no_accepted_request_carried_is_still_withdrawn_after_resume() {
    let store = MemoryStore::default();
    let provider = three_answers();
    let mut live = Live::start(live_agent(provider.clone(), &store, Endpoints::One));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    live.close().await;
    let mut records = records(&store);
    screenshot_turn(&mut records);

    let sent = scripted(vec![image_refused(), answer("Text only.", 1_000)]);
    let agent = resumed_agent(sent.clone(), &store, &records, Endpoints::One).unwrap();
    let mut again = Live::start(agent);
    assert!(completed(&again.turn("u4", "what do you see?").await));
    let (_, seen) = again.close().await;
    assert!(
        seen.iter()
            .any(|e| matches!(e, AgentEvent::AttachmentsWithdrawn { .. })),
        "the tool's image was still provisional"
    );
    let image = |body: &Value| body.to_string().contains(PIXEL);
    assert!(image(&sent.body(0)));
    assert!(!image(&sent.body(1)));
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_turn_resumes_with_the_same_bytes() {
    for close in [false, true] {
        let store = MemoryStore::default();
        let provider = scripted(vec![
            answer("One.", 1_000),
            Step::hang(),
            answer("Three.", 1_200),
        ]);
        let mut live = Live::start(live_agent(provider.clone(), &store, Endpoints::One));
        assert!(completed(&live.turn("u1", "first").await));
        live.send(send("u2", "second")).await;
        live.until(|e| matches!(e, AgentEvent::StepStarted { .. }))
            .await;
        if close {
            let (_, stopped) = live.close().await;
            assert!(stopped.iter().any(|event| matches!(event,
                AgentEvent::InputWithdrawn { ids, by: wisp_protocol::WithdrawnBy::SessionEnded }
                    if ids == &[InputId("u2".into())]
            )));
            let mut records = records(&store);
            let at = crate::clock::Clock::now(&StillClock).to_string();
            for record in &mut records {
                record.at.clone_from(&at);
                if let LogEvent::TurnEnded { stats, .. } = &mut record.event {
                    stats.started_at.clone_from(&at);
                    stats.duration_ms = 0;
                    stats.first_byte_ms = stats.first_byte_ms.map(|_| 0);
                }
            }
            let next = scripted(vec![answer("Three.", 1_200), answer("Four.", 1_200)]);
            let agent = resumed_agent(next.clone(), &store, &records, Endpoints::One).unwrap();
            assert_eq!(agent.returned_inputs()[0].id, InputId("u2".into()));
            let mut again = Live::start(agent);
            assert!(completed(&again.turn("u3", "third").await));
            again
                .send(ClientCommand::ReturnedTaken {
                    ids: vec![InputId("u2".into())],
                })
                .await;
            assert!(completed(&again.turn("u4", "second").await));
            again.close().await;
            let bytes: Vec<_> = next
                .sent()
                .iter()
                .map(|sent| (sent.beta(), sent.text()))
                .collect();
            insta::assert_debug_snapshot!(
                "closed_first_reply_resume_request_bytes",
                (crate::session::sha256_json_hex(&records).unwrap(), bytes,)
            );
        } else {
            live.send(ClientCommand::Cancel).await;
            live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
                .await;
            let records = records(&store);
            third_turn_after_resume(live, &provider, &store, &records, Endpoints::One).await;
        }
    }
}

#[tokio::test]
async fn a_failed_turn_resumes_with_the_same_bytes() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 1_000),
        block_refused(),
        answer("Three.", 1_200),
    ]);
    let mut live = Live::start(live_agent(provider.clone(), &store, Endpoints::One));
    assert!(completed(&live.turn("u1", "first").await));
    let failed = live.turn("u2", "second").await;
    assert!(matches!(failed, TurnOutcome::Error { .. }), "{failed:?}");
    let records = records(&store);
    third_turn_after_resume(live, &provider, &store, &records, Endpoints::One).await;
}

#[tokio::test]
async fn models_that_thought_still_count_as_account_bound_after_resume() {
    let store = MemoryStore::default();
    let provider = three_answers();
    let mut live = Live::start(live_agent(provider.clone(), &store, Endpoints::One));
    assert!(completed(&live.turn("u1", "first").await));
    live.close().await;
    let mut records = records(&store);
    for record in &mut records {
        if let LogEvent::MessageCommitted { message, model, .. } = &mut record.event
            && message.role == Role::Assistant
        {
            message.content.insert(
                0,
                LoggedBlock::Inline(ContentBlock::Thinking {
                    thinking: "hm".into(),
                    signature: "sig".into(),
                }),
            );
            *model = Some(SONNET.into());
        }
    }
    let scripted: Arc<dyn Provider> = provider.clone();
    let bound: Arc<dyn Provider> = documented_as(&provider, true);
    let connection = |name: &str, provider: Arc<dyn Provider>| Connection {
        name: name.into(),
        provider,
        context: LogContext {
            endpoint_name: name.into(),
            ..log_context()
        },
        priced: true,
    };
    let seq = u64::try_from(records.len()).unwrap();
    let agent = Agent::new(scripted.clone(), default_session())
        .with_clock(Arc::new(StillClock))
        .with_connections(
            vec![connection("env", scripted), connection("other", bound)],
            vec![],
            "env",
        )
        .with_log(
            SessionLog::continuing(Box::new(copy_all(&store, &records)), "s1", seq),
            log_context(),
        )
        .resume(&records, 0)
        .unwrap();
    let mut again = Live::start(agent);
    change(&mut again, 1, Some("other"), None).await;
    let (_, seen) = again.close().await;
    let bound: Vec<&Vec<String>> = pick!(
        &seen,
        AgentEvent::SettingsChanged { account_bound, .. } => account_bound
    );
    assert_eq!(bound, [&vec![SONNET.to_owned()]]);
}

#[tokio::test]
async fn a_tool_s_attachment_resumes_with_the_same_bytes() {
    let scratch = scratch();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        named_call("t1", "snap", json!({"pdf": true}), 1_000),
        answer("Read it.", 1_100),
        answer("Two.", 1_200),
    ]);
    let tools = || Toolbox::new([snap::tool()]);
    let agent = live_agent(provider.clone(), &store, Endpoints::One)
        .with_tools(tools(), scratch.workspace())
        .with_platform("Linux")
        .with_resources(None);
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "read the pdf").await));
    let records = records(&store);
    assert!(records.iter().any(|r| matches!(
        &r.event,
        LogEvent::ToolCallFinished { attachments, .. } if !attachments.is_empty()
    )));
    assert!(completed(&live.turn("u2", "second").await));
    live.close().await;

    let pdf = wisp_media::encode_base64(snap::PDF);
    let sent_resumed = scripted(vec![answer("Two.", 1_200)]);
    let copy = copy_all(&store, &records);
    let seq = u64::try_from(records.len()).unwrap();
    let agent = Agent::new(sent_resumed.clone(), default_session())
        .with_clock(Arc::new(StillClock))
        .with_tools(tools(), scratch.workspace())
        .with_platform("Linux")
        .with_resources(None)
        .with_log(
            SessionLog::continuing(Box::new(copy), "s1", seq),
            log_context(),
        )
        .resume(&records, 0)
        .unwrap();
    let mut again = Live::start(agent);
    assert!(completed(&again.turn("u2", "second").await));
    again.close().await;
    assert!(sent_resumed.body(0)["messages"].to_string().contains(&pdf));
    assert_eq!(sent_resumed.body(0), provider.body(2));
}

#[tokio::test]
async fn a_failed_first_step_after_resume_logs_no_request_entry() {
    let store = MemoryStore::default();
    let provider = three_answers();
    let mut live = Live::start(live_agent(provider.clone(), &store, Endpoints::One));
    assert!(completed(&live.turn("u1", "first").await));
    live.close().await;
    let records = records(&store);
    assert!(records.iter().any(|r| matches!(
        r.event,
        LogEvent::StepEnded {
            view_len: Some(_),
            ..
        }
    )));

    let copy = copy_all(&store, &records);
    let seq = u64::try_from(records.len()).unwrap();
    let sent = scripted(vec![block_refused()]);
    let agent = Agent::new(sent.clone(), default_session())
        .with_clock(Arc::new(StillClock))
        .with_log(
            SessionLog::continuing(Box::new(copy.clone()), "s1", seq),
            log_context(),
        )
        .resume(&records, 0)
        .unwrap();
    let mut again = Live::start(agent);
    let failed = again.turn("u2", "second").await;
    assert!(matches!(failed, TurnOutcome::Error { .. }), "{failed:?}");
    again.close().await;
    let after = log_events(&copy);
    let steps: Vec<_> = after
        .iter()
        .skip(records.len())
        .filter_map(|e| match e {
            LogEvent::StepEnded {
                view_len,
                started_at,
                ttl,
                prefix,
                ..
            } => Some((*view_len, started_at.clone(), *ttl, prefix.clone())),
            _ => None,
        })
        .collect();
    assert!(!steps.is_empty());
    assert!(
        steps.iter().all(|s| *s == (None, None, None, None)),
        "{steps:?}"
    );
}

#[tokio::test]
async fn a_viewed_file_resumes_with_the_same_bytes() {
    let scratch = scratch();
    let picture = wisp_media::decode_base64(PIXEL).unwrap();
    std::fs::write(scratch.workspace().root.join("pixel.png"), &picture).unwrap();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        named_call("v1", "view_file", json!({"path": "pixel.png"}), 1_000),
        answer("Seen it.", 1_100),
        answer("Two.", 1_200),
    ]);
    let tools = || Toolbox::new([crate::tool::testkit::view_file()]);
    let agent = live_agent(provider.clone(), &store, Endpoints::One)
        .with_tools(tools(), scratch.workspace())
        .with_platform("Linux")
        .with_resources(None);
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "look at the picture").await));
    let records = records(&store);
    assert!(records.iter().any(|r| matches!(
        &r.event,
        LogEvent::ToolCallFinished { attachments, .. } if !attachments.is_empty()
    )));
    assert!(completed(&live.turn("u2", "second").await));
    live.close().await;

    let sent_resumed = scripted(vec![answer("Two.", 1_200)]);
    let copy = copy_all(&store, &records);
    let seq = u64::try_from(records.len()).unwrap();
    let agent = Agent::new(sent_resumed.clone(), default_session())
        .with_clock(Arc::new(StillClock))
        .with_tools(tools(), scratch.workspace())
        .with_platform("Linux")
        .with_resources(None)
        .with_log(
            SessionLog::continuing(Box::new(copy), "s1", seq),
            log_context(),
        )
        .resume(&records, 0)
        .unwrap();
    let mut again = Live::start(agent);
    assert!(completed(&again.turn("u2", "second").await));
    again.close().await;
    assert!(sent_resumed.body(0)["messages"].to_string().contains(PIXEL));
    assert_eq!(sent_resumed.body(0), provider.body(2));
}

#[tokio::test]
async fn a_resumed_ledger_keeps_a_tool_s_attachments() {
    let scratch = scratch();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        named_call("t1", "snap", json!({"pdf": true}), 1_000),
        image_refused(),
        named_call("q1", "confirm", json!({}), 1_050),
        answer("Read it.", 1_100),
        compaction(SUMMARY),
    ]);
    let tools = || Toolbox::new([snap::tool(), crate::tool::confirm_tool()]);
    let agent = live_agent(provider.clone(), &store, Endpoints::One)
        .with_tools(tools(), scratch.workspace())
        .with_user_input(true)
        .with_platform("Linux")
        .with_resources(None);
    let mut live = Live::start(agent);
    live.send(send("u1", "read the pdf")).await;
    live.until(|e| matches!(e, AgentEvent::QuestionAsked { .. }))
        .await;
    live.send(ClientCommand::Answer {
        call_id: "q1".into(),
        answers: vec![wisp_protocol::Answer::choose(0)],
    })
    .await;
    let ended = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(
        matches!(&ended, AgentEvent::TurnEnded { outcome, .. } if completed(outcome)),
        "{ended:?}"
    );
    let records = records(&store);
    let withdrawals = records
        .iter()
        .filter(|r| matches!(r.event, LogEvent::AttachmentsWithdrawn { .. }))
        .count();
    assert_eq!(withdrawals, 1);
    live.compact(2, None).await;
    let (agent, _) = live.close().await;
    let live_head = head_text(&agent);
    assert!(
        live_head.contains("attached application/pdf (withdrawn)"),
        "{live_head}"
    );
    assert!(
        live_head.contains("Turn 1: The user answered:"),
        "{live_head}"
    );

    let sent_resumed = scripted(vec![compaction(SUMMARY)]);
    let copy = copy_all(&store, &records);
    let seq = u64::try_from(records.len()).unwrap();
    let agent = Agent::new(sent_resumed.clone(), default_session())
        .with_clock(Arc::new(StillClock))
        .with_tools(tools(), scratch.workspace())
        .with_user_input(true)
        .with_platform("Linux")
        .with_resources(None)
        .with_log(
            SessionLog::continuing(Box::new(copy), "s1", seq),
            log_context(),
        )
        .resume(&records, 0)
        .unwrap();
    let mut again = Live::start(agent);
    again.compact(2, None).await;
    let (agent, _) = again.close().await;
    assert_eq!(head_text(&agent), live_head);
}

fn flagged_agent(
    from: &MemoryStore,
    records: &[LogRecord],
    flags: Vec<crate::settings::Given>,
) -> Agent {
    let store = copy_all(from, records);
    let seq = u64::try_from(records.len()).unwrap();
    Agent::new(scripted(Vec::new()), default_session())
        .with_clock(Arc::new(StillClock))
        .with_start_layers(crate::settings::host::StartLayers {
            flags,
            ..Default::default()
        })
        .with_log(
            SessionLog::continuing(Box::new(store), "s1", seq),
            log_context(),
        )
        .resume(records, 0)
        .unwrap()
}

#[tokio::test]
async fn session_values_are_restored_and_a_flag_given_on_resume_wins() {
    let store = MemoryStore::default();
    let provider = three_answers();
    let mut live = Live::start(live_agent(provider.clone(), &store, Endpoints::One));
    assert!(completed(&live.turn("u1", "first").await));
    set_option(&mut live, "time", SettingValue::Text("utc".into())).await;
    set_option(
        &mut live,
        "retry.budget_usd",
        SettingValue::Usd { micros: 2_000_000 },
    )
    .await;
    assert!(completed(&live.turn("u2", "second").await));
    set_option(&mut live, "cache_ttl", SettingValue::Text("5m".into())).await;
    set_option(
        &mut live,
        "display.theme",
        SettingValue::Text("light".into()),
    )
    .await;
    let records = records(&store);

    let restored = flagged_agent(&store, &records, Vec::new());
    assert_eq!(restored.retry.budget_usd, Some(2.0));
    assert_eq!(restored.session.message_time, MessageTime::Utc);
    let names: Vec<&str> = restored
        .settings
        .session
        .iter()
        .map(|g| g.target.spec().name)
        .collect();
    assert_eq!(names, ["cache_ttl", "retry.budget_usd", "time"]);
    assert_eq!(restored.next_cache_ttl(), Some(CacheTtl::FiveMinutes));

    let flagged = flagged_agent(
        &store,
        &records,
        vec![
            flag("time", "local"),
            flag("cache_ttl", "1h"),
            flag("effort", "low"),
            flag("model", SONNET),
        ],
    );
    assert_eq!(
        flagged.session.message_time,
        MessageTime::Utc,
        "a Retry keeps it"
    );
    assert_eq!(flagged.next_message_time(), Some(MessageTime::Local));
    assert_eq!(flagged.next_cache_ttl(), None, "the flag keeps 1h");
    assert_eq!(flagged.session.effort, Effort::Medium, "a Retry keeps it");
    assert_eq!(
        flagged.session.model, restored.session.model,
        "a Retry keeps it"
    );
    assert_eq!(
        flagged.pending_settings(),
        Some(("env", SONNET, Effort::Low))
    );
    let names: Vec<&str> = flagged
        .settings
        .session
        .iter()
        .map(|g| g.target.spec().name)
        .collect();
    assert_eq!(names, ["retry.budget_usd"]);

    third_turn_after_resume(live, &provider, &store, &records, Endpoints::One).await;
    let store = MemoryStore::default();
    let mut agent = live_agent(
        scripted(vec![answer("placeholder", 1_000)]),
        &store,
        Endpoints::One,
    );
    agent.session.model = "gemini-4-carbon-high".into();
    agent.session.effort = Effort::Medium;
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "placeholder").await));
    live.close().await;
    let records = test_guard::records(&store);
    for flags in [Vec::new(), vec![flag("effort", "low")]] {
        let restored = flagged_agent(&store, &records, flags);
        assert_eq!(restored.session.effort, Effort::High);
        assert!(restored.pending_settings().is_none());
        let mut live = Live::start(restored);
        live.send(ClientCommand::shortcut(
            RequestId(8),
            None,
            None,
            Some(Effort::High),
        ))
        .await;
        let changed = live
            .until(|e| {
                matches!(
                    e,
                    AgentEvent::SettingsChanged {
                        answers: Some(RequestId(8)),
                        ..
                    }
                )
            })
            .await;
        assert!(
            matches!(
                changed,
                AgentEvent::SettingsChanged {
                    cache: CacheEffect::Kept,
                    ..
                }
            ),
            "{changed:?}"
        );
        live.close().await;
    }
}

#[tokio::test]
async fn a_cache_ttl_set_back_before_the_next_turn_leaves_nothing_next() {
    let store = MemoryStore::default();
    let mut live = Live::start(live_agent(three_answers(), &store, Endpoints::One));
    assert!(completed(&live.turn("u1", "first").await));
    set_option(&mut live, "cache_ttl", SettingValue::Text("5m".into())).await;
    set_option(&mut live, "cache_ttl", SettingValue::Text("1h".into())).await;
    let (agent, _) = live.close().await;
    assert_eq!(agent.next_cache_ttl(), None);
}

#[tokio::test]
async fn each_next_turn_flag_given_on_resume_is_named_in_order() {
    let store = MemoryStore::default();
    let mut live = Live::start(live_agent(three_answers(), &store, Endpoints::One));
    assert!(completed(&live.turn("u1", "first").await));
    let records = records(&store);
    let on = |name, on| crate::settings::Given {
        value: SettingValue::Bool(on),
        ..flag(name, "")
    };
    let flags = vec![
        on("context.context_note", false),
        flag("time", "utc"),
        on("refusal_fallback", false),
        on("debug_cache", true),
        flag("cache_ttl", "5m"),
    ];
    let agent = flagged_agent(&store, &records, flags);
    let notice = agent.resume_notice_text().unwrap();
    let named = format!(
        "From your next message: cache TTL 5m (may rebuild the cache), \
        cache debugging on (may rebuild the cache), refusal fallback off ({CACHE_KEPT}), \
        time stamps utc ({CACHE_KEPT}), context note off ({CACHE_KEPT})."
    );
    assert!(notice.ends_with(&named), "{notice}");
}

#[tokio::test]
async fn a_time_or_context_line_change_after_the_last_turn_is_restored() {
    let store = MemoryStore::default();
    let provider = three_answers();
    let mut live = Live::start(live_agent(provider.clone(), &store, Endpoints::One));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    set_option(&mut live, "time", SettingValue::Text("utc".into())).await;
    set_option(&mut live, "context.context_note", SettingValue::Bool(false)).await;
    let records = records(&store);
    let agent = resumed_agent(scripted(Vec::new()), &store, &records, Endpoints::One).unwrap();
    assert_eq!(agent.session.message_time, MessageTime::Local);
    assert!(agent.session.context_note);
    assert_eq!(agent.next_message_time(), Some(MessageTime::Utc));
    assert_eq!(agent.next_context_note(), Some(false));
    let notice = agent.resume_notice_text().unwrap();
    assert!(!notice.contains("restored"), "{notice}");
    third_turn_after_resume(live, &provider, &store, &records, Endpoints::One).await;
}

fn gave_up() -> Step {
    failing(classified(
        ErrorKind::Server,
        "boom",
        FailurePhase::Response,
        RetryClass::Never,
    ))
}

fn given(name: &str, value: SettingValue) -> crate::settings::Given {
    crate::settings::Given {
        target: wisp_protocol::settings::Target::Name(wisp_protocol::settings::find(name).unwrap()),
        value,
        origin: format!("--{name}"),
    }
}

async fn retried(live: &mut Live) -> TurnOutcome {
    live.send(ClientCommand::Retry {
        turn: 2,
        endpoint: None,
        request: RequestId(7),
    })
    .await;
    match live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await
    {
        AgentEvent::TurnEnded { outcome, .. } => outcome,
        _ => unreachable!(),
    }
}

async fn retry_after_resume_matches_live(
    changes: Vec<(&str, SettingValue)>,
    flags: Vec<crate::settings::Given>,
) {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 1_000),
        gave_up(),
        answer("Two.", 1_100),
    ]);
    let mut live = Live::start(live_agent(provider.clone(), &store, Endpoints::One));
    assert!(completed(&live.turn("u1", "first").await));
    let failed = live.turn("u2", "second").await;
    assert!(matches!(failed, TurnOutcome::Error { .. }), "{failed:?}");
    for (name, value) in changes {
        set_option(&mut live, name, value).await;
    }
    let records = records(&store);
    assert!(completed(&retried(&mut live).await));
    live.close().await;
    let sent_live = provider.body(2);
    let text = sent_live.to_string();
    assert!(text.contains("Sent: "), "{text}");
    assert!(text.contains("Wisp compacts at"), "{text}");

    let sent_resumed = scripted(vec![answer("Two.", 1_100)]);
    let agent = Agent::new(sent_resumed.clone(), default_session())
        .with_clock(Arc::new(StillClock))
        .with_start_layers(crate::settings::host::StartLayers {
            flags,
            ..Default::default()
        });
    let seq = u64::try_from(records.len()).unwrap();
    let agent = connected(agent, sent_resumed.clone(), Endpoints::One)
        .with_log(
            SessionLog::continuing(Box::new(copy_all(&store, &records)), "s1", seq),
            log_context(),
        )
        .resume(&records, 0)
        .unwrap();
    let mut again = Live::start(agent);
    assert!(completed(&retried(&mut again).await));
    again.close().await;
    assert_eq!(sent_resumed.body(0), sent_live);
}

#[tokio::test]
async fn a_time_change_after_a_failure_waits_for_the_next_turn_on_a_resumed_retry() {
    retry_after_resume_matches_live(vec![("time", SettingValue::Text("off".into()))], vec![]).await;
}

#[tokio::test]
async fn a_context_line_change_after_a_failure_waits_for_the_next_turn_on_a_resumed_retry() {
    retry_after_resume_matches_live(
        vec![
            ("context.context_note", SettingValue::Bool(false)),
            ("context.auto_compact", SettingValue::Bool(false)),
        ],
        vec![],
    )
    .await;
}

#[tokio::test]
async fn time_and_context_line_flags_on_resume_leave_a_retry_alone() {
    retry_after_resume_matches_live(
        vec![],
        vec![
            given("time", SettingValue::Text("off".into())),
            given("context.context_note", SettingValue::Bool(false)),
        ],
    )
    .await;
}

#[test]
fn a_flag_beats_a_restored_value_at_any_site_of_its_setting() {
    use wisp_protocol::settings::{Place, SettingTarget, SitePath, find};
    let spec = find("compact_at").unwrap();
    let at_model = SettingTarget::Path(SitePath::new(Place::Model("claude-opus-5-5".into()), spec));
    let mut flagged = Agent::new(scripted(Vec::new()), default_session()).with_start_layers(
        crate::settings::host::StartLayers {
            flags: vec![given("compact_at", SettingValue::Tokens(400_000))],
            ..Default::default()
        },
    );
    assert!(!flagged.restore_session(&at_model, Some(SettingValue::Tokens(300_000))));
    assert!(flagged.settings.session.is_empty());
    let mut plain = Agent::new(scripted(Vec::new()), default_session());
    assert!(plain.restore_session(&at_model, Some(SettingValue::Tokens(300_000))));
    assert_eq!(plain.settings.session.len(), 1);
}

fn transformed(words: &str, input: u64, list: &Value) -> Step {
    let mut script = answer(words, input).script;
    script.insert(
        1,
        Ok(ProviderEvent::InputTransformations {
            provider: "anthropic".into(),
            transformations: serde_json::from_value(list.clone()).unwrap(),
        }),
    );
    Step::from(script)
}

#[tokio::test]
async fn input_transformations_are_logged_quietly_and_resume_sends_the_same_bytes() {
    let store = MemoryStore::default();
    let list = json!([{
        "type": "thinking_dropped",
        "path": "messages.1.content.0",
        "reason": "model_binding_mismatch"
    }]);
    let provider = scripted(vec![
        transformed("One.", 1_000, &json!([])),
        transformed("Two.", 1_100, &list),
        answer("Three.", 1_200),
    ]);
    let mut live = Live::start(live_agent(provider.clone(), &store, Endpoints::One));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    let seen: Vec<usize> = pick!(
        &live.seen,
        AgentEvent::InputTransformations {
            transformations, ..
        } => transformations.len()
    );
    assert_eq!(seen, [0, 1]);
    assert!(
        !live
            .seen
            .iter()
            .any(|e| matches!(e, AgentEvent::UnknownProviderEvent { .. }))
    );
    let records = records(&store);
    let logged: Vec<(Option<u64>, Value)> = records
        .iter()
        .filter_map(|r| match &r.event {
            LogEvent::InputTransformations {
                turn,
                transformations,
                ..
            } => Some((*turn, serde_json::to_value(transformations).unwrap())),
            _ => None,
        })
        .collect();
    assert_eq!(logged, [(Some(1), json!([])), (Some(2), list)]);
    assert!(
        !records
            .iter()
            .any(|r| matches!(r.event, LogEvent::UnknownProviderEvent { .. }))
    );
    third_turn_after_resume(live, &provider, &store, &records, Endpoints::One).await;
}

#[tokio::test]
async fn input_transformations_from_a_late_message_delta_are_logged() {
    let store = MemoryStore::default();
    let list = json!([{
        "type": "thinking_dropped",
        "path": "messages.1.content.0",
        "reason": "model_binding_mismatch"
    }]);
    let mut script = answer("One.", 1_000).script;
    let at = script.len() - 2;
    script.insert(
        at,
        Ok(ProviderEvent::InputTransformations {
            provider: "anthropic".into(),
            transformations: serde_json::from_value(list.clone()).unwrap(),
        }),
    );
    let provider = scripted(vec![Step::from(script)]);
    let mut live = Live::start(live_agent(provider, &store, Endpoints::One));
    assert!(completed(&live.turn("u1", "first").await));
    let logged: Vec<Value> = pick!(
        &log_events(&store),
        LogEvent::InputTransformations {
            turn: Some(1),
            transformations,
            ..
        } => serde_json::to_value(transformations).unwrap()
    );
    assert_eq!(logged, [list]);
}

#[tokio::test]
async fn an_overflow_fallback_that_keeps_turns_resumes_with_the_same_bytes() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 50_000),
        answer("Two.", 100_000),
        answer("Three.", 160_000),
        named_call("toolu_1", "bash", json!({}), 170_000),
        failing(ProviderError::new(
            ErrorKind::ContextOverflow,
            "prompt is too long",
        )),
        compaction(SUMMARY),
        answer("Four.", 900),
        answer("Five.", 1_000),
    ]);
    let agent = live_agent(provider.clone(), &store, Endpoints::One).with_auto_compact(false);
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    assert!(completed(&live.turn("u3", "third").await));
    let o = live.turn("u4", "fourth").await;
    assert!(completed(&o), "{o:?}");
    let records = records(&store);
    assert!(completed(&live.turn("u5", "fifth").await));
    let (agent, _) = live.close().await;
    assert!(agent.compaction.head.as_ref().is_some_and(|h| h.leading()));
    let sent_resumed = scripted(vec![answer("Five.", 1_000)]);
    let resumed = resumed_agent(sent_resumed.clone(), &store, &records, Endpoints::One)
        .unwrap()
        .with_auto_compact(false);
    let mut again = Live::start(resumed);
    assert!(completed(&again.turn("u5", "fifth").await));
    again.close().await;
    let last = provider.sent().len() - 1;
    assert_eq!(sent_resumed.body(0), provider.body(last));
}

#[tokio::test]
async fn a_window_the_provider_stated_is_restored_and_resumes_with_the_same_bytes() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 50_000),
        answer("Two.", 100_000),
        answer("Three.", 160_000),
        failing(ProviderError::new(
            ErrorKind::ContextOverflow,
            "prompt is too long: 250000 tokens > 200000 maximum",
        )),
        compaction(SUMMARY),
        answer("Four.", 900),
        answer("Five.", 1_000),
    ]);
    let agent = live_agent(provider.clone(), &store, Endpoints::One).with_auto_compact(false);
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    assert!(completed(&live.turn("u3", "third").await));
    let o = live.turn("u4", "fourth").await;
    assert!(completed(&o), "{o:?}");
    let records = records(&store);
    assert!(completed(&live.turn("u5", "fifth").await));
    live.close().await;
    let lowered: Vec<u64> = records
        .iter()
        .filter_map(|r| match &r.event {
            LogEvent::WindowLowered { tokens, .. } => Some(*tokens),
            _ => None,
        })
        .collect();
    assert_eq!(lowered, [200_000]);

    let sent_resumed = scripted(vec![answer("Five.", 1_000)]);
    let resumed = resumed_agent(sent_resumed.clone(), &store, &records, Endpoints::One)
        .unwrap()
        .with_auto_compact(false);
    let mut again = Live::start(resumed);
    assert!(completed(&again.turn("u5", "fifth").await));
    again.close().await;
    let last = provider.sent().len() - 1;
    let live_body = provider.body(last);
    assert!(
        live_body.to_string().contains("of 200k tokens"),
        "{live_body}"
    );
    assert_eq!(sent_resumed.body(0), live_body);
}

fn resumed_into(
    provider: Arc<Fake>,
    store: &MemoryStore,
    records: &[LogRecord],
) -> Result<Agent, String> {
    let seq = u64::try_from(records.len()).unwrap();
    let agent = Agent::new(provider.clone(), default_session()).with_clock(Arc::new(StillClock));
    connected(agent, provider, Endpoints::One)
        .with_log(
            SessionLog::continuing(Box::new(store.clone()), "s1", seq),
            log_context(),
        )
        .resume(records, 0)
}

fn compacted_covers(records: &[LogRecord]) -> Vec<Option<(u64, u64)>> {
    records
        .iter()
        .filter_map(|r| match &r.event {
            LogEvent::ContextCompacted { covers, .. } => {
                Some(covers.as_ref().map(|c| (c.first_turn, c.last_turn)))
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_fallback_after_a_resume_keeps_its_turn_range_through_the_next_resume() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 50_000),
        answer("Two.", 100_000),
        answer("Three.", 120_000),
    ]);
    let agent = live_agent(provider.clone(), &store, Endpoints::One).with_auto_compact(false);
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    assert!(completed(&live.turn("u3", "third").await));
    live.close().await;
    let records = records(&store);

    let after = scripted(vec![
        failing(ProviderError::new(
            ErrorKind::ContextOverflow,
            "prompt is too long: 250000 tokens > 200000 maximum",
        )),
        compaction(SUMMARY),
        answer("Four.", 900),
    ]);
    let second = copy_all(&store, &records);
    let resumed = resumed_into(after.clone(), &second, &records)
        .unwrap()
        .with_auto_compact(false);
    assert!(resumed.epoch.entries().iter().all(|e| e.seq.is_some()));
    let mut live = Live::start(resumed);
    let large = "y".repeat(240_000);
    let outcome = live.turn("u4", &large).await;
    let (agent, _) = live.close().await;
    assert!(completed(&outcome), "{outcome:?}");
    let head = agent.compaction.head.as_ref().unwrap();
    assert!(head.leading());
    assert_eq!(head.turns, (1, 3));
    let records = test_guard::records(&second);
    assert_eq!(compacted_covers(&records), [Some((1, 3))]);
    let reply = records
        .iter()
        .filter(|r| {
            matches!(
                &r.event,
                LogEvent::MessageCommitted { message, .. } if message.role == Role::Assistant
            )
        })
        .map(|r| r.seq)
        .nth(2)
        .unwrap();
    let last_seq = records.iter().find_map(|r| match &r.event {
        LogEvent::ContextCompacted { covers, .. } => covers.as_ref().map(|c| c.last_seq),
        _ => None,
    });
    assert_eq!(last_seq, Some(reply - 1));

    let third = copy_all(&second, &records);
    let again = resumed_into(scripted(vec![]), &third, &records).unwrap();
    let head = again.compaction.head.as_ref().unwrap();
    assert_eq!(head.turns, (1, 3));
}

async fn windowed_turn_after_resume(
    live: Live,
    sent_live: &Arc<Fake>,
    store: &MemoryStore,
    records: &[LogRecord],
    (id, text): (&str, &str),
) {
    let windows = models(super::smaller::WINDOWS);
    let mut live = live;
    assert!(completed(&live.turn(id, text).await));
    live.close().await;
    let sent_resumed = scripted(vec![answer("Next.", 1_200)]);
    let copied = copy_all(store, records);
    let seq = u64::try_from(records.len()).unwrap();
    let fresh =
        Agent::new(sent_resumed.clone(), default_session()).with_clock(Arc::new(StillClock));
    let agent = connected(fresh, sent_resumed.clone(), Endpoints::TwoOnOther)
        .with_model_settings(windows)
        .with_log(
            SessionLog::continuing(Box::new(copied), "s1", seq),
            log_context(),
        )
        .resume(records, 0)
        .unwrap();
    assert_eq!(agent.endpoint_name(), "other");
    let mut again = Live::start(agent);
    assert!(completed(&again.turn(id, text).await));
    again.close().await;
    assert_sends_live_last(&sent_resumed, sent_live, id);
}

fn windowed(provider: &Arc<Fake>, store: &MemoryStore) -> Agent {
    live_agent(provider.clone(), store, Endpoints::Two)
        .with_model_settings(models(super::smaller::WINDOWS))
}

#[tokio::test]
async fn a_smaller_window_switch_resumes_with_the_same_bytes() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 300_000),
        compaction(SUMMARY),
        answer("Two.", 2_000),
        answer("Three.", 2_500),
    ]);
    let mut live = Live::start(windowed(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    change(&mut live, 1, Some("other"), None).await;
    assert!(completed(&live.turn("u2", "second").await));
    let records = records(&store);
    assert!(records.iter().any(|r| matches!(
        &r.event,
        LogEvent::ContextCompacted {
            trigger: CompactionTrigger::Switch,
            ..
        }
    )));
    windowed_turn_after_resume(live, &provider, &store, &records, ("u3", "third")).await;
}

#[tokio::test]
async fn a_smaller_window_retry_resumes_with_the_same_bytes() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 150_000),
        answer("Two.", 300_000),
        failing(server_error()),
        compaction(SUMMARY),
        answer("Retried.", 3_000),
        answer("Four.", 3_500),
    ]);
    let mut live = Live::start(windowed(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    let failed = live.turn("u3", "third").await;
    assert!(
        matches!(failed, TurnOutcome::Error { offer: Some(_), .. }),
        "{failed:?}"
    );
    live.send(ClientCommand::Retry {
        turn: 3,
        endpoint: Some("other".into()),
        request: RequestId(2),
    })
    .await;
    let ended = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(
        matches!(
            ended,
            AgentEvent::TurnEnded {
                outcome: TurnOutcome::Completed { .. },
                ..
            }
        ),
        "{ended:?}"
    );
    let records = records(&store);
    assert!(records.iter().any(|r| matches!(
        &r.event,
        LogEvent::ContextCompacted {
            trigger: CompactionTrigger::Overflow,
            ..
        }
    )));
    windowed_turn_after_resume(live, &provider, &store, &records, ("u5", "fifth")).await;
}

async fn retried_on_other(live: &mut Live, request: u64) -> AgentEvent {
    live.send(ClientCommand::Retry {
        turn: 3,
        endpoint: Some("other".into()),
        request: RequestId(request),
    })
    .await;
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await
}

#[tokio::test]
async fn a_resumed_switching_retry_skips_the_fallback_and_holds_the_same_settings() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 150_000),
        answer("Two.", 300_000),
        failing(server_error()),
        compaction(SUMMARY),
        answer("Retried.", 3_000),
    ]);
    let mut live = Live::start(windowed(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    let failed = live.turn("u3", "third").await;
    assert!(
        matches!(failed, TurnOutcome::Error { offer: Some(_), .. }),
        "{failed:?}"
    );
    let records = records(&store);
    let ended = retried_on_other(&mut live, 2).await;
    let (live_agent, seen) = live.close().await;
    assert!(ended_completed(&ended), "{ended:?}");
    assert!(overflow_compacted(&seen));

    let sent_resumed = scripted(vec![answer("Retried.", 3_000)]);
    let agent = resumed_agent(sent_resumed.clone(), &store, &records, Endpoints::Two)
        .unwrap()
        .with_model_settings(models(super::smaller::WINDOWS));
    assert!(agent.retry_offer().is_some());
    assert_eq!(agent.standing_size(), Some((0, 0)));
    let mut again = Live::start(agent);
    let ended = retried_on_other(&mut again, 2).await;
    let (agent, resumed_seen) = again.close().await;
    assert!(ended_completed(&ended), "{ended:?}");
    assert!(!overflow_compacted(&resumed_seen));
    assert_eq!(sent_resumed.sent().len(), 1);
    assert!(
        sent_resumed.body(0)["messages"]
            .to_string()
            .contains("third")
    );
    assert_eq!(agent.endpoint_name(), live_agent.endpoint_name());
    assert_eq!(agent.pending_settings(), live_agent.pending_settings());
}

fn overflow_compacted(seen: &[AgentEvent]) -> bool {
    seen.iter().any(|e| {
        matches!(
            e,
            AgentEvent::CompactionStarted {
                trigger: CompactionTrigger::Overflow,
                ..
            }
        )
    })
}

#[tokio::test]
async fn an_overflow_after_rewinding_a_resumed_session_falls_back_through_the_kept_anchor() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 50_000),
        answer("Two.", 100_000),
        answer("Three.", 120_000),
    ]);
    let agent = live_agent(provider.clone(), &store, Endpoints::One).with_auto_compact(false);
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    assert!(completed(&live.turn("u3", "third").await));
    live.close().await;
    let records = records(&store);
    let next = records.last().unwrap().seq + 1;
    let mut lines = reread(&records).unwrap();
    assert!(matches!(records[1].event, LogEvent::HookRan { .. }));
    lines[1] = Line::Skipped;

    let after = scripted(vec![
        failing(ProviderError::new(
            ErrorKind::ContextOverflow,
            "prompt is too long: 250000 tokens > 200000 maximum",
        )),
        compaction(SUMMARY),
        answer("Four.", 900),
    ]);
    let second = copy_all(&store, &records);
    let agent = Agent::new(after.clone(), default_session()).with_clock(Arc::new(StillClock));
    let resumed = connected(agent, after.clone(), Endpoints::One)
        .with_log(
            SessionLog::continuing(Box::new(second.clone()), "s1", next),
            log_context(),
        )
        .resume_lines(lines, 0)
        .unwrap()
        .with_auto_compact(false);
    let mut live = Live::start(resumed);
    live.send(ClientCommand::Rewind {
        to: wisp_protocol::RewindPoint::Turn { turn: 3 },
        request: RequestId(10),
    })
    .await;
    let done = live
        .until(|e| matches!(e, AgentEvent::Rewound { .. } | AgentEvent::Rejected { .. }))
        .await;
    assert!(matches!(done, AgentEvent::Rewound { .. }), "{done:?}");
    let large = "y".repeat(240_000);
    let outcome = live.turn("u4", &large).await;
    let (agent, _) = live.close().await;
    assert!(completed(&outcome), "{outcome:?}");
    let head = agent.compaction.head.as_ref().unwrap();
    assert!(head.leading());
    assert_eq!(head.turns, (1, 2));

    let written = test_guard::records(&second);
    assert_eq!(compacted_covers(&written), [Some((1, 2))]);
    let reply = written
        .iter()
        .filter(|r| {
            matches!(
                &r.event,
                LogEvent::MessageCommitted { message, .. } if message.role == Role::Assistant
            )
        })
        .map(|r| r.seq)
        .nth(1)
        .unwrap();
    let last_seq = written.iter().find_map(|r| match &r.event {
        LogEvent::ContextCompacted { covers, .. } => covers.as_ref().map(|c| c.last_seq),
        _ => None,
    });
    assert_eq!(last_seq, Some(reply - 1));
}

fn window_in_force(agent: &Agent) -> Option<(u64, wisp_protocol::context::WindowSource)> {
    agent
        .next_model_settings()
        .resolve(agent.endpoint_name(), &agent.session.model, None, 64_000)
        .window
        .map(|w| (w.tokens, w.source))
}

async fn clear_window(live: &mut Live) {
    let mut cleared = SettingChange::named("context_window", SettingValue::Tokens(0)).unwrap();
    cleared.value = None;
    live.send(ClientCommand::ChangeSettings {
        request: RequestId(10),
        changes: vec![cleared],
        save: false,
    })
    .await;
    live.until(|e| {
        matches!(
            e,
            AgentEvent::SettingsChanged {
                answers: Some(RequestId(10)),
                ..
            }
        )
    })
    .await;
}

async fn session_window_over_a_stated_limit(session_window: u64) {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 50_000),
        answer("Two.", 100_000),
        answer("Three.", 160_000),
        failing(ProviderError::new(
            ErrorKind::ContextOverflow,
            "prompt is too long: 250000 tokens > 200000 maximum",
        )),
        compaction(SUMMARY),
        answer("Four.", 900),
    ]);
    let agent = live_agent(provider.clone(), &store, Endpoints::One).with_auto_compact(false);
    let mut live = Live::start(agent);
    for (id, text) in [
        ("u1", "first"),
        ("u2", "second"),
        ("u3", "third"),
        ("u4", "fourth"),
    ] {
        assert!(completed(&live.turn(id, text).await));
    }
    set_option(
        &mut live,
        "context_window",
        SettingValue::Tokens(session_window),
    )
    .await;
    let records = records(&store);
    clear_window(&mut live).await;
    let (live_agent, _) = live.close().await;
    let stated = Some((200_000, wisp_protocol::context::WindowSource::Overflow));
    assert_eq!(window_in_force(&live_agent), stated);

    let resumed = resumed_agent(scripted(Vec::new()), &store, &records, Endpoints::One)
        .unwrap()
        .with_auto_compact(false);
    assert_eq!(
        window_in_force(&resumed).map(|w| w.0),
        Some(session_window.min(200_000))
    );
    let mut again = Live::start(resumed);
    clear_window(&mut again).await;
    let (resumed, _) = again.close().await;
    assert_eq!(window_in_force(&resumed), stated);
}

#[tokio::test]
async fn a_smaller_session_window_keeps_the_stated_limit_across_a_resume() {
    session_window_over_a_stated_limit(150_000).await;
}

#[tokio::test]
async fn a_larger_session_window_keeps_the_stated_limit_across_a_resume() {
    session_window_over_a_stated_limit(300_000).await;
}

fn appended_record(records: &mut Vec<LogRecord>, event: LogEvent) {
    let last = records.last().unwrap();
    let record = LogRecord {
        schema_version: last.schema_version,
        seq: last.seq + 1,
        at: last.at.clone(),
        event,
    };
    records.push(record);
}

fn snap_resumed(
    provider: Arc<Fake>,
    store: MemoryStore,
    records: &[LogRecord],
    scratch: &crate::test_guard::ScratchDir,
) -> Agent {
    let seq = u64::try_from(records.len()).unwrap();
    Agent::new(provider, default_session())
        .with_clock(Arc::new(StillClock))
        .with_tools(Toolbox::new([snap::tool()]), scratch.workspace())
        .with_platform("Linux")
        .with_resources(None)
        .with_log(
            SessionLog::continuing(Box::new(store), "s1", seq),
            log_context(),
        )
        .resume(records, 0)
        .unwrap()
}

#[tokio::test]
async fn a_lost_tool_attachment_binds_controls_and_cuts_thinking_on_a_fallen_back_pair() {
    let scratch = scratch();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        named_call("t1", "snap", json!({"pdf": true}), 1_000),
        answer("Read it.", 1_100),
    ]);
    let agent = live_agent(provider, &store, Endpoints::One)
        .with_tools(Toolbox::new([snap::tool()]), scratch.workspace())
        .with_platform("Linux")
        .with_resources(None);
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "read the pdf").await));
    live.close().await;
    let mut records = records(&store);
    for record in &mut records {
        if let LogEvent::MessageCommitted { message, .. } = &mut record.event
            && message.role == Role::Assistant
            && message
                .content
                .iter()
                .all(|b| !matches!(b, LoggedBlock::Inline(ContentBlock::ToolUse { .. })))
        {
            message.content.insert(
                0,
                LoggedBlock::Inline(ContentBlock::Thinking {
                    thinking: "hm".into(),
                    signature: "sig".into(),
                }),
            );
        }
    }
    appended_record(
        &mut records,
        LogEvent::BindingControls {
            model: "claude-opus-5-5".into(),
            state: FeatureState::FallenBack,
            endpoint_name: Some("env".into()),
        },
    );

    let whole = snap_resumed(
        scripted(Vec::new()),
        copy_all(&store, &records),
        &records,
        &scratch,
    );
    assert!(!whole.settings.binding_controls);

    let gone = stored(&records);
    let mut agent = snap_resumed(scripted(Vec::new()), gone.clone(), &records, &scratch);
    assert!(agent.settings.binding_controls);
    let notice = agent.resume_notice_text().unwrap_or_default().to_owned();
    assert!(
        notice.contains("no longer available, so the cache is rebuilt"),
        "{notice}"
    );
    let note_at = agent
        .history()
        .iter()
        .position(|m| {
            m.content
                .iter()
                .any(|b| matches!(b, ContentBlock::ToolResult { .. }))
        })
        .unwrap();
    let (tx, mut rx) = mpsc::channel(4096);
    agent.start_log(&tx).await;
    drop(tx);
    while rx.recv().await.is_some() {}
    drop(agent);
    let after = test_guard::records(&gone);
    let written: Vec<&LogEvent> = after[records.len()..]
        .iter()
        .map(|r| &r.event)
        .filter(|e| matches!(e, LogEvent::ImageLost { .. } | LogEvent::ThinkingCut { .. }))
        .collect();
    let [
        LogEvent::ImageLost { note, .. },
        LogEvent::ThinkingCut { from, .. },
    ] = written.as_slice()
    else {
        panic!("{written:#?}");
    };
    assert!(
        note.starts_with("[attachment no longer available: application/pdf"),
        "{note}"
    );
    assert_eq!(from.message, note_at);

    let logged = crate::resume::LoggedSession::read(&after).unwrap();
    assert_eq!(logged.current.binding_controls, Some(true));
    let again = snap_resumed(scripted(Vec::new()), gone, &after, &scratch);
    assert!(again.settings.binding_controls);
}

async fn lost_image_notice(
    session: wisp_protocol::SessionConfig,
    layers: crate::settings::host::StartLayers,
) -> String {
    let scratch = scratch();
    let store = MemoryStore::default();
    let provider = scripted(vec![
        named_call("t1", "snap", json!({"pdf": true}), 1_000),
        answer("Read it.", 1_100),
    ]);
    let agent = live_agent(provider, &store, Endpoints::One)
        .with_tools(Toolbox::new([snap::tool()]), scratch.workspace())
        .with_platform("Linux")
        .with_resources(None);
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "read the pdf").await));
    live.close().await;
    let records = records(&store);
    let (_, log) = continued(&records);
    let agent = Agent::new(scripted(Vec::new()), session)
        .with_clock(Arc::new(StillClock))
        .with_start_layers(layers)
        .with_tools(Toolbox::new([snap::tool()]), scratch.workspace())
        .with_platform("Linux")
        .with_resources(None)
        .with_log(log, log_context())
        .resume(&records, 0)
        .unwrap();
    agent.resume_notice_text().unwrap_or_default().to_owned()
}

#[tokio::test]
async fn a_flag_rebuild_names_an_image_lost_since_the_session() {
    let layers = crate::settings::host::StartLayers {
        flags: vec![given(
            "model",
            SettingValue::Text("claude-sonnet-5-5".into()),
        )],
        ..Default::default()
    };
    let notice = lost_image_notice(default_session(), layers).await;
    assert!(
        notice.contains(
            "Your flags change the model to claude-sonnet-5-5, and an image this session sent is no longer available: the cache is rebuilt"
        ),
        "{notice}"
    );
    assert!(
        notice.contains("and the model's earlier reasoning is dropped."),
        "{notice}"
    );
}

#[tokio::test]
async fn a_prompt_rebuild_names_an_image_lost_since_the_session() {
    let session = wisp_protocol::SessionConfig {
        system: Some("You are Wisp, a careful assistant.".into()),
        ..default_session()
    };
    let notice = lost_image_notice(session, Default::default()).await;
    assert!(
        notice.contains(
            "Wisp's prompt or tools changed since this session, and an image this session sent is no longer available: the cache is rebuilt"
        ),
        "{notice}"
    );
}

fn overflowed_on_turn_four(after: Vec<Step>) -> Arc<Fake> {
    let mut steps = vec![
        answer("One.", 50_000),
        answer("Two.", 100_000),
        answer("Three.", 160_000),
        failing(ProviderError::new(
            ErrorKind::ContextOverflow,
            "prompt is too long: 250000 tokens > 200000 maximum",
        )),
        compaction(SUMMARY),
        answer("Four.", 900),
    ];
    steps.extend(after);
    scripted(steps)
}

async fn four_turns(live: &mut Live) {
    for (id, text) in [
        ("u1", "first"),
        ("u2", "second"),
        ("u3", "third"),
        ("u4", "fourth"),
    ] {
        assert!(completed(&live.turn(id, text).await));
    }
}

async fn overflowed_log() -> Vec<LogRecord> {
    let store = MemoryStore::default();
    let agent = live_agent(overflowed_on_turn_four(Vec::new()), &store, Endpoints::One)
        .with_auto_compact(false);
    let mut live = Live::start(agent);
    four_turns(&mut live).await;
    live.close().await;
    records(&store)
}

fn damaged_lines(records: &[LogRecord], damaged: usize) -> Vec<crate::session::Line> {
    records
        .iter()
        .enumerate()
        .map(|(index, r)| {
            if index == damaged {
                crate::session::Line::Damaged(wisp_protocol::log::DamageKind::Unreadable)
            } else {
                crate::session::Line::Record(Box::new(r.clone()))
            }
        })
        .collect()
}

fn resumed_from_lines(
    provider: Arc<Fake>,
    records: &[LogRecord],
    damaged: usize,
) -> (Agent, MemoryStore) {
    resumed_on(provider, records, damaged, Endpoints::One)
}

fn resumed_on(
    provider: Arc<Fake>,
    records: &[LogRecord],
    damaged: usize,
    endpoints: Endpoints,
) -> (Agent, MemoryStore) {
    let (back, log) = continued(records);
    let fresh = Agent::new(provider.clone(), default_session()).with_clock(Arc::new(StillClock));
    let agent = connected(fresh, provider, endpoints)
        .with_log(log, log_context())
        .resume_lines(damaged_lines(records, damaged), 0)
        .unwrap()
        .with_auto_compact(false);
    (agent, back)
}

fn last_reply(records: &[LogRecord]) -> usize {
    records
        .iter()
        .rposition(|r| {
            matches!(
                &r.event,
                LogEvent::MessageCommitted { message, .. } if message.role == Role::Assistant
            )
        })
        .unwrap()
}

fn fell_back_through(records: &[LogRecord]) -> Option<u64> {
    records.iter().find_map(|r| match r.event {
        LogEvent::ResumeFellBack { last_turn, .. } => last_turn,
        _ => None,
    })
}

#[tokio::test]
async fn a_window_stated_in_a_turn_a_fallback_drops_is_still_restored() {
    let records = overflowed_log().await;
    let lowered = records
        .iter()
        .position(|r| matches!(r.event, LogEvent::WindowLowered { .. }))
        .unwrap();
    let damaged = last_reply(&records);
    assert!(lowered < damaged);

    let sent_resumed = scripted(vec![answer("Again.", 1_000)]);
    let (resumed, back) = resumed_from_lines(sent_resumed.clone(), &records, damaged);
    let mut again = Live::start(resumed);
    assert!(completed(&again.turn("u4", "fourth").await));
    again.close().await;
    assert_eq!(fell_back_through(&test_guard::records(&back)), Some(3));
    let body = sent_resumed.body(0).to_string();
    assert!(body.contains("of 200k tokens"), "{body}");
}

#[tokio::test]
async fn a_window_stated_in_a_dropped_turn_matches_a_live_rewind_twin() {
    let records = overflowed_log().await;

    let twin = overflowed_on_turn_four(vec![answer("Five.", 1_000)]);
    let twin_store = MemoryStore::default();
    let agent = live_agent(twin.clone(), &twin_store, Endpoints::One).with_auto_compact(false);
    let mut live = Live::start(agent);
    four_turns(&mut live).await;
    live.send(ClientCommand::Rewind {
        to: wisp_protocol::RewindPoint::Turn { turn: 4 },
        request: RequestId(10),
    })
    .await;
    live.until(|e| matches!(e, AgentEvent::Rewound { .. }))
        .await;
    assert!(completed(&live.turn("u5", "fifth").await));
    live.close().await;

    let sent_resumed = scripted(vec![answer("Five.", 1_000)]);
    let (resumed, back) = resumed_from_lines(sent_resumed.clone(), &records, last_reply(&records));
    let mut again = Live::start(resumed);
    assert!(completed(&again.turn("u5", "fifth").await));
    again.close().await;

    assert_eq!(fell_back_through(&test_guard::records(&back)), Some(3));
    let last = twin.sent().len() - 1;
    let twin_body = twin.body(last);
    assert!(
        twin_body.to_string().contains("of 200k tokens"),
        "{twin_body}"
    );
    assert_eq!(sent_resumed.body(0), twin_body);
}

#[tokio::test]
async fn a_fresh_start_uses_the_window_a_later_resume_restores() {
    let records = overflowed_log().await;

    let twin = scripted(vec![answer("Five.", 1_000), answer("Six.", 1_100)]);
    let (agent, _) = resumed_from_lines(twin.clone(), &records, 0);
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u5", "fifth").await));
    assert!(completed(&live.turn("u6", "sixth").await));
    live.close().await;
    let first = twin.body(0).to_string();
    assert!(first.contains("of 200k tokens"), "{first}");

    let fresh = scripted(vec![answer("Five.", 1_000)]);
    let (agent, back) = resumed_from_lines(fresh.clone(), &records, 0);
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u5", "fifth").await));
    live.close().await;
    let written = test_guard::records(&back);
    assert!(written.iter().any(|r| matches!(
        r.event,
        LogEvent::ResumeFellBack {
            kept_through: None,
            ..
        }
    )));

    let later = scripted(vec![answer("Six.", 1_100)]);
    let (agent, _) = resumed_from_lines(later.clone(), &written, 0);
    let mut again = Live::start(agent);
    assert!(completed(&again.turn("u6", "sixth").await));
    again.close().await;
    assert_eq!(later.body(0), twin.body(1));
}

fn carried_states(agent: &Agent, endpoint: &str, model: &str) -> Vec<crate::agent::PairState> {
    crate::provider::pair_features()
        .map(|feature| agent.pair_state(endpoint, model, feature))
        .collect()
}

#[tokio::test]
async fn a_fresh_start_keeps_the_endpoint_facts_a_later_resume_restores() {
    use crate::agent::PairState;
    use crate::agent::settings::state_record;
    use crate::provider::{Feature, pair_features};
    use crate::resume::{FeatureFact, feature_state};
    let mut records = overflowed_log().await;
    let (endpoint, model) = records
        .iter()
        .find_map(|r| match &r.event {
            LogEvent::WindowLowered {
                endpoint, model, ..
            } => Some((endpoint.clone(), model.clone())),
            _ => None,
        })
        .unwrap();
    let pairs: Vec<(Feature, FeatureState)> = pair_features()
        .zip(
            [FeatureState::FallenBack, FeatureState::Proven]
                .into_iter()
                .cycle(),
        )
        .collect();
    for (feature, state) in &pairs {
        let record = state_record(*feature, &endpoint, &model, *state);
        assert_eq!(
            feature_state(&record),
            Some(FeatureFact::Pair {
                feature: *feature,
                endpoint: Some(&endpoint),
                model: &model,
                state: *state,
            })
        );
        assert_eq!(
            format!("{state:?}"),
            format!("{:?}", PairState::from(*state))
        );
        appended_record(&mut records, record);
    }
    let search = state_record(
        Feature::NativeWebSearch,
        &endpoint,
        &model,
        FeatureState::FallenBack,
    );
    assert!(matches!(
        feature_state(&search),
        Some(FeatureFact::Search { .. })
    ));
    for event in [
        LogEvent::BindingControls {
            model: model.clone(),
            state: FeatureState::FallenBack,
            endpoint_name: None,
        },
        LogEvent::NativeCompaction {
            model: model.clone(),
            state: FeatureState::Proven,
            endpoint_name: Some("elsewhere".into()),
        },
        search,
    ] {
        appended_record(&mut records, event);
    }
    let expected: Vec<PairState> = pairs
        .iter()
        .map(|(feature, state)| match feature {
            Feature::BindingControls => FeatureState::FallenBack,
            _ => *state,
        })
        .map(PairState::from)
        .collect();

    let fresh = scripted(vec![answer("Five.", 1_000)]);
    let (agent, back) = resumed_from_lines(fresh, &records, 0);
    assert_eq!(carried_states(&agent, &endpoint, &model), expected);
    assert_eq!(
        agent.pair_state(
            "elsewhere",
            &model,
            crate::provider::Feature::NativeCompaction
        ),
        PairState::Unknown
    );
    assert!(agent.search.refused.contains(&endpoint));
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u5", "fifth").await));
    live.close().await;

    let written = test_guard::records(&back);
    let (later, _) = resumed_from_lines(scripted(Vec::new()), &written, 0);
    assert_eq!(carried_states(&later, &endpoint, &model), expected);
    assert!(later.search.refused.contains(&endpoint));
}

async fn overflowed_with(events: Vec<LogEvent>) -> Vec<LogRecord> {
    let mut records = overflowed_log().await;
    for event in events {
        appended_record(&mut records, event);
    }
    records
}

#[tokio::test]
async fn a_fresh_start_keeps_no_state_for_another_endpoint() {
    use crate::agent::PairState;
    use crate::provider::Feature;
    let records = overflowed_with(vec![
        LogEvent::PerMessageEffort {
            model: "claude-opus-5-5".into(),
            state: FeatureState::FallenBack,
            endpoint_name: Some("other".into()),
        },
        LogEvent::WindowLowered {
            endpoint: "other".into(),
            model: "claude-opus-5-5".into(),
            tokens: 150_000,
        },
        LogEvent::NativeWebSearch {
            endpoint_name: "other".into(),
            state: FeatureState::FallenBack,
        },
    ])
    .await;
    let learned = |agent: &Agent| {
        (
            agent
                .models
                .resolve("other", "claude-opus-5-5", None, 64_000)
                .window
                .map(|w| w.tokens),
            agent.search.refused.contains("other"),
        )
    };

    let (agent, back) = resumed_on(
        scripted(vec![answer("Five.", 1_000)]),
        &records,
        0,
        Endpoints::Two,
    );
    let fresh = agent.pair_state("other", "claude-opus-5-5", Feature::PerMessageEffort);
    assert_eq!(learned(&agent), (Some(150_000), true));
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u5", "fifth").await));
    live.close().await;

    let written = test_guard::records(&back);
    let (later, _) = resumed_on(scripted(Vec::new()), &written, 0, Endpoints::Two);
    let resumed = later.pair_state("other", "claude-opus-5-5", Feature::PerMessageEffort);
    assert_eq!(learned(&later), (Some(150_000), true));
    assert_eq!(fresh, PairState::Unknown);
    assert_eq!(fresh, resumed);
}

#[tokio::test]
async fn a_carried_binding_record_binds_a_fresh_start_as_a_later_resume_does() {
    let records = overflowed_with(vec![LogEvent::BindingControls {
        model: "claude-opus-5-5".into(),
        state: FeatureState::Proven,
        endpoint_name: None,
    }])
    .await;

    let (idle, back) = resumed_from_lines(scripted(Vec::new()), &records, 0);
    let fresh = idle.settings.binding_controls;
    let live = Live::start(idle);
    live.close().await;
    let (later, _) = resumed_from_lines(scripted(Vec::new()), &test_guard::records(&back), 0);
    assert!(fresh);
    assert_eq!(later.settings.binding_controls, fresh);

    let (agent, back) = resumed_from_lines(scripted(vec![answer("Five.", 1_000)]), &records, 0);
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u5", "fifth").await));
    live.close().await;
    let (later, _) = resumed_from_lines(scripted(Vec::new()), &test_guard::records(&back), 0);
    assert!(later.settings.binding_controls);
}

fn last_message_lines(body: &Value) -> Vec<String> {
    body["messages"].as_array().unwrap().last().unwrap()["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|b| b["text"].as_str())
        .filter(|t| t.starts_with("Sent: ") || t.starts_with("Context: "))
        .map(|t| t.split(' ').next().unwrap_or_default().to_owned())
        .collect()
}

async fn lines_after_resume(layers: crate::settings::host::StartLayers) -> Vec<String> {
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("One.", 1_000)]);
    let mut live = Live::start(live_agent(provider.clone(), &store, Endpoints::One));
    assert!(completed(&live.turn("u1", "first").await));
    live.close().await;
    assert_eq!(last_message_lines(&provider.body(0)), ["Sent:", "Context:"]);
    let records = records(&store);
    let resumed = scripted(vec![answer("Two.", 1_100)]);
    let seq = u64::try_from(records.len()).unwrap();
    let agent = Agent::new(resumed.clone(), default_session())
        .with_clock(Arc::new(StillClock))
        .with_start_layers(layers);
    let agent = connected(agent, resumed.clone(), Endpoints::One)
        .with_log(
            SessionLog::continuing(Box::new(copy_all(&store, &records)), "s1", seq),
            log_context(),
        )
        .resume(&records, 0)
        .unwrap();
    let mut again = Live::start(agent);
    assert!(completed(&again.turn("u2", "second").await));
    again.close().await;
    last_message_lines(&resumed.body(0))
}

#[tokio::test]
async fn a_resume_keeps_its_logged_time_and_note_when_the_saved_ones_changed_since() {
    use wisp_protocol::settings::Place;
    let saved = super::background::saved(vec![
        (Place::Global, "time", SettingValue::Text("off".into())),
        (
            Place::Global,
            "context.context_note",
            SettingValue::Bool(false),
        ),
    ]);
    assert_eq!(lines_after_resume(saved).await, ["Sent:", "Context:"]);
}

#[tokio::test]
async fn a_time_flag_on_resume_still_wins_over_the_logged_value() {
    let flags = crate::settings::host::StartLayers {
        flags: vec![given("time", SettingValue::Text("off".into()))],
        ..Default::default()
    };
    assert_eq!(lines_after_resume(flags).await, ["Context:"]);
}

#[tokio::test]
async fn a_plain_retry_after_time_is_switched_off_still_stamps() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 1_000),
        gave_up(),
        answer("Two.", 1_100),
    ]);
    let mut live = Live::start(live_agent(provider.clone(), &store, Endpoints::One));
    assert!(completed(&live.turn("u1", "first").await));
    let failed = live.turn("u2", "second").await;
    assert!(matches!(failed, TurnOutcome::Error { .. }), "{failed:?}");
    set_option(&mut live, "time", SettingValue::Text("off".into())).await;
    assert!(completed(&retried(&mut live).await));
    live.close().await;
    assert_eq!(last_message_lines(&provider.body(2)), ["Sent:", "Context:"]);
}
