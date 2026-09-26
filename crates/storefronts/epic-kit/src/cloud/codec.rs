//! Bounded BuildPatch save encoder. Decoding reuses the installer's verified
//! manifest/chunk readers; Cloud adds stricter identity and path constraints.
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
};

use sailover_core::transfer::CancelToken;
use sha1::{Digest, Sha1};

use super::{
    CloudGame, CloudRules, MAX_FILE_BYTES, MAX_FILES, MAX_TOTAL_BYTES, identity, path_identity,
    relative_path,
};
use crate::{
    EpicError, content,
    content_manifest::{self, Chunk, ChunkGuid, ChunkPart, Manifest, ManifestFile},
};

const CHUNK_SIZE: usize = 1024 * 1024;
pub(crate) const MAX_MANIFEST_BYTES: usize = 16 * 1024 * 1024;

pub(crate) struct Package {
    pub manifest_path: String,
    pub manifest: Vec<u8>,
    pub chunks: BTreeMap<String, Vec<u8>>,
}

pub(crate) fn parse(
    bytes: &[u8],
    game: CloudGame<'_>,
    account: &str,
) -> Result<Manifest, EpicError> {
    game.validate()?;
    identity(account)?;
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(EpicError::UnsupportedContent);
    }
    let manifest = content_manifest::parse(bytes)?;
    // Native launcher manifests identify the complete catalog tuple; Legendary
    // writes the compact app/account form. Accept only these exact identities.
    let compact = format!("{}{account}", game.app_name);
    let native = format!(
        "{}:{}:{}:{account}",
        game.namespace, game.catalog_item_id, game.app_name
    );
    if (manifest.app_name != compact && manifest.app_name != native)
        || !manifest.launch_executable.is_empty()
        || !manifest.launch_command.is_empty()
    {
        return Err(EpicError::Protocol);
    }
    if manifest.files.len() > MAX_FILES || manifest.chunks.len() > MAX_FILES {
        return Err(EpicError::UnsupportedContent);
    }
    let mut paths = BTreeSet::new();
    let mut total = 0usize;
    for file in &manifest.files {
        relative_path(&file.filename)?;
        if file.symlink_target.is_some() || file.flags != 0 {
            return Err(EpicError::UnsafePath(file.filename.clone()));
        }
        if !paths.insert(path_identity(&file.filename)) {
            return Err(EpicError::UnsafePath(file.filename.clone()));
        }
        let size = usize::try_from(file.size()).map_err(|_| EpicError::UnsupportedContent)?;
        if size > MAX_FILE_BYTES {
            return Err(EpicError::UnsupportedContent);
        }
        total = total
            .checked_add(size)
            .ok_or(EpicError::UnsupportedContent)?;
        if total > MAX_TOTAL_BYTES {
            return Err(EpicError::UnsupportedContent);
        }
    }
    for path in &paths {
        for (index, _) in path.match_indices('/') {
            if paths.contains(&path[..index]) {
                return Err(EpicError::UnsafePath(path.clone()));
            }
        }
    }
    let expanded = manifest.chunks.values().try_fold(0usize, |sum, chunk| {
        sum.checked_add(chunk.window_size as usize)
            .ok_or(EpicError::UnsupportedContent)
    })?;
    if expanded > MAX_TOTAL_BYTES + CHUNK_SIZE {
        return Err(EpicError::UnsupportedContent);
    }
    Ok(manifest)
}

pub(crate) fn reconstruct(
    manifest: &Manifest,
    chunks: &BTreeMap<ChunkGuid, Vec<u8>>,
    cancel: &CancelToken,
) -> Result<BTreeMap<String, Vec<u8>>, EpicError> {
    let mut result = BTreeMap::new();
    for file in &manifest.files {
        cancel.check()?;
        let mut data = Vec::with_capacity(file.size() as usize);
        for part in &file.parts {
            let chunk = chunks.get(&part.guid).ok_or(EpicError::Integrity)?;
            let end = (part.offset as usize)
                .checked_add(part.size as usize)
                .ok_or(EpicError::Protocol)?;
            data.extend_from_slice(
                chunk
                    .get(part.offset as usize..end)
                    .ok_or(EpicError::Integrity)?,
            );
        }
        if Sha1::digest(&data).as_slice() != file.hash {
            return Err(EpicError::Integrity);
        }
        result.insert(file.filename.clone(), data);
    }
    Ok(result)
}

pub(crate) fn package(
    game: CloudGame<'_>,
    account: &str,
    rules: &CloudRules,
    files: &BTreeMap<String, Vec<u8>>,
    seconds: u64,
    cancel: &CancelToken,
) -> Result<Package, EpicError> {
    game.validate()?;
    identity(account)?;
    rules.validate()?;
    if files.len() > MAX_FILES {
        return Err(EpicError::UnsupportedContent);
    }
    let mut total = 0usize;
    for (name, data) in files {
        // The runtime already selected managed local files and retained remote
        // files outside those filters. Re-filtering here would discard the latter.
        relative_path(name)?;
        if data.len() > MAX_FILE_BYTES {
            return Err(EpicError::UnsupportedContent);
        }
        total = total
            .checked_add(data.len())
            .ok_or(EpicError::UnsupportedContent)?;
        if total > MAX_TOTAL_BYTES {
            return Err(EpicError::UnsupportedContent);
        }
    }
    let build = timestamp(seconds)?;
    let mut manifest = Manifest {
        app_name: format!("{}{account}", game.app_name),
        build_version: build.clone(),
        launch_executable: String::new(),
        launch_command: String::new(),
        files: Vec::new(),
        chunks: BTreeMap::new(),
        feature_level: 18,
        custom_fields: BTreeMap::from([
            (
                "CloudSaveFolder".into(),
                rules.cloud_save_folder.clone().unwrap_or_default(),
            ),
            (
                "CloudSaveFolder_MAC".into(),
                rules.cloud_save_folder_mac.clone(),
            ),
        ]),
    };
    let mut encoded_chunks = BTreeMap::new();
    let mut buffer = Vec::with_capacity(CHUNK_SIZE);
    let mut guid = new_guid();
    for (filename, data) in files {
        cancel.check()?;
        let mut file = ManifestFile {
            filename: filename.clone(),
            symlink_target: None,
            hash: Sha1::digest(data).into(),
            flags: 0,
            parts: Vec::new(),
        };
        let mut remaining = data.as_slice();
        while !remaining.is_empty() {
            cancel.check()?;
            if buffer.len() == CHUNK_SIZE {
                finish_chunk(&mut buffer, guid, &mut manifest, &mut encoded_chunks)?;
                guid = new_guid();
            }
            let size = remaining.len().min(CHUNK_SIZE - buffer.len());
            file.parts.push(ChunkPart {
                guid,
                offset: buffer.len() as u32,
                size: size as u32,
            });
            buffer.extend_from_slice(&remaining[..size]);
            remaining = &remaining[size..];
        }
        manifest.files.push(file);
    }
    if !buffer.is_empty() {
        finish_chunk(&mut buffer, guid, &mut manifest, &mut encoded_chunks)?;
    }
    let bytes = encode_manifest(&manifest)?;
    // Validate the complete generated structure before any service mutation.
    parse(&bytes, game, account)?;
    Ok(Package {
        manifest_path: format!("manifests/{build}.manifest"),
        manifest: bytes,
        chunks: encoded_chunks,
    })
}

fn new_guid() -> ChunkGuid {
    let bytes = *uuid::Uuid::new_v4().as_bytes();
    std::array::from_fn(|index| {
        u32::from_le_bytes(
            bytes[index * 4..index * 4 + 4]
                .try_into()
                .expect("fixed GUID word"),
        )
    })
}

fn finish_chunk(
    buffer: &mut Vec<u8>,
    guid: ChunkGuid,
    manifest: &mut Manifest,
    encoded: &mut BTreeMap<String, Vec<u8>>,
) -> Result<(), EpicError> {
    buffer.resize(CHUNK_SIZE, 0);
    let hash = content::rolling_hash(buffer);
    let sha1 = Sha1::digest(&*buffer).into();
    let identifier: Vec<u8> = guid.iter().flat_map(|word| word.to_le_bytes()).collect();
    let mut compressor =
        flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    compressor.write_all(buffer)?;
    let payload = compressor.finish()?;
    let mut bytes = Vec::with_capacity(66 + payload.len());
    for word in [0xb1fe_3aa2, 3, 66, payload.len() as u32] {
        bytes.extend(word.to_le_bytes());
    }
    for word in guid {
        bytes.extend(word.to_le_bytes());
    }
    bytes.extend(hash.to_le_bytes());
    bytes.push(1);
    bytes.extend(sha1);
    bytes.push(3);
    bytes.extend((CHUNK_SIZE as u32).to_le_bytes());
    bytes.extend(payload);
    let chunk = Chunk {
        guid,
        hash,
        sha1,
        group: (crc32fast::hash(&identifier) % 100) as u8,
        window_size: CHUNK_SIZE as u32,
        file_size: bytes.len() as u64,
    };
    encoded.insert(chunk.relative_path(18), bytes);
    manifest.chunks.insert(guid, chunk);
    buffer.clear();
    Ok(())
}

fn string(output: &mut Vec<u8>, value: &str) {
    // BuildPatch FString uses UTF-16 for non-ASCII text, including macOS paths.
    if value.is_ascii() {
        output.extend((value.len() as u32 + 1).to_le_bytes());
        output.extend(value.as_bytes());
        output.push(0);
    } else {
        let units: Vec<_> = value.encode_utf16().collect();
        output.extend((-(units.len() as i32 + 1)).to_le_bytes());
        for unit in units {
            output.extend(unit.to_le_bytes());
        }
        output.extend([0, 0]);
    }
}

fn section(output: &mut Vec<u8>, payload: Vec<u8>) {
    output.extend((payload.len() as u32 + 5).to_le_bytes());
    output.push(0);
    output.extend(payload);
}

fn encode_manifest(manifest: &Manifest) -> Result<Vec<u8>, EpicError> {
    let mut body = Vec::new();
    let mut meta = 18u32.to_le_bytes().to_vec();
    meta.push(0);
    meta.extend(0u32.to_le_bytes());
    for value in [&manifest.app_name, &manifest.build_version, "", ""] {
        string(&mut meta, value);
    }
    meta.extend(0u32.to_le_bytes());
    for _ in 0..3 {
        string(&mut meta, "");
    }
    section(&mut body, meta);
    let chunks: Vec<_> = manifest.chunks.values().collect();
    let mut list = (chunks.len() as u32).to_le_bytes().to_vec();
    for chunk in &chunks {
        for word in chunk.guid {
            list.extend(word.to_le_bytes());
        }
    }
    for chunk in &chunks {
        list.extend(chunk.hash.to_le_bytes());
    }
    for chunk in &chunks {
        list.extend(chunk.sha1);
    }
    for chunk in &chunks {
        list.push(chunk.group);
    }
    for chunk in &chunks {
        list.extend(chunk.window_size.to_le_bytes());
    }
    for chunk in &chunks {
        list.extend(chunk.file_size.to_le_bytes());
    }
    section(&mut body, list);
    let mut list = (manifest.files.len() as u32).to_le_bytes().to_vec();
    for file in &manifest.files {
        string(&mut list, &file.filename);
    }
    for _ in &manifest.files {
        string(&mut list, "");
    }
    for file in &manifest.files {
        list.extend(file.hash);
    }
    list.extend(std::iter::repeat_n(0, manifest.files.len()));
    for _ in &manifest.files {
        list.extend(0u32.to_le_bytes());
    }
    for file in &manifest.files {
        list.extend((file.parts.len() as u32).to_le_bytes());
        for part in &file.parts {
            list.extend(28u32.to_le_bytes());
            for word in part.guid {
                list.extend(word.to_le_bytes());
            }
            list.extend(part.offset.to_le_bytes());
            list.extend(part.size.to_le_bytes());
        }
    }
    section(&mut body, list);
    let mut custom = (manifest.custom_fields.len() as u32).to_le_bytes().to_vec();
    for key in manifest.custom_fields.keys() {
        string(&mut custom, key);
    }
    for value in manifest.custom_fields.values() {
        string(&mut custom, value);
    }
    section(&mut body, custom);
    if body.len() > MAX_MANIFEST_BYTES - 41 {
        return Err(EpicError::UnsupportedContent);
    }
    // Raw bodies are a supported BuildPatch container form and avoid nested
    // compression ambiguity. The SHA1 still covers the complete manifest body.
    let mut output = 0x44be_c00cu32.to_le_bytes().to_vec();
    output.extend(41u32.to_le_bytes());
    output.extend((body.len() as u32).to_le_bytes());
    output.extend((body.len() as u32).to_le_bytes());
    output.extend(Sha1::digest(&body));
    output.push(0);
    output.extend(18u32.to_le_bytes());
    output.extend(body);
    Ok(output)
}

fn leap(year: u64) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}
fn months(year: u64) -> [u64; 12] {
    [
        31,
        if leap(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ]
}

pub(crate) fn timestamp(seconds: u64) -> Result<String, EpicError> {
    let mut days = seconds / 86_400;
    let mut year = 1970;
    while days >= if leap(year) { 366 } else { 365 } {
        days -= if leap(year) { 366 } else { 365 };
        year += 1;
        if year > 9999 {
            return Err(EpicError::Protocol);
        }
    }
    let mut month = 1;
    for count in months(year) {
        if days < count {
            break;
        }
        days -= count;
        month += 1;
    }
    let time = seconds % 86_400;
    Ok(format!(
        "{year:04}.{month:02}.{:02}-{:02}.{:02}.{:02}",
        days + 1,
        time / 3600,
        time % 3600 / 60,
        time % 60
    ))
}

pub(crate) fn manifest_time(name: &str) -> Result<u64, EpicError> {
    let name = name
        .strip_prefix("manifests/")
        .and_then(|s| s.strip_suffix(".manifest"))
        .ok_or(EpicError::Protocol)?;
    if name.len() != 19 {
        return Err(EpicError::Protocol);
    }
    let values: Vec<u64> = name
        .split(['.', '-'])
        .map(|s| s.parse().map_err(|_| EpicError::Protocol))
        .collect::<Result<_, _>>()?;
    if values.len() != 6 {
        return Err(EpicError::Protocol);
    }
    let [year, month, day, hour, minute, second] =
        <[u64; 6]>::try_from(values).map_err(|_| EpicError::Protocol)?;
    if !(1970..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || day == 0
        || day > months(year)[month as usize - 1]
        || hour > 23
        || minute > 59
        || second > 59
    {
        return Err(EpicError::Protocol);
    }
    let days: u64 = (1970..year)
        .map(|y| if leap(y) { 366 } else { 365 })
        .sum::<u64>()
        + months(year)[..month as usize - 1].iter().sum::<u64>()
        + day
        - 1;
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second;
    if timestamp(seconds)? != name {
        return Err(EpicError::Protocol);
    }
    Ok(seconds)
}

#[cfg(test)]
mod identity_tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        let manifest = Manifest {
            app_name: name.into(),
            build_version: "2026.09.22-00.00.00".into(),
            launch_executable: String::new(),
            launch_command: String::new(),
            files: vec![],
            chunks: BTreeMap::new(),
            feature_level: 18,
            custom_fields: BTreeMap::new(),
        };
        encode_manifest(&manifest).unwrap()
    }

    fn game() -> CloudGame<'static> {
        CloudGame {
            namespace: "space",
            catalog_item_id: "item",
            app_name: "game",
        }
    }

    #[test]
    fn accepts_native_cloud_manifest_identity_from_exact_game_tuple() {
        parse(&fixture("space:item:game:account"), game(), "account")
            .expect("native Cloud identity must decode");
        parse(&fixture("gameaccount"), game(), "account")
            .expect("Legendary Cloud identity remains compatible");
    }

    #[test]
    fn rejects_different_native_tuple_components_and_ambiguous_forms() {
        for name in [
            "foreign:item:game:account",
            "space:foreign:game:account",
            "space:item:foreign:account",
            "space:item:game:foreign",
            "item:game:account",
            "space:item:game:",
            "space:item:game:account:",
            "space::item:game:account",
            "Space:item:game:account",
            "game:account",
            "game_account",
            "gameforeign",
            "foreignaccount",
        ] {
            assert!(parse(&fixture(name), game(), "account").is_err(), "{name}");
        }
        let native = fixture("space:item:game:account");
        for expected in [
            CloudGame {
                namespace: "foreign",
                ..game()
            },
            CloudGame {
                catalog_item_id: "foreign",
                ..game()
            },
            CloudGame {
                app_name: "foreign",
                ..game()
            },
        ] {
            assert!(parse(&native, expected, "account").is_err());
        }
        assert!(parse(&native, game(), "foreign").is_err());
        // Caller identities must not introduce delimiters, including when the
        // manifest uses the legacy representation without namespace/catalog.
        for invalid in [
            CloudGame {
                namespace: "space:extra",
                ..game()
            },
            CloudGame {
                catalog_item_id: "../item",
                ..game()
            },
            CloudGame {
                app_name: "game:extra",
                ..game()
            },
        ] {
            assert!(parse(&fixture("gameaccount"), invalid, "account").is_err());
        }
    }
}
