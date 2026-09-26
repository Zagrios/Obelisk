//! Restartable diagnostic restricted to one newly named disposable Cloud file.
use super::*;
use crate::cloud::{CloudListing, CloudUpload, UploadPhase, UploadTransaction};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

const SIZE: usize = 2 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Probe {
    before: CloudListing,
    name: String,
    timestamp: u64,
    interrupted: PathBuf,
    prefix_bytes: Option<usize>,
    retry: Option<PathBuf>,
    cleanup: Option<PathBuf>,
}

fn save(directory: &Path, probe: &Probe) -> Result<(), CloudError> {
    let mut file = tempfile::NamedTempFile::new_in(directory).map_err(|_| CloudError::Storage)?;
    serde_json::to_writer(file.as_file_mut(), probe).map_err(|_| CloudError::Storage)?;
    file.as_file().sync_all().map_err(|_| CloudError::Storage)?;
    file.persist(directory.join("interruption-probe.json"))
        .map_err(|_| CloudError::Storage)?;
    crate::cloud::upload::sync(directory)
}

fn payload() -> Vec<u8> {
    // Reproducible diagnostic bytes; contains no save or account material.
    (0..SIZE)
        .map(|i| ((i.wrapping_mul(73) ^ (i >> 8)) & 255) as u8)
        .collect()
}

fn upload(probe: &Probe) -> CloudUpload {
    CloudUpload {
        name: probe.name.clone(),
        bytes: payload(),
        timestamp: probe.timestamp,
        platforms: u32::MAX,
    }
}

fn original_files(
    listing: &CloudListing,
    fixture: &str,
) -> BTreeMap<String, crate::cloud::CloudFile> {
    listing
        .files
        .iter()
        .filter(|f| f.persist_state == 0 && f.name != fixture)
        .map(|f| (f.name.clone(), f.clone()))
        .collect()
}

impl Client {
    /// First invocation interrupts an HTTP body before CM commit. Invoke again
    /// after restarting SailOver to close the old batch, verify remote state, create
    /// a fresh attempt, verify it and remove only the diagnostic file.
    pub async fn cloud_interruption_probe(
        &self,
        app_id: u32,
        directory: &Path,
    ) -> Result<String, CloudError> {
        let preferences = self.cloud_preferences(app_id).await?;
        if preferences.account_enabled != Some(true) || preferences.game_enabled != Some(true) {
            return Err(CloudError::Revoked);
        }
        let directory = directory.canonicalize().map_err(|_| CloudError::Storage)?;
        let journal = directory.join("interruption-probe.json");
        let authorized = || self.cloud_authorized();
        let mut probe: Probe = match fs::read(&journal) {
            Ok(bytes) if bytes.len() <= 64 * 1024 * 1024 => {
                serde_json::from_slice(&bytes).map_err(|_| CloudError::Storage)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let backup: serde_json::Value =
                    serde_json::from_str(&self.cloud_backup(app_id, &directory).await?)
                        .map_err(|_| CloudError::Storage)?;
                let before: CloudListing = serde_json::from_value(backup["listing"].clone())
                    .map_err(|_| CloudError::Storage)?;
                let cm = &self.cm;
                let quota = cm.cloud_quota(app_id).await?;
                if quota
                    .max_bytes
                    .zip(quota.existing_bytes)
                    .is_none_or(|(max, used)| max.saturating_sub(used) < SIZE as u64)
                    || quota
                        .max_files
                        .zip(quota.existing_files)
                        .is_none_or(|(max, used)| max <= used)
                {
                    return Err(CloudError::Invalid("probe quota"));
                }
                let marker = tempfile::Builder::new()
                    .prefix("fixture-")
                    .tempdir_in(&directory)
                    .map_err(|_| CloudError::Storage)?;
                let name = format!(
                    "__sailover_cloud_interruption_{}.bin",
                    marker
                        .path()
                        .file_name()
                        .and_then(|s| s.to_str())
                        .ok_or(CloudError::Storage)?
                );
                if before.files.iter().any(|f| f.name == name) {
                    return Err(CloudError::Changed);
                }
                let timestamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|_| CloudError::Storage)?
                    .as_secs();
                let mut transaction = UploadTransaction::create(
                    &directory,
                    before.clone(),
                    vec![CloudUpload {
                        name: name.clone(),
                        bytes: payload(),
                        timestamp,
                        platforms: u32::MAX,
                    }],
                    vec![],
                )?;
                let mut probe = Probe {
                    before,
                    name,
                    timestamp,
                    interrupted: transaction.directory().into(),
                    prefix_bytes: None,
                    retry: None,
                    cleanup: None,
                };
                save(&directory, &probe)?;
                let prefix = cm
                    .cloud_interrupt_upload_probe(
                        &self.http,
                        &mut transaction,
                        &directory.join("transport-trace.jsonl"),
                        authorized,
                    )
                    .await?;
                if !matches!(transaction.phase(), UploadPhase::Open { .. })
                    || transaction.is_settled()
                {
                    return Err(CloudError::Invalid("probe interruption phase"));
                }
                probe.prefix_bytes = Some(prefix);
                save(&directory, &probe)?;
                return serde_json::to_string_pretty(&serde_json::json!({
                    "operation":"interrupted_http_upload", "status":"awaiting_restart", "app_id":app_id,
                    "payload_bytes":SIZE, "prefix_consumed_by_http_client":prefix,
                    "provider_receipt_of_prefix_proven":false, "file_commit_sent":false,
                    "phase":transaction.phase(), "transaction":transaction.directory(),
                    "original_file_count":original_files(&probe.before, &probe.name).len(),
                    "before_revision":probe.before.change_number
                })).map_err(|_| CloudError::Storage);
            }
            _ => return Err(CloudError::Storage),
        };
        if probe.before.app_id != app_id
            || !probe.name.starts_with("__sailover_cloud_interruption_")
            || probe.name.contains('/')
            || probe.before.files.iter().any(|f| f.name == probe.name)
            || [
                &Some(probe.interrupted.clone()),
                &probe.retry,
                &probe.cleanup,
            ]
            .into_iter()
            .flatten()
            .any(|p| p.parent() != Some(directory.as_path()))
        {
            return Err(CloudError::Invalid("probe scope"));
        }
        let cm = &self.cm;
        let mut interrupted = UploadTransaction::open(&probe.interrupted)?;
        cm.cloud_settle_upload_probe(
            &self.http,
            &mut interrupted,
            &directory.join("transport-trace.jsonl"),
            authorized,
        )
        .await?;
        if !interrupted.is_settled() {
            return Err(CloudError::Uncertain);
        }
        let originals = original_files(&probe.before, &probe.name);
        let mut fresh = cm.cloud_listing(app_id).await?;
        if fresh.steam_id != probe.before.steam_id
            || original_files(&fresh, &probe.name) != originals
        {
            return Err(CloudError::Changed);
        }
        // Verify every original file after closure, before admitting a new plan.
        for file in originals.values() {
            if !authorized() {
                return Err(CloudError::Revoked);
            }
            cm.cloud_download(&self.http, app_id, file).await?;
        }
        if cm.cloud_listing(app_id).await? != fresh {
            return Err(CloudError::Changed);
        }
        let after_settlement = fresh.change_number;
        if probe.cleanup.is_none() {
            let mut retry = if let Some(path) = &probe.retry {
                UploadTransaction::open(path)?
            } else {
                if fresh.files.iter().any(|f| f.name == probe.name) {
                    return Err(CloudError::Changed);
                }
                let retry = UploadTransaction::create(
                    &directory,
                    fresh.clone(),
                    vec![upload(&probe)],
                    vec![],
                )?;
                probe.retry = Some(retry.directory().into());
                save(&directory, &probe)?;
                retry
            };
            fresh = cm
                .cloud_upload_probe(
                    &self.http,
                    &mut retry,
                    &directory.join("transport-trace.jsonl"),
                    authorized,
                )
                .await?;
            let cleanup = UploadTransaction::create(
                &directory,
                fresh.clone(),
                vec![],
                vec![probe.name.clone()],
            )?;
            probe.cleanup = Some(cleanup.directory().into());
            save(&directory, &probe)?;
        }
        let mut cleanup =
            UploadTransaction::open(probe.cleanup.as_ref().ok_or(CloudError::Storage)?)?;
        let after = cm
            .cloud_upload(&self.http, &mut cleanup, authorized)
            .await?;
        if original_files(&after, &probe.name) != originals
            || after
                .files
                .iter()
                .any(|f| f.name == probe.name && f.persist_state == 0)
        {
            return Err(CloudError::Changed);
        }
        serde_json::to_string_pretty(&serde_json::json!({
            "operation":"interrupted_http_upload", "status":"verified", "app_id":app_id,
            "payload_bytes":SIZE, "prefix_consumed_by_http_client":probe.prefix_bytes,
            "provider_receipt_of_prefix_proven":false, "interrupted_file_commit_sent":false,
            "settled_phase":interrupted.phase(), "before_revision":probe.before.change_number,
            "after_settlement_revision":after_settlement, "cleaned_revision":after.change_number,
            "original_file_count":originals.len(), "original_remote_contents_preserved":true,
            "fresh_attempt_verified":true, "fixture_removed":true
        }))
        .map_err(|_| CloudError::Storage)
    }
}
