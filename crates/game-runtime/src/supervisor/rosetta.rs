//! Rosetta translation gate shared by the Steam and Epic supervisors.
use crate::supervisor::Error;
use std::path::{Path, PathBuf};
use tokio::sync::Notify;

/// The Rosetta helper app bundled beside the storefront helpers.
pub(crate) fn helper(helpers: &Path) -> PathBuf {
    helpers.join("SailOver Rosetta.app/Contents/MacOS/SailOverRosetta")
}

pub(crate) struct Gate {
    pub(crate) helper: PathBuf,
    pub(crate) check: fn(&Path) -> Result<bool, Error>,
    changed: Notify,
}

impl Gate {
    pub(crate) fn new(helper: PathBuf, check: fn(&Path) -> Result<bool, Error>) -> Self {
        Self {
            helper,
            check,
            changed: Notify::new(),
        }
    }

    /// Wake `wait` after the published request or cancellation state changed.
    pub(crate) fn wake(&self) {
        self.changed.notify_waiters();
    }

    /// Synchronous check for command handlers running off the executor.
    pub(crate) fn available(&self) -> Result<bool, Error> {
        (self.check)(&self.helper)
    }

    pub(crate) async fn probe(&self) -> Result<bool, Error> {
        let helper = self.helper.clone();
        let check = self.check;
        tokio::task::spawn_blocking(move || check(&helper))
            .await
            .map_err(|_| Error::StartFailed)?
    }

    /// Returns true if installation interrupted preparation and short-lived
    /// launch credentials may need refreshing. `pending` also checks session
    /// cancellation; callers must `wake` when resuming or cancelling the session.
    pub(crate) async fn wait<E: From<Error>>(
        &self,
        request: impl FnOnce() -> Result<(), E>,
        pending: impl Fn() -> Result<bool, E>,
    ) -> Result<bool, E> {
        pending()?;
        if self.probe().await? {
            pending()?;
            return Ok(false);
        }
        request()?;
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            // Register before reading state, including when resume/cancel races
            // with publication of the installation request.
            notified.as_mut().enable();
            if !pending()? {
                break;
            }
            notified.await;
        }
        // Acknowledgement alone cannot authorize launch: verify translation again.
        let available = self.probe().await?;
        pending()?;
        if !available {
            return Err(Error::RosettaUnavailable.into());
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests;
