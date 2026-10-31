//! What Obelisk hands a new supervisor and what that supervisor makes of it.
use super::{Launchable, Prerequisites, Refusals, validate};
use crate::{
    diagnostics, files, native,
    supervisor::{
        self, Target,
        architecture::Architecture,
        session::{Opening, Prepared},
        storage,
    },
    wine,
};
use obelisk_core::{Platform, launch::LaunchTarget, transfer::CancelToken};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, io::Read};

/// What the handoff pipe carries to a supervisor. The credential travels
/// solely through the private pipe; the request is never persisted, formatted
/// with Debug, or put in argv. Wire: `config, id, account_id, <credential's
/// keys>, environment, wine`.
#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub(crate) struct Request<G: Launchable> {
    pub config: G::Configuration,
    pub id: String,
    pub account_id: String,
    #[serde(flatten)]
    pub credential: G::Credential,
    /// Built in Obelisk's process: a supervisor starts without any
    /// environment.
    #[serde(default)]
    pub environment: HashMap<String, String>,
    /// How a Windows build runs.
    #[serde(default)]
    pub wine: Option<wine::Config>,
}

/// Obelisk's side: `validate`, then `runs_in` (else `Unsupported`), then
/// spawns the supervisor with an empty environment, hands it one request and
/// returns once it acknowledged that launch (the acknowledgement names this
/// account and this `G::store`). `wine` is how a Windows build runs, from
/// `ready`, and must be absent for a Mac build.
pub(crate) fn start<G: Launchable>(
    configuration: G::Configuration,
    account_id: String,
    credential: G::Credential,
    wine: Option<wine::Config>,
    cancel: CancelToken,
) -> Result<supervisor::Client<G>, G::Error> {
    validate::<G>(&configuration, &account_id)?;
    let build = G::build(&configuration);
    if !build.runs_in(wine.as_ref()) {
        return Err(G::Error::unsupported());
    }
    let store = G::store(&configuration);
    let request = Request::<G>::new(configuration, account_id, credential, wine);
    let build = G::build(&request.config);
    Ok(supervisor::Client::<G>::start_with(
        build.session_directory,
        build.helper,
        request.id.clone(),
        supervisor::Client::<G>::supervisor_command(build.helper),
        supervisor::client::inspect,
        |pipe| storage::send(pipe, &request),
        |ack| ack.account_id == request.account_id && ack.store == store,
        || cancel.is_cancelled(),
    )?)
}

impl<G: Launchable> Request<G> {
    /// The supervisor's side: reads one request and checks it as `start`
    /// would. `validate` first (`Unsupported`), then the session's own facts
    /// (`Session`: the id is not a uuid, Wine and the prefix do not match, the
    /// helper sits beside no folder of helpers). Not a request at all is
    /// `Session` too.
    pub fn open(input: &mut dyn Read) -> Result<(Opening<G>, Self), G::Error> {
        let request: Self = storage::packet(input)?;
        validate::<G>(&request.config, &request.account_id)?;
        let build = G::build(&request.config);
        if uuid::Uuid::parse_str(&request.id).is_err() || !build.runs_in(request.wine.as_ref()) {
            return Err(G::Error::session());
        }
        let opening = Opening {
            root: build.session_directory.to_owned(),
            id: request.id.clone(),
            account_id: request.account_id.clone(),
            store: G::store(&request.config),
            helpers: supervisor::rosetta::helpers_of_host(build.helper)
                .ok_or_else(G::Error::session)?
                .into(),
        };
        Ok((opening, request))
    }

    /// The build's target: Wine in its prefix for a Windows build, natively
    /// for a Mac build, with the request's environment. `launched` is what the
    /// installation (GOG) or the online plan (Epic) says to start. `file` is a
    /// private file the game takes the path of as an argument, placed where
    /// the engine reads it and kept until `Started::kept` drops.
    pub fn target(
        &self,
        launched: &LaunchTarget,
        file: Option<ArgumentFile<'_>>,
    ) -> Result<Started, G::Error> {
        let build = G::build(&self.config);
        match &self.wine {
            Some(wine) => {
                let mut target = wine::target::<Prerequisites>(
                    wine,
                    &build.wine_game(),
                    launched,
                    self.environment.clone(),
                )?;
                let kept = match file {
                    Some(file) => {
                        let (directory, path) = wine::private_file(wine, file.name, file.bytes)?;
                        target.arguments.push(format!("{}={path}", file.argument));
                        Some(directory)
                    }
                    None => None,
                };
                Ok(Started {
                    target,
                    architecture: wine.architecture(),
                    kept,
                })
            }
            None => {
                let mut arguments = launched.arguments.clone();
                let kept = match file {
                    Some(file) => {
                        let (directory, path) = files::private_temp_file(
                            &std::env::temp_dir(),
                            file.folder,
                            file.name,
                            file.bytes,
                        )?;
                        arguments.push(format!("{}={}", file.argument, path.display()));
                        Some(directory)
                    }
                    None => None,
                };
                let (target, architecture) = native::target(
                    build.install,
                    launched,
                    arguments,
                    self.environment.clone(),
                    diagnostics::game_log(build.home, build.game.into(), Platform::Mac),
                )?;
                Ok(Started {
                    target,
                    architecture,
                    kept,
                })
            }
        }
    }

    /// A request to start `configuration`, with an id of its own and the
    /// environment of its home.
    pub fn new(
        configuration: G::Configuration,
        account_id: String,
        credential: G::Credential,
        wine: Option<wine::Config>,
    ) -> Self {
        Self {
            environment: native::environment(G::build(&configuration).home),
            config: configuration,
            id: uuid::Uuid::new_v4().to_string(),
            account_id,
            credential,
            wine,
        }
    }

    /// The prepared game: run through the request's helper's `game-exec` with
    /// the target's environment, `host` kept until the game exits.
    pub fn prepared<H>(&self, host: H, target: &Target) -> Prepared<H> {
        Prepared {
            host,
            helper: G::build(&self.config).helper.to_owned(),
            subcommand: "game-exec",
            environment: target.environment.clone(),
        }
    }
}

/// A private file the game reads, given as `argument=<path>`, last of the
/// arguments. Epic's ownership token: argument `-epicovt`, name
/// `ownership.bin`, folder `obelisk-epic-ownership-`.
pub(crate) struct ArgumentFile<'a> {
    pub argument: &'static str,
    /// The prefix of the private folder a Mac build keeps it in.
    pub folder: &'static str,
    pub name: &'static str,
    pub bytes: &'a [u8],
}

/// What a request starts: the target, the architecture it runs as, and the
/// private folder that holds its `ArgumentFile`.
pub(crate) struct Started {
    pub target: Target,
    pub architecture: Architecture,
    /// The private folder holding the `ArgumentFile`: it lives as long as the
    /// game.
    pub kept: Option<tempfile::TempDir>,
}
