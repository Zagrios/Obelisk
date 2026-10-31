//! Epic's side of an installation Transfer: obelisk-core runs the order around
//! it, this module prepares Epic's manifest and runs its content engine.
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use obelisk_core::{
    install::hex,
    installation::{self, Destination, InstallFailure, InstalledFiles, TransferError},
    platform::Platform,
    transfer::{TransferBusy, TransferContext, Transfers},
};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

use super::{Client, PLATFORM, validate_identity, write_platform};
pub use crate::content_manifest::Prerequisite;
use crate::{EpicError, content::ContentClient, details};

pub use obelisk_core::{installation::TransferProgress, transfer::TransferState};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Installation {
    pub app_name: String,
    pub namespace: String,
    pub catalog_item_id: String,
    pub account_id: String,
    pub directory: String,
    #[serde(serialize_with = "write_platform")]
    pub platform: Platform,
    pub total_bytes: u64,
    pub build_version: String,
    pub launch_executable: String,
    pub launch_command: String,
    #[serde(default)]
    pub deployment_id: Option<String>,
    /// What the build asks to run once before its game first starts.
    #[serde(default)]
    pub prerequisite: Option<Prerequisite>,
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
    /// Epic's public EOS overlay asset, installed separately from the game's
    /// files. This deliberately does not require an account-library entry:
    /// the overlay is a launcher component, not an owned game.
    pub async fn install_social_overlay(&self, parent: &Path) -> Result<PathBuf, EpicError> {
        const NAMESPACE: &str = "302e5ede476149b1bc3e4fe6ae45e50e";
        const ITEM: &str = "cc15684f44d849e89e9bf4cec0508b68";
        const APP: &str = "98bc04bc842e4906993fd6d6644ffb8d";
        let transfers = Downloads::default();
        let installed: Installation = installation::install(
            &transfers,
            APP,
            Destination {
                parent,
                directory: "EOSOverlay",
            },
            |destination, cancel| async move {
                let fetched = cancel
                    .race(self.with_access_token(|token| {
                        let destination = destination.clone();
                        async move {
                            details::fetch_manifest(
                                &self.api,
                                &token,
                                NAMESPACE,
                                ITEM,
                                APP,
                                Platform::Windows,
                                Some(&destination),
                                |_| None,
                            )
                            .await
                        }
                    }))
                    .await??;
                let manifest = fetched.manifest.as_ref().ok_or(EpicError::Protocol)?;
                // Registering an incomplete or incompatible overlay must never
                // turn a successful download into a silent no-op in the game.
                for name in [
                    "EOSOVH-Win64-Shipping.dll",
                    "EOSOverlayRenderer-Win64-Shipping.exe",
                ] {
                    if !manifest
                        .files
                        .iter()
                        .any(|file| file.filename == name && file.size() > 0)
                    {
                        return Err(EpicError::UnsupportedContent);
                    }
                }
                Ok(PreparedDownload {
                    fetched,
                    account_id: self.credentials().await?.account_id,
                })
            },
            |prepared, destination, transfer| async move {
                self.install_files(
                    NAMESPACE,
                    ITEM,
                    APP,
                    Platform::Windows,
                    prepared,
                    &destination,
                    &transfer,
                )
                .await
            },
        )
        .await?;
        Ok(installed.directory.into())
    }

    pub fn progress(&self) -> Option<TransferProgress> {
        self.downloads.snapshot()
    }

    pub fn cancel_download(&self) {
        self.build_choice.cancel();
        self.downloads.cancel();
    }

    /// Whether Epic's live build of the game differs from the one installed
    /// in `directory`, as the launcher service names its version. An
    /// installation whose receipt is gone needs its Transfer too.
    pub async fn update_available(
        &self,
        namespace: &str,
        catalog_item_id: &str,
        app_name: &str,
        platform: Platform,
        directory: &Path,
    ) -> Result<bool, EpicError> {
        validate_identity(namespace, catalog_item_id, app_name)?;
        let Some(installed) =
            installation::read_receipt::<Installation>(directory)?.filter(|receipt| {
                receipt.namespace == namespace
                    && receipt.app_name == app_name
                    && receipt.platform == platform
            })
        else {
            return Ok(true);
        };
        let live = self
            .with_access_token(|token| async move {
                details::live_build_version(
                    &self.api,
                    &token,
                    namespace,
                    catalog_item_id,
                    app_name,
                    platform,
                )
                .await
            })
            .await?;
        Ok(live != installed.build_version)
    }

    pub async fn download(
        &self,
        namespace: &str,
        catalog_item_id: &str,
        app_name: &str,
        platform: Platform,
        parent: &Path,
    ) -> Result<Installation, EpicError> {
        validate_identity(namespace, catalog_item_id, app_name)?;
        let directory = installation_directory_name(namespace, catalog_item_id, app_name, platform);
        installation::install(
            &self.downloads,
            app_name,
            Destination {
                parent,
                directory: &directory,
            },
            |destination, cancel| async move {
                let prepared = cancel
                    .race(self.prepare_download(
                        namespace,
                        catalog_item_id,
                        app_name,
                        platform,
                        &destination,
                    ))
                    .await??;
                // A revoked account withdraws nothing: the receipt stays until
                // the installation lock is taken.
                if self.session.lock().await.is_none() {
                    return Err(EpicError::Unauthorized);
                }
                Ok(prepared)
            },
            |prepared, destination, transfer| async move {
                self.install_files(
                    namespace,
                    catalog_item_id,
                    app_name,
                    platform,
                    prepared,
                    &destination,
                    &transfer,
                )
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
        platform: Platform,
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
                        progress.show(snapshot);
                        progress.network_bytes =
                            progress.network_bytes.saturating_add(network_bytes);
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
                platform,
                total_bytes: summary.total_bytes,
                build_version: manifest.build_version.clone(),
                launch_executable: manifest.launch_executable.clone(),
                launch_command: manifest.launch_command.clone(),
                deployment_id: fetched.deployment_id,
                prerequisite: manifest.prerequisite.clone(),
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
        platform: Platform,
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
                    |key| self.build_choice.take(key),
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
    platform: Platform,
) -> String {
    // Identity is hashed, never interpolated into a filesystem path. App names
    // can include separators and are not globally unique across namespaces.
    let identity =
        serde_json::to_vec(&(namespace, catalog_item_id, app_name)).expect("strings serialize");
    format!(
        "epic-{}-{}",
        hex(&Sha1::digest(identity)),
        PLATFORM.name(platform)
    )
}
