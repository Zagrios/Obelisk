//! GOG's part of an account's installations: where each build lives, the
//! credentials every account call renews and saves first, and what GOG's
//! network does for them. Only the network is a seam; tests stand in for it.
use super::{Configuration, Error, Gog, Product};
use crate::{
    Locations, cloud_status,
    installations::{self, BoxFuture, Credentials, Receipt, Transfer},
    launch_shell::{self, NoCredential},
    sessions::{Admission, Failure, Storefront as _, failed},
};
use gog_kit::{GogError, client::Client as GogClient};
use obelisk_core::{Platform, installation::TransferProgress};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// What an account's installations ask of GOG's network. Every call may wait
/// on it.
pub trait Network: Send + Sync + 'static {
    /// Renews the account's credentials before a long account call.
    fn prepare(&self) -> BoxFuture<'_, Result<(), Failure>>;
    /// The account the session is signed in to now.
    fn account_id(&self) -> BoxFuture<'_, Result<String, Failure>>;
    /// The build a Transfer without a platform installs, with the depots of
    /// `language`: the Mac build when its game runs natively on Apple
    /// silicon, else the Windows build.
    fn platform<'a>(
        &'a self,
        product: &'a Product,
        language: &'a str,
    ) -> BoxFuture<'a, Result<Platform, Failure>>;
    /// Runs `transfer` into the folder it names, with the depots of `language`.
    fn download<'a>(
        &'a self,
        transfer: &'a Transfer<Gog>,
        language: &'a str,
    ) -> BoxFuture<'a, Result<gog_kit::client::Installation, Failure>>;
    fn progress(&self) -> Option<TransferProgress>;
    fn cancel(&self);
    /// Whether GOG's current build differs from the one `receipt` installed.
    fn update_available<'a>(
        &'a self,
        receipt: &'a Receipt<Gog>,
    ) -> BoxFuture<'a, Result<bool, Failure>>;
}

impl Network for GogClient {
    fn prepare(&self) -> BoxFuture<'_, Result<(), Failure>> {
        Box::pin(async move { GogClient::prepare(self).await.map_err(failed) })
    }
    fn account_id(&self) -> BoxFuture<'_, Result<String, Failure>> {
        Box::pin(async move {
            self.credentials()
                .await
                .map(|credentials| credentials.user_id)
                .map_err(failed)
        })
    }
    fn platform<'a>(
        &'a self,
        product: &'a Product,
        language: &'a str,
    ) -> BoxFuture<'a, Result<Platform, Failure>> {
        Box::pin(async move {
            GogClient::platform(self, &product.product_id, language)
                .await
                .map_err(failed)
        })
    }
    fn download<'a>(
        &'a self,
        transfer: &'a Transfer<Gog>,
        language: &'a str,
    ) -> BoxFuture<'a, Result<gog_kit::client::Installation, Failure>> {
        Box::pin(async move {
            // The Transfer's begin set it; GOG still refuses a download without one.
            let platform = transfer
                .platform
                .ok_or(GogError::Protocol)
                .map_err(failed)?;
            GogClient::download(
                self,
                &transfer.product.product_id,
                platform,
                language,
                &transfer.directory,
            )
            .await
            .map_err(failed)
        })
    }
    fn progress(&self) -> Option<TransferProgress> {
        GogClient::progress(self)
    }
    fn cancel(&self) {
        self.cancel_download();
    }
    fn update_available<'a>(
        &'a self,
        receipt: &'a Receipt<Gog>,
    ) -> BoxFuture<'a, Result<bool, Failure>> {
        Box::pin(async move {
            GogClient::update_available(
                self,
                &receipt.product.product_id,
                receipt.platform,
                Path::new(&receipt.directory),
            )
            .await
            .map_err(failed)
        })
    }
}

/// One signed-in GOG account's installations, as they reach GOG.
pub struct Content<N: Network> {
    network: Arc<N>,
    credentials: Credentials,
    /// The language whose depots Transfers install, such as `fr-FR`.
    language: String,
    locations: Locations,
}

impl<N: Network> Content<N> {
    pub fn new(
        network: Arc<N>,
        saved: Arc<dyn Fn() + Send + Sync>,
        language: String,
        locations: Locations,
    ) -> Self {
        Self {
            network,
            credentials: Credentials::new(saved, Gog::SESSION_FAILED),
            language,
            locations,
        }
    }

    /// What a Game session of the build starts: a Windows build runs in its
    /// Wine `prefix`.
    fn configuration(&self, receipt: &Receipt<Gog>, prefix: Option<PathBuf>) -> Configuration {
        Configuration {
            home: self.locations.home.clone(),
            install: receipt.directory.clone().into(),
            session_directory: self.locations.gog_sessions.clone(),
            helper: self.locations.helper.clone(),
            product_id: receipt.product.product_id.clone(),
            platform: receipt.platform,
            title: receipt.name.clone(),
            prefix,
        }
    }
}

impl<N: Network> installations::Content<Gog> for Content<N> {
    fn begin<'a>(
        &'a self,
        transfer: &'a Transfer<Gog>,
    ) -> BoxFuture<'a, Result<Platform, Failure>> {
        Box::pin(async move {
            // As every GOG account call: renewed and saved credentials first.
            self.credentials.prepare(self.network.prepare()).await?;
            match transfer.platform {
                Some(platform) => Ok(platform),
                None => {
                    let platform = self
                        .network
                        .platform(&transfer.product, &self.language)
                        .await;
                    self.credentials.save_later();
                    platform
                }
            }
        })
    }
    fn download<'a>(
        &'a self,
        transfer: &'a Transfer<Gog>,
    ) -> BoxFuture<'a, Result<Receipt<Gog>, Failure>> {
        Box::pin(async move {
            let installed = self.network.download(transfer, &self.language).await;
            self.credentials.save_later();
            let installed = installed?;
            Ok(Receipt {
                product: Product {
                    product_id: installed.product_id,
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
        receipt: &'a Receipt<Gog>,
    ) -> BoxFuture<'a, Result<bool, Failure>> {
        self.network.update_available(receipt)
    }
    fn launch<'a>(
        &'a self,
        receipt: &'a Receipt<Gog>,
        admission: Admission<Gog>,
        _: Option<&'a std::convert::Infallible>,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            let windows = launch_shell::windows(receipt, &self.locations).await?;
            launch_shell::hand_off(
                admission,
                windows,
                &self.locations,
                |prefix| self.configuration(receipt, prefix),
                Signing(self.network.clone()),
            )
            .await
        })
    }
    /// GOG keeps no Cloud saves for Obelisk yet.
    fn archive<'a>(&'a self, _: &'a Receipt<Gog>) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async { Ok(()) })
    }
    fn removed<'a>(&'a self, _: &'a Receipt<Gog>) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async { Ok(()) })
    }
    fn directory_name(&self, receipt: &Receipt<Gog>) -> String {
        gog_kit::client::installation_directory_name(&receipt.product.product_id, receipt.platform)
    }
    /// Its installer scripts ran with the old folder: they run again with
    /// the new one at the next launch.
    fn relocated(&self, receipt: &Receipt<Gog>, from: &Path) -> Result<(), String> {
        receipt.relocated(&self.locations, from)?;
        if receipt.platform != Platform::Windows {
            return Ok(());
        }
        crate::wine::forget_prerequisites(&self.locations, Path::new(&receipt.directory), |id| {
            id.starts_with("gog:isi:") || id.starts_with("gog:installer:")
        })
        .map_err(|error| error.to_string())
    }
    fn remove_prefix(&self, receipt: &Receipt<Gog>) -> Option<Result<(), String>> {
        receipt.remove_prefix(&self.locations)
    }
    fn maintain<'a>(
        &'a self,
        _: &'a String,
        _: Option<&'a Receipt<Gog>>,
        _: Arc<crate::cloud::Control>,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async { Err(failed(Error::Unsupported)) })
    }
    fn indicators(&self, _: &[String]) -> Result<Vec<(String, cloud_status::Indicator)>, Failure> {
        Ok(Vec::new())
    }
}

/// Signs a GOG launch in with the account GOG's session is signed in to. The
/// build is DRM free, so the account is all its supervisor is given.
struct Signing<N: Network>(Arc<N>);

impl<N: Network> launch_shell::SignIn<Gog> for Signing<N> {
    fn credential(&self) -> BoxFuture<'_, Result<(String, NoCredential), Failure>> {
        Box::pin(async move { Ok((self.0.account_id().await?, NoCredential {})) })
    }
}

#[cfg(test)]
pub(crate) mod tests;
