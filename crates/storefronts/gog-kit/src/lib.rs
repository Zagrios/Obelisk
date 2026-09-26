//! Reserved for SailOver's GOG integration. No authentication or downloads yet.

use sailover_core::storefront::{StorefrontCapabilities, StorefrontDescriptor, StorefrontId};

pub fn descriptor() -> StorefrontDescriptor {
    StorefrontDescriptor {
        id: StorefrontId::Gog,
        display_name: "GOG",
        capabilities: StorefrontCapabilities::default(),
    }
}
