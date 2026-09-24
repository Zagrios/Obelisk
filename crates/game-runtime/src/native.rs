//! A native game's executable inside its installation, and the environment
//! every game starts with, whatever its storefront.
use crate::{Error, supervisor::architecture::Architecture};
use forge_core::launch::LaunchTarget;
use std::{
    collections::HashMap,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

/// The executable `target` names inside `root`: an application bundle's
/// declared executable, or the file itself. It must be a Mach-O Forge can run.
/// Installers may drop its execute permission; only then is it repaired.
pub fn executable(root: &Path, target: &LaunchTarget) -> Result<(PathBuf, Architecture), Error> {
    let root = root.canonicalize()?;
    let named = Path::new(&target.executable)
        .canonicalize()
        .map_err(|_| Error::NotExecutable)?;
    let executable = if target.is_application_bundle {
        bundle_executable(&named)?
    } else {
        named
    };
    if !executable.starts_with(&root) || !executable.is_file() {
        return Err(Error::NotExecutable);
    }
    let architecture = Architecture::preferred(&executable)?;
    let mode = std::fs::metadata(&executable)?.permissions().mode();
    if mode & 0o100 == 0 {
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(mode | 0o100))?;
    }
    Ok((executable, architecture))
}

/// The executable an application bundle declares, which must stay inside it.
fn bundle_executable(bundle: &Path) -> Result<PathBuf, Error> {
    let output = Command::new("/usr/bin/plutil")
        .args(["-extract", "CFBundleExecutable", "raw", "-o", "-"])
        .arg(bundle.join("Contents/Info.plist"))
        .env_clear()
        .output()?;
    let name = std::str::from_utf8(&output.stdout)
        .map_err(|_| Error::InvalidApplication)?
        .trim();
    if !output.status.success()
        || name.is_empty()
        || name.len() > 255
        || name.contains('/')
        || name == "."
        || name == ".."
    {
        return Err(Error::InvalidApplication);
    }
    let executable = bundle
        .join("Contents/MacOS")
        .join(name)
        .canonicalize()
        .map_err(|_| Error::InvalidApplication)?;
    if !executable.starts_with(bundle) {
        return Err(Error::InvalidApplication);
    }
    Ok(executable)
}

/// The environment every game starts with: the player's home, account and
/// language, and nothing else of Forge's. A supervisor starts without any
/// environment, so Forge builds this before handing the game over.
pub fn environment(home: &Path) -> HashMap<String, String> {
    environment_from(home, |key| std::env::var(key).ok())
}

fn environment_from(home: &Path, read: impl Fn(&str) -> Option<String>) -> HashMap<String, String> {
    let mut environment = HashMap::from([
        ("HOME".to_owned(), home.to_string_lossy().into_owned()),
        (
            "PATH".to_owned(),
            "/usr/bin:/bin:/usr/sbin:/sbin".to_owned(),
        ),
        (
            "TMPDIR".to_owned(),
            read("TMPDIR").unwrap_or_else(|| std::env::temp_dir().to_string_lossy().into_owned()),
        ),
    ]);
    if let Some(user) = read("USER").or_else(|| read("LOGNAME")) {
        environment.insert("USER".into(), user.clone());
        environment.insert("LOGNAME".into(), user);
    }
    for key in ["LANG", "LC_ALL", "LC_CTYPE"] {
        if let Some(value) = read(key).filter(|value| !value.is_empty()) {
            environment.insert(key.into(), value);
        }
    }
    // Without any language, many games misread their own text files.
    if !["LANG", "LC_ALL", "LC_CTYPE"]
        .iter()
        .any(|key| environment.contains_key(*key))
    {
        environment.insert("LANG".into(), "en_US.UTF-8".into());
    }
    environment
}

#[cfg(test)]
mod tests;
