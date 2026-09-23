use super::*;
use crate::{Error, protocol::Credentials};
use std::{
    fs,
    os::fd::OwnedFd,
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        net::UnixStream,
    },
    path::Path,
    process::{Command as Process, Stdio},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

type Probe = fn(&Path, u32) -> Result<Option<Identity>, Error>;

/// Dropping a UI client only detaches. It never owns or stops a game/host.
pub struct Client {
    root: PathBuf,
    helper: PathBuf,
    id: String,
    probe: Probe,
    last: Mutex<Option<Snapshot>>,
}

/// Excludes launch admission while Cloud maintenance runs in Forge, including
/// stores left by a dead supervisor. Construction checks the kernel
/// identities, including birth time. Both storefronts' guards share this type.
pub struct IdleGuard {
    _starting: fs::File,
    _owner: fs::File,
}
impl IdleGuard {
    /// `starting` and `owner` are a provider's held `start.lock` and
    /// `owner.lock`, after it has found no live supervisor or game.
    pub(crate) fn hold(starting: fs::File, owner: fs::File) -> Self {
        Self {
            _starting: starting,
            _owner: owner,
        }
    }
}
impl Client {
    pub fn idle_guard(root: &Path, helper: &Path) -> Result<IdleGuard, Error> {
        Self::idle_guard_with(root, helper, inspect)
    }

    pub(super) fn idle_guard_with(
        root: &Path,
        helper: &Path,
        probe: Probe,
    ) -> Result<IdleGuard, Error> {
        let starting = storage::lock(root, "start.lock")?;
        let owner = storage::lock(root, "owner.lock")?;
        if let Some(saved) = storage::read(root)?
            && !saved.finished()
            && (probe(helper, saved.supervisor.pid)?.as_ref() == Some(&saved.supervisor)
                || owns_live_child(&saved, helper, probe)?)
        {
            return Err(Error::SessionBusy);
        }
        Ok(IdleGuard::hold(starting, owner))
    }
    pub fn recover(root: &Path, helper: &Path) -> Result<Option<Self>, Error> {
        paced(Self::recover_with(root, helper, inspect))
    }

    pub(super) fn recover_with(
        root: &Path,
        helper: &Path,
        probe: Probe,
    ) -> Result<Option<Self>, Error> {
        let _starting = storage::lock(root, "start.lock")?;
        let Some(snapshot) = storage::read(root)? else {
            return Ok(None);
        };
        let client = Self {
            root: root.into(),
            helper: helper.into(),
            id: snapshot.id,
            probe,
            last: Mutex::new(None),
        };
        let first = client.observe(None)?;
        if first.finished() {
            Ok(None)
        } else {
            *client.last.lock().unwrap() = Some(first);
            Ok(Some(client))
        }
    }

    pub fn snapshot(&self, after: Option<u64>) -> Result<Snapshot, Error> {
        let result = paced(self.observe(after));
        if let Ok(snapshot) = &result {
            let mut last = self.last.lock().unwrap();
            if last
                .as_ref()
                .is_none_or(|old| old.revision <= snapshot.revision)
            {
                *last = Some(snapshot.clone());
            }
        }
        result
    }

    fn observe(&self, after: Option<u64>) -> Result<Snapshot, Error> {
        let disk = storage::read_id(&self.root, &self.id);
        let cached = self.last.lock().unwrap().clone();
        let saved = match (disk, cached) {
            (Ok(Some(saved)), Some(last)) if last.revision > saved.revision => last,
            (Ok(Some(saved)), _) => saved,
            (_, Some(last)) => last,
            (Err(error), None) => return Err(error),
            (Ok(None), None) => return Err(Error::Unavailable),
        };
        if saved.id != self.id {
            return Err(Error::InvalidSession);
        }
        if saved.finished() {
            return Ok(saved);
        }
        // A failed inspection or RPC is uncertainty, not proof of host death.
        // Keep the durable lifecycle and all guards while reconnecting.
        match self.observe_saved(saved.clone(), after) {
            Ok(snapshot) => Ok(snapshot),
            Err(_) => {
                std::thread::sleep(Duration::from_millis(500));
                Ok(Snapshot {
                    reconnecting: true,
                    ..saved
                })
            }
        }
    }

    fn observe_saved(&self, saved: Snapshot, after: Option<u64>) -> Result<Snapshot, Error> {
        // Authenticate the recorded process by UID, executable and kernel birth
        // timestamp, not PID alone. Socket permissions and the session UUID bind
        // the connection to this supervisor instance.
        if (self.probe)(&self.helper, saved.supervisor.pid)?.as_ref() == Some(&saved.supervisor) {
            let response = exchange(
                &saved,
                &Command::Observe {
                    id: self.id.clone(),
                    after,
                },
            );
            if response.is_err()
                && let Some(final_state) = storage::read_id(&self.root, &self.id)?
                && final_state.finished()
            {
                return Ok(final_state);
            }
            return response;
        }
        let _owner = storage::lock(&self.root, "owner.lock")?;
        let mut orphan = saved;
        orphan.reconnecting = false;
        orphan.host = HostState::Lost;
        orphan.revision += 1;
        if owns_live_child(&orphan, &self.helper, self.probe)? {
            // Exceptional supervisor loss preserves even a helper that has not
            // yet replaced itself. Never restart or cancel its accepted launch.
            if after.is_some() {
                std::thread::sleep(Duration::from_millis(500));
            }
        } else {
            orphan.phase = Phase::Finished;
            orphan.exit_code = None;
            orphan.starting = None;
        }
        Ok(orphan)
    }

    pub fn cancel_preparation(&self) {
        if let Ok(Some(saved)) = storage::read_id(&self.root, &self.id)
            && saved.id == self.id
            && !saved.finished()
        {
            let _ = exchange(
                &saved,
                &Command::Cancel {
                    id: self.id.clone(),
                },
            );
        }
    }

    pub fn cloud_choice(
        &self,
        account_id: u64,
        decision_id: u64,
        choice: crate::cloud::Choice,
    ) -> Result<(), Error> {
        self.cloud_command(
            account_id,
            Command::CloudChoice {
                id: self.id.clone(),
                account_id,
                decision_id,
                choice,
            },
        )
    }

    pub fn revoke_cloud(&self, account_id: u64) -> Result<(), Error> {
        self.cloud_command(
            account_id,
            Command::Revoke {
                id: self.id.clone(),
                account_id,
            },
        )
    }

    pub fn resume_after_rosetta(&self, account_id: u64, decision_id: u64) -> Result<(), Error> {
        let response = self.account_command(
            account_id,
            Command::ResumeAfterRosetta {
                id: self.id.clone(),
                account_id,
                decision_id,
            },
        )?;
        if response.rosetta.is_some() {
            return Err(Error::RosettaUnavailable);
        }
        if response.finished() && response.phase != Phase::Finished {
            return Err(Error::InvalidSession);
        }
        Ok(())
    }

    fn cloud_command(&self, account_id: u64, command: Command) -> Result<(), Error> {
        self.account_command(account_id, command).map(|_| ())
    }

    fn account_command(&self, account_id: u64, command: Command) -> Result<Snapshot, Error> {
        let saved = storage::read_id(&self.root, &self.id)?.ok_or(Error::Unavailable)?;
        if account_id == 0
            || saved.id != self.id
            || saved.account_id != account_id
            || (self.probe)(&self.helper, saved.supervisor.pid)?.as_ref() != Some(&saved.supervisor)
        {
            return Err(Error::InvalidSession);
        }
        let response = exchange(&saved, &command)?;
        let mut last = self.last.lock().unwrap();
        if last
            .as_ref()
            .is_none_or(|old| old.revision <= response.revision)
        {
            *last = Some(response.clone());
        }
        Ok(response)
    }
}

fn owns_live_child(snapshot: &Snapshot, helper: &Path, probe: Probe) -> Result<bool, Error> {
    if let Some(game) = &snapshot.game
        && probe(helper, game.pid)?.as_ref() == Some(game)
    {
        return Ok(true);
    }
    match &snapshot.starting {
        Some(StartingGame::Process {
            identity,
            executable,
        }) => {
            let Some(actual) = probe(helper, identity.pid)? else {
                return Ok(false);
            };
            // SETEXEC keeps PID, UID and kernel birth time, but changes the
            // executable. Permit only the recorded wrapper or intended game.
            Ok(actual == *identity
                || actual.pid == identity.pid
                    && actual.uid == identity.uid
                    && actual.birth_seconds == identity.birth_seconds
                    && actual.birth_microseconds == identity.birth_microseconds
                    && Path::new(&actual.executable) == executable)
        }
        None if snapshot.game.is_none() && snapshot.phase == Phase::Running => {
            Err(Error::SessionBusy)
        }
        None => Ok(false),
    }
}

pub fn start(
    helper: &Path,
    credentials: &Credentials,
    request: Request,
    cancelled: &AtomicBool,
) -> Result<Client, Error> {
    let mut command = Process::new(helper);
    command
        .arg("supervise")
        .env_clear()
        .env("PATH", "/usr/bin:/bin");
    start_command(helper, credentials, request, cancelled, command, inspect)
}

pub(super) fn start_command(
    helper: &Path,
    credentials: &Credentials,
    request: Request,
    cancelled: &AtomicBool,
    mut command: Process,
    probe: Probe,
) -> Result<Client, Error> {
    let _starting = storage::lock(&request.root, "start.lock")?;
    if let Some(saved) = storage::read(&request.root)? {
        let old = Client {
            root: request.root.clone(),
            helper: helper.into(),
            id: saved.id,
            probe,
            last: Mutex::new(None),
        };
        if !old.snapshot(None)?.finished() {
            return Err(Error::SessionBusy);
        }
    }
    // Even an absent record must not permit racing an owner that is publishing
    // its first state. The supervisor takes this lock for its entire lifetime.
    let _starting = storage::owner_released(&request.root, _starting)?;
    if cancelled.load(Ordering::Acquire) {
        return Err(Error::Cancelled);
    }
    // A private duplex pipe carries the request on stdin and one acknowledgement
    // on stderr. Neither credentials nor acknowledgements enter log files.
    let (mut pipe, child_pipe) = UnixStream::pair()?;
    pipe.set_read_timeout(Some(Duration::from_secs(15)))?;
    pipe.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut child = command
        .stdin(Stdio::from(OwnedFd::from(child_pipe.try_clone()?)))
        .stdout(Stdio::null())
        .stderr(Stdio::from(OwnedFd::from(child_pipe)))
        .spawn()
        .map_err(|_| Error::StartFailed)?;
    drop(command);
    let handoff = (|| {
        credentials.write(&mut pipe)?;
        storage::send(&mut pipe, &request)
    })();
    let client = Client {
        root: request.root,
        helper: helper.into(),
        id: request.id,
        probe,
        last: Mutex::new(None),
    };
    let result = handoff.and_then(|_| {
        let acknowledged = storage::packet::<Snapshot>(&pipe).and_then(|snapshot| {
            if snapshot.id != client.id
                || snapshot.account_id != credentials.steam_id
                || snapshot.app_id != request.config.app_id
                || snapshot.supervisor.pid != child.id()
                || (client.probe)(&client.helper, child.id())?.as_ref()
                    != Some(&snapshot.supervisor)
            {
                return Err(Error::InvalidSession);
            }
            Ok(())
        });
        // A committed launch survives a lost reply (including a UI crash). Its
        // durable record and authenticated control socket remain recoverable.
        if let Err(error) = acknowledged {
            match storage::read_id(&client.root, &client.id)? {
                Some(_) => {
                    client.snapshot(None)?;
                }
                None => return Err(error),
            }
        }
        if cancelled.load(Ordering::Acquire) {
            client.cancel_preparation();
        }
        Ok(client)
    });
    // Reap while this UI exists. Parent exit naturally reparents the supervisor;
    // none of its stdio is attached to Forge after the one-time handoff.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    result
}

fn exchange(saved: &Snapshot, command: &Command) -> Result<Snapshot, Error> {
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
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    storage::send(&mut stream, command)?;
    let response: Snapshot = storage::packet(stream)?;
    if response.id != saved.id
        || response.app_id != saved.app_id
        || response.account_id != saved.account_id
        || response.supervisor != saved.supervisor
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

// All native failure retries are paced here, including failures before a valid
// snapshot exists. Swift owns presentation, not transport retry policy.
fn paced<T>(result: Result<T, Error>) -> Result<T, Error> {
    if result.is_err() {
        std::thread::sleep(Duration::from_millis(500));
    }
    result
}

#[cfg(test)]
mod cloud_commands {
    use super::*;

    fn identity(birth_seconds: u64) -> Identity {
        Identity {
            pid: 42,
            uid: 501,
            birth_seconds,
            birth_microseconds: 0,
            executable: "/fixture/supervisor".into(),
        }
    }

    fn fixture(probe: Probe) -> (tempfile::TempDir, Client) {
        let root = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let snapshot = Snapshot {
            id: id.clone(),
            app_id: 42,
            account_id: 123,
            supervisor: identity(1),
            game: None,
            starting: None,
            socket: root.path().join("absent.sock"),
            phase: Phase::Preparing,
            cloud: crate::cloud::Status {
                phase: crate::cloud::Phase::Conflict,
                id: 7,
                error: None,
                allowed_choices: vec![
                    crate::cloud::Choice::Local,
                    crate::cloud::Choice::Cloud,
                    crate::cloud::Choice::Cancel,
                ],
            },
            host: HostState::Stopped,
            exit_code: None,
            error: None,
            revision: 1,
            reconnecting: false,
            rosetta: None,
        };
        storage::write(root.path(), &snapshot).unwrap();
        let client = Client {
            root: root.path().into(),
            helper: "/fixture/supervisor".into(),
            id,
            probe,
            last: Mutex::new(None),
        };
        (root, client)
    }

    fn must_not_probe(_: &Path, _: u32) -> Result<Option<Identity>, Error> {
        panic!("a mismatched account or session must fail before process inspection");
    }

    #[test]
    fn rejects_another_account_before_sending_choice_or_revocation() {
        let (_root, client) = fixture(must_not_probe);
        assert!(matches!(
            client.cloud_choice(456, 7, crate::cloud::Choice::Local),
            Err(Error::InvalidSession)
        ));
        assert!(matches!(
            client.revoke_cloud(456),
            Err(Error::InvalidSession)
        ));
    }

    #[test]
    fn rejects_a_replaced_session_record_before_process_inspection() {
        let (root, client) = fixture(must_not_probe);
        let mut replacement = storage::read(root.path()).unwrap().unwrap();
        replacement.id = uuid::Uuid::new_v4().to_string();
        storage::write(root.path(), &replacement).unwrap();
        fs::copy(
            root.path().join(format!("{}.json", replacement.id)),
            root.path().join(format!("{}.json", client.id)),
        )
        .unwrap();
        assert!(matches!(
            client.revoke_cloud(123),
            Err(Error::InvalidSession)
        ));
    }

    #[test]
    fn rejects_reused_pid_before_sending_choice_or_revocation() {
        fn replaced(_: &Path, _: u32) -> Result<Option<Identity>, Error> {
            Ok(Some(identity(2)))
        }
        let (_root, client) = fixture(replaced);
        assert!(matches!(
            client.cloud_choice(123, 7, crate::cloud::Choice::Cloud),
            Err(Error::InvalidSession)
        ));
        assert!(matches!(
            client.revoke_cloud(123),
            Err(Error::InvalidSession)
        ));
    }
}
