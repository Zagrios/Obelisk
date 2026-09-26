//! SailOver's independent Steam protocol client: authentication, licenses and
//! verified SteamPipe downloads. No .NET runtime or SteamCMD process is used.

pub mod auth;
pub mod client;
pub mod cloud;
pub mod cm;
pub mod content;
pub mod friends;
pub mod install_script;
mod kv;
pub mod language;
pub mod launch;
mod message;
pub mod news;
mod proto;

use sailover_core::storefront::{StorefrontCapabilities, StorefrontDescriptor, StorefrontId};

pub fn descriptor() -> StorefrontDescriptor {
    StorefrontDescriptor {
        id: StorefrontId::Steam,
        display_name: "Steam",
        capabilities: StorefrontCapabilities {
            authentication: true,
            owned_library: true,
            downloads: true,
            local_import: false,
        },
    }
}
