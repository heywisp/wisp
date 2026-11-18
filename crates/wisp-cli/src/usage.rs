use std::collections::BTreeSet;
use std::ffi::OsString;

use clap::FromArgMatches;
use clap::builder::{PossibleValuesParser, TypedValueParser};
use clap::error::{ContextKind, ContextValue, ErrorKind};
use wisp_protocol::settings::{Form, SETTINGS, SettingValue, find, one_of};
use wisp_protocol::{CacheTtl, Effort, MessageTime};

pub(crate) const USAGE_EXIT: u8 = 2;

pub(crate) fn choices<T: Copy + Send + Sync + 'static>(
    all: &'static [T],
    name: fn(T) -> &'static str,
) -> impl TypedValueParser<Value = T> {
    let fallback = all[0];
    PossibleValuesParser::new(all.iter().map(|v| name(*v))).map(move |text| {
        all.iter()
            .copied()
            .find(|v| name(*v) == text)
            .unwrap_or(fallback)
    })
}

pub(crate) fn effort() -> impl TypedValueParser<Value = Effort> {
    choices(&Effort::ALL, Effort::as_str)
}

pub(crate) fn cache_ttl() -> impl TypedValueParser<Value = CacheTtl> {
    choices(&CacheTtl::ALL, CacheTtl::as_str)
}

pub(crate) fn message_time() -> impl TypedValueParser<Value = MessageTime> {
    choices(&MessageTime::ALL, MessageTime::as_str)
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Failure {
    pub(crate) text: String,
    pub(crate) code: i32,
    pub(crate) stdout: bool,
}

#[cfg(test)]
pub(crate) fn parse_from<T, I>(args: I) -> Result<T, Failure>
where
    T: clap::CommandFactory + FromArgMatches,
    I: IntoIterator<Item = OsString>,
{
    parse_with::<T, I>(crate::tools::command::<T>(), args)
}

pub(crate) fn parse_with<T, I>(command: clap::Command, args: I) -> Result<T, Failure>
where
    T: FromArgMatches,
    I: IntoIterator<Item = OsString>,
{
    let args: Vec<OsString> = args.into_iter().collect();
    let root = command.clone();
    let mut command = command;
    let parsed = command
        .try_get_matches_from_mut(args.iter().cloned())
        .and_then(|mut matches| T::from_arg_matches_mut(&mut matches));
    parsed.map_err(|error| failure(&error, &root, &args))
}

fn takes_value(arg: &clap::Arg) -> bool {
    arg.get_action().takes_values()
}

fn walk(root: &clap::Command, args: &[OsString]) -> String {
    let mut path = vec![root.get_name().to_owned()];
    let mut current = root;
    let mut globals: Vec<&clap::Arg> = Vec::new();
    let mut rest = args.iter().skip(1);
    while let Some(arg) = rest.next() {
        let Some(text) = arg.to_str() else {
            continue;
        };
        if text == "--" {
            break;
        }
        let known = |arg: &&clap::Arg| match text.strip_prefix("--") {
            Some(long) => arg.get_long() == Some(long),
            None => {
                let mut chars = text.chars().skip(1);
                matches!((chars.next(), chars.next()), (Some(c), None) if arg.get_short() == Some(c))
            }
        };
        let flag = (text.starts_with('-') && text.len() > 1).then(|| {
            current
                .get_arguments()
                .chain(globals.iter().copied())
                .find(known)
        });
        match flag {
            Some(Some(found)) if takes_value(found) => {
                rest.next();
            }
            Some(_) => {}
            None => {
                if let Some(sub) = current.find_subcommand(text) {
                    globals.extend(current.get_arguments().filter(|a| a.is_global_set()));
                    path.push(sub.get_name().to_owned());
                    current = sub;
                }
            }
        }
    }
    path.join(" ")
}

fn failure(error: &clap::Error, root: &clap::Command, args: &[OsString]) -> Failure {
    match error.kind() {
        ErrorKind::DisplayHelp
        | ErrorKind::DisplayVersion
        | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => Failure {
            text: error.render().to_string(),
            code: error.exit_code(),
            stdout: !error.use_stderr(),
        },
        _ => Failure {
            text: message(error, root, &walk(root, args), args),
            code: i32::from(USAGE_EXIT),
            stdout: false,
        },
    }
}

fn strings(value: Option<&ContextValue>) -> Vec<String> {
    match value {
        Some(ContextValue::String(one)) => vec![one.clone()],
        Some(ContextValue::Strings(many)) => many.clone(),
        Some(ContextValue::StyledStr(text)) => vec![text.to_string()],
        Some(ContextValue::StyledStrs(texts)) => texts.iter().map(ToString::to_string).collect(),
        _ => Vec::new(),
    }
}

fn command_path(error: &clap::Error) -> Option<String> {
    let usage = strings(error.get(ContextKind::Usage));
    let first = usage
        .first()
        .and_then(|u| u.lines().find(|l| !l.trim().is_empty()).map(str::to_owned))
        .unwrap_or_default();
    let words: Vec<&str> = first
        .trim()
        .trim_start_matches("Usage:")
        .split_whitespace()
        .take_while(|w| !w.starts_with(['[', '<', '-']) && *w != "...")
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

fn arg_name(display: &str) -> String {
    display
        .split_whitespace()
        .next()
        .unwrap_or(display)
        .trim_matches(['[', ']', '<', '>'])
        .to_owned()
}

fn without_list(flag: &str) -> String {
    SETTINGS
        .iter()
        .find(|spec| spec.flags.contains(&flag))
        .map_or_else(
            || "got a value it can't use".to_owned(),
            |spec| spec.describe(Form::Rule),
        )
}

pub(crate) fn setting(name: &'static str) -> impl TypedValueParser<Value = SettingValue> {
    move |text: &str| -> Result<SettingValue, String> {
        let spec = find(name).ok_or_else(|| name.to_owned())?;
        spec.parse(text).map_err(|e| e.rule)
    }
}

fn hidden_key_given(root: &clap::Command, walked: &str, raw: &[OsString]) -> bool {
    let mut current = root;
    for word in walked.split(' ').skip(1) {
        match current.find_subcommand(word) {
            Some(sub) => current = sub,
            None => return false,
        }
    }
    let longs: Vec<String> = current
        .get_arguments()
        .filter(|arg| {
            arg.is_hide_set()
                && arg
                    .get_value_names()
                    .is_some_and(|names| names.iter().any(|n| n == "KEY"))
        })
        .filter_map(|arg| arg.get_long().map(|long| format!("--{long}")))
        .collect();
    raw.iter().skip(1).filter_map(|a| a.to_str()).any(|text| {
        let flag = text.split_once('=').map_or(text, |(flag, _)| flag);
        longs.iter().any(|long| long == flag)
    })
}

fn message(error: &clap::Error, root: &clap::Command, walked: &str, raw: &[OsString]) -> String {
    let command = command_path(error).unwrap_or_else(|| walked.to_owned());
    let args: Vec<String> = strings(error.get(ContextKind::InvalidArg))
        .iter()
        .map(|a| arg_name(a))
        .collect();
    let flag = args.first().cloned();
    if hidden_key_given(root, walked, raw) {
        return format!("wisp: {}", crate::manage::KEY_ON_COMMAND_LINE);
    }
    let mut text = match error.kind() {
        ErrorKind::UnknownArgument => format!(
            "{command}: unexpected argument (not shown, in case it is a key); see {command} --help"
        ),
        ErrorKind::InvalidSubcommand => format!(
            "{command}: unknown command (not shown, in case it is a key); see {command} --help"
        ),
        ErrorKind::InvalidValue
        | ErrorKind::ValueValidation
        | ErrorKind::TooManyValues
        | ErrorKind::TooFewValues
        | ErrorKind::WrongNumberOfValues
        | ErrorKind::NoEquals
        | ErrorKind::InvalidUtf8 => {
            let valid = strings(error.get(ContextKind::ValidValue));
            match (&flag, valid.is_empty()) {
                (Some(flag), false) => format!(
                    "{command}: {flag} must be {} (what was given isn't shown)",
                    one_of(&valid)
                ),
                (Some(flag), true) => format!(
                    "{command}: {flag} {} (what was given isn't shown)",
                    without_list(flag)
                ),
                (None, _) => format!(
                    "{command}: a value it can't use (not shown, in case it is a key); see {command} --help"
                ),
            }
        }
        ErrorKind::MissingRequiredArgument => {
            format!(
                "{command}: missing {}; see {command} --help",
                args.join(", ")
            )
        }
        ErrorKind::MissingSubcommand => {
            format!("{command}: a command is needed; see {command} --help")
        }
        ErrorKind::ArgumentConflict => {
            let prior = strings(error.get(ContextKind::PriorArg))
                .first()
                .map(|p| arg_name(p));
            let names = (flag.as_deref(), prior.as_deref());
            let named = match names {
                (Some("--from-environment"), Some(other))
                | (Some(other), Some("--from-environment"))
                    if other == "--key-env" || other == "--url-env" =>
                {
                    Some(other.to_owned())
                }
                _ => None,
            };
            match (&flag, prior, named) {
                (_, _, Some(other)) => format!(
                    "{command}: {other} names a variable read at each start; --from-environment is for a one-time import with --key-var"
                ),
                (Some(a), Some(b), None) if *a == b => {
                    format!("{command}: {a} was given more than once")
                }
                (Some(a), Some(b), None) => format!("{command}: {a} can't be used with {b}"),
                _ => format!("{command}: these flags can't be used together; see {command} --help"),
            }
        }
        _ => format!(
            "{command}: the command line isn't valid (not shown, in case it holds a key); see {command} --help"
        ),
    };
    let known = Known::of(root);
    for (kind, what, names) in [
        (ContextKind::SuggestedArg, "argument", &known.args),
        (
            ContextKind::SuggestedSubcommand,
            "subcommand",
            &known.commands,
        ),
        (ContextKind::SuggestedValue, "value", &known.values),
    ] {
        for tip in strings(error.get(kind)) {
            if names.contains(&tip) {
                text.push_str(&format!("\n\n  tip: a similar {what} exists: {tip}"));
            }
        }
    }
    text
}

#[derive(Default)]
struct Known {
    args: BTreeSet<String>,
    commands: BTreeSet<String>,
    values: BTreeSet<String>,
}

impl Known {
    fn of(root: &clap::Command) -> Self {
        let mut known = Self::default();
        known.add(root);
        known
    }

    fn add(&mut self, command: &clap::Command) {
        for arg in command.get_arguments() {
            if let Some(long) = arg.get_long() {
                self.args.insert(format!("--{long}"));
            }
            if let Some(short) = arg.get_short() {
                self.args.insert(format!("-{short}"));
            }
            for value in arg.get_possible_values() {
                self.values.insert(value.get_name().to_owned());
            }
        }
        for sub in command.get_subcommands() {
            self.commands.insert(sub.get_name().to_owned());
            self.add(sub);
        }
    }
}

#[cfg(test)]
mod tests;
