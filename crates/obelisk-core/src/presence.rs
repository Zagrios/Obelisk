//! How the player shows to their friends on a storefront.
use serde::{Deserialize, Serialize};

/// The status the player picks for an account. Each storefront kit says it
/// in its own protocol: Steam's persona states, Epic's presence statuses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnlineStatus {
    #[default]
    Online,
    Away,
    /// Friends see the account offline; it still reads their presence.
    Invisible,
}
