//! Account-fenced EGS Cloud transport. Each metadata request refreshes OAuth
//! independently; a rejected token never replays a complete upload transaction.
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{SystemTime, UNIX_EPOCH},
};

use obelisk_core::{
    Platform,
    install::hex,
    transfer::{CancelToken, Window},
};
use reqwest::Url;
use serde::Deserialize;
use sha1::{Digest, Sha1};

use super::{Client, endpoint, validate_identity};
use crate::{
    EpicError,
    auth::{bounded_bytes, check_status, credential_free},
    cloud::{CloudGame, CloudListing, CloudRules, codec, identity, relative_path},
    content,
    content_manifest::{Chunk, ChunkGuid},
};

const MAX_METADATA_BYTES: usize = 16 * 1024 * 1024;
const MAX_CHUNK_BYTES: usize = 64 * 1024 * 1024 + 98;
// The service has been observed to truncate at 1000 entries. Until pagination is
// established, a full page is ambiguous and must never be treated as complete.
const INVENTORY_LIMIT: usize = 1000;
const LINK_BATCH: usize = 100;
// Chunks are 1 MiB, so a large save is hundreds of round trips. Signed-link
// transfers carry no Authorization and no ordering, so they overlap freely.
const TRANSFERS: Window = Window {
    width: 8,
    bytes: u64::MAX,
};

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    #[serde(default, deserialize_with = "nullable_metadata")]
    hash: String,
    #[serde(default, deserialize_with = "nullable_metadata")]
    last_modified: String,
    read_link: Option<String>,
    write_link: Option<String>,
}

// Link responses legitimately contain null revision metadata. Inventories still
// require both nonempty values in list(); other JSON scalar types remain invalid.
fn nullable_metadata<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<String, D::Error> {
    Option::<String>::deserialize(deserializer).map(Option::unwrap_or_default)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Listing {
    #[serde(deserialize_with = "unique_entries")]
    files: BTreeMap<String, Entry>,
    #[serde(default)]
    next_cursor: Option<serde_json::Value>,
    #[serde(default)]
    has_more: bool,
}

fn unique_entries<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, Entry>, D::Error> {
    struct Entries;
    impl<'de> serde::de::Visitor<'de> for Entries {
        type Value = BTreeMap<String, Entry>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("unique cloud file paths")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> Result<Self::Value, M::Error> {
            let mut files = BTreeMap::new();
            while let Some((name, entry)) = map.next_entry::<String, Entry>()? {
                if files.len() >= INVENTORY_LIMIT || files.insert(name, entry).is_some() {
                    return Err(serde::de::Error::custom(
                        "duplicate or excessive cloud files",
                    ));
                }
            }
            Ok(files)
        }
    }
    deserializer.deserialize_map(Entries)
}

impl Client {
    pub async fn cloud_configuration(
        &self,
        namespace: &str,
        catalog_item_id: &str,
        app_name: &str,
        platform: Platform,
    ) -> Result<Option<CloudRules>, EpicError> {
        let attributes = self
            .catalog_attributes(namespace, catalog_item_id, app_name)
            .await?;
        CloudRules::from_attributes(&attributes, platform)
    }

    /// What the catalog says of the game's Cloud, to tell why a game has
    /// none: each attribute whose name mentions it, with its value.
    pub async fn cloud_attributes(
        &self,
        namespace: &str,
        catalog_item_id: &str,
        app_name: &str,
    ) -> Result<BTreeMap<String, String>, EpicError> {
        let attributes = self
            .catalog_attributes(namespace, catalog_item_id, app_name)
            .await?;
        Ok(attributes
            .into_iter()
            .filter(|(name, _)| name.to_ascii_lowercase().contains("cloud"))
            .map(|(name, value)| {
                let text = match value.get("value") {
                    Some(serde_json::Value::String(text)) => text.clone(),
                    Some(other) => other.to_string(),
                    None => value.to_string(),
                };
                (name, text.chars().take(400).collect())
            })
            .collect())
    }

    /// The custom attributes of the game's catalog item.
    async fn catalog_attributes(
        &self,
        namespace: &str,
        catalog_item_id: &str,
        app_name: &str,
    ) -> Result<BTreeMap<String, serde_json::Value>, EpicError> {
        validate_identity(namespace, catalog_item_id, app_name)?;
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Catalog {
            #[serde(default)]
            custom_attributes: BTreeMap<String, serde_json::Value>,
        }
        self.with_access_token(|token| async move {
            self.ensure_owned(&token, namespace, catalog_item_id, app_name)
                .await?;
            let mut url = endpoint(
                &self.api.catalog,
                &[
                    "catalog",
                    "api",
                    "shared",
                    "namespace",
                    namespace,
                    "bulk",
                    "items",
                ],
            )?;
            url.query_pairs_mut()
                .append_pair("id", catalog_item_id)
                .append_pair("includeDLCDetails", "true")
                .append_pair("includeMainGameDetails", "true")
                .append_pair("country", "US")
                .append_pair("locale", "en");
            let mut catalog: BTreeMap<String, Catalog> = self.api.get(url, &token).await?;
            let item = catalog.remove(catalog_item_id).ok_or(EpicError::Protocol)?;
            Ok(item.custom_attributes)
        })
        .await
    }

    /// Lists the inventory and reads its latest manifest. An empty inventory
    /// and a valid empty manifest are supported; malformed or incomplete data
    /// is an error.
    pub async fn cloud_list(
        &self,
        game: CloudGame<'_>,
        cancel: &CancelToken,
    ) -> Result<CloudListing, EpicError> {
        let account = self.credentials().await?.account_id;
        let cloud = Cloud {
            client: self,
            game,
            account: &account,
            cancel,
        };
        cloud.authorize().await?;
        let inventory = cloud.list().await?;
        cloud.listing(&inventory).await
    }

    /// The inventory's revision now, for the account `listing` was read for.
    pub async fn cloud_revision(
        &self,
        game: CloudGame<'_>,
        listing: &CloudListing,
        cancel: &CancelToken,
    ) -> Result<String, EpicError> {
        let cloud = Cloud {
            client: self,
            game,
            account: &listing.account,
            cancel,
        };
        cloud.validate()?;
        let revision = cloud.revision(&cloud.list().await?);
        cloud.fence().await?;
        Ok(revision)
    }

    /// Reads `names` of `listing`'s generation: only the chunks they use, in
    /// one window across link batches, each file checked against its SHA-1.
    pub async fn cloud_fetch(
        &self,
        game: CloudGame<'_>,
        listing: &CloudListing,
        names: &BTreeSet<String>,
        cancel: &CancelToken,
    ) -> Result<BTreeMap<String, Vec<u8>>, EpicError> {
        let cloud = Cloud {
            client: self,
            game,
            account: &listing.account,
            cancel,
        };
        cloud.validate()?;
        cloud.fetch(listing, Some(names)).await
    }

    /// Optimistic publication: unchanged baseline, complete chunk upload,
    /// manifest last, full readback, then stable inventory. EGS exposes no proven
    /// atomic compare-and-swap: another client can still race between checks.
    /// Returns the complete listing that confirmed it.
    pub async fn cloud_upload(
        &self,
        game: CloudGame<'_>,
        rules: &CloudRules,
        files: &BTreeMap<String, Vec<u8>>,
        expected_revision: &str,
        cancel: &CancelToken,
    ) -> Result<CloudListing, EpicError> {
        let account = self.credentials().await?.account_id;
        let cloud = Cloud {
            client: self,
            game,
            account: &account,
            cancel,
        };
        cloud.authorize().await?;
        let before = cloud.list().await?;
        if cloud.revision(&before) != expected_revision {
            return Err(EpicError::CloudConflict);
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| EpicError::Protocol)?
            .as_secs();
        let latest = before
            .keys()
            .map(|name| codec::manifest_time(name))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .max()
            .unwrap_or(0);
        // Avoid reusing a second-resolution manifest name, without silently
        // accepting a remote clock far in the future.
        if latest > now.saturating_add(300) {
            return Err(EpicError::CloudConflict);
        }
        let seconds = now.max(latest.saturating_add(1));
        cancel.check()?;
        let mut package = codec::package(game, &account, rules, files, seconds, cancel)?;
        let chunks = std::mem::take(&mut package.chunks);
        let batches = Batch::split(chunks.keys().cloned().collect());
        TRANSFERS
            .run(
                chunks.into_iter().enumerate(),
                |(_, (_, chunk))| chunk.len() as u64,
                || cancel.check().map_err(EpicError::from),
                |(index, (name, chunk))| {
                    let batch = &batches[index / LINK_BATCH];
                    let cloud = &cloud;
                    async move {
                        let links = batch.links(cloud).await?;
                        let link = links
                            .get(&name)
                            .and_then(|entry| entry.write_link.as_deref())
                            .ok_or(EpicError::Protocol)?;
                        cloud.put(link, chunk).await
                    }
                },
                |()| Ok(()),
            )
            .await?;
        // Request the manifest link only after every chunk PUT succeeds.
        // Content integrity is established by the complete readback below.
        if cloud.revision(&cloud.list().await?) != expected_revision {
            return Err(EpicError::CloudConflict);
        }
        let links = cloud
            .links(std::slice::from_ref(&package.manifest_path))
            .await?;
        let link = links
            .get(&package.manifest_path)
            .and_then(|entry| entry.write_link.as_deref())
            .ok_or(EpicError::Protocol)?;
        cloud
            .put(link, std::mem::take(&mut package.manifest))
            .await?;
        let after = cloud.list().await?;
        if !after.contains_key(&package.manifest_path)
            || cloud.revision_excluding(&after, &package.manifest_path) != expected_revision
        {
            return Err(EpicError::CloudConflict);
        }
        if latest_manifest(&after)?.map(|(name, _)| name) != Some(package.manifest_path.as_str()) {
            return Err(EpicError::CloudConflict);
        }
        let listing = cloud.listing(&after).await?;
        if cloud.fetch(&listing, None).await? != *files {
            return Err(EpicError::Integrity);
        }
        if cloud.revision(&cloud.list().await?) != listing.revision() {
            return Err(EpicError::CloudConflict);
        }
        cloud.fence().await?;
        Ok(listing)
    }
}

struct Cloud<'a> {
    client: &'a Client,
    game: CloudGame<'a>,
    account: &'a str,
    cancel: &'a CancelToken,
}

impl Cloud<'_> {
    fn revision(&self, listing: &BTreeMap<String, Entry>) -> String {
        revision(listing, self.game.app_name, self.account)
    }

    /// The revision the listing would have without `skip`, without copying it.
    fn revision_excluding(&self, listing: &BTreeMap<String, Entry>, skip: &str) -> String {
        revision(
            listing.iter().filter(|(name, _)| name.as_str() != skip),
            self.game.app_name,
            self.account,
        )
    }

    fn validate(&self) -> Result<(), EpicError> {
        self.cancel.check()?;
        self.game.validate()?;
        identity(self.account)
    }

    async fn authorize(&self) -> Result<(), EpicError> {
        self.validate()?;
        self.fence().await?;
        self.cancel
            .race(self.client.with_access_token(|token| async move {
                self.client
                    .ensure_owned(
                        &token,
                        self.game.namespace,
                        self.game.catalog_item_id,
                        self.game.app_name,
                    )
                    .await
            }))
            .await??;
        self.fence().await
    }

    async fn fence(&self) -> Result<(), EpicError> {
        self.cancel.check()?;
        if self.client.credentials().await?.account_id != self.account {
            return Err(EpicError::Unauthorized);
        }
        Ok(())
    }

    fn url(&self, manifests: bool) -> Result<Url, EpicError> {
        let mut segments = vec![
            "api",
            "v1",
            "access",
            "egstore",
            "savesync",
            self.account,
            self.game.app_name,
        ];
        if manifests {
            segments.push("manifests");
        }
        segments.push("");
        endpoint(&self.client.api.cloud, &segments)
    }

    async fn metadata(
        &self,
        names: Option<&[String]>,
    ) -> Result<BTreeMap<String, Entry>, EpicError> {
        self.fence().await?;
        let listing = self
            .cancel
            .race(self.client.with_access_token(|token| async move {
                let url = self.url(names.is_none())?;
                let request = match names {
                    Some(names) => self
                        .client
                        .api
                        .http
                        .post(url)
                        .json(&serde_json::json!({"files": names})),
                    None => self.client.api.http.get(url),
                }
                .bearer_auth(token);
                let response = request.send().await.map_err(|_| EpicError::Network)?;
                check_status(response.status())?;
                let bytes = bounded_bytes(response, MAX_METADATA_BYTES).await?;
                serde_json::from_slice::<Listing>(&bytes).map_err(|_| EpicError::Protocol)
            }))
            .await??;
        self.fence().await?;
        if listing.has_more
            || listing.next_cursor.is_some()
            || listing.files.len() >= INVENTORY_LIMIT
        {
            return Err(EpicError::UnsupportedContent);
        }
        let prefix = format!("savesync/{}/{}/", self.account, self.game.app_name);
        let mut result = BTreeMap::new();
        for (name, entry) in listing.files {
            let relative = name.strip_prefix(&prefix).unwrap_or(&name);
            relative_path(relative)?;
            if entry.hash.len() > 1024 || entry.last_modified.len() > 128 {
                return Err(EpicError::Protocol);
            }
            if result.insert(relative.to_owned(), entry).is_some() {
                return Err(EpicError::Protocol);
            }
        }
        Ok(result)
    }

    async fn list(&self) -> Result<BTreeMap<String, Entry>, EpicError> {
        let files = self.metadata(None).await?;
        for (name, entry) in &files {
            codec::manifest_time(name)?;
            if entry.hash.is_empty() || entry.last_modified.is_empty() {
                return Err(EpicError::Protocol);
            }
        }
        Ok(files)
    }

    async fn links(&self, names: &[String]) -> Result<BTreeMap<String, Entry>, EpicError> {
        if names.is_empty() || names.len() > LINK_BATCH {
            return Err(EpicError::Protocol);
        }
        let result = self.metadata(Some(names)).await?;
        if result.keys().collect::<BTreeSet<_>>() != names.iter().collect() {
            return Err(EpicError::Protocol);
        }
        Ok(result)
    }

    async fn get(&self, link: &str, limit: usize) -> Result<Vec<u8>, EpicError> {
        self.fence().await?;
        let url = signed_url(link)?;
        let bytes = self
            .cancel
            .race(async {
                // Api's client has no default Authorization and refuses redirects.
                let response = self
                    .client
                    .api
                    .http
                    .get(url)
                    .send()
                    .await
                    .map_err(|_| EpicError::Network)?;
                if !response.status().is_success() {
                    return Err(EpicError::Http(response.status().as_u16()));
                }
                bounded_bytes(response, limit).await
            })
            .await??;
        self.fence().await?;
        Ok(bytes)
    }

    async fn put(&self, link: &str, bytes: Vec<u8>) -> Result<(), EpicError> {
        self.fence().await?;
        let url = signed_url(link)?;
        self.cancel
            .race(async {
                let response = self
                    .client
                    .api
                    .http
                    .put(url)
                    .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                    .body(bytes)
                    .send()
                    .await
                    .map_err(|_| EpicError::Network)?;
                if !response.status().is_success() {
                    return Err(EpicError::Http(response.status().as_u16()));
                }
                Ok(())
            })
            .await??;
        self.fence().await
    }

    /// The generation `inventory` names: its latest manifest, read and parsed.
    async fn listing(
        &self,
        inventory: &BTreeMap<String, Entry>,
    ) -> Result<CloudListing, EpicError> {
        let revision = self.revision(inventory);
        let Some((_, entry)) = latest_manifest(inventory)? else {
            return Ok(CloudListing::read(revision, self.account.into(), None));
        };
        let bytes = self
            .get(
                entry.read_link.as_deref().ok_or(EpicError::Protocol)?,
                codec::MAX_MANIFEST_BYTES,
            )
            .await?;
        let manifest = codec::parse(&bytes, self.game, self.account)?;
        Ok(CloudListing::read(
            revision,
            self.account.into(),
            Some(manifest),
        ))
    }

    /// Reads `names` of `listing`'s generation, or every file when `None`.
    /// Only the chunks those files use are downloaded; every chunk when `None`.
    async fn fetch(
        &self,
        listing: &CloudListing,
        names: Option<&BTreeSet<String>>,
    ) -> Result<BTreeMap<String, Vec<u8>>, EpicError> {
        if names.is_some_and(|names| names.iter().any(|name| !listing.lists(name))) {
            return Err(EpicError::Protocol);
        }
        let Some(manifest) = listing.manifest.clone() else {
            // Only an empty inventory has no manifest to read chunks from.
            return match listing.files().next() {
                None => Ok(BTreeMap::new()),
                Some(_) => Err(EpicError::Protocol),
            };
        };
        let used: Option<BTreeSet<ChunkGuid>> = names.map(|names| {
            manifest
                .files
                .iter()
                .filter(|file| names.contains(&file.filename))
                .flat_map(|file| file.parts.iter().map(|part| part.guid))
                .collect()
        });
        let chunks: Vec<(String, &Chunk)> = manifest
            .chunks
            .values()
            .filter(|chunk| used.as_ref().is_none_or(|used| used.contains(&chunk.guid)))
            .map(|chunk| (chunk.relative_path(manifest.feature_level), chunk))
            .collect();
        let batches = Batch::split(chunks.iter().map(|(name, _)| name.clone()).collect());
        let mut decoded = BTreeMap::<ChunkGuid, Vec<u8>>::new();
        TRANSFERS
            .run(
                chunks.iter().enumerate(),
                |(_, (_, chunk))| chunk.file_size,
                || self.cancel.check().map_err(EpicError::from),
                |(index, (name, chunk))| {
                    let batch = &batches[index / LINK_BATCH];
                    async move {
                        let links = batch.links(self).await?;
                        let link = links
                            .get(name)
                            .and_then(|entry| entry.read_link.as_deref())
                            .ok_or(EpicError::Protocol)?;
                        let bytes = self
                            .get(link, MAX_CHUNK_BYTES.min(chunk.file_size as usize))
                            .await?;
                        self.cancel.check()?;
                        let guid = chunk.guid;
                        let chunk = (*chunk).clone();
                        let bytes = self
                            .cancel
                            .spawn_blocking(move || content::decode_chunk(&bytes, &chunk))
                            .await
                            .map_err(|_| EpicError::Protocol)??;
                        Ok((guid, bytes))
                    }
                },
                |(guid, bytes)| {
                    decoded.insert(guid, bytes);
                    Ok(())
                },
            )
            .await?;
        let names = names.cloned();
        let cancel = self.cancel.clone();
        self.cancel
            .spawn_blocking(move || {
                codec::reconstruct(&manifest, &decoded, names.as_ref(), &cancel)
            })
            .await
            .map_err(|_| EpicError::Protocol)?
    }
}

/// One link request's worth of objects. Its signed links are requested by the
/// first transfer that needs them, so the window refills across batches.
struct Batch {
    names: Vec<String>,
    links: tokio::sync::OnceCell<BTreeMap<String, Entry>>,
}

impl Batch {
    fn split(names: Vec<String>) -> Vec<Self> {
        names
            .chunks(LINK_BATCH)
            .map(|names| Self {
                names: names.to_vec(),
                links: tokio::sync::OnceCell::new(),
            })
            .collect()
    }

    async fn links(&self, cloud: &Cloud<'_>) -> Result<&BTreeMap<String, Entry>, EpicError> {
        self.links
            .get_or_try_init(|| cloud.links(&self.names))
            .await
    }
}

fn latest_manifest(listing: &BTreeMap<String, Entry>) -> Result<Option<(&str, &Entry)>, EpicError> {
    let mut latest = None;
    for (name, entry) in listing {
        let time = codec::manifest_time(name)?;
        if latest.is_none_or(|(previous, _, _)| time > previous) {
            latest = Some((time, name.as_str(), entry));
        }
    }
    Ok(latest.map(|(_, name, entry)| (name, entry)))
}

fn revision<'a>(
    listing: impl IntoIterator<Item = (&'a String, &'a Entry)>,
    app: &str,
    account: &str,
) -> String {
    let mut hash = Sha1::new();
    for value in [app, account] {
        hash.update((value.len() as u64).to_le_bytes());
        hash.update(value.as_bytes());
    }
    for (name, entry) in listing {
        for value in [
            name.as_str(),
            entry.hash.as_str(),
            entry.last_modified.as_str(),
        ] {
            hash.update((value.len() as u64).to_le_bytes());
            hash.update(value.as_bytes());
        }
    }
    hex(&hash.finalize())
}

fn signed_url(link: &str) -> Result<Url, EpicError> {
    if link.len() > 32_768 {
        return Err(EpicError::Protocol);
    }
    let url = Url::parse(link).map_err(|_| EpicError::Protocol)?;
    let host = url.host_str().ok_or(EpicError::Protocol)?;
    let allowed = ["epicgames.com", "epicgames.dev", "amazonaws.com"]
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")));
    #[cfg(test)]
    let allowed = allowed || url.scheme() == "http" && host == "127.0.0.1";
    #[cfg(not(test))]
    if url.scheme() != "https" {
        return Err(EpicError::Protocol);
    }
    if !allowed || !matches!(url.scheme(), "https" | "http") || !credential_free(&url) {
        return Err(EpicError::Protocol);
    }
    Ok(url)
}

#[cfg(test)]
mod tests;
