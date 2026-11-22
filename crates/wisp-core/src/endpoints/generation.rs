mod provider;
pub use provider::Guarded;

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

#[derive(Debug, Clone, Default)]
pub struct Generation(Arc<Mutex<State>>);

#[derive(Debug, Default)]
struct State {
    uses: usize,
    bindings: Arc<Mutex<std::collections::BTreeMap<String, usize>>>,
    phase: Phase,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Phase {
    #[default]
    Ready,
    Changing,
    Closing,
    Retired,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Unavailable {
    #[error("switch account first (/account OTHER)")]
    RootBound,
    #[error("{}", bound_text(.0))]
    Bound(Vec<String>),
    #[error("this account is busy; cancel its work first and retry")]
    Busy,
    #[error("this account is being updated; retry after authentication completes")]
    Changing,
    #[error("this account's connection was replaced or removed; reload accounts before retrying")]
    Retired,
    #[error("this account was signed out in another window; /account chooses another")]
    Closing,
}

fn bound_text(agents: &[String]) -> String {
    let (noun, pronoun) = if agents.len() == 1 {
        ("agent", "it")
    } else {
        ("agents", "them")
    };
    format!(
        "used by {noun} {}; end {pronoun} first",
        wisp_tool_text::output::and_list(agents)
    )
}

fn ready(phase: Phase) -> Result<(), Unavailable> {
    match phase {
        Phase::Ready => Ok(()),
        Phase::Changing => Err(Unavailable::Changing),
        Phase::Closing => Err(Unavailable::Closing),
        Phase::Retired => Err(Unavailable::Retired),
    }
}

pub fn admit(generation: Option<Generation>) -> Result<Option<Admission>, Unavailable> {
    generation.map(|generation| generation.admit()).transpose()
}

impl Generation {
    fn state(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn available(&self) -> Result<(), Unavailable> {
        ready(self.state().phase)
    }

    pub fn admit(&self) -> Result<Admission, Unavailable> {
        let mut state = self.state();
        ready(state.phase)?;
        state.uses += 1;
        Ok(Admission(Arc::new(Held(self.clone()))))
    }

    pub fn bind(&self, path: &str) -> Result<Binding, Unavailable> {
        let state = self.state();
        ready(state.phase)?;
        *state
            .bindings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(path.to_owned())
            .or_default() += 1;
        Ok(Binding(Arc::new(BindingHeld {
            bindings: state.bindings.clone(),
            path: path.to_owned(),
        })))
    }

    pub fn change(&self) -> Result<Change, Unavailable> {
        self.change_inner(false)
    }
    pub fn remove(&self) -> Result<Change, Unavailable> {
        self.change_inner(true)
    }

    pub fn close(&self) -> Result<Change, Unavailable> {
        let mut state = self.state();
        ready(state.phase)?;
        state.phase = Phase::Closing;
        Ok(Change(self.clone()))
    }

    fn change_inner(&self, removing: bool) -> Result<Change, Unavailable> {
        let mut state = self.state();
        ready(state.phase)?;
        if removing {
            let bindings = state
                .bindings
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if bindings.contains_key("/") {
                return Err(Unavailable::RootBound);
            }
            if !bindings.is_empty() {
                return Err(Unavailable::Bound(bindings.keys().cloned().collect()));
            }
        }
        if state.uses != 0 {
            return Err(Unavailable::Busy);
        }
        state.phase = Phase::Changing;
        Ok(Change(self.clone()))
    }
}

#[derive(Debug, Clone)]
pub struct Binding(Arc<BindingHeld>);

#[derive(Debug)]
struct BindingHeld {
    bindings: Arc<Mutex<std::collections::BTreeMap<String, usize>>>,
    path: String,
}

impl Binding {
    pub fn covers(&self, generation: &Generation, path: &str) -> bool {
        self.0.path == path && Arc::ptr_eq(&self.0.bindings, &generation.state().bindings)
    }
}

impl Drop for BindingHeld {
    fn drop(&mut self) {
        let mut bindings = self.bindings.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(count) = bindings.get_mut(&self.path) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                bindings.remove(&self.path);
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Admission(Arc<Held>);

impl Admission {
    pub fn generation(&self) -> &Generation {
        &self.0.0
    }
}

#[derive(Debug)]
struct Held(Generation);

impl Drop for Held {
    fn drop(&mut self) {
        let mut state = self.0.state();
        state.uses = state.uses.saturating_sub(1);
        if state.phase == Phase::Closing && state.uses == 0 {
            state.phase = Phase::Retired;
        }
    }
}

#[derive(Debug)]
pub struct Change(Generation);

impl Change {
    pub(crate) fn replacement(&self) -> Generation {
        Generation(Arc::new(Mutex::new(State {
            bindings: self.0.state().bindings.clone(),
            ..State::default()
        })))
    }

    pub fn retire(self) {
        let mut state = self.0.state();
        if state.phase != Phase::Closing || state.uses == 0 {
            state.phase = Phase::Retired;
        }
    }
}

impl Drop for Change {
    fn drop(&mut self) {
        let mut state = self.0.state();
        if state.phase == Phase::Changing {
            state.phase = Phase::Ready;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloned_admissions_hold_the_generation_until_every_owner_releases() {
        let generation = Generation::default();
        let admitted = generation.admit().unwrap();
        let queued = admitted.clone();
        drop(admitted);
        assert!(matches!(generation.change(), Err(Unavailable::Busy)));
        drop(queued);
        let changing = generation.change().unwrap();
        assert!(matches!(generation.admit(), Err(Unavailable::Changing)));
        drop(changing);
        assert!(generation.admit().is_ok());
    }

    #[test]
    fn retirement_refuses_old_clones_and_does_not_poison_a_new_generation() {
        let generation = Generation::default();
        let old_clone = generation.clone();
        generation.change().unwrap().retire();
        assert!(matches!(old_clone.admit(), Err(Unavailable::Retired)));
        assert!(matches!(old_clone.change(), Err(Unavailable::Retired)));
        assert!(Generation::default().admit().is_ok());
    }

    #[test]
    fn every_entry_refuses_while_changing_and_after_retirement() {
        let generation = Generation::default();
        let refuses = |expected: Unavailable| {
            assert_eq!(generation.available(), Err(expected.clone()));
            assert_eq!(generation.admit().err(), Some(expected.clone()));
            assert_eq!(generation.bind("/").err(), Some(expected.clone()));
            assert_eq!(generation.change().err(), Some(expected.clone()));
            assert_eq!(generation.remove().err(), Some(expected));
        };
        let changing = generation.change().unwrap();
        refuses(Unavailable::Changing);
        changing.retire();
        refuses(Unavailable::Retired);

        let generation = Generation::default();
        let _bound = generation.bind("/").unwrap();
        let held = generation.admit().unwrap();
        let closing = generation.close().unwrap();
        assert_eq!(generation.admit().err(), Some(Unavailable::Closing));
        assert_eq!(generation.bind("/").err(), Some(Unavailable::Closing));
        assert_eq!(generation.close().err(), Some(Unavailable::Closing));
        closing.retire();
        assert_eq!(generation.available(), Err(Unavailable::Closing));
        drop(held);
        assert_eq!(generation.available(), Err(Unavailable::Retired));
    }

    #[test]
    fn admit_passes_unguarded_providers_and_refuses_retired_ones() {
        assert!(matches!(admit(None), Ok(None)));
        let generation = Generation::default();
        assert!(matches!(admit(Some(generation.clone())), Ok(Some(_))));
        generation.change().unwrap().retire();
        assert_eq!(admit(Some(generation)).err(), Some(Unavailable::Retired));
    }
}

#[cfg(test)]
mod binding_tests {
    use super::*;

    #[test]
    fn idle_child_binding_blocks_removal_but_not_safe_reauthorization() {
        let generation = Generation::default();
        let binding = generation.bind("/worker").unwrap();
        let single = generation.remove().unwrap_err();
        assert_eq!(single, Unavailable::Bound(vec!["/worker".to_owned()]));
        assert_eq!(single.to_string(), "used by agent /worker; end it first");
        let alpha = generation.bind("/alpha").unwrap();
        let both = generation.remove().unwrap_err();
        assert_eq!(
            both,
            Unavailable::Bound(vec!["/alpha".to_owned(), "/worker".to_owned()])
        );
        assert_eq!(
            both.to_string(),
            "used by agents /alpha and /worker; end them first"
        );
        let root = generation.bind("/").unwrap();
        assert_eq!(generation.remove().unwrap_err(), Unavailable::RootBound);
        drop(root);
        drop(generation.change().unwrap());
        drop((binding, alpha));
        assert!(generation.remove().is_ok());
    }
}

#[cfg(test)]
mod race_tests {
    use super::*;

    #[test]
    fn mutation_and_new_admission_are_mutually_exclusive() {
        for _ in 0..32 {
            let generation = Generation::default();
            let worker = generation.clone();
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let other = barrier.clone();
            let thread = std::thread::spawn(move || {
                other.wait();
                worker.admit()
            });
            barrier.wait();
            let changing = generation.change();
            let admitted = thread.join().unwrap();
            assert_ne!(changing.is_ok(), admitted.is_ok());
            drop(changing);
            drop(admitted);
            assert!(generation.admit().is_ok());
        }
        for _ in 0..32 {
            let generation = Generation::default();
            let worker = generation.clone();
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let other = barrier.clone();
            let thread = std::thread::spawn(move || {
                other.wait();
                worker.admit()
            });
            barrier.wait();
            let closing = generation.close().unwrap();
            let admitted = thread.join().unwrap();
            closing.retire();
            assert_eq!(
                generation.available().err(),
                Some(match admitted {
                    Ok(_) => Unavailable::Closing,
                    Err(_) => Unavailable::Retired,
                })
            );
            drop(admitted);
            assert_eq!(generation.available(), Err(Unavailable::Retired));
        }
    }
}
