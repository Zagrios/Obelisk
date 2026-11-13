//! Starting an installed Epic or GOG build as a Game session: its
//! configuration checked, its Wine prefix readied, one request handed to a new
//! supervisor, and that supervisor's target. A storefront supplies what its
//! builds name of themselves and who signs its launch in.
mod handover;
mod request;
#[cfg(test)]
pub(crate) mod tests;

use crate::{installations, launch, profile, supervisor, wine};
use obelisk_core::{Platform, launch::LaunchTarget};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::path::Path;

pub(crate) use handover::{SignIn, hand_off, windows};
pub(crate) use request::{ArgumentFile, Request, Started};

/// What the shell needs of a storefront's Game session error beyond
/// `Provider`'s own: the conversions `session_error_conversions!` makes, and
/// the storefront's `Io`. `unsupported` and `session` are the two refusals the
/// shell raises itself. Blanket-implemented.
pub(crate) trait Refusals:
    From<launch::Error> + From<supervisor::Error> + From<std::io::Error>
{
    /// A build the storefront cannot start.
    fn unsupported() -> Self {
        launch::Error::InvalidApplication.into()
    }
    /// A handoff or a join the storefront cannot trust.
    fn session() -> Self {
        supervisor::Error::InvalidSession.into()
    }
}
impl<E> Refusals for E where E: From<launch::Error> + From<supervisor::Error> + From<std::io::Error> {}

/// What every storefront's configuration holds of one installed build,
/// borrowed. A view, not a shared struct: each storefront's `Configuration` is
/// its own record with `deny_unknown_fields`, which serde cannot combine with
/// `flatten`.
pub(crate) struct Build<'a> {
    pub home: &'a Path,
    pub install: &'a Path,
    pub session_directory: &'a Path,
    pub helper: &'a Path,
    pub platform: Platform,
    /// The game's name in the store, which the Linux engine shows.
    pub title: &'a str,
    /// Set for a Windows build, which runs in Wine.
    pub prefix: Option<&'a Path>,
    /// The game as Obelisk's profiles and logs name it.
    pub game: profile::Game<'a>,
}

impl<'a> Build<'a> {
    /// Whether `wine` runs this build in its own prefix, and only a Windows
    /// build.
    pub fn runs_in(&self, wine: Option<&wine::Config>) -> bool {
        self.prefix == wine.map(|wine| wine.prefix.as_path())
    }

    /// The game as Wine starts a Windows build.
    pub fn wine_game(&self) -> wine::Game<'a> {
        wine::Game {
            install: self.install,
            title: self.title,
        }
    }
}

/// What the installation's files say its build starts, and the programs its
/// Windows build runs once in its prefix first.
pub(crate) struct Installed {
    pub target: LaunchTarget,
    pub prerequisites: Vec<wine::Prerequisite>,
}

/// The credential of a storefront whose launch needs none (GOG): no key on the
/// wire.
#[derive(Serialize, Deserialize)]
pub(crate) struct NoCredential {}

/// What a storefront adds to the launch shell. Implemented on the marker
/// types.
pub(crate) trait Launchable:
    installations::Storefront<Account = String, Error: Refusals>
{
    /// What a Game session starts, as the handoff carries it.
    type Configuration: Clone + Serialize + DeserializeOwned + Send + Sync + 'static;
    /// What the supervisor signs in with, beside the account: Epic's one-use
    /// exchange code. Never `Debug`; it travels only through the private
    /// handoff pipe.
    type Credential: Serialize + DeserializeOwned + Send + 'static;

    fn build(configuration: &Self::Configuration) -> Build<'_>;
    /// The game a session of `configuration` records and its acknowledgement
    /// names.
    fn store(configuration: &Self::Configuration) -> Self::Store;
    /// What only this storefront's configuration can be wrong about. Runs
    /// first, so its error wins over the shared ones (Epic: an invalid save
    /// scope is `Vault`, `Busy` or `SharedSaves`).
    fn check(configuration: &Self::Configuration, account: &str) -> Result<(), Self::Error>;
    /// What the installation's files say its build starts and needs first.
    /// Reads files.
    fn installed(configuration: &Self::Configuration) -> Result<Installed, Self::Error>;
}

/// `configuration` for `account`: `Launchable::check`, then a Mac build with
/// no prefix or a Windows build in one, then every path plain and absolute.
/// Everything after `check` is the storefront's `Unsupported`. Epic's Cloud
/// record calls it too.
pub(crate) fn validate<G: Launchable>(
    configuration: &G::Configuration,
    account: &str,
) -> Result<(), G::Error> {
    G::check(configuration, account)?;
    let build = G::build(configuration);
    // A Mac build runs natively; a Windows build in its prefix.
    let prefix = match (build.platform, build.prefix) {
        (Platform::Mac, None) => None,
        (Platform::Windows, Some(prefix)) => Some(prefix),
        _ => return Err(G::Error::unsupported()),
    };
    if [
        build.home,
        build.install,
        build.session_directory,
        build.helper,
    ]
    .into_iter()
    .chain(prefix)
    .any(|path| !supervisor::plain_absolute(path))
    {
        return Err(G::Error::unsupported());
    }
    Ok(())
}

/// What a storefront adds to the prefix of an installation: the programs its
/// files name. Every other addition of a Wine storefront stays the default.
struct Prerequisites(Vec<wine::Prerequisite>);

impl wine::Storefront for Prerequisites {
    fn prerequisites(&self) -> Result<Vec<wine::Prerequisite>, launch::Error> {
        Ok(self.0.clone())
    }
}

/// The conversions a storefront's session `Error` makes from the supervisor's
/// errors and the launch failure, alike for Epic and GOG: its `Session`,
/// `Busy`, `Cancelled`, `Storage`, `Unsupported`, `RosettaUnavailable` and
/// `Launch` stand for them.
macro_rules! session_error_conversions {
    ($error:ty) => {
        impl From<$crate::supervisor::Error> for $error {
            fn from(error: $crate::supervisor::Error) -> Self {
                use $crate::supervisor::Error as E;
                match error {
                    E::Storage => Self::Storage,
                    E::NewerSession | E::Busy => Self::Busy,
                    E::Cancelled => Self::Cancelled,
                    E::RosettaUnavailable => Self::RosettaUnavailable,
                    E::InvalidSession | E::Unavailable | E::StartFailed => Self::Session,
                }
            }
        }
        impl From<$crate::launch::Error> for $error {
            fn from(error: $crate::launch::Error) -> Self {
                use $crate::launch::Error as E;
                match error {
                    E::IncompatibleArchitecture | E::WineUnavailable => Self::Launch(error),
                    E::InvalidApplication | E::NotExecutable => Self::Unsupported,
                    // Epic's `Storage` key speaks of saves, not of readying a launch.
                    E::Storage | E::StartFailed | E::Unavailable => Self::Session,
                }
            }
        }
    };
}
pub(crate) use session_error_conversions;
