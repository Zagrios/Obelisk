//! GOG's answers to the build choice: the builds its content system lists,
//! and a Mac build's depot items, read to find the program it plays.
use obelisk_core::{
    build_choice::{GameBuilds, Inspection, Launch, Offered, Verdict},
    mac_build::Listing,
    platform::Platform,
    transfer::CancelToken,
};

use super::{
    BuildKey, add_depots, build_key, build_manifest, content_platform, owned, owned_depots,
    present_build,
};
use crate::{
    GogError,
    client::{Client, launch},
    content::{self, SecureLinks},
    manifest::{Build, Installed},
};

/// A product's current builds, with the depots of `language`.
pub(super) struct CurrentBuilds<'a> {
    pub(super) client: &'a Client,
    pub(super) product_id: &'a str,
    pub(super) language: &'a str,
}

/// Why reading a Mac build stopped.
pub(super) enum Unread {
    /// Its depots list what no installation may hold, as checking them
    /// told.
    Invalid(GogError),
    /// GOG did not give the build's manifests, or a file's first bytes.
    Other(GogError),
}

impl From<GogError> for Unread {
    fn from(error: GogError) -> Self {
        Self::Other(error)
    }
}

impl From<Unread> for GogError {
    fn from(unread: Unread) -> Self {
        match unread {
            Unread::Invalid(error) | Unread::Other(error) => error,
        }
    }
}

/// The files and links of `installed`, each file named by its place.
fn listing(installed: &Installed) -> Listing<usize> {
    let mut listing = Listing::default();
    for (index, file) in installed.files.iter().enumerate() {
        listing.file(&file.path, index, file.file.size());
    }
    for link in &installed.links {
        listing.link(&link.path, &link.target);
    }
    listing
}

impl GameBuilds for CurrentBuilds<'_> {
    type Build = Build;
    type Key = BuildKey;
    type Kept = Installed;
    type Error = GogError;
    type Failure = Unread;

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

    /// The program the build's primary play task names, read from the items
    /// its depots list. The items are kept for the download of a build that
    /// is picked.
    async fn inspect(
        &self,
        mac: &Build,
        cancel: &CancelToken,
    ) -> Result<Inspection<Installed>, Unread> {
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
        installed.validate().map_err(Unread::Invalid)?;
        let from_root = |path: &str| match content_root.as_str() {
            "" => path.to_owned(),
            root => format!("{root}/{path}"),
        };
        let listing = listing(&installed);
        let links = SecureLinks::new(client.secure_links());
        let read = |index: usize, bytes: u64| {
            content::read(&client.cdn, &links, &installed.files[index], bytes, cancel)
        };
        let mut program = None;
        for info in launch::info_files(product_id) {
            if let Some((file, size)) = listing.find(&from_root(&info))
                && size <= launch::MAX_INFO_BYTES as u64
            {
                program = launch::played_program(&read(file, size).await?);
                break;
            }
        }
        let Some(program) = program else {
            return Ok(Inspection::no_program());
        };
        let played = Launch {
            executable: &from_root(&program),
            libraries: &[],
        };
        let verdict = Verdict::of(&listing, played, read).await?;
        Ok(verdict.keeping(|| installed))
    }

    /// Whatever the check of the build's items finds: an unsafe or duplicate
    /// path, a file inside another, a malformed chunk, a link leaving the
    /// installation. The same path caught as the depots are added, or a
    /// chunk GOG's servers do not hold, fails the pick.
    fn is_refusal(failure: &Unread) -> bool {
        matches!(failure, Unread::Invalid(_))
    }
}

#[cfg(test)]
mod tests {
    use obelisk_core::build_choice::{NOT_NATIVE, Unavailable};
    use serde_json::json;

    use super::*;
    use crate::{client::tests::signed_in, manifest::Item};

    /// A client of an account no key asks anything of.
    fn client() -> Client {
        signed_in(crate::auth::Api::new().unwrap())
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
    fn a_build_s_files_and_links_are_listed_where_its_installation_holds_them() {
        let chunk = json!({"md5": "a", "size": 5000, "compressedMd5": "b", "compressedSize": 9});
        let items: Vec<Item> = serde_json::from_value(json!([
            {"type": "DepotDirectory", "path": "Contents/Resources"},
            {"type": "DepotFile", "path": "Contents\\MacOS\\Game", "chunks": [chunk, chunk]},
            {"type": "DepotLink", "path": "Contents/MacOS/Launcher", "target": "Contents/MacOS/Game"},
        ]))
        .unwrap();
        let mut installed = Installed::default();
        installed.add("Game.app", "1", items).unwrap();
        let listing = listing(&installed);
        // A file is named by its place among the installation's files.
        let program = Some((0, 10_000));
        assert_eq!(listing.find("Game.app/Contents/MacOS/Game"), program);
        assert_eq!(listing.find("Game.app/Contents/MacOS/Launcher"), program);
        assert_eq!(listing.find("Game.app/Contents/Resources"), None);
    }

    #[test]
    fn only_what_the_check_of_a_build_s_items_finds_is_a_refusal() {
        // What the check ends with, and what else stops a read.
        let errors = || {
            [
                GogError::Protocol,
                GogError::UnsafePath("../game".into()),
                GogError::Http(404),
                GogError::Http(503),
                GogError::Network,
                GogError::Integrity,
            ]
        };
        for error in errors() {
            assert!(
                CurrentBuilds::is_refusal(&Unread::Invalid(error)),
                "the check gives way, whatever it found"
            );
        }
        // The same path caught as the depots are added is no refusal, nor is
        // a chunk GOG's servers do not hold.
        for error in errors() {
            let named = format!("{error:?}");
            assert!(!CurrentBuilds::is_refusal(&Unread::from(error)), "{named}");
        }
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
