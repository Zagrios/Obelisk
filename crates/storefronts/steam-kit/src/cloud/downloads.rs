use super::{CloudError, CloudFile};
use futures_util::{StreamExt, stream::FuturesUnordered};

const MAX_DOWNLOADS: usize = 4;
const BATCH_BYTES: u64 = 32 * 1024 * 1024;

pub(crate) trait DownloadPlans {
    type Plan;
    fn prepare(
        &mut self,
        file: CloudFile,
    ) -> impl Future<Output = Result<Self::Plan, CloudError>> + Send;
}

/// CM requests remain serialized on their connection. HTTP bodies overlap in
/// bounded groups and are consumed immediately. Dropping this future cancels
/// every in-flight read; no detached worker can outlive the transfer owner.
pub(crate) async fn download_files<
    S: DownloadPlans,
    D: Future<Output = Result<Vec<u8>, CloudError>>,
>(
    files: &[CloudFile],
    plans: &mut S,
    download: impl Fn(CloudFile, S::Plan) -> D,
    authorized: &impl Fn() -> bool,
    mut receive: impl FnMut(&CloudFile, Vec<u8>) -> Result<(), CloudError>,
) -> Result<(), CloudError> {
    let mut remaining = files;
    while !remaining.is_empty() {
        // Large files run alone. Smaller sets overlap without accumulating the
        // complete save set in memory. Each decoder also bounds transfer bytes.
        let mut count = 1;
        let mut bytes = u64::from(remaining[0].size);
        while count < remaining.len().min(MAX_DOWNLOADS)
            && bytes + u64::from(remaining[count].size) <= BATCH_BYTES
        {
            bytes += u64::from(remaining[count].size);
            count += 1;
        }
        let (batch, rest) = remaining.split_at(count);
        let mut pending = FuturesUnordered::new();
        for file in batch {
            check(authorized)?;
            let prepared = plans.prepare(file.clone()).await?;
            let download = download(file.clone(), prepared);
            pending.push(async move {
                check(authorized)?;
                let bytes = download.await?;
                check(authorized)?;
                file.verify(&bytes)?;
                Ok::<_, CloudError>((file, bytes))
            });
        }
        while let Some(result) = pending.next().await {
            let (file, bytes) = result?;
            check(authorized)?;
            receive(file, bytes)?;
        }
        remaining = rest;
    }
    check(authorized)
}

fn check(authorized: &impl Fn() -> bool) -> Result<(), CloudError> {
    if authorized() {
        Ok(())
    } else {
        Err(CloudError::Revoked)
    }
}

#[cfg(test)]
mod tests;
