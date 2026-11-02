//! GOG's answers to the build choice: the builds its content system lists,
//! and a Mac build's depot items, read to find the program it plays.
use obelisk_core::{
    build_choice::{GameBuilds, Inspection, Offered},
    platform::Platform,
    transfer::CancelToken,
};

use super::{
    BuildKey, add_depots, build_key, build_manifest, content_platform, owned, owned_depots,
    present_build,
};
use crate::{
    GogError,
    client::Client,
    manifest::{Build, Installed},
};

/// A product's current builds, with the depots of `language`.
pub(super) struct CurrentBuilds<'a> {
    pub(super) client: &'a Client,
    pub(super) product_id: &'a str,
    pub(super) language: &'a str,
}

impl GameBuilds for CurrentBuilds<'_> {
    type Build = Build;
    type Key = BuildKey;
    type Kept = Installed;
    type Error = GogError;

    async fn builds(&self) -> Result<Offered<Build>, GogError> {
        let api = &self.client.tokens.api;
        let (mac, windows) = tokio::try_join!(
            present_build(api, self.product_id, content_platform(Platform::Mac)),
            present_build(api, self.product_id, content_platform(Platform::Windows)),
        )?;
        Ok(Offered { mac, windows })
    }

    async fn key(&self, mac: &Build) -> Result<Option<BuildKey>, GogError> {
        Ok(build_key(self.product_id, &mac.build_id, self.language))
    }

    /// Whether the program the build's primary play task names holds an
    /// arm64 slice. It reads the build's manifests, then the first bytes of
    /// a few of its files. A build Obelisk cannot install does not.
    async fn inspect(
        &self,
        mac: &Build,
        cancel: &CancelToken,
    ) -> Result<Inspection<Installed>, GogError> {
        let (client, product_id) = (self.client, self.product_id);
        let api = &client.tokens.api;
        let manifest = build_manifest(api, product_id, mac).await?;
        let owned = client
            .with_access_token(|token| async move { owned(api, &token).await })
            .await?;
        let (_, depots) = owned_depots(&manifest, &owned, self.language)?;
        let content_root = manifest.content_root()?;
        let mut installed = Installed::default();
        let depots = depots
            .iter()
            .map(|depot| (depot.manifest.as_str(), depot.product_id.as_str()));
        add_depots(api, &mut installed, &content_root, "meta", depots).await?;
        if installed.validate().is_err() {
            return Ok(Inspection::NotNative);
        }
        Ok(
            match client
                .plays_natively(product_id, &installed, &content_root, cancel)
                .await?
            {
                true => Inspection::Native(installed),
                false => Inspection::NotNative,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use obelisk_core::{
        build_choice::{NOT_NATIVE, Unavailable},
        watch::Revisions,
    };
    use tokio::sync::Mutex;

    use super::*;
    use crate::{auth::Api, client::TokenSource};

    /// A client signed in to no account, which no key asks anything of.
    fn client() -> Client {
        Client {
            downloads: Default::default(),
            tokens: TokenSource {
                api: Arc::new(Api::new().unwrap()),
                session: Arc::new(Mutex::new(None)),
                credential_events: Revisions::new(),
            },
            avatar_url: None,
            build_choice: Default::default(),
        }
    }

    #[tokio::test]
    async fn a_build_gog_gives_no_id_keeps_no_verdict() {
        let client = client();
        let game = CurrentBuilds {
            client: &client,
            product_id: "1",
            language: "en-US",
        };
        let build = |build_id: &str| Build {
            build_id: build_id.into(),
            branch: None,
            generation: 2,
            version_name: String::new(),
            link: String::new(),
        };
        assert_eq!(game.key(&build("")).await.unwrap(), None);
        assert_eq!(
            game.key(&build("52")).await.unwrap(),
            Some(("1".into(), "52".into(), "en-US".into()))
        );
    }

    #[test]
    fn a_game_without_an_installable_build_says_why() {
        assert!(matches!(
            GogError::from(Unavailable::NoBuild),
            GogError::NoBuild
        ));
        assert!(matches!(
            GogError::from(Unavailable::NotNative),
            GogError::Service { message } if message == NOT_NATIVE
        ));
        assert_eq!(NOT_NATIVE, "backend.install.macBuildNotNative");
    }
}
