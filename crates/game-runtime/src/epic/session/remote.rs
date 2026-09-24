//! The Epic Cloud service as a sync session uses it. Production passes the
//! authenticated `epic_kit` client; tests pass an in-memory store.
use super::*;
use std::future::Future;

pub(crate) trait CloudRemote: Send + Sync + 'static {
    /// The account of the authenticated session, never a remembered one.
    fn account_id(&self) -> impl Future<Output = std::result::Result<String, EpicError>> + Send;

    fn cloud_configuration(
        &self,
        namespace: &str,
        catalog_item_id: &str,
        app_name: &str,
        platform: &str,
    ) -> impl Future<Output = std::result::Result<Option<CloudRules>, EpicError>> + Send;

    fn cloud_download(
        &self,
        game: CloudGame<'_>,
        cancel: &CancelToken,
    ) -> impl Future<Output = std::result::Result<CloudSnapshot, EpicError>> + Send;

    /// Publishes a complete set, then returns its verified readback.
    fn cloud_upload(
        &self,
        game: CloudGame<'_>,
        rules: &CloudRules,
        files: &BTreeMap<String, Vec<u8>>,
        expected_revision: &str,
        cancel: &CancelToken,
    ) -> impl Future<Output = std::result::Result<CloudSnapshot, EpicError>> + Send;
}

impl CloudRemote for EpicClient {
    async fn account_id(&self) -> std::result::Result<String, EpicError> {
        Ok(self.credentials().await?.account_id)
    }

    async fn cloud_configuration(
        &self,
        namespace: &str,
        catalog_item_id: &str,
        app_name: &str,
        platform: &str,
    ) -> std::result::Result<Option<CloudRules>, EpicError> {
        EpicClient::cloud_configuration(self, namespace, catalog_item_id, app_name, platform).await
    }

    async fn cloud_download(
        &self,
        game: CloudGame<'_>,
        cancel: &CancelToken,
    ) -> std::result::Result<CloudSnapshot, EpicError> {
        EpicClient::cloud_download(self, game, cancel).await
    }

    async fn cloud_upload(
        &self,
        game: CloudGame<'_>,
        rules: &CloudRules,
        files: &BTreeMap<String, Vec<u8>>,
        expected_revision: &str,
        cancel: &CancelToken,
    ) -> std::result::Result<CloudSnapshot, EpicError> {
        EpicClient::cloud_upload(self, game, rules, files, expected_revision, cancel).await
    }
}
