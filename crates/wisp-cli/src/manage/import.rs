use std::collections::BTreeSet;
use std::io::IsTerminal;
use std::process::ExitCode;

use clap::Args;
use wisp_core::config_file::{ConfigText, NewEndpoint};
use wisp_core::endpoints::{CheckOutcome, EndpointName, PricingMode};
use wisp_core::secrets::import::{Found, Origin as FoundOrigin, Other, Source, find};
use wisp_core::secrets::{
    Change, Edit, FileStore, Outcome, Secret, StoreKind, StoredRecord, VariableName, check_key,
};
use wisp_render::chrome::endpoint_card::{
    FORBIDDEN_NOTE, NOT_OFFERED_NOTE, SAVE_ANYWAY, check_failed,
};
use wisp_tui::term::prompt::{PromptError, Prompter};

use super::{
    Applied, Asked, BOTH_SOURCES, Context, Draft, Fallback, Origin, Picked, Saving, StoreChoice,
    VarSource, apply_change, cancelled, choose, confirm, credentials_words, early_store_check,
    fail, pick_store, probe, probe_endpoint, report, save_stored, store_words, stores,
    suggested_name, usage_fail, var_source,
};
use crate::endpoints::short;

#[cfg_attr(test, derive(Debug))]
#[derive(Args)]
pub(crate) struct ImportArgs {
    #[arg(
        long,
        help = "Read the variables from the environment, once, instead of a keys file"
    )]
    from_environment: bool,
    #[arg(long, value_name = "STORE", value_parser = stores(), help = concat!("Where the keys are kept: auto (the default: the keychain where this machine has one, else the file), keyring (the system keychain) or file (", wisp_protocol::credentials_beside_config!(), ", readable only by you)"))]
    store: Option<StoreChoice>,
    #[arg(
        long,
        help = "Store without the count_tokens check (the local key check still runs)"
    )]
    no_check: bool,
    #[arg(
        long,
        help = "Without a terminal, also import a token whose URL variable is only in the other source; it is used with the provider's own URL"
    )]
    no_url: bool,
}

struct Chosen {
    source: VarSource,
    default_file: bool,
}

fn choose_source(
    args: &ImportArgs,
    ctx: &Context<'_>,
    home: Option<&str>,
) -> Result<Chosen, ExitCode> {
    if args.from_environment || ctx.env_file.is_some() {
        return match var_source(ctx, args.from_environment) {
            Ok(Some(source)) => Ok(Chosen {
                source,
                default_file: false,
            }),
            Ok(None) => Err(fail("nothing to import from")),
            Err(e) if e == BOTH_SOURCES => Err(usage_fail(e)),
            Err(e) => Err(fail(e)),
        };
    }
    default_source(ctx, home)
}

fn default_source(ctx: &Context<'_>, home: Option<&str>) -> Result<Chosen, ExitCode> {
    let Some(path) = ctx.env_file_path() else {
        return Err(fail(
            "no home directory: set HOME or XDG_CONFIG_HOME, or pass --env-file FILE or --from-environment",
        ));
    };
    match wisp_core::secrets::import::read_file(&path, false) {
        Ok(Some(file)) => Ok(Chosen {
            source: VarSource {
                words: short(&path, home),
                origin: Origin::File(file),
            },
            default_file: true,
        }),
        Ok(None) => Err(fail(format!(
            "there is no {} to import from; give --env-file FILE or --from-environment",
            short(&path, home)
        ))),
        Err(e) => Err(fail(e)),
    }
}

fn label(found: &Found, words: &str) -> String {
    match &found.origin {
        FoundOrigin::Provider => format!("{} in {words}", found.names()),
        FoundOrigin::Table(name) => format!("{name} ({}) in {words}", found.names()),
    }
}

fn ask_name(
    prompter: &Prompter,
    found: &Found,
    suggested: &EndpointName,
    taken: &BTreeSet<EndpointName>,
) -> Asked<EndpointName> {
    loop {
        let answer = match prompter.visible(&format!("Name for {} [{suggested}]: ", found.names()))
        {
            Ok(answer) => answer,
            Err(PromptError::Cancelled) => return Asked::Cancelled,
            Err(e) => return Asked::from(e),
        };
        let answer = answer.trim();
        if answer.is_empty() {
            return Asked::Answer(suggested.clone());
        }
        match super::parse_name(answer) {
            Ok(name) if taken.contains(&name) => eprintln!("\"{name}\" is taken; choose another."),
            Ok(name) => return Asked::Answer(name),
            Err(e) => eprintln!("{e}"),
        }
    }
}

enum Verdict {
    Save(Option<&'static str>),
    Skip(String),
}

fn check(
    found: &Found,
    key: &str,
    ctx: &Context<'_>,
    path: &std::path::Path,
    shown: wisp_core::endpoints::PathShown,
    prompter: Option<&Prompter>,
) -> Asked<Verdict> {
    let custom = found
        .url
        .as_ref()
        .map(|u| u.expose().to_owned())
        .or_else(|| found.config_url.clone());
    let draft = Draft {
        provider: found.provider,
        url: custom,
        auth: found.auth,
        key: None,
    };
    eprint!(
        "Checking it with one count_tokens request (free on Anthropic's side; a gateway may count it)\u{2026} "
    );
    let outcome = probe(&probe_endpoint(&draft, key, path, shown), ctx.model);
    let reason = match &outcome {
        Ok(o) => {
            eprintln!("{o}");
            o.to_string()
        }
        Err(e) => {
            eprintln!("not checked: {e}");
            format!("not checked: {e}")
        }
    };
    match outcome {
        Ok(CheckOutcome::Ok) => return Asked::Answer(Verdict::Save(None)),
        Ok(CheckOutcome::Forbidden) => {
            return Asked::Answer(Verdict::Save(Some(FORBIDDEN_NOTE)));
        }
        Ok(CheckOutcome::NotOffered) => {
            return Asked::Answer(Verdict::Save(Some(NOT_OFFERED_NOTE)));
        }
        _ => {}
    }
    let Some(prompter) = prompter else {
        return Asked::Answer(Verdict::Skip(format!("the check failed ({reason})")));
    };
    match choose(
        prompter,
        &check_failed(&reason),
        &["Skip it", SAVE_ANYWAY],
        0,
    ) {
        Asked::Answer(0) => Asked::Answer(Verdict::Skip(format!("the check failed ({reason})"))),
        Asked::Answer(_) => Asked::Answer(Verdict::Save(None)),
        Asked::Cancelled => Asked::Cancelled,
        Asked::Declined => Asked::Declined,
        Asked::Failed(e) => Asked::Failed(e),
    }
}

#[derive(Default)]
pub(crate) struct Options {
    store: Option<StoreChoice>,
    no_check: bool,
    no_url: bool,
}

pub(crate) fn run(args: &ImportArgs, ctx: &Context<'_>) -> ExitCode {
    let home = ctx.home_text();
    let chosen = match choose_source(args, ctx, home.as_deref()) {
        Ok(chosen) => chosen,
        Err(code) => return code,
    };
    let options = Options {
        store: args.store,
        no_check: args.no_check,
        no_url: args.no_url,
    };
    import(&options, ctx, chosen, &mut ImportRun::default())
}

#[derive(Default)]
pub(crate) struct ImportRun {
    seen: Vec<Secret>,
    fallback: Fallback,
    stopped: bool,
    spans_places: bool,
}

impl ImportRun {
    pub(crate) fn across_places() -> Self {
        Self {
            spans_places: true,
            ..Self::default()
        }
    }

    pub(crate) fn stored_any(&self) -> bool {
        !self.seen.is_empty()
    }

    pub(crate) fn stopped(&self) -> bool {
        self.stopped
    }

    fn already_stored(&self, key: &Secret) -> bool {
        self.seen.contains(key)
    }

    pub(crate) fn save(
        &mut self,
        saving: &Saving<'_>,
        picked: &Picked,
        make: &dyn Fn(StoreKind) -> Change,
        key: &Secret,
    ) -> Result<(Applied, StoreKind), ExitCode> {
        match save_stored(saving, picked, &mut self.fallback, make) {
            Ok(saved) => {
                self.seen.push(key.clone());
                Ok(saved)
            }
            Err(code) => {
                self.stopped = true;
                Err(code)
            }
        }
    }
}

pub(crate) fn offer(
    ctx: &Context<'_>,
    from: crate::presence::Place,
    run: &mut ImportRun,
) -> ExitCode {
    let home = ctx.home_text();
    let chosen = match from {
        crate::presence::Place::Environment => Chosen {
            source: VarSource {
                origin: Origin::Environment,
                words: "the environment".to_owned(),
            },
            default_file: false,
        },
        crate::presence::Place::File => match default_source(ctx, home.as_deref()) {
            Ok(chosen) => chosen,
            Err(code) => return code,
        },
    };
    import(&Options::default(), ctx, chosen, run)
}

fn import(args: &Options, ctx: &Context<'_>, chosen: Chosen, run: &mut ImportRun) -> ExitCode {
    let home = ctx.home_text();
    let home = home.as_deref();
    let (path, shown) = match ctx.target(false) {
        Ok(target) => target,
        Err(e) => return fail(e),
    };
    let (existed, config) =
        match ConfigText::read(&path, shown).and_then(|t| t.current().map(|c| (t, c))) {
            Ok((text, config)) => (text.existed(), config),
            Err(e) => return fail(e),
        };
    let store = FileStore::beside(&path, shown);
    if let Err(e) = early_store_check(&store, home) {
        return fail(e);
    }
    let picked = match pick_store(args.store, ctx, &credentials_words(&store, home)) {
        Ok(picked) => picked,
        Err(e) => return fail(e),
    };
    let default_file = ctx.env_file_path().and_then(|p| {
        wisp_core::secrets::import::read_file(&p, false)
            .ok()
            .flatten()
    });
    let env_has = |var: &VariableName| {
        wisp_core::secrets::import::read_var(
            wisp_core::secrets::import::Place::Environment(&wisp_start::paths::real_env),
            var,
        )
        .is_some()
    };
    let file_has = |var: &VariableName| {
        default_file
            .as_ref()
            .and_then(|f| f.vars.get(var.raw()))
            .is_some_and(|v| !v.trim().is_empty())
    };
    let other = match &chosen.source.origin {
        Origin::Environment => default_file.as_ref().map(|f| {
            let words = short(&f.path, home);
            Other {
                has: &file_has,
                hint: format!("--env-file {words}"),
                words,
            }
        }),
        Origin::File(_) => Some(Other {
            has: &env_has,
            words: "the environment".to_owned(),
            hint: "--from-environment".to_owned(),
        }),
    };
    let words = chosen.source.words.clone();
    let source = Source {
        from: chosen.source.place(),
        words: words.clone(),
    };
    let config_ref = existed.then_some(&config);
    let mut findings = find(config_ref, &source, other.as_ref());
    let before = findings.found.len();
    findings.found.retain(|f| !run.already_stored(&f.key));
    if findings.found.is_empty() && findings.skipped.is_empty() && before > 0 {
        println!("Nothing more to import from {words}: its keys were stored above.");
        return ExitCode::SUCCESS;
    }
    if findings.found.is_empty() && findings.skipped.is_empty() {
        println!(
            "Nothing to import: no provider key and no key the config names is set in {words}."
        );
        return ExitCode::FAILURE;
    }
    if !findings.found.is_empty() {
        println!("Found:");
        for found in &findings.found {
            println!("  {}", label(found, &words));
        }
    }
    for skipped in &findings.skipped {
        println!("Skipped {}: {}", skipped.what, skipped.reason);
    }
    let interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    let prompter = ctx.prompter("Cancelled; nothing more was imported.");
    let prompter = interactive.then_some(&prompter);
    let mut taken: BTreeSet<EndpointName> = config.endpoints.keys().cloned().collect();
    let mut accepted: Vec<(EndpointName, Found, String)> = Vec::new();
    let mut failed = false;
    for found in findings.found {
        let shown_label = label(&found, &words);
        if let Some(warning) = &found.warning {
            eprintln!("warning: {warning}");
            let go = match prompter {
                Some(prompter) => match confirm(prompter, "Use the provider's own URL?", false) {
                    Asked::Answer(go) => go,
                    Asked::Cancelled => return cancelled(prompter),
                    Asked::Declined => false,
                    Asked::Failed(e) => return fail(e),
                },
                None => args.no_url,
            };
            if !go {
                println!("Skipped {shown_label}: {warning}");
                failed |= prompter.is_none();
                continue;
            }
        }
        let name = match (&found.origin, prompter) {
            (FoundOrigin::Table(name), _) => name.clone(),
            (FoundOrigin::Provider, None) => suggested_name(found.provider, None, &taken),
            (FoundOrigin::Provider, Some(prompter)) => {
                let url = found.url.as_ref().map(Secret::expose);
                let suggested = suggested_name(found.provider, url, &taken);
                match ask_name(prompter, &found, &suggested, &taken) {
                    Asked::Answer(name) => name,
                    Asked::Cancelled => return cancelled(prompter),
                    Asked::Declined => continue,
                    Asked::Failed(e) => return fail(e),
                }
            }
        };
        if let Some(prompter) = prompter {
            match confirm(prompter, &format!("Import {shown_label}?"), true) {
                Asked::Answer(true) => {}
                Asked::Answer(false) | Asked::Declined => continue,
                Asked::Cancelled => return cancelled(prompter),
                Asked::Failed(e) => return fail(e),
            }
        }
        let key = match check_key(found.key.expose()) {
            Ok(key) => key.to_owned(),
            Err(problem) => {
                println!("Skipped {shown_label}: {problem}");
                failed = true;
                continue;
            }
        };
        if !args.no_check {
            match check(&found, &key, ctx, &path, shown, prompter) {
                Asked::Answer(Verdict::Save(note)) => {
                    if let Some(note) = note {
                        println!("{note}");
                    }
                }
                Asked::Answer(Verdict::Skip(why)) => {
                    println!("Skipped {shown_label}: {why}");
                    failed = true;
                    continue;
                }
                Asked::Cancelled => match prompter {
                    Some(prompter) => return cancelled(prompter),
                    None => return fail("nothing more was imported"),
                },
                Asked::Declined => continue,
                Asked::Failed(e) => return fail(e),
            }
        }
        taken.insert(name.clone());
        accepted.push((name, found, key));
    }
    let file_words = credentials_words(&store, home);
    let mut imported: Vec<EndpointName> = Vec::new();
    let mut variables: BTreeSet<String> = BTreeSet::new();
    let keys = ctx.keychain.access();
    let mut saving = Saving {
        path: &path,
        shown,
        home,
        keys,
        prompter,
        many: accepted.len() > 1 || run.spans_places,
        saved_before: 0,
    };
    for (name, found, key) in accepted {
        let record = StoredRecord::Key {
            key: Secret::new(key),
            url: found.url.clone(),
        };
        let make = |store: StoreKind| match &found.origin {
            FoundOrigin::Provider => Change::Add {
                name: name.clone(),
                endpoint: NewEndpoint {
                    provider: found.provider,
                    auth: found.auth,
                    store,
                    pricing: PricingMode::List,
                },
                record: record.clone(),
                make_default: false,
            },
            FoundOrigin::Table(_) => Change::Edit {
                name: name.clone(),
                edit: Edit {
                    record: Some(record.clone()),
                    store: Some(store),
                    ..Edit::default()
                },
            },
        };
        saving.saved_before = imported.len().max(run.seen.len());
        let (applied, kind) = match run.save(&saving, &picked, &make, &found.key) {
            Ok(saved) => saved,
            Err(code) => return code,
        };
        println!(
            "Imported \"{name}\" ({}) into {}.",
            found.names(),
            store_words(kind, &file_words)
        );
        if let Outcome::Added { created: true, .. } = applied.outcome
            && shown == wisp_core::endpoints::PathShown::Given
        {
            println!("Created {}.", wisp_core::endpoints::GIVEN_CONFIG);
        }
        failed |= report(&applied, ctx, &path) != ExitCode::SUCCESS;
        variables.extend(found.variables.iter().map(|v| v.raw().to_owned()));
        imported.push(name);
    }
    if let (None, Some(first)) = (&config.default, imported.first()) {
        let change = Change::SetDefault {
            name: first.clone(),
        };
        match apply_change(&path, shown, change, home, keys) {
            Ok(applied) => {
                println!("\"{first}\" is now the default account.");
                failed |= report(&applied, ctx, &path) != ExitCode::SUCCESS;
            }
            Err(code) => return code,
        }
    }
    if !imported.is_empty()
        && let Origin::File(file) = &chosen.source.origin
    {
        let every = file.vars.keys().all(|k| variables.contains(k));
        match prompter {
            Some(prompter) if chosen.default_file && every => {
                let question =
                    format!("Delete {words} now? Its keys are stored, so Wisp no longer needs it.");
                match confirm(prompter, &question, false) {
                    Asked::Answer(true) => match std::fs::remove_file(&file.path) {
                        Ok(()) => println!("Deleted {words}."),
                        Err(e) => return fail(format!("could not delete {words}: {}", e.kind())),
                    },
                    Asked::Answer(false) | Asked::Declined => {}
                    Asked::Cancelled => return cancelled(prompter),
                    Asked::Failed(e) => return fail(e),
                }
            }
            _ => {
                let lines: Vec<String> = file
                    .vars
                    .keys()
                    .filter(|k| variables.contains(*k))
                    .map(|k| wisp_core::secrets::shown_variable(k).to_owned())
                    .collect();
                if !lines.is_empty() {
                    println!(
                        "Wisp no longer needs these lines in {words}: {}",
                        wisp_core::secrets::import::and_words(&lines)
                    );
                }
            }
        }
    }
    if imported.is_empty() {
        println!("Nothing was imported.");
        return ExitCode::FAILURE;
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
