//! Opt-in, disposable-file diagnostic. Never used by the production uploader.
use super::*;
use crate::cloud::upload::{self, Transport};
use futures_util::StreamExt;
use std::{io::Write, os::unix::fs::OpenOptionsExt, path::Path};
use tokio::sync::oneshot;

impl SteamConnection {
    pub(crate) async fn cloud_settle_upload_probe(
        &self,
        http: &reqwest::Client,
        transaction: &mut crate::cloud::UploadTransaction,
        trace: &Path,
        authorized: impl Fn() -> bool,
    ) -> Result<(), CloudError> {
        self.ensure_logged_on()?;
        if transaction.steam_id() != self.steam_id {
            return Err(CloudError::Revoked);
        }
        tokio::time::timeout(
            Duration::from_secs(120),
            upload::settle(
                &mut InterruptedTransport {
                    inner: UploadTransport {
                        connection: self,
                        http,
                        authorized: &authorized,
                    },
                    started: None,
                    interrupt: false,
                    trace,
                },
                transaction,
                &authorized,
            ),
        )
        .await
        .map_err(|_| CloudError::Timeout)?
    }
    /// Drop the real upload executor while its HTTP body is still incomplete.
    /// The returned count means bytes consumed by the HTTP client, not bytes
    /// acknowledged by the storage provider. No file commit is sent.
    pub(crate) async fn cloud_interrupt_upload_probe(
        &self,
        http: &reqwest::Client,
        transaction: &mut crate::cloud::UploadTransaction,
        trace: &Path,
        authorized: impl Fn() -> bool,
    ) -> Result<usize, CloudError> {
        self.ensure_logged_on()?;
        if transaction.steam_id() != self.steam_id {
            return Err(CloudError::Revoked);
        }
        let (started, observed) = oneshot::channel();
        let mut transport = InterruptedTransport {
            inner: UploadTransport {
                connection: self,
                http,
                authorized: &authorized,
            },
            started: Some(started),
            interrupt: true,
            trace,
        };
        tokio::time::timeout(Duration::from_secs(120), async {
            tokio::select! {
                result = upload::execute(&mut transport, transaction, &authorized) => {
                    result?;
                    Err(CloudError::Invalid("probe did not interrupt an upload"))
                }
                prefix = observed => {
                    prefix.map_err(|_| CloudError::Network)
                }
            }
        })
        .await
        .map_err(|_| CloudError::Timeout)?
    }

    pub(crate) async fn cloud_upload_probe(
        &self,
        http: &reqwest::Client,
        transaction: &mut crate::cloud::UploadTransaction,
        trace: &Path,
        authorized: impl Fn() -> bool,
    ) -> Result<CloudListing, CloudError> {
        self.ensure_logged_on()?;
        if transaction.steam_id() != self.steam_id {
            return Err(CloudError::Revoked);
        }
        tokio::time::timeout(
            Duration::from_secs(120),
            upload::execute(
                &mut InterruptedTransport {
                    inner: UploadTransport {
                        connection: self,
                        http,
                        authorized: &authorized,
                    },
                    started: None,
                    interrupt: false,
                    trace,
                },
                transaction,
                &authorized,
            ),
        )
        .await
        .map_err(|_| CloudError::Timeout)?
    }
}

struct InterruptedTransport<'a, F> {
    inner: UploadTransport<'a, F>,
    started: Option<oneshot::Sender<usize>>,
    interrupt: bool,
    trace: &'a Path,
}

fn trace(path: &Path, event: serde_json::Value) -> Result<(), CloudError> {
    let mut output = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(path)
        .map_err(|_| CloudError::Storage)?;
    serde_json::to_writer(&mut output, &event).map_err(|_| CloudError::Storage)?;
    output
        .write_all(b"\n")
        .and_then(|_| output.sync_all())
        .map_err(|_| CloudError::Storage)
}

fn result_code<T>(result: &Result<T, CloudError>) -> Option<i32> {
    match result {
        Ok(_) => Some(1),
        Err(CloudError::Cm(CmError::Rejected { result, .. })) => Some(*result),
        _ => None,
    }
}

impl<F: Fn() -> bool> Transport for InterruptedTransport<'_, F> {
    async fn commit(
        &mut self,
        request: &wire::CommitUploadRequest,
    ) -> Result<wire::CommitUploadResponse, CloudError> {
        if self.interrupt {
            return Err(CloudError::Invalid("probe reached file commit"));
        }
        trace(
            self.trace,
            serde_json::json!({"event":"file_commit_request", "transfer_succeeded":request.succeeded}),
        )?;
        let result = self.inner.commit(request).await;
        trace(
            self.trace,
            serde_json::json!({"event":"file_commit_receipt", "ok":result.is_ok(),
            "committed":result.as_ref().ok().and_then(|r| r.committed)}),
        )?;
        result
    }

    async fn call<R: Message + Default>(
        &mut self,
        name: &str,
        request: &impl Message,
    ) -> Result<R, CloudError> {
        trace(
            self.trace,
            serde_json::json!({"event":"cm_request", "method":name}),
        )?;
        let result = self.inner.call(name, request).await;
        trace(
            self.trace,
            serde_json::json!({"event":"cm_response", "method":name,
            "result":result_code(&result), "ok":result.is_ok()}),
        )?;
        result
    }

    async fn block(&mut self, block: &wire::UploadBlock, bytes: &[u8]) -> Result<(), CloudError> {
        trace(
            self.trace,
            serde_json::json!({"event":"http_start", "interrupt":self.interrupt,
            "length":block.length, "explicit_body":block.body.is_some()}),
        )?;
        if !self.interrupt {
            let result = self.inner.block(block, bytes).await;
            trace(
                self.trace,
                serde_json::json!({"event":"http_finish", "ok":result.is_ok()}),
            )?;
            return result;
        }
        const PREFIX: usize = 64 * 1024;
        let mut request = upload::block_request(block, bytes)?;
        let body = request
            .body()
            .and_then(reqwest::Body::as_bytes)
            .ok_or(CloudError::Invalid("probe body"))?;
        // Pass through multipart control requests, interrupt only a data block.
        if block.body.is_some() || body.len() <= PREFIX {
            return self.inner.block(block, bytes).await;
        }
        let length = body.len();
        let prefix = Bytes::copy_from_slice(&body[..PREFIX]);
        let started = self
            .started
            .take()
            .ok_or(CloudError::Invalid("probe already armed"))?;
        let stream = futures_util::stream::once(async { Ok::<_, std::io::Error>(prefix) }).chain(
            futures_util::stream::once(async move {
                // A second poll proves that the first chunk was consumed.
                let _ = started.send(PREFIX);
                std::future::pending::<Result<Bytes, std::io::Error>>().await
            }),
        );
        request
            .headers_mut()
            .insert(reqwest::header::CONTENT_LENGTH, length.into());
        *request.body_mut() = Some(reqwest::Body::wrap_stream(stream));
        if !(self.inner.authorized)() {
            return Err(CloudError::Revoked);
        }
        let response = self
            .inner
            .http
            .execute(request)
            .await
            .map_err(|_| CloudError::Network)?;
        // An early HTTP response is not a successful interrupted-body proof.
        drop(response);
        Err(CloudError::Network)
    }

    async fn listing(&mut self, app: u32) -> Result<CloudListing, CloudError> {
        self.inner.listing(app).await
    }

    async fn download(&mut self, app: u32, file: &CloudFile) -> Result<Vec<u8>, CloudError> {
        self.inner.download(app, file).await
    }
}
