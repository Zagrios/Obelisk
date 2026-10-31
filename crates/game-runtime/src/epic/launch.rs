//! An Epic launch: its own Epic session from a one-use exchange code, EGS
//! Cloud, the launch plan and its ownership token, and Epic presence.
use super::session as cloud_session;
use super::*;
use crate::launch_shell::{ArgumentFile, Request, Started};
use crate::supervisor::{
    Error as Core,
    architecture::Architecture,
    session::{self as lifecycle, LaunchCloud, Opening, Prepared, Session},
};
use epic_kit::{client::LaunchPlan, friends::Activity};
use std::{
    io::{Read, Write},
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
    /// Stops the launch's Epic connections but never revokes its session:
    /// the game's own session came from it, and outlives the launch when the
    /// game's program exits before the game does. Epic's launcher never
    /// revokes the session it launches games from either.
    fn release(&self);
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
            config.platform,
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
    fn release(&self) {
        self.shutdown_observers();
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
    request: Request<Epic>,
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
        let (opening, request) = Request::<Epic>::open(input)?;
        let launch = Self {
            request,
            api: None,
            activity: None,
            ownership: None,
        };
        Ok((opening, launch))
    }

    async fn authenticate(&mut self, _: &Arc<Session<Epic>>) -> Result<()> {
        let api = S::login(
            &self.request.credential.exchange_code,
            &self.request.account_id,
        )
        .await?;
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
        let started = started(&self.request, &plan)?;
        self.activity = plan.activity;
        self.ownership = started.kept;
        Ok((started.target, Some(started.architecture)))
    }

    async fn prepare_host(
        &mut self,
        _: &Arc<Session<Epic>>,
        target: &Target,
        _: Option<Architecture>,
        _: Option<&Box<dyn LaunchCloud<Error>>>,
    ) -> Result<Prepared<Host>> {
        Ok(self.request.prepared(
            Host {
                _ownership: self.ownership.take(),
            },
            target,
        ))
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
            api.release();
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

/// What `request` starts of `plan`: its target, with the ownership token it
/// carries.
fn started(request: &Request<Epic>, plan: &LaunchPlan) -> Result<Started> {
    request.target(&plan.target, ownership(plan))
}

/// The plan's ownership token, which the game reads from a private file its
/// `-epicovt` argument names.
fn ownership(plan: &LaunchPlan) -> Option<ArgumentFile<'_>> {
    plan.ownership_token.as_deref().map(|bytes| ArgumentFile {
        argument: "-epicovt",
        folder: "obelisk-epic-ownership-",
        name: "ownership.bin",
        bytes,
    })
}

/// The `obelisk-steam-host epic-supervise` entry point.
pub fn serve(input: impl Read, output: impl Write, inspect: lifecycle::Inspect) -> Result<()> {
    lifecycle::serve::<Launch>(input, output, inspect)
}

#[cfg(test)]
pub(crate) mod tests;
