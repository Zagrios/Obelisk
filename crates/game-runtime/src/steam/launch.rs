//! A Steam launch: the native host that owns the game's Steam session, Steam
//! Cloud, and the game slice chosen against the pinned Steam API library.
use super::*;
use crate::{
    package,
    supervisor::{
        Error as Core,
        architecture::Architecture,
        session::{self, LaunchCloud, Opening, Prepared, Session},
        storage,
    },
};
use host::{Host, HostUpdate, NativeStorage, Startup};
use std::{io::Read, marker::PhantomData, sync::Arc};

/// Makes `ISteamUtils::IsOverlayEnabled` true for a game in Wine, so it asks
/// Steam for its overlay (callback 1010001) and Obelisk shows its own: many
/// games hide their invite or store buttons otherwise. The game's own
/// steamclient answers it, never the host. In the pinned steamclient.dylib,
/// that answer is `getenv("SteamOS") != NULL`, and only without it the
/// `IsOverlayEnabled` export of a loaded overlay renderer, through `dlsym`:
/// arm64 0xa060b8 (string 0x15a3dd0), x86_64 0x9c6586 (string 0x16af134),
/// the slice lsteamclient loads. Neither checks the value.
pub(crate) const OVERLAY_ENABLED: (&str, &str) = ("SteamOS", "1");

/// What a Steam launch asks of the machine. Tests replace it.
pub(crate) trait Machine: Send + Sync + 'static {
    /// The game slice to launch against the pinned Steam API library.
    fn select(target: &Target, config: &Config) -> Result<Option<Architecture>, Error>;
    /// Starts the native host for this account and game.
    fn prepare(
        startup: &Startup,
        config: Config,
        storage: Option<NativeStorage>,
    ) -> Result<Host, Error>;
    fn publish(host: &Host, active: bool);
    fn cloud(
        config: cloud::Config,
        credentials: Credentials,
        id: String,
        control: Arc<crate::cloud::Control>,
    ) -> Result<Box<dyn SteamCloud>, Error>;
    fn rosetta(helper: &Path) -> Result<bool, Core>;
}

/// The machine Obelisk runs on.
pub(crate) struct Mac;
impl Machine for Mac {
    fn select(target: &Target, config: &Config) -> Result<Option<Architecture>, Error> {
        if let Some(wine) = &config.wine {
            return Ok(Some(wine.architecture()));
        }
        let library = config.steam_api.as_deref().ok_or(Error::ApiMissing)?;
        Ok(Some(Architecture::select(&target.executable, library)?))
    }
    fn prepare(
        startup: &Startup,
        config: Config,
        storage: Option<NativeStorage>,
    ) -> Result<Host, Error> {
        startup.prepare(config, storage)
    }
    fn publish(host: &Host, active: bool) {
        let _ = host.set_presence(active);
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
    startup: Arc<Startup>,
    machine: PhantomData<M>,
}

impl<M: Machine> session::Launch for Launch<M> {
    type P = Steam;
    /// None when Valve's Windows Steam starts the game, which needs no host.
    type Host = Option<Host>;
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
        let helpers = crate::supervisor::rosetta::helpers(&request.config.helpers)
            .unwrap_or(&request.config.helpers);
        let opening = Opening {
            root: request.root.clone(),
            id: request.id.clone(),
            account_id: credentials.steam_id,
            store: Store::new(request.config.app_id),
            helpers: helpers.into(),
        };
        let launch = Self {
            startup: Arc::new(Startup::new(credentials.clone())),
            credentials,
            request,
            machine: PhantomData,
        };
        Ok((opening, launch))
    }

    fn interrupt(&self) -> Box<dyn Fn() + Send + Sync> {
        let startup = self.startup.clone();
        Box::new(move || startup.cancel())
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
    ) -> Result<Prepared<Option<Host>>, Error> {
        // Valve's Windows Steam signs in and stands beside the game itself;
        // its launcher starts in Rosetta's Wine, as a GOG game's Wine does.
        if super::windows_steam(self.request.config.wine.as_ref()) {
            return Ok(Prepared {
                host: None,
                helper: self.request.config.helpers.join(package::HOST),
                subcommand: "game-exec",
                environment: target.environment.clone(),
            });
        }
        let storage = match cloud {
            Some(cloud) => cloud.native_storage()?,
            None => None,
        };
        let (startup, config) = (self.startup.clone(), self.request.config.clone());
        let host = tokio::task::spawn_blocking(move || M::prepare(&startup, config, storage))
            .await
            .map_err(|_| Error::StartFailed)??;
        // The Intel helper comes from the prepared host's components; arm64
        // games use the bundled host's gated exec.
        let (helper, subcommand) = if architecture == Some(Architecture::Intel) {
            (host.directory().join(package::SDK_INTEL), "exec-game")
        } else {
            (self.request.config.helpers.join(package::HOST), "game-exec")
        };
        let mut environment = target.environment.clone();
        environment.extend(host.environment());
        game_environment(
            &mut environment,
            self.request.config.app_id,
            self.request.config.wine.is_some(),
            host.directory(),
        );
        Ok(Prepared {
            host: Some(host),
            helper,
            subcommand,
            environment,
        })
    }

    fn running(&self, host: &Arc<Option<Host>>, pid: u32) -> Box<dyn Fn(bool) + Send + Sync> {
        if let Some(host) = host.as_ref() {
            let _ = host.started(pid);
        }
        let host = host.clone();
        Box::new(move |active| {
            if let Some(host) = host.as_ref() {
                M::publish(host, active);
            }
        })
    }

    fn control(&self, host: &Arc<Option<Host>>) -> Option<session::Control<Steam>> {
        host.as_ref().as_ref()?;
        let host = host.clone();
        Some(Box::new(move |control| match host.as_ref() {
            Some(host) => host.control(control),
            None => Err(Core::Unavailable),
        }))
    }

    fn watch(
        &self,
        host: &Arc<Option<Host>>,
        session: &Arc<Session<Steam>>,
    ) -> Option<std::thread::JoinHandle<()>> {
        host.as_ref().as_ref()?;
        let (host, session) = (host.clone(), session.clone());
        Some(std::thread::spawn(move || {
            let Some(host) = host.as_ref() else {
                return;
            };
            let mut previous = HostUpdate::new(HostState::Online);
            loop {
                let next = host.next_update(&previous);
                let _ = session.update(|record| record.store.apply(&next));
                if matches!(next.state, HostState::Stopped | HostState::Lost) {
                    break;
                }
                previous = next;
            }
        }))
    }

    fn stopped(&self, host: &Option<Host>, exit: Option<i32>) {
        if let Some(host) = host {
            host.stopped(exit);
        }
    }

    fn terminal(record: &mut Snapshot) {
        record.store.apply(&HostUpdate::new(HostState::Stopped));
    }

    fn rosetta(helper: &Path) -> Result<bool, Core> {
        M::rosetta(helper)
    }
}

/// What the game's own Steam API reads, over the host's environment: its
/// AppID, and in Wine, the host's engine and an enabled overlay.
fn game_environment(
    environment: &mut std::collections::HashMap<String, String>,
    app_id: u32,
    wine: bool,
    directory: &Path,
) {
    let app_id = app_id.to_string();
    environment.insert("SteamAppId".into(), app_id.clone());
    environment.insert("SteamGameId".into(), app_id);
    if wine {
        let (name, value) = OVERLAY_ENABLED;
        environment.insert(name.into(), value.into());
        // lsteamclient loads the Intel slice of the host's steamclient.dylib.
        environment.insert(
            "STEAM_COMPAT_CLIENT_INSTALL_PATH".into(),
            directory.to_string_lossy().into_owned(),
        );
    }
}

/// Games ship the Steam API library beside their executable or inside their
/// bundle. Links are never followed, and the search stays bounded.
pub(crate) fn steam_api(target: &obelisk_core::launch::LaunchTarget) -> Result<PathBuf, Error> {
    library(target, &["libsteam_api.dylib"])
}

/// The first file under the game's directory, its bundle or its executable's
/// folder, named like one of `names`.
fn library(target: &obelisk_core::launch::LaunchTarget, names: &[&str]) -> Result<PathBuf, Error> {
    let executable = Path::new(&target.executable)
        .canonicalize()
        .map_err(|_| Error::ApiMissing)?;
    if target.is_application_bundle {
        return library_in(&executable, names);
    }
    library_in(executable.parent().ok_or(Error::ApiMissing)?, names)
}

/// The first file under `directory` named like one of `names`, ignoring case.
fn library_in(directory: &Path, names: &[&str]) -> Result<PathBuf, Error> {
    const MAX_ENTRIES: usize = 20_000;
    let directory = directory.canonicalize().map_err(|_| Error::ApiMissing)?;
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

/// The `obelisk-steam-host supervise` entry point.
pub fn serve(
    input: impl Read,
    output: impl std::io::Write,
    inspect: session::Inspect,
) -> Result<(), Error> {
    session::serve::<Launch>(input, output, inspect)
}
