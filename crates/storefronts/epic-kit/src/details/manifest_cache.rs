//! Raw manifests keyed by the current asset's immutable hash and full identity.
//! Account rights and CDN metadata are always fetched before consulting this cache.
use std::path::{Path, PathBuf};

use forge_core::install::{self, PARTIAL_DIRECTORY, hex};
use sha1::{Digest, Sha1};

use crate::{
    EpicError,
    client::validate_identity,
    content::validate_manifest,
    content_manifest::{self, Manifest},
    manifest::MAX_MANIFEST_BYTES,
};

#[derive(Clone)]
pub(crate) struct ManifestCacheKey {
    identity: String,
    hash: String,
    app_name: String,
}

impl ManifestCacheKey {
    pub(super) fn new(
        namespace: &str,
        catalog_item_id: &str,
        app_name: &str,
        platform: &str,
        hash: Option<&str>,
    ) -> Option<Self> {
        let hash = hash?;
        if validate_identity(namespace, catalog_item_id, app_name, platform).is_err()
            || hash.len() != 40
            || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return None;
        }
        let identity = serde_json::to_vec(&(namespace, catalog_item_id, app_name, platform))
            .expect("strings serialize");
        Some(Self {
            identity: hex(&Sha1::digest(identity)),
            hash: hash.to_ascii_lowercase(),
            app_name: app_name.into(),
        })
    }

    fn relative_path(&self) -> PathBuf {
        PathBuf::from(PARTIAL_DIRECTORY)
            .join("manifests")
            .join(&self.identity)
            .join(format!("{}.manifest", self.hash))
    }
}

pub(super) struct CachedManifest {
    pub bytes: Vec<u8>,
    pub manifest: Manifest,
}

pub(super) fn load(
    destination: &Path,
    key: &ManifestCacheKey,
) -> Result<Option<CachedManifest>, EpicError> {
    let Some(bytes) =
        install::read_cached_file(destination, &key.relative_path(), MAX_MANIFEST_BYTES)?
    else {
        return Ok(None);
    };
    let validated = (|| {
        super::checked_hash(&bytes, Some(&key.hash))?;
        validate(&bytes, &key.app_name)
    })();
    match validated {
        Ok(manifest) => Ok(Some(CachedManifest { bytes, manifest })),
        // A corrupt cache or an obsolete format is only a miss. Filesystem
        // redirections were rejected above, before reading any cache bytes.
        Err(EpicError::Protocol | EpicError::UnsupportedContent | EpicError::UnsafePath(_)) => {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

pub(super) fn validate(bytes: &[u8], app_name: &str) -> Result<Manifest, EpicError> {
    let manifest = content_manifest::parse(bytes)?;
    if manifest.app_name != app_name {
        return Err(EpicError::Protocol);
    }
    validate_manifest(&manifest)?;
    Ok(manifest)
}

/// The caller holds the installation lock and has validated these exact bytes.
/// Persist only the original body; URLs, authentication and sidecars stay in memory.
pub(crate) fn store(
    destination: &Path,
    key: &ManifestCacheKey,
    bytes: &[u8],
) -> Result<(), EpicError> {
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(EpicError::Protocol);
    }
    Ok(install::write_cached_file(
        destination,
        &key.relative_path(),
        bytes,
    )?)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::fs::{self, File};

    pub(in crate::details) fn fixture(app: &str, filename: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "ManifestFileVersion": "012000000000",
            "bIsFileData": false,
            "AppNameString": app,
            "BuildVersionString": "1.0",
            "LaunchExeString": filename,
            "LaunchCommand": "",
            "FileManifestList": [{
                "Filename": filename,
                "FileHash": Sha1::digest([]).iter().map(|byte| format!("{byte:03}")).collect::<String>(),
                "FileChunkParts": [],
            }],
            "ChunkHashList": {},
            "ChunkShaList": {},
            "DataGroupList": {},
            "ChunkFilesizeList": {},
        }))
        .unwrap()
    }

    fn key(bytes: &[u8]) -> ManifestCacheKey {
        ManifestCacheKey::new(
            "ns",
            "catalog",
            "game",
            "Mac",
            Some(
                &Sha1::digest(bytes)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>(),
            ),
        )
        .unwrap()
    }

    #[test]
    fn caches_raw_bytes_and_preserves_validated_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let bytes = fixture("game", "game.exe");
        let key = key(&bytes);
        store(directory.path(), &key, &bytes).unwrap();
        let snapshot = load(directory.path(), &key).unwrap().unwrap();
        assert_eq!(snapshot.bytes, bytes);
        assert_eq!(snapshot.manifest.files[0].filename, "game.exe");
        let path = directory.path().join(key.relative_path());
        assert_eq!(fs::read(&path).unwrap(), bytes);
        fs::write(path, b"corrupted after load").unwrap();
        assert!(load(directory.path(), &key).unwrap().is_none());
        assert_eq!(snapshot.manifest.app_name, "game");
    }

    #[test]
    fn cache_keys_require_full_identity_and_valid_current_hash() {
        let bytes = fixture("game", "game.exe");
        let hash = Sha1::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let original = key(&bytes).relative_path();
        for (namespace, catalog, app, platform, hash) in [
            ("other", "catalog", "game", "Mac", hash.as_str()),
            ("ns", "other", "game", "Mac", hash.as_str()),
            ("ns", "catalog", "other", "Mac", hash.as_str()),
            ("ns", "catalog", "game", "Windows", hash.as_str()),
            (
                "ns",
                "catalog",
                "game",
                "Mac",
                "0000000000000000000000000000000000000000",
            ),
        ] {
            assert_ne!(
                ManifestCacheKey::new(namespace, catalog, app, platform, Some(hash))
                    .unwrap()
                    .relative_path(),
                original
            );
        }
        assert!(ManifestCacheKey::new("ns", "catalog", "game", "Mac", None).is_none());
        assert!(ManifestCacheKey::new("ns", "catalog", "game", "Mac", Some("bad")).is_none());
        assert!(ManifestCacheKey::new("", "catalog", "game", "Mac", Some(&hash)).is_none());
        assert!(ManifestCacheKey::new("ns", "catalog", "\0", "Mac", Some(&hash)).is_none());
        assert_eq!(
            ManifestCacheKey::new("ns", "catalog", "game", "Mac", Some(&hash.to_uppercase()))
                .unwrap()
                .relative_path(),
            original
        );
    }

    #[test]
    fn corrupt_oversized_mismatched_and_unsafe_manifests_are_misses() {
        let directory = tempfile::tempdir().unwrap();
        for bytes in [
            b"invalid".to_vec(),
            fixture("other-app", "game.exe"),
            fixture("game", "../escape"),
        ] {
            let key = key(&bytes);
            store(directory.path(), &key, &bytes).unwrap();
            assert!(load(directory.path(), &key).unwrap().is_none());
        }
        let bytes = fixture("game", "game.exe");
        let key = key(&bytes);
        store(directory.path(), &key, &bytes).unwrap();
        let path = directory.path().join(key.relative_path());
        fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
        assert!(load(directory.path(), &key).unwrap().is_none());
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_len(MAX_MANIFEST_BYTES as u64 + 1)
            .unwrap();
        assert!(load(directory.path(), &key).unwrap().is_none());
    }

    #[test]
    fn cache_miss_does_not_create_installation() {
        let directory = tempfile::tempdir().unwrap();
        let absent = directory.path().join("absent");
        assert!(
            load(&absent, &key(&fixture("game", "game.exe")))
                .unwrap()
                .is_none()
        );
        assert!(!absent.exists());
    }

    #[cfg(unix)]
    #[test]
    fn cache_parent_symlinks_and_file_links_are_rejected() {
        let bytes = fixture("game", "game.exe");
        let key = key(&bytes);
        for parent in [true, false] {
            for symlink in [true, false] {
                if parent && !symlink {
                    continue;
                }
                let directory = tempfile::tempdir().unwrap();
                let outside = tempfile::tempdir().unwrap();
                let external_file = outside.path().join("outside");
                fs::write(&external_file, &bytes).unwrap();
                if parent {
                    std::os::unix::fs::symlink(
                        outside.path(),
                        directory.path().join(".forge-content"),
                    )
                    .unwrap();
                } else {
                    let relative = key.relative_path();
                    fs::create_dir_all(directory.path().join(relative.parent().unwrap())).unwrap();
                    let cache_path = directory.path().join(relative);
                    if symlink {
                        std::os::unix::fs::symlink(&external_file, cache_path).unwrap();
                    } else {
                        fs::hard_link(&external_file, cache_path).unwrap();
                    }
                }
                assert!(matches!(
                    load(directory.path(), &key),
                    Err(EpicError::UnsafePath(_))
                ));
                assert!(matches!(
                    store(directory.path(), &key, &bytes),
                    Err(EpicError::UnsafePath(_))
                ));
                assert_eq!(fs::read(&external_file).unwrap(), bytes);
                assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 1);
            }
        }
    }
}
