//! A Steam launch: the native host that owns the game's Steam session, Steam
//! Cloud, and the game slice chosen against the pinned Steam API library.
use super::*;
use crate::{
    NativeStorage, Runtime, package,
    supervisor::{
        Error as Core,
        architecture::Architecture,
        session::{self, LaunchCloud, Opening, Prepared, Session},
        storage,
    },
};
use std::{io::Read, marker::PhantomData, sync::Arc};

/// What a Steam launch asks of the machine. Tests replace it.
pub(crate) trait Machine: Send + Sync + 'static {
    /// The game slice to launch against the pinned Steam API library.
    fn select(target: &Target, config: &Config) -> Result<Option<Architecture>, Error>;
    /// Starts the native host for this account and game.
    fn prepare(
        launch: &crate::Launch,
        config: Config,
        storage: Option<NativeStorage>,
    ) -> Result<Runtime, Error>;
    fn publish(runtime: &Runtime, active: bool);
    fn cloud(
        config: cloud::Config,
        credentials: Credentials,
        id: String,
        control: Arc<crate::cloud::Control>,
    ) -> Result<Box<dyn SteamCloud>, Error>;
    fn rosetta(helper: &Path) -> Result<bool, Core>;
}

/// The machine SailOver runs on.
pub(crate) struct Mac;
impl Machine for Mac {
    fn select(target: &Target, config: &Config) -> Result<Option<Architecture>, Error> {
        if config.wine.is_some() {
            return Ok(Some(crate::wine::ARCHITECTURE));
        }
        Architecture::select(&target.executable, &config.steam_api).map(Some)
    }
    fn prepare(
        launch: &crate::Launch,
        config: Config,
        storage: Option<NativeStorage>,
    ) -> Result<Runtime, Error> {
        launch.prepare(config, storage)
    }
    fn publish(runtime: &Runtime, active: bool) {
        let _ = runtime.set_presence(active);
    }
    fn cloud(
        config: cloud::Config,
        credentials: Credentials,
        id: String,
        control: Arc<crate::cloud::Control>,
    ) -> Result<Box<dyn SteamCloud>, Error> {
        Ok(Box::new(cloud::open(config, credentials, id, control)?))
    }
    fn rosetta(helper: &Path) -> Result<bool, Core> {
        crate::supervisor::architecture::rosetta_available(helper)
    }
}

/// Steam Cloud as a launch uses it: its prepared sync also names the SDK
/// Cloud storage the native host serves.
pub(crate) trait SteamCloud: LaunchCloud<Error> {
    fn native_storage(&self) -> Result<Option<NativeStorage>, Error>;
}
impl SteamCloud for cloud::Session {
    fn native_storage(&self) -> Result<Option<NativeStorage>, Error> {
        self.store().native_storage()
    }
}

pub(crate) struct Launch<M: Machine = Mac> {
    request: Request,
    /// Zeroized on drop. Only the host and Steam Cloud receive them.
    credentials: Credentials,
    launch: Arc<crate::Launch>,
    machine: PhantomData<M>,
}

impl<M: Machine> session::Launch for Launch<M> {
    type P = Steam;
    type Host = Runtime;
    type Cloud = Box<dyn SteamCloud>;

    fn open(input: &mut dyn Read) -> Result<(Opening<Steam>, Self), Error> {
        let credentials = Credentials::read(&mut *input)?;
        let request: Request = storage::packet(input)?;
        if uuid::Uuid::parse_str(&request.id).is_err()
            || request.config.app_id == 0
            || request.cloud.as_ref().is_some_and(|cloud| {
                cloud.app_id != request.config.app_id
                    || cloud.prefix.as_ref()
                        != request.config.wine.as_ref().map(|wine| &wine.prefix)
            })
        {
            return Err(Error::InvalidSession);
        }
        // The Rosetta helper app sits beside the storefront helpers directory.
        let helpers = request
            .config
            .helpers
            .parent()
            .unwrap_or(&request.config.helpers);
        let opening = Opening {
            root: request.root.clone(),
            id: request.id.clone(),
            account_id: credentials.steam_id,
            store: Store {
                app_id: request.config.app_id,
                host: HostState::Online,
            },
            helpers: helpers.into(),
        };
        let launch = Self {
            launch: Arc::new(crate::Launch::new(credentials.clone())),
            credentials,
            request,
            machine: PhantomData,
        };
        Ok((opening, launch))
    }

    fn interrupt(&self) -> Box<dyn Fn() + Send + Sync> {
        let launch = self.launch.clone();
        Box::new(move || launch.cancel())
    }

    fn wine(&self) -> Option<crate::wine::Config> {
        self.request.config.wine.clone()
    }

    async fn cloud(
        &mut self,
        session: &Arc<Session<Steam>>,
    ) -> Result<Option<Box<dyn SteamCloud>>, Error> {
        let Some(config) = self.request.cloud.clone() else {
            return Ok(None);
        };
        Ok(Some(M::cloud(
            config,
            self.credentials.clone(),
            self.request.id.clone(),
            session.cloud().clone(),
        )?))
    }

    async fn target(
        &mut self,
        _: &Arc<Session<Steam>>,
    ) -> Result<(Target, Option<Architecture>), Error> {
        let architecture = M::select(&self.request.target, &self.request.config)?;
        Ok((self.request.target.clone(), architecture))
    }

    async fn prepare_host(
        &mut self,
        _: &Arc<Session<Steam>>,
        target: &Target,
        architecture: Option<Architecture>,
        cloud: Option<&Box<dyn SteamCloud>>,
    ) -> Result<Prepared<Runtime>, Error> {
        let storage = match cloud {
            Some(cloud) => cloud.native_storage()?,
            None => None,
        };
        let (launch, config) = (self.launch.clone(), self.request.config.clone());
        let runtime = tokio::task::spawn_blocking(move || M::prepare(&launch, config, storage))
            .await
            .map_err(|_| Error::StartFailed)??;
        // The Intel helper comes from the prepared runtime; arm64 games use
        // the bundled host's gated exec.
        let (helper, subcommand) = if architecture == Some(Architecture::Intel) {
            (
                runtime._prepared.directory.join(package::SDK_INTEL),
                "exec-game",
            )
        } else {
            (self.request.config.helpers.join(package::HOST), "game-exec")
        };
        let app_id = self.request.config.app_id.to_string();
        let mut environment = target.environment.clone();
        environment.extend(runtime.environment());
        environment.insert("SteamAppId".into(), app_id.clone());
        environment.insert("SteamGameId".into(), app_id);
        if self.request.config.wine.is_some() {
            // lsteamclient loads the Intel slice of the host's steamclient.dylib.
            environment.insert(
                "STEAM_COMPAT_CLIENT_INSTALL_PATH".into(),
                runtime._prepared.directory.to_string_lossy().into_owned(),
            );
        }
        Ok(Prepared {
            host: runtime,
            helper,
            subcommand,
            environment,
        })
    }

    fn running(&self, host: &Arc<Runtime>, pid: u32) -> Box<dyn Fn(bool) + Send + Sync> {
        let _ = host.started(pid);
        let host = host.clone();
        Box::new(move |active| M::publish(&host, active))
    }

    fn watch(
        &self,
        host: &Arc<Runtime>,
        session: &Arc<Session<Steam>>,
    ) -> Option<std::thread::JoinHandle<()>> {
        let (host, session) = (host.clone(), session.clone());
        Some(std::thread::spawn(move || {
            let mut previous = HostState::Online;
            loop {
                let next = host.next_state(previous);
                let _ = session.update(|record| record.store.host = next);
                if matches!(next, HostState::Stopped | HostState::Lost) {
                    break;
                }
                previous = next;
            }
        }))
    }

    fn stopped(&self, host: &Runtime, exit: Option<i32>) {
        host.stopped(exit);
    }

    fn terminal(record: &mut Snapshot) {
        record.store.host = HostState::Stopped;
    }

    fn rosetta(helper: &Path) -> Result<bool, Core> {
        M::rosetta(helper)
    }
}

/// Games ship the Steam API library beside their executable or inside their
/// bundle. Links are never followed, and the search stays bounded.
pub(crate) fn steam_api(target: &sailover_core::launch::LaunchTarget) -> Result<PathBuf, Error> {
    library(target, &["libsteam_api.dylib"])
}

/// The first file under the game's directory named like one of `names`,
/// ignoring case as Windows file names do.
pub(crate) fn library(
    target: &sailover_core::launch::LaunchTarget,
    names: &[&str],
) -> Result<PathBuf, Error> {
    const MAX_ENTRIES: usize = 20_000;
    let executable = Path::new(&target.executable)
        .canonicalize()
        .map_err(|_| Error::ApiMissing)?;
    let directory = if target.is_application_bundle {
        executable
    } else {
        executable.parent().ok_or(Error::ApiMissing)?.to_owned()
    };
    let mut pending = vec![directory.clone()];
    let mut inspected = 0;
    while let Some(folder) = pending.pop() {
        let mut entries: Vec<_> = std::fs::read_dir(&folder)
            .map_err(|_| Error::ApiMissing)?
            .filter_map(std::result::Result::ok)
            .collect();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            inspected += 1;
            if inspected > MAX_ENTRIES {
                return Err(Error::ApiMissing);
            }
            let name = entry.file_name();
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file()
                && names
                    .iter()
                    .any(|library| name.to_string_lossy().eq_ignore_ascii_case(library))
                && let Ok(library) = entry.path().canonicalize()
                && library.starts_with(&directory)
            {
                return Ok(library);
            }
        }
    }
    Err(Error::ApiMissing)
}

#[cfg(test)]
pub(crate) mod tests;

/// The `sailover-steam-host supervise` entry point.
pub fn serve(
    input: impl Read,
    output: impl std::io::Write,
    inspect: session::Inspect,
) -> Result<(), Error> {
    session::serve::<Launch>(input, output, inspect)
}
