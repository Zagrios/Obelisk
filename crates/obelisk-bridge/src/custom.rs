//! UniFFI bridge for launching and managing custom Windows executables.

use crate::{LaunchEngine, LaunchGraphics, ObeliskLocations};
use game_runtime::{
    Locations,
    custom::{self, CustomError, CustomLaunchConfig},
    wine::{Engine, Graphics},
};
use std::{collections::HashMap, path::PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CustomGameLaunchConfig {
    pub executable: String,
    pub arguments: Vec<String>,
    pub prefix: String,
    pub runner: Option<String>,
    pub engine: LaunchEngine,
    pub graphics: LaunchGraphics,
    pub ray_tracing: bool,
    pub performance_hud: bool,
    pub render_percent: u32,
    pub environment: HashMap<String, String>,
}

#[derive(Clone, Debug, thiserror::Error, uniffi::Error)]
pub enum CustomLaunchError {
    #[error("backend.runtime.wineUnavailable")]
    WineUnavailable,
    #[error("backend.runtime.notExecutable")]
    NotExecutable,
    #[error("backend.runtime.invalidApplication")]
    InvalidApplication,
    #[error("launch start failed")]
    StartFailed,
    #[error("launch storage failed")]
    Storage,
    #[error("{message}")]
    Failed { message: String },
}

impl From<CustomError> for CustomLaunchError {
    fn from(error: CustomError) -> Self {
        match error {
            CustomError::WineUnavailable => Self::WineUnavailable,
            CustomError::NotExecutable => Self::NotExecutable,
            CustomError::InvalidApplication => Self::InvalidApplication,
            CustomError::StartFailed => Self::StartFailed,
            CustomError::Storage => Self::Storage,
            CustomError::Failed(message) => Self::Failed { message },
        }
    }
}

#[uniffi::export]
pub fn launch_custom_executable(
    config: CustomGameLaunchConfig,
    locations: ObeliskLocations,
) -> Result<u32, CustomLaunchError> {
    let locations = Locations::from(&locations);
    let engine = match config.engine {
        LaunchEngine::Rosetta => Engine::Rosetta,
        LaunchEngine::Linux => Engine::Linux,
    };
    let graphics = match config.graphics {
        LaunchGraphics::D3dMetal => Graphics::D3DMetal,
        LaunchGraphics::Dxmt => Graphics::Dxmt,
    };
    let launch_config = CustomLaunchConfig {
        executable: PathBuf::from(config.executable),
        arguments: config.arguments,
        prefix: PathBuf::from(config.prefix),
        runner: config.runner.map(PathBuf::from),
        engine,
        graphics,
        ray_tracing: config.ray_tracing,
        performance_hud: config.performance_hud,
        render_percent: config.render_percent,
        environment: config.environment,
    };
    Ok(custom::launch_custom(&launch_config, &locations)?)
}

#[uniffi::export]
pub fn stop_custom_executable(
    pid: u32,
    prefix: String,
    runner: Option<String>,
    locations: ObeliskLocations,
) -> Result<(), CustomLaunchError> {
    let locations = Locations::from(&locations);
    custom::stop_custom(
        pid,
        std::path::Path::new(&prefix),
        runner.as_deref().map(std::path::Path::new),
        &locations,
    )
    .map_err(|message| CustomLaunchError::Failed { message })
}

#[uniffi::export]
pub fn force_quit_custom_executable(
    pid: u32,
    prefix: String,
    runner: Option<String>,
    locations: ObeliskLocations,
) -> Result<(), CustomLaunchError> {
    let locations = Locations::from(&locations);
    custom::force_quit_custom(
        pid,
        std::path::Path::new(&prefix),
        runner.as_deref().map(std::path::Path::new),
        &locations,
    )
    .map_err(|message| CustomLaunchError::Failed { message })
}

#[uniffi::export]
pub fn is_custom_process_running(pid: u32) -> bool {
    custom::is_custom_process_running(pid)
}

#[uniffi::export]
pub async fn download_default_wine_engine(
    locations: ObeliskLocations,
) -> Result<String, CustomLaunchError> {
    let locations = Locations::from(&locations);
    Ok(custom::download_default_wine_engine(&locations).await?)
}

#[uniffi::export]
pub fn is_default_wine_installed(locations: ObeliskLocations) -> bool {
    let locations = Locations::from(&locations);
    custom::is_default_wine_installed(&locations)
}

#[uniffi::export]
pub fn recommended_graphics_backend(executable_path: String) -> LaunchGraphics {
    let path = std::path::Path::new(&executable_path);
    let graphics = custom::recommended_graphics(path);
    match graphics {
        Graphics::D3DMetal => LaunchGraphics::D3dMetal,
        Graphics::Dxmt => LaunchGraphics::Dxmt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_running_bridge_check() {
        assert!(is_custom_process_running(std::process::id()));
        assert!(!is_custom_process_running(0));
    }
}
