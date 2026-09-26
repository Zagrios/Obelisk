use super::*;
use crate::content;
use sailover_core::transfer::CancelToken;

fn rules(path: &str) -> CloudRules {
    CloudRules {
        cloud_save_folder: Some("{AppData}/Game".into()),
        cloud_save_folder_mac: path.into(),
        include: vec![],
        exclude: vec![],
    }
}

#[test]
fn mac_rules_require_explicit_metadata_and_preserve_both_platforms() {
    use serde_json::json;
    assert!(
        CloudRules::from_attributes(&BTreeMap::new())
            .unwrap()
            .is_none()
    );
    let attrs = BTreeMap::from([
        (
            "CloudSaveFolder_MAC".into(),
            json!({"value":"{UserLibrary}/Application Support/Example"}),
        ),
        (
            "CloudSaveFolder".into(),
            json!({"value":"{AppData}/Example"}),
        ),
        ("CloudIncludeList".into(), json!({"value":"*.sav,profile/"})),
        (
            "CloudExcludeList".into(),
            json!({"value":"*.bak,profile/cache/"}),
        ),
    ]);
    let rules = CloudRules::from_attributes(&attrs).unwrap().unwrap();
    assert!(rules.accepts("nested/one.sav").unwrap());
    assert!(rules.accepts("profile/options.json").unwrap());
    assert!(!rules.accepts("profile/cache/slot.sav").unwrap());
    assert!(!rules.accepts("profile/one.bak").unwrap());
    assert!(!rules.accepts("other.txt").unwrap());
    assert!(rules.accepts("../foreign.sav").is_err());
    let mut bad = attrs.clone();
    bad.insert("CloudSaveFolder_MAC".into(), json!({"value":false}));
    assert!(CloudRules::from_attributes(&bad).is_err());
    bad.insert("CloudSaveFolder_MAC".into(), json!({"value":""}));
    assert!(CloudRules::from_attributes(&bad).is_err());
    assert!(parse_filters(Some("*.sav,,*.json".into())).is_err());
    assert!(parse_filters(Some("[0-9].sav".into())).is_err());
}

#[test]
fn resolver_confines_roots_and_keeps_missing_installation_recoverable() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    fs::create_dir(&home).unwrap();
    let install = temp.path().join("deleted/game");
    let resolved = rules("{USERLIBRARY}/Application Support/{AppName}/{EpicId}")
        .resolve(&home, &install, "account", "game")
        .unwrap();
    assert_eq!(
        resolved.directory,
        home.canonicalize()
            .unwrap()
            .join("Library/Application Support/game/account")
    );
    for path in [
        "{AppData}/Game",
        "{Unknown}/Game",
        "{Home}/../foreign",
        "{Home}/Library",
        "{Home}/",
        "/tmp/foreign",
    ] {
        assert!(
            rules(path)
                .resolve(&home, &install, "account", "game")
                .is_err(),
            "{path}"
        );
    }
    let resolved = rules("{InstallDir}/saves")
        .resolve(&home, &install, "account", "game")
        .unwrap();
    assert!(resolved.directory.ends_with("deleted/game/saves"));
    assert!(resolved.check_directory().is_ok());
}

#[test]
fn replacement_of_root_with_symlink_is_rejected_after_resolution() {
    #[cfg(unix)]
    {
        let temp = tempfile::tempdir().unwrap();
        let resolved = rules("{Home}/saves/child")
            .resolve(temp.path(), temp.path(), "account", "game")
            .unwrap();
        std::os::unix::fs::symlink(temp.path(), temp.path().join("saves")).unwrap();
        assert!(resolved.check_directory().is_err());
    }
}

#[test]
fn buildpatch_roundtrip_preserves_unicode_empty_files_and_chunk_boundaries() {
    let rules = rules("{Home}/Données");
    let cancel = CancelToken::new();
    let files = BTreeMap::from([
        (
            "Données/slot.sav".into(),
            (0..(1024 * 1024 + 31))
                .map(|index| (index % 251) as u8)
                .collect(),
        ),
        ("empty.sav".into(), Vec::new()),
        ("settings.sav".into(), b"settings".to_vec()),
    ]);
    let package =
        codec::package(game(), "account", &rules, &files, 1_790_000_000, &cancel).unwrap();
    let manifest = codec::parse(&package.manifest, game(), "account").unwrap();
    assert_eq!(manifest.chunks.len(), 2);
    assert_eq!(
        manifest.custom_fields["CloudSaveFolder_MAC"],
        rules.cloud_save_folder_mac
    );
    assert_eq!(manifest.custom_fields["CloudSaveFolder"], "{AppData}/Game");
    let chunks: BTreeMap<_, _> = manifest
        .chunks
        .values()
        .map(|chunk| {
            (
                chunk.guid,
                content::decode_chunk(
                    &package.chunks[&chunk.relative_path(manifest.feature_level)],
                    chunk,
                )
                .unwrap(),
            )
        })
        .collect();
    assert_eq!(
        codec::reconstruct(&manifest, &chunks, &cancel).unwrap(),
        files
    );
    assert!(codec::parse(&package.manifest, game(), "wrong-account").is_err());
    let mut damaged = package.manifest.clone();
    *damaged.last_mut().unwrap() ^= 1;
    assert!(codec::parse(&damaged, game(), "account").is_err());
    let chunk = manifest.chunks.values().next().unwrap();
    let mut damaged = package.chunks[&chunk.relative_path(18)].clone();
    *damaged.last_mut().unwrap() ^= 1;
    assert!(content::decode_chunk(&damaged, chunk).is_err());
    let mut damaged = chunks;
    damaged.get_mut(&chunk.guid).unwrap()[0] ^= 1;
    assert!(codec::reconstruct(&manifest, &damaged, &cancel).is_err());
}

#[test]
fn valid_empty_generation_and_unsafe_names_are_distinct() {
    let rules = rules("{Home}/saves");
    let cancel = CancelToken::new();
    let package = codec::package(game(), "account", &rules, &BTreeMap::new(), 0, &cancel).unwrap();
    let manifest = codec::parse(&package.manifest, game(), "account").unwrap();
    assert!(manifest.files.is_empty());
    assert!(manifest.chunks.is_empty());
    assert!(
        codec::reconstruct(&manifest, &BTreeMap::new(), &cancel)
            .unwrap()
            .is_empty()
    );
    for names in [
        vec!["../foreign"],
        vec!["nested", "nested/file"],
        vec!["Slot.sav", "slot.sav"],
        vec!["données.sav", "donne\u{301}es.sav"],
        vec!["Straße.sav", "STRASSE.sav"],
        vec!["a//b"],
        vec!["a\\b"],
    ] {
        let files = names
            .into_iter()
            .map(|name| (name.to_owned(), vec![1]))
            .collect();
        assert!(codec::package(game(), "account", &rules, &files, 0, &cancel).is_err());
    }
}

#[test]
fn encodes_preserved_remote_files_outside_local_filters() {
    let mut rules = rules("{Home}/saves");
    rules.include = vec!["*.sav".into()];
    rules.exclude = vec!["cache/*".into()];
    let files = BTreeMap::from([
        ("slot.sav".into(), b"managed".to_vec()),
        ("cache/remote.bin".into(), b"preserved remote".to_vec()),
    ]);
    let cancel = CancelToken::new();
    let package = codec::package(game(), "account", &rules, &files, 0, &cancel)
        .expect("already selected remote files must survive packaging");
    let manifest = codec::parse(&package.manifest, game(), "account").unwrap();
    let chunks = manifest
        .chunks
        .values()
        .map(|chunk| {
            (
                chunk.guid,
                content::decode_chunk(
                    &package.chunks[&chunk.relative_path(manifest.feature_level)],
                    chunk,
                )
                .unwrap(),
            )
        })
        .collect();
    assert_eq!(
        codec::reconstruct(&manifest, &chunks, &cancel).unwrap(),
        files
    );
}

#[test]
fn timestamps_validate_calendar_dates_and_roundtrip_leap_boundaries() {
    for seconds in [0, 951_782_400, 1_790_000_000, 4_102_444_799] {
        let value = codec::timestamp(seconds).unwrap();
        assert_eq!(
            codec::manifest_time(&format!("manifests/{value}.manifest")).unwrap(),
            seconds
        );
    }
    assert!(codec::manifest_time("manifests/2025.02.29-00.00.00.manifest").is_err());
    assert!(codec::manifest_time("manifests/2024.02.29-24.00.00.manifest").is_err());
    assert!(codec::manifest_time("manifests/2024.2.29-00.00.00.manifest").is_err());
}

#[test]
fn decodes_independently_generated_legendary_cloud_fixture() {
    let bytes = include_bytes!("fixtures/legendary-42f6bdea/save.manifest");
    let chunk_bytes = include_bytes!("fixtures/legendary-42f6bdea/save.chunk");
    let manifest = codec::parse(bytes, game(), "account").unwrap();
    assert_eq!(manifest.feature_level, 18);
    assert_eq!(manifest.build_version, "2026.09.22-00.00.00");
    assert_eq!(
        manifest.custom_fields["CloudSaveFolder_MAC"],
        "{Home}/Données"
    );
    assert_eq!(manifest.custom_fields["CloudSaveFolder"], "{AppData}/Game");
    assert_eq!(manifest.chunks.len(), 1);
    let chunk = manifest.chunks.values().next().unwrap();
    assert_eq!(chunk.guid, [0x01020304, 0x11121314, 0x21222324, 0x31323334]);
    let chunks = BTreeMap::from([(
        chunk.guid,
        content::decode_chunk(chunk_bytes, chunk).unwrap(),
    )]);
    let files = codec::reconstruct(&manifest, &chunks, &CancelToken::new()).unwrap();
    assert_eq!(
        files,
        BTreeMap::from([
            ("Données/slot.sav".into(), b"legendary-byte-proof".to_vec()),
            ("empty.sav".into(), vec![]),
            ("settings.sav".into(), (0..=255).collect()),
        ])
    );
}

fn game() -> CloudGame<'static> {
    CloudGame {
        namespace: "space",
        catalog_item_id: "item",
        app_name: "game",
    }
}
