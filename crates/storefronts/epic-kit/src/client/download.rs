//! Account-scoped downloads use the same cancellation and atomic receipt
//! primitives as Steam. Content formats stay in the storefront's own engine.
use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::Arc,
};

use forge_core::{
    install::{LOCK_FILE, RECEIPT_FILE, checked_file_path, hex, try_lock_file},
    transfer::{TransferBusy, TransferContext, TransferStatus, Transfers},
};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

use super::{Client, validate_identity};
use crate::{EpicError, content::ContentClient, details};

pub use forge_core::transfer::TransferState;

#[derive(Clone, Debug)]
pub struct TransferProgress {
    pub app_name: String,
    pub state: TransferState,
    pub completed_bytes: u64,
    pub total_bytes: u64,
    pub network_bytes: u64,
    pub content_bytes: u64,
    pub current_file: String,
    pub message: String,
}

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

impl TransferStatus for TransferProgress {
    fn finish(&mut self, state: TransferState, error: Option<String>) {
        self.state = state;
        self.message = error.unwrap_or_else(|| {
            match state {
                TransferState::Complete if self.content_bytes == 0 => {
                    "backend.epic.files_up_to_date"
                }
                TransferState::Complete => "backend.epic.download_complete",
                _ => "backend.epic.cancelled",
            }
            .into()
        });
    }
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
        self.downloads
            .run(
                TransferProgress {
                    app_name: app_name.into(),
                    state: TransferState::Preparing,
                    completed_bytes: 0,
                    total_bytes: 0,
                    network_bytes: 0,
                    content_bytes: 0,
                    current_file: String::new(),
                    message: "backend.epic.preparing".into(),
                },
                |transfer| async move {
                    let result = self
                        .download_inner(
                            namespace,
                            catalog_item_id,
                            app_name,
                            platform,
                            parent,
                            &transfer,
                        )
                        .await;
                    if matches!(result, Err(EpicError::Cancelled)) {
                        transfer.cancel_token().cancel();
                    }
                    result
                },
            )
            .await
    }

    async fn download_inner(
        &self,
        namespace: &str,
        catalog_item_id: &str,
        app_name: &str,
        platform: &str,
        parent: &Path,
        transfer: &TransferContext<TransferProgress>,
    ) -> Result<Installation, EpicError> {
        let cancel = transfer.cancel_token();
        validate_identity(namespace, catalog_item_id, app_name, platform)?;
        if !parent.is_absolute() || !parent.is_dir() {
            return Err(EpicError::Service {
                message: "backend.epic.choose_existing_folder".into(),
            });
        }
        let parent = parent.canonicalize()?;
        let destination =
            installation_directory(&parent, namespace, catalog_item_id, app_name, platform);
        let (mut fetched, account_id) = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(EpicError::Cancelled),
            result = self.prepare_download(namespace, catalog_item_id, app_name, platform, &destination) => result?,
        };
        let network_bytes = fetched.network_bytes;
        let manifest = Arc::new(fetched.manifest.take().ok_or(EpicError::Protocol)?);
        cancel.check()?;
        if self.session.lock().await.is_none() {
            return Err(EpicError::Unauthorized);
        }
        // Like Steam, retain a process-wide destination lock until the final
        // receipt is published. The engine's lock also protects detached workers.
        let _installation_lock = lock_installation(&destination)?;
        if fetched.network_bytes != 0
            && let Some(key) = &fetched.cache_key
        {
            // This bounded atomic write remains under the destination lock;
            // cancelling or dropping the future cannot detach a cache writer.
            cancel.check()?;
            details::manifest_cache::store(&destination, key, &fetched.bytes)?;
        }
        drop(fetched.bytes);
        let receipt = checked_file_path(&destination, Path::new(RECEIPT_FILE))?;
        if receipt.exists() {
            std::fs::remove_file(&receipt)?;
        }

        let progress = transfer.clone();
        let summary = ContentClient::new(self.api.http.clone())
            .download(
                Arc::clone(&manifest),
                &fetched.base_urls,
                &destination,
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
        transfer.update(|progress| {
            progress.completed_bytes = summary.total_bytes;
            progress.total_bytes = summary.total_bytes;
            progress.network_bytes = network_bytes.saturating_add(summary.downloaded_bytes);
            progress.content_bytes = summary.content_bytes;
            progress.current_file.clear();
        });
        let installation = Installation {
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
        };
        checked_file_path(&destination, Path::new(RECEIPT_FILE))?;
        forge_core::write_atomic(
            &receipt,
            &serde_json::to_vec_pretty(&installation).map_err(|_| EpicError::Protocol)?,
        )?;
        Ok(installation)
    }

    async fn prepare_download(
        &self,
        namespace: &str,
        catalog_item_id: &str,
        app_name: &str,
        platform: &str,
        cache_directory: &Path,
    ) -> Result<(details::FetchedManifest, String), EpicError> {
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
        Ok((fetched, self.credentials().await?.account_id))
    }
}

fn reject_symlink(path: &Path) -> Result<(), EpicError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(EpicError::UnsafePath(path.to_string_lossy().into_owned()))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn lock_installation(destination: &Path) -> Result<File, EpicError> {
    reject_symlink(destination)?;
    std::fs::create_dir_all(destination)?;
    reject_symlink(destination)?;
    let lock_path = checked_file_path(destination, Path::new(LOCK_FILE))?;
    try_lock_file(&lock_path)?.ok_or(EpicError::AlreadyDownloading)
}

fn installation_directory(
    parent: &Path,
    namespace: &str,
    catalog_item_id: &str,
    app_name: &str,
    platform: &str,
) -> PathBuf {
    parent.join(installation_directory_name(
        namespace,
        catalog_item_id,
        app_name,
        platform,
    ))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installation_lock_covers_receipt_publication_and_releases_on_drop() {
        let parent = tempfile::tempdir().unwrap();
        let destination = parent.path().join("game");
        let first = lock_installation(&destination).unwrap();
        let receipt = destination.join(".forge-install.json");
        forge_core::write_atomic(&receipt, b"installed").unwrap();
        assert!(matches!(
            lock_installation(&destination),
            Err(EpicError::AlreadyDownloading)
        ));
        assert_eq!(std::fs::read(&receipt).unwrap(), b"installed");
        drop(first);
        assert!(lock_installation(&destination).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn installation_lock_rejects_symlinks_hardlinks_and_special_paths() {
        let parent = tempfile::tempdir().unwrap();
        let target = parent.path().join("outside");
        std::fs::write(&target, "unchanged").unwrap();
        let symlink_root = parent.path().join("symlink");
        std::fs::create_dir(&symlink_root).unwrap();
        std::os::unix::fs::symlink(&target, symlink_root.join(".forge-install.lock")).unwrap();
        assert!(matches!(
            lock_installation(&symlink_root),
            Err(EpicError::UnsafePath(_))
        ));
        let hardlink_root = parent.path().join("hardlink");
        std::fs::create_dir(&hardlink_root).unwrap();
        std::fs::hard_link(&target, hardlink_root.join(".forge-install.lock")).unwrap();
        assert!(matches!(
            lock_installation(&hardlink_root),
            Err(EpicError::UnsafePath(_))
        ));
        let special_root = parent.path().join("special");
        std::fs::create_dir_all(special_root.join(".forge-install.lock")).unwrap();
        assert!(matches!(
            lock_installation(&special_root),
            Err(EpicError::UnsafePath(_))
        ));
        let linked_root = parent.path().join("linked-root");
        std::os::unix::fs::symlink(&special_root, &linked_root).unwrap();
        assert!(matches!(
            lock_installation(&linked_root),
            Err(EpicError::UnsafePath(_))
        ));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "unchanged");
    }
}
