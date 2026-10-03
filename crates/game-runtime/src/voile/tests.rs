use super::*;

/// A release archive laid out as voile's package.sh makes it, and the release
/// that pins it. `manifest_version` is what its runtime.json claims.
fn release_archive(directory: &Path, version: &str, manifest_version: &str) -> (PathBuf, Release) {
    let package = directory.join("package");
    let runtime = package.join(format!("voile-{version}"));
    fs::create_dir_all(runtime.join("bin")).unwrap();
    fs::write(runtime.join("bin/wine"), "wine").unwrap();
    fs::write(
        runtime.join(MANIFEST),
        format!(r#"{{"version": "{manifest_version}"}}"#),
    )
    .unwrap();
    let archive = directory.join(format!("voile-{version}.tar.xz"));
    let status = std::process::Command::new("/usr/bin/tar")
        .arg("-cJf")
        .arg(&archive)
        .arg("-C")
        .arg(&package)
        .arg(format!("voile-{version}"))
        .status()
        .unwrap();
    assert!(status.success());
    let release = Release {
        version: version.into(),
        url: "https://example.invalid/voile.tar.xz".into(),
        size: fs::metadata(&archive).unwrap().len(),
        sha256: crate::sha(&archive).unwrap(),
        linux: None,
    };
    (archive, release)
}

/// The Linux engine's archive as voile's package-linux.sh makes it, pinned as
/// `release`'s `linux` asset.
fn linux_archive(directory: &Path, release: &mut Release, manifest_version: &str) -> PathBuf {
    let version = release.version.clone();
    let package = directory.join("package-linux");
    let runtime = package.join(format!("voile-linux-{version}"));
    fs::create_dir_all(runtime.join("engine")).unwrap();
    fs::create_dir_all(runtime.join("wine/bin")).unwrap();
    fs::write(runtime.join("engine/fexdarwin-run"), "engine").unwrap();
    fs::write(runtime.join("wine/bin/wine"), "wine").unwrap();
    fs::write(
        runtime.join(MANIFEST),
        format!(r#"{{"version": "{manifest_version}"}}"#),
    )
    .unwrap();
    let archive = directory.join(format!("voile-linux-{version}.tar.xz"));
    let status = std::process::Command::new("/usr/bin/tar")
        .arg("-cJf")
        .arg(&archive)
        .arg("-C")
        .arg(&package)
        .arg(format!("voile-linux-{version}"))
        .status()
        .unwrap();
    assert!(status.success());
    release.linux = Some(Asset {
        url: "https://example.invalid/voile-linux.tar.xz".into(),
        size: fs::metadata(&archive).unwrap().len(),
        sha256: crate::sha(&archive).unwrap(),
    });
    archive
}

fn names(root: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn the_checked_in_release_names_its_archive() {
    let release = release();
    assert!(release.url.ends_with(&format!(
        "/releases/download/{0}/voile-{0}.tar.xz",
        release.version
    )));
    assert_eq!(release.sha256.len(), 64);
    if let Some(linux) = &release.linux {
        assert!(linux.url.ends_with(&format!(
            "/releases/download/{0}/voile-linux-{0}.tar.xz",
            release.version
        )));
        assert_eq!(linux.sha256.len(), 64);
    }
}

#[tokio::test]
async fn a_release_replaces_the_one_before_it_and_leaves_nothing_unfinished() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("Voile");
    let (archive, release) = release_archive(directory.path(), "v1.0.0-2", "v1.0.0-2");
    fs::create_dir_all(root.join("v1.0.0-1/bin")).unwrap();
    fs::create_dir_all(root.join(".interrupted")).unwrap();
    assert_eq!(located(&root, &release), None);

    let runtime = Install::new()
        .install(&root, &release, Some(&archive))
        .await
        .unwrap();

    assert_eq!(runtime, root.join("v1.0.0-2"));
    assert_eq!(located(&root, &release), Some(runtime));
    assert_eq!(names(&root), ["v1.0.0-2"]);
}

#[tokio::test]
async fn a_damaged_archive_installs_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("Voile");
    let (archive, mut release) = release_archive(directory.path(), "v1.0.0-2", "v1.0.0-2");
    release.sha256 = "0".repeat(64);
    let failure = Install::new()
        .install(&root, &release, Some(&archive))
        .await;
    assert_eq!(failure, Err(Failure::Damaged));
    assert_eq!(names(&root), Vec::<String>::new());
}

#[tokio::test]
async fn an_archive_of_another_release_installs_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("Voile");
    let (archive, release) = release_archive(directory.path(), "v1.0.0-2", "v1.0.0-1");
    let failure = Install::new()
        .install(&root, &release, Some(&archive))
        .await;
    assert_eq!(failure, Err(Failure::Damaged));
    assert_eq!(names(&root), Vec::<String>::new());
}

#[tokio::test]
async fn the_development_runtime_takes_precedence_over_any_release() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("Voile");
    let (archive, release) = release_archive(directory.path(), "v1.0.0-2", "v1.0.0-2");
    fs::create_dir_all(root.join("current/bin")).unwrap();
    fs::write(root.join("current/bin/wine"), "wine").unwrap();
    let runtime = Install::new()
        .install(&root, &release, Some(&archive))
        .await
        .unwrap();
    assert_eq!(runtime, root.join("current"));
    assert_eq!(names(&root), ["current"]);
}

#[tokio::test]
async fn a_cancelled_install_stops_before_it_starts() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("Voile");
    let (archive, release) = release_archive(directory.path(), "v1.0.0-2", "v1.0.0-2");
    let install = Install::new();
    install.cancel();
    let failure = install.install(&root, &release, Some(&archive)).await;
    assert_eq!(failure, Err(Failure::Cancelled));
    assert!(!root.exists());
}

#[tokio::test]
async fn the_linux_runtime_goes_into_its_release_and_survives_the_windows_install() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("Voile");
    let (archive, mut release) = release_archive(directory.path(), "v1.0.0-2", "v1.0.0-2");
    let linux = linux_archive(directory.path(), &mut release, "v1.0.0-2");
    fs::create_dir_all(root.join("v1.0.0-1/linux")).unwrap();
    assert_eq!(located_linux(&root, &release), None);

    let install = Install::new();
    let runtime = install
        .install_linux(
            &root,
            &release,
            release.linux.as_ref().unwrap(),
            Some(&linux),
        )
        .await
        .unwrap();
    assert_eq!(runtime, root.join("v1.0.0-2/linux"));
    assert_eq!(install.progress().1, release.linux.as_ref().unwrap().size);
    assert_eq!(names(&root), ["v1.0.0-2"]);

    // The Windows runtime installed afterwards keeps it.
    Install::new()
        .install(&root, &release, Some(&archive))
        .await
        .unwrap();
    assert_eq!(located(&root, &release), Some(root.join("v1.0.0-2")));
    assert_eq!(located_linux(&root, &release), Some(runtime));
}

#[tokio::test]
async fn a_linux_archive_of_another_release_installs_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("Voile");
    let (_, mut release) = release_archive(directory.path(), "v1.0.0-2", "v1.0.0-2");
    let linux = linux_archive(directory.path(), &mut release, "v1.0.0-1");
    let failure = Install::new()
        .install_linux(
            &root,
            &release,
            release.linux.as_ref().unwrap(),
            Some(&linux),
        )
        .await;
    assert_eq!(failure, Err(Failure::Damaged));
    assert_eq!(located_linux(&root, &release), None);
}

#[tokio::test]
async fn the_development_linux_runtime_takes_precedence() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("Voile");
    let (_, mut release) = release_archive(directory.path(), "v1.0.0-2", "v1.0.0-2");
    let linux = linux_archive(directory.path(), &mut release, "v1.0.0-2");
    fs::create_dir_all(root.join("current/linux/engine")).unwrap();
    fs::create_dir_all(root.join("current/linux/wine/bin")).unwrap();
    fs::write(root.join("current/linux/engine/fexdarwin-run"), "engine").unwrap();
    fs::write(root.join("current/linux/wine/bin/wine"), "wine").unwrap();
    let runtime = Install::new()
        .install_linux(
            &root,
            &release,
            release.linux.as_ref().unwrap(),
            Some(&linux),
        )
        .await
        .unwrap();
    assert_eq!(runtime, root.join("current/linux"));
    assert_eq!(names(&root), ["current"]);
}
