//! A recording storefront: the core's lifecycle with nothing else around it.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Recording;
/// The same storefront, waiting for its UI's Begin.
pub(crate) struct Gated;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Game {
    name: String,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum Failure {
    #[error("fixture.cancelled")]
    Cancelled,
    #[error("fixture.{0:?}")]
    Core(Error),
}
impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        match error {
            Error::Cancelled => Self::Cancelled,
            error => Self::Core(error),
        }
    }
}

impl Provider for Recording {
    type Account = u64;
    type Store = Game;
    type Error = Failure;
    const SUPERVISE: &'static str = "unused";
    const CANCELLED: &'static [&'static str] = &["fixture.cancelled"];

    fn valid(_: &Record<Self>) -> bool {
        true
    }
    fn cancelled(error: &Failure) -> bool {
        matches!(error, Failure::Cancelled)
    }
    fn same_game(a: &Game, b: &Game) -> bool {
        a == b
    }
    fn orphaned(_: &mut Record<Self>, _: bool) {}
}

const ACCOUNT: u64 = 7;

#[derive(Serialize, Deserialize)]
struct Request {
    sessions: PathBuf,
    directory: PathBuf,
    id: String,
}

fn handoff(
    directory: &Path,
    sessions: &Path,
    id: &str,
    pipe: &mut std::os::unix::net::UnixStream,
) -> Result<(), Error> {
    storage::send(
        pipe,
        &Request {
            sessions: sessions.into(),
            directory: directory.into(),
            id: id.into(),
        },
    )
}

impl Harness for Recording {
    type P = Recording;
    const NAME: &'static str = "recording";
    fn account() -> u64 {
        ACCOUNT
    }
    fn other_account() -> u64 {
        ACCOUNT + 1
    }
    fn handoff(
        directory: &Path,
        sessions: &Path,
        id: &str,
        pipe: &mut std::os::unix::net::UnixStream,
    ) -> Result<(), Error> {
        handoff(directory, sessions, id, pipe)
    }
    fn serve(
        input: std::io::StdinLock<'static>,
        output: std::io::StderrLock<'static>,
    ) -> Result<(), String> {
        serve::<Recorder<false>>(input, output, inspect).map_err(|error| format!("{error:?}"))
    }
    fn previous_build_record(id: &str, live: &Identity, socket: &Path) -> serde_json::Value {
        serde_json::json!({
            "id": id, "account_id": ACCOUNT, "phase": "Running", "revision": 3,
            "supervisor": live, "game": live, "socket": socket,
        })
    }
}

impl Harness for Gated {
    type P = Recording;
    const NAME: &'static str = "gated";
    fn account() -> u64 {
        ACCOUNT
    }
    fn other_account() -> u64 {
        ACCOUNT + 1
    }
    fn handoff(
        directory: &Path,
        sessions: &Path,
        id: &str,
        pipe: &mut std::os::unix::net::UnixStream,
    ) -> Result<(), Error> {
        handoff(directory, sessions, id, pipe)
    }
    fn serve(
        input: std::io::StdinLock<'static>,
        output: std::io::StderrLock<'static>,
    ) -> Result<(), String> {
        serve::<Recorder<true>>(input, output, inspect).map_err(|error| format!("{error:?}"))
    }
    fn previous_build_record(id: &str, live: &Identity, socket: &Path) -> serde_json::Value {
        serde_json::json!({
            "id": id, "account_id": ACCOUNT, "phase": "Running", "revision": 3,
            "supervisor": live, "game": live, "socket": socket,
        })
    }
}

struct Recorder<const BEGIN: bool> {
    directory: PathBuf,
    script: Script,
}

impl<const BEGIN: bool> Launch for Recorder<BEGIN> {
    type P = Recording;
    type Host = ();
    const BEGIN: bool = BEGIN;

    fn open(input: &mut dyn Read) -> Result<(Opening<Recording>, Self), Failure> {
        let request: Request = storage::packet(input)?;
        let (directory, script) = fixture_directory();
        write_helper(&directory.join("helper"), &directory, &script);
        Ok((
            Opening {
                root: request.sessions,
                id: request.id,
                account_id: ACCOUNT,
                store: Game {
                    name: "fixture".into(),
                },
                helpers: directory.clone(),
            },
            Self { directory, script },
        ))
    }

    async fn authenticate(&mut self, _: &Arc<Session<Recording>>) -> Result<(), Failure> {
        record(&self.directory, "authenticate");
        Ok(())
    }

    async fn prepare_cloud(
        &mut self,
        session: &Arc<Session<Recording>>,
        _: &SessionAlive<'_>,
    ) -> Result<(), Failure> {
        record(&self.directory, "cloud.prepare");
        if self.script.hold_cloud {
            fs::write(self.directory.join("cloud-ready"), b"").unwrap();
            hold(&self.directory, "release-cloud", || session.check()).await?;
        }
        if self.script.ask {
            let cloud = session.cloud();
            let answer = cloud
                .ask(crate::cloud::Question::Conflict, None)
                .await
                .map_err(|_| Failure::Cancelled)?;
            record(&self.directory, &format!("answered:{answer:?}"));
            cloud.show(crate::cloud::Phase::Ready, None);
        }
        Ok(())
    }

    async fn target(
        &mut self,
        _: &Arc<Session<Recording>>,
    ) -> Result<(Target, Option<Architecture>), Failure> {
        record(&self.directory, "target");
        Ok((
            target(&self.directory, &self.script),
            self.script.intel.then_some(Architecture::Intel),
        ))
    }

    async fn prepare_host(
        &mut self,
        _: &Arc<Session<Recording>>,
        target: &Target,
        _: Option<Architecture>,
    ) -> Result<Prepared<()>, Failure> {
        record(&self.directory, "host.prepare");
        Ok(Prepared {
            host: (),
            helper: self.directory.join("helper"),
            subcommand: "game-exec",
            environment: target.environment.clone(),
        })
    }

    fn admitted(&mut self) -> Result<(), Failure> {
        record(&self.directory, "cloud.started");
        Ok(())
    }

    fn running(&self, _: &Arc<()>, _: u32) -> Box<dyn Fn(bool) + Send + Sync> {
        let directory = self.directory.clone();
        Box::new(move |active| record(&directory, &format!("presence:{active}")))
    }

    fn stopped(&self, _: &(), _: Option<i32>) {}

    async fn finish(&mut self, _: &Arc<Session<Recording>>, confirmed: bool) {
        if self.script.hold_finish {
            fs::write(self.directory.join("finish-ready"), b"").unwrap();
            let _ = hold(&self.directory, "release-finish", || Ok::<_, Error>(())).await;
        }
        record(&self.directory, &format!("cloud.finished:{confirmed}"));
    }

    fn abort(&mut self, _: &Arc<Session<Recording>>) {
        record(&self.directory, "cloud.aborted");
    }

    fn rosetta(helper: &Path) -> Result<bool, Error> {
        Ok(helper.exists())
    }
}

mod lifecycle_of_the_core {
    super::super::lifecycle_suite!(super::Recording);
}

#[test]
fn begin_opens_the_start_gate_once_and_only_for_its_account() {
    super::begin_opens_the_start_gate_once_and_only_for_its_account::<Gated>();
}
