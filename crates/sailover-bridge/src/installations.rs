//! One account's installations across UniFFI, for either storefront. Rust
//! keeps the Installation receipts, runs the Transfers, launches, uninstalls
//! and Cloud maintenance, and holds the account's Cloud question; Swift
//! presents the state and forwards the player's actions. Games cross as Swift
//! names them: a Steam app id, or an Epic `namespace:app` pair; installations
//! as their receipt's id.
use crate::{
    CloudChoice, CloudIndicator, CloudStatus, EpicSession, GameSessionError, GameTransfer,
    SailOverLocations, SteamCredentials, SteamSession, StorefrontSessions,
    game_session::Storefronts,
};
use game_runtime::{
    Locations, epic,
    installations::{self, Installations, Report},
    sessions::{Failure, Sessions, Storefront as _},
    steam,
};
use std::sync::Arc;

/// One installation of the account, as its receipt records it.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Installation {
    /// Names the installation in every action.
    pub id: String,
    pub game: String,
    pub name: String,
    pub directory: String,
    pub platform: String,
    pub total_bytes: u64,
    /// A Transfer started over it and has not succeeded since.
    pub needs_verification: bool,
}

/// What a storefront offers to install of one game.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GameOffer {
    /// Its build for `platform`, with the depots of `language`.
    Steam {
        app_id: u32,
        name: String,
        platform: String,
        language: String,
    },
    /// Its build for `platform`.
    Epic {
        namespace: String,
        catalog_item_id: String,
        app_name: String,
        title: String,
        platform: String,
    },
}

/// A failure the player sees, and what it is about.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum InstallationReport {
    /// A storefront's localization key.
    Storefront { key: String },
    /// The chosen folder could not be created.
    DownloadDirectory { message: String },
    /// An uninstall failed for this reason, a localization key.
    Uninstall { reason: String },
}

/// What may start now, of one kind of action.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GameOpening {
    Closed,
    /// It may start, for any game but these.
    Open {
        except: Vec<String>,
    },
}

/// What the Reservation lets start now.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct InstallationGate {
    /// A Transfer or uninstall.
    pub modify: GameOpening,
    /// A launch, of any game a Transfer may touch.
    pub launch: bool,
    pub maintain: GameOpening,
}

/// A storage warning, a localization key; `id` changes each time one is raised.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct InstallationWarning {
    pub id: u64,
    pub key: String,
}

/// Cloud maintenance of one game: its sync's progress and the Cloud question
/// it waits on.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CloudMaintenance {
    pub game: String,
    pub cloud: CloudStatus,
}

/// A game's Cloud indicator.
#[derive(Clone, Debug, uniffi::Record)]
pub struct GameCloudIndicator {
    pub game: String,
    pub indicator: CloudIndicator,
}

/// Everything the account's installations present.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct InstallationsState {
    /// Grows with every change.
    pub revision: u64,
    pub installations: Vec<Installation>,
    pub transfer: Option<GameTransfer>,
    /// The game the last Transfer progress belongs to.
    pub transfer_game: Option<String>,
    pub downloading: bool,
    pub launching: bool,
    pub verifying_before_launch: bool,
    pub launch_game: Option<String>,
    pub uninstalling: bool,
    pub uninstall_game: Option<String>,
    /// The game whose Transfer stopped before its receipt: the player can
    /// resume it.
    pub resumable: Option<String>,
    pub report: Option<InstallationReport>,
    pub warning: Option<InstallationWarning>,
    pub maintenance: Option<CloudMaintenance>,
    pub gate: InstallationGate,
}

/// How a storefront's games, receipts and offers cross UniFFI.
trait Named: installations::Storefront {
    fn name(game: &Self::Game) -> String;
    /// The game Swift names `name`.
    fn named(name: &str) -> Option<Self::Game>;
    fn offer(offer: GameOffer) -> Option<Self::Offer>;
    fn installation(receipt: Self::Receipt) -> Installation;
}

impl Named for steam::Steam {
    fn name(game: &u32) -> String {
        game.to_string()
    }
    fn named(name: &str) -> Option<u32> {
        name.parse().ok()
    }
    fn offer(offer: GameOffer) -> Option<steam::Offer> {
        match offer {
            GameOffer::Steam {
                app_id,
                name,
                platform,
                language,
            } => Some(steam::Offer {
                app_id,
                name,
                platform,
                language,
            }),
            GameOffer::Epic { .. } => None,
        }
    }
    fn installation(receipt: steam::Receipt) -> Installation {
        Installation {
            id: <Self as installations::Storefront>::receipt_id(&receipt),
            game: receipt.app_id.to_string(),
            name: receipt.name,
            directory: receipt.directory,
            platform: receipt.platform,
            total_bytes: receipt.total_bytes,
            needs_verification: receipt.needs_verification == Some(true),
        }
    }
}

impl Named for epic::Epic {
    fn name(game: &epic::Store) -> String {
        format!("{}:{}", game.namespace, game.app_name)
    }
    /// An Epic namespace has no colon.
    fn named(name: &str) -> Option<epic::Store> {
        let (namespace, app_name) = name.split_once(':')?;
        Some(epic::Store {
            namespace: namespace.into(),
            app_name: app_name.into(),
        })
    }
    fn offer(offer: GameOffer) -> Option<epic::Offer> {
        match offer {
            GameOffer::Epic {
                namespace,
                catalog_item_id,
                app_name,
                title,
                platform,
            } => Some(epic::Offer {
                namespace,
                catalog_item_id,
                app_name,
                title,
                platform,
            }),
            GameOffer::Steam { .. } => None,
        }
    }
    fn installation(receipt: epic::Receipt) -> Installation {
        Installation {
            id: <Self as installations::Storefront>::receipt_id(&receipt),
            game: Self::name(&<Self as installations::Storefront>::receipt_game(&receipt)),
            name: receipt.name,
            directory: receipt.directory,
            platform: receipt.platform,
            total_bytes: receipt.total_bytes,
            needs_verification: receipt.needs_verification == Some(true),
        }
    }
}

impl From<Report> for InstallationReport {
    fn from(report: Report) -> Self {
        match report {
            Report::Storefront(key) => Self::Storefront { key },
            Report::DownloadDirectory(message) => Self::DownloadDirectory { message },
            Report::Uninstall(reason) => Self::Uninstall { reason },
        }
    }
}

fn opening<S: Named>(opening: installations::Opening<S::Game>) -> GameOpening {
    match opening {
        installations::Opening::Closed => GameOpening::Closed,
        installations::Opening::Open { except } => GameOpening::Open {
            except: except.iter().map(S::name).collect(),
        },
    }
}

fn state<S: Named>(state: installations::State<S>) -> InstallationsState {
    let name = |game: Option<S::Game>| game.as_ref().map(S::name);
    InstallationsState {
        revision: state.revision,
        installations: state.receipts.into_iter().map(S::installation).collect(),
        transfer: state.transfer.map(Into::into),
        transfer_game: name(state.transfer_game),
        downloading: state.downloading,
        launching: state.launching,
        verifying_before_launch: state.verifying_before_launch,
        launch_game: name(state.launch_game),
        uninstalling: state.uninstalling,
        uninstall_game: name(state.uninstall_game),
        resumable: name(state.last_download.as_ref().map(S::request_game)),
        report: state.report.map(Into::into),
        warning: state
            .warning
            .map(|(id, key)| InstallationWarning { id, key }),
        maintenance: state.maintenance.map(|maintenance| CloudMaintenance {
            game: S::name(&maintenance.game),
            cloud: maintenance.cloud.into(),
        }),
        gate: InstallationGate {
            modify: opening::<S>(state.gate.modify),
            launch: state.gate.launch,
            maintain: opening::<S>(state.gate.maintain),
        },
    }
}

/// A blocking call off the async runtime: an answer to a Game session's
/// question reaches its supervisor, an indicator read reads journals.
async fn call<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, Failure> + Send + 'static,
) -> Result<T, GameSessionError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| GameSessionError::new("backend.steam.runtime.startFailed"))?
        .map_err(Into::into)
}

async fn answer<S: Named>(
    installations: Installations<S>,
    game: String,
    decision_id: u64,
    choice: CloudChoice,
) -> Result<(), GameSessionError> {
    let game = S::named(&game).ok_or_else(|| GameSessionError::new(S::INVALID_SESSION))?;
    call(move || installations.answer_cloud(&game, decision_id, choice.into())).await
}

async fn indicators<S: Named>(
    installations: Installations<S>,
    games: Vec<String>,
) -> Result<Vec<GameCloudIndicator>, GameSessionError> {
    let games: Vec<_> = games.iter().filter_map(|game| S::named(game)).collect();
    let indicators = call(move || installations.cloud_indicators(&games)).await?;
    Ok(indicators
        .into_iter()
        .map(|(game, indicator)| GameCloudIndicator {
            game: S::name(&game),
            indicator: indicator.into(),
        })
        .collect())
}

fn account<S: installations::Storefront>(
    content: impl installations::Content<S> + 'static,
    sessions: Arc<Sessions<S>>,
    account: S::Account,
    receipts: String,
    locations: &Locations,
) -> Installations<S> {
    Installations::new(
        Arc::new(content),
        sessions,
        account,
        receipts.into(),
        locations.library.clone(),
        locations.home.clone(),
    )
}

enum Account {
    Steam(Installations<steam::Steam>),
    Epic(Installations<epic::Epic>),
}

/// Runs `$body` with the account's installations as `$installations`,
/// whichever storefront keeps them.
macro_rules! each {
    ($account:expr, $installations:ident => $body:expr) => {
        match &$account.account {
            Account::Steam($installations) => $body,
            Account::Epic($installations) => $body,
        }
    };
}

/// One signed-in account's installed games and the actions on them.
#[derive(uniffi::Object)]
pub struct AccountInstallations {
    account: Account,
}

#[uniffi::export(async_runtime = "tokio")]
impl AccountInstallations {
    /// The installations of the Steam account `session` signed in, whose
    /// receipts `receipts` keeps.
    #[uniffi::constructor]
    pub fn steam(
        session: Arc<SteamSession>,
        credentials: SteamCredentials,
        sessions: Arc<StorefrontSessions>,
        receipts: String,
        locations: SailOverLocations,
    ) -> Result<Arc<Self>, GameSessionError> {
        let Storefronts::Steam(sessions) = &*sessions.sessions else {
            return Err(GameSessionError::new(steam::Steam::INVALID_SESSION));
        };
        let locations: Locations = (&locations).into();
        let steam_id = credentials.steam_id;
        let content = steam::content::Content::new(
            session.client.clone(),
            game_runtime::protocol::Credentials {
                steam_id,
                account_name: credentials.account_name,
                refresh_token: credentials.refresh_token,
            },
            locations.clone(),
        );
        let installations = account(content, sessions.clone(), steam_id, receipts, &locations);
        Ok(Arc::new(Self {
            account: Account::Steam(installations),
        }))
    }

    /// The installations of the Epic account `account_id`, which `session`
    /// signed in. `language` names the game's language at launch. The
    /// session's own persistence saves the credentials their calls rotate.
    #[uniffi::constructor]
    pub fn epic(
        session: Arc<EpicSession>,
        account_id: String,
        language: String,
        sessions: Arc<StorefrontSessions>,
        receipts: String,
        locations: SailOverLocations,
    ) -> Result<Arc<Self>, GameSessionError> {
        let Storefronts::Epic(sessions) = &*sessions.sessions else {
            return Err(GameSessionError::new(epic::Epic::INVALID_SESSION));
        };
        let locations: Locations = (&locations).into();
        // Every account call waits until the credentials it rotated are saved.
        let (persistence, client) = (session.persistence.clone(), session.client.clone());
        let saved = Arc::new(move || persistence.wait(|| client.credentials_revision()));
        let content = epic::content::Content::new(
            session.client.clone(),
            saved,
            account_id.clone(),
            language,
            locations.clone(),
        );
        let installations = account(content, sessions.clone(), account_id, receipts, &locations);
        Ok(Arc::new(Self {
            account: Account::Epic(installations),
        }))
    }

    pub fn state(&self) -> InstallationsState {
        each!(self, installations => state(installations.state()))
    }

    /// Waits for a revision newer than `after`, or returns at once.
    pub async fn changed(&self, after: u64) -> InstallationsState {
        each!(self, installations => state(installations.changed(after).await))
    }

    /// A DEBUG diagnostic holds the storefront while it runs.
    pub fn set_diagnostic(&self, running: bool) {
        each!(self, installations => installations.set_diagnostic(running))
    }

    /// Installs what `offer` names, or updates the installation of its
    /// platform; a Transfer of the game that stopped resumes instead. An
    /// offer of the other storefront does nothing.
    pub fn install(&self, offer: GameOffer) {
        match &self.account {
            Account::Steam(installations) => {
                if let Some(offer) = <steam::Steam as Named>::offer(offer) {
                    installations.install(offer);
                }
            }
            Account::Epic(installations) => {
                if let Some(offer) = <epic::Epic as Named>::offer(offer) {
                    installations.install(offer);
                }
            }
        }
    }

    /// Updates `installation` in place; a Transfer of the game that stopped
    /// resumes instead.
    pub fn update(&self, installation: String) {
        each!(self, installations => installations.update(&installation))
    }

    /// Stops the Transfer, and a launch still preparing.
    pub fn cancel(&self) {
        each!(self, installations => installations.cancel())
    }

    /// Launches `installation`, verifying it first when `verify` asks for it
    /// or it needs it.
    pub fn launch(&self, installation: String, verify: bool) {
        each!(self, installations => installations.launch(&installation, verify))
    }

    pub fn uninstall(&self, installation: String) {
        each!(self, installations => installations.uninstall(&installation))
    }

    /// Stops the account's Transfer, Cloud maintenance and launch still
    /// preparing.
    pub fn signed_out(&self) {
        each!(self, installations => installations.signed_out())
    }

    /// Syncs the game's Cloud saves: those of `installation`, or, when
    /// `None`, those the uninstalled game's saved context names.
    pub fn maintain_cloud(&self, game: String, installation: Option<String>) {
        match &self.account {
            Account::Steam(installations) => {
                if let Some(game) = <steam::Steam as Named>::named(&game) {
                    installations.maintain_cloud(game, installation.as_deref());
                }
            }
            Account::Epic(installations) => {
                if let Some(game) = <epic::Epic as Named>::named(&game) {
                    installations.maintain_cloud(game, installation.as_deref());
                }
            }
        }
    }

    /// Answers the Cloud question the game shows with `decision_id`, its
    /// maintenance's or its Game session's.
    pub async fn answer_cloud(
        &self,
        game: String,
        decision_id: u64,
        choice: CloudChoice,
    ) -> Result<(), GameSessionError> {
        each!(self, installations => answer(installations.clone(), game, decision_id, choice).await)
    }

    /// The Cloud indicators of `games`, and of every other game of the
    /// account with pending work, which Swift does not know about.
    pub async fn cloud_indicators(
        &self,
        games: Vec<String>,
    ) -> Result<Vec<GameCloudIndicator>, GameSessionError> {
        each!(self, installations => indicators(installations.clone(), games).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn games_cross_as_swift_names_them_and_back() {
        let store = epic::Store {
            namespace: "sandbox".into(),
            app_name: "game:with:colons".into(),
        };
        let name = <epic::Epic as Named>::name(&store);
        assert_eq!(name, "sandbox:game:with:colons");
        assert_eq!(<epic::Epic as Named>::named(&name), Some(store));
        assert_eq!(<epic::Epic as Named>::named("no-colon"), None);
        assert_eq!(<steam::Steam as Named>::named("620"), Some(620));
        assert_eq!(<steam::Steam as Named>::named("sandbox:game"), None);
    }

    #[test]
    fn an_installation_crosses_under_the_id_its_actions_name() {
        let receipt = epic::Receipt {
            namespace: "sandbox".into(),
            catalog_item_id: "catalog".into(),
            app_name: "game".into(),
            name: "Fixture".into(),
            directory: "/library/Games/Epic/epic-hash-Windows".into(),
            platform: "Windows".into(),
            total_bytes: 7,
            needs_verification: Some(true),
        };
        let installation = <epic::Epic as Named>::installation(receipt.clone());
        assert_eq!(
            installation.id,
            <epic::Epic as installations::Storefront>::receipt_id(&receipt)
        );
        assert_eq!(installation.game, "sandbox:game");
        assert!(installation.needs_verification);
        let offer = GameOffer::Steam {
            app_id: 620,
            name: "Portal 2".into(),
            platform: "macos".into(),
            language: "french".into(),
        };
        assert_eq!(
            <epic::Epic as Named>::offer(offer.clone()),
            None,
            "an offer of the other storefront is refused"
        );
        assert_eq!(
            <steam::Steam as Named>::offer(offer).map(|offer| offer.app_id),
            Some(620)
        );
    }
}
