//! Why a game cannot be readied or started, whatever its storefront: its
//! Mac build's slices, its program, or the Wine that runs it.

/// `IncompatibleArchitecture` and `WineUnavailable` are localization keys every
/// storefront shows as they are. Steam shows `InvalidApplication` and
/// `NotExecutable` as they are too, while Epic and GOG call them unsupported.
/// Each storefront names `Storage`, `StartFailed` and `Unavailable` with its
/// own keys, as it does `supervisor::Error`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// A Mac build without an arm64 slice, installed before Obelisk
    /// installed such a game's Windows build instead.
    #[error("backend.runtime.macBuildNotNative")]
    IncompatibleArchitecture,
    #[error("backend.runtime.invalidApplication")]
    InvalidApplication,
    #[error("backend.runtime.notExecutable")]
    NotExecutable,
    #[error("backend.runtime.wineUnavailable")]
    WineUnavailable,
    #[error("launch storage failed")]
    Storage,
    #[error("launch start failed")]
    StartFailed,
    #[error("launch file unavailable")]
    Unavailable,
}
impl From<std::io::Error> for Error {
    fn from(_: std::io::Error) -> Self {
        Self::Storage
    }
}
