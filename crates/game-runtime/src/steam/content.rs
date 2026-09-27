//! Steam's part of an account's installations: where each installation and
//! its Cloud saves live, and what Steam's network does for them. Only the
//! network is a seam; tests stand in for it.
use super::{Download, Receipt, Steam, cloud as steam_cloud};
use crate::{
    Locations,
    cloud::Control,
    cloud_status,
    installations::{self, BoxFuture, blocking},
    protocol::Credentials,
    sessions::{self, Admission, Failure, Storefront as _, Supervised},
};
use sailover_core::{installation::TransferProgress, launch::LaunchTarget, transfer::CancelToken};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use steam_kit::cloud::{CloudListing, CloudMetadata};

/// What an account's installations ask of Steam's network. Every call may
/// wait on it.
pub trait Network: Send + Sync + 'static {
    /// Runs one Transfer into the folder `request` names.
    fn download<'a>(
        &'a self,
        request: &'a Download,
    ) -> BoxFuture<'a, Result<steam_kit::client::Installation, Failure>>;
    fn progress(&self) -> Option<TransferProgress>;
    fn cancel(&self);
    /// What starts the game installed in `install`.
    fn launch_target<'a>(
        &'a self,
        app_id: u32,
        platform: &'a str,
        install: &'a Path,
    ) -> BoxFuture<'a, Result<LaunchTarget, Failure>>;
    /// The Cloud rules the saves of `app_id` follow, `None` without Steam Cloud.
    fn archive_rules(
        &self,
        app_id: u32,
    ) -> BoxFuture<'_, Result<Option<(CloudMetadata, CloudListing)>, steam_cloud::Failure>>;
    /// Syncs Cloud saves on the player's request, while `idle` keeps every
    /// Steam launch out. It blocks.
    fn maintain(
        &self,
        target: steam_cloud::MaintenanceTarget,
        credentials: &Credentials,
        control: Arc<Control>,
        idle: &super::IdleGuard,
    ) -> Result<(), steam_cloud::Failure>;
}

impl Network for steam_kit::client::Client {
    fn download<'a>(
        &'a self,
        request: &'a Download,
    ) -> BoxFuture<'a, Result<steam_kit::client::Installation, Failure>> {
        Box::pin(async move {
            steam_kit::client::Client::download(
                self,
                request.app_id,
                &request.platform,
                &request.language,
                &request.directory,
            )
            .await
            .map_err(|error| Failure::Failed(error.to_string()))
        })
    }
    fn progress(&self) -> Option<TransferProgress> {
        steam_kit::client::Client::progress(self)
    }
    fn cancel(&self) {
        self.cancel_download();
    }
    fn launch_target<'a>(
        &'a self,
        app_id: u32,
        platform: &'a str,
        install: &'a Path,
    ) -> BoxFuture<'a, Result<LaunchTarget, Failure>> {
        Box::pin(async move {
            self.launch_plan(app_id, platform, install)
                .await
                .map(|plan| plan.target)
                .map_err(|error| Failure::Failed(error.to_string()))
        })
    }
    fn archive_rules(
        &self,
        app_id: u32,
    ) -> BoxFuture<'_, Result<Option<(CloudMetadata, CloudListing)>, steam_cloud::Failure>> {
        Box::pin(async move {
            self.cloud_archive_metadata(app_id)
                .await
                .map_err(Into::into)
        })
    }
    fn maintain(
        &self,
        target: steam_cloud::MaintenanceTarget,
        credentials: &Credentials,
        control: Arc<Control>,
        idle: &super::IdleGuard,
    ) -> Result<(), steam_cloud::Failure> {
        steam_cloud::maintain(target, credentials, control, idle)
    }
}

/// One signed-in Steam account's installations, as they reach Steam.
pub struct Content<N: Network> {
    network: Arc<N>,
    credentials: Credentials,
    locations: Locations,
}

impl<N: Network> Content<N> {
    pub fn new(network: Arc<N>, credentials: Credentials, locations: Locations) -> Self {
        Self {
            network,
            credentials,
            locations,
        }
    }

    fn account(&self) -> u64 {
        self.credentials.steam_id
    }

    /// Where the installation's Cloud saves live: a Windows game's in its
    /// Wine prefix.
    fn cloud(&self, receipt: &Receipt) -> steam_cloud::Config {
        steam_cloud::Config {
            root: self.locations.cloud_archive.clone(),
            home: self.locations.home.clone(),
            install: receipt.directory.clone().into(),
            app_id: receipt.app_id,
            prefix: self
                .windows(receipt)
                .map(|windows| windows.prefix().to_owned()),
        }
    }

    /// The installation's Wine prefix, when it is a Windows one.
    fn windows(&self, receipt: &Receipt) -> Option<crate::wine::Installation> {
        let install = Path::new(&receipt.directory);
        crate::wine::Installation::of(&self.locations, &receipt.platform, install)
            .ok()
            .flatten()
    }
}

/// A Cloud failure, as the player sees it.
fn key(failure: steam_cloud::Failure) -> Failure {
    Failure::Failed(failure.key().into())
}

/// How a Cloud sync the player asked for ended.
fn cloud_failure(failure: steam_cloud::Failure) -> Failure {
    match failure {
        steam_cloud::Failure::Cancelled => Failure::Cancelled,
        failure => Failure::Failed(failure.key().into()),
    }
}

impl<N: Network> installations::Content<Steam> for Content<N> {
    fn download<'a>(&'a self, request: &'a Download) -> BoxFuture<'a, Result<Receipt, Failure>> {
        Box::pin(async move {
            let installed = self.network.download(request).await?;
            Ok(Receipt {
                app_id: installed.app_id,
                name: installed.name,
                directory: installed.directory,
                platform: installed.platform,
                language: installed.language,
                total_bytes: installed.total_bytes,
                needs_verification: None,
            })
        })
    }
    fn progress(&self) -> Option<TransferProgress> {
        self.network.progress()
    }
    fn cancel(&self) {
        self.network.cancel();
    }
    fn launch<'a>(
        &'a self,
        receipt: &'a Receipt,
        _: &'a Download,
        admission: Admission<Steam>,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            let install = PathBuf::from(&receipt.directory);
            let target = self
                .network
                .launch_target(receipt.app_id, &receipt.platform, &install)
                .await?;
            admission
                .hand_off(Handover {
                    locations: self.locations.clone(),
                    credentials: self.credentials.clone(),
                    app_id: receipt.app_id,
                    platform: receipt.platform.clone(),
                    install,
                    target,
                })
                .await
        })
    }
    fn archive<'a>(
        &'a self,
        receipt: &'a Receipt,
        _: &'a Download,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            let rules = self
                .network
                .archive_rules(receipt.app_id)
                .await
                .map_err(key)?;
            let Some((metadata, listing)) = rules else {
                return Ok(());
            };
            let config = self.cloud(receipt);
            blocking(crate::Error::Storage.to_string(), move || {
                steam_cloud::archive_with_rules(&config, metadata, listing)
            })
            .await?
            .map_err(key)
        })
    }
    fn removed<'a>(
        &'a self,
        receipt: &'a Receipt,
        _: &'a Download,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            let (config, account) = (self.cloud(receipt), self.account());
            blocking(Steam::SESSION_FAILED, move || {
                steam_cloud::removed(&config, account)
            })
            .await?
            .map_err(key)
        })
    }
    fn directory_name(&self, receipt: &Receipt, _: &Download) -> String {
        format!("steam-{}-{}", receipt.app_id, receipt.platform)
    }
    fn remove_prefix(&self, receipt: &Receipt) -> Option<Result<(), String>> {
        let windows = self.windows(receipt)?;
        Some(
            windows
                .remove(&self.locations)
                .map_err(|error| error.to_string()),
        )
    }
    fn maintain<'a>(
        &'a self,
        game: &'a u32,
        installed: Option<(&'a Receipt, &'a Download)>,
        control: Arc<Control>,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            // An uninstalled game syncs what its saved context names.
            let target = match installed {
                Some((receipt, _)) => {
                    steam_cloud::MaintenanceTarget::Installed(self.cloud(receipt))
                }
                None => steam_cloud::MaintenanceTarget::Uninstalled {
                    root: self.locations.cloud_archive.clone(),
                    app_id: *game,
                },
            };
            let (network, credentials) = (self.network.clone(), self.credentials.clone());
            let locations = self.locations.clone();
            blocking(Steam::SESSION_FAILED, move || {
                // Its idle guard keeps every launch of Steam out meanwhile.
                let idle = super::Client::idle_guard(&locations.steam_sessions, &locations.helper)
                    .map_err(|error| Failure::Failed(error.to_string()))?;
                network
                    .maintain(target, &credentials, control, &idle)
                    .map_err(cloud_failure)
            })
            .await?
        })
    }
    fn indicators(&self, games: &[u32]) -> Result<Vec<(u32, cloud_status::Indicator)>, Failure> {
        steam_cloud::indicators(&self.locations.cloud_archive, self.account(), games).map_err(key)
    }
}

/// Starting an installed Steam game: its launch and Wine prefix readied,
/// then its supervisor, which signs in with the account's credentials.
struct Handover {
    locations: Locations,
    credentials: Credentials,
    app_id: u32,
    platform: String,
    install: PathBuf,
    target: LaunchTarget,
}

impl sessions::Handover<Steam> for Handover {
    type Prepared = super::Request;
    fn voile(&self) -> Option<PathBuf> {
        let windows = crate::wine::Installation::of(&self.locations, &self.platform, &self.install);
        matches!(windows, Ok(Some(_))).then(|| self.locations.voile.clone())
    }
    fn prepare(&self) -> BoxFuture<'_, Result<super::Request, Failure>> {
        let (locations, app_id) = (self.locations.clone(), self.app_id);
        let (platform, install, target) = (
            self.platform.clone(),
            self.install.clone(),
            self.target.clone(),
        );
        Box::pin(async move {
            blocking(crate::Error::StartFailed.to_string(), move || {
                super::prepare(&locations, app_id, &platform, &install, &target)
            })
            .await?
            .map_err(|error| match error {
                crate::Error::Cancelled => Failure::Cancelled,
                error => Failure::Failed(error.to_string()),
            })
        })
    }
    fn start(
        self,
        request: super::Request,
        cancel: CancelToken,
    ) -> Result<Arc<dyn Supervised<Steam>>, String> {
        let helper = request.config.helpers.join(crate::package::HOST);
        super::start(&helper, &self.credentials, request, &cancel)
            .map(|client| Arc::new(client) as Arc<dyn Supervised<Steam>>)
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests;
