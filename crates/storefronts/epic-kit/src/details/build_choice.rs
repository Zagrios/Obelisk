//! Epic's answers to the build choice: the game's live builds, and a Mac
//! build's manifest, read to find its launch executable.
use obelisk_core::{
    Platform,
    build_choice::{GameBuilds, Inspection, Launch, Offered, Verdict},
    mac_build::Listing,
    transfer::CancelToken,
};

use super::{
    Asset, BuildKey, FetchedManifest, asset_manifest, build_key, live_builds, manifest_cache,
    report, trace,
};
use crate::{EpicError, auth::Api, client::PLATFORM, content::ContentClient};

/// A game's live builds, as the launcher service names them for the account.
pub(crate) struct LiveBuilds<'a> {
    pub(crate) api: &'a Api,
    /// How a Mac build's files are read from Epic's CDN.
    pub(crate) content: &'a ContentClient,
    pub(crate) access_token: &'a str,
    pub(crate) namespace: &'a str,
    pub(crate) catalog_item_id: &'a str,
    pub(crate) app_name: &'a str,
}

/// Why reading a Mac build stopped.
pub(crate) enum Unread {
    /// Its manifest is not one Obelisk installs, as checking it told.
    Manifest(EpicError),
    /// Epic did not give the manifest, or a file's first bytes.
    Other(EpicError),
}

impl Unread {
    fn error(&self) -> &EpicError {
        match self {
            Self::Manifest(error) | Self::Other(error) => error,
        }
    }
}

impl From<EpicError> for Unread {
    fn from(error: EpicError) -> Self {
        Self::Other(error)
    }
}

impl From<Unread> for EpicError {
    fn from(unread: Unread) -> Self {
        match unread {
            Unread::Manifest(error) | Unread::Other(error) => error,
        }
    }
}

impl GameBuilds for LiveBuilds<'_> {
    type Build = Asset;
    type Key = BuildKey;
    type Kept = FetchedManifest;
    type Error = EpicError;
    type Failure = Unread;

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

    /// The build's launch executable, read from the files its manifest
    /// lists. The manifest is kept for the download of a build that is
    /// picked.
    async fn inspect(
        &self,
        mac: &Asset,
        cancel: &CancelToken,
    ) -> Result<Inspection<FetchedManifest>, Unread> {
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
            let (validated, bytes) =
                tokio::task::spawn_blocking(move || (manifest_cache::validate(&bytes), bytes))
                    .await
                    .map_err(|_| EpicError::Protocol)?;
            let manifest = validated.map_err(Unread::Manifest)?;
            let mut listing = Listing::default();
            for (index, file) in manifest.files.iter().enumerate() {
                listing.file(&file.filename, index, file.size());
            }
            let read = |index: usize, bytes: u64| {
                let file = &manifest.files[index];
                self.content
                    .read(&manifest, &base_urls, file, bytes, cancel)
            };
            let launch = Launch {
                executable: &manifest.launch_executable,
                libraries: &[],
            };
            let verdict = Verdict::of(&listing, launch, read).await?;
            Ok(verdict.keeping(|| FetchedManifest {
                bytes,
                base_urls,
                deployment_id,
                network_bytes,
                manifest: Some(manifest),
                cache_key: None,
            }))
        }
        .await;
        // A refusal is an answer, not a fetch that failed.
        if let Err(failure) = &inspection
            && !Self::is_refusal(failure)
        {
            report(&mut trace, failure.error());
        }
        inspection
    }

    /// Only a manifest in a form Obelisk does not install. An unsafe or
    /// duplicate path in it, or a chunk Epic's servers do not hold or that
    /// Obelisk cannot decode, fails the pick.
    fn is_refusal(failure: &Unread) -> bool {
        matches!(failure, Unread::Manifest(EpicError::UnsupportedContent))
    }
}

#[cfg(test)]
mod tests {
    use obelisk_core::build_choice::Unavailable;

    use super::*;

    #[test]
    fn only_a_manifest_in_a_form_obelisk_does_not_install_is_a_refusal() {
        let refused = LiveBuilds::is_refusal;
        assert!(refused(&Unread::Manifest(EpicError::UnsupportedContent)));
        // An unsafe or duplicate path the check of the manifest finds.
        for error in [EpicError::UnsafePath("../game".into()), EpicError::Protocol] {
            let named = format!("{error:?}");
            assert!(!refused(&Unread::Manifest(error)), "{named}");
        }
        // A chunk of the program Obelisk cannot decode or Epic's servers do
        // not hold, and what else stops a fetch.
        for error in [
            EpicError::UnsupportedContent,
            EpicError::Http(404),
            EpicError::Http(503),
            EpicError::Network,
            EpicError::Protocol,
            EpicError::Integrity,
        ] {
            let named = format!("{error:?}");
            assert!(!refused(&Unread::from(error)), "{named}");
        }
    }

    #[test]
    fn a_game_without_an_installable_build_says_why() {
        assert!(matches!(
            EpicError::from(Unavailable::NoBuild),
            EpicError::Protocol
        ));
        assert!(matches!(
            EpicError::from(Unavailable::NotNative),
            EpicError::Service { message } if message == "backend.install.macBuildNotNative"
        ));
    }
}
