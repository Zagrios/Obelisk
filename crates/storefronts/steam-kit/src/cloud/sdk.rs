//! Root-0 RemoteStorage projection. Only use while the native host is stopped.
use super::{CloudError, CloudFile, LocalCloudFile};
use crate::kv::{self, Kv};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    os::unix::fs::MetadataExt,
    path::PathBuf,
};

pub struct SdkCloudPaths {
    directory: PathBuf,
    app_id: u32,
    mask: u32,
}
impl SdkCloudPaths {
    /// `mask` is the platform of the game, as a `CloudPlatform` mask. A file
    /// synced to other platforms only stays in the Cloud.
    pub fn new(directory: PathBuf, app_id: u32, mask: u32) -> Result<Self, CloudError> {
        if app_id == 0 || mask == 0 || !directory.is_absolute() {
            return Err(CloudError::Invalid("SDK cache scope"));
        }
        super::paths::plain_path(&directory)?;
        Ok(Self {
            directory,
            app_id,
            mask,
        })
    }
    pub fn destination(&self, name: &str, platforms: u32) -> Result<Option<PathBuf>, CloudError> {
        let normalized = super::paths::relative(name, false)?;
        if normalized != name {
            return Err(CloudError::Invalid("SDK filename alias"));
        }
        if platforms & self.mask == 0 {
            return Ok(None);
        }
        let path = self.directory.join("remote").join(name);
        super::paths::plain_path(&path)?;
        Ok(Some(path))
    }
    pub fn existing_file(&self, file: &CloudFile) -> Result<Option<PathBuf>, CloudError> {
        if file.persist_state != 0 {
            return Ok(None);
        }
        self.destination(&file.name, file.platforms)
    }
    fn records(&self) -> Result<Vec<(String, u32, u32)>, CloudError> {
        let path = self.directory.join("remotecache.vdf");
        super::paths::plain_path(&path)?;
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => return Err(CloudError::Storage),
        };
        let root = kv::text(&bytes).map_err(|_| CloudError::Invalid("SDK cache index"))?;
        let [(key, app)] = root.children() else {
            return Err(CloudError::Invalid("SDK cache scope"));
        };
        if key != &self.app_id.to_string() {
            return Err(CloudError::Invalid("SDK cache AppID"));
        }
        if !matches!(app, Kv::Object(_)) {
            return Err(CloudError::Invalid("SDK cache object"));
        }
        let mut records = Vec::new();
        let mut names = BTreeSet::new();
        for (name, value) in app.children() {
            if !names.insert(name.to_lowercase()) {
                return Err(CloudError::Invalid("SDK cache alias"));
            }
            if let Kv::Value(_) = value {
                if !["changenumber", "ostype"].contains(&name.to_ascii_lowercase().as_str()) {
                    return Err(CloudError::Invalid("SDK cache field"));
                }
                continue;
            }
            let mut fields = BTreeSet::new();
            for (key, _) in value.children() {
                if !fields.insert(key.to_lowercase()) {
                    return Err(CloudError::Invalid("SDK cache duplicate field"));
                }
            }
            let number = |field: &str| {
                value
                    .get(field)
                    .and_then(Kv::text)
                    .and_then(|n| n.parse::<i64>().ok())
                    .ok_or(CloudError::Invalid("SDK cache field"))
            };
            if number("root")? != 0 {
                return Err(CloudError::Invalid("SDK cache root"));
            }
            let state = number("persiststate")?;
            if ![0, 2].contains(&state) {
                return Err(CloudError::Invalid("SDK cache persistence state"));
            }
            let platforms = number("platformstosync2")?;
            if !(-1..=u32::MAX as i64).contains(&platforms) {
                return Err(CloudError::Invalid("SDK cache platform mask"));
            }
            self.destination(name, platforms as u32)?;
            records.push((name.clone(), platforms as u32, state as u32));
        }
        Ok(records)
    }
    pub fn local_files(&self, known: &[CloudFile]) -> Result<Vec<LocalCloudFile>, CloudError> {
        let mut files = BTreeMap::new();
        for file in known {
            if let Some(path) = self.destination(&file.name, file.platforms)? {
                files.insert(
                    file.name.clone(),
                    LocalCloudFile {
                        name: file.name.clone(),
                        path,
                        platforms: file.platforms,
                    },
                );
            }
        }
        for (name, platforms, state) in self.records()? {
            if known
                .iter()
                .any(|file| file.name == name && file.platforms & self.mask == 0)
            {
                return Err(CloudError::Invalid(
                    "SDK local record conflicts with foreign platform",
                ));
            }
            if let Some(path) = self.destination(&name, platforms)? {
                if state == 2 && path.exists() {
                    return Err(CloudError::Invalid("SDK deletion still has bytes"));
                }
                files.insert(
                    name.clone(),
                    LocalCloudFile {
                        name,
                        path,
                        platforms,
                    },
                );
            }
        }
        let mut identities = BTreeSet::new();
        for file in files.values() {
            match fs::symlink_metadata(&file.path) {
                Ok(m)
                    if !m.is_file() || m.nlink() != 1 || !identities.insert((m.dev(), m.ino())) =>
                {
                    return Err(CloudError::Invalid("SDK file alias"));
                }
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    return Err(CloudError::Storage);
                }
                _ => (),
            }
        }
        let expected: BTreeSet<_> = files.values().map(|file| file.path.clone()).collect();
        let mut pending = vec![(self.directory.join("remote"), 0)];
        let mut count = 0;
        while let Some((directory, depth)) = pending.pop() {
            if depth > 64 {
                return Err(CloudError::Invalid("SDK directory depth"));
            }
            if !super::paths::plain_path(&directory)? {
                continue;
            }
            for entry in fs::read_dir(&directory).map_err(|_| CloudError::Storage)? {
                count += 1;
                if count > 100_000 {
                    return Err(CloudError::Invalid("SDK scan size"));
                }
                let path = entry.map_err(|_| CloudError::Storage)?.path();
                let metadata = fs::symlink_metadata(&path).map_err(|_| CloudError::Storage)?;
                if metadata.file_type().is_symlink() {
                    return Err(CloudError::Invalid("SDK symlink"));
                }
                if metadata.is_dir() {
                    pending.push((path, depth + 1));
                } else if !expected.contains(&path) {
                    return Err(CloudError::Invalid("unindexed SDK save"));
                }
            }
        }
        Ok(files.into_values().collect())
    }
    /// Replace the index before a fresh native generation, never while loaded.
    pub fn prepare_index(&self, files: &[LocalCloudFile]) -> Result<(), CloudError> {
        use sha1::{Digest, Sha1};
        let mut index = format!(
            "\"{}\"\n{{\n\"ChangeNumber\" \"0\"\n\"OSType\" \"-1\"\n",
            self.app_id
        );
        for file in files {
            if self.destination(&file.name, file.platforms)?.as_ref() != Some(&file.path) {
                return Err(CloudError::Invalid("SDK cache binding"));
            }
            let data = match fs::read(&file.path) {
                Ok(bytes) => bytes,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Err(CloudError::Storage),
            };
            let time = fs::metadata(&file.path)
                .map_err(|_| CloudError::Storage)?
                .mtime();
            let name = file.name.replace('"', "\\\"");
            index.push_str(&format!("\"{name}\"\n{{\n\"root\" \"0\"\n\"size\" \"{}\"\n\"localtime\" \"{time}\"\n\"time\" \"{time}\"\n\"remotetime\" \"0\"\n\"sha\" \"{}\"\n\"syncstate\" \"3\"\n\"persiststate\" \"0\"\n\"platformstosync2\" \"{}\"\n}}\n", data.len(), Sha1::digest(&data).iter().map(|b| format!("{b:02x}")).collect::<String>(), file.platforms as i32));
        }
        index.push_str("}\n");
        fs::create_dir_all(&self.directory).map_err(|_| CloudError::Storage)?;
        let mut output =
            tempfile::NamedTempFile::new_in(&self.directory).map_err(|_| CloudError::Storage)?;
        output
            .write_all(index.as_bytes())
            .map_err(|_| CloudError::Storage)?;
        output
            .as_file()
            .sync_all()
            .map_err(|_| CloudError::Storage)?;
        output
            .persist(self.directory.join("remotecache.vdf"))
            .map_err(|_| CloudError::Storage)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, SdkCloudPaths, LocalCloudFile) {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().canonicalize().unwrap();
        let paths = SdkCloudPaths::new(directory, 42, 2).unwrap();
        let path = paths.destination("nested/slot.dat", 7).unwrap().unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"fixture").unwrap();
        let file = LocalCloudFile {
            name: "nested/slot.dat".into(),
            path,
            platforms: 7,
        };
        (temp, paths, file)
    }
    #[test]
    fn index_roundtrip_preserves_nested_names_bytes_and_platforms() {
        let (_temp, paths, file) = fixture();
        paths.prepare_index(std::slice::from_ref(&file)).unwrap();
        assert_eq!(paths.local_files(&[]).unwrap(), vec![file.clone()]);
        assert_eq!(fs::read(file.path).unwrap(), b"fixture");
    }
    #[test]
    fn unindexed_writes_and_foreign_or_aliased_paths_never_become_empty_saves() {
        let (_temp, paths, file) = fixture();
        assert!(paths.local_files(&[]).is_err());
        paths.prepare_index(std::slice::from_ref(&file)).unwrap();
        fs::write(paths.directory.join("remote/unindexed.dat"), b"last write").unwrap();
        assert!(paths.local_files(&[]).is_err());
        for name in [
            "../escape",
            "/absolute",
            "a//b",
            "./slot",
            "a\\b",
            "%MacHome%save",
        ] {
            assert!(paths.destination(name, 7).is_err());
        }
        assert_eq!(paths.destination("foreign.dat", 1).unwrap(), None);
    }
    #[test]
    fn a_windows_game_keeps_windows_files_and_leaves_mac_only_files_in_the_cloud() {
        let temp = tempfile::tempdir().unwrap();
        let paths = SdkCloudPaths::new(temp.path().canonicalize().unwrap(), 42, 1).unwrap();
        assert!(paths.destination("windows.dat", 1).unwrap().is_some());
        assert!(
            paths
                .destination("everywhere.dat", u32::MAX)
                .unwrap()
                .is_some()
        );
        assert_eq!(paths.destination("mac.dat", 2).unwrap(), None);
        let mac = CloudFile {
            name: "mac.dat".into(),
            sha1: String::new(),
            size: 0,
            timestamp: 0,
            persist_state: 0,
            platforms: 2,
            reupload_requested: false,
        };
        paths.prepare_index(&[]).unwrap();
        assert_eq!(paths.local_files(&[mac]).unwrap(), vec![]);
    }
    #[test]
    fn changed_app_scope_duplicate_records_and_symbolic_links_are_rejected() {
        let (_temp, paths, file) = fixture();
        paths.prepare_index(std::slice::from_ref(&file)).unwrap();
        let index = paths.directory.join("remotecache.vdf");
        let text = fs::read_to_string(&index).unwrap();
        fs::write(&index, text.replacen("\"42\"", "\"43\"", 1)).unwrap();
        assert!(paths.local_files(&[]).is_err());
        fs::write(
            &index,
            text.replace("\"root\" \"0\"", "\"root\" \"0\"\n\"root\" \"1\""),
        )
        .unwrap();
        assert!(paths.local_files(&[]).is_err());
        fs::write(&index, text).unwrap();
        fs::remove_file(&file.path).unwrap();
        std::os::unix::fs::symlink("/tmp/unrelated", &file.path).unwrap();
        assert!(paths.local_files(&[]).is_err());
    }
}
