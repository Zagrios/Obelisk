//! Shared vocabulary for independently implemented storefront kits.
//!
//! Capabilities describe working implementations, not planned integrations.
//! Authentication, remote libraries, and download interfaces belong in their
//! respective kits once an actual integration establishes their requirements.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorefrontId {
    Steam,
    Epic,
    Gog,
    Other,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StorefrontCapabilities {
    pub authentication: bool,
    pub owned_library: bool,
    pub downloads: bool,
    pub local_import: bool,
}

impl StorefrontCapabilities {
    pub fn any_available(self) -> bool {
        self.authentication || self.owned_library || self.downloads || self.local_import
    }
}

#[derive(Clone, Debug)]
pub struct StorefrontDescriptor {
    pub id: StorefrontId,
    pub display_name: &'static str,
    pub capabilities: StorefrontCapabilities,
}
