mod auth;
mod browser;
mod card;
mod daemon;
mod endpoints;
mod first_run;
mod host;
mod keep;
mod manage;
mod output;
mod plugins;
mod restart;
mod resume;
mod settings;
mod setup_server;
mod tui;
mod usage;

use std::ffi::{OsStr, OsString};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use wisp_client::Channels;
use wisp_core::agent;
use wisp_core::endpoints::{EndpointBook, EndpointName, ProviderKind};
use wisp_core::instructions::Paths;
use wisp_core::plugin::{InstructionsDoctor, Registry};
use wisp_core::secrets::detect::KeychainSetting;
use wisp_core::secrets::keyring::{Budget, TERMINAL_DEADLINE};
#[cfg(test)]
use wisp_core::session::new_session_id;
use wisp_core::session::{JsonlStore, session_uuid};
use wisp_core::settings::Given;
use wisp_protocol::endpoint::StoreChoice;
use wisp_protocol::settings::{SettingValue, Target, names};
use wisp_protocol::{ACCOUNT_FLAG, CacheTtl, Effort, MessageTime};
use wisp_render::chrome::account::SaidWords;
use wisp_start::instructions::Settings;
use wisp_start::paths::{home_dir, models_folder, usage_cache, xdg_config_home, xdg_data_home};
use wisp_start::run::{
    Run, Setup, configured, display_env, keychain_variable, load_keychain, marked_in, preset_place,
    shown_choice, shown_flags, start_env,
};
use wisp_start::start::{self, StartAt, search_endpoints};
use wisp_start::{agents, context, instructions, keychain, presence, tools};
use wisp_tui::app::SessionFacts;
use wisp_tui::app::endpoint_card::CardSetup;
use wisp_tui::app::setup::{self, Opening, SetupChannels};
use wisp_tui::term::SavedDisplay;

use crate::output::{OutputMode, Presentation};
use crate::resume::{Asked, Shown};

#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[global_allocator]
static ALLOCATOR: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

const CHAT_ID: &str = "A chat's id, or the start of one";

#[cfg_attr(test, derive(Debug))]
#[derive(Parser, Default)]
#[command(
    name = "wisp",
    version,
    about = "Chat with an AI assistant from the terminal"
)]
struct Args {
    #[arg(long, hide = true, num_args = 0..=1, require_equals = true)]
    host: Option<Option<PathBuf>>,
    #[arg(long, hide = true)]
    restarted: bool,
    #[arg(skip)]
    host_args: Vec<std::ffi::OsString>,
    #[arg(
        long,
        global = true,
        value_name = "MODEL",
        value_parser = usage::setting(names::MODEL),
        help = concat!("Model ID (the default is claude-opus-5-5; model in ", wisp_protocol::config_file!(), " sets it too)")
    )]
    model: Option<SettingValue>,
    #[arg(
        long,
        global = true,
        value_parser = usage::effort(),
        help = "Thinking effort: low, medium (the default), high, xhigh or max"
    )]
    effort: Option<Effort>,
    #[arg(
        long,
        global = true,
        value_parser = usage::cache_ttl(),
        help = "Prompt-cache lifetime: 1h for interactive use (the default), 5m for continuous loops"
    )]
    cache_ttl: Option<CacheTtl>,
    #[arg(
        long,
        global = true,
        value_name = "MODE",
        value_parser = usage::message_time(),
        help = "Stamp each message with the time it was sent: local (offset only, the default), utc or off"
    )]
    time: Option<MessageTime>,
    #[arg(
        long,
        global = true,
        value_name = "ON|OFF",
        value_parser = usage::setting(names::CONTEXT_CONTEXT_NOTE),
        help = concat!("Tell the model how full its context is, with a Context: line on each message: on (the default) or off. context_note under [context] in ", wisp_protocol::config_file!(), " sets it too")
    )]
    context_note: Option<SettingValue>,
    #[arg(
        long,
        global = true,
        help = concat!("Don't compact automatically before a request that would reach the compaction point; /compact still works. auto_compact under [context] in ", wisp_protocol::config_file!(), " sets it too")
    )]
    no_auto_compact: bool,
    #[arg(
        long,
        global = true,
        value_name = "TOKENS|PERCENT",
        value_parser = usage::setting(names::COMPACT_AT),
        help = concat!("Compact at this many tokens (400k) or this share of the window (85%) for this session, above the model tables in ", wisp_protocol::config_file!(), "; never past window \u{2212} reserve")
    )]
    compact_at: Option<SettingValue>,
    #[arg(
        long,
        global = true,
        help = "Ask the API to diagnose prompt-cache misses"
    )]
    debug_cache: bool,
    #[arg(
        long,
        global = true,
        help = "Before a turn of the main agent that ran tools ends, ask the model once to check its work against the task"
    )]
    self_check: bool,
    #[arg(
        long,
        global = true,
        value_name = "USD",
        value_parser = usage::setting(names::RETRY_BUDGET_USD),
        help = "Cap what retries of failed requests may cost per turn, in US dollars. Without it, recoverable failures keep retrying until the request succeeds or you stop them"
    )]
    retry_budget: Option<SettingValue>,
    #[arg(
        long,
        global = true,
        help = "Don't re-run refused requests on Anthropic's fallback model"
    )]
    no_fallback: bool,
    #[arg(
        long = "set",
        global = true,
        value_name = "NAME=VALUE",
        help = "Set any setting for this run, such as --set effort=low; repeat it for more. wisp config list shows them"
    )]
    set: Vec<String>,
    #[arg(
        long,
        global = true,
        value_name = "PATH",
        help = concat!("Read settings from this file instead of `", wisp_protocol::config_dir!(), wisp_protocol::keys_file!(), "`. A `", wisp_protocol::keys_file!(), "` in the current directory is never read on its own")
    )]
    env_file: Option<PathBuf>,
    #[arg(
        long,
        global = true,
        value_name = "PATH",
        help = concat!("Read named accounts from this file instead of `", wisp_protocol::config_dir!(), wisp_protocol::config_file!(), "`; its keys go in ", wisp_protocol::credentials_file!(), " beside it. A config in the current directory is never read on its own")
    )]
    config: Option<PathBuf>,
    #[arg(
        long,
        global = true,
        value_name = "FILE",
        help = "Add the MCP servers in this file for this run, written as config.toml's [mcp.servers.<name>] tables. config.toml isn't changed"
    )]
    mcp_config: Option<PathBuf>,
    #[arg(
        long,
        global = true,
        value_name = "MODE",
        value_parser = usage::choices(&KeychainSetting::ALL, KeychainSetting::as_str),
        help = concat!("Use the system keychain: auto (the default: when this machine has one, not under WSL), on or off (keys only in variables and ", wisp_protocol::credentials_file!(), "). WISP_KEYCHAIN and keychain in ", wisp_protocol::config_file!(), " set it too")
    )]
    keychain: Option<KeychainSetting>,
    #[arg(
        long = "account",
        alias = "endpoint",
        global = true,
        value_name = "NAME",
        help = format!("Start on this account. {}=NAME does the same", endpoints::CHOICE_VAR)
    )]
    endpoint: Option<String>,
    #[arg(
        long,
        global = true,
        help = "Don't write a session log (by default one goes to `~/.local/share/wisp/sessions/`)"
    )]
    no_log: bool,
    #[arg(
        long,
        value_name = "ID",
        num_args = 0..=1,
        conflicts_with = "no_log",
        help = "Resume the latest session in this directory, or the one whose ID starts with ID"
    )]
    resume: Option<Option<String>>,
    #[arg(long, requires = "resume", help = resume::UNLOCKED_HELP)]
    resume_unlocked: bool,
    #[arg(
        long,
        value_name = "ID",
        conflicts_with_all = ["resume", "resume_unlocked", "no_log"],
        help = "Continue the session with this ID, or start one when there is none. ID is a random UUID you choose, such as uuidgen prints; give it again to continue the same session"
    )]
    session_id: Option<OsString>,
    #[arg(
        long,
        value_enum,
        value_name = "MODE",
        default_value_t = OutputMode::Auto,
        help = "How replies are shown: auto (the default) needs a terminal and shows the tui; tui forces it on a dumb terminal; jsonl is for scripts and pipes (unstable until M6)"
    )]
    output: OutputMode,
    #[arg(long, hide = true, value_name = "FILE")]
    replay: Option<PathBuf>,
    #[arg(
        long,
        global = true,
        help = "Show the full token and cache line after each turn, all of the thinking, and a line for every hook run"
    )]
    verbose: bool,
    #[arg(
        long,
        global = true,
        value_enum,
        value_name = "SOURCE",
        help = concat!("Which instruction file goes in the system prompt: auto (the project's AGENTS.md, else your default), default (only ~/.config/wisp/AGENTS.md) or none. [instructions] source in ", wisp_protocol::config_file!(), " sets it too")
    )]
    instructions: Option<instructions::InstructionsArg>,
    #[arg(
        long,
        global = true,
        value_enum,
        value_name = "NAME",
        help = "Built-in instructions that go first in the system prompt's instructions, before any instruction file: none or autonomous (for a task done on its own, with nobody to answer questions). Applies to new sessions. [instructions] extra in config.toml sets it too"
    )]
    instructions_extra: Option<instructions::ExtraArg>,
    #[arg(
        long,
        global = true,
        value_name = "TOKENS",
        value_parser = instructions::parse_warn_tokens,
        help = "Warn when the instruction files come to more than this many tokens, such as 8000 or 8k (the default is 8k). Nothing is ever cut"
    )]
    instructions_warn_tokens: Option<u64>,
    #[arg(
        long,
        global = true,
        value_delimiter = ',',
        value_name = "TOOLS",
        help = "Which tools the model may use, comma-separated"
    )]
    tools: Option<Vec<String>>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[cfg_attr(test, derive(Debug))]
#[derive(Subcommand)]
enum Command {
    #[command(about = "Sign in to Claude, ChatGPT, Antigravity or an API key")]
    Login {
        #[arg(value_name = "WHAT")]
        what: Option<String>,
        #[arg(value_name = "NAME", hide = true)]
        more: Option<String>,
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        #[arg(long, hide = true, value_parser = manage::providers())]
        provider: Option<ProviderKind>,
        #[arg(long, hide = true, value_parser = manage::stores())]
        store: Option<StoreChoice>,
        #[arg(long, hide = true)]
        default: bool,
    },
    #[command(about = "Show your accounts and what needs you")]
    Status { name: Option<String> },
    #[command(about = "Sign out of an account")]
    Logout {
        name: Option<String>,
        #[arg(long, help = "Sign out without asking, for scripts")]
        yes: bool,
    },
    #[command(about = "Print what Wisp detected about this terminal, for bug reports")]
    Doctor {
        #[arg(
            long,
            help = "Also make one count_tokens request per available account (free on Anthropic's side; a gateway may count it)"
        )]
        check: bool,
        #[arg(
            value_name = "NAME",
            requires = "check",
            help = "With --check, check only these accounts"
        )]
        names: Vec<String>,
    },
    #[command(
        name = "account",
        about = "Add, list and remove accounts, and choose where new chats start",
        alias = "endpoint",
        arg_required_else_help = true,
        subcommand_required = true
    )]
    Endpoint {
        #[command(subcommand)]
        command: manage::EndpointCommand,
    },
    #[command(
        about = "View and change settings: list, get, set, unset and path",
        arg_required_else_help = true,
        subcommand_required = true
    )]
    Config {
        #[command(subcommand)]
        command: settings::ConfigCommand,
    },
    #[command(
        about = "List plugins, and turn them on or off, remove them and add them back",
        arg_required_else_help = true,
        subcommand_required = true
    )]
    Plugin {
        #[command(subcommand)]
        command: plugins::PluginCommand,
    },
    #[command(
        about = "Set up an account: asks where it connects, how it takes the key and the key (hidden), checks it and saves it"
    )]
    Setup,
    #[command(about = "Quit chats by id, without opening them")]
    Quit {
        #[arg(value_name = "ID", required = true, help = CHAT_ID)]
        ids: Vec<String>,
    },
    #[command(about = format!("{}, all or by id", wisp_render::transcript::copy::RESTART_KEPT))]
    Restart {
        #[arg(value_name = "ID", help = CHAT_ID)]
        ids: Vec<String>,
    },
    #[command(about = "Show the chats kept open, and start or stop Wisp's daemon")]
    Daemon {
        #[command(subcommand)]
        command: Option<daemon::Command>,
    },
}

fn shown_by_client(path: &wisp_protocol::settings::SitePath) -> bool {
    path.spec().owner == wisp_protocol::settings::Owner::Client
}

fn typed_flags(args: &Args) -> Vec<settings::Flag> {
    use settings::Flag;
    let text = |s: &str| SettingValue::Text(s.to_owned());
    let mut out = Vec::new();
    if let Some(model) = &args.model {
        out.push(Flag::new("--model", names::MODEL, model.clone()));
    }
    if let Some(effort) = args.effort {
        out.push(Flag::new("--effort", names::EFFORT, text(effort.as_str())));
    }
    if let Some(ttl) = args.cache_ttl {
        out.push(Flag::new(
            "--cache-ttl",
            names::CACHE_TTL,
            text(ttl.as_str()),
        ));
    }
    if let Some(time) = args.time {
        out.push(Flag::new("--time", names::TIME, text(time.as_str())));
    }
    if let Some(note) = &args.context_note {
        out.push(Flag::new(
            "--context-note",
            names::CONTEXT_CONTEXT_NOTE,
            note.clone(),
        ));
    }
    if args.no_auto_compact {
        out.push(Flag::new(
            "--no-auto-compact",
            names::CONTEXT_AUTO_COMPACT,
            SettingValue::Bool(false),
        ));
    }
    if let Some(point) = &args.compact_at {
        out.push(Flag::new("--compact-at", names::COMPACT_AT, point.clone()));
    }
    if args.debug_cache {
        out.push(Flag::new(
            "--debug-cache",
            names::DEBUG_CACHE,
            SettingValue::Bool(true),
        ));
    }
    if args.self_check {
        out.push(Flag::new(
            "--self-check",
            names::SELF_CHECK_BEFORE_END,
            SettingValue::Bool(true),
        ));
    }
    if let Some(budget) = &args.retry_budget {
        out.push(Flag::new(
            "--retry-budget",
            names::RETRY_BUDGET_USD,
            budget.clone(),
        ));
    }
    if args.no_fallback {
        out.push(Flag::new(
            "--no-fallback",
            names::REFUSAL_FALLBACK,
            SettingValue::Bool(false),
        ));
    }
    if let Some(endpoint) = &args.endpoint {
        out.push(Flag::new(ACCOUNT_FLAG, names::ENDPOINT, text(endpoint)));
    }
    if let Some(keychain) = args.keychain {
        out.push(Flag::new(
            "--keychain",
            names::KEYCHAIN,
            text(keychain.as_str()),
        ));
    }
    if args.no_log {
        out.push(Flag::new("--no-log", names::LOG, SettingValue::Bool(false)));
    }
    if args.verbose {
        out.push(Flag::new(
            "--verbose",
            names::DISPLAY_VERBOSE,
            SettingValue::Bool(true),
        ));
    }
    if let Some(source) = args.instructions {
        out.push(Flag::new(
            "--instructions",
            names::INSTRUCTIONS_SOURCE,
            text(source.name()),
        ));
    }
    if let Some(extra) = args.instructions_extra {
        out.push(Flag::new(
            "--instructions-extra",
            names::INSTRUCTIONS_EXTRA,
            text(extra.name()),
        ));
    }
    if let Some(tokens) = args.instructions_warn_tokens {
        out.push(Flag::new(
            "--instructions-warn-tokens",
            names::INSTRUCTIONS_WARN_TOKENS,
            SettingValue::Tokens(tokens),
        ));
    }
    if let Some(tools) = &args.tools {
        out.push(Flag::new(
            "--tools",
            names::TOOLS,
            SettingValue::List(tools::flag_names(tools).unwrap_or_default()),
        ));
    }
    out
}

fn setup_of(args: &Args) -> Result<Setup, ExitCode> {
    if let Some(Err(e)) = args.tools.as_deref().map(tools::flag_names) {
        eprintln!("wisp: {e}");
        return Err(ExitCode::from(usage::USAGE_EXIT));
    }
    let flags = settings::flag_layer(typed_flags(args), &args.set).map_err(|e| {
        eprintln!("wisp: {e}");
        ExitCode::from(usage::USAGE_EXIT)
    })?;
    let saved = endpoints::config_path(
        wisp_protocol::CONFIG_FILE,
        args.config.as_deref(),
        xdg_config_home(),
        home_dir(),
    )
    .and_then(|path| std::fs::read_to_string(path).ok())
    .and_then(|text| wisp_core::models::ConfigModels::parse(&text).ok())
    .map(|models| models.models)
    .unwrap_or_default();
    settings::alias_clash(&flags, &saved).map_err(|e| {
        eprintln!("wisp: {e}");
        ExitCode::from(usage::USAGE_EXIT)
    })?;
    let endpoint = settings::flag_value(&flags, names::ENDPOINT).map(SettingValue::text);
    Ok(Setup { flags, endpoint })
}

fn with_account_help(command: clap::Command, width: usize) -> clap::Command {
    command
        .mut_subcommand("login", |login| {
            login.override_help(auth::login::help(width, &wisp_tui::term::prompt::glyphs()))
        })
        .mut_subcommand("status", |status| {
            status.override_help(auth::login::status_help(
                width,
                &wisp_tui::term::prompt::glyphs(),
            ))
        })
}

fn parse_args() -> Result<Args, ExitCode> {
    let command = with_account_help(
        tools::command::<Args>(),
        wisp_tui::term::prompt::terminal_columns(),
    );
    usage::parse_with::<Args, _>(command, std::env::args_os())
        .map(|mut args| {
            args.host_args = std::env::args_os().skip(1).collect();
            args
        })
        .map_err(|failure| {
            if failure.stdout {
                print!("{}", failure.text);
            } else {
                eprintln!("{}", failure.text.trim_end());
            }
            u8::try_from(failure.code).map_or(ExitCode::FAILURE, ExitCode::from)
        })
}

fn pin(args: &mut Args, sessions: Option<&Path>) -> Result<(), &'static str> {
    let Some(value) = &args.session_id else {
        return Ok(());
    };
    let id = value.to_str().and_then(session_uuid).ok_or(
        "wisp: --session-id takes a UUID, such as uuidgen prints (what was given isn't shown)",
    )?;
    if sessions.is_some_and(|dir| JsonlStore::has_log(dir, &id)) {
        args.resume = Some(Some(id.clone()));
    }
    args.session_id = Some(id.into());
    Ok(())
}

fn main() -> ExitCode {
    host::build();
    wisp_start::paths::use_process_env();
    tools::leave_out(plugins::LEARN_ONLY);
    let mut args = match parse_args() {
        Ok(args) => args,
        Err(code) => return code,
    };
    if let Err(error) = pin(
        &mut args,
        JsonlStore::default_dir(xdg_data_home(), home_dir()).as_deref(),
    ) {
        eprintln!("{error}");
        return ExitCode::from(2);
    }
    if let Some(Command::Daemon { command }) = &args.command {
        return on_runtime(false, || daemon::run(command.as_ref()));
    }
    if let Some(Command::Restart { ids }) = &args.command {
        return on_runtime(false, || restart::run(ids));
    }
    if let Some(Command::Quit { ids }) = &args.command {
        return on_runtime(false, || keep::quit(ids));
    }
    let setup = match setup_of(&args) {
        Ok(setup) => setup,
        Err(code) => return code,
    };
    if args.host.is_some() {
        return on_runtime(false, || host::run(args, setup));
    }
    if args.command.is_none()
        && let Some(path) = &args.replay
    {
        return replay(&args, &setup, path);
    }
    let keychain = match load_keychain(args.config.as_deref(), &setup) {
        Ok(keychain) => keychain,
        Err(e) => {
            eprintln!("wisp: {e}");
            return ExitCode::FAILURE;
        }
    };
    let shared = Arc::new(keychain);
    let keychain = &*shared;
    match &args.command {
        Some(Command::Login {
            what,
            more,
            name,
            provider,
            store,
            default,
        }) => {
            let model = model_of(&args, &setup);
            return auth::login::login(
                &manage_context(&args, &setup, &model, keychain),
                Arc::clone(&shared),
                &auth::login::Args {
                    what: what.as_deref(),
                    more: more.as_deref(),
                    name: name.as_deref(),
                    provider: *provider,
                    store: *store,
                    make_default: *default,
                },
            );
        }
        Some(Command::Status { name }) => {
            let model = model_of(&args, &setup);
            let ctx = manage_context(&args, &setup, &model, keychain);
            return auth::accounts::status(&ctx, name.as_deref());
        }
        Some(Command::Logout { name, yes }) => {
            let model = model_of(&args, &setup);
            let ctx = manage_context(&args, &setup, &model, keychain);
            return auth::logout(&ctx, Arc::clone(&shared), name.as_deref(), *yes);
        }
        Some(Command::Doctor { check, names }) => {
            let names = check.then(|| names.clone());
            return doctor(&args, &setup, keychain, names);
        }
        Some(Command::Endpoint { command }) => {
            let model = model_of(&args, &setup);
            let ctx = manage_context(&args, &setup, &model, keychain);
            return manage::run(command, &ctx);
        }
        Some(chosen @ (Command::Config { .. } | Command::Plugin { .. })) => {
            let model = model_of(&args, &setup);
            let ctx = manage_context(&args, &setup, &model, keychain);
            let found = config_facts(&args, &setup, keychain);
            let cx = settings::Command {
                manage: &ctx,
                flags: &found.flags,
                env: found.env,
                endpoint: found.endpoint,
                start: found.start,
                secrets: found.secrets,
                keys_file: found.keys_file,
                model: model.clone(),
                endpoints: found.endpoints,
                kinds: found.kinds,
                wires: found.wires,
            };
            return match chosen {
                Command::Plugin { command } => plugins::run(command, &cx),
                Command::Config { command } => settings::run(command, &cx),
                Command::Doctor { .. }
                | Command::Endpoint { .. }
                | Command::Setup
                | Command::Quit { .. }
                | Command::Restart { .. }
                | Command::Daemon { .. }
                | Command::Login { .. }
                | Command::Status { .. }
                | Command::Logout { .. } => ExitCode::FAILURE,
            };
        }
        Some(Command::Setup) => {
            let model = model_of(&args, &setup);
            return first_run::command(&manage_context(&args, &setup, &model, keychain));
        }
        Some(Command::Daemon { .. } | Command::Quit { .. } | Command::Restart { .. }) => {
            return ExitCode::FAILURE;
        }
        None => {}
    }
    if keychain.detected().available() && args.env_file.is_some() {
        eprintln!("wisp: {}", endpoints::ENV_FILE_REFUSED);
        return ExitCode::FAILURE;
    }
    let model = model_of(&args, &setup);
    let ctx = manage_context(&args, &setup, &model, keychain);
    let opening = first_run::start(&ctx, setup.endpoint.as_deref(), args.output);
    match opening {
        first_run::Start::Session => run(args, setup, shared),
        first_run::Start::Done(code) => code,
        first_run::Start::Card(card) => setup_run(args, setup, shared, card),
    }
}

fn replay(args: &Args, setup: &Setup, path: &Path) -> ExitCode {
    let start = ReplayPaths {
        xdg_config_home: xdg_config_home(),
        home: home_dir(),
        home_shown: std::env::var("HOME").ok(),
    };
    let (settings, facts) = replay_start(args, setup, start);
    on_runtime(true, || tui::replay(path, settings, facts))
}

struct ReplayPaths {
    xdg_config_home: Option<PathBuf>,
    home: Option<PathBuf>,
    home_shown: Option<String>,
}

fn replay_start(
    args: &Args,
    setup: &Setup,
    paths: ReplayPaths,
) -> (wisp_tui::app::replay::Settings, wisp_tui::app::SessionFacts) {
    let shown = if args.config.is_some() {
        wisp_core::endpoints::PathShown::Given
    } else {
        wisp_core::endpoints::PathShown::Default
    };
    let (read, config) = match endpoints::config_path(
        wisp_protocol::CONFIG_FILE,
        args.config.as_deref(),
        paths.xdg_config_home,
        paths.home,
    ) {
        Some(file) => settings::host::read_with_config(&file, shown, paths.home_shown.as_deref()),
        None => (
            wisp_core::settings::host::SavedRead {
                often: Vec::new(),
                saved: Vec::new(),
                file: wisp_protocol::settings::FileState::Missing,
            },
            None,
        ),
    };
    let endpoint = tui::replaying::endpoint(setup.endpoint.as_deref(), config.as_ref());
    let resolved = Run::resolve_on(setup, config.as_ref(), Some(&endpoint.name), None);
    let facts = tui::replay_facts(
        &endpoint,
        &resolved.model,
        resolved.effort,
        resolved.cache_ttl,
    );
    let settings = tui::replaying::settings(tui::replaying::Start {
        model: &resolved.model,
        effort: resolved.effort,
        endpoint: &endpoint,
        flags: &shown_flags(&setup.flags, &configured(config.as_ref())),
        env: display_env(),
        config: config.as_ref(),
        read,
        max_tokens: facts.max_tokens,
        cache_ttl: resolved.cache_ttl,
    });
    (settings, facts)
}

struct ConfigFacts {
    env: Vec<Given>,
    flags: Vec<Given>,
    endpoint: Option<String>,
    start: Option<String>,
    secrets: settings::SecretStates,
    keys_file: Option<String>,
    endpoints: Vec<String>,
    kinds: std::collections::BTreeMap<String, wisp_protocol::settings::CredentialKind>,
    wires: std::collections::BTreeMap<String, ProviderKind>,
}

fn forced_layer(
    full_screen: bool,
    env: &wisp_tui::term::Env,
    streams: wisp_tui::term::Streams,
) -> Vec<Given> {
    if !full_screen {
        return Vec::new();
    }
    wisp_tui::app::config_screen::forced_display(env, streams, wisp_tui::term::Flags::default())
        .into_iter()
        .filter_map(|(name, resolved)| {
            let spec = wisp_protocol::settings::find(name)?;
            Some(Given {
                target: wisp_protocol::settings::Target::Name(spec),
                value: resolved.value,
                origin: resolved.source,
            })
        })
        .collect()
}

fn config_facts(args: &Args, setup: &Setup, keychain: &keychain::Keychain) -> ConfigFacts {
    let flags = endpoints::Flags {
        env_file: None,
        config: args.config.as_deref(),
        endpoint: setup.endpoint.as_deref(),
        keychain,
        set: &setup.flags,
    };
    let sources = endpoints::Sources::load(&flags, xdg_config_home(), home_dir());
    let book = sources.book().ok();
    let env = start_env(
        &sources,
        book.as_ref(),
        keychain,
        keychain_variable().as_deref(),
    );
    let config = sources.config.as_ref().ok().and_then(Option::as_ref);
    let home = std::env::var("HOME").ok();
    let start = book
        .as_ref()
        .map(|book| endpoints::start_words(book, config, home.as_deref()));
    let known = configured(config);
    let endpoint = match &book {
        Some(book) => Some(book.start.to_string()),
        None => setup
            .endpoint
            .as_deref()
            .or(sources.variable.as_ref().map(|c| c.name.as_str()))
            .map(|name| shown_choice(name, &known))
            .or_else(|| config.and_then(|c| c.default.as_ref().map(ToString::to_string))),
    };
    let flags = shown_flags(&setup.flags, &known);
    let variables = keychain::keychain(&sources.inputs).ok();
    let present = |name: &str| {
        variables
            .as_ref()
            .and_then(wisp_core::endpoints::Keychain::variables)
            .is_some_and(|v| {
                (v.env())(name).is_some() || v.env_file().is_some_and(|f| f.vars.contains_key(name))
            })
    };
    let records = sources.secrets.records().ok();
    let mut secrets = settings::SecretStates::new();
    for (name, endpoint) in config.map(|c| &c.endpoints).into_iter().flatten() {
        let stored = records.and_then(|r| r.get(name));
        let key = match &endpoint.key {
            wisp_core::endpoints::KeyFrom::Variable(v) => present(v.raw()),
            wisp_core::endpoints::KeyFrom::Store(_) => stored.is_some(),
        };
        let stored_url = stored.is_some_and(|s| match &s.record {
            wisp_core::secrets::StoredRecord::Key { url, .. } => url.is_some(),
        });
        let url = endpoint.url.is_some()
            || endpoint.url_env.as_ref().is_some_and(|v| present(v.raw()))
            || stored_url;
        secrets.insert(name.to_string(), (key, url));
    }
    let (keys_file, ready) = book
        .as_ref()
        .and_then(|b| {
            let name = wisp_core::endpoints::keys_file_account(&b.entries)?;
            let ready = matches!(
                b.entries.get(name),
                Some(wisp_core::endpoints::Availability::Ready(_))
            );
            Some((name.to_string(), ready))
        })
        .unzip();
    let key =
        ready.unwrap_or(false) || present("ANTHROPIC_API_KEY") || present("ANTHROPIC_AUTH_TOKEN");
    secrets
        .entry(
            keys_file
                .clone()
                .unwrap_or_else(|| wisp_protocol::CLAUDE_API.to_owned()),
        )
        .or_insert((key, present("ANTHROPIC_BASE_URL")));
    let summaries = sources
        .entries()
        .map(|entries| endpoints::summaries(&entries, &Default::default(), &[], None, &[]))
        .unwrap_or_default();
    let kinds = summaries
        .iter()
        .filter_map(|s| Some((s.name.clone(), s.credential.clone()?)))
        .collect();
    let wires = summaries
        .iter()
        .filter_map(|s| Some((s.name.clone(), ProviderKind::parse(&s.provider)?)))
        .collect();
    ConfigFacts {
        env,
        flags,
        endpoint,
        start,
        secrets,
        keys_file,
        endpoints: known.iter().map(|name| (*name).to_owned()).collect(),
        kinds,
        wires,
    }
}

fn manage_context<'a>(
    args: &'a Args,
    setup: &'a Setup,
    model: &'a str,
    keychain: &'a keychain::Keychain,
) -> manage::Context<'a> {
    let saved = saved_config(args);
    manage::Context {
        env_file: args.env_file.as_deref(),
        config: args.config.as_deref(),
        set: &setup.flags,
        model,
        caps: prompt_caps(&saved_display(
            &setup.flags,
            saved.as_ref().map(|c| &c.settings),
        )),
        claude_usage: Run::resolve_on(setup, saved.as_ref(), None, None).claude_usage,
        xdg_config_home: xdg_config_home(),
        home: home_dir(),
        keychain,
        models: models_folder(),
    }
}

fn saved_config(args: &Args) -> Option<wisp_core::endpoints::ConfigFile> {
    let path = endpoints::config_path(
        wisp_protocol::CONFIG_FILE,
        args.config.as_deref(),
        xdg_config_home(),
        home_dir(),
    )?;
    endpoints::read_config(&path, args.config.is_some())
        .ok()
        .flatten()
}

fn model_of(args: &Args, setup: &Setup) -> String {
    let config = saved_config(args);
    settings::resolved_model(&setup.flags, config.as_ref().map(|c| &c.settings))
}

fn saved_display(
    flags: &[Given],
    saved: Option<&wisp_core::config::SavedSettings>,
) -> SavedDisplay {
    let saved_pairs: Vec<(String, String)> = saved
        .into_iter()
        .flat_map(|s| s.iter())
        .filter(|(path, _)| shown_by_client(path))
        .map(|(path, entry)| (path.to_string(), entry.value.text()))
        .collect();
    let flag_pairs: Vec<(String, String)> = flags
        .iter()
        .filter_map(|g| match &g.target {
            Target::Name(spec) if spec.owner == wisp_protocol::settings::Owner::Client => {
                Some((spec.name.to_owned(), g.value.text()))
            }
            Target::Path(path) if shown_by_client(path) => Some((path.to_string(), g.value.text())),
            Target::Name(_) | Target::Path(_) => None,
        })
        .collect();
    SavedDisplay::from_pairs(saved_pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .with_flags(flag_pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())))
}

fn display_of(setup: &Setup, sources: &endpoints::Sources) -> SavedDisplay {
    let config = sources.config.as_ref().ok().and_then(Option::as_ref);
    saved_display(&setup.flags, config.map(|c| &c.settings))
}

pub(crate) fn prompt_caps(display: &SavedDisplay) -> wisp_tui::term::Caps {
    wisp_tui::term::Caps::resolve_saved(
        &wisp_tui::term::Env::current(),
        wisp_tui::term::Streams::current(),
        wisp_tui::term::Flags::default(),
        display,
        None,
    )
}

fn flags<'a>(
    args: &'a Args,
    setup: &'a Setup,
    keychain: &'a keychain::Keychain,
) -> endpoints::Flags<'a> {
    endpoints::Flags {
        env_file: args.env_file.as_deref(),
        config: args.config.as_deref(),
        endpoint: setup.endpoint.as_deref(),
        keychain,
        set: &setup.flags,
    }
}

fn sources(args: &Args, setup: &Setup, keychain: &keychain::Keychain) -> endpoints::Sources {
    endpoints::Sources::load(&flags(args, setup, keychain), xdg_config_home(), home_dir())
}

fn doctor(
    args: &Args,
    setup: &Setup,
    keychain: &keychain::Keychain,
    check: Option<Vec<String>>,
) -> ExitCode {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let home = std::env::var("HOME").ok();
    let mut sources = sources(args, setup, keychain);
    if let Some(path) = &args.mcp_config
        && let Err(err) = sources.add_servers(path)
    {
        eprintln!("wisp: {err}");
        return ExitCode::FAILURE;
    }
    let book = sources.book().ok();
    let start_endpoint = setup
        .endpoint
        .as_deref()
        .or_else(|| book.as_ref().map(|b| b.start.as_str()));
    let resolved = Run::resolve(setup, &sources, start_endpoint);
    let variables = keychain::keychain(&sources.inputs);
    let report = endpoints::doctor(
        &sources,
        keychain,
        variables.as_ref().map_err(String::as_str),
        home.as_deref(),
    );
    let cwd = std::env::current_dir().unwrap_or_default();
    let mcp = match start::mcp_service(&sources, cwd.clone(), keychain, false) {
        Ok(mcp) => mcp,
        Err(err) => {
            eprintln!("wisp: {err}");
            return ExitCode::FAILURE;
        }
    };
    let endpoints = book
        .as_ref()
        .map(|book| wisp_core::endpoints::entry_redactions(&book.entries));
    let held = sources.held_redactions(endpoints.as_ref());
    let (hook_rows, hooks_broken) = hooks_report(&tools::registry().0);
    let loaded = context::load(sources.config_path.as_deref()).with_flags(&resolved.flags);
    let workspace = wisp_core::plugin::DoctorWorkspace {
        cwd,
        plugin_data: JsonlStore::default_plugin_data_dir(xdg_data_home(), home_dir()),
    };
    let mut written = wisp_tui::term::doctor::run(&mut out, &display_of(setup, &sources))
        .and_then(|()| tools_doctor(&mut out, mcp, &held, &workspace))
        .and_then(|()| {
            out.write_all(rows_text("hooks", &hook_rows).as_bytes())?;
            out.write_all(rows_text("plugins", &plugin_rows(home.as_deref())).as_bytes())?;
            out.flush()
        })
        .and_then(|()| store_doctor(&mut out, home.as_deref()))
        .and_then(|()| instructions_doctor(&mut out, &resolved, &sources, &loaded))
        .and_then(|()| context_doctor(&mut out, &resolved, &sources, &loaded))
        .and_then(|()| search_doctor(&mut out, &resolved, &sources))
        .and_then(|()| agents_doctor(&mut out, &resolved, &sources))
        .and_then(|()| {
            out.write_all(report.text.as_bytes())?;
            out.flush()
        });
    let mut broken = report.broken || hooks_broken;
    let targets = match &check {
        Some(names) if written.is_ok() => {
            let targets = endpoints::check_targets(
                &sources,
                variables.as_ref().map_err(String::as_str),
                names,
            );
            if targets.is_none() {
                broken = true;
                let skipped = endpoints::check_skipped(&sources.file_words());
                written = writeln!(out, "\naccount check\n  {skipped}").and_then(|()| out.flush());
            }
            targets
        }
        _ => None,
    };
    if let Some(targets) = targets {
        let check_model = Run::resolve(setup, &sources, None).model;
        let checked = wisp_core::runtime::build(&mut tokio::runtime::Builder::new_current_thread())
            .map(|runtime| {
                runtime.block_on(endpoints::check(
                    targets,
                    &check_model,
                    wisp_core::endpoints::CHECK_TIMEOUT,
                    models_folder().as_deref(),
                ))
            });
        match checked {
            Ok(checked) => {
                broken |= checked.broken;
                written = out
                    .write_all(checked.text.as_bytes())
                    .and_then(|()| out.flush());
            }
            Err(e) => written = Err(e),
        }
    }
    match written {
        Ok(()) if broken => ExitCode::FAILURE,
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("wisp: doctor: {e}");
            ExitCode::FAILURE
        }
    }
}

fn store_doctor(out: &mut impl std::io::Write, home: Option<&str>) -> std::io::Result<()> {
    let folders = [
        (
            "attachments",
            JsonlStore::default_attachments_dir(xdg_data_home(), home_dir()),
        ),
        (
            "images",
            JsonlStore::default_images_dir(xdg_data_home(), home_dir()),
        ),
    ];
    let mut text = String::new();
    for (title, dir) in folders {
        text.push_str(&format!("\n{title}\n"));
        for (key, value) in folder_rows(dir.as_deref(), home) {
            let value = wisp_tui::term::doctor::clean(&value);
            text.push_str(&format!("  {key:<18} {value}\n"));
        }
    }
    out.write_all(text.as_bytes())?;
    out.flush()
}

fn folder_rows(dir: Option<&Path>, home: Option<&str>) -> [(&'static str, String); 2] {
    let Some(dir) = dir else {
        return [
            ("folder", "none (set HOME or XDG_DATA_HOME)".to_owned()),
            ("saved", "none yet".to_owned()),
        ];
    };
    let folder = wisp_render::chrome::home_short(&dir.to_string_lossy(), home);
    let saved = match wisp_core::session::images_usage(dir) {
        Ok(None) => "none yet".to_owned(),
        Ok(Some(usage)) => {
            let noun = if usage.files == 1 { "file" } else { "files" };
            format!("{} {noun}, {}", usage.files, decimal_size(usage.bytes))
        }
        Err(e) => format!("error: {e}"),
    };
    [("folder", folder), ("saved", saved)]
}

fn decimal_size(bytes: u64) -> String {
    #[allow(
        clippy::cast_precision_loss,
        reason = "a size shown to one decimal place"
    )]
    let value = bytes as f64;
    if bytes < 1_000 {
        format!("{bytes} B")
    } else if bytes < 1_000_000 {
        format!("{:.1} kB", value / 1e3)
    } else if bytes < 1_000_000_000 {
        format!("{:.1} MB", value / 1e6)
    } else {
        format!("{:.1} GB", value / 1e9)
    }
}

fn instruction_paths(start: PathBuf) -> wisp_core::instructions::Paths {
    instructions::paths(start, xdg_config_home(), home_dir())
}

fn instructions_doctor(
    out: &mut impl std::io::Write,
    resolved: &Run,
    sources: &endpoints::Sources,
    loaded: &context::Loaded,
) -> std::io::Result<()> {
    let settings = &resolved.instructions;
    let model = resolved.model.as_str();
    let info = match sources.book() {
        Ok(b) => (b.active.priced())
            .then(|| {
                loaded
                    .settings
                    .price(b.active.name.as_str(), model, b.active.provider)
            })
            .flatten(),
        Err(_) => loaded
            .settings
            .price(wisp_protocol::CLAUDE_API, model, ProviderKind::IMPLICIT),
    };
    let text = match std::env::current_dir() {
        Ok(start) => {
            let (registry, _) = tools::registry();
            let rows = doctor_rows(
                &registry,
                &instruction_paths(start),
                settings,
                info.as_ref().map(|info| (model, info)),
            )?;
            doctor_text(Some(rows), None)
        }
        Err(e) => doctor_text(
            None,
            Some(&format!("could not find the working directory: {e}")),
        ),
    };
    out.write_all(text.as_bytes())?;
    out.flush()
}

fn doctor_rows(
    registry: &Registry,
    paths: &Paths,
    settings: &Settings,
    priced: Option<(&str, &wisp_core::pricing::ModelInfo)>,
) -> std::io::Result<Vec<wisp_protocol::DoctorRow>> {
    let ask = InstructionsDoctor {
        start: paths.start.clone(),
        home: paths.home.clone(),
        config_dir: paths.config_dir.clone(),
        source: &settings.source,
        warn_tokens: settings.warn_tokens,
        priced,
    };
    let runtime = wisp_core::runtime::build(&mut tokio::runtime::Builder::new_current_thread())?;
    Ok(runtime.block_on(wisp_core::plugin::instructions_doctor(registry, ask)))
}

fn doctor_text(rows: Option<Vec<wisp_protocol::DoctorRow>>, failure: Option<&str>) -> String {
    let rows = match (rows, failure) {
        (Some(rows), _) => rows,
        (None, Some(failure)) => vec![wisp_protocol::DoctorRow::text("error", failure)],
        (None, None) => Vec::new(),
    };
    format!("\ninstructions\n{}", crate::doctor_lines(rows, 9))
}

fn context_doctor(
    out: &mut impl std::io::Write,
    resolved: &Run,
    sources: &endpoints::Sources,
    loaded: &context::Loaded,
) -> std::io::Result<()> {
    let book = sources.book().ok();
    let endpoint = book.as_ref().map_or_else(
        || wisp_protocol::CLAUDE_API.to_owned(),
        |b| b.start.as_str().to_owned(),
    );
    let rows = context::doctor_rows(
        loaded,
        &endpoint,
        &resolved.model,
        context::Run {
            max_tokens: agent::DEFAULT_MAX_TOKENS,
            auto_compact: resolved.auto_compact,
            cache_ttl: resolved.cache_ttl,
        },
        &doctor_connections(book.as_ref()),
    );
    out.write_all(context::doctor_text(&rows).as_bytes())?;
    out.flush()
}

fn search_doctor(
    out: &mut impl std::io::Write,
    resolved: &Run,
    sources: &endpoints::Sources,
) -> std::io::Result<()> {
    let loaded = context::load(sources.config_path.as_deref()).with_flags(&resolved.flags);
    let book = sources.book().ok();
    let listed = book.as_ref().map_or_else(Vec::new, search_endpoints);
    let requested = tools::web_search_on(resolved.tools.as_deref());
    let ready = doctor_connections(book.as_ref());
    let rows = context::search_rows(&loaded, &listed, &resolved.model, requested, &ready);
    out.write_all(context::search_text(&rows).as_bytes())?;
    out.flush()
}

fn agents_doctor(
    out: &mut impl std::io::Write,
    resolved: &Run,
    sources: &endpoints::Sources,
) -> std::io::Result<()> {
    let config = match &sources.config {
        Ok(Some(file)) => agents::Config::Read(file),
        Ok(None) => agents::Config::Missing,
        Err(_) => agents::Config::Broken,
    };
    let place = preset_place(sources, resolved.instructions.warn_tokens);
    let view = sources.book().ok().map(|book| {
        let (ready, unavailable) =
            endpoints::connections(&book.entries, models_folder().as_deref());
        endpoints::view(&book.entries, &ready, &unavailable)
    });
    let rows = agents::doctor_rows(config, &resolved.flags, &place, view.as_ref());
    out.write_all(agents::doctor_text(&rows).as_bytes())?;
    out.flush()
}

fn doctor_connections(book: Option<&EndpointBook>) -> Vec<agent::Connection> {
    let folder = models_folder();
    let ready = |book: &EndpointBook| endpoints::connections(&book.entries, folder.as_deref()).0;
    book.map_or_else(Vec::new, ready)
}

fn hooks_report(registry: &wisp_core::plugin::Registry) -> (Vec<(String, String)>, bool) {
    let mut problems = registry.hook_table().problems();
    problems.extend(registry.section_problems());
    let broken = problems.iter().any(|p| p.cycle);
    let rows = problems
        .into_iter()
        .map(|p| (p.plugin, p.message))
        .collect();
    (rows, broken)
}

fn plugin_rows(home: Option<&str>) -> Vec<(String, String)> {
    let folders = tools::wasm_folders();
    if folders.is_empty() {
        return Vec::new();
    }
    let (_, problems) = tools::registry();
    let mut rows = Vec::new();
    for (id, folder) in folders {
        let shown = wisp_render::chrome::home_short(&folder.to_string_lossy(), home);
        rows.push((id.clone(), shown));
        rows.extend(
            problems
                .iter()
                .filter(|p| p.plugin == id)
                .map(|p| (id.clone(), p.message.clone())),
        );
    }
    rows
}

fn rows_text(title: &str, rows: &[(String, String)]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let mut text = format!("\n{title}\n");
    for (plugin, message) in rows {
        let plugin = wisp_tui::term::doctor::clean(plugin);
        let message = wisp_tui::term::doctor::clean(message);
        text.push_str(&format!("  {plugin:<18} {message}\n"));
    }
    text
}

fn doctor_lines(rows: Vec<wisp_protocol::DoctorRow>, key: usize) -> String {
    use wisp_protocol::DoctorCell;
    let rows: Vec<wisp_protocol::DoctorRow> = rows
        .into_iter()
        .map(|row| wisp_protocol::DoctorRow {
            key: row.key,
            cells: row
                .cells
                .into_iter()
                .map(|cell| match cell {
                    DoctorCell::Text { text } => DoctorCell::Text {
                        text: wisp_tui::term::doctor::clean(&text),
                    },
                    bytes @ DoctorCell::Bytes { .. } => bytes,
                })
                .collect(),
        })
        .collect();
    wisp_render::doctor::table(&rows, key)
        .into_iter()
        .map(|line| format!("  {line}\n"))
        .collect()
}

#[cfg(unix)]
fn tools_rows(
    mcp: wisp_core::mcp::Service,
    redactions: &wisp_core::session::Redactions,
    workspace: &wisp_core::plugin::DoctorWorkspace,
) -> std::io::Result<Vec<wisp_protocol::DoctorRow>> {
    let (registry, _) = tools::registry();
    let on = registry.default_on();
    let runtime = wisp_core::runtime::build(&mut tokio::runtime::Builder::new_multi_thread())?;
    let rows = runtime.block_on(wisp_core::plugin::doctor_rows(
        &registry,
        &on,
        mcp,
        redactions,
        workspace.clone(),
    ));
    runtime.shutdown_timeout(start::SHUTDOWN_WAIT);
    Ok(rows)
}

#[cfg(unix)]
fn tools_doctor(
    out: &mut impl std::io::Write,
    mcp: wisp_core::mcp::Service,
    redactions: &wisp_core::session::Redactions,
    workspace: &wisp_core::plugin::DoctorWorkspace,
) -> std::io::Result<()> {
    let text = format!(
        "\ntools\n{}",
        doctor_lines(tools_rows(mcp, redactions, workspace)?, 18)
    );
    out.write_all(text.as_bytes())?;
    out.flush()
}

#[cfg(not(unix))]
fn tools_doctor(
    _out: &mut impl std::io::Write,
    _mcp: wisp_core::mcp::Service,
    _redactions: &wisp_core::session::Redactions,
    _workspace: &wisp_core::plugin::DoctorWorkspace,
) -> std::io::Result<()> {
    Ok(())
}

fn on_runtime<F: Future<Output = ExitCode>>(
    terminal: bool,
    work: impl FnOnce() -> F + Send,
) -> ExitCode {
    if terminal && let Err(e) = wisp_tui::term::hold_deaths(start::SIGNAL_LIMIT) {
        eprintln!("wisp: {e}");
        return ExitCode::FAILURE;
    }
    let runtime = match wisp_core::runtime::build(&mut tokio::runtime::Builder::new_multi_thread())
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("wisp: {e}");
            return ExitCode::FAILURE;
        }
    };
    let code = std::thread::scope(|scope| {
        let root = std::thread::Builder::new()
            .name("wisp-root".to_owned())
            .stack_size(wisp_core::runtime::THREAD_STACK_BYTES)
            .spawn_scoped(scope, move || {
                let code = runtime.block_on(work());
                runtime.shutdown_timeout(start::SHUTDOWN_WAIT);
                code
            });
        match root.map(std::thread::ScopedJoinHandle::join) {
            Ok(Ok(code)) => code,
            Ok(Err(_)) => ExitCode::from(ROOT_PANICKED),
            Err(e) => {
                eprintln!("wisp: {e}");
                ExitCode::FAILURE
            }
        }
    });
    if terminal {
        wisp_tui::term::die_if_dying();
    }
    code
}

const ROOT_PANICKED: u8 = 101;

struct Started {
    channels: Channels,
    opening: wisp_protocol::Opening,
    task: agent::AgentTask,
    presentation: Presentation,
    view: Option<wisp_protocol::SessionView>,
}

enum Stop {
    Failed(String),
    Ask(Box<wisp_tui::app::setup::Ask>),
}

impl From<String> for Stop {
    fn from(text: String) -> Self {
        Self::Failed(text)
    }
}

impl From<start::Stop> for Stop {
    fn from(stop: start::Stop) -> Self {
        match stop {
            start::Stop::Failed(text) => Self::Failed(text),
            start::Stop::Ask(asking) => {
                let start::Asking {
                    question,
                    book,
                    model,
                    effort,
                    cache_ttl,
                    size,
                    on_accounts,
                } = *asking;
                let (facts, view) = ask_facts(
                    &book,
                    &question.left.name,
                    &model,
                    effort,
                    cache_ttl,
                    on_accounts,
                );
                Self::Ask(Box::new(question.asked(facts, &view, size)))
            }
        }
    }
}

fn ask_facts(
    book: &EndpointBook,
    account: &str,
    model: &str,
    effort: Effort,
    cache_ttl: CacheTtl,
    on_accounts: Vec<wisp_render::chrome::AccountFacts>,
) -> (SessionFacts, wisp_core::agents::endpoints::EndpointView) {
    let (ready, unavailable) = endpoints::connections(&book.entries, models_folder().as_deref());
    let facts = SessionFacts {
        endpoint_name: Some(account.to_owned()),
        endpoints: endpoints::endpoint_facts(&book.entries, &book.marks, &unavailable, &ready),
        models: tui::models(&ready),
        on_accounts,
        ..tui::facts(
            tui::Chosen {
                model,
                effort: effort.as_str(),
                cache_ttl,
                endpoint: endpoints::reach(&book.active),
                max_tokens: agent::DEFAULT_MAX_TOKENS,
                claude_usage: false,
            },
            None,
            None,
            None,
        )
    };
    (facts, endpoints::view(&book.entries, &ready, &unavailable))
}

fn tool_rule(presenter: wisp_tui::present::choose::Presenter) -> tools::ToolRule {
    use wisp_tui::present::choose::Presenter;
    match presenter {
        Presenter::Tui => tools::ToolRule::Configured,
        Presenter::Jsonl => tools::ToolRule::Flagged,
    }
}

fn asks(chosen: &wisp_tui::present::choose::Chosen, at: Option<&StartAt>) -> bool {
    chosen.presenter == wisp_tui::present::choose::Presenter::Tui
        && at.is_none()
        && std::io::stdin().is_terminal()
        && std::io::stderr().is_terminal()
}

async fn start_session(
    args: &Args,
    setup: &Setup,
    keychain: &Arc<keychain::Keychain>,
    chosen: &wisp_tui::present::choose::Chosen,
    at: Option<&StartAt>,
) -> Result<Started, Stop> {
    let (built, presentation) = build_session(args, setup, keychain, chosen, at, None).await?;
    let started = start::start(built);
    Ok(Started {
        channels: started.channels,
        opening: started.opening,
        task: started.task,
        presentation,
        view: started.view,
    })
}

async fn build_session(
    args: &Args,
    setup: &Setup,
    keychain: &Arc<keychain::Keychain>,
    chosen: &wisp_tui::present::choose::Chosen,
    at: Option<&StartAt>,
    host: Option<&wisp_protocol::link::Start>,
) -> Result<(start::Built, Presentation), Stop> {
    use wisp_tui::app::resumed::StartNote;
    use wisp_tui::present::choose::Presenter;
    let on_tui = chosen.presenter == Presenter::Tui;
    let inputs = start::Inputs {
        config: args.config.as_deref(),
        mcp_config: args.mcp_config.as_deref(),
        env_file: args.env_file.as_deref(),
        keychain,
        setup,
        resume: args.resume.as_ref().map(|id| resume::Request {
            id: id.as_deref(),
            unlocked: args.resume_unlocked,
        }),
        session_id: args.session_id.as_deref().and_then(OsStr::to_str),
        at,
        asks: host.map_or_else(|| asks(chosen, at), |start| start.asks && at.is_none()),
        tool_rule: tool_rule(chosen.presenter),
        full_screen: on_tui,
        restarted: args.restarted,
        forced: forced_layer(
            on_tui && host.is_none(),
            &wisp_tui::term::Env::current(),
            wisp_tui::term::Streams::current(),
        ),
        voice: false,
        sessions: JsonlStore::default_dir(xdg_data_home(), home_dir()),
        presets: wisp_core::agents::Presets::default(),
        picked: start::Picked::default(),
    };
    let mut opened = start::open(inputs).await?;
    let model = model_of(args, &opened.setup);
    let ctx = manage_context(args, &opened.setup, &model, keychain);
    let mut start_note = None;
    if on_tui && let Some((name, left)) = &opened.gone {
        let marks = opened
            .sources
            .marks(&opened.sources.entries().unwrap_or_default());
        let marks = |name: &str| marked_in(&marks, name);
        start_note = left.started(name, &marks);
        if left.state == Some(resume::State::Gone) {
            let moved = EndpointName::parse(name)
                .ok_or_else(|| format!("{name} is not an account name"))
                .and_then(|to| manage::move_start(&to, &ctx));
            if let Err(e) = moved {
                start_note = None;
                opened.start_line = Some(format!("{} ({e})", left.continuing(name)));
            }
        }
    }
    let start_note = start_note.or(opened.said.take().map(StartNote::Said));
    let mut host = manage::host::CliEndpointHost::of(
        &ctx,
        opened.setup.endpoint.as_deref(),
        Arc::clone(keychain),
        Budget::Short,
    );
    let host: start::Host = Box::new(move |catalog| {
        host.catalog = Some(catalog);
        Arc::new(host)
    });
    let accounts = opened
        .resumed
        .as_ref()
        .and_then(|_| auth::accounts::gather(&ctx).ok())
        .map(|accounts| start::Accounts {
            unusable: accounts.unusable(),
            reaches: Box::new(move |name| auth::login::reaches(name, &accounts)),
        });
    let card = card::setup(&opened.sources, &ctx);
    let mut built = start::build(opened, Some(host), accounts).await?;
    built.opening.card = card;
    built
        .opening
        .start
        .get_or_insert_with(Default::default)
        .note = start_note;
    let presentation = Presentation {
        presenter: chosen.presenter,
        verbose: matches!(
            built.setup.value(names::DISPLAY_VERBOSE),
            Some(SettingValue::Bool(true))
        ),
        tui_verbose: built.resolved.verbose,
        display: display_of(&built.setup, &built.sources),
    };
    Ok((built, presentation))
}

fn run(args: Args, setup: Setup, keychain: Arc<keychain::Keychain>) -> ExitCode {
    on_runtime(true, || {
        session(args, setup, keychain, keep::Back::default())
    })
}

fn relaunch(args: &Args, id: String) -> Args {
    let mut next = Args {
        config: args.config.clone(),
        mcp_config: args.mcp_config.clone(),
        env_file: args.env_file.clone(),
        keychain: args.keychain,
        verbose: args.verbose,
        output: args.output,
        set: resume::pick::kept_sets(&args.set),
        resume: Some(Some(id)),
        ..Args::default()
    };
    for (flag, path) in [
        ("--config", &next.config),
        ("--mcp-config", &next.mcp_config),
        ("--env-file", &next.env_file),
    ] {
        if let Some(path) = path {
            next.host_args
                .extend([flag.into(), path.clone().into_os_string()]);
        }
    }
    if let Some(keychain) = next.keychain {
        next.host_args
            .push(format!("--keychain={}", keychain.as_str()).into());
    }
    if next.verbose {
        next.host_args.push("--verbose".into());
    }
    if let Some(output) = clap::ValueEnum::to_possible_value(&next.output) {
        next.host_args
            .push(format!("--output={}", output.get_name()).into());
    }
    for set in &next.set {
        next.host_args.push(format!("--set={set}").into());
    }
    next.host_args.push(
        format!(
            "--resume={}",
            next.resume
                .as_ref()
                .and_then(Option::as_ref)
                .map_or("", String::as_str)
        )
        .into(),
    );
    next
}

struct Chooser {
    log: bool,
    display: SavedDisplay,
    verbose: bool,
    known: Vec<String>,
    marks: std::collections::BTreeMap<wisp_core::endpoints::EndpointName, u8>,
}

impl Chooser {
    fn of(args: &Args, setup: &Setup, keychain: &keychain::Keychain) -> Chooser {
        let sources = endpoints::Sources::load_with(
            &flags(args, setup, keychain),
            xdg_config_home(),
            home_dir(),
            false,
        );
        let resolved = Run::resolve(setup, &sources, None);
        let entries = sources.entries().unwrap_or_default();
        Chooser {
            log: resolved.log,
            display: display_of(setup, &sources),
            verbose: resolved.verbose,
            known: entries.keys().map(ToString::to_string).collect(),
            marks: sources.marks(&entries),
        }
    }

    fn chats(&self, here: &str) -> Vec<wisp_protocol::ChatRow> {
        JsonlStore::default_dir(xdg_data_home(), home_dir())
            .and_then(|dir| {
                wisp_core::resume::chat_rows(
                    dir,
                    here,
                    None,
                    jiff::Timestamp::now(),
                    |name| self.known.iter().any(|known| known == name),
                    |name| marked_in(&self.marks, name),
                )
                .ok()
            })
            .unwrap_or_default()
    }

    async fn pick(
        &self,
        chats: Vec<wisp_protocol::ChatRow>,
        focus: Option<&str>,
    ) -> Option<String> {
        tui::pick(chats, focus, &self.display, self.verbose).await
    }
}

enum Completion {
    Local(agent::AgentTask),
    Hosted(Box<wisp_client::link::Link>),
}

impl Completion {
    async fn end(self, end: output::RunEnd, back: &mut keep::Back) -> (ExitCode, Option<String>) {
        let session = match &self {
            Self::Hosted(link) => Some(link.welcome.session.clone()),
            Self::Local(_) => None,
        };
        let code = match self {
            Self::Local(task) => {
                start::end(
                    "wisp",
                    task,
                    end.forced,
                    tokio::time::Instant::now() + start::SHUTDOWN_WAIT,
                    end.exit_code(),
                )
                .await
            }
            Self::Hosted(mut link) => keep::finish(&mut link, &end).await,
        };
        match end.cut {
            Some(block) => match back.next(block, session, tokio::time::Instant::now()) {
                Ok(id) => (code, Some(id)),
                Err(block) => {
                    back.failed(Some(block));
                    (ExitCode::FAILURE, None)
                }
            },
            None => (code, end.resume),
        }
    }
}

enum WindowStop {
    Start(Stop),
    Done(ExitCode),
}

impl From<Stop> for WindowStop {
    fn from(stop: Stop) -> Self {
        Self::Start(stop)
    }
}

impl From<String> for WindowStop {
    fn from(text: String) -> Self {
        Self::Start(Stop::Failed(text))
    }
}

struct WindowNotes<'a> {
    warnings: Vec<String>,
    wsl_error: Option<&'a str>,
    back: Option<&'a mut keep::Back>,
}

async fn start_window(
    program: &Path,
    args: &Args,
    setup: &Setup,
    keychain: &Arc<keychain::Keychain>,
    chosen: &wisp_tui::present::choose::Chosen,
    at: Option<&StartAt>,
    notes: WindowNotes<'_>,
) -> Result<(setup::SessionStart, CardSetup, Completion), WindowStop> {
    let WindowNotes {
        warnings: mut notes,
        wsl_error,
        mut back,
    } = notes;
    let leave_note = match keep::open(
        program,
        args,
        setup,
        keychain,
        chosen,
        at,
        back.as_deref_mut(),
    )
    .await?
    {
        keep::Opened::Link(mut link) => {
            let (mut session, card) = keep::session(
                &mut link,
                notes,
                args.restarted || back.as_ref().is_some_and(|back| back.restarting),
                back.as_ref()
                    .and_then(|back| back.unsent.as_ref())
                    .is_some_and(|unsent| !unsent.is_empty()),
            );
            session.session.alerts = keep::windows_notes(wsl_error);
            return Ok((session, card, Completion::Hosted(link)));
        }
        keep::Opened::Done(code) => return Err(WindowStop::Done(code)),
        keep::Opened::Window(note) => Some(note),
        keep::Opened::Ask => None,
    };
    notes.extend(leave_note.iter().cloned());
    let started = start_session(args, setup, keychain, chosen, at).await?;
    let (mut session, card) = tui::session(started.opening, started.view, started.channels);
    session.session.leave_note = leave_note;
    session.warnings.extend(notes);
    session.session.alerts = keep::windows_notes(wsl_error);
    Ok((session, card, Completion::Local(started.task)))
}

enum Running {
    Window(setup::SessionStart, CardSetup, Completion),
    Inline(Started),
}

async fn session(
    mut args: Args,
    mut setup: Setup,
    keychain: Arc<keychain::Keychain>,
    mut back: keep::Back,
) -> ExitCode {
    let chosen = match output::choose(args.output) {
        Ok(chosen) => chosen,
        Err(e) => {
            let pending = back.failed(None);
            eprintln!("wisp: {e}");
            return if pending {
                ExitCode::FAILURE
            } else {
                ExitCode::from(2)
            };
        }
    };
    let on_tui = chosen.presenter == wisp_tui::present::choose::Presenter::Tui;
    let program = match std::env::current_exe() {
        Ok(program) => program,
        Err(e) => {
            back.failed(None);
            eprintln!("wisp: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut wsl_error = if back.note().is_some() {
        None
    } else {
        match keep::offer(&args, &setup, &keychain, &chosen) {
            Ok(error) => error,
            Err(code) => return code,
        }
    };
    let here = resume::here();
    let mut picked = false;
    loop {
        let mut note = None;
        if on_tui
            && matches!(args.resume, Some(None))
            && let Some(here) = here.as_deref()
        {
            let chooser = Chooser::of(&args, &setup, &keychain);
            if chooser.log {
                match resume::pick::choose(chooser.chats(here), here) {
                    resume::pick::Choice::Open { id, note: newer } => {
                        args.resume = Some(Some(id));
                        note = newer;
                    }
                    resume::pick::Choice::Refuse(text) => {
                        eprintln!("{text}");
                        return ExitCode::FAILURE;
                    }
                    resume::pick::Choice::Pick { chats } => match chooser.pick(chats, None).await {
                        Some(id) => {
                            args.resume = Some(Some(id));
                            picked = true;
                        }
                        None => return ExitCode::SUCCESS,
                    },
                }
            }
        }
        let running = if on_tui {
            start_window(
                &program,
                &args,
                &setup,
                &keychain,
                &chosen,
                None,
                WindowNotes {
                    warnings: note.into_iter().chain(back.note()).collect(),
                    wsl_error: wsl_error.as_deref(),
                    back: Some(&mut back),
                },
            )
            .await
            .map(|(session, card, task)| Running::Window(session, card, task))
        } else {
            start_session(&args, &setup, &keychain, &chosen, None)
                .await
                .map(Running::Inline)
                .map_err(WindowStop::Start)
        };
        let running = match running {
            Ok(running) => running,
            Err(WindowStop::Done(_)) if back.failed(None) => return ExitCode::FAILURE,
            Err(WindowStop::Done(code)) => return code,
            Err(WindowStop::Start(Stop::Ask(mut ask))) => {
                if back.failed(None) {
                    return ExitCode::FAILURE;
                }
                if let (true, Some(Some(id)), Some(here)) = (picked, &args.resume, here.as_deref())
                {
                    let chats = Chooser::of(&args, &setup, &keychain).chats(here);
                    ask.back = Some((chats, id.clone()));
                }
                let (code, resume) = setup_session(
                    &args,
                    &setup,
                    &keychain,
                    Setting::Choose(ask),
                    wsl_error.as_deref(),
                    &mut back,
                )
                .await;
                let Some(id) = resume else {
                    return code;
                };
                args = relaunch(&args, id);
                setup = match setup_of(&args) {
                    Ok(setup) => setup,
                    Err(_) if back.failed(None) => return ExitCode::FAILURE,
                    Err(code) => return code,
                };
                picked |= back.note().is_none();
                continue;
            }
            Err(WindowStop::Start(Stop::Failed(text))) => {
                if back.failed(None) {
                    eprintln!("{text}");
                    return ExitCode::FAILURE;
                }
                let dir = JsonlStore::default_dir(xdg_data_home(), home_dir());
                if let (true, Some(Some(id)), Some(here)) = (picked, &args.resume, here.as_deref())
                    && resume::pick::taken(dir, id)
                {
                    let chooser = Chooser::of(&args, &setup, &keychain);
                    match chooser.pick(chooser.chats(here), Some(id)).await {
                        Some(id) => {
                            args.resume = Some(Some(id));
                            continue;
                        }
                        None => return ExitCode::SUCCESS,
                    }
                }
                eprintln!("{text}");
                return ExitCode::FAILURE;
            }
        };
        wsl_error = None;
        back.started();
        args.restarted = false;
        let (end, task) = match running {
            Running::Window(session, card, task) => {
                let chooser = Chooser::of(&args, &setup, &keychain);
                let opening = Opening::Session {
                    session: Box::new(session.session),
                    card,
                    notes: session.warnings,
                    started: session.started,
                };
                (
                    tui::open(
                        opening,
                        session.quit,
                        &chooser.display,
                        chooser.verbose,
                        &mut back,
                    )
                    .await,
                    task,
                )
            }
            Running::Inline(started) => (
                output::run(
                    &started.presentation,
                    started.channels,
                    started.opening,
                    started.view,
                )
                .await,
                Completion::Local(started.task),
            ),
        };
        let cut = end.cut.is_some();
        let (code, resume) = task.end(end, &mut back).await;
        let Some(id) = resume else {
            return code;
        };
        args = relaunch(&args, id);
        setup = match setup_of(&args) {
            Ok(setup) => setup,
            Err(_) if back.failed(None) => return ExitCode::FAILURE,
            Err(code) => return code,
        };
        picked |= !cut;
    }
}

fn setup_run(
    args: Args,
    setup: Setup,
    keychain: Arc<keychain::Keychain>,
    card: CardSetup,
) -> ExitCode {
    on_runtime(true, || async move {
        let mut back = keep::Back::default();
        let (code, resume) = setup_session(
            &args,
            &setup,
            &keychain,
            Setting::FirstRun(card),
            None,
            &mut back,
        )
        .await;
        let Some(id) = resume else {
            return code;
        };
        let args = relaunch(&args, id);
        let setup = match setup_of(&args) {
            Ok(setup) => setup,
            Err(_) if back.failed(None) => return ExitCode::FAILURE,
            Err(code) => return code,
        };
        session(args, setup, keychain, back).await
    })
}

enum Setting {
    FirstRun(CardSetup),
    Choose(Box<wisp_tui::app::setup::Ask>),
}

async fn setup_session(
    args: &Args,
    setup: &Setup,
    keychain: &Arc<keychain::Keychain>,
    setting: Setting,
    wsl_error: Option<&str>,
    back: &mut keep::Back,
) -> (ExitCode, Option<String>) {
    let chosen = match output::choose(args.output) {
        Ok(chosen) => chosen,
        Err(e) => {
            eprintln!("wisp: {e}");
            return (ExitCode::from(2), None);
        }
    };
    let sources = endpoints::Sources::load_with(
        &flags(args, setup, keychain),
        xdg_config_home(),
        home_dir(),
        true,
    );
    let resolved = Run::resolve(setup, &sources, None);
    let model = model_of(args, setup);
    let ctx = manage_context(args, setup, &model, keychain);
    let host = manage::host::CliEndpointHost::of(
        &ctx,
        setup.endpoint.as_deref(),
        Arc::clone(keychain),
        Budget::Fresh(TERMINAL_DEADLINE),
    );
    let (commands, server_commands) = tokio::sync::mpsc::channel(16);
    let (server_events, events) = tokio::sync::mpsc::channel(64);
    let (started, start) = tokio::sync::oneshot::channel();
    let channels = SetupChannels { events, commands };
    let opening = match setting {
        Setting::FirstRun(card) => Opening::FirstRun {
            setup: channels,
            card,
            start,
        },
        Setting::Choose(ask) => Opening::Choose {
            setup: channels,
            card: card::setup(&sources, &ctx),
            start,
            ask,
        },
    };
    let program = match std::env::current_exe() {
        Ok(program) => program,
        Err(e) => {
            eprintln!("wisp: {e}");
            return (ExitCode::FAILURE, None);
        }
    };
    let server = setup_server::serve(
        &host,
        server_commands,
        server_events,
        started,
        async |account, follow| {
            let at = StartAt { account, follow };
            start_window(
                &program,
                args,
                setup,
                keychain,
                &chosen,
                Some(&at),
                WindowNotes {
                    warnings: Vec::new(),
                    wsl_error,
                    back: None,
                },
            )
            .await
            .map_err(|stop| match stop {
                WindowStop::Start(Stop::Failed(text)) => text,
                WindowStop::Start(Stop::Ask(ask)) => ask.lead.plain(),
                WindowStop::Done(_) => String::new(),
            })
            .map(|(session, _, task)| (session, task))
        },
    );
    let display = display_of(setup, &sources);
    tokio::pin!(server);
    let mut served = None;
    let mut end = {
        let tui = tui::open(opening, None, &display, resolved.verbose, back);
        tokio::pin!(tui);
        loop {
            tokio::select! {
                end = &mut tui => break end,
                task = &mut server, if served.is_none() => served = Some(task),
            }
        }
    };
    let now = tokio::time::Instant::now();
    let by = now + start::SHUTDOWN_WAIT;
    let wait_until = if end.forced {
        by
    } else {
        now + setup::SAVE_WAIT
    };
    let task = match served {
        Some(task) => task,
        None => tokio::time::timeout_at(wait_until, &mut server)
            .await
            .ok()
            .flatten(),
    };
    match task {
        Some(task) => task.end(end, back).await,
        None if back.failed(end.cut.take()) => (ExitCode::FAILURE, None),
        None => (end.exit_code(), end.resume),
    }
}

#[cfg(test)]
#[path = "../tests/it/support/turn_lines.rs"]
mod turn_lines;

#[cfg(test)]
#[path = "../../wisp-tui/tests/support/pick.rs"]
mod pick;

#[cfg(test)]
#[path = "../../wisp-tui/tests/support/scratch.rs"]
mod scratch;

#[cfg(test)]
#[path = "../../wisp-render/src/testing.rs"]
mod testing;

#[cfg(test)]
#[path = "../tests/it/support/server.rs"]
#[allow(dead_code, reason = "the agreement gateway reads requests through it")]
mod server;

#[cfg(test)]
fn scratch_dir(name: &str) -> scratch::ScratchDir {
    scratch::ScratchDir::new("wisp-cli-", name)
}

#[cfg(test)]
mod tests {
    use wisp_core::context::CompactAt;
    use wisp_protocol::agents::Preset;
    use wisp_render::chrome::account::AccountKind;
    use wisp_start::run::{StartPreset, micros_to_usd};
    use wisp_start::start::{
        extra_flag, instructions_flag, plugin_warnings, unlocked_warning, with_warnings,
    };

    use super::*;

    fn session_facts(
        book: &EndpointBook,
        chosen: tui::Chosen<'_>,
        refused: &[(String, String)],
        ready: &[wisp_core::agent::Connection],
    ) -> SessionFacts {
        wisp_start::models::session_facts(
            book,
            chosen,
            endpoints::summaries(&book.entries, &book.marks, refused, None, ready),
            ready,
        )
        .into()
    }

    #[test]
    fn a_failed_start_lists_its_warnings_after_the_error() {
        let warnings = [
            "wisp: preset reviewer is off".to_owned(),
            "wisp: \x1b[31mred\x1b[0m".to_owned(),
            "wisp: session 01a1822e is open in another Wisp; resuming 01a17f2c instead".to_owned(),
        ];
        assert_eq!(
            with_warnings("wisp: no endpoint".to_owned(), warnings),
            "wisp: no endpoint\nwisp: preset reviewer is off\nwisp: ^[[31mred^[[0m\nwisp: session 01a1822e is open in another Wisp; resuming 01a17f2c instead"
        );
        assert_eq!(
            with_warnings("wisp: no endpoint".to_owned(), Vec::new()),
            "wisp: no endpoint"
        );
    }

    #[test]
    fn the_instructions_flags_reach_the_spec() {
        use wisp_core::instructions::Extra;
        use wisp_protocol::InstructionsSource;
        let typed = |name: &'static str, value: &str| {
            vec![settings::Flag::new(
                "--instructions-extra",
                name,
                SettingValue::Text(value.into()),
            )]
        };
        let none = settings::flag_layer(Vec::new(), &[]).unwrap();
        assert_eq!(instructions_flag(&none), None);
        assert_eq!(extra_flag(&none), None);
        let on = settings::flag_layer(
            typed("instructions.extra", "autonomous"),
            &["instructions.source=none".to_owned()],
        )
        .unwrap();
        assert_eq!(instructions_flag(&on), Some(InstructionsSource::None));
        assert_eq!(extra_flag(&on), Some(Some(Extra::Autonomous)));
        let off =
            settings::flag_layer(Vec::new(), &["instructions.extra=none".to_owned()]).unwrap();
        assert_eq!(extra_flag(&off), Some(None));
    }

    #[test]
    fn plugin_problems_lead_the_startup_warnings_load_before_start() {
        let problem = |plugin: &str, message: &str| wisp_core::plugin::Problem {
            plugin: plugin.into(),
            message: message.into(),
        };
        let warnings = plugin_warnings(
            &[problem("a", "it clashes")],
            &[problem("b", "it panicked")],
            vec!["wisp: preset x is turned off".into()],
        );
        assert_eq!(
            warnings,
            [
                "wisp: plugin a: it clashes",
                "wisp: plugin b: it panicked",
                "wisp: preset x is turned off",
            ]
        );
    }

    mod instructions;
    mod replay;
    mod stack;

    #[test]
    fn forced_display_rows_reach_a_full_screen_session_and_never_jsonl() {
        let env = wisp_tui::term::Env::from_pairs([
            ("LANG", "en_US.UTF-8"),
            ("TERM", "xterm-256color"),
            ("NO_COLOR", "1"),
        ]);
        let tty = wisp_tui::term::Streams {
            stdin_tty: true,
            stdout_tty: true,
            stderr_tty: true,
        };
        let forced: Vec<(String, String)> = forced_layer(true, &env, tty)
            .into_iter()
            .map(|g| (g.target.spec().name.to_owned(), g.origin))
            .collect();
        assert_eq!(
            forced,
            [("display.color".to_owned(), "NO_COLOR is set".to_owned())]
        );
        assert!(forced_layer(false, &env, tty).is_empty());
    }

    fn resolved_with(file: &Path, extra: &[&str]) -> Run {
        started_with(file, extra, None)
    }

    fn started_with(file: &Path, extra: &[&str], preset: Option<&Preset>) -> Run {
        let mut all = vec![
            std::ffi::OsString::from("wisp"),
            std::ffi::OsString::from("--config"),
            file.as_os_str().to_owned(),
        ];
        all.extend(extra.iter().map(std::ffi::OsString::from));
        let args = usage::parse_from::<Args, _>(all).unwrap();
        let setup = setup_of(&args).unwrap();
        let keychain = keychain::Keychain::off();
        let sources = endpoints::Sources::load(&flags(&args, &setup, &keychain), None, None);
        let config = sources.config.as_ref().ok().and_then(Option::as_ref);
        let start = preset.map(|preset| StartPreset {
            preset,
            account: None,
        });
        Run::resolve_on(&setup, config, None, start)
    }

    #[test]
    fn a_main_presets_model_and_effort_lose_to_every_other_layer() {
        let dir = &scratch_dir("main-preset-start").root;
        let empty = dir.join("empty.toml");
        std::fs::write(&empty, "").unwrap();
        let saved = dir.join("saved.toml");
        std::fs::write(&saved, "effort = \"high\"\n").unwrap();
        let preset = Preset {
            name: "brain".into(),
            description: "Teaches".into(),
            model: Some("claude-sonnet-5-5".into()),
            effort: Some(Effort::Low),
            preload: None,
            tools_off: Vec::new(),
            spawn: None,
            instructions: None,
            endpoint: None,
            fallback: Vec::new(),
            worker: None,
        };
        let start = |file: &Path, extra: &[&str]| {
            let run = started_with(file, extra, Some(&preset));
            (run.model, run.effort)
        };
        let sonnet = "claude-sonnet-5-5".to_owned();
        assert_eq!(start(&empty, &[]), (sonnet.clone(), Effort::Low));
        assert_eq!(start(&saved, &[]), (sonnet, Effort::High));
        assert_eq!(
            start(&empty, &["--model", "claude-opus-5-5", "--effort", "max"]),
            ("claude-opus-5-5".to_owned(), Effort::Max)
        );
    }

    #[test]
    fn the_context_note_comes_from_the_flag_then_the_context_table_then_on() {
        let dir = &scratch_dir("context-note").root;
        let config = dir.join("config.toml");
        std::fs::write(&config, "[context]\ncontext_note = false\n").unwrap();
        let empty = dir.join("empty.toml");
        std::fs::write(&empty, "").unwrap();
        let note = |extra: &[&str], file: &Path| resolved_with(file, extra).context_note;
        assert!(note(&[], &empty));
        assert!(!note(&[], &config));
        assert!(note(&["--context-note", "on"], &config));
        assert!(!note(&["--context-note", "off"], &empty));
        assert!(!note(&["--set", "context.context_note=off"], &empty));
    }

    #[test]
    fn claude_usage_comes_from_the_flag_then_the_config_then_on() {
        let dir = &scratch_dir("claude-usage").root;
        let session = agent::default_session();
        let lookup = |extra: &[&str], config: &str| {
            let file = dir.join("config.toml");
            std::fs::write(&file, config).unwrap();
            let run = resolved_with(&file, extra);
            assert_eq!(
                run.chosen(&session, wisp_render::chrome::Endpoint::Official)
                    .claude_usage,
                run.claude_usage
            );
            run.claude_usage
        };
        assert!(lookup(&[], ""));
        assert!(lookup(&[], "claude_usage = true\n"));
        assert!(!lookup(&[], "claude_usage = false\n"));
        assert!(!lookup(&["--set", "claude_usage=off"], ""));
        assert!(lookup(
            &["--set", "claude_usage=on"],
            "claude_usage = false\n"
        ));
    }

    #[test]
    fn a_saved_and_a_set_section_switch_reach_the_display_settings() {
        let dir = &scratch_dir("slot-switch").root;
        let config = dir.join("config.toml");
        std::fs::write(&config, "[plugins.aa.slots]\nnotes = false\n").unwrap();
        let all = vec![
            std::ffi::OsString::from("wisp"),
            std::ffi::OsString::from("--config"),
            config.as_os_str().to_owned(),
        ];
        let args = usage::parse_from::<Args, _>(all).unwrap();
        let mut setup = setup_of(&args).unwrap();
        let Ok(Target::Path(path)) = wisp_protocol::settings::parse_target("plugins.bb.slots.tree")
        else {
            panic!("the switch path was refused");
        };
        setup.flags.push(Given {
            target: Target::Path(path),
            value: SettingValue::Bool(true),
            origin: "--set plugins.bb.slots.tree=on".to_owned(),
        });
        let keychain = keychain::Keychain::off();
        let sources = endpoints::Sources::load(&flags(&args, &setup, &keychain), None, None);
        let display = display_of(&setup, &sources);
        let saved = display.section("plugins.aa.slots.notes").unwrap();
        assert!(!saved.value);
        assert!(saved.source.starts_with("config.toml"), "{}", saved.source);
        let flagged = display.section("plugins.bb.slots.tree").unwrap();
        assert!(flagged.value);
        assert!(flagged.source.starts_with("--set"), "{}", flagged.source);
        assert!(display.section("plugins.aa.hooks.notes").is_none());
    }

    #[test]
    fn the_self_check_is_off_by_default_and_on_with_its_flag_or_set() {
        let on = |extra: &[&str]| {
            let mut all = vec!["wisp", "--config", "/dev/null"];
            all.extend(extra);
            let args = usage::parse_from::<Args, _>(all.into_iter().map(std::ffi::OsString::from))
                .unwrap();
            let setup = setup_of(&args).unwrap();
            let layers = wisp_core::settings::Layers {
                flags: setup.flags.clone(),
                ..Default::default()
            };
            let current = wisp_core::settings::Current {
                endpoint: None,
                model: String::new(),
            };
            let resolved = wisp_core::settings::resolve(&layers, &current);
            resolved
                .get("self_check.before_end")
                .and_then(|r| r.value())
                .cloned()
        };
        assert_eq!(on(&[]), Some(SettingValue::Bool(false)));
        assert_eq!(on(&["--self-check"]), Some(SettingValue::Bool(true)));
        assert_eq!(
            on(&["--set", "self_check.before_end=on"]),
            Some(SettingValue::Bool(true))
        );
    }

    #[test]
    fn automatic_compaction_comes_from_the_flag_then_the_context_table_then_on() {
        let dir = &scratch_dir("auto-compact").root;
        let config = dir.join("config.toml");
        std::fs::write(&config, "[context]\nauto_compact = false\n").unwrap();
        let empty = dir.join("empty.toml");
        std::fs::write(&empty, "").unwrap();
        let run = |extra: &[&str], file: &Path| {
            let run = resolved_with(file, extra);
            let models = wisp_core::context::ModelSettings::shipped_only().with_flags(&run.flags);
            let point = models
                .resolve("env", "my-model", None, agent::DEFAULT_MAX_TOKENS)
                .compact_at;
            (run.auto_compact, point.map(|(at, _)| at))
        };
        assert_eq!(run(&[], &empty), (true, None));
        assert_eq!(run(&[], &config), (false, None));
        assert_eq!(run(&["--no-auto-compact"], &empty), (false, None));
        assert_eq!(
            run(&["--set", "context.auto_compact=off"], &empty),
            (false, None)
        );
        assert_eq!(
            run(&["--compact-at", "400k"], &empty),
            (true, Some(CompactAt::Tokens(400_000)))
        );
        assert_eq!(
            run(&["--compact-at", "85%"], &config),
            (false, CompactAt::parse("85%"))
        );
    }

    #[test]
    fn extra_instructions_come_from_the_flag_then_set_then_the_config_then_none() {
        use wisp_core::instructions::Extra;
        let dir = &scratch_dir("extra").root;
        let config = dir.join("config.toml");
        std::fs::write(&config, "[instructions]\nextra = \"autonomous\"\n").unwrap();
        let empty = dir.join("empty.toml");
        std::fs::write(&empty, "").unwrap();
        let extra = |extra: &[&str], file: &Path| resolved_with(file, extra).instructions_extra;
        assert_eq!(extra(&[], &empty), None);
        assert_eq!(extra(&[], &config), Some(Extra::Autonomous));
        assert_eq!(
            extra(&["--instructions-extra", "autonomous"], &empty),
            Some(Extra::Autonomous)
        );
        assert_eq!(extra(&["--instructions-extra", "none"], &config), None);
        assert_eq!(
            extra(&["--set", "instructions.extra=autonomous"], &empty),
            Some(Extra::Autonomous)
        );
        let refused = Args::try_parse_from(["wisp", "--instructions-extra", "benchmark"])
            .err()
            .unwrap()
            .to_string();
        assert!(refused.contains("--instructions-extra"), "{refused}");
        assert!(
            refused.contains("none") && refused.contains("autonomous"),
            "{refused}"
        );
    }

    #[test]
    fn counting_comes_from_set_then_the_context_table_then_on() {
        let dir = &scratch_dir("count-tokens").root;
        let config = dir.join("config.toml");
        std::fs::write(&config, "[context]\ncount_tokens = false\n").unwrap();
        let empty = dir.join("empty.toml");
        std::fs::write(&empty, "").unwrap();
        let counting = |extra: &[&str], file: &Path| resolved_with(file, extra).count_tokens;
        assert!(counting(&[], &empty));
        assert!(!counting(&[], &config));
        assert!(counting(&["--set", "context.count_tokens=on"], &config));
        assert!(!counting(&["--set", "context.count_tokens=off"], &empty));
    }

    #[test]
    fn env_file_and_config_are_never_taken_from_the_current_directory() {
        let home = Some(PathBuf::from("/home/u"));
        for name in [wisp_protocol::KEYS_FILE, wisp_protocol::CONFIG_FILE] {
            let path = |explicit: Option<&Path>, xdg: Option<PathBuf>, home| {
                endpoints::config_path(name, explicit, xdg, home)
            };
            assert_eq!(
                path(None, None, home.clone()),
                Some(PathBuf::from(format!("/home/u/.config/wisp/{name}")))
            );
            assert_eq!(
                path(None, Some("/xdg".into()), home.clone()),
                Some(PathBuf::from(format!("/xdg/wisp/{name}")))
            );
            assert_eq!(
                path(None, Some("rel".into()), home.clone()),
                Some(PathBuf::from(format!("/home/u/.config/wisp/{name}")))
            );
            assert_eq!(path(None, Some("".into()), None), None);
            assert_eq!(
                path(Some(Path::new(name)), Some("/xdg".into()), home.clone()),
                Some(PathBuf::from(name))
            );
            assert_eq!(path(None, None, None), None);
        }
    }

    #[test]
    fn the_shown_file_names_match_the_lookup_and_the_flags() {
        use clap::CommandFactory;
        let home = PathBuf::from("/home/u");
        let args = Args::command();
        for (name, shown, given) in [
            (
                wisp_protocol::KEYS_FILE,
                wisp_protocol::DEFAULT_KEYS_FILE,
                wisp_protocol::GIVEN_ENV_FILE,
            ),
            (
                wisp_protocol::CONFIG_FILE,
                wisp_protocol::DEFAULT_CONFIG_FILE,
                wisp_protocol::GIVEN_CONFIG,
            ),
        ] {
            let path = endpoints::config_path(name, None, None, Some(home.clone())).unwrap();
            assert_eq!(wisp_protocol::shorten_path(&path, Some(&home)), shown);
            let (_, flag) = given.rsplit_once("--").unwrap();
            assert!(
                args.get_arguments().any(|a| a.get_long() == Some(flag)),
                "{given}"
            );
        }
    }

    fn book(env: &[(&str, &str)], config: Option<&str>) -> EndpointBook {
        let vars: std::collections::BTreeMap<String, String> = env
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        let config = config.map(|text| {
            wisp_core::endpoints::ConfigFile::parse(
                Path::new("/home/u/.config/wisp/config.toml"),
                wisp_core::endpoints::PathShown::Default,
                text,
            )
            .unwrap()
        });
        let env = |name: &str| vars.get(name).cloned();
        EndpointBook::resolve(
            config.as_ref(),
            &wisp_core::secrets::NoSecrets,
            &wisp_core::endpoints::Keychain::Absent(wisp_core::endpoints::Variables::new(
                &env, None, false,
            )),
            None,
        )
        .unwrap()
    }

    #[test]
    fn an_exported_key_over_a_configured_default_reaches_the_welcome_panel_as_a_note() {
        let config = "default = \"manifest\"\n[endpoints.manifest]\nauth = \"bearer\"\nkey_env = \"GW_TOKEN\"\nurl = \"https://gw.example.com\"\n";
        let exported = book(
            &[("ANTHROPIC_API_KEY", "sk-secret"), ("GW_TOKEN", "tok")],
            Some(config),
        );
        let note = "Using ANTHROPIC_API_KEY from the environment, not the default account \"manifest\" (--account manifest to use it)";
        assert_eq!(
            exported.overridden.as_ref().map(|o| o.note()).as_deref(),
            Some(note)
        );
        let facts = session_facts(&exported, chosen(&exported), &[], &[]);
        assert_eq!(facts.endpoint_note.as_deref(), Some(note));
        assert_eq!(facts.endpoint_name.as_deref(), Some("claude-api"));
        let plain = book(&[("ANTHROPIC_API_KEY", "sk-secret")], None);
        assert_eq!(
            session_facts(&plain, chosen(&plain), &[], &[]).endpoint_note,
            None
        );
    }

    #[test]
    fn an_api_key_is_reported_as_an_api_key() {
        let official = book(&[("ANTHROPIC_API_KEY", "sk-secret")], None);
        assert_eq!(
            endpoints::credential_kind(&official.active.credential),
            wisp_protocol::settings::CredentialKind::ApiKey
        );
    }

    fn chosen(book: &EndpointBook) -> tui::Chosen<'static> {
        tui::Chosen {
            model: "claude-sonnet-5-5",
            effort: "low",
            cache_ttl: CacheTtl::OneHour,
            endpoint: endpoints::reach(&book.active),
            max_tokens: 64_000,
            claude_usage: false,
        }
    }

    #[test]
    fn session_facts_hold_no_credential_value() {
        let url = "https://CANARYUSER:CANARYPASS@GW.example.com:8443/CANARYPATH?k=CANARYQUERY";
        let config = "default = \"gw\"\n[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW\"\nurl_env = \"GW_URL\"\n[endpoints.work]\nauth = \"api_key\"\nkey_env = \"WORK\"\n";
        for (env, config) in [
            (
                vec![
                    ("ANTHROPIC_API_KEY", "sk-ant-CANARYKEY0001"),
                    ("ANTHROPIC_BASE_URL", url),
                ],
                None,
            ),
            (
                vec![
                    ("ANTHROPIC_AUTH_TOKEN", "CANARYTOKEN0002"),
                    ("ANTHROPIC_BASE_URL", url),
                ],
                None,
            ),
            (
                vec![
                    ("GW", "CANARYTOKEN0003"),
                    ("GW_URL", url),
                    ("WORK", "sk-ant-CANARYKEY0004"),
                ],
                Some(config),
            ),
        ] {
            let book = book(&env, config);
            let opening = wisp_start::models::session_facts(
                &book,
                chosen(&book),
                endpoints::summaries(&book.entries, &book.marks, &[], None, &[]),
                &[],
            );
            let wire = serde_json::to_string(&opening).unwrap();
            let debug = format!("{opening:?}");
            let facts = wisp_render::chrome::Facts::from(opening);
            assert_eq!(facts.host.as_deref(), Some("gw.example.com:8443"));
            let shown = format!("{facts:?} {book:?} {debug} {wire} {:?}", facts.credential())
                .to_ascii_uppercase();
            for canary in [
                "CANARYKEY0001",
                "CANARYTOKEN0002",
                "CANARYTOKEN0003",
                "CANARYKEY0004",
                "CANARYUSER",
                "CANARYPASS",
                "CANARYPATH",
                "CANARYQUERY",
            ] {
                assert!(!shown.contains(canary), "{canary} in {shown}");
            }
        }
    }

    #[test]
    fn a_pricing_flag_prices_its_endpoint_for_one_run_only() {
        let config = "default = \"ok\"\n[endpoints.ok]\nauth = \"api_key\"\nkey_env = \"OK\"\n[endpoints.free]\nauth = \"api_key\"\nkey_env = \"OK\"\npricing = \"none\"\n";
        let flags = settings::flag_layer(
            Vec::new(),
            &[
                "endpoints.ok.pricing=none".to_owned(),
                "endpoints.free.pricing=list".to_owned(),
            ],
        )
        .unwrap();
        let priced = |book: &EndpointBook, name: &str| {
            endpoints::endpoint_facts(&book.entries, &book.marks, &[], &[])
                .into_iter()
                .find(|f| f.name == name)
                .unwrap()
                .priced
        };
        let plain = book(&[("OK", "k")], Some(config));
        assert!(priced(&plain, "ok"));
        assert!(!priced(&plain, "free"));
        let flagged = book(&[("OK", "k")], Some(config)).with_flags(&flags);
        assert!(!priced(&flagged, "ok"));
        assert!(priced(&flagged, "free"));
        let (ready, _) = endpoints::connections(&flagged.entries, None);
        let ok = ready.iter().find(|c| c.name == "ok").unwrap();
        assert!(!ok.priced);
    }

    #[test]
    fn unavailable_rows_carry_their_real_kind_and_reach() {
        let config = "[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW\"\nurl = \"https://gw.example.com\"\n[endpoints.direct]\nauth = \"api_key\"\nkey_env = \"DIRECT\"\n[endpoints.viavar]\nauth = \"bearer\"\nkey_env = \"VIA\"\nurl_env = \"VIA_URL\"\n[endpoints.ok]\nauth = \"api_key\"\nkey_env = \"OK\"\n";
        let book = book(&[("OK", "k")], Some(config));
        let facts = endpoints::endpoint_facts(&book.entries, &book.marks, &[], &[]);
        let row = |name: &str| facts.iter().find(|f| f.name == name).unwrap().clone();
        let gw = row("gw");
        assert!(gw.unavailable.is_some());
        assert_eq!(gw.credential, Some(wisp_tui::app::Credential::AuthToken));
        assert_eq!(gw.reach, Some(wisp_render::chrome::Endpoint::Custom));
        assert_eq!(gw.provider, "anthropic");
        let direct = row("direct");
        assert!(direct.unavailable.is_some());
        assert_eq!(direct.credential, Some(wisp_tui::app::Credential::ApiKey));
        assert_eq!(direct.reach, Some(wisp_render::chrome::Endpoint::Official));
        let via = row("viavar");
        assert!(via.unavailable.is_some());
        assert_eq!(via.reach, None);
    }

    #[test]
    fn an_endpoint_whose_provider_cannot_be_built_is_shown_unavailable() {
        let config = "default = \"ok\"\n[endpoints.bad]\nauth = \"api_key\"\nkey_env = \"BAD\"\n[endpoints.ok]\nauth = \"api_key\"\nkey_env = \"OK\"\n[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW\"\nurl = \"https://gw.example.com\"\n[endpoints.ctl]\nauth = \"api_key\"\nkey_env = \"CTL\"\n";
        let book = book(
            &[
                ("BAD", "sk-ant-k\u{e9}y"),
                ("OK", "k"),
                ("GW", "t"),
                ("CTL", "sk-ant-\u{7f}"),
            ],
            Some(config),
        );
        let (ready, mut refused) = endpoints::connections(&book.entries, None);
        assert_eq!(
            refused.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            ["ctl"],
            "a key that can't go in a header never reaches a connection"
        );
        assert!(ready.iter().any(|c| c.name == "bad"));
        refused.push((
            "bad".to_owned(),
            "BAD can't be sent: the HTTP client could not start".to_owned(),
        ));
        let facts = session_facts(&book, chosen(&book), &refused, &[]);
        let bad = facts.endpoint_facts("bad").unwrap();
        assert_eq!(
            bad.unavailable.as_deref(),
            Some("BAD can't be sent: the HTTP client could not start")
        );
        assert!(!bad.priced);
        assert_eq!(bad.credential, Some(wisp_tui::app::Credential::ApiKey));
        assert!(facts.endpoint_facts("ctl").unwrap().unavailable.is_some());
        assert_eq!(
            facts
                .available_endpoints()
                .iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>(),
            ["gw", "ok"]
        );
        let ok = facts.endpoint_facts("ok").unwrap();
        assert!(ok.unavailable.is_none() && ok.priced);
    }

    #[test]
    fn the_label_names_the_endpoint_only_when_there_is_a_choice() {
        let glyphs =
            wisp_render::theme::glyphs::Glyphs::of(wisp_render::theme::glyphs::GlyphSet::Unicode);
        let full = |book: &EndpointBook| {
            session_facts(book, chosen(book), &[], &[])
                .credential()
                .unwrap()
                .full(&glyphs)
        };
        let only_env = book(&[("ANTHROPIC_API_KEY", "k")], None);
        assert_eq!(full(&only_env), AccountKind::ClaudeApi.label());
        let with_config = book(
            &[("ANTHROPIC_API_KEY", "k")],
            Some("[endpoints.work]\nauth = \"api_key\"\nkey_env = \"WORK\"\n"),
        );
        assert_eq!(full(&with_config), AccountKind::ClaudeApi.label());
        let gw = "[endpoints.manifest]\nauth = \"bearer\"\nkey_env = \"GW\"\nurl = \"https://gw.example.com/a\"\n[endpoints.direct]\nauth = \"api_key\"\nkey_env = \"DIRECT\"\n";
        let one_named = book(&[("GW", "t")], Some(gw));
        assert_eq!(
            full(&one_named),
            "manifest \u{b7} Gateway \u{b7} gw.example.com"
        );
        let two = book(
            &[("GW", "t"), ("DIRECT", "k"), ("ANTHROPIC_API_KEY", "e")],
            Some(gw),
        );
        assert_eq!(
            full(&two),
            format!("claude-api \u{b7} {}", AccountKind::ClaudeApi.label())
        );
        let facts = session_facts(&two, chosen(&two), &[], &[]);
        assert_eq!(facts.endpoints.len(), 3);
        assert_eq!(facts.endpoint_note, None);
    }

    #[test]
    fn the_retry_budget_is_opt_in_and_a_dollar_amount() {
        assert_eq!(Args::try_parse_from(["wisp"]).unwrap().retry_budget, None);
        for (text, micros) in [("0.5", 500_000), ("$1.25", 1_250_000), ("0", 0)] {
            let args = Args::try_parse_from(["wisp", "--retry-budget", text]).unwrap();
            assert_eq!(
                args.retry_budget,
                Some(SettingValue::Usd { micros }),
                "{text}"
            );
        }
        assert_eq!(micros_to_usd(1_250_000), 1.25);
        for text in ["-1", "abc", "inf", "NaN", ""] {
            assert!(
                Args::try_parse_from(["wisp", "--retry-budget", text]).is_err(),
                "{text}"
            );
        }
    }

    #[test]
    fn wisp_account_is_wisp_endpoint() {
        for words in [
            ["wisp", "account", "add", "work"],
            ["wisp", "account", "edit", "work"],
        ] {
            let args = Args::try_parse_from(words).unwrap();
            assert!(matches!(args.command, Some(Command::Endpoint { .. })));
        }
    }

    #[test]
    fn pin_normalizes_new_and_existing_ids() {
        let scratch = scratch_dir("pin");
        let dir = scratch.dir("sessions");
        let id = "550e8400-e29b-41d4-a716-446655440000";
        let raw = "550E8400E29B41D4A716446655440000";
        let args = || Args::try_parse_from(["wisp", "--session-id", raw]).unwrap();

        let mut fresh = args();
        pin(&mut fresh, Some(&dir)).unwrap();
        assert_eq!(fresh.session_id.as_deref(), Some(OsStr::new(id)));
        assert_eq!(fresh.resume, None);

        JsonlStore::open(&dir).unwrap().create_session(id).unwrap();
        let mut existing = args();
        pin(&mut existing, Some(&dir)).unwrap();
        assert_eq!(existing.session_id.as_deref(), Some(OsStr::new(id)));
        assert_eq!(existing.resume, Some(Some(id.to_owned())));
    }

    #[cfg(unix)]
    #[test]
    fn pin_refuses_non_utf8_without_repeating_it() {
        use std::os::unix::ffi::OsStrExt;

        let raw = OsStr::from_bytes(b"CANARY_non_utf8_\xff");
        let mut args =
            Args::try_parse_from([OsStr::new("wisp"), OsStr::new("--session-id"), raw]).unwrap();
        let error = pin(&mut args, None).unwrap_err();
        assert!(!error.contains("CANARY"));
    }

    #[test]
    fn no_log_flag_writes_nothing() {
        use wisp_core::kit::{JsonlLogs, OpenLog, ROOT};

        let open_log = |on, dir| {
            JsonlLogs {
                on,
                dir,
                root_id: None,
            }
            .open(ROOT)
        };
        let args = Args::try_parse_from(["wisp", "--no-log"]).unwrap();
        assert!(args.no_log);
        assert!(!Args::try_parse_from(["wisp"]).unwrap().no_log);

        let dir = std::env::temp_dir().join(format!("wisp-nolog-{}", new_session_id()));
        assert!(open_log(false, Some(dir.clone())).unwrap().is_none());
        assert!(!dir.exists(), "--no-log must not create the log directory");

        let opened = open_log(true, Some(dir.clone())).unwrap().unwrap();
        assert_eq!(opened.path.parent(), Some(dir.as_path()));
        assert!(opened.path.to_string_lossy().ends_with(".jsonl"));
        assert_eq!(opened.unlocked, None);
        assert!(opened.path.exists());
        let id = opened.log.id().to_owned();
        assert_eq!(
            JsonlStore::open(&dir)
                .unwrap()
                .open_session(&id)
                .unwrap_err()
                .to_string(),
            format!(
                "session {} is open in another Wisp",
                wisp_protocol::short_id(&id)
            )
        );
        drop(opened);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            unlocked_warning("No locks available (os error 37)"),
            "wisp: the session log's filesystem doesn't support file locks (No locks available (os error 37)), so this session runs unlocked"
        );

        assert!(open_log(true, None).is_err());
        let file = std::env::temp_dir().join(format!("wisp-nolog-file-{}", new_session_id()));
        std::fs::write(&file, "").unwrap();
        assert!(open_log(true, Some(file.join("sessions"))).is_err());
        std::fs::remove_file(&file).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn the_tools_section_keeps_its_one_cell_rows_as_they_were() {
        let scratch = tempfile::tempdir().unwrap();
        let workspace = wisp_core::plugin::DoctorWorkspace {
            cwd: scratch.path().to_owned(),
            plugin_data: Some(scratch.path().join("plugin-data")),
        };
        let mut out = Vec::new();
        let redactions = wisp_core::session::Redactions::default();
        tools_doctor(
            &mut out,
            wisp_core::mcp::Service::default(),
            &redactions,
            &workspace,
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        let mut expected = String::from("\ntools\n");
        for row in tools_rows(wisp_core::mcp::Service::default(), &redactions, &workspace).unwrap()
        {
            let [wisp_protocol::DoctorCell::Text { text }] = row.cells.as_slice() else {
                panic!("{} has more than one cell", row.key);
            };
            let text = wisp_tui::term::doctor::clean(text);
            expected.push_str(&format!("  {:<18} {text}\n", row.key));
        }
        if cfg!(any(target_os = "linux", target_os = "macos")) {
            assert!(text.starts_with("\ntools\n  bash  "), "{text}");
        } else {
            assert!(text.starts_with("\ntools\n  memory"), "{text}");
        }
        assert_eq!(text, expected);
    }

    #[test]
    fn the_help_lists_account_and_not_its_hidden_alias() {
        use clap::CommandFactory;
        let help = Args::command().render_long_help().to_string();
        let commands: Vec<&str> = help
            .lines()
            .skip_while(|line| !line.starts_with("Commands:"))
            .take_while(|line| !line.is_empty())
            .collect();
        let commands = commands.join("\n");
        assert!(
            commands
                .lines()
                .any(|line| line.trim_start().starts_with("account ")),
            "{commands}"
        );
        assert!(!commands.contains("endpoint"), "{commands}");
        assert_eq!(
            wisp_render::chrome::account::vocabulary_slips(&commands),
            Vec::<&str>::new(),
            "{commands}"
        );
        assert!(Args::try_parse_from(["wisp", "endpoint", "list"]).is_ok());
        let flag = "Start on this account. WISP_ACCOUNT=NAME does the same";
        assert!(help.contains(flag), "{help}");
        assert!(
            commands.contains("quit     Quit chats by id, without opening them"),
            "{commands}"
        );
        let quit = Args::try_parse_from(["wisp", "quit", "--help"])
            .unwrap_err()
            .to_string();
        assert!(
            quit.contains("Usage: wisp quit [OPTIONS] <ID>..."),
            "{quit}"
        );
        assert!(quit.contains("A chat's id, or the start of one"), "{quit}");
        assert!(Args::try_parse_from(["wisp", "quit"]).is_err());
        assert!(
            commands.contains("daemon   Show the chats kept open, and start or stop Wisp's daemon")
        );
        assert!(commands.find("quit ").unwrap() < commands.find("daemon ").unwrap());
        assert!(
            commands.contains(
                "restart  Restart the chats kept open on the newest version, all or by id\n"
            ),
            "{commands}"
        );
        let restart = Args::try_parse_from(["wisp", "restart", "--help"])
            .unwrap_err()
            .to_string();
        assert!(
            restart.contains("Usage: wisp restart [OPTIONS] [ID]..."),
            "{restart}"
        );
        assert!(
            restart.contains("A chat's id, or the start of one"),
            "{restart}"
        );
        assert!(!help.contains("--restarted"));
        assert!(
            Args::try_parse_from(["wisp", "--restarted"])
                .unwrap()
                .restarted
        );
        assert!(matches!(
            Args::try_parse_from(["wisp", "restart"]).unwrap().command,
            Some(Command::Restart { ids }) if ids.is_empty()
        ));
        assert!(matches!(
            Args::try_parse_from(["wisp", "restart", "0123", "4567"]).unwrap().command,
            Some(Command::Restart { ids }) if ids == ["0123", "4567"]
        ));
        let daemon = Args::try_parse_from(["wisp", "daemon", "--help"])
            .unwrap_err()
            .to_string();
        assert!(daemon.contains("Usage: wisp daemon [OPTIONS] [COMMAND]"));
        assert!(daemon.contains("Start Wisp's daemon"));
        assert!(daemon.contains("Stop Wisp's daemon; chats stay open"));
        assert!(
            !daemon
                .lines()
                .any(|line| line.trim_start().starts_with("serve "))
        );
        assert!(matches!(
            Args::try_parse_from(["wisp", "daemon"]).unwrap().command,
            Some(Command::Daemon { command: None })
        ));
        for subcommand in ["start", "stop", "serve"] {
            assert!(Args::try_parse_from(["wisp", "daemon", subcommand]).is_ok());
        }

        assert!(
            matches!(Args::try_parse_from(["wisp", "quit", "01ab", "2345"]).unwrap().command, Some(Command::Quit { ids }) if ids == ["01ab", "2345"])
        );
    }

    #[test]
    fn a_hook_cycle_breaks_doctor_and_a_missing_requirement_only_shows() {
        use wisp_plugin_testkit::hooks as kit;
        let report = |sources: Vec<(&str, String)>| {
            let sources = sources
                .into_iter()
                .map(|(id, body)| {
                    let make: wisp_core::plugin::Make =
                        kit::instantiate(kit::none(), kit::Calls::default());
                    (kit::manifest(id, &body), make)
                })
                .collect();
            let (registry, _) = wisp_core::plugin::Registry::from_sources(
                sources,
                &wisp_core::plugin::PluginConfig::default(),
            );
            crate::hooks_report(&registry)
        };
        let hook = |name: &str, extra: &str| {
            format!("\n[[hooks]]\nname = \"{name}\"\npoint = \"after-turn\"\n{extra}")
        };
        let (rows, broken) = report(vec![
            ("aa", hook("x", "after = [\"bb/y\"]\n")),
            ("bb", hook("y", "after = [\"aa/x\"]\n")),
        ]);
        assert!(broken, "{rows:?}");
        assert!(!rows.is_empty());
        let (rows, broken) = report(vec![("aa", hook("x", "requires = [\"nope/q\"]\n"))]);
        assert!(!broken, "{rows:?}");
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(crate::rows_text("hooks", &rows).starts_with("\nhooks\n"));
    }

    #[test]
    fn a_section_cycle_breaks_doctor_and_an_unknown_name_only_shows() {
        use wisp_plugin_testkit::hooks as kit;
        let report = |sources: Vec<(&str, String)>| {
            let sources = sources
                .into_iter()
                .map(|(id, body)| {
                    let make: wisp_core::plugin::Make =
                        kit::sections(kit::none(), kit::views(), kit::Calls::default());
                    (kit::slotted(id, &body), make)
                })
                .collect();
            let (registry, _) = wisp_core::plugin::Registry::from_sources(
                sources,
                &wisp_core::plugin::PluginConfig::default(),
            );
            crate::hooks_report(&registry)
        };
        let (rows, broken) = report(vec![
            ("aa", kit::section("xx", "after = [\"bb/yy\"]\n")),
            ("bb", kit::section("yy", "after = [\"aa/xx\"]\n")),
        ]);
        assert!(broken, "{rows:?}");
        assert!(rows.iter().any(|(plugin, _)| plugin == "aa"), "{rows:?}");
        let (rows, broken) = report(vec![("aa", kit::section("xx", "after = [\"nope/qq\"]\n"))]);
        assert!(!broken, "{rows:?}");
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(
            crate::rows_text("hooks", &rows).contains("nope/qq"),
            "{rows:?}"
        );
    }
}
