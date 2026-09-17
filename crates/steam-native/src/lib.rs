#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
compile_error!("The pinned native Steam engine requires ARM64 macOS");

pub mod abi;
pub mod host;
pub mod mach;
pub mod native;
pub mod sdk;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub use steam_runtime::client_sha256;

pub fn sha(path: &std::path::Path) -> Result<String> {
    Ok(steam_runtime::sha(path)?)
}
pub fn pinned_client(path: &std::path::Path) -> Result<()> {
    if sha(path)? != client_sha256() {
        return Err("Unrecognized Steam engine; refusing private ABI calls".into());
    }
    Ok(())
}
