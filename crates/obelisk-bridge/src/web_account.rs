//! One web storefront Account session across UniFFI: Epic's or GOG's, whose
//! account signs in on its storefront's web page and whose refresh
//! credentials rotate with its calls. Rust signs the account in or restores
//! it, saves each rotation, lists its games and keeps them in the Library
//! cache, and reads its playtimes, achievements and game details; Swift
//! presents them. Games cross as Swift names them: an Epic `namespace:app`
//! pair or a GOG product id. Playtimes cross in seconds. Only refresh
//! credentials cross into the macOS Keychain; the account access token stays
//! inside the Rust session.
use crate::{
    CredentialSave, EpicFriendsSnapshot, EpicGame, GameOffer, GamePlatform, GogGame, LibraryCache,
    OnlineStatus, SessionStorefront,
    credentials::{self, Persistence, Rotating},
    library_cache::{self, GameAchievements, LibraryAchievement, PlaytimeLibrary, PlaytimeUnit},
};
use game_runtime::{epic, gog, sessions::Storefront as _, steam};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    future::Future,
    sync::Arc,
};

/// Why an account call failed. Swift shows each storefront's own text for
/// it; `Service` carries the storefront's localization key or message.
#[derive(Debug, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum WebAccountError {
    #[error("the storefront could not be reached")]
    Network,
    /// The storefront refused the account: it has to sign in again.
    #[error("the storefront refused the account")]
    Unauthorized,
    #[error("the storefront refused the sign-in code")]
    InvalidCode,
    #[error("the storefront's answer could not be read")]
    InvalidResponse,
    #[error("{message}")]
    Service { message: String },
}

/// What the Keychain keeps of an account: who it is, the name its storefront
/// shows, and the refresh token its calls rotate.
#[derive(Clone, uniffi::Record)]
pub struct WebAccountCredentials {
    pub account_id: String,
    pub name: String,
    pub refresh_token: String,
}

/// The account's games, as its storefront's records tell them.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum WebAccountGames {
    Epic { games: Vec<EpicGame> },
    Gog { games: Vec<GogGame> },
}

/// What a game's page adds to its library entry: the build it shows before
/// any download, and its size.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct WebGameDetails {
    /// Unknown when the storefront could not tell.
    pub platform: Option<GamePlatform>,
    /// Whether the storefront offers the account both builds: its player
    /// then chooses between them on the game's page.
    #[uniffi(default = false)]
    pub both_builds: bool,
    /// Unknown when the storefront serves no build to measure.
    pub installation_bytes: Option<u64>,
}

/// What a web storefront's kit supplies to its Account session: Epic's or
/// GOG's.
pub(crate) trait WebStore: Sized + 'static {
    /// The kit's client, signed in.
    type Client: Rotating;
    /// A game as Swift names it.
    type Game: Send + Sync;
    /// What the kit's details request names of a game.
    type Product: Send + Sync;
    /// A game as the kit lists it.
    type Listed: Send;
    /// The game as the library shows it, and as its cache entry keeps it.
    type Shown: Serialize + DeserializeOwned + Send;

    /// Names the storefront's libraries in the Library cache.
    const CACHE: &'static str;
    /// How the storefront counts playtime.
    const PLAYTIME: PlaytimeUnit;
    /// The games whose achievements a library reads at once.
    const ACHIEVEMENT_READS: usize;

    /// The game Swift names `name`.
    fn named(name: &str) -> Option<Self::Game>;
    /// What `offer` names for the details request; none for an offer of
    /// another storefront.
    fn product(offer: GameOffer) -> Option<Self::Product>;
    /// The name the storefront tells the game's playtime by.
    fn playtime_id(game: &Self::Game) -> &str;
    /// The name the storefront reads the game's achievements by.
    fn achievements_id(game: &Self::Game) -> &str;
    /// The game as the library shows it, and everything else the storefront
    /// returned for it.
    fn shown(listed: Self::Listed) -> (Self::Shown, Value);
    /// The name Swift gives the game.
    fn id(game: &Self::Shown) -> String;

    /// Signs in with the code the storefront's web page returned.
    fn sign_in(code: &str) -> impl Future<Output = Result<Self::Client, WebAccountError>> + Send;
    fn restore(
        credentials: WebAccountCredentials,
    ) -> impl Future<Output = Result<Self::Client, WebAccountError>> + Send;
    fn credentials(
        client: &Self::Client,
    ) -> impl Future<Output = Result<WebAccountCredentials, WebAccountError>> + Send;
    fn games(
        client: &Self::Client,
    ) -> impl Future<Output = Result<Vec<Self::Listed>, WebAccountError>> + Send;
    /// How long the account played `games`, in the storefront's unit, by
    /// the name the storefront tells each by. A game never played has no
    /// entry; a storefront that answers every game at once may tell more
    /// than `games`.
    fn playtimes(
        client: &Self::Client,
        games: &[Self::Game],
    ) -> impl Future<Output = Result<BTreeMap<String, u64>, WebAccountError>> + Send;
    /// The achievements the storefront reads by `id`, in `locale`.
    fn achievements(
        client: &Self::Client,
        id: &str,
        locale: &str,
    ) -> impl Future<Output = Result<Vec<LibraryAchievement>, WebAccountError>> + Send;
    fn details(
        client: &Self::Client,
        product: &Self::Product,
        language: &str,
    ) -> impl Future<Output = Result<WebGameDetails, WebAccountError>> + Send;
    fn disconnect(
        client: &Self::Client,
    ) -> impl Future<Output = Result<(), WebAccountError>> + Send;
    /// Stops the session's observers, such as its credentials watch, at
    /// once: Swift has nothing to await.
    fn shutdown_observers(client: &Self::Client);
    /// The account's avatar; none for a storefront that tells none.
    fn avatar_url(_: &Self::Client) -> Option<String> {
        None
    }
    /// An address that opens the storefront's site signed in as the account,
    /// then `redirect`; none for a storefront whose site takes no sign-in
    /// from the app.
    fn web_sign_in_url(
        _: &Self::Client,
        _redirect: &str,
    ) -> impl Future<Output = Result<Option<String>, WebAccountError>> + Send {
        async { Ok(None) }
    }
}

/// One account's session on the web storefront `S`.
pub(crate) struct Session<S: WebStore> {
    /// Shared with the installations and Cloud maintenance of the account.
    pub(crate) client: Arc<S::Client>,
    pub(crate) persistence: Arc<Persistence>,
    /// Where the account's library is kept as it loads.
    library: Option<PlaytimeLibrary<S>>,
}

impl<S: WebStore> Session<S> {
    async fn sign_in(
        code: &str,
        library: Option<Arc<LibraryCache>>,
    ) -> Result<Self, WebAccountError> {
        Self::with(S::sign_in(code).await?, library).await
    }

    async fn restore(
        credentials: WebAccountCredentials,
        library: Option<Arc<LibraryCache>>,
    ) -> Result<Self, WebAccountError> {
        Self::with(S::restore(credentials).await?, library).await
    }

    async fn with(
        client: S::Client,
        library: Option<Arc<LibraryCache>>,
    ) -> Result<Self, WebAccountError> {
        let library = match library {
            Some(library) => Some(library.web(&S::credentials(&client).await?.account_id)),
            None => None,
        };
        Ok(Self {
            client: Arc::new(client),
            persistence: Default::default(),
            library,
        })
    }

    async fn credentials(&self) -> Result<WebAccountCredentials, WebAccountError> {
        S::credentials(&self.client).await
    }

    /// Starts saving the account's credentials through `saver`, once.
    fn persist(&self, saver: Arc<dyn CredentialSave>) {
        credentials::persist(self.client.clone(), self.persistence.clone(), saver);
    }

    async fn games(&self) -> Result<Vec<S::Shown>, WebAccountError> {
        let games = S::games(&self.client).await?;
        Ok(match &self.library {
            Some(library) => library.games(games),
            None => games.into_iter().map(|game| S::shown(game).0).collect(),
        })
    }

    async fn playtimes(&self, games: Vec<String>) -> Result<HashMap<String, u64>, WebAccountError> {
        let (names, games) = named::<S>(games);
        let told = S::playtimes(&self.client, &games).await?;
        if let Some(library) = &self.library {
            library.playtimes(&told);
        }
        Ok(played::<S>(names, &games, &told))
    }

    /// `ids`' achievements in `locale`, by the names the storefront reads
    /// them by: the library's for a game not played since they were read,
    /// unless `fresh`, else the storefront's, a few games at a time, which
    /// the library then keeps. A game the storefront did not answer for is
    /// left out; when none could be told, the first failure.
    async fn read_achievements(
        &self,
        ids: Vec<String>,
        locale: &str,
        fresh: bool,
    ) -> Result<Vec<GameAchievements>, WebAccountError> {
        let client = &self.client;
        library_cache::achievements(
            self.library.as_ref().map(|library| library.achievements()),
            ids,
            locale,
            fresh,
            S::ACHIEVEMENT_READS,
            |id| async move { S::achievements(client, &id, locale).await },
        )
        .await
    }

    async fn achievements(
        &self,
        game: &str,
        locale: &str,
        fresh: bool,
    ) -> Result<Vec<LibraryAchievement>, WebAccountError> {
        let game = S::named(game).ok_or(WebAccountError::InvalidResponse)?;
        let id = S::achievements_id(&game).to_owned();
        let read = self.read_achievements(vec![id], locale, fresh).await?;
        Ok(read
            .into_iter()
            .next()
            .map(|game| game.achievements)
            .unwrap_or_default())
    }

    async fn achievement_shares(
        &self,
        games: Vec<String>,
        locale: &str,
    ) -> Result<HashMap<String, f64>, WebAccountError> {
        let (names, games) = named::<S>(games);
        let read = self
            .read_achievements(achievement_ids::<S>(&games), locale, false)
            .await?;
        let shares: HashMap<String, f64> = library_cache::unlocked_shares(read).collect();
        Ok(names
            .into_iter()
            .zip(&games)
            .filter_map(|(name, game)| Some((name, *shares.get(S::achievements_id(game))?)))
            .collect())
    }

    async fn details(
        &self,
        offer: GameOffer,
        language: &str,
    ) -> Result<WebGameDetails, WebAccountError> {
        let product = S::product(offer).ok_or(WebAccountError::InvalidResponse)?;
        S::details(&self.client, &product, language).await
    }

    fn shutdown_observers(&self) {
        S::shutdown_observers(&self.client);
    }

    fn avatar_url(&self) -> Option<String> {
        S::avatar_url(&self.client)
    }

    async fn web_sign_in_url(&self, redirect: &str) -> Result<Option<String>, WebAccountError> {
        S::web_sign_in_url(&self.client, redirect).await
    }

    async fn disconnect(&self) -> Result<(), WebAccountError> {
        S::disconnect(&self.client).await
    }
}

/// The games Swift names `names`, each beside its name. A name that names no
/// game of the storefront is left out.
fn named<S: WebStore>(names: Vec<String>) -> (Vec<String>, Vec<S::Game>) {
    names
        .into_iter()
        .filter_map(|name| {
            let game = S::named(&name)?;
            Some((name, game))
        })
        .unzip()
}

/// The names the storefront reads the achievements of `games` by, each once:
/// several games may share one, such as an Epic sandbox's.
fn achievement_ids<S: WebStore>(games: &[S::Game]) -> Vec<String> {
    let mut asked = HashSet::new();
    games
        .iter()
        .map(S::achievements_id)
        .filter(|id| asked.insert(*id))
        .map(str::to_owned)
        .collect()
}

/// The playtime of each of `games` that `told` tells, in seconds, by the
/// name Swift gives the game. Every game the storefront tells by one name,
/// such as an Epic app's entries in several sandboxes, counts its time.
fn played<S: WebStore>(
    names: Vec<String>,
    games: &[S::Game],
    told: &BTreeMap<String, u64>,
) -> HashMap<String, u64> {
    names
        .into_iter()
        .zip(games)
        .filter_map(|(name, game)| {
            let playtime = *told.get(S::playtime_id(game))?;
            Some((name, S::PLAYTIME.seconds(playtime)))
        })
        .collect()
}

pub(crate) enum Account {
    Epic(Session<epic::Epic>),
    Gog(Session<gog::Gog>),
}

/// Runs `$body` with the account's session as `$session`, whichever
/// storefront signed it in.
macro_rules! each {
    ($account:expr, $session:ident => $body:expr) => {
        match &$account.account {
            Account::Epic($session) => $body,
            Account::Gog($session) => $body,
        }
    };
}

/// One Epic or GOG account's session, from its sign-in or restoration until
/// it disconnects.
#[derive(uniffi::Object)]
pub struct WebAccountSession {
    pub(crate) account: Account,
}

impl WebAccountSession {
    /// Epic's client; none for a GOG account, which has no friends here.
    fn epic(&self) -> Option<&epic_kit::client::Client> {
        match &self.account {
            Account::Epic(session) => Some(&session.client),
            Account::Gog(_) => None,
        }
    }
}

/// Steam's account signs in through its own session.
fn not_web() -> WebAccountError {
    WebAccountError::Service {
        message: steam::Steam::INVALID_SESSION.into(),
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl WebAccountSession {
    /// Signs in to `storefront`, Epic or GOG, with the code its web page
    /// returned. Keeps every library it loads in `library`.
    #[uniffi::constructor]
    pub async fn sign_in(
        storefront: SessionStorefront,
        authorization_code: String,
        library: Option<Arc<LibraryCache>>,
    ) -> Result<Arc<Self>, WebAccountError> {
        let code = authorization_code.as_str();
        let account = match storefront {
            SessionStorefront::Epic => Account::Epic(Session::sign_in(code, library).await?),
            SessionStorefront::Gog => Account::Gog(Session::sign_in(code, library).await?),
            SessionStorefront::Steam => return Err(not_web()),
        };
        Ok(Arc::new(Self { account }))
    }

    /// Restores the account `credentials` name on `storefront`, Epic or
    /// GOG. Keeps every library it loads in `library`.
    #[uniffi::constructor]
    pub async fn restore(
        storefront: SessionStorefront,
        credentials: WebAccountCredentials,
        library: Option<Arc<LibraryCache>>,
    ) -> Result<Arc<Self>, WebAccountError> {
        let account = match storefront {
            SessionStorefront::Epic => Account::Epic(Session::restore(credentials, library).await?),
            SessionStorefront::Gog => Account::Gog(Session::restore(credentials, library).await?),
            SessionStorefront::Steam => return Err(not_web()),
        };
        Ok(Arc::new(Self { account }))
    }

    pub async fn credentials(&self) -> Result<WebAccountCredentials, WebAccountError> {
        each!(self, session => session.credentials().await)
    }

    /// Saves the account's credentials through `credentials` now and after
    /// every rotation, one save at a time and in order, until the session's
    /// observers stop. Every account call relies on it.
    pub async fn persist(&self, credentials: Arc<dyn CredentialSave>) {
        each!(self, session => session.persist(credentials))
    }

    /// The next revision of the credentials after `after_revision`, or none
    /// once the session's observers stop.
    pub async fn next_credentials_revision(&self, after_revision: u64) -> Option<u64> {
        each!(self, session => session.client.next(after_revision).await)
    }

    /// Stops the credentials watch and Epic's friends at once.
    pub fn shutdown_observers(&self) {
        each!(self, session => session.shutdown_observers())
    }

    /// The account's avatar, as its GOG profile shows it; none for Epic.
    pub fn avatar_url(&self) -> Option<String> {
        each!(self, session => session.avatar_url())
    }

    /// Opens Epic's site signed in as the account, then `redirect_url`, an
    /// Epic page. The address holds a single-use code: never keep or log it.
    /// None for GOG, whose site takes no sign-in from the app.
    pub async fn web_sign_in_url(
        &self,
        redirect_url: String,
    ) -> Result<Option<String>, WebAccountError> {
        each!(self, session => session.web_sign_in_url(&redirect_url).await)
    }

    pub async fn games(&self) -> Result<WebAccountGames, WebAccountError> {
        Ok(match &self.account {
            Account::Epic(session) => WebAccountGames::Epic {
                games: session.games().await?,
            },
            Account::Gog(session) => WebAccountGames::Gog {
                games: session.games().await?,
            },
        })
    }

    /// How long the account played each of `games`, in seconds, by game; a
    /// game never played has no entry. Epic answers every game in one
    /// request, GOG one game at a time.
    pub async fn playtimes(
        &self,
        games: Vec<String>,
    ) -> Result<HashMap<String, u64>, WebAccountError> {
        each!(self, session => session.playtimes(games).await)
    }

    /// The game's achievements in `locale`, such as the game page's: the
    /// library's while the game was not played since they were read, unless
    /// `fresh`, else the storefront's, which the library then keeps.
    pub async fn achievements(
        &self,
        game: String,
        locale: String,
        fresh: bool,
    ) -> Result<Vec<LibraryAchievement>, WebAccountError> {
        each!(self, session => session.achievements(&game, &locale, fresh).await)
    }

    /// The share of each of `games`' achievements the account unlocked, by
    /// game, as `achievements` reads them: none for a game without
    /// achievements or that the storefront did not answer for. It fails only
    /// when no game could be told.
    pub async fn achievement_shares(
        &self,
        games: Vec<String>,
        locale: String,
    ) -> Result<HashMap<String, f64>, WebAccountError> {
        each!(self, session => session.achievement_shares(games, &locale).await)
    }

    /// What the page of the game `offer` names adds to its library entry.
    /// `language` names GOG's depots, such as `fr-FR`.
    pub async fn game_details(
        &self,
        offer: GameOffer,
        language: String,
    ) -> Result<WebGameDetails, WebAccountError> {
        each!(self, session => session.details(offer, &language).await)
    }

    pub async fn disconnect(&self) -> Result<(), WebAccountError> {
        each!(self, session => session.disconnect().await)
    }

    /// Shows an Epic account to its friends as `status` says: from the next
    /// friends connection on, and at once on a live one. Friends are Epic's
    /// alone here: a GOG session ignores their calls.
    pub fn set_status(&self, status: OnlineStatus) {
        if let Some(epic) = self.epic() {
            epic.set_status(status.into());
        }
    }

    pub async fn start_friends(&self) {
        if let Some(epic) = self.epic() {
            epic.start_friends();
        }
    }

    pub async fn reconnect_friends(&self) {
        if let Some(epic) = self.epic() {
            epic.reconnect_friends();
        }
    }

    pub fn stop_friends(&self) {
        if let Some(epic) = self.epic() {
            epic.stop_friends();
        }
    }

    /// An Epic account's friends once they change from `after_revision`;
    /// none for a GOG account.
    pub async fn next_friends(&self, after_revision: u64) -> Option<EpicFriendsSnapshot> {
        Some(self.epic()?.next_friends(after_revision).await.into())
    }
}

#[cfg(test)]
mod tests;
