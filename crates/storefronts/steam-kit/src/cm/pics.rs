//! PICS product metadata: what Steam declares about an app, its depots and
//! launch options, and the rights a license package grants.
use std::collections::BTreeSet;

use super::CmError;
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
}

/// App ownership and depot entitlement are distinct package fields.
#[derive(Clone, Debug, Default)]
pub struct ContentLicenses {
    pub apps: BTreeSet<u32>,
    pub depots: BTreeSet<u32>,
}

impl ContentLicenses {
    pub(super) fn extend_package(&mut self, root: &kv::Kv) {
        for (field, ids) in [("appids", &mut self.apps), ("depotids", &mut self.depots)] {
            if let Some(entries) = root.get(field) {
                ids.extend(entries.children().iter().filter_map(|(_, value)| {
                    value.text()?.parse::<u32>().ok().filter(|id| *id > 0)
                }));
            }
        }
    }
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
pub(crate) fn supports_platform(os_list: &[String], platform: &str) -> bool {
    os_list.is_empty() || os_list.iter().any(|os| os == platform)
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
        rights.extend_package(&package);
        assert_eq!(rights.apps, BTreeSet::from([588650]));
        assert_eq!(
            rights.depots,
            BTreeSet::from([588651, 588652, 588653, 588654])
        );
        assert!(!rights.depots.contains(&1046443));
        rights.extend_package(
            &kv::text(br#""appids" { "0" "99" } "depotids" { "0" "1046443" "1" "0" "2" "bad" }"#)
                .unwrap(),
        );
        assert!(rights.depots.contains(&1046443));
        assert!(rights.apps.contains(&99));
        assert!(!rights.apps.contains(&1046443));
        assert!(!rights.depots.contains(&99));
        assert!(!rights.depots.contains(&0));
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
