//! BuildPatch manifests needed to reconstruct and verify installed files.
//!
//! Format references: Legendary's models/manifest.py and models/json_manifest.py,
//! and EpicManifestParser's UE/FChunkInfo.cs for versioned chunk lists.
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{MapAccess, Visitor},
};
use sha1::{Digest, Sha1};

use crate::{EpicError, manifest::MAX_MANIFEST_BYTES};

const MAX_EXPANDED_BYTES: usize = 256 * 1024 * 1024;
const MAX_ITEMS: usize = 1_000_000;
const MAX_STRING_BYTES: usize = 1024 * 1024;
const MAX_CHUNK_BYTES: u64 = 64 * 1024 * 1024;

pub(crate) type ChunkGuid = [u32; 4];

#[derive(Clone, Debug)]
pub(crate) struct Manifest {
    pub app_name: String,
    pub build_version: String,
    pub launch_executable: String,
    pub launch_command: String,
    pub files: Vec<ManifestFile>,
    pub chunks: BTreeMap<ChunkGuid, Chunk>,
    pub feature_level: u32,
    pub custom_fields: BTreeMap<String, String>,
    pub prerequisite: Option<Prerequisite>,
}

/// The program a build asks to run once before its game first starts, such as
/// a runtime installer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prerequisite {
    pub ids: Vec<String>,
    pub name: String,
    /// Relative to the installation, with Windows separators.
    pub path: String,
    /// Its Windows command line.
    pub arguments: String,
}

impl Prerequisite {
    /// A build without a prerequisite program names none.
    fn named(ids: Vec<String>, name: String, path: String, arguments: String) -> Option<Self> {
        (!path.trim().is_empty()).then_some(Self {
            ids,
            name,
            path,
            arguments,
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ManifestFile {
    pub filename: String,
    pub symlink_target: Option<String>,
    pub hash: [u8; 20],
    pub flags: u8,
    pub parts: Vec<ChunkPart>,
}

impl ManifestFile {
    pub(crate) fn size(&self) -> u64 {
        // Parsing checks this sum before exposing a manifest to the installer.
        self.parts.iter().map(|part| u64::from(part.size)).sum()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ChunkPart {
    pub guid: ChunkGuid,
    pub offset: u32,
    pub size: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct Chunk {
    pub guid: ChunkGuid,
    pub hash: u64,
    pub sha1: [u8; 20],
    pub group: u8,
    pub window_size: u32,
    pub file_size: u64,
}

impl Chunk {
    pub(crate) fn relative_path(&self, feature_level: u32) -> String {
        let [a, b, c, d] = self.guid;
        if feature_level < 3 {
            return format!(
                "Chunks/{:02}/{a:08X}{b:08X}{c:08X}{d:08X}.chunk",
                self.group
            );
        }
        if feature_level >= 22 {
            let guid: Vec<u8> = self
                .guid
                .iter()
                .flat_map(|word| word.to_le_bytes())
                .collect();
            return format!(
                "ChunksV5/plain/{:02}/{}_{}.chunk",
                self.group,
                URL_SAFE_NO_PAD.encode(self.hash.to_le_bytes()),
                URL_SAFE_NO_PAD.encode(guid),
            );
        }
        let directory = match feature_level {
            3..6 => "ChunksV2",
            6..15 => "ChunksV3",
            _ => "ChunksV4",
        };
        format!(
            "{directory}/{:02}/{:016X}_{a:08X}{b:08X}{c:08X}{d:08X}.chunk",
            self.group, self.hash
        )
    }
}

pub(crate) fn parse(data: &[u8]) -> Result<Manifest, EpicError> {
    if data.len() > MAX_MANIFEST_BYTES {
        return Err(EpicError::Protocol);
    }
    let manifest = if data.iter().find(|byte| !byte.is_ascii_whitespace()) == Some(&b'{') {
        json_manifest(data)?
    } else {
        binary_manifest(data)?
    };
    validate(&manifest)?;
    Ok(manifest)
}

fn validate(manifest: &Manifest) -> Result<(), EpicError> {
    // Earlier formats do not carry the chunk SHA1 required by this installer.
    if !(12..=24).contains(&manifest.feature_level) {
        return Err(EpicError::UnsupportedContent);
    }
    if manifest.app_name.is_empty()
        || manifest.files.len() > MAX_ITEMS
        || manifest.chunks.len() > MAX_ITEMS
    {
        return Err(EpicError::Protocol);
    }
    for value in [
        &manifest.app_name,
        &manifest.build_version,
        &manifest.launch_executable,
        &manifest.launch_command,
    ] {
        validate_string(value)?;
    }
    for chunk in manifest.chunks.values() {
        if chunk.guid == [0; 4]
            || chunk.sha1 == [0; 20]
            || chunk.group >= 100
            || chunk.window_size == 0
            || u64::from(chunk.window_size) > MAX_CHUNK_BYTES
            || chunk.file_size < 41
            || chunk.file_size > MAX_CHUNK_BYTES
        {
            return Err(EpicError::Protocol);
        }
    }
    let mut names = BTreeSet::new();
    let mut total_size = 0_u64;
    let mut total_parts = 0_usize;
    for file in &manifest.files {
        validate_string(&file.filename)?;
        if file.filename.is_empty() || !names.insert(&file.filename) || file.flags & !7 != 0 {
            return Err(EpicError::Protocol);
        }
        if let Some(target) = &file.symlink_target {
            validate_string(target)?;
        } else if file.hash == [0; 20] {
            return Err(EpicError::Protocol);
        }
        total_parts = total_parts
            .checked_add(file.parts.len())
            .ok_or(EpicError::Protocol)?;
        if total_parts > MAX_ITEMS {
            return Err(EpicError::Protocol);
        }
        for part in &file.parts {
            let chunk = manifest.chunks.get(&part.guid).ok_or(EpicError::Protocol)?;
            let end = part
                .offset
                .checked_add(part.size)
                .ok_or(EpicError::Protocol)?;
            if part.size == 0 || end > chunk.window_size {
                return Err(EpicError::Protocol);
            }
            total_size = total_size
                .checked_add(u64::from(part.size))
                .ok_or(EpicError::Protocol)?;
        }
    }
    Ok(())
}

fn validate_string(value: &str) -> Result<(), EpicError> {
    if value.len() > MAX_STRING_BYTES || value.contains('\0') {
        return Err(EpicError::Protocol);
    }
    Ok(())
}

/// The fixed BuildPatch container prefix shared by the full parser and the
/// size-only reader. Bounds are checked before any payload is expanded.
pub(crate) struct BinaryHeader {
    pub(crate) flags: u8,
    pub(crate) version: u32,
    header_size: usize,
    expanded_size: usize,
    stored_size: usize,
    hash: [u8; 20],
}

/// Parses the container prefix and returns a reader over the remaining
/// header bytes, which later versions use for encryption metadata.
pub(crate) fn binary_header(data: &[u8]) -> Result<(BinaryHeader, Reader<'_>), EpicError> {
    let mut header = Reader(data);
    if header.u32()? != 0x44be_c00c {
        return Err(EpicError::Protocol);
    }
    let header_size = header.u32()? as usize;
    let expanded_size = header.u32()? as usize;
    let stored_size = header.u32()? as usize;
    let hash = header.array::<20>()?;
    let flags = header.byte()?;
    let version = header.u32()?;
    if flags > 3
        || (flags & 2 != 0 && version < 22)
        || header_size < if version >= 22 { 73 } else { 41 }
        || expanded_size > MAX_EXPANDED_BYTES
        || stored_size > MAX_MANIFEST_BYTES
    {
        return Err(EpicError::Protocol);
    }
    Ok((
        BinaryHeader {
            flags,
            version,
            header_size,
            expanded_size,
            stored_size,
            hash,
        },
        header,
    ))
}

/// Expands (when zlib-compressed) and hash-checks the container body.
pub(crate) fn binary_body<'a>(
    data: &'a [u8],
    header: &BinaryHeader,
) -> Result<std::borrow::Cow<'a, [u8]>, EpicError> {
    let payload = data.get(header.header_size..).ok_or(EpicError::Protocol)?;
    if payload.len() != header.stored_size {
        return Err(EpicError::Protocol);
    }
    let body: std::borrow::Cow<'a, [u8]> = if header.flags & 1 != 0 {
        let mut decoder = flate2::read::ZlibDecoder::new(payload);
        let mut bytes = Vec::new();
        decoder
            .by_ref()
            .take(header.expanded_size as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| EpicError::Protocol)?;
        if decoder.total_in() != payload.len() as u64 {
            return Err(EpicError::Protocol);
        }
        bytes.into()
    } else {
        payload.into()
    };
    if body.len() != header.expanded_size || Sha1::digest(&body).as_slice() != header.hash {
        return Err(EpicError::Protocol);
    }
    Ok(body)
}

fn binary_manifest(data: &[u8]) -> Result<Manifest, EpicError> {
    let (header, mut rest) = binary_header(data)?;
    let version = header.version;
    if version > 24 || header.flags & 2 != 0 {
        return Err(EpicError::UnsupportedContent);
    }
    if version >= 22 {
        // Names and launch metadata cannot be used before authenticated decryption.
        if rest.guid()? != [0; 4] || rest.array::<16>()? != [0; 16] {
            return Err(EpicError::UnsupportedContent);
        }
    }
    let body = binary_body(data, &header)?;
    let mut body = Reader(&body);
    let mut manifest = metadata(body.section(5)?)?;
    manifest.chunks = chunks(body.section(9)?)?;
    manifest.files = files(body.section(9)?)?;
    manifest.custom_fields = custom_fields(body.section(9)?)?;
    if version >= 24 && !body.0.is_empty() {
        // FEncryptedData serializes a versioned byte array. A plain manifest
        // may retain an empty array; nonempty encrypted metadata needs secrets.
        let mut encrypted = body.section(9)?;
        if encrypted.byte()? != 0 {
            return Err(EpicError::UnsupportedContent);
        }
        if encrypted.u32()? != 0 {
            return Err(EpicError::UnsupportedContent);
        }
        encrypted.finish()?;
    }
    body.finish()?;
    Ok(manifest)
}

fn metadata(mut reader: Reader<'_>) -> Result<Manifest, EpicError> {
    let version = reader.byte()?;
    if version > 2 {
        return Err(EpicError::UnsupportedContent);
    }
    let feature_level = reader.u32()?;
    if !(12..=24).contains(&feature_level) {
        return Err(EpicError::UnsupportedContent);
    }
    match reader.byte()? {
        0 => (),
        1 => return Err(EpicError::UnsupportedContent),
        _ => return Err(EpicError::Protocol),
    }
    reader.u32()?; // App ID.
    let app_name = reader.string()?;
    let build_version = reader.string()?;
    let launch_executable = reader.string()?;
    let launch_command = reader.string()?;
    let mut prerequisite_ids = Vec::new();
    for _ in 0..reader.count(4)? {
        prerequisite_ids.push(reader.string()?);
    }
    let prerequisite = Prerequisite::named(
        prerequisite_ids,
        reader.string()?,
        reader.string()?,
        reader.string()?,
    );
    if version >= 1 {
        reader.string()?; // Build ID.
    }
    if version >= 2 {
        reader.string()?; // Uninstaller executable.
        reader.string()?; // Uninstaller arguments.
    }
    reader.finish()?;
    Ok(Manifest {
        app_name,
        build_version,
        launch_executable,
        launch_command,
        files: Vec::new(),
        chunks: BTreeMap::new(),
        feature_level,
        custom_fields: BTreeMap::new(),
        prerequisite,
    })
}

fn chunks(mut reader: Reader<'_>) -> Result<BTreeMap<ChunkGuid, Chunk>, EpicError> {
    let version = reader.byte()?;
    if version > 3 {
        return Err(EpicError::UnsupportedContent);
    }
    // These fields follow the section's data version, independently of the
    // manifest feature level used to choose the chunk download directory.
    let count = reader.count(match version {
        0 => 57,
        1 => 73,
        2 => 77,
        _ => 93,
    })?;
    let mut chunks = Vec::with_capacity(count);
    for _ in 0..count {
        chunks.push(Chunk {
            guid: reader.guid()?,
            hash: 0,
            sha1: [0; 20],
            group: 0,
            window_size: 0,
            file_size: 0,
        });
    }
    for chunk in &mut chunks {
        chunk.hash = reader.u64()?;
    }
    for chunk in &mut chunks {
        chunk.sha1 = reader.array()?;
    }
    for chunk in &mut chunks {
        chunk.group = reader.byte()?;
    }
    for chunk in &mut chunks {
        chunk.window_size = reader.u32()?;
    }
    for chunk in &mut chunks {
        chunk.file_size = reader.u64()?;
    }
    if version >= 1 {
        for _ in 0..count {
            if reader.guid()? != [0; 4] {
                return Err(EpicError::UnsupportedContent);
            }
        }
    }
    if version >= 2 {
        for chunk in &chunks {
            let compressed_size = u64::from(reader.u32()?);
            if compressed_size > chunk.file_size {
                return Err(EpicError::Protocol);
            }
        }
    }
    if version >= 3 {
        for _ in 0..count {
            if reader.array::<16>()? != [0; 16] {
                return Err(EpicError::UnsupportedContent);
            }
        }
    }
    reader.finish()?;
    let mut result = BTreeMap::new();
    for chunk in chunks {
        if result.insert(chunk.guid, chunk).is_some() {
            return Err(EpicError::Protocol);
        }
    }
    Ok(result)
}

fn files(mut reader: Reader<'_>) -> Result<Vec<ManifestFile>, EpicError> {
    let version = reader.byte()?;
    if version > 2 {
        return Err(EpicError::UnsupportedContent);
    }
    let count = reader.count(37)?;
    let mut files = Vec::with_capacity(count);
    for _ in 0..count {
        files.push(ManifestFile {
            filename: reader.string()?,
            symlink_target: None,
            hash: [0; 20],
            flags: 0,
            parts: Vec::new(),
        });
    }
    for file in &mut files {
        let target = reader.string()?;
        file.symlink_target = (!target.is_empty()).then_some(target);
    }
    for file in &mut files {
        file.hash = reader.array()?;
    }
    for file in &mut files {
        file.flags = reader.byte()?;
    }
    for _ in &files {
        for _ in 0..reader.count(4)? {
            reader.string()?;
        } // Install tags.
    }
    let mut total_parts = 0_usize;
    for file in &mut files {
        let count = reader.count(28)?;
        total_parts = total_parts.checked_add(count).ok_or(EpicError::Protocol)?;
        if total_parts > MAX_ITEMS {
            return Err(EpicError::Protocol);
        }
        for _ in 0..count {
            let mut part = reader.section(28)?;
            file.parts.push(ChunkPart {
                guid: part.guid()?,
                offset: part.u32()?,
                size: part.u32()?,
            });
            part.finish()?;
        }
    }
    if version >= 1 {
        for _ in &files {
            if reader.u32()? != 0 {
                reader.take(16)?;
            }
        }
        for _ in &files {
            reader.string()?;
        } // MIME types.
    }
    if version >= 2 {
        reader.take(count.checked_mul(32).ok_or(EpicError::Protocol)?)?;
    }
    reader.finish()?;
    Ok(files)
}

fn custom_fields(mut reader: Reader<'_>) -> Result<BTreeMap<String, String>, EpicError> {
    if reader.byte()? != 0 {
        return Err(EpicError::UnsupportedContent);
    }
    let count = reader.count(8)?;
    let keys = (0..count)
        .map(|_| reader.string())
        .collect::<Result<Vec<_>, _>>()?;
    let mut result = BTreeMap::new();
    for key in keys {
        if result.insert(key, reader.string()?).is_some() {
            return Err(EpicError::Protocol);
        }
    }
    reader.finish()?;
    Ok(result)
}

pub(crate) struct Reader<'a>(pub(crate) &'a [u8]);

impl<'a> Reader<'a> {
    pub(crate) fn take(&mut self, count: usize) -> Result<&'a [u8], EpicError> {
        let (value, remaining) = self.0.split_at_checked(count).ok_or(EpicError::Protocol)?;
        self.0 = remaining;
        Ok(value)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], EpicError> {
        self.take(N)?.try_into().map_err(|_| EpicError::Protocol)
    }
    pub(crate) fn byte(&mut self) -> Result<u8, EpicError> {
        Ok(self.take(1)?[0])
    }
    pub(crate) fn u32(&mut self) -> Result<u32, EpicError> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    fn u64(&mut self) -> Result<u64, EpicError> {
        Ok(u64::from_le_bytes(self.array()?))
    }
    fn guid(&mut self) -> Result<ChunkGuid, EpicError> {
        Ok([self.u32()?, self.u32()?, self.u32()?, self.u32()?])
    }
    pub(crate) fn count(&mut self, minimum_bytes: usize) -> Result<usize, EpicError> {
        let count = self.u32()? as usize;
        if count > MAX_ITEMS || count > self.0.len() / minimum_bytes {
            return Err(EpicError::Protocol);
        }
        Ok(count)
    }
    pub(crate) fn section(&mut self, minimum_bytes: usize) -> Result<Reader<'a>, EpicError> {
        let size = self.u32()? as usize;
        if size < minimum_bytes {
            return Err(EpicError::Protocol);
        }
        Ok(Reader(self.take(size - 4)?))
    }
    pub(crate) fn string(&mut self) -> Result<String, EpicError> {
        let length = self.u32()? as i32;
        if length == 0 {
            return Ok(String::new());
        }
        let unit = if length < 0 { 2 } else { 1 };
        let size = (length.unsigned_abs() as usize)
            .checked_mul(unit)
            .ok_or(EpicError::Protocol)?;
        if size > MAX_STRING_BYTES {
            return Err(EpicError::Protocol);
        }
        let bytes = self.take(size)?;
        if bytes[size - unit..].iter().any(|byte| *byte != 0) {
            return Err(EpicError::Protocol);
        }
        let value = if length < 0 {
            let units: Vec<u16> = bytes[..size - 2]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|value| u16::from_le_bytes(*value))
                .collect();
            String::from_utf16(&units).map_err(|_| EpicError::Protocol)?
        } else {
            String::from_utf8(bytes[..size - 1].to_vec()).map_err(|_| EpicError::Protocol)?
        };
        validate_string(&value)?;
        Ok(value)
    }
    pub(crate) fn finish(self) -> Result<(), EpicError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(EpicError::Protocol)
        }
    }
}

#[derive(Deserialize)]
struct JsonManifest {
    #[serde(rename = "CustomFields", default, deserialize_with = "unique_map")]
    custom_fields: BTreeMap<String, String>,
    #[serde(rename = "ManifestFileVersion", default = "json_version")]
    version: String,
    #[serde(rename = "bIsFileData", default)]
    file_data: bool,
    #[serde(rename = "AppNameString")]
    app_name: String,
    #[serde(rename = "BuildVersionString", default)]
    build_version: String,
    #[serde(rename = "LaunchExeString", default)]
    launch_executable: String,
    #[serde(rename = "LaunchCommand", default)]
    launch_command: String,
    #[serde(rename = "PrereqIds", default)]
    prerequisite_ids: Vec<String>,
    #[serde(rename = "PrereqName", default)]
    prerequisite_name: String,
    #[serde(rename = "PrereqPath", default)]
    prerequisite_path: String,
    #[serde(rename = "PrereqArgs", default)]
    prerequisite_arguments: String,
    #[serde(rename = "FileManifestList")]
    files: Vec<JsonFile>,
    #[serde(rename = "ChunkFilesizeList", deserialize_with = "unique_map")]
    sizes: BTreeMap<String, String>,
    #[serde(rename = "ChunkHashList", deserialize_with = "unique_map")]
    hashes: BTreeMap<String, String>,
    #[serde(rename = "ChunkShaList", deserialize_with = "unique_map")]
    sha1s: BTreeMap<String, String>,
    #[serde(rename = "DataGroupList", deserialize_with = "unique_map")]
    groups: BTreeMap<String, String>,
}

fn json_version() -> String {
    "013000000000".to_owned()
}

#[derive(Deserialize)]
struct JsonFile {
    #[serde(rename = "Filename")]
    filename: String,
    #[serde(rename = "SymlinkTarget", default)]
    symlink_target: String,
    #[serde(rename = "FileHash")]
    hash: String,
    #[serde(rename = "bIsReadOnly", default)]
    read_only: bool,
    #[serde(rename = "bIsCompressed", default)]
    compressed: bool,
    #[serde(rename = "bIsUnixExecutable", default)]
    executable: bool,
    #[serde(rename = "FileChunkParts")]
    parts: Vec<JsonPart>,
}

#[derive(Deserialize)]
struct JsonPart {
    #[serde(rename = "Guid")]
    guid: String,
    #[serde(rename = "Offset")]
    offset: String,
    #[serde(rename = "Size")]
    size: String,
}

fn json_manifest(data: &[u8]) -> Result<Manifest, EpicError> {
    let json: JsonManifest = serde_json::from_slice(data).map_err(|_| EpicError::Protocol)?;
    let feature_level = blob_u32(&json.version)?;
    if json.file_data || !(12..22).contains(&feature_level) {
        return Err(EpicError::UnsupportedContent);
    }
    let mut chunks = BTreeMap::new();
    if json.sizes.len() > MAX_ITEMS
        || json.files.len() > MAX_ITEMS
        || json.hashes.len() != json.sizes.len()
        || json.sha1s.len() != json.sizes.len()
        || json.groups.len() != json.sizes.len()
    {
        return Err(EpicError::Protocol);
    }
    for (guid, size) in json.sizes {
        let chunk = Chunk {
            guid: hex_guid(&guid)?,
            hash: blob_number(json.hashes.get(&guid).ok_or(EpicError::Protocol)?)?,
            sha1: hex_bytes(json.sha1s.get(&guid).ok_or(EpicError::Protocol)?)?,
            group: blob_number(json.groups.get(&guid).ok_or(EpicError::Protocol)?)?
                .try_into()
                .map_err(|_| EpicError::Protocol)?,
            window_size: 1024 * 1024,
            file_size: blob_number(&size)?,
        };
        if chunks.insert(chunk.guid, chunk).is_some() {
            return Err(EpicError::Protocol);
        }
    }
    let mut files = Vec::with_capacity(json.files.len());
    let mut part_count = 0_usize;
    for file in json.files {
        part_count = part_count
            .checked_add(file.parts.len())
            .ok_or(EpicError::Protocol)?;
        if part_count > MAX_ITEMS {
            return Err(EpicError::Protocol);
        }
        let hash = blob_bytes(&file.hash)?;
        let parts = file
            .parts
            .into_iter()
            .map(|part| {
                Ok(ChunkPart {
                    guid: hex_guid(&part.guid)?,
                    offset: blob_u32(&part.offset)?,
                    size: blob_u32(&part.size)?,
                })
            })
            .collect::<Result<Vec<_>, EpicError>>()?;
        files.push(ManifestFile {
            filename: file.filename,
            symlink_target: (!file.symlink_target.is_empty()).then_some(file.symlink_target),
            hash,
            flags: u8::from(file.read_only)
                | (u8::from(file.compressed) << 1)
                | (u8::from(file.executable) << 2),
            parts,
        });
    }
    Ok(Manifest {
        app_name: json.app_name,
        build_version: json.build_version,
        launch_executable: json.launch_executable,
        launch_command: json.launch_command,
        files,
        chunks,
        feature_level,
        custom_fields: json.custom_fields,
        prerequisite: Prerequisite::named(
            json.prerequisite_ids,
            json.prerequisite_name,
            json.prerequisite_path,
            json.prerequisite_arguments,
        ),
    })
}

fn unique_map<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, String>, D::Error> {
    struct UniqueMap;
    impl<'de> Visitor<'de> for UniqueMap {
        type Value = BTreeMap<String, String>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("an object with distinct chunk GUIDs")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut result = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, String>()? {
                if result.len() >= MAX_ITEMS || result.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom(
                        "duplicate or excessive chunk GUIDs",
                    ));
                }
            }
            Ok(result)
        }
    }
    deserializer.deserialize_map(UniqueMap)
}

fn blob_bytes<const N: usize>(value: &str) -> Result<[u8; N], EpicError> {
    if value.len() != N * 3 {
        return Err(EpicError::Protocol);
    }
    let mut bytes = [0; N];
    for (index, digits) in value.as_bytes().as_chunks::<3>().0.iter().enumerate() {
        if !digits.iter().all(u8::is_ascii_digit) {
            return Err(EpicError::Protocol);
        }
        let byte = digits
            .iter()
            .fold(0_u16, |number, digit| number * 10 + u16::from(digit - b'0'));
        bytes[index] = byte.try_into().map_err(|_| EpicError::Protocol)?;
    }
    Ok(bytes)
}

pub(crate) fn blob_number(value: &str) -> Result<u64, EpicError> {
    if value.is_empty() || value.len() > 24 || !value.len().is_multiple_of(3) {
        return Err(EpicError::Protocol);
    }
    let mut padded = value.to_owned();
    padded.extend(std::iter::repeat_n('0', 24 - value.len()));
    Ok(u64::from_le_bytes(blob_bytes(&padded)?))
}

fn blob_u32(value: &str) -> Result<u32, EpicError> {
    blob_number(value)?
        .try_into()
        .map_err(|_| EpicError::Protocol)
}

fn hex_bytes<const N: usize>(value: &str) -> Result<[u8; N], EpicError> {
    if value.len() != N * 2 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(EpicError::Protocol);
    }
    let mut result = [0; N];
    for (index, output) in result.iter_mut().enumerate() {
        *output = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| EpicError::Protocol)?;
    }
    Ok(result)
}

fn hex_guid(value: &str) -> Result<ChunkGuid, EpicError> {
    let bytes: [u8; 16] = hex_bytes(value)?;
    let mut guid = [0; 4];
    for (word, bytes) in guid.iter_mut().zip(bytes.as_chunks::<4>().0.iter()) {
        *word = u32::from_be_bytes(*bytes);
    }
    Ok(guid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const GUID: ChunkGuid = [0x1122_aabb, 0x3344_ccdd, 0x5566_eeff, 0x7788_0099];
    const GUID_TEXT: &str = "1122AABB3344CCDD5566EEFF77880099";
    const GAME_PATH: &str = "Game.app/Contents/MacOS/Game";
    const HASH: u64 = 0x0102_0304_0506_0708;

    fn blob(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:03}")).collect()
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn json_fixture() -> serde_json::Value {
        serde_json::json!({
            "ManifestFileVersion": blob(&13_u32.to_le_bytes()),
            "bIsFileData": false,
            "AppNameString": "ExampleGame",
            "BuildVersionString": "1.2.3",
            "LaunchExeString": GAME_PATH,
            "LaunchCommand": "-epicportal",
            "FileManifestList": [{
                "Filename": GAME_PATH,
                "FileHash": blob(&Sha1::digest(b"game")),
                "bIsUnixExecutable": true,
                "InstallTags": ["optional"],
                "FileChunkParts": [{"Guid":GUID_TEXT,"Offset":blob(&1_u32.to_le_bytes()),"Size":blob(&4_u32.to_le_bytes())}]
            }],
            "ChunkFilesizeList": {GUID_TEXT: blob(&72_u64.to_le_bytes())},
            "ChunkHashList": {GUID_TEXT: blob(&HASH.to_le_bytes())},
            "ChunkShaList": {GUID_TEXT: hex(&Sha1::digest(b"!game?"))},
            "DataGroupList": {GUID_TEXT: "042"},
        })
    }

    fn parse_json(value: &serde_json::Value) -> Result<Manifest, EpicError> {
        parse(&serde_json::to_vec(value).unwrap())
    }

    fn section(version: u8, data: Vec<u8>) -> Vec<u8> {
        let mut result = ((data.len() + 5) as u32).to_le_bytes().to_vec();
        result.push(version);
        result.extend(data);
        result
    }

    fn string(value: &str) -> Vec<u8> {
        let mut result = ((value.len() + 1) as u32).to_le_bytes().to_vec();
        result.extend(value.as_bytes());
        result.push(0);
        result
    }

    fn body(feature_level: u32, metadata_version: u8, file_version: u8) -> Vec<u8> {
        body_with_chunk_version(
            feature_level,
            metadata_version,
            file_version,
            if feature_level >= 22 { 3 } else { 0 },
        )
    }

    fn body_with_chunk_version(
        feature_level: u32,
        metadata_version: u8,
        file_version: u8,
        chunk_version: u8,
    ) -> Vec<u8> {
        let mut metadata = feature_level.to_le_bytes().to_vec();
        metadata.push(0); // Chunk-based manifest.
        metadata.extend(7_u32.to_le_bytes()); // App ID.
        for value in ["ExampleGame", "1.2.3", GAME_PATH, "-epicportal"] {
            metadata.extend(string(value));
        }
        metadata.extend(1_u32.to_le_bytes());
        metadata.extend(string("prerequisite-id"));
        for value in ["runtime", "support/setup.exe", "-silent"] {
            metadata.extend(string(value));
        }
        if metadata_version >= 1 {
            metadata.extend(string("build-id"));
        }
        if metadata_version >= 2 {
            metadata.extend(string("uninstall.exe"));
            metadata.extend(string("/quiet"));
        }
        let mut result = section(metadata_version, metadata);
        let mut chunks = 1_u32.to_le_bytes().to_vec();
        chunks.extend(GUID.iter().flat_map(|word| word.to_le_bytes()));
        chunks.extend(HASH.to_le_bytes());
        chunks.extend(Sha1::digest(b"!game?"));
        chunks.push(42);
        chunks.extend(6_u32.to_le_bytes());
        chunks.extend(if feature_level >= 22 { 104_u64 } else { 72_u64 }.to_le_bytes());
        if chunk_version >= 1 {
            chunks.extend([0; 16]); // No chunk encryption key.
        }
        if chunk_version >= 2 {
            chunks.extend(6_u32.to_le_bytes());
        }
        if chunk_version >= 3 {
            chunks.extend([0; 16]); // No chunk encryption tag.
        }
        result.extend(section(chunk_version, chunks));
        let mut files = 2_u32.to_le_bytes().to_vec();
        files.extend(string(GAME_PATH));
        let unicode: Vec<u16> = "Données.txt".encode_utf16().collect();
        files.extend((-(unicode.len() as i32 + 1)).to_le_bytes());
        files.extend(unicode.iter().flat_map(|unit| unit.to_le_bytes()));
        files.extend([0; 2]);
        files.extend(string(""));
        files.extend(string(""));
        files.extend(Sha1::digest(b"game"));
        files.extend(Sha1::digest(b""));
        files.extend([4, 0]);
        files.extend(1_u32.to_le_bytes());
        files.extend(string("optional"));
        files.extend(0_u32.to_le_bytes());
        files.extend(1_u32.to_le_bytes());
        files.extend(28_u32.to_le_bytes());
        files.extend(GUID.iter().flat_map(|word| word.to_le_bytes()));
        files.extend(1_u32.to_le_bytes());
        files.extend(4_u32.to_le_bytes());
        files.extend(0_u32.to_le_bytes());
        if file_version >= 1 {
            files.extend(1_u32.to_le_bytes());
            files.extend([1; 16]);
            files.extend(0_u32.to_le_bytes());
            files.extend(string("application/octet-stream"));
            files.extend(string("text/plain"));
        }
        if file_version >= 2 {
            files.extend([1; 64]);
        }
        result.extend(section(file_version, files));
        let mut custom = 1_u32.to_le_bytes().to_vec();
        custom.extend(string("platform"));
        custom.extend(string("Mac"));
        result.extend(section(0, custom));
        result
    }

    fn binary(body: &[u8], version: u32, compressed: bool) -> Vec<u8> {
        let payload = if compressed {
            let mut encoder =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(body).unwrap();
            encoder.finish().unwrap()
        } else {
            body.to_vec()
        };
        let mut result = 0x44be_c00c_u32.to_le_bytes().to_vec();
        result.extend(if version >= 22 { 73_u32 } else { 41_u32 }.to_le_bytes());
        result.extend((body.len() as u32).to_le_bytes());
        result.extend((payload.len() as u32).to_le_bytes());
        result.extend(Sha1::digest(body));
        result.push(u8::from(compressed));
        result.extend(version.to_le_bytes());
        if version >= 22 {
            result.extend([0; 32]);
        }
        result.extend(payload);
        result
    }

    #[test]
    fn a_manifest_keeps_its_prerequisite_program_and_a_build_without_one_names_none() {
        let expected = Some(Prerequisite {
            ids: vec!["prerequisite-id".into()],
            name: "runtime".into(),
            path: "support/setup.exe".into(),
            arguments: "-silent".into(),
        });
        let manifest = parse(&binary(&body(20, 2, 2), 20, false)).unwrap();
        assert_eq!(manifest.prerequisite, expected);

        let mut json = json_fixture();
        assert_eq!(parse_json(&json).unwrap().prerequisite, None);
        let object = json.as_object_mut().unwrap();
        object.insert("PrereqIds".into(), serde_json::json!(["prerequisite-id"]));
        object.insert("PrereqName".into(), "runtime".into());
        object.insert("PrereqPath".into(), "support/setup.exe".into());
        object.insert("PrereqArgs".into(), "-silent".into());
        assert_eq!(parse_json(&json).unwrap().prerequisite, expected);
    }

    #[test]
    fn reads_binary_versions_raw_and_zlib_with_unicode_and_empty_files() {
        for feature in [13, 15, 18, 19, 20, 21, 22, 24] {
            for metadata_version in 0..=2 {
                for file_version in 0..=2 {
                    for compressed in [false, true] {
                        let bytes = binary(
                            &body(feature, metadata_version, file_version),
                            feature.max(18),
                            compressed,
                        );
                        let manifest = parse(&bytes).unwrap();
                        assert_eq!(manifest.app_name, "ExampleGame");
                        assert_eq!(manifest.build_version, "1.2.3");
                        assert_eq!(manifest.launch_executable, GAME_PATH);
                        assert_eq!(manifest.launch_command, "-epicportal");
                        assert_eq!(manifest.files[0].size(), 4);
                        assert_eq!(manifest.files[0].hash, Sha1::digest(b"game").as_slice());
                        assert_eq!(manifest.files[0].flags, 4);
                        assert_eq!(manifest.files[0].parts[0].guid, GUID);
                        assert_eq!(manifest.files[0].parts[0].offset, 1);
                        assert_eq!(manifest.files[1].filename, "Données.txt");
                        assert_eq!(manifest.files[1].size(), 0);
                        assert_eq!(manifest.chunks[&GUID].window_size, 6);
                        assert_eq!(
                            manifest.chunks[&GUID].sha1,
                            Sha1::digest(b"!game?").as_slice()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn chunk_list_versions_are_independent_of_manifest_feature_level() {
        for feature in [21, 22, 23, 24] {
            for chunk_version in 0..=3 {
                for compressed in [false, true] {
                    let bytes = binary(
                        &body_with_chunk_version(feature, 2, 2, chunk_version),
                        feature,
                        compressed,
                    );
                    let manifest = parse(&bytes).unwrap();
                    assert_eq!(manifest.chunks[&GUID].window_size, 6);
                    assert_eq!(manifest.files[0].size(), 4);
                }
            }
        }
        let unknown = binary(&body_with_chunk_version(24, 2, 2, 4), 24, false);
        assert!(matches!(
            parse(&unknown),
            Err(EpicError::UnsupportedContent)
        ));
    }

    #[test]
    fn modern_plain_manifests_allow_an_empty_encrypted_metadata_array() {
        for compressed in [false, true] {
            let mut empty = body(24, 2, 2);
            empty.extend(section(0, 0_u32.to_le_bytes().to_vec()));
            assert_eq!(
                parse(&binary(&empty, 24, compressed)).unwrap().files[0].size(),
                4
            );
            // The array belongs to manifest encryption support, introduced at24.
            assert!(parse(&binary(&empty, 21, compressed)).is_err());
            let mut bad_length = body(24, 2, 2);
            bad_length.extend(section(0, vec![0; 5]));
            assert!(parse(&binary(&bad_length, 24, compressed)).is_err());
            let mut ciphertext = body(24, 2, 2);
            let mut data = 1_u32.to_le_bytes().to_vec();
            data.push(1);
            ciphertext.extend(section(0, data));
            assert!(matches!(
                parse(&binary(&ciphertext, 24, compressed)),
                Err(EpicError::UnsupportedContent)
            ));
        }
    }

    #[test]
    fn legacy_json_reads_decimal_blobs_hashes_and_flags() {
        let mut json = json_fixture();
        json.as_object_mut().unwrap().remove("ManifestFileVersion");
        let manifest = parse_json(&json).unwrap();
        assert_eq!(manifest.feature_level, 13);
        assert_eq!(manifest.files[0].size(), 4);
        assert_eq!(manifest.files[0].flags, 4);
        assert_eq!(manifest.files[0].hash, Sha1::digest(b"game").as_slice());
        assert_eq!(manifest.chunks[&GUID].window_size, 1024 * 1024);
        assert_eq!(manifest.chunks[&GUID].hash, HASH);
        assert_eq!(manifest.chunks[&GUID].group, 42);
    }

    #[test]
    fn chunk_paths_follow_manifest_feature_level_and_epic_guid_byte_order() {
        let manifest = parse_json(&json_fixture()).unwrap();
        let chunk = &manifest.chunks[&GUID];
        for (version, directory) in [
            (3, "ChunksV2"),
            (6, "ChunksV3"),
            (14, "ChunksV3"),
            (15, "ChunksV4"),
            (21, "ChunksV4"),
        ] {
            assert_eq!(
                chunk.relative_path(version),
                format!("{directory}/42/0102030405060708_{GUID_TEXT}.chunk")
            );
        }
        assert_eq!(
            chunk.relative_path(0),
            format!("Chunks/42/{GUID_TEXT}.chunk")
        );
        // Independently encoded LE words: BB AA 22 11 DD CC 44 33 FF EE 66 55 99 00 88 77.
        assert_eq!(
            chunk.relative_path(22),
            "ChunksV5/plain/42/CAcGBQQDAgE_u6oiEd3MRDP_7mZVmQCIdw.chunk"
        );
    }

    #[test]
    fn rejects_corrupt_binary_headers_hashes_counts_strings_and_chunk_bounds() {
        for compressed in [false, true] {
            let valid = binary(&body(20, 2, 2), 21, compressed);
            for end in 0..valid.len() {
                assert!(parse(&valid[..end]).is_err(), "prefix {end}");
            }
            for index in [0, 16, 20, 36] {
                let mut corrupt = valid.clone();
                corrupt[index] ^= 0x80;
                assert!(parse(&corrupt).is_err());
            }
            for index in [4, 8, 12] {
                let mut corrupt = valid.clone();
                corrupt[index..index + 4].copy_from_slice(&u32::MAX.to_le_bytes());
                assert!(parse(&corrupt).is_err());
            }
            let mut trailing = body(20, 2, 2);
            trailing.push(0);
            assert!(parse(&binary(&trailing, 21, compressed)).is_err());
            let mut excessive = body(20, 2, 2);
            let chunks_offset = u32::from_le_bytes(excessive[..4].try_into().unwrap()) as usize;
            excessive[chunks_offset + 5..chunks_offset + 9]
                .copy_from_slice(&u32::MAX.to_le_bytes());
            assert!(parse(&binary(&excessive, 21, compressed)).is_err());
            let mut invalid_string = body(20, 2, 2);
            invalid_string[14..18].copy_from_slice(&i32::MIN.to_le_bytes());
            assert!(parse(&binary(&invalid_string, 21, compressed)).is_err());
            let mut bounds = body(20, 2, 2);
            bounds[chunks_offset + 9 + 45..chunks_offset + 9 + 49]
                .copy_from_slice(&3_u32.to_le_bytes());
            assert!(parse(&binary(&bounds, 21, compressed)).is_err());
        }
    }

    #[test]
    fn rejects_unknown_encrypted_and_file_data_formats_explicitly() {
        let mut encrypted = binary(&body(24, 2, 2), 24, true);
        encrypted[36] |= 2;
        assert!(matches!(
            parse(&encrypted),
            Err(EpicError::UnsupportedContent)
        ));
        let mut encrypted_chunks = body(22, 2, 2);
        let chunks_offset = u32::from_le_bytes(encrypted_chunks[..4].try_into().unwrap()) as usize;
        encrypted_chunks[chunks_offset + 9 + 57] = 1;
        assert!(matches!(
            parse(&binary(&encrypted_chunks, 22, false)),
            Err(EpicError::UnsupportedContent)
        ));
        for (metadata_version, file_version) in [(3, 2), (2, 3)] {
            assert!(matches!(
                parse(&binary(
                    &body(24, metadata_version, file_version),
                    24,
                    false
                )),
                Err(EpicError::UnsupportedContent)
            ));
        }
        let mut file_data = json_fixture();
        file_data["bIsFileData"] = true.into();
        assert!(matches!(
            parse_json(&file_data),
            Err(EpicError::UnsupportedContent)
        ));
        let mut old_manifest = json_fixture();
        old_manifest["ManifestFileVersion"] = blob(&11_u32.to_le_bytes()).into();
        assert!(matches!(
            parse_json(&old_manifest),
            Err(EpicError::UnsupportedContent)
        ));
    }

    #[test]
    fn rejects_json_missing_chunks_duplicates_zero_hashes_overflow_and_bad_blobs() {
        let original = json_fixture();
        for (field, value) in [
            ("Offset", blob(&u32::MAX.to_le_bytes())),
            ("Size", blob(&u64::MAX.to_le_bytes())),
            ("Size", "256000000000".to_owned()),
            ("Size", "-01000000000".to_owned()),
            ("Size", "0000".to_owned()),
            ("Size", "000000000000".to_owned()),
            ("Guid", "00000000000000000000000000000001".to_owned()),
        ] {
            let mut json = original.clone();
            json["FileManifestList"][0]["FileChunkParts"][0][field] = value.into();
            assert!(parse_json(&json).is_err(), "{field}");
        }
        let mut duplicate_file = original.clone();
        duplicate_file["FileManifestList"]
            .as_array_mut()
            .unwrap()
            .push(original["FileManifestList"][0].clone());
        assert!(parse_json(&duplicate_file).is_err());
        for field in ["ChunkShaList", "ChunkHashList", "DataGroupList"] {
            let mut json = original.clone();
            json[field].as_object_mut().unwrap().clear();
            assert!(parse_json(&json).is_err());
        }
        for (field, value) in [
            ("ChunkShaList", "00".repeat(20)),
            ("ChunkShaList", "g0".repeat(20)),
            ("DataGroupList", "100".to_owned()),
            ("ChunkFilesizeList", blob(&u64::MAX.to_le_bytes())),
        ] {
            let mut json = original.clone();
            json[field][GUID_TEXT] = value.into();
            assert!(parse_json(&json).is_err());
        }
        let mut zero_file_hash = original.clone();
        zero_file_hash["FileManifestList"][0]["FileHash"] = "000".repeat(20).into();
        assert!(parse_json(&zero_file_hash).is_err());
        let mut case_duplicate = original.clone();
        for field in [
            "ChunkShaList",
            "ChunkHashList",
            "DataGroupList",
            "ChunkFilesizeList",
        ] {
            case_duplicate[field][GUID_TEXT.to_lowercase()] = original[field][GUID_TEXT].clone();
        }
        assert!(parse_json(&case_duplicate).is_err());
        let json = serde_json::to_string(&original).unwrap();
        let duplicate = json.replace(
            &format!("\"DataGroupList\":{{\"{GUID_TEXT}\":\"042\"}}"),
            &format!("\"DataGroupList\":{{\"{GUID_TEXT}\":\"042\",\"{GUID_TEXT}\":\"042\"}}"),
        );
        assert_ne!(json, duplicate);
        assert!(parse(duplicate.as_bytes()).is_err());
    }
}
