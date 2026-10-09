//! Drain native inference before replacing shared GPU resources.

use std::sync::{Condvar, Mutex, PoisonError};

#[derive(Default)]
pub(super) struct InferenceGate {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Default)]
struct State {
    active: usize,
    replacing: bool,
}

pub(super) struct InferenceGuard<'a>(&'a InferenceGate);
pub(super) struct ReplacementGuard<'a>(&'a InferenceGate);

impl InferenceGate {
    pub(super) fn enter(&self) -> InferenceGuard<'_> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        while state.replacing {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
        state.active += 1;
        InferenceGuard(self)
    }

    pub(super) fn replace(&self) -> ReplacementGuard<'_> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        while state.replacing {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
        // Stop new inference before waiting, so queued calls cannot starve cleanup.
        state.replacing = true;
        self.changed.notify_all();
        while state.active > 0 {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
        ReplacementGuard(self)
    }
}

impl Drop for InferenceGuard<'_> {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.active -= 1;
        self.0.changed.notify_all();
    }
}

impl Drop for ReplacementGuard<'_> {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.replacing = false;
        self.0.changed.notify_all();
    }
}

#[cfg(test)]
mod tests;
