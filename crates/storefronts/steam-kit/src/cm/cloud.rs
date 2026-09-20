use super::*;
use crate::cloud::{
    CloudError, CloudFile, CloudListing, CloudQuota, UploadTransaction, deadline,
    preferences::ROAMING_CONFIG_APP, upload, wire,
};

#[cfg(debug_assertions)]
mod interruption_probe;

const UPLOAD_DEADLINE: u64 = 120;

impl SteamConnection {
    /// Read only the account's remote roaming configuration. Never consult a
    /// local Steam installation or change preferences as an ownership shortcut.
    pub async fn cloud_preferences(
        &mut self,
        http: &reqwest::Client,
        app_id: u32,
    ) -> Result<crate::cloud::CloudPreferences, CloudError> {
        self.cloud_preferences_cached(http, app_id, &mut Default::default())
            .await
    }

    pub async fn cloud_preferences_cached(
        &mut self,
        http: &reqwest::Client,
        app_id: u32,
        cache: &mut crate::cloud::CloudPreferencesCache,
    ) -> Result<crate::cloud::CloudPreferences, CloudError> {
        let before = self.cloud_listing(ROAMING_CONFIG_APP).await?;
        let preferences = cache
            .read(app_id, &before, async |file| {
                self.cloud_download(http, ROAMING_CONFIG_APP, file).await
            })
            .await?;
        if self.cloud_listing(ROAMING_CONFIG_APP).await? != before {
            return Err(CloudError::Changed);
        }
        Ok(preferences)
    }
    /// Low-level, journaled transport. The caller must hold the coordinator's
    /// upload permit and exclude a native synchronizer for this account/store.
    /// The guard is rechecked before every request; UI closure is not revocation.
    pub async fn cloud_upload(
        &mut self,
        http: &reqwest::Client,
        transaction: &mut UploadTransaction,
        authorized: impl Fn() -> bool,
    ) -> Result<CloudListing, CloudError> {
        let mut transport = self.upload_transport(http, transaction, &authorized)?;
        deadline(
            UPLOAD_DEADLINE,
            upload::execute(&mut transport, transaction, &authorized),
        )
        .await
    }

    /// Like cloud_upload, handing verified readback bytes to the caller so it
    /// can stage them without downloading the same remote files a second time.
    /// Bytes remain provisional until this returns a complete stable listing.
    pub async fn cloud_upload_verified(
        &mut self,
        http: &reqwest::Client,
        transaction: &mut UploadTransaction,
        authorized: impl Fn() -> bool,
        receive: impl FnMut(&CloudFile, Vec<u8>) -> Result<(), CloudError>,
    ) -> Result<CloudListing, CloudError> {
        let mut transport = self.upload_transport(http, transaction, &authorized)?;
        deadline(
            UPLOAD_DEADLINE,
            upload::execute_verified(&mut transport, transaction, &authorized, receive),
        )
        .await
    }

    /// Obtain a positive closure receipt without resending file mutations.
    /// Ambiguous old file requests remain blocked; the caller must read a new
    /// complete remote view after this method returns before reconciliation.
    pub async fn cloud_settle_upload(
        &mut self,
        http: &reqwest::Client,
        transaction: &mut UploadTransaction,
        authorized: impl Fn() -> bool,
    ) -> Result<(), CloudError> {
        let mut transport = self.upload_transport(http, transaction, &authorized)?;
        deadline(
            UPLOAD_DEADLINE,
            upload::settle(&mut transport, transaction, &authorized),
        )
        .await
    }

    /// Shared admission for journaled upload operations: the session must be
    /// logged on as the transaction's account.
    fn upload_transport<'a, F: Fn() -> bool>(
        &'a mut self,
        http: &'a reqwest::Client,
        transaction: &UploadTransaction,
        authorized: &'a F,
    ) -> Result<UploadTransport<'a, F>, CloudError> {
        self.ensure_logged_on()?;
        if transaction.steam_id() != self.steam_id {
            return Err(CloudError::Revoked);
        }
        Ok(UploadTransport {
            connection: self,
            http,
            authorized,
        })
    }

    pub async fn cloud_listing(&mut self, app_id: u32) -> Result<CloudListing, CloudError> {
        if app_id == 0 {
            return Err(CloudError::Invalid("app id"));
        }
        let response: wire::ListingResponse = self
            .service(
                "Cloud.GetAppFileChangelist#1",
                &wire::ListingRequest {
                    app_id,
                    synced_change_number: Some(0),
                },
            )
            .await?;
        crate::cloud::listing(self.steam_id, app_id, response)
    }

    pub async fn cloud_quota(&mut self, app_id: u32) -> Result<CloudQuota, CloudError> {
        if app_id == 0 {
            return Err(CloudError::Invalid("app id"));
        }
        let response: wire::QuotaResponse = self
            .service(
                "Cloud.ClientGetAppQuotaUsage#1",
                &wire::AppRequest { app_id },
            )
            .await?;
        Ok(CloudQuota {
            existing_files: response.existing_files,
            existing_bytes: response.existing_bytes,
            max_files: response.max_files,
            max_bytes: response.max_bytes,
        })
    }

    pub async fn cloud_download(
        &mut self,
        http: &reqwest::Client,
        app_id: u32,
        file: &CloudFile,
    ) -> Result<Vec<u8>, CloudError> {
        if file.persist_state != 0 {
            return Err(CloudError::Invalid("persist state"));
        }
        let response: wire::DownloadResponse = self
            .service(
                "Cloud.ClientFileDownload#1",
                &wire::DownloadRequest {
                    app_id,
                    name: file.name.clone(),
                },
            )
            .await?;
        crate::cloud::download(http, app_id, file, response).await
    }

    /// Bounded concurrent HTTP reads on this account's serialized CM session.
    /// The caller owns the listing checks before and after the complete set.
    pub async fn cloud_download_files(
        &mut self,
        http: &reqwest::Client,
        app_id: u32,
        files: &[CloudFile],
        authorized: impl Fn() -> bool,
        receive: impl FnMut(&CloudFile, Vec<u8>) -> Result<(), CloudError>,
    ) -> Result<(), CloudError> {
        self.ensure_logged_on()?;
        crate::cloud::downloads::download_files(
            files,
            &mut DownloadPlans {
                connection: self,
                app_id,
            },
            |file, response| {
                let http = http.clone();
                async move { crate::cloud::download(&http, app_id, &file, response).await }
            },
            &authorized,
            receive,
        )
        .await
    }
}

struct DownloadPlans<'a> {
    connection: &'a mut SteamConnection,
    app_id: u32,
}

impl crate::cloud::downloads::DownloadPlans for DownloadPlans<'_> {
    type Plan = wire::DownloadResponse;

    async fn prepare(&mut self, file: CloudFile) -> Result<Self::Plan, CloudError> {
        if file.persist_state != 0 {
            return Err(CloudError::Invalid("persist state"));
        }
        Ok(self
            .connection
            .service(
                "Cloud.ClientFileDownload#1",
                &wire::DownloadRequest {
                    app_id: self.app_id,
                    name: file.name,
                },
            )
            .await?)
    }
}

struct UploadTransport<'a, F> {
    connection: &'a mut SteamConnection,
    http: &'a reqwest::Client,
    authorized: &'a F,
}

impl<F: Fn() -> bool> crate::cloud::upload::Transport for UploadTransport<'_, F> {
    async fn download_files(
        &mut self,
        app: u32,
        files: &[CloudFile],
        authorized: &impl Fn() -> bool,
        receive: impl FnMut(&CloudFile, Vec<u8>) -> Result<(), CloudError>,
    ) -> Result<(), CloudError> {
        self.connection
            .cloud_download_files(
                self.http,
                app,
                files,
                || (self.authorized)() && authorized(),
                receive,
            )
            .await
    }
    async fn commit(
        &mut self,
        request: &wire::CommitUploadRequest,
    ) -> Result<wire::CommitUploadResponse, CloudError> {
        if !(self.authorized)() {
            return Err(CloudError::Revoked);
        }
        self.connection.ensure_logged_on()?;
        let job = self
            .connection
            .send(
                SERVICE_REQUEST,
                request,
                Some("Cloud.ClientCommitFileUpload#1"),
            )
            .await?;
        let packet = self
            .connection
            .wait_for(SERVICE_RESPONSE, Some(job))
            .await?;
        commit_response(packet.header.result, &packet.body, request.succeeded)
    }
    async fn call<R: Message + Default>(
        &mut self,
        name: &str,
        request: &impl Message,
    ) -> Result<R, CloudError> {
        if !(self.authorized)() {
            return Err(CloudError::Revoked);
        }
        Ok(self.connection.service(name, request).await?)
    }
    async fn block(&mut self, block: &wire::UploadBlock, bytes: &[u8]) -> Result<(), CloudError> {
        let request = crate::cloud::upload::block_request(block, bytes)?;
        if !(self.authorized)() {
            return Err(CloudError::Revoked);
        }
        let response = self
            .http
            .execute(request)
            .await
            .map_err(|_| CloudError::Network)?;
        if !response.status().is_success() {
            return Err(CloudError::Network);
        }
        crate::client::read_bounded(response, 1024 * 1024)
            .await
            .map_err(|_| CloudError::Network)?;
        Ok(())
    }
    async fn listing(&mut self, app: u32) -> Result<CloudListing, CloudError> {
        if !(self.authorized)() {
            return Err(CloudError::Revoked);
        }
        self.connection.cloud_listing(app).await
    }
    async fn download(&mut self, app: u32, file: &CloudFile) -> Result<Vec<u8>, CloudError> {
        if !(self.authorized)() {
            return Err(CloudError::Revoked);
        }
        let response: wire::DownloadResponse = self
            .connection
            .service(
                "Cloud.ClientFileDownload#1",
                &wire::DownloadRequest {
                    app_id: app,
                    name: file.name.clone(),
                },
            )
            .await?;
        if !(self.authorized)() {
            return Err(CloudError::Revoked);
        }
        crate::cloud::download(self.http, app, file, response).await
    }
}

fn commit_response(
    result: Option<i32>,
    body: &[u8],
    succeeded: bool,
) -> Result<wire::CommitUploadResponse, CloudError> {
    // Steam reports a failed HTTP transfer with EResult::Fail and an explicit
    // file_committed=false body. That is the requested negative receipt, not
    // upload success. Never apply this exception to a positive commit, an
    // absent/malformed receipt, a missing envelope result or another rejection.
    if !succeeded && result == Some(2) {
        let response =
            wire::CommitUploadResponse::decode(body).map_err(|e| CloudError::Cm(e.into()))?;
        if response.committed == Some(false) {
            return Ok(response);
        }
    }
    check_result(result, "backend.steam.cm.action.contentRequest")?;
    wire::CommitUploadResponse::decode(body).map_err(|e| CloudError::Cm(e.into()))
}

#[cfg(test)]
mod upload_receipt_tests {
    use super::*;

    #[test]
    fn failed_transfer_accepts_only_the_explicit_negative_file_receipt() {
        // Exact nonsensitive body/result observed in the authenticated restart
        // probe: EResult::Fail with file_committed explicitly false.
        let receipt = commit_response(Some(2), &[0x08, 0x00], false).unwrap();
        assert_eq!(receipt.committed, Some(false));
        for (result, body, succeeded) in [
            (Some(2), vec![0x08, 0x00], true),
            (Some(2), vec![0x08, 0x01], false),
            (Some(2), vec![], false),
            (Some(10), vec![0x08, 0x00], false),
            (None, vec![0x08, 0x00], false),
            (Some(2), vec![0x80], false),
        ] {
            assert!(commit_response(result, &body, succeeded).is_err());
        }
    }
}
