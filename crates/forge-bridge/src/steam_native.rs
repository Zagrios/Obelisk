//! A single-use native launch receives the session already loaded by Forge.
//! It never queries Keychain, and cancellation crosses UniFFI explicitly.
use crate::{SteamCredentials, SteamError};
use std::sync::Arc;

impl From<steam_runtime::Error> for SteamError {
    fn from(error: steam_runtime::Error) -> Self {
        Self::Failure {
            reason: error.to_string(),
        }
    }
}

#[derive(uniffi::Object)]
pub struct SteamNativeLaunch {
    launch: Arc<steam_runtime::Launch>,
}

#[derive(uniffi::Record)]
pub struct SteamNativeConfiguration {
    pub app_id: u32,
    pub helpers: String,
    pub source: String,
    pub cache: String,
    pub diagnostics: String,
    pub steam_api: String,
    pub home: String,
}

#[uniffi::export(async_runtime = "tokio")]
impl SteamNativeLaunch {
    #[uniffi::constructor]
    pub fn new(credentials: SteamCredentials) -> Arc<Self> {
        Arc::new(Self {
            launch: Arc::new(steam_runtime::Launch::new(
                steam_runtime::protocol::Credentials {
                    steam_id: credentials.steam_id,
                    account_name: credentials.account_name,
                    refresh_token: credentials.refresh_token,
                },
            )),
        })
    }
    pub fn cancel(&self) {
        self.launch.cancel();
    }
    pub async fn prepare(
        &self,
        configuration: SteamNativeConfiguration,
    ) -> Result<Arc<SteamNativeRuntime>, SteamError> {
        let launch = self.launch.clone();
        let SteamNativeConfiguration {
            app_id,
            helpers,
            source,
            cache,
            diagnostics,
            steam_api,
            home,
        } = configuration;
        let runtime = tokio::task::spawn_blocking(move || {
            launch.prepare(steam_runtime::Config {
                app_id,
                helpers: helpers.into(),
                source: source.into(),
                cache: cache.into(),
                diagnostics: diagnostics.into(),
                steam_api: steam_api.into(),
                home: home.into(),
            })
        })
        .await
        .map_err(|_| steam_runtime::Error::StartFailed)??;
        Ok(Arc::new(SteamNativeRuntime { runtime }))
    }
}

#[derive(uniffi::Object)]
pub struct SteamNativeRuntime {
    runtime: steam_runtime::Runtime,
}

#[uniffi::export]
impl SteamNativeRuntime {
    pub fn environment(&self) -> std::collections::HashMap<String, String> {
        self.runtime.environment()
    }
    pub fn adapter_path(&self) -> String {
        self.runtime.adapter()
    }
    pub fn discovery_service(&self) -> String {
        self.runtime.service()
    }
    pub fn diagnostic_path(&self) -> String {
        self.runtime.diagnostic_path()
    }
    pub fn game_started(&self, pid: u32) -> Result<(), SteamError> {
        self.runtime.started(pid).map_err(Into::into)
    }
    pub fn stop(&self, exit_code: Option<i32>) {
        self.runtime.stopped(exit_code);
    }
}
