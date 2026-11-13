//! Private files and folders any part of Obelisk writes.
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(crate) fn private_directory(path: &Path) -> std::io::Result<()> {
    fs::create_dir_all(path)?;
    if !fs::symlink_metadata(path)?.file_type().is_dir() {
        return Err(std::io::ErrorKind::NotADirectory.into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
/// Refuses a path for Cloud storage that is relative, climbs out with `..`,
/// or passes through a symbolic link other than macOS's /var and /tmp.
pub(crate) fn safe_storage_path(path: &Path) -> cloud_vault::Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(cloud_vault::Error::Invalid);
    }
    for parent in path.ancestors() {
        match fs::symlink_metadata(parent) {
            Ok(m)
                if m.file_type().is_symlink()
                    && ![Path::new("/var"), Path::new("/tmp")].contains(&parent) =>
            {
                return Err(cloud_vault::Error::Invalid);
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
/// Writes `bytes` to a file named `name` in a new folder inside `parent`
/// that only this user opens, whose name starts with `prefix`. The folder
/// lives as long as the returned TempDir.
pub(crate) fn private_temp_file(
    parent: &Path,
    prefix: &str,
    name: &str,
    bytes: &[u8],
) -> std::io::Result<(tempfile::TempDir, PathBuf)> {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::Builder::new()
        .prefix(prefix)
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in(parent)?;
    let path = directory.path().join(name);
    obelisk_core::write_atomic(&path, bytes)?;
    Ok((directory, path))
}
