use super::*;
use std::collections::VecDeque;

#[test]
fn upload_platform_wire_defaults_do_not_turn_none_into_all() {
    // Tag 7 is absent only for Valve's schema default (all platforms).
    let none = wire::BeginUploadRequest {
        platforms: 0,
        ..Default::default()
    }
    .encode_to_vec();
    assert_eq!(none, [0x38, 0x00]);
    assert_eq!(
        wire::BeginUploadRequest::decode(none.as_slice())
            .unwrap()
            .platforms,
        0
    );
    assert_eq!(
        wire::BeginUploadRequest::decode(&[][..]).unwrap().platforms,
        u32::MAX
    );
}

fn file(name: &str, bytes: &[u8]) -> CloudFile {
    CloudFile {
        name: name.into(),
        sha1: crate::content::hex(&Sha1::digest(bytes)),
        size: bytes.len() as u32,
        timestamp: 1,
        persist_state: 0,
        platforms: 7,
        reupload_requested: false,
    }
}
fn listing(files: Vec<CloudFile>) -> CloudListing {
    CloudListing {
        steam_id: 0x0110000100000001,
        app_id: 42,
        change_number: 2,
        files,
    }
}
fn fixture() -> (tempfile::TempDir, UploadTransaction, Fake) {
    let root = tempfile::tempdir().unwrap();
    let parent = root.path().canonicalize().unwrap();
    let before = listing(vec![file("keep", b"same"), file("delete", b"old")]);
    let job = UploadTransaction::create(
        &parent,
        before.clone(),
        vec![CloudUpload {
            name: "new".into(),
            bytes: b"new-data".to_vec(),
            timestamp: 4,
            platforms: 7,
        }],
        vec!["delete".into()],
    )
    .unwrap();
    (
        root,
        job,
        Fake {
            remote: before,
            bodies: BTreeMap::from([
                ("keep".into(), b"same".to_vec()),
                ("delete".into(), b"old".to_vec()),
            ]),
            calls: vec![],
            fault: None,
            commit: true,
            blocks: VecDeque::new(),
            pending: None,
            corrupt_download: false,
            readback_changed: false,
            change_after_download: false,
            pause_block: false,
            move_journal: None,
            abort_receipt: Some(false),
        },
    )
}

#[test]
fn opening_requires_current_upload_schema_and_recovery_information() {
    let (_root, job, _server) = fixture();
    let directory = job.directory().to_owned();
    let path = directory.join("state.json");
    let current = serde_json::to_value(&job.state).unwrap();
    drop(job);
    assert!(UploadTransaction::open(&directory).is_ok());
    for case in 0..3 {
        let mut old = current.clone();
        match case {
            0 => old["schema"] = 1.into(),
            1 => old["schema"] = 2.into(),
            2 => {
                old.as_object_mut().unwrap().remove("unanswered_mutation");
            }
            _ => unreachable!(),
        }
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        let before = fs::read(&path).unwrap();
        assert!(UploadTransaction::open(&directory).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
    }
}

struct Fake {
    remote: CloudListing,
    bodies: BTreeMap<String, Vec<u8>>,
    calls: Vec<String>,
    fault: Option<&'static str>,
    commit: bool,
    blocks: VecDeque<Vec<u8>>,
    pending: Option<wire::BeginUploadRequest>,
    corrupt_download: bool,
    readback_changed: bool,
    change_after_download: bool,
    pause_block: bool,
    move_journal: Option<(PathBuf, PathBuf)>,
    abort_receipt: Option<bool>,
}
impl Fake {
    fn response<R: Message + Default>(
        &mut self,
        name: &str,
        response: &impl Message,
    ) -> Result<R, CloudError> {
        if self.fault == Some(name) {
            self.fault = None;
            return Err(CloudError::Network);
        }
        Ok(R::decode(response.encode_to_vec().as_slice()).unwrap())
    }
}
impl Transport for Fake {
    async fn call<R: Message + Default>(
        &mut self,
        name: &str,
        request: &impl Message,
    ) -> Result<R, CloudError> {
        self.calls.push(name.into());
        let data = request.encode_to_vec();
        match name {
            "Cloud.BeginAppUploadBatch#1" => {
                let req = wire::BeginBatchRequest::decode(data.as_slice()).unwrap();
                assert_eq!(req.uploads, ["new"]);
                assert_eq!(req.deletes, ["delete"]);
                if let Some((from, to)) = self.move_journal.take() {
                    fs::rename(from, to).unwrap();
                }
                self.response(
                    name,
                    &wire::BeginBatchResponse {
                        batch_id: Some(123),
                        change_number: Some(3),
                    },
                )
            }
            "Cloud.ClientBeginFileUpload#1" => {
                if self.pending.is_some() {
                    return Err(CloudError::Cm(crate::cm::CmError::Rejected {
                        action: "fixture file busy",
                        result: 10,
                    }));
                }
                let req = wire::BeginUploadRequest::decode(data.as_slice()).unwrap();
                assert!(!req.can_encrypt);
                assert_eq!(req.file_size, req.raw_size);
                self.pending = Some(req);
                let plan = wire::BeginUploadResponse {
                    encrypted: false,
                    blocks: vec![
                        wire::UploadBlock {
                            https: true,
                            host: "steamcloud-test.storage.googleapis.com".into(),
                            path: "/part1?secret=never-log".into(),
                            method: 4,
                            offset: 0,
                            length: 3,
                            ..Default::default()
                        },
                        wire::UploadBlock {
                            https: true,
                            host: "steamcloud-test.storage.googleapis.com".into(),
                            path: "/part2".into(),
                            method: 4,
                            offset: 3,
                            length: 5,
                            ..Default::default()
                        },
                    ],
                };
                self.response(name, &plan)
            }
            "Cloud.ClientCommitFileUpload#1" => {
                let req = wire::CommitUploadRequest::decode(data.as_slice()).unwrap();
                let pending = self.pending.take().unwrap();
                assert_eq!(req.sha, pending.sha);
                assert_eq!(req.name, pending.name);
                if !req.succeeded {
                    self.calls.push("abort-file".into());
                    self.blocks.clear();
                }
                if self.commit && req.succeeded {
                    let bytes: Vec<_> = self.blocks.drain(..).flatten().collect();
                    assert_eq!(
                        crate::content::hex(&Sha1::digest(&bytes)),
                        crate::content::hex(&req.sha)
                    );
                    self.remote.files.retain(|file| file.name != req.name);
                    self.remote.files.push(file(&req.name, &bytes));
                    self.bodies.insert(req.name, bytes);
                }
                self.response(
                    name,
                    &wire::CommitUploadResponse {
                        committed: if req.succeeded {
                            Some(self.commit)
                        } else {
                            self.abort_receipt
                        },
                    },
                )
            }
            "Cloud.ClientDeleteFile#1" => {
                let req = wire::DeleteRequest::decode(data.as_slice()).unwrap();
                assert!(req.explicit);
                assert_eq!(req.batch_id, 123);
                self.remote.files.retain(|file| file.name != req.name);
                self.bodies.remove(&req.name);
                self.response(name, &wire::EmptyResponse {})
            }
            "Cloud.CompleteAppUploadBatchBlocking#1" => {
                let req = wire::CompleteBatchRequest::decode(data.as_slice()).unwrap();
                self.calls.push(format!("complete-result-{}", req.result));
                self.remote.change_number = 3;
                self.response(name, &wire::EmptyResponse {})
            }
            _ => panic!("unexpected test RPC"),
        }
    }
    async fn block(&mut self, block: &wire::UploadBlock, bytes: &[u8]) -> Result<(), CloudError> {
        self.calls.push("block".into());
        if self.fault == Some("block") {
            self.fault = None;
            return Err(CloudError::Network);
        }
        let request = block_request(block, bytes)?;
        self.blocks
            .push_back(request.body().unwrap().as_bytes().unwrap().into());
        if self.pause_block {
            std::future::pending::<()>().await;
        }
        Ok(())
    }
    async fn listing(&mut self, _: u32) -> Result<CloudListing, CloudError> {
        self.calls.push("listing".into());
        if self.readback_changed && self.remote.change_number == 3 {
            self.remote
                .files
                .push(file("concurrent", b"someone else's edit"));
            self.readback_changed = false;
        }
        Ok(self.remote.clone())
    }
    async fn download(&mut self, _: u32, file: &CloudFile) -> Result<Vec<u8>, CloudError> {
        self.calls.push("download".into());
        if self.change_after_download {
            self.remote.change_number += 1;
            self.change_after_download = false;
        }
        if self.corrupt_download {
            Ok(b"corrupt".to_vec())
        } else {
            Ok(self.bodies[&file.name].clone())
        }
    }
}

#[tokio::test]
async fn upload_readback_can_confirm_the_vault_without_downloading_twice() {
    let (root, mut job, mut server) = fixture();
    let scope = cloud_vault::Scope {
        steam_id: job.steam_id(),
        app_id: 42,
        cloud_app_id: 42,
    };
    let mut vault = cloud_vault::Vault::open(&root.path().join("vault"), scope).unwrap();
    vault.set_preferences(cloud_vault::Preferences {
        account_enabled: Some(true),
        game_enabled: Some(true),
    });
    let before = cloud_vault::RemoteView {
        revision: "2".into(),
        files: cloud_vault::Manifest(
            server
                .bodies
                .iter()
                .map(|(name, bytes)| (name.clone(), vault.stage(bytes, 7).unwrap()))
                .collect(),
        ),
    };
    vault.confirm_equal(&before.files, &before).unwrap();
    let desired = cloud_vault::Manifest(BTreeMap::from([
        ("keep".into(), vault.stage(b"same", 7).unwrap()),
        ("new".into(), vault.stage(b"new-data", 7).unwrap()),
    ]));
    let auth = vault.authorize(scope.steam_id).unwrap();
    let id = vault
        .queue_upload(&desired, &before, "fixture", auth)
        .unwrap();
    let permit = vault.begin_upload(&id, auth, &before).unwrap();
    let mut readback = cloud_vault::Manifest::default();
    let listing = execute_verified(&mut server, &mut job, &|| true, |file, bytes| {
        assert!(
            readback
                .0
                .insert(
                    file.name.clone(),
                    vault.stage(&bytes, file.platforms).unwrap()
                )
                .is_none()
        );
        Ok(())
    })
    .await
    .unwrap();
    let readback = cloud_vault::RemoteView {
        revision: listing.change_number.to_string(),
        files: readback,
    };
    vault.complete_upload(&permit, true, &readback).unwrap();
    assert_eq!(vault.baseline(), Some(&readback));
    assert!(vault.pending().is_none());
    assert_eq!(readback.files, desired);
    assert_eq!(
        server
            .calls
            .iter()
            .filter(|call| *call == "download")
            .count(),
        2
    );
}

#[tokio::test]
async fn provisional_readback_never_confirms_corruption_storage_failure_or_remote_change() {
    for fault in 0..3 {
        let (_root, mut job, mut server) = fixture();
        server.corrupt_download = fault == 0;
        server.change_after_download = fault == 2;
        let mut received = 0;
        let result = execute_verified(&mut server, &mut job, &|| true, |_, _| {
            received += 1;
            if fault == 1 {
                Err(CloudError::Storage)
            } else {
                Ok(())
            }
        })
        .await;
        assert!(matches!(
            (fault, result),
            (0, Err(CloudError::Integrity))
                | (1, Err(CloudError::Storage))
                | (2, Err(CloudError::Changed))
        ));
        assert_eq!(job.phase(), &UploadPhase::Acknowledged);
        assert_eq!(received, [0, 1, 2][fault]);
        let directory = job.directory().to_owned();
        drop(job);
        assert_eq!(
            UploadTransaction::open(&directory).unwrap().phase(),
            &UploadPhase::Acknowledged
        );
    }
}

#[tokio::test]
async fn interrupted_http_recovery_releases_file_before_a_fresh_upload() {
    let (root, mut job, mut server) = fixture();
    server.pause_block = true;
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(20),
            execute(&mut server, &mut job, &|| true),
        )
        .await
        .is_err()
    );
    let path = job.directory().to_owned();
    drop(job);
    let mut job = UploadTransaction::open(&path).unwrap();
    server.pause_block = false;
    server.calls.clear();
    settle(&mut server, &mut job, &|| true).await.unwrap();
    assert!(job.is_settled());
    let mut fresh = UploadTransaction::create(
        &root.path().canonicalize().unwrap(),
        server.remote.clone(),
        vec![CloudUpload {
            name: "new".into(),
            bytes: b"new-data".to_vec(),
            timestamp: 4,
            platforms: 7,
        }],
        vec!["delete".into()],
    )
    .unwrap();
    execute(&mut server, &mut fresh, &|| true)
        .await
        .expect("settlement must release the staged file");
    assert_eq!(server.bodies["new"], b"new-data");
    assert_eq!(server.bodies["keep"], b"same");
    assert_eq!(
        &server.calls[..4],
        [
            "Cloud.ClientCommitFileUpload#1",
            "abort-file",
            "Cloud.CompleteAppUploadBatchBlocking#1",
            "complete-result-2"
        ]
    );
}

#[tokio::test]
async fn http_failure_reports_failed_file_before_batch_closure() {
    let (_root, mut job, mut server) = fixture();
    server.fault = Some("block");
    assert!(matches!(
        execute(&mut server, &mut job, &|| true).await,
        Err(CloudError::Network)
    ));
    assert!(job.is_settled());
    assert!(server.pending.is_none());
    assert_eq!(
        &server.calls[server.calls.len() - 4..],
        [
            "Cloud.ClientCommitFileUpload#1",
            "abort-file",
            "Cloud.CompleteAppUploadBatchBlocking#1",
            "complete-result-2"
        ]
    );
    assert!(!server.bodies.contains_key("new"));
    assert_eq!(server.bodies["delete"], b"old");
}

#[tokio::test]
async fn lost_or_malformed_abort_receipt_never_allows_replay_or_settlement() {
    for (lost, receipt) in [(true, Some(false)), (false, None), (false, Some(true))] {
        let (_root, mut job, mut server) = fixture();
        server.pause_block = true;
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(20),
                execute(&mut server, &mut job, &|| true)
            )
            .await
            .is_err()
        );
        let path = job.directory().to_owned();
        drop(job);
        let mut job = UploadTransaction::open(&path).unwrap();
        server.abort_receipt = receipt;
        if lost {
            server.fault = Some("Cloud.ClientCommitFileUpload#1");
        }
        assert!(settle(&mut server, &mut job, &|| true).await.is_err());
        assert!(!job.is_settled());
        drop(job);
        let mut job = UploadTransaction::open(&path).unwrap();
        server.calls.clear();
        assert!(matches!(
            settle(&mut server, &mut job, &|| true).await,
            Err(CloudError::Uncertain)
        ));
        assert!(server.calls.is_empty());
        assert_eq!(server.bodies["keep"], b"same");
        assert!(!server.bodies.contains_key("new"));
    }
}

#[tokio::test]
async fn multipart_upload_requires_file_and_batch_acknowledgement_and_verified_readback() {
    let (_root, mut job, mut server) = fixture();
    let actual = execute(&mut server, &mut job, &|| true).await.unwrap();
    assert_eq!(actual.change_number, 3);
    assert_eq!(job.phase(), &UploadPhase::Verified);
    assert_eq!(server.bodies.get("new").unwrap(), b"new-data");
    assert_eq!(server.bodies.get("keep").unwrap(), b"same");
    assert!(!server.bodies.contains_key("delete"));
    assert_eq!(
        server.calls,
        [
            "listing",
            "Cloud.BeginAppUploadBatch#1",
            "Cloud.ClientBeginFileUpload#1",
            "block",
            "block",
            "Cloud.ClientCommitFileUpload#1",
            "Cloud.ClientDeleteFile#1",
            "Cloud.CompleteAppUploadBatchBlocking#1",
            "complete-result-1",
            "listing",
            "download",
            "download",
            "listing"
        ]
    );
    let directory = job.directory().to_owned();
    drop(job);
    let mut reopened = UploadTransaction::open(&directory).unwrap();
    server.calls.clear();
    execute(&mut server, &mut reopened, &|| true).await.unwrap();
    assert!(
        server
            .calls
            .iter()
            .all(|name| name == "listing" || name == "download")
    );
    let journal = fs::read_to_string(directory.join("state.json")).unwrap();
    assert!(!journal.contains("never-log") && !journal.contains("https:"));
}

#[tokio::test]
async fn interruption_at_each_remote_mutation_never_restarts_an_uncertain_transaction() {
    for fault in [
        "Cloud.BeginAppUploadBatch#1",
        "Cloud.ClientBeginFileUpload#1",
        "block",
        "Cloud.ClientCommitFileUpload#1",
        "Cloud.ClientDeleteFile#1",
        "Cloud.CompleteAppUploadBatchBlocking#1",
    ] {
        let (_root, mut job, mut server) = fixture();
        server.fault = Some(fault);
        assert!(
            execute(&mut server, &mut job, &|| true).await.is_err(),
            "{fault}"
        );
        let directory = job.directory().to_owned();
        drop(job);
        let mut reopened = UploadTransaction::open(&directory).unwrap();
        server.calls.clear();
        assert!(
            matches!(
                execute(&mut server, &mut reopened, &|| true).await,
                Err(CloudError::Uncertain)
            ),
            "{fault}"
        );
        assert!(server.calls.is_empty(), "{fault}");
        assert_eq!(server.bodies["keep"], b"same");
    }
}

#[tokio::test]
async fn dropping_an_active_transfer_preserves_uncertainty_and_never_commits() {
    let (_root, mut job, mut server) = fixture();
    server.pause_block = true;
    let cancelled = tokio::time::timeout(
        std::time::Duration::from_millis(20),
        execute(&mut server, &mut job, &|| true),
    )
    .await;
    assert!(cancelled.is_err());
    assert_eq!(server.blocks.len(), 1);
    assert!(!server.bodies.contains_key("new"));
    assert!(
        !server
            .calls
            .iter()
            .any(|call| call == "Cloud.ClientCommitFileUpload#1")
    );
    let directory = job.directory().to_owned();
    drop(job);
    let mut reopened = UploadTransaction::open(&directory).unwrap();
    server.calls.clear();
    assert!(matches!(
        execute(&mut server, &mut reopened, &|| true).await,
        Err(CloudError::Uncertain)
    ));
    assert!(server.calls.is_empty());
}

#[tokio::test]
async fn journal_failure_after_batch_admission_prevents_all_file_mutations() {
    let (_root, mut job, mut server) = fixture();
    let original = job.directory().to_owned();
    let moved = original.with_extension("moved");
    server.move_journal = Some((original.clone(), moved.clone()));
    assert!(matches!(
        execute(&mut server, &mut job, &|| true).await,
        Err(CloudError::Storage)
    ));
    assert!(
        !server
            .calls
            .iter()
            .any(|call| call == "Cloud.ClientBeginFileUpload#1")
    );
    fs::rename(&moved, &original).unwrap();
    server.calls.clear();
    assert!(matches!(
        execute(&mut server, &mut job, &|| true).await,
        Err(CloudError::Storage)
    ));
    assert!(server.calls.is_empty());
    drop(job);
    let mut reopened = UploadTransaction::open(&original).unwrap();
    assert!(matches!(
        execute(&mut server, &mut reopened, &|| true).await,
        Err(CloudError::Uncertain)
    ));
    assert!(server.calls.is_empty());
}

#[tokio::test]
async fn failed_commit_closes_batch_as_failure_and_does_not_delete_other_files() {
    let (_root, mut job, mut server) = fixture();
    server.commit = false;
    assert!(matches!(
        execute(&mut server, &mut job, &|| true).await,
        Err(CloudError::NotCommitted)
    ));
    assert!(server.calls.contains(&"complete-result-2".into()));
    assert!(!server.calls.contains(&"complete-result-1".into()));
    assert!(server.bodies.contains_key("delete"));
}

#[tokio::test]
async fn negative_commit_and_acknowledged_closure_are_durable_without_resending_files() {
    let (_root, mut job, mut server) = fixture();
    server.commit = false;
    assert!(matches!(
        execute(&mut server, &mut job, &|| true).await,
        Err(CloudError::NotCommitted)
    ));
    let path = job.directory().to_owned();
    drop(job);
    let mut job = UploadTransaction::open(&path).unwrap();
    // A negative file result and positive batch closure leave no unanswered
    // mutation. The existing transport discards that closure acknowledgement.
    assert_eq!(
        serde_json::to_value(job.phase()).unwrap(),
        serde_json::json!({"Closed": {"batch_id": 123}})
    );
    assert_eq!(server.bodies["keep"], b"same");
    assert!(server.bodies.contains_key("delete"));
    assert!(!server.bodies.contains_key("new"));
    assert!(job.is_settled());
    server.calls.clear();
    settle(&mut server, &mut job, &|| true).await.unwrap();
    assert!(server.calls.is_empty());
}

#[tokio::test]
async fn lost_closure_reply_can_be_settled_without_repeating_acknowledged_file_mutations() {
    let (_root, mut job, mut server) = fixture();
    server.fault = Some("Cloud.CompleteAppUploadBatchBlocking#1");
    assert!(matches!(
        execute(&mut server, &mut job, &|| true).await,
        Err(CloudError::Network)
    ));
    assert!(!job.is_settled());
    let path = job.directory().to_owned();
    drop(job);
    let mut job = UploadTransaction::open(&path).unwrap();
    server.calls.clear();
    settle(&mut server, &mut job, &|| true).await.unwrap();
    assert!(job.is_settled());
    assert_eq!(
        server.calls,
        [
            "Cloud.CompleteAppUploadBatchBlocking#1",
            "complete-result-2"
        ]
    );
    assert_eq!(server.bodies["new"], b"new-data");
    assert!(!server.bodies.contains_key("delete"));
    drop(job);
    assert!(UploadTransaction::open(&path).unwrap().is_settled());
}

#[tokio::test]
async fn closure_ack_never_settles_a_file_request_with_a_lost_response() {
    for fault in [
        "Cloud.BeginAppUploadBatch#1",
        "Cloud.ClientBeginFileUpload#1",
        "Cloud.ClientCommitFileUpload#1",
        "Cloud.ClientDeleteFile#1",
    ] {
        let (_root, mut job, mut server) = fixture();
        server.fault = Some(fault);
        assert!(execute(&mut server, &mut job, &|| true).await.is_err());
        assert!(!server.calls.contains(&"abort-file".into()), "{fault}");
        let path = job.directory().to_owned();
        drop(job);
        let mut job = UploadTransaction::open(&path).unwrap();
        assert!(!job.is_settled(), "{fault}");
        server.calls.clear();
        assert!(
            matches!(
                settle(&mut server, &mut job, &|| true).await,
                Err(CloudError::Uncertain)
            ),
            "{fault}"
        );
        assert!(server.calls.is_empty(), "{fault}");
    }
}

#[tokio::test]
async fn stopped_http_body_before_commit_can_close_but_revocation_or_lost_ack_cannot_settle() {
    let (_root, mut job, mut server) = fixture();
    server.pause_block = true;
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(20),
            execute(&mut server, &mut job, &|| true),
        )
        .await
        .is_err()
    );
    let path = job.directory().to_owned();
    drop(job);
    let mut job = UploadTransaction::open(&path).unwrap();
    server.calls.clear();
    assert!(matches!(
        settle(&mut server, &mut job, &|| false).await,
        Err(CloudError::Revoked)
    ));
    assert!(server.calls.is_empty());
    server.fault = Some("Cloud.CompleteAppUploadBatchBlocking#1");
    assert!(settle(&mut server, &mut job, &|| true).await.is_err());
    assert!(!job.is_settled());
    server.calls.clear();
    settle(&mut server, &mut job, &|| true).await.unwrap();
    assert!(job.is_settled());
    assert_eq!(
        server.calls,
        [
            "Cloud.CompleteAppUploadBatchBlocking#1",
            "complete-result-2"
        ]
    );
    assert!(!server.bodies.contains_key("new"));
    assert!(server.bodies.contains_key("delete"));
}

#[tokio::test]
async fn stale_remote_view_or_revocation_prevents_mutation_and_readback_cannot_hide_divergence() {
    let (_root, mut job, mut server) = fixture();
    assert!(matches!(
        execute(&mut server, &mut job, &|| false).await,
        Err(CloudError::Revoked)
    ));
    assert!(server.calls.is_empty());
    server.remote.change_number += 1;
    assert!(matches!(
        execute(&mut server, &mut job, &|| true).await,
        Err(CloudError::Changed)
    ));
    assert_eq!(server.calls, ["listing"]);
    assert_eq!(job.phase(), &UploadPhase::Prepared);
    for corrupt in [false, true] {
        let (_root, mut job, mut server) = fixture();
        server.corrupt_download = corrupt;
        server.readback_changed = !corrupt;
        assert!(execute(&mut server, &mut job, &|| true).await.is_err());
        assert_eq!(job.phase(), &UploadPhase::Acknowledged);
    }
}

#[tokio::test]
async fn authorization_is_checked_between_requests_and_bad_archives_never_reach_network() {
    use std::cell::Cell;
    let (_root, mut job, mut server) = fixture();
    let calls = Cell::new(0);
    let guard = || {
        calls.set(calls.get() + 1);
        calls.get() < 5
    };
    assert!(matches!(
        execute(&mut server, &mut job, &guard).await,
        Err(CloudError::Revoked)
    ));
    assert!(
        !server
            .calls
            .contains(&"Cloud.ClientCommitFileUpload#1".into())
    );
    let (_root, mut job, mut server) = fixture();
    fs::write(job.directory().join("file-0.bin"), b"corrupt").unwrap();
    assert!(matches!(
        execute(&mut server, &mut job, &|| true).await,
        Err(CloudError::Integrity)
    ));
    assert_eq!(server.calls, ["listing"]);
}

#[test]
fn upload_plans_honor_explicit_bodies_offsets_methods_and_authorities() {
    let mut block = wire::UploadBlock {
        https: true,
        host: "steamcloud-test.storage.googleapis.com".into(),
        path: "/data?sig=secret".into(),
        method: 4,
        offset: 2,
        length: 3,
        ..Default::default()
    };
    assert_eq!(
        block_request(&block, b"abcdef")
            .unwrap()
            .body()
            .unwrap()
            .as_bytes()
            .unwrap(),
        b"cde"
    );
    block.offset = u64::MAX;
    assert!(block_request(&block, b"abcdef").is_err());
    block.body = Some(b"completion document".to_vec());
    block.method = 3;
    assert_eq!(
        block_request(&block, b"abcdef")
            .unwrap()
            .body()
            .unwrap()
            .as_bytes()
            .unwrap(),
        b"completion document"
    );
    block.body = Some(vec![]);
    block.method = 1;
    assert!(block_request(&block, b"abcdef").is_ok());
    block.body = None;
    assert!(block_request(&block, b"abcdef").is_err());
    block.offset = 0;
    block.method = 4;
    block.headers = vec![wire::Header {
        name: "Host".into(),
        value: "evil.invalid".into(),
    }];
    assert!(block_request(&block, b"abcdef").is_err());
    block.headers.clear();
    block.https = false;
    assert!(block_request(&block, b"abcdef").is_err());
    block.https = true;
    block.host = "localhost".into();
    assert!(block_request(&block, b"abcdef").is_err());
}

#[test]
fn journal_lock_and_immutable_content_survive_reopen_and_reject_links() {
    let (_root, job, _) = fixture();
    let directory = job.directory().to_owned();
    assert!(UploadTransaction::open(&directory).is_err());
    drop(job);
    let reopened = UploadTransaction::open(&directory).unwrap();
    drop(reopened);
    fs::hard_link(directory.join("file-0.bin"), directory.join("alias.bin")).unwrap();
    assert!(UploadTransaction::open(&directory).is_err());
}
