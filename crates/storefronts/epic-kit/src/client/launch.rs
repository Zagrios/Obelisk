//! Resolve a verified Epic installation and obtain fresh, short-lived game
//! credentials. Process execution remains in the native host.
use std::{collections::BTreeMap, path::Path};

use sailover_core::{
    installation::{self, InstallFailure},
    launch::{self, LaunchTarget},
};
use serde::Deserialize;

use super::{Client, Installation, Prerequisite, endpoint, validate_identity};
use crate::{
    EpicError,
    friends::{Activity, publishable_text},
};

// Intentionally no Debug: arguments contain a one-use exchange code and the
// optional ownership token must only be handed to the game being launched.
pub struct LaunchPlan {
    pub app_name: String,
    /// Absent when the catalog offers no publishable presence identity.
    pub activity: Option<Activity>,
    pub platform: String,
    pub target: LaunchTarget,
    pub ownership_token: Option<Vec<u8>>,
}

#[derive(Deserialize)]
struct ExchangeCode {
    code: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LaunchCatalogItem {
    #[serde(default)]
    title: serde_json::Value,
    #[serde(default)]
    custom_attributes: BTreeMap<String, serde_json::Value>,
}

struct LaunchCredentials {
    exchange: String,
    additional: String,
    ownership_token: Option<Vec<u8>>,
    activity: Option<Activity>,
}

impl LaunchCatalogItem {
    fn attribute(&self, name: &str) -> Result<Option<&str>, EpicError> {
        self.custom_attributes
            .get(name)
            .map(|attribute| {
                attribute
                    .get("value")
                    .and_then(serde_json::Value::as_str)
                    .ok_or(EpicError::Protocol)
            })
            .transpose()
    }
}

/// What the installation in `directory` asks to run once before its game
/// first starts. Only the local receipt is read, so this runs before any
/// short-lived game credential exists.
pub fn installed_prerequisite(
    namespace: &str,
    catalog_item_id: &str,
    app_name: &str,
    platform: &str,
    directory: &Path,
) -> Result<Option<Prerequisite>, EpicError> {
    validate_identity(namespace, catalog_item_id, app_name, platform)?;
    let installed =
        installation::open::<Installation>(directory).map_err(|failure| match failure {
            InstallFailure::Io(_) => launch_error("directoryUnreadable"),
            failure => failure.into(),
        })?;
    let receipt = installed
        .receipt
        .as_ref()
        .ok_or_else(|| launch_error("incompleteInstallation"))?;
    if receipt.namespace != namespace
        || receipt.catalog_item_id != catalog_item_id
        || receipt.app_name != app_name
        || receipt.platform != platform
    {
        return Err(launch_error("incompleteInstallation"));
    }
    Ok(receipt.prerequisite.clone())
}

impl Client {
    pub async fn launch_plan(
        &self,
        namespace: &str,
        catalog_item_id: &str,
        app_name: &str,
        platform: &str,
        directory: &Path,
        language: &str,
    ) -> Result<LaunchPlan, EpicError> {
        let native_platform = match platform {
            "Mac" => "macos",
            "Windows" => "windows",
            _ => return Err(launch_error("unsupportedPlatform")),
        };
        validate_identity(namespace, catalog_item_id, app_name, platform)?;
        let credentials = self.credentials().await?;
        if !directory.is_absolute() || !directory.is_dir() {
            return Err(launch_error("directoryMissing"));
        }
        // No Transfer of this installation can start until the plan returns.
        let installed =
            installation::open::<Installation>(directory).map_err(|failure| match failure {
                InstallFailure::Io(_) => launch_error("directoryUnreadable"),
                failure => failure.into(),
            })?;
        let root = &installed.root;
        let receipt = installed
            .receipt
            .as_ref()
            .ok_or_else(|| launch_error("incompleteInstallation"))?;
        if receipt.namespace != namespace
            || receipt.catalog_item_id != catalog_item_id
            || receipt.app_name != app_name
            || receipt.platform != platform
            || receipt.account_id != credentials.account_id
            || Path::new(&receipt.directory).canonicalize().ok().as_ref() != Some(root)
        {
            return Err(launch_error("incompleteInstallation"));
        }
        // Resolve the exact publisher executable and argv with Steam's shared
        // path confinement and quoting rules. Never invoke a shell.
        let executable = receipt.launch_executable.replace('\\', "/");
        let working_directory = Path::new(&executable)
            .parent()
            .and_then(Path::to_str)
            .unwrap_or("");
        let mut target = launch::resolve(
            native_platform,
            root,
            &executable,
            &receipt.launch_command,
            working_directory,
        )
        .map_err(|error| launch_error(error.key_suffix()))?;
        let account_id = credentials.account_id.as_str();
        let launch = self
            .with_access_token(|token| async move {
                self.launch_credentials(&token, account_id, namespace, catalog_item_id, app_name)
                    .await
            })
            .await?;
        // Disconnect may complete while the account APIs are in flight.
        let current = self.credentials().await?;
        if current.account_id != receipt.account_id {
            return Err(EpicError::Unauthorized);
        }
        target.arguments.extend(
            launch::parse_arguments(&launch.additional, native_platform)
                .map_err(|error| launch_error(error.key_suffix()))?,
        );
        target.arguments.extend([
            "-AUTH_LOGIN=unused".into(),
            format!("-AUTH_PASSWORD={}", launch.exchange),
            "-AUTH_TYPE=exchangecode".into(),
            format!("-epicapp={app_name}"),
            "-epicenv=Prod".into(),
            "-EpicPortal".into(),
            format!("-epicusername={}", current.display_name),
            format!("-epicuserid={}", current.account_id),
            format!("-epiclocale={}", epic_locale(language)),
            format!("-epicsandboxid={namespace}"),
        ]);
        if let Some(deployment) = &receipt.deployment_id {
            if deployment.is_empty() || deployment.len() > 1024 || deployment.contains('\0') {
                return Err(EpicError::Protocol);
            }
            target
                .arguments
                .push(format!("-epicdeploymentid={deployment}"));
        }
        Ok(LaunchPlan {
            app_name: app_name.into(),
            activity: launch.activity,
            platform: platform.into(),
            target,
            ownership_token: launch.ownership_token,
        })
    }

    async fn launch_credentials(
        &self,
        access_token: &str,
        account_id: &str,
        namespace: &str,
        catalog_item_id: &str,
        app_name: &str,
    ) -> Result<LaunchCredentials, EpicError> {
        self.ensure_owned(access_token, namespace, catalog_item_id, app_name)
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
            .append_pair("locale", "en");
        let mut catalog: BTreeMap<String, LaunchCatalogItem> =
            self.api.get(url, access_token).await?;
        let item = catalog.remove(catalog_item_id).ok_or(EpicError::Protocol)?;
        let additional = item
            .attribute("AdditionalCommandLine")?
            .unwrap_or_default()
            .to_owned();
        let requires_ownership = item
            .attribute("OwnershipToken")?
            .is_some_and(|value| value.eq_ignore_ascii_case("true"));
        let ownership = async {
            if requires_ownership {
                self.api
                    .ownership_token(access_token, account_id, namespace, catalog_item_id)
                    .await
                    .map(Some)
            } else {
                Ok(None)
            }
        };
        let url = endpoint(&self.api.account, &["account", "api", "oauth", "exchange"])?;
        let (exchange, ownership_token): (ExchangeCode, Option<Vec<u8>>) =
            tokio::try_join!(self.api.get(url, access_token), ownership)?;
        if exchange.code.is_empty()
            || exchange.code.len() > 2048
            || !exchange
                .code
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(EpicError::Protocol);
        }
        // Presence metadata is optional and must never prevent a valid launch.
        // An unusable title still publishes the activity, without a name.
        let name = item
            .title
            .as_str()
            .filter(|title| publishable_text(title, Activity::NAME_MAX))
            .map(str::to_owned);
        let activity = item.attribute("PresenceId").ok().and_then(|value| {
            let id = value.unwrap_or(namespace);
            (publishable_text(id, Activity::ID_MAX) && !id.starts_with("p-")).then(|| Activity {
                id: id.to_owned(),
                name,
            })
        });
        Ok(LaunchCredentials {
            exchange: exchange.code,
            additional,
            ownership_token,
            activity,
        })
    }
}

fn launch_error(suffix: &str) -> EpicError {
    EpicError::Service {
        message: format!("backend.epic.launch.{suffix}"),
    }
}

fn epic_locale(language: &str) -> String {
    let value = language.replace('_', "-");
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        "en".into()
    } else {
        value
    }
}
