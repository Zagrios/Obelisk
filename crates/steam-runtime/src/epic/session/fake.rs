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
