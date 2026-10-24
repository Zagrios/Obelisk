//! The Epic Cloud service as a sync session uses it. Production passes the
//! authenticated `epic_kit` client; tests pass an in-memory store.
use super::*;
use crate::cloud::view::{self, Listed};
use epic_kit::cloud::CloudListing;
use std::{collections::BTreeSet, future::Future};

pub(crate) trait CloudRemote: Send + Sync + 'static {
    /// The account of the authenticated session, never a remembered one.
    fn account_id(&self) -> impl Future<Output = std::result::Result<String, EpicError>> + Send;

    fn cloud_configuration(
        &self,
        namespace: &str,
        catalog_item_id: &str,
        app_name: &str,
        platform: Platform,
    ) -> impl Future<Output = std::result::Result<Option<CloudRules>, EpicError>> + Send;

    /// What the catalog says of the game's Cloud, by attribute, to tell a
    /// player why a game has none. Nothing by default.
    fn cloud_attributes(
        &self,
        _namespace: &str,
        _catalog_item_id: &str,
        _app_name: &str,
    ) -> impl Future<Output = std::result::Result<BTreeMap<String, String>, EpicError>> + Send {
        async { Ok(BTreeMap::new()) }
    }

    /// The game's Cloud now: each file's name, size and hash.
    fn cloud_list(
        &self,
        game: CloudGame<'_>,
        cancel: &CancelToken,
    ) -> impl Future<Output = std::result::Result<CloudListing, EpicError>> + Send;

    /// The revision the game's Cloud has now, for `listing`'s account.
    fn cloud_revision(
        &self,
        game: CloudGame<'_>,
        listing: &CloudListing,
        cancel: &CancelToken,
    ) -> impl Future<Output = std::result::Result<String, EpicError>> + Send;

    /// Reads `names` of `listing`, each checked against its hash.
    fn cloud_fetch(
        &self,
        game: CloudGame<'_>,
        listing: &CloudListing,
        names: &BTreeSet<String>,
        cancel: &CancelToken,
    ) -> impl Future<Output = std::result::Result<BTreeMap<String, Vec<u8>>, EpicError>> + Send;

    /// Publishes a complete set, then returns the listing its verified
    /// readback confirmed.
    fn cloud_upload(
        &self,
        game: CloudGame<'_>,
        rules: &CloudRules,
        files: &BTreeMap<String, Vec<u8>>,
        expected_revision: &str,
        cancel: &CancelToken,
    ) -> impl Future<Output = std::result::Result<CloudListing, EpicError>> + Send;
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
        platform: Platform,
    ) -> std::result::Result<Option<CloudRules>, EpicError> {
        EpicClient::cloud_configuration(self, namespace, catalog_item_id, app_name, platform).await
    }

    async fn cloud_attributes(
        &self,
        namespace: &str,
        catalog_item_id: &str,
        app_name: &str,
    ) -> std::result::Result<BTreeMap<String, String>, EpicError> {
        EpicClient::cloud_attributes(self, namespace, catalog_item_id, app_name).await
    }

    async fn cloud_list(
        &self,
        game: CloudGame<'_>,
        cancel: &CancelToken,
    ) -> std::result::Result<CloudListing, EpicError> {
        EpicClient::cloud_list(self, game, cancel).await
    }

    async fn cloud_revision(
        &self,
        game: CloudGame<'_>,
        listing: &CloudListing,
        cancel: &CancelToken,
    ) -> std::result::Result<String, EpicError> {
        EpicClient::cloud_revision(self, game, listing, cancel).await
    }

    async fn cloud_fetch(
        &self,
        game: CloudGame<'_>,
        listing: &CloudListing,
        names: &BTreeSet<String>,
        cancel: &CancelToken,
    ) -> std::result::Result<BTreeMap<String, Vec<u8>>, EpicError> {
        EpicClient::cloud_fetch(self, game, listing, names, cancel).await
    }

    async fn cloud_upload(
        &self,
        game: CloudGame<'_>,
        rules: &CloudRules,
        files: &BTreeMap<String, Vec<u8>>,
        expected_revision: &str,
        cancel: &CancelToken,
    ) -> std::result::Result<CloudListing, EpicError> {
        EpicClient::cloud_upload(self, game, rules, files, expected_revision, cancel).await
    }
}

/// What the last view verified of the files Obelisk does not manage. An
/// upload republishes them.
pub(crate) struct Kept {
    revision: String,
    files: BTreeMap<String, Vec<u8>>,
}

/// An EGS Cloud listing, split by the game's Cloud rules.
pub(crate) struct EpicListing {
    listing: CloudListing,
    /// The saves Obelisk manages, with their sizes.
    managed: Vec<(String, u64)>,
    unmanaged: BTreeSet<String>,
}

impl EpicListing {
    pub(super) fn new(listing: CloudListing, rules: &CloudRules) -> Result<Self> {
        let filters = rules.filters()?;
        let mut managed = Vec::new();
        let mut unmanaged = BTreeSet::new();
        for (name, size) in listing.files() {
            if filters.accepts(name)? {
                managed.push((name.to_owned(), size));
            } else {
                unmanaged.insert(name.to_owned());
            }
        }
        Ok(Self {
            listing,
            managed,
            unmanaged,
        })
    }
}

impl view::Listing for EpicListing {
    fn revision(&self) -> String {
        self.listing.revision().to_owned()
    }
    fn saves(&self) -> Vec<Listed<'_>> {
        self.managed
            .iter()
            .map(|(name, size)| Listed {
                name,
                size: *size,
                platforms: 2,
            })
            .collect()
    }
    fn holds(&self, name: &str, bytes: &[u8]) -> bool {
        self.listing.holds(name, bytes)
    }
}

/// EGS Cloud for one game and account, for one attempt.
pub(crate) struct EpicRemote<'a, R> {
    pub(super) api: &'a R,
    pub(super) game: CloudGame<'a>,
    pub(super) rules: &'a CloudRules,
    pub(super) kept: &'a mut Option<Kept>,
    /// Unmanaged files this remote's read fetched.
    pub(super) unmanaged: Option<BTreeMap<String, Vec<u8>>>,
    pub(super) control: &'a Control,
}

impl<R: CloudRemote> view::Remote for EpicRemote<'_, R> {
    type Failure = Error;
    type Listing = EpicListing;
    const JOURNAL: &'static str = "epic-egs-cloud";

    async fn list(&mut self) -> Result<EpicListing> {
        let listing = self
            .api
            .cloud_list(self.game, self.control.cancellation())
            .await?;
        EpicListing::new(listing, self.rules)
    }
    async fn unchanged(&mut self, listing: &EpicListing) -> Result<bool> {
        let revision = self
            .api
            .cloud_revision(self.game, &listing.listing, self.control.cancellation())
            .await?;
        Ok(revision == listing.listing.revision())
    }
    /// Also reads the files Obelisk does not manage, under the same listing,
    /// for the next upload to republish.
    async fn fetch(
        &mut self,
        listing: &EpicListing,
        names: &[&str],
        mut receive: impl FnMut(&str, Vec<u8>) -> std::result::Result<(), cloud_vault::Error>,
    ) -> Result<()> {
        let mut wanted: BTreeSet<String> = names.iter().map(|name| (*name).to_owned()).collect();
        wanted.extend(listing.unmanaged.iter().cloned());
        let mut unmanaged = BTreeMap::new();
        if !wanted.is_empty() {
            let files = self
                .api
                .cloud_fetch(
                    self.game,
                    &listing.listing,
                    &wanted,
                    self.control.cancellation(),
                )
                .await?;
            for (name, bytes) in files {
                if listing.unmanaged.contains(&name) {
                    unmanaged.insert(name, bytes);
                } else {
                    receive(&name, bytes)?;
                }
            }
        }
        self.unmanaged = Some(unmanaged);
        Ok(())
    }
    fn adopt(&mut self, listing: EpicListing, _: &RemoteView, _: &Vault) -> Result<()> {
        // A commit reads nothing: the next upload needs a fresh view.
        if let Some(files) = self.unmanaged.take() {
            *self.kept = Some(Kept {
                revision: listing.listing.revision().into(),
                files,
            });
        }
        Ok(())
    }
    async fn commit(
        &mut self,
        change: view::Change<'_, Error>,
    ) -> Result<view::Committed<EpicListing>> {
        let kept = self
            .kept
            .take()
            .filter(|kept| kept.revision == change.view().revision)
            .ok_or(Error::Recovery)?;
        // Retained remote bytes move into the upload set; the kept copy
        // would otherwise stay live alongside the packaged one.
        let files = upload_files(change.vault(), self.rules, kept.files, change.local())?;
        self.control.check()?;
        let (api, game, rules) = (self.api, self.game, self.rules);
        let cancel = self.control.cancellation();
        // Steam's confirming listing comes from its commit reply. EGS gives
        // no such receipt, so `cloud_upload` reads the whole published set
        // back and returns the listing that readback confirmed.
        change
            .send(
                || Ok(()),
                async || {
                    let listing = api
                        .cloud_upload(game, rules, &files, &kept.revision, cancel)
                        .await?;
                    EpicListing::new(listing, rules)
                },
            )
            .await
    }
}
