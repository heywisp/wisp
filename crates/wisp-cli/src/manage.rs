use std::collections::BTreeSet;
use std::io::{BufRead, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::builder::TypedValueParser;
use clap::{Args, Subcommand};
use wisp_core::config_file::{ConfigText, NewEndpoint, NewTable, WriteProblem, canonical_config};
use wisp_core::endpoints::{
    AuthKind, CHECK_TIMEOUT, CheckOutcome, ConfigFile, EndpointName, GIVEN_CONFIG, PathShown,
    PricingMode, ProviderKind, ResolvedEndpoint, UrlSource, http_url,
};
use wisp_core::provider::credential::{EnvFile, GIVEN_CREDENTIALS, Source};
use wisp_core::secrets::import::{Place, read_var};
use wisp_core::secrets::{
    Applied, Change, ChangeError, Failed, FileStore, KeyringAccess, KeyringError, LOCK_WAIT,
    Outcome, Secret, StoreKind, StoredRecord, VariableName, cant_detach_words, check_key,
    keychain_off_for_flag,
};
use wisp_protocol::endpoint::{StoreChoice, loopback_host};
use wisp_protocol::{IN_KEYCHAIN, VAR_NAME_CHARS};
use wisp_render::chrome::account::AccountKind;
use wisp_render::chrome::endpoint_card::{
    AddAnswer, AddQuestion, FORBIDDEN_NOTE, KEY_REFUSED, NOT_HTTP, NOT_OFFERED_NOTE, PASTE_AGAIN,
    THIS_URL, check_failed, forbidden_ask, keychain_question, not_offered_ask, plain_http,
    plain_http_risk, remove_question,
};
use wisp_render::transcript::copy::{REMOVE_KEEP, YES_NO};
use wisp_tui::term::print::Stream;
use wisp_tui::term::prompt::{CANCEL_EXIT, PromptError, Prompter, lead_lines};

use crate::auth::login::{Row, numbered_text, rows_text};
use crate::endpoints::{self, Owners, note_words, short};
use crate::usage;
pub(crate) use wisp_start::settings::KEY_ON_COMMAND_LINE;

pub(crate) fn store_kinds() -> impl TypedValueParser<Value = StoreKind> {
    usage::choices(&StoreKind::ALL, StoreKind::as_str)
}

pub(crate) fn store_words(kind: StoreKind, file: &str) -> String {
    match kind {
        StoreKind::Keyring => IN_KEYCHAIN.to_owned(),
        StoreKind::File => format!("{file}, readable only by you"),
    }
}

pub(crate) enum Picked {
    Kind(StoreKind),
    Auto,
}

pub(crate) fn pick_store_and_note(
    choice: Option<StoreChoice>,
    keychain: &crate::keychain::Keychain,
    file: &str,
) -> Result<(Picked, Option<String>), String> {
    match choice.and_then(StoreKind::chosen) {
        Some(StoreKind::Keyring) => match keychain.off_error() {
            Some(KeyringError::Off { origin }) => Err(origin),
            _ => Ok((Picked::Kind(StoreKind::Keyring), None)),
        },
        Some(kind) => Ok((Picked::Kind(kind), None)),
        None if keychain.detected().available() => Ok((Picked::Auto, None)),
        None => Ok((Picked::Kind(StoreKind::File), keychain.absent_note(file))),
    }
}

pub(crate) fn pick_store(
    choice: Option<StoreChoice>,
    ctx: &Context<'_>,
    file: &str,
) -> Result<Picked, String> {
    let (picked, note) = pick_store_and_note(choice, ctx.keychain, file)
        .map_err(|origin| keychain_off_for_flag(&origin))?;
    if let Some(note) = note {
        eprintln!("{note}");
    }
    Ok(picked)
}

pub(crate) fn keychain_failed_stop(error: &KeyringError, file: &str, rest: bool) -> String {
    let which = error
        .which()
        .unwrap_or_else(|| host::REFUSED_REQUEST.to_owned());
    let (unsaved, keys) = if rest {
        ("the rest weren't saved", "the keys")
    } else {
        ("nothing was saved", "the key")
    };
    format!(
        "the keychain {which}, so {unsaved}; unlock it and try again, or pass --store file to keep {keys} in {file}"
    )
}

fn auths() -> impl TypedValueParser<Value = AuthKind> {
    usage::choices(&AuthKind::ALL, AuthKind::as_str)
}

pub(crate) fn providers() -> impl TypedValueParser<Value = ProviderKind> {
    usage::choices(&ProviderKind::ALL, ProviderKind::as_str)
}

pub(crate) fn stores() -> impl TypedValueParser<Value = StoreChoice> {
    usage::choices(&StoreChoice::ALL, StoreChoice::as_str)
}

fn pricings() -> impl TypedValueParser<Value = PricingMode> {
    usage::choices(&PricingMode::ALL, PricingMode::as_str)
}

#[cfg_attr(test, derive(Debug))]
#[derive(Subcommand)]
pub(crate) enum EndpointCommand {
    #[command(
        about = "Add an account; asks for anything not given as a flag, and reads the key hidden or from stdin"
    )]
    Add(Box<AddArgs>),
    #[command(about = "List your accounts and what needs you")]
    List { name: Option<String> },
    #[command(
        about = "List models available to a named OpenAI, ChatGPT, Gemini or Antigravity connection"
    )]
    Models { name: String },
    #[command(about = "Remove an account and delete its stored key")]
    Remove {
        #[arg(value_name = "NAME", help = "The account to remove")]
        name: String,
        #[arg(long, help = "Don't ask first")]
        yes: bool,
        #[arg(
            long,
            help = "For a key in the keychain: remove only the account and leave its keychain item, whether or not the keychain answers"
        )]
        keep_key: bool,
    },
    #[command(about = "Start on this account from now on")]
    Default {
        #[arg(value_name = "NAME", help = "The account to start on")]
        name: String,
    },
    #[command(
        about = "Change an account: its key, URL, auth, pricing or name; asks which field without flags on a terminal"
    )]
    Edit(Box<edit::EditArgs>),
    #[command(
        about = concat!("Store keys found in ", wisp_protocol::config_dir!(), wisp_protocol::keys_file!(), " (or ", wisp_protocol::given_env_file!(), ") or, with --from-environment, in the environment")
    )]
    Import(import::ImportArgs),
}

#[cfg_attr(test, derive(Debug))]
#[derive(Args)]
pub(crate) struct AddArgs {
    #[arg(
        value_name = "NAME",
        help = name_help()
    )]
    name: Option<String>,
    #[arg(long, value_name = "PROVIDER", value_parser = providers(), help = "The provider: anthropic, openai or gemini")]
    provider: Option<ProviderKind>,
    #[arg(
        long,
        value_name = "URL",
        help = "A gateway or proxy URL instead of the provider's own API. Other programs can see the command line, so a URL holding a token is better typed at the prompt"
    )]
    url: Option<String>,
    #[arg(long, value_name = "AUTH", value_parser = auths(), help = "How the key is sent: api_key (x-api-key header) or bearer (Authorization: Bearer header)")]
    auth: Option<AuthKind>,
    #[arg(long, value_name = "STORE", value_parser = stores(), help = concat!("Where the key is kept: auto (the default: the keychain where this machine has one, else the file), keyring (the system keychain) or file (", wisp_protocol::credentials_beside_config!(), ", readable only by you)"))]
    store: Option<StoreChoice>,
    #[arg(long, value_name = "PRICING", value_parser = pricings(), help = "list (estimates at list prices, the default) or none (tokens only)")]
    pricing: Option<PricingMode>,
    #[arg(long, help = "Make it the default account")]
    default: bool,
    #[arg(
        long,
        help = "Save without the count_tokens check (the local key check still runs)"
    )]
    no_check: bool,
    #[arg(
        long,
        help = "Read the key from stdin: one line, never from a terminal (printenv VAR | wisp account add NAME --key-stdin)"
    )]
    key_stdin: bool,
    #[arg(
        long,
        value_name = "VAR",
        help = concat!("Read the key once from this variable, in ", wisp_protocol::given_env_file!(), " or, with --from-environment, in the environment")
    )]
    key_var: Option<String>,
    #[arg(
        long,
        value_name = "VAR",
        help = "Read the URL once from this variable, from the same source as --key-var"
    )]
    url_var: Option<String>,
    #[arg(
        long,
        conflicts_with_all = ["key_env", "url_env"],
        help = "With --key-var or --url-var, read the variables from the environment instead of a file"
    )]
    from_environment: bool,
    #[arg(
        long,
        value_name = "VAR",
        help = "Don't store a key: name the variable that holds it, read at each start where no keychain is used (nothing is read now)"
    )]
    key_env: Option<String>,
    #[arg(
        long,
        value_name = "VAR",
        help = "With --key-env, name the variable that holds the URL"
    )]
    url_env: Option<String>,
    #[arg(long, hide = true, value_name = "KEY")]
    key: Option<String>,
    #[arg(long, hide = true, value_name = "KEY")]
    api_key: Option<String>,
    #[arg(long, hide = true, value_name = "KEY")]
    token: Option<String>,
}

pub(crate) const VARIABLE_MAX: usize = 128;

pub(crate) fn given_variable(text: &str, flag: &str) -> Result<VariableName, String> {
    VariableName::parse(text)
        .filter(|_| text.len() <= VARIABLE_MAX)
        .ok_or_else(|| {
            format!(
                "{flag} needs a variable name ({VAR_NAME_CHARS}); what was given isn't shown, in case it is a key"
            )
        })
}

pub(crate) fn named_variable(text: &str, flag: &str) -> Result<VariableName, String> {
    let name = given_variable(text, flag)?;
    if name.looks_like_key() {
        return Err(format!(
            "{flag} needs a variable name such as WORK_API_KEY; what was given looks like a key, so it wasn't saved or shown"
        ));
    }
    Ok(name)
}

fn parse_variable(
    text: Option<&str>,
    flag: &str,
    rule: fn(&str, &str) -> Result<VariableName, String>,
) -> Result<Option<VariableName>, String> {
    text.map(|t| rule(t, flag)).transpose()
}

pub(crate) enum Origin {
    Environment,
    File(EnvFile),
}

pub(crate) struct VarSource {
    pub(crate) origin: Origin,
    pub(crate) words: String,
}

impl VarSource {
    pub(crate) fn place(&self) -> Place<'_> {
        match &self.origin {
            Origin::Environment => {
                wisp_core::secrets::import::Place::Environment(&wisp_start::paths::real_env)
            }
            Origin::File(file) => Place::File(file),
        }
    }

    fn read(&self, name: &VariableName) -> Option<String> {
        read_var(self.place(), name)
    }
}

pub(crate) const BOTH_SOURCES: &str = "--env-file and --from-environment are two sources; give one";

pub(crate) fn var_source(
    ctx: &Context<'_>,
    from_environment: bool,
) -> Result<Option<VarSource>, String> {
    match (ctx.env_file, from_environment) {
        (Some(_), true) => Err(BOTH_SOURCES.to_owned()),
        (None, true) => Ok(Some(VarSource {
            origin: Origin::Environment,
            words: "the environment".to_owned(),
        })),
        (Some(path), false) => match wisp_core::secrets::import::read_file(path, true)? {
            Some(file) => Ok(Some(VarSource {
                words: wisp_core::workspace::home_shown(&file.path),
                origin: Origin::File(file),
            })),
            None => Err(wisp_core::secrets::missing_env_file()),
        },
        (None, false) => Ok(None),
    }
}

#[derive(Clone, Copy)]
pub(crate) enum NothingWas {
    Saved,
    Changed,
}

impl NothingWas {
    fn words(self) -> &'static str {
        match self {
            Self::Saved => "saved",
            Self::Changed => "changed",
        }
    }
}

pub(crate) fn key_from_var(
    source: &VarSource,
    name: &VariableName,
    outcome: NothingWas,
) -> Result<String, String> {
    let done = outcome.words();
    let Some(value) = source.read(name) else {
        return Err(format!(
            "the variable given to --key-var isn't set in {}; nothing was {done}",
            source.words
        ));
    };
    match check_key(&value) {
        Ok(key) => Ok(key.to_owned()),
        Err(problem) => Err(format!(
            "the variable given to --key-var: {problem} Nothing was {done}."
        )),
    }
}

pub(crate) fn url_from_var(
    source: &VarSource,
    name: &VariableName,
    outcome: NothingWas,
) -> Result<String, String> {
    let done = outcome.words();
    match source.read(name) {
        Some(url) if http_url(&url).is_some() => Ok(url),
        Some(_) => Err(format!(
            "the variable given to --url-var isn't an http or https URL; nothing was {done}"
        )),
        None => Err(format!(
            "the variable given to --url-var isn't set in {}; nothing was {done}",
            source.words
        )),
    }
}

pub(crate) struct Context<'a> {
    pub(crate) env_file: Option<&'a Path>,
    pub(crate) config: Option<&'a Path>,
    pub(crate) model: &'a str,
    pub(crate) caps: wisp_tui::term::Caps,
    pub(crate) claude_usage: bool,
    pub(crate) xdg_config_home: Option<PathBuf>,
    pub(crate) home: Option<PathBuf>,
    pub(crate) keychain: &'a crate::keychain::Keychain,
    pub(crate) models: Option<PathBuf>,
}

impl Context<'_> {
    pub(crate) fn sources(&self) -> endpoints::Sources {
        let flags = endpoints::Flags {
            env_file: self.env_file,
            config: self.config,
            endpoint: None,
            keychain: self.keychain,
            set: &[],
        };
        endpoints::Sources::load(&flags, self.xdg_config_home.clone(), self.home.clone())
    }

    pub(crate) fn prompter(&self, cancel: &str) -> Prompter {
        Prompter::new(cancel, self.caps.clone())
    }

    pub(crate) fn home_text(&self) -> Option<String> {
        self.home.as_ref().map(|h| h.display().to_string())
    }

    pub(crate) fn target(&self, must_exist: bool) -> Result<(PathBuf, PathShown), String> {
        match self.config {
            Some(path) => {
                let missing = std::fs::symlink_metadata(path).is_err();
                let folder = path.parent().filter(|p| !p.as_os_str().is_empty());
                if folder.is_some_and(|f| !f.is_dir()) || (must_exist && missing) {
                    return Err(endpoints::missing_given(path));
                }
                Ok((path.to_owned(), PathShown::Given))
            }
            None => endpoints::config_path(
                wisp_protocol::CONFIG_FILE,
                None,
                self.xdg_config_home.clone(),
                self.home.clone(),
            )
            .map(|p| (p, PathShown::Default))
            .ok_or_else(|| {
                "no home directory: set HOME or XDG_CONFIG_HOME, or pass --config".to_owned()
            }),
        }
    }

    fn default_config(&self) -> (Option<PathBuf>, Option<PathBuf>) {
        let default = endpoints::config_path(
            wisp_protocol::CONFIG_FILE,
            None,
            self.xdg_config_home.clone(),
            self.home.clone(),
        );
        let canonical = default
            .as_deref()
            .and_then(|p| std::fs::canonicalize(p).ok());
        (canonical, default)
    }

    fn ignore_env_file(&self, command: &str) {
        if self.env_file.is_some() {
            eprintln!("--env-file isn't used by wisp account {command}; ignoring it");
        }
    }

    fn env_file_path(&self) -> Option<PathBuf> {
        endpoints::config_path(
            wisp_protocol::KEYS_FILE,
            None,
            self.xdg_config_home.clone(),
            self.home.clone(),
        )
    }
}

pub(crate) fn fail(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("wisp: {message}");
    ExitCode::FAILURE
}

fn usage_fail(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("wisp: {message}");
    ExitCode::from(usage::USAGE_EXIT)
}

fn declined() -> ExitCode {
    eprintln!("Nothing was saved.");
    ExitCode::FAILURE
}

pub(crate) struct Flow {
    cancel: &'static str,
    declined: &'static str,
    setup: bool,
    signing_in: bool,
}

impl Flow {
    pub(crate) fn cancel(&self) -> &'static str {
        self.cancel
    }

    fn declined(&self) -> ExitCode {
        eprintln!("{}", self.declined);
        ExitCode::FAILURE
    }
}

const ADD: Flow = Flow {
    cancel: "Cancelled; nothing was saved.",
    declined: "Nothing was saved.",
    setup: false,
    signing_in: false,
};

pub(crate) const EDIT: Flow = ADD;

const SIGN_IN: Flow = Flow {
    cancel: crate::auth::login::CANCELLED,
    declined: "Nothing was saved.",
    setup: false,
    signing_in: true,
};

pub(crate) const SETUP: Flow = Flow {
    cancel: "Setup cancelled; nothing was saved. Run wisp setup to try again.",
    declined: "Nothing was saved. Run wisp setup to try again.",
    setup: true,
    signing_in: false,
};

pub(crate) fn add_signing_in(
    ctx: &Context<'_>,
    name: Option<String>,
    provider: Option<ProviderKind>,
) -> ExitCode {
    add(
        &AddArgs {
            provider,
            ..asking(name, false)
        },
        ctx,
        &SIGN_IN,
    )
}

pub(crate) fn add_asking(ctx: &Context<'_>, make_default: bool, flow: &Flow) -> ExitCode {
    add(&asking(None, make_default), ctx, flow)
}

fn asking(name: Option<String>, make_default: bool) -> AddArgs {
    AddArgs {
        name,
        provider: None,
        url: None,
        auth: None,
        store: None,
        pricing: None,
        default: make_default,
        no_check: false,
        key_stdin: false,
        key_var: None,
        url_var: None,
        from_environment: false,
        key_env: None,
        url_env: None,
        key: None,
        api_key: None,
        token: None,
    }
}

pub(crate) fn cancelled(prompter: &Prompter) -> ExitCode {
    eprintln!("{}", prompter.cancel_message());
    ExitCode::from(CANCEL_EXIT)
}

fn name_help() -> String {
    format!(
        "A label for the account: {}. It is shown in lists, so never a key",
        wisp_protocol::endpoint_name_rule()
    )
}

fn parse_name(text: &str) -> Result<EndpointName, String> {
    parse_name_for(text, &wisp_core::endpoints::host::reserved_name())
}

fn parse_name_for(text: &str, env: &str) -> Result<EndpointName, String> {
    match EndpointName::parse(text) {
        Some(name) if name.is_env() => Err(env.to_owned()),
        Some(name) => Ok(name),
        None => Err(wisp_protocol::bad_account_name()),
    }
}

pub(crate) fn run(command: &EndpointCommand, ctx: &Context<'_>) -> ExitCode {
    match command {
        EndpointCommand::Models { name } => crate::auth::models(ctx, name),
        EndpointCommand::Add(args) => add(args, ctx, &ADD),
        EndpointCommand::List { name } => list(ctx, name.as_deref()),
        EndpointCommand::Remove {
            name,
            yes,
            keep_key,
        } => remove(name, *yes, *keep_key, ctx),
        EndpointCommand::Default { name } => set_default(name, ctx),
        EndpointCommand::Edit(args) => edit::run(args, ctx),
        EndpointCommand::Import(args) => import::run(args, ctx),
    }
}

pub(crate) fn credentials_shown(ctx: &Context<'_>) -> String {
    let home = ctx.home_text();
    match ctx.target(false) {
        Ok((path, shown)) => credentials_words(&FileStore::beside(&path, shown), home.as_deref()),
        Err(_) => wisp_core::secrets::CREDENTIALS.to_owned(),
    }
}

fn credentials_words(store: &FileStore, home: Option<&str>) -> String {
    match store.shown() {
        PathShown::Default => short(store.path(), home),
        PathShown::Given => GIVEN_CREDENTIALS.to_owned(),
    }
}

pub(crate) fn change_error(error: &ChangeError, home: Option<&str>) -> String {
    match error {
        ChangeError::Config(e) => match &e.problem {
            WriteProblem::TargetNotWritable(note) => match note.shown {
                PathShown::Default => format!(
                    "{} \u{2192} {}: Wisp can't write that folder; nothing was changed",
                    short(&e.path, home),
                    short(&note.path, home)
                ),
                PathShown::Given => format!("{}; nothing was changed", note_words(note, home)),
            },
            _ => e.to_string(),
        },
        ChangeError::CantDetach {
            unreadable,
            credentials,
            shown,
        } => {
            let file = match shown {
                PathShown::Default => short(credentials, home),
                PathShown::Given => GIVEN_CREDENTIALS.to_owned(),
            };
            cant_detach_words(unreadable, &file)
        }
        other => other.to_string(),
    }
}

fn report(applied: &Applied, ctx: &Context<'_>, path: &Path) -> ExitCode {
    let home = ctx.home_text();
    let (default, default_shown) = ctx.default_config();
    let owners = Owners {
        this: canonical_config(path),
        default,
        default_shown: default_shown.as_deref(),
        home: home.as_deref(),
    };
    if let Some(note) = &applied.detached {
        println!("{}", owners.detach_words(note));
    }
    for note in &applied.notes {
        eprintln!("note: {}", note_words(note, home.as_deref()));
    }
    for left in &applied.left {
        let wisp_core::secrets::StoreError::Keychain(error) = &left.error else {
            eprintln!(
                "wisp: the old file-store record for {} wasn't deleted: {}; it still holds the key in {}",
                left.name,
                left.error,
                wisp_core::secrets::CREDENTIALS
            );
            continue;
        };
        let which = error
            .which()
            .unwrap_or_else(|| host::REFUSED_REQUEST.to_owned());
        eprintln!(
            "wisp: the keychain {which}, so the old keychain item for {} wasn't deleted (account {}); it still holds the key, so delete that item with your keychain app",
            left.name, left.account
        );
    }
    if applied.left.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

struct Draft {
    provider: ProviderKind,
    url: Option<String>,
    auth: AuthKind,
    key: Option<String>,
}

pub(crate) enum Asked<T> {
    Answer(T),
    Cancelled,
    Declined,
    Failed(String),
}

impl<T> From<PromptError> for Asked<T> {
    fn from(e: PromptError) -> Self {
        match e {
            PromptError::Cancelled => Self::Cancelled,
            PromptError::Io(e) => Self::Failed(format!("could not read the terminal: {e}")),
        }
    }
}

impl<T> Asked<T> {
    pub(crate) fn and_then<U>(self, then: impl FnOnce(T) -> Asked<U>) -> Asked<U> {
        match self {
            Self::Answer(answer) => then(answer),
            Self::Cancelled => Asked::Cancelled,
            Self::Declined => Asked::Declined,
            Self::Failed(why) => Asked::Failed(why),
        }
    }
}

macro_rules! ask {
    ($e:expr) => {
        match $e {
            Ok(value) => value,
            Err(e) => return Asked::from(e),
        }
    };
}

pub(crate) fn label_rows(labels: &[&str]) -> Vec<Row> {
    labels
        .iter()
        .map(|label| Row {
            word: String::new(),
            mark: None,
            label: (*label).to_owned(),
            detail: String::new(),
            keep_detail: false,
        })
        .collect()
}

pub(crate) fn choose(
    prompter: &Prompter,
    lead: &str,
    options: &[&str],
    start: usize,
) -> Asked<usize> {
    let rows = label_rows(options);
    let count = rows.len();
    let start = Some(start);
    Asked::Answer(ask!(pick_row(
        prompter,
        lead,
        &rows,
        start,
        &choose_prompt(count, start),
        |answer| picked(answer, count, start),
    )))
}

fn choose_add(
    prompter: &Prompter,
    lead: &str,
    question: AddQuestion,
    gateway: bool,
) -> Asked<AddAnswer> {
    let (answers, start) = question.answers(gateway);
    let labels: Vec<&str> = answers.iter().copied().map(AddAnswer::label).collect();
    choose(prompter, lead, &labels, start).and_then(|row| match answers.get(row) {
        Some(answer) => Asked::Answer(*answer),
        None => Asked::Declined,
    })
}

pub(crate) fn pick_row(
    prompter: &Prompter,
    lead: &str,
    rows: &[Row],
    start: Option<usize>,
    prompt: &str,
    answer: impl Fn(&str) -> Option<usize>,
) -> Result<usize, PromptError> {
    let glyphs = prompter.glyphs();
    let layout = |width: usize| rows_text(rows, width, &glyphs);
    let columns = wisp_tui::term::prompt::terminal_columns();
    let listed: usize = layout(columns).iter().map(|(lines, _)| lines.len()).sum();
    if prompter.fits(lead_lines(lead, columns, &glyphs).len() + listed) {
        return prompter.select(lead, &layout, start);
    }
    for line in lead.lines() {
        eprintln!("{line}");
    }
    eprint!("{}", numbered_text(rows, columns, &glyphs));
    pick(prompter, prompt, rows.len(), answer)
}

pub(crate) fn pick(
    prompter: &Prompter,
    prompt: &str,
    count: usize,
    answer: impl Fn(&str) -> Option<usize>,
) -> Result<usize, PromptError> {
    loop {
        let line = prompter.visible(prompt)?;
        let line = line.trim();
        match answer(line) {
            Some(index) => return Ok(index),
            None if line.is_empty() => {}
            None => eprintln!("Choose 1 to {count}."),
        }
    }
}

pub(crate) fn choose_prompt(count: usize, start: Option<usize>) -> String {
    match (start, count) {
        (Some(row), _) => format!("Choose [{}]: ", row + 1),
        (None, 1) => "Choose 1: ".to_owned(),
        (None, count) => format!("Choose 1-{count}: "),
    }
}

pub(crate) fn picked(answer: &str, count: usize, start: Option<usize>) -> Option<usize> {
    if answer.is_empty() {
        return start;
    }
    answer
        .parse::<usize>()
        .ok()
        .filter(|n| (1..=count).contains(n))
        .map(|n| n - 1)
}

pub(crate) fn confirm(prompter: &Prompter, question: &str, default_yes: bool) -> Asked<bool> {
    choose(prompter, question, &YES_NO, usize::from(!default_yes))
        .and_then(|row| Asked::Answer(row == 0))
}

fn ask_url(prompter: &Prompter) -> Asked<String> {
    loop {
        let answer = ask!(prompter.visible("Base URL: "));
        let answer = answer.trim();
        if http_url(answer).is_some() {
            return Asked::Answer(answer.to_owned());
        }
        eprintln!("{NOT_HTTP}");
    }
}

fn ask_auth(prompter: &Prompter, provider: ProviderKind) -> Asked<AuthKind> {
    let choices: Vec<AuthKind> = provider
        .auths()
        .iter()
        .copied()
        .filter(|a| !a.requires_oauth())
        .collect();
    let words: Vec<&str> = choices.iter().map(|a| provider.auth_words(*a)).collect();
    match choose(prompter, "How does it take the key?", &words, 0) {
        Asked::Answer(pick) => Asked::Answer(
            choices
                .get(pick)
                .copied()
                .unwrap_or_else(|| provider.official_auth()),
        ),
        Asked::Cancelled => Asked::Cancelled,
        Asked::Declined => Asked::Declined,
        Asked::Failed(e) => Asked::Failed(e),
    }
}

fn ask_key(prompter: &Prompter, flow: &Flow) -> Asked<String> {
    let question = if flow.signing_in {
        "Paste the API key: "
    } else {
        "Paste the key, then press Enter: "
    };
    loop {
        let answer = ask!(prompter.masked(question));
        match check_key(&answer) {
            Ok(key) => return Asked::Answer(key.to_owned()),
            Err(problem) => eprintln!("{problem} {PASTE_AGAIN}"),
        }
    }
}

fn plain_http_host(url: &str) -> Option<String> {
    let parsed = http_url(url)?;
    let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
    (parsed.scheme() == "http" && !loopback_host(&host)).then_some(host)
}

pub(crate) fn api_name(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::Anthropic => wisp_protocol::CLAUDE_API,
        ProviderKind::OpenAi => wisp_protocol::OPENAI_API,
        ProviderKind::Gemini => wisp_protocol::GEMINI_API,
    }
}

fn taken_names(ctx: &Context<'_>, config: &ConfigFile) -> BTreeSet<EndpointName> {
    let keys_file = ctx.sources().entries().unwrap_or_default();
    config
        .endpoints
        .keys()
        .chain(keys_file.keys())
        .cloned()
        .collect()
}

pub(crate) fn signing_in_name(
    front: &mut dyn crate::auth::login::Front,
    taken: &BTreeSet<EndpointName>,
    suggested: &EndpointName,
) -> Result<EndpointName, crate::auth::login::Stop> {
    let taken_by = |answer: &str| taken.iter().any(|name| name.as_str() == answer);
    let answer = crate::auth::login::name_step(front, &taken_by, suggested.as_str())?;
    EndpointName::parse(&answer).ok_or(crate::auth::login::Stop::Said)
}

pub(crate) fn suggested_name(
    provider: ProviderKind,
    url: Option<&str>,
    taken: &BTreeSet<EndpointName>,
) -> EndpointName {
    let base = match url.and_then(http_url) {
        None => wisp_core::secrets::import::default_alias(provider).to_owned(),
        Some(url) => url
            .host_str()
            .unwrap_or_default()
            .trim_start_matches('[')
            .split(['.', ':', ']'])
            .next()
            .unwrap_or_default()
            .to_owned(),
    };
    let cleaned: String = base
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if wisp_protocol::endpoint_char(c) {
                c
            } else {
                '-'
            }
        })
        .collect();
    let cleaned = cleaned.trim_start_matches(['-', '_']);
    let cleaned: String = cleaned
        .chars()
        .take(wisp_protocol::MAX_ENDPOINT_NAME - 4)
        .collect();
    let base = EndpointName::parse(&cleaned)
        .filter(|n| !n.is_env())
        .unwrap_or_else(|| EndpointName::keys_file(false));
    if !taken.contains(&base) {
        return base;
    }
    (2..)
        .filter_map(|n| EndpointName::parse(&format!("{base}-{n}")))
        .find(|n| !taken.contains(n))
        .unwrap_or(base)
}

fn read_stdin_key() -> Result<String, String> {
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| format!("could not read stdin: {}", e.kind()))?;
    match check_key(&line) {
        Ok(key) => Ok(key.to_owned()),
        Err(problem) => Err(format!("{problem} Nothing was saved.")),
    }
}

fn probe_endpoint(draft: &Draft, key: &str, path: &Path, shown: PathShown) -> ResolvedEndpoint {
    let credential = draft.auth.credential(key.to_owned());
    let (base_url, url_source) = match &draft.url {
        Some(url) => (url.clone(), UrlSource::Stored(StoreKind::File)),
        None => (draft.provider.default_url().to_owned(), UrlSource::Official),
    };
    ResolvedEndpoint {
        name: EndpointName::env(),
        provider: draft.provider,
        credential,
        base_url,
        source: Source::CredentialFile {
            path: wisp_core::secrets::credentials_path(path),
            shown,
        },
        key_env: None,
        url_source,
        pricing: PricingMode::List,
    }
}

fn probe(endpoint: &ResolvedEndpoint, model: &str) -> Result<CheckOutcome, String> {
    let runtime = wisp_core::runtime::build(&mut tokio::runtime::Builder::new_current_thread())
        .map_err(|e| format!("could not start the check: {e}"))?;
    runtime
        .block_on(endpoint.provider.probe(endpoint, model, CHECK_TIMEOUT))
        .map_err(|e| e.to_string())
}

enum Checked {
    Save(Option<&'static str>),
    Retry(Retry),
    Stop(String),
}

enum Retry {
    Key,
    Url,
    Again,
}

fn after_check(
    outcome: Result<CheckOutcome, String>,
    prompter: Option<&Prompter>,
    official: bool,
) -> Asked<Checked> {
    let reason = match &outcome {
        Ok(CheckOutcome::Ok) => return Asked::Answer(Checked::Save(None)),
        Ok(CheckOutcome::KeyRefused) => {
            let Some(prompter) = prompter else {
                return Asked::Answer(Checked::Stop(
                    "the key was refused (401), so nothing was saved".to_owned(),
                ));
            };
            return choose_add(prompter, KEY_REFUSED, AddQuestion::KeyRefused, false).and_then(
                |answer| {
                    Asked::Answer(match answer {
                        AddAnswer::EnterKeyAgain => Checked::Retry(Retry::Key),
                        AddAnswer::SaveAnyway => Checked::Save(None),
                        _ => return Asked::Declined,
                    })
                },
            );
        }
        Ok(CheckOutcome::Forbidden) => {
            let Some(prompter) = prompter else {
                return Asked::Answer(Checked::Save(Some(FORBIDDEN_NOTE)));
            };
            return choose_add(prompter, &forbidden_ask(), AddQuestion::Forbidden, false)
                .and_then(save_on_yes);
        }
        Ok(CheckOutcome::NotOffered) => {
            let Some(prompter) = prompter else {
                return Asked::Answer(Checked::Save(Some(NOT_OFFERED_NOTE)));
            };
            return choose_add(prompter, &not_offered_ask(), AddQuestion::NotOffered, false)
                .and_then(save_on_yes);
        }
        Ok(other) => other.to_string(),
        Err(e) => format!("not checked: {e}"),
    };
    let Some(prompter) = prompter else {
        return Asked::Answer(Checked::Stop(format!(
            "the check failed ({reason}), so nothing was saved; --no-check saves without it"
        )));
    };
    choose_add(
        prompter,
        &check_failed(&reason),
        AddQuestion::CheckFailed,
        !official,
    )
    .and_then(|answer| {
        Asked::Answer(match answer {
            AddAnswer::TryAgain => Checked::Retry(Retry::Again),
            AddAnswer::EnterUrlAgain => Checked::Retry(Retry::Url),
            AddAnswer::SaveAnyway => Checked::Save(None),
            _ => return Asked::Declined,
        })
    })
}

fn save_on_yes(answer: AddAnswer) -> Asked<Checked> {
    match answer {
        AddAnswer::Yes => Asked::Answer(Checked::Save(None)),
        _ => Asked::Declined,
    }
}

fn early_store_check(store: &FileStore, home: Option<&str>) -> Result<(), String> {
    match store.read_every() {
        Ok(_) => Ok(()),
        Err(e @ wisp_core::secrets::StoreError::Linked(_)) => Err(e.to_string()),
        Err(e) => Err(format!(
            "{e}; fix or move aside {} first; nothing was changed",
            credentials_words(store, home)
        )),
    }
}

struct AddVars {
    key_var: Option<VariableName>,
    url_var: Option<VariableName>,
    key_env: Option<VariableName>,
    url_env: Option<VariableName>,
}

fn add_vars(args: &AddArgs) -> Result<AddVars, String> {
    let vars = AddVars {
        key_var: parse_variable(args.key_var.as_deref(), "--key-var", given_variable)?,
        url_var: parse_variable(args.url_var.as_deref(), "--url-var", given_variable)?,
        key_env: parse_variable(args.key_env.as_deref(), "--key-env", named_variable)?,
        url_env: parse_variable(args.url_env.as_deref(), "--url-env", named_variable)?,
    };
    let one_source = "one source per account";
    if vars.key_env.is_some() {
        for (given, flag) in [
            (args.store.is_some(), "--store"),
            (args.key_stdin, "--key-stdin"),
            (vars.key_var.is_some(), "--key-var"),
            (vars.url_var.is_some(), "--url-var"),
        ] {
            if given {
                return Err(format!(
                    "--key-env names the variable that holds the key, so it can't be used with {flag}; {one_source}"
                ));
            }
        }
        if args.url.is_some() && vars.url_env.is_some() {
            return Err(format!("--url can't be used with --url-env; {one_source}"));
        }
    } else if vars.url_env.is_some() {
        return Err("--url-env goes with --key-env".to_owned());
    }
    if args.key_stdin && vars.key_var.is_some() {
        return Err(format!(
            "--key-stdin can't be used with --key-var; {one_source}"
        ));
    }
    if args.url.is_some() && vars.url_var.is_some() {
        return Err(format!("--url can't be used with --url-var; {one_source}"));
    }
    Ok(vars)
}

fn add(args: &AddArgs, ctx: &Context<'_>, flow: &Flow) -> ExitCode {
    if args.key.is_some() || args.api_key.is_some() || args.token.is_some() {
        return usage_fail(KEY_ON_COMMAND_LINE);
    }
    let vars = match add_vars(args) {
        Ok(vars) => vars,
        Err(e) => return usage_fail(e),
    };
    let reads = vars.key_var.is_some() || vars.url_var.is_some();
    if !reads && args.from_environment {
        return usage_fail("--from-environment goes with --key-var or --url-var");
    }
    if let Some(key_env) = &vars.key_env {
        if ctx.env_file.is_some() {
            eprintln!("--env-file isn't used by wisp account add without --key-var; ignoring it");
        }
        return add_table(args, ctx, key_env.clone(), vars.url_env.clone());
    }
    let source = if reads {
        match var_source(ctx, args.from_environment) {
            Ok(Some(source)) => Some(source),
            Ok(None) => {
                return usage_fail(
                    "--key-var and --url-var need --env-file FILE or --from-environment, to say where to read them",
                );
            }
            Err(e) if e == BOTH_SOURCES => return usage_fail(e),
            Err(e) => return fail(e),
        }
    } else {
        if ctx.env_file.is_some() && !flow.setup {
            eprintln!("--env-file isn't used by wisp account add without --key-var; ignoring it");
        }
        None
    };
    let home = ctx.home_text();
    let home = home.as_deref();
    let (path, shown) = match ctx.target(false) {
        Ok(target) => target,
        Err(e) => return fail(e),
    };
    let name = match args.name.as_deref().map(parse_name) {
        None => None,
        Some(Ok(name)) => Some(name),
        Some(Err(e)) => return usage_fail(e),
    };
    let current = match ConfigText::read(&path, shown).and_then(|t| t.current().map(|c| (t, c))) {
        Ok((text, config)) => (text.existed(), config),
        Err(e) => return fail(e),
    };
    let (existed, config) = current;
    if let Some(name) = &name
        && config.endpoints.contains_key(name)
    {
        return fail(format!(
            "an account named \"{name}\" already exists; remove it first with wisp account remove {name}"
        ));
    }
    let store = FileStore::beside(&path, shown);
    if let Err(e) = early_store_check(&store, home) {
        return fail(e);
    }
    let picked = match pick_store(args.store, ctx, &credentials_words(&store, home)) {
        Ok(picked) => picked,
        Err(e) => return fail(e),
    };
    let stdin_terminal = std::io::stdin().is_terminal();
    let interactive = !args.key_stdin && stdin_terminal && std::io::stderr().is_terminal();
    if args.key_stdin && stdin_terminal {
        return usage_fail(
            "--key-stdin reads the key from a pipe, not a terminal; leave it out to be asked for the key",
        );
    }
    if !interactive && !args.key_stdin && vars.key_var.is_none() {
        return fail(
            "wisp account add needs a terminal for its questions, or the key piped in with --key-stdin (printenv VAR | wisp account add NAME --key-stdin)",
        );
    }
    if let Some(url) = &args.url
        && http_url(url).is_none()
    {
        return usage_fail("--url must be an http or https URL (what was given isn't shown)");
    }
    let given_url = args.url.is_some() || vars.url_var.is_some();
    if !interactive && given_url && args.auth.is_none() {
        return usage_fail(
            "--auth is needed with --url without a terminal (api_key or bearer); it is never guessed",
        );
    }
    let prompter = ctx.prompter(flow.cancel);
    let prompter = interactive.then_some(&prompter);
    if shown == PathShown::Given && !existed {
        eprintln!("{GIVEN_CONFIG} doesn't exist yet; it will be created.");
    }
    let provider = args.provider.unwrap_or_default();
    if prompter.is_some() && args.provider.is_none() && !flow.signing_in {
        eprintln!("Provider: {} (others are planned)", provider.title());
    }
    let mut draft = Draft {
        provider,
        url: args.url.clone(),
        auth: args.auth.unwrap_or_else(|| provider.official_auth()),
        key: None,
    };
    if let Some(source) = &source {
        if let Some(var) = &vars.url_var {
            match url_from_var(source, var, NothingWas::Saved) {
                Ok(url) => draft.url = Some(url),
                Err(e) => return fail(e),
            }
        }
        if let Some(var) = &vars.key_var {
            match key_from_var(source, var, NothingWas::Saved) {
                Ok(key) => draft.key = Some(key),
                Err(e) => return fail(e),
            }
        }
    }
    if let Some(prompter) = prompter
        && draft.url.is_none()
    {
        let official = format!(
            "{}'s API ({})",
            provider.title(),
            http_url(provider.default_url())
                .and_then(|u| u.host_str().map(str::to_owned))
                .unwrap_or_default()
        );
        let (lead, apis) = if flow.signing_in && args.provider.is_none() {
            (
                "Which kind of key is it?",
                vec![
                    (
                        AccountKind::ClaudeApi.label().to_owned(),
                        ProviderKind::Anthropic,
                    ),
                    (
                        AccountKind::OpenAiApi.label().to_owned(),
                        ProviderKind::OpenAi,
                    ),
                    (
                        AccountKind::GeminiApi.label().to_owned(),
                        ProviderKind::Gemini,
                    ),
                ],
            )
        } else {
            ("Where does it connect?", vec![(official, provider)])
        };
        let mut options: Vec<&str> = apis.iter().map(|(label, _)| label.as_str()).collect();
        options.push("A gateway or proxy, at a URL you give");
        match choose(prompter, lead, &options, 0) {
            Asked::Answer(index) => match apis.get(index) {
                Some(&(_, api)) => {
                    draft.provider = api;
                    draft.auth = args.auth.unwrap_or_else(|| api.official_auth());
                }
                None => {
                    draft.url = match ask_url(prompter) {
                        Asked::Answer(url) => Some(url),
                        Asked::Cancelled => return cancelled(prompter),
                        Asked::Declined => return flow.declined(),
                        Asked::Failed(e) => return fail(e),
                    };
                }
            },
            Asked::Cancelled => return cancelled(prompter),
            Asked::Declined => return flow.declined(),
            Asked::Failed(e) => return fail(e),
        }
    }
    let provider = draft.provider;
    if let Some(host) = draft.url.as_deref().and_then(plain_http_host) {
        match prompter {
            Some(prompter) => {
                match choose_add(prompter, &plain_http(&host), AddQuestion::PlainHttp, false) {
                    Asked::Answer(AddAnswer::Yes) => {}
                    Asked::Answer(_) | Asked::Declined => return flow.declined(),
                    Asked::Cancelled => return cancelled(prompter),
                    Asked::Failed(e) => return fail(e),
                }
            }
            None => eprintln!("warning: {}", plain_http_risk(THIS_URL, &host)),
        }
    }
    if let Some(prompter) = prompter
        && args.auth.is_none()
        && draft.url.is_some()
    {
        match ask_auth(prompter, provider) {
            Asked::Answer(auth) => draft.auth = auth,
            Asked::Cancelled => return cancelled(prompter),
            Asked::Declined => return flow.declined(),
            Asked::Failed(e) => return fail(e),
        }
    }
    let mut note = None;
    loop {
        if draft.key.is_none() {
            draft.key = Some(match prompter {
                Some(prompter) => match ask_key(prompter, flow) {
                    Asked::Answer(key) => key,
                    Asked::Cancelled => return cancelled(prompter),
                    Asked::Declined => return flow.declined(),
                    Asked::Failed(e) => return fail(e),
                },
                None => match read_stdin_key() {
                    Ok(key) => key,
                    Err(e) => return fail(e),
                },
            });
        }
        let Some(key) = draft.key.clone() else {
            continue;
        };
        if args.no_check || draft.auth.subscription().is_some() {
            break;
        }
        if flow.signing_in {
            eprint!("Checking the key\u{2026} ");
        } else {
            eprint!(
                "Checking it with one count_tokens request (free on Anthropic's side; a gateway may count it)\u{2026} "
            );
        }
        let outcome = probe(&probe_endpoint(&draft, &key, &path, shown), ctx.model);
        match &outcome {
            Ok(o) => eprintln!("{o}"),
            Err(e) => eprintln!("not checked: {e}"),
        }
        match after_check(outcome, prompter, draft.url.is_none()) {
            Asked::Answer(Checked::Save(saved_note)) => {
                note = saved_note;
                break;
            }
            Asked::Answer(Checked::Retry(Retry::Key)) => draft.key = None,
            Asked::Answer(Checked::Retry(Retry::Again)) => {}
            Asked::Answer(Checked::Retry(Retry::Url)) => {
                let Some(prompter) = prompter else {
                    return fail("nothing was saved");
                };
                draft.url = match ask_url(prompter) {
                    Asked::Answer(url) => Some(url),
                    Asked::Cancelled => return cancelled(prompter),
                    Asked::Declined => return flow.declined(),
                    Asked::Failed(e) => return fail(e),
                };
            }
            Asked::Answer(Checked::Stop(why)) => return fail(why),
            Asked::Declined => return flow.declined(),
            Asked::Cancelled => match prompter {
                Some(prompter) => return cancelled(prompter),
                None => return fail("nothing was saved"),
            },
            Asked::Failed(e) => return fail(e),
        }
    }
    let taken = taken_names(ctx, &config);
    let suggested = match EndpointName::parse(api_name(provider)) {
        Some(name) if flow.signing_in && draft.url.is_none() => name,
        _ => suggested_name(provider, draft.url.as_deref(), &taken),
    };
    let name = match (name, prompter) {
        (Some(name), _) => name,
        (None, Some(prompter)) if flow.signing_in => {
            match signing_in_name(
                &mut crate::auth::login::Asking(prompter),
                &taken,
                &suggested,
            ) {
                Ok(name) => name,
                Err(crate::auth::login::Stop::Cancelled) => return cancelled(prompter),
                Err(_) => return fail("nothing was saved"),
            }
        }
        (None, None) if taken.contains(&suggested) => {
            return fail(format!(
                "{} Pass --name.",
                wisp_protocol::account_name_taken(suggested.as_str())
            ));
        }
        (None, None) => suggested,
        (None, Some(prompter)) => loop {
            let answer = match prompter.visible(&format!("Name for this account [{suggested}]: ")) {
                Ok(answer) => answer,
                Err(PromptError::Cancelled) => return cancelled(prompter),
                Err(PromptError::Io(e)) => {
                    return fail(format!("could not read the terminal: {e}"));
                }
            };
            let answer = answer.trim();
            if answer.is_empty() {
                break suggested;
            }
            match parse_name(answer) {
                Ok(name) if taken.contains(&name) => {
                    eprintln!("\"{name}\" is taken; choose another.");
                }
                Ok(name) => break name,
                Err(e) => eprintln!("{e}"),
            }
        },
    };
    let Some(key) = draft.key.take() else {
        return fail("nothing was saved");
    };
    let record = StoredRecord::Key {
        key: Secret::new(key),
        url: draft.url.clone().map(Secret::new),
    };
    let make = |store: StoreKind| Change::Add {
        name: name.clone(),
        endpoint: NewEndpoint {
            provider,
            auth: draft.auth,
            store,
            pricing: args.pricing.unwrap_or_default(),
        },
        record: record.clone(),
        make_default: args.default,
    };
    let saving = Saving {
        path: &path,
        shown,
        home,
        keys: ctx.keychain.access(),
        prompter,
        many: false,
        saved_before: 0,
    };
    let (applied, kind) = match save_stored(&saving, &picked, &mut Fallback::Ask, &make) {
        Ok(saved) => saved,
        Err(code) => return code,
    };
    let what = if draft.url.is_some() {
        "Its key and URL are"
    } else {
        "Its key is"
    };
    if flow.signing_in {
        let kind = AccountKind::of(
            endpoints::auth_credential(draft.auth),
            if draft.url.is_some() {
                wisp_render::chrome::Endpoint::Custom
            } else {
                wisp_render::chrome::Endpoint::Official
            },
            provider.as_str(),
        );
        let accounts = crate::auth::accounts::gather(ctx);
        let done = crate::auth::login::Done {
            name: name.to_string(),
            mark: accounts
                .as_ref()
                .ok()
                .and_then(|a| a.get(name.as_str())?.mark),
            ready: true,
            cleanup_pending: false,
        };
        let first = accounts.is_ok_and(|a| a.is_first(name.as_str()));
        let lines = crate::auth::login::result_lines(
            kind,
            &done,
            false,
            first,
            &wisp_tui::term::prompt::glyphs(),
            wisp_tui::term::prompt::terminal_columns(),
        );
        let _ = crate::auth::login::print_lines(&lines, &ctx.caps, Stream::Stdout, 0);
        return ExitCode::SUCCESS;
    }
    let lead = if args.default {
        format!("Saved \"{name}\" as your default account.")
    } else {
        format!("Saved \"{name}\".")
    };
    println!(
        "{lead} {what} in {}.",
        store_words(kind, &credentials_words(&store, home))
    );
    if flow.setup && !args.default {
        println!(
            "Your default account is unchanged. To start on \"{name}\" instead: wisp account default {name}"
        );
    }
    if let Some(note) = note {
        println!("{note}");
    }
    if let Outcome::Added { created: true, .. } = applied.outcome
        && shown == PathShown::Given
    {
        println!("Created {GIVEN_CONFIG}.");
    }
    let code = report(&applied, ctx, &path);
    if prompter.is_some() {
        println!("Add more any time with: wisp account add");
    }
    code
}

pub(crate) fn unread_on_keychain(key_env: &VariableName) -> String {
    format!(
        "{key_env} isn't read on this machine, which has a keychain; wisp account import stores its key"
    )
}

fn add_table(
    args: &AddArgs,
    ctx: &Context<'_>,
    key_env: VariableName,
    url_env: Option<VariableName>,
) -> ExitCode {
    let home = ctx.home_text();
    let home = home.as_deref();
    let (path, shown) = match ctx.target(false) {
        Ok(target) => target,
        Err(e) => return fail(e),
    };
    let name = match args.name.as_deref().map(parse_name) {
        None => None,
        Some(Ok(name)) => Some(name),
        Some(Err(e)) => return usage_fail(e),
    };
    if let Some(url) = &args.url
        && http_url(url).is_none()
    {
        return usage_fail("--url must be an http or https URL (what was given isn't shown)");
    }
    let (existed, config) =
        match ConfigText::read(&path, shown).and_then(|t| t.current().map(|c| (t, c))) {
            Ok((text, config)) => (text.existed(), config),
            Err(e) => return fail(e),
        };
    if let Some(name) = &name
        && config.endpoints.contains_key(name)
    {
        return fail(format!(
            "an account named \"{name}\" already exists; remove it first with wisp account remove {name}"
        ));
    }
    let custom = args.url.is_some() || url_env.is_some();
    let interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    let provider = args.provider.unwrap_or_default();
    let auth = match (args.auth, custom, interactive) {
        (Some(auth), _, _) => auth,
        (None, false, _) => provider.official_auth(),
        (None, true, false) => {
            return usage_fail(
                "--auth is needed with --url or --url-env without a terminal (api_key or bearer); it is never guessed",
            );
        }
        (None, true, true) => {
            let prompter = ctx.prompter("Cancelled; nothing was saved.");
            match ask_auth(&prompter, provider) {
                Asked::Answer(auth) => auth,
                Asked::Cancelled => return cancelled(&prompter),
                Asked::Declined => return declined(),
                Asked::Failed(e) => return fail(e),
            }
        }
    };
    let taken = taken_names(ctx, &config);
    let name = name.unwrap_or_else(|| suggested_name(provider, args.url.as_deref(), &taken));
    if shown == PathShown::Given && !existed {
        eprintln!("{GIVEN_CONFIG} doesn't exist yet; it will be created.");
    }
    let change = Change::AddTable {
        name: name.clone(),
        table: NewTable {
            provider,
            auth,
            key_env: key_env.clone(),
            url: args.url.clone(),
            url_env: url_env.clone(),
            pricing: args.pricing.unwrap_or_default(),
        },
        make_default: args.default,
    };
    let applied = match apply_change(&path, shown, change, home, ctx.keychain.access()) {
        Ok(applied) => applied,
        Err(code) => return code,
    };
    let lead = if args.default {
        format!("Saved \"{name}\" as your default account.")
    } else {
        format!("Saved \"{name}\".")
    };
    let url_words = url_env
        .as_ref()
        .map_or_else(String::new, |var| format!(", and its URL from {var}"));
    println!(
        "{lead} Its key is read from the variable {key_env} at each start{url_words}; nothing was read now. wisp doctor --check {name} checks it."
    );
    if ctx.keychain.detected().available() {
        println!("{}", unread_on_keychain(&key_env));
    }
    if let Outcome::Added { created: true, .. } = applied.outcome
        && shown == PathShown::Given
    {
        println!("Created {GIVEN_CONFIG}.");
    }
    report(&applied, ctx, &path)
}

fn read_existing(path: &Path, shown: PathShown) -> Result<ConfigFile, String> {
    ConfigText::read(path, shown)
        .and_then(|t| t.current())
        .map_err(|e| e.to_string())
}

fn preflight(
    path: &Path,
    shown: PathShown,
    config: &ConfigFile,
    change: &Change,
    home: Option<&str>,
    keys: &dyn KeyringAccess,
) -> Result<(), String> {
    keys.begin_phase();
    let leftover =
        matches!(change, Change::Remove { name, .. } if !config.endpoints.contains_key(name));
    let files = config
        .endpoints
        .values()
        .any(|e| e.store() == Some(StoreKind::File));
    if files || leftover {
        early_store_check(&FileStore::beside(path, shown), home)?;
    }
    wisp_core::secrets::check(path, shown, change, keys).map_err(|e| change_error(&e, home))
}

fn try_change(
    path: &Path,
    shown: PathShown,
    change: Change,
    keys: &dyn KeyringAccess,
) -> Result<Applied, Failed> {
    keys.begin_phase();
    wisp_core::secrets::check(path, shown, &change, keys)?;
    wisp_core::secrets::apply_noting(path, shown, change, keys, LOCK_WAIT)
}

pub(crate) fn move_start(name: &EndpointName, ctx: &Context<'_>) -> Result<(), String> {
    let (path, shown) = ctx.target(true)?;
    let change = Change::SetDefault { name: name.clone() };
    try_change(&path, shown, change, ctx.keychain.access())
        .map(|_| ())
        .map_err(|failed| change_error(&failed.error, ctx.home_text().as_deref()))
}

fn failed_code(failed: &Failed, home: Option<&str>) -> ExitCode {
    for note in &failed.notes {
        eprintln!("note: {}", note_words(note, home));
    }
    fail(change_error(&failed.error, home))
}

fn apply_change(
    path: &Path,
    shown: PathShown,
    change: Change,
    home: Option<&str>,
    keys: &dyn KeyringAccess,
) -> Result<Applied, ExitCode> {
    try_change(path, shown, change, keys).map_err(|failed| failed_code(&failed, home))
}

#[derive(Default)]
pub(crate) enum Fallback {
    #[default]
    Ask,
    Yes,
}

pub(crate) struct Saving<'a> {
    pub(crate) path: &'a Path,
    pub(crate) shown: PathShown,
    pub(crate) home: Option<&'a str>,
    pub(crate) keys: &'a dyn KeyringAccess,
    pub(crate) prompter: Option<&'a Prompter>,
    pub(crate) many: bool,
    pub(crate) saved_before: usize,
}

pub(crate) fn save_stored(
    saving: &Saving<'_>,
    picked: &Picked,
    fallback: &mut Fallback,
    make: &dyn Fn(StoreKind) -> Change,
) -> Result<(Applied, StoreKind), ExitCode> {
    let ask = |question: &str| {
        saving
            .prompter
            .map(|p| choose_add(p, question, AddQuestion::KeychainFile, false))
    };
    save_stored_asking(saving, picked, fallback, make, &ask)
}

fn save_stored_asking(
    saving: &Saving<'_>,
    picked: &Picked,
    fallback: &mut Fallback,
    make: &dyn Fn(StoreKind) -> Change,
    ask: &dyn Fn(&str) -> Option<Asked<AddAnswer>>,
) -> Result<(Applied, StoreKind), ExitCode> {
    let kind = match (picked, &fallback) {
        (Picked::Kind(kind), _) => *kind,
        (Picked::Auto, Fallback::Yes) => StoreKind::File,
        (Picked::Auto, Fallback::Ask) => StoreKind::Keyring,
    };
    let failed = match try_change(saving.path, saving.shown, make(kind), saving.keys) {
        Ok(applied) => return Ok((applied, kind)),
        Err(failed) => failed,
    };
    let error = match (&failed.error, picked) {
        (ChangeError::KeychainWrite { error, .. }, Picked::Auto)
            if !matches!(error, KeyringError::Unconfirmed { .. }) =>
        {
            error.clone()
        }
        _ => return Err(failed_code(&failed, saving.home)),
    };
    for note in &failed.notes {
        eprintln!("note: {}", note_words(note, saving.home));
    }
    let file = credentials_words(&FileStore::beside(saving.path, saving.shown), saving.home);
    let which = error
        .which()
        .unwrap_or_else(|| host::REFUSED_REQUEST.to_owned());
    let Some(answer) = ask(&keychain_question(&which, &file, saving.many)) else {
        return Err(fail(keychain_failed_stop(
            &error,
            &file,
            saving.saved_before > 0,
        )));
    };
    match answer {
        Asked::Answer(AddAnswer::Yes) => {}
        Asked::Answer(_) | Asked::Declined if saving.saved_before > 0 => {
            return Err(fail("the rest weren't saved"));
        }
        Asked::Answer(_) | Asked::Declined => return Err(declined()),
        Asked::Cancelled => {
            return Err(match saving.prompter {
                Some(prompter) => cancelled(prompter),
                None => fail("nothing was saved"),
            });
        }
        Asked::Failed(e) => return Err(fail(e)),
    }
    *fallback = Fallback::Yes;
    apply_change(
        saving.path,
        saving.shown,
        make(StoreKind::File),
        saving.home,
        saving.keys,
    )
    .map(|applied| (applied, StoreKind::File))
}

fn owned_record(
    path: &Path,
    shown: PathShown,
    config: &ConfigFile,
    name: &EndpointName,
    keys: &dyn KeyringAccess,
) -> Option<StoreKind> {
    let id = config.store_id.as_ref()?;
    keys.begin_phase();
    let this = canonical_config(path);
    let mine = |owner: Option<std::path::PathBuf>| this.is_some() && owner == this;
    let kind = config.endpoints.get(name).map(|e| e.store());
    let file = matches!(kind, None | Some(Some(StoreKind::File)))
        && FileStore::beside(path, shown)
            .read_all(id)
            .ok()
            .and_then(|records| records.get(name).map(|s| s.owner.clone()))
            .is_some_and(mine);
    if file {
        return Some(StoreKind::File);
    }
    let keyring = matches!(kind, None | Some(Some(StoreKind::Keyring)))
        && !matches!(keys.fixed(), Some(KeyringError::Off { .. }))
        && keys
            .read(id, name)
            .ok()
            .flatten()
            .is_some_and(|s| mine(s.owner));
    keyring.then_some(StoreKind::Keyring)
}

fn remove(name: &str, yes: bool, keep_key: bool, ctx: &Context<'_>) -> ExitCode {
    ctx.ignore_env_file("remove");
    let home = ctx.home_text();
    let home = home.as_deref();
    let (path, shown) = match ctx.target(true) {
        Ok(target) => target,
        Err(e) => return fail(e),
    };
    let name = match parse_name_for(name, &wisp_core::endpoints::host::no_table("remove")) {
        Ok(name) => name,
        Err(e) => return usage_fail(e),
    };
    if std::fs::symlink_metadata(&path).is_err() {
        return fail(ChangeError::NoSuchEndpoint(name));
    }
    let config = match read_existing(&path, shown) {
        Ok(config) => config,
        Err(e) => return fail(e),
    };
    let keys = ctx.keychain.access();
    let change = Change::Remove {
        name: name.clone(),
        keep_key,
        chat: None,
    };
    if keep_key && let Err(e) = preflight(&path, shown, &config, &change, home, keys) {
        return fail(e);
    }
    let owned = if keep_key {
        None
    } else {
        owned_record(&path, shown, &config, &name, keys)
    };
    let file = credentials_words(&FileStore::beside(&path, shown), home);
    let place = |kind: Option<StoreKind>| match kind {
        Some(StoreKind::Keyring) => IN_KEYCHAIN.to_owned(),
        _ => file.clone(),
    };
    let question = match (config.endpoints.get(&name), owned) {
        (Some(_), Some(_)) => remove_question(name.as_str(), Some(&place(owned))),
        (Some(_), None) if keep_key => {
            format!("Remove \"{name}\" and leave its key in the keychain?")
        }
        (Some(_), None) => remove_question(name.as_str(), None),
        (None, Some(_)) => format!("Remove the leftover key for \"{name}\"?"),
        (None, None) => {
            if let Err(e) = preflight(&path, shown, &config, &change, home, keys) {
                return fail(e);
            }
            return fail(ChangeError::NoSuchEndpoint(name));
        }
    };
    if !keep_key && let Err(e) = preflight(&path, shown, &config, &change, home, keys) {
        return fail(e);
    }
    if !yes {
        let interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
        if !interactive {
            return fail(
                "wisp account remove asks first; pass --yes to remove without a terminal. Nothing was changed",
            );
        }
        let prompter = ctx.prompter("Cancelled; nothing was changed.");
        match choose(&prompter, &question, &REMOVE_KEEP, 1) {
            Asked::Answer(0) => {}
            Asked::Answer(_) | Asked::Declined => {
                println!("Nothing was changed.");
                return ExitCode::SUCCESS;
            }
            Asked::Cancelled => return cancelled(&prompter),
            Asked::Failed(e) => return fail(e),
        }
    }
    let applied = match apply_change(&path, shown, change, home, keys) {
        Ok(applied) => applied,
        Err(code) => return code,
    };
    let config_words = match shown {
        PathShown::Default => wisp_protocol::CONFIG_FILE.to_owned(),
        PathShown::Given => GIVEN_CONFIG.to_owned(),
    };
    match &applied.outcome {
        Outcome::Removed {
            store,
            deleted,
            was_default,
            kept,
            new_start,
        } => {
            let mut line = match (store, deleted, kept) {
                (_, _, Some(account)) => format!(
                    "Removed \"{name}\". Its key is still in the keychain (item {account}); run wisp account remove {name} when the keychain answers to delete it."
                ),
                (Some(_), true, None) => {
                    format!(
                        "Removed \"{name}\" and deleted its key from {}.",
                        place(*store)
                    )
                }
                (Some(_), false, None) => format!("Removed \"{name}\"."),
                (None, _, None) => format!(
                    "Removed \"{name}\". Its key is in a variable, which remove never touches."
                ),
            };
            let start = new_start.as_ref().map(|start| {
                let state = config
                    .endpoints
                    .get(start)
                    .map_or(wisp_render::chrome::account::AccountState::Api, |e| {
                        wisp_render::chrome::account::AccountState::of(e.auth.billing())
                    });
                let sources = ctx.sources();
                let marks = sources
                    .entries()
                    .map(|e| sources.marks(&e))
                    .unwrap_or_default();
                let mark = marks.get(start).copied();
                let glyphs = ctx.prompter("").glyphs();
                let text = wisp_render::chrome::account::start_line(
                    start.as_str(),
                    mark,
                    state,
                    wisp_render::chrome::account::Start::Now,
                    &glyphs,
                );
                let tone = wisp_render::theme::Token::Text;
                wisp_render::chrome::account::mark_line(&text, tone, mark, &glyphs)
            });
            if *was_default && start.is_none() {
                line.push_str(&format!(
                    " It was the default account, so {config_words} names none now."
                ));
            }
            println!("{line}");
            if let Some(start) = start {
                let _ = crate::auth::login::print_lines(&[start], &ctx.caps, Stream::Stdout, 0);
            }
        }
        Outcome::LeftoverRemoved => {
            println!(
                "Deleted the leftover key for \"{name}\" from {}.",
                place(owned)
            );
        }
        _ => {}
    }
    report(&applied, ctx, &path)
}

fn set_default(name: &str, ctx: &Context<'_>) -> ExitCode {
    ctx.ignore_env_file("default");
    let home = ctx.home_text();
    let home = home.as_deref();
    let (path, shown) = match ctx.target(true) {
        Ok(target) => target,
        Err(e) => return fail(e),
    };
    let name = match EndpointName::parse(name) {
        Some(name) => name,
        None => return usage_fail(wisp_protocol::bad_account_name()),
    };
    if std::fs::symlink_metadata(&path).is_ok() {
        let config = match read_existing(&path, shown) {
            Ok(config) => config,
            Err(e) => return fail(e),
        };
        if !name.names_keys_file() && !config.endpoints.contains_key(&name) {
            return fail(ChangeError::NoSuchEndpoint(name));
        }
        let change = Change::SetDefault { name: name.clone() };
        if let Err(e) = preflight(&path, shown, &config, &change, home, ctx.keychain.access()) {
            return fail(e);
        }
    } else if !name.names_keys_file() {
        return fail(ChangeError::NoSuchEndpoint(name));
    }
    let change = Change::SetDefault { name: name.clone() };
    let applied = match apply_change(&path, shown, change, home, ctx.keychain.access()) {
        Ok(applied) => applied,
        Err(code) => return code,
    };
    println!("\"{name}\" is now the default account.");
    report(&applied, ctx, &path)
}

fn list(ctx: &Context<'_>, name: Option<&str>) -> ExitCode {
    if ctx.config.is_some()
        && let Err(e) = ctx.target(true)
    {
        return fail(e);
    }
    crate::auth::accounts::status(ctx, name)
}

pub(crate) mod edit;
pub(crate) mod host;
pub(crate) mod import;

#[cfg(test)]
mod tests;
