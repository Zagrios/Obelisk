#[cfg(not(all(
    target_os = "macos",
    any(target_arch = "aarch64", target_arch = "x86_64")
)))]
compile_error!("The native Steam helpers require 64-bit macOS");

#[cfg(target_arch = "aarch64")]
pub mod abi;
pub mod events;
#[cfg(target_arch = "aarch64")]
pub mod host;
#[cfg(target_arch = "aarch64")]
pub mod mach;
pub mod native;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub use game_runtime::client_sha256;

fn sha(path: &std::path::Path) -> Result<String> {
    Ok(game_runtime::sha(path)?)
}
pub fn pinned_client(path: &std::path::Path) -> Result<()> {
    if sha(path)? != client_sha256() {
        return Err("Unrecognized Steam engine; refusing private ABI calls".into());
    }
    Ok(())
}
