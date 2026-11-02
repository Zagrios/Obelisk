//! How the player shows to their friends, the same choice for every
//! storefront that has friends.

/// The status the player picks for a storefront account.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, uniffi::Enum)]
pub enum OnlineStatus {
    #[default]
    Online,
    Away,
    /// Friends see the account offline.
    Invisible,
}

impl From<OnlineStatus> for obelisk_core::OnlineStatus {
    fn from(status: OnlineStatus) -> Self {
        match status {
            OnlineStatus::Online => Self::Online,
            OnlineStatus::Away => Self::Away,
            OnlineStatus::Invisible => Self::Invisible,
        }
    }
}
