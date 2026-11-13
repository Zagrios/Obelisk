//! PICS product metadata: what Steam declares about an app, its depots and
//! launch options, and the rights a license package grants.
use std::collections::{BTreeMap, BTreeSet};

use obelisk_core::Platform;

use super::{CmError, OwnedGameInfo};
use crate::client::PLATFORM;
use crate::kv;

#[derive(Clone, Debug)]
pub struct AppInfo {
    pub app_id: u32,
    pub name: String,
    /// Declared application platforms. Empty means Steam supplied no declaration.
    pub os_list: Vec<String>,
    pub free_to_download: bool,
    pub depots: Vec<DepotInfo>,
    pub launch_configs: Vec<LaunchConfig>,
    /// Complete Steam UFS configuration, including unknown future fields.
    pub cloud: Option<crate::cloud::CloudMetadata>,
    /// The whole app info as PICS returned it.
    pub(crate) tree: kv::Kv,
}

impl AppInfo {
    /// The whole app info as PICS returned it, such as its store details,
    /// associations, assets, depots and launch options, as JSON.
    pub fn metadata(&self) -> serde_json::Value {
        self.tree.get("appinfo").unwrap_or(&self.tree).to_json()
    }

    /// The game as the library lists one the account never played, when it
    /// is a released game, demo or playtest, which Steam's library all lists
    /// as games: not a DLC, tool or soundtrack, nor a game Steam has yet to
    /// release.
    pub(crate) fn unplayed_game(&self) -> Option<OwnedGameInfo> {
        let game = self.common("type").is_some_and(|kind| {
            ["game", "demo", "beta"]
                .iter()
                .any(|listed| kind.eq_ignore_ascii_case(listed))
        });
        let released = self
            .common("releasestate")
            .is_none_or(|state| state.eq_ignore_ascii_case("released"));
        (game && released && !self.name.is_empty()).then(|| OwnedGameInfo {
            app_id: self.app_id,
            name: self.name.clone(),
            icon_hash: self.common("icon").unwrap_or_default().to_owned(),
            last_played: 0,
            playtime_minutes: 0,
            record: serde_json::Value::Null,
        })
    }

    /// Whether its publisher keeps the game out of Family Sharing, which
    /// then stays with the account that bought it.
    pub(crate) fn excluded_from_sharing(&self) -> bool {
        self.common("exfgls") == Some("1")
    }

    /// Whether the Steam store has a page for the app.
    pub(crate) fn on_store(&self) -> bool {
        self.common("store_asset_mtime").is_some()
    }

    fn common(&self, field: &str) -> Option<&str> {
        let root = self.tree.get("appinfo").unwrap_or(&self.tree);
        root.at(&["common", field]).and_then(kv::Kv::text)
    }
}

/// App ownership and depot entitlement are distinct package fields.
#[derive(Clone, Debug, Default)]
pub struct ContentLicenses {
    pub apps: BTreeSet<u32>,
    pub depots: BTreeSet<u32>,
    /// The apps the account's own licenses grant, not a family member's.
    pub own_apps: BTreeSet<u32>,
    /// The apps Steam's free license grants every account, such as Team
    /// Fortress 2 and Valve's Spacewar test app.
    pub free_apps: BTreeSet<u32>,
    /// When the account got each app: the creation of its oldest license
    /// that grants it, in seconds since the Unix epoch.
    pub acquired: BTreeMap<u32, u32>,
    /// The packages PICS does not know, and those it withholds from the
    /// account: what they grant is missing here.
    pub unknown_packages: Vec<u32>,
    pub withheld_packages: Vec<u32>,
}

impl ContentLicenses {
    /// Fails when Steam left a package out: depot and DLC rights read
    /// without it would leave depots out of a download.
    pub(crate) fn complete(&self) -> Result<(), CmError> {
        let code = if !self.unknown_packages.is_empty() {
            "backend.steam.cm.packagesUnavailable"
        } else if !self.withheld_packages.is_empty() {
            "backend.steam.cm.incompletePackageAccess"
        } else {
            return Ok(());
        };
        Err(CmError::Protocol(code.into()))
    }

    /// Adds what a package grants, under a license created at `created`
    /// when Steam told it, which is the account's `own` or a family member's.
    pub(super) fn extend_package(&mut self, root: &kv::Kv, created: Option<u32>, own: bool) {
        for app in package_ids(root, "appids") {
            self.apps.insert(app);
            if own {
                self.own_apps.insert(app);
            }
            if let Some(created) = created {
                let acquired = self.acquired.entry(app).or_insert(created);
                *acquired = (*acquired).min(created);
            }
        }
        self.depots.extend(package_ids(root, "depotids"));
    }

    /// Adds what Steam's free license grants, which is the account's own.
    pub(super) fn extend_free_license(&mut self, root: &kv::Kv, created: Option<u32>) {
        self.extend_package(root, created, true);
        self.free_apps.extend(package_ids(root, "appids"));
    }
}

/// The nonzero IDs a package lists under `field`.
fn package_ids<'a>(root: &'a kv::Kv, field: &str) -> impl Iterator<Item = u32> + 'a {
    root.get(field).into_iter().flat_map(|entries| {
        entries
            .children()
            .iter()
            .filter_map(|(_, value)| value.text()?.parse::<u32>().ok().filter(|id| *id > 0))
    })
}

#[derive(Clone, Debug)]
pub struct DepotInfo {
    pub depot_id: u32,
    pub shared_from_app: Option<u32>,
    pub dlc_app_id: Option<u32>,
    pub os_list: Vec<String>,
    pub os_arch: Option<String>,
    pub language: Option<String>,
    pub public_manifest: Option<u64>,
    pub size_bytes: Option<u64>,
}

/// Steam `oslist` semantics: an empty list means no platform restriction.
pub(crate) fn supports_platform(os_list: &[String], platform: Platform) -> bool {
    os_list.is_empty() || os_list.iter().any(|os| os == PLATFORM.name(platform))
}

#[derive(Clone, Debug)]
pub struct LaunchConfig {
    pub executable: String,
    pub arguments: String,
    pub working_directory: String,
    pub launch_type: String,
    pub os_list: Vec<String>,
    pub os_arch: Option<String>,
}

fn string_at(tree: &kv::Kv, path: &[&str]) -> String {
    tree.at(path)
        .and_then(kv::Kv::text)
        .unwrap_or_default()
        .to_owned()
}

fn optional_at(tree: &kv::Kv, path: &[&str]) -> Option<String> {
    tree.at(path)
        .and_then(kv::Kv::text)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn number_at<T: std::str::FromStr>(tree: &kv::Kv, path: &[&str]) -> Option<T> {
    tree.at(path)
        .and_then(kv::Kv::text)
        .and_then(|value| value.parse().ok())
}

fn os_list(tree: &kv::Kv, path: &[&str]) -> Vec<String> {
    string_at(tree, path)
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

pub(super) fn parse_app(app_id: u32, bytes: &[u8]) -> Result<AppInfo, CmError> {
    let tree = kv::text(bytes)?;
    let root = tree.get("appinfo").unwrap_or(&tree);
    if number_at::<u32>(root, &["appid"]).is_some_and(|id| id != app_id) {
        return Err(CmError::Protocol("backend.steam.cm.unexpectedAppId".into()));
    }
    let mut depots = Vec::new();
    if let Some(entries) = root.get("depots") {
        for (id, depot) in entries.children() {
            let Ok(depot_id) = id.parse::<u32>() else {
                continue;
            };
            depots.push(DepotInfo {
                depot_id,
                shared_from_app: number_at(depot, &["depotfromapp"]),
                dlc_app_id: number_at(depot, &["dlcappid"]),
                os_list: os_list(depot, &["config", "oslist"]),
                os_arch: optional_at(depot, &["config", "osarch"]),
                language: optional_at(depot, &["config", "language"]),
                public_manifest: number_at(depot, &["manifests", "public", "gid"])
                    .or_else(|| number_at(depot, &["manifests", "public"])),
                size_bytes: number_at(depot, &["manifests", "public", "size"]),
            });
        }
    }
    let mut launch_configs = Vec::new();
    if let Some(entries) = root.at(&["config", "launch"]) {
        for (_, launch) in entries.children() {
            let executable = string_at(launch, &["executable"]);
            if executable.is_empty() {
                continue;
            }
            launch_configs.push(LaunchConfig {
                executable,
                arguments: string_at(launch, &["arguments"]),
                working_directory: string_at(launch, &["workingdir"]),
                launch_type: string_at(launch, &["type"]),
                os_list: os_list(launch, &["config", "oslist"]),
                os_arch: optional_at(launch, &["config", "osarch"]),
            });
        }
    }
    let mut declared_os = os_list(root, &["common", "oslist"]);
    if declared_os.is_empty() {
        declared_os = launch_configs
            .iter()
            .flat_map(|launch| launch.os_list.iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
    }
    Ok(AppInfo {
        app_id,
        name: string_at(root, &["common", "name"]),
        os_list: declared_os,
        free_to_download: matches!(
            string_at(root, &["common", "freetodownload"]).as_str(),
            "1" | "true"
        ),
        depots,
        launch_configs,
        cloud: root.get("ufs").map(crate::cloud::CloudMetadata::from),
        tree,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_info_retains_generic_cloud_rules_and_unknown_fields() {
        let app = parse_app(42, br#""appinfo" {
            "appid" "42" "ufs" {
                "quota" "100000" "maxnumfiles" "32"
                "savefiles" { "0" { "root" "MacDocuments" "path" "Game/{64BitSteamID}" "pattern" "*.sav" } }
                "rootoverrides" { "0" { "root" "WinMyDocuments" "useinstead" "MacDocuments" } }
                "future-field" { "nested" "retained" }
                "duplicate" "a" "duplicate" "b"
            }
        }"#).unwrap();
        let cloud = app.cloud.unwrap();
        assert_eq!(cloud.field("quota").and_then(|v| v.text()), Some("100000"));
        assert_eq!(
            cloud
                .field("savefiles")
                .unwrap()
                .field("0")
                .unwrap()
                .field("path")
                .unwrap()
                .text(),
            Some("Game/{64BitSteamID}")
        );
        assert!(cloud.field("rootoverrides").is_some());
        assert_eq!(
            cloud
                .field("future-field")
                .unwrap()
                .field("nested")
                .unwrap()
                .text(),
            Some("retained")
        );
        assert!(cloud.field("duplicate").is_none());
        assert_eq!(
            cloud
                .entries()
                .unwrap()
                .iter()
                .filter(|(key, _)| key == "duplicate")
                .count(),
            2
        );
        let absent = parse_app(42, br#""appinfo" { "appid" "42" }"#).unwrap();
        assert!(absent.cloud.is_none());
        let malformed =
            parse_app(42, br#""appinfo" { "appid" "42" "ufs" "invalid shape" }"#).unwrap();
        assert!(malformed.cloud.unwrap().entries().is_none());
    }

    #[test]
    fn package_entitlements_preserve_app_and_depot_namespaces() {
        let package = kv::text(br#""appids" { "0" "588650" } "depotids" { "0" "588651" "1" "588652" "2" "588653" "3" "588654" }"#).unwrap();
        let mut rights = ContentLicenses::default();
        rights.extend_package(&package, None, true);
        assert_eq!(rights.apps, BTreeSet::from([588650]));
        assert_eq!(
            rights.depots,
            BTreeSet::from([588651, 588652, 588653, 588654])
        );
        assert!(!rights.depots.contains(&1046443));
        rights.extend_package(
            &kv::text(br#""appids" { "0" "99" } "depotids" { "0" "1046443" "1" "0" "2" "bad" }"#)
                .unwrap(),
            None,
            false,
        );
        assert!(rights.depots.contains(&1046443));
        assert!(rights.apps.contains(&99));
        // A family member's license grants downloads and a borrowed game.
        assert_eq!(rights.own_apps, BTreeSet::from([588650]));
        assert!(!rights.apps.contains(&1046443));
        assert!(!rights.depots.contains(&99));
        assert!(!rights.depots.contains(&0));
    }

    #[test]
    fn rights_missing_a_package_are_incomplete() {
        let mut rights = ContentLicenses::default();
        assert!(rights.complete().is_ok());
        rights.withheld_packages.push(3);
        let error = rights.complete().unwrap_err().to_string();
        assert!(error.contains("incompletePackageAccess"), "{error}");
        rights.unknown_packages.push(2);
        let error = rights.complete().unwrap_err().to_string();
        assert!(error.contains("packagesUnavailable"), "{error}");
    }

    #[test]
    fn an_app_is_acquired_with_its_oldest_license() {
        let bundle = kv::text(br#""appids" { "0" "10" "1" "20" }"#).unwrap();
        let single = kv::text(br#""appids" { "0" "10" }"#).unwrap();
        let mut rights = ContentLicenses::default();
        rights.extend_package(&single, Some(2_000), true);
        rights.extend_package(&bundle, Some(1_000), true);
        rights.extend_package(&kv::text(br#""appids" { "0" "30" }"#).unwrap(), None, true);
        assert_eq!(rights.acquired, BTreeMap::from([(10, 1_000), (20, 1_000)]));
        assert!(rights.apps.contains(&30));
    }

    /// App 730 with the given `common` fields.
    fn common_app(common: &str) -> AppInfo {
        let info = format!(r#""appinfo" {{ "appid" "730" "common" {{ {common} }} }}"#);
        parse_app(730, info.as_bytes()).unwrap()
    }

    #[test]
    fn only_a_released_game_demo_or_playtest_joins_the_library_unplayed() {
        let app = |common: &str| common_app(common).unplayed_game();
        let game = app(
            r#""name" "Counter-Strike 2" "type" "Game" "icon" "8dbc71" "releasestate" "released""#,
        )
        .unwrap();
        assert_eq!(game.name, "Counter-Strike 2");
        assert_eq!(game.icon_hash, "8dbc71");
        assert_eq!((game.last_played, game.playtime_minutes), (0, 0));
        assert!(app(r#""name" "Old game" "type" "game""#).is_some());
        assert!(app(r#""name" "Trials Rising Demo" "type" "Demo""#).is_some());
        assert!(app(r#""name" "WARDOGS Playtest" "type" "Beta""#).is_some());
        assert!(app(r#""name" "Soundtrack" "type" "Music""#).is_none());
        assert!(app(r#""name" "Expansion" "type" "DLC""#).is_none());
        assert!(app(r#""name" "Server" "type" "Tool""#).is_none());
        assert!(app(r#""name" "Upcoming" "type" "Game" "releasestate" "prerelease""#).is_none());
        assert!(app(r#""type" "Game""#).is_none());
    }

    #[test]
    fn a_publisher_can_keep_a_game_from_family_sharing() {
        assert!(common_app(r#""name" "Kept Home" "exfgls" "1""#).excluded_from_sharing());
        assert!(!common_app(r#""name" "Lendable" "exfgls" "0""#).excluded_from_sharing());
        assert!(!common_app(r#""name" "Lendable""#).excluded_from_sharing());
    }

    #[test]
    fn parses_download_filters_shared_depots_and_launches() {
        let app = parse_app(42, br#""appinfo" {
            "appid" "42" "common" { "name" "Test" "type" "Game" "FreeToDownload" "1" "oslist" "windows" }
            "config" { "installdir" "Test" "launch" { "0" { "executable" "game.exe" "workingdir" "bin" "type" "default" "config" { "oslist" "windows" } } } }
            "depots" {
                "43" { "config" { "oslist" "windows,macos" "osarch" "64" "language" "french" }
                    "manifests" { "public" { "gid" "123456789012345" "size" "1000" "download" "500" } } }
                "44" { "depotfromapp" "99" "dlcappid" "45" }
                "branches" { "public" { "buildid" "123" } }
            }
        }"#).unwrap();
        assert_eq!(app.name, "Test");
        assert_eq!(app.os_list, ["windows"]);
        assert!(app.free_to_download);
        assert_eq!(app.depots.len(), 2);
        assert_eq!(app.depots[0].public_manifest, Some(123456789012345));
        assert_eq!(app.depots[0].os_list, ["windows", "macos"]);
        assert_eq!(app.depots[0].size_bytes, Some(1000));
        assert_eq!(app.depots[1].shared_from_app, Some(99));
        assert_eq!(app.depots[1].dlc_app_id, Some(45));
        assert_eq!(app.launch_configs[0].executable, "game.exe");
        assert_eq!(app.launch_configs[0].working_directory, "bin");
        assert_eq!(app.launch_configs[0].launch_type, "default");
    }

    #[test]
    fn missing_app_platforms_fall_back_to_explicit_launch_platforms_only() {
        let unknown = parse_app(
            1,
            br#""appinfo" { "depots" { "2" { "manifests" { "public" "3" } } } }"#,
        )
        .unwrap();
        assert!(unknown.os_list.is_empty());
        let with_launch = parse_app(1, br#""appinfo" { "config" { "launch" { "0" { "executable" "game.exe" "config" { "oslist" "Windows" } } } } }"#).unwrap();
        assert_eq!(with_launch.os_list, ["windows"]);
    }
}
