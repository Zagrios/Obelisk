//! An Epic launch: its own Epic session from a one-use exchange code, EGS
//! Cloud, the launch plan and its ownership token, and Epic presence.
use super::session as cloud_session;
use super::*;
use crate::supervisor::{
    Error as Core,
    architecture::Architecture,
    client::SessionAlive,
    session::{self as lifecycle, Opening, Operation, Prepared, Session},
    storage,
};
use epic_kit::{client::LaunchPlan, friends::Activity};
use std::{
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::Path,
    process::Command as Process,
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
    /// EGS Cloud for this game, unless its rules are unsupported.
    async fn cloud(
        self: &Arc<Self>,
        config: Configuration,
        account_id: String,
        control: Arc<Control>,
    ) -> Result<Option<Box<dyn EpicCloud>>>;
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
        control: Arc<Control>,
    ) -> Result<Option<Box<dyn EpicCloud>>> {
        Ok(
            cloud_session::Session::new(config, account_id, Arc::clone(self), control, true)
                .await?
                .map(|cloud| Box::new(cloud) as Box<dyn EpicCloud>),
        )
    }
    fn rosetta(helper: &Path) -> std::result::Result<bool, Core> {
        crate::supervisor::architecture::rosetta_available(helper)
    }
}

/// EGS Cloud as a launch uses it.
pub(crate) trait EpicCloud: Send {
    fn prepare<'a>(&'a mut self, alive: &'a SessionAlive<'a>) -> Operation<'a, Result<()>>;
    fn started(&mut self, id: &str) -> Result<()>;
    /// Settles the exit save; `confirmed` is false when exec was never
    /// confirmed, and the save is then only captured and kept pending.
    fn finished<'a>(&'a mut self, id: &'a str, confirmed: bool) -> Operation<'a, ()>;
    /// Releases a launch abandoned before spawn. `started` tells whether the
    /// game guard was taken; `revoked` whether Account revocation ended it.
    fn abandoned(&mut self, id: &str, started: bool, revoked: bool);
}
impl<R: cloud_session::CloudRemote> EpicCloud for cloud_session::Session<R> {
    fn prepare<'a>(&'a mut self, alive: &'a SessionAlive<'a>) -> Operation<'a, Result<()>> {
        Box::pin(cloud_session::Session::prepare(self, alive))
    }
    fn started(&mut self, id: &str) -> Result<()> {
        cloud_session::Session::started(self, id)
    }
    fn finished<'a>(&'a mut self, id: &'a str, confirmed: bool) -> Operation<'a, ()> {
        Box::pin(async move {
            // The session's outcome is the game's: a Cloud failure lives in
            // the Cloud status.
            let _ = if confirmed {
                cloud_session::Session::finished(self, id).await
            } else {
                cloud_session::Session::aborted(self, id)
            };
        })
    }
    fn abandoned(&mut self, id: &str, started: bool, revoked: bool) {
        if started {
            let _ = cloud_session::Session::aborted(self, id);
        }
        if revoked {
            let _ = cloud_session::Session::revoked(self);
        }
    }
}

pub(crate) struct Launch<S: Services = epic_kit::client::Client> {
    request: Request,
    api: Option<Arc<S>>,
    cloud: Option<Box<dyn EpicCloud>>,
    started: bool,
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
    const BEGIN: bool = true;

    fn open(input: &mut dyn Read) -> Result<(Opening<Epic>, Self)> {
        let request: Request = storage::packet(input)?;
        request.config.validate(&request.account_id)?;
        if uuid::Uuid::parse_str(&request.id).is_err() {
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
            cloud: None,
            started: false,
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

    async fn prepare_cloud(
        &mut self,
        session: &Arc<Session<Epic>>,
        alive: &SessionAlive<'_>,
    ) -> Result<()> {
        let api = self.api()?.clone();
        self.cloud = api
            .cloud(
                self.request.config.clone(),
                self.request.account_id.clone(),
                session.cloud().clone(),
            )
            .await?;
        if let Some(cloud) = &mut self.cloud {
            cloud.prepare(alive).await?;
        }
        Ok(())
    }

    async fn target(
        &mut self,
        session: &Arc<Session<Epic>>,
    ) -> Result<(Target, Option<Architecture>)> {
        let plan = self.api()?.launch_plan(&self.request.config).await?;
        session.cloud().check()?;
        // A fresh plan, after Rosetta was installed, drops the old token.
        let (target, architecture, ownership) = native_target(&plan, &self.request.config)?;
        self.activity = plan.activity;
        self.ownership = ownership;
        Ok((target, Some(architecture)))
    }

    async fn prepare_host(
        &mut self,
        _: &Arc<Session<Epic>>,
        target: &Target,
        _: Option<Architecture>,
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

    fn admitted(&mut self) -> Result<()> {
        if let Some(cloud) = &mut self.cloud {
            cloud.started(&self.request.id)?;
            self.started = true;
        }
        Ok(())
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

    async fn finish(&mut self, _: &Arc<Session<Epic>>, confirmed: bool) {
        if let Some(cloud) = &mut self.cloud {
            cloud.finished(&self.request.id, confirmed).await;
        }
    }

    fn abort(&mut self, session: &Arc<Session<Epic>>) {
        let revoked = session.cloud().revoked();
        if let Some(cloud) = &mut self.cloud {
            cloud.abandoned(&self.request.id, self.started, revoked);
        }
    }

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

/// `config.helper` is `Helpers/SteamNative/forge-steam-host`; the Rosetta
/// helper app sits beside `SteamNative`, as for Steam.
fn helpers(config: &Configuration) -> Result<PathBuf> {
    Ok(config
        .helper
        .parent()
        .and_then(Path::parent)
        .ok_or(Error::Session)?
        .into())
}

pub(super) fn native_target(
    plan: &LaunchPlan,
    config: &Configuration,
) -> Result<(Target, Architecture, Option<tempfile::TempDir>)> {
    let root = config.install.canonicalize()?;
    let mut executable = Path::new(&plan.target.executable).canonicalize()?;
    if plan.target.is_application_bundle {
        let output = Process::new("/usr/bin/plutil")
            .args(["-extract", "CFBundleExecutable", "raw", "-o", "-"])
            .arg(executable.join("Contents/Info.plist"))
            .env_clear()
            .output()?;
        let name = std::str::from_utf8(&output.stdout)
            .map_err(|_| Error::Unsupported)?
            .trim();
        if !output.status.success()
            || name.is_empty()
            || name.len() > 255
            || name.contains('/')
            || name == "."
            || name == ".."
        {
            return Err(Error::Unsupported);
        }
        executable = executable
            .join("Contents/MacOS")
            .join(name)
            .canonicalize()?;
    }
    if !executable.starts_with(&root) || !executable.is_file() {
        return Err(Error::Unsupported);
    }
    // The installer may omit execute permission for a Mach-O. Match the shared
    // native launcher: repair only the selected executable after verification.
    let architecture = Architecture::preferred(&executable)?;
    let metadata = std::fs::metadata(&executable)?;
    if metadata.permissions().mode() & 0o100 == 0 {
        std::fs::set_permissions(
            &executable,
            std::fs::Permissions::from_mode(metadata.permissions().mode() | 0o100),
        )?;
    }
    let mut arguments = plan.target.arguments.clone();
    let ownership = if let Some(bytes) = &plan.ownership_token {
        let directory = tempfile::Builder::new()
            .prefix("forge-epic-ownership-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()?;
        let path = directory.path().join("ownership.bin");
        crate::files::atomic_write(&path, crate::files::Durability::FileSync, |f| {
            f.write_all(bytes)?;
            Ok(())
        })?;
        arguments.push(format!("-epicovt={}", path.display()));
        Some(directory)
    } else {
        None
    };
    let environment = [
        ("HOME".into(), config.home.to_string_lossy().into_owned()),
        ("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()),
        (
            "TMPDIR".into(),
            std::env::temp_dir().to_string_lossy().into_owned(),
        ),
        ("LANG".into(), "en_US.UTF-8".into()),
    ]
    .into_iter()
    .collect();
    Ok((
        Target {
            executable,
            arguments,
            working_directory: plan.target.working_directory.clone().into(),
            environment,
        },
        architecture,
        ownership,
    ))
}

/// The `forge-steam-host epic-supervise` entry point.
pub fn serve(input: impl Read, output: impl Write, inspect: lifecycle::Inspect) -> Result<()> {
    lifecycle::serve::<Launch>(input, output, inspect)
}

#[cfg(test)]
pub(crate) mod tests;
