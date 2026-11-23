//! `run`: a GOG game, launched as Obelisk launches it, through the same
//! installations, Game session and supervisor.
use crate::resolve::{Installed, Kept};
use game_runtime::{
    gog::{self, Gog, content::Network},
    installations::{self, Installations, Report, Transfer},
    queue::DownloadQueue,
    runtimes::{Answer, Runtimes},
    sessions::{BoxFuture, Failure, Sessions, Storefront as _, Supervised},
    supervisor::{self, Phase},
};
use obelisk_core::installation::TransferProgress;
use std::{process::ExitCode, sync::Arc};

/// GOG's network as a launch from here sees it: the account its receipt book
/// names, no newer build, and nothing to download.
struct Offline(String);

impl Network for Offline {
    fn prepare(&self) -> BoxFuture<'_, Result<(), Failure>> {
        Box::pin(async { Ok(()) })
    }
    fn account_id(&self) -> BoxFuture<'_, Result<String, Failure>> {
        Box::pin(async { Ok(self.0.clone()) })
    }
    fn platform<'a>(
        &'a self,
        _: &'a gog::Product,
        _: &'a str,
        _: Option<obelisk_core::Platform>,
    ) -> BoxFuture<'a, Result<obelisk_core::Platform, Failure>> {
        Box::pin(async { Err(Failure::Failed(Gog::SESSION_FAILED.into())) })
    }
    fn download<'a>(
        &'a self,
        _: &'a Transfer<Gog>,
        _: &'a str,
    ) -> BoxFuture<'a, Result<gog_kit::client::Installation, Failure>> {
        Box::pin(async { Err(Failure::Failed(Gog::SESSION_FAILED.into())) })
    }
    fn progress(&self) -> Option<TransferProgress> {
        None
    }
    fn cancel(&self) {}
    fn update_available<'a>(
        &'a self,
        _: &'a installations::Receipt<Gog>,
    ) -> BoxFuture<'a, Result<bool, Failure>> {
        Box::pin(async { Ok(false) })
    }
}

pub(crate) fn run(
    found: &Installed,
    locations: &game_runtime::Locations,
) -> Result<ExitCode, String> {
    let Kept::Gog(receipt) = &found.receipt else {
        return Err(format!(
            "{} launches with its {} account's session, which only Obelisk holds: run launches GOG games",
            found.name(),
            found.store()
        ));
    };
    let (sessions_folder, helper) = (locations.gog_sessions.clone(), locations.helper.clone());
    if !helper.is_file() {
        return Err(format!(
            "{} is missing: --app names no Obelisk build",
            helper.display()
        ));
    }
    let sessions = Arc::new(Sessions::<Gog>::new(
        Box::new(move || {
            Ok(
                supervisor::Client::<Gog>::recover(&sessions_folder, &helper)
                    .map_err(|error| error.to_string())?
                    .map(|client| Arc::new(client) as Arc<dyn Supervised<Gog>>),
            )
        }),
        Runtimes::new(locations),
    ));
    let content = gog::content::Content::new(
        Arc::new(Offline(found.account.clone())),
        Arc::new(|| {}),
        "en".into(),
        locations.clone(),
    );
    let installations = Installations::new(
        Arc::new(content),
        sessions.clone(),
        found.account.clone(),
        found.book.clone(),
        locations.home.clone(),
        game_runtime::games_folder::GamesFolder::new(&locations.library),
        installations::Queueing {
            queue: DownloadQueue::new(),
            account: found.account.clone(),
            name: Clone::clone,
        },
    );
    let id = receipt.id();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?
        .block_on(follow(&installations, &sessions, &id))
}

/// Launches `id`, answers what its session asks, and reports each phase
/// until its game ends.
async fn follow(
    installations: &Installations<Gog>,
    sessions: &Sessions<Gog>,
    id: &str,
) -> Result<ExitCode, String> {
    let mut after = sessions.state().revision;
    while sessions.state().busy() {
        eprintln!("waiting for the session a previous Obelisk left");
        after = sessions.changed(after).await.revision;
    }
    installations.launch(id);
    if !installations.state().launching {
        return Err(
            "the game cannot launch now: it is busy in Obelisk, or its receipt is gone".into(),
        );
    }
    let mut shown = None;
    let mut started = false;
    loop {
        let state = sessions.changed(after).await;
        after = state.revision;
        if let Some(prompt) = &state.rosetta {
            sessions.decline_rosetta(&prompt.session, &prompt.account, prompt.decision_id);
            return Err(
                "the game needs Rosetta: install it with softwareupdate --install-rosetta".into(),
            );
        }
        let Some(active) = &state.active else {
            if !started {
                let report = installations.state().report;
                if installations.state().launching {
                    continue;
                }
                return Err(failure(report, "the launch stopped"));
            }
            let played = state.last.and_then(|last| last.played);
            // No exit code: a signal ended it.
            let code = played
                .as_ref()
                .and_then(|played| played.exit_code)
                .unwrap_or(1);
            eprintln!("the game ended ({played:?})");
            return Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)));
        };
        started = true;
        if let Some(update) = &active.voile_update {
            let answer = if update.required {
                Answer::Update
            } else {
                Answer::Skip
            };
            eprintln!("Voile {} is offered: {answer:?}", update.version);
            sessions
                .answer_voile_update(&active.id, answer)
                .map_err(|error| format!("{error:?}"))?;
        }
        if shown != Some(active.phase) {
            shown = Some(active.phase);
            eprintln!("{:?}", active.phase);
            if active.phase == Phase::Failed {
                return Err(failure(
                    installations.state().report,
                    "the game session failed",
                ));
            }
        }
    }
}

/// Why the launch failed, as its installations' `report` says, else
/// `otherwise`.
fn failure(report: Option<Report>, otherwise: &str) -> String {
    match report {
        Some(Report::Storefront(key)) => format!("the launch failed: {key}"),
        Some(report) => format!("the launch failed: {report:?}"),
        None => otherwise.into(),
    }
}
