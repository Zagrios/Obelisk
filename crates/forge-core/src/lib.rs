//! Platform-independent library catalog and storefront capability descriptions.
//!
//! This crate never launches a process or reads credentials.
//! Swift owns macOS presentation and native application launching.

mod atomic_file;
pub mod install;
pub mod launch;
mod library;
pub mod storefront;
pub mod transfer;
pub mod verification;
pub mod watch;

pub use atomic_file::{write_atomic, write_atomic_with};
pub use library::{CoreError, Game, GameKind, Library};
