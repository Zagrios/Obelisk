//! Read only the installed file size from Epic BuildPatch manifests, tolerating
//! encrypted metadata sections that the full parser must reject.
// Format reference: Legendary's models/manifest.py and models/json_manifest.py.
use crate::{
    EpicError,
    content_manifest::{self, Reader, blob_number},
};
use serde::Deserialize;

pub(crate) const MAX_MANIFEST_BYTES: usize = 64 * 1024 * 1024;

pub(crate) fn installation_size(data: &[u8]) -> Result<u64, EpicError> {
    if data.len() > MAX_MANIFEST_BYTES {
        return Err(EpicError::Protocol);
    }
    if data.iter().find(|byte| !byte.is_ascii_whitespace()) != Some(&b'{') {
        return binary_size(data);
    }
    let manifest: JsonManifest = serde_json::from_slice(data).map_err(|_| EpicError::Protocol)?;
    manifest.files.iter().try_fold(0_u64, |total, file| {
        file.parts.iter().try_fold(total, |total, part| {
            total
                .checked_add(blob_number(&part.size)?)
                .ok_or(EpicError::Protocol)
        })
    })
}

fn binary_size(data: &[u8]) -> Result<u64, EpicError> {
    let (header, _) = content_manifest::binary_header(data)?;
    let version = header.version;
    let flags = header.flags;
    let body = content_manifest::binary_body(data, &header)?;
    let mut body = Reader(&body);
    body.section(5)?; // Metadata; its section length handles versioned fields.
    body.section(9)?; // Chunk download sizes are not installed file sizes.
    let mut files = body.section(9)?;
    let file_version = files.byte()?;
    if file_version > 2 {
        return Err(EpicError::Protocol);
    }
    let count = files.count(37)?;
    for _ in 0..count {
        files.string()?;
    }
    for _ in 0..count {
        files.string()?;
    }
    files.take(count.checked_mul(21).ok_or(EpicError::Protocol)?)?; // hashes and flags
    for _ in 0..count {
        for _ in 0..files.count(4)? {
            files.string()?;
        }
    }
    let mut size = 0_u64;
    for _ in 0..count {
        for _ in 0..files.count(28)? {
            let mut part = files.section(28)?;
            part.take(20)?; // GUID and offset within the source chunk.
            size = size
                .checked_add(u64::from(part.u32()?))
                .ok_or(EpicError::Protocol)?;
        }
    }
    if file_version >= 1 {
        for _ in 0..count {
            if files.u32()? != 0 {
                files.take(16)?;
            }
        }
        for _ in 0..count {
            files.string()?;
        }
    }
    if file_version >= 2 {
        files.take(count.checked_mul(32).ok_or(EpicError::Protocol)?)?;
    }
    files.finish()?;
    body.section(9)?; // Custom fields are irrelevant to installation size.
    if version >= 22 && (!body.0.is_empty() || flags & 2 != 0) {
        // Encryption hides names and launch metadata, not the file-part sizes.
        // Reading those sizes does not require decrypting this opaque section.
        body.section(5)?;
    }
    body.finish()?;
    Ok(size)
}

#[derive(Deserialize)]
struct JsonManifest {
    #[serde(rename = "FileManifestList")]
    files: Vec<JsonFile>,
}

#[derive(Deserialize)]
struct JsonFile {
    #[serde(rename = "FileChunkParts")]
    parts: Vec<JsonPart>,
}

#[derive(Deserialize)]
struct JsonPart {
    #[serde(rename = "Size")]
    size: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha1::{Digest, Sha1};
    use std::io::Write;

    #[test]
    fn json_manifest_sums_uncompressed_parts_including_optional_files() {
        let data = br#"{
            "FileManifestList": [
                {"Filename":"game.exe", "FileChunkParts":[{"Size":"000016000000"},{"Size":"001000000000"}]},
                {"Filename":"optional.pak", "InstallTags":["optional"], "FileChunkParts":[{"Size":"002008000000"}]}
            ],
            "ChunkFilesizeList":{"compressed":"001000000000"}
        }"#;
        assert_eq!(installation_size(data).unwrap(), 6147);
    }

    fn section(version: u8, data: Vec<u8>) -> Vec<u8> {
        let mut bytes = ((data.len() + 5) as u32).to_le_bytes().to_vec();
        bytes.push(version);
        bytes.extend(data);
        bytes
    }

    fn string(value: &str) -> Vec<u8> {
        let mut bytes = ((value.len() + 1) as i32).to_le_bytes().to_vec();
        bytes.extend(value.as_bytes());
        bytes.push(0);
        bytes
    }

    fn binary_body(file_version: u8) -> Vec<u8> {
        let mut metadata = 18_u32.to_le_bytes().to_vec();
        metadata.extend([0; 5]); // file-data flag and app ID
        for value in ["app", "1.0", "game.exe", ""] {
            metadata.extend(string(value));
        }
        metadata.extend(0_u32.to_le_bytes()); // no prerequisites
        for _ in 0..3 {
            metadata.extend(string(""));
        }
        let mut body = section(0, metadata);
        body.extend(section(0, 0_u32.to_le_bytes().to_vec())); // no chunk metadata needed
        let mut files = 2_u32.to_le_bytes().to_vec();
        files.extend(string("game.exe"));
        files.extend((-2_i32).to_le_bytes());
        files.extend([0xe9, 0, 0, 0]); // UTF-16 filename
        for _ in 0..2 {
            files.extend(string(""));
        }
        files.extend([0; 40]); // file hashes
        files.extend([0; 2]); // file flags
        files.extend(0_u32.to_le_bytes());
        files.extend(1_u32.to_le_bytes());
        files.extend(string("optional"));
        for sizes in [&[4096_u32, 1][..], &[2050_u32][..]] {
            files.extend((sizes.len() as u32).to_le_bytes());
            for size in sizes {
                files.extend(28_u32.to_le_bytes());
                files.extend([0; 20]); // GUID + chunk offset
                files.extend(size.to_le_bytes());
            }
        }
        if file_version >= 1 {
            files.extend(1_u32.to_le_bytes());
            files.extend([0; 16]);
            files.extend(0_u32.to_le_bytes());
            files.extend(string("application/octet-stream"));
            files.extend(string(""));
        }
        if file_version >= 2 {
            files.extend([0; 64]);
        }
        body.extend(section(file_version, files));
        body.extend(section(0, 0_u32.to_le_bytes().to_vec())); // custom fields
        body
    }

    fn binary_manifest(body: &[u8], compressed: bool) -> Vec<u8> {
        let payload = if compressed {
            let mut encoder =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(body).unwrap();
            encoder.finish().unwrap()
        } else {
            body.to_vec()
        };
        let mut manifest = 0x44be_c00c_u32.to_le_bytes().to_vec();
        manifest.extend(41_u32.to_le_bytes());
        manifest.extend((body.len() as u32).to_le_bytes());
        manifest.extend((payload.len() as u32).to_le_bytes());
        manifest.extend(Sha1::digest(body));
        manifest.push(u8::from(compressed));
        manifest.extend(18_u32.to_le_bytes());
        manifest.extend(payload);
        manifest
    }

    #[test]
    fn binary_manifests_sum_file_parts_for_raw_zlib_and_extended_file_records() {
        for version in 0..=2 {
            for compressed in [false, true] {
                let data = binary_manifest(&binary_body(version), compressed);
                assert_eq!(installation_size(&data).unwrap(), 6147);
            }
        }
    }

    #[test]
    fn encrypted_metadata_does_not_hide_file_sizes_in_modern_manifests() {
        for compressed in [false, true] {
            let mut body = binary_body(2);
            body.extend(section(0, vec![1, 2, 3])); // opaque encrypted names and launch metadata
            let mut manifest = binary_manifest(&body, compressed);
            manifest[4..8].copy_from_slice(&73_u32.to_le_bytes());
            manifest[36] |= 2;
            manifest[37..41].copy_from_slice(&24_u32.to_le_bytes());
            manifest.splice(41..41, [0; 32]); // secret GUID and encryption tag
            assert_eq!(installation_size(&manifest).unwrap(), 6147);
        }
    }

    #[test]
    fn malformed_and_truncated_manifests_never_return_a_partial_size() {
        for compressed in [false, true] {
            let valid = binary_manifest(&binary_body(2), compressed);
            for end in 0..valid.len() {
                assert!(installation_size(&valid[..end]).is_err(), "prefix {end}");
            }
            let mut corrupt_hash = valid.clone();
            corrupt_hash[16] ^= 1;
            assert!(installation_size(&corrupt_hash).is_err());
            let mut huge_body = valid.clone();
            huge_body[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
            assert!(installation_size(&huge_body).is_err());
            let mut extra_body = binary_body(2);
            extra_body.push(0);
            assert!(installation_size(&binary_manifest(&extra_body, compressed)).is_err());
            let mut malformed_string = binary_body(0);
            let file_start =
                u32::from_le_bytes(malformed_string[..4].try_into().unwrap()) as usize + 9;
            malformed_string[file_start + 9..file_start + 13]
                .copy_from_slice(&i32::MIN.to_le_bytes());
            assert!(installation_size(&binary_manifest(&malformed_string, compressed)).is_err());
            let mut excessive_count = binary_body(0);
            excessive_count[file_start + 5..file_start + 9]
                .copy_from_slice(&u32::MAX.to_le_bytes());
            assert!(installation_size(&binary_manifest(&excessive_count, compressed)).is_err());
        }
        for size in [
            "",
            "256",
            "-01",
            "abc",
            "0000",
            "255255255255255255255255000",
        ] {
            let json = serde_json::json!({"FileManifestList":[{"FileChunkParts":[{"Size":size}]}]});
            assert!(installation_size(json.to_string().as_bytes()).is_err());
        }
        let overflow = br#"{"FileManifestList":[{"FileChunkParts":[{"Size":"255255255255255255255255"},{"Size":"001"}]}]}"#;
        assert!(installation_size(overflow).is_err());
    }
}
