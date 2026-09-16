//! Account-scoped downloads use the same cancellation and atomic receipt
//! primitives as Steam. Content formats stay in the storefront's own engine.
use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
};

use forge_core::{
    install::{LOCK_FILE, RECEIPT_FILE, checked_file_path, hex, try_lock_file},
    transfer::CancelToken,
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

#[derive(Default)]
pub(super) struct Downloads {
    active: Mutex<Option<CancelToken>>,
    progress: Arc<Mutex<Option<TransferProgress>>>,
}

/// Dropping the UniFFI future must stop workers and release the session slot,
/// including when a caller disappears without explicitly requesting cancel.
struct ActiveDownload<'a>(&'a Downloads);

impl Drop for ActiveDownload<'_> {
    fn drop(&mut self) {
        let mut active = self.0.active.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(cancel) = active.take() {
            cancel.cancel();
        }
        let mut progress = self
            .0
            .progress
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(progress) = progress.as_mut()
            && matches!(
                progress.state,
                TransferState::Preparing | TransferState::Verifying | TransferState::Downloading
            )
        {
            progress.state = TransferState::Cancelled;
            progress.message = "backend.epic.cancelled".into();
        }
    }
}

impl Client {
    pub fn progress(&self) -> Option<TransferProgress> {
        self.downloads
            .progress
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn cancel_download(&self) {
        if let Some(cancel) = self
            .downloads
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            cancel.cancel();
        }
    }

    pub async fn download(
        &self,
        namespace: &str,
        catalog_item_id: &str,
        app_name: &str,
        platform: &str,
        parent: &Path,
    ) -> Result<Installation, EpicError> {
        let cancel = {
            let mut active = self
                .downloads
                .active
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if active.is_some() {
                return Err(EpicError::AlreadyDownloading);
            }
            let cancel = CancelToken::new();
            *active = Some(cancel.clone());
            cancel
        };
        let _active = ActiveDownload(&self.downloads);
        *self
            .downloads
            .progress
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(TransferProgress {
            app_name: app_name.into(),
            state: TransferState::Preparing,
            completed_bytes: 0,
            total_bytes: 0,
            network_bytes: 0,
            content_bytes: 0,
            current_file: String::new(),
            message: "backend.epic.preparing".into(),
        });
        // The content engine observes the same token and drains its disk worker.
        // Only the read-only preparation phase races directly against cancel.
        let result = self
            .download_inner(
                namespace,
                catalog_item_id,
                app_name,
                platform,
                parent,
                &cancel,
            )
            .await;
        let mut progress = self
            .downloads
            .progress
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(progress) = progress.as_mut() {
            match &result {
                Ok(_) => {
                    progress.state = TransferState::Complete;
                    progress.message = if progress.content_bytes == 0 {
                        "backend.epic.files_up_to_date"
                    } else {
                        "backend.epic.download_complete"
                    }
                    .into();
                }
                Err(error) => {
                    progress.state =
                        if cancel.is_cancelled() || matches!(error, EpicError::Cancelled) {
                            TransferState::Cancelled
                        } else {
                            TransferState::Failed
                        };
                    progress.message = error.to_string();
                }
            }
        }
        result
    }

    async fn download_inner(
        &self,
        namespace: &str,
        catalog_item_id: &str,
        app_name: &str,
        platform: &str,
        parent: &Path,
        cancel: &CancelToken,
    ) -> Result<Installation, EpicError> {
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

        let progress = self.downloads.progress.clone();
        let callback_cancel = cancel.clone();
        let summary = ContentClient::new(self.api.http.clone())
            .download(
                Arc::clone(&manifest),
                &fetched.base_urls,
                &destination,
                cancel,
                move |snapshot| {
                    if callback_cancel.is_cancelled() {
                        return;
                    }
                    debug_assert!(snapshot.completed_files <= snapshot.total_files);
                    let mut progress = progress.lock().unwrap_or_else(PoisonError::into_inner);
                    if let Some(progress) = progress.as_mut() {
                        progress.state = snapshot.phase.into();
                        progress.completed_bytes = snapshot.completed_bytes;
                        progress.total_bytes = snapshot.total_bytes;
                        progress.network_bytes =
                            network_bytes.saturating_add(snapshot.network_bytes);
                        progress.content_bytes = snapshot.content_bytes;
                        progress.current_file = snapshot.current_file;
                        progress.message.clear();
                    }
                },
            )
            .await?;
        cancel.check()?;
        if self.session.lock().await.is_none() {
            return Err(EpicError::Unauthorized);
        }
        debug_assert!(summary.reused_bytes <= summary.total_bytes);
        debug_assert_eq!(summary.completed_files as usize, manifest.files.len());
        if let Some(progress) = self
            .downloads
            .progress
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
        {
            progress.completed_bytes = summary.total_bytes;
            progress.total_bytes = summary.total_bytes;
            progress.network_bytes = network_bytes.saturating_add(summary.downloaded_bytes);
            progress.content_bytes = summary.content_bytes;
            progress.current_file.clear();
        }
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
