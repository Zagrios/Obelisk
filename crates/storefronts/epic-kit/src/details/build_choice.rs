//! Epic's answers to the build choice: the game's live builds, and a Mac
//! build's manifest, read to find its launch executable.
use obelisk_core::{
    Platform,
    build_choice::{GameBuilds, Inspection, Offered},
    mac_build,
    transfer::CancelToken,
};

use super::{
    Asset, BuildKey, FetchedManifest, asset_manifest, build_key, live_builds, manifest_cache,
    reported, trace,
};
use crate::{EpicError, auth::Api, client::PLATFORM, content::ContentClient};

/// A game's live builds, as the launcher service names them for the account.
pub(crate) struct LiveBuilds<'a> {
    api: &'a Api,
    access_token: &'a str,
    namespace: &'a str,
    catalog_item_id: &'a str,
    app_name: &'a str,
}

impl<'a> LiveBuilds<'a> {
    pub(crate) fn new(
        api: &'a Api,
        access_token: &'a str,
        namespace: &'a str,
        catalog_item_id: &'a str,
        app_name: &'a str,
    ) -> Self {
        Self {
            api,
            access_token,
            namespace,
            catalog_item_id,
            app_name,
        }
    }
}

impl GameBuilds for LiveBuilds<'_> {
    type Build = Asset;
    type Key = BuildKey;
    type Kept = FetchedManifest;
    type Error = EpicError;

    async fn builds(&self) -> Result<Offered<Asset>, EpicError> {
        let (mac, windows) = live_builds(
            self.api,
            self.access_token,
            self.namespace,
            self.catalog_item_id,
            self.app_name,
        )
        .await?;
        Ok(Offered { mac, windows })
    }

    async fn key(&self, mac: &Asset) -> Result<Option<BuildKey>, EpicError> {
        let label = PLATFORM.name(Platform::Mac);
        Ok(build_key(
            [self.namespace, self.catalog_item_id, self.app_name, label],
            mac,
        ))
    }

    /// Whether the build's launch executable holds an arm64 slice: its
    /// manifest, then the first bytes of its program. A build Obelisk cannot
    /// install does not.
    async fn inspect(
        &self,
        mac: &Asset,
        cancel: &CancelToken,
    ) -> Result<Inspection<FetchedManifest>, EpicError> {
        let label = PLATFORM.name(Platform::Mac);
        let (namespace, catalog_item_id, app_name) =
            (self.namespace, self.catalog_item_id, self.app_name);
        let mut trace = trace(namespace, catalog_item_id, app_name, label);
        let inspection = async {
            let FetchedManifest {
                bytes,
                base_urls,
                deployment_id,
                network_bytes,
                ..
            } = asset_manifest(
                self.api,
                mac,
                namespace,
                catalog_item_id,
                app_name,
                label,
                None,
                &mut trace,
            )
            .await?;
            let validated =
                tokio::task::spawn_blocking(move || (manifest_cache::validate(&bytes), bytes))
                    .await
                    .map_err(|_| EpicError::Protocol)?;
            let (manifest, bytes) = match validated {
                (Ok(manifest), bytes) => (manifest, bytes),
                (Err(EpicError::UnsupportedContent), _) => return Ok(Inspection::NotNative),
                (Err(error), _) => return Err(error),
            };
            let mut listing = mac_build::Listing::default();
            for (index, file) in manifest.files.iter().enumerate() {
                listing.file(&file.filename, index, file.size());
            }
            let content = ContentClient::new(self.api.http.clone());
            let read = |index: usize, bytes: u64| {
                content.read(&manifest, &base_urls, &manifest.files[index], bytes, cancel)
            };
            let native =
                mac_build::runs_natively(&listing, &manifest.launch_executable, &[], read).await?;
            Ok(match native {
                true => Inspection::Native(FetchedManifest {
                    bytes,
                    base_urls,
                    deployment_id,
                    network_bytes,
                    manifest: Some(manifest),
                    cache_key: None,
                }),
                false => Inspection::NotNative,
            })
        }
        .await;
        reported(&mut trace, inspection)
    }
}
