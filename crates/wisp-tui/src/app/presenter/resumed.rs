use tokio::time::Instant;

use wisp_protocol::log::FeatureState;
use wisp_protocol::subscription_auth::{Followed, Stayed};
use wisp_protocol::{AgentEvent, AgentPath, ClientCommand, Feature, InputId};
use wisp_render::chrome::account::{Move, Moved, SignedOut};

use super::Presenter;
use crate::app::picker::Picker;
use crate::app::resumed::{Continuing, History, Resumed, StartNote, returned_note};
use crate::app::settings::Answer;
use crate::app::subscription_login::login_hint;
use crate::widgets::composer::Saved;
use wisp_render::text::sanitize;
use wisp_render::transcript::{Choice as Shown, copy};

impl Presenter {
    pub(super) fn started_on(&mut self, note: Option<StartNote>) {
        let Some(note) = note else {
            return;
        };
        let name = self.facts.endpoint_name.clone().unwrap_or_default();
        let (reason, again_with) = match note {
            StartNote::Said(line) => (Some(line), None),
            StartNote::SignIn(name) => (None, Some(login_hint(&name))),
        };
        let chat = Moved {
            how: Move::Started,
            state: self.facts.account_state(&name),
            from: None,
            model: self.facts.short_model(),
            effort: (!self.facts.effort.is_empty()).then(|| self.facts.effort.clone()),
            rebuild: None,
            mark: self.facts.mark_of(&name),
            name,
        };
        self.transcript.signed_out(SignedOut {
            chat: Some(chat),
            reason,
            again_with,
            ..SignedOut::default()
        });
    }

    pub(super) fn open_start_pick(&mut self) {
        let Some(pick) = self.facts.start_pick.take() else {
            return;
        };
        self.tracker.picks_start();
        let on = self.tracker.wanted().endpoint;
        let mark = self.facts.mark_of(&on);
        self.open_accounts(Picker::starting(pick, &on, mark), self.at(Instant::now()));
    }

    pub(super) fn show_start_notes(&mut self, notes: &[String]) {
        self.view.sync(self.transcript.cells().len());
        for note in notes {
            self.view.note(vec![sanitize(note)]);
        }
    }

    pub(super) fn show_resumed(&mut self, resumed: Resumed, notes: &[String]) {
        let at = self.at(Instant::now());
        self.continuing = resumed.continuing.clone().map(|continuing| Held {
            continuing,
            agents: (Vec::new(), Vec::new()),
        });
        let mut view = resumed.view;
        let ids: Vec<InputId> = view.returned.iter().filter_map(|r| r.id.clone()).collect();
        view.returned.retain(|r| {
            r.id.as_ref()
                .is_none_or(|id| self.returned.insert(id.clone()))
        });
        self.inputs
            .seed_ids(view.input_ids.iter().map(String::as_str));
        let mut history =
            History::new(self.glyphs, view.started.clone()).retried_on(view.retried_on.clone());
        for event in &view.events {
            if let AgentEvent::FeatureStateChanged {
                endpoint,
                feature: Feature::NativeWebSearch,
                state: FeatureState::FallenBack,
                ..
            } = event
            {
                self.tracker.search_refused_before(endpoint);
            }
            if let AgentEvent::FeatureStateChanged {
                endpoint,
                model,
                feature: Feature::PerMessageEffort,
                state,
            } = event
            {
                self.tracker.set_pair(endpoint, model, *state);
            }
            if let AgentEvent::AgentRenamed { agent, to, .. } = event {
                self.agents.renamed(agent, to);
            }
            if matches!(event, AgentEvent::ProcessRestored { .. }) {
                self.feed_processes(&AgentPath::root(), event, at);
                self.transcript.replay(event, at);
                continue;
            }
            if history.apply(event, &self.facts, &mut self.transcript) {
                continue;
            }
            if matches!(event, AgentEvent::Rewound { .. }) {
                self.rewind_before(None, event);
            }
            self.transcript.replay(event, at);
            if let Some(cut) = self.transcript.take_cut() {
                self.view.cut(cut);
            }
            self.status.replayed(event);
            if matches!(
                event,
                AgentEvent::TurnStarted { .. } | AgentEvent::TurnEnded { .. }
            ) {
                let offered = self.retry.failed().is_some();
                let retried = matches!(
                    event,
                    AgentEvent::TurnStarted {
                        retry_of: Some(_),
                        ..
                    }
                );
                self.retry_event(event, at);
                if offered && retried {
                    let used = copy::retried(None);
                    self.transcript
                        .set_choice(self.retry.cell(), Some(Shown::Used(used)));
                }
                if let Some(note) = history.take_retry_note() {
                    self.transcript.retrying(&note);
                }
                history.turn_edge(event, &mut self.transcript);
            }
        }
        self.inputs.seed_mail(history.mail_drawn());
        self.transcript.resume_done();
        if self.tracker.show(&mut self.facts) {
            self.transcript.set_provider(&self.facts.provider);
            self.view.refresh_banner();
        }
        let glyphs = self.glyphs;
        let saved: Vec<Saved> = view
            .returned
            .iter()
            .map(|r| {
                let data = r.images.iter().map(|i| i.data.clone()).collect();
                Saved::with_images(&r.text, data, |i, n| {
                    r.images
                        .get(i)
                        .map(|image| {
                            copy::image_chip(
                                n,
                                (image.width, image.height),
                                wisp_media::tokens(image.width, image.height),
                                glyphs,
                            )
                        })
                        .unwrap_or_default()
                })
            })
            .collect();
        if !saved.is_empty() {
            self.composer.restore_ahead(&saved);
            self.typed_changed(at);
        }
        if !ids.is_empty()
            && let Some(commands) = &self.commands
        {
            let _ = commands.try_send(ClientCommand::ReturnedTaken { ids });
        }
        self.show_start_notes(notes);
        if let Some(note) = returned_note(&view.returned) {
            self.transcript.note(&note);
        }
        self.view.sync(self.transcript.cells().len());
        self.frames.mark();
    }

    pub(super) fn resumed_agents(&mut self, event: &AgentEvent) {
        let AgentEvent::SessionResumed { moved, stay, .. } = event else {
            return;
        };
        if let Some(held) = &mut self.continuing {
            held.agents = (moved.clone(), stay.clone());
        } else if !moved.is_empty() || !stay.is_empty() {
            let dest = self.tracker.wanted().endpoint;
            self.transcript.signed_out(SignedOut {
                agents: self.followed(moved, &dest, &[]),
                kept: self.kept(stay, &dest),
                not_resumed: self.not_resumed(stay),
                ..SignedOut::default()
            });
        }
    }

    pub(super) fn continued(&mut self, answer: Answer) -> Answer {
        let Answer::Moved(mut moved, after) = answer else {
            return answer;
        };
        let Some(held) = self.continuing.take() else {
            return Answer::Moved(moved, after);
        };
        let Held {
            continuing,
            agents: (followed, stay),
        } = held;
        moved.how = Move::Continuing;
        moved.from = continuing.from;
        moved.rebuild = continuing.rebuild.or(moved.rebuild);
        let out = SignedOut {
            agents: self.followed(&followed, &moved.name, &[]),
            kept: self.kept(&stay, &moved.name),
            not_resumed: self.not_resumed(&stay),
            reason: continuing.reason,
            chat: Some(*moved),
            ..SignedOut::default()
        };
        self.transcript.signed_out(out);
        Answer::Notes(after)
    }
}

#[derive(Debug)]
pub(super) struct Held {
    continuing: Continuing,
    agents: (Vec<Followed>, Vec<Stayed>),
}
