//! Handing an admitted build to a new supervisor: its launch settings
//! resolved, its Wine prefix readied, a credential asked of whoever signs the
//! launch in.
use super::{Launchable, Prerequisites, Refusals, request};
use crate::{
    Locations,
    installations::{Receipt, blocking},
    sessions::{self, Admission, BeforeBegin, BoxFuture, Failure, Supervised, failed},
    wine,
};
use obelisk_core::{Platform, transfer::CancelToken};
use std::{path::PathBuf, sync::Arc};

/// Who signs a launch in. Asked once the game's prefix is ready.
pub(crate) trait SignIn<G: Launchable>: Send + Sync + 'static {
    /// The account the game is launched for, and what its supervisor signs in
    /// with.
    fn credential(&self) -> BoxFuture<'_, Result<(String, G::Credential), Failure>>;
    /// Runs once the new supervisor holds the game, before its start gate
    /// opens.
    fn before_begin(&self) -> BeforeBegin {
        Box::new(|| {})
    }
}

/// A receipt's launch settings, resolved once off the async runtime (finding
/// Easy Anti-Cheat may read hundreds of folders). A failure ends the launch
/// with `G::Error::from(error)`, as Steam's does; a lost join is
/// `G::Error::session()`. Separate from `hand_off`, so a storefront acts only on
/// the hand-off (Epic saves the credentials it rotated, but not after a launch
/// that never began) and a test hands over a resolved `Installation`.
pub(crate) async fn windows<G: Launchable>(
    receipt: &Receipt<G>,
    locations: &Locations,
) -> Result<Option<wine::Installation>, Failure> {
    let lost = G::Error::session().to_string();
    receipt
        .windows_for_launch(locations, lost)
        .await?
        .map_err(|error| failed(G::Error::from(error)))
}

/// Hands the admitted build to a new supervisor through
/// `Admission::hand_off`: the runtimes (and the Voile answer), then, for a
/// Windows build, `ready` off the async runtime, then `sign_in.credential()`,
/// then `start`. `configuration` receives the prefix of `windows`, the only way
/// a configuration gets one.
pub(crate) async fn hand_off<G: Launchable>(
    admission: Admission<G>,
    windows: Option<wine::Installation>,
    locations: &Locations,
    configuration: impl FnOnce(Option<PathBuf>) -> G::Configuration + Send,
    sign_in: impl SignIn<G>,
) -> Result<(), Failure> {
    let prefix = windows.as_ref().map(|windows| windows.prefix().to_owned());
    let handover = Handover {
        sign_in,
        configuration: configuration(prefix),
        windows,
        locations: locations.clone(),
    };
    admission.hand_off(handover).await
}

/// Starting an installed build: its Wine prefix readied, then its credential,
/// then its supervisor.
pub(super) struct Handover<G: Launchable, S> {
    pub(super) sign_in: S,
    pub(super) configuration: G::Configuration,
    /// Set for a Windows build, whose prefix it readies.
    pub(super) windows: Option<wine::Installation>,
    pub(super) locations: Locations,
}

/// The Windows-ready step: reads the installation's files, then readies
/// `windows`' prefix with the prerequisites they name. Blocks. Runs before any
/// credential exists, since a prerequisite may take minutes.
fn ready<G: Launchable>(
    windows: &wine::Installation,
    configuration: &G::Configuration,
    locations: &Locations,
) -> Result<wine::Config, G::Error> {
    let installed = G::installed(configuration)?;
    Ok(windows.ready(
        locations,
        &installed.target,
        &Prerequisites(installed.prerequisites),
    )?)
}

impl<G: Launchable, S: SignIn<G>> sessions::Handover<G> for Handover<G, S> {
    /// The account, its credential, and how a Windows build runs.
    type Prepared = (String, G::Credential, Option<wine::Config>);

    fn platform(&self) -> Platform {
        G::build(&self.configuration).platform
    }

    fn windows(&self) -> Option<&wine::Installation> {
        self.windows.as_ref()
    }

    fn prepare(&self) -> BoxFuture<'_, Result<Self::Prepared, Failure>> {
        Box::pin(async move {
            // A Windows build runs in Wine, readied before any credential
            // exists: a prerequisite may take minutes.
            let wine = match &self.windows {
                None => None,
                Some(windows) => {
                    let (windows, configuration, locations) = (
                        windows.clone(),
                        self.configuration.clone(),
                        self.locations.clone(),
                    );
                    let lost = G::Error::session().to_string();
                    Some(
                        blocking(lost, move || {
                            ready::<G>(&windows, &configuration, &locations)
                        })
                        .await?
                        .map_err(failed)?,
                    )
                }
            };
            let (account, credential) = self.sign_in.credential().await?;
            Ok((account, credential, wine))
        })
    }

    fn before_begin(&self) -> BeforeBegin {
        self.sign_in.before_begin()
    }

    fn start(
        self,
        (account, credential, wine): Self::Prepared,
        cancel: CancelToken,
    ) -> Result<Arc<dyn Supervised<G>>, String> {
        request::start::<G>(self.configuration, account, credential, wine, cancel)
            .map(|client| Arc::new(client) as Arc<dyn Supervised<G>>)
            .map_err(|error| error.to_string())
    }
}
