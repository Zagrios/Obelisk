//! Epic's part of an account's installations: where each build and its Cloud
//! saves live, the credentials every account call renews and saves first, and
//! what Epic's network does for them. Only the network is a seam; tests stand
//! in for it.
use super::{
    Configuration, Epic, Error, ExchangeCode, IdleGuard, Product, Store, session::Context,
};
use crate::{
    Locations,
    cloud::{Control, record},
    cloud_status,
    installations::{self, BoxFuture, Credentials, Receipt, Transfer, blocking},
    launch_shell,
    sessions::{Admission, BeforeBegin, Failure, Storefront as _, failed},
};
use cloud_vault::{EpicScope, StoreScope};
use epic_kit::{EpicError, client::Client as EpicClient};
use obelisk_core::{Platform, installation::TransferProgress};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// What an account's installations ask of Epic's network. Every call may wait
/// on it.
pub trait Network: Send + Sync + 'static {
    /// Renews the account's credentials before a long account call.
    fn prepare(&self) -> BoxFuture<'_, Result<(), Failure>>;
    /// The account the session is signed in to now.
    fn account_id(&self) -> BoxFuture<'_, Result<String, Failure>>;
    /// A short-lived code the game signs in with.
    fn exchange_code(&self) -> BoxFuture<'_, Result<String, Failure>>;
    /// The build a Transfer without a platform installs: the Mac build when
    /// its game runs natively on Apple silicon, else the Windows build.
    fn platform<'a>(&'a self, product: &'a Product) -> BoxFuture<'a, Result<Platform, Failure>>;
    /// Runs `transfer` into the folder its begin settled the platform of.
    fn download<'a>(
        &'a self,
        transfer: &'a Transfer<Epic>,
    ) -> BoxFuture<'a, Result<epic_kit::client::Installation, Failure>>;
    fn progress(&self) -> Option<TransferProgress>;
    fn cancel(&self);
    /// Whether Epic's live build differs from the one `receipt` installed.
    fn update_available<'a>(
        &'a self,
        receipt: &'a Receipt<Epic>,
    ) -> BoxFuture<'a, Result<bool, Failure>>;
    /// Saves the build's Cloud archive before its files go.
    fn archive(&self, config: Configuration) -> BoxFuture<'_, Result<(), Error>>;
    /// Syncs Cloud saves on the player's request, while `idle` keeps every
    /// Epic launch out.
    fn maintain<'a>(
        self: Arc<Self>,
        config: Configuration,
        control: Arc<Control>,
        idle: &'a IdleGuard,
    ) -> BoxFuture<'a, Result<(), Error>>;
}

impl Network for EpicClient {
    fn prepare(&self) -> BoxFuture<'_, Result<(), Failure>> {
        Box::pin(async move { EpicClient::prepare(self).await.map(|_| ()).map_err(failed) })
    }
    fn account_id(&self) -> BoxFuture<'_, Result<String, Failure>> {
        Box::pin(async move {
            self.credentials()
                .await
                .map(|credentials| credentials.account_id)
                .map_err(failed)
        })
    }
    fn exchange_code(&self) -> BoxFuture<'_, Result<String, Failure>> {
        Box::pin(async move { EpicClient::exchange_code(self).await.map_err(failed) })
    }
    fn platform<'a>(&'a self, product: &'a Product) -> BoxFuture<'a, Result<Platform, Failure>> {
        Box::pin(async move {
            EpicClient::platform(
                self,
                &product.namespace,
                &product.catalog_item_id,
                &product.app_name,
            )
            .await
            .map_err(failed)
        })
    }
    fn download<'a>(
        &'a self,
        transfer: &'a Transfer<Epic>,
    ) -> BoxFuture<'a, Result<epic_kit::client::Installation, Failure>> {
        Box::pin(async move {
            // The Transfer's begin set it; Epic still refuses a download without one.
            let platform = transfer
                .platform
                .ok_or(EpicError::Protocol)
                .map_err(failed)?;
            EpicClient::download(
                self,
                &transfer.product.namespace,
                &transfer.product.catalog_item_id,
                &transfer.product.app_name,
                platform,
                &transfer.directory,
            )
            .await
            .map_err(failed)
        })
    }
    fn progress(&self) -> Option<TransferProgress> {
        EpicClient::progress(self)
    }
    fn cancel(&self) {
        self.cancel_download();
    }
    fn update_available<'a>(
        &'a self,
        receipt: &'a Receipt<Epic>,
    ) -> BoxFuture<'a, Result<bool, Failure>> {
        Box::pin(async move {
            EpicClient::update_available(
                self,
                &receipt.product.namespace,
                &receipt.product.catalog_item_id,
                &receipt.product.app_name,
                receipt.platform,
                Path::new(&receipt.directory),
            )
            .await
            .map_err(failed)
        })
    }
    fn archive(&self, config: Configuration) -> BoxFuture<'_, Result<(), Error>> {
        Box::pin(super::archive(config, self))
    }
    fn maintain<'a>(
        self: Arc<Self>,
        config: Configuration,
        control: Arc<Control>,
        idle: &'a IdleGuard,
    ) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(super::maintain(config, self, control, idle))
    }
}

/// One signed-in Epic account's installations, as they reach Epic.
pub struct Content<N: Network> {
    network: Arc<N>,
    credentials: Credentials,
    account: String,
    /// The language games launch in.
    language: String,
    locations: Locations,
}

impl<N: Network> Content<N> {
    pub fn new(
        network: Arc<N>,
        saved: Arc<dyn Fn() + Send + Sync>,
        account: String,
        language: String,
        locations: Locations,
    ) -> Self {
        Self {
            network,
            credentials: Credentials::new(saved, Epic::SESSION_FAILED),
            account,
            language,
            locations,
        }
    }

    /// As every Epic account call: renewed and saved credentials first.
    async fn prepare(&self) -> Result<(), Failure> {
        self.credentials.prepare(self.network.prepare()).await
    }

    /// The build's Cloud configuration: a Windows build keeps its saves in
    /// its Wine `prefix`.
    fn configuration(&self, receipt: &Receipt<Epic>, prefix: Option<PathBuf>) -> Configuration {
        Configuration {
            root: self.locations.cloud_archive.clone(),
            home: self.locations.home.clone(),
            install: receipt.directory.clone().into(),
            session_directory: self.locations.epic_sessions.clone(),
            helper: self.locations.helper.clone(),
            namespace: receipt.product.namespace.clone(),
            catalog_item_id: receipt.product.catalog_item_id.clone(),
            app_name: receipt.product.app_name.clone(),
            platform: receipt.platform,
            language: self.language.clone(),
            title: receipt.name.clone(),
            prefix,
        }
    }

    /// The build's Cloud configuration as its saves live now: a Windows build
    /// keeps them in the Wine prefix of the engine it runs on.
    async fn cloud_configuration(&self, receipt: &Receipt<Epic>) -> Result<Configuration, Failure> {
        Ok(self.configuration(receipt, receipt.wine_prefix(&self.locations).await?))
    }

    /// Where the Cloud archive keeps the saves of `game` for this account.
    fn scope(&self, game: &Store) -> StoreScope {
        EpicScope {
            account_id: self.account.clone(),
            namespace: game.namespace.clone(),
            app_name: game.app_name.clone(),
        }
        .into()
    }

    /// What maintenance of `game` syncs: its installation, or, uninstalled,
    /// the configuration its Cloud record names, with this Obelisk's paths.
    async fn maintenance(
        &self,
        game: &Store,
        installed: Option<&Receipt<Epic>>,
    ) -> Result<Configuration, Failure> {
        if let Some(receipt) = installed {
            return self.cloud_configuration(receipt).await;
        }
        let (root, scope) = (self.locations.cloud_archive.clone(), self.scope(game));
        let saved = blocking(Epic::SESSION_FAILED, move || {
            record::saved::<Context>(&root, &scope)
        })
        .await?
        .map_err(failed)?;
        let mut configuration = saved
            .map(|record| record.config)
            .ok_or_else(|| failed(Error::Failed))?;
        configuration.root = self.locations.cloud_archive.clone();
        configuration.session_directory = self.locations.epic_sessions.clone();
        configuration.helper = self.locations.helper.clone();
        Ok(configuration)
    }
}

impl<N: Network> installations::Content<Epic> for Content<N> {
    fn begin<'a>(
        &'a self,
        transfer: &'a Transfer<Epic>,
    ) -> BoxFuture<'a, Result<Platform, Failure>> {
        Box::pin(async move {
            self.prepare().await?;
            match transfer.platform {
                Some(platform) => Ok(platform),
                None => {
                    let platform = self.network.platform(&transfer.product).await;
                    // The call may have renewed the account's credentials.
                    self.credentials.save_later();
                    platform
                }
            }
        })
    }
    fn download<'a>(
        &'a self,
        transfer: &'a Transfer<Epic>,
    ) -> BoxFuture<'a, Result<Receipt<Epic>, Failure>> {
        Box::pin(async move {
            let installed = self.network.download(transfer).await;
            self.credentials.save_later();
            let installed = installed?;
            Ok(Receipt {
                product: Product {
                    namespace: transfer.product.namespace.clone(),
                    catalog_item_id: transfer.product.catalog_item_id.clone(),
                    app_name: installed.app_name,
                },
                name: transfer.name.clone(),
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
        self.credentials.cancel();
        self.network.cancel();
    }
    fn update_available<'a>(
        &'a self,
        receipt: &'a Receipt<Epic>,
    ) -> BoxFuture<'a, Result<bool, Failure>> {
        Box::pin(async move {
            let available = self.network.update_available(receipt).await;
            // The call may have renewed the account's credentials.
            self.credentials.save_later();
            available
        })
    }
    fn launch<'a>(
        &'a self,
        receipt: &'a Receipt<Epic>,
        admission: Admission<Epic>,
        _: Option<&'a std::convert::Infallible>,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            let windows = launch_shell::windows(receipt, &self.locations).await?;
            let outcome = launch_shell::hand_off(
                admission,
                windows,
                &self.locations,
                |prefix| self.configuration(receipt, prefix),
                Signing {
                    network: self.network.clone(),
                    saved: self.credentials.saved.clone(),
                },
            )
            .await;
            self.credentials.save_later();
            outcome
        })
    }
    fn archive<'a>(&'a self, receipt: &'a Receipt<Epic>) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            self.prepare().await?;
            let configuration = self.cloud_configuration(receipt).await?;
            let archived = self.network.archive(configuration).await;
            self.credentials.save_later();
            archived.map_err(failed)
        })
    }
    fn removed<'a>(&'a self, receipt: &'a Receipt<Epic>) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            let (config, account) = (
                self.cloud_configuration(receipt).await?,
                self.account.clone(),
            );
            blocking(Epic::SESSION_FAILED, move || {
                record::removed::<Context>(&config, &account)
            })
            .await?
            .map_err(failed)
        })
    }
    fn directory_name(&self, receipt: &Receipt<Epic>) -> String {
        epic_kit::client::installation_directory_name(
            &receipt.product.namespace,
            &receipt.product.catalog_item_id,
            &receipt.product.app_name,
            receipt.platform,
        )
    }
    fn relocated(&self, receipt: &Receipt<Epic>, from: &Path) -> Result<(), String> {
        receipt.relocated(&self.locations, from)
    }
    fn remove_prefix(&self, receipt: &Receipt<Epic>) -> Option<Result<(), String>> {
        receipt.remove_prefix(&self.locations)
    }
    fn maintain<'a>(
        &'a self,
        game: &'a Store,
        installed: Option<&'a Receipt<Epic>>,
        control: Arc<Control>,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            let config = self.maintenance(game, installed).await?;
            let (sessions, helper) = (config.session_directory.clone(), config.helper.clone());
            // Its idle guard keeps every launch of Epic out meanwhile.
            let idle = blocking(Epic::SESSION_FAILED, move || {
                super::Client::idle_guard(&sessions, &helper)
            })
            .await?
            .map_err(failed)?;
            match self.network.clone().maintain(config, control, &idle).await {
                Err(Error::Cancelled) => Err(Failure::Cancelled),
                result => result.map_err(failed),
            }
        })
    }
    fn indicators(
        &self,
        games: &[Store],
    ) -> Result<Vec<(Store, cloud_status::Indicator)>, Failure> {
        let scopes: Vec<StoreScope> = games.iter().map(|game| self.scope(game)).collect();
        let shown = record::indicators::<Context>(
            &self.locations.cloud_archive,
            &self.account.as_str().into(),
            &scopes,
        )
        .map_err(failed)?;
        Ok(shown
            .into_iter()
            .filter_map(|(scope, indicator)| match scope {
                StoreScope::Epic(scope) => Some((
                    Store {
                        namespace: scope.namespace,
                        app_name: scope.app_name,
                    },
                    indicator,
                )),
                StoreScope::Steam(_) => None,
            })
            .collect())
    }
}

/// Signs an Epic launch in with an exchange code for the account Epic's session
/// is signed in to.
struct Signing<N: Network> {
    network: Arc<N>,
    saved: Arc<dyn Fn() + Send + Sync>,
}

impl<N: Network> launch_shell::SignIn<Epic> for Signing<N> {
    fn credential(&self) -> BoxFuture<'_, Result<(String, ExchangeCode), Failure>> {
        Box::pin(async move {
            let account = self.network.account_id().await?;
            let exchange_code = self.network.exchange_code().await?;
            // The code must belong to the account the game is launched for.
            if self.network.account_id().await? != account {
                return Err(failed(epic_kit::EpicError::Unauthorized));
            }
            Ok((account, ExchangeCode { exchange_code }))
        })
    }
    /// The exchange rotated the saved credentials, before the game starts.
    fn before_begin(&self) -> BeforeBegin {
        let saved = self.saved.clone();
        Box::new(move || saved())
    }
}

#[cfg(test)]
pub(crate) mod tests;
