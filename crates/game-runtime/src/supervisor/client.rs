//! The UI's side of a Game session: starting or recovering its supervisor,
//! observing it and sending it commands. Dropping a client only detaches; it
//! never owns or stops a game.
use super::*;
use std::{
    fs,
    marker::PhantomData,
    os::fd::OwnedFd,
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        net::UnixStream,
    },
    path::Path,
    process::{Command as Process, Stdio},
    sync::Mutex,
    time::{Duration, Instant},
};

pub(crate) type Probe = fn(&Path, u32) -> Result<Option<Identity>, Error>;

/// Every native failure and uncertain observation is paced here, including
/// failures before a valid record exists. The UI owns presentation, not
/// transport retry policy.
const PACE: Duration = Duration::from_millis(500);

pub struct Client<P: Provider> {
    root: PathBuf,
    helper: PathBuf,
    id: String,
    probe: Probe,
    last: Mutex<Option<Record<P>>>,
}

/// Answers whether the Game session with this ID still owns a process.
pub(crate) type SessionAlive<'a> = dyn Fn(&str) -> Result<bool, Error> + Sync + 'a;

/// No other Game session holds the saves.
#[cfg(test)]
pub(crate) fn no_other_session(_: &str) -> Result<bool, Error> {
    Ok(false)
}

/// A storefront's `owner.lock`, held: no other supervisor of that storefront
/// is alive.
pub struct OwnerLock<P> {
    _file: fs::File,
    root: PathBuf,
    provider: PhantomData<P>,
}
impl<P: Provider> OwnerLock<P> {
    pub(crate) fn take(root: &Path) -> Result<Self, Error> {
        Ok(Self {
            _file: storage::lock(root, "owner.lock")?,
            root: root.into(),
            provider: PhantomData,
        })
    }

    /// With no other supervisor alive, a session without a record is dead: it
    /// was never accepted, or a previous build launched it.
    pub(crate) fn session_alive(
        &self,
        id: &str,
        inspect: impl Fn(u32) -> Result<Option<Identity>, Error>,
    ) -> Result<bool, Error> {
        match journal::by_id::<Record<P>>(&self.root, id)? {
            Some(record) if !record.finished() => {
                Ok(record.liveness(inspect)? != ownership::Liveness::Dead)
            }
            _ => Ok(false),
        }
    }
}

/// Excludes a storefront's launch admission while Cloud maintenance runs in
/// SailOver, including stores left by a dead supervisor. Construction checks the
/// kernel identities of the recorded supervisor and game.
pub struct IdleGuard<P> {
    _starting: fs::File,
    owner: OwnerLock<P>,
    helper: PathBuf,
    probe: Probe,
}
impl<P: Provider> IdleGuard<P> {
    pub(crate) fn session_alive(&self, id: &str) -> Result<bool, Error> {
        self.owner
            .session_alive(id, |pid| (self.probe)(&self.helper, pid))
    }
}

impl<P: Provider> Client<P> {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn idle_guard(root: &Path, helper: &Path) -> Result<IdleGuard<P>, P::Error> {
        Ok(Self::idle_guard_with(root, helper, inspect)?)
    }

    pub(crate) fn idle_guard_with(
        root: &Path,
        helper: &Path,
        probe: Probe,
    ) -> Result<IdleGuard<P>, Error> {
        let starting = storage::lock(root, "start.lock")?;
        let owner = OwnerLock::take(root)?;
        if let Some(saved) = journal::current::<Record<P>>(root)?
            && !saved.finished()
            && (ownership::alive(&saved.supervisor, |pid| probe(helper, pid))?
                || saved.liveness(|pid| probe(helper, pid))? != ownership::Liveness::Dead)
        {
            return Err(Error::Busy);
        }
        Ok(IdleGuard {
            _starting: starting,
            owner,
            helper: helper.into(),
            probe,
        })
    }

    pub fn recover(root: &Path, helper: &Path) -> Result<Option<Self>, P::Error> {
        Ok(paced(Self::recover_with(root, helper, inspect))?)
    }

    pub(crate) fn recover_with(
        root: &Path,
        helper: &Path,
        probe: Probe,
    ) -> Result<Option<Self>, Error> {
        let _starting = storage::lock(root, "start.lock")?;
        let Some(record) = journal::current::<Record<P>>(root)? else {
            return Ok(None);
        };
        let client = Self::new(root, helper, record.id, probe);
        let first = client.observe(None)?;
        if first.finished() {
            Ok(None)
        } else {
            *client.last.lock().unwrap() = Some(first);
            Ok(Some(client))
        }
    }

    fn new(root: &Path, helper: &Path, id: String, probe: Probe) -> Self {
        Self {
            root: root.into(),
            helper: helper.into(),
            id,
            probe,
            last: Mutex::new(None),
        }
    }

    pub fn snapshot(&self, after: Option<u64>) -> Result<Record<P>, P::Error> {
        let result = paced(self.observe(after));
        if let Ok(record) = &result {
            self.remember(record);
        }
        Ok(result?)
    }

    fn remember(&self, record: &Record<P>) {
        let mut last = self.last.lock().unwrap();
        if last
            .as_ref()
            .is_none_or(|old| old.revision <= record.revision)
        {
            *last = Some(record.clone());
        }
    }

    fn observe(&self, after: Option<u64>) -> Result<Record<P>, Error> {
        let disk = journal::by_id::<Record<P>>(&self.root, &self.id);
        let cached = self.last.lock().unwrap().clone();
        let saved = match (disk, cached) {
            (Ok(Some(saved)), Some(last)) if last.revision > saved.revision => last,
            (Ok(Some(saved)), _) => saved,
            (_, Some(last)) => last,
            (Err(error), None) => return Err(error),
            (Ok(None), None) => return Err(Error::Unavailable),
        };
        if saved.finished() {
            return Ok(saved);
        }
        // A failed inspection or RPC is uncertainty, not proof of the
        // supervisor's death. Keep the record and every guard while reconnecting.
        match self.observe_saved(saved.clone(), after) {
            Ok(record) => Ok(record),
            Err(_) => {
                std::thread::sleep(PACE);
                Ok(Record {
                    reconnecting: true,
                    ..saved
                })
            }
        }
    }

    fn observe_saved(&self, saved: Record<P>, after: Option<u64>) -> Result<Record<P>, Error> {
        // Socket permissions and the session UUID bind the connection to this
        // supervisor instance once its kernel identity matches.
        if ownership::alive(&saved.supervisor, |pid| (self.probe)(&self.helper, pid))? {
            let response = exchange(
                &saved,
                &Command::<P>::Observe {
                    id: self.id.clone(),
                    after,
                },
            );
            if response.is_err()
                && let Some(last) = journal::by_id::<Record<P>>(&self.root, &self.id)?
                && last.finished()
            {
                return Ok(last);
            }
            return response;
        }
        // Only the supervisor writes the journal. A lost supervisor's record
        // is retired in memory, and only while no other supervisor can start.
        let _owner = storage::lock(&self.root, "owner.lock")?;
        let mut orphan = saved;
        orphan.reconnecting = false;
        orphan.revision += 1;
        match orphan.liveness(|pid| (self.probe)(&self.helper, pid))? {
            ownership::Liveness::Dead => {
                orphan.phase = Phase::Finished;
                orphan.exit_code = None;
                orphan.game = None;
                orphan.starting = None;
                orphan.rosetta = None;
                P::orphaned(&mut orphan, false);
            }
            ownership::Liveness::Owned => {
                // Supervisor loss preserves even a helper that has not replaced
                // itself yet. Never restart or cancel its accepted launch.
                P::orphaned(&mut orphan, true);
                if after.is_some() {
                    std::thread::sleep(PACE);
                }
            }
            ownership::Liveness::Unknown => return Err(Error::Busy),
        }
        Ok(orphan)
    }

    /// Best effort: a supervisor ignores Cancel once the game was spawned.
    pub fn cancel(&self) {
        if let Ok(Some(saved)) = journal::by_id::<Record<P>>(&self.root, &self.id)
            && !saved.finished()
            && ownership::alive(&saved.supervisor, |pid| (self.probe)(&self.helper, pid))
                .unwrap_or(false)
        {
            let _ = exchange(
                &saved,
                &Command::<P>::Cancel {
                    id: self.id.clone(),
                },
            );
        }
    }

    /// Ends the game, which gets a few seconds to quit before it is killed;
    /// its exit Cloud sync still runs. Before spawn it cancels preparation.
    pub fn stop(&self) -> Result<(), P::Error> {
        self.command(
            None,
            Command::<P>::Stop {
                id: self.id.clone(),
            },
            false,
        )?;
        Ok(())
    }

    /// Opens a supervisor's start gate. Begin is idempotent, so a lost request
    /// is retried while the supervisor lives, instead of silently expiring.
    pub fn begin(&self) -> Result<(), P::Error> {
        // As long as the supervisor itself waits for it.
        let deadline = Instant::now() + super::session::BEGIN_DEADLINE;
        loop {
            let saved =
                journal::by_id::<Record<P>>(&self.root, &self.id)?.ok_or(Error::Unavailable)?;
            let command = Command::<P>::Begin {
                id: self.id.clone(),
                account_id: saved.account_id.clone(),
            };
            match self.command(None, command, false) {
                Ok(_) => return Ok(()),
                Err(Error::InvalidSession) => return Err(Error::InvalidSession.into()),
                Err(error) if Instant::now() >= deadline => return Err(error.into()),
                Err(_) => std::thread::sleep(PACE),
            }
        }
    }

    pub fn choose(
        &self,
        account_id: &P::Account,
        decision_id: u64,
        choice: crate::cloud::Choice,
    ) -> Result<(), P::Error> {
        let command = Command::<P>::CloudChoice {
            id: self.id.clone(),
            account_id: account_id.clone(),
            decision_id,
            choice,
        };
        self.command(Some(account_id), command, true)?;
        Ok(())
    }

    pub fn revoke(&self, account_id: &P::Account) -> Result<(), P::Error> {
        let command = Command::<P>::Revoke {
            id: self.id.clone(),
            account_id: account_id.clone(),
        };
        self.command(Some(account_id), command, false)?;
        Ok(())
    }

    pub fn resume_after_rosetta(
        &self,
        account_id: &P::Account,
        decision_id: u64,
    ) -> Result<(), P::Error> {
        let command = Command::<P>::ResumeAfterRosetta {
            id: self.id.clone(),
            account_id: account_id.clone(),
            decision_id,
        };
        let response = self.command(Some(account_id), command, true)?;
        if response.rosetta.is_some() {
            return Err(Error::RosettaUnavailable.into());
        }
        if response.finished() && response.phase != Phase::Finished {
            return Err(Error::InvalidSession.into());
        }
        Ok(())
    }

    /// An answer to a terminal session is refused, so a stale answer never
    /// reads as accepted. Other commands to it have nothing left to do.
    pub(super) fn command(
        &self,
        account_id: Option<&P::Account>,
        command: Command<P>,
        answer: bool,
    ) -> Result<Record<P>, Error> {
        let saved = journal::by_id::<Record<P>>(&self.root, &self.id)?.ok_or(Error::Unavailable)?;
        if account_id.is_some_and(|account_id| *account_id != saved.account_id) {
            return Err(Error::InvalidSession);
        }
        if saved.finished() {
            return if answer {
                Err(Error::InvalidSession)
            } else {
                Ok(saved)
            };
        }
        if !ownership::alive(&saved.supervisor, |pid| (self.probe)(&self.helper, pid))? {
            return Err(Error::InvalidSession);
        }
        let response = exchange(&saved, &command)?;
        self.remember(&response);
        Ok(response)
    }

    /// The default supervisor command: the storefront's helper subcommand
    /// with an empty environment.
    pub(crate) fn supervisor_command(helper: &Path) -> Process {
        let mut command = Process::new(helper);
        command
            .arg(P::SUPERVISE)
            .env_clear()
            .env("PATH", "/usr/bin:/bin");
        command
    }

    /// Spawns a supervisor and hands it the launch. `handoff` writes the
    /// storefront's request, including any credentials, to the private pipe;
    /// `acknowledges` checks that the reply describes that launch.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start_with(
        root: &Path,
        helper: &Path,
        id: String,
        mut command: Process,
        probe: Probe,
        handoff: impl FnOnce(&mut UnixStream) -> Result<(), Error>,
        acknowledges: impl Fn(&Record<P>) -> bool,
        cancelled: impl Fn() -> bool,
    ) -> Result<Self, Error> {
        let starting = storage::lock(root, "start.lock")?;
        if let Some(saved) = journal::current::<Record<P>>(root)? {
            let old = Self::new(root, helper, saved.id, probe);
            if !paced(old.observe(None))?.finished() {
                return Err(Error::Busy);
            }
        }
        // Even an absent record must not permit racing an owner that is
        // publishing its first state. A supervisor holds owner.lock for life.
        let _starting = storage::owner_released(root, starting)?;
        if cancelled() {
            return Err(Error::Cancelled);
        }
        // A private duplex pipe carries the request on stdin and one
        // acknowledgement on stderr. Neither reaches a log file.
        let (mut pipe, child_pipe) = UnixStream::pair()?;
        pipe.set_read_timeout(Some(Duration::from_secs(15)))?;
        pipe.set_write_timeout(Some(Duration::from_secs(3)))?;
        let mut child = command
            .stdin(Stdio::from(OwnedFd::from(child_pipe.try_clone()?)))
            .stdout(Stdio::null())
            .stderr(Stdio::from(OwnedFd::from(child_pipe)))
            .spawn()
            .map_err(|_| Error::StartFailed)?;
        drop(command);
        let client = Self::new(root, helper, id, probe);
        let result = handoff(&mut pipe).and_then(|()| {
            let acknowledged = storage::packet::<Record<P>>(&pipe).and_then(|ack| {
                if ack.id != client.id
                    || ack.supervisor.pid != child.id()
                    || !acknowledges(&ack)
                    || !ownership::alive(&ack.supervisor, |pid| probe(helper, pid))?
                {
                    return Err(Error::InvalidSession);
                }
                Ok(())
            });
            // An accepted launch survives a lost reply, including a UI crash:
            // its durable record and authenticated socket stay recoverable.
            if let Err(error) = acknowledged {
                match journal::by_id::<Record<P>>(root, &client.id)? {
                    Some(_) => drop(paced(client.observe(None))?),
                    None => return Err(error),
                }
            }
            if cancelled() {
                client.cancel();
            }
            Ok(())
        });
        // Reap while this UI exists. Parent exit reparents the supervisor;
        // none of its stdio stays attached to SailOver after the handoff.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        result.map(|()| client)
    }
}

fn exchange<P: Provider>(saved: &Record<P>, command: &Command<P>) -> Result<Record<P>, Error> {
    let parent = saved.socket.parent().ok_or(Error::InvalidSession)?;
    let metadata = fs::symlink_metadata(parent)?;
    if !metadata.file_type().is_dir()
        || metadata.uid() != saved.supervisor.uid
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(Error::InvalidSession);
    }
    let mut stream = UnixStream::connect(&saved.socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(20)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    storage::send_command(&mut stream, command)?;
    let response: Record<P> = storage::packet(stream)?;
    if response.id != saved.id
        || response.account_id != saved.account_id
        || !P::same_game(&response.store, &saved.store)
        || !ownership::same_process(&saved.supervisor, &response.supervisor)
    {
        return Err(Error::InvalidSession);
    }
    Ok(response)
}

pub(crate) fn inspect(helper: &Path, pid: u32) -> Result<Option<Identity>, Error> {
    let output = Process::new(helper)
        .arg("process-identity")
        .arg(pid.to_string())
        .env_clear()
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()?;
    if !output.status.success() || output.stdout.len() > 8192 {
        return Err(Error::Unavailable);
    }
    serde_json::from_slice(&output.stdout).map_err(|_| Error::InvalidSession)
}

fn paced<T>(result: Result<T, Error>) -> Result<T, Error> {
    if result.is_err() {
        std::thread::sleep(PACE);
    }
    result
}

#[cfg(test)]
mod tests;
