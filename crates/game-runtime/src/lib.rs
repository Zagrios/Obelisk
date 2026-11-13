//! Safe launcher-side supervision. Native ABI code lives in the separate helper.
pub mod cloud;
pub mod cloud_status;
pub mod diagnostics;
mod easy_anti_cheat;
pub mod epic;
mod files;
pub mod games_folder;
pub mod gog;
mod input;
pub mod installations;
pub mod launch;
pub mod launch_settings;
mod launch_shell;
mod locations;
pub mod metal_hud;
mod native;
pub mod notes;
pub mod package;
mod pe;
pub mod profile;
pub mod protocol;
pub mod queue;
mod remote;
pub mod runtimes;
pub mod sessions;
pub mod steam;
pub mod supervisor;
pub mod valve;
pub mod voile;
pub mod wine;
pub use locations::Locations;
use std::time::Duration;

/// Every supervisor entry point drives its async work from one blocking thread.
pub(crate) fn executor() -> std::io::Result<Executor> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map(|runtime| Executor(Some(runtime)))
}

/// Dropping waits at most a second for blocking work, so a pending DNS lookup
/// cannot hold a caller whose async work has already completed.
pub(crate) struct Executor(Option<tokio::runtime::Runtime>);
impl Executor {
    pub(crate) fn block_on<F: std::future::Future>(&self, future: F) -> F::Output {
        self.0
            .as_ref()
            .expect("executor runs until dropped")
            .block_on(future)
    }
}
impl Drop for Executor {
    fn drop(&mut self) {
        if let Some(runtime) = self.0.take() {
            runtime.shutdown_timeout(Duration::from_secs(1));
        }
    }
}

/// Seconds since the Unix epoch.
pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// The pinned engine hash lives only in the runtime manifest; this reads it back.
pub fn client_sha256() -> &'static str {
    &package::entry("steamclient.dylib").sha256
}
pub fn sha(path: &std::path::Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    let mut file = std::io::BufReader::with_capacity(64 * 1024, std::fs::File::open(path)?);
    std::io::copy(&mut file, &mut digest)?;
    Ok(format!("{:x}", digest.finalize()))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;
    /// Parallel fixture processes may retain a dropped flock until exec. Bound
    /// the wait and unlock the probe before the caller reopens the lock file.
    pub(crate) fn wait_for_lock_release(path: &std::path::Path) {
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        if lock.try_lock().is_ok() {
            lock.unlock().unwrap();
            return;
        }
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let result = lock.lock().and_then(|()| lock.unlock());
            let _ = send.send(result);
        });
        receive
            .recv_timeout(Duration::from_secs(2))
            .expect("dropped fixture lock remained held")
            .expect("probing the released fixture lock failed");
    }
}
