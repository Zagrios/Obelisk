//! Epic's part of an account's installations: where each build and its Cloud
//! saves live, the credentials every account call renews and saves first, and
//! what Epic's network does for them. Only the network is a seam; tests stand
//! in for it.
use super::{Configuration, Download, Epic, Error, IdleGuard, Receipt, Store};
use crate::{
    Locations,
    cloud::Control,
    cloud_status,
    installations::{self, BoxFuture, blocking},
    sessions::{Admission, BeforeBegin, Canceller, Failure, Handoff, Storefront as _, Supervised},
};
use epic_kit::client::Client as EpicClient;
use sailover_core::{installation::TransferProgress, transfer::CancelToken};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
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
    /// Runs one Transfer into the folder `request` names.
    fn download<'a>(
        &'a self,
        request: &'a Download,
    ) -> BoxFuture<'a, Result<epic_kit::client::Installation, Failure>>;
    fn progress(&self) -> Option<TransferProgress>;
    fn cancel(&self);
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

fn failed(error: impl std::fmt::Display) -> Failure {
    Failure::Failed(error.to_string())
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
    fn download<'a>(
        &'a self,
        request: &'a Download,
    ) -> BoxFuture<'a, Result<epic_kit::client::Installation, Failure>> {
        Box::pin(async move {
            EpicClient::download(
                self,
                &request.namespace,
                &request.catalog_item_id,
                &request.app_name,
                &request.platform,
                &request.directory,
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
    /// Waits until the credentials an account call rotated are saved. It
    /// blocks.
    saved: Arc<dyn Fn() + Send + Sync>,
    account: String,
    /// The language games launch in.
    language: String,
    locations: Locations,
    /// A Transfer was cancelled while its credentials were being renewed.
    cancelled: AtomicBool,
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
            saved,
            account,
            language,
            locations,
            cancelled: AtomicBool::new(false),
        }
    }

    /// As every Epic account call: renewed and saved credentials first. A
    /// cancellation meanwhile stops the call before it starts.
    async fn prepare(&self) -> Result<(), Failure> {
        self.cancelled.store(false, Ordering::Release);
        self.network.prepare().await?;
        self.save().await?;
        match self.cancelled.load(Ordering::Acquire) {
            true => Err(Failure::Cancelled),
            false => Ok(()),
        }
    }

    /// Waits until the credentials an account call rotated are saved.
    async fn save(&self) -> Result<(), Failure> {
        let saved = self.saved.clone();
        blocking(Epic::SESSION_FAILED, move || saved()).await
    }

    /// Saves them once the caller moved on: a completed Transfer's receipt
    /// never waits for Keychain.
    fn save_later(&self) {
        let saved = self.saved.clone();
        tokio::task::spawn_blocking(move || saved());
    }

    /// The build's Cloud configuration: a Windows build keeps its saves in
    /// its Wine prefix.
    fn configuration(&self, receipt: &Receipt, request: &Download) -> Configuration {
        Configuration {
            root: self.locations.cloud_archive.clone(),
            home: self.locations.home.clone(),
            install: receipt.directory.clone().into(),
            session_directory: self.locations.epic_sessions.clone(),
            helper: self.locations.helper.clone(),
            namespace: receipt.namespace.clone(),
            catalog_item_id: request.catalog_item_id.clone(),
            app_name: receipt.app_name.clone(),
            platform: receipt.platform.clone(),
            language: self.language.clone(),
            prefix: windows(receipt)
                .then(|| {
                    crate::wine::prefix(
                        &self.locations.wine_prefixes,
                        Path::new(&receipt.directory),
                    )
                    .ok()
                })
                .flatten(),
        }
    }

    /// What maintenance of `game` syncs: its installation, or, uninstalled,
    /// the configuration its journal saved, with this SailOver's paths.
    async fn maintenance(
        &self,
        game: &Store,
        installed: Option<(&Receipt, &Download)>,
    ) -> Result<Configuration, Failure> {
        if let Some((receipt, request)) = installed {
            return Ok(self.configuration(receipt, request));
        }
        let (root, account, wanted) = (
            self.locations.cloud_archive.clone(),
            self.account.clone(),
            (game.namespace.clone(), game.app_name.clone()),
        );
        let saved = blocking(Epic::SESSION_FAILED, move || {
            super::indicators(&root, &account, &[wanted])
        })
        .await?
        .map_err(failed)?
        .into_iter()
        .find(|shown| shown.namespace == game.namespace && shown.app_name == game.app_name)
        .and_then(|shown| shown.configuration);
        let mut configuration = saved.ok_or_else(|| failed(Error::Failed))?;
        configuration.root = self.locations.cloud_archive.clone();
        configuration.session_directory = self.locations.epic_sessions.clone();
        configuration.helper = self.locations.helper.clone();
        Ok(configuration)
    }
}

fn windows(receipt: &Receipt) -> bool {
    receipt.platform == "Windows"
}

impl<N: Network> installations::Content<Epic> for Content<N> {
    fn download<'a>(&'a self, request: &'a Download) -> BoxFuture<'a, Result<Receipt, Failure>> {
        Box::pin(async move {
            self.prepare().await?;
            let installed = self.network.download(request).await;
            self.save_later();
            let installed = installed?;
            Ok(Receipt {
                namespace: request.namespace.clone(),
                app_name: installed.app_name,
                name: request.title.clone(),
                directory: installed.directory,
                platform: installed.platform,
                total_bytes: installed.total_bytes,
                needs_verification: Some(false),
            })
        })
    }
    fn progress(&self) -> Option<TransferProgress> {
        self.network.progress()
    }
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.network.cancel();
    }
    fn launch<'a>(
        &'a self,
        receipt: &'a Receipt,
        request: &'a Download,
        admission: Admission<Epic>,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            let handoff = hand_off(
                self.network.clone(),
                self.configuration(receipt, request),
                self.locations.clone(),
                admission.canceller(),
            );
            // The exchange rotated the saved credentials, before the game starts.
            let saved = self.saved.clone();
            let before: BeforeBegin = Box::new(move || saved());
            let outcome = admission.launch_before(handoff, before).await;
            self.save_later();
            outcome
        })
    }
    fn archive<'a>(
        &'a self,
        receipt: &'a Receipt,
        request: &'a Download,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            self.prepare().await?;
            let archived = self
                .network
                .archive(self.configuration(receipt, request))
                .await;
            self.save_later();
            archived.map_err(failed)
        })
    }
    fn removed<'a>(
        &'a self,
        receipt: &'a Receipt,
        request: &'a Download,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            let (config, account) = (self.configuration(receipt, request), self.account.clone());
            blocking(Epic::SESSION_FAILED, move || {
                super::removed(&config, &account)
            })
            .await?
            .map_err(failed)
        })
    }
    fn directory_name(&self, receipt: &Receipt, request: &Download) -> String {
        epic_kit::client::installation_directory_name(
            &receipt.namespace,
            &request.catalog_item_id,
            &receipt.app_name,
            &receipt.platform,
        )
    }
    fn remove_prefix(&self, receipt: &Receipt) -> Option<Result<(), String>> {
        windows(receipt).then(|| {
            crate::wine::remove(&self.locations, Path::new(&receipt.directory))
                .map_err(|error| error.to_string())
        })
    }
    fn maintain<'a>(
        &'a self,
        game: &'a Store,
        installed: Option<(&'a Receipt, &'a Download)>,
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
        let games: Vec<_> = games
            .iter()
            .map(|game| (game.namespace.clone(), game.app_name.clone()))
            .collect();
        Ok(
            super::indicators(&self.locations.cloud_archive, &self.account, &games)
                .map_err(failed)?
                .into_iter()
                .map(|shown| {
                    let game = Store {
                        namespace: shown.namespace,
                        app_name: shown.app_name,
                    };
                    (game, shown.indicator)
                })
                .collect(),
        )
    }
}

/// Hands the build `configuration` names to its supervisor for `admission`,
/// readying Voile and the prefix of a Windows build before any exchange code
/// exists. A failure before the supervisor starts is definite.
async fn hand_off<N: Network>(
    network: Arc<N>,
    configuration: Configuration,
    locations: Locations,
    admission: Canceller<Epic>,
) -> Result<Arc<dyn Supervised<Epic>>, Handoff> {
    // A Windows build runs in Wine, readied before any exchange code exists.
    let wine = match configuration.prefix.is_some() {
        false => None,
        true => {
            admission
                .install_runtime(&locations.voile)
                .await
                .map_err(Handoff::Refused)?;
            let configuration = configuration.clone();
            Some(
                blocking(Error::Session.to_string(), move || {
                    super::prepare_windows(&configuration, &locations)
                })
                .await
                .map_err(Handoff::Refused)?
                .map_err(|error| Handoff::Refused(failed(error)))?,
            )
        }
    };
    let account = network.account_id().await.map_err(Handoff::Refused)?;
    let exchange = network.exchange_code().await.map_err(Handoff::Refused)?;
    // The code must belong to the account the game is launched for.
    if network.account_id().await.map_err(Handoff::Refused)? != account {
        return Err(Handoff::Refused(failed(epic_kit::EpicError::Unauthorized)));
    }
    if admission.cancelled() {
        return Err(Handoff::Refused(Failure::Cancelled));
    }
    let cancel = CancelToken::new();
    let token = cancel.clone();
    admission.on_cancel(move || token.cancel());
    let started = tokio::task::spawn_blocking(move || {
        super::start(configuration, account, exchange, wine, cancel)
    })
    .await
    .map_err(|_| Handoff::Lost(Error::Session.to_string()))?
    .map_err(|error| Handoff::Lost(error.to_string()))?;
    Ok(Arc::new(started) as Arc<dyn Supervised<Epic>>)
}

#[cfg(test)]
mod tests;
