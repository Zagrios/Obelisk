//! An in-memory Steam Cloud for session tests: one app's files, its Cloud
//! rules and preferences, and failures injected into the next connection.
use super::*;
use sha1::{Digest, Sha1};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Mutex,
};
use steam_kit::cloud::{CloudError, CloudFile, CloudMetadata, CloudPreferences};

pub(crate) struct FakeSteamCloud {
    state: Arc<Mutex<FakeState>>,
}

struct FakeState {
    steam_id: u64,
    app_id: u32,
    metadata: Option<CloudMetadata>,
    enabled: Option<bool>,
    change_number: u64,
    files: BTreeMap<String, (Vec<u8>, u32)>,
    uploads: usize,
    downloads: usize,
    failures: VecDeque<Failure>,
}

/// AutoCloud rule for `*.sav` files at the top of the installation.
pub(crate) fn save_rule() -> CloudMetadata {
    let rule = CloudMetadata::Object(
        [
            ("root", "GameInstall"),
            ("path", ""),
            ("pattern", "*.sav"),
            ("recursive", "0"),
        ]
        .into_iter()
        .map(|(key, value)| (key.into(), CloudMetadata::Value(value.into())))
        .collect(),
    );
    CloudMetadata::Object(vec![(
        "savefiles".into(),
        CloudMetadata::Object(vec![("0".into(), rule)]),
    )])
}

/// The remote name of `slot.sav` under [`save_rule`].
pub(crate) const SLOT: &str = "%GameInstall%slot.sav";

impl FakeSteamCloud {
    pub(crate) fn new(steam_id: u64, app_id: u32) -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeState {
                steam_id,
                app_id,
                metadata: Some(save_rule()),
                enabled: Some(true),
                change_number: 1,
                files: BTreeMap::new(),
                uploads: 0,
                downloads: 0,
                failures: VecDeque::new(),
            })),
        }
    }

    pub(crate) fn handle(&self) -> Self {
        Self {
            state: Arc::clone(&self.state),
        }
    }

    /// Another device publishes this file.
    pub(crate) fn publish(&self, name: &str, bytes: &[u8]) {
        self.publish_for(name, bytes, 2);
    }

    /// Another device publishes this file for these platforms only.
    pub(crate) fn publish_for(&self, name: &str, bytes: &[u8], platforms: u32) {
        let mut state = self.state.lock().unwrap();
        state.files.insert(name.into(), (bytes.to_vec(), platforms));
        state.change_number += 1;
    }

    pub(crate) fn set_metadata(&self, metadata: Option<CloudMetadata>) {
        self.state.lock().unwrap().metadata = metadata;
    }

    pub(crate) fn set_enabled(&self, enabled: Option<bool>) {
        self.state.lock().unwrap().enabled = enabled;
    }

    pub(crate) fn fail_next_connection(&self, failure: Failure) {
        self.state.lock().unwrap().failures.push_back(failure);
    }

    pub(crate) fn file(&self, name: &str) -> Option<Vec<u8>> {
        let state = self.state.lock().unwrap();
        state.files.get(name).map(|(bytes, _)| bytes.clone())
    }

    pub(crate) fn file_platforms(&self, name: &str) -> Option<u32> {
        self.state
            .lock()
            .unwrap()
            .files
            .get(name)
            .map(|(_, mask)| *mask)
    }

    pub(crate) fn uploads(&self) -> usize {
        self.state.lock().unwrap().uploads
    }

    /// Files read from this Cloud so far.
    pub(crate) fn downloads(&self) -> usize {
        self.state.lock().unwrap().downloads
    }

    /// The Cloud rules and listing Steam gives an uninstall archive, `None`
    /// without Steam Cloud.
    pub(crate) fn archive_rules(&self) -> Option<(CloudMetadata, CloudListing)> {
        let state = self.state.lock().unwrap();
        state
            .metadata
            .clone()
            .map(|metadata| (metadata, state.listing()))
    }
}

impl FakeState {
    fn listing(&self) -> CloudListing {
        CloudListing {
            steam_id: self.steam_id,
            app_id: self.app_id,
            change_number: self.change_number,
            files: self
                .files
                .iter()
                .map(|(name, (bytes, platforms))| CloudFile {
                    name: name.clone(),
                    sha1: Sha1::digest(bytes)
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect(),
                    size: bytes.len() as u32,
                    timestamp: 1,
                    persist_state: 0,
                    platforms: *platforms,
                    reupload_requested: false,
                })
                .collect(),
        }
    }
}

impl CloudRemote for FakeSteamCloud {
    type Connection = FakeConnection;
    async fn connect(
        &self,
        _credentials: &Credentials,
        _app_id: u32,
        _control: Arc<Control>,
    ) -> Result<FakeConnection, Failure> {
        if let Some(failure) = self.state.lock().unwrap().failures.pop_front() {
            return Err(failure);
        }
        Ok(FakeConnection {
            state: Arc::clone(&self.state),
        })
    }
}

pub(crate) struct FakeConnection {
    state: Arc<Mutex<FakeState>>,
}

impl CloudConnection for FakeConnection {
    async fn download_files(
        &mut self,
        files: &[CloudFile],
        mut receive: impl FnMut(&CloudFile, Vec<u8>) -> Result<(), CloudError>,
    ) -> Result<(), Failure> {
        for file in files {
            let bytes = {
                let mut state = self.state.lock().unwrap();
                state.downloads += 1;
                state
                    .files
                    .get(&file.name)
                    .map(|(bytes, _)| bytes.clone())
                    .ok_or(Failure::Changed)?
            };
            file.verify(&bytes)?;
            receive(file, bytes)?;
        }
        Ok(())
    }
    async fn listing(&mut self) -> Result<CloudListing, Failure> {
        Ok(self.state.lock().unwrap().listing())
    }
    async fn cloud_metadata(&mut self) -> Result<Option<CloudMetadata>, Failure> {
        Ok(self.state.lock().unwrap().metadata.clone())
    }
    async fn preferences(
        &mut self,
        _cache: &mut CloudPreferencesCache,
    ) -> Result<CloudPreferences, CloudError> {
        let enabled = self.state.lock().unwrap().enabled;
        Ok(CloudPreferences {
            account_enabled: enabled,
            game_enabled: enabled,
            revision: 1,
        })
    }
    async fn settle_upload(
        &mut self,
        _transaction: &mut UploadTransaction,
    ) -> Result<(), CloudError> {
        Ok(())
    }
    async fn upload(
        &mut self,
        transaction: &mut UploadTransaction,
    ) -> Result<CloudListing, CloudError> {
        let mut state = self.state.lock().unwrap();
        for (index, file) in transaction.uploads().iter().enumerate() {
            let bytes = transaction.upload_body(index)?;
            state
                .files
                .insert(file.name.clone(), (bytes, file.platforms));
        }
        for name in transaction.deletes() {
            state.files.remove(name);
        }
        state.change_number += 1;
        state.uploads += 1;
        Ok(state.listing())
    }
}
