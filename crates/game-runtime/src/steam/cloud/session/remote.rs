use super::*;
use cloud_vault::FileVersion;
use steam_kit::cloud::{CloudError, CloudFile, CloudMetadata, CloudPreferences};

/// Remote file reads, the part of a connection that builds a verified view.
pub(crate) trait RemoteSource {
    async fn download_files(
        &mut self,
        files: &[CloudFile],
        receive: impl FnMut(&CloudFile, Vec<u8>) -> Result<(), CloudError>,
    ) -> Result<(), Failure>;
    async fn listing(&mut self) -> Result<CloudListing, Failure>;
}

/// One authenticated Steam Cloud connection, bound to one app.
pub(crate) trait CloudConnection: RemoteSource {
    /// The app's Cloud rules, or `None` when the game has no Steam Cloud.
    async fn cloud_metadata(&mut self) -> Result<Option<CloudMetadata>, Failure>;
    async fn preferences(
        &mut self,
        cache: &mut CloudPreferencesCache,
    ) -> Result<CloudPreferences, CloudError>;
    async fn settle_upload(
        &mut self,
        transaction: &mut UploadTransaction,
    ) -> Result<(), CloudError>;
    /// Commits the transaction, then hands back every surviving remote file
    /// for staging and returns the complete listing read afterwards.
    async fn upload_verified(
        &mut self,
        transaction: &mut UploadTransaction,
        receive: impl FnMut(&CloudFile, Vec<u8>) -> Result<(), CloudError>,
    ) -> Result<CloudListing, CloudError>;
}

/// Where a session's Cloud connections come from: Steam's CM in production,
/// an in-memory store in tests.
pub(crate) trait CloudRemote {
    type Connection: CloudConnection;
    async fn connect(
        &self,
        credentials: &Credentials,
        app_id: u32,
        control: Arc<Control>,
    ) -> Result<Self::Connection, Failure>;
}

pub(crate) struct SteamCm {
    http: reqwest::Client,
}

impl SteamCm {
    pub(crate) fn new() -> Result<Self, Failure> {
        Ok(Self {
            http: steam_kit::client::http_client().map_err(|_| Failure::Network)?,
        })
    }
}

impl CloudRemote for SteamCm {
    type Connection = CmConnection;
    async fn connect(
        &self,
        credentials: &Credentials,
        app_id: u32,
        control: Arc<Control>,
    ) -> Result<CmConnection, Failure> {
        let mut cm = SteamConnection::connect(self.http.clone()).await?;
        cm.log_on(
            &credentials.account_name,
            credentials.steam_id,
            &credentials.refresh_token,
        )
        .await?;
        Ok(CmConnection {
            cm,
            http: self.http.clone(),
            app_id,
            control,
        })
    }
}

pub(crate) struct CmConnection {
    cm: SteamConnection,
    http: reqwest::Client,
    app_id: u32,
    control: Arc<Control>,
}

impl RemoteSource for CmConnection {
    async fn download_files(
        &mut self,
        files: &[CloudFile],
        receive: impl FnMut(&CloudFile, Vec<u8>) -> Result<(), CloudError>,
    ) -> Result<(), Failure> {
        let control = &self.control;
        Ok(self
            .cm
            .cloud_download_files(
                &self.http,
                self.app_id,
                files,
                || control.check().is_ok(),
                receive,
            )
            .await?)
    }
    async fn listing(&mut self) -> Result<CloudListing, Failure> {
        Ok(self.cm.cloud_listing(self.app_id).await?)
    }
}

impl CloudConnection for CmConnection {
    async fn cloud_metadata(&mut self) -> Result<Option<CloudMetadata>, Failure> {
        Ok(self.cm.app_info(self.app_id).await?.cloud)
    }
    async fn preferences(
        &mut self,
        cache: &mut CloudPreferencesCache,
    ) -> Result<CloudPreferences, CloudError> {
        self.cm
            .cloud_preferences_cached(&self.http, self.app_id, cache)
            .await
    }
    async fn settle_upload(
        &mut self,
        transaction: &mut UploadTransaction,
    ) -> Result<(), CloudError> {
        let control = &self.control;
        self.cm
            .cloud_settle_upload(&self.http, transaction, || control.check().is_ok())
            .await
    }
    async fn upload_verified(
        &mut self,
        transaction: &mut UploadTransaction,
        receive: impl FnMut(&CloudFile, Vec<u8>) -> Result<(), CloudError>,
    ) -> Result<CloudListing, CloudError> {
        let control = &self.control;
        self.cm
            .cloud_upload_verified(&self.http, transaction, || control.check().is_ok(), receive)
            .await
    }
}

impl<R: CloudRemote> Adapter<R> {
    fn cached_file(&self, vault: &Vault, file: &CloudFile) -> Option<FileVersion> {
        // Context survives restart. Its listing can have been refreshed before
        // remote bytes, so never trust that pairing: verify the actual blob
        // against the newly listed Steam hash and size on every cache hit.
        let mut checked = std::collections::BTreeSet::new();
        [
            Some(&self.downloaded),
            self.context.as_ref().map(|c| &c.remote.files),
            vault.baseline().map(|b| &b.files),
            vault.local(),
        ]
        .into_iter()
        .flatten()
        .filter_map(|files| files.0.get(&file.name))
        .filter(|cached| cached.size == u64::from(file.size))
        .filter(|cached| checked.insert((&cached.hash, cached.size)))
        .find_map(|cached| {
            let bytes = vault.read_content(cached).ok()?;
            file.verify(&bytes).ok()?;
            Some(FileVersion {
                platforms: file.platforms,
                ..cached.clone()
            })
        })
    }

    pub(super) async fn read_remote(
        &mut self,
        source: &mut impl RemoteSource,
        vault: &Vault,
        control: &Control,
        listing: &CloudListing,
    ) -> Result<RemoteView, Failure> {
        if listing.steam_id != self.credentials.steam_id || listing.app_id != self.config.app_id {
            return Err(Failure::Authentication);
        }
        let paths = self.context()?.paths()?;
        let mut files = Manifest::default();
        let mut missing = Vec::new();
        for file in &listing.files {
            control.check()?;
            if paths.existing_file(file)?.is_none() {
                continue;
            }
            if let Some(version) = self.cached_file(vault, file) {
                files.0.insert(file.name.clone(), version);
            } else {
                missing.push(file.clone());
            }
        }
        source
            .download_files(&missing, |file, bytes| {
                let version = vault
                    .stage(&bytes, file.platforms)
                    .map_err(|_| CloudError::Storage)?;
                self.downloaded.0.insert(file.name.clone(), version.clone());
                files.0.insert(file.name.clone(), version);
                Ok(())
            })
            .await?;
        // Even an entirely cached view is valid only while the remote listing
        // stays unchanged. No cache may authorize a stale conflict decision.
        if source.listing().await? != *listing {
            return Err(Failure::Changed);
        }
        control.check()?;
        self.downloaded = files.clone();
        Ok(RemoteView {
            revision: listing.change_number.to_string(),
            files,
        })
    }
}

#[cfg(test)]
mod tests;
