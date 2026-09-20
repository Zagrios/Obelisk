use super::wire;
use crate::content::hex;
use serde::Serialize;
use sha1::{Digest, Sha1};
use std::collections::BTreeSet;
use std::io::{Cursor, Read};

#[derive(Debug, thiserror::Error)]
pub enum CloudError {
    #[error("Steam Cloud response is invalid: {0}")]
    Invalid(&'static str),
    #[error("Steam Cloud returned a delta without a complete baseline")]
    Delta,
    #[error(
        "Steam Cloud file format is unsupported: encrypted={encrypted}, transfer={transfer_size:?}, raw={raw_size}"
    )]
    UnsupportedFormat {
        encrypted: bool,
        transfer_size: Option<u32>,
        raw_size: u32,
    },
    #[error("Steam Cloud changed during synchronization")]
    Changed,
    #[error("Steam Cloud transfer failed")]
    Network,
    #[error("Steam Cloud download host is unsupported: {0}")]
    Host(String),
    #[error("Steam Cloud content failed verification")]
    Integrity,
    #[error("Steam Cloud backup storage failed")]
    Storage,
    #[error("Steam Cloud request exceeded its deadline")]
    Timeout,
    #[error("Steam Cloud upload requires reconciliation before another attempt")]
    Uncertain,
    #[error("Steam did not confirm the uploaded file")]
    NotCommitted,
    #[error("Steam Cloud transfer authorization was revoked")]
    Revoked,
    #[error("{0}")]
    Cm(#[from] crate::cm::CmError),
}

#[derive(Clone, Debug, Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct CloudFile {
    pub name: String,
    pub sha1: String,
    pub size: u32,
    pub timestamp: u64,
    pub persist_state: i32,
    pub platforms: u32,
    pub reupload_requested: bool,
}

impl CloudFile {
    /// Validate bytes against this remote identity.
    pub fn verify(&self, bytes: &[u8]) -> Result<(), CloudError> {
        if bytes.len() != self.size as usize || hex(&Sha1::digest(bytes)) != self.sha1 {
            return Err(CloudError::Integrity);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct CloudListing {
    pub steam_id: u64,
    pub app_id: u32,
    pub change_number: u64,
    pub files: Vec<CloudFile>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CloudQuota {
    pub existing_files: Option<u32>,
    pub existing_bytes: Option<u64>,
    pub max_files: Option<u32>,
    pub max_bytes: Option<u64>,
}

pub(crate) fn listing(
    steam_id: u64,
    app_id: u32,
    raw: wire::ListingResponse,
) -> Result<CloudListing, CloudError> {
    if raw.is_only_delta.unwrap_or(false) {
        return Err(CloudError::Delta);
    }
    let change_number = raw
        .current_change_number
        .ok_or(CloudError::Invalid("change number"))?;
    let mut names = BTreeSet::new();
    let mut files = Vec::new();
    for file in raw.files {
        let prefix = match file.prefix_index {
            None => "",
            Some(index) => raw
                .path_prefixes
                .get(index as usize)
                .ok_or(CloudError::Invalid("path prefix"))?,
        };
        // The path-prefix index is presence-sensitive. These are remote names,
        // never joined to the filesystem path of the backup.
        let separator = if prefix.is_empty() || prefix.ends_with('/') {
            ""
        } else {
            "/"
        };
        let name = format!("{prefix}{separator}{}", file.name);
        if name.is_empty()
            || name.len() > 4096
            || name.chars().any(char::is_control)
            || !names.insert(name.clone())
            || !(0..=2).contains(&file.persist_state)
            || file.persist_state == 0 && file.sha.len() != 20
        {
            return Err(CloudError::Invalid("file identity"));
        }
        files.push(CloudFile {
            name,
            sha1: hex(&file.sha),
            size: file.size,
            timestamp: file.timestamp,
            persist_state: file.persist_state,
            platforms: file.platforms,
            reupload_requested: file.reupload,
        });
    }
    files.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(CloudListing {
        steam_id,
        app_id,
        change_number,
        files,
    })
}

pub(crate) fn download_url(raw: &wire::DownloadResponse) -> Result<reqwest::Url, CloudError> {
    if !raw.https || !raw.path.starts_with('/') || raw.host.contains(['/', '?', '#', '@']) {
        return Err(CloudError::Invalid("download URL"));
    }
    let url = reqwest::Url::parse(&format!("https://{}{}", raw.host, raw.path))
        .map_err(|_| CloudError::Invalid("download URL"))?;
    let host = url.host_str().ok_or(CloudError::Invalid("download host"))?;
    let allowed = [
        "steamusercontent.com",
        "steamcontent.com",
        "steamstatic.com",
        "amazonaws.com",
        "steamuserimages-a.akamaihd.net",
    ]
    .iter()
    .any(|suffix| host == *suffix || host.ends_with(&format!(".{suffix}")))
        || host
            .strip_suffix(".blob.core.windows.net")
            .is_some_and(|account| {
                account.starts_with("steamcloud")
                    && account.bytes().all(|byte| byte.is_ascii_alphanumeric())
            })
        || host
            .strip_suffix(".storage.googleapis.com")
            .is_some_and(|bucket| {
                bucket.starts_with("steamcloud-")
                    && bucket
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            });
    if !allowed {
        return Err(CloudError::Host(host.to_owned()));
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.port_or_known_default() != Some(443)
    {
        return Err(CloudError::Invalid("download host"));
    }
    Ok(url)
}

pub(crate) async fn download(
    http: &reqwest::Client,
    app_id: u32,
    file: &CloudFile,
    raw: wire::DownloadResponse,
) -> Result<Vec<u8>, CloudError> {
    if raw.app_id != Some(app_id)
        || raw.deleted
        || raw.raw_size != Some(file.size)
        || hex(&raw.sha) != file.sha1
        || raw.timestamp != file.timestamp
    {
        return Err(CloudError::Changed);
    }
    if raw.encrypted
        || raw.file_size.is_none_or(|s| s > 256 * 1024 * 1024)
        || file.size > 256 * 1024 * 1024
    {
        return Err(CloudError::UnsupportedFormat {
            encrypted: raw.encrypted,
            transfer_size: raw.file_size,
            raw_size: file.size,
        });
    }
    let mut request = http.get(download_url(&raw)?);
    for header in raw.headers {
        let name = reqwest::header::HeaderName::from_bytes(header.name.as_bytes())
            .map_err(|_| CloudError::Invalid("header name"))?;
        if name == reqwest::header::HOST {
            if !header.value.eq_ignore_ascii_case(&raw.host) {
                return Err(CloudError::Invalid("download Host mismatch"));
            }
            // Azure plans include Host. Reqwest derives the same value from
            // the validated HTTPS URL; do not install an independent authority.
            continue;
        }
        if name == reqwest::header::PROXY_AUTHORIZATION {
            return Err(CloudError::Invalid("download proxy authorization"));
        }
        let value = reqwest::header::HeaderValue::from_str(&header.value)
            .map_err(|_| CloudError::Invalid("header value"))?;
        request = request.header(name, value);
    }
    let response = request.send().await.map_err(|_| CloudError::Network)?;
    if !response.status().is_success() {
        return Err(CloudError::Network);
    }
    let transfer_size = raw.file_size.unwrap() as usize;
    let bytes = crate::client::read_bounded(response, transfer_size)
        .await
        .map_err(|_| CloudError::Network)?;
    if bytes.len() != transfer_size {
        return Err(CloudError::Integrity);
    }
    let bytes = unpack(file, bytes)?;
    file.verify(&bytes)?;
    Ok(bytes)
}

fn unpack(file: &CloudFile, transfer: Vec<u8>) -> Result<Vec<u8>, CloudError> {
    if transfer.len() == file.size as usize {
        return Ok(transfer);
    }
    let mut archive =
        zip::ZipArchive::new(Cursor::new(transfer)).map_err(|_| CloudError::Integrity)?;
    if archive.len() != 1 {
        return Err(CloudError::Integrity);
    }
    let entry = archive.by_index(0).map_err(|_| CloudError::Integrity)?;
    if entry.is_dir() || entry.encrypted() || entry.size() != u64::from(file.size) {
        return Err(CloudError::Integrity);
    }
    // The archive entry's name is intentionally ignored. It can never escape
    // into the filesystem; decoded bytes are verified and stored by index.
    let mut bytes = Vec::new();
    entry
        .take(u64::from(file.size) + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| CloudError::Integrity)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn complete_lists_expand_prefixes_but_deltas_never_become_full_sets() {
        let raw = wire::ListingResponse {
            current_change_number: Some(10),
            path_prefixes: vec!["%MacDocuments%Game/".into()],
            files: vec![wire::FileInfo {
                name: "save.dat".into(),
                prefix_index: Some(0),
                sha: vec![1; 20],
                ..Default::default()
            }],
            ..Default::default()
        };
        let full = listing(123, 42, raw.clone()).unwrap();
        assert_eq!(full.files[0].name, "%MacDocuments%Game/save.dat");
        assert!(matches!(
            listing(
                123,
                42,
                wire::ListingResponse {
                    is_only_delta: Some(true),
                    ..raw.clone()
                }
            ),
            Err(CloudError::Delta)
        ));
        let mut invalid = raw;
        invalid.files[0].prefix_index = Some(2);
        assert!(listing(123, 42, invalid).is_err());
    }
    #[test]
    fn compressed_downloads_require_one_bounded_verified_entry() {
        use std::io::Write;
        let payload = vec![b'x'; 4096];
        let file = CloudFile {
            name: "save.dat".into(),
            sha1: hex(&Sha1::digest(&payload)),
            size: payload.len() as u32,
            timestamp: 0,
            persist_state: 0,
            platforms: 7,
            reupload_requested: false,
        };
        for entries in [1, 2] {
            let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
            for index in 0..entries {
                zip.start_file(
                    format!("../../outside-{index}"),
                    zip::write::SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Deflated),
                )
                .unwrap();
                zip.write_all(&payload).unwrap();
            }
            let encoded = zip.finish().unwrap().into_inner();
            assert!(encoded.len() < payload.len());
            let result = unpack(&file, encoded);
            if entries == 1 {
                let bytes = result.unwrap();
                file.verify(&bytes).unwrap();
                assert_eq!(bytes, payload);
            } else {
                assert!(result.is_err());
            }
        }
    }

    #[test]
    fn download_endpoints_and_content_are_checked_without_exposing_signed_urls() {
        let mut response = wire::DownloadResponse {
            https: true,
            host: "steamcloud.s3.amazonaws.com".into(),
            path: "/file?signature=fixture".into(),
            ..Default::default()
        };
        assert!(download_url(&response).is_ok());
        response.host = "steamcloudparis.blob.core.windows.net".into();
        assert!(download_url(&response).is_ok());
        response.host = "steamcloud-paris.storage.googleapis.com".into();
        assert!(download_url(&response).is_ok());
        for host in [
            "localhost",
            "127.0.0.1",
            "steamusercontent.com.evil.invalid",
            "steamusercontent.com@evil.invalid",
            "steamcloudparis.blob.core.windows.net.evil.invalid",
            "unrelated.blob.core.windows.net",
            "steamcloud.paris.blob.core.windows.net",
        ] {
            response.host = host.into();
            assert!(download_url(&response).is_err());
        }
        let file = CloudFile {
            name: "save".into(),
            size: 4,
            sha1: hex(&Sha1::digest(b"good")),
            timestamp: 0,
            persist_state: 0,
            platforms: 7,
            reupload_requested: false,
        };
        assert!(file.verify(b"good").is_ok());
        assert!(file.verify(b"evil").is_err());
        assert!(file.verify(b"goo").is_err());
    }
}
