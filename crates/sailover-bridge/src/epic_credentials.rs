//! An Epic account's credentials rotate with its calls. Each rotation is
//! saved in order, one save at a time, so no call has to remember to.
use std::{
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

/// Saves the Epic account's current credentials, which account calls rotate.
/// It may block.
#[uniffi::export(with_foreign)]
pub trait EpicCredentialSave: Send + Sync {
    fn save(&self);
}

/// How long a launch waits for rotated credentials to be saved.
const SAVE_TIMEOUT: Duration = Duration::from_secs(60);

/// Where an account's credentials stand: the rotation last saved, once
/// persistence runs.
#[derive(Default)]
pub(crate) struct Persistence {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Default)]
struct State {
    running: bool,
    stopped: bool,
    saved: Option<u64>,
}

impl Persistence {
    fn saved(&self, revision: u64) {
        let mut state = self.state.lock().unwrap();
        state.saved = Some(revision);
        self.changed.notify_all();
    }

    fn stopped(&self) {
        self.state.lock().unwrap().stopped = true;
        self.changed.notify_all();
    }

    /// Waits until the rotation `current` names was saved. Returns at once
    /// without persistence, once it stopped, or after `SAVE_TIMEOUT`.
    pub(crate) fn wait(&self, current: impl Fn() -> u64) {
        let deadline = Instant::now() + SAVE_TIMEOUT;
        let mut state = self.state.lock().unwrap();
        while state.running
            && !state.stopped
            && state.saved.is_none_or(|saved| saved < current())
            && Instant::now() < deadline
        {
            state = self
                .changed
                .wait_timeout(state, Duration::from_millis(100))
                .unwrap()
                .0;
        }
    }
}

/// An account whose credentials rotate.
pub(crate) trait Rotating: Send + Sync + 'static {
    fn revision(&self) -> u64;
    /// The next revision after `after`, or `None` once observers stop.
    fn next(&self, after: u64) -> impl Future<Output = Option<u64>> + Send;
}

impl Rotating for epic_kit::client::Client {
    fn revision(&self) -> u64 {
        self.credentials_revision()
    }
    async fn next(&self, after: u64) -> Option<u64> {
        self.next_credentials_revision(after).await
    }
}

/// Starts saving `account`'s credentials through `saver`, once per session.
pub(crate) fn persist<A: Rotating>(
    account: Arc<A>,
    persistence: Arc<Persistence>,
    saver: Arc<dyn EpicCredentialSave>,
) {
    {
        let mut state = persistence.state.lock().unwrap();
        if state.running {
            return;
        }
        state.running = true;
    }
    tokio::spawn(async move {
        let mut revision = account.revision();
        loop {
            let save = saver.clone();
            // The saver reads the latest credentials: a rotation during the
            // save is saved again next.
            let _ = tokio::task::spawn_blocking(move || save.save()).await;
            persistence.saved(revision);
            match account.next(revision).await {
                Some(next) => revision = next,
                None => break,
            }
        }
        persistence.stopped();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::sync::watch;

    struct Account(watch::Sender<(u64, bool)>);
    impl Rotating for Account {
        fn revision(&self) -> u64 {
            self.0.borrow().0
        }
        async fn next(&self, after: u64) -> Option<u64> {
            let mut changes = self.0.subscribe();
            let state = *changes
                .wait_for(|(revision, stopped)| *stopped || *revision != after)
                .await
                .ok()?;
            (!state.1).then_some(state.0)
        }
    }

    /// Records the revision it saw at each save.
    struct Saver(Arc<Account>, Mutex<Vec<u64>>, AtomicU64);
    impl EpicCredentialSave for Saver {
        fn save(&self) {
            std::thread::sleep(Duration::from_millis(self.2.load(Ordering::Acquire)));
            self.1.lock().unwrap().push(self.0.revision());
        }
    }

    async fn eventually(predicate: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !predicate() {
            assert!(Instant::now() < deadline, "timed out");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn every_rotation_is_saved_in_order_and_a_launch_waits_for_the_last() {
        let account = Arc::new(Account(watch::channel((1, false)).0));
        let saver = Arc::new(Saver(
            account.clone(),
            Mutex::new(Vec::new()),
            AtomicU64::new(0),
        ));
        let persistence = Arc::new(Persistence::default());
        persist(account.clone(), persistence.clone(), saver.clone());
        persist(account.clone(), persistence.clone(), saver.clone());
        eventually(|| saver.1.lock().unwrap().len() == 1).await;

        saver.2.store(200, Ordering::Release);
        account.0.send_modify(|state| state.0 = 2);
        let (waiting, current) = (persistence.clone(), account.clone());
        tokio::task::spawn_blocking(move || waiting.wait(|| current.revision()))
            .await
            .unwrap();
        assert_eq!(
            *saver.1.lock().unwrap(),
            [1, 2],
            "one loop saves each rotation once"
        );

        account.0.send_modify(|state| state.1 = true);
        eventually(|| persistence.state.lock().unwrap().stopped).await;
        account.0.send_modify(|state| state.0 = 3);
        let started = Instant::now();
        persistence.wait(|| account.revision());
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a stopped persistence never holds a launch"
        );
    }
}
