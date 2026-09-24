//! Epic's side of an installation Transfer: forge-core runs the order around
//! it, this module prepares Epic's manifest and runs its content engine.
use std::{path::Path, sync::Arc};

use forge_core::{
    install::hex,
    installation::{self, Destination, InstallFailure, InstalledFiles, TransferError},
    transfer::{TransferBusy, TransferContext, Transfers},
};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

use super::{Client, validate_identity};
use crate::{EpicError, content::ContentClient, details};

pub use forge_core::{installation::TransferProgress, transfer::TransferState};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Installation {
    pub app_name: String,
    pub namespace: String,
    pub catalog_item_id: String,
    pub account_id: String,
    pub directory: String,
    pub platform: String,
    pub total_bytes: u64,
    pub build_version: String,
    pub launch_executable: String,
    pub launch_command: String,
    #[serde(default)]
    pub deployment_id: Option<String>,
}

pub(super) type Downloads = Transfers<TransferProgress>;

impl From<TransferBusy> for EpicError {
    fn from(_: TransferBusy) -> Self {
        Self::AlreadyDownloading
    }
}

impl From<InstallFailure> for EpicError {
    fn from(failure: InstallFailure) -> Self {
        match failure {
            InstallFailure::ParentMissing => Self::Service {
                message: "backend.epic.choose_existing_folder".into(),
            },
            InstallFailure::UnsafeDirectory(name) => Self::UnsafePath(name),
            InstallFailure::DestinationIsLink(path)
            | InstallFailure::DestinationIsFile(path)
            | InstallFailure::UnsafeLock(path)
            | InstallFailure::UnsafeReceipt(path) => {
                Self::UnsafePath(path.to_string_lossy().into_owned())
            }
            InstallFailure::Locked => Self::AlreadyDownloading,
            InstallFailure::Io(error) => Self::Io(error),
        }
    }
}

impl TransferError for EpicError {
    fn is_cancellation(&self) -> bool {
        matches!(self, Self::Cancelled)
    }
}

/// Everything fetched before the installation lock is taken.
struct PreparedDownload {
    fetched: details::FetchedManifest,
    account_id: String,
}

impl Client {
    pub fn progress(&self) -> Option<TransferProgress> {
        self.downloads.snapshot()
    }

    pub fn cancel_download(&self) {
        self.downloads.cancel();
    }

    pub async fn download(
        &self,
        namespace: &str,
        catalog_item_id: &str,
        app_name: &str,
        platform: &str,
        parent: &Path,
    ) -> Result<Installation, EpicError> {
        validate_identity(namespace, catalog_item_id, app_name, platform)?;
        let directory = installation_directory_name(namespace, catalog_item_id, app_name, platform);
        installation::install(
            &self.downloads,
            app_name,
            Destination {
                parent,
                directory: &directory,
            },
            |destination, cancel| async move {
                let prepared = tokio::select! {
                    biased;
                    () = cancel.cancelled() => return Err(EpicError::Cancelled),
                    result = self.prepare_download(namespace, catalog_item_id, app_name, platform, &destination) => result?,
                };
                // A revoked account withdraws nothing: the receipt stays until
                // the installation lock is taken.
                if self.session.lock().await.is_none() {
                    return Err(EpicError::Unauthorized);
                }
                Ok(prepared)
            },
            |prepared, destination, transfer| async move {
                self.install_files(namespace, catalog_item_id, app_name, platform, prepared, &destination, &transfer)
                    .await
            },
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn install_files(
        &self,
        namespace: &str,
        catalog_item_id: &str,
        app_name: &str,
        platform: &str,
        prepared: PreparedDownload,
        destination: &Path,
        transfer: &TransferContext<TransferProgress>,
    ) -> Result<InstalledFiles<Installation>, EpicError> {
        let cancel = transfer.cancel_token();
        let PreparedDownload {
            mut fetched,
            account_id,
        } = prepared;
        let network_bytes = fetched.network_bytes;
        let manifest = Arc::new(fetched.manifest.take().ok_or(EpicError::Protocol)?);
        if fetched.network_bytes != 0
            && let Some(key) = &fetched.cache_key
        {
            // This bounded atomic write remains under the installation lock;
            // cancelling or dropping the future cannot detach a cache writer.
            cancel.check()?;
            details::manifest_cache::store(destination, key, &fetched.bytes)?;
        }
        drop(fetched.bytes);

        let progress = transfer.clone();
        let summary = ContentClient::new(self.api.http.clone())
            .download(
                Arc::clone(&manifest),
                &fetched.base_urls,
                destination,
                cancel,
                move |snapshot| {
                    debug_assert!(snapshot.completed_files <= snapshot.total_files);
                    progress.update(|progress| {
                        progress.state = snapshot.phase.into();
                        progress.completed_bytes = snapshot
                            .verification_bytes
                            .unwrap_or(snapshot.completed_bytes);
                        progress.total_bytes = snapshot.total_bytes;
                        progress.network_bytes =
                            network_bytes.saturating_add(snapshot.network_bytes);
                        progress.content_bytes = snapshot.content_bytes;
                        progress.current_file = snapshot.current_file;
                        progress.message.clear();
                    });
                },
            )
            .await?;
        cancel.check()?;
        if self.session.lock().await.is_none() {
            return Err(EpicError::Unauthorized);
        }
        debug_assert!(summary.reused_bytes <= summary.total_bytes);
        debug_assert_eq!(summary.completed_files as usize, manifest.files.len());
        Ok(InstalledFiles {
            receipt: Installation {
                app_name: app_name.into(),
                namespace: namespace.into(),
                catalog_item_id: catalog_item_id.into(),
                account_id,
                directory: destination.to_string_lossy().into_owned(),
                platform: platform.into(),
                total_bytes: summary.total_bytes,
                build_version: manifest.build_version.clone(),
                launch_executable: manifest.launch_executable.clone(),
                launch_command: manifest.launch_command.clone(),
                deployment_id: fetched.deployment_id,
            },
            total_bytes: summary.total_bytes,
            network_bytes: network_bytes.saturating_add(summary.downloaded_bytes),
            content_bytes: summary.content_bytes,
        })
    }

    async fn prepare_download(
        &self,
        namespace: &str,
        catalog_item_id: &str,
        app_name: &str,
        platform: &str,
        cache_directory: &Path,
    ) -> Result<PreparedDownload, EpicError> {
        let fetched = self
            .with_access_token(|token| async move {
                self.ensure_owned(&token, namespace, catalog_item_id, app_name)
                    .await?;
                details::fetch_manifest(
                    &self.api,
                    &token,
                    namespace,
                    catalog_item_id,
                    app_name,
                    platform,
                    Some(cache_directory),
                )
                .await
            })
            .await?;
        Ok(PreparedDownload {
            fetched,
            account_id: self.credentials().await?.account_id,
        })
    }
}

/// The dedicated directory name for one Epic installation. The host relies on
/// this exact scheme to recognise and remove installations it created.
pub fn installation_directory_name(
    namespace: &str,
    catalog_item_id: &str,
    app_name: &str,
    platform: &str,
) -> String {
    // Identity is hashed, never interpolated into a filesystem path. App names
    // can include separators and are not globally unique across namespaces.
    let identity =
        serde_json::to_vec(&(namespace, catalog_item_id, app_name)).expect("strings serialize");
    format!("epic-{}-{platform}", hex(&Sha1::digest(identity)))
}
