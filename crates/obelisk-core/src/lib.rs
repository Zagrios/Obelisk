//! Platform-independent installation, Transfer and download rules shared by the storefront kits.
//!
//! This crate never launches a process or reads credentials.
//! Swift owns macOS presentation and native application launching.

mod atomic_file;
pub mod build_choice;
pub mod date;
pub mod install;
pub mod installation;
pub mod launch;
pub mod library_cache;
pub mod mac_build;
pub mod platform;
pub mod publish;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod transfer;
pub mod verification;
pub mod watch;

pub use atomic_file::{write_atomic, write_atomic_with};
pub use platform::Platform;
