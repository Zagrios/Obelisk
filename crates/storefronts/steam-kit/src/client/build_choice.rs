//! Steam's answers to the build choice: the builds the account's licenses
//! grant, and a Mac build's depot manifests, read to find the program and
//! Steam API library a launch would start.
use std::sync::Arc;

use obelisk_core::{
    build_choice::{GameBuilds, Inspection, Launch, Offered, Verdict},
    mac_build::Listing,
    platform::Platform,
};

use super::{
    Client, ClientError,
    build::{Kept, Revision, Selection},
};
use crate::{
    cm::{AppInfo, ContentLicenses},
    content::{CancelToken, DepotFiles, DepotSource},
};

/// The account's licenses and a game's PICS metadata, from which both of
/// its builds are selected.
#[derive(Clone)]
pub(super) struct Licensed {
    pub(super) owned: Arc<ContentLicenses>,
    pub(super) app: Arc<AppInfo>,
}

impl Licensed {
    /// The builds offered, of the Mac build and the Windows build, each
    /// when the account's licenses select depots of it.
    pub(super) fn offered(self, mac: bool, windows: bool) -> Offered<Licensed> {
        Offered {
            mac: mac.then(|| self.clone()),
            windows: windows.then_some(self),
        }
    }
}

/// A game the account owns, with the depots of `language`.
pub(super) struct OwnedApp<'a> {
    pub(super) client: &'a Client,
    pub(super) app_id: u32,
    pub(super) language: &'a str,
}

impl GameBuilds for OwnedApp<'_> {
    type Build = Licensed;
    type Key = Revision;
    type Kept = Kept;
    type Error = ClientError;
    /// Steam's errors tell a refusal apart wherever they arise.
    type Failure = ClientError;

    async fn builds(&self) -> Result<Offered<Licensed>, ClientError> {
        let (owned, app) = self.client.owned_app(self.app_id).await?;
        let installable =
            |platform| !Selection::of(&app, &owned, platform, self.language).is_empty();
        let (mac, windows) = (installable(Platform::Mac), installable(Platform::Windows));
        Ok(Licensed { owned, app }.offered(mac, windows))
    }

    /// A build without a Mac launch configuration has no program to read,
    /// so it keeps no verdict.
    async fn key(&self, mac: &Licensed) -> Result<Option<Revision>, ClientError> {
        if crate::launch::configuration(&mac.app, Platform::Mac).is_none() {
            return Ok(None);
        }
        let selection = Selection::of(&mac.app, &mac.owned, Platform::Mac, self.language);
        let resolved = self.client.build_of(selection).await?;
        Ok(Some(resolved.revision()))
    }

    /// The build's program and the Steam API library beside it, as a launch
    /// would start them, read from the build's depots. The depots' manifests
    /// are kept for the download of a build that is picked.
    async fn inspect(
        &self,
        mac: &Licensed,
        cancel: &CancelToken,
    ) -> Result<Inspection<Kept>, ClientError> {
        let Some(executable) = crate::launch::configuration(&mac.app, Platform::Mac)
            .map(|config| config.executable.clone())
        else {
            return Ok(Inspection::no_program());
        };
        let (client, app) = (self.client, &*mac.app);
        let selection = Selection::of(app, &mac.owned, Platform::Mac, self.language);
        let resolved = client.build_of(selection).await?;
        let servers = client.content_servers(app.app_id).await?;
        // Each depot checks again, but a build may have none.
        cancel.check()?;
        let mut depots = Vec::new();
        for depot in resolved.depots() {
            let prepared = depot.prepare(&client.cm, cancel).await?;
            let request_code = prepared.request_code(&client.cm).await?;
            depots.push((prepared, request_code));
        }
        let authorizations: Vec<_> = depots
            .iter()
            .map(|(depot, _)| depot.authorization(&client.cm))
            .collect();
        let content = &client.content;
        let mut files = Vec::new();
        let mut listing = Listing::default();
        for ((depot, request_code), authorization) in depots.iter().zip(&authorizations) {
            let source = DepotSource {
                depot_id: depot.depot_id,
                manifest_id: depot.manifest_id,
                request_code: *request_code,
                depot_key: depot.key,
                servers: &servers,
                authorization: Some(authorization),
            };
            let depot_files = content.depot_files(source, cancel).await?;
            depot_files.list(&mut listing, files.len());
            files.push(depot_files);
        }
        let read = |(depot, file): (usize, usize), bytes: u64| {
            let files: &Vec<_> = &files;
            files[depot].read(file, bytes, cancel)
        };
        let launch = Launch {
            executable: &executable,
            libraries: &["libsteam_api.dylib"],
        };
        let verdict = Verdict::of(&listing, launch, read).await?;
        Ok(verdict.keeping(|| files.into_iter().map(DepotFiles::into_manifest).collect()))
    }

    /// A build Steam refuses or whose depots hold what Obelisk cannot
    /// install, as `ContentError::is_refusal` and `CmError::is_refusal` name
    /// them: an unsafe path, invalid data, a link Obelisk does not make, a
    /// status that is no outage, or Steam denying the account.
    fn is_refusal(failure: &ClientError) -> bool {
        matches!(failure, ClientError::Refused(_))
    }
}
