use super::{
    Failure,
    store::{Context, directory},
};
use std::path::PathBuf;
use steam_kit::cloud::{AutoCloudPaths, CloudError, CloudFile, LocalCloudFile, SdkCloudPaths};

pub(super) enum Paths {
    Auto(AutoCloudPaths),
    Sdk(SdkCloudPaths),
    Mixed(AutoCloudPaths, SdkCloudPaths),
}
impl Paths {
    pub fn existing_file(&self, file: &CloudFile) -> Result<Option<PathBuf>, CloudError> {
        Ok(match self {
            Self::Auto(p) => p.existing_file(file)?,
            Self::Sdk(p) => p.existing_file(file)?,
            Self::Mixed(auto, sdk) => {
                if file.name.starts_with('%') {
                    auto.existing_file(file)?
                } else {
                    sdk.existing_file(file)?
                }
            }
        })
    }
    pub fn destination(&self, name: &str, platforms: u32) -> Result<Option<PathBuf>, CloudError> {
        Ok(match self {
            Self::Auto(p) => p.destination(name, platforms)?,
            Self::Sdk(p) => p.destination(name, platforms)?,
            Self::Mixed(auto, sdk) => {
                if name.starts_with('%') {
                    auto.destination(name, platforms)?
                } else {
                    sdk.destination(name, platforms)?
                }
            }
        })
    }
    pub fn local_files(&self, files: &[CloudFile]) -> Result<Vec<LocalCloudFile>, CloudError> {
        Ok(match self {
            Self::Auto(p) => p.local_files(files)?,
            Self::Sdk(p) => p.local_files(files)?,
            Self::Mixed(auto, sdk) => {
                let (autofiles, sdkfiles): (Vec<_>, Vec<_>) =
                    files.iter().cloned().partition(|f| f.name.starts_with('%'));
                let mut files = auto.local_files(&autofiles)?;
                files.extend(sdk.local_files(&sdkfiles)?);
                files
            }
        })
    }
}
impl Context {
    pub fn sdk_home(&self) -> Result<PathBuf, Failure> {
        directory(&self.config, self.account_id)?;
        let name = self
            .config
            .root
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or(Failure::Storage)?;
        let home = self
            .config
            .root
            .with_file_name(format!("{name}-sdk"))
            .join(self.account_id.to_string())
            .join(self.config.app_id.to_string());
        super::store::safe(&home)?;
        Ok(home)
    }
    pub fn sdk_paths(&self) -> Result<SdkCloudPaths, Failure> {
        let entries = self.metadata.entries().ok_or(Failure::Unsupported)?;
        let mut keys = std::collections::BTreeSet::new();
        for (key, _) in entries {
            if !keys.insert(key.to_ascii_lowercase()) {
                return Err(Failure::Unsupported);
            }
        }
        for key in ["quota", "maxnumfiles"] {
            if !self
                .metadata
                .field(key)
                .and_then(|v| v.text())
                .and_then(|v| v.parse::<u64>().ok())
                .is_some_and(|n| n > 0)
            {
                return Err(Failure::Unsupported);
            }
        }
        Ok(SdkCloudPaths::new(
            self.sdk_home()?.join(format!(
                "Library/Application Support/Steam/userdata/{}/{}",
                self.account_id as u32, self.config.app_id
            )),
            self.config.app_id,
        )?)
    }
    pub fn prepare_sdk(&self) -> Result<Option<crate::NativeStorage>, Failure> {
        if self.config.prefix.is_some()
            || (self.metadata.field("savefiles").is_some()
                && self.metadata.field("quota").is_none())
        {
            return Ok(None);
        }
        let paths = self.sdk_paths()?;
        // Reconciliation has finished and no native generation exists yet.
        // Bindings are authoritative here, including newly downloaded files.
        let files = self
            .bindings
            .iter()
            .filter(|(name, _)| !name.starts_with('%'))
            .map(|(name, file)| LocalCloudFile {
                name: name.clone(),
                path: file.path.clone(),
                platforms: file.platforms,
            })
            .collect::<Vec<_>>();
        paths.prepare_index(&files)?;
        Ok(Some(crate::NativeStorage {
            home: self.sdk_home()?,
            account_id: self.account_id,
            app_id: self.config.app_id,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use steam_kit::cloud::CloudMetadata;
    fn metadata() -> CloudMetadata {
        CloudMetadata::Object(vec![
            ("quota".into(), CloudMetadata::Value("1000000".into())),
            ("maxnumfiles".into(), CloudMetadata::Value("1000".into())),
        ])
    }
    #[test]
    fn sdk_storage_is_separate_for_each_account_and_app_and_outside_archives() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let mut config = super::super::Config {
            root: base.join("cloud"),
            home: base.join("home"),
            install: base.join("game"),
            app_id: 42,
            prefix: None,
        };
        let first = Context::new(&config, 0x011000010000002a, metadata());
        let paths = first.sdk_paths().unwrap();
        let path = paths.destination("slot.dat", 7).unwrap().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"first account").unwrap();
        paths
            .prepare_index(&[LocalCloudFile {
                name: "slot.dat".into(),
                path: path.clone(),
                platforms: 7,
            }])
            .unwrap();
        assert_eq!(paths.local_files(&[]).unwrap().len(), 1);
        assert!(!path.starts_with(&config.root));
        let second = Context::new(&config, 0x011000010000002b, metadata());
        assert!(
            second
                .sdk_paths()
                .unwrap()
                .local_files(&[])
                .unwrap()
                .is_empty()
        );
        config.app_id = 43;
        let other_app = Context::new(&config, 0x011000010000002a, metadata());
        assert!(
            other_app
                .sdk_paths()
                .unwrap()
                .local_files(&[])
                .unwrap()
                .is_empty()
        );
        assert_eq!(std::fs::read(path).unwrap(), b"first account");
    }
    #[test]
    fn invalid_or_ambiguous_ufs_cannot_enable_sdk_storage() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let config = super::super::Config {
            root: base.join("cloud"),
            home: base.join("home"),
            install: base.join("game"),
            app_id: 42,
            prefix: None,
        };
        for metadata in [
            CloudMetadata::Object(vec![]),
            CloudMetadata::Value("invalid".into()),
            CloudMetadata::Object(vec![
                ("quota".into(), CloudMetadata::Value("1000".into())),
                ("quota".into(), CloudMetadata::Value("1000".into())),
            ]),
        ] {
            assert!(
                Context::new(&config, 0x011000010000002a, metadata)
                    .sdk_paths()
                    .is_err()
            );
        }
    }
}
