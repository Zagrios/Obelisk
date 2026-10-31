//! The rules of a build that no request to a scripted Steam can see: which
//! depots the licenses select, the apps a build reads and asks under, and
//! what names it. What Steam is asked for is pinned through `Client` in
//! `depots_tests`.
use super::*;

fn depot(id: u32, os: &str, language: Option<&str>, dlc: Option<u32>) -> DepotInfo {
    DepotInfo {
        depot_id: id,
        shared_from_app: None,
        dlc_app_id: dlc,
        os_list: if os.is_empty() {
            vec![]
        } else {
            vec![os.into()]
        },
        os_arch: None,
        language: language.map(Into::into),
        public_manifest: Some(1),
        size_bytes: None,
    }
}

/// A depot of an app other than the game's, with `manifest` of its own or none.
fn shared(id: u32, source: u32, manifest: Option<u64>) -> DepotInfo {
    DepotInfo {
        shared_from_app: Some(source),
        public_manifest: manifest,
        ..depot(id, "", None, None)
    }
}

fn licenses(
    apps: impl IntoIterator<Item = u32>,
    depots: impl IntoIterator<Item = u32>,
) -> ContentLicenses {
    ContentLicenses {
        apps: apps.into_iter().collect(),
        depots: depots.into_iter().collect(),
        ..ContentLicenses::default()
    }
}

fn app(depots: Vec<DepotInfo>) -> AppInfo {
    AppInfo {
        app_id: 1,
        name: "Game".into(),
        free_to_download: false,
        os_list: vec!["windows".into(), "macos".into()],
        launch_configs: vec![],
        cloud: None,
        tree: crate::kv::Kv::Object(Vec::new()),
        depots,
    }
}

/// The depots the licenses select, as `Selection::of` lists them.
fn selected_depots<'a>(
    app: &'a AppInfo,
    owned: &ContentLicenses,
    platform: Platform,
    language: &str,
) -> Vec<&'a DepotInfo> {
    Selection::of(app, owned, platform, language).depots
}

/// `app`, a game free to download, at the manifests of its own depots.
fn build_of_own_manifests(app: &AppInfo) -> Build {
    let app = AppInfo {
        free_to_download: true,
        ..app.clone()
    };
    Selection::of(&app, &licenses([], []), Platform::Windows, "english")
        .resolve(&HashMap::new())
        .unwrap()
}

#[test]
fn unlicensed_depot_without_dlc_tag_is_not_selected() {
    // Dead Cells publishes this additional Mac depot without dlcappid.
    // Its base package grants 588652, but not 1046443.
    let mut app = app(vec![
        depot(588652, "macos", None, None),
        depot(1046443, "macos", None, None),
    ]);
    app.app_id = 588650;
    let rights = licenses([588650], [588651, 588652, 588653, 588654]);
    let selected: Vec<_> = selected_depots(&app, &rights, Platform::Mac, "english")
        .iter()
        .map(|d| d.depot_id)
        .collect();
    assert_eq!(selected, [588652]);
    let rights = licenses([588650], [588652, 1046443]);
    assert_eq!(
        selected_depots(&app, &rights, Platform::Mac, "english").len(),
        2
    );
}

#[test]
fn a_selection_is_empty_unless_the_licenses_grant_a_depot_of_the_platform() {
    let app = AppInfo {
        os_list: vec!["windows".into(), "macos".into()],
        ..app(vec![
            depot(2, "windows", None, None),
            depot(3, "macos", None, None),
        ])
    };
    let empty = |owned: &ContentLicenses, platform| {
        Selection::of(&app, owned, platform, "english").is_empty()
    };
    let both = licenses([1], [2, 3]);
    assert!(!empty(&both, Platform::Mac));
    assert!(!empty(&both, Platform::Windows));
    let windows = licenses([1], [2]);
    assert!(empty(&windows, Platform::Mac));
    assert!(!empty(&windows, Platform::Windows));
    let neither = licenses([1], []);
    assert!(empty(&neither, Platform::Mac));
    assert!(empty(&neither, Platform::Windows));
}

#[test]
fn depot_rights_do_not_grant_dlc_app_ownership() {
    let app = app(vec![depot(2, "macos", None, Some(99))]);
    assert!(selected_depots(&app, &licenses([1], [2, 99]), Platform::Mac, "english").is_empty());
    assert_eq!(
        selected_depots(&app, &licenses([1, 99], [2]), Platform::Mac, "english").len(),
        1
    );
}

#[test]
fn free_to_download_depots_do_not_require_package_membership() {
    let mut app = app(vec![depot(2, "macos", None, None)]);
    assert!(selected_depots(&app, &licenses([1], []), Platform::Mac, "english").is_empty());
    app.free_to_download = true;
    assert_eq!(
        selected_depots(&app, &licenses([1], []), Platform::Mac, "english").len(),
        1
    );
}

#[test]
fn unlicensed_depots_cannot_choose_language_or_architecture() {
    let mut english = depot(2, "macos", Some("english"), None);
    english.os_arch = Some("32".into());
    let mut unlicensed = depot(3, "macos", Some("french"), None);
    unlicensed.os_arch = Some("64".into());
    let app = app(vec![english, unlicensed]);
    let rights = licenses([1], [2]);
    let selected = selected_depots(&app, &rights, Platform::Mac, "french");
    assert_eq!(selected.iter().map(|d| d.depot_id).collect::<Vec<_>>(), [2]);
}

#[test]
fn selection_respects_platform_language_and_dlc_ownership() {
    let app = AppInfo {
        app_id: 1,
        name: "Game".into(),
        free_to_download: false,
        os_list: vec!["windows".into(), "macos".into()],
        launch_configs: vec![],
        cloud: None,
        tree: crate::kv::Kv::Object(Vec::new()),
        depots: vec![
            depot(1, "", None, None),
            depot(2, "windows", Some("english"), None),
            depot(3, "windows", Some("french"), None),
            depot(4, "macos", None, None),
            depot(5, "windows", None, Some(99)),
        ],
    };
    let owned = licenses([1], [1, 2, 3, 4, 5]);
    assert_eq!(
        selected_depots(&app, &owned, Platform::Windows, "french")
            .iter()
            .map(|d| d.depot_id)
            .collect::<Vec<_>>(),
        vec![1, 3]
    );
    assert_eq!(
        selected_depots(&app, &owned, Platform::Mac, "french")
            .iter()
            .map(|d| d.depot_id)
            .collect::<Vec<_>>(),
        vec![1, 4]
    );
}

#[test]
fn unavailable_depots_cannot_change_language_or_architecture_selection() {
    let mut english = depot(2, "windows", Some("english"), None);
    english.os_arch = Some("32".into());
    let mut unavailable_dlc = depot(3, "windows", Some("french"), Some(99));
    unavailable_dlc.os_arch = Some("64".into());
    let mut unpublished = depot(4, "windows", Some("french"), None);
    unpublished.public_manifest = None;
    unpublished.os_arch = Some("64".into());
    let mut other_platform = depot(5, "macos", Some("french"), None);
    other_platform.os_arch = Some("64".into());
    let app = app(vec![
        depot(1, "", None, None),
        english,
        unavailable_dlc,
        unpublished,
        other_platform,
    ]);
    let selected: Vec<_> = selected_depots(
        &app,
        &licenses([1], [1, 2, 3, 4, 5]),
        Platform::Windows,
        "french",
    )
    .into_iter()
    .map(|depot| depot.depot_id)
    .collect();
    assert_eq!(selected, [1, 2]);
}

#[test]
fn neutral_depots_cannot_create_unsupported_or_unknown_platform_support() {
    let owned = licenses([1], [1, 2, 3, 4, 5]);
    let mut app = app(vec![
        depot(1, "", None, None),
        depot(2, "windows", None, None),
    ]);
    app.os_list = vec!["windows".into()];
    assert!(selected_depots(&app, &owned, Platform::Mac, "english").is_empty());
    assert_eq!(
        selected_depots(&app, &owned, Platform::Windows, "english").len(),
        2
    );
    app.os_list.clear();
    assert!(selected_depots(&app, &owned, Platform::Mac, "english").is_empty());
    assert_eq!(
        selected_depots(&app, &owned, Platform::Windows, "english").len(),
        2
    );
    app.depots.retain(|depot| depot.os_list.is_empty());
    assert!(selected_depots(&app, &owned, Platform::Windows, "english").is_empty());
}

#[test]
fn unowned_platform_specific_dlc_cannot_establish_unknown_app_platform() {
    let mut app = app(vec![
        depot(1, "", None, None),
        depot(2, "macos", None, Some(99)),
    ]);
    app.os_list.clear();
    assert!(
        selected_depots(
            &app,
            &licenses([1], [1, 2, 3, 4, 5]),
            Platform::Mac,
            "english"
        )
        .is_empty()
    );
    assert_eq!(
        selected_depots(
            &app,
            &licenses([1, 99], [1, 2, 3, 4, 5]),
            Platform::Mac,
            "english"
        )
        .len(),
        2
    );
}

#[test]
fn architecture_is_chosen_among_depots_for_the_selected_language() {
    let mut english = depot(2, "windows", Some("english"), None);
    english.os_arch = Some("64".into());
    let mut french = depot(3, "windows", Some("french"), None);
    french.os_arch = Some("32".into());
    let app = app(vec![english, french]);
    assert_eq!(
        selected_depots(
            &app,
            &licenses([1], [1, 2, 3, 4, 5]),
            Platform::Windows,
            "french"
        )[0]
        .depot_id,
        3
    );
}

#[test]
fn a_windows_game_gets_the_common_redistributables_it_names_without_owning_them() {
    let mut directx = depot(228_990, "windows", None, None);
    directx.shared_from_app = Some(COMMON_REDISTRIBUTABLES);
    directx.public_manifest = None;
    let mut visual_cpp = depot(228_989, "windows", None, None);
    visual_cpp.shared_from_app = Some(COMMON_REDISTRIBUTABLES);
    let mut dotnet = depot(229_005, "windows", None, None);
    dotnet.shared_from_app = Some(COMMON_REDISTRIBUTABLES);
    let mut other = depot(3, "windows", None, None);
    other.shared_from_app = Some(99);
    other.public_manifest = None;
    let app = app(vec![
        depot(2, "windows", None, None),
        directx,
        visual_cpp,
        dotnet,
        other,
    ]);
    let selected: Vec<_> = selected_depots(
        &app,
        &licenses([1], [2, 228_989, 229_005]),
        Platform::Windows,
        "english",
    )
    .iter()
    .map(|depot| depot.depot_id)
    .collect();
    assert_eq!(selected, [2, 228_990, 228_989]);
    assert!(selected_depots(&app, &licenses([1], [2]), Platform::Mac, "english").is_empty());
}

#[test]
fn the_sources_of_shared_depots_are_deduplicated_ascending_and_exclude_the_game_and_depots_with_a_manifest()
 {
    let mut app = app(vec![
        shared(2, 30, None),
        shared(3, 20, None),
        shared(4, 30, None),
        // The game's own app, and an app whose depot has a manifest already.
        shared(5, 1, None),
        shared(6, 40, Some(60)),
        depot(7, "", None, None),
    ]);
    app.free_to_download = true;
    let selection = Selection::of(&app, &licenses([], []), Platform::Windows, "english");
    assert_eq!(selection.depots.len(), 6);
    assert_eq!(selection.sources(), [20, 30]);
    // A depot of the game's own or with a manifest needs no source.
    app.depots = vec![depot(7, "", None, None), shared(5, 1, None)];
    let selection = Selection::of(&app, &licenses([], []), Platform::Windows, "english");
    assert!(selection.sources().is_empty());
}

#[test]
fn a_build_is_each_selected_depot_at_its_public_manifest_in_the_apps_order() {
    let mut app = app(vec![
        // Its own manifest wins over the source app's.
        shared(2, 99, Some(20)),
        shared(3, 99, None),
        // Selected, but nobody published a manifest for it.
        shared(4, 1, None),
        depot(5, "", None, None),
    ]);
    app.free_to_download = true;
    let source = AppInfo {
        app_id: 99,
        depots: vec![
            DepotInfo {
                public_manifest: Some(31),
                ..depot(2, "", None, None)
            },
            DepotInfo {
                public_manifest: Some(30),
                ..depot(3, "", None, None)
            },
        ],
        ..app.clone()
    };
    let selection = Selection::of(&app, &licenses([], []), Platform::Windows, "english");
    assert_eq!(selection.depots.len(), 4);
    let build = selection
        .resolve(&HashMap::from([(99, Arc::new(source))]))
        .unwrap();
    let manifests: Vec<_> = build
        .depots()
        .iter()
        .map(|depot| (depot.depot_id, depot.manifest_id))
        .collect();
    assert_eq!(manifests, [(2, 20), (3, 30), (5, 1)]);
    assert_eq!(
        build.revision(),
        Revision {
            app_id: 1,
            manifests: vec![(2, 20), (3, 30), (5, 1)],
        }
    );
}

#[test]
fn depots_are_keyed_and_authorized_by_the_apps_that_hold_them() {
    let shared_windows = |id, source| DepotInfo {
        os_list: vec!["windows".into()],
        ..shared(id, source, None)
    };
    let held_by = |id, source: u32| AppInfo {
        app_id: source,
        depots: vec![DepotInfo {
            public_manifest: Some(7),
            ..depot(id, "windows", None, None)
        }],
        ..app(vec![])
    };
    let sources = HashMap::from([
        (99, Arc::new(held_by(4, 99))),
        (
            COMMON_REDISTRIBUTABLES,
            Arc::new(held_by(5, COMMON_REDISTRIBUTABLES)),
        ),
    ]);
    let mut app = app(vec![
        depot(2, "windows", None, None),
        // A depot of the game's own, listed as shared from it.
        DepotInfo {
            shared_from_app: Some(1),
            ..depot(3, "windows", None, None)
        },
        shared_windows(4, 99),
        shared_windows(5, COMMON_REDISTRIBUTABLES),
    ]);
    // (depot, whose key, who authorizes)
    let apps = |app: &AppInfo| -> Vec<(u32, u32, u32)> {
        let owned = licenses([1], [2, 3, 4]);
        let build = Selection::of(app, &owned, Platform::Windows, "english")
            .resolve(&sources)
            .unwrap();
        build
            .depots()
            .iter()
            .map(|depot| (depot.depot_id, depot.key_app, depot.authorizing_app))
            .collect()
    };
    assert_eq!(
        apps(&app),
        [
            (2, 1, 1),
            (3, 1, 1),
            // Shared: keyed under the game, authorized by the source app.
            (4, 1, 99),
            // The Common Redistributables are their own app for both.
            (5, COMMON_REDISTRIBUTABLES, COMMON_REDISTRIBUTABLES),
        ]
    );
    // A game free to download authorizes every request through itself, but a
    // redistributable's key still comes from its own app.
    app.free_to_download = true;
    assert_eq!(
        apps(&app),
        [
            (2, 1, 1),
            (3, 1, 1),
            (4, 1, 1),
            (5, COMMON_REDISTRIBUTABLES, 1),
        ]
    );
}

#[test]
fn a_revision_names_the_app_and_each_manifest_in_order() {
    let revision = |app_id: u32, manifests: &[(u32, u64)]| {
        let depots = manifests
            .iter()
            .map(|&(id, manifest)| DepotInfo {
                public_manifest: Some(manifest),
                ..depot(id, "", None, None)
            })
            .collect();
        build_of_own_manifests(&AppInfo {
            app_id,
            ..app(depots)
        })
        .revision()
    };
    let named = revision(1, &[(2, 20), (3, 30)]);
    assert_eq!(
        named,
        Revision {
            app_id: 1,
            manifests: vec![(2, 20), (3, 30)],
        }
    );
    // Two reads of the same data name one build.
    assert_eq!(named, revision(1, &[(2, 20), (3, 30)]));
    assert_eq!(
        HashSet::from([named.clone()]),
        HashSet::from([revision(1, &[(2, 20), (3, 30)])])
    );
    // A moved manifest, another app, a depot more or less, or another order
    // each name another build.
    assert_ne!(named, revision(1, &[(2, 20), (3, 31)]));
    assert_ne!(named, revision(9, &[(2, 20), (3, 30)]));
    assert_ne!(named, revision(1, &[(2, 20)]));
    assert_ne!(named, revision(1, &[(3, 30), (2, 20)]));
}
