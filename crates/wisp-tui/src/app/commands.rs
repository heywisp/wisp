use wisp_protocol::COMPACT_BETWEEN_TURNS;
use wisp_protocol::plugin::{CommandArg, CommandBusy, PluginEntry};
use wisp_render::transcript::copy::REWIND_BETWEEN_TURNS;
use wisp_render::transcript::copy::REWIND_SUMMARY;
use wisp_render::transcript::copy::{RESUME_BETWEEN_TURNS, RESUME_SUMMARY};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandId {
    Usage,
    Login,
    Logout,
    Model,
    Effort,
    Endpoint,
    Context,
    Compact,
    Rewind,
    Config,
    Processes,
    Agents,
    Sidebar,
    Help,
    Exit,
    Leave,
    Resume,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Models,
    Efforts,
    Endpoints,
    Settings,
    Words(&'static [(&'static str, &'static str)]),
    Sidebar(&'static [(&'static str, &'static str)]),
    Free,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Arg {
    pub name: &'static str,
    pub source: Source,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Busy {
    Runs,
    BetweenTurns(&'static str),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InChat {
    Agent,
    Over,
    Main,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Command {
    pub id: CommandId,
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub args: &'static [Arg],
    pub summary: &'static str,
    pub busy: Busy,
    pub chat: InChat,
}

pub const CONTEXT_WORDS: &[(&str, &str)] = &[("all", "List each item")];
pub const SIDEBAR_WORDS: &[(&str, &str)] = &[("open", "Open it over the transcript")];

pub const COMMANDS: &[Command] = &[
    Command {
        id: CommandId::Model,
        name: "model",
        aliases: &[],
        args: &[Arg {
            name: "id",
            source: Source::Models,
        }],
        summary: "Switch the model",
        busy: Busy::Runs,
        chat: InChat::Agent,
    },
    Command {
        id: CommandId::Effort,
        name: "effort",
        aliases: &[],
        args: &[Arg {
            name: "level",
            source: Source::Efforts,
        }],
        summary: "Set how hard the model thinks",
        busy: Busy::Runs,
        chat: InChat::Agent,
    },
    Command {
        id: CommandId::Endpoint,
        name: "account",
        aliases: &[],
        args: &[Arg {
            name: "name",
            source: Source::Endpoints,
        }],
        summary: "Switch the chat's account",
        busy: Busy::Runs,
        chat: InChat::Agent,
    },
    Command {
        id: CommandId::Context,
        name: "context",
        aliases: &[],
        args: &[Arg {
            name: "detail",
            source: Source::Words(CONTEXT_WORDS),
        }],
        summary: "Show what fills the context",
        busy: Busy::Runs,
        chat: InChat::Agent,
    },
    Command {
        id: CommandId::Compact,
        name: "compact",
        aliases: &[],
        args: &[Arg {
            name: "focus",
            source: Source::Free,
        }],
        summary: "Summarize to free up context",
        busy: Busy::BetweenTurns(COMPACT_BETWEEN_TURNS),
        chat: InChat::Agent,
    },
    Command {
        id: CommandId::Rewind,
        name: "rewind",
        aliases: &["undo"],
        args: &[],
        summary: REWIND_SUMMARY,
        busy: Busy::BetweenTurns(REWIND_BETWEEN_TURNS),
        chat: InChat::Agent,
    },
    Command {
        id: CommandId::Config,
        name: "config",
        aliases: &[],
        args: &[Arg {
            name: "filter",
            source: Source::Settings,
        }],
        summary: "View and change settings",
        busy: Busy::Runs,
        chat: InChat::Main,
    },
    Command {
        id: CommandId::Processes,
        name: "ps",
        aliases: &[],
        args: &[],
        summary: "Show background processes",
        busy: Busy::Runs,
        chat: InChat::Over,
    },
    Command {
        id: CommandId::Agents,
        name: "agents",
        aliases: &[],
        args: &[],
        summary: "Show agents and their spend",
        busy: Busy::Runs,
        chat: InChat::Main,
    },
    Command {
        id: CommandId::Sidebar,
        name: "sidebar",
        aliases: &[],
        args: &[Arg {
            name: "open",
            source: Source::Sidebar(SIDEBAR_WORDS),
        }],
        summary: "Toggle sidebar, open a page",
        busy: Busy::Runs,
        chat: InChat::Main,
    },
    Command {
        id: CommandId::Help,
        name: "help",
        aliases: &[],
        args: &[],
        summary: "Show commands and keys",
        busy: Busy::Runs,
        chat: InChat::Over,
    },
    Command {
        id: CommandId::Exit,
        name: "exit",
        aliases: &["quit"],
        args: &[],
        summary: "Quit, ending the chat",
        busy: Busy::Runs,
        chat: InChat::Main,
    },
    Command {
        id: CommandId::Leave,
        name: "leave",
        aliases: &[],
        args: &[],
        summary: "Leave; the chat stays open",
        busy: Busy::Runs,
        chat: InChat::Main,
    },
    Command {
        id: CommandId::Resume,
        name: "resume",
        aliases: &[],
        args: &[Arg {
            name: "filter",
            source: Source::Free,
        }],
        summary: RESUME_SUMMARY,
        busy: Busy::BetweenTurns(RESUME_BETWEEN_TURNS),
        chat: InChat::Main,
    },
    Command {
        id: CommandId::Usage,
        name: "usage",
        aliases: &[],
        args: &[Arg {
            name: "name",
            source: Source::Endpoints,
        }],
        summary: "Open live usage page",
        busy: Busy::Runs,
        chat: InChat::Main,
    },
    Command {
        id: CommandId::Login,
        name: "login",
        aliases: &[],
        args: &[
            Arg {
                name: "provider",
                source: Source::Free,
            },
            Arg {
                name: "name",
                source: Source::Endpoints,
            },
        ],
        summary: "Sign in to an account",
        busy: Busy::BetweenTurns("/login runs between turns."),
        chat: InChat::Main,
    },
    Command {
        id: CommandId::Logout,
        name: "logout",
        aliases: &[],
        args: &[Arg {
            name: "name",
            source: Source::Free,
        }],
        summary: "Sign out of an account",
        busy: Busy::Runs,
        chat: InChat::Main,
    },
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginCommand {
    pub plugin: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub args: Vec<CommandArg>,
    pub summary: String,
    pub between_turns: Option<String>,
    pub short: bool,
}

impl PluginCommand {
    fn bases(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.name.as_str()).chain(self.aliases.iter().map(String::as_str))
    }

    pub fn names(&self) -> Vec<String> {
        let full = self.bases().map(|base| self.full(base));
        if self.short {
            self.bases().map(str::to_owned).chain(full).collect()
        } else {
            full.collect()
        }
    }

    pub fn shown(&self, full: bool) -> Vec<String> {
        if full || !self.short {
            self.bases().map(|base| self.full(base)).collect()
        } else {
            self.bases().map(str::to_owned).collect()
        }
    }

    pub fn full(&self, base: &str) -> String {
        format!("{}:{base}", self.plugin)
    }

    pub fn base_of(&self, word: &str) -> Option<&str> {
        let base = match word.split_once(':') {
            Some((plugin, base)) if plugin.eq_ignore_ascii_case(&self.plugin) => base,
            Some(_) => return None,
            None if self.short => word,
            None => return None,
        };
        self.bases().find(|name| name.eq_ignore_ascii_case(base))
    }

    pub fn synopsis_args(&self) -> String {
        self.args
            .iter()
            .map(|arg| format!(" [{}]", arg.name))
            .collect()
    }
}

pub fn plugin_commands(plugins: &[PluginEntry]) -> Vec<PluginCommand> {
    plugins
        .iter()
        .filter(|plugin| plugin.installed && plugin.enabled)
        .flat_map(|plugin| {
            plugin.commands.iter().map(|command| PluginCommand {
                plugin: plugin.id.clone(),
                name: command.name.clone(),
                aliases: command.aliases.clone(),
                args: command.args.clone(),
                summary: command.summary.clone(),
                between_turns: match &command.busy {
                    CommandBusy::BetweenTurns { hint } => Some(hint.clone()),
                    CommandBusy::Runs | CommandBusy::Unknown => None,
                },
                short: command.short,
            })
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    BuiltIn(&'static Command),
    Plugin(PluginCommand),
}

impl Entry {
    pub fn key(&self) -> String {
        match self {
            Entry::BuiltIn(command) => command.name.to_owned(),
            Entry::Plugin(command) => command.full(&command.name),
        }
    }

    pub fn names(&self) -> Vec<String> {
        match self {
            Entry::BuiltIn(command) => names(command).map(str::to_owned).collect(),
            Entry::Plugin(command) => command.names(),
        }
    }

    pub fn word_for(&self, typed: &str) -> String {
        match self {
            Entry::BuiltIn(command) => command.name.to_owned(),
            Entry::Plugin(command)
                if !command.short || matched(self, typed).is_some_and(|hit| hit.contains(':')) =>
            {
                command.full(&command.name)
            }
            Entry::Plugin(command) => command.name.clone(),
        }
    }

    pub fn takes_args(&self) -> bool {
        match self {
            Entry::BuiltIn(command) => !command.args.is_empty(),
            Entry::Plugin(command) => !command.args.is_empty(),
        }
    }

    pub fn between_turns(&self) -> bool {
        match self {
            Entry::BuiltIn(command) => matches!(command.busy, Busy::BetweenTurns(_)),
            Entry::Plugin(command) => command.between_turns.is_some(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Parsed<'a> {
    Command {
        id: CommandId,
        args: &'a str,
    },
    Plugin {
        command: &'a PluginCommand,
        base: &'a str,
        args: &'a str,
    },
    Unknown(&'a str),
    Message,
}

pub fn is_name(word: &str) -> bool {
    let mut chars = word.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '-')
}

pub fn is_word(word: &str) -> bool {
    match word.split_once(':') {
        Some((plugin, name)) => is_name(plugin) && (name.is_empty() || is_name(name)),
        None => is_name(word),
    }
}

pub fn names(command: &Command) -> impl Iterator<Item = &'static str> {
    std::iter::once(command.name).chain(command.aliases.iter().copied())
}

pub const HIDDEN_ALIASES: &[(CommandId, &str)] = &[(CommandId::Endpoint, "endpoint")];

pub fn accepted(command: &Command) -> impl Iterator<Item = &'static str> {
    let id = command.id;
    names(command).chain(
        HIDDEN_ALIASES
            .iter()
            .filter(move |(hidden, _)| *hidden == id)
            .map(|(_, name)| *name),
    )
}

pub fn parse<'a>(plugins: &'a [PluginCommand], text: &'a str, attachments: bool) -> Parsed<'a> {
    parse_in(COMMANDS, plugins, text, attachments)
}

pub fn parse_in<'a>(
    commands: &[Command],
    plugins: &'a [PluginCommand],
    text: &'a str,
    attachments: bool,
) -> Parsed<'a> {
    if attachments || text.contains(['\n', '\r']) {
        return Parsed::Message;
    }
    let Some(rest) = text.strip_prefix('/') else {
        return Parsed::Message;
    };
    let (word, args) = match rest.split_once(char::is_whitespace) {
        Some((word, args)) => (word, args.trim()),
        None => (rest, ""),
    };
    if !is_word(word) || word.ends_with(':') {
        return Parsed::Message;
    }
    if let Some(command) = commands
        .iter()
        .find(|c| accepted(c).any(|n| n.eq_ignore_ascii_case(word)))
    {
        return Parsed::Command {
            id: command.id,
            args,
        };
    }
    plugins
        .iter()
        .find_map(|command| {
            command.base_of(word).map(|base| Parsed::Plugin {
                command,
                base,
                args,
            })
        })
        .unwrap_or(Parsed::Unknown(word))
}

pub fn get(id: CommandId) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.id == id)
}

pub fn between_turns(id: CommandId) -> Option<&'static str> {
    match get(id).map(|c| c.busy) {
        Some(Busy::BetweenTurns(hint)) => Some(hint),
        Some(Busy::Runs) | None => None,
    }
}

pub fn in_chat(id: CommandId) -> InChat {
    get(id).map_or(InChat::Agent, |c| c.chat)
}

pub fn matching(word: &str) -> Vec<&'static Command> {
    matching_in(COMMANDS, word)
}

pub fn matching_in(commands: &'static [Command], word: &str) -> Vec<&'static Command> {
    matching_entries(commands, &[], word)
        .into_iter()
        .filter_map(|entry| match entry {
            Entry::BuiltIn(command) => Some(command),
            Entry::Plugin(_) => None,
        })
        .collect()
}

pub fn matching_entries(
    commands: &'static [Command],
    plugins: &[PluginCommand],
    word: &str,
) -> Vec<Entry> {
    let word = word.to_ascii_lowercase();
    let (exact, rest): (Vec<Entry>, Vec<Entry>) = commands
        .iter()
        .map(Entry::BuiltIn)
        .chain(plugins.iter().cloned().map(Entry::Plugin))
        .partition(|entry| entry.names().contains(&word));
    exact
        .into_iter()
        .chain(
            rest.into_iter()
                .filter(|entry| entry.names().iter().any(|n| n.starts_with(&word))),
        )
        .collect()
}

pub fn matched(entry: &Entry, word: &str) -> Option<String> {
    let word = word.to_ascii_lowercase();
    let names = entry.names();
    names
        .iter()
        .find(|n| **n == word)
        .or_else(|| names.iter().find(|n| n.starts_with(&word)))
        .cloned()
}

pub fn synopsis(command: &Command) -> String {
    let mut out = format!("/{}", command.name);
    for arg in command.args {
        match arg.source {
            Source::Words(words) | Source::Sidebar(words) => {
                for (word, _) in words {
                    out.push_str(&format!(" [{word}]"));
                }
            }
            Source::Models
            | Source::Efforts
            | Source::Endpoints
            | Source::Settings
            | Source::Free => {
                out.push_str(&format!(" [{}]", arg.name));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests;
