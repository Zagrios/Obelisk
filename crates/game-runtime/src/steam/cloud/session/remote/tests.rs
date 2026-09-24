//! Verified remote reads: cached bytes are reused only while Steam still lists
//! them unchanged.
use super::*;
use crate::cloud::sync::Storefront;
use crate::cloud::sync::tests::Harness;
use crate::steam::cloud::session::fake::{FakeSteamCloud, save_rule};
use crate::steam::cloud::session::tests::{ACCOUNT, SteamGame, credentials};
use steam_kit::cloud::CloudPreferencesCache;

const FIRST: &[u8] = b"cloud v1";
const SECOND: &[u8] = b"cloud v2";
const NAME: &str = "%GameInstall%slot.sav";

struct Source {
    listing: CloudListing,
    bytes: Vec<u8>,
    downloads: usize,
    listings: usize,
}
impl Source {
    fn new() -> Self {
        Self {
            listing: CloudListing {
                steam_id: ACCOUNT,
                app_id: 42,
                change_number: 1,
                files: vec![CloudFile {
                    name: NAME.into(),
                    sha1: "15db5ca03437f87f7c50e14f09d72ea5e04f39a0".into(),
                    size: FIRST.len() as u32,
                    timestamp: 1,
                    persist_state: 0,
                    platforms: 7,
                    reupload_requested: false,
                }],
            },
            bytes: FIRST.into(),
            downloads: 0,
            listings: 0,
        }
    }
}
impl RemoteSource for Source {
    async fn download_files(
        &mut self,
        files: &[CloudFile],
        mut receive: impl FnMut(&CloudFile, Vec<u8>) -> Result<(), CloudError>,
    ) -> Result<(), Failure> {
        for file in files {
            self.downloads += 1;
            file.verify(&self.bytes)?;
            receive(file, self.bytes.clone())?;
        }
        Ok(())
    }
    async fn listing(&mut self) -> Result<CloudListing, Failure> {
        self.listings += 1;
        Ok(self.listing.clone())
    }
}

/// A game whose AutoCloud rules bind `slot.sav` in the installation.
fn fixture() -> (SteamGame, Adapter<FakeSteamCloud>, Vault, Control) {
    let game = SteamGame::game();
    let mut context = super::Context::new(&game.config, ACCOUNT, save_rule());
    context.listing = Source::new().listing;
    context.save().unwrap();
    let (adapter, vault) = open(&game);
    (game, adapter, vault, Control::new(|_| {}))
}

fn open(game: &SteamGame) -> (Adapter<FakeSteamCloud>, Vault) {
    let vault = Vault::open(&game.config.root, scope(&game.config, ACCOUNT)).unwrap();
    let mut adapter = Adapter {
        config: game.config.clone(),
        credentials: credentials(),
        remote: game.cloud.handle(),
        preferences: CloudPreferencesCache::default(),
        downloaded: Manifest::default(),
        context: None,
    };
    adapter.load().unwrap();
    (adapter, vault)
}

#[tokio::test]
async fn unchanged_files_reuse_verified_bytes_across_retry_and_restart() {
    let (game, mut adapter, vault, control) = fixture();
    let mut source = Source::new();
    let listing = source.listing.clone();

    let remote = adapter
        .read_remote(&mut source, &vault, &control, &listing)
        .await
        .unwrap();
    adapter
        .read_remote(&mut source, &vault, &control, &listing)
        .await
        .unwrap();

    assert_eq!(source.downloads, 1);
    assert_eq!(source.listings, 2);
    adapter
        .set_listing(listing.clone(), remote, &vault)
        .unwrap();
    drop((adapter, vault));
    let (mut adapter, vault) = open(&game);
    adapter
        .read_remote(&mut source, &vault, &control, &listing)
        .await
        .unwrap();
    assert_eq!(source.downloads, 1);
    assert_eq!(source.listings, 3);
}

#[tokio::test]
async fn changed_content_missing_blob_and_remote_races_never_use_stale_cache() {
    let (game, mut adapter, vault, control) = fixture();
    let mut source = Source::new();
    let first = source.listing.clone();
    let remote = adapter
        .read_remote(&mut source, &vault, &control, &first)
        .await
        .unwrap();
    let blob = Harness::scope(&game)
        .directory(&game.config.root)
        .unwrap()
        .join("blobs")
        .join(remote.files.0[NAME].hash.as_str());
    std::fs::remove_file(&blob).unwrap();
    adapter
        .read_remote(&mut source, &vault, &control, &first)
        .await
        .unwrap();
    assert_eq!(source.downloads, 2);

    source.listing.change_number = 2;
    source.listing.files[0].sha1 = "2df584286a54a4b83b7fc9161069b37d61cf8e07".into();
    source.bytes = SECOND.into();
    let second = source.listing.clone();
    let remote = adapter
        .read_remote(&mut source, &vault, &control, &second)
        .await
        .unwrap();
    assert_eq!(source.downloads, 3);
    assert_eq!(vault.read_content(&remote.files.0[NAME]).unwrap(), SECOND);

    source.listing.change_number = 3;
    assert!(matches!(
        adapter
            .read_remote(&mut source, &vault, &control, &second)
            .await,
        Err(Failure::Changed)
    ));
    assert_eq!(source.downloads, 3);
    let mut foreign = second.clone();
    foreign.steam_id += 1;
    assert!(matches!(
        adapter
            .read_remote(&mut source, &vault, &control, &foreign)
            .await,
        Err(Failure::Authentication)
    ));
    source.listing.files.clear();
    let deleted = source.listing.clone();
    let remote = adapter
        .read_remote(&mut source, &vault, &control, &deleted)
        .await
        .unwrap();
    assert!(remote.files.0.is_empty());
    assert!(adapter.downloaded.0.is_empty());
    control.cancel();
    assert!(matches!(
        adapter
            .read_remote(&mut source, &vault, &control, &second)
            .await,
        Err(Failure::Cancelled)
    ));
}
