# Settlement of interrupted Steam Cloud uploads

Research date: September 19, 2026. The initial source review below was followed by the authenticated interrupted-upload experiment recorded at the end of this document.

Forge preserves batch-close acknowledgements and records each staged file. Its current retry policy is described below. Public sources do **not** establish a general terminal-status protocol for a lost batch ID or an old file mutation with no reply. A fresh listing, an empty pending-operation list, and elapsed time do not prove an old operation was cancelled.

## Current recovery policy: compare again after failure

The user subsequently chose GameNative-style retry after reviewing [its public implementation](steam-cloud-gamenative-recovery.md). This supersedes the permanent-block policy described in the original investigation below. It does not change the provider guarantees established by that investigation.

Forge retains the old upload journal without inventing an acknowledgement. It still tries the verified cleanup of an interrupted HTTP body. If the receipt is unknown or a transient error prevents closing the old batch, the coordinator reads and verifies a fresh complete Cloud view. Only after that read succeeds does it retire the local pending attempt and compare again against the last confirmed baseline and any explicit local choice. Equal local/Cloud content converges; local-only changes admit a fresh upload; divergent changes require the conflict sheet. The former transaction is never replayed or marked successful by this policy.

A failed read leaves pending work intact. Revocation, account mismatch, disabled preferences, corrupt storage and unsupported metadata remain hard failures. An acknowledged old transaction keeps the existing verified reconciliation path. Old schema-1/2 intermediate transactions use the new comparison path after journal validation, without rewriting their unknown outcome.

Launch preparation and post-exit synchronization now make at most three attempts, waiting one then two seconds after transient failures, within the existing 120-second total budget. A pending earlier exit is synchronized before the next game launch, using the current local files. Exhaustion presents the normal recoverable sync failure or retains pending work after exit. A later launch starts a new budget. The conflict-only modal and progress inside the launch button remain in place.

This is a client retry policy, not a proof that an old server request cannot complete later. Server-side ordering and terminal-operation guarantees remain unknown. New uploads still require valid Steam replies and full verified readback. The missing-receipt, reopen, fresh-plan, conflict, identical-content, deletion, authorization and retry-budget checks are automated fixtures; no real Steam response was intentionally dropped for this change.

## Original provider-contract investigation

## What the sources establish

Valve describes a batch as announced upload/delete intent. Its completion call releases admission for subsequent batches, including when some operations failed. The request carries the original batch ID and reports success or failure with result 1 or 2. Omitting completion can block new uploads for several minutes. This duration is not a documented deadline for every earlier request to stop. The file upload separately reports `file_committed`; false means that upload did not replace the visible file. Valve documents no batch rollback, replay guarantee, or read-only batch-status endpoint here. [Valve ICloudService](https://partner.steamgames.com/doc/webapi/ICloudService#CompleteAppUploadBatch).

The pinned CM schema has both `CompleteAppUploadBatch`, a notification with no response, and `CompleteAppUploadBatchBlocking`, a request with an empty response body. Its request identifies `appid` and `batch_id`. The file commit request instead identifies `appid`, filename, SHA, and transfer success; it carries **no batch ID or operation nonce**. A successful CM envelope must therefore be retained separately from file-content verification. The schema describes message shape, not server ordering or idempotence. [CM Cloud definitions, lines 192–267 and 409–413](https://github.com/SteamDatabase/Protobufs/blob/d35adb23d7d48ad9673261f9572b60d1bfe06c12/steam/steammessages_cloud.steamclient.proto#L192).

JavaSteam waits for the blocking batch response. Its wrapper returns `fileCommitted` for file commit. It exposes no recovery query or duplicate-close semantics in these methods. That is evidence of a client implementation, not of Valve's internal execution ordering. [JavaSteam methods, lines 281–322](https://github.com/joshuatam/JavaSteam/blob/433f2ad15c36d5e690a4fe77401ec3f6b960641e/src/main/java/in/dragonbra/javasteam/steam/handlers/steamcloud/SteamCloud.kt#L281).

`SignalAppLaunchIntent` sends launch intent and returns pending operations. Those operations contain type, machine name, client ID, update time, OS and device type. They contain no batch ID, file identity, terminal receipt or completion revision. Thus an empty response cannot identify and settle a particular old batch. The request also contains `ignore_pending_operations`; this is unsuitable as a read-only recovery probe. [Launch-intent messages](https://github.com/SteamDatabase/Protobufs/blob/d35adb23d7d48ad9673261f9572b60d1bfe06c12/steam/steammessages_cloud.steamclient.proto#L354), [pending-operation fields](https://github.com/SteamDatabase/Protobufs/blob/d35adb23d7d48ad9673261f9572b60d1bfe06c12/steam/steammessages_client_objects.proto#L212).

The inspected pinned `steam/` and `webui/` protobuf files have no `GetAppSessionInfo`, `AbortFileUpload` or `CompleteFileUpload` definition. Those names must not be invented as CM recovery RPCs. The relevant file operation is `ClientCommitFileUpload(transfer_succeeded=...)`; the Web contract likewise requires its commit call after transfer failure. Neither source establishes that replaying a negative commit cancels a previously sent positive commit. [Full pinned protocol tree](https://github.com/SteamDatabase/Protobufs/tree/d35adb23d7d48ad9673261f9572b60d1bfe06c12), [Valve file commit contract](https://partner.steamgames.com/doc/webapi/ICloudService#CommitHTTPUpload).

## Consequences for Forge's phases

This table describes the original schema-1 journal, inspected before the settlement changes. Its limits explain why schema-2 records unanswered mutations separately. The recovery rules are conservative deductions from the sources above and Forge's serialized requests, not claims about undocumented server behavior.

| Durable phase | What the current journal proves | Safe next action |
| --- | --- | --- |
| `Beginning` | Begin may have arrived; the batch ID is absent. Forge persists `Open` before it sends file operations. | Retain the unresolved journal and block a replacement upload. The reviewed APIs cannot find or close this specific batch. Neither elapsed time nor new listing proves its terminal state. |
| `Open { batch_id }` | A batch ID is known. The current phase does not say which file operations were sent or acknowledged. | A live execution with all earlier mutations answered may close the batch with result 2 and retain the successful response. After a crash, do not infer this condition from `Open`. |
| `Completing { batch_id }` | Forge finished its file loop and persisted intent to close, but did not durably retain the completion response. | Keep the result ambiguous. A duplicate close might work, but public sources do not specify its idempotence or the meaning of errors for an already closed batch. |
| `Interrupted { batch_id: Some(...) }` | A preceding operation failed. Best-effort close may or may not have run or succeeded. | Preserve any observed positive close response. Separately retain whether a file mutation still lacks an answer. Schema-1 journals do not encode enough to infer safe settlement after reopening. |
| `Interrupted { batch_id: None }` | No usable batch identity. | Same recovery limit as `Beginning`. |
| `Acknowledged` / `Verified` | The successful normal completion response was persisted after all file commits and deletes returned successfully. | Perform or repeat read-only verification under the existing account and ownership guards. Only matching complete metadata, verified bytes and a stable second listing establish the desired observed content. |

## Minimal safe implementation

1. Persist the outcome of best-effort batch closure. A positive result is useful evidence and should survive restart. Keep a distinct failed-batch closure receipt; do not turn it into normal upload success or imply rollback or terminal status for an unanswered file mutation.
2. Record operation uncertainty separately. Immediately before batch admission or a file begin/commit/delete call, persist that it may be in flight; clear that fact only when its correlated reply has been classified and persisted. A request that timed out remains uncertain even when another request on the same connection later succeeds. Batch-closure uncertainty remains represented by the absence of its durable positive receipt.
3. Allow a failed batch to proceed to reconciliation only when its positive close response is durable and no earlier metadata mutation is unresolved. For example, all previous file commits returned, then an HTTP body transfer failed before any commit for that file was sent. The remaining staged body is distinct from a sent Cloud metadata commit.
4. Reconcile a settled failed batch with a complete verified remote snapshot and the immutable local archive. Classify the observed result as unchanged, desired, partial or conflicting. Rebuild a fresh plan from the observed remote state before any new batch. Never replay the old upload/delete sequence.
5. Keep unknown-ID batches and batches with unanswered metadata mutations blocked. A future provider-specific terminal proof must identify the original operation and establish that it cannot still mutate Cloud. No such proof was found in this review.

The important distinction is receipt completeness, not connection identity. Keeping the same CM connection does not by itself prove that a timed-out request finished. Forge's current `wait_for` explicitly drops late responses belonging to older job IDs. A new connection provides still less local information about those requests. [Forge CM transport](../crates/storefronts/steam-kit/src/cm.rs), [Forge upload executor](../crates/storefronts/steam-kit/src/cloud/upload.rs).

## Review of the schema-2 implementation

The implementation now records `unanswered_mutation` before batch admission and file begin/commit/delete requests, then clears it after usable correlated responses. A missing commit boolean stays ambiguous. `Closed` records the positive failure-close response without clearing any earlier file uncertainty. Schema-1 journals missing the new field default to uncertainty; their original `Prepared`, `Acknowledged` and `Verified` phases retain the guarantees already established by their control flow. [Upload executor and journal](../crates/storefronts/steam-kit/src/cloud/upload.rs).

`settle` can send another completion request for a known batch only when no earlier file mutation lacks a reply. It proceeds only on a fresh positive CM result; any rejection, missing response or timeout keeps recovery blocked. This does not assume that repeated completion will succeed or that an error such as `NoMatch` means success. Because all earlier file operations are already accounted for, it also does not require the completion call to stop an older file commit. The runtime obtains a complete verified remote view after settlement before it reconciles the coordinator journal. [CM settlement adapter](../crates/storefronts/steam-kit/src/cm/cloud.rs), [runtime recovery](../crates/game-runtime/src/steam/cloud/session.rs).

For an interrupted HTTP body with no subsequent CM commit sent, the terminal claim concerns the batch and published Cloud metadata. Valve's two-step upload contract requires a separate commit to publish the file. It does not prove that the storage provider has stopped receiving an old body, removed temporary objects, or released every resource. `is_settled` must not be described as proving those facts. The mocked interrupted-body test checks Forge's decision and request ordering, not storage-provider execution. [Valve upload contract](https://partner.steamgames.com/doc/webapi/ICloudService#BeginHTTPUpload), [settlement tests](../crates/storefronts/steam-kit/src/cloud/upload/tests.rs).

## Remaining unknowns

- Whether a successful blocking batch close prevents an earlier, still-running file commit or deletion from applying afterward. In particular, `ClientCommitFileUpload` has no batch ID.
- Whether repeated batch completion is idempotent, which result means an already closed batch, and whether failure completion can replace a previously sent success completion.
- Whether any provider API can recover a lost batch ID or return a terminal receipt for it.
- Whether server expiry cancels every outstanding operation or merely releases batch admission.
- Whether a negative file commit can safely settle an earlier positive commit with no reply, and how it distinguishes later attempts with the same filename and SHA.

The [existing authenticated upload experiment](steam-cloud-authenticated-upload.md) proves ordinary success, verified readback and disposable-file deletion. It does not answer these interruption questions. An authorized disposable-file experiment could record late replies and duplicate-close results, but one observed timing would still not establish a universal ordering guarantee.


## Authenticated HTTP interruption and schema 3

The real restart trial exposed a gap in schema 2. After stopping a 2 MiB disposable upload before file commit, Steam accepted failed-batch closure and admitted a fresh batch. `ClientBeginFileUpload` for the same filename then returned `EResult::Busy` (10). A second disposable-file trial reproduced that exact boundary. Batch closure alone had not released the staged file. No user save was modified by these trials.

Schema 3 records the staged upload's index atomically with the classified begin-file receipt. While that index is present and `unanswered_mutation` is false, Forge knows that no positive commit was sent. It reports `transfer_succeeded=false` for that exact filename and SHA before closing the batch. It never sends this negative commit after an unanswered positive commit. Lost or malformed negative replies also remain blocked after reopening. The development-only cleanup on September 20 removed schema-1/2 readers. Only current schema-3 journals can be opened; interrupted current-format transactions retain their existing recovery handling.

The authenticated negative commit returned a correlated CM envelope with result 2 and the exact two-byte protobuf body `08 00`, explicitly `file_committed=false`. Forge previously discarded this body because the generic service helper rejected every non-1 envelope. The typed commit adapter now accepts this particular negative receipt only for a request declaring failure. A positive commit with result 2, another error code, a missing envelope result, an empty/malformed body or `file_committed=true` does not receive this exception. A failed-file receipt does not make the upload successful; it only permits failed-batch closure and a complete fresh remote reconciliation.

This endpoint-specific classification follows the documented meaning of an explicit negative file result and the observed CM response. It does not establish that a negative commit can cancel an earlier unanswered positive one, nor that provider-side temporary objects have disappeared. [Valve file commit contract](https://partner.steamgames.com/doc/webapi/ICloudService#CommitHTTPUpload).

Two regressions went red before their fixes: interruption/reopen/fresh upload reproduced error 10, and parsing the real negative receipt reproduced error 2. Further tests cover HTTP failure without process restart, lost and malformed abort replies, preservation of unknown positive-commit outcomes and conservative opening of schema-2 intermediate journals.

The corrected authenticated cycle passed after a complete Forge restart. A fresh upload of the same disposable filename succeeded, all remote bytes were verified, and the fixture was deleted with another verified transaction. The original 18 local and remote files were preserved. See [the exact scope and redacted observations](evidence/steam-cloud-runtime-2026-09-19/interrupted-upload-observations.json). The original unanswered diagnostic journals were retained; the experiment did not rewrite them into successful transactions.
