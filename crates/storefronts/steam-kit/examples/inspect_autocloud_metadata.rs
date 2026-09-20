//! Parse a read-only Forge metadata report without accessing actual save paths.
use std::{fs, path::Path};
use steam_kit::cloud::{CloudListing, MacCloudPaths};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("metadata report required")?;
    let report: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
    for app in report["apps"].as_array().ok_or("apps missing")? {
        let mac = app["os_list"]
            .as_array()
            .is_some_and(|v| v.iter().any(|os| os == "macos"));
        if !mac {
            continue;
        }
        let metadata = serde_json::from_value(app["ufs"].clone())?;
        let listing: Option<CloudListing> = serde_json::from_value(app["listing"].clone())?;
        // Fixture roots exercise parsing only. No filesystem scan. Existing
        // names use the manifest's authenticated identity for substitutions.
        let result = MacCloudPaths::new(
            &metadata,
            listing.as_ref().map_or(0x011000010000002a, |l| l.steam_id),
            Path::new("/Users/fixture"),
            Path::new("/Games/fixture"),
        );
        let status = match result {
            Err(error) => error.to_string(),
            Ok(paths) => {
                let mut mapped = 0;
                let mut failed = 0;
                for file in listing.into_iter().flat_map(|l| l.files) {
                    match paths.existing_file(&file) {
                        Ok(Some(_)) => mapped += 1,
                        Ok(None) => (),
                        Err(_) => failed += 1,
                    }
                }
                format!("parsed; mapped={mapped}; unmapped={failed}")
            }
        };
        println!("{} {}: {}", app["app_id"], app["name"], status);
    }
    Ok(())
}
