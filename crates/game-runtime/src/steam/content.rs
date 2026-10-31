//! Steam's part of an account's installations: where each installation and
//! its Cloud saves live, and what Steam's network does for them. Only the
//! network is a seam; tests stand in for it.
use super::{Product, Steam, cloud as steam_cloud};
use crate::{
    Locations,
    cloud::{Control, record},
    cloud_status,
    installations::{self, BoxFuture, Receipt, Transfer, blocking},
    protocol::Credentials,
    sessions::{self, Admission, Failure, Storefront as _, Supervised, failed},
};
use cloud_vault::StoreScope;
use obelisk_core::{
    Platform, installation::TransferProgress, launch::LaunchTarget, transfer::CancelToken,
};
use std::{path::Path, sync::Arc};
use steam_kit::cloud::{CloudListing, CloudMetadata};

/// What an account's installations ask of Steam's network. Every call may
/// wait on it.
pub trait Network: Send + Sync + 'static {
    /// The build a Transfer without a platform installs, of those the
    /// account's licenses grant: the Mac build when its game runs natively
    /// on Apple silicon, else the Windows build.
    fn platform<'a>(
        &'a self,
        app_id: u32,
        language: &'a str,
    ) -> BoxFuture<'a, Result<Platform, Failure>>;
    /// Runs `transfer` into the folder it names.
    fn download<'a>(
        &'a self,
        transfer: &'a Transfer<Steam>,
    ) -> BoxFuture<'a, Result<steam_kit::client::Installation, Failure>>;
    fn progress(&self) -> Option<TransferProgress>;
    fn cancel(&self);
    /// Whether Steam offers a newer build than the one `receipt` installed.
    fn update_available<'a>(
        &'a self,
        receipt: &'a Receipt<Steam>,
    ) -> BoxFuture<'a, Result<bool, Failure>>;
    /// Waits until it reaches Steam through another CM connection than
    /// `connection`, and returns that one's number. A client alone never
    /// does: only the account session connects again.
    fn reconnected(&self, connection: u64) -> BoxFuture<'_, u64> {
        let _ = connection;
        Box::pin(std::future::pending())
    }
    /// What starts the game installed in `install`.
    fn launch_target<'a>(
        &'a self,
        app_id: u32,
        platform: Platform,
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
    fn platform<'a>(
        &'a self,
        app_id: u32,
        language: &'a str,
    ) -> BoxFuture<'a, Result<Platform, Failure>> {
        Box::pin(async move {
            steam_kit::client::Client::platform(self, app_id, language)
                .await
                .map_err(failed)
        })
    }
    fn download<'a>(
        &'a self,
        transfer: &'a Transfer<Steam>,
    ) -> BoxFuture<'a, Result<steam_kit::client::Installation, Failure>> {
        Box::pin(async move {
            // The Transfer's begin set it; Steam still refuses a download without one.
            let platform = transfer
                .platform
                .ok_or_else(|| {
                    steam_kit::client::ClientError::Failure(
                        "backend.steam.client.unsupportedPlatformOrLanguage".into(),
                    )
                })
                .map_err(failed)?;
            steam_kit::client::Client::download(
                self,
                transfer.product.app_id,
                platform,
                &transfer.product.language,
                &transfer.directory,
            )
            .await
            .map_err(failed)
        })
    }
    fn progress(&self) -> Option<TransferProgress> {
        steam_kit::client::Client::progress(self)
    }
    fn cancel(&self) {
        self.cancel_download();
    }
    fn update_available<'a>(
        &'a self,
        receipt: &'a Receipt<Steam>,
    ) -> BoxFuture<'a, Result<bool, Failure>> {
        Box::pin(async move {
            steam_kit::client::Client::update_available(
                self,
                receipt.product.app_id,
                receipt.platform,
                &receipt.product.language,
                Path::new(&receipt.directory),
            )
            .await
            .map_err(failed)
        })
    }
    fn launch_target<'a>(
        &'a self,
        app_id: u32,
        platform: Platform,
        install: &'a Path,
    ) -> BoxFuture<'a, Result<LaunchTarget, Failure>> {
        Box::pin(async move {
            self.launch_plan(app_id, platform, install)
                .await
                .map(|plan| plan.target)
                .map_err(failed)
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

    /// Where the installation's Cloud saves live: a Windows game's in the
    /// Wine prefix of the engine it runs on.
    async fn cloud(&self, receipt: &Receipt<Steam>) -> Result<steam_cloud::Config, Failure> {
        Ok(steam_cloud::Config {
            root: self.locations.cloud_archive.clone(),
            home: self.locations.home.clone(),
            install: receipt.directory.clone().into(),
            app_id: receipt.product.app_id,
            prefix: receipt.wine_prefix(&self.locations).await?,
        })
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
    /// A Transfer without a platform installs the build Steam picks.
    fn begin<'a>(
        &'a self,
        transfer: &'a Transfer<Steam>,
    ) -> BoxFuture<'a, Result<Platform, Failure>> {
        Box::pin(async move {
            match transfer.platform {
                Some(platform) => Ok(platform),
                None => {
                    self.network
                        .platform(transfer.product.app_id, &transfer.product.language)
                        .await
                }
            }
        })
    }
    fn download<'a>(
        &'a self,
        transfer: &'a Transfer<Steam>,
    ) -> BoxFuture<'a, Result<Receipt<Steam>, Failure>> {
        Box::pin(async move {
            let installed = self.network.download(transfer).await?;
            Ok(Receipt {
                product: Product {
                    app_id: installed.app_id,
                    language: installed.language,
                },
                name: installed.name,
                directory: installed.directory,
                platform: installed.platform,
                total_bytes: installed.total_bytes,
                needs_verification: false,
            })
        })
    }
    fn progress(&self) -> Option<TransferProgress> {
        self.network.progress()
    }
    fn cancel(&self) {
        self.network.cancel();
    }
    fn update_available<'a>(
        &'a self,
        receipt: &'a Receipt<Steam>,
    ) -> BoxFuture<'a, Result<bool, Failure>> {
        self.network.update_available(receipt)
    }
    fn reconnected(&self, connection: u64) -> BoxFuture<'_, u64> {
        self.network.reconnected(connection)
    }
    fn launch<'a>(
        &'a self,
        receipt: &'a Receipt<Steam>,
        admission: Admission<Steam>,
        join: Option<&'a super::Join>,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            let mut target = self
                .network
                .launch_target(
                    receipt.product.app_id,
                    receipt.platform,
                    Path::new(&receipt.directory),
                )
                .await?;
            if let Some(join) = join {
                target.arguments.extend(join.arguments(receipt.platform));
            }
            // Its launch settings, resolved once.
            let windows = receipt
                .windows_for_launch(&self.locations, super::Error::StartFailed.to_string())
                .await?
                .map_err(|error| failed(super::Error::from(error)))?;
            admission
                .hand_off(Handover {
                    locations: self.locations.clone(),
                    credentials: self.credentials.clone(),
                    receipt: receipt.clone(),
                    target,
                    windows,
                })
                .await
        })
    }
    fn archive<'a>(&'a self, receipt: &'a Receipt<Steam>) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            let rules = self
                .network
                .archive_rules(receipt.product.app_id)
                .await
                .map_err(key)?;
            let Some((metadata, listing)) = rules else {
                return Ok(());
            };
            let config = self.cloud(receipt).await?;
            blocking(super::Error::Storage.to_string(), move || {
                steam_cloud::archive_with_rules(&config, metadata, listing)
            })
            .await?
            .map_err(key)
        })
    }
    fn removed<'a>(&'a self, receipt: &'a Receipt<Steam>) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            let (config, account) = (self.cloud(receipt).await?, self.account());
            blocking(Steam::SESSION_FAILED, move || {
                record::removed::<steam_cloud::Context>(&config, &account)
            })
            .await?
            .map_err(key)
        })
    }
    fn directory_name(&self, receipt: &Receipt<Steam>) -> String {
        steam_kit::client::installation_directory_name(receipt.product.app_id, receipt.platform)
    }
    fn relocated(&self, receipt: &Receipt<Steam>, from: &Path) -> Result<(), String> {
        receipt.relocated(&self.locations, from)
    }
    fn remove_prefix(&self, receipt: &Receipt<Steam>) -> Option<Result<(), String>> {
        receipt.remove_prefix(&self.locations)
    }
    fn maintain<'a>(
        &'a self,
        game: &'a u32,
        installed: Option<&'a Receipt<Steam>>,
        control: Arc<Control>,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            // An uninstalled game syncs what its saved context names.
            let target = match installed {
                Some(receipt) => {
                    steam_cloud::MaintenanceTarget::Installed(self.cloud(receipt).await?)
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
                    .map_err(failed)?;
                network
                    .maintain(target, &credentials, control, &idle)
                    .map_err(cloud_failure)
            })
            .await?
        })
    }
    fn indicators(&self, games: &[u32]) -> Result<Vec<(u32, cloud_status::Indicator)>, Failure> {
        let scopes: Vec<StoreScope> = games
            .iter()
            .map(|app_id| steam_cloud::app_scope(self.account(), *app_id).into())
            .collect();
        let shown = record::indicators::<steam_cloud::Context>(
            &self.locations.cloud_archive,
            &self.account().into(),
            &scopes,
        )
        .map_err(key)?;
        Ok(shown
            .into_iter()
            .filter_map(|(scope, indicator)| match scope {
                StoreScope::Steam(scope) => Some((scope.app_id, indicator)),
                StoreScope::Epic(_) => None,
            })
            .collect())
    }
}

/// Starting an installed Steam game: its launch and Wine prefix readied,
/// then its supervisor, which signs in with the account's credentials.
struct Handover {
    locations: Locations,
    credentials: Credentials,
    receipt: Receipt<Steam>,
    target: LaunchTarget,
    /// Set for a Windows build, whose prefix it readies.
    windows: Option<crate::wine::Installation>,
}

impl sessions::Handover<Steam> for Handover {
    type Prepared = super::Request;
    fn platform(&self) -> Platform {
        self.receipt.platform
    }
    fn windows(&self) -> Option<&crate::wine::Installation> {
        self.windows.as_ref()
    }
    fn prepare(&self) -> BoxFuture<'_, Result<super::Request, Failure>> {
        let (locations, receipt, target, windows) = (
            self.locations.clone(),
            self.receipt.clone(),
            self.target.clone(),
            self.windows.clone(),
        );
        Box::pin(async move {
            blocking(super::Error::StartFailed.to_string(), move || {
                super::prepare(&locations, &receipt, windows.as_ref(), &target)
            })
            .await?
            .map_err(|error| match error {
                super::Error::Cancelled => Failure::Cancelled,
                error => failed(error),
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
