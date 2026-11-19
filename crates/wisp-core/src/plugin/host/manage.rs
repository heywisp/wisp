use std::collections::{BTreeMap, BTreeSet};

use futures::FutureExt;
use futures::future::BoxFuture;
use wisp_plugin_api::host::CallContext;
use wisp_plugin_api::index::Kind as Entry;
use wisp_plugin_api::manage::{Action, Change, Kind, Manage};
use wisp_protocol::settings::{SettingValue, find, names};
use wisp_protocol::{Answer, Question, QuestionOption};
use wisp_tool_text::manage as text;

use super::{KernelHost, PluginHost, in_render, locked};
use crate::agent::checked;
use crate::call::user::{AskReply, Goes, SecretReply, UserChannel};
use crate::plugin::imports::Import;

pub(crate) const SEARCH_PLUGIN: &str = "search";

#[derive(Debug, Clone, Default)]
pub(crate) struct Managing {
    pub(crate) shell: bool,
    pub(crate) window: bool,
    pub(crate) child: bool,
    pub(crate) path: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Switching {
    pub(crate) runnable: BTreeSet<String>,
    pub(crate) natives: BTreeSet<String>,
    pub(crate) leaders: BTreeMap<String, String>,
    pub(crate) owners: BTreeMap<String, String>,
    pub(crate) finder: Option<String>,
    pub(crate) listed: BTreeSet<String>,
    pub(crate) own: BTreeSet<String>,
    pub(crate) tools_off: BTreeSet<String>,
    pub(crate) stuck: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Switch {
    Listed {
        name: String,
        off: bool,
        by_agent: bool,
    },
    Tools {
        name: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Decided {
    Done(Switch, String),
    Unchanged(String),
    Ask {
        question: String,
        then: Switch,
        done: String,
        alone: String,
    },
    Refused(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Approval {
    Approved,
    NotNow,
    Own(String),
    NoAnswer,
    Unavailable,
}

impl Switching {
    pub(crate) fn decide(
        &self,
        skills: &BTreeSet<String>,
        servers: &BTreeSet<String>,
        change: &Change,
    ) -> Decided {
        let name = match change.kind {
            Kind::Tool => self.leaders.get(&change.name).unwrap_or(&change.name),
            Kind::Skill | Kind::Server => &change.name,
        }
        .clone();
        if self.natives.contains(&name) {
            return Decided::Refused(text::native(&name));
        }
        if let Some(spec) = find(names::TURNED_OFF)
            && let Err(refusal) = checked(spec, &SettingValue::List(vec![name.clone()]))
        {
            return Decided::Refused(refusal);
        }
        let kind = text::kind_name(change.kind);
        let off = |by_agent| Switch::Listed {
            name: name.clone(),
            off: true,
            by_agent,
        };
        match change.action {
            Action::Off
                if self.listed.contains(&name)
                    || (change.kind == Kind::Tool && self.tools_off.contains(&name)) =>
            {
                Decided::Unchanged(text::already(&name, false))
            }
            Action::Off
                if (change.kind == Kind::Skill && skills.contains(&name))
                    || (change.kind == Kind::Server && servers.contains(&name)) =>
            {
                Decided::Done(off(true), text::now_off(&name))
            }
            Action::Off if change.kind == Kind::Tool && self.runnable.contains(&name) => {
                match self.owners.get(&name).filter(|o| *o == SEARCH_PLUGIN) {
                    Some(_) => Decided::Ask {
                        question: text::ask_off(&name, self.finder.as_ref() == Some(&name)),
                        then: off(false),
                        done: text::now_off(&name),
                        alone: text::stays(&name),
                    },
                    None => Decided::Done(off(true), text::now_off(&name)),
                }
            }
            Action::Off => Decided::Refused(text::nothing_named(kind, &name)),
            Action::On if change.kind == Kind::Tool && self.stuck.contains(&name) => {
                Decided::Refused(text::would_rebuild(&name))
            }
            Action::On if self.listed.contains(&name) => {
                let tools_off = change.kind == Kind::Tool && self.tools_off.contains(&name);
                let on = |by_agent| {
                    if tools_off {
                        Switch::Tools { name: name.clone() }
                    } else {
                        Switch::Listed {
                            name: name.clone(),
                            off: false,
                            by_agent,
                        }
                    }
                };
                if self.own.contains(&name) && !tools_off {
                    Decided::Done(on(true), text::now_on(&name))
                } else {
                    Decided::Ask {
                        question: text::ask_on(&name),
                        then: on(false),
                        done: text::now_on(&name),
                        alone: text::off_in_list(&name),
                    }
                }
            }
            Action::On if change.kind == Kind::Tool && self.tools_off.contains(&name) => {
                Decided::Ask {
                    question: text::ask_on(&name),
                    then: Switch::Tools { name: name.clone() },
                    done: text::now_on(&name),
                    alone: text::off_in_tools(&name),
                }
            }
            Action::On
                if (change.kind == Kind::Tool && self.runnable.contains(&name))
                    || (change.kind == Kind::Skill && skills.contains(&name))
                    || (change.kind == Kind::Server && servers.contains(&name)) =>
            {
                Decided::Unchanged(text::already(&name, true))
            }
            Action::On => Decided::Refused(text::nothing_named(kind, &name)),
            Action::Add | Action::Remove => Decided::Refused(text::nothing_named(kind, &name)),
        }
    }

    fn take(&mut self, switch: &Switch) {
        match switch {
            Switch::Listed {
                name,
                off,
                by_agent,
            } => {
                if *off {
                    self.listed.insert(name.clone());
                    if *by_agent {
                        self.own.insert(name.clone());
                    }
                } else {
                    self.listed.remove(name);
                    self.own.remove(name);
                    if self.owners.contains_key(name) {
                        self.runnable.insert(name.clone());
                    }
                }
            }
            Switch::Tools { name } => {
                self.tools_off.remove(name);
                self.listed.remove(name);
                self.own.remove(name);
                self.runnable.insert(name.clone());
            }
        }
    }
}

pub(crate) async fn approve(user: &UserChannel, question: String, approve_line: &str) -> Approval {
    let asked = Question {
        header: text::HEADER.to_owned(),
        question,
        options: vec![
            QuestionOption {
                label: text::APPROVE.to_owned(),
                description: approve_line.to_owned(),
            },
            QuestionOption {
                label: text::NOT_NOW.to_owned(),
                description: String::new(),
            },
        ],
        multi_select: false,
        source: None,
    };
    match user.ask(vec![asked]).await {
        AskReply::Answered { answers } => match answers.first() {
            Some(Answer {
                own: Some(words), ..
            }) if !words.trim().is_empty() => {
                Approval::Own(words.split_whitespace().collect::<Vec<_>>().join(" "))
            }
            Some(answer) if answer.chosen == [0] => Approval::Approved,
            Some(answer) if answer.chosen == [1] => Approval::NotNow,
            _ => Approval::NoAnswer,
        },
        AskReply::Unavailable => Approval::Unavailable,
        _ => Approval::NoAnswer,
    }
}

impl KernelHost {
    pub(crate) fn set_switching(&self, switching: Option<Switching>) {
        locked(&self.index).switching = switching;
    }

    pub(crate) fn set_managing(&self, managing: Managing) {
        locked(&self.index).managing = managing;
    }

    pub(crate) fn show_link(&self, link: wisp_protocol::plugin::LinkNote) {
        locked(&self.index).links.push(link);
    }

    pub(crate) fn take_links(&self) -> Vec<wisp_protocol::plugin::LinkNote> {
        std::mem::take(&mut locked(&self.index).links)
    }

    pub(crate) fn take_added_servers(&self) -> Vec<String> {
        std::mem::take(&mut locked(&self.index).added_servers)
    }

    pub(crate) fn take_switches(&self) -> Vec<Switch> {
        std::mem::take(&mut locked(&self.index).pending)
    }

    fn decide_switch(&self, change: &Change) -> Decided {
        let state = locked(&self.index);
        let Some(switching) = &state.switching else {
            return Decided::Refused(text::NOT_MAIN.to_owned());
        };
        let skills = state
            .entries
            .iter()
            .filter(|entry| entry.kind == Entry::Skill)
            .map(|entry| entry.name.clone())
            .collect();
        switching.decide(
            &skills,
            &self.mcp().rows().into_iter().map(|row| row.name).collect(),
            change,
        )
    }

    fn switched(&self, switch: Switch) {
        let mut state = locked(&self.index);
        if let Some(switching) = &mut state.switching {
            switching.take(&switch);
        }
        state.pending.push(switch);
    }
}

impl Manage for PluginHost {
    fn manage<'a>(
        &'a self,
        change: Change,
        ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, Result<String, String>> {
        async move {
            if wisp_protocol::looks_like_key(&change.name) {
                return Err(text::KEY_NAME.into());
            }
            if self.refuses(Import::ManageManage) {
                return Err(in_render());
            }
            let call = self.owned_call(Import::ManageManage, ctx, SEARCH_PLUGIN)?;
            if matches!(change.action, Action::Add | Action::Remove) {
                return self.manage_server(change, ctx, &call.user).await;
            }
            match self.kernel.decide_switch(&change) {
                Decided::Refused(refusal) => Err(refusal),
                Decided::Unchanged(done) => Ok(done),
                Decided::Done(switch, done) => {
                    self.kernel.switched(switch);
                    Ok(done)
                }
                Decided::Ask {
                    question,
                    then,
                    done,
                    alone,
                } => match approve(&call.user, question, text::FOR_THIS_CHAT).await {
                    Approval::Approved if !ctx.cancelled() => {
                        self.kernel.switched(then);
                        Ok(done)
                    }
                    Approval::Approved | Approval::NoAnswer => Ok(text::NO_ANSWER.to_owned()),
                    Approval::NotNow => Ok(text::CHOSE_NOT_NOW.to_owned()),
                    Approval::Own(words) => Ok(text::own_words(&words)),
                    Approval::Unavailable => Err(alone),
                },
            }
        }
        .boxed()
    }
}

impl PluginHost {
    async fn manage_server(
        &self,
        change: Change,
        ctx: &dyn CallContext,
        user: &UserChannel,
    ) -> Result<String, String> {
        if self.origin != wisp_plugin_api::manifest::Origin::Bundled {
            return Err("an installed plugin can't manage MCP servers".into());
        }
        if change.kind != Kind::Server {
            return Err(text::nothing_named(
                text::kind_name(change.kind),
                &change.name,
            ));
        }
        let name = &change.name;
        let add = change.action == Action::Add;
        let service = self.kernel.mcp();
        if change
            .keys
            .iter()
            .any(|key| !wisp_protocol::plain_variable(key))
        {
            return Err(wisp_tool_text::mcp::KEY_VARIABLE.into());
        }
        let rekey =
            add && !change.keys.is_empty() && service.rows().iter().any(|row| row.name == *name);
        let was_waiting = service
            .rows()
            .iter()
            .any(|row| row.name == *name && row.state == crate::mcp::State::NeedsSignIn);
        let words = if rekey {
            Vec::new()
        } else if add {
            let words = match change.from.as_deref() {
                Some(from) => {
                    let words = shlex::split(from).ok_or(wisp_tool_text::mcp::UNFINISHED_QUOTE)?;
                    let Some((command, args)) = words.split_first() else {
                        return Err(wisp_tool_text::mcp::MISSING_COMMAND.into());
                    };
                    crate::mcp::check_add(name, command, args)?;
                    words
                }
                None if !was_waiting => {
                    return Err(if service.rows().iter().any(|row| row.name == *name) {
                        text::not_waiting(name)
                    } else {
                        wisp_tool_text::mcp::MISSING_COMMAND.into()
                    });
                }
                None => Vec::new(),
            };
            if !was_waiting && service.rows().iter().any(|row| row.name == *name) {
                return Err(wisp_tool_text::mcp::NAME_EXISTS.into());
            }
            words
        } else {
            if !service.rows().iter().any(|row| row.name == *name) {
                return Err(wisp_tool_text::mcp::removed(name));
            }
            Vec::new()
        };
        let config = if rekey {
            Some(service.server_config(name)?)
        } else if add && !change.keys.is_empty() {
            let (command, args) = words
                .split_first()
                .ok_or(wisp_tool_text::mcp::MISSING_COMMAND)?;
            Some(crate::config::McpServer {
                command: command.clone(),
                args: args.to_vec(),
                keys: change.keys.clone(),
            })
        } else {
            None
        };
        if config.as_ref().is_some_and(|config| {
            !change.keys.is_empty() && crate::mcp::is_address(&config.command)
        }) {
            return Err(wisp_tool_text::mcp::ADDRESS_KEYS.into());
        }
        let asking = if rekey {
            change.keys.clone()
        } else if let Some(config) = &config {
            service.missing_keys(name, config).await?
        } else {
            Vec::new()
        };
        let managing = locked(&self.kernel.index).managing.clone();
        if managing.child && !asking.is_empty() {
            return Err(if rekey {
                wisp_tool_text::mcp::child_rekey(name, &asking)
            } else {
                wisp_tool_text::mcp::child_key(name)
            });
        }
        if !managing.shell {
            let alone = text::no_shell(if add { "add" } else { "remove" }, name);
            if managing.child {
                return Err(alone);
            }
            #[cfg(unix)]
            if let Some(command) = words
                .first()
                .filter(|command| !crate::mcp::is_address(command))
            {
                let program = if command.contains('/') {
                    std::path::Path::new(&ctx.cwd())
                        .join(command)
                        .to_string_lossy()
                        .into_owned()
                } else {
                    command.clone()
                };
                if crate::jobs::exec::find_program(&program).is_none() {
                    let command = std::path::Path::new(command)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("the command");
                    return Err(wisp_tool_text::mcp::not_installed(command));
                }
            }
            let question = if add {
                text::ask_add(
                    name,
                    if rekey {
                        "the server's key"
                    } else if was_waiting {
                        "the server's sign-in"
                    } else {
                        change.from.as_deref().unwrap_or_default()
                    },
                )
            } else {
                text::ask_remove(name)
            };
            let line = if add {
                text::ADD_SERVER_APPROVAL
            } else {
                text::REMOVE_SERVER_APPROVAL
            };
            match approve(user, question, line).await {
                Approval::Approved if !ctx.cancelled() => {}
                Approval::Approved | Approval::NoAnswer => return Ok(text::NO_ANSWER.into()),
                Approval::NotNow => return Ok(text::CHOSE_NOT_NOW.into()),
                Approval::Own(words) => return Ok(text::own_words(&words)),
                Approval::Unavailable => return Err(alone),
            }
        }
        if ctx.cancelled() {
            return Ok(text::NO_ANSWER.into());
        }
        let by = managing.child.then_some(managing.path.as_str());
        if !add {
            service.remove_by(name, by).await?;
            return Ok(wisp_tool_text::mcp::remove_done(name));
        }
        let mut typed = BTreeMap::new();
        for key in &asking {
            if ctx.cancelled() {
                return Ok(text::NO_ANSWER.into());
            }
            let goes = Goes::Server(wisp_protocol::SecretDestination {
                server: name.clone(),
                command: if rekey { None } else { change.from.clone() },
            });
            let reply = tokio::select! {
                biased;
                () = ctx.wait_cancelled() => return Ok(text::NO_ANSWER.into()),
                reply = user.secret(goes, None, key.clone(), wisp_tool_text::mcp::key_reason(name), wisp_protocol::Ends { start: 4, end: 4 }) => reply,
            };
            if ctx.cancelled() {
                return Ok(text::NO_ANSWER.into());
            }
            match reply {
                SecretReply::Entered(value) => {
                    typed.insert(key.clone(), value);
                }
                SecretReply::Busy | SecretReply::ProcessEnded => return Ok(text::NO_ANSWER.into()),
                SecretReply::Unavailable => {
                    return Err(if rekey {
                        wisp_tool_text::mcp::no_rekey_client(name, &asking)
                    } else {
                        wisp_tool_text::mcp::no_key_client(name, &asking)
                    });
                }
            }
        }
        let mut kept = true;
        if rekey {
            let result = service
                .set_keys_until(name, typed, ctx.wait_cancelled())
                .await?;
            return Ok(if result.kept {
                wisp_tool_text::mcp::rekey_done(name, &change.keys)
            } else {
                wisp_tool_text::mcp::key_not_kept(name)
            });
        }
        if let Some(config) = config {
            self.check_server(name, &config.command, &config.args)?;
            let result = service
                .add_keys_by(name, config, typed, by, ctx.wait_cancelled())
                .await?;
            kept = result.kept;
        } else if !was_waiting {
            let (command, args) = words
                .split_first()
                .ok_or(wisp_tool_text::mcp::MISSING_COMMAND)?;
            tokio::select! {
                biased;
                () = ctx.wait_cancelled() => return Ok(text::NO_ANSWER.into()),
                result = self.add_server(name, command, args, by) => { result?; },
            }
        }
        if !managing.child {
            locked(&self.kernel.index).added_servers.push(name.clone());
        }
        if !kept {
            return Ok(wisp_tool_text::mcp::key_not_kept(name));
        }
        let waiting = service
            .rows()
            .iter()
            .any(|row| row.name == *name && row.state == crate::mcp::State::NeedsSignIn);
        if waiting {
            if !managing.child || was_waiting {
                service.note(crate::mcp::Note::SignIn(name.clone()));
            }
            if managing.child {
                return Ok(text::child_waiting(name));
            }
            if service.sign_in_start(name).await?.is_some() {
                if !managing.window {
                    return Ok(text::waiting_unshown(name));
                }
                service.await_sign_in(name);
                return Ok(text::added_waiting(name));
            }
        }
        Ok(if managing.child {
            format!("{name} added; {}", text::CHILD_ADDED)
        } else {
            format!("{name} added")
        })
    }
}
