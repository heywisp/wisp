use std::io::IsTerminal;
use std::process::ExitCode;

use clap::Args;
use wisp_core::config_file::ConfigText;
use wisp_core::endpoints::{
    AuthKind, ConfigFile, EndpointConfig, EndpointName, KeyFrom, PathShown, PricingMode,
    ProviderKind, host, http_url,
};
use wisp_core::secrets::{
    Change, ChangeError, Edit, FileStore, KeyringError, Secret, StoreKind, StoredRecord,
    VariableName, keychain_off_for_flag, keychain_words,
};
use wisp_tui::term::prompt::Prompter;

use super::{
    Asked, Checked, Context, Draft, KEY_ON_COMMAND_LINE, NothingWas, Retry, VarSource, after_check,
    apply_change, ask_auth, ask_key, ask_url, auths, cancelled, change_error, choose, declined,
    early_store_check, fail, given_variable, key_from_var, named_variable, parse_name_for,
    pricings, probe, probe_endpoint, providers, read_stdin_key, report, store_kinds, url_from_var,
    usage_fail, var_source,
};

#[cfg_attr(test, derive(Debug))]
#[derive(Args)]
pub(crate) struct EditArgs {
    #[arg(value_name = "NAME", help = "The account to change")]
    name: String,
    #[arg(long, help = "Replace the key: asks for the new one, hidden")]
    replace_key: bool,
    #[arg(
        long,
        help = "Read the new key from stdin: one line, never from a terminal"
    )]
    key_stdin: bool,
    #[arg(
        long,
        value_name = "VAR",
        help = concat!("Read the new key once from this variable, in ", wisp_protocol::given_env_file!(), " or, with --from-environment, in the environment")
    )]
    key_var: Option<String>,
    #[arg(
        long,
        value_name = "VAR",
        help = "Read the new URL once from this variable, from the same source as --key-var"
    )]
    url_var: Option<String>,
    #[arg(
        long,
        conflicts_with_all = ["key_env", "url_env"],
        help = "With --key-var or --url-var, read the variables from the environment instead of a file"
    )]
    from_environment: bool,
    #[arg(long, value_name = "PROVIDER", value_parser = providers(), help = "The provider: anthropic, openai or gemini")]
    provider: Option<ProviderKind>,
    #[arg(
        long,
        value_name = "URL",
        help = "A new gateway or proxy URL. Other programs can see the command line, so a URL holding a token is better typed at the prompt or read with --url-var"
    )]
    url: Option<String>,
    #[arg(long, help = "Go back to the provider's own URL")]
    no_url: bool,
    #[arg(long, value_name = "AUTH", value_parser = auths(), help = "How the key is sent: api_key or bearer")]
    auth: Option<AuthKind>,
    #[arg(long, value_name = "PRICING", value_parser = pricings(), help = "list (estimates at list prices) or none (tokens only)")]
    pricing: Option<PricingMode>,
    #[arg(
        long,
        value_name = "STORE",
        value_parser = store_kinds(),
        help = concat!("Move the stored key: keyring (the system keychain) or file (", wisp_protocol::credentials_beside_config!(), ", readable only by you)")
    )]
    store: Option<StoreKind>,
    #[arg(
        long = "name",
        value_name = "NEW",
        help = format!("Rename the account ({})", wisp_protocol::endpoint_name_rule())
    )]
    rename: Option<String>,
    #[arg(
        long,
        help = "Save without the count_tokens check (the local key check still runs)"
    )]
    no_check: bool,
    #[arg(
        long,
        value_name = "VAR",
        help = "For an account that reads its key from a variable: the variable's new name"
    )]
    key_env: Option<String>,
    #[arg(
        long,
        value_name = "VAR",
        help = "For an account that reads its key from a variable: the variable that holds the URL"
    )]
    url_env: Option<String>,
    #[arg(long, hide = true, value_name = "KEY")]
    key: Option<String>,
    #[arg(long, hide = true, value_name = "KEY")]
    api_key: Option<String>,
    #[arg(long, hide = true, value_name = "KEY")]
    token: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum NewKey {
    #[default]
    Keep,
    Prompt,
    Stdin,
    Var(VariableName),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum NewUrl {
    #[default]
    Keep,
    Literal(String),
    Var(VariableName),
    Official,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Wanted {
    key: NewKey,
    url: NewUrl,
    provider: Option<ProviderKind>,
    auth: Option<AuthKind>,
    pricing: Option<PricingMode>,
    rename: Option<EndpointName>,
    store: Option<StoreKind>,
    key_env: Option<VariableName>,
    url_env: Option<VariableName>,
}

impl Wanted {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    fn reads(&self) -> bool {
        matches!(self.key, NewKey::Var(_)) || matches!(self.url, NewUrl::Var(_))
    }
}

const ONE_SOURCE: &str = "one source per account";

fn wanted(args: &EditArgs) -> Result<Wanted, String> {
    let key_var = args
        .key_var
        .as_deref()
        .map(|t| given_variable(t, "--key-var"))
        .transpose()?;
    let url_var = args
        .url_var
        .as_deref()
        .map(|t| given_variable(t, "--url-var"))
        .transpose()?;
    let key_env = args
        .key_env
        .as_deref()
        .map(|t| named_variable(t, "--key-env"))
        .transpose()?;
    let url_env = args
        .url_env
        .as_deref()
        .map(|t| named_variable(t, "--url-env"))
        .transpose()?;
    let keys = [args.replace_key, args.key_stdin, key_var.is_some()];
    if keys.iter().filter(|k| **k).count() > 1 {
        return Err(format!(
            "give one of --replace-key, --key-stdin and --key-var; {ONE_SOURCE}"
        ));
    }
    let urls = [args.url.is_some(), args.no_url, url_var.is_some()];
    if urls.iter().filter(|u| **u).count() > 1 {
        return Err(format!(
            "give one of --url, --url-var and --no-url; {ONE_SOURCE}"
        ));
    }
    if url_env.is_some() && (args.url.is_some() || args.no_url) {
        return Err(format!(
            "--url-env can't be used with --url or --no-url; {ONE_SOURCE}"
        ));
    }
    if let Some(url) = &args.url
        && http_url(url).is_none()
    {
        return Err("--url must be an http or https URL (what was given isn't shown)".to_owned());
    }
    let rename = args
        .rename
        .as_deref()
        .map(|t| parse_name_for(t, &host::reserved_name()))
        .transpose()?;
    let key = match (args.replace_key, args.key_stdin, key_var) {
        (true, _, _) => NewKey::Prompt,
        (_, true, _) => NewKey::Stdin,
        (_, _, Some(var)) => NewKey::Var(var),
        _ => NewKey::Keep,
    };
    let url = match (&args.url, args.no_url, url_var) {
        (Some(url), _, _) => NewUrl::Literal(url.clone()),
        (_, true, _) => NewUrl::Official,
        (_, _, Some(var)) => NewUrl::Var(var),
        _ => NewUrl::Keep,
    };
    let wanted = Wanted {
        key,
        url,
        provider: args.provider,
        auth: args.auth,
        pricing: args.pricing,
        rename,
        store: args.store,
        key_env,
        url_env,
    };
    if args.from_environment && !wanted.reads() {
        return Err("--from-environment goes with --key-var or --url-var".to_owned());
    }
    Ok(wanted)
}

pub(crate) fn field(name: &str, field: &str, value: &str, ctx: &Context<'_>) -> ExitCode {
    #[derive(clap::Parser)]
    #[command(name = "wisp account edit")]
    struct One {
        #[command(flatten)]
        args: EditArgs,
    }
    let flag = format!("--{}", field.replace('_', "-"));
    match <One as clap::Parser>::try_parse_from(["wisp account edit", name, flag.as_str(), value]) {
        Ok(one) => run(&one.args, ctx),
        Err(_) => usage_fail(format!(
            "endpoints.{name}.{field} can't take that value (what was given isn't shown)"
        )),
    }
}

pub(crate) fn key_args(name: &str) -> Result<EditArgs, String> {
    use clap::FromArgMatches;
    let command = <EditArgs as clap::Args>::augment_args(clap::Command::new("edit"));
    let matches = command
        .try_get_matches_from(["edit", name, "--replace-key"])
        .map_err(|e| e.to_string())?;
    EditArgs::from_arg_matches(&matches).map_err(|e| e.to_string())
}

pub(crate) fn run(args: &EditArgs, ctx: &Context<'_>) -> ExitCode {
    if args.key.is_some() || args.api_key.is_some() || args.token.is_some() {
        return usage_fail(KEY_ON_COMMAND_LINE);
    }
    let mut wanted = match wanted(args) {
        Ok(wanted) => wanted,
        Err(e) => return usage_fail(e),
    };
    let source = if wanted.reads() {
        match var_source(ctx, args.from_environment) {
            Ok(Some(source)) => Some(source),
            Ok(None) => {
                return usage_fail(
                    "--key-var and --url-var need --env-file FILE or --from-environment, to say where to read them",
                );
            }
            Err(e) if e == super::BOTH_SOURCES => return usage_fail(e),
            Err(e) => return fail(e),
        }
    } else {
        if ctx.env_file.is_some() {
            eprintln!("--env-file isn't used by wisp account edit without --key-var; ignoring it");
        }
        None
    };
    let home = ctx.home_text();
    let home = home.as_deref();
    let (path, shown) = match ctx.target(true) {
        Ok(target) => target,
        Err(e) => return fail(e),
    };
    let name = match parse_name_for(&args.name, &host::no_table("edit")) {
        Ok(name) => name,
        Err(e) => return usage_fail(e),
    };
    if std::fs::symlink_metadata(&path).is_err() {
        return fail(ChangeError::NoSuchEndpoint(name));
    }
    let config = match ConfigText::read(&path, shown).and_then(|t| t.current()) {
        Ok(config) => config,
        Err(e) => return fail(e),
    };
    let Some(endpoint) = config.endpoints.get(&name).cloned() else {
        return fail(ChangeError::NoSuchEndpoint(name));
    };
    if let Some(new) = &wanted.rename
        && *new != name
        && config.endpoints.contains_key(new)
    {
        return fail(ChangeError::NameTaken(new.clone()));
    }
    let interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    let prompter = ctx.prompter("Cancelled; nothing was changed.");
    let prompter = interactive.then_some(&prompter);
    let variable = matches!(endpoint.key, KeyFrom::Variable(_));
    if wanted.is_empty() {
        let Some(prompter) = prompter else {
            return fail(
                "nothing to change: give a flag such as --replace-key, --url or --pricing (see wisp account edit --help)",
            );
        };
        match pick(prompter, &endpoint, variable) {
            Asked::Answer(picked) => wanted = picked,
            Asked::Cancelled => return cancelled(prompter),
            Asked::Declined => return declined(),
            Asked::Failed(e) => return fail(e),
        }
    }
    let effective_auth = wanted.auth.unwrap_or(endpoint.auth);
    if let Some(kind) = wanted
        .auth
        .and_then(AuthKind::subscription)
        .filter(|k| k.requires_oauth())
        .or_else(|| {
            (wanted.key != NewKey::Keep)
                .then(|| effective_auth.subscription())
                .flatten()
                .filter(|k| k.requires_oauth())
        })
    {
        let shown = wisp_protocol::subscription_auth::SubscriptionProvider::from(kind);
        return fail(shown.login_hint());
    }
    let job = Job {
        ctx,
        path: &path,
        shown,
        home,
        config: &config,
        name: &name,
        endpoint: &endpoint,
        source: source.as_ref(),
        prompter,
        no_check: args.no_check,
    };
    if variable {
        job.table(wanted)
    } else {
        job.stored(wanted)
    }
}

fn ask_variable(prompter: &Prompter, flag: &str) -> Asked<VariableName> {
    loop {
        let answer = match prompter.visible("Variable name: ") {
            Ok(answer) => answer,
            Err(e) => return Asked::from(e),
        };
        match named_variable(answer.trim(), flag) {
            Ok(name) => return Asked::Answer(name),
            Err(e) => eprintln!("{e}"),
        }
    }
}

fn ask_rename(prompter: &Prompter) -> Asked<EndpointName> {
    loop {
        let answer = match prompter.visible("New name: ") {
            Ok(answer) => answer,
            Err(e) => return Asked::from(e),
        };
        match parse_name_for(answer.trim(), &host::reserved_name()) {
            Ok(name) => return Asked::Answer(name),
            Err(e) => eprintln!("{e}"),
        }
    }
}

macro_rules! answer {
    ($e:expr) => {
        match $e {
            Asked::Answer(value) => value,
            Asked::Cancelled => return Asked::Cancelled,
            Asked::Declined => return Asked::Declined,
            Asked::Failed(e) => return Asked::Failed(e),
        }
    };
}

#[derive(Clone, Copy)]
enum Field {
    Key,
    KeyEnv,
    UrlEnv,
    Url,
    Official,
    Auth,
    Pricing,
    Rename,
}

fn pick(prompter: &Prompter, endpoint: &EndpointConfig, variable: bool) -> Asked<Wanted> {
    let fields: &[(Field, &str)] = if variable {
        &[
            (Field::KeyEnv, "The variable that holds the key"),
            (Field::UrlEnv, "The variable that holds the URL"),
            (Field::Url, "The URL"),
            (Field::Official, "Go back to the provider's own URL"),
            (Field::Auth, "How it takes the key"),
            (Field::Pricing, "Pricing"),
            (Field::Rename, "The name"),
        ]
    } else {
        &[
            (Field::Key, "The key (asked hidden)"),
            (Field::Url, "The URL"),
            (Field::Official, "Go back to the provider's own URL"),
            (Field::Auth, "How it takes the key"),
            (Field::Pricing, "Pricing"),
            (Field::Rename, "The name"),
        ]
    };
    let labels: Vec<&str> = fields.iter().map(|(_, label)| *label).collect();
    let index = answer!(choose(prompter, "What do you want to change?", &labels, 0));
    let mut wanted = Wanted::default();
    match fields.get(index).map(|(field, _)| *field) {
        Some(Field::Key) | None => wanted.key = NewKey::Prompt,
        Some(Field::KeyEnv) => wanted.key_env = Some(answer!(ask_variable(prompter, "--key-env"))),
        Some(Field::UrlEnv) => wanted.url_env = Some(answer!(ask_variable(prompter, "--url-env"))),
        Some(Field::Url) => wanted.url = NewUrl::Literal(answer!(ask_url(prompter))),
        Some(Field::Official) => wanted.url = NewUrl::Official,
        Some(Field::Auth) => wanted.auth = Some(answer!(ask_auth(prompter, endpoint.provider))),
        Some(Field::Pricing) => {
            let names = PricingMode::ALL.map(PricingMode::as_str);
            let chosen = answer!(choose(prompter, "Pricing:", &names, 0));
            wanted.pricing = PricingMode::ALL.get(chosen).copied();
        }
        Some(Field::Rename) => wanted.rename = Some(answer!(ask_rename(prompter))),
    }
    Asked::Answer(wanted)
}

struct Job<'a> {
    ctx: &'a Context<'a>,
    path: &'a std::path::Path,
    shown: PathShown,
    home: Option<&'a str>,
    config: &'a ConfigFile,
    name: &'a EndpointName,
    endpoint: &'a EndpointConfig,
    source: Option<&'a VarSource>,
    prompter: Option<&'a Prompter>,
    no_check: bool,
}

fn changes_words(wanted: &Wanted, name: &EndpointName) -> String {
    let mut words: Vec<String> = Vec::new();
    if wanted.key != NewKey::Keep {
        words.push("a new key".to_owned());
    }
    match &wanted.url {
        NewUrl::Keep => {}
        NewUrl::Literal(_) | NewUrl::Var(_) => words.push("a new URL".to_owned()),
        NewUrl::Official => words.push("the provider's own URL".to_owned()),
    }
    if let Some(var) = &wanted.key_env {
        words.push(format!("its key from {var}"));
    }
    if let Some(var) = &wanted.url_env {
        words.push(format!("its URL from {var}"));
    }
    if let Some(provider) = wanted.provider {
        words.push(format!("provider {provider}"));
    }
    if let Some(auth) = wanted.auth {
        words.push(format!("auth {}", auth.as_str()));
    }
    if let Some(pricing) = wanted.pricing {
        words.push(format!("pricing {}", pricing.as_str()));
    }
    if wanted.rename.as_ref().is_some_and(|new| new != name) {
        words.push(format!("renamed from {name}"));
    }
    match wanted.store {
        Some(StoreKind::Keyring) => {
            words.push(format!("its key in {}", wisp_protocol::IN_KEYCHAIN))
        }
        Some(StoreKind::File) => words.push(format!("its key in {}", wisp_protocol::CREDENTIALS)),
        None => {}
    }
    words.join(", ")
}

impl Job<'_> {
    fn finish(&self, edit: Edit, wanted: &Wanted, note: Option<&str>) -> ExitCode {
        let change = Change::Edit {
            name: self.name.clone(),
            edit,
        };
        let applied = match apply_change(
            self.path,
            self.shown,
            change,
            self.home,
            self.ctx.keychain.access(),
        ) {
            Ok(applied) => applied,
            Err(code) => return code,
        };
        let target = wanted.rename.as_ref().unwrap_or(self.name);
        let changes = changes_words(wanted, self.name);
        if changes.is_empty() {
            println!("Updated \"{target}\".");
        } else {
            println!("Updated \"{target}\": {changes}.");
        }
        if let Some(note) = note {
            println!("{note}");
        }
        report(&applied, self.ctx, self.path)
    }

    fn table(&self, wanted: Wanted) -> ExitCode {
        if wanted.key != NewKey::Keep
            || matches!(wanted.url, NewUrl::Var(_))
            || wanted.store.is_some()
        {
            return fail(format!(
                "{} names its key as a variable; store it first with wisp account import, or change the variable with --key-env",
                self.name
            ));
        }
        let (url, url_env) = match (&wanted.url, &wanted.url_env) {
            (NewUrl::Literal(url), _) => (Some(Some(url.clone())), Some(None)),
            (NewUrl::Official, _) => (Some(None), Some(None)),
            (_, Some(var)) => (Some(None), Some(Some(var.clone()))),
            _ => (None, None),
        };
        let edit = Edit {
            rename: wanted.rename.clone().filter(|new| new != self.name),
            provider: wanted.provider,
            auth: wanted.auth,
            pricing: wanted.pricing,
            url,
            url_env,
            key_env: wanted.key_env.clone(),
            ..Edit::default()
        };
        let reads_differently = wanted.key_env.is_some()
            || wanted.url_env.is_some()
            || wanted.url != NewUrl::Keep
            || wanted.provider.is_some()
            || wanted.auth.is_some();
        let target = wanted.rename.as_ref().unwrap_or(self.name);
        let key_env = wanted.key_env.as_ref().or(match &self.endpoint.key {
            wisp_core::endpoints::KeyFrom::Variable(var) => Some(var),
            _ => None,
        });
        let unread = key_env
            .filter(|_| self.ctx.keychain.detected().available())
            .map(|var| format!(" {}", super::unread_on_keychain(var)))
            .unwrap_or_default();
        let note = reads_differently.then(|| {
            format!(
                "Nothing was read or checked now; wisp doctor --check {target} checks it.{unread}"
            )
        });
        self.finish(edit, &wanted, note.as_deref())
    }

    fn stored(&self, mut wanted: Wanted) -> ExitCode {
        if wanted.key_env.is_some() || wanted.url_env.is_some() {
            return fail(format!(
                "{}'s key is stored; to read a variable instead, remove it and add it again with --key-env",
                self.name
            ));
        }
        let from = self.endpoint.store();
        if wanted.store == from {
            wanted.store = None;
        }
        if wanted.store == Some(StoreKind::Keyring)
            && let Some(KeyringError::Off { origin }) = self.ctx.keychain.off_error()
        {
            return fail(keychain_off_for_flag(&origin));
        }
        let store = FileStore::beside(self.path, self.shown);
        if (from == Some(StoreKind::File) || wanted.store == Some(StoreKind::File))
            && let Err(e) = early_store_check(&store, self.home)
        {
            return fail(e);
        }
        let touches = wanted.key != NewKey::Keep
            || wanted.url != NewUrl::Keep
            || wanted.provider.is_some()
            || wanted.rename.as_ref().is_some_and(|new| new != self.name)
            || wanted.store.is_some();
        let record = match (from, &self.config.store_id) {
            (Some(StoreKind::Keyring), Some(id)) if touches => {
                self.ctx.keychain.access().begin_phase();
                match self.ctx.keychain.access().read(id, self.name) {
                    Ok(stored) => stored.map(|s| s.record),
                    Err(error) => return fail(keychain_words(self.name, &error, false)),
                }
            }
            (Some(StoreKind::Keyring), _) => None,
            (_, id) => id
                .as_ref()
                .and_then(|id| store.read_all(id).ok())
                .and_then(|records| records.get(self.name).map(|s| s.record.clone())),
        };
        let (current_key, record_url) = match &record {
            Some(StoredRecord::Key { key, url }) => (
                Some(key.expose().to_owned()),
                url.as_ref().map(|u| u.expose().to_owned()),
            ),
            None => (None, None),
        };
        let key_change = wanted.key != NewKey::Keep;
        let writes_record = key_change
            || matches!(wanted.url, NewUrl::Literal(_) | NewUrl::Var(_))
            || (wanted.url == NewUrl::Official && record_url.is_some());
        let probes = !self.no_check
            && wanted
                .auth
                .unwrap_or(self.endpoint.auth)
                .subscription()
                .is_none()
            && (key_change || wanted.url != NewUrl::Keep || wanted.provider.is_some());
        let needs_key =
            writes_record || probes || wanted.rename.is_some() || wanted.store.is_some();
        if current_key.is_none() && !key_change && needs_key {
            return fail(ChangeError::NoRecord(self.name.clone()));
        }
        if let Err(e) = self.precheck(&wanted, writes_record) {
            return fail(e);
        }
        if key_change
            && record.is_none()
            && self.endpoint.url.is_none()
            && wanted.url == NewUrl::Keep
        {
            match self.prompter {
                None => {
                    return fail(format!(
                        "{}'s URL was in its lost record; give --url, --url-var or --no-url",
                        self.name
                    ));
                }
                Some(prompter) => match self.ask_lost_url(prompter) {
                    Asked::Answer(url) => wanted.url = url,
                    Asked::Cancelled => return cancelled(prompter),
                    Asked::Declined => return declined(),
                    Asked::Failed(e) => return fail(e),
                },
            }
        }
        if let NewUrl::Var(var) = &wanted.url {
            let Some(source) = self.source else {
                return fail("nothing was changed");
            };
            match url_from_var(source, var, NothingWas::Changed) {
                Ok(url) => wanted.url = NewUrl::Literal(url),
                Err(e) => return fail(e),
            }
        }
        let mut key = match &wanted.key {
            NewKey::Keep => current_key.clone(),
            NewKey::Prompt => match self.prompter {
                None => {
                    return fail(
                        "--replace-key asks for the new key on a terminal; without one, use --key-stdin or --key-var",
                    );
                }
                Some(prompter) => match ask_key(prompter, &super::EDIT) {
                    Asked::Answer(key) => Some(key),
                    Asked::Cancelled => return cancelled(prompter),
                    Asked::Declined => return declined(),
                    Asked::Failed(e) => return fail(e),
                },
            },
            NewKey::Stdin => {
                if std::io::stdin().is_terminal() {
                    return usage_fail(
                        "--key-stdin reads the key from a pipe, not a terminal; use --replace-key to be asked for it",
                    );
                }
                match read_stdin_key() {
                    Ok(key) => Some(key),
                    Err(e) => return fail(e),
                }
            }
            NewKey::Var(var) => {
                let Some(source) = self.source else {
                    return fail("nothing was changed");
                };
                match key_from_var(source, var, NothingWas::Changed) {
                    Ok(key) => Some(key),
                    Err(e) => return fail(e),
                }
            }
        };
        let mut note = None;
        if probes {
            loop {
                let Some(current) = key.clone() else {
                    return fail(ChangeError::NoRecord(self.name.clone()));
                };
                let draft = Draft {
                    provider: wanted.provider.unwrap_or(self.endpoint.provider),
                    url: self.custom_url(&wanted.url, record_url.as_deref()),
                    auth: wanted.auth.unwrap_or(self.endpoint.auth),
                    key: None,
                };
                eprint!(
                    "Checking it with one count_tokens request (free on Anthropic's side; a gateway may count it)\u{2026} "
                );
                let outcome = probe(
                    &probe_endpoint(&draft, &current, self.path, self.shown),
                    self.ctx,
                );
                match &outcome {
                    Ok(o) => eprintln!("{o}"),
                    Err(e) => eprintln!("not checked: {e}"),
                }
                match after_check(outcome, self.prompter, draft.url.is_none()) {
                    Asked::Answer(Checked::Save(saved)) => {
                        note = saved;
                        break;
                    }
                    Asked::Answer(Checked::Retry(Retry::Again)) => {}
                    Asked::Answer(Checked::Retry(Retry::Key)) => {
                        let Some(prompter) = self.prompter else {
                            return fail("nothing was changed");
                        };
                        match ask_key(prompter, &super::EDIT) {
                            Asked::Answer(new) => {
                                key = Some(new);
                                wanted.key = NewKey::Prompt;
                            }
                            Asked::Cancelled => return cancelled(prompter),
                            Asked::Declined => return declined(),
                            Asked::Failed(e) => return fail(e),
                        }
                    }
                    Asked::Answer(Checked::Retry(Retry::Url)) => {
                        let Some(prompter) = self.prompter else {
                            return fail("nothing was changed");
                        };
                        match ask_url(prompter) {
                            Asked::Answer(url) => wanted.url = NewUrl::Literal(url),
                            Asked::Cancelled => return cancelled(prompter),
                            Asked::Declined => return declined(),
                            Asked::Failed(e) => return fail(e),
                        }
                    }
                    Asked::Answer(Checked::Stop(why)) => {
                        return fail(why.replace("nothing was saved", "nothing was changed"));
                    }
                    Asked::Declined => return declined(),
                    Asked::Cancelled => match self.prompter {
                        Some(prompter) => return cancelled(prompter),
                        None => return fail("nothing was changed"),
                    },
                    Asked::Failed(e) => return fail(e),
                }
            }
        }
        let key_change = wanted.key != NewKey::Keep;
        let writes_record = key_change
            || matches!(wanted.url, NewUrl::Literal(_))
            || (wanted.url == NewUrl::Official && record_url.is_some());
        let (new_record_url, config_url) = match &wanted.url {
            NewUrl::Keep | NewUrl::Var(_) => (record_url.clone(), None),
            NewUrl::Literal(url) => (Some(url.clone()), self.endpoint.url.as_ref().map(|_| None)),
            NewUrl::Official => (None, self.endpoint.url.as_ref().map(|_| None)),
        };
        let record = match (writes_record, key) {
            (true, Some(key)) => Some(StoredRecord::Key {
                key: Secret::new(key),
                url: new_record_url.map(Secret::new),
            }),
            (true, None) => return fail(ChangeError::NoRecord(self.name.clone())),
            (false, _) => None,
        };
        let edit = Edit {
            rename: wanted.rename.clone().filter(|new| new != self.name),
            provider: wanted.provider,
            auth: wanted.auth,
            pricing: wanted.pricing,
            record,
            url: config_url,
            store: wanted.store,
            ..Edit::default()
        };
        self.finish(edit, &wanted, note)
    }

    fn precheck(&self, wanted: &Wanted, writes_record: bool) -> Result<(), String> {
        let record = writes_record.then(|| StoredRecord::Key {
            key: Secret::new("placeholder-checked-before-the-key-is-read".to_owned()),
            url: None,
        });
        let url = match wanted.url {
            NewUrl::Keep => None,
            NewUrl::Literal(_) | NewUrl::Var(_) | NewUrl::Official => {
                self.endpoint.url.as_ref().map(|_| None)
            }
        };
        let change = Change::Edit {
            name: self.name.clone(),
            edit: Edit {
                rename: wanted.rename.clone().filter(|new| new != self.name),
                provider: wanted.provider,
                auth: wanted.auth,
                pricing: wanted.pricing,
                record,
                url,
                store: wanted.store,
                ..Edit::default()
            },
        };
        wisp_core::secrets::check(self.path, self.shown, &change, self.ctx.keychain.access())
            .map_err(|e| change_error(&e, self.home))
    }

    fn custom_url(&self, wanted: &NewUrl, record_url: Option<&str>) -> Option<String> {
        match wanted {
            NewUrl::Literal(url) => Some(url.clone()),
            NewUrl::Official => None,
            NewUrl::Keep | NewUrl::Var(_) => record_url
                .map(str::to_owned)
                .or_else(|| self.endpoint.url.clone()),
        }
    }

    fn ask_lost_url(&self, prompter: &Prompter) -> Asked<NewUrl> {
        let provider = self.endpoint.provider;
        let official = format!(
            "{}'s API ({})",
            provider.title(),
            http_url(provider.default_url())
                .and_then(|u| u.host_str().map(str::to_owned))
                .unwrap_or_default()
        );
        let lead = format!("Where does {} connect?", self.name);
        match answer!(choose(
            prompter,
            &lead,
            &[&official, "A gateway or proxy, at a URL you give"],
            0
        )) {
            0 => Asked::Answer(NewUrl::Official),
            _ => Asked::Answer(NewUrl::Literal(answer!(ask_url(prompter)))),
        }
    }
}
