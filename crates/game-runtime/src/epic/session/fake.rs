//! An in-memory Epic Cloud for session tests. It keeps one complete set,
//! bumps the revision on every upload, counts the files it is asked to read,
//! can fail the next calls and can change between a listing and its check. An
//! account's installations reach it as Epic's network, for their Cloud saves
//! only.
use super::*;
use crate::{
    epic::{Product, content::Network},
    installations::{BoxFuture, Receipt, Transfer},
    sessions::Failure,
};
use epic_kit::cloud::CloudListing;
use obelisk_core::installation::TransferProgress;
use std::{
    collections::{BTreeSet, VecDeque},
    sync::Mutex,
};

pub(crate) struct FakeCloud {
    account: String,
    rules: Mutex<Option<CloudRules>>,
    state: Mutex<FakeState>,
}

struct FakeState {
    files: BTreeMap<String, Vec<u8>>,
    uploads: usize,
    revision: u64,
    fetched: Vec<String>,
    failures: VecDeque<(Call, EpicError)>,
    /// Another device publishes this set before the next revision check.
    race: Option<BTreeMap<String, Vec<u8>>>,
}

impl FakeState {
    fn listing(&self, account: &str) -> CloudListing {
        CloudListing::of(format!("r{}", self.revision), account.into(), &self.files)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Call {
    Configuration,
    List,
    Fetch,
    Upload,
}

impl FakeCloud {
    pub(crate) fn new(account: &str, rules: Option<CloudRules>) -> Arc<Self> {
        Arc::new(Self {
            account: account.into(),
            rules: Mutex::new(rules),
            state: Mutex::new(FakeState {
                files: BTreeMap::new(),
                uploads: 0,
                revision: 0,
                fetched: Vec::new(),
                failures: VecDeque::new(),
                race: None,
            }),
        })
    }

    /// Another device publishes this complete set.
    pub(crate) fn publish(&self, files: &[(&str, &[u8])]) {
        let mut state = self.state.lock().unwrap();
        state.revision += 1;
        state.files = files
            .iter()
            .map(|(name, bytes)| ((*name).to_owned(), bytes.to_vec()))
            .collect();
    }

    /// Another device publishes this complete set while a read is under way.
    pub(crate) fn publish_during_read(&self, files: &[(&str, &[u8])]) {
        self.state.lock().unwrap().race = Some(
            files
                .iter()
                .map(|(name, bytes)| ((*name).to_owned(), bytes.to_vec()))
                .collect(),
        );
    }

    pub(crate) fn set_rules(&self, rules: Option<CloudRules>) {
        *self.rules.lock().unwrap() = rules;
    }

    pub(crate) fn fail_next(&self, call: Call, error: EpicError) {
        self.state.lock().unwrap().failures.push_back((call, error));
    }

    pub(crate) fn files(&self) -> BTreeMap<String, Vec<u8>> {
        self.state.lock().unwrap().files.clone()
    }

    pub(crate) fn uploads(&self) -> usize {
        self.state.lock().unwrap().uploads
    }

    /// Every file read so far, in the order it was asked for.
    pub(crate) fn fetched(&self) -> Vec<String> {
        self.state.lock().unwrap().fetched.clone()
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
        _platform: Platform,
    ) -> std::result::Result<CloudCatalog, EpicError> {
        self.failure(Call::Configuration)?;
        Ok(CloudCatalog {
            rules: self.rules.lock().unwrap().clone(),
            attributes: BTreeMap::new(),
        })
    }

    async fn cloud_list(
        &self,
        _game: CloudGame<'_>,
        _cancel: &CancelToken,
    ) -> std::result::Result<CloudListing, EpicError> {
        self.failure(Call::List)?;
        Ok(self.state.lock().unwrap().listing(&self.account))
    }

    async fn cloud_revision(
        &self,
        _game: CloudGame<'_>,
        _listing: &CloudListing,
        _cancel: &CancelToken,
    ) -> std::result::Result<String, EpicError> {
        let mut state = self.state.lock().unwrap();
        if let Some(files) = state.race.take() {
            state.files = files;
            state.revision += 1;
        }
        Ok(format!("r{}", state.revision))
    }

    async fn cloud_fetch(
        &self,
        _game: CloudGame<'_>,
        listing: &CloudListing,
        names: &BTreeSet<String>,
        _cancel: &CancelToken,
    ) -> std::result::Result<BTreeMap<String, Vec<u8>>, EpicError> {
        self.failure(Call::Fetch)?;
        let mut state = self.state.lock().unwrap();
        state.fetched.extend(names.iter().cloned());
        names
            .iter()
            .map(|name| {
                let bytes = state.files.get(name).ok_or(EpicError::Protocol)?;
                if !listing.holds(name, bytes) {
                    return Err(EpicError::Integrity);
                }
                Ok((name.clone(), bytes.clone()))
            })
            .collect()
    }

    async fn cloud_upload(
        &self,
        _game: CloudGame<'_>,
        _rules: &CloudRules,
        files: &BTreeMap<String, Vec<u8>>,
        expected_revision: &str,
        _cancel: &CancelToken,
    ) -> std::result::Result<CloudListing, EpicError> {
        self.failure(Call::Upload)?;
        let mut state = self.state.lock().unwrap();
        if format!("r{}", state.revision) != expected_revision {
            return Err(EpicError::CloudConflict);
        }
        state.revision += 1;
        state.uploads += 1;
        state.files = files.clone();
        Ok(state.listing(&self.account))
    }
}

/// Cloud saves ask Epic's network for no Transfer and no launch.
impl Network for FakeCloud {
    fn prepare(&self) -> BoxFuture<'_, std::result::Result<(), Failure>> {
        Box::pin(async { Ok(()) })
    }
    fn account_id(&self) -> BoxFuture<'_, std::result::Result<String, Failure>> {
        Box::pin(async move { Ok(self.account.clone()) })
    }
    fn exchange_code(&self) -> BoxFuture<'_, std::result::Result<String, Failure>> {
        unreachable!("Cloud saves launch nothing")
    }
    fn platform<'a>(
        &'a self,
        _: &'a Product,
    ) -> BoxFuture<'a, std::result::Result<Platform, Failure>> {
        unreachable!("Cloud saves pick no build")
    }
    fn download<'a>(
        &'a self,
        _: &'a Transfer<Epic>,
    ) -> BoxFuture<'a, std::result::Result<epic_kit::client::Installation, Failure>> {
        unreachable!("Cloud saves run no Transfer")
    }
    fn progress(&self) -> Option<TransferProgress> {
        None
    }
    fn cancel(&self) {}
    fn update_available<'a>(
        &'a self,
        _: &'a Receipt<Epic>,
    ) -> BoxFuture<'a, std::result::Result<bool, Failure>> {
        unreachable!("Cloud saves check no update")
    }
    fn cloud_rules<'a>(
        &'a self,
        config: &'a Configuration,
    ) -> BoxFuture<'a, Result<Option<CloudRules>>> {
        Box::pin(async move {
            Ok(CloudRemote::cloud_configuration(
                self,
                &config.namespace,
                &config.catalog_item_id,
                &config.app_name,
                config.platform,
            )
            .await?
            .rules)
        })
    }
    fn maintain<'a>(
        self: Arc<Self>,
        config: Configuration,
        control: Arc<Control>,
        idle: &'a IdleGuard,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(maintain(config, self, control, idle))
    }
}
