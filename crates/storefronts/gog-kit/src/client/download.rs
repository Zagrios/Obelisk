//! GOG's side of an installation Transfer: obelisk-core runs the order around
//! it, this module picks the build, its depots and redistributables, and runs
//! the content engine.
use std::{collections::HashSet, path::Path, sync::Arc};

use build_choice::CurrentBuilds;
use futures_util::{FutureExt, StreamExt, TryStreamExt, stream};
use md5::Digest as _;
use obelisk_core::{
    build_choice::GameBuilds,
    install::hex,
    installation::{self, Destination, InstallFailure, InstalledFiles, TransferError},
    mac_build,
    platform::Platform,
    transfer::{CancelToken, CdnClient, TransferBusy, TransferContext, Transfers},
};
use serde::{Deserialize, Serialize};

use super::{Client, PLATFORM, content_platform, launch, valid_product_id, write_platform};
use crate::{
    GogError,
    auth::Api,
    content::{self, Endpoint, SecureLinks},
    http::{bounded_bytes, check_status, inflate_json, send},
    library::endpoint,
    manifest::{
        self, Build, BuildManifest, Builds, DEPENDENCIES, DependencyDepot, DependencyRepository,
        Depot, DepotManifest, Installed, Product, galaxy_path,
    },
};

pub use obelisk_core::{installation::TransferProgress, transfer::TransferState};

/// Manifests are small; anything larger is not one.
const MAX_MANIFEST_BYTES: usize = 64 * 1024 * 1024;
const MANIFEST_CONCURRENCY: usize = 8;

/// A GOG installation, as its own receipt records it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Installation {
    pub product_id: String,
    pub user_id: String,
    pub directory: String,
    #[serde(serialize_with = "write_platform")]
    pub platform: Platform,
    pub total_bytes: u64,
    pub build_id: String,
    pub version_name: String,
    /// The language whose depots it holds, such as `fr-FR`.
    pub language: String,
    /// Where the build's files are inside the installation folder: its
    /// bundle, for a Mac build that names one.
    #[serde(default)]
    pub content_root: String,
    /// The installer scripts run in the Install Script Interpreter.
    #[serde(default)]
    pub script_interpreter: bool,
    /// The game and the DLCs it holds.
    #[serde(default)]
    pub products: Vec<Product>,
    /// The redistributables to run once before the game first starts.
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
}

/// A redistributable installed with the game, which runs once in its prefix.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Dependency {
    pub id: String,
    /// The program, from the installation's content root.
    pub path: String,
    pub arguments: String,
}

mod build_choice;

pub(super) type Downloads = Transfers<TransferProgress>;

/// A build of a product, in a language: what a Mac build's facts are kept by.
type BuildKey = (String, String, String);

/// The key of a build of a product in `language`; none for a build GOG gives no id.
fn build_key(product_id: &str, build_id: &str, language: &str) -> Option<BuildKey> {
    (!build_id.is_empty()).then(|| {
        (
            product_id.to_owned(),
            build_id.to_owned(),
            language.to_owned(),
        )
    })
}

/// Each Mac build's verdict, and the depot items of the last native one,
/// which its download starts from.
pub(super) type BuildChoice = obelisk_core::build_choice::BuildChoice<BuildKey, Installed>;

impl From<TransferBusy> for GogError {
    fn from(_: TransferBusy) -> Self {
        Self::AlreadyDownloading
    }
}

impl From<InstallFailure> for GogError {
    fn from(failure: InstallFailure) -> Self {
        match failure {
            InstallFailure::ParentMissing => Self::Service {
                message: "backend.gog.choose_existing_folder".into(),
            },
            InstallFailure::UnsafeDirectory(name) => Self::UnsafePath(name),
            InstallFailure::DestinationIsLink(path)
            | InstallFailure::DestinationIsFile(path)
            | InstallFailure::UnsafeLock(path)
            | InstallFailure::UnsafeReceipt(path) => {
                Self::UnsafePath(path.to_string_lossy().into_owned())
            }
            InstallFailure::Locked => Self::AlreadyDownloading,
            InstallFailure::Io(error) => Self::Io(error),
        }
    }
}

impl TransferError for GogError {
    fn is_cancellation(&self) -> bool {
        matches!(self, Self::Cancelled)
    }
}

/// Everything fetched before the installation lock is taken.
struct Prepared {
    manifest: BuildManifest,
    version_name: String,
    products: Vec<Product>,
    dependencies: Vec<Dependency>,
    installed: Installed,
    user_id: String,
}

/// The build Obelisk installs of `product_id` for `platform` (`osx` or
/// `windows`), and its manifest.
async fn current_build(
    api: &Api,
    product_id: &str,
    platform: &str,
) -> Result<(BuildManifest, String), GogError> {
    let build = listed_build(api, product_id, platform).await?;
    Ok((
        build_manifest(api, product_id, &build).await?,
        build.version_name,
    ))
}

/// The manifest of `product_id`'s listed `build`.
async fn build_manifest(
    api: &Api,
    product_id: &str,
    build: &Build,
) -> Result<BuildManifest, GogError> {
    let manifest: BuildManifest = document(api, content::checked_url(&build.link)?).await?;
    if manifest.base_product_id != product_id {
        return Err(GogError::Protocol);
    }
    Ok(manifest)
}

/// The build Obelisk installs of `product_id` for `platform`, as GOG lists
/// it, before its manifest is read.
async fn listed_build(api: &Api, product_id: &str, platform: &str) -> Result<Build, GogError> {
    let mut url = endpoint(
        &api.content,
        &["products", product_id, "os", platform, "builds"],
    )?;
    url.query_pairs_mut().append_pair("generation", "2");
    let response = api
        .http
        .get(url)
        .send()
        .await
        .map_err(|_| GogError::Network)?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(GogError::NoBuild);
    }
    check_status(response.status())?;
    let builds: Builds = crate::http::bounded_json(response, 16 * 1024 * 1024).await?;
    builds.current().ok_or(GogError::NoBuild)
}

/// `listed_build`, none when GOG has no such build.
async fn present_build(
    api: &Api,
    product_id: &str,
    platform: &str,
) -> Result<Option<Build>, GogError> {
    match listed_build(api, product_id, platform).await {
        Ok(build) => Ok(Some(build)),
        Err(GogError::NoBuild) => Ok(None),
        Err(error) => Err(error),
    }
}

/// A content-system document, without any account credential, inflated and
/// parsed off the async workers.
async fn document<T: serde::de::DeserializeOwned + Send + 'static>(
    api: &Api,
    url: reqwest::Url,
) -> Result<T, GogError> {
    let bytes = bounded_bytes(send(api.http.get(url)).await?, MAX_MANIFEST_BYTES).await?;
    tokio::task::spawn_blocking(move || inflate_json(&bytes, MAX_MANIFEST_BYTES as u64 * 4))
        .await
        .map_err(|_| GogError::Protocol)?
}

/// A depot's items, from its manifest under the CDN's `folder`.
async fn depot_items(
    api: &Api,
    folder: &str,
    manifest: &str,
) -> Result<Vec<manifest::Item>, GogError> {
    let url = content::checked_url(&format!(
        "{}/content-system/v2/{folder}/{}",
        api.cdn,
        galaxy_path(manifest)?
    ))?;
    let manifest: DepotManifest = document(api, url).await?;
    Ok(manifest.depot.items)
}

/// Adds the items of `depots`, each a manifest under the CDN's `folder` and
/// the secure link that serves its chunks, under `content_root`, in order.
async fn add_depots<'a>(
    api: &Api,
    installed: &mut Installed,
    content_root: &str,
    folder: &str,
    depots: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Result<(), GogError> {
    let (manifests, sources): (Vec<_>, Vec<_>) = depots.into_iter().unzip();
    let fetches: Vec<_> = manifests
        .into_iter()
        .map(|manifest| depot_items(api, folder, manifest))
        .collect();
    let items: Vec<Vec<manifest::Item>> = stream::iter(fetches)
        .buffered(MANIFEST_CONCURRENCY)
        .try_collect()
        .await?;
    for (source, items) in sources.into_iter().zip(items) {
        installed.add(content_root, source, items)?;
    }
    Ok(())
}

/// Every product the account owns, DLCs included.
async fn owned(api: &Api, access_token: &str) -> Result<HashSet<String>, GogError> {
    #[derive(Deserialize)]
    struct Owned {
        #[serde(default)]
        owned: Vec<serde_json::Value>,
    }
    let owned: Owned = api
        .get(
            endpoint(&api.embed, &["user", "data", "games"])?,
            access_token,
        )
        .await?;
    Ok(owned
        .owned
        .into_iter()
        .filter_map(|id| match id {
            serde_json::Value::Number(id) => Some(id.to_string()),
            serde_json::Value::String(id) => Some(id),
            _ => None,
        })
        .collect())
}

/// The products of the build the account owns: the game, and its DLCs.
fn owned_products(
    manifest: &BuildManifest,
    owned: &HashSet<String>,
) -> Result<Vec<Product>, GogError> {
    if !owned.contains(&manifest.base_product_id) {
        return Err(GogError::Service {
            message: "backend.gog.game_not_owned".into(),
        });
    }
    let mut products: Vec<Product> = manifest
        .products
        .iter()
        .filter(|product| {
            owned.contains(&product.product_id) && valid_product_id(&product.product_id)
        })
        .cloned()
        .collect();
    if !products
        .iter()
        .any(|product| product.product_id == manifest.base_product_id)
    {
        products.insert(
            0,
            Product {
                product_id: manifest.base_product_id.clone(),
                name: String::new(),
                temp_executable: String::new(),
                temp_arguments: String::new(),
            },
        );
    }
    Ok(products)
}

/// The products of the build the account owns, and their depots an
/// installation in `language` holds.
fn owned_depots<'a>(
    manifest: &'a BuildManifest,
    owned: &HashSet<String>,
    language: &str,
) -> Result<(Vec<Product>, Vec<&'a Depot>), GogError> {
    let products = owned_products(manifest, owned)?;
    let ids: HashSet<&str> = products
        .iter()
        .map(|product| product.product_id.as_str())
        .collect();
    let depots = manifest::depots(manifest, &ids, language);
    Ok((products, depots))
}

impl Client {
    pub fn progress(&self) -> Option<TransferProgress> {
        self.downloads.snapshot()
    }

    pub fn cancel_download(&self) {
        self.build_choice.cancel();
        self.downloads.cancel();
    }

    /// The build a download of the game installs, with the depots of
    /// `language`: the Mac build when its game runs natively on Apple
    /// silicon, else the Windows build. `cancel_download` stops it.
    pub async fn platform(&self, product_id: &str, language: &str) -> Result<Platform, GogError> {
        self.build_choice
            .pick(async |pick| {
                if !valid_product_id(product_id) || language.is_empty() || language.len() > 32 {
                    return Err(GogError::Protocol);
                }
                pick.of(&CurrentBuilds {
                    client: self,
                    product_id,
                    language,
                })
                .await
            })
            .await
    }

    /// The build the game's page shows before any download, and its size
    /// with the depots of `language`: the Mac build, unless a download found
    /// that its game does not run natively and GOG has a Windows build of
    /// it. None of the build's files is read.
    pub(super) async fn shown_build(
        &self,
        access_token: &str,
        product_id: &str,
        language: &str,
    ) -> Result<(Platform, Option<u64>), GogError> {
        let game = CurrentBuilds {
            client: self,
            product_id,
            language,
        };
        let offered = game.builds().await?;
        let (platform, build) = self
            .build_choice
            .shown(&game, offered)
            .await
            .ok_or(GogError::NoBuild)?;
        let size = match self
            .build_size(access_token, product_id, &build, language)
            .await
        {
            Ok(size) => Some(size),
            Err(GogError::Unauthorized) => return Err(GogError::Unauthorized),
            Err(_) => None,
        };
        Ok((platform, size))
    }

    /// Whether the program the primary play task of `installed` starts holds
    /// an arm64 slice.
    async fn plays_natively(
        &self,
        product_id: &str,
        installed: &Installed,
        content_root: &str,
        cancel: &CancelToken,
    ) -> Result<bool, GogError> {
        let from_root = |path: &str| match content_root {
            "" => path.to_owned(),
            root => format!("{root}/{path}"),
        };
        let mut listing = mac_build::Listing::default();
        for (index, file) in installed.files.iter().enumerate() {
            listing.file(&file.path, index, file.file.size());
        }
        for link in &installed.links {
            listing.link(&link.path, &link.target);
        }
        let links = SecureLinks::new(self.secure_links());
        let cdn = &CdnClient::new(self.tokens.api.http.clone());
        let links = &links;
        let read = move |index: usize, bytes: u64| {
            content::read(cdn, links, &installed.files[index], bytes, cancel)
        };
        let mut program = None;
        for info in launch::info_files(product_id) {
            if let Some((file, size)) = listing.find(&from_root(&info))
                && size <= launch::MAX_INFO_BYTES as u64
            {
                program = launch::played_program(&read(file, size).await?);
                break;
            }
        }
        let Some(program) = program else {
            return Ok(false);
        };
        mac_build::runs_natively(&listing, &from_root(&program), &[], read).await
    }

    /// The size of the listed `build` an installation in `language` holds.
    async fn build_size(
        &self,
        access_token: &str,
        product_id: &str,
        build: &Build,
        language: &str,
    ) -> Result<u64, GogError> {
        let api = &self.tokens.api;
        let (manifest, owned) = tokio::try_join!(
            build_manifest(api, product_id, build),
            owned(api, access_token),
        )?;
        let (_, depots) = owned_depots(&manifest, &owned, language)?;
        Ok(depots.iter().map(|depot| depot.size).sum())
    }

    /// Whether GOG's current build of the game differs from the one installed
    /// in `directory`. An installation whose receipt is gone needs its
    /// Transfer too. It reads GOG's public build list only.
    pub async fn update_available(
        &self,
        product_id: &str,
        platform: Platform,
        directory: &Path,
    ) -> Result<bool, GogError> {
        if !valid_product_id(product_id) {
            return Err(GogError::Protocol);
        }
        let Some(installed) = installation::read_receipt::<Installation>(directory)?
            .filter(|receipt| receipt.product_id == product_id && receipt.platform == platform)
        else {
            return Ok(true);
        };
        let current =
            listed_build(&self.tokens.api, product_id, content_platform(platform)).await?;
        // A build without its identity cannot be compared.
        Ok(!current.build_id.is_empty()
            && !installed.build_id.is_empty()
            && current.build_id != installed.build_id)
    }

    /// Installs `product_id`'s current build for `platform`, with the depots
    /// of `language`, in its dedicated folder under `parent`, or updates and
    /// repairs the installation there.
    pub async fn download(
        &self,
        product_id: &str,
        platform: Platform,
        language: &str,
        parent: &Path,
    ) -> Result<Installation, GogError> {
        if !valid_product_id(product_id) || language.is_empty() || language.len() > 32 {
            return Err(GogError::Protocol);
        }
        let directory = installation_directory_name(product_id, platform);
        installation::install(
            &self.downloads,
            product_id,
            Destination {
                parent,
                directory: &directory,
            },
            |_, cancel| async move {
                let prepared = cancel
                    .race(self.prepare_download(product_id, platform, language))
                    .await??;
                // A revoked account withdraws nothing: the receipt stays until
                // the installation lock is taken.
                if self.tokens.session.lock().await.is_none() {
                    return Err(GogError::Unauthorized);
                }
                Ok(prepared)
            },
            |prepared, destination, transfer| async move {
                self.install_files(
                    product_id,
                    platform,
                    language,
                    prepared,
                    &destination,
                    &transfer,
                )
                .await
            },
        )
        .await
    }

    async fn prepare_download(
        &self,
        product_id: &str,
        platform: Platform,
        language: &str,
    ) -> Result<Prepared, GogError> {
        let user_id = self.credentials().await?.user_id;
        let api = &self.tokens.api;
        let ((manifest, version_name), owned) = tokio::try_join!(
            current_build(api, product_id, content_platform(platform)),
            self.with_access_token(|token| async move { owned(api, &token).await }),
        )?;
        let (products, depots) = owned_depots(&manifest, &owned, language)?;
        let content_root = manifest.content_root()?;
        // The depot items the build's pick read, when it is that build.
        let picked = build_key(product_id, &manifest.build_id, language)
            .and_then(|key| self.build_choice.take(&key));
        let mut installed = match picked {
            Some(installed) => installed,
            None => {
                let mut installed = Installed::default();
                let depots = depots
                    .iter()
                    .map(|depot| (depot.manifest.as_str(), depot.product_id.as_str()));
                add_depots(api, &mut installed, &content_root, "meta", depots).await?;
                installed
            }
        };
        let dependencies = if platform == Platform::Windows && !manifest.dependencies.is_empty() {
            self.add_dependencies(&manifest, &content_root, &mut installed)
                .await?
        } else {
            Vec::new()
        };
        installed.validate()?;
        Ok(Prepared {
            manifest,
            version_name,
            products,
            dependencies,
            installed,
            user_id,
        })
    }

    /// Adds the redistributables the build names to the installation, and
    /// returns those that run once in its prefix.
    async fn add_dependencies(
        &self,
        manifest: &BuildManifest,
        content_root: &str,
        installed: &mut Installed,
    ) -> Result<Vec<Dependency>, GogError> {
        #[derive(Deserialize)]
        struct Repository {
            repository_manifest: String,
        }
        let mut url = endpoint(&self.tokens.api.content, &["dependencies", "repository"])?;
        url.query_pairs_mut().append_pair("generation", "2");
        let repository: Repository = self.tokens.api.get_public(url).await?;
        let repository: DependencyRepository = document(
            &self.tokens.api,
            content::checked_url(&repository.repository_manifest)?,
        )
        .await?;
        let wanted: Vec<&DependencyDepot> = manifest
            .dependencies
            .iter()
            .filter_map(|id| {
                repository
                    .depots
                    .iter()
                    .find(|depot| &depot.dependency_id == id)
            })
            .filter(|depot| depot.installs_in_wine())
            .collect();
        let depots = wanted
            .iter()
            .map(|depot| (depot.manifest.as_str(), DEPENDENCIES));
        add_depots(
            &self.tokens.api,
            installed,
            content_root,
            "dependencies/meta",
            depots,
        )
        .await?;
        Ok(wanted
            .into_iter()
            .filter(|depot| !depot.executable.path.is_empty())
            .map(|depot| Dependency {
                id: depot.dependency_id.clone(),
                path: depot.executable.path.replace('\\', "/"),
                arguments: depot.executable.arguments.clone(),
            })
            .collect())
    }

    async fn install_files(
        &self,
        product_id: &str,
        platform: Platform,
        language: &str,
        prepared: Prepared,
        destination: &Path,
        transfer: &TransferContext<TransferProgress>,
    ) -> Result<InstalledFiles<Installation>, GogError> {
        let cancel = transfer.cancel_token();
        let Prepared {
            manifest,
            version_name,
            products,
            dependencies,
            installed,
            user_id,
        } = prepared;
        let key = hex(&md5::Md5::digest(
            format!("{product_id}\0{}\0{language}", manifest.build_id).as_bytes(),
        ));
        let links = Arc::new(SecureLinks::new(self.secure_links()));
        let progress = transfer.clone();
        let summary = content::download(
            CdnClient::new(self.tokens.api.http.clone()),
            links,
            Arc::new(installed),
            key,
            destination,
            cancel,
            move |snapshot| progress.update(|progress| progress.show(snapshot)),
        )
        .await?;
        cancel.check()?;
        if self.tokens.session.lock().await.is_none() {
            return Err(GogError::Unauthorized);
        }
        Ok(InstalledFiles {
            receipt: Installation {
                product_id: product_id.into(),
                user_id,
                directory: destination.to_string_lossy().into_owned(),
                platform,
                total_bytes: summary.total_bytes,
                build_id: manifest.build_id.clone(),
                version_name,
                language: language.into(),
                content_root: manifest.content_root()?,
                script_interpreter: manifest.script_interpreter,
                products,
                dependencies,
            },
            total_bytes: summary.total_bytes,
            network_bytes: summary.downloaded_bytes,
            content_bytes: summary.content_bytes,
        })
    }

    /// Renews a product's secure link with the account's credentials, or the
    /// redistributables' open link.
    fn secure_links(&self) -> content::Renew {
        let tokens = self.tokens.clone();
        Arc::new(move |source: String| {
            let tokens = tokens.clone();
            async move {
                let api = &tokens.api;
                #[derive(Deserialize)]
                struct Links {
                    #[serde(default)]
                    urls: Vec<Endpoint>,
                }
                if source == DEPENDENCIES {
                    let mut url = endpoint(&api.content, &["open_link"])?;
                    url.query_pairs_mut()
                        .append_pair("generation", "2")
                        .append_pair("_version", "2")
                        .append_pair("path", "/dependencies/store/");
                    let links: Links = api.get_public(url).await?;
                    return Ok(links.urls);
                }
                if !valid_product_id(&source) {
                    return Err(GogError::Protocol);
                }
                let mut url = endpoint(&api.content, &["products", &source, "secure_link"])?;
                url.query_pairs_mut()
                    .append_pair("generation", "2")
                    .append_pair("_version", "2")
                    .append_pair("path", "/");
                let links: Links = tokens
                    .with_access_token(|token| {
                        let url = url.clone();
                        async move { api.get(url, &token).await }
                    })
                    .await?;
                Ok(links.urls)
            }
            .boxed()
        })
    }
}

/// The dedicated directory name for one GOG installation. The host relies on
/// this exact scheme to recognise and remove installations it created.
pub fn installation_directory_name(product_id: &str, platform: Platform) -> String {
    format!("gog-{product_id}-{}", PLATFORM.name(platform))
}
