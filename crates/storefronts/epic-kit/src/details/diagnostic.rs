//! What a failed manifest fetch received, written by debug builds only to
//! `~/Library/Logs/SailOver/Epic` for diagnosis. It never holds an access
//! token or a signed URL: the asset response keeps its keys and value types,
//! and manifest URLs keep their host.
use serde::Serialize;
use serde_json::Value;

#[derive(Default, Serialize)]
pub(super) struct Trace {
    pub platform: String,
    pub namespace: String,
    pub catalog_item_id: String,
    pub app_name: String,
    /// The step that failed.
    pub step: &'static str,
    pub error: String,
    pub assets: Option<Value>,
    pub asset_hash: Option<String>,
    pub attempts: Vec<Attempt>,
    /// The last manifest downloaded, public CDN content, written beside.
    #[serde(skip)]
    pub manifest: Option<Vec<u8>>,
}

#[derive(Default, Serialize)]
pub(super) struct Attempt {
    pub host: String,
    pub step: &'static str,
    pub status: Option<u16>,
    pub bytes: Option<usize>,
    /// The app name the manifest itself carries, when it parses.
    pub manifest_app_name: Option<String>,
    pub parse_error: Option<String>,
    pub error: String,
}

/// Values that are identities or sizes, never credentials.
const KEPT: [&str; 7] = [
    "appName",
    "labelName",
    "buildVersion",
    "hash",
    "namespace",
    "catalogItemId",
    "name",
];

/// `value` with every string replaced by its length, except identities, and
/// every URL by its host.
pub(super) fn redact(value: &Value) -> Value {
    fn walk(key: Option<&str>, value: &Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(name, value)| (name.clone(), walk(Some(name), value)))
                    .collect(),
            ),
            Value::Array(items) => Value::Array(items.iter().map(|item| walk(key, item)).collect()),
            Value::String(text) if key == Some("uri") => {
                Value::String(format!("host {}", host(text)))
            }
            Value::String(text) if key.is_some_and(|key| KEPT.contains(&key)) => {
                Value::String(text.clone())
            }
            Value::String(text) => Value::String(format!("<{} characters>", text.len())),
            other => other.clone(),
        }
    }
    walk(None, value)
}

pub(super) fn host(uri: &str) -> String {
    reqwest::Url::parse(uri)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_else(|| "<unparsable>".into())
}

/// Where debug builds write diagnostics; none in release builds or tests,
/// which fail fetches on purpose and must not write into the home folder.
fn directory() -> Option<std::path::PathBuf> {
    if !cfg!(debug_assertions) || cfg!(test) {
        return None;
    }
    let directory =
        std::path::Path::new(&std::env::var_os("HOME")?).join("Library/Logs/SailOver/Epic");
    std::fs::create_dir_all(&directory).ok()?;
    Some(directory)
}

/// A chunk the content engine could not decode: its public bytes, its URL
/// without any query, and what the manifest expected of it.
pub(crate) fn write_chunk(
    url: &reqwest::Url,
    chunk: &crate::content_manifest::Chunk,
    bytes: &[u8],
    error: &crate::EpicError,
) {
    let Some(directory) = directory() else {
        return;
    };
    let name = format!(
        "chunk-{:08X}{:08X}{:08X}{:08X}",
        chunk.guid[0], chunk.guid[1], chunk.guid[2], chunk.guid[3]
    );
    let mut url = url.clone();
    url.set_query(None);
    let summary = serde_json::json!({
        "url": url.as_str(),
        "error": format!("{error:?}"),
        "received_bytes": bytes.len(),
        "expected_file_size": chunk.file_size,
        "window_size": chunk.window_size,
        "group": chunk.group,
        "header": bytes.iter().take(98).map(|byte| format!("{byte:02x}")).collect::<String>(),
    });
    if let Ok(json) = serde_json::to_vec_pretty(&summary) {
        let _ = std::fs::write(directory.join(format!("{name}.json")), json);
    }
    let _ = std::fs::write(directory.join(format!("{name}.chunk")), bytes);
}

/// Writes `trace` and its manifest in debug builds; release builds keep nothing.
pub(super) fn write(trace: &Trace) {
    let Some(directory) = directory() else {
        return;
    };
    let name = format!("manifest-{}-{}", trace.app_name, trace.platform);
    if let Ok(json) = serde_json::to_vec_pretty(trace) {
        let _ = std::fs::write(directory.join(format!("{name}.json")), json);
    }
    if let Some(manifest) = &trace.manifest {
        let _ = std::fs::write(directory.join(format!("{name}.manifest")), manifest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_redacted_asset_response_keeps_identities_and_hosts_but_no_signature() {
        let response = serde_json::json!({"elements": [{
            "appName": "Game",
            "labelName": "Live-Windows",
            "hash": "0123",
            "manifests": [{
                "uri": "https://download.epicgames.com/Builds/Org/secret/path.manifest",
                "queryParams": [{"name": "f_token", "value": "signed-secret"}]
            }]
        }]});
        let redacted = redact(&response);
        let element = &redacted["elements"][0];
        assert_eq!(element["appName"], "Game");
        assert_eq!(element["labelName"], "Live-Windows");
        let manifest = &element["manifests"][0];
        assert_eq!(manifest["uri"], "host download.epicgames.com");
        assert_eq!(manifest["queryParams"][0]["name"], "f_token");
        assert_eq!(manifest["queryParams"][0]["value"], "<13 characters>");
        let text = redacted.to_string();
        assert!(!text.contains("signed-secret") && !text.contains("secret/path"));
    }
}
