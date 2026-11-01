//! A Steam game's build for one Platform and language: the depots the
//! account's licenses select, the public manifest of each and the keys to
//! download them. The Mac pick, the update check and the download all read a
//! build from here.
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    ops::RangeInclusive,
    sync::Arc,
};

use futures_util::{StreamExt, TryStreamExt};
use obelisk_core::platform::Platform;

use super::{Client, ClientError, DepotAuthorization, InstalledDepot, PLATFORM};
use crate::{
    cm::{
        AppInfo, CONCURRENT_REQUESTS, CmError, ContentLicenses, DepotInfo, SteamConnection,
        supports_platform,
    },
    content::{CachedManifest, CancelToken},
};

#[cfg(test)]
mod tests;

/// Steamworks Common Redistributables, free to every account. Windows games
/// name its depots, such as Visual C++ or DirectX, for Steam to install and run
/// before their first launch.
const COMMON_REDISTRIBUTABLES: u32 = 228_980;
/// Its depots a download leaves out: .NET Framework and XNA, which needs it,
/// 229000 to 229012. Their installers do not work in Wine, and .NET games run
/// on Wine Mono instead. The others, such as Visual C++, install in the prefix
/// as Steam installs them under Proton.
const SKIPPED_REDISTRIBUTABLES: RangeInclusive<u32> = 229_000..=229_012;

/// What a pick of a native Mac build read, by depot and manifest.
pub(super) type Kept = HashMap<(u32, u64), CachedManifest>;

/// Names one immutable build: its app, and the public manifest of each depot a
/// download installs, in the order it installs them. A pick files its verdict
/// and what it read under it, and the download takes them back by it. It
/// carries no Platform: a game whose two builds share every depot names both
/// alike. Not Steam's `buildid`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct Revision {
    app_id: u32,
    /// (depot, manifest).
    manifests: Vec<(u32, u64)>,
}

#[cfg(test)]
impl Revision {
    /// The revision of `app_id` at `manifests`, as `Build::revision` names it.
    pub(super) fn fabricated(app_id: u32, manifests: &[(u32, u64)]) -> Self {
        Self {
            app_id,
            manifests: manifests.to_vec(),
        }
    }
}

/// The depots the account's licenses select of an app for a Platform and a
/// language. It reads nothing from Steam: the game's page and the builds
/// offered ask for nothing more.
pub(super) struct Selection<'a> {
    app: &'a AppInfo,
    depots: Vec<&'a DepotInfo>,
}

impl<'a> Selection<'a> {
    /// A game that does not ship for `platform`, or whose depots the licenses
    /// do not grant, selects nothing.
    pub(super) fn of(
        app: &'a AppInfo,
        owned: &ContentLicenses,
        platform: Platform,
        language: &str,
    ) -> Self {
        let selected = |depots| Self { app, depots };
        if !supports_platform(&app.os_list, platform) {
            return selected(Vec::new());
        }
        let eligible: Vec<_> = app
            .depots
            .iter()
            .filter(|depot| {
                let redistributable = is_redistributable(depot);
                supports_platform(&depot.os_list, platform)
                    && !(redistributable && SKIPPED_REDISTRIBUTABLES.contains(&depot.depot_id))
                    && (app.free_to_download
                        || owned.depots.contains(&depot.depot_id)
                        || redistributable)
                    && depot.dlc_app_id.is_none_or(|dlc| owned.apps.contains(&dlc))
                    && (depot.public_manifest.is_some() || depot.shared_from_app.is_some())
            })
            .collect();
        // Shared asset depots often have no OS restriction. They do not establish
        // that the application ships an executable for an otherwise unknown OS.
        if app.os_list.is_empty()
            && !eligible
                .iter()
                .any(|depot| depot.os_list.iter().any(|os| os == PLATFORM.name(platform)))
        {
            return selected(Vec::new());
        }
        let has_language = eligible
            .iter()
            .any(|depot| depot.language.as_deref() == Some(language));
        let language = if has_language { language } else { "english" };
        let eligible: Vec<_> = eligible
            .into_iter()
            .filter(|depot| {
                depot
                    .language
                    .as_deref()
                    .is_none_or(|value| value.is_empty() || value == language)
            })
            .collect();
        let arch = if eligible
            .iter()
            .any(|depot| depot.os_arch.as_deref() == Some("64"))
        {
            "64"
        } else {
            "32"
        };
        selected(
            eligible
                .into_iter()
                .filter(|depot| {
                    depot
                        .os_arch
                        .as_deref()
                        .is_none_or(|value| value == arch || value.is_empty())
                })
                .collect(),
        )
    }

    pub(super) fn is_empty(&self) -> bool {
        self.depots.is_empty()
    }

    /// What PICS says the selected depots weigh; a depot that states none
    /// counts for nothing.
    pub(super) fn size_bytes(&self) -> u64 {
        self.depots
            .iter()
            .filter_map(|depot| depot.size_bytes)
            .sum()
    }

    /// The apps whose PICS metadata names the manifests of the shared depots
    /// that have none of their own: each once, ascending, never the game's own
    /// app. One PICS query covers them all.
    fn sources(&self) -> Vec<u32> {
        let sources: BTreeSet<_> = self
            .depots
            .iter()
            .filter_map(|depot| source_app(self.app, depot))
            .collect();
        sources.into_iter().collect()
    }

    /// Reads each depot's public manifest, from its source app for a shared
    /// depot without one of its own. A depot with no public manifest is left
    /// out. `sources` holds the apps `sources` named.
    ///
    /// Fails `CmError::MissingApp` when `sources` lacks one, and
    /// `backend.steam.client.sharedManifestMissing` when a source lists no
    /// public manifest for the depot.
    fn resolve(&self, sources: &HashMap<u32, Arc<AppInfo>>) -> Result<Build, ClientError> {
        let app = self.app;
        let mut depots = Vec::new();
        for depot in &self.depots {
            let Some(manifest_id) = public_manifest(app, depot, sources)? else {
                continue;
            };
            depots.push(Depot {
                depot_id: depot.depot_id,
                manifest_id,
                key_app: if is_redistributable(depot) {
                    COMMON_REDISTRIBUTABLES
                } else {
                    app.app_id
                },
                authorizing_app: content_authorizing_app(app, depot),
                estimated_bytes: depot.size_bytes.unwrap_or(0),
            });
        }
        Ok(Build {
            app_id: app.app_id,
            depots,
        })
    }
}

impl Client {
    /// `selection` at its public manifests. The source apps come in one query
    /// through the app info cache, so a second build of the same game asks
    /// Steam nothing.
    pub(super) async fn build_of(&self, selection: Selection<'_>) -> Result<Build, ClientError> {
        let sources = self.cached_app_infos(&selection.sources()).await?;
        selection.resolve(&sources)
    }
}

/// A selection at its public manifests. It holds no secret.
pub(super) struct Build {
    app_id: u32,
    depots: Vec<Depot>,
}

impl Build {
    pub(super) fn depots(&self) -> &[Depot] {
        &self.depots
    }

    /// The only way to name a build: the pick and the download both name it
    /// from the very depots they read, so the two cannot drift.
    pub(super) fn revision(&self) -> Revision {
        Revision {
            app_id: self.app_id,
            manifests: self
                .depots
                .iter()
                .map(|depot| (depot.depot_id, depot.manifest_id))
                .collect(),
        }
    }

    /// Whether this build is another than `installed`: the depots and
    /// manifests differ, whatever their order. A build Steam would not
    /// install, with no depot, is no update.
    pub(super) fn is_update_of(&self, installed: &[InstalledDepot]) -> bool {
        if self.depots.is_empty() {
            return false;
        }
        let latest: HashSet<_> = self
            .depots
            .iter()
            .map(|depot| (depot.depot_id, depot.manifest_id))
            .collect();
        let installed: HashSet<_> = installed
            .iter()
            .map(|depot| (depot.depot_id, depot.manifest_id))
            .collect();
        latest != installed
    }

    /// Every depot's key, in the build's order, at most `CONCURRENT_REQUESTS`
    /// asked at once. It checks for cancellation first, and each depot again
    /// before its key. A build with no depot fails
    /// `backend.steam.client.noPublicManifest`.
    pub(super) async fn prepare(
        &self,
        cm: &SteamConnection,
        cancel: &CancelToken,
    ) -> Result<Vec<PreparedDepot>, ClientError> {
        cancel.check()?;
        if self.depots.is_empty() {
            return Err(ClientError::Failure(
                "backend.steam.client.noPublicManifest".into(),
            ));
        }
        let lookups: Vec<_> = self
            .depots
            .iter()
            .map(|depot| depot.prepare(cm, cancel))
            .collect();
        futures_util::stream::iter(lookups)
            .buffered(CONCURRENT_REQUESTS)
            .try_collect()
            .await
    }
}

/// One depot of a build at its public manifest, with the apps Steam answers
/// for it.
#[derive(Debug)]
pub(super) struct Depot {
    depot_id: u32,
    manifest_id: u64,
    /// Whose key Steam gives: the Common Redistributables' for their depots,
    /// else the game's.
    key_app: u32,
    /// Whom Steam authorizes the manifest request code and the CDN tokens
    /// through: the game when it is free to download, else the app the depot
    /// is shared from, else the game.
    authorizing_app: u32,
    estimated_bytes: u64,
}

impl Depot {
    /// Checks for cancellation, then asks for the depot's key. A key that is
    /// not 32 bytes fails `backend.steam.client.invalidDepotKey`.
    pub(super) async fn prepare(
        &self,
        cm: &SteamConnection,
        cancel: &CancelToken,
    ) -> Result<PreparedDepot, ClientError> {
        cancel.check()?;
        let key: [u8; 32] = cm
            .depot_key(self.depot_id, self.key_app)
            .await?
            .try_into()
            .map_err(|_| ClientError::Failure("backend.steam.client.invalidDepotKey".into()))?;
        Ok(PreparedDepot {
            depot_id: self.depot_id,
            manifest_id: self.manifest_id,
            authorizing_app: self.authorizing_app,
            key,
            estimated_bytes: self.estimated_bytes,
            picked: None,
        })
    }
}

// Intentionally no Debug: depot keys are credentials.
/// A depot with its key, ready for a manifest request code and the CDN.
pub(super) struct PreparedDepot {
    pub(super) depot_id: u32,
    pub(super) manifest_id: u64,
    pub(super) authorizing_app: u32,
    pub(super) key: [u8; 32],
    pub(super) estimated_bytes: u64,
    /// The manifest the build's pick read, as the CDN served it.
    pub(super) picked: Option<CachedManifest>,
}

impl PreparedDepot {
    /// A fresh code to download the depot's manifest from the CDN.
    pub(super) async fn request_code(&self, cm: &SteamConnection) -> Result<u64, ClientError> {
        cm.manifest_request_code(self.depot_id, self.authorizing_app, self.manifest_id)
            .await
            .map_err(Into::into)
    }

    /// How a CDN server that refuses the depot is authorized over the CM.
    pub(super) fn authorization<'a>(&self, cm: &'a SteamConnection) -> DepotAuthorization<'a> {
        DepotAuthorization {
            cm,
            app_id: self.authorizing_app,
            depot_id: self.depot_id,
        }
    }
}

fn is_redistributable(depot: &DepotInfo) -> bool {
    depot.shared_from_app == Some(COMMON_REDISTRIBUTABLES)
}

/// The app other than the game whose PICS metadata names the manifest of a
/// shared depot that has none of its own.
fn source_app(app: &AppInfo, depot: &DepotInfo) -> Option<u32> {
    depot
        .shared_from_app
        .filter(|source| depot.public_manifest.is_none() && *source != app.app_id)
}

/// The depot's public manifest ID, taken from its source app for a shared
/// depot without its own. `None` when it has no public manifest.
fn public_manifest(
    app: &AppInfo,
    depot: &DepotInfo,
    sources: &HashMap<u32, Arc<AppInfo>>,
) -> Result<Option<u64>, ClientError> {
    let Some(source) = source_app(app, depot) else {
        return Ok(depot.public_manifest);
    };
    sources
        .get(&source)
        .ok_or(CmError::MissingApp(source))?
        .depots
        .iter()
        .find(|other| other.depot_id == depot.depot_id)
        .and_then(|other| other.public_manifest)
        .map(Some)
        .ok_or_else(|| {
            ClientError::Failure(crate::message::format(
                "backend.steam.client.sharedManifestMissing",
                &[depot.depot_id.to_string()],
            ))
        })
}

fn content_authorizing_app(app: &AppInfo, depot: &DepotInfo) -> u32 {
    // FreeToDownload titles authorize shared depot requests through their own app,
    // even when the manifest itself must be looked up on the source app.
    if app.free_to_download {
        app.app_id
    } else {
        depot.shared_from_app.unwrap_or(app.app_id)
    }
}
