//! An Epic launch: its own Epic session from a one-use exchange code, EGS
//! Cloud, the launch plan and its ownership token, and Epic presence.
use super::session as cloud_session;
use super::*;
use crate::supervisor::{
    Error as Core,
    architecture::Architecture,
    session::{self as lifecycle, LaunchCloud, Opening, Prepared, Session},
    storage,
};
use epic_kit::{client::LaunchPlan, friends::Activity};
use std::{
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::Path,
};

/// The Epic services a launch uses. Tests replace them.
#[allow(async_fn_in_trait)]
pub(crate) trait Services: Sized + Send + Sync + 'static {
    /// Opens the launch's own Epic session; never the UI's remembered token.
    async fn login(exchange_code: &str, account_id: &str) -> Result<Arc<Self>>;
    async fn launch_plan(&self, config: &Configuration) -> Result<LaunchPlan>;
    fn start_presence(&self, activity: Activity);
    fn stop_presence(&self);
    async fn disconnect(&self);
    /// EGS Cloud for this game and Game session.
    async fn cloud(
        self: &Arc<Self>,
        config: Configuration,
        account_id: String,
        session_id: String,
        control: Arc<Control>,
    ) -> Result<Box<dyn LaunchCloud<Error>>>;
    fn rosetta(helper: &Path) -> std::result::Result<bool, Core>;
}

impl Services for epic_kit::client::Client {
    async fn login(exchange_code: &str, account_id: &str) -> Result<Arc<Self>> {
        Ok(Arc::new(
            Self::from_exchange_code(exchange_code, account_id).await?,
        ))
    }
    async fn launch_plan(&self, config: &Configuration) -> Result<LaunchPlan> {
        Ok(epic_kit::client::Client::launch_plan(
            self,
            &config.namespace,
            &config.catalog_item_id,
            &config.app_name,
            &config.platform,
            &config.install,
            &config.language,
        )
        .await?)
    }
    fn start_presence(&self, activity: Activity) {
        // Social availability never changes the game's lifetime.
        let _ = self.start_game_presence(activity);
    }
    fn stop_presence(&self) {
        self.stop_friends();
    }
    async fn disconnect(&self) {
        let _ = epic_kit::client::Client::disconnect(self).await;
    }
    async fn cloud(
        self: &Arc<Self>,
        config: Configuration,
        account_id: String,
        session_id: String,
        control: Arc<Control>,
    ) -> Result<Box<dyn LaunchCloud<Error>>> {
        let sync = cloud_session::open(config, account_id, Arc::clone(self), control, session_id)?;
        Ok(Box::new(sync))
    }
    fn rosetta(helper: &Path) -> std::result::Result<bool, Core> {
        crate::supervisor::architecture::rosetta_available(helper)
    }
}

pub(crate) struct Launch<S: Services = epic_kit::client::Client> {
    request: Request,
    api: Option<Arc<S>>,
    activity: Option<Activity>,
    ownership: Option<tempfile::TempDir>,
}

/// Keeps the launch plan's ownership token until the game exits.
pub(crate) struct Host {
    _ownership: Option<tempfile::TempDir>,
}

impl<S: Services> Launch<S> {
    fn api(&self) -> Result<&Arc<S>> {
        self.api.as_ref().ok_or(Error::Session)
    }
}

impl<S: Services> lifecycle::Launch for Launch<S> {
    type P = Epic;
    type Host = Host;
    type Cloud = Box<dyn LaunchCloud<Error>>;
    const BEGIN: bool = true;

    fn wine(&self) -> Option<crate::wine::Config> {
        self.request.wine.clone()
    }

    fn open(input: &mut dyn Read) -> Result<(Opening<Epic>, Self)> {
        let request: Request = storage::packet(input)?;
        request.config.validate(&request.account_id)?;
        if uuid::Uuid::parse_str(&request.id).is_err()
            || !request.config.runs_in(request.wine.as_ref())
        {
            return Err(Error::Session);
        }
        let opening = Opening {
            root: request.config.session_directory.clone(),
            id: request.id.clone(),
            account_id: request.account_id.clone(),
            store: Store {
                namespace: request.config.namespace.clone(),
                app_name: request.config.app_name.clone(),
            },
            helpers: helpers(&request.config)?,
        };
        let launch = Self {
            request,
            api: None,
            activity: None,
            ownership: None,
        };
        Ok((opening, launch))
    }

    async fn authenticate(&mut self, _: &Arc<Session<Epic>>) -> Result<()> {
        let api = S::login(&self.request.exchange_code, &self.request.account_id).await?;
        self.api = Some(api);
        Ok(())
    }

    async fn cloud(
        &mut self,
        session: &Arc<Session<Epic>>,
    ) -> Result<Option<Box<dyn LaunchCloud<Error>>>> {
        let api = self.api()?.clone();
        Ok(Some(
            api.cloud(
                self.request.config.clone(),
                self.request.account_id.clone(),
                self.request.id.clone(),
                session.cloud().clone(),
            )
            .await?,
        ))
    }

    async fn target(
        &mut self,
        session: &Arc<Session<Epic>>,
    ) -> Result<(Target, Option<Architecture>)> {
        let plan = self.api()?.launch_plan(&self.request.config).await?;
        session.cloud().check()?;
        // A fresh plan, after Rosetta was installed, drops the old token.
        let request = &self.request;
        let (target, architecture, ownership) = match &request.wine {
            Some(wine) => windows_target(&plan, &request.config, wine, &request.environment)?,
            None => native_target(&plan, &request.config, &request.environment)?,
        };
        self.activity = plan.activity;
        self.ownership = ownership;
        Ok((target, Some(architecture)))
    }

    async fn prepare_host(
        &mut self,
        _: &Arc<Session<Epic>>,
        target: &Target,
        _: Option<Architecture>,
        _: Option<&Box<dyn LaunchCloud<Error>>>,
    ) -> Result<Prepared<Host>> {
        Ok(Prepared {
            host: Host {
                _ownership: self.ownership.take(),
            },
            helper: self.request.config.helper.clone(),
            subcommand: "game-exec",
            environment: target.environment.clone(),
        })
    }

    fn running(&self, _: &Arc<Host>, _: u32) -> Box<dyn Fn(bool) + Send + Sync> {
        let (Ok(api), activity) = (self.api(), self.activity.clone()) else {
            return Box::new(|_| {});
        };
        let api = api.clone();
        Box::new(move |active| match (&activity, active) {
            (Some(activity), true) => api.start_presence(activity.clone()),
            (_, true) => {}
            (_, false) => api.stop_presence(),
        })
    }

    fn stopped(&self, _: &Host, _: Option<i32>) {}

    async fn close(&mut self) {
        if let Some(api) = &self.api {
            api.disconnect().await;
        }
    }

    fn terminal(record: &mut Snapshot) {
        record.cloud.allowed_choices.clear();
        if record.phase == Phase::Failed
            && !matches!(
                record.cloud.phase,
                CloudPhase::Conflict | CloudPhase::Pending
            )
        {
            record.cloud.phase = CloudPhase::Failed;
        }
    }

    fn rosetta(helper: &Path) -> std::result::Result<bool, Core> {
        S::rosetta(helper)
    }
}

/// `config.helper` is `Helpers/SteamNative/sailover-steam-host`; the Rosetta
/// helper app sits beside `SteamNative`, as for Steam.
fn helpers(config: &Configuration) -> Result<PathBuf> {
    Ok(config
        .helper
        .parent()
        .and_then(Path::parent)
        .ok_or(Error::Session)?
        .into())
}

/// The plan's ownership token in a private folder that lives as long as the
/// game, and the file that holds it.
fn ownership_token(plan: &LaunchPlan) -> Result<Option<(tempfile::TempDir, PathBuf)>> {
    let Some(bytes) = &plan.ownership_token else {
        return Ok(None);
    };
    let directory = tempfile::Builder::new()
        .prefix("sailover-epic-ownership-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()?;
    let path = directory.path().join("ownership.bin");
    crate::files::atomic_write(&path, crate::files::Durability::FileSync, |f| {
        f.write_all(bytes)?;
        Ok(())
    })?;
    Ok(Some((directory, path)))
}

/// A Windows build, which Wine starts in its prefix.
pub(super) fn windows_target(
    plan: &LaunchPlan,
    config: &Configuration,
    wine: &crate::wine::Config,
    environment: &std::collections::HashMap<String, String>,
) -> Result<(Target, Architecture, Option<tempfile::TempDir>)> {
    let mut target = crate::wine::target(&config.install, &plan.target, environment.clone(), wine)?;
    let ownership = ownership_token(plan)?.map(|(directory, path)| {
        // The game reads the token through Wine's view of the Mac's disk.
        target
            .arguments
            .push(format!("-epicovt={}", crate::wine::windows_path(&path)));
        directory
    });
    Ok((target, crate::wine::ARCHITECTURE, ownership))
}

pub(super) fn native_target(
    plan: &LaunchPlan,
    config: &Configuration,
    environment: &std::collections::HashMap<String, String>,
) -> Result<(Target, Architecture, Option<tempfile::TempDir>)> {
    let (executable, architecture) = crate::native::executable(&config.install, &plan.target)?;
    let mut arguments = plan.target.arguments.clone();
    let ownership = ownership_token(plan)?.map(|(directory, path)| {
        arguments.push(format!("-epicovt={}", path.display()));
        directory
    });
    Ok((
        Target {
            executable,
            arguments,
            working_directory: plan.target.working_directory.clone().into(),
            environment: environment.clone(),
            log: None,
        },
        architecture,
        ownership,
    ))
}

/// The `sailover-steam-host epic-supervise` entry point.
pub fn serve(input: impl Read, output: impl Write, inspect: lifecycle::Inspect) -> Result<()> {
    lifecycle::serve::<Launch>(input, output, inspect)
}

#[cfg(test)]
pub(crate) mod tests;
