use super::*;
use steam_cloud::FileVersion;
use steam_kit::cloud::{CloudError, CloudFile};

trait RemoteSource {
    async fn download_files(
        &mut self,
        files: &[CloudFile],
        receive: impl FnMut(&CloudFile, Vec<u8>) -> Result<(), CloudError>,
    ) -> Result<(), Failure>;
    async fn listing(&mut self) -> Result<CloudListing, Failure>;
}

struct SteamRemote<'a> {
    cm: &'a mut SteamConnection,
    http: &'a reqwest::Client,
    app_id: u32,
    control: &'a Control,
}

impl RemoteSource for SteamRemote<'_> {
    async fn download_files(
        &mut self,
        files: &[CloudFile],
        receive: impl FnMut(&CloudFile, Vec<u8>) -> Result<(), CloudError>,
    ) -> Result<(), Failure> {
        Ok(self
            .cm
            .cloud_download_files(
                self.http,
                self.app_id,
                files,
                || self.control.check().is_ok(),
                receive,
            )
            .await?)
    }
    async fn listing(&mut self) -> Result<CloudListing, Failure> {
        Ok(self.cm.cloud_listing(self.app_id).await?)
    }
}

impl Session {
    pub(super) async fn remote(
        &mut self,
        cm: &mut SteamConnection,
        http: &reqwest::Client,
        listing: &CloudListing,
    ) -> Result<RemoteView, Failure> {
        let control = self.control.clone();
        let mut source = SteamRemote {
            cm,
            http,
            app_id: self.config.app_id,
            control: &control,
        };
        self.read_remote(&mut source, listing).await
    }

    fn cached_file(&self, file: &CloudFile) -> Option<FileVersion> {
        // Context survives restart. Its listing can have been refreshed before
        // remote bytes, so never trust that pairing: verify the actual blob
        // against the newly listed Steam hash and size on every cache hit.
        let mut checked = std::collections::BTreeSet::new();
        [
            Some(&self.downloaded),
            self.context.as_ref().map(|c| &c.remote.files),
            self.vault.baseline().map(|b| &b.files),
            self.vault.local(),
        ]
        .into_iter()
        .flatten()
        .filter_map(|files| files.0.get(&file.name))
        .filter(|cached| cached.size == u64::from(file.size))
        .filter(|cached| checked.insert((&cached.hash, cached.size)))
        .find_map(|cached| {
            let bytes = self.vault.read_content(cached).ok()?;
            file.verify(&bytes).ok()?;
            Some(FileVersion {
                platforms: file.platforms,
                ..cached.clone()
            })
        })
    }

    async fn read_remote(
        &mut self,
        source: &mut impl RemoteSource,
        listing: &CloudListing,
    ) -> Result<RemoteView, Failure> {
        if listing.steam_id != self.credentials.steam_id || listing.app_id != self.config.app_id {
            return Err(Failure::Authentication);
        }
        let paths = self.context()?.paths()?;
        let mut files = Manifest::default();
        let mut missing = Vec::new();
        for file in &listing.files {
            self.control.check()?;
            if paths.existing_file(file)?.is_none() {
                continue;
            }
            if let Some(version) = self.cached_file(file) {
                files.0.insert(file.name.clone(), version);
            } else {
                missing.push(file.clone());
            }
        }
        source
            .download_files(&missing, |file, bytes| {
                let version = self
                    .vault
                    .stage(&bytes, file.platforms)
                    .map_err(|_| CloudError::Storage)?;
                self.downloaded.0.insert(file.name.clone(), version.clone());
                files.0.insert(file.name.clone(), version);
                Ok(())
            })
            .await?;
        // Even an entirely cached view is valid only while the remote listing
        // stays unchanged. No cache may authorize a stale conflict decision.
        if source.listing().await? != *listing {
            return Err(Failure::Changed);
        }
        self.control.check()?;
        self.downloaded = files.clone();
        Ok(RemoteView {
            revision: listing.change_number.to_string(),
            files,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use steam_kit::cloud::CloudMetadata;

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
        fn new(session: &Session) -> Self {
            Self {
                listing: CloudListing {
                    steam_id: session.credentials.steam_id,
                    app_id: session.config.app_id,
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

    fn fixture() -> (tempfile::TempDir, Session, Source) {
        let (temp, mut session, _) = super::super::removal_tests::fixture();
        // The removal fixture uses synthetic names. This fixture exercises the
        // real AutoCloud resolver with its canonical GameInstall names.
        session.vault.record_local(&Manifest::default()).unwrap();
        let rule = CloudMetadata::Object(
            [
                ("root", "GameInstall"),
                ("path", ""),
                ("pattern", "*.sav"),
                ("recursive", "0"),
            ]
            .into_iter()
            .map(|(k, v)| (k.into(), CloudMetadata::Value(v.into())))
            .collect(),
        );
        let context = session.context.as_mut().unwrap();
        context.metadata = CloudMetadata::Object(vec![(
            "savefiles".into(),
            CloudMetadata::Object(vec![("0".into(), rule)]),
        )]);
        context.installation_removed = false;
        context.save().unwrap();
        let source = Source::new(&session);
        (temp, session, source)
    }

    #[test]
    fn activation_without_early_capture_preserves_files_and_later_capture_reads_current_bytes() {
        let (_temp, mut session, source) = fixture();
        // The shared fixture uses synthetic ownership names. Start an empty
        // archive for this test of the real AutoCloud projection.
        session.config.root = session.config.root.with_file_name("fresh-cloud");
        session.vault = Vault::open(
            &session.config.root,
            scope(&session.config, session.credentials.steam_id),
        )
        .unwrap();
        session.context.as_mut().unwrap().config = session.config.clone();
        let save = session.config.install.join("slot.sav");
        std::fs::write(&save, FIRST).unwrap();
        session.context.as_mut().unwrap().listing = source.listing;
        let before = session.vault.local().cloned();
        session.activate_before_launch().unwrap();
        session.context().unwrap().save().unwrap();
        assert_eq!(std::fs::read(&save).unwrap(), FIRST);
        assert_eq!(session.vault.local(), before.as_ref());

        // An interrupted network check has not discarded the physical saves.
        let config = session.config.clone();
        let credentials = session.credentials.clone();
        let control = session.control.clone();
        drop(session);
        let mut session = Session::open(
            config,
            credentials,
            uuid::Uuid::new_v4().to_string(),
            control,
        )
        .unwrap();
        std::fs::write(&save, SECOND).unwrap();
        let captured = session.capture_before_launch().unwrap().unwrap();
        assert_eq!(
            session.vault.read_content(&captured.0[NAME]).unwrap(),
            SECOND
        );
        assert_eq!(session.vault.local(), Some(&captured));
    }

    #[test]
    fn unchanged_files_reuse_verified_bytes_across_retry_and_restart() {
        let (_temp, mut session, mut source) = fixture();
        let runtime = session.runtime.clone();
        let listing = source.listing.clone();
        let remote = runtime
            .block_on(session.read_remote(&mut source, &listing))
            .unwrap();
        runtime
            .block_on(session.read_remote(&mut source, &listing))
            .unwrap();
        assert_eq!(source.downloads, 1);
        assert_eq!(source.listings, 2);
        session.set_listing(listing.clone(), remote).unwrap();
        let config = session.config.clone();
        let credentials = session.credentials.clone();
        let control = session.control.clone();
        drop(session);
        let mut session = Session::open(
            config,
            credentials,
            uuid::Uuid::new_v4().to_string(),
            control,
        )
        .unwrap();
        let runtime = session.runtime.clone();
        runtime
            .block_on(session.read_remote(&mut source, &listing))
            .unwrap();
        assert_eq!(source.downloads, 1);
        assert_eq!(source.listings, 3);
    }

    #[test]
    fn changed_content_missing_blob_and_remote_races_never_use_stale_cache() {
        let (_temp, mut session, mut source) = fixture();
        let runtime = session.runtime.clone();
        let first = source.listing.clone();
        let remote = runtime
            .block_on(session.read_remote(&mut source, &first))
            .unwrap();
        let blob = session
            .config
            .root
            .join(session.credentials.steam_id.to_string())
            .join(session.config.app_id.to_string())
            .join("blobs")
            .join(remote.files.0[NAME].hash.as_str());
        std::fs::remove_file(&blob).unwrap();
        runtime
            .block_on(session.read_remote(&mut source, &first))
            .unwrap();
        assert_eq!(source.downloads, 2);
        source.listing.change_number = 2;
        source.listing.files[0].sha1 = "2df584286a54a4b83b7fc9161069b37d61cf8e07".into();
        source.bytes = SECOND.into();
        let second = source.listing.clone();
        let remote = runtime
            .block_on(session.read_remote(&mut source, &second))
            .unwrap();
        assert_eq!(source.downloads, 3);
        assert_eq!(
            session.vault.read_content(&remote.files.0[NAME]).unwrap(),
            SECOND
        );
        source.listing.change_number = 3;
        assert!(matches!(
            runtime.block_on(session.read_remote(&mut source, &second)),
            Err(Failure::Changed)
        ));
        assert_eq!(source.downloads, 3);
        let mut foreign = second.clone();
        foreign.steam_id += 1;
        assert!(matches!(
            runtime.block_on(session.read_remote(&mut source, &foreign)),
            Err(Failure::Authentication)
        ));
        source.listing.files.clear();
        let deleted = source.listing.clone();
        let remote = runtime
            .block_on(session.read_remote(&mut source, &deleted))
            .unwrap();
        assert!(remote.files.0.is_empty());
        assert!(session.downloaded.0.is_empty());
        session.control.cancel();
        assert!(matches!(
            runtime.block_on(session.read_remote(&mut source, &second)),
            Err(Failure::Cancelled)
        ));
    }
}
