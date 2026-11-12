use super::*;

/// A release archive laid out as voile's package.sh makes it, with the Linux
/// engine's runtime in its `linux` folder, and the release that pins it.
/// `manifest_version` is what its runtime.json claims.
fn release_archive(directory: &Path, version: &str, manifest_version: &str) -> (PathBuf, Release) {
    let package = directory.join("package");
    lay(
        &package,
        &format!("voile-{version}"),
        &format!(r#"{{"version": "{manifest_version}"}}"#),
    );
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
    };
    (archive, release)
}

fn names(root: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// A release's runtime in `root`, in `folder`, as its archive and an install
/// lay it, whose runtime.json holds `manifest`.
fn lay(root: &Path, folder: &str, manifest: &str) {
    let runtime = root.join(folder);
    fs::create_dir_all(runtime.join("bin")).unwrap();
    fs::write(runtime.join("bin/wine"), "wine").unwrap();
    fs::create_dir_all(runtime.join("linux/engine")).unwrap();
    fs::create_dir_all(runtime.join("linux/wine/bin")).unwrap();
    fs::write(runtime.join("linux/engine/fexdarwin-run"), "engine").unwrap();
    fs::write(runtime.join("linux/wine/bin/wine"), "wine").unwrap();
    fs::write(runtime.join(MANIFEST), manifest).unwrap();
}

/// The feed Obelisk read last, in `root`, offering `releases`.
fn feed(root: &Path, releases: &str) {
    feed_with_prereleases(root, releases, "");
}

/// The feed Obelisk read last, in `root`, offering `releases` and
/// `prereleases`.
fn feed_with_prereleases(root: &Path, releases: &str, prereleases: &str) {
    fs::create_dir_all(root).unwrap();
    fs::write(
        root.join(feed::KEPT),
        format!(r#"{{"format": 1, "releases": [{releases}], "prereleases": [{prereleases}]}}"#),
    )
    .unwrap();
}

/// A release a feed offers, of `contract`.
fn offered(version: &str, contract: u32, withdrawn: Option<&str>) -> String {
    let withdrawn = withdrawn
        .map(|reason| format!(r#", "withdrawn": "{reason}""#))
        .unwrap_or_default();
    format!(
        r#"{{"version": "{version}", "contract": {contract}, "url": "https://example.invalid/voile-{version}.tar.xz", "size": 3, "sha256": "{}"{withdrawn}}}"#,
        "a".repeat(64)
    )
}

/// A release a feed offers, of the first contract, which brings `notes`.
fn noted(version: &str, notes: &str) -> String {
    offered(version, 1, None).replacen('{', &format!(r#"{{"notes": "{notes}", "#), 1)
}

#[test]
fn the_checked_in_release_names_its_archive_and_obelisk_s_contract() {
    let release = release();
    assert!(release.url.ends_with(&format!(
        "/releases/download/{0}/voile-{0}.tar.xz",
        release.version
    )));
    assert_eq!(release.sha256.len(), 64);
    assert!(is_version(&release.version));
    assert!(contract() >= 1);
}

#[test]
fn versions_order_by_their_numbers() {
    assert_eq!(numbers("v26.3.0-16"), Some(vec![26, 3, 0, 16, 1, 0]));
    assert!(numbers("v26.3.0-10") > numbers("v26.3.0-9"));
    assert!(numbers("v26.4.0-1") > numbers("v26.3.0-16"));
    assert_eq!(numbers("current"), None);
    assert_eq!(numbers("v26.3.0-x"), None);
    assert!(is_version("v26.3.0-16"));
    assert!(!is_version("v26.3"));
    assert!(!is_version("26.3.0-16"));
    assert!(!is_version("v26.3.0.1-16"));
    assert!(!is_version("v26.3.0-+16"));
    assert!(
        !is_version("v26.3.0-16-dev2"),
        "a local build is no release"
    );
}

#[test]
fn a_beta_comes_after_the_release_before_and_before_its_own() {
    assert_eq!(numbers("v26.3.0-16.beta2"), Some(vec![26, 3, 0, 16, 0, 2]));
    assert!(numbers("v26.3.0-15") < numbers("v26.3.0-16.beta1"));
    assert!(numbers("v26.3.0-16.beta1") < numbers("v26.3.0-16.beta2"));
    assert!(numbers("v26.3.0-16.beta9") < numbers("v26.3.0-16.beta10"));
    assert!(numbers("v26.3.0-16.beta10") < numbers("v26.3.0-16"));
    assert!(
        !is_version("v26.3.0-16.beta1"),
        "a profile names releases only"
    );
    assert!(is_beta("v26.3.0-16.beta1"));
    assert!(!is_beta("v26.3.0-16"));
    assert!(!is_beta("v26.3.0-16.beta"));
    assert!(!is_version("v26.3.0-16.beta"));
    assert!(!is_version("v26.3.0-16.betax"));
    assert!(!is_version("v26.3.0-16.beta1.beta2"));
}

#[test]
fn games_run_in_the_newest_release_of_obelisk_s_contract() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    assert_eq!(runtime(root), None);
    // A release before CONTRACT kept the first contract.
    lay(root, "v1.0.0-9", r#"{"version": "v1.0.0-9"}"#);
    lay(
        root,
        "v1.0.0-10",
        r#"{"version": "v1.0.0-10", "contract": 1}"#,
    );
    lay(
        root,
        "v1.0.0-11",
        r#"{"version": "v1.0.0-11", "contract": 2}"#,
    );
    lay(
        root,
        "v1.0.0-12",
        r#"{"version": "v1.0.0-1", "contract": 1}"#,
    );
    assert_eq!(runtime(root), Some(root.join("v1.0.0-10")));
    assert_eq!(linux_runtime(root), Some(root.join("v1.0.0-10/linux")));
    assert_eq!(installed_release(root), Some("v1.0.0-10".into()));
}

#[test]
fn a_withdrawn_release_runs_no_game_and_goes_with_the_others_replaced() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    lay(root, "v1.0.0-1", r#"{"version": "v1.0.0-1"}"#);
    lay(root, "v1.0.0-2", r#"{"version": "v1.0.0-2"}"#);
    lay(
        root,
        "v1.0.0-3",
        r#"{"version": "v1.0.0-3", "contract": 2}"#,
    );
    feed(
        root,
        &[
            offered("v1.0.0-2", 1, Some("breaks a game")),
            offered("v1.0.0-1", 1, None),
        ]
        .join(","),
    );
    fs::create_dir_all(root.join(".unfinished")).unwrap();
    assert_eq!(runtime(root), Some(root.join("v1.0.0-1")));

    remove_replaced(root);

    assert_eq!(names(root), [".unfinished", "feed.json", "v1.0.0-1"]);
}

#[test]
fn the_release_wanted_is_the_newest_the_feed_offers_for_obelisk_s_contract() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    assert_eq!(&Offer::read(root).wanted(), release(), "no feed read yet");
    feed(
        root,
        &[
            offered("v1.0.0-5", 1, Some("breaks a game")),
            offered("v1.0.0-4", 2, None),
            offered("v1.0.0-3", 1, None),
            offered("v1.0.0-2", 1, None),
        ]
        .join(","),
    );
    let wanted = Offer::read(root).wanted();
    assert_eq!(wanted.version, "v1.0.0-3");
    assert_eq!(wanted.url, "https://example.invalid/voile-v1.0.0-3.tar.xz");
}

/// Whether a launch in `root`, of a game that needs `minimum` or a newer
/// release, asks about a newer one, and must take it: `None` when it does
/// not ask.
fn asks(root: &Path, minimum: Option<&str>) -> Option<bool> {
    match standing(root, minimum) {
        Standing::Outdated(update) => Some(update.required),
        _ => None,
    }
}

#[test]
fn a_launch_waits_for_a_missing_release_and_asks_about_a_newer_one() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    feed(root, &offered("v1.0.0-3", 1, None));
    assert_eq!(standing(root, None), Standing::Missing);

    lay(root, "v1.0.0-2", r#"{"version": "v1.0.0-2"}"#);
    assert_eq!(asks(root, None), Some(false));
    assert_eq!(
        asks(root, Some("v1.0.0-3")),
        Some(true),
        "the game needs the release offered"
    );
    assert_eq!(
        asks(root, Some("v1.0.0-9")),
        Some(false),
        "no release offered is enough: the game runs in the newest it can"
    );
    assert_eq!(asks(root, Some("v1.0.0-2")), Some(false));

    lay(root, "v1.0.0-3", r#"{"version": "v1.0.0-3"}"#);
    assert_eq!(standing(root, Some("v1.0.0-3")), Standing::Current);

    fs::create_dir_all(root.join("current/bin")).unwrap();
    fs::write(root.join("current/bin/wine"), "wine").unwrap();
    feed(root, &offered("v1.0.0-4", 1, None));
    assert_eq!(standing(root, Some("v1.0.0-4")), Standing::Current);
}

#[test]
fn a_launch_offers_what_each_release_newer_than_the_one_in_place_brings() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    lay(root, "v1.0.0-2", r#"{"version": "v1.0.0-2"}"#);
    feed(
        root,
        &[
            offered("v1.0.0-6", 2, None),
            offered("v1.0.0-5", 1, Some("breaks a game")),
            noted("v1.0.0-4", "### Fixes\\n\\n- Fewer crashes"),
            offered("v1.0.0-3", 1, None),
            noted("v1.0.0-2", "Already in place"),
            noted("v1.0.0-1", "Older"),
        ]
        .join(","),
    );

    let notes = |version: &str, notes: &str| Notes {
        version: version.into(),
        notes: notes.into(),
    };
    assert_eq!(
        standing(root, None),
        Standing::Outdated(Update {
            version: "v1.0.0-4".into(),
            required: false,
            releases: vec![
                notes("v1.0.0-4", "### Fixes\n\n- Fewer crashes"),
                notes("v1.0.0-3", ""),
            ],
        })
    );
}

#[tokio::test]
async fn a_release_keeps_the_ones_before_it_until_they_are_removed() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("Voile");
    let (archive, release) = release_archive(directory.path(), "v1.0.0-2", "v1.0.0-2");
    fs::create_dir_all(root.join("v1.0.0-1/bin")).unwrap();
    fs::create_dir_all(root.join(".interrupted")).unwrap();

    let runtime = Install::new()
        .install(&root, &release, Some(&archive))
        .await
        .unwrap();

    assert_eq!(runtime, root.join("v1.0.0-2"));
    assert_eq!(self::runtime(&root), Some(runtime));
    assert_eq!(names(&root), ["v1.0.0-1", "v1.0.0-2"]);

    // Once no game may run from them, the ones before it go.
    fs::create_dir_all(root.join("current/bin")).unwrap();
    fs::create_dir_all(root.join(".unfinished")).unwrap();
    remove_replaced(&root);
    assert_eq!(names(&root), [".unfinished", "current", "v1.0.0-2"]);

    // The same release again is in place already.
    assert_eq!(
        Install::new()
            .install(&root, &release, Some(&root.join("no archive")))
            .await,
        Ok(root.join("v1.0.0-2"))
    );
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
async fn the_installed_release_is_the_runtime_games_run_in() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("Voile");
    let (archive, release) = release_archive(directory.path(), "v1.0.0-2", "v1.0.0-2");
    assert_eq!(installed_release(&root), None);
    Install::new()
        .install(&root, &release, Some(&archive))
        .await
        .unwrap();
    assert_eq!(installed_release(&root), Some("v1.0.0-2".into()));
    fs::create_dir_all(root.join("current/bin")).unwrap();
    fs::write(root.join("current/bin/wine"), "wine").unwrap();
    fs::write(root.join("current").join(MANIFEST), r#"{"version": "dev"}"#).unwrap();
    assert_eq!(installed_release(&root), Some("dev".into()));
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
async fn the_linux_runtime_comes_with_its_release() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("Voile");
    let (archive, release) = release_archive(directory.path(), "v1.0.0-2", "v1.0.0-2");
    assert_eq!(linux_runtime(&root), None);
    Install::new()
        .install(&root, &release, Some(&archive))
        .await
        .unwrap();
    assert_eq!(linux_runtime(&root), Some(root.join("v1.0.0-2/linux")));
}

#[test]
fn a_linux_runtime_left_from_another_release_is_not_used() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("Voile");
    fs::create_dir_all(root.join("v1.0.0-2/linux/engine")).unwrap();
    fs::create_dir_all(root.join("v1.0.0-2/linux/wine/bin")).unwrap();
    fs::write(root.join("v1.0.0-2/linux/engine/fexdarwin-run"), "engine").unwrap();
    fs::write(root.join("v1.0.0-2/linux/wine/bin/wine"), "wine").unwrap();
    // No Windows runtime of this release around it.
    assert_eq!(linux_runtime(&root), None);
}

#[test]
fn the_development_linux_runtime_takes_precedence() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("Voile");
    fs::create_dir_all(root.join("current/linux/engine")).unwrap();
    fs::create_dir_all(root.join("current/linux/wine/bin")).unwrap();
    fs::write(root.join("current/linux/engine/fexdarwin-run"), "engine").unwrap();
    fs::write(root.join("current/linux/wine/bin/wine"), "wine").unwrap();
    assert_eq!(linux_runtime(&root), Some(root.join("current/linux")));
}

#[test]
fn a_player_without_beta_releases_leaves_a_pre_release_for_the_newest_release() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    lay(root, "v1.0.0-2", r#"{"version": "v1.0.0-2"}"#);
    feed_with_prereleases(
        root,
        &offered("v1.0.0-2", 1, None),
        &noted("v1.0.0-3", "A pre-release"),
    );
    assert_eq!(
        standing(root, None),
        Standing::Current,
        "nothing newer to take"
    );
    assert_eq!(
        asks(root, Some("v1.0.0-3")),
        None,
        "a game that needs a pre-release runs in the newest release"
    );

    // The pre-release a player took while they took beta releases.
    lay(root, "v1.0.0-3", r#"{"version": "v1.0.0-3"}"#);
    assert_eq!(runtime(root), Some(root.join("v1.0.0-2")));
    assert_eq!(linux_runtime(root), Some(root.join("v1.0.0-2/linux")));
    remove_replaced(root);
    assert_eq!(names(root), ["feed.json", "v1.0.0-2"]);

    // Once the release before it is gone, a launch waits for the release.
    fs::remove_dir_all(root.join("v1.0.0-2")).unwrap();
    lay(root, "v1.0.0-3", r#"{"version": "v1.0.0-3"}"#);
    assert_eq!(runtime(root), None);
    assert_eq!(standing(root, None), Standing::Missing);
    assert_eq!(Offer::read(root).wanted().version, "v1.0.0-2");
}

#[test]
fn a_player_who_takes_beta_releases_runs_the_newest_release_or_pre_release() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    lay(root, "v1.0.0-2", r#"{"version": "v1.0.0-2"}"#);
    feed_with_prereleases(
        root,
        &offered("v1.0.0-2", 1, None),
        &[
            offered("v1.0.0-5", 1, Some("breaks a game")),
            noted("v1.0.0-4", "Newest pre-release"),
            noted("v1.0.0-3", "Older pre-release"),
        ]
        .join(","),
    );
    take_prereleases(root, true).unwrap();
    take_prereleases(root, true).unwrap();

    let notes = |version: &str, notes: &str| Notes {
        version: version.into(),
        notes: notes.into(),
    };
    assert_eq!(
        standing(root, None),
        Standing::Outdated(Update {
            version: "v1.0.0-4".into(),
            required: false,
            releases: vec![
                notes("v1.0.0-4", "Newest pre-release"),
                notes("v1.0.0-3", "Older pre-release"),
            ],
        })
    );
    assert_eq!(asks(root, Some("v1.0.0-4")), Some(true));

    lay(root, "v1.0.0-4", r#"{"version": "v1.0.0-4"}"#);
    assert_eq!(runtime(root), Some(root.join("v1.0.0-4")));
    remove_replaced(root);
    assert_eq!(names(root), ["feed.json", "prereleases", "v1.0.0-4"]);

    // A release newer than every pre-release is the one wanted.
    feed_with_prereleases(
        root,
        &[offered("v1.0.0-6", 1, None), offered("v1.0.0-2", 1, None)].join(","),
        &offered("v1.0.0-4", 1, None),
    );
    assert_eq!(Offer::read(root).wanted().version, "v1.0.0-6");

    // Turned off, the pre-release in place is left for the newest release.
    take_prereleases(root, false).unwrap();
    take_prereleases(root, false).unwrap();
    assert_eq!(names(root), ["feed.json", "v1.0.0-4"]);
    assert_eq!(runtime(root), None);
    assert_eq!(standing(root, None), Standing::Missing);
}

#[test]
fn a_pre_release_turned_into_a_release_runs_for_every_player() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    lay(root, "v1.0.0-3", r#"{"version": "v1.0.0-3"}"#);
    feed_with_prereleases(
        root,
        &[offered("v1.0.0-3", 1, None), offered("v1.0.0-2", 1, None)].join(","),
        "",
    );
    assert_eq!(runtime(root), Some(root.join("v1.0.0-3")));
    assert_eq!(standing(root, None), Standing::Current);
}

#[test]
fn a_player_who_takes_beta_releases_runs_the_newest_beta_until_its_release() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    lay(root, "v1.0.0-2", r#"{"version": "v1.0.0-2"}"#);
    lay(root, "v1.0.0-3.beta2", r#"{"version": "v1.0.0-3.beta2"}"#);
    feed_with_prereleases(
        root,
        &offered("v1.0.0-2", 1, None),
        &[
            offered("v1.0.0-3.beta10", 1, None),
            offered("v1.0.0-3.beta2", 1, None),
        ]
        .join(","),
    );
    assert_eq!(runtime(root), Some(root.join("v1.0.0-2")), "betas off");
    take_prereleases(root, true).unwrap();
    assert_eq!(runtime(root), Some(root.join("v1.0.0-3.beta2")));
    assert_eq!(Offer::read(root).wanted().version, "v1.0.0-3.beta10");
    assert!(
        matches!(standing(root, None), Standing::Outdated(update) if update.version == "v1.0.0-3.beta10")
    );

    // Its release comes after every beta of it.
    feed_with_prereleases(
        root,
        &[offered("v1.0.0-3", 1, None), offered("v1.0.0-2", 1, None)].join(","),
        &offered("v1.0.0-3.beta10", 1, None),
    );
    assert_eq!(Offer::read(root).wanted().version, "v1.0.0-3");
}

#[test]
fn a_beta_the_feed_no_longer_lists_is_still_left_without_beta_releases() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    lay(root, "v1.0.0-2", r#"{"version": "v1.0.0-2"}"#);
    lay(root, "v1.0.0-3.beta1", r#"{"version": "v1.0.0-3.beta1"}"#);
    feed(root, &offered("v1.0.0-2", 1, None));
    assert_eq!(runtime(root), Some(root.join("v1.0.0-2")));
    remove_replaced(root);
    assert_eq!(names(root), ["feed.json", "v1.0.0-2"]);

    lay(root, "v1.0.0-3.beta1", r#"{"version": "v1.0.0-3.beta1"}"#);
    take_prereleases(root, true).unwrap();
    assert_eq!(runtime(root), Some(root.join("v1.0.0-3.beta1")));

    // A beta turned into a release is one for every player.
    take_prereleases(root, false).unwrap();
    feed(
        root,
        &[
            offered("v1.0.0-3.beta1", 1, None),
            offered("v1.0.0-2", 1, None),
        ]
        .join(","),
    );
    assert_eq!(runtime(root), Some(root.join("v1.0.0-3.beta1")));
}
