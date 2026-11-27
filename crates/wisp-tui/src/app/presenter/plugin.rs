use std::time::Duration;

use wisp_protocol::plugin::CommandOutcome;
use wisp_protocol::{AgentEvent, AgentPath, ClientCommand, RequestId, Secret};
use wisp_render::transcript::copy;

use super::Presenter;
use crate::app::commands::{PluginCommand, plugin_commands};

impl Presenter {
    fn run_plugin(&mut self, name: String, args: String, at: Duration) -> Option<RequestId> {
        let request = self.next_request();
        let arrival = self.shown_parts().inputs.arrive();
        let command = ClientCommand::RunPluginCommand {
            name: name.clone(),
            args: wisp_protocol::Secret::new(args),
            request,
        };
        if self.aimed(command, at) {
            let parts = self.shown_parts();
            parts.transcript.command_asked(request, &name);
            parts.runs.insert(request, arrival);
            Some(request)
        } else {
            None
        }
    }

    pub(super) fn typed_plugin(
        &mut self,
        command: &PluginCommand,
        base: &str,
        spelling: &str,
        args: &str,
        busy: bool,
        at: Duration,
    ) -> bool {
        let Some(masked) = wisp_protocol::masked_command(args) else {
            self.shown_composer_mut().clear();
            self.suggest.reset();
            self.hint(copy::KEY_SHAPED_COMMAND, at);
            return false;
        };
        if busy && let Some(hint) = &command.between_turns {
            self.hint(hint, at);
            return false;
        }
        self.shown_composer_mut().clear();
        let Some(request) = self.run_plugin(command.full(base), args.to_owned(), at) else {
            return false;
        };
        let mut parts = self.shown_parts();
        let command = format!("/{spelling}");
        if args.is_empty() {
            echo_command(&mut parts, &command);
        } else {
            let echo = Secret::new(format!("{command} {masked}"));
            parts.plugin_echo.insert(request, (command, echo));
        }
        true
    }

    pub(super) fn commanding(&self) -> bool {
        !self.plugin_runs.is_empty()
    }

    pub(super) fn plugin_event(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::CatalogChanged { catalog } => {
                catalog.declare();
                self.suggest.set_plugins(plugin_commands(&catalog.plugins));
                self.facts.search.follow(&catalog.plugins);
                self.facts.providers_off = wisp_render::chrome::providers_off(&catalog.plugins);
            }
            AgentEvent::SettingsSnapshot(snapshot) => {
                wisp_protocol::Catalog {
                    plugins: snapshot.plugins.clone(),
                }
                .declare();
                self.suggest.set_plugins(plugin_commands(&snapshot.plugins));
                self.facts.search.follow(&snapshot.plugins);
                self.facts.providers_off = wisp_render::chrome::providers_off(&snapshot.plugins);
            }
            _ => self.plugin_answered(None, event),
        }
    }

    pub(super) fn plugin_answered(&mut self, agent: Option<&AgentPath>, event: &AgentEvent) {
        let mut parts = self.parts_mut(agent);
        match event {
            AgentEvent::CommandDone {
                answers,
                outcome,
                args_safe_to_echo,
            } => {
                if let Some((base, echo)) = parts.plugin_echo.remove(answers) {
                    echo_command(
                        &mut parts,
                        if *args_safe_to_echo {
                            echo.expose()
                        } else {
                            &base
                        },
                    );
                }
                let arrival = parts.runs.remove(answers);
                if let (Some(arrival), CommandOutcome::Sent { input }) = (arrival, outcome) {
                    parts.inputs.place_reply(input, arrival);
                }
            }
            AgentEvent::Rejected {
                request: Some(request),
                ..
            } => {
                parts.runs.remove(request);
                if let Some((base, _)) = parts.plugin_echo.remove(request) {
                    echo_command(&mut parts, &base);
                }
            }
            _ => {}
        }
    }
}

fn echo_command(parts: &mut super::Parts<'_>, echo: &str) {
    parts
        .composer
        .remember(&crate::widgets::composer::Saved::from_text(echo));
    parts.transcript.echo_command(echo);
}
