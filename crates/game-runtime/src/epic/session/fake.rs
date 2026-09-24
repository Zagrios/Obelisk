//! An in-memory Epic Cloud for session tests. It keeps one complete snapshot,
//! bumps the revision on every upload and can fail the next calls.
use super::*;
use std::{collections::VecDeque, sync::Mutex};

pub(crate) struct FakeCloud {
    account: String,
    rules: Mutex<Option<CloudRules>>,
    state: Mutex<FakeState>,
}

struct FakeState {
    snapshot: CloudSnapshot,
    uploads: usize,
    revision: u64,
    failures: VecDeque<(Call, EpicError)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Call {
    Configuration,
    Download,
    Upload,
}

impl FakeCloud {
    pub(crate) fn new(account: &str, rules: Option<CloudRules>) -> Arc<Self> {
        Arc::new(Self {
            account: account.into(),
            rules: Mutex::new(rules),
            state: Mutex::new(FakeState {
                snapshot: CloudSnapshot {
                    revision: "r0".into(),
                    files: BTreeMap::new(),
                },
                uploads: 0,
                revision: 0,
                failures: VecDeque::new(),
            }),
        })
    }

    /// Another device publishes this complete set.
    pub(crate) fn publish(&self, files: &[(&str, &[u8])]) {
        let mut state = self.state.lock().unwrap();
        state.revision += 1;
        state.snapshot = CloudSnapshot {
            revision: format!("r{}", state.revision),
            files: files
                .iter()
                .map(|(name, bytes)| ((*name).to_owned(), bytes.to_vec()))
                .collect(),
        };
    }

    pub(crate) fn set_rules(&self, rules: Option<CloudRules>) {
        *self.rules.lock().unwrap() = rules;
    }

    pub(crate) fn fail_next(&self, call: Call, error: EpicError) {
        self.state.lock().unwrap().failures.push_back((call, error));
    }

    pub(crate) fn files(&self) -> BTreeMap<String, Vec<u8>> {
        self.state.lock().unwrap().snapshot.files.clone()
    }

    pub(crate) fn uploads(&self) -> usize {
        self.state.lock().unwrap().uploads
    }

    fn failure(&self, call: Call) -> std::result::Result<(), EpicError> {
        let mut state = self.state.lock().unwrap();
        match state.failures.iter().position(|(next, _)| *next == call) {
            Some(index) => Err(state.failures.remove(index).unwrap().1),
            None => Ok(()),
        }
    }
}

impl CloudRemote for FakeCloud {
    async fn account_id(&self) -> std::result::Result<String, EpicError> {
        Ok(self.account.clone())
    }

    async fn cloud_configuration(
        &self,
        _namespace: &str,
        _catalog_item_id: &str,
        _app_name: &str,
        _platform: &str,
    ) -> std::result::Result<Option<CloudRules>, EpicError> {
        self.failure(Call::Configuration)?;
        Ok(self.rules.lock().unwrap().clone())
    }

    async fn cloud_download(
        &self,
        _game: CloudGame<'_>,
        _cancel: &CancelToken,
    ) -> std::result::Result<CloudSnapshot, EpicError> {
        self.failure(Call::Download)?;
        Ok(self.state.lock().unwrap().snapshot.clone())
    }

    async fn cloud_upload(
        &self,
        _game: CloudGame<'_>,
        _rules: &CloudRules,
        files: &BTreeMap<String, Vec<u8>>,
        expected_revision: &str,
        _cancel: &CancelToken,
    ) -> std::result::Result<CloudSnapshot, EpicError> {
        self.failure(Call::Upload)?;
        let mut state = self.state.lock().unwrap();
        if state.snapshot.revision != expected_revision {
            return Err(EpicError::CloudConflict);
        }
        state.revision += 1;
        state.uploads += 1;
        state.snapshot = CloudSnapshot {
            revision: format!("r{}", state.revision),
            files: files.clone(),
        };
        Ok(state.snapshot.clone())
    }
}

/// A launch's Cloud channel without a supervisor: what the sync shows and
/// asks, and the player's answer to the question it shows.
pub(crate) struct LaunchChannel {
    cancel: CancelToken,
    state: Mutex<Shown>,
    changed: std::sync::Condvar,
}

#[derive(Default)]
struct Shown {
    phase: CloudPhase,
    decision_id: u64,
    allowed_choices: Vec<Choice>,
    answer: Option<Choice>,
}

impl LaunchChannel {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            cancel: CancelToken::new(),
            state: Mutex::new(Shown::default()),
            changed: std::sync::Condvar::new(),
        })
    }
    pub(crate) fn phase(&self) -> CloudPhase {
        self.state.lock().unwrap().phase
    }
    pub(crate) fn decision_id(&self) -> u64 {
        self.state.lock().unwrap().decision_id
    }
    /// Answers the question shown after decision `shown`, once it offers
    /// `choice`.
    pub(crate) fn answer_after(&self, shown: u64, choice: Choice) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.decision_id <= shown || !state.allowed_choices.contains(&choice) {
            return false;
        }
        state.answer = Some(choice);
        self.changed.notify_all();
        true
    }
}

impl CloudChannel for LaunchChannel {
    fn cancellation(&self) -> &CancelToken {
        &self.cancel
    }
    fn launching(&self) -> bool {
        true
    }
    fn show(&self, phase: CloudPhase, _: Option<String>) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        state.phase = phase;
        state.allowed_choices.clear();
        Ok(())
    }
    fn question(
        &self,
        phase: CloudPhase,
        _: Option<String>,
        choices: Vec<Choice>,
    ) -> Result<Choice> {
        let mut state = self.state.lock().unwrap();
        state.phase = phase;
        state.decision_id += 1;
        state.allowed_choices = choices;
        state.answer = None;
        loop {
            self.check()?;
            if let Some(choice) = state.answer.take() {
                state.allowed_choices.clear();
                return Ok(choice);
            }
            state = self
                .changed
                .wait_timeout(state, std::time::Duration::from_millis(50))
                .unwrap()
                .0;
        }
    }
}
