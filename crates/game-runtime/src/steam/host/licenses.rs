//! What the Steam client keeps on disk between sessions: the licenses it
//! received, the package information it fetched and the information of the
//! apps they hold. Steam reads them at start and asks its servers only for
//! what changed since, so it knows the account's games before the network has
//! answered. A host's home is thrown away with its launch, so each launch
//! began cold and a large library could take over a minute to publish its
//! licenses. Obelisk keeps these files for the account, the app information as
//! much as the licenses. Ownership still comes from the ticket the servers
//! issue, never from these files.
use super::files::atomic_write;
use crate::{files::private_directory, steam::Error};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// One account's kept files, and where the client has them.
pub(crate) struct LicenseCache {
    /// Each file as kept and as the client reads and writes it.
    files: [(PathBuf, PathBuf); 3],
    /// The folders the client writes them in.
    folders: [PathBuf; 2],
}
impl LicenseCache {
    /// The kept files of `steam_id` for a client whose Steam directory is
    /// `steam`. They live in the runtime cache, whose cleanup only sweeps the
    /// stale launches and runtimes it names.
    pub fn new(cache: &Path, steam_id: u64, steam: &Path) -> Result<Self, Error> {
        let kept = cache.join(format!("account-{steam_id}"));
        private_directory(&kept)?;
        let appcache = steam.join("appcache");
        let config = steam.join(format!("userdata/{}/config", steam_id as u32));
        let file = |folder: &Path, name: &str| (kept.join(name), folder.join(name));
        Ok(Self {
            files: [
                file(&appcache, "packageinfo.vdf"),
                file(&appcache, "appinfo.vdf"),
                file(&config, "licensecache"),
            ],
            folders: [appcache, config],
        })
    }
    pub fn folders(&self) -> &[PathBuf] {
        &self.folders
    }
    /// The folder of the kept files.
    #[cfg(test)]
    pub(super) fn kept(&self) -> &Path {
        self.files[0].0.parent().unwrap()
    }
    /// Gives the client the kept files it has none of yet, and the folders to
    /// write them in. A file already there is newer than the kept one.
    pub fn seed(&self) -> Result<(), Error> {
        for folder in &self.folders {
            private_directory(folder)?;
        }
        for (kept, target) in &self.files {
            if kept.is_file() && !target.exists() {
                fs::copy(kept, target)?;
            }
        }
        Ok(())
    }
    /// Keeps the client's files, replacing the kept ones whole. What the client
    /// left unfinished fails its own checks when read back, as it would after
    /// a crash of Steam itself.
    pub fn keep(&self) {
        for (kept, source) in &self.files {
            // The client's folder is its own: follow nothing it linked.
            if fs::symlink_metadata(source).is_ok_and(|file| file.is_file() && file.len() > 0) {
                let _ = atomic_write(kept, |file| {
                    std::io::copy(&mut fs::File::open(source)?, file)?;
                    Ok(())
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cache of the account whose id is 42, and the client's Steam
    /// directory beside it.
    fn setup() -> (tempfile::TempDir, LicenseCache, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let steam = root.path().join("steam");
        let cache = LicenseCache::new(root.path(), 0x011000010000002a, &steam).unwrap();
        (root, cache, steam)
    }

    #[test]
    fn a_cold_host_gets_the_kept_files_and_the_folders_to_write_in() {
        let (_root, cache, steam) = setup();
        fs::write(cache.kept().join("licensecache"), b"licenses").unwrap();
        cache.seed().unwrap();
        assert_eq!(
            fs::read(steam.join("userdata/42/config/licensecache")).unwrap(),
            b"licenses"
        );
        // Only what was kept: the client writes the rest.
        assert!(!steam.join("appcache/packageinfo.vdf").exists());
        assert!(steam.join("appcache").is_dir());
    }

    #[test]
    fn seeding_never_replaces_a_file_the_client_already_has() {
        let (_root, cache, steam) = setup();
        fs::write(cache.kept().join("packageinfo.vdf"), b"older").unwrap();
        fs::create_dir_all(steam.join("appcache")).unwrap();
        fs::write(steam.join("appcache/packageinfo.vdf"), b"newer").unwrap();
        cache.seed().unwrap();
        assert_eq!(
            fs::read(steam.join("appcache/packageinfo.vdf")).unwrap(),
            b"newer"
        );
    }

    #[test]
    fn what_the_client_wrote_is_kept_for_the_next_host_of_the_account() {
        let (root, cache, steam) = setup();
        cache.seed().unwrap();
        let (packages, licenses) = (
            steam.join("appcache/packageinfo.vdf"),
            steam.join("userdata/42/config/licensecache"),
        );
        fs::write(&packages, b"packages").unwrap();
        fs::write(&licenses, b"licenses").unwrap();
        cache.keep();
        fs::write(&packages, b"more packages").unwrap();
        cache.keep();

        let fresh = root.path().join("fresh");
        let next = LicenseCache::new(root.path(), 0x011000010000002a, &fresh).unwrap();
        next.seed().unwrap();
        assert_eq!(
            fs::read(fresh.join("appcache/packageinfo.vdf")).unwrap(),
            b"more packages"
        );
        assert_eq!(
            fs::read(fresh.join("userdata/42/config/licensecache")).unwrap(),
            b"licenses"
        );
        // Another account of the same Mac starts from nothing.
        let stranger = LicenseCache::new(root.path(), 0x011000010000002b, &fresh).unwrap();
        assert_eq!(fs::read_dir(stranger.kept()).unwrap().count(), 0);
    }

    #[test]
    fn an_empty_or_linked_file_never_replaces_a_kept_one() {
        let (root, cache, steam) = setup();
        fs::write(cache.kept().join("licensecache"), b"licenses").unwrap();
        fs::write(cache.kept().join("packageinfo.vdf"), b"packages").unwrap();
        cache.seed().unwrap();
        let (packages, licenses) = (
            steam.join("appcache/packageinfo.vdf"),
            steam.join("userdata/42/config/licensecache"),
        );
        fs::write(&licenses, b"").unwrap();
        fs::remove_file(&packages).unwrap();
        let secret = root.path().join("secret");
        fs::write(&secret, b"not the client's").unwrap();
        std::os::unix::fs::symlink(&secret, &packages).unwrap();
        cache.keep();
        assert_eq!(
            fs::read(cache.kept().join("licensecache")).unwrap(),
            b"licenses"
        );
        assert_eq!(
            fs::read(cache.kept().join("packageinfo.vdf")).unwrap(),
            b"packages"
        );
    }
}
