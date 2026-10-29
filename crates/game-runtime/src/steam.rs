//! The Steam adapter of the Game session supervisor.
use crate::{
    installations::Receipt,
    launch_settings::SteamClient,
    protocol::Credentials,
    runtimes::Runtime,
    supervisor::{self, Record, Target},
};
use host::{Config, HostState};
use obelisk_core::{Platform, transfer::CancelToken};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub mod account_session;
pub mod cloud;
pub mod content;
pub mod host;
pub(crate) mod launch;
mod windows;
mod windows_steam;
pub use launch::serve;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Steam;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Store {
    pub app_id: u32,
    pub host: HostState,
    #[serde(flatten)]
    pub requests: host::GameRequests,
}
impl Store {
    /// A new session of game `app_id`, its host online.
    pub fn new(app_id: u32) -> Self {
        Self {
            app_id,
            host: HostState::Online,
            requests: host::GameRequests::default(),
        }
    }
    /// What the host tells now.
    pub(crate) fn apply(&mut self, update: &host::HostUpdate) {
        self.host = update.state;
        self.requests = update.requests.clone();
    }
}

/// Why a Steam launch, its host or its Cloud failed, as a localization key.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Cloud(cloud::Failure),
    #[error("backend.steam.runtime.sessionBusy")]
    SessionBusy,
    #[error("backend.steam.runtime.cancelled")]
    Cancelled,
    #[error("backend.steam.runtime.invalidSession")]
    InvalidSession,
    #[error("backend.steam.runtime.unavailable")]
    Unavailable,
    #[error("backend.steam.runtime.versionMismatch")]
    VersionMismatch,
    #[error("backend.steam.runtime.startFailed")]
    StartFailed,
    #[error("backend.steam.runtime.authenticationFailed")]
    Authentication,
    #[error("backend.steam.runtime.ownershipFailed")]
    Ownership,
    #[error("backend.steam.runtime.timeout")]
    Timeout,
    #[error("backend.steam.runtime.storageFailed")]
    Storage,
    #[error("backend.runtime.rosettaUnavailable")]
    RosettaUnavailable,
    #[error("backend.steam.runtime.apiMissing")]
    ApiMissing,
    /// What readying or starting the game failed with, shown as every
    /// storefront shows it.
    #[error("{0}")]
    Launch(crate::launch::Error),
    /// Valve's Windows Steam starts the game, and is signed in to no account.
    #[error("backend.steam.windowsSteam.signInRequired")]
    WindowsSteamSignedOut,
    /// Valve's Windows Steam did not take the account's login.
    #[error("backend.steam.windowsSteam.signInFailed")]
    WindowsSteamSignIn,
}
impl From<std::io::Error> for Error {
    fn from(_: std::io::Error) -> Self {
        Self::Storage
    }
}
impl From<supervisor::Error> for Error {
    fn from(error: supervisor::Error) -> Self {
        use supervisor::Error as E;
        match error {
            E::Storage => Self::Storage,
            E::InvalidSession => Self::InvalidSession,
            E::NewerSession | E::Busy => Self::SessionBusy,
            E::Unavailable => Self::Unavailable,
            E::Cancelled => Self::Cancelled,
            E::StartFailed => Self::StartFailed,
            E::RosettaUnavailable => Self::RosettaUnavailable,
        }
    }
}
impl From<crate::launch::Error> for Error {
    fn from(error: crate::launch::Error) -> Self {
        use crate::launch::Error as E;
        match error {
            E::Storage => Self::Storage,
            E::StartFailed => Self::StartFailed,
            E::Unavailable => Self::Unavailable,
            E::IncompatibleArchitecture
            | E::InvalidApplication
            | E::NotExecutable
            | E::WineUnavailable => Self::Launch(error),
        }
    }
}

/// What the player asks of a running Steam game, through its supervisor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Control {
    /// Obelisk's overlay opened or closed over the game.
    Overlay { active: bool, user_initiated: bool },
    /// The player accepted `friend`'s invitation into the game.
    AcceptInvite {
        friend: u64,
        target: crate::protocol::InviteTarget,
    },
    /// The player's answer to the purchase the game waits on.
    AuthorizeMicroTxn { order_id: u64, approve: bool },
}

/// What a Steam game joins as it starts, as Steam's client starts a game
/// from an invitation it accepted (steamui's `steam://joinlobby` and
/// `steam://rungame`): the lobby or connect string a running game accepts.
pub type Join = crate::protocol::InviteTarget;

impl Join {
    /// The arguments it adds after the game's own, for a build of
    /// `platform`: `+connect_lobby <lobby>`, or the connect string split as
    /// that platform's command line does, never through a shell.
    pub fn arguments(&self, platform: Platform) -> Vec<String> {
        match self {
            Self::Lobby { lobby_id } => vec!["+connect_lobby".into(), lobby_id.to_string()],
            Self::Connect { connect } => obelisk_core::launch::parse_arguments(connect, platform)
                .unwrap_or_else(|_| connect.split_whitespace().map(str::to_owned).collect()),
        }
    }
}

pub type Snapshot = Record<Steam>;
pub type Client = supervisor::Client<Steam>;
pub type IdleGuard = supervisor::IdleGuard<Steam>;

#[derive(Serialize, Deserialize)]
pub struct Request {
    pub root: PathBuf,
    pub id: String,
    pub config: Config,
    pub target: Target,
    #[serde(default)]
    pub cloud: Option<cloud::Config>,
}

impl supervisor::Provider for Steam {
    type Account = u64;
    type Store = Store;
    type Control = Control;
    type Error = Error;
    const SUPERVISE: &'static str = "supervise";
    const CANCELLED: &'static [&'static str] = &["backend.steam.runtime.cancelled"];

    fn valid(record: &Record<Self>) -> bool {
        record.store.app_id != 0 && record.account_id != 0
    }

    fn cancelled(error: &Error) -> bool {
        matches!(
            error,
            Error::Cancelled | Error::Cloud(cloud::Failure::Cancelled)
        )
    }

    fn same_game(a: &Store, b: &Store) -> bool {
        a.app_id == b.app_id
    }

    /// The UI reports host loss and keeps protecting a game that still runs.
    fn orphaned(record: &mut Record<Self>, _owned: bool) {
        record.store.host = HostState::Lost;
    }
}

/// The launch of the Steam game `receipt` installed, started as `target`
/// says, with its Cloud, ready for its supervisor. A Windows game runs in
/// Wine, in the installation's own prefix, which is readied first, as
/// `windows` resolved it: the receipt's own `Receipt::windows`, None for a
/// Mac build.
pub fn prepare(
    locations: &crate::Locations,
    receipt: &Receipt<Steam>,
    windows: Option<&crate::wine::Installation>,
    target: &obelisk_core::launch::LaunchTarget,
) -> Result<Request, Error> {
    let home = &locations.home;
    let install = Path::new(&receipt.directory);
    let app_id = receipt.product.app_id;
    let (wine, target, steam_api) = match windows {
        Some(windows) if windows.steam_client() == SteamClient::Windows => {
            let storefront = windows_steam::Prefix {
                install,
                app_id,
                prefix: windows.prefix(),
            };
            let wine = windows.ready(locations, target, &storefront)?;
            // Steam would wait, hidden, for a sign-in no one sees.
            if windows_steam::account(&wine.prefix).is_none() {
                return Err(Error::WindowsSteamSignedOut);
            }
            let launched =
                windows_steam::target(&wine, &receipt.wine_game(), target, app_id, home)?;
            (Some(wine), launched, None)
        }
        Some(windows) => {
            let wine = windows.ready(locations, target, &windows::Prefix { install })?;
            let launched = windows::target(&wine, &receipt.wine_game(), target, home)?;
            (Some(wine), launched, None)
        }
        None => {
            let (launched, steam_api) = native_target(install, target, home, app_id)?;
            (None, launched, Some(steam_api))
        }
    };
    // Valve's Windows Steam keeps the game's Cloud saves itself.
    let cloud = (!windows_steam(wine.as_ref())).then(|| cloud::Config {
        root: locations.cloud_archive.clone(),
        home: home.clone(),
        install: install.to_owned(),
        app_id,
        prefix: wine.as_ref().map(|wine| wine.prefix.clone()),
    });
    Ok(Request {
        root: locations.steam_sessions.clone(),
        id: String::new(),
        config: Config {
            app_id,
            cloud_save_roots: vec![install.to_owned()],
            helpers: locations.helpers.clone(),
            package: locations.steam_runtime_package.clone(),
            cache: locations.steam_runtime_cache.clone(),
            diagnostics: crate::diagnostics::folder(
                &locations.home,
                crate::queue::Storefront::Steam,
            ),
            steam_api,
            home: home.clone(),
            wine,
        },
        cloud,
        target,
    })
}

/// Whether Valve's Windows Steam starts the game that runs in `wine`, rather
/// than Obelisk's Steam host.
pub(crate) fn windows_steam(wine: Option<&crate::wine::Config>) -> bool {
    wine.is_some_and(|wine| wine.settings.steam_client == SteamClient::Windows)
}

/// The installation `receipt` names when Valve's Windows Steam starts its
/// game, as its Launch settings ask.
fn windows_steam_installation(
    locations: &crate::Locations,
    receipt: &Receipt<Steam>,
) -> Result<Option<crate::wine::Installation>, Error> {
    Ok(receipt
        .windows(locations)?
        .filter(|windows| windows.steam_client() == SteamClient::Windows))
}

/// The account Valve's Windows Steam signs in to by itself to start the game
/// `receipt` names; `None` while it would ask, or when Obelisk's Steam
/// client starts the game.
pub fn windows_steam_account(
    locations: &crate::Locations,
    receipt: &Receipt<Steam>,
) -> Result<Option<String>, Error> {
    Ok(windows_steam_installation(locations, receipt)?
        .and_then(|windows| windows_steam::account(windows.prefix())))
}

/// Signs Valve's Windows Steam, which starts the game `receipt` names, in to
/// `account_name`'s account with `refresh_token`, a SteamClient login token
/// Obelisk signed in for it alone. Its prefix is readied first, and Steam
/// installed there when it is missing.
pub async fn sign_in_windows_steam(
    locations: crate::Locations,
    receipt: Receipt<Steam>,
    account_name: String,
    refresh_token: String,
) -> Result<(), Error> {
    let refresh_token = zeroize::Zeroizing::new(refresh_token);
    let readied = locations.clone();
    let config = tokio::task::spawn_blocking(move || {
        let windows =
            windows_steam_installation(&readied, &receipt)?.ok_or(Error::WindowsSteamSignIn)?;
        let storefront = windows_steam::Prefix {
            install: Path::new(&receipt.directory),
            app_id: receipt.product.app_id,
            prefix: windows.prefix(),
        };
        Ok::<_, Error>(windows.ready_prefix(&readied, &storefront)?)
    })
    .await
    .map_err(|_| Error::StartFailed)??;
    windows_steam::sign_in(&config, &locations.home, &account_name, &refresh_token).await
}

/// The Mac game `target` names inside `install`, and its Steam API library.
fn native_target(
    install: &Path,
    target: &obelisk_core::launch::LaunchTarget,
    home: &Path,
    app_id: u32,
) -> Result<(Target, PathBuf), Error> {
    let steam_api = launch::steam_api(target)?;
    let (target, _) = crate::native::target(
        install,
        target,
        target.arguments.clone(),
        crate::native::environment(home),
        crate::diagnostics::game_log(home, crate::diagnostics::Game::Steam(app_id), Platform::Mac),
    )?;
    Ok((target, steam_api))
}

/// Starts the supervisor of `request`'s game, as a new Game session;
/// `cancelled` stops a start in progress.
pub fn start(
    helper: &Path,
    credentials: &Credentials,
    mut request: Request,
    cancelled: &CancelToken,
) -> Result<Client, Error> {
    request.id = uuid::Uuid::new_v4().to_string();
    start_command(
        helper,
        credentials,
        request,
        cancelled,
        Client::supervisor_command(helper),
        supervisor::client::inspect,
    )
}

pub(crate) fn start_command(
    helper: &Path,
    credentials: &Credentials,
    request: Request,
    cancelled: &CancelToken,
    command: std::process::Command,
    probe: supervisor::client::Probe,
) -> Result<Client, Error> {
    let root = request.root.clone();
    let id = request.id.clone();
    let app_id = request.config.app_id;
    let account_id = credentials.steam_id;
    Ok(Client::start_with(
        &root,
        helper,
        id,
        command,
        probe,
        |pipe| {
            credentials
                .write(&mut *pipe)
                .map_err(|_| supervisor::Error::StartFailed)?;
            supervisor::storage::send(pipe, &request)
        },
        |ack| ack.account_id == account_id && ack.store.app_id == app_id,
        || cancelled.is_cancelled(),
    )?)
}

/// What a Steam installation names of its game: the app, and the language
/// whose depots it installs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Product {
    pub app_id: u32,
    pub language: String,
}

impl crate::installations::Storefront for Steam {
    type Product = Product;
    type Join = Join;
    const PLATFORM: obelisk_core::platform::Spelling = steam_kit::client::PLATFORM;
    const RECEIPTS_UNAVAILABLE: &'static str = "model.steam.receiptsUnavailable";
    const RECEIPTS_NOT_SAVED: &'static str = "model.steam.receiptsNotSaved";
    const GAMES_DIRECTORY: &'static str = "Steam";
    const QUEUE: crate::queue::Storefront = crate::queue::Storefront::Steam;
    fn product_game(product: &Product) -> u32 {
        product.app_id
    }
    fn profile_game(product: &Product) -> crate::profile::Game<'_> {
        crate::profile::Game::Steam {
            app_id: product.app_id,
        }
    }
    fn receipt_id(app_id: &u32, platform: Platform) -> String {
        format!("{app_id}-{}", Self::PLATFORM.name(platform))
    }
    fn transfer_name(app_id: &u32) -> String {
        app_id.to_string()
    }
}

impl crate::sessions::Storefront for Steam {
    type Game = u32;
    const BEGINS: bool = false;
    const INVALID_SESSION: &'static str = "backend.steam.runtime.invalidSession";
    const SESSION_FAILED: &'static str = "backend.steam.runtime.startFailed";
    /// Every Steam game runs beside Obelisk's Steam host, whatever its
    /// platform or engine.
    const RUNTIMES: &'static [Runtime] = &[Runtime::SteamComponents];
    fn game(store: &Store) -> u32 {
        store.app_id
    }
}

/// What the player asks of a running Steam game.
impl crate::sessions::Sessions<Steam> {
    /// Obelisk's overlay opened, or closed, over session `id`'s game, which
    /// Steam's overlay would tell it. `user_initiated` when the player opened
    /// it, not the game.
    pub fn overlay_state(
        &self,
        id: &str,
        active: bool,
        user_initiated: bool,
    ) -> Result<(), crate::sessions::Failure> {
        self.control(
            id,
            Control::Overlay {
                active,
                user_initiated,
            },
        )
    }

    /// The player accepted `friend`'s invitation into session `id`'s running
    /// game: the game joins as if Steam's overlay had accepted it.
    pub fn accept_invite(
        &self,
        id: &str,
        friend: u64,
        target: crate::protocol::InviteTarget,
    ) -> Result<(), crate::sessions::Failure> {
        if !crate::protocol::individual(friend) || !target.valid() {
            return Err(crate::sessions::Failure::Failed(
                <Steam as crate::sessions::Storefront>::INVALID_SESSION.to_owned(),
            ));
        }
        self.control(id, Control::AcceptInvite { friend, target })
    }

    /// The player's answer to the purchase `order_id` session `id`'s game
    /// waits on.
    pub fn authorize_microtxn(
        &self,
        id: &str,
        order_id: u64,
        approve: bool,
    ) -> Result<(), crate::sessions::Failure> {
        self.control(id, Control::AuthorizeMicroTxn { order_id, approve })
    }
}

#[cfg(test)]
mod join_tests {
    use super::Join;
    use obelisk_core::Platform;

    #[test]
    fn a_join_adds_what_steams_client_adds_to_the_command_line() {
        let lobby = Join::Lobby {
            lobby_id: 109_775_244_421_126_053,
        };
        assert!(lobby.valid());
        assert_eq!(
            lobby.arguments(Platform::Windows),
            ["+connect_lobby", "109775244421126053"]
        );
        let connect = Join::Connect {
            connect: r#"+connect "10.0.0.1:27015" +password "two words""#.into(),
        };
        for platform in [Platform::Windows, Platform::Mac] {
            assert_eq!(
                connect.arguments(platform),
                ["+connect", "10.0.0.1:27015", "+password", "two words"],
                "{platform:?}"
            );
        }
        // A Mac command line it cannot read splits on spaces.
        let unbalanced = Join::Connect {
            connect: "+connect 'x y".into(),
        };
        assert_eq!(unbalanced.arguments(Platform::Mac), ["+connect", "'x", "y"]);
        for invalid in [
            Join::Lobby {
                lobby_id: 76_561_197_960_265_729,
            },
            Join::Connect {
                connect: String::new(),
            },
            Join::Connect {
                connect: "a".repeat(256),
            },
        ] {
            assert!(!invalid.valid(), "{invalid:?}");
        }
    }
}

#[cfg(test)]
mod error_tests {
    use super::{Error, supervisor};
    use crate::launch::Error as Launch;

    #[test]
    fn a_steam_launch_shows_every_failure_with_its_key() {
        for (failure, key) in [
            (
                Launch::IncompatibleArchitecture,
                "backend.runtime.macBuildNotNative",
            ),
            (
                Launch::InvalidApplication,
                "backend.runtime.invalidApplication",
            ),
            (Launch::NotExecutable, "backend.runtime.notExecutable"),
            (Launch::WineUnavailable, "backend.runtime.wineUnavailable"),
            (Launch::Storage, "backend.steam.runtime.storageFailed"),
            (Launch::StartFailed, "backend.steam.runtime.startFailed"),
            (Launch::Unavailable, "backend.steam.runtime.unavailable"),
        ] {
            assert_eq!(Error::from(failure).to_string(), key, "{failure:?}");
        }
        for (failure, key) in [
            (
                supervisor::Error::Storage,
                "backend.steam.runtime.storageFailed",
            ),
            (
                supervisor::Error::InvalidSession,
                "backend.steam.runtime.invalidSession",
            ),
            (
                supervisor::Error::NewerSession,
                "backend.steam.runtime.sessionBusy",
            ),
            (supervisor::Error::Busy, "backend.steam.runtime.sessionBusy"),
            (
                supervisor::Error::Unavailable,
                "backend.steam.runtime.unavailable",
            ),
            (
                supervisor::Error::Cancelled,
                "backend.steam.runtime.cancelled",
            ),
            (
                supervisor::Error::StartFailed,
                "backend.steam.runtime.startFailed",
            ),
            (
                supervisor::Error::RosettaUnavailable,
                "backend.runtime.rosettaUnavailable",
            ),
        ] {
            assert_eq!(Error::from(failure).to_string(), key, "{failure:?}");
        }
    }
}
