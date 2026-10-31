//! What the tests of a storefront's installed files look at on disk: modes,
//! the files an installation keeps, and a folder swapped for a link behind a
//! download's back.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use crate::{install::PARTIAL_DIRECTORY, transfer::DownloadProgress};

/// The permission bits of the file at `path`.
pub fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

pub fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// Writes `bytes` at `relative` under `root` with `mode`, making its folders.
pub fn place(root: &Path, relative: &str, bytes: &[u8], mode: u32) -> PathBuf {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, bytes).unwrap();
    set_mode(&path, mode);
    path
}

/// The record a finished Transfer leaves for the next one.
pub fn record(root: &Path) -> PathBuf {
    root.join(PARTIAL_DIRECTORY).join("verified.json")
}

/// Every partial file the installation still keeps, finished or not.
pub fn partial_files(root: &Path) -> Vec<PathBuf> {
    fn walk(directory: &Path, found: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(directory) else {
            return;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, found);
            } else if path.to_string_lossy().contains(".part") {
                found.push(path);
            }
        }
    }
    let mut found = Vec::new();
    walk(&root.join(PARTIAL_DIRECTORY), &mut found);
    found
}

/// A progress callback that replaces the folder `directory` with a link to
/// `outside` once `total` bytes are assembled: before the download publishes
/// the file there.
pub fn swap_for_a_link_when_assembled(
    directory: PathBuf,
    outside: PathBuf,
    total: u64,
) -> impl Fn(DownloadProgress) + Send + Sync + 'static {
    let swapped = AtomicBool::new(false);
    move |progress| {
        if progress.completed_bytes >= total && !swapped.swap(true, Ordering::SeqCst) {
            fs::remove_dir(&directory).unwrap();
            std::os::unix::fs::symlink(&outside, &directory).unwrap();
        }
    }
}
