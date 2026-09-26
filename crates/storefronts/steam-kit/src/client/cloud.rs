use super::*;
use crate::cloud::{
    CloudError, deadline,
    upload::{sync, write_private},
};
use std::{fs, os::unix::fs::PermissionsExt};

#[cfg(debug_assertions)]
mod interruption_probe;

impl Client {
    /// Publisher metadata and Mac manifests. Never downloads or mutates saves.
    #[cfg(debug_assertions)]
    pub async fn cloud_autocloud_metadata(&self) -> Result<String, CloudError> {
        deadline(240, async {
            let cm = &self.cm;
            let games = cm.owned_games().await?;
            let mut apps = Vec::new();
            let mut inspected = 0;
            for batch in games.chunks(30) {
                if !self.cloud_authorized() {
                    return Err(CloudError::Revoked);
                }
                let ids: Vec<_> = batch.iter().map(|g| g.app_id).collect();
                for app in cm.app_infos(&ids).await? {
                    inspected += 1;
                    if let Some(ufs) = app.cloud {
                        let listing = if app.os_list.iter().any(|os| os == "macos")
                            && ufs.field("savefiles").is_some()
                        {
                            Some(cm.cloud_listing(app.app_id).await?)
                        } else {
                            None
                        };
                        apps.push(serde_json::json!({"app_id":app.app_id,
                            "name":app.name,"os_list":app.os_list,"ufs":ufs,
                            "listing":listing}));
                    }
                }
            }
            serde_json::to_string_pretty(&serde_json::json!({
                "operation":"read_only_autocloud_metadata", "inspected":inspected,
                "owned_count":games.len(), "apps":apps
            }))
            .map_err(|_| CloudError::Storage)
        })
        .await
    }
    /// Whether Cloud transfers may still run. Sign-out clears this; UI closure
    /// and lost IPC do not.
    pub(crate) fn cloud_authorized(&self) -> bool {
        self.cloud_authorized
            .load(std::sync::atomic::Ordering::Acquire)
    }
    /// Read-only discovery of owned, empty Cloud namespaces for isolated SDK
    /// validation. This neither acquires licenses nor starts native sync.
    #[cfg(debug_assertions)]
    pub async fn cloud_sdk_candidates(&self) -> Result<String, CloudError> {
        let cm = &self.cm;
        let games = cm.owned_games().await?;
        let mut candidates = Vec::new();
        let mut inspected = 0;
        for batch in games.chunks(30).take(4) {
            let ids: Vec<_> = batch.iter().map(|g| g.app_id).collect();
            for app in cm.app_infos(&ids).await? {
                if !self.cloud_authorized() {
                    return Err(CloudError::Revoked);
                }
                inspected += 1;
                let Some(ufs) = app.cloud else {
                    continue;
                };
                // An empty explicit AutoCloud rule list still needs its own
                // interpretation; require an absent list for this SDK probe.
                if ufs.field("savefiles").is_some() {
                    continue;
                }
                let quota = match cm.cloud_quota(app.app_id).await {
                    Ok(q)
                        if q.max_bytes.is_some_and(|n| n >= 1024)
                            && q.max_files.is_some_and(|n| n >= 2)
                            && q.existing_files == Some(0)
                            && q.existing_bytes == Some(0) =>
                    {
                        q
                    }
                    _ => continue,
                };
                let listing = cm.cloud_listing(app.app_id).await?;
                if listing.files.iter().any(|f| f.persist_state == 0) {
                    continue;
                }
                candidates.push(serde_json::json!({"app_id":app.app_id,
                    "name":batch.iter().find(|g| g.app_id == app.app_id).map(|g| &g.name),
                    "quota":quota,"revision":listing.change_number,"ufs":ufs}));
                if candidates.len() == 3 {
                    break;
                }
            }
            if candidates.len() == 3 {
                break;
            }
        }
        serde_json::to_string_pretty(
            &serde_json::json!({"operation":"read_only_sdk_candidate_search",
            "inspected":inspected,"candidates":candidates}),
        )
        .map_err(|_| CloudError::Storage)
    }
    /// Live rules and remote identities for preserving local saves before
    /// uninstall. This reads CM metadata only; it does not transfer save bytes.
    pub async fn cloud_archive_metadata(
        &self,
        app_id: u32,
    ) -> Result<Option<(crate::cloud::CloudMetadata, crate::cloud::CloudListing)>, CloudError> {
        deadline(120, async {
            if !self.cloud_authorized() {
                return Err(CloudError::Revoked);
            }
            let cm = &self.cm;
            let Some(metadata) = cm.app_info(app_id).await?.cloud else {
                return Ok(None);
            };
            if metadata.field("savefiles").is_none() {
                return Ok(None);
            }
            let listing = cm.cloud_listing(app_id).await?;
            if !self.cloud_authorized() {
                return Err(CloudError::Revoked);
            }
            Ok(Some((metadata, listing)))
        })
        .await
    }
    /// Opt-in validation with no native runtime or game running. A complete
    /// verified backup precedes the unique disposable file; only that file is
    /// deleted. This does not activate synchronization for production launches.
    #[cfg(debug_assertions)]
    pub async fn cloud_transfer_probe(
        &self,
        app_id: u32,
        directory: &Path,
    ) -> Result<String, CloudError> {
        let preferences = self.cloud_preferences(app_id).await?;
        if preferences.account_enabled != Some(true) || preferences.game_enabled != Some(true) {
            return Err(CloudError::Revoked);
        }
        let backup = self.cloud_backup(app_id, directory).await?;
        let backup: serde_json::Value =
            serde_json::from_str(&backup).map_err(|_| CloudError::Storage)?;
        let before: crate::cloud::CloudListing =
            serde_json::from_value(backup["listing"].clone()).map_err(|_| CloudError::Storage)?;
        let directory = directory.canonicalize().map_err(|_| CloudError::Storage)?;
        let marker = tempfile::Builder::new()
            .prefix("fixture-")
            .tempdir_in(&directory)
            .map_err(|_| CloudError::Storage)?;
        let name = format!(
            "__sailover_cloud_validation_{}.bin",
            marker
                .path()
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or(CloudError::Storage)?
        );
        if before.files.iter().any(|file| file.name == name) {
            return Err(CloudError::Changed);
        }
        let bytes = b"SailOver Steam Cloud transport validation\nOnly this disposable file is created and removed.\n".to_vec();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| CloudError::Storage)?
            .as_secs();
        let mut upload = crate::cloud::UploadTransaction::create(
            &directory,
            before.clone(),
            vec![crate::cloud::CloudUpload {
                name: name.clone(),
                bytes,
                timestamp,
                platforms: u32::MAX,
            }],
            vec![],
        )?;
        let cm = &self.cm;
        let authorized = || self.cloud_authorized();
        let uploaded = cm.cloud_upload(&self.http, &mut upload, authorized).await?;
        let mut cleanup = crate::cloud::UploadTransaction::create(
            &directory,
            uploaded.clone(),
            vec![],
            vec![name.clone()],
        )?;
        let after = cm
            .cloud_upload(&self.http, &mut cleanup, authorized)
            .await?;
        let persisted = |listing: &crate::cloud::CloudListing| -> std::collections::BTreeMap<_, _> {
            listing
                .files
                .iter()
                .filter(|file| file.persist_state == 0)
                .map(|file| {
                    (
                        file.name.clone(),
                        (file.sha1.clone(), file.size, file.platforms),
                    )
                })
                .collect()
        };
        if persisted(&before) != persisted(&after) {
            return Err(CloudError::Changed);
        }
        serde_json::to_string_pretty(&serde_json::json!({
            "operation":"disposable_cloud_upload_readback_delete", "status":"verified", "app_id":app_id,
            "original_file_count":persisted(&before).len(), "before_revision":before.change_number,
            "uploaded_revision":uploaded.change_number, "cleaned_revision":after.change_number,
            "upload_directory":upload.directory(), "cleanup_directory":cleanup.directory(),
            "backup_directory":backup["backup_directory"], "preferences":preferences,
            "original_remote_contents_preserved":true, "fixture_removed":true
        })).map_err(|_| CloudError::Storage)
    }

    pub async fn cloud_preferences(
        &self,
        app_id: u32,
    ) -> Result<crate::cloud::CloudPreferences, CloudError> {
        deadline(30, async {
            self.cm.cloud_preferences(&self.http, app_id).await
        })
        .await
    }
    /// Read-only remote operation. Backups use numbered local filenames; remote
    /// names, including Auto-Cloud roots, never become filesystem paths.
    pub async fn cloud_backup(&self, app_id: u32, directory: &Path) -> Result<String, CloudError> {
        deadline(120, self.cloud_backup_inner(app_id, directory)).await
    }

    async fn cloud_backup_inner(
        &self,
        app_id: u32,
        directory: &Path,
    ) -> Result<String, CloudError> {
        if !directory.is_absolute()
            || !fs::symlink_metadata(directory).is_ok_and(|m| m.file_type().is_dir())
        {
            return Err(CloudError::Storage);
        }
        let directory = directory.canonicalize().map_err(|_| CloudError::Storage)?;
        let cm = &self.cm;
        let metadata = cm.app_info(app_id).await?;
        let quota = cm.cloud_quota(app_id).await?;
        let before = cm.cloud_listing(app_id).await?;
        let inspection = serde_json::json!({"operation":"read_only_cloud_inspection", "listing":before, "quota":quota, "ufs":metadata.cloud});
        let inspection_path =
            directory.join(format!("inspection-{app_id}-{}.json", before.change_number));
        match fs::File::create_new(inspection_path) {
            Ok(output) => {
                let bytes =
                    serde_json::to_vec_pretty(&inspection).map_err(|_| CloudError::Storage)?;
                write_private(output, &bytes)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(CloudError::Storage),
        }

        // A fresh generation prevents an interrupted backup from replacing a
        // previously verified one. A success manifest is published last.
        let backup = tempfile::Builder::new()
            .prefix("remote-")
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(&directory)
            .map_err(|_| CloudError::Storage)?;
        let mut files = Vec::new();
        for (index, file) in before.files.iter().enumerate() {
            if file.persist_state != 0 {
                continue;
            }
            let bytes = cm.cloud_download(&self.http, app_id, file).await?;
            let local_name = format!("file-{index:05}.bin");
            let output = fs::File::create(backup.path().join(&local_name))
                .map_err(|_| CloudError::Storage)?;
            write_private(output, &bytes)?;
            files.push(serde_json::json!({"remote":file,"backup_file":local_name}));
        }
        let after = cm.cloud_listing(app_id).await?;
        if before != after {
            return Err(CloudError::Changed);
        }
        let report = serde_json::json!({"operation":"read_only_cloud_backup", "steam_id":before.steam_id,
            "app_id":app_id,"change_number":before.change_number,"quota":quota,"listing":before,
            "ufs":metadata.cloud,"files":files,"backup_directory":backup.path()});
        let json = serde_json::to_string_pretty(&report).map_err(|_| CloudError::Storage)?;
        let output = fs::File::create(backup.path().join("manifest.json"))
            .map_err(|_| CloudError::Storage)?;
        write_private(output, json.as_bytes())?;
        sync(backup.path())?;
        let _ = backup.keep();
        sync(&directory)?;
        Ok(json)
    }
}
