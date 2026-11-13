//! Which of a game's builds a download installs, and which one its page
//! shows before any download. Only a download reads a Mac build's files, as
//! its Transfer starts, once per build; the page shows the Mac build until
//! one found that its game does not run natively. Each storefront answers
//! for its builds through a [`GameBuilds`] adapter: it says what a Mac build
//! lists and what a launch of it starts. Whether that runs natively, and what
//! a build its storefront refuses or fails to give does to the pick, is told
//! here.
use std::{
    collections::HashMap,
    future::Future,
    hash::Hash,
    sync::{Mutex, MutexGuard, PoisonError},
};

use crate::{
    Platform,
    mac_build::{self, Listing},
    transfer::{CancelToken, Cancelled},
};

#[cfg(test)]
mod tests;

/// The message key of a Mac-only game that does not run natively, the same
/// on every storefront.
pub const NOT_NATIVE: &str = "backend.install.macBuildNotNative";

/// The message key of a Mac build its player chose that the pick refuses,
/// the same on every storefront.
pub const MAC_REFUSED: &str = "backend.install.macBuildUnavailable";

/// The builds a storefront offers the account of one game, as it lists them.
pub struct Offered<B> {
    pub mac: Option<B>,
    pub windows: Option<B>,
}

impl<B> Offered<B> {
    /// Whether the storefront offers both builds: the player then chooses
    /// between them on the game's page.
    pub fn both(&self) -> bool {
        self.mac.is_some() && self.windows.is_some()
    }
}

/// What a launch of a Mac build starts, as its storefront names it.
#[derive(Clone, Copy, Debug)]
pub struct Launch<'a> {
    /// The program or the application bundle, a path from the installation's
    /// root.
    pub executable: &'a str,
    /// The names a library the program loads may have. The first file so
    /// named in the folder a launch searches must run natively too; empty
    /// when the storefront pairs the program with none.
    pub libraries: &'a [&'a str],
}

/// Whether a Mac build's game runs natively on Apple silicon, as its files
/// told.
#[derive(Debug)]
#[must_use]
pub struct Verdict(bool);

impl Verdict {
    /// Judges the build whose files are in `listing` and whose launch starts
    /// `launch`. `read` gives a file's first bytes, never more than its size;
    /// only the few files that tell are read.
    ///
    /// # Errors
    ///
    /// Whatever `read` fails with.
    pub async fn of<F: Copy, E, Read>(
        listing: &Listing<F>,
        launch: Launch<'_>,
        read: impl Fn(F, u64) -> Read,
    ) -> Result<Self, E>
    where
        Read: Future<Output = Result<Vec<u8>, E>>,
    {
        mac_build::runs_natively(listing, launch.executable, launch.libraries, read)
            .await
            .map(Self)
    }

    /// With what the build's download starts from, which `kept` makes only
    /// for a game that runs natively.
    pub fn keeping<V>(self, kept: impl FnOnce() -> V) -> Inspection<V> {
        Inspection(self.0.then(kept))
    }
}

/// What reading a Mac build's files told: whether its game runs natively,
/// and then what the build's download starts from.
#[derive(Debug)]
#[must_use]
pub struct Inspection<V>(Option<V>);

impl<V> Inspection<V> {
    /// Of a build that names no program for a launch to start: its game does
    /// not run natively.
    pub fn no_program() -> Self {
        Self(None)
    }

    /// What the build's download starts from; none when its game does not
    /// run natively. For a storefront's tests of its own `inspect`.
    #[cfg(any(test, feature = "testing"))]
    pub fn into_kept(self) -> Option<V> {
        self.0
    }
}

/// Why a game has no build a download can install. Storefront errors
/// convert it, as they convert `Cancelled`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// The storefront offers the account neither build.
    NoBuild,
    /// Only a Mac build, whose game does not run natively.
    NotNative,
    /// The Mac build its player chose, which the pick does not take: its
    /// game does not run natively, or its storefront refuses it.
    MacRefused,
}

/// One game's builds at one storefront, as the build choice asks for them.
pub trait GameBuilds: Sync {
    /// What the storefront lists of a build.
    type Build: Send + Sync;
    /// Names one immutable Mac build; its verdict is kept by it.
    type Key: Clone + Eq + Hash + Send;
    /// What the download of a native Mac build starts from.
    type Kept: Send;
    type Error: From<Unavailable> + From<Cancelled> + Send;
    /// Why reading a Mac build stopped: the storefront's error, with where
    /// it arose when [`is_refusal`](Self::is_refusal) needs that told.
    type Failure: Into<Self::Error> + Send;

    /// The builds the account can install. Reads no build's files.
    fn builds(&self) -> impl Future<Output = Result<Offered<Self::Build>, Self::Error>> + Send;

    /// The key of the Mac build `mac`; none when the storefront names no
    /// immutable build, which keeps no verdict.
    fn key(
        &self,
        mac: &Self::Build,
    ) -> impl Future<Output = Result<Option<Self::Key>, Self::Error>> + Send;

    /// Reads what `mac` lists, has [`Verdict::of`] judge what a launch of it
    /// starts, and keeps what its download starts from.
    fn inspect(
        &self,
        mac: &Self::Build,
        cancel: &CancelToken,
    ) -> impl Future<Output = Result<Inspection<Self::Kept>, Self::Failure>> + Send;

    /// Whether `failure` is a refusal, which asking again does not change:
    /// the storefront denies the build, or it holds what Obelisk cannot
    /// install. A refused Mac build gives way to the Windows build; any other
    /// failure may pass, and fails the pick.
    fn is_refusal(failure: &Self::Failure) -> bool;
}

/// One storefront client's build choice for the session: each Mac build's
/// verdict, what the last pick read of a native build, and the pick a
/// cancellation stops.
pub struct BuildChoice<K, V> {
    verdicts: Mutex<HashMap<K, bool>>,
    kept: Mutex<Option<(K, V)>>,
    picking: Mutex<Option<CancelToken>>,
}

impl<K, V> Default for BuildChoice<K, V> {
    fn default() -> Self {
        Self {
            verdicts: Mutex::new(HashMap::new()),
            kept: Mutex::new(None),
            picking: Mutex::new(None),
        }
    }
}

impl<K: Clone + Eq + Hash, V> BuildChoice<K, V> {
    /// The build a download installs. `work` runs the choice with
    /// [`Pick::of`], inside whatever the storefront wraps around its calls,
    /// such as its token funnel; [`cancel`](Self::cancel) stops all of it.
    pub async fn pick<E: From<Cancelled>>(
        &self,
        work: impl for<'a> AsyncFnOnce(Pick<'a, K, V>) -> Result<Platform, E>,
    ) -> Result<Platform, E> {
        let token = CancelToken::new();
        *self.lock_picking() = Some(token.clone());
        let picked = token
            .race(work(Pick {
                choice: self,
                cancel: token.clone(),
            }))
            .await;
        self.lock_picking().take();
        picked?
    }

    /// The build a game's page shows, of those `offered`, with what the
    /// storefront listed of it: the Mac build, unless a pick found that its
    /// game does not run natively and there is a Windows build. Reads no
    /// build's files; a key the storefront cannot tell shows the Mac build.
    pub async fn shown<G: GameBuilds<Key = K>>(
        &self,
        game: &G,
        offered: Offered<G::Build>,
    ) -> Option<(Platform, G::Build)> {
        // Only with both builds can a verdict change the answer, and a key
        // may cost the storefront a request.
        let not_native = match (&offered.mac, &offered.windows) {
            (Some(mac), Some(_)) => {
                matches!(game.key(mac).await, Ok(Some(key)) if self.verdict(&key) == Some(false))
            }
            _ => false,
        };
        match (offered.mac, offered.windows) {
            (Some(_), Some(windows)) if not_native => Some((Platform::Windows, windows)),
            (Some(mac), _) => Some((Platform::Mac, mac)),
            (None, Some(windows)) => Some((Platform::Windows, windows)),
            (None, None) => None,
        }
    }

    /// What the last pick read of `build`, once.
    pub fn take(&self, build: &K) -> Option<V> {
        let mut kept = self.lock_kept();
        match kept.as_ref() {
            Some((key, _)) if key == build => kept.take().map(|(_, value)| value),
            _ => None,
        }
    }

    /// Stops a pick in flight and forgets what the last one read.
    pub fn cancel(&self) {
        if let Some(token) = self.lock_picking().as_ref() {
            token.cancel();
        }
        self.lock_kept().take();
    }

    fn verdict(&self, build: &K) -> Option<bool> {
        self.lock_verdicts().get(build).copied()
    }

    fn lock_verdicts(&self) -> MutexGuard<'_, HashMap<K, bool>> {
        self.verdicts.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_kept(&self) -> MutexGuard<'_, Option<(K, V)>> {
        self.kept.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_picking(&self) -> MutexGuard<'_, Option<CancelToken>> {
        self.picking.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A pick in flight, which its cancellation stops.
pub struct Pick<'a, K, V> {
    choice: &'a BuildChoice<K, V>,
    cancel: CancelToken,
}

impl<K: Clone + Eq + Hash, V> Pick<'_, K, V> {
    /// The Mac build when its game runs natively, else the Windows build.
    /// A Mac build is read once per key.
    pub async fn of<G: GameBuilds<Key = K, Kept = V>>(
        &self,
        game: &G,
    ) -> Result<Platform, G::Error> {
        let offered = game.builds().await?;
        if let Some(mac) = &offered.mac
            && self.runs_natively(game, mac).await?
        {
            return Ok(Platform::Mac);
        }
        match (offered.mac, offered.windows) {
            (_, Some(_)) => Ok(Platform::Windows),
            (Some(_), None) => Err(Unavailable::NotNative.into()),
            (None, None) => Err(Unavailable::NoBuild.into()),
        }
    }

    /// The build its player chose, else the pick of [`of`](Self::of). The
    /// Windows build is taken without reading the Mac build. The Mac build
    /// is read as `of` reads it, and never gives way to the Windows build:
    /// one the pick would not take is refused.
    pub async fn of_choice<G: GameBuilds<Key = K, Kept = V>>(
        &self,
        game: &G,
        chosen: Option<Platform>,
    ) -> Result<Platform, G::Error> {
        let Some(chosen) = chosen else {
            return self.of(game).await;
        };
        let offered = game.builds().await?;
        match (chosen, offered.mac, offered.windows) {
            (Platform::Windows, _, Some(_)) => Ok(Platform::Windows),
            (Platform::Mac, Some(mac), _) if self.runs_natively(game, &mac).await? => {
                Ok(Platform::Mac)
            }
            (Platform::Mac, Some(_), _) => Err(Unavailable::MacRefused.into()),
            _ => Err(Unavailable::NoBuild.into()),
        }
    }

    /// Whether the game of `mac` runs natively, from its verdict when one is
    /// kept, else from its files. A build its storefront refuses does not,
    /// and keeps that verdict: it gives way to the Windows build. Any other
    /// failure fails the pick and keeps no verdict, since falling back on it
    /// could install the Windows build of a game that runs natively. What a
    /// native build's read is kept for its download, in place of any other
    /// build's.
    async fn runs_natively<G: GameBuilds<Key = K, Kept = V>>(
        &self,
        game: &G,
        mac: &G::Build,
    ) -> Result<bool, G::Error> {
        let key = game.key(mac).await?;
        if let Some(native) = key.as_ref().and_then(|key| self.choice.verdict(key)) {
            return Ok(native);
        }
        let kept = match game.inspect(mac, &self.cancel).await {
            Ok(Inspection(kept)) => kept,
            Err(failure) if G::is_refusal(&failure) => {
                // A pick being stopped ends cancelled, whatever its
                // storefront answered meanwhile.
                self.cancel.check()?;
                None
            }
            Err(failure) => return Err(failure.into()),
        };
        let native = kept.is_some();
        if let Some(key) = key {
            self.choice.lock_verdicts().insert(key.clone(), native);
            *self.choice.lock_kept() = kept.map(|kept| (key, kept));
        }
        Ok(native)
    }
}
