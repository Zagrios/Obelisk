use super::*;
use crate::supervisor::storage;
use std::{
    fs,
    os::{
        fd::OwnedFd,
        unix::{
            fs::{MetadataExt, PermissionsExt},
            net::UnixStream,
        },
    },
    path::Path,
    process::{Command as Process, Stdio},
};

/// An observer never owns the game's lifetime. Dropping it only detaches.
pub struct Client {
    root: PathBuf,
    helper: PathBuf,
    id: String,
}
impl Client {
    pub fn begin(&self) -> Result<()> {
        let saved = read_snapshot(&self.root, Some(&self.id))?.ok_or(Error::Session)?;
        self.command(
            Some(&saved.account_id),
            Command::Begin {
                id: self.id.clone(),
                account: saved.account_id.clone(),
            },
        )
    }
    pub fn recover(root: &Path, helper: &Path) -> Result<Option<Self>> {
        let _starting = storage::lock(root, "start.lock")?;
        let Some(saved) = read_snapshot(root, None)? else {
            return Ok(None);
        };
        if saved.finished() {
            return Ok(None);
        }
        let client = Self {
            root: root.into(),
            helper: helper.into(),
            id: saved.id,
        };
        Ok((!client.snapshot(None)?.finished()).then_some(client))
    }
    pub fn snapshot(&self, after: Option<u64>) -> Result<Snapshot> {
        let mut saved = read_snapshot(&self.root, Some(&self.id))?.ok_or(Error::Session)?;
        if saved.finished() {
            return Ok(saved);
        }
        match self.live_snapshot(&mut saved, after) {
            Ok(value) => {
                if value.reconnecting {
                    std::thread::sleep(Duration::from_millis(250));
                }
                Ok(value)
            }
            Err(_) => {
                // Lost IPC or process-inspection permission is not actual exit.
                std::thread::sleep(Duration::from_millis(250));
                saved.reconnecting = true;
                Ok(saved)
            }
        }
    }
    /// Reads through to the supervisor, or retires a snapshot whose owner is gone.
    fn live_snapshot(&self, saved: &mut Snapshot, after: Option<u64>) -> Result<Snapshot> {
        if inspect(&self.helper, saved.supervisor.pid)?.as_ref() == Some(&saved.supervisor) {
            return exchange(
                saved,
                &Command::Observe {
                    id: self.id.clone(),
                    after,
                },
            );
        }
        let _owner = storage::lock(&self.root, "owner.lock")?;
        if owns_child(saved, |pid| inspect(&self.helper, pid))? {
            saved.reconnecting = true;
        } else {
            saved.phase = Phase::Finished;
            saved.game = None;
            saved.starting = None;
            if !matches!(
                saved.cloud_phase,
                CloudPhase::Unsupported | CloudPhase::Disabled | CloudPhase::Synced
            ) {
                saved.cloud_phase = CloudPhase::Pending;
            }
            saved.allowed_choices.clear();
            saved.rosetta = None;
            saved.revision += 1;
            write_snapshot(&self.root, saved)?;
        }
        Ok(saved.clone())
    }
    pub fn cancel_preparation(&self) {
        let _ = self.command(
            None,
            Command::Cancel {
                id: self.id.clone(),
            },
        );
    }
    pub fn choose(&self, account: &str, decision: u64, choice: Choice) -> Result<()> {
        self.command(
            Some(account),
            Command::Choose {
                id: self.id.clone(),
                account: account.into(),
                decision,
                choice,
            },
        )
    }
    pub fn revoke_cloud(&self, account: &str) -> Result<()> {
        self.command(
            Some(account),
            Command::Revoke {
                id: self.id.clone(),
                account: account.into(),
            },
        )
    }
    pub fn resume_after_rosetta(&self, account: &str, decision_id: u64) -> Result<()> {
        let response = self.command_snapshot(
            Some(account),
            Command::ResumeAfterRosetta {
                id: self.id.clone(),
                account: account.into(),
                decision_id,
            },
        )?;
        if response.rosetta.is_some() {
            return Err(Error::RosettaUnavailable);
        }
        if response.finished() && response.phase != Phase::Finished {
            return Err(Error::Session);
        }
        Ok(())
    }
    fn command(&self, account: Option<&str>, command: Command) -> Result<()> {
        self.command_snapshot(account, command).map(|_| ())
    }
    fn command_snapshot(&self, account: Option<&str>, command: Command) -> Result<Snapshot> {
        let saved = read_snapshot(&self.root, Some(&self.id))?.ok_or(Error::Session)?;
        if account.is_some_and(|a| a != saved.account_id) {
            return Err(Error::Session);
        }
        if saved.finished() {
            return Ok(saved);
        }
        if inspect(&self.helper, saved.supervisor.pid)?.as_ref() != Some(&saved.supervisor) {
            return Err(Error::Session);
        }
        exchange(&saved, &command)
    }
}

pub fn start(
    config: Configuration,
    account_id: String,
    exchange_code: String,
    launch: bool,
    cancel: CancelToken,
) -> Result<Client> {
    config.validate(&account_id)?;
    let starting = storage::lock(&config.session_directory, "start.lock")?;
    if let Some(saved) = read_snapshot(&config.session_directory, None)?
        && !saved.finished()
        && (inspect(&config.helper, saved.supervisor.pid)?.as_ref() == Some(&saved.supervisor)
            || owns_child(&saved, |pid| inspect(&config.helper, pid))?)
    {
        return Err(Error::Busy);
    }
    let _starting = storage::owner_released(&config.session_directory, starting)?;
    cancel.check().map_err(|_| Error::Cancelled)?;
    let request = Request {
        config: config.clone(),
        id: uuid::Uuid::new_v4().to_string(),
        account_id,
        exchange_code,
        launch,
    };
    let (mut pipe, child_pipe) = UnixStream::pair()?;
    pipe.set_read_timeout(Some(Duration::from_secs(15)))?;
    pipe.set_write_timeout(Some(Duration::from_secs(3)))?;
    let mut child = Process::new(&config.helper)
        .arg("epic-supervise")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::from(OwnedFd::from(child_pipe.try_clone()?)))
        .stdout(Stdio::null())
        .stderr(Stdio::from(OwnedFd::from(child_pipe)))
        .spawn()?;
    let client = Client {
        root: config.session_directory,
        helper: config.helper,
        id: request.id.clone(),
    };
    let handoff = storage::send(&mut pipe, &request)
        .map_err(Error::from)
        .and_then(|()| {
            let ack: Snapshot = storage::packet(&pipe)?;
            if ack.id != request.id
                || ack.account_id != request.account_id
                || ack.app_name != config.app_name
                || ack.namespace != config.namespace
                || ack.supervisor.pid != child.id()
                || inspect(&client.helper, child.id())?.as_ref() != Some(&ack.supervisor)
            {
                return Err(Error::Session);
            }
            Ok(())
        });
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    if let Err(error) = handoff {
        // Preserve accepted work when its acknowledgement was lost.
        if read_snapshot(&client.root, Some(&client.id))?.is_none() {
            return Err(error);
        }
    }
    if cancel.is_cancelled() {
        client.cancel_preparation();
    }
    Ok(client)
}

fn exchange(saved: &Snapshot, command: &Command) -> Result<Snapshot> {
    let parent = saved.socket.parent().ok_or(Error::Session)?;
    let metadata = fs::symlink_metadata(parent)?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != saved.supervisor.uid
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(Error::Session);
    }
    let mut stream = UnixStream::connect(&saved.socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(20)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    storage::send(&mut stream, command)?;
    let response: Snapshot = storage::packet(stream)?;
    if response.id != saved.id
        || response.account_id != saved.account_id
        || response.namespace != saved.namespace
        || response.app_name != saved.app_name
        || response.supervisor != saved.supervisor
    {
        return Err(Error::Session);
    }
    Ok(response)
}

pub(super) fn inspect(helper: &Path, pid: u32) -> Result<Option<Identity>> {
    crate::supervisor::client::inspect(helper, pid).map_err(Into::into)
}
