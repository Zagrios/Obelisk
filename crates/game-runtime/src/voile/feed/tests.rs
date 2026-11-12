use super::*;

/// A feed signed as Voile's Feed workflow signs one, with OpenSSL, by a key
/// made for these tests and thrown away.
const SIGNED: &[u8] = include_bytes!("signed-feed.json");
const SIGNATURE: &[u8] = include_bytes!("signed-feed.json.sig");
const SIGNER: &str = "auwHg9+CBTsKq3oTfRLk54XE1fcAUz5XLuEgFifqx6s=";

#[test]
fn obelisk_checks_the_feed_with_an_ed25519_key() {
    let key = base64::engine::general_purpose::STANDARD
        .decode(KEY)
        .expect("Voile's feed key, from scripts/feed-key.sh");
    assert_eq!(key.len(), 32);
}

#[test]
fn a_feed_holds_only_with_the_signature_its_signer_made() {
    assert!(signed(SIGNED, SIGNATURE, SIGNER));
    // The workflow writes no newline after it; one is no matter.
    assert!(signed(SIGNED, &[SIGNATURE, b"\n"].concat(), SIGNER));

    let mut changed = SIGNED.to_vec();
    let at = changed.iter().position(|&byte| byte == b'6').unwrap();
    changed[at] = b'7';
    assert!(!signed(&changed, SIGNATURE, SIGNER));
    assert!(!signed(SIGNED, SIGNATURE, KEY));
    assert!(!signed(SIGNED, b"not base64", SIGNER));
    assert!(!signed(SIGNED, SIGNATURE, "not a key"));
}

#[test]
fn obelisk_reads_the_releases_of_a_feed_of_its_format() {
    assert_eq!(
        releases(SIGNED).unwrap(),
        [Offered {
            contract: 1,
            release: Release {
                version: "v26.3.0-16".into(),
                url: "https://example.invalid/voile-v26.3.0-16.tar.xz".into(),
                size: 3,
                sha256: "a3c2e7b9b0f2f5e8c4d1e6a7b8c9d0e1f2a3b4c5d6e7f8091a2b3c4d5e6f7081".into(),
            },
            notes: String::new(),
            withdrawn: None,
            prerelease: false,
        }]
    );
    assert_eq!(releases(br#"{"format": 2, "releases": []}"#), None);
    assert_eq!(releases(b"<html>"), None);
}

#[test]
fn the_feed_kept_is_the_one_launches_read() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("Voile");
    assert_eq!(offered(&root), Vec::<Offered>::new());
    crate::remote::keep(&root.join(KEPT), SIGNED).unwrap();
    assert_eq!(offered(&root), releases(SIGNED).unwrap());
    assert_eq!(fs::read(root.join(KEPT)).unwrap(), SIGNED);
}

#[test]
fn obelisk_reads_the_pre_releases_after_the_releases() {
    let entry = |version: &str| {
        format!(
            r#"{{"version": "{version}", "contract": 1, "url": "https://example.invalid/{version}", "size": 3, "sha256": "{}"}}"#,
            "a".repeat(64)
        )
    };
    let feed = format!(
        r#"{{"format": 1, "releases": [{}], "prereleases": [{}, {}]}}"#,
        entry("v1.0.0-2"),
        entry("v1.0.0-3"),
        entry("v1.0.0-2"),
    );
    let read: Vec<_> = releases(feed.as_bytes())
        .unwrap()
        .into_iter()
        .map(|offered| (offered.release.version, offered.prerelease))
        .collect();
    assert_eq!(
        read,
        [
            ("v1.0.0-2".to_string(), false),
            ("v1.0.0-3".to_string(), true)
        ],
        "a version that is both is a release"
    );
}

#[test]
fn the_player_s_choice_of_beta_releases_stays_beside_the_feed() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("Voile");
    assert!(!takes_prereleases(&root));
    take_prereleases(&root, false).unwrap();
    assert!(!root.exists(), "turning it off creates nothing");
    take_prereleases(&root, true).unwrap();
    assert!(takes_prereleases(&root));
    take_prereleases(&root, false).unwrap();
    assert!(!takes_prereleases(&root));
}
