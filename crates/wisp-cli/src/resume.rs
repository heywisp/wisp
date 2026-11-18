use wisp_core::agents::endpoints::EndpointView;
use wisp_core::endpoints::goes::shown as shown_name;
use wisp_render::chrome::account::Said;
#[cfg(test)]
use wisp_render::chrome::account::SaidWords;
pub(crate) use wisp_start::resume::*;
use wisp_tui::app::SessionFacts;
use wisp_tui::app::resumed::StartNote;
use wisp_tui::app::setup::Ask;

pub(crate) mod pick;

pub(crate) const UNLOCKED_HELP: &str = "Resume a session whose log is on a filesystem without file locks. If the session is still open in another Wisp, both write to the log, their records collide on `seq` and the whole log becomes unreadable; and truncating a cut last line can destroy a line the other Wisp is writing. Use it only when you know the session has ended";

#[cfg(test)]
pub(crate) use wisp_core::kit::logged_root as root;

#[cfg(test)]
pub(crate) fn refresh_priced(facts: &mut SessionFacts, agent: &wisp_core::agent::Agent) {
    wisp_start::resume::refresh_priced(
        facts
            .endpoints
            .iter_mut()
            .map(|endpoint| (endpoint.name.as_str(), &mut endpoint.priced)),
        agent,
    );
}

pub(crate) trait Shown {
    fn started(&self, to: &str, marks: Marks<'_>) -> Option<StartNote>;
}

impl Shown for Left {
    fn started(&self, to: &str, marks: Marks<'_>) -> Option<StartNote> {
        let name = who(&self.name, marks);
        let said = |said: Said| Some(StartNote::Said(said));
        match self.state.as_ref()? {
            State::Off => said(self.said(marks)),
            State::Expired | State::SignedOut => {
                Some(StartNote::SignIn(shown_name(&self.name).to_owned()))
            }
            State::Unusable(_, words) => said(Said::State(name, words.clone())),
            State::Unread(_) => said(Said::State(name, self.why())),
            State::Gone => {
                let (name, from) = self.missing(marks);
                said(Said::StartsOn(name, from, who(to, marks)))
            }
        }
    }
}

pub(crate) trait Asked {
    fn asked(self, facts: SessionFacts, view: &EndpointView, size: Option<u64>) -> Ask;
}

impl Asked for Question {
    fn asked(self, facts: SessionFacts, view: &EndpointView, size: Option<u64>) -> Ask {
        let ways = self.ways(|name| (view.ready(name), facts.billing_on(name)));
        Ask {
            lead: self.left.lead(&|name| facts.mark_of(name)),
            resumed: self.left.session.is_some(),
            from: self.left.billing,
            ways,
            again: matches!(self.left.state, Some(State::Expired | State::SignedOut))
                .then(|| self.left.name.clone()),
            facts,
            size,
            back: None,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
