//! One account's transfer admission, cancellation, progress and completion.
use std::{
    fmt::Display,
    future::Future,
    sync::{Arc, Mutex, PoisonError},
};

use super::{CancelToken, TransferState};

/// Storefronts retain their progress fields and terminal message vocabulary.
pub trait TransferStatus: Clone + Send + 'static {
    fn finish(&mut self, state: TransferState, error: Option<String>);
}

#[derive(Clone, Copy, Debug)]
pub struct TransferBusy;

/// Admits one transfer at a time, including outstanding blocking work after
/// its caller disappears. Progress remains readable without an async lock.
pub struct Transfers<P> {
    state: Arc<Mutex<State<P>>>,
}

struct State<P> {
    progress: Option<P>,
    active: Option<Active>,
}

struct Active {
    cancel: CancelToken,
    terminal: Option<(TransferState, Option<String>)>,
}

impl<P> Default for Transfers<P> {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                progress: None,
                active: None,
            })),
        }
    }
}

impl<P: TransferStatus> Transfers<P> {
    pub fn snapshot(&self) -> Option<P> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.reap();
        state.progress.clone()
    }

    pub fn cancel(&self) {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(active) = &state.active {
            active.cancel.cancel();
        }
    }

    /// The operation and its progress callbacks share an attempt identity.
    /// Dropping this future cancels that attempt. Registered blocking work
    /// retains admission until it has really stopped, even after a drop.
    pub async fn run<T, E, F, Fut>(&self, initial: P, operation: F) -> Result<T, E>
    where
        E: From<TransferBusy> + Display,
        F: FnOnce(TransferContext<P>) -> Fut,
        Fut: Future<Output = Result<T, E>>,
    {
        let context = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.reap();
            if state.active.is_some() {
                return Err(TransferBusy.into());
            }
            let cancel = CancelToken::new();
            state.active = Some(Active {
                cancel: cancel.clone(),
                terminal: None,
            });
            state.progress = Some(initial);
            TransferContext {
                state: Arc::clone(&self.state),
                cancel,
            }
        };
        let mut owner = Owner(Some(context.clone()));
        let result = operation(context.clone()).await;
        // Close publication before waiting for any outstanding work. Neither a
        // late callback nor explicit cancellation can rewrite this outcome.
        let terminal = match &result {
            Ok(_) => (TransferState::Complete, None),
            Err(error) => (
                if context.cancel.is_cancelled() {
                    TransferState::Cancelled
                } else {
                    TransferState::Failed
                },
                Some(error.to_string()),
            ),
        };
        context.finish(terminal);
        context.cancel.wait_for_workers().await;
        owner.0.take();
        // Terminal state is published together with releasing admission.
        self.snapshot();
        result
    }
}

impl<P: TransferStatus> State<P> {
    fn reap(&mut self) {
        let Some(active) = &mut self.active else {
            return;
        };
        if active.cancel.workers_finished()
            && let Some((phase, error)) = active.terminal.take()
        {
            if let Some(progress) = &mut self.progress {
                progress.finish(phase, error);
            }
            self.active = None;
        }
    }
}

/// Attempt-scoped progress publication. Keeping a callback alive does not hold
/// admission; callbacks from cancelled or older attempts are ignored.
#[derive(Clone)]
pub struct TransferContext<P> {
    state: Arc<Mutex<State<P>>>,
    cancel: CancelToken,
}

impl<P: TransferStatus> TransferContext<P> {
    pub fn cancel_token(&self) -> &CancelToken {
        &self.cancel
    }

    pub fn update(&self, update: impl FnOnce(&mut P)) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.active.as_ref().is_some_and(|active| {
            Arc::ptr_eq(&active.cancel.0, &self.cancel.0)
                && active.terminal.is_none()
                && !self.cancel.is_cancelled()
        }) && let Some(progress) = &mut state.progress
        {
            update(progress);
        }
    }

    fn finish(&self, terminal: (TransferState, Option<String>)) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(active) = &mut state.active
            && Arc::ptr_eq(&active.cancel.0, &self.cancel.0)
            && active.terminal.is_none()
        {
            active.terminal = Some(terminal);
        }
        state.reap();
    }
}

struct Owner<P: TransferStatus>(Option<TransferContext<P>>);

impl<P: TransferStatus> Drop for Owner<P> {
    fn drop(&mut self) {
        if let Some(context) = &self.0 {
            context.cancel.cancel();
            context.finish((TransferState::Cancelled, None));
        }
    }
}

#[cfg(test)]
mod tests;
