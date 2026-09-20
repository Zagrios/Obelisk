use super::{CloudError, CloudFile, CloudListing};
use crate::kv::Kv;

/// The Steam client's own Cloud namespace, which stores roaming preferences.
pub(crate) const ROAMING_CONFIG_APP: u32 = 7;
pub(crate) const ROAMING_CONFIG_FILE: &str = "sharedconfig.vdf";

/// Actual roaming preferences, separate from the game's metadata/capabilities.
/// Missing remote storage leaves these unknown. Defaults are applied only to a
/// verified, well-formed UserRoamingConfigStore, matching the pinned runtime.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct CloudPreferences {
    pub account_enabled: Option<bool>,
    pub game_enabled: Option<bool>,
    pub revision: u64,
}

/// A session-local cache of verified preferences. A fresh listing is still
/// required on every read; account, app and file identity must all match.
#[derive(Default)]
pub struct CloudPreferencesCache {
    entry: Option<(u64, u32, CloudFile, CloudPreferences)>,
}

impl CloudPreferencesCache {
    pub(crate) async fn read(
        &mut self,
        app_id: u32,
        listing: &CloudListing,
        download: impl AsyncFnOnce(&CloudFile) -> Result<Vec<u8>, CloudError>,
    ) -> Result<CloudPreferences, CloudError> {
        let Some(file) = listing
            .files
            .iter()
            .find(|f| f.name == ROAMING_CONFIG_FILE && f.persist_state == 0)
        else {
            self.entry = None;
            return Ok(CloudPreferences {
                account_enabled: None,
                game_enabled: None,
                revision: listing.change_number,
            });
        };
        if file.size > 16 * 1024 * 1024 {
            return Err(CloudError::Invalid("Cloud preference size"));
        }
        if let Some((account, app, cached, preferences)) = &self.entry
            && *account == listing.steam_id
            && *app == app_id
            && cached == file
        {
            return Ok(CloudPreferences {
                revision: listing.change_number,
                ..preferences.clone()
            });
        }
        let bytes = download(file).await?;
        file.verify(&bytes)?;
        let preferences = parse_preferences(&bytes, app_id, listing.change_number)?;
        self.entry = Some((listing.steam_id, app_id, file.clone(), preferences.clone()));
        Ok(preferences)
    }
}

pub(crate) fn parse_preferences(
    bytes: &[u8],
    app_id: u32,
    revision: u64,
) -> Result<CloudPreferences, CloudError> {
    if app_id == 0 {
        return Err(CloudError::Invalid("Cloud preference app id"));
    }
    let tree =
        crate::kv::text(bytes).map_err(|_| CloudError::Invalid("Cloud preference document"))?;
    let root = child(Some(&tree), "UserRoamingConfigStore")?
        .ok_or(CloudError::Invalid("Cloud preference root"))?;
    object(root)?;
    let software = child(Some(root), "Software")?;
    let valve = child(software, "Valve")?;
    let steam = child(valve, "Steam")?;
    let account_enabled = boolean(steam, "CloudEnabled")?;
    let apps = child(steam, "apps")?;
    let game = child(apps, &app_id.to_string())?;
    let game_enabled = boolean(game, "cloudenabled")?;
    Ok(CloudPreferences {
        account_enabled: Some(account_enabled),
        game_enabled: Some(game_enabled),
        revision,
    })
}

fn object(node: &Kv) -> Result<&[(String, Kv)], CloudError> {
    match node {
        Kv::Object(entries) => Ok(entries),
        _ => Err(CloudError::Invalid("Cloud preference object")),
    }
}
fn child<'a>(node: Option<&'a Kv>, name: &str) -> Result<Option<&'a Kv>, CloudError> {
    let Some(node) = node else { return Ok(None) };
    let mut entries = object(node)?
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case(name));
    let value = entries.next().map(|(_, value)| value);
    if entries.next().is_some() {
        return Err(CloudError::Invalid("duplicate Cloud preference"));
    }
    Ok(value)
}
fn boolean(node: Option<&Kv>, name: &str) -> Result<bool, CloudError> {
    match child(node, name)? {
        None => Ok(true),
        Some(Kv::Value(value)) if value == "1" => Ok(true),
        Some(Kv::Value(value)) if value == "0" => Ok(false),
        _ => Err(CloudError::Invalid("Cloud preference value")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha1::{Digest, Sha1};

    #[tokio::test]
    async fn cached_preferences_refresh_on_content_account_app_and_missing_file_changes() {
        let enabled = br#""UserRoamingConfigStore" {}"#;
        let disabled = br#""UserRoamingConfigStore" { "Software" { "Valve" { "Steam" { "CloudEnabled" "0" } } } }"#;
        let file = |bytes: &[u8]| CloudFile {
            name: "sharedconfig.vdf".into(),
            sha1: crate::content::hex(&Sha1::digest(bytes)),
            size: bytes.len() as u32,
            timestamp: 1,
            persist_state: 0,
            platforms: 7,
            reupload_requested: false,
        };
        let mut listing = CloudListing {
            steam_id: 42,
            app_id: 7,
            change_number: 1,
            files: vec![file(enabled)],
        };
        let mut cache = CloudPreferencesCache::default();
        let mut downloads = 0;
        assert_eq!(
            cache
                .read(10, &listing, async |_| {
                    downloads += 1;
                    Ok(enabled.to_vec())
                })
                .await
                .unwrap()
                .account_enabled,
            Some(true)
        );
        listing.change_number = 2;
        assert_eq!(
            cache
                .read(10, &listing, async |_| {
                    panic!("unchanged config downloaded")
                })
                .await
                .unwrap()
                .revision,
            2
        );
        listing.files[0] = file(disabled);
        assert_eq!(
            cache
                .read(10, &listing, async |_| {
                    downloads += 1;
                    Ok(disabled.to_vec())
                })
                .await
                .unwrap()
                .account_enabled,
            Some(false)
        );
        listing.steam_id = 43;
        cache
            .read(10, &listing, async |_| {
                downloads += 1;
                Ok(disabled.to_vec())
            })
            .await
            .unwrap();
        cache
            .read(11, &listing, async |_| {
                downloads += 1;
                Ok(disabled.to_vec())
            })
            .await
            .unwrap();
        assert_eq!(downloads, 4);
        listing.files.clear();
        let unknown = cache
            .read(11, &listing, async |_| panic!("absent config downloaded"))
            .await
            .unwrap();
        assert_eq!(unknown.account_enabled, None);
        assert_eq!(unknown.game_enabled, None);
        listing.files.push(file(enabled));
        assert!(
            cache
                .read(11, &listing, async |_| Ok(disabled.to_vec()))
                .await
                .is_err()
        );
        assert_eq!(
            cache
                .read(11, &listing, async |_| Ok(enabled.to_vec()))
                .await
                .unwrap()
                .account_enabled,
            Some(true)
        );
    }
    #[test]
    fn remote_roaming_preferences_keep_account_and_game_choices_separate() {
        let data = br#""UserRoamingConfigStore" { "Software" { "Valve" { "Steam" { "CloudEnabled" "0" "apps" { "42" { "cloudenabled" "1" } "43" { "cloudenabled" "0" } } } } } }"#;
        let first = parse_preferences(data, 42, 7).unwrap();
        assert_eq!(first.account_enabled, Some(false));
        assert_eq!(first.game_enabled, Some(true));
        assert_eq!(
            parse_preferences(data, 43, 7).unwrap().game_enabled,
            Some(false)
        );
        assert_eq!(
            parse_preferences(data, 44, 7).unwrap().game_enabled,
            Some(true)
        );
        assert_eq!(
            parse_preferences(br#""UserRoamingConfigStore" {}"#, 42, 7)
                .unwrap()
                .account_enabled,
            Some(true)
        );
    }
    #[test]
    fn missing_malformed_or_ambiguous_documents_never_enable_cloud() {
        for data in [
            "",
            "{}",
            "UserRoamingConfigStore value",
            "UserRoamingConfigStore {} UserRoamingConfigStore {}",
            "UserRoamingConfigStore { Software invalid }",
            "UserRoamingConfigStore { Software { Valve { Steam { CloudEnabled 0 cloudenabled 1 } } } }",
            "UserRoamingConfigStore { Software { Valve { Steam { CloudEnabled yes } } } }",
        ] {
            assert!(parse_preferences(data.as_bytes(), 42, 1).is_err(), "{data}");
        }
    }
}
