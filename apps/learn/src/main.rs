use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use learn_web::{Ended, FOLDER, KEY, Shown, WebSetup, failed_turn};
use wisp_client::{AGENT_STOPPED, Channels, drain_within, interrupts};
use wisp_core::endpoints::ConfigFile;
use wisp_core::endpoints::PathShown;
use wisp_core::session::JsonlStore;
use wisp_core::settings::{Given, START_ORIGIN};
use wisp_protocol::AgentEvent;
use wisp_protocol::settings::{Kind, Place, SettingTarget, SettingValue, Target, names};
use wisp_render::text::sanitize;
use wisp_start::endpoints::{READABLE, short};
use wisp_start::paths::{home_dir, xdg_config_home, xdg_data_home};
use wisp_start::run::{Setup, load_keychain};
use wisp_start::start::{self, FORCED_EXIT, Inputs, Picked, SHUTDOWN_WAIT, Stop};
use wisp_start::tools::ToolRule;

const APP: &str = "learn";
const FLOW: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../flows/interactive_education"
);
const SETTINGS: &str = "settings.toml";
const ACCOUNTS: &str = "learn: the flow's settings can't hold accounts";
const OPEN_ELSEWHERE: &str = "learn: the last lesson is open in another learn; close that one, or start fresh with learn --new";

#[derive(Debug, Parser)]
#[command(
    name = "learn",
    version,
    about = "The learning app: a voice lesson in the browser, continuing the last lesson"
)]
struct Args {
    #[arg(long, help = "Start a fresh lesson instead of continuing the last one")]
    new: bool,
    #[arg(
        long,
        value_name = "NAME",
        help = "Start the brain on this account for this run"
    )]
    account: Option<String>,
    #[arg(
        long,
        value_name = "N",
        default_value_t = 8443,
        help = "The port to serve on"
    )]
    port: u16,
}

fn main() -> ExitCode {
    wisp_start::paths::use_process_env();
    let args = Args::parse();
    let runtime = wisp_core::runtime::build(&mut tokio::runtime::Builder::new_multi_thread());
    let runtime = match runtime {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("learn: {e}");
            return ExitCode::FAILURE;
        }
    };
    let code = runtime.block_on(async {
        lesson(args).await.unwrap_or_else(|e| {
            eprintln!("{}", sanitize(&e));
            ExitCode::FAILURE
        })
    });
    runtime.shutdown_timeout(SHUTDOWN_WAIT);
    code
}

async fn lesson(args: Args) -> Result<ExitCode, String> {
    let flow = std::fs::canonicalize(FLOW).map_err(|_| {
        format!("learn: the flow's folder {FLOW} is missing; reinstall learn from the Wisp folder")
    })?;
    let (flags, presets) = flow_settings(&flow)?;
    let setup = Setup {
        flags,
        endpoint: args.account.clone(),
    };
    let keychain = Arc::new(load_keychain(None, &setup).map_err(|e| format!("learn: {e}"))?);
    let own = wisp_core::instructions::config_home(xdg_config_home(), home_dir().as_deref())
        .map(|config| config.join(APP));
    let picks = own.as_ref().map(|own| own.join(SETTINGS));
    let sessions = JsonlStore::app_dir(APP, xdg_data_home(), home_dir());
    let last = match (&sessions, args.new) {
        (Some(dir), false) => newest(dir)?,
        _ => None,
    };
    if last.as_ref().is_some_and(|last| last.open_elsewhere) {
        return Err(OPEN_ELSEWHERE.to_owned());
    }
    let picked = match (&last, &picks) {
        (None, Some(file)) => read_picked(file),
        _ => Picked::default(),
    };
    let inputs = Inputs {
        config: None,
        env_file: None,
        keychain: &keychain,
        setup: &setup,
        resume: last.as_ref().map(|last| wisp_start::resume::Request {
            id: Some(&last.id),
            unlocked: false,
        }),
        session_id: None,
        at: None,
        asks: false,
        tool_rule: ToolRule::Exactly,
        full_screen: false,
        restarted: false,
        forced: Vec::new(),
        voice: true,
        sessions,
        presets,
        picked,
    };
    let mut opened = match start::open(inputs).await {
        Ok(opened) => opened,
        Err(Stop::Failed(text)) => return Err(text),
        Err(Stop::Ask(ask)) => return Err(ask.question.script()),
    };
    let start = opened.book.start.as_str().to_owned();
    if let Some(account) = opened.unpicked.take().filter(|account| *account != start) {
        eprintln!(
            "{}",
            sanitize(&format!(
                "learn: {account}, picked in Settings, is no longer an account here; starting on {start}"
            ))
        );
    }
    let folder = own.as_ref().map(|own| own.join(FOLDER));
    opened
        .client_warnings
        .extend(folder.as_deref().and_then(key_warning));
    let built = start::build(opened, None, None).await?;
    let started = start::start(built);
    let Some(lane) = started.lane else {
        return Err("learn: this session has no voice layer".to_owned());
    };
    for warning in started
        .opening
        .start
        .as_ref()
        .into_iter()
        .flat_map(|start| &start.warnings)
    {
        eprintln!("{}", sanitize(warning));
    }
    let setup = WebSetup {
        port: args.port,
        folder,
        address: wisp_core::voice::default_ip().ok(),
    };
    let replayed = started.view.map(|r| r.events).unwrap_or_default();
    let served = serve(
        setup,
        &replayed,
        started.channels,
        (lane.up, lane.down),
        (&started.opening.model, picks.as_deref()),
    )
    .await;
    let (code, forced) = match served {
        Ok(false) => (ExitCode::SUCCESS, false),
        Ok(true) => (ExitCode::from(FORCED_EXIT), true),
        Err(e) => {
            eprintln!("learn: {}", sanitize(&e));
            (ExitCode::FAILURE, false)
        }
    };
    let by = tokio::time::Instant::now() + SHUTDOWN_WAIT;
    Ok(start::end(APP, started.task, forced, by, code).await)
}

fn flow_settings(flow: &Path) -> Result<(Vec<Given>, wisp_core::agents::Presets), String> {
    let path = flow.join(SETTINGS);
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("learn: {}: {e}", path.display()))?;
    let file =
        ConfigFile::parse(&path, PathShown::Default, &text).map_err(|e| format!("learn: {e}"))?;
    let accounts = file
        .settings
        .iter()
        .any(|(at, _)| at.spec().kind == Kind::EndpointName);
    if accounts || file.default.is_some() || !file.endpoints.is_empty() {
        return Err(ACCOUNTS.to_owned());
    }
    let flags = file
        .settings
        .iter()
        .filter(|(at, _)| matches!(at.place, Place::Global))
        .map(|(at, saved)| {
            let spec = at.spec();
            let value = match (&saved.value, spec.name) {
                (SettingValue::Text(file), names::VOICE_INSTRUCTIONS) => {
                    SettingValue::Text(flow.join(file).display().to_string())
                }
                (value, _) => value.clone(),
            };
            Given {
                target: Target::Name(spec),
                value,
                origin: START_ORIGIN.to_owned(),
            }
        })
        .collect();
    Ok((flags, file.presets.rooted(flow)))
}

fn newest(dir: &Path) -> Result<Option<wisp_core::resume::Listed>, String> {
    let shown = wisp_core::workspace::home_shown(dir);
    let store = JsonlStore::open(dir).map_err(|e| format!("learn: {shown}: {e}"))?;
    wisp_core::resume::newest(&store).map_err(|e| format!("learn: {shown}: {e}"))
}

fn read_picked(file: &Path) -> Picked {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Picked::default(),
        Err(e) => return unread(file, &e.to_string()),
    };
    toml::from_str(&text).unwrap_or_else(|e| unread(file, e.message()))
}

fn unread(file: &Path, reason: &str) -> Picked {
    let shown = wisp_core::workspace::home_shown(file);
    let line = format!("learn: {shown} doesn't read ({reason}); starting without it");
    eprintln!("{}", sanitize(&line));
    Picked::default()
}

fn remember(event: &AgentEvent, file: &Path) -> Result<(), String> {
    let AgentEvent::SettingsChanged {
        answers,
        endpoint,
        model,
        effort,
        changes,
        withdrawn,
        ..
    } = event
    else {
        return Ok(());
    };
    let picked = |wanted: &[&str]| {
        changes.iter().any(|c| match &c.target {
            SettingTarget::Name(name) => wanted.contains(&name.as_str()),
            SettingTarget::Path(_) => false,
        })
    };
    let mut kept = std::fs::read_to_string(file)
        .ok()
        .and_then(|text| toml::from_str::<Picked>(&text).ok())
        .unwrap_or_default();
    let (pair, by_effort) = match (answers, withdrawn) {
        (_, Some(_)) => (kept.account.is_some(), kept.effort.is_some()),
        (Some(_), None) => (
            picked(&[names::ENDPOINT, names::MODEL]),
            picked(&[names::EFFORT]),
        ),
        (None, None) => (false, false),
    };
    if !pair && !by_effort {
        return Ok(());
    }
    if pair {
        kept.account = Some(endpoint.clone());
        kept.model = Some(model.clone());
    }
    if by_effort {
        kept.effort = Some(*effort);
    }
    let text = toml::to_string(&kept).map_err(|e| e.to_string())?;
    if let Some(folder) = file.parent() {
        std::fs::create_dir_all(folder).map_err(|e| e.to_string())?;
    }
    wisp_core::secrets::write_atomic(file, text.as_bytes(), 0o600).map_err(|e| e.to_string())
}

fn key_warning(folder: &Path) -> Option<String> {
    let key = folder.join(KEY);
    let home = home_dir().map(|h| h.display().to_string());
    wisp_core::secrets::readable_by_others(&key)
        .then(|| format!("learn: {} {READABLE}", short(&key, home.as_deref())))
}

async fn serve(
    setup: WebSetup,
    replayed: &[AgentEvent],
    channels: Channels,
    lane: (
        tokio::sync::mpsc::Sender<wisp_protocol::voice::AudioFrame>,
        tokio::sync::mpsc::Receiver<wisp_protocol::voice::AudioFrame>,
    ),
    (model, picks): (&str, Option<&Path>),
) -> Result<bool, String> {
    let Channels {
        commands,
        mut events,
        quit,
    } = channels;
    let mut interrupt = interrupts()?;
    let stop = async move {
        interrupt.recv().await;
    };
    let ended = learn_web::serve(
        setup,
        replayed,
        commands,
        &mut events,
        lane,
        stop,
        |shown| match shown {
            Shown::Address(line) => println!("{line}"),
            Shown::Event(event) => {
                if let Some(line) = failed_turn(event, model) {
                    eprintln!("{line}");
                }
                if let Some(Err(e)) = picks.map(|file| remember(event, file)) {
                    let shown = picks
                        .map(wisp_core::workspace::home_shown)
                        .unwrap_or_default();
                    eprintln!(
                        "{}",
                        sanitize(&format!("learn: could not save the pick to {shown}: {e}"))
                    );
                }
            }
        },
    )
    .await?;
    if ended == Ended::AgentStopped {
        return Err(AGENT_STOPPED.to_owned());
    }
    drain_within(&mut events, quit, |_| Ok(())).await
}

#[cfg(test)]
mod tests;
