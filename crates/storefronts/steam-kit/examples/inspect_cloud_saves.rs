//! Read-only local-discovery validation against an authenticated Cloud backup.
//! Stop the game before invoking this diagnostic. No save or Cloud data changes.
use sha1::{Digest, Sha1};
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::Path,
};
use steam_kit::cloud::{AutoCloudPaths, CloudListing, CloudMetadata};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 5 {
        return Err("usage: inspect_cloud_saves BACKUP_MANIFEST HOME INSTALL REPORT".into());
    }
    let backup: serde_json::Value = serde_json::from_slice(&fs::read(&args[1])?)?;
    let listing: CloudListing = serde_json::from_value(backup["listing"].clone())?;
    let metadata: CloudMetadata = serde_json::from_value(backup["ufs"].clone())?;
    if backup["operation"] != "read_only_cloud_backup"
        || backup["steam_id"].as_u64() != Some(listing.steam_id)
        || backup["app_id"].as_u64() != Some(u64::from(listing.app_id))
    {
        return Err("backup identity mismatch".into());
    }
    let paths = AutoCloudPaths::mac(
        &metadata,
        listing.steam_id,
        Path::new(&args[2]),
        Path::new(&args[3]),
    )?;
    let selected = paths.local_files(&listing.files)?;
    let mut entries = Vec::new();
    let mut matching = 0;
    for file in &selected {
        let (size, digest) = match fs::File::open(&file.path) {
            Ok(input) => {
                let mut bytes = Vec::new();
                input.take(256 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
                if bytes.len() > 256 * 1024 * 1024 {
                    return Err("save exceeds validation bound".into());
                }
                (
                    Some(bytes.len()),
                    Some(
                        Sha1::digest(&bytes)
                            .iter()
                            .map(|byte| format!("{byte:02x}"))
                            .collect::<String>(),
                    ),
                )
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (None, None),
            Err(error) => return Err(error.into()),
        };
        let matches = listing.files.iter().any(|remote| {
            remote.name == file.name
                && Some(remote.size as usize) == size
                && Some(&remote.sha1) == digest.as_ref()
                && remote.platforms == file.platforms
        });
        matching += usize::from(matches);
        entries.push(serde_json::json!({"name":file.name, "path":file.path, "bytes":size, "sha1":digest, "platforms":file.platforms, "matches_remote":matches}));
    }
    let report = serde_json::json!({"operation":"read_only_local_cloud_discovery", "selected":entries, "selected_count":selected.len(), "matches_remote_count":matching});
    let mut output = fs::File::create_new(&args[4])?;
    output.set_permissions(fs::Permissions::from_mode(0o600))?;
    output.write_all(serde_json::to_string_pretty(&report)?.as_bytes())?;
    output.sync_all()?;
    println!(
        "{} files selected; {} match the verified remote backup",
        selected.len(),
        matching
    );
    Ok(())
}
