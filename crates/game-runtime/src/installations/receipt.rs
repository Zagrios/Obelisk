//! The Installation receipt, the Transfer that writes it and the Offer a new
//! installation starts from: the same records for every storefront, which
//! adds only its product. Which installation a Transfer goes over is decided
//! here alone.
use super::Storefront;
use obelisk_core::Platform;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// An Installation receipt, as the account's receipt file records it: the
/// storefront's product keys beside its own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", bound = "")]
pub struct Receipt<S: Storefront> {
    #[serde(flatten)]
    pub product: S::Product,
    pub name: String,
    pub directory: String,
    #[serde(serialize_with = "spelled::<S, _>")]
    pub platform: Platform,
    pub total_bytes: u64,
    /// A Transfer started over this installation and has not succeeded since.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub needs_verification: bool,
}

/// One Transfer: a game's build, installed under a parent folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transfer<S: Storefront> {
    pub product: S::Product,
    /// The game's title, as the download queue shows it.
    pub name: String,
    /// None while the storefront picks the build, as the Transfer starts.
    pub platform: Option<Platform>,
    /// The parent folder the installation directory goes in.
    pub directory: PathBuf,
}

/// What the storefront offers to install: one game, whose build it picks as
/// the Transfer starts, before any folder is chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer<S: Storefront> {
    pub product: S::Product,
    pub name: String,
}

/// Writes `platform` as storefront S spells it in its receipt file.
fn spelled<S: Storefront, Z: serde::Serializer>(
    platform: &Platform,
    serializer: Z,
) -> Result<Z::Ok, Z::Error> {
    S::PLATFORM.serialize(*platform, serializer)
}

impl<S: Storefront> Receipt<S> {
    pub fn game(&self) -> S::Game {
        S::product_game(&self.product)
    }

    /// One per game and platform: the installation each action names.
    pub fn id(&self) -> String {
        S::receipt_id(&self.game(), self.platform)
    }

    /// Its game as Wine starts a Windows build: in its folder, with its name
    /// in the store.
    pub(crate) fn wine_game(&self) -> crate::wine::Game<'_> {
        crate::wine::Game {
            install: Path::new(&self.directory),
            title: &self.name,
        }
    }

    /// Its launch settings, by its game's profile, the choices kept for it in
    /// Obelisk's `library` and the player's language.
    pub(crate) fn launch_settings(&self, library: &Path) -> crate::launch_settings::View {
        crate::launch_settings::of(
            library,
            S::profile_game(&self.product),
            Path::new(&self.directory),
            crate::native::player_locale().as_deref(),
        )
    }

    /// Keeps `choices` as its player's launch settings in Obelisk's
    /// `library`, and returns its launch settings from its next launch on.
    pub(crate) fn choose_launch_settings(
        &self,
        library: &Path,
        choices: crate::launch_settings::Choices,
    ) -> std::io::Result<crate::launch_settings::View> {
        crate::launch_settings::choose(
            library,
            S::profile_game(&self.product),
            Path::new(&self.directory),
            crate::native::player_locale().as_deref(),
            choices,
        )
    }

    /// Its Wine prefix and how its game runs there, when it is a Windows
    /// build: its launch settings resolved once, for one launch.
    pub fn windows(
        &self,
        locations: &crate::Locations,
    ) -> Result<Option<crate::wine::Installation>, crate::launch::Error> {
        if self.platform != Platform::Windows {
            return Ok(None);
        }
        let launch_settings = self.launch_settings(&locations.library);
        crate::wine::Installation::new(
            locations,
            S::profile_game(&self.product),
            Path::new(&self.directory),
            launch_settings,
        )
        .map(Some)
    }

    /// `windows`, worked out off the async runtime as a launch reads it:
    /// finding Easy Anti-Cheat may read hundreds of folders. `failed` names a
    /// lost join.
    pub(crate) async fn windows_for_launch(
        &self,
        locations: &crate::Locations,
        failed: impl Into<String>,
    ) -> Result<
        Result<Option<crate::wine::Installation>, crate::launch::Error>,
        crate::sessions::Failure,
    > {
        let (locations, receipt) = (locations.clone(), self.clone());
        super::blocking(failed, move || receipt.windows(&locations)).await
    }

    /// Its Wine prefix when it is a Windows build, the one of the engine it
    /// runs on, without the rest of its launch settings: where its saves
    /// live. None for a Mac build, or one whose folder names no prefix.
    /// Worked out off the async runtime, as `windows_for_launch` does:
    /// without a choice of engine, finding Easy Anti-Cheat reads folders of
    /// the installation, on a disk that may still be spinning up. A lost join
    /// is the storefront's `SESSION_FAILED`.
    pub(crate) async fn wine_prefix(
        &self,
        locations: &crate::Locations,
    ) -> Result<Option<PathBuf>, crate::sessions::Failure> {
        if self.platform != Platform::Windows {
            return Ok(None);
        }
        let (library, prefixes) = (locations.library.clone(), locations.wine_prefixes.clone());
        let (product, directory) = (self.product.clone(), PathBuf::from(&self.directory));
        super::blocking(S::SESSION_FAILED, move || {
            let game = S::profile_game(&product);
            crate::launch_settings::engine(&library, game, &directory)
                .prefix(&prefixes, &directory)
                .ok()
        })
        .await
    }

    /// Follows its installation, which moved from `from`: the Cloud
    /// archive's claims on the saves inside it name their new place.
    pub(crate) fn relocated(
        &self,
        locations: &crate::Locations,
        from: &Path,
    ) -> Result<(), String> {
        cloud_vault::relocate(&locations.cloud_archive, from, Path::new(&self.directory))
            .map_err(|error| error.to_string())
    }

    /// Deletes its Wine prefix once its folder is gone. `None` when it has
    /// none.
    pub(crate) fn remove_prefix(&self, locations: &crate::Locations) -> Option<Result<(), String>> {
        if self.platform != Platform::Windows {
            return None;
        }
        crate::wine::remove(locations, Path::new(&self.directory))
            .map(|removed| removed.map_err(|error| error.to_string()))
    }

    /// The Transfer that verifies its installation, or updates it in place,
    /// with the build it records.
    pub(super) fn transfer(&self) -> Transfer<S> {
        Transfer {
            product: self.product.clone(),
            name: self.name.clone(),
            platform: Some(self.platform),
            directory: parent(&self.directory),
        }
    }
}

impl<S: Storefront> Transfer<S> {
    pub fn game(&self) -> S::Game {
        S::product_game(&self.product)
    }

    /// The name its progress gives the game.
    pub fn progress_name(&self) -> String {
        S::transfer_name(&self.game())
    }

    /// Whether it goes over `receipt`'s installation: its game's of its
    /// platform, or of any platform while the storefront picks the build.
    pub(super) fn lands_on(&self, receipt: &Receipt<S>) -> bool {
        receipt.game() == self.game()
            && self
                .platform
                .is_none_or(|platform| platform == receipt.platform)
    }
}

impl<S: Storefront> Offer<S> {
    /// The Transfer that installs it under `directory`.
    pub(super) fn transfer(self, directory: PathBuf) -> Transfer<S> {
        Transfer {
            product: self.product,
            name: self.name,
            platform: None,
            directory,
        }
    }
}

/// The folder an installation's own folder sits in.
fn parent(directory: &str) -> PathBuf {
    Path::new(directory)
        .parent()
        .map(Path::to_owned)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
